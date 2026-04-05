//! Cron scheduling system for the emberforge CLI tool.
//!
//! Provides cron expression parsing, task scheduling, and persistence.
//! Supports standard 5-field cron format: `minute hour day-of-month month day-of-week`.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A parsed cron schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    pub minutes: Vec<u8>,
    pub hours: Vec<u8>,
    pub days_of_month: Vec<u8>,
    pub months: Vec<u8>,
    pub days_of_week: Vec<u8>,
}

/// Error from parsing a cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronParseError {
    pub message: String,
    pub field: Option<String>,
}

impl std::fmt::Display for CronParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ref field) = self.field {
            write!(f, "cron parse error in field '{}': {}", field, self.message)
        } else {
            write!(f, "cron parse error: {}", self.message)
        }
    }
}

impl std::error::Error for CronParseError {}

/// A scheduled task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    /// Unique task identifier.
    pub id: String,
    /// Human-readable name/description.
    pub name: String,
    /// The cron expression string (e.g. `"*/5 * * * *"`).
    pub schedule: String,
    /// The prompt/command to execute when triggered.
    pub prompt: String,
    /// Whether this task repeats (`true`) or fires once then auto-deletes.
    #[serde(default = "default_true")]
    pub recurring: bool,
    /// Whether this task survives session restarts.
    #[serde(default)]
    pub durable: bool,
    /// ISO 8601 timestamp when the task was created.
    pub created_at: String,
    /// ISO 8601 timestamp of last execution (if any).
    #[serde(default)]
    pub last_run_at: Option<String>,
    /// Number of times this task has fired.
    #[serde(default)]
    pub run_count: u32,
    /// Maximum age in days before auto-expiry (default 7).
    #[serde(default = "default_max_age_days")]
    pub max_age_days: u32,
}

fn default_true() -> bool {
    true
}

fn default_max_age_days() -> u32 {
    7
}

/// Result of a scheduler tick.
#[derive(Debug, Clone)]
pub struct SchedulerTickResult {
    /// Tasks that should fire this tick.
    pub triggered: Vec<ScheduledTask>,
    /// Task IDs that were auto-expired.
    pub expired: Vec<String>,
}

// ---------------------------------------------------------------------------
// Cron expression parsing
// ---------------------------------------------------------------------------

/// Field names used for error messages.
const FIELD_NAMES: [&str; 5] = ["minute", "hour", "day-of-month", "month", "day-of-week"];

/// Inclusive ranges for each field: (min, max).
const FIELD_RANGES: [(u8, u8); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];

/// Parse a 5-field cron expression into a `CronSchedule`.
///
/// Supported syntax per field: `*`, `N`, `N-M`, `N-M/S`, `*/S`, `N,M,L`.
pub fn parse_cron(expr: &str) -> Result<CronSchedule, CronParseError> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CronParseError {
            message: format!("expected 5 fields, got {}", fields.len()),
            field: None,
        });
    }

    let mut parsed: Vec<Vec<u8>> = Vec::with_capacity(5);
    for (i, &token) in fields.iter().enumerate() {
        let (lo, hi) = FIELD_RANGES[i];
        let name = FIELD_NAMES[i];
        let mut values = parse_field(token, lo, hi, name)?;

        // Normalise day-of-week: 7 → 0 (both mean Sunday).
        if i == 4 {
            for v in &mut values {
                if *v == 7 {
                    *v = 0;
                }
            }
            values.sort_unstable();
            values.dedup();
        }

        parsed.push(values);
    }

    Ok(CronSchedule {
        minutes: parsed.remove(0),
        hours: parsed.remove(0),
        days_of_month: parsed.remove(0),
        months: parsed.remove(0),
        days_of_week: parsed.remove(0),
    })
}

/// Parse a single cron field token. Returns a sorted, deduplicated list of
/// matching values within `[lo, hi]`.
fn parse_field(token: &str, lo: u8, hi: u8, name: &str) -> Result<Vec<u8>, CronParseError> {
    // Handle comma-separated lists first.
    if token.contains(',') {
        let mut all = Vec::new();
        for part in token.split(',') {
            all.extend(parse_atom(part.trim(), lo, hi, name)?);
        }
        all.sort_unstable();
        all.dedup();
        return Ok(all);
    }

    parse_atom(token, lo, hi, name)
}

