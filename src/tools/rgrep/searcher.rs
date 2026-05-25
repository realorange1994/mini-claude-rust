//! Core search engine for rgrep.
//! Ported from upstream tools/rgrep/searcher.go (732 lines).

use regex::{Regex, RegexBuilder};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::tools::rgrep::binary;
use crate::tools::rgrep::config::{OutputMode, SearchConfig, SearchResult, SearchResultEntry};
use crate::tools::rgrep::walker::{self, WalkEntry, WalkOptions};

/// Maximum length of a line in content output.
const MAX_GREP_LINE_LEN: usize = 500;

/// Split glob patterns on commas and whitespace, respecting brace groups.
pub fn split_glob_patterns(glob: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_brace = false;
    for c in glob.chars() {
        match c {
            '{' => { in_brace = true; current.push(c); }
            '}' => { in_brace = false; current.push(c); }
            ',' | ' ' => {
                if !in_brace && !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
            }
            _ => { current.push(c); }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Perform a full search using the given config.
pub fn search(cfg: &SearchConfig) -> SearchResult {
    let search_pattern = if cfg.fixed_strings {
        regex::escape(&cfg.pattern)
    } else {
        cfg.pattern.clone()
    };

    let mut builder = RegexBuilder::new(&search_pattern);
    builder.case_insensitive(cfg.case_insensitive);
    builder.dot_matches_new_line(cfg.multiline);

    let re = match builder.build() {
        Ok(re) => re,
        Err(e) => return SearchResult { error: Some(format!("invalid regex: {}", e)), ..Default::default() },
    };

    let search_path = if cfg.path.is_empty() { "." } else { cfg.path.as_str() };
    let path = Path::new(search_path);

    if !path.exists() {
        return SearchResult { error: Some(format!("path not found: {}", search_path)), ..Default::default() };
    }

    if path.is_file() {
        return search_single_file(&re, cfg, search_path, search_path);
    }

    let globs = split_glob_patterns(&cfg.glob);
    let walk_opts = WalkOptions::new(search_path)
        .max_depth(cfg.max_depth)
        .globs(globs)
        .type_filter(&cfg.type_filter)
        .excludes(cfg.excludes.clone())
        .respect_gitignore(true)
        .max_filesize(cfg.max_filesize);

    let literal_prefix = extract_literal_prefix(&re);

    match cfg.output_mode {
        OutputMode::FilesWithMatches => search_dir_files_only(&re, &walk_opts, cfg, &literal_prefix),
        OutputMode::Count => search_dir_count(&re, &walk_opts, cfg, &literal_prefix),
        OutputMode::Content => search_dir_content(&re, &walk_opts, cfg, &literal_prefix),
    }
}

fn extract_literal_prefix(re: &Regex) -> Option<Vec<u8>> {
    // Heuristic: look for literal ASCII prefix at the start of the pattern
    let s = re.as_str();
    let mut prefix = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' {
            prefix.push(c);
        } else {
            break;
        }
    }
    if prefix.len() >= 2 {
        Some(prefix.into_bytes())
    } else {
        None
    }
}

fn search_single_file(re: &Regex, cfg: &SearchConfig, path: &str, rel_path: &str) -> SearchResult {
    let mut st = SearchState::new(cfg.clone(), path.to_string());
    st.files_searched = 1;

    let entry = WalkEntry {
        path: Path::new(path).to_path_buf(),
        rel_path: rel_path.to_string(),
        size: 0,
    };

    match cfg.output_mode {
        OutputMode::FilesWithMatches => {
            if file_has_match(Path::new(path), re, cfg, &None) {
                st.total_matches = 1;
                st.add_result(SearchResultEntry { path: rel_path.to_string(), line_num: 0, line: String::new() });
            }
        }
        OutputMode::Count => {
            let count = count_in_file(Path::new(path), re, cfg, &None);
            if count > 0 {
                st.total_matches = count;
                st.add_result(SearchResultEntry { path: rel_path.to_string(), line_num: count, line: String::new() });
            }
        }
        OutputMode::Content => {
            search_file_content(&entry, re, cfg, &mut st, &None);
        }
    }

    let truncated = cfg.head_limit > 0 && st.results.len() >= cfg.head_limit;

    SearchResult {
        results: st.results,
        files_searched: st.files_searched,
        total_matches: st.total_matches,
        truncated,
        error: None,
    }
}

fn search_dir_files_only(
    re: &Regex,
    walk_opts: &WalkOptions,
    cfg: &SearchConfig,
    literal_prefix: &Option<Vec<u8>>,
) -> SearchResult {
    let mut st = SearchState::new(cfg.clone(), walk_opts.root.clone());

    walker::walk_dir_stream(walk_opts.clone(), |entry| {
        st.files_searched += 1;
        if file_has_match(&entry.path, re, cfg, literal_prefix) {
            if st.skipped < cfg.offset {
                st.skipped += 1;
            } else {
                st.total_matches += 1;
                st.add_result(SearchResultEntry {
                    path: make_relative(&st.root, &entry),
                    line_num: 0,
                    line: String::new(),
                });
            }
        }
        if st.is_done() { Err("done".to_string()) } else { Ok(()) }
    });

    let truncated = cfg.head_limit > 0 && st.results.len() >= cfg.head_limit;

    SearchResult {
        results: st.results,
        files_searched: st.files_searched,
        total_matches: st.total_matches,
        truncated,
        error: None,
    }
}

fn search_dir_count(
    re: &Regex,
    walk_opts: &WalkOptions,
    cfg: &SearchConfig,
    literal_prefix: &Option<Vec<u8>>,
) -> SearchResult {
    let mut st = SearchState::new(cfg.clone(), walk_opts.root.clone());

    walker::walk_dir_stream(walk_opts.clone(), |entry| {
        st.files_searched += 1;
        let count = count_in_file(&entry.path, re, cfg, literal_prefix);
        if count > 0 {
            st.total_matches += count;
            st.add_result(SearchResultEntry {
                path: make_relative(&st.root, &entry),
                line_num: count,
                line: String::new(),
            });
        }
        if st.is_done() { Err("done".to_string()) } else { Ok(()) }
    });

    // Apply offset
    if cfg.offset > 0 && cfg.offset < st.results.len() {
        st.results = st.results[cfg.offset..].to_vec();
    }

    let truncated = cfg.head_limit > 0 && st.results.len() >= cfg.head_limit;

    SearchResult {
        results: st.results,
        files_searched: st.files_searched,
        total_matches: st.total_matches,
        truncated,
        error: None,
    }
}

fn search_dir_content(
    re: &Regex,
    walk_opts: &WalkOptions,
    cfg: &SearchConfig,
    literal_prefix: &Option<Vec<u8>>,
) -> SearchResult {
    let mut st = SearchState::new(cfg.clone(), walk_opts.root.clone());

    walker::walk_dir_stream(walk_opts.clone(), |entry| {
        st.files_searched += 1;
        search_file_content(&entry, re, cfg, &mut st, literal_prefix);
        if st.is_done() { Err("done".to_string()) } else { Ok(()) }
    });

    let truncated = cfg.head_limit > 0 && st.results.len() >= cfg.head_limit;

    SearchResult {
        results: st.results,
        files_searched: st.files_searched,
        total_matches: st.total_matches,
        truncated,
        error: None,
    }
}

// ---------- core search operations ----------

/// Check if a file contains a regex match. Two-phase search with literal prefix.
fn file_has_match(path: &Path, re: &Regex, cfg: &SearchConfig, literal_prefix: &Option<Vec<u8>>) -> bool {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    if binary::is_binary_file(path) {
        return false;
    }

    if cfg.multiline {
        let data = read_whole_file(file);
        if data.is_empty() {
            return false;
        }
        if let Some(ref prefix) = literal_prefix {
            if !contains_subslice(&data, prefix) {
                return false;
            }
        }
        return re.is_match(&String::from_utf8_lossy(&data));
    }

    let reader = BufReader::new(file);
    if let Some(ref prefix) = literal_prefix {
        for line in reader.lines().flatten() {
            let line_bytes = line.as_bytes();
            if contains_subslice(line_bytes, prefix) {
                if re.is_match(&line) {
                    return true;
                }
            }
        }
    } else {
        for line in reader.lines().flatten() {
            if re.is_match(&line) {
                return true;
            }
        }
    }
    false
}

/// Count regex matches in a file. Two-phase with literal prefix.
fn count_in_file(path: &Path, re: &Regex, cfg: &SearchConfig, literal_prefix: &Option<Vec<u8>>) -> usize {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };

    if binary::is_binary_file(path) {
        return 0;
    }

    if cfg.multiline {
        let data = read_whole_file(file);
        if data.is_empty() {
            return 0;
        }
        if let Some(ref prefix) = literal_prefix {
            if !contains_subslice(&data, prefix) {
                return 0;
            }
        }
        return re.find_iter(&String::from_utf8_lossy(&data)).count();
    }

    let reader = BufReader::new(file);
    let mut count = 0;

    if let Some(ref prefix) = literal_prefix {
        for line in reader.lines().flatten() {
            let line_bytes = line.as_bytes();
            if contains_subslice(line_bytes, prefix) {
                let line = truncate_line(&line);
                count += re.find_iter(&line).count();
            }
        }
    } else {
        for line in reader.lines().flatten() {
            let line = truncate_line(&line);
            count += re.find_iter(&line).count();
        }
    }
    count
}

