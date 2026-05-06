//! ExecTool - Shell command execution with security guards
//!
//! This module also contains the background bash task tools (TaskStopTool,
//! TaskOutputTool) and the background bash spawning engine that was previously
//! in bash_task_tools.rs.

use crate::tools::{Tool, ToolResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;

// ─── Security Helper Functions ─────────────────────────────────────────────────

/// Safe environment variable names that are allowed in ${...} expansion.
/// Dangerous vars like PATH, PYTHONPATH, GOFLAGS, NODE_OPTIONS, GIT_DIR
/// are excluded as they can redirect code execution, import arbitrary
/// modules, or bypass security. Matches upstream's SAFE_ENV_VARS principles.
const SAFE_ENV_VARS: &[&str] = &[
    // Go - build/runtime settings only
    "GOOS", "GOARCH", "GOEXPERIMENT", "GO111MODULE", "CGO_ENABLED",
    // Rust - logging/debugging only
    "RUST_BACKTRACE", "RUST_LOG",
    // Node - environment name only (NOT NODE_OPTIONS)
    "NODE_ENV", "NPM_CONFIG_REGISTRY",
    // Python - behavior flags only (NOT PYTHONPATH)
    "PYTHONIOENCODING", "PYTHONDONTWRITEBYTECODE",
    // Pytest
    "PYTEST_DISABLE_PLUGIN_AUTOLOAD", "PYTEST_DEBUG",
    // API keys
    "ANTHROPIC_API_KEY",
    // Locale and terminal
    "LANG", "LC_ALL", "LC_CTYPE", "LC_TIME", "CHARSET",
    "TERM", "COLORTERM", "NO_COLOR", "FORCE_COLOR", "TZ",
    // Color configuration
    "LS_COLORS", "LSCOLORS", "GREP_COLOR", "GREP_COLORS", "GCC_COLORS",
    // Display formatting
    "TIME_STYLE", "BLOCK_SIZE", "BLOCKSIZE",
    // Home/identity
    "HOME", "USER", "LOGNAME", "PWD", "SHELL",
    // Temp directories
    "TMPDIR", "TEMP", "TMP",
    // Display for GUI apps
    "DISPLAY", "WAYLAND_DISPLAY",
    // Proxy
    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY",
    "http_proxy", "https_proxy", "no_proxy",
    // CI
    "CI", "GITHUB_ACTIONS",
    // Windows
    "SYSTEMROOT", "PROGRAMFILES", "PROGRAMFILES(X86)",
    "APPDATA", "LOCALAPPDATA", "HOMEDRIVE", "HOMEPATH",
];

/// Detect command substitution patterns: $(), backticks, <(), >(), $((, dangerous ${VAR}.
fn detect_command_substitution(cmd: &str) -> Option<String> {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (i, c) in cmd.char_indices() {
        if escaped { escaped = false; continue; }
        match c {
            '\\' if !in_single_quote => { escaped = true; }
            '\'' if !in_double_quote => { in_single_quote = !in_single_quote; }
            '"' if !in_single_quote => { in_double_quote = !in_double_quote; }
            '$' if !in_single_quote && !in_double_quote => {
                if i + 1 < cmd.len() {
                    let next = cmd.chars().nth(i + 1);
                    match next {
                        Some('(') => {
                            let rest: String = cmd[i + 2..].chars().take(20).collect();
                            if rest.starts_with("(") || rest.starts_with("((") {
                                return Some("$() command substitution".to_string());
                            }
                            if rest.starts_with("<(") || rest.starts_with(">(") {
                                return Some("process substitution".to_string());
                            }
                        }
                        Some('{') => {
                            let end = cmd[i + 2..].find('}');
                            if let Some(end_idx) = end {
                                let var_content = &cmd[i + 2..i + 2 + end_idx];
                                let safe_patterns = [":-", ":=", ":?"];
                                let is_safe_pattern = safe_patterns.iter().any(|p| var_content.contains(p));
                                if is_safe_pattern { continue; }
                                let var_name = var_content.split(|c| c == ':' || c == '-' || c == '=' || c == '?').next().unwrap_or(var_content);
                                if !is_safe_variable(var_name) {
                                    return Some(format!("${{{}}} variable substitution", var_name));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            '`' if !in_single_quote && !in_double_quote => {
                return Some("backtick command substitution".to_string());
            }
            _ => {}
        }
    }
    if cmd.contains('`') { return Some("backtick command substitution".to_string()); }
    None
}

/// Check if an environment variable name is safe to expand.
fn is_safe_variable(name: &str) -> bool {
    if SAFE_ENV_VARS.iter().any(|&s| s == name) { return true; }
    if name.chars().all(|c| c.is_ascii_digit()) { return true; }
    let special_vars = ["?", "!", "0", "#", "@", "*", "-", "_", "$"];
    if special_vars.contains(&name) { return true; }
    false
}

/// Strip safe wrapper commands from the command string.
fn strip_safe_wrappers(cmd: &str) -> String {
    let trimmed = cmd.trim();
    static WRAPPERS: OnceLock<Vec<&'static str>> = OnceLock::new();
    let wrappers = WRAPPERS.get_or_init(|| {
        vec!["timeout", "nice", "nohup", "env", "stdbuf", "ionice",
             "unbuffer", "command", "builtin", "time", "sudo", "doas"]
    });
    let mut result = trimmed.to_string();
    let mut changed = true;
    while changed {
        changed = false;
        let lower = result.to_lowercase();
        for wrapper in wrappers.iter() {
            if lower.starts_with(*wrapper) {
                let rest = result[wrapper.len()..].trim_start();
                if *wrapper == "timeout" {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if !parts.is_empty() && parts[0].chars().all(|c| c.is_ascii_digit()) {
                        result = parts[1..].join(" ");
                    } else { result = rest.to_string(); }
                    changed = true; break;
                }
                if *wrapper == "nice" {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.first() == Some(&"-n") && parts.len() > 2 {
                        result = parts[2..].join(" ");
                    } else if parts.first().map(|s| s.starts_with('-')).unwrap_or(false) && !parts.is_empty() && parts[0].len() > 1 {
                        result = parts[1..].join(" ");
                    } else { result = rest.to_string(); }
                    changed = true; break;
                }
                if *wrapper == "env" {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    let mut start_idx = 0;
                    for (idx, part) in parts.iter().enumerate() {
                        if part.contains('=') { start_idx = idx + 1; } else { break; }
                    }
                    if start_idx < parts.len() { result = parts[start_idx..].join(" "); }
                    else { result = parts.join(" "); }
                    changed = true; break;
                }
                // sudo/doas: strip all flags and the command name, exposing what comes after
                if (*wrapper == "sudo" || *wrapper == "doas") && !rest.is_empty() {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    let mut start_idx = 0;
                    for (idx, part) in parts.iter().enumerate() {
                        if part.starts_with('-') { start_idx = idx + 1; } else { break; }
                    }
                    // Also skip the next positional arg (the command being wrapped)
                    if start_idx < parts.len() { result = parts[start_idx + 1..].join(" "); }
                    changed = true; break;
                }
                if !rest.is_empty() {
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    let mut start_idx = 0;
                    for (idx, part) in parts.iter().enumerate() {
                        if part.starts_with('-') { start_idx = idx + 1; } else { break; }
                    }
                    if start_idx < parts.len() {
                        result = parts[start_idx..].join(" ");
                        changed = true; break;
                    }
                }
            }
        }
    }
    result.trim().to_string()
}

/// Split compound commands on shell operators while respecting quoted strings.
fn split_compound_command(cmd: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let chars: Vec<(usize, char)> = cmd.char_indices().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let (_idx, c) = chars[i];
        if escaped { current.push(c); escaped = false; i += 1; continue; }
        match c {
            '\\' if !in_single_quote => { escaped = true; current.push(c); }
            '\'' if !in_double_quote => { in_single_quote = !in_single_quote; current.push(c); }
            '"' if !in_single_quote => { in_double_quote = !in_double_quote; current.push(c); }
            '&' if !in_single_quote && !in_double_quote => {
                if i + 1 < len && chars[i + 1].1 == '&' {
                    let t = current.trim();
                    if !t.is_empty() { result.push(t.to_string()); }
                    current.clear(); i += 2; continue;
                }
                current.push(c);
            }
            '|' if !in_single_quote && !in_double_quote => {
                if i + 1 < len && chars[i + 1].1 == '|' {
                    let t = current.trim();
                    if !t.is_empty() { result.push(t.to_string()); }
                    current.clear(); i += 2; continue;
                }
                let t = current.trim();
                if !t.is_empty() { result.push(t.to_string()); }
                current.clear();
            }
            ';' if !in_single_quote && !in_double_quote => {
                let t = current.trim();
                if !t.is_empty() { result.push(t.to_string()); }
                current.clear();
            }
            '\n' if !in_single_quote && !in_double_quote => {
                let t = current.trim();
                if !t.is_empty() { result.push(t.to_string()); }
                current.clear();
            }
            _ => { current.push(c); }
        }
        i += 1;
    }
    let t = current.trim();
    if !t.is_empty() { result.push(t.to_string()); }
    if result.is_empty() { result.push(cmd.trim().to_string()); }
    result
}

/// Extract the base command from a potentially modified command string.
fn extract_base_command(cmd: &str) -> String {
    let stripped = strip_safe_wrappers(cmd);
    let parts: Vec<&str> = stripped.split_whitespace().collect();
    if parts.is_empty() { return String::new(); }
    let mut start_idx = 0;
    for (idx, part) in parts.iter().enumerate() {
        if part.contains('=') { start_idx = idx + 1; } else { break; }
    }
    let remaining: Vec<&str> = if start_idx < parts.len() { parts[start_idx..].to_vec() } else { parts.clone() };
    if remaining.is_empty() { return String::new(); }
    let binary = remaining[0];
    if binary.contains('/') || binary.contains('\\') {
        if let Some(last_slash) = binary.rfind(|c| c == '/' || c == '\\') {
            let base = &binary[last_slash + 1..];
            if remaining.len() > 1 { let next = remaining[1]; if !next.starts_with('-') { return format!("{} {}", base, next); } }
            return base.to_string();
        }
    }
    if remaining.len() > 1 { let next = remaining[1]; if !next.starts_with('-') { return format!("{} {}", binary, next); } }
    binary.to_string()
}

/// Check if a command is read-only (does not modify any state).
#[allow(dead_code)]
fn is_read_only_command(cmd: &str) -> bool {
    let base = extract_base_command(cmd).to_lowercase();
    let read_only = ["ls","cat","head","tail","wc","find","grep","rg","echo","pwd","which",
        "env","date","type","file","stat","du","df","free","uname","hostname","id","whoami",
        "uptime","history","tree","xxd","hexdump","od","sort","uniq","diff","comm","jq",
        "git status","git log","git diff","git branch","git show","git tag","git remote -v",
        "gh pr view","gh issue view","gh pr list","gh issue list",
        "go version","go env","cargo --version","npm --version","node --version",
        "python --version","pip list","pip show"];
    if read_only.contains(&base.as_str()) { return true; }
    if base.starts_with("git branch") { return !cmd.contains("-d") && !cmd.contains("-D") && !cmd.contains("-m") && !cmd.contains("-M"); }
    if base.starts_with("git stash") { return !cmd.contains("drop") && !cmd.contains("clear") && !cmd.contains("pop"); }
    if base.starts_with("git clean") { return !cmd.contains("-f") && !cmd.contains("--force"); }
    if base.starts_with("git reset") { return !cmd.contains("--hard") && !cmd.contains("--merge"); }
    false
}

/// Destructive command pattern with description.
struct DestructivePattern { pattern: &'static str, description: &'static str }

const DESTRUCTIVE_PATTERNS: &[DestructivePattern] = &[
    DestructivePattern { pattern: r"^\s*rm\s", description: "rm - removes files" },
    DestructivePattern { pattern: r"^\s*rmdir\s", description: "rmdir - removes directories" },
    DestructivePattern { pattern: r"^\s*unlink\s", description: "unlink - removes file link" },
    DestructivePattern { pattern: r"^\s*del\s", description: "del - Windows file deletion" },
    DestructivePattern { pattern: r"^\s*erase\s", description: "erase - removes files" },
    DestructivePattern { pattern: r"^\s*shred\s", description: "shred - securely deletes files" },
    DestructivePattern { pattern: r"^\s*wipefs\s", description: "wipefs - erases filesystem signatures" },
    DestructivePattern { pattern: r"^\s*dd\s+.*\bof=", description: "dd with output file - disk operation" },
    DestructivePattern { pattern: r"^\s*mkfs", description: "mkfs - creates filesystem" },
    DestructivePattern { pattern: r"^\s*format\s", description: "format - formats disk" },
    DestructivePattern { pattern: r"^\s*fdisk\s", description: "fdisk - disk partition tool" },
    DestructivePattern { pattern: r"^\s*parted\s", description: "parted - partition editor" },
    DestructivePattern { pattern: r"git\s+push\s+.*-f", description: "git push --force" },
    DestructivePattern { pattern: r"git\s+push\s+.*--force", description: "git push --force" },
    DestructivePattern { pattern: r"git\s+push\s+.*-F", description: "git push with force flag" },
    DestructivePattern { pattern: r"git\s+reset\s+--hard", description: "git reset --hard" },
    DestructivePattern { pattern: r"git\s+reset\s+--merge", description: "git reset --merge" },
    DestructivePattern { pattern: r"git\s+clean\s+.*-f", description: "git clean -f (force clean)" },
    DestructivePattern { pattern: r"git\s+clean\s+.*-fd", description: "git clean -fd" },
    DestructivePattern { pattern: r"git\s+checkout\s+\.", description: "git checkout . (discard changes)" },
    DestructivePattern { pattern: r"git\s+checkout\s+--\s*\.", description: "git checkout . (discard changes)" },
    DestructivePattern { pattern: r"git\s+stash\s+drop", description: "git stash drop (delete stash)" },
    DestructivePattern { pattern: r"git\s+stash\s+clear", description: "git stash clear (delete all stashes)" },
    DestructivePattern { pattern: r"git\s+branch\s+-[dD]\s", description: "git branch -d/-D (delete branch)" },
    DestructivePattern { pattern: r"git\s+worktree\s+remove", description: "git worktree remove" },
    DestructivePattern { pattern: r"git\s+filter-branch", description: "git filter-branch (history rewrite)" },
    DestructivePattern { pattern: r"git\s+filter-repo", description: "git filter-repo (history rewrite)" },
    DestructivePattern { pattern: r"kubectl\s+delete", description: "kubectl delete (removes resources)" },
    DestructivePattern { pattern: r"kubectl\s+apply\s+.*--force", description: "kubectl apply --force" },
    DestructivePattern { pattern: r"kubectl\s+rollout\s+undo", description: "kubectl rollout undo" },
    DestructivePattern { pattern: r"docker\s+rm\s", description: "docker rm (remove containers)" },
    DestructivePattern { pattern: r"docker\s+rmi\s", description: "docker rmi (remove images)" },
    DestructivePattern { pattern: r"docker\s+container\s+rm", description: "docker container rm" },
    DestructivePattern { pattern: r"docker\s+image\s+rm", description: "docker image rm" },
    DestructivePattern { pattern: r"docker\s+volume\s+rm", description: "docker volume rm" },
    DestructivePattern { pattern: r"docker\s+network\s+rm", description: "docker network rm" },
    DestructivePattern { pattern: r"docker\s+prune\s+.*-f", description: "docker prune (cleanup)" },
    DestructivePattern { pattern: r"docker\s+system\s+prune", description: "docker system prune" },
    DestructivePattern { pattern: r"podman\s+rm\s", description: "podman rm" },
    DestructivePattern { pattern: r"podman\s+rmi\s", description: "podman rmi" },
    DestructivePattern { pattern: r"npm\s+uninstall\s", description: "npm uninstall (removes packages)" },
    DestructivePattern { pattern: r"npm\s+rm\s", description: "npm rm (remove packages)" },
    DestructivePattern { pattern: r"yarn\s+remove\s", description: "yarn remove" },
    DestructivePattern { pattern: r"pnpm\s+remove\s", description: "pnpm remove" },
    DestructivePattern { pattern: r"pip\s+uninstall\s", description: "pip uninstall (removes packages)" },
    DestructivePattern { pattern: r"pip3\s+uninstall\s", description: "pip uninstall" },
    DestructivePattern { pattern: r"gem\s+uninstall\s", description: "gem uninstall" },
    DestructivePattern { pattern: r"cargo\s+clean\s", description: "cargo clean (removes build artifacts)" },
    DestructivePattern { pattern: r"cargo\s+remove\s", description: "cargo remove (removes dependencies)" },
    DestructivePattern { pattern: r"terraform\s+destroy\s", description: "terraform destroy (destroys infrastructure)" },
    DestructivePattern { pattern: r"terraform\s+apply\s+.*-destroy", description: "terraform apply -destroy" },
    DestructivePattern { pattern: r"terraform\s+state\s+rm", description: "terraform state rm (removes from state)" },
    DestructivePattern { pattern: r"vagrant\s+destroy", description: "vagrant destroy (removes VMs)" },
    DestructivePattern { pattern: r"vagrant\s+halt", description: "vagrant halt (stops VMs)" },
    DestructivePattern { pattern: r"DROP\s+TABLE", description: "DROP TABLE (database)" },
    DestructivePattern { pattern: r"DROP\s+DATABASE", description: "DROP DATABASE (database)" },
    DestructivePattern { pattern: r"TRUNCATE\s+TABLE", description: "TRUNCATE TABLE (database)" },
    DestructivePattern { pattern: r"DELETE\s+FROM", description: "DELETE FROM (database operation)" },
    DestructivePattern { pattern: r"shutdown\s", description: "shutdown (system shutdown)" },
    DestructivePattern { pattern: r"reboot\s", description: "reboot (system restart)" },
    DestructivePattern { pattern: r"poweroff\s", description: "poweroff (system power off)" },
    DestructivePattern { pattern: r"init\s+0", description: "init 0 (halt system)" },
    DestructivePattern { pattern: r"init\s+6", description: "init 6 (reboot system)" },
    DestructivePattern { pattern: r"systemctl\s+poweroff", description: "systemctl poweroff" },
    DestructivePattern { pattern: r"systemctl\s+reboot", description: "systemctl reboot" },
    DestructivePattern { pattern: r"systemctl\s+kill\s", description: "systemctl kill" },
    DestructivePattern { pattern: r"service\s+.*\s+stop", description: "service stop" },
    DestructivePattern { pattern: r"killall\s", description: "killall (kill processes)" },
    DestructivePattern { pattern: r"pkill\s", description: "pkill (kill by name)" },
    DestructivePattern { pattern: r"kill\s+-9\s", description: "kill -9 (force kill)" },
    DestructivePattern { pattern: r">\s*/dev/", description: "redirect to device (dangerous)" },
    DestructivePattern { pattern: r">\s*/proc/", description: "redirect to proc (dangerous)" },
    DestructivePattern { pattern: r">\s*/sys/", description: "redirect to sys (dangerous)" },
    DestructivePattern { pattern: r"rm\s+-[rf]{1,2}\s", description: "rm -rf (recursive force delete)" },
    DestructivePattern { pattern: r"rm\s+--recursive\s", description: "rm --recursive" },
    DestructivePattern { pattern: r"del\s+/[fqs]\s", description: "del with force flag (Windows)" },
    DestructivePattern { pattern: r"del\s+/s", description: "del /s (recursive delete Windows)" },
    DestructivePattern { pattern: r"rmrf\s", description: "rmrf (recursive delete)" },
    DestructivePattern { pattern: r"rm\s+-rf\s+/", description: "rm -rf / (delete all)" },
    DestructivePattern { pattern: r"chmod\s+777", description: "chmod 777 (world writable)" },
    DestructivePattern { pattern: r"chmod\s+-R\s+777", description: "chmod -R 777 (recursive world writable)" },
    DestructivePattern { pattern: r"chown\s+.*\s+-R\s+", description: "chown recursive" },
    // PowerShell destructive cmdlets
    DestructivePattern { pattern: r"remove-item\s", description: "Remove-Item (PowerShell delete)" },
    DestructivePattern { pattern: r"\bri\s+", description: "ri alias (PowerShell Remove-Item)" },
    DestructivePattern { pattern: r"remove-itemproperty\s", description: "Remove-ItemProperty (PowerShell)" },
    // Docker destructive operations
    DestructivePattern { pattern: r"docker\s+system\s+prune", description: "docker system prune (remove all unused data)" },
    DestructivePattern { pattern: r"docker\s+(container|image|volume|network)\s+prune", description: "docker prune (remove resources)" },
    DestructivePattern { pattern: r"docker\s+rm\s", description: "docker rm (remove container)" },
    DestructivePattern { pattern: r"docker\s+rmi\s", description: "docker rmi (remove image)" },
    DestructivePattern { pattern: r"docker\s+volume\s+rm\s", description: "docker volume rm" },
    // Git destructive operations via exec (bypasses git_tool security)
    DestructivePattern { pattern: r"git\s+push\s+.*--force", description: "git push --force (force push)" },
    DestructivePattern { pattern: r"git\s+push\s+-f\b", description: "git push -f (force push)" },
    DestructivePattern { pattern: r"git\s+clean\s+-[fd]", description: "git clean -fd (remove untracked files)" },
    DestructivePattern { pattern: r"git\s+reset\s+--hard", description: "git reset --hard (discard changes)" },
    DestructivePattern { pattern: r"git\s+checkout\s+--force", description: "git checkout --force" },
    DestructivePattern { pattern: r"git\s+rebase\s+--interactive", description: "git rebase --interactive (rewrites history)" },
    DestructivePattern { pattern: r"git\s+filter-branch", description: "git filter-branch (rewrites history)" },
    DestructivePattern { pattern: r"git\s+reflog\s+expire", description: "git reflog expire (lose recovery)" },
];

/// Check if a command is destructive. Returns (is_destructive, reason).
fn is_destructive_command(cmd: &str) -> (bool, String) {
    let stripped = strip_safe_wrappers(cmd);
    let parts = split_compound_command(&stripped);
    // Check ALL subcommands, not just the first one — a safe first command
    // followed by a destructive one (e.g. "git status && rm -rf /") must be caught.
    for part in &parts {
        let part_lower = part.to_lowercase();
        for dp in DESTRUCTIVE_PATTERNS {
            if let Ok(re) = Regex::new(dp.pattern) {
                if re.is_match(&part_lower) { return (true, dp.description.to_string()); }
            }
        }
    }
    (false, String::new())
}

/// Extract the targets (non-flag arguments) from a deletion command.
fn extract_deletion_targets(cmd: &str) -> Vec<String> {
    let stripped = strip_safe_wrappers(cmd);
    let parts: Vec<&str> = stripped.split_whitespace().collect();
    if parts.is_empty() { return Vec::new(); }
    let mut start_idx = 1;
    for part in parts.iter().skip(1) {
        if part.starts_with('-') {
            let flag_content = if part.starts_with("--") { &part[2..] } else { &part[1..] };
            if flag_content.is_empty() { start_idx += 1; continue; }
            let single_flags = ["-r", "-R", "-f", "--recursive", "--force", "--no-preserve-root", "-d", "-v", "--verbose", "-i", "--interactive", "-I", "--one-file-system"];
            if single_flags.contains(part) || flag_content.chars().all(|c| "rfRfvdviI".contains(c)) {
                start_idx += 1;
            } else { start_idx += 1; }
        } else if *part == "--" { start_idx += 1; break; } else { break; }
    }
    let mut targets = Vec::new();
    for part in parts.iter().skip(start_idx) {
        if !part.starts_with('-') && *part != "--" { targets.push(part.to_string()); }
    }
    targets
}

/// Paths that are considered dangerous to delete.
const DANGEROUS_PATHS: &[&str] = &[
    "/", "/.", "~", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64",
    "/var", "/home", "/root", "/opt", "/boot", "/sys", "/proc", "/dev",
    "/run", "/tmp", "/var/tmp", "/var/log", "/var/cache", "/srv", "/mnt",
    "/media", "/lost+found", "/snap",
    "C:\\", "C:/", "D:\\", "D:/", "C:\\Windows", "C:\\Program Files",
    "C:\\Program Files (x86)", "C:\\ProgramData", "C:\\Users",
    "C:\\System Volume Information",
    ".git", ".gitconfig", ".gitignore", ".gitmodules", ".github",
    ".claude", ".clauderc", "go.mod", "go.sum", "Cargo.toml", "Cargo.lock",
    "package.json", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "Makefile", "CMakeLists.txt", "build.gradle", "pom.xml",
    "requirements.txt", "Pipfile", "pyproject.toml", "setup.py", "setup.cfg",
    "rust-toolchain.toml", ".vscode", ".idea",
];

/// Check if a path is considered dangerous for deletion.
fn is_dangerous_deletion_path(path: &str) -> Option<String> {
    let normalized = path.trim();
    if normalized.is_empty() { return None; }
    for dangerous in DANGEROUS_PATHS {
        if normalized.eq_ignore_ascii_case(dangerous) {
            return Some(format!("Dangerous path '{}' would be deleted", path));
        }
        let with_slash = format!("{}/", dangerous);
        if normalized.eq_ignore_ascii_case(&with_slash) {
            return Some(format!("Dangerous path '{}' would be deleted", path));
        }
    }
    // Block bare wildcard deletion
    if normalized == "*" || normalized == "*/*" {
        return Some("Bare wildcard deletion is blocked".into());
    }
    if normalized.contains('*') || normalized.contains('?') {
        if normalized.starts_with('/') || normalized.starts_with('*') {
            return Some(format!("Glob pattern '{}' could match dangerous paths", path));
        }
        if normalized.starts_with('~') || normalized.starts_with("$HOME") {
            return Some(format!("Pattern '{}' matches home directory", path));
        }
    }
    // Block tilde variants: ~user, ~+, ~-, ~N (bash tilde expansion)
    if normalized.starts_with('~') && normalized != "~" && !normalized.starts_with("~/") {
        return Some(format!("Tilde expansion '{}' could resolve to another home directory", path));
    }
    if contains_path_escape(path) {
        return Some(format!("Path '{}' contains traversal that could escape", path));
    }
    // Block ANY Windows drive root child (C:\X, D:\foo, etc.)
    if normalized.len() >= 2 {
        let first_char = normalized.chars().next().unwrap();
        if normalized.chars().nth(1) == Some(':') && first_char.is_alphabetic() {
            let rest = &normalized[2..];
            if rest.is_empty() || rest == "\\" || rest == "/" {
                return Some(format!("Dangerous Windows path '{}' would be deleted", path));
            }
            // Block any direct child of a drive root (C:\X, D:\Y)
            if rest.starts_with('\\') || rest.starts_with('/') {
                let child = &rest[1..];
                if !child.is_empty() && !child.contains('\\') && !child.contains('/') {
                    return Some(format!("Dangerous Windows drive root child '{}' would be deleted", path));
                }
            }
        }
    }
    let lower = normalized.to_lowercase();
    if lower.contains(".git/objects") || lower.contains(".git/refs") || lower.contains("/.git/") {
        return Some(format!("Git internal directory '{}' would be deleted", path));
    }
    None
}

/// Validate output redirection targets (> file, >> file) against dangerous paths.
/// Prevents writing to sensitive system files via redirection.
fn validate_redirect_targets(cmd: &str) -> Option<String> {
    let targets = extract_redirect_targets(cmd);
    for target in &targets {
        if let Some(reason) = validate_redirect_path(target) {
            return Some(reason);
        }
    }
    None
}

/// Extract `>` and `>>` redirect targets from a command, respecting quoted strings.
fn extract_redirect_targets(cmd: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;

    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if escaped { escaped = false; i += 1; continue; }
        if c == b'\\' && !in_single_quote { escaped = true; i += 1; continue; }
        if c == b'\'' && !in_double_quote { in_single_quote = !in_single_quote; i += 1; continue; }
        if c == b'"' && !in_single_quote { in_double_quote = !in_double_quote; i += 1; continue; }
        if in_single_quote || in_double_quote { i += 1; continue; }
        // Look for > outside quotes
        if c == b'>' {
            // Skip process substitution >(
            if i + 1 < bytes.len() && bytes[i + 1] == b'(' { i += 1; continue; }
            // Find start of target (skip optional second > and whitespace)
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'>' { j += 1; }
            // Skip whitespace
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') { j += 1; }
            // Extract target token (until whitespace or shell operator)
            let start = j;
            while j < bytes.len() {
                match bytes[j] {
                    b' ' | b'\t' | b';' | b'&' | b'|' | b'>' | b'<' => break,
                    _ => j += 1,
                }
            }
            if j > start {
                targets.push(cmd[start..j].to_string());
            }
            i += 1;
            continue;
        }
        i += 1;
    }

    targets
}

/// Check if a redirect target path is dangerous.
fn validate_redirect_path(target: &str) -> Option<String> {
    // Block shell expansion: $VAR, ${VAR}, %VAR%
    if target.contains('$') || target.contains('%') {
        return Some("Output redirection to shell-expanded paths is blocked".into());
    }
    // Block process substitution and backticks
    if target.contains('(') || target.contains('`') {
        return Some("Output redirection to process substitutions is blocked".into());
    }
    if target.starts_with('=') {
        return Some("Output redirection to =cmd is blocked".into());
    }

    // Strip surrounding quotes
    let trimmed = if (target.starts_with('\'') && target.ends_with('\''))
        || (target.starts_with('"') && target.ends_with('"'))
    {
        &target[1..target.len() - 1]
    } else {
        target
    };
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.replace('\\', "/");

    // /dev/* allowed only for /dev/null
    if normalized.starts_with("/dev/") && normalized != "/dev/null" {
        return Some("Output redirection to /dev/* is blocked (except /dev/null)".into());
    }

    // Block system directories
    if normalized.starts_with("/proc/") || normalized.starts_with("/sys/") {
        return Some("Output redirection to /proc/ or /sys/ is blocked".into());
    }
    if normalized.starts_with("/etc/") || normalized == "/etc" {
        return Some("Output redirection to /etc/ is blocked".into());
    }
    if normalized.starts_with("/usr/") || normalized == "/usr" {
        return Some("Output redirection to /usr/ is blocked".into());
    }
    if normalized.starts_with("/bin/") || normalized == "/bin"
        || normalized.starts_with("/sbin/") || normalized == "/sbin"
    {
        return Some("Output redirection to system bin directories is blocked".into());
    }
    if normalized.starts_with("/boot/") || normalized == "/boot" {
        return Some("Output redirection to /boot/ is blocked".into());
    }
    if normalized.starts_with("/var/") || normalized == "/var" {
        return Some("Output redirection to /var/ is blocked".into());
    }

    // Block ~/.ssh/*
    if normalized.starts_with("~/.ssh") || normalized.starts_with("$HOME/.ssh") {
        return Some("Output redirection to ~/.ssh/ is blocked".into());
    }

    // Block project config files
    let patterns = [".claude/", ".env", ".env.local", "settings.json"];
    for pattern in &patterns {
        if normalized.starts_with(pattern) || normalized.ends_with(pattern) || normalized == *pattern {
            return Some("Output redirection to project config files is blocked".into());
        }
    }

    None
}

/// Validate that deletion paths in a command are safe.
fn validate_deletion_paths(cmd: &str) -> Option<String> {
    let base = extract_base_command(cmd).to_lowercase();
    let deletion_commands = ["rm", "rmdir", "unlink", "del", "erase", "remove-item", "ri", "rd"];
    if !deletion_commands.iter().any(|&d| base.starts_with(d)) { return None; }
    let targets = extract_deletion_targets(cmd);
    for target in targets {
        if let Some(err) = is_dangerous_deletion_path(&target) { return Some(err); }
    }
    None
}

/// Check for UNC paths that could leak NTLM credentials via SMB/WebDAV.
/// Matches upstream's containsVulnerableUncPath logic.
fn contains_vulnerable_unc_path(cmd: &str) -> bool {
    // Pattern 1: UNC paths with backslashes (\\server\share)
    // Also catches WebDAV: \\server@SSL@8443\, \\server@8443@SSL\
    static UNC_BACKSLASH: OnceLock<Regex> = OnceLock::new();
    let re1 = UNC_BACKSLASH.get_or_init(|| {
        Regex::new(r#"\\\\[^\s\\/]+(?:@(?:\d+|ssl))?(?:[\\/]|$|\s)"#).unwrap()
    });
    if re1.is_match(cmd) {
        return true;
    }

    // Pattern 2: Forward-slash UNC paths (//server/share)
    // Rust regex doesn't support lookbehind, so match and verify the preceding char
    static UNC_FORSLASH: OnceLock<Regex> = OnceLock::new();
    let re2 = UNC_FORSLASH.get_or_init(|| {
        Regex::new(r"//[^\s\\/]+(?:@(?:\d+|ssl))?(?:[\\/]|$|\s)").unwrap()
    });
    if let Some(loc) = re2.find(cmd) {
        // Ensure not preceded by ':' (which would indicate https:// or similar)
        let start = loc.start();
        if start == 0 || cmd.as_bytes()[start - 1] != b':' {
            return true;
        }
    }

    // Pattern 3: DavWWWRoot marker (Windows WebDAV redirector)
    if cmd.contains("DavWWWRoot") {
        return true;
    }

    // Pattern 4: IPv4 literal UNC paths (\\192.168.1.1\share)
    static UNC_IPV4: OnceLock<Regex> = OnceLock::new();
    let re4 = UNC_IPV4.get_or_init(|| {
        Regex::new(r#"^\\\\\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}[\\/]"#).unwrap()
    });
    if re4.is_match(cmd) {
        return true;
    }

    false
}

/// Extract quoted regions (single, double, backtick) from a command.
fn extract_quoted_regions(cmd: &str) -> Vec<(usize, usize)> {
    let mut regions = Vec::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let mut quote_start: Option<usize> = None;
    for (i, c) in cmd.char_indices() {
        if escaped { escaped = false; continue; }
        match c {
            '\\' if !in_single_quote => { escaped = true; }
            '\'' if !in_double_quote => {
                if in_single_quote { if let Some(start) = quote_start.take() { regions.push((start, i)); } in_single_quote = false; }
                else { quote_start = Some(i); in_single_quote = true; }
            }
            '"' if !in_single_quote => {
                if in_double_quote { if let Some(start) = quote_start.take() { regions.push((start, i)); } in_double_quote = false; }
                else { quote_start = Some(i); in_double_quote = true; }
            }
            '`' => {
                if in_single_quote || in_double_quote { continue; }
                if let Some(start) = quote_start { regions.push((start, i)); quote_start = None; }
                else { quote_start = Some(i); }
            }
            _ => {}
        }
    }
    regions
}

/// Check if a byte position is within a quoted region.
fn is_in_quoted_region(pos: usize, regions: &[(usize, usize)]) -> bool {
    for &(start, end) in regions { if pos >= start && pos <= end { return true; } }
    false
}

/// Detect glob patterns, bracket patterns, and brace expansion in destructive commands.
fn detect_expansion(cmd: &str) -> Option<String> {
    let base = extract_base_command(cmd);
    let base_lower = base.to_lowercase();
    let destructive_prefixes = ["rm", "del", "rmdir", "unlink", "erase", "mv", "cp", "chmod", "chown"];
    if !destructive_prefixes.iter().any(|&p| base_lower.starts_with(p)) { return None; }
    let quoted_regions = extract_quoted_regions(cmd);
    for (i, c) in cmd.char_indices() {
        if c == '*' && !is_in_quoted_region(i, &quoted_regions) {
            return Some("Glob pattern '*' detected with destructive command (could match multiple files)".to_string());
        }
        if c == '?' && !is_in_quoted_region(i, &quoted_regions) {
            return Some("Glob pattern '?' detected with destructive command".to_string());
        }
    }
    for (i, c) in cmd.char_indices() {
        if c == '[' && !is_in_quoted_region(i, &quoted_regions) {
            let rest = &cmd[i..];
            if let Some(end) = rest[1..].find(']') {
                let inner = &rest[1..end + 1];
                if inner.contains('-') || inner.chars().any(|c| c.is_ascii_alphanumeric()) {
                    return Some(format!("Bracket pattern detected with destructive command: {}", inner));
                }
            }
        }
    }
    for (i, c) in cmd.char_indices() {
        if c == '{' && !is_in_quoted_region(i, &quoted_regions) {
            let rest = &cmd[i..];
            if rest.contains("..") || rest.contains(',') {
                return Some("Brace expansion detected with destructive command".to_string());
            }
        }
    }
    None
}

/// Check if an IO error is due to process exit with non-zero status.
#[allow(dead_code)]
fn is_exit_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::Other
}

/// Read at most `limit` bytes from a reader, preventing unbounded memory growth.
/// This prevents OOM when a command produces very large output.
/// Matches Go's readLimited() behavior.
fn read_limited<R: std::io::Read>(r: &mut R, limit: usize) -> Vec<u8> {
    let mut buf = vec![0u8; limit];
    let mut off = 0;
    loop {
        if off >= limit {
            break;
        }
        match r.read(&mut buf[off..]) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    buf.truncate(off);
    buf
}

/// Check if a path contains directory traversal (../).
fn contains_path_escape(path: &str) -> bool {
    path.contains("..")
}


/// Background task callback: (command, working_dir) -> (task_id, output_file, error_text)
pub type BashBgTaskCallback =
    Arc<dyn Fn(String, String) -> (String, String, String) + Send + Sync>;

// ─── ExecTool ────────────────────────────────────────────────────────────────

/// ExecTool executes shell commands with security guards and background support.
pub struct ExecTool {
    /// When set, enables run_in_background support. The callback spawns a background
    /// bash task and returns (task_id, output_file, error_text).
    pub background_callback: Option<BashBgTaskCallback>,
}

impl ExecTool {
    pub fn new() -> Self {
        Self {
            background_callback: None,
        }
    }

    /// Create with a background task callback.
    pub fn with_background_callback(callback: BashBgTaskCallback) -> Self {
        Self {
            background_callback: Some(callback),
        }
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ExecTool {
    fn clone(&self) -> Self {
        Self {
            background_callback: self.background_callback.clone(),
        }
    }
}

impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        "Execute a shell command. On Windows, use PowerShell syntax (`;` to separate commands, not `&&`). Use `curl.exe` instead of `curl` on Windows (curl is alias to Invoke-WebRequest). Use for running scripts, installing packages, git operations, and any shell task. Commands run in the current working directory. Supports running commands in the background with run_in_background=true. ALWAYS prefer dedicated tools for file operations. NEVER use exec for file reading when file_read exists. NEVER use exec for file editing when file_edit exists."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "description": {
                    "type": "string",
                    "description": "A short description of what this command does. Useful for understanding intent when reviewing risky commands."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the command (default: current directory)."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (max 600000 / 10 minutes). Default: 120000 (2 minutes)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Set to true to run this command in the background. Returns immediately with a task ID. Use task_output to check results later."
                }
            },
            "required": ["command"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> Option<ToolResult> {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => cmd.trim(),
            None => return None,
        };

        // Split compound commands
        let subcommands = split_compound_command(command);

        // Track if any subcommand contains cd (directory change)
        let has_cd = subcommands.iter().any(|sub| {
            let stripped = sub.trim();
            let first = stripped.split_whitespace().next().unwrap_or("").to_lowercase();
            // Extract base (strip any wrapper like /bin/cd)
            let base = std::path::Path::new(&first)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&first)
                .to_lowercase();
            base == "cd" || base == "pushd" || base == "popd"
        });

        // If cd is present in compound command, validate redirects more strictly
        // (path resolution would be relative to the changed directory)
        if has_cd {
            if let Some(reason) = validate_redirect_targets(command) {
                return Some(ToolResult::error(reason));
            }
        }

        for subcmd in &subcommands {
            // Strip safe wrappers
            let stripped = strip_safe_wrappers(subcmd);

            // Check for command substitution
            if let Some(reason) = detect_command_substitution(&stripped) {
                return Some(ToolResult::error(format!("Command substitution detected: {}", reason)));
            }

            // Check for glob/brace expansion in destructive commands
            if let Some(reason) = detect_expansion(&stripped) {
                return Some(ToolResult::error(reason));
            }

            // Check for destructive commands
            let (is_destructive, reason) = is_destructive_command(&stripped);
            if is_destructive {
                return Some(ToolResult::error(format!("Destructive command detected: {}", reason)));
            }

            // Validate deletion paths
            if let Some(reason) = validate_deletion_paths(&stripped) {
                return Some(ToolResult::error(reason));
            }

            // Validate output redirection targets
            if let Some(reason) = validate_redirect_targets(&stripped) {
                return Some(ToolResult::error(reason));
            }
        }

        // Apply existing regex patterns as additional safety net
        let lower = command.to_lowercase();

        // Check for dangerous patterns (cached regexes)
        static DANGEROUS: OnceLock<Vec<Regex>> = OnceLock::new();
        let dangerous = DANGEROUS.get_or_init(|| {
            [
                r"\brm\s+-[rf]{1,2}\b",
                r"\bdel\s+/[fq]\b",
                r"\brmdir\s+/s\b",
                r"format\b",
                r"\b(mkfs|diskpart)\b",
                r"\bdd\s+.*\bof=",
                r">\s*/dev/sd",
                r"\b(shutdown|reboot|poweroff)\b",
                r":\(\)\s*\{.*\};\s*:",
                r"&\S*&\S*&",
                // PowerShell destructive cmdlets
                r"\bremov(?:e|ed)-item\b",
                r"\bremov(?:e|ed)-itemproperty\b",
                r"\bdocker\s+system\s+prune\b",
                r"\bdocker\s+\S+\s+prune\b",
                // Git destructive via exec
                r"\bgit\s+push\s+.*--force\b",
                r"\bgit\s+push\s+-f\b",
                r"\bgit\s+clean\s+-[fd]",
                r"\bgit\s+reset\s+--hard\b",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });

        for re in dangerous {
            if re.is_match(&lower) {
                return Some(ToolResult::error(format!("Dangerous command pattern detected: {}", re.as_str())));
            }
        }

        // Check for .git directory destruction (cached regexes)
        static GIT_HARMFUL: OnceLock<Vec<Regex>> = OnceLock::new();
        let git_harmful = GIT_HARMFUL.get_or_init(|| {
            [
                r"rm\s+-rf.*\.git",
                r"rm\s+-r.*\.git",
                r"rmdir.*\.git",
                r"del.*\.git",
                r"rmrf.*\.git",
                r"remove-item.*\.git",
                r"\bri\s+.*\.git",
                r"rd\s+/s.*\.git",
                r"git\s+clean\s+-[fd].*\.git",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });
        for re in git_harmful {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Command would destroy .git directory"));
            }
        }

        // Check for home directory destruction (cached regexes)
        static HOME_HARMFUL: OnceLock<Vec<Regex>> = OnceLock::new();
        let home_harmful = HOME_HARMFUL.get_or_init(|| {
            [
                r"rm\s+-rf\s*~",
                r"rm\s+-rf\s+/home",
                r"rm\s+-rf\s+/",
                r"rm\s+-rf\s+C:\\Users",
                r"del\s+/[fq]\s+\w+\\.*",
                r"remove-item.*~",
                r"remove-item.*C:\\Users",
                r"remove-item.*/home",
                r"\bri\s+.*~",
                r"\bri\s+.*C:\\Users",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });
        for re in home_harmful {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Command would destroy home directory or system root"));
            }
        }

        // Check for internal URLs (cached regexes)
        static URL_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
        let url_patterns = URL_PATTERNS.get_or_init(|| {
            [
                r"https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|192\.168\.\d+\.\d+|10\.\d+\.\d+\.\d+|172\.(1[6-9]|2\d|3[01])\.\d+\.\d+)[:/]",
                r"https?://[0-9]+(?:\.[0-9]+){3}:\d+",
            ].iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
        });

        for re in url_patterns {
            if re.is_match(&lower) {
                return Some(ToolResult::error("Internal/private URL detected"));
            }
        }

        // Check for UNC paths (SMB/WebDAV) that could leak NTLM credentials on Windows
        if contains_vulnerable_unc_path(command) {
            return Some(ToolResult::error(
                "UNC path detected: commands targeting SMB/WebDAV shares are blocked",
            ));
        }

        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ExecutesCode, crate::tools::ToolCapability::Subprocess]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Classifier
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        // Check for background execution request
        let run_in_background = params
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if run_in_background {
            return self.exec_in_background(&params);
        }

        self.exec_foreground(params)
    }
}

// ─── Foreground execution ───────────────────────────────────────────────────

impl ExecTool {
    fn exec_foreground(&self, params: HashMap<String, Value>) -> ToolResult {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.trim(),
            None => return ToolResult::error("Error: empty command"),
        };

        if command.is_empty() {
            return ToolResult::error("Error: empty command");
        }

        let timeout_ms = params
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(120000)  // default: 2 minutes (matching official Claude Code)
            .clamp(1, 600000) as u64;

        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        // Determine shell: powershell -> bash -> cmd on Windows (matching Go)
        // Cached with OnceLock to avoid spawning a process every call
        static SHELL_CACHE: OnceLock<(&'static str, &'static str)> = OnceLock::new();
        let (shell, flag) = SHELL_CACHE.get_or_init(|| {
            if cfg!(target_os = "windows") {
                if std::process::Command::new("powershell").output().is_ok() {
                    ("powershell", "-Command")
                } else if std::process::Command::new("bash").output().is_ok() {
                    ("bash", "-c")
                } else {
                    ("cmd", "/C")
                }
            } else {
                ("bash", "-c")
            }
        });

        // Build command with platform-specific process group setup
        // On Unix, set process group so we can kill the entire tree on timeout (tree-kill).
        // On Windows, process groups are not used — child.kill() terminates the process.
        let mut cmd = Command::new(shell);
        cmd.arg(flag)
            .arg(command)
            .current_dir(&working_dir)
            .stdin(std::process::Stdio::null())  // Isolate from REPL stdin
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let output_result = cmd.spawn();

        let mut child = match output_result {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        // Read stdout/stderr concurrently to avoid pipe deadlock.
        // If the child produces >64KB of output and we don't read the pipes,
        // the OS pipe buffer fills, the child blocks on write, and we deadlock.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_thread = std::thread::spawn(move || {
            if let Some(mut pipe) = stdout_pipe {
                read_limited(&mut pipe, 50000)
            } else {
                Vec::new()
            }
        });
        let stderr_thread = std::thread::spawn(move || {
            if let Some(mut pipe) = stderr_pipe {
                read_limited(&mut pipe, 25000)
            } else {
                Vec::new()
            }
        });

        // Apply timeout: wait for process to exit, kill if it doesn't
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let start = std::time::Instant::now();
        let timed_out = loop {
            match child.try_wait() {
                Ok(Some(_)) => break false,
                Ok(None) => {
                    if std::time::Instant::now().duration_since(start) >= timeout {
                        // Kill the entire process group (matching upstream's tree-kill)
                        let pid = child.id();
                        #[cfg(unix)]
                        unsafe {
                            // Negative PID = process group. Cast to i32 for Unix kill.
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                        #[cfg(windows)]
                        let _ = child.kill(); // Windows: kill() already terminates the process tree
                        let _ = child.kill();
                        let _ = child.wait();
                        break true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(_) => break false,
            }
        };

        if timed_out {
            // Drain the reader threads to avoid leaking
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return ToolResult::error(format!(
                "Error: command timed out after {}ms: {}",
                timeout_ms, command
            ));
        }

        let output = match child.wait() {
            Ok(o) => o,
            Err(e) => return ToolResult::error(format!("Error: {}", e)),
        };

        let stdout_bytes = stdout_thread.join().unwrap_or_default();
        let stderr_bytes = stderr_thread.join().unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let stderr = String::from_utf8_lossy(&stderr_bytes);

        // Extract exit code, handling signal-killed processes with 128+signal convention.
        let exit_code = match output.code() {
            Some(code) => code,
            None => {
                // Process was terminated by a signal (Unix).
                // Use the standard convention: 128 + signal number.
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(signal) = output.signal() {
                        128 + signal
                    } else {
                        -1
                    }
                }
                #[cfg(not(unix))]
                {
                    -1
                }
            }
        };

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(&stderr);
        }

        // Add exit code
        result.push_str(&format!("\nExit code: {}", exit_code));

        // Truncate if too large (matching official: default 30k, prefix-only with line count)
        const MAX_OUTPUT: usize = 30000;
        if result.len() > MAX_OUTPUT {
            // Find a valid UTF-8 boundary near MAX_OUTPUT
            let mut cut = MAX_OUTPUT;
            while cut > 0 && !result.is_char_boundary(cut) { cut -= 1; }
            let truncated_part = &result[..cut];
            let total_lines = result.matches('\n').count();
            let shown_lines = truncated_part.matches('\n').count();
            let remaining_lines = total_lines - shown_lines;
            result = format!(
                "{}\n\n... [{} lines truncated] ...",
                truncated_part,
                remaining_lines
            );
        }

        if result.is_empty() {
            result = "(no output)".to_string();
        }

        let metadata = crate::tools::ToolResultMetadata {
            tool_name: "exec".to_string(),
            exit_code: Some(exit_code),
            duration_ms: 0,
            output_lines: 0,
            truncated: false,
        };

        ToolResult {
            output: result,
            is_error: !output.success(),
            metadata,
        }
    }
}

// ─── Background execution ───────────────────────────────────────────────────

impl ExecTool {
    /// Execute a command in the background. Delegates to the background callback
    /// if set, otherwise falls back to foreground execution.
    fn exec_in_background(&self, params: &HashMap<String, Value>) -> ToolResult {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.trim().to_string(),
            None => return ToolResult::error("Error: empty command"),
        };

        if command.is_empty() {
            return ToolResult::error("Error: empty command");
        }

        // If no callback is configured, fall back to foreground execution
        if self.background_callback.is_none() {
            return self.exec_foreground(params.clone());
        }

        // Determine working directory
        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());

        let callback = self.background_callback.as_ref().unwrap();
        let (task_id, output_file, err_text) = callback(command.clone(), working_dir.clone());

        if !err_text.is_empty() {
            return ToolResult::error(err_text);
        }

        ToolResult::ok(format!(
            "Background task started.\nTask ID: {}\nOutput file: {}\nUse the task_output tool to check results when ready.",
            task_id, output_file
        ))
    }
}

// ─── TaskStopTool ───────────────────────────────────────────────────────────

/// Callback for stopping/killing a background task by ID.
type TaskStopFunc = Arc<dyn Fn(String) -> Result<(), String> + Send + Sync>;

pub struct TaskStopTool {
    stop_func: TaskStopFunc,
}

impl TaskStopTool {
    pub fn new(stop_func: TaskStopFunc) -> Self {
        Self { stop_func }
    }
}

impl Clone for TaskStopTool {
    fn clone(&self) -> Self {
        Self {
            stop_func: Arc::clone(&self.stop_func),
        }
    }
}

impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop a running background bash task by its ID. Use this to terminate long-running or stuck processes."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to stop (e.g., 'b3f2a1c4')"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        match (self.stop_func)(task_id.to_string()) {
            Ok(()) => ToolResult::ok(format!("Task {} stopped successfully", task_id)),
            Err(e) => ToolResult::error(e),
        }
    }
}

// ─── TaskOutputTool ─────────────────────────────────────────────────────────

/// Callback for reading background task output.
/// (task_id, block, timeout_ms) -> (output, error_text)
type TaskOutputFunc =
    Arc<dyn Fn(String, bool, u64) -> (String, String) + Send + Sync>;

/// task_output reads the output file of a background bash task.
pub struct TaskOutputTool {
    output_func: TaskOutputFunc,
}

impl TaskOutputTool {
    pub fn new(output_func: TaskOutputFunc) -> Self {
        Self { output_func }
    }
}

impl Clone for TaskOutputTool {
    fn clone(&self) -> Self {
        Self {
            output_func: Arc::clone(&self.output_func),
        }
    }
}

impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        "Read the output of a background bash task. Returns the full output file content with a status header."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task (e.g., 'b3f2a1c4')"
                },
                "block": {
                    "type": "boolean",
                    "description": "If true, wait for the task to complete before returning (default: false)"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Maximum time to wait when block=true, in milliseconds (default: 30000, max: 600000)"
                }
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if task_id.is_empty() {
            return ToolResult::error("task_id is required");
        }

        let block = params
            .get("block")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let timeout_ms = params
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(30000)  // default: 30 seconds (matching official)
            .clamp(1, 600000) as u64;

        let (output, err_text) =
            (self.output_func)(task_id.to_string(), block, timeout_ms);

        if !err_text.is_empty() {
            return ToolResult::error(err_text);
        }

        ToolResult::ok(output)
    }
}

