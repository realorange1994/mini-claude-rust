//! GlobTool - Find files matching glob patterns

use crate::tools::{Tool, ToolResult, expand_path, is_ignored_dir, is_unc_path};
use glob::glob;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for GlobTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Fast file pattern matching tool that works with any codebase size. \
         Supports glob patterns like \"**/*.js\" or \"src/**/*.ts\". \
         Returns matching file paths sorted by modification time. \
         Use this tool when you need to find files by name patterns. \
         When you are doing an open ended search that may require multiple rounds of globbing and grepping, use the Agent tool instead."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g. '**/*.py'). Patterns without '**/' are auto-prefixed."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 100)."
                },
                "excludes": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Glob patterns to exclude (files/dirs matching any are skipped, e.g. ['*.test.go', 'vendor'])."
                }
            },
            "required": ["pattern"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required"),
        };

        // Support path (official) and directory (legacy alias)
        let dir = params
            .get("path")
            .or_else(|| params.get("directory"))
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let head_limit = params
            .get("head_limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(100)
            .max(1) as usize;

        let excludes: Vec<String> = params
            .get("excludes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let base_dir = expand_path(dir);

        // SECURITY: Skip filesystem operations for UNC paths to prevent NTLM credential leaks.
        if is_unc_path(&base_dir) {
            return ToolResult::error(format!("Error: UNC path access deferred: {}", base_dir.display()));
        }

        if !base_dir.is_dir() {
            return ToolResult::error(format!("Error: directory not found: {}", base_dir.display()));
        }

        // Auto-prefix with **/ if pattern has no slash
        let pattern = if !pattern.contains('/') && !pattern.starts_with("**/") {
            format!("**/{}", pattern)
        } else {
            pattern.to_string()
        };

        let full_pattern = base_dir.join(&pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();

        let mut matches: Vec<(PathBuf, std::io::Result<std::fs::Metadata>)> = Vec::new();

        for entry in glob(&pattern_str).into_iter().flatten().flatten() {
            if entry.is_file() {
                // Check excludes
                let relative = entry.strip_prefix(&base_dir).unwrap_or(&entry);
                let relative_str = relative.to_string_lossy();

                let should_exclude = excludes.iter().any(|ex| {
                    glob_match(ex, &relative_str) || glob_match(ex, &relative.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())
                });

                // Also skip files inside ignored directories
                let in_ignored_dir = relative
                    .components()
                    .any(|c| {
                        if let std::path::Component::Normal(name) = c {
                            is_ignored_dir(name)
                        } else {
                            false
                        }
                    });

                if !should_exclude && !in_ignored_dir {
                    let metadata = entry.metadata();
                    matches.push((entry, metadata));
                }
            }
        }

        if matches.is_empty() {
            return ToolResult::ok("No files matched.".to_string());
        }

        // Sort by modification time (newest first)
        matches.sort_by(|a, b| {
            let time_a = a.1.as_ref().ok().and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
                .unwrap_or(0);
            let time_b = b.1.as_ref().ok().and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
                .unwrap_or(0);
            time_b.cmp(&time_a)
        });

        let total = matches.len();
        let matches: Vec<_> = matches.into_iter().take(head_limit).collect();

        let mut lines = Vec::new();
        for (path, _meta) in matches {
            lines.push(path.display().to_string());
        }

        if total > head_limit {
            lines.push(format!("(showing first {} of {} matches)", head_limit, total));
        }

        ToolResult::ok(lines.join("\n"))
    }
}


fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern = glob::Pattern::new(pattern);
    match pattern {
        Ok(p) => p.matches(name),
        Err(_) => false,
    }
}
