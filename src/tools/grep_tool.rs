//! GrepTool - Search file contents using regex

use crate::tools::{Tool, ToolResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const MAX_GREP_MATCHES: usize = 250;
const MAX_GREP_LINE_LEN: usize = 500;

pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for GrepTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents using regex. Uses ripgrep (rg) if available, otherwise falls back to Go regexp."
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
                    "description": "File or directory to search (default: current directory)."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob to filter files (e.g. '*.py')."
                },
                "type": {
                    "type": "string",
                    "description": "Language type filter (e.g. 'go', 'py', 'js', 'ts', 'rust', 'java')."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case insensitive search (default: false)."
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as literal string, not regex (default: false)."
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output mode: 'content' (default), 'files_with_matches', or 'count'.",
                    "enum": ["content", "files_with_matches", "count"]
                },
                "count_matches": {
                    "type": "boolean",
                    "description": "Count per-line match occurrences (not just matching lines). Only with content mode."
                },
                "context_before": {
                    "type": "integer",
                    "description": "Lines of context before each match (default: 0)."
                },
                "context_after": {
                    "type": "integer",
                    "description": "Lines of context after each match (default: 0)."
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context before and after each match (default: 0, max: 3)."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 250)."
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)."
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

        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let include = params
            .get("glob")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let type_filter = params
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let case_insensitive = params
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let fixed_strings = params
            .get("fixed_strings")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let output_mode = params
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content");

        let count_matches = params
            .get("count_matches")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let head_limit = params
            .get("head_limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(MAX_GREP_MATCHES as i64)
            .max(1) as usize;

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let ctx_before = params
            .get("context_before")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let ctx_after = params
            .get("context_after")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let ctx_combined = params
            .get("context")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        // If ctx_combined > 0 and before/after not set, apply to both
        let ctx_before = if ctx_combined > 0 && ctx_before == 0 { ctx_combined } else { ctx_before };
        let ctx_after = if ctx_combined > 0 && ctx_after == 0 { ctx_combined } else { ctx_after };

        let search_path = expand_path(path);
        if !search_path.exists() {
            return ToolResult::error(format!("Error: path not found: {}", search_path.display()));
        }

        // Try ripgrep first, fall back to Go regex (matching Go version)
        if is_rg_available() {
            return rg_search(
                pattern, &search_path, include, &type_filter,
                case_insensitive, fixed_strings, output_mode,
                ctx_before, ctx_after, head_limit, offset,
            );
        }

        go_search(
            pattern, &search_path, include, &type_filter,
            case_insensitive, fixed_strings, output_mode,
            head_limit, offset, ctx_combined, count_matches,
        )
    }
}

fn is_ignored_dir(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        ".git" | "node_modules" | "__pycache__" | ".venv" | "venv"
            | "dist" | "build" | ".DS_Store" | ".tox"
            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache"
            | ".coverage" | "htmlcov" | "target"
    )
}

fn is_rg_available() -> bool {
    Command::new("rg").arg("--version").output().is_ok()
}

fn expand_path(p: &str) -> PathBuf {
    let p = if p.starts_with('~') {
        if let Ok(home) = std::env::var("HOME") {
            p.replacen('~', &home, 1)
        } else {
            p.to_string()
        }
    } else {
        p.to_string()
    };
    let path = Path::new(&p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| Path::new(".").to_path_buf())
            .join(path)
    }
}

