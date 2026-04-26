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
                    "No history for: {}\nSnapshots are created automatically before and after write/edit/multi_edit operations.\nA file must be modified at least once to have history.",
                    full_path.display()
                ));
            }

            let snapshots = self.history.get_snapshots(&full_path);
            let total = snapshots.len();
            let start = offset.min(total);
            let end = (start + limit).min(total);

            let mut output = format!(
                "History for: {} ({} versions, showing {}-{}){}\n\n",
                full_path.display(),
                total,
                start + 1,
                end,
                if !full_path.exists() { " [FILE DELETED]" } else { "" }
            );

            for (i, snap) in snapshots.iter().skip(start).take(end - start).enumerate() {
                let version_num = start + i + 1;
                let label = if version_num == total {
                    format!("v{} (current)", version_num)
                } else {
                    format!("v{}", version_num)
                };

                // Detect if this is a "before" snapshot (same content as next version)
                // These are pre-execution snapshots that didn't change content — merge display with next
                let is_before = i + start + 1 < total
                    && snapshots[i + start].checksum == snapshots[i + start + 1].checksum;

                // Detect if previous snapshot had same checksum (was merged "before")
                let is_after_merge = i + start > 0
                    && snapshots[i + start - 1].checksum == snap.checksum;

                let desc = if snap.description.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", snap.description)
                };

                // For "before" snapshots, show a compact merged line with the next version
                if is_before {
                    let next_desc = &snapshots[i + start + 1].description;
                    // Show only the "after" description (the meaningful one about what changed)
                    let merged_desc = if next_desc.is_empty() {
                        desc.clone()
                    } else {
                        format!(" - {}", next_desc)
                    };
                    output.push_str(&format!(
                        "[{}] {} - {} bytes{} (merged)\n",
                        label,
                        snap.timestamp.format("%Y-%m-%d %H:%M:%S"),
                        snap.content.len(),
                        merged_desc
                    ));
                    continue;
                }

                // For "after" snapshots that were already merged with previous, skip
                if is_after_merge {
                    continue;
                }

                output.push_str(&format!(
                    "[{}] {} - {} bytes{}\n",
                    label,
                    snap.timestamp.format("%Y-%m-%d %H:%M:%S"),
                    snap.content.len(),
                    desc
                ));
            }

            if end < total {
                output.push_str(&format!("\n... {} more versions. Use offset={} to see more.\n", total - end, end));
            }

            output.push_str("\nUse file_history_read to view a specific version, file_history_diff to see changes between versions, file_restore to undo last change.");
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
                "Invalid version {}. Available versions: 1-{} (omit 'version' to read current/latest)",
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

// ─── P0: file_history_diff ───

pub struct FileHistoryDiffTool {
    history: Arc<FileHistory>,
}

impl FileHistoryDiffTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryDiffTool {
    fn name(&self) -> &str {
        "file_history_diff"
    }

