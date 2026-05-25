//! Interruptible stdin readline with context cancellation.
//! Ported from upstream tools/readline_unix.go (68 lines),
//! tools/readline_windows.go (99 lines).
//!
//! On Unix, reads from stdin with periodic timeout checks.
//! On Windows, uses WaitForSingleObject for polling stdin.

use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Read a line from stdin with interrupt support.
/// Checks the interrupt flag every 100ms.
/// Returns the line read (without newline) or an error if interrupted.
pub fn read_line_with_interrupt(interrupted: &Arc<AtomicBool>) -> io::Result<String> {
    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        if interrupted.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "readline interrupted"));
        }

        // Try to read with timeout via non-blocking approach
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = stdin.as_raw_fd();

            // Use select to poll with 100ms timeout
            let mut tv = libc::timeval {
                tv_sec: 0,
                tv_usec: 100_000, // 100ms
            };
            let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(fd, &mut fds);

            let result = unsafe {
                libc::select(
                    fd + 1,
                    &mut fds,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut tv,
                )
            };

            if result < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EINTR {
                    continue;
                }
                return Err(io::Error::last_os_error());
            }

            if result > 0 && libc::FD_ISSET(fd, &mut fds) {
                line.clear();
                stdin.lock().read_line(&mut line)?;
                // Strip trailing newline
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                return Ok(line);
            }
            // Timeout — loop back to check interrupt flag
            continue;
        }

        #[cfg(windows)]
        {
            // On Windows, use a simple sleep-and-check approach
            // Full WaitForSingleObject integration requires unsafe win32 calls
            // which are available via the windows-sys crate. For simplicity,
            // we use a blocking read with a short timeout loop.
            let _stdin_handle = stdin.lock();

            // Try non-blocking read attempt
            // std::io::Stdin is blocking, so we use a separate thread approach
            line.clear();

            // Spawn a reader thread to allow interrupt checking
            let (tx, rx) = std::sync::mpsc::channel();
            let _interrupted_clone = interrupted.clone();

            std::thread::spawn(move || {
                let mut buf = String::new();
                let result = io::stdin().lock().read_line(&mut buf);
                let _ = tx.send((result, buf));
            });

            loop {
                if interrupted.load(Ordering::SeqCst) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "readline interrupted",
                    ));
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok((result, buf)) => {
                        let mut buf = buf;
                        if buf.ends_with('\n') {
                            buf.pop();
                            if buf.ends_with('\r') {
                                buf.pop();
                            }
                        }
                        return result.map(|_| buf);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "readline thread disconnected",
                        ));
                    }
                }
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            // Fallback: simple blocking read
            line.clear();
            stdin.lock().read_line(&mut line)?;
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            return Ok(line);
        }
    }
}

/// Blocking readline without interrupt support (fallback).
pub fn read_line_blocking() -> io::Result<String> {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_line_interrupted() {
        let interrupted = Arc::new(AtomicBool::new(true));
        let result = read_line_with_interrupt(&interrupted);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
}