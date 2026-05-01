//! Streaming response handling for agent loop
//! Full implementation of SSE parsing, stall detection, chunk collection,
//! and transient error recovery (matching hermes-agent patterns).

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures::StreamExt;

use crate::agent_loop::tool_arg_summary;
use crate::prompt_caching::cache_system_prompt;
use crate::rate_limit::{parse_rate_limit_headers, RateLimitState};
use crate::tools::truncate_at;

/// Streaming chunk types
#[derive(Debug, Clone)]
pub enum ChunkType {
    Text,
    ToolCall,
    ToolArgument,
    Thinking,
    Usage,
    #[allow(dead_code)]
    Error,
    Done,
    BlockStop,
}

/// A single event emitted during a streaming response
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub chunk_type: ChunkType,
    pub content: String,
    pub id: Option<String>,
    pub name: Option<String>,
    pub usage: Option<Usage>,
}

/// Token usage information
#[derive(Debug, Clone)]
pub struct Usage {
    #[allow(dead_code)]
    pub input_tokens: i64,
    #[allow(dead_code)]
    pub output_tokens: i64,
}

/// CollectHandler assembles streamed tokens into a complete response
pub struct CollectHandler {
    text: RwLock<String>,
    tool_calls: RwLock<Vec<ToolCallInfo>>,
    thinking: RwLock<String>,
    tool_use_as_text: RwLock<bool>,
    usage: RwLock<Option<Usage>>,
    finish_reason: RwLock<Option<String>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Result of a streaming API call, including partial delivery on failure.
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub tool_calls: Vec<ToolCallInfo>,
    pub text: String,
    pub thinking: String,
    pub completed: bool,
    /// Why the stream ended. Matches Anthropic stop_reason values:
    /// - "end_turn": normal completion
    /// - "stop_sequence": stop sequence hit
    /// - "max_tokens": output token limit reached
    /// - "tool_use": model yielded to tool use
    /// - None: stream ended abnormally (error, stall, interrupt)
    pub finish_reason: Option<String>,
}

/// Detect transient errors that are safe to retry (matching hermes-agent patterns).
fn is_transient_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    let patterns = [
        "connection lost", "connection reset", "connection error",
        "peer closed", "broken pipe", "upstream connect error",
        "timeout", "timed out", "pool timeout", "connect timeout",
        "remote protocol error", "stream error",
    ];
    patterns.iter().any(|p| lower.contains(p))
}

/// Maximum stream retries before giving up (matching hermes-agent default of 2).
const MAX_STREAM_RETRIES: usize = 2;

impl CollectHandler {
    pub fn new() -> Self {
        Self {
            text: RwLock::new(String::new()),
            tool_calls: RwLock::new(Vec::new()),
            thinking: RwLock::new(String::new()),
            tool_use_as_text: RwLock::new(false),
            usage: RwLock::new(None),
            finish_reason: RwLock::new(None),
        }
    }

