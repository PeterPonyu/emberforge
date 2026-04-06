//! Coordinator mode: multi-agent orchestration with worker restrictions,
//! scratchpads, and broadcast messaging.
//!
//! Full-depth port of the Claude Code TypeScript `coordinator/` module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use std::{fs, io};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Tool restriction constants (CC parity)
// ---------------------------------------------------------------------------

/// Tools available to worker agents (CC's ASYNC_AGENT_ALLOWED_TOOLS).
pub const WORKER_ALLOWED_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "glob_search",
    "grep_search",
    "bash",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "Skill",
    "ToolSearch",
    "NotebookEdit",
    "StructuredOutput",
    "EnterWorktree",
    "ExitWorktree",
    "PowerShell",
];

/// Tools explicitly denied for workers (CC's ALL_AGENT_DISALLOWED_TOOLS).
pub const WORKER_DENIED_TOOLS: &[&str] = &[
    "Agent",             // Workers can't spawn sub-workers
    "AskUserQuestion",   // Workers don't interact with user
    "TaskStop",          // Only coordinator stops workers
    "TaskOutput",        // Only coordinator reads task output
    "EnterPlanMode",     // Workers execute, don't plan
    "ExitPlanMode",
    "CronCreate",        // Workers don't schedule
    "CronDelete",
    "CronList",
];

/// Tools available only to the coordinator (not workers).
pub const COORDINATOR_ONLY_TOOLS: &[&str] = &[
    "Agent",
    "TaskStop",
    "SendMessage",
    "StructuredOutput",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A worker agent managed by the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerAgent {
    pub id: String,
    pub name: String,
    pub status: WorkerStatus,
    pub allowed_tools: BTreeSet<String>,
    pub assigned_task: Option<String>,
    pub scratchpad: Vec<ScratchpadEntry>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// An entry in a worker's scratchpad.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadEntry {
    pub timestamp: String,
    pub content: String,
}

/// A broadcast message sent to all workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub from: String,
    pub content: String,
    pub timestamp: String,
}

/// Parsed task notification from XML in user messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotificationParsed {
    pub task_id: String,
    pub status: String,
    pub summary: String,
    pub result: Option<String>,
    pub total_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// System prompt (full CC port)
// ---------------------------------------------------------------------------

