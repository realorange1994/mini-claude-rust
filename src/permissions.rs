use crate::tools::ToolResult;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Write};

/// Permission mode for tool execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Ask,
    Auto,
    Plan,
}

impl PermissionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "auto" => PermissionMode::Auto,
            "plan" => PermissionMode::Plan,
            _ => PermissionMode::Ask,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Ask => "ask",
            PermissionMode::Auto => "auto",
            PermissionMode::Plan => "plan",
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for PermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_str(s))
    }
}

impl Serialize for PermissionMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PermissionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_str(&s))
    }
}

/// Read a single keystroke from the console, bypassing stdin.
/// On Windows, uses `_getch()` from msvcrt which reads directly from the console.
#[cfg(windows)]
fn read_console_char() -> Option<char> {
    extern "C" {
        fn _getch() -> i32;
    }
    let ch = unsafe { _getch() };
    match ch {
        -1 | 0 => {
            // Extended key (function keys, arrows) - read second byte and ignore
            unsafe { _getch() };
            None
        }
        27 => Some('\x1b'),  // Escape
        13 | 10 => Some('\n'), // Enter
        _ => {
            let c = ch as u8 as char;
            if c.is_ascii_control() && c != '\n' && c != '\r' {
                return None;
            }
            Some(c)
        }
    }
}

#[cfg(not(windows))]
fn read_console_char() -> Option<char> {
    use std::process::Command;
    let output = Command::new("sh")
        .args(["-c", "stty -icanon -echo; dd bs=1 count=1 2>/dev/null; stty icanon echo"])
        .output()
        .ok()?;
    let bytes = output.stdout;
    if bytes.is_empty() {
        return None;
    }
    Some(bytes[0] as char)
}

/// Read a single char from console with prompt, bypassing stdin buffering
fn read_key(prompt: &str) -> Option<char> {
    // Print prompt
    print!("{}", prompt);
    let _ = io::stdout().flush();

    let ch = read_console_char();

    // Echo the character and newline
    if let Some(c) = ch {
        println!("{}", c);
    } else {
        println!("n");
    }
    ch
}

/// PermissionGate checks if tool execution is allowed
pub struct PermissionGate {
    pub config: crate::config::Config,
}

impl PermissionGate {
    pub fn new(config: crate::config::Config) -> Self {
        Self { config }
    }

    /// Check if a command is safe (read-only, no approval needed)
    fn is_safe_command(&self, command: &str) -> bool {
        let cmd = command.trim().to_lowercase();
        for allowed in &self.config.allowed_commands {
            let allowed_lower = allowed.to_lowercase();
            if cmd == allowed_lower || cmd.starts_with(&format!("{} ", allowed_lower)) {
                return true;
            }
        }
        false
    }

    /// Check if a tool call matches any denied patterns
    fn check_denied_patterns(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> Option<String> {
        let denied_patterns = &self.config.denied_patterns;

        // Check command parameter for exec/terminal tools
        if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
            let cmd_lower = cmd.to_lowercase();
            for pattern in denied_patterns {
                if cmd_lower.contains(&pattern.to_lowercase()) {
                    return Some(format!(
                        "Permission denied: matches denied pattern '{}'",
                        pattern
                    ));
                }
            }
        }

        // Check path parameter for file tools (write_file, edit_file, multi_edit, fileops)
        if ["write_file", "edit_file", "multi_edit", "fileops"].contains(&tool_name) {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                let path_lower = path.to_lowercase();
                for pattern in denied_patterns {
                    if path_lower.contains(&pattern.to_lowercase()) {
                        return Some(format!(
                            "Permission denied: matches denied pattern '{}'",
                            pattern
                        ));
                    }
                }
            }
        }

