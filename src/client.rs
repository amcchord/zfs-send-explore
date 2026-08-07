//! UI-facing service layer shared by the native Windows client and tests.
//!
//! The types here deliberately contain no window-system objects. A UI can run
//! every method on a worker thread, then send the small catalog or directory
//! result back to its event loop.

use crate::filesystem::DirectoryEntry;
use crate::inception::{DiskContainerKind, InceptionSession, VolumeInfo};
use crate::operations::{self, EncryptionRequirement, Sidecar};
use crate::pool::{PoolExtraction, PoolMember};
use crate::stream::FEATURE_RAW;
use anyhow::{Context, Result, anyhow, bail};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

/// The two read-only backup sources understood by the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    SendStream,
    PoolMember,
}

/// One selectable filesystem view in a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceView {
    /// User-facing label, including whether the view is current, full, or
    /// incremental.
    pub label: String,
    /// Stable selector accepted by the corresponding backend.
    pub selector: String,
    /// True when extracting from this view writes incremental-update metadata.
    pub update_eligible: bool,
    /// True when this send-stream view requires a ZFS key to browse.
    pub encrypted: bool,
}

/// Lightweight description retained by the UI after opening a source.
#[derive(Debug, Clone)]
pub struct SourceCatalog {
    pub kind: SourceKind,
    pub path: PathBuf,
    pub title: String,
    pub summary: String,
    pub views: Vec<SourceView>,
}

/// Result of extracting one selected file.
#[derive(Debug, Clone)]
pub struct ClientExtraction {
    pub logical_size: u64,
    pub sha256: String,
    pub update_eligible: bool,
}

/// Lightweight nested-image description retained by the Windows UI.
#[derive(Clone)]
pub struct InceptionCatalog {
    pub image_path: String,
    pub image_offset: u64,
    pub stored_size: u64,
    pub disk_size: u64,
    pub container: DiskContainerKind,
    pub volumes: Vec<VolumeInfo>,
    session: Arc<InceptionSession>,
}

impl InceptionCatalog {
    /// List a directory through the already-inspected virtual disk. Retaining
    /// the session avoids rescanning a large ZFS send stream on every click.
    pub fn list_directory(&self, volume: Option<&str>, path: &str) -> Result<Vec<DirectoryEntry>> {
        self.session.list_directory(volume, path)
    }

    /// Extract from the already-inspected subordinate filesystem. Nested
    /// extractions intentionally do not produce ZFS incremental sidecars.
    pub fn extract(
        &self,
        volume: Option<&str>,
        path: &str,
        destination: &Path,
        force: bool,
    ) -> Result<ClientExtraction> {
        let extraction = self.session.extract(volume, path, destination, force)?;
        Ok(ClientExtraction {
            logical_size: extraction.logical_size,
            sha256: extraction.sha256,
            update_eligible: false,
        })
    }
}

