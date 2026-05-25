//! File utility functions for text formatting, diff generation, and path manipulation.
//! Ported from upstream utils_file.go (269 lines), consolidated from diff.go and file_utils.go.

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::Command;

// ============================================================================
// Structured Diff
// ============================================================================

/// Generate a unified diff between two strings.
/// Tries git diff first, falls back to simple line-by-line diff.
pub fn structured_diff(old_content: &str, new_content: &str, file_path: &str) -> String {
    // Try git diff
    if which_git() {
        if let Some(result) = git_diff(old_content, new_content, file_path) {
            return result;
        }
    }
    // Fallback
    simple_diff(old_content, new_content, file_path)
}

/// Check if git is available.
fn which_git() -> bool {
    #[cfg(target_os = "windows")]
    {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

/// Use git diff for proper unified diff output.
fn git_diff(old_content: &str, new_content: &str, file_path: &str) -> Option<String> {
    let tmp_dir = std::env::temp_dir();
    let old_file = tmp_dir.join("cc_diff_old");
    let new_file = tmp_dir.join("cc_diff_new");

    fs::write(&old_file, old_content).ok()?;
    fs::write(&new_file, new_content).ok()?;

    let output = Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--unified=3",
            old_file.to_str()?,
            new_file.to_str()?,
        ])
        .output()
        .ok()?;

    // Clean up temp files
    let _ = fs::remove_file(&old_file);
    let _ = fs::remove_file(&new_file);

    let result = String::from_utf8_lossy(&output.stdout);
    // Replace temp paths with a/b/ paths
    let old_path_str = old_file.to_string_lossy().to_string();
    let new_path_str = new_file.to_string_lossy().to_string();
    let result = result
        .replace(&old_path_str, &format!("a/{}", file_path))
        .replace(&new_path_str, &format!("b/{}", file_path));

    Some(result)
}

/// Produce a basic line-by-line diff when git is unavailable.
fn simple_diff(old_content: &str, new_content: &str, file_path: &str) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    let mut result = String::new();
    writeln!(result, "--- a/{}", file_path).unwrap();
    writeln!(result, "+++ b/{}", file_path).unwrap();

    let max_lines = old_lines.len().max(new_lines.len());

    for i in 0..max_lines {
        let old_line = old_lines.get(i);
        let new_line = new_lines.get(i);

        if old_line != new_line {
            if let Some(&line) = old_line {
                writeln!(result, "-{}", line).unwrap();
            }
            if let Some(&line) = new_line {
                writeln!(result, "+{}", line).unwrap();
            }
        }
    }

    result
}

// ============================================================================
// Text Formatting
// ============================================================================

/// Convert leading tabs to 2 spaces each.
/// Only leading tabs on each line are converted; tabs within the line are preserved.
pub fn convert_leading_tabs_to_spaces(content: &str) -> String {
    if !content.contains('\t') {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len() * 2);
    for (i, line) in content.lines().enumerate() {
        if i > 0 {
            result.push('\n');
        }

        // Count leading tabs
        let leading_tabs = line.chars().take_while(|&c| c == '\t').count();

        // Replace leading tabs with 2 spaces each
        for _ in 0..leading_tabs {
            result.push_str("  ");
        }
        result.push_str(&line[leading_tabs..]);
    }

    result
}

/// Options for AddLineNumbers.
pub struct AddLineNumbersOptions<'a> {
    pub content: &'a str,
    pub start_line: usize, // 1-indexed
}

/// Add line numbers to content, starting from StartLine.
/// Uses compact format: "N\tline" (tab-separated).
pub fn add_line_numbers(opts: AddLineNumbersOptions) -> String {
    if opts.content.is_empty() {
        return String::new();
    }

    opts.content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let num = opts.start_line + i;
            format!("{}\t{}", num, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

use once_cell::sync::Lazy;
use regex::Regex;

static STRIP_LINE_NUMBER_RE: Lazy<Regex> = Lazy::new(|| {
    // Matches optional whitespace, a number, then arrow (→) or tab separator.
    Regex::new(r"^\s*\d+[\u{2192}\t](.*)$").unwrap()
});

/// Remove the line number prefix from a line.
/// Supports formats: "N→line" or "N\tline" with optional leading whitespace.
pub fn strip_line_number_prefix(line: &str) -> &str {
    STRIP_LINE_NUMBER_RE
        .captures(line)
        .map(|caps| caps.get(1).map(|m| m.as_str()).unwrap_or(line))
        .unwrap_or(line)
}

// ============================================================================
// Path Utilities
// ============================================================================

/// Compare two paths for equality, handling platform differences.
/// On Windows, normalizes to forward slashes and lowercases for case-insensitive comparison.
/// On Unix, only normalizes separators.
pub fn paths_equal(path1: &str, path2: &str) -> bool {
    normalize_path_for_comparison(path1) == normalize_path_for_comparison(path2)
}

/// Normalize a path for comparison across platforms.
/// Resolves dot segments, removes redundant separators, converts backslashes to slashes.
/// On Windows, also lowercases the path.
pub fn normalize_path_for_comparison(file_path: &str) -> String {
    // Use PathBuf to normalize separators and resolve . and ..
    let path = Path::new(file_path);

    // Get canonical components manually (don't require file to exist)
    let mut normalized = file_path.replace('\\', "/");

    // Remove redundant slashes
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }

    // Remove trailing slash (unless root)
    if normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }

    // Lowercase on Windows
    if cfg!(windows) {
        normalized = normalized.to_lowercase();
    }

    normalized
}

