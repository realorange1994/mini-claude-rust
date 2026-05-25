use std::path::PathBuf;
use std::process::Command;

/// WorktreeConfig specifies worktree isolation settings for a sub-agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorktreeConfig {
    pub enabled: bool,
    pub name: Option<String>,
    pub keep: bool,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: None,
            keep: false,
        }
    }
}

/// WorktreeResult holds the result of setting up a worktree.
pub struct WorktreeResult {
    pub path: PathBuf,
    pub cleanup: Box<dyn FnOnce() -> Result<(), std::io::Error>>,
}

/// Generates a short hex string suitable for unique naming (8 hex chars).
pub fn uuid_v4_short() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let b: [u8; 4] = rng.gen();
    format!("{:08x}", u32::from_be_bytes(b))
}

/// SetupWorktree creates a git worktree for isolated agent execution.
/// Returns the worktree path and a cleanup function.
/// If cfg.enabled is false, returns an empty path with a no-op cleanup.
pub fn setup_worktree(cfg: &WorktreeConfig) -> Result<WorktreeResult, String> {
    if !cfg.enabled {
        return Ok(WorktreeResult {
            path: PathBuf::new(),
            cleanup: Box::new(|| Ok(())),
        });
    }

    // Generate worktree name
    let name = cfg.name.clone().unwrap_or_else(|| format!("agent-{}", uuid_v4_short()));

    // Create worktree directory path
    let worktree_dir = std::path::Path::new(".claude")
        .join("worktrees")
        .join(&name);

    // Ensure parent directory exists
    if let Some(parent) = worktree_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create worktree directory: {}", e))?;
    }

    // Create worktree: git worktree add <path>
    let output = Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_dir)
        .output()
        .map_err(|e| format!("failed to execute git worktree add: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "failed to create worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let keep = cfg.keep;
    let worktree_dir_clone = worktree_dir.clone();
    let cleanup = Box::new(move || -> Result<(), std::io::Error> {
        if keep {
            return Ok(());
        }
        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&worktree_dir_clone)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("failed to remove worktree: {}", String::from_utf8_lossy(&output.stderr)),
            ));
        }
        Ok(())
    });

    Ok(WorktreeResult {
        path: worktree_dir,
        cleanup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_v4_short_format() {
        let id = uuid_v4_short();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_uuid_v4_short_uniqueness() {
        let id1 = uuid_v4_short();
        let id2 = uuid_v4_short();
        // Not guaranteed unique but extremely unlikely to collide
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_worktree_config_default() {
        let cfg = WorktreeConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.name.is_none());
        assert!(!cfg.keep);
    }

    #[test]
    fn test_setup_worktree_disabled() {
        let cfg = WorktreeConfig::default();
        let result = setup_worktree(&cfg).unwrap();
        assert!(result.path.as_os_str().is_empty());
        // cleanup should be no-op
        (result.cleanup)().unwrap();
    }
}