// ─── Helper functions for building callbacks ────────────────────────────────

/// Build a stop callback from a TaskStore.
pub fn make_task_stop_func(task_store: crate::task_store::SharedTaskStore) -> TaskStopFunc {
    Arc::new(move |task_id: String| task_store.kill_task(&task_id))
}

/// Build an output callback from a TaskStore.
/// Returns (output, error_text).
pub fn make_task_output_func(task_store: crate::task_store::SharedTaskStore) -> TaskOutputFunc {
    Arc::new(move |task_id: String, block: bool, timeout_ms: u64| {
        let task_arc = task_store.get_task(&task_id);

        let task_arc = match task_arc {
            Some(t) => t,
            None => {
                return (
                    String::new(),
                    format!("Background task {} not found", task_id),
                );
            }
        };

        // If block is true, wait for task to finish
        if block {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
            loop {
                let is_terminal = {
                    let task = task_arc.lock().unwrap();
                    task.is_terminal()
                };
                if is_terminal {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let task = task_arc.lock().unwrap();
                    return (
                        format!(
                            "Task {} ({}) -- timeout after {}ms (still running, try increasing timeout or check task_output again later)",
                            task_id,
                            task.status,
                            timeout_ms
                        ),
                        String::new(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }

        // Read output file
        let (output_file, status) = {
            let task = task_arc.lock().unwrap();
            match task.output_file {
                Some(ref path) => (path.clone(), task.status),
                None => {
                    return (
                        String::new(),
                        format!("Task {} has no output file", task_id),
                    );
                }
            }
        };

        let content = match std::fs::read_to_string(&output_file) {
            Ok(c) => c,
            Err(e) => {
                return (
                    String::new(),
                    format!("Error reading output file: {}", e),
                );
            }
        };

        // Truncate if too large (matching official: default 30k, prefix-only with line count)
        const MAX_OUTPUT: usize = 30000;
        let output = if content.len() > MAX_OUTPUT {
            // Find a valid UTF-8 boundary
            let mut cut = MAX_OUTPUT;
            while cut > 0 && !content.is_char_boundary(cut) { cut -= 1; }
            let truncated_part = &content[..cut];
            let total_lines = content.matches('\n').count();
            let shown_lines = truncated_part.matches('\n').count();
            let remaining_lines = total_lines - shown_lines;
            format!(
                "{}\n\n... [{} lines truncated] ...",
                truncated_part,
                remaining_lines
            )
        } else {
            content
        };

        (
            format!("Task {} ({}) -- output:\n{}", task_id, status, output),
            String::new(),
        )
    })
}

/// Build a background callback from a TaskStore.
/// This is called by ExecTool when run_in_background=true.
/// Returns (task_id, output_file, error_text).
pub fn make_bash_bg_callback(
    task_store: crate::task_store::SharedTaskStore,
    notification_tx: std::sync::Arc<tokio::sync::mpsc::UnboundedSender<String>>,
) -> BashBgTaskCallback {
    Arc::new(move |command: String, working_dir: String| {
        let tx = notification_tx.clone();
        spawn_background_bash(&task_store, tx, command, working_dir)
    })
}

// ─── Background bash spawning ──────────────────────────────────────────────

/// Spawn a background bash command and register it in the TaskStore.
/// Returns (task_id, output_file, error_text).
fn spawn_background_bash(
    task_store: &crate::task_store::SharedTaskStore,
    notification_tx: std::sync::Arc<tokio::sync::mpsc::UnboundedSender<String>>,
    command: String,
    working_dir: String,
) -> (String, String, String) {
    use crate::task_store::bash_bg_tasks_dir;

    // Determine shell
    let (shell, flag) = detect_shell_inline();

    // Create output directory
    let output_dir = bash_bg_tasks_dir();
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        return (
            String::new(),
            String::new(),
            format!("Error: failed to create task output directory: {}", e),
        );
    }

    // Create/truncate the output file with a temporary path (will be renamed after registration)
    // First, we need to register to get the canonical task_id from TaskStore
    // Use a placeholder for the header
    let output_file = output_dir.join("pending.output");
    if let Err(e) = write_output_header(&output_file, "pending", &command, &working_dir) {
        return (
            String::new(),
            String::new(),
            format!("Error: failed to create output file: {}", e),
        );
    }

    // Register task in the TaskStore -- this generates the canonical task_id
    let output_file_str = output_file.to_string_lossy().to_string();
    let task_id = task_store.register_bash_bg_task(command.clone(), output_file_str.clone());

    // Rename the output file to use the canonical task_id
    let final_output_file = output_dir.join(format!("{}.output", task_id));
    let _ = std::fs::rename(&output_file, &final_output_file);
    // Update the TaskStore entry with the correct output file path
    let final_output_file_str = final_output_file.to_string_lossy().to_string();
    task_store.update_output_file(&task_id, final_output_file_str.clone());

    // Re-write header with correct task_id
    if let Err(e) = write_output_header(&final_output_file, &task_id, &command, &working_dir) {
        return (
            String::new(),
            String::new(),
            format!("Error: failed to write output header: {}", e),
        );
    }

    // Spawn a dedicated background thread to run the process
    let task_store_clone = Arc::clone(task_store);
    let notification_tx_clone = notification_tx.clone();
    let output_file_clone = final_output_file_str.clone();
    let task_id_clone = task_id.clone();
    let command_clone = command.clone();
    let working_dir_clone = working_dir.clone();
    let shell_owned = shell.to_string();
    let flag_owned = flag.to_string();

    std::thread::Builder::new()
        .name(format!("bg-task-{}", task_id))
        .spawn(move || {
            run_background_bash(
                &task_store_clone,
                notification_tx_clone,
                &task_id_clone,
                &output_file_clone,
                &shell_owned,
                &flag_owned,
                &command_clone,
                &working_dir_clone,
            );
        })
        .expect("failed to spawn background task thread");

    (task_id, final_output_file_str, String::new())
}

/// Detect shell inline (no caching -- for spawned threads).
fn detect_shell_inline() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        if std::process::Command::new("powershell")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            ("powershell", "-Command")
        } else if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            ("bash", "-c")
        } else {
            ("cmd", "/C")
        }
    } else {
        ("bash", "-c")
    }
}

