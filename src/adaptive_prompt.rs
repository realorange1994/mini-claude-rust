//! Adaptive prompt instructions based on user task mode detection.
//! Ported from upstream adaptive_prompt.go (89 lines).
//!
//! Detects the task mode from a user's message via keyword matching
//! and returns mode-specific behavioral instructions.

/// Task mode detected from user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMode {
    Debug,
    Refactor,
    Create,
    Search,
    General,
}

impl std::fmt::Display for TaskMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskMode::Debug => write!(f, "debug"),
            TaskMode::Refactor => write!(f, "refactor"),
            TaskMode::Create => write!(f, "create"),
            TaskMode::Search => write!(f, "search"),
            TaskMode::General => write!(f, "general"),
        }
    }
}

/// Detect the task mode from a user message via keyword matching.
pub fn detect_task_mode(message: &str) -> TaskMode {
    let lower = message.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    // Debug mode
    if words.iter().any(|w| matches!(*w, "debug" | "fix" | "bug" | "error" | "crash" | "broken" | "fails" | "why"))
        && words.iter().any(|w| matches!(*w, "not" | "won't" | "doesn't" | "didn't" | "isn't" | "can't"))
    {
        return TaskMode::Debug;
    }

    // Refactor mode
    if words.iter().any(|w| matches!(*w, "refactor" | "cleanup" | "clean" | "simplify" | "reorganize" | "restructure" | "improve"))
    {
        return TaskMode::Refactor;
    }

    // Create mode
    if words.iter().any(|w| matches!(*w, "create" | "build" | "write" | "implement" | "add" | "new" | "make" | "generate"))
    {
        return TaskMode::Create;
    }

    // Search mode
    if words.iter().any(|w| matches!(*w, "find" | "search" | "look" | "where" | "locate" | "show" | "grep"))
    {
        return TaskMode::Search;
    }

    TaskMode::General
}

/// Get mode-specific adaptive instructions.
pub fn adaptive_task_instructions(mode: TaskMode) -> &'static str {
    match mode {
        TaskMode::Debug => {
            "You are in DEBUG mode. Focus on root cause analysis:
1. Reproduce the issue first to understand it
2. Read the relevant code carefully, trace the execution path
3. Identify the root cause, not just the symptom
4. Fix the minimal code change that addresses the root cause
5. Verify the fix works and doesn't break anything else
Do not make unrelated changes."
        }
        TaskMode::Refactor => {
            "You are in REFACTOR mode. Focus on improving code quality:
1. Understand the current code structure first
2. Identify code smells, duplication, or complexity
3. Make small, incremental changes that preserve behavior
4. Run tests after each change to verify nothing breaks
5. Keep the refactoring focused - don't add new features
Do not change public APIs or break existing tests."
        }
        TaskMode::Create => {
            "You are in CREATE mode. Focus on building new functionality:
1. Understand the requirements fully before writing code
2. Follow the existing code style and architecture patterns
3. Write clean, well-structured code with appropriate abstractions
4. Add tests for the new functionality
5. Update relevant documentation
Make sure the new code integrates cleanly with the existing codebase."
        }
        TaskMode::Search => {
            "You are in SEARCH mode. Focus on finding information efficiently:
1. Use the most targeted search approach (specific patterns, not broad)
2. Read only the files that are directly relevant
3. Summarize findings concisely
4. Avoid reading or editing files that don't contribute to the answer
Do not make changes unless specifically asked."
        }
        TaskMode::General => {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_debug_mode() {
        assert_eq!(detect_task_mode("why doesn't this work? I get an error"), TaskMode::Debug);
        assert_eq!(detect_task_mode("fix the bug that crashes"), TaskMode::Debug);
    }

    #[test]
    fn test_detect_refactor_mode() {
        assert_eq!(detect_task_mode("refactor this function to be cleaner"), TaskMode::Refactor);
        assert_eq!(detect_task_mode("cleanup and simplify the code"), TaskMode::Refactor);
    }

    #[test]
    fn test_detect_create_mode() {
        assert_eq!(detect_task_mode("create a new endpoint for user login"), TaskMode::Create);
        assert_eq!(detect_task_mode("implement a sorting algorithm"), TaskMode::Create);
    }

    #[test]
    fn test_detect_search_mode() {
        assert_eq!(detect_task_mode("find where the config is loaded"), TaskMode::Search);
        assert_eq!(detect_task_mode("search for all usages of this function"), TaskMode::Search);
    }

    #[test]
    fn test_detect_general_mode() {
        assert_eq!(detect_task_mode("hello, how are you?"), TaskMode::General);
        assert_eq!(detect_task_mode("explain how this works"), TaskMode::General);
    }

    #[test]
    fn test_adaptive_instructions_not_empty() {
        for mode in &[TaskMode::Debug, TaskMode::Refactor, TaskMode::Create, TaskMode::Search] {
            assert!(!adaptive_task_instructions(*mode).is_empty());
        }
        assert!(adaptive_task_instructions(TaskMode::General).is_empty());
    }
}
