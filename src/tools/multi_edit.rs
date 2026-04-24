//! MultiEditTool - Apply multiple search/replace edits atomically

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct MultiEditTool;

impl MultiEditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MultiEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for MultiEditTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn description(&self) -> &str {
        "Apply multiple search/replace edits to a file atomically. If any edit fails, all are rolled back. Accepts a list of {old_string, new_string} pairs."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "edits": {
                    "type": "array",
                    "description": "List of {old_string, new_string} edit operations.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "Exact text to find."
                            },
                            "new_string": {
                                "type": "string",
                                "description": "Text to replace it with."
                            }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edits"]
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

        let edits_raw = match params.get("edits") {
            Some(v) => v,
            None => return ToolResult::error("Error: edits is required"),
        };

        let edits_array = match edits_raw.as_array() {
            Some(arr) => arr,
            None => return ToolResult::error("Error: edits must be an array"),
        };

        if edits_array.is_empty() {
            return ToolResult::error("Error: edits must not be empty");
        }

        let mut edits = Vec::new();
        for (i, e) in edits_array.iter().enumerate() {
            let m = match e.as_object() {
                Some(m) => m,
                None => return ToolResult::error(format!("Error: edit {} must be an object", i + 1)),
            };

            let old_str = match m.get("old_string").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return ToolResult::error(format!("Error: edit {}: old_string must not be empty", i + 1)),
            };

            let new_str = m.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            edits.push((old_str.to_string(), new_str.to_string()));
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        // Normalize CRLF
        let mut content = content.replace("\r\n", "\n");
        let mut has_crlf = content.contains('\r');
        
        for (old, new) in &mut edits {
            *old = old.replace("\r\n", "\n");
            *new = new.replace("\r\n", "\n");
        }

        // Dry run: validate all edits
        let mut test_content = content.clone();
        for (i, (old, _)) in edits.iter().enumerate() {
            if !test_content.contains(old) {
                return ToolResult::error(format!(
                    "Error: edit {} failed: old_text not found: {:?}",
                    i + 1,
                    truncate(old, 80)
                ));
            }
            if let Some(pos) = test_content.find(old) {
                test_content = format!(
                    "{}{}{}",
                    &test_content[..pos],
                    &edits[i].1,
                    &test_content[pos + old.len()..]
                );
            }
        }

        // Apply atomically
        for (old, new) in &edits {
            content = content.replacen(old, new, 1);
        }

        if has_crlf {
            content = restore_crlf(&content);
        }

        if let Err(e) = fs::write(&path, &content) {
            return ToolResult::error(format!("Error writing file: {}", e));
        }

        ToolResult::ok(format!("Applied {} edits to {}", edits.len(), path.display()))
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

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

fn restore_crlf(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + s.len() / 10);
    for (i, c) in s.chars().enumerate() {
        if c == '\n' && (i == 0 || s.chars().nth(i - 1) != Some('\r')) {
            result.push('\r');
        }
        result.push(c);
    }
    result
}
