import DiskArbitration
import Foundation
import IOKit

/// One partition, or the whole disk, that the user can open read-only.
struct DriveSlice: Identifiable, Hashable {
    let bsdName: String
    let size: Int64
    let content: String
    let mountPath: String?
    let whole: Bool
    var mediaID: UInt64 = 0
    var id: String { "\(bsdName):\(mediaID)" }
    var path: String { "/dev/\(bsdName)" }
    var sizeLabel: String { ByteCountFormatter.string(fromByteCount: size, countStyle: .file) }

    enum Kind { case zfs, linux, other, system, mounted }
    var kind: Kind {
        if mountPath != nil { return .mounted }
        switch content.uppercased() {
        // OpenZFS (Linux, illumos, macOS) and FreeBSD ZFS partition types.
        case "6A898CC3-1DD2-11B2-99A6-080020736631", "516E7CBA-6ECF-11D6-8FF8-00022D09712B": return .zfs
        // Datto and many Linux-hosted pools use the generic Linux data type.
        case "LINUX", "0FC63DAF-8483-4772-8E79-3D69D8477DE4", "SOLARIS": return .linux
        case "LINUX_SWAP", "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F", "EFI",
             "C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "21686148-6449-6E6F-744E-656564454649":
            return .system
        default: return .other
        }
    }
    var typeLabel: String {
        switch content.uppercased() {
        case "6A898CC3-1DD2-11B2-99A6-080020736631", "516E7CBA-6ECF-11D6-8FF8-00022D09712B": return "ZFS"
        case "LINUX", "0FC63DAF-8483-4772-8E79-3D69D8477DE4": return "Linux"
        case "LINUX_SWAP", "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F": return "Linux swap"
        case "EFI", "C12A7328-F81F-11D2-BA4B-00A0C93EC93B": return "EFI system"
        case "21686148-6449-6E6F-744E-656564454649": return "BIOS boot"
        case "FDISK_PARTITION_SCHEME": return "MBR partition table"
        case "GUID_PARTITION_SCHEME": return "GPT partition table"
        case "": return "Unpartitioned"
        default: return content.replacingOccurrences(of: "_", with: " ")
        }
    }
    var hint: String {
        switch kind {
        case .zfs: return "ZFS partition"
        case .linux: return "macOS can’t read this · may hold a ZFS pool"
        case .mounted: return "Mounted by macOS at \(mountPath!)"
        case .system: return "System partition"
        case .other: return whole ? "Whole disk · ZFS Explore looks for the pool partition" : "Not mounted by macOS"
        }
    }
}

/// A physical (or attached image) disk and its partitions.
struct AttachedDrive: Identifiable {
    let disk: DriveSlice
    let name: String
    let bus: String
    var partitions: [DriveSlice]
    var id: String { disk.id }
    var title: String { "\(name) · \(bus) · \(disk.sizeLabel)" }

    /// The slice most likely to hold the pool: a ZFS-typed partition, then the
    /// largest partition macOS left unmounted, then the whole disk.
    var suggested: DriveSlice {
        partitions.first { $0.kind == .zfs }
            ?? partitions.filter { $0.kind == .linux || $0.kind == .other }.max { $0.size < $1.size }
            ?? disk
    }

    /// External and attached-image disks from DiskArbitration, including disks
    /// whose partitions macOS cannot mount and Finder therefore never shows.
    static func scan() -> [AttachedDrive] {
        guard let session = DASessionCreate(kCFAllocatorDefault) else { return [] }
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOMedia"), &iterator) == KERN_SUCCESS else { return [] }
        defer { IOObjectRelease(iterator) }
        var disks: [String: AttachedDrive] = [:]
        var children: [(whole: String, slice: DriveSlice)] = []
        var virtualWithVolumes = Set<String>()
        while case let service = IOIteratorNext(iterator), service != 0 {
            defer { IOObjectRelease(service) }
            guard let disk = DADiskCreateFromIOMedia(kCFAllocatorDefault, session, service),
                  let info = DADiskCopyDescription(disk) as? [String: Any],
                  let bsd = info[kDADiskDescriptionMediaBSDNameKey as String] as? String else { continue }
            // Internal storage never holds an offline pool member for this app.
            if info[kDADiskDescriptionDeviceInternalKey as String] as? Bool == true { continue }
            let content = info[kDADiskDescriptionMediaContentKey as String] as? String ?? ""
            // APFS containers are synthesized disks, not partitions on the device.
            if content.uppercased() == "EF57347C-0000-11AA-AA11-00306543ECAC" { continue }
            let mount = (info[kDADiskDescriptionVolumePathKey as String] as? URL)?.path
            let bus = info[kDADiskDescriptionDeviceProtocolKey as String] as? String ?? "External"
            var slice = DriveSlice(
                bsdName: bsd,
                size: (info[kDADiskDescriptionMediaSizeKey as String] as? NSNumber)?.int64Value ?? 0,
                content: content, mountPath: mount,
                whole: info[kDADiskDescriptionMediaWholeKey as String] as? Bool ?? false)
            IORegistryEntryGetRegistryEntryID(service, &slice.mediaID)
            var wholeName = bsd
            // Keep the whole-disk object alive while its BSD name is copied.
            if let wholeDisk = DADiskCopyWholeDisk(disk), let name = DADiskGetBSDName(wholeDisk) {
                wholeName = String(cString: name)
            }
            if slice.whole {
                let vendor = (info[kDADiskDescriptionDeviceVendorKey as String] as? String ?? "").trimmingCharacters(in: .whitespaces)
                let model = (info[kDADiskDescriptionDeviceModelKey as String] as? String ?? "").trimmingCharacters(in: .whitespaces)
                let name = [vendor, model].filter { !$0.isEmpty }.joined(separator: " ")
                disks[bsd] = AttachedDrive(disk: slice, name: name.isEmpty ? "External disk" : name,
                                           bus: bus == "Virtual Interface" ? "Disk image" : bus, partitions: [])
            } else {
                children.append((wholeName, slice))
            }
            // Mounted or Apple-formatted disk images (such as this app's own DMG)
            // are not backups. A pool image attached with `-nomount` stays listed.
            if bus == "Virtual Interface", mount != nil || content.uppercased().hasSuffix("-11AA-AA11-00306543ECAC") {
                virtualWithVolumes.insert(wholeName)
            }
        }
        for child in children { disks[child.whole]?.partitions.append(child.slice) }
        return disks.values
            .filter { !($0.bus == "Disk image" && virtualWithVolumes.contains($0.disk.bsdName)) }
            .map { drive in
                var drive = drive
                drive.partitions.sort { $0.bsdName.localizedStandardCompare($1.bsdName) == .orderedAscending }
                return drive
            }
            .sorted { $0.disk.bsdName.localizedStandardCompare($1.disk.bsdName) == .orderedAscending }
    }
}
