//! Read-only exploration of filesystems stored inside regular ZFS files.
//!
//! An outer ZFS file is presented as a bounded positioned reader. Partition
//! tables and inner filesystems are then parsed directly from that reader, so
//! even a multi-terabyte sparse image does not need to be exported first.

use crate::compression::{decode_embedded_write, decode_replay_write};
use crate::datto::{DettoImage, derive_agent_key};
use crate::encrypted::{DatasetKey, EncryptionParams, decompress_block, is_encrypted_object_type};
use crate::filesystem::{DirectoryEntry, ObjectIndex, ResolvedPath, SnapshotPlan, plan_snapshot};
use crate::pool::PoolMember;
use crate::sparse;
use crate::stream::{DMU_SUBSTREAM, FEATURE_RAW, RECORD_SIZE, RecordKind, StreamReader};
use crate::tree::{RecursiveExtraction, extract_directory_tree};
use anyhow::{Context, Result, anyhow, bail};
use ext4_view::{Ext4, Ext4Read};
use fat::{FatFs, FatVariant, FileId};
use fs_core::{BlockDevice, BlockRead};
use ntfs::{Ntfs, NtfsAttributeFlags, NtfsFile, NtfsReadSeek, indexes::NtfsFileNameIndex};
use qcow2::Qcow2Reader;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;
use vmdk::VmdkReader;

const MAX_GPT_ENTRIES: u32 = 16_384;
const MAX_GPT_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EBR_PARTITIONS: usize = 128;
const COPY_BUFFER_SIZE: usize = 1024 * 1024;

/// A finite read-only byte source. Implementations must either fill the entire
/// buffer or fail; callers never receive short reads from a virtual image.
pub(crate) trait ImageRead: Send + Sync {
    fn len(&self) -> u64;
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemKind {
    Ntfs,
    Fat12,
    Fat16,
    Fat32,
    Exfat,
    Ext4,
}

impl fmt::Display for FilesystemKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Ntfs => "NTFS",
            Self::Fat12 => "FAT12",
            Self::Fat16 => "FAT16",
            Self::Fat32 => "FAT32",
            Self::Exfat => "exFAT",
            Self::Ext4 => "ext4/ext2",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskContainerKind {
    Raw,
    Qcow2,
    Vmdk,
}

impl fmt::Display for DiskContainerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Raw => "raw",
            Self::Qcow2 => "QCOW2",
            Self::Vmdk => "VMDK monolithicSparse",
        })
    }
}

/// One raw filesystem or partition found in the nested image.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    /// Stable selector accepted by `--volume` and the GUI.
    pub selector: String,
    /// `raw`, `gpt`, or `mbr`.
    pub scheme: String,
    /// Partition number, or zero for a raw/superfloppy filesystem.
    pub partition: u32,
    pub offset: u64,
    pub length: u64,
    pub partition_type: String,
    pub name: String,
    pub filesystem: Option<FilesystemKind>,
    /// A concise parser diagnostic for an unsupported or damaged volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

impl VolumeInfo {
    pub fn label(&self) -> String {
        let filesystem = self
            .filesystem
            .map_or_else(|| "unknown filesystem".to_owned(), |kind| kind.to_string());
        let name = if self.name.is_empty() {
            String::new()
        } else {
            format!(" · {}", self.name)
        };
        format!(
            "{} · {} · {} bytes{}",
            self.selector, filesystem, self.length, name
        )
    }
}

#[derive(Debug, Clone)]
pub struct NestedExtraction {
    pub logical_size: u64,
    pub sha256: String,
    pub filesystem: FilesystemKind,
    pub volume: String,
}

/// An inspected inner disk image. All source access remains read-only.
pub struct InceptionSession {
    source: Arc<dyn ImageRead>,
    image_path: String,
    stored_size: u64,
    disk_size: u64,
    container: DiskContainerKind,
    image_offset: u64,
    volumes: Vec<VolumeInfo>,
}

impl InceptionSession {
    /// Open a standalone local disk image. This uses the same bounded,
    /// read-only container, partition, and filesystem stack as images stored
    /// inside ZFS; the file is never attached or mounted by the host.
    pub fn from_file_at(path: &Path, image_offset: u64, image_length: Option<u64>) -> Result<Self> {
        let source = Arc::new(FileImage::open(path)?) as Arc<dyn ImageRead>;
        Self::inspect_source_at(
            source,
            path.display().to_string(),
            image_offset,
            image_length,
        )
    }

    pub fn from_send(
        stream: &Path,
        snapshot: Option<&str>,
        image_path: &str,
        key_material: Option<&[u8]>,
    ) -> Result<Self> {
        Self::from_send_at(stream, snapshot, image_path, key_material, 0, None)
    }

    pub fn from_send_at(
        stream: &Path,
        snapshot: Option<&str>,
        image_path: &str,
        key_material: Option<&[u8]>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        let plan = plan_snapshot(stream, snapshot)?;
        let index = ObjectIndex::build_plan_with_key(stream, &plan, key_material)?;
        let resolved = index.resolve_path(image_path)?;
        if resolved.dirent_type != 8 {
            bail!("{} is not a regular file", resolved.normalized_path);
        }
        let normalized_path = resolved.normalized_path.clone();
        let source = Arc::new(StreamImage::build(stream, &plan, &resolved, key_material)?)
            as Arc<dyn ImageRead>;
        Self::inspect_source_at(source, normalized_path, image_offset, image_length)
    }

    pub fn from_pool(member: &Path, dataset: &str, image_path: &str) -> Result<Self> {
        Self::from_pool_at(member, dataset, image_path, 0, None)
    }

    pub fn from_pool_with_key(
        member: &Path,
        dataset: &str,
        image_path: &str,
        key_material: Option<&[u8]>,
    ) -> Result<Self> {
        Self::from_pool_at_with_key(member, dataset, image_path, key_material, 0, None)
    }

    pub fn from_pool_at(
        member: &Path,
        dataset: &str,
        image_path: &str,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        Self::from_pool_at_with_key(
            member,
            dataset,
            image_path,
            None,
            image_offset,
            image_length,
        )
    }

    pub fn from_pool_at_with_key(
        member: &Path,
        dataset: &str,
        image_path: &str,
        key_material: Option<&[u8]>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        let pool = PoolMember::open(member)?;
        Self::from_pool_member_at_with_keys(
            pool,
            dataset,
            image_path,
            key_material,
            None,
            None,
            image_offset,
            image_length,
        )
    }

    /// Open a disk image from an already-unlocked pool. A `.detto` image is
    /// decrypted on demand when an agent password is provided; ordinary raw,
    /// QCOW2, and VMDK files keep the existing path.
    #[allow(clippy::too_many_arguments)]
    pub fn from_pool_member_at_with_keys(
        pool: PoolMember,
        dataset: &str,
        image_path: &str,
        key_material: Option<&[u8]>,
        datto_agent_password: Option<&[u8]>,
        key_stash_path: Option<&str>,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        let is_detto = image_path.to_ascii_lowercase().ends_with(".detto");
        let datto_key = if is_detto {
            let password = datto_agent_password.ok_or_else(|| {
                anyhow!(
                    "{image_path} is an encrypted Datto .detto image; provide --agent-password-file"
                )
            })?;
            let stash_path = match key_stash_path {
                Some(path) => path.to_owned(),
                None => find_datto_key_stash(&pool, dataset, key_material)?,
            };
            let stash =
                pool.read_small_file_with_key(dataset, &stash_path, key_material, 4 * 1024 * 1024)?;
            Some(
                derive_agent_key(&stash, password)
                    .with_context(|| format!("unlocking {image_path} with {stash_path}"))?,
            )
        } else {
            if datto_agent_password.is_some() || key_stash_path.is_some() {
                bail!("Datto agent credentials are only valid for a .detto image");
            }
            None
        };
        let source = pool.into_image_file_with_key(dataset, image_path, key_material)?;
        let mut source = Arc::new(source) as Arc<dyn ImageRead>;
        if let Some(key) = datto_key {
            source = Arc::new(DettoImage::new(source, key)?) as Arc<dyn ImageRead>;
        }
        Self::inspect_source_at(source, image_path.to_owned(), image_offset, image_length)
    }

    /// Build a session around any bounded source so tests can exercise nested
    /// formats without manufacturing ZFS media.
    #[cfg(test)]
    pub(crate) fn inspect_source(source: Arc<dyn ImageRead>, image_path: String) -> Result<Self> {
        Self::inspect_source_at(source, image_path, 0, None)
    }

    pub(crate) fn inspect_source_at(
        source: Arc<dyn ImageRead>,
        image_path: String,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        if image_offset > source.len() {
            bail!(
                "nested image offset {image_offset} exceeds ZFS file size {}",
                source.len()
            );
        }
        let stored_size = image_length.unwrap_or(source.len() - image_offset);
        let window =
            Arc::new(SlicedImage::new(source, image_offset, stored_size)?) as Arc<dyn ImageRead>;
        if stored_size < 512 {
            bail!("nested image window is only {stored_size} bytes");
        }
        let (container, source) = open_disk_container(window)?;
        let disk_size = source.len();
        if disk_size < 512 {
            bail!("nested virtual disk is only {disk_size} bytes");
        }
        let layouts = discover_partitions(&source)?;
        let layouts = if layouts.is_empty() {
            vec![PartitionLayout {
                selector: "raw".to_owned(),
                scheme: "raw".to_owned(),
                partition: 0,
                offset: 0,
                length: disk_size,
                partition_type: "unpartitioned".to_owned(),
                name: String::new(),
            }]
        } else {
            layouts
        };

        let volumes = layouts
            .into_iter()
            .map(|layout| probe_volume(&source, layout))
            .collect::<Vec<_>>();
        Ok(Self {
            source,
            image_path,
            stored_size,
            disk_size,
            container,
            image_offset,
            volumes,
        })
    }

    pub fn image_path(&self) -> &str {
        &self.image_path
    }

    pub fn image_size(&self) -> u64 {
        self.disk_size
    }

    pub fn stored_size(&self) -> u64 {
        self.stored_size
    }

    pub fn container(&self) -> DiskContainerKind {
        self.container
    }

    pub fn image_offset(&self) -> u64 {
        self.image_offset
    }

    pub fn volumes(&self) -> &[VolumeInfo] {
        &self.volumes
    }

    pub fn list_directory(&self, volume: Option<&str>, path: &str) -> Result<Vec<DirectoryEntry>> {
        let volume = self.select_volume(volume)?;
        let filesystem = volume.filesystem.ok_or_else(|| {
            anyhow!(
                "volume {} has no supported filesystem ({})",
                volume.selector,
                volume
                    .diagnostic
                    .as_deref()
                    .unwrap_or("filesystem signature was not recognized")
            )
        })?;
        let source = self.volume_source(volume)?;
        match filesystem {
            FilesystemKind::Ntfs => list_ntfs(source, path),
            FilesystemKind::Fat12
            | FilesystemKind::Fat16
            | FilesystemKind::Fat32
            | FilesystemKind::Exfat => list_fat(source, path),
            FilesystemKind::Ext4 => list_ext4(source, path),
        }
    }

    pub fn extract(
        &self,
        volume: Option<&str>,
        path: &str,
        output: &Path,
        force: bool,
    ) -> Result<NestedExtraction> {
        let volume = self.select_volume(volume)?;
        let filesystem = volume.filesystem.ok_or_else(|| {
            anyhow!(
                "volume {} does not contain a supported filesystem",
                volume.selector
            )
        })?;
        if output.exists() && !force {
            bail!(
                "output {} already exists (pass --force to replace it)",
                output.display()
            );
        }
        let source = self.volume_source(volume)?;
        let mut destination = ExtractionTarget::new(output)?;
        let logical_size = match filesystem {
            FilesystemKind::Ntfs => extract_ntfs(source, path, &mut destination)?,
            FilesystemKind::Fat12
            | FilesystemKind::Fat16
            | FilesystemKind::Fat32
            | FilesystemKind::Exfat => extract_fat(source, path, &mut destination)?,
            FilesystemKind::Ext4 => extract_ext4(source, path, &mut destination)?,
        };
        let sha256 = destination.finish(output, logical_size, force)?;
        Ok(NestedExtraction {
            logical_size,
            sha256,
            filesystem,
            volume: volume.selector.clone(),
        })
    }

    /// Recursively extract a directory without following symlinks or special
    /// entries. The destination tree is staged and published only after every
    /// regular file succeeds.
    pub fn extract_tree(
        &self,
        volume: Option<&str>,
        path: &str,
        output: &Path,
        force: bool,
    ) -> Result<RecursiveExtraction> {
        // Force volume selection once so an ambiguous image fails before the
        // staging directory is created.
        self.select_volume(volume)?;
        extract_directory_tree(
            path,
            output,
            force,
            |directory| self.list_directory(volume, directory),
            |source, destination| {
                self.extract(volume, source, destination, false)
                    .map(|result| result.logical_size)
            },
        )
    }

    /// Inspect a regular file inside the active subordinate filesystem as one
    /// more virtual disk layer. Reads remain positioned all the way back to the
    /// original source, so even a very large sparse child image is not staged
    /// or materialized before it can be browsed.
    pub fn inspect_child_image_at(
        &self,
        volume: Option<&str>,
        image_path: &str,
        image_offset: u64,
        image_length: Option<u64>,
    ) -> Result<Self> {
        let volume = self.select_volume(volume)?;
        let filesystem = volume.filesystem.ok_or_else(|| {
            anyhow!(
                "volume {} does not contain a supported filesystem",
                volume.selector
            )
        })?;
        let source = self.volume_source(volume)?;
        let normalized = normalized_inner_path(image_path)?;
        let length = inner_file_length(source.clone(), filesystem, &normalized)?;
        let image = Arc::new(FilesystemFileImage {
            source,
            filesystem,
            path: normalized.clone(),
            len: length,
        }) as Arc<dyn ImageRead>;
        Self::inspect_source_at(
            image,
            format!("{}!{}:{}", self.image_path, volume.selector, normalized),
            image_offset,
            image_length,
        )
    }