    /// Handle a single chunk
    pub fn handle(&self, chunk: StreamChunk) {
        match chunk.chunk_type {
            ChunkType::Text => {
                let mut text = self.text.write().unwrap();
                let content_lower = chunk.content.to_lowercase();

                // Detect model echoing tool syntax as text (2-of-3 structural markers)
                let has_type = content_lower.contains(r#""type":"tool_use""#)
                    || content_lower.contains(r#""type": "tool_use""#);
                let has_id = content_lower.contains(r#""id":""#)
                    || content_lower.contains(r#""id": ""#);
                let has_name = content_lower.contains(r#""name":""#)
                    || content_lower.contains(r#""name": ""#);

                if (has_type && has_id) || (has_type && has_name) || (has_id && has_name) {
                    let mut flag = self.tool_use_as_text.write().unwrap();
                    *flag = true;
                } else {
                    text.push_str(&chunk.content);
                }
            }
            ChunkType::ToolCall => {
                let mut calls = self.tool_calls.write().unwrap();
                calls.push(ToolCallInfo {
                    id: chunk.id.unwrap_or_default(),
                    name: chunk.name.unwrap_or_default(),
                    arguments: String::new(),
                });
            }
            ChunkType::ToolArgument => {
                let mut calls = self.tool_calls.write().unwrap();
                if let Some(last) = calls.last_mut() {
                    last.arguments.push_str(&chunk.content);
                }
            }
            ChunkType::Thinking => {
                let mut thinking = self.thinking.write().unwrap();
                thinking.push_str(&chunk.content);
            }
            ChunkType::Usage => {
                if let Some(u) = chunk.usage {
                    let mut usage = self.usage.write().unwrap();
                    *usage = Some(u);
                }
            }
            ChunkType::Error => {
                // Error is handled by caller
            }
            ChunkType::Done | ChunkType::BlockStop => {
                // Stream finished
            }
        }
    }

    /// Get the assembled text (falls back to thinking when text is empty)
    pub fn full_response(&self) -> String {
        let text = self.text.read().unwrap();
        if !text.is_empty() {
            return text.clone();
        }
        let thinking = self.thinking.read().unwrap();
        thinking.clone()
    }

    /// Get the thinking content
    pub fn thinking(&self) -> String {
        let thinking = self.thinking.read().unwrap();
        thinking.clone()
    }

    /// Check if model echoed tool syntax as text
    pub fn is_tool_use_as_text(&self) -> bool {
        *self.tool_use_as_text.read().unwrap()
    }

    /// Get tool calls
    pub fn tool_calls(&self) -> Vec<ToolCallInfo> {
        self.tool_calls.read().unwrap().clone()
    }

    /// Set the finish reason from the stream
    pub fn set_finish_reason(&self, reason: String) {
        let mut fr = self.finish_reason.write().unwrap();
        *fr = Some(reason);
    }

    /// Get the finish reason
    pub fn finish_reason(&self) -> Option<String> {
        self.finish_reason.read().unwrap().clone()
    }

    /// Check if there's a partial (incomplete) tool call being accumulated.
    /// A tool call is partial if it has an id/name but no arguments yet completed
    /// (the stream cut off mid-tool-call, so args may be incomplete JSON).
    pub fn has_partial_tool_call(&self) -> bool {
        let calls = self.tool_calls.read().unwrap();
        if calls.is_empty() {
            return false;
        }
        // Last tool call has no arguments -- stream cut off during tool_use block
        let last = calls.last().unwrap();
        last.arguments.is_empty()
    }

    /// Check if any tool call has truncated (invalid JSON) arguments.
    /// Matching Hermes: if JSON parse fails, the tool args were cut off mid-stream.
    pub fn has_truncated_tool_args(&self) -> bool {
        let calls = self.tool_calls.read().unwrap();
        for call in calls.iter() {
            if !call.arguments.is_empty() {
                if serde_json::from_str::<serde_json::Value>(&call.arguments).is_err() {
                    return true;
                }
            }
        }
        false
    }

    /// Clear the last partial tool call and any trailing arguments.
    /// Used before retry to avoid duplicating tool_call entries on reconnect.
    pub fn clear_partial_tool_call(&self) {
        let mut calls = self.tool_calls.write().unwrap();
        if !calls.is_empty() {
            calls.pop();
        }
    }

    /// Clear all pending text that was already streamed to the user.
    /// Used when retry cannot recover text deltas (text-only case).
    pub fn clear_text(&self) {
        let mut text = self.text.write().unwrap();
        text.clear();
    }

    /// Clear all accumulated state (text, tool_calls, thinking).
    /// Used before stream retries where the API will send a completely
    /// new response -- old collected data would have mismatched IDs.
    pub fn clear_all(&self) {
        let mut text = self.text.write().unwrap();
        text.clear();
        drop(text);
        let mut calls = self.tool_calls.write().unwrap();
        calls.clear();
        drop(calls);
        let mut thinking = self.thinking.write().unwrap();
        thinking.clear();
    }

    /// Get usage info
    #[allow(dead_code)]
    pub fn usage(&self) -> Option<Usage> {
        self.usage.read().unwrap().clone()
    }
}

impl Default for CollectHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// TerminalHandler prints clean output to terminal
// --- Thinking block filter state machine (Hermes-style) ---

/// Tag constants for the think filter state machine.
const THINK_OPEN_LONG: &str = "<thinking>";
const THINK_OPEN_SHORT: &str = "<think>";
const THINK_CLOSE_LONG: &str = "</thinking>";
const THINK_CLOSE_SHORT: &str = "</think>";

/// ANSI escape codes for dim/gray styling of thinking content.
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RESET: &str = "\x1b[0m";

/// State machine for filtering `<thinking>...</thinking>`, `<think>...</think>`,
/// and Anthropic extended thinking blocks from terminal display.
/// Thinking content is shown in a dimmed style; tag markers are stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkFilterState {
    /// Normal text -- pass through as-is
    Normal,
    /// Detected `<` that might start `<thinking>` or `<think>`; consuming tag
    InThinkOpenTag,
    /// Inside a thinking block -- output content with ANSI dim styling
    InThinkBlock,
    /// Detected `<` that might start `</thinking>` or `</think>`; consuming tag
    InThinkCloseTag,
}

/// Output action from the think filter state machine for a single character.
#[derive(Debug, Clone, PartialEq)]
pub enum ThinkFilterAction {
    /// Print this character as normal (non-thinking) text
    Print(char),
    /// This character is thinking content; the caller should wrap with dim styling
    Think(char),
    /// Suppress this character (it is part of a tag being consumed)
    Suppress,
    /// False-alarm recovery: replay these buffered characters as normal text
    FlushNormal(String),
    /// False-alarm recovery: replay these buffered characters as thinking text
    FlushThink(String),
    /// Transition: just entered a thinking block, emit ANSI dim start code
    EnterThink,
    /// Transition: just exited a thinking block, emit ANSI reset code
    ExitThink,
}

impl ThinkFilterState {
    /// Process a character through the state machine.
    /// Returns (new_state, Vec<ThinkFilterAction>) describing what the caller
    /// should do with this character and any buffered content.
    pub fn process(self, c: char, buf: &mut String) -> (Self, Vec<ThinkFilterAction>) {
        match self {
            ThinkFilterState::Normal => {
                // Check for opening tag <think> or <thinking>
                if c == '<' {
                    buf.push(c);
                    return (ThinkFilterState::InThinkOpenTag, vec![ThinkFilterAction::Suppress]);
                }
                (ThinkFilterState::Normal, vec![ThinkFilterAction::Print(c)])
            }
            ThinkFilterState::InThinkOpenTag => {
                buf.push(c);
                let buffered = buf.as_str();

                // Check for complete open tags
                if buffered.ends_with(THINK_OPEN_LONG) || buffered.ends_with(THINK_OPEN_SHORT) {
                    buf.clear();
                    return (
                        ThinkFilterState::InThinkBlock,
                        vec![ThinkFilterAction::Suppress, ThinkFilterAction::EnterThink],
                    );
                }

                // Check if still a potential prefix of either open tag
                if THINK_OPEN_LONG.starts_with(buffered) || THINK_OPEN_SHORT.starts_with(buffered) {
                    return (ThinkFilterState::InThinkOpenTag, vec![ThinkFilterAction::Suppress]);
                }

                // False alarm: '<' was not part of a thinking tag.
                // Replay the buffered characters as normal text.
                let to_flush = buf.clone();
                buf.clear();
                (
                    ThinkFilterState::Normal,
                    vec![ThinkFilterAction::FlushNormal(to_flush)],
                )
            }
            ThinkFilterState::InThinkBlock => {
                // Check for closing tag </think> or </thinking>
                if c == '<' {
                    buf.push(c);
                    return (ThinkFilterState::InThinkCloseTag, vec![ThinkFilterAction::Suppress]);
                }
                (ThinkFilterState::InThinkBlock, vec![ThinkFilterAction::Think(c)])
            }
            ThinkFilterState::InThinkCloseTag => {
                buf.push(c);
                let buffered = buf.as_str();

                // Check for complete close tags
                if buffered.ends_with(THINK_CLOSE_LONG) || buffered.ends_with(THINK_CLOSE_SHORT) {
                    buf.clear();
                    return (
                        ThinkFilterState::Normal,
                        vec![ThinkFilterAction::Suppress, ThinkFilterAction::ExitThink],
                    );
                }

                // Check if still a potential prefix of either close tag
                if THINK_CLOSE_LONG.starts_with(buffered) || THINK_CLOSE_SHORT.starts_with(buffered) {
                    return (ThinkFilterState::InThinkCloseTag, vec![ThinkFilterAction::Suppress]);
                }

                // False alarm: '<' in thinking block was not a close tag.
                // Replay the buffered characters as thinking text.
                let to_flush = buf.clone();
                buf.clear();
                (
                    ThinkFilterState::InThinkBlock,
                    vec![ThinkFilterAction::FlushThink(to_flush)],
                )
            }
        }
    }
}

// --- Streaming progress metrics ---

/// Tracks streaming progress metrics for adaptive timeouts and user feedback
#[derive(Debug)]
pub struct StreamProgress {
    /// Time when the stream started
    pub start_time: Instant,
    /// Time when the first byte was received (TTFB)
    pub first_byte_time: Option<Instant>,
    /// Total tokens received so far
    pub tokens_received: usize,
    /// Total text characters received
    pub chars_received: usize,
    /// Number of tool calls received
    pub tool_calls_received: usize,
}

impl StreamProgress {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            first_byte_time: None,
            tokens_received: 0,
            chars_received: 0,
            tool_calls_received: 0,
        }
    }

    /// Record that a byte/chunk was received
    pub fn record_chunk(&mut self, content_len: usize) {
        if self.first_byte_time.is_none() {
            self.first_byte_time = Some(Instant::now());
        }
        self.chars_received += content_len;
        // Rough token estimate
        self.tokens_received += content_len.div_ceil(4);
    }

    /// Record a tool call
    pub fn record_tool_call(&mut self) {
        self.tool_calls_received += 1;
    }

    /// Get time-to-first-byte in milliseconds
    pub fn ttfb_ms(&self) -> Option<u64> {
        self.first_byte_time.map(|t| {
            t.duration_since(self.start_time).as_millis() as u64
        })
    }

    /// Get current throughput in tokens/second
    pub fn tokens_per_second(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.tokens_received as f64 / elapsed
        } else {
            0.0
        }
    }

    /// Get elapsed time since stream start
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }
}

// --- Error classification for retry ---

/// Classify an HTTP error as retryable or not.
/// Retryable errors: network timeout, 429 rate limit, 500-504 server errors
/// Non-retryable: 400 bad request, 401/403 auth errors, other client errors
pub fn is_retryable_http_error(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    matches!(code, 429 | 500 | 502 | 503 | 504)
}

/// Classify an error message as retryable or not
pub fn is_retryable_error_msg(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    // Transient errors that are worth retrying
    lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("reset")
        || lower.contains("broken pipe")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("529") // Anthropic overloaded
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("500")
        || lower.contains("429")
}

/// Get retry delay for a given retry attempt with exponential backoff
/// Base: 2s, max: 18s, jitter: ±0.5s
pub fn retry_delay(attempt: usize) -> Duration {
    let base_secs = 2u64;
    let max_secs = 18u64;
    let delay = (base_secs * 2u64.pow(attempt.min(4) as u32)).min(max_secs);
    Duration::from_secs(delay)
}

pub struct TerminalHandler {
    seen_tool_call: RwLock<bool>,
    thinking_buf: RwLock<String>,
    cur_tool_name: RwLock<String>,
    cur_tool_args: RwLock<String>,
    /// Think filter state machine: tracks whether we are inside a thinking block
    think_filter_state: RwLock<ThinkFilterState>,
    /// Buffer for the think filter state machine (holds partial tag matches)
    think_filter_buf: RwLock<String>,
    /// Whether ANSI dim styling is currently active (to avoid duplicate escape codes)
    think_dim_active: RwLock<bool>,
    /// Whether thinking was already printed to avoid double-print in Done
    thinking_printed: RwLock<bool>,
}

impl TerminalHandler {
    pub fn new() -> Self {
        Self {
            seen_tool_call: RwLock::new(false),
            thinking_buf: RwLock::new(String::new()),
            cur_tool_name: RwLock::new(String::new()),
            cur_tool_args: RwLock::new(String::new()),
            think_filter_state: RwLock::new(ThinkFilterState::Normal),
            think_filter_buf: RwLock::new(String::new()),
            think_dim_active: RwLock::new(false),
            thinking_printed: RwLock::new(false),
        }
    }

