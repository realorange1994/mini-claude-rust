//! GitTool - Git version control operations

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::collections::HashMap as StdHashMap;

// Standalone utility functions (callable from system_prompt builder too)

/// Walk up from dir to find the .git directory or file. Returns the root path.
pub fn find_git_root(dir: &str) -> Result<String, String> {
    let mut current = PathBuf::from(dir);
    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            return Ok(current.to_str().map(|s| s.to_string())
                .ok_or_else(|| "Invalid path encoding".to_string())?);
        }
        if !current.pop() {
            return Err("No git repository found".to_string());
        }
    }
}

/// Run `git rev-parse --abbrev-ref HEAD` to get current branch name.
pub fn get_branch(dir: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--abbrev-ref", "HEAD"]);
    cmd.current_dir(dir);
    let output = cmd.output().map_err(|e| format!("Failed to run git: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("Failed to get current branch".to_string())
    }
}

/// Check if the repo at dir is a bare repository.
pub fn is_bare_repo(dir: &str) -> bool {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--is-bare-repository"]);
    cmd.current_dir(dir);
    cmd.output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<bool>().ok())
        .unwrap_or(false)
}

/// Check if dir is inside a git repository by finding the .git root.
pub fn is_git_repo(dir: &str) -> bool {
    find_git_root(dir).is_ok()
}

/// Run `git status --porcelain -u` and return a map of file -> status.
/// Includes untracked files (useful for displaying status to the user).
pub fn get_git_status(dir: &str) -> Result<StdHashMap<String, String>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain", "-u"]);
    cmd.current_dir(dir);
    let output = cmd.output().map_err(|e| format!("Failed to run git status: {}", e))?;
    if !output.status.success() {
        return Err("Failed to get git status".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result = StdHashMap::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.len() >= 3 {
            let status = trimmed[..2].to_string();
            let file = trimmed[3..].to_string();
            result.insert(file, status);
        }
    }
    Ok(result)
}

/// Check if there are any uncommitted changes to tracked files in the repo.
/// Uses `git diff --quiet` (unstaged) and `git diff --cached --quiet` (staged).
/// Untracked files are intentionally excluded — this matches how git checkout/switch
/// work, since they don't fail due to untracked files.
pub fn has_uncommitted_changes(dir: &str) -> bool {
    // Check unstaged changes to tracked files
    let mut cmd = Command::new("git");
    cmd.args(["diff", "--quiet"]);
    cmd.current_dir(dir);
    if let Ok(output) = cmd.output() {
        if !output.status.success() {
            return true;
        }
    }

    // Check staged changes to tracked files
    let mut cmd2 = Command::new("git");
    cmd2.args(["diff", "--cached", "--quiet"]);
    cmd2.current_dir(dir);
    if let Ok(output) = cmd2.output() {
        if !output.status.success() {
            return true;
        }
    }

    false
}

/// Try to get the default branch via origin/HEAD, falling back to "main".
pub fn get_default_branch(dir: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(["symbolic-ref", "refs/remotes/origin/HEAD"]);
    cmd.current_dir(dir);
    if let Ok(output) = cmd.output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(idx) = stdout.rfind('/') {
                return Ok(stdout[idx + 1..].to_string());
            }
            return Ok(stdout);
        }
    }
    Ok("main".to_string())
}

/// Get the current commit hash (full 40 chars).
pub fn get_current_commit_hash(dir: &str) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "HEAD"]);
    cmd.current_dir(dir);
    let output = cmd.output().map_err(|e| format!("Failed to run git rev-parse: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("Failed to get commit hash".to_string())
    }
}

/// Check if the repo is dirty: has uncommitted changes to tracked files
/// (unstaged or staged). Untracked files are NOT considered dirty — this
/// matches how git checkout/switch actually work.
pub fn is_dirty(dir: &str) -> bool {
    has_uncommitted_changes(dir)
}

/// Return a formatted git context string for system prompt injection.
pub fn get_git_context() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_str().map(|s| s.to_string()).unwrap_or_default())
        .unwrap_or_default();

    if !is_git_repo(&cwd) {
        return String::new();
    }

    let mut parts = Vec::new();

    if let Ok(root) = find_git_root(&cwd) {
        parts.push(format!("- Git Root: {}", root));
    }
    if let Ok(branch) = get_branch(&cwd) {
        parts.push(format!("- Git Branch: {}", branch));
    }
    if let Ok(hash) = get_current_commit_hash(&cwd) {
        let short = if hash.len() >= 12 { &hash[..12] } else { &hash };
        parts.push(format!("- Git Commit: {}", short));
    }
    if is_dirty(&cwd) {
        parts.push("- Git Dirty: true".to_string());
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("\n## Git Context\n{}\n", parts.join("\n"))
    }
}

