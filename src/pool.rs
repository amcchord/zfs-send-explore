//! Read-only browsing and extraction from an offline ZFS pool member.
//!
//! This is deliberately a separate backend from send-stream replay. It reads
//! labels and block pointers directly from a vdev using positioned I/O, so even
//! a multi-terabyte member is never buffered in full.

use crate::compression::decompress_block;
use crate::filesystem::DirectoryEntry;
use crate::operations::{SIDECAR_VERSION, Sidecar, guid_string, save_sidecar, sidecar_path};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use zfs_core::{
    BLKPTR_SIZE, Blkptr, ChecksumType, DMU_OST_META, DMU_OST_ZFS, DNODE_SIZE, Dnode, Endian,
    LABEL_SIZE, MAX_BLOCK_SIZE, MAX_INDIRECT_LEVELS, ObjsetPhys, Reader, SaLayouts, SaRegistry,
    VdevLabel, ZPL_DIRENT_OBJ_MASK, decode_sa_bonus, decode_znode_phys, label_offsets,
    parse_sa_layouts, parse_sa_registry, zap_list, zap_lookup,
};

const MAX_DATASETS: usize = 16_384;
const MAX_SNAPSHOTS_PER_DATASET: usize = 65_536;
const MAX_ZAP_BYTES: usize = 64 * 1024 * 1024;
const DMU_OT_ZNODE: u8 = 17;
const DMU_OT_SA: u8 = 44;
const ZPL_MASTER_NODE_OBJ: u64 = 1;

/// Summary of the selected pool member and the state rooted at its active
/// uberblock.
#[derive(Debug, Clone, Serialize)]
pub struct PoolInspection {
    pub pool_name: String,
    pub pool_guid: String,
    pub vdev_guid: String,
    pub vdev_type: String,
    pub top_level_vdevs: u64,
    pub source_bytes: u64,
    pub txg: u64,
    pub endian: String,
    pub datasets: usize,
    pub snapshots: usize,
}

/// One filesystem dataset reachable through the pool's DSL directory tree.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DatasetInfo {
    pub name: String,
    pub dsl_dir_object: u64,
    pub head_dataset_object: u64,
    pub head_guid: String,
}

/// One named snapshot and the DSL dataset object that pins its objset.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PoolSnapshot {
    pub dataset: String,
    pub name: String,
    pub full_name: String,
    pub dataset_object: u64,
    pub guid: String,
    pub creation_txg: u64,
    pub creation_time: u64,
}

/// Result of an extraction from a pool member.
#[derive(Debug, Clone)]
pub struct PoolExtraction {
    pub path: String,
    pub object_id: u64,
    pub logical_size: u64,
    pub sha256: String,
    /// True when extraction came from a named snapshot and therefore emitted a
    /// send-compatible `.zfse.json` sidecar.
    pub sidecar_written: bool,
}

trait ReadAt {
    fn len(&self) -> u64;
    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()>;
}

#[derive(Debug)]
struct FileSource {
    path: PathBuf,
    file: File,
    len: u64,
}

impl FileSource {
    fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("opening pool member {} read-only", path.display()))?;
        let metadata_len = file
            .metadata()
            .with_context(|| format!("reading metadata for {}", path.display()))?
            .len();
        let len = if metadata_len != 0 {
            metadata_len
        } else {
            // Linux block devices commonly report st_size == 0 but support
            // lseek(SEEK_END), which yields the actual device size.
            let mut size_probe = file.try_clone()?;
            size_probe.seek(SeekFrom::End(0)).with_context(|| {
                format!(
                    "determining the size of block device {}; pass the ZFS vdev partition rather than a whole-disk container",
                    path.display()
                )
            })?
        };
        if len < (4 * LABEL_SIZE) as u64 {
            bail!(
                "pool member {} is only {len} bytes (too small to contain four ZFS labels)",
                path.display()
            );
        }
        Ok(Self {
            path: path.to_owned(),
            file,
            len,
        })
    }
}

impl ReadAt for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let requested = u64::try_from(buffer.len()).context("read size exceeds u64")?;
        let end = offset
            .checked_add(requested)
            .ok_or_else(|| anyhow!("pool-member read offset overflow"))?;
        if end > self.len {
            bail!(
                "read [{offset}, {end}) is outside {} ({} bytes)",
                self.path.display(),
                self.len
            );
        }

        let mut filled = 0usize;
        while filled < buffer.len() {
            let position = offset + filled as u64;
            #[cfg(unix)]
            let count = {
                use std::os::unix::fs::FileExt;
                self.file.read_at(&mut buffer[filled..], position)
            };
            #[cfg(windows)]
            let count = {
                use std::os::windows::fs::FileExt;
                self.file.seek_read(&mut buffer[filled..], position)
            };
            #[cfg(not(any(unix, windows)))]
            compile_error!("pool-member positioned reads currently require Unix or Windows");

            let count = count.with_context(|| {
                format!(
                    "reading {} bytes at offset {position} from {}",
                    buffer.len() - filled,
                    self.path.display()
                )
            })?;
            if count == 0 {
                bail!(
                    "short read at offset {position} from {}",
                    self.path.display()
                );
            }
            filled += count;
        }
        Ok(())
    }
}

