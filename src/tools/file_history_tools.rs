//! File history tools - undo/rewind functionality with read/grep/glob support

use crate::tools::{Tool, ToolResult, expand_path, truncate_at};
use crate::filehistory::FileHistory;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub struct FileHistoryTool {
    history: Arc<FileHistory>,
}

impl FileHistoryTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryTool {
    fn name(&self) -> &str {
        "file_history"
    }

    fn description(&self) -> &str {
        "List version history for files. Usage: (1) With 'path': show version history for that file. (2) Without 'path': list all files with history. Supports 'pattern' glob filter (e.g., '*.rs'), 'offset' and 'limit' for pagination. Each version shows timestamp and size. Use file_history_read to view content, file_restore/file_rewind to restore."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to show history for. If not provided, lists all files with history."
                },
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs', 'src/**/*.py'). Only used when path is not provided."
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip first N versions (for pagination). Default: 0.",
                    "minimum": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of versions to show. Default: 10.",
                    "minimum": 1
                }
            },
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        // If path is provided, show history for that file
        if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
            let full_path = expand_path(path);
            let count = self.history.count(&full_path);

            if count == 0 {
                return ToolResult::ok(format!(
                    "No history for: {}\n(Snapshots are created automatically before write/edit operations)",
                    full_path.display()
                ));
            }

            let snapshots = self.history.get_snapshots(&full_path);
            let total = snapshots.len();
            let start = offset.min(total);
            let end = (start + limit).min(total);

            let mut output = format!(
                "History for: {} ({} versions, showing {}-{})\n\n",
                full_path.display(),
                total,
                start + 1,
                end
            );

            for (i, snap) in snapshots.iter().skip(start).take(end - start).enumerate() {
                let version_num = start + i + 1;
                let version = if version_num == total {
                    "current"
                } else {
                    &format!("v{}", version_num)
                };
                output.push_str(&format!(
                    "[{}] {} - {} bytes\n",
                    version,
                    snap.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    snap.content.len()
                ));
            }

            if end < total {
                output.push_str(&format!("\n... {} more versions. Use offset={} to see more.\n", total - end, end));
            }

            output.push_str("\nUse file_history_read to view a specific version, file_restore to undo last change.");
            ToolResult::ok(output)
        } else {
            // List all files with history, optionally filtered by glob pattern
            let pattern = params.get("pattern").and_then(|v| v.as_str());
            let all_files = self.history.list_all_files();

            let filtered: Vec<_> = if let Some(pattern) = pattern {
                let glob_pattern = glob::Pattern::new(pattern).unwrap_or_else(|_| {
                    glob::Pattern::new("*").unwrap()
                });
                all_files.into_iter()
                    .filter(|p| glob_pattern.matches(&p.to_string_lossy()))
                    .collect()
            } else {
                all_files
            };

            if filtered.is_empty() {
                return ToolResult::ok("No files with history found.");
            }

            let total = filtered.len();
            let start = offset.min(total);
            let end = (start + limit).min(total);

            let mut output = format!(
                "Files with history ({} total, showing {}-{})\n\n",
                total,
                start + 1,
                end
            );

            for path in filtered.iter().skip(start).take(end - start) {
                let count = self.history.count(path);
                output.push_str(&format!("{} ({} versions)\n", path.display(), count));
            }

            if end < total {
                output.push_str(&format!("\n... {} more files. Use offset={} to see more.\n", total - end, end));
            }

            output.push_str("\nUse file_history --path <file> to see version details.");
            ToolResult::ok(output)
        }
    }
}

pub struct FileHistoryReadTool {
    history: Arc<FileHistory>,
}

impl FileHistoryReadTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryReadTool {
    fn name(&self) -> &str {
        "file_history_read"
    }

