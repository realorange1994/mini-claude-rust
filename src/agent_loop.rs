use crate::compact::Compactor;
use crate::config::Config;
use crate::context::{ConversationContext, ConversationEntry, MessageContent, ToolUseBlock, ToolResultBlock, ToolResultContent};
use crate::filehistory::FileHistory;
use crate::permissions::PermissionGate;
use crate::skills::SkillTracker;
use crate::streaming::{CollectHandler, TerminalHandler, StallDetector, process_sse_events, ToolCallInfo};
use crate::tools::{expand_path, truncate_at, ToolResult, Registry};
use crate::transcript::{Transcript, TranscriptEntry, TYPE_USER, TYPE_ASSISTANT, TYPE_TOOL_USE, TYPE_TOOL_RESULT, TYPE_SYSTEM, TYPE_ERROR, TYPE_COMPACT, TYPE_SUMMARY};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Continue reason tracks why the agent loop is continuing (inspired by Claude Code's 7 continue reasons)
#[derive(Debug, Clone, PartialEq, Default)]
enum ContinueReason {
    #[default]
    None,
    NextTurn,
    PromptTooLong,
    MaxOutputTokens,
    ModelConfused,
    ContextOverflow,
}

/// Transition tracking for context management (kept for tool->text transition tracking)
#[derive(Debug, Clone, PartialEq, Default)]
enum Transition {
    #[default]
    None,
    ToolsToText,
}

/// The core agent loop that drives the AI interaction
pub struct AgentLoop {
    pub config: Config,
    pub registry: Arc<RwLock<Registry>>,
    gate: PermissionGate,
    context: Arc<RwLock<ConversationContext>>,
    client: reqwest::Client,
    use_stream: bool,
    max_tool_chars: usize,
    max_turns: usize,
    base_url: String,
    api_key: String,
    transcript: Transcript,
    compactor: RwLock<Compactor>,
    file_history: Arc<FileHistory>,
    rt: tokio::runtime::Runtime,
    /// Shared interrupted flag (can be set from Ctrl+C handler)
    interrupted: Arc<std::sync::atomic::AtomicBool>,
    /// Tracks which skills have been shown/read/used across turns
    skill_tracker: Arc<RwLock<SkillTracker>>,
}

impl AgentLoop {
    pub fn new(config: Config, registry: Registry, use_stream: bool) -> Self {
        let api_key = config.api_key.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
                .unwrap_or_default()
        });

        if api_key.is_empty() {
            eprintln!("Error: ANTHROPIC_API_KEY environment variable is not set (or use --api-key)");
            std::process::exit(1);
        }

        let base_url = config.base_url.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".to_string())
        });

        let client_builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                // Set both x-api-key (Anthropic native) and Bearer (OpenAI-compatible)
                // to support both native Anthropic API and third-party proxy APIs
                headers.insert(
                    reqwest::header::HeaderName::from_static("x-api-key"),
                    api_key.parse().unwrap(),
                );
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", api_key).parse().unwrap(),
                );
                headers
            });

        let client = client_builder.build().unwrap_or_default();

        let max_turns = config.max_turns;
        let file_history = config.file_history.clone().unwrap_or_else(|| Arc::new(FileHistory::new()));
        let context = ConversationContext::new(config.clone());
        let gate = PermissionGate::new(config.clone());

        // Initialize transcript writer (matching Go's behavior)
        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let transcript_dir = PathBuf::from(".claude").join("transcripts");
        let _ = std::fs::create_dir_all(&transcript_dir);
        let transcript_path = transcript_dir.join(format!("{}.jsonl", session_id));
        let transcript = Transcript::new(&transcript_path);
        // Write system entry with model/mode info (matching Go format)
        let _ = transcript.add_system(format!("model={}, mode={}", gate.config.model, gate.config.permission_mode));

        // Initialize compactor with config values
        let compactor = RwLock::new(
            Compactor::new()
                .with_threshold(config.auto_compact_threshold)
                .with_buffer(config.auto_compact_buffer)
                .with_max_tokens(crate::compact::model_context_window(&gate.config.model))
        );

        // Create multi-thread tokio runtime for this agent
        // This properly handles spawn_blocking calls from reqwest
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        Self {
            config: gate.config.clone(),
            registry: Arc::new(RwLock::new(registry)),
            gate,
            context: Arc::new(RwLock::new(context)),
            client,
            use_stream,
            max_tool_chars: 8192,
            max_turns,
            base_url,
            api_key,
            transcript,
            compactor,
            file_history,
            rt,
            interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skill_tracker: Arc::new(RwLock::new(SkillTracker::new())),
        }
    }

    /// Create agent from existing transcript (resume session)
    pub fn from_transcript(
        config: Config,
        registry: Registry,
        use_stream: bool,
        transcript_path: &Path,
    ) -> Result<Self> {
        let api_key = config.api_key.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
                .unwrap_or_default()
        });

        if api_key.is_empty() {
            return Err(anyhow!("ANTHROPIC_API_KEY environment variable is not set"));
        }

        let base_url = config.base_url.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".to_string())
        });

        let client_builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::HeaderName::from_static("x-api-key"),
                    api_key.parse().unwrap(),
                );
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", api_key).parse().unwrap(),
                );
                headers
            });

        let client = client_builder.build().unwrap_or_default();

        let max_turns = config.max_turns;
        let file_history = config.file_history.clone().unwrap_or_else(|| Arc::new(FileHistory::new()));
        let gate = PermissionGate::new(config.clone());

        // Read transcript and rebuild context
        let transcript = Transcript::new(&transcript_path.to_path_buf());
        let entries = transcript.read_all()
            .map_err(|e| anyhow!("Failed to read transcript: {}", e))?;

        let context = Self::rebuild_context_from_transcript(&entries, config.clone());

        // Create new transcript file for this session
        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let transcript_dir = PathBuf::from(".claude").join("transcripts");
        let _ = std::fs::create_dir_all(&transcript_dir);
        let new_transcript_path = transcript_dir.join(format!("{}.jsonl", session_id));
        let new_transcript = Transcript::new(&new_transcript_path);

        // Log resume info
        let _ = new_transcript.add_user(format!(
            "Resumed from {} ({} messages restored)",
            transcript_path.display(),
            entries.len()
        ));

        let compactor = RwLock::new(
            Compactor::new()
                .with_threshold(config.auto_compact_threshold)
                .with_buffer(config.auto_compact_buffer)
                .with_max_tokens(crate::compact::model_context_window(&config.model))
        );

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        Ok(Self {
            config: gate.config.clone(),
            registry: Arc::new(RwLock::new(registry)),
            gate,
            context: Arc::new(RwLock::new(context)),
            client,
            use_stream,
            max_tool_chars: 8192,
            max_turns,
            base_url,
            api_key,
            transcript: new_transcript,
            compactor,
            file_history,
            rt,
            interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skill_tracker: Arc::new(RwLock::new(SkillTracker::new())),
        })
    }

    /// Rebuild conversation context from transcript entries (Go format)
    fn rebuild_context_from_transcript(
        entries: &[TranscriptEntry],
        config: Config,
    ) -> ConversationContext {
        let mut context = ConversationContext::new(config);

        // Group consecutive tool_use entries and tool_result entries
        // Anthropic API expects:
        // - assistant message can have multiple tool_use blocks
        // - user message can have multiple tool_result blocks
        let mut pending_tool_uses: Vec<ToolUseBlock> = Vec::new();
        let mut pending_tool_results: Vec<ToolResultBlock> = Vec::new();

        for entry in entries {
            match entry.type_.as_str() {
                TYPE_USER => {
                    // Flush any pending tool results first
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(pending_tool_results.clone());
                        pending_tool_results.clear();
                    }
                    // Flush any pending tool uses (shouldn't happen before user, but handle it)
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    context.add_user_message(entry.content.clone());
                }
                TYPE_ASSISTANT => {
                    // Flush pending items first
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(pending_tool_results.clone());
                        pending_tool_results.clear();
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    // Add assistant text if present
                    if !entry.content.is_empty() {
                        context.add_assistant_text(entry.content.clone());
                    }
                }
                TYPE_TOOL_USE => {
                    // Accumulate tool_use blocks - they will be flushed when we see
                    // a tool_result, user, or assistant entry
                    if let (Some(name), Some(id)) = (&entry.tool_name, &entry.tool_id) {
                        let input: HashMap<String, serde_json::Value> = entry.tool_args
                            .clone()
                            .unwrap_or_default();
                        pending_tool_uses.push(ToolUseBlock {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        });
                    }
                }
                TYPE_TOOL_RESULT => {
                    // Flush pending tool uses first (create assistant message with all tool calls)
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    // Accumulate tool_result blocks
                    if let Some(id) = &entry.tool_id {
                        pending_tool_results.push(ToolResultBlock {
                            tool_use_id: id.clone(),
                            content: vec![ToolResultContent::Text { text: entry.content.clone() }],
                            is_error: false,
                        });
                    }
                }
                TYPE_SYSTEM | TYPE_ERROR => {
                    // Flush pending items before system/error
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(pending_tool_results.clone());
                        pending_tool_results.clear();
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    // Skip system and error entries
                }
                TYPE_COMPACT => {
                    // Flush pending items before compact boundary
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(pending_tool_results.clone());
                        pending_tool_results.clear();
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    // Re-add compact boundary marker
                    let pre_tokens = entry.content
                        .split_whitespace()
                        .find(|s| s.chars().all(|c| c.is_ascii_digit()))
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);
                    context.add_compact_boundary(crate::context::CompactTrigger::Auto, pre_tokens);
                }
                TYPE_SUMMARY => {
                    // Flush pending items before summary
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(pending_tool_results.clone());
                        pending_tool_results.clear();
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(pending_tool_uses.clone());
                        pending_tool_uses.clear();
                    }
                    // Re-add summary
                    context.add_summary(entry.content.clone());
                }
                _ => {
                    // Skip unknown types
                }
            }
        }

        // Flush any remaining pending items at the end
        if !pending_tool_uses.is_empty() {
            context.add_assistant_tool_calls(pending_tool_uses);
        }
        if !pending_tool_results.is_empty() {
            context.add_tool_results(pending_tool_results);
        }

        context
    }

    /// Process a user message through the agent loop
    pub fn run(&self, user_message: &str) -> String {
        // Clear interrupted flag at start of new request
        self.interrupted.store(false, std::sync::atomic::Ordering::SeqCst);

        // Add user message to context
        {
            let mut ctx = self.context.blocking_write();
            ctx.add_user_message(user_message.to_string());
        }

        // Log user message to transcript
        let _ = self.transcript.add_user(user_message.to_string());

        // Refresh skills if files changed
        // Note: skill_loader is behind &self, so we skip refresh_if_changed here
        // (it requires &mut self on Loader). Skills are refreshed at startup.

        // Build system prompt with skill tracker
        let tracker = self.skill_tracker.blocking_read();
        let system_prompt = crate::config::build_system_prompt(
            &*self.registry.blocking_read(),
            &self.config.permission_mode,
            &self.config.project_dir,
            self.config.skill_loader.as_ref(),
            Some(&tracker),
        );
        drop(tracker);

        // Get messages and tools for API call
        let messages = self.entries_to_messages();
        let tools = self.get_tools_schema();

        // Run the async agent loop using stored runtime
        match self.rt.block_on(self.run_agent_loop(&system_prompt, &messages, &tools)) {
            Ok(response) => {
                // Log assistant response to transcript (Go format: content + model)
                let _ = self.transcript.add_assistant(response.clone(), Some(self.config.model.clone()));

                let mut ctx = self.context.blocking_write();
                ctx.add_assistant_text(response.clone());
                response
            }
            Err(e) => {
                let err_msg = format!("Error: {}", e);
                let _ = self.transcript.add_assistant(err_msg.clone(), Some(self.config.model.clone()));
                err_msg
            }
        }
    }

    /// Convert conversation entries to API message format (sync)
    fn entries_to_messages(&self) -> Vec<serde_json::Value> {
        let ctx = self.context.blocking_read();
        Self::entries_to_messages_from_ctx(ctx.entries())
    }

    /// Async version for use inside async context
    async fn entries_to_messages_async(&self) -> Vec<serde_json::Value> {
        let ctx = self.context.read().await;
        Self::entries_to_messages_from_ctx(ctx.entries())
    }

    /// Shared logic: convert entries to API message format
    fn entries_to_messages_from_ctx(entries: &[ConversationEntry]) -> Vec<serde_json::Value> {
        entries
            .iter()
            .filter_map(|entry| {
                let (role, content): (String, Vec<serde_json::Value>) = match &entry.content {
                    MessageContent::Text(text) => {
                        (entry.role.as_str().to_string(),
                        vec![serde_json::json!({"type": "text", "text": text})])
                    }
                    MessageContent::ToolUseBlocks(blocks) => {
                        ("assistant".to_string(),
                        blocks.iter().map(|b| {
                            serde_json::json!({
                                "type": "tool_use",
                                "id": b.id,
                                "name": b.name,
                                "input": b.input
                            })
                        }).collect())
                    }
                    MessageContent::ToolResultBlocks(blocks) => {
                        ("user".to_string(),
                        blocks.iter().map(|b| {
                            let content_values: Vec<serde_json::Value> = b.content.iter()
                                .filter_map(|c| serde_json::to_value(c).ok())
                                .collect();
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": b.tool_use_id,
                                "is_error": b.is_error,
                                "content": content_values
                            })
                        }).collect())
                    }
                    MessageContent::Summary(text) => {
                        ("user".to_string(),
                        vec![serde_json::json!({"type": "text", "text": text})])
                    }
                    MessageContent::CompactBoundary { .. } => {
                        // Skip compact boundaries in API messages — they're metadata only
                        return None;
                    }
                };
                Some(serde_json::json!({
                    "role": role,
                    "content": content
                }))
            })
            .collect()
    }

    /// Run the agent loop asynchronously
    async fn run_agent_loop(
        &self,
        system_prompt: &str,
        _messages: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> Result<String> {
        let mut turn = 0;
        let mut last_transition = Transition::None;
        let mut consecutive_stalls = 0;
        let mut context_errors = 0;
        let mut continue_reason = ContinueReason::None;
        let mut max_output_tokens_retries = 0;
        let mut consecutive_empty_responses = 0;
        const MAX_CONTEXT_RECOVERY: usize = 3;
        const MAX_OUTPUT_TOKENS_RETRIES: usize = 3;
        const MAX_EMPTY_RESPONSES: usize = 3;

        loop {
            // Check for interruption (Ctrl+C)
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok("[Interrupted by user]".to_string());
            }

            turn += 1;
            if turn > self.max_turns {
                break;
            }

            // Run compaction before API call (matching Go's CompactContext)
            // Uses async LLM-driven compaction when threshold is reached
            if self.config.auto_compact_enabled {
                {
                    let mut ctx = self.context.write().await;
                    let mut compactor = self.compactor.write().await;
                    let stats = compactor.compact(
                        &mut ctx,
                        &self.client,
                        &self.config.model,
                        &self.api_key,
                        &self.base_url,
                    ).await;
                    if stats.phase != crate::compact::CompactPhase::None {
                        eprintln!("[Compaction] {:?}: {} -> {} entries, ~{} tokens saved",
                            stats.phase, stats.entries_before, stats.entries_after, stats.estimated_tokens_saved);
                        // Log compaction event to transcript
                        let _ = self.transcript.add_compact(
                            format!("{:?}", stats.phase),
                            stats.estimated_tokens_saved,
                        );
                    }
                }

                // Log summary to transcript if one was added
                {
                    let ctx = self.context.read().await;
                    if let Some(idx) = ctx.last_compact_boundary_index() {
                        // Check if a summary follows the compact boundary
                        if idx + 1 < ctx.len() {
                            let summary_msg = &ctx.messages()[idx + 1];
                            if summary_msg.is_summary() {
                                if let Some(text) = summary_msg.text_content() {
                                    let _ = self.transcript.add_summary(text.to_string());
                                }
                            }
                        }
                    }
                }
            }

            // Rebuild messages from current context state (includes tool results)
            let messages = self.entries_to_messages_async().await;

            eprintln!();

            // Call with retry and fallback
            let result = self.call_with_retry_and_fallback(
                system_prompt,
                &messages,
                tools,
                &last_transition,
                &continue_reason,
            ).await;

            match result {
                Ok((tool_calls, text)) => {
                    consecutive_stalls = 0;
                    context_errors = 0;
                    max_output_tokens_retries = 0;
                    consecutive_empty_responses = 0;
                    continue_reason = ContinueReason::NextTurn;

                    if !tool_calls.is_empty() {
                        // Execute tools
                        last_transition = Transition::ToolsToText;

                        // Pre-check permissions sequentially (avoid concurrent stdin reads in ask mode)
                        struct ToolCallEntry {
                            index: usize,
                            tc: ToolCallInfo,
                            params: HashMap<String, serde_json::Value>,
                            denied: bool,
                            err_text: String,
                        }

                        let mut entries: Vec<ToolCallEntry> = Vec::new();
                        for (i, tc) in tool_calls.iter().enumerate() {
                            let params: HashMap<String, serde_json::Value> =
                                serde_json::from_str(&tc.arguments).unwrap_or_default();

                            let registry = self.registry.read().await;
                            let tool = registry.get(&tc.name);
                            let (denied, err_text) = if let Some(tool) = tool {
                                if let Some(result) = self.gate.check(tool.as_ref(), params.clone()) {
                                    (true, result.output)
                                } else {
                                    (false, String::new())
                                }
                            } else {
                                (true, format!("Error: unknown tool '{}'", tc.name))
                            };

                            entries.push(ToolCallEntry {
                                index: i,
                                tc: tc.clone(),
                                params,
                                denied,
                                err_text,
                            });
                        }

                        // Execute approved tool calls concurrently
                        let mut handles = Vec::new();
                        for entry in entries {
                            // Check for interruption before each tool
                            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                                break;
                            }

                            // Record tool_use to transcript BEFORE execution (matching Go's WriteToolUse)
                            let params_for_transcript: HashMap<String, serde_json::Value> =
                                serde_json::from_str(&entry.tc.arguments).unwrap_or_default();
                            let _ = self.transcript.add_tool_use(
                                entry.tc.id.clone(),
                                entry.tc.name.clone(),
                                params_for_transcript,
                            );

                            if entry.denied {
                                // Denied tools are handled immediately
                                let output = entry.err_text;
                                // Record tool_result to transcript
                                let _ = self.transcript.add_tool_result(
                                    entry.tc.id.clone(),
                                    entry.tc.name.clone(),
                                    output.clone(),
                                );
                                let tc = entry.tc.clone();
                                handles.push(tokio::task::spawn(async move {
                                    (entry.index, output, true, std::time::Duration::ZERO, false, tc.id, tc.name)
                                }));
                            } else {
                                // Approved tools execute concurrently
                                let tc = entry.tc.clone();
                                let params = entry.params.clone();
                                let max_tool_chars = self.max_tool_chars;
                                let interrupted = self.interrupted.clone();

                                // Clone what we need for the spawned task
                                let registry_clone = self.registry.clone();
                                let file_history = self.file_history.clone();

                                handles.push(tokio::task::spawn(async move {
                                    let start = std::time::Instant::now();
                                    let timeout = std::time::Duration::from_secs(300);

                                    let tool_name = tc.name.clone();

                                    // Capture path for post-execution snapshot before params is moved
                                    let snapshot_path = if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                        params.get("path").and_then(|v| v.as_str()).map(|p| expand_path(p))
                                    } else {
                                        None
                                    };

                                    // Build snapshot description from tool name and params
                                    let snapshot_desc = if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                        let old_str_preview = params.get("old_string").and_then(|v| v.as_str()).map(|s| {
                                            if s.len() > 50 { format!("{}...", &s[..50]) } else { s.to_string() }
                                        });
                                        let new_str_preview = params.get("new_string").and_then(|v| v.as_str()).map(|s| {
                                            if s.len() > 50 { format!("{}...", &s[..50]) } else { s.to_string() }
                                        });
                                        match (&*tool_name, old_str_preview, new_str_preview) {
                                            ("edit_file", Some(old), Some(new)) => format!("edit: '{}' → '{}'", old, new),
                                            ("multi_edit", _, _) => "multi_edit".to_string(),
                                            ("write_file", _, _) => "write_file".to_string(),
                                            _ => tool_name.clone(),
                                        }
                                    } else {
                                        String::new()
                                    };

                                    // Capture fileops delete info before params is moved
                                    let fileops_delete_info = if tool_name == "fileops" {
                                        let op = params.get("operation").and_then(|v| v.as_str());
                                        let path = params.get("path").and_then(|v| v.as_str()).map(|p| expand_path(p));
                                        match (op, path) {
                                            (Some("rm"), Some(p)) => Some(("rm", p)),
                                            (Some("rmrf"), Some(p)) => Some(("rmrf", p)),
                                            _ => None,
                                        }
                                    } else {
                                        None
                                    };

                                    // Auto-snapshot before write/edit tools (captures pre-modification state)
                                    // No description prefix — the post-execution snapshot carries the operation description
                                    if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                        if let Some(path) = snapshot_path.as_ref() {
                                            let _ = file_history.snapshot(path);
                                        }
                                    }

                                    let tool_result = tokio::time::timeout(timeout, async {
                                        let registry = registry_clone.read().await;
                                        let tool = registry.get(&tool_name);
                                        match tool {
                                            Some(t) => {
                                                // Validate required parameters (matching Go's ValidateParams)
                                                if let Some(val_err) = crate::tools::validate_params(t.as_ref(), &params) {
                                                    return val_err;
                                                }
                                                t.execute(params)
                                            }
                                            None => ToolResult::error(format!("Tool not found: {}", tool_name)),
                                        }
                                    }).await;

                                    let elapsed = start.elapsed();
                                    let output = match tool_result {
                                        Ok(result) => {
                                            // Post-execution snapshot: captures new files and final state
                                            if !result.is_error {
                                                if let Some(path) = snapshot_path.as_ref() {
                                                    let _ = file_history.snapshot_current_with_desc(path, snapshot_desc.clone());
                                                }
                                                // Clear file history for deleted files (rm/rmrf)
                                                if let Some((op, del_path)) = &fileops_delete_info {
                                                    file_history.clear(del_path);
                                                    if *op == "rmrf" {
                                                        file_history.clear_under_dir(del_path);
                                                    }
                                                }
                                            }
                                            let output = if result.output.len() > max_tool_chars {
                                                let limit = max_tool_chars;
                                                let first = limit * 4 / 5;
                                                let last = limit - first;
                                                let mut first_end = first;
                                                while first_end > 0 && !result.output.is_char_boundary(first_end) {
                                                    first_end -= 1;
                                                }
                                                let last_start = result.output.len() - last;
                                                let mut last_end = last_start;
                                                while last_end < result.output.len() && !result.output.is_char_boundary(last_end) {
                                                    last_end += 1;
                                                }
                                                format!("{}\n\n... [OUTPUT TRUNCATED] ...\n\n{}",
                                                    &result.output[..first_end],
                                                    &result.output[last_end..])
                                            } else {
                                                result.output.clone()
                                            };
                                            (output, result.is_error, elapsed)
                                        }
                                        Err(_) => {
                                            let output = format!("Error: {} timed out after {:?}", tc.name, timeout);
                                            eprintln!("  [{}] timed out", tc.name);
                                            (output, true, elapsed)
                                        }
                                    };
                                    (entry.index, output.0, output.1, output.2, interrupted.load(std::sync::atomic::Ordering::SeqCst), tc.id, tc.name)
                                }));
                            }
                        }

                        // Check if interrupted during tool execution
                        if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                            return Ok("[Interrupted by user]".to_string());
                        }

                        // Collect results in order and record to transcript
                        let mut tool_results: Vec<(usize, String, bool, std::time::Duration, String, String)> = Vec::new();
                        for handle in handles {
                            if let Ok(result) = handle.await {
                                // result is (index, output, is_error, elapsed, was_interrupted, tool_id, tool_name)
                                if result.4 {
                                    // Tool was interrupted
                                    return Ok("[Interrupted by user]".to_string());
                                }
                                // Record tool_result to transcript (matching Go's WriteToolResult)
                                let _ = self.transcript.add_tool_result(
                                    result.5.clone(),  // tool_id
                                    result.6.clone(),  // tool_name
                                    result.1.clone(),  // output
                                );
                                tool_results.push((result.0, result.1, result.2, result.3, result.5, result.6));
                            }
                        }
                        tool_results.sort_by_key(|r| r.0);

                        // Display results (matching Go's ASCII format: [+] tool: preview / [x] tool (time): error)
                        for (_i, output, is_error, elapsed, _tool_id, tool_name) in &tool_results {
                            let elapsed_str = format!("{:.2}s", elapsed.as_secs_f64());
                            if *is_error {
                                let preview = limit_str(output, 150);
                                eprintln!("  [x] {} ({}): {}", tool_name, elapsed_str, preview);
                            } else {
                                // Print success result preview
                                let preview = tool_result_preview(tool_name, output);
                                if preview.is_empty() {
                                    eprintln!("  [+] {}", tool_name);
                                } else {
                                    eprintln!("  [+] {}: {}", tool_name, preview);
                                }
                            }
                        }

                        // Build proper content blocks for tool calls and results
                        // 1. Store assistant tool_use blocks
                        let tool_use_blocks: Vec<crate::context::ToolUseBlock> = tool_calls.iter().map(|tc| {
                            let params: HashMap<String, serde_json::Value> =
                                serde_json::from_str(&tc.arguments).unwrap_or_default();
                            crate::context::ToolUseBlock {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: params,
                            }
                        }).collect();

                        // Track skill usage for discovery system
                        for tc in &tool_calls {
                            if tc.name == "read_skill" {
                                let params: HashMap<String, serde_json::Value> =
                                    serde_json::from_str(&tc.arguments).unwrap_or_default();
                                if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                                    let mut tracker = self.skill_tracker.write().await;
                                    tracker.mark_read(name);
                                    tracker.mark_used(name);
                                }
                            }
                        }

                        // 2. Store tool_result blocks
                        let tool_result_blocks: Vec<crate::context::ToolResultBlock> = tool_calls.iter().enumerate().map(|(i, tc)| {
                            // Find the result for this tool call
                            let (output, is_error) = tool_results.iter()
                                .find(|(idx, _, _, _, _, _)| *idx == i)
                                .map(|(_, output, is_error, _, _, _)| (output.clone(), *is_error))
                                .unwrap_or_else(|| ("Error: no result".to_string(), true));

                            crate::context::ToolResultBlock {
                                tool_use_id: tc.id.clone(),
                                content: vec![crate::context::ToolResultContent::Text { text: output }],
                                is_error,
                            }
                        }).collect();

                        let mut ctx = self.context.write().await;
                        ctx.add_assistant_tool_calls(tool_use_blocks);
                        ctx.add_tool_results(tool_result_blocks);

                        // Check for interruption after tool execution
                        if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                            return Ok("[Interrupted by user]".to_string());
                        }

                    } else if !text.is_empty() {
                        // Final response
                        return Ok(text);
                    } else {
                        // No text and no tool calls — could be a thinking-only response
                        // This happens when the model uses extended thinking but hasn't produced text yet.
                        // Continue the loop to let the model produce more output.
                        consecutive_empty_responses += 1;
                        if consecutive_empty_responses >= MAX_EMPTY_RESPONSES {
                            eprintln!("[!] No actionable response after {} attempts, giving up", MAX_EMPTY_RESPONSES);
                            return Err(anyhow!("Model returned no actionable response {} times in a row", MAX_EMPTY_RESPONSES));
                        }
                        eprintln!("[!] No text/tool_use in response (attempt {}/{}), continuing...",
                            consecutive_empty_responses, MAX_EMPTY_RESPONSES);
                        // Inject hint to encourage actual output
                        let mut ctx = self.context.write().await;
                        ctx.add_user_message(
                            "Please continue and provide your response in text or use a tool.".to_string(),
                        );
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();

                    // Max output tokens hit — resume directly without truncation
                    if err_str.contains("maximum output length")
                        || err_str.contains("max_tokens")
                        || (err_str.contains("400") && err_str.contains("output")) {
                        max_output_tokens_retries += 1;
                        continue_reason = ContinueReason::MaxOutputTokens;

                        if max_output_tokens_retries <= MAX_OUTPUT_TOKENS_RETRIES {
                            eprintln!(
                                "[!] Output token limit hit (retry {}/{}), resuming directly...",
                                max_output_tokens_retries, MAX_OUTPUT_TOKENS_RETRIES
                            );
                            let mut ctx = self.context.write().await;
                            ctx.add_user_message(
                                "Output token limit reached. Resume directly — no apology, no recap. \
                                Pick up mid-thought and break remaining work into smaller pieces.".to_string(),
                            );
                            continue;
                        } else {
                            eprintln!("[!] Max output tokens recovery exhausted, falling back to truncation");
                            // Fall through to context recovery
                        }
                    }

                    // Model confusion - inject corrective message and retry
                    if err_str.contains("model confused") {
                        eprintln!("[!] Model confused, injecting corrective message...");
                        continue_reason = ContinueReason::ModelConfused;
                        let mut ctx = self.context.write().await;
                        ctx.add_user_message(
                            "ERROR: Your previous response was malformed. \
                            Do NOT output tool syntax as text. Use proper tool calls only.".to_string(),
                        );
                        last_transition = Transition::ToolsToText;
                        continue;
                    }

                    eprintln!("[!] Turn failed: {}", e);

                    // Detect context length error
                    if err_str.contains("context_length") || err_str.contains("prompt is too long") ||
                       err_str.contains("400") || err_str.contains("stream stalled") || err_str.contains("context canceled") {
                        context_errors += 1;
                        continue_reason = ContinueReason::PromptTooLong;

                        if context_errors > MAX_CONTEXT_RECOVERY {
                            eprintln!("[!] Context recovery exhausted after {} attempts, giving up", MAX_CONTEXT_RECOVERY);
                            return Ok("Error: Context overflow - unable to recover".to_string());
                        }

                        // Progressive recovery: try LLM compact first, then truncation
                        if context_errors == 1 && self.config.auto_compact_enabled {
                            // First attempt: try LLM-driven compaction
                            eprintln!("[!] Context overflow, attempting LLM compaction...");
                            let mut ctx = self.context.write().await;
                            let mut compactor = self.compactor.write().await;
                            let _ = compactor.compact(
                                &mut ctx,
                                &self.client,
                                &self.config.model,
                                &self.api_key,
                                &self.base_url,
                            ).await;
                        } else if context_errors <= 2 {
                            eprintln!("[!] Context overflow, truncating history (phase 1/2)...");
                            let mut ctx = self.context.write().await;
                            ctx.truncate_history();
                        } else {
                            eprintln!("[!] Context still full, aggressive truncation (phase 2/2)...");
                            let mut ctx = self.context.write().await;
                            ctx.aggressive_truncate_history();
                        }
                        continue;
                    }

                    // Check for consecutive stalls
                    consecutive_stalls += 1;
                    if consecutive_stalls >= 3 {
                        // If max turns reached, try for final summary
                        if turn >= self.max_turns {
                            eprintln!("\n[!] Max turns ({}) reached, requesting final answer...", self.max_turns);
                            return self.request_final_summary(system_prompt, tools).await;
                        }
                        return Err(anyhow!("Too many consecutive failures"));
                    }
                }
            }
        }

        // Max turns reached - try to get a final summary
        eprintln!("\n[!] Max turns ({}) reached, requesting final answer...", self.max_turns);
        self.request_final_summary(system_prompt, tools).await
    }

    /// Request a final summary when max turns is reached
    async fn request_final_summary(
        &self,
        system_prompt: &str,
        tools: &[serde_json::Value],
    ) -> Result<String> {
        // Add a hint message asking for summary
        {
            let mut ctx = self.context.write().await;
            ctx.add_user_message(
                "You have reached the maximum number of tool use turns. \
                Please provide a final summary based on the work done so far. \
                Do NOT call any more tools.".to_string(),
            );
        }

        // Get updated messages (async)
        let messages = self.entries_to_messages_async().await;

        // Try one more non-streaming call
        match self.call_api_non_streaming(system_prompt, &messages, tools).await {
            Ok((_, text)) => {
                if !text.is_empty() {
                    let mut ctx = self.context.write().await;
                    ctx.add_assistant_text(text.clone());
                    return Ok(text);
                }
            }
            Err(e) => {
                eprintln!("[!] Final summary call failed: {}", e);
            }
        }

        Ok("(max turns reached without a final response)".to_string())
    }

    /// Call the API with retry and fallback
    async fn call_with_retry_and_fallback(
        &self,
        system_prompt: &str,
        messages: &[serde_json::Value],
        tools: &[serde_json::Value],
        _last_transition: &Transition,
        _continue_reason: &ContinueReason,
    ) -> Result<(Vec<ToolCallInfo>, String)> {
        const MAX_RETRIES: usize = 10;
        const INITIAL_BACKOFF_MS: u64 = 2000;
        const MAX_BACKOFF_MS: u64 = 18000;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        // Always try streaming first — it's more reliable across different
        // API/proxy configurations. Non-streaming can hang on some proxies
        // that don't flush the response until the entire body is ready.
        for attempt in 0..MAX_RETRIES {
            // Check for interruption before each attempt
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(anyhow!("Request cancelled by user"));
            }

            match self.try_stream_once(system_prompt, messages, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let err_str = e.to_string();

                    // Check if interrupted
                    if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(anyhow!("Request cancelled by user"));
                    }

                    // Check if it's a transient error
                    if !is_transient_error(&err_str) {
                        eprintln!("[!] Non-transient streaming error: {}", e);
                        break;
                    }

                    if attempt < MAX_RETRIES - 1 {
                        eprintln!("[!] Streaming attempt {} failed (transient), retrying in {}ms: {}",
                            attempt + 1, backoff_ms, e);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    }
                }
            }
        }
        eprintln!("[!] Streaming failed after {} attempts, falling back to non-streaming", MAX_RETRIES);

        // Fall back to non-streaming with retries
        self.call_with_non_streaming_fallback(system_prompt, messages, tools).await
    }

    /// Try a single streaming attempt
    async fn try_stream_once(
        &self,
        system_prompt: &str,
        messages: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> Result<(Vec<ToolCallInfo>, String)> {
        let collect = CollectHandler::new();
        let term = TerminalHandler::new();
        let stall = Arc::new(StallDetector::new());

        let result = process_sse_events(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.config.model,
            16384,
            system_prompt,
            messages,
            tools,
            &collect,
            &term,
            &stall,
            self.interrupted.clone(),
        ).await;

        // Check if interrupted
        if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("Request cancelled by user"));
        }

        result?;

        let tool_calls = collect.tool_calls();
        let text = collect.full_response();
        let is_confused = collect.is_tool_use_as_text();

        if is_confused {
            return Err(anyhow!("model confused: echoed tool syntax as text"));
        }

        Ok((tool_calls, text))
    }

    /// Call non-streaming API with retry
    async fn call_with_non_streaming_fallback(
        &self,
        system_prompt: &str,
        messages: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> Result<(Vec<ToolCallInfo>, String)> {
        const MAX_RETRIES: usize = 10;
        const INITIAL_BACKOFF_MS: u64 = 2000;
        const MAX_BACKOFF_MS: u64 = 18000;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            // Check for interruption before each attempt
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(anyhow!("Request cancelled by user"));
            }

            match self.call_api_non_streaming(system_prompt, messages, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Check if interrupted
                    if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(anyhow!("Request cancelled by user"));
                    }

                    let err_str = e.to_string();
                    if !is_transient_error(&err_str) {
                        return Err(e);
                    }

                    if attempt < MAX_RETRIES - 1 {
                        eprintln!("[!] Non-streaming attempt {} failed (transient), retrying in {}ms: {}",
                            attempt + 1, backoff_ms, e);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                    }
                }
            }
        }

        Err(anyhow!("Non-streaming failed after {} attempts", MAX_RETRIES))
    }

    /// Call non-streaming API
    /// Returns (tool_calls, text)
    async fn call_api_non_streaming(
        &self,
        system_prompt: &str,
        messages: &[serde_json::Value],
        tools: &[serde_json::Value],
    ) -> Result<(Vec<ToolCallInfo>, String)> {
        let mut payload = serde_json::Map::new();
        payload.insert("model".to_string(), serde_json::json!(self.config.model));
        payload.insert("max_tokens".to_string(), serde_json::json!(16384));
        // Match Go SDK format: system is an array of text blocks
        payload.insert("system".to_string(), serde_json::json!([{"type": "text", "text": system_prompt}]));
        payload.insert("messages".to_string(), serde_json::json!(messages));
        if !tools.is_empty() {
            payload.insert("tools".to_string(), serde_json::json!(tools));
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        // Check for interruption before request
        if self.is_interrupted() {
            return Err(anyhow!("Request cancelled by user"));
        }

        let (cancel_token, cancel_handle) = self.interrupt_cancel_token();

        // Race HTTP send against cancellation
        let response = tokio::select! {
            resp = self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("Authorization", format!("Bearer {}", &self.api_key))
                .header("Content-Type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .json(&payload)
                .send() => {
                    match resp {
                        Ok(r) => r,
                        Err(e) => {
                            cancel_handle.abort();
                            if self.is_interrupted() {
                                return Err(anyhow!("Request cancelled by user"));
                            }
                            return Err(anyhow!("API request failed: {}", e));
                        }
                    }
                }
            _ = cancel_token.cancelled() => {
                cancel_handle.abort();
                return Err(anyhow!("Request cancelled by user"));
            }
        };

        cancel_handle.abort();

        if self.is_interrupted() {
            return Err(anyhow!("Request cancelled by user"));
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("API error {}: {}", status, body));
        }

        // Race JSON parsing against cancellation
        let (cancel_token2, cancel_handle2) = self.interrupt_cancel_token();
        let body_result = tokio::select! {
            val = response.json::<serde_json::Value>() => val,
            _ = cancel_token2.cancelled() => {
                cancel_handle2.abort();
                return Err(anyhow!("Request cancelled by user"));
            }
        };
        cancel_handle2.abort();

        let body = body_result.map_err(|e| anyhow!("Failed to parse response: {}", e))?;

        // Parse Anthropic format response: {"content": [{"type": "text"/"tool_use"/"thinking"}, ...]}
        let mut tool_calls = Vec::new();
        let mut text = String::new();
        let mut thinking = String::new();

        if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
            if content.is_empty() {
                eprintln!("[DEBUG] Response has empty content array. stop_reason={:?}, body={}",
                    body.get("stop_reason"),
                    serde_json::to_string(&body).unwrap_or_else(|_| "<failed to serialize>".to_string())
                );
            }
            for block in content {
                if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                    match block_type {
                        "text" => {
                            if let Some(text_val) = block.get("text").and_then(|t| t.as_str()) {
                                text.push_str(text_val);
                            }
                        }
                        "tool_use" => {
                            if let (Some(id), Some(name)) = (
                                block.get("id").and_then(|i| i.as_str()),
                                block.get("name").and_then(|n| n.as_str()),
                            ) {
                                let args = block.get("input").map(|i| i.to_string()).unwrap_or_default();

                                let summary = tool_arg_summary(&name, &args);
                                if !summary.is_empty() {
                                    eprintln!("  [{}]: {}", name, summary);
                                } else {
                                    eprintln!("  [{}]", name);
                                }

                                tool_calls.push(ToolCallInfo {
                                    id: id.to_string(),
                                    name: name.to_string(),
                                    arguments: args,
                                });
                            }
                        }
                        "thinking" => {
                            if let Some(th) = block.get("thinking").and_then(|t| t.as_str()) {
                                thinking.push_str(th);
                            }
                        }
                        _ => {
                            // Log unknown block types for debugging
                            eprintln!("[DEBUG] Unknown content block type: {}", block_type);
                        }
                    }
                }
            }
        } else {
            // No "content" field at all
            let body_preview = serde_json::to_string(&body)
                .unwrap_or_else(|_| "<failed to serialize>".to_string());
            eprintln!("[DEBUG] Missing 'content' field in response. stop_reason={:?}, body={}",
                body.get("stop_reason"),
                body_preview
            );
        }

        // Display thinking if present
        if !thinking.is_empty() {
            let preview = truncate_at(thinking.lines().next().unwrap_or(""), 120);
            eprintln!("\n[THINK] {}", preview);
        }

        // Debug: log when parsed result has no actionable content
        if tool_calls.is_empty() && text.is_empty() {
            let content_types: Vec<String> = body.get("content")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter()
                    .filter_map(|b| b.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
                    .collect())
                .unwrap_or_default();
            eprintln!("[DEBUG] Parsed response has no text/tool_use. content_types={}, stop_reason={:?}, thinking_len={}",
                content_types.join(","),
                body.get("stop_reason"),
                thinking.len()
            );
        }

        Ok((tool_calls, text))
    }

    /// Get tools schema for API
    fn get_tools_schema(&self) -> Vec<serde_json::Value> {
        let registry = self.registry.blocking_read();
        registry.all_tools()
            .iter()
            .map(|tool| {
                let mut schema = serde_json::Map::new();
                schema.insert("name".to_string(), serde_json::json!(tool.name()));
                schema.insert("description".to_string(), serde_json::json!(tool.description()));
                schema.insert("input_schema".to_string(), serde_json::json!(tool.input_schema()));
                serde_json::Value::Object(schema)
            })
            .collect()
    }

    /// Execute a tool call with permission checking
    #[allow(dead_code)]
    pub async fn execute_tool(&self, name: &str, params: HashMap<String, serde_json::Value>) -> Result<ToolResult> {
        let registry = self.registry.read().await;

        let tool = registry.get(name).ok_or_else(|| anyhow!("Tool not found: {}", name))?;

        // Check permissions
        if let Some(result) = self.gate.check(tool.as_ref(), params.clone()) {
            return Ok(result);
        }

        // Execute with 5-minute timeout (matching Go's toolTimeout)
        let tool_name = name.to_string();
        let timeout = std::time::Duration::from_secs(300);
        let start = std::time::Instant::now();

        // Since tools are sync, use spawn_blocking
        let tool_ref = tool.clone();
        let params_clone = params.clone();
        let result = tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || {
            tool_ref.execute(params_clone)
        })).await;

        let elapsed = start.elapsed();

        match result {
            Ok(Ok(tool_result)) => {
                eprintln!("[Tool: {}] completed in {:.2}s", tool_name, elapsed.as_secs_f64());
                Ok(tool_result)
            }
            Ok(Err(e)) => {
                eprintln!("[Tool: {}] join error after {:.2}s: {}", tool_name, elapsed.as_secs_f64(), e);
                Err(anyhow!("Tool execution panicked: {}", e))
            }
            Err(_) => {
                eprintln!("[Tool: {}] timed out after {:?}", tool_name, timeout);
                Ok(ToolResult {
                    output: format!("Error: {} timed out after {:?}", tool_name, timeout),
                    is_error: true,
                })
            }
        }
    }

    /// Truncate context when it gets too long
    #[allow(dead_code)]
    async fn truncate_context(&self) -> bool {
        // Use built-in truncation
        let mut ctx = self.context.write().await;
        let len = ctx.len();

        if len <= 4 {
            return false;
        }

        // Try progressive truncation
        if len > 20 {
            ctx.aggressive_truncate_history();
        } else {
            ctx.truncate_history();
        }

        eprintln!("[!] Context truncated from {} to {} entries", len, ctx.len());
        true
    }

    /// Truncate long tool output (keep first 80% and last 20%)
    #[allow(dead_code)]
    fn truncate_output(&self, output: &str, limit: usize) -> String {
        let limit = if limit == 0 { 8192 } else { limit };
        if output.len() <= limit {
            return output.to_string();
        }
        let first = limit * 4 / 5;
        let last = limit - first;
        // Safe char boundary truncation
        let mut first_end = first;
        while first_end > 0 && !output.is_char_boundary(first_end) {
            first_end -= 1;
        }
        let last_start = output.len() - last;
        let mut last_end = last_start;
        while last_end < output.len() && !output.is_char_boundary(last_end) {
            last_end += 1;
        }
        format!("{}\n\n... [OUTPUT TRUNCATED] ...\n\n{}",
            &output[..first_end],
            &output[last_end..])
    }

    /// Close releases resources (MCP servers, etc.)
    pub fn close(&self) {
        if let Some(ref mgr) = self.config.mcp_manager {
            mgr.stop_all();
        }
    }

    /// Get the transcript filename for resume hint
    pub fn transcript_filename(&self) -> &str {
        self.transcript.filename()
    }

    /// Set interrupted flag (from Ctrl+C handler)
    pub fn set_interrupted(&self, value: bool) {
        self.interrupted.store(value, std::sync::atomic::Ordering::SeqCst);
    }

    /// Check if interrupted
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Create a CancellationToken that gets cancelled when the interrupted flag is set.
    /// Polls every 100ms. Drop the join handle to stop polling.
    fn interrupt_cancel_token(&self) -> (CancellationToken, tokio::task::JoinHandle<()>) {
        let token = CancellationToken::new();
        let cloned = token.clone();
        let interrupted = self.interrupted.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                    cloned.cancel();
                    return;
                }
            }
        });
        (token, handle)
    }

    /// Get a clone of the interrupted flag for use in Ctrl+C handler
    pub fn interrupted_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.interrupted.clone()
    }
}