    pub fn handle(&self, chunk: StreamChunk) {
        match chunk.chunk_type {
            ChunkType::Thinking => {
                let mut buf = self.thinking_buf.write().unwrap();
                buf.push_str(&chunk.content);
            }
            ChunkType::ToolCall => {
                let mut seen = self.seen_tool_call.write().unwrap();
                *seen = true;
                drop(seen);

                // Show buffered thinking before tool call
                {
                    let buf = self.thinking_buf.read().unwrap();
                    if !buf.is_empty() {
                        let preview = truncate_at(buf.lines().next().unwrap_or(""), 120);
                        eprintln!("\n[THINK] {}", preview);
                        drop(buf);
                        let mut printed = self.thinking_printed.write().unwrap();
                        *printed = true;
                    }
                }
                let mut buf = self.thinking_buf.write().unwrap();
                buf.clear();
                drop(buf);

                let mut name = self.cur_tool_name.write().unwrap();
                *name = chunk.name.unwrap_or_default();
                let mut args = self.cur_tool_args.write().unwrap();
                args.clear();
            }
            ChunkType::ToolArgument => {
                let mut args = self.cur_tool_args.write().unwrap();
                args.push_str(&chunk.content);
            }
            ChunkType::BlockStop => {
                // Flush pending tool call
                self.flush_tool_call();
            }
            ChunkType::Done => {
                self.flush_tool_call();
                // Close any open ANSI dim styling at end of stream
                {
                    let mut dim = self.think_dim_active.write().unwrap();
                    if *dim {
                        eprint!("{}", ANSI_RESET);
                        *dim = false;
                    }
                }
                // Reset think filter state for next stream
                {
                    let mut state = self.think_filter_state.write().unwrap();
                    *state = ThinkFilterState::Normal;
                    let mut buf = self.think_filter_buf.write().unwrap();
                    buf.clear();
                }
                // Flush buffered thinking if no tool call was seen and not already printed
                let seen = self.seen_tool_call.read().unwrap();
                let printed = self.thinking_printed.read().unwrap();
                if !*seen && !*printed {
                    drop(seen);
                    drop(printed);
                    let buf = self.thinking_buf.read().unwrap();
                    if !buf.is_empty() {
                        let preview = truncate_at(buf.lines().next().unwrap_or(""), 120);
                        eprintln!("\n[THINK] {}", preview);
                    }
                }
            }
            ChunkType::Text => {
                // Flush any pending tool call before text
                self.flush_tool_call();
                // Flush buffered thinking if any (thinking arrives before text in stream)
                {
                    let buf = self.thinking_buf.read().unwrap();
                    if !buf.is_empty() {
                        let preview = truncate_at(buf.lines().next().unwrap_or(""), 120);
                        eprintln!("\n[THINK] {}", preview);
                        drop(buf);
                        let mut buf = self.thinking_buf.write().unwrap();
                        buf.clear();
                        drop(buf);
                        let mut printed = self.thinking_printed.write().unwrap();
                        *printed = true;
                    }
                }
                // Run text through the think filter state machine
                self.filter_and_print(&chunk.content);
            }
            _ => {}
        }
    }

