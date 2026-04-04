//! Live Ollama integration tests.
//!
//! Require a running Ollama server at localhost:11434.
//! Run: cargo test -p api --test ollama_live_integration -- --nocapture

use std::process::Command;

use api::{
    ContentBlockDelta, InputContentBlock, InputMessage, MessageRequest,
    OutputContentBlock, ProviderClient, ProviderKind, StreamEvent,
    ToolChoice, ToolDefinition,
};

fn ollama_available() -> bool {
    Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "http://localhost:11434/api/tags"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
        .unwrap_or(false)
}

// ── Provider Detection ───────────────────────────────────────────────────

#[test]
fn provider_client_routes_ollama_models() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }
    let client = ProviderClient::from_model("qwen3:8b");
    assert!(client.is_ok(), "should resolve Ollama model: {:?}", client.err());
    assert_eq!(client.unwrap().provider_kind(), ProviderKind::Ollama);
}

#[test]
fn provider_client_routes_multiple_ollama_families() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }
    for model in ["llama3.2:1b", "gemma3:1b", "phi4-mini", "deepseek-r1:1.5b", "mistral:7b-instruct-v0.3-q4_K_M"] {
        let client = ProviderClient::from_model(model);
        assert!(client.is_ok(), "{model} should route to Ollama: {:?}", client.err());
        assert_eq!(client.unwrap().provider_kind(), ProviderKind::Ollama, "{model}");
    }
}

// ── Live Generation ──────────────────────────────────────────────────────

#[tokio::test]
async fn ollama_send_message_generates_text() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }

    let client = ProviderClient::from_model("qwen2.5:0.5b").expect("client");

    let request = MessageRequest {
        model: "qwen2.5:0.5b".to_string(),
        max_tokens: 32,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: "Reply with exactly: HELLO".to_string(),
            }],
        }],
        system: Some("Be brief.".to_string()),
        tools: None,
        tool_choice: None,
        stream: false,
    };

    let response = client.send_message(&request).await;
    assert!(response.is_ok(), "send_message failed: {:?}", response.err());
    let msg = response.unwrap();
    assert!(!msg.content.is_empty(), "response should have content");
    let has_text = msg.content.iter().any(|b| matches!(b, OutputContentBlock::Text { text } if !text.is_empty()));
    assert!(has_text, "should contain non-empty text");
    eprintln!("  qwen2.5:0.5b send_message: OK");
}

#[tokio::test]
async fn ollama_streaming_produces_events() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }

    let client = ProviderClient::from_model("qwen2.5:0.5b").expect("client");

    let request = MessageRequest {
        model: "qwen2.5:0.5b".to_string(),
        max_tokens: 32,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: "Say hi.".to_string(),
            }],
        }],
        system: Some("Be brief.".to_string()),
        tools: None,
        tool_choice: None,
        stream: true,
    };

    let mut stream = client.stream_message(&request).await.expect("stream");
    let mut event_count = 0;
    let mut got_text = false;
    let mut got_stop = false;

    while let Some(event) = stream.next_event().await.expect("event") {
        event_count += 1;
        match &event {
            StreamEvent::ContentBlockDelta(delta) => {
                if let ContentBlockDelta::TextDelta { text } = &delta.delta {
                    if !text.is_empty() { got_text = true; }
                }
            }
            StreamEvent::MessageStop(_) => { got_stop = true; }
            _ => {}
        }
    }

    assert!(event_count > 0, "should receive events");
    assert!(got_text, "should receive text");
    assert!(got_stop, "should receive stop");
    eprintln!("  streaming: {event_count} events, text={got_text}, stop={got_stop}");
}

#[tokio::test]
async fn ollama_multiple_models_generate() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }

    let models = ["qwen2.5:0.5b", "llama3.2:1b", "gemma3:1b"];
    for model in models {
        let client = ProviderClient::from_model(model).expect("client");
        let request = MessageRequest {
            model: model.to_string(),
            max_tokens: 16,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "What is 2+2?".to_string(),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
        };
        let result = client.send_message(&request).await;
        assert!(result.is_ok(), "{model} failed: {:?}", result.err());
        eprintln!("  {model}: OK");
    }
}

#[tokio::test]
async fn ollama_tool_calling() {
    if !ollama_available() {
        eprintln!("SKIPPED: Ollama not running");
        return;
    }

    let client = ProviderClient::from_model("qwen2.5:7b-instruct-q8_0").expect("client");
    let request = MessageRequest {
        model: "qwen2.5:7b-instruct-q8_0".to_string(),
        max_tokens: 256,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: "What's the weather in Tokyo?".to_string(),
            }],
        }],
        system: Some("Use the get_weather tool.".to_string()),
        tools: Some(vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("Get weather for a city".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        }]),
        tool_choice: Some(ToolChoice::Auto),
        stream: false,
    };

    let result = client.send_message(&request).await;
    assert!(result.is_ok(), "tool call failed: {:?}", result.err());
    let msg = result.unwrap();
    assert!(!msg.content.is_empty());

    let tool_calls: Vec<_> = msg.content.iter()
        .filter(|b| matches!(b, OutputContentBlock::ToolUse { .. }))
        .collect();
    if !tool_calls.is_empty() {
        eprintln!("  Tool call: {} calls", tool_calls.len());
    } else {
        eprintln!("  Model responded with text (acceptable)");
    }
}
