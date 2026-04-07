//! Model profiles: auto-detected capabilities for Ollama models.
//!
//! Queries the Ollama `/api/show` endpoint to determine context window size,
//! model family, parameter count, and quantization level. Falls back to
//! sensible defaults when Ollama is unreachable or model metadata is missing.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Cached model profile with auto-detected capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    pub model: String,
    pub family: String,
    pub parameter_size: String,
    pub quantization: String,
    pub context_window: u32,
    pub supports_tools: bool,
    pub is_thinking_model: bool,
    pub max_tokens: u32,
}

impl ModelProfile {
    /// Create a default profile for an unknown model.
    #[must_use]
    pub fn default_for(model: &str) -> Self {
        let family = model.split(':').next().unwrap_or(model).to_string();
        let is_thinking = THINKING_FAMILIES.iter().any(|f| family.starts_with(f));
        let supports_tools = !NON_TOOL_FAMILIES.iter().any(|f| family.starts_with(f));
        Self {
            model: model.to_string(),
            family: family.clone(),
            parameter_size: "unknown".to_string(),
            quantization: "unknown".to_string(),
            context_window: 8192,
            supports_tools,
            is_thinking_model: is_thinking,
            max_tokens: 4096,
        }
    }

    /// Recommended `max_tokens` based on context window (25% of context).
    #[must_use]
    pub fn recommended_max_tokens(&self) -> u32 {
        (self.context_window / 4).clamp(1024, 32_000)
    }

    /// How many tokens to reserve for system prompt + tools + history.
    #[must_use]
    pub fn context_budget(&self) -> u32 {
        self.context_window.saturating_sub(self.recommended_max_tokens())
    }

    /// Should we trigger compaction? Returns true when estimated usage exceeds 80%.
    #[must_use]
    pub fn should_compact(&self, estimated_tokens: u32) -> bool {
        estimated_tokens > (self.context_window * 4) / 5
    }
}

const THINKING_FAMILIES: &[&str] = &["qwen3", "deepseek-r1"];
const NON_TOOL_FAMILIES: &[&str] = &[
    "starcoder2", "yi", "solar", "falcon3", "internlm2", "exaone3.5", "aya-expanse",
];

/// Query Ollama's `/api/show` endpoint for model metadata.
///
/// Returns `None` if Ollama is unreachable or the model doesn't exist.
#[must_use]
pub fn query_ollama_model_info(model: &str) -> Option<ModelProfile> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let response = client
        .post("http://localhost:11434/api/show")
        .json(&serde_json::json!({"name": model}))
        .send()
        .ok()?;

    if !response.status().is_success() {
        return None;
    }

    let body: serde_json::Value = response.json().ok()?;
    let model_info = body.get("model_info")?.as_object()?;
    let details = body.get("details")?.as_object()?;

    // Extract context length — look for any key containing "context_length"
    let context_window = model_info
        .iter()
        .find(|(k, _)| k.contains("context_length"))
        .and_then(|(_, v)| v.as_u64())
        .unwrap_or(8192)
        .try_into()
        .unwrap_or(8192u32);

    let family = details
        .get("family")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let parameter_size = details
        .get("parameter_size")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let quantization = details
        .get("quantization_level")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let is_thinking = THINKING_FAMILIES.iter().any(|f| family.starts_with(f));
    let supports_tools = !NON_TOOL_FAMILIES.iter().any(|f| family.starts_with(f));
    let max_tokens = (context_window / 4).clamp(1024, 32_000);

    Some(ModelProfile {
        model: model.to_string(),
        family,
        parameter_size,
        quantization,
        context_window,
        supports_tools,
        is_thinking_model: is_thinking,
        max_tokens,
    })
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTag>,
}

#[derive(Debug, Deserialize)]
struct OllamaTag {
    name: String,
}

/// List locally available Ollama model tags via `/api/tags`.
pub fn list_ollama_models() -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| error.to_string())?;

    let response = client
        .get("http://localhost:11434/api/tags")
        .send()
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }

    let mut models = response
        .json::<OllamaTagsResponse>()
        .map_err(|error| error.to_string())?
        .models
        .into_iter()
        .map(|model| model.name)
        .collect::<Vec<_>>();

    models.sort();
    models.dedup();
    Ok(models)
}

/// Thread-safe profile cache. Avoids re-querying Ollama for every turn.
static PROFILE_CACHE: std::sync::LazyLock<Mutex<BTreeMap<String, ModelProfile>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Return a cached profile when present, otherwise fall back immediately to a default profile.
#[must_use]
pub fn cached_profile_or_default(model: &str) -> ModelProfile {
    PROFILE_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(model)
        .cloned()
        .unwrap_or_else(|| ModelProfile::default_for(model))
}

/// Populate the profile cache for a model.
pub fn warm_profile_cache(model: &str) {
    let _ = get_profile(model);
}

/// Get or fetch a model profile. Caches results for the session.
#[must_use]
pub fn get_profile(model: &str) -> ModelProfile {
    let mut cache = PROFILE_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(profile) = cache.get(model) {
        return profile.clone();
    }

    let profile = query_ollama_model_info(model).unwrap_or_else(|| ModelProfile::default_for(model));
    cache.insert(model.to_string(), profile.clone());
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_detects_thinking_models() {
        let qwen3 = ModelProfile::default_for("qwen3:8b");
        assert!(qwen3.is_thinking_model);
        assert!(qwen3.supports_tools);

        let deepseek = ModelProfile::default_for("deepseek-r1:1.5b");
        assert!(deepseek.is_thinking_model);
    }

    #[test]
    fn default_profile_detects_non_tool_models() {
        let starcoder = ModelProfile::default_for("starcoder2:3b");
        assert!(!starcoder.supports_tools);
        assert!(!starcoder.is_thinking_model);

        let falcon = ModelProfile::default_for("falcon3:3b");
        assert!(!falcon.supports_tools);
    }

    #[test]
    fn recommended_max_tokens_is_quarter_of_context() {
        let mut profile = ModelProfile::default_for("test:1b");
        profile.context_window = 32768;
        assert_eq!(profile.recommended_max_tokens(), 8192);

        profile.context_window = 131072;
        assert_eq!(profile.recommended_max_tokens(), 32_000); // capped at 32k
    }

    #[test]
    fn should_compact_triggers_at_80_percent() {
        let mut profile = ModelProfile::default_for("test:1b");
        profile.context_window = 10_000;
        assert!(!profile.should_compact(7_000));
        assert!(profile.should_compact(8_001));
    }

    #[test]
    fn context_budget_leaves_room_for_output() {
        let mut profile = ModelProfile::default_for("test:1b");
        profile.context_window = 32768;
        let budget = profile.context_budget();
        assert_eq!(budget, 32768 - 8192);
    }
}