    fn select_volume(&self, selector: Option<&str>) -> Result<&VolumeInfo> {
        if let Some(selector) = selector {
            return self
                .volumes
                .iter()
                .find(|volume| volume.selector.eq_ignore_ascii_case(selector))
                .ok_or_else(|| {
                    anyhow!(
                        "volume {selector:?} was not found (available: {})",
                        self.volumes
                            .iter()
                            .map(|volume| volume.selector.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
        }
        let supported = self
            .volumes
            .iter()
            .filter(|volume| volume.filesystem.is_some())
            .collect::<Vec<_>>();
        match supported.as_slice() {
            [only] => Ok(only),
            [] => bail!(
                "{} contains no supported NTFS, FAT, exFAT, or ext filesystem",
                self.image_path
            ),
            _ => bail!(
                "{} contains multiple supported volumes ({}); choose one with --volume",
                self.image_path,
                supported
                    .iter()
                    .map(|volume| volume.selector.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn volume_source(&self, volume: &VolumeInfo) -> Result<Arc<dyn ImageRead>> {
        Ok(Arc::new(SlicedImage::new(
            self.source.clone(),
            volume.offset,
            volume.length,
        )?))
    }
}

struct FileImage {
    file: Mutex<File>,
    len: u64,
}

impl FileImage {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("opening standalone disk image {}", path.display()))?;
        let len = file
            .metadata()
            .with_context(|| format!("reading disk-image size for {}", path.display()))?
            .len();
        Ok(Self {
            file: Mutex::new(file),
            len,
        })
    }
}

impl ImageRead for FileImage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .context("standalone image read offset overflows")?;
        if end > self.len {
            bail!(
                "standalone image read [{offset}, {end}) exceeds {} bytes",
                self.len
            );
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| anyhow!("standalone image file lock was poisoned"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buffer)?;
        Ok(())
    }
}

struct FilesystemFileImage {
    source: Arc<dyn ImageRead>,
    filesystem: FilesystemKind,
    path: String,
    len: u64,
}

impl ImageRead for FilesystemFileImage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .context("inner image-file read offset overflows")?;
        if end > self.len {
            bail!(
                "inner image-file read [{offset}, {end}) exceeds {} bytes",
                self.len
            );
        }
        read_inner_file_exact_at(
            self.source.clone(),
            self.filesystem,
            &self.path,
            offset,
            buffer,
        )
    }
}

fn find_datto_key_stash(
    pool: &PoolMember,
    dataset: &str,
    key_material: Option<&[u8]>,
) -> Result<String> {
    let entries = pool
        .list_directory_with_key(dataset, "/config", key_material)
        .context("listing Datto agent /config directory")?;
    let matches = entries
        .into_iter()
        .filter(|entry| {
            entry.dirent_type == 8
                && entry
                    .name
                    .to_ascii_lowercase()
                    .ends_with(".encryptionkeystash")
        })
        .map(|entry| format!("/config/{}", entry.name))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        [] => {
            bail!("no *.encryptionKeyStash file was found in {dataset}/config; provide --key-stash")
        }
        _ => bail!(
            "multiple *.encryptionKeyStash files were found in {dataset}/config; choose one with --key-stash"
        ),
    }
}

fn open_disk_container(
    source: Arc<dyn ImageRead>,
) -> Result<(DiskContainerKind, Arc<dyn ImageRead>)> {
    let mut header = [0_u8; 512];
    source.read_exact_at(0, &mut header)?;
    if header[..4] == [b'Q', b'F', b'I', 0xfb] {
        let version = u32::from_be_bytes(header[4..8].try_into().unwrap());
        if !(2..=3).contains(&version) {
            bail!(
                "QCOW version {version} is not supported; inception mode requires a self-contained QCOW2 v2 or v3 image"
            );
        }
        let reader = Qcow2Reader::from_reader(SourceCursor::new(source))
            .map_err(|error| anyhow!(error))
            .context("opening QCOW2 container")?;
        let length = reader.virtual_disk_size();
        return Ok((
            DiskContainerKind::Qcow2,
            Arc::new(QcowImage {
                reader: Mutex::new(reader),
                length,
            }),
        ));
    }
    if &header[..4] == b"KDMV" {
        let device = Arc::new(ImageBlockDevice { source });
        let reader = VmdkReader::open_on_device(device)
            .map_err(|error| anyhow!(error))
            .context("opening VMDK container")?;
        return Ok((DiskContainerKind::Vmdk, Arc::new(VmdkImage(reader))));
    }
    if header.starts_with(b"# Disk DescriptorFile")
        || header.windows(10).any(|window| window == b"createType")
    {
        bail!(
            "this is a VMDK descriptor that references external extent files; inception mode currently supports self-contained monolithicSparse VMDK files only"
        );
    }
    Ok((DiskContainerKind::Raw, source))
}

struct QcowImage {
    reader: Mutex<Qcow2Reader>,
    length: u64,
}

impl ImageRead for QcowImage {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| anyhow!("QCOW2 virtual read offset overflows"))?;
        if end > self.length {
            bail!(
                "QCOW2 virtual read [{offset}, {end}) exceeds {} bytes",
                self.length
            );
        }
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| anyhow!("QCOW2 reader lock was poisoned"))?;
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(buffer)?;
        Ok(())
    }
}

struct ImageBlockDevice {
    source: Arc<dyn ImageRead>,
}

impl BlockRead for ImageBlockDevice {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> fs_core::Result<()> {
        self.source
            .read_exact_at(offset, buffer)
            .map_err(|error| fs_core::Error::Custom(format!("{error:#}")))
    }

    fn size_bytes(&self) -> u64 {
        self.source.len()
    }
}

impl BlockDevice for ImageBlockDevice {}

struct VmdkImage(VmdkReader);

impl ImageRead for VmdkImage {
    fn len(&self) -> u64 {
        self.0.virtual_size()
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        self.0
            .read_at(offset, buffer)
            .map_err(|error| anyhow!(error))
    }
}

struct ExtractionTarget {
    temporary: NamedTempFile,
    digest: Sha256,
    offset: u64,
}

impl ExtractionTarget {
    fn new(output: &Path) -> Result<Self> {
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = NamedTempFile::new_in(parent)
            .with_context(|| format!("creating temporary file in {}", parent.display()))?;
        sparse::prepare(temporary.as_file())?;
        Ok(Self {
            temporary,
            digest: Sha256::new(),
            offset: 0,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        sparse::write_extent(self.temporary.as_file_mut(), self.offset, bytes)?;
        self.digest.update(bytes);
        self.offset = self
            .offset
            .checked_add(bytes.len() as u64)
            .context("nested extraction offset overflow")?;
        Ok(())
    }

    fn finish(mut self, output: &Path, length: u64, force: bool) -> Result<String> {
        if self.offset != length {
            bail!(
                "inner filesystem returned {} bytes for a {length}-byte file",
                self.offset
            );
        }
        self.temporary.as_file_mut().set_len(length)?;
        self.temporary.as_file_mut().sync_all()?;
        if output.exists() && !force {
            bail!("{} already exists", output.display());
        }
        self.temporary
            .persist(output)
            .map_err(|error| anyhow!("persisting {}: {}", output.display(), error.error))?;
        Ok(format!("{:x}", self.digest.finalize()))
    }
}

#[derive(Clone)]
struct SlicedImage {
    source: Arc<dyn ImageRead>,
    base: u64,
    len: u64,
}

impl SlicedImage {
    fn new(source: Arc<dyn ImageRead>, base: u64, len: u64) -> Result<Self> {
        let end = base
            .checked_add(len)
            .ok_or_else(|| anyhow!("nested partition byte range overflows"))?;
        if len == 0 || end > source.len() {
            bail!(
                "nested partition [{base}, {end}) is outside the {}-byte image",
                source.len()
            );
        }
        Ok(Self { source, base, len })
    }
}

impl ImageRead for SlicedImage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| anyhow!("nested partition read offset overflows"))?;
        if end > self.len {
            bail!(
                "nested partition read [{offset}, {end}) exceeds {} bytes",
                self.len
            );
        }
        self.source.read_exact_at(self.base + offset, buffer)
    }
}

#[derive(Clone)]
struct SourceCursor {
    source: Arc<dyn ImageRead>,
    position: u64,
}

impl SourceCursor {
    fn new(source: Arc<dyn ImageRead>) -> Self {
        Self {
            source,
            position: 0,
        }
    }
}

impl Read for SourceCursor {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.source.len() || buffer.is_empty() {
            return Ok(0);
        }
        let count = usize::try_from((self.source.len() - self.position).min(buffer.len() as u64))
            .unwrap_or(buffer.len());
        self.source
            .read_exact_at(self.position, &mut buffer[..count])
            .map_err(|error| io::Error::other(format!("{error:#}")))?;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for SourceCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => i128::from(self.source.len()) + i128::from(value),
        };
        if next < 0 || next > i128::from(self.source.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek is outside the bounded nested volume",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

struct Ext4Source(SourceCursor);

impl Ext4Read for Ext4Source {
    fn read(
        &mut self,
        start_byte: u64,
        destination: &mut [u8],
    ) -> std::result::Result<(), Box<dyn Error + Send + Sync + 'static>> {
        self.0
            .source
            .read_exact_at(start_byte, destination)
            .map_err(|error| Box::new(io::Error::other(format!("{error:#}"))) as _)
    }
}

fn normalize_inner_path(path: &str) -> Result<Vec<&str>> {
    if !path.starts_with('/') {
        bail!("inner filesystem path must be absolute");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("inner filesystem path cannot contain '..'"),
            value if value.contains('\0') => bail!("inner filesystem path contains a NUL byte"),
            value => parts.push(value),
        }
    }
    Ok(parts)
}

fn normalized_inner_path(path: &str) -> Result<String> {
    let parts = normalize_inner_path(path)?;
    Ok(if parts.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", parts.join("/"))
    })
}

fn list_fat(source: Arc<dyn ImageRead>, path: &str) -> Result<Vec<DirectoryEntry>> {
    let fs = FatFs::open(SourceCursor::new(source)).context("opening FAT-family filesystem")?;
    let id = resolve_fat(&fs, path)?;
    let meta = fs.meta(id)?;
    if !meta.is_dir {
        bail!("{} is not a directory", normalized_inner_path(path)?);
    }
    let mut entries = fs
        .read_dir(id)?
        .into_iter()
        .filter(|node| {
            !node.is_deleted && !node.is_volume_label && node.name != "." && node.name != ".."
        })
        .map(|node| DirectoryEntry {
            name: node.name,
            object_id: u64::from(node.first_cluster),
            dirent_type: if node.is_dir { 4 } else { 8 },
            logical_size: (!node.is_dir).then_some(node.size),
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(entries)
}

fn resolve_fat<R: Read + Seek>(fs: &FatFs<R>, path: &str) -> Result<FileId> {
    let mut current = fs.root();
    for component in normalize_inner_path(path)? {
        if let Some(exact) = fs.lookup(current, component.as_bytes())? {
            current = exact;
            continue;
        }

        // fat-core deliberately exposes the on-disk spelling. FAT and exFAT
        // lookup semantics are case-insensitive, however, so fall back to the
        // effective long name and its 8.3 alias instead of making CLI/UI paths
        // depend on the stored capitalization.
        let matches = fs
            .read_dir(current)?
            .into_iter()
            .filter(|node| {
                !node.is_deleted
                    && !node.is_volume_label
                    && (fat_name_matches(&node.name, component)
                        || fat_name_matches(&node.short_name, component))
            })
            .map(|node| node.id)
            .collect::<Vec<_>>();
        current = match matches.as_slice() {
            [only] => *only,
            [] => bail!("inner path {path:?} was not found"),
            _ => {
                bail!("inner path component {component:?} is ambiguous in a damaged FAT directory")
            }
        };
    }
    Ok(current)
}

fn fat_name_matches(stored: &str, requested: &str) -> bool {
    stored.eq_ignore_ascii_case(requested) || stored.to_lowercase() == requested.to_lowercase()
}

fn extract_fat(
    source: Arc<dyn ImageRead>,
    path: &str,
    target: &mut ExtractionTarget,
) -> Result<u64> {
    let fs = FatFs::open(SourceCursor::new(source)).context("opening FAT-family filesystem")?;
    let id = resolve_fat(&fs, path)?;
    let meta = fs.meta(id)?;
    if meta.is_dir {
        bail!("{} is not a regular file", normalized_inner_path(path)?);
    }
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    while offset < meta.size {
        let wanted = usize::try_from((meta.size - offset).min(buffer.len() as u64))?;
        let count = fs.read_at(id, offset, &mut buffer[..wanted])?;
        if count == 0 {
            bail!("FAT cluster chain ended at byte {offset} of {}", meta.size);
        }
        target.write(&buffer[..count])?;
        offset += count as u64;
    }
    Ok(meta.size)
}

fn open_ntfs(source: Arc<dyn ImageRead>) -> Result<(Ntfs, SourceCursor)> {
    let mut reader = SourceCursor::new(source);
    let mut ntfs = Ntfs::new(&mut reader).context("opening NTFS filesystem")?;
    ntfs.read_upcase_table(&mut reader)
        .context("reading the NTFS $UpCase table")?;
    Ok((ntfs, reader))
}

fn resolve_ntfs<'n>(ntfs: &'n Ntfs, reader: &mut SourceCursor, path: &str) -> Result<NtfsFile<'n>> {
    let mut current = ntfs.root_directory(reader)?;
    for component in normalize_inner_path(path)? {
        let next = {
            let index = current.directory_index(reader)?;
            let mut finder = index.finder();
            let entry = NtfsFileNameIndex::find(&mut finder, ntfs, reader, component)
                .transpose()?
                .ok_or_else(|| anyhow!("inner path {path:?} was not found"))?;
            entry.to_file(ntfs, reader)?
        };
        current = next;
    }
    Ok(current)
}

