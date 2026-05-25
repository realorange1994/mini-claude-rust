//! .gitignore pattern matching for rgrep.
//! Ported from upstream tools/rgrep/gitignore.go (323 lines).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// A single .gitignore pattern.
#[derive(Debug, Clone)]
struct GitIgnorePattern {
    pattern: String,
    negated: bool,
    dir_only: bool,
    rooted: bool,
}

/// Holds all loaded .gitignore patterns and answers "should this path be ignored?".
pub struct GitIgnoreMatcher {
    patterns: Vec<GitIgnorePattern>,
}

impl GitIgnoreMatcher {
    pub fn new() -> Self {
        Self { patterns: Vec::new() }
    }

    /// Read and parse a .gitignore file, adding its patterns.
    pub fn load_file(&mut self, path: &Path) -> std::io::Result<()> {
        let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if let Some(pat) = parse_gitignore_line(&line, &base_dir) {
                self.patterns.push(pat);
            }
        }
        Ok(())
    }

    /// Load .gitignore from the given directory if it exists.
    pub fn load_from_dir(&mut self, dir: &Path) -> std::io::Result<()> {
        let gitignore_path = dir.join(".gitignore");
        if gitignore_path.is_file() {
            self.load_file(&gitignore_path)?;
        }
        Ok(())
    }

    /// Check whether a path should be ignored.
    /// rel_path is relative to the search root. is_dir indicates if the path is a directory.
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        let rel_path = normalize_path(rel_path);
        let mut ignored = false;
        for pat in &self.patterns {
            if pat.dir_only && !is_dir {
                continue;
            }
            if match_gitignore_pattern(pat, &rel_path, is_dir) {
                ignored = !pat.negated;
            }
        }
        ignored
    }

    /// Return debug info about loaded patterns.
    pub fn debug_info(&self) -> String {
        if self.patterns.is_empty() {
            "no .gitignore patterns loaded".to_string()
        } else {
            format!("{} .gitignore patterns loaded", self.patterns.len())
        }
    }
}

fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// Parse a single .gitignore line into a pattern.
/// Returns None for blank lines and comments.
fn parse_gitignore_line(line: &str, _base_dir: &Path) -> Option<GitIgnorePattern> {
    let line = line.trim_end();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let mut negated = false;
    let mut dir_only = false;
    let mut rooted = false;
    let mut pattern = line.to_string();

    if pattern.starts_with('!') {
        negated = true;
        pattern = pattern[1..].to_string();
    }

    if pattern.ends_with('/') {
        dir_only = true;
        pattern = pattern.trim_end_matches('/').to_string();
    }

    if pattern.starts_with('/') {
        rooted = true;
        pattern = pattern[1..].to_string();
    }

    Some(GitIgnorePattern {
        pattern,
        negated,
        dir_only,
        rooted,
    })
}

fn match_gitignore_pattern(pat: &GitIgnorePattern, rel_path: &str, _is_dir: bool) -> bool {
    let pattern = &pat.pattern;
    let has_slash = pattern.contains('/');

    if pat.rooted || has_slash {
        return match_glob(pattern, rel_path);
    }

    // Pattern without slash: match against any path component
    let parts: Vec<&str> = rel_path.split('/').collect();
    for i in (0..parts.len()).rev() {
        if match_glob(pattern, parts[i]) {
            return true;
        }
        if _is_dir && i > 0 {
            let dir_path = parts[..=i].join("/");
            if match_glob(pattern, &dir_path) {
                return true;
            }
        }
    }
    false
}

/// Glob matching with support for **, ?, and char ranges.
fn match_glob(pattern: &str, name: &str) -> bool {
    if pattern == name {
        return true;
    }

    if !pattern.contains("**") {
        if let Ok(matched) = glob_match_simple(pattern, name) {
            if matched {
                return true;
            }
        }
        if let Some(idx) = name.rfind('/') {
            if let Ok(matched) = glob_match_simple(pattern, &name[idx + 1..]) {
                if matched {
                    return true;
                }
            }
        }
        return false;
    }

    match_doublestar(pattern, name)
}