impl SourceCatalog {
    /// Validate and catalog a ZFS send file without requesting an encryption
    /// key. Snapshot names remain visible in raw sends.
    pub fn open_send(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let inspection = operations::inspect_stream(path)?;
        let views = inspection
            .snapshots
            .iter()
            .map(|snapshot| {
                let mode = if snapshot.features & FEATURE_RAW != 0 {
                    "raw encrypted"
                } else {
                    "plain"
                };
                let relation = if snapshot.from_guid == 0 {
                    "full"
                } else {
                    "incremental"
                };
                SourceView {
                    label: format!("{}  —  {relation}, {mode}", snapshot.dataset_name),
                    selector: format!("0x{:016x}", snapshot.to_guid),
                    update_eligible: true,
                    encrypted: snapshot.features & FEATURE_RAW != 0,
                }
            })
            .collect::<Vec<_>>();
        let title = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into(),
        );
        Ok(Self {
            kind: SourceKind::SendStream,
            path: path.to_owned(),
            title,
            summary: format!(
                "{} snapshot{} · {} stream bytes",
                views.len(),
                if views.len() == 1 { "" } else { "s" },
                inspection.stream_bytes
            ),
            views,
        })
    }

    /// Open an exported vdev member, vdev image, or supported whole-disk image.
    pub fn open_pool(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let pool = PoolMember::open(path)?;
        let inspection = pool.inspect()?;
        let datasets = pool.datasets()?;
        let snapshots = pool.snapshots(None)?;
        let mut views = Vec::with_capacity(datasets.len() + snapshots.len());
        for snapshot in snapshots {
            views.push(SourceView {
                label: format!("{}  —  snapshot", snapshot.full_name),
                selector: snapshot.full_name,
                update_eligible: true,
                encrypted: false,
            });
        }
        for dataset in datasets {
            views.push(SourceView {
                label: format!("{}  —  current (read-only)", dataset.name),
                selector: dataset.name,
                update_eligible: false,
                encrypted: false,
            });
        }
        if views.is_empty() {
            bail!(
                "pool {} contains no browseable filesystem datasets",
                inspection.pool_name
            );
        }
        Ok(Self {
            kind: SourceKind::PoolMember,
            path: path.to_owned(),
            title: inspection.pool_name.clone(),
            summary: format!(
                "{} · txg {} · {} dataset{} · {} snapshot{}",
                inspection.vdev_type,
                inspection.txg,
                inspection.datasets,
                if inspection.datasets == 1 { "" } else { "s" },
                inspection.snapshots,
                if inspection.snapshots == 1 { "" } else { "s" },
            ),
            views,
        })
    }

    pub fn view(&self, index: usize) -> Result<&SourceView> {
        self.views
            .get(index)
            .ok_or_else(|| anyhow!("selected source view {index} no longer exists"))
    }

    /// List one directory. `key_file` is consulted only for a raw encrypted
    /// send and is never retained in the catalog.
    pub fn list_directory(
        &self,
        view_index: usize,
        path: &str,
        key_file: Option<&Path>,
    ) -> Result<Vec<DirectoryEntry>> {
        let view = self.view(view_index)?;
        match self.kind {
            SourceKind::SendStream => {
                let key = key_for_snapshot(&self.path, &view.selector, key_file)?;
                operations::list_directory_snapshot_with_key(
                    &self.path,
                    path,
                    Some(&view.selector),
                    key.as_deref().map(Vec::as_slice),
                )
            }
            SourceKind::PoolMember => {
                PoolMember::open(&self.path)?.list_directory(&view.selector, path)
            }
        }
    }

    /// Extract one regular file from the selected view.
    pub fn extract(
        &self,
        view_index: usize,
        source_path: &str,
        destination: &Path,
        force: bool,
        key_file: Option<&Path>,
    ) -> Result<ClientExtraction> {
        let view = self.view(view_index)?;
        match self.kind {
            SourceKind::SendStream => {
                let key = key_for_snapshot(&self.path, &view.selector, key_file)?;
                let sidecar = operations::extract_snapshot_with_key(
                    &self.path,
                    source_path,
                    destination,
                    force,
                    Some(&view.selector),
                    key.as_deref().map(Vec::as_slice),
                )?;
                Ok(extraction_from_sidecar(sidecar))
            }
            SourceKind::PoolMember => {
                let extraction = PoolMember::open(&self.path)?.extract(
                    &view.selector,
                    source_path,
                    destination,
                    force,
                )?;
                Ok(extraction_from_pool(extraction))
            }
        }
    }

    /// Detect the disk container, partitions, and subordinate filesystems in
    /// one regular file from the selected ZFS view.
    pub fn inspect_inception(
        &self,
        view_index: usize,
        image_path: &str,
        key_file: Option<&Path>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<InceptionCatalog> {
        let session = Arc::new(self.open_inception(
            view_index,
            image_path,
            key_file,
            image_offset,
            image_length,
        )?);
        Ok(InceptionCatalog {
            image_path: session.image_path().to_owned(),
            image_offset: session.image_offset(),
            stored_size: session.stored_size(),
            disk_size: session.image_size(),
            container: session.container(),
            volumes: session.volumes().to_vec(),
            session,
        })
    }

    fn open_inception(
        &self,
        view_index: usize,
        image_path: &str,
        key_file: Option<&Path>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<InceptionSession> {
        let view = self.view(view_index)?;
        match self.kind {
            SourceKind::SendStream => {
                let key = key_for_snapshot(&self.path, &view.selector, key_file)?;
                InceptionSession::from_send_at(
                    &self.path,
                    Some(&view.selector),
                    image_path,
                    key.as_deref().map(Vec::as_slice),
                    image_offset,
                    image_length,
                )
            }
            SourceKind::PoolMember => InceptionSession::from_pool_at(
                &self.path,
                &view.selector,
                image_path,
                image_offset,
                image_length,
            ),
        }
    }
}

/// Apply one standalone incremental send to a file previously extracted from
/// a named snapshot. Backend validation and atomic replacement are unchanged.
pub fn apply_incremental(stream: &Path, target: &Path, key_file: Option<&Path>) -> Result<Sidecar> {
    let requirement = operations::apply_encryption_requirement(stream)?;
    let key = read_key_for_requirement(requirement, key_file)?;
    operations::apply_incremental_with_key(stream, target, key.as_deref().map(Vec::as_slice))
}

/// Join a displayed directory with a child name while preserving normalized
/// absolute ZFS paths.
pub fn child_path(directory: &str, name: &str) -> Result<String> {
    if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
        bail!("invalid directory entry name {name:?}");
    }
    if directory == "/" {
        Ok(format!("/{name}"))
    } else {
        Ok(format!("{}/{name}", directory.trim_end_matches('/')))
    }
}