/// Check if an error is transient (retryable)
pub fn is_transient_error(err_str: &str) -> bool {
    let patterns = [
        "connection",
        "timeout",
        "timed out",
        "network",
        "rate limit",
        "429",
        "500",
        "502",
        "503",
        "504",
        "upstream",
        "reset",
        "broken pipe",
        "temporary",
        "transient",
    ];

    let err_lower = err_str.to_lowercase();
    patterns.iter().any(|p| err_lower.contains(p))
}

/// Limit a string to max chars, adding "..." if truncated
pub fn limit_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Truncate at char boundary
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Generate a summary of tool arguments for display
pub fn tool_arg_summary(tool_name: &str, args_json: &str) -> String {
    let input: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_str(args_json).unwrap_or_default();

    match tool_name {
        "read_file" | "write_file" | "edit_file" | "multi_edit" | "fileops" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    return path.to_string();
                }
            }
        }
        "list_dir" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    return path.to_string();
                }
            }
            return ".".to_string();
        }
        "exec" | "terminal" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                if cmd.len() > 120 {
                    return format!("{}...", &cmd[..120.min(cmd.len())]);
                }
                return cmd.to_string();
            }
        }
        "grep" => {
            if let (Some(pattern), Some(path)) = (
                input.get("pattern").and_then(|v| v.as_str()),
                input.get("path").and_then(|v| v.as_str()),
            ) {
                return format!("\"{}\" in {}", pattern, path);
            }
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                return pattern.to_string();
            }
        }
        "glob" => {
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                return pattern.to_string();
            }
        }
        "system" => {
            if let Some(op) = input.get("operation").and_then(|v| v.as_str()) {
                return op.to_string();
            }
        }
        "git" => {
            if let Some(args) = input.get("args").and_then(|v| v.as_str()) {
                return format!("git {}", args);
            }
        }
        "web_search" | "exa_search" => {
            if let Some(query) = input.get("query").and_then(|v| v.as_str()) {
                return query.to_string();
            }
        }
        "web_fetch" => {
            if let Some(url) = input.get("url").and_then(|v| v.as_str()) {
                return url.to_string();
            }
        }
        _ => {}
    }

    // Fallback: compact format
    let parts: Vec<String> = input
        .iter()
        .filter_map(|(k, v)| {
            let v_str = match v {
                serde_json::Value::String(s) if !s.is_empty() => {
                    if s.len() > 80 {
                        format!("{}...", &s[..80])
                    } else {
                        s.clone()
                    }
                }
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            Some(format!("{}={}", k, v_str))
        })
        .take(3)
        .collect();

    parts.join(", ")
}