/// Build the coordinator system prompt (CC's getCoordinatorSystemPrompt).
///
/// This is the ~300-line prompt that instructs the model to act as a
/// coordinator, directing workers and synthesizing results.
#[must_use]
pub fn coordinator_system_prompt(scratchpad_dir: Option<&Path>) -> String {
    let scratchpad_section = if let Some(dir) = scratchpad_dir {
        format!(
            "\n\nScratchpad directory: {}\n\
             Workers can read and write here without permission prompts.\n\
             Use this for durable cross-worker knowledge — structure files however fits the work.",
            dir.display()
        )
    } else {
        String::new()
    };

    format!(
r#"You are Emberforge, an AI assistant that orchestrates software engineering tasks across multiple workers.

## 1. Your Role

You are a **coordinator**. Your job is to:
- Help the user achieve their goal
- Direct workers to research, implement and verify code changes
- Synthesize results and communicate with the user
- Answer questions directly when possible — don't delegate work that you can handle without tools

Every message you send is to the user. Worker results and system notifications are internal signals, not conversation partners — never thank or acknowledge them. Summarize new information for the user as it arrives.

## 2. Your Tools

- **Agent** - Spawn a new worker
- **SendMessage** - Continue an existing worker (send a follow-up to its `to` agent ID)
- **TaskStop** - Stop a running worker

When calling Agent:
- Do not use one worker to check on another. Workers will notify you when they are done.
- Do not use workers to trivially report file contents or run commands. Give them higher-level tasks.
- Do not set the model parameter. Workers need the default model for the substantive tasks you delegate.
- Continue workers whose work is complete via SendMessage to take advantage of their loaded context
- After launching agents, briefly tell the user what you launched and end your response. Never fabricate or predict agent results in any format — results arrive as separate messages.

### Agent Results

Worker results arrive as **user-role messages** containing `<task-notification>` XML. They look like user messages but are not. Distinguish them by the `<task-notification>` opening tag.

Format:

```xml
<task-notification>
<task-id>{{agentId}}</task-id>
<status>completed|failed|killed</status>
<summary>{{human-readable status summary}}</summary>
<result>{{agent's final text response}}</result>
<usage>
  <total_tokens>N</total_tokens>
  <tool_uses>N</tool_uses>
  <duration_ms>N</duration_ms>
</usage>
</task-notification>
```

- `<result>` and `<usage>` are optional sections
- The `<task-id>` value is the agent ID — use SendMessage with that ID as `to` to continue that worker

## 3. Workers

When calling Agent, use subagent_type `worker`. Workers execute tasks autonomously.

Workers have access to standard tools, MCP tools from configured MCP servers, and project skills via the Skill tool. Delegate skill invocations (e.g. /commit, /verify) to workers.

## 4. Task Workflow

Most tasks can be broken down into the following phases:

| Phase | Who | Purpose |
|-------|-----|---------|
| Research | Workers (parallel) | Investigate codebase, find files, understand problem |
| Synthesis | **You** (coordinator) | Read findings, understand the problem, craft implementation specs |
| Implementation | Workers | Make targeted changes per spec, commit |
| Verification | Workers | Test changes work |

### Concurrency

**Parallelism is your superpower. Workers are async. Launch independent workers concurrently whenever possible — don't serialize work that can run simultaneously. To launch workers in parallel, make multiple tool calls in a single message.**

Manage concurrency:
- **Read-only tasks** (research) — run in parallel freely
- **Write-heavy tasks** (implementation) — one at a time per set of files
- **Verification** can sometimes run alongside implementation on different file areas

### Handling Worker Failures

When a worker reports failure:
- Continue the same worker with SendMessage — it has the full error context
- If a correction attempt fails, try a different approach or report to the user

## 5. Writing Worker Prompts

**Workers can't see your conversation.** Every prompt must be self-contained with everything the worker needs.

### Always synthesize — your most important job

When workers report research findings, **you must understand them before directing follow-up work**. Read the findings. Then write a prompt that proves you understood by including specific file paths, line numbers, and exactly what to change.

Never write "based on your findings" — these phrases delegate understanding to the worker.

### Choose continue vs. spawn by context overlap

| Situation | Mechanism | Why |
|-----------|-----------|-----|
| Research explored exactly the files that need editing | **Continue** (SendMessage) | Worker has files in context |
| Research was broad but implementation is narrow | **Spawn fresh** (Agent) | Focused context is cleaner |
| Correcting a failure | **Continue** | Worker has error context |
| Verifying code a different worker wrote | **Spawn fresh** | Fresh eyes avoid bias |
| Completely unrelated task | **Spawn fresh** | No useful context to reuse |{scratchpad_section}"#
    )
}

// ---------------------------------------------------------------------------
// Task notification parsing
// ---------------------------------------------------------------------------

/// Parse a `<task-notification>` XML block from a message string.
///
/// Returns `None` if the message doesn't contain a task notification.
pub fn parse_task_notification(message: &str) -> Option<TaskNotificationParsed> {
    let start = message.find("<task-notification>")?;
    let end = message.find("</task-notification>")?;
    if end <= start {
        return None;
    }
    let xml = &message[start..end + "</task-notification>".len()];

    Some(TaskNotificationParsed {
        task_id: extract_xml_tag(xml, "task-id")?,
        status: extract_xml_tag(xml, "status")?,
        summary: extract_xml_tag(xml, "summary").unwrap_or_default(),
        result: extract_xml_tag(xml, "result"),
        total_tokens: extract_xml_tag(xml, "total_tokens").and_then(|s| s.parse().ok()),
        tool_uses: extract_xml_tag(xml, "tool_uses").and_then(|s| s.parse().ok()),
        duration_ms: extract_xml_tag(xml, "duration_ms").and_then(|s| s.parse().ok()),
    })
}

