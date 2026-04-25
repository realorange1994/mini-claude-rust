//! FileWriteTool - Write content to a file

use crate::tools::{Tool, ToolResult, expand_path};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

const MAX_WRITE_SIZE: usize = 10 * 1024 * 1024; // 10MB

pub struct FileWriteTool;

impl FileWriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileWriteTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if they don't exist. Overwrites if the file already exists."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write."
                }
            },
            "required": ["path", "content"]
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

        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Error: content is required"),
        };

        if content.len() > MAX_WRITE_SIZE {
            return ToolResult::error(format!(
                "Error: content too large ({} bytes, max {} bytes)",
                content.len(),
                MAX_WRITE_SIZE
            ));
        }

        // Create parent directories
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return ToolResult::error(format!("Error creating directory: {}", e));
            }
        }

        // Write file
        if let Err(e) = fs::write(&path, content) {
            return ToolResult::error(format!("Error writing file: {}", e));
        }

        ToolResult::ok(format!("Wrote {} chars to {}", content.len(), path.display()))
    }
}

