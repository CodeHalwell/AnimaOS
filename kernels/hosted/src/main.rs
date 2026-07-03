//! Linux process emulation entry point — boots the somatic stack in-process
//! for local rapid CI and developer experimentation.
//!
//! # Backend selection (E1.3)
//!
//! The hosted kernel selects an LLM backend at startup via the
//! `ANIMA_BACKEND` environment variable.  Recognised values:
//!
//! | Value         | Backend                              |
//! |---------------|--------------------------------------|
//! | `anthropic`   | Anthropic Claude (fixture mode)      |
//! | `openai`      | OpenAI GPT (fixture mode)            |
//! | `mock`        | Built-in deterministic mock          |
//! | _(any other)_ | Falls back to `mock`                 |
//!
//! Example: `ANIMA_BACKEND=anthropic cargo run --bin anima-hosted`
//!
//! # Phase 1 M1.6 demo
//!
//! Two concurrent agents execute through a shared backend; their audit logs
//! are printed to stdout on completion.
//!
//! # `anima why` subcommand (E5.2)
//!
//! Running `cargo run --bin anima-hosted -- why` exercises the Striatal Gate on
//! a sample of representative events and prints the most recent `GateDecision`
//! audit entry in human-readable form, satisfying E5.2 exit criterion 3.
//!
//! # `anima identity` subcommand (E5.5)
//!
//! ```text
//! cargo run --bin anima-hosted -- identity show [<key>]
//! cargo run --bin anima-hosted -- identity set <key> <value>
//! ```
//!
//! Inspects and edits the agent's identity memory stored in
//! `~/.anima/anima/identity.json`.  Every `set` is recorded in an in-process
//! audit log that is printed on exit, satisfying E5.5 exit criterion 1.
//!
//! # `anima doctor` subcommand (E9 S9.3)
//!
//! Running `cargo run --bin anima-hosted -- doctor` detects GPU capabilities,
//! available RAM, local inference providers (Ollama, LM Studio, vLLM, llama.cpp),
//! and configured API keys, then prints a tier recommendation.
//!
//! # `anima ask` subcommand (E7 S7.4 — cortex invocation seam)
//!
//! Running `cargo run --bin anima-hosted -- ask "<task>"` builds a
//! [`vita::InvokeRequest`] from the task text, the default tool registry, and
//! the agent's identity memory, then drives it through a
//! [`vita::ChatCortexBridge`].  The chat backend is a CI-safe fixture by default
//! (text-only, no tool dispatch); a live tool-calling OpenAI-compatible backend
//! is opt-in via `ANIMA_COMPAT_LIVE=1` + `ANIMA_COMPAT_URL`.  Tool calls the
//! cortex emits are routed back through the registry.
//!
//! ```text
//! cargo run --bin anima-hosted -- ask "summarise the AnimaOS project"
//! ```
//!
//! # `anima init` subcommand (E9 S9.1)
//!
//! Running `cargo run --bin anima-hosted -- init` runs the guided first-run
//! wizard: preflight → provider binding → identity bootstrap → config snippet.
//! State is persisted in `~/.anima/anima/onboarding.json` so the wizard is
//! idempotent and re-runs skip completed steps.
//!
//! ```text
//! cargo run --bin anima-hosted -- init
//! cargo run --bin anima-hosted -- init --non-interactive   # CI / scripted
//! ```

mod cortex;
mod doctor;
mod init;
mod syscall_router;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use console::{Console, ServerConfig};
use interoception::{HomeostaticMonitor, InteroceptiveSensorBundle};
use llm_backends::factory::BackendFactory;
use memory::VirtualContextManager;
use scheduler::Task;
use senses::{HumanGuidance, SensoryBridge};
// E11: skill crate referenced inside cmd_skills via use statements
use alerts::{
    AlertCondition, AlertRule, AlertRuleRegistry, AlertSeverity, ComparisonOp, MetricField,
};
use knowledge_graph::{Entity, EntityKind, KnowledgeGraph, Relation, RelationKind};
use metrics::{aggregate, registry_from_audit, render_text_report};
use vita::gate::Gate;
use vita::{
    record_gate_decision, somatic_execution_loop, AuditEntry, AuditLog, EventFeatures,
    GateOverride, HomeostaticSignals, IdentityMemory, LifecycleConfig, LifecycleManager,
    SemanticClass, ThresholdGate,
};