/// Parse a single atom: `*`, `*/S`, `N`, `N-M`, or `N-M/S`.
fn parse_atom(token: &str, lo: u8, hi: u8, name: &str) -> Result<Vec<u8>, CronParseError> {
    let make_err = |msg: String| CronParseError {
        message: msg,
        field: Some(name.to_string()),
    };

    // Wildcard with optional step: `*` or `*/S`.
    if token.starts_with('*') {
        if token == "*" {
            return Ok((lo..=hi).collect());
        }
        if let Some(step_str) = token.strip_prefix("*/") {
            let step = parse_u8(step_str)
                .map_err(|_| make_err(format!("invalid step in '{token}'")))?;
            if step == 0 {
                return Err(make_err(format!("step must be > 0 in '{token}'")));
            }
            return Ok((lo..=hi).step_by(step as usize).collect());
        }
        return Err(make_err(format!("invalid wildcard syntax '{token}'")));
    }

    // Range with optional step: `N-M` or `N-M/S`.
    if token.contains('-') {
        let (range_part, step) = if token.contains('/') {
            let parts: Vec<&str> = token.splitn(2, '/').collect();
            let s = parse_u8(parts[1])
                .map_err(|_| make_err(format!("invalid step in '{token}'")))?;
            if s == 0 {
                return Err(make_err(format!("step must be > 0 in '{token}'")));
            }
            (parts[0], s)
        } else {
            (token, 1)
        };

        let bounds: Vec<&str> = range_part.splitn(2, '-').collect();
        if bounds.len() != 2 {
            return Err(make_err(format!("invalid range '{token}'")));
        }
        let start = parse_u8(bounds[0])
            .map_err(|_| make_err(format!("invalid range start in '{token}'")))?;
        let end = parse_u8(bounds[1])
            .map_err(|_| make_err(format!("invalid range end in '{token}'")))?;

        validate_bound(start, lo, hi, name, token)?;
        validate_bound(end, lo, hi, name, token)?;

        if start > end {
            return Err(make_err(format!(
                "range start ({start}) > end ({end}) in '{token}'"
            )));
        }

        return Ok((start..=end).step_by(step as usize).collect());
    }

    // Exact value.
    let val =
        parse_u8(token).map_err(|_| make_err(format!("invalid value '{token}'")))?;
    validate_bound(val, lo, hi, name, token)?;
    Ok(vec![val])
}

fn parse_u8(s: &str) -> Result<u8, ()> {
    s.parse::<u8>().map_err(|_| ())
}

