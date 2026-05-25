//! Directory walker for rgrep, with gitignore and config filtering.
//! Ported from upstream tools/rgrep/walker.go (396 lines).

use std::fs;
use std::path::{Path, PathBuf};

use crate::tools::rgrep::binary;
use crate::tools::rgrep::gitignore::GitIgnoreMatcher;
use crate::tools::rgrep::types;

/// Common directory names to skip during traversal.
const IGNORED_DIRS: &[&str] = &[
    ".git", ".svn", ".hg", ".bzr", ".jj", ".sl", ".claude",
    "node_modules", "__pycache__", ".venv", "venv", ".tox",
    ".mypy_cache", ".pytest_cache", ".ruff_cache", ".coverage",
    "htmlcov", ".cargo", ".rustup", "target", ".gradle",
    ".dart_tool", ".cache", "dist", "build", "out",
];

/// Binary extensions to skip.
const BINARY_EXTS: &[&str] = &[
    ".exe", ".dll", ".so", ".dylib", ".o", ".a", ".lib",
    ".bin", ".dat", ".db", ".sqlite", ".sqlite3",
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".tiff", ".tif",
    ".zip", ".gz", ".tar", ".bz2", ".xz", ".7z", ".rar", ".lzma",
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx",
    ".mp3", ".mp4", ".avi", ".mov", ".wmv", ".flv", ".mkv", ".wav", ".ogg",
    ".pyc", ".pyo", ".pyd", ".class", ".jar", ".war",
    ".woff", ".woff2", ".ttf", ".eot", ".otf",
    ".wasm",
];

/// A file entry found during directory traversal.
pub struct WalkEntry {
    pub path: PathBuf,
    pub rel_path: String,
    pub size: u64,
}

/// Walk options controlling directory traversal.
#[derive(Clone)]
pub struct WalkOptions {
    pub root: String,
    pub max_depth: usize,
    pub globs: Vec<String>,
    pub type_filter: String,
    pub excludes: Vec<String>,
    pub respect_gitignore: bool,
    pub max_filesize: u64,
}

impl WalkOptions {
    pub fn new(root: &str) -> Self {
        Self {
            root: root.to_string(),
            max_depth: 0,
            globs: Vec::new(),
            type_filter: String::new(),
            excludes: Vec::new(),
            respect_gitignore: true,
            max_filesize: 0,
        }
    }

    pub fn max_depth(mut self, n: usize) -> Self {
        self.max_depth = n;
        self
    }

    pub fn globs(mut self, globs: Vec<String>) -> Self {
        self.globs = globs;
        self
    }

    pub fn type_filter(mut self, type_name: &str) -> Self {
        self.type_filter = type_name.to_string();
        self
    }

    pub fn excludes(mut self, excludes: Vec<String>) -> Self {
        self.excludes = excludes;
        self
    }

    pub fn respect_gitignore(mut self, respect: bool) -> Self {
        self.respect_gitignore = respect;
        self
    }

    pub fn max_filesize(mut self, bytes: u64) -> Self {
        self.max_filesize = bytes;
        self
    }
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            root: ".".to_string(),
            max_depth: 0,
            globs: Vec::new(),
            type_filter: String::new(),
            excludes: Vec::new(),
            respect_gitignore: true,
            max_filesize: 0,
        }
    }
}

/// Walk a directory tree and return matching file entries.
pub fn walk_dir(opts: WalkOptions) -> Vec<WalkEntry> {
    let root = PathBuf::from(&opts.root);
    if !root.is_dir() {
        return Vec::new();
    }

    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let gitignore = if opts.respect_gitignore {
        Some(crate::tools::rgrep::gitignore::load_gitignore_from_repo_root(&root))
    } else {
        None
    };

    let type_exts: Vec<String> = if !opts.type_filter.is_empty() {
        types::extensions_for_type(&opts.type_filter)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|e| format!(".{}", e.trim_start_matches('.')))
            .collect()
    } else {
        Vec::new()
    };

    let root_depth = root.components().count();
    let mut entries = Vec::new();

    walk_recursive(&root, &root, &gitignore, &opts, &type_exts, root_depth, &mut entries);
    entries
}

