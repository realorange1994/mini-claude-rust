//! Bash command security validation.
//! Ported from upstream exec_bash_security.go (1773 lines).
//!
//! Implements CheckBashPermission — the main entry point for bash/shell
//! permission checks. Uses regex pattern matching, env var validation,
//! and per-command security checks (jq, sed, xargs, fd, rg, gh, git, docker).

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::{HashMap, HashSet};

// ===========================================================================
// PermissionResult — shared result type for permission checks
// ===========================================================================

/// Result of a permission check for a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResult {
    /// Command is allowed (read-only verified).
    Allow,
    /// Command is denied (hard block).
    Deny(String),
    /// Command requires user approval with a reason.
    Ask(String),
    /// No opinion — fall through to existing checks.
    Passthrough,
}

// ===========================================================================
// SAFE_ENV_VARS allowlist (upstream bashPermissions.ts lines 378-430)
// ===========================================================================

static SAFE_ENV_VARS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut s = HashSet::new();
    // Go toolchain
    s.insert("GOEXPERIMENT");
    s.insert("GOOS");
    s.insert("GOARCH");
    s.insert("CGO_ENABLED");
    s.insert("GO111MODULE");
    // Rust toolchain
    s.insert("RUST_BACKTRACE");
    s.insert("RUST_LOG");
    // Node.js
    s.insert("NODE_ENV");
    // Python
    s.insert("PYTHONUNBUFFERED");
    s.insert("PYTHONDONTWRITEBYTECODE");
    s.insert("PYTEST_DISABLE_PLUGIN_AUTOLOAD");
    s.insert("PYTEST_DEBUG");
    // Anthropic SDK
    s.insert("ANTHROPIC_API_KEY");
    // Locale and language
    s.insert("LANG");
    s.insert("LANGUAGE");
    s.insert("LC_ALL");
    s.insert("LC_CTYPE");
    s.insert("LC_TIME");
    s.insert("CHARSET");
    // Terminal
    s.insert("TERM");
    s.insert("COLORTERM");
    s.insert("NO_COLOR");
    s.insert("FORCE_COLOR");
    s.insert("TZ");
    // Color and formatting
    s.insert("LS_COLORS");
    s.insert("LSCOLORS");
    s.insert("GREP_COLOR");
    s.insert("GREP_COLORS");
    s.insert("GCC_COLORS");
    // Date and size formatting
    s.insert("TIME_STYLE");
    s.insert("BLOCK_SIZE");
    s.insert("BLOCKSIZE");
    s
});

static UNSAFE_ENV_PREFIXES: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "PATH=",
        "LD_PRELOAD=",
        "LD_LIBRARY_PATH=",
        "DYLD_",
        "PYTHONPATH=",
        "NODE_PATH=",
        "GOFLAGS=",
        "RUSTFLAGS=",
        "NODE_OPTIONS=",
        "HOME=",
        "TMPDIR=",
        "SHELL=",
        "BASH_ENV=",
    ]
});

/// Check if a VAR=val assignment prefix is unsafe. Returns the prefix if found.
fn is_unsafe_env_prefix(token: &str) -> Option<&'static str> {
    let lower = token.to_lowercase();
    for prefix in UNSAFE_ENV_PREFIXES.iter() {
        if lower.starts_with(prefix.to_lowercase().as_str()) {
            return Some(prefix);
        }
    }
    None
}

/// Check if s looks like a shell control operator.
fn is_shell_operator(s: &str) -> bool {
    matches!(s, "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" | ")")
}

/// Scan the command for unsafe environment variable prefixes.
/// Returns the unsafe variable assignment if found, empty string if all safe.
pub fn check_unsafe_env_prefixes(cmd: &str) -> String {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.is_empty() {
        return String::new();
    }
    for field in &fields {
        if is_shell_operator(field) {
            break;
        }
        if !field.contains('=') {
            break;
        }
        if is_unsafe_env_prefix(field).is_some() {
            return field.to_string();
        }
    }
    String::new()
}

// ===========================================================================
// Bash security patterns (upstream bashSecurity.ts 23-step validator chain)
// ===========================================================================

struct BashSecurityPattern {
    name: &'static str,
    re: &'static str,
    severity: &'static str, // "deny" or "ask"
}

static BASH_SECURITY_PATTERN_DEFS: &[BashSecurityPattern] = &[
    // --- DENY patterns ---
    BashSecurityPattern {
        name: "ANSI-C quoting obfuscation ($'...')",
        re: r#"\$'[^']*'"#,
        severity: "deny",
    },
    BashSecurityPattern {
        name: "IFS injection ($IFS)",
        re: r"(?i)\$IFS|\$\{[^}]*IFS[^}]*\}",
        severity: "deny",
    },
    BashSecurityPattern {
        name: "Unicode whitespace injection",
        re: r#"[\x{00a0}\x{1680}\x{2000}-\x{200b}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]"#,
        severity: "deny",
    },
    BashSecurityPattern {
        name: "Carriage return injection",
        re: r"\r",
        severity: "deny",
    },
    BashSecurityPattern {
        name: "Backslash-escaped shell operator",
        re: r#"\\[&;|]"#,
        severity: "deny",
    },
    BashSecurityPattern {
        name: "Zsh dangerous builtin (zmodload/emulate/sysopen)",
        re: r"(?i)\b(?:zmodload|emulate|sysopen|sysread|syswrite|sysseek|zpty|ztcp|zsocket|mapfile)\b",
        severity: "deny",
    },
    // --- ASK patterns ---
    BashSecurityPattern {
        name: "Shell metacharacters in quoted context",
        re: r#"["'][^"']*([&;|])[^"']*["']"#,
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Variable expansion before pipe/redirect",
        re: r"\$[A-Z_][A-Z_0-9]*\s*[|>]",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Mid-word hash comment",
        re: r"\w#\w",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Quote/comment boundary manipulation",
        re: r#"#\s*['\"]|['\"]\s*#"#,
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Dangerous shell executable prefix",
        re: r"(?i)(?:^|[\s;&|])\b(?:sh|bash|zsh|fish|csh|tcsh|ksh|dash)\b",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Dangerous command modifier prefix",
        re: r"(?i)(?:^|[\s;&|])\b(?:env|nice|stdbuf|nohup|sudo|doas|pkexec)\b\s",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Command substitution ($()/backtick/process substitution)",
        re: r#"\$\(|`|<\(|=\w+"#,
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Brace expansion",
        re: r"\{[^}]*\}",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Backslash-escaped whitespace",
        re: r#"\\[ \t]"#,
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Newline injection",
        re: r"(?m)^.*\n.*$",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Control character injection",
        re: r"[\x00-\x08\x0b\x0c\x0e-\x1f]",
        severity: "ask",
    },
    BashSecurityPattern {
        name: "Incomplete compound command",
        re: r"(?:\||\|\||&&|;)\s*$",
        severity: "ask",
    },
];

