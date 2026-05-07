//! Permissions module - rule-based permission system
//!
//! Mirrors the Go permissions package for upstream alignment.

pub mod rule_parser;
pub mod rule_store;
pub mod rules_loader;
pub mod internal_paths;
pub mod path_validation;
pub mod auto_strip;
mod main; // PermissionGate, PermissionMode, etc.

// Re-export from main.rs (the old permissions.rs)
pub use main::{PermissionGate, PermissionMode};

pub use rule_parser::{ParsedRule, parse_rule, parse_rules};
pub use rule_store::RuleStore;
pub use rules_loader::{PermissionsConfig, load_rules_from_config, load_rules_from_file, load_rules_from_all_sources};
pub use path_validation::{OperationType, PathValidationResult, validate_path, validate_read_path};
pub use auto_strip::{DANGEROUS_SHELL_PATTERNS, is_dangerous_allow_rule, stripped_rules_summary};

// Tool name constants for rule system integration
pub const FILE_READ_TOOL_NAME: &str = "read_file";
pub const FILE_WRITE_TOOL_NAME: &str = "write_file";
pub const FILE_EDIT_TOOL_NAME: &str = "edit_file";
pub const EXEC_TOOL_NAME: &str = "exec";
pub const GIT_TOOL_NAME: &str = "git";

/// Map upstream tool names to internal names
pub fn upstream_to_internal(upstream: &str) -> String {
    match upstream {
        "Read" => FILE_READ_TOOL_NAME.to_string(),
        "Write" => FILE_WRITE_TOOL_NAME.to_string(),
        "Edit" => FILE_EDIT_TOOL_NAME.to_string(),
        "Bash" => EXEC_TOOL_NAME.to_string(),
        _ => upstream.to_string(),
    }
}