/// Walk a directory tree and call fn for each matching file.
/// Unlike walk_dir, this processes files one at a time, keeping memory constant.
pub fn walk_dir_stream(opts: WalkOptions, mut fn_entry: impl FnMut(WalkEntry) -> Result<(), String>) -> Result<(), String> {
    let root = PathBuf::from(&opts.root);
    if !root.is_dir() {
        return Ok(());
    }

    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };

    let gitignore = if opts.respect_gitignore {
        Some(crate::tools::rgrep::gitignore::load_gitignore_from_repo_root(&root))
    } else {
        None
    };

    let type_exts: Vec<String> = if !opts.type_filter.is_empty() {
        types::extensions_for_type(&opts.type_filter)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|e| format!(".{}", e.trim_start_matches('.')))
            .collect()
    } else {
        Vec::new()
    };

    let root_depth = root.components().count();

    walk_recursive_stream(&root, &root, &gitignore, &opts, &type_exts, root_depth, &mut fn_entry)
}

fn walk_recursive(
    current: &Path,
    root: &Path,
    gitignore: &Option<GitIgnoreMatcher>,
    opts: &WalkOptions,
    type_exts: &[String],
    root_depth: usize,
    entries: &mut Vec<WalkEntry>,
) {
    let dir_entries = match fs::read_dir(current) {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in dir_entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }

            if let Some(gi) = gitignore {
                let rel = path_relative(&path, root);
                if gi.is_ignored(&rel, true) {
                    continue;
                }
            }

            let rel = path_relative(&path, root);
            if match_excludes(&opts.excludes, &rel, true) {
                continue;
            }

            if opts.max_depth > 0 {
                let cur_depth = path.components().count().saturating_sub(root_depth);
                if cur_depth >= opts.max_depth {
                    continue;
                }
            }

            walk_recursive(&path, root, gitignore, opts, type_exts, root_depth, entries);
        } else {
            if let Some(gi) = gitignore {
                let rel = path_relative(&path, root);
                if gi.is_ignored(&rel, false) {
                    continue;
                }
            }

            let rel = path_relative(&path, root);
            if match_excludes(&opts.excludes, &rel, false) {
                continue;
            }

            if !opts.globs.is_empty() {
                let matched = opts.globs.iter().any(|g| match_name(g, &name, &rel));
                if !matched {
                    continue;
                }
            }

            if !type_exts.is_empty() {
                let ext = format!(".{}", path.extension().unwrap_or_default().to_string_lossy().to_lowercase());
                if !type_exts.contains(&ext) {
                    continue;
                }
            }

            if let Some(ext) = path.extension() {
                let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
                if BINARY_EXTS.contains(&ext_str.as_str()) {
                    continue;
                }
            }

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = metadata.len();
            if opts.max_filesize > 0 && size > opts.max_filesize {
                continue;
            }

            entries.push(WalkEntry {
                path: path.clone(),
                rel_path: rel,
                size,
            });
        }
    }
}

fn walk_recursive_stream(
    current: &Path,
    root: &Path,
    gitignore: &Option<GitIgnoreMatcher>,
    opts: &WalkOptions,
    type_exts: &[String],
    root_depth: usize,
    fn_entry: &mut impl FnMut(WalkEntry) -> Result<(), String>,
) -> Result<(), String> {
    let dir_entries = match fs::read_dir(current) {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };

    for entry in dir_entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }

            if let Some(gi) = gitignore {
                let rel = path_relative(&path, root);
                if gi.is_ignored(&rel, true) {
                    continue;
                }
            }

            let rel = path_relative(&path, root);
            if match_excludes(&opts.excludes, &rel, true) {
                continue;
            }

            if opts.max_depth > 0 {
                let cur_depth = path.components().count().saturating_sub(root_depth);
                if cur_depth >= opts.max_depth {
                    continue;
                }
            }

            walk_recursive_stream(&path, root, gitignore, opts, type_exts, root_depth, fn_entry)?;
        } else {
            if let Some(gi) = gitignore {
                let rel = path_relative(&path, root);
                if gi.is_ignored(&rel, false) {
                    continue;
                }
            }

            let rel = path_relative(&path, root);
            if match_excludes(&opts.excludes, &rel, false) {
                continue;
            }

            if !opts.globs.is_empty() {
                let matched = opts.globs.iter().any(|g| match_name(g, &name, &rel));
                if !matched {
                    continue;
                }
            }

            if !type_exts.is_empty() {
                let ext = format!(".{}", path.extension().unwrap_or_default().to_string_lossy().to_lowercase());
                if !type_exts.contains(&ext) {
                    continue;
                }
            }

            if let Some(ext) = path.extension() {
                let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
                if BINARY_EXTS.contains(&ext_str.as_str()) {
                    continue;
                }
            }

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = metadata.len();
            if opts.max_filesize > 0 && size > opts.max_filesize {
                continue;
            }

            fn_entry(WalkEntry {
                path: path.clone(),
                rel_path: rel,
                size,
            })?;
        }
    }
    Ok(())
}

