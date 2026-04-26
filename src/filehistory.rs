//! File history tracking for undo/rewind functionality

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use chrono::{DateTime, Utc, Duration};

/// A snapshot of a file at a point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub checksum: String,
    /// Human-readable description of what changed (e.g., "edit_file: replace X with Y")
    #[serde(default)]
    pub description: String,
    /// Tombstone: source file was deleted. Disk JSON is kept for audit;
    /// the snapshot is excluded from queries and restore operations.
    #[serde(default)]
    pub deleted: bool,
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
    max_age: Option<Duration>,
    snapshots_dir: Option<PathBuf>,
}

/// Result of a line-by-line diff between two snapshots
#[derive(Debug, Clone)]
pub struct DiffResult {
    pub from_version: usize,
    pub to_version: usize,
    pub hunks: Vec<DiffHunk>,
}

/// A single hunk of diff output
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// Starting line number in the "from" version (1-indexed)
    pub from_line: usize,
    /// Number of lines in the "from" side of this hunk
    pub from_count: usize,
    /// Starting line number in the "to" version (1-indexed)
    pub to_line: usize,
    /// Number of lines in the "to" side of this hunk
    pub to_count: usize,
    /// Diff lines: "+" = added, "-" = removed, " " = context
    pub lines: Vec<String>,
}

/// Type of change to search for in file history
pub enum SearchMode {
    /// Find versions where text was added
    Added,
    /// Find versions where text was removed
    Removed,
    /// Find versions where text changed (added or removed)
    Changed,
}

impl FileHistory {
    pub fn new() -> Self {
        Self {
            snapshots: RwLock::new(HashMap::new()),
            max_snapshots: 50,
            max_age: Some(Duration::days(7)),
            snapshots_dir: None,
        }
    }

    /// Create FileHistory with disk persistence
    pub fn new_with_dir(snapshots_dir: &Path) -> Self {
        let this = Self {
            snapshots: RwLock::new(HashMap::new()),
            max_snapshots: 50,
            max_age: Some(Duration::days(7)),
            snapshots_dir: Some(snapshots_dir.to_path_buf()),
        };
        this.load_from_disk();
        this
    }

    // ─── Disk persistence ───