fn rg_search(
    pattern: &str,
    path: &Path,
    include: &str,
    type_filter: &str,
    case_insensitive: bool,
    fixed_strings: bool,
    output_mode: &str,
    ctx_before: usize,
    ctx_after: usize,
    head_limit: usize,
    offset: usize,
) -> ToolResult {
    let mut args = vec!["--no-heading".to_string(), "--line-number".to_string()];

    match output_mode {
        "files_with_matches" => args.push("--files-with-matches".to_string()),
        "count" => args.push("--count".to_string()),
        _ => {}
    }
    if case_insensitive { args.push("-i".to_string()); }
    if fixed_strings { args.push("-F".to_string()); }
    if ctx_before > 0 { args.push("-B".to_string()); args.push(ctx_before.to_string()); }
    if ctx_after > 0 { args.push("-A".to_string()); args.push(ctx_after.to_string()); }

    args.push("-m".to_string());
    args.push(head_limit.to_string());
    args.push(pattern.to_string());
    args.push(path.to_string_lossy().to_string());

    if !include.is_empty() {
        args.push("--glob".to_string());
        args.push(include.to_string());
    }
    if !type_filter.is_empty() {
        let type_map = get_type_map();
        if let Some(exts) = type_map.get(&type_filter.to_lowercase()) {
            for e in exts {
                args.push("--type-add".to_string());
                args.push(format!("mytype:{}", e));
            }
            args.push("--type".to_string());
            args.push("mytype".to_string());
        }
    }

    let output = match Command::new("rg").args(&args).output() {
        Ok(o) => o,
        Err(e) => return ToolResult::error(format!("Error running rg: {}", e)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let text = if !stdout.is_empty() { stdout } else if !stderr.is_empty() { stderr.to_string() } else { String::new() };
    if text.is_empty() {
        if !output.status.success() {
            return ToolResult::error(format!("Error running rg: {}", String::from_utf8_lossy(&output.stderr)));
        }
        return ToolResult::ok("No matches found.".to_string());
    }

    let mut lines: Vec<&str> = text.lines().collect();
    if offset > 0 && offset < lines.len() {
        lines = lines[offset..].to_vec();
    }
    if lines.len() > head_limit {
        lines = lines[..head_limit].to_vec();
        lines.push("(showing first N matches, truncated)");
    }

    ToolResult::ok(lines.join("\n"))
}

fn go_search(
    pattern: &str,
    path: &Path,
    include: &str,
    type_filter: &str,
    case_insensitive: bool,
    fixed_strings: bool,
    output_mode: &str,
    head_limit: usize,
    offset: usize,
    ctx_lines: usize,
    count_matches: bool,
) -> ToolResult {
    let search_pattern = if fixed_strings {
        regex::escape(pattern)
    } else if case_insensitive {
        format!("(?i){}", pattern)
    } else {
        pattern.to_string()
    };
    let re = match Regex::new(&search_pattern) {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("Invalid regex: {}", e)),
    };

    let type_map = get_type_map();
    let allowed_exts: Vec<String> = if !type_filter.is_empty() {
        type_map.get(&type_filter.to_lowercase()).cloned().unwrap_or_default()
    } else { Vec::new() };

    let mut files = Vec::new();
    let info = match fs::metadata(path) {
        Ok(i) => i,
        Err(e) => return ToolResult::error(format!("Error: {}", e)),
    };
    if info.is_file() {
        files.push(path.to_path_buf());
    } else {
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_entry(|e| !is_ignored_dir(e.file_name()))
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() { continue; }
            let fp = entry.path();
            if !include.is_empty() {
                let name = fp.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                if !glob_match(include, &name) { continue; }
            }
            if !allowed_exts.is_empty() {
                let ext_with_dot = format!(".{}", path_ext_lower(fp).unwrap_or_default());
                if !allowed_exts.contains(&ext_with_dot) { continue; }
            }
            if let Some(ext) = path_ext_lower(fp) {
                if matches!(ext.as_str(), ".exe" | ".dll" | ".so" | ".bin") { continue; }
            }
            files.push(fp.to_path_buf());
        }
    }

    let files_searched = files.len();
    match output_mode {
        "files_with_matches" => go_search_files_only(&re, &files, head_limit, offset, files_searched),
        "count" => go_search_count(&re, &files, files_searched),
        _ => go_search_content(&re, &files, head_limit, offset, ctx_lines, count_matches, files_searched),
    }
}

fn path_ext_lower(p: &Path) -> Option<String> {
    p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
}

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_GREP_LINE_LEN { line.to_string() }
    else { format!("{}...", &line[..MAX_GREP_LINE_LEN]) }
}

fn go_search_content(
    re: &Regex, files: &[PathBuf], head_limit: usize, offset: usize,
    ctx_lines: usize, count_matches: bool, files_searched: usize,
) -> ToolResult {
    let mut matches = Vec::new();
    let mut skipped = 0;
    let mut total_match_count = 0;
    let cwd = std::env::current_dir().ok().unwrap_or_else(|| PathBuf::from("."));

    for fp in files {
        let data = match fs::read_to_string(fp) { Ok(d) => d, Err(_) => continue };
        let lines: Vec<&str> = data.split('\n').collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !re.is_match(trimmed) { continue; }
            total_match_count += 1;
            if skipped < offset { skipped += 1; continue; }
            let rel_path = fp.strip_prefix(&cwd).unwrap_or(fp).to_string_lossy().to_string();

            if count_matches {
                let count = re.find_iter(trimmed).count();
                if ctx_lines > 0 {
                    let start = i.saturating_sub(ctx_lines);
                    let end = (i + ctx_lines).min(lines.len() - 1);
                    for j in start..=end {
                        let prefix = if j == i { ">>> " } else { "    " };
                        matches.push(format!("{}:{}: {}{}", rel_path, j + 1, prefix, truncate_line(lines[j].trim())));
                    }
                    matches.push(format!("  [{} match(es) on this line]", count));
                } else {
                    matches.push(format!("{}:{}:[{}] {}", rel_path, i + 1, count, truncate_line(trimmed)));
                }
            } else if ctx_lines > 0 {
                let start = i.saturating_sub(ctx_lines);
                let end = (i + ctx_lines).min(lines.len() - 1);
                for j in start..=end {
                    let prefix = if j == i { ">>> " } else { "    " };
                    matches.push(format!("{}:{}: {}{}", rel_path, j + 1, prefix, truncate_line(lines[j].trim())));
                }
            } else {
                matches.push(format!("{}:{}:{}", rel_path, i + 1, truncate_line(trimmed)));
            }
            if matches.len() >= head_limit {
                matches.push(format!("(showing first {} matches, truncated)", head_limit));
                return ToolResult::ok(matches.join("\n"));
            }
        }
    }

    if matches.is_empty() {
        if offset > 0 && skipped > 0 {
            return ToolResult::ok(format!("No matches after skipping first {} results. (Searched {} files, {} matches total)", offset, files_searched, total_match_count));
        }
        return ToolResult::ok(format!("No matches found. (Searched {} files)", files_searched));
    }
    let mut summary = format!("(Searched {} files, {} matches", files_searched, total_match_count);
    if matches.len() < total_match_count { summary.push_str(&format!(", showing first {}", matches.len())); }
    summary.push(')');
    ToolResult::ok(format!("{}\n{}", matches.join("\n"), summary))
}