    fn description(&self) -> &str {
        "Show diff between two versions of a file. Parameters: 'path' (required), 'from' (version: v3, last1, current, or tag name), 'to' (version specifier, default: current). Shows added/removed/changed lines in unified-diff format. Essential for understanding what changed between versions."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file."
                },
                "from": {
                    "type": "string",
                    "description": "Starting version (v1, v3, current, last1, or tag name). Default: previous version."
                },
                "to": {
                    "type": "string",
                    "description": "Ending version (v1, v3, current, last1, or tag name). Default: current version."
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
        let total = self.history.count(&full_path);
        if total == 0 {
            return ToolResult::error(format!("No history for: {}", full_path.display()));
        }

        // Resolve from version (default: previous = total - 1)
        let from_spec = params.get("from").and_then(|v| v.as_str()).unwrap_or("last1");
        let from_ver = if params.contains_key("from") {
            self.history.resolve_version(&full_path, from_spec)
        } else {
            // Default: previous version
            if total >= 2 { Some(total - 1) } else { Some(1) }
        };

        let to_spec = params.get("to").and_then(|v| v.as_str()).unwrap_or("current");
        let to_ver = if params.contains_key("to") {
            self.history.resolve_version(&full_path, to_spec)
        } else {
            Some(total)
        };

        let from_ver = match from_ver {
            Some(v) => v,
            None => return ToolResult::error(format!(
                "Cannot resolve 'from' version '{}' for {}. Use file_history to see available versions.", from_spec, full_path.display()
            )),
        };
        let to_ver = match to_ver {
            Some(v) => v,
            None => return ToolResult::error(format!(
                "Cannot resolve 'to' version '{}' for {}. Use file_history to see available versions.", to_spec, full_path.display()
            )),
        };

        if from_ver == to_ver {
            return ToolResult::ok(format!("v{} and v{} are the same version. No differences.", from_ver, to_ver));
        }

        let diff_result = match self.history.diff(&full_path, from_ver, to_ver) {
            Some(d) => d,
            None => return ToolResult::error("Failed to compute diff."),
        };

        let mut output = format!("Diff: {} (v{} → v{})\n\n", full_path.display(), from_ver, to_ver);

        if diff_result.hunks.is_empty() {
            output.push_str("No differences found.\n");
            return ToolResult::ok(output);
        }

        let mut total_added = 0usize;
        let mut total_removed = 0usize;

        for hunk in &diff_result.hunks {
            output.push_str(&format!("@@ -{},{} +{},{} @@\n",
                hunk.from_line, hunk.from_count,
                hunk.to_line, hunk.to_count));
            for line in &hunk.lines {
                if line.starts_with('+') && !line.starts_with("++") {
                    total_added += 1;
                } else if line.starts_with('-') && !line.starts_with("--") {
                    total_removed += 1;
                }
                output.push_str(line);
                output.push('\n');
            }
        }

        output.push_str(&format!("\nSummary: +{} lines added, -{} lines removed", total_added, total_removed));
        ToolResult::ok(output)
    }
}

// ─── P1: file_history_search (added/removed/changed) ───

pub struct FileHistorySearchTool {
    history: Arc<FileHistory>,
}

impl FileHistorySearchTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistorySearchTool {
    fn name(&self) -> &str {
        "file_history_search"
    }

    fn description(&self) -> &str {
        "Search for when text was added, removed, or changed across versions. Parameters: 'path' (required), 'query' (required, text to search for), 'mode' (optional: 'added', 'removed', or 'changed'. Default: 'changed'), 'ignore_case' (optional, default: false). Shows which versions introduced or removed the matching text."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to search."
                },
                "query": {
                    "type": "string",
                    "description": "Text to search for (literal string, not regex)."
                },
                "mode": {
                    "type": "string",
                    "description": "Search mode: 'added' (text was added), 'removed' (text was removed), or 'changed' (either). Default: 'changed'.",
                    "enum": ["added", "removed", "changed"]
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case insensitive search. Default: false."
                }
            },
            "required": ["path", "query"]
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

        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Error: query is required"),
        };

        let mode_str = params.get("mode").and_then(|v| v.as_str()).unwrap_or("changed");
        let mode = match mode_str {
            "added" => crate::filehistory::SearchMode::Added,
            "removed" => crate::filehistory::SearchMode::Removed,
            _ => crate::filehistory::SearchMode::Changed,
        };

        let ignore_case = params.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);

        let full_path = expand_path(path);
        if self.history.count(&full_path) == 0 {
            return ToolResult::error(format!("No history for: {}", full_path.display()));
        }

        let results = self.history.search(&full_path, query, mode, ignore_case);

        if results.is_empty() {
            return ToolResult::ok(format!(
                "No versions where '{}' was {} in: {}",
                query, mode_str, full_path.display()
            ));
        }

        let mut output = format!("Versions where '{}' was {} in {}:\n\n", query, mode_str, full_path.display());
        for (ver, details) in &results {
            output.push_str(&format!("v{}:\n{}\n\n", ver, details));
        }

        ToolResult::ok(output)
    }
}

// ─── P1: file_history_summary ───

pub struct FileHistorySummaryTool {
    history: Arc<FileHistory>,
}

impl FileHistorySummaryTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistorySummaryTool {
    fn name(&self) -> &str {
        "file_history_summary"
    }

    fn description(&self) -> &str {
        "Show a summary of all files with history and their change counts. Parameters: 'since' (optional, time filter like '1h', '30m', '1d' for last 1 hour/30 minutes/1 day). Shows each file's version count and latest change description. Useful for getting an overview of what changed in the session."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "since": {
                    "type": "string",
                    "description": "Time filter: show changes since this time ago. Examples: '1h' (1 hour), '30m' (30 minutes), '1d' (1 day). Default: show all."
                }
            },
            "required": []
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let since = params.get("since").and_then(|v| v.as_str())
            .and_then(|s| parse_duration(s));

