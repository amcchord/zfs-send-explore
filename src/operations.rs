use crate::filesystem::{DirectoryEntry, ObjectIndex, ResolvedPath};
use crate::stream::{BeginRecord, RecordKind, StreamReader};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const SIDECAR_VERSION: u32 = 1;
const ZERO_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub begin: BeginRecord,
    pub stream_bytes: u64,
    pub records: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub format_version: u32,
    pub path: String,
    pub object_id: u64,
    pub object_type: u32,
    pub bonus_type: u32,
    pub logical_size: u64,
    pub size_bonus_offset: Option<u64>,
    pub snapshot_guid: String,
    pub sha256: String,
}

pub fn inspect_stream(path: &Path) -> Result<Inspection> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = StreamReader::new(file);
    let mut begin = None;
    let mut records = BTreeMap::new();
    let mut stream_bytes = 0;
    while let Some(record) = reader.next_record()? {
        *records.entry(record.kind.name().to_owned()).or_insert(0) += 1;
        stream_bytes = record.stream_offset + 312 + record.payload.len() as u64;
        if let RecordKind::Begin(header) = record.kind {
            begin = Some(header);
        }
    }
    if !reader.saw_end() {
        bail!("stream has no END record");
    }
    Ok(Inspection {
        begin: begin.ok_or_else(|| anyhow!("stream has no BEGIN record"))?,
        stream_bytes,
        records,
    })
}

pub fn list_directory(stream: &Path, path: &str) -> Result<Vec<DirectoryEntry>> {
    ObjectIndex::build(stream)?.list_directory(path)
}

pub fn extract(stream: &Path, path: &str, output: &Path, force: bool) -> Result<Sidecar> {
    let index = ObjectIndex::build(stream)?;
    let resolved = index.resolve_path(path)?;
    if resolved.dirent_type != 8 {
        bail!("{} is not a regular file", resolved.normalized_path);
    }
    if output.exists() && !force {
        bail!(
            "output {} already exists (pass --force to replace it)",
            output.display()
        );
    }

    let mut temporary = temporary_for(output)?;
    temporary.as_file_mut().set_len(resolved.logical_size)?;
    replay_object(stream, &index.begin, &resolved, temporary.as_file_mut())?;
    temporary.as_file_mut().set_len(resolved.logical_size)?;
    temporary.as_file_mut().sync_all()?;
    persist_replace(temporary, output, force)?;

    let sidecar = Sidecar {
        format_version: SIDECAR_VERSION,
        path: resolved.normalized_path,
        object_id: resolved.object_id,
        object_type: resolved.object_type,
        bonus_type: resolved.bonus_type,
        logical_size: resolved.logical_size,
        size_bonus_offset: resolved.size_bonus_offset,
        snapshot_guid: guid_string(index.begin.to_guid),
        sha256: sha256_file(output)?,
    };
    save_sidecar(output, &sidecar)?;
    Ok(sidecar)
}

