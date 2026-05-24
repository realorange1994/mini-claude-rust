//! Error reporter for capturing and storing error events.
//!
//! Provides a lightweight local error reporting system that can be
//! extended to integrate with Sentry or other services.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Represents a captured error event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub timestamp: String,
    pub message: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub stack: String,
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub model: String,
    pub severity: String, // "error", "warning", "info"
}

/// Error reporter that writes events to .claude/errors/
pub struct ErrorReporter {
    dir: PathBuf,
    enabled: bool,
    events: Vec<ErrorEvent>,
}

impl ErrorReporter {
    /// Create a new error reporter that writes events to .claude/errors/
    pub fn new() -> Self {
        let dir = PathBuf::from(".claude/errors");
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            enabled: true,
            events: Vec::new(),
        }
    }

    /// Record an error event.
    pub fn capture(&mut self, msg: &str, severity: &str, context: HashMap<String, serde_json::Value>) {
        if !self.enabled {
            return;
        }
        let event = ErrorEvent {
            timestamp: Utc::now().to_rfc3339(),
            message: msg.to_string(),
            event_type: classify_error_type(msg),
            stack: String::new(),
            context,
            session_id: String::new(),
            model: String::new(),
            severity: severity.to_string(),
        };
        self.events.push(event.clone());

        // Write to daily log file
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let file_path = self.dir.join(format!("{}.jsonl", day));
        if let Ok(data) = serde_json::to_string(&event) {
            if let Ok(mut f) = OpenOptions::new()
                .append(true)
                .create(true)
                .write(true)
                .open(&file_path)
            {
                let _ = writeln!(f, "{}", data);
            }
        }
    }

    /// Record an error-level event.
    pub fn capture_error(&mut self, msg: &str, context: HashMap<String, serde_json::Value>) {
        self.capture(msg, "error", context);
    }

    /// Record a warning-level event.
    pub fn capture_warning(&mut self, msg: &str, context: HashMap<String, serde_json::Value>) {
        self.capture(msg, "warning", context);
    }

    /// Get the last N captured events.
    pub fn get_recent(&self, n: usize) -> Vec<ErrorEvent> {
        let start = if n > self.events.len() {
            0
        } else {
            self.events.len() - n
        };
        self.events[start..].to_vec()
    }

    /// Enable or disable error reporting.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns a summary of captured events by type.
    pub fn summary(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for e in &self.events {
            *counts.entry(e.event_type.clone()).or_insert(0) += 1;
        }
        counts
    }
}

impl Default for ErrorReporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Categorize an error message into a type.
fn classify_error_type(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if contains_any(&lower, &["context_length_exceeded", "context overflow", "max context"]) {
        return "context_overflow".to_string();
    }
    if contains_any(&lower, &["529", "overloaded"]) {
        return "overloaded".to_string();
    }
    if contains_any(&lower, &["429", "rate limit"]) {
        return "rate_limit".to_string();
    }
    if contains_any(&lower, &["stream stalled", "stream error", "stream interrupted"]) {
        return "stream_error".to_string();
    }
    if contains_any(&lower, &["2013", "tool pairing"]) {
        return "tool_pairing".to_string();
    }
    if contains_any(&lower, &["permission", "denied", "blocked"]) {
        return "permission".to_string();
    }
    if contains_any(&lower, &["timeout", "deadline exceeded"]) {
        return "timeout".to_string();
    }
    if contains_any(&lower, &["network", "connection", "dns"]) {
        return "network".to_string();
    }
    "unknown".to_string()
}

fn contains_any(s: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| s.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_error_type() {
        assert_eq!(classify_error_type("context_length_exceeded"), "context_overflow");
        assert_eq!(classify_error_type("529 overloaded"), "overloaded");
        assert_eq!(classify_error_type("429 rate limit"), "rate_limit");
        assert_eq!(classify_error_type("stream stalled"), "stream_error");
        assert_eq!(classify_error_type("permission denied"), "permission");
        assert_eq!(classify_error_type("timeout"), "timeout");
        assert_eq!(classify_error_type("network connection"), "network");
        assert_eq!(classify_error_type("something else"), "unknown");
    }

    #[test]
    fn test_error_reporter_capture() {
        let mut reporter = ErrorReporter::new();
        let mut ctx = HashMap::new();
        ctx.insert("key".to_string(), serde_json::json!("value"));
        reporter.capture_error("test error", ctx);
        assert_eq!(reporter.events.len(), 1);
        assert_eq!(reporter.events[0].severity, "error");
    }

    #[test]
    fn test_error_reporter_disabled() {
        let mut reporter = ErrorReporter::new();
        reporter.set_enabled(false);
        let ctx = HashMap::new();
        reporter.capture_error("test error", ctx);
        assert_eq!(reporter.events.len(), 0);
    }

    #[test]
    fn test_error_reporter_summary() {
        let mut reporter = ErrorReporter::new();
        let ctx = HashMap::new();
        reporter.capture_error("context_length_exceeded", ctx.clone());
        reporter.capture_error("429 rate limit", ctx.clone());
        reporter.capture_error("context overflow again", ctx);
        let summary = reporter.summary();
        assert_eq!(*summary.get("context_overflow").unwrap_or(&0), 2);
        assert_eq!(*summary.get("rate_limit").unwrap_or(&0), 1);
    }
}
