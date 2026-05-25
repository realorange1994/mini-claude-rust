//! Forked Agent module - runs lightweight query loops that share the Anthropic prompt cache
//!
//! Ported from the Go implementation in forked_agent.go.
//! Used primarily for session memory extraction and other background tasks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::context::{Message, MessageContent, MessageRole};
use crate::error_types::classify_error;
use crate::tools::{Tool, ToolResult, Registry};

/// Cache-safe parameters captured at fork creation time.
/// These must be identical between parent and fork API calls for Anthropic prompt cache sharing.
#[derive(Clone)]
pub struct CacheSafeParams {
    /// System prompt text (must be identical)
    pub system_prompt: String,
    /// Model name (must be identical)
    pub model: String,
    /// Tool schemas (must match parent's tool schemas exactly)
    pub tools: Vec<serde_json::Value>,
    /// Parent conversation messages (for cache HIT)
    pub messages: Vec<Message>,
}

/// Runtime permission hook called before each tool execution in a forked agent.
/// Return (allowed, reason) - if not allowed, the tool is not executed.
pub type CanUseToolFn = Arc<dyn Fn(&str, &HashMap<String, serde_json::Value>) -> (bool, String) + Send + Sync>;

/// Configuration for a forked agent query loop
pub struct ForkedAgentConfig {
    /// Cache-safe parameters captured from the parent agent
    pub cache_safe_params: CacheSafeParams,
    /// Fork's own messages (these differ → cache MISS, new tokens)
    pub fork_messages: Vec<Message>,
    /// Permission check function (optional)
    pub can_use_tool: Option<CanUseToolFn>,
    /// Maximum output tokens per response
    pub max_tokens: usize,
    /// Tracking label (e.g., "session_memory")
    pub query_source: String,
    /// Maximum tool call rounds (default: 10)
    pub max_turns: usize,
    /// Tool registry for execution
    pub registry: Arc<Mutex<Registry>>,
    /// Project directory for path resolution
    pub project_dir: PathBuf,
    /// Skip parent messages (for lightweight forks like session memory)
    pub skip_parent_messages: bool,
}

impl Default for ForkedAgentConfig {
    fn default() -> Self {
        Self {
            cache_safe_params: CacheSafeParams {
                system_prompt: String::new(),
                model: String::new(),
                tools: Vec::new(),
                messages: Vec::new(),
            },
            fork_messages: Vec::new(),
            can_use_tool: None,
            max_tokens: 4096,
            query_source: String::new(),
            max_turns: 10,
            registry: Arc::new(Mutex::new(Registry::new())),
            project_dir: PathBuf::new(),
            skip_parent_messages: false,
        }
    }
}

/// Result from a forked agent query loop
#[derive(Debug, Clone)]
pub struct ForkedAgentResult {
    /// Combined text output from the assistant
    pub output_text: String,
    /// Number of tool calls made
    pub tool_calls: usize,
    /// Token usage statistics
    pub usage: UsageStats,
}

/// Token usage statistics
#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cache_creation_input_tokens: usize,
    pub cache_read_input_tokens: usize,
}