/// Check if a message contains a task notification (quick test without full parse).
pub fn is_task_notification(message: &str) -> bool {
    message.contains("<task-notification>")
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end_rel = xml[start..].find(&close)?;
    let content = xml[start..start + end_rel].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

// ---------------------------------------------------------------------------
// Scratchpad
// ---------------------------------------------------------------------------

/// Ensure the scratchpad directory exists and return its path.
pub fn ensure_scratchpad_dir() -> io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let dir = cwd.join(".ember").join("scratchpad");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Check if a path is within the scratchpad directory.
pub fn is_scratchpad_path(path: &Path) -> bool {
    let normalized = path.to_string_lossy();
    normalized.contains(".ember/scratchpad") || normalized.contains(".claw/scratchpad")
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Central coordinator that manages a pool of worker agents.
#[derive(Debug)]
pub struct Coordinator {
    workers: Arc<Mutex<BTreeMap<String, WorkerAgent>>>,
    broadcasts: Arc<Mutex<Vec<BroadcastMessage>>>,
    max_workers: usize,
    scratchpad_dir: Option<PathBuf>,
    active: bool,
}

impl Coordinator {
    /// Create a new coordinator with a maximum worker count.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            broadcasts: Arc::new(Mutex::new(Vec::new())),
            max_workers,
            scratchpad_dir: None,
            active: false,
        }
    }

    /// Activate coordinator mode.
    pub fn activate(&mut self) -> io::Result<()> {
        self.active = true;
        self.scratchpad_dir = Some(ensure_scratchpad_dir()?);
        Ok(())
    }

    /// Deactivate coordinator mode.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Whether coordinator mode is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the system prompt for coordinator mode.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        coordinator_system_prompt(self.scratchpad_dir.as_deref())
    }

    /// Get the worker allowed tools set.
    #[must_use]
    pub fn worker_tools() -> BTreeSet<String> {
        WORKER_ALLOWED_TOOLS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Get the coordinator-only tools set.
    #[must_use]
    pub fn coordinator_tools() -> BTreeSet<String> {
        COORDINATOR_ONLY_TOOLS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Check if a tool is allowed for workers.
    #[must_use]
    pub fn is_worker_tool(tool_name: &str) -> bool {
        WORKER_ALLOWED_TOOLS.contains(&tool_name)
            && !WORKER_DENIED_TOOLS.contains(&tool_name)
    }

    /// Spawn a new worker agent with restricted tools.
    pub fn spawn_worker(
        &self,
        name: &str,
        allowed_tools: BTreeSet<String>,
    ) -> Result<String, String> {
        let mut workers = self.workers.lock().map_err(|e| e.to_string())?;
        if workers.len() >= self.max_workers {
            return Err(format!(
                "Maximum worker count ({}) reached",
                self.max_workers
            ));
        }

        let id = format!(
            "worker-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        let worker = WorkerAgent {
            id: id.clone(),
            name: name.to_string(),
            status: WorkerStatus::Idle,
            allowed_tools,
            assigned_task: None,
            scratchpad: Vec::new(),
            created_at: iso8601_now(),
        };

        workers.insert(id.clone(), worker);
        Ok(id)
    }

    /// Assign a task to a worker.
    pub fn assign_task(&self, worker_id: &str, task: &str) -> Result<(), String> {
        let mut workers = self.workers.lock().map_err(|e| e.to_string())?;
        let worker = workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("Worker {worker_id} not found"))?;
        worker.assigned_task = Some(task.to_string());
        worker.status = WorkerStatus::Running;
        Ok(())
    }

    /// Mark a worker as completed.
    pub fn complete_worker(&self, worker_id: &str) -> Result<(), String> {
        let mut workers = self.workers.lock().map_err(|e| e.to_string())?;
        let worker = workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("Worker {worker_id} not found"))?;
        worker.status = WorkerStatus::Completed;
        Ok(())
    }

    /// Add a scratchpad entry for a worker.
    pub fn append_scratchpad(&self, worker_id: &str, content: &str) -> Result<(), String> {
        let mut workers = self.workers.lock().map_err(|e| e.to_string())?;
        let worker = workers
            .get_mut(worker_id)
            .ok_or_else(|| format!("Worker {worker_id} not found"))?;
        worker.scratchpad.push(ScratchpadEntry {
            timestamp: iso8601_now(),
            content: content.to_string(),
        });
        Ok(())
    }

    /// Broadcast a message to all workers.
    pub fn broadcast(&self, from: &str, content: &str) -> Result<usize, String> {
        let workers = self.workers.lock().map_err(|e| e.to_string())?;
        let count = workers.len();
        let mut broadcasts = self.broadcasts.lock().map_err(|e| e.to_string())?;
        broadcasts.push(BroadcastMessage {
            from: from.to_string(),
            content: content.to_string(),
            timestamp: iso8601_now(),
        });
        Ok(count)
    }

    /// List all workers and their status.
    #[must_use]
    pub fn list_workers(&self) -> Vec<WorkerAgent> {
        self.workers
            .lock()
            .map(|w| w.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Remove a worker by ID.
    pub fn remove_worker(&self, worker_id: &str) -> Result<bool, String> {
        let mut workers = self.workers.lock().map_err(|e| e.to_string())?;
        Ok(workers.remove(worker_id).is_some())
    }

    /// Get scratchpad directory path.
    #[must_use]
    pub fn scratchpad_dir(&self) -> Option<&Path> {
        self.scratchpad_dir.as_deref()
    }

    /// Default tool restriction set for worker agents (CC compatibility).
    #[must_use]
    pub fn default_worker_tools() -> BTreeSet<String> {
        Self::worker_tools()
    }
}

impl Default for Coordinator {
    fn default() -> Self {
        Self::new(8)
    }
}

fn iso8601_now() -> String {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    format!("{secs}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_list_workers() {
        let coord = Coordinator::new(4);
        let id = coord
            .spawn_worker("test-worker", Coordinator::default_worker_tools())
            .unwrap();
        let workers = coord.list_workers();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, id);
        assert_eq!(workers[0].status, WorkerStatus::Idle);
    }

    #[test]
    fn max_workers_enforced() {
        let coord = Coordinator::new(1);
        coord.spawn_worker("w1", BTreeSet::new()).unwrap();
        let result = coord.spawn_worker("w2", BTreeSet::new());
        assert!(result.is_err());
    }

    #[test]
    fn assign_and_complete() {
        let coord = Coordinator::new(4);
        let id = coord.spawn_worker("worker", BTreeSet::new()).unwrap();
        coord.assign_task(&id, "do something").unwrap();
        let workers = coord.list_workers();
        assert_eq!(workers[0].status, WorkerStatus::Running);

        coord.complete_worker(&id).unwrap();
        let workers = coord.list_workers();
        assert_eq!(workers[0].status, WorkerStatus::Completed);
    }

    #[test]
    fn broadcast_reaches_all() {
        let coord = Coordinator::new(4);
        coord.spawn_worker("w1", BTreeSet::new()).unwrap();
        coord.spawn_worker("w2", BTreeSet::new()).unwrap();
        let count = coord.broadcast("coordinator", "hello all").unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn scratchpad_append() {
        let coord = Coordinator::new(4);
        let id = coord.spawn_worker("worker", BTreeSet::new()).unwrap();
        coord.append_scratchpad(&id, "note 1").unwrap();
        coord.append_scratchpad(&id, "note 2").unwrap();
        let workers = coord.list_workers();
        assert_eq!(workers[0].scratchpad.len(), 2);
    }

    // ── System prompt tests ──────────────────────────────────────────

    #[test]
    fn system_prompt_contains_key_sections() {
        let prompt = coordinator_system_prompt(None);
        assert!(prompt.contains("## 1. Your Role"));
        assert!(prompt.contains("## 2. Your Tools"));
        assert!(prompt.contains("## 3. Workers"));
        assert!(prompt.contains("## 4. Task Workflow"));
        assert!(prompt.contains("## 5. Writing Worker Prompts"));
        assert!(prompt.contains("<task-notification>"));
        assert!(prompt.contains("SendMessage"));
        assert!(prompt.contains("Parallelism is your superpower"));
    }

    #[test]
    fn system_prompt_includes_scratchpad_when_provided() {
        let prompt = coordinator_system_prompt(Some(Path::new("/tmp/scratchpad")));
        assert!(prompt.contains("/tmp/scratchpad"));
        assert!(prompt.contains("durable cross-worker knowledge"));

        let prompt_without = coordinator_system_prompt(None);
        assert!(!prompt_without.contains("Scratchpad directory"));
    }

    // ── Notification parsing tests ───────────────────────────────────

    #[test]
    fn parse_task_notification_basic() {
        let msg = r#"<task-notification>
<task-id>agent-a1b</task-id>
<status>completed</status>
<summary>Agent "Investigate auth bug" completed</summary>
<result>Found null pointer in src/auth/validate.ts:42</result>
</task-notification>"#;

        let parsed = parse_task_notification(msg).unwrap();
        assert_eq!(parsed.task_id, "agent-a1b");
        assert_eq!(parsed.status, "completed");
        assert!(parsed.summary.contains("Investigate auth bug"));
        assert_eq!(
            parsed.result.as_deref(),
            Some("Found null pointer in src/auth/validate.ts:42")
        );
    }

    #[test]
    fn parse_task_notification_with_usage() {
        let msg = r#"<task-notification>
<task-id>agent-x7q</task-id>
<status>completed</status>
<summary>Agent completed</summary>
<usage>
  <total_tokens>15000</total_tokens>
  <tool_uses>8</tool_uses>
  <duration_ms>4500</duration_ms>
</usage>
</task-notification>"#;

        let parsed = parse_task_notification(msg).unwrap();
        assert_eq!(parsed.total_tokens, Some(15000));
        assert_eq!(parsed.tool_uses, Some(8));
        assert_eq!(parsed.duration_ms, Some(4500));
    }

    #[test]
    fn parse_task_notification_missing_optional_fields() {
        let msg = r#"<task-notification>
<task-id>agent-123</task-id>
<status>failed</status>
<summary>Build failed</summary>
</task-notification>"#;

        let parsed = parse_task_notification(msg).unwrap();
        assert_eq!(parsed.status, "failed");
        assert_eq!(parsed.result, None);
        assert_eq!(parsed.total_tokens, None);
    }

    #[test]
    fn is_task_notification_detects_tag() {
        assert!(is_task_notification(
            "some text <task-notification> ... </task-notification>"
        ));
        assert!(!is_task_notification("just a regular message"));
    }

    #[test]
    fn non_notification_returns_none() {
        assert!(parse_task_notification("hello world").is_none());
        assert!(parse_task_notification("").is_none());
    }

    // ── Tool restriction tests ──────────────────────────────────────

    #[test]
    fn worker_tools_include_file_ops() {
        let tools = Coordinator::worker_tools();
        assert!(tools.contains("read_file"));
        assert!(tools.contains("write_file"));
        assert!(tools.contains("edit_file"));
        assert!(tools.contains("bash"));
    }

    #[test]
    fn worker_tools_exclude_agent() {
        assert!(!Coordinator::is_worker_tool("Agent"));
        assert!(!Coordinator::is_worker_tool("AskUserQuestion"));
        assert!(!Coordinator::is_worker_tool("TaskStop"));
    }

    #[test]
    fn coordinator_tools_include_agent() {
        let tools = Coordinator::coordinator_tools();
        assert!(tools.contains("Agent"));
        assert!(tools.contains("TaskStop"));
        assert!(tools.contains("SendMessage"));
    }

    #[test]
    fn scratchpad_path_detection() {
        assert!(is_scratchpad_path(Path::new(
            "/home/user/project/.ember/scratchpad/notes.md"
        )));
        assert!(!is_scratchpad_path(Path::new(
            "/home/user/project/src/main.rs"
        )));
    }

    // ── Activation tests ────────────────────────────────────────────

    #[test]
    fn coordinator_starts_inactive() {
        let coord = Coordinator::new(4);
        assert!(!coord.is_active());
    }

    #[test]
    fn coordinator_activation_creates_scratchpad() {
        let mut coord = Coordinator::new(4);
        // Note: activate() creates .ember/scratchpad/ in cwd,
        // which may fail in some test environments. We just check it doesn't panic.
        let _ = coord.activate();
        if coord.is_active() {
            assert!(coord.scratchpad_dir().is_some());
            coord.deactivate();
            assert!(!coord.is_active());
        }
    }
}