fn validate_bound(val: u8, lo: u8, hi: u8, name: &str, token: &str) -> Result<(), CronParseError> {
    if val < lo || val > hi {
        return Err(CronParseError {
            message: format!("value {val} out of range {lo}-{hi} in '{token}'"),
            field: Some(name.to_string()),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Schedule matching
// ---------------------------------------------------------------------------

/// Check if a `CronSchedule` matches the given time components.
///
/// All five fields must match simultaneously (AND logic).
pub fn schedule_matches(
    schedule: &CronSchedule,
    minute: u8,
    hour: u8,
    day_of_month: u8,
    month: u8,
    day_of_week: u8,
) -> bool {
    schedule.minutes.contains(&minute)
        && schedule.hours.contains(&hour)
        && schedule.days_of_month.contains(&day_of_month)
        && schedule.months.contains(&month)
        && schedule.days_of_week.contains(&day_of_week)
}

// ---------------------------------------------------------------------------
// Task management
// ---------------------------------------------------------------------------

/// Create a new scheduled task. Validates the cron expression before creating.
pub fn create_task(
    name: &str,
    schedule: &str,
    prompt: &str,
    recurring: bool,
    durable: bool,
) -> Result<ScheduledTask, CronParseError> {
    // Validate the schedule.
    let _ = parse_cron(schedule)?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("cron-{nanos}");

    let created_at = iso8601_now();

    Ok(ScheduledTask {
        id,
        name: name.to_string(),
        schedule: schedule.to_string(),
        prompt: prompt.to_string(),
        recurring,
        durable,
        created_at,
        last_run_at: None,
        run_count: 0,
        max_age_days: default_max_age_days(),
    })
}

/// The persistence file path within a project directory.
fn tasks_file(project_dir: &Path) -> std::path::PathBuf {
    project_dir.join(".ember").join("scheduled_tasks.json")
}

/// Load durable tasks from `.ember/scheduled_tasks.json`.
pub fn load_durable_tasks(project_dir: &Path) -> io::Result<Vec<ScheduledTask>> {
    let path = tasks_file(project_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let tasks: Vec<ScheduledTask> = serde_json::from_str(&data).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
    })?;
    Ok(tasks)
}

/// Save durable tasks to `.ember/scheduled_tasks.json`.
pub fn save_durable_tasks(project_dir: &Path, tasks: &[ScheduledTask]) -> io::Result<()> {
    let path = tasks_file(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tasks).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
    })?;
    std::fs::write(&path, json)
}

/// Delete a task by ID. Returns `true` if the task was found and removed.
pub fn delete_task(tasks: &mut Vec<ScheduledTask>, task_id: &str) -> bool {
    let before = tasks.len();
    tasks.retain(|t| t.id != task_id);
    tasks.len() < before
}

// ---------------------------------------------------------------------------
// Scheduler tick
// ---------------------------------------------------------------------------

/// Run a scheduler tick: determine which tasks should fire at the current
/// local time, update their metadata, remove expired/one-shot tasks.
pub fn tick(tasks: &mut Vec<ScheduledTask>) -> SchedulerTickResult {
    let (minute, hour, day, month, weekday) = current_local_time();
    tick_with_time(tasks, minute, hour, day, month, weekday)
}

/// Testable inner tick that accepts explicit time components.
fn tick_with_time(
    tasks: &mut Vec<ScheduledTask>,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    weekday: u8,
) -> SchedulerTickResult {
    let now_iso = iso8601_now();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut triggered = Vec::new();
    let mut expired = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();

    for task in tasks.iter_mut() {
        // Check expiry.
        if let Some(age_secs) = task_age_secs(task, now_secs) {
            let max_secs = u64::from(task.max_age_days) * 86_400;
            if age_secs > max_secs {
                expired.push(task.id.clone());
                to_remove.push(task.id.clone());
                continue;
            }
        }

        // Parse schedule (skip if invalid — should not happen for validated tasks).
        let Ok(sched) = parse_cron(&task.schedule) else {
            continue;
        };

        if schedule_matches(&sched, minute, hour, day, month, weekday) {
            task.last_run_at = Some(now_iso.clone());
            task.run_count += 1;
            triggered.push(task.clone());

            if !task.recurring {
                to_remove.push(task.id.clone());
            }
        }
    }

    // Remove expired and one-shot tasks.
    tasks.retain(|t| !to_remove.contains(&t.id));

    SchedulerTickResult { triggered, expired }
}

/// Compute the age of a task in seconds, based on its `created_at` field.
fn task_age_secs(task: &ScheduledTask, now_secs: u64) -> Option<u64> {
    let created = parse_iso8601_to_epoch(&task.created_at)?;
    Some(now_secs.saturating_sub(created))
}

/// Very simple ISO 8601 epoch parser — handles `YYYY-MM-DDTHH:MM:SSZ` format.
fn parse_iso8601_to_epoch(s: &str) -> Option<u64> {
    // Expect format: 2026-04-05T12:30:00Z
    let s = s.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    let date_parts: Vec<&str> = date_part.split('-').collect();
    let time_parts: Vec<&str> = time_part.split(':').collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }

    let year: i64 = date_parts[0].parse().ok()?;
    let month: i64 = date_parts[1].parse().ok()?;
    let day: i64 = date_parts[2].parse().ok()?;
    let hour: i64 = time_parts[0].parse().ok()?;
    let min: i64 = time_parts[1].parse().ok()?;
    let sec: i64 = time_parts[2].parse().ok()?;

    // Days from Unix epoch using a simplified algorithm.
    let epoch_days = days_from_civil(year, month, day);
    let total_secs = epoch_days * 86_400 + hour * 3600 + min * 60 + sec;
    if total_secs < 0 {
        return None;
    }
    Some(total_secs as u64)
}

/// Days from 1970-01-01 for a given civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = if m > 2 {
        (153 * (m - 3) + 2) / 5 + day as u64 - 1
    } else {
        (153 * (m + 9) + 2) / 5 + day as u64 - 1
    };
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// Local time (UTC-based, safe — no libc)
// ---------------------------------------------------------------------------

