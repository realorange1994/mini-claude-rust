//! ExaSearchTool - Exa deep web search

use crate::tools::{Tool, ToolResult, contains_internal_url, strip_tags};
use serde_json::Value;
use std::collections::HashMap;

pub struct ExaSearchTool;

impl ExaSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExaSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ExaSearchTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for ExaSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using Exa AI. Returns relevant content with titles, URLs, and text snippets."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default: 10)"
                },
                "type": {
                    "type": "string",
                    "description": "Search type: 'auto', 'fast', or 'deep'. Default: 'auto'."
                },
                "livecrawl": {
                    "type": "string",
                    "description": "Crawl mode: 'fallback' or 'preferred'. Default: 'fallback'."
                }
            },
            "required": ["query"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> Option<ToolResult> {
        let query = params.get("query")?.as_str()?;
        if contains_internal_url(query) {
            return Some(ToolResult::error("Search blocked: internal URL detected in query"));
        }
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly, crate::tools::ToolCapability::Network]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Error: query is required"),
        };

        let num_results = params
            .get("num_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(10)
            .max(1) as usize;

        // Try Exa API first
        if let Ok(api_key) = std::env::var("EXA_API_KEY") {
            match exa_search(&api_key, query, num_results) {
                Ok(results) => {
                    if !results.is_empty() {
                        return ToolResult::ok(results);
                    }
                }
                Err(e) => {
                    eprintln!("Exa search error: {}", e);
                }
            }
        }

        // Fallback to Bing
        let results = fallback_bing_search(query, num_results);
        ToolResult::ok(results)
    }
}

fn exa_search(api_key: &str, query: &str, num_results: usize) -> Result<String, String> {
    // Exa API integration
    let client = reqwest::blocking::Client::new();

    let response = client
        .post("https://api.exa.ai/search")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "numResults": num_results,
            "type": "auto"
        }))
        .send()
        .map_err(|e| format!("Request failed: {}", e))?;

    let body: serde_json::Value = response
        .json()
        .map_err(|e| format!("Parse failed: {}", e))?;

    let mut output = format!("Exa search results for: {}\n\n", query);

    if let Some(results) = body.get("results").and_then(|r| r.as_array()) {
        for (i, result) in results.iter().enumerate() {
            if let (Some(title), Some(url)) = (
                result.get("title").and_then(|t| t.as_str()),
                result.get("url").and_then(|u| u.as_str()),
            ) {
                let snippet = result
                    .get("snippet")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");

                output.push_str(&format!("{}. {}\n", i + 1, title));
                output.push_str(&format!("   URL: {}\n", url));
                if !snippet.is_empty() {
                    output.push_str(&format!("   {}\n\n", snippet));
                }
            }
        }
    }

    Ok(output.trim().to_string())
}

fn fallback_bing_search(query: &str, num_results: usize) -> String {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();

    let search_url = format!(
        "https://www.bing.com/search?q={}&setmkt=en-US",
        urlencoding::encode(query)
    );

    let response = match client
        .get(&search_url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .send()
    {
        Ok(r) => r,
        Err(_) => return format!("No results found for: {}", query),
    };

    let body = match response.text() {
        Ok(b) => b,
        Err(_) => return format!("No results found for: {}", query),
    };

    let mut output = format!("Search results for: {}\n\n", query);

    // Cached regex patterns
    use std::sync::OnceLock;
    static LI_RE: OnceLock<regex::Regex> = OnceLock::new();
    static TITLE_RE: OnceLock<regex::Regex> = OnceLock::new();
    static URL_RE: OnceLock<regex::Regex> = OnceLock::new();

    let li_re = LI_RE.get_or_init(|| regex::Regex::new(r#"<li[^>]*class="[^"]*b_algo[^"]*"[^>]*>(.*?)</li>"#).unwrap());
    let title_re = TITLE_RE.get_or_init(|| regex::Regex::new(r#"<h2[^>]*>(.*?)</h2>"#).unwrap());
    let url_re = URL_RE.get_or_init(|| regex::Regex::new(r#"href="([^"]+)""#).unwrap());

    let mut count = 0;
    for cap in li_re.captures_iter(&body) {
        if count >= num_results {
            break;
        }

        let block = &cap[1];

        if let Some(title_cap) = title_re.captures(block) {
            let title = strip_tags(&title_cap[1]);
            if let Some(url_cap) = url_re.captures(block) {
                let url = &url_cap[1];
                count += 1;
                output.push_str(&format!("{}. {}\n", count, title));
                output.push_str(&format!("   URL: {}\n\n", url));
            }
        }
    }

    if count == 0 {
        return format!("No results found for: {}", query);
    }

    output.trim().to_string()
}

