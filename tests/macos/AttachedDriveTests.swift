import Foundation

@main struct AttachedDriveTests {
    static func main() {
        func slice(_ name: String, _ content: String, _ size: Int64 = 1024, mount: String? = nil, whole: Bool = false, media: UInt64 = 0) -> DriveSlice {
            DriveSlice(bsdName: name, size: size, content: content, mountPath: mount, whole: whole, mediaID: media)
        }
        let whole = slice("disk99", "FDisk_partition_scheme", whole: true)
        let linux = slice("disk99s3", "Linux", 4096)
        let zfs = slice("disk99s2", "6A898CC3-1DD2-11B2-99A6-080020736631")
        let efi = slice("disk99s1", "EFI", 16384)
        let mounted = slice("disk99s4", "Linux", 32768, mount: "/Volumes/Example")
        var drive = AttachedDrive(disk: whole, name: "Example external drive", bus: "USB", partitions: [efi, linux, mounted, zfs])
        precondition(drive.suggested.id == zfs.id)
        drive.partitions.removeLast()
        precondition(drive.suggested.id == linux.id)
        drive.partitions = [efi, mounted]
        precondition(drive.suggested.id == whole.id)
        precondition(slice("disk99s2", "516E7CBA-6ECF-11D6-8FF8-00022D09712B").typeLabel == "ZFS")
        precondition(zfs.path == "/dev/disk99s2")
        precondition(slice("disk99", "", whole: true, media: 1).id != slice("disk99", "", whole: true, media: 2).id, "Reused BSD names must not keep a prior selection")
        let second = AttachedDrive(disk: slice("disk100", "", whole: true, media: 3), name: "Second", bus: "USB", partitions: [])
        var selection = DriveSelection()
        precondition(selection.available(in: [drive, second]).isEmpty, "Showing drives must not select one")
        selection.targets[drive.id] = drive.disk
        selection.targets[second.id] = second.disk
        precondition(selection.available(in: [second, drive]).map { $0.1.id } == [second.disk.id, drive.disk.id])
        precondition(selection.unavailable(in: [drive]) == [second.disk])
        let replacement = AttachedDrive(disk: slice("disk100", "", whole: true, media: 4), name: "Second", bus: "USB", partitions: [])
        precondition(selection.unavailable(in: [drive, replacement]) == [second.disk])
        selection.removeUnavailable(in: [drive, replacement])
        precondition(selection.targets.count == 1 && selection.targets[drive.id] == drive.disk)
        precondition(!drive.hasZFSPartition && !replacement.hasZFSPartition)
        drive.partitions.append(zfs)
        precondition(drive.hasZFSPartition)
        selection.targets[drive.id] = zfs
        precondition(selection.available(in: [drive]).map { $0.1 } == [zfs])
        drive.partitions.removeLast()
        precondition(selection.available(in: [drive]).isEmpty && selection.unavailable(in: [drive]) == [zfs],
                     "A vanished partition must not fall back to the whole disk")
        let actual = AttachedDrive.scan()
        for d in actual { print("\(d.disk.path) \(d.title) \(d.partitions.map(\.path))") }
        print("PASS: explicit selection, partition choices, ZFS hints, hot-plug identities and live discovery")
    }
}
