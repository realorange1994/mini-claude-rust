//! @ Context References -- expand @file, @folder, @diff, @staged, @git:N, @url in user messages.
//!
//! When a user types `@file:main.go` or `@diff`, the reference is expanded
//! into a context block attached to the message. Token budget guardrails
//! prevent context overflow: 25% soft warning, 50% hard block.

use regex::Regex;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Maximum number of lines to read from a file.
const MAX_LINE_LIMIT: usize = 1000;

/// Default maximum depth for folder listings.
const MAX_FOLDER_DEPTH: usize = 3;

/// A parsed @ reference in a user message.
#[derive(Debug, Clone)]
pub struct ContextReference {
    pub raw: String,
    pub kind: String,
    pub target: String,
    pub line_start: Option<usize>, // 1-indexed, inclusive
    pub line_end: Option<usize>,   // 1-indexed, inclusive
    pub start: usize,
    pub end: usize,
}

/// Result of expanding @ references in a message.
#[derive(Debug, Clone)]
pub struct ContextReferenceResult {
    pub message: String,
    pub original_message: String,
    pub references: Vec<ContextReference>,
    pub warnings: Vec<String>,
    pub injected_tokens: usize,
    pub expanded: bool,
    pub blocked: bool,
}

/// Sensitive directories that should never be exposed.
const SENSITIVE_DIRS: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".azure", ".config/gh", ".config/git",
];

/// File content cache to avoid re-reading the same file.
static FILE_CACHE: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

fn get_file_cache() -> std::sync::MutexGuard<'static, Option<std::collections::HashMap<String, String>>> {
    FILE_CACHE.lock().unwrap()
}

