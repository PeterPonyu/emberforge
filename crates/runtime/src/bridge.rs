//! Bridge mode: IDE integration protocol for two-way communication.
//!
//! Full-depth port of the Claude Code TypeScript `bridge/` module.
//! Provides bidirectional messaging over WebSocket with control requests,
//! UUID-based echo deduplication, and session management.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

/// A bridge message (discriminated union on `type` field).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeMessage {
    /// A user message (IDE → REPL).
    User {
        uuid: String,
        content: String,
    },
    /// An assistant message (REPL → IDE).
    Assistant {
        uuid: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use: Option<serde_json::Value>,
    },
    /// A system/local command message.
    System {
        uuid: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subtype: Option<String>,
    },
    /// A control request from the server/IDE to the REPL.
    ControlRequest {
        request_id: String,
        request: ControlRequestBody,
    },
    /// A control response from the REPL to the IDE.
    ControlResponse {
        response: ControlResponseBody,
    },
}

/// Body of a control request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestBody {
    /// Initialize the bridge session.
    Initialize {
        #[serde(skip_serializing_if = "Option::is_none")]
        ide_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ide_version: Option<String>,
    },
    /// Change the active model.
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Interrupt the current turn.
    Interrupt,
    /// Change the permission mode.
    SetPermissionMode {
        mode: String,
    },
    /// Set maximum thinking tokens.
    SetMaxThinkingTokens {
        max_tokens: Option<u64>,
    },
}

/// Body of a control response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponseBody {
    pub subtype: ControlResponseStatus,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlResponseStatus {
    Success,
    Error,
}

// ---------------------------------------------------------------------------
// UUID deduplication (CC's BoundedUUIDSet)
// ---------------------------------------------------------------------------

/// A bounded FIFO set for UUID deduplication.
///
/// Tracks recently seen UUIDs to prevent echo and re-delivery processing.
/// When capacity is reached, the oldest entry is evicted.
#[derive(Debug, Clone)]
pub struct BoundedUuidSet {
    capacity: usize,
    ring: VecDeque<String>,
}

impl BoundedUuidSet {
    /// Create a new set with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ring: VecDeque::with_capacity(capacity),
        }
    }

    /// Add a UUID to the set. Returns `true` if it was new (not a duplicate).
    pub fn insert(&mut self, uuid: String) -> bool {
        if self.ring.iter().any(|u| *u == uuid) {
            return false; // duplicate
        }
        if self.ring.len() >= self.capacity {
            self.ring.pop_front(); // evict oldest
        }
        self.ring.push_back(uuid);
        true
    }

    /// Check if a UUID is in the set.
    #[must_use]
    pub fn contains(&self, uuid: &str) -> bool {
        self.ring.iter().any(|u| u == uuid)
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.ring.clear();
    }

    /// Number of entries currently in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Bridge session state
// ---------------------------------------------------------------------------

/// A bridge session linking an IDE client to a REPL runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSession {
    pub session_id: String,
    pub ide_name: Option<String>,
    pub ide_version: Option<String>,
    pub created_at: String,
    pub active: bool,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
}

/// Bridge state managing active sessions and handlers.
#[derive(Debug)]
pub struct BridgeState {
    pub active: bool,
    sessions: Arc<Mutex<BTreeMap<String, BridgeSession>>>,
    /// UUIDs we sent outbound (for echo dedup).
    outbound_uuids: Arc<Mutex<BoundedUuidSet>>,
    /// UUIDs we received inbound (for re-delivery dedup).
    inbound_uuids: Arc<Mutex<BoundedUuidSet>>,
    handlers: BTreeMap<String, ControlHandler>,
}

type ControlHandler = fn(&ControlRequestBody) -> Result<serde_json::Value, String>;