    fn load_from_disk(&self) {
        let dir = match &self.snapshots_dir {
            Some(d) => d,
            None => return,
        };

        if !dir.is_dir() {
            return;
        }

        let mut map: HashMap<PathBuf, Vec<FileSnapshot>> = HashMap::new();
        let now = Utc::now();
        let min_keep = 5; // always keep at least this many recent versions

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
                    description: String::new(),
                    deleted: false,
                };
                map.entry(file_path).or_default().push(snapshot);
                continue;
            }

            if let Ok(snapshot) = serde_json::from_str::<FileSnapshot>(&content) {
                let file_path = snapshot.path.clone();
                map.entry(file_path).or_default().push(snapshot);
            }
        }

        // Sort, trim by max_snapshots, and optionally trim by age
        for snapshots in map.values_mut() {
            snapshots.sort_by_key(|s| s.timestamp);

            // Backfill descriptions for legacy snapshots (loaded from disk without description)
            for (i, snap) in snapshots.iter_mut().enumerate() {
                if snap.description.is_empty() {
                    if i == 0 {
                        snap.description = if snap.content.is_empty() {
                            "empty (pre-modification snapshot)".to_string()
                        } else {
                            format!("initial ({} bytes)", snap.content.len())
                        };
                    } else {
                        snap.description = format!("v{}", i + 1);
                    }
                }
            }

            // Trim by max_snapshots
            while snapshots.len() > self.max_snapshots {
                snapshots.remove(0);
            }

            // Trim by age, but keep at least min_keep versions
            if let Some(max_age) = self.max_age {
                while snapshots.len() > min_keep {
                    let age = now - snapshots[0].timestamp;
                    if age > max_age {
                        snapshots.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }

        let mut store = self.snapshots.write().unwrap();
        *store = map;
    }

    fn save_to_disk(&self, snapshot: &FileSnapshot) {
        let dir = match &self.snapshots_dir {
            Some(d) => d,
            None => return,
        };

        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("[file_history] Failed to create snapshots dir: {}", e);
            return;
        }

        let encoded = snapshot.path.to_string_lossy()
            .replacen(':', "", 1)
            .replace('\\', "_")
            .replace('/', "_");
        let timestamp = snapshot.timestamp.format("%Y%m%dT%H%M%S%.3f");
        let filename = format!("{}_E__{}.json", timestamp, encoded);
        let file_path = dir.join(&filename);

        let disk_snap = serde_json::json!({
            "file_path": snapshot.path.to_string_lossy(),
            "content": snapshot.content,
            "timestamp": snapshot.timestamp.to_rfc3339(),
            "description": snapshot.description,
        });

        if let Err(e) = fs::write(&file_path, disk_snap.to_string()) {
            eprintln!("[file_history] Failed to write snapshot: {}", e);
        }
    }

    // ─── Core snapshot methods ───

    /// Snapshot a file BEFORE modification (pre-write snapshot)
    pub fn snapshot(&self, path: &Path) -> std::io::Result<Option<FileSnapshot>> {
        self.snapshot_with_desc(path, String::new())
    }

    /// Snapshot a file with a description of the change
    pub fn snapshot_with_desc(&self, path: &Path, desc: String) -> std::io::Result<Option<FileSnapshot>> {
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
            description: desc,
            deleted: false,
        };

        file_snapshots.push(snapshot.clone());
        drop(snapshots);

        self.save_to_disk(&snapshot);
        self.trim_snapshots(path);

        Ok(Some(snapshot))
    }

    /// Snapshot the current state of a file (used after creating new files)
    pub fn snapshot_current(&self, path: &Path) -> std::io::Result<Option<FileSnapshot>> {
        self.snapshot_current_with_desc(path, String::new())
    }

    pub fn snapshot_current_with_desc(&self, path: &Path, desc: String) -> std::io::Result<Option<FileSnapshot>> {
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(path)?;
        let checksum = format!("{:x}", md5_hash(&content));

        let mut snapshots = self.snapshots.write().unwrap();
        let file_snapshots = snapshots.entry(path.to_path_buf()).or_insert_with(Vec::new);

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
            description: desc,
            deleted: false,
        };

        file_snapshots.push(snapshot.clone());
        drop(snapshots);

        self.save_to_disk(&snapshot);
        self.trim_snapshots(path);

        Ok(Some(snapshot))
    }

    // ─── Restore and rewind ───

    /// Restore the previous version of a file (undo last change)
    pub fn restore(&self, path: &Path) -> std::io::Result<Option<String>> {
        self.restore_internal(path, 1)
    }

    /// Rewind N versions back
    pub fn rewind(&self, path: &Path, steps: usize) -> std::io::Result<Option<String>> {
        self.restore_internal(path, steps)
    }

    /// Checkout a specific version by writing its content to disk and recording a snapshot
    pub fn checkout(&self, path: &Path, version: usize) -> std::io::Result<Option<String>> {
        let target_content;
        {
            let snapshots = self.snapshots.read().unwrap();
            let file_snapshots = match snapshots.get(path) {
                Some(f) => f,
                None => return Ok(None),
            };
            // Filter to non-deleted, get version
            let active: Vec<&FileSnapshot> = file_snapshots.iter().filter(|s| !s.deleted).collect();
            if version < 1 || version > active.len() {
                return Ok(None);
            }
            target_content = active[version - 1].content.clone();
        }

        // Write target content to disk
        fs::write(path, &target_content)?;

        // Snapshot the checked-out state
        self.snapshot_current_with_desc(path, format!("checkout: v{}", version))?;

        Ok(Some(target_content))
    }

    fn restore_internal(&self, path: &Path, steps: usize) -> std::io::Result<Option<String>> {
        if steps == 0 {
            return Ok(None);
        }

        let mut snapshots = self.snapshots.write().unwrap();
        let file_snapshots = match snapshots.get_mut(path) {
            Some(f) => f,
            None => return Ok(None),
        };

        let len = file_snapshots.len();
        if len < 2 {
            return Ok(None);
        }

        // Collapse non-deleted snapshots into distinct content states (by checksum),
        // keeping the first occurrence of each unique checksum.
        // This makes restore step through *content changes*, not raw snapshot count.
        let mut distinct: Vec<(usize, String)> = Vec::new(); // (snapshot_idx, checksum)
        let mut seen_checksums = std::collections::HashSet::new();
        for i in 0..len {
            if file_snapshots[i].deleted {
                continue; // Skip tombstoned snapshots
            }
            let ck = file_snapshots[i].checksum.clone();
            if seen_checksums.insert(ck) {
                distinct.push((i, file_snapshots[i].checksum.clone()));
            }
        }

        if distinct.len() < 2 {
            return Ok(None); // Only one content state, nothing to restore
        }

        // The current state is the last distinct entry
        let current_idx = distinct.len() - 1;
        if steps > current_idx {
            return Ok(None); // Not enough distinct states to go back that far
        }

        let target_distinct_idx = current_idx - steps;
        let target_snap_idx = distinct[target_distinct_idx].0;
        let target_content = file_snapshots[target_snap_idx].content.clone();
        let target_checksum = distinct[target_distinct_idx].1.clone();
        let target_ver = target_snap_idx + 1;

        drop(snapshots);

        // Write target content to disk FIRST (so snapshot matches actual file)
        fs::write(path, &target_content)?;

        // Then snapshot the restored state (content matches what's on disk)
        let restore_snapshot = FileSnapshot {
            path: path.to_path_buf(),
            content: target_content.clone(),
            timestamp: Utc::now(),
            checksum: target_checksum,
            description: format!("restore: to v{}", target_ver),
            deleted: false,
        };

        {
            let mut snapshots = self.snapshots.write().unwrap();
            if let Some(file_snapshots) = snapshots.get_mut(path) {
                file_snapshots.push(restore_snapshot.clone());
            }
        }

        self.save_to_disk(&restore_snapshot);
        self.trim_snapshots(path);

        Ok(Some(target_content))
    }

    // ─── Tags ───

    /// Add a tag to the current (latest non-deleted) snapshot of a file
    pub fn add_tag(&self, path: &Path, tag: &str) -> bool {
        let mut snapshots = self.snapshots.write().unwrap();
        let file_snapshots = match snapshots.get_mut(path) {
            Some(f) if !f.is_empty() => f,
            _ => return false,
        };
        // Find the last non-deleted snapshot
        let last = file_snapshots.iter_mut().rev().find(|s| !s.deleted);
        let last = match last {
            Some(s) => s,
            None => return false,
        };
        if !last.description.is_empty() {
            last.description = format!("{} [{}]", last.description, tag);
        } else {
            last.description = format!("[{}]", tag);
        }
        true
    }

    /// List all tags across all non-deleted snapshots for a file
    pub fn list_tags(&self, path: &Path) -> Vec<(usize, String)> {
        let snapshots = self.snapshots.read().unwrap();
        let file_snapshots = match snapshots.get(path) {
            Some(f) => f,
            None => return Vec::new(),
        };
        let mut tags = Vec::new();
        let mut ver = 0;
        for snap in file_snapshots.iter() {
            if snap.deleted { continue; }
            ver += 1;
            // Extract tags from description: text in square brackets
            if let Some(start) = snap.description.find('[') {
                if let Some(end) = snap.description[start..].find(']') {
                    tags.push((ver, snap.description[start + 1..start + end].to_string()));
                }
            }
        }
        tags
    }

    // ─── Diff ───

    /// Compute a line-by-line diff between two versions of a file
    /// version numbers are 1-indexed
    pub fn diff(&self, path: &Path, from_version: usize, to_version: usize) -> Option<DiffResult> {
        let snapshots = self.snapshots.read().unwrap();
        let file_snapshots = snapshots.get(path)?;
        let active: Vec<&FileSnapshot> = file_snapshots.iter().filter(|s| !s.deleted).collect();

        if from_version < 1 || to_version < 1 || from_version > active.len() || to_version > active.len() {
            return None;
        }

        let from_lines: Vec<&str> = active[from_version - 1].content.lines().collect();
        let to_lines: Vec<&str> = active[to_version - 1].content.lines().collect();

        let lcs = longest_common_subsequence(&from_lines, &to_lines);
        let hunks = compute_hunks(&from_lines, &to_lines, &lcs);

        Some(DiffResult {
            from_version,
            to_version,
            hunks,
        })
    }

    /// Search for versions where text was added, removed, or changed
    pub fn search(&self, path: &Path, pattern: &str, mode: SearchMode, ignore_case: bool) -> Vec<(usize, String)> {
        let snapshots = self.snapshots.read().unwrap();
        let file_snapshots = match snapshots.get(path) {
            Some(f) => f,
            None => return Vec::new(),
        };
        let active: Vec<&FileSnapshot> = file_snapshots.iter().filter(|s| !s.deleted).collect();
        if active.len() < 2 {
            return Vec::new();
        }

        let re_pattern = if ignore_case {
            format!("(?i){}", regex::escape(pattern))
        } else {
            regex::escape(pattern)
        };
        let re = match regex::Regex::new(&re_pattern) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let mut results = Vec::new();
        for i in 1..active.len() {
            let prev_lines: std::collections::HashSet<&str> = active[i - 1].content.lines().collect();
            let curr_lines: std::collections::HashSet<&str> = active[i].content.lines().collect();

            let added: Vec<&&str> = curr_lines.difference(&prev_lines).collect();
            let removed: Vec<&&str> = prev_lines.difference(&curr_lines).collect();

            let matches = match mode {
                SearchMode::Added => added.iter().any(|l| re.is_match(l)),
                SearchMode::Removed => removed.iter().any(|l| re.is_match(l)),
                SearchMode::Changed => added.iter().any(|l| re.is_match(l)) || removed.iter().any(|l| re.is_match(l)),
            };

            if matches {
                let mut details = Vec::new();
                if !added.is_empty() {
                    for line in added.iter().take(3).filter(|l| re.is_match(**l)) {
                        details.push(format!("+ {}", line));
                    }
                }
                if !removed.is_empty() {
                    for line in removed.iter().take(3).filter(|l| re.is_match(**l)) {
                        details.push(format!("- {}", line));
                    }
                }
                results.push((i + 1, details.join("\n")));
            }
        }
        results
    }

    // ─── Summary and timeline ───

    /// Get a summary of all files changed, optionally filtered by time
    pub fn get_summary(&self, since: Option<DateTime<Utc>>) -> Vec<(PathBuf, Vec<FileSnapshot>)> {
        let snapshots = self.snapshots.read().unwrap();
        let mut result: Vec<(PathBuf, Vec<FileSnapshot>)> = Vec::new();

        for (path, snaps) in snapshots.iter() {
            let filtered: Vec<FileSnapshot> = snaps.iter()
                .filter(|s| !s.deleted)
                .filter(|s| since.map(|t| s.timestamp >= t).unwrap_or(true))
                .cloned()
                .collect();
            if !filtered.is_empty() {
                result.push((path.clone(), filtered));
            }
        }

        // Sort by latest change
        result.sort_by(|a, b| {
            let a_latest = a.1.last().map(|s| s.timestamp).unwrap_or(DateTime::default());
            let b_latest = b.1.last().map(|s| s.timestamp).unwrap_or(DateTime::default());
            b_latest.cmp(&a_latest)
        });

        result
    }

    /// Get a flat timeline of all changes across all files, optionally filtered by time
    pub fn get_timeline(&self, since: Option<DateTime<Utc>>) -> Vec<(DateTime<Utc>, PathBuf, usize, String)> {
        let snapshots = self.snapshots.read().unwrap();
        let mut entries: Vec<(DateTime<Utc>, PathBuf, usize, String)> = Vec::new();

        for (path, snaps) in snapshots.iter() {
            let mut ver = 0;
            for snap in snaps.iter() {
                if snap.deleted { continue; }
                ver += 1;
                if since.map(|s| snap.timestamp >= s).unwrap_or(true) {
                    let desc = if snap.description.is_empty() {
                        format!("v{} ({} bytes)", ver, snap.content.len())
                    } else {
                        format!("v{}: {} ({} bytes)", ver, snap.description, snap.content.len())
                    };
                    entries.push((snap.timestamp, path.clone(), ver, desc));
                }
            }
        }

        entries.sort_by_key(|e| e.0);
        entries
    }

    // ─── Resolve version specifier ───

    /// Resolve a version specifier string to a 1-indexed version number.
    /// Supported formats:
    /// - "v3" or "3" → version 3
    /// - "current" or "latest" → last version
    /// - "last2" → 2 versions back from current
    /// - "tagname" → version with matching tag in description
    pub fn resolve_version(&self, path: &Path, spec: &str) -> Option<usize> {
        let snapshots = self.snapshots.read().unwrap();
        let file_snapshots = snapshots.get(path)?;

        // Build non-deleted view — this is what users see as "v1, v2, v3..."
        let active: Vec<&FileSnapshot> = file_snapshots.iter().filter(|s| !s.deleted).collect();
        let total = active.len();
        if total == 0 {
            return None;
        }

        if spec == "current" || spec == "latest" {
            return Some(total);
        }

        // "lastN" pattern
        if let Some(rest) = spec.strip_prefix("last") {
            if let Ok(n) = rest.parse::<usize>() {
                if n > 0 && n < total {
                    return Some(total - n);
                }
            }
        }

        // "vN" or "N" pattern
        let num_str = spec.strip_prefix('v').unwrap_or(spec);
        if let Ok(n) = num_str.parse::<usize>() {
            if n > 0 && n <= total {
                return Some(n);
            }
        }

        // Try to match tag in description (only non-deleted)
        for (i, snap) in active.iter().enumerate() {
            if snap.description.contains(spec) {
                return Some(i + 1);
            }
        }

        None
    }

    // ─── Basic accessors ───

    pub fn count(&self, path: &Path) -> usize {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.get(path).map(|s| s.iter().filter(|s| !s.deleted).count()).unwrap_or(0)
    }

    pub fn get_snapshots(&self, path: &Path) -> Vec<FileSnapshot> {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.get(path)
            .map(|s| s.iter().filter(|s| !s.deleted).cloned().collect())
            .unwrap_or_default()
    }

    pub fn list_all_files(&self) -> Vec<PathBuf> {
        let snapshots = self.snapshots.read().unwrap();
        snapshots.keys().cloned().collect()
    }

    pub fn clear(&self, path: &Path) {
        let mut snapshots = self.snapshots.write().unwrap();
        snapshots.remove(path);
    }

    pub fn clear_all(&self) {
        let mut snapshots = self.snapshots.write().unwrap();
        snapshots.clear();
    }

    /// Clear history for all tracked files under a directory (for rmrf)
    pub fn clear_under_dir(&self, dir: &Path) {
        let mut snapshots = self.snapshots.write().unwrap();
        snapshots.retain(|path, _| !path.starts_with(dir));
    }

    // ─── Internal helpers ───

    fn trim_snapshots(&self, path: &Path) {
        let mut store = self.snapshots.write().unwrap();
        if let Some(file_snapshots) = store.get_mut(path) {
            let now = Utc::now();
            let min_keep = 5;

            // Trim by max_snapshots
            while file_snapshots.len() > self.max_snapshots {
                file_snapshots.remove(0);
            }

            // Trim by age
            if let Some(max_age) = self.max_age {
                while file_snapshots.len() > min_keep {
                    let age = now - file_snapshots[0].timestamp;
                    if age > max_age {
                        file_snapshots.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }
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
            max_age: self.max_age,
            snapshots_dir: self.snapshots_dir.clone(),
        }
    }
}

// ─── Diff algorithms ───

/// Compute the longest common subsequence of two slices of lines.
/// Returns (from_idx, to_idx) pairs for matching lines.
fn longest_common_subsequence<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(usize, usize)> {
    let m = a.len();
    let n = b.len();

    if m == 0 || n == 0 || m * n > 1_000_000 {
        // For very large files, use a simpler approach
        return Vec::new();
    }

    // Use a 2-row DP to save memory
    let mut dp: Vec<Vec<usize>> = vec![vec![0; n + 1]; 2];

    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                dp[i % 2][j] = dp[(i - 1) % 2][j - 1] + 1;
            } else {
                dp[i % 2][j] = std::cmp::max(dp[(i - 1) % 2][j], dp[i % 2][j - 1]);
            }
        }
    }

    // Backtrack to reconstruct the LCS
    let mut result = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            result.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[(i - 1) % 2][j] >= dp[i % 2][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    result.reverse();
    result
}

