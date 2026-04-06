//! Coordinator mode: multi-agent orchestration with worker restrictions,
//! scratchpads, and broadcast messaging.
//!
//! Mirrors the Claude Code TypeScript `coordinator/` module.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Central coordinator that manages a pool of worker agents.
#[derive(Debug)]
pub struct Coordinator {
    workers: Arc<Mutex<BTreeMap<String, WorkerAgent>>>,
    broadcasts: Arc<Mutex<Vec<BroadcastMessage>>>,
    max_workers: usize,
}

impl Coordinator {
    /// Create a new coordinator with a maximum worker count.
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            broadcasts: Arc::new(Mutex::new(Vec::new())),
            max_workers,
        }
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

    /// Default tool restriction set for worker agents.
    #[must_use]
    pub fn default_worker_tools() -> BTreeSet<String> {
        [
            "read_file",
            "write_file",
            "edit_file",
            "glob_search",
            "grep_search",
            "bash",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "SendMessage",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
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
        coord
            .spawn_worker("w1", BTreeSet::new())
            .unwrap();
        let result = coord.spawn_worker("w2", BTreeSet::new());
        assert!(result.is_err());
    }

    #[test]
    fn assign_and_complete() {
        let coord = Coordinator::new(4);
        let id = coord
            .spawn_worker("worker", BTreeSet::new())
            .unwrap();
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
        let id = coord
            .spawn_worker("worker", BTreeSet::new())
            .unwrap();
        coord.append_scratchpad(&id, "note 1").unwrap();
        coord.append_scratchpad(&id, "note 2").unwrap();
        let workers = coord.list_workers();
        assert_eq!(workers[0].scratchpad.len(), 2);
    }
}
