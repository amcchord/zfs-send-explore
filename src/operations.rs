use crate::encrypted::{DatasetKey, EncryptionParams, decompress_block, is_encrypted_object_type};
use crate::filesystem::{
    DirectoryEntry, ObjectIndex, ResolvedPath, SnapshotPlan, plan_snapshot, snapshot_headers,
};
use crate::stream::{BeginRecord, DMU_SUBSTREAM, FEATURE_RAW, RecordKind, StreamReader};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub(crate) const SIDECAR_VERSION: u32 = 1;
const ZERO_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub begin: BeginRecord,
    pub snapshots: Vec<BeginRecord>,
    pub stream_bytes: u64,
    pub records: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct EncryptionRequirement {
    pub dataset_name: String,
    pub key_format: String,
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
    let mut snapshots = Vec::new();
    let mut records = BTreeMap::new();
    let mut stream_bytes = 0;
    while let Some(record) = reader.next_record()? {
        *records.entry(record.kind.name().to_owned()).or_insert(0) += 1;
        stream_bytes = record.stream_offset + 312 + record.payload.len() as u64;
        if let RecordKind::Begin(header) = record.kind
            && header.header_type == DMU_SUBSTREAM
        {
            snapshots.push(header);
        }
    }
    if !reader.saw_end() {
        bail!("stream has no END record");
    }
    let begin = snapshots
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("stream has no snapshot substream"))?;
    Ok(Inspection {
        begin,
        snapshots,
        stream_bytes,
        records,
    })
}

pub fn list_directory(stream: &Path, path: &str) -> Result<Vec<DirectoryEntry>> {
    ObjectIndex::build(stream)?.list_directory(path)
}

pub fn list_directory_snapshot(
    stream: &Path,
    path: &str,
    snapshot: Option<&str>,
) -> Result<Vec<DirectoryEntry>> {
    list_directory_snapshot_with_key(stream, path, snapshot, None)
}

pub fn list_directory_snapshot_with_key(
    stream: &Path,
    path: &str,
    snapshot: Option<&str>,
    key_material: Option<&[u8]>,
) -> Result<Vec<DirectoryEntry>> {
    let plan = plan_snapshot(stream, snapshot)?;
    ObjectIndex::build_plan_with_key(stream, &plan, key_material)?.list_directory(path)
}

pub fn extract(stream: &Path, path: &str, output: &Path, force: bool) -> Result<Sidecar> {
    extract_snapshot(stream, path, output, force, None)
}

pub fn extract_snapshot(
    stream: &Path,
    path: &str,
    output: &Path,
    force: bool,
    snapshot: Option<&str>,
) -> Result<Sidecar> {
    extract_snapshot_with_key(stream, path, output, force, snapshot, None)
}

