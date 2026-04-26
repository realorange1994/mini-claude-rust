//! Integration tests for skills module

use miniclaudecode_rust::skills::{Loader, check_dependencies, parse_frontmatter};
use std::path::Path;
use tempfile::TempDir;

// ─── parse_frontmatter tests ───

#[test]
fn parse_frontmatter_basic() {
    let content = r#"---
name: test-skill
description: A test skill
always: false
version: 1.0.0
---

Some content after frontmatter.
"#;
    let meta = parse_frontmatter(content);
    assert_eq!(meta.name, "test-skill");
    assert_eq!(meta.description, "A test skill");
    assert!(!meta.always);
    assert_eq!(meta.version, "1.0.0");
}

#[test]
fn parse_frontmatter_always_true() {
    let content = r#"---
name: always-skill
description: Always active
always: true
version: 2.0.0
---

Content.
"#;
    let meta = parse_frontmatter(content);
    assert!(meta.always);
}

#[test]
fn parse_frontmatter_requires_comma() {
    let content = r#"---
name: dep-skill
description: Has deps
always: false
version: 1.0.0
requires: git, npm, node
---
"#;
    let meta = parse_frontmatter(content);
    assert_eq!(meta.requires, vec!["git", "npm", "node"]);
}

#[test]
fn parse_frontmatter_tags_comma() {
    let content = r#"---
name: tagged-skill
description: Has tags
always: false
version: 1.0.0
tags: rust, testing, tools
---
"#;
    let meta = parse_frontmatter(content);
    assert_eq!(meta.tags, vec!["rust", "testing", "tools"]);
}

#[test]
fn parse_frontmatter_requires_list() {
    let content = r#"---
name: list-deps
description: List dependencies
always: false
version: 1.0.0
requires:
  - cargo
  - rustc
---
"#;
    let meta = parse_frontmatter(content);
    assert_eq!(meta.requires, vec!["cargo", "rustc"]);
}

#[test]
fn parse_frontmatter_extended_requires() {
    let content = r#"---
name: extended-deps
description: Extended deps
always: false
version: 1.0.0
extended_requires:
  - bins: cargo, rustc
  - env: CARGO_HOME
---
"#;
    let meta = parse_frontmatter(content);
    assert_eq!(meta.extended_requires_bins, vec!["cargo", "rustc"]);
    assert_eq!(meta.extended_requires_env, vec!["CARGO_HOME"]);
}

#[test]
fn parse_frontmatter_empty_requires() {
    let content = r#"---
name: no-deps
description: No dependencies
always: false
version: 1.0.0
---
"#;
    let meta = parse_frontmatter(content);
    assert!(meta.requires.is_empty());
    assert!(meta.tags.is_empty());
    assert!(meta.extended_requires_bins.is_empty());
    assert!(meta.extended_requires_env.is_empty());
}

#[test]
fn parse_frontmatter_no_frontmatter() {
    let content = "Just plain text, no frontmatter.";
    let meta = parse_frontmatter(content);
    assert_eq!(meta.name, "");
    assert_eq!(meta.description, "");
    assert!(!meta.always);
}

#[test]
fn parse_frontmatter_missing_closing() {
    // Without closing ---, the parser still reads key-value pairs
    // because in_frontmatter is set on the opening --- but never closed
    let content = "---\nname: broken\n";
    let meta = parse_frontmatter(content);
    assert_eq!(meta.name, "broken");
}

// ─── check_dependencies tests ───

#[test]
fn check_dependencies_empty() {
    let (available, missing) = check_dependencies(&[], &[], &[], Path::new("/tmp"));
    assert!(available);
    assert!(missing.is_empty());
}

#[test]
fn check_dependencies_env_present() {
    // HOME is always present on Windows too
    std::env::set_var("TEST_SKILL_VAR", "yes");
    let (available, _missing) = check_dependencies(
        &["TEST_SKILL_VAR".to_string()],
        &[],
        &[],
        Path::new("/tmp"),
    );
    assert!(available);
    std::env::remove_var("TEST_SKILL_VAR");
}

#[test]
fn check_dependencies_env_missing() {
    let (available, missing) = check_dependencies(
        &["UNLIKELY_ENV_VAR_999".to_string()],
        &[],
        &[],
        Path::new("/tmp"),
    );
    // Note: the skill logic treats uppercase with underscore as env var
    assert!(!available);
    assert!(!missing.is_empty());
}

#[test]
fn check_dependencies_file_exists() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("myfile.txt");
    std::fs::write(&file_path, "test").unwrap();

    let (available, _missing) = check_dependencies(
        &["myfile.txt".to_string()],
        &[],
        &[],
        dir.path(),
    );
    assert!(available);
}

// ─── Loader tests ───

#[test]
fn loader_new() {
    let loader = Loader::new(Path::new("/tmp"));
    let skills = loader.list_skills();
    assert!(skills.is_empty());
}

#[test]
fn loader_empty_dir() {
    let dir = TempDir::new().unwrap();
    let mut loader = Loader::new(dir.path());
    loader.refresh();
    assert!(loader.list_skills().is_empty());
    assert!(loader.get_always_skills().is_empty());
}

#[test]
fn loader_load_single_skill() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("test-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: test-skill\ndescription: A test\nalways: false\nversion: 1.0\n---\n\nSkill content here.",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let skills = loader.list_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "test-skill");
    assert_eq!(skills[0].description, "A test");
    assert!(!skills[0].always);
}

