use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

const DEFAULT_MAX_MEMORY: i64 = 8 * 1024 * 1024; // 8MB
const CIRCULAR_BUFFER_SIZE: usize = 1000;

/// CircularBuffer is a fixed-size ring buffer for strings.
pub struct CircularBuffer {
    buf: Vec<String>,
    head: usize,
    size: usize,
    capacity: usize,
}

impl CircularBuffer {
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 { CIRCULAR_BUFFER_SIZE } else { capacity };
        Self {
            buf: vec![String::new(); cap],
            head: 0,
            size: 0,
            capacity: cap,
        }
    }

    pub fn append(&mut self, s: String) {
        self.buf[self.head] = s;
        self.head = (self.head + 1) % self.capacity;
        if self.size < self.capacity {
            self.size += 1;
        }
    }

    pub fn get_all(&self) -> Vec<String> {
        if self.size == 0 {
            return vec![];
        }
        let start = (self.head + self.capacity - self.size) % self.capacity;
        (0..self.size).map(|i| self.buf[(start + i) % self.capacity].clone()).collect()
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        if n == 0 || self.size == 0 {
            return vec![];
        }
        let count = n.min(self.size);
        let start = (self.head + self.capacity - count) % self.capacity;
        (0..count).map(|i| self.buf[(start + i) % self.capacity].clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.size
    }
}

/// BgTaskProgress is a snapshot of a background task's progress.
#[derive(Debug, Clone)]
pub struct BgTaskProgress {
    pub total_lines: i64,
    pub total_bytes: i64,
    pub last_activity: SystemTime,
    pub is_complete: bool,
    pub exit_code: i32,
    pub last_5_lines: String,
    pub last_100_lines: String,
    pub description: String,
    pub token_count: usize,
    pub tool_use_count: usize,
}

/// BgTaskOutputConfig configures how BgTaskOutput is created.
pub struct BgTaskOutputConfig {
    pub task_id: String,
    pub output_path: Option<String>,
    pub max_memory: i64,
    pub file_mode: bool,
}

/// BgTaskOutput manages output for background tasks.
pub struct BgTaskOutput {
    task_id: String,
    output_path: Option<PathBuf>,
    stdout_file: Option<File>,
    memory_buffer: Option<CircularBuffer>,
    memory_bytes: i64,
    spill_file: Option<File>,
    spill_path: Option<PathBuf>,
    total_lines: AtomicI64,
    total_bytes: AtomicI64,
    last_activity_ms: AtomicI64,
    description: Mutex<String>,
    is_complete: AtomicBool,
    exit_code: AtomicI32,
    token_count: Mutex<usize>,
    tool_use_count: Mutex<usize>,
    file_mode: bool,
    initialized: bool,
}

impl BgTaskOutput {
    pub fn new(config: BgTaskOutputConfig) -> Self {
        let max_mem = if config.max_memory <= 0 { DEFAULT_MAX_MEMORY } else { config.max_memory };
        let output_path = config.output_path.map(PathBuf::from);

        let mut stdout_file = None;
        let mut memory_buffer = None;

        if config.file_mode {
            if let Some(ref path) = output_path {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Ok(f) = OpenOptions::new().create(true).write(true).truncate(true).open(path) {
                    stdout_file = Some(f);
                }
            }
        } else {
            memory_buffer = Some(CircularBuffer::new(CIRCULAR_BUFFER_SIZE));
        }

        Self {
            task_id: config.task_id,
            output_path,
            stdout_file,
            memory_buffer,
            memory_bytes: 0,
            spill_file: None,
            spill_path: None,
            total_lines: AtomicI64::new(0),
            total_bytes: AtomicI64::new(0),
            last_activity_ms: AtomicI64::new(0),
            description: Mutex::new(String::new()),
            is_complete: AtomicBool::new(false),
            exit_code: AtomicI32::new(-1),
            token_count: Mutex::new(0),
            tool_use_count: Mutex::new(0),
            file_mode: config.file_mode,
            initialized: true,
        }
    }

