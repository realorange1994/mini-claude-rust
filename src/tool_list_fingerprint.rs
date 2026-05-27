//! Tool list fingerprinting for prompt cache preservation.
//!
//! Ported from `go:tool_list_fingerprint.go`.
//!
//! When the tool list changes (tools added, removed, or schemas modified),
//! the entire prompt prefix shifts and all cached tokens are lost.
//! By tracking the fingerprint, we can:
//!   - Skip unnecessary tool re-registration when nothing changed
//!   - Log when cache invalidation is caused by tool list drift
//!   - Pin the tool list between turns to preserve cache hits

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Tracks the hash of the complete tool list to detect schema drift.
pub struct ToolListFingerprint {
    last_hash: String,
    tool_count: usize,
    drift_detected: bool,
}

impl ToolListFingerprint {
    pub fn new() -> Self {
        Self {
            last_hash: String::new(),
            tool_count: 0,
            drift_detected: false,
        }
    }

    /// Compute a deterministic hash of the tool list.
    /// Tool names are sorted to ensure stable hashing.
    pub fn compute_tool_list_hash(
        tool_names: &[String],
        tool_schemas: &HashMap<String, String>,
    ) -> String {
        let mut sorted = tool_names.to_vec();
        sorted.sort();

        let mut hasher = DefaultHasher::new();
        for name in &sorted {
            name.hash(&mut hasher);
            0u8.hash(&mut hasher); // null separator
            if let Some(schema) = tool_schemas.get(name) {
                schema.hash(&mut hasher);
            }
            0u8.hash(&mut hasher);
        }

        let h = hasher.finish();
        format!("{:016x}", h)
    }

    /// Check if the tool list has drifted since the last call.
    /// Returns true if drift was detected. Records the new fingerprint.
    pub fn check_and_record(
        &mut self,
        tool_names: &[String],
        tool_schemas: &HashMap<String, String>,
    ) -> bool {
        let new_hash = Self::compute_tool_list_hash(tool_names, tool_schemas);
        let new_count = tool_names.len();

        self.drift_detected = !self.last_hash.is_empty()
            && (self.last_hash != new_hash || self.tool_count != new_count);

        self.last_hash = new_hash;
        self.tool_count = new_count;

        self.drift_detected
    }

    pub fn drift_detected(&self) -> bool {
        self.drift_detected
    }

    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }
}

/// Tracks important content that must survive compaction.
///
/// When folding/compacting the context, certain content must be preserved
/// (active skill, constraints, tool results in progress).
pub struct FoldSummaryPin {
    pub active_skills: Vec<String>,
    pub constraints: Vec<String>,
    pub in_progress_tool_call: String,
    pub system_prompt: String,
}

impl FoldSummaryPin {
    pub fn new() -> Self {
        Self {
            active_skills: Vec::new(),
            constraints: Vec::new(),
            in_progress_tool_call: String::new(),
            system_prompt: String::new(),
        }
    }

    /// Record the currently active skill.
    pub fn set_active_skill(&mut self, skill_name: &str) {
        // Replace, don't append — only one active skill at a time.
        self.active_skills = vec![skill_name.to_string()];
    }

    /// Add a constraint that must survive folding.
    pub fn add_constraint(&mut self, constraint: &str) {
        // Deduplicate.
        if !self.constraints.iter().any(|c| c == constraint) {
            self.constraints.push(constraint.to_string());
        }
    }

    /// Record a tool call awaiting results.
    pub fn set_in_progress_tool_call(&mut self, tool_use_id: &str) {
        self.in_progress_tool_call = tool_use_id.to_string();
    }

    /// Cache the system prompt for extracting pinned constraints during compaction.
    pub fn set_system_prompt(&mut self, system_prompt: &str) {
        self.system_prompt = system_prompt.to_string();
    }

    /// Generate a prompt fragment that ensures pinned content survives compaction.
    /// This is prepended to the compaction summary.
    pub fn build_pin_prompt(&self) -> String {
        let mut parts = Vec::new();

        // Extract pinned constraints from system prompt.
        if !self.system_prompt.is_empty() {
            if let Some(constraints) = extract_pinned_constraints(&self.system_prompt) {
                if !constraints.is_empty() {
                    parts.push(format!("[PINNED CONSTRAINTS]\n{}", constraints));
                }
            }
        }

        if !self.active_skills.is_empty() {
            let skills_json = serde_json::to_string(&self.active_skills)
                .unwrap_or_default();
            parts.push(format!("active_skills={}", skills_json));
        }

        if !self.constraints.is_empty() {
            let constraints_json = serde_json::to_string(&self.constraints)
                .unwrap_or_default();
            parts.push(format!("constraints={}", constraints_json));
        }

        if !self.in_progress_tool_call.is_empty() {
            parts.push(format!("in_progress_tool={}", self.in_progress_tool_call));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("[PERSIST] {}", parts.join(" "))
        }
    }

