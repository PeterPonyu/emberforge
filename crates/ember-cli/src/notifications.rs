//! Notification system — send events to external services via webhooks.
//!
//! Supports Discord, Slack, and custom webhook endpoints.
//! Configured via settings: `{ "notifications": { "webhooks": [...] } }`

use std::collections::BTreeMap;
use std::io;

/// A notification event to send to configured webhooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationEvent {
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    pub metadata: BTreeMap<String, String>,
}

/// Categories of notification events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationKind {
    SessionStart,
    SessionEnd,
    TurnComplete,
    TaskComplete,
    Error,
}

impl NotificationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::TurnComplete => "turn_complete",
            Self::TaskComplete => "task_complete",
            Self::Error => "error",
        }
    }

    /// Parse a notification kind from its string form (case-insensitive),
    /// mirroring the vocabulary produced by [`Self::as_str`].
    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "session_start" => Some(Self::SessionStart),
            "session_end" => Some(Self::SessionEnd),
            "turn_complete" => Some(Self::TurnComplete),
            "task_complete" => Some(Self::TaskComplete),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    fn emoji(self) -> &'static str {
        match self {
            Self::SessionStart => "🔥",
            Self::SessionEnd => "✅",
            Self::TurnComplete => "💬",
            Self::TaskComplete => "🎯",
            Self::Error => "❌",
        }
    }
}

/// Webhook configuration for a single endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebhookConfig {
    pub url: String,
    pub provider: WebhookProvider,
    /// Only send these event kinds (empty = all).
    pub filter: Vec<NotificationKind>,
}

/// Supported webhook providers with format-specific payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookProvider {
    Discord,
    Slack,
    Custom,
}

impl WebhookProvider {
    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "discord" => Some(Self::Discord),
            "slack" => Some(Self::Slack),
            "custom" | "webhook" => Some(Self::Custom),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Slack => "slack",
            Self::Custom => "custom",
        }
    }

    /// Auto-detect provider from URL pattern.
    pub fn detect_from_url(url: &str) -> Self {
        if url.contains("discord.com/api/webhooks") || url.contains("discordapp.com/api/webhooks") {
            Self::Discord
        } else if url.contains("hooks.slack.com") {
            Self::Slack
        } else {
            Self::Custom
        }
    }
}

/// Format the notification payload for the target provider.
fn format_payload(event: &NotificationEvent, provider: WebhookProvider) -> String {
    match provider {
        WebhookProvider::Discord => {
            let content = format!(
                "{} **[{}]** {}\n{}",
                event.kind.emoji(),
                event.kind.as_str(),
                event.title,
                event.body
            );
            // Discord webhook expects { "content": "..." }
            serde_json::json!({ "content": truncate(&content, 2000) }).to_string()
        }
        WebhookProvider::Slack => {
            let text = format!(
                "{} *[{}]* {}\n{}",
                event.kind.emoji(),
                event.kind.as_str(),
                event.title,
                event.body
            );
            // Slack webhook expects { "text": "..." }
            serde_json::json!({ "text": truncate(&text, 3000) }).to_string()
        }
        WebhookProvider::Custom => {
            // Generic JSON payload with all fields.
            serde_json::json!({
                "event": event.kind.as_str(),
                "title": event.title,
                "body": event.body,
                "metadata": event.metadata,
            })
            .to_string()
        }
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}...", &value[..max.saturating_sub(3)])
    }
}

/// Decide whether an event should be delivered to a webhook.
///
/// An empty filter means "deliver all"; otherwise only kinds present in the
/// filter are delivered.
fn should_deliver(config: &WebhookConfig, kind: NotificationKind) -> bool {
    config.filter.is_empty() || config.filter.contains(&kind)
}

/// Send a notification to a single webhook (blocking HTTP POST).
fn send_webhook(
    config: &WebhookConfig,
    event: &NotificationEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    // Filter: if the webhook has a filter list and the event kind isn't in it, skip.
    if !should_deliver(config, event.kind) {
        return Ok(());
    }

    let payload = format_payload(event, config.provider);

    // Use a short-lived curl subprocess to avoid pulling in reqwest as a dependency
    // in the CLI crate. This runs in the background and doesn't block the REPL.
    let status = std::process::Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &payload,
            "--max-time",
            "5",
            &config.url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(exit) if exit.success() => Ok(()),
        Ok(exit) => Err(format!("webhook returned exit code {exit}").into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err("curl not found — install curl to enable webhook notifications".into())
        }
        Err(e) => Err(Box::new(e)),
    }
}

/// Notification dispatcher — holds webhook configs and sends events.
#[derive(Debug, Clone, Default)]
pub(crate) struct NotificationDispatcher {
    webhooks: Vec<WebhookConfig>,
}

impl NotificationDispatcher {
    #[must_use]
    pub fn new(webhooks: Vec<WebhookConfig>) -> Self {
        Self { webhooks }
    }