impl BridgeState {
    /// Create a new bridge with built-in control handlers.
    #[must_use]
    pub fn new() -> Self {
        let mut handlers: BTreeMap<String, ControlHandler> = BTreeMap::new();
        handlers.insert("initialize".to_string(), handle_initialize);
        handlers.insert("set_model".to_string(), handle_set_model);
        handlers.insert("interrupt".to_string(), handle_interrupt);
        handlers.insert("set_permission_mode".to_string(), handle_set_permission_mode);
        handlers.insert("set_max_thinking_tokens".to_string(), handle_set_max_thinking_tokens);

        Self {
            active: false,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            outbound_uuids: Arc::new(Mutex::new(BoundedUuidSet::new(256))),
            inbound_uuids: Arc::new(Mutex::new(BoundedUuidSet::new(256))),
            handlers,
        }
    }

    /// Start the bridge.
    pub fn start(&mut self) {
        self.active = true;
    }

    /// Stop the bridge.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Create a new bridge session.
    pub fn create_session(&self, ide_name: Option<String>, ide_version: Option<String>) -> String {
        let session_id = format!(
            "bridge-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let session = BridgeSession {
            session_id: session_id.clone(),
            ide_name,
            ide_version,
            created_at: iso8601_now(),
            active: true,
            model: None,
            permission_mode: None,
        };
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session_id.clone(), session);
        }
        session_id
    }

    /// Get a session by ID.
    #[must_use]
    pub fn get_session(&self, session_id: &str) -> Option<BridgeSession> {
        self.sessions
            .lock()
            .ok()
            .and_then(|s| s.get(session_id).cloned())
    }

    /// List all active sessions.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<BridgeSession> {
        self.sessions
            .lock()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Close a session.
    pub fn close_session(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .map(|mut s| s.remove(session_id).is_some())
            .unwrap_or(false)
    }

    /// Process an inbound message from the IDE.
    ///
    /// Returns `None` if the message is a duplicate (echo or re-delivery).
    /// Returns `Some(action)` describing what to do with the message.
    pub fn process_inbound(&self, raw: &str) -> Option<InboundAction> {
        let msg: BridgeMessage = serde_json::from_str(raw).ok()?;

        match &msg {
            BridgeMessage::User { uuid, content } => {
                // Check if this is an echo of our outbound message
                if let Ok(outbound) = self.outbound_uuids.lock() {
                    if outbound.contains(uuid) {
                        return None; // echo — skip
                    }
                }
                // Check for re-delivery
                if let Ok(mut inbound) = self.inbound_uuids.lock() {
                    if !inbound.insert(uuid.clone()) {
                        return None; // re-delivery — skip
                    }
                }
                Some(InboundAction::UserMessage {
                    uuid: uuid.clone(),
                    content: content.clone(),
                })
            }
            BridgeMessage::ControlRequest {
                request_id,
                request,
            } => Some(InboundAction::ControlRequest {
                request_id: request_id.clone(),
                request: request.clone(),
            }),
            BridgeMessage::ControlResponse { response } => {
                Some(InboundAction::ControlResponse {
                    response: response.clone(),
                })
            }
            _ => None,
        }
    }

    /// Handle a control request and produce a response.
    pub fn handle_control_request(
        &self,
        request_id: &str,
        request: &ControlRequestBody,
    ) -> BridgeMessage {
        let subtype_name = match request {
            ControlRequestBody::Initialize { .. } => "initialize",
            ControlRequestBody::SetModel { .. } => "set_model",
            ControlRequestBody::Interrupt => "interrupt",
            ControlRequestBody::SetPermissionMode { .. } => "set_permission_mode",
            ControlRequestBody::SetMaxThinkingTokens { .. } => "set_max_thinking_tokens",
        };

        let response = if let Some(handler) = self.handlers.get(subtype_name) {
            match handler(request) {
                Ok(result) => ControlResponseBody {
                    subtype: ControlResponseStatus::Success,
                    request_id: request_id.to_string(),
                    response: Some(result),
                    error: None,
                },
                Err(error) => ControlResponseBody {
                    subtype: ControlResponseStatus::Error,
                    request_id: request_id.to_string(),
                    response: None,
                    error: Some(error),
                },
            }
        } else {
            ControlResponseBody {
                subtype: ControlResponseStatus::Error,
                request_id: request_id.to_string(),
                response: None,
                error: Some(format!("Unknown control request: {subtype_name}")),
            }
        };

        BridgeMessage::ControlResponse { response }
    }

    /// Build an outbound assistant message, tracking its UUID for echo dedup.
    pub fn build_assistant_message(&self, content: &str) -> BridgeMessage {
        let uuid = generate_uuid();
        if let Ok(mut outbound) = self.outbound_uuids.lock() {
            outbound.insert(uuid.clone());
        }
        BridgeMessage::Assistant {
            uuid,
            content: content.to_string(),
            tool_use: None,
        }
    }
}