pub fn apply_incremental(stream: &Path, target: &Path) -> Result<Sidecar> {
    let sidecar_path = sidecar_path(target);
    let sidecar_file = File::open(&sidecar_path).with_context(|| {
        format!(
            "opening extraction metadata {}; extract the file with this tool first",
            sidecar_path.display()
        )
    })?;
    let mut sidecar: Sidecar = serde_json::from_reader(sidecar_file)
        .with_context(|| format!("reading {}", sidecar_path.display()))?;
    if sidecar.format_version != SIDECAR_VERSION {
        bail!(
            "unsupported sidecar format version {}",
            sidecar.format_version
        );
    }
    let current_hash = sha256_file(target)?;
    if current_hash != sidecar.sha256 {
        bail!(
            "target {} does not match the extracted base file (SHA-256 differs)",
            target.display()
        );
    }
    let current_size = fs::metadata(target)?.len();
    if current_size != sidecar.logical_size {
        bail!(
            "target size is {current_size}, but extraction metadata expects {}",
            sidecar.logical_size
        );
    }

    let source = File::open(target)?;
    let mut temporary = temporary_for(target)?;
    io::copy(&mut io::BufReader::new(source), temporary.as_file_mut())?;

    let file = File::open(stream).with_context(|| format!("opening {}", stream.display()))?;
    let mut reader = StreamReader::new(file);
    let mut begin = None;
    let mut new_size = sidecar.logical_size;
    let mut saw_target_change = false;
    let mut target_deleted = false;

    while let Some(record) = reader.next_record()? {
        match record.kind {
            RecordKind::Begin(header) => {
                let expected = parse_guid(&sidecar.snapshot_guid)?;
                if header.from_guid != expected {
                    bail!(
                        "incremental stream starts from {}, but target metadata is at {}",
                        guid_string(header.from_guid),
                        sidecar.snapshot_guid
                    );
                }
                begin = Some(header);
            }
            RecordKind::Object(object) if object.object == sidecar.object_id => {
                if object.bonus_type != sidecar.bonus_type {
                    bail!("target object's bonus type changed; re-extract from a new full stream");
                }
                if let Some(offset) = sidecar.size_bonus_offset {
                    let offset =
                        usize::try_from(offset).context("stored size offset is too large")?;
                    if offset + 8 > record.payload.len() {
                        bail!("incremental OBJECT record has a truncated size attribute");
                    }
                    new_size = u64::from_le_bytes(
                        record.payload[offset..offset + 8]
                            .try_into()
                            .expect("eight-byte checked range"),
                    );
                }
                saw_target_change = true;
            }
            RecordKind::Write(write) if write.object == sidecar.object_id => {
                if write.compression_type != 0 {
                    bail!(
                        "compressed incremental WRITE is unsupported; create the stream without zfs send -c"
                    );
                }
                temporary
                    .as_file_mut()
                    .seek(SeekFrom::Start(write.offset))?;
                temporary.as_file_mut().write_all(&record.payload)?;
                saw_target_change = true;
            }
            RecordKind::WriteEmbedded(write) if write.object == sidecar.object_id => {
                bail!(
                    "embedded incremental WRITE is unsupported; create the stream without zfs send -e"
                );
            }
            RecordKind::WriteByRef => {
                bail!("deduplicated WRITE_BYREF streams are unsupported")
            }
            RecordKind::Free(free) if free.object == sidecar.object_id => {
                zero_range(temporary.as_file_mut(), free.offset, free.length)?;
                saw_target_change = true;
            }
            RecordKind::FreeObjects(range) => {
                let end = range.first_object.saturating_add(range.object_count);
                if sidecar.object_id >= range.first_object && sidecar.object_id < end {
                    target_deleted = true;
                }
            }
            RecordKind::ObjectRange => bail!("raw OBJECT_RANGE streams are unsupported"),
            RecordKind::Redact => bail!("redacted streams are unsupported"),
            _ => {}
        }
    }
    if !reader.saw_end() {
        bail!("incremental stream has no END record");
    }
    if target_deleted {
        bail!("the target object was deleted or replaced; re-extract it from a new full stream");
    }
    if !saw_target_change {
        bail!(
            "the incremental stream contains no changes for {}",
            sidecar.path
        );
    }
    let begin = begin.ok_or_else(|| anyhow!("incremental stream has no BEGIN record"))?;
    temporary.as_file_mut().set_len(new_size)?;
    temporary.as_file_mut().sync_all()?;
    persist_replace(temporary, target, true)?;

    sidecar.logical_size = new_size;
    sidecar.snapshot_guid = guid_string(begin.to_guid);
    sidecar.sha256 = sha256_file(target)?;
    save_sidecar(target, &sidecar)?;
    Ok(sidecar)
}

fn replay_object(
    stream: &Path,
    expected_begin: &BeginRecord,
    resolved: &ResolvedPath,
    output: &mut File,
) -> Result<()> {
    let file = File::open(stream)?;
    let mut reader = StreamReader::new(file);
    while let Some(record) = reader.next_record()? {
        match record.kind {
            RecordKind::Begin(header) if header.to_guid != expected_begin.to_guid => {
                bail!("stream changed between indexing and extraction");
            }
            RecordKind::Write(write) if write.object == resolved.object_id => {
                if write.compression_type != 0 {
                    bail!("compressed WRITE records are unsupported; omit zfs send -c");
                }
                output.seek(SeekFrom::Start(write.offset))?;
                output.write_all(&record.payload)?;
            }
            RecordKind::WriteEmbedded(write) if write.object == resolved.object_id => {
                bail!(
                    "embedded WRITE records are unsupported; create the stream without zfs send -e"
                );
            }
            RecordKind::WriteByRef => bail!("deduplicated WRITE_BYREF streams are unsupported"),
            _ => {}
        }
    }
    if !reader.saw_end() {
        bail!("stream has no END record");
    }
    Ok(())
}

fn zero_range(file: &mut File, offset: u64, length: u64) -> Result<()> {
    let current_len = file.metadata()?.len();
    if offset >= current_len {
        return Ok(());
    }
    if length == u64::MAX {
        file.set_len(offset)?;
        return Ok(());
    }
    let mut remaining = length.min(current_len - offset);
    file.seek(SeekFrom::Start(offset))?;
    let zeros = [0_u8; ZERO_CHUNK_SIZE];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(ZERO_CHUNK_SIZE as u64)).unwrap();
        file.write_all(&zeros[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

fn temporary_for(path: &Path) -> Result<NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary file in {}", parent.display()))
}

fn persist_replace(temporary: NamedTempFile, path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("{} already exists", path.display());
    }
    temporary
        .persist(path)
        .map_err(|error| anyhow!("persisting {}: {}", path.display(), error.error))?;
    Ok(())
}

fn save_sidecar(target: &Path, sidecar: &Sidecar) -> Result<()> {
    let path = sidecar_path(target);
    let mut temporary = temporary_for(&path)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), sidecar)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    persist_replace(temporary, &path, true)
}

fn sidecar_path(target: &Path) -> PathBuf {
    let mut name: OsString = target.as_os_str().to_owned();
    name.push(".zfse.json");
    PathBuf::from(name)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn guid_string(guid: u64) -> String {
    format!("0x{guid:016x}")
}

fn parse_guid(value: &str) -> Result<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(value, 16).with_context(|| format!("invalid snapshot GUID {value:?}"))
}