/// Exit status recorded by CLI error paths; `main` exits with this after the
/// dispatched subcommand returns (0 = success, 1 = runtime failure,
/// 2 = usage error — the conventional CLI meanings).
static CLI_EXIT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Record a non-zero exit status for the current CLI invocation without
/// unwinding — later, more severe codes do not downgrade earlier ones.
mod audit_view;
mod commands;

fn cli_fail(code: i32) {
    let _ = CLI_EXIT.fetch_max(code, std::sync::atomic::Ordering::Relaxed);
}

fn cli_exit() -> ! {
    std::process::exit(CLI_EXIT.load(std::sync::atomic::Ordering::Relaxed));
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::as_mut(&mut future).poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// `true` when an environment flag is set to a truthy value (`1`/`true`/`yes`/`on`).
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Build the default tool registry surfaced by the hosted kernel (E7 + Wave-1).
///
/// Registers the deterministic, CI-safe tool set: `web-search` (fixture
/// provider) alongside the actuators browser family (`browser` / `browse` /
/// `extract`) backed by [`MockBrowserDriver`].  Live drivers (SearXNG,
/// Playwright) remain opt-in behind their own env/feature gates and are never
/// wired here, so this path stays hermetic.
pub(crate) fn build_default_tool_registry() -> praxis::ToolRegistry {
    use actuators::browser::{
        BrowserExtractTool, BrowserNavigateTool, BrowserReadTextTool, MockBrowserDriver, MockPage,
    };
    use actuators::web_search::{SearchResult, WebSearchTool};
    use actuators::EgressGuard;

    let registry = praxis::ToolRegistry::new();

    // web-search over a deterministic fixture provider (no network).
    registry.register(WebSearchTool::with_fixture(vec![SearchResult {
        title: "AnimaOS".to_string(),
        url: "https://example.com/animaos".to_string(),
        snippet: "A self-preserving agent operating system.".to_string(),
    }]));

    // Browser family: each tool gets its own MockBrowserDriver seeded with the
    // same canned page (the fixture driver is stateless, so per-tool instances
    // are equivalent and keep the tools exercisable offline).  Each tool gets the
    // default HTTPS-only egress guard (defence-in-depth alongside the dispatch
    // egress screen).
    let canned_url = "https://example.com/animaos";
    let mock_page = MockPage::new("AnimaOS", "AnimaOS is a self-preserving agent OS.")
        .with_extraction("h1", vec!["AnimaOS".to_string()]);
    registry.register(BrowserNavigateTool::new(
        MockBrowserDriver::new().with_page(canned_url, mock_page.clone()),
        EgressGuard::default(),
    ));
    registry.register(BrowserReadTextTool::new(
        MockBrowserDriver::new().with_page(canned_url, mock_page.clone()),
        EgressGuard::default(),
    ));
    registry.register(BrowserExtractTool::new(
        MockBrowserDriver::new().with_page(canned_url, mock_page),
        EgressGuard::default(),
    ));

    registry
}

fn build_agent(
    agent_id: &str,
    policy: &str,
    backend: Arc<dyn scheduler::LlmBackend>,
    tasks: Vec<Task>,
    max_iterations: u32,
) -> LifecycleManager {
    let mut manager = LifecycleManager::new(
        agent_id,
        SensoryBridge::new(HumanGuidance::new(policy)),
        VirtualContextManager::with_capacity(0, 4096),
        LifecycleConfig { max_context: 4096 },
        HumanGuidance::new("boot"),
        backend,
        Some(max_iterations),
    );
    for task in tasks {
        manager.agenda.push(task);
    }
    manager
}

fn run_agent(mut manager: LifecycleManager) -> LifecycleManager {
    let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0);
    block_on(somatic_execution_loop(&mut manager, &monitor)).expect("lifecycle loop failed");
    manager
}