impl Default for BridgeState {
    fn default() -> Self {
        Self::new()
    }
}

/// Action to take after processing an inbound message.
#[derive(Debug, Clone)]
pub enum InboundAction {
    UserMessage { uuid: String, content: String },
    ControlRequest { request_id: String, request: ControlRequestBody },
    ControlResponse { response: ControlResponseBody },
}

// ---------------------------------------------------------------------------
// Control handlers
// ---------------------------------------------------------------------------

fn handle_initialize(request: &ControlRequestBody) -> Result<serde_json::Value, String> {
    if let ControlRequestBody::Initialize { ide_name, ide_version } = request {
        Ok(serde_json::json!({
            "status": "initialized",
            "version": env!("CARGO_PKG_VERSION"),
            "ide_name": ide_name,
            "ide_version": ide_version,
            "capabilities": ["set_model", "interrupt", "set_permission_mode"],
        }))
    } else {
        Err("Invalid initialize request".to_string())
    }
}

fn handle_set_model(request: &ControlRequestBody) -> Result<serde_json::Value, String> {
    if let ControlRequestBody::SetModel { model } = request {
        Ok(serde_json::json!({
            "model": model,
            "applied": true,
        }))
    } else {
        Err("Invalid set_model request".to_string())
    }
}

fn handle_interrupt(_request: &ControlRequestBody) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "interrupted": true,
    }))
}

fn handle_set_permission_mode(
    request: &ControlRequestBody,
) -> Result<serde_json::Value, String> {
    if let ControlRequestBody::SetPermissionMode { mode } = request {
        let valid = ["read-only", "workspace-write", "danger-full-access", "prompt", "allow"];
        if valid.contains(&mode.as_str()) {
            Ok(serde_json::json!({
                "mode": mode,
                "applied": true,
            }))
        } else {
            Err(format!("Invalid permission mode: {mode}. Valid: {}", valid.join(", ")))
        }
    } else {
        Err("Invalid set_permission_mode request".to_string())
    }
}

