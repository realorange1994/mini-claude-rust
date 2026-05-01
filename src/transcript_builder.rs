use crate::context::ConversationContext;
use std::collections::HashMap;

/// Build a compact conversation transcript for the auto mode classifier.
/// Includes user messages (truncated) and tool calls, but NOT assistant text
/// (security requirement: agent must not influence classifier decisions).
pub fn build_compact_transcript(ctx: &ConversationContext, max_messages: usize) -> String {
    let max_messages = if max_messages == 0 { 20 } else { max_messages };

    let entries = ctx.entries();
    if entries.is_empty() {
        return String::new();
    }

    let start = entries.len().saturating_sub(max_messages);
    let recent = &entries[start..];

    let mut sb = String::new();
    for entry in recent {
        match &entry.content {
            crate::context::MessageContent::Text(text) => {
                if entry.role == crate::context::MessageRole::User {
                    let mut truncated = text.as_str();
                    if truncated.len() > 500 {
                        truncated = &truncated[..500];
                    }
                    sb.push_str(&format!("[User] {}\n", truncated));
                }
                // Skip assistant text (security: don't let agent influence classifier)
            }
            crate::context::MessageContent::ToolUseBlocks(blocks) => {
                for block in blocks {
                    let input_desc = format_tool_input_compact(
                        &block.name,
                        &block.input,
                    );
                    sb.push_str(&format!("[Tool: {}] {}\n", block.name, input_desc));
                }
            }
            crate::context::MessageContent::ToolResultBlocks(blocks) => {
                for r in blocks {
                    let content = extract_tool_result_text(&r.content);
                    let truncated = if content.len() > 100 {
                        &content[..100]
                    } else {
                        content.as_str()
                    };
                    sb.push_str(&format!("[Result] {}\n", truncated));
                }
            }
            // Skip CompactBoundaryContent, SummaryContent, AttachmentContent
            _ => {}
        }
    }

    sb
}

/// Extract plain text from tool result content blocks.
fn extract_tool_result_text(blocks: &[crate::context::ToolResultContent]) -> String {
    let parts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            crate::context::ToolResultContent::Text { text } => Some(text.as_str()),
        })
        .collect();
    parts.join(" ")
}

/// Format tool input in a compact form for the classifier transcript.
pub fn format_tool_input_compact(tool_name: &str, input: &HashMap<String, serde_json::Value>) -> String {
    match tool_name {
        "exec" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                if cmd.len() > 200 {
                    return format!("{}...", &cmd[..200]);
                }
                return cmd.to_string();
            }
        }
        "write_file" | "edit_file" | "read_file" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                return path.to_string();
            }
        }
        "grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
            return format!("{:?} in {}", pattern, path);
        }
        "glob" => {
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                return pattern.to_string();
            }
        }
        _ => {}
    }

    // Generic fallback
    let parts: Vec<String> = input
        .iter()
        .map(|(k, v)| {
            let s = format!("{}", v);
            if s.len() > 80 {
                format!("{}={}...", k, &s[..80])
            } else {
                format!("{}={}", k, s)
            }
        })
        .collect();
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::*;
    use crate::config::Config;

    fn make_test_context() -> ConversationContext {
        let config = Config::default();
        ConversationContext::new(config)
    }

    #[test]
    fn test_build_empty_transcript() {
        let ctx = make_test_context();
        let result = build_compact_transcript(&ctx, 20);
        assert!(result.is_empty());
    }

    #[test]
    fn test_build_transcript_with_user_messages() {
        let mut ctx = make_test_context();
        ctx.add_user_message("Hello world".to_string());
        ctx.add_assistant_text("Hi there".to_string());
        ctx.add_user_message("How are you?".to_string());

        let result = build_compact_transcript(&ctx, 20);
        assert!(result.contains("[User] Hello world"));
        assert!(result.contains("[User] How are you?"));
        // Assistant text should NOT appear
        assert!(!result.contains("Hi there"));
    }

    #[test]
    fn test_user_message_truncation() {
        let mut ctx = make_test_context();
        let long_msg = "x".repeat(600);
        ctx.add_user_message(long_msg.clone());

        let result = build_compact_transcript(&ctx, 20);
        assert!(result.contains("[User] "));
        // Should be truncated to 500 chars + "..."
        let user_line = result.lines().next().unwrap();
        assert!(user_line.len() < long_msg.len() + 20); // truncated with prefix
    }

    #[test]
    fn test_tool_use_appears_in_transcript() {
        let mut ctx = make_test_context();
        let mut input = HashMap::new();
        input.insert("command".to_string(), serde_json::json!("ls -la"));
        ctx.add_assistant_tool_calls(vec![ToolUseBlock {
            id: "tool-1".to_string(),
            name: "exec".to_string(),
            input,
        }]);
        ctx.add_tool_results(vec![ToolResultBlock {
            tool_use_id: "tool-1".to_string(),
            content: vec![ToolResultContent::Text { text: "file1.rs".to_string() }],
            is_error: false,
        }]);

        let result = build_compact_transcript(&ctx, 20);
        assert!(result.contains("[Tool: exec] ls -la"));
        assert!(result.contains("[Result] file1.rs"));
    }

    #[test]
    fn test_max_messages_limit() {
        let mut ctx = make_test_context();
        for i in 0..10 {
            ctx.add_user_message(format!("Message {}", i));
        }

        let result = build_compact_transcript(&ctx, 5);
        // Should only contain last 5 messages
        assert!(result.contains("[User] Message 9"));
        assert!(!result.contains("[User] Message 4"));
    }

    #[test]
    fn test_format_tool_input_compact_exec() {
        let mut input = HashMap::new();
        input.insert("command".to_string(), serde_json::json!("git status"));
        assert_eq!(format_tool_input_compact("exec", &input), "git status");
    }

    #[test]
    fn test_format_tool_input_compact_long_command() {
        let mut input = HashMap::new();
        let long_cmd = "x".repeat(300);
        input.insert("command".to_string(), serde_json::json!(long_cmd));
        let result = format_tool_input_compact("exec", &input);
        assert!(result.ends_with("..."));
        assert!(result.len() < 220); // ~200 + "..."
    }

    #[test]
    fn test_format_tool_input_compact_file_path() {
        let mut input = HashMap::new();
        input.insert("path".to_string(), serde_json::json!("src/main.rs"));
        assert_eq!(
            format_tool_input_compact("read_file", &input),
            "src/main.rs"
        );
    }

    #[test]
    fn test_extract_tool_result_text() {
        let blocks = vec![
            ToolResultContent::Text { text: "hello".to_string() },
            ToolResultContent::Text { text: "world".to_string() },
        ];
        assert_eq!(extract_tool_result_text(&blocks), "hello world");
    }
}