fn list_ntfs(source: Arc<dyn ImageRead>, path: &str) -> Result<Vec<DirectoryEntry>> {
    let (ntfs, mut reader) = open_ntfs(source)?;
    let directory = resolve_ntfs(&ntfs, &mut reader, path)?;
    if !directory.is_directory() {
        bail!("{} is not a directory", normalized_inner_path(path)?);
    }
    let index = directory.directory_index(&mut reader)?;
    let mut iterator = index.entries();
    let mut entries = Vec::new();
    while let Some(entry) = iterator.next(&mut reader) {
        let entry = entry?;
        let Some(file_name) = entry.key().transpose()? else {
            continue;
        };
        let name = file_name.name().to_string_lossy();
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        entries.push(DirectoryEntry {
            name,
            object_id: entry.file_reference().file_record_number(),
            dirent_type: if file_name.is_directory() { 4 } else { 8 },
            logical_size: (!file_name.is_directory()).then_some(file_name.data_size()),
        });
    }
    Ok(entries)
}

fn extract_ntfs(
    source: Arc<dyn ImageRead>,
    path: &str,
    target: &mut ExtractionTarget,
) -> Result<u64> {
    let (ntfs, mut reader) = open_ntfs(source)?;
    let file = resolve_ntfs(&ntfs, &mut reader, path)?;
    if file.is_directory() {
        bail!("{} is not a regular file", normalized_inner_path(path)?);
    }
    let data_item = file
        .data(&mut reader, "")
        .transpose()?
        .ok_or_else(|| anyhow!("{path:?} has no unnamed NTFS $DATA stream"))?;
    let data_attribute = data_item.to_attribute()?;
    let flags = data_attribute.flags();
    if flags.contains(NtfsAttributeFlags::COMPRESSED) {
        bail!("{path:?} uses NTFS compression, which inception mode cannot decode yet");
    }
    if flags.contains(NtfsAttributeFlags::ENCRYPTED) {
        bail!("{path:?} is EFS-encrypted; an unencrypted NTFS $DATA stream is required");
    }
    let mut value = data_attribute.value(&mut reader)?;
    let length = value.len();
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        let count = value.read(&mut reader, &mut buffer)?;
        if count == 0 {
            break;
        }
        target.write(&buffer[..count])?;
    }
    Ok(length)
}

fn open_ext4(source: Arc<dyn ImageRead>) -> Result<Ext4> {
    Ext4::load(Box::new(Ext4Source(SourceCursor::new(source))))
        .map_err(|error| anyhow!(error))
        .context("opening ext filesystem")
}