/// Compiled security patterns (lazy-initialized).
static COMPILED_BASH_SECURITY_PATTERNS: Lazy<Vec<(String, Regex, String)>> = Lazy::new(|| {
    BASH_SECURITY_PATTERN_DEFS
        .iter()
        .filter_map(|p| {
            Regex::new(p.re).ok().map(|re| {
                (
                    format!("Bash security: {}", p.name),
                    re,
                    p.severity.to_string(),
                )
            })
        })
        .collect()
});

/// Scan a command for known dangerous bash patterns.
/// Returns (deny_messages, ask_messages).
fn check_bash_security_patterns(cmd: &str) -> (Vec<String>, Vec<String>) {
    let mut deny_msgs = Vec::new();
    let mut ask_msgs = Vec::new();
    for (msg, re, severity) in COMPILED_BASH_SECURITY_PATTERNS.iter() {
        if re.is_match(cmd) {
            if severity == "deny" {
                deny_msgs.push(msg.clone());
            } else {
                ask_msgs.push(msg.clone());
            }
        }
    }
    (deny_msgs, ask_msgs)
}

// ===========================================================================
// jq security (upstream bashSecurity.ts #2, #3 validateJqCommand)
// ===========================================================================

static JQ_DENY_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\bjq\b.*system\s*\(").unwrap(),
        Regex::new(r"(?i)\bjq\b.*-[a-z]*f[a-z]*\b").unwrap(),
        Regex::new(r"(?i)\bjq\b.*--from-file\b").unwrap(),
        Regex::new(r"(?i)\bjq\b.*--rawfile\b").unwrap(),
        Regex::new(r"(?i)\bjq\b.*--slurpfile\b").unwrap(),
        Regex::new(r"(?i)\bjq\b.*-L\b").unwrap(),
        Regex::new(r"(?i)\bjq\b.*--library-directory\b").unwrap(),
    ]
});

static JQ_ASK_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\bjq\b.*env\[").unwrap(),
        Regex::new(r"(?i)\bjq\b.*\$ENV\[").unwrap(),
        Regex::new(r"(?i)\bjq\b.*input_filename").unwrap(),
    ]
});

fn check_jq_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("jq ") && !lower.ends_with("jq") {
        return String::new();
    }
    for re in JQ_DENY_PATTERNS.iter() {
        if re.is_match(&lower) {
            return "jq security: dangerous jq operation detected".to_string();
        }
    }
    for re in JQ_ASK_PATTERNS.iter() {
        if re.is_match(&lower) {
            return "jq security: jq accessing external data sources".to_string();
        }
    }
    String::new()
}

// ===========================================================================
// sed security (upstream sedValidation.ts + sedEditParser.ts)
// ===========================================================================

fn is_print_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    if !cmd.ends_with('p') {
        return false;
    }
    let prefix = &cmd[..cmd.len() - 1];
    if prefix.is_empty() {
        return true;
    }
    // Check for N or N,M pattern
    if let Some(comma_idx) = prefix.find(',') {
        let before = &prefix[..comma_idx];
        let after = &prefix[comma_idx + 1..];
        if before.is_empty() || after.is_empty() {
            return false;
        }
        return before.chars().all(|c| c.is_ascii_digit())
            && after.chars().all(|c| c.is_ascii_digit());
    }
    prefix.chars().all(|c| c.is_ascii_digit())
}

fn validate_sed_flags(cmd: &str, allowed: &HashSet<&str>) -> bool {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    for f in &fields {
        if !f.starts_with('-') || *f == "--" {
            continue;
        }
        if f.starts_with("--") {
            if !allowed.contains(f) {
                return false;
            }
            continue;
        }
        // Combined short flags like -nEr
        if f.len() > 2 {
            for c in f[1..].chars() {
                let single = format!("-{}", c);
                if !allowed.contains(single.as_str()) {
                    return false;
                }
            }
            continue;
        }
        // Single short flag
        if !allowed.contains(*f) {
            return false;
        }
    }
    true
}

fn has_sed_n_flag(cmd: &str) -> bool {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    for &f in &fields {
        if f == "-n" || f == "--quiet" || f == "--silent" {
            return true;
        }
        if f.starts_with('-') && !f.starts_with("--") && f.len() > 1 && f.contains('n') {
            return true;
        }
    }
    false
}

fn is_line_print_cmd(cmd: &str, expressions: &[&str]) -> bool {
    if expressions.is_empty() {
        return false;
    }
    let allowed: HashSet<&str> = [
        "-n", "--quiet", "--silent", "-E", "--regexp-extended", "-r", "-z", "--zero-terminated",
        "--posix",
    ]
    .into_iter()
    .collect();
    if !validate_sed_flags(cmd, &allowed) {
        return false;
    }
    if !has_sed_n_flag(cmd) {
        return false;
    }
    expressions.iter().all(|expr| {
        expr.split(';')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .all(|p| is_print_command(p))
    })
}

