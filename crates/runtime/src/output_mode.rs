//! Structured output modes for transport-safe, machine-readable output.
//!
//! Separates human-readable terminal chrome from machine-readable output
//! to enable clean JSON, NDJSON, and streaming event modes.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::session::{ContentBlock, ConversationMessage};
use crate::usage::TokenUsage;

// ── Output mode enum ───────────────────────────────────────────────────

/// Output mode controlling how the CLI renders responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// Human-readable terminal output with colors, spinners, markdown.
    Terminal,
    /// Single JSON object per response (no streaming).
    Json,
    /// Newline-delimited JSON events (streaming-friendly).
    Ndjson,
    /// Minimal text output (no chrome, no colors).
    Plain,
}

impl OutputMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Plain => "plain",
        }
    }

    /// Parse from a string (case-insensitive).
    #[must_use]
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "terminal" | "tty" => Some(Self::Terminal),
            "json" => Some(Self::Json),
            "ndjson" | "jsonl" => Some(Self::Ndjson),
            "plain" | "text" => Some(Self::Plain),
            _ => None,
        }
    }

    /// Whether this mode supports streaming (partial output before response is complete).
    #[must_use]
    pub fn is_streaming(self) -> bool {
        matches!(self, Self::Terminal | Self::Ndjson)
    }

    /// Whether this mode should suppress terminal chrome (colors, spinners, HUD).
    #[must_use]
    pub fn suppress_chrome(self) -> bool {
        !matches!(self, Self::Terminal)
    }
}

// ── Structured output events ───────────────────────────────────────────

/// A structured output event for JSON/NDJSON modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputEvent {
    /// Assistant text output.
    Text { content: String },

    /// Tool was invoked.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// Tool produced a result.
    ToolResult {
        tool_use_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },

    /// Token usage for this turn.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    },

    /// Turn completed.
    TurnEnd { iterations: usize },

    /// Error message.
    Error { message: String },

    /// System message (compaction notice, warning, etc.).
    System { message: String },
}

// ── Output writer ──────────────────────────────────────────────────────

/// Writes structured output events to a writer based on the active mode.
pub struct OutputWriter<W: Write> {
    writer: W,
    mode: OutputMode,
    event_buffer: Vec<OutputEvent>,
}

impl<W: Write> OutputWriter<W> {
    #[must_use]
    pub fn new(writer: W, mode: OutputMode) -> Self {
        Self {
            writer,
            mode,
            event_buffer: Vec::new(),
        }
    }

    /// Get the active output mode.
    #[must_use]
    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    /// Write a single event. In NDJSON mode, writes immediately.
    /// In JSON mode, buffers until `flush_json` is called.
    pub fn write_event(&mut self, event: OutputEvent) -> io::Result<()> {
        match self.mode {
            OutputMode::Ndjson => {
                let json = serde_json::to_string(&event)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                writeln!(self.writer, "{json}")?;
                self.writer.flush()
            }
            OutputMode::Json => {
                self.event_buffer.push(event);
                Ok(())
            }
            OutputMode::Plain => {
                if let OutputEvent::Text { content } = &event {
                    write!(self.writer, "{content}")?;
                    self.writer.flush()?;
                }
                Ok(())
            }
            OutputMode::Terminal => {
                // Terminal mode doesn't use the structured writer — output
                // goes through the terminal renderer instead.
                Ok(())
            }
        }
    }

