use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use zeroize::Zeroizing;
use zfs_send_extract::{inception::InceptionSession, operations, pool::PoolMember};

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
    /// Explore a disk image stored as a regular file in a send-stream snapshot.
    Inception {
        #[command(subcommand)]
        command: InceptionCommand,
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
        /// ZFS vdev partition, file vdev, or supported GPT whole disk/image (read-only).
        member: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List filesystem datasets reachable from the member.
    Datasets {
        /// ZFS vdev partition, file vdev, or supported GPT whole disk/image.
        member: PathBuf,
    },
    /// List named snapshots stored in the pool.
    Snapshots {
        /// ZFS vdev partition, file vdev, or supported GPT whole disk/image.
        member: PathBuf,
        /// Restrict output to one full dataset name.
        dataset: Option<String>,
    },
    /// List one directory from a current dataset or named snapshot.
    List {
        /// ZFS vdev partition, file vdev, or supported GPT whole disk/image.
        member: PathBuf,
        /// Full dataset name, optionally followed by @snapshot.
        dataset: String,
        /// Absolute path inside the dataset or snapshot.
        #[arg(default_value = "/")]
        path: String,
    },
    /// Extract one regular file directly from a dataset or snapshot.
    Extract {
        /// ZFS vdev partition, file vdev, or supported GPT whole disk/image.
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
    /// Explore a disk image stored as a regular file in a dataset or snapshot.
    Inception {
        #[command(subcommand)]
        command: PoolInceptionCommand,
    },
}

#[derive(Debug, Clone, Args)]
struct ImageWindow {
    /// Byte offset within the ZFS file where the disk container starts.
    #[arg(
        long,
        default_value_t = 0,
        value_name = "BYTES",
        value_parser = parse_byte_argument
    )]
    image_offset: u64,
    /// Limit the disk container to this many bytes after --image-offset.
    #[arg(long, value_name = "BYTES", value_parser = parse_byte_argument)]
    image_length: Option<u64>,
}

#[derive(Debug, Subcommand)]
enum InceptionCommand {
    /// Detect the disk container, partition table, and inner filesystems.
    Inspect {
        stream: PathBuf,
        /// Absolute path of the disk image inside the ZFS snapshot.
        image: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
        #[command(flatten)]
        window: ImageWindow,
        #[arg(long)]
        json: bool,
    },
    /// List a directory in a filesystem inside the selected disk image.
    List {
        stream: PathBuf,
        image: String,
        /// Absolute path inside the subordinate filesystem.
        #[arg(default_value = "/")]
        path: String,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
        /// Volume selector reported by `inception inspect` (for example gpt2).
        #[arg(long)]
        volume: Option<String>,
        #[command(flatten)]
        window: ImageWindow,
    },
    /// Extract one file from a filesystem inside the selected disk image.
    Extract {
        stream: PathBuf,
        image: String,
        /// Absolute path inside the subordinate filesystem.
        path: String,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
        #[arg(long)]
        volume: Option<String>,
        #[command(flatten)]
        window: ImageWindow,
    },
}

#[derive(Debug, Subcommand)]
enum PoolInceptionCommand {
    /// Detect the disk container, partition table, and inner filesystems.
    Inspect {
        member: PathBuf,
        dataset: String,
        image: String,
        #[command(flatten)]
        window: ImageWindow,
        #[arg(long)]
        json: bool,
    },
    /// List a directory in a filesystem inside the selected disk image.
    List {
        member: PathBuf,
        dataset: String,
        image: String,
        #[arg(default_value = "/")]
        path: String,
        #[arg(long)]
        volume: Option<String>,
        #[command(flatten)]
        window: ImageWindow,
    },
    /// Extract one file from a filesystem inside the selected disk image.
    Extract {
        member: PathBuf,
        dataset: String,
        image: String,
        path: String,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        volume: Option<String>,
        #[command(flatten)]
        window: ImageWindow,
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
        Command::Inception { command } => run_inception(command)?,
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
        PoolCommand::Inception { command } => run_pool_inception(command)?,
    }
    Ok(())
}