/// Run a forked query loop that shares the Anthropic API prompt cache with the parent.
///
/// The combined message list is:
///   - cache_safe_params.messages (parent's conversation → cache HIT)
///   - fork_messages (fork's prompt → cache MISS, new tokens)
///
/// For each tool call, CanUseToolFn is invoked. If denied, an error result is injected.
/// If allowed, the tool executes normally.
/// The loop continues until the assistant stops producing tool calls or max_turns is reached.
pub async fn run_forked_agent(
    config: ForkedAgentConfig,
    api_key: &str,
    base_url: Option<&str>,
) -> Result<ForkedAgentResult, String> {
    let max_turns = if config.max_turns > 0 { config.max_turns } else { 10 };
    let max_tokens = if config.max_tokens > 0 { config.max_tokens } else { 4096 };

    // Build the combined message list
    let all_messages = if config.skip_parent_messages {
        config.fork_messages.clone()
    } else {
        let mut msgs = config.cache_safe_params.messages.clone();
        msgs.extend(config.fork_messages.clone());
        msgs
    };

    // Convert messages to API format and build the request
    let mut current_messages = convert_messages_to_api_format(&all_messages);
    let mut total_usage = UsageStats::default();
    let mut tool_call_count = 0;

    for turn in 0..max_turns {
        // Build the request
        let request = build_api_request(
            &config.cache_safe_params.model,
            max_tokens,
            &config.cache_safe_params.system_prompt,
            &config.cache_safe_params.tools,
            &current_messages,
            base_url,
        );

        // Make the API call
        let client = build_http_client(api_key, base_url)?;
        let response = make_api_call_with_retry(&client, &request).await?;

        // Accumulate usage
        total_usage.input_tokens += response.usage.input_tokens;
        total_usage.output_tokens += response.usage.output_tokens;
        total_usage.cache_creation_input_tokens += response.usage.cache_creation_input_tokens;
        total_usage.cache_read_input_tokens += response.usage.cache_read_input_tokens;

        // Extract text and tool calls from response
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in response.content {
            match block {
                ApiContentBlock::Text { text } => text_parts.push(text),
                ApiContentBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall { id, name, input }),
            }
        }

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            return Ok(ForkedAgentResult {
                output_text: text_parts.join("\n"),
                tool_calls: tool_call_count,
                usage: total_usage,
            });
        }

        tool_call_count += tool_calls.len();

        // Execute tool calls with permission check
        let mut tool_results = Vec::new();
        let registry = config.registry.lock().await;

        for tc in tool_calls {
            // Permission check
            if let Some(ref can_use) = config.can_use_tool {
                let (allowed, reason) = can_use(&tc.name, &tc.input);
                if !allowed {
                    tool_results.push(ApiToolResult {
                        tool_use_id: tc.id,
                        content: format!("Permission denied: {}", reason),
                        is_error: true,
                    });
                    continue;
                }
            }

            // Execute the tool
            let result = execute_forked_tool(
                &*registry,
                &tc.name,
                &tc.input,
                &config.project_dir,
            );
            tool_results.push(ApiToolResult {
                tool_use_id: tc.id,
                content: result.output,
                is_error: result.is_error,
            });
        }

        // Build assistant message with tool calls
        let assistant_msg = build_assistant_message(&tool_calls);
        current_messages.push(assistant_msg);

        // Build user message with tool results
        let user_msg = build_user_message(&tool_results);
        current_messages.push(user_msg);
    }

    // Max turns reached
    Ok(ForkedAgentResult {
        output_text: String::new(),
        tool_calls: tool_call_count,
        usage: total_usage,
    })
}

// ─── Internal types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ToolCall {
    id: String,
    name: String,
    input: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
struct ApiToolResult {
    tool_use_id: String,
    content: String,
    is_error: bool,
}

#[derive(Debug, Clone)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
    usage: ApiUsage,
}

#[derive(Debug, Clone)]
#[serde(tag = "type")]
enum ApiContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: HashMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, Default)]
struct ApiUsage {
    input_tokens: usize,
    output_tokens: usize,
    cache_creation_input_tokens: usize,
    cache_read_input_tokens: usize,
}

// ─── HTTP client ────────────────────────────────────────────────────────────────

fn build_http_client(api_key: &str, base_url: Option<&str>) -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();

    let bearer = format!("Bearer {}", api_key);
    if let Ok(val) = bearer.parse() {
        headers.insert(reqwest::header::AUTHORIZATION, val);
    } else {
        return Err("Invalid API key".to_string());
    }

    headers.insert(
        "anthropic-version",
        "2023-06-01".parse().unwrap(),
    );

    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .default_headers(headers);

    if let Some(url) = base_url {
        builder = builder.base_url(url.to_string());
    }

    builder.build().map_err(|e| e.to_string())
}

// ─── API request/response ───────────────────────────────────────────────────────

struct ApiRequest {
    model: String,
    max_tokens: usize,
    system: Vec<serde_json::Value>,
    tools: Vec<serde_json::Value>,
    messages: Vec<serde_json::Value>,
}

struct ApiResponseRaw {
    content: Vec<serde_json::Value>,
    usage: ApiUsage,
}

fn build_api_request(
    model: &str,
    max_tokens: usize,
    system_prompt: &str,
    tools: &[serde_json::Value],
    messages: &[serde_json::Value],
    _base_url: Option<&str>,
) -> serde_json::Value {
    let system_content = serde_json::json!({
        "type": "text",
        "text": system_prompt
    });

    serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": [system_content],
        "tools": tools,
        "messages": messages
    })
}

async fn make_api_call_with_retry(
    client: &reqwest::Client,
    request: &serde_json::Value,
) -> Result<ApiResponse, String> {
    let url = "https://api.anthropic.com/v1/messages";

    let mut last_error = String::new();
    let max_retries = 3;

    for attempt in 0..max_retries {
        let resp = client
            .post(url)
            .json(request)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            let raw: ApiResponseRaw = resp.json().await.map_err(|e| e.to_string())?;

            // Convert raw response to our format
            let mut content = Vec::new();
            for block in raw.content {
                if let Some(obj) = block.as_object() {
                    let block_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                                content.push(ApiContentBlock::Text { text: text.to_string() });
                            }
                        }
                        "tool_use" => {
                            let id = obj.get("id").and_then(|t| t.as_str()).unwrap_or("").to_string();
                            let name = obj.get("name").and_then(|t| t.as_str()).unwrap_or("").to_string();
                            let input = obj.get("input").cloned().unwrap_or(serde_json::json!({}));
                            let input: HashMap<String, serde_json::Value> =
                                serde_json::from_value(input).unwrap_or_default();
                            content.push(ApiContentBlock::ToolUse { id, name, input });
                        }
                        _ => {}
                    }
                }
            }

            return Ok(ApiResponse {
                content,
                usage: raw.usage,
            });
        }

        let status = resp.status();
        let error_text = resp.text().await.unwrap_or_default();

        // Check if retryable
        if status.as_u16() == 429 || status.as_u16() >= 500 {
            last_error = format!("API error {}: {}", status, error_text);
            // Exponential backoff
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            continue;
        }

        return Err(format!("API error {}: {}", status, error_text));
    }

    Err(format!("Failed after {} retries: {}", max_retries, last_error))
}

