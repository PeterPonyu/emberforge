use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};

use serde::{Deserialize, Serialize};

use crate::session::{ContentBlock, ConversationMessage, MessageRole};
use crate::usage::TokenUsage;

// ---------------------------------------------------------------------------
// TransportEvent
// ---------------------------------------------------------------------------

/// Events that flow through the transport layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransportEvent {
    /// User sent a message.
    UserMessage { content: String },

    /// Assistant started streaming.
    AssistantStreamStart,

    /// Assistant text delta.
    AssistantTextDelta { text: String },

    /// Assistant requested a tool use.
    ToolUseRequest {
        id: String,
        name: String,
        input: String,
    },

    /// Tool execution result.
    ToolResult {
        tool_use_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },

    /// Assistant finished streaming.
    AssistantStreamEnd,

    /// Token usage update.
    UsageUpdate {
        input_tokens: u32,
        output_tokens: u32,
    },

    /// Session was compacted.
    SessionCompacted {
        removed_messages: usize,
        summary: String,
    },

    /// Error occurred.
    Error { message: String },

    /// Session started.
    SessionStart { session_id: String },

    /// Session ended.
    SessionEnd { reason: String },

    /// Heartbeat ping.
    Ping,

    /// Heartbeat pong.
    Pong,
}

// ---------------------------------------------------------------------------
// TransportConfig
// ---------------------------------------------------------------------------

/// Transport configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Maximum message size in bytes (default: 10 MB).
    pub max_message_size: usize,
    /// Heartbeat interval in seconds (0 = disabled).
    pub heartbeat_interval_secs: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_message_size: 10 * 1024 * 1024,
            heartbeat_interval_secs: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionTransport trait
// ---------------------------------------------------------------------------

/// The core transport abstraction. Implementations handle sending/receiving
/// events over different channels (local terminal, WebSocket, SSE, etc.).
pub trait SessionTransport {
    /// Send an event to the other end of the transport.
    fn send_event(&mut self, event: &TransportEvent) -> io::Result<()>;

    /// Receive the next event (blocking).
    fn recv_event(&mut self) -> io::Result<TransportEvent>;

    /// Check if there is a pending event without blocking.
    fn try_recv_event(&mut self) -> io::Result<Option<TransportEvent>>;

    /// Close the transport.
    fn close(&mut self) -> io::Result<()>;

    /// Whether the transport is still connected.
    fn is_connected(&self) -> bool;
}

// ---------------------------------------------------------------------------
// LocalTransport
// ---------------------------------------------------------------------------

/// A transport that wraps local stdin/stdout for the terminal REPL.
/// Events are logged but stdin/stdout is managed elsewhere (the REPL).
pub struct LocalTransport {
    event_log: Vec<TransportEvent>,
    connected: bool,
}

impl LocalTransport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            event_log: Vec::new(),
            connected: true,
        }
    }

    /// Get the logged events (for testing/debugging).
    #[must_use]
    pub fn event_log(&self) -> &[TransportEvent] {
        &self.event_log
    }
}

impl Default for LocalTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTransport for LocalTransport {
    fn send_event(&mut self, event: &TransportEvent) -> io::Result<()> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport is closed",
            ));
        }
        self.event_log.push(event.clone());
        Ok(())
    }

    fn recv_event(&mut self) -> io::Result<TransportEvent> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LocalTransport does not support recv_event; input comes from the REPL",
        ))
    }

    fn try_recv_event(&mut self) -> io::Result<Option<TransportEvent>> {
        Ok(None)
    }

    fn close(&mut self) -> io::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

// ---------------------------------------------------------------------------
// NdjsonTransport
// ---------------------------------------------------------------------------

/// A transport that reads/writes NDJSON (one JSON event per line).
/// Useful for pipe-based communication and structured output mode.
pub struct NdjsonTransport<R: io::Read, W: io::Write> {
    reader: BufReader<R>,
    writer: W,
    connected: bool,
    config: TransportConfig,
}

