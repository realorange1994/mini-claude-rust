//! Cost tracker for token usage across models.
//!
//! Tracks raw token counts per model, with persistence to JSON.

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

/// Tracks token usage across models.
#[derive(Debug)]
pub struct CostTracker {
    total_input_tokens: i64,
    total_output_tokens: i64,
    per_model: HashMap<String, ModelUsage>,
}

/// Per-model token consumption.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new() -> Self {
        Self {
            total_input_tokens: 0,
            total_output_tokens: 0,
            per_model: HashMap::new(),
        }
    }

    /// Record token usage from an API response.
    pub fn add_usage(&mut self, model: &str, input_tokens: i64, output_tokens: i64) {
        if input_tokens == 0 && output_tokens == 0 {
            return;
        }
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;

        let entry = self.per_model.entry(model.to_string()).or_insert(ModelUsage {
            input_tokens: 0,
            output_tokens: 0,
        });
        entry.input_tokens += input_tokens;
        entry.output_tokens += output_tokens;
    }

    /// Returns a human-readable summary string of token usage.
    pub fn format_cost_display(&self) -> String {
        if self.per_model.is_empty() {
            return "Total: 0 tokens".to_string();
        }

        // Aggregate by family
        let mut families: HashMap<String, (i64, i64)> = HashMap::new();
        for (model, entry) in &self.per_model {
            let fam = family_name(model);
            let (input, output) = families.entry(fam).or_insert((0, 0));
            *input += entry.input_tokens;
            *output += entry.output_tokens;
        }

        let total_tokens = self.total_input_tokens + self.total_output_tokens;

        let parts: Vec<String> = families
            .iter()
            .map(|(fam, (input, output))| {
                format!(
                    "{}: {} in, {} out",
                    fam,
                    format_token_count(*input),
                    format_token_count(*output)
                )
            })
            .collect();

        format!(
            "Total: {} tokens ({} in, {} out) | {}",
            format_token_count(total_tokens),
            format_token_count(self.total_input_tokens),
            format_token_count(self.total_output_tokens),
            parts.join(", ")
        )
    }

    /// Get a copy of the per-model usage breakdown.
    pub fn get_per_model_usage(&self) -> HashMap<String, ModelUsage> {
        self.per_model.clone()
    }

    /// Persist the cost tracker state to a JSON file.
    pub fn save_to_file(&self, path: &str) -> Result<(), String> {
        let data = CostTrackerFile {
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            per_model: self.per_model.clone(),
        };
        let json = serde_json::to_string_pretty(&data).map_err(|e| format!("cost_tracker: marshal: {}", e))?;
        fs::write(path, json).map_err(|e| format!("cost_tracker: write {}: {}", path, e))
    }

    /// Restore the cost tracker state from a JSON file.
    pub fn load_from_file(&mut self, path: &str) -> Result<(), String> {
        let raw = match fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("cost_tracker: read {}: {}", path, e)),
        };
        let data: CostTrackerFile = serde_json::from_str(&raw)
            .map_err(|e| format!("cost_tracker: unmarshal {}: {}", path, e))?;
        self.total_input_tokens = data.total_input_tokens;
        self.total_output_tokens = data.total_output_tokens;
        self.per_model = data.per_model;
        Ok(())
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe cost tracker wrapper.
pub struct SharedCostTracker {
    inner: Mutex<CostTracker>,
}

impl SharedCostTracker {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CostTracker::new()),
        }
    }

    pub fn add_usage(&self, model: &str, input_tokens: i64, output_tokens: i64) {
        self.inner.lock().unwrap().add_usage(model, input_tokens, output_tokens);
    }

    pub fn format_cost_display(&self) -> String {
        self.inner.lock().unwrap().format_cost_display()
    }

    pub fn save_to_file(&self, path: &str) -> Result<(), String> {
        self.inner.lock().unwrap().save_to_file(path)
    }

    pub fn load_from_file(&self, path: &str) -> Result<(), String> {
        self.inner.lock().unwrap().load_from_file(path)
    }
}

impl Default for SharedCostTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CostTrackerFile {
    total_input_tokens: i64,
    total_output_tokens: i64,
    per_model: HashMap<String, ModelUsage>,
}

/// Format token count with k/M suffixes for readability.
pub fn format_token_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Map a full model ID to a short display name.
fn family_name(model: &str) -> String {
    let lower = model.to_lowercase();
    if lower.contains("opus") {
        return "Opus".to_string();
    }
    if lower.contains("sonnet") {
        return "Sonnet".to_string();
    }
    if lower.contains("haiku") {
        return "Haiku".to_string();
    }
    if lower.contains("m2.7") {
        return "m2.7".to_string();
    }
    if lower.contains("m2.5") {
        return "m2.5".to_string();
    }
    if lower.contains("m2.1") {
        return "m2.1".to_string();
    }
    if lower.contains("deepseek") {
        return "DeepSeek".to_string();
    }
    if lower.contains("kimi") {
        return "Kimi".to_string();
    }
    if lower.contains("glm") {
        return "GLM".to_string();
    }
    if lower.contains("qwen") {
        return "Qwen".to_string();
    }
    if lower.contains("doubao") {
        return "Doubao".to_string();
    }
    // Trim provider prefix and version suffix for unknown models
    let parts: Vec<&str> = model.split('-').collect();
    if parts.len() > 2 {
        parts[..2].join("-")
    } else {
        model.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cost_tracker_add_usage() {
        let mut tracker = CostTracker::new();
        tracker.add_usage("claude-3-sonnet", 100, 50);
        tracker.add_usage("claude-3-sonnet", 200, 100);
        tracker.add_usage("claude-3-haiku", 500, 200);

        assert_eq!(tracker.total_input_tokens, 800);
        assert_eq!(tracker.total_output_tokens, 350);
    }

    #[test]
    fn test_cost_tracker_format_display() {
        let mut tracker = CostTracker::new();
        tracker.add_usage("claude-3-sonnet", 100_000, 50_000);
        let display = tracker.format_cost_display();
        assert!(display.contains("Total:"));
        assert!(display.contains("Sonnet"));
    }

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count(500), "500");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(1_500_000), "1.5M");
    }

    #[test]
    fn test_family_name() {
        assert_eq!(family_name("claude-3-opus-20240229"), "Opus");
        assert_eq!(family_name("claude-3-sonnet-20240229"), "Sonnet");
        assert_eq!(family_name("claude-3-haiku-20240307"), "Haiku");
        assert_eq!(family_name("deepseek-v3"), "DeepSeek");
    }

    #[test]
    fn test_save_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cost.json");
        let path_str = path.to_str().unwrap();

        let mut tracker = CostTracker::new();
        tracker.add_usage("claude-3-sonnet", 100, 50);
        tracker.save_to_file(path_str).unwrap();

        let mut tracker2 = CostTracker::new();
        tracker2.load_from_file(path_str).unwrap();
        assert_eq!(tracker2.total_input_tokens, 100);
        assert_eq!(tracker2.total_output_tokens, 50);
    }
}
