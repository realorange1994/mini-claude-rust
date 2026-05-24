//! CLAUDE.md utilities ported from upstream.
//!
//! Handles memory file detection, HTML comment stripping, etc.

use regex::Regex;
use std::lazy::SyncLazy;
use std::path::Path;

/// Recommended max character count for a memory file.
pub const MAX_MEMORY_CHARACTER_COUNT: usize = 40000;

/// Memory file info holds information about a memory file.
#[derive(Debug, Clone)]
pub struct MemoryFileInfo {
    pub path: String,
    pub file_type: String, // "Project", "User", "Local", "Managed", "AutoMem", "TeamMem"
    pub content: String,
}

static COMMENT_SPAN_REGEX: SyncLazy<Regex> = SyncLazy::new(|| {
    Regex::new(r"<!--[\s\S]*?-->").unwrap()
});

/// Strip block-level HTML comments from markdown content.
/// Inline comments within paragraphs are preserved (CommonMark paragraph semantics).
/// Unclosed comments are left in place.
/// Content inside fenced code blocks (``` ... ```) is fully preserved.
pub fn strip_html_comments(content: &str) -> (String, bool) {
    if !content.contains("<!--") {
        return (content.to_string(), false);
    }

    let mut result_lines = Vec::new();
    let mut stripped = false;
    let mut in_code_block = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        // Toggle fenced code block tracking
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            result_lines.push(line.to_string());
            continue;
        }

        if in_code_block {
            result_lines.push(line.to_string());
        } else if is_html_block_line(line) {
            let residue = strip_comment_spans(line);
            if !residue.is_empty() {
                result_lines.push(residue);
            }
            stripped = true;
        } else {
            result_lines.push(line.to_string());
        }
    }

    (result_lines.join("\n"), stripped)
}

/// Returns true if the line starts with <!-- and contains -->
fn is_html_block_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("<!--") && trimmed.contains("-->")
}

/// Remove well-formed HTML comment spans from a line.
fn strip_comment_spans(line: &str) -> String {
    COMMENT_SPAN_REGEX.replace_all(line, "").to_string()
}

/// Check if a file path is a memory file (CLAUDE.md, CLAUDE.local.md,
/// or .md files in .claude/rules/ directories).
pub fn is_memory_file_path(file_path: &str) -> bool {
    let path = Path::new(file_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // CLAUDE.md or CLAUDE.local.md anywhere (case-sensitive)
    if name == "CLAUDE.md" || name == "CLAUDE.local.md" {
        return true;
    }

    // .md files in .claude/rules/ directories
    if name.ends_with(".md") && file_path.contains("/.claude/rules/") {
        return true;
    }

    false
}

/// Returns files whose content exceeds MAX_MEMORY_CHARACTER_COUNT.
pub fn get_large_memory_files(files: &[MemoryFileInfo]) -> Vec<MemoryFileInfo> {
    files
        .iter()
        .filter(|f| f.content.len() > MAX_MEMORY_CHARACTER_COUNT)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_comments_basic() {
        let input = "hello\n<!-- comment -->\nworld";
        let (result, stripped) = strip_html_comments(input);
        assert!(stripped);
        assert_eq!(result, "hello\nworld");
    }

    #[test]
    fn test_strip_html_comments_no_comments() {
        let input = "hello\nworld";
        let (result, stripped) = strip_html_comments(input);
        assert!(!stripped);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_html_comments_preserves_code_blocks() {
        let input = "hello\n```\n<!-- not a comment -->\n```\nworld";
        let (result, stripped) = strip_html_comments(input);
        assert!(!stripped);
        assert_eq!(result, input);
    }

    #[test]
    fn test_is_memory_file_path() {
        assert!(is_memory_file_path("CLAUDE.md"));
        assert!(is_memory_file_path("CLAUDE.local.md"));
        assert!(is_memory_file_path("/path/.claude/rules/custom.md"));
        assert!(!is_memory_file_path("README.md"));
        assert!(!is_memory_file_path("rules.md"));
    }

    #[test]
    fn test_get_large_memory_files() {
        let large_content = "x".repeat(MAX_MEMORY_CHARACTER_COUNT + 1);
        let files = vec![
            MemoryFileInfo {
                path: "small.md".to_string(),
                file_type: "Project".to_string(),
                content: "small".to_string(),
            },
            MemoryFileInfo {
                path: "large.md".to_string(),
                file_type: "Project".to_string(),
                content: large_content,
            },
        ];
        let result = get_large_memory_files(&files);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "large.md");
    }
}