    /// Returns true if any webhooks are configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.webhooks.is_empty()
    }

    /// Send a notification to all configured webhooks.
    /// Errors are logged to stderr but do not interrupt the caller.
    pub fn notify(&self, event: &NotificationEvent) {
        for webhook in &self.webhooks {
            if let Err(e) = send_webhook(webhook, event) {
                eprintln!(
                    "\x1b[2m[notify] {} webhook failed: {e}\x1b[0m",
                    webhook.provider.as_str()
                );
            }
        }
    }

    /// Send a notification in a background thread (non-blocking).
    pub fn notify_async(&self, event: NotificationEvent) {
        if !self.is_active() {
            return;
        }
        let dispatcher = self.clone();
        std::thread::spawn(move || {
            dispatcher.notify(&event);
        });
    }
}

/// Parse webhook configurations from settings JSON.
///
/// Expected format:
/// ```json
/// {
///   "notifications": {
///     "webhooks": [
///       { "url": "https://discord.com/api/webhooks/...", "provider": "discord" },
///       {
///         "url": "https://hooks.slack.com/...",
///         "provider": "slack",
///         "filter": ["error", "task_complete"]
///       }
///     ]
///   }
/// }
/// ```
///
/// Each webhook may carry an optional `filter` (alias `events`) array of
/// [`NotificationKind`] strings (e.g. `"session_start"`, `"error"`). Only the
/// listed kinds are delivered to that webhook. An absent or empty filter
/// preserves the default behavior of delivering every event kind (see
/// [`send_webhook`]). Unrecognized kind strings are skipped with a warning,
/// matching how the rest of this parser drops invalid values rather than
/// failing the whole config.
pub(crate) fn parse_webhook_configs(settings: &serde_json::Value) -> Vec<WebhookConfig> {
    let Some(notifications) = settings.get("notifications") else {
        return Vec::new();
    };
    let Some(webhooks) = notifications.get("webhooks").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    webhooks
        .iter()
        .filter_map(|entry| {
            let url = entry.get("url")?.as_str()?.to_string();
            let provider = entry
                .get("provider")
                .and_then(|v| v.as_str())
                .and_then(WebhookProvider::from_str)
                .unwrap_or_else(|| WebhookProvider::detect_from_url(&url));
            let filter = parse_kind_filter(entry);
            Some(WebhookConfig {
                url,
                provider,
                filter,
            })
        })
        .collect()
}

