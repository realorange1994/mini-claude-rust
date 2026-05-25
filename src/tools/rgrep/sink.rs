//! Output formatting for rgrep search results.
//! Ported from upstream tools/rgrep/sink.go (90 lines).

use crate::tools::rgrep::config::{OutputMode, SearchConfig, SearchResult};

/// Format a SearchResult into a human-readable string.
pub fn format_result(result: &SearchResult, cfg: &SearchConfig) -> String {
    if let Some(ref err) = result.error {
        return format!("Error: {}", err);
    }

    match cfg.output_mode {
        OutputMode::FilesWithMatches => format_files_with_match(result, cfg),
        OutputMode::Count => format_count(result, cfg),
        OutputMode::Content => format_content(result, cfg),
    }
}

fn format_files_with_match(result: &SearchResult, cfg: &SearchConfig) -> String {
    if result.results.is_empty() {
        return format!("No matches found. (Searched {} files)", result.files_searched);
    }

    let mut lines: Vec<String> = Vec::new();
    for r in &result.results {
        lines.push(r.path.clone());
    }

    if result.truncated && cfg.head_limit > 0 {
        lines.push(format!("(showing first {} matches, truncated)", cfg.head_limit));
    }

    let summary = format!("\n(Searched {} files, {} matches)", result.files_searched, result.total_matches);
    lines.join("\n") + &summary
}

fn format_count(result: &SearchResult, cfg: &SearchConfig) -> String {
    if result.results.is_empty() {
        return format!("No matches found. (Searched {} files)", result.files_searched);
    }

    let mut lines: Vec<String> = Vec::new();
    for r in &result.results {
        // Output format: path:count (matching ripgrep --count)
        lines.push(format!("{}:{}", r.path, r.line_num));
    }

    if result.truncated && cfg.head_limit > 0 {
        lines.push(format!("(showing first {} matches, truncated)", cfg.head_limit));
    }

    let summary = format!("\n(Searched {} files, {} matches total)", result.files_searched, result.total_matches);
    lines.join("\n") + &summary
}

fn format_content(result: &SearchResult, cfg: &SearchConfig) -> String {
    if result.results.is_empty() {
        if cfg.offset > 0 {
            return format!(
                "No matches after skipping first {} results. (Searched {} files, {} matches total)",
                cfg.offset, result.files_searched, result.total_matches
            );
        }
        return format!("No matches found. (Searched {} files)", result.files_searched);
    }

    let mut lines: Vec<String> = Vec::new();
    for r in &result.results {
        if cfg.show_line_nums {
            lines.push(format!("{}:{}:{}", r.path, r.line_num, r.line));
        } else {
            lines.push(format!("{}:{}", r.path, r.line));
        }
    }

    if result.truncated && cfg.head_limit > 0 {
        lines.push(format!("(showing first {} matches, truncated)", cfg.head_limit));
    }

    let showing = if result.results.len() < result.total_matches {
        format!(", showing first {}", result.results.len())
    } else {
        String::new()
    };
    let summary = format!(
        "\n(Searched {} files, {} matches{})",
        result.files_searched, result.total_matches, showing
    );

    lines.join("\n") + &summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::rgrep::config::SearchResultEntry;

    #[test]
    fn test_format_content() {
        let result = SearchResult {
            results: vec![
                SearchResultEntry { path: "src/main.rs".to_string(), line_num: 1, line: "fn main() {}".to_string() },
            ],
            files_searched: 5,
            total_matches: 1,
            truncated: false,
            error: None,
        };

        let cfg = SearchConfig::new("main").output_mode(OutputMode::Content);
        let output = format_result(&result, &cfg);
        assert!(output.contains("src/main.rs:1:fn main()"));
        assert!(output.contains("Searched 5 files"));
    }

    #[test]
    fn test_format_files_with_match() {
        let result = SearchResult {
            results: vec![
                SearchResultEntry { path: "src/main.rs".to_string(), line_num: 0, line: String::new() },
                SearchResultEntry { path: "src/lib.rs".to_string(), line_num: 0, line: String::new() },
            ],
            files_searched: 10,
            total_matches: 3,
            truncated: false,
            error: None,
        };

        let cfg = SearchConfig::new("main").output_mode(OutputMode::FilesWithMatches);
        let output = format_result(&result, &cfg);
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
        assert!(output.contains("10 files"));
    }

    #[test]
    fn test_format_count() {
        let result = SearchResult {
            results: vec![
                SearchResultEntry { path: "src/main.rs".to_string(), line_num: 3, line: String::new() },
            ],
            files_searched: 5,
            total_matches: 3,
            truncated: false,
            error: None,
        };

        let cfg = SearchConfig::new("main").output_mode(OutputMode::Count);
        let output = format_result(&result, &cfg);
        assert!(output.contains("src/main.rs:3"));
        assert!(output.contains("5 files"));
    }
}
