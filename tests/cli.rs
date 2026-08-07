use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
