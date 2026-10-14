//! Private line-delimited JSON bridge for the native macOS app.
//! No listener, shell commands, telemetry, or credential persistence. The only
//! program it runs is Apple's `/usr/libexec/authopen`, to obtain a read-only
//! handle to a disk device after the user approves the system prompt.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;
use zfs_send_extract::client::{InceptionCatalog, SourceCatalog, child_path};
use zfs_send_extract::filesystem::DirectoryEntry;
#[cfg(target_os = "macos")]
use zfs_send_extract::source::register_authorized_source;
use zfs_send_extract::source::release_authorized_sources;

#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum Request {
    Open {
        path: String,
        /// Ask the operating system for read access when the device is denied.
        #[serde(default)]
        authorize: bool,
    },
    Select {
        index: usize,
    },
    Unlock {
        key: Option<String>,
        key_file: Option<String>,
    },
    List {
        path: String,
    },
    Enter {
        name: String,
    },
    Back,
    Volume {
        selector: String,
    },
    Extract {
        name: String,
        destination: String,
    },
    Close,
}

struct Layer {
    catalog: InceptionCatalog,
    volume: Option<String>,
    path: String,
}

#[derive(Default)]
struct Session {
    source: Option<SourceCatalog>,
    title: String,
    view: usize,
    path: String,
    key: Option<Zeroizing<Vec<u8>>>,
    layers: Vec<Layer>,
    /// Device path whose read-only handle came from an authorization prompt.
    authorized: Option<PathBuf>,
}

impl Session {
    fn source(&self) -> Result<&SourceCatalog> {
        self.source.as_ref().context("Choose a backup first")
    }

    fn key(&self) -> Option<&[u8]> {
        self.key.as_deref().map(Vec::as_slice)
    }

    fn locked(&self) -> bool {
        self.layers.is_empty()
            && self.key.is_none()
            && self
                .source
                .as_ref()
                .and_then(|s| s.views.get(self.view))
                .is_some_and(|v| v.encrypted)
    }

    fn list(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        if let Some(layer) = self.layers.last() {
            if layer.volume.is_none() {
                return Ok(vec![]);
            }
            layer.catalog.list_directory(layer.volume.as_deref(), path)
        } else {
            self.source()?
                .list_directory_with_key_material(self.view, path, self.key())
        }
    }

    fn current_path(&self) -> &str {
        self.layers.last().map_or(&self.path, |layer| &layer.path)
    }

