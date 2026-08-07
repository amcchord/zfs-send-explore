use crate::compression::{decode_embedded_write, decode_replay_write};
use crate::encrypted::{
    DatasetKey, EncryptionParams, RawBlockPointer, RawDnode, RawDnodeRange, RawSidecarState,
    decompress_block, is_encrypted_object_type,
};
use crate::stream::{
    BeginRecord, DMU_SUBSTREAM, FEATURE_RAW, ObjectRecord, RecordKind, StreamReader,
};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeMap, BTreeSet};
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
    pub spill: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct ObjectIndex {
    pub begin: BeginRecord,
    pub objects: BTreeMap<u64, ObjectMeta>,
    data: BTreeMap<u64, Vec<u8>>,
    raw_crypto: Option<(u64, u64)>,
    raw_dnodes: BTreeMap<u64, RawDnode>,
}

#[derive(Debug, Clone)]
pub struct SnapshotPlan {
    pub target: BeginRecord,
    pub chain: Vec<u64>,
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
    pub size_spill_offset: Option<u64>,
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
        Self::build_snapshot(path, None)
    }

    pub fn build_snapshot(path: &Path, selector: Option<&str>) -> Result<Self> {
        let plan = plan_snapshot(path, selector)?;
        Self::build_plan(path, &plan)
    }

    pub fn build_plan(path: &Path, plan: &SnapshotPlan) -> Result<Self> {
        Self::build_plan_with_key(path, plan, None)
    }

    pub fn build_plan_with_key(
        path: &Path,
        plan: &SnapshotPlan,
        key_material: Option<&[u8]>,
    ) -> Result<Self> {
        if plan.target.features & FEATURE_RAW != 0 {
            return Self::build_raw_full(path, plan, key_material);
        }
        let file = File::open(path)
            .with_context(|| format!("opening ZFS send stream {}", path.display()))?;
        let mut reader = StreamReader::new(file);
        let selected = plan.chain.iter().copied().collect::<BTreeSet<_>>();
        let mut active_snapshot = None;
        let mut seen = BTreeSet::new();
        let mut objects = BTreeMap::new();
        let mut data: BTreeMap<u64, Vec<u8>> = BTreeMap::new();

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
                RecordKind::Object(object) => {
                    let bonus_len = usize::try_from(object.bonus_length)
                        .unwrap_or(usize::MAX)
                        .min(record.payload.len());
                    let preserve_data = objects
                        .get(&object.object)
                        .is_some_and(|meta: &ObjectMeta| meta.object_type == object.object_type);
                    let spill = objects.get(&object.object).and_then(|meta| {
                        (object.flags & 0x04 != 0)
                            .then(|| meta.spill.clone())
                            .flatten()
                    });
                    objects.insert(
                        object.object,
                        ObjectMeta {
                            object_type: object.object_type,
                            bonus_type: object.bonus_type,
                            block_size: object.block_size,
                            max_block_id: object.max_block_id,
                            bonus: record.payload[..bonus_len].to_vec(),
                            spill,
                        },
                    );
                    if capture_object_data(object.object_type) {
                        if !preserve_data {
                            data.insert(object.object, Vec::new());
                        } else {
                            data.entry(object.object).or_default();
                        }
                    } else {
                        data.remove(&object.object);
                    }
                }
                RecordKind::Write(write) => {
                    if let Some(bytes) = data.get_mut(&write.object) {
                        let plaintext = decode_replay_write(
                            write.compression_type,
                            &record.payload,
                            write.logical_size,
                        )?;
                        write_extent(bytes, write.offset, &plaintext)?;
                    }
                }
                RecordKind::WriteEmbedded(write) => {
                    if let Some(bytes) = data.get_mut(&write.object) {
                        let plaintext = decode_embedded_write(
                            write.compression_type,
                            write.embedded_type,
                            &record.payload,
                            write.physical_size,
                            write.logical_size,
                        )?;
                        replace_extent(bytes, write.offset, write.length, &plaintext)?;
                    }
                }
                RecordKind::WriteByRef => {
                    bail!(
                        "deduplicated WRITE_BYREF record at offset {} is unsupported",
                        record.stream_offset
                    );
                }
                RecordKind::ObjectRange(_) => {
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
                RecordKind::Spill(spill) => {
                    let payload = decode_replay_write(0, &record.payload, spill.logical_size)?;
                    if let Some(meta) = objects.get_mut(&spill.object) {
                        meta.spill = Some(payload);
                    }
                }
                RecordKind::Begin(_) | RecordKind::End => {}
            }
        }
        if !reader.saw_end() {
            bail!("ZFS send stream has no END record");
        }
        if seen.len() != plan.chain.len() {
            bail!("stream changed between snapshot discovery and indexing");
        }

        Ok(Self {
            begin: plan.target.clone(),
            objects,
            data,
            raw_crypto: None,
            raw_dnodes: BTreeMap::new(),
        })
    }

    fn build_raw_full(
        path: &Path,
        plan: &SnapshotPlan,
        key_material: Option<&[u8]>,
    ) -> Result<Self> {
        let key_material = key_material.ok_or_else(|| {
            anyhow!("encrypted raw send requires --key-file or an interactive key prompt")
        })?;
        let file = File::open(path)
            .with_context(|| format!("opening ZFS send stream {}", path.display()))?;
        let mut reader = StreamReader::new(file);
        let selected = plan.chain.iter().copied().collect::<BTreeSet<_>>();
        let mut active_snapshot = None;
        let mut key: Option<DatasetKey> = None;
        let mut raw_crypto = None;
        let mut current_range: Option<RawDnodeRange> = None;
        let mut raw_dnodes = BTreeMap::new();
        let mut objects = BTreeMap::new();
        let mut data: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut seen = BTreeSet::new();

        while let Some(record) = reader.next_record()? {
            match &record.kind {
                RecordKind::Begin(header) => {
                    active_snapshot = (header.header_type == DMU_SUBSTREAM)
                        .then_some(header.to_guid)
                        .filter(|guid| selected.contains(guid));
                    if let Some(guid) = active_snapshot {
                        seen.insert(guid);
                        let params = EncryptionParams::from_begin_payload(&record.payload)?;
                        match raw_crypto {
                            None => raw_crypto = Some((params.guid, params.version)),
                            Some((crypto_guid, crypto_version))
                                if crypto_guid != params.guid
                                    || crypto_version != params.version =>
                            {
                                bail!(
                                    "selected raw snapshot chain changes encryption identity or format"
                                );
                            }
                            Some(_) => {}
                        }
                        key = Some(params.unlock(key_material)?);
                    }
                    continue;
                }
                RecordKind::End if active_snapshot.is_some() => {
                    finish_raw_range_state(
                        key.as_ref().expect("active raw stream has a key"),
                        current_range.take(),
                        &raw_dnodes,
                        &mut objects,
                    )?;
                    active_snapshot = None;
                    key = None;
                    continue;
                }
                RecordKind::End => {
                    active_snapshot = None;
                    continue;
                }
                _ if active_snapshot.is_none() => continue,
                _ => {}
            }

            let dataset_key = key.as_ref().expect("active raw stream has a key");
            let (_, crypto_version) = raw_crypto.expect("active raw stream has crypto identity");
            match record.kind {
                RecordKind::ObjectRange(range) => {
                    finish_raw_range_state(
                        dataset_key,
                        current_range.replace(RawDnodeRange {
                            first_object: range.first_object,
                            object_slots: range.object_slots,
                            salt: range.salt,
                            iv: range.iv,
                            mac: range.mac,
                            flags: range.flags,
                            crypto_version,
                        }),
                        &raw_dnodes,
                        &mut objects,
                    )?;
                }
                RecordKind::Object(object) => {
                    let range = current_range
                        .as_ref()
                        .ok_or_else(|| anyhow!("raw OBJECT record appears before OBJECT_RANGE"))?;
                    let range_end = range.first_object.saturating_add(range.object_slots);
                    if object.object < range.first_object || object.object >= range_end {
                        bail!("raw OBJECT record falls outside its OBJECT_RANGE");
                    }
                    let previous = raw_dnodes.get(&object.object);
                    let compatible =
                        previous.is_some_and(|dnode| raw_layout_compatible(dnode, &object));
                    let mut blocks = if compatible {
                        previous
                            .expect("compatible raw dnode exists")
                            .blocks
                            .clone()
                    } else {
                        Vec::new()
                    };
                    blocks.retain(|block| block.block_id <= object.max_block_id);
                    let spill = if object.flags & 0x04 != 0 && compatible {
                        previous.and_then(|dnode| dnode.spill.clone())
                    } else {
                        None
                    };
                    let preserve_data = objects
                        .get(&object.object)
                        .is_some_and(|meta: &ObjectMeta| meta.object_type == object.object_type);
                    let preserve_spill = objects.get(&object.object).and_then(|meta| {
                        (object.flags & 0x04 != 0)
                            .then(|| meta.spill.clone())
                            .flatten()
                    });
                    let dnode = RawDnode {
                        object: object.object,
                        object_type: object.object_type,
                        bonus_type: object.bonus_type,
                        block_size: object.block_size,
                        bonus_length: object.bonus_length,
                        checksum_type: object.checksum_type,
                        compression: object.compression,
                        slots: object.dnode_slots,
                        flags: object.flags,
                        indirect_block_shift: object.indirect_block_shift,
                        levels: object.levels,
                        block_pointers: object.block_pointers,
                        max_block_id: object.max_block_id,
                        bonus_ciphertext: record.payload,
                        blocks,
                        spill,
                    };
                    raw_dnodes.insert(object.object, dnode);
                    objects.insert(
                        object.object,
                        ObjectMeta {
                            object_type: object.object_type,
                            bonus_type: object.bonus_type,
                            block_size: object.block_size,
                            max_block_id: object.max_block_id,
                            bonus: Vec::new(),
                            spill: preserve_spill,
                        },
                    );
                    if capture_object_data(object.object_type) {
                        if preserve_data {
                            data.entry(object.object).or_default();
                        } else {
                            data.insert(object.object, Vec::new());
                        }
                    } else {
                        data.remove(&object.object);
                    }
                }
                RecordKind::Write(write) => {
                    let dnode = raw_dnodes.get_mut(&write.object).ok_or_else(|| {
                        anyhow!("raw WRITE for object {} has no OBJECT record", write.object)
                    })?;
                    if write.logical_size == 0 || write.offset % write.logical_size != 0 {
                        bail!(
                            "raw WRITE for object {} has an invalid block offset",
                            write.object
                        );
                    }
                    let pointer = RawBlockPointer {
                        block_id: write.offset / write.logical_size,
                        object_type: write.object_type,
                        logical_size: write.logical_size,
                        physical_size: write.compressed_size,
                        compression: write.compression_type,
                        flags: write.flags,
                        mac: write.mac,
                    };
                    if let Some(existing) = dnode
                        .blocks
                        .iter_mut()
                        .find(|block| block.block_id == pointer.block_id)
                    {
                        *existing = pointer;
                    } else {
                        dnode.blocks.push(pointer);
                    }

                    if let Some(bytes) = data.get_mut(&write.object) {
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
                        let plaintext = decompress_block(
                            write.compression_type,
                            &protected,
                            write.logical_size,
                        )?;
                        write_extent(bytes, write.offset, &plaintext)?;
                    }
                }
                RecordKind::Free(free) => {
                    if let Some(dnode) = raw_dnodes.get_mut(&free.object) {
                        free_raw_blocks(dnode, free.offset, free.length)?;
                    }
                    if let Some(bytes) = data.get_mut(&free.object) {
                        free_extent(bytes, free.offset, free.length)?;
                    }
                }
                RecordKind::FreeObjects(range) => {
                    let end = range.first_object.saturating_add(range.object_count);
                    objects.retain(|id, _| *id < range.first_object || *id >= end);
                    data.retain(|id, _| *id < range.first_object || *id >= end);
                    raw_dnodes.retain(|id, _| *id < range.first_object || *id >= end);
                }
                RecordKind::WriteEmbedded(write) => bail!(
                    "raw embedded WRITE for object {} is unsupported",
                    write.object
                ),
                RecordKind::WriteByRef => bail!("raw deduplicated WRITE_BYREF is unsupported"),
                RecordKind::Spill(spill) => {
                    let dnode = raw_dnodes.get_mut(&spill.object).ok_or_else(|| {
                        anyhow!("raw SPILL for object {} has no OBJECT record", spill.object)
                    })?;
                    dnode.spill = Some(RawBlockPointer {
                        block_id: u64::MAX,
                        object_type: DMU_OT_SA,
                        logical_size: spill.logical_size,
                        physical_size: spill.compressed_size,
                        compression: spill.compression_type,
                        flags: spill.flags,
                        mac: spill.mac,
                    });
                    let protected = dataset_key.decrypt_block(
                        &spill.salt,
                        &spill.iv,
                        &spill.mac,
                        &[],
                        &record.payload,
                    )?;
                    let plaintext =
                        decompress_block(spill.compression_type, &protected, spill.logical_size)?;
                    objects
                        .get_mut(&spill.object)
                        .expect("raw spill object was inserted")
                        .spill = Some(plaintext);
                }
                RecordKind::Redact => bail!("redacted streams are unsupported"),
                RecordKind::Begin(_) | RecordKind::End => {}
            }
        }
        if !reader.saw_end() {
            bail!("ZFS send stream has no END record");
        }
        if seen.len() != plan.chain.len() {
            bail!("selected raw snapshot chain disappeared while indexing the stream");
        }
        Ok(Self {
            begin: plan.target.clone(),
            objects,
            data,
            raw_crypto,
            raw_dnodes,
        })
    }

    pub fn raw_sidecar_state(&self, object: u64) -> Result<Option<RawSidecarState>> {
        let Some((crypto_guid, crypto_version)) = self.raw_crypto else {
            return Ok(None);
        };
        let object_slots = 32;
        let first_object = object / object_slots * object_slots;
        let end = first_object + object_slots;
        let dnodes = self
            .raw_dnodes
            .range(first_object..end)
            .map(|(_, dnode)| dnode.clone())
            .collect::<Vec<_>>();
        if !dnodes.iter().any(|dnode| dnode.object == object) {
            bail!("raw state for extracted object {object} is missing");
        }
        Ok(Some(RawSidecarState {
            crypto_guid,
            crypto_version,
            first_object,
            object_slots,
            dnodes,
            target_spill: self
                .objects
                .get(&object)
                .and_then(|meta| meta.spill.clone()),
        }))
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
            size_spill_offset: size_spill_offset(meta, &registry, &layouts),
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

fn finish_raw_range_state(
    key: &DatasetKey,
    range: Option<RawDnodeRange>,
    raw_dnodes: &BTreeMap<u64, RawDnode>,
    objects: &mut BTreeMap<u64, ObjectMeta>,
) -> Result<()> {
    let Some(range) = range else {
        return Ok(());
    };
    let end = range
        .first_object
        .checked_add(range.object_slots)
        .ok_or_else(|| anyhow!("raw OBJECT_RANGE overflows its object IDs"))?;
    let dnodes = raw_dnodes
        .range(range.first_object..end)
        .map(|(_, dnode)| dnode.clone())
        .collect::<Vec<_>>();
    let bonuses = key.decrypt_dnode_bonuses(range, &dnodes)?;
    for dnode in &dnodes {
        if let Some(bonus) = bonuses.get(&dnode.object) {
            let length =
                usize::try_from(dnode.bonus_length).context("bonus length is too large")?;
            let meta = objects
                .get_mut(&dnode.object)
                .ok_or_else(|| anyhow!("raw object {} is missing from the index", dnode.object))?;
            meta.bonus = bonus[..length].to_vec();
        } else if dnode.bonus_length != 0 {
            let length =
                usize::try_from(dnode.bonus_length).context("bonus length is too large")?;
            if dnode.bonus_ciphertext.len() < length {
                bail!("raw object {} has a truncated bonus", dnode.object);
            }
            objects
                .get_mut(&dnode.object)
                .expect("raw object was inserted")
                .bonus = dnode.bonus_ciphertext[..length].to_vec();
        }
    }
    Ok(())
}

fn raw_layout_compatible(dnode: &RawDnode, object: &ObjectRecord) -> bool {
    dnode.object_type == object.object_type
        && dnode.block_size == object.block_size
        && dnode.slots == object.dnode_slots
        && dnode.indirect_block_shift == object.indirect_block_shift
        && dnode.levels == object.levels
        && dnode.block_pointers == object.block_pointers
}

fn free_raw_blocks(dnode: &mut RawDnode, offset: u64, length: u64) -> Result<()> {
    let end = if length == u64::MAX {
        u64::MAX
    } else {
        offset
            .checked_add(length)
            .ok_or_else(|| anyhow!("raw FREE extent overflow"))?
    };
    dnode.blocks.retain(|block| {
        let Some(start) = block.block_id.checked_mul(block.logical_size) else {
            return false;
        };
        let block_end = start.saturating_add(block.logical_size);
        block_end <= offset || start >= end
    });
    Ok(())
}

pub fn snapshot_headers(path: &Path) -> Result<Vec<BeginRecord>> {
    let file =
        File::open(path).with_context(|| format!("opening ZFS send stream {}", path.display()))?;
    let mut reader = StreamReader::new(file);
    let mut snapshots = Vec::new();
    while let Some(record) = reader.next_record()? {
        if let RecordKind::Begin(header) = record.kind
            && header.header_type == DMU_SUBSTREAM
        {
            snapshots.push(header);
        }
    }
    if !reader.saw_end() {
        bail!("ZFS send stream has no END record");
    }
    if snapshots.is_empty() {
        bail!("ZFS send stream contains no snapshot substreams");
    }
    Ok(snapshots)
}

pub fn plan_snapshot(path: &Path, selector: Option<&str>) -> Result<SnapshotPlan> {
    let snapshots = snapshot_headers(path)?;
    let target_index = select_snapshot(&snapshots, selector)?;
    let target = snapshots[target_index].clone();
    let mut chain = vec![target.to_guid];
    let mut cursor = target_index;
    let mut from_guid = target.from_guid;

    while from_guid != 0 {
        let Some((index, header)) = snapshots[..cursor]
            .iter()
            .enumerate()
            .rev()
            .find(|(_, header)| header.to_guid == from_guid)
        else {
            bail!(
                "snapshot {} depends on base GUID 0x{from_guid:016x}, which is not present earlier in the send file",
                target.dataset_name
            );
        };
        chain.push(header.to_guid);
        cursor = index;
        from_guid = header.from_guid;
    }
    chain.reverse();
    Ok(SnapshotPlan { target, chain })
}

fn select_snapshot(snapshots: &[BeginRecord], selector: Option<&str>) -> Result<usize> {
    let Some(selector) = selector else {
        if snapshots.len() == 1 {
            return Ok(0);
        }
        bail!(
            "send file contains {} snapshots; choose one with --snapshot (run `snapshots` to list them)",
            snapshots.len()
        );
    };

    let exact = snapshots
        .iter()
        .enumerate()
        .filter(|(_, header)| header.dataset_name == selector)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }

    let short = selector.strip_prefix('@').unwrap_or(selector);
    let named = snapshots
        .iter()
        .enumerate()
        .filter(|(_, header)| {
            header
                .dataset_name
                .rsplit_once('@')
                .is_some_and(|(_, name)| name == short)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if named.len() == 1 {
        return Ok(named[0]);
    }
    if named.len() > 1 {
        bail!(
            "snapshot name {selector:?} is ambiguous; use the full dataset@snapshot name or GUID"
        );
    }

    if let Some(guid) = parse_selector_guid(selector) {
        let matches = snapshots
            .iter()
            .enumerate()
            .filter(|(_, header)| header.to_guid == guid)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches[0]);
        }
        if matches.len() > 1 {
            bail!("snapshot GUID {selector:?} appears more than once in the send file");
        }
    }

    bail!("snapshot {selector:?} was not found in the send file")
}