fn is_substitution_cmd(cmd: &str, expressions: &[&str], has_files: bool, allow_writes: bool) -> bool {
    if !allow_writes && has_files {
        return false;
    }
    let mut allowed: HashSet<&str> = [
        "-E", "--regexp-extended", "-r", "--posix",
    ]
    .into_iter()
    .collect();
    if allow_writes {
        allowed.insert("-i");
        allowed.insert("--in-place");
    }
    if !validate_sed_flags(cmd, &allowed) {
        return false;
    }
    if expressions.len() != 1 {
        return false;
    }
    let expr = expressions[0].trim();
    if expr.is_empty() || !expr.starts_with('s') {
        return false;
    }
    let rest = &expr[1..];
    if rest.is_empty() {
        return false;
    }
    let delim = rest.chars().next().unwrap();
    if delim == '\\' || delim == '\n' {
        return false;
    }
    // Count delimiters (skip escaped)
    let mut count = 0i32;
    let mut last_delim_idx = -1i32;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == delim {
            count += 1;
            last_delim_idx = i as i32;
        }
    }
    if count != 2 || last_delim_idx < 0 {
        return false;
    }
    // Extract and validate flags
    let rest_bytes = rest.as_bytes();
    let flag_start = (last_delim_idx as usize) + 1;
    if flag_start >= rest.len() {
        return true;
    }
    let expr_flags = &rest[flag_start..];
    let valid_flags: HashSet<char> = ['g', 'p', 'i', 'I', 'm', 'M'].into_iter().collect();
    let mut digit_seen = false;
    for c in expr_flags.chars() {
        if c >= '1' && c <= '9' {
            if digit_seen {
                return false;
            }
            digit_seen = true;
            continue;
        }
        if !valid_flags.contains(&c) {
            return false;
        }
    }
    true
}

fn has_file_args(cmd: &str) -> bool {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.is_empty() || fields[0].to_lowercase() != "sed" {
        return false;
    }
    let mut found_expr = false;
    for f in &fields[1..] {
        if *f == "-e" || *f == "--expression" {
            found_expr = true;
            continue;
        }
        if f.starts_with("-e=") || f.starts_with("--expression=") {
            found_expr = true;
            continue;
        }
        if f.starts_with('-') {
            continue;
        }
        if !found_expr {
            found_expr = true;
            continue;
        }
        return true;
    }
    false
}

fn extract_sed_expressions(cmd: &str) -> Vec<String> {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.is_empty() || fields[0].to_lowercase() != "sed" {
        return Vec::new();
    }
    // Reject dangerous flag combos
    for f in &fields {
        if f.starts_with("-e") && f.len() > 2 {
            for c in f[2..].chars() {
                if c == 'w' || c == 'W' || c == 'e' {
                    return Vec::new();
                }
            }
        }
        if *f == "-w" || *f == "-W" {
            return Vec::new();
        }
    }

    let mut expressions = Vec::new();
    let mut found_e_flag = false;
    let mut i = 1;
    while i < fields.len() {
        let f = fields[i];
        if f == "-e" || f == "--expression" {
            found_e_flag = true;
            i += 1;
            if i < fields.len() {
                expressions.push(fields[i].to_string());
            }
            i += 1;
            continue;
        }
        if f.starts_with("--expression=") {
            found_e_flag = true;
            expressions.push(f[13..].to_string());
            i += 1;
            continue;
        }
        if f.starts_with("-e=") {
            found_e_flag = true;
            expressions.push(f[3..].to_string());
            i += 1;
            continue;
        }
        if f.starts_with('-') {
            i += 1;
            continue;
        }
        if !found_e_flag {
            found_e_flag = true;
            expressions.push(f.to_string());
            i += 1;
            continue;
        }
        break;
    }
    expressions
}

fn contains_dangerous_operations(expression: &str) -> bool {
    let cmd = expression.trim();
    if cmd.is_empty() {
        return false;
    }
    // Reject non-ASCII
    if cmd.chars().any(|c| c > '\x7f') {
        return true;
    }
    // Reject braces
    if cmd.contains('{') || cmd.contains('}') {
        return true;
    }
    // Reject newlines
    if cmd.contains('\n') {
        return true;
    }
    // Reject comments (# not after s command)
    if let Some(idx) = cmd.find('#') {
        if !(idx > 0 && cmd.as_bytes()[idx - 1] == b's') {
            return true;
        }
    }
    // Reject negation
    if cmd.starts_with('!') || cmd.contains('!') {
        return true;
    }
    // Reject tilde
    if cmd.contains('~') {
        return true;
    }
    // Reject comma at start
    if cmd.starts_with(',') {
        return true;
    }
    // Reject backslash tricks
    if cmd.starts_with("s\\") {
        return true;
    }
    if cmd.contains("\\#") || cmd.contains("\\|") || cmd.contains("\\%") || cmd.contains("\\@") {
        return true;
    }
    // Reject w/W/e/E commands
    static SED_WRITE_EXEC_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"^(?:[wWeE]\s*\S+|\d+\s*[wWeE]|\$[ \t]+[wWeE]|/\w+/[IMim]*[ \t]+[wWeE]|\d+,\d+[ \t]*[wWeE]|/\w+/[IMim]*,/\w+/[IMim]*\s*[wWeE]|(?:^s.|^\d+\s*e|^\$\s*e|^/\w+/[IMim]*\s*e|^\d+,\d+\s*e|^\d+,\$\s*e))"
        ).unwrap()
    });
    if SED_WRITE_EXEC_RE.is_match(cmd) {
        return true;
    }
    // Reject y command
    if cmd.starts_with('y') && cmd.len() > 1 {
        return true;
    }
    // Reject substitution with w/e flag
    if cmd.starts_with('s') && cmd.len() > 1 {
        let rest = &cmd[1..];
        let delim = rest.chars().next().unwrap();
        let mut count = 0u32;
        let mut flags_start = None;
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == delim {
                count += 1;
                if count == 3 {
                    flags_start = Some(i + delim.len_utf8());
                    break;
                }
            }
        }
        if let Some(start) = flags_start {
            let flags = &rest[start..];
            if flags.chars().any(|c| c == 'w' || c == 'e' || c == 'W' || c == 'E') {
                return true;
            }
        }
    }
    static SED_WRITE_IN_CONTEXT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"/[^/]*\s+[wWeE]").unwrap());
    if SED_WRITE_IN_CONTEXT_RE.is_match(cmd) {
        return true;
    }
    false
}