/// Get the current UTC time components.
///
/// Returns `(minute, hour, day_of_month, month, day_of_week)`.
/// Uses UTC because accessing the system timezone without `libc` unsafe calls
/// or the `chrono` crate is not portable. For scheduling purposes UTC is
/// deterministic and consistent.
pub fn current_local_time() -> (u8, u8, u8, u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    utc_components(secs)
}

/// Break a Unix timestamp (seconds since epoch) into UTC components.
fn utc_components(secs: u64) -> (u8, u8, u8, u8, u8) {
    let days = (secs / 86_400) as i64;
    let day_secs = secs % 86_400;

    let hour = (day_secs / 3600) as u8;
    let minute = ((day_secs % 3600) / 60) as u8;

    // Day of week: 1970-01-01 was a Thursday (4).
    let weekday = ((days + 4).rem_euclid(7)) as u8;

    // Civil date from day count (Howard Hinnant).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let _y = if m <= 2 { y + 1 } else { y };

    (minute, hour, d, m, weekday)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Human-readable description of a cron schedule.
pub fn describe_schedule(schedule: &CronSchedule) -> String {
    let all_min = schedule.minutes.len() == 60;
    let all_hr = schedule.hours.len() == 24;
    let all_dom = schedule.days_of_month.len() == 31;
    let all_mon = schedule.months.len() == 12;
    let all_dow = schedule.days_of_week.len() == 7;

    // "every minute"
    if all_min && all_hr && all_dom && all_mon && all_dow {
        return "every minute".to_string();
    }

    // "every N minutes" — check for regular step from 0.
    if all_hr && all_dom && all_mon && all_dow && schedule.minutes.len() > 1 {
        if let Some(step) = detect_step(&schedule.minutes) {
            return format!("every {step} minutes");
        }
    }

    // "every N hours"
    if schedule.minutes.len() == 1
        && all_dom
        && all_mon
        && all_dow
        && schedule.hours.len() > 1
    {
        if let Some(step) = detect_step(&schedule.hours) {
            return format!(
                "every {step} hours at :{:02}",
                schedule.minutes[0]
            );
        }
    }

    // "daily at HH:MM"
    if schedule.minutes.len() == 1
        && schedule.hours.len() == 1
        && all_dom
        && all_mon
        && all_dow
    {
        return format!(
            "daily at {:02}:{:02}",
            schedule.hours[0], schedule.minutes[0]
        );
    }

    // "every <weekday> at HH:MM"
    if schedule.minutes.len() == 1
        && schedule.hours.len() == 1
        && all_dom
        && all_mon
        && schedule.days_of_week.len() == 1
    {
        let day_name = weekday_name(schedule.days_of_week[0]);
        return format!(
            "every {day_name} at {:02}:{:02}",
            schedule.hours[0], schedule.minutes[0]
        );
    }

    // Fallback: reconstruct the expression.
    format!(
        "cron({} {} {} {} {})",
        field_to_string(&schedule.minutes, 0, 59),
        field_to_string(&schedule.hours, 0, 23),
        field_to_string(&schedule.days_of_month, 1, 31),
        field_to_string(&schedule.months, 1, 12),
        field_to_string(&schedule.days_of_week, 0, 6),
    )
}

fn detect_step(values: &[u8]) -> Option<u8> {
    if values.len() < 2 || values[0] != 0 {
        // Only detect step patterns starting from 0 for cleaner descriptions.
        // Actually, be lenient: just check uniform spacing.
        if values.len() < 2 {
            return None;
        }
    }
    let step = values[1] - values[0];
    if step == 0 {
        return None;
    }
    for pair in values.windows(2) {
        if pair[1] - pair[0] != step {
            return None;
        }
    }
    Some(step)
}

fn weekday_name(dow: u8) -> &'static str {
    match dow {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Unknown",
    }
}

