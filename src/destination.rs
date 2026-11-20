//! Publish restores without overwriting a newly arrived destination. A durable
//! intent file lets the next attempt finish an interrupted payload/sidecar pair.
use crate::operations::{Sidecar, sidecar_path};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

pub(crate) fn temporary_for(path: &Path) -> Result<NamedTempFile> {
    fs::create_dir_all(parent(path))?;
    Ok(NamedTempFile::new_in(parent(path))?)
}

// Identity survives rename. Size and modification time detect ordinary edits
// to the very same file while an incremental update is being computed.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
struct Identity {
    volume: u64,
    file: u64,
    size: u64,
    modified: i128,
}

#[cfg(unix)]
fn identity(path: &Path) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path)?;
    Ok(Identity {
        volume: m.dev(),
        file: m.ino(),
        size: m.len(),
        modified: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
    })
}

#[cfg(windows)]
fn identity(path: &Path) -> io::Result<Identity> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };
    let file = File::options()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the output is writable and the handle remains open until return.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let combine = |hi, lo| u64::from(hi) << 32 | u64::from(lo);
    Ok(Identity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: combine(info.nFileIndexHigh, info.nFileIndexLow),
        size: combine(info.nFileSizeHigh, info.nFileSizeLow),
        modified: i128::from(combine(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        )),
    })
}

fn existing(path: &Path) -> Result<Option<Identity>> {
    match identity(path) {
        Ok(id) => Ok(Some(id)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("inspecting {}", path.display())),
    }
}

/// Compare canonical ancestors as well as filesystem IDs (hard links and
/// case-insensitive aliases). Physical devices themselves cannot be a target.
fn protect_source(source: Option<&Path>, path: &Path) -> Result<()> {
    let Some(id) = existing(path)? else {
        return Ok(());
    };
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() || m.is_dir(),
        "destination {} is a link or special file",
        path.display()
    );
    if let Some(source) = source {
        #[cfg(windows)]
        if source
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(r"\\.\physicaldrive")
        {
            return Ok(()); // Device targets were excluded above.
        }
        let resolved = fs::canonicalize(source)
            .with_context(|| format!("checking source identity for {}", source.display()))?;
        let parent_path = fs::canonicalize(parent(source))?;
        for ancestor in resolved.ancestors().chain(parent_path.ancestors()) {
            let src = identity(ancestor)?;
            ensure!(
                (id.volume, id.file) != (src.volume, src.file),
                "destination {} is the source backup or contains it; choose another destination",
                path.display()
            );
        }
    }
    Ok(())
}

fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_SAVE.with_borrow_mut(|fault| fault.take_if(|path| path == to).is_some()) {
        return Err(io::Error::from(io::ErrorKind::StorageFull));
    }
    platform_rename_exclusive(from, to)
}

#[cfg(test)]
thread_local! {
    static FAIL_SAVE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn platform_rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both inputs are live NUL-terminated paths. The flags guarantee
    // that a destination created concurrently can never be replaced.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn platform_rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    // Use absolute extended-length paths just as std::fs does. The destination
    // does not yet exist so resolve its parent rather than the entire path.
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("missing file name"))?;
        Ok(fs::canonicalize(parent(path))?
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect())
    }
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: both buffers are terminated and live until the call returns.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_rename_exclusive(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "exclusive restore publication is unavailable on this platform",
    ))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path; // Windows moves use MOVEFILE_WRITE_THROUGH.
    Ok(())
}

fn lock(file: &File) -> Result<()> {
    #[cfg(unix)]
    let success = {
        use std::os::fd::AsRawFd;
        // SAFETY: lock is released when the live file descriptor is closed.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    };
    #[cfg(windows)]
    let success = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
        };
        // The same handle reads the manifest. The byte-range lock follows
        // the file through publication and is released on process exit.
        let mut overlap = windows_sys::Win32::System::IO::OVERLAPPED::default();
        unsafe {
            LockFileEx(
                file.as_raw_handle().cast(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlap,
            ) != 0
        }
    };
    if !success {
        return Err(io::Error::last_os_error())
            .context("another restore is saving this destination; try again after it finishes");
    }
    Ok(())
}

fn journal_name(target: &Path) -> Result<PathBuf> {
    Ok(parent(target).join(format!(".zfse-{}.pending", Intent::name_hash(target)?)))
}

#[derive(Serialize, Deserialize)]
struct Change {
    old: Option<Identity>,
    new: Option<Identity>,
}