    fn description(&self) -> &str {
        "Read content from a specific version of a file in history. Parameters: 'path' (required), 'version' (1=oldest, omit for current), 'offset' (line number, 1-indexed), 'limit' (max lines, default 2000). Use file_history first to see available versions. Output includes line numbers and pagination hints."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file."
                },
                "version": {
                    "type": "integer",
                    "description": "Version number to read (1 = oldest, omit for current). Default: current version.",
                    "minimum": 1
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed). Default: 1.",
                    "minimum": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read. Default: 2000.",
                    "minimum": 1
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
            Some(p) => p,
            None => return ToolResult::error("Error: path is required"),
        };

        let full_path = expand_path(path);
        let snapshots = self.history.get_snapshots(&full_path);

        if snapshots.is_empty() {
            return ToolResult::error(format!("No history for: {}", full_path.display()));
        }

        // Get version (default to current/last)
        let version = params.get("version").and_then(|v| v.as_u64()).unwrap_or(snapshots.len() as u64) as usize;
        if version == 0 || version > snapshots.len() {
            return ToolResult::error(format!(
                "Invalid version {}. Available versions: 1-{}",
                version,
                snapshots.len()
            ));
        }

        let snapshot = &snapshots[version - 1];
        let content = &snapshot.content;

        // Pagination
        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start = (offset.saturating_sub(1)).min(total_lines);
        let end = (start + limit).min(total_lines);

        let mut output = format!(
            "File: {} (version {}/{})\nTimestamp: {}\nLines {}-{} of {}\n\n",
            full_path.display(),
            version,
            snapshots.len(),
            snapshot.timestamp.format("%Y-%m-%d %H:%M:%S"),
            start + 1,
            end,
            total_lines
        );

        for (i, line) in lines.iter().skip(start).take(end - start).enumerate() {
            output.push_str(&format!("{:6}\t{}\n", start + i + 1, line));
        }

        if end < total_lines {
            output.push_str(&format!(
                "\n... {} more lines. Use offset={} to continue reading.",
                total_lines - end,
                end + 1
            ));
        }

        ToolResult::ok(output)
    }
}

pub struct FileHistoryGrepTool {
    history: Arc<FileHistory>,
}

impl FileHistoryGrepTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryGrepTool {
    fn name(&self) -> &str {
        "file_history_grep"
    }

    fn description(&self) -> &str {
        "Search within file history using regex. Parameters: 'pattern' (required, regex), 'path' (optional, searches all files if omitted), 'version' (optional, searches all versions if omitted), 'context' (lines around match, default 2), 'ignore_case' (default false). Output format: file:version:line:content. Useful for finding when code was changed or deleted."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Path to the file to search. If not provided, searches all files with history."
                },
                "version": {
                    "type": "integer",
                    "description": "Specific version to search (1 = oldest). If not provided, searches all versions.",
                    "minimum": 1
                },
                "context": {
                    "type": "integer",
                    "description": "Number of context lines to show before and after match. Default: 2.",
                    "minimum": 0
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case insensitive search. Default: false."
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

        let context = params.get("context").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
        let ignore_case = params.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);

        // Build regex
        let re = if ignore_case {
            Regex::new(&format!("(?i){}", pattern))
        } else {
            Regex::new(pattern)
        };

        let re = match re {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid regex pattern: {}", e)),
        };

        let mut output = String::new();
        let mut total_matches = 0;

        // Search specific file
        if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
            let full_path = expand_path(path);
            let snapshots = self.history.get_snapshots(&full_path);

            if snapshots.is_empty() {
                return ToolResult::error(format!("No history for: {}", full_path.display()));
            }

            let version = params.get("version").and_then(|v| v.as_u64());

            let versions_to_search: Vec<(usize, &crate::filehistory::FileSnapshot)> = if let Some(v) = version {
                let v = v as usize;
                if v == 0 || v > snapshots.len() {
                    return ToolResult::error(format!(
                        "Invalid version {}. Available versions: 1-{}",
                        v,
                        snapshots.len()
                    ));
                }
                vec![(v, &snapshots[v - 1])]
            } else {
                snapshots.iter().enumerate().map(|(i, s)| (i + 1, s)).collect()
            };