    pub fn write_stdout(&mut self, data: &str) {
        if !self.initialized { return; }

        let byte_len = data.len() as i64;
        self.total_bytes.fetch_add(byte_len, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.last_activity_ms.store(now, Ordering::Relaxed);

        let new_lines = data.matches('\n').count() as i64;
        self.total_lines.fetch_add(new_lines, Ordering::Relaxed);

        if self.file_mode {
            if let Some(ref mut f) = self.stdout_file {
                let _ = f.write_all(data.as_bytes());
            }
            return;
        }

        // Pipe mode
        if let Some(ref mut buf) = self.memory_buffer {
            buf.append(data.to_string());
        }
        self.memory_bytes += byte_len;

        if self.memory_bytes > DEFAULT_MAX_MEMORY && self.spill_file.is_none() {
            self.spill_to_disk();
        }

        if let Some(ref mut f) = self.spill_file {
            let _ = f.write_all(data.as_bytes());
        }
    }

    pub fn write_stderr(&mut self, data: &str) {
        if !self.initialized { return; }
        let formatted = format!("STDERR:\n{}", data);
        self.write_stdout(&formatted);
    }

    fn spill_to_disk(&mut self) {
        let path = match &self.output_path {
            Some(p) => p.with_extension("spill"),
            None => return,
        };

        if let Ok(mut f) = OpenOptions::new().create(true).write(true).truncate(true).open(&path) {
            if let Some(ref buf) = self.memory_buffer {
                for line in buf.get_all() {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            self.spill_file = Some(f);
            self.spill_path = Some(path);
        }
    }

    pub fn get_stdout(&self) -> String {
        if !self.initialized { return String::new(); }

        if self.file_mode {
            if let Some(ref path) = self.output_path {
                if let Ok(data) = fs::read_to_string(path) {
                    return data;
                }
            }
            if let Some(ref buf) = self.memory_buffer {
                return buf.get_all().join("");
            }
            return String::new();
        }

        let mut result = String::new();

        if let Some(ref path) = self.spill_path {
            if let Ok(data) = fs::read_to_string(path) {
                result.push_str(&data);
            }
        }

        if let Some(ref buf) = self.memory_buffer {
            for line in buf.get_all() {
                result.push_str(&line);
            }
        }

        result
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        if !self.initialized { return vec![]; }

        if let Some(ref buf) = self.memory_buffer {
            return buf.tail(n);
        }

        if let Some(ref path) = self.output_path {
            return self.tail_file(path, n);
        }

        vec![]
    }

    fn tail_file(&self, path: &Path, n: usize) -> Vec<String> {
        let f = match File::open(path) {
            Ok(f) => f,
            Err(_) => return vec![],
        };

        let metadata = match f.metadata() {
            Ok(m) => m,
            Err(_) => return vec![],
        };

        let file_size = metadata.len();
        if file_size == 0 { return vec![]; }

        let mut f = BufReader::new(f);
        let buf_size = 4096u64.min(file_size);

        if f.seek(SeekFrom::End(-(buf_size as i64))).is_err() {
            return vec![];
        }

        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            match f.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
                    if !trimmed.is_empty() || !line.is_empty() {
                        lines.push(trimmed);
                    }
                }
                Err(_) => break,
            }
        }

        if lines.len() > n {
            lines = lines.split_off(lines.len() - n);
        }
        lines
    }

    pub fn get_progress(&self) -> BgTaskProgress {
        if !self.initialized {
            return BgTaskProgress {
                total_lines: 0, total_bytes: 0,
                last_activity: SystemTime::UNIX_EPOCH,
                is_complete: false, exit_code: -1,
                last_5_lines: String::new(), last_100_lines: String::new(),
                description: String::new(), token_count: 0, tool_use_count: 0,
            };
        }

        let total_lines = self.total_lines.load(Ordering::Relaxed);
        let total_bytes = self.total_bytes.load(Ordering::Relaxed);
        let last_activity_ms = self.last_activity_ms.load(Ordering::Relaxed);
        let is_complete = self.is_complete.load(Ordering::Relaxed);
        let exit_code = self.exit_code.load(Ordering::Relaxed);

        let description = self.description.lock().unwrap().clone();
        let token_count = *self.token_count.lock().unwrap();
        let tool_use_count = *self.tool_use_count.lock().unwrap();

        let last_activity = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(last_activity_ms as u64);

        let (last_5, last_100) = if let Some(ref buf) = self.memory_buffer {
            (buf.tail(5).join(""), buf.tail(100).join(""))
        } else if let Some(ref path) = self.output_path {
            let lines = self.tail_file(path, 100);
            let l100 = lines.join("\n");
            let l5 = if lines.len() > 5 { lines[lines.len()-5..].join("\n") } else { l100.clone() };
            (l5, l100)
        } else {
            (String::new(), String::new())
        };

        BgTaskProgress {
            total_lines, total_bytes, last_activity, is_complete, exit_code,
            last_5_lines: last_5, last_100_lines: last_100,
            description, token_count, tool_use_count,
        }
    }

    pub fn update_progress(&self, description: &str, tokens: usize, tool_uses: usize) {
        if !description.is_empty() {
            *self.description.lock().unwrap() = description.to_string();
        }
        if tokens > 0 {
            *self.token_count.lock().unwrap() = tokens;
        }
        if tool_uses > 0 {
            *self.tool_use_count.lock().unwrap() = tool_uses;
        }
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.last_activity_ms.store(now, Ordering::Relaxed);
    }

    pub fn set_complete(&mut self, code: i32) {
        self.is_complete.store(true, Ordering::Relaxed);
        self.exit_code.store(code, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.last_activity_ms.store(now, Ordering::Relaxed);
        self.stdout_file = None;
        self.spill_file = None;
    }

    pub fn is_complete(&self) -> bool { self.is_complete.load(Ordering::Relaxed) }
    pub fn get_exit_code(&self) -> i32 { self.exit_code.load(Ordering::Relaxed) }
    pub fn get_task_id(&self) -> &str { &self.task_id }
    pub fn get_output_path(&self) -> Option<&Path> { self.output_path.as_deref() }

    pub fn close(&mut self) {
        self.stdout_file = None;
        self.spill_file = None;
    }
}

/// BgTaskOutputStore is a thread-safe registry of background task outputs.
pub struct BgTaskOutputStore {
    tasks: RwLock<HashMap<String, Arc<Mutex<BgTaskOutput>>>>,
}

impl BgTaskOutputStore {
    pub fn new() -> Self {
        Self { tasks: RwLock::new(HashMap::new()) }
    }

