//! Bash read-only command validation.
//! Ported from upstream exec_bash_readonly.go (794 lines).
//!
//! Per-command flag allowlists with typed validation (none/number/string/char/braces/EOF).
//! Used by CheckBashPermission to auto-allow known safe read-only commands.

use once_cell::sync::Lazy;
use std::collections::HashMap;

// ===========================================================================
// Flag argument type constants
// ===========================================================================

const FLAG_NONE: &str = "none";
const FLAG_NUMBER: &str = "number";
const FLAG_STRING: &str = "string";
const FLAG_CHAR: &str = "char";
const FLAG_BRACE: &str = "{}";
const FLAG_EOF: &str = "EOF";

// ===========================================================================
// Bash read-only command flag configs
// ===========================================================================

struct BashROFlagConfig {
    safe_flags: HashMap<&'static str, &'static str>,
    dangerous_flags: HashMap<&'static str, bool>,
    respects_double_dash: bool,
}

static BASH_READ_ONLY_COMMANDS: Lazy<HashMap<&'static str, BashROFlagConfig>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // --- xargs ---
    m.insert("xargs", BashROFlagConfig {
        safe_flags: [
            ("-I", FLAG_BRACE), ("-n", FLAG_NUMBER), ("-P", FLAG_NUMBER),
            ("-L", FLAG_NUMBER), ("-s", FLAG_NUMBER), ("-E", FLAG_EOF),
            ("-0", FLAG_NONE), ("-t", FLAG_NONE), ("-r", FLAG_NONE),
            ("-x", FLAG_NONE), ("-d", FLAG_CHAR),
        ].into_iter().collect(),
        dangerous_flags: [("-i", true), ("-e", true)].into_iter().collect(),
        respects_double_dash: true,
    });

    // --- file ---
    m.insert("file", BashROFlagConfig {
        safe_flags: [
            ("--brief", FLAG_NONE), ("-b", FLAG_NONE),
            ("--mime", FLAG_NONE), ("-i", FLAG_NONE),
            ("--mime-type", FLAG_NONE), ("--mime-encoding", FLAG_NONE),
            ("--apple", FLAG_NONE), ("--check-encoding", FLAG_NONE),
            ("-c", FLAG_NONE),
            ("--exclude", FLAG_STRING), ("--exclude-quiet", FLAG_STRING),
            ("--print0", FLAG_NONE), ("-0", FLAG_NONE),
            ("-f", FLAG_STRING), ("-F", FLAG_STRING),
            ("--separator", FLAG_STRING),
            ("--help", FLAG_NONE), ("--version", FLAG_NONE), ("-v", FLAG_NONE),
            ("--no-dereference", FLAG_NONE), ("-h", FLAG_NONE),
            ("--dereference", FLAG_NONE), ("-L", FLAG_NONE),
            ("--magic-file", FLAG_STRING), ("-m", FLAG_STRING),
            ("--keep-going", FLAG_NONE), ("-k", FLAG_NONE),
            ("--list", FLAG_NONE), ("-l", FLAG_NONE),
            ("--no-buffer", FLAG_NONE), ("-n", FLAG_NONE),
            ("--preserve-date", FLAG_NONE), ("-p", FLAG_NONE),
            ("--raw", FLAG_NONE), ("-r", FLAG_NONE),
            ("-s", FLAG_NONE), ("--special-files", FLAG_NONE),
            ("--uncompress", FLAG_NONE), ("-z", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- sort ---
    m.insert("sort", BashROFlagConfig {
        safe_flags: [
            ("--ignore-leading-blanks", FLAG_NONE), ("-b", FLAG_NONE),
            ("--dictionary-order", FLAG_NONE), ("-d", FLAG_NONE),
            ("--ignore-case", FLAG_NONE), ("-f", FLAG_NONE),
            ("--general-numeric-sort", FLAG_NONE), ("-g", FLAG_NONE),
            ("--human-numeric-sort", FLAG_NONE), ("-h", FLAG_NONE),
            ("--ignore-nonprinting", FLAG_NONE), ("-i", FLAG_NONE),
            ("--month-sort", FLAG_NONE), ("-M", FLAG_NONE),
            ("--numeric-sort", FLAG_NONE), ("-n", FLAG_NONE),
            ("--random-sort", FLAG_NONE), ("-R", FLAG_NONE),
            ("--reverse", FLAG_NONE), ("-r", FLAG_NONE),
            ("--sort", FLAG_STRING),
            ("--stable", FLAG_NONE), ("-s", FLAG_NONE),
            ("--unique", FLAG_NONE), ("-u", FLAG_NONE),
            ("--version-sort", FLAG_NONE), ("-V", FLAG_NONE),
            ("--zero-terminated", FLAG_NONE), ("-z", FLAG_NONE),
            ("--key", FLAG_STRING), ("-k", FLAG_STRING),
            ("--field-separator", FLAG_STRING), ("-t", FLAG_STRING),
            ("--check", FLAG_NONE), ("-c", FLAG_NONE),
            ("--check-char-order", FLAG_NONE), ("-C", FLAG_NONE),
            ("--merge", FLAG_NONE), ("-m", FLAG_NONE),
            ("--buffer-size", FLAG_STRING), ("-S", FLAG_STRING),
            ("--parallel", FLAG_NUMBER), ("--batch-size", FLAG_NUMBER),
            ("--help", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- man ---
    m.insert("man", BashROFlagConfig {
        safe_flags: [
            ("-a", FLAG_NONE), ("--all", FLAG_NONE),
            ("-d", FLAG_NONE),
            ("-f", FLAG_NONE), ("--whatis", FLAG_NONE),
            ("-h", FLAG_NONE),
            ("-k", FLAG_NONE), ("--apropos", FLAG_NONE),
            ("-l", FLAG_STRING), ("-w", FLAG_NONE),
            ("-S", FLAG_STRING), ("-s", FLAG_STRING),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- help ---
    m.insert("help", BashROFlagConfig {
        safe_flags: [
            ("-d", FLAG_NONE), ("-m", FLAG_NONE), ("-s", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- netstat ---
    m.insert("netstat", BashROFlagConfig {
        safe_flags: [
            ("-a", FLAG_NONE), ("-L", FLAG_NONE),
            ("-l", FLAG_NONE), ("-n", FLAG_NONE),
            ("-t", FLAG_NONE), ("-p", FLAG_NONE),
            ("-i", FLAG_NONE), ("-I", FLAG_STRING),
            ("-s", FLAG_NONE), ("-r", FLAG_NONE),
            ("-m", FLAG_NONE), ("-v", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- ps ---
    m.insert("ps", BashROFlagConfig {
        safe_flags: [
            ("-e", FLAG_NONE), ("-A", FLAG_NONE),
            ("-a", FLAG_NONE), ("-d", FLAG_NONE),
            ("-N", FLAG_NONE), ("--deselect", FLAG_NONE),
            ("-f", FLAG_NONE), ("-F", FLAG_NONE),
            ("-l", FLAG_NONE), ("-j", FLAG_NONE),
            ("-y", FLAG_NONE), ("-w", FLAG_NONE),
            ("-ww", FLAG_NONE),
            ("--width", FLAG_NUMBER),
            ("-c", FLAG_NONE),
            ("-H", FLAG_NONE), ("--forest", FLAG_NONE),
            ("--headers", FLAG_NONE), ("--no-headers", FLAG_NONE),
            ("-n", FLAG_STRING),
            ("--sort", FLAG_STRING),
            ("-o", FLAG_STRING), ("--format", FLAG_STRING),
            ("-L", FLAG_NONE), ("-T", FLAG_NONE), ("-m", FLAG_NONE),
            ("-C", FLAG_STRING), ("-G", FLAG_STRING), ("-g", FLAG_STRING),
            ("-p", FLAG_STRING), ("--pid", FLAG_STRING),
            ("-q", FLAG_STRING), ("--quick-pid", FLAG_STRING),
            ("-s", FLAG_STRING), ("--sid", FLAG_STRING),
            ("-t", FLAG_STRING), ("--tty", FLAG_STRING),
            ("-U", FLAG_STRING), ("-u", FLAG_STRING),
            ("--user", FLAG_STRING),
            ("--help", FLAG_NONE), ("--info", FLAG_NONE),
            ("-V", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- base64 ---
    m.insert("base64", BashROFlagConfig {
        safe_flags: [
            ("-d", FLAG_NONE), ("-D", FLAG_NONE), ("--decode", FLAG_NONE),
            ("-b", FLAG_NUMBER), ("--break", FLAG_NUMBER),
            ("-w", FLAG_NUMBER), ("--wrap", FLAG_NUMBER),
            ("-i", FLAG_STRING), ("--input", FLAG_STRING),
            ("--ignore-garbage", FLAG_NONE),
            ("-h", FLAG_NONE), ("--help", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: false, // macOS base64 doesn't respect POSIX --
    });

    // --- grep ---
    m.insert("grep", BashROFlagConfig {
        safe_flags: [
            ("-e", FLAG_STRING), ("--regexp", FLAG_STRING),
            ("-f", FLAG_STRING), ("--file", FLAG_STRING),
            ("-F", FLAG_NONE), ("--fixed-strings", FLAG_NONE),
            ("-G", FLAG_NONE), ("--basic-regexp", FLAG_NONE),
            ("-E", FLAG_NONE), ("--extended-regexp", FLAG_NONE),
            ("-P", FLAG_NONE), ("--perl-regexp", FLAG_NONE),
            ("-i", FLAG_NONE), ("--ignore-case", FLAG_NONE),
            ("--no-ignore-case", FLAG_NONE),
            ("-v", FLAG_NONE), ("--invert-match", FLAG_NONE),
            ("-w", FLAG_NONE), ("--word-regexp", FLAG_NONE),
            ("-x", FLAG_NONE), ("--line-regexp", FLAG_NONE),
            ("-c", FLAG_NONE), ("--count", FLAG_NONE),
            ("--color", FLAG_STRING), ("--colour", FLAG_STRING),
            ("-L", FLAG_NONE), ("--files-without-match", FLAG_NONE),
            ("-l", FLAG_NONE), ("--files-with-matches", FLAG_NONE),
            ("-m", FLAG_NUMBER), ("--max-count", FLAG_NUMBER),
            ("-o", FLAG_NONE), ("--only-matching", FLAG_NONE),
            ("-q", FLAG_NONE), ("--quiet", FLAG_NONE), ("--silent", FLAG_NONE),
            ("-s", FLAG_NONE), ("--no-messages", FLAG_NONE),
            ("-b", FLAG_NONE), ("--byte-offset", FLAG_NONE),
            ("-H", FLAG_NONE), ("--with-filename", FLAG_NONE),
            ("-h", FLAG_NONE), ("--no-filename", FLAG_NONE),
            ("--label", FLAG_STRING),
            ("-n", FLAG_NONE), ("--line-number", FLAG_NONE),
            ("-T", FLAG_NONE), ("--initial-tab", FLAG_NONE),
            ("-u", FLAG_NONE), ("--unix-byte-offsets", FLAG_NONE),
            ("-Z", FLAG_NONE), ("--null", FLAG_NONE),
            ("-z", FLAG_NONE), ("--null-data", FLAG_NONE),
            ("-A", FLAG_NUMBER), ("--after-context", FLAG_NUMBER),
            ("-B", FLAG_NUMBER), ("--before-context", FLAG_NUMBER),
            ("-C", FLAG_NUMBER), ("--context", FLAG_NUMBER),
            ("--group-separator", FLAG_STRING),
            ("--no-group-separator", FLAG_NONE),
            ("-a", FLAG_NONE), ("--text", FLAG_NONE),
            ("--binary-files", FLAG_STRING),
            ("-D", FLAG_STRING), ("--devices", FLAG_STRING),
            ("-d", FLAG_STRING), ("--directories", FLAG_STRING),
            ("--exclude", FLAG_STRING), ("--exclude-from", FLAG_STRING),
            ("--exclude-dir", FLAG_STRING),
            ("--include", FLAG_STRING),
            ("-r", FLAG_NONE), ("--recursive", FLAG_NONE),
            ("-R", FLAG_NONE), ("--dereference-recursive", FLAG_NONE),
            ("--line-buffered", FLAG_NONE),
            ("-U", FLAG_NONE), ("--binary", FLAG_NONE),
            ("--help", FLAG_NONE), ("-V", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- rg (ripgrep) ---
    m.insert("rg", BashROFlagConfig {
        safe_flags: [
            ("-e", FLAG_STRING), ("--regexp", FLAG_STRING),
            ("-f", FLAG_STRING),
            ("-i", FLAG_NONE), ("--ignore-case", FLAG_NONE),
            ("-S", FLAG_NONE), ("--smart-case", FLAG_NONE),
            ("-F", FLAG_NONE), ("--fixed-strings", FLAG_NONE),
            ("-w", FLAG_NONE), ("--word-regexp", FLAG_NONE),
            ("-v", FLAG_NONE), ("--invert-match", FLAG_NONE),
            ("-c", FLAG_NONE), ("--count", FLAG_NONE),
            ("-l", FLAG_NONE), ("--files-with-matches", FLAG_NONE),
            ("--files-without-match", FLAG_NONE),
            ("-n", FLAG_NONE), ("--line-number", FLAG_NONE),
            ("-o", FLAG_NONE), ("--only-matching", FLAG_NONE),
            ("-A", FLAG_NUMBER), ("--after-context", FLAG_NUMBER),
            ("-B", FLAG_NUMBER), ("--before-context", FLAG_NUMBER),
            ("-C", FLAG_NUMBER), ("--context", FLAG_NUMBER),
            ("-H", FLAG_NONE), ("-h", FLAG_NONE),
            ("--heading", FLAG_NONE), ("--no-heading", FLAG_NONE),
            ("-q", FLAG_NONE), ("--quiet", FLAG_NONE),
            ("--column", FLAG_NONE),
            ("-g", FLAG_STRING), ("--glob", FLAG_STRING),
            ("-t", FLAG_STRING), ("--type", FLAG_STRING),
            ("-T", FLAG_STRING), ("--type-not", FLAG_STRING),
            ("--type-list", FLAG_NONE),
            ("--hidden", FLAG_NONE),
            ("--no-ignore", FLAG_NONE),
            ("-u", FLAG_NONE),
            ("-m", FLAG_NUMBER), ("--max-count", FLAG_NUMBER),
            ("-d", FLAG_NUMBER), ("--max-depth", FLAG_NUMBER),
            ("-a", FLAG_NONE), ("--text", FLAG_NONE),
            ("-z", FLAG_NONE),
            ("-L", FLAG_NONE), ("--follow", FLAG_NONE),
            ("--color", FLAG_STRING),
            ("--json", FLAG_NONE),
            ("--stats", FLAG_NONE),
            ("--help", FLAG_NONE), ("--version", FLAG_NONE),
            ("--debug", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- sha256sum / sha1sum / md5sum ---
    let hash_flags: HashMap<&str, &str> = [
        ("-b", FLAG_NONE), ("--binary", FLAG_NONE),
        ("-t", FLAG_NONE), ("--text", FLAG_NONE),
        ("-c", FLAG_NONE), ("--check", FLAG_NONE),
        ("--ignore-missing", FLAG_NONE),
        ("--quiet", FLAG_NONE), ("--status", FLAG_NONE),
        ("--strict", FLAG_NONE),
        ("-w", FLAG_NONE), ("--warn", FLAG_NONE),
        ("--tag", FLAG_NONE),
        ("-z", FLAG_NONE), ("--zero", FLAG_NONE),
        ("--help", FLAG_NONE), ("--version", FLAG_NONE),
    ].into_iter().collect();
    for name in &["sha256sum", "sha1sum", "md5sum"] {
        m.insert(name, BashROFlagConfig {
            safe_flags: hash_flags.clone(),
            dangerous_flags: HashMap::new(),
            respects_double_dash: true,
        });
    }

    // --- tree ---
    m.insert("tree", BashROFlagConfig {
        safe_flags: [
            ("-a", FLAG_NONE), ("-d", FLAG_NONE), ("-l", FLAG_NONE),
            ("-f", FLAG_NONE), ("-x", FLAG_NONE),
            ("-L", FLAG_NUMBER),
            ("-P", FLAG_STRING), ("-I", FLAG_STRING),
            ("--gitignore", FLAG_NONE), ("--gitfile", FLAG_STRING),
            ("--ignore-case", FLAG_NONE), ("--matchdirs", FLAG_NONE),
            ("--metafirst", FLAG_NONE), ("--prune", FLAG_NONE),
            ("--info", FLAG_NONE), ("--infofile", FLAG_STRING),
            ("--noreport", FLAG_NONE), ("--charset", FLAG_STRING),
            ("--filelimit", FLAG_NUMBER),
            ("-q", FLAG_NONE), ("-N", FLAG_NONE), ("-Q", FLAG_NONE),
            ("-p", FLAG_NONE), ("-u", FLAG_NONE), ("-g", FLAG_NONE),
            ("-s", FLAG_NONE), ("-h", FLAG_NONE),
            ("--si", FLAG_NONE), ("--du", FLAG_NONE),
            ("-D", FLAG_NONE), ("--timefmt", FLAG_STRING),
            ("-F", FLAG_NONE), ("--inodes", FLAG_NONE),
            ("--device", FLAG_NONE),
            ("-v", FLAG_NONE), ("-t", FLAG_NONE),
            ("-c", FLAG_NONE), ("-U", FLAG_NONE), ("-r", FLAG_NONE),
            ("--dirsfirst", FLAG_NONE), ("--filesfirst", FLAG_NONE),
            ("--sort", FLAG_STRING),
            ("-i", FLAG_NONE), ("-A", FLAG_NONE), ("-S", FLAG_NONE),
            ("-C", FLAG_NONE), ("-X", FLAG_NONE), ("-J", FLAG_NONE),
            ("-H", FLAG_STRING),
            ("--nolinks", FLAG_NONE),
            ("--hintro", FLAG_STRING), ("--houtro", FLAG_STRING),
            ("-T", FLAG_STRING), ("--hyperlink", FLAG_NONE),
            ("--scheme", FLAG_STRING), ("--authority", FLAG_STRING),
            ("--fromfile", FLAG_NONE), ("--fromtabfile", FLAG_NONE),
            ("--fflinks", FLAG_NONE),
            ("--help", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- date ---
    m.insert("date", BashROFlagConfig {
        safe_flags: [
            ("-d", FLAG_STRING), ("--date", FLAG_STRING),
            ("-r", FLAG_STRING), ("--reference", FLAG_STRING),
            ("-u", FLAG_NONE), ("--utc", FLAG_NONE),
            ("--universal", FLAG_NONE),
            ("-I", FLAG_NONE), ("--iso-8601", FLAG_STRING),
            ("-R", FLAG_NONE), ("--rfc-email", FLAG_NONE),
            ("--rfc-3339", FLAG_STRING),
            ("--debug", FLAG_NONE), ("--help", FLAG_NONE),
            ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- hostname ---
    m.insert("hostname", BashROFlagConfig {
        safe_flags: [
            ("-f", FLAG_NONE), ("--fqdn", FLAG_NONE), ("--long", FLAG_NONE),
            ("-s", FLAG_NONE), ("--short", FLAG_NONE),
            ("-i", FLAG_NONE), ("--ip-address", FLAG_NONE),
            ("-I", FLAG_NONE), ("--all-ip-addresses", FLAG_NONE),
            ("-a", FLAG_NONE), ("--alias", FLAG_NONE),
            ("-d", FLAG_NONE), ("--domain", FLAG_NONE),
            ("-A", FLAG_NONE), ("--all-fqdns", FLAG_NONE),
            ("-v", FLAG_NONE), ("--verbose", FLAG_NONE),
            ("-h", FLAG_NONE), ("--help", FLAG_NONE),
            ("-V", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- info ---
    m.insert("info", BashROFlagConfig {
        safe_flags: [
            ("-f", FLAG_STRING), ("--file", FLAG_STRING),
            ("-d", FLAG_STRING), ("--directory", FLAG_STRING),
            ("-n", FLAG_STRING), ("--node", FLAG_STRING),
            ("-a", FLAG_NONE), ("--all", FLAG_NONE),
            ("-k", FLAG_STRING), ("--apropos", FLAG_STRING),
            ("-w", FLAG_NONE), ("--where", FLAG_NONE),
            ("--location", FLAG_NONE), ("--show-options", FLAG_NONE),
            ("--vi-keys", FLAG_NONE), ("--subnodes", FLAG_NONE),
            ("-h", FLAG_NONE), ("--help", FLAG_NONE),
            ("--usage", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- lsof ---
    m.insert("lsof", BashROFlagConfig {
        safe_flags: [
            ("-?", FLAG_NONE), ("-h", FLAG_NONE), ("-v", FLAG_NONE),
            ("-a", FLAG_NONE), ("-b", FLAG_NONE),
            ("-C", FLAG_NONE), ("-l", FLAG_NONE),
            ("-n", FLAG_NONE), ("-N", FLAG_NONE),
            ("-O", FLAG_NONE), ("-P", FLAG_NONE),
            ("-Q", FLAG_NONE), ("-R", FLAG_NONE),
            ("-t", FLAG_NONE), ("-U", FLAG_NONE),
            ("-V", FLAG_NONE), ("-X", FLAG_NONE),
            ("-H", FLAG_NONE), ("-E", FLAG_NONE),
            ("-F", FLAG_NONE), ("-g", FLAG_NONE),
            ("-i", FLAG_NONE), ("-K", FLAG_NONE),
            ("-L", FLAG_NONE), ("-o", FLAG_NONE),
            ("-r", FLAG_NONE), ("-s", FLAG_NONE),
            ("-S", FLAG_NONE), ("-T", FLAG_NONE),
            ("-x", FLAG_NONE),
            ("-A", FLAG_STRING), ("-c", FLAG_STRING),
            ("-d", FLAG_STRING), ("-e", FLAG_STRING),
            ("-k", FLAG_STRING), ("-p", FLAG_STRING),
            ("-u", FLAG_STRING),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- pgrep ---
    m.insert("pgrep", BashROFlagConfig {
        safe_flags: [
            ("-d", FLAG_STRING), ("--delimiter", FLAG_STRING),
            ("-l", FLAG_NONE), ("--list-name", FLAG_NONE),
            ("-a", FLAG_NONE), ("--list-full", FLAG_NONE),
            ("-v", FLAG_NONE), ("--inverse", FLAG_NONE),
            ("-w", FLAG_NONE), ("--lightweight", FLAG_NONE),
            ("-c", FLAG_NONE), ("--count", FLAG_NONE),
            ("-f", FLAG_NONE), ("--full", FLAG_NONE),
            ("-g", FLAG_STRING), ("--pgroup", FLAG_STRING),
            ("-G", FLAG_STRING), ("--group", FLAG_STRING),
            ("-i", FLAG_NONE), ("--ignore-case", FLAG_NONE),
            ("-n", FLAG_NONE), ("--newest", FLAG_NONE),
            ("-o", FLAG_NONE), ("--oldest", FLAG_NONE),
            ("-O", FLAG_STRING), ("--older", FLAG_STRING),
            ("-P", FLAG_STRING), ("--parent", FLAG_STRING),
            ("-s", FLAG_STRING), ("--session", FLAG_STRING),
            ("-t", FLAG_STRING), ("--terminal", FLAG_STRING),
            ("-u", FLAG_STRING), ("--euid", FLAG_STRING),
            ("-U", FLAG_STRING), ("--uid", FLAG_STRING),
            ("-x", FLAG_NONE), ("--exact", FLAG_NONE),
            ("-F", FLAG_STRING), ("--pidfile", FLAG_STRING),
            ("-L", FLAG_NONE), ("--logpidfile", FLAG_NONE),
            ("-r", FLAG_STRING), ("--runstates", FLAG_STRING),
            ("--ns", FLAG_STRING), ("--nslist", FLAG_STRING),
            ("--help", FLAG_NONE),
            ("-V", FLAG_NONE), ("--version", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- tput ---
    m.insert("tput", BashROFlagConfig {
        safe_flags: [
            ("-T", FLAG_STRING), ("-V", FLAG_NONE), ("-x", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    // --- ss ---
    m.insert("ss", BashROFlagConfig {
        safe_flags: [
            ("-h", FLAG_NONE), ("--help", FLAG_NONE),
            ("-V", FLAG_NONE), ("--version", FLAG_NONE),
            ("-n", FLAG_NONE), ("--numeric", FLAG_NONE),
            ("-r", FLAG_NONE), ("--resolve", FLAG_NONE),
            ("-a", FLAG_NONE), ("--all", FLAG_NONE),
            ("-l", FLAG_NONE), ("--listening", FLAG_NONE),
            ("-o", FLAG_NONE), ("--options", FLAG_NONE),
            ("-e", FLAG_NONE), ("--extended", FLAG_NONE),
            ("-m", FLAG_NONE), ("--memory", FLAG_NONE),
            ("-p", FLAG_NONE), ("--processes", FLAG_NONE),
            ("-i", FLAG_NONE), ("--info", FLAG_NONE),
            ("-s", FLAG_NONE), ("--summary", FLAG_NONE),
            ("-4", FLAG_NONE), ("--ipv4", FLAG_NONE),
            ("-6", FLAG_NONE), ("--ipv6", FLAG_NONE),
            ("-0", FLAG_NONE), ("--packet", FLAG_NONE),
            ("-t", FLAG_NONE), ("--tcp", FLAG_NONE),
            ("-M", FLAG_NONE), ("--mptcp", FLAG_NONE),
            ("-S", FLAG_NONE), ("--sctp", FLAG_NONE),
            ("-u", FLAG_NONE), ("--udp", FLAG_NONE),
            ("-d", FLAG_NONE), ("--dccp", FLAG_NONE),
            ("-w", FLAG_NONE), ("--raw", FLAG_NONE),
            ("-x", FLAG_NONE), ("--unix", FLAG_NONE),
            ("--tipc", FLAG_NONE), ("--vsock", FLAG_NONE),
            ("-f", FLAG_STRING), ("--family", FLAG_STRING),
            ("-A", FLAG_STRING), ("--query", FLAG_STRING),
            ("--socket", FLAG_STRING),
            ("-Z", FLAG_NONE), ("--context", FLAG_NONE),
            ("-z", FLAG_NONE), ("--contexts", FLAG_NONE),
            ("-b", FLAG_NONE), ("--bpf", FLAG_NONE),
            ("-E", FLAG_NONE), ("--events", FLAG_NONE),
            ("-H", FLAG_NONE), ("--no-header", FLAG_NONE),
            ("-O", FLAG_NONE), ("--oneline", FLAG_NONE),
            ("--tipcinfo", FLAG_NONE), ("--tos", FLAG_NONE),
            ("--cgroup", FLAG_NONE), ("--inet-sockopt", FLAG_NONE),
        ].into_iter().collect(),
        dangerous_flags: HashMap::new(),
        respects_double_dash: true,
    });

    m
});

// ===========================================================================
// Flag validation helpers
// ===========================================================================

fn bash_ro_strip_quotes(s: &str) -> &str {
    let len = s.len();
    if len >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'\'' && bytes[len - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[len - 1] == b'"')
        {
            return &s[1..len - 1];
        }
    }
    s
}

fn bash_ro_validate_flag_arg_typed(value: &str, arg_type: &str) -> bool {
    match arg_type {
        "none" => false,
        "number" => !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()),
        "string" => true,
        "char" => value.len() == 1,
        "{}" => value == "{}",
        "EOF" => value == "EOF",
        _ => false,
    }
}

// ===========================================================================
// validate_bash_ro_flags — validates flags for a read-only command
// ===========================================================================

fn validate_bash_ro_flags(cmd: &str, config: &BashROFlagConfig) -> bool {
    let fields: Vec<&str> = cmd.split_whitespace().collect();
    if fields.len() <= 1 {
        return true;
    }
    let args = &fields[1..];
    let respects_dd = config.respects_double_dash;
    let mut seen_dd = false;

    let mut i = 0;
    while i < args.len() {
        let token = args[i];
        if token.is_empty() {
            i += 1;
            continue;
        }
        if token == "--" {
            if respects_dd {
                break;
            }
            seen_dd = true;
            i += 1;
            continue;
        }
        if !token.starts_with('-') && !seen_dd {
            i += 1;
            continue;
        }
        if !token.starts_with('-') && seen_dd {
            i += 1;
            continue;
        }
        if token.len() < 2 {
            i += 1;
            continue;
        }

        // Check dangerous flags
        if config.dangerous_flags.contains_key(token) {
            return false;
        }

        // Handle numeric shorthand like -A20
        if token.as_bytes()[0] == b'-' && token.as_bytes()[1].is_ascii_digit() {
            i += 1;
            continue;
        }

        // Parse flag
        let has_equals = token.contains('=');
        let (flag, inline_value) = if has_equals {
            let parts: Vec<&str> = token.splitn(2, '=').collect();
            (parts[0], Some(parts[1]))
        } else {
            (token, None)
        };

        if let Some(arg_type) = config.safe_flags.get(flag) {
            if *arg_type == FLAG_NONE {
                if has_equals {
                    return false;
                }
                i += 1;
                continue;
            }

            // Flag takes an argument
            let arg_value = if let Some(iv) = inline_value {
                bash_ro_strip_quotes(iv).to_string()
            } else {
                if i + 1 >= args.len() {
                    return false;
                }
                i += 1;
                bash_ro_strip_quotes(args[i]).to_string()
            };

            if !bash_ro_validate_flag_arg_typed(&arg_value, arg_type) {
                return false;
            }
            i += 1;
            continue;
        }

        // Not in safe_flags — check combined short flags
        if flag.len() == 2 && flag.starts_with('-') {
            return false;
        }
        if flag.len() > 2 && flag.starts_with('-') && !flag.starts_with("--") {
            // Try as short flag with attached arg
            let single_flag = &flag[..2];
            if let Some(st) = config.safe_flags.get(single_flag) {
                if *st != FLAG_NONE {
                    let single_arg = &flag[2..];
                    if bash_ro_validate_flag_arg_typed(single_arg, st) {
                        i += 1;
                        continue;
                    }
                }
            }
            // Combined short flags: all must be 'none' type
            for c in flag[1..].chars() {
                let single = format!("-{}", c);
                match config.safe_flags.get(single.as_str()) {
                    Some(ft) if *ft == FLAG_NONE => {}
                    _ => return false,
                }
            }
            i += 1;
            continue;
        }

        return false;
    }
    true
}

// ===========================================================================
// check_bash_read_only_command — main entry point
// ===========================================================================

/// Checks if a command is a known read-only command with safe flags.
/// Returns true if read-only, false if unknown or dangerous.
pub fn check_bash_read_only_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return false;
    }
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.is_empty() {
        return false;
    }
    let bin = fields[0].to_lowercase();
    let config = match BASH_READ_ONLY_COMMANDS.get(bin.as_str()) {
        Some(c) => c,
        None => return false,
    };
    validate_bash_ro_flags(trimmed, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grep_read_only() {
        assert!(check_bash_read_only_command("grep -r pattern"));
        assert!(check_bash_read_only_command("grep -i -n pattern file.txt"));
        assert!(!check_bash_read_only_command("grep --unknown-flag pattern"));
    }

    #[test]
    fn test_sort_read_only() {
        assert!(check_bash_read_only_command("sort -n -r"));
        assert!(!check_bash_read_only_command("sort -o output.txt"));
    }

    #[test]
    fn test_ps_read_only() {
        assert!(check_bash_read_only_command("ps aux"));
        assert!(check_bash_read_only_command("ps -ef"));
    }

    #[test]
    fn test_rg_read_only() {
        assert!(check_bash_read_only_command("rg -i pattern"));
        assert!(check_bash_read_only_command("rg --json pattern"));
    }

    #[test]
    fn test_xargs_dangerous_flags() {
        assert!(!check_bash_read_only_command("xargs -i rm {}"));
        assert!(!check_bash_read_only_command("xargs -e cmd"));
    }

    #[test]
    fn test_base64_read_only() {
        assert!(check_bash_read_only_command("base64 -d"));
        assert!(check_bash_read_only_command("base64 --decode"));
    }

    #[test]
    fn test_unknown_command() {
        assert!(!check_bash_read_only_command("python script.py"));
        assert!(!check_bash_read_only_command("rm -rf /"));
    }

    #[test]
    fn test_flag_arg_validation() {
        assert!(bash_ro_validate_flag_arg_typed("42", FLAG_NUMBER));
        assert!(!bash_ro_validate_flag_arg_typed("abc", FLAG_NUMBER));
        assert!(bash_ro_validate_flag_arg_typed("x", FLAG_CHAR));
        assert!(!bash_ro_validate_flag_arg_typed("xy", FLAG_CHAR));
        assert!(bash_ro_validate_flag_arg_typed("{}", FLAG_BRACE));
        assert!(!bash_ro_validate_flag_arg_typed("{", FLAG_BRACE));
        assert!(bash_ro_validate_flag_arg_typed("EOF", FLAG_EOF));
        assert!(!bash_ro_validate_flag_arg_typed("END", FLAG_EOF));
    }
}