fn parse_selector_guid(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse().ok(),
            |hex| u64::from_str_radix(hex, 16).ok(),
        )
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
        DMU_OT_SA => {
            let mut attrs = decode_sa_bonus(&meta.bonus, registry, layouts, Endian::Little)?;
            if let Some(spill) = meta.spill.as_deref()
                && let Some(spill_attrs) = decode_sa_bonus(spill, registry, layouts, Endian::Little)
            {
                merge_spill_attrs(&mut attrs, spill_attrs, spill, registry, layouts);
            }
            Some(attrs)
        }
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
    sa_attribute_offset(&meta.bonus, "ZPL_SIZE", registry, layouts)
}

fn size_spill_offset(meta: &ObjectMeta, registry: &SaRegistry, layouts: &SaLayouts) -> Option<u64> {
    (meta.bonus_type == DMU_OT_SA)
        .then_some(meta.spill.as_deref())
        .flatten()
        .and_then(|spill| sa_attribute_offset(spill, "ZPL_SIZE", registry, layouts))
}

fn sa_attribute_offset(
    buffer: &[u8],
    name: &str,
    registry: &SaRegistry,
    layouts: &SaLayouts,
) -> Option<u64> {
    if buffer.len() < 8 {
        return None;
    }
    let attr_id = registry.by_name(name)?.id;
    let info = u16::from_le_bytes(buffer[4..6].try_into().ok()?);
    let layout = u64::from(info & 0x03ff);
    let mut offset = usize::from((info >> 10) & 0x3f) << 3;
    for id in layouts.attr_ids(layout)? {
        if *id == attr_id {
            return (offset + usize::from(registry.size_of(*id)?) <= buffer.len())
                .then_some(offset as u64);
        }
        let size = usize::from(registry.size_of(*id)?);
        if size == 0 {
            return None;
        }
        offset = offset.checked_add(size)?;
    }
    None
}

