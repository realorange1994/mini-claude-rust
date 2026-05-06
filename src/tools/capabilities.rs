//! Tool capability definitions for security-aware tool classification.
//!
//! Provides a type-safe way to describe what each tool can do,
//! enabling permission decisions based on declared capabilities rather
//! than hardcoded tool-name matching.

/// Capabilities a tool may possess.
///
/// A tool can have multiple capabilities. Permission decisions are made
/// by examining which capabilities a tool declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    /// Tool only reads data and never modifies system state.
    ReadOnly,
    /// Tool writes or modifies files on disk.
    WritesFiles,
    /// Tool executes arbitrary code or commands.
    ExecutesCode,
    /// Tool makes network requests.
    Network,
    /// Tool spawns subprocesses or background tasks.
    Subprocess,
    /// Tool can be sandboxed (restricted to a limited namespace).
    Sandboxable,
    /// Tool creates or modifies persistent background services.
    CreatesServices,
    /// Tool modifies shell profiles or system configuration.
    ModifiesShellEnv,
}

/// How much user involvement is required before executing a tool.
///
/// This drives the permission check flow:
/// - `Auto`: Execute immediately without any prompt or classifier call
/// - `Suggest`: Run through the LLM classifier (in Auto mode); suggest approval
/// - `Required`: Always ask the user directly
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    /// Auto-approved: execute immediately with no user involvement.
    Auto,
    /// Classifier-reviewed: in Auto mode, go through the LLM classifier.
    /// If the classifier allows it, execute. Otherwise fall back to Required.
    Classifier,
    /// Explicit approval required: always ask the user directly.
    Required,
}

impl Default for ApprovalRequirement {
    fn default() -> Self {
        ApprovalRequirement::Auto
    }
}

impl ApprovalRequirement {
    /// Whether this requirement means the tool can run without any user interaction.
    pub fn is_auto_approved(&self) -> bool {
        matches!(self, ApprovalRequirement::Auto)
    }

    /// Whether this requirement means the tool needs the LLM classifier.
    pub fn needs_classifier(&self) -> bool {
        matches!(self, ApprovalRequirement::Classifier)
    }

    /// Whether this requirement means the tool needs explicit user approval.
    pub fn needs_user_approval(&self) -> bool {
        matches!(self, ApprovalRequirement::Required)
    }
}

/// Overall safety level for a tool, derived from its declared capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SafetyLevel {
    /// Read-only, no side effects beyond internal state.
    Safe,
    /// Modifies files or state within the workspace/project.
    Caution,
    /// Executes code, modifies system state, or makes network calls.
    Dangerous,
    #[default]
    Unknown,
}

impl SafetyLevel {
    pub fn from_capabilities(caps: &[ToolCapability]) -> Self {
        if caps.is_empty() {
            return SafetyLevel::Unknown;
        }
        if caps.iter().all(|c| matches!(c, ToolCapability::ReadOnly)) {
            return SafetyLevel::Safe;
        }
        if caps.iter().any(|c| {
            matches!(
                c,
                ToolCapability::ExecutesCode
                    | ToolCapability::Network
                    | ToolCapability::CreatesServices
                    | ToolCapability::ModifiesShellEnv
            )
        }) {
            return SafetyLevel::Dangerous;
        }
        // Has WritesFiles but nothing more dangerous
        SafetyLevel::Caution
    }

    pub fn is_safe(&self) -> bool {
        matches!(self, SafetyLevel::Safe)
    }

    pub fn is_dangerous(&self) -> bool {
        matches!(self, SafetyLevel::Dangerous)
    }
}

/// A tool's declared capabilities and approval requirement.
/// Convenience struct combining the two for permission checks.
#[derive(Debug, Clone, Default)]
pub struct ToolProfile {
    pub capabilities: Vec<ToolCapability>,
    pub approval: ApprovalRequirement,
}

impl ToolProfile {
    pub fn new(capabilities: Vec<ToolCapability>, approval: ApprovalRequirement) -> Self {
        Self {
            capabilities,
            approval,
        }
    }

    pub fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::from_capabilities(&self.capabilities)
    }

    /// Read-only tool that auto-approves.
    pub fn read_only() -> Self {
        Self {
            capabilities: vec![ToolCapability::ReadOnly],
            approval: ApprovalRequirement::Auto,
        }
    }

    /// Tool that writes files, goes through classifier in Auto mode.
    pub fn writes_files() -> Self {
        Self {
            capabilities: vec![ToolCapability::WritesFiles],
            approval: ApprovalRequirement::Classifier,
        }
    }

    /// Tool that executes code, requires explicit approval.
    pub fn executes_code() -> Self {
        Self {
            capabilities: vec![ToolCapability::ExecutesCode],
            approval: ApprovalRequirement::Required,
        }
    }
}