fn check_sed_security(cmd: &str) -> String {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.is_empty() || fields[0].to_lowercase() != "sed" {
        return String::new();
    }

    let expressions = extract_sed_expressions(cmd);
    let has_files = has_file_args(cmd);
    let expr_refs: Vec<&str> = expressions.iter().map(|s| s.as_str()).collect();

    let is_pattern1 = is_line_print_cmd(cmd, &expr_refs);
    let mut is_pattern2 = is_substitution_cmd(cmd, &expr_refs, has_files, false);

    if is_pattern2 {
        for expr in &expressions {
            if expr.contains(';') {
                is_pattern2 = false;
                break;
            }
        }
    }

    if !is_pattern1 && !is_pattern2 {
        for expr in &expressions {
            if contains_dangerous_operations(expr) {
                return "sed security: dangerous operation in sed expression".to_string();
            }
        }
        if expressions.is_empty() {
            let lower = cmd.to_lowercase();
            if Regex::new(r"(?i)\bsed\b.*\bw\s+\S+")
                .unwrap()
                .is_match(&lower)
                || Regex::new(r"(?i)\bsed\b.*\bW\s+\S+")
                    .unwrap()
                    .is_match(&lower)
            {
                return "sed security: write to file (w/W command)".to_string();
            }
            if Regex::new(r"(?i)\bsed\b.*\br\s+\S+")
                .unwrap()
                .is_match(&lower)
            {
                return "sed security: read file (r command)".to_string();
            }
            if Regex::new(r"(?i)\bsed\b.*s/[^/]*/[^/]*/[a-zA-Z]*e")
                .unwrap()
                .is_match(&lower)
            {
                return "sed security: execute shell command (s///e flag)".to_string();
            }
        }
        return String::new();
    }

    for expr in &expressions {
        if contains_dangerous_operations(expr) {
            return "sed security: dangerous operation in sed expression".to_string();
        }
    }
    String::new()
}

// ===========================================================================
// xargs security (upstream readOnlyValidation.ts xargs config)
// ===========================================================================

static SAFE_XARGS_FLAGS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("-I", "{}");
    m.insert("-n", "number");
    m.insert("-P", "number");
    m.insert("-L", "number");
    m.insert("-s", "number");
    m.insert("-E", "EOF");
    m.insert("-0", "none");
    m.insert("-t", "none");
    m.insert("-r", "none");
    m.insert("-x", "none");
    m.insert("-d", "char");
    m
});

static XARGS_SAFE_TARGETS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    ["echo", "printf", "wc", "grep", "head", "tail"]
        .into_iter()
        .collect()
});

static XARGS_ARG_TAKING_FLAGS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    ["-I", "-n", "-P", "-L", "-s", "-E", "-d"]
        .into_iter()
        .collect()
});