pub fn parent_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_owned(),
        Some((parent, _)) => parent.to_owned(),
    }
}

fn key_for_snapshot(
    stream: &Path,
    selector: &str,
    key_file: Option<&Path>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let requirement = operations::encryption_requirement(stream, Some(selector))?;
    read_key_for_requirement(requirement, key_file)
}

fn read_key_for_requirement(
    requirement: Option<EncryptionRequirement>,
    key_file: Option<&Path>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(requirement) = requirement else {
        return Ok(None);
    };
    let path = key_file.ok_or_else(|| {
        anyhow!(
            "{} uses a raw encrypted send; choose its {} key file first",
            requirement.dataset_name,
            requirement.key_format
        )
    })?;
    let maximum_size = match requirement.key_format.as_str() {
        "raw" => 32_u64,
        "hex" => 65,
        "passphrase" => 513,
        _ => unreachable!("encryption requirement validates the key format"),
    };
    let mut file = File::open(path)
        .with_context(|| format!("opening ZFS key file {}", path.display()))?
        .take(maximum_size + 1);
    let mut material = Vec::new();
    file.read_to_end(&mut material)
        .with_context(|| format!("reading ZFS key file {}", path.display()))?;
    if material.len() as u64 > maximum_size {
        bail!(
            "ZFS {} key file {} is too large",
            requirement.key_format,
            path.display()
        );
    }
    if requirement.key_format != "raw" && material.last() == Some(&b'\n') {
        material.pop();
    }
    Ok(Some(Zeroizing::new(material)))
}

fn extraction_from_sidecar(sidecar: Sidecar) -> ClientExtraction {
    ClientExtraction {
        logical_size: sidecar.logical_size,
        sha256: sidecar.sha256,
        update_eligible: true,
    }
}

fn extraction_from_pool(extraction: PoolExtraction) -> ClientExtraction {
    ClientExtraction {
        logical_size: extraction.logical_size,
        sha256: extraction.sha256,
        update_eligible: extraction.sidecar_written,
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceCatalog, SourceKind, apply_incremental, child_path, parent_path};
    use std::path::Path;

    #[test]
    fn send_catalog_browses_and_extracts_a_selected_snapshot() {
        let catalog =
            SourceCatalog::open_send(Path::new("tests/fixtures/multi-snapshot.zfs")).unwrap();
        assert_eq!(catalog.kind, SourceKind::SendStream);
        assert_eq!(catalog.views.len(), 3);
        let s2 = catalog
            .views
            .iter()
            .position(|view| view.label.contains("@s2"))
            .unwrap();
        let entries = catalog.list_directory(s2, "/", None).unwrap();
        assert!(entries.iter().any(|entry| entry.name == "only-s2.txt"));

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("version.txt");
        let extraction = catalog
            .extract(s2, "/version.txt", &target, false, None)
            .unwrap();
        assert!(extraction.update_eligible);
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "snapshot two has a longer value\n"
        );
    }

    #[test]
    fn navigation_paths_stay_absolute_and_normalized() {
        assert_eq!(child_path("/", "docs").unwrap(), "/docs");
        assert_eq!(child_path("/docs", "notes.txt").unwrap(), "/docs/notes.txt");
        assert!(child_path("/", "../secret").is_err());
        assert_eq!(parent_path("/docs/notes"), "/docs");
        assert_eq!(parent_path("/docs"), "/");
        assert_eq!(parent_path("/"), "/");
    }

    #[test]
    fn client_update_flow_advances_a_verified_extraction() {
        let catalog = SourceCatalog::open_send(Path::new("tests/fixtures/tiny-full.zfs")).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("hello.txt");
        catalog
            .extract(0, "/hello.txt", &target, false, None)
            .unwrap();
        let updated = apply_incremental(
            Path::new("tests/fixtures/tiny-incremental.zfs"),
            &target,
            None,
        )
        .unwrap();
        assert_eq!(updated.logical_size, 58);
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "hello from the incremental snapshot\nwith an appended line\n"
        );
    }
}
