//! Bridge mode: IDE integration protocol for two-way communication.
//!
//! Mirrors the Claude Code TypeScript `bridge/` module.
//! Provides JSON-RPC-style messaging over stdio or TCP for IDE extensions.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

/// A JSON-RPC-style request from the IDE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A JSON-RPC-style response to the IDE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeError {
    pub code: i32,
    pub message: String,
}

/// A notification pushed from Emberforge to the IDE (no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeNotification {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Bridge state
// ---------------------------------------------------------------------------

/// The bridge mode state machine.
#[derive(Debug)]
pub struct BridgeState {
    pub active: bool,
    pub connected_ide: Option<String>,
    handlers: BTreeMap<String, BridgeHandler>,
}

type BridgeHandler = fn(&serde_json::Value) -> Result<serde_json::Value, String>;

impl BridgeState {
    /// Create a new bridge with built-in method handlers.
    #[must_use]
    pub fn new() -> Self {
        let mut handlers: BTreeMap<String, BridgeHandler> = BTreeMap::new();
        handlers.insert("ping".to_string(), handle_ping);
        handlers.insert("getStatus".to_string(), handle_get_status);
        handlers.insert("getCapabilities".to_string(), handle_get_capabilities);
        handlers.insert("openFile".to_string(), handle_open_file);
        handlers.insert("getSelection".to_string(), handle_get_selection);

        Self {
            active: false,
            connected_ide: None,
            handlers,
        }
    }

    /// Start the bridge (mark as active).
    pub fn start(&mut self, ide_name: Option<String>) {
        self.active = true;
        self.connected_ide = ide_name;
    }

    /// Stop the bridge.
    pub fn stop(&mut self) {
        self.active = false;
        self.connected_ide = None;
    }

    /// Dispatch a request to the appropriate handler.
    pub fn handle_request(&self, request: &BridgeRequest) -> BridgeResponse {
        if let Some(handler) = self.handlers.get(&request.method) {
            match handler(&request.params) {
                Ok(result) => BridgeResponse {
                    id: request.id,
                    result: Some(result),
                    error: None,
                },
                Err(message) => BridgeResponse {
                    id: request.id,
                    result: None,
                    error: Some(BridgeError { code: -1, message }),
                },
            }
        } else {
            BridgeResponse {
                id: request.id,
                result: None,
                error: Some(BridgeError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                }),
            }
        }
    }

    /// Run a stdio-based bridge loop (blocking).
    pub fn run_stdio(&self) -> io::Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut out = stdout.lock();

        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let request: BridgeRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let error_response = BridgeResponse {
                        id: 0,
                        result: None,
                        error: Some(BridgeError {
                            code: -32700,
                            message: format!("Parse error: {e}"),
                        }),
                    };
                    serde_json::to_writer(&mut out, &error_response)?;
                    writeln!(out)?;
                    out.flush()?;
                    continue;
                }
            };

            let response = self.handle_request(&request);
            serde_json::to_writer(&mut out, &response)?;
            writeln!(out)?;
            out.flush()?;

            // Exit on shutdown method
            if request.method == "shutdown" {
                break;
            }
        }

        Ok(())
    }
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in handlers
// ---------------------------------------------------------------------------

fn handle_ping(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "pong": true }))
}

fn handle_get_status(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "status": "active",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

fn handle_get_capabilities(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "capabilities": [
            "openFile",
            "getSelection",
            "ping",
            "getStatus",
            "getCapabilities",
        ]
    }))
}

fn handle_open_file(params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'path' parameter".to_string())?;

    if !std::path::Path::new(path).exists() {
        return Err(format!("File not found: {path}"));
    }

    Ok(serde_json::json!({
        "opened": true,
        "path": path,
    }))
}

fn handle_get_selection(_params: &serde_json::Value) -> Result<serde_json::Value, String> {
    // Placeholder: real implementation would query the IDE
    Ok(serde_json::json!({
        "selection": null,
        "message": "No IDE connected for selection query"
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_handler() {
        let bridge = BridgeState::new();
        let req = BridgeRequest {
            id: 1,
            method: "ping".to_string(),
            params: serde_json::Value::Null,
        };
        let resp = bridge.handle_request(&req);
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn unknown_method() {
        let bridge = BridgeState::new();
        let req = BridgeRequest {
            id: 2,
            method: "nonexistent".to_string(),
            params: serde_json::Value::Null,
        };
        let resp = bridge.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn start_stop() {
        let mut bridge = BridgeState::new();
        assert!(!bridge.active);
        bridge.start(Some("vscode".into()));
        assert!(bridge.active);
        assert_eq!(bridge.connected_ide.as_deref(), Some("vscode"));
        bridge.stop();
        assert!(!bridge.active);
    }
}