    /// Run text through the think filter state machine and print to stderr.
    /// Thinking content is wrapped with ANSI dim codes; tag markers are stripped.
    /// False-alarm recovery correctly replays buffered characters.
    fn filter_and_print(&self, text: &str) {
        let mut state = self.think_filter_state.write().unwrap();
        let mut buf = self.think_filter_buf.write().unwrap();
        let mut dim = self.think_dim_active.write().unwrap();

        for c in text.chars() {
            let (new_state, actions) = state.process(c, &mut buf);
            *state = new_state;

            for action in actions {
                match action {
                    ThinkFilterAction::Print(ch) => {
                        // If dim is active, close it before printing normal text
                        if *dim {
                            eprint!("{}", ANSI_RESET);
                            *dim = false;
                        }
                        eprint!("{}", ch);
                    }
                    ThinkFilterAction::Think(ch) => {
                        // If dim is not active, open it before printing thinking text
                        if !*dim {
                            eprint!("{}", ANSI_DIM);
                            *dim = true;
                        }
                        eprint!("{}", ch);
                    }
                    ThinkFilterAction::Suppress => {
                        // Part of a tag -- do not print
                    }
                    ThinkFilterAction::FlushNormal(flushed) => {
                        // False alarm: replay buffered characters as normal text
                        if *dim {
                            eprint!("{}", ANSI_RESET);
                            *dim = false;
                        }
                        eprint!("{}", flushed);
                    }
                    ThinkFilterAction::FlushThink(flushed) => {
                        // False alarm inside think block: replay as thinking text
                        if !*dim {
                            eprint!("{}", ANSI_DIM);
                            *dim = true;
                        }
                        eprint!("{}", flushed);
                    }
                    ThinkFilterAction::EnterThink => {
                        // Entering thinking block -- open dim styling
                        if !*dim {
                            eprint!("{}", ANSI_DIM);
                            *dim = true;
                        }
                    }
                    ThinkFilterAction::ExitThink => {
                        // Exiting thinking block -- close dim styling
                        if *dim {
                            eprint!("{}", ANSI_RESET);
                            *dim = false;
                        }
                    }
                }
            }
        }

        // Flush any remaining partial tag in the buffer at end of stream
        if !buf.is_empty() {
            let remaining = buf.clone();
            buf.clear();
            match *state {
                ThinkFilterState::InThinkOpenTag => {
                    // Partial open tag at end of stream -- treat as normal text
                    if *dim {
                        eprint!("{}", ANSI_RESET);
                        *dim = false;
                    }
                    eprint!("{}", remaining);
                    *state = ThinkFilterState::Normal;
                }
                ThinkFilterState::InThinkCloseTag => {
                    // Partial close tag inside thinking block -- treat as thinking text
                    if !*dim {
                        eprint!("{}", ANSI_DIM);
                        *dim = true;
                    }
                    eprint!("{}", remaining);
                }
                _ => {}
            }
        }
    }

    fn flush_tool_call(&self) {
        let name = {
            let n = self.cur_tool_name.read().unwrap();
            if n.is_empty() {
                return;
            }
            n.clone()
        };

        let args = {
            let a = self.cur_tool_args.read().unwrap();
            a.clone()
        };

        // Show a summary of the tool args
        let summary = tool_arg_summary(&name, &args);
        if !summary.is_empty() {
            eprintln!("  [{}]: {}", name, summary);
        } else {
            eprintln!("  [{}]", name);
        }

        let mut n = self.cur_tool_name.write().unwrap();
        *n = String::new();
        let mut a = self.cur_tool_args.write().unwrap();
        a.clear();
    }
}

impl Default for TerminalHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// StallDetector monitors streaming for stalls
pub struct StallDetector {
    last_event: RwLock<Instant>,
    stall_timeout: RwLock<Duration>,
    startup_timeout: RwLock<Duration>,
    stall_count: RwLock<usize>,
}

impl StallDetector {
    pub fn new() -> Self {
        Self {
            last_event: RwLock::new(Instant::now()),
            stall_timeout: RwLock::new(Duration::from_secs(90)),
            startup_timeout: RwLock::new(Duration::from_secs(120)),
            stall_count: RwLock::new(0),
        }
    }

    /// Configure timeouts dynamically based on provider and context size.
    /// - Local providers: very long timeouts (effectively no stall detection)
    /// - Large contexts (>50K tokens): 240s stall, 300s startup
    /// - Very large contexts (>100K tokens): 300s stall, 360s startup
    /// - Default: 90s stall, 120s startup
    pub fn configure(&self, is_local: bool, context_tokens: usize) {
        let mut stall = self.stall_timeout.write().unwrap();
        let mut startup = self.startup_timeout.write().unwrap();
        if is_local {
            // Local providers can be very slow on cold start -- use very long timeouts
            *stall = Duration::from_secs(300);
            *startup = Duration::from_secs(600);
        } else if context_tokens > 100_000 {
            *stall = Duration::from_secs(300);
            *startup = Duration::from_secs(360);
        } else if context_tokens > 50_000 {
            *stall = Duration::from_secs(240);
            *startup = Duration::from_secs(300);
        }
        // else: keep defaults (90s / 120s)
    }

    /// Reset timer on successful event
    pub fn reset(&self) {
        let mut last = self.last_event.write().unwrap();
        *last = Instant::now();
        let mut count = self.stall_count.write().unwrap();
        *count = 0;
    }

