//! Starter buddy command implementation.
//!
//! Mirrors the semantics of the translated ports
//! (`emberforge-ts`, `emberforge-go`, `emberforge-cpp`) so that
//! `/buddy` behaves identically across all four language implementations.
//!
//! The state file is JSON with fields: `next_index`, `companion`, `muted`.
//! Location resolution order:
//!   1. explicit path passed to `StarterBuddyState::new`
//!   2. `EMBER_BUDDY_STATE_PATH`
//!   3. `EMBER_CONFIG_HOME/buddy-state.json`
//!   4. `$HOME/.emberforge/buddy-state.json`
//!   5. `./.emberforge/buddy-state.json`

use std::env;
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

struct BuddyTemplate {
    name: &'static str,
    species: &'static str,
    personality: &'static str,
}

const BUDDY_TEMPLATES: &[BuddyTemplate] = &[
    BuddyTemplate {
        name: "Waddles",
        species: "duck",
        personality: "Quirky and easily amused. Leaves rubber duck debugging tips everywhere.",
    },
    BuddyTemplate {
        name: "Goosberry",
        species: "goose",
        personality: "Assertive and honks at bad code. Takes no prisoners in code reviews.",
    },
    BuddyTemplate {
        name: "Gooey",
        species: "blob",
        personality: "Adaptable and goes with the flow. Sometimes splits into two when confused.",
    },
    BuddyTemplate {
        name: "Whiskers",
        species: "cat",
        personality: "Independent and judgmental. Watches you type with mild disdain.",
    },
    BuddyTemplate {
        name: "Ember",
        species: "dragon",
        personality: "Fiery and passionate about architecture. Hoards good variable names.",
    },
    BuddyTemplate {
        name: "Inky",
        species: "octopus",
        personality: "Multitasker extraordinaire. Wraps tentacles around every problem at once.",
    },
    BuddyTemplate {
        name: "Hoots",
        species: "owl",
        personality:
            "Wise but verbose. Always says \"let me think about that\" for exactly 3 seconds.",
    },
    BuddyTemplate {
        name: "Waddleford",
        species: "penguin",
        personality: "Cool under pressure. Slides gracefully through merge conflicts.",
    },
    BuddyTemplate {
        name: "Shelly",
        species: "turtle",
        personality: "Patient and thorough. Believes slow and steady wins the deploy.",
    },
    BuddyTemplate {
        name: "Trailblazer",
        species: "snail",
        personality: "Methodical and leaves a trail of useful comments. Never rushes.",
    },
    BuddyTemplate {
        name: "Casper",
        species: "ghost",
        personality: "Ethereal and appears at the worst possible moments with spooky insights.",
    },
    BuddyTemplate {
        name: "Axie",
        species: "axolotl",
        personality: "Regenerative and cheerful. Recovers from any bug with a smile.",
    },
    BuddyTemplate {
        name: "Chill",
        species: "capybara",
        personality: "Zen master. Remains calm while everything around is on fire.",
    },
    BuddyTemplate {
        name: "Spike",
        species: "cactus",
        personality: "Prickly on the outside but full of good intentions. Thrives on neglect.",
    },
    BuddyTemplate {
        name: "Byte",
        species: "robot",
        personality: "Efficient and literal. Processes feedback in binary.",
    },
    BuddyTemplate {
        name: "Flops",
        species: "rabbit",
        personality: "Energetic and hops between tasks. Finishes before you start.",
    },
    BuddyTemplate {
        name: "Spore",
        species: "mushroom",
        personality: "Quietly insightful. Grows on you over time.",
    },
    BuddyTemplate {
        name: "Chonk",
        species: "chonk",
        personality: "Big, warm, and takes up the whole couch. Prioritizes comfort over elegance.",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarterBuddyCompanion {
    pub name: String,
    pub species: String,
    pub personality: String,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredCompanion {
    name: String,
    species: String,
    personality: String,
}

pub struct StarterBuddyState {
    path: PathBuf,
    next_index: usize,
    companion: Option<StoredCompanion>,
    muted: bool,
}

impl StarterBuddyState {
    /// Construct a new state handle. `None` resolves via env vars / HOME.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let resolved = path
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(default_state_path);
        let mut state = Self {
            path: resolved,
            next_index: 0,
            companion: None,
            muted: false,
        };
        state.load();
        state
    }

    fn load(&mut self) {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        if let Some(idx) = value.get("next_index").and_then(Value::as_u64) {
            self.next_index = usize::try_from(idx).unwrap_or(0);
        }
        self.muted = value
            .get("muted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.companion = value.get("companion").and_then(|companion| {
            if companion.is_null() {
                return None;
            }
            Some(StoredCompanion {
                name: companion.get("name")?.as_str()?.to_string(),
                species: companion.get("species")?.as_str()?.to_string(),
                personality: companion.get("personality")?.as_str()?.to_string(),
            })
        });
    }

    fn persist(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let companion_value = match &self.companion {
            Some(c) => json!({
                "name": c.name,
                "species": c.species,
                "personality": c.personality,
            }),
            None => Value::Null,
        };
        let snapshot = json!({
            "next_index": self.next_index,
            "companion": companion_value,
            "muted": self.muted,
        });
        if let Ok(mut text) = serde_json::to_string_pretty(&snapshot) {
            text.push('\n');
            let _ = fs::write(&self.path, text);
        }
    }

    fn materialize(&self) -> Option<StarterBuddyCompanion> {
        self.companion.as_ref().map(|c| StarterBuddyCompanion {
            name: c.name.clone(),
            species: c.species.clone(),
            personality: c.personality.clone(),
            muted: self.muted,
        })
    }

    #[must_use]
    pub fn current(&self) -> Option<StarterBuddyCompanion> {
        self.materialize()
    }

    /// Hatch a new companion when none exists. Returns `(created, companion)`.
    pub fn hatch(&mut self) -> (bool, StarterBuddyCompanion) {
        if let Some(existing) = self.materialize() {
            return (false, existing);
        }
        let template = &BUDDY_TEMPLATES[self.next_index % BUDDY_TEMPLATES.len()];
        self.next_index += 1;
        self.companion = Some(StoredCompanion {
            name: template.name.to_string(),
            species: template.species.to_string(),
            personality: template.personality.to_string(),
        });
        self.muted = false;
        self.persist();
        (true, self.materialize().expect("companion just hatched"))
    }

    /// Replace the existing companion with the next template.
    pub fn rehatch(&mut self) -> StarterBuddyCompanion {
        let template = &BUDDY_TEMPLATES[self.next_index % BUDDY_TEMPLATES.len()];
        self.next_index += 1;
        self.companion = Some(StoredCompanion {
            name: template.name.to_string(),
            species: template.species.to_string(),
            personality: template.personality.to_string(),
        });
        self.muted = false;
        self.persist();
        self.materialize().expect("companion just rehatched")
    }

    pub fn mute(&mut self) -> Option<StarterBuddyCompanion> {
        self.companion.as_ref()?;
        self.muted = true;
        self.persist();
        self.materialize()
    }

    pub fn unmute(&mut self) -> Option<StarterBuddyCompanion> {
        self.companion.as_ref()?;
        self.muted = false;
        self.persist();
        self.materialize()
    }
}

fn default_state_path() -> PathBuf {
    if let Ok(explicit) = env::var("EMBER_BUDDY_STATE_PATH") {
        if !explicit.trim().is_empty() {
            return PathBuf::from(explicit);
        }
    }
    if let Ok(config_home) = env::var("EMBER_CONFIG_HOME") {
        if !config_home.trim().is_empty() {
            return PathBuf::from(config_home).join("buddy-state.json");
        }
    }
    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".emberforge").join("buddy-state.json");
        }
    }
    PathBuf::from(".emberforge").join("buddy-state.json")
}