/// Get path relative to root as a forward-slash string.
fn path_relative(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

/// Check if a path matches any exclude pattern.
fn match_excludes(excludes: &[String], rel_path: &str, is_dir: bool) -> bool {
    for pattern in excludes {
        let pattern = pattern.trim_start_matches("./");
        let rel = rel_path.trim_start_matches("./");
        if !pattern.contains('/') && !pattern.contains("**") {
            let name = std::path::Path::new(rel)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name == pattern {
                return true;
            }
            continue;
        }
        if gitignore_match(pattern, rel, is_dir) {
            return true;
        }
    }
    false
}

fn gitignore_match(pattern: &str, rel_path: &str, _is_dir: bool) -> bool {
    if pattern == rel_path {
        return true;
    }
    if !pattern.contains("**") {
        if let Ok(m) = glob_match(pattern, rel_path) {
            if m {
                return true;
            }
        }
        if let Some(idx) = rel_path.rfind('/') {
            if let Ok(m) = glob_match(pattern, &rel_path[idx + 1..]) {
                return true;
            }
        }
        return false;
    }
    doublestar_match(pattern, rel_path)
}

fn glob_match(pattern: &str, name: &str) -> Result<bool, ()> {
    let mut p = pattern.chars().peekable();
    let mut n = name.chars().peekable();
    while let Some(&pc) = p.peek() {
        match pc {
            '*' => {
                p.next();
                while p.peek() == Some(&'*') {
                    p.next();
                }
                if p.peek().is_none() {
                    return Ok(true);
                }
                let rem_p: String = p.collect();
                let rem_n: String = n.collect();
                for i in 0..=rem_n.len() {
                    if let Ok(true) = glob_match(&rem_p, &rem_n[i..]) {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            '?' => {
                p.next();
                n.next().ok_or(())?;
            }
            _ => {
                p.next();
                if n.next() != Some(pc) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(n.next().is_none())
}

fn doublestar_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.splitn(2, "**").collect();
    if parts.len() != 2 {
        return false;
    }
    let prefix = parts[0].trim_end_matches('/');
    let suffix = parts[1].trim_start_matches('/');
    if prefix.is_empty() && suffix.is_empty() {
        return true;
    }

    let name_parts: Vec<&str> = name.split('/').collect();
    for i in 0..=name_parts.len() {
        let prefix_name = if i > 0 { name_parts[..i].join("/") } else { String::new() };
        let suffix_name = if i < name_parts.len() { name_parts[i..].join("/") } else { String::new() };
        let prefix_ok = prefix.is_empty() || glob_match(prefix, &prefix_name).unwrap_or(false);
        if !prefix_ok {
            continue;
        }
        let suffix_ok = if suffix.is_empty() {
            true
        } else if suffix.contains("**") {
            doublestar_match(suffix, &suffix_name)
        } else {
            glob_match(suffix, &suffix_name).unwrap_or(false)
        };
        if prefix_ok && suffix_ok {
            return true;
        }
    }
    false
}

fn match_name(glob_pattern: &str, name: &str, rel: &str) -> bool {
    if let Ok(m) = glob_match(glob_pattern, name) {
        if m {
            return true;
        }
    }
    if let Ok(m) = glob_match(glob_pattern, rel) {
        return m;
    }
    false
}

/// Check if a file is binary by scanning for null bytes.
pub fn is_binary_file(path: &Path) -> bool {
    binary::is_binary_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_relative() {
        let root = Path::new("/tmp/test");
        let path = Path::new("/tmp/test/src/main.rs");
        assert_eq!(path_relative(path, root), "src/main.rs");
    }

    #[test]
    fn test_walk_dir() {
        let dir = std::env::temp_dir().join("rgrep_walk_test");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("a.rs"), "fn main() {}");
        let _ = std::fs::write(dir.join("b.txt"), "hello");
        let sub = dir.join("sub");
        let _ = std::fs::create_dir_all(&sub);
        let _ = std::fs::write(sub.join("c.rs"), "fn foo() {}");

        let entries = walk_dir(WalkOptions::new(&dir.to_string_lossy()));
        assert_eq!(entries.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
