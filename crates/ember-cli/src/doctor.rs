//! Diagnostic checks for validating the local LLM setup.
//!
//! The `/doctor` slash command runs quick or full diagnostics and caches results.

use std::collections::BTreeMap;
use std::io;
use std::{env, fs};
use std::path::PathBuf;

use runtime::model_profiles;
use runtime::Session;
use serde_json::json;

use crate::{
    build_runtime, build_system_prompt, chrono_now_iso8601, collect_tool_uses,
    collect_tool_results, discover_available_models,
    final_assistant_text, resolve_model_alias, truncate_for_summary,
    ConfigLoader, PermissionMode, DOCTOR_FAMILY_REPRESENTATIVES, VERSION,
};

// ---------------------------------------------------------------------------
//  Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DoctorCheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl DoctorCheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }

    fn severity(self) -> u8 {
        match self {
            Self::Pass | Self::Skip => 0,
            Self::Warn => 1,
            Self::Fail => 2,
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "pass" => Some(Self::Pass),
            "warn" => Some(Self::Warn),
            "fail" => Some(Self::Fail),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorCheckStatus,
    pub detail: String,
}

impl DoctorCheck {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "status": self.status.as_str(),
            "detail": self.detail,
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            name: value.get("name")?.as_str()?.to_string(),
            status: DoctorCheckStatus::from_str(value.get("status")?.as_str()?)?,
            detail: value.get("detail")?.as_str()?.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub scope: String,
    pub cache_key: String,
    pub ran_at: String,
    pub target: String,
    pub binary: String,
    pub status: DoctorCheckStatus,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "scope": self.scope,
            "cache_key": self.cache_key,
            "ran_at": self.ran_at,
            "target": self.target,
            "binary": self.binary,
            "status": self.status.as_str(),
            "checks": self.checks.iter().map(DoctorCheck::to_json).collect::<Vec<_>>(),
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            scope: value.get("scope")?.as_str()?.to_string(),
            cache_key: value.get("cache_key")?.as_str()?.to_string(),
            ran_at: value.get("ran_at")?.as_str()?.to_string(),
            target: value.get("target")?.as_str()?.to_string(),
            binary: value.get("binary")?.as_str()?.to_string(),
            status: DoctorCheckStatus::from_str(value.get("status")?.as_str()?)?,
            checks: value
                .get("checks")?
                .as_array()?
                .iter()
                .filter_map(DoctorCheck::from_json)
                .collect(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorCache {
    pub quick: Option<DoctorReport>,
    pub full: Option<DoctorReport>,
}

impl DoctorCache {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "version": 1,
            "quick": self.quick.as_ref().map(DoctorReport::to_json),
            "full": self.full.as_ref().map(DoctorReport::to_json),
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            quick: object.get("quick").and_then(DoctorReport::from_json),
            full: object.get("full").and_then(DoctorReport::from_json),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorMode {
    Quick,
    Full,
    Status,
    Reset,
}

// ---------------------------------------------------------------------------
//  Public API
// ---------------------------------------------------------------------------

pub fn run_doctor_cli(mode: Option<&str>, model: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", run_doctor(mode, model)?);
    Ok(())
}

pub fn run_doctor(mode: Option<&str>, model: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mode = parse_doctor_mode(mode).map_err(io::Error::other)?;
    let mut cache = load_doctor_cache()?;

    Ok(match mode {
        DoctorMode::Quick => {
            let cache_key = doctor_quick_cache_key(model);
            if let Some(report) = cache.quick.as_ref().filter(|report| report.cache_key == cache_key)
            {
                format_doctor_report(report, true)
            } else {
                println!("Running quick diagnostics for {model}...");
                let report = run_quick_doctor(model)?;
                cache.quick = Some(report.clone());
                save_doctor_cache(&cache)?;
                format_doctor_report(&report, false)
            }
        }
        DoctorMode::Full => {
            let models = doctor_full_inventory()?;
            let cache_key = doctor_full_cache_key(&models);
            if let Some(report) = cache.full.as_ref().filter(|report| report.cache_key == cache_key)
            {
                format_doctor_report(report, true)
            } else {
                println!(
                    "Running full family audit for {} representative model(s)...",
                    models.len()
                );
                let report = run_full_doctor(&models)?;
                cache.full = Some(report.clone());
                save_doctor_cache(&cache)?;
                format_doctor_report(&report, false)
            }
        }
        DoctorMode::Status => format_doctor_status(&cache, model),
        DoctorMode::Reset => {
            reset_doctor_cache()?;
            "Diagnostics cache cleared\n  Next             /doctor quick | /doctor full".to_string()
        }
    })
}

pub fn startup_doctor_hint(model: &str) -> Option<String> {
    let cache = load_doctor_cache().ok()?;
    let quick_key = doctor_quick_cache_key(model);
    match cache.quick {
        Some(report) if report.cache_key == quick_key => match report.status {
            DoctorCheckStatus::Pass | DoctorCheckStatus::Skip => None,
            DoctorCheckStatus::Warn | DoctorCheckStatus::Fail => Some(
                "Tip: /doctor status shows the cached setup issues for this model.".to_string(),
            ),
        },
        _ => Some(
            "Tip: run /doctor quick once to validate the local setup; the result is cached."
                .to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
//  Quick doctor
// ---------------------------------------------------------------------------

fn run_quick_doctor(model: &str) -> Result<DoctorReport, Box<dyn std::error::Error>> {
    let catalog = discover_available_models(model);
    let catalog_status = if catalog.ollama_status.starts_with("reachable") {
        DoctorCheckStatus::Pass
    } else {
        DoctorCheckStatus::Fail
    };
    let mut checks = vec![DoctorCheck {
        name: "local catalog".to_string(),
        status: catalog_status,
        detail: catalog.ollama_status,
    }];

    let generation_prompt = "Say hello in one short sentence.";
    let generation = run_doctor_turn(model, generation_prompt)?;
    let generation_text = final_assistant_text(&generation);
    checks.push(DoctorCheck {
        name: "one-shot prompt".to_string(),
        status: if generation_text.trim().is_empty() {
            DoctorCheckStatus::Fail
        } else {
            DoctorCheckStatus::Pass
        },
        detail: if generation_text.trim().is_empty() {
            "empty response".to_string()
        } else {
            truncate_for_summary(&generation_text, 90)
        },
    });

    let profile = model_profiles::get_profile(model);
    if profile.supports_tools {
        let tool_prompt = "Use the bash tool once to run: printf DOCTOR_TOOL_OK. After the tool finishes, reply with exactly DOCTOR_TOOL_OK.";
        let tool_summary = run_doctor_turn(model, tool_prompt)?;
        let tool_names = collect_tool_uses(&tool_summary)
            .into_iter()
            .filter_map(|entry| entry.get("name").and_then(|value| value.as_str()).map(str::to_string))
            .collect::<Vec<_>>();
        let tool_outputs = collect_tool_results(&tool_summary)
            .into_iter()
            .filter_map(|entry| entry.get("output").and_then(|value| value.as_str()).map(str::to_string))
            .collect::<Vec<_>>();
        let tool_results = tool_outputs.join("\n");
        let tool_text = final_assistant_text(&tool_summary);
        let used_bash = tool_names.iter().any(|name| name == "bash");
        let concise_tool_output = tool_outputs
            .iter()
            .find_map(|output| doctor_tool_stdout(output))
            .map(|stdout| format!("stdout={stdout}"));
        let saw_real_tool_result = tool_outputs.iter().any(|output| {
            doctor_tool_stdout(output)
                .is_some_and(|stdout| normalized_contains(&stdout, "DOCTOR_TOOL_OK"))
                || normalized_contains(output, "DOCTOR_TOOL_OK")
        });
        checks.push(DoctorCheck {
            name: "tool calling".to_string(),
            status: if used_bash && saw_real_tool_result {
                DoctorCheckStatus::Pass
            } else if used_bash {
                DoctorCheckStatus::Warn
            } else {
                DoctorCheckStatus::Fail
            },
            detail: if used_bash && !tool_results.trim().is_empty() {
                concise_tool_output
                    .unwrap_or_else(|| summarize_doctor_tool_output(&tool_results))
            } else if used_bash {
                format!(
                    "tools={} | {}",
                    tool_names.join(", "),
                    truncate_for_summary(&tool_text, 70)
                )
            } else {
                format!("no real tool call | {}", truncate_for_summary(&tool_text, 70))
            },
        });
    } else {
        checks.push(DoctorCheck {
            name: "tool calling".to_string(),
            status: DoctorCheckStatus::Skip,
            detail: format!("{} does not advertise tool support", profile.family),
        });
    }

    if profile.is_thinking_model {
        let thinking_summary = run_doctor_turn(model, "What is 2 + 2? Think step by step, then end with 4.")?;
        let thinking_text = final_assistant_text(&thinking_summary);
        checks.push(DoctorCheck {
            name: "thinking output".to_string(),
            status: if normalized_contains(&thinking_text, "4") {
                DoctorCheckStatus::Pass
            } else if thinking_text.trim().is_empty() {
                DoctorCheckStatus::Fail
            } else {
                DoctorCheckStatus::Warn
            },
            detail: if thinking_text.trim().is_empty() {
                "empty response".to_string()
            } else {
                truncate_for_summary(&thinking_text, 90)
            },
        });
    } else {
        checks.push(DoctorCheck {
            name: "thinking output".to_string(),
            status: DoctorCheckStatus::Skip,
            detail: format!("{} is not a thinking-family model", profile.family),
        });
    }

    Ok(DoctorReport {
        scope: "quick".to_string(),
        cache_key: doctor_quick_cache_key(model),
        ran_at: chrono_now_iso8601(),
        target: model.to_string(),
        binary: current_binary_label(),
        status: aggregate_doctor_status(&checks),
        checks,
    })
}

// ---------------------------------------------------------------------------
//  Full doctor
// ---------------------------------------------------------------------------

fn run_full_doctor(models: &[String]) -> Result<DoctorReport, Box<dyn std::error::Error>> {
    let checks = if models.is_empty() {
        vec![DoctorCheck {
            name: "family audit".to_string(),
            status: DoctorCheckStatus::Fail,
            detail: "no local Ollama models were reported".to_string(),
        }]
    } else {
        let mut checks = Vec::new();
        for model in models {
            checks.push(run_family_doctor_check(model)?);
        }
        checks
    };

    Ok(DoctorReport {
        scope: "full".to_string(),
        cache_key: doctor_full_cache_key(models),
        ran_at: chrono_now_iso8601(),
        target: format!("{} family representative(s)", models.len()),
        binary: current_binary_label(),
        status: aggregate_doctor_status(&checks),
        checks,
    })
}

fn run_family_doctor_check(model: &str) -> Result<DoctorCheck, Box<dyn std::error::Error>> {
    let profile = model_profiles::get_profile(model);
    let mut fragments = Vec::new();
    let mut status = DoctorCheckStatus::Pass;

    let generation = run_doctor_turn(model, "Say hello in one short sentence.")?;
    let generation_text = final_assistant_text(&generation);
    let generation_status = if generation_text.trim().is_empty() {
        DoctorCheckStatus::Fail
    } else {
        DoctorCheckStatus::Pass
    };
    status = worst_doctor_status(status, generation_status);
    fragments.push(format!(
        "generation {}",
        match generation_status {
            DoctorCheckStatus::Pass => "ok",
            DoctorCheckStatus::Warn => "soft",
            DoctorCheckStatus::Fail => "failed",
            DoctorCheckStatus::Skip => "skip",
        }
    ));

    if profile.supports_tools {
        let tool_summary = run_doctor_turn(
            model,
            "Use the bash tool once to run: printf DOCTOR_TOOL_OK. After the tool finishes, reply with exactly DOCTOR_TOOL_OK.",
        )?;
        let used_tool = collect_tool_uses(&tool_summary).into_iter().any(|entry| {
            entry
                .get("name")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == "bash")
        });
        let saw_real_tool_result = collect_tool_results(&tool_summary).into_iter().any(|entry| {
            entry
                .get("output")
                .and_then(|value| value.as_str())
                .is_some_and(|value| normalized_contains(value, "DOCTOR_TOOL_OK"))
        });
        let tool_status = if used_tool && saw_real_tool_result {
            DoctorCheckStatus::Pass
        } else if used_tool {
            DoctorCheckStatus::Warn
        } else {
            DoctorCheckStatus::Fail
        };
        status = worst_doctor_status(status, tool_status);
        fragments.push(format!(
            "tools {}",
            if used_tool && saw_real_tool_result {
                "ok"
            } else if used_tool {
                "soft"
            } else {
                "missed"
            }
        ));
    } else {
        fragments.push("tools skip".to_string());
    }

    if profile.is_thinking_model {
        let thinking = run_doctor_turn(model, "What is 2 + 2? Think step by step, then end with 4.")?;
        let thinking_text = final_assistant_text(&thinking);
        let thinking_status = if normalized_contains(&thinking_text, "4") {
            DoctorCheckStatus::Pass
        } else if thinking_text.trim().is_empty() {
            DoctorCheckStatus::Fail
        } else {
            DoctorCheckStatus::Warn
        };
        status = worst_doctor_status(status, thinking_status);
        fragments.push(format!(
            "thinking {}",
            match thinking_status {
                DoctorCheckStatus::Pass => "ok",
                DoctorCheckStatus::Warn => "soft",
                DoctorCheckStatus::Fail => "failed",
                DoctorCheckStatus::Skip => "skip",
            }
        ));
    } else {
        fragments.push("thinking skip".to_string());
    }

    Ok(DoctorCheck {
        name: model.to_string(),
        status,
        detail: format!("{} | family {}", fragments.join("; "), profile.family),
    })
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

fn run_doctor_turn(
    model: &str,
    prompt: &str,
) -> Result<runtime::TurnSummary, Box<dyn std::error::Error>> {
    let mut runtime = build_runtime(
        Session::new(),
        model.to_string(),
        build_system_prompt()?,
        true,
        false,
        None,
        PermissionMode::DangerFullAccess,
        None,
    )?;
    runtime
        .run_turn(prompt, None)
        .map_err(|error| io::Error::other(error.to_string()).into())
}

fn normalized_contains(text: &str, needle: &str) -> bool {
    let normalized_text = text
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect::<String>();
    let normalized_needle = needle
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect::<String>();
    normalized_text.contains(&normalized_needle)
}

fn summarize_doctor_tool_output(output: &str) -> String {
    doctor_tool_stdout(output)
        .map(|stdout| format!("stdout={stdout}"))
        .unwrap_or_else(|| truncate_for_summary(output, 90))
}

fn doctor_tool_stdout(output: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .and_then(|value| {
            value
                .get("stdout")
                .and_then(|stdout| stdout.as_str())
                .map(|stdout| stdout.trim().to_string())
                .filter(|stdout| !stdout.is_empty())
        })
}

fn aggregate_doctor_status(checks: &[DoctorCheck]) -> DoctorCheckStatus {
    checks.iter().fold(DoctorCheckStatus::Pass, |current, check| {
        worst_doctor_status(current, check.status)
    })
}

fn worst_doctor_status(current: DoctorCheckStatus, next: DoctorCheckStatus) -> DoctorCheckStatus {
    if next.severity() > current.severity() {
        next
    } else {
        current
    }
}

pub fn parse_doctor_mode(mode: Option<&str>) -> Result<DoctorMode, String> {
    let token = mode
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.split_whitespace().next())
        .unwrap_or("quick");

    match token {
        "quick" | "launch" | "startup" => Ok(DoctorMode::Quick),
        "full" | "families" | "all" => Ok(DoctorMode::Full),
        "status" | "show" => Ok(DoctorMode::Status),
        "reset" | "clear" => Ok(DoctorMode::Reset),
        other => Err(format!(
            "unsupported doctor mode '{other}'. Use quick, full, status, or reset."
        )),
    }
}

// ---------------------------------------------------------------------------
//  Formatting
// ---------------------------------------------------------------------------

fn format_doctor_report(report: &DoctorReport, from_cache: bool) -> String {
    let mut lines = vec![
        "Diagnostics".to_string(),
        format!("  Scope            {}", report.scope),
        format!("  Target           {}", report.target),
        format!("  Status           {}", report.status.badge()),
        format!("  Cache            {}", if from_cache { "hit" } else { "refreshed" }),
        format!("  Ran at           {}", report.ran_at),
        format!("  Binary           {}", report.binary),
        "Checks".to_string(),
    ];
    lines.extend(report.checks.iter().map(|check| {
        format!(
            "  {status:<5} {name:<16} {detail}",
            status = check.status.badge(),
            name = truncate_for_summary(&check.name, 16),
            detail = check.detail,
        )
    }));
    lines.push("Next".to_string());
    lines.push("  /doctor status   Show cached quick/full results".to_string());
    lines.push("  /doctor reset    Clear cached diagnostics and rerun later".to_string());
    if report.scope == "quick" {
        lines.push("  /doctor full     Run the slower family audit once and cache it".to_string());
    } else {
        lines.push("  /doctor quick    Re-check the current interactive model".to_string());
    }
    lines.join("\n")
}

pub fn format_doctor_status(cache: &DoctorCache, model: &str) -> String {
    let full_models = doctor_full_inventory().unwrap_or_default();
    let quick_key = doctor_quick_cache_key(model);
    let full_key = doctor_full_cache_key(&full_models);
    let mut lines = vec!["Diagnostics cache".to_string()];
    lines.push(render_doctor_cache_line(
        "Quick",
        cache.quick.as_ref(),
        Some(quick_key.as_str()),
    ));
    lines.push(render_doctor_cache_line(
        "Full",
        cache.full.as_ref(),
        Some(full_key.as_str()),
    ));
    lines.push("Next".to_string());
    lines.push("  /doctor quick    Run or reuse the current-model diagnostic".to_string());
    lines.push("  /doctor full     Run or reuse the cached family audit".to_string());
    lines.push("  /doctor reset    Clear cached diagnostics".to_string());
    lines.join("\n")
}

fn render_doctor_cache_line(
    label: &str,
    report: Option<&DoctorReport>,
    expected_key: Option<&str>,
) -> String {
    match report {
        Some(report) => {
            let freshness = if expected_key.is_some_and(|key| report.cache_key == key) {
                "current"
            } else {
                "stale"
            };
            format!(
                "  {label:<16} {status:<4} {freshness:<7} {target} @ {ran_at}",
                status = report.status.badge(),
                target = report.target,
                ran_at = report.ran_at,
            )
        }
        None => format!("  {label:<16} not yet run"),
    }
}

// ---------------------------------------------------------------------------
//  Cache persistence
// ---------------------------------------------------------------------------

fn doctor_full_inventory() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let installed = model_profiles::list_ollama_models().map_err(io::Error::other)?;
    let mut by_family = BTreeMap::<String, Vec<String>>::new();
    for model in installed {
        by_family
            .entry(model.split(':').next().unwrap_or(&model).to_string())
            .or_default()
            .push(model);
    }

    let mut selected = Vec::new();
    for (family, candidates) in by_family {
        if let Some((_, preferred)) = DOCTOR_FAMILY_REPRESENTATIVES
            .iter()
            .find(|(candidate_family, _)| *candidate_family == family)
        {
            if candidates.iter().any(|candidate| candidate == preferred) {
                selected.push((*preferred).to_string());
                continue;
            }
        }
        if let Some(first) = candidates.into_iter().next() {
            selected.push(first);
        }
    }
    Ok(selected)
}

fn doctor_quick_cache_key(model: &str) -> String {
    format!("quick:{VERSION}:{}", resolve_model_alias(model))
}

fn doctor_full_cache_key(models: &[String]) -> String {
    format!("full:{VERSION}:{}", models.join(","))
}

fn current_binary_label() -> String {
    env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown binary>".to_string())
}

fn doctor_cache_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = env::current_dir()?;
    let loader = ConfigLoader::default_for(&cwd);
    let config_home = loader.config_home().to_path_buf();
    fs::create_dir_all(&config_home)?;
    Ok(config_home.join("diagnostics.json"))
}

fn load_doctor_cache() -> Result<DoctorCache, Box<dyn std::error::Error>> {
    let path = doctor_cache_path()?;
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(DoctorCache::default());
    };
    let value = serde_json::from_str::<serde_json::Value>(&contents).unwrap_or_else(|_| json!({}));
    Ok(DoctorCache::from_json(&value).unwrap_or_default())
}

fn save_doctor_cache(cache: &DoctorCache) -> Result<(), Box<dyn std::error::Error>> {
    let path = doctor_cache_path()?;
    fs::write(path, serde_json::to_string_pretty(&cache.to_json())?)?;
    Ok(())
}

fn reset_doctor_cache() -> Result<(), Box<dyn std::error::Error>> {
    let path = doctor_cache_path()?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}
