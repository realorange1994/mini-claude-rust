//! Path safety utilities for workspace boundary enforcement.
//!
//! Provides:
//! - `resolve_path`: symlink-resolving path canonicalization
//! - `path_escapes_workspace`: `..` traversal detection
//! - `WorkspaceTrust`: per-workspace trust list persistence

use std::path::{Path, PathBuf};

/// Resolve a path with symlink normalization and `..` component validation.
///
/// 1. Expands `~` to home directory
/// 2. Resolves relative paths against cwd
/// 3. Canonicalizes symlinks (falls back to parent-dir canonicalize for non-existent paths)
/// 4. Checks UNC path (SMB credential leak)
/// 5. Validates against workspace boundary
pub fn resolve_path(path: &str, workspace: &Path) -> Result<PathBuf, String> {
    // UNC path check — accessing \\server\share triggers SMB auth
    if super::is_unc_path(Path::new(path)) {
        return Err(format!("UNC path {:?} blocked (SMB credential leak risk)", path));
    }

    let expanded = super::expand_path(path);
    let abs_workspace = canonicalize_fallback(workspace);

    // Canonicalize the resolved path (follow symlinks)
    let abs_resolved = canonicalize_with_parent_fallback(&expanded);

    // Check workspace boundary
    if let Ok(rel) = abs_resolved.strip_prefix(&abs_workspace) {
        if rel.starts_with("..") {
            return Err(format!(
                "path {:?} escapes workspace via .. traversal",
                path
            ));
        }
    } else {
        return Err(format!(
            "path {:?} is outside the project directory {:?}",
            path, abs_workspace
        ));
    }

    Ok(abs_resolved)
}

/// Detect if a path escapes the workspace via `..` components.
///
/// Tracks a depth counter: `..` decrements, non-`..` increments.
/// If depth goes negative at any point, the path escapes.
/// Handles both Unix (`/`) and Windows (`C:\`) absolute paths.
pub fn path_escapes_workspace(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut depth: i32 = 0;

    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Canonicalize a path, falling back to the original on failure.
fn canonicalize_fallback(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Canonicalize a path. If the file doesn't exist yet, canonicalize the
/// parent directory and append the filename.
fn canonicalize_with_parent_fallback(path: &Path) -> PathBuf {
    match dunce::canonicalize(path) {
        Ok(p) => p,
        Err(_) => {
            if let Some(parent) = path.parent() {
                if let Ok(canonical_parent) = dunce::canonicalize(parent) {
                    canonical_parent.join(path.file_name().unwrap_or_default())
                } else {
                    path.to_path_buf()
                }
            } else {
                path.to_path_buf()
            }
        }
    }
}

// ─── Workspace Trust ──────────────────────────────────────────────────────────

/// Per-workspace trust list. Stored at `~/.claude/workspace-trust.json`.
///
/// Format:
/// ```json
/// {
///   "trusted_paths": ["/home/user/project-a", "/home/user/project-b"]
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct WorkspaceTrust {
    trusted_paths: Vec<PathBuf>,
}

impl WorkspaceTrust {
    /// Load workspace trust from the default location.
    pub fn load() -> Self {
        let path = Self::trust_file_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(arr) = doc.get("trusted_paths").and_then(|v| v.as_array()) {
                    let trusted: Vec<PathBuf> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(PathBuf::from))
                        .collect();
                    return Self { trusted_paths: trusted };
                }
            }
        }
        Self::default()
    }

    /// Check if a workspace path is trusted.
    pub fn permits(&self, workspace: &Path) -> bool {
        let canonical = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        self.trusted_paths.iter().any(|p| {
            let canonical_p = dunce::canonicalize(p).unwrap_or_else(|_| p.clone());
            canonical == canonical_p || canonical.starts_with(canonical_p)
        })
    }

    /// Add a workspace to the trust list and persist.
    pub fn add(&mut self, workspace: &Path) {
        let canonical = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        if !self.permits(&canonical) {
            self.trusted_paths.push(canonical);
            let _ = self.save();
        }
    }

    /// Remove a workspace from the trust list and persist.
    pub fn remove(&mut self, workspace: &Path) {
        let canonical = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        self.trusted_paths.retain(|p| {
            let canonical_p = dunce::canonicalize(p).unwrap_or_else(|_| p.clone());
            canonical_p != canonical
        });
        let _ = self.save();
    }

    fn trust_file_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("workspace-trust.json")
    }

    fn save(&self) -> std::io::Result<()> {
        let path = Self::trust_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let arr: Vec<String> = self
            .trusted_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let doc = serde_json::json!({ "trusted_paths": arr });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap_or_default())
    }
}