/// Parse @ references from a user message.
pub fn parse_context_references(message: &str) -> Vec<ContextReference> {
    if message.is_empty() {
        return Vec::new();
    }

    // Match @diff, @staged, @file:path, @file:path:10-50, @folder:path, @git:N, @url:url
    // Also supports quoted values: @file:"path with spaces.py"
    let re = Regex::new(r#"@(?:(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>"[^"]+"|\S+))"#)
        .expect("invalid regex");

    let mut refs = Vec::new();
    for cap in re.captures_iter(message) {
        let full = cap.get(0).unwrap();
        let raw = full.as_str().to_string();

        // Email/social exclusion: @ must not be preceded by a word character
        if full.start() > 0 {
            let prev = message.as_bytes()[full.start() - 1];
            if is_word_char(prev) {
                continue;
            }
        }

        if let Some(simple) = cap.name("simple") {
            refs.push(ContextReference {
                raw,
                kind: simple.as_str().to_string(),
                target: String::new(),
                line_start: None,
                line_end: None,
                start: full.start(),
                end: full.end(),
            });
        } else if let (Some(kind), Some(value)) = (cap.name("kind"), cap.name("value")) {
            let value_str = strip_trailing_punctuation(value.as_str());
            let value_str = strip_quotes(&value_str);
            let kind_str = kind.as_str();

            // Parse line range for @file:path:10-50
            let (target, line_start, line_end) = if kind_str == "file" {
                parse_file_target(&value_str)
            } else {
                (value_str.to_string(), None, None)
            };

            refs.push(ContextReference {
                raw,
                kind: kind_str.to_string(),
                target,
                line_start,
                line_end,
                start: full.start(),
                end: full.end(),
            });
        }
    }

    refs
}

/// Check if a byte is a word character (for email/social exclusion).
fn is_word_char(b: u8) -> bool {
    (b >= b'a' && b <= b'z') || (b >= b'A' && b <= b'Z') ||
    (b >= b'0' && b <= b'9') || b == b'_' || b == b'/'
}

/// Expand @ references in a user message.
pub fn preprocess_context_references(
    message: &str,
    cwd: &Path,
    context_length: usize,
) -> ContextReferenceResult {
    let refs = parse_context_references(message);
    if refs.is_empty() {
        return ContextReferenceResult {
            message: message.to_string(),
            original_message: message.to_string(),
            references: Vec::new(),
            warnings: Vec::new(),
            injected_tokens: 0,
            expanded: false,
            blocked: false,
        };
    }

    let mut warnings = Vec::new();
    let mut blocks = Vec::new();
    let mut injected_tokens = 0usize;

    for ref_item in &refs {
        let (block, warning) = expand_reference(ref_item, cwd);
        if !warning.is_empty() {
            // Inject the error as a context block so the model understands what happened
            // instead of just seeing a stripped message + cryptic warning
            let error_block = format!("## {} (error)\n{}", ref_item.raw, warning);
            blocks.push(error_block);
            warnings.push(warning);
        }
        if !block.is_empty() {
            blocks.push(block.clone());
            injected_tokens += block.len() / 4;
        }
    }

    // Token budget guardrails
    let hard_limit = context_length / 2;
    let soft_limit = context_length / 4;
    let hard_limit = if hard_limit < 1 { 1 } else { hard_limit };
    let soft_limit = if soft_limit < 1 { 1 } else { soft_limit };

    if injected_tokens > hard_limit {
        let mut block_warnings = warnings.clone();
        block_warnings.push(format!(
            "@ context injection refused: {} tokens exceeds the 50% hard limit ({}).",
            injected_tokens, hard_limit
        ));
        return ContextReferenceResult {
            message: message.to_string(),
            original_message: message.to_string(),
            references: refs,
            warnings: block_warnings,
            injected_tokens,
            expanded: false,
            blocked: true,
        };
    }

    if injected_tokens > soft_limit {
        warnings.push(format!(
            "@ context injection warning: {} tokens exceeds the 25% soft limit ({}).",
            injected_tokens, soft_limit
        ));
    }

    let has_warnings = !warnings.is_empty();
    let has_blocks = !blocks.is_empty();

    // Remove @ reference tokens from message, then append context blocks
    let stripped = remove_reference_tokens(message, &refs);
    let mut final_msg = stripped;

    if has_warnings {
        final_msg.push_str("\n\n--- Context Warnings ---\n");
        for w in &warnings {
            final_msg.push_str(&format!("- {}\n", w));
        }
    }

    if has_blocks {
        final_msg.push_str("\n\n--- Attached Context ---\n");
        final_msg.push_str("IMPORTANT: The following context has been attached by the user. Use this context directly instead of calling tools to read the same files or run the same commands.\n\n");
        final_msg.push_str(&blocks.join("\n\n"));
    }

    let final_msg = final_msg.trim().to_string();

    ContextReferenceResult {
        message: final_msg,
        original_message: message.to_string(),
        references: refs,
        warnings,
        injected_tokens,
        expanded: has_blocks || has_warnings,
        blocked: false,
    }
}

fn expand_reference(ref_item: &ContextReference, cwd: &Path) -> (String, String) {
    match ref_item.kind.as_str() {
        "file" => expand_file_reference(ref_item, cwd),
        "folder" => expand_folder_reference(ref_item, cwd),
        "diff" => expand_git_reference(ref_item, cwd, &["diff"], "@diff"),
        "staged" => expand_git_reference(ref_item, cwd, &["diff", "--staged"], "@staged"),
        "git" => {
            let mut count = 1;
            if !ref_item.target.is_empty() {
                if let Ok(n) = ref_item.target.parse::<usize>() {
                    count = n.clamp(1, 10);
                }
            }
            expand_git_reference(
                ref_item,
                cwd,
                &["log", &format!("-{}", count), "-p"],
                &format!("@git:{}", count),
            )
        }
        "url" => expand_url_reference(ref_item),
        _ => (String::new(), format!("{}: unsupported reference type", ref_item.raw)),
    }
}

fn expand_file_reference(ref_item: &ContextReference, cwd: &Path) -> (String, String) {
    let path = resolve_path(cwd, &ref_item.target);

    if let Some(err) = ensure_path_allowed(&path, cwd) {
        return (String::new(), err);
    }

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                " (permission denied)"
            } else {
                ""
            };
            return (String::new(), format!("File not found: {}{}", ref_item.target, hint));
        }
    };

    if metadata.is_dir() {
        return (
            String::new(),
            format!("{}: path is a directory, use @folder: instead", ref_item.raw),
        );
    }

    // Check file size -- reject files over 10MB
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    if metadata.len() > MAX_FILE_SIZE {
        return (String::new(), format!(
            "{}: file is too large ({} bytes, max {} MB)",
            ref_item.raw, metadata.len(), MAX_FILE_SIZE / (1024 * 1024)
        ));
    }

    // Check cache first
    let cache_key = path.to_string_lossy().to_string();
    let cached_content = {
        let cache = get_file_cache();
        if let Some(map) = cache.as_ref() {
            map.get(&cache_key).cloned()
        } else {
            None
        }
    };

    let text = if let Some(content) = cached_content {
        content
    } else {
        // Stream-read file with line limit
        let (lines, truncated) = match read_file_lines(&path, MAX_LINE_LIMIT) {
            Ok(result) => result,
            Err(e) => {
                let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    " (permission denied)"
                } else {
                    ""
                };
                return (String::new(), format!("Cannot read file: {}{}", ref_item.target, hint));
            }
        };

        if is_binary_lines(&lines) {
            return (String::new(), format!("{}: binary files are not supported", ref_item.raw));
        }

        let mut text = lines.join("\n");
        if truncated {
            text.push_str(&format!("\n... (truncated at {} lines)", MAX_LINE_LIMIT));
        }

        // Cache the content
        {
            let mut cache = get_file_cache();
            if cache.is_none() {
                *cache = Some(std::collections::HashMap::new());
            }
            if let Some(map) = cache.as_mut() {
                map.insert(cache_key.clone(), text.clone());
            }
        }

        text
    };

    let lang = code_fence_language(&path);

    // Apply line range if specified
    let (display_text, lines_hint) = if let Some(line_start) = ref_item.line_start {
        let all_lines: Vec<&str> = text.lines().collect();
        let total_lines = all_lines.len();

        // If requested start is beyond file length, return a clear message
        if line_start > total_lines {
            return (String::new(), format!(
                "{}: file has {} lines, but line {} was requested",
                ref_item.raw, total_lines, line_start
            ));
        }

        let start_idx = if line_start > 0 { line_start - 1 } else { 0 };
        let end_idx = ref_item.line_end
            .map(|end| end.min(all_lines.len()))
            .unwrap_or(all_lines.len());
        let end_idx = end_idx.max(start_idx);

        // Cap to MAX_LINE_LIMIT
        let capped_end = if end_idx - start_idx > MAX_LINE_LIMIT {
            start_idx + MAX_LINE_LIMIT
        } else {
            end_idx
        };

        let selected: Vec<String> = all_lines[start_idx..capped_end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {}", start_idx + i + 1, line))
            .collect();

        let mut dt = selected.join("\n");
        if capped_end < end_idx {
            dt.push_str(&format!("\n... (truncated at {} lines)", MAX_LINE_LIMIT));
        }

        let actual_end = ref_item.line_end.unwrap_or(capped_end);
        let mut hint = format!(" (lines {}-{}, file has {} lines)", line_start, actual_end.min(total_lines), total_lines);
        if let Some(requested_end) = ref_item.line_end {
            if requested_end > total_lines {
                hint = format!(" (lines {}-{}, file has {} lines -- requested end {} adjusted)", line_start, total_lines, total_lines, requested_end);
            }
        }

        (dt, hint)
    } else {
        (text.to_string(), String::new())
    };

    let tokens = display_text.len() / 4;

    (
        format!("## @file:{}{} ({} tokens)\n```{}\n{}\n```", ref_item.target, lines_hint, tokens, lang, display_text),
        String::new(),
    )
}