        let summary = self.history.get_summary(since);

        if summary.is_empty() {
            let since_msg = since.map(|s| format!(" since {}", s.format("%Y-%m-%d %H:%M"))).unwrap_or_default();
            return ToolResult::ok(format!("No files with history found{}.", since_msg));
        }

        let mut output = format!("Files with history ({} files):\n\n", summary.len());

        for (path, snaps) in &summary {
            let deleted = if !path.exists() { " [DELETED]" } else { "" };
            let last = snaps.last();
            let latest_desc = last.map(|s| {
                if s.description.is_empty() {
                    format!("{} bytes", s.content.len())
                } else {
                    s.description.clone()
                }
            }).unwrap_or_default();
            let latest_time = last.map(|s| s.timestamp.format("%H:%M:%S").to_string()).unwrap_or_default();

            output.push_str(&format!(
                "{} ({} versions, latest: {} at {}){}\n",
                path.display(),
                snaps.len(),
                latest_desc,
                latest_time,
                deleted
            ));
        }

        ToolResult::ok(output)
    }
}

// ─── P1: file_history_timeline ───

pub struct FileHistoryTimelineTool {
    history: Arc<FileHistory>,
}

impl FileHistoryTimelineTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryTimelineTool {
    fn name(&self) -> &str {
        "file_history_timeline"
    }

    fn description(&self) -> &str {
        "Show a chronological timeline of all file changes across all files. Parameters: 'since' (optional, time filter like '1h', '30m', '1d'), 'limit' (optional, max entries, default 20). Useful for understanding the order of changes across multiple files."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "since": {
                    "type": "string",
                    "description": "Time filter: show changes since this time ago. Examples: '1h', '30m', '1d'. Default: show all."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of timeline entries. Default: 20.",
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
        let since = params.get("since").and_then(|v| v.as_str())
            .and_then(|s| parse_duration(s));
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        let timeline = self.history.get_timeline(since);

        if timeline.is_empty() {
            return ToolResult::ok("No changes found in timeline.");
        }

        let mut output = format!("Timeline ({} entries):\n\n", timeline.len().min(limit));

        for (ts, path, _ver, desc) in timeline.iter().take(limit) {
            let deleted = if !path.exists() { " [DELETED]" } else { "" };
            output.push_str(&format!(
                "{} {} {}{}\n",
                ts.format("%H:%M:%S"),
                path.display(),
                desc,
                deleted
            ));
        }

        if timeline.len() > limit {
            output.push_str(&format!("\n... {} more entries. Use limit={} to see more.", timeline.len() - limit, limit + 20));
        }

        ToolResult::ok(output)
    }
}

// ─── P2: file_history_tag ───

pub struct FileHistoryTagTool {
    history: Arc<FileHistory>,
}

impl FileHistoryTagTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryTagTool {
    fn name(&self) -> &str {
        "file_history_tag"
    }

    fn description(&self) -> &str {
        "Add a named tag to the current version of a file, or list existing tags. Parameters: 'path' (required), 'tag' (optional, tag name to add. If omitted, lists existing tags). Tags can be used with file_history_diff (from/to) and file_restore for easy reference. Use before risky operations."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file."
                },
                "tag": {
                    "type": "string",
                    "description": "Tag name to add to current version. If omitted, lists existing tags."
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

        if let Some(tag) = params.get("tag").and_then(|v| v.as_str()) {
            if self.history.add_tag(&full_path, tag) {
                ToolResult::ok(format!("Tagged current version of {} as [{}]", full_path.display(), tag))
            } else {
                ToolResult::error(format!("No history for: {}", full_path.display()))
            }
        } else {
            let tags = self.history.list_tags(&full_path);
            if tags.is_empty() {
                return ToolResult::ok(format!("No tags for: {}", full_path.display()));
            }
            let mut output = format!("Tags for {}:\n", full_path.display());
            for (ver, tag) in &tags {
                output.push_str(&format!("  v{}: [{}]\n", ver, tag));
            }
            ToolResult::ok(output)
        }
    }
}

// ─── Annotate ───

pub struct FileHistoryAnnotateTool {
    history: Arc<FileHistory>,
}

impl FileHistoryAnnotateTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryAnnotateTool {
    fn name(&self) -> &str {
        "file_history_annotate"
    }

