//! `anima init` — guided first-run / onboarding wizard (E9 S9.1).
//!
//! Walks an operator through preflight → provider binding → identity bootstrap.
//! The wizard is idempotent and resumable: completed steps are persisted in
//! `~/.anima/<agent_id>/onboarding.json`, and re-running picks up where it left
//! off.
//!
//! Two modes:
//! - **Interactive** (stdout is a TTY): prompts the user at each step.
//! - **Non-interactive** (piped stdout, CI, `--non-interactive`): prints a
//!   suggested configuration file and exits without prompting, so scripts and
//!   tests can exercise the wizard path without blocking.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::doctor::{self, DoctorReport};

// ── Onboarding state ──────────────────────────────────────────────────────────

/// Persisted state written to `~/.anima/<agent_id>/onboarding.json`.
///
/// Fields are additive: a missing field means that step was not yet completed.
/// This ensures forward-compatibility when new steps are added.
#[derive(Debug, Clone, Default)]
pub struct OnboardingState {
    /// Version of the state schema (bumped when fields are added).
    pub schema_version: u32,
    /// True when the preflight (doctor) step passed without blocking issues.
    pub preflight_ok: bool,
    /// Chosen `ANIMA_BACKEND` value for cheap-local tier.
    pub cheap_local_backend: Option<String>,
    /// Chosen backend value for the mid-tier tier (E9 S9.5 per-tier dispatch).
    pub mid_tier_backend: Option<String>,
    /// Chosen `ANIMA_BACKEND` value for frontier tier.
    pub frontier_backend: Option<String>,
    /// Whether the identity bootstrap step has been run.
    pub identity_bootstrapped: bool,
    /// Human-readable name the user entered for themselves.
    pub operator_name: Option<String>,
    /// Whether the full wizard completed successfully.
    pub complete: bool,
}

/// Default path for the onboarding state file.
///
/// Resolves to `~/.anima/<agent_id>/onboarding.json`.
pub fn default_state_path(agent_id: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".anima")
        .join(agent_id)
        .join("onboarding.json")
}

/// Load an existing onboarding state from a JSON file, or return a fresh
/// default if the file does not exist yet.
pub fn load_state(path: &Path) -> io::Result<OnboardingState> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(OnboardingState::default()),
        Err(e) => return Err(e),
    };
    parse_state(&text)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "onboarding.json parse error"))
}

/// Persist the onboarding state atomically (write-to-tmp then rename).
pub fn save_state(path: &Path, state: &OnboardingState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serialise_state(state);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    // On Windows, `rename` fails if the destination already exists.
    #[cfg(target_os = "windows")]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ── JSON serialisation (manual — no extra crate dependency) ──────────────────

fn serialise_state(s: &OnboardingState) -> String {
    use serde_json::json;
    json!({
        "schema_version": s.schema_version,
        "preflight_ok": s.preflight_ok,
        "cheap_local_backend": s.cheap_local_backend,
        "mid_tier_backend": s.mid_tier_backend,
        "frontier_backend": s.frontier_backend,
        "identity_bootstrapped": s.identity_bootstrapped,
        "operator_name": s.operator_name,
        "complete": s.complete,
    })
    .to_string()
}

fn parse_state(text: &str) -> Option<OnboardingState> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(OnboardingState {
        schema_version: v["schema_version"].as_u64().unwrap_or(0) as u32,
        preflight_ok: v["preflight_ok"].as_bool().unwrap_or(false),
        cheap_local_backend: v["cheap_local_backend"].as_str().map(|s| s.to_string()),
        mid_tier_backend: v["mid_tier_backend"].as_str().map(|s| s.to_string()),
        frontier_backend: v["frontier_backend"].as_str().map(|s| s.to_string()),
        identity_bootstrapped: v["identity_bootstrapped"].as_bool().unwrap_or(false),
        operator_name: v["operator_name"].as_str().map(|s| s.to_string()),
        complete: v["complete"].as_bool().unwrap_or(false),
    })
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Detect whether stdout is connected to an interactive terminal.
fn is_interactive() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal()
}

