use std::fs;
use std::path::{Path, PathBuf};

/// ChunkInfo holds metadata about an archived conversation chunk.
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub index: usize,
    pub path: PathBuf,
    pub topics: String,
}

/// ChunkMessage represents a single message for archival.
#[derive(Debug, Clone)]
pub struct ChunkMessage {
    pub role: String,
    pub content: String,
}

/// ArchiveChunk writes discarded conversation messages to a chunk .md file.
pub fn archive_chunk(
    dir: &str,
    session_id: &str,
    chunk_index: usize,
    compression_level: usize,
    topics: &str,
    messages: &[ChunkMessage],
) -> Result<String, String> {
    if dir.is_empty() {
        return Err("chunk archival: dir is empty".to_string());
    }

    let chunk_dir = PathBuf::from(dir).join("chunks");
    fs::create_dir_all(&chunk_dir).map_err(|e| format!("chunk archival: mkdir: {}", e))?;

    let filename = format!("chunk-{}.md", chunk_index);
    let filepath = chunk_dir.join(&filename);

    let now = chrono::Local::now().to_rfc3339();
    let message_count = messages.len();

    let mut content = String::new();
    content.push_str("---\n");
    content.push_str(&format!("session_id: {}\n", session_id));
    content.push_str(&format!("chunk: {}\n", chunk_index));
    content.push_str(&format!("compression_level: {}\n", compression_level));
    content.push_str(&format!("archived_at: {}\n", now));
    content.push_str(&format!("message_count: {}\n", message_count));
    content.push_str(&format!("topics: {}\n", topics));
    content.push_str("---\n\n");
    content.push_str(&format!("# Session Chunk {}\n\n", chunk_index));

    for msg in messages {
        content.push_str(&format!("## {}\n\n", capitalize_role(&msg.role)));
        let display_content = if msg.content.len() > 500 {
            format!("{}...\n(truncated)", &msg.content[..500])
        } else {
            msg.content.clone()
        };
        content.push_str(&display_content);
        content.push_str("\n\n");
    }

    fs::write(&filepath, &content).map_err(|e| format!("chunk archival: write: {}", e))?;

    Ok(filepath.to_string_lossy().to_string())
}

/// ReadChunkTopics reads all chunk files and returns their metadata.
pub fn read_chunk_topics(dir: &str) -> Vec<ChunkInfo> {
    let chunk_dir = PathBuf::from(dir).join("chunks");
    if !chunk_dir.exists() {
        return vec![];
    }

    let mut chunks = Vec::new();
    if let Ok(entries) = fs::read_dir(&chunk_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    if filename.starts_with("chunk-") {
                        let index: usize = filename
                            .trim_start_matches("chunk-")
                            .trim_end_matches(".md")
                            .parse()
                            .unwrap_or(0);

                        let topics = if let Ok(content) = fs::read_to_string(&path) {
                            extract_topics_from_frontmatter(&content)
                        } else {
                            String::new()
                        };

                        chunks.push(ChunkInfo { index, path, topics });
                    }
                }
            }
        }
    }

    chunks.sort_by_key(|c| c.index);
    chunks
}

/// ReadChunkContent reads the full content of a chunk file.
pub fn read_chunk_content(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("read chunk: {}", e))
}

/// DeleteChunk removes a chunk file.
pub fn delete_chunk(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|e| format!("delete chunk: {}", e))
}

fn capitalize_role(role: &str) -> String {
    match role {
        "user" => "User".to_string(),
        "assistant" => "Assistant".to_string(),
        "system" => "System".to_string(),
        "tool" => "Tool".to_string(),
        _ => role.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default()
            + &role[1..],
    }
}

fn extract_topics_from_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return String::new();
    }
    let end = match content[3..].find("---") {
        Some(i) => i + 3,
        None => return String::new(),
    };
    let frontmatter = &content[3..end];
    for line in frontmatter.lines() {
        if let Some(topics) = line.strip_prefix("topics:") {
            return topics.trim().to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_archive_chunk() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_str().unwrap().to_string();

        let messages = vec![
            ChunkMessage { role: "user".to_string(), content: "Hello".to_string() },
            ChunkMessage { role: "assistant".to_string(), content: "Hi there!".to_string() },
        ];

        let result = archive_chunk(&dir_path, "session-1", 1, 1, "test, hello", &messages);
        assert!(result.is_ok());

        let path = result.unwrap();
        assert!(Path::new(&path).exists());

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("session_id: session-1"));
        assert!(content.contains("chunk: 1"));
        assert!(content.contains("topics: test, hello"));
        assert!(content.contains("## User"));
        assert!(content.contains("## Assistant"));
    }

    #[test]
    fn test_read_chunk_topics() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_str().unwrap().to_string();

        let messages = vec![
            ChunkMessage { role: "user".to_string(), content: "Test".to_string() },
        ];

        archive_chunk(&dir_path, "s1", 1, 1, "topic1", &messages).unwrap();
        archive_chunk(&dir_path, "s1", 2, 1, "topic2", &messages).unwrap();

        let chunks = read_chunk_topics(&dir_path);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].index, 1);
        assert_eq!(chunks[0].topics, "topic1");
        assert_eq!(chunks[1].index, 2);
        assert_eq!(chunks[1].topics, "topic2");
    }

    #[test]
    fn test_archive_chunk_empty_dir() {
        let result = archive_chunk("", "s1", 1, 1, "test", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_topics_from_frontmatter() {
        let content = "---\nsession_id: abc\nchunk: 1\ntopics: git, rust\n---\n\n# Content";
        assert_eq!(extract_topics_from_frontmatter(content), "git, rust");
    }

    #[test]
    fn test_capitalize_role() {
        assert_eq!(capitalize_role("user"), "User");
        assert_eq!(capitalize_role("assistant"), "Assistant");
        assert_eq!(capitalize_role("system"), "System");
    }
}
