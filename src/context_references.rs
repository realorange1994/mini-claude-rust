//! @ Context References — expand @file, @folder, @diff, @staged, @git:N, @url in user messages.
//!
//! When a user types `@file:main.go` or `@diff`, the reference is expanded
//! into a context block attached to the message. Token budget guardrails
//! prevent context overflow: 25% soft warning, 50% hard block.

use regex::Regex;
use std::fs;
use std::io::Read as IoRead;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// Parse @ references from a user message.
pub fn parse_context_references(message: &str) -> Vec<ContextReference> {
    if message.is_empty() {
        return Vec::new();
    }

    // Match @diff, @staged, @file:path, @file:path:10-50, @folder:path, @git:N, @url:url
    let re = Regex::new(r"@(?:(?P<simple>diff|staged)\b|(?P<kind>file|folder|git|url):(?P<value>\S+))")
        .expect("invalid regex");

    let mut refs = Vec::new();
    for cap in re.captures_iter(message) {
        let full = cap.get(0).unwrap();
        let raw = full.as_str().to_string();

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
        "diff" => expand_git_reference(ref_item, cwd, &["diff"], "git diff"),
        "staged" => expand_git_reference(ref_item, cwd, &["diff", "--staged"], "git diff --staged"),
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
                &format!("git log -{} -p", count),
            )
        }
        "url" => expand_url_reference(ref_item),
        _ => (String::new(), format!("{}: unsupported reference type", ref_item.raw)),
    }
}

fn expand_file_reference(ref_item: &ContextReference, cwd: &Path) -> (String, String) {
    let path = resolve_path(cwd, &ref_item.target);

    if let Some(err) = ensure_path_allowed(&path) {
        return (String::new(), format!("{}: {}", ref_item.raw, err));
    }

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return (String::new(), format!("{}: file not found", ref_item.raw)),
    };

    if metadata.is_dir() {
        return (
            String::new(),
            format!("{}: path is a directory, use @folder: instead", ref_item.raw),
        );
    }

    let content = match fs::read(&path) {
        Ok(c) => c,
        Err(e) => return (String::new(), format!("{}: {}", ref_item.raw, e)),
    };

    if is_binary_content(&content) {
        return (String::new(), format!("{}: binary files are not supported", ref_item.raw));
    }

    let text = String::from_utf8_lossy(&content);
    let lang = code_fence_language(&path);

    // Apply line range if specified
    let (display_text, range_hint) = if let Some(line_start) = ref_item.line_start {
        let lines: Vec<&str> = text.lines().collect();
        let start_idx = if line_start > 0 { line_start - 1 } else { 0 };
        let end_idx = ref_item.line_end
            .map(|end| end.min(lines.len()))
            .unwrap_or(lines.len());
        let end_idx = end_idx.max(start_idx);

        let selected: Vec<String> = lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {}", start_idx + i + 1, line))
            .collect();

        (selected.join("\n"), format!(":{}-{}", line_start, ref_item.line_end.unwrap_or(line_start)))
    } else {
        (text.to_string(), String::new())
    };

    let tokens = display_text.len() / 4;

    (
        format!("\u{1f4c4} @file:\"{}\"{} ({} tokens)\n```{}\n{}\n```", ref_item.target, range_hint, tokens, lang, display_text),
        String::new(),
    )
}

fn expand_folder_reference(ref_item: &ContextReference, cwd: &Path) -> (String, String) {
    let path = resolve_path(cwd, &ref_item.target);

    if let Some(err) = ensure_path_allowed(&path) {
        return (String::new(), format!("{}: {}", ref_item.raw, err));
    }

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return (String::new(), format!("{}: folder not found", ref_item.raw)),
    };

    if !metadata.is_dir() {
        return (
            String::new(),
            format!("{}: path is not a directory", ref_item.raw),
        );
    }

    let listing = build_folder_listing(&path, cwd, 200);
    let tokens = listing.len() / 4;

    (
        format!("\u{1f4c1} @folder:\"{}\" ({} tokens)\n{}", ref_item.target, tokens, listing),
        String::new(),
    )
}

fn expand_git_reference(
    ref_item: &ContextReference,
    cwd: &Path,
    args: &[&str],
    label: &str,
) -> (String, String) {
    let output = match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(o) => o,
        Err(e) => return (String::new(), format!("{}: {}", ref_item.raw, e)),
    };

    let content = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return (String::new(), format!("{}: {}", ref_item.raw, stderr));
    };

    let content = if content.is_empty() {
        "(no output)".to_string()
    } else {
        content
    };

    let tokens = content.len() / 4;

    (
        format!("\u{1f9fe} {} ({} tokens)\n```diff\n{}\n```", label, tokens, content),
        String::new(),
    )
}