    /// Flush buffered events as a single JSON array (for JSON mode).
    pub fn flush_json(&mut self) -> io::Result<()> {
        if self.mode != OutputMode::Json {
            return Ok(());
        }
        let json = serde_json::to_string_pretty(&self.event_buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{json}")?;
        self.event_buffer.clear();
        self.writer.flush()
    }

    /// Get a reference to buffered events (for JSON mode).
    #[must_use]
    pub fn buffered_events(&self) -> &[OutputEvent] {
        &self.event_buffer
    }
}

// ── Conversion helpers ─────────────────────────────────────────────────

/// Convert a conversation message to structured output events.
#[must_use]
pub fn message_to_output_events(message: &ConversationMessage) -> Vec<OutputEvent> {
    let mut events = Vec::new();
    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => {
                events.push(OutputEvent::Text {
                    content: text.clone(),
                });
            }
            ContentBlock::ToolUse { id, name, input } => {
                let input_value = serde_json::from_str(input)
                    .unwrap_or_else(|_| json!({ "raw": input }));
                events.push(OutputEvent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input_value,
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => {
                events.push(OutputEvent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    output: output.clone(),
                    is_error: *is_error,
                });
            }
        }
    }
    if let Some(usage) = message.usage {
        events.push(OutputEvent::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
        });
    }
    events
}

