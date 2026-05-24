use std::path::PathBuf;
use std::time::SystemTime;

/// SidechainTranscript manages transcript recording for sub-agents.
/// It records the sub-agent's conversation separately from the parent,
/// while maintaining a link back to the parent's entry via ParentUUID.
#[derive(Debug)]
pub struct SidechainTranscript {
    pub parent_uuid: String,
    pub path: PathBuf,
}

impl SidechainTranscript {
    /// Creates a sidechain transcript for a sub-agent.
    /// The transcript file is named sidechain-<agentName>-<timestamp>.jsonl
    /// and placed in the sessionDir.
    pub fn new(parent_uuid: &str, session_dir: &str, agent_name: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!("sidechain-{}-{}.jsonl", agent_name, timestamp);
        let path = PathBuf::from(session_dir).join(filename);
        Self {
            parent_uuid: parent_uuid.to_string(),
            path,
        }
    }

    /// RecordToolUse records a tool use in the sidechain transcript.
    pub fn record_tool_use(&self, _tool_id: &str, _tool_name: &str, _args: &serde_json::Value) {
        // TODO: implement when transcript writer is integrated
    }

    /// RecordToolResult records a tool result in the sidechain transcript.
    pub fn record_tool_result(&self, _tool_id: &str, _tool_name: &str, _result: &str) {
        // TODO: implement when transcript writer is integrated
    }

    /// RecordSystem records a system message in the sidechain transcript.
    pub fn record_system(&self, _content: &str) {
        // TODO: implement when transcript writer is integrated
    }

    /// RecordError records an error in the sidechain transcript.
    pub fn record_error(&self, _err: &str) {
        // TODO: implement when transcript writer is integrated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_sidechain_transcript() {
        let st = SidechainTranscript::new("uuid-123", "/tmp/sessions", "test-agent");
        assert_eq!(st.parent_uuid, "uuid-123");
        assert!(st.path.to_str().unwrap().contains("sidechain-test-agent-"));
        assert!(st.path.to_str().unwrap().ends_with(".jsonl"));
    }

    #[test]
    fn test_record_methods_dont_panic() {
        let st = SidechainTranscript::new("uuid-456", "/tmp", "agent");
        st.record_tool_use("id", "tool", &serde_json::json!({}));
        st.record_tool_result("id", "tool", "result");
        st.record_system("system msg");
        st.record_error("error msg");
    }
}
