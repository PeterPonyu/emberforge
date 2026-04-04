//! Background task management — listing, stopping, attaching to agents.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::{chrono_now_iso8601, truncate_for_summary};

const TASK_LOG_TAIL_LINES: usize = 80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BackgroundTaskCounts {
    pub(crate) total_running: usize,
    pub(crate) session_running: usize,
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

    fn worker_pid(&self) -> Option<u32> {
        self.manifest
            .get("workerPid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
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
    }
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
    #[cfg(not(target_os = "linux"))]
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

fn task_health_badge(record: &TaskManifestRecord) -> Option<String> {
    if !record.is_active() {
        return None;
    }
    let age = task_heartbeat_age(record)?;
    (age.whole_seconds() > 15).then(|| format!("heartbeat {}s old", age.whole_seconds()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskWatchSnapshot {
    status: String,
    detail: Option<String>,
    health: Option<String>,
    stop_requested_at: Option<String>,
    stop_reason: Option<String>,
}

fn task_watch_snapshot(record: &TaskManifestRecord) -> TaskWatchSnapshot {
    TaskWatchSnapshot {
        status: record.status().to_string(),
        detail: record.status_detail().map(ToOwned::to_owned),
        health: task_health_badge(record),
        stop_requested_at: record.stop_requested_at_raw().map(ToOwned::to_owned),
        stop_reason: record.stop_reason().map(ToOwned::to_owned),
    }
}

fn render_task_watch_update(
    previous: Option<&TaskWatchSnapshot>,
    record: &TaskManifestRecord,
) -> Option<String> {
    let current = task_watch_snapshot(record);
    let mut lines = Vec::new();

    if previous.map(|snapshot| snapshot.status.as_str()) != Some(current.status.as_str()) {
        lines.push(format!("[task] {} status {}", record.id(), current.status));
    }
    if previous.and_then(|snapshot| snapshot.detail.as_deref()) != current.detail.as_deref() {
        if let Some(detail) = current.detail.as_deref().filter(|detail| !detail.trim().is_empty()) {
            lines.push(format!(
                "[task] detail {}",
                truncate_for_summary(detail, 120)
            ));
        }
    }
    if previous.and_then(|snapshot| snapshot.stop_requested_at.as_deref())
        != current.stop_requested_at.as_deref()
    {
        if let Some(stop_requested_at) = current.stop_requested_at.as_deref() {
            lines.push(format!("[task] stop requested {stop_requested_at}"));
        }
    }
    if previous.and_then(|snapshot| snapshot.stop_reason.as_deref())
        != current.stop_reason.as_deref()
    {
        if let Some(stop_reason) = current.stop_reason.as_deref().filter(|reason| !reason.trim().is_empty()) {
            lines.push(format!(
                "[task] reason {}",
                truncate_for_summary(stop_reason, 120)
            ));
        }
    }
    if previous.and_then(|snapshot| snapshot.health.as_deref()) != current.health.as_deref() {
        if let Some(health) = current.health.as_deref() {
            lines.push(format!("[task] health {health}"));
        }
    }

    if previous.is_none() && lines.is_empty() {
        lines.push(format!("[task] {} status {}", record.id(), current.status));
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

fn render_task_entry_lines(
    task: &TaskManifestRecord,
    current_session_id: Option<&str>,
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
    if let Some(health) = task_health_badge(task) {
        lines.push(format!("         {health}"));
    }
    lines
}

fn push_task_section(
    lines: &mut Vec<String>,
    title: &str,
    tasks: &[&TaskManifestRecord],
    current_session_id: Option<&str>,
) {
    if tasks.is_empty() {
        return;
    }
    lines.push(title.to_string());
    for task in tasks {
        lines.extend(render_task_entry_lines(task, current_session_id));
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
            push_task_section(&mut lines, "Current session", &current_tasks, Some(current_session_id));
            push_task_section(&mut lines, "Other tasks", &other_tasks, Some(current_session_id));
        } else {
            let all_tasks = tasks.iter().collect::<Vec<_>>();
            push_task_section(&mut lines, "Entries", &all_tasks, Some(current_session_id));
        }
    } else {
        let all_tasks = tasks.iter().collect::<Vec<_>>();
        push_task_section(&mut lines, "Entries", &all_tasks, None);
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
) -> String {
    let mut lines = vec![String::from("Task")];
    lines.push(format!("  Id               {}", task.id()));
    lines.push(format!("  Kind             {}", task.task_kind()));
    lines.push(format!("  Status           {}", task.status()));
    lines.push(format!("  Session          {}", task_session_badge(task, current_session_id)));
    lines.push(format!("  Description      {}", task.description()));
    if let Some(model) = task.model() {
        lines.push(format!("  Model            {model}"));
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
    if let Some(health) = task_health_badge(task) {
        lines.push(format!("  Health           {health}"));
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
    if let Some(health) = task_health_badge(task) {
        lines.push(format!("  Health           {health}"));
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
    fn task_logs_report_includes_detail_stop_reason_and_health() {
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
        assert!(report.contains("Health           heartbeat"));
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
    fn task_watch_update_reports_status_detail_stop_and_health_changes() {
        let root = temp_dir("watch-update");
        fs::create_dir_all(&root).expect("create temp dir");
        let running = make_record(
            &root,
            json!({
                "agentId": "agent-123",
                "status": "running",
                "description": "Review branch",
                "statusDetail": "Scanning files",
            }),
        );
        let initial = render_task_watch_update(None, &running).expect("initial update");
        assert!(initial.contains("status running"));
        assert!(initial.contains("detail Scanning files"));

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
            }),
        );

        let snapshot = task_watch_snapshot(&running);
        let update = render_task_watch_update(Some(&snapshot), &stopping).expect("transition update");
        assert!(update.contains("status stopping"));
        assert!(update.contains("detail Stop requested; waiting for the current step to finish"));
        assert!(update.contains("stop requested 2026-04-04T00:00:00Z"));
        assert!(update.contains("reason Requested from /tasks stop"));
        assert!(update.contains("health heartbeat"));

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
}