fn check_xargs_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("xargs ") && !lower.ends_with("xargs") {
        return String::new();
    }

    let fields: Vec<&str> = cmd.split_whitespace().collect();
    let xargs_idx = fields
        .iter()
        .position(|f| f.to_lowercase() == "xargs");
    let xargs_idx = match xargs_idx {
        Some(i) => i,
        None => return String::new(),
    };

    let args = &fields[xargs_idx + 1..];

    // Find target command
    let mut target_idx: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if arg == "--" {
            target_idx = Some(i + 1);
            break;
        }
        if !arg.starts_with('-') {
            target_idx = Some(i);
            break;
        }
        if let Some(arg_type) = SAFE_XARGS_FLAGS.get(arg) {
            if *arg_type != "none" {
                i += 2;
                continue;
            }
        }
        if arg.starts_with('-') && arg.len() > 2 && !arg.starts_with("--") {
            i += 1;
            continue;
        }
        if arg.starts_with("--") && arg.contains('=') {
            i += 1;
            continue;
        }
        i += 1;
    }

    let xargs_flags = match target_idx {
        Some(idx) => &args[..idx],
        None => args,
    };

    // Step 1: Check for dangerous flags
    for arg in xargs_flags {
        if *arg == "-i" {
            return "xargs security: -i flag has GNU optional-arg parser differential — use -I {} instead".to_string();
        }
        if *arg == "-e" {
            return "xargs security: -e flag has GNU optional-arg parser differential — use -E EOF instead".to_string();
        }
        if arg.starts_with('-') && arg.len() > 2 && !arg.starts_with("--") {
            for c in arg[1..].chars() {
                if c == 'i' {
                    return "xargs security: bundled -i flag has GNU optional-arg parser differential".to_string();
                }
                if c == 'e' {
                    return "xargs security: bundled -e flag has GNU optional-arg parser differential".to_string();
                }
            }
        }
    }

    // Step 2: Reject bundled flags containing arg-taking flags
    for arg in xargs_flags {
        if arg.starts_with('-') && arg.len() > 2 && !arg.starts_with("--") && !arg.contains('=') {
            let mut has_arg_flag = false;
            let mut all_none = true;
            for c in arg[1..].chars() {
                let short = format!("-{}", c);
                if !SAFE_XARGS_FLAGS.contains_key(short.as_str()) {
                    return format!("xargs security: unrecognized flag {}", short);
                }
                if XARGS_ARG_TAKING_FLAGS.contains(short.as_str()) {
                    has_arg_flag = true;
                }
                if SAFE_XARGS_FLAGS.get(short.as_str()).map_or(true, |v| *v != "none") {
                    all_none = false;
                }
            }
            if has_arg_flag && !all_none {
                return "xargs security: bundled flags with arg-taking flags must be separated".to_string();
            }
        }
    }

    // Step 3: Validate all xargs flags are in safe allowlist
    for arg in xargs_flags {
        if !arg.starts_with('-') || *arg == "--" {
            continue;
        }
        if arg.starts_with('-') && arg.len() > 2 && !arg.starts_with("--") {
            continue; // bundled, already validated
        }
        if arg.starts_with("--") {
            let safe_long: HashSet<&str> = [
                "--null",
                "--delimiter",
                "--max-args",
                "--max-procs",
                "--max-lines",
                "--max-chars",
                "--eof",
                "--verbose",
                "--no-run-if-empty",
                "--exit",
            ]
            .into_iter()
            .collect();
            let flag_part = if let Some(eq_idx) = arg.find('=') {
                &arg[..eq_idx]
            } else {
                arg
            };
            if !safe_long.contains(flag_part) {
                return format!("xargs security: unrecognized long flag: {}", flag_part);
            }
            continue;
        }
        if !SAFE_XARGS_FLAGS.contains_key(*arg) {
            return format!("xargs security: unrecognized flag: {}", arg);
        }
    }

    // Step 4: Validate target command is in safe allowlist
    if let Some(idx) = target_idx {
        if idx < args.len() {
            let mut target_cmd = args[idx];
            if target_cmd.to_lowercase() == "--" && idx + 1 < args.len() {
                target_cmd = args[idx + 1];
            }
            if !XARGS_SAFE_TARGETS.contains(target_cmd.to_lowercase().as_str()) {
                return format!(
                    "xargs security: target command '{}' is not in safe allowlist",
                    target_cmd
                );
            }
        }
    }

    String::new()
}

// ===========================================================================
// fd/fdfind security (upstream EXTERNAL_READONLY_COMMANDS)
// ===========================================================================

static FD_DANGEROUS_FLAGS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    ["-x", "--exec", "-X", "--exec-batch", "-l"].into_iter().collect()
});

fn check_fd_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("fd ") && !lower.contains("fdfind ") {
        return String::new();
    }
    for f in cmd.split_whitespace() {
        if FD_DANGEROUS_FLAGS.contains(f.to_lowercase().as_str()) {
            return "fd security: --exec/-x flag executes arbitrary commands".to_string();
        }
    }
    String::new()
}

// ===========================================================================
// ripgrep security (upstream RIPGREP_READ_ONLY_COMMANDS)
// ===========================================================================

static RG_DANGEROUS_FLAGS: Lazy<HashSet<&'static str>> =
    Lazy::new(|| ["--pre", "--pre-glob", "--search-zip"].into_iter().collect());

fn check_rg_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("rg ") && !lower.contains("ripgrep ") {
        return String::new();
    }
    for f in cmd.split_whitespace() {
        if RG_DANGEROUS_FLAGS.contains(f.to_lowercase().as_str()) {
            return "ripgrep security: --pre/--search-zip flag has security implications".to_string();
        }
    }
    String::new()
}

// ===========================================================================
// gh CLI security (upstream GH_READ_ONLY_COMMANDS)
// ===========================================================================

static GH_READ_ONLY_SUBS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "issue list",
        "issue view",
        "pr list",
        "pr view",
        "pr diff",
        "pr commits",
        "pr checks",
        "repo view",
        "repo list",
        "run list",
        "run view",
        "status",
    ]
    .into_iter()
    .collect()
});

static GH_DANGEROUS_SUBS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    ["auth", "secret", "variable", "ssh-key"]
        .into_iter()
        .collect()
});

static GH_REPO_EXFIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)--repo=\S+\.\S+/").unwrap());
static GH_URL_EXFIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:https?://|git@)").unwrap());
static GH_HOST_EXFIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\bgh\s+.*[a-z]+\.[a-z]+/[a-z]+/[a-z]+").unwrap());

fn check_gh_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("gh ") && !lower.ends_with("gh") {
        return String::new();
    }
    if GH_REPO_EXFIL.is_match(&lower) {
        return "gh security: --repo= form can redirect requests to external host".to_string();
    }
    if GH_URL_EXFIL.is_match(&lower) {
        return "gh security: URL form can redirect to external host".to_string();
    }
    if GH_HOST_EXFIL.is_match(&lower) {
        return "gh security: HOST/OWNER/REPO format can exfiltrate data".to_string();
    }
    let fields: Vec<&str> = lower.split_whitespace().collect();
    for (i, f) in fields.iter().enumerate() {
        if *f == "gh" && i + 1 < fields.len() {
            let sub: String = fields[i + 1..].join(" ");
            for ds in GH_DANGEROUS_SUBS.iter() {
                if sub.starts_with(ds) {
                    return format!("gh security: gh {} requires approval", ds);
                }
            }
            if sub.starts_with("api") && !sub.contains("--method get") {
                return "gh security: gh api requires --method GET for read-only".to_string();
            }
            if sub.starts_with("gist") {
                for dangerous in &["gist create", "gist edit", "gist delete"] {
                    if sub.starts_with(dangerous) {
                        return format!("gh security: gh {} requires approval", dangerous);
                    }
                }
            }
            break;
        }
    }
    String::new()
}

