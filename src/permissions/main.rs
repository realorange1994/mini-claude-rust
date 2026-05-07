use crate::auto_classifier::{AutoModeClassifier, is_auto_allowlisted};
use crate::context::ConversationContext;
use crate::tools::{ToolResult, ApprovalRequirement, ToolPermissionResult, PermissionBehavior};
use super::rule_store::RuleStore;
use super::path_validation::{validate_path, validate_read_path, OperationType, PathValidationResult};
use super::auto_strip::is_dangerous_allow_rule;
use super::rule_parser::ParsedRule;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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

/// Records a user's explicit approval for a tool action (from AskUserQuestion).
struct ApprovedAction {
    tool_name: String,
    params: String, // compact serialization for matching
    expires: Instant,
}

/// PermissionGate checks if tool execution is allowed
pub struct PermissionGate {
    pub config: crate::config::Config,
    classifier: Option<AutoModeClassifier>,
    transcript_src: Option<Arc<tokio::sync::RwLock<ConversationContext>>>,
    denial_count: AtomicUsize,
    recently_approved: std::sync::Mutex<Vec<ApprovedAction>>,
    /// Rule store for permission rules loaded from settings
    rule_store: Option<Arc<RuleStore>>,
    /// Project directory for path validation
    project_dir: Option<String>,
    /// Stripped dangerous rules (for auto mode restoration)
    stripped_rules: std::sync::Mutex<Option<Vec<(String, Vec<ParsedRule>)>>>,
}

impl PermissionGate {
    pub fn new(config: crate::config::Config) -> Self {
        Self {
            config,
            classifier: None,
            transcript_src: None,
            denial_count: AtomicUsize::new(0),
            recently_approved: std::sync::Mutex::new(Vec::new()),
            rule_store: None,
            project_dir: None,
            stripped_rules: std::sync::Mutex::new(None),
        }
    }

    /// Set the rule store and project directory for permission rule checks.
    pub fn with_rule_store(&mut self, store: Arc<RuleStore>, project_dir: String) {
        self.rule_store = Some(store);
        self.project_dir = Some(project_dir);
    }

    /// Set the auto mode classifier.
    pub fn set_classifier(&mut self, classifier: AutoModeClassifier) {
        self.classifier = Some(classifier);
    }

    /// Set the transcript source for the classifier.
    pub fn set_transcript_source(&mut self, src: Arc<tokio::sync::RwLock<ConversationContext>>) {
        self.transcript_src = Some(src);
    }

    /// Clear classifier cache and approval state after context compaction.
    pub fn reset_post_compact(&self) {
        if let Some(ref classifier) = self.classifier {
            classifier.clear_cache();
        }
        if let Ok(mut approved) = self.recently_approved.lock() {
            approved.clear();
        }
        self.denial_count.store(0, std::sync::atomic::Ordering::SeqCst);
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
        // Step 0: Auto mode — strip dangerous allow rules on entry
        let auto_mode_stripped = self.try_strip_dangerous_rules();

        // Helper: restore stripped rules on early exit
        let restore_stripped = || {
            if auto_mode_stripped {
                self.restore_stripped_rules();
            }
        };

        let tool_name = tool.name();
        let upstream_name = super::upstream_to_internal(tool_name);
        let content = self.extract_rule_content(tool_name, &params);
        let path_param = self.extract_path_param(tool_name, &params);

        // STEP 1a: Tool-level deny rule (bypass-immune)
        if let Some(rule) = self.find_tool_level_deny(&upstream_name) {
            restore_stripped();
            return Some(ToolResult::error(format!("Permission denied by rule: {}", rule)));
        }

        // STEP 1b: Content-specific deny rule (bypass-immune)
        if !content.is_empty() {
            if let Some(rule) = self.find_content_deny(&upstream_name, &content) {
                restore_stripped();
                return Some(ToolResult::error(format!("Permission denied by rule: {}", rule)));
            }
        }

        // STEP 1c: File path validation for write/read/fileops tools
        if !path_param.is_empty() {
            let op_type = if self.is_write_tool(tool_name) {
                OperationType::Write
            } else {
                OperationType::Read
            };
            let v_result = if op_type == OperationType::Read {
                self.validate_read_path_for_tool(&path_param)
            } else {
                self.validate_path_for_tool(&path_param, op_type)
            };
            if let Some(v_result) = v_result {
                if !v_result.allowed {
                    // Bypass mode: allow all path access (skip validation)
                    // Auto mode: skip path validation — let classifier decide
                    let mode = *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner());
                    if mode == PermissionMode::Bypass || mode == PermissionMode::Auto {
                        // Fall through to allow / classifier evaluation
                    } else {
                        restore_stripped();
                        if v_result.reason == "safetyCheck" || v_result.reason == "rule" {
                            if self.should_avoid_prompts() {
                                return Some(ToolResult::error(format!(
                                    "Permission denied: {} (interactive prompts disabled for sub-agent)",
                                    v_result.message
                                )));
                            }
                            if !self.ask_user(tool_name, &params, Some(&v_result.message)) {
                                return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                            }
                        } else {
                            return Some(ToolResult::error(format!("Permission denied: {}", v_result.message)));
                        }
                    }
                }
            }
        }

