//! System prompt builder

use crate::config::Config;
use crate::tools::Registry;
use crate::skills::Loader;
use std::path::Path;

/// Build the system prompt for the agent
pub fn build_system_prompt(
    registry: &Registry,
    permission_mode: &crate::permissions::PermissionMode,
    project_dir: &Path,
    skill_loader: Option<&Loader>,
) -> String {
    let mut prompt = String::new();

    prompt.push_str(r#"You are miniClaudeCode, a lightweight AI coding assistant that operates in the terminal.

## Environment
- OS: "#);
    prompt.push_str(std::env::consts::OS);
    prompt.push_str("\n- Working Directory: ");
    if let Ok(cwd) = std::env::current_dir() {
        prompt.push_str(&cwd.display().to_string());
    }
    prompt.push_str(
        r#"
- Shell: PowerShell on Windows, sh/bash on Unix

You have access to the following tools to help you with software engineering tasks:
"#,
    );

    // Add tool list
    for tool in registry.all_tools() {
        prompt.push_str("- **");
        prompt.push_str(tool.name());
        prompt.push_str("**: ");
        prompt.push_str(tool.description());
        prompt.push('\n');
    }

    // Add permission mode description
    prompt.push_str("\n## Operating Rules\n\n");
    prompt.push_str("1. Always read a file before editing it.\n");
    prompt.push_str("2. Use tools to accomplish tasks -- don't just describe what to do.\n");
    prompt.push_str("3. When running bash commands, prefer non-destructive read operations.\n");
    prompt.push_str("4. For file edits, provide enough context in old_string to uniquely match.\n");
    prompt.push_str("5. Be concise and direct in your responses.\n");
    prompt.push_str("6. On Windows, use PowerShell syntax and commands (e.g., Get-ChildItem, Test-Path, Copy-Item).\n");
    prompt.push_str("7. Use git directly for git operations -- it is available in the PATH.\n\n");

    // Add permission mode
    prompt.push_str("## Current Permission Mode: ");
    prompt.push_str(&permission_mode.to_string().to_uppercase());
    prompt.push_str("\n\n");

    match permission_mode {
        crate::permissions::PermissionMode::Ask => {
            prompt.push_str("In ASK mode, potentially dangerous operations will require user confirmation.");
        }
        crate::permissions::PermissionMode::Auto => {
            prompt.push_str("In AUTO mode, all operations are auto-approved (use with caution).");
        }
        crate::permissions::PermissionMode::Plan => {
            prompt.push_str("In PLAN mode, only read-only operations are allowed. Write operations are blocked.");
        }
    }
    prompt.push('\n');

    // Load project instructions from CLAUDE.md
    let claude_md = project_dir.join("CLAUDE.md");
    if claude_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&claude_md) {
            prompt.push_str("\n## Project Instructions (from CLAUDE.md)\n\n");
            prompt.push_str(content.trim());
            prompt.push('\n');
        }
    }

    // Add skills section
    if let Some(loader) = skill_loader {
        let always_skills = loader.get_always_skills();
        if !always_skills.is_empty() {
            prompt.push_str("\n## Active Skills\n\n");
            for skill in always_skills {
                if let Some(content) = loader.load_skill(&skill.name) {
                    prompt.push_str("### Skill: ");
                    prompt.push_str(&skill.name);
                    prompt.push_str("\n\n");
                    prompt.push_str(&content);
                    prompt.push_str("\n\n");
                }
            }
        }

        let summary = loader.build_skills_summary();
        if !summary.is_empty() {
            prompt.push_str("\n## Available Skills\n\n");
            prompt.push_str(&summary);
            prompt.push('\n');
        }
    }

    prompt
}