    fn state(&self) -> Result<Value> {
        let entries = if self.locked() || (self.source.is_none() && self.layers.is_empty()) {
            vec![]
        } else {
            self.list(self.current_path())?
        };
        let mut entries: Vec<Value> = entries
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name, "directory": entry.dirent_type == 4,
                    "regular": entry.dirent_type == 8, "size": entry.logical_size,
                })
            })
            .collect();
        entries.sort_by(|a, b| {
            b["directory"]
                .as_bool()
                .cmp(&a["directory"].as_bool())
                .then_with(|| {
                    a["name"]
                        .as_str()
                        .unwrap()
                        .to_lowercase()
                        .cmp(&b["name"].as_str().unwrap().to_lowercase())
                })
        });
        let views: Vec<Value> = self.source.as_ref().map_or(vec![], |s| {
            s.views
                .iter()
                .map(|v| {
                    json!({
                        "label": v.label, "encrypted": v.encrypted, "key_format": v.key_format,
                        "created_at": v.created_at, "selector": v.selector,
                    })
                })
                .collect()
        });
        let layer = self.layers.last();
        let volumes: Vec<Value> = layer.map_or(vec![], |l| {
            l.catalog.volumes.iter().map(|v| json!({
            "selector": v.selector, "label": v.label(), "supported": v.filesystem.is_some(),
                "bytes": v.length, "filesystem": v.filesystem.map(|f| f.to_string()),
        })).collect()
        });
        Ok(json!({
            "title": self.title, "summary": self.source.as_ref().map(|s| &s.summary),
            "views": views, "view": self.view, "locked": self.locked(),
            "path": self.current_path(), "entries": entries, "volumes": volumes,
            "volume": layer.and_then(|l| l.volume.as_ref()),
            "layers": self.layers.iter().map(|l| &l.catalog.image_path).collect::<Vec<_>>(),
            "can_back": self.layers.len() > usize::from(self.source.is_none()),
        }))
    }

    fn new_layer(catalog: InceptionCatalog) -> Layer {
        let supported: Vec<_> = catalog
            .volumes
            .iter()
            .filter(|v| v.filesystem.is_some())
            .collect();
        // Never silently pick one of several potentially recoverable volumes.
        let volume = (supported.len() == 1).then(|| supported[0].selector.clone());
        Layer {
            catalog,
            volume,
            path: "/".into(),
        }
    }

    fn open_source(source_path: &Path) -> Result<(Self, Value)> {
        let mut candidate = Self {
            path: "/".into(),
            title: source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            ..Self::default()
        };
        match SourceCatalog::open_send(source_path) {
            Ok(source) => candidate.source = Some(source),
            Err(send_error) => match SourceCatalog::open_pool(source_path) {
                Ok(source) => candidate.source = Some(source),
                Err(pool_error) => match InceptionCatalog::open_file(source_path, 0, None) {
                    Ok(image) => candidate.layers.push(Self::new_layer(image)),
                    Err(image_error) => bail!(
                        "Could not open this backup. Choose a ZFS send, an offline single-disk/mirror pool image, or a supported disk image.\n\nZFS stream: {send_error:#}\nZFS pool: {pool_error:#}\nDisk image: {image_error:#}"
                    ),
                },
            },
        }
        // Keep an empty catalog from creating an unusable desktop session.
        if candidate
            .source
            .as_ref()
            .is_some_and(|s| s.views.is_empty())
        {
            bail!("This backup contains no browsable filesystem views");
        }
        let state = candidate.state()?;
        Ok((candidate, state))
    }

    fn handle(&mut self, request: Request) -> Result<Value> {
        match request {
            Request::Open { path, authorize } => {
                let source_path = Path::new(&path);
                if authorize {
                    authorize_device(source_path)?;
                }
                match Self::open_source(source_path) {
                    Ok((mut candidate, state)) => {
                        candidate.authorized = authorize.then(|| source_path.to_owned());
                        release_authorized_sources(candidate.authorized.as_deref());
                        *self = candidate;
                        Ok(state)
                    }
                    Err(error) => {
                        // A failed open leaves the current session and its handle in place.
                        release_authorized_sources(self.authorized.as_deref());
                        Err(error)
                    }
                }
            }
            Request::Select { index } => {
                self.source()?.view(index)?;
                // Switching views always forgets the previous dataset's key and nested readers.
                let old_view = self.view;
                let old_path = std::mem::replace(&mut self.path, "/".into());
                let old_key = self.key.take();
                let old_layers = std::mem::take(&mut self.layers);
                self.view = index;
                match self.state() {
                    Ok(state) => Ok(state),
                    Err(e) => {
                        self.view = old_view;
                        self.path = old_path;
                        self.key = old_key;
                        self.layers = old_layers;
                        Err(e)
                    }
                }
            }
            Request::Unlock { key, key_file } => {
                let material = if let Some(key) = key {
                    Zeroizing::new(key.into_bytes())
                } else if let Some(path) = key_file {
                    let file = std::fs::File::open(path).context("Opening key file")?;
                    if file.metadata()?.len() > 4096 {
                        bail!("Key files must be at most 4096 bytes");
                    }
                    let mut material = Zeroizing::new(Vec::new());
                    std::io::Read::read_to_end(
                        &mut std::io::Read::take(file, 4097),
                        &mut material,
                    )?;
                    if material.len() > 4096 {
                        bail!("Key file grew beyond 4096 bytes");
                    }
                    material
                } else {
                    bail!("Enter a key or choose a key file");
                };
                // Authenticate before retaining the supplied key.
                self.source()?
                    .list_directory_with_key_material(self.view, "/", Some(&material))?;
                self.key = Some(material);
                self.path = "/".into();
                self.state()
            }
            Request::List { path } => {
                self.list(&path)?;
                if let Some(layer) = self.layers.last_mut() {
                    layer.path = path;
                } else {
                    self.path = path;
                }
                self.state()
            }
            Request::Enter { name } => {
                if self.layers.len() >= 8 {
                    bail!("Up to eight nested disk images can be open at once");
                }
                let path = child_path(self.current_path(), &name)?;
                let catalog = if let Some(layer) = self.layers.last() {
                    layer
                        .catalog
                        .inspect_child(layer.volume.as_deref(), &path, 0, None)?
                } else {
                    self.source()?.inspect_inception_with_key_material(
                        self.view,
                        &path,
                        self.key(),
                        None,
                        None,
                        0,
                        None,
                    )?
                };
                self.layers.push(Self::new_layer(catalog));
                match self.state() {
                    Ok(state) => Ok(state),
                    Err(e) => {
                        self.layers.pop();
                        Err(e)
                    }
                }
            }
            Request::Back => {
                if self.layers.len() <= usize::from(self.source.is_none()) {
                    bail!("Already at the source");
                }
                let previous = self.layers.pop().unwrap();
                match self.state() {
                    Ok(state) => Ok(state),
                    Err(e) => {
                        self.layers.push(previous);
                        Err(e)
                    }
                }
            }
            Request::Volume { selector } => {
                let layer = self.layers.last_mut().context("Open a disk image first")?;
                if !layer
                    .catalog
                    .volumes
                    .iter()
                    .any(|v| v.selector == selector && v.filesystem.is_some())
                {
                    bail!("Choose a supported filesystem volume");
                }
                layer.catalog.list_directory(Some(&selector), "/")?;
                layer.volume = Some(selector);
                layer.path = "/".into();
                self.state()
            }
            Request::Extract { name, destination } => {
                let entry = self
                    .list(self.current_path())?
                    .into_iter()
                    .find(|e| e.name == name)
                    .context("Select an existing file or folder first")?;
                let path = child_path(self.current_path(), &name)?;
                let destination = Path::new(&destination);
                // Never overwrite from the desktop bridge, even after a Save dialog replacement prompt.
                if std::fs::symlink_metadata(destination).is_ok() {
                    bail!(
                        "A file or folder already exists there. Choose a new name or destination; your existing files have been kept."
                    );
                }
                if entry.dirent_type == 4 {
                    let result = if let Some(layer) = self.layers.last() {
                        layer.catalog.extract_tree(
                            layer.volume.as_deref(),
                            &path,
                            destination,
                            false,
                        )?
                    } else {
                        self.source()?.extract_tree_with_key_material(
                            self.view,
                            &path,
                            destination,
                            false,
                            self.key(),
                        )?
                    };
                    Ok(
                        json!({"restored": destination, "bytes": result.logical_bytes, "files": result.files,
                        "skipped": result.skipped_entries}),
                    )
                } else if entry.dirent_type == 8 {
                    let result = if let Some(layer) = self.layers.last() {
                        layer
                            .catalog
                            .extract(layer.volume.as_deref(), &path, destination, false)?
                    } else {
                        self.source()?.extract_with_key_material(
                            self.view,
                            &path,
                            destination,
                            false,
                            self.key(),
                        )?
                    };
                    Ok(
                        json!({"restored": destination, "bytes": result.logical_size, "sha256": result.sha256, "files": 1, "skipped": 0}),
                    )
                } else {
                    bail!(
                        "Only regular files and folders can be restored; links and special files are not followed"
                    );
                }
            }
            Request::Close => {
                *self = Self::default();
                release_authorized_sources(None);
                self.state()
            }
        }
    }
}