    pub fn register(&self, task: Arc<Mutex<BgTaskOutput>>) {
        let id = task.lock().unwrap().get_task_id().to_string();
        self.tasks.write().unwrap().insert(id, task);
    }

    pub fn get(&self, task_id: &str) -> Option<Arc<Mutex<BgTaskOutput>>> {
        self.tasks.read().unwrap().get(task_id).cloned()
    }

    pub fn remove(&self, task_id: &str) {
        if let Some(task) = self.tasks.write().unwrap().remove(task_id) {
            task.lock().unwrap().close();
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.tasks.read().unwrap().keys().cloned().collect()
    }
}

/// GenerateOutputPath creates a standard output file path for a background task.
pub fn generate_output_path(base_dir: &str, task_id: &str) -> PathBuf {
    PathBuf::from(base_dir).join(".claude").join("task-outputs").join(format!("{}.txt", task_id))
}

/// ReadFileTail reads the last N bytes from a file.
pub fn read_file_tail(path: &Path, byte_count: usize) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    if size == 0 { return Ok(String::new()); }

    let read_size = byte_count.min(size as usize);
    f.seek(SeekFrom::End(-(read_size as i64)))?;

    let mut buf = vec![0u8; read_size];
    let n = f.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).to_string())
}

/// CountFileLines counts the total number of lines in a file.
pub fn count_file_lines(path: &Path) -> std::io::Result<i64> {
    let f = File::open(path)?;
    let metadata = f.metadata()?;
    if metadata.len() == 0 { return Ok(0); }

    let reader = BufReader::new(f);
    let mut count: i64 = 0;
    for line in reader.lines() {
        if line.is_ok() { count += 1; }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circular_buffer_append() {
        let mut cb = CircularBuffer::new(3);
        cb.append("a".to_string());
        cb.append("b".to_string());
        cb.append("c".to_string());
        assert_eq!(cb.get_all(), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_circular_buffer_overflow() {
        let mut cb = CircularBuffer::new(3);
        cb.append("a".to_string());
        cb.append("b".to_string());
        cb.append("c".to_string());
        cb.append("d".to_string()); // overwrites "a"
        assert_eq!(cb.get_all(), vec!["b", "c", "d"]);
    }

    #[test]
    fn test_circular_buffer_tail() {
        let mut cb = CircularBuffer::new(5);
        for i in 0..5 { cb.append(format!("line{}", i)); }
        assert_eq!(cb.tail(2), vec!["line3", "line4"]);
    }

    #[test]
    fn test_bg_task_output_pipe_mode() {
        let config = BgTaskOutputConfig {
            task_id: "test-task".to_string(),
            output_path: None,
            max_memory: 0,
            file_mode: false,
        };
        let mut output = BgTaskOutput::new(config);
        output.write_stdout("hello\n");
        output.write_stdout("world\n");
        assert_eq!(output.get_stdout(), "hello\nworld\n");
    }

    #[test]
    fn test_bg_task_output_progress() {
        let config = BgTaskOutputConfig {
            task_id: "test-progress".to_string(),
            output_path: None,
            max_memory: 0,
            file_mode: false,
        };
        let output = BgTaskOutput::new(config);
        output.update_progress("working", 100, 5);
        let progress = output.get_progress();
        assert_eq!(progress.description, "working");
        assert_eq!(progress.token_count, 100);
        assert_eq!(progress.tool_use_count, 5);
    }

    #[test]
    fn test_bg_task_output_set_complete() {
        let config = BgTaskOutputConfig {
            task_id: "test-complete".to_string(),
            output_path: None,
            max_memory: 0,
            file_mode: false,
        };
        let mut output = BgTaskOutput::new(config);
        assert!(!output.is_complete());
        output.set_complete(0);
        assert!(output.is_complete());
        assert_eq!(output.get_exit_code(), 0);
    }

    #[test]
    fn test_bg_task_output_store() {
        let store = BgTaskOutputStore::new();
        let config = BgTaskOutputConfig {
            task_id: "store-test".to_string(),
            output_path: None,
            max_memory: 0,
            file_mode: false,
        };
        let task = Arc::new(Mutex::new(BgTaskOutput::new(config)));
        store.register(task);
        assert!(store.get("store-test").is_some());
        assert_eq!(store.list(), vec!["store-test"]);
        store.remove("store-test");
        assert!(store.get("store-test").is_none());
    }

    #[test]
    fn test_generate_output_path() {
        let path = generate_output_path("/tmp", "task-123");
        assert_eq!(path.to_str().unwrap(), "/tmp/.claude/task-outputs/task-123.txt");
    }
}
