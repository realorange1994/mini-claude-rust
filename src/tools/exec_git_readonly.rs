//! Git read-only command validation.
//! Ported from upstream exec_git_readonly.go (705 lines).
//!
//! Validates git subcommands and their flags against an allowlist.
//! Supports: diff, log, show, status, blame, ls-files, branch (list only),
//! tag (list only), reflog (show only), remote (show only), config --get,
//! rev-parse, describe, ls-remote, shortlog, stash list/show,
//! merge-base, for-each-ref, grep, worktree list.

use once_cell::sync::Lazy;
use std::collections::HashMap;

// ===========================================================================
// Git subcommand flag allowlists
// ===========================================================================

struct GitSubFlagConfig {
    safe_flags: Vec<&'static str>,
    arg_taking_flags: Vec<&'static str>,
    dangerous: bool,
}

static GIT_SUB_COMMANDS: Lazy<HashMap<&'static str, GitSubFlagConfig>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // diff
    m.insert("diff", GitSubFlagConfig {
        safe_flags: vec![
            "--stat", "--numstat", "--name-only", "--name-status", "--shortstat",
            "--no-color", "--color", "--color-words", "--word-diff",
            "-U", "-p", "--patch", "--raw", "--patch-with-raw",
            "--summary", "--patch-with-stat",
            "-R", "--reverse",
            "-w", "--ignore-all-space", "-b", "--ignore-space-change",
            "--ignore-space-at-eol", "--ignore-cr-at-eol",
            "--indent-heuristic", "--no-indent-heuristic",
            "--patience", "--histogram", "--diff-algorithm",
            "--find-renames", "-M", "--find-copies", "-C",
            "--find-copies-harder", "-D", "--irreversible-delete",
            "-l", "--diff-filter", "-S", "-G", "--pickaxe-all",
            "--pickaxe-regex", "-O", "--skip", "--rotate-to",
            "--skip-to", "--find-object",
            "-a", "--text", "--no-index", "--exit-code",
            "--quiet", "--ext-diff", "--no-ext-diff",
            "--textconv", "--no-textconv",
            "--ignore-submodules",
            "--src-prefix", "--dst-prefix", "--no-prefix",
            "--inter-hunk-context", "--ws-error-highlight",
            "--anchored",
            "--cached", "--staged",
            "--merge-base",
            "--base", "--ours", "--theirs",
            "-z",
            "-q",
            "--follow",
            "-W", "--function-context",
            "--ita2-invisible",
            "--expand-tabs", "--notes",
        ],
        arg_taking_flags: vec![
            "-U", "-l", "-S", "-G", "-O", "--skip", "--rotate-to", "--skip-to",
            "--find-object", "--inter-hunk-context", "--src-prefix", "--dst-prefix",
            "--diff-filter", "--diff-algorithm", "--ws-error-highlight", "--anchored",
            "--expand-tabs", "--notes", "--color", "--word-diff", "--ignore-submodules",
        ],
        dangerous: false,
    });

    // log
    m.insert("log", GitSubFlagConfig {
        safe_flags: vec![
            "--oneline", "--decorate", "--decorate-refs", "--decorate-refs-exclude",
            "--source", "--mailmap", "--use-mailmap",
            "--no-color", "--color", "--graph", "--show-signature",
            "--stat", "--numstat", "--name-only", "--name-status", "--shortstat",
            "-U", "-p", "--patch", "--raw", "--patch-with-raw",
            "--summary", "--patch-with-stat",
            "--merges", "--no-merges", "--first-parent",
            "--ancestry-path", "--full-history",
            "-L", "--follow",
            "-w", "--ignore-all-space", "-b", "--ignore-space-change",
            "--reverse", "-R",
            "--find-renames", "-M", "--find-copies", "-C",
            "--diff-filter",
            "-n", "--max-count", "--skip",
            "--since", "--after", "--until", "--before",
            "--author", "--committer", "--grep",
            "--all-match", "--invert-grep",
            "-i", "--regexp-ignore-case",
            "--basic-regexp", "-E", "--extended-regexp",
            "-F", "--fixed-strings", "-P", "--perl-regexp",
            "--remove-empty",
            "--merges", "--no-merges",
            "--min-parents", "--max-parents", "--no-walk",
            "--reflog", "--walk-reflogs",
            "-g",
            "--children", "--parents",
            "--left-right", "--cherry-mark", "--cherry-pick",
            "--right-only", "--left-only",
            "--count",
            "--simplify-merges", "--simplify-by-decoration",
            "--full-diff",
            "--relative-date", "--date",
            "--pretty", "--format",
            "--abbrev-commit", "--no-abbrev-commit",
            "-z", "-q",
            "-x",
            "--diff-merges", "--no-diff-merges",
            "--combined-all-paths",
            "--cc",
            "-m", "-c",
            "--notes",
            "--expand-tabs",
        ],
        arg_taking_flags: vec![
            "-U", "-L", "-n", "--max-count", "--skip",
            "--since", "--after", "--until", "--before",
            "--author", "--committer", "--grep",
            "--min-parents", "--max-parents",
            "--pretty", "--format", "--date",
            "--diff-filter", "--abbrev", "-x",
            "--diff-merges", "--notes", "--expand-tabs",
            "--color", "--decorate-refs", "--decorate-refs-exclude",
            "--ignore-submodules",
        ],
        dangerous: false,
    });

    // show
    m.insert("show", GitSubFlagConfig {
        safe_flags: vec![
            "--stat", "--numstat", "--name-only", "--name-status", "--shortstat",
            "-U", "-p", "--patch", "--raw",
            "--no-color", "--color",
            "-w", "--ignore-all-space", "-b", "--ignore-space-change",
            "--find-renames", "-M", "--find-copies", "-C",
            "--pretty", "--format",
            "--abbrev-commit", "--no-abbrev-commit",
            "--relative-date", "--date",
            "-q", "-s",
            "--show-signature",
            "--notes",
            "--expand-tabs",
        ],
        arg_taking_flags: vec![
            "-U", "--pretty", "--format", "--date",
            "--abbrev", "--notes", "--expand-tabs", "--color",
            "--ignore-submodules",
        ],
        dangerous: false,
    });

    // status
    m.insert("status", GitSubFlagConfig {
        safe_flags: vec![
            "-s", "--short", "--branch", "-b",
            "--porcelain", "--no-color", "--color",
            "--show-stash", "--long", "-v", "--verbose",
            "-u", "--untracked-files",
            "--ignore-submodules",
            "--column", "--no-column",
            "-z",
        ],
        arg_taking_flags: vec![
            "-u", "--untracked-files", "--column", "--color", "--ignore-submodules",
        ],
        dangerous: false,
    });

    // blame
    m.insert("blame", GitSubFlagConfig {
        safe_flags: vec![
            "-b", "--root", "--show-stats",
            "-L", "-l", "-t", "-S", "--score",
            "-f", "--show-name", "-n", "--show-number",
            "-p", "--porcelain", "--line-porcelain",
            "-c", "-w", "--incremental",
            "-M", "-C",
            "--no-color", "--color",
            "-e", "--show-email",
            "-s", "--abbrev",
            "--date",
            "--reverse",
        ],
        arg_taking_flags: vec![
            "-L", "--score", "--abbrev", "--date", "--color",
        ],
        dangerous: false,
    });

    // ls-files
    m.insert("ls-files", GitSubFlagConfig {
        safe_flags: vec![
            "-c", "--cached", "-d", "--deleted",
            "-m", "--modified", "-o", "--others",
            "-i", "--ignored", "-s", "--stage",
            "-u", "--unmerged", "-k", "--killed",
            "--directory", "--no-empty-directory",
            "-e", "--exclude",
            "-x", "--exclude-from",
            "-X", "--exclude-per-directory",
            "--exclude-standard",
            "--full-name", "--recurse-submodules",
            "-t", "-v",
            "-f",
            "-z",
            "--deduplicate",
            "--debug",
            "-h",
        ],
        arg_taking_flags: vec![
            "-x", "-X", "--exclude-from", "--exclude-per-directory", "--exclude",
        ],
        dangerous: false,
    });

    // branch
    m.insert("branch", GitSubFlagConfig {
        safe_flags: vec![
            "-a", "--all", "-r", "--remotes",
            "-l", "--list",
            "-v", "-vv", "--verbose",
            "-q", "--quiet",
            "--no-color", "--color",
            "--merged", "--no-merged",
            "--contains", "--no-contains",
            "--sort", "--format",
            "--points-at",
            "--abbrev",
            "--show-current",
            "-t", "--track",
            "--no-track",
        ],
        arg_taking_flags: vec![
            "--merged", "--no-merged", "--contains", "--no-contains",
            "--sort", "--format", "--points-at", "--abbrev", "--color",
        ],
        dangerous: true, // positional args can create/modify branches
    });

    // tag
    m.insert("tag", GitSubFlagConfig {
        safe_flags: vec![
            "-l", "--list",
            "-n",
            "--contains", "--no-contains",
            "--merged", "--no-merged",
            "--sort", "--format",
            "--no-color", "--color",
        ],
        arg_taking_flags: vec![
            "--contains", "--no-contains", "--merged", "--no-merged",
            "--sort", "--format", "--color",
        ],
        dangerous: true, // positional args can create/delete tags
    });

    // reflog
    m.insert("reflog", GitSubFlagConfig {
        safe_flags: vec![
            "show",
            "--oneline", "--no-abbrev-commit", "--date",
            "-n",
            "--stat", "--name-only", "--name-status",
            "-p", "--patch",
            "--pretty", "--format",
            "--no-color", "--color",
        ],
        arg_taking_flags: vec![
            "--date", "-n", "--pretty", "--format", "--color",
        ],
        dangerous: true, // subcommands like expire/delete are dangerous
    });

    // remote
    m.insert("remote", GitSubFlagConfig {
        safe_flags: vec![
            "-v", "--verbose",
            "show",
        ],
        arg_taking_flags: vec![],
        dangerous: true,
    });

    // config
    m.insert("config", GitSubFlagConfig {
        safe_flags: vec![
            "--get", "--get-all", "--get-regexp",
            "--list", "-l",
            "--get-color", "--get-colorbool",
            "--name-only", "--show-origin", "--show-scope",
            "--show-name",
            "-z", "--null",
            "--includes", "--no-includes",
            "--local", "--global", "--system", "--worktree",
        ],
        arg_taking_flags: vec![
            "--get", "--get-all", "--get-regexp",
            "--get-color", "--get-colorbool",
        ],
        dangerous: false,
    });

    // rev-parse
    m.insert("rev-parse", GitSubFlagConfig {
        safe_flags: vec![
            "--short", "--verify", "--quiet", "-q",
            "--abbrev-ref",
            "--symbolic", "--symbolic-full-name",
            "--show-toplevel", "--show-prefix",
            "--show-cdup", "--is-inside-work-tree",
            "--is-inside-git-dir", "--is-bare-repository",
            "--resolve-git-dir",
            "--git-dir", "--git-common-dir",
            "--local-env-vars",
            "--path-format",
        ],
        arg_taking_flags: vec![
            "--short", "--abbrev-ref", "--resolve-git-dir", "--path-format",
        ],
        dangerous: false,
    });

    // describe
    m.insert("describe", GitSubFlagConfig {
        safe_flags: vec![
            "--all", "--tags", "--contains",
            "--abbrev", "--candidates", "--exact-match",
            "--debug", "--long", "--match", "--exclude",
            "--always", "--dirty", "--broken",
            "--first-parent",
        ],
        arg_taking_flags: vec![
            "--abbrev", "--candidates", "--match", "--exclude", "--dirty", "--broken",
        ],
        dangerous: false,
    });

    // ls-remote
    m.insert("ls-remote", GitSubFlagConfig {
        safe_flags: vec![
            "--heads", "-h", "--tags", "-t",
            "--refs",
            "--quiet", "-q",
            "--exit-code",
            "--get-url",
            "--sort", "--server-option",
            "--symref",
            "--no-tags",
        ],
        arg_taking_flags: vec![
            "--sort", "--server-option",
        ],
        dangerous: false,
    });

    // shortlog
    m.insert("shortlog", GitSubFlagConfig {
        safe_flags: vec![
            "-n", "--numbered",
            "-s", "--summary",
            "-e", "--email",
            "-c", "--committer",
            "-w",
            "--format",
            "--group",
            "--no-color", "--color",
        ],
        arg_taking_flags: vec![
            "-w", "--format", "--group", "--color",
        ],
        dangerous: false,
    });

    // stash (list/show only)
    m.insert("stash", GitSubFlagConfig {
        safe_flags: vec![
            "list", "show",
            "--stat", "--name-only", "--name-status",
            "-p", "--patch",
            "--no-color", "--color",
            "-u", "--include-untracked",
        ],
        arg_taking_flags: vec![
            "--color",
        ],
        dangerous: true, // only list/show are safe
    });

    // merge-base
    m.insert("merge-base", GitSubFlagConfig {
        safe_flags: vec![
            "-a", "--all",
            "--is-ancestor",
            "--independent",
            "--fork-point",
            "--octopus",
        ],
        arg_taking_flags: vec![],
        dangerous: false,
    });

    // for-each-ref
    m.insert("for-each-ref", GitSubFlagConfig {
        safe_flags: vec![
            "--format", "--sort", "--count",
            "--contains", "--no-contains",
            "--merged", "--no-merged",
            "--points-at",
            "--ignore-case",
            "--no-color", "--color",
        ],
        arg_taking_flags: vec![
            "--format", "--sort", "--count",
            "--contains", "--no-contains",
            "--merged", "--no-merged", "--points-at", "--color",
        ],
        dangerous: false,
    });

    // grep
    m.insert("grep", GitSubFlagConfig {
        safe_flags: vec![
            "-n", "--line-number",
            "-h", "-H",
            "-l", "--files-with-matches",
            "-L", "--files-without-matches",
            "-e", "--regexp",
            "-f", "--file",
            "-i", "--ignore-case",
            "-v", "--invert-match",
            "-w", "--word-regexp",
            "-c", "--count",
            "--heading", "--break",
            "--show-function",
            "-p", "--show-function",
            "-W", "--function-context",
            "-o", "--only-matching",
            "-E", "--extended-regexp",
            "-G", "--basic-regexp",
            "-P", "--perl-regexp",
            "-F", "--fixed-strings",
            "--all-match", "--invert-grep",
            "-q", "--quiet",
            "--max-depth",
            "-a", "--text",
            "-I",
            "--textconv", "--no-textconv",
            "--recursive", "--no-recursive",
            "-r",
            "--untracked", "--no-untracked",
            "--cached",
            "--exclude-standard",
            "-O", "--open-files-in-pager",
            "-z",
            "--column",
            "--no-color", "--color",
            "-m", "--max-count",
        ],
        arg_taking_flags: vec![
            "-e", "-f", "-O", "--max-depth", "--max-count",
            "--color", "--regexp", "--file",
        ],
        dangerous: false,
    });

    // worktree (list only)
    m.insert("worktree", GitSubFlagConfig {
        safe_flags: vec![
            "list",
            "--porcelain",
            "-v", "--verbose",
        ],
        arg_taking_flags: vec![],
        dangerous: true, // add/remove/prune are dangerous
    });

    m
});