    pub fn clear(&mut self) {
        self.active_skills.clear();
        self.constraints.clear();
        self.in_progress_tool_call.clear();
    }
}

/// Extract pinned constraints from system prompt to preserve them across compaction.
/// Pattern: # HIGH PRIORITY constraints, # User memory, # Project memory.
fn extract_pinned_constraints(system_prompt: &str) -> Option<String> {
    let headers = [
        "HIGH PRIORITY constraints",
        "User memory",
        "Project memory",
    ];

    let mut results = Vec::new();
    let mut current = Vec::new();
    let mut active_header = false;

    for line in system_prompt.lines() {
        let trimmed = line.trim();
        let is_header = trimmed.starts_with("# ");

        // Check if this line starts any of our target sections.
        let header_matched = if is_header {
            headers.iter().any(|h| line.contains(h))
        } else {
            false
        };

        if header_matched {
            // Save previous block if any.
            if !current.is_empty() {
                results.push(current.join("\n"));
            }
            active_header = true;
            current = vec![line.to_string()];
        } else if active_header {
            // Check for end of section (new header or end of file).
            if is_header {
                results.push(current.join("\n"));
                active_header = false;
                current.clear();
            } else if !trimmed.is_empty() {
                current.push(line.to_string());
            }
        }
    }

    // Don't forget the last block.
    if !current.is_empty() {
        results.push(current.join("\n"));
    }

    if results.is_empty() {
        None
    } else {
        Some(results.join("\n\n"))
    }
}

/// Classifies the type of tool-list drift for cache impact assessment.
/// Ordered by "cache cost" — identity and append are nearly free; reorder is catastrophic.
#[derive(Debug, PartialEq)]
pub enum DriftKind {
    /// No change — same tools, same order, same content.
    Identity,
    /// New tools added at end, existing unchanged.
    Append,
    /// Same tool set but schemas changed.
    Edit,
    /// Same tool set but order changed — cache as bad as structural.
    Reorder,
    /// Tools removed — catastrophic regardless of other changes.
    Remove,
}

/// Contains the classification of tool list drift.
pub struct DriftReport {
    pub kind: DriftKind,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub edited: Vec<String>,
}

