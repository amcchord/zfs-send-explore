//! Sparse-file primitives shared by send replay, pool extraction, and updates.
//!
//! Windows files must be explicitly marked sparse before zero ranges can be
//! deallocated.  Unix files become sparse when unwritten offsets are skipped;
//! on Linux and macOS we additionally punch already-allocated ranges when an
//! incremental send frees them.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

const CHUNK_SIZE: usize = 1024 * 1024;

/// Prepare a destination for sparse writes. Filesystems that do not expose a
/// sparse-file control simply retain ordinary zero-filled allocation.
pub fn prepare(file: &File) -> Result<()> {
    platform::prepare(file)
}

/// Write one logical extent while leaving all-zero chunks unallocated where
/// the host filesystem supports holes.
pub fn write_extent(file: &mut File, offset: u64, bytes: &[u8]) -> Result<()> {
    let mut position = offset;
    for chunk in bytes.chunks(CHUNK_SIZE) {
        if chunk.iter().all(|byte| *byte == 0) {
            zero_range(file, position, chunk.len() as u64)?;
        } else {
            file.seek(SeekFrom::Start(position))?;
            file.write_all(chunk)?;
        }
        position = position
            .checked_add(chunk.len() as u64)
            .context("sparse extent offset overflow")?;
    }
    Ok(())
}

/// Make a logical range read as zero and deallocate it when possible.
pub fn zero_range(file: &mut File, offset: u64, length: u64) -> Result<()> {
    let current_len = file.metadata()?.len();
    if offset >= current_len || length == 0 {
        return Ok(());
    }
    if length == u64::MAX {
        file.set_len(offset)?;
        return Ok(());
    }
    let length = length.min(current_len - offset);
    if platform::punch_hole(file, offset, length)? {
        return Ok(());
    }
    write_zeros(file, offset, length)
}

/// Copy a file into an empty destination without expanding source holes. The
/// destination length is established up front, then only allocated ranges are
/// copied when the platform can enumerate them. A zero-detecting fallback is
/// used on other filesystems.
pub fn copy(source: &mut File, destination: &mut File) -> Result<u64> {
    let length = source.metadata()?.len();
    prepare(destination)?;
    destination.set_len(length)?;
    if length == 0 {
        return Ok(0);
    }
    if platform::copy_allocated(source, destination, length)? {
        return Ok(length);
    }

    source.seek(SeekFrom::Start(0))?;
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    while offset < length {
        let wanted = usize::try_from((length - offset).min(CHUNK_SIZE as u64))
            .expect("chunk size always fits usize");
        source.read_exact(&mut buffer[..wanted])?;
        write_extent(destination, offset, &buffer[..wanted])?;
        offset += wanted as u64;
    }
    Ok(length)
}

fn copy_range(source: &mut File, destination: &mut File, offset: u64, length: u64) -> Result<()> {
    source.seek(SeekFrom::Start(offset))?;
    destination.seek(SeekFrom::Start(offset))?;
    let mut remaining = length;
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(CHUNK_SIZE as u64))
            .expect("chunk size always fits usize");
        source.read_exact(&mut buffer[..count])?;
        destination.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

