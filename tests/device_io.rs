//! Native device regression matrix. Every attachment is a disposable synthetic
//! fixture, attached read-only and detached on success or failure.
#![cfg(any(target_os = "macos", windows))]
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

struct Desktop {
    child: Child,
    input: ChildStdin,
    responses: Receiver<String>,
}
impl Desktop {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zfs-explore-service"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, responses) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            responses,
        }
    }
    fn request(&mut self, request: Value) -> Value {
        writeln!(self.input, "{request}").unwrap();
        self.input.flush().unwrap();
        serde_json::from_str(
            &self
                .responses
                .recv_timeout(Duration::from_secs(30))
                .expect("Device operations must respond"),
        )
        .unwrap()
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Attached {
    path: String,
    image: PathBuf,
}
impl Attached {
    #[cfg(target_os = "macos")]
    fn open(image: &Path) -> Self {
        let output = Command::new("hdiutil")
            .args([
                "attach",
                "-nomount",
                "-readonly",
                "-imagekey",
                "diskimage-class=CRawDiskImage",
            ])
            .arg(image)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        assert!(path.starts_with("/dev/disk"));
        Self {
            path,
            image: image.to_owned(),
        }
    }
    #[cfg(windows)]
    fn open(image: &Path) -> Self {
        // Add a standard fixed VHD footer to a COPY of this fixture. Only that
        // copy is attached. Mount-DiskImage never initializes/formats a disk.
        let path = image.with_extension("vhd");
        fs::copy(image, &path).unwrap();
        let size = fs::metadata(image).unwrap().len();
        let mut footer = [0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[8..12].copy_from_slice(&2u32.to_be_bytes());
        footer[12..16].copy_from_slice(&0x10000u32.to_be_bytes());
        footer[16..24].fill(0xff);
        footer[28..32].copy_from_slice(b"zfse");
        footer[32..36].copy_from_slice(&0x10000u32.to_be_bytes());
        footer[36..40].copy_from_slice(b"Wi2k");
        footer[40..48].copy_from_slice(&size.to_be_bytes());
        footer[48..56].copy_from_slice(&size.to_be_bytes());
        footer[56..60].copy_from_slice(&[0x03, 0xff, 16, 63]);
        footer[60..64].copy_from_slice(&2u32.to_be_bytes());
        // Each attachment has a distinct VHD identity; Windows caches storage
        // objects by this GUID across detach/attach operations.
        let identity = Sha256::digest(path.to_string_lossy().as_bytes());
        footer[68..84].copy_from_slice(&identity[..16]);
        let checksum = !footer.iter().map(|&b| u32::from(b)).sum::<u32>();
        footer[64..68].copy_from_slice(&checksum.to_be_bytes());
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&footer)
            .unwrap();
        let escaped = path.to_str().unwrap().replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='Stop'; Mount-DiskImage -ImagePath '{escaped}' -Access ReadOnly -NoDriveLetter | Out-Null; $device=(Get-DiskImage -ImagePath '{escaped}').DevicePath; if ($device -notmatch 'PhysicalDrive[0-9]+$') {{ throw 'The disposable disk has no device path' }}; $device"
        );
        // The guard must exist before attachment so an error during discovery
        // also detaches the exact image this test created.
        let mut attached = Self {
            path: String::new(),
            image: path,
        };
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        attached.path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert!(
            attached.path.starts_with(r"\\.\PhysicalDrive"),
            "Unexpected device path"
        );
        attached
    }
    fn detach(&self) {
        assert!(
            self.image.exists(),
            "Only detach a device created by this test"
        );
        #[cfg(target_os = "macos")]
        let status = Command::new("hdiutil")
            .args(["detach", "-quiet", &self.path])
            .status()
            .unwrap();
        #[cfg(windows)]
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Dismount-DiskImage -ImagePath '{}'",
                    self.image.display().to_string().replace('\'', "''")
                ),
            ])
            .status()
            .unwrap();
        assert!(status.success(), "Detach only the disposable test device");
    }
}
impl Drop for Attached {
    fn drop(&mut self) {
        // Best effort fallback when an assertion failed; normal detach is verified.
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", &self.path])
                .output();
        }
        #[cfg(windows)]
        {
            let _ = Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Dismount-DiskImage -ImagePath '{}' -ErrorAction SilentlyContinue",
                        self.image.display().to_string().replace('\'', "''")
                    ),
                ])
                .output();
        }
    }
}
fn expand(name: &str, base64: bool) -> Vec<u8> {
    let input = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap();
    let compressed = if base64 {
        base64::engine::general_purpose::STANDARD
            .decode(
                input
                    .iter()
                    .copied()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
    } else {
        input
    };
    let mut bytes = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(&compressed[..])
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}
fn mbr(payload: &[u8], number: usize, kind: u8) -> Vec<u8> {
    let mut bytes = vec![0; 1024 * 1024 + payload.len()];
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let entry = 446 + (number - 1) * 16;
    bytes[entry + 4] = kind;
    bytes[entry + 8..entry + 12].copy_from_slice(&2048u32.to_le_bytes());
    bytes[entry + 12..entry + 16].copy_from_slice(&((payload.len() / 512) as u32).to_le_bytes());
    bytes[1024 * 1024..].copy_from_slice(payload);
    bytes
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires administrator and the Windows disk-image service; run explicitly in disposable lab"
)]
fn attached_devices_restore_and_reopen_after_eject() {
    let temporary = tempfile::tempdir().unwrap();
    let pool = expand("native-encrypted-pool.img.zst", false);
    let fat = expand("inception/fat16.img.zst.b64", true);
    let mut desktop = Desktop::start();
    for (name, bytes, encrypted) in [
        ("pool", pool.clone(), true),
        ("pool-mbr3", mbr(&pool, 3, 0x83), true),
        ("fat16", fat.clone(), false),
        ("fat16-mbr1", mbr(&fat, 1, 0x06), false),
    ] {
        let image = temporary.path().join(format!("{name}.img"));
        fs::write(&image, &bytes).unwrap();
        let before = Sha256::digest(&bytes);
        let device = Attached::open(&image);
        #[cfg(windows)]
        let paths = [device.path.clone()];
        #[cfg(target_os = "macos")]
        let paths = [
            device.path.clone(),
            device.path.replace("/dev/disk", "/dev/rdisk"),
        ];
        for (i, path) in paths.iter().enumerate() {
            let response = desktop.request(json!({"method":"open", "path":path}));
            assert_eq!(response["ok"], true, "{name}: {response}");
            if encrypted {
                let index = response["result"]["views"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|view| view["selector"] == "encpool/secret")
                    .unwrap();
                assert_eq!(
                    desktop.request(json!({"method":"select", "index":index}))["ok"],
                    true
                );
                assert_eq!(
                    desktop.request(json!({"method":"unlock", "key":"hunter2!"}))["ok"],
                    true
                );
            } else {
                assert_eq!(
                    desktop.request(json!({"method":"list", "path":"/subdir"}))["ok"],
                    true
                );
            }
            let output = temporary.path().join(format!("{name}-{i}.txt"));
            let filename = if encrypted {
                "greeting.txt"
            } else {
                "NESTED.TXT"
            };
            let restored =
                desktop.request(json!({"method":"extract", "name":filename, "destination":output}));
            assert_eq!(restored["ok"], true, "{name}: {restored}");
            let actual = format!("{:x}", Sha256::digest(fs::read(&output).unwrap()));
            let expected = if encrypted {
                "fb13243e8d0033038d1740d8bb6cc0c9f34dd7087f9de133c6813530bb36d042".to_owned()
            } else {
                format!("{:x}", Sha256::digest(b"nested file content 16\n"))
            };
            assert_eq!(actual, expected, "Independent fixture oracle for {name}");
            assert_eq!(
                desktop.request(json!({"method":"extract", "name":filename, "destination":output}))
                    ["ok"],
                false
            );
            assert_eq!(
                desktop
                    .request(json!({"method":"open", "path":temporary.path().join("missing.img")}))
                    ["ok"],
                false
            );
            assert_eq!(
                desktop.request(
                    json!({"method":"list", "path": if encrypted { "/" } else { "/subdir" }})
                )["ok"],
                true
            );
        }
        // macOS refuses to detach a filesystem device while its handle is open.
        // Closing the desktop must release it, so normal ejection succeeds.
        assert_eq!(desktop.request(json!({"method":"close"}))["ok"], true);
        device.detach();
        assert_eq!(
            Sha256::digest(fs::read(&image).unwrap()),
            before,
            "The source remains unchanged"
        );
        // The old session must report an error, and a new source must still open.
        // Standalone images hold an OS handle; detached-device behavior varies.
        let opened = desktop.request(json!({"method":"open", "path":device.path}));
        assert_eq!(opened["ok"], false);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-full.zfs");
        assert_eq!(
            desktop.request(json!({"method":"open", "path":source}))["ok"],
            true
        );
        assert_eq!(desktop.request(json!({"method":"close"}))["ok"], true);
        println!(
            "PASS {name}: native device, restore oracle, overwrite refusal, failed-open preservation, read-only source and detach/reopen"
        );
    }
}
