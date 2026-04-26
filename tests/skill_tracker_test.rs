//! Integration tests for SkillTracker

use miniclaudecode_rust::skills::{SkillTracker, SkillInfo, parse_frontmatter};
use std::path::PathBuf;

fn make_skill_info(name: &str, desc: &str, always: bool, available: bool) -> SkillInfo {
    SkillInfo {
        name: name.to_string(),
        path: PathBuf::from(format!("/tmp/{}", name)),
        source: "workspace".to_string(),
        available,
        always,
        description: desc.to_string(),
        commands: vec![],
        tags: vec![],
        version: "1.0".to_string(),
        missing_deps: vec![],
        when_to_use: None,
    }
}

#[test]
fn tracker_new_is_empty() {
    let tracker = SkillTracker::new();
    assert!(tracker.is_new_skill("anything"));
    assert!(!tracker.was_read("anything"));
    assert!(!tracker.was_used("anything"));
}

#[test]
fn tracker_mark_shown() {
    let mut tracker = SkillTracker::new();
    assert!(tracker.is_new_skill("skill-a"));
    tracker.mark_shown("skill-a");
    assert!(!tracker.is_new_skill("skill-a"));
}

#[test]
fn tracker_mark_read() {
    let mut tracker = SkillTracker::new();
    assert!(!tracker.was_read("skill-a"));
    tracker.mark_read("skill-a");
    assert!(tracker.was_read("skill-a"));
}

#[test]
fn tracker_mark_used() {
    let mut tracker = SkillTracker::new();
    assert!(!tracker.was_used("skill-a"));
    tracker.mark_used("skill-a");
    assert!(tracker.was_used("skill-a"));
}

#[test]
fn tracker_independent_states() {
    let mut tracker = SkillTracker::new();
    tracker.mark_shown("a");
    tracker.mark_read("b");
    tracker.mark_used("c");

    assert!(!tracker.is_new_skill("a"));
    assert!(tracker.is_new_skill("b"));
    assert!(tracker.is_new_skill("c"));

    assert!(!tracker.was_read("a"));
    assert!(tracker.was_read("b"));
    assert!(!tracker.was_read("c"));

    assert!(!tracker.was_used("a"));
    assert!(!tracker.was_used("b"));
    assert!(tracker.was_used("c"));
}

#[test]
fn tracker_get_unsent_skills() {
    let mut tracker = SkillTracker::new();
    tracker.mark_shown("shown-skill");

    let skills = vec![
        make_skill_info("shown-skill", "Already shown", false, true),
        make_skill_info("new-skill", "Not yet shown", false, true),
        make_skill_info("always-skill", "Always on", true, true),
    ];

    let unsent = tracker.get_unsent_skills(&skills);
    let unsent_names: Vec<&str> = unsent.iter().map(|s| s.name.as_str()).collect();

    // always-skill is included (always filter), new-skill is included (new)
    // shown-skill is excluded (already shown)
    assert!(unsent_names.contains(&"new-skill"));
    assert!(unsent_names.contains(&"always-skill"));
    assert!(!unsent_names.contains(&"shown-skill"));
}

#[test]
fn tracker_discovery_reminder_with_unsent() {
    let tracker = SkillTracker::new();
    let skills = vec![
        make_skill_info("new-skill", "Not yet shown", false, true),
    ];

    let reminder = tracker.generate_discovery_reminder(&skills);
    assert!(reminder.is_some());
    let text = reminder.unwrap();
    assert!(text.contains("1 unread skill"));
    assert!(text.contains("search_skills"));
    assert!(text.contains("read_skill"));
}

#[test]
fn tracker_discovery_reminder_no_unsent() {
    let mut tracker = SkillTracker::new();
    tracker.mark_shown("skill-a");

    let skills = vec![
        make_skill_info("skill-a", "Already shown", false, true),
    ];

    let reminder = tracker.generate_discovery_reminder(&skills);
    assert!(reminder.is_none());
}

#[test]
fn tracker_clone() {
    let mut tracker = SkillTracker::new();
    tracker.mark_shown("a");
    tracker.mark_read("b");

    let cloned = tracker.clone();
    assert!(!cloned.is_new_skill("a"));
    assert!(cloned.was_read("b"));
}

#[test]
fn tracker_default() {
    let tracker = SkillTracker::default();
    assert!(tracker.is_new_skill("anything"));
}

#[test]
fn tracker_read_used_counts() {
    let mut tracker = SkillTracker::new();
    assert_eq!(tracker.read_count(), 0);
    assert_eq!(tracker.used_count(), 0);

    tracker.mark_read("a");
    tracker.mark_read("b");
    tracker.mark_used("c");
    assert_eq!(tracker.read_count(), 2);
    assert_eq!(tracker.used_count(), 1);
}
