//! Session memory — persistent structured notes across conversation turns.
//!
//! Ported from the Go Phase 4 implementation:
//! - MemoryEntry with category, content, timestamp, source
//! - SessionMemory with CRUD, disk persistence, background flush
//! - Markdown storage format compatible with the Go version
//! - Extraction state machine for SM-compact triggering
//! - Session memory template format (10-section structured markdown)

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Categories for memory entries.
pub const VALID_CATEGORIES: &[&str] = &["preference", "decision", "state", "reference", "test"];

/// Default session memory template matching upstream's structured format.
/// Each section has a header and italic description (template instruction).
/// The LLM updates only the content, preserving the structure.
pub const DEFAULT_SESSION_MEMORY_TEMPLATE: &str = r#"# Session Title
_A short and distinctive 5-10 word descriptive title. Super info dense, no filler_

# Current State
_What is actively being worked on right now? Pending tasks not yet completed. Immediate next steps._

# Task Specification
_What did the user asked to build? Any design decisions or explanatory context._

# Files and Functions
_What are the important files? What do they contain and why are they relevant?_

# Workflow
_What bash commands are usually run and in what order? How to interpret their output?_

# Errors & Corrections
_Errors encountered and how they were fixed. What did the user correct? What approaches failed?_

# Codebase and System Documentation
_What are the important system components? How do they work/fit together?_

# Learnings
_What has worked well? What has not? What to avoid? Do not duplicate items from other sections._

# Key Results
_If the user asked for a specific output (answer, table, document), repeat the exact result here._

# Worklog
_Step by step, what was attempted and done? Very terse summary for each step._
"#;

/// Token budget constants — reduced from upstream defaults to improve cache hit rates.
/// For a coding agent, most sections (Learnings, Key Results, Worklog) are
/// redundant with git/file state. Keeping Current State and Errors is sufficient.
const MAX_TOKENS_PER_SECTION: i64 = 2500;
const MAX_TOTAL_SESSION_MEMORY_TOKENS: i64 = 10000;

/// Entry expiration: state entries expire after 7 days,
/// other categories expire after 30 days.
const ENTRY_EXPIRATION_STATE: Duration = Duration::from_secs(7 * 24 * 3600);
const ENTRY_EXPIRATION_OTHER: Duration = Duration::from_secs(30 * 24 * 3600);

/// Max entries per category (to prevent unbounded growth)
const MAX_STATE_ENTRIES: usize = 20;
const MAX_DECISION_ENTRIES: usize = 30;
const MAX_PREFERENCE_ENTRIES: usize = 20;
const MAX_REFERENCE_ENTRIES: usize = 50;
const MAX_TEST_ENTRIES: usize = 20;

/// Extraction thresholds — raised from upstream defaults to reduce forked agent API calls.
/// Matches Go's minimumMessageTokensToInit, minimumTokensBetweenUpdate, toolCallsBetweenUpdates.
const MINIMUM_MESSAGE_TOKENS_TO_INIT: i64 = 20000;
const MINIMUM_TOKENS_BETWEEN_UPDATE: i64 = 10000;
const TOOL_CALLS_BETWEEN_UPDATES: usize = 3;

/// A single memory note.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub category: String,  // "preference" | "decision" | "state" | "reference" | "test"
    pub content: String,   // the actual note text
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub source: String,    // "user" | "assistant" | "auto" | "disk"
}

impl MemoryEntry {
    /// Returns true if the entry is older than the category TTL.
    pub fn is_expired(&self) -> bool {
        let elapsed = chrono::Utc::now().signed_duration_since(self.timestamp);
        let ttl = expiration_for_category(&self.category);
        elapsed.num_seconds() > ttl.as_secs() as i64
    }
}

/// Returns the TTL for entries in a given category.
fn expiration_for_category(category: &str) -> Duration {
    match category {
        "state" => ENTRY_EXPIRATION_STATE,
        _ => ENTRY_EXPIRATION_OTHER,
    }
}

/// Returns the max entries limit for a given category.
fn max_entries_for_category(category: &str) -> usize {
    match category {
        "state" => MAX_STATE_ENTRIES,
        "decision" => MAX_DECISION_ENTRIES,
        "preference" => MAX_PREFERENCE_ENTRIES,
        "reference" => MAX_REFERENCE_ENTRIES,
        "test" => MAX_TEST_ENTRIES,
        _ => 20,
    }
}

