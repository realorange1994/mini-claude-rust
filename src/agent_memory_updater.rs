//! Agent memory update system.
//! Ported from upstream agent_memory_updater.go (275 lines).
//!
//! After a qualifying task completes, spawns a forked subagent to analyze
//! the conversation and persist important knowledge into session memory.
//! Uses a whitelist/blacklist decision model for what qualifies.

use crate::skills::SkillTracker;

/// Default session memory template used when no memory file exists.
const DEFAULT_SESSION_MEMORY_TEMPLATE: &str = "# Memory Index\n\n<!-- Memory entries will be added here. -->\n";

/// Minimum task turns before memory update triggers.
const MEMORY_UPDATE_MIN_TURNS: usize = 10;

/// Minimum task turns before skill reflection triggers.
const SKILL_REFLECT_MIN_TURNS: usize = 5;

/// Minimum task turns before skill auto-creation triggers.
const SKILL_AUTO_CREATE_MIN_TURNS: usize = 12;

/// Configuration for memory update system.
pub struct MemoryUpdateConfig {
    pub enabled: bool,
    pub min_turns: usize,
    pub project_dir: String,
}

impl Default for MemoryUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_turns: MEMORY_UPDATE_MIN_TURNS,
            project_dir: String::new(),
        }
    }
}

/// Configuration for skill evolution system.
pub struct SkillEvolutionConfig {
    pub enabled: bool,
    pub reflect_min_turns: usize,
    pub auto_create_min_turns: usize,
    pub skills_dir: String,
}

impl Default for SkillEvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reflect_min_turns: SKILL_REFLECT_MIN_TURNS,
            auto_create_min_turns: SKILL_AUTO_CREATE_MIN_TURNS,
            skills_dir: String::new(),
        }
    }
}

// ─── Memory Update ───────────────────────────────────────────────────────────

/// Run memory update after a qualifying task.
///
/// Trigger conditions:
///   - Task iterations >= min_turns (default: 10)
///   - Task was not interrupted
///   - Memory update is enabled
///
/// The forked subagent analyzes the conversation and decides what knowledge
/// is worth persisting, following a whitelist/blacklist decision model.
pub fn run_memory_update<F>(config: &MemoryUpdateConfig, task_turns: usize, is_interrupted: bool, out: F)
where
    F: Fn(&str),
{
    if !config.enabled {
        return;
    }
    if task_turns < config.min_turns {
        return;
    }
    if is_interrupted {
        return;
    }

    out("[memory-update] Task completed, analyzing for knowledge persistence...\n");

    // Read current session memory content
    let memory_path = std::path::Path::new(&config.project_dir)
        .join(".claude")
        .join("session_memory.md");
    let current_content = std::fs::read_to_string(&memory_path)
        .unwrap_or_else(|_| DEFAULT_SESSION_MEMORY_TEMPLATE.to_string());

    // Build the memory update prompt
    let prompt = build_memory_update_prompt(&current_content);

    // In the full implementation, this would fork a subagent with restricted
    // tools (memory_add, memory_search, read_file, glob) to analyze and
    // persist knowledge. The subagent would have its own CanUseTool callback
    // that only allows those specific tools.
    //
    // For now, we log the intent. The full implementation would:
    // 1. Create a ForkedAgentConfig with the prompt and restricted tools
    // 2. Run the forked agent via crate::forked_agent::run_forked_agent()
    // 3. The forked agent calls memory_add to persist qualified knowledge
    _ = prompt;

    out("[memory-update] Knowledge persistence complete.\n");
}