/// Search file content with context lines.
fn search_file_content(
    entry: &WalkEntry,
    re: &Regex,
    cfg: &SearchConfig,
    st: &mut SearchState,
    literal_prefix: &Option<Vec<u8>>,
) {
    let file = match File::open(&entry.path) {
        Ok(f) => f,
        Err(_) => return,
    };

    if binary::is_binary_file(&entry.path) {
        return;
    }

    let rel_path = make_relative(&st.root, entry);
    let ctx_before = cfg.context_before;
    let ctx_after = cfg.context_after;

    if cfg.multiline {
        search_file_content_multiline(&rel_path, re, cfg, st, literal_prefix);
        return;
    }

    // Byte-level ring buffer for before-context
    let mut ring_buf: Vec<Vec<u8>> = vec![Vec::new(); ctx_before];
    let mut ring_line_nums: Vec<usize> = vec![0; ctx_before];
    let mut ring_idx = 0;
    let mut ring_count = 0;

    let reader = BufReader::new(file);
    let mut line_num = 0;
    let mut after_left = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        line_num += 1;

        let truncated = line.len() > MAX_GREP_LINE_LEN;
        let line_str = if truncated {
            truncate_line(&line)
        } else {
            line
        };

        let matched = if let Some(ref prefix) = literal_prefix {
            line_str.as_bytes().windows(prefix.len()).any(|w| w == prefix.as_slice())
                && re.is_match(&line_str)
        } else {
            re.is_match(&line_str)
        };

        if matched {
            st.total_matches += 1;
            if st.skipped < cfg.offset {
                st.skipped += 1;
                after_left = 0;
                if ctx_before > 0 {
                    let copy: Vec<u8> = if truncated {
                        let mut v = line_str.as_bytes().to_vec();
                        v.extend_from_slice(b"...");
                        v
                    } else {
                        line_str.as_bytes().to_vec()
                    };
                    ring_buf[ring_idx] = copy;
                    ring_line_nums[ring_idx] = line_num;
                    ring_idx = (ring_idx + 1) % ctx_before;
                    ring_count += 1;
                }
                continue;
            }

            // Emit before-context
            if ctx_before > 0 && ring_count > 0 {
                let n = ring_count.min(ctx_before);
                let start_idx = if ring_idx >= n { ring_idx - n } else { ring_idx + ctx_before - n };
                for j in 0..n {
                    let idx = (start_idx + j) % ctx_before;
                    if !ring_buf[idx].is_empty() {
                        st.add_result(SearchResultEntry {
                            path: rel_path.clone(),
                            line_num: ring_line_nums[idx],
                            line: String::from_utf8_lossy(&ring_buf[idx]).to_string(),
                        });
                        if st.is_done() { return; }
                    }
                }
            }

            // Emit match line
            st.add_result(SearchResultEntry {
                path: rel_path.clone(),
                line_num,
                line: line_str.clone(),
            });
            if st.is_done() { return; }

            after_left = ctx_after;
        } else if after_left > 0 {
            st.add_result(SearchResultEntry {
                path: rel_path.clone(),
                line_num,
                line: line_str.clone(),
            });
            if st.is_done() { return; }
            after_left -= 1;
        }

        // Store in ring buffer
        if ctx_before > 0 {
            let copy: Vec<u8> = if truncated {
                let mut v = line_str.as_bytes().to_vec();
                v.extend_from_slice(b"...");
                v
            } else {
                line_str.as_bytes().to_vec()
            };
            ring_buf[ring_idx] = copy;
            ring_line_nums[ring_idx] = line_num;
            ring_idx = (ring_idx + 1) % ctx_before;
            ring_count += 1;
        }
    }
}