fn run_inception(command: InceptionCommand) -> Result<()> {
    match command {
        InceptionCommand::Inspect {
            stream,
            image,
            snapshot,
            key_file,
            window,
            json,
        } => {
            let key = load_key_material(&stream, snapshot.as_deref(), key_file.as_ref())?;
            let session = InceptionSession::from_send_at(
                &stream,
                snapshot.as_deref(),
                &image,
                key.as_deref().map(Vec::as_slice),
                window.image_offset,
                window.image_length,
            )?;
            print_inception_inspection(&session, json)?;
        }
        InceptionCommand::List {
            stream,
            image,
            path,
            snapshot,
            key_file,
            volume,
            window,
        } => {
            let key = load_key_material(&stream, snapshot.as_deref(), key_file.as_ref())?;
            let session = InceptionSession::from_send_at(
                &stream,
                snapshot.as_deref(),
                &image,
                key.as_deref().map(Vec::as_slice),
                window.image_offset,
                window.image_length,
            )?;
            print_directory(session.list_directory(volume.as_deref(), &path)?);
        }
        InceptionCommand::Extract {
            stream,
            image,
            path,
            output,
            force,
            snapshot,
            key_file,
            volume,
            window,
        } => {
            let key = load_key_material(&stream, snapshot.as_deref(), key_file.as_ref())?;
            let session = InceptionSession::from_send_at(
                &stream,
                snapshot.as_deref(),
                &image,
                key.as_deref().map(Vec::as_slice),
                window.image_offset,
                window.image_length,
            )?;
            let extraction = session.extract(volume.as_deref(), &path, &output, force)?;
            println!(
                "extracted {} bytes from {} volume {} to {} (sha256 {})",
                extraction.logical_size,
                extraction.filesystem,
                extraction.volume,
                output.display(),
                extraction.sha256
            );
        }
    }
    Ok(())
}

fn run_pool_inception(command: PoolInceptionCommand) -> Result<()> {
    match command {
        PoolInceptionCommand::Inspect {
            member,
            dataset,
            image,
            window,
            json,
        } => {
            let session = InceptionSession::from_pool_at(
                &member,
                &dataset,
                &image,
                window.image_offset,
                window.image_length,
            )?;
            print_inception_inspection(&session, json)?;
        }
        PoolInceptionCommand::List {
            member,
            dataset,
            image,
            path,
            volume,
            window,
        } => {
            let session = InceptionSession::from_pool_at(
                &member,
                &dataset,
                &image,
                window.image_offset,
                window.image_length,
            )?;
            print_directory(session.list_directory(volume.as_deref(), &path)?);
        }
        PoolInceptionCommand::Extract {
            member,
            dataset,
            image,
            path,
            output,
            force,
            volume,
            window,
        } => {
            let session = InceptionSession::from_pool_at(
                &member,
                &dataset,
                &image,
                window.image_offset,
                window.image_length,
            )?;
            let extraction = session.extract(volume.as_deref(), &path, &output, force)?;
            println!(
                "extracted {} bytes from {} volume {} to {} (sha256 {})",
                extraction.logical_size,
                extraction.filesystem,
                extraction.volume,
                output.display(),
                extraction.sha256
            );
        }
    }
    Ok(())
}

