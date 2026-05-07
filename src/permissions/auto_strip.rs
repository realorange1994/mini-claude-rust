//! Auto strip - Strip dangerous allow rules in auto mode
//!
//! Matches upstream's CROSS_PLATFORM_CODE_EXEC + DANGEROUS_BASH_PATTERNS.

use super::rule_parser::ParsedRule;
use super::rule_store::RuleStore;

/// Dangerous shell patterns that should not be auto-allowed
pub const DANGEROUS_SHELL_PATTERNS: &[&str] = &[
    // CROSS_PLATFORM_CODE_EXEC
    "python", "python3", "python2", "node", "deno", "tsx", "ruby", "perl",
    "php", "lua", "npx", "bunx", "npm run", "yarn run", "pnpm run",
    "bun run", "bash", "sh", "ssh",
    // BASH additions
    "zsh", "fish", "eval", "exec", "env", "xargs", "sudo",
];

/// Check if a parsed allow rule matches a dangerous shell pattern
pub fn is_dangerous_allow_rule(rule: &ParsedRule) -> bool {
    if rule.behavior != "allow" {
        return false;
    }

    // Only applies to Bash and Exec tools
    let tool_lower = rule.tool_name.to_lowercase();
    if tool_lower != "bash" && tool_lower != "exec" {
        return false;
    }

    if rule.is_tool_level() {
        return true;
    }

    // Check if content matches any dangerous pattern
    for pattern in DANGEROUS_SHELL_PATTERNS {
        if matches_dangerous_pattern(&rule.content, pattern) {
            return true;
        }
    }

    false
}

/// Check if content matches a dangerous pattern
fn matches_dangerous_pattern(content: &str, pattern: &str) -> bool {
    let content_lower = content.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    // Exact match
    if content_lower == pattern_lower {
        return true;
    }

    // Prefix syntax: "python:*"
    if content_lower == format!("{}:*", pattern_lower) {
        return true;
    }

    // Trailing wildcard: "python*"
    if content_lower == format!("{}*", pattern_lower) {
        return true;
    }

    // Space wildcard: "python *"
    if content_lower == format!("{} *", pattern_lower) {
        return true;
    }

    // Flag wildcard: "python -*"
    if content_lower == format!("{} -*", pattern_lower) {
        return true;
    }

    // Content starts with pattern followed by space
    if content_lower.starts_with(&format!("{} ", pattern_lower)) {
        return true;
    }

    // Content starts with pattern followed by colon
    if content_lower.starts_with(&format!("{}:", pattern_lower)) {
        return true;
    }

    false
}

/// Summary of stripped rules for display
pub fn stripped_rules_summary(stash: &[(String, Vec<ParsedRule>)]) -> String {
    if stash.is_empty() {
        return String::new();
    }

    let parts: Vec<String> = stash
        .iter()
        .flat_map(|(key, rules)| {
            rules.iter().map(move |r| format!("{}: {}", key, r))
        })
        .collect();

    format!("Stripped dangerous allow rules: {}", parts.join(", "))
}
