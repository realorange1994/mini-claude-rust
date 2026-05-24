//! String utility functions.

/// Truncate a string to a maximum length, adding ellipsis if truncated.
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

/// Truncate a string to a maximum number of lines.
pub fn truncate_lines(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().take(max_lines).collect();
    let total_lines = s.lines().count();
    if total_lines <= max_lines {
        s.to_string()
    } else {
        format!("{}\n... ({} more lines)", lines.join("\n"), total_lines - max_lines)
    }
}

/// Indent each line of a string by the given number of spaces.
pub fn indent(s: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    s.lines()
        .map(|line| format!("{}{}", prefix, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove common leading whitespace from all lines (dedent).
pub fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.is_empty() {
        return s.to_string();
    }

    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|l| {
            if l.len() >= min_indent && !l.trim().is_empty() {
                &l[min_indent..]
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Join lines with proper newline handling.
pub fn join_lines(lines: &[&str]) -> String {
    lines.join("\n")
}

/// Check if a string is a valid identifier (alphanumeric + underscore).
pub fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.chars().next().unwrap().is_ascii_digit()
}

/// Pluralize a word based on count.
pub fn pluralize(count: usize, singular: &str, plural: Option<&str>) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        plural.unwrap_or(&format!("{}s", singular)).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_lines() {
        let s = "line1\nline2\nline3\nline4";
        let result = truncate_lines(s, 2);
        assert!(result.contains("2 more lines"));
    }

    #[test]
    fn test_indent() {
        let s = "hello\nworld";
        let result = indent(s, 4);
        assert_eq!(result, "    hello\n    world");
    }

    #[test]
    fn test_dedent() {
        let s = "    hello\n    world";
        let result = dedent(s);
        assert_eq!(result, "hello\nworld");
    }

    #[test]
    fn test_is_identifier() {
        assert!(is_identifier("hello"));
        assert!(is_identifier("hello_world"));
        assert!(!is_identifier("123hello"));
        assert!(!is_identifier("hello-world"));
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize(1, "item", None), "item");
        assert_eq!(pluralize(2, "item", None), "items");
        assert_eq!(pluralize(2, "mouse", Some("mice")), "mice");
    }
}