fn print_directory(entries: Vec<zfs_send_extract::filesystem::DirectoryEntry>) {
    for entry in entries {
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

fn print_inception_inspection(session: &InceptionSession, json: bool) -> Result<()> {
    if json {
        let output = serde_json::json!({
            "image_path": session.image_path(),
            "image_offset": session.image_offset(),
            "stored_bytes": session.stored_size(),
            "container": session.container(),
            "virtual_disk_bytes": session.image_size(),
            "volumes": session.volumes(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    println!("image: {}", session.image_path());
    println!("image offset: {}", session.image_offset());
    println!("stored bytes: {}", session.stored_size());
    println!("container: {}", session.container());
    println!("virtual disk bytes: {}", session.image_size());
    println!("volumes: {}", session.volumes().len());
    for volume in session.volumes() {
        let filesystem = volume
            .filesystem
            .map_or_else(|| "unknown".to_owned(), |kind| kind.to_string());
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            volume.selector,
            filesystem,
            volume.offset,
            volume.length,
            volume.partition_type,
            volume.name
        );
        if let Some(diagnostic) = &volume.diagnostic {
            println!("  note: {diagnostic}");
        }
    }
    Ok(())
}

fn parse_byte_argument(value: &str) -> std::result::Result<u64, String> {
    let compact = value.trim().replace('_', "");
    if compact.is_empty() {
        return Err("byte count cannot be empty".to_owned());
    }
    let parsed = if let Some(hex) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        compact.parse()
    };
    parsed.map_err(|_| format!("{value:?} is not a byte count (use decimal or 0x hexadecimal)"))
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

#[cfg(test)]
mod tests {
    use super::{
        Cli, Command, InceptionCommand, PoolCommand, PoolInceptionCommand, parse_byte_argument,
    };
    use clap::Parser;

    #[test]
    fn image_windows_accept_decimal_hex_and_grouping() {
        assert_eq!(parse_byte_argument("4096").unwrap(), 4096);
        assert_eq!(parse_byte_argument("0x1000").unwrap(), 4096);
        assert_eq!(parse_byte_argument("1_048_576").unwrap(), 1_048_576);
        assert!(parse_byte_argument("4 MiB").is_err());
    }

    #[test]
    fn cli_exposes_every_inception_operation_and_image_window() {
        let inspect = Cli::try_parse_from([
            "zfse",
            "inception",
            "inspect",
            "backup.zfs",
            "/vms/disk.qcow2",
            "--snapshot",
            "nightly",
            "--image-offset",
            "0x1000",
            "--image-length",
            "1_048_576",
            "--json",
        ])
        .unwrap();
        let Command::Inception {
            command:
                InceptionCommand::Inspect {
                    snapshot,
                    window,
                    json,
                    ..
                },
        } = inspect.command
        else {
            panic!("wrong inception inspect command")
        };
        assert_eq!(snapshot.as_deref(), Some("nightly"));
        assert_eq!(window.image_offset, 4096);
        assert_eq!(window.image_length, Some(1_048_576));
        assert!(json);

        let list = Cli::try_parse_from([
            "zfse",
            "inception",
            "list",
            "backup.zfs",
            "/vms/disk.vmdk",
            "/Windows/System32",
            "--volume",
            "gpt2",
        ])
        .unwrap();
        let Command::Inception {
            command: InceptionCommand::List { path, volume, .. },
        } = list.command
        else {
            panic!("wrong inception list command")
        };
        assert_eq!(path, "/Windows/System32");
        assert_eq!(volume.as_deref(), Some("gpt2"));

        let extract = Cli::try_parse_from([
            "zfse",
            "inception",
            "extract",
            "backup.zfs",
            "/vms/disk.raw",
            "/etc/hostname",
            "--output",
            "hostname",
            "--force",
        ])
        .unwrap();
        let Command::Inception {
            command: InceptionCommand::Extract { path, force, .. },
        } = extract.command
        else {
            panic!("wrong inception extract command")
        };
        assert_eq!(path, "/etc/hostname");
        assert!(force);

        let pool = Cli::try_parse_from([
            "zfse",
            "pool",
            "inception",
            "extract",
            "member.img",
            "tank/vms@nightly",
            "/disk.qcow2",
            "/Users/example/report.docx",
            "--output",
            "report.docx",
            "--volume",
            "mbr1",
            "--image-offset",
            "4096",
        ])
        .unwrap();
        let Command::Pool {
            command:
                PoolCommand::Inception {
                    command:
                        PoolInceptionCommand::Extract {
                            dataset,
                            volume,
                            window,
                            ..
                        },
                },
        } = pool.command
        else {
            panic!("wrong pool inception extract command")
        };
        assert_eq!(dataset, "tank/vms@nightly");
        assert_eq!(volume.as_deref(), Some("mbr1"));
        assert_eq!(window.image_offset, 4096);
    }
}