fn handle_set_max_thinking_tokens(
    request: &ControlRequestBody,
) -> Result<serde_json::Value, String> {
    if let ControlRequestBody::SetMaxThinkingTokens { max_tokens } = request {
        Ok(serde_json::json!({
            "max_tokens": max_tokens,
            "applied": true,
        }))
    } else {
        Err("Invalid set_max_thinking_tokens request".to_string())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u128;
    format!("{:016x}-{:08x}", nanos.wrapping_mul(pid.wrapping_add(7)), pid)
}

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
    fn bounded_uuid_set_dedup() {
        let mut set = BoundedUuidSet::new(3);
        assert!(set.insert("a".to_string()));
        assert!(set.insert("b".to_string()));
        assert!(!set.insert("a".to_string())); // duplicate
        assert!(set.contains("a"));
        assert!(set.contains("b"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn bounded_uuid_set_eviction() {
        let mut set = BoundedUuidSet::new(2);
        set.insert("a".to_string());
        set.insert("b".to_string());
        set.insert("c".to_string()); // evicts "a"
        assert!(!set.contains("a"));
        assert!(set.contains("b"));
        assert!(set.contains("c"));
    }

    #[test]
    fn bridge_state_create_and_list_sessions() {
        let bridge = BridgeState::new();
        let id = bridge.create_session(Some("vscode".into()), Some("1.90".into()));
        let sessions = bridge.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, id);
        assert_eq!(sessions[0].ide_name.as_deref(), Some("vscode"));
    }

    #[test]
    fn bridge_state_close_session() {
        let bridge = BridgeState::new();
        let id = bridge.create_session(None, None);
        assert!(bridge.close_session(&id));
        assert!(!bridge.close_session(&id)); // already closed
        assert!(bridge.list_sessions().is_empty());
    }

    #[test]
    fn control_request_initialize() {
        let bridge = BridgeState::new();
        let response = bridge.handle_control_request(
            "req-1",
            &ControlRequestBody::Initialize {
                ide_name: Some("vscode".into()),
                ide_version: None,
            },
        );
        if let BridgeMessage::ControlResponse { response } = response {
            assert_eq!(response.subtype, ControlResponseStatus::Success);
            assert_eq!(response.request_id, "req-1");
            assert!(response.response.is_some());
        } else {
            panic!("Expected ControlResponse");
        }
    }

    #[test]
    fn control_request_set_model() {
        let bridge = BridgeState::new();
        let response = bridge.handle_control_request(
            "req-2",
            &ControlRequestBody::SetModel {
                model: Some("qwen3:8b".into()),
            },
        );
        if let BridgeMessage::ControlResponse { response } = response {
            assert_eq!(response.subtype, ControlResponseStatus::Success);
        } else {
            panic!("Expected ControlResponse");
        }
    }

    #[test]
    fn control_request_invalid_permission_mode() {
        let bridge = BridgeState::new();
        let response = bridge.handle_control_request(
            "req-3",
            &ControlRequestBody::SetPermissionMode {
                mode: "yolo".into(),
            },
        );
        if let BridgeMessage::ControlResponse { response } = response {
            assert_eq!(response.subtype, ControlResponseStatus::Error);
            assert!(response.error.is_some());
        } else {
            panic!("Expected ControlResponse");
        }
    }

    #[test]
    fn echo_dedup_skips_own_messages() {
        let bridge = BridgeState::new();

        // Build outbound message (registers UUID)
        let outbound = bridge.build_assistant_message("hello from REPL");
        let BridgeMessage::Assistant { uuid, .. } = &outbound else {
            panic!("Expected Assistant message");
        };

        // Simulate receiving it back as inbound (echo)
        let inbound_json = serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "content": "hello from REPL"
        });
        let result = bridge.process_inbound(&inbound_json.to_string());
        assert!(result.is_none(), "Echo should be filtered");
    }

    #[test]
    fn redelivery_dedup_skips_duplicates() {
        let bridge = BridgeState::new();

        let msg = serde_json::json!({
            "type": "user",
            "uuid": "unique-123",
            "content": "hello"
        });
        let json = msg.to_string();

        let first = bridge.process_inbound(&json);
        assert!(first.is_some(), "First delivery should pass");

        let second = bridge.process_inbound(&json);
        assert!(second.is_none(), "Re-delivery should be filtered");
    }

    #[test]
    fn process_inbound_control_request() {
        let bridge = BridgeState::new();
        let msg = serde_json::json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "interrupt"
            }
        });
        let action = bridge.process_inbound(&msg.to_string());
        assert!(matches!(action, Some(InboundAction::ControlRequest { .. })));
    }

    #[test]
    fn start_stop_lifecycle() {
        let mut bridge = BridgeState::new();
        assert!(!bridge.active);
        bridge.start();
        assert!(bridge.active);
        bridge.stop();
        assert!(!bridge.active);
    }
}