#[derive(Serialize, Deserialize)]
struct Intent {
    version: u32,
    target_name_hash: String,
    stage_identity: (u64, u64),
    staging: String,
    data: Change,
    metadata: Option<Change>,
}

impl Intent {
    fn staged(&self, target: &Path) -> Result<PathBuf> {
        ensure!(
            Self::name_hash(target)? == self.target_name_hash,
            "pending restore belongs to a different destination"
        );
        ensure!(
            self.version == 1
                && self.staging.starts_with(".zfse-save-")
                && self.staging.len() > ".zfse-save-".len()
                && self.staging[".zfse-save-".len()..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric()),
            "unrecognized pending restore record; keep it for inspection"
        );
        let stage = parent(target).join(&self.staging);
        ensure!(
            fs::symlink_metadata(&stage)?.file_type().is_dir(),
            "invalid pending restore staging folder"
        );
        let id = identity(&stage)?;
        ensure!(
            (id.volume, id.file) == self.stage_identity,
            "pending restore staging folder has changed"
        );
        Ok(stage)
    }

    fn name_hash(target: &Path) -> Result<String> {
        use sha2::{Digest, Sha256};
        let name = target.file_name().context("missing destination name")?;
        Ok(format!("{:x}", Sha256::digest(name.as_encoded_bytes())))
    }

    /// Each rename is exclusive. Keeping both previous objects until both
    /// new names have been made durable makes every intermediate state resumable.
    fn commit(&self, target: &Path, journal: &Path, locked: File) -> Result<()> {
        let owned = identity(journal)?;
        let stage = self.staged(target)?;
        let result = match self.finish(target, journal) {
            Ok(()) => Ok(()),
            Err(error) if existing(journal)? == Some(owned) => match self.rollback(target, journal) {
                Ok(()) => Err(error).context("restore could not be saved; staged changes rolled back"),
                Err(rollback) => Err(error).context(format!("restore is pending at {}; retry after resolving the destination problem ({rollback:#})", journal.display())),
            },
            Err(error) => Err(error),
        };
        // Windows can keep a deleted directory entry pending until its last
        // handle closes. Drop our journal handle before removing the folder.
        drop(locked);
        if stage.join("retired").try_exists()? {
            fs::remove_dir_all(&stage)
                .with_context(|| format!("cleaning up completed restore at {}", stage.display()))?;
        }
        result
    }

    fn rollback(&self, target: &Path, journal: &Path) -> Result<()> {
        let stage = self.staged(target)?;
        let side = sidecar_path(target);
        for (name, path, change) in [
            ("metadata", side.as_path(), self.metadata.as_ref()),
            ("data", target, Some(&self.data)),
        ] {
            let Some(change) = change else { continue };
            let new = stage.join(name);
            let old = stage.join(format!("old-{name}"));
            if let Some(id) = change.new
                && existing(&new)?.is_none()
                && existing(path)? == Some(id)
            {
                rename_exclusive(path, &new)?;
                if existing(&new)? != Some(id) {
                    rename_exclusive(&new, path).with_context(|| {
                        format!("recover the displaced item from {}", new.display())
                    })?;
                    bail!("destination {} changed during rollback", path.display());
                }
            }
            if let Some(id) = existing(&old)? {
                ensure!(
                    Some(id) == change.old,
                    "previous item in {} changed",
                    old.display()
                );
                rename_exclusive(&old, path)
                    .with_context(|| format!("previous item retained at {}", old.display()))?;
            }
        }
        sync_directory(parent(target))?;
        self.retire(target, journal, &stage)
    }

