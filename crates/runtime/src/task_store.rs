//! Disk-backed task store: persistent task state using `.ember-agents/` manifests.
//!
//! Provides a typed API over the same JSON manifest format used by the agent
//! system, so that tasks created via `TaskCreate` tools are visible to the CLI's
//! `/tasks` command and vice versa.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TASK_ID_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const STALL_POLL_INTERVAL: Duration = Duration::from_secs(5);
const SIGKILL_GRACE: Duration = Duration::from_secs(5);

/// Patterns that suggest a shell command is waiting for interactive input.
const INTERACTIVE_PROMPT_PATTERNS: &[&str] = &[
    "(y/n)", "(Y/n)", "(yes/no)", "[y/N]", "[Y/n]", "[yes/no]",
    "Do you", "Would you", "Are you sure",
    "Press Enter", "Continue?", "Overwrite?",
    "Password:", "password:", "passphrase",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Task kind — shell subprocess or agent conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Shell,
    Agent,
}

/// Task status lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Finishing,
    Stopping,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Finishing | Self::Stopping)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Finishing => "finishing",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Persistent task manifest stored as `.ember-agents/{task_id}.json`.
///
/// Field names match the existing agent manifest format for compatibility
/// with the CLI's `task_mgmt.rs` loader.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskManifest {
    /// Unique task identifier (e.g. `b8k3m9p1`).
    pub agent_id: String,
    /// Human-readable description.
    pub description: String,
    /// Current status.
    pub status: String,
    /// Task kind discriminator.
    #[serde(default = "default_task_kind")]
    pub task_kind: String,
    /// The prompt or command to execute.
    #[serde(default)]
    pub prompt: String,
    /// Model override (for agent tasks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Path to the output/log file.
    #[serde(default)]
    pub output_file: String,
    /// PID of the worker process (for shell tasks + supervision).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_pid: Option<u32>,
    /// ISO 8601 timestamps.
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Heartbeat for liveness monitoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<String>,
    /// Brief status detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    /// Stop request timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_requested_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Activity log entries.
    #[serde(default)]
    pub activity: Vec<serde_json::Value>,
}

fn default_task_kind() -> String {
    "subagent".to_string()
}

/// XML notification generated when a task reaches terminal status.
#[derive(Debug, Clone)]
pub struct TaskNotification {
    pub task_id: String,
    pub status: String,
    pub summary: String,
    pub output_tail: String,
}

// ---------------------------------------------------------------------------
// Task ID generation
// ---------------------------------------------------------------------------