// ===========================================================================
// Dangerous subcommand callbacks
// ===========================================================================

/// Returns true if the git subcommand with its flags requires approval
/// despite being partially in the allowlist (e.g., branch, tag, reflog, remote).
fn is_dangerous_subcommand(sub: &str, args: &[&str]) -> bool {
    match sub {
        "branch" | "tag" => {
            // If there are any positional args, it's dangerous
            args.iter().any(|a| !a.starts_with('-'))
        }
        "reflog" => {
            // Only "show" is safe; "expire" and "delete" are dangerous
            if let Some(first) = args.first() {
                let lower = first.to_lowercase();
                lower != "show" && !lower.starts_with('-')
            } else {
                false // bare "git reflog" is equivalent to "git reflog show"
            }
        }
        "remote" => {
            // Only "show" and bare "-v" are safe
            if let Some(first) = args.first() {
                let lower = first.to_lowercase();
                lower != "show" && lower != "-v" && lower != "--verbose" && !lower.starts_with('-')
            } else {
                false // bare "git remote" lists remotes
            }
        }
        "stash" => {
            // Only "list" and "show" are safe
            if let Some(first) = args.first() {
                let lower = first.to_lowercase();
                lower != "list" && lower != "show" && !lower.starts_with('-')
            } else {
                true // bare "git stash" would stash changes
            }
        }
        "worktree" => {
            // Only "list" is safe
            if let Some(first) = args.first() {
                let lower = first.to_lowercase();
                lower != "list" && !lower.starts_with('-')
            } else {
                true
            }
        }
        _ => false,
    }
}

