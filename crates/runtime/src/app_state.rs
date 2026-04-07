use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Context describing the currently active team for a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamContext {
    pub team_name: String,
    pub team_file_path: PathBuf,
    pub lead_agent_id: String,
}

/// Session-scoped application state shared across tool invocations.
///
/// Wrap in `Arc<AppState>` and pass via `ToolExecutionContext` to tool
/// implementations that need to read or write team context.
///
/// `team_context` is held under a `Mutex` so that `execute_team_create` can
/// write it and `execute_team_delete` can clear it without requiring `&mut`
/// access to the `AppState` itself.
#[derive(Debug, Default)]
pub struct AppState {
    pub team_context: Mutex<Option<TeamContext>>,
}

impl AppState {
    /// Construct an empty `AppState` with no active team.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            team_context: Mutex::new(None),
        })
    }

    /// Read the current team context (acquires mutex, clones the value).
    #[must_use]
    pub fn get_team_context(&self) -> Option<TeamContext> {
        self.team_context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the current team context.
    pub fn set_team_context(&self, ctx: Option<TeamContext>) {
        let mut guard = self
            .team_context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = ctx;
    }
}
