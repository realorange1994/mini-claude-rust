use crate::auto_classifier::{AutoModeClassifier, is_auto_allowlisted};
use crate::context::ConversationContext;
use crate::tools::ToolResult;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Permission mode for tool execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Ask,
    Auto,
    Plan,
    Bypass,
}

impl PermissionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bypass" => PermissionMode::Bypass,
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
            PermissionMode::Bypass => "bypass",
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

/// Check if a string contains shell metacharacters that could be used for
/// command injection (e.g., `git status; rm -rf /` after `git status `).
fn contains_shell_metacharacters(s: &str) -> bool {
    s.contains('&') || s.contains('|') || s.contains(';') || s.contains('`') ||
    s.contains('$') || s.contains('(') || s.contains(')') || s.contains('{') ||
    s.contains('}') || s.contains('[') || s.contains(']') || s.contains('<') ||
    s.contains('>') || s.contains('!') || s.contains('#') || s.contains('~') ||
    s.contains('\n') || s.contains('\r')
}

/// PermissionGate checks if tool execution is allowed
pub struct PermissionGate {
    pub config: crate::config::Config,
    classifier: Option<AutoModeClassifier>,
    transcript_src: Option<Arc<tokio::sync::RwLock<ConversationContext>>>,
    denial_count: AtomicUsize,
}

impl PermissionGate {
    pub fn new(config: crate::config::Config) -> Self {
        Self {
            config,
            classifier: None,
            transcript_src: None,
            denial_count: AtomicUsize::new(0),
        }
    }

    /// Set the auto mode classifier.
    pub fn set_classifier(&mut self, classifier: AutoModeClassifier) {
        self.classifier = Some(classifier);
    }

    /// Set the transcript source for the classifier.
    pub fn set_transcript_source(&mut self, src: Arc<tokio::sync::RwLock<ConversationContext>>) {
        self.transcript_src = Some(src);
    }

