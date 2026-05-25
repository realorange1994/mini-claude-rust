//! Cron expression parsing and scheduling.
//! Ported from upstream Go: cron.go (468 lines), cron_tasks.go (409 lines),
//! cron_scheduler.go (466 lines), cron_tools.go (248 lines).
//!
//! Provides a 5-field cron expression parser, task management with session
//! and file-backed storage, jitter-based scheduling to prevent herd effects,
//! scheduler lock for single-instance operation, missed task detection, and
//! tool implementations (cron_create, cron_delete, cron_list).

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{Datelike, Timelike};

// ─── Cron Expression Parsing ─────────────────────────────────────────────────

/// A single cron field (e.g., "*/5", "1,15", "3-5").
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum CronField {
    /// Match any value (*).
    Any,
    /// Match exact values (1,2,3).
    Exact(Vec<i32>),
    /// Match a range (1-5).
    Range(i32, i32),
    /// Match with step (*/5 or 1-10/2).
    Step(Option<(i32, i32)>, i32),
}

impl CronField {
    /// Parse a single cron field.
    pub fn parse(field: &str) -> Result<Self, String> {
        let field = field.trim();
        if field == "*" {
            return Ok(CronField::Any);
        }

        // Step: */5 or 1-10/2
        if let Some(slash_pos) = field.find('/') {
            let range_part = &field[..slash_pos];
            let step: i32 = field[slash_pos + 1..]
                .parse()
                .map_err(|_| format!("invalid step: {}", &field[slash_pos + 1..]))?;

            let range = if range_part == "*" {
                None
            } else if let Some(dash_pos) = range_part.find('-') {
                let start: i32 = range_part[..dash_pos]
                    .parse()
                    .map_err(|_| format!("invalid range start: {}", &range_part[..dash_pos]))?;
                let end: i32 = range_part[dash_pos + 1..]
                    .parse()
                    .map_err(|_| format!("invalid range end: {}", &range_part[dash_pos + 1..]))?;
                Some((start, end))
            } else {
                return Err(format!("invalid step range: {}", range_part));
            };

            return Ok(CronField::Step(range, step));
        }

        // Range: 1-5
        if let Some(dash_pos) = field.find('-') {
            let start: i32 = field[..dash_pos]
                .parse()
                .map_err(|_| format!("invalid range start: {}", &field[..dash_pos]))?;
            let end: i32 = field[dash_pos + 1..]
                .parse()
                .map_err(|_| format!("invalid range end: {}", &field[dash_pos + 1..]))?;
            return Ok(CronField::Range(start, end));
        }

        // Comma-separated: 1,2,3
        if field.contains(',') {
            let mut values = Vec::new();
            for part in field.split(',') {
                let v: i32 = part
                    .trim()
                    .parse()
                    .map_err(|_| format!("invalid value: {}", part))?;
                values.push(v);
            }
            return Ok(CronField::Exact(values));
        }

        // Single value
        let v: i32 = field
            .parse()
            .map_err(|_| format!("invalid value: {}", field))?;
        Ok(CronField::Exact(vec![v]))
    }

    /// Check if a value matches this field.
    pub fn matches(&self, value: i32) -> bool {
        match self {
            CronField::Any => true,
            CronField::Exact(vals) => vals.contains(&value),
            CronField::Range(start, end) => value >= *start && value <= *end,
            CronField::Step(range, step) => {
                if *step == 0 {
                    return false;
                }
                let in_range = match range {
                    None => true,
                    Some((start, end)) => value >= *start && value <= *end,
                };
                in_range && (value % *step == 0)
            }
        }
    }

    /// Convert back to cron string representation.
    pub fn to_cron_string(&self) -> String {
        match self {
            CronField::Any => "*".to_string(),
            CronField::Exact(vals) => {
                if vals.len() == 1 {
                    format!("{}", vals[0])
                } else {
                    vals.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
                }
            }
            CronField::Range(start, end) => format!("{start}-{end}"),
            CronField::Step(None, step) => format!("*/{step}"),
            CronField::Step(Some((start, end)), step) => format!("{start}-{end}/{step}"),
        }
    }
}

/// Parsed cron expression (5-field: M H DoM Mon DoW).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CronExpr {
    pub minute: CronField,
    pub hour: CronField,
    pub day_of_month: CronField,
    pub month: CronField,
    pub day_of_week: CronField,
}

impl CronExpr {
    /// Parse a 5-field cron expression: "M H DoM Mon DoW".
    pub fn parse(expr: &str) -> Result<Self, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "cron expression must have 5 fields, got {}: {}",
                fields.len(),
                expr
            ));
        }

        Ok(CronExpr {
            minute: CronField::parse(fields[0])?,
            hour: CronField::parse(fields[1])?,
            day_of_month: CronField::parse(fields[2])?,
            month: CronField::parse(fields[3])?,
            day_of_week: CronField::parse(fields[4])?,
        })
    }

    /// Convert back to cron expression string.
    pub fn to_cron_string(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.minute.to_cron_string(),
            self.hour.to_cron_string(),
            self.day_of_month.to_cron_string(),
            self.month.to_cron_string(),
            self.day_of_week.to_cron_string(),
        )
    }
}

// ─── Cron Task Data Model ────────────────────────────────────────────────────

/// CronTask represents a scheduled prompt.
///
/// Two flavors:
/// - One-shot (recurring: false) — fire once, then auto-delete.
/// - Recurring (recurring: true) — fire on schedule, reschedule from now,
///   persist until deleted or auto-expire after 7 days.
#[derive(Debug, Clone)]
pub struct CronTask {
    pub id: String,
    pub expr: CronExpr,
    pub prompt: String,
    pub created_at_ms: i64,
    pub last_fired_at_ms: Option<i64>,
    pub recurring: bool,
    pub permanent: bool,
    /// Runtime-only fields (never written to disk):
    pub durable: bool,
    pub agent_id: String,
}

// ─── Jitter Configuration ────────────────────────────────────────────────────

