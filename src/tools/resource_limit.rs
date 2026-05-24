use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

/// ResourceLimit defines resource constraints for a process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimit {
    pub max_cpu_seconds: Option<f64>,
    pub max_memory_mb: Option<u64>,
    pub max_file_size_mb: Option<u64>,
    pub max_processes: Option<u32>,
    pub max_open_files: Option<u32>,
}

impl Default for ResourceLimit {
    fn default() -> Self {
        Self {
            max_cpu_seconds: Some(300.0),   // 5 minutes
            max_memory_mb: Some(1024),       // 1GB
            max_file_size_mb: Some(100),     // 100MB
            max_processes: Some(100),
            max_open_files: Some(256),
        }
    }
}

impl ResourceLimit {
    pub fn new() -> Self { Self::default() }

    pub fn unlimited() -> Self {
        Self {
            max_cpu_seconds: None,
            max_memory_mb: None,
            max_file_size_mb: None,
            max_processes: None,
            max_open_files: None,
        }
    }

    pub fn with_cpu_seconds(mut self, seconds: f64) -> Self {
        self.max_cpu_seconds = Some(seconds);
        self
    }

    pub fn with_memory_mb(mut self, mb: u64) -> Self {
        self.max_memory_mb = Some(mb);
        self
    }

    pub fn with_file_size_mb(mut self, mb: u64) -> Self {
        self.max_file_size_mb = Some(mb);
        self
    }

    pub fn with_max_processes(mut self, n: u32) -> Self {
        self.max_processes = Some(n);
        self
    }

    pub fn with_max_open_files(mut self, n: u32) -> Self {
        self.max_open_files = Some(n);
        self
    }
}

/// ResourceLimitStore is a registry of resource limits for named process types.
pub struct ResourceLimitStore {
    limits: Mutex<HashMap<String, ResourceLimit>>,
    default_limit: ResourceLimit,
}

impl ResourceLimitStore {
    pub fn new() -> Self {
        let mut limits = HashMap::new();
        // Set default limits for common process types
        limits.insert("bash".to_string(), ResourceLimit::default().with_cpu_seconds(120.0));
        limits.insert("python".to_string(), ResourceLimit::default().with_cpu_seconds(300.0));
        limits.insert("node".to_string(), ResourceLimit::default().with_cpu_seconds(300.0));

        Self {
            limits: Mutex::new(limits),
            default_limit: ResourceLimit::default(),
        }
    }

    pub fn get(&self, process_type: &str) -> ResourceLimit {
        self.limits.lock().unwrap()
            .get(process_type)
            .cloned()
            .unwrap_or_else(|| self.default_limit.clone())
    }

    pub fn set(&self, process_type: &str, limit: ResourceLimit) {
        self.limits.lock().unwrap().insert(process_type.to_string(), limit);
    }

    pub fn remove(&self, process_type: &str) {
        self.limits.lock().unwrap().remove(process_type);
    }

    pub fn list(&self) -> Vec<String> {
        self.limits.lock().unwrap().keys().cloned().collect()
    }
}

/// Check if a resource limit is exceeded based on current usage.
pub fn check_resource_limit(limit: &ResourceLimit, cpu_seconds: f64, memory_mb: u64, open_files: u32) -> Result<(), String> {
    if let Some(max_cpu) = limit.max_cpu_seconds {
        if cpu_seconds > max_cpu {
            return Err(format!("CPU time limit exceeded: {:.1}s > {:.1}s", cpu_seconds, max_cpu));
        }
    }
    if let Some(max_mem) = limit.max_memory_mb {
        if memory_mb > max_mem {
            return Err(format!("Memory limit exceeded: {}MB > {}MB", memory_mb, max_mem));
        }
    }
    if let Some(max_files) = limit.max_open_files {
        if open_files > max_files {
            return Err(format!("Open files limit exceeded: {} > {}", open_files, max_files));
        }
    }
    Ok(())
}

/// Get system memory info in MB.
pub fn get_system_memory_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if line.starts_with("MemTotal:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb / 1024;
                        }
                    }
                }
            }
        }
    }
    8192 // default 8GB
}

/// Get current process memory usage in MB.
pub fn get_process_memory_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/self/status") {
            for line in content.lines() {
                if line.starts_with("VmRSS:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb / 1024;
                        }
                    }
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_limit_default() {
        let limit = ResourceLimit::default();
        assert_eq!(limit.max_cpu_seconds, Some(300.0));
        assert_eq!(limit.max_memory_mb, Some(1024));
    }

    #[test]
    fn test_resource_limit_builder() {
        let limit = ResourceLimit::new()
            .with_cpu_seconds(60.0)
            .with_memory_mb(512);
        assert_eq!(limit.max_cpu_seconds, Some(60.0));
        assert_eq!(limit.max_memory_mb, Some(512));
    }

    #[test]
    fn test_resource_limit_unlimited() {
        let limit = ResourceLimit::unlimited();
        assert!(limit.max_cpu_seconds.is_none());
        assert!(limit.max_memory_mb.is_none());
    }

    #[test]
    fn test_check_resource_limit_ok() {
        let limit = ResourceLimit::default();
        assert!(check_resource_limit(&limit, 100.0, 512, 10).is_ok());
    }

    #[test]
    fn test_check_resource_limit_cpu_exceeded() {
        let limit = ResourceLimit::default();
        assert!(check_resource_limit(&limit, 400.0, 512, 10).is_err());
    }

    #[test]
    fn test_check_resource_limit_memory_exceeded() {
        let limit = ResourceLimit::default();
        assert!(check_resource_limit(&limit, 100.0, 2048, 10).is_err());
    }

    #[test]
    fn test_resource_limit_store() {
        let store = ResourceLimitStore::new();
        let bash_limit = store.get("bash");
        assert_eq!(bash_limit.max_cpu_seconds, Some(120.0));

        store.set("custom", ResourceLimit::new().with_cpu_seconds(30.0));
        let custom_limit = store.get("custom");
        assert_eq!(custom_limit.max_cpu_seconds, Some(30.0));

        let unknown = store.get("unknown");
        assert_eq!(unknown.max_cpu_seconds, Some(300.0)); // default
    }
}