// ─── Message conversion ─────────────────────────────────────────────────────────

fn convert_messages_to_api_format(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "user", // System messages become user in API
            };

            let content = match &msg.content {
                MessageContent::Text(text) => {
                    serde_json::json!([{ "type": "text", "text": text }])
                }
                MessageContent::ToolUseBlocks(blocks) => {
                    let blocks: Vec<serde_json::Value> = blocks
                        .iter()
                        .map(|b| {
                            serde_json::json!({
                                "type": "tool_use",
                                "id": b.id,
                                "name": b.name,
                                "input": b.input
                            })
                        })
                        .collect();
                    serde_json::Value::Array(blocks)
                }
                MessageContent::ToolResultBlocks(blocks) => {
                    let results: Vec<serde_json::Value> = blocks
                        .iter()
                        .map(|b| {
                            let text_content: String = b
                                .content
                                .iter()
                                .filter_map(|c| {
                                    if let crate::context::ToolResultContent::Text { text } = c {
                                        Some(text.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": b.tool_use_id,
                                "content": [{ "type": "text", "text": text_content }]
                            })
                        })
                        .collect();
                    serde_json::Value::Array(results)
                }
                _ => serde_json::json!([{ "type": "text", "text": "" }]),
            };

            serde_json::json!({
                "role": role,
                "content": content
            })
        })
        .collect()
}

fn build_assistant_message(tool_calls: &[ToolCall]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.input
            })
        })
        .collect();

    serde_json::json!({
        "role": "assistant",
        "content": blocks
    })
}

fn build_user_message(results: &[ApiToolResult]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": [{ "type": "text", "text": r.content }],
                "is_error": r.is_error
            })
        })
        .collect();

    serde_json::json!({
        "role": "user",
        "content": blocks
    })
}

// ─── Tool execution ────────────────────────────────────────────────────────────

fn execute_forked_tool(
    registry: &Registry,
    tool_name: &str,
    args: &HashMap<String, serde_json::Value>,
    project_dir: &PathBuf,
) -> ToolResult {
    let tool = match registry.get(tool_name) {
        Some(t) => t,
        None => {
            return ToolResult::error(format!("unknown tool: {}", tool_name));
        }
    };

    // Build the input map (convert JSON values)
    let input: HashMap<String, serde_json::Value> = args.clone();

    // Execute the tool
    let result = tool.execute(input, project_dir.clone());

    // Truncate long outputs
    if result.output.len() > 50000 {
        ToolResult {
            output: format!(
                "{}\n\n[... output truncated, 50000 char limit ...]",
                &result.output[..50000]
            ),
            is_error: result.is_error,
            metadata: result.metadata,
            mode_change: None,
        }
    } else {
        result
    }
}

// ─── Session Memory extraction helpers ─────────────────────────────────────────