    fn description(&self) -> &str {
        "Add a user annotation/comment to a specific version of a file. Parameters: 'path' (required), 'version' (version specifier: v3, current, last2, or tag name), 'message' (required, annotation text). Annotations help document why changes were made. Use file_history first to see available versions."
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
                    "type": "string",
                    "description": "Version to annotate: v3, current, last2, or tag name."
                },
                "message": {
                    "type": "string",
                    "description": "Annotation text explaining the change."
                }
            },
            "required": ["path", "version", "message"]
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

        let version_spec = match params.get("version").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => return ToolResult::error("Error: version is required"),
        };
        let message = match params.get("message").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return ToolResult::error("Error: message is required"),
        };

        let version = self.history.resolve_version(&full_path, version_spec);
        let version = match version {
            Some(v) => v,
            None => return ToolResult::error(format!(
                "Cannot resolve version '{}' for {}. Use file_history to see available versions.",
                version_spec, full_path.display()
            )),
        };

        if self.history.annotate_snapshot(&full_path, version, message) {
            ToolResult::ok(format!(
                "Annotated v{} of {}: {}", version, full_path.display(), message
            ))
        } else {
            ToolResult::error(format!("No history for: {}", full_path.display()))
        }
    }
}

// ─── Batch operations ───

pub struct FileHistoryBatchTool {
    history: Arc<FileHistory>,
}

impl FileHistoryBatchTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryBatchTool {
    fn name(&self) -> &str {
        "file_history_batch"
    }

    fn description(&self) -> &str {
        "Perform batch operations on multiple files matching a glob pattern. Parameters: 'pattern' (required, glob like '*.rs' or 'src/**/*.py'), 'action' (optional: 'list'=summary per file, 'read'=show current version content, 'diff'=show change stats; default 'list'), 'version' (optional, version to read/diff; default 'current'). Use file_history first to see which files have history."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g., '*.rs', 'src/**/*.py')."
                },
                "action": {
                    "type": "string",
                    "description": "Action to perform: 'list' (summary), 'read' (content), 'diff' (change stats). Default: list.",
                    "enum": ["list", "read", "diff"]
                },
                "version": {
                    "type": "integer",
                    "description": "Version number to read or diff (1 = oldest). Used with action=read or action=diff. Default: current version.",
                    "minimum": 1
                }
            },
            "required": ["pattern"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let pattern_str = match params.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Error: pattern is required"),
        };
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("list");
        let version = params.get("version").and_then(|v| v.as_u64()).map(|v| v as usize);

        let glob_pattern = match glob::Pattern::new(pattern_str) {
            Ok(p) => p,
            Err(_) => return ToolResult::error(format!("Invalid glob pattern: {}", pattern_str)),
        };

        let all_files = self.history.list_all_files();
        let matched: Vec<_> = all_files
            .into_iter()
            .filter(|p| glob_pattern.matches(&p.to_string_lossy()))
            .collect();

        if matched.is_empty() {
            return ToolResult::ok(format!(
                "No files with history match pattern '{}'.", pattern_str
            ));
        }

        let mut output = format!(
            "Batch results for pattern '{}' ({} files):\n\n",
            pattern_str, matched.len()
        );

        let max_lines = 100; // Per-file line limit for action=read

        for path in &matched {
            let snapshots = self.history.get_snapshots(path);
            let count = snapshots.len();
            if count == 0 {
                continue;
            }

            output.push_str(&format!("=== {} ({} versions) ===\n", path.display(), count));

            match action {
                "list" => {
                    // Show last 2 versions summary
                    for snap in snapshots.iter().rev().take(2).rev() {
                        let desc = if snap.description.is_empty() {
                            format!("{} bytes", snap.content.len())
                        } else {
                            format!("{} ({} bytes)", snap.description, snap.content.len())
                        };
                        output.push_str(&format!("  {}\n", desc));
                    }
                }
                "read" => {
                    let ver = version.unwrap_or(count);
                    if ver == 0 || ver > count {
                        output.push_str(&format!("  [invalid version {}]\n", ver));
                        continue;
                    }
                    let snap = &snapshots[ver - 1];
                    let lines: Vec<&str> = snap.content.lines().collect();
                    let display_lines: Vec<_> = lines.iter().take(max_lines).collect();
                    for (i, line) in display_lines.iter().enumerate() {
                        output.push_str(&format!("  {:>4} {}\n", i + 1, line));
                    }
                    if lines.len() > max_lines {
                        output.push_str(&format!("  ... ({} more lines, omitted)\n", lines.len() - max_lines));
                    }
                }
                "diff" => {
                    let ver = version.unwrap_or(count);
                    if ver <= 1 || count < 2 {
                        output.push_str("  [no diff available, need at least 2 versions]\n");
                        continue;
                    }
                    if let Some(diff) = self.history.diff(path, 1, ver) {
                        let mut added = 0;
                        let mut removed = 0;
                        for hunk in &diff.hunks {
                            for line in &hunk.lines {
                                if line.starts_with("+ ") { added += 1; }
                                if line.starts_with("- ") { removed += 1; }
                            }
                        }
                        output.push_str(&format!("  v1 -> v{}: +{} -{}\n", ver, added, removed));
                    } else {
                        output.push_str("  [diff failed]\n");
                    }
                }
                _ => {
                    output.push_str(&format!("  [unknown action: {}]\n", action));
                }
            }

            output.push('\n');
        }

        output.push_str("Use file_history --path <file> for full version list of a single file.");
        ToolResult::ok(output)
    }
}