fn expand_folder_reference(ref_item: &ContextReference, cwd: &Path) -> (String, String) {
    let path = resolve_path(cwd, &ref_item.target);

    if let Some(err) = ensure_path_allowed(&path, cwd) {
        return (String::new(), err);
    }

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                " (permission denied)"
            } else {
                ""
            };
            return (String::new(), format!("Folder not found: {}{}", ref_item.target, hint));
        }
    };

    if !metadata.is_dir() {
        return (
            String::new(),
            format!("{}: path is not a directory, use @file: instead", ref_item.raw),
        );
    }

    // Check if folder is empty
    let is_empty = fs::read_dir(&path)
        .map(|entries| entries.count() == 0)
        .unwrap_or(false);

    if is_empty {
        return (
            format!("## @folder:{} (0 tokens)\n(empty directory -- no files or subdirectories)", ref_item.target),
            String::new(),
        );
    }

    let listing = build_folder_listing(&path, cwd, 200, MAX_FOLDER_DEPTH);
    let tokens = listing.len() / 4;

    (
        format!("## @folder:{} ({} tokens)\n{}", ref_item.target, tokens, listing),
        String::new(),
    )
}

fn expand_git_reference(
    ref_item: &ContextReference,
    cwd: &Path,
    args: &[&str],
    label: &str,
) -> (String, String) {
    // First check if we're in a git repository
    let git_check = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output();

    match git_check {
        Ok(output) if !output.status.success() => {
            return (String::new(), format!(
                "{}: not a git repository -- git references require a git repo", label
            ));
        }
        Err(_) => {
            return (String::new(), format!(
                "{}: git is not installed or not available in PATH", label
            ));
        }
        _ => {}
    }

    let output = match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(o) => o,
        Err(e) => return (String::new(), format!("{}: git command failed -- {}", label, e)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return (String::new(), format!("{}: {}", label, if stderr.is_empty() { "unknown git error" } else { &stderr }));
    }

    let content = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let (content, is_empty) = if content.is_empty() {
        // Provide context-specific empty messages so the model understands what "empty" means
        let empty_msg = match label {
            "@diff" => "(working tree is clean -- no unstaged changes)",
            "@staged" => "(nothing staged -- no staged changes to commit)",
            s if s.starts_with("@git:") => "(no commits found in this repository)",
            _ => "(no output)",
        };
        (empty_msg.to_string(), true)
    } else {
        (content, false)
    };

    let tokens = content.len() / 4;

    let block = if is_empty {
        format!("## {} (0 tokens)\n{}", label, content)
    } else {
        format!("## {} ({} tokens)\n```diff\n{}\n```", label, tokens, content)
    };

    (block, String::new())
}

