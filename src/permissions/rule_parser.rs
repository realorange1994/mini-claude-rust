//! Rule parser - Parse ToolName(content) rule strings
//!
//! Matches upstream's rule parsing format:
//! - "Bash" → tool-level rule (matches entire tool)
//! - "Bash(git:*)" → content-specific rule
//! - "Bash(*)", "Bash()" → collapses to tool-level
//! - Escaped parens: \( → (, \) → ) inside content
//! - Legacy aliases: Task → Agent, KillShell → TaskStop

use std::fmt;

/// A parsed permission rule
#[derive(Debug, Clone)]
pub struct ParsedRule {
    pub tool_name: String,
    pub content: String,
    pub behavior: String, // "allow" | "deny" | "ask"
}

impl fmt::Display for ParsedRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_tool_level() {
            write!(f, "{}", self.tool_name)
        } else {
            write!(f, "{}({})", self.tool_name, self.content)
        }
    }
}

impl ParsedRule {
    /// Returns true if this is a tool-level rule (no content or wildcard content)
    pub fn is_tool_level(&self) -> bool {
        self.content.is_empty() || self.content == "*"
    }

    /// Check if this rule matches a given tool name
    /// Handles MCP server-level matching: "mcp__server1" matches "mcp__server1__tool1"
    pub fn tool_matches(&self, query_tool: &str) -> bool {
        if self.tool_name == query_tool {
            return true;
        }
        // MCP server-level matching
        if self.tool_name.starts_with("mcp__") && query_tool.starts_with("mcp__") {
            // "mcp__server1" matches "mcp__server1__tool1"
            if query_tool.starts_with(&format!("{}__", self.tool_name)) {
                return true;
            }
            // "mcp__server1__*" matches all tools from server1
            if self.tool_name.ends_with("__*") {
                let server_prefix = &self.tool_name[..self.tool_name.len() - 2]; // remove __*
                if query_tool.starts_with(&format!("{}__", server_prefix)) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if this rule's content matches the given content
    pub fn content_matches(&self, query_content: &str) -> bool {
        if self.is_tool_level() {
            return true; // tool-level matches everything
        }
        glob_match(&self.content, query_content)
    }
}

/// Parse a single rule string like "Bash(git:*)" or "Edit"
pub fn parse_rule(rule_string: &str) -> Result<ParsedRule, String> {
    let s = rule_string.trim();
    if s.is_empty() {
        return Err("empty rule string".to_string());
    }

    // Find the opening paren
    let open_idx = match s.find('(') {
        Some(idx) => idx,
        None => {
            // No parens: tool-level rule
            let tool_name = apply_legacy_aliases(s);
            return Ok(ParsedRule {
                tool_name,
                content: String::new(),
                behavior: String::new(),
            });
        }
    };

    // Find the matching closing paren
    let close_idx = match s.rfind(')') {
        Some(idx) => idx,
        None => return Err(format!("unmatched '(' in rule: {}", s)),
    };

    let tool_name_part = &s[..open_idx];
    let content_part = &s[open_idx + 1..close_idx];

    let tool_name = apply_legacy_aliases(tool_name_part);

    // Unescape \( and \) in content
    let content = content_part
        .replace("\\(", "(")
        .replace("\\)", ")");

    // Collapse "Bash(*)" and "Bash()" to tool-level
    if content == "*" || content.is_empty() {
        return Ok(ParsedRule {
            tool_name,
            content: String::new(),
            behavior: String::new(),
        });
    }

    Ok(ParsedRule {
        tool_name,
        content,
        behavior: String::new(),
    })
}

/// Parse multiple rule strings with a given behavior
pub fn parse_rules(rules: &[String], behavior: &str) -> Vec<ParsedRule> {
    rules
        .iter()
        .filter_map(|r| match parse_rule(r) {
            Ok(mut rule) => {
                rule.behavior = behavior.to_string();
                Some(rule)
            }
            Err(_) => None,
        })
        .collect()
}

/// Apply legacy tool name aliases
fn apply_legacy_aliases(name: &str) -> String {
    match name {
        "Task" => "Agent".to_string(),
        "KillShell" => "TaskStop".to_string(),
        "AgentOutputTool" => "TaskOutput".to_string(),
        _ => name.to_string(),
    }
}

/// Glob-style pattern matching for rule content
/// Supports:
/// - "git:*" → matches "git status", "git log", etc.
/// - "*.env" → matches ".env", "foo.env"
/// - ".env" → matches ".env" exactly
/// - "*" → matches everything
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern == text {
        return true;
    }

    // Handle colon-separated prefix: "git:*"
    if let Some(colon_pos) = pattern.find(':') {
        let prefix = &pattern[..colon_pos];
        let suffix = &pattern[colon_pos + 1..];

        // Check if text starts with the prefix
        if !text.starts_with(prefix) {
            return false;
        }
        let text_rest = &text[prefix.len()..];

        if suffix == "*" {
            return true;
        }
        if suffix.is_empty() {
            return text_rest.is_empty();
        }
        return glob_match(suffix, text_rest);
    }

    // Handle leading wildcard: "*.env"
    if let Some(stripped) = pattern.strip_prefix('*') {
        return text.ends_with(stripped) || text.ends_with(&format!(".{}", stripped));
    }

    // Handle trailing wildcard: "git*"
    if let Some(stripped) = pattern.strip_suffix('*') {
        return text.starts_with(stripped);
    }

    // Handle middle wildcard: "foo*bar"
    if let Some(star_pos) = pattern.find('*') {
        let prefix = &pattern[..star_pos];
        let suffix = &pattern[star_pos + 1..];
        return text.starts_with(prefix) && text.ends_with(suffix);
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_level() {
        let rule = parse_rule("Bash").unwrap();
        assert_eq!(rule.tool_name, "Bash");
        assert!(rule.is_tool_level());
        assert!(rule.content.is_empty());
    }

    #[test]
    fn test_parse_content_rule() {
        let rule = parse_rule("Bash(git:*)").unwrap();
        assert_eq!(rule.tool_name, "Bash");
        assert_eq!(rule.content, "git:*");
        assert!(!rule.is_tool_level());
    }

    #[test]
    fn test_parse_wildcard_collapses() {
        let rule = parse_rule("Bash(*)").unwrap();
        assert!(rule.is_tool_level());

        let rule = parse_rule("Bash()").unwrap();
        assert!(rule.is_tool_level());
    }

    #[test]
    fn test_parse_escaped_parens() {
        let rule = parse_rule(r"Edit(foo\(1\))").unwrap();
        assert_eq!(rule.content, "foo(1)");
    }

    #[test]
    fn test_legacy_aliases() {
        let rule = parse_rule("Task").unwrap();
        assert_eq!(rule.tool_name, "Agent");

        let rule = parse_rule("KillShell").unwrap();
        assert_eq!(rule.tool_name, "TaskStop");
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match(".env", ".env"));
        assert!(!glob_match(".env", "foo.env"));
    }

    #[test]
    fn test_glob_match_colon_wildcard() {
        assert!(glob_match("git:*", "git status"));
        assert!(glob_match("git:*", "git log"));
        assert!(!glob_match("git:*", "npm install"));
    }

    #[test]
    fn test_glob_match_leading_wildcard() {
        assert!(glob_match("*.env", ".env"));
        assert!(glob_match("*.env", "foo.env"));
    }

    #[test]
    fn test_glob_match_trailing_wildcard() {
        assert!(glob_match("git*", "git status"));
        assert!(glob_match("git*", "git"));
    }

    #[test]
    fn test_mcp_server_matching() {
        let rule = ParsedRule {
            tool_name: "mcp__server1".to_string(),
            content: String::new(),
            behavior: "allow".to_string(),
        };
        assert!(rule.tool_matches("mcp__server1__tool1"));
        assert!(!rule.tool_matches("mcp__server2__tool1"));
    }
}