fn merge_spill_attrs(
    base: &mut zfs_core::ZplAttrs,
    spill: zfs_core::ZplAttrs,
    buffer: &[u8],
    registry: &SaRegistry,
    layouts: &SaLayouts,
) {
    macro_rules! replace_if_present {
        ($name:literal, $field:ident) => {
            if sa_attribute_offset(buffer, $name, registry, layouts).is_some() {
                base.$field = spill.$field;
            }
        };
    }
    replace_if_present!("ZPL_MODE", mode);
    replace_if_present!("ZPL_SIZE", size);
    replace_if_present!("ZPL_LINKS", links);
    replace_if_present!("ZPL_UID", uid);
    replace_if_present!("ZPL_GID", gid);
    replace_if_present!("ZPL_GEN", r#gen);
    replace_if_present!("ZPL_PARENT", parent);
    replace_if_present!("ZPL_FLAGS", flags);
    replace_if_present!("ZPL_ATIME", atime);
    replace_if_present!("ZPL_MTIME", mtime);
    replace_if_present!("ZPL_CTIME", ctime);
    replace_if_present!("ZPL_CRTIME", crtime);
    base.unknown_attr_ids.extend(spill.unknown_attr_ids);
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

fn replace_extent(bytes: &mut Vec<u8>, offset: u64, length: u64, payload: &[u8]) -> Result<()> {
    if u64::try_from(payload.len()).unwrap_or(u64::MAX) > length {
        bail!("embedded replay payload exceeds its logical block length");
    }
    let start = usize::try_from(offset).context("embedded object offset is too large")?;
    let length = usize::try_from(length).context("embedded object length is too large")?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| anyhow!("embedded metadata object extent overflow"))?;
    if end > MAX_METADATA_OBJECT {
        bail!("metadata object exceeds the 64 MiB safety limit");
    }
    if bytes.len() < end {
        bytes.resize(end, 0);
    }
    bytes[start..end].fill(0);
    bytes[start..start + payload.len()].copy_from_slice(payload);
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