fn expand_url_reference(ref_item: &ContextReference) -> (String, String) {
    let url = &ref_item.target;

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (String::new(), format!(
            "{}: invalid URL -- must start with http:// or https://", ref_item.raw
        ));
    }

    // Use curl for URL fetching (available on all platforms)
    let output = match Command::new("curl")
        .args([
            "-sL",           // silent, follow redirects
            "-w", "%{http_code}",  // write HTTP status code to stdout
            "--max-time", "30",
            "--connect-timeout", "10",
            "-H", "User-Agent: Mozilla/5.0 (compatible; miniClaudeCode/1.0)",
            url,
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => return (String::new(), format!(
            "{}: curl not available -- {}", ref_item.raw, e
        )),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let hint = if stderr.contains("timed out") {
            " (request timed out after 30s)"
        } else if stderr.contains("Connection refused") || stderr.contains("Could not resolve host") {
            " (connection refused or DNS resolution failed)"
        } else {
            ""
        };
        return (String::new(), format!(
            "{}: fetch failed -- {}{}", ref_item.raw, if stderr.is_empty() { "unknown error" } else { &stderr }, hint
        ));
    }

    let raw_output = String::from_utf8_lossy(&output.stdout);
    // curl -w "%{http_code}" appends the status code at the end
    // Extract it and the body separately
    let (body, http_code) = if let Some(pos) = raw_output.rfind('\n') {
        let last_line = raw_output[pos+1..].trim();
        if last_line.chars().all(|c| c.is_ascii_digit()) {
            (raw_output[..pos].to_string(), last_line.to_string())
        } else {
            (raw_output.to_string(), String::new())
        }
    } else {
        (raw_output.to_string(), String::new())
    };

    // Check HTTP status code
    if !http_code.is_empty() && http_code != "200" {
        let status_hint = match http_code.as_str() {
            "401" | "403" => " (authentication required or access denied)",
            "404" => " (page not found)",
            "429" => " (rate limited -- too many requests)",
            s if s.starts_with('5') => " (server error)",
            _ => "",
        };
        return (String::new(), format!(
            "{}: HTTP {}{}", ref_item.raw, http_code, status_hint
        ));
    }

    if body.trim().is_empty() {
        return (String::new(), format!(
            "{}: page returned empty content", ref_item.raw
        ));
    }

    // Extract content from HTML
    let text = extract_html_content(&body);
    if text.trim().is_empty() {
        return (String::new(), format!(
            "{}: page has no extractable text content (may be JS-rendered or binary)", ref_item.raw
        ));
    }

    let tokens = text.len() / 4;

    // Extract title
    let title = extract_html_title(&body);
    let title_hint = if title.is_empty() { String::new() } else { format!("Title: {}\n", title) };

    (
        format!("## @url:{} ({} tokens)\n{}{}", url, tokens, title_hint, text),
        String::new(),
    )
}

