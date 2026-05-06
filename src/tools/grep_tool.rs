//! GrepTool - Search file contents using regex

use crate::tools::{Tool, ToolResult, expand_path, is_ignored_dir};
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
        "Search file contents using regex in a codebase. \
        ALWAYS use grep for content search. NEVER invoke grep or rg via exec. \
        Uses ripgrep (rg) if available, otherwise falls back to built-in regex. \
        Supports glob and language type filters, context lines, and output modes. \
        For advanced ripgrep features (multiline, PCRE2, etc.) use the exec tool with caution."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for. For literal text, use fixed_strings=true instead of escaping special regex characters."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in. Defaults to current directory. To avoid scanning too many files, use max_depth to limit directory traversal."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob to filter files (e.g. '*.py'). Only files matching this pattern are searched."
                },
                "type": {
                    "type": "string",
                    "description": "Language type filter. Common values: go, py, js, ts, rust, java, sh, yaml, json, md, html, css."
                },
                "-i": {
                    "type": "boolean",
                    "description": "Case insensitive search (rg -i). Default: false."
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Alias for -i. Case insensitive search (default: false)."
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Alias for -i. Case insensitive search (default: false)."
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as a literal string, not regex (default: false)."
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output mode (default: files_with_matches): 'content' shows matching lines, 'files_with_matches' shows file paths, 'count' shows per-file match counts.",
                    "enum": ["content", "files_with_matches", "count"]
                },
                "-B": {
                    "type": "integer",
                    "description": "Number of lines to show before each match (rg -B). Requires output_mode: content, ignored otherwise."
                },
                "-A": {
                    "type": "integer",
                    "description": "Number of lines to show after each match (rg -A). Requires output_mode: content, ignored otherwise."
                },
                "-C": {
                    "type": "integer",
                    "description": "Alias for context. Number of lines to show before and after each match."
                },
                "context": {
                    "type": "integer",
                    "description": "Number of lines to show before and after each match (rg -C). Requires output_mode: content, ignored otherwise."
                },
                "context_before": {
                    "type": "integer",
                    "description": "Alias for -B. Lines of context before each match (default: 0)."
                },
                "context_after": {
                    "type": "integer",
                    "description": "Alias for -A. Lines of context after each match (default: 0)."
                },
                "-n": {
                    "type": "boolean",
                    "description": "Show line numbers in output (rg -n). Requires output_mode: content, ignored otherwise. Defaults to true."
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline mode where . matches newlines and patterns can span lines (rg -U --multiline-dotall). Default: false."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to search. Limits how many levels of subdirectories to traverse. Useful for avoiding scanning too many files (default: unlimited)."
                },
                "max_filesize": {
                    "type": "string",
                    "description": "Maximum file size to search (e.g. '1M', '500K', '100B'). Files larger than this are skipped. Only applies when ripgrep is available."
                },
                "head_limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 250)."
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

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
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

        // Support -i, ignore_case, and case_insensitive
        let mut case_insensitive = params.get("-i").and_then(|v| v.as_bool()).unwrap_or(false);
        if !case_insensitive {
            case_insensitive = params.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
        }
        if !case_insensitive {
            case_insensitive = params.get("case_insensitive").and_then(|v| v.as_bool()).unwrap_or(false);
        }
        let fixed_strings = params.get("fixed_strings").and_then(|v| v.as_bool()).unwrap_or(false);
        let multiline = params.get("multiline").and_then(|v| v.as_bool()).unwrap_or(false);
        let show_line_numbers = params.get("-n").and_then(|v| v.as_bool()).unwrap_or(true);

        let output_mode = params
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("files_with_matches");

        let count_matches = params
            .get("count_matches")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let head_limit_raw = params
            .get("head_limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(MAX_GREP_MATCHES as i64);
        // Upstream: head_limit=0 means unlimited (escape hatch)
        // For ripgrep: -m 0 means unlimited; for native: use usize::MAX
        let head_limit = if head_limit_raw < 0 {
            MAX_GREP_MATCHES
        } else if head_limit_raw == 0 {
            usize::MAX
        } else {
            head_limit_raw as usize
        };

        let offset = params
            .get("offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        // Parse context params (official: -C/context takes precedence over -B/-A)
        // Support both official names (-B, -A, -C) and legacy aliases (context_before, context_after)
        let mut ctx_before = params.get("-B").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        if ctx_before == 0 {
            ctx_before = params.get("context_before").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        }
        let mut ctx_after = params.get("-A").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        if ctx_after == 0 {
            ctx_after = params.get("context_after").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        }
        let mut ctx_combined = params.get("-C").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        if ctx_combined == 0 {
            ctx_combined = params.get("context").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        }
        if ctx_combined > 0 {
            if ctx_before == 0 { ctx_before = ctx_combined; }
            if ctx_after == 0 { ctx_after = ctx_combined; }
        }

        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let max_filesize = params
            .get("max_filesize")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let search_path = expand_path(path);
        if !search_path.exists() {
            return ToolResult::error(format!("Error: path not found: {}", search_path.display()));
        }

        // Try ripgrep first, fall back to Go regex (matching Go version)
        if is_rg_available() {
            return rg_search(
                pattern, &search_path, include, &type_filter,
                case_insensitive, fixed_strings, output_mode,
                show_line_numbers, multiline,
                ctx_before, ctx_after, head_limit, offset, max_depth, max_filesize,
            );
        }

        go_search(
            pattern, &search_path, include, &type_filter,
            case_insensitive, fixed_strings, output_mode,
            head_limit, offset, ctx_combined as usize, count_matches, max_depth,
        )
    }


}

