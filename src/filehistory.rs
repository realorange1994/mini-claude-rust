//! File history tracking for undo/rewind functionality

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use chrono::{DateTime, Utc};

/// A snapshot of a file before modification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub checksum: String,
}

/// On-disk snapshot format (written by harness agent)
#[derive(Debug, Deserialize)]
struct DiskSnapshot {
    file_path: String,
    content: String,
    timestamp: String,
}

/// FileHistory - tracks file modifications for undo/rewind
#[derive(Debug)]
pub struct FileHistory {
    snapshots: RwLock<HashMap<PathBuf, Vec<FileSnapshot>>>,
    max_snapshots: usize,
    snapshots_dir: Option<PathBuf>,
}

impl FileHistory {
    pub fn new() -> Self {
        Self {
            snapshots: RwLock::new(HashMap::new()),
            max_snapshots: 10,
            snapshots_dir: None,
        }
    }

    /// Create FileHistory with disk persistence
    pub fn new_with_dir(snapshots_dir: &Path) -> Self {
        let this = Self {
            snapshots: RwLock::new(HashMap::new()),
            max_snapshots: 10,
            snapshots_dir: Some(snapshots_dir.to_path_buf()),
        };
        this.load_from_disk();
        this
    }

    /// Load all snapshots from .claude/snapshots/ directory
    fn load_from_disk(&self) {
        let dir = match &self.snapshots_dir {
            Some(d) => d,
            None => return,
        };

        if !dir.is_dir() {
            return;
        }

        let mut map: HashMap<PathBuf, Vec<FileSnapshot>> = HashMap::new();

        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Try harness format first (file_path/content/timestamp)
            if let Ok(disk_snap) = serde_json::from_str::<DiskSnapshot>(&content) {
                let file_path = PathBuf::from(&disk_snap.file_path);
                let ts = chrono::DateTime::parse_from_rfc3339(&disk_snap.timestamp)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let checksum = format!("{:x}", md5_hash(&disk_snap.content));
                let snapshot = FileSnapshot {
                    path: file_path.clone(),
                    content: disk_snap.content,
                    timestamp: ts,
                    checksum,
                };
                map.entry(file_path).or_default().push(snapshot);
                continue;
            }

            // Try native FileSnapshot format
            if let Ok(snapshot) = serde_json::from_str::<FileSnapshot>(&content) {
                let file_path = snapshot.path.clone();
                map.entry(file_path).or_default().push(snapshot);
            }
        }

        // Sort each file's snapshots by timestamp and trim to max
        for snapshots in map.values_mut() {
            snapshots.sort_by_key(|s| s.timestamp);
            while snapshots.len() > self.max_snapshots {
                snapshots.remove(0);
            }
        }