/// Scheduler jitter tuning knobs to prevent thundering herd effects.
#[derive(Debug, Clone, Copy)]
pub struct CronJitterConfig {
    /// Recurring forward delay as fraction of interval.
    pub recurring_frac: f64,
    /// Upper bound on recurring delay (ms).
    pub recurring_cap_ms: i64,
    /// One-shot backward lead: maximum ms to fire early.
    pub oneshot_max_ms: i64,
    /// One-shot backward lead: minimum ms to fire early.
    pub oneshot_floor_ms: i64,
    /// Jitter fires where minute % N == 0.
    pub oneshot_minute_mod: i64,
    /// Auto-expire recurring tasks this many ms after creation (0 = never).
    pub recurring_max_age_ms: i64,
}

impl Default for CronJitterConfig {
    fn default() -> Self {
        Self {
            recurring_frac: 0.1,
            recurring_cap_ms: 15 * 60 * 1000,       // 15 minutes
            oneshot_max_ms: 90 * 1000,               // 90 seconds
            oneshot_floor_ms: 0,
            oneshot_minute_mod: 30,                  // :00 and :30
            recurring_max_age_ms: 7 * 24 * 60 * 60 * 1000, // 7 days
        }
    }
}

// ─── Session Task Store ──────────────────────────────────────────────────────

static SESSION_CRON_TASKS: Lazy<Mutex<HashMap<String, CronTask>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn session_cron_tasks() -> std::sync::MutexGuard<'static, HashMap<String, CronTask>> {
    SESSION_CRON_TASKS.lock().unwrap()
}

pub fn add_session_cron_task(t: CronTask) {
    session_cron_tasks().insert(t.id.clone(), t);
}

pub fn get_session_cron_tasks() -> Vec<CronTask> {
    session_cron_tasks().values().cloned().collect()
}

pub fn remove_session_cron_tasks(ids: &[String]) -> usize {
    let mut map = session_cron_tasks();
    let mut count = 0;
    for id in ids {
        if map.remove(id).is_some() {
            count += 1;
        }
    }
    count
}

// ─── File I/O ────────────────────────────────────────────────────────────────

/// JSON structure for .claude/scheduled_tasks.json.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CronFile {
    tasks: Vec<CronTaskDisk>,
}

/// Disk-only representation (no runtime fields).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CronTaskDisk {
    id: String,
    cron: String,
    prompt: String,
    #[serde(rename = "createdAt")]
    created_at_ms: i64,
    #[serde(rename = "lastFiredAt", default, skip_serializing_if = "Option::is_none")]
    last_fired_at_ms: Option<i64>,
    #[serde(rename = "recurring", default)]
    recurring: bool,
    #[serde(rename = "permanent", default)]
    permanent: bool,
}

const CRON_FILE_NAME: &str = "scheduled_tasks.json";

fn get_cron_file_path(dir: &str) -> PathBuf {
    let dir = if dir.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        PathBuf::from(dir)
    };
    dir.join(CRON_FILE_NAME)
}

/// Read and parse .claude/scheduled_tasks.json.
fn read_cron_tasks(dir: &str) -> Result<Vec<CronTask>, std::io::Error> {
    let path = get_cron_file_path(dir);
    let data = match fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let file: CronFile = match serde_json::from_str(&data) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for t in file.tasks {
        if t.id.is_empty() || t.cron.is_empty() || t.prompt.is_empty() || t.created_at_ms == 0 {
            continue;
        }
        if CronExpr::parse(&t.cron).is_err() {
            continue;
        }
        out.push(CronTask {
            id: t.id,
            expr: CronExpr::parse(&t.cron).unwrap(),
            prompt: t.prompt,
            created_at_ms: t.created_at_ms,
            last_fired_at_ms: t.last_fired_at_ms,
            recurring: t.recurring,
            permanent: t.permanent,
            durable: true,
            agent_id: String::new(),
        });
    }
    Ok(out)
}

/// Write tasks to .claude/scheduled_tasks.json.
fn write_cron_tasks(tasks: &[CronTask], dir: &str) -> Result<(), std::io::Error> {
    let cron_dir = if dir.is_empty() {
        std::env::current_dir().unwrap_or_default().join(".claude")
    } else {
        PathBuf::from(dir).join(".claude")
    };
    fs::create_dir_all(&cron_dir)?;

    let disk_tasks: Vec<CronTaskDisk> = tasks
        .iter()
        .map(|t| CronTaskDisk {
            id: t.id.clone(),
            cron: t.expr.to_cron_string(),
            prompt: t.prompt.clone(),
            created_at_ms: t.created_at_ms,
            last_fired_at_ms: t.last_fired_at_ms,
            recurring: t.recurring,
            permanent: t.permanent,
        })
        .collect();

    let file = CronFile { tasks: disk_tasks };
    let data = serde_json::to_string_pretty(&file).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    fs::write(get_cron_file_path(dir), data + "\n")
}

/// Generate an 8-hex-char task ID.
fn generate_task_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 4] = rng.gen();
    format!("{:02x}{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2], bytes[3])
}

/// Add a new task. Returns generated ID.
pub fn add_cron_task(
    cron: &str,
    prompt: &str,
    recurring: bool,
    durable: bool,
    agent_id: &str,
    dir: &str,
) -> Result<String, std::io::Error> {
    let id = generate_task_id();
    let expr = CronExpr::parse(cron).expect("cron already validated");

    let task = CronTask {
        id: id.clone(),
        expr,
        prompt: prompt.to_string(),
        created_at_ms: now_ms(),
        last_fired_at_ms: None,
        recurring,
        permanent: false,
        durable,
        agent_id: if agent_id.is_empty() { String::new() } else { agent_id.to_string() },
    };

    if !durable {
        add_session_cron_task(task);
        return Ok(id);
    }

    let mut tasks = read_cron_tasks(dir)?;
    tasks.push(task);
    write_cron_tasks(&tasks, dir)?;
    Ok(id)
}

