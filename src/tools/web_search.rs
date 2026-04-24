//! WebSearchTool - Web search using Bing/360

use crate::tools::{Tool, ToolResult};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;

pub struct WebSearchTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WebSearchTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_scraper"
    }

    fn description(&self) -> &str {
        "Search the web using Bing/360 HTML scraping. Fallback search when web_search fails."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results (1-10, default: 10)"
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

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let query = match params.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Error: query is required"),
        };

        let count = params
            .get("count")
            .and_then(|v| v.as_i64())
            .unwrap_or(10)
            .max(1)
            .min(10) as usize;

        let results = search_bing(query, count);

        match results {
            Ok(results) => {
                if results.is_empty() {
                    return ToolResult::ok(format!("No results found for: {}", query));
                }

                let mut output = format!("Search results for: {}\n", query);
                for (i, result) in results.iter().enumerate() {
                    output.push_str(&format!("{}. {}\n", i + 1, result.title));
                    output.push_str(&format!("   URL: {}\n", result.url));
                    if !result.snippet.is_empty() {
                        output.push_str(&format!("   {}\n", result.snippet));
                    }
                }

                ToolResult::ok(output.trim().to_string())
            }
            Err(e) => ToolResult::error(format!("Search error: {}", e)),
        }
    }
}

#[derive(Debug)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

fn search_bing(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    // Check for proxy
    let proxy = std::env::var("HTTP_PROXY").ok().and_then(|p| {
        reqwest::Proxy::http(&p).ok()
    });

    let mut client_builder = Client::builder()
        .timeout(std::time::Duration::from_secs(30));

    if let Some(proxy_url) = proxy {
        client_builder = client_builder.proxy(proxy_url);
    }

    let client = client_builder
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let search_url = format!(
        "https://www.bing.com/search?q={}&setmkt=en-US",
        urlencoding::encode(query)
    );

    let request = client
        .get(&search_url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9");

    let response = request.send().map_err(|e| format!("Request failed: {}", e))?;

    let body = response.text().map_err(|e| format!("Read body failed: {}", e))?;

    let results = parse_bing_results(&body, max_results);

    if results.is_empty() {
        // Try 360 search as fallback
        search_360(query, max_results)
    } else {
        Ok(results)
    }
}

fn parse_bing_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Simple regex-based parsing for Bing results
    let re = regex::Regex::new(r#"<li[^>]*class="[^"]*b_algo[^"]*"[^>]*>(.*?)</li>"#).unwrap();
    
    for cap in re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }

        let block = &cap[1];

        // Extract title and URL
        let title_re = regex::Regex::new(r#"<h2[^>]*>(.*?)</h2>"#).unwrap();
        let title = title_re
            .captures(block)
            .map(|c| strip_tags(&c[1]))
            .unwrap_or_default();

        let url_re = regex::Regex::new(r#"href="([^"]+)""#).unwrap();
        let url = url_re
            .captures(block)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        // Extract snippet
        let snippet_re = regex::Regex::new(r#"<p[^>]*>(.*?)</p>"#).unwrap();
        let snippet = snippet_re
            .captures(block)
            .map(|c| strip_tags(&c[1]))
            .unwrap_or_default();

        if !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    results
}

fn search_360(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let search_url = format!(
        "https://www.so.com/s?q={}",
        urlencoding::encode(query)
    );

    let response = client
        .get(&search_url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .send()
        .map_err(|e| format!("Request failed: {}", e))?;

    let body = response.text().map_err(|e| format!("Read body failed: {}", e))?;

    Ok(parse_360_results(&body, max_results))
}

fn parse_360_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    let re = regex::Regex::new(r#"<li[^>]*class="[^"]*res[^"]*"[^>]*>(.*?)</li>"#).unwrap();
    
    for cap in re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }

        let block = &cap[1];

        let title_re = regex::Regex::new(r#"<h3[^>]*>(.*?)</h3>"#).unwrap();
        let title = title_re
            .captures(block)
            .map(|c| strip_tags(&c[1]))
            .unwrap_or_default();

        let url_re = regex::Regex::new(r#"href="([^"]+)""#).unwrap();
        let url = url_re
            .captures(block)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        let snippet_re = regex::Regex::new(r#"<p[^>]*>(.*?)</p>"#).unwrap();
        let snippet = snippet_re
            .captures(block)
            .map(|c| strip_tags(&c[1]))
            .unwrap_or_default();

        if !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    results
}

pub(crate) fn strip_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }

    // Decode HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

fn contains_internal_url(s: &str) -> bool {
    let internal_patterns = [
        r"localhost",
        r"127\.0\.0\.1",
        r"0\.0\.0\.0",
        r"192\.168\.\d+\.\d+",
        r"10\.\d+\.\d+\.\d+",
        r"172\.(1[6-9]|2\d|3[01])\.\d+\.\d+",
    ];

    for pattern in &internal_patterns {
        if regex::Regex::new(pattern).unwrap().is_match(s) {
            return true;
        }
    }

    false
}
