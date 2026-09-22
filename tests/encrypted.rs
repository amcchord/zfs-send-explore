use std::fs::File;
use std::path::Path;
use zfs_send_extract::encrypted::{EncryptionParams, decompress_block, is_encrypted_object_type};
use zfs_send_extract::filesystem::{ObjectIndex, plan_snapshot};
use zfs_send_extract::stream::{RecordKind, StreamReader};

#[test]
fn unlocks_authenticates_and_decrypts_a_raw_send_fixture() {
    let path = Path::new("tests/fixtures/encrypted-raw-s1.zfs");

    let mut reader = StreamReader::new(File::open(path).unwrap());
    let begin = reader.next_record().unwrap().unwrap();
    let params = EncryptionParams::from_begin_payload(&begin.payload).unwrap();
    assert_eq!(params.key_format_name().unwrap(), "passphrase");
    let key = params.unlock(b"zfs-send-fixture-passphrase").unwrap();

    let mut verified_hmac = false;
    let mut decrypted_file = false;
    while let Some(record) = reader.next_record().unwrap() {
        if let RecordKind::Write(write) = record.kind {
            if write.object == 1 {
                assert!(!is_encrypted_object_type(write.object_type));
                key.authenticate_block(&record.payload, &write.mac).unwrap();
                verified_hmac = true;
            }
            if write.object == 128 {
                assert!(is_encrypted_object_type(write.object_type));
                let compressed = key
                    .decrypt_block(&write.salt, &write.iv, &write.mac, &[], &record.payload)
                    .unwrap();
                let plain =
                    decompress_block(write.compression_type, &compressed, write.logical_size)
                        .unwrap();
                assert_eq!(&plain[..16], b"encrypted hello\n");
                let mut bad_mac = write.mac;
                bad_mac[0] ^= 1;
                assert!(
                    key.decrypt_block(&write.salt, &write.iv, &bad_mac, &[], &record.payload)
                        .is_err()
                );
                decrypted_file = true;
            }
        }
    }
    assert!(verified_hmac);
    assert!(decrypted_file);

    let plan = plan_snapshot(path, None).unwrap();
    let index = ObjectIndex::build_plan_with_key(path, &plan, Some(b"zfs-send-fixture-passphrase"))
        .unwrap();
    let resolved = index.resolve_path("/docs/hello.txt").unwrap();
    assert_eq!(resolved.object_id, 128);
    assert_eq!(resolved.logical_size, 16);
}

#[test]
fn raw_send_authenticates_completely_sparse_indirect_subtrees() {
    use zfs_send_extract::client::SourceCatalog;
    let source = SourceCatalog::open_send("tests/fixtures/sparse-indirect-raw.zfs").unwrap();
    let key = b"public-sparse-regression-passphrase";
    let entries = source
        .list_directory_with_key_material(0, "/", Some(key))
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.name == "sparse.bin" && e.logical_size == Some(512 * 1024 * 1024))
    );
    let output = tempfile::tempdir().unwrap();
    let restored = source
        .extract_with_key_material(
            0,
            "/hello.txt",
            &output.path().join("hello.txt"),
            false,
            Some(key),
        )
        .unwrap();
    assert_eq!(
        restored.sha256,
        "2e9979937ed94de4090045ab5f71102891bc0f3c5f69e05979b7e6f660c5fd37"
    );
    assert_eq!(
        std::fs::read(output.path().join("hello.txt")).unwrap(),
        b"sparse indirect block regression\n"
    );
}