    /// Check if stalled. Returns Some(duration) if stalled.
    #[allow(dead_code)]
    pub fn check_stall(&self) -> Option<Duration> {
        let last = *self.last_event.read().unwrap();
        let stall = self.stall_timeout.read().unwrap();
        let elapsed = last.elapsed();
        if elapsed > *stall {
            Some(elapsed)
        } else {
            None
        }
    }

    /// Increment stall count and return count
    #[allow(dead_code)]
    pub fn increment_stall(&self) -> usize {
        let mut count = self.stall_count.write().unwrap();
        *count += 1;
        *count
    }

    /// Get stall timeout based on whether first event has been received
    #[allow(dead_code)]
    pub fn timeout(&self) -> Duration {
        let last = *self.last_event.read().unwrap();
        let startup = self.startup_timeout.read().unwrap();
        let stall = self.stall_timeout.read().unwrap();
        if last.elapsed() < *startup {
            // Use startup timeout until first event
            *startup
        } else {
            *stall
        }
    }
}

impl Default for StallDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// ProcessSseEvents processes SSE events from the Anthropic API.
/// Retries on transient errors and returns partial results on failure.
///
/// Retry strategy (matching hermes-agent):
/// - No deltas sent yet: clean retry, accumulators untouched
/// - Deltas sent + tool call in-flight: clear partial tool, retry with marker
/// - Deltas sent (text only): return partial stub (can't retry without duplicating text)
pub async fn process_sse_events(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    max_tokens: i64,
    system: &str,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    collect: &CollectHandler,
    term: &TerminalHandler,
    stall: &Arc<StallDetector>,
    interrupted: Arc<std::sync::atomic::AtomicBool>,
    rate_state: &RateLimitState,
) -> Result<StreamResult> {
    // Check for interruption before starting
    if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(anyhow!("Request cancelled by user"));
    }

    // Configure stall detector based on provider and context size
    let estimated_tokens = estimate_message_tokens(messages) + system.len() / 4;
    let is_local = is_local_endpoint(base_url);
    stall.configure(is_local, estimated_tokens);

    // Build request payload (reusable across retries)
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_string(), serde_json::json!(model));
    payload.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
    let mut sys_arr = serde_json::json!([{"type": "text", "text": system}]);
    cache_system_prompt(&mut sys_arr);
    payload.insert("system".to_string(), sys_arr);
    payload.insert("messages".to_string(), serde_json::json!(messages));
    if !tools.is_empty() {
        payload.insert("tools".to_string(), serde_json::json!(tools));
    }

    let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let mut retry = 0;
    // Track what was already delivered to the user so we can decide
    // whether retry is safe or would cause duplication.
    let mut deltas_state = DeltasState::None; // tracks: none, text_only, tool_in_flight

    loop {
        // Check for interruption before each attempt
        if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
            return partial_result(collect, false);
        }

        // Create cancellation token for this attempt
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel_token.clone();
        let interrupted_clone = interrupted.clone();
        let cancel_guard = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                if interrupted_clone.load(std::sync::atomic::Ordering::SeqCst) {
                    cancel_clone.cancel();
                    return;
                }
            }
        });

        // Race HTTP send against cancellation
        let response = tokio::select! {
            resp = client
                .post(&url)
                .header("x-api-key", api_key)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("anthropic-version", "2023-06-01")
                .body(serde_json::to_string(&payload).unwrap())
                .send() => {
                    match resp {
                        Ok(r) => r,
                        Err(e) => {
                            cancel_guard.abort();
                            if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                                return partial_result(collect, false);
                            }
                            let err_str = e.to_string();
                            if is_transient_error(&err_str) && retry < MAX_STREAM_RETRIES {
                                retry += 1;
                                eprintln!("[!] Stream connection failed (attempt {}/{}), reconnecting...", retry, MAX_STREAM_RETRIES);
                                stall.reset();
                                // Clear accumulated state before retry -- the API will send
                                // a completely new response with new tool IDs on reconnect,
                                // so old collected data would have mismatched IDs.
                                collect.clear_all();
                                continue;
                            }
                            // Non-transient or retries exhausted
                            return partial_result(collect, false);
                        }
                    }
                }
            _ = cancel_token.cancelled() => {
                cancel_guard.abort();
                return partial_result(collect, false);
            }
        };

        cancel_guard.abort();

        // Capture rate limit headers from response
        if let Some(rl) = parse_rate_limit_headers(response.headers(), "") {
            rate_state.update(&rl);
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            // Check if streaming is not supported (switch to non-streaming upstream)
            if body.contains("stream") && body.contains("not supported") {
                return Err(anyhow!("streaming not supported by this provider"));
            }
            return Err(anyhow!("API error {}: {}", status, body));
        }

        let mut stream = response.bytes_stream();
        let mut sse_detected = false;

        // Stream processing loop with stall timeout
        loop {
            // Check for interruption during streaming
            if interrupted.load(std::sync::atomic::Ordering::SeqCst) {
                return partial_result(collect, false);
            }

            // Stall timeout: race next chunk against timeout
            let timeout_dur = stall.timeout();
            let chunk = tokio::select! {
                result = stream.next() => result,
                _ = tokio::time::sleep(timeout_dur) => {
                    if retry < MAX_STREAM_RETRIES {
                        retry += 1;
                        eprintln!("[!] Stream stalled for {:?}, reconnecting (attempt {}/{})...",
                            timeout_dur, retry, MAX_STREAM_RETRIES);
                        stall.reset();
                        // Clear accumulated state before retry -- the API will send
                        // a completely new response with new tool IDs on reconnect.
                        collect.clear_all();
                        drop(stream);
                        break; // retry outer loop
                    }
                    // Retries exhausted, return partial
                    return partial_result(collect, false);
                }
            };

            let bytes = match chunk {
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    let err_str = e.to_string();
                    // Transient error: retry if we haven't exceeded limit
                    if is_transient_error(&err_str) && retry < MAX_STREAM_RETRIES {
                        retry += 1;
                        stall.reset();

                        // Clear accumulated state before retry -- the API will send
                        // a completely new response with new tool IDs on reconnect.
                        collect.clear_all();

                        // Decide retry strategy based on what was already sent
                        match &deltas_state {
                            DeltasState::None => {
                                // Nothing sent yet -- clean retry
                                eprintln!("[!] Stream error (attempt {}/{}), reconnecting...", retry, MAX_STREAM_RETRIES);
                            }
                            DeltasState::ToolInFlight(_) => {
                                eprintln!("\n  [!] Connection dropped mid-tool-call; reconnecting (attempt {}/{})...", retry, MAX_STREAM_RETRIES);
                            }
                            DeltasState::TextOnly => {
                                // Text already streamed to user -- can't retry without duplication
                                eprintln!("\n  [!] Stream interrupted after text output, returning partial result...");
                                return partial_result(collect, false);
                            }
                        }
                        break; // retry outer loop
                    }
                    // Non-transient or retries exhausted: return partial results
                    return partial_result(collect, false);
                }
                None => {
                    // Stream ended normally
                    break;
                }
            };

            // Reset stall tracking on each event
            stall.reset();

            let raw = String::from_utf8_lossy(&bytes);

            // Try to detect non-SSE JSON response (raw Anthropic message format)
            if !sse_detected {
                let trimmed = raw.trim();
                if trimmed.starts_with('{') && !trimmed.starts_with("data:") && !trimmed.starts_with("event:") {
                    // Non-SSE JSON - parse as complete message and return
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        parse_anthropic_message(&msg, collect, term);
                        return partial_result(collect, true);
                    }
                }
                sse_detected = true;
            }

            // Process bytes as SSE -- accumulate raw bytes to handle multi-byte UTF-8 correctly.
            // Using `buf.push(b as char)` would corrupt multi-byte UTF-8 sequences (M-10).
            let mut byte_buf: Vec<u8> = Vec::new();
            for b in bytes {
                if b == b'\n' {
                    // Convert accumulated bytes to a string (handles multi-byte UTF-8)
                    let line_bytes = byte_buf.trim_ascii().to_vec();
                    byte_buf.clear();

                    if line_bytes.is_empty() {
                        continue;
                    }

                    let line = String::from_utf8_lossy(&line_bytes).to_string();

                    // Parse SSE line: "data: <json>" or "data:<json>"
                    let data = line.strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"));
                    if let Some(data) = data {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                            // Handle proxy returning complete message as single SSE event
                            // (type="message" with full content array instead of incremental events)
                            if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
                                if event_type == "message" {
                                    parse_anthropic_message(&event, collect, term);
                                    return partial_result(collect, true);
                                }

                                // Extract stop_reason from message_delta
                                if event_type == "message_delta" {
                                    if let Some(delta) = event.get("delta") {
                                        if let Some(stop_reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                                            collect.set_finish_reason(stop_reason.to_string());
                                        }
                                    }
                                }
                            }

                            if let Some(chunk) = parse_sse_event(&event) {
                                // Track delta state: what type of content was delivered
                                match &chunk.chunk_type {
                                    ChunkType::ToolCall => {
                                        // A tool call started -- track it as in-flight
                                        deltas_state = DeltasState::ToolInFlight(chunk.id.clone());
                                    }
                                    ChunkType::Text if matches!(deltas_state, DeltasState::None) => {
                                        // First text delta, no tool call yet
                                        deltas_state = DeltasState::TextOnly;
                                    }
                                    _ => {}
                                }

                                collect.handle(chunk.clone());
                                term.handle(chunk.clone());

                                if collect.is_tool_use_as_text() {
                                    return Err(anyhow!("model confused: echoed tool syntax as text"));
                                }
                            }
                        }
                    }
                } else if b != b'\r' {
                    byte_buf.push(b);
                }
            }
        }

        // If we get here, stream ended normally (None from stream.next())
        // Signal end of stream
        term.handle(StreamChunk {
            chunk_type: ChunkType::Done,
            content: String::new(),
            id: None,
            name: None,
            usage: None,
        });

        return partial_result(collect, true);
    }
}

