//! MCP client module - Model Context Protocol support

pub mod client;

pub use client::parse_tool_result;

use self::client::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// MCP Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

/// MCP Tool Result
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,
}

/// Tool annotated with its source server name
pub struct ToolWithServer {
    pub tool: Tool,
    pub server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

/// MCP Manager - manages MCP server connections
pub struct Manager {
    clients: RwLock<HashMap<String, Arc<Client>>>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let clients = self.clients.read().unwrap();
        f.debug_struct("Manager")
            .field("servers", &clients.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Manager {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
        }
    }

    /// Register a local MCP server (stdio mode)
    pub fn register(&self, name: &str, command: &str, args: &[String], env: HashMap<String, String>) {
        let client = Client::new_stdio(name, command, args, &env);
        let mut clients = self.clients.write().unwrap();
        clients.insert(name.to_string(), Arc::new(client));
    }

    /// Register a remote MCP server (HTTP+SSE mode)
    pub fn register_remote(&self, name: &str, url: &str, env: HashMap<String, String>) {
        let client = Client::new_remote(name, url, &env);
        let mut clients = self.clients.write().unwrap();
        clients.insert(name.to_string(), Arc::new(client));
    }

    /// Start all registered MCP servers
    pub fn start_all(&self) -> Result<(), String> {
        let clients = self.clients.read().unwrap();
        let mut started: Vec<(String, Arc<Client>)> = Vec::new();

        for (name, client) in clients.iter() {
            if let Err(e) = client.start() {
                // Rollback already-started servers
                for (_, c) in &started {
                    let _ = c.stop();
                }
                return Err(format!("MCP server {}: {}", name, e));
            }
            started.push((name.clone(), client.clone()));
        }

        Ok(())
    }

    /// Stop all servers
    pub fn stop_all(&self) {
        let clients = self.clients.read().unwrap();
        for (_, client) in clients.iter() {
            let _ = client.stop();
        }
    }

    /// List all registered server names
    pub fn list_servers(&self) -> Vec<String> {
        let clients = self.clients.read().unwrap();
        clients.keys().cloned().collect()
    }

    /// List all available tools from all servers
    pub fn list_tools(&self) -> Vec<Tool> {
        let clients = self.clients.read().unwrap();
        let mut all = Vec::new();
        for (_, client) in clients.iter() {
            all.extend(client.tools());
        }
        all
    }

    /// Get server connection status
    pub fn get_server_status(&self, name: &str) -> String {
        let clients = self.clients.read().unwrap();
        match clients.get(name) {
            Some(client) => {
                // Check if the client has discovered tools (indicates successful start)
                if !client.tools().is_empty() {
                    "connected".to_string()
                } else {
                    "disconnected".to_string()
                }
            }
            None => "not found".to_string(),
        }
    }

    /// List all tools annotated with their source server name.
    pub fn all_tools_with_server(&self) -> Vec<ToolWithServer> {
        let clients = self.clients.read().unwrap();
        let mut result = Vec::new();
        for (server_name, client) in clients.iter() {
            for tool in client.tools() {
                result.push(ToolWithServer {
                    tool: tool.clone(),
                    server: server_name.clone(),
                });
            }
        }
        result
    }

    /// Get usage instructions for a specific MCP server.
    pub fn get_server_instructions(&self, name: &str) -> String {
        let clients = self.clients.read().unwrap();
        match clients.get(name) {
            Some(client) => client.instructions(),
            None => String::new(),
        }
    }

    /// Get all MCP server instructions as a map[serverName]instructions.
    pub fn all_server_instructions(&self) -> HashMap<String, String> {
        let clients = self.clients.read().unwrap();
        let mut result = HashMap::new();
        for (name, client) in clients.iter() {
            let instr = client.instructions();
            if !instr.is_empty() {
                result.insert(name.clone(), instr);
            }
        }
        result
    }

    /// Call a tool by name, searching across all servers.
    /// Releases the client map lock before doing I/O to avoid blocking other operations.
    pub fn call_tool(&self, name: &str, args: HashMap<String, serde_json::Value>) -> Result<ToolResult, String> {
        // Find the right client while holding the lock, then release before I/O
        let client = {
            let clients = self.clients.read().unwrap();
            let mut found: Option<Arc<Client>> = None;
            for (_server_name, client) in clients.iter() {
                for tool in client.tools() {
                    if tool.name == name {
                        found = Some(client.clone());
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            found
        };

        match client {
            Some(c) => c.call_tool(name, args),
            None => Err(format!("tool not found: {}", name)),
        }
    }

    /// Call a tool on a specific server.
    /// Releases the client map lock before doing I/O.
    pub fn call_tool_with_server(&self, server: &str, tool: &str, args: HashMap<String, serde_json::Value>) -> Result<ToolResult, String> {
        let client = {
            let clients = self.clients.read().unwrap();
            clients.get(server)
                .cloned()
                .ok_or_else(|| format!("server not found: {}", server))
        }?;

        client.call_tool(tool, args)
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Manager {
    fn clone(&self) -> Self {
        Self {
            clients: RwLock::new(self.clients.read().unwrap().clone()),
        }
    }
}
