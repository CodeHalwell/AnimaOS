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

mod doctor;
mod init;

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
use vita::gate::Gate;
use vita::{
    record_gate_decision, somatic_execution_loop, AuditEntry, AuditLog, EventFeatures,
    GateOverride, HomeostaticSignals, IdentityMemory, LifecycleConfig, LifecycleManager,
    SemanticClass, ThresholdGate,
};

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

fn print_audit(manager: &LifecycleManager) {
    println!(
        "[{}] backend={} state={:?} dispatched={} audit={}",
        manager.agent_id,
        manager.backend.id(),
        manager.state,
        manager.scheduler.dispatched_tasks.len(),
        manager.audit.len()
    );
    for entry in manager.audit.entries() {
        match entry {
            AuditEntry::TaskStarted {
                task_id,
                tier,
                prompt,
                ..
            } => println!("  → started   task={task_id} tier={tier} prompt={prompt:?}"),
            AuditEntry::TaskCompleted {
                task_id,
                tokens_emitted,
                response,
                ..
            } => println!(
                "  ✓ completed task={task_id} tokens={tokens_emitted} response={response:?}"
            ),
            AuditEntry::TaskFailed { task_id, error, .. } => {
                println!("  ✗ failed    task={task_id} error={error}")
            }
            AuditEntry::SleepEntered { .. } => println!("  zzz sleep_entered"),
            AuditEntry::WakeEntered { .. } => println!("  ☀  wake_entered"),
            AuditEntry::SleepPhaseStarted { phase, .. } => {
                println!("  →   sleep_phase_started phase={phase}")
            }
            AuditEntry::SleepPhaseCompleted { phase, success, .. } => {
                let mark = if *success { "✓" } else { "✗" };
                println!("  {mark}   sleep_phase_completed phase={phase} success={success}");
            }
            // EX.2 memory pressure entries
            AuditEntry::MemoryPressureEvent {
                agent_id,
                level,
                active_tokens,
                max_context,
            } => {
                println!(
                    "  ⚠  memory_pressure agent={agent_id} level={level} \
                     tokens={active_tokens}/{max_context}"
                );
            }
            // E5.1 cortex entries
            AuditEntry::CortexInvoked {
                task_id,
                latency_to_first_action_ms,
                ..
            } => println!(
                "  ⚙  cortex_invoked task={task_id} latency_ms={latency_to_first_action_ms}"
            ),
            AuditEntry::CortexCompleted {
                task_id,
                tool_calls,
                summary_len,
                ..
            } => println!(
                "  ✓  cortex_completed task={task_id} tool_calls={tool_calls} summary_len={summary_len}"
            ),
            AuditEntry::CortexFault { task_id, error, .. } => {
                println!("  ✗  cortex_fault task={task_id} error={error}")
            }
            // ── E5.6 — Defence Layer ──────────────────────────────────────────
            AuditEntry::DefenceVeto {
                invocation_id,
                detector,
                action_blocked,
                reason,
                ..
            } => {
                println!(
                    "  🛡  DEFENCE VETO inv={invocation_id} detector={detector} \
                     action={action_blocked:?} reason={reason:?}"
                );
            }
            AuditEntry::AttentionDemandEscalated {
                invocation_id,
                veto_count,
                window_secs,
                ..
            } => {
                println!(
                    "  ⚠  ATTENTION ESCALATED inv={invocation_id} \
                     vetoes={veto_count} window={window_secs}s"
                );
            }
            // E5.2 gate decision entries
            AuditEntry::GateDecision {
                event_id,
                invoke,
                cost_class,
                urgency,
                novelty,
                value_score,
                threshold_applied,
                thermal_load,
                financial_budget,
                attention_demand,
                reasoning,
                override_active,
                ..
            } => {
                let verdict = if *invoke {
                    format!("INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
                } else {
                    "BLOCK".to_string()
                };
                let override_tag = if *override_active { " [OVERRIDE]" } else { "" };
                println!(
                    "  🔀 gate_decision event={event_id} verdict={verdict}{override_tag}"
                );
                println!(
                    "       urgency={urgency:.3} novelty={novelty:.3} \
                     value={value_score:.3} threshold={threshold_applied:.3}"
                );
                println!(
                    "       thermal={thermal_load:.3} financial_budget={financial_budget:.3} \
                     attention={attention_demand:.3}"
                );
                println!("       reasoning: {reasoning}");
            }
            // E5.3 router decision entries
            AuditEntry::RouterDecision {
                event_id,
                route_id,
                model_selector,
                tool_scope_name,
                tools_available,
                tools_permitted,
                memory_scope_identity,
                memory_scope_l2,
                memory_scope_l3,
                max_turns,
                max_tool_calls,
                ..
            } => {
                println!(
                    "  🗺  router_decision event={event_id} route={route_id} \
                     model={model_selector} scope={tool_scope_name}"
                );
                println!(
                    "       tools: {tools_permitted}/{tools_available} permitted"
                );
                println!(
                    "       memory: identity={memory_scope_identity} \
                     l2={memory_scope_l2} l3={memory_scope_l3}"
                );
                println!(
                    "       termination: max_turns={max_turns} max_tool_calls={max_tool_calls}"
                );
            }
            // E5.5 identity memory audit entries
            AuditEntry::IdentityUpdated { agent_id, key, old_value, new_value } => {
                let old_tag = match old_value {
                    Some(v) => format!(" (was {v:?})"),
                    None => " (new key)".to_owned(),
                };
                println!(
                    "  📝 identity_updated agent={agent_id} key={key:?} → {new_value:?}{old_tag}"
                );
            }
            // E5.7 interoceptive modulation audit entries
            AuditEntry::InteroceptiveSnapshot {
                agent_id,
                tick_ns,
                thermal_load,
                compute_pressure,
                memory_pressure,
                power_budget,
                financial_budget,
                attention_demand,
                aggregate_stress,
            } => {
                println!(
                    "  📊 interoceptive_snapshot agent={agent_id} tick_ns={tick_ns}"
                );
                println!(
                    "       thermal={thermal_load:.3} compute={compute_pressure:.3} \
                     memory={memory_pressure:.3}"
                );
                println!(
                    "       power={power_budget:.3} financial={financial_budget:.3} \
                     attention={attention_demand:.3}"
                );
                println!("       aggregate_stress={aggregate_stress:.3}");
            }
            AuditEntry::RouterModulated {
                event_id,
                requested_route_id,
                effective_route_id,
                reason,
                ..
            } => {
                println!(
                    "  ⬇  router_modulated event={event_id} \
                     requested={requested_route_id} → effective={effective_route_id}"
                );
                println!("       reason: {reason}");
            }
            // E5.4 KV-cache controller entries
            AuditEntry::KvGatePass {
                task_id,
                total_blocks,
                retained_blocks,
                budget,
                fallback_lru,
                needles_retained,
                total_needles,
                ..
            } => {
                let mode = if *fallback_lru { "LRU-fallback" } else { "controller" };
                println!(
                    "  🔒 kv_gate_pass task={task_id} mode={mode} \
                     retained={retained_blocks}/{total_blocks} budget={budget} \
                     needles={needles_retained}/{total_needles}"
                );
            }
            AuditEntry::KvControllerFaulted {
                task_id,
                fault_count,
                ..
            } => {
                println!(
                    "  ⚠  kv_controller_faulted task={task_id} fault_count={fault_count} \
                     (switching to LRU fallback)"
                );
            }
            // S5.7.6 Cache-Controller Modulation
            AuditEntry::KvMemoryPressureModulation {
                task_id,
                memory_pressure,
                nominal_budget,
                effective_budget,
                ..
            } => {
                println!(
                    "  🧠 kv_pressure_modulation task={task_id} \
                     pressure={memory_pressure:.2} budget={nominal_budget}→{effective_budget} \
                     (eviction more aggressive under pressure)"
                );
            }
        }
    }
}

// ── `anima identity` subcommand (E5.5 exit criterion 1) ──────────────────────

/// Implements the `anima identity show [<key>]` and
/// `anima identity set <key> <value>` subcommands.
///
/// Satisfies E5.5 exit criterion 1: "a user can run `anima identity show` and
/// `anima identity set <key> <value>` to inspect and edit identity memory;
/// edits round-trip through the audit log."
fn cmd_identity(args: &[String]) {
    const AGENT_ID: &str = "anima";

    let path = IdentityMemory::default_path(AGENT_ID);
    let mut store = IdentityMemory::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open identity store ({e}); using in-memory fallback");
        IdentityMemory::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("show") => {
            if let Some(key) = args.get(1) {
                // Show a single fact.
                match store.get_fact(key) {
                    Some(value) => println!("{key} = {value}"),
                    None => println!("{key}: (not set)"),
                }
            } else {
                // Show the full identity document.
                let json = store.to_json();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json).unwrap_or_default()
                );
            }
        }
        Some("set") => {
            match (args.get(1), args.get(2)) {
                (Some(key), Some(value)) => {
                    match store.set_fact(key, value, &mut log, AGENT_ID) {
                        Ok(()) => {
                            println!("identity: set {key:?} = {value:?}");
                            // Print the audit trail so the caller can verify the
                            // round-trip (E5.5 exit criterion 1).
                            for entry in log.entries() {
                                if let AuditEntry::IdentityUpdated {
                                    key,
                                    new_value,
                                    old_value,
                                    ..
                                } = entry
                                {
                                    let old_tag = match old_value {
                                        Some(v) => format!(" (was {v:?})"),
                                        None => " (new key)".to_owned(),
                                    };
                                    println!("audit: identity_updated key={key:?} → {new_value:?}{old_tag}");
                                }
                            }
                        }
                        Err(e) => eprintln!("identity: error: {e}"),
                    }
                }
                _ => {
                    eprintln!("usage: anima-hosted identity set <key> <value>");
                }
            }
        }
        _ => {
            eprintln!("usage: anima-hosted identity show [<key>]");
            eprintln!("       anima-hosted identity set <key> <value>");
        }
    }
}

