//! FileReadTool - Read file contents with optional line range

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024; // 2 MB
const READ_FILE_DEFAULT_LIMIT: usize = 2000;
const READ_FILE_MAX_CHARS: usize = 15000;

pub struct FileReadTool;

impl FileReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileReadTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns numbered lines for easy reference."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file."
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based start line (optional)."
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of lines to read (optional)."
                }
            },
            "required": ["path"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => expand_path(p),
            None => return ToolResult::error("Error: path is required"),
        };

        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        if metadata.is_dir() {
            return ToolResult::error(format!("Error: not a file: {}", path.display()));
        }

        if metadata.len() > MAX_FILE_SIZE {
            return ToolResult::error(format!("Error: file too large (>{} bytes)", MAX_FILE_SIZE));
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        let content = content.replace("\r\n", "\n");
        let mut lines: Vec<&str> = content.lines().collect();
        
        // Remove trailing empty element
        if lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }

        let total = lines.len();

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(1) as usize)
            .unwrap_or(1);

        let limit = params
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(1) as usize)
            .unwrap_or(READ_FILE_DEFAULT_LIMIT);

        if offset > total {
            return ToolResult::error(format!(
                "Error: offset {} is beyond end of file ({} lines)",
                offset, total
            ));
        }

        let start = offset.saturating_sub(1);
        let end = (start + limit).min(total);
        let selected = &lines[start..end];

        let mut result = String::new();
        for (i, line) in selected.iter().enumerate() {
            result.push_str(&format!("{}| {}\n", offset + i, line));
        }

        // Truncate if too many chars
        if result.len() > READ_FILE_MAX_CHARS {
            result.truncate(READ_FILE_MAX_CHARS);
            result.push_str("\n\n[OUTPUT TRUNCATED]");
        }

        // Add pagination hint
        if end < total {
            result.push_str(&format!(
                "\n\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
                offset,
                end,
                total,
                end + 1
            ));
        } else {
            result.push_str(&format!("\n\n(End of file - {} lines total)", total));
        }

        ToolResult::ok(result.trim_end().to_string())
    }
}

fn expand_path(p: &str) -> std::path::PathBuf {
    let p = if p.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            p.replacen('~', &home, 1)
        } else {
            p.to_string()
        }
    } else {
        p.to_string()
    };

    let path = std::path::Path::new(&p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| Path::new(".").to_path_buf())
            .join(path)
    }
}
