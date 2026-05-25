//! Binary file detection for rgrep.
//! Ported from upstream tools/rgrep/binary.go (43 lines).

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Number of bytes to check for binary detection.
const CHECK_SIZE: usize = 8192;

/// Check if a file appears to be binary by scanning for null bytes.
/// Reads the first CHECK_SIZE bytes and returns true if any null byte is found.
pub fn is_binary_file(path: &Path) -> bool {
    let mut buf = [0u8; CHECK_SIZE];
    match File::open(path) {
        Ok(mut file) => match file.read(&mut buf) {
            Ok(n) => buf[..n].contains(&0),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Check if a byte slice appears to be binary data.
pub fn is_binary_data(data: &[u8]) -> bool {
    data.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_text_file_not_binary() {
        let dir = std::env::temp_dir().join("rgrep_binary_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("text.txt");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"hello world\nthis is text\n").unwrap();
        drop(f);
        assert!(!is_binary_file(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_binary_data() {
        assert!(is_binary_data(b"hello\x00world"));
        assert!(!is_binary_data(b"hello world"));
    }
}