// ── `anima why` subcommand (E5.2 exit criterion 3 + E5.7 exit criterion 2) ───

/// Exercises the Striatal Gate on representative events, records the decisions
/// to an in-process audit log, and prints the most recent `GateDecision` entry.
///
/// In E5.7 the function is extended to also sample the interoceptive sensor
/// bundle and display the live signal snapshot alongside gate decisions,
/// satisfying E5.7 exit criterion 2: "The `anima why` CLI command includes the
/// homeostatic signal values at the time of the decision."
fn cmd_why() {
    use interoception::{HomeostaticMonitor, InteroceptiveSensorBundle};
    use vita::AuditLog;

    println!("anima why — Striatal Gate + Interoceptive Modulation (E5.2 / E5.7)\n");

    // ── Live interoceptive snapshot (E5.7, exit criterion 2) ─────────────────
    let sensor_bundle = InteroceptiveSensorBundle::with_defaults();
    let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0); // baseline (no stress)

    // Use epoch 0 as the timestamp sentinel for this demo (no real spend).
    let live_signals = sensor_bundle.sample(&monitor, 0, 4096, 0);
    let live_homeostatic = HomeostaticSignals::from_interoceptive(&live_signals);

    println!("━━━ Live interoceptive snapshot (from sensors)");
    println!(
        "  thermal_load    : {:.3}  (TTFT-ratio proxy)",
        live_signals.thermal_load
    );
    println!(
        "  compute_pressure: {:.3}  (TTFT-ratio proxy)",
        live_signals.compute_pressure
    );
    println!(
        "  memory_pressure : {:.3}  (L1 context fill)",
        live_signals.memory_pressure
    );
    println!(
        "  power_budget    : {:.3}  (disabled sensor → AC sentinel)",
        live_signals.power_budget
    );
    println!(
        "  financial_budget: {:.3}  (no spend recorded → full budget)",
        live_signals.financial_budget
    );
    println!(
        "  attention_demand: {:.3}  (disabled sensor → user-present sentinel)",
        live_signals.attention_demand
    );
    println!("  aggregate_stress: {:.3}", live_signals.aggregate_stress());
    println!();

    let gate = ThresholdGate::with_defaults();
    let mut log = AuditLog::new();
    let agent_id = "anima";

    // Sample scenarios covering the full range of gate outcomes.
    let scenarios: &[(&str, EventFeatures, HomeostaticSignals, GateOverride)] = &[
        (
            "background-cleanup",
            EventFeatures {
                urgency: 0.1,
                novelty: 0.05,
                semantic_class: SemanticClass::BackgroundTask,
                user_facing: false,
            },
            HomeostaticSignals::neutral(),
            GateOverride::None,
        ),
        (
            "user-question",
            EventFeatures {
                urgency: 0.7,
                novelty: 0.4,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            },
            HomeostaticSignals::neutral(),
            GateOverride::None,
        ),
        (
            "high-priority-under-thermal",
            EventFeatures {
                urgency: 0.8,
                novelty: 0.3,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            },
            HomeostaticSignals {
                thermal_load: 0.9,
                ..HomeostaticSignals::neutral()
            },
            GateOverride::None,
        ),
        (
            "operator-emergency",
            EventFeatures {
                urgency: 0.4,
                novelty: 0.2,
                semantic_class: SemanticClass::OperatorCommand,
                user_facing: false,
            },
            HomeostaticSignals::neutral(),
            GateOverride::OperatorForced {
                reason: "emergency shutdown initiated".to_string(),
            },
        ),
    ];

    for (event_id, event, signals, override_hint) in scenarios {
        let decision = gate.decide(event_id, event, signals, override_hint);
        record_gate_decision(&mut log, agent_id, &decision, event, signals);
    }

    println!("Evaluated {} scenarios. Full audit trail:\n", log.len());
    for entry in log.entries() {
        if let AuditEntry::GateDecision {
            event_id,
            invoke,
            cost_class,
            urgency,
            novelty,
            user_facing,
            semantic_class,
            value_score,
            threshold_applied,
            thermal_load,
            compute_pressure,
            memory_pressure,
            power_budget,
            financial_budget,
            attention_demand,
            reasoning,
            override_active,
            ..
        } = entry
        {
            let verdict = if *invoke {
                format!("✅ INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
            } else {
                "🚫 BLOCK".to_string()
            };
            let override_tag = if *override_active {
                "  ⚠ OVERRIDE ACTIVE"
            } else {
                ""
            };
            println!("━━━ event: {event_id}{override_tag}");
            println!("  verdict         : {verdict}");
            println!("  reasoning       : {reasoning}");
            println!("  ─ event features ─────────────────────────────");
            println!("  urgency         : {urgency:.3}");
            println!("  novelty         : {novelty:.3}");
            println!("  user_facing     : {user_facing}");
            println!("  semantic_class  : {semantic_class}");
            println!("  value_score     : {value_score:.4}");
            println!("  threshold       : {threshold_applied:.4}");
            println!("  ─ homeostatic signals ────────────────────────");
            println!("  thermal_load    : {thermal_load:.3}");
            println!("  compute_pressure: {compute_pressure:.3}");
            println!("  memory_pressure : {memory_pressure:.3}");
            println!("  power_budget    : {power_budget:.3}");
            println!("  financial_budget: {financial_budget:.3}");
            println!("  attention_demand: {attention_demand:.3}");
            println!();
        }
    }

    // Print the most recent GateDecision (satisfies E5.2 exit criterion 3).
    if let Some(AuditEntry::GateDecision {
        event_id,
        reasoning,
        invoke,
        cost_class,
        ..
    }) = log
        .entries()
        .iter()
        .rev()
        .find(|e| matches!(e, AuditEntry::GateDecision { .. }))
    {
        let verdict = if *invoke {
            format!("INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
        } else {
            "BLOCK".to_string()
        };
        println!("Most recent gate decision: event={event_id} verdict={verdict}");
        println!("Reasoning: {reasoning}");
    }

    // ── E5.7 Router modulation demo ───────────────────────────────────────────
    println!("\n━━━ E5.7 Router modulation with live interoceptive signals");
    use vita::{record_modulated_router_decision, CostClass, SemanticClass as SC, StaticRouter};

    let router =
        StaticRouter::with_defaults().expect("default router should construct without error");

    // Show how the router would modulate under various stress levels.
    let modulation_scenarios: &[(&str, f32, f32, f32, CostClass)] = &[
        ("neutral (full budgets)", 1.0, 1.0, 0.0, CostClass::Frontier),
        ("mild thermal stress", 1.0, 1.0, 0.85, CostClass::Frontier),
        (
            "moderate financial (30%)",
            0.30,
            1.0,
            0.0,
            CostClass::Frontier,
        ),
        (
            "severe financial (10%)",
            0.10,
            1.0,
            0.0,
            CostClass::Frontier,
        ),
        ("severe power (10%)", 1.0, 0.10, 0.0, CostClass::Frontier),
        (
            "live sensor signals",
            live_homeostatic.financial_budget,
            live_homeostatic.power_budget,
            live_homeostatic.thermal_load,
            CostClass::Frontier,
        ),
    ];

    let mut mod_audit = AuditLog::new();
    for (label, financial, power, thermal, cost_class) in modulation_scenarios {
        let sigs = HomeostaticSignals {
            thermal_load: *thermal,
            compute_pressure: *thermal,
            memory_pressure: 0.0,
            power_budget: *power,
            financial_budget: *financial,
            attention_demand: 0.5,
        };
        let decision = router.resolve_with_modulation(SC::UserQuery, *cost_class, &sigs);
        record_modulated_router_decision(&mut mod_audit, "anima", label, &decision, 3, 0);
        let mod_tag = if decision.was_modulated {
            format!(
                " → {} [MODULATED: {}]",
                decision.effective_route.id,
                decision.modulation_reason.as_deref().unwrap_or("?")
            )
        } else {
            format!(" → {} [no modulation]", decision.effective_route.id)
        };
        println!(
            "  {label}: requested={}{mod_tag}",
            decision.requested_route.id
        );
    }
    println!();
    println!(
        "RouterModulated audit entries: {}",
        mod_audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::RouterModulated { .. }))
            .count()
    );
}