// ===========================================================================
// git command callbacks (upstream additionalCommandIsDangerousCallback)
// ===========================================================================

fn check_git_branch_security(fields: &[&str]) -> String {
    let safe_flags: HashSet<&str> = [
        "-a", "-r", "-v", "-vv", "--list", "--merged", "--no-merged",
        "--contains", "--no-contains", "--sort", "--format", "--points-at",
        "--no-color", "--abbrev", "--verbose", "--quiet", "--all", "--remotes",
        "--show-current",
    ]
    .into_iter()
    .collect();

    for f in fields {
        if !f.starts_with('-') {
            return "git security: positional args to 'git branch' can create/modify branches".to_string();
        }
        let lower = f.to_lowercase();
        if !safe_flags.contains(lower.as_str()) && !f.contains('=') {
            return format!("git security: unrecognized flag to 'git branch': {}", f);
        }
    }
    String::new()
}

fn check_git_tag_security(fields: &[&str]) -> String {
    let safe_flags: HashSet<&str> = [
        "-l", "--list", "-n", "--contains", "--no-contains",
        "--merged", "--no-merged", "--sort", "--format", "--no-color",
    ]
    .into_iter()
    .collect();

    for f in fields {
        if !f.starts_with('-') {
            return "git security: positional args to 'git tag' can create/delete tags".to_string();
        }
        let lower = f.to_lowercase();
        if !safe_flags.contains(lower.as_str()) && !f.contains('=') {
            return format!("git security: unrecognized flag to 'git tag': {}", f);
        }
    }
    String::new()
}

fn check_git_reflog_security(fields: &[&str]) -> String {
    if !fields.is_empty() {
        let sub = fields[0].to_lowercase();
        if sub == "expire" || sub == "delete" {
            return "git security: 'git reflog expire/delete' destroys reflog history".to_string();
        }
    }
    String::new()
}

fn check_git_remote_security(fields: &[&str]) -> String {
    if fields.is_empty() {
        return "git security: 'git remote' without -v can modify remotes".to_string();
    }
    for f in fields {
        let lower = f.to_lowercase();
        if lower != "-v" && lower != "--verbose" {
            return "git security: 'git remote' with non -v flags can modify remotes".to_string();
        }
    }
    String::new()
}

fn check_git_security(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("git ") && !lower.ends_with("git") {
        return String::new();
    }
    let fields: Vec<&str> = lower.split_whitespace().collect();
    for (i, f) in fields.iter().enumerate() {
        if *f == "git" && i + 1 < fields.len() {
            let global_flags_with_args: HashSet<&str> = [
                "-c", "-C", "--config-env", "--exec-path",
                "--git-dir", "--work-tree", "--namespace", "--super-prefix",
            ]
            .into_iter()
            .collect();
            let mut sub_idx = i + 1;
            while sub_idx < fields.len() {
                let fld = fields[sub_idx];
                if !fld.starts_with('-') {
                    break;
                }
                if fld.contains('=') {
                    sub_idx += 1;
                    continue;
                }
                if global_flags_with_args.contains(fld) {
                    sub_idx += 2;
                    continue;
                }
                sub_idx += 1;
            }
            if sub_idx >= fields.len() {
                return String::new();
            }
            let sub = fields[sub_idx];
            let remaining = &fields[sub_idx + 1..];
            match sub {
                "branch" => {
                    let msg = check_git_branch_security(remaining);
                    if !msg.is_empty() { return msg; }
                }
                "tag" => {
                    let msg = check_git_tag_security(remaining);
                    if !msg.is_empty() { return msg; }
                }
                "reflog" => {
                    let msg = check_git_reflog_security(remaining);
                    if !msg.is_empty() { return msg; }
                }
                "remote" => {
                    let msg = check_git_remote_security(remaining);
                    if !msg.is_empty() { return msg; }
                }
                _ => {}
            }
            break;
        }
    }
    String::new()
}

// ===========================================================================
// Docker security (upstream DOCKER_READ_ONLY_COMMANDS)
// ===========================================================================

static DOCKER_READ_ONLY_SUBS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "logs", "inspect", "ps", "images", "info", "version",
        "stats", "events", "history", "top", "port", "diff",
    ]
    .into_iter()
    .collect()
});

static DOCKER_DANGEROUS_SUBS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "rm", "rmi", "kill", "stop", "pause", "unpause", "restart",
        "run", "create", "start", "exec", "cp", "commit", "build",
        "load", "import", "push", "pull", "tag", "rename", "update",
        "network create", "network connect", "network disconnect", "network rm",
        "volume create", "volume rm",
    ]
    .into_iter()
    .collect()
});

fn check_docker_security(cmd: &str) -> Option<PermissionResult> {
    let lower = cmd.to_lowercase();
    if !lower.contains("docker ") && !lower.ends_with("docker") {
        return None;
    }
    let fields: Vec<&str> = lower.split_whitespace().collect();
    for (i, f) in fields.iter().enumerate() {
        if *f == "docker" && i + 1 < fields.len() {
            let sub: String = fields[i + 1..].join(" ");
            for rs in DOCKER_READ_ONLY_SUBS.iter() {
                if sub.starts_with(rs) {
                    return Some(PermissionResult::Allow);
                }
            }
            for ds in DOCKER_DANGEROUS_SUBS.iter() {
                if sub.starts_with(ds) {
                    return Some(PermissionResult::Ask(format!(
                        "docker: write operation '{}' requires approval",
                        ds
                    )));
                }
            }
            if sub.contains("prune") {
                return Some(PermissionResult::Deny(
                    "docker: prune operations are blocked".to_string(),
                ));
            }
            return Some(PermissionResult::Ask(
                "docker: unrecognized subcommand requires approval".to_string(),
            ));
        }
    }
    None
}