/// Prints the top-level usage summary for `anima-hosted help` (also `--help` /
/// `-h`): one aligned line per subcommand, header, and a docs pointer.
fn print_cli_help() {
    println!("anima-hosted — the AnimaOS hosted agent");
    println!();
    println!("usage: anima-hosted <command> [args...]");
    println!();
    println!("commands:");
    for (cmd, desc) in [
        (
            "why",
            "explain recent gate decisions with live interoceptive signals",
        ),
        ("identity", "show or edit identity-memory facts"),
        (
            "skills",
            "manage the skill registry (list, register, promote, ...)",
        ),
        ("tools", "list and exercise the registered tools"),
        (
            "ask|cortex",
            "run a one-shot task through the cortex bridge",
        ),
        ("serve", "start the agent with the operator console server"),
        ("digest", "print an activity digest from the audit log"),
        ("snapshot", "write a versioned agent-state snapshot"),
        ("replay", "replay past gate decisions from the audit log"),
        (
            "users",
            "manage per-user profiles, trust tiers, and consent",
        ),
        ("workspace", "manage multi-user workspaces"),
        ("jobs", "manage scheduled jobs in the cron engine"),
        ("doctor", "run environment preflight checks"),
        ("init", "guided first-run setup wizard"),
        ("quota", "inspect per-user quota usage and policy"),
        ("config", "show, validate, or initialise the runtime config"),
        ("sessions", "manage conversation history"),
        ("data", "export, delete, and consent-check personal data"),
        ("feedback", "record and analyse response-quality feedback"),
        ("stats", "print performance analytics reports"),
        ("cache", "inspect, clear, or warm the tool response cache"),
        ("graph", "manage the knowledge graph"),
        (
            "metrics",
            "aggregate audit metrics (text, json, prometheus)",
        ),
        ("alert", "manage metric alert rules"),
        ("webhook", "manage outbound webhook endpoints"),
        (
            "diagnose",
            "run diagnostic health checks over the audit log",
        ),
        ("demo", "run the two-agent somatic-loop demo"),
    ] {
        println!("  {cmd:<11} {desc}");
    }
    println!();
    println!("See docs/getting-started.md for a full walkthrough.");
}

