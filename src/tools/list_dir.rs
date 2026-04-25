//! ListDirTool - List directory contents

use crate::tools::{Tool, ToolResult, expand_path, is_ignored_dir};
use serde_json::Value;
use std::collections::HashMap;

pub struct ListDirTool;

impl ListDirTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListDirTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ListDirTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List directory contents. Shows files and subdirectories. Supports recursive listing with ignored directories (.git, node_modules, etc.)."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to list (default: current directory)."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Recursively list subdirectories (default: false)."
                },
                "max_entries": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (default: 200)."
                }
            },
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let recursive = params
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let max_entries = params
            .get("max_entries")
            .and_then(|v| v.as_i64())
            .unwrap_or(200)
            .max(1) as usize;

        let dir = expand_path(path);
        if !dir.is_dir() {
            return ToolResult::error(format!("Error: not a directory: {}", dir.display()));
        }

        if recursive {
            list_dir_recursive(&dir, max_entries)
        } else {
            list_dir_simple(&dir, max_entries)
        }
    }
}

fn list_dir_simple(dir: &std::path::Path, max_entries: usize) -> ToolResult {
    let mut entries = Vec::new();
    let mut total = 0;

    match std::fs::read_dir(dir) {
        Ok(read_dir) => {
            for entry in read_dir.flatten() {
                total += 1;
                if entries.len() < max_entries {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let display = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        format!("{}/", name)
                    } else {
                        name
                    };
                    entries.push(display);
                }
            }
        }
        Err(e) => return ToolResult::error(format!("Error reading directory: {}", e)),
    }

    if entries.is_empty() && total == 0 {
        return ToolResult::ok(format!("Directory {} is empty", dir.display()));
    }

    let result = entries.join("\n");
    if total > max_entries {
        ToolResult::ok(format!("{}\n\n(truncated, showing first {} of {} entries)",
            result, max_entries, total))
    } else {
        ToolResult::ok(result)
    }
}

fn list_dir_recursive(dir: &std::path::Path, max_entries: usize) -> ToolResult {
    let mut entries = Vec::new();
    let mut total = 0;

    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e.file_name()))
        .for_each(|e| {
            if let Ok(entry) = e {
                let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
                if rel.as_os_str() != "." {
                    total += 1;
                    if entries.len() < max_entries {
                        let is_dir = entry.metadata().map(|m| m.is_dir()).unwrap_or(false);
                        let display = if is_dir {
                            format!("{}/", rel.display())
                        } else {
                            rel.display().to_string()
                        };
                        entries.push(display);
                    }
                }
            }
        });

    if entries.is_empty() && total == 0 {
        return ToolResult::ok(format!("Directory {} is empty", dir.display()));
    }

    let result = entries.join("\n");
    if total > max_entries {
        ToolResult::ok(format!("{}\n\n(truncated, showing first {} of {} entries)",
            result, max_entries, total))
    } else {
        ToolResult::ok(result)
    }
}
