use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const TEAM_LEAD_NAME: &str = "team-lead";

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TeamFile {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: u64,
    pub lead_agent_id: String,
    pub lead_session_id: String,
    pub members: Vec<TeamMember>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TeamMember {
    pub agent_id: String,
    pub name: String,
    pub agent_type: String,
    pub model: String,
    pub joined_at: u64,
    pub tmux_pane_id: String,
    pub cwd: String,
    pub subscriptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

// ---------------------------------------------------------------------------
// Session cleanup registry
// ---------------------------------------------------------------------------

fn cleanup_registry() -> &'static Mutex<HashSet<String>> {
    static REGISTRY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn register_team_for_session_cleanup(name: &str) {
    if let Ok(mut set) = cleanup_registry().lock() {
        set.insert(name.to_string());
    }
}

pub fn unregister_team_for_session_cleanup(name: &str) {
    if let Ok(mut set) = cleanup_registry().lock() {
        set.remove(name);
    }
}

/// Drains and returns all registered team names. Called by session teardown.
#[must_use]
pub fn take_registered_teams_for_cleanup() -> Vec<String> {
    if let Ok(mut set) = cleanup_registry().lock() {
        let names: Vec<String> = set.iter().cloned().collect();
        set.clear();
        names
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// File path helpers
// ---------------------------------------------------------------------------

/// Returns the base teams directory:
///   `$XDG_DATA_HOME/emberforge/teams`  or  `~/.emberforge/teams`
#[must_use]
pub fn default_teams_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg).join("emberforge").join("teams")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".emberforge").join("teams")
    }
}

/// Returns the path for a team's JSON file under `teams_dir`.
#[must_use]
pub fn get_team_file_path(name: &str, teams_dir: &Path) -> PathBuf {
    teams_dir.join(format!("{name}.json"))
}

// ---------------------------------------------------------------------------
// Read / write
// ---------------------------------------------------------------------------

/// Reads and deserialises a team file. Returns `None` if the file does not exist.
#[must_use]
pub fn read_team_file(name: &str, teams_dir: &Path) -> Option<TeamFile> {
    let path = get_team_file_path(name, teams_dir);
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Serialises and writes a team file, creating parent directories as needed.
pub fn write_team_file(name: &str, team: &TeamFile, teams_dir: &Path) -> Result<(), String> {
    let path = get_team_file_path(name, teams_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(team).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Removes the team JSON file for `name`. Safe to call even if the file is
/// already absent.
pub fn cleanup_team_directories(name: &str, teams_dir: &Path) -> Result<(), String> {
    let path = get_team_file_path(name, teams_dir);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unique name generation
// ---------------------------------------------------------------------------

const ADJECTIVES: &[&str] = &["brave", "calm", "dark", "fast", "keen", "mild", "neat", "bold", "wise", "warm"];
const ANIMALS: &[&str] = &["bear", "crow", "deer", "duck", "elk", "fawn", "finch", "hawk", "kite", "lion"];

/// Returns a slug like `"brave-tiger-42"`. Uses the current timestamp as an
/// entropy source to avoid adding a new crate dependency.
#[must_use]
pub fn generate_word_slug() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    let adj = ADJECTIVES[now % ADJECTIVES.len()];
    let animal = ANIMALS[(now / 13) % ANIMALS.len()];
    let num = (now / 997) % 90 + 10; // always 2 digits: 10-99
    format!("{adj}-{animal}-{num}")
}

/// Returns `provided` unchanged if no team file exists for it; otherwise
/// generates a unique slug.
#[must_use]
pub fn generate_unique_team_name(provided: &str, teams_dir: &Path) -> String {
    if !get_team_file_path(provided, teams_dir).exists() {
        return provided.to_string();
    }
    generate_word_slug()
}

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

#[must_use]
pub fn now_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