/// Remove tasks by ID from both session store and file.
pub fn remove_cron_tasks(ids: &[String], dir: &str) -> Result<(), std::io::Error> {
    if ids.is_empty() {
        return Ok(());
    }

    let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let session_count = remove_session_cron_tasks(ids);
    if session_count == ids.len() {
        return Ok(());
    }

    let tasks = read_cron_tasks(dir)?;
    let task_count = tasks.len();
    let file_count = tasks.iter().filter(|t| t.durable).count();
    let remaining: Vec<CronTask> = tasks
        .into_iter()
        .filter(|t| !id_set.contains(t.id.as_str()))
        .collect();

    if remaining.len() == task_count - session_count {
        // Check if all remaining were already in session
        let remaining_file = remaining.iter().filter(|t| t.durable).count();
        if remaining_file == file_count {
            return Ok(());
        }
    }
    write_cron_tasks(&remaining, dir)
}

/// List all tasks (file + session merged).
pub fn list_all_cron_tasks(dir: &str) -> Vec<CronTask> {
    let file_tasks = read_cron_tasks(dir).unwrap_or_default();
    let session_tasks = get_session_cron_tasks();
    file_tasks.into_iter().chain(session_tasks).collect()
}

/// Mark lastFiredAt on recurring tasks and persist.
pub fn mark_cron_tasks_fired(ids: &[String], fired_at_ms: i64, dir: &str) -> Result<(), std::io::Error> {
    if ids.is_empty() {
        return Ok(());
    }
    let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();

    let mut tasks = read_cron_tasks(dir)?;
    let mut changed = false;
    for t in &mut tasks {
        if id_set.contains(t.id.as_str()) {
            t.last_fired_at_ms = Some(fired_at_ms);
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    write_cron_tasks(&tasks, dir)
}

/// Quick check if cron file has valid tasks.
pub fn has_cron_tasks_sync(dir: &str) -> bool {
    let path = get_cron_file_path(dir);
    let data = match fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let file: Result<CronFile, _> = serde_json::from_str(&data);
    match file {
        Ok(f) => !f.tasks.is_empty(),
        Err(_) => false,
    }
}

/// Current epoch in milliseconds.
fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// Convert epoch ms to chrono::DateTime.
fn time_unix_ms(ms: i64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
}

// ─── Cron Computation Helpers ────────────────────────────────────────────────

/// Compute next fire time in epoch ms strictly after `from_ms`.
fn next_cron_run_ms(cron: &str, from_ms: i64) -> Option<i64> {
    let expr = CronExpr::parse(cron).ok()?;
    compute_next_cron_run(&expr, from_ms)
}

/// Compute next datetime strictly after `from_ms` matching the cron expression.
/// Returns None if no match within 366 days.
pub fn compute_next_cron_run(expr: &CronExpr, from_ms: i64) -> Option<i64> {
    use chrono::TimeZone;
    // Start from the next whole minute
    let from_dt = chrono::Local.timestamp_millis_opt(from_ms).single()?;
    let trunc_ms = from_dt.timestamp_millis() - (from_dt.timestamp_millis() % 60000);
    let t0 = chrono::Local.timestamp_millis_opt(trunc_ms).single()?;
    let mut t = t0.checked_add_signed(chrono::Duration::minutes(1))?;

    for _ in 0..(366 * 24 * 60) {
        let month = t.month() as i32;
        if !expr.month.matches(month) {
            // Jump to start of next month
            let (y, m) = if t.month() == 12 {
                (t.year() + 1, 1)
            } else {
                (t.year(), t.month() + 1)
            };
            t = chrono::Local.with_ymd_and_hms(y, m, 1, 0, 0, 0).single()?;
            continue;
        }

        let dom = t.day() as i32;
        let dow = datetime_to_cron_weekday(&t);

        // Day matching: cron uses (dom AND month) OR (dow AND month)
        // When both dom and dow are *, any day matches
        let dom_wild = matches!(&expr.day_of_month, CronField::Any);
        let dow_wild = matches!(&expr.day_of_week, CronField::Any);

        let day_matches = if dom_wild && dow_wild {
            true
        } else if dom_wild {
            expr.day_of_week.matches(dow)
        } else if dow_wild {
            expr.day_of_month.matches(dom)
        } else {
            expr.day_of_month.matches(dom) || expr.day_of_week.matches(dow)
        };

        if !day_matches {
            let next_day = chrono::Local.with_ymd_and_hms(
                t.year(), t.month(), t.day(), 0, 0, 0
            ).single()?
            .checked_add_signed(chrono::Duration::days(1))?;
            t = next_day;
            continue;
        }

        if !expr.hour.matches(t.hour() as i32) {
            t = t.checked_add_signed(chrono::Duration::hours(1))?;
            t = chrono::Local.with_ymd_and_hms(
                t.year(), t.month(), t.day(), t.hour(), 0, 0
            ).single()?;
            continue;
        }

        if !expr.minute.matches(t.minute() as i32) {
            t = t.checked_add_signed(chrono::Duration::minutes(1))?;
            continue;
        }

        return Some(t.timestamp_millis());
    }
    None
}

/// Convert chrono weekday to cron weekday (Sunday=0).
fn datetime_to_cron_weekday(dt: &chrono::DateTime<chrono::Local>) -> i32 {
    let wday = dt.weekday();
    match wday.number_from_monday() as i32 {
        1 => 1, // Mon
        2 => 2, // Tue
        3 => 3, // Wed
        4 => 4, // Thu
        5 => 5, // Fri
        6 => 6, // Sat
        7 => 0, // Sun
        _ => 0,
    }
}

/// Check if a datetime matches the cron expression.
pub fn matches_datetime(expr: &CronExpr, dt: &chrono::DateTime<chrono::Local>) -> bool {
    let minute = dt.minute() as i32;
    let hour = dt.hour() as i32;
    let day = dt.day() as i32;
    let month = dt.month() as i32;
    let weekday = datetime_to_cron_weekday(dt);

    if !expr.minute.matches(minute) { return false; }
    if !expr.hour.matches(hour) { return false; }
    if !expr.month.matches(month) { return false; }

    // Day matching logic
    let dom_wild = matches!(&expr.day_of_month, CronField::Any);
    let dow_wild = matches!(&expr.day_of_week, CronField::Any);

    let day_matches = if dom_wild && dow_wild {
        true
    } else if dom_wild {
        expr.day_of_week.matches(weekday)
    } else if dow_wild {
        expr.day_of_month.matches(day)
    } else {
        expr.day_of_month.matches(day) || expr.day_of_week.matches(weekday)
    };
    day_matches
}

// ─── Jitter ──────────────────────────────────────────────────────────────────

/// Deterministic value in [0, 1) from task ID.
fn jitter_frac(task_id: &str) -> f64 {
    if task_id.len() < 8 {
        return 0.0;
    }
    // Parse first 8 hex chars as u32
    let mut n: u32 = 0;
    for (i, byte) in task_id[..8].as_bytes().iter().enumerate() {
        let val = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return 0.0,
        };
        n |= (val as u32) << (28 - i * 4);
    }
    n as f64 / 0x100000000u64 as f64
}

/// Next fire time with forward jitter for recurring tasks.
fn jittered_next_cron_run_ms(cron: &str, from_ms: i64, task_id: &str, cfg: CronJitterConfig) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let t2 = next_cron_run_ms(cron, t1)?;
    // No second match (pinned date) → no herd risk
    let jitter = (jitter_frac(task_id) * cfg.recurring_frac * (t2 - t1) as f64) as i64;
    let jitter = jitter.min(cfg.recurring_cap_ms);
    Some(t1 + jitter)
}

