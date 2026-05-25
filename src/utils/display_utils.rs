//! Display and rendering utilities.
//! Ported from upstream utils_display.go (166 lines).
//!
//! Provides:
//! - HookSummary: system stop hook summary messages
//! - CollapseHookSummaries: collapse consecutive hook summaries
//! - ToolUseGrouping: group consecutive identical tool uses

// =============================================================================
// Hook Summary Types
// =============================================================================

/// System stop hook summary message.
#[derive(Debug, Clone)]
pub struct HookSummary {
    pub type_: String,
    pub subtype: String,
    pub hook_label: String,
    pub hook_count: usize,
    pub hook_infos: Vec<String>,
    pub hook_errors: Vec<String>,
    pub prevented_continuation: bool,
    pub has_output: bool,
    pub total_duration_ms: u64,
}

impl HookSummary {
    /// Check if this is a labeled hook summary (system stop type).
    pub fn is_labeled(&self) -> bool {
        self.type_ == "system"
            && self.subtype == "stop_hook_summary"
            && !self.hook_label.is_empty()
    }
}

/// Collapse consecutive hook summary messages with the same hook_label
/// into a single summary.
pub fn collapse_hook_summaries(messages: Vec<HookSummary>) -> Vec<HookSummary> {
    let mut result: Vec<HookSummary> = Vec::with_capacity(messages.len());
    let mut i = 0;

    while i < messages.len() {
        if messages[i].is_labeled() {
            let label = messages[i].hook_label.clone();
            let mut group: Vec<HookSummary> = Vec::new();

            while i < messages.len()
                && messages[i].is_labeled()
                && messages[i].hook_label == label
            {
                group.push(messages[i].clone());
                i += 1;
            }

            if group.len() == 1 {
                result.push(group.remove(0));
            } else {
                // Collapse the group into a single summary
                let mut merged = HookSummary {
                    type_: group[0].type_.clone(),
                    subtype: group[0].subtype.clone(),
                    hook_label: label,
                    hook_count: 0,
                    hook_infos: Vec::new(),
                    hook_errors: Vec::new(),
                    prevented_continuation: false,
                    has_output: false,
                    total_duration_ms: 0,
                };

                for m in &group {
                    merged.hook_count += m.hook_count;
                    merged.hook_infos.extend_from_slice(&m.hook_infos);
                    merged.hook_errors.extend_from_slice(&m.hook_errors);
                    if m.prevented_continuation {
                        merged.prevented_continuation = true;
                    }
                    if m.has_output {
                        merged.has_output = true;
                    }
                    if m.total_duration_ms > merged.total_duration_ms {
                        merged.total_duration_ms = m.total_duration_ms;
                    }
                }
                result.push(merged);
            }
        } else {
            result.push(messages[i].clone());
            i += 1;
        }
    }

    result
}

// =============================================================================
// Tool Use Grouping Types
// =============================================================================

/// A single tool use invocation.
#[derive(Debug, Clone)]
pub struct ToolUseEntry {
    pub id: String,
    pub name: String,
    pub input: String,
    pub output: String,
    pub status: String,
}

/// A grouping of consecutive identical tool uses.
#[derive(Debug, Clone)]
pub struct ToolUseGroup {
    pub id: String,
    pub name: String,
    pub input: String,
    pub output: String,
    pub status: String,
    pub is_grouped: bool,
    pub tool_uses: Vec<ToolUseEntry>,
}

/// Group consecutive identical tool uses.
/// This collapses repeated tool invocations (like multiple file reads)
/// into a single expandable group.
pub fn apply_grouping(tool_uses: Vec<ToolUseEntry>) -> Vec<ToolUseGroup> {
    let mut result: Vec<ToolUseGroup> = Vec::new();

    let mut i = 0;
    while i < tool_uses.len() {
        let entry = tool_uses[i].clone();

        // Collect consecutive same-name tool uses
        let mut group: Vec<ToolUseEntry> = vec![entry.clone()];
        i += 1;

        while i < tool_uses.len() && tool_uses[i].name == entry.name {
            group.push(tool_uses[i].clone());
            i += 1;
        }

        if group.len() == 1 {
            result.push(ToolUseGroup {
                id: entry.id,
                name: entry.name,
                input: entry.input,
                output: entry.output,
                status: entry.status,
                is_grouped: false,
                tool_uses: group,
            });
        } else {
            result.push(ToolUseGroup {
                id: entry.id,
                name: entry.name,
                input: entry.input,
                output: entry.output,
                status: entry.status,
                is_grouped: true,
                tool_uses: group,
            });
        }
    }

    result
}