/// Tracks what content was already streamed to the user, used to decide
/// whether a retry is safe or would cause text duplication.
#[derive(Debug, Clone)]
enum DeltasState {
    /// No deltas sent yet -- clean retry is safe
    None,
    /// Text was already streamed -- retry would duplicate text
    TextOnly,
    /// A tool call started with this ID but may be incomplete
    ToolInFlight(Option<String>),
}

/// Estimate total message tokens (rough: ~4 chars per token).
/// Used to configure stall timeout for large contexts.
fn estimate_message_tokens(messages: &[serde_json::Value]) -> usize {
    let mut total_chars = 0;
    for msg in messages {
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    total_chars += text.len();
                }
                // Tool results and tool_use blocks are smaller in token count
            }
        }
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            total_chars += text.len();
        }
    }
    total_chars / 4 // rough estimate: ~4 chars per token
}

/// Detect if the base_url points to a local provider (localhost, 127.0.0.1, etc.)
fn is_local_endpoint(base_url: &str) -> bool {
    let lower = base_url.to_lowercase();
    lower.contains("localhost")
        || lower.contains("127.0.0.1")
        || lower.contains("0.0.0.0")
        || lower.contains("::1")
        || lower.contains("local")
}

/// Build a StreamResult from the CollectHandler.
/// `completed` is true when the stream ended normally, false when partial results
/// are returned after a failure -- this lets the agent loop distinguish success from failure.
fn partial_result(collect: &CollectHandler, completed: bool) -> Result<StreamResult> {
    Ok(StreamResult {
        tool_calls: collect.tool_calls(),
        text: collect.full_response(),
        thinking: collect.thinking(),
        completed,
        finish_reason: collect.finish_reason(),
    })
}

