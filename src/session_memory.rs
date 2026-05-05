//! Session memory — persistent structured notes across conversation turns.
//!
//! Ported from the Go Phase 4 implementation:
//! - MemoryEntry with category, content, timestamp, source
//! - SessionMemory with CRUD, disk persistence, background flush
//! - Markdown storage format compatible with the Go version

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Categories for memory entries.
pub const VALID_CATEGORIES: &[&str] = &["preference", "decision", "state", "reference"];

/// A single memory note.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub category: String,  // "preference" | "decision" | "state" | "reference"
    pub content: String,   // the actual note text
    pub timestamp: String, // RFC3339 timestamp
    pub source: String,    // "user" | "assistant" | "auto" | "disk"
}

/// Manages structured notes that persist across the session.
/// Runs a background thread that periodically flushes notes to disk.
/// All methods take `&self` (interior mutability) so it works behind `Arc`.
pub struct SessionMemory {
    inner: Arc<Mutex<SessionMemoryInner>>,
    stop_flag: Arc<AtomicBool>,
    flush_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for SessionMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("SessionMemory")
            .field("entries_count", &guard.entries.len())
            .field("project_dir", &guard.project_dir)
            .field("file_path", &guard.file_path)
            .field("dirty", &guard.dirty)
            .finish()
    }
}

struct SessionMemoryInner {
    entries: Vec<MemoryEntry>,
    project_dir: PathBuf,
    file_path: PathBuf,
    dirty: bool,
    max_entries: usize,
    /// Callback invoked when a note is added, used to mark the system prompt dirty.
    on_add: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SessionMemory {
    /// Creates a new SessionMemory for the given project directory.
    /// Loads existing entries from disk if present.
    pub fn new(project_dir: &Path) -> Self {
        let file_path = project_dir.join(".claude").join("session_memory.md");
        let mut inner = SessionMemoryInner {
            entries: Vec::new(),
            project_dir: project_dir.to_path_buf(),
            file_path,
            dirty: false,
            max_entries: 100,
            on_add: None,
        };
        inner.load_from_disk();

        let inner = Arc::new(Mutex::new(inner));
        let stop_flag = Arc::new(AtomicBool::new(false));

        Self {
            inner,
            stop_flag,
            flush_handle: Mutex::new(None),
        }
    }

    /// Sets the callback invoked when a note is added.
    pub fn set_on_add<F>(&self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.on_add = Some(Arc::new(callback));
    }

    /// Adds a new memory entry and marks the memory as dirty.
    /// If an entry with the same category+content exists, its timestamp is updated.
    pub fn add_note(&self, category: &str, content: &str, source: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = chrono::Utc::now().to_rfc3339();

        // Deduplicate: if same category+content exists, update timestamp
        for entry in guard.entries.iter_mut() {
            if entry.category == category && entry.content == content {
                entry.timestamp = now;
                guard.dirty = true;
                let cb = guard.on_add.clone();
                drop(guard);
                if let Some(cb) = cb {
                    cb();
                }
                return;
            }
        }

        guard.entries.push(MemoryEntry {
            category: category.to_string(),
            content: content.to_string(),
            timestamp: now,
            source: source.to_string(),
        });

        // Enforce max entries (keep newest)
        if guard.entries.len() > guard.max_entries {
            let drain_to = guard.entries.len() - guard.max_entries;
            guard.entries.drain(..drain_to);
        }

        guard.dirty = true;
        let cb = guard.on_add.clone();
        drop(guard);
        if let Some(cb) = cb {
            cb();
        }
    }

    /// Returns all memory entries, sorted by category then timestamp (newest first within each category).
    pub fn get_notes(&self) -> Vec<MemoryEntry> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = guard.entries.clone();
        result.sort_by(|a, b| {
            match a.category.cmp(&b.category) {
                std::cmp::Ordering::Equal => b.timestamp.cmp(&a.timestamp),
                other => other,
            }
        });
        result
    }