        // STEP 1d: Tool-level ask rule (bypass-immune)
        // Bypass mode: skip ask rules, allow through
        let mode = *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner());
        if mode != PermissionMode::Bypass {
            if let Some(rule) = self.find_tool_level_ask(&upstream_name) {
                restore_stripped();
                if self.should_avoid_prompts() {
                    return Some(ToolResult::error(format!(
                        "Permission denied: {} requires confirmation (interactive prompts disabled for sub-agent)",
                        rule
                    )));
                }
                let msg = format!("Tool requires confirmation by rule: {}", rule);
                if !self.ask_user(tool_name, &params, Some(&msg)) {
                    return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                }
                return None;
            }

            // STEP 1e: Content-specific ask rule (bypass-immune)
            if !content.is_empty() {
                if let Some(rule) = self.find_content_ask(&upstream_name, &content) {
                    restore_stripped();
                    if self.should_avoid_prompts() {
                        return Some(ToolResult::error(format!(
                            "Permission denied: {} requires confirmation (interactive prompts disabled for sub-agent)",
                            rule
                        )));
                    }
                    let msg = format!("Tool requires confirmation by rule: {}", rule);
                    if !self.ask_user(tool_name, &params, Some(&msg)) {
                        return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                    }
                    return None;
                }
            }
        }

        // Step 2: tool-level self-check returns PermissionResult
        let result = tool.check_permissions(&params);

        // Step 2d: deny is always bypass-immune
        if result.behavior == PermissionBehavior::Deny {
            restore_stripped();
            return Some(ToolResult::error(format!("Permission denied: {}", result.message)));
        }

        // Step 2e: ask from safetyCheck is bypass-immune
        // Step 2f: ask from tool rules (non-safetyCheck) — also bypass-immune per upstream
        // Bypass mode: skip, allow through
        let mode = *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner());
        if mode != PermissionMode::Bypass && result.behavior == PermissionBehavior::Ask {
            restore_stripped();
            if self.should_avoid_prompts() {
                return Some(ToolResult::error(format!(
                    "Permission denied: {} (interactive prompts disabled for sub-agent)", result.message
                )));
            }
            if !self.ask_user(tool.name(), &params, Some(&result.message)) {
                return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
            }
            return None; // user approved
        }

        // Layer 1.5: Denied patterns check (hard denial)
        if let Some(denial) = self.check_denied_patterns(tool.name(), &params) {
            restore_stripped();
            return Some(ToolResult::error(denial));
        }

        // When should_avoid_prompts is true (sub-agents), tools that require user
        // approval are auto-denied. Read-only and auto-approved tools pass through.
        // This prevents sub-agents from ever blocking on an interactive user prompt.
        if self.should_avoid_prompts() {
            match tool.approval_requirement() {
                ApprovalRequirement::Required => {
                    // Cannot prompt user in sub-agent context, deny
                    return Some(ToolResult::error(format!(
                        "Permission denied: {} requires user approval and sub-agents cannot prompt for it.",
                        tool.name()
                    )));
                }
                ApprovalRequirement::Classifier => {
                    // For exec, still allow safe read-only commands
                    if tool.name() == "exec" {
                        if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                            if self.is_safe_command(cmd) {
                                return None;
                            }
                        }
                    }
                    // Other classifier tools need classifier or are denied
                    return Some(ToolResult::error(format!(
                        "Permission denied: {} requires classifier evaluation and sub-agents cannot provide it.",
                        tool.name()
                    )));
                }
                ApprovalRequirement::Auto => {
                    // Auto-approved tools pass through
                    return None;
                }
            }
        }

        // Step 2a: bypass mode — allow all (only reached if 1d-1g didn't return)
        match *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner()) {
            PermissionMode::Bypass => {
                restore_stripped();
                // Bypass mode: allow all tools directly.
                // Layer 1 deny/ask (bypass-immune) already handled above.
                // This aligns with upstream's bypassPermissions behavior (step 2a).
                None
            }
            PermissionMode::Auto => {
                // Step 3c: toolAlwaysAllowedRule — if rule store has a tool-level
                // allow rule for this tool, allow without classifier evaluation.
                if let Some(ref store) = self.rule_store {
                    if store.has_allow_rule(&upstream_name) {
                        restore_stripped();
                        return None;
                    }
                }
                // Use auto mode classifier (if available) or fall back to allow-all
                restore_stripped();
                self.check_auto_mode(tool.name(), &params, &result)
            }
            PermissionMode::Plan => {
                // Plan mode: read-only tools only. Blocks write operations.
                // Matches Go's Plan mode writeTools check.
                let write_tools = [
                    "exec", "write_file", "edit_file", "multi_edit", "fileops",
                ];
                if write_tools.contains(&tool.name()) {
                    restore_stripped();
                    return Some(ToolResult::error(format!(
                        "Permission denied: '{}' is blocked in PLAN mode (read-only).",
                        tool.name()
                    )));
                }
                restore_stripped();
                None
            }
            PermissionMode::Ask => {
                // Use approval_requirement() to decide behavior
                match tool.approval_requirement() {
                    ApprovalRequirement::Auto => {
                        // Auto-approved tools pass through
                        restore_stripped();
                        return None;
                    }
                    ApprovalRequirement::Required => {
                        // Always prompt user for approval
                        if !self.ask_user(tool.name(), &params, None) {
                            restore_stripped();
                            return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                        }
                        restore_stripped();
                        return None;
                    }
                    ApprovalRequirement::Classifier => {
                        // Classifier tools: check for safe exec commands, otherwise prompt
                        if tool.name() == "exec" {
                            if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                                if self.is_safe_command(cmd) {
                                    restore_stripped();
                                    return None;
                                }
                            }
                        }
                        if !self.ask_user(tool.name(), &params, None) {
                            restore_stripped();
                            return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
                        }
                        restore_stripped();
                        return None;
                    }
                }
            }
        }
    }

    /// Get current permission mode
    #[allow(dead_code)]
    pub fn mode(&self) -> PermissionMode {
        *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set permission mode
    #[allow(dead_code)]
    pub fn set_mode(&mut self, mode: PermissionMode) {
        *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Auto mode permission check using the classifier.
    /// Uses the tool's approval_requirement() to decide the path:
    /// - Auto: auto-allow
    /// - Required: block (cannot prompt user in auto mode without classifier)
    /// - Classifier: evaluate via LLM classifier (with whitelist fallback)
    /// After consecutive denials exceeding the limit, falls back to interactive prompt.
    /// When classifier is nil/disabled: auto-allow (legacy behavior).
    /// Auto mode permission check using the classifier.
    /// Uses the tool's approval_requirement() to decide the path:
    /// - Auto: auto-allow
    /// - Required: block (cannot prompt user in auto mode without classifier)
    /// - Classifier: evaluate via LLM classifier (with whitelist fallback)
    /// After consecutive denials exceeding the limit, falls back to interactive prompt.
    /// When classifier is nil/disabled: auto-allow (legacy behavior).
    fn check_auto_mode(
        &self,
        tool_name: &str,
        params: &std::collections::HashMap<String, serde_json::Value>,
        tool_result: &ToolPermissionResult,
    ) -> Option<ToolResult> {
        // If tool returned ask with classifier_approvable=false, always prompt user (never bypass)
        if tool_result.behavior == PermissionBehavior::Ask && !tool_result.classifier_approvable {
            if self.should_avoid_prompts() {
                return Some(ToolResult::error(format!(
                    "Permission denied: {} (interactive prompts disabled for sub-agent)", tool_result.message
                )));
            }
            if !self.ask_user(tool_name, params, Some(&tool_result.message)) {
                return Some(ToolResult::error("Permission denied: user rejected.".to_string()));
            }
            return None;
        }

        // Check auto allowlist (whitelist of known-safe operations)
        if is_auto_allowlisted(tool_name, params) {
            self.denial_count.store(0, Ordering::SeqCst);
            return None;
        }

        // If classifier is not available, fall back to legacy behavior: allow all
        let classifier = match &self.classifier {
            Some(c) if c.is_enabled() => c,
            _ => return None, // No classifier configured: auto mode allows all tools (old behavior)
        };

        // Check if this tool was explicitly approved by the user via AskUserQuestion.
        // If the user said "Yes, continue", their explicit consent is binding.
        if self.tool_matches_recent_approval(tool_name, params) {
            self.denial_count.store(0, Ordering::SeqCst);
            return None;
        }

        // Build transcript for classifier context
        let transcript = if let Some(src) = &self.transcript_src {
            match src.try_read() {
                Ok(ctx) => crate::transcript_builder::build_compact_transcript(&ctx, 20),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        };

        // Convert params for classifier
        let result = classifier.classify(tool_name, params, &transcript);

        if !result.allow {
            let count = self.denial_count.fetch_add(1, Ordering::SeqCst) + 1;
            let denial_limit = self.config.auto_denial_limit;
            // After consecutive denials, fall back to interactive prompt
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

    /// Record that the user explicitly approved a tool action via AskUserQuestion.
    /// The approval is valid for 2 minutes and allows matching tool calls to bypass the classifier.
    pub fn record_user_approval(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) {
        let compact = compact_params(tool_name, params);
        let action = ApprovedAction {
            tool_name: tool_name.to_string(),
            params: compact,
            expires: Instant::now() + Duration::from_secs(120),
        };
        let mut approved = self.recently_approved.lock().unwrap_or_else(|e| e.into_inner());
        approved.push(action);
        // Trim expired entries
        let now = Instant::now();
        approved.retain(|a| a.expires > now);
    }

    /// Check if this tool call matches a recent user approval from AskUserQuestion.
    fn tool_matches_recent_approval(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> bool {
        let compact = compact_params(tool_name, params);
        let now = Instant::now();
        let approved = self.recently_approved.lock().unwrap_or_else(|e| e.into_inner());
        for a in approved.iter() {
            if a.expires > now && a.tool_name == tool_name && a.params == compact {
                return true;
            }
        }
        false
    }
}

/// Produce a compact string representation of tool params for matching user approvals.
fn compact_params(tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> String {
    match tool_name {
        "exec" => {
            if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                return cmd.to_string();
            }
        }
        "write_file" | "edit_file" | "multi_edit" => {
            if let Some(p) = params.get("file_path").and_then(|v| v.as_str()) {
                return p.to_string();
            }
        }
        "fileops" => {
            if let Some(p) = params.get("path").and_then(|v| v.as_str()) {
                return p.to_string();
            }
        }
        "git" => {
            if let Some(args) = params.get("args").and_then(|v| v.as_str()) {
                return args.to_string();
            }
        }
        _ => {
            if let Ok(json) = serde_json::to_string(params) {
                return json;
            }
        }
    }
    String::new()
}

impl Clone for PermissionGate {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            classifier: None, // Classifiers are not cloned (they hold HTTP clients)
            transcript_src: self.transcript_src.clone(),
            denial_count: AtomicUsize::new(self.denial_count.load(Ordering::SeqCst)),
            recently_approved: std::sync::Mutex::new(Vec::new()), // Don't clone pending approvals
            rule_store: None,
            project_dir: None,
            stripped_rules: std::sync::Mutex::new(None),
        }
    }
}

// ─── Rule checking helpers ─────────────────────────────────────────────────────

impl PermissionGate {
    /// Try to strip dangerous allow rules in auto mode
    fn try_strip_dangerous_rules(&self) -> bool {
        let mode = *self.config.permission_mode.lock().unwrap_or_else(|e| e.into_inner());
        if mode != PermissionMode::Auto {
            return false;
        }
        if self.should_avoid_prompts() {
            return false;
        }
        if self.rule_store.is_none() {
            return false;
        }

        let mut guard = self.stripped_rules.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return false; // Already stripped
        }

        if let Some(ref store) = self.rule_store {
            let stash = store.strip_dangerous_allow_rules();
            if !stash.is_empty() {
                *guard = Some(stash);
                return true;
            }
        }
        false
    }

    /// Restore stripped dangerous rules
    fn restore_stripped_rules(&self) {
        if let Ok(mut guard) = self.stripped_rules.lock() {
            if let Some(stash) = guard.take() {
                if let Some(ref store) = self.rule_store {
                    store.restore_stripped_rules(stash);
                }
            }
        }
    }

    fn find_tool_level_deny(&self, upstream_name: &str) -> Option<String> {
        let store = self.rule_store.as_ref()?;
        if store.has_deny_rule(upstream_name) {
            for rule in store.get_rules_for_tool(upstream_name) {
                if rule.behavior == "deny" && rule.is_tool_level() {
                    return Some(rule.to_string());
                }
            }
            return Some(upstream_name.to_string()); // Tool-level deny
        }
        None
    }

    fn find_content_deny(&self, upstream_name: &str, content: &str) -> Option<String> {
        let store = self.rule_store.as_ref()?;
        store
            .find_content_rule(upstream_name, content, "deny")
            .map(|r| r.to_string())
    }

    fn find_tool_level_ask(&self, upstream_name: &str) -> Option<String> {
        let store = self.rule_store.as_ref()?;
        if store.has_ask_rule(upstream_name) {
            for rule in store.get_rules_for_tool(upstream_name) {
                if rule.behavior == "ask" && rule.is_tool_level() {
                    return Some(rule.to_string());
                }
            }
            return Some(upstream_name.to_string());
        }
        None
    }

    fn find_content_ask(&self, upstream_name: &str, content: &str) -> Option<String> {
        let store = self.rule_store.as_ref()?;
        store
            .find_content_rule(upstream_name, content, "ask")
            .map(|r| r.to_string())
    }

    fn extract_rule_content(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> String {
        match tool_name {
            "exec" => params.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "write_file" | "edit_file" | "multi_edit" => params.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "read_file" => params.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "fileops" => params.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "git" => params.get("args").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            _ => String::new(),
        }
    }

    fn extract_path_param(&self, tool_name: &str, params: &std::collections::HashMap<String, serde_json::Value>) -> String {
        match tool_name {
            "write_file" | "edit_file" | "multi_edit" | "read_file" => {
                params.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string()
            }
            "fileops" => params.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            _ => String::new(),
        }
    }

    fn is_write_tool(&self, tool_name: &str) -> bool {
        matches!(tool_name, "write_file" | "edit_file" | "multi_edit" | "fileops")
    }

    fn validate_path_for_tool(&self, path: &str, op_type: OperationType) -> Option<PathValidationResult> {
        let store = self.rule_store.as_ref().map(|s| s.as_ref());
        let cwd = self.project_dir.as_deref().unwrap_or("");
        Some(validate_path(path, op_type, store, cwd))
    }

    fn validate_read_path_for_tool(&self, path: &str) -> Option<PathValidationResult> {
        let store = self.rule_store.as_ref().map(|s| s.as_ref());
        Some(validate_read_path(path, store))
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