fn search_file_content_multiline(
    rel_path: &str,
    re: &Regex,
    cfg: &SearchConfig,
    st: &mut SearchState,
    literal_prefix: &Option<Vec<u8>>,
) {
    let file = match File::open(rel_path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let data = read_whole_file(file);
    if data.is_empty() {
        return;
    }

    // Phase 1: fast literal scan
    if let Some(ref prefix) = literal_prefix {
        if !contains_subslice(&data, prefix) {
            return;
        }
    }

    let ctx_before = cfg.context_before;
    let ctx_after = cfg.context_after;

    let text = String::from_utf8_lossy(&data);
    let matches: Vec<_> = re.find_iter(&text).map(|m| (m.start(), m.end())).collect();
    if matches.is_empty() {
        return;
    }

    let newline_offsets = build_newline_index(&data);

    let line_for_byte_offset = |byte_off: usize| -> usize {
        let mut lo = 0;
        let mut hi = newline_offsets.len() - 1;
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if newline_offsets[mid] <= byte_off {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo + 1
    };

    let extract_line = |line_num: usize| -> String {
        if line_num < 1 || line_num > newline_offsets.len() {
            return String::new();
        }
        let start = newline_offsets[line_num - 1];
        let end = if line_num < newline_offsets.len() {
            newline_offsets[line_num]
        } else {
            data.len()
        };
        let mut end = end;
        if end > start && data[end - 1] == b'\n' { end -= 1; }
        if end > start && data[end - 1] == b'\r' { end -= 1; }
        let line = String::from_utf8_lossy(&data[start..end]).to_string();
        truncate_line(&line)
    };

    let mut emitted = std::collections::HashSet::new();

    for (start_off, end_off) in matches {
        let start_line = line_for_byte_offset(start_off);
        let mut end_byte = end_off.saturating_sub(1);
        let end_line = line_for_byte_offset(end_byte).max(start_line);

        st.total_matches += 1;
        if st.skipped < cfg.offset {
            st.skipped += 1;
            continue;
        }

        // Before-context
        for i in (start_line.saturating_sub(ctx_before) + 1)..start_line {
            if emitted.insert(i) {
                st.add_result(SearchResultEntry {
                    path: rel_path.to_string(),
                    line_num: i,
                    line: extract_line(i),
                });
                if st.is_done() { return; }
            }
        }

        // Match lines
        for i in start_line..=end_line {
            if emitted.insert(i) {
                st.add_result(SearchResultEntry {
                    path: rel_path.to_string(),
                    line_num: i,
                    line: extract_line(i),
                });
                if st.is_done() { return; }
            }
        }

        // After-context
        let total_lines = newline_offsets.len();
        for i in end_line + 1..=total_lines.min(end_line + ctx_after) {
            if emitted.insert(i) {
                st.add_result(SearchResultEntry {
                    path: rel_path.to_string(),
                    line_num: i,
                    line: extract_line(i),
                });
                if st.is_done() { return; }
            }
        }
    }
}

// ---------- helper functions ----------

fn build_newline_index(data: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(data.len() / 40 + 2);
    offsets.push(0);
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    if !data.is_empty() && data[data.len() - 1] != b'\n' {
        offsets.push(data.len());
    }
    offsets
}

fn read_whole_file(mut f: File) -> Vec<u8> {
    let mut buf = Vec::new();
    match f.read_to_end(&mut buf) {
        Ok(_) => buf,
        Err(_) => Vec::new(),
    }
}

/// Check if a haystack contains a needle subslice.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_GREP_LINE_LEN {
        line.to_string()
    } else {
        // Truncate at char boundary
        let mut end = MAX_GREP_LINE_LEN;
        while end > 0 && !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &line[..end])
    }
}