fn resolve_path(cwd: &Path, target: &str) -> PathBuf {
    let path = PathBuf::from(target);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// Parse @file target with optional line range: "path:10-50" -> (path, Some(10), Some(50))
fn parse_file_target(value: &str) -> (String, Option<usize>, Option<usize>) {
    let line_re = Regex::new(r"^(.+):(\d+)(?:-(\d+))?$").expect("invalid line range regex");

    if let Some(cap) = line_re.captures(value) {
        let path = cap.get(1).unwrap().as_str().to_string();
        let start: usize = cap.get(2).unwrap().as_str().parse().unwrap_or(1);
        let end = cap.get(3).and_then(|m| m.as_str().parse::<usize>().ok());

        let looks_like_file = path.contains('.')
            || path.contains('/')
            || path.contains('\\')
            || !path.contains(':');

        if looks_like_file && start > 0 {
            return (path, Some(start), end);
        }
    }

    (value.to_string(), None, None)
}

/// Read a file line-by-line up to max_lines, returning (lines, truncated).
fn read_file_lines(path: &Path, max_lines: usize) -> std::io::Result<(Vec<String>, bool)> {
    let f = fs::File::open(path)?;
    let reader = BufReader::new(f);
    let mut lines = Vec::new();
    let mut truncated = false;

    for line in reader.lines() {
        let line = line?;
        lines.push(line);
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }
    }

    Ok((lines, truncated))
}

/// Check if lines appear to be binary.
fn is_binary_lines(lines: &[String]) -> bool {
    let check_len = lines.len().min(16);
    for line in &lines[..check_len] {
        if line.as_bytes().contains(&0) {
            return true;
        }
    }
    false
}

/// Extract meaningful content from HTML, removing scripts/styles.
fn extract_html_content(html: &str) -> String {
    // Remove <script> and <style> blocks entirely
    let script_re = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let style_re = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let mut text = script_re.replace_all(html, "").to_string();
    text = style_re.replace_all(&text, "").to_string();

    // Try to extract <article>, <main>, or <body> content
    let article_re = Regex::new(r"(?is)<article[^>]*>(.*?)</article>").unwrap();
    if let Some(cap) = article_re.captures(&text) {
        text = cap.get(1).unwrap().as_str().to_string();
    } else {
        let main_re = Regex::new(r"(?is)<main[^>]*>(.*?)</main>").unwrap();
        if let Some(cap) = main_re.captures(&text) {
            text = cap.get(1).unwrap().as_str().to_string();
        } else {
            let body_re = Regex::new(r"(?is)<body[^>]*>(.*?)</body>").unwrap();
            if let Some(cap) = body_re.captures(&text) {
                text = cap.get(1).unwrap().as_str().to_string();
            }
        }
    }

    // Remove remaining HTML tags
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    text = tag_re.replace_all(&text, "").to_string();

    // Collapse excessive whitespace
    let ws_re = Regex::new(r"\n{3,}").unwrap();
    text = ws_re.replace_all(&text, "\n\n").to_string();
    let space_re = Regex::new(r"  +").unwrap();
    text = space_re.replace_all(&text, " ").to_string();

    // Decode common HTML entities
    text = text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    text.trim().to_string()
}

/// Extract the <title> from HTML.
fn extract_html_title(html: &str) -> String {
    let title_re = Regex::new(r"(?i)<title[^>]*>(.*?)</title>").unwrap();
    if let Some(cap) = title_re.captures(html) {
        return cap.get(1).unwrap().as_str().trim().to_string();
    }
    String::new()
}