// ===========================================================================
// Flag validation
// ===========================================================================

fn is_git_flag_arg_taking(sub: &str, flag: &str) -> bool {
    if let Some(config) = GIT_SUB_COMMANDS.get(sub) {
        // Exact match
        if config.arg_taking_flags.contains(&flag) {
            return true;
        }
        // Handle --flag=VALUE form
        if flag.contains('=') {
            let base = flag.split('=').next().unwrap_or(flag);
            return config.arg_taking_flags.contains(&base);
        }
    }
    false
}

fn is_safe_git_flag(sub: &str, flag: &str) -> bool {
    if let Some(config) = GIT_SUB_COMMANDS.get(sub) {
        if config.safe_flags.contains(&flag) {
            return true;
        }
        // Handle --flag=VALUE form
        if flag.contains('=') {
            let base = flag.split('=').next().unwrap_or(flag);
            return config.safe_flags.contains(&base);
        }
    }
    false
}

fn validate_git_subcommand_flags(sub: &str, args: &[&str]) -> bool {
    let config = match GIT_SUB_COMMANDS.get(sub) {
        Some(c) => c,
        None => return false,
    };

    // Check dangerous callback first
    if config.dangerous && is_dangerous_subcommand(sub, args) {
        return false;
    }

    let mut i = 0;
    while i < args.len() {
        let arg = args[i];

        // Skip subcommand keyword for compound subcommands like "reflog show"
        if i == 0 && !arg.starts_with('-') && config.dangerous {
            // For "branch -a", "tag -l", "reflog show", "remote show", etc.
            // the first non-flag arg is part of the subcommand itself
            if is_dangerous_subcommand(sub, args) {
                return false;
            }
            i += 1;
            continue;
        }

        if !arg.starts_with('-') {
            // Positional arg — could be a path/rev for some commands
            i += 1;
            continue;
        }

        if arg == "--" {
            break;
        }

        // Handle combined short flags like -on
        if arg.len() > 2 && !arg.starts_with("--") {
            // Check if it's a known flag with attached value (like -U3)
            let single = &arg[..2];
            if is_git_flag_arg_taking(sub, single) {
                // Flag with attached value — valid
                i += 1;
                continue;
            }
            // Combined short flags
            for c in arg[1..].chars() {
                let flag = format!("-{}", c);
                if !is_safe_git_flag(sub, &flag) {
                    return false;
                }
                if is_git_flag_arg_taking(sub, &flag) {
                    return false; // can't bundle arg-taking flags
                }
            }
            i += 1;
            continue;
        }

        if !is_safe_git_flag(sub, arg) {
            return false;
        }

        if is_git_flag_arg_taking(sub, arg) && !arg.contains('=') {
            // Next token is the argument value
            i += 2;
            continue;
        }

        i += 1;
    }

    true
}

