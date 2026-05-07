use crate::tools::{expand_path, is_unc_path};
use std::path::Path;

/// Permission behavior for tool self-check.
/// Matches upstream's PermissionResult behavior: allow, deny, ask, passthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionBehavior {
    /// Grant permission without user interaction.
    Allow,
    /// Hard denial that is bypass-immune (even in bypass mode).
    Deny,
    /// Requires user approval. When from a safetyCheck, bypass-immune.
    Ask,
    /// Defers to the framework's mode-based logic.
    Passthrough,
}

/// Structured result from a tool's permission self-check.
#[derive(Debug, Clone)]
pub struct ToolPermissionResult {
    pub behavior: PermissionBehavior,
    /// Human-readable reason (required for deny/ask).
    pub message: String,
    /// Reason category: "safetyCheck", "tool", "rule", or empty.
    pub decision_reason: &'static str,
}

impl ToolPermissionResult {
    pub fn allow() -> Self {
        Self {
            behavior: PermissionBehavior::Allow,
            message: String::new(),
            decision_reason: "",
        }
    }

    pub fn deny(msg: &str) -> Self {
        Self {
            behavior: PermissionBehavior::Deny,
            message: msg.to_string(),
            decision_reason: "tool",
        }
    }

    pub fn ask(msg: &str, reason: &'static str) -> Self {
        Self {
            behavior: PermissionBehavior::Ask,
            message: msg.to_string(),
            decision_reason: reason,
        }
    }

    pub fn passthrough() -> Self {
        Self {
            behavior: PermissionBehavior::Passthrough,
            message: String::new(),
            decision_reason: "",
        }
    }

    /// Returns true if this result should NOT be overridden by bypass mode.
    /// Deny is always bypass-immune. Ask from safetyCheck is bypass-immune.
    pub fn is_bypass_immune(&self) -> bool {
        match self.behavior {
            PermissionBehavior::Deny => true,
            PermissionBehavior::Ask => self.decision_reason == "safetyCheck",
            _ => false,
        }
    }
}

/// Dangerous files that should not be auto-edited without explicit permission.
/// Matches upstream's DANGEROUS_FILES.
const DANGEROUS_FILES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
    ".claude.json",
];

/// Dangerous directories that should be protected from auto-editing.
/// Matches upstream's DANGEROUS_DIRECTORIES.
const DANGEROUS_DIRECTORIES: &[&str] = &[
    ".git",
    ".vscode",
    ".idea",
    ".claude",
];

/// Check if a file path points to a dangerous file or directory.
/// Returns Some(message) if dangerous, None if safe.
pub fn is_dangerous_file_path(path: &str) -> Option<String> {
    let expanded = expand_path(path);
    let path_str = expanded.to_string_lossy();

    // UNC path check
    if is_unc_path(Path::new(&*path_str)) {
        return Some("path appears to be a UNC path that could access network resources".to_string());
    }

    // Normalize to forward slashes for segment comparison
    let normalized = path_str.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();

    // Check if any path segment is a dangerous directory
    for (i, seg) in segments.iter().enumerate() {
        let seg_lower = seg.to_lowercase();
        for &dir in DANGEROUS_DIRECTORIES {
            // Skip .claude/worktrees/ (structural path, not user-created)
            if dir == ".claude" {
                if i + 1 < segments.len() && segments[i + 1].to_lowercase() == "worktrees" {
                    continue;
                }
            }
            if seg_lower == dir {
                return Some(format!("file is inside a sensitive directory: {}", dir));
            }
        }
    }

    // Check filename against dangerous files
    if let Some(file_name) = segments.last() {
        let file_lower = file_name.to_lowercase();
        for &dangerous_file in DANGEROUS_FILES {
            if file_lower == dangerous_file.to_lowercase() {
                return Some(format!("file is a sensitive configuration file: {}", dangerous_file));
            }
        }
    }

    None
}

/// Detect suspicious Windows path patterns.
/// Returns Some(message) if suspicious, None if safe.
pub fn has_suspicious_windows_path_pattern(path: &str) -> Option<String> {
    // 8.3 short names (GIT~1, CLAUDE~1)
    if path.contains('~') {
        let re = regex::Regex::new(r"~\d").unwrap();
        if re.is_match(path) {
            return Some("path contains 8.3 short name pattern".to_string());
        }
    }
    // Long path prefixes
    if path.starts_with(r"\\?\") || path.starts_with(r"\\.\")
        || path.starts_with("//?/") || path.starts_with("//./")
    {
        return Some("path uses a long path prefix".to_string());
    }
    // Trailing dots or spaces
    if path.ends_with('.') || path.ends_with(' ') {
        return Some("path has trailing dots or spaces".to_string());
    }
    // DOS device names (.git.CON, settings.json.PRN, etc.)
    if let Some(dot_pos) = path.rfind('.') {
        let suffix = &path[dot_pos + 1..];
        let suffix_upper = suffix.to_uppercase();
        let dos_names = ["CON", "PRN", "AUX", "NUL"];
        if dos_names.contains(&suffix_upper.as_str()) {
            return Some("path contains a DOS device name".to_string());
        }
        if (suffix_upper.starts_with("COM") || suffix_upper.starts_with("LPT"))
            && suffix_upper.len() == 4
            && suffix_upper.chars().nth(3).map_or(false, |c| c >= '1' && c <= '9')
        {
            return Some("path contains a DOS device name".to_string());
        }
    }
    // Three or more consecutive dots as path component (.../file.txt)
    if path.contains("...") {
        let re = regex::Regex::new(r"(^|[/\\])\.{3,}([/\\]|$)").unwrap();
        if re.is_match(path) {
            return Some("path contains three or more consecutive dots as a path component".to_string());
        }
    }
    // UNC paths
    if is_unc_path(Path::new(path)) {
        return Some("path is a UNC path that could leak credentials".to_string());
    }
    None
}

/// Check if a file path is safe for auto-editing.
/// Returns ToolPermissionResult: passthrough if safe, ask with safetyCheck if unsafe.
pub fn check_path_safety_for_auto_edit(path: &str) -> ToolPermissionResult {
    // Check suspicious Windows path patterns
    if has_suspicious_windows_path_pattern(path).is_some() {
        return ToolPermissionResult::ask(
            &format!("Claude requested permissions to write to {}, which contains a suspicious Windows path pattern that requires manual approval.", path),
            "safetyCheck",
        );
    }

    // Check dangerous files/directories
    if let Some(msg) = is_dangerous_file_path(path) {
        return ToolPermissionResult::ask(
            &format!("Claude requested permissions to edit {} which is a sensitive file: {}", path, msg),
            "safetyCheck",
        );
    }

    ToolPermissionResult::passthrough()
}
