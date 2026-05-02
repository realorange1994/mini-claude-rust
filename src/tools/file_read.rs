//! FileReadTool - Read file contents with optional line range

use crate::tools::{Tool, ToolResult, expand_path};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

const MAX_FILE_SIZE: u64 = 256 * 1024; // 256 KB, matching Claude Code official

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
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from. Only provide if the file is too large to read at once."
                },
                "limit": {
                    "type": "integer",
                    "description": "The number of lines to read. Only provide if the file is too large to read at once."
                }
            },
            "required": ["file_path"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = params.get("file_path")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("path").and_then(|v| v.as_str()));
        let path = match path {
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
            return ToolResult::error("Error: file too large (>256 KB). Use offset and limit parameters to read specific portions.".to_string());
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        // Strip UTF-8 BOM (matching official Claude Code behavior)
        let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
        let content = content.replace("\r\n", "\n");
        let mut lines: Vec<&str> = content.lines().collect();
        
        // Remove trailing empty element
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        let total = lines.len();

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
            .map(|v| if v < 1 { 1 } else { v as usize })
            .unwrap_or(1);

        // Official: limit=0 or missing means read entire file
        let limit = params
            .get("limit")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok())))
            .map(|v| if v <= 0 { total } else { v as usize })
            .unwrap_or(total); // default: read entire file (matching Claude Code official)

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