/// Compute diff hunks from the LCS
fn compute_hunks(from_lines: &[&str], to_lines: &[&str], lcs: &[(usize, usize)]) -> Vec<DiffHunk> {
    if lcs.is_empty() {
        // Entire file changed
        let mut lines = Vec::new();
        for line in from_lines {
            lines.push(format!("- {}", line));
        }
        for line in to_lines {
            lines.push(format!("+ {}", line));
        }
        if !lines.is_empty() {
            return vec![DiffHunk {
                from_line: 1,
                from_count: from_lines.len(),
                to_line: 1,
                to_count: to_lines.len(),
                lines,
            }];
        }
        return Vec::new();
    }

    let mut hunks = Vec::new();
    let mut hunk_lines = Vec::new();
    let mut hunk_from_start: usize = 0;
    let mut hunk_to_start: usize = 0;
    let mut from_idx: usize = 0;
    let mut to_idx: usize = 0;
    let context = 3;

    for &(lcs_from, lcs_to) in lcs {
        // Lines before this LCS match
        let removed_count = lcs_from.saturating_sub(from_idx);
        let added_count = lcs_to.saturating_sub(to_idx);

        if removed_count > 0 || added_count > 0 {
            if hunk_lines.is_empty() {
                // Add context from before
                let ctx_start = from_idx.saturating_sub(context);
                for k in ctx_start..from_idx {
                    hunk_lines.push(format!("  {}", from_lines[k]));
                }
                hunk_from_start = ctx_start;
                hunk_to_start = to_idx.saturating_sub(from_idx.saturating_sub(ctx_start));
                from_idx = ctx_start;
            }

            // Removed lines
            for k in from_idx..lcs_from {
                hunk_lines.push(format!("- {}", from_lines[k]));
            }
            // Added lines
            for k in to_idx..lcs_to {
                hunk_lines.push(format!("+ {}", to_lines[k]));
            }

            from_idx = lcs_from;
            to_idx = lcs_to;
        } else if !hunk_lines.is_empty() {
            // Add context and close hunk
            let ctx_end = std::cmp::min(lcs_from + context, from_lines.len().min(to_lines.len()));
            for k in lcs_from..ctx_end {
                if k < from_lines.len() && k < to_lines.len() && from_lines[k] == to_lines[k] {
                    hunk_lines.push(format!("  {}", from_lines[k]));
                }
            }
            if !hunk_lines.is_empty() {
                let fc = hunk_lines.iter().filter(|l| l.starts_with('-') || l.starts_with(' ')).count();
                let tc = hunk_lines.iter().filter(|l| l.starts_with('+') || l.starts_with(' ')).count();
                hunks.push(DiffHunk {
                    from_line: hunk_from_start.saturating_add(1),
                    from_count: fc,
                    to_line: hunk_to_start.saturating_add(1),
                    to_count: tc,
                    lines: std::mem::take(&mut hunk_lines),
                });
            }
            from_idx = lcs_from + 1;
            to_idx = lcs_to + 1;
        } else {
            from_idx = lcs_from + 1;
            to_idx = lcs_to + 1;
        }
    }

    // Trailing changes after last LCS match
    let trailing_from = from_lines.len().saturating_sub(from_idx);
    let trailing_to = to_lines.len().saturating_sub(to_idx);
    if trailing_from > 0 || trailing_to > 0 {
        // Add trailing context
        if hunk_lines.is_empty() {
            let ctx_start = from_idx.saturating_sub(context);
            for k in ctx_start..from_idx {
                hunk_lines.push(format!("  {}", from_lines[k]));
            }
            hunk_from_start = ctx_start;
            hunk_to_start = to_idx.saturating_sub(from_idx.saturating_sub(ctx_start));
            from_idx = ctx_start;
        }
        for k in from_idx..from_lines.len() {
            hunk_lines.push(format!("- {}", from_lines[k]));
        }
        for k in to_idx..to_lines.len() {
            hunk_lines.push(format!("+ {}", to_lines[k]));
        }
    }

    if !hunk_lines.is_empty() {
        let fc = hunk_lines.iter().filter(|l| l.starts_with('-') || l.starts_with(' ')).count();
        let tc = hunk_lines.iter().filter(|l| l.starts_with('+') || l.starts_with(' ')).count();
        hunks.push(DiffHunk {
            from_line: hunk_from_start.saturating_add(1),
            from_count: fc,
            to_line: hunk_to_start.saturating_add(1),
            to_count: tc,
            lines: hunk_lines,
        });
    }

    hunks
}

fn md5_hash(data: &str) -> u128 {
    let mut hash: u128 = 0;
    for (i, byte) in data.bytes().enumerate() {
        hash = hash.wrapping_add((byte as u128).wrapping_mul(i as u128 + 1));
        hash = hash.rotate_left(7);
    }
    hash
}