fn list_ext4(source: Arc<dyn ImageRead>, path: &str) -> Result<Vec<DirectoryEntry>> {
    let fs = open_ext4(source)?;
    let path = normalized_inner_path(path)?;
    let metadata = fs
        .symlink_metadata(path.as_bytes())
        .map_err(|error| anyhow!(error))?;
    if !metadata.is_dir() {
        bail!("{path} is not a directory");
    }
    let mut entries = Vec::new();
    for entry in fs
        .read_dir(path.as_bytes())
        .map_err(|error| anyhow!(error))?
    {
        let entry = entry.map_err(|error| anyhow!(error))?;
        let name = entry
            .file_name()
            .as_str()
            .map_err(|_| {
                anyhow!("ext directory contains a non-UTF-8 name that this UI cannot represent")
            })?
            .to_owned();
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| anyhow!(error))?;
        let metadata = entry.metadata().map_err(|error| anyhow!(error))?;
        entries.push(DirectoryEntry {
            object_id: stable_path_id(&entry.path().display().to_string()),
            name,
            dirent_type: if file_type.is_dir() {
                4
            } else if file_type.is_regular_file() {
                8
            } else if file_type.is_symlink() {
                10
            } else {
                0
            },
            logical_size: file_type.is_regular_file().then_some(metadata.len()),
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn extract_ext4(
    source: Arc<dyn ImageRead>,
    path: &str,
    target: &mut ExtractionTarget,
) -> Result<u64> {
    let fs = open_ext4(source)?;
    let path = normalized_inner_path(path)?;
    let metadata = fs
        .symlink_metadata(path.as_bytes())
        .map_err(|error| anyhow!(error))?;
    if !metadata.file_type().is_regular_file() {
        bail!("{path} is not a regular file (symlinks are not followed for extraction)");
    }
    let length = metadata.len();
    let mut file = fs.open(path.as_bytes()).map_err(|error| anyhow!(error))?;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        let count = file
            .read_bytes(&mut buffer)
            .map_err(|error| anyhow!(error))?;
        if count == 0 {
            break;
        }
        target.write(&buffer[..count])?;
    }
    Ok(length)
}

fn inner_file_length(
    source: Arc<dyn ImageRead>,
    filesystem: FilesystemKind,
    path: &str,
) -> Result<u64> {
    match filesystem {
        FilesystemKind::Fat12
        | FilesystemKind::Fat16
        | FilesystemKind::Fat32
        | FilesystemKind::Exfat => {
            let fs =
                FatFs::open(SourceCursor::new(source)).context("opening FAT-family filesystem")?;
            let id = resolve_fat(&fs, path)?;
            let metadata = fs.meta(id)?;
            if metadata.is_dir {
                bail!("{path} is not a regular file");
            }
            Ok(metadata.size)
        }
        FilesystemKind::Ntfs => {
            let (ntfs, mut reader) = open_ntfs(source)?;
            let file = resolve_ntfs(&ntfs, &mut reader, path)?;
            if file.is_directory() {
                bail!("{path} is not a regular file");
            }
            let item = file
                .data(&mut reader, "")
                .transpose()?
                .ok_or_else(|| anyhow!("{path:?} has no unnamed NTFS $DATA stream"))?;
            let attribute = item.to_attribute()?;
            validate_ntfs_image_attribute(path, attribute.flags())?;
            Ok(attribute.value_length())
        }
        FilesystemKind::Ext4 => {
            let fs = open_ext4(source)?;
            let metadata = fs
                .symlink_metadata(path.as_bytes())
                .map_err(|error| anyhow!(error))?;
            if !metadata.file_type().is_regular_file() {
                bail!("{path} is not a regular file (symlinks are not followed)");
            }
            Ok(metadata.len())
        }
    }
}

fn read_inner_file_exact_at(
    source: Arc<dyn ImageRead>,
    filesystem: FilesystemKind,
    path: &str,
    offset: u64,
    buffer: &mut [u8],
) -> Result<()> {
    match filesystem {
        FilesystemKind::Fat12
        | FilesystemKind::Fat16
        | FilesystemKind::Fat32
        | FilesystemKind::Exfat => {
            let fs =
                FatFs::open(SourceCursor::new(source)).context("opening FAT-family filesystem")?;
            let id = resolve_fat(&fs, path)?;
            let mut complete = 0_usize;
            while complete < buffer.len() {
                let count = fs.read_at(id, offset + complete as u64, &mut buffer[complete..])?;
                if count == 0 {
                    bail!(
                        "FAT image file ended before byte {}",
                        offset + buffer.len() as u64
                    );
                }
                complete += count;
            }
        }
        FilesystemKind::Ntfs => {
            let (ntfs, mut reader) = open_ntfs(source)?;
            let file = resolve_ntfs(&ntfs, &mut reader, path)?;
            let item = file
                .data(&mut reader, "")
                .transpose()?
                .ok_or_else(|| anyhow!("{path:?} has no unnamed NTFS $DATA stream"))?;
            let attribute = item.to_attribute()?;
            validate_ntfs_image_attribute(path, attribute.flags())?;
            let mut value = attribute.value(&mut reader)?;
            value.seek(&mut reader, SeekFrom::Start(offset))?;
            value.read_exact(&mut reader, buffer)?;
        }
        FilesystemKind::Ext4 => {
            let fs = open_ext4(source)?;
            let mut file = fs.open(path.as_bytes()).map_err(|error| anyhow!(error))?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(buffer)?;
        }
    }
    Ok(())
}

fn validate_ntfs_image_attribute(path: &str, flags: NtfsAttributeFlags) -> Result<()> {
    if flags.contains(NtfsAttributeFlags::COMPRESSED) {
        bail!("{path:?} uses NTFS compression, which inception mode cannot decode yet");
    }
    if flags.contains(NtfsAttributeFlags::ENCRYPTED) {
        bail!("{path:?} is EFS-encrypted; an unencrypted NTFS $DATA stream is required");
    }
    Ok(())
}

fn stable_path_id(path: &str) -> u64 {
    // FNV-1a is only a display identifier here, never an integrity primitive.
    path.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[derive(Debug, Clone)]
struct PartitionLayout {
    selector: String,
    scheme: String,
    partition: u32,
    offset: u64,
    length: u64,
    partition_type: String,
    name: String,
}

fn discover_partitions(source: &Arc<dyn ImageRead>) -> Result<Vec<PartitionLayout>> {
    if let Some(result) = discover_gpt(source)? {
        return Ok(result);
    }

    // FAT-family and NTFS superfloppies also end their boot sector in 55 AA.
    // Their executable boot code can contain arbitrary nonzero bytes in the
    // legacy MBR-entry region, so validate a complete filesystem at byte zero
    // before interpreting those bytes as a partition table. GPT remains first
    // because its checksummed metadata is an unambiguous disk-level signal.
    let raw = PartitionLayout {
        selector: "raw".to_owned(),
        scheme: "raw".to_owned(),
        partition: 0,
        offset: 0,
        length: source.len(),
        partition_type: "unpartitioned".to_owned(),
        name: String::new(),
    };
    if probe_volume(source, raw).filesystem.is_some() {
        return Ok(Vec::new());
    }
    discover_mbr(source)
}

fn discover_gpt(source: &Arc<dyn ImageRead>) -> Result<Option<Vec<PartitionLayout>>> {
    let mut saw_signature = false;
    let mut errors = Vec::new();
    for sector_size in [512_u64, 4096] {
        if source.len() < sector_size.saturating_mul(2) {
            continue;
        }
        let last_lba = source.len() / sector_size - 1;
        for lba in [1_u64, last_lba] {
            let offset = lba.saturating_mul(sector_size);
            let mut signature = [0_u8; 8];
            if source.read_exact_at(offset, &mut signature).is_err() || &signature != b"EFI PART" {
                continue;
            }
            saw_signature = true;
            match parse_gpt(source, sector_size, lba) {
                Ok(partitions) => return Ok(Some(partitions)),
                Err(error) => errors.push(format!(
                    "{sector_size}-byte sector header at LBA {lba}: {error:#}"
                )),
            }
        }
    }
    if saw_signature {
        bail!(
            "GPT signatures were found but no header validated ({})",
            errors.join("; ")
        );
    }
    Ok(None)
}

fn parse_gpt(
    source: &Arc<dyn ImageRead>,
    sector_size: u64,
    header_lba: u64,
) -> Result<Vec<PartitionLayout>> {
    let mut sector = vec![0_u8; sector_size as usize];
    source.read_exact_at(header_lba * sector_size, &mut sector)?;
    if &sector[..8] != b"EFI PART" {
        bail!("missing GPT signature");
    }
    let header_size = u32::from_le_bytes(sector[12..16].try_into().unwrap());
    if !(92..=sector.len() as u32).contains(&header_size) {
        bail!("invalid GPT header size {header_size}");
    }
    let expected_header_crc = u32::from_le_bytes(sector[16..20].try_into().unwrap());
    let mut header = sector[..header_size as usize].to_vec();
    header[16..20].fill(0);
    let actual_header_crc = crc32fast::hash(&header);
    if actual_header_crc != expected_header_crc {
        bail!(
            "GPT header CRC mismatch (stored {expected_header_crc:#010x}, computed {actual_header_crc:#010x})"
        );
    }
    let current_lba = u64::from_le_bytes(sector[24..32].try_into().unwrap());
    if current_lba != header_lba {
        bail!("GPT current LBA is {current_lba}, expected {header_lba}");
    }
    let total_lbas = source.len() / sector_size;
    let alternate_lba = u64::from_le_bytes(sector[32..40].try_into().unwrap());
    if alternate_lba >= total_lbas || alternate_lba == current_lba {
        bail!("GPT alternate LBA {alternate_lba} is invalid");
    }
    let first_usable_lba = u64::from_le_bytes(sector[40..48].try_into().unwrap());
    let last_usable_lba = u64::from_le_bytes(sector[48..56].try_into().unwrap());
    if first_usable_lba == 0 || first_usable_lba > last_usable_lba || last_usable_lba >= total_lbas
    {
        bail!("GPT usable range {first_usable_lba}..={last_usable_lba} is outside the nested disk");
    }
    let entry_lba = u64::from_le_bytes(sector[72..80].try_into().unwrap());
    let entry_count = u32::from_le_bytes(sector[80..84].try_into().unwrap());
    let entry_size = u32::from_le_bytes(sector[84..88].try_into().unwrap());
    let expected_entries_crc = u32::from_le_bytes(sector[88..92].try_into().unwrap());
    if entry_count == 0 || entry_count > MAX_GPT_ENTRIES {
        bail!("GPT entry count {entry_count} is outside 1..={MAX_GPT_ENTRIES}");
    }
    if !(128..=4096).contains(&entry_size) || !entry_size.is_multiple_of(8) {
        bail!("GPT entry size {entry_size} is invalid");
    }
    let entries_len = u64::from(entry_count)
        .checked_mul(u64::from(entry_size))
        .context("GPT entry-array size overflows")?;
    if entries_len > MAX_GPT_ENTRY_BYTES {
        bail!("GPT entry array exceeds the 64 MiB safety limit");
    }
    let entries_offset = entry_lba
        .checked_mul(sector_size)
        .context("GPT entry-array offset overflows")?;
    let entries_end = entries_offset
        .checked_add(entries_len)
        .context("GPT entry-array end overflows")?;
    if entries_end > source.len() {
        bail!("GPT entry array is outside the nested image");
    }
    let mut entries = vec![0_u8; entries_len as usize];
    source.read_exact_at(entries_offset, &mut entries)?;
    let actual_entries_crc = crc32fast::hash(&entries);
    if actual_entries_crc != expected_entries_crc {
        bail!(
            "GPT entry-array CRC mismatch (stored {expected_entries_crc:#010x}, computed {actual_entries_crc:#010x})"
        );
    }

    let mut partitions = Vec::new();
    for (index, entry) in entries.chunks_exact(entry_size as usize).enumerate() {
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        if first_lba < first_usable_lba || last_lba < first_lba || last_lba > last_usable_lba {
            bail!(
                "GPT partition {} range {first_lba}..={last_lba} is outside the usable range",
                index + 1
            );
        }
        let offset = first_lba
            .checked_mul(sector_size)
            .context("GPT partition offset overflows")?;
        let length = (last_lba - first_lba + 1)
            .checked_mul(sector_size)
            .context("GPT partition length overflows")?;
        let name_units = entry[56..entry.len().min(128)]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|value| *value != 0)
            .collect::<Vec<_>>();
        partitions.push(PartitionLayout {
            selector: format!("gpt{}", index + 1),
            scheme: "gpt".to_owned(),
            partition: (index + 1) as u32,
            offset,
            length,
            partition_type: format_gpt_guid(&entry[..16]),
            name: String::from_utf16_lossy(&name_units),
        });
    }
    Ok(partitions)
}

fn format_gpt_guid(bytes: &[u8]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        u16::from_le_bytes(bytes[4..6].try_into().unwrap()),
        u16::from_le_bytes(bytes[6..8].try_into().unwrap()),
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn discover_mbr(source: &Arc<dyn ImageRead>) -> Result<Vec<PartitionLayout>> {
    if source.len() < 512 {
        return Ok(Vec::new());
    }
    let mut sector = [0_u8; 512];
    source.read_exact_at(0, &mut sector)?;
    if sector[510..512] != [0x55, 0xaa] {
        return Ok(Vec::new());
    }
    let mut partitions = Vec::new();
    let mut extended = Vec::new();
    for index in 0..4 {
        let entry = &sector[446 + index * 16..446 + (index + 1) * 16];
        let kind = entry[4];
        let start = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;
        let sectors = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as u64;
        if kind == 0 || sectors == 0 {
            continue;
        }
        if matches!(kind, 0x05 | 0x0f | 0x85) {
            extended.push((start, sectors));
            continue;
        }
        partitions.push(mbr_layout(
            source,
            (index + 1) as u32,
            kind,
            start,
            sectors,
        )?);
    }
    let mut logical_number = 5_u32;
    for (base, container_sectors) in extended {
        let container_end = base
            .checked_add(container_sectors)
            .context("MBR extended-partition range overflows")?;
        if base == 0 || container_end.saturating_mul(512) > source.len() {
            bail!("MBR extended-partition container lies outside the nested image");
        }
        let mut ebr_lba = base;
        let mut seen = BTreeSet::new();
        let mut terminated = false;
        for _ in 0..MAX_EBR_PARTITIONS {
            if !seen.insert(ebr_lba) {
                bail!("MBR extended-partition chain contains a loop at LBA {ebr_lba}");
            }
            let offset = ebr_lba.checked_mul(512).context("EBR offset overflows")?;
            let mut ebr = [0_u8; 512];
            source
                .read_exact_at(offset, &mut ebr)
                .with_context(|| format!("reading EBR at LBA {ebr_lba}"))?;
            if ebr[510..512] != [0x55, 0xaa] {
                bail!("extended partition has an invalid EBR signature at LBA {ebr_lba}");
            }
            let first = &ebr[446..462];
            let kind = first[4];
            let relative = u32::from_le_bytes(first[8..12].try_into().unwrap()) as u64;
            let sectors = u32::from_le_bytes(first[12..16].try_into().unwrap()) as u64;
            if kind != 0 && sectors != 0 {
                let start = ebr_lba
                    .checked_add(relative)
                    .context("logical MBR partition LBA overflows")?;
                let logical_end = start
                    .checked_add(sectors)
                    .context("logical MBR partition range overflows")?;
                if start < base || logical_end > container_end {
                    bail!("logical MBR partition lies outside its extended container");
                }
                partitions.push(mbr_layout(source, logical_number, kind, start, sectors)?);
                logical_number += 1;
            }
            let next = &ebr[462..478];
            let next_relative = u32::from_le_bytes(next[8..12].try_into().unwrap()) as u64;
            let next_sectors = u32::from_le_bytes(next[12..16].try_into().unwrap()) as u64;
            if next[4] == 0 || next_sectors == 0 {
                terminated = true;
                break;
            }
            if !matches!(next[4], 0x05 | 0x0f | 0x85) {
                bail!(
                    "EBR link entry has non-extended partition type 0x{:02x}",
                    next[4]
                );
            }
            if next_relative >= container_sectors {
                bail!("next EBR lies outside its extended-partition container");
            }
            ebr_lba = base
                .checked_add(next_relative)
                .context("next EBR LBA overflows")?;
        }
        if !terminated {
            bail!("MBR extended-partition chain exceeds {MAX_EBR_PARTITIONS} entries");
        }
    }
    Ok(partitions)
}

fn mbr_layout(
    source: &Arc<dyn ImageRead>,
    number: u32,
    kind: u8,
    start_lba: u64,
    sectors: u64,
) -> Result<PartitionLayout> {
    let offset = start_lba
        .checked_mul(512)
        .context("MBR partition offset overflows")?;
    let length = sectors
        .checked_mul(512)
        .context("MBR partition length overflows")?;
    let end = offset
        .checked_add(length)
        .context("MBR partition end overflows")?;
    if start_lba == 0 || end > source.len() {
        bail!("MBR partition {number} lies outside the nested image");
    }
    Ok(PartitionLayout {
        selector: format!("mbr{number}"),
        scheme: "mbr".to_owned(),
        partition: number,
        offset,
        length,
        partition_type: format!("0x{kind:02x}"),
        name: String::new(),
    })
}

fn probe_volume(source: &Arc<dyn ImageRead>, layout: PartitionLayout) -> VolumeInfo {
    let sliced = match SlicedImage::new(source.clone(), layout.offset, layout.length) {
        Ok(value) => Arc::new(value) as Arc<dyn ImageRead>,
        Err(error) => {
            return volume_info(layout, None, Some(format!("{error:#}")));
        }
    };
    let mut boot = [0_u8; 512];
    if let Err(error) = sliced.read_exact_at(0, &mut boot) {
        return volume_info(
            layout,
            None,
            Some(format!("reading boot sector: {error:#}")),
        );
    }

    if &boot[3..11] == b"NTFS    " {
        let mut reader = SourceCursor::new(sliced);
        return match Ntfs::new(&mut reader) {
            Ok(_) => volume_info(layout, Some(FilesystemKind::Ntfs), None),
            Err(error) => volume_info(layout, None, Some(format!("invalid NTFS: {error}"))),
        };
    }

    if &boot[3..11] == b"EXFAT   " || looks_like_fat(&boot) {
        return match FatFs::open(SourceCursor::new(sliced)) {
            Ok(fs) => volume_info(layout, Some(fat_kind(fs.variant())), None),
            Err(error) => volume_info(
                layout,
                None,
                Some(format!("invalid FAT-family filesystem: {error}")),
            ),
        };
    }

    if layout.length >= 1082 {
        let mut magic = [0_u8; 2];
        if sliced.read_exact_at(1024 + 56, &mut magic).is_ok() && magic == [0x53, 0xef] {
            return match open_ext4(sliced) {
                Ok(_) => volume_info(layout, Some(FilesystemKind::Ext4), None),
                Err(error) => volume_info(
                    layout,
                    None,
                    Some(format!("invalid ext filesystem: {error:#}")),
                ),
            };
        }
    }
    volume_info(
        layout,
        None,
        Some("no supported filesystem signature was found".to_owned()),
    )
}

fn volume_info(
    layout: PartitionLayout,
    filesystem: Option<FilesystemKind>,
    diagnostic: Option<String>,
) -> VolumeInfo {
    VolumeInfo {
        selector: layout.selector,
        scheme: layout.scheme,
        partition: layout.partition,
        offset: layout.offset,
        length: layout.length,
        partition_type: layout.partition_type,
        name: layout.name,
        filesystem,
        diagnostic,
    }
}

fn looks_like_fat(boot: &[u8; 512]) -> bool {
    let bytes_per_sector = u16::from_le_bytes(boot[11..13].try_into().unwrap());
    let sectors_per_cluster = boot[13];
    let reserved_sectors = u16::from_le_bytes(boot[14..16].try_into().unwrap());
    let fat_count = boot[16];
    let total_sectors_16 = u16::from_le_bytes(boot[19..21].try_into().unwrap());
    let total_sectors_32 = u32::from_le_bytes(boot[32..36].try_into().unwrap());

    boot[510..512] == [0x55, 0xaa]
        && matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        && sectors_per_cluster.is_power_of_two()
        && reserved_sectors != 0
        && matches!(fat_count, 1 | 2)
        && (total_sectors_16 != 0 || total_sectors_32 != 0)
}

fn fat_kind(variant: FatVariant) -> FilesystemKind {
    match variant {
        FatVariant::Fat12 => FilesystemKind::Fat12,
        FatVariant::Fat16 => FilesystemKind::Fat16,
        FatVariant::Fat32 => FilesystemKind::Fat32,
        FatVariant::ExFat => FilesystemKind::Exfat,
    }
}

#[derive(Clone)]
struct Segment {
    end: u64,
    block: Arc<BlockDescriptor>,
    block_offset: u64,
}

#[derive(Default)]
struct SegmentMap(BTreeMap<u64, Segment>);

impl SegmentMap {
    fn clear(&mut self) {
        self.0.clear();
    }

    fn remove(&mut self, start: u64, length: u64) -> Result<()> {
        let end = if length == u64::MAX {
            u64::MAX
        } else {
            start
                .checked_add(length)
                .context("nested FREE range overflows")?
        };
        if start == end {
            return Ok(());
        }
        let mut keys = self
            .0
            .range(start..end)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        if let Some((&key, segment)) = self.0.range(..=start).next_back()
            && segment.end > start
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        let mut preserved = Vec::new();
        for key in keys {
            let Some(segment) = self.0.remove(&key) else {
                continue;
            };
            if key < start {
                preserved.push((
                    key,
                    Segment {
                        end: start,
                        block: segment.block.clone(),
                        block_offset: segment.block_offset,
                    },
                ));
            }
            if segment.end > end {
                preserved.push((
                    end,
                    Segment {
                        end: segment.end,
                        block: segment.block,
                        block_offset: segment.block_offset + (end - key),
                    },
                ));
            }
        }
        self.0.extend(preserved);
        Ok(())
    }

    fn insert(&mut self, start: u64, length: u64, block: Arc<BlockDescriptor>) -> Result<()> {
        if length == 0 {
            return Ok(());
        }
        self.remove(start, length)?;
        let end = start
            .checked_add(length)
            .context("nested WRITE range overflows")?;
        self.0.insert(
            start,
            Segment {
                end,
                block,
                block_offset: 0,
            },
        );
        Ok(())
    }
}

#[derive(Clone)]
struct BlockDescriptor {
    id: u64,
    payload_offset: u64,
    payload_len: usize,
    payload_crc32: u32,
    logical_len: u64,
    encoding: BlockEncoding,
}

#[derive(Clone)]
enum BlockEncoding {
    Replay {
        compression: u8,
    },
    Embedded {
        compression: u8,
        embedded_type: u8,
        physical_size: u32,
    },
    Raw {
        key_index: usize,
        encrypted: bool,
        compression: u8,
        salt: [u8; 8],
        iv: [u8; 12],
        mac: [u8; 16],
    },
}

struct StreamImage {
    stream: PathBuf,
    file: File,
    len: u64,
    segments: SegmentMap,
    keys: Vec<DatasetKey>,
    cache: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}

impl StreamImage {
    fn build(
        stream: &Path,
        plan: &SnapshotPlan,
        resolved: &ResolvedPath,
        key_material: Option<&[u8]>,
    ) -> Result<Self> {
        let scanner = File::open(stream)
            .with_context(|| format!("opening ZFS send stream {}", stream.display()))?;
        let mut reader = StreamReader::new(scanner);
        let selected = plan.chain.iter().copied().collect::<BTreeSet<_>>();
        let mut active = false;
        let mut current_key = None;
        let mut seen = BTreeSet::new();
        let mut object_type = None;
        let mut object_exists = false;
        let mut segments = SegmentMap::default();
        let mut keys = Vec::new();
        let mut next_block_id = 1_u64;

        while let Some(record) = reader.next_record()? {
            match &record.kind {
                RecordKind::Begin(header) => {
                    active =
                        header.header_type == DMU_SUBSTREAM && selected.contains(&header.to_guid);
                    current_key = None;
                    if active {
                        seen.insert(header.to_guid);
                        if header.features & FEATURE_RAW != 0 {
                            let material = key_material
                                .ok_or_else(|| anyhow!("encrypted raw send requires a key"))?;
                            keys.push(
                                EncryptionParams::from_begin_payload(&record.payload)?
                                    .unlock(material)?,
                            );
                            current_key = Some(keys.len() - 1);
                        }
                    }
                    continue;
                }
                RecordKind::End => {
                    active = false;
                    current_key = None;
                    continue;
                }
                _ if !active => continue,
                _ => {}
            }

            match record.kind {
                RecordKind::Object(object) if object.object == resolved.object_id => {
                    if object_type != Some(object.object_type) {
                        segments.clear();
                    }
                    object_type = Some(object.object_type);
                    object_exists = true;
                }
                RecordKind::Write(write) if write.object == resolved.object_id => {
                    if !object_exists {
                        bail!("nested image WRITE appears before its OBJECT record");
                    }
                    let encoding = if let Some(key_index) = current_key {
                        BlockEncoding::Raw {
                            key_index,
                            encrypted: is_encrypted_object_type(write.object_type),
                            compression: write.compression_type,
                            salt: write.salt,
                            iv: write.iv,
                            mac: write.mac,
                        }
                    } else {
                        BlockEncoding::Replay {
                            compression: write.compression_type,
                        }
                    };
                    let descriptor = Arc::new(BlockDescriptor {
                        id: next_block_id,
                        payload_offset: record.stream_offset + RECORD_SIZE as u64,
                        payload_len: record.payload.len(),
                        payload_crc32: crc32fast::hash(&record.payload),
                        logical_len: write.logical_size,
                        encoding,
                    });
                    next_block_id += 1;
                    segments.insert(write.offset, write.logical_size, descriptor)?;
                }
                RecordKind::WriteEmbedded(write) if write.object == resolved.object_id => {
                    if current_key.is_some() {
                        bail!("raw encrypted embedded WRITE records are unsupported");
                    }
                    let descriptor = Arc::new(BlockDescriptor {
                        id: next_block_id,
                        payload_offset: record.stream_offset + RECORD_SIZE as u64,
                        payload_len: record.payload.len(),
                        payload_crc32: crc32fast::hash(&record.payload),
                        logical_len: u64::from(write.logical_size),
                        encoding: BlockEncoding::Embedded {
                            compression: write.compression_type,
                            embedded_type: write.embedded_type,
                            physical_size: write.physical_size,
                        },
                    });
                    next_block_id += 1;
                    segments.remove(write.offset, write.length)?;
                    segments.insert(
                        write.offset,
                        u64::from(write.logical_size).min(write.length),
                        descriptor,
                    )?;
                }
                RecordKind::Free(free) if free.object == resolved.object_id => {
                    segments.remove(free.offset, free.length)?;
                }
                RecordKind::FreeObjects(range) => {
                    let end = range.first_object.saturating_add(range.object_count);
                    if resolved.object_id >= range.first_object && resolved.object_id < end {
                        segments.clear();
                        object_type = None;
                        object_exists = false;
                    }
                }
                RecordKind::WriteByRef => bail!("deduplicated WRITE_BYREF streams are unsupported"),
                RecordKind::Redact => bail!("redacted streams are unsupported"),
                _ => {}
            }
        }
        if !reader.saw_end() {
            bail!("ZFS send stream has no END record");
        }
        if seen.len() != plan.chain.len() {
            bail!("stream changed while indexing the nested image");
        }
        if !object_exists {
            bail!(
                "nested image object {} is absent from the selected snapshot",
                resolved.object_id
            );
        }
        Ok(Self {
            stream: stream.to_owned(),
            file: File::open(stream)?,
            len: resolved.logical_size,
            segments,
            keys,
            cache: Mutex::new(None),
        })
    }

    fn decode(&self, descriptor: &BlockDescriptor) -> Result<Arc<Vec<u8>>> {
        {
            let cache = self
                .cache
                .lock()
                .map_err(|_| anyhow!("nested replay cache lock was poisoned"))?;
            if let Some((id, bytes)) = cache.as_ref()
                && *id == descriptor.id
            {
                return Ok(bytes.clone());
            }
        }
        let mut payload = vec![0_u8; descriptor.payload_len];
        read_file_exact_at(&self.file, descriptor.payload_offset, &mut payload).with_context(
            || {
                format!(
                    "reading replay payload at offset {} from {}",
                    descriptor.payload_offset,
                    self.stream.display()
                )
            },
        )?;
        if crc32fast::hash(&payload) != descriptor.payload_crc32 {
            bail!("ZFS send payload changed after inception mode indexed it");
        }
        let decoded = match &descriptor.encoding {
            BlockEncoding::Replay { compression } => {
                decode_replay_write(*compression, &payload, descriptor.logical_len)?
            }
            BlockEncoding::Embedded {
                compression,
                embedded_type,
                physical_size,
            } => decode_embedded_write(
                *compression,
                *embedded_type,
                &payload,
                *physical_size,
                u32::try_from(descriptor.logical_len)
                    .context("embedded logical size exceeds u32")?,
            )?,
            BlockEncoding::Raw {
                key_index,
                encrypted,
                compression,
                salt,
                iv,
                mac,
            } => {
                let key = self
                    .keys
                    .get(*key_index)
                    .ok_or_else(|| anyhow!("raw replay key is missing"))?;
                let protected = if *encrypted {
                    key.decrypt_block(salt, iv, mac, &[], &payload)?
                } else {
                    key.authenticate_block(&payload, mac)?;
                    payload
                };
                decompress_block(*compression, &protected, descriptor.logical_len)?
            }
        };
        if decoded.len() as u64 != descriptor.logical_len {
            bail!(
                "decoded replay block is {} bytes, expected {}",
                decoded.len(),
                descriptor.logical_len
            );
        }
        let decoded = Arc::new(decoded);
        *self
            .cache
            .lock()
            .map_err(|_| anyhow!("nested replay cache lock was poisoned"))? =
            Some((descriptor.id, decoded.clone()));
        Ok(decoded)
    }
}

impl ImageRead for StreamImage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| anyhow!("nested send-file read offset overflows"))?;
        if end > self.len {
            bail!(
                "nested read [{offset}, {end}) exceeds the {}-byte ZFS file",
                self.len
            );
        }
        buffer.fill(0);
        if buffer.is_empty() {
            return Ok(());
        }
        for (&segment_start, segment) in self.segments.0.range(..end) {
            if segment.end <= offset {
                continue;
            }
            let copy_start = segment_start.max(offset);
            let copy_end = segment.end.min(end);
            let destination_start = usize::try_from(copy_start - offset)?;
            let destination_end = usize::try_from(copy_end - offset)?;
            let source_start = usize::try_from(segment.block_offset + copy_start - segment_start)?;
            let source_end = source_start + (destination_end - destination_start);
            let decoded = self.decode(&segment.block)?;
            let bytes = decoded
                .get(source_start..source_end)
                .ok_or_else(|| anyhow!("nested extent points outside its decoded replay block"))?;
            buffer[destination_start..destination_end].copy_from_slice(bytes);
        }
        Ok(())
    }
}