fn main() {
    // Rust ignores SIGPIPE, so `println!` panics with a backtrace when stdout
    // closes early (`anima-hosted help | head`). Die quietly with the
    // conventional shell status (128 + SIGPIPE = 141) instead, without
    // `unsafe` signal handling — the workspace quarantine stays intact.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if msg.contains("Broken pipe") {
            std::process::exit(141);
        }
        default_hook(info);
    }));

    // ── Subcommand dispatch ───────────────────────────────────────────────────
    // A single `match` over the leading argument routes every subcommand. Arms
    // that handle a command run it and `cli_exit()` (which diverges, so they
    // unify with the fall-through arms); `demo`/no-argument fall through to the
    // two-agent demo below, and an unrecognised command exits 2. `rest` is the
    // argument tail passed to each handler — `get(1..)` keeps it panic-free when
    // no subcommand was supplied.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("why") => {
            commands::cmd_why();
            cli_exit();
        }
        Some("identity") => {
            commands::cmd_identity(rest);
            cli_exit();
        }
        Some("skills") => {
            commands::cmd_skills(rest);
            cli_exit();
        }
        Some("tools") => {
            commands::cmd_tools(rest);
            cli_exit();
        }
        Some("ask") | Some("cortex") => {
            commands::cmd_ask(rest);
            cli_exit();
        }
        Some("serve") => {
            commands::cmd_serve();
            cli_exit();
        }
        Some("digest") => {
            commands::cmd_digest(rest);
            cli_exit();
        }
        Some("snapshot") => {
            commands::cmd_snapshot(rest);
            cli_exit();
        }
        Some("replay") => {
            commands::cmd_replay(rest);
            cli_exit();
        }
        Some("users") => {
            commands::cmd_users(rest);
            cli_exit();
        }
        Some("workspace") => {
            commands::cmd_workspace(rest);
            cli_exit();
        }
        Some("jobs") => {
            commands::cmd_jobs(rest);
            cli_exit();
        }
        Some("doctor") => {
            let report = doctor::run_doctor();
            doctor::print_report(&report);
            cli_exit();
        }
        Some("init") => {
            let non_interactive = args.iter().any(|a| a == "--non-interactive");
            let reset = args.iter().any(|a| a == "--reset");
            init::run_init("anima", non_interactive, reset);
            cli_exit();
        }
        Some("quota") => {
            commands::cmd_quota(rest);
            cli_exit();
        }
        Some("config") => {
            commands::cmd_config(rest);
            cli_exit();
        }
        Some("sessions") => {
            commands::cmd_sessions(rest);
            cli_exit();
        }
        Some("data") => {
            commands::cmd_data(rest);
            cli_exit();
        }
        Some("feedback") => {
            commands::cmd_feedback(rest);
            cli_exit();
        }
        Some("stats") => {
            commands::cmd_stats(rest);
            cli_exit();
        }
        Some("cache") => {
            commands::cmd_cache(rest);
            cli_exit();
        }
        Some("graph") => {
            commands::cmd_graph(rest);
            cli_exit();
        }
        Some("metrics") => {
            commands::cmd_metrics(rest);
            cli_exit();
        }
        Some("alert") => {
            commands::cmd_alert(rest);
            cli_exit();
        }
        Some("webhook") => {
            commands::cmd_webhook(rest);
            cli_exit();
        }
        Some("diagnose") => {
            commands::cmd_diagnose(rest);
            cli_exit();
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_cli_help();
            cli_exit();
        }
        // Explicit `demo` runs the two-agent demo below; a bare invocation
        // keeps doing the same for back-compat, with a hint on stderr.
        Some("demo") => {}
        None => {
            eprintln!("(no subcommand — running the two-agent demo; see 'anima-hosted help')");
        }
        Some(other) => {
            eprintln!("anima-hosted: unknown command '{other}' — see 'anima-hosted help'");
            std::process::exit(2);
        }
    }

    // ── Backend selection (E1.3) ─────────────────────────────────────────────
    let provider = std::env::var("ANIMA_BACKEND").unwrap_or_else(|_| "mock".to_string());
    let backend = BackendFactory::from_env_or_mock(&provider);
    println!(
        "anima-hosted: selected backend={} model={} max_ctx={}",
        backend.id(),
        backend.model_id(),
        backend.max_context_tokens()
    );

    // ── Two-agent demo (E1.6) ────────────────────────────────────────────────
    let agent_a = build_agent(
        "agent-a",
        "optimize-for-low-token-usage",
        Arc::clone(&backend),
        vec![
            Task::new(1, 0, "draft the morning status report"),
            Task::new(2, 1, "summarize overnight telemetry"),
        ],
        6,
    );

    let agent_b = build_agent(
        "agent-b",
        "prioritize-tooling-throughput",
        Arc::clone(&backend),
        vec![
            Task::new(101, 0, "answer the operator question"),
            Task::new(102, 2, "compact yesterday memory archive"),
        ],
        6,
    );

    println!("booting two somatic loops over a shared backend...\n");

    let handle_a = std::thread::spawn(move || run_agent(agent_a));
    let handle_b = std::thread::spawn(move || run_agent(agent_b));

    let agent_a = handle_a.join().expect("agent-a thread panicked");
    let agent_b = handle_b.join().expect("agent-b thread panicked");

    audit_view::print_audit(&agent_a);
    println!();
    audit_view::print_audit(&agent_b);
}
