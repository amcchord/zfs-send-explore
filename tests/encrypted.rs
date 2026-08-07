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