/// Extract the most relevant part of a tool result for display (matching Go's toolResultPreview)
pub fn tool_result_preview(tool_name: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();

    match tool_name {
        "exec" => {
            let cleaned = clean_exec_output(output);
            if cleaned.is_empty() {
                return "(no output)".to_string();
            }
            return limit_str(&cleaned, 120);
        }
        "read_file" => {
            if let Some(first) = lines.first() {
                if first.contains("File:") {
                    return first.to_string();
                }
            }
        }
        "write_file" | "edit_file" | "multi_edit" => {
            if output.contains('/') || output.contains('\\') {
                for line in &lines {
                    if line.contains('.') && (line.contains('/') || line.contains('\\')) {
                        return line.to_string();
                    }
                }
            }
        }
        "list_dir" => {
            return limit_str(output, 100);
        }
        _ => {}
    }

    // Fallback: first line, truncated
    if let Some(first) = lines.first() {
        limit_str(first, 120)
    } else {
        String::new()
    }
}

/// Strip STDOUT/STDERR headers and return the actual content
pub fn clean_exec_output(output: &str) -> String {
    let mut cleaned = output.strip_prefix("STDOUT:\n").unwrap_or(output);
    cleaned = cleaned.strip_prefix("STDERR:\n").unwrap_or(cleaned);
    cleaned = cleaned.trim_end();

    // If both stdout and stderr are present, prefer stdout
    if output.starts_with("STDOUT:\n") && output.contains("\nSTDERR:\n") {
        if let Some(pos) = output.find("\nSTDERR:\n") {
            let stdout_part = output["STDOUT:\n".len()..pos].trim();
            let stderr_part = output[pos + "\nSTDERR:\n".len()..].trim();
            if !stdout_part.is_empty() {
                return stdout_part.to_string();
            }
            return stderr_part.to_string();
        }
    }

    cleaned.to_string()
}
