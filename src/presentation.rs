//! Plain-language wording shared by the desktop clients.
//!
//! Technical names (dataset paths, GUIDs, partition selectors) remain available
//! as secondary detail. Nothing here changes what is read or which item is
//! selected; it only describes the choices in words a first-time user knows.

use crate::client::{SourceCatalog, SourceKind, SourceView, ViewKind};
use crate::inception::{FilesystemKind, VolumeInfo};
use crate::snapshot_time::SnapshotTimeZone;

/// Primary text for a restore point: its date, or what it is when undated.
pub fn view_title(view: &SourceView, zone: SnapshotTimeZone) -> String {
    if let Some(time) = view.created_at.and_then(|time| zone.format_short(time)) {
        return time;
    }
    match view.kind {
        ViewKind::Current => "Current files".to_owned(),
        _ => view
            .snapshot
            .clone()
            .unwrap_or_else(|| short_name(&view.dataset).to_owned()),
    }
}

/// Secondary text for a restore point: which backup it belongs to and how it
/// was captured, without ZFS vocabulary.
pub fn view_detail(view: &SourceView) -> String {
    let name = short_name(&view.dataset);
    match view.kind {
        _ if view.parent => format!("{name} · holds other backups"),
        ViewKind::Snapshot => format!(
            "{name} · {}",
            view.snapshot.as_deref().unwrap_or("snapshot")
        ),
        ViewKind::Current => format!("{name} · live files, not a snapshot"),
        ViewKind::FullSend => format!("{name} · full copy"),
        ViewKind::IncrementalSend => format!("{name} · changes only"),
    }
}

/// One-line label for list controls that cannot show two lines or icons.
pub fn view_line(view: &SourceView, zone: SnapshotTimeZone) -> String {
    let lock = if view.encrypted {
        "  ·  encrypted"
    } else {
        ""
    };
    format!("{}  —  {}{lock}", view_title(view, zone), view_detail(view))
}

/// What kind of backup is open and how many restore points it offers, e.g.
/// "Slide backup · 12 restore points".
pub fn source_description(catalog: &SourceCatalog) -> String {
    let kind = match catalog.kind {
        SourceKind::SendStream => "ZFS send file",
        SourceKind::PoolMember if catalog.title.starts_with("Slide Box") => "Slide backup",
        SourceKind::PoolMember if catalog.title.starts_with("Datto") => "Datto backup drive",
        SourceKind::PoolMember => "ZFS pool",
    };
    let points = catalog
        .views
        .iter()
        .filter(|view| view.kind != ViewKind::Current && !view.parent)
        .count();
    match points {
        0 => kind.to_owned(),
        1 => format!("{kind} · 1 restore point"),
        count => format!("{kind} · {count} restore points"),
    }
}

/// The final component of a dataset name (`pool/agents/linux` → `linux`).
pub fn short_name(dataset: &str) -> &str {
    dataset.rsplit('/').next().unwrap_or(dataset)
}

/// Index of the restore point a first-time user most likely wants: the newest
/// dated snapshot of a dataset that holds files, else the first view.
pub fn recommended_view(views: &[SourceView]) -> Option<usize> {
    views
        .iter()
        .enumerate()
        .filter(|(_, view)| view.kind != ViewKind::Current && !view.parent)
        .max_by_key(|(index, view)| (view.created_at.unwrap_or(0), std::cmp::Reverse(*index)))
        .map(|(index, _)| index)
        .or_else(|| (!views.is_empty()).then_some(0))
}

const EFI_SYSTEM: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const MICROSOFT_RESERVED: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
const WINDOWS_RECOVERY: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";
const BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
const LINUX_FILESYSTEM: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const LINUX_SWAP: &str = "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f";
const BIOS_BOOT: &str = "21686148-6449-6e6f-744e-656564454649";

/// What a partition is for, in words: "Windows (C: drive)", "EFI system", …
pub fn volume_purpose(volume: &VolumeInfo) -> String {
    let kind = volume.partition_type.to_ascii_lowercase();
    let filesystem = volume.filesystem;
    match kind.as_str() {
        EFI_SYSTEM | "0xef" => "EFI system (startup files)".to_owned(),
        MICROSOFT_RESERVED => "Microsoft reserved".to_owned(),
        WINDOWS_RECOVERY | "0x27" => "Windows recovery".to_owned(),
        LINUX_SWAP | "0x82" => "Linux swap".to_owned(),
        BIOS_BOOT => "BIOS boot".to_owned(),
        LINUX_FILESYSTEM | "0x83" => "Linux files".to_owned(),
        BASIC_DATA | "0x07" if filesystem == Some(FilesystemKind::Ntfs) => {
            "Windows files".to_owned()
        }
        _ if !volume.name.is_empty() => volume.name.clone(),
        _ => match filesystem {
            Some(FilesystemKind::Ntfs) => "Windows files".to_owned(),
            Some(FilesystemKind::Ext4) => "Linux files".to_owned(),
            Some(_) => "Files".to_owned(),
            None => "Unrecognized contents".to_owned(),
        },
    }
}