/// Check if a path is allowed: not in sensitive dirs and not outside CWD.
fn ensure_path_allowed(path: &Path, cwd: &Path) -> Option<String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let home = PathBuf::from(home);

    // Check sensitive directories
    for dir in SENSITIVE_DIRS {
        let sensitive_path = home.join(dir);
        if path.starts_with(&sensitive_path) {
            return Some("path is in a sensitive directory and cannot be attached".to_string());
        }
    }

    // Path traversal protection: path must be within cwd
    // Use dunce to get consistent absolute paths on Windows (avoids UNC \\?\ prefix
    // that std::fs::canonicalize produces, which breaks string prefix comparison).
    let abs_cwd = match dunce::canonicalize(cwd) {
        Ok(p) => p,
        Err(_) => {
            // Fallback: if cwd can't be resolved, skip the check
            return None;
        }
    };

    // Try to canonicalize the path (works for existing files)
    // For non-existent files, resolve manually
    let abs_path = if path.exists() {
        match dunce::canonicalize(path) {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        }
    } else {
        // File doesn't exist yet -- resolve relative to cwd manually
        let mut resolved = abs_cwd.clone();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    resolved.pop();
                }
                std::path::Component::Normal(c) => {
                    resolved.push(c);
                }
                std::path::Component::CurDir => {}
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    resolved = path.to_path_buf();
                    break;
                }
            }
        }
        resolved
    };

    // Normalize separators for comparison
    let abs_path_str = abs_path.to_string_lossy().replace('\\', "/");
    let abs_cwd_str = abs_cwd.to_string_lossy().replace('\\', "/");

    if !abs_path_str.starts_with(&abs_cwd_str) {
        return Some("path traversal outside working directory is not allowed".to_string());
    }

    None
}

fn remove_reference_tokens(message: &str, refs: &[ContextReference]) -> String {
    if refs.is_empty() {
        return message.to_string();
    }

    let mut parts = Vec::new();
    let mut cursor = 0;

    for ref_item in refs {
        if ref_item.start > cursor {
            parts.push(&message[cursor..ref_item.start]);
        }
        cursor = ref_item.end;
    }
    if cursor < message.len() {
        parts.push(&message[cursor..]);
    }

    let result = parts.join("");
    let re = Regex::new(r"\s{2,}").unwrap();
    re.replace_all(&result, " ").trim().to_string()
}

fn strip_trailing_punctuation(value: &str) -> String {
    let mut s = value.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == '!' || c == '?');

    loop {
        if s.ends_with(')') && s.matches(')').count() > s.matches('(').count() {
            s = &s[..s.len() - 1];
        } else if s.ends_with(']') && s.matches(']').count() > s.matches('[').count() {
            s = &s[..s.len() - 1];
        } else if s.ends_with('}') && s.matches('}').count() > s.matches('{').count() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }

    s.to_string()
}

/// Strip surrounding quotes from a value.
fn strip_quotes(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"') ||
           (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'') {
            return value[1..value.len()-1].to_string();
        }
    }
    value.to_string()
}

fn is_binary_content(content: &[u8]) -> bool {
    let check_len = content.len().min(4096);
    content[..check_len].iter().any(|&b| b == 0)
}

fn code_fence_language(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "go" => "go",
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "json" => "json",
        "md" => "markdown",
        "sh" => "bash",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "c" | "h" => "c",
        "cpp" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "sql" => "sql",
        "html" => "html",
        "css" => "css",
        _ => "",
    }
}

fn build_folder_listing(path: &Path, cwd: &Path, limit: usize, max_depth: usize) -> String {
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let mut lines = vec![format!("{}/", rel.display())];
    let mut count = 0;

    if let Ok(entries) = walkdir(path, &mut count, limit, max_depth, 0) {
        lines.extend(entries);
    }

    if count >= limit {
        lines.push("- ...".to_string());
    }

    lines.join("\n")
}

