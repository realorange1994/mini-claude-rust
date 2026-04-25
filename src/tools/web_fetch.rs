//! WebFetchTool - Fetch and extract readable content from URLs

use crate::tools::{Tool, ToolResult, contains_internal_url, strip_tags};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::HashMap;

pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WebFetchTool {
    fn clone(&self) -> Self {
        Self
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and extract readable text content. Strips HTML, removes scripts/styles, extracts title and meta description."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (must include http:// or https://)."
                },
                "extractMode": {
                    "type": "string",
                    "description": "Extraction mode: 'text', 'markdown', or 'json' (default: markdown)."
                }
            },
            "required": ["url"]
        }).as_object().unwrap().clone()
    }

    fn check_permissions(&self, params: &HashMap<String, Value>) -> Option<ToolResult> {
        let url = params.get("url")?.as_str()?;
        if url.starts_with("file://") {
            return Some(ToolResult::error("Blocked: file:// URLs are not allowed"));
        }
        if contains_internal_url(url) {
            return Some(ToolResult::error("Blocked: internal/private URLs are not allowed"));
        }
        None
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolResult::error("Error: url is required"),
        };

        let extract_mode = params
            .get("extractMode")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");

        fetch_url(url, extract_mode)
    }
}

fn fetch_url(url: &str, extract_mode: &str) -> ToolResult {
    // Check for proxy
    let proxy = std::env::var("HTTP_PROXY").ok().and_then(|p| {
        reqwest::Proxy::http(&p).ok()
    });

    let mut client_builder = Client::builder()
        .timeout(std::time::Duration::from_secs(30));

    if let Some(proxy_url) = proxy {
        client_builder = client_builder.proxy(proxy_url);
    }

    let client = match client_builder.build() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Error: client build failed: {}", e)),
    };

    let request = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9");

    let response = match request.send() {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("Error: fetch failed: {}", e)),
    };

    if !response.status().is_success() {
        return ToolResult::error(format!(
            "Error: HTTP {}: {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or("Unknown")
        ));
    }

    // Extract content type first (before consuming response)
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let body = match response.bytes() {
        Ok(b) => b.to_vec(),
        Err(e) => return ToolResult::error(format!("Error: read body: {}", e)),
    };

    let mut content = String::new();

    // Handle compression
    let body_content = if content_type.contains("gzip") {
        let mut decoder = GzDecoder::new(&body[..]);
        std::io::Read::read_to_string(&mut decoder, &mut content).ok();
        content.clone()
    } else {
        String::from_utf8_lossy(&body).to_string()
    };

    // Extract content based on type
    let text = if content_type.contains("html") || content_type.is_empty() {
        if extract_mode == "text" {
            strip_html_simple(&body_content)
        } else {
            extract_text_from_html(&body_content)
        }
    } else {
        body_content.clone()
    };

    let title = extract_html_title(&body_content);
    let description = extract_html_meta(&body_content, "description");

    let mut result = String::new();
    
    if !title.is_empty() {
        result.push_str(&format!("Title: {}\n\n", title));
    }
    if !description.is_empty() {
        result.push_str(&format!("Description: {}\n\n", description));
    }

    if extract_mode == "json" {
        result.push_str(&serde_json::json!({
            "url": url,
            "content": text,
            "content_type": content_type
        }).to_string());
    } else {
        result.push_str("--- Content ---\n");
        result.push_str(&text);
    }

    // Truncate if too large
    const MAX_BODY_SIZE: usize = 1 << 20; // 1MB
    if result.len() > MAX_BODY_SIZE {
        let half = MAX_BODY_SIZE / 2;
        let mut first_end = half;
        while first_end > 0 && !result.is_char_boundary(first_end) { first_end -= 1; }
        let mid_start = result.len() - half;
        let mut mid_end = mid_start;
        while mid_end < result.len() && !result.is_char_boundary(mid_end) { mid_end += 1; }
        let truncated = result.len() - (first_end + (result.len() - mid_end));
        result = format!(
            "{}\n\n... ({} chars truncated) ...\n\n{}",
            &result[..first_end],
            truncated,
            &result[mid_end..]
        );
    }

    ToolResult::ok(result)
}

fn strip_html_simple(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                result.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }

    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_text_from_html(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_depth: usize = 0;

    let html_lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let chars_lower: Vec<char> = html_lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let _c_lower = chars_lower[i];

        // Check for script/style tags
        if i + 7 < chars_lower.len() && chars_lower[i..i+7] == ['<', 's', 'c', 'r', 'i', 'p', 't'] {
            in_script = true;
            tag_depth += 1;
            i += 7;
            continue;
        }
        if i + 8 < chars_lower.len() && chars_lower[i..i+8] == ['<', '/', 's', 'c', 'r', 'i', 'p', 't'] {
            in_script = false;
            tag_depth = tag_depth.saturating_sub(1);
            i += 8;
            continue;
        }
        if i + 6 < chars_lower.len() && chars_lower[i..i+6] == ['<', 's', 't', 'y', 'l', 'e'] {
            in_style = true;
            tag_depth += 1;
            i += 6;
            continue;
        }
        if i + 7 < chars_lower.len() && chars_lower[i..i+7] == ['<', '/', 's', 't', 'y', 'l', 'e'] {
            in_style = false;
            tag_depth = tag_depth.saturating_sub(1);
            i += 7;
            continue;
        }

        if in_script || in_style || tag_depth > 0 {
            if c == '<' {
                tag_depth += 1;
            } else if c == '>' {
                tag_depth = tag_depth.saturating_sub(1);
            }
            i += 1;
            continue;
        }

        match c {
            '<' => {
                in_tag = true;
                if !result.ends_with('\n') && !result.is_empty() {
                    result.push('\n');
                }
            }
            '>' => {
                in_tag = false;
            }
            _ if !in_tag => {
                result.push(c);
            }
            _ => {}
        }

        i += 1;
    }

    // Clean up whitespace
    let mut cleaned = String::new();
    let mut last_was_space = false;
    
    for c in result.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                cleaned.push(' ');
                last_was_space = true;
            }
        } else {
            cleaned.push(c);
            last_was_space = false;
        }
    }

    cleaned.trim().to_string()
}

fn extract_html_title(html: &str) -> String {
    use std::sync::OnceLock;
    static TITLE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = TITLE_RE.get_or_init(|| regex::Regex::new(r"(?i)<title[^>]*>(.*?)</title>").unwrap());
    re.captures(html)
        .map(|c| strip_tags(&c[1]))
        .unwrap_or_default()
}

fn extract_html_meta(html: &str, name: &str) -> String {
    let patterns = [
        format!(r#"name="{}"[^>]*content="([^"]+)""#, name),
        format!(r#"property="{}"[^>]*content="([^"]+)""#, name),
        format!(r#"content="([^"]+)"[^>]*name="{}""#, name),
    ];

    for pattern in &patterns {
        if let Ok(re) = regex::Regex::new(&format!("(?i){}", pattern)) {
            if let Some(cap) = re.captures(html) {
                return cap[1].to_string();
            }
        }
    }

    String::new()
}

