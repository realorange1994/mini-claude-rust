use crate::compact::Compactor;
use crate::config::Config;
use crate::context::{ConversationContext, ConversationEntry, MessageContent};
use crate::filehistory::FileHistory;
use crate::permissions::PermissionGate;
use crate::streaming::{CollectHandler, TerminalHandler, StallDetector, process_sse_events, ToolCallInfo};
use crate::tools::{truncate_at, ToolResult, Registry};
use crate::transcript::Transcript;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};

/// Transition tracking for context management
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
    file_history: FileHistory,
    rt: tokio::runtime::Runtime,
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
        let context = ConversationContext::new(config.clone());
        let gate = PermissionGate::new(config);

        // Initialize transcript writer (matching Go's behavior)
        let session_id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let transcript_dir = PathBuf::from(".claude").join("transcripts");
        let _ = std::fs::create_dir_all(&transcript_dir);
        let transcript_path = transcript_dir.join(format!("{}.jsonl", session_id));
        let transcript = Transcript::new(&transcript_path);
        let _ = transcript.add_user(format!("model={}, mode={}", gate.config.model, gate.config.permission_mode));

        // Initialize compactor
        let compactor = RwLock::new(Compactor::new());

        // Initialize file history for undo/rewind
        let file_history = FileHistory::new();

        // Create a single tokio runtime reused across all run() calls
        let rt = tokio::runtime::Runtime::new().unwrap();

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
        }
    }

    /// Process a user message through the agent loop
    pub fn run(&self, user_message: &str) -> String {
        // Add user message to context
        {
            let mut ctx = self.context.blocking_write();
            ctx.add_user_message(user_message.to_string());
        }

        // Log user message to transcript
        let _ = self.transcript.add_user(user_message.to_string());

        let system_prompt = crate::config::build_system_prompt(
            &self.registry.blocking_read(),
            &self.config.permission_mode,
            &self.config.project_dir,
            self.config.skill_loader.as_ref(),
        );

        // Get messages and tools for API call
        let messages = self.entries_to_messages();
        let tools = self.get_tools_schema();

        // Run the async agent loop in a blocking way
        let result = self.rt.block_on(self.run_agent_loop(&system_prompt, &messages, &tools));

        match result {
            Ok(response) => {
                // Log assistant response to transcript
                let _ = self.transcript.add_assistant(response.clone(), Vec::new());

                let mut ctx = self.context.blocking_write();
                ctx.add_assistant_text(response.clone());
                response
            }
            Err(e) => {
                let err_msg = format!("Error: {}", e);
                let _ = self.transcript.add_assistant(err_msg.clone(), Vec::new());
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
            .map(|entry| {
                let content: Vec<serde_json::Value> = match &entry.content {
                    MessageContent::Text(text) => {
                        // Anthropic requires content to be an array
                        vec![serde_json::json!({"type": "text", "text": text})]
                    }
                    MessageContent::ToolUseBlocks(blocks) => {
                        blocks.iter().map(|b| {
                            serde_json::json!({
                                "type": "tool_use",
                                "id": b.id,
                                "name": b.name,
                                "input": b.input
                            })
                        }).collect()
                    }
                    MessageContent::ToolResultBlocks(blocks) => {
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
                        }).collect()
                    }
                };
                serde_json::json!({
                    "role": entry.role,
                    "content": content
                })
            })
            .collect()
    }

    /// Run the agent loop asynchronously
    async fn run_agent_loop(&self, system_prompt: &str, _messages: &[serde_json::Value], tools: &[serde_json::Value]) -> Result<String> {
        let mut turn = 0;
        let mut last_transition = Transition::None;
        let mut consecutive_stalls = 0;
        let mut context_errors = 0;
        const MAX_CONTEXT_RECOVERY: usize = 3;

        loop {
            turn += 1;
            if turn > self.max_turns {
                break;
            }

            // Run compaction before API call (matching Go's CompactContext)
            {
                let mut ctx = self.context.write().await;
                let mut compactor = self.compactor.write().await;
                let stats = compactor.compact(&mut ctx);
                if stats.phase != crate::compact::CompactPhase::None {
                    eprintln!("[Compaction] {:?}: {} -> {} entries, ~{} tokens saved",
                        stats.phase, stats.entries_before, stats.entries_after, stats.estimated_tokens_saved);
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
            ).await;

            match result {
                Ok((tool_calls, text)) => {
                    consecutive_stalls = 0;
                    context_errors = 0;

                    if !tool_calls.is_empty() {
                        // Execute tools
                        last_transition = Transition::ToolsToText;

                        // Print all tool calls upfront
                        for tc in &tool_calls {
                            let params: HashMap<String, serde_json::Value> =
                                serde_json::from_str(&tc.arguments).unwrap_or_default();
                            if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                                eprintln!("  $ {}", cmd);
                            } else if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                                eprintln!("  [{}] {}", tc.name, path);
                            } else if !params.is_empty() {
                                // Show first meaningful parameter
                                let mut printed = false;
                                for (key, val) in &params {
                                    match val {
                                        serde_json::Value::String(s) if !s.is_empty() => {
                                            eprintln!("  [{}] {}={}", tc.name, key, limit_str(s, 120));
                                            printed = true;
                                            break;
                                        }
                                        serde_json::Value::Number(n) => {
                                            eprintln!("  [{}] {}={}", tc.name, key, n);
                                            printed = true;
                                            break;
                                        }
                                        serde_json::Value::Bool(b) => {
                                            eprintln!("  [{}] {}={}", tc.name, key, b);
                                            printed = true;
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                if !printed {
                                    eprintln!("  [{}]", tc.name);
                                }
                            } else {
                                eprintln!("  [{}]", tc.name);
                            }
                        }

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
                            let transcript = &self.transcript;
                            if entry.denied {
                                // Denied tools are handled immediately
                                let output = entry.err_text;
                                let _ = transcript.add_tool_result(
                                    entry.tc.id.clone(),
                                    entry.tc.name.clone(),
                                    entry.tc.arguments.clone(),
                                    output.clone(),
                                );
                                handles.push(tokio::task::spawn(async move {
                                    (entry.index, output, true, std::time::Duration::ZERO)
                                }));
                            } else {
                                // Approved tools execute concurrently
                                let tc = entry.tc.clone();
                                let params = entry.params.clone();
                                let max_tool_chars = self.max_tool_chars;

                                // Clone what we need for the spawned task
                                let registry_clone = self.registry.clone();
                                let file_history = self.file_history.clone();

                                handles.push(tokio::task::spawn(async move {
                                    let start = std::time::Instant::now();
                                    let timeout = std::time::Duration::from_secs(300);

                                    let tool_name = tc.name.clone();

                                    // Auto-snapshot before write/edit tools (matching Go's TakeSnapshot)
                                    if tool_name == "write_file" || tool_name == "edit_file" || tool_name == "multi_edit" {
                                        if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                                            if !path.is_empty() {
                                                let _ = file_history.snapshot(std::path::Path::new(path));
                                            }
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
                                    (entry.index, output.0, output.1, output.2)
                                }));
                            }
                        }

                        // Collect results in order
                        let mut tool_results: Vec<(usize, String, bool, std::time::Duration)> = Vec::new();
                        for handle in handles {
                            if let Ok(result) = handle.await {
                                tool_results.push(result);
                            }
                        }
                        tool_results.sort_by_key(|r| r.0);

                        // Display results (matching Go's ASCII format: [+] tool: preview / [x] tool (time): error)
                        for (i, output, is_error, elapsed) in &tool_results {
                            let tc = &tool_calls[*i];
                            let elapsed_str = format!("{:.2}s", elapsed.as_secs_f64());
                            if *is_error {
                                let preview = limit_str(output, 150);
                                eprintln!("  [x] {} ({}): {}", tc.name, elapsed_str, preview);
                            } else {
                                let preview = tool_result_preview(&tc.name, output);
                                if tc.name == "exec" {
                                    eprintln!("  {}", preview);
                                } else {
                                    eprintln!("  [+] {}: {}", tc.name, preview);
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

                        // 2. Store tool_result blocks
                        let tool_result_blocks: Vec<crate::context::ToolResultBlock> = tool_calls.iter().enumerate().map(|(i, tc)| {
                            // Find the result for this tool call
                            let (output, is_error) = tool_results.iter()
                                .find(|(idx, _, _, _)| *idx == i)
                                .map(|(_, output, is_error, _)| (output.clone(), *is_error))
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

                    } else if !text.is_empty() {
                        // Final response
                        return Ok(text);
                    } else {
                        // No output, try again
                        eprintln!("[!] Empty response, continuing...");
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();

                    // Model confusion - inject corrective message and retry
                    if err_str.contains("model confused") {
                        eprintln!("[!] Model confused, injecting corrective message...");
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
                    if err_str.contains("context_length") || err_str.contains("400") ||
                       err_str.contains("stream stalled") || err_str.contains("context canceled") {
                        context_errors += 1;
                        if context_errors > MAX_CONTEXT_RECOVERY {
                            eprintln!("[!] Context recovery exhausted after {} attempts, giving up", MAX_CONTEXT_RECOVERY);
                            return Ok("Error: Context overflow - unable to recover".to_string());
                        }

                        // 3-phase progressive recovery
                        if context_errors <= 1 {
                            eprintln!("[!] Context overflow, truncating history (phase 1/3)...");
                            let mut ctx = self.context.write().await;
                            ctx.truncate_history();
                        } else if context_errors <= 2 {
                            eprintln!("[!] Context still full, aggressive truncation (phase 2/3)...");
                            let mut ctx = self.context.write().await;
                            ctx.aggressive_truncate_history();
                        } else {
                            eprintln!("[!] Context still full, dropping to minimum (phase 3/3)...");
                            let mut ctx = self.context.write().await;
                            ctx.minimum_history();
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
    ) -> Result<(Vec<ToolCallInfo>, String)> {
        const MAX_RETRIES: usize = 10;
        const INITIAL_BACKOFF_MS: u64 = 2000;
        const MAX_BACKOFF_MS: u64 = 18000;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        // Try streaming first if enabled
        if self.use_stream {
            for attempt in 0..MAX_RETRIES {
                match self.try_stream_once(system_prompt, messages, tools).await {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        let err_str = e.to_string();

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
        }

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

        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);

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
            &mut cancel_rx,
        ).await;

        // Cancel any pending operations
        let _ = cancel_tx.send(()).await;

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
            match self.call_api_non_streaming(system_prompt, messages, tools).await {
                Ok(result) => return Ok(result),
                Err(e) => {
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

        let response = self.client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("Authorization", format!("Bearer {}", &self.api_key))
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("API error {}: {}", status, body));
        }

        let body: serde_json::Value = response.json().await?;

        // Parse response
        let mut tool_calls = Vec::new();
        let mut text = String::new();
        let mut thinking = String::new();

        if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
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
                        _ => {}
                    }
                }
            }
        }

        // Display thinking if present
        if !thinking.is_empty() {
            let preview = truncate_at(thinking.lines().next().unwrap_or(""), 120);
            eprintln!("\n[THINK] {}", preview);
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
}

/// Check if an error is transient (retryable)
fn is_transient_error(err_str: &str) -> bool {
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
fn limit_str(s: &str, max: usize) -> String {
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

/// Extract the most relevant part of a tool result for display (matching Go's toolResultPreview)
fn tool_result_preview(tool_name: &str, output: &str) -> String {
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
fn clean_exec_output(output: &str) -> String {
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