/// Next fire time with backward jitter for one-shot tasks.
fn oneshot_jittered_next_cron_run_ms(cron: &str, from_ms: i64, task_id: &str, cfg: CronJitterConfig) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let fire_time = time_unix_ms(t1);
    if fire_time.minute() as i64 % cfg.oneshot_minute_mod != 0 {
        return Some(t1);
    }
    let lead = cfg.oneshot_floor_ms + (jitter_frac(task_id) * (cfg.oneshot_max_ms - cfg.oneshot_floor_ms) as f64) as i64;
    let result = t1 - lead;
    Some(result.max(from_ms))
}

/// Find tasks whose next scheduled run (from createdAt) is in the past.
fn find_missed_tasks(tasks: &[CronTask], now_ms: i64) -> Vec<CronTask> {
    tasks
        .iter()
        .filter(|t| {
            if let Some(next) = next_cron_run_ms(&t.expr.to_cron_string(), t.created_at_ms) {
                next < now_ms
            } else {
                false
            }
        })
        .cloned()
        .collect()
}

/// Check if a recurring task should be auto-expired.
fn is_recurring_task_aged(t: &CronTask, now_ms: i64, max_age_ms: i64) -> bool {
    max_age_ms > 0 && t.recurring && !t.permanent && (now_ms - t.created_at_ms) >= max_age_ms
}

// ─── CronToHuman ─────────────────────────────────────────────────────────────

const DAY_NAMES: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

fn format_local_time(minute: i32, hour: i32) -> String {
    let h = if hour == 0 { 12 } else if hour > 12 { hour - 12 } else { hour };
    let ampm = if hour >= 12 { "PM" } else { "AM" };
    format!("{h}:{minute:02} {ampm}")
}

/// Convert a cron expression to human-readable form.
pub fn cron_to_human(cron: &str) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 5 {
        return cron.to_string();
    }

    let (minute, hour, dom, month, dow) = (parts[0], parts[1], parts[2], parts[3], parts[4]);

    // Every N minutes: */N * * * *
    if dom == "*" && month == "*" && dow == "*" && hour == "*" {
        if let Some(n_str) = minute.strip_prefix("*/") {
            if let Ok(n) = n_str.parse::<i32>() {
                if n > 0 {
                    return if n == 1 {
                        "Every minute".to_string()
                    } else {
                        format!("Every {n} minutes")
                    };
                }
            }
        }
    }

    // Every hour at :M: M * * * *
    if hour == "*" && dom == "*" && month == "*" && dow == "*" {
        if let Ok(n) = minute.parse::<i32>() {
            if n == 0 {
                return "Every hour".to_string();
            }
            return format!("Every hour at :{n:02}");
        }
    }

    // Every N hours: 0 */N * * *
    if dom == "*" && month == "*" && dow == "*" {
        if let Some(n_str) = hour.strip_prefix("*/") {
            if let (Ok(m), Ok(n)) = (minute.parse::<i32>(), n_str.parse::<i32>()) {
                if n > 0 {
                    let suffix = if m != 0 {
                        format!(" at :{m:02}")
                    } else {
                        String::new()
                    };
                    return if n == 1 {
                        format!("Every hour{suffix}")
                    } else {
                        format!("Every {n} hours{suffix}")
                    };
                }
            }
        }
    }

    // Need single-digit minute and hour for remaining cases
    let (Ok(m), Ok(h)) = (minute.parse::<i32>(), hour.parse::<i32>()) else {
        return cron.to_string();
    };

    // Daily: M H * * *
    if dom == "*" && month == "*" && dow == "*" {
        return format!("Every day at {}", format_local_time(m, h));
    }

    // Specific day of week: M H * * D
    if dom == "*" && month == "*" && dow.len() == 1 {
        if let Ok(d) = dow.parse::<i32>() {
            return format!("Every {} at {}", DAY_NAMES[(d % 7) as usize], format_local_time(m, h));
        }
    }

    // Weekdays: M H * * 1-5
    if dom == "*" && month == "*" && dow == "1-5" {
        return format!("Weekdays at {}", format_local_time(m, h));
    }

    cron.to_string()
}

// ─── Scheduler ───────────────────────────────────────────────────────────────

const CHECK_INTERVAL_MS: u64 = 1000;
const INFINITY_MS: i64 = i64::MAX;

/// Result of a task firing.
#[derive(Debug)]
pub struct FireAction {
    pub task_id: String,
    pub prompt: String,
    pub is_recurring: bool,
}

/// Scheduler lock data.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SchedulerLockData {
    session_id: String,
    pid: u32,
    acquired_at: i64,
}

const LOCK_FILE_NAME: &str = "scheduled_tasks.lock";

fn get_lock_path(dir: &str) -> PathBuf {
    let dir = if dir.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        PathBuf::from(dir)
    };
    dir.join(LOCK_FILE_NAME)
}

