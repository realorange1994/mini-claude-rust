//! GitTool - Git version control operations

use crate::tools::{Tool, ToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

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
        "Execute Git version control operations. Supports clone, init, add, rm, mv, restore, switch, commit, push, pull, fetch, branch, checkout, merge, rebase, cherry-pick, revert, stash, clean, reset, tag, status, diff, log, shortlog, blame, reflog, remote, show, describe, ls-files, ls-tree, rev-parse, rev-list, and worktree operations."
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
                             "remote", "show", "describe", "ls-files", "ls-tree", "rev-parse", "rev-list", "worktree"]
                },
                "repo": {
                    "type": "string",
                    "description": "Repository URL (for clone)"
                },
                "path": {
                    "type": "string",
                    "description": "Local path (clone destination, or target for init/worktree)"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name (for checkout, branch, push, pull, worktree)"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for commit)"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to stage (for add), remove (for rm), move (for mv), restore (for restore), show diff (for diff), list (for ls-files), or blame (for blame)"
                },
                "remote": {
                    "type": "string",
                    "description": "Remote name (default: origin)"
                },
                "target": {
                    "type": "string",
                    "description": "Target branch or commit (for merge, rebase, describe)"
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
                    "description": "Restore the index only (for restore --staged)"
                },
                "worktree": {
                    "type": "boolean",
                    "description": "Restore the working tree (default for restore)"
                },
                "force": {
                    "type": "boolean",
                    "description": "Force operation (for switch -f, clean -f, cherry-pick --continue)"
                },
                "ours_theirs": {
                    "type": "string",
                    "description": "Checkout ours or theirs during merge conflict (for checkout --ours/--theirs)"
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
                    "description": "Author for commit (--author='Name <email>')"
                },
                "cached": {
                    "type": "boolean",
                    "description": "Remove from index only, not working tree (for rm --cached)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Allow recursive removal when a trailing slash is used (for rm -r)"
                },
                "source": {
                    "type": "string",
                    "description": "Source commit/tree to restore from (for restore --source)"
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
                "max_count": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (for log, rev-list, default: 20)"
                },
                "proxy": {
                    "type": "string",
                    "description": "HTTP/SOCKS proxy URL for git operations (e.g. 'http://127.0.0.1:7890', 'socks5://127.0.0.1:1080'). Sets https_proxy and http_proxy environment variables for the git command."
                }
            },
            "required": ["operation"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let operation = match params.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return ToolResult::error("Error: operation is required"),
        };

        let work_dir = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let args = build_git_args(&params, operation);
        if args.is_err() {
            return ToolResult::error(args.unwrap_err());
        }

        let mut cmd = Command::new("git");
        cmd.args(&args.unwrap());

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

                ToolResult {
                    output: result,
                    is_error: !output.status.success(),
                }
            }
            Err(e) => ToolResult::error(format!("Error: {}", e)),
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
            if params.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false) {
                args.push("-r".to_string());
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
            if params.get("source").and_then(|v| v.as_str()).is_some() {
                args.push("--source".to_string());
                args.push(params["source"].as_str().unwrap().to_string());
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
                args.push(branch.to_string());
            }
        }
        "checkout" => {
            args.push("checkout".to_string());
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
            if let Some(files) = params.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        args.push(s.to_string());
                    }
                }
            } else {
                return Err("files is required for blame (file path)".to_string());
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
        _ => return Err(format!("unknown operation: {}", operation)),
    }

    // Add extra flags
    if let Some(flags) = params.get("flags").and_then(|v| v.as_array()) {
        for f in flags {
            if let Some(s) = f.as_str() {
                args.push(s.to_string());
            }
        }
    }

    Ok(args)
}
