use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use zfs_send_extract::client::{SourceCatalog, SourceKind};
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

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn expand_native_encrypted_pool(target: &Path) {
    let compressed = File::open(fixture("native-encrypted-pool.img.zst")).unwrap();
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(compressed).unwrap();
    let mut output = File::create(target).unwrap();
    let mut buffer = [0_u8; 64 * 1024];
    let mut length = 0_u64;
    loop {
        let read = decoder.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        if buffer[..read].iter().all(|byte| *byte == 0) {
            output.seek(SeekFrom::Current(read as i64)).unwrap();
        } else {
            output.write_all(&buffer[..read]).unwrap();
        }
        length += read as u64;
    }
    output.set_len(length).unwrap();
    assert_eq!(length, 128 * 1024 * 1024);
}

fn flip_byte_at(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x40;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&byte).unwrap();
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
fn real_openzfs_stream_recursively_extracts_a_staged_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("recovered");
    let extract = run(&[
        "extract-tree",
        fixture("tiny-full.zfs").to_str().unwrap(),
        "/",
        "--output",
        target.to_str().unwrap(),
    ]);
    assert!(
        extract.status.success(),
        "{}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_eq!(
        fs::read(target.join("hello.txt")).unwrap(),
        b"hello from the base snapshot\n"
    );
    assert!(target.join("subdir").is_dir());
    assert!(!target.join("hello.txt.zfse.json").exists());
    assert!(String::from_utf8_lossy(&extract.stdout).contains("3 files"));
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
fn native_encrypted_pool_lists_and_extracts_with_an_authenticated_key() {
    let temporary = tempfile::tempdir().unwrap();
    let member = temporary.path().join("native-encrypted-pool.img");
    expand_native_encrypted_pool(&member);

    let key = temporary.path().join("passphrase");
    fs::write(&key, b"hunter2!\n").unwrap();

    let missing_key = run(&[
        "pool",
        "list",
        member.to_str().unwrap(),
        "encpool/secret",
        "/",
    ]);
    assert!(!missing_key.status.success());
    assert!(String::from_utf8_lossy(&missing_key.stderr).contains("--key-file"));

    let wrong_key = temporary.path().join("wrong-passphrase");
    fs::write(&wrong_key, b"definitely-wrong\n").unwrap();
    let rejected = run(&[
        "pool",
        "list",
        member.to_str().unwrap(),
        "encpool/secret",
        "/",
        "--key-file",
        wrong_key.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("supplied key did not authenticate")
    );

    let list = run(&[
        "pool",
        "list",
        member.to_str().unwrap(),
        "encpool/secret",
        "/",
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let directory = String::from_utf8_lossy(&list.stdout);
    assert!(directory.contains("blob.bin"));
    assert!(directory.contains("greeting.txt"));

    let catalog = SourceCatalog::open_pool(&member).unwrap();
    assert_eq!(catalog.kind, SourceKind::PoolMember);
    let encrypted_view = catalog
        .views
        .iter()
        .position(|view| view.selector == "encpool/secret")
        .unwrap();
    assert!(catalog.views[encrypted_view].encrypted);
    assert!(
        catalog
            .list_directory(encrypted_view, "/", None)
            .unwrap_err()
            .to_string()
            .contains("choose its passphrase key file")
    );
    assert!(
        catalog
            .list_directory(encrypted_view, "/", Some(&wrong_key))
            .unwrap_err()
            .to_string()
            .contains("supplied key did not authenticate")
    );
    assert!(
        catalog
            .list_directory(encrypted_view, "/", Some(&key))
            .unwrap()
            .iter()
            .any(|entry| entry.name == "greeting.txt")
    );
    let client_target = temporary.path().join("client-greeting.txt");
    let client_extraction = catalog
        .extract(
            encrypted_view,
            "/greeting.txt",
            &client_target,
            false,
            Some(&key),
        )
        .unwrap();
    assert_eq!(client_extraction.logical_size, 20);
    assert_eq!(fs::read(&client_target).unwrap(), b"hello-encrypted-zfs\n");

    for (source, name, expected_size, expected_sha256) in [
        (
            "/greeting.txt",
            "greeting.txt",
            20,
            "fb13243e8d0033038d1740d8bb6cc0c9f34dd7087f9de133c6813530bb36d042",
        ),
        (
            "/blob.bin",
            "blob.bin",
            8192,
            "99b3de991bdff384c8489a793fb8698ba4002fbff01acb2c20dc0b79dc8cdf42",
        ),
    ] {
        let target = temporary.path().join(name);
        let extract = run(&[
            "pool",
            "extract",
            member.to_str().unwrap(),
            "encpool/secret",
            source,
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
        assert_eq!(fs::metadata(&target).unwrap().len(), expected_size);
        assert_eq!(sha256(&target), expected_sha256);
        assert!(!PathBuf::from(format!("{}.zfse.json", target.display())).exists());
    }

    // This fixture's encrypted dnode array has two DVAs. Corrupting the first
    // verifies folded-checksum rejection and alternate-copy fallback; corrupting
    // both proves unauthenticated metadata is never returned.
    flip_byte_at(&member, 4_747_264);
    let fallback = run(&[
        "pool",
        "list",
        member.to_str().unwrap(),
        "encpool/secret",
        "/",
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        fallback.status.success(),
        "{}",
        String::from_utf8_lossy(&fallback.stderr)
    );
    flip_byte_at(&member, 21_516_288);
    let corrupt = run(&[
        "pool",
        "list",
        member.to_str().unwrap(),
        "encpool/secret",
        "/",
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("checksum mismatch"));
}

#[test]
fn raw_zstd_spill_chain_extracts_and_applies_an_authenticated_incremental() {
    let full = fixture("advanced-raw-full.zfs");
    let incremental = fixture("advanced-raw-incremental.zfs");
    let temporary = tempfile::tempdir().unwrap();
    let key = temporary.path().join("passphrase");
    fs::write(&key, b"zfs-send-fixture-passphrase\n").unwrap();

    let mut reader = StreamReader::new(fs::File::open(&full).unwrap());
    let mut saw_spill = false;
    let mut saw_zstd = false;
    while let Some(record) = reader.next_record().unwrap() {
        match record.kind {
            RecordKind::Spill(_) => saw_spill = true,
            RecordKind::Write(write) if write.compression_type == 16 => saw_zstd = true,
            _ => {}
        }
    }
    assert!(saw_spill, "real raw fixture must contain an SA spill block");
    assert!(saw_zstd, "real raw fixture must contain Zstandard blocks");

    let target = temporary.path().join("raw-target.bin");
    let extract = run(&[
        "extract",
        full.to_str().unwrap(),
        "/payload/target.bin",
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
    assert_eq!(fs::metadata(&target).unwrap().len(), 2 * 1024 * 1024);
    assert_eq!(
        sha256(&target),
        "9aa35c1088ccaa0785d8c10fa23c740e9202368d29fb2e4972ec10a28d9d490f"
    );
    let sidecar = fs::read_to_string(format!("{}.zfse.json", target.display())).unwrap();
    assert!(sidecar.contains("\"raw_state\""));
    assert!(!sidecar.contains("zfs-send-fixture-passphrase"));

    let missing_key = run(&[
        "apply",
        incremental.to_str().unwrap(),
        target.to_str().unwrap(),
    ]);
    assert!(!missing_key.status.success());
    assert!(String::from_utf8_lossy(&missing_key.stderr).contains("--key-file"));

    let apply = run(&[
        "apply",
        incremental.to_str().unwrap(),
        target.to_str().unwrap(),
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(fs::metadata(&target).unwrap().len(), 2_359_296);
    assert_eq!(
        sha256(&target),
        "db5d733aba08cebf9d52621e01f64fe925c071a74de697dc2ba4792bfac38d28"
    );

    let history = temporary.path().join("raw-history.zfs");
    let mut history_bytes = fs::read(&full).unwrap();
    history_bytes.extend_from_slice(&fs::read(&incremental).unwrap());
    fs::write(&history, history_bytes).unwrap();
    let from_chain = temporary.path().join("raw-chain-target.bin");
    let chain_extract = run(&[
        "extract",
        history.to_str().unwrap(),
        "/payload/target.bin",
        "--snapshot",
        "s2",
        "--output",
        from_chain.to_str().unwrap(),
        "--key-file",
        key.to_str().unwrap(),
    ]);
    assert!(
        chain_extract.status.success(),
        "{}",
        String::from_utf8_lossy(&chain_extract.stderr)
    );
    assert_eq!(sha256(&from_chain), sha256(&target));
}

#[test]
fn compressed_and_embedded_streams_extract_and_apply() {
    let full = fixture("advanced-plain-full.zfs");
    let incremental = fixture("advanced-plain-incremental.zfs");
    let temporary = tempfile::tempdir().unwrap();

    let mut reader = StreamReader::new(fs::File::open(&full).unwrap());
    let mut saw_compressed = false;
    let mut saw_embedded = false;
    while let Some(record) = reader.next_record().unwrap() {
        match record.kind {
            RecordKind::Write(write) if write.compression_type != 0 => saw_compressed = true,
            RecordKind::WriteEmbedded(_) => saw_embedded = true,
            _ => {}
        }
    }
    assert!(saw_compressed);
    assert!(saw_embedded);

    let target = temporary.path().join("plain-target.bin");
    let embedded = temporary.path().join("embedded.bin");
    for (path, output) in [
        ("/payload/target.bin", target.as_path()),
        ("/payload/embedded.bin", embedded.as_path()),
    ] {
        let result = run(&[
            "extract",
            full.to_str().unwrap(),
            path,
            "--output",
            output.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        sha256(&target),
        "af2e223b435354ed53190ee2e9cbbe8612e90ab691639aa91f71dce0de47a027"
    );
    assert_eq!(
        sha256(&embedded),
        "fcf23bb6294ddeca564cb0cf6a256dd15dc01516a792f644b694e172e4f7f89f"
    );

    for output in [&target, &embedded] {
        let result = run(&[
            "apply",
            incremental.to_str().unwrap(),
            output.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        sha256(&target),
        "c8a198dbc1e37af9ae2899906d7b95266fc2c1e2e3f49f5a5b8c3463d14ef368"
    );
    assert_eq!(
        sha256(&embedded),
        "e45b0cd2e205653ec280d92f9bad6b9b793f0e98c948dac72cd0f558de7ba1c5"
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