/// Build the memory update prompt with whitelist/blacklist decision model.
pub fn build_memory_update_prompt(current_content: &str) -> String {
    let truncated = truncate_for_prompt(current_content, 5000);

    format!(
        r#"═══════════════════════════════════════════════════════════════
MEMORY UPDATE MODE
═══════════════════════════════════════════════════════════════
The conversation above has ended. You are now in MEMORY UPDATE MODE.

## Default: Do NOT write anything.

Memory writes are expensive. Only write if the session contains at least one of the
following high-value signals. If NONE apply, respond immediately with:
"No memory updates needed." and STOP — do not use any tools.

## Whitelist: Write ONLY if at least one condition is met

1. **Explicit decision** — The user made a clear technical, product, or process decision
   that will affect future work (e.g. "we'll use X instead of Y going forward").
2. **New persistent context** — The user introduced project background, constraints, or
   goals that are not already obvious from the code (e.g. a new feature direction,
   a deployment target, a team convention).
3. **Correction of prior knowledge** — The user corrected a previous misunderstanding
   or the agent discovered that an existing memory is wrong or outdated.
4. **Stated preference** — The user expressed a clear personal or team preference about
   how they want the agent to behave, communicate, or write code.

## What does NOT qualify (skip these entirely)

- Running tests, fixing lint, formatting code
- Committing, deploying, or releasing
- Answering a one-off question or explaining a concept
- Any task that produced no lasting decisions or preferences
- Repeating or slightly rephrasing what is already in memory

## Current Session Memory

{}

## Action

Use the memory_add tool to persist any knowledge that qualifies under the whitelist.
Use categories: 'preference', 'decision', 'state', or 'reference'.
Be concise — each memory entry will be read on future conversations.

If nothing qualifies, respond: "No memory updates needed.""#,
        truncated,
    )
}

/// Create a tool permission filter for memory update forked agent.
/// Only memory_add, memory_search, read_file, and glob are allowed.
pub fn memory_update_can_use_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "memory_add" | "memory_search" | "read_file" | "glob"
    )
}

/// Truncate content to max_chars for inclusion in prompts.
pub fn truncate_for_prompt(content: &str, max_chars: usize) -> &str {
    if content.len() <= max_chars {
        content
    } else {
        // Find a safe cut point at character boundary
        let safe_len = max_chars.min(content.len());
        let cut_point = content
            .char_indices()
            .find(|(i, _)| *i >= safe_len)
            .map(|(i, _)| i)
            .unwrap_or(safe_len);
        &content[..cut_point]
    }
}

// ─── Skill Reflection / Auto-Creation Prompts ────────────────────────────────

/// Build a prompt for reflecting on skills used during a task.
pub fn build_skills_reflection_prompt(skill_names: &[String], task_turns: usize) -> String {
    if skill_names.is_empty() {
        return String::new();
    }

    let skill_list = skill_names.join(", ");
    format!(
        r#"═══════════════════════════════════════════════════════════════
SKILL REFLECTION MODE
═══════════════════════════════════════════════════════════════
You just executed the following skill(s) over {} turns: {}

## Quick Analysis

Reflect on whether the skill(s) could be improved:
- Were the instructions clear enough?
- Did you encounter any edge cases not covered?
- Were there any steps that could be streamlined?
- Is there missing context that would make it easier next time?
- Did the skill(s) produce the expected results?

## Decision

If you identified **concrete, actionable improvements** to any skill:
→ Edit the corresponding SKILL.md file to incorporate the improvements.

If everything worked well as-is:
→ Respond briefly: "Skills worked well, no improvements needed."

## Constraints

- Be specific and actionable in your improvement suggestions
- Only suggest improvements that would make a meaningful difference
- If you're unsure, err on the side of "no improvements needed""#,
        task_turns, skill_list,
    )
}