/// Checks if the given content is essentially just the default template
/// (no user-written content). Used to detect whether session memory has
/// actual extracted content or is just the empty template.
pub fn is_session_memory_template_only(content: &str) -> bool {
    content.trim() == DEFAULT_SESSION_MEMORY_TEMPLATE.trim()
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
    /// LastSummarizedMessageUUID tracks the UUID of the most recent message that
    /// has been summarized by session memory extraction. This enables incremental
    /// SM-compact: subsequent compactions only compact forward from this point.
    last_summarized_message_uuid: String,
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
            last_summarized_message_uuid: String::new(),
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
        let now = chrono::Utc::now();

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

        // Enforce per-category max entries (keep newest)
        let cat = category.to_string();
        Self::trim_category_entries(&mut guard.entries, &cat);

        guard.dirty = true;
        let cb = guard.on_add.clone();
        drop(guard);
        if let Some(cb) = cb {
            cb();
        }
    }

    /// Removes oldest entries in a category to enforce max.
    fn trim_category_entries(entries: &mut Vec<MemoryEntry>, category: &str) {
        let max = max_entries_for_category(category);
        let count = entries.iter().filter(|e| e.category == *category).count();
        if count <= max {
            return;
        }
        let excess = count - max;
        let mut removed = 0;
        let mut to_remove = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if e.category == *category && removed < excess {
                to_remove.push(i);
                removed += 1;
            }
        }
        // Remove in reverse order to preserve indices
        to_remove.reverse();
        for i in to_remove {
            entries.remove(i);
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

    /// Removes entries older than their category TTL.
    pub fn remove_expired_entries(&self) -> usize {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = guard.entries.len();
        guard.entries.retain(|e| !e.is_expired());
        let removed = before - guard.entries.len();
        if removed > 0 {
            guard.dirty = true;
        }
        removed
    }

    /// Removes all entries in the "state" category.
    /// Called at session start to prevent stale session context from
    /// previous sessions from bleeding in.
    pub fn clear_state_entries(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = guard.entries.len();
        guard.entries.retain(|e| e.category != "state");
        if guard.entries.len() < before {
            guard.dirty = true;
        }
    }

    /// Appends conclusion entries as state memory.
    /// Called before compaction so the agent's accumulated work knowledge
    /// is preserved across compaction.
    pub fn save_conclusions(&self, conclusions: &[String]) {
        if conclusions.is_empty() {
            return;
        }
        let now = chrono::Utc::now();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        for c in conclusions {
            if c.is_empty() {
                continue;
            }
            // Check if this conclusion already exists to avoid duplicates
            let exists = guard.entries.iter().any(|e| {
                e.category == "state" && e.content == *c
            });
            if !exists {
                guard.entries.push(MemoryEntry {
                    category: "state".to_string(),
                    content: c.clone(),
                    timestamp: now,
                    source: "auto".to_string(),
                });
            }
        }

        // Enforce max state entries
        Self::trim_category_entries(&mut guard.entries, "state");
        guard.dirty = true;
    }

    /// Returns the UUID of the most recently summarized message for incremental SM-compact.
    /// Returns "" if no compaction has occurred.
    pub fn get_last_summarized_message_uuid(&self) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_summarized_message_uuid.clone()
    }

    /// Sets the UUID of the most recently summarized message for incremental SM-compact.
    pub fn set_last_summarized_message_uuid(&self, uuid: &str) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.last_summarized_message_uuid = uuid.to_string();
        guard.dirty = true;
    }

    /// Formats memory entries for injection into the system prompt.
    /// Returns an empty string if there are no entries.
    pub fn format_for_prompt(&self) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::format_entries_for_prompt_inner(&guard.entries)
    }

    /// Formats memory entries for injection after compaction.
    /// Each section is truncated to max_section_chars (~8000) matching
    /// upstream's truncateSessionMemoryForCompact.
    pub fn format_for_prompt_truncated(&self, max_section_chars: usize) -> String {
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

        // Token budget: ~4 chars/token
        let max_total_chars = (MAX_TOTAL_SESSION_MEMORY_TOKENS * 4) as usize;
        let max_section_chars = max_section_chars.min(max_total_chars);

        let mut result = String::new();
        let header = String::from("## Session Memory\n\nThe following notes were recorded during this or previous sessions. Use them as context.\n\n");
        result.push_str(&header);

        let mut total_used = header.len();

        for cat in &categories {
            let section_header = format!("### {}\n", cat);
            let section_header_len = section_header.len();

            // Check total budget
            if total_used + section_header_len > max_total_chars {
                break;
            }

            result.push_str(&section_header);
            total_used += section_header_len;
            let mut section_used = section_header_len;

            if let Some(entries) = groups.get(cat) {
                for entry in entries {
                    let line = format!("- {}\n", entry.content);
                    let line_len = line.len();

                    // Total budget check
                    if total_used + line_len > max_total_chars {
                        break;
                    }

                    // Per-section budget check
                    if section_used + line_len > max_section_chars {
                        let remaining = max_section_chars - section_used - "  [... truncated ...]\n".len();
                        if remaining > 0 {
                            let truncated = Self::truncate_line(&line, remaining);
                            result.push_str(&truncated);
                            result.push_str("  [... truncated ...]\n");
                        }
                        break;
                    }
                    result.push_str(&line);
                    section_used += line_len;
                    total_used += line_len;
                }
            }
            result.push('\n');
        }

        result
    }

    fn truncate_line(line: &str, max_len: usize) -> String {
        if line.len() <= max_len {
            return line.to_string();
        }
        // Try sentence boundary (. )
        if let Some(idx) = line[..max_len].rfind(". ") {
            if idx > 0 {
                return format!("{}.\n", &line[..idx]);
            }
        }
        // Try newline
        if let Some(idx) = line[..max_len].rfind('\n') {
            return format!("{}\n", &line[..idx]);
        }
        format!("{}\n", &line[..max_len])
    }

    /// Formats memory entries for injection into the system prompt (inner helper).
    fn format_entries_for_prompt_inner(entries: &[MemoryEntry]) -> String {
        if entries.is_empty() {
            return String::new();
        }

        // Group by category
        let mut groups: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();
        for entry in entries {
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

    /// Formats session memory as the 10-section template.
    /// Used for disk storage — the file is the single source of truth.
    pub fn format_for_template(&self) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::format_for_template_inner(&guard.entries)
    }

    fn format_for_template_inner(entries: &[MemoryEntry]) -> String {
        if entries.is_empty() {
            return DEFAULT_SESSION_MEMORY_TEMPLATE.to_string();
        }

        // Group entries by category
        let mut section_content: HashMap<String, Vec<String>> = HashMap::new();
        for e in entries {
            section_content.entry(e.category.clone()).or_default().push(e.content.clone());
        }

        let mut sb = String::new();

        // Session Title
        sb.push_str("# Session Title\n");
        sb.push_str("_A short and distinctive 5-10 word descriptive title. Super info dense, no filler_\n");
        if let Some(items) = section_content.get("state") {
            if !items.is_empty() {
                sb.push_str(&items[0]);
                for item in &items[1..] {
                    if sb.len() < 200 {
                        sb.push_str(" | ");
                        sb.push_str(item);
                    }
                }
            }
        }
        sb.push_str("\n\n");

        // Current State
        sb.push_str("# Current State\n");
        sb.push_str("_What is actively being worked on right now? Pending tasks not yet completed. Immediate next steps._\n");
        if let Some(items) = section_content.get("state") {
            for item in items {
                sb.push_str("- ");
                sb.push_str(item);
                sb.push('\n');
            }
        }
        sb.push('\n');

        // Task Specification (use decision entries)
        sb.push_str("# Task Specification\n");
        sb.push_str("_What did the user ask to build? Any design decisions or explanatory context._\n");
        if let Some(items) = section_content.get("decision") {
            for item in items {
                sb.push_str("- ");
                sb.push_str(item);
                sb.push('\n');
            }
        }
        sb.push('\n');

        // Files and Functions (use reference entries)
        sb.push_str("# Files and Functions\n");
        sb.push_str("_What are the important files? What do they contain and why are they relevant?_\n");
        if let Some(items) = section_content.get("reference") {
            for item in items {
                sb.push_str("- ");
                sb.push_str(item);
                sb.push('\n');
            }
        }
        sb.push('\n');

        // Workflow (no default category)
        sb.push_str("# Workflow\n");
        sb.push_str("_What bash commands are usually run and in what order? How to interpret their output?_\n");
        sb.push('\n');

        // Errors & Corrections
        sb.push_str("# Errors & Corrections\n");
        sb.push_str("_Errors encountered and how they were fixed. What did the user correct? What approaches failed?_\n");
        sb.push('\n');

        // Codebase and System Documentation
        sb.push_str("# Codebase and System Documentation\n");
        sb.push_str("_What are the important system components? How do they work/fit together?_\n");
        sb.push('\n');

        // Learnings (use preference entries)
        sb.push_str("# Learnings\n");
        sb.push_str("_What has worked well? What has not? What to avoid? Do not duplicate items from other sections._\n");
        if let Some(items) = section_content.get("preference") {
            for item in items {
                sb.push_str("- ");
                sb.push_str(item);
                sb.push('\n');
            }
        }
        sb.push('\n');

        // Key Results
        sb.push_str("# Key Results\n");
        sb.push_str("_If the user asked for a specific output (answer, table, document), repeat the exact result here._\n");
        sb.push('\n');

        // Worklog
        sb.push_str("# Worklog\n");
        sb.push_str("_Step by step, what was attempted and done? Very terse summary for each step._\n");
        sb.push('\n');

        sb
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

        // Write in the 10-section template format (matching what the forked agent
        // sees/edits and what compaction reads). This ensures the disk file is the
        // single source of truth.
        let content = Self::format_for_template_inner(&guard.entries);

        // Atomic write: write to temp file in same directory, then rename.
        let tmp_path = guard.file_path.with_extension("md.tmp");
        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(format!("write memory file tmp: {}", e));
        }
        if let Err(e) = fs::rename(&tmp_path, &guard.file_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("rename memory file: {}", e));
        }

        guard.dirty = false;
        Ok(())
    }

    /// Returns the default session memory template.
    pub fn load_session_memory_template() -> &'static str {
        DEFAULT_SESSION_MEMORY_TEMPLATE
    }
}

