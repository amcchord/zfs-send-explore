//! Windows publishes physical disk names even for media without a drive letter.
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_DEVICE_DISK, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, QueryDosDeviceW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_EXTENT, DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
    IOCTL_STORAGE_GET_DEVICE_NUMBER, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT,
    STORAGE_DEVICE_NUMBER, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

pub(super) fn physical_disk_numbers() -> io::Result<Vec<u32>> {
    let mut names = vec![0u16; 4096];
    loop {
        // SAFETY: a null name requests the complete list. Capacity is in UTF-16 code units.
        let count = unsafe { QueryDosDeviceW(null(), names.as_mut_ptr(), names.len() as u32) };
        if count > 0 {
            return Ok(parse_disk_names(&names[..count as usize]));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error);
        }
        let capacity = u32::try_from(names.len())
            .ok()
            .and_then(|n| n.checked_mul(2))
            .ok_or_else(|| io::Error::other("Windows device name list is too large"))?;
        names.resize(capacity as usize, 0);
    }
}

fn parse_disk_names(names: &[u16]) -> Vec<u32> {
    let mut disks = Vec::new();
    for name in names.split(|&ch| ch == 0) {
        let Ok(name) = String::from_utf16(name) else {
            continue;
        };
        let Some(prefix) = name.get(.."PhysicalDrive".len()) else {
            continue;
        };
        let suffix = &name[prefix.len()..];
        if prefix.eq_ignore_ascii_case("PhysicalDrive")
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(number) = suffix.parse::<u32>()
        {
            disks.push(number);
        }
    }
    disks.sort_unstable();
    disks.dedup();
    disks
}

/// Resolve the Windows installation volume through its mount point. A spanned
/// system volume may have several extents, including multiple on one disk.
pub(super) fn windows_volume_disks() -> io::Result<Vec<u32>> {
    let mut directory = vec![0u16; 32768];
    let mut mount = vec![0u16; 32768];
    let mut volume = [0u16; 64]; // A volume GUID path has a fixed format.
    // SAFETY: all buffers are writable and their capacities are supplied in UTF-16 units.
    unsafe {
        let length = GetSystemWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32);
        if length == 0
            || length as usize >= directory.len()
            || GetVolumePathNameW(directory.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) == 0
            || GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let end = volume
        .iter()
        .position(|&ch| ch == 0)
        .and_then(|n| n.checked_sub(1))
        .filter(|&last| volume[last] == u16::from(b'\\'))
        .ok_or_else(|| io::Error::other("Invalid Windows volume path"))?;
    let file = File::options()
        .access_mode(0)
        .open(OsString::from_wide(&volume[..end]))?;
    let mut output = vec![0u8; size_of::<VOLUME_DISK_EXTENTS>()];
    loop {
        let mut returned = 0u32;
        // SAFETY: DeviceIoControl fills an output buffer of the indicated size.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle().cast(),
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                null(),
                0,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok != 0 {
            return extent_disk_numbers(&output[..returned as usize]);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            // Basic volumes can also report their single device number directly.
            return single_volume_disk(&file);
        }
        let size = extent_bytes(&output)?;
        if size <= output.len() {
            return Err(error);
        }
        output.resize(size, 0);
    }
}

fn extent_bytes(output: &[u8]) -> io::Result<usize> {
    let count = output
        .first_chunk::<4>()
        .map(|b| u32::from_ne_bytes(*b))
        .filter(|&n| n > 0)
        .ok_or_else(|| io::Error::other("Windows supplied no disk extents"))?;
    let bytes = (count as usize)
        .checked_mul(size_of::<DISK_EXTENT>())
        .and_then(|n| n.checked_add(offset_of!(VOLUME_DISK_EXTENTS, Extents)))
        .filter(|&n| u32::try_from(n).is_ok())
        .ok_or_else(|| io::Error::other("Invalid Windows volume extent count"))?;
    Ok(bytes)
}

fn extent_disk_numbers(output: &[u8]) -> io::Result<Vec<u32>> {
    let size = extent_bytes(output)?;
    let extents = output
        .get(offset_of!(VOLUME_DISK_EXTENTS, Extents)..size)
        .ok_or_else(|| io::Error::other("Incomplete Windows volume extent list"))?;
    let mut numbers: Vec<_> = extents
        .chunks_exact(size_of::<DISK_EXTENT>())
        .map(|extent| u32::from_ne_bytes(*extent.first_chunk().unwrap()))
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    Ok(numbers)
}

fn single_volume_disk(file: &File) -> io::Result<Vec<u32>> {
    let mut device = STORAGE_DEVICE_NUMBER::default();
    let mut returned = 0u32;
    // SAFETY: the output matches IOCTL_STORAGE_GET_DEVICE_NUMBER exactly.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            null(),
            0,
            (&mut device as *mut STORAGE_DEVICE_NUMBER).cast(),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if (returned as usize) < size_of::<STORAGE_DEVICE_NUMBER>()
        || device.DeviceType != FILE_DEVICE_DISK
        || device.DeviceNumber == u32::MAX
    {
        return Err(io::Error::other(
            "Windows could not identify its system disk",
        ));
    }
    Ok(vec![device.DeviceNumber])
}

