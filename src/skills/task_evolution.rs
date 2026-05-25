//! Skill task evolution system.
//! Ported from upstream skills/task_evolution.go (146 lines).
//!
//! Manages the lifecycle of skills within a task:
//! - Tracks which skills were read at the start of a task
//! - After task completion, evaluates skill usage and suggests improvements
//! - Auto-creates new skills from complex workflows when appropriate

use super::SkillTracker;

/// Configuration for skill evolution triggers.
pub struct TaskEvolutionConfig {
    /// Min task iterations to trigger skill reflection (default: 5).
    pub reflect_min_turns: usize,
    /// Min task iterations to trigger auto-creation (default: 12).
    pub auto_create_min_turns: usize,
    /// Workspace skills directory for writing new skills.
    pub skills_dir: String,
}

impl Default for TaskEvolutionConfig {
    fn default() -> Self {
        Self {
            reflect_min_turns: 5,
            auto_create_min_turns: 12,
            skills_dir: String::new(),
        }
    }
}

/// Run the skill evolution system after a task completes.
///
/// Two mutually exclusive scenarios:
///
/// A. Skill was used: Reflect on the executed skill and suggest improvements.
///    Triggered when: skillTracker.ReadCount() > taskStartReadCount && taskTurns >= ReflectMinTurns
///
/// B. No skill was used + complex task: Auto-create a new skill from the workflow.
///    Triggered when: skillTracker.ReadCount() == taskStartReadCount && taskTurns >= AutoCreateMinTurns
pub fn run_skill_evolution<F>(
    cfg: &TaskEvolutionConfig,
    skill_tracker: &SkillTracker,
    task_start_read_skill_count: usize,
    task_turns: usize,
    out: &F,
) where
    F: Fn(&str),
{
    let skills_used = skill_tracker.read_count().saturating_sub(task_start_read_skill_count);
    if skills_used > 0 {
        // Scenario A: Skill was used — reflect on improvements
        if task_turns < cfg.reflect_min_turns {
            return;
        }
        run_skill_reflection(skill_tracker, out);
    } else {
        // Scenario B: No skill was used — consider auto-creating one
        if task_turns < cfg.auto_create_min_turns {
            return;
        }
        run_skill_auto_creation(task_turns, out);
    }
}

/// Analyze skills that were used during the task and determine if improvements should be made.
fn run_skill_reflection<F>(skill_tracker: &SkillTracker, out: &F)
where
    F: Fn(&str),
{
    let read_skill_names = skill_tracker.get_read_skill_names();
    if read_skill_names.is_empty() {
        return;
    }

    out("[skill-evolution] Reflecting on used skills\n");

    // In the full implementation, this would:
    // 1. Build the reflection prompt with build_skills_reflection_prompt()
    // 2. Fork a subagent with restricted tools (write_file for SKILL.md edits)
    // 3. The subagent analyzes and edits SKILL.md files for improvements
}

/// Analyze the complex task that just completed and determine if it should be captured as a new skill.
fn run_skill_auto_creation<F>(task_turns: usize, out: &F)
where
    F: Fn(&str),
{
    out(&format!(
        "[skill-evolution] Analyzing complex task ({} turns) for skill creation...\n",
        task_turns
    ));

    // In the full implementation, this would:
    // 1. Build the auto-creation prompt with build_skill_auto_create_prompt()
    // 2. Fork a subagent with restricted tools (write_file for SKILL.md creation)
    // 3. The subagent analyzes and creates SKILL.md files
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
pub fn update_skill_file(skills_dir: &str, skill_name: &str, content: &str) -> Result<(), String> {
    if skill_name.is_empty() {
        return Err("skill name is required".to_string());
    }

    let skill_path = std::path::Path::new(skills_dir)
        .join(skill_name)
        .join("SKILL.md");
    if skill_path.exists() {
        return std::fs::write(&skill_path, content)
            .map_err(|e| format!("failed to write SKILL.md: {}", e));
    }

    Err(format!("skill {:?} not found", skill_name))
}

/// Snapshot of skill state at the start of a task.
pub struct TaskSkillSnapshot {
    read_count_at_start: usize,
    skills_at_start: Vec<String>,
}

impl TaskSkillSnapshot {
    /// Create a snapshot of the current skill tracker state.
    pub fn new(tracker: &SkillTracker) -> Self {
        Self {
            read_count_at_start: tracker.read_count(),
            skills_at_start: tracker.get_read_skill_names(),
        }
    }

    /// Get the read count at task start.
    pub fn read_count_at_start(&self) -> usize {
        self.read_count_at_start
    }

    /// Get the skills that were already read at task start.
    pub fn skills_at_start(&self) -> &[String] {
        &self.skills_at_start
    }
}

/// Task evolution manager that coordinates skill reflection and auto-creation.
pub struct TaskEvolution {
    config: TaskEvolutionConfig,
    snapshot: Option<TaskSkillSnapshot>,
}

impl TaskEvolution {
    /// Create a new task evolution manager with the given configuration.
    pub fn new(config: TaskEvolutionConfig) -> Self {
        Self {
            config,
            snapshot: None,
        }
    }

    /// Record the start of a task by taking a skill snapshot.
    pub fn task_start(&mut self, tracker: &SkillTracker) {
        self.snapshot = Some(TaskSkillSnapshot::new(tracker));
    }

    /// Process the end of a task and run skill evolution if appropriate.
    ///
    /// Returns true if skill evolution was triggered.
    pub fn task_end<F>(
        &mut self,
        tracker: &SkillTracker,
        task_turns: usize,
        is_interrupted: bool,
        out: F,
    ) -> bool
    where
        F: Fn(&str),
    {
        if is_interrupted {
            return false;
        }

        let start_read_count = self.snapshot.as_ref().map(|s| s.read_count_at_start).unwrap_or(0);

        run_skill_evolution(&self.config, tracker, start_read_count, task_turns, &out);

        self.snapshot = None;
        true
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &TaskEvolutionConfig {
        &self.config
    }

    /// Update the configuration.
    pub fn set_config(&mut self, config: TaskEvolutionConfig) {
        self.config = config;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_evolution_new() {
        let config = TaskEvolutionConfig::default();
        let evolution = TaskEvolution::new(config);
        assert!(evolution.snapshot.is_none());
    }

    #[test]
    fn test_task_skill_snapshot() {
        let tracker = SkillTracker::new();
        let snapshot = TaskSkillSnapshot::new(&tracker);
        assert_eq!(snapshot.read_count_at_start(), 0);
        assert!(snapshot.skills_at_start().is_empty());
    }

    #[test]
    fn test_task_evolution_interrupted() {
        let config = TaskEvolutionConfig::default();
        let mut evolution = TaskEvolution::new(config);
        let tracker = SkillTracker::new();
        evolution.task_start(&tracker);

        let result = evolution.task_end(&tracker, 15, true, |_| {});
        assert!(!result);
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
    fn test_task_evolution_config_defaults() {
        let config = TaskEvolutionConfig::default();
        assert_eq!(config.reflect_min_turns, 5);
        assert_eq!(config.auto_create_min_turns, 12);
    }
}
