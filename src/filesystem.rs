use crate::stream::{BeginRecord, RecordKind, StreamReader};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use zfs_core::{
    Endian, SaLayouts, SaRegistry, ZPL_DIRENT_OBJ_MASK, decode_sa_bonus, decode_znode_phys,
    parse_sa_layouts, parse_sa_registry, zap_list, zap_lookup,
};

const DMU_OT_ZNODE: u32 = 17;
const DMU_OT_DIRECTORY_CONTENTS: u32 = 20;
const DMU_OT_MASTER_NODE: u32 = 21;
const DMU_OT_SA: u32 = 44;
const DMU_OT_SA_MASTER_NODE: u32 = 45;
const DMU_OT_SA_ATTR_REGISTRATION: u32 = 46;
const DMU_OT_SA_ATTR_LAYOUTS: u32 = 47;
const MAX_METADATA_OBJECT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ObjectMeta {
    pub object_type: u32,
    pub bonus_type: u32,
    pub block_size: u32,
    pub max_block_id: u64,
    pub bonus: Vec<u8>,
}

#[derive(Debug)]
pub struct ObjectIndex {
    pub begin: BeginRecord,
    pub objects: BTreeMap<u64, ObjectMeta>,
    data: BTreeMap<u64, Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPath {
    pub normalized_path: String,
    pub object_id: u64,
    pub object_type: u32,
    pub bonus_type: u32,
    pub logical_size: u64,
    pub dirent_type: u8,
    pub size_bonus_offset: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: String,
    pub object_id: u64,
    pub dirent_type: u8,
    pub logical_size: Option<u64>,
}

impl ObjectIndex {
    pub fn build(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("opening ZFS send stream {}", path.display()))?;
        let mut reader = StreamReader::new(file);
        let mut begin = None;
        let mut objects = BTreeMap::new();
        let mut data: BTreeMap<u64, Vec<u8>> = BTreeMap::new();

        while let Some(record) = reader.next_record()? {
            match record.kind {
                RecordKind::Begin(header) => begin = Some(header),
                RecordKind::Object(object) => {
                    let bonus_len = usize::try_from(object.bonus_length)
                        .unwrap_or(usize::MAX)
                        .min(record.payload.len());
                    objects.insert(
                        object.object,
                        ObjectMeta {
                            object_type: object.object_type,
                            bonus_type: object.bonus_type,
                            block_size: object.block_size,
                            max_block_id: object.max_block_id,
                            bonus: record.payload[..bonus_len].to_vec(),
                        },
                    );
                    if capture_object_data(object.object_type) {
                        data.insert(object.object, Vec::new());
                    } else {
                        data.remove(&object.object);
                    }
                }
                RecordKind::Write(write) => {
                    if write.compression_type != 0 {
                        bail!(
                            "compressed WRITE record at offset {} is unsupported; create the stream without zfs send -c",
                            record.stream_offset
                        );
                    }
                    if let Some(bytes) = data.get_mut(&write.object) {
                        write_extent(bytes, write.offset, &record.payload)?;
                    }
                }
                RecordKind::WriteEmbedded(write) => {
                    bail!(
                        "embedded WRITE for object {} at offset {} is unsupported; create the stream without zfs send -e",
                        write.object,
                        record.stream_offset
                    );
                }
                RecordKind::WriteByRef => {
                    bail!(
                        "deduplicated WRITE_BYREF record at offset {} is unsupported",
                        record.stream_offset
                    );
                }
                RecordKind::ObjectRange => {
                    bail!("raw OBJECT_RANGE record is unsupported");
                }
                RecordKind::Redact => bail!("redacted streams are unsupported"),
                RecordKind::Free(free) => {
                    if let Some(bytes) = data.get_mut(&free.object) {
                        free_extent(bytes, free.offset, free.length)?;
                    }
                }
                RecordKind::FreeObjects(range) => {
                    let end = range.first_object.saturating_add(range.object_count);
                    objects.retain(|id, _| *id < range.first_object || *id >= end);
                    data.retain(|id, _| *id < range.first_object || *id >= end);
                }
                RecordKind::End | RecordKind::Spill => {}
            }
        }
        if !reader.saw_end() {
            bail!("ZFS send stream has no END record");
        }
        let begin = begin.ok_or_else(|| anyhow!("ZFS send stream has no BEGIN record"))?;
        if begin.from_guid != 0 {
            bail!("browsing requires a full send stream, but this stream is incremental");
        }

        Ok(Self {
            begin,
            objects,
            data,
        })
    }

    pub fn resolve_path(&self, path: &str) -> Result<ResolvedPath> {
        let normalized = normalize_path(path)?;
        let (registry, layouts) = self.sa_context()?;
        let master = self
            .data
            .get(&1)
            .ok_or_else(|| anyhow!("ZPL master node object 1 is absent"))?;
        let mut current = zap_lookup(master, "ROOT")
            .ok_or_else(|| anyhow!("ZPL master node has no ROOT entry"))?;
        let mut dirent_type = 4;

        for component in normalized.split('/').filter(|part| !part.is_empty()) {
            let directory = self.data.get(&current).ok_or_else(|| {
                anyhow!("path component {component:?} is not a readable directory")
            })?;
            let raw = zap_list(directory)
                .into_iter()
                .find(|(name, _)| name == component)
                .map(|(_, value)| value)
                .ok_or_else(|| anyhow!("path {normalized:?} was not found in the stream"))?;
            current = raw & ZPL_DIRENT_OBJ_MASK;
            dirent_type = (raw >> 60) as u8;
        }

        let meta = self.objects.get(&current).ok_or_else(|| {
            anyhow!("object {current} for path {normalized:?} has no OBJECT record")
        })?;
        let attrs = decode_attrs(meta, &registry, &layouts)
            .ok_or_else(|| anyhow!("could not decode ZPL metadata for object {current}"))?;

        Ok(ResolvedPath {
            normalized_path: normalized,
            object_id: current,
            object_type: meta.object_type,
            bonus_type: meta.bonus_type,
            logical_size: attrs.size,
            dirent_type,
            size_bonus_offset: size_bonus_offset(meta, &registry, &layouts),
        })
    }