/// Ask macOS for a read-only handle to a disk device the user cannot open,
/// through the standard administrator prompt. `authopen` passes the open
/// descriptor back over a socket; no privileged process outlives the call and
/// the device's permissions are not changed.
#[cfg(target_os = "macos")]
fn authorize_device(path: &Path) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};

    if !is_disk_device_path(path) {
        bail!("Choose a disk device such as /dev/disk4 or /dev/rdisk4s2 for authorization");
    }
    let before =
        std::fs::symlink_metadata(path).context("The selected disk is no longer attached")?;
    if !(before.file_type().is_block_device() || before.file_type().is_char_device()) {
        bail!("The selected path is not a disk device");
    }
    match std::fs::File::open(path) {
        Ok(file) => return register_authorized_source(path, file),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(error).context("Opening the selected disk read-only"),
    }
    let (receiver, sender) = UnixStream::pair().context("Creating the authorization socket")?;
    let mut child = Command::new("/usr/libexec/authopen")
        .arg("-stdoutpipe")
        .arg("-o")
        .arg(libc::O_RDONLY.to_string())
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(sender)))
        .stderr(Stdio::null())
        .spawn()
        .context("Starting the macOS authorization prompt")?;

    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    };
    // Aligned space for one SCM_RIGHTS message carrying a single descriptor.
    let mut control = [0u64; 4];
    // SAFETY: msghdr is plain data; every pointer refers to a live local buffer.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as libc::socklen_t;
    // SAFETY: recvmsg writes only within the buffers described above. The
    // sender's end was moved into the child, so this returns 0 if it exits.
    let received = loop {
        let received = unsafe { libc::recvmsg(receiver.as_raw_fd(), &mut message, 0) };
        if received >= 0
            || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            break received;
        }
    };
    if received < 0 {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .context("Waiting for the authorization prompt")?;

    // SAFETY: CMSG_* walk the control buffer filled by recvmsg above.
    let descriptor = unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if received > 0
            && !header.is_null()
            && (*header).cmsg_level == libc::SOL_SOCKET
            && (*header).cmsg_type == libc::SCM_RIGHTS
            && (*header).cmsg_len == libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32)
        {
            Some(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                libc::CMSG_DATA(header).cast::<libc::c_int>(),
            )))
        } else {
            None
        }
    };
    let Some(descriptor) = descriptor else {
        bail!(
            "Read access to {} was not granted ({status}). An administrator must approve the macOS prompt.",
            path.display()
        );
    };
    let file = std::fs::File::from(descriptor);
    if !status.success() || message.msg_flags & libc::MSG_CTRUNC != 0 {
        bail!("Read access was not granted. Choose the drive again to retry.");
    }
    let after = file.metadata()?;
    if (before.dev(), before.ino(), before.rdev()) != (after.dev(), after.ino(), after.rdev()) {
        bail!("The disk changed during authorization. Choose the drive again.");
    }
    register_authorized_source(path, file)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_disk_device_path(path: &Path) -> bool {
    if path.parent() != Some(Path::new("/dev")) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(numbers) = name
        .strip_prefix("disk")
        .or_else(|| name.strip_prefix("rdisk"))
    else {
        return false;
    };
    numbers
        .split('s')
        .all(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(all(test, target_os = "macos"))]
mod authorization_tests {
    #[test]
    fn authorization_accepts_only_exact_disk_nodes() {
        for path in ["/dev/disk4", "/dev/rdisk4s2", "/dev/disk1s2s3"] {
            assert!(
                super::is_disk_device_path(std::path::Path::new(path)),
                "{path}"
            );
        }
        for path in [
            "/dev/null",
            "/dev/disk",
            "/dev/disk4/../null",
            "/dev/disk4s",
            "/tmp/disk4",
            "/dev/disk-1",
        ] {
            assert!(
                !super::is_disk_device_path(std::path::Path::new(path)),
                "{path}"
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn authorize_device(_path: &Path) -> Result<()> {
    bail!("Device authorization is only available on macOS")
}

fn main() -> Result<()> {
    let mut session = Session::default();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        // Bound a request, including an accidentally pasted oversized key.
        let mut line = Zeroizing::new(Vec::new());
        let length = (&mut input)
            .take(64 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if length == 0 {
            break;
        }
        if length > 64 * 1024 {
            bail!("Desktop request exceeds 64 KiB");
        }
        let result = serde_json::from_slice::<Request>(&line)
            .context("Invalid desktop request")
            .and_then(|request| session.handle(request));
        let response = match result {
            Ok(value) => json!({"ok": true, "result": value}),
            // Do not echo the original JSON request (it may contain credentials).
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