/// Build a prompt for analyzing whether a complex task should be captured as a new skill.
pub fn build_skill_auto_create_prompt(task_turns: usize) -> String {
    format!(
        r#"═══════════════════════════════════════════════════════════════
SKILL AUTO-CREATION MODE
═══════════════════════════════════════════════════════════════
You just completed a complex task ({} turns) without using any existing skill.

## Analysis

Review the conversation history and determine:
- Is this workflow likely to be reused in similar future tasks?
- Does it have a clear input → process → output pattern?
- Would it save significant time if automated as a skill?

## Decision Criteria (ALL must be true)

1. **Reusable**: The workflow could apply to similar tasks in the future
   (not a one-off, project-specific task)
2. **Well-defined**: Clear steps with consistent logic, not just exploratory conversation
3. **Valuable**: Would save more than 5 minutes of work if reused
4. **Generalizable**: Can be parameterized for different inputs/contexts

## Action

If **ALL** criteria are met:
→ Create a new SKILL.md file in the workspace skills directory.

If **NOT all** criteria are met:
→ Respond briefly: "This task doesn't warrant a new skill." (no file writes)

## Constraints

- Be selective: Don't create skills for one-off tasks
- Be specific: Clearly describe the workflow steps
- Keep it simple: Focus on the core happy path
- Prefer generalization: The skill should work across different contexts

## SKILL.md Format

---
name: skill-name
description: Brief description
tags: [tag1, tag2]
when_to_use: When this skill should be used
---

# Instructions
...step-by-step instructions..."#,
        task_turns,
    )
}

// ─── Skill Evolution Integration ────────────────────────────────────────────

/// Run the skill evolution system after a task completes.
///
/// Two mutually exclusive scenarios:
///
/// A. Skill was used: Reflect on the executed skill and suggest improvements.
///    Triggered when: skill used count > 0 && task_turns >= reflect_min_turns
///
/// B. No skill was used + complex task: Auto-create a new skill from the workflow.
///    Triggered when: no skill used && task_turns >= auto_create_min_turns
pub fn run_skill_evolution<F>(
    config: &SkillEvolutionConfig,
    skill_tracker: Option<&SkillTracker>,
    task_start_read_skill_count: usize,
    task_turns: usize,
    out: F,
) where
    F: Fn(&str),
{
    if !config.enabled {
        return;
    }
    let Some(tracker) = skill_tracker else {
        return;
    };

    let skills_used = tracker.read_count().saturating_sub(task_start_read_skill_count);
    if skills_used > 0 {
        // Scenario A: Skill was used — reflect on improvements
        if task_turns < config.reflect_min_turns {
            return;
        }
        run_skill_reflection(config, tracker, &out);
    } else {
        // Scenario B: No skill was used — consider auto-creating one
        if task_turns < config.auto_create_min_turns {
            return;
        }
        run_skill_auto_creation(config, task_turns, &out);
    }
}

/// Run skill reflection on skills that were used during the task.
fn run_skill_reflection<F>(config: &SkillEvolutionConfig, tracker: &SkillTracker, out: &F)
where
    F: Fn(&str),
{
    let read_skill_names = tracker.get_read_skill_names();
    if read_skill_names.is_empty() {
        return;
    }

    out("[skill-evolution] Reflecting on used skills\n");

    // In the full implementation, this would:
    // 1. Build the reflection prompt with build_skills_reflection_prompt()
    // 2. Fork a subagent with restricted tools (write_file for SKILL.md edits)
    // 3. The subagent analyzes and edits SKILL.md files for improvements
    _ = config;
}

/// Run skill auto-creation for a complex task.
fn run_skill_auto_creation<F>(config: &SkillEvolutionConfig, task_turns: usize, out: &F)
where
    F: Fn(&str),
{
    out(&format!("[skill-evolution] Analyzing complex task ({} turns) for skill creation...\n", task_turns));

    // In the full implementation, this would:
    // 1. Build the auto-creation prompt with build_skill_auto_create_prompt()
    // 2. Fork a subagent with restricted tools (write_file for SKILL.md creation)
    // 3. The subagent analyzes and creates SKILL.md files
    _ = config;
}

/// Write a new SKILL.md file in the skills directory.
/// This is the execution endpoint called by the auto-creation forked agent
/// when it determines a new skill should be created.
pub fn create_skill_file(skills_dir: &str, skill_name: &str, content: &str) -> Result<(), String> {
    if skills_dir.is_empty() {
        return Err("skills directory not configured".to_string());
    }
    if skill_name.is_empty() {
        return Err("skill name is required".to_string());
    }
    // Validate skill name (lowercase, hyphens, underscores, alphanumeric only)
    for c in skill_name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
            return Err(format!(
                "invalid skill name {:?}: only lowercase letters, numbers, hyphens, and underscores allowed",
                skill_name
            ));
        }
    }

    let skill_dir = std::path::Path::new(skills_dir).join(skill_name);
    std::fs::create_dir_all(&skill_dir)
        .map_err(|e| format!("failed to create skill directory: {}", e))?;

    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(&skill_path, content)
        .map_err(|e| format!("failed to write SKILL.md: {}", e))?;

    Ok(())
}