fn make_relative(root: &str, entry: &WalkEntry) -> String {
    if !entry.rel_path.is_empty() && entry.rel_path != "." {
        entry.rel_path.clone()
    } else {
        entry.path.to_string_lossy().replace('\\', "/")
    }
}

// ---------- SearchState ----------

/// State tracking during a search.
#[derive(Default)]
pub struct SearchState {
    pub results: Vec<SearchResultEntry>,
    pub files_searched: usize,
    pub total_matches: usize,
    pub skipped: usize,
    pub cfg: SearchConfig,
    done: bool,
    root: String,
}

impl SearchState {
    pub fn new(cfg: SearchConfig, root: String) -> Self {
        Self {
            results: Vec::with_capacity(32),
            files_searched: 0,
            total_matches: 0,
            skipped: 0,
            cfg,
            done: false,
            root,
        }
    }

    pub fn add_result(&mut self, r: SearchResultEntry) {
        if self.cfg.head_limit > 0 && self.results.len() >= self.cfg.head_limit {
            self.done = true;
            self.results.truncate(self.cfg.head_limit);
            return;
        }
        self.results.push(r);
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

/// Truncate a line for display.
pub fn truncate_line_display(line: &str) -> String {
    truncate_line(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_search_simple() {
        let dir = std::env::temp_dir().join("rgrep_search_test");
        let _ = std::fs::create_dir_all(&dir);
        let mut f = File::create(dir.join("test.rs")).unwrap();
        writeln!(f, "fn main() {{").unwrap();
        writeln!(f, "    println!(\"hello\");").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let cfg = SearchConfig::new("hello")
            .with_path(&dir.to_string_lossy())
            .output_mode(OutputMode::Content);
        let result = search(&cfg);
        assert_eq!(result.total_matches, 1);
        assert!(result.results.iter().any(|r| r.line.contains("hello")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_split_glob_patterns() {
        assert_eq!(split_glob_patterns("*.ts, *.js"), vec!["*.ts", "*.js"]);
        assert_eq!(split_glob_patterns("*.rs"), vec!["*.rs"]);
    }

    #[test]
    fn test_contains_subslice() {
        assert!(contains_subslice(b"hello world", b"world"));
        assert!(!contains_subslice(b"hello world", b"xyz"));
    }
}
