//! Path validation - Validate file paths for dangerous patterns

use std::path::Path;

use super::internal_paths::is_internal_editable_path;
use super::rule_store::RuleStore;

/// Operation type for path validation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OperationType {
    Read,
    Write,
    Create,
}

/// Result of path validation
#[derive(Debug)]
pub struct PathValidationResult {
    pub allowed: bool,
    pub reason: String,  // "rule", "safetyCheck", "other", "workingDir"
    pub message: String,
}

/// Validate a path for write/create operations
pub fn validate_path(
    path: &str,
    op_type: OperationType,
    rule_store: Option<&RuleStore>,
    cwd: &str,
) -> PathValidationResult {
    let mut result = PathValidationResult {
        allowed: false,
        reason: "other".to_string(),
        message: String::new(),
    };

    // 1. Expand ~ to homedir
    let expanded = expand_tilde(path);
    let resolved = resolve_symlinks(&expanded);

    // 2. Block UNC paths
    if is_unc_path(&expanded) {
        result.message = "UNC network paths require manual approval".to_string();
        result.reason = "other".to_string();
        return result;
    }

    // 3. Block tilde variants
    if is_tilde_variant(&expanded) {
        result.message = "Tilde expansion variants require manual approval".to_string();
        result.reason = "other".to_string();
        return result;
    }

    // 4. Block shell expansion syntax
    if has_shell_expansion(&expanded) {
        result.message = "Shell expansion syntax in paths requires manual approval".to_string();
        result.reason = "other".to_string();
        return result;
    }

    // 5. Block glob patterns in write/create operations
    if (op_type == OperationType::Write || op_type == OperationType::Create)
        && has_glob_chars(&expanded)
    {
        result.message = "Glob patterns are not allowed in write operations".to_string();
        result.reason = "other".to_string();
        return result;
    }

    // 6a. Check deny rules in rule store
    if let Some(rs) = rule_store {
        if let Some(rule) = rs.find_content_rule("Edit", path, "deny") {
            result.message = format!("Path denied by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
        if let Some(rule) = rs.find_content_rule("Write", path, "deny") {
            result.message = format!("Path denied by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
    }

    // 6b. Internal editable paths bypass dangerous-dir checks
    if op_type == OperationType::Write || op_type == OperationType::Create {
        if is_internal_editable_path(&resolved, cwd) {
            result.allowed = true;
            return result;
        }
    }

    // 6c. Safety checks
    if op_type == OperationType::Write || op_type == OperationType::Create {
        if let Some(safety_result) = check_path_safety(&resolved) {
            if !safety_result.allowed {
                result.message = safety_result.message;
                result.reason = "safetyCheck".to_string();
                return result;
            }
        }
    }

    // 6d. Check ask rules
    if let Some(rs) = rule_store {
        if let Some(rule) = rs.find_content_rule("Edit", path, "ask") {
            result.message = format!("Path requires confirmation by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
        if let Some(rule) = rs.find_content_rule("Write", path, "ask") {
            result.message = format!("Path requires confirmation by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
    }

    // 6e. Check allow rules
    if let Some(rs) = rule_store {
        if rs.find_content_rule("Edit", path, "allow").is_some() {
            result.allowed = true;
            return result;
        }
        if rs.find_content_rule("Write", path, "allow").is_some() {
            result.allowed = true;
            return result;
        }
    }

    // Default: deny
    result.message = "Path access not allowed by default".to_string();
    result.reason = "other".to_string();
    result
}

/// Validate a path for read operations
pub fn validate_read_path(
    path: &str,
    rule_store: Option<&RuleStore>,
) -> PathValidationResult {
    let mut result = PathValidationResult {
        allowed: false,
        reason: "other".to_string(),
        message: String::new(),
    };

    let expanded = expand_tilde(path);

    // 1. UNC paths
    if is_unc_path(&expanded) {
        result.message = "UNC network paths require manual approval".to_string();
        result.reason = "other".to_string();
        return result;
    }

    // 2. Suspicious Windows patterns
    if let Some(msg) = has_suspicious_windows_pattern(&expanded) {
        result.message = msg;
        result.reason = "other".to_string();
        return result;
    }

    // 3. Check deny rules
    if let Some(rs) = rule_store {
        if let Some(rule) = rs.find_content_rule("Read", path, "deny") {
            result.message = format!("Path denied by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
    }

    // 4. Check ask rules
    if let Some(rs) = rule_store {
        if let Some(rule) = rs.find_content_rule("Read", path, "ask") {
            result.message = format!("Path requires confirmation by rule: {}", rule);
            result.reason = "rule".to_string();
            return result;
        }
    }

    // 5. Internal readable paths
    if super::internal_paths::is_internal_readable_path(&expand_tilde(path)) {
        result.allowed = true;
        return result;
    }

    // 6. Check allow rules
    if let Some(rs) = rule_store {
        if rs.find_content_rule("Read", path, "allow").is_some() {
            result.allowed = true;
            return result;
        }
    }

    result.message = "Path read not allowed by default".to_string();
    result.reason = "other".to_string();
    result
}

fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok())
        {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}

fn resolve_symlinks(path: &str) -> String {
    std::fs::read_link(path)
        .map(|l| l.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

fn is_tilde_variant(path: &str) -> bool {
    path.starts_with("~+") || path.starts_with("~-")
}

fn has_shell_expansion(path: &str) -> bool {
    path.starts_with('$') || path.starts_with('%') || path.starts_with('=')
}

fn has_glob_chars(path: &str) -> bool {
    path.contains('*') || path.contains('?')
        || path.contains('[') || path.contains('{')
        || path.contains('}')
}

fn check_path_safety(path: &str) -> Option<PathValidationResult> {
    // Check for dangerous file patterns
    let path_lower = path.to_lowercase();
    let dangerous = [
        ".ssh", ".aws", ".netrc", ".git-credentials",
        ".env", ".env.local",
    ];
    for d in &dangerous {
        if path_lower.contains(d) {
            return Some(PathValidationResult {
                allowed: false,
                reason: "safetyCheck".to_string(),
                message: format!("Path {} matches dangerous pattern: {}", path, d),
            });
        }
    }
    None
}

fn has_suspicious_windows_pattern(path: &str) -> Option<String> {
    use regex::Regex;

    // 8.3 short names
    if Regex::new(r"~\d")
        .map(|r| r.is_match(path))
        .unwrap_or(false)
    {
        return Some("path contains 8.3 short name pattern".to_string());
    }

    // Long path prefixes
    if path.starts_with("\\\\?\\") || path.starts_with("\\\\.\\")
        || path.starts_with("//?/") || path.starts_with("//./")
    {
        return Some("path uses a long path prefix".to_string());
    }

    // Trailing dots or spaces
    if Regex::new(r"[.\s]+$")
        .map(|r| r.is_match(path))
        .unwrap_or(false)
    {
        return Some("path has trailing dots or spaces".to_string());
    }

    // DOS device names
    if Regex::new(r"\.(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?i)$")
        .map(|r| r.is_match(path))
        .unwrap_or(false)
    {
        return Some("path contains a DOS device name".to_string());
    }

    // Three or more consecutive dots
    if Regex::new(r"(^|[/\\])\.{3,}([/\\]|$)")
        .map(|r| r.is_match(path))
        .unwrap_or(false)
    {
        return Some("path contains three or more consecutive dots as a path component".to_string());
    }

    None
}