/// Parse a complete Anthropic message JSON (non-streaming format) and extract tool calls/text
pub fn parse_anthropic_message(
    msg: &serde_json::Value,
    collect: &CollectHandler,
    term: &TerminalHandler,
) -> Vec<ToolCallInfo> {
    if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
        for block in content {
            if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                match block_type {
                    "thinking" => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            let preview = truncate_at(thinking.lines().next().unwrap_or(""), 120);
                            eprintln!("\n[THINK] {}", preview);
                        }
                    }
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            // Push text as a text chunk
                            collect.handle(StreamChunk {
                                chunk_type: ChunkType::Text,
                                content: text.to_string(),
                                id: None,
                                name: None,
                                usage: None,
                            });
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name)) = (
                            block.get("id").and_then(|i| i.as_str()),
                            block.get("name").and_then(|n| n.as_str()),
                        ) {
                            let args = block.get("input").map(|i| i.to_string()).unwrap_or_default();
                            // Push tool call chunk - send to collect first, then term (same as SSE path)
                            let tool_call_chunk = StreamChunk {
                                chunk_type: ChunkType::ToolCall,
                                content: String::new(),
                                id: Some(id.to_string()),
                                name: Some(name.to_string()),
                                usage: None,
                            };
                            collect.handle(tool_call_chunk.clone());
                            term.handle(tool_call_chunk);
                            // Push arguments chunk
                            let args_chunk = StreamChunk {
                                chunk_type: ChunkType::ToolArgument,
                                content: args,
                                id: None,
                                name: None,
                                usage: None,
                            };
                            collect.handle(args_chunk.clone());
                            term.handle(args_chunk);
                            // Push block stop to flush
                            let block_stop = StreamChunk {
                                chunk_type: ChunkType::BlockStop,
                                content: String::new(),
                                id: None,
                                name: None,
                                usage: None,
                            };
                            term.handle(block_stop);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    collect.tool_calls()
}

