//! Session cleanup utilities.
//! Ported from upstream cleanup.go (157 lines).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Cleanup manager for stale session files.
pub struct CleanupManager {
    cutoff_days: i64,
    project_dir: PathBuf,
}

impl CleanupManager {
    /// Create a cleanup manager with a 30-day default cutoff.
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            cutoff_days: 30,
            project_dir: project_dir.into(),
        }
    }

    /// Set the retention period in days.
    pub fn set_cutoff(&mut self, days: i64) {
        if days > 0 {
            self.cutoff_days = days;
        }
    }

    /// Perform cleanup of stale files in .claude/ directories.
    /// Returns the number of files removed.
    pub fn run(&self) -> std::io::Result<usize> {
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(self.cutoff_days as u64 * 86400))
            .unwrap_or(SystemTime::now());
        let mut removed = 0;

        let dirs = [
            self.project_dir.join(".claude").join("transcripts"),
            self.project_dir.join(".claude").join("plans"),
            self.project_dir.join(".claude").join("sessions"),
        ];

        for dir in &dirs {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if modified < cutoff {
                                let _ = fs::remove_file(entry.path());
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }

        // Clean stale .bak files (notebook backups)
        removed += self.remove_stale_pattern("*.bak", &cutoff);

        // Clean stale .tmp.* files (atomic write leftovers)
        let one_day = SystemTime::now()
            .checked_sub(Duration::from_secs(86400))
            .unwrap_or(SystemTime::now());
        removed += self.remove_stale_tmp_files(&one_day);

        Ok(removed)
    }

    /// Remove files matching a glob pattern that are older than cutoff.
    fn remove_stale_pattern(&self, pattern: &str, cutoff: &SystemTime) -> usize {
        let mut removed = 0;
        if let Ok(paths) = glob::glob(&self.project_dir.join("**").join(pattern).to_string_lossy()) {
            for entry in paths.flatten() {
                if let Ok(meta) = fs::metadata(&entry) {
                    if let Ok(modified) = meta.modified() {
                        if modified < *cutoff {
                            let _ = fs::remove_file(&entry);
                            removed += 1;
                        }
                    }
                }
            }
        }
        removed
    }

    /// Remove .tmp.* files older than the given cutoff.
    fn remove_stale_tmp_files(&self, cutoff: &SystemTime) -> usize {
        let mut removed = 0;

        // Root directory
        if let Ok(paths) =
            glob::glob(&self.project_dir.join("*.tmp.*").to_string_lossy())
        {
            for entry in paths.flatten() {
                if is_stale_file(&entry, cutoff) {
                    let _ = fs::remove_file(&entry);
                    removed += 1;
                }
            }
        }

        // Subdirectories (one level deep, skip internal dirs)
        if let Ok(subdirs) = fs::read_dir(&self.project_dir) {
            for subdir in subdirs.flatten() {
                let path = subdir.path();
                if !path.is_dir() {
                    continue;
                }
                let name = path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
                if name == ".claude" || name == "node_modules" {
                    continue;
                }
                let pattern = path.join("*.tmp.*");
                if let Ok(paths) = glob::glob(&pattern.to_string_lossy()) {
                    for entry in paths.flatten() {
                        if is_stale_file(&entry, cutoff) {
                            let _ = fs::remove_file(&entry);
                            removed += 1;
                        }
                    }
                }
            }
        }

        removed
    }
}

/// Check if a file is older than the given cutoff.
fn is_stale_file(path: &Path, cutoff: &SystemTime) -> bool {
    if let Ok(meta) = fs::metadata(path) {
        if let Ok(modified) = meta.modified() {
            return modified < *cutoff;
        }
    }
    false
}

/// Remove .tmp.* files older than 1 day. Called at startup.
pub fn cleanup_stale_temp_files(project_dir: impl AsRef<Path>) -> usize {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(86400))
        .unwrap_or(SystemTime::now());

    let mut removed = 0;
    let dir = project_dir.as_ref();

    // Root level
    if let Ok(paths) = glob::glob(&dir.join("*.tmp.*").to_string_lossy()) {
        for entry in paths.flatten() {
            if is_stale_file(&entry, &cutoff) {
                let _ = fs::remove_file(&entry);
                removed += 1;
            }
        }
    }

    // One level deep
    if let Ok(subdirs) = fs::read_dir(dir) {
        for subdir in subdirs.flatten() {
            let path = subdir.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
            if name == ".claude" || name == "node_modules" {
                continue;
            }
            let pattern = path.join("*.tmp.*");
            if let Ok(paths) = glob::glob(&pattern.to_string_lossy()) {
                for entry in paths.flatten() {
                    if is_stale_file(&entry, &cutoff) {
                        let _ = fs::remove_file(&entry);
                        removed += 1;
                    }
                }
            }
        }
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_is_stale_file() {
        let dir = std::env::temp_dir().join("test_stale_file");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.txt");
        {
            let mut f = fs::File::create(&path).unwrap();
            writeln!(f, "test").unwrap();
        }
        // Fresh file should not be stale
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(86400))
            .unwrap();
        assert!(!is_stale_file(&path, &cutoff));

        // Set file modification time to past
        let past = SystemTime::now().checked_sub(Duration::from_secs(172800)).unwrap();
        let _ = fs::File::open(&path).and_then(|f| f.set_modified(past));
        assert!(is_stale_file(&path, &cutoff));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cleanup_manager_new() {
        let cm = CleanupManager::new("/tmp/test");
        assert_eq!(cm.cutoff_days, 30);
    }
}
