use std::fs;
use std::path::Path;

/// Recursively finds all files matching a predicate
pub fn find_files(dir: &Path, predicate: &dyn Fn(&Path) -> bool) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if !dir.exists() {
        return result;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(find_files(&path, predicate));
            } else if predicate(&path) {
                result.push(path);
            }
        }
    }
    result
}

/// Ensures a directory exists, creating it if necessary
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Reads a file to string, returning None if it doesn't exist
pub fn read_file_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Safely writes data to a file, creating parent directories if needed
pub fn safe_write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ensure_dir() {
        let tmp = TempDir::new().unwrap();
        let new_dir = tmp.path().join("a/b/c");
        ensure_dir(&new_dir).unwrap();
        assert!(new_dir.exists());
    }

    #[test]
    fn test_read_file_optional() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        assert!(read_file_optional(&file).is_none());
        fs::write(&file, "hello").unwrap();
        assert_eq!(read_file_optional(&file), Some("hello".to_string()));
    }

    #[test]
    fn test_safe_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("sub/dir/test.txt");
        safe_write(&file, "hello").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
    }

    #[test]
    fn test_find_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("b.rs"), "").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub/c.txt"), "").unwrap();

        let txt_files = find_files(tmp.path(), &|p| {
            p.extension().map(|e| e == "txt").unwrap_or(false)
        });
        assert_eq!(txt_files.len(), 2);
    }
}