/// Full label: "Partition 3 · Windows files · NTFS · 41.9 GB".
pub fn volume_title(volume: &VolumeInfo) -> String {
    let place = if volume.partition == 0 {
        "Whole disk".to_owned()
    } else {
        format!("Partition {}", volume.partition)
    };
    let filesystem = volume
        .filesystem
        .map_or_else(|| "can't be read".to_owned(), |kind| kind.to_string());
    format!(
        "{place} · {} · {filesystem} · {}",
        volume_purpose(volume),
        format_size(volume.length)
    )
}

/// True for small helper partitions that rarely hold the files people want.
fn is_helper_partition(volume: &VolumeInfo) -> bool {
    matches!(
        volume.partition_type.to_ascii_lowercase().as_str(),
        EFI_SYSTEM
            | MICROSOFT_RESERVED
            | WINDOWS_RECOVERY
            | BIOS_BOOT
            | LINUX_SWAP
            | "0xef"
            | "0x27"
            | "0x82"
    )
}

/// The readable volume most likely to hold the protected computer's files:
/// the largest supported non-helper partition, else the largest supported one.
pub fn recommended_volume(volumes: &[VolumeInfo]) -> Option<usize> {
    let largest = |helpers: bool| {
        volumes
            .iter()
            .enumerate()
            .filter(|(_, volume)| {
                volume.filesystem.is_some() && (helpers || !is_helper_partition(volume))
            })
            .max_by_key(|(index, volume)| (volume.length, std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    };
    largest(false).or_else(|| largest(true))
}

/// Explorer-style size: "1 byte", "13 bytes", "10.0 KB", "41.9 GB" (1024-based).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    match bytes {
        1 => return "1 byte".to_owned(),
        0..1024 => return format!("{bytes} bytes"),
        _ => {}
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A failure explained for a person rather than a parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendlyError {
    pub title: String,
    pub message: String,
    /// The original engine text, for "Show details" and support requests.
    pub details: String,
}

/// Turn an engine error into a headline, a next step, and retained details.
pub fn friendly_error(raw: &str) -> FriendlyError {
    let lower = raw.to_ascii_lowercase();
    let (title, message) = if lower.contains("could not open this backup")
        || lower.contains("source was not recognized")
    {
        (
            "This file isn't a backup ZFS Explore can open",
            "Choose a ZFS send file, a Slide or ZFS pool image, or a virtual disk (RAW, QCOW2 or VMDK). Single-disk and mirrored pools are supported.",
        )
    } else if lower.contains("already exists") {
        (
            "Something with that name is already there",
            "Choose a different name or folder. Nothing was overwritten.",
        )
    } else if lower.contains("did not authenticate")
        || lower.contains("raw zfs key must")
        || lower.contains("hex zfs key must")
    {
        (
            "That key didn't unlock this backup",
            "Check that you copied the whole key, or choose the key file for this backup, and try again.",
        )
    } else if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("os error 5")
        || lower.contains("not granted")
    {
        (
            "ZFS Explore wasn't allowed to read this",
            "For an attached drive, approve the administrator prompt (Mac) or start ZFS Explore with \"Run as administrator\" (Windows), then try again.",
        )
    } else if lower.contains("no longer attached")
        || lower.contains("no such file")
        || lower.contains("cannot find the file")
    {
        (
            "The backup can't be found",
            "Reconnect the drive or check that the file is still in the same place, then open it again.",
        )
    } else if lower.contains("no space") || lower.contains("disk full") {
        (
            "There isn't enough free space",
            "Choose a destination with more free space and try again.",
        )
    } else if lower.contains("is encrypted")
        || lower.contains("requires a dataset key")
        || lower.contains("provide its")
    {
        (
            "This backup is locked",
            "Enter its encryption key or choose its key file to continue.",
        )
    } else {
        (
            "That didn't work",
            "ZFS Explore couldn't finish this step. Your backup has not been changed. The details below can help with troubleshooting.",
        )
    };
    FriendlyError {
        title: title.to_owned(),
        message: message.to_owned(),
        details: raw.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(kind: ViewKind, dataset: &str, created_at: Option<u64>, parent: bool) -> SourceView {
        SourceView {
            label: String::new(),
            created_at,
            selector: dataset.to_owned(),
            update_eligible: false,
            encrypted: false,
            key_format: None,
            kind,
            dataset: dataset.to_owned(),
            snapshot: (kind != ViewKind::Current).then(|| "s1".to_owned()),
            parent,
        }
    }

    fn volume(
        partition: u32,
        kind: &str,
        filesystem: Option<FilesystemKind>,
        length: u64,
    ) -> VolumeInfo {
        VolumeInfo {
            selector: format!("gpt{partition}"),
            scheme: "gpt".to_owned(),
            partition,
            offset: 0,
            length,
            partition_type: kind.to_owned(),
            name: String::new(),
            filesystem,
            diagnostic: None,
        }
    }

    #[test]
    fn recommends_the_newest_snapshot_of_a_dataset_with_files() {
        let views = [
            view(ViewKind::Snapshot, "pool/agents/linux", Some(10), false),
            view(ViewKind::Snapshot, "pool/agents/linux", Some(30), false),
            view(ViewKind::Snapshot, "pool/agents", Some(40), true),
            view(ViewKind::Current, "pool/agents/linux", None, false),
        ];
        assert_eq!(recommended_view(&views), Some(1));
        assert_eq!(recommended_view(&views[3..]), Some(0));
        assert_eq!(recommended_view(&[]), None);
    }

    #[test]
    fn recommends_the_largest_data_partition_over_helpers() {
        let volumes = [
            volume(1, EFI_SYSTEM, Some(FilesystemKind::Fat32), 512 << 20),
            volume(2, MICROSOFT_RESERVED, None, 16 << 20),
            volume(3, BASIC_DATA, Some(FilesystemKind::Ntfs), 60 << 30),
            volume(4, WINDOWS_RECOVERY, Some(FilesystemKind::Ntfs), 700 << 20),
        ];
        assert_eq!(recommended_volume(&volumes), Some(2));
        assert_eq!(recommended_volume(&volumes[..2]), Some(0));
        assert_eq!(recommended_volume(&volumes[1..2]), None);
        assert_eq!(
            volume_title(&volumes[2]),
            "Partition 3 · Windows files · NTFS · 60.0 GB"
        );
        assert_eq!(
            volume_title(&volumes[1]),
            "Partition 2 · Microsoft reserved · can't be read · 16.0 MB"
        );
    }

    #[test]
    fn describes_views_without_zfs_vocabulary() {
        let current = view(ViewKind::Current, "pool/agents/linux", None, false);
        assert_eq!(
            view_title(&current, SnapshotTimeZone::Local),
            "Current files"
        );
        assert_eq!(view_detail(&current), "linux · live files, not a snapshot");
        let mut incremental = view(ViewKind::IncrementalSend, "lab/data", None, false);
        incremental.encrypted = true;
        assert_eq!(view_detail(&incremental), "data · changes only");
        assert_eq!(
            view_line(&incremental, SnapshotTimeZone::Local),
            "s1  —  data · changes only  ·  encrypted"
        );
        let parent = view(ViewKind::Current, "pool/agents", None, true);
        assert_eq!(view_detail(&parent), "agents · holds other backups");
    }

    #[test]
    fn sizes_read_like_explorer() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1), "1 byte");
        assert_eq!(format_size(1023), "1023 bytes");
        assert_eq!(format_size(10 * 1024), "10.0 KB");
        assert_eq!(format_size(67_108_864), "64.0 MB");
    }

    #[test]
    fn errors_keep_details_and_explain_the_next_step() {
        let error = friendly_error(
            "Could not open this backup. Choose a ZFS send…\n\nZFS stream: I/O error at stream offset 0",
        );
        assert_eq!(error.title, "This file isn't a backup ZFS Explore can open");
        assert!(error.details.contains("I/O error"));
        assert_eq!(
            friendly_error("the supplied key did not authenticate the encrypted ZFS dataset").title,
            "That key didn't unlock this backup"
        );
        assert_eq!(
            friendly_error("A file or folder already exists there.").title,
            "Something with that name is already there"
        );
        assert_eq!(friendly_error("unexpected").title, "That didn't work");
    }
}
