//! Slice-aware ANSI string utilities.
//!
//! Handles strings containing ANSI escape sequences, ensuring
//! that slicing and truncation don't break escape sequences.

/// Find the visible (non-ANSI) length of a string.
pub fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            len += c.len_utf8();
        }
    }
    len
}

/// Truncate a string to a maximum visible length, preserving ANSI escape sequences.
pub fn truncate_ansi(s: &str, max_visible_len: usize) -> String {
    let mut result = String::new();
    let mut visible_count = 0;
    let mut in_escape = false;
    let mut escape_buf = String::new();

    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            escape_buf.clear();
            escape_buf.push(c);
        } else if in_escape {
            escape_buf.push(c);
            if c.is_ascii_alphabetic() {
                in_escape = false;
                result.push_str(&escape_buf);
                escape_buf.clear();
            }
        } else {
            if visible_count >= max_visible_len {
                break;
            }
            result.push(c);
            visible_count += 1;
        }
    }

    // Add reset sequence if we truncated
    if visible_count >= max_visible_len && result.contains("\x1b[") {
        result.push_str("\x1b[0m");
    }

    result
}

/// Strip all ANSI escape sequences from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;

    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Split a string into lines, preserving ANSI sequences within each line.
pub fn split_lines_ansi(s: &str) -> Vec<String> {
    s.split('\n').map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visible_len_plain() {
        assert_eq!(visible_len("hello"), 5);
    }

    #[test]
    fn test_visible_len_ansi() {
        assert_eq!(visible_len("\x1b[31mhello\x1b[0m"), 5);
    }

    #[test]
    fn test_truncate_ansi() {
        let result = truncate_ansi("\x1b[31mhello world\x1b[0m", 5);
        assert!(result.contains("\x1b[31m"));
        assert!(result.contains("\x1b[0m"));
        assert_eq!(visible_len(&result), 5);
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn test_split_lines_ansi() {
        let lines = split_lines_ansi("line1\nline2\nline3");
        assert_eq!(lines.len(), 3);
    }
}