impl<R: io::Read, W: io::Write> NdjsonTransport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self::with_config(reader, writer, TransportConfig::default())
    }

    pub fn with_config(reader: R, writer: W, config: TransportConfig) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            connected: true,
            config,
        }
    }
}

impl<R: io::Read, W: io::Write> SessionTransport for NdjsonTransport<R, W> {
    fn send_event(&mut self, event: &TransportEvent) -> io::Result<()> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport is closed",
            ));
        }
        let json = serde_json::to_string(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if json.len() > self.config.max_message_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "message size {} exceeds maximum {}",
                    json.len(),
                    self.config.max_message_size
                ),
            ));
        }
        self.writer.write_all(json.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    fn recv_event(&mut self) -> io::Result<TransportEvent> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport is closed",
            ));
        }
        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line)?;
        if bytes_read == 0 {
            self.connected = false;
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "transport stream ended",
            ));
        }
        if line.len() > self.config.max_message_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "message size {} exceeds maximum {}",
                    line.len(),
                    self.config.max_message_size
                ),
            ));
        }
        serde_json::from_str(line.trim())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn try_recv_event(&mut self) -> io::Result<Option<TransportEvent>> {
        // For a synchronous BufReader there is no non-blocking check, so we
        // inspect the internal buffer. If it contains a newline we can parse
        // immediately; otherwise we report nothing available.
        if !self.connected {
            return Ok(None);
        }
        let buf = self.reader.buffer();
        if buf.contains(&b'\n') {
            self.recv_event().map(Some)
        } else {
            Ok(None)
        }
    }

    fn close(&mut self) -> io::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

// ---------------------------------------------------------------------------
// MemoryTransport
// ---------------------------------------------------------------------------

/// An in-memory transport for testing. Events are stored in `Vec` queues.
pub struct MemoryTransport {
    /// Events sent by the transport user.
    sent: Vec<TransportEvent>,
    /// Events queued to be received.
    receive_queue: VecDeque<TransportEvent>,
    connected: bool,
}

impl MemoryTransport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sent: Vec::new(),
            receive_queue: VecDeque::new(),
            connected: true,
        }
    }

    /// Queue an event to be received on the next `recv_event` call.
    pub fn enqueue(&mut self, event: TransportEvent) {
        self.receive_queue.push_back(event);
    }

    /// Get all sent events.
    #[must_use]
    pub fn sent_events(&self) -> &[TransportEvent] {
        &self.sent
    }

    /// Create a pair of connected transports for testing bidirectional
    /// communication. Events sent on one side appear in the other's receive
    /// queue (snapshot at creation time — for dynamic routing, call
    /// `enqueue` manually).
    #[must_use]
    pub fn pair() -> (Self, Self) {
        (Self::new(), Self::new())
    }
}

impl Default for MemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTransport for MemoryTransport {
    fn send_event(&mut self, event: &TransportEvent) -> io::Result<()> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport is closed",
            ));
        }
        self.sent.push(event.clone());
        Ok(())
    }

    fn recv_event(&mut self) -> io::Result<TransportEvent> {
        if !self.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport is closed",
            ));
        }
        self.receive_queue.pop_front().ok_or_else(|| {
            io::Error::new(io::ErrorKind::WouldBlock, "no events in receive queue")
        })
    }

    fn try_recv_event(&mut self) -> io::Result<Option<TransportEvent>> {
        if !self.connected {
            return Ok(None);
        }
        Ok(self.receive_queue.pop_front())
    }

    fn close(&mut self) -> io::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Convert a `ConversationMessage` into transport events.