// ─── P3: unified file_history_checkout ───

pub struct FileHistoryCheckoutTool {
    history: Arc<FileHistory>,
}

impl FileHistoryCheckoutTool {
    pub fn new(history: Arc<FileHistory>) -> Self {
        Self { history }
    }
}

impl Tool for FileHistoryCheckoutTool {
    fn name(&self) -> &str {
        "file_history_checkout"
    }

    fn description(&self) -> &str {
        "Checkout a specific version of a file (unified restore/rewind). Parameters: 'path' (required), 'version' (version specifier: v3, current, last2, or tag name). Restores the file to the specified version and records the checkout as a new version (so redo is possible). Use file_history first to see available versions."
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
                    "type": "string",
                    "description": "Version to checkout: v3, current, last2, or tag name. Default: previous version."
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
        let total = self.history.count(&full_path);
        if total == 0 {
            return ToolResult::error(format!("No history for: {}", full_path.display()));
        }

        let version_spec = params.get("version").and_then(|v| v.as_str()).unwrap_or("last1");
        let target_ver = self.history.resolve_version(&full_path, version_spec);

        let target_ver = match target_ver {
            Some(v) => v,
            None => return ToolResult::error(format!(
                "Cannot resolve version '{}' for {}. Use file_history to see available versions.",
                version_spec, full_path.display()
            )),
        };

        if target_ver == total {
            return ToolResult::ok(format!("Already at v{} (current) for: {}", target_ver, full_path.display()));
        }

        match self.history.checkout(&full_path, target_ver) {
            Ok(Some(content)) => {
                let preview = if content.len() > 200 {
                    format!("{}...", &content[..200])
                } else {
                    content.clone()
                };
                ToolResult::ok(format!(
                    "Checked out v{} of {}\nContent preview:\n{}",
                    target_ver,
                    full_path.display(),
                    preview
                ))
            }
            Ok(None) => ToolResult::error(format!(
                "Cannot checkout v{} for: {}. Not enough history.",
                target_ver, full_path.display()
            )),
            Err(e) => ToolResult::error(format!("Error checking out file: {}", e)),
        }
    }
}

// ─── Helper: parse time duration strings ───

fn parse_duration(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let now = chrono::Utc::now();
    let s = s.trim();

    // Try "Nd" for N days
    if let Some(num) = s.strip_suffix('d') {
        if let Ok(n) = num.parse::<i64>() {
            return Some(now - chrono::Duration::days(n));
        }
    }

    // Try "Nh" for N hours
    if let Some(num) = s.strip_suffix('h') {
        if let Ok(n) = num.parse::<i64>() {
            return Some(now - chrono::Duration::hours(n));
        }
    }

    // Try "Nm" for N minutes
    if let Some(num) = s.strip_suffix('m') {
        if let Ok(n) = num.parse::<i64>() {
            return Some(now - chrono::Duration::minutes(n));
        }
    }

    None
}
