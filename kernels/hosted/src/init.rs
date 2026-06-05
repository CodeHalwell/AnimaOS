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
        let frontier = prompt_with_default(
            "Accept frontier recommendation?  (press Enter to accept, or type a value)",
            &rec.frontier,
        );
        state.cheap_local_backend = Some(cheap);
        state.frontier_backend = Some(frontier);
    } else {
        state.cheap_local_backend = Some(rec.cheap_local.clone());
        state.frontier_backend = Some(rec.frontier.clone());
        println!("  cheap-local  → {}", rec.cheap_local);
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

    if interactive {
        println!(
            "Your agent stores a lightweight identity document so it can address you\n\
             correctly and respect your preferences across sessions.\n"
        );
        let name = prompt("  What is your name (or how should the agent address you)? ")
            .unwrap_or_else(|| "Operator".to_string());

        // Write fact through the existing IdentityMemory path.
        let path = vita::IdentityMemory::default_path(agent_id);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut identity =
            vita::IdentityMemory::open(&path).unwrap_or_else(|_| vita::IdentityMemory::in_memory());
        let mut log = vita::AuditLog::new();
        let _ = identity.set_fact("operator_name", &name, &mut log, agent_id);
        if let Err(e) = identity.flush_document() {
            eprintln!("  warning: could not persist identity ({e})");
        }

        state.operator_name = Some(name.clone());
        state.identity_bootstrapped = true;
        println!("\n  Saved: operator_name = {name:?}\n");
        println!("  You can update this later with:\n");
        println!("    anima-hosted identity set operator_name \"your name\"\n");
    } else {
        // Non-interactive: just document what the user should do.
        println!(
            "  Non-interactive mode: seed identity manually after first boot:\n\n\
             \x20\x20  anima-hosted identity set operator_name \"Your Name\"\n\
             \x20\x20  anima-hosted identity set working_hours \"09:00-18:00 UTC\"\n"
        );
        state.identity_bootstrapped = true;
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
    let frontier = state
        .frontier_backend
        .as_deref()
        .map(infer_backend_env_value)
        .unwrap_or("mock");

    println!("  Add to your shell profile or `.env` file:\n");
    println!("  # AnimaOS — E9 onboarding config");
    println!("  export ANIMA_BACKEND={cheap}");
    if frontier != cheap {
        println!("  # For frontier routing, the router will use: {frontier}");
    }
    println!();
    println!("  Start the agent:");
    println!("  cargo run --bin anima-hosted -- serve");
    println!("  # or: docker compose up --build\n");
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
            frontier_backend: Some("anthropic".to_string()),
            identity_bootstrapped: false,
            operator_name: None,
            complete: false,
        };
        save_state(&path, &state).expect("save must succeed");
        let loaded = load_state(&path).expect("load must succeed");
        assert_eq!(loaded.cheap_local_backend.as_deref(), Some("ollama"));
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

    // ── State path helper ─────────────────────────────────────────────────────

    #[test]
    fn default_state_path_contains_agent_id_and_filename() {
        let p = default_state_path("my-agent");
        let s = p.to_string_lossy();
        assert!(s.contains("my-agent"));
        assert!(s.ends_with("onboarding.json"));
    }
}