    fn finish(&self, target: &Path, journal: &Path) -> Result<()> {
        let stage = self.staged(target)?;
        let metadata = sidecar_path(target);
        let mut changes = vec![("data", target, &self.data)];
        if let Some(change) = &self.metadata {
            changes.push(("metadata", metadata.as_path(), change));
        }
        for (name, path, change) in &changes {
            let current = existing(path)?;
            let staged = existing(&stage.join(name))?;
            if staged.is_none() && current == change.new {
                continue;
            }
            ensure!(
                staged == change.new,
                "pending restore payload changed at {}",
                stage.display()
            );
            let old = stage.join(format!("old-{name}"));
            if current.is_some() {
                ensure!(
                    current == change.old && existing(&old)?.is_none(),
                    "destination {} changed while restoring; existing content was kept",
                    path.display()
                );
                rename_exclusive(path, &old)?;
                // A different process may have replaced the name just before
                // the rename. In that case return the actual object untouched.
                if existing(&old)? != change.old {
                    rename_exclusive(&old, path).with_context(|| {
                        format!("recover the displaced item from {}", old.display())
                    })?;
                    bail!("destination {} changed during replacement", path.display());
                }
                sync_directory(&stage)?;
                sync_directory(parent(target))?;
            } else if let Some(expected) = change.old {
                ensure!(
                    existing(&old)? == Some(expected),
                    "destination {} went missing",
                    path.display()
                );
            }
            if staged.is_some() {
                rename_exclusive(&stage.join(name), path).with_context(|| {
                    format!("saving {} without overwriting another item", path.display())
                })?;
                sync_directory(parent(target))?;
                sync_directory(&stage)?;
            }
        }
        self.retire(target, journal, &stage)
    }

    fn retire(&self, target: &Path, journal: &Path, stage: &Path) -> Result<()> {
        // Retire the journal first. A later restore can now use this name,
        // so cleanup only touches our uniquely named staging directory.
        rename_exclusive(journal, &stage.join("retired"))?;
        sync_directory(parent(target))
    }
}

pub(crate) struct RestoreDestination {
    path: PathBuf,
    data: Option<Identity>,
    metadata: Option<Option<Identity>>,
}