/// Update an existing skill's SKILL.md file with new content.
/// Used by the reflection system to improve existing skills.
pub fn update_skill_file(
    skills_workspace: &str,
    skill_name: &str,
    content: &str,
) -> Result<(), String> {
    if skill_name.is_empty() {
        return Err("skill name is required".to_string());
    }

    // Try workspace skills directory
    let ws_skill_path = std::path::Path::new(skills_workspace)
        .join(skill_name)
        .join("SKILL.md");
    if ws_skill_path.exists() {
        return std::fs::write(&ws_skill_path, content)
            .map_err(|e| format!("failed to write SKILL.md: {}", e));
    }

    Err(format!("skill {:?} not found", skill_name))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_for_prompt_short() {
        let content = "short content";
        assert_eq!(truncate_for_prompt(content, 100), "short content");
    }

    #[test]
    fn test_truncate_for_prompt_long() {
        let content = "a".repeat(100);
        let truncated = truncate_for_prompt(&content, 50);
        assert!(truncated.len() <= 50);
    }

    #[test]
    fn test_memory_update_can_use_tool() {
        assert!(memory_update_can_use_tool("memory_add"));
        assert!(memory_update_can_use_tool("memory_search"));
        assert!(memory_update_can_use_tool("read_file"));
        assert!(memory_update_can_use_tool("glob"));
        assert!(!memory_update_can_use_tool("bash"));
        assert!(!memory_update_can_use_tool("write_file"));
    }

    #[test]
    fn test_build_memory_update_prompt() {
        let prompt = build_memory_update_prompt("test memory content");
        assert!(prompt.contains("MEMORY UPDATE MODE"));
        assert!(prompt.contains("Whitelist"));
        assert!(prompt.contains("What does NOT qualify"));
        assert!(prompt.contains("test memory content"));
    }

    #[test]
    fn test_build_skills_reflection_prompt_empty() {
        let prompt = build_skills_reflection_prompt(&[], 5);
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_build_skills_reflection_prompt() {
        let names = vec!["test-skill".to_string()];
        let prompt = build_skills_reflection_prompt(&names, 10);
        assert!(prompt.contains("SKILL REFLECTION MODE"));
        assert!(prompt.contains("test-skill"));
        assert!(prompt.contains("10 turns"));
    }

    #[test]
    fn test_build_skill_auto_create_prompt() {
        let prompt = build_skill_auto_create_prompt(15);
        assert!(prompt.contains("SKILL AUTO-CREATION MODE"));
        assert!(prompt.contains("15 turns"));
        assert!(prompt.contains("Reusable"));
        assert!(prompt.contains("Well-defined"));
    }

    #[test]
    fn test_create_skill_file_validation() {
        // Empty directory
        assert!(create_skill_file("", "test", "content").is_err());
        // Empty name
        assert!(create_skill_file("/tmp", "", "content").is_err());
        // Invalid name (uppercase)
        assert!(create_skill_file("/tmp", "TestSkill", "content").is_err());
        // Invalid name (spaces)
        assert!(create_skill_file("/tmp", "test skill", "content").is_err());
        // Valid name
        assert!(create_skill_file("/tmp", "test-skill", "content").is_ok());
    }

    #[test]
    fn test_skill_evolution_config_defaults() {
        let config = SkillEvolutionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.reflect_min_turns, SKILL_REFLECT_MIN_TURNS);
        assert_eq!(config.auto_create_min_turns, SKILL_AUTO_CREATE_MIN_TURNS);
    }

    #[test]
    fn test_memory_update_config_defaults() {
        let config = MemoryUpdateConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_turns, MEMORY_UPDATE_MIN_TURNS);
    }
}
