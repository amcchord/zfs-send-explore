//! Shared read-only access and size detection for pool and filesystem devices.
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Read-only handles a desktop client obtained through an operating-system
/// authorization prompt (macOS `authopen`) for device nodes the user cannot
/// open directly. Pool members are reopened per operation by path, so the
/// handle is kept here and duplicated for subsequent reads of that source.
static AUTHORIZED_SOURCES: Mutex<BTreeMap<PathBuf, File>> = Mutex::new(BTreeMap::new());

/// Pin a read-only handle to `path` until the desktop session is closed.
#[cfg(unix)]
pub fn register_authorized_source(path: &Path, file: File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: F_GETFL only reads the flags of this live descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        if flags & libc::O_ACCMODE != libc::O_RDONLY {
            bail!("An authorized source handle must be read-only");
        }
        // SCM_RIGHTS does not preserve close-on-exec on macOS. Do not pass an
        // earlier backup handle into a later authorization child process.
        // SAFETY: F_SETFD sets descriptor flags on this live, owned descriptor.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    AUTHORIZED_SOURCES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_owned(), file);
    Ok(())
}

/// Close every authorized handle except the one for `keep`, if any.
pub fn release_authorized_sources(keep: Option<&Path>) {
    AUTHORIZED_SOURCES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|path, _| Some(path.as_path()) == keep);
}

pub(crate) fn open_read_only(path: &Path) -> std::io::Result<File> {
    let authorized = AUTHORIZED_SOURCES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(file) = authorized.get(path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let current = std::fs::metadata(path)?;
            let approved = file.metadata()?;
            if (current.dev(), current.ino(), current.rdev())
                != (approved.dev(), approved.ino(), approved.rdev())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "The approved device changed or was disconnected. Choose the drive again.",
                ));
            }
        }
        return file.try_clone();
    }
    drop(authorized);
    File::open(path)
}

/// Regular files may be empty. Devices often have zero or unavailable metadata
/// size, so use the platform's disk geometry control before a seek fallback.
pub(crate) fn source_length(file: &File, path: &Path) -> Result<u64> {
    if let Ok(metadata) = file.metadata()
        && (metadata.is_file() || metadata.len() != 0)
    {
        return Ok(metadata.len());
    }
    let len = device_length(file).or_else(|device_error| {
        let mut probe = file.try_clone()?;
        probe.seek(SeekFrom::End(0)).with_context(|| {
            format!(
                "determining the size of {} ({device_error:#})",
                path.display()
            )
        })
    })?;
    if len == 0 {
        bail!("could not determine the size of {}", path.display());
    }
    Ok(len)
}

#[cfg(windows)]
fn device_length(file: &File) -> Result<u64> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::ptr;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO};

    let mut length = GET_LENGTH_INFORMATION::default();
    let mut returned = 0_u32;
    // SAFETY: the output is a correctly sized POD structure and the call is
    // synchronous, so neither the handle nor output pointer can outlive scope.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            IOCTL_DISK_GET_LENGTH_INFO,
            ptr::null(),
            0,
            (&mut length as *mut GET_LENGTH_INFORMATION).cast(),
            size_of::<GET_LENGTH_INFORMATION>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("querying physical drive length");
    }
    u64::try_from(length.Length).context("physical drive reported a negative length")
}

