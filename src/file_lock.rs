//! File locking utilities for cross-platform file-based locks.
//! Ported from upstream file_lock_unix.go (85 lines).
//!
//! Uses OS-level file locks to prevent concurrent access to shared resources
//! like the message file or the session state.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// A file-based lock that is automatically released when dropped.
pub struct FileLock {
    file: Option<File>,
    path: PathBuf,
}

impl FileLock {
    /// Try to acquire an exclusive lock on the file.
    /// Returns Ok(FileLock) if the lock was acquired, Err otherwise.
    /// The lock is released when the FileLock is dropped.
    pub fn try_lock(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        // Try to acquire an exclusive lock
        if try_acquire_lock(&file)? {
            Ok(Self {
                file: Some(file),
                path,
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("lock is held by another process: {}", path.display()),
            ))
        }
    }

    /// Try to acquire a shared (read) lock on the file.
    pub fn try_lock_shared(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        if try_acquire_shared_lock(&file)? {
            Ok(Self {
                file: Some(file),
                path,
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("shared lock is held exclusively: {}", path.display()),
            ))
        }
    }

    /// Get the lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Explicitly release the lock.
    pub fn unlock(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.take() {
            release_lock(&file)?;
        }
        Ok(())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Release the lock by dropping the file handle
            // On Unix, flock is released automatically on close
            // On Windows, the LockFileEx is released on close
            let _ = release_lock(&file);
        }
    }
}

// ============================================================================
// Platform-specific lock implementations
// ============================================================================

#[cfg(unix)]
fn try_acquire_lock(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    Ok(result == 0)
}

#[cfg(unix)]
fn try_acquire_shared_lock(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) };
    Ok(result == 0)
}

#[cfg(unix)]
fn release_lock(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_rawFd();
    let result = unsafe { libc::flock(fd, libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_acquire_lock(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as HANDLE;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };

    let result = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };

    Ok(result != 0)
}

#[cfg(windows)]
fn try_acquire_shared_lock(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::LOCKFILE_FAIL_IMMEDIATELY;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as HANDLE;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };

    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::LockFileEx(
            handle,
            LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };

    Ok(result != 0)
}

#[cfg(windows)]
fn release_lock(file: &File) -> io::Result<()> {
    // On Windows, the lock is released when the file handle is closed
    // (which happens when File is dropped). No explicit unlock needed.
    let _ = file;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn try_acquire_lock(file: &File) -> io::Result<bool> {
    // Fallback: no real locking, just succeed
    Ok(true)
}

#[cfg(not(any(unix, windows)))]
fn try_acquire_shared_lock(file: &File) -> io::Result<bool> {
    Ok(true)
}

#[cfg(not(any(unix, windows)))]
fn release_lock(file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_lock_acquire_release() {
        let dir = std::env::temp_dir().join("cc_lock_test");
        let _ = fs::create_dir_all(&dir);
        let lock_path = dir.join("test.lock");

        // Acquire lock
        let lock = FileLock::try_lock(&lock_path).expect("should acquire lock");
        assert!(lock.path().exists());

        // Second lock should fail
        let result = FileLock::try_lock(&lock_path);
        assert!(result.is_err());

        // Release
        drop(lock);

        // Should be able to acquire again
        let lock2 = FileLock::try_lock(&lock_path).expect("should acquire lock after release");
        drop(lock2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_file_lock_shared() {
        let dir = std::env::temp_dir().join("cc_lock_shared_test");
        let _ = fs::create_dir_all(&dir);
        let lock_path = dir.join("test_shared.lock");

        // Two shared locks should coexist
        let lock1 = FileLock::try_lock_shared(&lock_path).expect("should acquire shared lock 1");
        let lock2 = FileLock::try_lock_shared(&lock_path).expect("should acquire shared lock 2");

        drop(lock1);
        drop(lock2);

        let _ = fs::remove_dir_all(&dir);
    }
}