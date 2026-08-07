use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use zfs_send_extract::operations;

#[derive(Debug, Parser)]
#[command(name = "zfs-send-extract")]
#[command(about = "Browse and extract files from plain ZFS send streams without ZFS")]
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
    /// List one directory from a full send stream.
    List {
        /// Full ZFS send stream.
        stream: PathBuf,
        /// Absolute path inside the snapshot.
        #[arg(default_value = "/")]
        path: String,
    },
    /// Extract one regular file and write update metadata beside it.
    Extract {
        /// Full ZFS send stream.
        stream: PathBuf,
        /// Absolute path inside the snapshot.
        path: String,
        /// Destination file.
        #[arg(short, long)]
        output: PathBuf,
        /// Replace an existing destination.
        #[arg(long)]
        force: bool,
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
                println!("dataset: {}", inspection.begin.dataset_name);
                println!("to GUID: 0x{:016x}", inspection.begin.to_guid);
                println!("from GUID: 0x{:016x}", inspection.begin.from_guid);
                println!("features: 0x{:014x}", inspection.begin.features);
                println!("stream bytes: {}", inspection.stream_bytes);
                for (name, count) in inspection.records {
                    println!("{name}: {count}");
                }
            }
        }
        Command::List { stream, path } => {
            for entry in operations::list_directory(&stream, &path)? {
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
        } => {
            let sidecar = operations::extract(&stream, &path, &output, force)?;
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
