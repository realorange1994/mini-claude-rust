//! Internal paths - Check Claude internal paths (plans, scratchpad, memory, etc.)

use std::path::Path;

/// Check if a path is Claude Code's internal editable path
/// that should bypass dangerous-directory checks.
pub fn is_internal_editable_path(path: &str, cwd: &str) -> bool {
    let abs = resolve_path(path, cwd);

    if is_plan_file(&abs) { return true; }
    if is_in_scratchpad_dir(&abs) { return true; }
    if is_in_jobs_dir(&abs) { return true; }
    if is_agent_memory_path(&abs) { return true; }
    if is_in_auto_memory_dir(&abs) { return true; }
    if is_launch_config(&abs, cwd) { return true; }

    false
}

/// Check if a path is Claude Code's internal readable path.
pub fn is_internal_readable_path(path: &str) -> bool {
    let abs = resolve_path(path, "");

    if is_in_session_memory_dir(&abs) { return true; }
    if is_in_projects_dir(&abs) { return true; }
    if is_plan_file(&abs) { return true; }
    if is_in_tool_results_dir(&abs) { return true; }
    if is_in_scratchpad_dir(&abs) { return true; }
    if is_in_project_temp_dir(&abs) { return true; }
    if is_agent_memory_path(&abs) { return true; }
    if is_in_auto_memory_dir(&abs) { return true; }
    if is_in_tasks_dir(&abs) { return true; }
    if is_in_teams_dir(&abs) { return true; }
    if is_in_bundled_skills_dir(&abs) { return true; }

    false
}

fn home_dir() -> Option<String> {
    std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
}

fn is_plan_file(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let plans_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("plans");
        if abs.starts_with(plans_dir.to_str().unwrap_or("")) {
            return std::path::Path::new(abs).extension().map_or(false, |e| e == "md");
        }
    }
    false
}

fn is_in_scratchpad_dir(abs: &str) -> bool {
    abs.contains("scratchpad")
}

fn is_in_jobs_dir(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let jobs_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("jobs");
        return abs.starts_with(jobs_dir.to_str().unwrap_or(""));
    }
    false
}

fn is_agent_memory_path(abs: &str) -> bool {
    abs.contains(".claude") && abs.contains("projects") && abs.contains("agent-memory")
}

fn is_in_auto_memory_dir(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let memory_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("auto-memory");
        return abs.starts_with(memory_dir.to_str().unwrap_or(""));
    }
    false
}

fn is_launch_config(abs: &str, cwd: &str) -> bool {
    if cwd.is_empty() { return false; }
    let launch_config = std::path::Path::new(cwd)
        .join(".claude")
        .join("launch.json");
    abs == launch_config.to_str().unwrap_or("")
}

fn is_in_session_memory_dir(abs: &str) -> bool {
    abs.contains("session-memory")
}

fn is_in_projects_dir(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let proj_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("projects");
        return abs.starts_with(proj_dir.to_str().unwrap_or(""));
    }
    false
}

fn is_in_tool_results_dir(abs: &str) -> bool {
    abs.contains("tool-results")
}

fn is_in_project_temp_dir(abs: &str) -> bool {
    if cfg!(windows) {
        let tmp = std::env::temp_dir();
        abs.starts_with(tmp.to_str().unwrap_or("")) && abs.contains("claude-")
    } else {
        abs.starts_with("/tmp/claude-")
    }
}

fn is_in_tasks_dir(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let tasks_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("tasks");
        return abs.starts_with(tasks_dir.to_str().unwrap_or(""));
    }
    false
}

fn is_in_teams_dir(abs: &str) -> bool {
    if let Some(home) = home_dir() {
        let teams_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("teams");
        return abs.starts_with(teams_dir.to_str().unwrap_or(""));
    }
    false
}

fn is_in_bundled_skills_dir(abs: &str) -> bool {
    abs.contains("bundled-skills")
}

fn resolve_path(path: &str, cwd: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string());
    }
    let base = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        std::path::PathBuf::from(cwd)
    };
    base.join(p)
        .to_string_lossy()
        .to_string()
}