fn go_search_files_only(re: &Regex, files: &[PathBuf], head_limit: usize, offset: usize, files_searched: usize) -> ToolResult {
    let mut found = Vec::new();
    let mut skipped = 0;
    let cwd = std::env::current_dir().ok().unwrap_or_else(|| PathBuf::from("."));
    for fp in files {
        if found.len() >= head_limit { break; }
        let data = match fs::read_to_string(fp) { Ok(d) => d, Err(_) => continue };
        if re.is_match(&data) {
            if skipped < offset { skipped += 1; continue; }
            found.push(fp.strip_prefix(&cwd).unwrap_or(fp).to_string_lossy().to_string());
        }
    }
    if found.is_empty() {
        return ToolResult::ok(format!("No matches found. (Searched {} files)", files_searched));
    }
    ToolResult::ok(format!("{}\n(Searched {} files, {} matches)", found.join("\n"), files_searched, found.len()))
}

fn go_search_count(re: &Regex, files: &[PathBuf], files_searched: usize) -> ToolResult {
    let mut lines = Vec::new();
    let mut total_matches = 0;
    let cwd = std::env::current_dir().ok().unwrap_or_else(|| PathBuf::from("."));
    for fp in files {
        let data = match fs::read_to_string(fp) { Ok(d) => d, Err(_) => continue };
        let count = data.lines().filter(|l| re.is_match(l)).count();
        if count > 0 {
            lines.push(format!("{}:{}", fp.strip_prefix(&cwd).unwrap_or(fp).to_string_lossy(), count));
            total_matches += count;
        }
    }
    if lines.is_empty() {
        return ToolResult::ok(format!("No matches found. (Searched {} files)", files_searched));
    }
    ToolResult::ok(format!("{}\n(Searched {} files, {} matching lines)", lines.join("\n"), files_searched, total_matches))
}

fn get_type_map() -> HashMap<String, Vec<String>> {
    let mut m = HashMap::new();
    m.insert("py".to_string(), vec![".py".to_string(), ".pyi".to_string()]);
    m.insert("python".to_string(), vec![".py".to_string(), ".pyi".to_string()]);
    m.insert("js".to_string(), vec![".js".to_string(), ".jsx".to_string(), ".mjs".to_string(), ".cjs".to_string()]);
    m.insert("ts".to_string(), vec![".ts".to_string(), ".tsx".to_string(), ".mts".to_string(), ".cts".to_string()]);
    m.insert("go".to_string(), vec![".go".to_string()]);
    m.insert("rust".to_string(), vec![".rs".to_string()]);
    m.insert("java".to_string(), vec![".java".to_string()]);
    m.insert("sh".to_string(), vec![".sh".to_string(), ".bash".to_string()]);
    m.insert("yaml".to_string(), vec![".yaml".to_string(), ".yml".to_string()]);
    m.insert("json".to_string(), vec![".json".to_string()]);
    m.insert("md".to_string(), vec![".md".to_string(), ".mdx".to_string()]);
    m.insert("html".to_string(), vec![".html".to_string(), ".htm".to_string()]);
    m.insert("css".to_string(), vec![".css".to_string(), ".scss".to_string(), ".sass".to_string()]);
    m
}

fn glob_match(pattern: &str, name: &str) -> bool {
    glob::Pattern::new(pattern).map(|p| p.matches(name)).unwrap_or(false)
}
