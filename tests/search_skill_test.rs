//! Integration tests for SearchSkillTool

use miniclaudecode_rust::skills::Loader;
use miniclaudecode_rust::tools::skill_tools::SearchSkillTool;
use miniclaudecode_rust::tools::{Tool, ToolResult};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

fn setup_skills() -> (TempDir, Arc<Loader>) {
    let dir = TempDir::new().unwrap();

    // Create weather skill
    let weather_dir = dir.path().join("weather");
    std::fs::create_dir(&weather_dir).unwrap();
    std::fs::write(
        weather_dir.join("SKILL.md"),
        "---\nname: weather\ndescription: Get current weather and forecast\ntags: api, weather, forecast\nalways: false\nversion: 1.0\n---\n\nWeather skill content.",
    ).unwrap();

    // Create hardware skill
    let hw_dir = dir.path().join("hardware");
    std::fs::create_dir(&hw_dir).unwrap();
    std::fs::write(
        hw_dir.join("SKILL.md"),
        "---\nname: hardware\ndescription: Read and control I2C and SPI peripherals\ntags: i2c, spi, hardware, embedded\nalways: false\nversion: 1.0\n---\n\nHardware skill content.",
    ).unwrap();

    // Create git-helper skill
    let git_dir = dir.path().join("git-helper");
    std::fs::create_dir(&git_dir).unwrap();
    std::fs::write(
        git_dir.join("SKILL.md"),
        "---\nname: git-helper\ndescription: Advanced git operations and workflows\ntags: git, vcs, version-control\nalways: false\nversion: 1.0\n---\n\nGit helper content.",
    ).unwrap();

    let mut loader = Loader::new(dir.path());
    loader.refresh();

    (dir, Arc::new(loader))
}

fn make_params(query: &str) -> HashMap<String, serde_json::Value> {
    let mut params = HashMap::new();
    params.insert("query".to_string(), serde_json::json!(query));
    params
}

#[test]
fn search_exact_name_match() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("weather"));
    assert!(!result.is_error);
    assert!(result.output.contains("weather"));
    assert!(result.output.contains("Get current weather"));
}

#[test]
fn search_by_tag() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("i2c"));
    assert!(!result.is_error);
    assert!(result.output.contains("hardware"));
}

#[test]
fn search_by_description() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("forecast"));
    assert!(!result.is_error);
    assert!(result.output.contains("weather"));
}

#[test]
fn search_multi_term() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("git version"));
    assert!(!result.is_error);
    assert!(result.output.contains("git-helper"));
}

#[test]
fn search_no_results() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("nonexistent-skill-xyz"));
    assert!(!result.is_error);
    assert!(result.output.contains("No skills found"));
}

#[test]
fn search_empty_query() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params(""));
    assert!(result.is_error);
}

#[test]
fn search_missing_query_param() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(HashMap::new());
    assert!(result.is_error);
}

#[test]
fn search_tool_name_and_description() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    assert_eq!(tool.name(), "search_skills");
    assert!(tool.description().contains("Search available skills"));
}

#[test]
fn search_no_skills_available() {
    let dir = TempDir::new().unwrap();
    let mut loader = Loader::new(dir.path());
    loader.refresh();
    let tool = SearchSkillTool::new(Arc::new(loader));

    let result = tool.execute(make_params("anything"));
    assert!(!result.is_error);
    assert!(result.output.contains("No skills available"));
}

#[test]
fn search_results_contain_hint() {
    let (_dir, loader) = setup_skills();
    let tool = SearchSkillTool::new(loader);

    let result = tool.execute(make_params("weather"));
    assert!(result.output.contains("read_skill"));
}
