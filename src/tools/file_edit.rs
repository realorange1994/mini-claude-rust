//! FileEditTool - Edit a file by replacing exact strings

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct FileEditTool;

impl FileEditTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for FileEditTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string with a new string. Provide enough context in old_string to uniquely identify the target."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find (must be unique in the file)."
                },
                "new_string": {
                    "type": "string",
                    "description": "Text to replace it with."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)."
                }
            },
            "required": ["path", "old_string", "new_string"]
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

        let old_str = match params.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Error: old_string must not be empty"),
        };

        let new_str = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("Error: file not found: {}", path.display()))
            }
            Err(e) => return ToolResult::error(format!("Error reading file: {}", e)),
        };

        // Normalize CRLF
        let mut content = content.replace("\r\n", "\n");
        let mut old_str = old_str.replace("\r\n", "\n");
        let mut new_str = new_str.replace("\r\n", "\n");
        let has_crlf = content.contains('\r');

        let count = content.matches(&old_str).count();
        if count == 0 {
            return ToolResult::error(format!(
                "Error: old_text not found in {}. Verify the file content.",
                path.display()
            ));
        }

        if count > 1 && !replace_all {
            return ToolResult::error(format!(
                "Warning: old_text appears {} times. Provide more context or set replace_all=true.",
                count
            ));
        }

        if replace_all {
            content = content.replace(&old_str, &new_str);
        } else {
            if let Some(pos) = content.find(&old_str) {
                content = format!("{}{}{}", &content[..pos], new_str, &content[pos + old_str.len()..]);
            }
        }

        // Restore CRLF if original had it
        if has_crlf {
            content = restore_crlf(&content);
        }

        if let Err(e) = fs::write(&path, &content) {
            return ToolResult::error(format!("Error writing file: {}", e));
        }

        ToolResult::ok(format!("Successfully edited {}", path.display()))
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
