use std::collections::HashMap;
use std::sync::RwLock;

/// AgentHandle represents a running or completed sub-agent that can be addressed by name.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub name: String,
    pub task_id: String,
    pub status: String, // "running", "completed", "failed"
    pub result: Option<String>,
}

/// AgentHandleStore tracks named agents for routing via send_message.
/// It provides a lightweight name -> handle mapping on top of the task store.
pub struct AgentHandleStore {
    agents: RwLock<HashMap<String, AgentHandle>>,
}

impl AgentHandleStore {
    /// Creates an empty agent handle store.
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Register adds or updates a named agent handle.
    pub fn register(&self, name: String, handle: AgentHandle) {
        let mut agents = self.agents.write().unwrap();
        agents.insert(name, handle);
    }

    /// Lookup returns the handle for a given name, or None if not found.
    pub fn lookup(&self, name: &str) -> Option<AgentHandle> {
        let agents = self.agents.read().unwrap();
        agents.get(name).cloned()
    }

    /// List returns all registered agent handles.
    pub fn list(&self) -> Vec<AgentHandle> {
        let agents = self.agents.read().unwrap();
        agents.values().cloned().collect()
    }

    /// Complete updates the status and result of a named agent.
    pub fn complete(&self, name: &str, result: String) {
        let mut agents = self.agents.write().unwrap();
        if let Some(h) = agents.get_mut(name) {
            if h.status != "completed" && h.status != "failed" {
                h.status = "completed".to_string();
                h.result = Some(result);
            }
        }
    }

    /// Fail updates the status of a named agent to indicate failure.
    pub fn fail(&self, name: &str, err_msg: String) {
        let mut agents = self.agents.write().unwrap();
        if let Some(h) = agents.get_mut(name) {
            if h.status != "completed" && h.status != "failed" {
                h.status = "failed".to_string();
                h.result = Some(err_msg);
            }
        }
    }

    /// Count returns the number of registered agents.
    pub fn count(&self) -> usize {
        let agents = self.agents.read().unwrap();
        agents.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let store = AgentHandleStore::new();
        let handle = AgentHandle {
            name: "test-agent".to_string(),
            task_id: "task-1".to_string(),
            status: "running".to_string(),
            result: None,
        };
        store.register("test-agent".to_string(), handle);
        let found = store.lookup("test-agent").unwrap();
        assert_eq!(found.name, "test-agent");
        assert_eq!(found.status, "running");
    }

    #[test]
    fn test_lookup_not_found() {
        let store = AgentHandleStore::new();
        assert!(store.lookup("nonexistent").is_none());
    }

    #[test]
    fn test_complete() {
        let store = AgentHandleStore::new();
        let handle = AgentHandle {
            name: "agent-1".to_string(),
            task_id: "task-1".to_string(),
            status: "running".to_string(),
            result: None,
        };
        store.register("agent-1".to_string(), handle);
        store.complete("agent-1", "done".to_string());
        let found = store.lookup("agent-1").unwrap();
        assert_eq!(found.status, "completed");
        assert_eq!(found.result, Some("done".to_string()));
    }

    #[test]
    fn test_fail() {
        let store = AgentHandleStore::new();
        let handle = AgentHandle {
            name: "agent-2".to_string(),
            task_id: "task-2".to_string(),
            status: "running".to_string(),
            result: None,
        };
        store.register("agent-2".to_string(), handle);
        store.fail("agent-2", "error occurred".to_string());
        let found = store.lookup("agent-2").unwrap();
        assert_eq!(found.status, "failed");
        assert_eq!(found.result, Some("error occurred".to_string()));
    }

    #[test]
    fn test_list() {
        let store = AgentHandleStore::new();
        for i in 0..3 {
            let handle = AgentHandle {
                name: format!("agent-{}", i),
                task_id: format!("task-{}", i),
                status: "running".to_string(),
                result: None,
            };
            store.register(format!("agent-{}", i), handle);
        }
        assert_eq!(store.list().len(), 3);
    }

    #[test]
    fn test_count() {
        let store = AgentHandleStore::new();
        assert_eq!(store.count(), 0);
        let handle = AgentHandle {
            name: "a".to_string(),
            task_id: "t".to_string(),
            status: "running".to_string(),
            result: None,
        };
        store.register("a".to_string(), handle);
        assert_eq!(store.count(), 1);
    }
}
