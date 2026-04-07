//! Context collapse: advanced context optimization beyond basic compaction.
//!
//! Mirrors the Claude Code TypeScript `CONTEXT_COLLAPSE` feature.
//! This module provides strategies for reducing context size while preserving
//! the most relevant information for the current task.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A segment of conversation context with metadata about its importance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSegment {
    /// Unique identifier for this segment.
    pub id: String,
    /// The text content.
    pub content: String,
    /// Estimated token count.
    pub estimated_tokens: usize,
    /// Importance score (0.0 to 1.0).
    pub importance: f64,
    /// Source of the segment (system, user, assistant, `tool_result`).
    pub source: String,
    /// Whether this segment is pinned (never collapsed).
    pub pinned: bool,
}

/// Strategy for collapsing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollapseStrategy {
    /// Remove lowest-importance segments first.
    ImportanceBased,
    /// Remove oldest segments first, preserving recent context.
    RecencyBased,
    /// Summarize groups of similar segments.
    Summarize,
    /// Truncate long tool results to their essential output.
    TruncateToolResults,
}

/// Configuration for context collapse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapseConfig {
    /// Target token budget after collapse.
    pub target_tokens: usize,
    /// Strategy to use.
    pub strategy: CollapseStrategy,
    /// Number of recent messages to always preserve.
    pub preserve_recent: usize,
    /// Maximum length for tool result truncation (chars).
    pub max_tool_result_chars: usize,
}

/// Result of a context collapse operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollapseResult {
    /// Segments that survived the collapse.
    pub retained: Vec<ContextSegment>,
    /// Number of segments removed.
    pub removed_count: usize,
    /// Estimated tokens saved.
    pub tokens_saved: usize,
    /// Strategy that was applied.
    pub strategy: CollapseStrategy,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl Default for CollapseConfig {
    fn default() -> Self {
        Self {
            target_tokens: 100_000,
            strategy: CollapseStrategy::ImportanceBased,
            preserve_recent: 6,
            max_tool_result_chars: 8000,
        }
    }
}

/// Collapse context segments according to the given config.
#[must_use]
pub fn collapse_context(
    segments: &[ContextSegment],
    config: &CollapseConfig,
) -> CollapseResult {
    let total_tokens: usize = segments.iter().map(|s| s.estimated_tokens).sum();

    if total_tokens <= config.target_tokens {
        return CollapseResult {
            retained: segments.to_vec(),
            removed_count: 0,
            tokens_saved: 0,
            strategy: config.strategy,
        };
    }

    match config.strategy {
        CollapseStrategy::ImportanceBased | CollapseStrategy::Summarize => {
            collapse_by_importance(segments, config)
        }
        CollapseStrategy::RecencyBased => collapse_by_recency(segments, config),
        CollapseStrategy::TruncateToolResults => truncate_tool_results(segments, config),
    }
}

fn collapse_by_importance(
    segments: &[ContextSegment],
    config: &CollapseConfig,
) -> CollapseResult {
    let total = segments.len();
    let preserve_start = total.saturating_sub(config.preserve_recent);

    // Score and sort older segments by importance
    let mut candidates: Vec<(usize, &ContextSegment)> = segments[..preserve_start]
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.pinned)
        .collect();
    candidates.sort_by(|a, b| a.1.importance.partial_cmp(&b.1.importance).unwrap_or(std::cmp::Ordering::Equal));

    let mut retained = Vec::new();
    let mut removed_count = 0;
    let mut tokens_saved = 0;
    let mut current_tokens: usize = segments.iter().map(|s| s.estimated_tokens).sum();
    let mut removed_indices = std::collections::BTreeSet::new();

    // Remove lowest importance segments until under budget
    for (idx, seg) in &candidates {
        if current_tokens <= config.target_tokens {
            break;
        }
        removed_indices.insert(*idx);
        current_tokens -= seg.estimated_tokens;
        tokens_saved += seg.estimated_tokens;
        removed_count += 1;
    }

    for (i, seg) in segments.iter().enumerate() {
        if !removed_indices.contains(&i) {
            retained.push(seg.clone());
        }
    }

    CollapseResult {
        retained,
        removed_count,
        tokens_saved,
        strategy: CollapseStrategy::ImportanceBased,
    }
}