fn read_file_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        #[cfg(unix)]
        let count = {
            use std::os::unix::fs::FileExt;
            file.read_at(&mut buffer[filled..], offset + filled as u64)?
        };
        #[cfg(windows)]
        let count = {
            use std::os::windows::fs::FileExt;
            file.seek_read(&mut buffer[filled..], offset + filled as u64)?
        };
        #[cfg(not(any(unix, windows)))]
        compile_error!("inception-mode positioned reads require Unix or Windows");
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short positioned read",
            ));
        }
        filled += count;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BlockDescriptor, BlockEncoding, DiskContainerKind, FilesystemKind, ImageRead,
        InceptionSession, SegmentMap, StreamImage,
    };
    use crate::filesystem::{ObjectIndex, plan_snapshot};
    use anyhow::{Result, bail};
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::sync::Arc;

    struct Bytes(Vec<u8>);

    impl ImageRead for Bytes {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }

        fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
            let start = offset as usize;
            let Some(bytes) = self.0.get(start..start + buffer.len()) else {
                bail!("outside test source");
            };
            buffer.copy_from_slice(bytes);
            Ok(())
        }
    }

    fn inception_error(result: Result<InceptionSession>) -> anyhow::Error {
        result
            .err()
            .expect("inception inspection unexpectedly succeeded")
    }

    #[test]
    fn finite_test_source_rejects_out_of_bounds_reads() {
        let source = Bytes(vec![1, 2, 3]);
        assert!(source.read_exact_at(2, &mut [0; 2]).is_err());
    }

    #[test]
    fn empty_segment_map_removes_ranges_without_panicking() {
        let mut map = SegmentMap::default();
        map.remove(10, 20).unwrap();
        assert!(map.0.is_empty());
    }

    #[test]
    fn replay_segments_split_cleanly_across_overwrites_and_frees() {
        let mut map = SegmentMap::default();
        let first = test_descriptor(1, 100);
        let replacement = test_descriptor(2, 20);
        map.insert(0, 100, first).unwrap();
        map.insert(40, 20, replacement).unwrap();
        assert_eq!(map.0.keys().copied().collect::<Vec<_>>(), [0, 40, 60]);
        assert_eq!(map.0[&0].end, 40);
        assert_eq!(map.0[&40].end, 60);
        assert_eq!(map.0[&60].block_offset, 60);

        map.remove(10, 70).unwrap();
        assert_eq!(map.0.keys().copied().collect::<Vec<_>>(), [0, 80]);
        assert_eq!(map.0[&0].end, 10);
        assert_eq!(map.0[&80].end, 100);
        assert_eq!(map.0[&80].block_offset, 80);
    }

    #[test]
    fn raw_fat_image_is_listed_and_extracted_without_materializing_a_disk() {
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(fat12_image())),
            "/vm/disk.raw".to_owned(),
        )
        .unwrap();
        assert_eq!(session.container(), DiskContainerKind::Raw);
        assert_eq!(session.volumes().len(), 1);
        assert_eq!(session.volumes()[0].filesystem, Some(FilesystemKind::Fat12));
        let entries = session.list_directory(None, "/").unwrap();
        assert!(entries.iter().any(|entry| entry.name == "HELLO.TXT"));

        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("hello.txt");
        let extraction = session.extract(None, "/HELLO.TXT", &output, false).unwrap();
        assert_eq!(extraction.logical_size, 5);
        assert_eq!(std::fs::read(output).unwrap(), b"hello");
    }

    #[test]
    fn explicit_offset_finds_a_raw_filesystem_after_leading_bytes() {
        let mut bytes = vec![0xa5; 4096];
        bytes.extend_from_slice(&fat12_image());
        let session = InceptionSession::inspect_source_at(
            Arc::new(Bytes(bytes)),
            "/vm/offset.raw".to_owned(),
            4096,
            None,
        )
        .unwrap();
        assert_eq!(session.image_offset(), 4096);
        assert_eq!(session.volumes()[0].filesystem, Some(FilesystemKind::Fat12));
    }

    #[test]
    fn qcow2_container_exposes_its_virtual_fat_disk() {
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(qcow2_with_first_cluster(&fat12_image()))),
            "/vm/disk.qcow2".to_owned(),
        )
        .unwrap();
        assert_eq!(session.container(), DiskContainerKind::Qcow2);
        assert_eq!(session.image_size(), fat12_image().len() as u64);
        assert_eq!(session.volumes()[0].filesystem, Some(FilesystemKind::Fat12));
        assert!(
            session
                .list_directory(None, "/")
                .unwrap()
                .iter()
                .any(|entry| entry.name == "HELLO.TXT")
        );
    }

    #[test]
    fn monolithic_sparse_vmdk_exposes_its_virtual_fat_disk() {
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(vmdk_with_first_grain(&fat12_image()))),
            "/vm/disk.vmdk".to_owned(),
        )
        .unwrap();
        assert_eq!(session.container(), DiskContainerKind::Vmdk);
        assert_eq!(session.volumes()[0].filesystem, Some(FilesystemKind::Fat12));
        assert!(
            session
                .list_directory(None, "/")
                .unwrap()
                .iter()
                .any(|entry| entry.name == "HELLO.TXT")
        );
    }

    #[test]
    fn mbr_partition_is_bounded_and_detected() {
        let fat = fat12_image();
        let mut disk = vec![0_u8; fat.len() + 512];
        disk[446 + 4] = 0x01;
        disk[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[446 + 12..446 + 16].copy_from_slice(&((fat.len() / 512) as u32).to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);
        disk[512..].copy_from_slice(&fat);

        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(disk)),
            "/vm/partitioned.raw".to_owned(),
        )
        .unwrap();
        let volume = &session.volumes()[0];
        assert_eq!(volume.selector, "mbr1");
        assert_eq!(volume.offset, 512);
        assert_eq!(volume.length, fat.len() as u64);
        assert_eq!(volume.filesystem, Some(FilesystemKind::Fat12));
    }

    #[test]
    fn mbr_extended_partition_chain_finds_a_logical_volume() {
        let fat = fat12_image();
        let fat_sectors = (fat.len() / 512) as u32;
        let mut disk = vec![0_u8; fat.len() + 1024];
        disk[446 + 4] = 0x0f;
        disk[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[446 + 12..446 + 16].copy_from_slice(&(fat_sectors + 1).to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);

        let ebr = 512;
        disk[ebr + 446 + 4] = 0x01;
        disk[ebr + 446 + 8..ebr + 446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[ebr + 446 + 12..ebr + 446 + 16].copy_from_slice(&fat_sectors.to_le_bytes());
        disk[ebr + 510..ebr + 512].copy_from_slice(&[0x55, 0xaa]);
        disk[1024..].copy_from_slice(&fat);

        let session =
            InceptionSession::inspect_source(Arc::new(Bytes(disk)), "/vm/logical.raw".to_owned())
                .unwrap();
        let volume = &session.volumes()[0];
        assert_eq!(volume.selector, "mbr5");
        assert_eq!(volume.offset, 1024);
        assert_eq!(volume.filesystem, Some(FilesystemKind::Fat12));
    }

    #[test]
    fn backup_gpt_recovers_a_bad_primary_header() {
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(gpt_disk_with_bad_primary())),
            "/vm/gpt.raw".to_owned(),
        )
        .unwrap();
        let volume = &session.volumes()[0];
        assert_eq!(volume.selector, "gpt1");
        assert_eq!(volume.name, "nested");
        assert_eq!(volume.filesystem, Some(FilesystemKind::Fat12));
    }

    #[test]
    fn supported_filesystems_work_across_every_layout_and_container() {
        let temporary = tempfile::tempdir().unwrap();
        let mut cases = 0;

        for fixture in filesystem_fixtures() {
            let filesystem = fixture.load();
            for layout in MatrixLayout::ALL {
                let (disk, selector) = layout.wrap(&filesystem, fixture.mbr_type);
                for container in MatrixContainer::ALL {
                    let label = format!("{} / {layout:?} / {container:?}", fixture.name);
                    let nested = container.wrap(&disk);
                    let session = InceptionSession::inspect_source(
                        Arc::new(Bytes(nested)),
                        format!("/matrix/{}.img", fixture.name),
                    )
                    .unwrap_or_else(|error| panic!("{label}: {error:#}"));

                    assert_eq!(session.container(), container.kind(), "{label}");
                    assert_eq!(session.image_size(), disk.len() as u64, "{label}");
                    assert_eq!(session.volumes().len(), 1, "{label}");
                    let volume = &session.volumes()[0];
                    assert_eq!(volume.selector, selector, "{label}");
                    assert_eq!(volume.filesystem, Some(fixture.kind), "{label}");

                    let entries = session
                        .list_directory(None, "/")
                        .unwrap_or_else(|error| panic!("{label}: listing root: {error:#}"));
                    assert!(
                        entries.iter().any(|entry| entry.name == fixture.root_entry),
                        "{label}: root did not contain {:?}: {entries:?}",
                        fixture.root_entry
                    );

                    let output = temporary.path().join(format!("matrix-{cases}.bin"));
                    let extraction = session
                        .extract(Some(&selector), fixture.path, &output, false)
                        .unwrap_or_else(|error| {
                            panic!("{label}: extracting {:?}: {error:#}", fixture.path)
                        });
                    assert_eq!(extraction.filesystem, fixture.kind, "{label}");
                    assert_eq!(
                        extraction.logical_size,
                        fixture.contents.len() as u64,
                        "{label}"
                    );
                    assert_eq!(std::fs::read(&output).unwrap(), fixture.contents, "{label}");

                    if layout == MatrixLayout::Raw && container == MatrixContainer::Raw {
                        fixture.assert_extended_behaviors(&session, temporary.path());
                    }
                    cases += 1;
                }
            }
        }

        assert_eq!(cases, 7 * 3 * 3);
    }

    #[test]
    fn explicit_windows_work_for_raw_qcow2_and_vmdk_images() {
        let guest = fat12_image();
        for container in MatrixContainer::ALL {
            let nested = container.wrap(&guest);
            let mut enclosing_file = vec![0xa5; 4096];
            enclosing_file.extend_from_slice(&nested);
            enclosing_file.extend_from_slice(&[0x5a; 2048]);
            let session = InceptionSession::inspect_source_at(
                Arc::new(Bytes(enclosing_file)),
                format!("/vm/offset-{container:?}.bin"),
                4096,
                Some(nested.len() as u64),
            )
            .unwrap_or_else(|error| panic!("{container:?}: {error:#}"));
            assert_eq!(session.container(), container.kind());
            assert_eq!(session.image_offset(), 4096);
            assert_eq!(session.stored_size(), nested.len() as u64);
            assert_eq!(session.image_size(), guest.len() as u64);
            assert_eq!(session.volumes()[0].filesystem, Some(FilesystemKind::Fat12));
        }
    }

    #[test]
    fn multiple_supported_volumes_require_and_honor_a_selector() {
        let disk = two_partition_mbr(&fat12_image());
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(disk)),
            "/vm/two-volumes.raw".to_owned(),
        )
        .unwrap();
        assert_eq!(session.volumes().len(), 2);
        let error = session.list_directory(None, "/").unwrap_err();
        assert!(format!("{error:#}").contains("multiple supported volumes"));
        assert!(
            session
                .list_directory(Some("MBR2"), "/")
                .unwrap()
                .iter()
                .any(|entry| entry.name == "HELLO.TXT")
        );
        let missing = session.list_directory(Some("mbr9"), "/").unwrap_err();
        assert!(format!("{missing:#}").contains("available: mbr1, mbr2"));
    }

    #[test]
    fn malformed_containers_and_partition_tables_are_rejected() {
        let mut qcow1 = vec![0_u8; 512];
        qcow1[..4].copy_from_slice(&[b'Q', b'F', b'I', 0xfb]);
        qcow1[4..8].copy_from_slice(&1_u32.to_be_bytes());
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(qcow1)),
            "/vm/qcow1.img".to_owned(),
        ));
        assert!(format!("{error:#}").contains("QCOW version 1 is not supported"));

        let mut overlay = qcow2_sparse(&fat12_image());
        overlay[8..16].copy_from_slice(&104_u64.to_be_bytes());
        overlay[16..20].copy_from_slice(&12_u32.to_be_bytes());
        overlay[104..116].copy_from_slice(b"parent.qcow2");
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(overlay)),
            "/vm/overlay.qcow2".to_owned(),
        ));
        assert!(format!("{error:#}").contains("backing file"));

        let mut encrypted = qcow2_sparse(&fat12_image());
        encrypted[32..36].copy_from_slice(&1_u32.to_be_bytes());
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(encrypted)),
            "/vm/encrypted.qcow2".to_owned(),
        ));
        assert!(format!("{error:#}").contains("encrypted QCOW2"));

        let mut descriptor = vec![0_u8; 512];
        let text = b"# Disk DescriptorFile\nversion=1\ncreateType=\"twoGbMaxExtentSparse\"\n";
        descriptor[..text.len()].copy_from_slice(text);
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(descriptor)),
            "/vm/external.vmdk".to_owned(),
        ));
        assert!(format!("{error:#}").contains("external extent files"));

        let mut gpt = gpt_disk(&fat12_image());
        let last_sector = gpt.len() / 512 - 1;
        gpt[512 + 32] ^= 0x80;
        gpt[last_sector * 512 + 32] ^= 0x80;
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(gpt)),
            "/vm/bad-gpt.raw".to_owned(),
        ));
        assert!(format!("{error:#}").contains("no header validated"));

        let mut out_of_bounds = vec![0_u8; 1024];
        out_of_bounds[446 + 4] = 0x83;
        out_of_bounds[446 + 8..446 + 12].copy_from_slice(&10_u32.to_le_bytes());
        out_of_bounds[446 + 12..446 + 16].copy_from_slice(&1_u32.to_le_bytes());
        out_of_bounds[510..512].copy_from_slice(&[0x55, 0xaa]);
        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(out_of_bounds)),
            "/vm/bad-mbr.raw".to_owned(),
        ));
        assert!(format!("{error:#}").contains("lies outside the nested image"));

        let error = inception_error(InceptionSession::inspect_source(
            Arc::new(Bytes(looping_ebr())),
            "/vm/looping-ebr.raw".to_owned(),
        ));
        assert!(format!("{error:#}").contains("chain contains a loop"));
    }

    #[test]
    fn unsafe_paths_unknown_filesystems_and_invalid_windows_are_explained() {
        let session = InceptionSession::inspect_source(
            Arc::new(Bytes(fat12_image())),
            "/vm/safe.raw".to_owned(),
        )
        .unwrap();
        let traversal = session.list_directory(None, "/../secret").unwrap_err();
        assert!(format!("{traversal:#}").contains("cannot contain '..'"));
        let relative = session.list_directory(None, "relative").unwrap_err();
        assert!(format!("{relative:#}").contains("must be absolute"));

        let unknown = InceptionSession::inspect_source(
            Arc::new(Bytes(vec![0_u8; 4096])),
            "/vm/unknown.raw".to_owned(),
        )
        .unwrap();
        assert_eq!(unknown.volumes()[0].filesystem, None);
        assert!(
            unknown.volumes()[0]
                .diagnostic
                .as_deref()
                .unwrap()
                .contains("no supported filesystem signature")
        );
        let error = unknown.list_directory(None, "/").unwrap_err();
        assert!(format!("{error:#}").contains("contains no supported"));

        let source = Arc::new(Bytes(vec![0_u8; 4096]));
        assert!(
            inception_error(InceptionSession::inspect_source_at(
                source.clone(),
                "bad".to_owned(),
                4097,
                None,
            ))
            .to_string()
            .contains("exceeds ZFS file size")
        );
        assert!(
            inception_error(InceptionSession::inspect_source_at(
                source,
                "bad".to_owned(),
                2048,
                Some(4096),
            ))
            .to_string()
            .contains("outside")
        );
    }

    #[test]
    fn send_backing_reads_only_the_selected_plain_object() {
        let stream = std::path::Path::new("tests/fixtures/tiny-full.zfs");
        let plan = plan_snapshot(stream, None).unwrap();
        let index = ObjectIndex::build_plan_with_key(stream, &plan, None).unwrap();
        let resolved = index.resolve_path("/hello.txt").unwrap();
        let image = StreamImage::build(stream, &plan, &resolved, None).unwrap();
        let mut bytes = vec![0_u8; image.len() as usize];
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, b"hello from the base snapshot\n");
    }

    #[test]
    fn send_backing_replays_an_incremental_snapshot_chain() {
        let stream = std::path::Path::new("tests/fixtures/multi-snapshot.zfs");
        let plan = plan_snapshot(stream, Some("s2")).unwrap();
        let index = ObjectIndex::build_plan_with_key(stream, &plan, None).unwrap();
        let resolved = index.resolve_path("/version.txt").unwrap();
        let image = StreamImage::build(stream, &plan, &resolved, None).unwrap();
        let mut bytes = vec![0_u8; image.len() as usize];
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, b"snapshot two has a longer value\n");
    }

    #[test]
    fn send_backing_authenticates_raw_encrypted_blocks() {
        let stream = std::path::Path::new("tests/fixtures/encrypted-raw-s1.zfs");
        let key = b"zfs-send-fixture-passphrase";
        let plan = plan_snapshot(stream, None).unwrap();
        let index = ObjectIndex::build_plan_with_key(stream, &plan, Some(key)).unwrap();
        let resolved = index.resolve_path("/docs/hello.txt").unwrap();
        let image = StreamImage::build(stream, &plan, &resolved, Some(key)).unwrap();
        let mut bytes = vec![0_u8; image.len() as usize];
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, b"encrypted hello\n");
    }

    #[derive(Clone, Copy)]
    enum FixtureSource {
        InlineFat12,
        ZstdBase64 {
            encoded: &'static str,
            compressed_sha256: &'static str,
            raw_sha256: &'static str,
            raw_len: usize,
        },
    }

    #[derive(Clone, Copy)]
    struct FilesystemFixture {
        name: &'static str,
        source: FixtureSource,
        kind: FilesystemKind,
        mbr_type: u8,
        root_entry: &'static str,
        path: &'static str,
        contents: &'static [u8],
    }

    impl FilesystemFixture {
        fn load(self) -> Vec<u8> {
            match self.source {
                FixtureSource::InlineFat12 => fat12_image(),
                FixtureSource::ZstdBase64 {
                    encoded,
                    compressed_sha256,
                    raw_sha256,
                    raw_len,
                } => {
                    decode_zstd_fixture(self.name, encoded, compressed_sha256, raw_sha256, raw_len)
                }
            }
        }

        fn assert_extended_behaviors(
            self,
            session: &InceptionSession,
            output_dir: &std::path::Path,
        ) {
            let directory_target = output_dir.join(format!("{}-directory", self.name));
            let error = session
                .extract(None, "/", &directory_target, false)
                .unwrap_err();
            assert!(format!("{error:#}").contains("not a regular file"));

            match self.name {
                "ntfs" => {
                    let resident = extract_bytes(
                        session,
                        "/file-with-12345",
                        &output_dir.join("ntfs-resident"),
                    );
                    assert_eq!(resident, b"12345");

                    let non_resident = extract_bytes(
                        session,
                        "/1000-bytes-file",
                        &output_dir.join("ntfs-non-resident"),
                    );
                    assert_eq!(non_resident, b"12345".repeat(200));

                    let sparse =
                        extract_bytes(session, "/sparse-file", &output_dir.join("ntfs-sparse"));
                    assert_eq!(sparse.len(), 500_005);
                    assert_eq!(&sparse[..5], b"12345");
                    assert!(sparse[5..500_000].iter().all(|byte| *byte == 0));
                    assert_eq!(&sparse[500_000..], b"11111");

                    let many = session.list_directory(None, "/many_subdirs").unwrap();
                    assert_eq!(many.len(), 512);
                    assert!(many.iter().any(|entry| entry.name == "512"));
                }
                "fat16" => {
                    let long = extract_bytes(
                        session,
                        "/LongFileName_16.txt",
                        &output_dir.join("fat16-long"),
                    );
                    assert_eq!(long, b"this file has a long name for LFN reassembly test\n");
                    let nested = extract_bytes(
                        session,
                        "/SUBDIR/nested.txt",
                        &output_dir.join("fat16-case-folded"),
                    );
                    assert_eq!(nested, b"nested file content 16\n");
                }
                "fat32" => {
                    let long = extract_bytes(
                        session,
                        "/Long Matrix Filename FAT32.txt",
                        &output_dir.join("fat32-long"),
                    );
                    assert_eq!(long, b"long FAT32 filename content\n");

                    let sparse =
                        extract_bytes(session, "/SPARSE.BIN", &output_dir.join("fat32-sparse"));
                    assert_eq!(sparse.len(), 4 * 1024 * 1024 + 4);
                    assert_eq!(&sparse[..4], b"HEAD");
                    assert!(sparse[4..4 * 1024 * 1024].iter().all(|byte| *byte == 0));
                    assert_eq!(&sparse[4 * 1024 * 1024..], b"TAIL");
                }
                "exfat" => {
                    let long = extract_bytes(
                        session,
                        "/LongFileName_exfat.txt",
                        &output_dir.join("exfat-long"),
                    );
                    assert_eq!(
                        long,
                        b"this exFAT file has a long name stored in File Name entries\n"
                    );
                    let nested = extract_bytes(
                        session,
                        "/SUBDIR/nested.txt",
                        &output_dir.join("exfat-case-folded"),
                    );
                    assert_eq!(nested, b"nested exfat content\n");
                }
                "ext4" => {
                    assert_ext_holes(session, &output_dir.join("ext4-holes"));
                    let error = session
                        .extract(
                            None,
                            "/dir1/dir2/sym_abs",
                            &output_dir.join("ext4-symlink"),
                            false,
                        )
                        .unwrap_err();
                    assert!(format!("{error:#}").contains("symlinks are not followed"));
                }
                "ext2" => assert_ext_holes(session, &output_dir.join("ext2-holes")),
                "fat12" => {}
                other => panic!("unhandled filesystem fixture {other}"),
            }
        }
    }

    fn filesystem_fixtures() -> [FilesystemFixture; 7] {
        [
            FilesystemFixture {
                name: "fat12",
                source: FixtureSource::InlineFat12,
                kind: FilesystemKind::Fat12,
                mbr_type: 0x01,
                root_entry: "HELLO.TXT",
                path: "/HELLO.TXT",
                contents: b"hello",
            },
            FilesystemFixture {
                name: "fat16",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/fat16.img.zst.b64"
                    )),
                    compressed_sha256: "2f2ec24e7e62ae256eeb3197540e28822f130a4cf1130d812a2b6dba0062a8dd",
                    raw_sha256: "b8dee10dcb38b6e6dfefe9e5a551405bdb1e600fb81789e80420070abdc71f8b",
                    raw_len: 4_194_304,
                },
                kind: FilesystemKind::Fat16,
                mbr_type: 0x06,
                root_entry: "HELLO.TXT",
                path: "/subdir/NESTED.TXT",
                contents: b"nested file content 16\n",
            },
            FilesystemFixture {
                name: "fat32",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/fat32.img.zst.b64"
                    )),
                    compressed_sha256: "ed871400721a9fa11f0839bb2bb51d2bde132eec9e10f40875b414d02a9c3e3b",
                    raw_sha256: "c43eced7ec3fe9dd78a9ee402d5ecb691f58688b377e7445aa4e8e08236a0045",
                    raw_len: 67_108_864,
                },
                kind: FilesystemKind::Fat32,
                mbr_type: 0x0c,
                root_entry: "HELLO.TXT",
                path: "/subdir/NESTED.TXT",
                contents: b"nested FAT32 matrix content\n",
            },
            FilesystemFixture {
                name: "exfat",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/exfat.img.zst.b64"
                    )),
                    compressed_sha256: "cdd8cbc4944cf92fab78b2d325ce03444527713906e0fab7c66e3a16c9cbf136",
                    raw_sha256: "4e0ab00a9c753bc20f9ece484534bbcdab59c1688a1d30590426ce4b9dba0601",
                    raw_len: 2_097_152,
                },
                kind: FilesystemKind::Exfat,
                mbr_type: 0x07,
                root_entry: "HELLO.TXT",
                path: "/subdir/NESTED.TXT",
                contents: b"nested exfat content\n",
            },
            FilesystemFixture {
                name: "ntfs",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/ntfs.img.zst.b64"
                    )),
                    compressed_sha256: "d53e13e45543501d861898f70e70d26a731ad79d4245ac6039aab6235d555dad",
                    raw_sha256: "e3612c182b8010e3599b5eb93bff427c7d824e85bdc2ddbe46e378e3ba814eb9",
                    raw_len: 2_097_152,
                },
                kind: FilesystemKind::Ntfs,
                mbr_type: 0x07,
                root_entry: "file-with-12345",
                path: "/file-with-12345",
                contents: b"12345",
            },
            FilesystemFixture {
                name: "ext4",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/ext4.img.zst.b64"
                    )),
                    compressed_sha256: "17c740fa68d260e70a3e8e146c1537f8f5b2fee3eb3a65c6385cd493ecb5a09b",
                    raw_sha256: "58f4ec5f880a1934bc23a73bc0d07b50a987736d7a1a8fbbafdc99bd36dfbee3",
                    raw_len: 67_108_864,
                },
                kind: FilesystemKind::Ext4,
                mbr_type: 0x83,
                root_entry: "small_file",
                path: "/small_file",
                contents: b"hello, world!",
            },
            FilesystemFixture {
                name: "ext2",
                source: FixtureSource::ZstdBase64 {
                    encoded: include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/inception/ext2.img.zst.b64"
                    )),
                    compressed_sha256: "b7008bdc6a2d50fdcb0684e506632e39421e18db7dbdc4d56c12422656102511",
                    raw_sha256: "b277b932c8f001c302920cbe9f47245b82e3232a123c82e5a595c5fd09c8dc3f",
                    raw_len: 100_663_296,
                },
                kind: FilesystemKind::Ext4,
                mbr_type: 0x83,
                root_entry: "small_file",
                path: "/small_file",
                contents: b"hello, world!",
            },
        ]
    }

    fn decode_zstd_fixture(
        name: &str,
        encoded: &str,
        compressed_sha256: &str,
        raw_sha256: &str,
        raw_len: usize,
    ) -> Vec<u8> {
        let compressed = decode_base64(encoded);
        assert_eq!(
            format!("{:x}", Sha256::digest(&compressed)),
            compressed_sha256,
            "{name} compressed fixture hash"
        );
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(compressed.as_slice())
            .unwrap_or_else(|error| panic!("{name}: opening Zstandard fixture: {error}"));
        let mut raw = Vec::with_capacity(raw_len);
        decoder
            .read_to_end(&mut raw)
            .unwrap_or_else(|error| panic!("{name}: decompressing fixture: {error}"));
        assert_eq!(raw.len(), raw_len, "{name} raw fixture length");
        assert_eq!(
            format!("{:x}", Sha256::digest(&raw)),
            raw_sha256,
            "{name} raw fixture hash"
        );
        raw
    }

    fn decode_base64(encoded: &str) -> Vec<u8> {
        let mut decoded = Vec::with_capacity(encoded.len() * 3 / 4);
        let mut quartet = [0_u8; 4];
        let mut count = 0;
        for byte in encoded.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
            quartet[count] = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => 64,
                other => panic!("invalid base64 byte {other:#04x}"),
            };
            count += 1;
            if count == 4 {
                assert!(quartet[0] < 64 && quartet[1] < 64);
                decoded.push((quartet[0] << 2) | (quartet[1] >> 4));
                if quartet[2] < 64 {
                    decoded.push((quartet[1] << 4) | (quartet[2] >> 2));
                    if quartet[3] < 64 {
                        decoded.push((quartet[2] << 6) | quartet[3]);
                    }
                }
                count = 0;
            }
        }
        assert_eq!(count, 0, "base64 fixture ended inside a quartet");
        decoded
    }

    fn extract_bytes(session: &InceptionSession, path: &str, output: &std::path::Path) -> Vec<u8> {
        session.extract(None, path, output, false).unwrap();
        std::fs::read(output).unwrap()
    }

    fn assert_ext_holes(session: &InceptionSession, output: &std::path::Path) {
        let holes = extract_bytes(session, "/holes", output);
        let mut expected = Vec::with_capacity(10 * 1024);
        for value in [0, 0, 0xa1, 0xa2, 0, 0, 0xa3, 0xa4, 0, 0] {
            expected.extend(std::iter::repeat_n(value, 1024));
        }
        assert_eq!(holes, expected);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MatrixLayout {
        Raw,
        Mbr,
        Gpt,
    }

    impl MatrixLayout {
        const ALL: [Self; 3] = [Self::Raw, Self::Mbr, Self::Gpt];

        fn wrap(self, filesystem: &[u8], mbr_type: u8) -> (Vec<u8>, String) {
            match self {
                Self::Raw => (filesystem.to_vec(), "raw".to_owned()),
                Self::Mbr => (mbr_disk(filesystem, mbr_type), "mbr1".to_owned()),
                Self::Gpt => (gpt_disk(filesystem), "gpt1".to_owned()),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MatrixContainer {
        Raw,
        Qcow2,
        Vmdk,
    }

    impl MatrixContainer {
        const ALL: [Self; 3] = [Self::Raw, Self::Qcow2, Self::Vmdk];

        fn wrap(self, disk: &[u8]) -> Vec<u8> {
            match self {
                Self::Raw => disk.to_vec(),
                Self::Qcow2 => qcow2_sparse(disk),
                Self::Vmdk => vmdk_sparse(disk),
            }
        }

        fn kind(self) -> DiskContainerKind {
            match self {
                Self::Raw => DiskContainerKind::Raw,
                Self::Qcow2 => DiskContainerKind::Qcow2,
                Self::Vmdk => DiskContainerKind::Vmdk,
            }
        }
    }

    fn mbr_disk(filesystem: &[u8], partition_type: u8) -> Vec<u8> {
        const SECTOR: usize = 512;
        const FIRST_LBA: u32 = 2048;
        let sectors = u32::try_from(filesystem.len().div_ceil(SECTOR)).unwrap();
        let mut disk = vec![0_u8; (FIRST_LBA as usize + sectors as usize) * SECTOR];
        disk[446 + 4] = partition_type;
        disk[446 + 8..446 + 12].copy_from_slice(&FIRST_LBA.to_le_bytes());
        disk[446 + 12..446 + 16].copy_from_slice(&sectors.to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);
        let offset = FIRST_LBA as usize * SECTOR;
        disk[offset..offset + filesystem.len()].copy_from_slice(filesystem);
        disk
    }

    fn two_partition_mbr(filesystem: &[u8]) -> Vec<u8> {
        const SECTOR: usize = 512;
        let sectors = u32::try_from(filesystem.len().div_ceil(SECTOR)).unwrap();
        let starts = [1_u32, 1 + sectors];
        let mut disk = vec![0_u8; (starts[1] + sectors) as usize * SECTOR];
        for (index, start) in starts.into_iter().enumerate() {
            let entry = 446 + index * 16;
            disk[entry + 4] = 0x01;
            disk[entry + 8..entry + 12].copy_from_slice(&start.to_le_bytes());
            disk[entry + 12..entry + 16].copy_from_slice(&sectors.to_le_bytes());
            let offset = start as usize * SECTOR;
            disk[offset..offset + filesystem.len()].copy_from_slice(filesystem);
        }
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);
        disk
    }

    fn looping_ebr() -> Vec<u8> {
        let mut disk = vec![0_u8; 3 * 512];
        disk[446 + 4] = 0x0f;
        disk[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[446 + 12..446 + 16].copy_from_slice(&2_u32.to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);
        let ebr = 512;
        disk[ebr + 462 + 4] = 0x0f;
        disk[ebr + 462 + 8..ebr + 462 + 12].copy_from_slice(&0_u32.to_le_bytes());
        disk[ebr + 462 + 12..ebr + 462 + 16].copy_from_slice(&2_u32.to_le_bytes());
        disk[ebr + 510..ebr + 512].copy_from_slice(&[0x55, 0xaa]);
        disk
    }

    fn gpt_disk(filesystem: &[u8]) -> Vec<u8> {
        const SECTOR: usize = 512;
        const FIRST_PARTITION_LBA: u64 = 2048;
        let partition_sectors = filesystem.len().div_ceil(SECTOR) as u64;
        let total_lbas = FIRST_PARTITION_LBA + partition_sectors + 40;
        let last_lba = total_lbas - 1;
        let mut disk = vec![0_u8; total_lbas as usize * SECTOR];

        disk[446 + 4] = 0xee;
        disk[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[446 + 12..446 + 16]
            .copy_from_slice(&u32::try_from(total_lbas - 1).unwrap().to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);

        let mut entry = [0_u8; 128];
        entry[..16].copy_from_slice(&[
            0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26,
            0x99, 0xc7,
        ]);
        entry[16..32].fill(1);
        entry[32..40].copy_from_slice(&FIRST_PARTITION_LBA.to_le_bytes());
        entry[40..48].copy_from_slice(&(FIRST_PARTITION_LBA + partition_sectors - 1).to_le_bytes());
        for (index, unit) in "matrix".encode_utf16().enumerate() {
            entry[56 + index * 2..58 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let entries_crc = crc32fast::hash(&entry);
        disk[SECTOR * 2..SECTOR * 2 + entry.len()].copy_from_slice(&entry);
        let backup_entries_lba = last_lba - 1;
        let backup_entries_offset = backup_entries_lba as usize * SECTOR;
        disk[backup_entries_offset..backup_entries_offset + entry.len()].copy_from_slice(&entry);
        disk[SECTOR..SECTOR * 2].copy_from_slice(&gpt_header(
            1,
            last_lba,
            2,
            total_lbas,
            entries_crc,
        ));
        disk[last_lba as usize * SECTOR..(last_lba as usize + 1) * SECTOR].copy_from_slice(
            &gpt_header(last_lba, 1, backup_entries_lba, total_lbas, entries_crc),
        );

        let partition_offset = FIRST_PARTITION_LBA as usize * SECTOR;
        disk[partition_offset..partition_offset + filesystem.len()].copy_from_slice(filesystem);
        disk
    }

    fn qcow2_sparse(guest: &[u8]) -> Vec<u8> {
        const CLUSTER: usize = 64 * 1024;
        let guest_clusters = guest.len().div_ceil(CLUSTER);
        assert!(
            guest_clusters <= CLUSTER / 8,
            "test QCOW2 needs a second L2 table"
        );
        let allocated = guest
            .chunks(CLUSTER)
            .filter(|cluster| cluster.iter().any(|byte| *byte != 0))
            .count();
        let mut image = vec![0_u8; CLUSTER * (3 + allocated)];
        image[0..4].copy_from_slice(&[b'Q', b'F', b'I', 0xfb]);
        image[4..8].copy_from_slice(&3_u32.to_be_bytes());
        image[20..24].copy_from_slice(&16_u32.to_be_bytes());
        image[24..32].copy_from_slice(&(guest.len() as u64).to_be_bytes());
        image[36..40].copy_from_slice(&1_u32.to_be_bytes());
        image[40..48].copy_from_slice(&(CLUSTER as u64).to_be_bytes());
        image[96..100].copy_from_slice(&4_u32.to_be_bytes());
        image[100..104].copy_from_slice(&104_u32.to_be_bytes());
        image[CLUSTER..CLUSTER + 8].copy_from_slice(&((CLUSTER * 2) as u64).to_be_bytes());

        let mut physical_cluster = 3;
        for (guest_cluster, bytes) in guest.chunks(CLUSTER).enumerate() {
            if bytes.iter().all(|byte| *byte == 0) {
                continue;
            }
            let physical_offset = physical_cluster * CLUSTER;
            let l2_offset = CLUSTER * 2 + guest_cluster * 8;
            image[l2_offset..l2_offset + 8]
                .copy_from_slice(&(physical_offset as u64).to_be_bytes());
            image[physical_offset..physical_offset + bytes.len()].copy_from_slice(bytes);
            physical_cluster += 1;
        }
        image
    }

    fn vmdk_sparse(guest: &[u8]) -> Vec<u8> {
        const SECTOR: usize = 512;
        const GRAIN_SECTORS: usize = 128;
        const GRAIN_BYTES: usize = GRAIN_SECTORS * SECTOR;
        const ENTRIES_PER_TABLE: usize = 512;
        const TABLE_SECTORS: usize = ENTRIES_PER_TABLE * 4 / SECTOR;

        assert_eq!(guest.len() % SECTOR, 0);
        let capacity = guest.len() / SECTOR;
        let grains = capacity.div_ceil(GRAIN_SECTORS);
        let table_count = grains.div_ceil(ENTRIES_PER_TABLE);
        assert!(
            table_count <= SECTOR / 4,
            "test VMDK grain directory overflow"
        );
        let overhead = 3 + table_count * TABLE_SECTORS;
        let allocated = guest
            .chunks(GRAIN_BYTES)
            .filter(|grain| grain.iter().any(|byte| *byte != 0))
            .count();
        let mut image = vec![0_u8; (overhead + allocated * GRAIN_SECTORS) * SECTOR];
        image[0..4].copy_from_slice(b"KDMV");
        image[4..8].copy_from_slice(&1_u32.to_le_bytes());
        image[12..20].copy_from_slice(&(capacity as u64).to_le_bytes());
        image[20..28].copy_from_slice(&(GRAIN_SECTORS as u64).to_le_bytes());
        image[28..36].copy_from_slice(&1_u64.to_le_bytes());
        image[36..44].copy_from_slice(&1_u64.to_le_bytes());
        image[44..48].copy_from_slice(&(ENTRIES_PER_TABLE as u32).to_le_bytes());
        image[56..64].copy_from_slice(&2_u64.to_le_bytes());
        image[64..72].copy_from_slice(&(overhead as u64).to_le_bytes());
        image[73..77].copy_from_slice(b"\n \r\n");
        let descriptor = format!(
            "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\ncreateType=\"monolithicSparse\"\n\nRW {capacity} SPARSE \"matrix.vmdk\"\n"
        );
        image[SECTOR..SECTOR + descriptor.len()].copy_from_slice(descriptor.as_bytes());

        for table in 0..table_count {
            let table_sector = 3 + table * TABLE_SECTORS;
            let directory_entry = SECTOR * 2 + table * 4;
            image[directory_entry..directory_entry + 4]
                .copy_from_slice(&(table_sector as u32).to_le_bytes());
        }

        let mut physical_sector = overhead;
        for (grain_index, bytes) in guest.chunks(GRAIN_BYTES).enumerate() {
            if bytes.iter().all(|byte| *byte == 0) {
                continue;
            }
            let table = grain_index / ENTRIES_PER_TABLE;
            let slot = grain_index % ENTRIES_PER_TABLE;
            let table_sector = 3 + table * TABLE_SECTORS;
            let table_entry = table_sector * SECTOR + slot * 4;
            image[table_entry..table_entry + 4]
                .copy_from_slice(&(physical_sector as u32).to_le_bytes());
            let physical_offset = physical_sector * SECTOR;
            image[physical_offset..physical_offset + bytes.len()].copy_from_slice(bytes);
            physical_sector += GRAIN_SECTORS;
        }
        image
    }

    fn fat12_image() -> Vec<u8> {
        const SECTOR: usize = 512;
        const SECTORS: usize = 2880;
        let mut image = vec![0_u8; SECTOR * SECTORS];
        let boot = &mut image[..SECTOR];
        boot[0..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
        boot[3..11].copy_from_slice(b"ZFSETEST");
        boot[11..13].copy_from_slice(&512_u16.to_le_bytes());
        boot[13] = 1;
        boot[14..16].copy_from_slice(&1_u16.to_le_bytes());
        boot[16] = 2;
        boot[17..19].copy_from_slice(&224_u16.to_le_bytes());
        boot[19..21].copy_from_slice(&(SECTORS as u16).to_le_bytes());
        boot[21] = 0xf0;
        boot[22..24].copy_from_slice(&9_u16.to_le_bytes());
        boot[24..26].copy_from_slice(&18_u16.to_le_bytes());
        boot[26..28].copy_from_slice(&2_u16.to_le_bytes());
        boot[38] = 0x29;
        boot[39..43].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
        boot[43..54].copy_from_slice(b"ZFSE TEST  ");
        boot[54..62].copy_from_slice(b"FAT12   ");
        boot[510..512].copy_from_slice(&[0x55, 0xaa]);

        for fat_sector in [1_usize, 10] {
            let fat = &mut image[fat_sector * SECTOR..(fat_sector + 9) * SECTOR];
            fat[0..5].copy_from_slice(&[0xf0, 0xff, 0xff, 0xff, 0x0f]);
        }
        let root_offset = 19 * SECTOR;
        let entry = &mut image[root_offset..root_offset + 32];
        entry[0..11].copy_from_slice(b"HELLO   TXT");
        entry[11] = 0x20;
        entry[26..28].copy_from_slice(&2_u16.to_le_bytes());
        entry[28..32].copy_from_slice(&5_u32.to_le_bytes());
        let data_offset = 33 * SECTOR;
        image[data_offset..data_offset + 5].copy_from_slice(b"hello");
        image
    }

    fn test_descriptor(id: u64, logical_len: u64) -> Arc<BlockDescriptor> {
        Arc::new(BlockDescriptor {
            id,
            payload_offset: 0,
            payload_len: 0,
            payload_crc32: 0,
            logical_len,
            encoding: BlockEncoding::Replay { compression: 0 },
        })
    }

    fn qcow2_with_first_cluster(guest: &[u8]) -> Vec<u8> {
        const CLUSTER: usize = 64 * 1024;
        let mut image = vec![0_u8; CLUSTER * 4];
        image[0..4].copy_from_slice(&[b'Q', b'F', b'I', 0xfb]);
        image[4..8].copy_from_slice(&3_u32.to_be_bytes());
        image[20..24].copy_from_slice(&16_u32.to_be_bytes());
        image[24..32].copy_from_slice(&(guest.len() as u64).to_be_bytes());
        image[36..40].copy_from_slice(&1_u32.to_be_bytes());
        image[40..48].copy_from_slice(&(CLUSTER as u64).to_be_bytes());
        image[96..100].copy_from_slice(&4_u32.to_be_bytes());
        image[100..104].copy_from_slice(&104_u32.to_be_bytes());
        image[CLUSTER..CLUSTER + 8].copy_from_slice(&((CLUSTER * 2) as u64).to_be_bytes());
        image[CLUSTER * 2..CLUSTER * 2 + 8].copy_from_slice(&((CLUSTER * 3) as u64).to_be_bytes());
        image[CLUSTER * 3..CLUSTER * 4].copy_from_slice(&guest[..CLUSTER]);
        image
    }

    fn vmdk_with_first_grain(guest: &[u8]) -> Vec<u8> {
        const SECTOR: usize = 512;
        const GRAIN_SECTORS: u64 = 128;
        const GRAIN_OFFSET: u64 = 7;
        let capacity = guest.len() as u64 / SECTOR as u64;
        let mut image = vec![0_u8; (GRAIN_OFFSET + GRAIN_SECTORS) as usize * SECTOR];
        image[0..4].copy_from_slice(b"KDMV");
        image[4..8].copy_from_slice(&1_u32.to_le_bytes());
        image[12..20].copy_from_slice(&capacity.to_le_bytes());
        image[20..28].copy_from_slice(&GRAIN_SECTORS.to_le_bytes());
        image[28..36].copy_from_slice(&1_u64.to_le_bytes());
        image[36..44].copy_from_slice(&1_u64.to_le_bytes());
        image[44..48].copy_from_slice(&512_u32.to_le_bytes());
        image[56..64].copy_from_slice(&2_u64.to_le_bytes());
        image[64..72].copy_from_slice(&GRAIN_OFFSET.to_le_bytes());
        image[73..77].copy_from_slice(b"\n \r\n");
        let descriptor = format!(
            "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\ncreateType=\"monolithicSparse\"\n\nRW {capacity} SPARSE \"synthetic.vmdk\"\n"
        );
        image[SECTOR..SECTOR + descriptor.len()].copy_from_slice(descriptor.as_bytes());
        image[SECTOR * 2..SECTOR * 2 + 4].copy_from_slice(&3_u32.to_le_bytes());
        image[SECTOR * 3..SECTOR * 3 + 4].copy_from_slice(&(GRAIN_OFFSET as u32).to_le_bytes());
        let grain = &mut image
            [GRAIN_OFFSET as usize * SECTOR..(GRAIN_OFFSET + GRAIN_SECTORS) as usize * SECTOR];
        grain.copy_from_slice(&guest[..grain.len()]);
        image
    }

    fn gpt_disk_with_bad_primary() -> Vec<u8> {
        const SECTOR: usize = 512;
        const FIRST_PARTITION_LBA: u64 = 40;
        let fat = fat12_image();
        let partition_sectors = fat.len() as u64 / SECTOR as u64;
        let total_lbas = FIRST_PARTITION_LBA + partition_sectors + 40;
        let last_lba = total_lbas - 1;
        let mut disk = vec![0_u8; total_lbas as usize * SECTOR];

        disk[446 + 4] = 0xee;
        disk[446 + 8..446 + 12].copy_from_slice(&1_u32.to_le_bytes());
        disk[446 + 12..446 + 16]
            .copy_from_slice(&(u32::try_from(total_lbas - 1).unwrap()).to_le_bytes());
        disk[510..512].copy_from_slice(&[0x55, 0xaa]);

        let mut entry = [0_u8; 128];
        entry[..16].copy_from_slice(&[
            0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26,
            0x99, 0xc7,
        ]);
        entry[16..32].fill(1);
        entry[32..40].copy_from_slice(&FIRST_PARTITION_LBA.to_le_bytes());
        entry[40..48].copy_from_slice(&(FIRST_PARTITION_LBA + partition_sectors - 1).to_le_bytes());
        for (index, unit) in "nested".encode_utf16().enumerate() {
            entry[56 + index * 2..58 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let entries_crc = crc32fast::hash(&entry);
        disk[SECTOR * 2..SECTOR * 2 + entry.len()].copy_from_slice(&entry);
        let backup_entries_lba = last_lba - 1;
        let backup_entries_offset = backup_entries_lba as usize * SECTOR;
        disk[backup_entries_offset..backup_entries_offset + entry.len()].copy_from_slice(&entry);

        let primary = gpt_header(1, last_lba, 2, total_lbas, entries_crc);
        disk[SECTOR..SECTOR * 2].copy_from_slice(&primary);
        // Damage a CRC-covered byte while preserving the signature so discovery
        // must validate and recover through the backup header.
        disk[SECTOR + 32] ^= 0x80;
        let backup = gpt_header(last_lba, 1, backup_entries_lba, total_lbas, entries_crc);
        disk[last_lba as usize * SECTOR..(last_lba as usize + 1) * SECTOR].copy_from_slice(&backup);

        let partition_offset = FIRST_PARTITION_LBA as usize * SECTOR;
        disk[partition_offset..partition_offset + fat.len()].copy_from_slice(&fat);
        disk
    }

    fn gpt_header(
        current_lba: u64,
        alternate_lba: u64,
        entries_lba: u64,
        total_lbas: u64,
        entries_crc: u32,
    ) -> [u8; 512] {
        let mut header = [0_u8; 512];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        header[12..16].copy_from_slice(&92_u32.to_le_bytes());
        header[24..32].copy_from_slice(&current_lba.to_le_bytes());
        header[32..40].copy_from_slice(&alternate_lba.to_le_bytes());
        header[40..48].copy_from_slice(&34_u64.to_le_bytes());
        header[48..56].copy_from_slice(&(total_lbas - 34).to_le_bytes());
        header[56..72].fill(2);
        header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        header[80..84].copy_from_slice(&1_u32.to_le_bytes());
        header[84..88].copy_from_slice(&128_u32.to_le_bytes());
        header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32fast::hash(&header[..92]);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        header
    }
}