#[cfg(target_os = "macos")]
fn device_length(file: &File) -> Result<u64> {
    use std::os::unix::io::AsRawFd;
    // <sys/disk.h>: DKIOCGETBLOCKSIZE = _IOR('d', 24, uint32_t),
    // DKIOCGETBLOCKCOUNT = _IOR('d', 25, uint64_t).
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;
    let mut block_size: u32 = 0;
    let mut block_count: u64 = 0;
    // SAFETY: both ioctls write one correctly sized integer into the pointer.
    let ok = unsafe {
        libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut block_size) == 0
            && libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut block_count) == 0
    };
    if !ok {
        return Err(std::io::Error::last_os_error()).context("querying disk length");
    }
    block_count
        .checked_mul(u64::from(block_size))
        .ok_or_else(|| anyhow::anyhow!("disk length overflows u64"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn device_length(_file: &File) -> Result<u64> {
    bail!("platform has no block-device length control")
}

/// Device nodes require reads aligned to their logical sector size. Regular
/// files allow byte reads; the same positioned API serves both kinds.
pub(crate) fn source_block_size(file: &File) -> Result<usize> {
    if file.metadata().is_ok_and(|m| m.is_file()) {
        return Ok(1);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let mut size: u32 = 0;
        // SAFETY: the ioctl writes one u32 to a live buffer.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x4004_6418u64 as libc::c_ulong, &mut size) } != 0
        {
            return Err(std::io::Error::last_os_error()).context("querying disk sector size");
        }
        validate_block_size(size)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::IO::DeviceIoControl;
        use windows_sys::Win32::System::Ioctl::{DISK_GEOMETRY, IOCTL_DISK_GET_DRIVE_GEOMETRY};
        let mut geometry = DISK_GEOMETRY::default();
        let mut returned = 0;
        // SAFETY: the synchronous call writes to a correctly sized POD buffer.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                IOCTL_DISK_GET_DRIVE_GEOMETRY,
                std::ptr::null(),
                0,
                (&mut geometry as *mut DISK_GEOMETRY).cast(),
                std::mem::size_of::<DISK_GEOMETRY>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("querying disk sector size");
        }
        validate_block_size(geometry.BytesPerSector)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Ok(1)
    }
}
#[cfg(any(target_os = "macos", windows, test))]
fn validate_block_size(size: u32) -> Result<usize> {
    if !(512..=1024 * 1024).contains(&size) || !size.is_power_of_two() {
        bail!("Disk reported an invalid sector size: {size}");
    }
    Ok(size as usize)
}

pub(crate) fn read_exact_at(
    file: &File,
    block_size: usize,
    offset: u64,
    buffer: &mut [u8],
) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    if block_size == 1
        || (offset.is_multiple_of(block_size as u64) && buffer.len().is_multiple_of(block_size))
    {
        return positioned_read(file, offset, buffer);
    }
    // Bound scratch space even when a caller requests a large file range.
    let capacity = buffer
        .len()
        .saturating_add(block_size - 1)
        .min(1024 * 1024)
        .div_ceil(block_size)
        * block_size;
    let mut scratch = vec![0; capacity];
    let mut complete = 0;
    while complete < buffer.len() {
        let position = offset
            .checked_add(complete as u64)
            .context("device read offset overflows")?;
        let start = position / block_size as u64 * block_size as u64;
        let skip = (position - start) as usize;
        let count = (buffer.len() - complete).min(scratch.len() - skip);
        let read = (skip + count).div_ceil(block_size) * block_size;
        positioned_read(file, start, &mut scratch[..read])?;
        buffer[complete..complete + count].copy_from_slice(&scratch[skip..skip + count]);
        complete += count;
    }
    Ok(())
}
fn positioned_read(file: &File, offset: u64, buffer: &mut [u8]) -> Result<()> {
    let mut complete = 0;
    while complete < buffer.len() {
        let position = offset
            .checked_add(complete as u64)
            .context("source read offset overflows")?;
        #[cfg(unix)]
        let count = {
            use std::os::unix::fs::FileExt;
            file.read_at(&mut buffer[complete..], position)?
        };
        #[cfg(windows)]
        let count = {
            use std::os::windows::fs::FileExt;
            file.seek_read(&mut buffer[complete..], position)?
        };
        if count == 0 {
            bail!("Source ended during a read; the drive may have been disconnected");
        }
        complete += count;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regular_file_length_includes_empty_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.img");
        std::fs::write(&path, []).unwrap();
        assert_eq!(
            source_length(&File::open(&path).unwrap(), &path).unwrap(),
            0
        );
        std::fs::write(&path, [0; 1024]).unwrap();
        assert_eq!(
            source_length(&File::open(&path).unwrap(), &path).unwrap(),
            1024
        );
    }
    #[test]
    fn aligned_reads_use_explicit_offsets_and_preserve_bytes() {
        use std::io::Seek;
        let mut file = tempfile::tempfile().unwrap();
        let expected: Vec<_> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::io::Write::write_all(&mut file, &expected).unwrap();
        file.seek(SeekFrom::Start(13)).unwrap();
        for block in [1, 512, 4096] {
            for (offset, count) in [(17, 7), (511, 1024), (4093, 1024), (1, 2 * 1024 * 1024 + 3)] {
                let mut read = vec![0; count];
                read_exact_at(&file, block, offset as u64, &mut read).unwrap();
                assert_eq!(read, &expected[offset..offset + count]);
                // Windows seek_read updates the OS cursor, but every read uses
                // an explicit offset. Unix read_at leaves the cursor unchanged.
                #[cfg(unix)]
                assert_eq!(file.stream_position().unwrap(), 13);
            }
        }
        for invalid in [0, 1, 513, u32::MAX] {
            assert!(validate_block_size(invalid).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn approved_handles_are_read_only_pinned_and_released() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.img");
        std::fs::write(&path, b"original").unwrap();
        assert!(
            register_authorized_source(
                &path,
                File::options().read(true).write(true).open(&path).unwrap()
            )
            .is_err()
        );
        let file = File::open(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        register_authorized_source(&path, file).unwrap();
        assert_eq!(open_read_only(&path).unwrap().metadata().unwrap().len(), 8);
        release_authorized_sources(Some(&path));
        assert!(open_read_only(&path).is_ok());
        std::fs::rename(&path, directory.path().join("old.img")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(
            open_read_only(&path).is_err(),
            "Do not follow a reused device path"
        );
        release_authorized_sources(None);
        assert_eq!(open_read_only(&path).unwrap().metadata().unwrap().len(), 11);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&path).is_err() {
            assert!(open_read_only(&path).is_err());
        }
    }
}
