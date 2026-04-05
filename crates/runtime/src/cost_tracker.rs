use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::usage::{format_usd, pricing_for_model, ModelPricing, TokenUsage};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Per-model accumulated usage.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub api_calls: u32,
    pub total_cost_usd: f64,
    /// Whether pricing was a fallback estimate (model not in pricing table).
    pub is_fallback_pricing: bool,
}

/// Code change metrics tracked during the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeMetrics {
    pub lines_added: u64,
    pub lines_removed: u64,
    pub files_created: u32,
    pub files_modified: u32,
}

/// Timing metrics for the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TimingMetrics {
    /// Total API call duration in milliseconds.
    pub api_duration_ms: u64,
    /// Total tool execution duration in milliseconds.
    pub tool_duration_ms: u64,
}

/// Complete session cost state — the main tracker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostTracker {
    /// Per-model usage breakdown.
    pub models: BTreeMap<String, ModelUsage>,
    /// Aggregate across all models.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    /// Code change metrics.
    pub code_metrics: CodeMetrics,
    /// Timing metrics.
    pub timing: TimingMetrics,
    /// Number of conversation turns.
    pub turns: u32,
}

// ---------------------------------------------------------------------------
// CostTracker implementation
// ---------------------------------------------------------------------------

impl CostTracker {
    /// Create a new empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a single API call's usage for a given model.
    ///
    /// Looks up pricing via `pricing_for_model`. If the model is not found in
    /// the pricing table, falls back to the default sonnet-tier pricing and
    /// marks the entry as `is_fallback_pricing`.
    pub fn record_usage(&mut self, model: &str, usage: TokenUsage) {
        let (pricing, is_fallback) = match pricing_for_model(model) {
            Some(p) => (p, false),
            None => (ModelPricing::default_sonnet_tier(), true),
        };

        let cost = usage.estimate_cost_usd_with_pricing(pricing);
        let call_cost = cost.total_cost_usd();

        let entry = self
            .models
            .entry(model.to_string())
            .or_insert_with(|| ModelUsage {
                model: model.to_string(),
                ..Default::default()
            });

        entry.input_tokens += u64::from(usage.input_tokens);
        entry.output_tokens += u64::from(usage.output_tokens);
        entry.cache_read_tokens += u64::from(usage.cache_read_input_tokens);
        entry.cache_creation_tokens += u64::from(usage.cache_creation_input_tokens);
        entry.api_calls += 1;
        entry.total_cost_usd += call_cost;
        entry.is_fallback_pricing = is_fallback;

        // Update aggregates.
        self.total_input_tokens += u64::from(usage.input_tokens);
        self.total_output_tokens += u64::from(usage.output_tokens);
        self.total_cost_usd += call_cost;
    }

    /// Record code change metrics (e.g. after a file write/edit).
    pub fn record_code_change(&mut self, added: u64, removed: u64, is_new_file: bool) {
        self.code_metrics.lines_added += added;
        self.code_metrics.lines_removed += removed;
        if is_new_file {
            self.code_metrics.files_created += 1;
        } else {
            self.code_metrics.files_modified += 1;
        }
    }

    /// Record API call timing.
    pub fn record_api_timing(&mut self, duration_ms: u64) {
        self.timing.api_duration_ms += duration_ms;
    }

    /// Record tool execution timing.
    pub fn record_tool_timing(&mut self, duration_ms: u64) {
        self.timing.tool_duration_ms += duration_ms;
    }

    /// Increment the turn counter.
    pub fn record_turn(&mut self) {
        self.turns += 1;
    }

    /// Get total cost in USD.
    #[must_use]
    pub fn total_cost(&self) -> f64 {
        self.total_cost_usd
    }

    /// Format a detailed cost summary for display.
    ///
    /// Example output:
    /// ```text
    /// Session Cost: $0.1234
    ///   claude-opus-4-6: 3 calls, 1,234 in / 567 out, $0.0987
    ///   qwen3-8b: 2 calls, 890 in / 345 out, $0.0247
    /// Code: +42 / -15 lines, 3 files modified, 1 created
    /// ```
    #[must_use]
    pub fn format_summary(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!("Session Cost: {}", format_usd(self.total_cost_usd)));

