//! Streaming, portable receipt for a staged directory restore.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const VERSION: u32 = 1;
const METADATA: &str = "File contents and directory names. Ownership, permissions, timestamps, alternate streams and links are omitted.";

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Restore {
        version: u32,
        source: String,
        source_root: String,
        metadata: String,
    },
    File {
        path: String,
        bytes: u64,
        sha256: String,
    },
    Directory {
        path: String,
    },
    Skipped {
        path: String,
        reason: String,
    },
    End {
        entries: u64,
    },
}

pub(crate) struct ReportWriter {
    file: BufWriter<NamedTempFile>,
    skipped_report_names: HashSet<String>,
    entries: u64,
}

fn file_digest(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 128 * 1024];
    loop {
        crate::progress::checkpoint()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok((bytes, format!("{:x}", hash.finalize())))
}

impl ReportWriter {
    pub(crate) fn new(staging: &Path, source: &str, source_root: &str) -> Result<Self> {
        let mut report = Self {
            file: BufWriter::new(NamedTempFile::new_in(staging)?),
            entries: 0,
            skipped_report_names: HashSet::new(),
        };
        report.line(Record::Restore {
            version: VERSION,
            source: source.into(),
            source_root: source_root.into(),
            metadata: METADATA.into(),
        })?;
        Ok(report)
    }

    fn line(&mut self, record: Record) -> Result<()> {
        serde_json::to_writer(&mut self.file, &record)?;
        self.file.write_all(b"\n")?;
        Ok(())
    }

    pub(crate) fn entry(
        &mut self,
        relative: &str,
        target: &Path,
        kind: u8,
        expected: u64,
    ) -> Result<()> {
        let path = relative.to_owned();
        // A skipped root entry still owns its original name in the inventory.
        if !matches!(kind, 4 | 8) && !relative.contains('/') {
            let name = relative.to_ascii_lowercase();
            if name.starts_with("zfs restore report") && name.ends_with(".jsonl") {
                self.skipped_report_names.insert(name);
            }
        }
        let record = match kind {
            4 => Record::Directory { path },
            8 => {
                let (bytes, sha256) =
                    file_digest(target).with_context(|| format!("checking restored {path}"))?;
                ensure!(bytes == expected, "restored size differs for {path}");
                Record::File {
                    path,
                    bytes,
                    sha256,
                }
            }
            _ => Record::Skipped {
                path,
                reason: if kind == 10 {
                    "symbolic link"
                } else {
                    "special entry"
                }
                .into(),
            },
        };
        self.line(record)?;
        self.entries += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self, root: &Path) -> Result<String> {
        self.line(Record::End {
            entries: self.entries,
        })?;
        self.file.flush()?;
        let mut temporary = self.file.into_inner().map_err(|error| error.into_error())?;
        temporary.as_file().sync_all()?;
        for n in 0.. {
            let name = if n == 0 {
                "ZFS Restore Report.jsonl".into()
            } else {
                format!("ZFS Restore Report ({n}).jsonl")
            };
            if self
                .skipped_report_names
                .contains(&name.to_ascii_lowercase())
            {
                continue;
            }
            match temporary.persist_noclobber(root.join(&name)) {
                Ok(_) => return Ok(name),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    temporary = error.file
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        unreachable!()
    }
}

fn recorded_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_owned();
    for component in relative.split('/') {
        crate::tree::validate_component(component)?;
        path.push(component);
        let meta = fs::symlink_metadata(&path)?;
        ensure!(!meta.is_symlink(), "unexpected symbolic link");
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(
                meta.file_attributes() & 0x400 == 0,
                "unexpected reparse point"
            );
        }
    }
    Ok(path)
}

/// Verify every recorded regular file and directory against its sibling report.
pub fn verify(report: &Path) -> Result<u64> {
    let root = report.parent().unwrap_or(Path::new("."));
    let mut lines = BufReader::new(File::open(report)?).lines();
    let first = lines.next().context("empty restore report")??;
    ensure!(
        matches!(
            serde_json::from_str(&first)?,
            Record::Restore {
                version: VERSION,
                ..
            }
        ),
        "unsupported restore report"
    );
    let mut count = 0;
    let mut complete = false;
    for line in lines {
        ensure!(!complete, "unexpected record after end of restore report");
        match serde_json::from_str(&line?)? {
            Record::File {
                path,
                bytes,
                sha256,
            } => {
                let full =
                    recorded_path(root, &path).with_context(|| format!("checking {path}"))?;
                ensure!(full.is_file(), "{path} is no longer a regular file");
                let actual = file_digest(&full).with_context(|| format!("reading {path}"))?;
                ensure!(actual == (bytes, sha256), "restored file differs: {path}");
            }
            Record::Directory { path } => {
                ensure!(
                    recorded_path(root, &path)
                        .with_context(|| format!("checking {path}"))?
                        .is_dir(),
                    "expected directory {path}"
                );
            }
            Record::Skipped { .. } => {}
            Record::End { entries } => {
                ensure!(entries == count, "incomplete restore report");
                complete = true;
                continue;
            }
            Record::Restore { .. } => bail!("unexpected second restore header"),
        }
        count += 1;
    }
    ensure!(complete, "incomplete restore report");
    Ok(count)
}