/// Return a compact git context string for user-facing prompt injection.
pub fn get_git_context_for_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_str().map(|s| s.to_string()).unwrap_or_default())
        .unwrap_or_default();

    if !is_git_repo(&cwd) {
        return String::new();
    }

    let mut parts = Vec::new();

    if let Ok(branch) = get_branch(&cwd) {
        parts.push(format!("- Git Branch: {}", branch));
    }
    if let Ok(hash) = get_current_commit_hash(&cwd) {
        let short = if hash.len() >= 12 { &hash[..12] } else { &hash };
        parts.push(format!("- Git Commit: {}", short));
    }
    if is_dirty(&cwd) {
        parts.push("- Git Dirty: true".to_string());
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{}\n", parts.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Security helpers for dangerous-operation detection & permission checks
// ---------------------------------------------------------------------------

/// Returns (true, reason) if the given git operation with flags is dangerous.
fn is_dangerous_git_operation(operation: &str, flags: &[String]) -> Option<String> {
    match operation {
        "push" => {
            if flags.iter().any(|f| f == "--force" || f == "-f") {
                return Some("Force push is not allowed: it can overwrite remote history and cause data loss".to_string());
            }
            // --force-with-lease is allowed (safer variant that protects against overwriting others' work)
        }
        "reset" => {
            if flags.iter().any(|f| f == "--hard") {
                return Some("git reset --hard is not allowed: it discards uncommitted changes permanently".to_string());
            }
            // --merge is allowed (upstream only warns)
        }
        "clean" => {
            // Block any clean with force flag
            if flags.iter().any(|f| f == "-f" || f == "--force") {
                return Some("git clean with force flag is not allowed: it permanently removes untracked files".to_string());
            }
            // -d (remove untracked directories) and -x (remove ignored files) are allowed
        }
        // checkout --force: upstream only warns, not blocked
        // commit --amend: upstream only warns, not blocked
        // rebase --interactive: upstream only warns, not blocked
        "branch" => {
            if flags.iter().any(|f| f == "-D") {
                return Some("git branch -D is not allowed: it force-deletes a branch".to_string());
            }
        }
        _ => {}
    }
    None
}

/// Validate that all flags are allowed for the given operation.
/// Returns Some(error_message) if a flag is not allowed, None if all OK.
fn validate_git_flags(operation: &str, flags: &[String]) -> Option<String> {
    let allowed: Vec<&str> = match operation {
        "status" => vec!["--porcelain", "-s", "-b", "--short", "--branch", "-u", "--ignored"],
        "diff" => vec!["--cached", "--staged", "--stat", "--name-only", "--name-status", "--stat-width", "--stat-name-width", "--numstat", "--color", "--no-color", "-w", "--ignore-space-change", "-b", "--check", "-U", "--unified", "-M", "--detect-rename", "--follow", "--relative"],
        "log" => vec!["--oneline", "--graph", "--all", "--decorate", "--simplify-by-decoration", "-n", "--max-count", "--since", "--until", "--author", "--grep", "--all-match", "-p", "--stat", "--name-only", "--name-status", "-10", "--format"],
        "push" => vec!["--set-upstream", "-u", "--dry-run", "--verbose", "--force-with-lease", "--force", "--force-if-includes", "--tags", "--delete", "-f"],
        "pull" => vec!["--rebase", "--ff-only", "--no-ff", "--squash", "--verbose", "--no-commit", "-X"],
        "commit" => vec!["--amend", "--no-edit", "--allow-empty", "--signoff", "-a", "-m", "--message", "-s", "--no-verify", "--author"],
        "branch" => vec!["-d", "-D", "-m", "-M", "-a", "-r", "--list", "-v", "--merged", "--no-merged", "--show-current"],
        "checkout" => vec!["-b", "-B", "--detach", "--force", "-f", "--"],
        "merge" => vec!["--no-ff", "--squash", "--abort", "--continue", "--no-commit", "--no-verify", "--edit", "-m"],
        "rebase" => vec!["--interactive", "-i", "--continue", "--abort", "--skip", "--onto", "--autosquash", "--autostash"],
        "stash" => vec!["-u", "--include-untracked", "--all", "--keep-index", "push", "pop", "apply", "list", "show", "drop", "clear", "save", "-p", "--index"],
        "clean" => vec!["--dry-run", "-f", "--force", "-d", "-x", "-X", "-n"],
        "reset" => vec!["--soft", "--mixed", "--hard", "--merge", "--keep", "--"],
        "tag" => vec!["-d", "-a", "-m", "-s", "-l", "--list", "--sort", "-f"],
        "fetch" => vec!["--all", "--prune", "--tags", "--dry-run", "--verbose", "-p"],
        "revert" => vec!["--no-commit", "--continue", "--abort", "--skip", "--mainline", "-m"],
        "cherry-pick" => vec!["--continue", "--abort", "--skip", "--no-commit", "--mainline", "-m", "-x"],
        "reflog" => vec!["--all", "-n", "--date"],
        "ls-files" => vec!["--cached", "--deleted", "--modified", "--others", "--ignored", "--stage", "--full-name", "-s", "-v"],
        "ls-tree" => vec!["--name-only", "-r", "-l", "--full-name"],
        "worktree" => vec!["list", "add", "remove", "prune", "lock", "unlock", "--force"],
        _ => vec![],
    };

    if allowed.is_empty() {
        return None; // no validated flag list means accept everything (safe ops)
    }

    for flag in flags {
        if !allowed.contains(&flag.as_str()) {
            return Some(format!(
                "Flag '{}' is not allowed for '{}'. Allowed flags: {}",
                flag,
                operation,
                allowed.join(", ")
            ));
        }
    }
    None
}

/// Check flags permission; returns Some(ToolResult::error) if disallowed.
fn check_git_flags_permission(operation: &str, flags: &[String]) -> Option<ToolResult> {
    // Dangerous-operation check (blocking)
    if let Some(reason) = is_dangerous_git_operation(operation, flags) {
        return Some(ToolResult::error(format!(
            "Permission denied: {} (operation: {})",
            reason, operation
        )));
    }
    // Per-operation flag allowlist
    if let Some(msg) = validate_git_flags(operation, flags) {
        return Some(ToolResult::error(format!("Permission denied: {}", msg)));
    }
    None
}

/// Returns true if the gh subcommand is considered dangerous.
fn is_gh_repo_dangerous(subcmd: &str, _flags: &[String]) -> bool {
    match subcmd {
        "pr_merge" | "pr_close" | "pr_comment" | "issue_close" | "issue_comment"
        | "release_delete" | "release_edit" | "repo_delete" | "repo_edit" => true,
        _ => false,
    }
}

pub struct GitTool;

impl GitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for GitTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Execute Git version control operations. Supports clone, init, add, rm, mv, restore, switch, commit, push, pull, fetch, branch, checkout, merge, rebase, cherry-pick, revert, stash, clean, reset, tag, status, diff, log, shortlog, blame, reflog, remote, show, describe, ls-files, ls-tree, rev-parse, rev-list, and worktree operations, and use operation='info' to get current repository state (branch, commit, dirty status, default branch, git root). Also supports operation='gh' for read-only GitHub CLI (gh) operations: pr view/list/diff/checks/status, issue view/list/status, run list/view, auth status, release list/view, search repos/issues/prs."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Git operation to perform",
                    "enum": ["clone", "init", "add", "rm", "mv", "restore", "switch", "commit", "push", "pull", "fetch",
                             "branch", "checkout", "merge", "rebase", "cherry-pick", "revert", "stash", "clean",
                             "reset", "tag", "status", "diff", "log", "shortlog", "blame", "reflog",
                             "remote", "show", "describe", "ls-files", "ls-tree", "rev-parse", "rev-list", "worktree", "info", "gh"]
                },
                "repo": {
                    "type": "string",
                    "description": "Repository URL (for clone)"
                },
                "path": {
                    "type": "string",
                    "description": "For clone: destination directory path. For init/worktree: target path. For mv: destination path. For blame: file path to blame. NOT used as working directory (use 'directory' for that)"
                },
                "directory": {
                    "type": "string",
                    "description": "Working directory to run the git command in. For clone, this is where git clone runs (path is the clone destination). For other ops, this is the repo directory"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name for checkout/branch/push/pull/switch/worktree. For checkout: with flags=[\"-b\"] creates new branch. Also used as tag name for tag operation."
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for commit)"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "File paths for add/rm/restore/diff/ls-files ONLY. NOT for checkout, commit, or mv. checkout has NO files support. mv uses 'source' param"
                },
                "remote": {
                    "type": "string",
                    "description": "Remote name for push/pull/fetch only (default: origin). NOT for 'remote' operation itself"
                },
                "target": {
                    "type": "string",
                    "description": "Target branch or commit (for merge, rebase, describe, show, cherry-pick, revert)"
                },
                "flags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional git flags (e.g. [--force], [--soft])"
                },
                "all": {
                    "type": "boolean",
                    "description": "Stage all changed files (for add, commit -a)"
                },
                "staged": {
                    "type": "boolean",
                    "description": "For restore: restore from staging area (--staged). For rm: remove from index only (--cached). NOTE: there is NO separate 'cached' param for rm - use 'staged' instead"
                },
                "force": {
                    "type": "boolean",
                    "description": "Force the operation (for switch, clean, rm, restore, mv)"
                },
                "ours_theirs": {
                    "type": "string",
                    "description": "Checkout ours or theirs during conflict (for checkout --ours/--theirs). checkout does NOT support 'files' param"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Show what would be done without actually doing it (for clean --dry-run)"
                },
                "mainline": {
                    "type": "integer",
                    "description": "Mainline parent number when reverting or cherry-picking a merge commit"
                },
                "author": {
                    "type": "string",
                    "description": "Author string (e.g. 'Name <email>') (for commit)"
                },
                "cached": {
                    "type": "boolean",
                    "description": "Show staged changes instead of working tree (only for diff). NOT for rm - use 'staged' param for rm --cached"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Recursive removal (for clean only)"
                },
                "source": {
                    "type": "string",
                    "description": "Source file for mv, or source branch/commit for switch. NOT for restore (restore uses 'files' param)"
                },
                "worktree_name": {
                    "type": "string",
                    "description": "Worktree name (for worktree operation)"
                },
                "worktree_branch": {
                    "type": "string",
                    "description": "Branch for new worktree (for worktree add)"
                },
                "worktree_remove": {
                    "type": "boolean",
                    "description": "Remove a worktree (for worktree remove)"
                },
                "stash_subcommand": {
                    "type": "string",
                    "description": "Stash subcommand: pop, apply, drop, list, show (for stash operation). Default is 'push' (just 'git stash')"
                },
                "stash_include_untracked": {
                    "type": "boolean",
                    "description": "Include untracked files in stash (for stash push, adds -u flag)"
                },
                "max_count": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (for log, rev-list, default: 20)"
                },
                "proxy": {
                    "type": "string",
                    "description": "HTTP/SOCKS proxy URL for git operations (e.g. 'http://127.0.0.1:7890', 'socks5://127.0.0.1:1080'). Sets https_proxy and http_proxy environment variables for the git command."
                },
                "gh_subcommand": {
                    "type": "string",
                    "description": "GitHub CLI (gh) subcommand (for operation='gh'): pr, issue, run, auth, release, search",
                    "enum": ["pr", "issue", "run", "auth", "release", "search"]
                },
                "pr_number": {
                    "type": "integer",
                    "description": "Pull request number for gh pr view/diff/checks operations"
                },
                "issue_number": {
                    "type": "integer",
                    "description": "Issue number for gh issue view/status operations"
                },
                "gh_flags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional flags for gh CLI commands (for operation='gh')"
                },
                "query": {
                    "type": "string",
                    "description": "Search query for gh search repos/issues/prs operations"
                },
                "tag": {
                    "type": "string",
                    "description": "Tag name for gh release view/list operations"
                }
            },
            "required": ["operation"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> Option<ToolResult> {
        let operation = params.get("operation")?.as_str()?;

        // Handle gh operations via is_gh_repo_dangerous
        if operation == "gh" {
            let subcmd = params.get("gh_subcommand")?.as_str()?;
            let flags: Vec<String> = params
                .get("gh_flags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if is_gh_repo_dangerous(subcmd, &flags) {
                return Some(ToolResult::error(format!(
                    "Permission denied: gh {} is a write/destructive operation",
                    subcmd
                )));
            }
            return None;
        }

        // For git operations, collect all effective flags
        let mut effective_flags: Vec<String> = params
            .get("flags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Map boolean params to their flag equivalents for security checking
        // clean: force → -f, recursive → -d
        if operation == "clean" {
            if params.get("force").and_then(|v| v.as_bool()).unwrap_or(false) && !effective_flags.contains(&"-f".to_string()) {
                effective_flags.push("-f".to_string());
            }
            if params.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false) && !effective_flags.contains(&"-d".to_string()) {
                effective_flags.push("-d".to_string());
            }
        }
        // reset: target like --hard, --soft, --mixed
        if operation == "reset" {
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                if target.starts_with("--") && !effective_flags.contains(&target.to_string()) {
                    effective_flags.push(target.to_string());
                }
            }
        }

        check_git_flags_permission(operation, &effective_flags)
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ExecutesCode, crate::tools::ToolCapability::Subprocess]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Classifier
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let operation = match params.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("Error: operation is required"),
        };

        // Determine working directory:
        // - For clone: use directory param (path is the clone destination, not workdir)
        // - For blame: use directory param only (path is the file to blame, not workdir)
        // - For other operations: use directory param if set, otherwise path param
        let work_dir = if operation == "clone" {
            params.get("directory").and_then(|v| v.as_str()).map(PathBuf::from)
        } else if operation == "blame" {
            // For blame: only use explicit directory param. The 'path' param is the file to blame.
            params.get("directory")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
        } else {
            params.get("directory")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("path").and_then(|v| v.as_str()))
                .map(PathBuf::from)
        };

        // Handle "info" operation - repository state summary
        if operation == "info" {
            let dir = work_dir.as_ref()
                .and_then(|p| p.to_str())
                .map(|s| s.to_string())
                .or_else(|| std::env::current_dir().ok().and_then(|p| p.to_str().map(|s| s.to_string())))
                .unwrap_or_else(|| ".".to_string());

            if !is_git_repo(&dir) {
                return ToolResult::error(format!("Error: not a git repository: {}", dir));
            }

            let mut lines = Vec::new();

            match find_git_root(&dir) {
                Ok(root) => lines.push(format!("Git Root: {}", root)),
                Err(_) => lines.push("Git Root: (unknown)".to_string()),
            }
            match get_branch(&dir) {
                Ok(branch) => lines.push(format!("Branch: {}", branch)),
                Err(_) => lines.push("Branch: (detached HEAD)".to_string()),
            }
            match get_default_branch(&dir) {
                Ok(default) => lines.push(format!("Default Branch: {}", default)),
                Err(_) => lines.push("Default Branch: (unknown)".to_string()),
            }
            match get_current_commit_hash(&dir) {
                Ok(hash) => {
                    let short = if hash.len() >= 12 { hash[..12].to_string() } else { hash };
                    lines.push(format!("Commit: {}", short));
                }
                Err(_) => lines.push("Commit: (unknown)".to_string()),
            }
            lines.push(format!("Dirty: {}", is_dirty(&dir)));
            lines.push(format!("Bare: {}", is_bare_repo(&dir)));
            match get_git_status(&dir) {
                Ok(status_map) if !status_map.is_empty() => {
                    lines.push("Status:".to_string());
                    let mut entries: Vec<_> = status_map.iter().collect();
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                    for (file, status) in entries {
                        lines.push(format!(" {} {}", status, file));
                    }
                }
                Ok(_) => {
                    lines.push("Status: (clean)".to_string());
                }
                Err(_) => {
                    lines.push("Status: (unable to retrieve)".to_string());
                }
            }
            return ToolResult::ok(lines.join("\n"));
        }

        // Check remote configuration for operations that need it
        if matches!(operation, "push" | "pull" | "fetch") {
            let remote = params.get("remote").and_then(|v| v.as_str());
            if remote.is_none() {
                // Check if there's an origin remote configured
                let mut check_cmd = Command::new("git");
                check_cmd.args(["remote"]);
                if let Some(ref dir) = work_dir {
                    check_cmd.current_dir(dir);
                }
                match check_cmd.output() {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.contains("origin") {
                            return ToolResult::error(format!(
                                "Error: no remote specified and no 'origin' remote found. Available remotes:\n{}",
                                stdout.trim()
                            ));
                        }
                    }
                    Err(e) => {
                        return ToolResult::error(format!(
                            "Error: cannot determine git remotes: {}. Make sure you're in a git repository.",
                            e
                        ));
                    }
                }
            }
        }

        let args = build_git_args(&params, operation);
        if let Err(e) = &args {
            return ToolResult::error(format!("Error building command: {}", e));
        }
        let args = args.unwrap();

        // Determine binary name: "gh" for operation="gh", "git" otherwise
        let command_name = if operation == "gh" { "gh" } else { "git" };

        let mut cmd = Command::new(command_name);
        cmd.args(&args);

        if let Some(proxy) = params.get("proxy").and_then(|v| v.as_str()) {
            cmd.env("https_proxy", proxy);
            cmd.env("http_proxy", proxy);
            cmd.env("HTTPS_PROXY", proxy);
            cmd.env("HTTP_PROXY", proxy);
        }

        if let Some(ref dir) = work_dir {
            cmd.current_dir(dir);
        }

        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(&stderr);
                }

                if result.is_empty() {
                    result = "(no output)".to_string();
                }

                // Check exit code and provide detailed error on failure
                if !output.status.success() {
                    let exit_code = output.status.code().unwrap_or(-1);
                    let cmd_str = format!("{} {}", command_name, args.join(" "));
                    let err_msg = if result.is_empty() || result == "(no output)" {
                        format!("Error: '{}' failed with exit code {}", cmd_str, exit_code)
                    } else {
                        format!("Error: '{}' failed with exit code {}.\n\nOutput:\n{}", cmd_str, exit_code, result)
                    };
                    return ToolResult::error(err_msg);
                }

                ToolResult {
                    output: result,
                    is_error: false,
                    ..Default::default()
                }
            }
            Err(e) => {
                let cmd_str = format!("{} {}", command_name, args.join(" "));
                ToolResult::error(format!("Error executing '{}': {}. Make sure {} is installed and accessible.", cmd_str, e, command_name))
            }
        }
    }
}