/// Optional identification hint from Windows' partition metadata. A raw or
/// inaccessible disk is still a valid selection without this hint.
pub(super) fn has_zfs_partition(handle: HANDLE) -> bool {
    let mut layout = vec![0u8; 4096];
    loop {
        let mut returned = 0;
        // SAFETY: the live metadata-only handle is borrowed from drive inspection.
        // The kernel writes at most the supplied buffer length.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                null(),
                0,
                layout.as_mut_ptr().cast(),
                layout.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok != 0 {
            return layout
                .get(..returned as usize)
                .is_some_and(layout_has_zfs_partition);
        }
        let code = io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code as u32);
        // The hint is optional. Bound the cost of querying unusual disk layouts.
        if !matches!(code, Some(ERROR_INSUFFICIENT_BUFFER | ERROR_MORE_DATA))
            || layout.len() >= 1024 * 1024
        {
            return false;
        }
        layout.resize(layout.len() * 2, 0);
    }
}

fn layout_has_zfs_partition(layout: &[u8]) -> bool {
    if !layout.starts_with(&PARTITION_STYLE_GPT.to_ne_bytes()) {
        return false;
    }
    let Some(count) = layout.get(4..8).and_then(|bytes| bytes.first_chunk::<4>()) else {
        return false;
    };
    let count = u32::from_ne_bytes(*count) as usize;
    let start = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
    let Some(entries) = layout.get(start..) else {
        return false;
    };
    let entries = entries.chunks_exact(size_of::<PARTITION_INFORMATION_EX>());
    if entries.len() < count {
        return false;
    }
    // The GPT union starts with the 16-byte partition type GUID. GUIDs store
    // the first three fields little endian and the remaining eight bytes in order.
    let offset = offset_of!(PARTITION_INFORMATION_EX, Anonymous);
    entries.take(count).any(|entry| {
        if !entry.starts_with(&PARTITION_STYLE_GPT.to_ne_bytes()) {
            return false;
        }
        let mut guid = *entry[offset..].first_chunk::<16>().unwrap();
        guid[..4].reverse();
        guid[4..6].reverse();
        guid[6..8].reverse();
        matches!(
            u128::from_be_bytes(guid),
            0x6a898cc3_1dd2_11b2_99a6_080020736631 | 0x516e7cba_6ecf_11d6_8ff8_00022d09712b
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_names_include_high_and_nonconsecutive_numbers_only_once() {
        let names: Vec<u16> = "PhysicalDrive128\0C:\0PhysicalDrive2\0PHYSICALDRIVE128\0PhysicalDrive+1\0PhysicalDrive2x\0PhysicalDrive\0\0".encode_utf16().collect();
        assert_eq!(parse_disk_names(&names), [2, 128]);
    }

    #[test]
    fn zfs_partition_hint_requires_a_recognized_gpt_type() {
        let start = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        let mut layout = vec![0; start + size_of::<PARTITION_INFORMATION_EX>()];
        layout[..4].copy_from_slice(&PARTITION_STYLE_GPT.to_ne_bytes());
        layout[4..8].copy_from_slice(&1u32.to_ne_bytes());
        layout[start..start + 4].copy_from_slice(&PARTITION_STYLE_GPT.to_ne_bytes());
        let offset = start + offset_of!(PARTITION_INFORMATION_EX, Anonymous);
        let guid = [
            0xc3, 0x8c, 0x89, 0x6a, 0xd2, 0x1d, 0xb2, 0x11, 0x99, 0xa6, 0x08, 0x00, 0x20, 0x73,
            0x66, 0x31,
        ];
        layout[offset..offset + 16].copy_from_slice(&guid);
        assert!(layout_has_zfs_partition(&layout));
        assert!(!layout_has_zfs_partition(&layout[..layout.len() - 1]));
        layout[offset] ^= 1;
        assert!(!layout_has_zfs_partition(&layout));
        layout[..4].fill(0);
        assert!(!layout_has_zfs_partition(&layout));
    }

    #[test]
    fn system_volume_includes_each_disk_and_refuses_truncated_extents() {
        let start = offset_of!(VOLUME_DISK_EXTENTS, Extents);
        let stride = size_of::<DISK_EXTENT>();
        let mut bytes = vec![0; start + 3 * stride];
        bytes[..4].copy_from_slice(&3u32.to_ne_bytes());
        for (index, disk) in [12u32, 3, 12].iter().enumerate() {
            let offset = start + index * stride;
            bytes[offset..offset + 4].copy_from_slice(&disk.to_ne_bytes());
        }
        assert_eq!(extent_disk_numbers(&bytes).unwrap(), [3, 12]);
        assert!(extent_disk_numbers(&bytes[..bytes.len() - 1]).is_err());
    }
}
