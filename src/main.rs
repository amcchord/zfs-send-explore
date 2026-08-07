use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use zeroize::Zeroizing;
use zfs_send_extract::operations;

#[derive(Debug, Parser)]
#[command(name = "zfs-send-extract")]
#[command(about = "Browse and extract files from ZFS send streams without ZFS")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a stream and show its header and replay-record counts.
    Inspect {
        /// Full or incremental ZFS send stream.
        stream: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List snapshots contained in a send file.
    Snapshots {
        /// ZFS send file, including compound or concatenated streams.
        stream: PathBuf,
    },
    /// List one directory from a snapshot in a send file.
    List {
        /// ZFS send file.
        stream: PathBuf,
        /// Absolute path inside the snapshot.
        #[arg(default_value = "/")]
        path: String,
        /// Snapshot name, full dataset@snapshot name, or GUID.
        #[arg(long)]
        snapshot: Option<String>,
        /// File containing the ZFS passphrase, hex key, or 32-byte raw key.
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// Extract one regular file from a snapshot and write update metadata beside it.
    Extract {
        /// ZFS send file.
        stream: PathBuf,
        /// Absolute path inside the snapshot.
        path: String,
        /// Destination file.
        #[arg(short, long)]
        output: PathBuf,
        /// Replace an existing destination.
        #[arg(long)]
        force: bool,
        /// Snapshot name, full dataset@snapshot name, or GUID.
        #[arg(long)]
        snapshot: Option<String>,
        /// File containing the ZFS passphrase, hex key, or 32-byte raw key.
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// Atomically update a previously extracted file from an incremental send.
    Apply {
        /// Incremental ZFS send stream.
        stream: PathBuf,
        /// File previously created by `extract`.
        target: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect { stream, json } => {
            let inspection = operations::inspect_stream(&stream)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                println!("snapshots: {}", inspection.snapshots.len());
                for snapshot in &inspection.snapshots {
                    let mode = if snapshot.features & zfs_send_extract::stream::FEATURE_RAW != 0 {
                        "raw encrypted"
                    } else {
                        "plain"
                    };
                    println!(
                        "snapshot: {} ({mode}, to 0x{:016x}, from 0x{:016x})",
                        snapshot.dataset_name, snapshot.to_guid, snapshot.from_guid,
                    );
                }
                println!("stream bytes: {}", inspection.stream_bytes);
                for (name, count) in inspection.records {
                    println!("{name}: {count}");
                }
            }
        }
        Command::Snapshots { stream } => {
            for snapshot in operations::snapshots(&stream)? {
                let kind = if snapshot.from_guid == 0 {
                    "full"
                } else {
                    "incremental"
                };
                let mode = if snapshot.features & zfs_send_extract::stream::FEATURE_RAW != 0 {
                    "raw"
                } else {
                    "plain"
                };
                println!(
                    "{kind}\t{mode}\t0x{:016x}\t0x{:016x}\t{}",
                    snapshot.to_guid, snapshot.from_guid, snapshot.dataset_name
                );
            }
        }
        Command::List {
            stream,
            path,
            snapshot,
            key_file,
        } => {
            let key = load_key_material(&stream, snapshot.as_deref(), key_file.as_ref())?;
            for entry in operations::list_directory_snapshot_with_key(
                &stream,
                &path,
                snapshot.as_deref(),
                key.as_deref().map(Vec::as_slice),
            )? {
                let kind = match entry.dirent_type {
                    4 => 'd',
                    8 => 'f',
                    10 => 'l',
                    _ => '?',
                };
                let size = entry
                    .logical_size
                    .map_or_else(|| "-".into(), |value| value.to_string());
                println!("{kind}\t{size}\t{}\t{}", entry.object_id, entry.name);
            }
        }
        Command::Extract {
            stream,
            path,
            output,
            force,
            snapshot,
            key_file,
        } => {
            let key = load_key_material(&stream, snapshot.as_deref(), key_file.as_ref())?;
            let sidecar = operations::extract_snapshot_with_key(
                &stream,
                &path,
                &output,
                force,
                snapshot.as_deref(),
                key.as_deref().map(Vec::as_slice),
            )?;
            println!(
                "extracted {} bytes from object {} to {} (sha256 {})",
                sidecar.logical_size,
                sidecar.object_id,
                output.display(),
                sidecar.sha256
            );
        }
        Command::Apply { stream, target } => {
            let sidecar = operations::apply_incremental(&stream, &target)?;
            println!(
                "updated {} to {} bytes at {} (sha256 {})",
                sidecar.path, sidecar.logical_size, sidecar.snapshot_guid, sidecar.sha256
            );
        }
    }
    Ok(())
}

fn load_key_material(
    stream: &std::path::Path,
    snapshot: Option<&str>,
    key_file: Option<&PathBuf>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(requirement) = operations::encryption_requirement(stream, snapshot)? else {
        if key_file.is_some() {
            anyhow::bail!("--key-file is only valid for a raw encrypted send");
        }
        return Ok(None);
    };

    if let Some(path) = key_file {
        let maximum_size: u64 = match requirement.key_format.as_str() {
            "raw" => 32,
            "hex" => 65,
            "passphrase" => 513,
            _ => unreachable!("encryption requirement validates the key format"),
        };
        let mut file = std::fs::File::open(path)
            .map_err(|error| anyhow::anyhow!("reading ZFS key file {}: {error}", path.display()))?
            .take(maximum_size + 1);
        let mut material = Vec::new();
        file.read_to_end(&mut material)
            .map_err(|error| anyhow::anyhow!("reading ZFS key file {}: {error}", path.display()))?;
        if material.len() as u64 > maximum_size {
            anyhow::bail!(
                "ZFS {} key file {} is too large",
                requirement.key_format,
                path.display()
            );
        }
        if requirement.key_format != "raw" && material.last() == Some(&b'\n') {
            material.pop();
        }
        return Ok(Some(Zeroizing::new(material)));
    }

    if requirement.key_format == "raw" {
        anyhow::bail!("dataset uses a binary raw key; provide its 32-byte file with --key-file");
    }
    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "encrypted send requires --key-file when standard input is not an interactive terminal"
        );
    }
    let prompt = format!(
        "ZFS {} for {}: ",
        requirement.key_format, requirement.dataset_name
    );
    let material = rpassword::prompt_password(prompt)?;
    Ok(Some(Zeroizing::new(material.into_bytes())))
}
