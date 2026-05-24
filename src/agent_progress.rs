//! AgentProgress -- Progress writer for child agent outputs
//!
//! Ported from Go's agent_progress.go. Provides a `io.Writer` interface
//! that buffers output and writes to a writer with an agent progress prefix.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

// ─── AgentProgressWriter ─────────────────────────────────────────────────────

/// A writer that prefixes all writes with a fixed agent progress prefix.
///
/// Buffers internally to prevent partial writes of the prefix.
pub struct AgentProgressWriter {
    prefix: String,
    writer: Arc<Mutex<dyn Write + Send + 'static>>,
    buffer: Vec<u8>,
}

impl AgentProgressWriter {
    /// Create a new AgentProgressWriter that writes to the given writer
    /// with the specified prefix.
    pub fn new(prefix: String, writer: Arc<Mutex<dyn Write + Send + 'static>>) -> Self {
        Self {
            prefix,
            writer,
            buffer: Vec::new(),
        }
    }

    /// Create a new AgentProgressWriter with the default agent progress prefix.
    pub fn new_default(writer: Arc<Mutex<dyn Write + Send + 'static>>) -> Self {
        Self::new("[agent] ".to_string(), writer)
    }

    /// Flush the internal buffer to the underlying writer, prefixing as needed.
    /// Only flushes complete lines ending with '\n'; remaining incomplete content stays in buffer,
    /// unless `force_all` is true, in which case everything is flushed.
    fn flush_buffer(&mut self, force_all: bool) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // Find the last newline if not forcing all
        let bytes = &self.buffer;
        let (to_flush, to_keep) = if force_all {
            (&bytes[..], &[] as &[u8])
        } else {
            let mut last_newline = None;
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\n' {
                    last_newline = Some(i + 1);
                }
            }
            match last_newline {
                Some(pos) => (&bytes[..pos], &bytes[pos..]),
                None => (&[] as &[u8], &bytes[..]),
            }
        };

        if !to_flush.is_empty() {
            // Prefix each line in the to_flush portion
            let mut lines = Vec::new();
            let mut start = 0;
            
            while start < to_flush.len() {
                let end = to_flush[start..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|pos| start + pos + 1)
                    .unwrap_or(to_flush.len());

                let line = &to_flush[start..end];
                if !line.is_empty() {
                    lines.extend_from_slice(self.prefix.as_bytes());
                    lines.extend_from_slice(line);
                }
                start = end;
            }

            // Write the prefixed lines
            {
                let mut writer = self.writer.lock().unwrap();
                writer.write_all(&lines)?;
                writer.flush()?;
            }
        }

        // Save the incomplete part back to buffer
        self.buffer = to_keep.to_vec();
        Ok(())
    }
}

impl Write for AgentProgressWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        
        // If the buffer contains a newline, flush it immediately
        if self.buffer.contains(&b'\n') {
            self.flush_buffer(false)?;
        }
        
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer(false)
    }
}

impl Drop for AgentProgressWriter {
    fn drop(&mut self) {
        // Flush any remaining data when dropped (force all content)
        let _ = self.flush_buffer(true);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TestWriter {
        output: Vec<u8>,
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_agent_progress_writer_basic() {
        let test_writer = Arc::new(Mutex::new(TestWriter { output: Vec::new() }));
        let mut progress_writer = AgentProgressWriter::new_default(test_writer.clone());
        
        progress_writer.write_all(b"hello\nworld\n").unwrap();
        progress_writer.flush().unwrap();
        
        let result = String::from_utf8(test_writer.lock().unwrap().output.clone()).unwrap();
        assert_eq!(result, "[agent] hello\n[agent] world\n");
    }

    #[test]
    fn test_agent_progress_writer_partial_line() {
        let test_writer = Arc::new(Mutex::new(TestWriter { output: Vec::new() }));
        let mut progress_writer = AgentProgressWriter::new_default(test_writer.clone());
        
        progress_writer.write_all(b"partial").unwrap();
        // No newline, buffer won't be flushed yet
        assert!(test_writer.lock().unwrap().output.is_empty());
        
        progress_writer.write_all(b"\n").unwrap();
        progress_writer.flush().unwrap();
        
        let result = String::from_utf8(test_writer.lock().unwrap().output.clone()).unwrap();
        assert_eq!(result, "[agent] partial\n");
    }

    #[test]
    fn test_agent_progress_writer_multiple_writes() {
        let test_writer = Arc::new(Mutex::new(TestWriter { output: Vec::new() }));
        let mut progress_writer = AgentProgressWriter::new("[child] ".to_string(), test_writer.clone());
        
        progress_writer.write_all(b"first ").unwrap();
        progress_writer.write_all(b"line\nsecond ").unwrap();
        progress_writer.write_all(b"line\n").unwrap();
        progress_writer.flush().unwrap();
        
        let result = String::from_utf8(test_writer.lock().unwrap().output.clone()).unwrap();
        assert_eq!(result, "[child] first line\n[child] second line\n");
    }

    #[test]
    fn test_agent_progress_writer_drop_flushes() {
        let test_writer = Arc::new(Mutex::new(TestWriter { output: Vec::new() }));
        
        {
            let mut progress_writer = AgentProgressWriter::new_default(test_writer.clone());
            progress_writer.write_all(b"hello\nworld").unwrap();
            // Drop the writer, which should flush any remaining buffer
        }
        
        // After drop, the buffer should have been flushed
        let result = String::from_utf8(test_writer.lock().unwrap().output.clone()).unwrap();
        assert_eq!(result, "[agent] hello\n[agent] world");
    }
}
