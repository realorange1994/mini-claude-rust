/// Slices a string respecting ANSI escape sequences.
/// Ensures that ANSI escape sequences are not split in the middle.

/// Find the display width of a string, ignoring ANSI escape sequences
pub fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    re.replace_all(s, "").to_string()
}

/// Get the display width of a string (ignoring ANSI escapes)
pub fn display_width(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

/// Truncate a string to a maximum display width, preserving ANSI escapes
pub fn truncate_ansi(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    let mut visible_width = 0;

    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '[' {
            in_escape = true;
            result.push(chars[i]);
            i += 1;
            result.push(chars[i]);
            i += 1;
            while i < chars.len() && in_escape {
                result.push(chars[i]);
                if chars[i].is_ascii_alphabetic() {
                    in_escape = false;
                }
                i += 1;
            }
            continue;
        }

        if visible_width >= max_width {
            break;
        }

        result.push(chars[i]);
        visible_width += 1;
        i += 1;
    }

    if visible_width >= max_width && i < chars.len() {
        result.push_str("…");
    }

    result
}

/// Split string into lines, handling ANSI escape sequences
pub fn split_lines_ansi(s: &str) -> Vec<String> {
    s.lines().map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn test_strip_ansi_with_escape() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("\x1b[31mhello\x1b[0m"), 5);
    }

    #[test]
    fn test_truncate_ansi_plain() {
        assert_eq!(truncate_ansi("hello world", 5), "hello…");
    }

    #[test]
    fn test_truncate_ansi_with_escape() {
        let result = truncate_ansi("\x1b[31mhello world\x1b[0m", 5);
        assert!(result.contains("\x1b[31m"));
        assert!(result.contains("hello"));
    }
}
