//! Model router: selects the best model based on task complexity,
//! available providers, and cost constraints.
//!
//! The router can operate in several modes:
//! - **Fixed**: Always use a specific model (default behavior).
//! - **Auto**: Select model based on estimated task complexity.
//! - **Hybrid**: Use local model for tool execution, cloud for reasoning.

use crate::model_profiles::{get_profile, ModelProfile};

/// Routing strategy for model selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingStrategy {
    /// Always use the specified model.
    Fixed(String),
    /// Automatically select model based on task complexity.
    Auto {
        /// Small/fast model for simple queries and tool execution.
        fast_model: String,
        /// Large/capable model for complex reasoning.
        capable_model: String,
    },
    /// Use local model for tools, cloud for reasoning.
    Hybrid {
        local_model: String,
        cloud_model: String,
    },
}

impl Default for RoutingStrategy {
    fn default() -> Self {
        Self::Fixed("qwen3:8b".to_string())
    }
}

/// Estimated complexity of a user query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskComplexity {
    /// Simple queries: greetings, short answers, basic lookups.
    Simple,
    /// Medium: file operations, code edits, single-tool tasks.
    Medium,
    /// Complex: multi-step reasoning, architecture decisions, large refactors.
    Complex,
}

/// Select the best model for a given query and routing strategy.
#[must_use]
pub fn select_model(strategy: &RoutingStrategy, query: &str) -> String {
    match strategy {
        RoutingStrategy::Fixed(model) => model.clone(),
        RoutingStrategy::Auto {
            fast_model,
            capable_model,
        } => {
            let complexity = estimate_complexity(query);
            match complexity {
                TaskComplexity::Simple => fast_model.clone(),
                TaskComplexity::Medium | TaskComplexity::Complex => capable_model.clone(),
            }
        }
        RoutingStrategy::Hybrid {
            local_model,
            cloud_model,
        } => {
            let complexity = estimate_complexity(query);
            match complexity {
                TaskComplexity::Simple | TaskComplexity::Medium => local_model.clone(),
                TaskComplexity::Complex => cloud_model.clone(),
            }
        }
    }
}

/// Estimate the complexity of a user query from surface-level heuristics.
#[must_use]
pub fn estimate_complexity(query: &str) -> TaskComplexity {
    let words = query.split_whitespace().count();
    let has_code_markers = query.contains("```")
        || query.contains("refactor")
        || query.contains("architect")
        || query.contains("implement")
        || query.contains("design");
    let has_multi_step = query.contains("then")
        || query.contains("after that")
        || query.contains("step by step")
        || query.contains("and also")
        || query.contains("first")
        || query.contains("finally");

    if words <= 5 && !has_code_markers {
        return TaskComplexity::Simple;
    }
    if has_code_markers || has_multi_step || words > 50 {
        return TaskComplexity::Complex;
    }
    TaskComplexity::Medium
}

/// Get the profile for the selected model, returning routing context.
#[must_use]
pub fn route_with_profile(strategy: &RoutingStrategy, query: &str) -> (String, ModelProfile) {
    let model = select_model(strategy, query);
    let profile = get_profile(&model);
    (model, profile)
}

/// Parse a routing strategy from a model string.
/// - "auto" → Auto with default fast/capable models
/// - "hybrid" → Hybrid with default local/cloud models
/// - anything else → Fixed
#[must_use]
pub fn parse_strategy(model_str: &str) -> RoutingStrategy {
    match model_str.trim().to_lowercase().as_str() {
        "auto" => RoutingStrategy::Auto {
            fast_model: "qwen2.5:1.5b".to_string(),
            capable_model: "qwen3:8b".to_string(),
        },
        "hybrid" => RoutingStrategy::Hybrid {
            local_model: "qwen3:8b".to_string(),
            cloud_model: "claude-sonnet-4-6".to_string(),
        },
        _ => RoutingStrategy::Fixed(model_str.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_queries_are_simple() {
        assert_eq!(estimate_complexity("hello"), TaskComplexity::Simple);
        assert_eq!(estimate_complexity("hi"), TaskComplexity::Simple);
        assert_eq!(estimate_complexity("what time is it"), TaskComplexity::Simple);
    }

    #[test]
    fn code_queries_are_complex() {
        assert_eq!(
            estimate_complexity("refactor the authentication module to use JWT"),
            TaskComplexity::Complex
        );
        assert_eq!(
            estimate_complexity("implement a REST API with pagination"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn multi_step_queries_are_complex() {
        assert_eq!(
            estimate_complexity("first read the config, then update the database, finally restart the service"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn medium_queries() {
        assert_eq!(
            estimate_complexity("what files are in the src directory"),
            TaskComplexity::Medium
        );
    }

    #[test]
    fn fixed_strategy_always_returns_same_model() {
        let strategy = RoutingStrategy::Fixed("llama3.1:8b".to_string());
        assert_eq!(select_model(&strategy, "hello"), "llama3.1:8b");
        assert_eq!(select_model(&strategy, "implement a database"), "llama3.1:8b");
    }

    #[test]
    fn auto_strategy_routes_by_complexity() {
        let strategy = RoutingStrategy::Auto {
            fast_model: "qwen2.5:0.5b".to_string(),
            capable_model: "qwen3:8b".to_string(),
        };
        assert_eq!(select_model(&strategy, "hi"), "qwen2.5:0.5b");
        assert_eq!(
            select_model(&strategy, "refactor the auth module"),
            "qwen3:8b"
        );
    }

    #[test]
    fn parse_strategy_handles_modes() {
        assert!(matches!(
            parse_strategy("auto"),
            RoutingStrategy::Auto { .. }
        ));
        assert!(matches!(
            parse_strategy("hybrid"),
            RoutingStrategy::Hybrid { .. }
        ));
        assert!(matches!(
            parse_strategy("qwen3:8b"),
            RoutingStrategy::Fixed(_)
        ));
    }
}