/// Generate a task ID: prefix char + 8 random base-36 chars.
#[must_use]
pub fn generate_task_id(kind: TaskKind) -> String {
    let prefix = match kind {
        TaskKind::Shell => 'b',
        TaskKind::Agent => 'a',
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = u128::from(std::process::id());
    let seed = nanos.wrapping_mul(pid.wrapping_add(1));

    let mut id = String::with_capacity(9);
    id.push(prefix);
    let mut state = seed;
    for _ in 0..8 {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let idx = usize::try_from(state >> 64).unwrap_or(0) % TASK_ID_ALPHABET.len();
        id.push(TASK_ID_ALPHABET[idx] as char);
    }
    id
}

// ---------------------------------------------------------------------------
// Store directory
// ---------------------------------------------------------------------------

/// Resolve the task store directory, preferring env vars, then walking ancestors.
pub fn task_store_dir() -> io::Result<PathBuf> {
    if let Ok(path) = std::env::var("EMBER_AGENT_STORE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    if let Ok(path) = std::env::var("CLAW_AGENT_STORE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let cwd = std::env::current_dir()?;
    for ancestor in cwd.ancestors() {
        let dir = ancestor.join(".ember-agents");
        if dir.is_dir() {
            return Ok(dir);
        }
    }
    // Default: create in cwd
    Ok(cwd.join(".ember-agents"))
}

// ---------------------------------------------------------------------------
// Manifest CRUD
// ---------------------------------------------------------------------------

fn manifest_path(store_dir: &Path, task_id: &str) -> PathBuf {
    store_dir.join(format!("{task_id}.json"))
}

fn output_path(store_dir: &Path, task_id: &str) -> PathBuf {
    store_dir.join(format!("{task_id}.output"))
}

/// Create a new task manifest and persist it to disk.
pub fn create_task_manifest(
    kind: TaskKind,
    name: &str,
    prompt: &str,
    model: Option<&str>,
) -> io::Result<TaskManifest> {
    let store_dir = task_store_dir()?;
    fs::create_dir_all(&store_dir)?;

    let task_id = generate_task_id(kind);
    let output_file = output_path(&store_dir, &task_id);
    let now = iso8601_now();

    // Create empty output file
    fs::write(&output_file, "")?;

    let manifest = TaskManifest {
        agent_id: task_id.clone(),
        description: name.to_string(),
        status: TaskStatus::Pending.as_str().to_string(),
        task_kind: match kind {
            TaskKind::Shell => "shell".to_string(),
            TaskKind::Agent => "subagent".to_string(),
        },
        prompt: prompt.to_string(),
        model: model.map(str::to_string),
        output_file: output_file.display().to_string(),
        worker_pid: None,
        created_at: now.clone(),
        started_at: None,
        completed_at: None,
        updated_at: Some(now),
        last_heartbeat_at: None,
        status_detail: Some("Task created, waiting to start".to_string()),
        stop_requested_at: None,
        stop_reason: None,
        activity: Vec::new(),
    };

    save_manifest(&store_dir, &manifest)?;
    Ok(manifest)
}

/// Save a manifest to disk (atomic write via tempfile + rename).
pub fn save_manifest(store_dir: &Path, manifest: &TaskManifest) -> io::Result<()> {
    let path = manifest_path(store_dir, &manifest.agent_id);
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load a task manifest from disk by ID.
pub fn load_manifest(task_id: &str) -> io::Result<TaskManifest> {
    let store_dir = task_store_dir()?;
    let path = manifest_path(&store_dir, task_id);
    let json = fs::read_to_string(&path)?;
    serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// List all task manifests from the store directory.
pub fn list_manifests(status_filter: Option<&str>) -> io::Result<Vec<TaskManifest>> {
    let store_dir = task_store_dir()?;
    if !store_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut manifests = Vec::new();
    for entry in fs::read_dir(&store_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // Skip temp files
        if path.to_string_lossy().ends_with(".json.tmp") {
            continue;
        }
        let Ok(json) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<TaskManifest>(&json) else {
            continue;
        };
        if let Some(filter) = status_filter {
            if manifest.status != filter {
                continue;
            }
        }
        manifests.push(manifest);
    }

    // Sort by updated_at descending
    manifests.sort_by(|a, b| {
        b.updated_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.updated_at.as_deref().unwrap_or(""))
    });

    Ok(manifests)
}

/// Update a manifest field and persist.
pub fn update_manifest_status(
    task_id: &str,
    status: TaskStatus,
    detail: Option<&str>,
) -> io::Result<TaskManifest> {
    let store_dir = task_store_dir()?;
    let mut manifest = load_manifest(task_id)?;
    let now = iso8601_now();

    manifest.status = status.as_str().to_string();
    manifest.updated_at = Some(now.clone());
    manifest.last_heartbeat_at = Some(now.clone());

    if let Some(d) = detail {
        manifest.status_detail = Some(d.to_string());
    }

    if status == TaskStatus::Running && manifest.started_at.is_none() {
        manifest.started_at = Some(now.clone());
    }

    if status.is_terminal() && manifest.completed_at.is_none() {
        manifest.completed_at = Some(now.clone());
    }

    // Append activity entry
    manifest.activity.push(serde_json::json!({
        "at": now,
        "kind": "status",
        "status": status.as_str(),
        "message": detail.unwrap_or(""),
    }));
    // Cap activity log
    if manifest.activity.len() > 40 {
        let excess = manifest.activity.len() - 40;
        manifest.activity.drain(..excess);
    }

    save_manifest(&store_dir, &manifest)?;
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Shell task execution
// ---------------------------------------------------------------------------

/// Spawn a shell task: runs `command` as a subprocess, streams output to disk file.
/// Returns the updated manifest with PID and running status.
pub fn spawn_shell_task(task_id: &str, command: &str) -> io::Result<TaskManifest> {
    let store_dir = task_store_dir()?;
    let mut manifest = load_manifest(task_id)?;
    let output_file_path = PathBuf::from(&manifest.output_file);

    // Open output file for writing
    let stdout_file = fs::File::create(&output_file_path)?;
    let stderr_file = stdout_file.try_clone()?;

    // Spawn subprocess
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()?;

    let pid = child.id();
    let now = iso8601_now();

    manifest.status = TaskStatus::Running.as_str().to_string();
    manifest.worker_pid = Some(pid);
    manifest.started_at = Some(now.clone());
    manifest.updated_at = Some(now.clone());
    manifest.last_heartbeat_at = Some(now.clone());
    manifest.status_detail = Some(format!("Running (PID {pid})"));

    manifest.activity.push(serde_json::json!({
        "at": now,
        "kind": "status",
        "status": "running",
        "message": format!("Spawned shell process (PID {pid})"),
    }));

    save_manifest(&store_dir, &manifest)?;

    // Spawn monitor thread
    let monitor_task_id = task_id.to_string();
    let monitor_output = output_file_path.clone();
    thread::Builder::new()
        .name(format!("task-monitor-{task_id}"))
        .spawn(move || {
            monitor_shell_task(child, &monitor_task_id, &monitor_output);
        })
        .map_err(io::Error::other)?;

    // Spawn stall watchdog
    let watchdog_task_id = task_id.to_string();
    let watchdog_output = output_file_path;
    thread::Builder::new()
        .name(format!("task-watchdog-{task_id}"))
        .spawn(move || {
            stall_watchdog(&watchdog_task_id, &watchdog_output);
        })
        .map_err(io::Error::other)?;

    Ok(manifest)
}

/// Monitor a shell child process: wait for exit, update manifest status.
fn monitor_shell_task(mut child: Child, task_id: &str, _output_path: &Path) {
    let exit_status = child.wait();

    let (status, detail) = match exit_status {
        Ok(es) if es.success() => (
            TaskStatus::Completed,
            "Exited with code 0".to_string(),
        ),
        Ok(es) => {
            let code = es.code().unwrap_or(-1);
            (
                TaskStatus::Failed,
                format!("Exited with code {code}"),
            )
        }
        Err(e) => (
            TaskStatus::Failed,
            format!("Process error: {e}"),
        ),
    };

    // Check if stop was requested
    let final_status = if let Ok(manifest) = load_manifest(task_id) {
        if manifest.stop_requested_at.is_some() {
            TaskStatus::Cancelled
        } else {
            status
        }
    } else {
        status
    };

    let _ = update_manifest_status(task_id, final_status, Some(&detail));

    // Enqueue notification
    let _ = write_task_notification(task_id, final_status.as_str(), &detail);

    // Log
    eprintln!(
        "\x1b[2m[task {task_id}] {} — {detail}\x1b[0m",
        final_status.as_str()
    );
}

/// Stall watchdog: poll output file for interactive prompt patterns.
fn stall_watchdog(task_id: &str, output_path: &Path) {
    let mut last_size: u64 = 0;
    let mut stall_count: u32 = 0;

    loop {
        thread::sleep(STALL_POLL_INTERVAL);

        // Check if task is still active
        let Ok(manifest) = load_manifest(task_id) else { break };
        let status: TaskStatus = serde_json::from_value(
            serde_json::Value::String(manifest.status.clone()),
        )
        .unwrap_or(TaskStatus::Running);

        if status.is_terminal() {
            break;
        }

        // Update heartbeat
        let _ = update_heartbeat(task_id);

        // Check for growth
        let current_size = fs::metadata(output_path)
            .map(|m| m.len())
            .unwrap_or(0);

        if current_size == last_size {
            stall_count += 1;

            // After 9 polls (45s) with no growth, check for interactive prompt
            if stall_count >= 9 {
                if let Ok(content) = fs::read_to_string(output_path) {
                    if let Some(last_line) = content.lines().last() {
                        for pattern in INTERACTIVE_PROMPT_PATTERNS {
                            if last_line.contains(pattern) {
                                let _ = update_manifest_status(
                                    task_id,
                                    TaskStatus::Running,
                                    Some(&format!(
                                        "Stalled: may be waiting for input ({pattern})"
                                    )),
                                );
                                // Don't spam — reset counter
                                stall_count = 0;
                                break;
                            }
                        }
                    }
                }
            }
        } else {
            stall_count = 0;
            last_size = current_size;
        }
    }
}

fn update_heartbeat(task_id: &str) -> io::Result<()> {
    let store_dir = task_store_dir()?;
    let path = manifest_path(&store_dir, task_id);
    let json = fs::read_to_string(&path)?;
    let mut manifest: TaskManifest =
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    manifest.last_heartbeat_at = Some(iso8601_now());
    save_manifest(&store_dir, &manifest)
}

// ---------------------------------------------------------------------------
// Task stop
// ---------------------------------------------------------------------------

/// Request a task to stop. Sends SIGTERM, waits, then SIGKILL if needed.
pub fn stop_task(task_id: &str) -> io::Result<bool> {
    let store_dir = task_store_dir()?;
    let mut manifest = load_manifest(task_id)?;
    let now = iso8601_now();

    let status: TaskStatus = serde_json::from_value(
        serde_json::Value::String(manifest.status.clone()),
    )
    .unwrap_or(TaskStatus::Running);

    if status.is_terminal() {
        return Ok(false);
    }

    manifest.stop_requested_at = Some(now.clone());
    manifest.stop_reason = Some("User requested stop".to_string());
    manifest.status = TaskStatus::Stopping.as_str().to_string();
    manifest.updated_at = Some(now.clone());
    manifest.status_detail = Some("Stop requested; sending SIGTERM".to_string());

    manifest.activity.push(serde_json::json!({
        "at": now,
        "kind": "stop-requested",
        "status": "stopping",
        "message": "User requested stop",
    }));

    save_manifest(&store_dir, &manifest)?;

    // Send SIGTERM to the process
    if let Some(pid) = manifest.worker_pid {
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();

            // Wait briefly, then SIGKILL if still alive
            let kill_pid = pid;
            thread::spawn(move || {
                thread::sleep(SIGKILL_GRACE);
                if process_is_alive(kill_pid) {
                    let _ = Command::new("kill")
                        .arg("-KILL")
                        .arg(kill_pid.to_string())
                        .status();
                }
            });
        }
    }

    Ok(true)
}

/// Read task output (tail N lines).
pub fn read_task_output(task_id: &str, tail: usize) -> io::Result<(String, bool)> {
    let manifest = load_manifest(task_id)?;
    let content = fs::read_to_string(&manifest.output_file)?;
    let lines: Vec<&str> = content.lines().collect();
    let truncated = lines.len() > tail;
    let output = if truncated {
        lines[lines.len() - tail..].join("\n")
    } else {
        content
    };
    Ok((output, truncated))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Global notification queue — pending notifications to inject into next user message.
static NOTIFICATION_QUEUE: std::sync::LazyLock<Mutex<Vec<TaskNotification>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

fn write_task_notification(task_id: &str, status: &str, summary: &str) -> io::Result<()> {
    let output_tail = match read_task_output(task_id, 20) {
        Ok((tail, _)) => tail,
        Err(_) => String::new(),
    };

    let notification = TaskNotification {
        task_id: task_id.to_string(),
        status: status.to_string(),
        summary: summary.to_string(),
        output_tail,
    };

    NOTIFICATION_QUEUE
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))?
        .push(notification);

    Ok(())
}

/// Drain all pending task notifications, returning XML fragments for injection.
pub fn drain_notifications() -> Vec<String> {
    let Ok(mut queue) = NOTIFICATION_QUEUE.lock() else { return Vec::new() };
    let notifications: Vec<TaskNotification> = queue.drain(..).collect();
    drop(queue);

    notifications
        .into_iter()
        .map(|n| {
            format!(
                "<task-notification>\n\
                 <task-id>{}</task-id>\n\
                 <status>{}</status>\n\
                 <summary>{}</summary>\n\
                 <output>\n{}\n</output>\n\
                 </task-notification>",
                n.task_id, n.status, n.summary, n.output_tail
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn iso8601_now() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // RFC 3339 approximation
    let days = secs / 86400;
    let years = 1970 + days / 365;
    let remaining_days = days % 365;
    let months = remaining_days / 30 + 1;
    let day = remaining_days % 30 + 1;
    let hour = (secs % 86400) / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    format!(
        "{years:04}-{months:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    )
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Path::new(&format!("/proc/{pid}")).exists()
            || Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that modify the env var must hold the lock to avoid races.
    fn setup_test_store() -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "ember-task-store-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("EMBER_AGENT_STORE", dir.display().to_string());
        (dir, guard)
    }

    fn cleanup_test_store(dir: &Path) {
        std::env::remove_var("EMBER_AGENT_STORE");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn task_id_generation() {
        let id1 = generate_task_id(TaskKind::Shell);
        let id2 = generate_task_id(TaskKind::Agent);
        assert!(id1.starts_with('b'));
        assert!(id2.starts_with('a'));
        assert_eq!(id1.len(), 9);
        assert_ne!(id1, id2);
    }

    #[test]
    fn create_and_load_manifest() {
        let (dir, _guard) = setup_test_store();
        let manifest = create_task_manifest(TaskKind::Shell, "test task", "echo hello", None).unwrap();
        assert_eq!(manifest.status, "pending");
        assert!(manifest.agent_id.starts_with('b'));

        let loaded = load_manifest(&manifest.agent_id).unwrap();
        assert_eq!(loaded.agent_id, manifest.agent_id);
        assert_eq!(loaded.description, "test task");
        assert_eq!(loaded.prompt, "echo hello");

        cleanup_test_store(&dir);
    }

    #[test]
    fn list_and_filter_manifests() {
        let (dir, _guard) = setup_test_store();
        create_task_manifest(TaskKind::Shell, "task1", "echo 1", None).unwrap();
        let m2 = create_task_manifest(TaskKind::Shell, "task2", "echo 2", None).unwrap();

        update_manifest_status(&m2.agent_id, TaskStatus::Running, Some("running")).unwrap();

        let all = list_manifests(None).unwrap();
        assert_eq!(all.len(), 2);

        let running = list_manifests(Some("running")).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].agent_id, m2.agent_id);

        cleanup_test_store(&dir);
    }

    #[test]
    fn status_transitions() {
        let (dir, _guard) = setup_test_store();
        let manifest = create_task_manifest(TaskKind::Shell, "test", "true", None).unwrap();

        let updated = update_manifest_status(&manifest.agent_id, TaskStatus::Running, Some("started")).unwrap();
        assert_eq!(updated.status, "running");
        assert!(updated.started_at.is_some());

        let completed = update_manifest_status(&manifest.agent_id, TaskStatus::Completed, Some("done")).unwrap();
        assert_eq!(completed.status, "completed");
        assert!(completed.completed_at.is_some());

        cleanup_test_store(&dir);
    }

    #[test]
    fn spawn_and_monitor_shell_task() {
        let (dir, _guard) = setup_test_store();
        let manifest = create_task_manifest(TaskKind::Shell, "echo test", "echo hello world", None).unwrap();
        let task_id = manifest.agent_id.clone();

        let running = spawn_shell_task(&task_id, "echo hello world").unwrap();
        assert_eq!(running.status, "running");
        assert!(running.worker_pid.is_some());

        // Wait for subprocess to finish
        thread::sleep(Duration::from_millis(500));

        // Monitor thread should have updated status
        let final_manifest = load_manifest(&task_id).unwrap();
        assert_eq!(final_manifest.status, "completed");

        // Output file should have content
        let (output, _) = read_task_output(&task_id, 100).unwrap();
        assert!(output.contains("hello world"));

        cleanup_test_store(&dir);
    }

    #[test]
    fn stop_task_sends_signal() {
        let (dir, _guard) = setup_test_store();
        let manifest = create_task_manifest(TaskKind::Shell, "long task", "sleep 60", None).unwrap();
        let task_id = manifest.agent_id.clone();

        let _ = spawn_shell_task(&task_id, "sleep 60").unwrap();

        // Stop the task
        thread::sleep(Duration::from_millis(200));
        let stopped = stop_task(&task_id).unwrap();
        assert!(stopped);

        // Wait for process to die
        thread::sleep(Duration::from_secs(2));

        let final_manifest = load_manifest(&task_id).unwrap();
        assert!(
            final_manifest.status == "cancelled" || final_manifest.status == "stopping",
            "Expected cancelled or stopping, got: {}",
            final_manifest.status
        );

        cleanup_test_store(&dir);
    }

    #[test]
    fn notification_queue() {
        let _ = write_task_notification("test-123", "completed", "Task finished");
        let notifications = drain_notifications();
        assert!(!notifications.is_empty());
        let last = notifications.last().unwrap();
        assert!(last.contains("<task-id>test-123</task-id>"));
        assert!(last.contains("<status>completed</status>"));
    }

    #[test]
    fn activity_log_capped() {
        let (dir, _guard) = setup_test_store();
        let manifest = create_task_manifest(TaskKind::Shell, "test", "true", None).unwrap();
        let task_id = manifest.agent_id;

        for i in 0..50 {
            let _ = update_manifest_status(
                &task_id,
                TaskStatus::Running,
                Some(&format!("update {i}")),
            );
        }

        let loaded = load_manifest(&task_id).unwrap();
        assert!(loaded.activity.len() <= 40, "Activity log should be capped at 40");

        cleanup_test_store(&dir);
    }
}
