//! SkillTracker - tracks which skills have been shown/read/used across agent turns

use crate::skills::SkillInfo;
use std::collections::HashSet;

/// Tracks skill visibility and usage across agent loop turns.
/// Derives Clone so it can be shared via Arc<RwLock<SkillTracker>>.
#[derive(Debug, Clone, Default)]
pub struct SkillTracker {
    /// Skills already announced in system prompt
    shown_skills: HashSet<String>,
    /// Skills the model has read via read_skill tool
    read_skills: HashSet<String>,
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

    /// Mark a skill as read by the model (read_skill tool called)
    pub fn mark_read(&mut self, name: &str) {
        self.read_skills.insert(name.to_string());
    }

    /// Mark a skill as used (model performed actions after reading it)
    pub fn mark_used(&mut self, name: &str) {
        self.used_skills.insert(name.to_string());
    }

    /// Check if a specific skill was read
    pub fn was_read(&self, name: &str) -> bool {
        self.read_skills.contains(name)
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
}