/// Simple glob matching (no **).
fn glob_match_simple(pattern: &str, name: &str) -> Result<bool, ()> {
    let mut p_chars = pattern.chars().peekable();
    let mut n_chars = name.chars().peekable();

    while let Some(&pc) = p_chars.peek() {
        match pc {
            '*' => {
                p_chars.next();
                // Consume consecutive stars
                while p_chars.peek() == Some(&'*') {
                    p_chars.next();
                }
                if p_chars.peek().is_none() {
                    return Ok(true);
                }
                let remaining_pattern: String = p_chars.collect();
                let remaining_name: String = n_chars.collect();
                // Try matching remaining pattern at each position
                for i in 0..=remaining_name.len() {
                    if let Ok(true) = glob_match_simple(&remaining_pattern, &remaining_name[i..]) {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            '?' => {
                p_chars.next();
                if n_chars.next().is_none() {
                    return Ok(false);
                }
            }
            '[' => {
                p_chars.next();
                // Character class
                let negated = if p_chars.peek() == Some(&'!') {
                    p_chars.next();
                    true
                } else {
                    false
                };

                let mut class_chars = String::new();
                loop {
                    match p_chars.next() {
                        Some(']') => break,
                        Some(c) => class_chars.push(c),
                        None => return Err(()),
                    }
                }

                let nc = n_chars.next().ok_or(())?;
                let in_class = class_chars.chars().any(|c| c == nc);
                if in_class == negated {
                    return Ok(false);
                }
            }
            _ => {
                p_chars.next();
                if n_chars.next() != Some(pc) {
                    return Ok(false);
                }
            }
        }
    }

    Ok(n_chars.next().is_none())
}

/// Handle ** glob patterns.
fn match_doublestar(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.splitn(2, "**").collect();
    if parts.len() != 2 {
        return false;
    }

    let mut prefix = parts[0].trim_end_matches('/');
    let mut suffix = parts[1].trim_start_matches('/');

    if prefix.is_empty() && suffix.is_empty() {
        return true;
    }

    let name_parts: Vec<&str> = name.split('/').collect();
    for i in 0..=name_parts.len() {
        let prefix_name = if i > 0 { name_parts[..i].join("/") } else { String::new() };
        let suffix_name = if i < name_parts.len() { name_parts[i..].join("/") } else { String::new() };

        let prefix_ok = if prefix.is_empty() {
            true
        } else {
            glob_match_simple(prefix, &prefix_name).unwrap_or(false)
        };

        if !prefix_ok {
            continue;
        }

        let suffix_ok = if suffix.is_empty() {
            true
        } else if suffix.contains("**") {
            match_doublestar(suffix, &suffix_name)
        } else {
            glob_match_simple(suffix, &suffix_name).unwrap_or(false)
        };

        if prefix_ok && suffix_ok {
            return true;
        }
    }

    false
}

/// Find the git repo root by walking up from dir to find .git/.
pub fn find_git_repo_root(dir: &Path) -> Option<PathBuf> {
    let mut abs = dir.canonicalize().ok()?;
    loop {
        if abs.join(".git").is_dir() {
            return Some(abs);
        }
        let parent = abs.parent()?.to_path_buf();
        if parent == abs {
            return None;
        }
        abs = parent;
    }
}

/// Load .gitignore from the git repo root and all directories between search_root and repo_root.
pub fn load_gitignore_from_repo_root(search_root: &Path) -> GitIgnoreMatcher {
    let mut matcher = GitIgnoreMatcher::new();

    if let Some(repo_root) = find_git_repo_root(search_root) {
        // Collect directories from search_root up to repo_root
        let mut dirs = Vec::new();
        let mut dir = search_root.to_path_buf();
        loop {
            dirs.push(dir.clone());
            if dir == repo_root {
                break;
            }
            let parent = dir.parent().unwrap_or(&dir).to_path_buf();
            if parent == dir {
                break;
            }
            dir = parent;
        }
        // Load from repo root first (lowest priority), then deeper dirs (higher priority)
        for d in dirs.iter().rev() {
            let _ = matcher.load_from_dir(d);
        }
    }

    matcher
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gitignore_simple() {
        let mut matcher = GitIgnoreMatcher::new();
        // Simulate loading "*.log" pattern
        matcher.patterns.push(GitIgnorePattern {
            pattern: "*.log".to_string(),
            negated: false,
            dir_only: false,
            rooted: false,
        });
        assert!(matcher.is_ignored("error.log", false));
        assert!(!matcher.is_ignored("main.rs", false));
    }

    #[test]
    fn test_gitignore_negation() {
        let mut matcher = GitIgnoreMatcher::new();
        matcher.patterns.push(GitIgnorePattern {
            pattern: "*.log".to_string(),
            negated: false,
            dir_only: false,
            rooted: false,
        });
        matcher.patterns.push(GitIgnorePattern {
            pattern: "important.log".to_string(),
            negated: true,
            dir_only: false,
            rooted: false,
        });
        assert!(!matcher.is_ignored("important.log", false));
        assert!(matcher.is_ignored("other.log", false));
    }

    #[test]
    fn test_glob_simple() {
        assert!(glob_match_simple("*.rs", "main.rs").unwrap_or(false));
        assert!(!glob_match_simple("*.rs", "main.go").unwrap_or(false));
        assert!(glob_match_simple("?", "a").unwrap_or(false));
        assert!(!glob_match_simple("?", "ab").unwrap_or(false));
    }

    #[test]
    fn test_doublestar() {
        assert!(match_doublestar("src/**/*.rs", "src/tools/rgrep/mod.rs"));
        assert!(match_doublestar("**", "foo/bar.rs"));
        assert!(match_doublestar("**/*.rs", "foo/bar.rs"));
    }
}