/// Write the header to the output file.
fn write_output_header(
    path: &std::path::Path,
    task_id: &str,
    command: &str,
    working_dir: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "--- Background Task: {} ---", task_id)?;
    writeln!(f, "Command: {}", command)?;
    writeln!(f, "Working Dir: {}", working_dir)?;
    writeln!(
        f,
        "Started: {}",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    )?;
    writeln!(f, "--- Output ---\n")?;
    Ok(())
}

/// Run the background bash command in a dedicated thread.
/// Uses std::process::Command and writes output to file.
fn run_background_bash(
    task_store: &crate::task_store::SharedTaskStore,
    notification_tx: std::sync::Arc<tokio::sync::mpsc::UnboundedSender<String>>,
    task_id: &str,
    output_file: &str,
    shell: &str,
    flag: &str,
    command: &str,
    working_dir: &str,
) {
    let start = std::time::Instant::now();

    // Spawn the child process
    let mut cmd = std::process::Command::new(shell);
    cmd.arg(flag)
        .arg(command)
        .current_dir(working_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Set process group on Unix for tree-kill support
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let spawn_result = cmd.spawn();

    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            let err_text = format!("Error: failed to start command: {}", e);
            let _ = append_to_output_file(output_file, &format!("{}\n", err_text));
            let _ = task_store.fail_task(task_id, &err_text);
            let notification =
                make_notification(task_id, "failed", output_file, command, &err_text);
            let _ = notification_tx.send(notification);
            return;
        }
    };

    // Store the PID in the TaskStore BEFORE calling wait
    // This is the critical fix from the Go version -- Process must be set before Wait()
    let pid = child.id();
    task_store.set_pid(task_id, pid);

    // Wait for the process to complete
    let output_result = child.wait_with_output();
    let elapsed = start.elapsed();

    match output_result {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);

            // Write stdout to output file
            if !stdout_str.is_empty() {
                let _ = append_to_output_file(output_file, &stdout_str);
            }
            // Write stderr to output file
            if !stderr_str.is_empty() {
                if !stdout_str.is_empty() {
                    let _ = append_to_output_file(output_file, "\n--- stderr ---\n");
                }
                let _ = append_to_output_file(output_file, &stderr_str);
            }

            // Write footer
            let footer = format!(
                "\n--- Task Complete ---\nExit code: {}\nDuration: {:.2}s\nStatus: {}\n",
                exit_code,
                elapsed.as_secs_f64(),
                if exit_code == 0 {
                    "completed"
                } else {
                    "failed"
                }
            );
            let _ = append_to_output_file(output_file, &footer);

            // Guard: if task was already killed, don't overwrite status
            let already_killed = task_store.is_terminal(task_id);

            if !already_killed {
                if exit_code == 0 {
                    task_store.complete_task(task_id, "Command completed (exit code 0)");
                } else {
                    task_store.fail_task(
                        task_id,
                        &format!("Command failed with exit code {}", exit_code),
                    );
                }
            }

            // Send notification
            let (status_str, summary) = if already_killed {
                ("killed", "Command was stopped".to_string())
            } else if exit_code == 0 {
                ("completed", "Command completed successfully".to_string())
            } else {
                let summary = format!("Command failed (exit code {})", exit_code);
                ("failed", summary)
            };

            let notification =
                make_notification(task_id, status_str, output_file, command, &summary);
            let _ = notification_tx.send(notification);
        }
        Err(e) => {
            let err_text = format!("Error: failed to wait for command: {}", e);
            let _ = append_to_output_file(output_file, &format!("{}\n", err_text));

            // Guard: if already killed, don't overwrite
            if !task_store.is_terminal(task_id) {
                let _ = task_store.fail_task(task_id, &err_text);
            }

            let notification =
                make_notification(task_id, "failed", output_file, command, &err_text);
            let _ = notification_tx.send(notification);
        }
    }
}

/// Append text to the output file.
fn append_to_output_file(path: &str, text: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// Build an XML task notification string.
fn make_notification(
    task_id: &str,
    status: &str,
    output_file: &str,
    command: &str,
    summary: &str,
) -> String {
    let command_escaped = escape_xml(command);
    let summary_escaped = escape_xml(summary);
    format!(
        r#"<task-notification>
<task_id>{}</task_id>
<task_type>bash_background</task_type>
<status>{}</status>
<output_file>{}</output_file>
<command>{}</command>
<summary>{}</summary>
</task-notification>"#,
        task_id, status, output_file, command_escaped, summary_escaped
    )
}

/// Escape special characters for XML.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