/// Acquire exclusive scheduler lock.
fn try_acquire_scheduler_lock(dir: &str) -> bool {
    let lock = SchedulerLockData {
        session_id: format!("miniclaude-{}", std::process::id()),
        pid: std::process::id(),
        acquired_at: now_ms(),
    };

    let lock_path = get_lock_path(dir);
    let claude_dir = lock_path.parent().unwrap();
    let _ = fs::create_dir_all(claude_dir);

    // Try exclusive create
    let data = serde_json::to_string(&lock).unwrap();
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path);

    match result {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(data.as_bytes());
            true
        }
        Err(_) => {
            // Read existing lock
            if let Ok(existing_data) = fs::read_to_string(&lock_path) {
                if let Ok(existing) = serde_json::from_str::<SchedulerLockData>(&existing_data) {
                    if existing.pid == std::process::id() {
                        return true; // Ours
                    }
                    // Check if owner process is alive
                    if is_process_running(existing.pid) {
                        return false;
                    }
                }
            }
            // Stale lock — remove and retry
            let _ = fs::remove_file(&lock_path);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(data.as_bytes());
                    true
                }
                Err(_) => false,
            }
        }
    }
}

fn release_scheduler_lock(dir: &str) {
    let lock_path = get_lock_path(dir);
    if let Ok(data) = fs::read_to_string(&lock_path) {
        if let Ok(lock) = serde_json::from_str::<SchedulerLockData>(&data) {
            if lock.pid == std::process::id() {
                let _ = fs::remove_file(&lock_path);
            }
        }
    }
}

/// Check if a process is still running by PID.
fn is_process_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // On Unix, send signal 0 to check
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, 0) == 0
        }
    }
    // On Windows, conservative: assume alive
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Build missed task notification text.
fn build_missed_task_notification(missed: &[CronTask]) -> String {
    let plural = missed.len() > 1;
    let (were, they, these, _each) = if plural {
        ("s were", "They have", "these prompts", "each one")
    } else {
        (" was", "It has", "this prompt", "it")
    };

    let header = format!(
        "The following one-shot scheduled task{were} missed while Claude was not running. \
         {they} already been removed from .claude/scheduled_tasks.json.\n\n\
         Do NOT execute {these} yet. \
         First ask the user whether to run {these} now. \
         Only execute if the user confirms.",
    );

    let blocks: Vec<String> = missed.iter().map(|t| {
        let created = time_unix_ms(t.created_at_ms);
        let meta = format!("[{}, created {}]", cron_to_human(&t.expr.to_cron_string()), created.format("%Y-%m-%d %H:%M:%S"));

        // Find longest backtick run in prompt
        let longest_run = t.prompt.chars().fold((0, 0), |(max, cur), c| {
            if c == '`' {
                let n = cur + 1;
                (max.max(n), n)
            } else {
                (max, 0)
            }
        }).0;

        let fence_len = 3.max(longest_run + 1);
        let fence = "`".repeat(fence_len);

        format!("{meta}\n{fence}\n{}\n{fence}", t.prompt)
    }).collect();

    format!("{}\n\n{}", header, blocks.join("\n\n"))
}

/// In-memory cron scheduler with file persistence and lifecycle management.
/// Follows upstream design: lock-based single-instance, 1s check interval,
/// jitter-based scheduling, missed one-shot detection on startup.
pub struct CronScheduler {
    tasks: Vec<CronTask>,
    next_fire_at: HashMap<String, i64>,
    missed_asked: std::collections::HashSet<String>,
    in_flight: std::collections::HashSet<String>,
    is_owner: bool,
    stopped: bool,
    dir: String,
    on_fire: Option<Box<dyn Fn(String) + Send>>,
}

impl CronScheduler {
    /// Create a new scheduler (not started).
    pub fn new(dir: String) -> Self {
        Self {
            tasks: Vec::new(),
            next_fire_at: HashMap::new(),
            missed_asked: std::collections::HashSet::new(),
            in_flight: std::collections::HashSet::new(),
            is_owner: false,
            stopped: false,
            dir,
            on_fire: None,
        }
    }

