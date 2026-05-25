//! Search configuration and result types for rgrep.
//! Ported from upstream tools/rgrep/config.go (51 lines).

use serde::{Deserialize, Serialize};

/// Output mode controls what the search returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// Output matching lines with file paths
    Content,
    /// Output only file paths that have matches
    FilesWithMatches,
    /// Output count of matches per file
    Count,
}

impl Default for OutputMode {
    fn default() -> Self {
        Self::FilesWithMatches
    }
}

/// Search configuration holding all search parameters.
#[derive(Debug, Clone, Default)]
pub struct SearchConfig {
    /// Regex pattern to search for
    pub pattern: String,
    /// File or directory to search in (default ".")
    pub path: String,
    /// Glob filter, e.g. "*.py"
    pub glob: String,
    /// Language type filter, e.g. "go", "py"
    pub type_filter: String,
    /// Case insensitive search
    pub case_insensitive: bool,
    /// Treat pattern as literal string
    pub fixed_strings: bool,
    /// What the search returns (default: files_with_matches)
    pub output_mode: OutputMode,
    /// Show line numbers in content mode
    pub show_line_nums: bool,
    /// Multiline regex mode
    pub multiline: bool,
    /// Lines before match
    pub context_before: usize,
    /// Lines after match
    pub context_after: usize,
    /// Max results (0 = unlimited)
    pub head_limit: usize,
    /// Skip first N results
    pub offset: usize,
    /// Max directory depth (0 = unlimited)
    pub max_depth: usize,
    /// Max file size in bytes (0 = unlimited)
    pub max_filesize: u64,
    /// Exclude patterns (glob, supports **)
    pub excludes: Vec<String>,
}

impl SearchConfig {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            show_line_nums: true,
            ..Default::default()
        }
    }

    pub fn with_path(mut self, path: &str) -> Self {
        self.path = path.to_string();
        self
    }

    pub fn with_glob(mut self, glob: &str) -> Self {
        self.glob = glob.to_string();
        self
    }

    pub fn with_type_filter(mut self, type_name: &str) -> Self {
        self.type_filter = type_name.to_string();
        self
    }

    pub fn case_insensitive(mut self) -> Self {
        self.case_insensitive = true;
        self
    }

    pub fn fixed_strings(mut self) -> Self {
        self.fixed_strings = true;
        self
    }

    pub fn output_mode(mut self, mode: OutputMode) -> Self {
        self.output_mode = mode;
        self
    }

    pub fn show_line_nums(mut self) -> Self {
        self.show_line_nums = true;
        self
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn context(mut self, before: usize, after: usize) -> Self {
        self.context_before = before;
        self.context_after = after;
        self
    }

    pub fn head_limit(mut self, n: usize) -> Self {
        self.head_limit = n;
        self
    }

    pub fn offset(mut self, n: usize) -> Self {
        self.offset = n;
        self
    }

    pub fn max_depth(mut self, n: usize) -> Self {
        self.max_depth = n;
        self
    }

    pub fn max_filesize(mut self, bytes: u64) -> Self {
        self.max_filesize = bytes;
        self
    }

    pub fn excludes(mut self, patterns: Vec<String>) -> Self {
        self.excludes = patterns;
        self
    }
}

/// A single match from the search.
#[derive(Debug, Clone)]
pub struct SearchResultEntry {
    /// Relative path
    pub path: String,
    /// 1-based line number (0 for files/count mode)
    pub line_num: usize,
    /// The matching line content
    pub line: String,
}

/// Full result of a search.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub results: Vec<SearchResultEntry>,
    pub files_searched: usize,
    pub total_matches: usize,
    /// True if results were truncated by head_limit
    pub truncated: bool,
    /// Error message if search failed
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_config_builder() {
        let cfg = SearchConfig::new("hello")
            .with_path("/tmp")
            .with_glob("*.rs")
            .case_insensitive()
            .head_limit(10)
            .max_depth(5);

        assert_eq!(cfg.pattern, "hello");
        assert_eq!(cfg.path, "/tmp");
        assert_eq!(cfg.glob, "*.rs");
        assert!(cfg.case_insensitive);
        assert_eq!(cfg.head_limit, 10);
        assert_eq!(cfg.max_depth, 5);
    }

    #[test]
    fn test_output_mode_default() {
        assert_eq!(OutputMode::default(), OutputMode::FilesWithMatches);
    }
}