// ===========================================================================
// Parse git subcommand from command line
// ===========================================================================

/// Skip past git global flags like -C, -c, --git-dir, etc.
fn skip_git_global_flags(fields: &[&str]) -> Option<usize> {
    let mut i = 1; // skip "git" itself

    let global_flags_with_args: &[&str] = &[
        "-C", "-c", "--config-env", "--exec-path",
        "--git-dir", "--work-tree", "--namespace", "--super-prefix",
        "--man-path", "--info-path", "--html-path",
        "--list-cmds",
    ];

    while i < fields.len() {
        let f = fields[i];
        if !f.starts_with('-') {
            break;
        }
        // Handle --flag=value form
        if f.contains('=') {
            i += 1;
            continue;
        }
        // Check if this global flag takes an argument
        if global_flags_with_args.contains(&f) {
            i += 2;
            continue;
        }
        // Flag without argument (--version, --help, --no-replace-objects, etc.)
        i += 1;
    }

    if i < fields.len() {
        Some(i)
    } else {
        None
    }
}

/// Extract git subcommand and remaining args from a full command string.
fn parse_git_command(cmd: &str) -> Option<(String, Vec<&str>)> {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.is_empty() || fields[0].to_lowercase() != "git" {
        return None;
    }

    let sub_idx = skip_git_global_flags(&fields)?;
    let sub = fields[sub_idx].to_lowercase();

    let args: Vec<&str> = fields[sub_idx + 1..].to_vec();
    Some((sub, args))
}

