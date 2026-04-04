//! Background task management — listing, stopping, attaching to agents.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::{chrono_now_iso8601, truncate_for_summary};

const TASK_LOG_TAIL_LINES: usize = 80;
const TASK_HEARTBEAT_DELAY_SECS: i64 = 15;
const TASK_HEARTBEAT_STALLED_SECS: i64 = 45;
const TASK_ACTIVITY_LIMIT: usize = 40;
const TASK_ACTIVITY_RENDER_LIMIT: usize = 6;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BackgroundTaskCounts {
    pub(crate) total_running: usize,
    pub(crate) session_running: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskActivityEntry {
    at: String,
    kind: String,
    status: String,
    message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskManifestRecord {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest: serde_json::Value,
}

impl TaskManifestRecord {
    pub(crate) fn id(&self) -> &str {
        self.manifest
            .get("agentId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    }

    pub(crate) fn status(&self) -> &str {
        self.manifest
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    }

    pub(crate) fn description(&self) -> &str {
        self.manifest
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    }

    fn task_kind(&self) -> &str {
        self.manifest
            .get("taskKind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("subagent")
    }

    fn model(&self) -> Option<&str> {
        self.manifest.get("model").and_then(serde_json::Value::as_str)
    }

    fn output_file(&self) -> Option<&str> {
        self.manifest
            .get("outputFile")
            .and_then(serde_json::Value::as_str)
    }

    fn parent_session_id(&self) -> Option<&str> {
        self.manifest
            .get("parentSessionId")
            .and_then(serde_json::Value::as_str)
    }

    fn status_detail(&self) -> Option<&str> {
        self.manifest
            .get("statusDetail")
            .and_then(serde_json::Value::as_str)
    }

    fn updated_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("updatedAt")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                self.manifest
                    .get("completedAt")
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| {
                self.manifest
                    .get("startedAt")
                    .and_then(serde_json::Value::as_str)
            })
            .or_else(|| {
                self.manifest
                    .get("createdAt")
                    .and_then(serde_json::Value::as_str)
            })
    }

    fn heartbeat_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("lastHeartbeatAt")
            .and_then(serde_json::Value::as_str)
    }

    fn started_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("startedAt")
            .and_then(serde_json::Value::as_str)
    }

    fn created_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("createdAt")
            .and_then(serde_json::Value::as_str)
    }

    fn completed_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("completedAt")
            .and_then(serde_json::Value::as_str)
    }

    fn stop_requested_at_raw(&self) -> Option<&str> {
        self.manifest
            .get("stopRequestedAt")
            .and_then(serde_json::Value::as_str)
    }

    fn stop_reason(&self) -> Option<&str> {
        self.manifest
            .get("stopReason")
            .and_then(serde_json::Value::as_str)
    }

    fn restarted_from(&self) -> Option<&str> {
        self.manifest
            .get("restartedFrom")
            .and_then(serde_json::Value::as_str)
    }

    fn worker_pid(&self) -> Option<u32> {
        self.manifest
            .get("workerPid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    }

    fn activity_entries(&self) -> Vec<TaskActivityEntry> {
        self.manifest
            .get("activity")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(TaskActivityEntry {
                            at: entry.get("at")?.as_str()?.to_string(),
                            kind: entry.get("kind")?.as_str()?.to_string(),
                            status: entry.get("status")?.as_str()?.to_string(),
                            message: entry.get("message")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_active(&self) -> bool {
        matches!(self.status(), "running" | "finishing" | "stopping")
    }

    fn is_terminal(&self) -> bool {
        matches!(self.status(), "completed" | "failed" | "cancelled" | "interrupted")
    }

    fn sort_timestamp(&self) -> Option<OffsetDateTime> {
        parse_task_timestamp(self.updated_at_raw())
            .or_else(|| parse_task_timestamp(self.heartbeat_at_raw()))
            .or_else(|| parse_task_timestamp(self.started_at_raw()))
            .or_else(|| parse_task_timestamp(self.created_at_raw()))
    }

    fn set_string(&mut self, key: &str, value: impl Into<String>) {
        self.manifest[key] = serde_json::Value::String(value.into());
    }

    fn mark_stop_requested(&mut self, now: &str, reason: &str) {
        if self.stop_requested_at_raw().is_none() {
            self.set_string("stopRequestedAt", now.to_string());
        }
        self.set_string("stopReason", reason.to_string());
        if self.is_active() {
            self.set_string("status", String::from("stopping"));
            self.set_string(
                "statusDetail",
                String::from("Stop requested; waiting for the current step to finish"),
            );
        }
        self.set_string("updatedAt", now.to_string());
        let status = self.status().to_string();
        let message = self
            .status_detail()
            .unwrap_or("Stop requested; waiting for the current step to finish")
            .to_string();
        let _ = append_task_activity_value(
            &mut self.manifest,
            now,
            "stop-requested",
            &status,
            &message,
        );
    }
}

fn append_task_activity_value(
    manifest: &mut serde_json::Value,
    at: &str,
    kind: &str,
    status: &str,
    message: &str,
) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Some(object) = manifest.as_object_mut() else {
        return false;
    };
    let activity_value = object
        .entry(String::from("activity"))
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(activity) = activity_value.as_array_mut() else {
        return false;
    };

    let normalized_message = truncate_for_summary(trimmed, 120);
    if activity.last().is_some_and(|last| {
        last.get("kind").and_then(serde_json::Value::as_str) == Some(kind)
            && last.get("status").and_then(serde_json::Value::as_str) == Some(status)
            && last.get("message").and_then(serde_json::Value::as_str)
                == Some(normalized_message.as_str())
    }) {
        return false;
    }

    activity.push(serde_json::json!({
        "at": at,
        "kind": kind,
        "status": status,
        "message": normalized_message,
    }));

    let overflow = activity.len().saturating_sub(TASK_ACTIVITY_LIMIT);
    if overflow > 0 {
        activity.drain(0..overflow);
    }
    true
}

fn trim_env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn task_store_dirs(cwd: &Path) -> Vec<PathBuf> {
    if let Some(path) = trim_env_path("EMBER_AGENT_STORE") {
        return vec![path];
    }
    if let Some(path) = trim_env_path("CLAW_AGENT_STORE") {
        return vec![path];
    }

    let mut dirs = Vec::new();
    for ancestor in cwd.ancestors() {
        let ember = ancestor.join(".ember-agents");
        if ember.is_dir() {
            dirs.push(ember);
        }
        let claw = ancestor.join(".claw-agents");
        if claw.is_dir() {
            dirs.push(claw);
        }
    }
    if dirs.is_empty() {
        dirs.push(cwd.join(".ember-agents"));
        dirs.push(cwd.join(".claw-agents"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

pub(crate) fn load_task_manifests(cwd: &Path) -> Result<Vec<TaskManifestRecord>, Box<dyn std::error::Error>> {
    let mut manifests = std::collections::BTreeMap::<String, TaskManifestRecord>::new();

    for dir in task_store_dirs(cwd) {
        if !dir.is_dir() {
            continue;
        }

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(contents) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
                continue;
            };
            let Some(agent_id) = manifest
                .get("agentId")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
            else {
                continue;
            };

            let record = TaskManifestRecord {
                manifest_path: entry.path(),
                manifest,
            };
            manifests
                .entry(agent_id)
                .and_modify(|existing| {
                    if task_manifest_preferred(&record, existing) {
                        *existing = record.clone();
                    }
                })
                .or_insert(record);
        }
    }

    let mut records = manifests.into_values().collect::<Vec<_>>();
    for record in &mut records {
        let _ = reconcile_task_manifest(record);
    }
    records.sort_by(|left, right| {
        right
            .sort_timestamp()
            .cmp(&left.sort_timestamp())
            .then_with(|| left.id().cmp(right.id()))
    });
    Ok(records)
}

fn task_manifest_preferred(candidate: &TaskManifestRecord, existing: &TaskManifestRecord) -> bool {
    let candidate_is_ember = candidate.manifest_path.to_string_lossy().contains(".ember-agents");
    let existing_is_ember = existing.manifest_path.to_string_lossy().contains(".ember-agents");
    if candidate_is_ember != existing_is_ember {
        return candidate_is_ember;
    }
    candidate.sort_timestamp() > existing.sort_timestamp()
}

fn parse_task_timestamp(value: Option<&str>) -> Option<OffsetDateTime> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }

    OffsetDateTime::parse(raw, &Rfc3339).ok().or_else(|| {
        raw.parse::<i64>()
            .ok()
            .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
    })
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // kill(pid, 0) sends no signal; it only checks process existence.
        // Returns 0 if alive, -1 with ESRCH if the process does not exist.
        let ret = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(ret, Ok(status) if status.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

pub(crate) fn write_task_manifest(record: &TaskManifestRecord) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(
        &record.manifest_path,
        serde_json::to_string_pretty(&record.manifest)?,
    )?;
    Ok(())
}

pub(crate) fn reconcile_task_manifest(
    record: &mut TaskManifestRecord,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !record.is_active() {
        return Ok(false);
    }

    let now = chrono_now_iso8601();
    let mut changed = false;

    if record.stop_requested_at_raw().is_some() && record.status() != "stopping" {
        record.set_string("status", String::from("stopping"));
        record.set_string(
            "statusDetail",
            String::from("Stop requested; waiting for the current step to finish"),
        );
        record.set_string("updatedAt", now.clone());
        let _ = append_task_activity_value(
            &mut record.manifest,
            &now,
            "status",
            "stopping",
            "Stop requested; waiting for the current step to finish",
        );
        changed = true;
    }

    if let Some(pid) = record.worker_pid() {
        if !process_is_alive(pid) {
            record.set_string(
                "status",
                if record.stop_requested_at_raw().is_some() {
                    String::from("cancelled")
                } else {
                    String::from("interrupted")
                },
            );
            if record.completed_at_raw().is_none() {
                record.set_string("completedAt", now.clone());
            }
            record.set_string("updatedAt", now.clone());
            record.set_string(
                "statusDetail",
                if record.stop_requested_at_raw().is_some() {
                    String::from("Worker process exited after a stop request")
                } else {
                    String::from("Worker process is no longer alive; task was interrupted")
                },
            );
            let terminal_status = record.status().to_string();
            let terminal_message = record
                .status_detail()
                .unwrap_or("Worker process is no longer alive")
                .to_string();
            let _ = append_task_activity_value(
                &mut record.manifest,
                &now,
                "terminal",
                &terminal_status,
                &terminal_message,
            );
            changed = true;
        }
    }

    if changed {
        write_task_manifest(record)?;
    }
    Ok(changed)
}

fn task_heartbeat_age(record: &TaskManifestRecord) -> Option<time::Duration> {
    let timestamp = parse_task_timestamp(record.heartbeat_at_raw().or_else(|| record.updated_at_raw()))?;
    Some(OffsetDateTime::now_utc() - timestamp)
}

fn task_counts(
    cwd: &Path,
    current_session_id: Option<&str>,
) -> Result<BackgroundTaskCounts, Box<dyn std::error::Error>> {
    let mut counts = BackgroundTaskCounts::default();
    for record in load_task_manifests(cwd)? {
        if record.is_active() {
            counts.total_running += 1;
            if current_session_id
                .zip(record.parent_session_id())
                .is_some_and(|(current, owner)| current == owner)
            {
                counts.session_running += 1;
            }
        }
    }
    Ok(counts)
}

pub(crate) fn count_running_background_tasks(current_session_id: Option<&str>) -> Result<BackgroundTaskCounts, Box<dyn std::error::Error>> {
    task_counts(&env::current_dir()?, current_session_id)
}

pub(crate) fn task_status_label(status: &str) -> &'static str {
    match status {
        "completed" => "done",
        "running" => "run",
        "finishing" => "wrap",
        "stopping" => "stop",
        "failed" => "fail",
        "cancelled" => "stop",
        "interrupted" => "lost",
        _ => "?",
    }
}

pub(crate) fn shorten_task_id(id: &str) -> String {
    id.chars().take(12).collect()
}

pub(crate) fn shorten_session_id_for_report(session_id: &str) -> String {
    if session_id.len() <= 12 {
        session_id.to_string()
    } else {
        session_id[session_id.len() - 12..].to_string()
    }
}

fn task_session_badge(record: &TaskManifestRecord, current_session_id: Option<&str>) -> String {
    match (current_session_id, record.parent_session_id()) {
        (Some(current), Some(owner)) if current == owner => String::from("this-session"),
        (_, Some(owner)) => format!("session:{}", shorten_session_id_for_report(owner)),
        _ => String::from("session:-"),
    }
}

fn task_supervision_summary(record: &TaskManifestRecord) -> Option<String> {
    if let Some(pid) = record.worker_pid() {
        if !process_is_alive(pid) {
            return Some(String::from("worker no longer alive"));
        }
    }

    if !record.is_active() {
        return (record.status() == "interrupted").then(|| String::from("worker no longer alive"));
    }

    let age = task_heartbeat_age(record)?;
    let seconds = age.whole_seconds();
    if seconds <= TASK_HEARTBEAT_DELAY_SECS {
        Some(String::from("healthy"))
    } else if seconds <= TASK_HEARTBEAT_STALLED_SECS {
        Some(format!("delayed (heartbeat {seconds}s old)"))
    } else {
        Some(format!("stalled (heartbeat {seconds}s old)"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskWatchSnapshot {
    status: String,
    detail: Option<String>,
    supervision: Option<String>,
    stop_requested_at: Option<String>,
    stop_reason: Option<String>,
    activity_count: usize,
}

fn task_watch_snapshot(record: &TaskManifestRecord) -> TaskWatchSnapshot {
    TaskWatchSnapshot {
        status: record.status().to_string(),
        detail: record.status_detail().map(ToOwned::to_owned),
        supervision: task_supervision_summary(record),
        stop_requested_at: record.stop_requested_at_raw().map(ToOwned::to_owned),
        stop_reason: record.stop_reason().map(ToOwned::to_owned),
        activity_count: record.activity_entries().len(),
    }
}

fn render_task_activity_line(entry: &TaskActivityEntry) -> String {
    format!(
        "[task] activity {} | {} | {} | {}",
        entry.at,
        entry.kind,
        entry.status,
        truncate_for_summary(&entry.message, 120)
    )
}

fn render_recent_task_activity_lines(record: &TaskManifestRecord, limit: usize) -> Vec<String> {
    let activity = record.activity_entries();
    if activity.is_empty() {
        return Vec::new();
    }

    let start = activity.len().saturating_sub(limit);
    let mut lines = vec![String::from("  Activity")];
    for entry in &activity[start..] {
        lines.push(format!(
            "    - {} | {} | {} | {}",
            entry.at,
            entry.kind,
            entry.status,
            truncate_for_summary(&entry.message, 120)
        ));
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskRestartSpec {
    description: String,
    prompt: String,
    subagent_type: Option<String>,
    name: Option<String>,
    model: Option<String>,
}

fn task_manifest_string<'a>(task: &'a TaskManifestRecord, key: &str) -> Option<&'a str> {
    task.manifest
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn task_paths_match(current_cwd: &Path, task_cwd: &str) -> bool {
    let current = fs::canonicalize(current_cwd).unwrap_or_else(|_| current_cwd.to_path_buf());
    let task_path = PathBuf::from(task_cwd);
    let task = fs::canonicalize(&task_path).unwrap_or(task_path);
    current == task
}

fn extract_task_prompt_from_output(contents: &str) -> Option<String> {
    let marker = "## Prompt";
    let start = contents.find(marker)?;
    let mut prompt = contents[start + marker.len()..]
        .trim_start_matches(['\n', '\r'])
        .to_string();

    for terminator in ["\n## Progress:", "\n## Result"] {
        if let Some(index) = prompt.find(terminator) {
            prompt.truncate(index);
        }
    }

    let prompt = prompt.trim_end().to_string();
    (!prompt.is_empty()).then_some(prompt)
}

fn recover_task_prompt(task: &TaskManifestRecord) -> Result<String, String> {
    if let Some(prompt) = task_manifest_string(task, "prompt") {
        return Ok(prompt.to_string());
    }

    let output_path = task
        .output_file()
        .ok_or_else(|| String::from("the original task prompt was not persisted"))?;
    let contents = fs::read_to_string(output_path)
        .map_err(|_| String::from("the original task log is no longer available"))?;
    extract_task_prompt_from_output(&contents)
        .ok_or_else(|| String::from("the original delegated prompt could not be recovered"))
}

fn task_restart_spec(task: &TaskManifestRecord, current_cwd: &Path) -> Result<TaskRestartSpec, String> {
    if task.task_kind() != "subagent" {
        return Err(String::from("only local subagent tasks can be restarted safely"));
    }
    if task.status() != "interrupted" {
        return Err(format!(
            "task status `{}` is not restartable; only interrupted tasks can be restarted safely",
            task.status()
        ));
    }
    if let Some(task_cwd) = task_manifest_string(task, "cwd") {
        if !task_paths_match(current_cwd, task_cwd) {
            return Err(format!(
                "this task belongs to a different workspace ({task_cwd})"
            ));
        }
    }

    let description = task.description().trim();
    if description.is_empty() {
        return Err(String::from("the original task description is missing"));
    }

    Ok(TaskRestartSpec {
        description: description.to_string(),
        prompt: recover_task_prompt(task)?,
        subagent_type: task_manifest_string(task, "subagentType").map(ToOwned::to_owned),
        name: task_manifest_string(task, "name").map(ToOwned::to_owned),
        model: task.model().map(ToOwned::to_owned),
    })
}

fn render_task_next_action_lines(task: &TaskManifestRecord) -> Vec<String> {
    let supervision = task_supervision_summary(task);
    let is_stalled = supervision
        .as_deref()
        .is_some_and(|value| value.starts_with("stalled"));
    let has_logs = task.output_file().is_some();
    let mut bullets = Vec::new();

    match task.status() {
        "interrupted" => {
            let restart_policy = env::current_dir()
                .map_err(|_| String::from("the current workspace could not be determined"))
                .and_then(|cwd| task_restart_spec(task, &cwd).map(|_| ())); 
            match restart_policy {
                Ok(()) => {
                    bullets.push(format!(
                        "Create a replacement task: /tasks restart {}",
                        task.id()
                    ));
                    bullets.push(String::from(
                        "The original interrupted task is kept for history and log inspection.",
                    ));
                }
                Err(reason) => {
                    bullets.push(format!(
                        "Safe restart is unavailable because {reason}."
                    ));
                    bullets.push(String::from(
                        "Rerun the originating command manually to create a replacement task.",
                    ));
                }
            }
            if has_logs {
                bullets.push(format!("Inspect the saved log: /tasks logs {}", task.id()));
            }
        }
        "running" | "finishing" if is_stalled => {
            bullets.push(format!("Follow live output: /tasks attach {}", task.id()));
            if has_logs {
                bullets.push(format!("Inspect the saved log: /tasks logs {}", task.id()));
            }
            bullets.push(format!(
                "If no new output appears, request a stop with /tasks stop {} and rerun the originating command.",
                task.id()
            ));
        }
        "stopping" if is_stalled => {
            bullets.push(format!("Follow live output: /tasks attach {}", task.id()));
            if has_logs {
                bullets.push(format!("Inspect the saved log: /tasks logs {}", task.id()));
            }
            bullets.push(String::from(
                "A stop was already requested; rerun the originating command after this task exits if you still need fresh work.",
            ));
        }
        _ => {}
    }

    if bullets.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![String::from("  Next action")];
    for bullet in bullets {
        lines.push(format!("    - {bullet}"));
    }
    lines
}

fn render_task_watch_update(
    previous: Option<&TaskWatchSnapshot>,
    record: &TaskManifestRecord,
) -> Option<String> {
    let current = task_watch_snapshot(record);
    let mut lines = Vec::new();
    let activity = record.activity_entries();

    if previous.is_none() {
        lines.push(format!("[task] {} status {}", record.id(), current.status));
        if let Some(entry) = activity.last() {
            lines.push(render_task_activity_line(entry));
        } else if let Some(detail) = current.detail.as_deref().filter(|detail| !detail.trim().is_empty()) {
            lines.push(format!(
                "[task] detail {}",
                truncate_for_summary(detail, 120)
            ));
        }
        if let Some(supervision) = current
            .supervision
            .as_deref()
            .filter(|supervision| *supervision != "healthy")
        {
            lines.push(format!("[task] supervision {supervision}"));
        }
        return Some(lines.join("\n"));
    }

    let previous = previous.expect("previous snapshot present");
    if activity.len() > previous.activity_count {
        for entry in &activity[previous.activity_count..] {
            lines.push(render_task_activity_line(entry));
        }
    }

    if lines.is_empty() && previous.status != current.status {
        lines.push(format!("[task] {} status {}", record.id(), current.status));
    }
    if lines.is_empty() && previous.detail.as_deref() != current.detail.as_deref() {
        if let Some(detail) = current.detail.as_deref().filter(|detail| !detail.trim().is_empty()) {
            lines.push(format!(
                "[task] detail {}",
                truncate_for_summary(detail, 120)
            ));
        }
    }
    if lines.is_empty()
        && previous.stop_requested_at.as_deref() != current.stop_requested_at.as_deref()
    {
        if let Some(stop_requested_at) = current.stop_requested_at.as_deref() {
            lines.push(format!("[task] stop requested {stop_requested_at}"));
        }
    }
    if lines.is_empty() && previous.stop_reason.as_deref() != current.stop_reason.as_deref()
    {
        if let Some(stop_reason) = current.stop_reason.as_deref().filter(|reason| !reason.trim().is_empty()) {
            lines.push(format!(
                "[task] reason {}",
                truncate_for_summary(stop_reason, 120)
            ));
        }
    }
    if previous.supervision.as_deref() != current.supervision.as_deref()
    {
        if let Some(supervision) = current.supervision.as_deref() {
            lines.push(format!("[task] supervision {supervision}"));
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn render_task_terminal_summary(record: &TaskManifestRecord) -> String {
    let mut lines = vec![format!(
        "[task] {} finished with status {}",
        record.id(),
        record.status(),
    )];
    if let Some(detail) = record.status_detail().filter(|detail| !detail.trim().is_empty()) {
        lines.push(format!(
            "[task] detail {}",
            truncate_for_summary(detail, 120)
        ));
    }
    if let Some(stop_reason) = record.stop_reason().filter(|reason| !reason.trim().is_empty()) {
        lines.push(format!(
            "[task] reason {}",
            truncate_for_summary(stop_reason, 120)
        ));
    }
    lines.join("\n")
}

/// Pre-computed lineage mappings for restart chains.
///
/// `successors` maps a task id to the id of the task that replaced it (the
/// successor).  A task acquires a successor when another task sets
/// `restartedFrom` pointing at it.
///
/// `predecessors` is the reverse: it maps a task id back to the id it was
/// restarted from.
struct TaskLineageMap {
    /// task_id → successor_task_id  (the task that was spawned to replace it)
    successors: std::collections::HashMap<String, String>,
    /// task_id → predecessor_task_id  (the task it was restarted from)
    predecessors: std::collections::HashMap<String, String>,
}

impl TaskLineageMap {
    fn build(tasks: &[TaskManifestRecord]) -> Self {
        let mut successors = std::collections::HashMap::new();
        let mut predecessors = std::collections::HashMap::new();
        for task in tasks {
            if let Some(predecessor_id) = task.restarted_from() {
                successors
                    .entry(predecessor_id.to_string())
                    .or_insert_with(|| task.id().to_string());
                predecessors
                    .entry(task.id().to_string())
                    .or_insert_with(|| predecessor_id.to_string());
            }
        }
        Self {
            successors,
            predecessors,
        }
    }

    fn successor_of(&self, task_id: &str) -> Option<&str> {
        self.successors.get(task_id).map(String::as_str)
    }

    fn predecessor_of(&self, task_id: &str) -> Option<&str> {
        self.predecessors.get(task_id).map(String::as_str)
    }
}

fn render_task_entry_lines(
    task: &TaskManifestRecord,
    current_session_id: Option<&str>,
    lineage: &TaskLineageMap,
) -> Vec<String> {
    let mut lines = vec![format!(
        "  [{label:<4}] {id:<12} {status:<11} {session:<20} {desc}",
        label = task_status_label(task.status()),
        id = shorten_task_id(task.id()),
        status = task.status(),
        session = task_session_badge(task, current_session_id),
        desc = truncate_for_summary(task.description(), 52),
    )];
    if let Some(detail) = task.status_detail().filter(|detail| !detail.trim().is_empty()) {
        lines.push(format!("         {}", truncate_for_summary(detail, 88)));
    }
    if let Some(supervision) = task_supervision_summary(task).filter(|_| task.is_active()) {
        lines.push(format!("         supervision {supervision}"));
    }
    if let Some(predecessor) = lineage.predecessor_of(task.id()) {
        lines.push(format!("         ↳ restarted from {}", shorten_task_id(predecessor)));
    }
    if let Some(successor) = lineage.successor_of(task.id()) {
        lines.push(format!("         → replaced by {}", shorten_task_id(successor)));
    }
    lines
}

fn push_task_section(
    lines: &mut Vec<String>,
    title: &str,
    tasks: &[&TaskManifestRecord],
    current_session_id: Option<&str>,
    lineage: &TaskLineageMap,
) {
    if tasks.is_empty() {
        return;
    }
    lines.push(title.to_string());
    for task in tasks {
        lines.extend(render_task_entry_lines(task, current_session_id, lineage));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLogExcerpt {
    text: String,
    total_lines: usize,
    shown_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskLogStreamUpdate {
    text: String,
    next_len: usize,
}

fn task_log_excerpt(contents: &str, max_lines: usize) -> TaskLogExcerpt {
    if contents.is_empty() {
        return TaskLogExcerpt {
            text: String::new(),
            total_lines: 0,
            shown_lines: 0,
        };
    }

    let segments = contents.split_inclusive('\n').collect::<Vec<_>>();
    let total_lines = segments.len();
    let start = total_lines.saturating_sub(max_lines);

    TaskLogExcerpt {
        text: segments[start..].concat(),
        total_lines,
        shown_lines: total_lines.saturating_sub(start),
    }
}

fn task_log_stream_update(
    contents: &str,
    previous_len: usize,
    max_lines: usize,
) -> Option<TaskLogStreamUpdate> {
    if previous_len == 0 {
        let excerpt = task_log_excerpt(contents, max_lines);
        if excerpt.text.is_empty() {
            return None;
        }
        let text = if excerpt.total_lines > excerpt.shown_lines {
            format!(
                "[task] existing log has {} lines; showing last {}\n\n{}",
                excerpt.total_lines, excerpt.shown_lines, excerpt.text
            )
        } else {
            excerpt.text
        };
        return Some(TaskLogStreamUpdate {
            text,
            next_len: contents.len(),
        });
    }

    if contents.len() < previous_len {
        if contents.is_empty() {
            return Some(TaskLogStreamUpdate {
                text: String::from("\n[task] log file was truncated; waiting for new output\n"),
                next_len: 0,
            });
        }

        let excerpt = task_log_excerpt(contents, max_lines);
        let text = format!(
            "\n[task] log file was truncated; showing {} current line{}\n\n{}",
            excerpt.shown_lines,
            if excerpt.shown_lines == 1 { "" } else { "s" },
            excerpt.text,
        );
        return Some(TaskLogStreamUpdate {
            text,
            next_len: contents.len(),
        });
    }

    (contents.len() > previous_len).then(|| TaskLogStreamUpdate {
        text: contents[previous_len..].to_string(),
        next_len: contents.len(),
    })
}

pub(crate) fn render_task_list_report(
    tasks: &[TaskManifestRecord],
    current_session_id: Option<&str>,
) -> String {
    if tasks.is_empty() {
        return String::from("No background tasks.");
    }

    let lineage = TaskLineageMap::build(tasks);
    let running = tasks.iter().filter(|task| task.is_active()).count();
    let session_running = current_session_id.map_or(0, |session_id| {
        tasks.iter()
            .filter(|task| task.is_active() && task.parent_session_id() == Some(session_id))
            .count()
    });
    let mut lines = vec![format!(
        "Background tasks\n  Total            {}\n  Running          {}\n  This session     {}",
        tasks.len(),
        running,
        session_running,
    )];
    if let Some(current_session_id) = current_session_id {
        let current_tasks = tasks
            .iter()
            .filter(|task| task.parent_session_id() == Some(current_session_id))
            .collect::<Vec<_>>();
        let other_tasks = tasks
            .iter()
            .filter(|task| task.parent_session_id() != Some(current_session_id))
            .collect::<Vec<_>>();

        if !current_tasks.is_empty() {
            push_task_section(&mut lines, "Current session", &current_tasks, Some(current_session_id), &lineage);
            push_task_section(&mut lines, "Other tasks", &other_tasks, Some(current_session_id), &lineage);
        } else {
            let all_tasks = tasks.iter().collect::<Vec<_>>();
            push_task_section(&mut lines, "Entries", &all_tasks, Some(current_session_id), &lineage);
        }
    } else {
        let all_tasks = tasks.iter().collect::<Vec<_>>();
        push_task_section(&mut lines, "Entries", &all_tasks, None, &lineage);
    }
    lines.push(String::from("Next"));
    lines.push(String::from("  /tasks show <id>    Inspect manifest metadata"));
    lines.push(String::from("  /tasks logs <id>    Print the current task log"));
    lines.push(String::from("  /tasks attach <id>  Follow the task log until it exits"));
    lines.push(String::from("  /tasks stop <id>    Request a graceful stop"));
    lines.join("\n")
}

pub(crate) fn render_task_show_report(
    task: &TaskManifestRecord,
    current_session_id: Option<&str>,
    all_tasks: Option<&[TaskManifestRecord]>,
) -> String {
    let lineage = all_tasks.map(TaskLineageMap::build);
    let mut lines = vec![String::from("Task")];
    lines.push(format!("  Id               {}", task.id()));
    lines.push(format!("  Kind             {}", task.task_kind()));
    lines.push(format!("  Status           {}", task.status()));
    lines.push(format!("  Session          {}", task_session_badge(task, current_session_id)));
    lines.push(format!("  Description      {}", task.description()));
    if let Some(model) = task.model() {
        lines.push(format!("  Model            {model}"));
    }
    if let Some(predecessor) = lineage.as_ref().and_then(|l| l.predecessor_of(task.id())) {
        lines.push(format!("  Predecessor      {predecessor} (restarted from)"));
    }
    if let Some(successor) = lineage.as_ref().and_then(|l| l.successor_of(task.id())) {
        lines.push(format!("  Successor        {successor} (replaced by)"));
    }
    if let Some(detail) = task.status_detail().filter(|detail| !detail.trim().is_empty()) {
        lines.push(format!("  Detail           {}", truncate_for_summary(detail, 120)));
    }
    if let Some(created_at) = task.created_at_raw() {
        lines.push(format!("  Created          {created_at}"));
    }
    if let Some(started_at) = task.started_at_raw() {
        lines.push(format!("  Started          {started_at}"));
    }
    if let Some(updated_at) = task.updated_at_raw() {
        lines.push(format!("  Updated          {updated_at}"));
    }
    if let Some(heartbeat_at) = task.heartbeat_at_raw() {
        lines.push(format!("  Heartbeat        {heartbeat_at}"));
    }
    if let Some(supervision) = task_supervision_summary(task) {
        lines.push(format!("  Supervision      {supervision}"));
    }
    if let Some(completed_at) = task.completed_at_raw() {
        lines.push(format!("  Completed        {completed_at}"));
    }
    if let Some(stop_requested_at) = task.stop_requested_at_raw() {
        lines.push(format!("  Stop requested   {stop_requested_at}"));
    }
    if let Some(stop_reason) = task.stop_reason() {
        lines.push(format!("  Stop reason      {stop_reason}"));
    }
    if let Some(worker_pid) = task.worker_pid() {
        lines.push(format!("  Worker pid       {worker_pid}"));
    }
    if let Some(output_file) = task.output_file() {
        lines.push(format!("  Log file         {output_file}"));
    }
    lines.push(format!("  Manifest         {}", task.manifest_path.display()));
    lines.extend(render_task_next_action_lines(task));
    lines.extend(render_recent_task_activity_lines(
        task,
        TASK_ACTIVITY_RENDER_LIMIT,
    ));
    lines.join("\n")
}

pub(crate) fn render_task_logs_report(task: &TaskManifestRecord) -> Result<String, Box<dyn std::error::Error>> {
    let output_path = task
        .output_file()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task log path missing"))?;
    let contents = fs::read_to_string(output_path)?;
    let excerpt = task_log_excerpt(&contents, TASK_LOG_TAIL_LINES);
    let mut lines = vec![
        String::from("Task log"),
        format!("  Id               {}", task.id()),
        format!("  Status           {}", task.status()),
    ];
    if let Some(detail) = task.status_detail().filter(|detail| !detail.trim().is_empty()) {
        lines.push(format!("  Detail           {}", truncate_for_summary(detail, 120)));
    }
    if let Some(supervision) = task_supervision_summary(task) {
        lines.push(format!("  Supervision      {supervision}"));
    }
    if let Some(stop_requested_at) = task.stop_requested_at_raw() {
        lines.push(format!("  Stop requested   {stop_requested_at}"));
    }
    if let Some(stop_reason) = task.stop_reason().filter(|reason| !reason.trim().is_empty()) {
        lines.push(format!("  Stop reason      {}", truncate_for_summary(stop_reason, 120)));
    }
    lines.push(format!("  File             {}", output_path));
    if excerpt.total_lines > excerpt.shown_lines {
        lines.push(format!(
            "  Showing          last {} of {} lines",
            excerpt.shown_lines, excerpt.total_lines
        ));
    }
    lines.push(String::new());
    lines.push(excerpt.text);
    Ok(lines.join("\n"))
}

pub(crate) fn find_task_by_prefix<'a>(
    tasks: &'a [TaskManifestRecord],
    requested_id: &str,
) -> Result<&'a TaskManifestRecord, Box<dyn std::error::Error>> {
    let requested = requested_id.trim();
    let mut matches = tasks
        .iter()
        .filter(|task| task.id() == requested || task.id().starts_with(requested))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("task `{requested}` was not found"),
        )
        .into()),
        1 => Ok(matches.remove(0)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("task id `{requested}` is ambiguous; use a longer prefix"),
        )
        .into()),
    }
}

pub(crate) fn request_task_stop(
    cwd: &Path,
    requested_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let tasks = load_task_manifests(cwd)?;
    let task = find_task_by_prefix(&tasks, requested_id)?.clone();
    if task.is_terminal() {
        return Ok(format!(
            "Task stop\n  Id               {}\n  Status           {}\n  Result           already finished",
            task.id(),
            task.status(),
        ));
    }

    let now = chrono_now_iso8601();
    let reason = String::from("Requested from /tasks stop");
    let mut mutable_task = task;
    mutable_task.mark_stop_requested(&now, &reason);
    write_task_manifest(&mutable_task)?;
    Ok(format!(
        "Task stop\n  Id               {}\n  Status           {}\n  Result           stop requested\n  Detail           {}",
        mutable_task.id(),
        mutable_task.status(),
        mutable_task
            .status_detail()
            .unwrap_or("Stop requested; waiting for the current step to finish"),
    ))
}

fn request_task_restart_with<F>(
    cwd: &Path,
    requested_id: &str,
    executor: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce(&serde_json::Value) -> Result<String, Box<dyn std::error::Error>>,
{
    let tasks = load_task_manifests(cwd)?;
    let task = find_task_by_prefix(&tasks, requested_id)?.clone();
    let restart = match task_restart_spec(&task, cwd) {
        Ok(restart) => restart,
        Err(reason) => {
            return Ok(format!(
                "Task restart\n  Id               {}\n  Status           {}\n  Result           blocked\n  Reason           {}",
                task.id(),
                task.status(),
                reason,
            ));
        }
    };

    let payload = serde_json::json!({
        "description": restart.description,
        "prompt": restart.prompt,
        "subagent_type": restart.subagent_type,
        "name": restart.name,
        "model": restart.model,
        "restarted_from": task.id(),
    });
    let output = executor(&payload)?;
    let restarted: serde_json::Value = serde_json::from_str(&output)?;
    let replacement_id = restarted
        .get("agentId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "agent restart response missing agentId"))?
        .to_string();

    let now = chrono_now_iso8601();
    let mut original = task;
    let original_status = original.status().to_string();
    let _ = append_task_activity_value(
        &mut original.manifest,
        &now,
        "restart-requested",
        &original_status,
        &format!("Replacement task {replacement_id} created from interrupted task"),
    );
    write_task_manifest(&original)?;

    Ok(format!(
        "Task restart\n  Source           {}\n  Result           replacement task started\n  Replacement      {}\n  Show             /tasks show {}\n  Attach           /tasks attach {}",
        original.id(),
        replacement_id,
        replacement_id,
        replacement_id,
    ))
}

pub(crate) fn request_task_restart(
    cwd: &Path,
    requested_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    request_task_restart_with(cwd, requested_id, |payload| {
        tools::execute_tool("Agent", payload).map_err(|error| {
            io::Error::new(io::ErrorKind::Other, error.to_string()).into()
        })
    })
}

pub(crate) fn attach_to_task(cwd: &Path, requested_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let tasks = load_task_manifests(cwd)?;
    let task = find_task_by_prefix(&tasks, requested_id)?;
    let output_path = task
        .output_file()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "task log path missing"))?;
    let manifest_path = task.manifest_path.clone();

    println!(
        "Attaching to task {} ({})\n  Log file         {}\n",
        task.id(),
        task.status(),
        output_path,
    );

    let mut last_len = 0usize;
    let mut last_snapshot = None;
    loop {
        if let Ok(contents) = fs::read_to_string(output_path) {
            if let Some(update) = task_log_stream_update(&contents, last_len, TASK_LOG_TAIL_LINES) {
                print!("{}", update.text);
                io::stdout().flush()?;
                last_len = update.next_len;
            }
        }

        let manifest_contents = fs::read_to_string(&manifest_path)?;
        let manifest_value = serde_json::from_str::<serde_json::Value>(&manifest_contents)?;
        let mut record = TaskManifestRecord {
            manifest_path: manifest_path.clone(),
            manifest: manifest_value,
        };
        let _ = reconcile_task_manifest(&mut record);
        let snapshot = task_watch_snapshot(&record);
        if let Some(update) = render_task_watch_update(last_snapshot.as_ref(), &record) {
            println!("\n{update}");
        }
        last_snapshot = Some(snapshot);
        if record.is_terminal() {
            println!("\n{}", render_task_terminal_summary(&record));
            break;
        }

        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        render_task_logs_report, render_task_terminal_summary, render_task_watch_update,
        task_log_excerpt, task_log_stream_update, task_watch_snapshot, TaskManifestRecord,
    };
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ember-task-mgmt-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    fn make_record(root: &PathBuf, manifest: serde_json::Value) -> TaskManifestRecord {
        let manifest_id = manifest
            .get("agentId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("agent-123");
        TaskManifestRecord {
            manifest_path: root.join(format!("{manifest_id}.json")),
            manifest,
        }
    }

    #[test]
    fn task_list_report_groups_current_session_before_other_tasks() {
        let root = temp_dir("task-list");
        fs::create_dir_all(&root).expect("create temp dir");
        let other = make_record(
            &root,
            json!({
                "agentId": "agent-other",
                "status": "running",
                "description": "Other session audit",
                "parentSessionId": "session-other"
            }),
        );
        let current = make_record(
            &root,
            json!({
                "agentId": "agent-current",
                "status": "running",
                "description": "Current session review",
                "parentSessionId": "session-current"
            }),
        );
        let detached = make_record(
            &root,
            json!({
                "agentId": "agent-detached",
                "status": "completed",
                "description": "Detached cleanup"
            }),
        );

        let rendered = super::render_task_list_report(
            &[other, current, detached],
            Some("session-current"),
        );

        assert!(rendered.contains("Current session"));
        assert!(rendered.contains("Other tasks"));
        let current_index = rendered
            .find("Current session review")
            .expect("current task entry");
        let other_index = rendered
            .find("Other session audit")
            .expect("other task entry");
        let detached_index = rendered
            .find("Detached cleanup")
            .expect("detached task entry");
        assert!(current_index < other_index);
        assert!(current_index < detached_index);
        assert!(rendered.contains("this-session"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_logs_report_includes_detail_stop_reason_and_supervision() {
        let root = temp_dir("logs-report");
        fs::create_dir_all(&root).expect("create temp dir");
        let log_path = root.join("agent-123.md");
        let contents = (0..90)
            .map(|index| format!("line {index:03}\n"))
            .collect::<String>();
        fs::write(&log_path, contents).expect("write log file");
        let stale_heartbeat = OffsetDateTime::now_utc()
            .checked_sub(time::Duration::seconds(20))
            .expect("subtract heartbeat age")
            .format(&Rfc3339)
            .expect("format heartbeat");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-123",
                "status": "stopping",
                "description": "Review branch",
                "statusDetail": "Stop requested; waiting for the current step to finish",
                "lastHeartbeatAt": stale_heartbeat,
                "stopRequestedAt": "2026-04-04T00:00:00Z",
                "stopReason": "Requested from /tasks stop",
                "outputFile": log_path.display().to_string(),
            }),
        );

        let report = render_task_logs_report(&record).expect("render log report");

        assert!(report.contains("Task log"));
        assert!(report.contains("Status           stopping"));
        assert!(report.contains("Detail           Stop requested; waiting for the current step to finish"));
        assert!(report.contains("Supervision      delayed (heartbeat 20s old)"));
        assert!(report.contains("Stop requested   2026-04-04T00:00:00Z"));
        assert!(report.contains("Stop reason      Requested from /tasks stop"));
        assert!(report.contains("Showing          last 80 of 90 lines"));
        assert!(!report.contains("line 000"));
        assert!(report.contains("line 010"));
        assert!(report.contains("line 089"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_log_helpers_tail_initial_history_and_handle_truncation() {
        let full_log = (0..100)
            .map(|index| format!("line {index:03}\n"))
            .collect::<String>();
        let excerpt = task_log_excerpt(&full_log, 80);
        assert_eq!(excerpt.total_lines, 100);
        assert_eq!(excerpt.shown_lines, 80);
        assert!(!excerpt.text.contains("line 000"));
        assert!(excerpt.text.contains("line 020"));
        assert!(excerpt.text.contains("line 099"));

        let initial = task_log_stream_update(&full_log, 0, 80).expect("initial attach update");
        assert!(initial.text.contains("existing log has 100 lines; showing last 80"));
        assert!(!initial.text.contains("line 000"));
        assert_eq!(initial.next_len, full_log.len());

        let delta_source = format!("{}line 100\n", full_log);
        let delta = task_log_stream_update(&delta_source, full_log.len(), 80)
            .expect("delta update");
        assert_eq!(delta.text, "line 100\n");
        assert_eq!(delta.next_len, delta_source.len());

        let truncated = String::from("fresh line\nnext line\n");
        let reset = task_log_stream_update(&truncated, delta_source.len(), 80)
            .expect("reset update");
        assert!(reset.text.contains("log file was truncated"));
        assert!(reset.text.contains("fresh line"));
        assert_eq!(reset.next_len, truncated.len());
    }

    #[test]
    fn task_watch_update_reports_activity_and_supervision_changes() {
        let root = temp_dir("watch-update");
        fs::create_dir_all(&root).expect("create temp dir");
        let running = make_record(
            &root,
            json!({
                "agentId": "agent-123",
                "status": "running",
                "description": "Review branch",
                "statusDetail": "Scanning files",
                "activity": [
                    {
                        "at": "2026-04-04T00:00:00Z",
                        "kind": "created",
                        "status": "running",
                        "message": "Queued for background execution"
                    }
                ]
            }),
        );
        let initial = render_task_watch_update(None, &running).expect("initial update");
        assert!(initial.contains("status running"));
        assert!(initial.contains("activity 2026-04-04T00:00:00Z | created | running"));

        let stale_heartbeat = OffsetDateTime::now_utc()
            .checked_sub(time::Duration::seconds(25))
            .expect("subtract heartbeat age")
            .format(&Rfc3339)
            .expect("format heartbeat");
        let stopping = make_record(
            &root,
            json!({
                "agentId": "agent-123",
                "status": "stopping",
                "description": "Review branch",
                "statusDetail": "Stop requested; waiting for the current step to finish",
                "stopRequestedAt": "2026-04-04T00:00:00Z",
                "stopReason": "Requested from /tasks stop",
                "lastHeartbeatAt": stale_heartbeat,
                "activity": [
                    {
                        "at": "2026-04-04T00:00:00Z",
                        "kind": "created",
                        "status": "running",
                        "message": "Queued for background execution"
                    },
                    {
                        "at": "2026-04-04T00:00:02Z",
                        "kind": "stop-requested",
                        "status": "stopping",
                        "message": "Stop requested; waiting for the current step to finish"
                    }
                ]
            }),
        );

        let snapshot = task_watch_snapshot(&running);
        let update = render_task_watch_update(Some(&snapshot), &stopping).expect("transition update");
        assert!(update.contains("activity 2026-04-04T00:00:02Z | stop-requested | stopping"));
        assert!(update.contains("supervision delayed (heartbeat 25s old)"));
        assert!(render_task_watch_update(Some(&task_watch_snapshot(&stopping)), &stopping).is_none());

        let terminal = make_record(
            &root,
            json!({
                "agentId": "agent-123",
                "status": "cancelled",
                "description": "Review branch",
                "statusDetail": "Worker process exited after a stop request",
                "stopReason": "Requested from /tasks stop",
            }),
        );
        let final_summary = render_task_terminal_summary(&terminal);
        assert!(final_summary.contains("finished with status cancelled"));
        assert!(final_summary.contains("detail Worker process exited after a stop request"));
        assert!(final_summary.contains("reason Requested from /tasks stop"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_show_report_includes_supervision_summary_for_active_tasks() {
        let root = temp_dir("show-report");
        fs::create_dir_all(&root).expect("create temp dir");
        let log_path = root.join("agent-999.md");
        fs::write(&log_path, "# Agent Task\n").expect("write log file");
        let stale_heartbeat = OffsetDateTime::now_utc()
            .checked_sub(time::Duration::seconds(50))
            .expect("subtract heartbeat age")
            .format(&Rfc3339)
            .expect("format heartbeat");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-999",
                "status": "running",
                "description": "Background audit",
                "lastHeartbeatAt": stale_heartbeat,
                "workerPid": std::process::id(),
                "outputFile": log_path.display().to_string(),
            }),
        );

        let report = super::render_task_show_report(&record, None, None);

        assert!(report.contains("Status           running"));
        assert!(report.contains("Supervision      stalled (heartbeat 50s old)"));
        assert!(report.contains("  Next action"));
        assert!(report.contains("/tasks attach agent-999"));
        assert!(report.contains("/tasks logs agent-999"));
        assert!(report.contains("/tasks stop agent-999"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_show_report_omits_recovery_guidance_for_healthy_tasks() {
        let root = temp_dir("show-healthy");
        fs::create_dir_all(&root).expect("create temp dir");
        let current_heartbeat = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .expect("format heartbeat");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-healthy",
                "status": "running",
                "description": "Healthy task",
                "lastHeartbeatAt": current_heartbeat,
                "workerPid": std::process::id(),
            }),
        );

        let report = super::render_task_show_report(&record, None, None);

        assert!(report.contains("Supervision      healthy"));
        assert!(!report.contains("  Next action"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_show_report_includes_recovery_guidance_for_interrupted_tasks() {
        let root = temp_dir("show-interrupted");
        fs::create_dir_all(&root).expect("create temp dir");
        let log_path = root.join("agent-777.md");
        fs::write(&log_path, "# Agent Task\n").expect("write log file");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-777",
                "status": "interrupted",
                "description": "Interrupted task",
                "statusDetail": "Worker process is no longer alive; task was interrupted",
                "outputFile": log_path.display().to_string(),
            }),
        );

        let report = super::render_task_show_report(&record, None, None);

        assert!(report.contains("Status           interrupted"));
        assert!(report.contains("  Next action"));
        assert!(report.contains("Safe restart is unavailable because"));
        assert!(report.contains("/tasks logs agent-777"));
        assert!(report.contains("replacement task"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_show_report_includes_restart_hint_for_restartable_interrupted_tasks() {
        let root = temp_dir("show-restartable");
        fs::create_dir_all(&root).expect("create temp dir");
        let log_path = root.join("agent-778.md");
        fs::write(
            &log_path,
            "# Agent Task\n\n- id: agent-778\n\n## Prompt\n\nRe-check the interrupted work\n\n## Result\n\n- status: interrupted\n",
        )
        .expect("write log file");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-778",
                "status": "interrupted",
                "description": "Interrupted task",
                "statusDetail": "Worker process is no longer alive; task was interrupted",
                "subagentType": "Explore",
                "name": "interrupted-task",
                "outputFile": log_path.display().to_string(),
            }),
        );

        let report = super::render_task_show_report(&record, None, None);

        assert!(report.contains("Status           interrupted"));
        assert!(report.contains("/tasks restart agent-778"));
        assert!(report.contains("kept for history"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_restart_request_creates_replacement_task_and_records_activity() {
        let root = temp_dir("restart-task");
        let task_dir = root.join(".ember-agents");
        fs::create_dir_all(&task_dir).expect("create task dir");
        let manifest_path = task_dir.join("agent-123.json");
        let output_path = task_dir.join("agent-123.md");
        fs::write(
            &output_path,
            "# Agent Task\n\n- id: agent-123\n\n## Prompt\n\nRetry the interrupted task\n",
        )
        .expect("write output file");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "agentId": "agent-123",
                "status": "interrupted",
                "taskKind": "subagent",
                "name": "ship-audit",
                "description": "Audit the branch",
                "subagentType": "Explore",
                "model": "claude-opus-4-6",
                "outputFile": output_path.display().to_string(),
                "manifestFile": manifest_path.display().to_string(),
                "activity": [
                    {
                        "at": "2026-04-04T00:00:00Z",
                        "kind": "terminal",
                        "status": "interrupted",
                        "message": "Worker process is no longer alive; task was interrupted"
                    }
                ]
            }))
            .expect("manifest json"),
        )
        .expect("write manifest file");

        let replacement_manifest = task_dir.join("agent-456.json");
        let replacement_output = task_dir.join("agent-456.md");
        let report = super::request_task_restart_with(&root, "agent-123", |payload| {
            assert_eq!(payload["description"], "Audit the branch");
            assert_eq!(payload["prompt"], "Retry the interrupted task");
            assert_eq!(payload["subagent_type"], "Explore");
            assert_eq!(payload["name"], "ship-audit");
            assert_eq!(payload["model"], "claude-opus-4-6");
            assert_eq!(payload["restarted_from"], "agent-123");

            fs::write(&replacement_output, "# Agent Task\n").expect("write replacement log");
            fs::write(
                &replacement_manifest,
                serde_json::to_string_pretty(&json!({
                    "agentId": "agent-456",
                    "status": "running",
                    "outputFile": replacement_output.display().to_string(),
                    "manifestFile": replacement_manifest.display().to_string(),
                    "activity": [
                        {
                            "at": "2026-04-04T00:00:02Z",
                            "kind": "created",
                            "status": "running",
                            "message": "Queued for background execution"
                        },
                        {
                            "at": "2026-04-04T00:00:02Z",
                            "kind": "restarted",
                            "status": "running",
                            "message": "Restarted from interrupted task agent-123"
                        }
                    ]
                }))
                .expect("replacement json"),
            )
            .expect("write replacement manifest");

            Ok(json!({
                "agentId": "agent-456",
                "manifestFile": replacement_manifest.display().to_string(),
                "outputFile": replacement_output.display().to_string(),
                "status": "running"
            })
            .to_string())
        })
        .expect("restart should succeed");

        assert!(report.contains("replacement task started"));
        assert!(report.contains("Replacement      agent-456"));
        assert!(report.contains("/tasks show agent-456"));

        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&manifest_path).expect("read persisted manifest"),
        )
        .expect("persisted json");
        assert!(persisted["activity"]
            .as_array()
            .expect("activity array")
            .iter()
            .any(|entry| entry["kind"] == "restart-requested" && entry["message"].as_str().expect("message").contains("agent-456")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_restart_request_reports_blocked_when_prompt_cannot_be_recovered() {
        let root = temp_dir("restart-blocked");
        let task_dir = root.join(".ember-agents");
        fs::create_dir_all(&task_dir).expect("create task dir");
        let manifest_path = task_dir.join("agent-999.json");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "agentId": "agent-999",
                "status": "interrupted",
                "taskKind": "subagent",
                "description": "Blocked task",
                "manifestFile": manifest_path.display().to_string()
            }))
            .expect("manifest json"),
        )
        .expect("write manifest file");

        let report = super::request_task_restart_with(&root, "agent-999", |_payload| {
            panic!("executor should not be called for blocked restarts");
        })
        .expect("blocked report should succeed");

        assert!(report.contains("Result           blocked"));
        assert!(report.contains("delegated prompt could not be recovered") || report.contains("prompt was not persisted"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_show_report_includes_recent_activity_entries() {
        let root = temp_dir("show-activity");
        fs::create_dir_all(&root).expect("create temp dir");
        let record = make_record(
            &root,
            json!({
                "agentId": "agent-321",
                "status": "completed",
                "description": "Review branch",
                "activity": [
                    {
                        "at": "2026-04-04T00:00:00Z",
                        "kind": "created",
                        "status": "running",
                        "message": "Queued for background execution"
                    },
                    {
                        "at": "2026-04-04T00:00:04Z",
                        "kind": "status",
                        "status": "finishing",
                        "message": "Completed with 3 tool calls"
                    },
                    {
                        "at": "2026-04-04T00:00:05Z",
                        "kind": "terminal",
                        "status": "completed",
                        "message": "Finished successfully"
                    }
                ]
            }),
        );

        let report = super::render_task_show_report(&record, None, None);

        assert!(report.contains("  Activity"));
        assert!(report.contains("2026-04-04T00:00:00Z | created | running | Queued for background execution"));
        assert!(report.contains("2026-04-04T00:00:05Z | terminal | completed | Finished successfully"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lineage_map_surfaces_predecessor_and_successor() {
        let root = temp_dir("lineage-map");
        fs::create_dir_all(&root).expect("root dir");

        let original = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-orig",
                "name": "do-thing",
                "description": "Original task",
                "status": "interrupted"
            }),
        );
        let replacement = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-repl",
                "name": "do-thing",
                "description": "Replacement task",
                "status": "running",
                "restartedFrom": "agent-orig"
            }),
        );

        let tasks = vec![original, replacement];
        let lineage = super::TaskLineageMap::build(&tasks);

        assert_eq!(lineage.successor_of("agent-orig"), Some("agent-repl"));
        assert_eq!(lineage.predecessor_of("agent-repl"), Some("agent-orig"));
        assert_eq!(lineage.predecessor_of("agent-orig"), None);
        assert_eq!(lineage.successor_of("agent-repl"), None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn show_report_renders_lineage_fields_when_tasks_provided() {
        let root = temp_dir("lineage-show");
        fs::create_dir_all(&root).expect("root dir");

        let original = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-orig",
                "name": "do-thing",
                "description": "Original task",
                "status": "interrupted"
            }),
        );
        let replacement = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-repl",
                "name": "do-thing",
                "description": "Replacement task",
                "status": "running",
                "restartedFrom": "agent-orig"
            }),
        );

        let tasks = vec![original.clone(), replacement.clone()];

        let orig_report = super::render_task_show_report(&original, None, Some(&tasks));
        assert!(
            orig_report.contains("Successor"),
            "original should show successor: {orig_report}"
        );
        assert!(orig_report.contains("agent-repl"));

        let repl_report = super::render_task_show_report(&replacement, None, Some(&tasks));
        assert!(
            repl_report.contains("Predecessor"),
            "replacement should show predecessor: {repl_report}"
        );
        assert!(repl_report.contains("agent-orig"));

        let solo_report = super::render_task_show_report(&original, None, None);
        assert!(!solo_report.contains("Successor"));
        assert!(!solo_report.contains("Predecessor"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_report_shows_lineage_tags() {
        let root = temp_dir("lineage-list");
        fs::create_dir_all(&root).expect("root dir");

        let original = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-orig",
                "name": "do-thing",
                "description": "Original task",
                "status": "interrupted",
                "parentSessionId": "session-1"
            }),
        );
        let replacement = make_record(
            &root,
            json!({
                "version": 1,
                "taskKind": "subagent",
                "agentId": "agent-repl",
                "name": "do-thing",
                "description": "Replacement task",
                "status": "running",
                "parentSessionId": "session-1",
                "restartedFrom": "agent-orig"
            }),
        );

        let tasks = vec![original, replacement];
        let report = super::render_task_list_report(&tasks, Some("session-1"));

        assert!(
            report.contains("restarted from"),
            "list should show restart origin: {report}"
        );
        assert!(
            report.contains("replaced by"),
            "list should show replacement: {report}"
        );

        let _ = fs::remove_dir_all(root);
    }
}