    /// Returns memory entries whose content or category contains the query (case-insensitive).
    pub fn search_notes(&self, query: &str) -> Vec<MemoryEntry> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let lower = query.to_lowercase();
        guard
            .entries
            .iter()
            .filter(|e| {
                e.content.to_lowercase().contains(&lower)
                    || e.category.to_lowercase().contains(&lower)
            })
            .cloned()
            .collect()
    }

    /// Formats memory entries for injection into the system prompt.
    /// Returns an empty string if there are no entries.
    pub fn format_for_prompt(&self) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.entries.is_empty() {
            return String::new();
        }

        // Group by category
        let mut groups: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();
        for entry in &guard.entries {
            groups.entry(entry.category.clone()).or_default().push(entry);
        }

        let mut categories: Vec<String> = groups.keys().cloned().collect();
        categories.sort();

        let mut result = String::from("## Session Memory\n\n");
        result.push_str(
            "The following notes were recorded during this or previous sessions. Use them as context.\n\n",
        );

        for cat in &categories {
            result.push_str(&format!("### {}\n", cat));
            if let Some(entries) = groups.get(cat) {
                for entry in entries {
                    result.push_str(&format!("- {}\n", entry.content));
                }
            }
            result.push('\n');
        }

        result
    }

    /// Returns true if there are no memory entries.
    pub fn is_empty(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.is_empty()
    }

    /// Starts a background thread that periodically flushes memory to disk.
    /// Call `stop()` to terminate.
    pub fn start_flush_loop(&self) {
        let inner = Arc::clone(&self.inner);
        let stop_flag = Arc::clone(&self.stop_flag);

        let handle = thread::spawn(move || {
            let interval = Duration::from_secs(30);
            while !stop_flag.load(Ordering::SeqCst) {
                // Sleep in small increments so we can check stop_flag promptly
                let start = std::time::Instant::now();
                while start.elapsed() < interval && !stop_flag.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(200));
                }
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(e) = Self::flush_to_disk_inner(&inner) {
                    eprintln!("[memory] flush error: {}", e);
                }
            }
            // Final flush on stop
            let _ = Self::flush_to_disk_inner(&inner);
        });

        let mut guard = self.flush_handle.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(handle);
    }

    /// Signals the background flush thread to stop and waits for the final flush.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let mut guard = self.flush_handle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            let _ = handle.join();
        }
    }

    fn flush_to_disk_inner(inner: &Mutex<SessionMemoryInner>) -> Result<(), String> {
        let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.dirty {
            return Ok(());
        }

        // Ensure directory exists
        if let Some(dir) = guard.file_path.parent() {
            if let Err(e) = fs::create_dir_all(dir) {
                return Err(format!("create memory dir: {}", e));
            }
        }

        // Group by category
        let mut groups: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();
        for entry in &guard.entries {
            groups.entry(entry.category.clone()).or_default().push(entry);
        }
        let mut categories: Vec<String> = groups.keys().cloned().collect();
        categories.sort();

        let mut result = String::new();
        for cat in &categories {
            result.push_str(&format!("### {}\n", cat));
            if let Some(entries) = groups.get(cat) {
                for entry in entries {
                    result.push_str(&format!("<!-- {} -->\n", entry.timestamp));
                    result.push_str(&format!("- {}\n", entry.content));
                }
            }
            result.push('\n');
        }

        if let Err(e) = fs::write(&guard.file_path, &result) {
            return Err(format!("write memory file: {}", e));
        }

        guard.dirty = false;
        Ok(())
    }
}

impl SessionMemoryInner {
    fn load_from_disk(&mut self) {
        let data = match fs::read_to_string(&self.file_path) {
            Ok(d) => d,
            Err(_) => return, // no file yet
        };

        let mut current_category = String::new();
        let mut last_timestamp = String::new();

        for line in data.lines() {
            if let Some(rest) = line.strip_prefix("### ") {
                current_category = rest.trim().to_string();
            } else if line.starts_with("<!-- ") && line.ends_with(" -->") {
                let ts = &line[5..line.len() - 4];
                last_timestamp = ts.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("- ") {
                if !current_category.is_empty() {
                    self.entries.push(MemoryEntry {
                        category: current_category.clone(),
                        content: rest.trim().to_string(),
                        timestamp: last_timestamp.clone(),
                        source: "disk".to_string(),
                    });
                }
            }
        }
    }
}

impl Drop for SessionMemory {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_get_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("preference", "user likes dark mode", "assistant");
        sm.add_note("decision", "use rust for backend", "assistant");

        let notes = sm.get_notes();
        assert_eq!(notes.len(), 2);
        // Sorted by category: decision < preference
        assert_eq!(notes[0].category, "decision");
        assert_eq!(notes[1].category, "preference");
    }

    #[test]
    fn test_deduplication() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("preference", "user likes dark mode", "assistant");
        sm.add_note("preference", "user likes dark mode", "assistant");

        let notes = sm.get_notes();
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn test_search_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("preference", "user likes dark mode", "assistant");
        sm.add_note("decision", "use rust for backend", "assistant");

        let results = sm.search_notes("rust");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "use rust for backend");
    }

    #[test]
    fn test_format_for_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("preference", "user likes dark mode", "assistant");

        let formatted = sm.format_for_prompt();
        assert!(formatted.contains("## Session Memory"));
        assert!(formatted.contains("### preference"));
        assert!(formatted.contains("user likes dark mode"));
    }

    #[test]
    fn test_format_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        assert!(sm.format_for_prompt().is_empty());
    }

    #[test]
    fn test_disk_persistence() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let sm = SessionMemory::new(tmp.path());
            sm.add_note("preference", "user likes dark mode", "assistant");
            sm.add_note("decision", "use rust", "assistant");
            // Flush manually
            let _ = SessionMemory::flush_to_disk_inner(&sm.inner);
        }

        // Reload
        let sm2 = SessionMemory::new(tmp.path());
        let notes = sm2.get_notes();
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn test_max_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        for i in 0..110 {
            sm.add_note("state", &format!("item {}", i), "auto");
        }
        let notes = sm.get_notes();
        assert_eq!(notes.len(), 100); // max_entries = 100
        // Should keep newest (last 100)
        assert!(notes[0].content.contains("item 10"));
    }

    #[test]
    fn test_search_by_category() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("preference", "user likes dark mode", "assistant");
        sm.add_note("decision", "use rust for backend", "assistant");

        // Search by category name
        let results = sm.search_notes("preference");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, "preference");
    }

    #[test]
    fn test_flush_loop_and_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("state", "test state", "auto");
        sm.start_flush_loop();
        // Wait a moment for flush
        thread::sleep(Duration::from_millis(500));
        sm.stop();
        // Verify file was written
        let file_path = tmp.path().join(".claude").join("session_memory.md");
        assert!(file_path.exists());
    }

    #[test]
    fn test_arc_compatible() {
        // Verify that SessionMemory works behind Arc (shared references only)
        let tmp = tempfile::tempdir().unwrap();
        let sm = Arc::new(SessionMemory::new(tmp.path()));
        sm.add_note("decision", "use arc", "assistant");
        sm.start_flush_loop();

        let sm_clone = Arc::clone(&sm);
        sm_clone.add_note("state", "shared ref works", "auto");

        let notes = sm.get_notes();
        assert_eq!(notes.len(), 2);

        sm.stop();
    }
}
