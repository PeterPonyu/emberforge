//! Session teleport: export and import sessions for cross-machine migration.
//!
//! Mirrors the Claude Code TypeScript `utils/teleport` module.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Session;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A portable session bundle that can be serialized to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeleportBundle {
    /// Format version for forward compatibility.
    pub version: u32,
    /// The serialized session state.
    pub session: Session,
    /// Hostname of the machine that exported the bundle.
    pub source_host: String,
    /// ISO 8601 timestamp of export.
    pub exported_at: String,
    /// Optional human-readable title.
    pub title: Option<String>,
}

// ---------------------------------------------------------------------------
// Export / Import
// ---------------------------------------------------------------------------

/// Export a session to a teleport bundle file.
pub fn export_session(
    session: &Session,
    dest: &Path,
    title: Option<String>,
) -> io::Result<TeleportBundle> {
    let bundle = TeleportBundle {
        version: 1,
        session: session.clone(),
        source_host: hostname(),
        exported_at: iso8601_now(),
        title,
    };

    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, json)?;

    Ok(bundle)
}

/// Import a session from a teleport bundle file.
pub fn import_session(src: &Path) -> io::Result<TeleportBundle> {
    let json = fs::read_to_string(src)?;
    let bundle: TeleportBundle = serde_json::from_str(&json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(bundle)
}

/// Validate a bundle is compatible with this version.
pub fn validate_bundle(bundle: &TeleportBundle) -> Result<(), String> {
    if bundle.version > 1 {
        return Err(format!(
            "Bundle version {} is newer than supported (1). Please upgrade Emberforge.",
            bundle.version
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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
    fn roundtrip_teleport() {
        let session = Session::new();
        let dir = std::env::temp_dir().join("ember-teleport-test");
        let path = dir.join("bundle.json");

        let exported = export_session(&session, &path, Some("test".into())).unwrap();
        assert_eq!(exported.version, 1);
        assert_eq!(exported.title.as_deref(), Some("test"));

        let imported = import_session(&path).unwrap();
        validate_bundle(&imported).unwrap();
        assert_eq!(imported.session.messages.len(), session.messages.len());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
