use crate::compact::Compactor;
use crate::config::Config;
use crate::context::{ConversationContext, ConversationEntry, MessageContent, ToolUseBlock, ToolResultBlock, ToolResultContent};
use crate::filehistory::FileHistory;
use crate::permissions::PermissionGate;
use crate::skills::SkillTracker;
use crate::streaming::{CollectHandler, TerminalHandler, StallDetector, process_sse_events, ToolCallInfo};
use crate::prompt_caching::{apply_prompt_caching, cache_system_prompt};
use crate::error_types::classify_error;
use crate::rate_limit::RateLimitState;
use crate::tools::{expand_path, truncate_at, ToolResult, Registry};
use crate::transcript::{Transcript, TranscriptEntry, TYPE_USER, TYPE_ASSISTANT, TYPE_TOOL_USE, TYPE_TOOL_RESULT, TYPE_SYSTEM, TYPE_ERROR, TYPE_COMPACT, TYPE_SUMMARY};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Build a reqwest client with API key headers.
/// Returns None if the API key contains invalid header characters.
fn build_http_client(api_key: &str) -> Option<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(key_val) = api_key.parse::<reqwest::header::HeaderValue>() {
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            key_val,
        );
    } else {
        eprintln!("[WARN] API key contains invalid characters for HTTP header");
    }
    let bearer = format!("Bearer {}", api_key);
    if let Ok(bearer_val) = bearer.parse::<reqwest::header::HeaderValue>() {
        headers.insert(
            reqwest::header::AUTHORIZATION,
            bearer_val,
        );
    } else {
        eprintln!("[WARN] Bearer token contains invalid characters for HTTP header");
    }
    reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .default_headers(headers)
        .build()
        .ok()
}

/// Tracks iteration budget for the agent loop, allowing refunds for final answers
/// and a grace call for graceful termination.
pub struct IterationBudget {
    max: usize,
    consumed: u32,
    grace_called: bool,
}

impl IterationBudget {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            consumed: 0,
            grace_called: false,
        }
    }

    /// Consume one unit from the budget. Returns false when exhausted (and grace not yet called).
    pub fn consume(&mut self) -> bool {
        if (self.consumed as usize) < self.max {
            self.consumed += 1;
            true
        } else if !self.grace_called {
            // Already exhausted, let grace_call decide
            false
        } else {
            false
        }
    }

    /// Give one unit back -- used when the model produces a text-only final answer
    /// (no tool calls), since it shouldn't count against the budget.
    pub fn refund(&mut self) {
        if self.consumed > 0 {
            self.consumed -= 1;
        }
    }

    /// Attempt a grace call -- allows one extra API call after exhaustion for the
    /// model to produce a final answer. Returns true if the grace call is granted.
    pub fn grace_call(&mut self) -> bool {
        if !self.grace_called {
            self.grace_called = true;
            self.consumed += 1;
            true
        } else {
            false
        }
    }

    /// Returns remaining turns (for display purposes).
    pub fn remaining(&self) -> usize {
        if self.grace_called {
            0
        } else {
            self.max.saturating_sub(self.consumed as usize)
        }
    }
}

/// Continue reason tracks why the agent loop is continuing (inspired by Claude Code's 7 continue reasons)
#[derive(Debug, Clone, PartialEq, Default)]
enum ContinueReason {
    #[default]
    None,
    NextTurn,
    PromptTooLong,
    MaxOutputTokens,
    ModelConfused,
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
    pub(crate) context: Arc<RwLock<ConversationContext>>,
    client: reqwest::Client,
    pub use_stream: bool,
    /// Tracks whether the LAST API call actually used streaming (set by the async agent loop).
    /// Differs from `use_stream` which is the intended mode — when streaming fails and falls
    /// back to non-streaming, this field is set to false so main.rs knows to print the result.
    pub last_call_was_streaming: std::sync::atomic::AtomicBool,
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
    /// Rate limit state parsed from API response headers
    rate_limit_state: RateLimitState,
    /// Optional custom system prompt (used by sub-agents)
    custom_system_prompt: Option<String>,
    /// Counter for tool uses (for sub-agent reporting)
    tools_used: std::sync::atomic::AtomicUsize,
}