pub fn extract_snapshot_with_key(
    stream: &Path,
    path: &str,
    output: &Path,
    force: bool,
    snapshot: Option<&str>,
    key_material: Option<&[u8]>,
) -> Result<Sidecar> {
    let plan = plan_snapshot(stream, snapshot)?;
    let index = ObjectIndex::build_plan_with_key(stream, &plan, key_material)?;
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
    replay_object(
        stream,
        &plan,
        &resolved,
        temporary.as_file_mut(),
        key_material,
    )?;
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

pub fn encryption_requirement(
    stream: &Path,
    snapshot: Option<&str>,
) -> Result<Option<EncryptionRequirement>> {
    let plan = plan_snapshot(stream, snapshot)?;
    if plan.target.features & FEATURE_RAW == 0 {
        return Ok(None);
    }
    if plan.chain.len() != 1 || plan.target.from_guid != 0 {
        bail!(
            "raw encrypted incremental snapshot chains are not supported yet; select a full raw snapshot"
        );
    }
    let file = File::open(stream).with_context(|| format!("opening {}", stream.display()))?;
    let mut reader = StreamReader::new(file);
    while let Some(record) = reader.next_record()? {
        if let RecordKind::Begin(header) = record.kind
            && header.header_type == DMU_SUBSTREAM
            && header.to_guid == plan.target.to_guid
        {
            let params = EncryptionParams::from_begin_payload(&record.payload)?;
            return Ok(Some(EncryptionRequirement {
                dataset_name: header.dataset_name,
                key_format: params.key_format_name()?.to_owned(),
            }));
        }
    }
    bail!("selected encrypted snapshot disappeared while reading its key metadata")
}

pub fn snapshots(stream: &Path) -> Result<Vec<BeginRecord>> {
    snapshot_headers(stream)
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
            RecordKind::Begin(header) if header.header_type != DMU_SUBSTREAM => {}
            RecordKind::Begin(header) => {
                if begin.is_some() {
                    bail!("apply accepts exactly one incremental snapshot substream");
                }
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
            RecordKind::ObjectRange(_) => bail!("raw OBJECT_RANGE streams are unsupported"),
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
    plan: &SnapshotPlan,
    resolved: &ResolvedPath,
    output: &mut File,
    key_material: Option<&[u8]>,
) -> Result<()> {
    if plan.target.features & FEATURE_RAW != 0 {
        return replay_raw_object(stream, plan, resolved, output, key_material);
    }
    let file = File::open(stream)?;
    let mut reader = StreamReader::new(file);
    let selected = plan.chain.iter().copied().collect::<BTreeSet<_>>();
    let mut active_snapshot = None;
    let mut seen = BTreeSet::new();
    let mut object_exists = false;
    while let Some(record) = reader.next_record()? {
        match &record.kind {
            RecordKind::Begin(header) => {
                active_snapshot = (header.header_type == DMU_SUBSTREAM)
                    .then_some(header.to_guid)
                    .filter(|guid| selected.contains(guid));
                if let Some(guid) = active_snapshot {
                    seen.insert(guid);
                }
                continue;
            }
            RecordKind::End => {
                active_snapshot = None;
                continue;
            }
            _ if active_snapshot.is_none() => continue,
            _ => {}
        }

        match record.kind {
            RecordKind::Object(object) if object.object == resolved.object_id => {
                if !object_exists {
                    output.set_len(0)?;
                }
                object_exists = true;
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
            RecordKind::Free(free) if free.object == resolved.object_id => {
                zero_range(output, free.offset, free.length)?;
            }
            RecordKind::FreeObjects(range) => {
                let end = range.first_object.saturating_add(range.object_count);
                if resolved.object_id >= range.first_object && resolved.object_id < end {
                    output.set_len(0)?;
                    object_exists = false;
                }
            }
            RecordKind::ObjectRange(_) => bail!("raw OBJECT_RANGE streams are unsupported"),
            RecordKind::Redact => bail!("redacted streams are unsupported"),
            _ => {}
        }
    }
    if !reader.saw_end() {
        bail!("stream has no END record");
    }
    if seen.len() != plan.chain.len() {
        bail!("stream changed between indexing and extraction");
    }
    Ok(())
}

fn replay_raw_object(
    stream: &Path,
    plan: &SnapshotPlan,
    resolved: &ResolvedPath,
    output: &mut File,
    key_material: Option<&[u8]>,
) -> Result<()> {
    if plan.chain.len() != 1 || plan.target.from_guid != 0 {
        bail!("raw encrypted incremental snapshot chains are not supported yet");
    }
    let key_material = key_material.ok_or_else(|| anyhow!("encrypted raw send requires a key"))?;
    let file = File::open(stream)?;
    let mut reader = StreamReader::new(file);
    let mut active = false;
    let mut key: Option<DatasetKey> = None;
    let mut object_exists = false;
    while let Some(record) = reader.next_record()? {
        match &record.kind {
            RecordKind::Begin(header) => {
                active =
                    header.header_type == DMU_SUBSTREAM && header.to_guid == plan.target.to_guid;
                if active {
                    key = Some(
                        EncryptionParams::from_begin_payload(&record.payload)?
                            .unlock(key_material)?,
                    );
                }
                continue;
            }
            RecordKind::End if active => break,
            _ if !active => continue,
            _ => {}
        }
        let dataset_key = key.as_ref().expect("active raw stream has a key");
        match record.kind {
            RecordKind::Object(object) if object.object == resolved.object_id => {
                output.set_len(0)?;
                object_exists = true;
            }
            RecordKind::Write(write) if write.object == resolved.object_id => {
                let protected = if is_encrypted_object_type(write.object_type) {
                    dataset_key.decrypt_block(
                        &write.salt,
                        &write.iv,
                        &write.mac,
                        &[],
                        &record.payload,
                    )?
                } else {
                    dataset_key.authenticate_block(&record.payload, &write.mac)?;
                    record.payload
                };
                let plaintext =
                    decompress_block(write.compression_type, &protected, write.logical_size)?;
                output.seek(SeekFrom::Start(write.offset))?;
                output.write_all(&plaintext)?;
            }
            RecordKind::Free(free) if free.object == resolved.object_id => {
                zero_range(output, free.offset, free.length)?;
            }
            RecordKind::FreeObjects(range) => {
                let end = range.first_object.saturating_add(range.object_count);
                if resolved.object_id >= range.first_object && resolved.object_id < end {
                    output.set_len(0)?;
                    object_exists = false;
                }
            }
            RecordKind::WriteEmbedded(write) if write.object == resolved.object_id => {
                bail!("raw embedded WRITE records are unsupported")
            }
            RecordKind::WriteByRef => bail!("raw deduplicated WRITE_BYREF is unsupported"),
            RecordKind::Redact => bail!("redacted streams are unsupported"),
            _ => {}
        }
    }
    if !object_exists {
        bail!(
            "object {} was not present in the raw snapshot",
            resolved.object_id
        );
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

pub(crate) fn save_sidecar(target: &Path, sidecar: &Sidecar) -> Result<()> {
    let path = sidecar_path(target);
    let mut temporary = temporary_for(&path)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), sidecar)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    persist_replace(temporary, &path, true)
}

pub(crate) fn sidecar_path(target: &Path) -> PathBuf {
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

pub(crate) fn guid_string(guid: u64) -> String {
    format!("0x{guid:016x}")
}

fn parse_guid(value: &str) -> Result<u64> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u64::from_str_radix(value, 16).with_context(|| format!("invalid snapshot GUID {value:?}"))
}