#[test]
fn loader_load_skill_content() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: Test\nalways: false\nversion: 1.0\n---\n\nContent body.",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let content = loader.load_skill("my-skill").unwrap();
    assert!(content.contains("---"));
    assert!(content.contains("Content body."));
}

#[test]
fn loader_load_unknown_skill() {
    let dir = TempDir::new().unwrap();
    let loader = Loader::new(dir.path());
    assert!(loader.load_skill("unknown").is_none());
}

#[test]
fn loader_always_skill() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("always-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: always-skill\ndescription: Always on\nalways: true\nversion: 1.0\n---\n",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let always_skills = loader.get_always_skills();
    assert_eq!(always_skills.len(), 1);
    assert_eq!(always_skills[0].name, "always-skill");
}

#[test]
fn loader_multiple_skills() {
    let dir = TempDir::new().unwrap();

    for name in &["skill-a", "skill-b", "skill-c"] {
        let skill_dir = dir.path().join(name);
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {}\ndescription: Test\nalways: false\nversion: 1.0\n---\n", name),
        ).unwrap();
    }

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let skills = loader.list_skills();
    assert_eq!(skills.len(), 3);
}

#[test]
fn loader_skill_not_a_directory_ignored() {
    let dir = TempDir::new().unwrap();
    // Create a file, not a directory
    std::fs::write(dir.path().join("not-a-skill"), "content").unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();
    assert!(loader.list_skills().is_empty());
}

#[test]
fn loader_missing_skill_md() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("no-skill-md");
    std::fs::create_dir(&skill_dir).unwrap();
    // No SKILL.md file

    let mut loader = Loader::new(dir.path());
    loader.refresh();
    assert!(loader.list_skills().is_empty());
}

#[test]
fn loader_build_system_prompt() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("helper");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: helper\ndescription: Helper skill\nalways: false\nversion: 1.0\n---\n\nHelper content.",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let prompt = loader.build_system_prompt_for_skills(&["helper".to_string()]);
    assert!(prompt.contains("## Active Skills"));
    assert!(prompt.contains("**helper**"));
    assert!(prompt.contains("Helper skill"));
    // Should NOT contain full SKILL.md content (progressive disclosure)
    assert!(!prompt.contains("Helper content."));
}

#[test]
fn loader_build_system_prompt_unknown_skill() {
    let dir = TempDir::new().unwrap();
    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let prompt = loader.build_system_prompt_for_skills(&["unknown".to_string()]);
    // Unknown skills are silently skipped
    assert!(!prompt.contains("unknown"));
}

#[test]
fn loader_build_system_prompt_empty() {
    let dir = TempDir::new().unwrap();
    let loader = Loader::new(dir.path());
    let prompt = loader.build_system_prompt_for_skills(&[]);
    assert!(prompt.is_empty());
}

#[test]
fn loader_build_skills_summary() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("summary-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: summary-skill\ndescription: For summary\nalways: false\nversion: 1.0\n---\n",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let summary = loader.build_skills_summary();
    assert!(summary.contains("## Available Skills"));
    assert!(summary.contains("**summary-skill**"));
    assert!(summary.contains("For summary"));
    assert!(summary.contains("read_skill"));
}

#[test]
fn loader_build_skills_summary_empty() {
    let dir = TempDir::new().unwrap();
    let loader = Loader::new(dir.path());
    let summary = loader.build_skills_summary();
    assert!(summary.is_empty());
}

#[test]
fn loader_clone() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join("clone-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: clone-skill\ndescription: Clone test\nalways: true\nversion: 1.0\n---\n",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    let cloned = loader.clone();
    let skills = cloned.list_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "clone-skill");
}

#[test]
fn loader_set_builtin_dir() {
    let dir = TempDir::new().unwrap();
    let builtin = dir.path().join("builtin");
    std::fs::create_dir(&builtin).unwrap();

    let skill_dir = builtin.join("builtin-skill");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: builtin-skill\ndescription: Builtin\nalways: false\nversion: 1.0\n---\n",
    ).unwrap();

    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let mut loader = Loader::new(&workspace);
    loader.set_builtin_dir(&builtin);
    loader.refresh();

    let skills = loader.list_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].source, "builtin");
}

#[test]
fn loader_workspace_overrides_builtin() {
    let dir = TempDir::new().unwrap();
    let builtin = dir.path().join("builtin");
    std::fs::create_dir(&builtin).unwrap();
    let builtin_skill = builtin.join("shared-skill");
    std::fs::create_dir(&builtin_skill).unwrap();
    std::fs::write(
        builtin_skill.join("SKILL.md"),
        "---\nname: shared-skill\ndescription: Builtin version\nalways: false\nversion: 1.0\n---\n",
    ).unwrap();

    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ws_skill = workspace.join("shared-skill");
    std::fs::create_dir(&ws_skill).unwrap();
    std::fs::write(
        ws_skill.join("SKILL.md"),
        "---\nname: shared-skill\ndescription: Workspace version\nalways: true\nversion: 2.0\n---\n",
    ).unwrap();

    let mut loader = Loader::new(&workspace);
    loader.set_builtin_dir(&builtin);
    loader.refresh();

    let skills = loader.list_skills();
    assert_eq!(skills.len(), 1);
    // Workspace should override builtin (same directory name)
    assert_eq!(skills[0].description, "Workspace version");
}

// ─── SkillMeta default tests ───

#[test]
fn skill_meta_defaults() {
    let meta = parse_frontmatter("---\n---\n");
    assert_eq!(meta.name, "");
    assert_eq!(meta.description, "");
    assert!(!meta.always);
    assert!(meta.requires.is_empty());
    assert!(meta.tags.is_empty());
}