/// Return a display string for a (possibly grouped) tool use.
pub fn render_grouped_tool_use(group: &ToolUseGroup) -> String {
    if !group.is_grouped {
        format!("{}({})", group.name, group.id)
    } else {
        let count = group.tool_uses.len();
        format!("{}({}) [x{}]", group.name, group.id, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_labeled_hook_summary() {
        let s = HookSummary {
            type_: "system".to_string(),
            subtype: "stop_hook_summary".to_string(),
            hook_label: "PostToolUse".to_string(),
            hook_count: 1,
            hook_infos: vec![],
            hook_errors: vec![],
            prevented_continuation: false,
            has_output: true,
            total_duration_ms: 100,
        };
        assert!(s.is_labeled());

        let unlabeled = HookSummary {
            hook_label: "".to_string(),
            ..s
        };
        assert!(!unlabeled.is_labeled());
    }

    #[test]
    fn test_collapse_hook_summaries_single() {
        let msg = HookSummary {
            type_: "system".to_string(),
            subtype: "stop_hook_summary".to_string(),
            hook_label: "TestHook".to_string(),
            hook_count: 1,
            hook_infos: vec![],
            hook_errors: vec![],
            prevented_continuation: false,
            has_output: true,
            total_duration_ms: 100,
        };
        let result = collapse_hook_summaries(vec![msg]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hook_count, 1);
    }

    #[test]
    fn test_collapse_hook_summaries_group() {
        let mk = |count: usize, duration: u64| HookSummary {
            type_: "system".to_string(),
            subtype: "stop_hook_summary".to_string(),
            hook_label: "PostToolUse".to_string(),
            hook_count: count,
            hook_infos: vec![],
            hook_errors: vec![],
            prevented_continuation: false,
            has_output: true,
            total_duration_ms: duration,
        };

        let messages = vec![mk(1, 100), mk(2, 200), mk(3, 150)];
        let result = collapse_hook_summaries(messages);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hook_count, 6); // 1+2+3
        assert_eq!(result[0].total_duration_ms, 200); // max of [100, 200, 150]
    }

    #[test]
    fn test_apply_grouping() {
        let mk = |id: &str, name: &str| ToolUseEntry {
            id: id.to_string(),
            name: name.to_string(),
            input: String::new(),
            output: String::new(),
            status: String::new(),
        };

        let tool_uses = vec![
            mk("a", "read"),
            mk("b", "read"),
            mk("c", "write"),
            mk("d", "read"),
        ];

        let groups = apply_grouping(tool_uses);
        assert_eq!(groups.len(), 3);

        // First group: 2x read (grouped)
        assert!(groups[0].is_grouped);
        assert_eq!(groups[0].tool_uses.len(), 2);

        // Second: write (not grouped)
        assert!(!groups[1].is_grouped);

        // Third: read (not grouped, separate from earlier reads)
        assert!(!groups[2].is_grouped);
    }

    #[test]
    fn test_render_grouped_tool_use() {
        let single = ToolUseGroup {
            id: "abc".to_string(),
            name: "read".to_string(),
            input: String::new(),
            output: String::new(),
            status: String::new(),
            is_grouped: false,
            tool_uses: vec![],
        };
        assert_eq!(render_grouped_tool_use(&single), "read(abc)");

        let grouped = ToolUseGroup {
            is_grouped: true,
            tool_uses: vec![
                ToolUseEntry {
                    id: "x".to_string(),
                    name: String::new(),
                    input: String::new(),
                    output: String::new(),
                    status: String::new(),
                },
                ToolUseEntry {
                    id: "y".to_string(),
                    name: String::new(),
                    input: String::new(),
                    output: String::new(),
                    status: String::new(),
                },
            ],
            ..single.clone()
        };
        assert_eq!(render_grouped_tool_use(&grouped), "read(abc) [x2]");
    }
}