impl SessionMemoryInner {
    fn load_from_disk(&mut self) {
        let data = match fs::read_to_string(&self.file_path) {
            Ok(d) => d,
            Err(_) => return, // no file yet
        };

        self.entries = Self::parse_markdown_entries(&data);
    }

    /// Parses entries from a markdown session memory file.
    /// Handles both structured template format (with section headers like "# Section")
    /// and simple list format (with "### Category" headers).
    fn parse_markdown_entries(data: &str) -> Vec<MemoryEntry> {
        let mut entries = Vec::new();
        let mut current_category = String::new();
        let mut last_timestamp = chrono::Utc::now();

        for line in data.lines() {
            // Structured template section (upstream format): # Section Title
            if line.starts_with("# ") && !line.starts_with("## ") {
                // Map template sections to categories
                let lower = line[2..].trim().to_lowercase();
                if lower.contains("current state") || lower.contains("session title") {
                    current_category = "state".to_string();
                } else if lower.contains("task spec") {
                    current_category = "decision".to_string();
                } else if lower.contains("files") || lower.contains("workflow")
                    || lower.contains("key result") || lower.contains("worklog")
                    || lower.contains("codebase") {
                    current_category = "reference".to_string();
                } else if lower.contains("error") {
                    current_category = "decision".to_string();
                } else if lower.contains("learn") {
                    current_category = "preference".to_string();
                } else {
                    current_category = String::new();
                }
                continue;
            }

            // Simple list category header (legacy format): ### Category
            if let Some(rest) = line.strip_prefix("### ") {
                current_category = rest.trim().to_string();
                continue;
            }

            // Template description line (italic, starts with "_"): skip
            let trimmed = line.trim();
            if trimmed.starts_with('_') && trimmed.ends_with('_') {
                continue; // description line, skip
            }

            // Timestamp comment: <!-- timestamp -->
            if line.starts_with("<!-- ") && line.ends_with(" -->") {
                let ts = &line[5..line.len() - 4];
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts.trim()) {
                    last_timestamp = parsed.with_timezone(&chrono::Utc);
                }
                continue;
            }

            // List entry: - content
            if let Some(rest) = line.strip_prefix("- ") {
                let content = rest.trim();
                if !content.is_empty() && !current_category.is_empty() {
                    entries.push(MemoryEntry {
                        category: current_category.clone(),
                        content: content.to_string(),
                        timestamp: last_timestamp,
                        source: "disk".to_string(),
                    });
                }
            }
        }