            for (ver, snap) in versions_to_search {
                let lines: Vec<&str> = snap.content.lines().collect();
                for (line_num, line) in lines.iter().enumerate() {
                    if re.is_match(line) {
                        total_matches += 1;
                        output.push_str(&format!(
                            "\n{}:v{}:{}: {}\n",
                            full_path.display(),
                            ver,
                            line_num + 1,
                            line
                        ));

                        // Context before
                        for ctx in (0..context).rev() {
                            if line_num > ctx {
                                let ctx_line = lines[line_num - ctx - 1];
                                output.push_str(&format!("  {:6}\t{}\n", line_num - ctx, ctx_line));
                            }
                        }

                        // Match line
                        output.push_str(&format!("> {:6}\t{}\n", line_num + 1, line));

                        // Context after
                        for ctx in 1..=context {
                            if line_num + ctx < lines.len() {
                                let ctx_line = lines[line_num + ctx];
                                output.push_str(&format!("  {:6}\t{}\n", line_num + ctx + 1, ctx_line));
                            }
                        }
                    }
                }
            }
        } else {
            // Search all files
            let all_files = self.history.list_all_files();

            for file_path in all_files {
                let snapshots = self.history.get_snapshots(&file_path);

                for (ver, snap) in snapshots.iter().enumerate() {
                    let lines: Vec<&str> = snap.content.lines().collect();
                    for (line_num, line) in lines.iter().enumerate() {
                        if re.is_match(line) {
                            total_matches += 1;
                            output.push_str(&format!(
                                "{}:v{}:{}: {}\n",
                                file_path.display(),
                                ver + 1,
                                line_num + 1,
                                truncate_at(line, 200)
                            ));
                        }
                    }
                }
            }
        }

        if total_matches == 0 {
            ToolResult::ok(format!("No matches found for pattern: {}", pattern))
        } else {
            ToolResult::ok(format!("Found {} matches:\n{}", total_matches, output))
        }
    }
}

pub struct FileRestoreTool {
    history: Arc<FileHistory>,
}

impl FileRestoreTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileRestoreTool {
    fn name(&self) -> &str {
        "file_restore"
    }

    fn description(&self) -> &str {
        "Restore a file to its previous version (undo last write/edit). Only goes back one version. For multiple versions back, use file_rewind. Returns preview of restored content. Use file_history first to check available versions."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to restore."
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
            Some(p) => p,
            None => return ToolResult::error("Error: path is required"),
        };

        let full_path = expand_path(path);

        match self.history.restore(&full_path) {
            Ok(Some(content)) => {
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                ToolResult::ok(format!(
                    "Restored: {}\nContent preview:\n{}",
                    full_path.display(),
                    preview
                ))
            }
            Ok(None) => ToolResult::error(format!(
                "No previous version available for: {}",
                full_path.display()
            )),
            Err(e) => ToolResult::error(format!("Error restoring file: {}", e)),
        }
    }
}

pub struct FileRewindTool {
    history: Arc<FileHistory>,
}

impl FileRewindTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileRewindTool {
    fn name(&self) -> &str {
        "file_rewind"
    }

    fn description(&self) -> &str {
        "Rewind a file N versions back. Parameters: 'path' (required), 'steps' (required, how many versions to go back: 1=previous, 2=two versions back, etc.). Use file_history first to see available versions. Returns preview of rewound content. For single version undo, use file_restore."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to rewind."
                },
                "steps": {
                    "type": "integer",
                    "description": "Number of versions to go back (1 = previous, 2 = two versions back, etc.)",
                    "minimum": 1
                }
            },
            "required": ["path", "steps"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: path is required"),
        };

        let steps = match params.get("steps").and_then(|v| v.as_u64()) {
            Some(s) => s as usize,
            None => return ToolResult::error("Error: steps is required and must be a positive integer"),
        };

        if steps == 0 {
            return ToolResult::error("Error: steps must be at least 1");
        }

        let full_path = expand_path(path);

        match self.history.rewind(&full_path, steps) {
            Ok(Some(content)) => {
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                ToolResult::ok(format!(
                    "Rewound {} step(s): {}\nContent preview:\n{}",
                    steps,
                    full_path.display(),
                    preview
                ))
            }
            Ok(None) => ToolResult::error(format!(
                "Cannot rewind {} step(s) for: {}. Not enough history available.",
                steps,
                full_path.display()
            )),
            Err(e) => ToolResult::error(format!("Error rewinding file: {}", e)),
        }
    }
}