/// Check if a path contains parent-directory references (..).
/// Returns true if the path contains ".." as a path segment (not just part of a filename).
pub fn contains_path_traversal(path: &str) -> bool {
    static TRAVERSAL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(^|[/\\])\.\.([/\\]|$)").unwrap());
    TRAVERSAL_RE.is_match(path)
}

/// Normalize a path for use as a configuration key.
/// Resolves dot segments, converts backslashes to forward slashes.
pub fn normalize_path_for_config_key(path: &str) -> String {
    // Convert backslashes to forward slashes
    let result = path.replace('\\', "/");
    // Use PathBuf to resolve . and .. segments, then convert back
    let path = Path::new(&result);
    let components: Vec<_> = path.components().collect();

    let mut resolved = Vec::new();
    for component in &components {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(s) => {
                resolved.push(s.to_string_lossy().to_string());
            }
            _ => {}
        }
    }

    resolved.join("/")
}

/// Return a relative path from the current working directory if the path
/// is inside it, or the absolute path otherwise.
pub fn to_relative_path(abs_path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        let abs = Path::new(abs_path);
        if let Ok(rel) = abs.strip_prefix(&cwd) {
            // If relative path doesn't go above cwd, use it
            let rel_str = rel.to_string_lossy().to_string();
            if !rel_str.starts_with("..") {
                return rel_str;
            }
        }
    }
    abs_path.to_string()
}

/// Return the directory containing the given path.
/// If the path is a directory, returns it unchanged.
/// If the path is a file or doesn't exist, returns its parent directory.
pub fn get_directory_for_path(path: &str) -> String {
    let p = Path::new(path);
    if p.is_dir() {
        return path.to_string();
    }
    p.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_diff() {
        let old = "line1\nline2\nline3";
        let new = "line1\nchanged\nline3";
        let result = simple_diff(old, new, "test.txt");
        assert!(result.contains("--- a/test.txt"));
        assert!(result.contains("+++ b/test.txt"));
        assert!(result.contains("-line2"));
        assert!(result.contains("+changed"));
    }

    #[test]
    fn test_convert_leading_tabs_to_spaces() {
        assert_eq!(convert_leading_tabs_to_spaces("\thello"), "  hello");
        assert_eq!(convert_leading_tabs_to_spaces("\t\thello"), "    hello");
        // Internal tabs preserved
        assert_eq!(convert_leading_tabs_to_spaces("\thel\tlo"), "  hel\tlo");
    }

    #[test]
    fn test_add_line_numbers() {
        let result = add_line_numbers(AddLineNumbersOptions {
            content: "hello\nworld",
            start_line: 1,
        });
        assert_eq!(result, "1\thello\n2\tworld");
    }

    #[test]
    fn test_strip_line_number_prefix() {
        assert_eq!(strip_line_number_prefix("1\thello"), "hello");
        assert_eq!(strip_line_number_prefix("  42\tworld"), "world");
        assert_eq!(strip_line_number_prefix("no prefix"), "no prefix");
    }

    #[test]
    fn test_paths_equal() {
        assert!(paths_equal("/foo/bar", "/foo/bar"));
        assert!(paths_equal("/foo/bar/", "/foo/bar"));
    }

    #[test]
    fn test_contains_path_traversal() {
        assert!(contains_path_traversal("../foo"));
        assert!(contains_path_traversal("foo/../bar"));
        assert!(contains_path_traversal(".."));
        assert!(!contains_path_traversal("foo/bar"));
        assert!(!contains_path_traversal("foo..bar"));
    }

    #[test]
    fn test_normalize_path_for_config_key() {
        assert_eq!(normalize_path_for_config_key("foo/bar"), "foo/bar");
        assert_eq!(normalize_path_for_config_key("foo/../bar"), "bar");
        assert_eq!(normalize_path_for_config_key("foo/./bar"), "foo/bar");
    }

    #[test]
    fn test_get_directory_for_path() {
        assert_eq!(get_directory_for_path("/foo/bar.txt"), "/foo");
        assert_eq!(get_directory_for_path("/foo/bar"), "/foo");
    }
}