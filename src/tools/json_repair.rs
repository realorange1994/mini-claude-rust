//! JSON repair utility for fixing common LLM-generated JSON errors.
//! Ported from upstream tools/json_repair.go (165 lines).
//!
//! Repairs attempted:
//!   1. Trailing commas before } or ]
//!   2. Single-quoted strings → double-quoted
//!   3. JS-style line comments (// ...)
//!   4. Unbalanced brackets (append missing closing brackets)
//!   5. Unescaped newlines inside string values

use regex::Regex;
use std::sync::LazyLock;

static TRAILING_COMMA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r",\s*([}\]])").unwrap());
static SINGLE_QUOTE_VAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\s*'([^']*)'").unwrap());
static SINGLE_QUOTE_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*'([^']*)'\s*:").unwrap());
static LINE_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*//.*$").unwrap());
static INLINE_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*//.*$").unwrap());

/// Attempt to fix common JSON formatting errors in tool call arguments.
/// Returns repaired JSON if any fix produces valid JSON, else original.
pub fn repair_json(s: &str) -> String {
    // If already valid, return as-is
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return s.to_string();
    }

    let repairs: &[fn(&str) -> String] = &[
        repair_trailing_commas,
        repair_single_quotes,
        repair_comments,
        repair_unbalanced_brackets,
        repair_unescaped_newlines,
    ];

    // Apply each repair individually; return first valid result
    for repair_fn in repairs {
        let fixed = repair_fn(s);
        if fixed != s && serde_json::from_str::<serde_json::Value>(&fixed).is_ok() {
            return fixed;
        }
    }

    // Apply all repairs combined
    let mut fixed = s.to_string();
    for repair_fn in repairs {
        fixed = repair_fn(&fixed);
    }
    if serde_json::from_str::<serde_json::Value>(&fixed).is_ok() {
        return fixed;
    }

    s.to_string()
}

/// Remove trailing commas before } or ].
fn repair_trailing_commas(s: &str) -> String {
    TRAILING_COMMA_RE.replace_all(s, "$1").to_string()
}

/// Replace single-quoted string values with double quotes.
fn repair_single_quotes(s: &str) -> String {
    // Replace single-quoted values: 'value' → "value"
    let result = SINGLE_QUOTE_VAL_RE.replace_all(s, ": \"$1\"");
    // Replace single-quoted keys at line start: 'key': → "key":
    SINGLE_QUOTE_KEY_RE
        .replace_all(&result, "\"$1\":")
        .to_string()
}

/// Remove // line comments from JSON.
fn repair_comments(s: &str) -> String {
    let mut out = Vec::new();
    for line in s.lines() {
        // Full-line comments
        if LINE_COMMENT_RE.is_match(line) {
            continue;
        }
        // Inline comments (simple heuristic: if line doesn't start with a quote,
        // strip // suffix)
        let mut processed = line.to_string();
        if !line.contains('"') || line.find('"').unwrap_or(usize::MAX) > line.find("//").unwrap_or(usize::MAX) {
            processed = INLINE_COMMENT_RE.replace_all(line, "").to_string();
        }
        out.push(processed);
    }
    out.join("\n")
}

/// Append missing closing brackets.
fn repair_unbalanced_brackets(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut in_string = false;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && in_string {
            i += 2; // skip escaped char
            continue;
        }
        if bytes[i] == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if in_string {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'{' | b'[' => stack.push(bytes[i]),
            b'}' => {
                if !stack.is_empty() && *stack.last().unwrap() == b'{' {
                    stack.pop();
                }
            }
            b']' => {
                if !stack.is_empty() && *stack.last().unwrap() == b'[' {
                    stack.pop();
                }
            }
            _ => {}
        }
        i += 1;
    }

    let mut result = s.to_string();
    for &bracket in stack.iter().rev() {
        match bracket {
            b'{' => result.push('}'),
            b'[' => result.push(']'),
            _ => {}
        }
    }
    result
}

/// Replace bare newlines and tabs inside string values with escaped versions.
fn repair_unescaped_newlines(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len() + 16);
    let mut in_string = false;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && in_string {
            result.push(bytes[i] as char);
            i += 1;
            if i < bytes.len() {
                result.push(bytes[i] as char);
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            in_string = !in_string;
            result.push('"');
            i += 1;
            continue;
        }
        if in_string && bytes[i] == b'\n' {
            result.push_str("\\n");
            i += 1;
            continue;
        }
        if in_string && bytes[i] == b'\t' {
            result.push_str("\\t");
            i += 1;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_json_unchanged() {
        let json = r#"{"key": "value"}"#;
        assert_eq!(repair_json(json), json);
    }

    #[test]
    fn test_trailing_comma() {
        let broken = r#"{"key": "value",}"#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn test_trailing_comma_array() {
        let broken = r#"[1, 2, 3,]"#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn test_single_quotes() {
        let broken = r#"{'key': 'value'}"#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn test_unbalanced_brackets() {
        let broken = r#"{"key": "value""#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn test_unescaped_newlines() {
        let broken = r#"{"text": "line1
line2"}"#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }

    #[test]
    fn test_line_comments() {
        let broken = r#"{
  // this is a comment
  "key": "value"
}"#;
        let fixed = repair_json(broken);
        assert!(serde_json::from_str::<serde_json::Value>(&fixed).is_ok());
    }
}