    /// Check if a command is safe (read-only, no approval needed)
    fn is_safe_command(&self, command: &str) -> bool {
        let cmd = command.trim().to_lowercase();
        for allowed in &self.config.allowed_commands {
            let allowed_lower = allowed.to_lowercase();
            if cmd == allowed_lower {
                return true;
            }
            let prefix = format!("{} ", allowed_lower);
            if cmd.starts_with(&prefix) {
                let remainder = &cmd[prefix.len()..];
                if !contains_shell_metacharacters(remainder) {
                    return true;
                }
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

        // Check path parameter for file tools (write_file, edit_file, multi_edit use file_path; fileops uses path)
        if ["write_file", "edit_file", "multi_edit"].contains(&tool_name) {
            if let Some(path) = params.get("file_path").and_then(|v| v.as_str()) {
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
        } else if tool_name == "fileops" {
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

    /// Should interactive permission prompts be avoided?
    /// When true (e.g., for sub-agents with no terminal user), dangerous tools
    /// are auto-denied instead of blocking on user prompts.
    fn should_avoid_prompts(&self) -> bool {
        self.config.should_avoid_permission_prompts
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
            "write_file" | "edit_file" | "multi_edit" => {
                if let Some(path) = params.get("file_path").and_then(|v| v.as_str()) {
                    format!("[{}] {}", tool_name, path)
                } else {
                    format!("[{}]", tool_name)
                }
            }
            "fileops" => {
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
        // UNCONDITIONAL: Always run tool's own security check (dangerous operations, etc.)
        // This must not be bypassed by any permission mode.
        if let Some(denial) = tool.check_permissions(&params) {
            return Some(denial);
        }

        // When should_avoid_prompts is true (sub-agents), dangerous tools are
        // auto-denied and non-dangerous tools are auto-allowed. This prevents
        // sub-agents from ever blocking on an interactive user prompt.
        if self.should_avoid_prompts() {
            let dangerous_tools = ["exec", "write_file", "edit_file", "multi_edit", "fileops"];
            let tool_name = tool.name();
            if dangerous_tools.contains(&tool_name) {
                // For exec, still allow safe commands
                if tool_name == "exec" {
                    if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                        if self.is_safe_command(cmd) {
                            return None; // Safe command, allow
                        }
                    }
                }
                return Some(ToolResult::error(format!(
                    "Permission denied: {} is a dangerous tool and sub-agents cannot prompt for approval.",
                    tool_name
                )));
            }
            // Non-dangerous tool: auto-allow
            return None;
        }

        match self.config.permission_mode {
            PermissionMode::Bypass => {
                // Allow all tools directly without classifier evaluation.
                // Tool's own security check still runs unconditionally.
                None
            }
            PermissionMode::Auto => {
                // Use auto mode classifier (if available) or fall back to allow-all
                self.check_auto_mode(tool, &params)
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

    /// Auto mode permission check using the classifier.
    /// Safe tools are auto-allowed. Other tools are evaluated by the LLM classifier.
    /// After consecutive denials exceeding the limit, falls back to interactive prompt.
    /// When classifier is nil/disabled: auto-allow (legacy behavior).
    fn check_auto_mode(
        &self,
        tool: &dyn crate::tools::Tool,
        params: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<ToolResult> {
        let tool_name = tool.name();

        // Fast path: whitelisted tools are always allowed
        if is_auto_allowlisted(tool_name, params) {
            // Log what was auto-allowed so the decision is visible in traces
            let desc = match tool_name {
                "exec" => {
                    let cmd = params.get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<no command>");
                    format!("WHITELISTED: [exec]: {}", cmd)
                }
                "git" => {
                    let op = params.get("operation")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<no operation>");
                    format!("WHITELISTED: [git]: {}", op)
                }
                "process" => {
                    let op = params.get("operation")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<no operation>");
                    format!("WHITELISTED: [process]: {}", op)
                }
                other => format!("WHITELISTED: [{}]", other),
            };
            eprintln!("  [auto-classifier] {}", desc);
            self.denial_count.store(0, Ordering::SeqCst);
            return None;
        }

        // If classifier is not available, fall back to legacy behavior: allow all
        let classifier = match &self.classifier {
            Some(c) if c.is_enabled() => c,
            _ => return None, // No classifier configured: auto mode allows all tools (old behavior)
        };

        // Build transcript for classifier context
        let transcript = if let Some(src) = &self.transcript_src {
            // Try to get a read lock (non-blocking to avoid deadlocks)
            match src.try_read() {
                Ok(ctx) => crate::transcript_builder::build_compact_transcript(&ctx, 20),
                Err(_) => String::new(), // Lock contention: skip transcript
            }
        } else {
            String::new()
        };

        // Convert params from HashMap<String, Value> for classifier
        let result = classifier.classify(tool_name, params, &transcript);

        if !result.allow {
            let count = self.denial_count.fetch_add(1, Ordering::SeqCst) + 1;
            let denial_limit = self.config.auto_denial_limit;
            // After consecutive denials, fall back to interactive prompt
            // (but avoid prompts for sub-agents with no terminal user)
            if count >= denial_limit && !self.should_avoid_prompts() {
                eprintln!(
                    "  [auto-classifier] {} consecutive denials, falling back to manual approval",
                    count
                );
                if self.ask_user(tool_name, params, None) {
                    self.denial_count.store(0, Ordering::SeqCst);
                    return None;
                }
                return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
            }
            return Some(ToolResult::error(format!("Permission denied: {}", result.reason)));
        }

        // Allowed: reset denial count
        self.denial_count.store(0, Ordering::SeqCst);
        None
    }
}

impl Clone for PermissionGate {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            classifier: None, // Classifiers are not cloned (they hold HTTP clients)
            transcript_src: self.transcript_src.clone(),
            denial_count: AtomicUsize::new(self.denial_count.load(Ordering::SeqCst)),
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