        // Sort models by cost descending.
        let mut models: Vec<&ModelUsage> = self.models.values().collect();
        models.sort_by(|a, b| {
            b.total_cost_usd
                .partial_cmp(&a.total_cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for m in &models {
            let calls_label = if m.api_calls == 1 { "call" } else { "calls" };
            let fallback_marker = if m.is_fallback_pricing { " ~" } else { "" };
            lines.push(format!(
                "  {}: {} {}, {} in / {} out, {}{}",
                m.model,
                m.api_calls,
                calls_label,
                format_tokens(m.input_tokens),
                format_tokens(m.output_tokens),
                format_usd(m.total_cost_usd),
                fallback_marker,
            ));
        }

        // Code metrics (only if there is any activity).
        let cm = &self.code_metrics;
        if cm.lines_added > 0 || cm.lines_removed > 0 || cm.files_created > 0 || cm.files_modified > 0 {
            let mut parts: Vec<String> = Vec::new();
            if cm.lines_added > 0 || cm.lines_removed > 0 {
                parts.push(format!("+{} / -{} lines", cm.lines_added, cm.lines_removed));
            }
            if cm.files_modified > 0 {
                let label = if cm.files_modified == 1 { "file" } else { "files" };
                parts.push(format!("{} {} modified", cm.files_modified, label));
            }
            if cm.files_created > 0 {
                let label = if cm.files_created == 1 { "created" } else { "created" };
                parts.push(format!("{} {}", cm.files_created, label));
            }
            lines.push(format!("Code: {}", parts.join(", ")));
        }

        lines.join("\n")
    }

    /// Format a short one-line cost summary.
    ///
    /// Example: `"$0.1234 (5 turns, 1,801 tokens)"`
    #[must_use]
    pub fn format_short(&self) -> String {
        let total_tokens = self.total_input_tokens + self.total_output_tokens;
        let turns_label = if self.turns == 1 { "turn" } else { "turns" };
        format!(
            "{} ({} {}, {} tokens)",
            format_usd(self.total_cost_usd),
            self.turns,
            turns_label,
            format_tokens(total_tokens),
        )
    }
}

// ---------------------------------------------------------------------------
// Session persistence
// ---------------------------------------------------------------------------

/// Resolve the path to a session's cost file.
fn session_cost_path(project_dir: &Path, session_id: &str) -> PathBuf {
    project_dir
        .join(".ember")
        .join("sessions")
        .join(session_id)
        .join("costs.json")
}

/// Save cost tracker state for a session to a JSON file.
///
/// Creates intermediate directories as needed.
pub fn save_session_costs(
    project_dir: &Path,
    session_id: &str,
    tracker: &CostTracker,
) -> io::Result<()> {
    let path = session_cost_path(project_dir, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tracker)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

/// Load cost tracker state for a session from a JSON file.
///
/// Returns `Ok(None)` if the file does not exist.
pub fn load_session_costs(
    project_dir: &Path,
    session_id: &str,
) -> io::Result<Option<CostTracker>> {
    let path = session_cost_path(project_dir, session_id);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)?;
    let tracker: CostTracker = serde_json::from_str(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(tracker))
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a token count with comma separators for readability.
///
/// Examples: `1234` -> `"1,234"`, `1000000` -> `"1,000,000"`.
#[must_use]
pub fn format_tokens(count: u64) -> String {
    if count == 0 {
        return "0".to_string();
    }

    let s = count.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();

    let mut result = String::with_capacity(len + (len - 1) / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(b as char);
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::TokenUsage;
    use std::fs;

    fn sample_usage(input: u32, output: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    #[test]
    fn new_tracker_starts_empty() {
        let tracker = CostTracker::new();
        assert_eq!(tracker.total_input_tokens, 0);
        assert_eq!(tracker.total_output_tokens, 0);
        assert!((tracker.total_cost_usd - 0.0).abs() < f64::EPSILON);
        assert!(tracker.models.is_empty());
        assert_eq!(tracker.turns, 0);
        assert_eq!(tracker.code_metrics, CodeMetrics::default());
        assert_eq!(tracker.timing, TimingMetrics::default());
    }

    #[test]
    fn record_usage_accumulates_per_model() {
        let mut tracker = CostTracker::new();
        tracker.record_usage("claude-opus-4-6", sample_usage(100, 50));
        tracker.record_usage("claude-opus-4-6", sample_usage(200, 100));

        let entry = tracker.models.get("claude-opus-4-6").unwrap();
        assert_eq!(entry.api_calls, 2);
        assert_eq!(entry.input_tokens, 300);
        assert_eq!(entry.output_tokens, 150);
        assert!(!entry.is_fallback_pricing);

        assert_eq!(tracker.total_input_tokens, 300);
        assert_eq!(tracker.total_output_tokens, 150);
        assert!(tracker.total_cost_usd > 0.0);
    }

    #[test]
    fn record_usage_with_multiple_models() {
        let mut tracker = CostTracker::new();
        tracker.record_usage("claude-opus-4-6", sample_usage(1000, 500));
        tracker.record_usage("claude-haiku-4-5", sample_usage(1000, 500));

        assert_eq!(tracker.models.len(), 2);
        assert_eq!(tracker.total_input_tokens, 2000);
        assert_eq!(tracker.total_output_tokens, 1000);

        // Opus should cost more than Haiku for the same token counts.
        let opus_cost = tracker.models["claude-opus-4-6"].total_cost_usd;
        let haiku_cost = tracker.models["claude-haiku-4-5"].total_cost_usd;
        assert!(opus_cost > haiku_cost, "opus={opus_cost} should > haiku={haiku_cost}");
    }

    #[test]
    fn unknown_model_uses_fallback_pricing() {
        let mut tracker = CostTracker::new();
        tracker.record_usage("custom-model-v1", sample_usage(100, 50));

        let entry = tracker.models.get("custom-model-v1").unwrap();
        assert!(entry.is_fallback_pricing);
        assert!(entry.total_cost_usd > 0.0);
    }

    #[test]
    fn record_code_change_updates_metrics() {
        let mut tracker = CostTracker::new();
        tracker.record_code_change(42, 15, false);
        tracker.record_code_change(10, 0, true);

        assert_eq!(tracker.code_metrics.lines_added, 52);
        assert_eq!(tracker.code_metrics.lines_removed, 15);
        assert_eq!(tracker.code_metrics.files_modified, 1);
        assert_eq!(tracker.code_metrics.files_created, 1);
    }

    #[test]
    fn record_turn_increments_counter() {
        let mut tracker = CostTracker::new();
        assert_eq!(tracker.turns, 0);
        tracker.record_turn();
        tracker.record_turn();
        tracker.record_turn();
        assert_eq!(tracker.turns, 3);
    }

    #[test]
    fn record_timing_accumulates() {
        let mut tracker = CostTracker::new();
        tracker.record_api_timing(100);
        tracker.record_api_timing(250);
        tracker.record_tool_timing(50);

        assert_eq!(tracker.timing.api_duration_ms, 350);
        assert_eq!(tracker.timing.tool_duration_ms, 50);
    }

    #[test]
    fn format_tokens_various_sizes() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(5), "5");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_234), "1,234");
        assert_eq!(format_tokens(12_345), "12,345");
        assert_eq!(format_tokens(123_456), "123,456");
        assert_eq!(format_tokens(1_000_000), "1,000,000");
        assert_eq!(format_tokens(1_234_567_890), "1,234,567,890");
    }

    #[test]
    fn format_summary_produces_expected_shape() {
        let mut tracker = CostTracker::new();
        tracker.record_usage("claude-opus-4-6", sample_usage(1234, 567));
        tracker.record_usage("claude-opus-4-6", sample_usage(500, 200));
        tracker.record_usage("claude-opus-4-6", sample_usage(500, 200));
        tracker.record_usage("claude-haiku-4-5", sample_usage(890, 345));
        tracker.record_usage("claude-haiku-4-5", sample_usage(500, 200));
        tracker.record_code_change(42, 15, false);
        tracker.record_code_change(10, 0, false);
        tracker.record_code_change(5, 0, false);
        tracker.record_code_change(8, 3, true);

        let summary = tracker.format_summary();

        // Must start with session cost.
        assert!(summary.starts_with("Session Cost: $"), "got: {summary}");

        // Must contain both models.
        assert!(summary.contains("claude-opus-4-6"), "missing opus in:\n{summary}");
        assert!(summary.contains("claude-haiku-4-5"), "missing haiku in:\n{summary}");

        // Must contain calls count.
        assert!(summary.contains("3 calls"), "missing '3 calls' in:\n{summary}");
        assert!(summary.contains("2 calls"), "missing '2 calls' in:\n{summary}");

        // Must contain code metrics.
        assert!(summary.contains("Code:"), "missing Code: line in:\n{summary}");
        assert!(summary.contains("+65 / -18 lines"), "wrong line counts in:\n{summary}");
        assert!(summary.contains("3 files modified"), "wrong files modified in:\n{summary}");
        assert!(summary.contains("1 created"), "missing created in:\n{summary}");

        // Opus should appear before haiku (higher cost first).
        let opus_pos = summary.find("claude-opus-4-6").unwrap();
        let haiku_pos = summary.find("claude-haiku-4-5").unwrap();
        assert!(opus_pos < haiku_pos, "opus should come before haiku (cost desc)");
    }

    #[test]
    fn format_short_produces_expected_format() {
        let mut tracker = CostTracker::new();
        tracker.record_usage("claude-opus-4-6", sample_usage(1000, 801));
        tracker.record_turn();
        tracker.record_turn();
        tracker.record_turn();
        tracker.record_turn();
        tracker.record_turn();

        let short = tracker.format_short();

        assert!(short.starts_with('$'), "should start with $: {short}");
        assert!(short.contains("5 turns"), "should contain '5 turns': {short}");
        assert!(short.contains("1,801 tokens"), "should contain '1,801 tokens': {short}");
    }

    #[test]
    fn format_short_singular_turn() {
        let mut tracker = CostTracker::new();
        tracker.record_turn();
        let short = tracker.format_short();
        assert!(short.contains("1 turn,"), "should use singular 'turn': {short}");
    }

    /// Create a unique temp directory for a test and return its path.
    fn make_temp_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("emberforge_cost_tracker_tests")
            .join(test_name);
        // Clean up any previous run.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_load_round_trip() {
        let dir = make_temp_dir("save_load_round_trip");
        let mut tracker = CostTracker::new();
        tracker.record_usage("claude-opus-4-6", sample_usage(500, 200));
        tracker.record_code_change(10, 3, true);
        tracker.record_turn();
        tracker.record_api_timing(150);
        tracker.record_tool_timing(80);

        save_session_costs(&dir, "test-session-1", &tracker).unwrap();

        // Verify file exists at expected path.
        let expected = dir.join(".ember/sessions/test-session-1/costs.json");
        assert!(expected.exists(), "cost file should exist at {expected:?}");

        // Load and compare.
        let loaded = load_session_costs(&dir, "test-session-1")
            .unwrap()
            .expect("should load saved tracker");
        assert_eq!(loaded.total_input_tokens, tracker.total_input_tokens);
        assert_eq!(loaded.total_output_tokens, tracker.total_output_tokens);
        assert!((loaded.total_cost_usd - tracker.total_cost_usd).abs() < 1e-10);
        assert_eq!(loaded.turns, 1);
        assert_eq!(loaded.code_metrics, tracker.code_metrics);
        assert_eq!(loaded.timing, tracker.timing);
        assert_eq!(loaded.models.len(), 1);

        // Cleanup.
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = make_temp_dir("load_nonexistent");
        let result = load_session_costs(&dir, "no-such-session").unwrap();
        assert!(result.is_none());

        // Cleanup.
        let _ = fs::remove_dir_all(&dir);
    }
}
