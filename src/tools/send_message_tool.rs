use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

/// SendMessageTool sends messages to the main agent from a sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageInput {
    pub message: String,
    pub target_agent: Option<String>,
}

/// SendMessageTool allows sub-agents to send messages.
pub struct SendMessageTool {
    message_queue: Arc<Mutex<Vec<QueuedMessage>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub from_agent: String,
    pub message: String,
    pub timestamp: u64,
    pub target_agent: Option<String>,
}

impl SendMessageTool {
    pub fn new(queue: Arc<Mutex<Vec<QueuedMessage>>>) -> Self {
        Self { message_queue: queue }
    }

    pub fn send_message(&self, from: &str, message: &str, target: Option<&str>) -> Result<(), String> {
        let queued = QueuedMessage {
            from_agent: from.to_string(),
            message: message.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            target_agent: target.map(|s| s.to_string()),
        };
        self.message_queue.lock().unwrap().push(queued);
        Ok(())
    }

    pub fn drain_messages(&self) -> Vec<QueuedMessage> {
        let mut queue = self.message_queue.lock().unwrap();
        std::mem::take(&mut *queue)
    }

    pub fn has_messages(&self) -> bool {
        !self.message_queue.lock().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_message() {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let tool = SendMessageTool::new(queue);

        tool.send_message("agent-1", "Hello from agent 1", None).unwrap();
        tool.send_message("agent-2", "Hello from agent 2", Some("main")).unwrap();

        assert!(tool.has_messages());
        let msgs = tool.drain_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].from_agent, "agent-1");
        assert_eq!(msgs[0].message, "Hello from agent 1");
        assert_eq!(msgs[1].target_agent, Some("main".to_string()));
        assert!(!tool.has_messages());
    }

    #[test]
    fn test_drain_messages_empties_queue() {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let tool = SendMessageTool::new(queue);
        tool.send_message("a", "msg", None).unwrap();
        let _ = tool.drain_messages();
        assert!(!tool.has_messages());
    }
}