/// Parse an SSE data event into a StreamChunk
pub fn parse_sse_event(event: &serde_json::Value) -> Option<StreamChunk> {
    // Handle different event types from Anthropic SSE
    if let Some(gamma) = event.get("type").and_then(|v| v.as_str()) {
        match gamma {
            "message_start" => {
                // Message started
                Some(StreamChunk {
                    chunk_type: ChunkType::BlockStop,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            "content_block_start" => {
                let content_block = event.get("content_block")?;
                let block_type = content_block.get("type")?.as_str()?;
                if block_type == "tool_use" {
                    let id = content_block.get("id")?.as_str()?.to_string();
                    let name = content_block.get("name")?.as_str()?.to_string();
                    Some(StreamChunk {
                        chunk_type: ChunkType::ToolCall,
                        content: String::new(),
                        id: Some(id),
                        name: Some(name),
                        usage: None,
                    })
                } else if block_type == "thinking" {
                    Some(StreamChunk {
                        chunk_type: ChunkType::Thinking,
                        content: String::new(),
                        id: None,
                        name: None,
                        usage: None,
                    })
                } else {
                    None
                }
            }
            "content_block_delta" => {
                let delta = event.get("delta")?;
                let delta_type = delta.get("type")?.as_str()?;
                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::Text,
                            content: text,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    "input_json_delta" => {
                        let partial = delta.get("partial_json")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::ToolArgument,
                            content: partial,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    "thinking_delta" => {
                        let thinking = delta.get("thinking")?.as_str()?.to_string();
                        Some(StreamChunk {
                            chunk_type: ChunkType::Thinking,
                            content: thinking,
                            id: None,
                            name: None,
                            usage: None,
                        })
                    }
                    _ => None,
                }
            }
            "content_block_stop" => {
                Some(StreamChunk {
                    chunk_type: ChunkType::BlockStop,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    Some(StreamChunk {
                        chunk_type: ChunkType::Usage,
                        content: String::new(),
                        id: None,
                        name: None,
                        usage: Some(Usage {
                            input_tokens: input_tokens,
                            output_tokens: output_tokens,
                        }),
                    })
                } else {
                    None
                }
            }
            "message_stop" => {
                Some(StreamChunk {
                    chunk_type: ChunkType::Done,
                    content: String::new(),
                    id: None,
                    name: None,
                    usage: None,
                })
            }
            _ => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_stream_progress_ttfb() {
        let mut progress = StreamProgress::new();

        // Before any chunk is recorded, ttfb_ms should be None
        assert!(progress.ttfb_ms().is_none());

        // After recording a chunk, ttfb_ms should be Some with a value > 0
        progress.record_chunk(10);
        let ttfb = progress.ttfb_ms().expect("ttfb_ms should return Some after record_chunk");
        // TTFB should be >= 0 (practically 0 since Instant was just created,
        // but we only assert it is not negative which is guaranteed by u64)
        assert!(ttfb < 5000, "ttfb_ms should be reasonable, got {}", ttfb);
    }

    #[test]
    fn test_stream_progress_throughput() {
        let mut progress = StreamProgress::new();

        // Record some chunks so tokens accumulate
        progress.record_chunk(20); // 5 tokens (20 / 4, rounded up)
        progress.record_chunk(12); // 3 tokens (12 / 4, rounded up)

        // Give a tiny bit of time so elapsed > 0
        thread::sleep(std::time::Duration::from_millis(10));

        let tps = progress.tokens_per_second();
        assert!(tps > 0.0, "tokens_per_second should be > 0 after recording tokens, got {}", tps);
    }

    #[test]
    fn test_stream_progress_zero_tokens() {
        let progress = StreamProgress::new();

        // No chunks recorded -- elapsed may be 0 or near-0, tokens_per_second should be 0.0
        // because tokens_received is 0
        let tps = progress.tokens_per_second();
        assert_eq!(tps, 0.0, "tokens_per_second should be 0.0 with no tokens recorded, got {}", tps);
    }

    #[test]
    fn test_stream_progress_record_chunk() {
        let mut progress = StreamProgress::new();

        progress.record_chunk(10);
        assert_eq!(progress.chars_received, 10);
        assert_eq!(progress.tokens_received, 10usize.div_ceil(4)); // 3 tokens

        progress.record_chunk(7);
        assert_eq!(progress.chars_received, 17, "chars_received should accumulate");
        assert_eq!(progress.tokens_received, 10usize.div_ceil(4) + 7usize.div_ceil(4), "tokens_received should accumulate");
    }

    #[test]
    fn test_stream_progress_record_tool_call() {
        let mut progress = StreamProgress::new();

        assert_eq!(progress.tool_calls_received, 0);

        progress.record_tool_call();
        assert_eq!(progress.tool_calls_received, 1);

        progress.record_tool_call();
        progress.record_tool_call();
        assert_eq!(progress.tool_calls_received, 3, "tool_calls_received should increment each time");
    }

    // --- ThinkFilter state machine tests ---

    fn collect_actions(input: &str) -> Vec<ThinkFilterAction> {
        let mut state = ThinkFilterState::Normal;
        let mut buf = String::new();
        let mut all_actions = Vec::new();

        for c in input.chars() {
            let (new_state, actions) = state.process(c, &mut buf);
            state = new_state;
            all_actions.extend(actions);
        }

        // Flush any remaining partial tag in the buffer (end-of-stream handling)
        if !buf.is_empty() {
            let remaining = buf.clone();
            buf.clear();
            match state {
                ThinkFilterState::InThinkOpenTag => {
                    all_actions.push(ThinkFilterAction::FlushNormal(remaining));
                    state = ThinkFilterState::Normal;
                }
                ThinkFilterState::InThinkCloseTag => {
                    all_actions.push(ThinkFilterAction::FlushThink(remaining));
                }
                _ => {}
            }
        }

        all_actions
    }

    #[test]
    fn test_think_filter_long_tag() {
        // <thinking>...content...</thinking> should produce:
        // - Suppress for the tags
        // - EnterThink after open tag
        // - Think for content
        // - ExitThink after close tag
        let input = "<thinking>Hello world</thinking>";
        let actions = collect_actions(input);

        // Collect the output (what would be printed)
        let mut output = String::new();
        let mut think_content = String::new();
        let mut entered_think = false;
        let mut exited_think = false;

        for a in &actions {
            match a {
                ThinkFilterAction::Print(c) => output.push(*c),
                ThinkFilterAction::Think(c) => think_content.push(*c),
                ThinkFilterAction::EnterThink => entered_think = true,
                ThinkFilterAction::ExitThink => exited_think = true,
                ThinkFilterAction::Suppress => {}
                ThinkFilterAction::FlushNormal(s) => output.push_str(s),
                ThinkFilterAction::FlushThink(s) => think_content.push_str(s),
            }
        }

        assert!(entered_think, "Should enter think block");
        assert!(exited_think, "Should exit think block");
        assert_eq!(output, "");
        assert_eq!(think_content, "Hello world");
    }

    #[test]
    fn test_think_filter_short_tag() {
        // <think>...content</think> should work the same as long tags
        let input = "<think>I am thinking</think>";
        let actions = collect_actions(input);

        let mut think_content = String::new();
        let mut entered_think = false;
        let mut exited_think = false;

        for a in &actions {
            match a {
                ThinkFilterAction::Think(c) => think_content.push(*c),
                ThinkFilterAction::EnterThink => entered_think = true,
                ThinkFilterAction::ExitThink => exited_think = true,
                _ => {}
            }
        }

        assert!(entered_think, "Should enter think block with short tag");
        assert!(exited_think, "Should exit think block with short tag");
        assert_eq!(think_content, "I am thinking");
    }

    #[test]
    fn test_think_filter_false_alarm_recovery() {
        // <notathing> should NOT enter think block; the '<' should be replayed
        let input = "<notathing>hello";
        let actions = collect_actions(input);

        let mut output = String::new();
        for a in &actions {
            match a {
                ThinkFilterAction::Print(c) => output.push(*c),
                ThinkFilterAction::FlushNormal(s) => output.push_str(s),
                _ => {}
            }
        }

        assert_eq!(output, input, "Non-thinking tags should pass through unchanged");
    }

    #[test]
    fn test_think_filter_false_alarm_in_think_block() {
        // Inside a thinking block, <notathing> should NOT exit; should replay as thinking
        let input = "<thinking>foo<notathing>bar</thinking>";
        let actions = collect_actions(input);

        let mut think_content = String::new();
        for a in &actions {
            match a {
                ThinkFilterAction::Think(c) => think_content.push(*c),
                ThinkFilterAction::FlushThink(s) => think_content.push_str(s),
                _ => {}
            }
        }

        assert_eq!(think_content, "foo<notathing>bar", "False alarm inside think block should replay as thinking");
    }

    #[test]
    fn test_think_filter_mixed_text_and_thinking() {
        let input = "Hello <thinking>secret</thinking> world";
        let actions = collect_actions(input);

        let mut normal = String::new();
        let mut think = String::new();
        for a in &actions {
            match a {
                ThinkFilterAction::Print(c) => normal.push(*c),
                ThinkFilterAction::Think(c) => think.push(*c),
                ThinkFilterAction::FlushNormal(s) => normal.push_str(s),
                ThinkFilterAction::FlushThink(s) => think.push_str(s),
                _ => {}
            }
        }

        assert_eq!(normal, "Hello  world", "Normal text should pass through");
        assert_eq!(think, "secret", "Thinking content should be filtered");
    }

    #[test]
    fn test_think_filter_partial_tag_not_matched() {
        // <thi should not match; replay as normal text
        let input = "<thi";
        let actions = collect_actions(input);

        let mut output = String::new();
        for a in &actions {
            match a {
                ThinkFilterAction::Print(c) => output.push(*c),
                ThinkFilterAction::FlushNormal(s) => output.push_str(s),
                _ => {}
            }
        }

        assert_eq!(output, input, "Partial tag at end of stream should be flushed as normal");
    }

    #[test]
    fn test_think_filter_state_normal_to_inthinkblock() {
        let mut state = ThinkFilterState::Normal;
        let mut buf = String::new();

        // Process '<thinking>' character by character
        for c in "<thinking>".chars() {
            let (new_state, _) = state.process(c, &mut buf);
            state = new_state;
        }

        assert_eq!(state, ThinkFilterState::InThinkBlock);
    }

    #[test]
    fn test_think_filter_state_inthinkblock_to_normal() {
        let mut state = ThinkFilterState::InThinkBlock;
        let mut buf = String::new();

        // Process '</thinking>' character by character
        for c in "</thinking>".chars() {
            let (new_state, _) = state.process(c, &mut buf);
            state = new_state;
        }

        assert_eq!(state, ThinkFilterState::Normal);
    }

    #[test]
    fn test_think_filter_multiple_blocks() {
        let input = "A<thinking>1</thinking>B<thinking>2</thinking>C";
        let actions = collect_actions(input);

        let mut normal = String::new();
        let mut think = String::new();
        for a in &actions {
            match a {
                ThinkFilterAction::Print(c) => normal.push(*c),
                ThinkFilterAction::Think(c) => think.push(*c),
                ThinkFilterAction::FlushNormal(s) => normal.push_str(s),
                ThinkFilterAction::FlushThink(s) => think.push_str(s),
                _ => {}
            }
        }

        assert_eq!(normal, "ABC", "Multiple thinking blocks should yield correct normal text");
        assert_eq!(think, "12", "Multiple thinking blocks should yield all thinking content");
    }
}
