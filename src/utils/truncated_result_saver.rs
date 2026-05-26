//! Truncated Result Saver — saves full truncated tool output to disk for recall.
//!
//! When a tool produces output longer than the truncation limit, this module saves
//! the full original output to `.claude/truncated-results/` and returns a recall
//! message to the LLM so it knows the content is available via read_file.

use chrono::Local;
use rand::Rng;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const MAX_AGE_DAYS: u64 = 30;

/// Saves truncated tool output to disk and returns a recall hint.
#[derive(Clone)]
pub struct TruncatedResultSaver {
    project_dir: PathBuf,
    max_age: Duration,
}

impl TruncatedResultSaver {
    /// Create a new TruncatedResultSaver.
    pub fn new(project_dir: &str) -> Self {
        Self {
            project_dir: PathBuf::from(project_dir),
            max_age: Duration::from_secs(MAX_AGE_DAYS * 24 * 60 * 60),
        }
    }

    /// Save full tool output to disk. Returns a recall message if successful,
    /// empty string on failure.
    pub fn save(&self, tool_name: &str, content: &str) -> String {
        if self.project_dir.as_os_str().is_empty() {
            return String::new();
        }

        let dir = self.project_dir.join(".claude").join("truncated-results");
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("[TruncatedResultSaver] failed to create dir: {}", e);
            return String::new();
        }

        let timestamp = Local::now().format("%Y%m%d-%H%M%S");
        let random_hex = short_uuid8();
        let sanitized_name = sanitize_tool_name(tool_name);
        let filename = format!("{}-{}-{}.txt", timestamp, random_hex, sanitized_name);
        let file_path = dir.join(&filename);

        if let Err(e) = fs::write(&file_path, content) {
            eprintln!("[TruncatedResultSaver] failed to write file: {}", e);
            return String::new();
        }

        let relative_path = format!(
            ".claude/truncated-results/{}",
            filename
        );
        format!(
            "Full output saved to {} (use read_file to recall)",
            relative_path
        )
    }

    /// Clean up old truncated result files.
    pub fn cleanup_old(&self) {
        if self.project_dir.as_os_str().is_empty() {
            return;
        }

        let dir = self.project_dir.join(".claude").join("truncated-results");
        if !dir.exists() {
            return;
        }

        let now = SystemTime::now();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(duration) = now.duration_since(modified) {
                            if duration > self.max_age {
                                let _ = fs::remove_file(entry.path());
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Sanitize tool name for filesystem compatibility.
fn sanitize_tool_name(name: &str) -> String {
    let illegal_chars = ['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '];
    let mut result = String::with_capacity(name.len());
    for c in name.chars() {
        if illegal_chars.contains(&c) {
            result.push('_');
        } else {
            result.push(c);
        }
    }
    result
}

/// Generate 4-byte random hex string (8 hex chars).
fn short_uuid8() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 4] = rng.gen();
    format!("{:02x}{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2], bytes[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_tool_name() {
        assert_eq!(sanitize_tool_name("exec"), "exec");
        assert_eq!(sanitize_tool_name("edit_file"), "edit_file");
        assert_eq!(sanitize_tool_name("foo/bar"), "foo_bar");
        assert_eq!(sanitize_tool_name("foo:bar"), "foo_bar");
        assert_eq!(sanitize_tool_name("foo*bar"), "foo_bar");
        assert_eq!(sanitize_tool_name("foo bar"), "foo_bar");
    }

    #[test]
    fn test_short_uuid8() {
        let uuid = short_uuid8();
        assert_eq!(uuid.len(), 8);
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit()));
    }
}