fn field_to_string(values: &[u8], lo: u8, hi: u8) -> String {
    let full_count = (hi - lo + 1) as usize;
    if values.len() == full_count {
        return "*".to_string();
    }
    if values.len() == 1 {
        return values[0].to_string();
    }
    values
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Format a task for display in a list.
pub fn format_task_summary(task: &ScheduledTask) -> String {
    let sched_desc = match parse_cron(&task.schedule) {
        Ok(s) => describe_schedule(&s),
        Err(_) => task.schedule.clone(),
    };

    let status = if task.recurring { "recurring" } else { "one-shot" };
    let durability = if task.durable { "durable" } else { "ephemeral" };
    let runs = if task.run_count == 0 {
        "never run".to_string()
    } else {
        format!("run {} time{}", task.run_count, if task.run_count == 1 { "" } else { "s" })
    };

    format!(
        "[{}] {} — {} ({}, {}, {})",
        task.id, task.name, sched_desc, status, durability, runs
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Produce an ISO 8601 UTC timestamp for "now".
fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (minute, hour, day, month, weekday) = utc_components(secs);
    // We need year as well; re-derive it.
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = if m <= 2 { y + 1 } else { y };
    let _ = weekday; // unused here

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        hour,
        minute,
        secs % 60
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // -- Parsing tests --

    #[test]
    fn parse_wildcard_all() {
        let s = parse_cron("* * * * *").unwrap();
        assert_eq!(s.minutes, (0..=59).collect::<Vec<u8>>());
        assert_eq!(s.hours, (0..=23).collect::<Vec<u8>>());
        assert_eq!(s.days_of_month, (1..=31).collect::<Vec<u8>>());
        assert_eq!(s.months, (1..=12).collect::<Vec<u8>>());
        // 0-7 with 7→0 normalisation and dedup gives 0-6.
        assert_eq!(s.days_of_week, (0..=6).collect::<Vec<u8>>());
    }

    #[test]
    fn parse_exact_values() {
        let s = parse_cron("30 9 * * 1").unwrap();
        assert_eq!(s.minutes, vec![30]);
        assert_eq!(s.hours, vec![9]);
        assert_eq!(s.days_of_month, (1..=31).collect::<Vec<u8>>());
        assert_eq!(s.months, (1..=12).collect::<Vec<u8>>());
        assert_eq!(s.days_of_week, vec![1]);
    }

    #[test]
    fn parse_ranges() {
        let s = parse_cron("0-30 9-17 * * *").unwrap();
        assert_eq!(s.minutes, (0..=30).collect::<Vec<u8>>());
        assert_eq!(s.hours, (9..=17).collect::<Vec<u8>>());
    }

    #[test]
    fn parse_wildcard_step() {
        let s = parse_cron("*/5 * * * *").unwrap();
        assert_eq!(s.minutes, vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]);
    }

    #[test]
    fn parse_range_step() {
        let s = parse_cron("0-30/10 * * * *").unwrap();
        assert_eq!(s.minutes, vec![0, 10, 20, 30]);
    }

    #[test]
    fn parse_list() {
        let s = parse_cron("0,15,30,45 * * * *").unwrap();
        assert_eq!(s.minutes, vec![0, 15, 30, 45]);
    }

    #[test]
    fn parse_sunday_as_0_and_7() {
        let s0 = parse_cron("0 0 * * 0").unwrap();
        let s7 = parse_cron("0 0 * * 7").unwrap();
        assert_eq!(s0.days_of_week, vec![0]);
        assert_eq!(s7.days_of_week, vec![0]); // 7 normalised to 0
    }

    #[test]
    fn parse_reject_too_few_fields() {
        let err = parse_cron("* * *").unwrap_err();
        assert!(err.message.contains("expected 5 fields"));
        assert_eq!(err.field, None);
    }

    #[test]
    fn parse_reject_too_many_fields() {
        let err = parse_cron("* * * * * *").unwrap_err();
        assert!(err.message.contains("expected 5 fields"));
    }

    #[test]
    fn parse_reject_out_of_range() {
        let err = parse_cron("60 * * * *").unwrap_err();
        assert!(err.message.contains("out of range"));
        assert_eq!(err.field, Some("minute".to_string()));
    }

    #[test]
    fn parse_reject_bad_syntax() {
        let err = parse_cron("abc * * * *").unwrap_err();
        assert!(err.field.is_some());
    }

    #[test]
    fn parse_reject_zero_step() {
        let err = parse_cron("*/0 * * * *").unwrap_err();
        assert!(err.message.contains("step must be > 0"));
    }

    #[test]
    fn parse_reject_inverted_range() {
        let err = parse_cron("30-10 * * * *").unwrap_err();
        assert!(err.message.contains("start"));
    }

    // -- Matching tests --

    #[test]
    fn match_exact() {
        let s = parse_cron("30 9 15 6 1").unwrap();
        assert!(schedule_matches(&s, 30, 9, 15, 6, 1));
    }

    #[test]
    fn match_no_match() {
        let s = parse_cron("30 9 15 6 1").unwrap();
        assert!(!schedule_matches(&s, 31, 9, 15, 6, 1));
        assert!(!schedule_matches(&s, 30, 10, 15, 6, 1));
    }

    #[test]
    fn match_wildcard() {
        let s = parse_cron("*/15 * * * *").unwrap();
        assert!(schedule_matches(&s, 0, 5, 1, 1, 0));
        assert!(schedule_matches(&s, 15, 23, 31, 12, 6));
        assert!(schedule_matches(&s, 30, 0, 15, 6, 3));
        assert!(!schedule_matches(&s, 7, 0, 1, 1, 0));
    }

    // -- Task creation tests --

    #[test]
    fn create_task_valid() {
        let t = create_task("test", "*/5 * * * *", "echo hi", true, false).unwrap();
        assert!(t.id.starts_with("cron-"));
        assert_eq!(t.name, "test");
        assert_eq!(t.run_count, 0);
        assert!(t.recurring);
        assert!(!t.durable);
    }

    #[test]
    fn create_task_invalid_schedule() {
        let err = create_task("test", "bad", "echo hi", true, false).unwrap_err();
        assert!(err.message.contains("expected 5 fields"));
    }

    // -- Tick tests --

    #[test]
    fn tick_triggers_matching_task() {
        let mut tasks = vec![ScheduledTask {
            id: "cron-1".to_string(),
            name: "every min".to_string(),
            schedule: "* * * * *".to_string(),
            prompt: "echo".to_string(),
            recurring: true,
            durable: false,
            created_at: iso8601_now(),
            last_run_at: None,
            run_count: 0,
            max_age_days: 7,
        }];

        let result = tick_with_time(&mut tasks, 30, 9, 15, 6, 1);
        assert_eq!(result.triggered.len(), 1);
        assert_eq!(result.triggered[0].id, "cron-1");
        // Task still in the list (recurring).
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].run_count, 1);
        assert!(tasks[0].last_run_at.is_some());
    }

    #[test]
    fn tick_skips_non_matching() {
        let mut tasks = vec![ScheduledTask {
            id: "cron-2".to_string(),
            name: "noon only".to_string(),
            schedule: "0 12 * * *".to_string(),
            prompt: "echo".to_string(),
            recurring: true,
            durable: false,
            created_at: iso8601_now(),
            last_run_at: None,
            run_count: 0,
            max_age_days: 7,
        }];

        let result = tick_with_time(&mut tasks, 30, 9, 15, 6, 1);
        assert_eq!(result.triggered.len(), 0);
        assert_eq!(tasks[0].run_count, 0);
    }

    #[test]
    fn tick_removes_one_shot() {
        let mut tasks = vec![ScheduledTask {
            id: "cron-3".to_string(),
            name: "once".to_string(),
            schedule: "* * * * *".to_string(),
            prompt: "echo".to_string(),
            recurring: false,
            durable: false,
            created_at: iso8601_now(),
            last_run_at: None,
            run_count: 0,
            max_age_days: 7,
        }];

        let result = tick_with_time(&mut tasks, 0, 0, 1, 1, 4);
        assert_eq!(result.triggered.len(), 1);
        // One-shot removed after firing.
        assert!(tasks.is_empty());
    }

    #[test]
    fn tick_removes_expired() {
        // Create a task with created_at far in the past.
        let mut tasks = vec![ScheduledTask {
            id: "cron-4".to_string(),
            name: "old".to_string(),
            schedule: "* * * * *".to_string(),
            prompt: "echo".to_string(),
            recurring: true,
            durable: false,
            created_at: "2020-01-01T00:00:00Z".to_string(),
            last_run_at: None,
            run_count: 0,
            max_age_days: 1, // 1 day max age — long expired.
        }];

        let result = tick_with_time(&mut tasks, 0, 0, 1, 1, 4);
        assert_eq!(result.expired.len(), 1);
        assert_eq!(result.expired[0], "cron-4");
        assert!(tasks.is_empty());
    }

    // -- Persistence tests --

    #[test]
    fn durable_task_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ember_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let tasks = vec![
            create_task("t1", "*/5 * * * *", "prompt1", true, true).unwrap(),
            create_task("t2", "0 9 * * 1", "prompt2", false, true).unwrap(),
        ];

        save_durable_tasks(&dir, &tasks).unwrap();
        let loaded = load_durable_tasks(&dir).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "t1");
        assert_eq!(loaded[1].name, "t2");
        assert_eq!(loaded[0].schedule, "*/5 * * * *");
        assert!(loaded[0].recurring);
        assert!(!loaded[1].recurring);

        // Clean up.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let dir = PathBuf::from("/tmp/ember_nonexistent_test_dir_999");
        let tasks = load_durable_tasks(&dir).unwrap();
        assert!(tasks.is_empty());
    }

    // -- Delete tests --

    #[test]
    fn delete_task_found() {
        let mut tasks = vec![
            create_task("a", "* * * * *", "p", true, false).unwrap(),
            create_task("b", "* * * * *", "p", true, false).unwrap(),
        ];
        let id = tasks[0].id.clone();
        assert!(delete_task(&mut tasks, &id));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "b");
    }

    #[test]
    fn delete_task_not_found() {
        let mut tasks = vec![create_task("a", "* * * * *", "p", true, false).unwrap()];
        assert!(!delete_task(&mut tasks, "nonexistent"));
        assert_eq!(tasks.len(), 1);
    }

    // -- Describe tests --

    #[test]
    fn describe_every_minute() {
        let s = parse_cron("* * * * *").unwrap();
        assert_eq!(describe_schedule(&s), "every minute");
    }

    #[test]
    fn describe_every_5_minutes() {
        let s = parse_cron("*/5 * * * *").unwrap();
        assert_eq!(describe_schedule(&s), "every 5 minutes");
    }

    #[test]
    fn describe_daily() {
        let s = parse_cron("0 9 * * *").unwrap();
        assert_eq!(describe_schedule(&s), "daily at 09:00");
    }

    #[test]
    fn describe_weekly() {
        let s = parse_cron("30 8 * * 1").unwrap();
        assert_eq!(describe_schedule(&s), "every Monday at 08:30");
    }

    #[test]
    fn describe_fallback() {
        let s = parse_cron("0,30 9-17 * * 1-5").unwrap();
        let desc = describe_schedule(&s);
        assert!(desc.starts_with("cron("));
    }

    // -- Format summary test --

    #[test]
    fn format_task_summary_basic() {
        let t = create_task("backup", "0 2 * * *", "run backup", true, true).unwrap();
        let summary = format_task_summary(&t);
        assert!(summary.contains("backup"));
        assert!(summary.contains("daily at 02:00"));
        assert!(summary.contains("recurring"));
        assert!(summary.contains("durable"));
        assert!(summary.contains("never run"));
    }

    // -- UTC components test --

    #[test]
    fn utc_components_epoch() {
        // 1970-01-01 00:00:00 UTC is Thursday (4).
        let (min, hr, day, month, weekday) = utc_components(0);
        assert_eq!((min, hr, day, month, weekday), (0, 0, 1, 1, 4));
    }

    #[test]
    fn utc_components_known_date() {
        // 2026-04-05 is a Sunday.
        // 2026-04-05T15:30:00Z = days since epoch.
        let secs = parse_iso8601_to_epoch("2026-04-05T15:30:00Z").unwrap();
        let (min, hr, day, month, weekday) = utc_components(secs);
        assert_eq!(min, 30);
        assert_eq!(hr, 15);
        assert_eq!(day, 5);
        assert_eq!(month, 4);
        assert_eq!(weekday, 0); // Sunday
    }

    // -- ISO 8601 parser test --

    #[test]
    fn parse_iso8601_basic() {
        let epoch = parse_iso8601_to_epoch("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(epoch, 0);
    }

    #[test]
    fn parse_iso8601_invalid() {
        assert!(parse_iso8601_to_epoch("not-a-date").is_none());
    }

    // -- CronParseError display --

    #[test]
    fn error_display_with_field() {
        let err = CronParseError {
            message: "bad value".to_string(),
            field: Some("minute".to_string()),
        };
        let s = format!("{err}");
        assert!(s.contains("minute"));
        assert!(s.contains("bad value"));
    }

    #[test]
    fn error_display_without_field() {
        let err = CronParseError {
            message: "wrong count".to_string(),
            field: None,
        };
        let s = format!("{err}");
        assert!(s.contains("wrong count"));
        assert!(!s.contains("field"));
    }
}