// ===========================================================================
// bash_ro_is_git_read_only_command — main entry point
// ===========================================================================

/// Checks if a git command is read-only.
/// Returns true if read-only, false if unknown or dangerous.
pub fn bash_ro_is_git_read_only_command(cmd: &str) -> bool {
    let (sub, args) = match parse_git_command(cmd) {
        Some(pair) => pair,
        None => return false,
    };

    if !GIT_SUB_COMMANDS.contains_key(sub.as_str()) {
        return false;
    }

    validate_git_subcommand_flags(&sub, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_log_read_only() {
        assert!(bash_ro_is_git_read_only_command("git log --oneline"));
        assert!(bash_ro_is_git_read_only_command("git log -n 5 --stat"));
        assert!(!bash_ro_is_git_read_only_command("git log --unknown-flag"));
    }

    #[test]
    fn test_git_diff_read_only() {
        assert!(bash_ro_is_git_read_only_command("git diff"));
        assert!(bash_ro_is_git_read_only_command("git diff --stat HEAD~1"));
        assert!(bash_ro_is_git_read_only_command("git diff --cached"));
    }

    #[test]
    fn test_git_status_read_only() {
        assert!(bash_ro_is_git_read_only_command("git status"));
        assert!(bash_ro_is_git_read_only_command("git status -s"));
    }

    #[test]
    fn test_git_show_read_only() {
        assert!(bash_ro_is_git_read_only_command("git show HEAD"));
        assert!(bash_ro_is_git_read_only_command("git show --stat abc123"));
    }

    #[test]
    fn test_git_branch_read_only() {
        assert!(bash_ro_is_git_read_only_command("git branch -a"));
        assert!(bash_ro_is_git_read_only_command("git branch --list"));
        assert!(!bash_ro_is_git_read_only_command("git branch new-branch"));
    }

    #[test]
    fn test_git_tag_read_only() {
        assert!(bash_ro_is_git_read_only_command("git tag -l"));
        assert!(!bash_ro_is_git_read_only_command("git tag v1.0"));
    }

    #[test]
    fn test_git_reflog_read_only() {
        assert!(bash_ro_is_git_read_only_command("git reflog"));
        assert!(bash_ro_is_git_read_only_command("git reflog show"));
        assert!(!bash_ro_is_git_read_only_command("git reflog expire"));
        assert!(!bash_ro_is_git_read_only_command("git reflog delete HEAD@{1}"));
    }

    #[test]
    fn test_git_remote_read_only() {
        assert!(bash_ro_is_git_read_only_command("git remote -v"));
        assert!(bash_ro_is_git_read_only_command("git remote show origin"));
        assert!(!bash_ro_is_git_read_only_command("git remote add upstream url"));
    }

    #[test]
    fn test_git_config_read_only() {
        assert!(bash_ro_is_git_read_only_command("git config --get user.name"));
        assert!(bash_ro_is_git_read_only_command("git config --list"));
        assert!(!bash_ro_is_git_read_only_command("git config user.name NewName"));
    }

    #[test]
    fn test_git_stash_read_only() {
        assert!(bash_ro_is_git_read_only_command("git stash list"));
        assert!(bash_ro_is_git_read_only_command("git stash show"));
        assert!(!bash_ro_is_git_read_only_command("git stash"));
        assert!(!bash_ro_is_git_read_only_command("git stash pop"));
    }

    #[test]
    fn test_git_worktree_read_only() {
        assert!(bash_ro_is_git_read_only_command("git worktree list"));
        assert!(!bash_ro_is_git_read_only_command("git worktree add ../path"));
    }

    #[test]
    fn test_git_unknown_subcommand() {
        assert!(!bash_ro_is_git_read_only_command("git push"));
        assert!(!bash_ro_is_git_read_only_command("git commit -m test"));
        assert!(!bash_ro_is_git_read_only_command("git checkout main"));
    }

    #[test]
    fn test_git_global_flags() {
        assert!(bash_ro_is_git_read_only_command("git -C /tmp log --oneline"));
        assert!(bash_ro_is_git_read_only_command("git --git-dir=.git status"));
    }

    #[test]
    fn test_parse_git_command() {
        let (sub, args) = parse_git_command("git log --oneline -n 5").unwrap();
        assert_eq!(sub, "log");
        assert_eq!(args, vec!["--oneline", "-n", "5"]);

        let (sub, _) = parse_git_command("git -C /tmp diff --stat").unwrap();
        assert_eq!(sub, "diff");

        assert!(parse_git_command("echo hello").is_none());
    }
}