//! Transcript module - JSONL format conversation logging

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::RwLock;
use chrono::{DateTime, Utc};

/// A single conversation entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub timestamp: DateTime<Utc>,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
}

/// Transcript - manages conversation logging
pub struct Transcript {
    path: PathBuf,
    entries: RwLock<Vec<TranscriptEntry>>,
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
    pub fn write_entry(&self, entry: &TranscriptEntry) -> std::io::Result<()> {
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
        let entry = TranscriptEntry {
            timestamp: Utc::now(),
            role: "user".to_string(),
            content,
            tool_calls: Vec::new(),
        };
        self.write_entry(&entry)
    }

    /// Add an assistant message
    pub fn add_assistant(&self, content: String, tool_calls: Vec<ToolCall>) -> std::io::Result<()> {
        let entry = TranscriptEntry {
            timestamp: Utc::now(),
            role: "assistant".to_string(),
            content,
            tool_calls,
        };
        self.write_entry(&entry)
    }

    /// Add a tool result
    pub fn add_tool_result(&self, id: String, name: String, arguments: String, result: String) -> std::io::Result<()> {
        let entry = TranscriptEntry {
            timestamp: Utc::now(),
            role: "tool".to_string(),
            content: result,
            tool_calls: vec![ToolCall {
                id,
                name,
                arguments,
                result: None,
            }],
        };
        self.write_entry(&entry)
    }

    /// Read all entries from the transcript file
    #[allow(dead_code)]
    pub fn read_all(&self) -> std::io::Result<Vec<TranscriptEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(&line) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Replay transcript for debugging
    #[allow(dead_code)]
    pub fn replay<F>(&self, mut f: F) -> std::io::Result<()>
    where
        F: FnMut(&TranscriptEntry),
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