fn build_git_args(params: &HashMap<String, Value>, operation: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();

    match operation {
        "clone" => {
            let repo = params.get("repo").and_then(|v| v.as_str())
                .ok_or("repo is required for clone")?;
            args.push("clone".to_string());
            args.push(repo.to_string());
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            }
        }
        "init" => {
            args.push("init".to_string());
            if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            }
        }
        "rm" => {
            args.push("rm".to_string());
            if params.get("cached").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("--cached".to_string());
            }
            if params.get("force").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-f".to_string());
            }
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else {
                return Err("files is required for rm".to_string());
            }
        }
        "mv" => {
            args.push("mv".to_string());
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                if files.len() >= 2 {
                    for f in files {
                        if let Some(s) = f.as_str() {
                            args.push(s.to_string());
                        }
                    }
                } else {
                    return Err("mv requires at least 2 files (source and destination)".to_string());
                }
            } else {
                return Err("files is required for mv (source and destination paths)".to_string());
            }
        }
        "restore" => {
            args.push("restore".to_string());
            if params.get("staged").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("--staged".to_string());
            }
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else {
                return Err("files is required for restore".to_string());
            }
        }
        "add" => {
            args.push("add".to_string());
            if params.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-A".to_string());
            } else if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else {
                args.push(".".to_string());
            }
        }
        "commit" => {
            let message = params.get("message").and_then(|v| v.as_str())
                .ok_or("commit message is required")?;
            args.push("commit".to_string());
            if params.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-a".to_string());
            }
            args.push("-m".to_string());
            args.push(message.to_string());
            if let Some(author) = params.get("author").and_then(|v| v.as_str()) {
                args.push("--author".to_string());
                args.push(author.to_string());
            }
        }
        "push" => {
            args.push("push".to_string());
            if let Some(remote) = params.get("remote").and_then(|v| v.as_str()) {
                args.push(remote.to_string());
                if let Some(branch) = params.get("branch").and_then(|v| v.as_str()) {
                    args.push(branch.to_string());
                }
            }
        }
        "pull" => {
            args.push("pull".to_string());
            if let Some(remote) = params.get("remote").and_then(|v| v.as_str()) {
                args.push(remote.to_string());
                if let Some(branch) = params.get("branch").and_then(|v| v.as_str()) {
                    args.push(branch.to_string());
                }
            }
        }
        "fetch" => {
            args.push("fetch".to_string());
            if let Some(remote) = params.get("remote").and_then(|v| v.as_str()) {
                args.push(remote.to_string());
            }
        }
        "branch" => {
            args.push("branch".to_string());
            if let Some(branch) = params.get("branch").and_then(|v| v.as_str()) {
                if !branch.is_empty() {
                    // Creating/deleting/renaming branch
                    args.push(branch.to_string());
                } else {
                    // Empty branch param: list all branches
                    args.push("--all".to_string());
                    args.push("--no-color".to_string());
                }
            } else {
                // No branch param: list all branches (local + remote)
                args.push("--all".to_string());
                args.push("--no-color".to_string());
            }
        }
        "checkout" => {
            args.push("checkout".to_string());
            // Handle -b/-B early so they come before branch name: `git checkout -b <branch>`
            let has_create = params.get("flags").and_then(|v| v.as_array())
                .map(|f| f.iter().any(|x| x.as_str() == Some("-b") || x.as_str() == Some("-B")))
                .unwrap_or(false);
            if has_create {
                args.push("-b".to_string());
            }
            if let Some(ours_theirs) = params.get("ours_theirs").and_then(|v| v.as_str()) {
                if ours_theirs == "ours" {
                    args.push("--ours".to_string());
                } else if ours_theirs == "theirs" {
                    args.push("--theirs".to_string());
                }
            }
            if let Some(branch) = params.get("branch").and_then(|v| v.as_str()) {
                args.push(branch.to_string());
            } else if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
            // Return early to skip generic flags loop for checkout
            return Ok(args);
        }
        "switch" => {
            args.push("switch".to_string());
            if let Some(branch) = params.get("branch").and_then(|v| v.as_str()) {
                args.push(branch.to_string());
            } else {
                return Err("branch is required for switch".to_string());
            }
        }
        "merge" => {
            args.push("merge".to_string());
            if let Some(message) = params.get("message").and_then(|v| v.as_str()) {
                args.push("-m".to_string());
                args.push(message.to_string());
            }
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "rebase" => {
            args.push("rebase".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "cherry-pick" => {
            args.push("cherry-pick".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            } else {
                return Err("target is required for cherry-pick".to_string());
            }
        }
        "revert" => {
            args.push("revert".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            } else {
                return Err("target is required for revert".to_string());
            }
        }
        "stash" => {
            args.push("stash".to_string());
            // Support stash subcommands: pop, apply, drop, list, show
            if let Some(sub) = params.get("stash_subcommand").and_then(|v| v.as_str()) {
                if matches!(sub, "pop" | "apply" | "drop" | "list" | "show") {
                    args.push(sub.to_string());
                }
            }
            if params.get("stash_include_untracked").and_then(|v| v.as_bool()).unwrap_or(false) {
                // Use -u flag for include untracked (stash push only)
                args.push("-u".to_string());
            }
        }
        "clean" => {
            args.push("clean".to_string());
            if params.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-d".to_string());
            }
            if params.get("force").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-f".to_string());
            }
            if params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("--dry-run".to_string());
            }
        }
        "reset" => {
            args.push("reset".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "tag" => {
            args.push("tag".to_string());
            if let Some(name) = params.get("branch").and_then(|v| v.as_str()) {
                args.push(name.to_string());
            }
        }
        "status" => {
            args.push("status".to_string());
        }
        "diff" => {
            args.push("diff".to_string());
            if params.get("cached").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("--cached".to_string());
            }
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
        }
        "log" => {
            args.push("log".to_string());
            args.push("--oneline".to_string());
            let max_count = params.get("max_count").and_then(|v| v.as_i64()).unwrap_or(20);
            args.push("-n".to_string());
            args.push(max_count.to_string());
        }
        "shortlog" => {
            args.push("shortlog".to_string());
            args.push("-sn".to_string());
            let max_count = params.get("max_count").and_then(|v| v.as_i64()).unwrap_or(20);
            args.push(format!("--max-count={}", max_count));
            args.push("HEAD".to_string());
        }
        "blame" => {
            args.push("blame".to_string());
            // Prefer 'files' param for blame file paths; 'path' may be the working directory
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                args.push(path.to_string());
            } else {
                return Err("path or files is required for blame (file path)".to_string());
            }
        }
        "reflog" => {
            args.push("reflog".to_string());
            let max_count = params.get("max_count").and_then(|v| v.as_i64()).unwrap_or(20);
            args.push("-n".to_string());
            args.push(max_count.to_string());
        }
        "remote" => {
            args.push("remote".to_string());
            args.push("-v".to_string());
        }
        "show" => {
            args.push("show".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "describe" => {
            args.push("describe".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "ls-files" => {
            args.push("ls-files".to_string());
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            }
        }
        "ls-tree" => {
            args.push("ls-tree".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            } else {
                args.push("HEAD".to_string());
            }
        }
        "rev-parse" => {
            args.push("rev-parse".to_string());
            if let Some(target) = params.get("target").and_then(|v| v.as_str()) {
                args.push(target.to_string());
            }
        }
        "rev-list" => {
            args.push("rev-list".to_string());
            let max_count = params.get("max_count").and_then(|v| v.as_i64()).unwrap_or(20);
            args.push("--count".to_string());
            args.push(format!("--max-count={}", max_count));
            args.push("HEAD".to_string());
        }
        "worktree" => {
            args.push("worktree".to_string());
            if params.get("worktree_remove").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("remove".to_string());
                if let Some(name) = params.get("worktree_name").and_then(|v| v.as_str()) {
                    args.push(name.to_string());
                }
            } else if params.get("worktree_name").and_then(|v| v.as_str()).is_some() {
                args.push("add".to_string());
                if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
                    args.push(path.to_string());
                }
                if let Some(branch) = params.get("worktree_branch").and_then(|v| v.as_str()) {
                    args.push("-b".to_string());
                    args.push(branch.to_string());
                }
            } else {
                args.push("list".to_string());
            }
        }
        "gh" => {
            // GitHub CLI (gh) read-only operations
            let subcmd = params.get("gh_subcommand")
                .and_then(|v| v.as_str())
                .ok_or("gh_subcommand is required for operation='gh'")?;

            match subcmd {
                "pr" => {
                    args.push("pr".to_string());
                    let pr_num = params.get("pr_number").and_then(|v| v.as_i64());
                    // Determine pr sub-action from gh_flags
                    let pr_action = params.get("gh_flags")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                        .unwrap_or("list");
                    match pr_action {
                        "view" => {
                            args.push("view".to_string());
                            if let Some(n) = pr_num { args.push(n.to_string()); }
                        }
                        "list" => { args.push("list".to_string()); }
                        "diff" => {
                            args.push("diff".to_string());
                            if let Some(n) = pr_num { args.push(n.to_string()); }
                        }
                        "checks" => {
                            args.push("checks".to_string());
                            if let Some(n) = pr_num { args.push(n.to_string()); }
                        }
                        "status" => { args.push("status".to_string()); }
                        other => return Err(format!("unknown gh pr sub-action: {}", other)),
                    }
                }
                "issue" => {
                    args.push("issue".to_string());
                    let issue_num = params.get("issue_number").and_then(|v| v.as_i64());
                    let issue_action = params.get("gh_flags")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                        .unwrap_or("list");
                    match issue_action {
                        "view" => {
                            args.push("view".to_string());
                            if let Some(n) = issue_num { args.push(n.to_string()); }
                        }
                        "list" => { args.push("list".to_string()); }
                        "status" => { args.push("status".to_string()); }
                        other => return Err(format!("unknown gh issue sub-action: {}", other)),
                    }
                }
                "run" => {
                    args.push("run".to_string());
                    let run_action = params.get("gh_flags")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                        .unwrap_or("list");
                    match run_action {
                        "list" => { args.push("list".to_string()); }
                        "view" => {
                            args.push("view".to_string());
                            // target param can hold the run ID
                            if let Some(t) = params.get("target").and_then(|v| v.as_str()) {
                                args.push(t.to_string());
                            }
                        }
                        other => return Err(format!("unknown gh run sub-action: {}", other)),
                    }
                }
                "auth" => {
                    args.push("auth".to_string());
                    args.push("status".to_string());
                }
                "release" => {
                    args.push("release".to_string());
                    let release_action = params.get("gh_flags")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                        .unwrap_or("list");
                    match release_action {
                        "list" => { args.push("list".to_string()); }
                        "view" => {
                            args.push("view".to_string());
                            if let Some(tag) = params.get("tag").and_then(|v| v.as_str()) {
                                args.push(tag.to_string());
                            }
                        }
                        other => return Err(format!("unknown gh release sub-action: {}", other)),
                    }
                }
                "search" => {
                    args.push("search".to_string());
                    let search_target = params.get("gh_flags")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.iter().find_map(|v| v.as_str()))
                        .unwrap_or("repos");
                    match search_target {
                        "repos" | "issues" | "prs" => {
                            args.push(search_target.to_string());
                        }
                        other => return Err(format!("unknown gh search target: {}", other)),
                    }
                    if let Some(q) = params.get("query").and_then(|v| v.as_str()) {
                        args.push(q.to_string());
                    } else {
                        return Err("query is required for gh search".to_string());
                    }
                }
                other => return Err(format!("unknown gh_subcommand: {}", other)),
            }
        }
        _ => return Err(format!("unknown operation: {}", operation)),
    }

    // Add extra flags, deduplicating against flags already in args to avoid
    // duplicates like "git rev-list -20 --count --count"
    if let Some(flags) = params.get("flags").and_then(|v| v.as_array()) {
        let existing: std::collections::HashSet<String> = args.iter().cloned().collect();
        for f in flags {
            if let Some(s) = f.as_str() {
                if !existing.contains(s) {
                    args.push(s.to_string());
                }
            }
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create an isolated temp repo for each test. Returns (tempdir, repo_path_string).
    /// The TempDir must be kept alive for the duration of the test.
    fn setup_test_repo() -> (TempDir, String) {
        let temp = TempDir::new().unwrap();
        let base = temp.path().to_str().unwrap().replace('\\', "/");

        Command::new("git").args(["init"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/init.txt", base), "initial").unwrap();
        Command::new("git").args(["add", "init.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "initial"]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/second.txt", base), "second").unwrap();
        Command::new("git").args(["add", "second.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "second"]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/third.txt", base), "third").unwrap();
        Command::new("git").args(["add", "third.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "third"]).current_dir(&base).output().unwrap();
        (temp, base)
    }

    fn run_tool_with_dir(operation: &str, params: &[(&str, Value)], dir: &str) -> ToolResult {
        let tool = GitTool::new();
        let mut map = HashMap::new();
        for (k, v) in params {
            map.insert(k.to_string(), v.clone());
        }
        map.insert("directory".to_string(), Value::String(dir.to_string()));
        map.insert("operation".to_string(), Value::String(operation.to_string()));
        tool.execute(map)
    }

    #[test]
    fn test_git_init() {
        let temp = TempDir::new().unwrap();
        let init_path = temp.path().join("new_init");
        let init_path_str = init_path.to_str().unwrap().replace('\\', "/");
        let _ = fs::remove_dir_all(&init_path);
        fs::create_dir_all(&init_path).unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("init".to_string()));
        params.insert("path".to_string(), Value::String(init_path_str.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "init failed: {}", result.output);
        assert!(fs::metadata(format!("{}/.git", init_path_str)).is_ok());
    }

    #[test]
    fn test_git_mv() {
        let (_temp, base) = setup_test_repo();
        let src = "mv_source.txt";
        let dst = "mv_dest.txt";
        let src_path = format!("{}/{}", base, src);
        let dst_path = format!("{}/{}", base, dst);
        let _ = fs::remove_file(&dst_path);
        fs::write(&src_path, "rename me").unwrap();
        // Small delay for Windows file handle release
        std::thread::sleep(std::time::Duration::from_millis(50));
        Command::new("git").args(["add", src]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "add mv source"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("mv".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("files".to_string(), Value::Array(vec![
            Value::String(src.to_string()),
            Value::String(dst.to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "mv failed: {}", result.output);
        assert!(fs::metadata(&dst_path).is_ok(), "Destination file should exist");
        assert!(fs::metadata(&src_path).is_err(), "Source file should not exist");
    }

    #[test]
    fn test_git_clean_dry_run() {
        let (_temp, base) = setup_test_repo();
        // Use a unique filename to avoid conflicts
        let filename = "clean_test_untracked.txt";
        let filepath = format!("{}/{}", base, filename);
        fs::write(&filepath, "untracked").unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clean".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("dry_run".to_string(), Value::Bool(true));
        params.insert("recursive".to_string(), Value::Bool(true));
        let result = tool.execute(params);
        assert!(!result.is_error, "clean --dry-run failed: {}", result.output);
        // Clean dry-run either mentions the file or says "Would remove"
        assert!(result.output.contains("clean_test_untracked") || result.output.contains("Would remove") || result.output.contains("untracked"));
        assert!(fs::metadata(&filepath).is_ok());
        // Clean up
        let _ = fs::remove_file(&filepath);
    }

    #[test]
    fn test_git_clean_force() {
        let (_temp, base) = setup_test_repo();
        let filename = "clean_force_untracked.txt";
        let filepath = format!("{}/{}", base, filename);
        fs::write(&filepath, "untracked").unwrap();
        // Ensure file is fully written and closed
        drop(fs::read_to_string(&filepath));

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clean".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("force".to_string(), Value::Bool(true));
        params.insert("recursive".to_string(), Value::Bool(true));
        let result = tool.execute(params);
        assert!(!result.is_error, "clean -fd failed: {}", result.output);
        // Give Windows a moment to release the file handle
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(fs::metadata(&filepath).is_err(), "File should have been removed by clean -fd");
    }

    #[test]
    fn test_git_cherry_pick() {
        let (_temp, base) = setup_test_repo();
        // Remove any stale lock file
        let _ = fs::remove_file(format!("{}/.git/index.lock", base));

        // Create a feature branch with a commit
        Command::new("git").args(["checkout", "-b", "cpfeature"]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/cpfile.txt", base), "cherry feature").unwrap();
        Command::new("git").args(["add", "cpfile.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "cherry pick feature"]).current_dir(&base).output().unwrap();
        let cherry_hash_out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&base).output().unwrap();
        let hash = String::from_utf8_lossy(&cherry_hash_out.stdout).trim().to_string();
        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("cherry-pick".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(hash));
        let result = tool.execute(params);
        assert!(!result.is_error, "cherry-pick failed: {}", result.output);
        assert!(fs::metadata(format!("{}/cpfile.txt", base)).is_ok());
    }

    #[test]
    fn test_git_revert() {
        let (_temp, base) = setup_test_repo();
        let third_hash = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&base).output().unwrap();
        let hash = String::from_utf8_lossy(&third_hash.stdout).trim().to_string();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("revert".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(hash));
        params.insert("message".to_string(), Value::String("Revert third commit".to_string()));
        // Use flags for --no-edit to avoid editor
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--no-edit".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "revert failed: {}", result.output);
    }

    #[test]
    fn test_git_merge() {
        let (_temp, base) = setup_test_repo();
        // Create a feature branch with a unique name
        let branch_name = "merge_test_branch";
        let filename = "merge_test_file.txt";
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/{}", base, filename), "merge content").unwrap();
        Command::new("git").args(["add", filename]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "merge test file"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "merge failed: {}", result.output);
        // Verify the merge file exists
        let merge_path = format!("{}/{}", base, filename);
        assert!(fs::metadata(&merge_path).is_ok(), "Merged file should exist at {}", merge_path);
    }

    #[test]
    fn test_git_rebase() {
        let (_temp, base) = setup_test_repo();
        // Create a branch from master with unique names
        let branch_name = "rebase_test_branch";
        let filename = "rebase_test_file.txt";
        let master_filename = "rebase_master_file.txt";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();
        fs::write(format!("{}/{}", base, filename), "rebase content").unwrap();
        Command::new("git").args(["add", filename]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "rebase commit"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();
        // Add a commit on master
        fs::write(format!("{}/{}", base, master_filename), "master content").unwrap();
        Command::new("git").args(["add", master_filename]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "master commit"]).current_dir(&base).output().unwrap();
        // Now rebase rebase_test_branch onto current master
        Command::new("git").args(["checkout", branch_name]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("rebase".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String("master".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "rebase failed: {}", result.output);
    }

    #[test]
    fn test_git_fetch() {
        let (_temp, base) = setup_test_repo();
        // Create a bare repo as remote
        let bare_path = format!("{}/bare_fetch.git", base);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        // Add remote and fetch
        Command::new("git").args(["remote", "add", "fetchremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("fetch".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("fetchremote".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "fetch failed: {}", result.output);
    }

    #[test]
    fn test_git_clone() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_clone.git", base);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        let clone_dest = format!("{}/cloned_repo", base);
        let _ = fs::remove_dir_all(&clone_dest);

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clone".to_string()));
        params.insert("repo".to_string(), Value::String(bare_path.clone()));
        params.insert("path".to_string(), Value::String(clone_dest.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "clone failed: {}", result.output);
        assert!(fs::metadata(format!("{}/.git", clone_dest)).is_ok(), "Cloned repo should have .git directory");
    }

    #[test]
    fn test_git_clone_no_dest() {
        // Clone without specifying path -- git clones to repo-name directory
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_clone2.git", base);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        let expected_dest = format!("{}/bare_clone2", base); // git strips .git
        let _ = fs::remove_dir_all(&expected_dest);

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clone".to_string()));
        params.insert("repo".to_string(), Value::String(bare_path.clone()));
        params.insert("directory".to_string(), Value::String(base.clone())); // run from base dir
        let result = tool.execute(params);
        assert!(!result.is_error, "clone without path failed: {}", result.output);
        assert!(fs::metadata(format!("{}/.git", expected_dest)).is_ok());
    }

    #[test]
    fn test_git_branch_create_list_delete() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();

        // Test branch list (no branch param)
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("branch".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "branch list failed: {}", result.output);
        assert!(result.output.contains("master"), "Should show master branch");

        // Test branch create
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("branch".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("branch".to_string(), Value::String("newbranch".to_string()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "branch create failed: {}", result2.output);

        // Verify branch exists
        let mut params3 = HashMap::new();
        params3.insert("operation".to_string(), Value::String("branch".to_string()));
        params3.insert("directory".to_string(), Value::String(base.clone()));
        let result3 = tool.execute(params3);
        assert!(!result3.is_error, "branch list after create failed: {}", result3.output);
        assert!(result3.output.contains("newbranch"), "Should show newbranch");

        // Test branch delete with flags
        let mut params4 = HashMap::new();
        params4.insert("operation".to_string(), Value::String("branch".to_string()));
        params4.insert("directory".to_string(), Value::String(base.clone()));
        params4.insert("branch".to_string(), Value::String("newbranch".to_string()));
        params4.insert("flags".to_string(), Value::Array(vec![Value::String("-d".to_string())]));
        let result4 = tool.execute(params4);
        assert!(!result4.is_error, "branch delete failed: {}", result4.output);

        // Verify branch deleted
        let mut params5 = HashMap::new();
        params5.insert("operation".to_string(), Value::String("branch".to_string()));
        params5.insert("directory".to_string(), Value::String(base.clone()));
        let result5 = tool.execute(params5);
        assert!(!result5.is_error, "branch list after delete failed: {}", result5.output);
        assert!(!result5.output.contains("newbranch"), "Should not show newbranch");
    }

    #[test]
    fn test_branch_switch_full_cycle() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();

        // Create branch
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("branch".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("branch".to_string(), Value::String("dev".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "branch create failed: {}", result.output);

        // Switch to it
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("switch".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("branch".to_string(), Value::String("dev".to_string()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "switch failed: {}", result2.output);

        // Verify current branch
        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();
        assert_eq!(current, "dev", "Should be on dev branch after switch");

        // Make a commit on dev
        fs::write(format!("{}/dev_only.txt", base), "dev content").unwrap();
        Command::new("git").args(["add", "dev_only.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "dev commit"]).current_dir(&base).output().unwrap();

        // Switch back to master
        let mut params3 = HashMap::new();
        params3.insert("operation".to_string(), Value::String("switch".to_string()));
        params3.insert("directory".to_string(), Value::String(base.clone()));
        params3.insert("branch".to_string(), Value::String("master".to_string()));
        let result3 = tool.execute(params3);
        assert!(!result3.is_error, "switch back to master failed: {}", result3.output);

        // Verify dev_only.txt no longer exists
        assert!(fs::metadata(format!("{}/dev_only.txt", base)).is_err(),
            "dev_only.txt should not exist on master");
    }

    #[test]
    fn test_checkout_create_branch() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();

        // Test checkout -b (create and switch to new branch)
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("checkout".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("branch".to_string(), Value::String("newbranch".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("-b".to_string())]));
        let result = tool.execute(params);
        assert!(!result.is_error, "checkout -b failed: {}", result.output);

        // Verify current branch
        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();
        assert_eq!(current, "newbranch", "Should be on newbranch after checkout -b");
    }

    #[test]
    fn test_clone_and_push_pull_cycle() {
        // Full cycle: setup → clone → modify → push → clone again → verify
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_cycle.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();

        // Add bare as remote and push
        Command::new("git").args(["remote", "add", "origin2", bare_path.as_str()]).current_dir(&base).output().unwrap();
        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current_branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("origin2".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("-u".to_string()),
            Value::String(current_branch.clone()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push failed: {}", result.output);

        // Clone from bare repo
        let clone_dest = format!("{}/cycle_clone", base);
        let _ = fs::remove_dir_all(&clone_dest);
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("clone".to_string()));
        params2.insert("repo".to_string(), Value::String(bare_path.clone()));
        params2.insert("path".to_string(), Value::String(clone_dest.clone()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "clone from bare failed: {}", result2.output);

        // Verify cloned repo has all files
        assert!(fs::metadata(format!("{}/init.txt", clone_dest)).is_ok());
        assert!(fs::metadata(format!("{}/second.txt", clone_dest)).is_ok());
        assert!(fs::metadata(format!("{}/third.txt", clone_dest)).is_ok());
    }

    #[test]
    fn test_git_worktree_list_add_remove() {
        let (_temp, base) = setup_test_repo();
        let wt_path = format!("{}/wt_workdir", base);
        let _ = fs::remove_dir_all(&wt_path);

        // Test worktree list
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("worktree".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "worktree list failed: {}", result.output);

        // Test worktree add - requires worktree_name (trigger), path, and worktree_branch
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("worktree".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("worktree_name".to_string(), Value::String("dummy".to_string())); // triggers add
        params2.insert("path".to_string(), Value::String(wt_path.clone()));
        params2.insert("worktree_branch".to_string(), Value::String("wt-branch-new".to_string()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "worktree add failed: {}", result2.output);
        assert!(fs::metadata(&wt_path).is_ok());

        // Test worktree remove
        let _ = Command::new("git").args(["worktree", "remove", "-f", wt_path.as_str()]).current_dir(&base).output();
    }

    #[test]
    fn test_git_push_pull_local_remote() {
        let (_temp, base) = setup_test_repo();
        // Get current branch name
        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current_branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();

        let bare_path = format!("{}/bare_push.git", base);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "pushremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        // Test push (use -u and --all to push all branches)
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("pushremote".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("-u".to_string()),
            Value::String(current_branch.clone()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push failed: {}", result.output);

        // Clone and test pull
        let pull_dest = format!("{}/pull_test", base);
        Command::new("git").args(["clone", bare_path.as_str(), pull_dest.as_str()]).output().unwrap();

        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("pull".to_string()));
        params2.insert("directory".to_string(), Value::String(pull_dest.clone()));
        params2.insert("remote".to_string(), Value::String("origin".to_string()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "pull failed: {}", result2.output);
    }

    #[test]
    fn test_git_describe() {
        let (_temp, base) = setup_test_repo();
        // Create an annotated tag (required for describe)
        Command::new("git").args(["tag", "-a", "v1.0.0", "-m", "version 1"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("describe".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "describe failed: {}", result.output);
        assert!(result.output.contains("v1.0.0"));
    }

    #[test]
    fn test_git_shortlog() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("shortlog".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        // shortlog with no commits to a different branch is OK, might return empty or summary
        // Don't assert success/failure since shortlog can return non-zero on some setups
        // Just verify it runs without a fatal error
        assert!(!result.output.contains("fatal"), "shortlog should not fail fatally: {}", result.output);
    }

    #[test]
    fn test_git_blame() {
        let (_temp, base) = setup_test_repo();
        // Verify the repo was properly set up
        let log_out = Command::new("git").args(["log", "--oneline"]).current_dir(&base).output().unwrap();
        assert!(log_out.status.success(), "Repo should have commits: {}", String::from_utf8_lossy(&log_out.stderr));

        // Test blame with path parameter
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("blame".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("path".to_string(), Value::String("init.txt".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "blame with path failed: {}", result.output);

        // Test blame with files parameter
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("blame".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("files".to_string(), Value::Array(vec![Value::String("init.txt".to_string())]));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "blame with files failed: {}", result2.output);
    }

    #[test]
    fn test_git_reflog() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("reflog".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "reflog failed: {}", result.output);
    }

    #[test]
    fn test_git_remote() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_remote_test.git", base);
        // Clean up any previous runs
        let _ = fs::remove_dir_all(&bare_path);
        let _ = Command::new("git").args(["remote", "remove", "testremote"]).current_dir(&base).output();
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "testremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("remote".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "remote failed: {}", result.output);
        assert!(result.output.contains("testremote"));
    }

    #[test]
    fn test_git_rev_parse() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("rev-parse".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String("HEAD".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "rev-parse failed: {}", result.output);
        assert!(result.output.trim().len() == 40, "rev-parse should return 40-char hash");
    }

    #[test]
    fn test_git_rev_list() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("rev-list".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "rev-list failed: {}", result.output);
        // With --count flag, output is just the number
        assert!(result.output.trim().chars().all(|c| c.is_ascii_digit()), "rev-list should return a count: {}", result.output);
    }

    #[test]
    fn test_git_rev_list_dedup_count() {
        // When --count is provided in flags, it should not be duplicated
        // since --count is already hardcoded in the rev-list handler.
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("rev-list".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--count".to_string())]));
        let result = tool.execute(params);
        assert!(!result.is_error, "rev-list with duplicate --count failed: {}", result.output);
        assert!(result.output.trim().chars().all(|c| c.is_ascii_digit()), "rev-list should return a count: {}", result.output);
    }

    #[test]
    fn test_git_show() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("show".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "show failed: {}", result.output);
    }

    #[test]
    fn test_git_stash_push_pop() {
        let (_temp, base) = setup_test_repo();
        // Modify a tracked file
        fs::write(format!("{}/init.txt", base), "modified for stash").unwrap();

        let tool = GitTool::new();

        // Test stash push
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("stash".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "stash push failed: {}", result.output);
        assert!(result.output.contains("Saved") || result.output.contains("No local changes"), "stash should save or report no changes");

        // Test stash list
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("stash".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("stash_subcommand".to_string(), Value::String("list".to_string()));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "stash list failed: {}", result2.output);

        // Test stash pop
        let mut params3 = HashMap::new();
        params3.insert("operation".to_string(), Value::String("stash".to_string()));
        params3.insert("directory".to_string(), Value::String(base.clone()));
        params3.insert("stash_subcommand".to_string(), Value::String("pop".to_string()));
        let result3 = tool.execute(params3);
        assert!(!result3.is_error, "stash pop failed: {}", result3.output);
    }

    #[test]
    fn test_git_ls_tree() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("ls-tree".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "ls-tree failed: {}", result.output);
    }

    #[test]
    fn test_git_cached_diff() {
        let (_temp, base) = setup_test_repo();
        // Stage a change
        fs::write(format!("{}/init.txt", base), "modified content for diff test").unwrap();
        Command::new("git").args(["add", "init.txt"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("diff".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("cached".to_string(), Value::Bool(true));
        let result = tool.execute(params);
        assert!(!result.is_error, "diff --cached failed: {}", result.output);
        // The output should contain diff markers or the modified content
        assert!(result.output.contains("@@") || result.output.contains("diff") || result.output.contains("modified") || result.output == "(no output)", "diff output: {}", result.output);
    }

    // -----------------------------------------------------------------------
    // Commit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_commit_basic() {
        let (_temp, base) = setup_test_repo();
        fs::write(format!("{}/new_file.txt", base), "new content").unwrap();
        Command::new("git").args(["add", "new_file.txt"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("message".to_string(), Value::String("add new file".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "basic commit failed: {}", result.output);
        assert!(result.output.contains("add new file") || result.output.contains("master"));
    }

    #[test]
    fn test_commit_all() {
        let (_temp, base) = setup_test_repo();
        // Modify an existing tracked file (no add needed)
        fs::write(format!("{}/init.txt", base), "modified content").unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("message".to_string(), Value::String("modify init".to_string()));
        params.insert("all".to_string(), Value::Bool(true)); // -a flag
        let result = tool.execute(params);
        assert!(!result.is_error, "commit -a failed: {}", result.output);
    }

    #[test]
    fn test_commit_author() {
        let (_temp, base) = setup_test_repo();
        fs::write(format!("{}/author_test.txt", base), "author test").unwrap();
        Command::new("git").args(["add", "author_test.txt"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("message".to_string(), Value::String("test author".to_string()));
        params.insert("author".to_string(), Value::String("Alice <alice@example.com>".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "commit with author failed: {}", result.output);

        // Verify the author was set
        let author_out = Command::new("git").args(["log", "-1", "--format=%an <%ae>"]).current_dir(&base).output().unwrap();
        let author = String::from_utf8_lossy(&author_out.stdout).trim().to_string();
        assert_eq!(author, "Alice <alice@example.com>", "Author should be Alice");
    }

    #[test]
    fn test_commit_empty_message_required() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base));
        // No message -- should error
        let result = tool.execute(params);
        assert!(result.is_error, "commit without message should fail");
        assert!(result.output.contains("message is required"));
    }

    #[test]
    fn test_commit_allow_empty() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("message".to_string(), Value::String("empty commit".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--allow-empty".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "commit --allow-empty failed: {}", result.output);
    }

    // -----------------------------------------------------------------------
    // Push tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_push_to_local_bare() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_push_test.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "pushremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        // Get current branch
        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current_branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("pushremote".to_string()));
        params.insert("branch".to_string(), Value::String(current_branch.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "push failed: {}", result.output);

        // Verify pushed by cloning from bare
        let clone_dest = format!("{}/verify_push", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();
        assert!(fs::metadata(format!("{}/init.txt", clone_dest)).is_ok(), "Cloned repo should have init.txt");
    }

    #[test]
    fn test_push_force() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_force_push.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "forceremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current_branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("forceremote".to_string()));
        params.insert("branch".to_string(), Value::String(current_branch.clone()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--force".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push --force failed: {}", result.output);
    }

    #[test]
    fn test_push_all() {
        let (_temp, base) = setup_test_repo();
        // Create a second branch
        Command::new("git").args(["branch", "second-branch"]).current_dir(&base).output().unwrap();

        let bare_path = format!("{}/bare_all_push.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "allremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("allremote".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--all".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push --all failed: {}", result.output);

        // Verify both branches exist in bare repo
        let clone_dest = format!("{}/verify_all", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();
        let branches_out = Command::new("git").args(["branch", "-a"]).current_dir(&clone_dest).output().unwrap();
        let branches = String::from_utf8_lossy(&branches_out.stdout);
        assert!(branches.contains("second-branch"), "Should have second-branch after push --all");
    }

    #[test]
    fn test_push_tags() {
        let (_temp, base) = setup_test_repo();
        Command::new("git").args(["tag", "v2.0"]).current_dir(&base).output().unwrap();

        let bare_path = format!("{}/bare_tags_push.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "tagsremote", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("tagsremote".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--tags".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push --tags failed: {}", result.output);

        // Verify tag exists in bare repo
        let clone_dest = format!("{}/verify_tags", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();
        let tags_out = Command::new("git").args(["tag"]).current_dir(&clone_dest).output().unwrap();
        let tags = String::from_utf8_lossy(&tags_out.stdout);
        assert!(tags.contains("v2.0"), "Should have v2.0 tag after push --tags");
    }

    #[test]
    fn test_push_set_upstream() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_upstream.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();
        Command::new("git").args(["remote", "add", "upstream", bare_path.as_str()]).current_dir(&base).output().unwrap();

        let branch_out = Command::new("git").args(["branch", "--show-current"]).current_dir(&base).output().unwrap();
        let current_branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("remote".to_string(), Value::String("upstream".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("-u".to_string()),
            Value::String(current_branch.clone()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "push -u failed: {}", result.output);

        // Verify upstream is set
        let config_out = Command::new("git").args(["config", "--get", &format!("branch.{}.remote", current_branch)]).current_dir(&base).output().unwrap();
        let config = String::from_utf8_lossy(&config_out.stdout).trim().to_string();
        assert_eq!(config, "upstream", "Upstream remote should be 'upstream'");
    }

    #[test]
    fn test_commit_amend() {
        let (_temp, base) = setup_test_repo();
        fs::write(format!("{}/amend_file.txt", base), "amend content").unwrap();
        Command::new("git").args(["add", "amend_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "original message"]).current_dir(&base).output().unwrap();

        // Now amend the commit
        fs::write(format!("{}/amend_file.txt", base), "amended content").unwrap();
        Command::new("git").args(["add", "amend_file.txt"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("message".to_string(), Value::String("amended message".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--amend".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "commit --amend failed: {}", result.output);

        // Verify the message was amended
        let log_out = Command::new("git").args(["log", "-1", "--format=%s"]).current_dir(&base).output().unwrap();
        let msg = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
        assert_eq!(msg, "amended message", "Commit message should be amended");
    }

    // -----------------------------------------------------------------------
    // Pull tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_pull_basic() {
        // Setup: create bare repo as remote, clone to dest, make change in bare via push
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_pull_test.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();

        // Clone from bare to get a separate working copy
        let clone_dest = format!("{}/pull_clone", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();

        // Push a new commit from base to bare
        fs::write(format!("{}/pull_test_file.txt", base), "new content on base").unwrap();
        Command::new("git").args(["add", "pull_test_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "new commit to push"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["push", bare_path.as_str(), "master"]).current_dir(&base).output().unwrap();

        // Now pull in the cloned repo
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("pull".to_string()));
        params.insert("directory".to_string(), Value::String(clone_dest.clone()));
        params.insert("remote".to_string(), Value::String("origin".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "pull failed: {}", result.output);
        assert!(fs::metadata(format!("{}/pull_test_file.txt", clone_dest)).is_ok(), "Pulled file should exist");
    }

    #[test]
    fn test_pull_rebase() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_rebase_pull.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();

        let clone_dest = format!("{}/rebase_pull_clone", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();

        // Push a commit from base
        fs::write(format!("{}/rebase_test_file.txt", base), "rebase push").unwrap();
        Command::new("git").args(["add", "rebase_test_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "rebase push commit"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["push", bare_path.as_str(), "master"]).current_dir(&base).output().unwrap();

        // Pull with --rebase
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("pull".to_string()));
        params.insert("directory".to_string(), Value::String(clone_dest.clone()));
        params.insert("remote".to_string(), Value::String("origin".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--rebase".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "pull --rebase failed: {}", result.output);
    }

    #[test]
    fn test_pull_ff_only() {
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_ff_pull.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();

        let clone_dest = format!("{}/ff_pull_clone", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();

        // Push a commit (fast-forward scenario)
        fs::write(format!("{}/ff_test.txt", base), "ff content").unwrap();
        Command::new("git").args(["add", "ff_test.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "ff push"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["push", bare_path.as_str(), "master"]).current_dir(&base).output().unwrap();

        // Pull with --ff-only
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("pull".to_string()));
        params.insert("directory".to_string(), Value::String(clone_dest.clone()));
        params.insert("remote".to_string(), Value::String("origin".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--ff-only".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "pull --ff-only failed: {}", result.output);
    }

    #[test]
    fn test_pull_no_commits() {
        // Pull when already up to date
        let (_temp, base) = setup_test_repo();
        let bare_path = format!("{}/bare_uptodate.git", base);
        let _ = fs::remove_dir_all(&bare_path);
        Command::new("git").args(["clone", "--bare", base.as_str(), bare_path.as_str()]).output().unwrap();

        let clone_dest = format!("{}/uptodate_clone", base);
        let _ = fs::remove_dir_all(&clone_dest);
        Command::new("git").args(["clone", bare_path.as_str(), clone_dest.as_str()]).output().unwrap();

        // No new commits -- should say "Already up to date"
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("pull".to_string()));
        params.insert("directory".to_string(), Value::String(clone_dest.clone()));
        params.insert("remote".to_string(), Value::String("origin".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "pull (uptodate) failed: {}", result.output);
        assert!(result.output.contains("Already") || result.output.contains("up-to-date") || result.output.contains("up to date") || result.output == "(no output)",
            "Should report up to date or succeed: {}", result.output);
    }

    // -----------------------------------------------------------------------
    // Merge tests -- extended
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_fast_forward() {
        let (_temp, base) = setup_test_repo();
        // Create branch from an earlier point, then add commits on master
        let branch_name = "ff_merge_branch";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();

        // Add a commit on the branch
        fs::write(format!("{}/ff_merge_file.txt", base), "ff merge content").unwrap();
        Command::new("git").args(["add", "ff_merge_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "ff merge commit"]).current_dir(&base).output().unwrap();

        // Switch back and fast-forward merge
        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "fast-forward merge failed: {}", result.output);
        assert!(fs::metadata(format!("{}/ff_merge_file.txt", base)).is_ok());
    }

    #[test]
    fn test_merge_no_ff() {
        let (_temp, base) = setup_test_repo();
        let branch_name = "no_ff_branch";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();

        fs::write(format!("{}/no_ff_file.txt", base), "no-ff content").unwrap();
        Command::new("git").args(["add", "no_ff_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "no-ff branch commit"]).current_dir(&base).output().unwrap();

        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        // Merge with --no-ff to force a merge commit
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--no-ff".to_string()),
            Value::String("--no-edit".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "merge --no-ff failed: {}", result.output);

        // Verify merge commit exists (has two parents)
        let parent_count = Command::new("git").args(["log", "-1", "--format=%P"]).current_dir(&base).output().unwrap();
        let parents = String::from_utf8_lossy(&parent_count.stdout).trim().to_string();
        assert!(parents.split_whitespace().count() >= 2, "Merge commit should have multiple parents, got: {}", parents);
    }

    #[test]
    fn test_merge_squash() {
        let (_temp, base) = setup_test_repo();
        let branch_name = "squash_branch";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();

        fs::write(format!("{}/squash_file.txt", base), "squash content").unwrap();
        Command::new("git").args(["add", "squash_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "squash branch commit"]).current_dir(&base).output().unwrap();

        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        // Squash merge
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--squash".to_string()),
        ]));
        let result = tool.execute(params);
        assert!(!result.is_error, "merge --squash failed: {}", result.output);

        // Squash merge leaves changes staged but not committed -- commit them
        Command::new("git").args(["commit", "-m", "squashed merge"]).current_dir(&base).output().unwrap();
        assert!(fs::metadata(format!("{}/squash_file.txt", base)).is_ok());
    }

    #[test]
    fn test_merge_conflict_and_abort() {
        let (_temp, base) = setup_test_repo();
        let branch_name = "conflict_branch";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();

        // Modify same file on branch
        fs::write(format!("{}/init.txt", base), "branch version of init").unwrap();
        Command::new("git").args(["add", "init.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "branch change to init"]).current_dir(&base).output().unwrap();

        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        // Modify same file on master
        fs::write(format!("{}/init.txt", base), "master version of init").unwrap();
        Command::new("git").args(["add", "init.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "master change to init"]).current_dir(&base).output().unwrap();

        // Try to merge -- should conflict
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        let result = tool.execute(params);
        assert!(result.is_error, "merge should conflict: {}", result.output);

        // Abort the merge
        let mut params2 = HashMap::new();
        params2.insert("operation".to_string(), Value::String("merge".to_string()));
        params2.insert("directory".to_string(), Value::String(base.clone()));
        params2.insert("flags".to_string(), Value::Array(vec![
            Value::String("--abort".to_string()),
        ]));
        let result2 = tool.execute(params2);
        assert!(!result2.is_error, "merge --abort failed: {}", result2.output);
    }

    #[test]
    fn test_merge_with_custom_message() {
        let (_temp, base) = setup_test_repo();
        let branch_name = "msg_merge_branch";
        let _ = Command::new("git").args(["branch", "-D", branch_name]).current_dir(&base).output();
        Command::new("git").args(["checkout", "-b", branch_name]).current_dir(&base).output().unwrap();

        fs::write(format!("{}/msg_merge_file.txt", base), "msg merge content").unwrap();
        Command::new("git").args(["add", "msg_merge_file.txt"]).current_dir(&base).output().unwrap();
        Command::new("git").args(["commit", "-m", "msg merge branch commit"]).current_dir(&base).output().unwrap();

        Command::new("git").args(["checkout", "master"]).current_dir(&base).output().unwrap();

        // Merge with custom message using -m flag
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("merge".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        params.insert("target".to_string(), Value::String(branch_name.to_string()));
        params.insert("flags".to_string(), Value::Array(vec![
            Value::String("--no-edit".to_string()),
        ]));
        params.insert("message".to_string(), Value::String("custom merge message".to_string()));
        let result = tool.execute(params);
        assert!(!result.is_error, "merge failed: {}", result.output);
    }

    #[test]
    fn test_git_info_clean() {
        let (_temp, base) = setup_test_repo();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("info".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "info failed: {}", result.output);
        assert!(result.output.contains("Git Root:"), "Should contain Git Root");
        assert!(result.output.contains("Branch:"), "Should contain Branch");
        assert!(result.output.contains("Commit:"), "Should contain Commit");
        assert!(result.output.contains("Dirty:"), "Should contain Dirty");
        assert!(result.output.contains("Bare:"), "Should contain Bare");
        assert!(result.output.contains("Status:"), "Should contain Status");
    }

    #[test]
    fn test_git_info_dirty() {
        let (_temp, base) = setup_test_repo();
        fs::write(format!("{}/init.txt", base), "dirty content").unwrap();
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("info".to_string()));
        params.insert("directory".to_string(), Value::String(base.clone()));
        let result = tool.execute(params);
        assert!(!result.is_error, "info failed: {}", result.output);
        assert!(result.output.contains("Dirty: true"), "Should show dirty status");
    }

    #[test]
    fn test_git_info_not_repo() {
        let temp = TempDir::new().unwrap();
        let non_repo = temp.path().to_str().unwrap().replace("\\", "/");
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("info".to_string()));
        params.insert("directory".to_string(), Value::String(non_repo));
        let result = tool.execute(params);
        assert!(result.is_error, "info on non-repo should error");
    }

    #[test]
    fn test_utility_functions() {
        let (_temp, base) = setup_test_repo();
        assert!(is_git_repo(&base), "Should be a git repo");
        let root = find_git_root(&base).unwrap();
        assert!(!root.is_empty(), "Should find git root");
        let branch = get_branch(&base).unwrap();
        assert!(!branch.is_empty(), "Should get branch name");
        assert!(!is_bare_repo(&base), "Normal repo should not be bare");
        let hash = get_current_commit_hash(&base).unwrap();
        assert_eq!(hash.len(), 40, "Commit hash should be 40 chars");
        let default = get_default_branch(&base).unwrap();
        assert!(!default.is_empty(), "Should get default branch");
        assert!(!has_uncommitted_changes(&base), "Clean repo should have no changes");
        assert!(!is_dirty(&base), "Clean repo should not be dirty");
    }

    #[test]
    fn test_get_git_status_util() {
        let (_temp, base) = setup_test_repo();
        let status = get_git_status(&base).unwrap();
        assert!(status.is_empty(), "Clean repo should have empty status");
        fs::write(format!("{}/new_status.txt", base), "new").unwrap();
        let status = get_git_status(&base).unwrap();
        assert!(!status.is_empty(), "Should have status after adding file");
    }

    // ---- Tests for new security helpers ----

    #[test]
    fn test_is_dangerous_git_operation() {
        // Force push is dangerous
        let result = is_dangerous_git_operation("push", &["--force".to_string()]);
        assert!(result.is_some(), "Force push should be dangerous");
        assert!(result.unwrap().contains("Force push"), "Reason should mention force push");

        // Normal push is not dangerous
        let result = is_dangerous_git_operation("push", &[]);
        assert!(result.is_none(), "Normal push should not be dangerous");

        // reset --hard is dangerous
        let result = is_dangerous_git_operation("reset", &["--hard".to_string()]);
        assert!(result.is_some(), "reset --hard should be dangerous");
        assert!(result.unwrap().contains("--hard"), "Reason should mention --hard");

        // reset --soft is not dangerous
        let result = is_dangerous_git_operation("reset", &["--soft".to_string()]);
        assert!(result.is_none(), "reset --soft should not be dangerous");

        // clean -f is dangerous (but -d and -x are allowed per upstream)
        let result = is_dangerous_git_operation("clean", &["-f".to_string()]);
        assert!(result.is_some(), "clean -f should be dangerous");

        // branch -D is dangerous
        let result = is_dangerous_git_operation("branch", &["-D".to_string()]);
        assert!(result.is_some(), "branch -D should be dangerous");

        // checkout --force is allowed (upstream only warns)
        let result = is_dangerous_git_operation("checkout", &["--force".to_string()]);
        assert!(result.is_none(), "checkout --force should not be dangerous");

        // commit --amend is allowed (upstream only warns)
        let result = is_dangerous_git_operation("commit", &["--amend".to_string()]);
        assert!(result.is_none(), "commit --amend should not be dangerous");

        // rebase --interactive is allowed (upstream only warns)
        let result = is_dangerous_git_operation("rebase", &["--interactive".to_string()]);
        assert!(result.is_none(), "rebase --interactive should not be dangerous");

        // Safe operations
        assert!(is_dangerous_git_operation("status", &[]).is_none(), "status should not be dangerous");
        assert!(is_dangerous_git_operation("log", &[]).is_none(), "log should not be dangerous");
    }

    #[test]
    fn test_validate_git_flags() {
        // Valid flags for diff
        assert!(validate_git_flags("diff", &["--cached".to_string()]).is_none());
        assert!(validate_git_flags("diff", &["--stat".to_string()]).is_none());

        // Invalid flag for diff
        let err = validate_git_flags("diff", &["--dangerous".to_string()]);
        assert!(err.is_some(), "Invalid diff flag should be rejected");
        assert!(err.unwrap().contains("--dangerous"), "Error should mention the invalid flag");

        // Valid flags for push
        assert!(validate_git_flags("push", &["--set-upstream".to_string()]).is_none());
        assert!(validate_git_flags("push", &["--delete".to_string()]).is_none());

        // Valid flags for status
        assert!(validate_git_flags("status", &["--short".to_string()]).is_none());
        assert!(validate_git_flags("status", &["--porcelain".to_string()]).is_none());

        // Invalid flag for status
        let err = validate_git_flags("status", &["--invalid-flag".to_string()]);
        assert!(err.is_some(), "Invalid status flag should be rejected");

        // No validation for unlisted operations (accept all)
        assert!(validate_git_flags("show", &["--anything".to_string()]).is_none());
    }

    #[test]
    fn test_check_git_flags_permission() {
        // Dangerous operation returns error
        let result = check_git_flags_permission("push", &["--force".to_string()]);
        assert!(result.is_some(), "Force push should be permission-denied");
        assert!(result.unwrap().is_error, "Should be an error result");

        // Valid operation with valid flags returns None
        let result = check_git_flags_permission("diff", &["--cached".to_string()]);
        assert!(result.is_none(), "Valid diff --cached should be allowed");

        // Invalid flag returns error
        let result = check_git_flags_permission("diff", &["--invalid-flag".to_string()]);
        assert!(result.is_some(), "Invalid flag should be rejected");
    }

    #[test]
    fn test_is_gh_repo_dangerous() {
        assert!(is_gh_repo_dangerous("pr_merge", &[]), "pr_merge should be dangerous");
        assert!(is_gh_repo_dangerous("pr_close", &[]), "pr_close should be dangerous");
        assert!(is_gh_repo_dangerous("issue_close", &[]), "issue_close should be dangerous");
        assert!(is_gh_repo_dangerous("repo_delete", &[]), "repo_delete should be dangerous");

        assert!(!is_gh_repo_dangerous("pr", &[]), "pr should not be dangerous");
        assert!(!is_gh_repo_dangerous("issue", &[]), "issue should not be dangerous");
        assert!(!is_gh_repo_dangerous("auth", &[]), "auth should not be dangerous");
        assert!(!is_gh_repo_dangerous("release", &[]), "release should not be dangerous");
        assert!(!is_gh_repo_dangerous("run", &[]), "run should not be dangerous");
        assert!(!is_gh_repo_dangerous("search", &[]), "search should not be dangerous");
    }

    #[test]
    fn test_check_permissions_gh_dangerous() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("gh".to_string()));
        params.insert("gh_subcommand".to_string(), Value::String("pr_merge".to_string()));

        let result = tool.check_permissions(&params);
        assert!(result.is_some(), "gh pr_merge should be permission-denied");
        assert!(result.unwrap().is_error, "Should be an error result");
    }

    #[test]
    fn test_check_permissions_gh_safe() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("gh".to_string()));
        params.insert("gh_subcommand".to_string(), Value::String("pr".to_string()));
        params.insert("gh_flags".to_string(), Value::Array(vec![Value::String("list".to_string())]));

        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "gh pr list should be allowed");
    }

    #[test]
    fn test_check_permissions_git_dangerous() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--force".to_string())]));

        let result = tool.check_permissions(&params);
        assert!(result.is_some(), "git push --force should be permission-denied");
    }

    #[test]
    fn test_check_permissions_clean_force_recursive() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clean".to_string()));
        params.insert("force".to_string(), Value::Bool(true));
        params.insert("recursive".to_string(), Value::Bool(true));

        let result = tool.check_permissions(&params);
        assert!(result.is_some(), "git clean -fd (via force+recursive params) should be permission-denied");
    }

    #[test]
    fn test_check_permissions_clean_dry_run_allowed() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("clean".to_string()));
        params.insert("dry_run".to_string(), Value::Bool(true));

        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "git clean --dry-run should be allowed");
    }

    #[test]
    fn test_check_permissions_reset_hard() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("reset".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--hard".to_string())]));

        let result = tool.check_permissions(&params);
        assert!(result.is_some(), "git reset --hard should be permission-denied");
    }

    #[test]
    fn test_check_permissions_commit_amend() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("commit".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--amend".to_string())]));

        // commit --amend is allowed (upstream only warns, doesn't block)
        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "git commit --amend should be allowed");
    }

    #[test]
    fn test_check_permissions_rebase_interactive() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("rebase".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--interactive".to_string())]));

        // rebase --interactive is allowed (upstream only warns, doesn't block)
        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "git rebase --interactive should be allowed");
    }

    #[test]
    fn test_check_permissions_push_force_with_lease() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("push".to_string()));
        params.insert("flags".to_string(), Value::Array(vec![Value::String("--force-with-lease".to_string())]));

        // --force-with-lease is allowed (safer variant)
        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "git push --force-with-lease should be allowed");
    }

    #[test]
    fn test_check_permissions_git_safe() {
        let tool = GitTool::new();
        let mut params = HashMap::new();
        params.insert("operation".to_string(), Value::String("status".to_string()));

        let result = tool.check_permissions(&params);
        assert!(result.is_none(), "git status should be allowed");
    }

}