        entries
    }
}

impl Drop for SessionMemory {
    fn drop(&mut self) {
        self.stop();
    }
}

// ─── Extraction State ─────────────────────────────────────────────────────────

/// Tracks when the next session memory extraction should happen.
/// Used by SM-compact to decide when to fork the agent for extraction.
#[derive(Debug)]
pub struct ExtractionState {
    inner: Arc<Mutex<ExtractionStateInner>>,
}

#[derive(Debug)]
struct ExtractionStateInner {
    initialized: bool,
    tokens_at_last_extract: i64,
    tool_calls_since_last: usize,
    /// extraction_in_progress is set to true when a goroutine extraction is running
    /// and false when it completes. SM-compact waits for this to be false before
    /// proceeding, so it uses the freshest session memory content.
    extraction_in_progress: bool,
    extraction_started_at: Option<std::time::Instant>,
}

impl ExtractionState {
    /// Creates a new extraction state tracker.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ExtractionStateInner {
                initialized: false,
                tokens_at_last_extract: 0,
                tool_calls_since_last: 0,
                extraction_in_progress: false,
                extraction_started_at: None,
            })),
        }
    }

    /// Checks if the extraction thresholds have been met.
    /// Matches upstream: token threshold AND (tool call threshold OR no tool calls in last turn).
    pub fn should_extract(&self, current_tokens: i64, has_tool_calls_in_last_turn: bool) -> bool {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        if !guard.initialized {
            return current_tokens >= MINIMUM_MESSAGE_TOKENS_TO_INIT;
        }

        let tokens_since_last = current_tokens - guard.tokens_at_last_extract;
        let has_met_token_threshold = tokens_since_last >= MINIMUM_TOKENS_BETWEEN_UPDATE;
        let has_met_tool_call_threshold = guard.tool_calls_since_last >= TOOL_CALLS_BETWEEN_UPDATES;

        has_met_token_threshold && (has_met_tool_call_threshold || !has_tool_calls_in_last_turn)
    }

    /// Records that an extraction was performed.
    pub fn mark_extracted(&self, current_tokens: i64) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.initialized = true;
        guard.tokens_at_last_extract = current_tokens;
        guard.tool_calls_since_last = 0;
        guard.extraction_in_progress = false;
    }

    /// Increments the tool call counter.
    pub fn increment_tool_call(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.tool_calls_since_last += 1;
    }

    /// Signals that extraction has started.
    pub fn mark_extraction_in_progress(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.extraction_in_progress = true;
        guard.extraction_started_at = Some(std::time::Instant::now());
    }

    /// Waits (with timeout) for any in-progress extraction to complete.
    /// Returns immediately if extraction is stale (> 60s old, assumed abandoned).
    /// Returns true if extraction completed, false if timed out.
    pub fn wait_for_extraction(&self, timeout: Duration) -> bool {
        const CHECK_INTERVAL: Duration = Duration::from_secs(1);
        const STALE_THRESHOLD: Duration = Duration::from_secs(60);

        let deadline = std::time::Instant::now() + timeout;
        loop {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.extraction_in_progress {
                return true;
            }
            // If extraction is stale (> 60s old), don't wait — assume it crashed.
            if let Some(started_at) = guard.extraction_started_at {
                if started_at.elapsed() > STALE_THRESHOLD {
                    return false;
                }
            }
            drop(guard);

            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(CHECK_INTERVAL);
        }
    }
}