// ===========================================================================
// cd compound attack detection (upstream bashPermissions.ts lines 2182-2225)
// ===========================================================================

fn filepath_base(path: &str) -> String {
    let path = if let Some(idx) = path.rfind('/') {
        &path[idx + 1..]
    } else {
        path
    };
    if let Some(idx) = path.rfind('\\') {
        path[idx + 1..].to_string()
    } else {
        path.to_string()
    }
}

pub fn check_cd_compound_attacks(cmd: &str, subcmds: &[&str]) -> String {
    let mut cd_count = 0u32;
    let mut has_git = false;
    for sub in subcmds {
        let sub = sub.trim();
        if sub.is_empty() {
            continue;
        }
        let fields: Vec<&str> = sub.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let base = filepath_base(fields[0]);
        if base == "cd" || base == "pushd" || base == "popd" {
            cd_count += 1;
        }
        if base == "git" {
            has_git = true;
        }
    }
    if cd_count > 1 {
        return "Multiple cd commands in compound command".to_string();
    }
    if cd_count > 0 && has_git {
        return "cd + git compound command (bare repository attack vector)".to_string();
    }
    String::new()
}

// ===========================================================================
// Read-only command validation helper
// ===========================================================================

fn contains_command_substitution(inner: &str) -> bool {
    inner.contains("$(") || inner.contains('`')
}

pub fn is_read_only_command_with_flags(cmd: &str, inner: &str) -> bool {
    if contains_command_substitution(inner) {
        return false;
    }
    let lower = inner.to_lowercase();
    let fields: Vec<&str> = lower.split_whitespace().collect();
    if fields.is_empty() {
        return false;
    }
    match fields[0] {
        "xargs" => check_xargs_security(inner).is_empty(),
        "fd" | "fdfind" => check_fd_security(inner).is_empty(),
        "rg" | "ripgrep" => check_rg_security(inner).is_empty(),
        "jq" => check_jq_security(inner).is_empty(),
        "sed" => check_sed_security(inner).is_empty(),
        _ => false,
    }
}

// ===========================================================================
// QUOTED_NEWLINE security check (upstream bashSecurity.ts #23)
// ===========================================================================

pub fn validate_quoted_newline(cmd: &str) -> String {
    if !cmd.contains('\n') || !cmd.contains('#') {
        return String::new();
    }

    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let bytes = cmd.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\\' if !in_single_quote => {
                i += 2;
                continue;
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                i += 1;
                continue;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                i += 1;
                continue;
            }
            '\n' if in_single_quote || in_double_quote => {
                let mut next_line_start = i + 1;
                while next_line_start < bytes.len()
                    && (bytes[next_line_start] == b' ' || bytes[next_line_start] == b'\t')
                {
                    next_line_start += 1;
                }
                if next_line_start < bytes.len() && bytes[next_line_start] == b'#' {
                    return "Bash security: quoted newline followed by #-prefixed line can hide arguments from line-based permission checks".to_string();
                }
            }
            _ => {}
        }
        i += 1;
    }
    String::new()
}

// ===========================================================================
// PROC_ENVIRON_ACCESS security check (upstream bashSecurity.ts #13)
// ===========================================================================

static PROC_ENVIRON_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/proc/[^/]+/environ").unwrap());

pub fn validate_proc_environ_access(cmd: &str) -> String {
    if PROC_ENVIRON_RE.is_match(cmd) {
        return "Bash security: command accesses /proc/*/environ which could expose sensitive environment variables".to_string();
    }
    String::new()
}

// ===========================================================================
// GIT_COMMIT_SUBSTITUTION security check (upstream bashSecurity.ts #12)
// ===========================================================================