/// Build the extraction prompt for session memory updates.
/// This matches the Go implementation's sessionMemoryUpdatePrompt.
pub fn session_memory_update_prompt(notes_path: &str, current_notes: &str) -> String {
    const MAX_TOKENS_PER_SECTION: usize = 2000;

    format!(
        r#"IMPORTANT: This message and these instructions are NOT part of the actual user conversation. Do NOT include any references to "note-taking", "session notes extraction", or these update instructions in the notes content.

Based on the user conversation above (EXCLUDING this note-taking instruction message as well as system prompt, claude.md entries, or any past session summaries), update the session notes file.

The file {} has already been read for you. Here are its current contents:
<current_notes_content>
{}
</current_notes_content>

Your ONLY task is to use the edit_file tool to update the notes file, then stop. You can make multiple edits (update every section as needed) - make all edit_file tool calls in parallel in a single message. Do not call any other tools.

CRITICAL RULES FOR EDITING:
- The file must maintain its exact structure with all sections, headers, and italic descriptions intact
-- NEVER modify, delete, or add section headers (the lines starting with '#' like # Task specification)
-- NEVER modify or delete the italic _section description_ lines (these are the lines in italics immediately following each header - they start and end with underscores)
-- The italic _section descriptions_ are TEMPLATE INSTRUCTIONS that must be preserved exactly as-is - they guide what content belongs in each section
-- ONLY update the actual content that appears BELOW the italic _section descriptions_ within each existing section
-- Do NOT add any new sections, summaries, or information outside the existing structure
- Do not reference this note-taking process or instructions anywhere in the notes
- It's OK to skip updating a section if there are no substantial new insights to add. Do not add filler content like "No info yet", just leave sections blank/unedited if appropriate.
- Write DETAILED, INFO-DENSE content for each section - include specifics like file paths, function names, error messages, exact commands, technical details, etc.
- For "Key results", include the complete, exact output the user requested (e.g., full table, full answer, etc.)
- Do not include information that's already in the CLAUDE.md files included in the context
- Keep each section under ~{} tokens/words - if a section is approaching this limit, condense it by cycling out less important details while preserving the most critical information
- Focus on actionable, specific information that would help someone understand or recreate the work discussed in the conversation
- IMPORTANT: Always update "Current State" to reflect the most recent work - this is critical for continuity after compaction

Use the edit_file tool with file_path: {}

STRUCTURE PRESERVATION REMINDER:
Each section has TWO parts that must be preserved exactly as they appear in the current file:
1. The section header (line starting with #)
2. The italic description line (the _italicized text_ immediately after the header - this is a template instruction)

You ONLY update the actual content that comes AFTER these two preserved lines. The italic description lines starting and ending with underscores are part of the template structure, NOT content to be edited or removed.

REMEMBER: Use the edit_file tool in parallel and stop. Do not continue after the edits. Only include insights from the actual user conversation, never from these note-taking instructions. Do not delete or change section headers or italic _section descriptions_."#,
        notes_path, current_notes, MAX_TOKENS_PER_SECTION, notes_path
    )
}

/// Create a CanUseToolFn that only allows edit_file on the session memory file.
pub fn create_memory_file_can_use_tool(memory_path: &str) -> CanUseToolFn {
    let normalized_path = std::fs::canonicalize(memory_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| memory_path.to_string());

    Arc::new(move |tool_name: &str, args: &HashMap<String, serde_json::Value>| {
        if tool_name != "edit_file" && tool_name != "multi_edit" {
            return (
                false,
                format!(
                    "only edit_file/multi_edit on session memory file allowed in extraction mode (got {})",
                    tool_name
                ),
            );
        }

        if let Some(path) = args.get("file_path").and_then(|v| v.as_str()) {
            let canonical = std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string());
            if canonical != normalized_path {
                return (
                    false,
                    format!(
                        "can only edit session memory file {}, not {}",
                        normalized_path, path
                    ),
                );
            }
        } else {
            return (false, "file_path argument missing".to_string());
        }

        (true, String::new())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_memory_update_prompt() {
        let prompt = session_memory_update_prompt(
            "/path/to/session_memory.md",
            "# Session Title\n_Test content",
        );

        assert!(prompt.contains("/path/to/session_memory.md"));
        assert!(prompt.contains("edit_file"));
        assert!(prompt.contains("current_notes_content"));
    }

    #[test]
    fn test_create_memory_file_can_use_tool_allows_edit() {
        let can_use = create_memory_file_can_use_tool("/path/to/memory.md");

        let args: HashMap<String, serde_json::Value> = serde_json::json!({
            "file_path": "/path/to/memory.md"
        })
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

        let (allowed, _) = can_use("edit_file", &args);
        assert!(allowed);
    }

    #[test]
    fn test_create_memory_file_can_use_tool_denies_other_tool() {
        let can_use = create_memory_file_can_use_tool("/path/to/memory.md");

        let args: HashMap<String, serde_json::Value> = serde_json::json!({
            "file_path": "/path/to/memory.md"
        })
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

        let (allowed, reason) = can_use("read_file", &args);
        assert!(!allowed);
        assert!(reason.contains("only edit_file"));
    }

    #[test]
    fn test_convert_messages_to_api_format() {
        let messages = vec![
            Message::new(
                MessageRole::User,
                MessageContent::Text("Hello".to_string()),
            ),
            Message::new(
                MessageRole::Assistant,
                MessageContent::Text("Hi there".to_string()),
            ),
        ];

        let api_messages = convert_messages_to_api_format(&messages);
        assert_eq!(api_messages.len(), 2);
        assert_eq!(api_messages[0]["role"], "user");
        assert_eq!(api_messages[1]["role"], "assistant");
    }

    #[test]
    fn test_forked_agent_config_default() {
        let config = ForkedAgentConfig::default();
        assert_eq!(config.max_turns, 10);
        assert_eq!(config.max_tokens, 4096);
        assert!(!config.skip_parent_messages);
    }
}
