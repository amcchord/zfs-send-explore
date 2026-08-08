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
use crate::tree::RecursiveExtraction;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
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
    /// True when this source view requires a ZFS key to browse.
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
    container_key_file: Option<PathBuf>,
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

    pub fn extract_tree(
        &self,
        volume: Option<&str>,
        path: &str,
        destination: &Path,
        force: bool,
    ) -> Result<RecursiveExtraction> {
        self.session.extract_tree(volume, path, destination, force)
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
            container_key_file: None,
        })
    }

    /// Open an exported vdev member, vdev image, or supported whole-disk image.
    pub fn open_pool(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_pool_with_container_key_file(path, None)
    }

    /// Open a pool that may be wrapped by a LUKS container. Only the key-file
    /// path is retained; secret bytes are reread and zeroized per operation.
    pub fn open_pool_with_container_key_file(
        path: impl AsRef<Path>,
        container_key_file: Option<&Path>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let container_key =
            read_secret_file(container_key_file, "LUKS container passphrase", 4096)?;
        let pool =
            PoolMember::open_with_container_key(path, container_key.as_deref().map(Vec::as_slice))?;
        let inspection = pool.inspect()?;
        let datasets = pool.datasets()?;
        let snapshots = pool.snapshots(None)?;
        let mut encrypted_datasets = BTreeMap::new();
        for dataset in &datasets {
            encrypted_datasets.insert(
                dataset.name.clone(),
                pool.encryption_requirement(&dataset.name)?.is_some(),
            );
        }
        let mut views = Vec::with_capacity(datasets.len() + snapshots.len());
        for snapshot in snapshots {
            let encrypted = encrypted_datasets
                .get(&snapshot.dataset)
                .copied()
                .unwrap_or(false);
            views.push(SourceView {
                label: format!("{}  —  snapshot", snapshot.full_name),
                selector: snapshot.full_name,
                update_eligible: true,
                encrypted,
            });
        }
        for dataset in datasets {
            let encrypted = encrypted_datasets
                .get(&dataset.name)
                .copied()
                .unwrap_or(false);
            views.push(SourceView {
                label: format!("{}  —  current (read-only)", dataset.name),
                selector: dataset.name,
                update_eligible: false,
                encrypted,
            });
        }
        if views.is_empty() {
            bail!(
                "pool {} contains no browseable filesystem datasets",
                inspection.pool_name
            );
        }
        let vendor = match inspection.backup_format.as_str() {
            "slide_box" => "Slide Box",
            "datto_reverse_roundtrip" => "Datto Reverse RoundTrip",
            _ => "ZFS pool",
        };
        Ok(Self {
            kind: SourceKind::PoolMember,
            path: path.to_owned(),
            title: format!("{vendor} — {}", inspection.pool_name),
            summary: format!(
                "{vendor} · {} · txg {} · {} dataset{} · {} snapshot{}{}",
                inspection.vdev_type,
                inspection.txg,
                inspection.datasets,
                if inspection.datasets == 1 { "" } else { "s" },
                inspection.snapshots,
                if inspection.snapshots == 1 { "" } else { "s" },
                inspection
                    .container_encryption
                    .as_deref()
                    .map_or_else(String::new, |value| format!(" · {value}")),
            ),
            views,
            container_key_file: container_key_file.map(Path::to_owned),
        })
    }

    pub fn view(&self, index: usize) -> Result<&SourceView> {
        self.views
            .get(index)
            .ok_or_else(|| anyhow!("selected source view {index} no longer exists"))
    }

    /// List one directory. `key_file` is consulted only for an encrypted
    /// source and is never retained in the catalog.
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
                let pool = self.open_pool_member()?;
                let key = key_for_pool(&pool, &view.selector, key_file)?;
                pool.list_directory_with_key(
                    &view.selector,
                    path,
                    key.as_deref().map(Vec::as_slice),
                )
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
                let pool = self.open_pool_member()?;
                let key = key_for_pool(&pool, &view.selector, key_file)?;
                let extraction = pool.extract_with_key(
                    &view.selector,
                    source_path,
                    destination,
                    force,
                    key.as_deref().map(Vec::as_slice),
                )?;
                Ok(extraction_from_pool(extraction))
            }
        }
    }

    /// Recursively extract one selected ZFS directory into a staged tree.
    pub fn extract_tree(
        &self,
        view_index: usize,
        source_path: &str,
        destination: &Path,
        force: bool,
        key_file: Option<&Path>,
    ) -> Result<RecursiveExtraction> {
        let view = self.view(view_index)?;
        match self.kind {
            SourceKind::SendStream => {
                let key = key_for_snapshot(&self.path, &view.selector, key_file)?;
                operations::extract_tree_snapshot_with_key(
                    &self.path,
                    source_path,
                    destination,
                    force,
                    Some(&view.selector),
                    key.as_deref().map(Vec::as_slice),
                )
            }
            SourceKind::PoolMember => {
                let pool = self.open_pool_member()?;
                let key = key_for_pool(&pool, &view.selector, key_file)?;
                pool.extract_tree_with_key(
                    &view.selector,
                    source_path,
                    destination,
                    force,
                    key.as_deref().map(Vec::as_slice),
                )
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
        self.inspect_inception_with_datto(
            view_index,
            image_path,
            key_file,
            None,
            None,
            image_offset,
            image_length,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn inspect_inception_with_datto(
        &self,
        view_index: usize,
        image_path: &str,
        key_file: Option<&Path>,
        agent_password_file: Option<&Path>,
        key_stash_path: Option<&str>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<InceptionCatalog> {
        let session = Arc::new(self.open_inception(
            view_index,
            image_path,
            key_file,
            agent_password_file,
            key_stash_path,
            (image_offset, image_length),
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
        agent_password_file: Option<&Path>,
        key_stash_path: Option<&str>,
        image_window: (u64, Option<u64>),
    ) -> Result<InceptionSession> {
        let (image_offset, image_length) = image_window;
        let view = self.view(view_index)?;
        match self.kind {
            SourceKind::SendStream => {
                if agent_password_file.is_some() || key_stash_path.is_some() {
                    bail!("Datto agent credentials are only valid for a pool member");
                }
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
            SourceKind::PoolMember => {
                let pool = self.open_pool_member()?;
                let key = key_for_pool(&pool, &view.selector, key_file)?;
                let agent_password =
                    read_secret_file(agent_password_file, "Datto agent password", 4096)?;
                InceptionSession::from_pool_member_at_with_keys(
                    pool,
                    &view.selector,
                    image_path,
                    key.as_deref().map(Vec::as_slice),
                    agent_password.as_deref().map(Vec::as_slice),
                    key_stash_path,
                    image_offset,
                    image_length,
                )
            }
        }
    }

    fn open_pool_member(&self) -> Result<PoolMember> {
        let key = read_secret_file(
            self.container_key_file.as_deref(),
            "LUKS container passphrase",
            4096,
        )?;
        PoolMember::open_with_container_key(&self.path, key.as_deref().map(Vec::as_slice))
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

fn key_for_pool(
    pool: &PoolMember,
    selector: &str,
    key_file: Option<&Path>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let requirement = pool.encryption_requirement(selector)?;
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
            "{} is encrypted; choose its {} key file first",
            requirement.dataset_name,
            requirement.key_format
        )
    })?;
    let maximum_size = match requirement.key_format.as_str() {
        // Raw ZFS keys may also be supplied in Slide's 64-character hex form.
        "raw" => 66_u64,
        "hex" => 66,
        "passphrase" => 514,
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
    crate::encrypted::normalize_key_file_material(&requirement.key_format, &mut material)?;
    Ok(Some(Zeroizing::new(material)))
}

fn read_secret_file(
    path: Option<&Path>,
    label: &str,
    maximum_size: u64,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut file = File::open(path)
        .with_context(|| format!("opening {label} file {}", path.display()))?
        .take(maximum_size + 1);
    let mut material = Vec::new();
    file.read_to_end(&mut material)
        .with_context(|| format!("reading {label} file {}", path.display()))?;
    if material.len() as u64 > maximum_size {
        bail!("{label} file {} is too large", path.display());
    }
    if material.last() == Some(&b'\n') {
        material.pop();
        if material.last() == Some(&b'\r') {
            material.pop();
        }
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
    use super::{
        InceptionCatalog, SourceCatalog, SourceKind, apply_incremental, child_path, parent_path,
    };
    use crate::inception::{ImageRead, InceptionSession};
    use anyhow::{Result, bail};
    use std::path::Path;
    use std::sync::Arc;

    struct Bytes(Vec<u8>);

    impl ImageRead for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }

        fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
            let start = offset as usize;
            let Some(bytes) = self.0.get(start..start + buffer.len()) else {
                bail!("outside UI test source");
            };
            buffer.copy_from_slice(bytes);
            Ok(())
        }
    }

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
    fn windows_service_catalog_lists_and_extracts_from_a_retained_inception_session() {
        let session = Arc::new(
            InceptionSession::inspect_source(
                Arc::new(Bytes(small_fat12_image())),
                "/vms/disk.raw".to_owned(),
            )
            .unwrap(),
        );
        let catalog = InceptionCatalog {
            image_path: session.image_path().to_owned(),
            image_offset: session.image_offset(),
            stored_size: session.stored_size(),
            disk_size: session.image_size(),
            container: session.container(),
            volumes: session.volumes().to_vec(),
            session,
        };

        assert_eq!(catalog.volumes[0].selector, "raw");
        assert!(
            catalog
                .list_directory(None, "/")
                .unwrap()
                .iter()
                .any(|entry| entry.name == "HELLO.TXT")
        );
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("hello.txt");
        let extraction = catalog.extract(None, "/HELLO.TXT", &output, false).unwrap();
        assert_eq!(extraction.logical_size, 5);
        assert!(!extraction.update_eligible);
        assert_eq!(std::fs::read(output).unwrap(), b"hello");

        let tree = temporary.path().join("tree");
        let recursive = catalog.extract_tree(None, "/", &tree, false).unwrap();
        assert_eq!(recursive.files, 1);
        assert_eq!(recursive.directories, 1);
        assert_eq!(std::fs::read(tree.join("HELLO.TXT")).unwrap(), b"hello");
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

    #[test]
    fn send_catalog_recursively_extracts_a_directory_without_sidecars() {
        let catalog = SourceCatalog::open_send(Path::new("tests/fixtures/tiny-full.zfs")).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("tree");
        let extraction = catalog.extract_tree(0, "/", &target, false, None).unwrap();
        assert_eq!(extraction.files, 3);
        assert_eq!(
            std::fs::read(target.join("hello.txt")).unwrap(),
            b"hello from the base snapshot\n"
        );
        assert!(!target.join("hello.txt.zfse.json").exists());
    }

    fn small_fat12_image() -> Vec<u8> {
        const SECTOR: usize = 512;
        const SECTORS: usize = 64;
        let mut image = vec![0_u8; SECTOR * SECTORS];
        let boot = &mut image[..SECTOR];
        boot[0..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
        boot[3..11].copy_from_slice(b"ZFSETEST");
        boot[11..13].copy_from_slice(&512_u16.to_le_bytes());
        boot[13] = 1;
        boot[14..16].copy_from_slice(&1_u16.to_le_bytes());
        boot[16] = 2;
        boot[17..19].copy_from_slice(&16_u16.to_le_bytes());
        boot[19..21].copy_from_slice(&(SECTORS as u16).to_le_bytes());
        boot[21] = 0xf8;
        boot[22..24].copy_from_slice(&1_u16.to_le_bytes());
        boot[24..26].copy_from_slice(&1_u16.to_le_bytes());
        boot[26..28].copy_from_slice(&1_u16.to_le_bytes());
        boot[38] = 0x29;
        boot[43..54].copy_from_slice(b"ZFSE TEST  ");
        boot[54..62].copy_from_slice(b"FAT12   ");
        boot[510..512].copy_from_slice(&[0x55, 0xaa]);

        for fat_sector in [1_usize, 2] {
            let fat = &mut image[fat_sector * SECTOR..(fat_sector + 1) * SECTOR];
            fat[..5].copy_from_slice(&[0xf8, 0xff, 0xff, 0xff, 0x0f]);
        }
        let entry = &mut image[3 * SECTOR..3 * SECTOR + 32];
        entry[..11].copy_from_slice(b"HELLO   TXT");
        entry[11] = 0x20;
        entry[26..28].copy_from_slice(&2_u16.to_le_bytes());
        entry[28..32].copy_from_slice(&5_u32.to_le_bytes());
        image[4 * SECTOR..4 * SECTOR + 5].copy_from_slice(b"hello");
        image
    }
}