fn title_case_species(species: &str) -> String {
    let mut chars = species.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn render_companion(prefix: &str, companion: &StarterBuddyCompanion, note: Option<&str>) -> String {
    let status = if companion.muted { "muted" } else { "active" };
    let mut lines = vec![
        prefix.to_string(),
        format!("name: {}", companion.name),
        format!("species: {}", title_case_species(&companion.species)),
        format!("personality: {}", companion.personality),
        format!("status: {status}"),
    ];
    if let Some(note) = note {
        if !note.is_empty() {
            lines.push(note.to_string());
        }
    }
    lines.join("\n")
}

fn render_command(prefix: &str, lines: &[&str]) -> String {
    std::iter::once(prefix)
        .chain(lines.iter().copied())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Execute a `/buddy` invocation against the given state, returning the rendered
/// output. `payload` is the text after `/buddy ` (may be empty).
pub fn execute_buddy_command(state: &mut StarterBuddyState, payload: &str) -> String {
    let trimmed = payload.trim();
    let action = trimmed.split_whitespace().next().unwrap_or("");
    match action {
        "" => match state.current() {
            Some(companion) => render_companion(
                "[command] buddy",
                &companion,
                Some("commands: /buddy pet /buddy mute /buddy unmute /buddy hatch /buddy rehatch"),
            ),
            None => [
                "[command] buddy",
                "status: no companion",
                "tip: use /buddy hatch to get one",
            ]
            .join("\n"),
        },
        "hatch" => {
            if state.current().is_some() {
                return render_command(
                    "[command] buddy hatch",
                    &[
                        "status: companion already active",
                        "tip: use /buddy to inspect it or /buddy rehatch to replace it",
                    ],
                );
            }
            let (_, companion) = state.hatch();
            render_companion(
                "[command] buddy hatch",
                &companion,
                Some("note: starter buddy translation from claude-code-src"),
            )
        }
        "rehatch" => render_companion(
            "[command] buddy rehatch",
            &state.rehatch(),
            Some("note: previous companion replaced"),
        ),
        "pet" => match state.current() {
            Some(companion) => {
                let status = if companion.muted { "muted" } else { "active" };
                [
                    "[command] buddy pet".to_string(),
                    format!("reaction: {} purrs happily!", companion.name),
                    format!("status: {status}"),
                ]
                .join("\n")
            }
            None => [
                "[command] buddy pet",
                "status: no companion",
                "tip: use /buddy hatch to get one",
            ]
            .join("\n"),
        },
        "mute" => match state.current() {
            Some(companion) if companion.muted => render_command(
                "[command] buddy mute",
                &[
                    "status: already muted",
                    "tip: use /buddy unmute to bring it back",
                ],
            ),
            Some(_) => {
                let _ = state.mute();
                render_command(
                    "[command] buddy mute",
                    &[
                        "status: muted",
                        "note: companion will hide quietly until /buddy unmute",
                    ],
                )
            }
            None => [
                "[command] buddy mute",
                "status: no companion",
                "tip: use /buddy hatch to get one",
            ]
            .join("\n"),
        },
        "unmute" => match state.current() {
            Some(companion) if !companion.muted => {
                render_command("[command] buddy unmute", &["status: already active"])
            }
            Some(_) => {
                let _ = state.unmute();
                render_command(
                    "[command] buddy unmute",
                    &["status: active", "note: welcome back"],
                )
            }
            None => [
                "[command] buddy unmute",
                "status: no companion",
                "tip: use /buddy hatch to get one",
            ]
            .join("\n"),
        },
        other => [
            "[command] buddy".to_string(),
            format!("unsupported action: {other}"),
            "commands: /buddy pet /buddy mute /buddy unmute /buddy hatch /buddy rehatch"
                .to_string(),
        ]
        .join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn temp_state_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        env::temp_dir().join(format!("buddy-{label}-{nanos}.json"))
    }

    #[test]
    fn no_companion_prints_hint() {
        let _lock = env_lock();
        let path = temp_state_path("no-companion");
        let mut state = StarterBuddyState::new(Some(path.clone()));
        let output = execute_buddy_command(&mut state, "");
        assert!(output.contains("status: no companion"), "got:\n{output}");
        assert!(output.contains("/buddy hatch"), "got:\n{output}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn hatch_returns_first_template() {
        let _lock = env_lock();
        let path = temp_state_path("hatch");
        let mut state = StarterBuddyState::new(Some(path.clone()));
        let output = execute_buddy_command(&mut state, "hatch");
        assert!(output.contains("[command] buddy hatch"), "got:\n{output}");
        assert!(output.contains("name: Waddles"), "got:\n{output}");
        assert!(output.contains("species: Duck"), "got:\n{output}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn second_hatch_refuses_to_replace_existing_companion() {
        let _lock = env_lock();
        let path = temp_state_path("repeat-hatch");
        let mut state = StarterBuddyState::new(Some(path.clone()));
        execute_buddy_command(&mut state, "hatch");
        let output = execute_buddy_command(&mut state, "hatch");
        assert!(
            output.contains("status: companion already active"),
            "got:\n{output}"
        );
        assert!(output.contains("/buddy rehatch"), "got:\n{output}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn mute_pet_rehatch_lifecycle() {
        let _lock = env_lock();
        let path = temp_state_path("lifecycle");
        let mut state = StarterBuddyState::new(Some(path.clone()));
        execute_buddy_command(&mut state, "hatch");

        let mute_out = execute_buddy_command(&mut state, "mute");
        assert!(mute_out.contains("status: muted"), "got:\n{mute_out}");
        assert!(mute_out.contains("hide quietly"), "got:\n{mute_out}");

        let mute_again_out = execute_buddy_command(&mut state, "mute");
        assert!(
            mute_again_out.contains("status: already muted"),
            "got:\n{mute_again_out}"
        );

        let pet_out = execute_buddy_command(&mut state, "pet");
        assert!(
            pet_out.contains("reaction: Waddles purrs happily!"),
            "got:\n{pet_out}"
        );

        let unmute_out = execute_buddy_command(&mut state, "unmute");
        assert!(unmute_out.contains("status: active"), "got:\n{unmute_out}");
        assert!(unmute_out.contains("welcome back"), "got:\n{unmute_out}");

        let unmute_again_out = execute_buddy_command(&mut state, "unmute");
        assert!(
            unmute_again_out.contains("status: already active"),
            "got:\n{unmute_again_out}"
        );

        let rehatch_out = execute_buddy_command(&mut state, "rehatch");
        assert!(
            rehatch_out.contains("name: Goosberry"),
            "got:\n{rehatch_out}"
        );
        assert!(
            rehatch_out.contains("species: Goose"),
            "got:\n{rehatch_out}"
        );
        assert!(
            rehatch_out.contains("note: previous companion replaced"),
            "got:\n{rehatch_out}"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn state_persists_across_instances() {
        let _lock = env_lock();
        let path = temp_state_path("persist");
        {
            let mut state = StarterBuddyState::new(Some(path.clone()));
            execute_buddy_command(&mut state, "hatch");
            execute_buddy_command(&mut state, "rehatch");
        }
        let mut reopened = StarterBuddyState::new(Some(path.clone()));
        let out = execute_buddy_command(&mut reopened, "");
        assert!(out.contains("name: Goosberry"), "got:\n{out}");
        assert!(out.contains("species: Goose"), "got:\n{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn unsupported_action_is_reported() {
        let _lock = env_lock();
        let path = temp_state_path("unsupported");
        let mut state = StarterBuddyState::new(Some(path.clone()));
        let output = execute_buddy_command(&mut state, "feed");
        assert!(
            output.contains("unsupported action: feed"),
            "got:\n{output}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn env_path_is_respected_when_no_explicit_path() {
        let _lock = env_lock();
        let path = temp_state_path("env");
        // SAFETY: serialized under env_lock guard; test-only env mutation.
        env::set_var("EMBER_BUDDY_STATE_PATH", &path);
        let mut state = StarterBuddyState::new(None);
        execute_buddy_command(&mut state, "hatch");
        assert!(path.exists(), "env path should be written: {path:?}");
        env::remove_var("EMBER_BUDDY_STATE_PATH");
        let _ = fs::remove_file(&path);
    }
}