/// Build a complete JSON response from a turn's messages.
#[must_use]
pub fn build_json_response(
    assistant_messages: &[ConversationMessage],
    tool_results: &[ConversationMessage],
    usage: &TokenUsage,
    iterations: usize,
) -> serde_json::Value {
    let mut texts = Vec::new();
    let mut tool_uses = Vec::new();
    let mut results = Vec::new();

    for msg in assistant_messages {
        for block in &msg.blocks {
            match block {
                ContentBlock::Text { text } => texts.push(text.clone()),
                ContentBlock::ToolUse { id, name, input } => {
                    let input_val = serde_json::from_str(input)
                        .unwrap_or_else(|_| json!({ "raw": input }));
                    tool_uses.push(json!({
                        "id": id,
                        "name": name,
                        "input": input_val,
                    }));
                }
                ContentBlock::ToolResult { .. } => {}
            }
        }
    }

    for msg in tool_results {
        for block in &msg.blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } = block
            {
                results.push(json!({
                    "tool_use_id": tool_use_id,
                    "tool_name": tool_name,
                    "output": output,
                    "is_error": is_error,
                }));
            }
        }
    }

    json!({
        "text": texts.join("\n"),
        "tool_uses": tool_uses,
        "tool_results": results,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_tokens": usage.cache_read_input_tokens,
            "cache_creation_tokens": usage.cache_creation_input_tokens,
        },
        "iterations": iterations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ContentBlock, ConversationMessage};
    use crate::usage::TokenUsage;

    #[test]
    fn output_mode_parse_variants() {
        assert_eq!(OutputMode::from_str_loose("json"), Some(OutputMode::Json));
        assert_eq!(OutputMode::from_str_loose("NDJSON"), Some(OutputMode::Ndjson));
        assert_eq!(OutputMode::from_str_loose("jsonl"), Some(OutputMode::Ndjson));
        assert_eq!(OutputMode::from_str_loose("terminal"), Some(OutputMode::Terminal));
        assert_eq!(OutputMode::from_str_loose("tty"), Some(OutputMode::Terminal));
        assert_eq!(OutputMode::from_str_loose("plain"), Some(OutputMode::Plain));
        assert_eq!(OutputMode::from_str_loose("text"), Some(OutputMode::Plain));
        assert_eq!(OutputMode::from_str_loose("unknown"), None);
    }

    #[test]
    fn output_mode_properties() {
        assert!(OutputMode::Terminal.is_streaming());
        assert!(OutputMode::Ndjson.is_streaming());
        assert!(!OutputMode::Json.is_streaming());
        assert!(!OutputMode::Plain.is_streaming());

        assert!(!OutputMode::Terminal.suppress_chrome());
        assert!(OutputMode::Json.suppress_chrome());
        assert!(OutputMode::Ndjson.suppress_chrome());
        assert!(OutputMode::Plain.suppress_chrome());
    }

    #[test]
    fn output_event_serialization_roundtrip() {
        let events = vec![
            OutputEvent::Text {
                content: "hello".to_string(),
            },
            OutputEvent::ToolUse {
                id: "t1".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "ls"}),
            },
            OutputEvent::ToolResult {
                tool_use_id: "t1".to_string(),
                tool_name: "bash".to_string(),
                output: "file.rs".to_string(),
                is_error: false,
            },
            OutputEvent::Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 10,
                cache_creation_tokens: 5,
            },
            OutputEvent::TurnEnd { iterations: 2 },
            OutputEvent::Error {
                message: "oops".to_string(),
            },
            OutputEvent::System {
                message: "compacted".to_string(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).expect("serialize");
            let parsed: OutputEvent = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&parsed).expect("re-serialize");
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn ndjson_writer_writes_one_event_per_line() {
        let mut buf = Vec::new();
        {
            let mut writer = OutputWriter::new(&mut buf, OutputMode::Ndjson);
            writer
                .write_event(OutputEvent::Text {
                    content: "hello".to_string(),
                })
                .unwrap();
            writer
                .write_event(OutputEvent::Text {
                    content: "world".to_string(),
                })
                .unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("hello"));
        assert!(lines[1].contains("world"));
        // Each line should be valid JSON.
        for line in &lines {
            serde_json::from_str::<OutputEvent>(line).expect("valid JSON per line");
        }
    }

    #[test]
    fn json_writer_buffers_then_flushes() {
        let mut buf = Vec::new();
        {
            let mut writer = OutputWriter::new(&mut buf, OutputMode::Json);
            writer
                .write_event(OutputEvent::Text {
                    content: "hello".to_string(),
                })
                .unwrap();
            writer
                .write_event(OutputEvent::TurnEnd { iterations: 1 })
                .unwrap();
            assert_eq!(writer.buffered_events().len(), 2);
            writer.flush_json().unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<OutputEvent> = serde_json::from_str(&output).expect("valid JSON array");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn plain_writer_only_outputs_text_events() {
        let mut buf = Vec::new();
        {
            let mut writer = OutputWriter::new(&mut buf, OutputMode::Plain);
            writer
                .write_event(OutputEvent::Text {
                    content: "hello".to_string(),
                })
                .unwrap();
            writer
                .write_event(OutputEvent::ToolUse {
                    id: "t1".to_string(),
                    name: "bash".to_string(),
                    input: json!({}),
                })
                .unwrap();
            writer
                .write_event(OutputEvent::Text {
                    content: " world".to_string(),
                })
                .unwrap();
        }
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "hello world");
    }

    #[test]
    fn message_to_output_events_handles_all_block_types() {
        let msg = ConversationMessage::assistant_with_usage(
            vec![
                ContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "t1".to_string(),
                    name: "bash".to_string(),
                    input: r#"{"command":"ls"}"#.to_string(),
                },
            ],
            Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 20,
                cache_creation_input_tokens: 5,
                cache_read_input_tokens: 10,
            }),
        );
        let events = message_to_output_events(&msg);
        assert_eq!(events.len(), 3); // Text + ToolUse + Usage
        assert!(matches!(&events[0], OutputEvent::Text { content } if content == "Let me check."));
        assert!(matches!(&events[1], OutputEvent::ToolUse { name, .. } if name == "bash"));
        assert!(matches!(&events[2], OutputEvent::Usage { input_tokens: 100, .. }));
    }

    #[test]
    fn build_json_response_structure() {
        let assistant = vec![ConversationMessage::assistant(vec![ContentBlock::Text {
            text: "The answer is 42.".to_string(),
        }])];
        let tools = vec![ConversationMessage::tool_result(
            "t1",
            "bash",
            "42",
            false,
        )];
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };

        let response = build_json_response(&assistant, &tools, &usage, 2);
        assert_eq!(response["text"], "The answer is 42.");
        assert_eq!(response["iterations"], 2);
        assert_eq!(response["usage"]["input_tokens"], 100);
        assert_eq!(response["tool_results"][0]["output"], "42");
    }

    #[test]
    fn terminal_writer_is_noop() {
        let mut buf = Vec::new();
        {
            let mut writer = OutputWriter::new(&mut buf, OutputMode::Terminal);
            writer
                .write_event(OutputEvent::Text {
                    content: "hello".to_string(),
                })
                .unwrap();
        }
        // Terminal mode doesn't write to the structured output writer.
        assert!(buf.is_empty());
    }
}