fn expand_url_reference(ref_item: &ContextReference) -> (String, String) {
    let url = &ref_item.target;

    // Validate URL scheme
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (String::new(), format!("{}: only http/https URLs are supported", ref_item.raw));
    }

    // Use curl for URL fetching (available on all platforms)
    let output = match Command::new("curl")
        .args([
            "-sL",           // silent, follow redirects
            "--max-time", "30",  // 30s timeout
            "--connect-timeout", "10", // 10s connect timeout
            "-H", "User-Agent: miniClaudeCode/1.0",
            url,
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => return (String::new(), format!("{}: curl not available: {}", ref_item.raw, e)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return (String::new(), format!("{}: fetch failed: {}", ref_item.raw, if stderr.is_empty() { "unknown error" } else { &stderr }));
    }

    let content = String::from_utf8_lossy(&output.stdout);
    if content.is_empty() {
        return (String::new(), format!("{}: no content returned", ref_item.raw));
    }

    // Strip HTML tags for a rough text extraction
    let text = strip_html_tags(&content);
    let tokens = text.len() / 4;

    (
        format!("\u{1f310} @url:\"{}\" ({} tokens)\n{}", url, tokens, text),
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
    // Match pattern: filepath:digits-digits or filepath:digits-
    let line_re = Regex::new(r"^(.+):(\d+)(?:-(\d+))?$").expect("invalid line range regex");

    if let Some(cap) = line_re.captures(value) {
        let path = cap.get(1).unwrap().as_str().to_string();
        let start: usize = cap.get(2).unwrap().as_str().parse().unwrap_or(1);
        let end = cap.get(3).and_then(|m| m.as_str().parse::<usize>().ok());

        // Only treat as line range if the "path" part looks like a real file
        // (has an extension or is a known filename). Otherwise it's a Windows path like C:\...
        let path_part = &path;
        let looks_like_file = path_part.contains('.')
            || path_part.contains('/')
            || path_part.contains('\\')
            || !path_part.contains(':'); // no drive letter

        if looks_like_file && start > 0 {
            return (path, Some(start), end);
        }
    }

    (value.to_string(), None, None)
}

/// Strip HTML tags for rough text extraction from fetched URLs.
fn strip_html_tags(html: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    let text = re.replace_all(html, "");
    // Collapse excessive whitespace
    let ws_re = Regex::new(r"\n{3,}").unwrap();
    let text = ws_re.replace_all(&text, "\n\n");
    // Decode common HTML entities
    let text = text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    text.trim().to_string()
}

fn ensure_path_allowed(path: &Path) -> Option<String> {
    // Use $HOME on Unix, $USERPROFILE on Windows
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let home = PathBuf::from(home);

    for dir in SENSITIVE_DIRS {
        let sensitive_path = home.join(dir);
        if path.starts_with(&sensitive_path) {
            return Some("path is in a sensitive directory and cannot be attached".to_string());
        }
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
    // Clean up extra whitespace
    let re = Regex::new(r"\s{2,}").unwrap();
    re.replace_all(&result, " ").trim().to_string()
}

fn strip_trailing_punctuation(value: &str) -> String {
    let mut s = value.trim_end_matches(|c: char| c == ',' || c == '.' || c == ';' || c == '!' || c == '?');

    // Remove unbalanced closing brackets
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

fn build_folder_listing(path: &Path, cwd: &Path, limit: usize) -> String {
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let mut lines = vec![format!("{}/", rel.display())];
    let mut count = 0;

    if let Ok(entries) = walkdir(path, &mut count, limit, 0) {
        lines.extend(entries);
    }

    if count >= limit {
        lines.push("- ...".to_string());
    }

    lines.join("\n")
}

/// Simple recursive directory listing (avoids external walkdir crate).
fn walkdir(path: &Path, count: &mut usize, limit: usize, depth: usize) -> Result<Vec<String>, std::io::Error> {
    let mut entries = Vec::new();
    let indent = "  ".repeat(depth);

    if *count >= limit {
        return Ok(entries);
    }

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

        // Skip hidden files/dirs
        if name_str.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            entries.push(format!("{}- {}/", indent, name_str));
            *count += 1;
            if let Ok(sub) = walkdir(&entry.path(), count, limit, depth + 1) {
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
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(strip_html_tags("a &lt; b &amp; c"), "a < b & c");
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
        // Create a temp file with lots of content
        let dir = std::env::temp_dir();
        let file_path = dir.join("ctx_ref_test.txt");
        fs::write(&file_path, "x".repeat(1000)).unwrap();

        let msg = format!("@file:{}", file_path.display());
        // Very small context length to trigger hard limit
        let result = preprocess_context_references(&msg, &dir, 10);
        assert!(result.blocked);

        // Cleanup
        let _ = fs::remove_file(&file_path);
    }
}