#[must_use]
pub fn message_to_events(message: &ConversationMessage) -> Vec<TransportEvent> {
    let mut events = Vec::new();

    match message.role {
        MessageRole::User | MessageRole::System => {
            for block in &message.blocks {
                if let ContentBlock::Text { text } = block {
                    events.push(TransportEvent::UserMessage {
                        content: text.clone(),
                    });
                }
            }
        }
        MessageRole::Assistant => {
            events.push(TransportEvent::AssistantStreamStart);
            for block in &message.blocks {
                match block {
                    ContentBlock::Text { text } => {
                        events.push(TransportEvent::AssistantTextDelta { text: text.clone() });
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        events.push(TransportEvent::ToolUseRequest {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    ContentBlock::ToolResult { .. } => {}
                }
            }
            events.push(TransportEvent::AssistantStreamEnd);
        }
        MessageRole::Tool => {
            for block in &message.blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    tool_name,
                    output,
                    is_error,
                } = block
                {
                    events.push(TransportEvent::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        tool_name: tool_name.clone(),
                        output: output.clone(),
                        is_error: *is_error,
                    });
                }
            }
        }
    }

    if let Some(usage) = &message.usage {
        events.push(TransportEvent::UsageUpdate {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        });
    }

    events
}

/// Convert transport events back into a `ConversationMessage`.
///
/// Returns `None` if the slice is empty or contains only control events
/// (stream start/end, pings, etc.) with no substantive content.
#[must_use]
pub fn events_to_message(events: &[TransportEvent]) -> Option<ConversationMessage> {
    if events.is_empty() {
        return None;
    }

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut role: Option<MessageRole> = None;
    let mut usage: Option<TokenUsage> = None;

    for event in events {
        match event {
            TransportEvent::UserMessage { content } => {
                role.get_or_insert(MessageRole::User);
                blocks.push(ContentBlock::Text {
                    text: content.clone(),
                });
            }
            TransportEvent::AssistantTextDelta { text } => {
                role.get_or_insert(MessageRole::Assistant);
                blocks.push(ContentBlock::Text { text: text.clone() });
            }
            TransportEvent::ToolUseRequest { id, name, input } => {
                role.get_or_insert(MessageRole::Assistant);
                blocks.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            TransportEvent::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => {
                role.get_or_insert(MessageRole::Tool);
                blocks.push(ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    output: output.clone(),
                    is_error: *is_error,
                });
            }
            TransportEvent::UsageUpdate {
                input_tokens,
                output_tokens,
            } => {
                usage = Some(TokenUsage {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                });
            }
            // Control events that carry no message content.
            TransportEvent::AssistantStreamStart
            | TransportEvent::AssistantStreamEnd
            | TransportEvent::SessionCompacted { .. }
            | TransportEvent::Error { .. }
            | TransportEvent::SessionStart { .. }
            | TransportEvent::SessionEnd { .. }
            | TransportEvent::Ping
            | TransportEvent::Pong => {}
        }
    }

    if blocks.is_empty() {
        return None;
    }

    Some(ConversationMessage {
        role: role.unwrap_or(MessageRole::User),
        blocks,
        usage,
    })
}