/// Analyze before/after tool lists and classify the drift.
/// This helps understand cache impact: identity and append are cheap,
/// others cause cache misses.
pub fn classify_tool_list_drift(
    before: &[String],
    after: &[String],
    before_schemas: &HashMap<String, String>,
    after_schemas: &HashMap<String, String>,
) -> DriftReport {
    let before_set: std::collections::HashSet<&str> =
        before.iter().map(|s| s.as_str()).collect();
    let after_set: std::collections::HashSet<&str> =
        after.iter().map(|s| s.as_str()).collect();

    let added: Vec<String> = after
        .iter()
        .filter(|n| !before_set.contains(n.as_str()))
        .cloned()
        .collect();
    let removed: Vec<String> = before
        .iter()
        .filter(|n| !after_set.contains(n.as_str()))
        .cloned()
        .collect();

    let mut edited = Vec::new();
    let shared_len = before.len().min(after.len());
    for i in 0..shared_len {
        if before[i] == after[i]
            && before_schemas.get(&before[i]) != after_schemas.get(&after[i])
        {
            edited.push(before[i].clone());
        }
    }

    // Identity: same length, same names in order, same content.
    if before.len() == after.len() && edited.is_empty() {
        let same_order = (0..before.len()).all(|i| before[i] == after[i]);
        if same_order {
            return DriftReport {
                kind: DriftKind::Identity,
                added: Vec::new(),
                removed: Vec::new(),
                edited: Vec::new(),
            };
        }
    }

    // Remove anywhere: catastrophic.
    if !removed.is_empty() {
        return DriftReport {
            kind: DriftKind::Remove,
            added,
            removed,
            edited: Vec::new(),
        };
    }

    // Append: every before-tool stays, new ones at end.
    if after.len() > before.len() {
        let all_match = (0..before.len()).all(|i| {
            before[i] == after[i]
                && before_schemas.get(&before[i]) == after_schemas.get(&after[i])
        });
        if all_match {
            return DriftReport {
                kind: DriftKind::Append,
                added,
                removed: Vec::new(),
                edited: Vec::new(),
            };
        }
    }

    // Same name set? Then positions or content changed.
    let same_name_set = before_set.len() == after_set.len()
        && before_set.iter().all(|n| after_set.contains(n));

    if same_name_set {
        let positions_match = (0..before.len()).all(|i| before[i] == after[i]);
        if positions_match {
            return DriftReport {
                kind: DriftKind::Edit,
                added: Vec::new(),
                removed: Vec::new(),
                edited,
            };
        }
        // Same set, different order — cache-wise as bad as structural change.
        return DriftReport {
            kind: DriftKind::Reorder,
            added: Vec::new(),
            removed: Vec::new(),
            edited: Vec::new(),
        };
    }

    // Additions present but NOT clean appends — treat as reorder.
    DriftReport {
        kind: DriftKind::Reorder,
        added,
        removed: Vec::new(),
        edited: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_drift_identity() {
        let mut fp = ToolListFingerprint::new();
        let tools = vec!["read_file".into(), "write_file".into()];
        let mut schemas = HashMap::new();
        schemas.insert("read_file".into(), "schema1".into());
        schemas.insert("write_file".into(), "schema2".into());

        // First call: no drift.
        let r1 = fp.check_and_record(&tools, &schemas);
        assert!(!r1);

        // Same call: still no drift.
        let r2 = fp.check_and_record(&tools, &schemas);
        assert!(!r2);
    }

    #[test]
    fn test_fingerprint_drift_new_tool() {
        let mut fp = ToolListFingerprint::new();
        let tools = vec!["read_file".into(), "write_file".into()];
        let mut schemas = HashMap::new();
        schemas.insert("read_file".into(), "schema1".into());
        schemas.insert("write_file".into(), "schema2".into());

        fp.check_and_record(&tools, &schemas);

        // Add new tool.
        let tools2 = vec!["read_file".into(), "write_file".into(), "grep".into()];
        schemas.insert("grep".into(), "schema3".into());

        let r = fp.check_and_record(&tools2, &schemas);
        assert!(r);
        assert_eq!(fp.last_hash().len(), 16);
    }

    #[test]
    fn test_fold_pin_prompt() {
        let mut pin = FoldSummaryPin::new();
        pin.set_system_prompt("# HIGH PRIORITY constraints\n- Always use read_file before edit\n\nSome other content.");
        pin.set_active_skill("rust-coding");
        pin.add_constraint("use read_file before edit");
        pin.set_in_progress_tool_call("call_123");

        let prompt = pin.build_pin_prompt();
        assert!(prompt.contains("[PERSIST]"));
        assert!(prompt.contains("active_skills"));
        assert!(prompt.contains("in_progress_tool=call_123"));
    }

    #[test]
    fn test_drift_identity() {
        let before = vec!["a".into(), "b".into()];
        let after = vec!["a".into(), "b".into()];
        let mut schemas = HashMap::new();
        schemas.insert("a".into(), "s1".into());
        schemas.insert("b".into(), "s2".into());

        let report = classify_tool_list_drift(&before, &after, &schemas, &schemas);
        assert_eq!(report.kind, DriftKind::Identity);
    }

    #[test]
    fn test_drift_append() {
        let before = vec!["a".into(), "b".into()];
        let after = vec!["a".into(), "b".into(), "c".into()];
        let mut schemas = HashMap::new();
        schemas.insert("a".into(), "s1".into());
        schemas.insert("b".into(), "s2".into());
        schemas.insert("c".into(), "s3".into());

        let report = classify_tool_list_drift(&before, &after, &schemas, &schemas);
        assert_eq!(report.kind, DriftKind::Append);
        assert_eq!(report.added, vec!["c"]);
    }

    #[test]
    fn test_drift_remove() {
        let before = vec!["a".into(), "b".into(), "c".into()];
        let after = vec!["a".into(), "b".into()];
        let schemas = HashMap::new();

        let report = classify_tool_list_drift(&before, &after, &schemas, &schemas);
        assert_eq!(report.kind, DriftKind::Remove);
        assert_eq!(report.removed, vec!["c"]);
    }
}
