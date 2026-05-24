use regex::Regex;
use std::borrow::Cow;

/// OutputCleaner cleans terminal output by processing backspace characters,
/// ANSI escape sequences, and other control characters.
pub struct OutputCleaner;

impl OutputCleaner {
    /// Clean the output string by processing backspaces and ANSI escape sequences.
    pub fn clean(input: &str) -> String {
        let s = Self::process_backspaces(input);
        let s = Self::strip_ansi(&s);
        let s = Self::strip_carriage_returns(&s);
        Self::strip_control_chars(&s)
    }

    /// Process backspace characters (0x08), removing the preceding character.
    pub fn process_backspaces(input: &str) -> String {
        let mut result = Vec::new();
        for c in input.chars() {
            if c == '\x08' {
                result.pop();
            } else {
                result.push(c);
            }
        }
        result.into_iter().collect()
    }

    /// Strip ANSI escape sequences from the string.
    pub fn strip_ansi(input: &str) -> String {
        let re = Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
        let re2 = Regex::new(r"\x1b\][^\x07]*\x07").unwrap();
        let re3 = Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
        let s = re.replace_all(input, "");
        let s = re2.replace_all(&s, "");
        re3.replace_all(&s, "").into_owned()
    }

    /// Strip carriage return characters.
    pub fn strip_carriage_returns(input: &str) -> String {
        input.replace('\r', "")
    }

    /// Strip control characters except newline and tab.
    pub fn strip_control_chars(input: &str) -> String {
        input
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect()
    }

    /// Suppress think tags from the output.
    pub fn suppress_think(input: &str) -> Cow<str> {
        let re = Regex::new(r"<think[\s\S]*?</think\s*>").unwrap();
        re.replace_all(input, "")
    }

    /// Extract tool name from XML-like tags in the output.
    pub fn extract_tool_name(input: &str) -> Option<String> {
        let re = Regex::new(r"<([a-z_]+)>").unwrap();
        if let Some(caps) = re.captures(input) {
            if let Some(m) = caps.get(1) {
                return Some(m.as_str().to_string());
            }
        }
        None
    }

    /// Strip keypad mode sequences (like \x1b[?2004h and \x1b[?2004l).
    pub fn strip_keypad_mode(input: &str) -> String {
        let re = Regex::new(r"\x1b\[\?2004[hl]").unwrap();
        re.replace_all(input, "").into_owned()
    }

    /// Truncate output to a maximum length, adding an ellipsis if truncated.
    pub fn truncate_output(input: &str, max_len: usize) -> String {
        if input.len() <= max_len {
            return input.to_string();
        }
        format!("{}...(truncated)", &input[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_backspaces() {
        assert_eq!(OutputCleaner::process_backspaces("helllo\x08o"), "hello");
        assert_eq!(OutputCleaner::process_backspaces("test\x08\x08abc"), "abc");
        assert_eq!(OutputCleaner::process_backspaces("\x08\x08hello"), "hello");
        assert_eq!(OutputCleaner::process_backspaces("no backspace"), "no backspace");
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(OutputCleaner::strip_ansi("\x1b[32mhello\x1b[0m"), "hello");
        assert_eq!(OutputCleaner::strip_ansi("\x1b[1;31mred\x1b[0m bold"), "red bold");
    }

    #[test]
    fn test_strip_carriage_returns() {
        assert_eq!(OutputCleaner::strip_carriage_returns("hello\r\nworld"), "hello\nworld");
        assert_eq!(OutputCleaner::strip_carriage_returns("a\rb"), "ab");
    }

    #[test]
    fn test_suppress_think() {
        assert_eq!(OutputCleaner::suppress_think("<think\ninternal reasoning\n</think\nhello"), "hello");
        assert_eq!(OutputCleaner::suppress_think("no think tags"), "no think tags");
    }

    #[test]
    fn test_extract_tool_name() {
        assert_eq!(OutputCleaner::extract_tool_name("<read_file>"), Some("read_file".to_string()));
        assert_eq!(OutputCleaner::extract_tool_name("no tool"), None);
    }

    #[test]
    fn test_strip_keypad_mode() {
        assert_eq!(OutputCleaner::strip_keypad_mode("\x1b[?2004hhello\x1b[?2004l"), "hello");
    }

    #[test]
    fn test_truncate_output() {
        assert_eq!(OutputCleaner::truncate_output("hello", 10), "hello");
        assert_eq!(OutputCleaner::truncate_output("hello world", 5), "hello...(truncated)");
    }

    #[test]
    fn test_clean_full() {
        let input = "\x1b[32mhello\x1b[0m\x08o world\r\n";
        let cleaned = OutputCleaner::clean(input);
        assert!(cleaned.contains("hello world"));
    }
}
