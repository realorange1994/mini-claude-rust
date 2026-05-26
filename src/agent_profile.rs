//! Agent profile configuration from external files.
//! Ported from upstream agent_profile.go (92 lines).
//!
//! Loads SOUL.md, USER.md, and base_prompt.md from ~/.claude/
//! to customize agent personality and behavior without modifying source code.

use std::fs;
use std::path::PathBuf;

/// Agent profile holds customizable prompt sections.
#[derive(Debug, Clone, Default)]
pub struct AgentProfile {
    /// Agent personality/soul (from SOUL.md).
    pub soul: String,
    /// User context (from USER.md).
    pub user: String,
    /// Base prompt additions (from base_prompt.md).
    pub base_prompt: String,
}

impl AgentProfile {
    /// Load agent profile from ~/.claude/ directory.
    /// Missing files are silently skipped (defaults to empty strings).
    pub fn load() -> Self {
        let claude_dir = match dirs::home_dir() {
            Some(h) => h.join(".claude"),
            None => return Self::default(),
        };

        Self {
            soul: read_file(&claude_dir.join("SOUL.md")),
            user: read_file(&claude_dir.join("USER.md")),
            base_prompt: read_file(&claude_dir.join("base_prompt.md")),
        }
    }

    /// Load agent profile from a custom directory.
    pub fn load_from(dir: &PathBuf) -> Self {
        Self {
            soul: read_file(&dir.join("SOUL.md")),
            user: read_file(&dir.join("USER.md")),
            base_prompt: read_file(&dir.join("base_prompt.md")),
        }
    }

    /// Check if the user has a custom soul override.
    pub fn has_custom_soul(&self) -> bool {
        !self.soul.is_empty()
    }

    /// Format the profile for injection into the system prompt.
    /// Returns sections wrapped in XML-like tags.
    pub fn format_for_prompt(&self) -> String {
        let mut parts = Vec::new();

        if !self.base_prompt.is_empty() {
            parts.push(format!("<base_prompt>\n{}\n</base_prompt>", self.base_prompt.trim()));
        }

        if !self.soul.is_empty() {
            parts.push(format!("<soul>\n{}\n</soul>", self.soul.trim()));
        }

        if !self.user.is_empty() {
            parts.push(format!("<user_context>\n{}\n</user_context>", self.user.trim()));
        }

        parts.join("\n\n")
    }
}

/// Read a file to string, returning empty string on any error.
fn read_file(path: &PathBuf) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_profile() {
        let profile = AgentProfile::default();
        assert!(!profile.has_custom_soul());
        assert!(profile.format_for_prompt().is_empty());
    }

    #[test]
    fn test_format_for_prompt() {
        let profile = AgentProfile {
            soul: "Be helpful and concise.".to_string(),
            user: "I am a Rust developer.".to_string(),
            base_prompt: String::new(),
        };

        let formatted = profile.format_for_prompt();
        assert!(formatted.contains("<soul>"));
        assert!(formatted.contains("Be helpful and concise."));
        assert!(formatted.contains("<user_context>"));
        assert!(formatted.contains("I am a Rust developer."));
        assert!(!formatted.contains("<base_prompt>"));
    }

    #[test]
    fn test_has_custom_soul() {
        let profile = AgentProfile {
            soul: "Custom soul".to_string(),
            ..Default::default()
        };
        assert!(profile.has_custom_soul());
    }

    #[test]
    fn test_format_empty_sections() {
        let profile = AgentProfile {
            soul: String::new(),
            user: String::new(),
            base_prompt: String::new(),
        };
        assert!(profile.format_for_prompt().is_empty());
    }
}