impl RestoreDestination {
    /// A member of a new private staging tree. Its siblings came from the
    /// backup and must never be interpreted as this application's journals.
    pub(crate) fn staged_member(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            data: None,
            metadata: None,
        }
    }

    pub(crate) fn prepare(
        source: Option<&Path>,
        path: &Path,
        force: bool,
        companion: bool,
    ) -> Result<Self> {
        Self::recover(source, path)?;
        let data = existing(path)?;
        ensure!(
            force || data.is_none(),
            "destination {} already exists (choose another name)",
            path.display()
        );
        let metadata = if companion {
            let side = sidecar_path(path);
            protect_source(source, &side)?;
            let existing = existing(&side)?;
            ensure!(
                existing.is_none() || (force && data.is_some()),
                "companion file {} already exists",
                side.display()
            );
            if existing.is_some() {
                let old: Sidecar =
                    serde_json::from_reader(File::open(&side)?).with_context(|| {
                        format!(
                            "{} is not restore metadata; choose another destination",
                            side.display()
                        )
                    })?;
                ensure!(
                    old.format_version == crate::operations::SIDECAR_VERSION,
                    "unrecognized restore metadata in {}",
                    side.display()
                );
            }
            Some(existing)
        } else {
            None
        };
        Ok(Self {
            path: path.to_owned(),
            data,
            metadata,
        })
    }

    pub(crate) fn recover(source: Option<&Path>, target: &Path) -> Result<bool> {
        protect_source(source, target)?;
        let journal = journal_name(target)?;
        let Some(id) = existing(&journal)? else {
            return Ok(false);
        };
        protect_source(source, &journal)?;
        protect_source(source, &sidecar_path(target))?;
        ensure!(
            fs::symlink_metadata(&journal)?.file_type().is_file(),
            "invalid pending restore at {}",
            journal.display()
        );
        let file = File::options().read(true).write(true).open(&journal)?;
        lock(&file)?;
        ensure!(
            existing(&journal)? == Some(id),
            "pending restore changed; retry the operation"
        );
        let intent: Intent = serde_json::from_reader(std::io::Read::take(&file, 8192))
            .with_context(|| {
                format!("unrecognized pending restore record {}", journal.display())
            })?;
        protect_source(source, &intent.staged(target)?)?;
        intent.commit(target, &journal, file)?;
        Ok(true)
    }

    pub(crate) fn file(self, file: NamedTempFile, sidecar: Option<&Sidecar>) -> Result<()> {
        if self.data.is_none() && self.metadata.flatten().is_none() && sidecar.is_none() {
            file.persist_noclobber(&self.path).with_context(|| {
                format!(
                    "saving {} without replacing an existing file",
                    self.path.display()
                )
            })?;
            return sync_directory(parent(&self.path));
        }
        if self.data.is_some() {
            ensure!(
                fs::symlink_metadata(&self.path)?.file_type().is_file(),
                "file destination {} is not a regular file",
                self.path.display()
            );
        }
        // Closing the writing handle finalizes Windows last-write timestamps.
        let staged = file.into_temp_path();
        self.publish(&staged, sidecar)
    }

    pub(crate) fn directory(self, staged: &Path) -> Result<()> {
        if self.data.is_none() {
            rename_exclusive(staged, &self.path).with_context(|| {
                format!(
                    "saving {} without replacing an existing directory",
                    self.path.display()
                )
            })?;
            return sync_directory(parent(&self.path));
        }
        self.publish(staged, None)
    }

    fn publish(self, data: &Path, sidecar: Option<&Sidecar>) -> Result<()> {
        let dir = parent(&self.path);
        let stage = tempfile::Builder::new()
            .prefix(".zfse-save-")
            .tempdir_in(dir)?;
        let staged_data = stage.path().join("data");
        rename_exclusive(data, &staged_data)?;
        let metadata = self
            .metadata
            .map(|old| -> Result<Change> {
                let new = if let Some(sidecar) = sidecar {
                    let name = stage.path().join("metadata");
                    let mut file = File::create_new(&name)?;
                    serde_json::to_writer_pretty(&mut file, sidecar)?;
                    file.write_all(b"\n")?;
                    file.sync_all()?;
                    drop(file);
                    Some(identity(&name)?)
                } else {
                    None
                };
                Ok(Change { old, new })
            })
            .transpose()?;
        let intent = Intent {
            version: 1,
            stage_identity: {
                let id = identity(stage.path())?;
                (id.volume, id.file)
            },
            target_name_hash: Intent::name_hash(&self.path)?,
            staging: stage
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            data: Change {
                old: self.data,
                new: Some(identity(&staged_data)?),
            },
            metadata,
        };
        let journal = journal_name(&self.path)?;
        let mut pending = temporary_for(&journal)?;
        serde_json::to_writer(pending.as_file_mut(), &intent)?;
        pending.as_file().sync_all()?;
        lock(pending.as_file())?;
        sync_directory(stage.path())?;
        // Recheck both names before activation, so an ordinary name collision
        // leaves neither a partial pair nor a pending transaction.
        ensure!(
            existing(&self.path)? == self.data,
            "destination {} changed or already exists",
            self.path.display()
        );
        if let Some(expected) = self.metadata {
            ensure!(
                existing(&sidecar_path(&self.path))? == expected,
                "companion file for {} changed or already exists",
                self.path.display()
            );
        }
        let locked = pending
            .persist_noclobber(&journal)
            .with_context(|| format!("another restore is pending for {}", self.path.display()))?;
        let _kept = stage.keep();
        sync_directory(dir)?;
        intent
            .commit(&self.path, &journal, locked)
            .with_context(|| format!("saving restore to {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{apply_incremental, extract};
    use std::sync::{Arc, Barrier};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn racing_restores_publish_a_single_file_or_tree() {
        let root = tempfile::tempdir().unwrap();
        for directory in [false, true] {
            let output = root.path().join(if directory { "dir" } else { "file" });
            let barrier = Arc::new(Barrier::new(2));
            let results = std::thread::scope(|scope| {
                let jobs: Vec<_> = [b"one", b"two"]
                    .iter()
                    .map(|bytes| {
                        let gate = barrier.clone();
                        let output = &output;
                        scope.spawn(move || {
                            let ready =
                                RestoreDestination::prepare(None, output, false, false).unwrap();
                            let parent = output.parent().unwrap();
                            if directory {
                                let stage = tempfile::tempdir_in(parent).unwrap();
                                fs::write(stage.path().join("name"), bytes).unwrap();
                                gate.wait();
                                ready.directory(stage.path()).is_ok()
                            } else {
                                let mut file = temporary_for(output).unwrap();
                                file.write_all(*bytes).unwrap();
                                gate.wait();
                                ready.file(file, None).is_ok()
                            }
                        })
                    })
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(results.iter().filter(|x| **x).count(), 1);
            let winner = if results[0] { b"one" } else { b"two" };
            let payload = if directory {
                output.join("name")
            } else {
                output
            };
            assert_eq!(fs::read(payload).unwrap(), winner);
        }
    }

    #[test]
    fn source_ancestors_and_existing_companions_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("backups");
        fs::create_dir(&folder).unwrap();
        let source = folder.join("input.zfs");
        let alias = root.path().join("alias");
        fs::copy(fixture("tiny-full.zfs"), &source).unwrap();
        fs::hard_link(&source, &alias).unwrap();
        assert!(extract(&source, "/hello.txt", &source, true).is_err());
        assert!(extract(&source, "/hello.txt", &alias, true).is_err());
        assert!(
            crate::operations::extract_tree_snapshot_with_key(
                &source, "/", &folder, true, None, None
            )
            .is_err()
        );
        assert_eq!(
            fs::read(&source).unwrap(),
            fs::read(fixture("tiny-full.zfs")).unwrap()
        );
        let output = root.path().join("copy");
        let companion = sidecar_path(&output);
        fs::write(&companion, b"keep this document").unwrap();
        for force in [false, true] {
            assert!(extract(&source, "/hello.txt", &output, force).is_err());
            assert!(!output.exists());
            assert_eq!(fs::read(&companion).unwrap(), b"keep this document");
        }
    }

    #[test]
    fn full_destination_rolls_back_both_outputs_and_allows_retry() {
        for collision_on_metadata in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("file");
            let side = sidecar_path(&target);
            extract(&fixture("tiny-full.zfs"), "/hello.txt", &target, false).unwrap();
            let old = (fs::read(&target).unwrap(), fs::read(&side).unwrap());
            FAIL_SAVE.with_borrow_mut(|fault| {
                *fault = Some(if collision_on_metadata {
                    side.clone()
                } else {
                    target.clone()
                })
            });
            let err = apply_incremental(&fixture("tiny-incremental.zfs"), &target).unwrap_err();
            assert!(format!("{err:#}").contains("rolled back"));
            assert_eq!((fs::read(&target).unwrap(), fs::read(&side).unwrap()), old);
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
            let outcome = apply_incremental(&fixture("tiny-incremental.zfs"), &target).unwrap();
            assert_eq!(fs::metadata(&target).unwrap().len(), outcome.logical_size);
        }
    }

    #[test]
    fn incremental_retry_completes_every_interrupted_pair_state() {
        let root = tempfile::tempdir().unwrap();
        let full = fixture("tiny-full.zfs");
        let delta = fixture("tiny-incremental.zfs");
        let oracle = root.path().join("oracle");
        extract(&full, "/hello.txt", &oracle, false).unwrap();
        let expected = apply_incremental(&delta, &oracle).unwrap();
        for moved in 0..=4 {
            let temp = tempfile::tempdir_in(root.path()).unwrap();
            let target = temp.path().join("file");
            let side = sidecar_path(&target);
            extract(&full, "/hello.txt", &target, false).unwrap();
            // Construct the exact durable states on either side of each
            // exclusive rename in the saver, then invoke the normal retry.
            let stage = tempfile::Builder::new()
                .prefix(".zfse-save-")
                .tempdir_in(temp.path())
                .unwrap()
                .keep();
            let data = stage.join("data");
            let meta = stage.join("metadata");
            fs::copy(&oracle, &data).unwrap();
            fs::copy(sidecar_path(&oracle), &meta).unwrap();
            let stage_id = identity(&stage).unwrap();
            let intent = Intent {
                version: 1,
                target_name_hash: Intent::name_hash(&target).unwrap(),
                stage_identity: (stage_id.volume, stage_id.file),
                staging: stage.file_name().unwrap().to_str().unwrap().into(),
                data: Change {
                    old: existing(&target).unwrap(),
                    new: existing(&data).unwrap(),
                },
                metadata: Some(Change {
                    old: existing(&side).unwrap(),
                    new: existing(&meta).unwrap(),
                }),
            };
            fs::write(
                journal_name(&target).unwrap(),
                serde_json::to_vec(&intent).unwrap(),
            )
            .unwrap();
            for (from, to) in [
                (&target, stage.join("old-data")),
                (&data, target.clone()),
                (&side, stage.join("old-metadata")),
                (&meta, side.clone()),
            ]
            .into_iter()
            .take(moved)
            {
                rename_exclusive(from, &to).unwrap();
            }
            let result = apply_incremental(&delta, &target).unwrap();
            assert_eq!(result.snapshot_guid, expected.snapshot_guid);
            assert_eq!(fs::read(&target).unwrap(), fs::read(&oracle).unwrap());
            assert_eq!(
                fs::read(&side).unwrap(),
                fs::read(sidecar_path(&oracle)).unwrap()
            );
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
        }
        assert_eq!(
            apply_incremental(&delta, &oracle).unwrap().sha256,
            expected.sha256
        );
    }
}
