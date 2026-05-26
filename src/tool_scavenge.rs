//! Tool Scavenge — extracts tool calls from malformed or non-standard model output.
//!
//! This matches DeepSeek-Reasonix's scavenge repair pattern: R1 models sometimes
//! emit tool-call JSON inside reasoning_content or forget to populate the tool_calls
//! field entirely. Without scavenge, the model's intended tool invocations are lost.
//!
//! Three scavenge patterns supported:
//!   1. DSML invoke blocks: <|DSML|invoke name="read_file">...</|DSML|invoke>
//!   2. Bare JSON objects: { "name": "read_file", "arguments": {...} }
//!   3. OpenAI-style: { "type": "function", "function": { "name": ..., "arguments": ... } }

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Input size cap to prevent polynomial redos on pathological text.
const MAX_INPUT: usize = 100_000;

/// Scavenge tool calls from assistant text content parts.
/// Returns additional tool calls to merge with properly structured ones.
pub fn scavenge_tool_calls(text_parts: &[String]) -> Vec<Map<String, Value>> {
    let mut scavenged: Vec<Map<String, Value>> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let full_text = text_parts.join("\n");
    if full_text.is_empty() {
        return scavenged;
    }

    // Cap input to prevent polynomial-redos
    let capped = if full_text.len() > MAX_INPUT {
        &full_text[..MAX_INPUT]
    } else {
        &full_text
    };

    // Phase 1: DSML invoke blocks
    for call in scavenge_dsml(capped) {
        let key = call_signature(&call);
        if seen.insert(key) {
            scavenged.push(call);
        }
    }

    // Phase 2: Raw JSON objects
    for call in scavenge_raw_json(capped) {
        let key = call_signature(&call);
        if seen.insert(key) {
            scavenged.push(call);
        }
    }

    scavenged
}

// DSML regexes
static DSML_INVOKE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"<[\|｜]DSML[\|｜]invoke\s+name="([^"]+)">([\s\S]*?)<[\|｜]/DSML[\|｜]invoke>"#)
        .unwrap()
});

static DSML_PARAM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"<[\|｜]DSML[\|｜]parameter\s+name="([^"]+)"(?:\s+string="(true|false)")?\s*>([\s\S]*?)<[\|｜]/DSML[\|｜]parameter>"#)
        .unwrap()
});

/// Phase 1: Scavenge DSML invoke blocks from text.
fn scavenge_dsml(text: &str) -> Vec<Map<String, Value>> {
    let mut calls = Vec::new();

    for cap in DSML_INVOKE_RE.captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let body = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        let mut input = Map::new();

        // Parse parameters
        let param_matches: Vec<_> = DSML_PARAM_RE.captures_iter(body).collect();
        for param_cap in &param_matches {
            let param_name = param_cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            let string_flag = param_cap.get(2).map(|m| m.as_str()).unwrap_or("true");
            let raw = param_cap.get(3).map(|m| m.as_str()).unwrap_or("");

            if string_flag == "false" {
                // Try JSON parse, fall back to literal
                if let Ok(val) = serde_json::from_str::<Value>(raw) {
                    input.insert(param_name, val);
                    continue;
                }
            }
            input.insert(param_name, Value::String(raw.to_string()));
        }

        // If no parameters found, try parsing body as a single JSON value
        if input.is_empty() {
            if let Ok(val) = serde_json::from_str::<Value>(body) {
                if let Value::Object(obj) = val {
                    input = obj;
                }
            }
        }

        let mut call = Map::new();
        call.insert("name".to_string(), Value::String(name));
        call.insert("input".to_string(), Value::Object(input));
        calls.push(call);
    }

    calls
}

/// Phase 2: Scavenge bare JSON objects that look like tool calls.
fn scavenge_raw_json(text: &str) -> Vec<Map<String, Value>> {
    let mut calls = Vec::new();

    // Pattern 1: { "name": "tool", "arguments": {...} }
    // Match outer braces with balanced nesting for arguments
    let pattern1_re = Regex::new(
        r#"\{\s*"name"\s*:\s*"([^"]+)"\s*,\s*"arguments"\s*:\s*(\{[^{}]*\})\s*\}"#
    )
    .unwrap();
    for cap in pattern1_re.captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let args = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if let Ok(Value::Object(input)) = serde_json::from_str::<Value>(args) {
            let mut call = Map::new();
            call.insert("name".to_string(), Value::String(name.to_string()));
            call.insert("input".to_string(), Value::Object(input));
            calls.push(call);
        }
    }

    // Pattern 2: OpenAI-style
    // { "type": "function", "function": { "name": ..., "arguments": ... } }
    let pattern2_re = Regex::new(
        r#"\{\s*"type"\s*:\s*"function"\s*,\s*"function"\s*:\s*\{\s*"name"\s*:\s*"([^"]+)"\s*,\s*"arguments"\s*:\s*(\{[^{}]*\})\s*\}"#
    )
    .unwrap();
    for cap in pattern2_re.captures_iter(text) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let args = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if let Ok(Value::Object(input)) = serde_json::from_str::<Value>(args) {
            let mut call = Map::new();
            call.insert("name".to_string(), Value::String(name.to_string()));
            call.insert("input".to_string(), Value::Object(input));
            calls.push(call);
        }
    }

    calls
}

/// Generate a dedup key for a scavenged call.
fn call_signature(call: &Map<String, Value>) -> String {
    let name = call.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let input = call.get("input")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();
    format!("{}|{}", name, input)
}

/// Generate a synthetic tool_use_id for a scavenged call.
pub fn synthetic_id(name: &str, input: &Value) -> String {
    let args_json = serde_json::to_string(input).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(args_json.as_bytes());
    let hash = hasher.finalize();
    let hex_str: String = hash.iter().take(4).map(|b| format!("{:02x}", b)).collect();
    format!("scavenged_{}_{}", name, hex_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scavenge_dsml() {
        let text = r#"<|DSML|invoke name="read_file"><|DSML|parameter name="path" string="true">/tmp/test.txt<|/DSML|parameter></|DSML|invoke>"#;
        let calls = scavenge_dsml(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("name").and_then(|v| v.as_str()), Some("read_file"));
    }

    #[test]
    fn test_scavenge_raw_json_direct() {
        let text = r#"Here is some text {"name": "read_file", "arguments": {"path": "/tmp/test.txt"}} more text"#;
        let calls = scavenge_raw_json(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("name").and_then(|v| v.as_str()), Some("read_file"));
    }

    #[test]
    fn test_scavenge_raw_json_openai_style() {
        let text = r#"{"type": "function", "function": {"name": "grep", "arguments": {"pattern": "hello"}}}"#;
        let calls = scavenge_raw_json(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("name").and_then(|v| v.as_str()), Some("grep"));
    }

    #[test]
    fn test_scavenge_tool_calls_empty() {
        assert!(scavenge_tool_calls(&[]).is_empty());
        assert!(scavenge_tool_calls(&["".to_string()]).is_empty());
    }

    #[test]
    fn test_call_signature_dedup() {
        let mut call1 = Map::new();
        call1.insert("name".to_string(), Value::String("read_file".to_string()));
        call1.insert("input".to_string(), serde_json::json!({"path": "/tmp/test.txt"}));

        let mut call2 = call1.clone();

        let sig1 = call_signature(&call1);
        let sig2 = call_signature(&call2);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_input_cap() {
        // Long text beyond 100K chars should be capped
        let huge = "a".repeat(200_000);
        // Should not panic or hang
        let _ = scavenge_tool_calls(&[huge]);
    }
}