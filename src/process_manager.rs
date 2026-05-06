//! Process management utilities: tree-kill, process group management, and parent-death signals.
//!
//! Provides cross-platform (Unix/Windows) process lifecycle helpers.

/// Kill a process and all its children by sending SIGKILL to the process group.
///
/// On Unix: sends SIGKILL to `-pid` (the process group).
/// Falls back to single-process kill on ESRCH (process group doesn't exist).
///
/// On Windows: uses `taskkill /F /T /PID` which kills the process tree.
pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        // First try: kill the entire process group (tree-kill)
        let ret = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                // Process group already gone — fall back to single-process kill
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
            // Other errors (EPERM, etc.) are logged but not propagated
        }
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(&["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
}

/// Set the parent-death signal for a child process on Linux.
///
/// When the calling process exits, the child receives `signal`
/// (default: SIGTERM). This ensures background processes don't become orphans
/// if the agent is interrupted.
///
/// Uses `libc::prctl(PR_SET_PDEATHSIG, signal)` called in a child hook
/// before `exec`.
//
//  Safety: This is called in a pre_exec context (child process, before exec).
//  It only modifies the child process's own signal disposition.
pub fn install_pdeath_signal(signal: libc::c_int) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl is only called in the child after fork, before exec.
        // This is the standard pattern for parent-death signals.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, signal);
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = signal; // suppress unused warning
    }
}

/// Install parent-death signal (SIGTERM) for a child process.
/// Convenience wrapper around `install_pdeath_signal(SIGTERM)`.
pub fn install_sigterm_on_parent_death() {
    install_pdeath_signal(libc::SIGTERM);
}

/// RAII guard that kills a process (group) when dropped.
/// Use with `std::process::Command::before_exec()` on Unix.
pub struct ProcessGuard {
    pid: Option<u32>,
    killed: bool,
}

impl ProcessGuard {
    pub fn new(pid: u32) -> Self {
        Self {
            pid: Some(pid),
            killed: false,
        }
    }

    /// Mark the process as successfully killed.
    pub fn mark_killed(&mut self) {
        self.killed = true;
    }

    /// Consume the guard without killing (process already exited).
    pub fn forget(mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            if !self.killed {
                kill_process_group(pid);
            }
        }
    }
}
