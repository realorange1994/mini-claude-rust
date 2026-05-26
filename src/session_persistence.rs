//! Session persistence for conversation state.
//! Ported from upstream session_persistence.go (419 lines).
//!
//! Serializes the full conversation to JSON on exit, restores on next launch.
//! Session files are stored at {projectDir}/.claude/sessions/{sessionID}.json.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/// JSON-serializable snapshot of a full conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedConversation {
    pub session_id: String,
    pub model: String,
    pub permission_mode: String,
    pub created_at: String,
    pub updated_at: String,
    pub working_dir: String,
    #[serde(default)]
    pub entries: Vec<Value>,
    #[serde(default)]
    pub compression_level: i64,
    #[serde(default)]
    pub total_input_tokens: i64,
    #[serde(default)]
    pub total_output_tokens: i64,
    #[serde(default)]
    pub total_cache_read_tokens: i64,
    #[serde(default)]
    pub total_cache_creation_tokens: i64,
}

/// Returns the directory for session files.
pub fn sessions_dir(project_dir: &str) -> PathBuf {
    PathBuf::from(project_dir).join(".claude").join("sessions")
}

/// Save a serialized conversation to disk.
/// Returns the path of the saved file.
pub fn save_conversation(
    project_dir: &str,
    session_id: &str,
    model: &str,
    permission_mode: &str,
    messages: Vec<Value>,
    compression_level: i64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_read_tokens: i64,
    total_cache_creation_tokens: i64,
) -> Result<String, String> {
    if project_dir.is_empty() || session_id.is_empty() {
        return Err("project_dir and session_id are required".to_string());
    }

    let dir = sessions_dir(project_dir);
    if let Err(e) = fs::create_dir_all(&dir) {
        return Err(format!("failed to create sessions directory: {}", e));
    }

    let now = chrono::Utc::now().to_rfc3339();

    let path = dir.join(format!("{}.json", session_id));
    let existing_created_at = if path.exists() {
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(existing) = serde_json::from_str::<SerializedConversation>(&data) {
                if !existing.created_at.is_empty() {
                    Some(existing.created_at)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let created_at = existing_created_at.unwrap_or(now.clone());

    let snap = SerializedConversation {
        session_id: session_id.to_string(),
        model: model.to_string(),
        permission_mode: permission_mode.to_string(),
        created_at,
        updated_at: now,
        working_dir: project_dir.to_string(),
        entries: messages,
        compression_level,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_creation_tokens,
    };

    let data = serde_json::to_string_pretty(&snap)
        .map_err(|e| format!("failed to marshal conversation: {}", e))?;

    fs::write(&path, data)
        .map_err(|e| format!("failed to write session file: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

/// Load a serialized conversation from disk.
pub fn load_conversation(
    project_dir: &str,
    session_id: &str,
) -> Result<SerializedConversation, String> {
    let dir = sessions_dir(project_dir);
    let path = dir.join(format!("{}.json", session_id));

    let data = fs::read_to_string(&path)
        .map_err(|e| format!("session file not found: {}", e))?;

    serde_json::from_str(&data)
        .map_err(|e| format!("failed to parse session file: {}", e))
}

/// List all saved sessions, sorted by updated_at (most recent first).
pub fn list_sessions(project_dir: &str) -> Vec<SerializedConversation> {
    let dir = sessions_dir(project_dir);
    if !dir.exists() {
        return Vec::new();
    }

    let mut sessions = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().map_or(true, |ext| ext != "json") {
            continue;
        }
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(snap) = serde_json::from_str::<SerializedConversation>(&data) {
                sessions.push(snap);
            }
        }
    }

    // Sort by updated_at descending
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

/// Delete a session by session_id.
pub fn delete_session(project_dir: &str, session_id: &str) -> Result<(), String> {
    let dir = sessions_dir(project_dir);
    let path = dir.join(format!("{}.json", session_id));
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("failed to delete session file: {}", e))?;
    }
    Ok(())
}

/// Convenience wrapper: save a conversation snapshot from an AgentLoop.
/// Extracts all relevant fields from the agent.
pub fn save_conversation_from_agent<L: AgentLoopSnapshot>(
    project_dir: &str,
    session_id: &str,
    agent: &L,
) -> Result<String, String> {
    let snap = agent.snapshot(session_id, project_dir);
    save_conversation_snapshot(project_dir, snap)
}

/// Save a pre-built snapshot to disk.
pub fn save_conversation_snapshot(
    project_dir: &str,
    snap: SerializedConversation,
) -> Result<String, String> {
    if project_dir.is_empty() || snap.session_id.is_empty() {
        return Err("project_dir and session_id are required".to_string());
    }

    let dir = sessions_dir(project_dir);
    if let Err(e) = fs::create_dir_all(&dir) {
        return Err(format!("failed to create sessions directory: {}", e));
    }

    let path = dir.join(format!("{}.json", snap.session_id));

    let data = serde_json::to_string_pretty(&snap)
        .map_err(|e| format!("failed to marshal conversation: {}", e))?;

    fs::write(&path, data)
        .map_err(|e| format!("failed to write session file: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

/// Trait for extracting a conversation snapshot from an agent loop.
/// Implemented in main.rs to avoid circular dependency.
pub trait AgentLoopSnapshot {
    fn snapshot(&self, session_id: &str, project_dir: &str) -> SerializedConversation;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_load() {
        let tmp = std::env::temp_dir().join("test_session_save");
        let project_dir = tmp.to_string_lossy().to_string();

        let messages = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hello"}]}),
        ];

        let path = save_conversation(
            &project_dir,
            "test_session_123",
            "claude-sonnet-4-6-20250514",
            "ask",
            messages,
            0,
            100,
            50,
            10,
            20,
        ).unwrap();

        assert!(std::path::Path::new(&path).exists());

        let snap = load_conversation(&project_dir, "test_session_123").unwrap();
        assert_eq!(snap.session_id, "test_session_123");
        assert_eq!(snap.model, "claude-sonnet-4-6-20250514");
        assert_eq!(snap.permission_mode, "ask");
        assert_eq!(snap.total_input_tokens, 100);
        assert_eq!(snap.total_output_tokens, 50);
        assert_eq!(snap.entries.len(), 2);

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_list_sessions() {
        let tmp = std::env::temp_dir().join("test_session_list");
        let project_dir = tmp.to_string_lossy().to_string();

        // Save two sessions
        save_conversation(&project_dir, "session_a", "model_a", "ask", vec![], 0, 0, 0, 0, 0).unwrap();
        save_conversation(&project_dir, "session_b", "model_b", "auto", vec![], 0, 0, 0, 0, 0).unwrap();

        let sessions = list_sessions(&project_dir);
        assert!(sessions.len() >= 2);

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_delete_session() {
        let tmp = std::env::temp_dir().join("test_session_delete");
        let project_dir = tmp.to_string_lossy().to_string();

        save_conversation(&project_dir, "to_delete", "model", "ask", vec![], 0, 0, 0, 0, 0).unwrap();
        assert!(load_conversation(&project_dir, "to_delete").is_ok());

        delete_session(&project_dir, "to_delete").unwrap();
        assert!(load_conversation(&project_dir, "to_delete").is_err());

        let _ = fs::remove_dir_all(&tmp);
    }
}