    /// Set the callback for when a task fires.
    pub fn set_on_fire<F: Fn(String) + Send + 'static>(&mut self, callback: F) {
        self.on_fire = Some(Box::new(callback));
    }

    /// Start the scheduler: acquire lock, load tasks, surface missed ones, begin tick.
    pub fn start(&mut self) {
        if self.stopped {
            return;
        }

        self.is_owner = try_acquire_scheduler_lock(&self.dir);

        // Load file-backed tasks
        if let Ok(tasks) = read_cron_tasks(&self.dir) {
            self.tasks = tasks;
        }

        // Surface missed one-shot tasks
        let now = now_ms();
        let missed = find_missed_tasks(&self.tasks, now);
        let mut missed_oneshot_ids = Vec::new();
        for t in &missed {
            if !t.recurring && !self.missed_asked.contains(&t.id) {
                self.missed_asked.insert(t.id.clone());
                self.next_fire_at.insert(t.id.clone(), INFINITY_MS);
                missed_oneshot_ids.push(t.id.clone());
            }
        }

        if !missed_oneshot_ids.is_empty() {
            let missed_oneshot: Vec<CronTask> = self.tasks.iter()
                .filter(|t| missed_oneshot_ids.contains(&t.id))
                .cloned()
                .collect();
            let notification = build_missed_task_notification(&missed_oneshot);
            if let Some(ref cb) = self.on_fire {
                cb(notification);
            }
            // Remove missed tasks from disk
            let _ = remove_cron_tasks(&missed_oneshot_ids, &self.dir);
        }

        self.schedule_check();
    }

    /// Stop the scheduler.
    pub fn stop(&mut self) {
        self.stopped = true;
        if self.is_owner {
            self.is_owner = false;
            release_scheduler_lock(&self.dir);
        }
    }

    /// Get the earliest pending fire time, or 0 if nothing pending.
    pub fn get_next_fire_time(&self) -> i64 {
        self.next_fire_at.values()
            .filter(|&&v| v != INFINITY_MS)
            .min()
            .copied()
            .unwrap_or(0)
    }

    /// Schedule the next check tick.
    fn schedule_check(&mut self) {
        if self.stopped {
            return;
        }
        // Note: In a real async implementation this would use tokio::time::interval.
        // For now, this is a placeholder — the check() method would be called
        // by the agent loop's main event loop or a separate thread.
        // We expose `check()` for synchronous invocation.
    }

    /// Run one scheduler tick: evaluate all tasks, fire those due.
    /// Returns list of fire actions for the caller to handle.
    pub fn check(&mut self) -> Vec<FireAction> {
        if self.stopped {
            return Vec::new();
        }

        let now = now_ms();
        let mut seen = std::collections::HashSet::new();
        let mut fired_recurring_ids = Vec::new();
        let mut actions = Vec::new();

        // Process file-backed tasks (only if we own the lock)
        if self.is_owner {
            // Collect task data into owned values to avoid borrowing self
            let task_data: Vec<_> = self.tasks.iter().map(|t| {
                (t.id.clone(), t.expr.to_cron_string(), t.recurring,
                 t.created_at_ms, t.last_fired_at_ms, t.prompt.clone(), false)
            }).collect();
            let (ids, cron_strs, recurrences, created_ats, last_fired_ats, prompts, is_sessions_vec): (Vec<_>, Vec<_>, Vec<_>, Vec<_>, Vec<_>, Vec<_>, Vec<_>) =
                task_data.into_iter().fold(
                    (vec![], vec![], vec![], vec![], vec![], vec![], vec![]),
                    |mut acc, (id, cron, rec, created, last, prompt, is_session)| {
                        acc.0.push(id); acc.1.push(cron); acc.2.push(rec);
                        acc.3.push(created); acc.4.push(last); acc.5.push(prompt);
                        acc.6.push(is_session); acc
                    });
            for i in 0..ids.len() {
                let cron_str = cron_strs[i].clone();
                let cfg = CronJitterConfig::default();
                if let Some(action) = self.process_task(
                    ids[i].clone(), cron_str, recurrences[i],
                    created_ats[i], last_fired_ats[i], prompts[i].clone(),
                    is_sessions_vec[i], now, cfg, &mut seen,
                ) {
                    if action.is_recurring {
                        fired_recurring_ids.push(action.task_id.clone());
                    }
                    actions.push(action);
                }
            }
            // Persist lastFiredAt for recurring tasks
            if !fired_recurring_ids.is_empty() {
                for id in &fired_recurring_ids {
                    self.in_flight.insert(id.clone());
                }
                let ids_clone = fired_recurring_ids.clone();
                let dir = self.dir.clone();
                // Fire-and-forget persist (best effort)
                let _ = std::thread::spawn(move || {
                    let _ = mark_cron_tasks_fired(&ids_clone, now, &dir);
                });
                for id in &fired_recurring_ids {
                    self.in_flight.remove(id);
                }
            }
        }

        // Process session-only tasks
        for t in get_session_cron_tasks() {
            let cron_str = t.expr.to_cron_string();
            let cfg = CronJitterConfig::default();
            if let Some(action) = self.process_task(
                t.id.clone(), cron_str, t.recurring,
                t.created_at_ms, t.last_fired_at_ms, t.prompt.clone(),
                true, now, cfg, &mut seen,
            ) {
                if action.is_recurring {
                    fired_recurring_ids.push(action.task_id.clone());
                }
                actions.push(action);
            }
        }

        // If no live tasks, clear the schedule
        if seen.is_empty() {
            self.next_fire_at.clear();
            return actions;
        }

        // Evict stale schedule entries
        self.next_fire_at.retain(|id, _| seen.contains(id));

        actions
    }

    /// Evaluate a single task, fire if due. Returns action if fired.
    fn process_task(
        &mut self,
        task_id: String,
        cron_str: String,
        is_recurring: bool,
        created_at_ms: i64,
        last_fired_at_ms: Option<i64>,
        prompt: String,
        is_session: bool,
        now: i64,
        cfg: CronJitterConfig,
        seen: &mut std::collections::HashSet<String>,
    ) -> Option<FireAction> {
        seen.insert(task_id.clone());

        if self.in_flight.contains(&task_id) {
            return None;
        }

        let next = self.next_fire_at.entry(task_id.clone()).or_insert_with(|| {
            if is_recurring {
                let last_anchor = last_fired_at_ms.unwrap_or(created_at_ms);
                jittered_next_cron_run_ms(&cron_str, last_anchor, &task_id, cfg).unwrap_or(INFINITY_MS)
            } else {
                oneshot_jittered_next_cron_run_ms(&cron_str, created_at_ms, &task_id, cfg).unwrap_or(INFINITY_MS)
            }
        });

        if now < *next {
            return None;
        }

        // Fire!
        let action = FireAction {
            task_id: task_id.clone(),
            prompt: prompt.clone(),
            is_recurring,
        };

        if let Some(ref cb) = self.on_fire {
            cb(prompt.clone());
        }

        let aged = is_recurring && !is_session
            && cfg.recurring_max_age_ms > 0
            && (now - created_at_ms) >= cfg.recurring_max_age_ms;

        if is_recurring && !aged {
            // Reschedule from now
            let new_next = jittered_next_cron_run_ms(&cron_str, now, &task_id, cfg).unwrap_or(INFINITY_MS);
            self.next_fire_at.insert(task_id.clone(), new_next);
        } else if is_session {
            // One-shot session task: remove from memory
            remove_session_cron_tasks(&[task_id.clone()]);
            self.next_fire_at.remove(&task_id);
        } else {
            // One-shot file task: delete from disk
            self.in_flight.insert(task_id.clone());
            let ids = vec![task_id.clone()];
            let dir = self.dir.clone();
            let _ = std::thread::spawn(move || {
                let _ = remove_cron_tasks(&ids, &dir);
            });
            self.next_fire_at.remove(&task_id);
        }

        Some(action)
    }
}

// ─── Cron Tools ──────────────────────────────────────────────────────────────

use crate::tools::{Tool, ToolPermissionResult, ToolResult, ToolCapability, ApprovalRequirement};

const MAX_JOBS: usize = 50;