/// Prompt the user and return the trimmed input line, or `None` on EOF.
fn prompt(msg: &str) -> Option<String> {
    print!("{msg}");
    io::stdout().flush().ok()?;
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Prompt with a default value; returns the default if the user just hits Enter.
fn prompt_with_default(msg: &str, default: &str) -> String {
    print!("{msg} [{}]: ", default);
    io::stdout().flush().ok();
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok();
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed
    }
}

// ── Identity interview I/O abstraction (E9 S9.2) ─────────────────────────────

/// Abstraction over the question/answer I/O used by the identity interview.
///
/// Decoupling the interview *logic* from real stdin makes
/// [`run_identity_interview`] unit-testable: a scripted [`ScriptedIo`] feeds a
/// canned transcript with no terminal, while [`StdinIo`] drives the real
/// prompts in interactive mode.
pub trait InterviewIo {
    /// Ask one question.  `default`, when present, is offered as the value used
    /// if the user just hits Enter.  Returns the answer, or `None` when the user
    /// declines / EOF and no default applies.
    fn ask(&mut self, prompt: &str, default: Option<&str>) -> Option<String>;
}

/// Stdin-backed [`InterviewIo`] reusing the existing [`prompt`] /
/// [`prompt_with_default`] helpers.
pub struct StdinIo;

impl InterviewIo for StdinIo {
    fn ask(&mut self, prompt_msg: &str, default: Option<&str>) -> Option<String> {
        match default {
            Some(d) => Some(prompt_with_default(prompt_msg, d)),
            None => prompt(&format!("{prompt_msg}: ")),
        }
    }
}

/// One identity interview question: the fact `key` it seeds, the `prompt` text,
/// and an optional default applied when the user provides no answer.
struct InterviewQuestion {
    key: &'static str,
    prompt: &'static str,
    default: Option<&'static str>,
}

/// The structured interview question set, aligned with the `onboarding-interview`
/// builtin skill.  Each entry seeds one identity fact.
fn interview_questions() -> Vec<InterviewQuestion> {
    vec![
        InterviewQuestion {
            key: "operator_name",
            prompt: "What's your name, and what would you like me to call you?",
            default: Some("Operator"),
        },
        InterviewQuestion {
            key: "working_hours",
            prompt: "What are your typical working hours (e.g. 09:00-18:00 UTC)?",
            default: Some("09:00-18:00 UTC"),
        },
        InterviewQuestion {
            key: "primary_goals",
            prompt: "What kinds of tasks do you expect to use me for most?",
            default: Some("general assistance"),
        },
        InterviewQuestion {
            key: "boundaries",
            prompt: "Are there any topics or capabilities you'd like me to avoid?",
            default: Some("none"),
        },
        InterviewQuestion {
            key: "preferred_channel",
            prompt: "Do you prefer brief and direct replies, or detailed explanations?",
            default: Some("brief and direct"),
        },
    ]
}

/// Run the conversational identity interview, writing each collected answer as
/// an identity fact through [`vita::IdentityMemory::set_fact`] +
/// [`flush_document`](vita::IdentityMemory::flush_document).
///
/// The function is I/O-agnostic: drive it with [`StdinIo`] for the real wizard
/// or a [`ScriptedIo`] in tests.  It returns the `(key, value)` pairs it set, in
/// order, so callers (and tests) can assert on exactly what was seeded.
///
/// Errors writing a single fact are logged but do not abort the interview, so a
/// transient write failure on one key never loses the remaining answers.
pub fn run_identity_interview(
    io: &mut dyn InterviewIo,
    identity: &mut vita::IdentityMemory,
    agent_id: &str,
) -> Vec<(String, String)> {
    let mut log = vita::AuditLog::new();
    let mut set: Vec<(String, String)> = Vec::new();

    for q in interview_questions() {
        let answer = io
            .ask(q.prompt, q.default)
            .or_else(|| q.default.map(str::to_string));
        let value = match answer {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => continue, // no answer and no default — skip this fact.
        };
        match identity.set_fact(q.key, &value, &mut log, agent_id) {
            Ok(()) => set.push((q.key.to_string(), value)),
            Err(e) => eprintln!("  warning: could not set {:?} ({e})", q.key),
        }
    }

    if let Err(e) = identity.flush_document() {
        eprintln!("  warning: could not persist identity ({e})");
    }
    set
}

// ── Wizard steps ──────────────────────────────────────────────────────────────

