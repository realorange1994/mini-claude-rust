//! SkillTracker - tracks which skills have been shown/read/used across agent turns

use crate::skills::SkillInfo;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Tracks skill visibility and usage across agent loop turns.
/// Derives Clone so it can be shared via Arc<RwLock<SkillTracker>>.
#[derive(Debug, Clone, Default)]
pub struct SkillTracker {
    /// Skills already announced in system prompt
    shown_skills: HashSet<String>,
    /// Skills the model has read via read_skill tool, with timestamp
    read_skills: HashMap<String, Instant>,
    /// Skills the model has actively used after reading
    used_skills: HashSet<String>,
}

impl SkillTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if the skill has NOT been shown in system prompt yet
    pub fn is_new_skill(&self, name: &str) -> bool {
        !self.shown_skills.contains(name)
    }

    /// Mark a skill as announced in system prompt
    pub fn mark_shown(&mut self, name: &str) {
        self.shown_skills.insert(name.to_string());
    }

    /// Mark a skill as read by the model (read_skill tool called).
    /// Records the current timestamp for time-based ordering during post-compact recovery.
    pub fn mark_read(&mut self, name: &str) {
        self.read_skills.insert(name.to_string(), Instant::now());
    }

    /// Mark a skill as used (model performed actions after reading it)
    pub fn mark_used(&mut self, name: &str) {
        self.used_skills.insert(name.to_string());
    }

    /// Check if a specific skill was read
    pub fn was_read(&self, name: &str) -> bool {
        self.read_skills.contains_key(name)
    }

    /// Check if a specific skill was used
    pub fn was_used(&self, name: &str) -> bool {
        self.used_skills.contains(name)
    }

    /// Return names of skills that haven't been shown yet
    pub fn get_unsent_names(&self) -> Vec<String> {
        self.shown_skills.iter().cloned().collect()
    }

    /// Return SkillInfo for skills not yet shown in system prompt
    pub fn get_unsent_skills<'a>(&self, all_skills: &'a [SkillInfo]) -> Vec<&'a SkillInfo> {
        all_skills
            .iter()
            .filter(|s| s.always || self.is_new_skill(&s.name))
            .collect()
    }

    /// Generate a discovery reminder if there are unread skills
    pub fn generate_discovery_reminder(&self, all_skills: &[SkillInfo]) -> Option<String> {
        let unsent_count = all_skills
            .iter()
            .filter(|s| !s.always && self.is_new_skill(&s.name))
            .count();

        if unsent_count == 0 {
            return None;
        }

        Some(format!(
            "You have {} unread skill(s). Use search_skills to find skills by topic, or list_skills to see all available skills.\nUse read_skill to load a skill's full instructions.",
            unsent_count
        ))
    }

    /// Get count of skills read
    #[allow(dead_code)]
    pub fn read_count(&self) -> usize {
        self.read_skills.len()
    }

    /// Get count of skills used
    #[allow(dead_code)]
    pub fn used_count(&self) -> usize {
        self.used_skills.len()
    }

    /// Get names of skills that have been read, sorted by read time descending
    /// (most recently read first). Matches upstream's invokedAt-based ordering.
    pub fn get_read_skill_names(&self) -> Vec<String> {
        let mut entries: Vec<_> = self.read_skills.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1)); // most recent first
        entries.into_iter().map(|(name, _)| name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillInfo;
    use std::path::PathBuf;

    fn make_skill(name: &str, always: bool) -> SkillInfo {
        SkillInfo {
            name: name.to_string(),
            path: PathBuf::from(format!("skills/{}.md", name)),
            source: "builtin".to_string(),
            available: true,
            always,
            description: format!("{} skill", name),
            commands: vec![],
            tags: vec![],
            version: String::new(),
            missing_deps: vec![],
            when_to_use: None,
        }
    }

    #[test]
    fn test_is_new_skill() {
        let mut tracker = SkillTracker::new();
        assert!(tracker.is_new_skill("commit"), "skill should be new initially");
        tracker.mark_shown("commit");
        assert!(!tracker.is_new_skill("commit"), "skill should not be new after mark_shown");
    }

    #[test]
    fn test_mark_read() {
        let mut tracker = SkillTracker::new();
        assert!(!tracker.was_read("review"), "skill should not be read initially");
        tracker.mark_read("review");
        assert!(tracker.was_read("review"), "skill should be read after mark_read");
    }

    #[test]
    fn test_mark_used() {
        let mut tracker = SkillTracker::new();
        assert!(!tracker.was_used("simplify"), "skill should not be used initially");
        tracker.mark_used("simplify");
        assert!(tracker.was_used("simplify"), "skill should be used after mark_used");
    }

    #[test]
    fn test_get_unsent_skills() {
        let tracker = SkillTracker::new();
        let all_skills = vec![
            make_skill("commit", false),
            make_skill("review", false),
            make_skill("simplify", false),
        ];
        let unsent = tracker.get_unsent_skills(&all_skills);
        assert_eq!(unsent.len(), 3, "all skills should be unsent initially");

        let mut tracker = SkillTracker::new();
        tracker.mark_shown("commit");
        let unsent = tracker.get_unsent_skills(&all_skills);
        assert_eq!(unsent.len(), 2, "only un-shown skills should appear");
        assert!(unsent.iter().all(|s| s.name != "commit"), "commit should be excluded");
    }

    #[test]
    fn test_tracker_lifecycle() {
        let mut tracker = SkillTracker::new();
        let skill = "commit";

        // Stage 1: brand new skill
        assert!(tracker.is_new_skill(skill));
        assert!(!tracker.was_read(skill));
        assert!(!tracker.was_used(skill));

        // Stage 2: shown in system prompt
        tracker.mark_shown(skill);
        assert!(!tracker.is_new_skill(skill));
        assert!(!tracker.was_read(skill));
        assert!(!tracker.was_used(skill));

        // Stage 3: model reads the skill
        tracker.mark_read(skill);
        assert!(!tracker.is_new_skill(skill));
        assert!(tracker.was_read(skill));
        assert!(!tracker.was_used(skill));

        // Stage 4: model uses the skill
        tracker.mark_used(skill);
        assert!(!tracker.is_new_skill(skill));
        assert!(tracker.was_read(skill));
        assert!(tracker.was_used(skill));
    }

    #[test]
    fn test_read_skill_names_sorted_by_time() {
        let mut tracker = SkillTracker::new();
        tracker.mark_read("first");
        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(1));
        tracker.mark_read("second");
        std::thread::sleep(std::time::Duration::from_millis(1));
        tracker.mark_read("third");

        let names = tracker.get_read_skill_names();
        assert_eq!(names.len(), 3);
        // Most recently read should be first
        assert_eq!(names[0], "third");
        assert_eq!(names[1], "second");
        assert_eq!(names[2], "first");
    }
}