fn tool_success(output: impl Into<String>) -> ToolResult {
    ToolResult {
        output: output.into(),
        is_error: false,
        metadata: Default::default(),
        mode_change: None,
    }
}

fn tool_error(output: impl Into<String>) -> ToolResult {
    ToolResult {
        output: output.into(),
        is_error: true,
        metadata: Default::default(),
        mode_change: None,
    }
}

fn str_param(params: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn bool_param(params: &HashMap<String, serde_json::Value>, key: &str) -> bool {
    params.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn get_project_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ─── CronCreateTool ──────────────────────────────────────────────────────────

pub struct CronCreateTool;

impl Tool for CronCreateTool {
    fn name(&self) -> &str { "cron_create" }

    fn description(&self) -> &str {
        "Schedule a prompt to run at a future time — either recurring on a cron schedule, or once at a specific time.\n\n\
        Uses standard 5-field cron in the user's local timezone: minute hour day-of-month month day-of-week.\n\
        \"0 9 * * *\" means 9am local — no timezone conversion needed.\n\n\
        ## One-shot tasks (recurring: false)\n\
        Fire once then auto-delete. Pin minute/hour/day-of-month/month to specific values.\n\n\
        ## Recurring jobs (recurring: true, the default)\n\
        Fire on every cron match until deleted or auto-expired after 7 days.\n\n\
        ## Avoid :00 and :30 minute marks\n\
        When the user's request is approximate, pick a minute that is NOT 0 or 30.\n\n\
        ## Durability\n\
        By default (durable: false) the job lives only in this session. Pass durable: true to persist to .claude/scheduled_tasks.json.\n\n\
        Recurring tasks auto-expire after 7 days. Returns a job ID you can pass to cron_delete."
    }

    fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::json;
        let schema = json!({
            "type": "object",
            "required": ["cron", "prompt"],
            "properties": {
                "cron": {
                    "type": "string",
                    "description": "Standard 5-field cron expression in local time: \"M H DoM Mon DoW\" (e.g. \"*/5 * * * *\" = every 5 minutes)."
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt to enqueue at each fire time."
                },
                "recurring": {
                    "type": "boolean",
                    "description": "true (default) = fire on every cron match until deleted or auto-expired after 7 days. false = fire once then auto-delete."
                },
                "durable": {
                    "type": "boolean",
                    "description": "true = persist to .claude/scheduled_tasks.json and survive restarts. false (default) = in-memory only."
                }
            }
        });
        schema.as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, serde_json::Value>) -> ToolPermissionResult {
        ToolPermissionResult::allow()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, serde_json::Value>) -> ToolResult {
        let cron = str_param(&params, "cron");
        let prompt = str_param(&params, "prompt");
        let Some(cron) = cron else {
            return tool_error("Error: cron parameter is required");
        };
        let Some(prompt) = prompt else {
            return tool_error("Error: prompt parameter is required");
        };

        let recurring = params.get("recurring").and_then(|v| v.as_bool()).unwrap_or(true);
        let durable = bool_param(&params, "durable");

        // Validate cron
        if CronExpr::parse(&cron).is_err() {
            return tool_error(format!(
                "Error: invalid cron expression '{cron}'. Expected 5 fields: M H DoM Mon DoW."
            ));
        }
        if next_cron_run_ms(&cron, now_ms()).is_none() {
            return tool_error(format!(
                "Error: cron expression '{cron}' does not match any calendar date in the next year."
            ));
        }

        // Check task limit
        let dir = get_project_dir();
        let all_tasks = list_all_cron_tasks(&dir);
        if all_tasks.len() >= MAX_JOBS {
            return tool_error(format!("Error: too many scheduled jobs (max {MAX_JOBS}). Cancel one first."));
        }

        let id = match add_cron_task(&cron, &prompt, recurring, durable, "", &dir) {
            Ok(id) => id,
            Err(e) => return tool_error(format!("Error: failed to create cron task: {e}")),
        };

        let human = cron_to_human(&cron);
        let (where_str, _durable_msg) = if durable {
            ("Persisted to .claude/scheduled_tasks.json", "")
        } else {
            ("Session-only (not written to disk, dies when Claude exits)", "")
        };

        let content = if recurring {
            format!("Scheduled recurring job {id} ({human}). {where_str}. Auto-expires after 7 days. Use cron_delete to cancel sooner.")
        } else {
            format!("Scheduled one-shot task {id} ({human}). {where_str}. It will fire once then auto-delete.")
        };
        tool_success(content)
    }
}

// ─── CronDeleteTool ──────────────────────────────────────────────────────────

pub struct CronDeleteTool;

impl Tool for CronDeleteTool {
    fn name(&self) -> &str { "cron_delete" }

    fn description(&self) -> &str {
        "Cancel a scheduled cron job by ID. Removes it from .claude/scheduled_tasks.json (durable jobs) or the in-memory session store (session-only jobs)."
    }

    fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::json;
        let schema = json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Job ID returned by cron_create."
                }
            }
        });
        schema.as_object().unwrap().clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, serde_json::Value>) -> ToolPermissionResult {
        ToolPermissionResult::allow()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, serde_json::Value>) -> ToolResult {
        let Some(id) = str_param(&params, "id") else {
            return tool_error("Error: id parameter is required");
        };

        let dir = get_project_dir();
        let all_tasks = list_all_cron_tasks(&dir);
        let found = all_tasks.iter().any(|t| t.id == id);
        if !found {
            return tool_error(format!("Error: no scheduled job with id '{id}'"));
        }

        match remove_cron_tasks(&[id.clone()], &dir) {
            Ok(()) => tool_success(format!("Cancelled job {id}.")),
            Err(e) => tool_error(format!("Error: failed to delete cron task: {e}")),
        }
    }
}

// ─── CronListTool ────────────────────────────────────────────────────────────

pub struct CronListTool;

impl Tool for CronListTool {
    fn name(&self) -> &str { "cron_list" }

    fn description(&self) -> &str {
        "List scheduled cron jobs"
    }

    fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn check_permissions(&self, _params: &HashMap<String, serde_json::Value>) -> ToolPermissionResult {
        ToolPermissionResult::allow()
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn execute(&self, _params: HashMap<String, serde_json::Value>) -> ToolResult {
        let dir = get_project_dir();
        let all_tasks = list_all_cron_tasks(&dir);
        if all_tasks.is_empty() {
            return tool_success("No scheduled tasks.");
        }

        let lines: Vec<String> = all_tasks.iter().map(|t| {
            let human = cron_to_human(&t.expr.to_cron_string());
            let recurring_str = if t.recurring { "recurring" } else { "one-shot" };
            let durable_str = if !t.durable { " [session-only]" } else { "" };
            let prompt = if t.prompt.len() > 80 {
                format!("{}...", &t.prompt[..77])
            } else {
                t.prompt.clone()
            };
            format!("{} — {} ({}){}: {}", t.id, human, recurring_str, durable_str, prompt)
        }).collect();

        tool_success(format!("## Scheduled Jobs\n\n{}", lines.join("\n")))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cron_field_any() {
        let f = CronField::parse("*").unwrap();
        assert!(f.matches(5));
        assert!(f.matches(59));
    }

    #[test]
    fn test_parse_cron_field_exact() {
        let f = CronField::parse("5").unwrap();
        assert!(f.matches(5));
        assert!(!f.matches(6));
    }

    #[test]
    fn test_parse_cron_field_comma() {
        let f = CronField::parse("1,15").unwrap();
        assert!(f.matches(1));
        assert!(f.matches(15));
        assert!(!f.matches(10));
    }

    #[test]
    fn test_parse_cron_field_range() {
        let f = CronField::parse("1-5").unwrap();
        assert!(f.matches(3));
        assert!(!f.matches(6));
    }

    #[test]
    fn test_parse_cron_field_step() {
        let f = CronField::parse("*/5").unwrap();
        assert!(f.matches(0));
        assert!(f.matches(5));
        assert!(f.matches(30));
        assert!(!f.matches(3));
    }

    #[test]
    fn test_parse_cron_expr() {
        let expr = CronExpr::parse("*/5 * * * *").unwrap();
        assert!(expr.minute.matches(5));
        assert!(expr.hour.matches(14));
    }

    #[test]
    fn test_parse_cron_expr_invalid() {
        assert!(CronExpr::parse("*/5 * *").is_err());
        assert!(CronExpr::parse("").is_err());
    }

    #[test]
    fn test_scheduler_add_and_list() {
        let scheduler = CronScheduler::new(String::new());
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();
        // Manually add to session store
        let task = CronTask {
            id: "job1".to_string(),
            expr,
            prompt: "Morning standup".to_string(),
            created_at_ms: now_ms(),
            last_fired_at_ms: None,
            recurring: true,
            permanent: false,
            durable: false,
            agent_id: String::new(),
        };
        add_session_cron_task(task);
        let tasks = get_session_cron_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "job1");
        // Clean up
        remove_session_cron_tasks(&["job1".to_string()]);
    }

    #[test]
    fn test_cron_to_human() {
        assert_eq!(cron_to_human("*/5 * * * *"), "Every 5 minutes");
        assert_eq!(cron_to_human("* * * * *"), "Every minute");
        assert_eq!(cron_to_human("0 * * * *"), "Every hour");
        assert_eq!(cron_to_human("30 * * * *"), "Every hour at :30");
        assert_eq!(cron_to_human("0 9 * * *"), "Every day at 9:00 AM");
        assert_eq!(cron_to_human("0 14 * * 1"), "Every Monday at 2:00 PM");
        assert_eq!(cron_to_human("30 14 * * 1-5"), "Weekdays at 2:30 PM");
    }

    #[test]
    fn test_jitter_frac() {
        // Valid 8-hex-char ID should produce value in [0, 1)
        let frac = jitter_frac("abcd1234");
        assert!(frac >= 0.0 && frac < 1.0);

        // Short ID should produce 0
        assert_eq!(jitter_frac("abc"), 0.0);
    }

    #[test]
    fn test_generate_task_id() {
        let id = generate_task_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cron_expr_to_string() {
        let expr = CronExpr::parse("*/5 * 1-15 3,6 1-5").unwrap();
        let s = expr.to_cron_string();
        assert_eq!(s, "*/5 * 1-15 3,6 1-5");
    }

    #[test]
    fn test_find_missed_tasks() {
        let expr = CronExpr::parse("0 9 * * *").unwrap();
        let now = now_ms();
        let old = now - (8 * 24 * 60 * 60 * 1000); // 8 days ago
        let task = CronTask {
            id: "old".to_string(),
            expr,
            prompt: "test".to_string(),
            created_at_ms: old,
            last_fired_at_ms: None,
            recurring: true,
            permanent: false,
            durable: true,
            agent_id: String::new(),
        };
        let missed = find_missed_tasks(&[task], now);
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].id, "old");
    }

    #[test]
    fn test_is_recurring_task_aged() {
        let expr = CronExpr::parse("0 9 * * *").unwrap();
        let now = now_ms();
        let old = now - (8 * 24 * 60 * 60 * 1000); // 8 days ago (> 7 day max age)
        let task = CronTask {
            id: "old".to_string(),
            expr,
            prompt: "test".to_string(),
            created_at_ms: old,
            last_fired_at_ms: None,
            recurring: true,
            permanent: false,
            durable: true,
            agent_id: String::new(),
        };
        let max_age = 7 * 24 * 60 * 60 * 1000; // 7 days
        assert!(is_recurring_task_aged(&task, now, max_age));
    }

    #[test]
    fn test_permanent_not_aged() {
        let expr = CronExpr::parse("0 9 * * *").unwrap();
        let now = now_ms();
        let old = now - (30 * 24 * 60 * 60 * 1000); // 30 days ago
        let task = CronTask {
            id: "perm".to_string(),
            expr,
            prompt: "test".to_string(),
            created_at_ms: old,
            last_fired_at_ms: None,
            recurring: true,
            permanent: true,
            durable: true,
            agent_id: String::new(),
        };
        let max_age = 7 * 24 * 60 * 60 * 1000;
        assert!(!is_recurring_task_aged(&task, now, max_age), "permanent tasks should never expire");
    }
}