/// Print the initial banner.
fn print_banner() {
    println!("\n╔══════════════════════════════════════════════════════╗");
    println!("║          anima init — first-run wizard (E9)         ║");
    println!("╚══════════════════════════════════════════════════════╝\n");
    println!("This wizard will:");
    println!("  1. Check your hardware and available providers");
    println!("  2. Recommend backend bindings for each router tier");
    println!("  3. Seed your agent's identity memory");
    println!("  4. Write a ready-to-run config snippet\n");
}

/// Step 1: Run doctor and return the report.  Updates `state.preflight_ok`.
fn step_preflight(state: &mut OnboardingState, interactive: bool) -> DoctorReport {
    println!("━━━ Step 1 — Preflight\n");
    let report = doctor::run_doctor();
    doctor::print_report(&report);

    let blocking = report
        .providers
        .iter()
        .all(|p| !p.reachable && !p.configured);
    state.preflight_ok = !blocking;

    if blocking && interactive {
        println!(
            "⚠  No providers detected or configured.\n\
             Install Ollama (https://ollama.com) or set ANTHROPIC_API_KEY / OPENAI_API_KEY,\n\
             then re-run `anima-hosted init`.\n\
             Continuing with mock backend for now.\n"
        );
    }

    report
}

/// Step 2: Confirm or adjust provider tier bindings.  Updates `state.*_backend`.
fn step_providers(state: &mut OnboardingState, report: &DoctorReport, interactive: bool) {
    println!("━━━ Step 2 — Provider bindings\n");
    let rec = &report.recommendation;

    if interactive {
        println!("Recommended bindings based on your hardware:\n");
        println!("  cheap-local : {}", rec.cheap_local);
        println!("  mid-tier    : {}", rec.mid_tier);
        println!("  frontier    : {}\n", rec.frontier);

        let cheap = prompt_with_default(
            "Accept cheap-local recommendation?  (press Enter to accept, or type a value)",
            &rec.cheap_local,
        );
        let mid = prompt_with_default(
            "Accept mid-tier recommendation?  (press Enter to accept, or type a value)",
            &rec.mid_tier,
        );
        let frontier = prompt_with_default(
            "Accept frontier recommendation?  (press Enter to accept, or type a value)",
            &rec.frontier,
        );
        state.cheap_local_backend = Some(cheap);
        state.mid_tier_backend = Some(mid);
        state.frontier_backend = Some(frontier);
    } else {
        state.cheap_local_backend = Some(rec.cheap_local.clone());
        state.mid_tier_backend = Some(rec.mid_tier.clone());
        state.frontier_backend = Some(rec.frontier.clone());
        println!("  cheap-local  → {}", rec.cheap_local);
        println!("  mid-tier     → {}", rec.mid_tier);
        println!("  frontier     → {}", rec.frontier);
    }
    println!();
}