impl Default for ExtractionState {
    fn default() -> Self { Self::new() }
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
    fn test_max_entries_per_category() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        // Add more than MAX_STATE_ENTRIES (20)
        for i in 0..30 {
            sm.add_note("state", &format!("state item {}", i), "auto");
        }
        let notes = sm.get_notes();
        let state_notes: Vec<_> = notes.iter().filter(|n| n.category == "state").collect();
        assert_eq!(state_notes.len(), MAX_STATE_ENTRIES);
    }

    #[test]
    fn test_clear_state_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("state", "current state", "auto");
        sm.add_note("decision", "some decision", "assistant");

        sm.clear_state_entries();

        let notes = sm.get_notes();
        let has_state = notes.iter().any(|n| n.category == "state");
        assert!(!has_state);
        assert_eq!(notes.len(), 1); // decision still present
    }

    #[test]
    fn test_save_conclusions() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.save_conclusions(&["built the API".to_string(), "fixed the bug".to_string()]);

        let notes = sm.get_notes();
        let state_notes: Vec<_> = notes.iter().filter(|n| n.category == "state").collect();
        assert_eq!(state_notes.len(), 2);
    }

    #[test]
    fn test_save_conclusions_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.save_conclusions(&["same conclusion".to_string()]);
        sm.save_conclusions(&["same conclusion".to_string()]);

        let notes = sm.get_notes();
        let state_notes: Vec<_> = notes.iter().filter(|n| n.category == "state").collect();
        assert_eq!(state_notes.len(), 1);
    }

    #[test]
    fn test_last_summarized_message_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        assert!(sm.get_last_summarized_message_uuid().is_empty());

        sm.set_last_summarized_message_uuid("test-uuid-123");
        assert_eq!(sm.get_last_summarized_message_uuid(), "test-uuid-123");
    }

    #[test]
    fn test_format_for_template() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("state", "building the auth system", "auto");
        sm.add_note("decision", "use JWT for tokens", "assistant");

        let formatted = sm.format_for_template();
        assert!(formatted.contains("# Session Title"));
        assert!(formatted.contains("# Current State"));
        assert!(formatted.contains("# Task Specification"));
        assert!(formatted.contains("building the auth system"));
        assert!(formatted.contains("use JWT for tokens"));
    }

    #[test]
    fn test_format_for_template_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        let formatted = sm.format_for_template();
        assert_eq!(formatted.trim(), DEFAULT_SESSION_MEMORY_TEMPLATE.trim());
    }

    #[test]
    fn test_is_session_memory_template_only() {
        assert!(is_session_memory_template_only(DEFAULT_SESSION_MEMORY_TEMPLATE));
        assert!(!is_session_memory_template_only("# Custom Content\n- some data"));
    }

    #[test]
    fn test_extraction_state() {
        let es = ExtractionState::new();

        // Not initialized, below threshold
        assert!(!es.should_extract(10000, false));

        // Not initialized, above threshold
        assert!(es.should_extract(MINIMUM_MESSAGE_TOKENS_TO_INIT + 1, false));

        // Mark extracted
        es.mark_extracted(30000);

        // Not enough tokens since last extract
        assert!(!es.should_extract(35000, false));

        // Enough tokens and tool calls
        for _ in 0..TOOL_CALLS_BETWEEN_UPDATES {
            es.increment_tool_call();
        }
        assert!(es.should_extract(30000 + MINIMUM_TOKENS_BETWEEN_UPDATE + 1, true));

        // Enough tokens but no tool calls in last turn
        es.mark_extracted(40000);
        assert!(es.should_extract(40000 + MINIMUM_TOKENS_BETWEEN_UPDATE + 1, false));
    }

    #[test]
    fn test_wait_for_extraction_no_in_progress() {
        let es = ExtractionState::new();
        // Nothing in progress, should return immediately
        assert!(es.wait_for_extraction(Duration::from_secs(5)));
    }

    #[test]
    fn test_parse_markdown_entries_template_format() {
        let tmp = tempfile::tempdir().unwrap();
        let sm = SessionMemory::new(tmp.path());
        sm.add_note("state", "building auth system", "auto");
        sm.add_note("decision", "using JWT", "assistant");
        let _ = SessionMemory::flush_to_disk_inner(&sm.inner);

        // Reload
        let sm2 = SessionMemory::new(tmp.path());
        let notes = sm2.get_notes();
        assert!(notes.len() >= 2);
    }

    #[test]
    fn test_truncate_line() {
        let line = "This is a sentence. This is another. And a third.";
        let truncated = SessionMemory::truncate_line(line, 20);
        assert_eq!(truncated, "This is a sentence.\n");
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
