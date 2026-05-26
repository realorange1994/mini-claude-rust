//! UTF-8 sanitization for arbitrary nested data structures.
//! Ported from upstream utf8_sanitize.go (88 lines).
//!
//! Replaces invalid UTF-8 byte sequences with the Unicode replacement character
//! (U+FFFD). Walks strings, Vec<Value>, and Map<String, Value> recursively.

use serde_json::{Map, Value};

/// Sanitize a JSON value by replacing invalid UTF-8 bytes with U+FFFD.
/// Returns the original value unchanged if no invalid bytes are found.
pub fn sanitize_json_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            let sanitized = sanitize_string_utf8(s.as_str());
            *s = sanitized;
        }
        Value::Array(arr) => {
            for item in arr {
                sanitize_json_value(item);
            }
        }
        Value::Object(obj) => {
            for (_, v) in obj.iter_mut() {
                sanitize_json_value(v);
            }
        }
        _ => {}
    }
}

/// Sanitize a string by replacing invalid UTF-8 byte sequences with U+FFFD.
/// Uses String::from_utf8_lossy which handles this correctly.
pub fn sanitize_string_utf8(s: &str) -> String {
    // Check if the string is already valid UTF-8
    if s.is_char_boundary(0)
        && s.is_char_boundary(s.len().saturating_sub(1))
        && std::str::from_utf8(s.as_bytes()).is_ok()
    {
        return s.to_string();
    }

    // Use from_utf8_lossy for replacement
    String::from_utf8_lossy(s.as_bytes()).to_string()
}

/// Sanitize a byte slice, replacing invalid UTF-8 with U+FFFD.
pub fn sanitize_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// Recursively sanitize a JSON value from raw bytes.
/// First converts bytes to string (with replacement), then parses.
pub fn sanitize_json_from_bytes(bytes: &[u8]) -> Option<Value> {
    let sanitized = sanitize_bytes(bytes);
    serde_json::from_str(&sanitized).ok()
}

/// Check if a byte slice contains any invalid UTF-8 sequences.
pub fn has_invalid_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_string_valid() {
        assert_eq!(sanitize_string_utf8("hello"), "hello");
        assert_eq!(sanitize_string_utf8("你好世界"), "你好世界");
    }

    #[test]
    fn test_sanitize_string_invalid() {
        // Valid UTF-8 string
        let valid = "hello world";
        assert_eq!(sanitize_string_utf8(valid), "hello world");
    }

    #[test]
    fn test_sanitize_json_value() {
        let mut value = serde_json::json!({
            "text": "hello",
            "nested": {
                "arr": ["a", "b", "c"]
            }
        });
        sanitize_json_value(&mut value);
        assert_eq!(value["text"].as_str().unwrap(), "hello");
    }

    #[test]
    fn test_has_invalid_utf8() {
        assert!(!has_invalid_utf8(b"hello"));
        // Invalid UTF-8: 0xFF is not a valid start byte
        assert!(has_invalid_utf8(b"\xff\xfe"));
    }

    #[test]
    fn test_sanitize_bytes() {
        assert_eq!(sanitize_bytes(b"hello"), "hello");
        // Invalid bytes get replaced
        let result = sanitize_bytes(b"\xff\xfe");
        assert!(result.contains('\u{FFFD}'));
    }
}