impl AgentLoop {
    pub fn new(config: Config, registry: Registry, use_stream: bool) -> Result<Self> {
        let api_key = config.api_key.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
                .unwrap_or_default()
        });

        if api_key.is_empty() {
            return Err(anyhow!("ANTHROPIC_API_KEY environment variable is not set (or use --api-key)"));
        }

        let base_url = config.base_url.clone().unwrap_or_else(|| {
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".to_string())
        });

        let client = build_http_client(&api_key)
            .ok_or_else(|| anyhow!("failed to build HTTP client (invalid API key?)"))?;

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
        let session_memory = config.session_memory.clone();
        let compactor = RwLock::new(
            Compactor::new()
                .with_threshold(config.auto_compact_threshold)
                .with_buffer(config.auto_compact_buffer)
                .with_max_tokens(crate::compact::model_context_window(&gate.config.model))
                .with_session_memory(session_memory)
                .with_reactive_threshold(config.reactive_compact_threshold)
        );

        // Create multi-thread tokio runtime for this agent
        // This properly handles spawn_blocking calls from reqwest
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow!("failed to create tokio runtime: {}", e))?;

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
            transcript,
            compactor,
            file_history,
            rt,
            interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rate_limit_state: RateLimitState::default(),
            skill_tracker: Arc::new(RwLock::new(SkillTracker::new())),
            last_call_was_streaming: std::sync::atomic::AtomicBool::new(false),
            custom_system_prompt: None,
            tools_used: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Create agent from existing transcript (resume session)
    pub fn from_transcript(
        config: Config,
        registry: Registry,
        use_stream: bool,
        transcript_path: &Path,
        continue_transcript: bool,
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

        let client = build_http_client(&api_key)
            .ok_or_else(|| anyhow!("failed to build HTTP client (invalid API key?)"))?;

        let max_turns = config.max_turns;
        let file_history = config.file_history.clone().unwrap_or_else(|| Arc::new(FileHistory::new()));
        let gate = PermissionGate::new(config.clone());

        // Read transcript and rebuild context
        let transcript = Transcript::new(&transcript_path.to_path_buf());
        let entries = transcript.read_all()
            .map_err(|e| anyhow!("Failed to read transcript: {}", e))?;

        let mut context = Self::rebuild_context_from_transcript(&entries, config.clone());

        // Create transcript writer: continue original file or start new session
        let new_transcript = if continue_transcript {
            Transcript::new(&transcript_path.to_path_buf())
        } else {
            let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            let transcript_dir = PathBuf::from(".claude").join("transcripts");
            let _ = std::fs::create_dir_all(&transcript_dir);
            let path = transcript_dir.join(format!("{}.jsonl", session_id));
            let t = Transcript::new(&path);
            let _ = t.add_system(format!("model={}, mode={}", config.model, config.permission_mode));
            let _ = t.add_user(format!(
                "Resumed from {} ({} messages restored)",
                transcript_path.display(),
                entries.len()
            ));
            t
        };

        let session_memory = config.session_memory.clone();
        let mut compactor = Compactor::new()
            .with_threshold(config.auto_compact_threshold)
            .with_buffer(config.auto_compact_buffer)
            .with_max_tokens(crate::compact::model_context_window(&config.model))
            .with_session_memory(session_memory)
            .with_reactive_threshold(config.reactive_compact_threshold);

        // Preflight compression for resumed sessions: if the restored context
        // is too large (>100K estimated tokens), compact it synchronously
        // before starting the agent loop.
        const PREFLIGHT_TOKEN_THRESHOLD: usize = 100_000;
        const PREFLIGHT_MAX_ATTEMPTS: usize = 3;
        for _ in 0..PREFLIGHT_MAX_ATTEMPTS {
            let estimated = crate::compact::estimate_total_tokens(context.messages());
            if estimated <= PREFLIGHT_TOKEN_THRESHOLD {
                break;
            }
            let stats = compactor.compact_preflight(&mut context);
            if stats.phase == crate::compact::CompactPhase::None {
                break;
            }
            eprintln!(
                "[preflight-compact] {} -> {} entries, ~{} tokens saved",
                stats.entries_before, stats.entries_after, stats.estimated_tokens_saved
            );
        }

        let compactor = RwLock::new(compactor);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow!("failed to create tokio runtime: {}", e))?;

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
            rate_limit_state: RateLimitState::default(),
            skill_tracker: Arc::new(RwLock::new(SkillTracker::new())),
            last_call_was_streaming: std::sync::atomic::AtomicBool::new(false),
            custom_system_prompt: None,
            tools_used: std::sync::atomic::AtomicUsize::new(0),
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
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(std::mem::take(&mut pending_tool_results));
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
                    context.add_user_message(entry.content.clone());
                }
                TYPE_ASSISTANT => {
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(std::mem::take(&mut pending_tool_results));
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
                    if !entry.content.is_empty() {
                        context.add_assistant_text(entry.content.clone());
                    }
                }
                TYPE_TOOL_USE => {
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
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
                    if let Some(id) = &entry.tool_id {
                        pending_tool_results.push(ToolResultBlock {
                            tool_use_id: id.clone(),
                            content: vec![ToolResultContent::Text { text: entry.content.clone() }],
                            is_error: false,
                        });
                    }
                }
                TYPE_SYSTEM | TYPE_ERROR => {
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(std::mem::take(&mut pending_tool_results));
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
                    // Skip system and error entries
                }
                TYPE_COMPACT => {
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(std::mem::take(&mut pending_tool_results));
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
                    let pre_tokens = entry.content
                        .split_whitespace()
                        .find(|s| s.chars().all(|c| c.is_ascii_digit()))
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(0);
                    context.add_compact_boundary(crate::context::CompactTrigger::Auto, pre_tokens);
                }
                TYPE_SUMMARY => {
                    if !pending_tool_results.is_empty() {
                        context.add_tool_results(std::mem::take(&mut pending_tool_results));
                    }
                    if !pending_tool_uses.is_empty() {
                        context.add_assistant_tool_calls(std::mem::take(&mut pending_tool_uses));
                    }
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

        // Fix any inconsistencies from interrupted sessions:
        // - Orphaned tool_use without matching tool_result
        // - Orphaned tool_result without matching tool_use
        // - Consecutive same-role messages (breaks Anthropic API)
        context.validate_tool_pairing();
        context.fix_role_alternation();

        context
    }

    /// Process a user message through the agent loop
    pub fn run(&self, user_message: &str) -> String {
        // Clear interrupted flag at start of new request
        self.interrupted.store(false, std::sync::atomic::Ordering::SeqCst);

        // Expand @ context references (e.g., @file:main.go, @diff)
        let processed_msg = {
            let cwd = std::env::current_dir().unwrap_or_default();
            let est_tokens: usize = 200000; // use full context window size
            let result = crate::context_references::preprocess_context_references(
                user_message, &cwd, est_tokens,
            );
            if result.expanded && !result.blocked {
                result.message
            } else {
                if !result.warnings.is_empty() {
                    for w in &result.warnings {
                        eprintln!("[WARN] {}", w);
                    }
                }
                user_message.to_string()
            }
        };

        // Add user message to context
        {
            let mut ctx = self.context.blocking_write();
            ctx.add_user_message(processed_msg);
        }

        // Log user message to transcript
        let _ = self.transcript.add_user(user_message.to_string());

        // Refresh skills if files changed
        // Note: skill_loader is behind &self, so we skip refresh_if_changed here
        // (it requires &mut self on Loader). Skills are refreshed at startup.

        // Build system prompt — use custom one if set (sub-agents), otherwise build from registry
        let system_prompt = if let Some(ref custom) = self.custom_system_prompt {
            custom.clone()
        } else {
            let tracker = self.skill_tracker.blocking_read();
            let session_memory = self.config.session_memory.as_ref().map(|sm| sm.as_ref());
            let prompt = crate::config::build_system_prompt(
                &*self.registry.blocking_read(),
                &self.config.permission_mode,
                &self.config.project_dir,
                &self.config.model,
                self.config.skill_loader.as_ref(),
                Some(&tracker),
                session_memory,
            );
            drop(tracker);
            prompt
        };

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
                    MessageContent::Attachment(text) => {
                        ("user".to_string(),
                        vec![serde_json::json!({"type": "text", "text": text})])
                    }
                    MessageContent::CompactBoundary { .. } => {
                        // Skip compact boundaries in API messages -- they're metadata only
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
        let mut budget = IterationBudget::new(self.max_turns);
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

            if !budget.consume() {
                break;
            }

            // Run compaction before API call (matching Go's CompactContext)
            // Uses async LLM-driven compaction when threshold is reached

            // Phase 0: Micro-compact — clear old tool results every turn (cheap, no LLM call)
            if self.config.micro_compact_enabled {
                let keep_recent = self.config.micro_compact_keep_recent;
                let placeholder = self.config.micro_compact_placeholder.clone();
                let mut ctx = self.context.write().await;
                let cleared = ctx.micro_compact_entries(keep_recent, &placeholder);
                if cleared > 0 {
                    eprintln!("[micro-compact] Cleared {} old tool results", cleared);
                }
            }

            if self.config.auto_compact_enabled {
                // Feature 3: Reactive compact — trigger compaction when token count spikes
                // Check before regular compaction to handle sudden context growth
                let reactive_triggered = {
                    let ctx = self.context.read().await;
                    let current_tokens = crate::compact::estimate_total_tokens(ctx.messages());
                    let mut compactor = self.compactor.write().await;
                    compactor.should_reactive_compact(current_tokens)
                };

                // If reactive compact triggered, force the compaction threshold check
                // by temporarily lowering the threshold to ensure compaction runs
                if reactive_triggered {
                    let mut ctx = self.context.write().await;
                    let mut compactor = self.compactor.write().await;
                    // Save original threshold, force compaction, then restore
                    let saved_threshold = compactor.get_compact_threshold();
                    compactor.set_compact_threshold(0.0); // Force should_compact to return true
                    let stats = compactor.compact(
                        &mut ctx,
                        &self.client,
                        &self.config.model,
                        &self.api_key,
                        &self.base_url,
                    ).await;
                    compactor.set_compact_threshold(saved_threshold);
                    if stats.phase != crate::compact::CompactPhase::None {
                        eprintln!("[reactive-compact] Triggered: {} -> {} entries, ~{} tokens saved",
                            stats.entries_before, stats.entries_after, stats.estimated_tokens_saved);
                        let _ = self.transcript.add_compact(
                            format!("reactive-{:?}", stats.phase),
                            stats.estimated_tokens_saved,
                        );
                        let recovered_paths = self.post_compact_recovery().await;
                        let snip_count = self.config.post_compact_history_snip_count;
                        ctx.add_history_snip(snip_count, &recovered_paths);
                    }
                } else {
                    // Regular auto-compaction (token threshold based)
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

                            // Phase 2: Post-compact recovery — re-inject critical context
                            let recovered_paths = self.post_compact_recovery().await;

                            // Phase 3: History snip — preserve recent messages verbatim
                            let snip_count = self.config.post_compact_history_snip_count;
                            ctx.add_history_snip(snip_count, &recovered_paths);
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
            }

            // Rebuild messages from current context state (includes tool results)
            let messages = self.entries_to_messages_async().await;

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
                            timeout_secs: u64,
                            denied: bool,
                            err_text: String,
                        }

                        let mut entries: Vec<ToolCallEntry> = Vec::new();
                        for (i, tc) in tool_calls.iter().enumerate() {
                            let params: HashMap<String, serde_json::Value> =
                                serde_json::from_str(&tc.arguments).unwrap_or_default();

                            // Agent-controlled timeout -- default 600s
                            let timeout_secs = params.get("timeout")
                                .and_then(|v| v.as_i64())
                                .map(|v| v.max(1).min(600) as u64)
                                .unwrap_or(600);

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
                                timeout_secs,
                                denied,
                                err_text,
                            });
                        }

                        // Print all tool calls upfront (matching Go: "  [exec]: cmd" for exec, "  [tool] args" for others)
                        for entry in &entries {
                            let args_json = serde_json::to_string(&entry.params).unwrap_or_default();
                            let input_preview = tool_arg_summary(&entry.tc.name, &args_json);
                            if entry.tc.name == "exec" {
                                eprintln!("  [{}]: {}", entry.tc.name, input_preview);
                            } else {
                                eprintln!("  [{}] {}", entry.tc.name, input_preview);
                            }
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
                                let timeout_secs = entry.timeout_secs;
                                let max_tool_chars = self.max_tool_chars;
                                let interrupted = self.interrupted.clone();

                                // Clone what we need for the spawned task
                                let registry_clone = self.registry.clone();
                                let file_history = self.file_history.clone();

                                handles.push(tokio::task::spawn(async move {
                                    let start = std::time::Instant::now();
                                    let tool_timeout = Duration::from_secs(timeout_secs);

                                    let tool_name = tc.name.clone();

                                    // Capture path for post-execution snapshot before params is moved
                                    // Also captures read_file path for mark_file_read tracking
                                    let snapshot_path = if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" || tool_name == "read_file" {
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
                                    // No description prefix -- the post-execution snapshot carries the operation description
                                    if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                        if let Some(path) = snapshot_path.as_ref() {
                                            let _ = file_history.snapshot(path);
                                        }
                                    }

                                    // Clone snapshot_path for use both inside and after spawn_blocking
                                    let snapshot_path_post = snapshot_path.clone();

                                    // Execute tool on blocking thread pool -- ensures synchronous
                                    // syscalls don't block the async runtime's core threads.
                                    let tool_result = tokio::time::timeout(tool_timeout, tokio::task::spawn_blocking(move || {
                                        let registry = registry_clone.blocking_read();

                                        // Path traversal protection: file tools must stay within project directory
        let path_tools = ["read_file", "write_file", "edit_file", "multi_edit", "fileops", "list_dir"];
        if path_tools.contains(&tool_name.as_str()) {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    if let Err(e) = crate::tools::is_path_allowed(path) {
                        return ToolResult::error(e);
                    }
                }
            }
            // Also check destination for fileops (ln, cp, mv)
            if tool_name == "fileops" {
                if let Some(dest) = params.get("destination").and_then(|v| v.as_str()) {
                    if !dest.is_empty() {
                        if let Err(e) = crate::tools::is_path_allowed(dest) {
                            return ToolResult::error(e);
                        }
                    }
                }
            }
        }

        // Read-before-edit enforcement: file must be read and unmodified
                                        if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                            if let Some(path) = &snapshot_path {
                                                if let Err(msg) = registry.check_file_stale(&path.to_string_lossy()) {
                                                    return ToolResult::error(msg);
                                                }
                                            }
                                        }

                                        let tool = registry.get(&tool_name);
                                        match tool {
                                            Some(t) => {
                                                // Validate required parameters (matching Go's ValidateParams)
                                                if let Some(val_err) = crate::tools::validate_params(t.as_ref(), &params) {
                                                    return val_err;
                                                }
                                                // Coerce argument types to match schema (LLMs often pass wrong types)
                                                let schema = t.input_schema();
                                                let mut coerced = params;
                                                let coercion_result = crate::tools::coercion::coerce_arguments(&schema, &mut coerced);
                                                if !coercion_result.warnings.is_empty() {
                                                    for w in &coercion_result.warnings {
                                                        eprintln!("[coercion] {}", w);
                                                    }
                                                }
                                                let result = t.execute(coerced);
                                                // Mark file as read after successful read_file
                                                if !result.is_error && tool_name == "read_file" {
                                                    if let Some(path) = &snapshot_path {
                                                        registry.mark_file_read(&path.to_string_lossy());
                                                    }
                                                }
                                                result
                                            }
                                            None => ToolResult::error(format!("Tool not found: {}", tool_name)),
                                        }
                                    })).await;

                                    let elapsed = start.elapsed();
                                    let output = match tool_result {
                                        Ok(Ok(result)) => {
                                            // Post-execution snapshot: captures new files and final state
                                            if !result.is_error {
                                                if let Some(path) = snapshot_path_post.as_ref() {
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
                                        Ok(Err(e)) => {
                                            eprintln!("  [{}] panicked: {}", tc.name, e);
                                            let output = format!("Error: tool execution panicked: {}", e);
                                            (output, true, elapsed)
                                        }
                                        Err(_) => {
                                            let output = format!("Error: {} timed out after {:?}", tc.name, tool_timeout);
                                            eprintln!("  [{}] timed out after {:.1}s", tc.name, elapsed.as_secs_f64());
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
                                if tool_name == "exec" {
                                    // For exec, show result with tool name prefix (matching Go)
                                    eprintln!("  [+] {}: {}", tool_name, preview);
                                } else if preview.is_empty() {
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

                        // Update prev_turn_tokens for reactive compact detection
                        let current_tokens = crate::compact::estimate_total_tokens(ctx.messages());
                        drop(ctx);
                        {
                            let mut compactor = self.compactor.write().await;
                            compactor.update_prev_turn_tokens(current_tokens);
                        }

                        // Check for interruption after tool execution
                        if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                            return Ok("[Interrupted by user]".to_string());
                        }

                    } else if !text.is_empty() {
                        // Final response -- text-only (no tool calls), refund the budget
                        budget.refund();
                        return Ok(text);
                    } else {
                        // No text and no tool calls -- could be a thinking-only response
                        // This happens when the model uses extended thinking but hasn't produced text yet.
                        // Continue the loop to let the model produce more output.
                        consecutive_empty_responses += 1;
                        if consecutive_empty_responses >= MAX_EMPTY_RESPONSES {
                            eprintln!("[!] No actionable response after {} attempts, giving up", MAX_EMPTY_RESPONSES);
                            return Err(anyhow!("Model returned no actionable response {} times in a row", MAX_EMPTY_RESPONSES));
                        }
                        // When budget is exhausted but text is empty, grant a grace call
                        // so the model gets one more chance to produce a final answer
                        if !budget.consume() {
                            if budget.grace_call() {
                                eprintln!("\n[!] Budget exhausted, granting grace call for final answer...");
                            } else {
                                eprintln!("[!] No text/tool_use in response (attempt {}/{}), giving up...",
                                    consecutive_empty_responses, MAX_EMPTY_RESPONSES);
                                return Err(anyhow!("Model returned no actionable response {} times in a row", MAX_EMPTY_RESPONSES));
                            }
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

                    // Max output tokens hit -- resume directly without truncation
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
                                "Output token limit reached. Resume directly -- no apology, no recap. \
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

                    // 2013 error: tool_result doesn't follow tool_call -- repair pairing before retry
                    if err_str.contains("2013") || err_str.contains("tool call result does not follow tool call") {
                        eprintln!("[!] Tool pairing error (2013), repairing context...");
                        let mut ctx = self.context.write().await;
                        ctx.validate_tool_pairing();
                        ctx.fix_role_alternation();
                        continue;
                    }

                    // Truncated tool arguments - model was cut off mid-tool-call
                    if err_str.contains("truncated") || err_str.contains("incomplete JSON") {
                        eprintln!("[!] Tool arguments were truncated, injecting corrective hint...");
                        continue_reason = ContinueReason::MaxOutputTokens;
                        let mut ctx = self.context.write().await;
                        ctx.add_user_message(
                            "ERROR: Your tool call arguments was cut off due to length limits. \
                            Do NOT repeat the truncated tool call. \
                            If you need to make multiple tool calls, make them one at a time with shorter arguments.".to_string(),
                        );
                        continue;
                    }

                    eprintln!("[!] Turn failed: {}", e);

                    // Detect context length error
                    if err_str.contains("context_length") || err_str.contains("prompt is too long") ||
                       err_str.contains("400") || err_str.contains("stream stalled") {
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
                        // If budget exhausted, try for final summary
                        if budget.remaining() == 0 {
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
        _tools: &[serde_json::Value],  // ignore tools - force text-only response
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

        // Force text-only response by passing empty tools list
        let empty_tools: Vec<serde_json::Value> = vec![];

        // Try one more non-streaming call
        match self.call_api_non_streaming(system_prompt, &messages, &empty_tools).await {
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

        // Respect use_stream flag: if disabled, skip streaming entirely
        if !self.use_stream {
            return self.call_with_non_streaming_fallback(system_prompt, messages, tools).await;
        }

        // Always try streaming first -- it's more reliable across different
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

                    // 2013 error: tool pairing broken -- repair and rebuild messages before retry
                    if err_str.contains("2013") || err_str.contains("tool call result does not follow tool call") {
                        eprintln!("[!] Tool pairing error (2013) during stream, repairing context and rebuilding messages...");
                        {
                            let mut ctx = self.context.write().await;
                            ctx.validate_tool_pairing();
                            ctx.fix_role_alternation();
                        }
                        // Rebuild messages from repaired context so the fix takes effect on retry
                        let rebuilt = self.entries_to_messages_async().await;
                        // Retry with repaired messages instead of falling back to non-streaming
                        match self.try_stream_once(system_prompt, &rebuilt, tools).await {
                            Ok(result) => return Ok(result),
                            Err(e2) => {
                                eprintln!("[!] Stream still failed after 2013 repair: {}", e2);
                                // Fall through to non-streaming with rebuilt messages
                                return self.call_with_non_streaming_fallback(system_prompt, &rebuilt, tools).await;
                            }
                        }
                    }

                    // Check if it's a transient error using rich classification
                    let classification = classify_error(&err_str, 0, 0);
                    if !classification.retryable {
                        eprintln!("[!] Non-transient streaming error ({}): {}", classification.error.category(), e);
                        break;
                    }

                    // Use recovery hints for logging
                    if classification.hints.compress {
                        eprintln!("[!] Hint: compress context before retry");
                    }
                    if classification.hints.fallback {
                        eprintln!("[!] Hint: consider model/provider fallback");
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

        // Apply prompt caching to message array (system_and_3 strategy)
        let mut cached_messages = messages.to_vec();
        apply_prompt_caching(&mut cached_messages, "5m");

        let result = process_sse_events(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.config.model,
            16384,
            system_prompt,
            &cached_messages,
            tools,
            &collect,
            &term,
            &stall,
            self.interrupted.clone(),
            &self.rate_limit_state,
        ).await;

        // Check if interrupted
        if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("Request cancelled by user"));
        }

        let stream_result = result?;
        let tool_calls = stream_result.tool_calls;
        let text = stream_result.text;
        let is_confused = collect.is_tool_use_as_text();

        // If stream returned partial results (retry exhausted), signal transient
        // error so the outer retry loop in call_with_retry_and_fallback retries
        // with a fresh connection. This prevents the agent loop from acting on
        // incomplete tool calls.
        if !stream_result.completed {
            // When text was already streamed to the terminal before the failure,
            // return the text as-is (it was already shown) to avoid duplication.
            if stream_result.text_already_streamed {
                eprintln!("[!] Stream failed after text was already delivered; returning partial text");
                return Ok((tool_calls, text));
            }
            eprintln!("[!] Stream returned partial results, retrying with fresh connection");
            return Err(anyhow!("stream returned partial result (partial delivery)"));
        }

        if is_confused {
            return Err(anyhow!("model confused: echoed tool syntax as text"));
        }

        // Check for truncated tool arguments (matching Hermes truncated arg detection).
        // If tool args are incomplete JSON, the model was cut off mid-tool-call.
        // Return error so the agent loop can retry with corrective hint.
        if collect.has_truncated_tool_args() {
            let names: Vec<_> = tool_calls.iter().map(|t| t.name.clone()).collect();
            eprintln!("[!] Tool arguments truncated: {:?}, injecting corrective hint", names);
            return Err(anyhow!("tool arguments were truncated (incomplete JSON)"));
        }

        // Log finish_reason for debugging
        if let Some(reason) = stream_result.finish_reason {
            eprintln!("[DEBUG] Stream finish_reason={}", reason);
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
        let mut current_messages: Vec<serde_json::Value> = messages.to_vec();

        for attempt in 0..MAX_RETRIES {
            // Check for interruption before each attempt
            if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(anyhow!("Request cancelled by user"));
            }

            match self.call_api_non_streaming(system_prompt, &current_messages, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Check if interrupted
                    if self.interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(anyhow!("Request cancelled by user"));
                    }

                    let err_str = e.to_string();

                    // 2013 error: tool pairing broken -- repair and rebuild messages before retry
                    if err_str.contains("2013") || err_str.contains("tool call result does not follow tool call") {
                        eprintln!("[!] Tool pairing error (2013) in non-streaming, repairing context...");
                        {
                            let mut ctx = self.context.write().await;
                            ctx.validate_tool_pairing();
                            ctx.fix_role_alternation();
                        }
                        // Rebuild messages from repaired context and retry
                        current_messages = self.entries_to_messages_async().await;
                        continue;
                    }

                    let classification = classify_error(&err_str, 0, 0);
                    if !classification.retryable {
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
        // Apply prompt caching to messages (system_and_3 strategy)
        let mut cached_messages = messages.to_vec();
        apply_prompt_caching(&mut cached_messages, "5m");

        // Build system prompt array with cache_control
        let mut sys_arr = serde_json::json!([{"type": "text", "text": system_prompt}]);
        cache_system_prompt(&mut sys_arr);

        let mut payload = serde_json::Map::new();
        payload.insert("model".to_string(), serde_json::json!(self.config.model));
        payload.insert("max_tokens".to_string(), serde_json::json!(16384));
        payload.insert("system".to_string(), sys_arr);
        payload.insert("messages".to_string(), serde_json::json!(cached_messages));
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

        // Display thinking if present (only in streaming mode)
        if !thinking.is_empty() && self.use_stream {
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

        // Path traversal protection: file tools must stay within project directory
        let path_tools = ["read_file", "write_file", "edit_file", "multi_edit", "fileops", "list_dir"];
        if path_tools.contains(&name) {
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    if let Err(e) = crate::tools::is_path_allowed(path) {
                        return Ok(ToolResult::error(e));
                    }
                }
            }
            if name == "fileops" {
                if let Some(dest) = params.get("destination").and_then(|v| v.as_str()) {
                    if !dest.is_empty() {
                        if let Err(e) = crate::tools::is_path_allowed(dest) {
                            return Ok(ToolResult::error(e));
                        }
                    }
                }
            }
        }

        // Check permissions
        if let Some(result) = self.gate.check(tool.as_ref(), params.clone()) {
            return Ok(result);
        }

        // Coerce argument types to match schema
        let schema = tool.input_schema();
        let mut coerced = params;
        let coercion_result = crate::tools::coercion::coerce_arguments(&schema, &mut coerced);
        if !coercion_result.warnings.is_empty() {
            for w in &coercion_result.warnings {
                eprintln!("[coercion] {}", w);
            }
        }

        // Execute with 5-minute timeout (matching Go's toolTimeout)
        let tool_name = name.to_string();
        let timeout = std::time::Duration::from_secs(300);
        let start = std::time::Instant::now();

        // Since tools are sync, use spawn_blocking
        let tool_ref = tool.clone();
        let coerced_clone = coerced.clone();
        let result = tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || {
            tool_ref.execute(coerced_clone)
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
                    ..Default::default()
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

    /// Post-compact recovery re-injects critical context after compaction.
    /// This prevents the model from losing awareness of files it was working on
    /// and skills it was using, reducing wasted turns re-reading them.
    /// Returns the list of recovered file paths (for deduplication in AddHistorySnip).
    async fn post_compact_recovery(&self) -> Vec<String> {

        if !self.config.post_compact_recover_files {
            return vec![];
        }

        let mut recovered_paths = Vec::new();

        // --- File content recovery ---
        let registry = self.registry.read().await;
        let max_files = if self.config.post_compact_max_files == 0 {
            5
        } else {
            self.config.post_compact_max_files
        };
        let max_file_chars = if self.config.post_compact_max_file_chars == 0 {
            50_000
        } else {
            self.config.post_compact_max_file_chars
        };

        let paths = registry.get_recently_read_files(max_files);
        drop(registry);

        let mut total_chars = 0;
        let mut files_recovered = 0;
        let mut paths_to_remark = Vec::new();

        for path in &paths {
            // Expand the normalized path back to a real path
            let real_path = if std::path::Path::new(path).is_absolute() {
                path.clone()
            } else {
                self.config.project_dir.join(path).to_string_lossy().to_string()
            };

            if let Ok(data) = std::fs::read_to_string(&real_path) {
                let content = if total_chars + data.len() > max_file_chars {
                    let remaining = max_file_chars - total_chars;
                    if remaining < 200 {
                        break;
                    }
                    let truncated: String = data.chars().take(remaining).collect();
                    format!("{}\n... [truncated]", truncated)
                } else {
                    data.clone()
                };

                let attachment = format!(
                    "[Post-compact file recovery: {}]\n```\n{}\n```",
                    path, content
                );
                {
                    let mut ctx_mut = self.context.write().await;
                    ctx_mut.add_attachment(attachment);
                }
                total_chars += data.len();
                files_recovered += 1;
                recovered_paths.push(path.clone());
                paths_to_remark.push(path.clone());
            }
        }

        // Re-mark files as read so edit checks still work
        if !paths_to_remark.is_empty() {
            let registry = self.registry.read().await;
            for path in &paths_to_remark {
                registry.mark_file_read(path);
            }
        }

        if files_recovered > 0 {
            eprintln!(
                "[post-compact] Recovered {} files ({} chars)",
                files_recovered, total_chars
            );
        }

        // --- Skill content recovery ---
        if let Some(loader) = &self.config.skill_loader {
            let max_skill_chars = if self.config.post_compact_max_skill_chars == 0 {
                5_000
            } else {
                self.config.post_compact_max_skill_chars
            };
            let max_total_skill_chars = if self.config.post_compact_max_total_skill_chars == 0 {
                25_000
            } else {
                self.config.post_compact_max_total_skill_chars
            };

            let read_skills = {
                let tracker = self.skill_tracker.read().await;
                tracker.get_read_skill_names()
            };

            let mut total_skill_chars = 0;
            let mut skills_recovered = 0;

            for name in &read_skills {
                let content = match loader.load_skill(name) {
                    Some(c) => c,
                    None => continue,
                };
                if content.is_empty() {
                    continue;
                }

                let truncated = if content.len() > max_skill_chars {
                    let truncated: String = content.chars().take(max_skill_chars).collect();
                    format!("{}\n... [truncated]", truncated)
                } else {
                    content.clone()
                };

                if total_skill_chars + truncated.len() > max_total_skill_chars {
                    break;
                }

                let attachment = format!(
                    "[Post-compact skill recovery: {}]\n{}",
                    name, truncated
                );
                {
                    let mut ctx_mut = self.context.write().await;
                    ctx_mut.add_attachment(attachment);
                }
                total_skill_chars += truncated.len();
                skills_recovered += 1;
            }

            if skills_recovered > 0 {
                eprintln!(
                    "[post-compact] Recovered {} skills ({} chars)",
                    skills_recovered, total_skill_chars
                );
            }
        }

        recovered_paths
    }

    /// Close releases resources (MCP servers, session memory, etc.)
    pub fn close(&self) {
        if let Some(ref mgr) = self.config.mcp_manager {
            mgr.stop_all();
        }
        // Session memory is stopped on drop via Arc; the flush loop
        // uses its own Drop impl. No explicit stop needed here.
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

    /// Force compact the conversation context (for /compact command).
    /// Uses local truncation-based compaction (no LLM call).
    pub fn force_compact(&self) -> crate::compact::CompactStats {
        let mut context = self.context.blocking_write();
        let messages = context.messages().to_vec();
        let entries_before = context.len();
        let tokens_before = crate::compact::estimate_total_tokens(&messages);

        if messages.is_empty() {
            return crate::compact::CompactStats {
                phase: crate::compact::CompactPhase::None,
                entries_before: 0,
                entries_after: 0,
                estimated_tokens_saved: 0,
                estimated_tokens_before: 0,
                estimated_tokens_after: 0,
                tokens_after: 0,
                post_compact_tokens: 0,
            };
        }

        // If too few messages, nothing to compact
        if messages.len() <= 3 {
            return crate::compact::CompactStats {
                phase: crate::compact::CompactPhase::None,
                entries_before,
                entries_after: entries_before,
                estimated_tokens_saved: 0,
                estimated_tokens_before: tokens_before,
                estimated_tokens_after: tokens_before,
                tokens_after: tokens_before,
                post_compact_tokens: tokens_before,
            };
        }

        // Use local truncation: keep system + last N messages
        let keep_last = 10;
        let total = messages.len();
        let split = if total > keep_last + 1 { total - keep_last } else { 1 };

        // Keep first (system) and last keep_last messages
        let mut kept = vec![messages[0].clone()];
        kept.extend(messages[split..].to_vec());

        let tokens_after = crate::compact::estimate_total_tokens(&kept);
        let saved = tokens_before.saturating_sub(tokens_after);

        // Add compact boundary marker
        context.add_compact_boundary(crate::context::CompactTrigger::Manual, tokens_before);
        context.replace_messages(kept);

        let entries_after = context.len();

        crate::compact::CompactStats {
            phase: crate::compact::CompactPhase::Truncated,
            entries_before,
            entries_after,
            estimated_tokens_saved: saved,
            estimated_tokens_before: tokens_before,
            estimated_tokens_after: tokens_after,
            tokens_after,
            post_compact_tokens: tokens_after,
        }
    }

    /// Force partial compaction with direction and optional pivot index.
    /// Direction: "up_to" or "from". Pivot index defaults to midpoint if not provided.
    pub fn force_partial_compact(&self, direction: &str, pivot_index: Option<usize>) -> crate::compact::PartialCompactionResult {
        use crate::compact::PartialCompactDirection;

        let dir = match direction {
            "from" => PartialCompactDirection::From,
            _ => PartialCompactDirection::UpTo,
        };

        let mut context = self.context.blocking_write();
        let messages = context.messages().to_vec();
        let total = messages.len();

        // Default pivot: midpoint of messages
        let pivot = pivot_index.unwrap_or(total / 2);

        eprintln!("[partial-compact] direction={}, pivot={}, total_messages={}", direction, pivot, total);

        crate::compact::partial_compact(&mut context, dir, pivot)
    }

    /// Clear all conversation messages (for /clear command).
    /// Returns the number of messages cleared.
    pub fn clear_context(&self) -> usize {
        let mut context = self.context.blocking_write();
        let count = context.len();
        context.clear();
        count
    }

    /// Get a reference to the config
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get a reference to the registry (for sub-agent spawning)
    pub fn registry(&self) -> &Arc<RwLock<Registry>> {
        &self.registry
    }

    /// Get the number of tools used (for sub-agent reporting)
    pub fn tools_used_count(&self) -> usize {
        self.tools_used.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get a clone of the context Arc (for fork mode)
    pub fn context_arc(&self) -> Arc<RwLock<ConversationContext>> {
        self.context.clone()
    }

    /// Get the last assistant text from the conversation context as a partial result.
    /// Used when the agent's Run returns empty.
    pub fn get_partial_result(&self) -> String {
        let ctx = self.context.blocking_read();
        for entry in ctx.entries().iter().rev() {
            if entry.role.as_str() == "assistant" {
                if let MessageContent::Text(text) = &entry.content {
                    if !text.is_empty() {
                        return text.clone();
                    }
                }
            }
        }
        String::new()
    }

    /// Create a new AgentLoop for a sub-agent with a custom system prompt.
    /// Reuses the parent's API key, base URL, and HTTP client configuration.
    pub fn new_for_sub_agent(
        config: Config,
        registry: Registry,
        system_prompt: &str,
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

        let client = build_http_client(&api_key)
            .ok_or_else(|| anyhow!("failed to build HTTP client (invalid API key?)"))?;

        let max_turns = config.max_turns;
        let file_history = config.file_history.clone().unwrap_or_else(|| Arc::new(FileHistory::new()));
        let context = ConversationContext::new(config.clone());
        let gate = PermissionGate::new(config.clone());

        // Create sub-agent transcript in separate directory
        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S-sub").to_string();
        let transcript_dir = PathBuf::from(".claude").join("transcripts").join("sub-agents");
        let _ = std::fs::create_dir_all(&transcript_dir);
        let transcript_path = transcript_dir.join(format!("{}.jsonl", session_id));
        let transcript = Transcript::new(&transcript_path);
        let _ = transcript.add_system(format!("sub-agent: model={}, mode={}", gate.config.model, gate.config.permission_mode));

        let session_memory = config.session_memory.clone();
        let compactor = RwLock::new(
            Compactor::new()
                .with_threshold(config.auto_compact_threshold)
                .with_buffer(config.auto_compact_buffer)
                .with_max_tokens(crate::compact::model_context_window(&gate.config.model))
                .with_session_memory(session_memory)
                .with_reactive_threshold(config.reactive_compact_threshold)
        );

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow!("failed to create tokio runtime: {}", e))?;

        Ok(Self {
            config: gate.config.clone(),
            registry: Arc::new(RwLock::new(registry)),
            gate,
            context: Arc::new(RwLock::new(context)),
            client,
            use_stream: true,
            max_tool_chars: 8192,
            max_turns,
            base_url,
            api_key,
            transcript,
            compactor,
            file_history,
            rt,
            interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rate_limit_state: RateLimitState::default(),
            skill_tracker: Arc::new(RwLock::new(SkillTracker::new())),
            last_call_was_streaming: std::sync::atomic::AtomicBool::new(false),
            custom_system_prompt: Some(system_prompt.to_string()),
            tools_used: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

/// Re-export for backward compatibility
pub use crate::error_types::is_transient_error;

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
        "process" => {
            if let Some(name) = input.get("process_name").and_then(|v| v.as_str()) {
                return name.to_string();
            }
            if let Some(pid) = input.get("pid").and_then(|v| v.as_i64()) {
                return format!("PID {}", pid);
            }
        }
        "runtime_info" => {
            if let Some(show) = input.get("show").and_then(|v| v.as_str()) {
                return show.to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_consume() {
        let mut budget = IterationBudget::new(3);
        assert!(budget.consume()); // 1/3
        assert!(budget.consume()); // 2/3
        assert!(budget.consume()); // 3/3
        assert!(!budget.consume()); // exhausted
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn test_refund() {
        let mut budget = IterationBudget::new(3);
        assert!(budget.consume()); // 1/3
        assert!(budget.consume()); // 2/3
        assert!(budget.consume()); // 3/3
        assert!(!budget.consume()); // exhausted
        budget.refund(); // back to 2/3
        assert!(budget.consume()); // 3/3 again
        assert!(!budget.consume()); // exhausted again
    }

    #[test]
    fn test_grace_call_once() {
        let mut budget = IterationBudget::new(2);
        assert!(budget.consume()); // 1/2
        assert!(budget.consume()); // 2/2
        assert!(!budget.consume()); // exhausted

        // Grace call should work once
        assert!(budget.grace_call());
        assert_eq!(budget.remaining(), 0);

        // Second grace call should fail
        assert!(!budget.grace_call());
    }

    #[test]
    fn test_grace_call_then_exhausted() {
        let mut budget = IterationBudget::new(1);
        assert!(budget.consume()); // 1/1
        assert!(!budget.consume()); // exhausted

        // Grace call grants one more
        assert!(budget.grace_call());
        // Still can't consume after grace
        assert!(!budget.consume());
        // No more grace
        assert!(!budget.grace_call());
    }

    #[test]
    fn test_refund_does_not_restore_grace() {
        let mut budget = IterationBudget::new(1);
        assert!(budget.consume()); // 1/1
        assert!(budget.grace_call()); // grace used
        budget.refund(); // gives one back
        assert!(!budget.consume()); // can consume again
        assert!(!budget.grace_call()); // grace already called, can't be used again
    }

    #[test]
    fn test_remaining() {
        let mut budget = IterationBudget::new(5);
        assert_eq!(budget.remaining(), 5);
        budget.consume();
        assert_eq!(budget.remaining(), 4);
        budget.consume();
        budget.refund();
        assert_eq!(budget.remaining(), 4);
        // Consume all remaining
        budget.consume();
        budget.consume();
        budget.consume();
        assert_eq!(budget.remaining(), 1);
        budget.consume();
        assert_eq!(budget.remaining(), 0);
        budget.grace_call();
        assert_eq!(budget.remaining(), 0); // grace_used means 0 remaining
    }

    #[test]
    fn test_zero_max() {
        let mut budget = IterationBudget::new(0);
        assert!(!budget.consume()); // immediately exhausted
        assert!(budget.grace_call()); // grace still works
        assert!(!budget.grace_call()); // only once
    }
}