    pub fn list_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        let resolved = self.resolve_path(path)?;
        if resolved.dirent_type != 4 {
            bail!("{} is not a directory", resolved.normalized_path);
        }
        let (registry, layouts) = self.sa_context()?;
        let directory = self
            .data
            .get(&resolved.object_id)
            .ok_or_else(|| anyhow!("directory object {} has no data", resolved.object_id))?;
        let mut entries = zap_list(directory)
            .into_iter()
            .map(|(name, raw)| {
                let object_id = raw & ZPL_DIRENT_OBJ_MASK;
                let logical_size = self
                    .objects
                    .get(&object_id)
                    .and_then(|meta| decode_attrs(meta, &registry, &layouts))
                    .map(|attrs| attrs.size);
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

    fn sa_context(&self) -> Result<(SaRegistry, SaLayouts)> {
        let master = self
            .data
            .get(&1)
            .ok_or_else(|| anyhow!("ZPL master node object 1 has no data"))?;
        let sa_master_id = zap_lookup(master, "SA_ATTRS")
            .ok_or_else(|| anyhow!("ZPL master node has no SA_ATTRS entry"))?;
        let sa_master = self
            .data
            .get(&sa_master_id)
            .ok_or_else(|| anyhow!("SA master object {sa_master_id} has no data"))?;
        let registry_id = zap_lookup(sa_master, "REGISTRY")
            .ok_or_else(|| anyhow!("SA master object has no REGISTRY entry"))?;
        let layouts_id = zap_lookup(sa_master, "LAYOUTS")
            .ok_or_else(|| anyhow!("SA master object has no LAYOUTS entry"))?;
        let registry = self
            .data
            .get(&registry_id)
            .ok_or_else(|| anyhow!("SA registry object {registry_id} has no data"))?;
        let layouts = self
            .data
            .get(&layouts_id)
            .ok_or_else(|| anyhow!("SA layouts object {layouts_id} has no data"))?;
        Ok((parse_sa_registry(registry), parse_sa_layouts(layouts)))
    }
}

fn capture_object_data(object_type: u32) -> bool {
    matches!(
        object_type,
        DMU_OT_DIRECTORY_CONTENTS
            | DMU_OT_MASTER_NODE
            | DMU_OT_SA_MASTER_NODE
            | DMU_OT_SA_ATTR_REGISTRATION
            | DMU_OT_SA_ATTR_LAYOUTS
    )
}

fn decode_attrs(
    meta: &ObjectMeta,
    registry: &SaRegistry,
    layouts: &SaLayouts,
) -> Option<zfs_core::ZplAttrs> {
    match meta.bonus_type {
        DMU_OT_SA => decode_sa_bonus(&meta.bonus, registry, layouts, Endian::Little),
        DMU_OT_ZNODE => decode_znode_phys(&meta.bonus, Endian::Little),
        _ => None,
    }
}

fn size_bonus_offset(meta: &ObjectMeta, registry: &SaRegistry, layouts: &SaLayouts) -> Option<u64> {
    if meta.bonus_type == DMU_OT_ZNODE {
        return (meta.bonus.len() >= 88).then_some(80);
    }
    if meta.bonus_type != DMU_OT_SA || meta.bonus.len() < 8 {
        return None;
    }
    let size_id = registry.by_name("ZPL_SIZE")?.id;
    let info = u16::from_le_bytes(meta.bonus[4..6].try_into().ok()?);
    let layout = u64::from(info & 0x03ff);
    let mut offset = usize::from((info >> 10) & 0x3f) << 3;
    for id in layouts.attr_ids(layout)? {
        if *id == size_id {
            return (offset + 8 <= meta.bonus.len()).then_some(offset as u64);
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
        Ok("/".into())
    } else {
        Ok(format!("/{}", components.join("/")))
    }
}

fn write_extent(bytes: &mut Vec<u8>, offset: u64, payload: &[u8]) -> Result<()> {
    let start = usize::try_from(offset).context("metadata object offset is too large")?;
    let end = start
        .checked_add(payload.len())
        .ok_or_else(|| anyhow!("metadata object extent overflow"))?;
    if end > MAX_METADATA_OBJECT {
        bail!("metadata object exceeds the 64 MiB safety limit");
    }
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[start..end].copy_from_slice(payload);
    Ok(())
}

fn free_extent(bytes: &mut Vec<u8>, offset: u64, length: u64) -> Result<()> {
    let start = usize::try_from(offset).context("FREE offset is too large")?;
    if start >= bytes.len() {
        return Ok(());
    }
    if length == u64::MAX {
        bytes.truncate(start);
        return Ok(());
    }
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    let end = start.saturating_add(length).min(bytes.len());
    bytes[start..end].fill(0);
    Ok(())
}