        None
    }

    /// Ask user for approval via direct console input
    fn ask_user(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>, warning: Option<&str>) -> bool {
        // Format the tool call for display
        let preview = match tool_name {
            "exec" | "terminal" => {
                if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                    format!("$ {}", cmd)
                } else {
                    format!("[{}]", tool_name)
                }
            }
            "write_file" | "edit_file" | "multi_edit" | "fileops" => {
                if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                    format!("[{}] {}", tool_name, path)
                } else {
                    format!("[{}]", tool_name)
                }
            }
            _ => {
                format!("[{}]", tool_name)
            }
        };

        let prompt = if let Some(w) = warning {
            format!("\n  Allow {}?\n  Warning: {}\n  [y/N]: ", preview, w)
        } else {
            format!("\n  Allow {}? [y/N]: ", preview)
        };

        let ch = read_key(&prompt);
        let ch = ch.unwrap_or('n');
        ch == 'y' || ch == 'Y'
    }

    /// Check if a tool should be allowed to execute
    /// Returns Some(ToolResult) if blocked, None if allowed
    pub fn check(&self, tool: &dyn crate::tools::Tool, params: std::collections::HashMap<String, serde_json::Value>) -> Option<ToolResult> {
        match self.config.permission_mode {
            PermissionMode::Auto => {
                // All allowed in auto mode
                None
            }
            PermissionMode::Plan => {
                // Only read-only tools in plan mode
                let name = tool.name();
                let read_only_tools = [
                    "read_file", "grep", "glob", "list_dir", "git",
                    "system", "process", "terminal", "web_search",
                    "web_search_scraper", "web_fetch", "runtime_info", "list_mcp_tools",
                    "mcp_server_status", "list_skills",
                ];

                if !read_only_tools.contains(&name) {
                    return Some(ToolResult::error(format!(
                        "Permission denied: {} is not allowed in PLAN mode (read-only)",
                        name
                    )));
                }
                None
            }
            PermissionMode::Ask => {
                // Layer 1: Tool's own permission check (warnings)
                // Always ask user if tool returned a warning
                if let Some(warning) = tool.check_permissions(&params) {
                    if !self.ask_user(tool.name(), &params, Some(&warning.output)) {
                        return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                    }
                    return None; // user approved
                }

                // Layer 1.5: Denied patterns check (hard denial)
                if let Some(denial) = self.check_denied_patterns(tool.name(), &params) {
                    return Some(ToolResult::error(denial));
                }

                // Layer 2: Dangerous tools
                let dangerous_tools = ["exec", "write_file", "edit_file", "multi_edit", "fileops"];
                let tool_name = tool.name();
                let is_dangerous = dangerous_tools.contains(&tool_name);

                if is_dangerous {
                    if tool_name == "exec" {
                        if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                            if self.is_safe_command(cmd) {
                                return None; // Safe command, allow without asking
                            }
                        }
                    }
                    if !self.ask_user(tool_name, &params, None) {
                        return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                    }
                }

                None
            }
        }
    }

    /// Get current permission mode
    #[allow(dead_code)]
    pub fn mode(&self) -> PermissionMode {
        self.config.permission_mode
    }

    /// Set permission mode
    #[allow(dead_code)]
    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.config.permission_mode = mode;
    }
}

impl Clone for PermissionGate {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_mode_from_str() {
        assert_eq!(PermissionMode::from_str("ask"), PermissionMode::Ask);
        assert_eq!(PermissionMode::from_str("auto"), PermissionMode::Auto);
        assert_eq!(PermissionMode::from_str("plan"), PermissionMode::Plan);
        assert_eq!(PermissionMode::from_str("ASK"), PermissionMode::Ask);
        assert_eq!(PermissionMode::from_str("unknown"), PermissionMode::Ask);
    }

    #[test]
    fn test_permission_mode_as_str() {
        assert_eq!(PermissionMode::Ask.as_str(), "ask");
        assert_eq!(PermissionMode::Auto.as_str(), "auto");
        assert_eq!(PermissionMode::Plan.as_str(), "plan");
    }
}