/// Parse the optional `filter` (or `events`) array on a single webhook entry
/// into a list of [`NotificationKind`]s. Unknown kind strings are skipped with
/// a warning, consistent with the surrounding parser's lenient handling of bad
/// values. An absent or empty array yields an empty filter (deliver all).
fn parse_kind_filter(entry: &serde_json::Value) -> Vec<NotificationKind> {
    let Some(values) = entry
        .get("filter")
        .or_else(|| entry.get("events"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };

    values
        .iter()
        .filter_map(|value| {
            let raw = value.as_str()?;
            match NotificationKind::from_str(raw) {
                Some(kind) => Some(kind),
                None => {
                    eprintln!(
                        "warning: ignoring unknown notification kind in webhook filter: {raw:?}"
                    );
                    None
                }
            }
        })
        .collect()
}

/// Helper to create common notification events.
pub(crate) fn session_start_event(model: &str, session_id: &str) -> NotificationEvent {
    let mut metadata = BTreeMap::new();
    metadata.insert("model".to_string(), model.to_string());
    metadata.insert("session_id".to_string(), session_id.to_string());
    NotificationEvent {
        kind: NotificationKind::SessionStart,
        title: format!("Emberforge session started ({model})"),
        body: format!("Session {session_id} using model {model}"),
        metadata,
    }
}

pub(crate) fn turn_complete_event(
    model: &str,
    elapsed_ms: u64,
    input_preview: &str,
) -> NotificationEvent {
    let mut metadata = BTreeMap::new();
    metadata.insert("model".to_string(), model.to_string());
    metadata.insert("elapsed_ms".to_string(), elapsed_ms.to_string());
    let preview = if input_preview.len() > 80 {
        format!("{}...", &input_preview[..80])
    } else {
        input_preview.to_string()
    };
    NotificationEvent {
        kind: NotificationKind::TurnComplete,
        title: format!("Turn complete ({:.1}s)", elapsed_ms as f64 / 1000.0),
        body: preview,
        metadata,
    }
}

pub(crate) fn error_event(error_message: &str) -> NotificationEvent {
    NotificationEvent {
        kind: NotificationKind::Error,
        title: "Emberforge error".to_string(),
        body: error_message.to_string(),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_discord_provider_from_url() {
        assert_eq!(
            WebhookProvider::detect_from_url("https://discord.com/api/webhooks/123/abc"),
            WebhookProvider::Discord
        );
    }

    #[test]
    fn detect_slack_provider_from_url() {
        assert_eq!(
            WebhookProvider::detect_from_url("https://hooks.slack.com/services/T00/B00/xxx"),
            WebhookProvider::Slack
        );
    }

    #[test]
    fn detect_custom_for_unknown_url() {
        assert_eq!(
            WebhookProvider::detect_from_url("https://example.com/webhook"),
            WebhookProvider::Custom
        );
    }

    #[test]
    fn format_discord_payload() {
        let event = NotificationEvent {
            kind: NotificationKind::TurnComplete,
            title: "done".to_string(),
            body: "test".to_string(),
            metadata: BTreeMap::new(),
        };
        let payload = format_payload(&event, WebhookProvider::Discord);
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(parsed["content"].as_str().unwrap().contains("done"));
    }

    #[test]
    fn format_slack_payload() {
        let event = NotificationEvent {
            kind: NotificationKind::SessionStart,
            title: "started".to_string(),
            body: "test".to_string(),
            metadata: BTreeMap::new(),
        };
        let payload = format_payload(&event, WebhookProvider::Slack);
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(parsed["text"].as_str().unwrap().contains("started"));
    }

    #[test]
    fn format_custom_payload_includes_metadata() {
        let mut metadata = BTreeMap::new();
        metadata.insert("key".to_string(), "value".to_string());
        let event = NotificationEvent {
            kind: NotificationKind::Error,
            title: "oops".to_string(),
            body: "details".to_string(),
            metadata,
        };
        let payload = format_payload(&event, WebhookProvider::Custom);
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["event"], "error");
        assert_eq!(parsed["metadata"]["key"], "value");
    }

    #[test]
    fn parse_webhook_configs_from_settings() {
        let settings = serde_json::json!({
            "notifications": {
                "webhooks": [
                    { "url": "https://discord.com/api/webhooks/123/abc" },
                    { "url": "https://example.com/hook", "provider": "custom" }
                ]
            }
        });
        let configs = parse_webhook_configs(&settings);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].provider, WebhookProvider::Discord);
        assert_eq!(configs[1].provider, WebhookProvider::Custom);
    }

    #[test]
    fn empty_settings_returns_no_webhooks() {
        let configs = parse_webhook_configs(&serde_json::json!({}));
        assert!(configs.is_empty());
    }

    #[test]
    fn dispatcher_inactive_when_no_webhooks() {
        let d = NotificationDispatcher::default();
        assert!(!d.is_active());
    }

    #[test]
    fn parse_webhook_filter_from_settings() {
        let settings = serde_json::json!({
            "notifications": {
                "webhooks": [
                    {
                        "url": "https://example.com/hook",
                        "provider": "custom",
                        "filter": ["error", "task_complete"]
                    }
                ]
            }
        });
        let configs = parse_webhook_configs(&settings);
        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].filter,
            vec![NotificationKind::Error, NotificationKind::TaskComplete]
        );
    }

    #[test]
    fn parse_webhook_filter_accepts_events_alias() {
        let settings = serde_json::json!({
            "notifications": {
                "webhooks": [
                    {
                        "url": "https://example.com/hook",
                        "provider": "custom",
                        "events": ["session_start"]
                    }
                ]
            }
        });
        let configs = parse_webhook_configs(&settings);
        assert_eq!(configs[0].filter, vec![NotificationKind::SessionStart]);
    }

    #[test]
    fn parse_webhook_filter_skips_unknown_kinds() {
        let settings = serde_json::json!({
            "notifications": {
                "webhooks": [
                    {
                        "url": "https://example.com/hook",
                        "provider": "custom",
                        "filter": ["error", "not_a_real_kind"]
                    }
                ]
            }
        });
        let configs = parse_webhook_configs(&settings);
        // Unknown kinds are dropped; valid ones are retained.
        assert_eq!(configs[0].filter, vec![NotificationKind::Error]);
    }

    #[test]
    fn absent_filter_delivers_all_kinds() {
        let config = WebhookConfig {
            url: "https://example.com/hook".to_string(),
            provider: WebhookProvider::Custom,
            filter: Vec::new(),
        };
        for kind in [
            NotificationKind::SessionStart,
            NotificationKind::SessionEnd,
            NotificationKind::TurnComplete,
            NotificationKind::TaskComplete,
            NotificationKind::Error,
        ] {
            assert!(should_deliver(&config, kind));
        }
    }

    #[test]
    fn filtered_webhook_delivers_matching_and_skips_non_matching() {
        let config = WebhookConfig {
            url: "https://example.com/hook".to_string(),
            provider: WebhookProvider::Custom,
            filter: vec![NotificationKind::Error],
        };
        // Matching kind is delivered.
        assert!(should_deliver(&config, NotificationKind::Error));
        // Non-matching kind is skipped.
        assert!(!should_deliver(&config, NotificationKind::TurnComplete));
    }
}
