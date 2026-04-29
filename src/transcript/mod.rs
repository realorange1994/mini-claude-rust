//! Transcript module - JSONL format conversation logging
//! Fully compatible with Go format

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::RwLock;
use chrono::{DateTime, Utc};

/// Entry type constants (matching Go format)
pub const TYPE_USER: &str = "user";
pub const TYPE_ASSISTANT: &str = "assistant";
pub const TYPE_TOOL_USE: &str = "tool_use";
pub const TYPE_TOOL_RESULT: &str = "tool_result";
pub const TYPE_ERROR: &str = "error";
pub const TYPE_SYSTEM: &str = "system";
pub const TYPE_COMPACT: &str = "compact";
pub const TYPE_SUMMARY: &str = "summary";

/// A single transcript entry (matching Go format exactly)
///
/// Go format:
/// ```go
/// type Entry struct {
///     Type      string         `json:"type"`
///     Content   string         `json:"content,omitempty"`
///     ToolName  string         `json:"tool_name,omitempty"`
///     ToolArgs  map[string]any `json:"tool_args,omitempty"`
///     ToolID    string         `json:"tool_id,omitempty"`
///     Timestamp time.Time      `json:"timestamp"`
///     Model     string         `json:"model,omitempty"`
///     Error     string         `json:"error,omitempty"`
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Entry type: user, assistant, tool_use, tool_result, error, system
    #[serde(rename = "type")]
    pub type_: String,
    /// Content (optional for tool_use, required for others)
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub content: String,
    /// Tool name (for tool_use and tool_result)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
    /// Tool arguments (for tool_use)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_args: Option<HashMap<String, serde_json::Value>>,
    /// Tool ID (for tool_use and tool_result)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_id: Option<String>,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Model name (for assistant entries)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
    /// Error message (for error entries)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

