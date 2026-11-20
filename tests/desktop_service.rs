use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

struct Desktop {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
}
impl Desktop {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zfs-explore-service"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, responses) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
        }
    }
    fn request(&mut self, value: Value) -> Value {
        serde_json::to_writer(&mut self.stdin, &value).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
        let response = self
            .responses
            .recv_timeout(Duration::from_secs(20))
            .expect("desktop must respond while its input pipe remains open");
        serde_json::from_str(&response).unwrap()
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn desktop_restores_without_overwriting_and_preserves_session_after_errors() {
    let mut desktop = Desktop::start();
    let opened = desktop.request(json!({"method":"open", "path":fixture("tiny-full.zfs")}));
    assert_eq!(opened["ok"], true);
    assert_eq!(opened["result"]["title"], "tiny-full.zfs");
    let metadata =
        zfs_send_extract::operations::snapshots(std::path::Path::new(&fixture("tiny-full.zfs")))
            .unwrap();
    let view = &opened["result"]["views"][0];
    assert_eq!(view["selector"], format!("0x{:016x}", metadata[0].to_guid));
    let expected_time = (metadata[0].creation_time > 0).then_some(metadata[0].creation_time);
    assert_eq!(view["created_at"], json!(expected_time));
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("hello.txt");
    let request = json!({"method":"extract", "name":"hello.txt", "destination":destination});
    let restored = desktop.request(request.clone());
    assert_eq!(restored["ok"], true);
    assert_eq!(
        restored["result"]["sha256"],
        "6cd6c137c938428b8d751cafc33f05092621ba8070554961317bea289f35ca17"
    );
    let before = std::fs::read(&destination).unwrap();
    assert_eq!(desktop.request(request)["ok"], false);
    assert_eq!(std::fs::read(&destination).unwrap(), before);
    assert_eq!(
        desktop.request(json!({"method":"open", "path":fixture("missing.img")}))["ok"],
        false
    );
    assert_eq!(
        desktop.request(json!({"method":"list", "path":"/"}))["result"]["title"],
        "tiny-full.zfs"
    );
    assert_eq!(desktop.request(json!({"method":"extract", "name":"../hello.txt", "destination":output.path().join("escape")}))["ok"], false);
}

#[test]
fn desktop_authenticates_keys_and_forgets_them_when_switching_views() {
    let mut desktop = Desktop::start();
    let opened = desktop.request(json!({"method":"open", "path":fixture("encrypted-raw-s1.zfs")}));
    assert_eq!(opened["result"]["locked"], true);
    let wrong = desktop.request(json!({"method":"unlock", "key":"wrong-secret-should-not-appear"}));
    assert_eq!(wrong["ok"], false);
    assert!(!wrong.to_string().contains("wrong-secret-should-not-appear"));
    let unlocked = desktop.request(json!({"method":"unlock", "key":"zfs-send-fixture-passphrase"}));
    assert_eq!(unlocked["ok"], true);
    assert_eq!(unlocked["result"]["locked"], false);
    assert!(!unlocked.to_string().contains("zfs-send-fixture-passphrase"));
    let out = tempfile::tempdir().unwrap();
    assert_eq!(
        desktop.request(json!({"method":"list", "path":"/docs"}))["ok"],
        true
    );
    assert_eq!(
        desktop.request(
            json!({"method":"extract", "name":"hello.txt", "destination":out.path().join("hello")})
        )["ok"],
        true
    );
    assert_eq!(
        std::fs::read(out.path().join("hello")).unwrap(),
        b"encrypted hello\n"
    );
    assert_eq!(
        desktop.request(json!({"method":"select", "index":0}))["result"]["locked"],
        true
    );
    assert_eq!(
        desktop.request(json!({"method":"close"}))["result"]["title"],
        ""
    );
}

#[test]
fn invalid_device_authorization_preserves_the_existing_backup() {
    let mut desktop = Desktop::start();
    assert_eq!(
        desktop.request(json!({"method":"open", "path":fixture("tiny-full.zfs")}))["ok"],
        true
    );
    assert_eq!(
        desktop.request(json!({"method":"open", "path":"/dev/null", "authorize":true}))["ok"],
        false
    );
    assert_eq!(
        desktop.request(json!({"method":"list", "path":"/"}))["result"]["title"],
        "tiny-full.zfs"
    );
    assert_eq!(
        desktop
            .request(json!({"method":"open", "path":fixture("tiny-full.zfs"), "authorize":true}))["ok"],
        false
    );
    assert_eq!(
        desktop.request(json!({"method":"list", "path":"/"}))["ok"],
        true
    );
}

#[test]
fn unreadable_documents_and_compressed_downloads_preserve_the_open_source() {
    let mut desktop = Desktop::start();
    let original = fixture("tiny-full.zfs");
    assert_eq!(
        desktop.request(json!({"method":"open", "path":original}))["ok"],
        true
    );
    let temporary = tempfile::tempdir().unwrap();
    let document = temporary.path().join("document.txt");
    std::fs::write(&document, "ordinary document\n".repeat(100)).unwrap();
    for (path, title) in [
        (document, "This file isn't a backup ZFS Explore can open"),
        (
            std::path::PathBuf::from(fixture("native-encrypted-pool.img.zst")),
            "Expand this backup first",
        ),
    ] {
        let failed = desktop.request(json!({"method":"open", "path":path}));
        assert_eq!(failed["ok"], false, "{failed}");
        assert_eq!(failed["title"], title, "{failed}");
        let state = desktop.request(json!({"method":"list","path":"/"}));
        assert_eq!(state["result"]["title"], "tiny-full.zfs");
    }
}

#[test]
fn tied_snapshots_open_the_chain_tip_and_can_restore_a_specific_version() {
    let mut desktop = Desktop::start();
    let opened = desktop.request(json!({"method":"open", "path":fixture("multi-snapshot.zfs")}));
    let state = &opened["result"];
    let selected = state["view"].as_u64().unwrap() as usize;
    assert_eq!(state["views"][selected]["snapshot"], "s3");
    assert_eq!(state["recommended_view"], selected);
    assert_eq!(state["choose_dataset"], false);
    let history = desktop.request(json!({"method":"versions", "name":"version.txt"}));
    let rows = history["result"]["versions"].as_array().unwrap();
    let versions: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row["snapshot"].as_str().unwrap(),
                row["size"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(versions, [("s3", 6), ("s2", 32), ("s1", 13)]);
    for row in rows {
        let selected = desktop
            .request(json!({"method":"open_version", "name":"version.txt", "index":row["index"]}));
        assert_eq!(selected["ok"], true);
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("restored");
        assert_eq!(
            desktop
                .request(json!({"method":"extract", "name":"version.txt", "destination":target}))["ok"],
            true
        );
        let expected = match row["snapshot"].as_str().unwrap() {
            "s3" => "three\n",
            "s2" => "snapshot two has a longer value\n",
            _ => "snapshot one\n",
        };
        assert_eq!(std::fs::read_to_string(target).unwrap(), expected);
    }
    let missing = desktop.request(json!({"method":"versions","name":"only-s2.txt"}));
    let rows = missing["result"]["versions"].as_array().unwrap();
    assert_eq!(rows.iter().filter(|r| r["issue"].is_null()).count(), 1);
}

#[test]
fn a_multi_dataset_backup_requires_a_choice_and_keeps_history_in_its_dataset() {
    let directory = tempfile::tempdir().unwrap();
    let archive = directory.path().join("two.zfs");
    let mut bytes = std::fs::read(fixture("tiny-full.zfs")).unwrap();
    bytes.extend(std::fs::read(fixture("multi-snapshot.zfs")).unwrap());
    std::fs::write(&archive, bytes).unwrap();
    let mut desktop = Desktop::start();
    let open = desktop.request(json!({"method":"open","path":archive}));
    let state = &open["result"];
    assert_eq!(state["choose_dataset"], true);
    assert!(state["entries"].as_array().unwrap().is_empty());
    assert!(state["recommended_view"].is_null());
    assert_eq!(
        desktop.request(json!({"method":"list","path":"/"}))["ok"],
        false
    );
    let index = state["views"]
        .as_array()
        .unwrap()
        .iter()
        .position(|v| v["snapshot"] == "s3")
        .unwrap();
    let state = desktop.request(json!({"method":"select","index":index}));
    assert_eq!(state["result"]["choose_dataset"], false);
    let history = desktop.request(json!({"method":"versions","name":"version.txt"}));
    assert_eq!(history["result"]["versions"].as_array().unwrap().len(), 3);
}