fn collapse_by_recency(
    segments: &[ContextSegment],
    config: &CollapseConfig,
) -> CollapseResult {
    let total = segments.len();
    let preserve_start = total.saturating_sub(config.preserve_recent);

    let mut retained = Vec::new();
    let mut removed_count = 0;
    let mut tokens_saved = 0;
    let mut current_tokens: usize = segments.iter().map(|s| s.estimated_tokens).sum();

    for (i, seg) in segments.iter().enumerate() {
        if i >= preserve_start || seg.pinned || current_tokens <= config.target_tokens {
            retained.push(seg.clone());
        } else {
            current_tokens -= seg.estimated_tokens;
            tokens_saved += seg.estimated_tokens;
            removed_count += 1;
        }
    }

    CollapseResult {
        retained,
        removed_count,
        tokens_saved,
        strategy: CollapseStrategy::RecencyBased,
    }
}

fn truncate_tool_results(
    segments: &[ContextSegment],
    config: &CollapseConfig,
) -> CollapseResult {
    let mut retained = Vec::new();
    let mut tokens_saved = 0;
    let mut removed_count = 0;

    for seg in segments {
        if seg.source == "tool_result" && seg.content.len() > config.max_tool_result_chars {
            let truncated = &seg.content[..config.max_tool_result_chars];
            let new_tokens = estimate_tokens(truncated);
            tokens_saved += seg.estimated_tokens.saturating_sub(new_tokens);
            retained.push(ContextSegment {
                content: format!("{truncated}\n... [truncated]"),
                estimated_tokens: new_tokens,
                ..seg.clone()
            });
            removed_count += 1; // counts as a modification
        } else {
            retained.push(seg.clone());
        }
    }

    CollapseResult {
        retained,
        removed_count,
        tokens_saved,
        strategy: CollapseStrategy::TruncateToolResults,
    }
}

/// Simple token estimation (~4 chars per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Compute importance score for a segment based on heuristics.
#[must_use]
pub fn compute_importance(content: &str, source: &str, _position: usize, _total: usize) -> f64 {
    let mut score: f64 = 0.5;

    // System messages are high importance
    if source == "system" {
        score = 0.95;
    }

    // User messages are moderately important
    if source == "user" {
        score = 0.7;
    }

    // Short tool results are more important (likely the answer)
    if source == "tool_result" && content.len() < 500 {
        score = 0.6;
    }

    // Long tool results are less important (likely verbose output)
    if source == "tool_result" && content.len() > 5000 {
        score = 0.2;
    }

    // Content mentioning errors is important
    if content.contains("error") || content.contains("Error") || content.contains("failed") {
        score = score.max(0.8);
    }

    score
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_segment(id: &str, tokens: usize, importance: f64, source: &str) -> ContextSegment {
        ContextSegment {
            id: id.to_string(),
            content: "x".repeat(tokens * 4),
            estimated_tokens: tokens,
            importance,
            source: source.to_string(),
            pinned: false,
        }
    }

    #[test]
    fn no_collapse_under_budget() {
        let segments = vec![make_segment("1", 100, 0.5, "user")];
        let config = CollapseConfig {
            target_tokens: 200,
            ..Default::default()
        };
        let result = collapse_context(&segments, &config);
        assert_eq!(result.removed_count, 0);
        assert_eq!(result.retained.len(), 1);
    }

    #[test]
    fn importance_based_collapse() {
        let segments = vec![
            make_segment("low", 500, 0.1, "tool_result"),
            make_segment("med", 500, 0.5, "assistant"),
            make_segment("high", 500, 0.9, "user"),
            make_segment("recent1", 100, 0.5, "user"),
            make_segment("recent2", 100, 0.5, "assistant"),
        ];
        let config = CollapseConfig {
            target_tokens: 1200,
            strategy: CollapseStrategy::ImportanceBased,
            preserve_recent: 2,
            ..Default::default()
        };
        let result = collapse_context(&segments, &config);
        assert!(result.tokens_saved > 0);
        // The "low" importance segment should be removed first
        assert!(!result.retained.iter().any(|s| s.id == "low"));
    }

    #[test]
    fn recency_based_collapse() {
        let segments = vec![
            make_segment("old1", 500, 0.9, "user"),
            make_segment("old2", 500, 0.9, "assistant"),
            make_segment("recent1", 100, 0.5, "user"),
            make_segment("recent2", 100, 0.5, "assistant"),
        ];
        let config = CollapseConfig {
            target_tokens: 300,
            strategy: CollapseStrategy::RecencyBased,
            preserve_recent: 2,
            ..Default::default()
        };
        let result = collapse_context(&segments, &config);
        assert_eq!(result.retained.len(), 2);
        assert_eq!(result.retained[0].id, "recent1");
    }
}