/// Split glob on commas and whitespace, respecting brace groups.
fn split_glob_patterns(glob: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_brace = false;
    for c in glob.chars() {
        match c {
            '{' => { in_brace = true; current.push(c); }
            '}' => { in_brace = false; current.push(c); }
            ',' | ' ' if !in_brace => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn is_rg_available() -> bool {
    Command::new("rg").arg("--version").output().is_ok()
}


fn rg_search(
    pattern: &str,
    path: &Path,
    include: &str,
    type_filter: &str,
    case_insensitive: bool,
    fixed_strings: bool,
    output_mode: &str,
    show_line_numbers: bool,
    multiline: bool,
    ctx_before: i32,
    ctx_after: i32,
    head_limit: usize,
    offset: usize,
    max_depth: usize,
    max_filesize: &str,
) -> ToolResult {
    let mut args = vec![
        "--hidden".to_string(),
        "--max-columns".to_string(),
        "500".to_string(),
    ];

    // Exclude VCS directories (matching official Claude Code behavior)
    let vcs_dirs = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
    for dir in &vcs_dirs {
        args.push("--glob".to_string());
        args.push(format!("!{}", dir));
    }

    match output_mode {
        "files_with_matches" => args.push("--files-with-matches".to_string()),
        "count" => args.push("--count".to_string()),
        _ => {}
    }
    if case_insensitive { args.push("-i".to_string()); }
    if fixed_strings { args.push("-F".to_string()); }
    if multiline {
        args.push("-U".to_string());
        args.push("--multiline-dotall".to_string());
    }
    if ctx_before > 0 { args.push("-B".to_string()); args.push(ctx_before.to_string()); }
    if ctx_after > 0 { args.push("-A".to_string()); args.push(ctx_after.to_string()); }

    // Show line numbers only in content mode (matching official behavior)
    if show_line_numbers && output_mode == "content" {
        args.push("-n".to_string());
    }

    if max_depth > 0 { args.push("--max-depth".to_string()); args.push(max_depth.to_string()); }
    if !max_filesize.is_empty() { args.push("--max-filesize".to_string()); args.push(max_filesize.to_string()); }

    // Don't pass -m to rg — apply offset+head_limit in post-processing only.
    // Passing -m breaks pagination (offset would slice fewer results).

    if !type_filter.is_empty() {
        let type_map = get_type_map();
        if let Some(exts) = type_map.get(&type_filter.to_lowercase()) {
            for e in exts {
                let glob = if e.starts_with('*') { e.clone() } else { format!("*{}", e) };
                args.push("--type-add".to_string());
                args.push(format!("mytype:{}", glob));
            }
            args.push("--type".to_string());
            args.push("mytype".to_string());
        }
    }
    if !include.is_empty() {
        // Split glob on commas/spaces, matching upstream behavior
        for g in split_glob_patterns(include) {
            args.push("--glob".to_string());
            args.push(g.trim().to_string());
        }
    }

    // If pattern starts with dash, use -e flag to prevent rg from interpreting it as an option
    if pattern.starts_with('-') {
        args.push("-e".to_string());
        args.push(pattern.to_string());
    } else {
        args.push(pattern.to_string());
    }
    args.push(path.to_string_lossy().to_string());

    let output = match Command::new("rg").args(&args).output() {
        Ok(o) => o,
        Err(e) => return ToolResult::error(format!("Error running rg: {}", e)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);
    let is_error = !output.status.success() && exit_code != 1;
    let combined = format!("{}{}", stdout, stderr);
    let text = combined.trim().to_string();
    if text.is_empty() {
        // rg exits with code 1 when no matches found -- not a real error
        if is_error {
            return ToolResult::error(format!("Error running rg: {}", stderr.trim()));
        }
        return ToolResult::ok("No matches found.".to_string());
    }

    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if offset > 0 && offset < lines.len() {
        lines = lines[offset..].to_vec();
    }
    if head_limit > 0 && lines.len() > head_limit {
        lines = lines[..head_limit].to_vec();
        lines.push(format!("(showing first {} matches, truncated)", head_limit));
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
    max_depth: usize,
) -> ToolResult {
    let mut search_pattern = pattern.to_string();
    if fixed_strings {
        search_pattern = regex::escape(&search_pattern);
    }
    if case_insensitive {
        search_pattern = format!("(?i){}", search_pattern);
    }
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
        let base_depth = path.to_string_lossy().trim_end_matches(|c| c == '/' || c == '\\')
            .split(std::path::MAIN_SEPARATOR).count();
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_entry(|e| {
                if max_depth > 0 {
                    let cur_depth = e.path().to_string_lossy()
                        .trim_end_matches(|c| c == '/' || c == '\\')
                        .split(std::path::MAIN_SEPARATOR).count() - base_depth;
                    if cur_depth >= max_depth && e.path() != path {
                        return false;
                    }
                }
                !is_ignored_dir(e.file_name())
            })
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
        "count" => go_search_count(&re, &files, head_limit, offset, files_searched),
        _ => go_search_content(&re, &files, head_limit, offset, ctx_lines, count_matches, files_searched),
    }
}

fn path_ext_lower(p: &Path) -> Option<String> {
    p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
}

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_GREP_LINE_LEN { line.to_string() }
    else {
        let mut end = MAX_GREP_LINE_LEN;
        while end > 0 && !line.is_char_boundary(end) { end -= 1; }
        format!("{}...", &line[..end])
    }
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
            if !re.is_match(line) { continue; }
            total_match_count += 1;
            if skipped < offset { skipped += 1; continue; }
            let rel_path = fp.strip_prefix(&cwd).unwrap_or(fp).to_string_lossy().to_string();

            if count_matches {
                let count = re.find_iter(line).count();
                if ctx_lines > 0 {
                    let start = i.saturating_sub(ctx_lines);
                    let end = (i + ctx_lines).min(lines.len() - 1);
                    for j in start..=end {
                        let prefix = if j == i { ">>> " } else { "    " };
                        matches.push(format!("{}:{}: {}{}", rel_path, j + 1, prefix, truncate_line(lines[j])));
                    }
                    matches.push(format!("  [{} match(es) on this line]", count));
                } else {
                    matches.push(format!("{}:{}:[{}] {}", rel_path, i + 1, count, truncate_line(line)));
                }
            } else if ctx_lines > 0 {
                let start = i.saturating_sub(ctx_lines);
                let end = (i + ctx_lines).min(lines.len() - 1);
                for j in start..=end {
                    let prefix = if j == i { ">>> " } else { "    " };
                    matches.push(format!("{}:{}: {}{}", rel_path, j + 1, prefix, truncate_line(lines[j])));
                }
            } else {
                matches.push(format!("{}:{}:{}", rel_path, i + 1, truncate_line(line)));
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

fn go_search_count(re: &Regex, files: &[PathBuf], head_limit: usize, offset: usize, files_searched: usize) -> ToolResult {
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

    // Apply offset and head_limit (matching upstream behavior)
    let start = offset.min(lines.len());
    if start > 0 {
        lines = lines[start..].to_vec();
    }
    if head_limit > 0 && lines.len() > head_limit {
        lines = lines[..head_limit].to_vec();
        lines.push(format!("(showing first {} matches, truncated)", head_limit));
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