impl Entry {
    /// Create a user entry
    pub fn user(content: String) -> Self {
        Self {
            type_: TYPE_USER.to_string(),
            content,
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Create an assistant entry
    pub fn assistant(content: String, model: Option<String>) -> Self {
        Self {
            type_: TYPE_ASSISTANT.to_string(),
            content,
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model,
            error: None,
        }
    }

    /// Create a tool_use entry
    pub fn tool_use(tool_id: String, tool_name: String, tool_args: HashMap<String, serde_json::Value>) -> Self {
        Self {
            type_: TYPE_TOOL_USE.to_string(),
            content: String::new(),
            tool_name: Some(tool_name),
            tool_args: Some(tool_args),
            tool_id: Some(tool_id),
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Create a tool_result entry
    pub fn tool_result(tool_id: String, tool_name: String, content: String) -> Self {
        Self {
            type_: TYPE_TOOL_RESULT.to_string(),
            content,
            tool_name: Some(tool_name),
            tool_args: None,
            tool_id: Some(tool_id),
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Create a system entry
    pub fn system(content: String) -> Self {
        Self {
            type_: TYPE_SYSTEM.to_string(),
            content,
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Create an error entry
    pub fn error(error: String) -> Self {
        Self {
            type_: TYPE_ERROR.to_string(),
            content: String::new(),
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model: None,
            error: Some(error),
        }
    }

    /// Create a compact entry (compact boundary marker)
    pub fn compact(trigger: String, pre_compact_tokens: usize) -> Self {
        Self {
            type_: TYPE_COMPACT.to_string(),
            content: format!("Compacted conversation (trigger: {}, {} tokens compressed)", trigger, pre_compact_tokens),
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Create a summary entry (from LLM-driven compaction)
    pub fn summary(content: String) -> Self {
        Self {
            type_: TYPE_SUMMARY.to_string(),
            content,
            tool_name: None,
            tool_args: None,
            tool_id: None,
            timestamp: Utc::now(),
            model: None,
            error: None,
        }
    }

    /// Check if this is a tool_use entry
    pub fn is_tool_use(&self) -> bool {
        self.type_ == TYPE_TOOL_USE
    }

    /// Check if this is a tool_result entry
    pub fn is_tool_result(&self) -> bool {
        self.type_ == TYPE_TOOL_RESULT
    }

    /// Check if this is a user entry
    pub fn is_user(&self) -> bool {
        self.type_ == TYPE_USER
    }

    /// Check if this is an assistant entry
    pub fn is_assistant(&self) -> bool {
        self.type_ == TYPE_ASSISTANT
    }

    /// Check if this is a compact entry
    pub fn is_compact(&self) -> bool {
        self.type_ == TYPE_COMPACT
    }

    /// Check if this is a summary entry
    pub fn is_summary(&self) -> bool {
        self.type_ == TYPE_SUMMARY
    }
}

/// Transcript - manages conversation logging
pub struct Transcript {
    path: PathBuf,
    entries: RwLock<Vec<Entry>>,
}

impl Transcript {
    /// Create a new transcript
    pub fn new(path: &PathBuf) -> Self {
        Self {
            path: path.clone(),
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Get the transcript file path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Get the transcript filename
    pub fn filename(&self) -> &str {
        self.path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.jsonl")
    }

    /// Write an entry to the transcript file
    pub fn write_entry(&self, entry: &Entry) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        let json = serde_json::to_string(entry)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;

        let mut entries = self.entries.write().unwrap();
        entries.push(entry.clone());

        Ok(())
    }

    /// Add a user message
    pub fn add_user(&self, content: String) -> std::io::Result<()> {
        self.write_entry(&Entry::user(content))
    }

    /// Add an assistant message
    pub fn add_assistant(&self, content: String, model: Option<String>) -> std::io::Result<()> {
        self.write_entry(&Entry::assistant(content, model))
    }

    /// Add a tool_use entry
    pub fn add_tool_use(&self, tool_id: String, tool_name: String, tool_arg: HashMap<String, serde_json::Value>) -> std::io::Result<()> {
        self.write_entry(&Entry::tool_use(tool_id, tool_name, tool_arg))
    }

    /// Add a tool_result entry
    pub fn add_tool_result(&self, tool_id: String, tool_name: String, result: String) -> std::io::Result<()> {
        self.write_entry(&Entry::tool_result(tool_id, tool_name, result))
    }

    /// Add a system entry
    pub fn add_system(&self, content: String) -> std::io::Result<()> {
        self.write_entry(&Entry::system(content))
    }

    /// Add an error entry
    pub fn add_error(&self, error: String) -> std::io::Result<()> {
        self.write_entry(&Entry::error(error))
    }

    /// Add a compact entry
    pub fn add_compact(&self, trigger: String, pre_compact_tokens: usize) -> std::io::Result<()> {
        self.write_entry(&Entry::compact(trigger, pre_compact_tokens))
    }

    /// Add a summary entry
    pub fn add_summary(&self, content: String) -> std::io::Result<()> {
        self.write_entry(&Entry::summary(content))
    }

    /// Read all entries from the transcript file.
    /// Handles truncated last lines (from Ctrl+C / crash) by discarding them.
    pub fn read_all(&self) -> std::io::Result<Vec<Entry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut last_bad_line: Option<String> = None;

        for line in reader.lines() {
            let line = line?;
            if let Ok(entry) = serde_json::from_str::<Entry>(&line) {
                entries.push(entry);
                last_bad_line = None;
            } else {
                // Keep the bad line in case it's the last one (truncated write)
                last_bad_line = Some(line);
            }
        }
        // If the last line was corrupt (truncated JSON from crash/Ctrl+C),
        // it's safe to discard — it was an incomplete write.
        let _ = last_bad_line;

        Ok(entries)
    }

    /// Replay transcript for debugging
    #[allow(dead_code)]
    pub fn replay<F>(&self, mut f: F) -> std::io::Result<()>
    where
        F: FnMut(&Entry),
    {
        let entries = self.read_all()?;
        for entry in entries {
            f(&entry);
        }
        Ok(())
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            path: PathBuf::from("transcript.jsonl"),
            entries: RwLock::new(Vec::new()),
        }
    }
}

// ============================================================================
// Type alias for API consistency
// ============================================================================

/// Type alias for Entry (used in function signatures)
pub type TranscriptEntry = Entry;