/// `anima serve` — boot a single long-lived agent and expose the operator
/// console (HTTP/SSE telemetry + a guidance ingress).
///
/// This is the container/hosted realisation of `docs/11-operator-interface.md`.
/// The agent starts idle: it sleeps until operator guidance (or another sensory
/// event) wakes it, demonstrating the human-as-a-sense model directly. The
/// console never touches the lifecycle — it shares the `SensoryBridge` for
/// ingress and tails the durable audit log for egress.
fn cmd_serve() {
    // The console tails the agent's audit JSONL; make sure vita writes one by
    // pinning ANIMA_AUDIT_DIR (honoured by `AuditLog::from_env`) before the
    // LifecycleManager is constructed.
    let audit_dir = std::env::var("ANIMA_AUDIT_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.anima/audit")
    });
    std::env::set_var("ANIMA_AUDIT_DIR", &audit_dir);
    let _ = std::fs::create_dir_all(&audit_dir);

    let agent_id = std::env::var("ANIMA_AGENT_ID").unwrap_or_else(|_| "anima".to_string());
    let audit_path = std::path::PathBuf::from(&audit_dir).join(format!("{agent_id}.jsonl"));

    let provider = std::env::var("ANIMA_BACKEND").unwrap_or_else(|_| "mock".to_string());
    let backend = BackendFactory::from_env_or_mock(&provider);

    // The shared bridge: POSTed guidance lands in the very queue the loop drains.
    let bridge = SensoryBridge::new(HumanGuidance::new("operator-console"));

    let mut manager = LifecycleManager::new(
        &agent_id,
        bridge.clone(),
        VirtualContextManager::with_capacity(0, 8192),
        LifecycleConfig { max_context: 8192 },
        HumanGuidance::new("boot"),
        Arc::clone(&backend),
        None, // run forever
    );
    // Publish vital signs every iteration: the snapshot is written to the audit
    // log, where the console's tailer turns it into a `Vitals` event.
    manager.sensor_bundle = Some(Arc::new(InteroceptiveSensorBundle::with_defaults()));

    // Bring up the console (HTTP/SSE server + audit tailer) on its own threads.
    let console = Console::new(bridge.clone(), &audit_path, ServerConfig::from_env());
    let addr = console
        .start()
        .expect("operator console failed to bind — is the port already in use?");
    let token_note = std::env::var("ANIMA_CONSOLE_TOKEN")
        .map(|t| !t.is_empty())
        .unwrap_or(false);

    println!("anima-hosted: operator console listening on http://{addr}");
    println!(
        "  dashboard : http://{addr}/{}",
        if token_note { "?token=…" } else { "" }
    );
    println!("  events    : GET  http://{addr}/events   (Server-Sent Events)");
    println!("  guidance  : POST http://{addr}/guidance (afferent ingress)");
    if token_note {
        println!("  auth      : bearer token required (ANIMA_CONSOLE_TOKEN)");
    }
    println!("  backend   : {} ({})", backend.id(), backend.model_id());
    println!("  audit log : {}", audit_path.display());
    println!(
        "\nThe agent starts idle and sleeps until a sense wakes it. Send guidance —\n\
         it enters the sensory queue and is arbitrated by the gate, never executed directly:\n  \
         anima-console send \"summarise the overnight logs\" --priority High --url http://{addr}\n  \
         anima-console tui --url http://{addr}\n"
    );

    // Drive the somatic loop forever on a worker thread; the audit tailer +
    // SSE server (already running) surface everything it does.
    let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0);
    let worker = std::thread::Builder::new()
        .name("anima-somatic-loop".to_string())
        .spawn(move || {
            block_on(somatic_execution_loop(&mut manager, &monitor))
                .expect("somatic execution loop failed");
        })
        .expect("spawn somatic loop");
    worker.join().expect("somatic loop thread panicked");
}

fn main() {
    // ── Subcommand dispatch ───────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("why") {
        cmd_why();
        return;
    }
    if args.first().map(String::as_str) == Some("identity") {
        cmd_identity(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("serve") {
        cmd_serve();
        return;
    }
    if args.first().map(String::as_str) == Some("doctor") {
        let report = doctor::run_doctor();
        doctor::print_report(&report);
        return;
    }
    if args.first().map(String::as_str) == Some("init") {
        let non_interactive = args.iter().any(|a| a == "--non-interactive");
        let reset = args.iter().any(|a| a == "--reset");
        init::run_init("anima", non_interactive, reset);
        return;
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

    print_audit(&agent_a);
    println!();
    print_audit(&agent_b);
}
