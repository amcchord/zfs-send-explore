use super::*;
use std::io::Write;

fn xdr_pair(name: &str, kind: u32, value: &[u8]) -> Vec<u8> {
    let mut pair = vec![0; 8];
    pair.extend_from_slice(&(name.len() as u32).to_be_bytes());
    pair.extend_from_slice(name.as_bytes());
    pair.resize(pair.len().next_multiple_of(4), 0);
    pair.extend_from_slice(&kind.to_be_bytes());
    pair.extend_from_slice(&1_u32.to_be_bytes());
    pair.extend_from_slice(value);
    let size = (pair.len() as u32).to_be_bytes();
    pair[..4].copy_from_slice(&size);
    pair[4..8].copy_from_slice(&size);
    pair
}

fn root_slot() -> [u8; 1024] {
    let mut slot = [0_u8; 1024];
    for (i, word) in [zfs_core::UBERBLOCK_MAGIC, 5000, 15, 201, 42]
        .into_iter()
        .enumerate()
    {
        slot[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    slot
}

fn stripe_member(id: u64, state: u64) -> LabeledMember {
    let guid = 50 + id;
    let pool_guid = 100;
    let mut group = vec![0; 8];
    group.extend(xdr_pair("guid", 8, &guid.to_be_bytes()));
    group.extend([0; 8]);
    let mut nv = vec![1, 1, 0, 0];
    nv.extend([0; 8]);
    for (key, val) in [
        ("pool_guid", pool_guid),
        ("top_guid", guid),
        ("guid", guid),
        ("version", 5000),
        ("txg", 12),
        ("state", state),
    ] {
        nv.extend(xdr_pair(key, 8, &val.to_be_bytes()));
    }
    nv.extend(xdr_pair("vdev_tree", 19, &group));
    nv.extend([0; 8]);
    let mut bytes = vec![0; LABEL_SIZE];
    bytes[zfs_core::NVLIST_OFFSET..zfs_core::NVLIST_OFFSET + nv.len()].copy_from_slice(&nv);
    bytes[zfs_core::UBERBLOCK_RING_OFFSET..zfs_core::UBERBLOCK_RING_OFFSET + 1024]
        .copy_from_slice(&root_slot());
    let active = VdevLabel::parse(&bytes).unwrap();
    let image = tempfile::NamedTempFile::new().unwrap();
    image.as_file().set_len((4 * LABEL_SIZE) as u64).unwrap();
    LabeledMember {
        source: PoolSource::Plain(FileSource::open(image.path()).unwrap()),
        active,
        container_encryption: None,
        layout: PoolLayout {
            pool_name: "archive".into(),
            pool_guid: guid_string(pool_guid),
            member_guid: guid_string(guid),
            group_guid: guid_string(guid),
            group_id: id,
            group_type: "file".into(),
            top_level_groups: 2,
            stripe_candidate: true,
            supported_layout: false,
            guidance: String::new(),
        },
    }
}

#[test]
fn exported_and_active_stripes_pin_the_same_root_across_members() {
    for state in [0, 1] {
        let mut members = [stripe_member(0, state), stripe_member(1, state)];
        members[1].active.active_uberblock.timestamp += 2;
        validate_stripe(&members).unwrap();
        members[1].active.active_uberblock.txg -= 1;
        assert!(
            validate_stripe(&members)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    let mut members = [stripe_member(0, 0), stripe_member(1, 1)];
    assert!(validate_stripe(&members).is_err());
    members[1] = stripe_member(1, 0);
    // A reserved rootbp byte isn't surfaced by the decoded DVA fields. Compare
    // the complete on-disk root so two distinct raw pointers still disagree.
    let mut slot = root_slot();
    slot[40 + 56] = 1;
    members[1].active.active_uberblock = zfs_core::Uberblock::parse(&slot).unwrap();
    assert!(
        validate_stripe(&members)
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
}

#[test]
fn refuses_missing_duplicate_foreign_and_inconsistent_stripe_members() {
    let members = [stripe_member(1, 1)];
    assert!(
        validate_stripe(&members)
            .unwrap_err()
            .downcast::<PoolLayout>()
            .unwrap()
            .guidance
            .contains("group IDs 0")
    );
    let mut members = [stripe_member(0, 1), stripe_member(0, 1)];
    assert!(
        validate_stripe(&members)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    members[1] = stripe_member(1, 1);
    members[1].layout.pool_guid = guid_string(99);
    assert!(
        validate_stripe(&members)
            .unwrap_err()
            .to_string()
            .contains("different pools")
    );
    members[1] = stripe_member(1, 1);
    for member in &mut members {
        member.active.active_uberblock.guid_sum += 1;
    }
    assert!(
        validate_stripe(&members)
            .unwrap_err()
            .to_string()
            .contains("complete topology")
    );
}

#[test]
fn reads_dva_from_its_group_and_retries_a_ditto_copy_on_another_group() {
    let mut sources = Vec::new();
    for data in [0x21, 0x7a] {
        let mut image = tempfile::NamedTempFile::new().unwrap();
        image
            .as_file()
            .set_len(zfs_core::BOOT_SKEW + 512 + (2 * LABEL_SIZE) as u64)
            .unwrap();
        image.seek(SeekFrom::Start(zfs_core::BOOT_SKEW)).unwrap();
        image.write_all(&[data; 512]).unwrap();
        sources.push(PoolSource::Plain(FileSource::open(image.path()).unwrap()));
    }
    let mut pointer = Blkptr::default();
    pointer.dvas[0].asize_sectors = 1;
    pointer.dvas[0].vdev = 1;
    pointer.compression = zfs_core::CompressType::Off.raw();
    pointer.checksum = ChecksumType::Fletcher4.raw();
    pointer.checksum_words = zfs_core::checksum::fletcher4(&[0x7a; 512], pointer.byteorder);
    assert_eq!(read_block_from(&sources, &pointer).unwrap(), [0x7a; 512]);
    pointer.dvas[1] = pointer.dvas[0];
    pointer.dvas[0].vdev = 0;
    assert_eq!(read_block_from(&sources, &pointer).unwrap(), [0x7a; 512]);
    pointer.dvas[1].offset_sectors = 1_u64 << 55;
    assert!(
        read_block_from(&sources, &pointer)
            .unwrap_err()
            .to_string()
            .contains("offset overflow")
    );
    pointer.dvas[1].offset_sectors = 1;
    assert!(
        read_block_from(&sources, &pointer)
            .unwrap_err()
            .to_string()
            .contains("member bounds")
    );
}