pub fn validate_git_commit(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if !lower.contains("git") || !lower.contains("commit") || !lower.contains("-m") {
        return String::new();
    }
    if cmd.contains('\\') {
        return String::new();
    }

    // Extract the commit message content
    let mut quote_char: Option<char> = None;
    let mut msg_content = String::new();
    let mut remainder = String::new();
    let mut found_msg = false;

    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if !found_msg {
            if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'm' {
                let after_m = i + 2;
                if after_m < bytes.len() && bytes[after_m] == b'=' {
                    let after_eq = after_m + 1;
                    if after_eq < bytes.len() && (bytes[after_eq] == b'"' || bytes[after_eq] == b'\'')
                    {
                        quote_char = Some(bytes[after_eq] as char);
                        i = after_eq + 1;
                        found_msg = true;
                        continue;
                    }
                } else if after_m < bytes.len() && bytes[after_m] == b' ' {
                    let mut j = after_m + 1;
                    while j < bytes.len() && bytes[j] == b' ' {
                        j += 1;
                    }
                    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        quote_char = Some(bytes[j] as char);
                        i = j + 1;
                        found_msg = true;
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }

        if let Some(qc) = quote_char {
            if c == qc {
                if i + 1 < bytes.len() {
                    remainder = cmd[i + 1..].to_string();
                }
                break;
            }
        }
        msg_content.push(c);
        i += 1;
    }

    if !found_msg {
        return String::new();
    }

    if msg_content.contains("$(") || msg_content.contains('`') || msg_content.contains("${") {
        return "Bash security: command substitution in git commit message requires approval".to_string();
    }

    let rem = remainder.trim();
    if !rem.is_empty() {
        for c in rem.chars() {
            if c == ';' || c == '|' || c == '&' || c == '(' || c == ')' {
                return "Bash security: shell operator chaining after git commit -m requires approval".to_string();
            }
        }
        if rem.contains("$(") || rem.contains("${") {
            return "Bash security: command substitution after git commit -m requires approval".to_string();
        }
    }

    String::new()
}

// ===========================================================================
// CheckBashPermission — main entry point
// ===========================================================================

/// Evaluates a bash/shell command against security patterns, unsafe env vars,
/// and per-command validation.
///
/// Returns:
/// - `PermissionResult::Deny` for hard-blocked patterns
/// - `PermissionResult::Ask` for suspicious but not outright-blocked patterns
/// - `PermissionResult::Allow` for verified read-only commands
/// - `PermissionResult::Passthrough` if the command appears safe (fall through)
pub fn check_bash_permission(cmd: &str) -> PermissionResult {
    let lower = cmd.to_lowercase();
    let lower = lower.trim();
    if lower.is_empty() {
        return PermissionResult::Passthrough;
    }

    // Step 1: Bash security patterns
    let (deny_msgs, ask_msgs) = check_bash_security_patterns(lower);
    if !deny_msgs.is_empty() {
        return PermissionResult::Deny(deny_msgs.join("; "));
    }
    if !ask_msgs.is_empty() {
        return PermissionResult::Ask(ask_msgs.join("; "));
    }

    // Step 2: Unsafe env var prefixes
    let unsafe_env = check_unsafe_env_prefixes(lower);
    if !unsafe_env.is_empty() {
        return PermissionResult::Ask(format!("Unsafe environment variable: {}", unsafe_env));
    }

    // Step 2b: Quoted newline
    let msg = validate_quoted_newline(cmd);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }

    // Step 2c: /proc/*/environ access
    let msg = validate_proc_environ_access(cmd);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }

    // Step 2d: Git commit substitution
    let msg = validate_git_commit(cmd);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }

    // Step 3: Read-only command allowlist
    if crate::tools::bash_readonly::check_bash_read_only_command(cmd) {
        return PermissionResult::Allow;
    }

    // Step 4: Per-command security validation
    // jq
    let msg = check_jq_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // sed
    let msg = check_sed_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // xargs
    let msg = check_xargs_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // fd
    let msg = check_fd_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // rg
    let msg = check_rg_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // gh
    let msg = check_gh_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // git: check read-only allowlist first
    if crate::tools::exec_git_readonly::bash_ro_is_git_read_only_command(cmd) {
        return PermissionResult::Allow;
    }
    // git: check subcommand-specific callbacks
    let msg = check_git_security(lower);
    if !msg.is_empty() {
        return PermissionResult::Ask(msg);
    }
    // git: unknown subcommand → ask
    if lower.starts_with("git ") {
        return PermissionResult::Ask(
            "git: unrecognized subcommand or flags requires approval".to_string(),
        );
    }
    // docker
    if let Some(result) = check_docker_security(lower) {
        return result;
    }

    PermissionResult::Passthrough
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_unsafe_env_prefixes() {
        assert!(check_unsafe_env_prefixes("echo hello").is_empty());
        assert!(!check_unsafe_env_prefixes("PATH=/evil cmd").is_empty());
        assert!(check_unsafe_env_prefixes("RUST_LOG=debug cargo build").is_empty());
    }

    #[test]
    fn test_bash_security_patterns_deny() {
        let (deny, _) = check_bash_security_patterns("$'hello\\nworld'");
        assert!(!deny.is_empty());

        let (deny, _) = check_bash_security_patterns("echo $IFS");
        assert!(!deny.is_empty());
    }

    #[test]
    fn test_bash_security_patterns_ask() {
        let (_, ask) = check_bash_security_patterns("echo `whoami`");
        assert!(!ask.is_empty());
    }

    #[test]
    fn test_jq_security() {
        assert!(!check_jq_security("jq 'system(\"rm -rf /\")' file.json").is_empty());
        assert!(check_jq_security("jq '.name' file.json").is_empty());
    }

    #[test]
    fn test_xargs_security_dangerous() {
        assert!(!check_xargs_security("find . -name '*.tmp' | xargs -i rm {}").is_empty());
        assert!(!check_xargs_security("find . | xargs -e cmd").is_empty());
    }

    #[test]
    fn test_fd_security() {
        assert!(!check_fd_security("fd -x rm").is_empty());
        assert!(check_fd_security("fd -e rs").is_empty());
    }

    #[test]
    fn test_rg_security() {
        assert!(!check_rg_security("rg --pre=bash pattern").is_empty());
        assert!(check_rg_security("rg --json pattern").is_empty());
    }

    #[test]
    fn test_gh_security() {
        assert!(!check_gh_security("gh auth login").is_empty());
        assert!(!check_gh_security("gh secret set MY_SECRET").is_empty());
        assert!(check_gh_security("gh issue list").is_empty());
    }

    #[test]
    fn test_git_security() {
        assert!(!check_git_security("git branch new-branch").is_empty());
        assert!(check_git_security("git branch -a").is_empty());
    }

    #[test]
    fn test_docker_security() {
        assert_eq!(check_docker_security("docker ps"), Some(PermissionResult::Allow));
        assert!(matches!(check_docker_security("docker rm container"), Some(PermissionResult::Ask(_))));
        assert!(matches!(check_docker_security("docker system prune"), Some(PermissionResult::Deny(_))));
        assert_eq!(check_docker_security("echo hello"), None);
    }

    #[test]
    fn test_validate_quoted_newline() {
        let cmd = "echo \"hello\n#hidden arg\"";
        assert!(!validate_quoted_newline(cmd).is_empty());
    }

    #[test]
    fn test_validate_proc_environ() {
        assert!(!validate_proc_environ_access("cat /proc/123/environ").is_empty());
        assert!(validate_proc_environ_access("cat /proc/123/status").is_empty());
    }

    #[test]
    fn test_validate_git_commit() {
        assert!(!validate_git_commit("git commit -m \"$(whoami)\"").is_empty());
        assert!(validate_git_commit("git commit -m \"normal message\"").is_empty());
    }

    #[test]
    fn test_check_bash_permission_passthrough() {
        assert_eq!(check_bash_permission(""), PermissionResult::Passthrough);
    }
}