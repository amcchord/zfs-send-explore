use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use zfs_send_extract::stream::{RECORD_SIZE, RecordKind, StreamReader};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zfs-send-extract"))
        .args(arguments)
        .output()
        .expect("run CLI")
}

#[test]
fn real_openzfs_stream_extracts_and_applies_an_incremental() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("hello.txt");

    let list = run(&["list", fixture("tiny-full.zfs").to_str().unwrap(), "/"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(String::from_utf8_lossy(&list.stdout).contains("hello.txt"));
    assert!(String::from_utf8_lossy(&list.stdout).contains("subdir"));

    let extract = run(&[
        "extract",
        fixture("tiny-full.zfs").to_str().unwrap(),
        "/hello.txt",
        "--output",
        target.to_str().unwrap(),
    ]);
    assert!(
        extract.status.success(),
        "{}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        b"hello from the base snapshot\n"
    );

    let apply = run(&[
        "apply",
        fixture("tiny-incremental.zfs").to_str().unwrap(),
        target.to_str().unwrap(),
    ]);
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        b"hello from the incremental snapshot\nwith an appended line\n"
    );
}

#[test]
fn checksum_corruption_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let corrupt = temporary.path().join("corrupt.zfs");
    let mut bytes = fs::read(fixture("tiny-full.zfs")).unwrap();
    bytes[400] ^= 0x40;
    fs::write(&corrupt, bytes).unwrap();

    let output = run(&["inspect", corrupt.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"));
}

#[test]
fn real_multi_snapshot_archive_extracts_each_selected_version() {
    let archive = fixture("multi-snapshot.zfs");
    let temporary = tempfile::tempdir().unwrap();

    let snapshots = run(&["snapshots", archive.to_str().unwrap()]);
    assert!(
        snapshots.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshots.stderr)
    );
    let catalog = String::from_utf8_lossy(&snapshots.stdout);
    assert!(catalog.contains("labpool/snapshot-select@s1"));
    assert!(catalog.contains("labpool/snapshot-select@s2"));
    assert!(catalog.contains("labpool/snapshot-select@s3"));

    let no_selection = run(&["list", archive.to_str().unwrap(), "/"]);
    assert!(!no_selection.status.success());
    assert!(String::from_utf8_lossy(&no_selection.stderr).contains("choose one with --snapshot"));

    let cases = [
        ("s1", "snapshot one\n"),
        (
            "labpool/snapshot-select@s2",
            "snapshot two has a longer value\n",
        ),
        ("0xe1ce9857ca54940e", "three\n"),
    ];
    for (selector, expected) in cases {
        let target = temporary.path().join(format!("{selector}.txt"));
        let extract = run(&[
            "extract",
            archive.to_str().unwrap(),
            "/version.txt",
            "--snapshot",
            selector,
            "--output",
            target.to_str().unwrap(),
        ]);
        assert!(
            extract.status.success(),
            "selector {selector}: {}",
            String::from_utf8_lossy(&extract.stderr)
        );
        assert_eq!(fs::read_to_string(target).unwrap(), expected);
    }

    let s2 = run(&["list", archive.to_str().unwrap(), "/", "--snapshot", "s2"]);
    let s3 = run(&["list", archive.to_str().unwrap(), "/", "--snapshot", "s3"]);
    assert!(String::from_utf8_lossy(&s2.stdout).contains("only-s2.txt"));
    assert!(!String::from_utf8_lossy(&s3.stdout).contains("only-s2.txt"));
    assert!(String::from_utf8_lossy(&s3.stdout).contains("only-s3.txt"));
}

#[test]
fn compound_incrementals_without_their_base_are_explained() {
    let bytes = fs::read(fixture("multi-snapshot.zfs")).unwrap();
    let mut reader = StreamReader::new(Cursor::new(&bytes));
    let mut base_end = None;
    while let Some(record) = reader.next_record().unwrap() {
        if matches!(record.kind, RecordKind::End) {
            base_end = Some(record.stream_offset as usize + RECORD_SIZE + record.payload.len());
            break;
        }
    }

    let temporary = tempfile::tempdir().unwrap();
    let increments = temporary.path().join("increments.zfs");
    fs::write(&increments, &bytes[base_end.unwrap()..]).unwrap();
    let target = temporary.path().join("version.txt");
    let output = run(&[
        "extract",
        increments.to_str().unwrap(),
        "/version.txt",
        "--snapshot",
        "s2",
        "--output",
        target.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not present earlier"));
}

#[test]
fn raw_encrypted_send_lists_and_extracts_with_an_authenticated_key() {
    let stream = fixture("encrypted-raw-s1.zfs");
    let temporary = tempfile::tempdir().unwrap();
    let key = temporary.path().join("passphrase");
    fs::write(&key, b"zfs-send-fixture-passphrase\n").unwrap();

    let snapshots = run(&["snapshots", stream.to_str().unwrap()]);
    assert!(snapshots.status.success());
    assert!(String::from_utf8_lossy(&snapshots.stdout).contains("zfse-encrypted-tiny@s1"));

    let list = run(&[
        "list",
        stream.to_str().unwrap(),
        "/docs",
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert!(String::from_utf8_lossy(&list.stdout).contains("hello.txt"));

    let target = temporary.path().join("hello.txt");
    let extract = run(&[
        "extract",
        stream.to_str().unwrap(),
        "/docs/hello.txt",
        "--output",
        target.to_str().unwrap(),
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        extract.status.success(),
        "{}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_eq!(fs::read(&target).unwrap(), b"encrypted hello\n");

    let wrong_key = temporary.path().join("wrong-passphrase");
    fs::write(&wrong_key, b"definitely-wrong\n").unwrap();
    let rejected = run(&[
        "list",
        stream.to_str().unwrap(),
        "/",
        "--key-file",
        wrong_key.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("supplied key did not authenticate")
    );
}

#[test]
fn pool_commands_reject_a_non_zfs_member() {
    let temporary = tempfile::tempdir().unwrap();
    let member = temporary.path().join("not-zfs.img");
    fs::write(&member, vec![0u8; 4 * 256 * 1024]).unwrap();
    let output = run(&["pool", "inspect", member.to_str().unwrap()]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("no readable ZFS vdev label"), "{error}");
}