fn write_zeros(file: &mut File, offset: u64, length: u64) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    let zeros = [0_u8; 64 * 1024];
    let mut remaining = length;
    while remaining != 0 {
        let count = usize::try_from(remaining.min(zeros.len() as u64))
            .expect("zero buffer size always fits usize");
        file.write_all(&zeros[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use super::copy_range;
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    use std::fs::File;
    use std::mem::{size_of, size_of_val};
    use std::os::windows::io::AsRawHandle;
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, ERROR_NOT_SUPPORTED,
        GetLastError,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FILE_ALLOCATED_RANGE_BUFFER, FILE_ZERO_DATA_INFORMATION, FSCTL_QUERY_ALLOCATED_RANGES,
        FSCTL_SET_SPARSE, FSCTL_SET_ZERO_DATA,
    };

    pub fn prepare(file: &File) -> Result<()> {
        let mut returned = 0_u32;
        // SAFETY: the handle is owned by `file`, all optional buffers are null,
        // and the synchronous call completes before this function returns.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            if !sparse_unsupported(error) {
                return Err(std::io::Error::last_os_error()).context("marking destination sparse");
            }
        }
        Ok(())
    }

    pub fn punch_hole(file: &File, offset: u64, length: u64) -> Result<bool> {
        prepare(file)?;
        let end = offset.context_add(length)?;
        let range = FILE_ZERO_DATA_INFORMATION {
            FileOffset: i64::try_from(offset).context("sparse range starts beyond i64")?,
            BeyondFinalZero: i64::try_from(end).context("sparse range ends beyond i64")?,
        };
        let mut returned = 0_u32;
        // SAFETY: the input structure and file handle remain valid for the
        // duration of this synchronous control request.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_ZERO_DATA,
                (&range as *const FILE_ZERO_DATA_INFORMATION).cast::<c_void>(),
                size_of::<FILE_ZERO_DATA_INFORMATION>() as u32,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        if sparse_unsupported(unsafe { GetLastError() }) {
            return Ok(false);
        }
        Err(std::io::Error::last_os_error()).context("deallocating a sparse range")
    }

    pub fn copy_allocated(source: &mut File, destination: &mut File, length: u64) -> Result<bool> {
        const RANGE_COUNT: usize = 256;
        let mut query_offset = 0_u64;
        while query_offset < length {
            let query = FILE_ALLOCATED_RANGE_BUFFER {
                FileOffset: i64::try_from(query_offset).context("file offset exceeds i64")?,
                Length: i64::try_from(length - query_offset).context("file length exceeds i64")?,
            };
            let mut ranges = [FILE_ALLOCATED_RANGE_BUFFER::default(); RANGE_COUNT];
            let mut returned = 0_u32;
            // SAFETY: all buffers are correctly sized POD arrays and remain
            // live until the synchronous call returns.
            let ok = unsafe {
                DeviceIoControl(
                    source.as_raw_handle(),
                    FSCTL_QUERY_ALLOCATED_RANGES,
                    (&query as *const FILE_ALLOCATED_RANGE_BUFFER).cast::<c_void>(),
                    size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
                    ranges.as_mut_ptr().cast::<c_void>(),
                    size_of_val(&ranges) as u32,
                    &mut returned,
                    ptr::null_mut(),
                )
            };
            let error = if ok == 0 {
                unsafe { GetLastError() }
            } else {
                0
            };
            if ok == 0 && sparse_unsupported(error) {
                return Ok(false);
            }
            if ok == 0 && error != ERROR_MORE_DATA {
                return Err(std::io::Error::last_os_error())
                    .context("querying allocated file ranges");
            }
            let count = returned as usize / size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
            if count == 0 {
                break;
            }
            for range in &ranges[..count] {
                if range.FileOffset < 0 || range.Length <= 0 {
                    continue;
                }
                copy_range(
                    source,
                    destination,
                    range.FileOffset as u64,
                    range.Length as u64,
                )?;
            }
            let last = ranges[count - 1];
            query_offset = (last.FileOffset as u64)
                .checked_add(last.Length as u64)
                .context("allocated range overflow")?;
            if ok != 0 {
                break;
            }
        }
        Ok(true)
    }

    fn sparse_unsupported(error: u32) -> bool {
        matches!(
            error,
            ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED
        )
    }

    trait CheckedAdd {
        fn context_add(self, right: u64) -> Result<u64>;
    }

    impl CheckedAdd for u64 {
        fn context_add(self, right: u64) -> Result<u64> {
            self.checked_add(right).context("sparse range overflow")
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::copy_range;
    use anyhow::{Context, Result};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    pub fn prepare(_file: &File) -> Result<()> {
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn punch_hole(file: &File, offset: u64, length: u64) -> Result<bool> {
        let offset = libc::off_t::try_from(offset).context("sparse offset exceeds off_t")?;
        let length = libc::off_t::try_from(length).context("sparse length exceeds off_t")?;
        // SAFETY: fallocate does not retain the descriptor or any pointers.
        let result = unsafe {
            libc::fallocate(
                file.as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                offset,
                length,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::EOPNOTSUPP || code == libc::EINVAL)
        {
            return Ok(false);
        }
        Err(error).context("punching a sparse file hole")
    }

    #[cfg(target_os = "macos")]
    pub fn punch_hole(file: &File, offset: u64, length: u64) -> Result<bool> {
        let mut punch = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: libc::off_t::try_from(offset).context("sparse offset exceeds off_t")?,
            fp_length: libc::off_t::try_from(length).context("sparse length exceeds off_t")?,
        };
        // SAFETY: fcntl reads the stack structure during this call only.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &mut punch) };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::ENOTSUP || code == libc::EINVAL)
        {
            return Ok(false);
        }
        Err(error).context("punching an APFS sparse file hole")
    }

    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    pub fn punch_hole(_file: &File, _offset: u64, _length: u64) -> Result<bool> {
        Ok(false)
    }

    pub fn copy_allocated(source: &mut File, destination: &mut File, length: u64) -> Result<bool> {
        let descriptor = source.as_raw_fd();
        let mut offset = 0_u64;
        while offset < length {
            // SAFETY: lseek only changes the file description's cursor.
            let data = unsafe { libc::lseek(descriptor, offset as libc::off_t, libc::SEEK_DATA) };
            if data < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ENXIO) {
                    break;
                }
                if matches!(error.raw_os_error(), Some(code) if code == libc::EINVAL || code == libc::ENOTSUP)
                {
                    return Ok(false);
                }
                return Err(error).context("locating sparse file data");
            }
            // SAFETY: same as the SEEK_DATA call above.
            let hole = unsafe { libc::lseek(descriptor, data, libc::SEEK_HOLE) };
            if hole < 0 {
                let error = io::Error::last_os_error();
                if matches!(error.raw_os_error(), Some(code) if code == libc::EINVAL || code == libc::ENOTSUP)
                {
                    return Ok(false);
                }
                return Err(error).context("locating sparse file hole");
            }
            let start = data as u64;
            let end = (hole as u64).min(length);
            if end <= start {
                return Ok(false);
            }
            copy_range(source, destination, start, end - start)?;
            offset = end;
        }
        Ok(true)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use anyhow::Result;
    use std::fs::File;

    pub fn prepare(_file: &File) -> Result<()> {
        Ok(())
    }

    pub fn punch_hole(_file: &File, _offset: u64, _length: u64) -> Result<bool> {
        Ok(false)
    }

    pub fn copy_allocated(
        _source: &mut File,
        _destination: &mut File,
        _length: u64,
    ) -> Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{copy, prepare, write_extent, zero_range};
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};

    #[test]
    fn sparse_writes_and_copy_preserve_logical_contents() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.bin");
        let copy_path = directory.path().join("copy.bin");
        let mut source = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
            .unwrap();
        prepare(&source).unwrap();
        source.set_len(8 * 1024 * 1024).unwrap();
        write_extent(&mut source, 4096, b"first").unwrap();
        write_extent(&mut source, 7 * 1024 * 1024, b"last").unwrap();
        source.sync_all().unwrap();

        let mut destination = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&copy_path)
            .unwrap();
        copy(&mut source, &mut destination).unwrap();
        destination.sync_all().unwrap();
        assert_eq!(fs::metadata(&copy_path).unwrap().len(), 8 * 1024 * 1024);
        destination.seek(SeekFrom::Start(4096)).unwrap();
        let mut first = [0_u8; 5];
        destination.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"first");
        destination.seek(SeekFrom::Start(7 * 1024 * 1024)).unwrap();
        let mut last = [0_u8; 4];
        destination.read_exact(&mut last).unwrap();
        assert_eq!(&last, b"last");
    }

    #[test]
    fn freed_ranges_read_as_zero() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("range.bin");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(&vec![0x5a; 128 * 1024]).unwrap();
        zero_range(&mut file, 16 * 1024, 64 * 1024).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert!(bytes[..16 * 1024].iter().all(|byte| *byte == 0x5a));
        assert!(bytes[16 * 1024..80 * 1024].iter().all(|byte| *byte == 0));
        assert!(bytes[80 * 1024..].iter().all(|byte| *byte == 0x5a));
    }
}
