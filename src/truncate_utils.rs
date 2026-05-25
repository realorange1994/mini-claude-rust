//! Truncation utilities for output text.

/// Truncate text to a maximum number of bytes, trying to break at line boundaries.
pub fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    // Try to find a good break point at a newline
    let mut end = max_bytes;
    if let Some(pos) = text[..max_bytes].rfind('\n') {
        end = pos;
    }

    format!(
        "{}\n\n... (output truncated, {} bytes omitted)",
        &text[..end],
        text.len() - end
    )
}

/// Truncate text to a maximum number of lines.
pub fn truncate_by_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_string();
    }

    let kept: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    format!(
        "{}\n\n... ({} more lines)",
        kept.join("\n"),
        lines.len() - max_lines
    )
}

/// Truncate text to a maximum number of characters (Unicode-aware).
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated: String = text.chars().take(max_chars).collect();
    format!("{}...", truncated)
}

/// Smart truncate: truncates to the best boundary (line, then word, then char).
pub fn smart_truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    // Try line boundary first
    if let Some(pos) = text[..max_bytes].rfind('\n') {
        return format!(
            "{}\n\n... (truncated, {} bytes omitted)",
            &text[..pos],
            text.len() - pos
        );
    }

    // Try word boundary
    if let Some(pos) = text[..max_bytes].rfind(' ') {
        return format!(
            "{} ... (truncated, {} bytes omitted)",
            &text[..pos],
            text.len() - pos
        );
    }

    // Fall back to character boundary
    truncate_bytes(text, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_bytes_no_truncation() {
        assert_eq!(truncate_bytes("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_bytes_with_newline() {
        let text = "line1\nline2\nline3";
        let result = truncate_bytes(text, 12);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_truncate_by_lines() {
        let text = "line1\nline2\nline3\nline4";
        let result = truncate_by_lines(text, 2);
        assert!(result.contains("2 more lines"));
    }

    #[test]
    fn test_truncate_chars() {
        let result = truncate_chars("hello world", 5);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn test_smart_truncate() {
        let text = "hello world this is a test";
        let result = smart_truncate(text, 15);
        assert!(result.contains("truncated"));
    }
}
