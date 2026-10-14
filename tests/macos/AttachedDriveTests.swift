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
        let actual = AttachedDrive.scan()
        for d in actual { print("\(d.disk.path) \(d.title) \(d.partitions.map(\.path))") }
        print("PASS: ZFS/Linux selection, mounted/system fallback, device identity and live discovery")
    }
}