/// Recursive directory listing with depth control.
fn walkdir(path: &Path, count: &mut usize, limit: usize, max_depth: usize, depth: usize) -> Result<Vec<String>, std::io::Error> {
    if depth >= max_depth || *count >= limit {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let indent = "  ".repeat(depth + 1);

    let mut dir_entries: Vec<_> = fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .collect();
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        if *count >= limit {
            break;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            entries.push(format!("{}- {}/", indent, name_str));
            *count += 1;
            if let Ok(sub) = walkdir(&entry.path(), count, limit, max_depth, depth + 1) {
                entries.extend(sub);
            }
        } else {
            entries.push(format!("{}- {}", indent, name_str));
            *count += 1;
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file_reference() {
        let refs = parse_context_references("look at @file:main.go for details");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "file");
        assert_eq!(refs[0].target, "main.go");
        assert!(refs[0].line_start.is_none());
    }

    #[test]
    fn test_parse_file_reference_with_range() {
        let refs = parse_context_references("look at @file:main.go:10-50 for details");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "file");
        assert_eq!(refs[0].target, "main.go");
        assert_eq!(refs[0].line_start, Some(10));
        assert_eq!(refs[0].line_end, Some(50));
    }

    #[test]
    fn test_parse_file_reference_single_line() {
        let refs = parse_context_references("see @file:lib.rs:42");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "lib.rs");
        assert_eq!(refs[0].line_start, Some(42));
        assert_eq!(refs[0].line_end, None);
    }

    #[test]
    fn test_parse_folder_reference() {
        let refs = parse_context_references("check @folder:src/");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "folder");
        assert_eq!(refs[0].target, "src/");
    }

    #[test]
    fn test_parse_diff_reference() {
        let refs = parse_context_references("see @diff for changes");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "diff");
    }

    #[test]
    fn test_parse_staged_reference() {
        let refs = parse_context_references("review @staged changes");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "staged");
    }

    #[test]
    fn test_parse_git_reference() {
        let refs = parse_context_references("show @git:3 recent commits");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "git");
        assert_eq!(refs[0].target, "3");
    }

    #[test]
    fn test_parse_url_reference() {
        let refs = parse_context_references("fetch @url:https://example.com");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].kind, "url");
        assert_eq!(refs[0].target, "https://example.com");
    }

    #[test]
    fn test_parse_multiple_references() {
        let refs = parse_context_references("check @file:a.go and @file:b.go");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].target, "a.go");
        assert_eq!(refs[1].target, "b.go");
    }

    #[test]
    fn test_parse_no_references() {
        let refs = parse_context_references("just a normal message");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_email_exclusion() {
        // user@domain.com should NOT match
        let refs = parse_context_references("send to user@domain.com please");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_quoted_path() {
        let refs = parse_context_references("check @file:\"path with spaces.py\"");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].target, "path with spaces.py");
    }

    #[test]
    fn test_strip_trailing_punctuation() {
        assert_eq!(strip_trailing_punctuation("main.go,"), "main.go");
        assert_eq!(strip_trailing_punctuation("path/to/file."), "path/to/file");
        assert_eq!(strip_trailing_punctuation("test)"), "test");
        assert_eq!(strip_trailing_punctuation("func(x)"), "func(x)");
    }

    #[test]
    fn test_parse_file_target() {
        let (path, start, end) = parse_file_target("main.go:10-50");
        assert_eq!(path, "main.go");
        assert_eq!(start, Some(10));
        assert_eq!(end, Some(50));

        let (path, start, end) = parse_file_target("lib.rs:42");
        assert_eq!(path, "lib.rs");
        assert_eq!(start, Some(42));
        assert_eq!(end, None);

        let (path, start, end) = parse_file_target("main.go");
        assert_eq!(path, "main.go");
        assert!(start.is_none());
    }

    #[test]
    fn test_extract_html_content() {
        assert_eq!(extract_html_content("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(extract_html_content("a &lt; b &amp; c"), "a < b & c");
        // Script content should be removed
        assert_eq!(extract_html_content("<script>alert(1)</script>Hello"), "Hello");
    }

    #[test]
    fn test_extract_html_title() {
        assert_eq!(extract_html_title("<html><title>My Page</title></html>"), "My Page");
        assert_eq!(extract_html_title("<html><body>No title</body></html>"), "");
    }

    #[test]
    fn test_code_fence_language() {
        assert_eq!(code_fence_language(Path::new("main.go")), "go");
        assert_eq!(code_fence_language(Path::new("lib.rs")), "rust");
        assert_eq!(code_fence_language(Path::new("app.py")), "python");
        assert_eq!(code_fence_language(Path::new("unknown.xyz")), "");
    }

    #[test]
    fn test_preprocess_no_refs() {
        let result = preprocess_context_references("hello world", Path::new("."), 100000);
        assert!(!result.expanded);
        assert!(!result.blocked);
        assert_eq!(result.message, "hello world");
    }

    #[test]
    fn test_preprocess_blocked_by_hard_limit() {
        let dir = std::env::temp_dir();
        let file_path = dir.join("ctx_ref_test.txt");
        fs::write(&file_path, "x".repeat(1000)).unwrap();

        let msg = format!("@file:{}", file_path.display());
        let result = preprocess_context_references(&msg, &dir, 10);
        assert!(result.blocked);

        let _ = fs::remove_file(&file_path);
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"hello world\""), "hello world");
        assert_eq!(strip_quotes("'hello world'"), "hello world");
        assert_eq!(strip_quotes("no_quotes"), "no_quotes");
    }
}