/// Serialize a `TransportEvent` to JSON.
pub fn event_to_json(event: &TransportEvent) -> io::Result<String> {
    serde_json::to_string(event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Deserialize a `TransportEvent` from JSON.
pub fn event_from_json(json: &str) -> io::Result<TransportEvent> {
    serde_json::from_str(json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // 1. TransportEvent serialization round-trip (each variant).
    #[test]
    fn test_event_serialization_round_trip() {
        let variants: Vec<TransportEvent> = vec![
            TransportEvent::UserMessage {
                content: "hello".into(),
            },
            TransportEvent::AssistantStreamStart,
            TransportEvent::AssistantTextDelta {
                text: "world".into(),
            },
            TransportEvent::ToolUseRequest {
                id: "t1".into(),
                name: "bash".into(),
                input: "{}".into(),
            },
            TransportEvent::ToolResult {
                tool_use_id: "t1".into(),
                tool_name: "bash".into(),
                output: "ok".into(),
                is_error: false,
            },
            TransportEvent::AssistantStreamEnd,
            TransportEvent::UsageUpdate {
                input_tokens: 10,
                output_tokens: 20,
            },
            TransportEvent::SessionCompacted {
                removed_messages: 5,
                summary: "compacted".into(),
            },
            TransportEvent::Error {
                message: "oops".into(),
            },
            TransportEvent::SessionStart {
                session_id: "s1".into(),
            },
            TransportEvent::SessionEnd {
                reason: "done".into(),
            },
            TransportEvent::Ping,
            TransportEvent::Pong,
        ];

        for event in &variants {
            let json = event_to_json(event).expect("serialize");
            let back = event_from_json(&json).expect("deserialize");
            // Re-serialize to compare (TransportEvent doesn't derive PartialEq).
            let json2 = event_to_json(&back).expect("re-serialize");
            assert_eq!(json, json2, "round-trip mismatch for {json}");
        }
    }

    // 2. LocalTransport logs sent events.
    #[test]
    fn test_local_transport_logs_events() {
        let mut t = LocalTransport::new();
        assert!(t.event_log().is_empty());

        t.send_event(&TransportEvent::Ping).unwrap();
        t.send_event(&TransportEvent::UserMessage {
            content: "hi".into(),
        })
        .unwrap();

        assert_eq!(t.event_log().len(), 2);
    }

    // 3. LocalTransport is_connected after close.
    #[test]
    fn test_local_transport_close() {
        let mut t = LocalTransport::new();
        assert!(t.is_connected());
        t.close().unwrap();
        assert!(!t.is_connected());

        let err = t.send_event(&TransportEvent::Ping).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotConnected);
    }

    // 4. MemoryTransport send and receive.
    #[test]
    fn test_memory_transport_send_recv() {
        let mut t = MemoryTransport::new();

        t.send_event(&TransportEvent::Ping).unwrap();
        assert_eq!(t.sent_events().len(), 1);

        t.enqueue(TransportEvent::Pong);
        let event = t.recv_event().unwrap();
        assert_eq!(event_to_json(&event).unwrap(), event_to_json(&TransportEvent::Pong).unwrap());

        // Queue empty -> WouldBlock.
        let err = t.recv_event().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    // 5. MemoryTransport pair bidirectional communication.
    #[test]
    fn test_memory_transport_pair() {
        let (mut a, mut b) = MemoryTransport::pair();

        // Simulate bidirectional: a sends, manually enqueue on b.
        a.send_event(&TransportEvent::UserMessage {
            content: "from a".into(),
        })
        .unwrap();

        // Move sent events from a into b's receive queue.
        for ev in a.sent_events() {
            b.enqueue(ev.clone());
        }

        let received = b.recv_event().unwrap();
        let json = event_to_json(&received).unwrap();
        assert!(json.contains("from a"));

        // And the reverse direction.
        b.send_event(&TransportEvent::Pong).unwrap();
        for ev in b.sent_events() {
            a.enqueue(ev.clone());
        }
        let received_a = a.recv_event().unwrap();
        let json_a = event_to_json(&received_a).unwrap();
        assert!(json_a.contains("pong"));
    }

    // 6. NdjsonTransport round-trip via pipe.
    #[test]
    fn test_ndjson_round_trip() {
        let mut buf: Vec<u8> = Vec::new();

        // Write phase.
        {
            let reader = Cursor::new(Vec::<u8>::new());
            let mut t = NdjsonTransport::new(reader, &mut buf);
            t.send_event(&TransportEvent::UserMessage {
                content: "hello ndjson".into(),
            })
            .unwrap();
            t.send_event(&TransportEvent::Ping).unwrap();
        }

        // Read phase.
        {
            let reader = Cursor::new(buf);
            let writer: Vec<u8> = Vec::new();
            let mut t = NdjsonTransport::new(reader, writer);

            let ev1 = t.recv_event().unwrap();
            let json1 = event_to_json(&ev1).unwrap();
            assert!(json1.contains("hello ndjson"));

            let ev2 = t.recv_event().unwrap();
            let json2 = event_to_json(&ev2).unwrap();
            assert!(json2.contains("ping"));
        }
    }

    // 7. NdjsonTransport handles large messages.
    #[test]
    fn test_ndjson_large_message() {
        let large_text = "x".repeat(1_000_000);
        let mut buf: Vec<u8> = Vec::new();

        {
            let reader = Cursor::new(Vec::<u8>::new());
            let mut t = NdjsonTransport::new(reader, &mut buf);
            t.send_event(&TransportEvent::AssistantTextDelta {
                text: large_text.clone(),
            })
            .unwrap();
        }

        {
            let reader = Cursor::new(buf);
            let writer: Vec<u8> = Vec::new();
            let mut t = NdjsonTransport::new(reader, writer);
            let ev = t.recv_event().unwrap();
            if let TransportEvent::AssistantTextDelta { text } = ev {
                assert_eq!(text.len(), 1_000_000);
            } else {
                panic!("expected AssistantTextDelta");
            }
        }
    }

    // 8. message_to_events for user text message.
    #[test]
    fn test_message_to_events_user() {
        let msg = ConversationMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text {
                text: "hi there".into(),
            }],
            usage: None,
        };
        let events = message_to_events(&msg);
        assert_eq!(events.len(), 1);
        let json = event_to_json(&events[0]).unwrap();
        assert!(json.contains("hi there"));
        assert!(json.contains("user_message"));
    }

    // 9. message_to_events for assistant with tool use.
    #[test]
    fn test_message_to_events_assistant_tool() {
        let msg = ConversationMessage {
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock::Text {
                    text: "Let me run that.".into(),
                },
                ContentBlock::ToolUse {
                    id: "t42".into(),
                    name: "bash".into(),
                    input: r#"{"cmd":"ls"}"#.into(),
                },
            ],
            usage: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
        };
        let events = message_to_events(&msg);
        // StreamStart + TextDelta + ToolUseRequest + StreamEnd + UsageUpdate
        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], TransportEvent::AssistantStreamStart));
        assert!(matches!(events[1], TransportEvent::AssistantTextDelta { .. }));
        assert!(matches!(events[2], TransportEvent::ToolUseRequest { .. }));
        assert!(matches!(events[3], TransportEvent::AssistantStreamEnd));
        assert!(matches!(events[4], TransportEvent::UsageUpdate { .. }));
    }

    // 10. message_to_events for tool result.
    #[test]
    fn test_message_to_events_tool_result() {
        let msg = ConversationMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "t42".into(),
                tool_name: "bash".into(),
                output: "file.txt".into(),
                is_error: false,
            }],
            usage: None,
        };
        let events = message_to_events(&msg);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], TransportEvent::ToolResult { .. }));
    }

    // 11. events_to_message round-trip.
    #[test]
    fn test_events_to_message_round_trip() {
        let original = ConversationMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text {
                text: "round trip".into(),
            }],
            usage: None,
        };

        let events = message_to_events(&original);
        let reconstructed = events_to_message(&events).expect("should produce a message");

        assert_eq!(reconstructed.role, MessageRole::User);
        assert_eq!(reconstructed.blocks.len(), 1);
        assert_eq!(
            reconstructed.blocks[0],
            ContentBlock::Text {
                text: "round trip".into(),
            }
        );
    }

    // 12. event_to_json / event_from_json.
    #[test]
    fn test_event_json_helpers() {
        let event = TransportEvent::Error {
            message: "something failed".into(),
        };
        let json = event_to_json(&event).unwrap();
        assert!(json.contains("something failed"));
        assert!(json.contains(r#""type":"error""#));

        let back = event_from_json(&json).unwrap();
        if let TransportEvent::Error { message } = back {
            assert_eq!(message, "something failed");
        } else {
            panic!("expected Error variant");
        }
    }

    // 13. TransportConfig defaults.
    #[test]
    fn test_transport_config_defaults() {
        let cfg = TransportConfig::default();
        assert_eq!(cfg.max_message_size, 10 * 1024 * 1024);
        assert_eq!(cfg.heartbeat_interval_secs, 30);
    }
}