/// Step 3: Identity bootstrap.  Updates `state.operator_name` and
/// `state.identity_bootstrapped`.
fn step_identity(state: &mut OnboardingState, agent_id: &str, interactive: bool) {
    println!("━━━ Step 3 — Identity bootstrap\n");

    if state.identity_bootstrapped {
        println!("  (already completed — skipping)\n");
        return;
    }

    // Open (or create) the identity store at its canonical path.
    let path = vita::IdentityMemory::default_path(agent_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut identity =
        vita::IdentityMemory::open(&path).unwrap_or_else(|_| vita::IdentityMemory::in_memory());

    if interactive {
        println!(
            "Your agent stores a lightweight identity document so it can address you\n\
             correctly and respect your preferences across sessions.\n\
             I'll ask a few short questions — press Enter to accept a default.\n"
        );

        // Optional cortex-assisted warm opening (graceful fallback to the
        // deterministic line when no backend is configured).
        println!("  {}\n", cortex_opening_line(agent_id));

        let mut io = StdinIo;
        let set = run_identity_interview(&mut io, &mut identity, agent_id);

        // Mirror the operator name into onboarding state for the config step.
        if let Some((_, name)) = set.iter().find(|(k, _)| k == "operator_name") {
            state.operator_name = Some(name.clone());
        }
        state.identity_bootstrapped = true;

        println!("\n  Saved {} identity fact(s):", set.len());
        for (k, v) in &set {
            println!("    {k} = {v:?}");
        }
        println!("\n  You can update any of these later with:\n");
        println!("    anima-hosted identity set <key> \"<value>\"\n");
    } else {
        // Non-interactive: seed sensible, deterministic defaults so a scripted
        // run produces a complete identity document without prompting, and also
        // print the manual edit hints for operators who want to customise.
        let mut io = DefaultsIo;
        let set = run_identity_interview(&mut io, &mut identity, agent_id);
        if let Some((_, name)) = set.iter().find(|(k, _)| k == "operator_name") {
            state.operator_name = Some(name.clone());
        }
        state.identity_bootstrapped = true;

        println!(
            "  Non-interactive mode: seeded {} default identity fact(s).\n\
             \x20 Customise after first boot, e.g.:\n\n\
             \x20\x20  anima-hosted identity set operator_name \"Your Name\"\n\
             \x20\x20  anima-hosted identity set working_hours \"09:00-18:00 UTC\"\n",
            set.len()
        );
    }
}

/// Non-interactive [`InterviewIo`] that always returns each question's default
/// (or `None` when a question has no default).  Drives the deterministic
/// CI / scripted path so onboarding produces a complete identity document
/// without blocking on stdin.
struct DefaultsIo;

impl InterviewIo for DefaultsIo {
    fn ask(&mut self, _prompt: &str, default: Option<&str>) -> Option<String> {
        default.map(str::to_string)
    }
}

/// Returns a warm opening line for the identity interview.
///
/// OPTIONAL E9 S9.2 enhancement: when a chat backend is configured (the cortex
/// seam from Part A), this asks the cortex — primed with the
/// `onboarding-interview` skill body — to generate a one-line greeting.  It
/// always falls back to a deterministic line, so the interview never depends on
/// a live backend.
fn cortex_opening_line(agent_id: &str) -> String {
    const FALLBACK: &str = "Let's get you set up — a few quick questions and we're ready to go.";

    // Only attempt the cortex path when a live backend is explicitly configured;
    // otherwise the fixture would just echo a sentinel, so we skip it.
    if std::env::var("ANIMA_COMPAT_LIVE").as_deref() != Ok("1")
        || std::env::var("ANIMA_COMPAT_URL").is_err()
    {
        return FALLBACK.to_string();
    }

    // Load the onboarding-interview skill body as task framing.
    let task = skills::SkillRegistry::with_builtins()
        .load_body("onboarding-interview")
        .map(|b| {
            format!(
                "Using this onboarding guide, write ONE warm, single-sentence \
                 greeting to open the interview (no preamble):\n\n{}",
                b.instructions
            )
        })
        .unwrap_or_else(|_| {
            "Write ONE warm, single-sentence greeting to open a first-run setup interview."
                .to_string()
        });

    let registry = std::sync::Arc::new(crate::build_default_tool_registry());
    let dispatcher = crate::cortex::RegistryToolDispatcher::new(std::sync::Arc::clone(&registry));
    let bridge = crate::cortex::build_chat_cortex([], 2, 0);

    let request = vita::InvokeRequest {
        task_id: format!("onboarding-greeting-{agent_id}"),
        agent_id: agent_id.to_string(),
        description: task,
        tools: vec![],
        identity: serde_json::Value::Null,
        route_id: None,
        memory_scope: None,
        max_turns: Some(1),
        max_tool_calls: Some(0),
    };
    let mut audit = vita::AuditLog::new();
    use vita::CortexBackend;
    match bridge.invoke(request, &dispatcher, &mut audit) {
        Ok(result) if !result.output.trim().is_empty() => result.output.trim().to_string(),
        _ => FALLBACK.to_string(),
    }
}

/// Step 4: Print the generated environment config snippet.
fn step_config_snippet(state: &OnboardingState) {
    println!("━━━ Step 4 — Configuration snippet\n");

    let cheap = state
        .cheap_local_backend
        .as_deref()
        .map(infer_backend_env_value)
        .unwrap_or("mock");
    let mid = state
        .mid_tier_backend
        .as_deref()
        .map(infer_backend_env_value)
        .unwrap_or(cheap);
    let frontier = state
        .frontier_backend
        .as_deref()
        .map(infer_backend_env_value)
        .unwrap_or("mock");

    println!("  Add to your shell profile or `.env` file:\n");
    println!("  # AnimaOS — E9 onboarding config (per-tier router dispatch, S9.5)");
    // Keep the legacy single-backend selector for backward compatibility …
    println!("  export ANIMA_BACKEND={cheap}");
    // … and the per-tier overrides consumed by TierBackendChoices::from_env.
    println!("  export ANIMA_CHEAP_BACKEND={cheap}");
    println!("  export ANIMA_MID_BACKEND={mid}");
    println!("  export ANIMA_FRONTIER_BACKEND={frontier}");
    println!();
    println!("  Start the agent:");
    println!("  cargo run --bin anima-hosted -- serve");
    println!("  # or: docker compose up --build\n");
}

/// Resolve the per-tier backend choices for runtime dispatch (E9 S9.5).
///
/// Precedence, per tier (cheap-local / mid-tier / frontier):
/// 1. The per-tier env override (`ANIMA_CHEAP_BACKEND` / `ANIMA_MID_BACKEND` /
///    `ANIMA_FRONTIER_BACKEND`) when set — operator's explicit runtime choice.
/// 2. The value persisted by the `anima init` wizard in `onboarding.json`.
/// 3. The CI-hermetic default from [`TierBackendChoices::from_env`]
///    (mock unless a provider is configured/hinted).
///
/// This is the function `main.rs` calls to turn the wizard's "pick a model per
/// tier" choices into a live `vita::router::TierBackends` map.
pub fn resolve_tier_choices(agent_id: &str) -> llm_backends::TierBackendChoices {
    use llm_backends::{BackendKind, TierBackendChoices};

    // Env + defaults baseline (handles ANIMA_*_BACKEND overrides and fallbacks).
    let mut choices = TierBackendChoices::from_env();

    // Fold in saved wizard choices only where the operator did not set an
    // explicit per-tier env override (env always wins).
    let state = load_state(&default_state_path(agent_id)).unwrap_or_default();
    // Parse a stored choice: try the raw value first so canonical provider names
    // (lmstudio, vllm, …) round-trip exactly; fall back to the lossy
    // `infer_backend_env_value` mapping for descriptive recommendation strings
    // (e.g. "ollama (GGUF via Ollama)").
    let from_state = |stored: &Option<String>| -> Option<BackendKind> {
        let raw = stored.as_deref()?;
        BackendKind::parse(raw.trim()).or_else(|| BackendKind::parse(infer_backend_env_value(raw)))
    };

    if std::env::var("ANIMA_CHEAP_BACKEND").is_err() {
        if let Some(kind) = from_state(&state.cheap_local_backend) {
            choices.cheap_local = kind;
        }
    }
    if std::env::var("ANIMA_MID_BACKEND").is_err() {
        if let Some(kind) = from_state(&state.mid_tier_backend) {
            choices.mid_tier = kind;
        }
    }
    if std::env::var("ANIMA_FRONTIER_BACKEND").is_err() {
        if let Some(kind) = from_state(&state.frontier_backend) {
            choices.frontier = kind;
        }
    }

    choices
}

/// Map a recommendation string back to an `ANIMA_BACKEND` env value.
fn infer_backend_env_value(rec: &str) -> &'static str {
    if rec.contains("anthropic") {
        "anthropic"
    } else if rec.contains("openai") {
        "openai"
    } else if rec.contains("ollama") {
        "ollama"
    } else {
        "mock"
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the full onboarding wizard.
///
/// - `agent_id` identifies the agent state directory (`~/.anima/<agent_id>/`).
/// - `non_interactive` forces non-interactive mode regardless of TTY detection.
/// - `reset` discards any existing onboarding state and restarts the wizard.
pub fn run_init(agent_id: &str, non_interactive: bool, reset: bool) {
    let state_path = default_state_path(agent_id);
    let mut state = if reset {
        OnboardingState::default()
    } else {
        load_state(&state_path).unwrap_or_default()
    };
    state.schema_version = 1;

    if !reset && state.complete {
        println!(
            "\nOnboarding already complete for agent `{agent_id}`.\n\
             Re-run with `--reset` to start over, or edit identity with:\n\
             \x20\x20anima-hosted identity set <key> <value>\n"
        );
        return;
    }

    let interactive = !non_interactive && is_interactive();

    print_banner();

    // Step 1: preflight
    let report = step_preflight(&mut state, interactive);

    // Step 2: provider bindings
    step_providers(&mut state, &report, interactive);

    // Step 3: identity bootstrap
    step_identity(&mut state, agent_id, interactive);

    // Step 4: config snippet
    step_config_snippet(&state);

    // Finalise
    state.complete = true;
    if let Err(e) = save_state(&state_path, &state) {
        eprintln!("warning: could not save onboarding state ({e})");
    } else {
        println!(
            "✅ Onboarding complete. State saved to: {}\n",
            state_path.display()
        );
    }
    println!(
        "Your agent is ready. Run `anima-hosted serve` to wake it.\n\
         Reach the console at http://127.0.0.1:8088/ after it starts.\n"
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "anima_onboarding_test_{name}_{}.json",
            std::process::id()
        ))
    }

    // ── State serialisation round-trip ────────────────────────────────────────

    #[test]
    fn onboarding_state_round_trips_through_json() {
        let original = OnboardingState {
            schema_version: 1,
            preflight_ok: true,
            cheap_local_backend: Some("ollama".to_string()),
            mid_tier_backend: Some("lmstudio".to_string()),
            frontier_backend: Some("anthropic".to_string()),
            identity_bootstrapped: true,
            operator_name: Some("Alice".to_string()),
            complete: true,
        };
        let json = serialise_state(&original);
        let parsed = parse_state(&json).expect("must parse back");

        assert_eq!(parsed.schema_version, 1);
        assert!(parsed.preflight_ok);
        assert_eq!(parsed.cheap_local_backend.as_deref(), Some("ollama"));
        assert_eq!(parsed.mid_tier_backend.as_deref(), Some("lmstudio"));
        assert_eq!(parsed.frontier_backend.as_deref(), Some("anthropic"));
        assert!(parsed.identity_bootstrapped);
        assert_eq!(parsed.operator_name.as_deref(), Some("Alice"));
        assert!(parsed.complete);
    }

    #[test]
    fn partial_state_json_does_not_error() {
        let json = r#"{"schema_version": 1, "preflight_ok": true}"#;
        let state = parse_state(json).expect("must parse partial state");
        assert!(state.preflight_ok);
        assert!(!state.complete);
        assert!(state.cheap_local_backend.is_none());
    }

    #[test]
    fn empty_object_json_produces_default_state() {
        let state = parse_state("{}").expect("empty object must parse");
        assert!(!state.preflight_ok);
        assert!(!state.complete);
        assert_eq!(state.schema_version, 0);
    }

    #[test]
    fn invalid_json_returns_none() {
        assert!(parse_state("not json").is_none());
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    #[test]
    fn save_and_load_round_trips() {
        let path = tmp_path("save_load");
        let state = OnboardingState {
            schema_version: 1,
            preflight_ok: true,
            cheap_local_backend: Some("ollama".to_string()),
            mid_tier_backend: Some("lmstudio".to_string()),
            frontier_backend: Some("anthropic".to_string()),
            identity_bootstrapped: false,
            operator_name: None,
            complete: false,
        };
        save_state(&path, &state).expect("save must succeed");
        let loaded = load_state(&path).expect("load must succeed");
        assert_eq!(loaded.cheap_local_backend.as_deref(), Some("ollama"));
        assert_eq!(loaded.mid_tier_backend.as_deref(), Some("lmstudio"));
        assert_eq!(loaded.frontier_backend.as_deref(), Some("anthropic"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_state_returns_default_when_file_missing() {
        let path = tmp_path("nonexistent_definitely_does_not_exist");
        let state = load_state(&path).expect("missing file should return default, not error");
        assert!(!state.complete);
        assert!(!state.preflight_ok);
    }

    // ── infer_backend_env_value ───────────────────────────────────────────────

    #[test]
    fn infer_backend_env_value_maps_ollama() {
        assert_eq!(
            infer_backend_env_value("ollama (GGUF via Ollama)"),
            "ollama"
        );
    }

    #[test]
    fn infer_backend_env_value_maps_anthropic() {
        assert_eq!(infer_backend_env_value("anthropic"), "anthropic");
    }

    #[test]
    fn infer_backend_env_value_maps_openai() {
        assert_eq!(infer_backend_env_value("openai"), "openai");
    }

    #[test]
    fn infer_backend_env_value_falls_back_to_mock() {
        assert_eq!(infer_backend_env_value("mock (no local provider)"), "mock");
    }

    // ── Identity interview (E9 S9.2) ──────────────────────────────────────────

    /// Scripted [`InterviewIo`] that replays a fixed list of answers in order,
    /// then yields each question's default (so the interview always completes).
    struct ScriptedIo {
        answers: std::collections::VecDeque<String>,
    }

    impl ScriptedIo {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl InterviewIo for ScriptedIo {
        fn ask(&mut self, _prompt: &str, default: Option<&str>) -> Option<String> {
            self.answers
                .pop_front()
                .or_else(|| default.map(str::to_string))
        }
    }

    #[test]
    fn run_identity_interview_writes_expected_facts() {
        let mut identity = vita::IdentityMemory::in_memory();
        let mut io = ScriptedIo::new(&[
            "Alice",
            "08:00-16:00 CET",
            "research and writing",
            "no legal advice",
            "detailed explanations",
        ]);
        let set = run_identity_interview(&mut io, &mut identity, "anima");

        assert_eq!(set.len(), 5);
        assert_eq!(identity.get_fact("operator_name"), Some("Alice"));
        assert_eq!(identity.get_fact("working_hours"), Some("08:00-16:00 CET"));
        assert_eq!(
            identity.get_fact("primary_goals"),
            Some("research and writing")
        );
        assert_eq!(identity.get_fact("boundaries"), Some("no legal advice"));
        assert_eq!(
            identity.get_fact("preferred_channel"),
            Some("detailed explanations")
        );
    }

    #[test]
    fn run_identity_interview_uses_defaults_for_empty_answers() {
        // No scripted answers → every question falls back to its default.
        let mut identity = vita::IdentityMemory::in_memory();
        let mut io = ScriptedIo::new(&[]);
        let set = run_identity_interview(&mut io, &mut identity, "anima");

        assert_eq!(set.len(), 5);
        assert_eq!(identity.get_fact("operator_name"), Some("Operator"));
        assert_eq!(identity.get_fact("working_hours"), Some("09:00-18:00 UTC"));
        assert_eq!(
            identity.get_fact("preferred_channel"),
            Some("brief and direct")
        );
    }

    #[test]
    fn run_identity_interview_is_idempotent() {
        // Running twice with the same answers leaves the same facts (last write
        // wins; no duplication or error).
        let mut identity = vita::IdentityMemory::in_memory();
        let answers = ["Bob", "all hours", "ops", "none", "brief and direct"];
        let first = run_identity_interview(&mut ScriptedIo::new(&answers), &mut identity, "anima");
        let second = run_identity_interview(&mut ScriptedIo::new(&answers), &mut identity, "anima");

        assert_eq!(first, second);
        assert_eq!(identity.get_fact("operator_name"), Some("Bob"));
        assert_eq!(identity.get_fact("primary_goals"), Some("ops"));
    }

    #[test]
    fn defaults_io_yields_each_default() {
        let mut io = DefaultsIo;
        assert_eq!(io.ask("q", Some("d")).as_deref(), Some("d"));
        assert_eq!(io.ask("q", None), None);
    }

    #[test]
    fn non_interactive_identity_step_does_not_panic() {
        // Drive the non-interactive branch end-to-end against a temp HOME so it
        // writes to an isolated identity store and seeds deterministic defaults.
        let tmp_home =
            std::env::temp_dir().join(format!("anima_init_identity_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_home);
        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &tmp_home);

        let mut state = OnboardingState::default();
        step_identity(&mut state, "test-agent", /* interactive = */ false);
        assert!(state.identity_bootstrapped);

        // Restore env + clean up.
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp_home);
    }

    // ── State path helper ─────────────────────────────────────────────────────

    #[test]
    fn default_state_path_contains_agent_id_and_filename() {
        let p = default_state_path("my-agent");
        let s = p.to_string_lossy();
        assert!(s.contains("my-agent"));
        assert!(s.ends_with("onboarding.json"));
    }
}
