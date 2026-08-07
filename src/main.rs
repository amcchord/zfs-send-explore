use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use zeroize::Zeroizing;
use zfs_send_extract::{operations, pool::PoolMember};

#[derive(Debug, Parser)]
#[command(name = "zfs-send-extract")]
#[command(about = "Browse ZFS send streams and offline pool members without ZFS")]
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
        /// File containing the ZFS passphrase, hex key, or 32-byte raw key.
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// Browse an offline ZFS vdev member or image directly.
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PoolCommand {
    /// Validate a member and summarize its active pool state.
    Inspect {
        /// Exact ZFS vdev partition, file vdev, or image (opened read-only).
        member: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List filesystem datasets reachable from the member.
    Datasets {
        /// Exact ZFS vdev partition, file vdev, or image.
        member: PathBuf,
    },
    /// List named snapshots stored in the pool.
    Snapshots {
        /// Exact ZFS vdev partition, file vdev, or image.
        member: PathBuf,
        /// Restrict output to one full dataset name.
        dataset: Option<String>,
    },
    /// List one directory from a current dataset or named snapshot.
    List {
        /// Exact ZFS vdev partition, file vdev, or image.
        member: PathBuf,
        /// Full dataset name, optionally followed by @snapshot.
        dataset: String,
        /// Absolute path inside the dataset or snapshot.
        #[arg(default_value = "/")]
        path: String,
    },
    /// Extract one regular file directly from a dataset or snapshot.
    Extract {
        /// Exact ZFS vdev partition, file vdev, or image.
        member: PathBuf,
        /// Full dataset name, optionally followed by @snapshot.
        dataset: String,
        /// Absolute path inside the dataset or snapshot.
        path: String,
        /// Destination file.
        #[arg(short, long)]
        output: PathBuf,
        /// Replace an existing destination.
        #[arg(long)]
        force: bool,
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
        Command::Apply {
            stream,
            target,
            key_file,
        } => {
            let key = load_apply_key_material(&stream, key_file.as_ref())?;
            let sidecar = operations::apply_incremental_with_key(
                &stream,
                &target,
                key.as_deref().map(Vec::as_slice),
            )?;
            println!(
                "updated {} to {} bytes at {} (sha256 {})",
                sidecar.path, sidecar.logical_size, sidecar.snapshot_guid, sidecar.sha256
            );
        }
        Command::Pool { command } => run_pool(command)?,
    }
    Ok(())
}

fn run_pool(command: PoolCommand) -> Result<()> {
    match command {
        PoolCommand::Inspect { member, json } => {
            let pool = PoolMember::open(&member)?;
            let inspection = pool.inspect()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&inspection)?);
            } else {
                println!("pool: {}", inspection.pool_name);
                println!("pool guid: {}", inspection.pool_guid);
                println!(
                    "vdev: {} ({}, {} top-level)",
                    inspection.vdev_guid, inspection.vdev_type, inspection.top_level_vdevs
                );
                println!("member bytes: {}", inspection.source_bytes);
                println!("active txg: {}", inspection.txg);
                println!("byte order: {}", inspection.endian);
                println!("datasets: {}", inspection.datasets);
                println!("snapshots: {}", inspection.snapshots);
            }
        }
        PoolCommand::Datasets { member } => {
            let pool = PoolMember::open(&member)?;
            for dataset in pool.datasets()? {
                println!(
                    "{}\t{}\t{}",
                    dataset.head_guid, dataset.head_dataset_object, dataset.name
                );
            }
        }
        PoolCommand::Snapshots { member, dataset } => {
            let pool = PoolMember::open(&member)?;
            for snapshot in pool.snapshots(dataset.as_deref())? {
                println!(
                    "{}\t{}\t{}\t{}",
                    snapshot.guid,
                    snapshot.creation_txg,
                    snapshot.creation_time,
                    snapshot.full_name
                );
            }
        }
        PoolCommand::List {
            member,
            dataset,
            path,
        } => {
            let pool = PoolMember::open(&member)?;
            for entry in pool.list_directory(&dataset, &path)? {
                let kind = match entry.dirent_type {
                    4 => 'd',
                    8 => 'f',
                    10 => 'l',
                    _ => '?',
                };
                let size = entry
                    .logical_size
                    .map_or_else(|| "-".to_owned(), |value| value.to_string());
                println!("{kind}\t{size}\t{}\t{}", entry.object_id, entry.name);
            }
        }
        PoolCommand::Extract {
            member,
            dataset,
            path,
            output,
            force,
        } => {
            let pool = PoolMember::open(&member)?;
            let extraction = pool.extract(&dataset, &path, &output, force)?;
            println!(
                "extracted {} bytes from object {} to {} (sha256 {})",
                extraction.logical_size,
                extraction.object_id,
                output.display(),
                extraction.sha256
            );
            if !extraction.sidecar_written {
                println!(
                    "note: current-head extraction has no incremental-send sidecar; select a named snapshot to create one"
                );
            }
        }
    }
    Ok(())
}

fn load_key_material(
    stream: &std::path::Path,
    snapshot: Option<&str>,
    key_file: Option<&PathBuf>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let requirement = operations::encryption_requirement(stream, snapshot)?;
    load_key_for_requirement(requirement, key_file)
}

fn load_apply_key_material(
    stream: &std::path::Path,
    key_file: Option<&PathBuf>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let requirement = operations::apply_encryption_requirement(stream)?;
    load_key_for_requirement(requirement, key_file)
}

fn load_key_for_requirement(
    requirement: Option<operations::EncryptionRequirement>,
    key_file: Option<&PathBuf>,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let Some(requirement) = requirement else {
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