/// An opened offline vdev member. All methods are read-only with respect to the
/// source; extraction writes only to the explicitly selected destination.
pub struct PoolMember {
    source: FileSource,
    pool_name: String,
    pool_guid: u64,
    vdev_guid: u64,
    vdev_type: String,
    top_level_vdevs: u64,
    txg: u64,
    endian: Endian,
    mos: ObjsetPhys,
}

impl PoolMember {
    /// Open an exact ZFS vdev partition, a file-backed vdev, or an image of one.
    /// Whole disks containing a partition table are not auto-sliced yet.
    pub fn open(path: &Path) -> Result<Self> {
        let source = FileSource::open(path)?;
        let (front, back) = label_offsets(source.len());
        let offsets = front.into_iter().chain(back.into_iter().flatten());
        let mut parsed = Vec::new();
        let mut failures = Vec::new();

        for offset in offsets {
            let mut bytes = vec![0u8; LABEL_SIZE];
            if let Err(error) = source.read_exact_at(offset, &mut bytes) {
                failures.push(format!("label at {offset}: {error}"));
                continue;
            }
            match VdevLabel::parse(&bytes) {
                Ok(label) => parsed.push((offset, label)),
                Err(error) => failures.push(format!("label at {offset}: {error}")),
            }
        }

        if parsed.is_empty() {
            bail!(
                "{} has no readable ZFS vdev label; pass the exact ZFS partition or vdev image, not a GPT whole-disk image ({})",
                path.display(),
                failures.join("; ")
            );
        }

        parsed.sort_by_key(|(_, label)| label.active_uberblock.txg);
        let (_, active) = parsed.last().expect("non-empty labels");
        let config = &active.config;
        let pool_name = config
            .get_str("name")
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("active vdev label has no pool name"))?
            .to_owned();
        let pool_guid = config
            .get_u64("pool_guid")
            .ok_or_else(|| anyhow!("active vdev label has no pool_guid"))?;
        let top_level_vdevs = config
            .get_u64("vdev_children")
            .ok_or_else(|| anyhow!("active vdev label has no vdev_children count"))?;
        if top_level_vdevs != 1 {
            bail!(
                "pool {pool_name} has {top_level_vdevs} top-level vdevs; this command needs one member from every top-level vdev and currently accepts only a one-vdev pool"
            );
        }
        let vdev = config
            .get_nvlist("vdev_tree")
            .ok_or_else(|| anyhow!("active vdev label has no vdev_tree"))?;
        let vdev_type = vdev
            .get_str("type")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("active vdev label has no vdev type"))?
            .to_owned();
        if !matches!(vdev_type.as_str(), "disk" | "file" | "mirror") {
            bail!(
                "top-level vdev type {vdev_type:?} is unsupported; the first pool-member profile accepts a single disk/file vdev or one member of one mirror"
            );
        }
        let vdev_id = vdev.get_u64("id").unwrap_or(0);
        if vdev_id != 0 {
            bail!("the only top-level vdev has id {vdev_id}, not the supported id 0");
        }
        let vdev_guid = vdev.get_u64("guid").unwrap_or(0);

        for (_, label) in &parsed {
            if let Some(other) = label.config.get_u64("pool_guid")
                && other != pool_guid
            {
                bail!(
                    "vdev labels disagree on pool GUID ({pool_guid:#018x} versus {other:#018x}); refusing an inconsistent member"
                );
            }
        }

        let uberblock = &active.active_uberblock;
        let rootbp = uberblock.rootbp_full();
        let mos_block = read_block_from(&source, 0, &rootbp).with_context(|| {
            format!(
                "reading MOS from active uberblock txg {} on {}",
                uberblock.txg,
                path.display()
            )
        })?;
        let mos =
            ObjsetPhys::parse(&mos_block, uberblock.endian).context("decoding the MOS objset")?;
        if mos.os_type != DMU_OST_META {
            bail!(
                "active uberblock resolved objset type {}, not the MOS type {DMU_OST_META}",
                mos.os_type
            );
        }

        Ok(Self {
            source,
            pool_name,
            pool_guid,
            vdev_guid,
            vdev_type,
            top_level_vdevs,
            txg: uberblock.txg,
            endian: uberblock.endian,
            mos,
        })
    }

    pub fn inspect(&self) -> Result<PoolInspection> {
        let datasets = self.datasets()?;
        let mut snapshots = 0usize;
        for dataset in &datasets {
            snapshots = snapshots.saturating_add(self.snapshots_for(dataset)?.len());
        }
        Ok(PoolInspection {
            pool_name: self.pool_name.clone(),
            pool_guid: guid_string(self.pool_guid),
            vdev_guid: guid_string(self.vdev_guid),
            vdev_type: self.vdev_type.clone(),
            top_level_vdevs: self.top_level_vdevs,
            source_bytes: self.source.len(),
            txg: self.txg,
            endian: match self.endian {
                Endian::Little => "little".to_owned(),
                Endian::Big => "big".to_owned(),
            },
            datasets: datasets.len(),
            snapshots,
        })
    }

    /// Enumerate every filesystem dataset reachable through `dd_child_dir_zapobj`.
    pub fn datasets(&self) -> Result<Vec<DatasetInfo>> {
        let object_directory = self
            .mos_dnode(&self.mos, 1)?
            .ok_or_else(|| anyhow!("MOS object directory (object 1) is absent"))?;
        let object_directory = self.read_zap_object(&object_directory)?;
        let root = zap_lookup(&object_directory, "root_dataset")
            .ok_or_else(|| anyhow!("MOS object directory has no root_dataset entry"))?;

        let mut queue = VecDeque::from([(root, self.pool_name.clone())]);
        let mut seen = BTreeSet::new();
        let mut datasets = Vec::new();
        while let Some((dsl_dir_object, name)) = queue.pop_front() {
            if !seen.insert(dsl_dir_object) {
                continue;
            }
            if seen.len() > MAX_DATASETS {
                bail!("dataset tree exceeds the {MAX_DATASETS}-entry safety limit");
            }
            let directory = self
                .mos_dnode(&self.mos, dsl_dir_object)?
                .ok_or_else(|| anyhow!("DSL directory object {dsl_dir_object} is absent"))?;
            let reader = Reader::new(directory.endian);
            let head_dataset_object = reader.u64(&directory.bonus, 8);
            let child_zap_object = reader.u64(&directory.bonus, 32);

            if head_dataset_object != 0 {
                let head = self
                    .mos_dnode(&self.mos, head_dataset_object)?
                    .ok_or_else(|| {
                        anyhow!("head dataset object {head_dataset_object} is absent")
                    })?;
                datasets.push(DatasetInfo {
                    name: name.clone(),
                    dsl_dir_object,
                    head_dataset_object,
                    head_guid: guid_string(dataset_guid(&head)),
                });
            }

            if child_zap_object != 0 {
                let child_zap = self
                    .mos_dnode(&self.mos, child_zap_object)?
                    .ok_or_else(|| {
                        anyhow!("child-directory ZAP object {child_zap_object} is absent")
                    })?;
                let mut children = zap_list(&self.read_zap_object(&child_zap)?);
                children.sort_by(|left, right| left.0.cmp(&right.0));
                for (child_name, child_object) in children {
                    if child_name.starts_with('$') || child_name.starts_with('%') {
                        continue;
                    }
                    queue.push_back((child_object, format!("{name}/{child_name}")));
                }
            }
        }
        datasets.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(datasets)
    }

    pub fn snapshots(&self, dataset: Option<&str>) -> Result<Vec<PoolSnapshot>> {
        let datasets = self.datasets()?;
        if let Some(name) = dataset
            && !datasets.iter().any(|candidate| candidate.name == name)
        {
            bail!("dataset {name:?} was not found in pool {}", self.pool_name);
        }
        let mut snapshots = Vec::new();
        for item in &datasets {
            if dataset.is_none_or(|name| item.name == name) {
                snapshots.extend(self.snapshots_for(item)?);
            }
        }
        snapshots.sort_by(|left, right| left.full_name.cmp(&right.full_name));
        Ok(snapshots)
    }

    pub fn list_directory(&self, selector: &str, path: &str) -> Result<Vec<DirectoryEntry>> {
        let view = self.dataset_view(selector)?;
        let resolved = self.resolve_path(&view, path)?;
        if resolved.dirent_type != 4 {
            bail!("{} is not a directory", resolved.normalized_path);
        }
        let directory = self.read_zap_object(&resolved.dnode)?;
        let mut entries = zap_list(&directory)
            .into_iter()
            .map(|(name, raw)| {
                let object_id = raw & ZPL_DIRENT_OBJ_MASK;
                let logical_size = self
                    .mos_dnode(&view.zpl, object_id)
                    .ok()
                    .flatten()
                    .and_then(|dnode| attrs(&dnode, &view.registry, &view.layouts))
                    .map(|value| value.size);
                DirectoryEntry {
                    name,
                    object_id,
                    dirent_type: (raw >> 60) as u8,
                    logical_size,
                }
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    /// Extract a regular file. Named-snapshot extraction writes a sidecar that
    /// is compatible with `apply`; current-head extraction intentionally does
    /// not, because a head dataset is not a valid incremental-send base.
    pub fn extract(
        &self,
        selector: &str,
        path: &str,
        output: &Path,
        force: bool,
    ) -> Result<PoolExtraction> {
        let view = self.dataset_view(selector)?;
        let resolved = self.resolve_path(&view, path)?;
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
        let mut digest = Sha256::new();
        self.write_file(&resolved, temporary.as_file_mut(), &mut digest)?;
        temporary.as_file_mut().set_len(resolved.logical_size)?;
        temporary.as_file_mut().sync_all()?;
        persist_replace(temporary, output, force)?;
        let sha256 = format!("{:x}", digest.finalize());

        let sidecar_written = if let Some(snapshot_guid) = view.snapshot_guid {
            let sidecar = Sidecar {
                format_version: SIDECAR_VERSION,
                path: resolved.normalized_path.clone(),
                object_id: resolved.object_id,
                object_type: u32::from(resolved.dnode.dn_type),
                bonus_type: u32::from(resolved.dnode.dn_bonustype),
                logical_size: resolved.logical_size,
                size_bonus_offset: size_bonus_offset(
                    &resolved.dnode,
                    &view.registry,
                    &view.layouts,
                ),
                snapshot_guid: guid_string(snapshot_guid),
                sha256: sha256.clone(),
            };
            save_sidecar(output, &sidecar)?;
            true
        } else {
            let stale = sidecar_path(output);
            if stale.exists() {
                fs::remove_file(&stale).with_context(|| {
                    format!(
                        "removing stale incremental-send sidecar {} after current-head extraction",
                        stale.display()
                    )
                })?;
            }
            false
        };

        Ok(PoolExtraction {
            path: resolved.normalized_path,
            object_id: resolved.object_id,
            logical_size: resolved.logical_size,
            sha256,
            sidecar_written,
        })
    }

    fn snapshots_for(&self, dataset: &DatasetInfo) -> Result<Vec<PoolSnapshot>> {
        let head = self
            .mos_dnode(&self.mos, dataset.head_dataset_object)?
            .ok_or_else(|| {
                anyhow!(
                    "head dataset object {} is absent",
                    dataset.head_dataset_object
                )
            })?;
        let snapnames_object = Reader::new(head.endian).u64(&head.bonus, 32);
        if snapnames_object == 0 {
            return Ok(Vec::new());
        }
        let snapnames = self
            .mos_dnode(&self.mos, snapnames_object)?
            .ok_or_else(|| anyhow!("snapshot-name ZAP object {snapnames_object} is absent"))?;
        let mut entries = zap_list(&self.read_zap_object(&snapnames)?);
        if entries.len() > MAX_SNAPSHOTS_PER_DATASET {
            bail!(
                "dataset {} exceeds the {MAX_SNAPSHOTS_PER_DATASET}-snapshot safety limit",
                dataset.name
            );
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut snapshots = Vec::with_capacity(entries.len());
        for (name, dataset_object) in entries {
            let dnode = self
                .mos_dnode(&self.mos, dataset_object)?
                .ok_or_else(|| anyhow!("snapshot dataset object {dataset_object} is absent"))?;
            let reader = Reader::new(dnode.endian);
            snapshots.push(PoolSnapshot {
                dataset: dataset.name.clone(),
                full_name: format!("{}@{name}", dataset.name),
                name,
                dataset_object,
                guid: guid_string(dataset_guid(&dnode)),
                creation_time: reader.u64(&dnode.bonus, 48),
                creation_txg: reader.u64(&dnode.bonus, 56),
            });
        }
        Ok(snapshots)
    }

    fn dataset_view(&self, selector: &str) -> Result<DatasetView> {
        let (dataset_name, snapshot_name) = selector
            .split_once('@')
            .map_or((selector, None), |(dataset, snapshot)| {
                (dataset, Some(snapshot))
            });
        if dataset_name.is_empty() {
            bail!("dataset selector cannot be empty");
        }
        let dataset = self
            .datasets()?
            .into_iter()
            .find(|candidate| candidate.name == dataset_name)
            .ok_or_else(|| {
                anyhow!(
                    "dataset {dataset_name:?} was not found in pool {}",
                    self.pool_name
                )
            })?;

        let (dataset_object, snapshot_guid) = if let Some(snapshot_name) = snapshot_name {
            if snapshot_name.is_empty() {
                bail!("snapshot name cannot be empty");
            }
            let snapshots = self.snapshots_for(&dataset)?;
            let selected = if let Some(hex) = snapshot_name.strip_prefix("0x") {
                let guid = u64::from_str_radix(hex, 16)
                    .with_context(|| format!("invalid snapshot GUID {snapshot_name:?}"))?;
                snapshots
                    .into_iter()
                    .find(|snapshot| parse_guid(&snapshot.guid) == Some(guid))
            } else {
                snapshots
                    .into_iter()
                    .find(|snapshot| snapshot.name == snapshot_name)
            }
            .ok_or_else(|| anyhow!("snapshot {selector:?} was not found"))?;
            let guid = parse_guid(&selected.guid)
                .ok_or_else(|| anyhow!("snapshot {} has an invalid GUID", selected.full_name))?;
            (selected.dataset_object, Some(guid))
        } else {
            (dataset.head_dataset_object, None)
        };

        let dataset_dnode = self
            .mos_dnode(&self.mos, dataset_object)?
            .ok_or_else(|| anyhow!("DSL dataset object {dataset_object} is absent"))?;
        let dataset_pointer_raw = dataset_dnode
            .bonus
            .get(128..256)
            .ok_or_else(|| anyhow!("DSL dataset object {dataset_object} has a truncated ds_bp"))?;
        if blkptr_uses_crypt(dataset_pointer_raw, dataset_dnode.endian) {
            bail!(
                "{selector} uses native ZFS dataset encryption; encrypted pool-member extraction is not supported yet (raw encrypted send extraction is supported)"
            );
        }
        let dataset_pointer = parse_blkptr(dataset_pointer_raw, dataset_dnode.endian);
        let objset_block = read_block_from(&self.source, 0, &dataset_pointer)
            .with_context(|| format!("reading objset for {selector}"))?;
        let zpl = ObjsetPhys::parse(&objset_block, dataset_dnode.endian)
            .with_context(|| format!("decoding ZPL objset for {selector}"))?;
        if zpl.os_type != DMU_OST_ZFS {
            bail!(
                "{selector} resolves objset type {}, not a ZFS filesystem ({DMU_OST_ZFS}); encrypted datasets and volumes are outside the first pool-member profile",
                zpl.os_type
            );
        }

        let master = self
            .mos_dnode(&zpl, ZPL_MASTER_NODE_OBJ)?
            .ok_or_else(|| anyhow!("ZPL master node is absent for {selector}"))?;
        let master_data = self.read_zap_object(&master)?;
        let root_id = zap_lookup(&master_data, "ROOT")
            .ok_or_else(|| anyhow!("ZPL master node for {selector} has no ROOT entry"))?;
        let (registry, layouts) = self.sa_context(&zpl, &master_data)?;
        Ok(DatasetView {
            zpl,
            registry,
            layouts,
            root_id,
            snapshot_guid,
        })
    }

    fn sa_context(&self, zpl: &ObjsetPhys, master_data: &[u8]) -> Result<(SaRegistry, SaLayouts)> {
        let Some(sa_master_object) = zap_lookup(master_data, "SA_ATTRS") else {
            return Ok((SaRegistry::default(), SaLayouts::default()));
        };
        let sa_master = self
            .mos_dnode(zpl, sa_master_object)?
            .ok_or_else(|| anyhow!("SA master object {sa_master_object} is absent"))?;
        let sa_master = self.read_zap_object(&sa_master)?;
        let registry_object = zap_lookup(&sa_master, "REGISTRY")
            .ok_or_else(|| anyhow!("SA master object has no REGISTRY entry"))?;
        let layouts_object = zap_lookup(&sa_master, "LAYOUTS")
            .ok_or_else(|| anyhow!("SA master object has no LAYOUTS entry"))?;
        let registry = self
            .mos_dnode(zpl, registry_object)?
            .ok_or_else(|| anyhow!("SA registry object {registry_object} is absent"))?;
        let layouts = self
            .mos_dnode(zpl, layouts_object)?
            .ok_or_else(|| anyhow!("SA layouts object {layouts_object} is absent"))?;
        Ok((
            parse_sa_registry(&self.read_zap_object(&registry)?),
            parse_sa_layouts(&self.read_zap_object(&layouts)?),
        ))
    }

    fn resolve_path(&self, view: &DatasetView, path: &str) -> Result<ResolvedPoolPath> {
        let normalized = normalize_path(path)?;
        let mut current = view.root_id;
        let mut dirent_type = 4;
        for component in normalized.split('/').filter(|part| !part.is_empty()) {
            if dirent_type != 4 {
                bail!("path component {component:?} follows a non-directory object");
            }
            let directory = self
                .mos_dnode(&view.zpl, current)?
                .ok_or_else(|| anyhow!("directory object {current} is absent"))?;
            let directory = self.read_zap_object(&directory)?;
            let raw = zap_list(&directory)
                .into_iter()
                .find(|(name, _)| name == component)
                .map(|(_, value)| value)
                .ok_or_else(|| anyhow!("path {normalized:?} was not found"))?;
            current = raw & ZPL_DIRENT_OBJ_MASK;
            dirent_type = (raw >> 60) as u8;
        }
        let dnode = self
            .mos_dnode(&view.zpl, current)?
            .ok_or_else(|| anyhow!("object {current} for path {normalized:?} is absent"))?;
        let logical_size = attrs(&dnode, &view.registry, &view.layouts)
            .ok_or_else(|| anyhow!("could not decode ZPL metadata for object {current}"))?
            .size;
        Ok(ResolvedPoolPath {
            normalized_path: normalized,
            object_id: current,
            dirent_type,
            logical_size,
            dnode,
        })
    }

    fn write_file(
        &self,
        resolved: &ResolvedPoolPath,
        output: &mut File,
        digest: &mut Sha256,
    ) -> Result<()> {
        if resolved.logical_size == 0 {
            return Ok(());
        }
        let block_size = resolved.dnode.data_block_size();
        if block_size == 0 {
            bail!(
                "file object {} has a zero data block size",
                resolved.object_id
            );
        }
        let blocks = resolved.logical_size.div_ceil(block_size as u64);
        if blocks.saturating_sub(1) > resolved.dnode.dn_maxblkid {
            bail!(
                "file size {} needs {blocks} blocks but object {} ends at block {}",
                resolved.logical_size,
                resolved.object_id,
                resolved.dnode.dn_maxblkid
            );
        }
        let mut remaining = resolved.logical_size;
        for block_id in 0..blocks {
            let block = self
                .read_dnode_data(&resolved.dnode, block_id)
                .with_context(|| {
                    format!(
                        "reading file object {} block {block_id}",
                        resolved.object_id
                    )
                })?;
            let count = usize::try_from(remaining.min(block_size as u64))
                .context("file block length exceeds usize")?;
            let bytes = block.get(..count).ok_or_else(|| {
                anyhow!(
                    "file object {} block {block_id} decoded to {} bytes, expected at least {count}",
                    resolved.object_id,
                    block.len()
                )
            })?;
            output.write_all(bytes)?;
            digest.update(bytes);
            remaining -= count as u64;
        }
        Ok(())
    }

    fn mos_dnode(&self, objset: &ObjsetPhys, object_id: u64) -> Result<Option<Dnode>> {
        let meta = &objset.meta_dnode;
        let data_block_size = meta.data_block_size();
        if data_block_size < DNODE_SIZE {
            return Ok(None);
        }
        let per_block = (data_block_size / DNODE_SIZE) as u64;
        let block_id = object_id / per_block;
        if block_id > meta.dn_maxblkid {
            return Ok(None);
        }
        let within = usize::try_from(object_id % per_block)
            .unwrap_or(usize::MAX)
            .saturating_mul(DNODE_SIZE);
        let block = self.read_dnode_data(meta, block_id)?;
        let raw = block.get(within..).ok_or_else(|| {
            anyhow!("dnode object {object_id} starts outside its meta-dnode block")
        })?;
        let mut dnode = Dnode::parse(raw, objset.endian)
            .ok_or_else(|| anyhow!("dnode object {object_id} is truncated"))?;
        // zfs-forensic-core 0.1.1 skips the wrong reserved word when gathering
        // an embedded BP payload. Correct it while the original dnode bytes are
        // still available. This can be dropped when the dependency releases its
        // upstream fix.
        for (index, pointer) in dnode.blkptrs.iter_mut().enumerate() {
            let offset = 64 + index * BLKPTR_SIZE;
            if let Some(bytes) = raw.get(offset..offset + BLKPTR_SIZE) {
                correct_embedded_payload(pointer, bytes, objset.endian);
            }
        }
        Ok((dnode.dn_type != 0).then_some(dnode))
    }

    fn read_zap_object(&self, dnode: &Dnode) -> Result<Vec<u8>> {
        let block_size = dnode.data_block_size();
        if block_size == 0 {
            bail!("ZAP object has a zero block size");
        }
        let max_blocks = (MAX_ZAP_BYTES / block_size).max(1);
        let requested = usize::try_from(dnode.dn_maxblkid)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        if requested > max_blocks {
            bail!("ZAP object exceeds the {MAX_ZAP_BYTES}-byte safety limit");
        }
        let mut data = Vec::with_capacity(requested.saturating_mul(block_size));
        for block_id in 0..requested {
            data.extend_from_slice(&self.read_dnode_data(dnode, block_id as u64)?);
        }
        Ok(data)
    }

    fn read_dnode_data(&self, dnode: &Dnode, block_id: u64) -> Result<Vec<u8>> {
        if dnode.dn_nlevels == 0 || dnode.dn_nlevels > MAX_INDIRECT_LEVELS {
            bail!(
                "unsupported dnode indirection level {} (supported 1..={MAX_INDIRECT_LEVELS})",
                dnode.dn_nlevels
            );
        }
        if block_id > dnode.dn_maxblkid {
            bail!(
                "logical block {block_id} exceeds dnode maximum {}",
                dnode.dn_maxblkid
            );
        }
        let pointers_per_indirect = (dnode.indirect_block_size() / BLKPTR_SIZE).max(1);
        if !pointers_per_indirect.is_power_of_two() {
            bail!("dnode indirect block has a non-power-of-two pointer count");
        }
        let shift = pointers_per_indirect.trailing_zeros();
        let top_level = dnode.dn_nlevels - 1;
        let top_shift = u32::from(top_level) * shift;
        let top_index = usize::try_from(block_id >> top_shift).unwrap_or(usize::MAX);
        let mut pointer = *dnode.blkptr(top_index).ok_or_else(|| {
            anyhow!("logical block {block_id} selects absent top block pointer {top_index}")
        })?;
        let mut level = top_level;
        while level > 0 {
            let block = read_block_from(&self.source, 0, &pointer)?;
            level -= 1;
            let index_shift = u32::from(level) * shift;
            let child_index =
                usize::try_from((block_id >> index_shift) & ((pointers_per_indirect as u64) - 1))
                    .unwrap_or(usize::MAX);
            let offset = child_index.saturating_mul(BLKPTR_SIZE);
            let child = block
                .get(offset..offset.saturating_add(BLKPTR_SIZE))
                .ok_or_else(|| anyhow!("indirect child pointer {child_index} is truncated"))?;
            pointer = parse_blkptr(child, dnode.endian);
        }
        read_block_from(&self.source, 0, &pointer)
    }
}

fn parse_blkptr(raw: &[u8], endian: Endian) -> Blkptr {
    let mut pointer = Blkptr::parse(raw, endian);
    correct_embedded_payload(&mut pointer, raw, endian);
    pointer
}

fn blkptr_uses_crypt(raw: &[u8], endian: Endian) -> bool {
    Reader::new(endian).u64(raw, 48) & (1 << 61) != 0
}

fn correct_embedded_payload(pointer: &mut Blkptr, raw: &[u8], endian: Endian) {
    if !pointer.embedded {
        return;
    }
    // BPE payload words are every u64 except blk_prop (word 6) and logical
    // birth (word 10). OpenZFS defines the first payload byte as the low eight
    // bits of each decoded word, independent of the pool byte order.
    let reader = Reader::new(endian);
    let mut output = 0usize;
    for word in 0..16 {
        if matches!(word, 6 | 10) {
            continue;
        }
        let bytes = reader.u64(raw, word * 8).to_le_bytes();
        pointer.embedded_payload[output..output + 8].copy_from_slice(&bytes);
        output += 8;
    }
}

struct DatasetView {
    zpl: ObjsetPhys,
    registry: SaRegistry,
    layouts: SaLayouts,
    root_id: u64,
    snapshot_guid: Option<u64>,
}

struct ResolvedPoolPath {
    normalized_path: String,
    object_id: u64,
    dirent_type: u8,
    logical_size: u64,
    dnode: Dnode,
}

fn read_block_from(source: &dyn ReadAt, expected_vdev: u32, pointer: &Blkptr) -> Result<Vec<u8>> {
    let logical_size = pointer.lsize_bytes();
    if logical_size == 0 || logical_size > MAX_BLOCK_SIZE {
        bail!(
            "block logical size {logical_size} is outside the supported 1..={MAX_BLOCK_SIZE} range"
        );
    }
    if pointer.embedded {
        return decompress_block(
            pointer.compression,
            pointer.embedded_data(),
            logical_size as u64,
        );
    }
    if pointer.is_hole() {
        return Ok(vec![0u8; logical_size]);
    }

    let physical_size = pointer.psize_bytes();
    if physical_size == 0 || physical_size > MAX_BLOCK_SIZE {
        bail!("block physical size {physical_size} is outside the safety limit");
    }
    let checksum = ChecksumType::from_raw(pointer.checksum);
    if matches!(
        checksum,
        ChecksumType::Inherit
            | ChecksumType::On
            | ChecksumType::Other(_)
            | ChecksumType::Label
            | ChecksumType::GangHeader
            | ChecksumType::Zilog
    ) {
        bail!("unsupported on-disk checksum function {}", pointer.checksum);
    }

    let mut errors = Vec::new();
    for (copy, dva) in pointer.dvas.iter().enumerate() {
        if dva.is_empty() {
            continue;
        }
        if dva.vdev != expected_vdev {
            errors.push(format!(
                "DVA[{copy}] addresses unavailable top-level vdev {}",
                dva.vdev
            ));
            continue;
        }
        if dva.gang {
            errors.push(format!(
                "DVA[{copy}] is a gang block, which is not supported yet"
            ));
            continue;
        }
        let offset = dva.physical_byte_offset();
        let mut raw = vec![0u8; physical_size];
        if let Err(error) = source.read_exact_at(offset, &mut raw) {
            errors.push(format!("DVA[{copy}] at {offset}: {error}"));
            continue;
        }
        if matches!(
            zfs_core::checksum::verify(checksum, pointer.byteorder, &raw, pointer.checksum_words),
            Some(false)
        ) {
            errors.push(format!("DVA[{copy}] at {offset}: checksum mismatch"));
            continue;
        }
        match decompress_block(pointer.compression, &raw, logical_size as u64) {
            Ok(data) => return Ok(data),
            Err(error) => errors.push(format!("DVA[{copy}] at {offset}: {error}")),
        }
    }
    bail!("no readable copy of block pointer: {}", errors.join("; "))
}

fn attrs(dnode: &Dnode, registry: &SaRegistry, layouts: &SaLayouts) -> Option<zfs_core::ZplAttrs> {
    match dnode.dn_bonustype {
        DMU_OT_SA => decode_sa_bonus(&dnode.bonus, registry, layouts, dnode.endian),
        DMU_OT_ZNODE => decode_znode_phys(&dnode.bonus, dnode.endian),
        _ => None,
    }
}

fn dataset_guid(dnode: &Dnode) -> u64 {
    // dsl_dataset_phys_t: fsid_guid @ 104, permanent send GUID @ 112.
    Reader::new(dnode.endian).u64(&dnode.bonus, 112)
}

fn parse_guid(value: &str) -> Option<u64> {
    u64::from_str_radix(value.strip_prefix("0x")?, 16).ok()
}

fn size_bonus_offset(dnode: &Dnode, registry: &SaRegistry, layouts: &SaLayouts) -> Option<u64> {
    if dnode.dn_bonustype == DMU_OT_ZNODE {
        return (dnode.bonus.len() >= 88).then_some(80);
    }
    if dnode.dn_bonustype != DMU_OT_SA || dnode.bonus.len() < 8 {
        return None;
    }
    let size_id = registry.by_name("ZPL_SIZE")?.id;
    let info = Reader::new(dnode.endian).u16(&dnode.bonus, 4);
    let layout = u64::from(info & 0x03ff);
    let mut offset = usize::from((info >> 10) & 0x3f) << 3;
    for id in layouts.attr_ids(layout)? {
        if *id == size_id {
            return (offset + 8 <= dnode.bonus.len()).then_some(offset as u64);
        }
        let size = usize::from(registry.size_of(*id)?);
        if size == 0 {
            return None;
        }
        offset = offset.checked_add(size)?;
    }
    None
}

fn normalize_path(path: &str) -> Result<String> {
    let mut components = Vec::new();
    for component in path.split('/').filter(|part| !part.is_empty()) {
        if matches!(component, "." | "..") {
            bail!("path components '.' and '..' are not accepted");
        }
        components.push(component);
    }
    if components.is_empty() {
        Ok("/".to_owned())
    } else {
        Ok(format!("/{}", components.join("/")))
    }
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

#[cfg(test)]
mod tests {
    use super::{
        FileSource, ReadAt, blkptr_uses_crypt, normalize_path, parse_blkptr, read_block_from,
    };
    use std::fs;
    use zfs_core::{Blkptr, ChecksumType, CompressType, Endian};

    #[test]
    fn positioned_source_reads_without_moving_a_shared_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("member.img");
        let mut bytes = vec![0u8; 4 * zfs_core::LABEL_SIZE];
        bytes[17..21].copy_from_slice(b"test");
        fs::write(&path, bytes).unwrap();
        let source = FileSource::open(&path).unwrap();
        let mut first = [0u8; 4];
        let mut second = [0u8; 4];
        source.read_exact_at(17, &mut first).unwrap();
        source.read_exact_at(17, &mut second).unwrap();
        assert_eq!(&first, b"test");
        assert_eq!(first, second);
    }

    #[test]
    fn block_reader_enforces_fletcher_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("member.img");
        let physical = zfs_core::BOOT_SKEW as usize;
        let payload = vec![0x5au8; 512];
        let mut image = vec![0u8; physical + payload.len() + 4 * zfs_core::LABEL_SIZE];
        image[physical..physical + payload.len()].copy_from_slice(&payload);
        fs::write(&path, &image).unwrap();
        let source = FileSource::open(&path).unwrap();
        let mut pointer = Blkptr::default();
        pointer.dvas[0].asize_sectors = 1;
        pointer.compression = CompressType::Off.raw();
        pointer.checksum = ChecksumType::Fletcher4.raw();
        pointer.byteorder = Endian::Little;
        pointer.checksum_words = zfs_core::checksum::fletcher4(&payload, Endian::Little);
        assert_eq!(read_block_from(&source, 0, &pointer).unwrap(), payload);

        let mut sentinel = pointer;
        sentinel.checksum = ChecksumType::Inherit.raw();
        assert!(
            read_block_from(&source, 0, &sentinel)
                .unwrap_err()
                .to_string()
                .contains("unsupported on-disk checksum")
        );

        let mut corrupted = image;
        corrupted[physical] ^= 1;
        fs::write(&path, corrupted).unwrap();
        let source = FileSource::open(&path).unwrap();
        assert!(
            read_block_from(&source, 0, &pointer)
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
    }

    #[test]
    fn pool_paths_reject_parent_traversal() {
        assert_eq!(normalize_path("//dir/file").unwrap(), "/dir/file");
        assert!(normalize_path("/dir/../file").is_err());
    }

    #[test]
    fn embedded_payload_skips_prop_and_logical_birth_words() {
        for endian in [Endian::Little, Endian::Big] {
            let encode = |value: u64| match endian {
                Endian::Little => value.to_le_bytes(),
                Endian::Big => value.to_be_bytes(),
            };
            let mut raw = [0u8; 128];
            for word in 0..16u64 {
                raw[word as usize * 8..word as usize * 8 + 8]
                    .copy_from_slice(&encode(word * 0x0101_0101_0101_0101));
            }
            // Embedded, 112-byte physical/logical payload.
            let prop = 111u64 | (111u64 << 25) | (2u64 << 32) | (1u64 << 39);
            raw[48..56].copy_from_slice(&encode(prop));
            let pointer = parse_blkptr(&raw, endian);
            let expected_words = [0u64, 1, 2, 3, 4, 5, 7, 8, 9, 11, 12, 13, 14, 15];
            let mut expected = Vec::new();
            for word in expected_words {
                expected.extend_from_slice(&(word * 0x0101_0101_0101_0101).to_le_bytes());
            }
            assert_eq!(pointer.embedded_data(), expected);
        }
    }

    #[test]
    fn encrypted_block_pointer_flag_is_detected() {
        let mut raw = [0u8; 128];
        raw[48..56].copy_from_slice(&(1u64 << 61).to_le_bytes());
        assert!(blkptr_uses_crypt(&raw, Endian::Little));
        raw[48..56].fill(0);
        assert!(!blkptr_uses_crypt(&raw, Endian::Little));
    }
}
