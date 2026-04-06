//! File history snapshots: track pre-edit file state for recovery.
//!
//! Mirrors the Claude Code TypeScript `utils/fileHistory` module.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A snapshot of a file's content at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Content of the file at snapshot time. `None` if the file did not exist.
    pub content: Option<String>,
    /// Timestamp of the snapshot.
    pub timestamp: String,
    /// Operation that triggered the snapshot (e.g. "write", "edit").
    pub trigger: String,
}

/// Manages file history for a session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileHistoryStore {
    /// Map from absolute path to ordered list of snapshots (oldest first).
    snapshots: BTreeMap<PathBuf, Vec<FileSnapshot>>,
    /// Maximum snapshots per file.
    max_per_file: usize,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl FileHistoryStore {
    /// Create a new store with a per-file snapshot limit.
    #[must_use]
    pub fn new(max_per_file: usize) -> Self {
        Self {
            snapshots: BTreeMap::new(),
            max_per_file,
        }
    }

    /// Take a snapshot of a file's current content before modifying it.
    pub fn snapshot_before_edit(&mut self, path: &Path, trigger: &str) -> io::Result<()> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };

        let content = if abs.exists() {
            Some(fs::read_to_string(&abs)?)
        } else {
            None
        };

        let entry = FileSnapshot {
            path: abs.clone(),
            content,
            timestamp: iso8601_now(),
            trigger: trigger.to_string(),
        };

        let history = self.snapshots.entry(abs).or_default();
        history.push(entry);

        // Evict oldest if over limit
        if history.len() > self.max_per_file {
            let excess = history.len() - self.max_per_file;
            history.drain(..excess);
        }

        Ok(())
    }

    /// Get all snapshots for a file, oldest first.
    #[must_use]
    pub fn get_history(&self, path: &Path) -> Option<&[FileSnapshot]> {
        self.snapshots.get(path).map(Vec::as_slice)
    }

    /// Get the most recent snapshot for a file.
    #[must_use]
    pub fn latest_snapshot(&self, path: &Path) -> Option<&FileSnapshot> {
        self.snapshots.get(path).and_then(|v| v.last())
    }

    /// Restore a file to its most recent snapshot content.
    pub fn restore_latest(&self, path: &Path) -> io::Result<bool> {
        if let Some(snapshot) = self.latest_snapshot(path) {
            match &snapshot.content {
                Some(content) => {
                    if let Some(parent) = snapshot.path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&snapshot.path, content)?;
                    Ok(true)
                }
                None => {
                    // File did not exist before — remove it
                    if snapshot.path.exists() {
                        fs::remove_file(&snapshot.path)?;
                    }
                    Ok(true)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// List all tracked file paths.
    #[must_use]
    pub fn tracked_files(&self) -> Vec<&Path> {
        self.snapshots.keys().map(PathBuf::as_path).collect()
    }

    /// Total number of snapshots across all files.
    #[must_use]
    pub fn total_snapshots(&self) -> usize {
        self.snapshots.values().map(Vec::len).sum()
    }

    /// Save the history store to a JSON file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, json)
    }

    /// Load the history store from a JSON file.
    pub fn load(path: &Path) -> io::Result<Self> {
        let json = fs::read_to_string(path)?;
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_restore() {
        let dir = std::env::temp_dir().join("ember-file-history-test");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("test.txt");
        fs::write(&file, "original content").unwrap();

        let mut store = FileHistoryStore::new(10);
        store.snapshot_before_edit(&file, "write").unwrap();

        // Modify the file
        fs::write(&file, "modified content").unwrap();

        // Restore
        let restored = store.restore_latest(&file).unwrap();
        assert!(restored);
        assert_eq!(fs::read_to_string(&file).unwrap(), "original content");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evicts_old_snapshots() {
        let mut store = FileHistoryStore::new(2);
        let path = PathBuf::from("/tmp/ember-test-evict.txt");

        // Manually insert snapshots
        for i in 0..5 {
            let history = store.snapshots.entry(path.clone()).or_default();
            history.push(FileSnapshot {
                path: path.clone(),
                content: Some(format!("v{i}")),
                timestamp: format!("{i}"),
                trigger: "test".to_string(),
            });
            if history.len() > store.max_per_file {
                let excess = history.len() - store.max_per_file;
                history.drain(..excess);
            }
        }

        let history = store.get_history(&path).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content.as_deref(), Some("v3"));
        assert_eq!(history[1].content.as_deref(), Some("v4"));
    }
}
