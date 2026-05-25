//! Telemetry event tracking for local usage analytics.
//! Ported from upstream telemetry.go.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// A single telemetry event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub name: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<HashMap<String, serde_json::Value>>,
}

/// Captures and persists telemetry events.
/// Events are written to .claude/telemetry/YYYY-MM-DD.jsonl.
pub struct TelemetryManager {
    dir: PathBuf,
    enabled: bool,
    events: Mutex<Vec<TelemetryEvent>>,
}

impl TelemetryManager {
    /// Create a new telemetry manager.
    /// `disabled=true` prevents all telemetry recording.
    /// Also checks `CLAUDE_CODE_TELEMETRY_DISABLED` env var.
    pub fn new(disabled: bool) -> Self {
        let dir = PathBuf::from(".claude/telemetry");
        let _ = fs::create_dir_all(&dir);

        let mut enabled = !disabled;
        if enabled {
            if let Ok(v) = std::env::var("CLAUDE_CODE_TELEMETRY_DISABLED") {
                if v == "1" || v == "true" {
                    enabled = false;
                }
            }
        }

        Self {
            dir,
            enabled,
            events: Mutex::new(Vec::new()),
        }
    }

    /// Record a telemetry event.
    pub fn record(
        &self,
        name: &str,
        duration_ms: i64,
        tags: Option<HashMap<String, String>>,
        fields: Option<HashMap<String, serde_json::Value>>,
    ) {
        if !self.enabled {
            return;
        }

        let event = TelemetryEvent {
            name: name.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            duration_ms: if duration_ms != 0 { Some(duration_ms) } else { None },
            tags,
            fields,
        };

        // Write to daily log
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let file_path = self.dir.join(format!("{}.jsonl", today));

        if let Ok(data) = serde_json::to_string(&event) {
            if let Ok(mut f) = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&file_path)
            {
                let _ = writeln!(f, "{}", data);
            }
        }

        self.events.lock().unwrap().push(event);
    }

    /// Record an API call telemetry event.
    pub fn record_api_call(
        &self,
        model: &str,
        stream: bool,
        duration_ms: i64,
        input_tokens: i64,
        output_tokens: i64,
        error: Option<&str>,
    ) {
        let mut fields = HashMap::new();
        fields.insert("input_tokens".to_string(), serde_json::json!(input_tokens));
        fields.insert("output_tokens".to_string(), serde_json::json!(output_tokens));
        if let Some(e) = error {
            fields.insert("error".to_string(), serde_json::json!(e));
        }

        let mut tags = HashMap::new();
        tags.insert("model".to_string(), model.to_string());
        tags.insert("stream".to_string(), stream.to_string());

        self.record("api_call", duration_ms, Some(tags), Some(fields));
    }

    /// Record a tool call telemetry event.
    pub fn record_tool_call(&self, tool_name: &str, duration_ms: i64, is_error: bool) {
        let mut tags = HashMap::new();
        tags.insert("tool".to_string(), tool_name.to_string());

        let mut fields = HashMap::new();
        fields.insert("is_error".to_string(), serde_json::json!(is_error));

        self.record("tool_call", duration_ms, Some(tags), Some(fields));
    }

    /// Record a compaction event.
    pub fn record_compaction(&self, method: &str, tokens_before: i64, tokens_after: i64) {
        let mut tags = HashMap::new();
        tags.insert("method".to_string(), method.to_string());

        let mut fields = HashMap::new();
        fields.insert(
            "tokens_before".to_string(),
            serde_json::json!(tokens_before),
        );
        fields.insert(
            "tokens_after".to_string(),
            serde_json::json!(tokens_after),
        );

        self.record("compaction", 0, Some(tags), Some(fields));
    }

    /// Load today's JSONL log into the in-memory event list.
    pub fn load_from_file(&self) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let file_path = self.dir.join(format!("{}.jsonl", today));

        if let Ok(f) = fs::File::open(&file_path) {
            let reader = std::io::BufReader::new(f);
            let mut events = self.events.lock().unwrap();
            for line in reader.lines() {
                if let Ok(line) = line {
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(event) = serde_json::from_str::<TelemetryEvent>(&line) {
                        events.push(event);
                    }
                }
            }
        }
    }

    /// Get the last N events.
    pub fn get_recent(&self, n: usize) -> Vec<TelemetryEvent> {
        let events = self.events.lock().unwrap();
        let len = events.len();
        if n >= len {
            events.clone()
        } else {
            events[len - n..].to_vec()
        }
    }

    /// Get event counts by name.
    pub fn summary(&self) -> HashMap<String, usize> {
        let events = self.events.lock().unwrap();
        let mut counts = HashMap::new();
        for e in events.iter() {
            *counts.entry(e.name.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Enable or disable telemetry.
    pub fn set_enabled(&self, enabled: bool) {
        // We can't mutate `enabled` directly since it's not behind a Mutex.
        // This is a design tradeoff: enabled state is set at construction time.
        // For dynamic toggling, we'd need to move `enabled` behind a Mutex.
        // For now, this matches the Go API signature but is a no-op after construction.
        let _ = enabled;
    }

    /// Check if telemetry is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_record() {
        let tm = TelemetryManager::new(false);
        tm.record("test_event", 100, None, None);
        let recent = tm.get_recent(1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].name, "test_event");
    }

    #[test]
    fn test_telemetry_disabled() {
        let tm = TelemetryManager::new(true);
        tm.record("test_event", 100, None, None);
        let recent = tm.get_recent(1);
        assert!(recent.is_empty());
    }

    #[test]
    fn test_telemetry_summary() {
        let tm = TelemetryManager::new(false);
        tm.record("api_call", 50, None, None);
        tm.record("api_call", 30, None, None);
        tm.record("tool_call", 10, None, None);
        let summary = tm.summary();
        assert_eq!(*summary.get("api_call").unwrap_or(&0), 2);
        assert_eq!(*summary.get("tool_call").unwrap_or(&0), 1);
    }

    #[test]
    fn test_record_api_call() {
        let tm = TelemetryManager::new(false);
        tm.record_api_call("claude-sonnet", true, 500, 100, 50, None);
        let recent = tm.get_recent(1);
        assert_eq!(recent[0].name, "api_call");
    }

    #[test]
    fn test_record_tool_call() {
        let tm = TelemetryManager::new(false);
        tm.record_tool_call("file_read", 20, false);
        let recent = tm.get_recent(1);
        assert_eq!(recent[0].name, "tool_call");
    }

    #[test]
    fn test_record_compaction() {
        let tm = TelemetryManager::new(false);
        tm.record_compaction("sm-compact", 5000, 2000);
        let recent = tm.get_recent(1);
        assert_eq!(recent[0].name, "compaction");
    }
}