        let mut store = self.snapshots.write().unwrap();
        *store = map;
    }

    /// Save a snapshot to disk
    fn save_to_disk(&self, snapshot: &FileSnapshot) {
        let dir = match &self.snapshots_dir {
            Some(d) => d,
            None => return,
        };

        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("[file_history] Failed to create snapshots dir: {}", e);
            return;
        }

        // Encode file path: remove : from drive, replace \ and / with _
        let encoded = snapshot.path.to_string_lossy()
            .replacen(':', "", 1)
            .replace('\\', "_")
            .replace('/', "_");
        let timestamp = snapshot.timestamp.format("%Y%m%dT%H%M%S");
        let filename = format!("{}_E__{}.json", timestamp, encoded);
        let file_path = dir.join(&filename);

        // Write in harness-compatible format
        let disk_snap = serde_json::json!({
            "file_path": snapshot.path.to_string_lossy(),
            "content": snapshot.content,
            "timestamp": snapshot.timestamp.to_rfc3339()
        });

        if let Err(e) = fs::write(&file_path, disk_snap.to_string()) {
            eprintln!("[file_history] Failed to write snapshot: {}", e);
        }
    }

    /// Take a snapshot of a file before modification
    pub fn snapshot(&self, path: &Path) -> std::io::Result<Option<FileSnapshot>> {
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(path)?;
        let checksum = format!("{:x}", md5_hash(&content));

        let mut snapshots = self.snapshots.write().unwrap();
        let file_snapshots = snapshots.entry(path.to_path_buf()).or_insert_with(Vec::new);

        // Don't snapshot if content hasn't changed
        if let Some(last) = file_snapshots.last() {
            if last.checksum == checksum {
                return Ok(None);
            }
        }

        let snapshot = FileSnapshot {
            path: path.to_path_buf(),
            content: content.clone(),
            timestamp: Utc::now(),
            checksum: checksum.clone(),
        };

        file_snapshots.push(snapshot.clone());

        // Drop lock before writing to disk
        drop(snapshots);

        // Persist to disk
        self.save_to_disk(&snapshot);

        // Trim old snapshots (both in-memory and on disk)
        let mut store = self.snapshots.write().unwrap();
        if let Some(file_snapshots) = store.get_mut(path) {
            while file_snapshots.len() > self.max_snapshots {
                file_snapshots.remove(0);
            }
        }

        Ok(Some(snapshot))
    }

    /// Restore the previous version of a file
    #[allow(dead_code)]
    pub fn restore(&self, path: &Path) -> std::io::Result<Option<String>> {
        let mut snapshots = self.snapshots.write().unwrap();

        if let Some(file_snapshots) = snapshots.get_mut(path) {
            if file_snapshots.len() >= 2 {
                // Remove current version, get previous
                file_snapshots.pop();
                if let Some(previous) = file_snapshots.last() {
                    fs::write(path, &previous.content)?;
                    return Ok(Some(previous.content.clone()));
                }
            }
        }

        Ok(None)
    }

    /// Rewind to a specific number of versions back
    #[allow(dead_code)]
    pub fn rewind(&self, path: &Path, steps: usize) -> std::io::Result<Option<String>> {
        let mut snapshots = self.snapshots.write().unwrap();

        if let Some(file_snapshots) = snapshots.get_mut(path) {
            let target_len = file_snapshots.len().saturating_sub(steps).max(1);

            if target_len < file_snapshots.len() {
                let target = file_snapshots[target_len - 1].content.clone();
                file_snapshots.truncate(target_len);
                fs::write(path, &target)?;
                return Ok(Some(target));
            }
        }

        Ok(None)
    }

    /// Get the number of snapshots for a file
    #[allow(dead_code)]
    pub fn count(&self, path: &Path) -> usize {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.get(path).map(|s| s.len()).unwrap_or(0)
    }

    /// Get all snapshots for a file (for history listing)
    pub fn get_snapshots(&self, path: &Path) -> Vec<FileSnapshot> {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.get(path).cloned().unwrap_or_default()
    }

    /// List all files that have history
    pub fn list_all_files(&self) -> Vec<PathBuf> {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.keys().cloned().collect()
    }

    /// Clear all snapshots for a file
    #[allow(dead_code)]
    pub fn clear(&self, path: &Path) {
        let mut snapshots = self.snapshots.write().unwrap();
        snapshots.remove(path);
    }

    /// Clear all snapshots
    #[allow(dead_code)]
    pub fn clear_all(&self) {
        let mut snapshots = self.snapshots.write().unwrap();
        snapshots.clear();
    }
}

impl Default for FileHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileHistory {
    fn clone(&self) -> Self {
        Self {
            snapshots: RwLock::new(self.snapshots.read().unwrap().clone()),
            max_snapshots: self.max_snapshots,
            snapshots_dir: self.snapshots_dir.clone(),
        }
    }
}

fn md5_hash(data: &str) -> u128 {
    // Simple hash function (not real MD5, but sufficient for change detection)
    let mut hash: u128 = 0;
    for (i, byte) in data.bytes().enumerate() {
        hash = hash.wrapping_add((byte as u128).wrapping_mul(i as u128 + 1));
        hash = hash.rotate_left(7);
    }
    hash
}
