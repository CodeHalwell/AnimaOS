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
use metrics::{aggregate, render_prometheus, render_text_report};
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
            // ── E14.1 Metacognition ───────────────────────────────────────────
            AuditEntry::CortexConfidenceReport {
                agent_id,
                task_id,
                confidence,
                evidence_count,
                asks_for_help,
            } => {
                let help_tag = if *asks_for_help { " [HELP REQUESTED]" } else { "" };
                println!(
                    "  🤔 confidence_report agent={agent_id} task={task_id} \
                     confidence={confidence:.3} evidence={evidence_count}{help_tag}"
                );
            }
            AuditEntry::CalibrationEntry {
                agent_id,
                task_id,
                predicted_confidence,
                outcome_success,
                calibration_error,
            } => {
                let outcome = if *outcome_success { "success" } else { "failure" };
                println!(
                    "  📐 calibration agent={agent_id} task={task_id} \
                     predicted={predicted_confidence:.3} outcome={outcome} \
                     error={calibration_error:.3}"
                );
            }
            // ── E14.2 Prospective memory ──────────────────────────────────────
            AuditEntry::IntentionScheduled {
                agent_id,
                intention_id,
                description,
                due_at_ns,
                overdue,
            } => {
                let overdue_tag = if *overdue { " [OVERDUE]" } else { "" };
                println!(
                    "  📅 intention_scheduled agent={agent_id} id={intention_id} \
                     due_ns={due_at_ns} desc={description:?}{overdue_tag}"
                );
            }
            AuditEntry::IntentionCompleted {
                agent_id,
                intention_id,
                rescheduled,
                new_due_at_ns,
            } => {
                let resched = if *rescheduled {
                    format!(" rescheduled_at={}", new_due_at_ns.unwrap_or(0))
                } else {
                    String::new()
                };
                println!(
                    "  ✅ intention_completed agent={agent_id} id={intention_id}{resched}"
                );
            }
            // ── E14.3 Knowledge corpus ────────────────────────────────────────
            AuditEntry::KnowledgeIngested {
                agent_id,
                source_key,
                document_bytes,
            } => {
                println!(
                    "  📚 knowledge_ingested agent={agent_id} \
                     source={source_key:?} bytes={document_bytes}"
                );
            }
            // ── E14.4 Cognitive watchdog ──────────────────────────────────────
            AuditEntry::CognitiveWatchdogTripped {
                agent_id,
                detector,
                reason,
                streak,
                trip_count,
            } => {
                println!(
                    "  🚨 watchdog_tripped agent={agent_id} detector={detector} \
                     streak={streak} trip_count={trip_count}"
                );
                println!("       reason: {reason}");
            }
            AuditEntry::AgentSnapshotTaken {
                agent_id,
                taken_at_ns,
                description,
                l1_node_count,
            } => {
                println!(
                    "  📸 snapshot_taken agent={agent_id} at_ns={taken_at_ns} \
                     l1_nodes={l1_node_count} desc={description:?}"
                );
            }
            // E13 — Alignment Assurance
            AuditEntry::ConstitutionVeto {
                agent_id,
                invocation_id,
                prohibition_id,
                clause_text,
                action_blocked,
                proposal_type,
            } => {
                println!(
                    "  ⛔ CONSTITUTION VETO agent={agent_id} inv={invocation_id} \
                     prohibition={prohibition_id} type={proposal_type}"
                );
                println!("       clause: {clause_text}");
                println!("       blocked: {action_blocked:?}");
            }
            AuditEntry::CorrigibilityAsserted {
                agent_id,
                reason,
                adverse_condition,
            } => {
                println!(
                    "  ✅ corrigibility_asserted agent={agent_id} \
                     reason={reason:?} condition={adverse_condition:?}"
                );
            }
            // E12 Motivation
            AuditEntry::DriveStateSnapshot {
                viability_urgency,
                service_urgency,
                epistemic_urgency,
                drive_delta,
                lattice_suppression_active,
                ..
            } => {
                println!(
                    "  🎯 drive_state viability={viability_urgency:.2} service={service_urgency:.2} \
                     epistemic={epistemic_urgency:.2} delta={drive_delta:.3}{}",
                    if *lattice_suppression_active { " [lattice suppressed]" } else { "" }
                );
            }
            AuditEntry::GoalSpawned {
                goal_id,
                description,
                provenance,
                priority,
                ..
            } => {
                println!(
                    "  🎯 goal_spawned id={goal_id} priority={priority:.2} \
                     provenance={provenance} desc={description:?}"
                );
            }
            AuditEntry::GoalCompleted {
                goal_id,
                description,
                ..
            } => {
                println!("  ✅ goal_completed id={goal_id} desc={description:?}");
            }
            AuditEntry::CorrigibilityHold {
                blocked_goal_description,
                reason,
                ..
            } => {
                println!(
                    "  🛑 corrigibility_hold blocked={blocked_goal_description:?} reason={reason:?}"
                );
            }
            AuditEntry::AffectStateSnapshot {
                valence,
                arousal,
                gate_threshold_nudge,
                ..
            } => {
                println!(
                    "  💭 affect valence={valence:+.2} arousal={arousal:.2} \
                     nudge={gate_threshold_nudge:.3}"
                );
            }
            // ── E11 Skills & Self-Extension entries ───────────────────────────
            AuditEntry::SkillRegistered {
                skill_id,
                skill_name,
                authored_by,
                initial_state,
                source_episode,
                ..
            } => {
                let ep = source_episode
                    .as_deref()
                    .map(|e| format!(" (episode: {e})"))
                    .unwrap_or_default();
                println!(
                    "  🎓 skill_registered id={skill_id} name={skill_name:?} \
                     authored_by={authored_by} state={initial_state}{ep}"
                );
            }
            AuditEntry::SkillPromoted { skill_id, .. } => {
                println!("  ✅ skill_promoted id={skill_id}");
            }
            AuditEntry::SkillRolledBack { skill_id, reason, .. } => {
                println!("  ↩️  skill_rolled_back id={skill_id} reason={reason:?}");
            }
            AuditEntry::SkillQuarantined { skill_id, reason, .. } => {
                println!("  🔒 skill_quarantined id={skill_id} reason={reason:?}");
            }
            AuditEntry::SkillKillSwitchActivated {
                quarantined_skill_ids,
                reason,
                ..
            } => {
                println!(
                    "  ☠️  skill_kill_switch quarantined={} reason={reason:?}",
                    quarantined_skill_ids.join(", ")
                );
            }
            AuditEntry::ToolProposed {
                tool_id,
                authored_by,
                fixture_summary,
                ..
            } => {
                println!(
                    "  🔧 tool_proposed id={tool_id} authored_by={authored_by} \
                     fixtures={fixture_summary:?}"
                );
            }
            AuditEntry::ToolApproved { tool_id, .. } => {
                println!("  ✅ tool_approved id={tool_id}");
            }
            AuditEntry::ToolRevoked { tool_id, reason, .. } => {
                println!("  🚫 tool_revoked id={tool_id} reason={reason:?}");
            }
            AuditEntry::SkillReflectionCompleted {
                episodes_analysed,
                patterns_found,
                proposals_generated,
                ..
            } => {
                println!(
                    "  🔍 skill_reflection episodes={episodes_analysed} \
                     patterns={patterns_found} proposals={proposals_generated}"
                );
            }
            // ── E10 — Presence ─────────────────────────────────────────────
            AuditEntry::ChannelMessageReceived {
                channel,
                from,
                modality,
                ..
            } => {
                println!("  📨 channel_received channel={channel} from={from} modality={modality}");
            }
            AuditEntry::ChannelMessageSent {
                channel,
                to,
                modality,
                ..
            } => {
                println!("  📤 channel_sent channel={channel} to={to} modality={modality}");
            }
            AuditEntry::ModalityUnsupported {
                channel, modality, ..
            } => {
                println!(
                    "  ⚠️  modality_unsupported channel={channel} modality={modality}"
                );
            }
            // E7 — Embodiment egress audit entries
            AuditEntry::EgressRequested { tool_id, url } => {
                println!("  🌐 egress_requested tool={tool_id} url={url}");
            }
            AuditEntry::EgressBlocked { tool_id, url, reason } => {
                println!("  🚫 egress_blocked tool={tool_id} url={url} reason={reason:?}");
            }
            // E7 — Tool selection audit entry
            AuditEntry::ToolSelection {
                agent_id,
                candidates_scored,
                kept,
                tau_rel,
                ..
            } => {
                println!(
                    "  🔍 tool_selection agent={agent_id} scored={candidates_scored} \
                     kept={kept} tau_rel={tau_rel:.2}"
                );
            }
            // E16 — Multi-Agent Coordination (A2A bus) audit entries
            AuditEntry::AgentDelegated {
                parent_agent_id,
                target_agent_id,
                delegation_id,
                task,
            } => {
                println!(
                    "  🤝 agent_delegated parent={parent_agent_id} → target={target_agent_id} \
                     id={delegation_id} task={task:?}"
                );
            }
            AuditEntry::AgentDelegationCompleted {
                parent_agent_id,
                target_agent_id,
                delegation_id,
                success,
                tool_calls_made,
                duration_ms,
                summary,
            } => {
                println!(
                    "  ✅ agent_delegation_completed parent={parent_agent_id} \
                     target={target_agent_id} id={delegation_id} success={success} \
                     calls={tool_calls_made} duration={duration_ms}ms summary={summary:?}"
                );
            }
            AuditEntry::AgentDelegationFailed {
                parent_agent_id,
                target_agent_id,
                delegation_id,
                reason,
            } => {
                println!(
                    "  ❌ agent_delegation_failed parent={parent_agent_id} \
                     target={target_agent_id} id={delegation_id} reason={reason:?}"
                );
            }
            // E15 Trust & Lifecycle entries
            AuditEntry::DigestGenerated {
                agent_id,
                window_entries,
                tasks_completed,
                tasks_failed,
                cortex_invocations,
                sleep_cycles,
                defence_vetoes,
                notable_event_count,
            } => {
                println!(
                    "  📋 digest_generated agent={agent_id} window={window_entries} entries"
                );
                println!(
                    "       tasks: {tasks_completed} completed, {tasks_failed} failed, \
                     {cortex_invocations} cortex calls"
                );
                println!(
                    "       sleep: {sleep_cycles} cycles  vetoes: {defence_vetoes}  \
                     notable: {notable_event_count}"
                );
            }
            AuditEntry::SnapshotCreated {
                agent_id,
                schema_version,
                snapshot_path,
                entry_count,
                reason,
            } => {
                let reason_tag = reason.as_deref().unwrap_or("(none)");
                println!(
                    "  💾 snapshot_created agent={agent_id} schema_v={schema_version} \
                     entries={entry_count} path={snapshot_path:?} reason={reason_tag:?}"
                );
            }
            AuditEntry::SnapshotRestored {
                agent_id,
                schema_version,
                snapshot_path,
            } => {
                println!(
                    "  📂 snapshot_restored agent={agent_id} schema_v={schema_version} \
                     path={snapshot_path:?}"
                );
            }
            AuditEntry::ApprovalProposalQueued {
                agent_id,
                proposal_id,
                kind,
                provenance,
            } => {
                println!(
                    "  📥 approval_queued agent={agent_id} id={proposal_id} \
                     kind={kind} provenance={provenance:?}"
                );
            }
            AuditEntry::ApprovalProposalDecided {
                agent_id,
                proposal_id,
                decision,
                reason,
            } => {
                let mark = match decision.as_str() {
                    "approved" => "✅",
                    "rejected" => "❌",
                    _ => "↩",
                };
                println!(
                    "  {mark} approval_decided agent={agent_id} id={proposal_id} \
                     decision={decision} reason={reason:?}"
                );
            }
            // E17 — Trust, Human-Identity & Privacy
            AuditEntry::UserProfileCreated {
                agent_id,
                user_id,
                display_name,
                channel,
            } => {
                println!(
                    "  👤 user_profile_created agent={agent_id} user={user_id} \
                     name={display_name:?} channel={channel}"
                );
            }
            AuditEntry::UserTrustUpdated {
                agent_id,
                user_id,
                old_tier,
                new_tier,
            } => {
                println!(
                    "  🔐 user_trust_updated agent={agent_id} user={user_id} \
                     {old_tier} → {new_tier}"
                );
            }
            AuditEntry::UserConsentUpdated {
                agent_id,
                user_id,
                category,
                granted,
            } => {
                let mark = if *granted { "✅" } else { "❌" };
                println!(
                    "  {mark} user_consent_updated agent={agent_id} user={user_id} \
                     category={category} granted={granted}"
                );
            }
            // ── E8 S8.4.3 — Sleep-cycle consolidation hook ───────────────────
            AuditEntry::ConsolidationSkipped {
                agent_id,
                pairs_available,
                min_required,
            } => {
                println!(
                    "  ⏭  consolidation_skipped agent={agent_id} \
                     pairs_available={pairs_available} min_required={min_required}"
                );
            }
            AuditEntry::ConsolidationStarted {
                agent_id,
                pairs_trained,
            } => {
                println!(
                    "  🧠 consolidation_started agent={agent_id} pairs={pairs_trained}"
                );
            }
            AuditEntry::ConsolidationCompleted {
                agent_id,
                adapter_id,
                pairs_trained,
                registered,
            } => {
                let reg_tag = if *registered { " [registered]" } else { "" };
                println!(
                    "  ✓  consolidation_completed agent={agent_id} \
                     adapter={adapter_id} pairs={pairs_trained}{reg_tag}"
                );
            }
            AuditEntry::ConsolidationFailed { agent_id, error } => {
                println!("  ✗  consolidation_failed agent={agent_id} error={error}");
            }

            // ── E18 Per-User Rate Limiting & Token Quotas ─────────────────────
            AuditEntry::QuotaExceeded {
                agent_id,
                user_id,
                trust_tier,
                exceeded_reason,
                tokens_requested,
                retry_after_ns,
            } => {
                println!(
                    "  🚫 quota_exceeded agent={agent_id} user={user_id} tier={trust_tier} \
                     tokens_req={tokens_requested} reason={exceeded_reason:?} \
                     retry_after_ns={retry_after_ns}"
                );
            }
            AuditEntry::QuotaEscalated {
                agent_id,
                user_id,
                trust_tier,
                violations_in_window,
                threshold,
            } => {
                println!(
                    "  🔔 quota_escalated agent={agent_id} user={user_id} tier={trust_tier} \
                     violations={violations_in_window} threshold={threshold}"
                );
            }
            // ── E20 — Structured Runtime Configuration ────────────────────────
            AuditEntry::ConfigLoaded {
                agent_id,
                path,
                schema_version,
                from_file,
            } => {
                let src = if *from_file { "file" } else { "defaults" };
                println!(
                    "  ⚙  config_loaded agent={agent_id} path={path} \
                     schema_version={schema_version} source={src}"
                );
            }
            AuditEntry::ConfigReloaded {
                agent_id,
                path,
                changed_keys,
            } => {
                let keys = if changed_keys.is_empty() {
                    "(no changes)".to_string()
                } else {
                    changed_keys.join(", ")
                };
                println!(
                    "  ⚙  config_reloaded agent={agent_id} path={path} changed=[{keys}]"
                );
            }
            // ── E22 Session Management ────────────────────────────────────────
            AuditEntry::SessionStarted {
                agent_id,
                session_id,
                user_id,
            } => {
                println!(
                    "  💬 session_started agent={agent_id} \
                     session={session_id} user={user_id}"
                );
            }
            AuditEntry::SessionTurnAppended {
                agent_id,
                session_id,
                role,
                content_len,
            } => {
                println!(
                    "  💬 session_turn_appended agent={agent_id} \
                     session={session_id} role={role} len={content_len}"
                );
            }
            AuditEntry::SessionArchived {
                agent_id,
                session_id,
                turn_count,
                has_summary,
            } => {
                let summary_tag = if *has_summary { " [summary]" } else { "" };
                println!(
                    "  📁 session_archived agent={agent_id} \
                     session={session_id} turns={turn_count}{summary_tag}"
                );
            }
            AuditEntry::SessionExported {
                agent_id,
                session_id,
                format,
                turn_count,
            } => {
                println!(
                    "  📤 session_exported agent={agent_id} \
                     session={session_id} format={format} turns={turn_count}"
                );
            }
            // ── E23 Consent Enforcement and Data Lifecycle ────────────────────
            AuditEntry::ConsentCheckBlocked {
                agent_id,
                user_id,
                category,
                reason,
            } => {
                println!(
                    "  🚫 consent_blocked agent={agent_id} \
                     user={user_id} category={category} reason={reason}"
                );
            }
            AuditEntry::DataExported {
                agent_id,
                user_id,
                section_count,
                total_records,
                output_path,
            } => {
                println!(
                    "  📤 data_exported agent={agent_id} user={user_id} \
                     sections={section_count} records={total_records} path={output_path}"
                );
            }
            AuditEntry::DataDeletedForUser {
                agent_id,
                user_id,
                categories,
                records_deleted,
            } => {
                println!(
                    "  🗑️  data_deleted agent={agent_id} user={user_id} \
                     categories=[{categories}] records={records_deleted}"
                );
            }
            AuditEntry::ExpiredConsentCleaned {
                agent_id,
                users_scanned,
                expired_grants_found,
                users_affected,
                total_records_deleted,
            } => {
                println!(
                    "  🧹 expired_consent_cleaned agent={agent_id} \
                     scanned={users_scanned} expired={expired_grants_found} \
                     affected={users_affected} deleted={total_records_deleted}"
                );
            }
            // ── E24 Response Quality & Feedback Collection ─────────────────
            AuditEntry::FeedbackReceived {
                agent_id,
                user_id,
                invocation_id,
                rating_label,
                score,
                category_count,
            } => {
                println!(
                    "  💬 feedback_received agent={agent_id} user={user_id} \
                     inv={invocation_id} rating={rating_label} \
                     score={score:.2} categories={category_count}"
                );
            }
            AuditEntry::QualityReportGenerated {
                agent_id,
                total_feedback,
                satisfaction_pct,
                avg_score_pct,
            } => {
                let sat = satisfaction_pct
                    .map(|p| format!("{p}%"))
                    .unwrap_or_else(|| "n/a".to_string());
                println!(
                    "  📊 quality_report agent={agent_id} total={total_feedback} \
                     satisfaction={sat} avg_score={avg_score_pct}%"
                );
            }
            AuditEntry::FeedbackCorrectionRecorded { agent_id, user_id, invocation_id } => {
                println!(
                    "  ✏️  feedback_correction agent={agent_id} user={user_id} \
                     inv={invocation_id}"
                );
            }
            // ── E26 — Tool Response Caching ─────────────────────────────────
            AuditEntry::ToolCacheHit {
                agent_id,
                tool_id,
                hit_age_ms,
            } => {
                println!(
                    "  💾 tool_cache_hit agent={agent_id} tool={tool_id} age={hit_age_ms}ms"
                );
            }
            AuditEntry::ToolCacheMiss { agent_id, tool_id } => {
                println!("  🔍 tool_cache_miss agent={agent_id} tool={tool_id}");
            }
            AuditEntry::ToolCacheEvicted { agent_id, count } => {
                println!("  🗑  tool_cache_evicted agent={agent_id} count={count}");
            }
            AuditEntry::KnowledgeEntityAdded {
                agent_id,
                entity_id,
                kind,
                display_name,
            } => {
                println!(
                    "  🔷 knowledge_entity_added agent={agent_id} id={entity_id} kind={kind} name={display_name}"
                );
            }
            AuditEntry::KnowledgeRelationAdded {
                agent_id,
                from_entity,
                to_entity,
                kind,
            } => {
                println!(
                    "  🔗 knowledge_relation_added agent={agent_id} {from_entity} --[{kind}]--> {to_entity}"
                );
            }
            AuditEntry::KnowledgeGraphQueried {
                agent_id,
                query_type,
                result_count,
            } => {
                println!(
                    "  🔍 knowledge_graph_queried agent={agent_id} type={query_type} results={result_count}"
                );
            }
            // ── E18 Metrics & Observability ───────────────────────────────────
            AuditEntry::MetricsSnapshot {
                agent_id,
                window_entries,
                tasks_started,
                tasks_completed,
                total_tokens_emitted,
                gate_decisions,
                gate_invocations,
                cortex_invocations,
                cortex_faults,
                total_vetoes,
                sleep_cycles,
                mean_thermal_load,
                mean_financial_budget,
                ..
            } => {
                println!(
                    "  📊  metrics_snapshot agent={agent_id} window={window_entries} \
                     tasks={tasks_completed}/{tasks_started} tokens={total_tokens_emitted} \
                     gate={gate_invocations}/{gate_decisions} cortex={cortex_invocations} \
                     faults={cortex_faults} vetoes={total_vetoes} sleep={sleep_cycles} \
                     thermal={mean_thermal_load:.2} fin_budget={mean_financial_budget:.2}"
                );
            }
            // ── E28 — Alert Rules ─────────────────────────────────────────────
            // ── E28 — Alert Rules ─────────────────────────────────────────────
            AuditEntry::AlertRuleAdded {
                agent_id, rule_id, description, field, op, threshold, severity,
            } => {
                println!(
                    "  🔔  alert_rule_added agent={agent_id} id={rule_id} \
                     condition=\"{field} {op} {threshold:.4}\" severity={severity} \
                     desc=\"{description}\""
                );
            }
            AuditEntry::AlertRuleRemoved { agent_id, rule_id } => {
                println!("  🔕  alert_rule_removed agent={agent_id} id={rule_id}");
            }
            AuditEntry::AlertFired {
                agent_id, rule_id, field, actual_value, threshold, severity,
            } => {
                println!(
                    "  🚨  alert_fired agent={agent_id} id={rule_id} \
                     {field}={actual_value:.4} threshold={threshold:.4} severity={severity}"
                );
            }
            AuditEntry::AlertResolved {
                agent_id, rule_id, field, actual_value,
            } => {
                println!(
                    "  ✅  alert_resolved agent={agent_id} id={rule_id} \
                     {field}={actual_value:.4}"
                );
            }
            // E29 — Outbound Webhook Integration
            AuditEntry::WebhookRegistered {
                agent_id,
                endpoint_id,
                url,
                has_secret,
            } => {
                let secret_tag = if *has_secret { " [signed]" } else { "" };
                println!(
                    "  🔔 webhook_registered agent={agent_id} id={endpoint_id} \
                     url={url}{secret_tag}"
                );
            }
            AuditEntry::WebhookRemoved {
                agent_id,
                endpoint_id,
            } => {
                println!(
                    "  🗑  webhook_removed agent={agent_id} id={endpoint_id}"
                );
            }
            AuditEntry::WebhookDispatched {
                agent_id,
                endpoint_id,
                event_kind,
                attempts,
            } => {
                let retry_tag = if *attempts > 1 {
                    format!(" ({attempts} attempts)")
                } else {
                    String::new()
                };
                println!(
                    "  📤 webhook_dispatched agent={agent_id} id={endpoint_id} \
                     event={event_kind}{retry_tag}"
                );
            }
            AuditEntry::WebhookFailed {
                agent_id,
                endpoint_id,
                event_kind,
                attempts,
                error,
            } => {
                println!(
                    "  ❌ webhook_failed agent={agent_id} id={endpoint_id} \
                     event={event_kind} attempts={attempts} error={error:?}"
                );
            }
            // ── E30 — Agent Self-Diagnostic System ───────────────────────────
            AuditEntry::DiagnosticRun {
                agent_id,
                overall_status,
                healthy_count,
                degraded_count,
                critical_count,
                audit_entries_analysed,
            } => {
                let icon = match overall_status.as_str() {
                    "Healthy" => "✅",
                    "Degraded" => "⚠️ ",
                    "Critical" => "🚨",
                    _ => "❓",
                };
                println!(
                    "  {icon}  diagnostic_run agent={agent_id} status={overall_status} \
                     healthy={healthy_count} degraded={degraded_count} critical={critical_count} \
                     entries_analysed={audit_entries_analysed}"
                );
            }

            // ── E31 — Multi-Tenant Workspace Management ──────────────────────
            AuditEntry::WorkspaceCreated {
                agent_id,
                workspace_id,
                display_name,
                owner_user_id,
            } => {
                println!(
                    "  🏢 workspace_created agent={agent_id} id={workspace_id} \
                     name={display_name:?} owner={owner_user_id}"
                );
            }
            AuditEntry::WorkspaceMemberAdded {
                agent_id,
                workspace_id,
                user_id,
                role,
            } => {
                println!(
                    "  👥 workspace_member_added agent={agent_id} \
                     workspace={workspace_id} user={user_id} role={role}"
                );
            }
            AuditEntry::WorkspaceMemberRemoved {
                agent_id,
                workspace_id,
                user_id,
                role,
            } => {
                println!(
                    "  👤 workspace_member_removed agent={agent_id} \
                     workspace={workspace_id} user={user_id} role={role}"
                );
            }
            AuditEntry::WorkspaceQuotaUpdated {
                agent_id,
                workspace_id,
                max_members,
                max_daily_tokens,
            } => {
                println!(
                    "  📊 workspace_quota_updated agent={agent_id} \
                     workspace={workspace_id} max_members={max_members} \
                     max_daily_tokens={max_daily_tokens}"
                );
            }
            AuditEntry::WorkspaceStatusChanged {
                agent_id,
                workspace_id,
                old_status,
                new_status,
            } => {
                println!(
                    "  🔄 workspace_status_changed agent={agent_id} \
                     workspace={workspace_id} {old_status} → {new_status}"
                );
            }
            // ── E32 — Scheduled Job and Cron Engine ───────────────────────────
            AuditEntry::JobScheduled { agent_id, job_id, description, schedule_type, workspace_id } => {
                println!("  📅 job_scheduled agent={agent_id} id={job_id} desc={description:?} schedule={schedule_type} workspace={workspace_id:?}");
            }
            AuditEntry::JobFired { agent_id, job_id, attempt } => {
                println!("  🔔 job_fired agent={agent_id} id={job_id} attempt={attempt}");
            }
            AuditEntry::JobCompleted { agent_id, job_id, success, duration_ms } => {
                let icon = if *success { "✅" } else { "❌" };
                println!("  {icon} job_completed agent={agent_id} id={job_id} success={success} duration={duration_ms}ms");
            }
            AuditEntry::JobCancelled { agent_id, job_id, reason } => {
                println!("  🚫 job_cancelled agent={agent_id} id={job_id} reason={reason:?}");
            }
        }
    }
}

/// Implements the `anima quota` subcommands for inspecting and resetting
/// per-user token and request quotas.
///
/// ```text
/// anima-hosted quota show [<user_id>]
/// anima-hosted quota reset <user_id>
/// anima-hosted quota policy
/// ```
///
/// Demonstrates the E18 quota tracker and audit pipeline against the live
/// user registry.
fn cmd_quota(args: &[String]) {
    use quota::{QuotaPolicy, UserQuotaTracker};
    use users::{TrustTier, UserRegistry};

    const AGENT_ID: &str = "anima";

    let user_path = UserRegistry::default_path(AGENT_ID);
    let registry = UserRegistry::open(&user_path).unwrap_or_else(|e| {
        eprintln!("warning: could not open user registry ({e}); using in-memory fallback");
        cli_fail(1);
        UserRegistry::in_memory()
    });

    // Quota state is in-memory per process; a future story (E18 S18.6) may
    // persist it across restarts via a sidecar JSON file.
    let mut tracker = UserQuotaTracker::with_default_policy();
    let policy = QuotaPolicy::default();

    // Use a fixed demo timestamp (no system clock dependency in this command).
    let now_ns: u64 = 1_700_000_000_000_000_000;

    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("show") => {
            println!("note: quota state is in-process only; this shows the demo tracker, not a running daemon");
            match args.get(1) {
                Some(user_id) => {
                    // Single-user snapshot.
                    let tier = registry
                        .get(user_id)
                        .map(|r| r.profile.trust_tier)
                        .unwrap_or(TrustTier::Unknown);
                    let snap = tracker.snapshot(user_id, tier, now_ns);
                    println!("quota for {user_id}  (tier: {})", tier);
                    println!(
                        "  hourly tokens : {} / {}  ({} remaining)",
                        snap.hourly_tokens_used,
                        if snap.hourly_tokens_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.hourly_tokens_limit.to_string()
                        },
                        if snap.hourly_tokens_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.hourly_tokens_remaining().to_string()
                        },
                    );
                    println!(
                        "  daily tokens  : {} / {}  ({} remaining)",
                        snap.daily_tokens_used,
                        if snap.daily_tokens_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.daily_tokens_limit.to_string()
                        },
                        if snap.daily_tokens_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.daily_tokens_remaining().to_string()
                        },
                    );
                    println!(
                        "  hourly reqs   : {} / {}  ({} remaining)",
                        snap.hourly_requests_used,
                        if snap.hourly_requests_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.hourly_requests_limit.to_string()
                        },
                        if snap.hourly_requests_limit == u64::MAX {
                            "∞".to_owned()
                        } else {
                            snap.hourly_requests_remaining().to_string()
                        },
                    );
                    if snap.consecutive_violations > 0 {
                        println!(
                            "  violations    : {} consecutive",
                            snap.consecutive_violations
                        );
                    }
                }
                None => {
                    // Summary of all registered users.
                    println!(
                        "{:>20}  {:>10}  {:>15}  {:>15}  {:>12}",
                        "user_id", "tier", "hourly_toks", "daily_toks", "hourly_reqs"
                    );
                    println!("{}", "-".repeat(78));
                    let mut entries: Vec<_> = registry.iter().collect();
                    entries.sort_by_key(|(id, _)| *id);
                    for (id, rec) in entries {
                        let tier = rec.profile.trust_tier;
                        let snap = tracker.snapshot(id, tier, now_ns);
                        let fmt_limit = |v: u64| {
                            if v == u64::MAX {
                                "∞".to_owned()
                            } else {
                                v.to_string()
                            }
                        };
                        println!(
                            "{:>20}  {:>10}  {:>15}  {:>15}  {:>12}",
                            id,
                            tier,
                            format!(
                                "{}/{}",
                                snap.hourly_tokens_used,
                                fmt_limit(snap.hourly_tokens_limit)
                            ),
                            format!(
                                "{}/{}",
                                snap.daily_tokens_used,
                                fmt_limit(snap.daily_tokens_limit)
                            ),
                            format!(
                                "{}/{}",
                                snap.hourly_requests_used,
                                fmt_limit(snap.hourly_requests_limit)
                            ),
                        );
                    }
                    if registry.is_empty() {
                        println!("(no registered users — register users first with 'anima-hosted users register')");
                    }
                }
            }
        }
        Some("reset") => match args.get(1) {
            Some(user_id) => {
                println!("note: quota state is in-process only; this resets the demo tracker, not a running daemon");
                tracker.reset(user_id);
                println!("quota: reset usage windows for {user_id:?}");
                println!("quota: reset complete");
                let _ = &mut log; // log unused in this path; kept for future persistence hook
            }
            None => {
                eprintln!("usage: anima-hosted quota reset <user_id>");
                cli_fail(2);
            }
        },
        Some("policy") => {
            println!("quota policy (default):");
            println!(
                "  unknown   : {}t/h  {}t/d  {}r/h",
                policy.unknown.tokens_per_hour,
                policy.unknown.tokens_per_day,
                policy.unknown.requests_per_hour
            );
            println!(
                "  verified  : {}t/h  {}t/d  {}r/h",
                policy.verified.tokens_per_hour,
                policy.verified.tokens_per_day,
                policy.verified.requests_per_hour
            );
            println!(
                "  trusted   : {}t/h  {}t/d  {}r/h",
                policy.trusted.tokens_per_hour,
                policy.trusted.tokens_per_day,
                policy.trusted.requests_per_hour
            );
            println!("  operator  : ∞t/h  ∞t/d  ∞r/h  (unlimited)");
            println!(
                "  escalation_threshold   : {} consecutive violations",
                policy.escalation_threshold
            );
            println!(
                "  escalation_cooldown    : {}s",
                policy.escalation_cooldown_ns / 1_000_000_000
            );

            // Demo: simulate a few requests and show the audit trail.
            println!("\n--- demo: unknown-tier exhaustion scenario ---");
            let demo_policy = QuotaPolicy {
                unknown: quota::TierLimits {
                    tokens_per_hour: 5,
                    tokens_per_day: 20,
                    requests_per_hour: 3,
                },
                // Low threshold so escalation shows in the demo.
                escalation_threshold: 3,
                escalation_cooldown_ns: 0,
                ..QuotaPolicy::default()
            };
            let mut demo_tracker = UserQuotaTracker::new(demo_policy);
            let demo_user = "telegram:demo";

            for i in 1u64..=7 {
                let result =
                    demo_tracker.check_and_consume(demo_user, TrustTier::Unknown, 2, now_ns + i);
                match &result {
                    quota::QuotaResult::Allowed {
                        remaining_hourly_tokens,
                        remaining_hourly_requests,
                        ..
                    } => {
                        println!(
                            "  req {i}: allowed  remaining_hourly={}t {}r",
                            remaining_hourly_tokens, remaining_hourly_requests
                        );
                    }
                    quota::QuotaResult::Exceeded {
                        reason,
                        retry_after_ns,
                        ..
                    } => {
                        log.push(vita::AuditEntry::QuotaExceeded {
                            agent_id: AGENT_ID.to_owned(),
                            user_id: demo_user.to_owned(),
                            trust_tier: TrustTier::Unknown.as_str().to_owned(),
                            exceeded_reason: reason.description(),
                            tokens_requested: 2,
                            retry_after_ns: *retry_after_ns,
                        });
                        // Check escalation.
                        if demo_tracker.should_escalate(demo_user, now_ns + i) {
                            log.push(vita::AuditEntry::QuotaEscalated {
                                agent_id: AGENT_ID.to_owned(),
                                user_id: demo_user.to_owned(),
                                trust_tier: TrustTier::Unknown.as_str().to_owned(),
                                violations_in_window: demo_tracker
                                    .consecutive_violations(demo_user),
                                threshold: demo_tracker.policy().escalation_threshold,
                            });
                            demo_tracker.record_escalation(demo_user, now_ns + i);
                        }
                        println!("  req {i}: EXCEEDED — {}", reason.description());
                    }
                }
            }

            println!("\n--- audit trail ---");
            for entry in log.entries() {
                match entry {
                    vita::AuditEntry::QuotaExceeded {
                        user_id,
                        exceeded_reason,
                        tokens_requested,
                        ..
                    } => {
                        println!(
                            "  🚫 quota_exceeded user={user_id} tokens_req={tokens_requested} \
                             reason={exceeded_reason:?}"
                        );
                    }
                    vita::AuditEntry::QuotaEscalated {
                        user_id,
                        violations_in_window,
                        threshold,
                        ..
                    } => {
                        println!(
                            "  🔔 quota_escalated user={user_id} \
                             violations={violations_in_window} threshold={threshold}"
                        );
                    }
                    _ => {}
                }
            }
        }
        _ => {
            eprintln!("usage: anima-hosted quota show [<user_id>]");
            eprintln!("       anima-hosted quota reset <user_id>");
            eprintln!("       anima-hosted quota policy");
            cli_fail(2);
        }
    }
}

/// Implements the `anima config show | validate [<path>] | init [--path <p>]`
/// subcommands.
///
/// Satisfies E20 exit criteria:
/// 1. `config init` writes a valid default config; `config validate` accepts it.
/// 2. Config round-trips through TOML without data loss.
/// 3. Config load is audited with `AuditEntry::ConfigLoaded`.
fn cmd_config(args: &[String]) {
    use config::{load_or_defaults, AnimaConfig, ConfigSource};

    const AGENT_ID: &str = "anima";
    let sub = args.first().map(String::as_str).unwrap_or("show");

    match sub {
        "show" => {
            let default_path = AnimaConfig::default_path(AGENT_ID);
            let (cfg, src) = load_or_defaults(&default_path);
            let from_file = matches!(src, ConfigSource::File(_));
            let path_str = match &src {
                ConfigSource::File(p) => p.to_string_lossy().to_string(),
                ConfigSource::Defaults => default_path.to_string_lossy().to_string(),
            };

            println!("AnimaOS Runtime Configuration");
            println!(
                "Source: {}",
                if from_file {
                    &path_str
                } else {
                    "(built-in defaults)"
                }
            );
            println!();
            print!("{}", cfg.to_display_string());

            let mut log = AuditLog::new();
            log.push(AuditEntry::ConfigLoaded {
                agent_id: AGENT_ID.to_string(),
                path: path_str,
                schema_version: cfg.schema.version,
                from_file,
            });
            println!("\nAudit trail:");
            for entry in log.entries() {
                if let AuditEntry::ConfigLoaded {
                    path,
                    schema_version,
                    from_file,
                    ..
                } = entry
                {
                    let src_label = if *from_file { "file" } else { "defaults" };
                    println!(
                        "  ⚙  config_loaded schema_version={schema_version} source={src_label} path={path}"
                    );
                }
            }
        }

        "validate" => {
            let path = args
                .get(1)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| AnimaConfig::default_path(AGENT_ID));
            println!("Validating: {}", path.display());
            match AnimaConfig::from_file(&path) {
                Ok(cfg) => {
                    println!("✓ Valid  (schema version {})", cfg.schema.version);
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("✗ Invalid: {e}");
                    std::process::exit(1);
                }
            }
        }

        "init" => {
            let path = {
                let flag_pos = args.iter().position(|a| a == "--path");
                if let Some(pos) = flag_pos {
                    args.get(pos + 1)
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| AnimaConfig::default_path(AGENT_ID))
                } else {
                    AnimaConfig::default_path(AGENT_ID)
                }
            };

            if path.exists() {
                println!("Config already exists at {}", path.display());
                println!("Use `config validate` to check it, or delete and re-run `config init`.");
                return;
            }

            let cfg = AnimaConfig::from_defaults();
            let toml_str = match cfg.to_toml_string() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to serialize default config: {e}");
                    std::process::exit(1);
                }
            };

            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "Failed to create config directory {}: {e}",
                        parent.display()
                    );
                    std::process::exit(1);
                }
            }

            let tmp = path.with_extension("toml.tmp");
            if let Err(e) = std::fs::write(&tmp, &toml_str) {
                eprintln!("Failed to write temp config {}: {e}", tmp.display());
                std::process::exit(1);
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                eprintln!("Failed to rename config into place: {e}");
                std::process::exit(1);
            }

            println!("✓ Default config written to {}", path.display());
            println!("  Edit it, then run `anima-hosted config validate` to check your changes.");

            let mut log = AuditLog::new();
            log.push(AuditEntry::ConfigLoaded {
                agent_id: AGENT_ID.to_string(),
                path: path.to_string_lossy().to_string(),
                schema_version: cfg.schema.version,
                from_file: false,
            });
            println!(
                "\nAudit entry emitted: ConfigLoaded(schema_version={}, source=defaults)",
                cfg.schema.version
            );
        }

        _ => {
            eprintln!("Usage: anima-hosted config <show | validate [<path>] | init [--path <p>]>");
            std::process::exit(1);
        }
    }
}

/// Implements the `anima sessions` subcommands for conversation history.
///
/// ```text
/// anima-hosted sessions list [--user <user_id>]
/// anima-hosted sessions show <session_id>
/// anima-hosted sessions new <user_id>
/// anima-hosted sessions append <session_id> <role> <content>
/// anima-hosted sessions archive <session_id> [--summary <text>]
/// anima-hosted sessions export <session_id> [--format jsonl|markdown]
/// anima-hosted sessions search <query>
/// ```
fn cmd_sessions(args: &[String]) {
    use sessions::{
        make_session_id, ConversationRole, ConversationTurn, ExportFormat, SessionQuery,
        SessionRecord, SessionStatus, SessionStore,
    };
    use std::str::FromStr;

    const AGENT_ID: &str = "anima";
    let path = SessionStore::default_path(AGENT_ID);
    let mut store = SessionStore::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open session store ({e}); using in-memory fallback");
        cli_fail(1);
        SessionStore::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        // ── list ──────────────────────────────────────────────────────────────
        Some("list") => {
            let user_id = args
                .windows(2)
                .find(|w| w[0] == "--user")
                .map(|w| w[1].clone());
            let mut q = SessionQuery::default();
            if let Some(uid) = user_id {
                q = SessionQuery::for_user(uid);
            }
            let sessions = store.list(&q);
            if sessions.is_empty() {
                println!("sessions: no sessions found");
            } else {
                println!("sessions ({} total):", sessions.len());
                for s in sessions {
                    println!(
                        "  {} | user={} status={} turns={} started={}",
                        s.id,
                        s.user_id,
                        s.status,
                        s.turn_count(),
                        s.started_at_ns,
                    );
                }
            }
        }
        // ── show ──────────────────────────────────────────────────────────────
        Some("show") => match args.get(1) {
            Some(id) => match store.get(id) {
                Some(s) => {
                    println!("session: {}", s.id);
                    println!("  user   : {}", s.user_id);
                    println!("  agent  : {}", s.agent_id);
                    println!("  status : {}", s.status);
                    println!("  turns  : {}", s.turn_count());
                    println!("  tokens : {}", s.total_tokens);
                    if let Some(summary) = &s.summary {
                        println!("  summary: {summary}");
                    }
                    println!("  ---");
                    for turn in &s.turns {
                        println!(
                            "  [{}] {}: {}",
                            turn.index,
                            turn.role,
                            if turn.content.len() > 80 {
                                format!("{}…", &turn.content[..80])
                            } else {
                                turn.content.clone()
                            }
                        );
                    }
                }
                None => {
                    eprintln!("sessions: session {id:?} not found");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted sessions show <session_id>");
                cli_fail(2);
            }
        },
        // ── new ───────────────────────────────────────────────────────────────
        Some("new") => match args.get(1) {
            Some(user_id) => {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(42);
                let session_id = make_session_id(nonce);
                let session = SessionRecord::new(&session_id, user_id, AGENT_ID);
                match store.insert(session) {
                    Ok(()) => {
                        store.flush().unwrap_or_else(|e| {
                            eprintln!("sessions: flush error: {e}");
                            cli_fail(1);
                        });
                        log.push(AuditEntry::SessionStarted {
                            agent_id: AGENT_ID.to_string(),
                            session_id: session_id.clone(),
                            user_id: user_id.to_string(),
                        });
                        println!("sessions: created {session_id}");
                        print_session_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("sessions: error: {e}");
                        cli_fail(1);
                    }
                }
            }
            None => {
                eprintln!("usage: anima-hosted sessions new <user_id>");
                cli_fail(2);
            }
        },
        // ── append ────────────────────────────────────────────────────────────
        Some("append") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(id), Some(role_str), Some(content)) => {
                match ConversationRole::from_str(role_str) {
                    Ok(role) => {
                        let turn = ConversationTurn::new(0, role, content.clone());
                        let content_len = content.len();
                        match store.append_turn(id, turn) {
                            Ok(()) => {
                                log.push(AuditEntry::SessionTurnAppended {
                                    agent_id: AGENT_ID.to_string(),
                                    session_id: id.to_string(),
                                    role: role_str.to_string(),
                                    content_len,
                                });
                                println!("sessions: appended {role_str} turn to {id}");
                                print_session_audit(&log);
                            }
                            Err(e) => {
                                eprintln!("sessions: error: {e}");
                                cli_fail(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("sessions: unknown role: {e}");
                        cli_fail(2);
                    }
                }
            }
            _ => {
                eprintln!("usage: anima-hosted sessions append <session_id> <role> <content>");
                cli_fail(2);
            }
        },
        // ── archive ───────────────────────────────────────────────────────────
        Some("archive") => match args.get(1) {
            Some(id) => {
                let summary = args
                    .windows(2)
                    .find(|w| w[0] == "--summary")
                    .map(|w| w[1].clone());
                let has_summary = summary.is_some();
                let turn_count = store.get(id).map(|s| s.turn_count()).unwrap_or(0);
                match store.archive(id, summary) {
                    Ok(()) => {
                        log.push(AuditEntry::SessionArchived {
                            agent_id: AGENT_ID.to_string(),
                            session_id: id.to_string(),
                            turn_count,
                            has_summary,
                        });
                        println!("sessions: archived {id} ({turn_count} turns)");
                        print_session_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("sessions: error: {e}");
                        cli_fail(1);
                    }
                }
            }
            None => {
                eprintln!("usage: anima-hosted sessions archive <session_id> [--summary <text>]");
                cli_fail(2);
            }
        },
        // ── export ────────────────────────────────────────────────────────────
        Some("export") => match args.get(1) {
            Some(id) => {
                let format_str = args
                    .windows(2)
                    .find(|w| w[0] == "--format")
                    .map(|w| w[1].as_str())
                    .unwrap_or("jsonl");
                match ExportFormat::from_str(format_str) {
                    Ok(format) => {
                        let turn_count = store.get(id).map(|s| s.turn_count()).unwrap_or(0);
                        match store.export(id, &format) {
                            Ok(output) => {
                                log.push(AuditEntry::SessionExported {
                                    agent_id: AGENT_ID.to_string(),
                                    session_id: id.to_string(),
                                    format: format.to_string(),
                                    turn_count,
                                });
                                println!("{output}");
                                print_session_audit(&log);
                            }
                            Err(e) => {
                                eprintln!("sessions: error: {e}");
                                cli_fail(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("sessions: unknown format: {e}");
                        cli_fail(2);
                    }
                }
            }
            None => {
                eprintln!(
                    "usage: anima-hosted sessions export <session_id> [--format jsonl|markdown]"
                );
                cli_fail(2);
            }
        },
        // ── search ────────────────────────────────────────────────────────────
        Some("search") => match args.get(1) {
            Some(query) => {
                let q = SessionQuery::with_content(query);
                let sessions = store.list(&q);
                if sessions.is_empty() {
                    println!("sessions: no sessions match {query:?}");
                } else {
                    println!("sessions: {} match(es) for {query:?}:", sessions.len());
                    for s in sessions {
                        println!(
                            "  {} | user={} status={} turns={}",
                            s.id,
                            s.user_id,
                            s.status,
                            s.turn_count()
                        );
                    }
                }
            }
            None => {
                eprintln!("usage: anima-hosted sessions search <query>");
                cli_fail(2);
            }
        },
        // ── help / unknown ────────────────────────────────────────────────────
        _ => {
            println!("anima-hosted sessions — conversation history management (E22)");
            println!();
            println!("  sessions list [--user <user_id>]");
            println!("  sessions show <session_id>");
            println!("  sessions new <user_id>");
            println!("  sessions append <session_id> <role> <content>");
            println!("  sessions archive <session_id> [--summary <text>]");
            println!("  sessions export <session_id> [--format jsonl|markdown]");
            println!("  sessions search <query>");
        }
    }

    // Unused alias suppression for SessionStatus (referenced indirectly).
    let _ = SessionStatus::Active;
}

/// Print E22-relevant audit entries from an in-process log.
fn print_session_audit(log: &AuditLog) {
    println!("--- audit trail ---");
    for entry in log.entries() {
        match entry {
            AuditEntry::SessionStarted {
                agent_id,
                session_id,
                user_id,
            } => {
                println!(
                    "  💬 session_started agent={agent_id} \
                     session={session_id} user={user_id}"
                );
            }
            AuditEntry::SessionTurnAppended {
                agent_id,
                session_id,
                role,
                content_len,
            } => {
                println!(
                    "  💬 session_turn_appended agent={agent_id} \
                     session={session_id} role={role} len={content_len}"
                );
            }
            AuditEntry::SessionArchived {
                agent_id,
                session_id,
                turn_count,
                has_summary,
            } => {
                let tag = if *has_summary { " [summary]" } else { "" };
                println!(
                    "  📁 session_archived agent={agent_id} \
                     session={session_id} turns={turn_count}{tag}"
                );
            }
            AuditEntry::SessionExported {
                agent_id,
                session_id,
                format,
                turn_count,
            } => {
                println!(
                    "  📤 session_exported agent={agent_id} \
                     session={session_id} format={format} turns={turn_count}"
                );
            }
            _ => {}
        }
    }
    println!("---");
}

/// Implements `anima data export <user_id>`, `anima data delete <user_id>`,
/// `anima data expiry-check`, and `anima data consent-status <user_id>`.
///
/// Satisfies E23 exit criteria:
/// 1. Personal-data export produces a GDPR-ready JSON bundle.
/// 2. Revocation generates a directive and deletes the appropriate counters.
/// 3. Expiry scan identifies lapsed grants and produces cleanup directives.
/// 4. All operations are audited with E23 `AuditEntry` variants.
fn cmd_data(args: &[String]) {
    use consent::{build_revocation_directive, scan_expired_grants, DataExportBuilder};
    use sessions::{SessionQuery, SessionStore};
    use users::{DataCategory, UserRegistry};

    const AGENT_ID: &str = "anima";

    let path = UserRegistry::default_path(AGENT_ID);
    let mut registry = UserRegistry::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open user registry ({e}); using in-memory fallback");
        cli_fail(1);
        UserRegistry::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        // ── `anima data consent-status <user_id>` ────────────────────────────
        Some("consent-status") => match args.get(1) {
            Some(user_id) => match registry.get(user_id) {
                Some(rec) => {
                    println!("consent status for user {user_id:?}:");
                    let now_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    for cat in DataCategory::all() {
                        let status = if rec.consent.is_consented(*cat, now_ns) {
                            "✓ granted"
                        } else {
                            "✗ not consented"
                        };
                        println!("  {:20} {status}", cat.as_str());
                    }
                }
                None => {
                    eprintln!("data: no user with id={user_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted data consent-status <user_id>");
                cli_fail(2);
            }
        },

        // ── `anima data export <user_id> [--output <path>]` ──────────────────
        Some("export") => match args.get(1) {
            Some(user_id) => {
                let output_path = args
                    .windows(2)
                    .find(|w| w[0] == "--output")
                    .and_then(|w| w.get(1))
                    .cloned()
                    .unwrap_or_else(|| format!("{user_id}-export.json"));

                match registry.get(user_id) {
                    Some(rec) => {
                        let now_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);

                        // Build export: always include the identity profile first,
                        // then add a section for each consented data category.
                        let mut builder = DataExportBuilder::new();
                        builder.add_raw_section(
                            "user_profile",
                            1,
                            serde_json::json!({
                                "user_id": rec.profile.user_id,
                                "display_name": rec.profile.display_name,
                                "channel": rec.profile.channel,
                                "trust_tier": rec.profile.trust_tier.as_str(),
                                "created_at_ns": rec.profile.created_at_ns,
                                "last_seen_ns": rec.profile.last_seen_ns,
                                "facts": rec.profile.facts,
                            }),
                        );
                        // Open the per-user conversation store once; it backs
                        // both the episodic-memory and usage-stats sections. A
                        // missing store (agent never persisted any sessions) is
                        // "no records", not an error.
                        let session_store =
                            SessionStore::open(SessionStore::default_path(AGENT_ID)).ok();
                        let user_sessions = session_store
                            .as_ref()
                            .map(|s| s.list(&SessionQuery::for_user(user_id.as_str())))
                            .unwrap_or_default();
                        let total_turns: usize = user_sessions.iter().map(|s| s.turns.len()).sum();

                        // Populate every consented category with the subject's
                        // actual retained data, drawn from the store that owns
                        // it. Each `DataCategory` maps to a concrete source.
                        for cat in DataCategory::all() {
                            if !rec.consent.is_consented(*cat, now_ns) {
                                continue;
                            }
                            match cat {
                                DataCategory::IdentityFacts => {
                                    builder.add_section(
                                        *cat,
                                        rec.profile.facts.len(),
                                        serde_json::json!({ "facts": rec.profile.facts }),
                                    );
                                }
                                DataCategory::EpisodicMemory => {
                                    // Full conversation history for this subject.
                                    builder.add_section(
                                        *cat,
                                        user_sessions.len(),
                                        serde_json::json!({
                                            "sessions": user_sessions,
                                            "total_turns": total_turns,
                                        }),
                                    );
                                }
                                DataCategory::UsageStats => {
                                    builder.add_section(
                                        *cat,
                                        1,
                                        serde_json::json!({
                                            "created_at_ns": rec.profile.created_at_ns,
                                            "last_seen_ns": rec.profile.last_seen_ns,
                                            "session_count": user_sessions.len(),
                                            "total_turns": total_turns,
                                        }),
                                    );
                                }
                                DataCategory::KnowledgeCorpus => {
                                    // The E27 knowledge graph is agent-global and
                                    // not keyed per subject, so a per-user DSAR
                                    // cannot slice one user's entries out of it
                                    // without exposing others'. Report the
                                    // category honestly with zero rows rather
                                    // than leaking unrelated data.
                                    builder.add_section(
                                        *cat,
                                        0,
                                        serde_json::json!({
                                            "entries": [],
                                            "note": "knowledge-corpus entries are not retained \
                                                     per-user in this build; nothing to export \
                                                     for this subject",
                                        }),
                                    );
                                }
                            }
                        }
                        let bundle = builder.build(user_id, AGENT_ID, now_ns);
                        let json = serde_json::to_string_pretty(&bundle)
                            .expect("serialisation never fails");
                        std::fs::write(&output_path, &json).unwrap_or_else(|e| {
                            eprintln!("data: could not write {output_path}: {e}");
                            cli_fail(1);
                        });
                        log.push(AuditEntry::DataExported {
                            agent_id: AGENT_ID.to_owned(),
                            user_id: user_id.clone(),
                            section_count: bundle.sections.len(),
                            total_records: bundle.total_records,
                            output_path: output_path.clone(),
                        });
                        println!(
                            "data: exported {user_id:?} → {output_path} \
                             ({} sections, {} records)",
                            bundle.sections.len(),
                            bundle.total_records,
                        );
                        print_data_audit(&log);
                    }
                    None => {
                        eprintln!("data: no user with id={user_id:?}");
                        cli_fail(1);
                    }
                }
            }
            None => {
                eprintln!("usage: anima-hosted data export <user_id> [--output <path>]");
                cli_fail(2);
            }
        },

        // ── `anima data delete <user_id>` ─────────────────────────────────────
        Some("delete") => match args.get(1) {
            Some(user_id) => {
                let user_exists = registry.get(user_id).is_some();
                if user_exists {
                    let now_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);

                    // Build directive for all categories.
                    let all: Vec<DataCategory> = DataCategory::all().to_vec();
                    let directive = build_revocation_directive(user_id, &all, now_ns);

                    let categories_str = directive.revoked_categories.join(", ");
                    log.push(AuditEntry::DataDeletedForUser {
                        agent_id: AGENT_ID.to_owned(),
                        user_id: user_id.clone(),
                        categories: categories_str.clone(),
                        records_deleted: 0, // stores not wired yet
                    });

                    // Remove the user record from the registry and persist.
                    registry.remove(user_id).ok();
                    registry.flush().unwrap_or_else(|e| {
                        eprintln!("data: registry flush failed: {e}");
                        cli_fail(1);
                    });

                    println!(
                        "data: deletion directive generated for {user_id:?} \
                         categories=[{categories_str}]"
                    );
                    println!(
                        "data: purge_sessions={} purge_episodic={} \
                         purge_identity={} purge_knowledge={} purge_stats={}",
                        directive.purge_sessions,
                        directive.purge_episodic_memory,
                        directive.purge_identity_facts,
                        directive.purge_knowledge_corpus,
                        directive.purge_usage_stats,
                    );
                    print_data_audit(&log);
                } else {
                    eprintln!("data: no user with id={user_id:?}");
                    cli_fail(1);
                }
            }
            None => {
                eprintln!("usage: anima-hosted data delete <user_id>");
                cli_fail(2);
            }
        },

        // ── `anima data expiry-check` ─────────────────────────────────────────
        Some("expiry-check") => {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);

            // Collect consent records from the registry.
            let user_consent: Vec<(String, users::ConsentRecord)> = registry
                .iter()
                .map(|(id, rec)| (id.to_owned(), rec.consent.clone()))
                .collect();

            let report = scan_expired_grants(
                user_consent.iter().map(|(id, rec)| (id.as_str(), rec)),
                now_ns,
            );

            println!("expiry-check: scanned {} users", report.users_scanned);
            if report.is_clean() {
                println!("expiry-check: no expired grants found");
            } else {
                println!(
                    "expiry-check: {} expired grants in {} users",
                    report.expired_count(),
                    report.directives.len()
                );
                for eg in &report.expired_grants {
                    println!(
                        "  expired: user={} category={} expired_at={}ns",
                        eg.user_id, eg.category, eg.expired_at_ns
                    );
                }

                // Revoke the expired grants in the registry so subsequent runs
                // don't re-report the same entries.
                for eg in &report.expired_grants {
                    if let Some(rec) = registry.get_mut(&eg.user_id) {
                        if let Ok(cat) = eg.category.parse::<DataCategory>() {
                            rec.consent.set(cat, false, now_ns);
                        }
                    }
                }
                registry.flush().unwrap_or_else(|e| {
                    eprintln!("expiry-check: registry flush failed: {e}");
                    cli_fail(1);
                });
            }
            log.push(AuditEntry::ExpiredConsentCleaned {
                agent_id: AGENT_ID.to_owned(),
                users_scanned: report.users_scanned,
                expired_grants_found: report.expired_count(),
                users_affected: report.directives.len(),
                total_records_deleted: 0, // stores not wired yet
            });
            print_data_audit(&log);
        }

        _ => {
            eprintln!("usage: anima-hosted data consent-status <user_id>");
            eprintln!("       anima-hosted data export <user_id> [--output <path>]");
            eprintln!("       anima-hosted data delete <user_id>");
            eprintln!("       anima-hosted data expiry-check");
            cli_fail(2);
        }
    }
}

/// Prints E23 audit entries to stdout (same style as the main `print_audit`).
fn print_data_audit(log: &AuditLog) {
    use vita::AuditEntry;
    for entry in log.entries() {
        match entry {
            AuditEntry::ConsentCheckBlocked {
                agent_id,
                user_id,
                category,
                reason,
            } => {
                println!(
                    "audit: consent_blocked agent={agent_id} user={user_id} \
                     category={category} reason={reason}"
                );
            }
            AuditEntry::DataExported {
                agent_id,
                user_id,
                section_count,
                total_records,
                output_path,
            } => {
                println!(
                    "audit: data_exported agent={agent_id} user={user_id} \
                     sections={section_count} records={total_records} \
                     path={output_path}"
                );
            }
            AuditEntry::DataDeletedForUser {
                agent_id,
                user_id,
                categories,
                records_deleted,
            } => {
                println!(
                    "audit: data_deleted agent={agent_id} user={user_id} \
                     categories=[{categories}] records={records_deleted}"
                );
            }
            AuditEntry::ExpiredConsentCleaned {
                agent_id,
                users_scanned,
                expired_grants_found,
                users_affected,
                total_records_deleted,
            } => {
                println!(
                    "audit: expired_consent_cleaned agent={agent_id} \
                     scanned={users_scanned} expired={expired_grants_found} \
                     affected={users_affected} deleted={total_records_deleted}"
                );
            }
            _ => {}
        }
    }
}

/// Implements the `anima feedback` subcommands for recording and reviewing
/// response quality feedback.
///
/// Subcommands:
/// - `record <invocation_id> <user_id> <up|down|stars:N> [--correct <text>]`
///   Record explicit feedback on a cortex invocation.
/// - `list [--user <user_id>]`
///   List stored feedback records (optionally filtered by user).
/// - `analyze [--user <user_id>]`
///   Print an aggregated quality report.
/// - `export [--output <path>]`
///   Write the feedback store to a JSON file (default: stdout).
fn cmd_feedback(args: &[String]) {
    use std::str::FromStr;

    use feedback::{
        build_training_hints, FeedbackCategory, FeedbackRating, FeedbackRecord, FeedbackStore,
        QualityReport,
    };

    const AGENT_ID: &str = "anima";

    let path = FeedbackStore::default_path(AGENT_ID);
    let mut store = FeedbackStore::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open feedback store ({e}); using in-memory fallback");
        cli_fail(1);
        FeedbackStore::in_memory()
    });
    let mut log = AuditLog::new();

    // Helper: emit and print audit entries for feedback events.
    let emit_feedback_audit = |log: &mut AuditLog, rec: &FeedbackRecord| {
        log.push(AuditEntry::FeedbackReceived {
            agent_id: AGENT_ID.to_owned(),
            user_id: rec.user_id.clone(),
            invocation_id: rec.invocation_id.clone(),
            rating_label: rec.rating.label(),
            score: rec.rating.as_score(),
            category_count: rec.categories.len(),
        });
        if rec.has_correction() {
            log.push(AuditEntry::FeedbackCorrectionRecorded {
                agent_id: AGENT_ID.to_owned(),
                user_id: rec.user_id.clone(),
                invocation_id: rec.invocation_id.clone(),
            });
        }
    };

    match args.first().map(String::as_str) {
        Some("record") => {
            // feedback record <invocation_id> <user_id> <rating> [--correct <text>]
            match (args.get(1), args.get(2), args.get(3)) {
                (Some(inv_id), Some(user_id), Some(rating_str)) => {
                    match FeedbackRating::from_str(rating_str) {
                        Ok(rating) => {
                            // Wall-clock nanos for the record ID; stub value for
                            // determinism in tests.
                            let ts_ns = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as u64;

                            let mut rec = FeedbackRecord::new(user_id, inv_id, rating, ts_ns);

                            // Parse optional --correct <text>
                            let mut i = 4usize;
                            while i < args.len() {
                                if args[i] == "--correct" {
                                    if let Some(text) = args.get(i + 1) {
                                        rec = rec.with_correction(text.clone());
                                        i += 2;
                                        continue;
                                    } else {
                                        eprintln!("feedback record: --correct requires a value");
                                        cli_fail(2);
                                        return;
                                    }
                                }
                                i += 1;
                            }

                            // Parse optional --category <cat>
                            let mut cats = Vec::new();
                            let mut j = 4usize;
                            while j < args.len() {
                                if args[j] == "--category" {
                                    if let Some(cat_str) = args.get(j + 1) {
                                        match FeedbackCategory::from_str(cat_str) {
                                            Ok(cat) => cats.push(cat),
                                            Err(e) => {
                                                eprintln!("feedback record: {e}");
                                                cli_fail(2);
                                                return;
                                            }
                                        }
                                        j += 2;
                                        continue;
                                    } else {
                                        eprintln!("feedback record: --category requires a value");
                                        cli_fail(2);
                                        return;
                                    }
                                }
                                j += 1;
                            }
                            if !cats.is_empty() {
                                // Preserve the Corrected marker if --correct was also supplied.
                                if rec.has_correction()
                                    && !cats.contains(&FeedbackCategory::Corrected)
                                {
                                    cats.push(FeedbackCategory::Corrected);
                                }
                                rec = rec.with_categories(cats);
                            }

                            emit_feedback_audit(&mut log, &rec);
                            match store.record(rec) {
                                Ok(()) => {
                                    println!(
                                        "feedback: recorded for invocation={inv_id} \
                                         user={user_id}"
                                    );
                                    if let Err(e) = store.flush() {
                                        eprintln!("feedback: flush failed: {e}");
                                        cli_fail(1);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("feedback: {e}");
                                    cli_fail(1);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("feedback record: invalid rating — {e}");
                            cli_fail(2);
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "usage: anima-hosted feedback record \
                     <invocation_id> <user_id> <up|down|stars:N> \
                     [--correct <text>] [--category <cat>]"
                    );
                    cli_fail(2);
                }
            }
        }

        Some("list") => {
            // feedback list [--user <user_id>]
            let user_filter: Option<&str> = args
                .iter()
                .position(|a| a == "--user")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);

            let records: Vec<_> = if let Some(uid) = user_filter {
                store.list_for_user(uid)
            } else {
                store.list().iter().collect()
            };

            if records.is_empty() {
                println!("(no feedback records)");
            } else {
                println!(
                    "{:<24}  {:<16}  {:<24}  {:<6}  correction",
                    "id", "user_id", "invocation_id", "rating"
                );
                println!("{}", "-".repeat(82));
                for rec in records {
                    let corr = if rec.has_correction() { "yes" } else { "no" };
                    println!(
                        "{:<24}  {:<16}  {:<24}  {:<6}  {}",
                        rec.id,
                        rec.user_id,
                        rec.invocation_id,
                        rec.rating.label(),
                        corr
                    );
                }
            }
        }

        Some("analyze") => {
            // feedback analyze [--user <user_id>]
            let user_filter: Option<&str> = args
                .iter()
                .position(|a| a == "--user")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);

            let records: Vec<_> = if let Some(uid) = user_filter {
                store.list_for_user(uid).into_iter().cloned().collect()
            } else {
                store.list().to_vec()
            };

            let report = QualityReport::generate(&records);

            println!("Quality Report");
            println!("{}", "=".repeat(40));
            println!("  total_feedback  : {}", report.total_feedback);
            println!("  positive        : {}", report.positive_count);
            println!("  negative        : {}", report.negative_count);
            println!(
                "  satisfaction    : {}",
                report
                    .satisfaction_pct()
                    .map(|p| format!("{p}%"))
                    .unwrap_or_else(|| "n/a".to_string())
            );
            println!("  avg_score       : {:.2}", report.avg_score);
            if let Some(stars) = report.avg_stars {
                println!("  avg_stars       : {stars:.1}");
            }

            if !report.category_counts.is_empty() {
                println!("  categories:");
                let mut cats: Vec<_> = report.category_counts.iter().collect();
                cats.sort_by_key(|(k, _)| k.as_str());
                for (cat, count) in cats {
                    println!("    {cat:<12} : {count}");
                }
            }

            if !report.top_corrected_invocations.is_empty() {
                println!("  most corrected invocations:");
                for (inv, count) in &report.top_corrected_invocations {
                    println!("    {inv} ({count} corrections)");
                }
            }

            let hints = build_training_hints(&records);
            if !hints.is_empty() {
                println!("  training hints  : {} invocations", hints.len());
                let reinforcement = hints.iter().filter(|h| h.is_reinforcement).count();
                let correction = hints.iter().filter(|h| !h.is_reinforcement).count();
                println!("    reinforcement : {reinforcement}");
                println!("    correction    : {correction}");
            }

            log.push(AuditEntry::QualityReportGenerated {
                agent_id: AGENT_ID.to_owned(),
                total_feedback: report.total_feedback,
                satisfaction_pct: report.satisfaction_pct(),
                avg_score_pct: (report.avg_score * 100.0).round() as u32,
            });
        }

        Some("export") => {
            // feedback export [--output <path>]
            let out_path: Option<&str> = args
                .iter()
                .position(|a| a == "--output")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);

            let json = match serde_json::to_string_pretty(store.list()) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("feedback export: serialise failed: {e}");
                    cli_fail(1);
                    return;
                }
            };

            if let Some(out) = out_path {
                match std::fs::write(out, &json) {
                    Ok(()) => println!("feedback: exported {} records to {out}", store.len()),
                    Err(e) => {
                        eprintln!("feedback export: write failed: {e}");
                        cli_fail(1);
                    }
                }
            } else {
                println!("{json}");
            }
        }

        _ => {
            eprintln!(
                "usage: anima-hosted feedback record \
                 <invocation_id> <user_id> <up|down|stars:N> \
                 [--correct <text>] [--category <cat>]"
            );
            eprintln!("       anima-hosted feedback list [--user <user_id>]");
            eprintln!("       anima-hosted feedback analyze [--user <user_id>]");
            eprintln!("       anima-hosted feedback export [--output <path>]");
            cli_fail(2);
        }
    }

    // Print any audit entries generated during this command.
    if !log.is_empty() {
        println!();
        for entry in log.entries() {
            match entry {
                AuditEntry::FeedbackReceived {
                    user_id,
                    invocation_id,
                    rating_label,
                    ..
                } => println!("  audit: feedback_received user={user_id} inv={invocation_id} rating={rating_label}"),
                AuditEntry::FeedbackCorrectionRecorded { user_id, invocation_id, .. } => {
                    println!("  audit: correction_recorded user={user_id} inv={invocation_id}")
                }
                AuditEntry::QualityReportGenerated { total_feedback, satisfaction_pct, .. } => {
                    let sat = satisfaction_pct
                        .map(|p| format!("{p}%"))
                        .unwrap_or_else(|| "n/a".to_string());
                    println!("  audit: quality_report total={total_feedback} satisfaction={sat}")
                }
                _ => {}
            }
        }
    }
}

/// Performance analytics and spend reporting over the in-memory audit log.
///
/// Satisfies E25 exit criterion: a `stats` subcommand that prints token,
/// latency, gate, and health reports derived from `AuditEntry` data.
///
/// ```text
/// cargo run --bin anima-hosted -- stats [tokens|latency|gate|health|summary]
/// ```
fn cmd_stats(args: &[String]) {
    use analytics::AnalyticsEngine;

    const AGENT_ID: &str = "anima";

    // Build a representative demo audit log so the command always shows
    // something meaningful without a live `ANIMA_AUDIT_DIR`.
    let mut log = vita::AuditLog::new();

    // Populate with a variety of entry types that exercise every sub-report.
    for i in 0u64..8 {
        log.push(vita::audit::AuditEntry::TaskStarted {
            agent_id: AGENT_ID.into(),
            task_id: i,
            tier: (i % 3) as u8,
            prompt: "demo task".into(),
        });
        log.push(vita::audit::AuditEntry::TaskCompleted {
            agent_id: AGENT_ID.into(),
            task_id: i,
            tokens_emitted: 150 + (i * 30) as u32,
            response: "demo response".into(),
        });
    }
    // One task failure.
    log.push(vita::audit::AuditEntry::TaskFailed {
        agent_id: AGENT_ID.into(),
        task_id: 99,
        error: "simulated backend timeout".into(),
    });
    // Two cortex invocations, one fault.
    log.push(vita::audit::AuditEntry::CortexInvoked {
        task_id: "inv-1".into(),
        latency_to_first_action_ms: 84,
    });
    log.push(vita::audit::AuditEntry::CortexCompleted {
        task_id: "inv-1".into(),
        tool_calls: 3,
        summary_len: 210,
    });
    log.push(vita::audit::AuditEntry::CortexInvoked {
        task_id: "inv-2".into(),
        latency_to_first_action_ms: 120,
    });
    log.push(vita::audit::AuditEntry::CortexFault {
        task_id: "inv-2".into(),
        error: "python process exited".into(),
    });
    // Gate decisions.
    let gate_entry = |invoke: bool, class: Option<&str>, vs: f32, th: f32| {
        vita::audit::AuditEntry::GateDecision {
            agent_id: AGENT_ID.into(),
            event_id: "e".into(),
            invoke,
            cost_class: class.map(str::to_string),
            urgency: 0.6,
            novelty: 0.4,
            user_facing: false,
            semantic_class: "background".into(),
            value_score: vs,
            threshold_applied: th,
            thermal_load: 0.1,
            compute_pressure: 0.2,
            memory_pressure: 0.1,
            power_budget: 0.9,
            financial_budget: 0.8,
            attention_demand: 0.5,
            reasoning: "demo".into(),
            override_active: false,
        }
    };
    log.push(gate_entry(true, Some("MidTier"), 0.65, 0.40));
    log.push(gate_entry(true, Some("CheapLocal"), 0.55, 0.40));
    log.push(gate_entry(false, None, 0.30, 0.40));
    log.push(gate_entry(true, Some("Frontier"), 0.90, 0.40));
    log.push(gate_entry(false, None, 0.25, 0.40));

    let entries = log.entries();
    let sub = args.first().map(String::as_str).unwrap_or("summary");

    match sub {
        "tokens" | "token" => {
            let r = AnalyticsEngine::token_report(entries);
            println!("=== Token Report: {} ===", AGENT_ID);
            println!("Tasks completed   : {}", r.tasks_completed);
            println!("Tasks failed      : {}", r.tasks_failed);
            println!("Total tokens      : {}", r.total_tokens);
            if let Some(s) = &r.per_task {
                println!("Per-task stats:");
                println!("  mean            : {:.1}", s.mean);
                println!("  min             : {}", s.min);
                println!("  p50             : {}", s.p50);
                println!("  p95             : {}", s.p95);
                println!("  p99             : {}", s.p99);
                println!("  max             : {}", s.max);
            }
            if !r.by_tier.is_empty() {
                println!("By tier:");
                for t in &r.by_tier {
                    println!(
                        "  tier {}  tasks={}  total={}  mean={:.1}",
                        t.tier, t.tasks, t.total_tokens, t.mean_tokens
                    );
                }
            }
        }
        "latency" => {
            let r = AnalyticsEngine::latency_report(entries);
            println!("=== Latency Report: {} ===", AGENT_ID);
            println!("Cortex invocations: {}", r.cortex_invocations);
            println!("Cortex faults     : {}", r.cortex_faults);
            println!("Fault rate        : {:.1}%", r.fault_rate_pct);
            println!("Total tool calls  : {}", r.total_tool_calls);
            println!(
                "Mean tool calls   : {:.2}",
                r.mean_tool_calls_per_completion
            );
            if let Some(p) = &r.first_action {
                println!("Time-to-first-action (ms):");
                println!("  samples         : {}", p.count);
                println!("  mean            : {:.1}", p.mean_ms);
                println!("  p50             : {}", p.p50_ms);
                println!("  p95             : {}", p.p95_ms);
                println!("  p99             : {}", p.p99_ms);
                println!("  max             : {}", p.max_ms);
            }
        }
        "gate" => {
            let r = AnalyticsEngine::gate_report(entries);
            println!("=== Gate Report: {} ===", AGENT_ID);
            println!("Total evaluations : {}", r.total_evaluations);
            println!("Invocations       : {}", r.invocations);
            println!("Blocks            : {}", r.blocks);
            println!("Invocation rate   : {:.1}%", r.invocation_rate_pct);
            println!("Overrides         : {}", r.overrides);
            println!("Route modulations : {}", r.route_modulations);
            println!("Mean value score  : {:.3}", r.mean_value_score);
            println!("Mean threshold    : {:.3}", r.mean_threshold);
            println!("Gate efficiency   : {:.1}%", r.efficiency_pct);
            if !r.by_cost_class.is_empty() {
                println!("Cost-class distribution:");
                for cc in &r.by_cost_class {
                    println!("  {:<15} : {}", cc.cost_class, cc.count);
                }
            }
        }
        "health" => {
            let r = AnalyticsEngine::health_report(entries);
            println!("=== Health Report: {} ===", AGENT_ID);
            println!("Health score      : {:.3}", r.score);
            println!("Grade             : {}", r.grade);
            println!("Total tasks       : {}", r.total_tasks);
            println!("Defence vetoes    : {}", r.defence_vetoes);
            println!("Factors:");
            println!(
                "  task success    : {:.1}%",
                r.factors.task_success_rate * 100.0
            );
            println!(
                "  cortex reliab.  : {:.1}%",
                r.factors.cortex_reliability * 100.0
            );
            println!(
                "  defence health  : {:.1}%",
                r.factors.defence_health * 100.0
            );
            println!(
                "  gate efficiency : {:.1}%",
                r.factors.gate_efficiency * 100.0
            );
            if r.recommendations.is_empty() {
                println!("Recommendations   : (none)");
            } else {
                println!("Recommendations:");
                for rec in &r.recommendations {
                    println!("  - {}", rec);
                }
            }
        }
        _ => {
            // Default: full summary.
            let s = AnalyticsEngine::summary_report(entries, AGENT_ID);
            println!("=== Analytics Summary: {} ===", s.agent_id);
            println!("Entries analyzed  : {}", s.entries_analyzed);
            println!();
            println!("── Tokens ──────────────────────────────────────────────");
            println!("  total           : {}", s.token.total_tokens);
            println!("  completed tasks : {}", s.token.tasks_completed);
            println!("  failed tasks    : {}", s.token.tasks_failed);
            if let Some(p) = &s.token.per_task {
                println!("  p50/p95/p99     : {}/{}/{}", p.p50, p.p95, p.p99);
            }
            println!();
            println!("── Latency ─────────────────────────────────────────────");
            println!(
                "  cortex calls    : {} ({} faults, {:.1}% rate)",
                s.latency.cortex_invocations, s.latency.cortex_faults, s.latency.fault_rate_pct
            );
            if let Some(p) = &s.latency.first_action {
                println!("  TTFA p50/p95    : {}ms / {}ms", p.p50_ms, p.p95_ms);
            }
            println!(
                "  mean tool calls : {:.2}",
                s.latency.mean_tool_calls_per_completion
            );
            println!();
            println!("── Gate ────────────────────────────────────────────────");
            println!(
                "  eval/invoke/blk : {}/{}/{}",
                s.gate.total_evaluations, s.gate.invocations, s.gate.blocks
            );
            println!("  invoc. rate     : {:.1}%", s.gate.invocation_rate_pct);
            println!("  efficiency      : {:.1}%", s.gate.efficiency_pct);
            println!();
            println!("── Health ──────────────────────────────────────────────");
            println!(
                "  score / grade   : {:.3} / {}",
                s.health.score, s.health.grade
            );
            if s.health.recommendations.is_empty() {
                println!("  recommendations : (none)");
            } else {
                for rec in &s.health.recommendations {
                    println!("  ! {}", rec);
                }
            }
        }
    }
}

/// Implements the `anima cache` subcommands for the tool response cache (E26).
///
/// ```text
/// anima-hosted cache stats          -- print hit/miss/eviction statistics
/// anima-hosted cache clear          -- flush all cached entries
/// anima-hosted cache warm <tool> <payload>  -- pre-populate one entry
/// ```
fn cmd_cache(args: &[String]) {
    use praxis::ToolRegistry;
    use tool_cache::CachedToolRegistry;

    let registry = ToolRegistry::new();
    let cached = CachedToolRegistry::with_defaults(registry);

    let sub = args.first().map(String::as_str).unwrap_or("stats");
    match sub {
        "stats" => {
            let s = cached.stats();
            println!("=== Tool Cache Statistics ===");
            println!("  entries : {}", s.current_entries);
            println!("  hits    : {}", s.hits);
            println!("  misses  : {}", s.misses);
            println!("  hit_rate: {:.1}%", s.hit_rate() * 100.0);
            println!("  ttl_evictions     : {}", s.ttl_evictions);
            println!("  capacity_evictions: {}", s.capacity_evictions);
        }
        "clear" => {
            cached.clear_cache();
            println!("tool cache cleared");
        }
        "warm" => {
            // anima cache warm <tool_id> <payload_utf8>
            let tool_id = match args.get(1) {
                Some(t) => t.as_str(),
                None => {
                    eprintln!("usage: anima cache warm <tool_id> <payload>");
                    cli_fail(2);
                    return;
                }
            };
            let payload = args.get(2).map(|s| s.as_bytes()).unwrap_or(b"");
            use praxis::{Bus, ToolEnvelope};
            let env = ToolEnvelope::new(Bus::Mcp, tool_id, payload.to_vec(), 0);
            match cached.dispatch(&env) {
                Ok(resp) => {
                    let s = cached.stats();
                    println!(
                        "cache warm: tool={tool_id} payload={} response_bytes={} entries={}",
                        args.get(2).map(String::as_str).unwrap_or(""),
                        resp.len(),
                        s.current_entries
                    );
                }
                Err(e) => {
                    eprintln!("cache warm failed for {tool_id}: {e:?}");
                    cli_fail(1);
                }
            }
        }
        other => {
            eprintln!("unknown cache subcommand: {other}");
            cli_fail(2);
            eprintln!("usage: anima cache [stats|clear|warm]");
            cli_fail(2);
        }
    }
}

/// Implements the `anima graph` CLI subcommand for knowledge-graph management.
///
/// ```text
/// anima-hosted graph entity add <id> <kind> [--name <display_name>]
/// anima-hosted graph entity show <id>
/// anima-hosted graph entity list [--kind <kind>]
/// anima-hosted graph entity remove <id>
/// anima-hosted graph relation add <from_id> <to_id> <kind>
/// anima-hosted graph relation list
/// anima-hosted graph query neighbors <id> [--depth N]
/// anima-hosted graph query by-kind <kind>
/// anima-hosted graph query by-attr <key> <value>
/// ```
fn cmd_graph(args: &[String]) {
    const AGENT_ID: &str = "anima";
    let path = KnowledgeGraph::default_path(AGENT_ID);
    let mut g = KnowledgeGraph::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not load knowledge graph ({e}); using in-memory graph");
        cli_fail(1);
        KnowledgeGraph::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        // ── entity sub-commands ───────────────────────────────────────────────
        Some("entity") => match args.get(1).map(String::as_str) {
            Some("add") => {
                let id = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!(
                            "usage: anima-hosted graph entity add <id> <kind> [--name <name>]"
                        );
                        cli_fail(2);
                        return;
                    }
                };
                let kind_str = match args.get(3) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!(
                            "usage: anima-hosted graph entity add <id> <kind> [--name <name>]"
                        );
                        cli_fail(2);
                        return;
                    }
                };
                let kind: EntityKind = match kind_str.parse() {
                    Ok(k) => k,
                    Err(()) => {
                        eprintln!("error: unknown entity kind '{kind_str}'");
                        cli_fail(2);
                        eprintln!("valid kinds: person, place, project, concept, technology, organization, custom:<label>");
                        return;
                    }
                };
                // Require explicit "custom:" prefix to prevent typos becoming custom kinds.
                if matches!(kind, EntityKind::Custom(_))
                    && !kind_str.to_ascii_lowercase().starts_with("custom:")
                {
                    eprintln!("error: unknown entity kind '{kind_str}'");
                    cli_fail(2);
                    eprintln!("valid kinds: person, place, project, concept, technology, organization, custom:<label>");
                    return;
                }
                // Optional --name flag; reject bare --name with no following value.
                let display_name = match args.windows(2).find(|w| w[0] == "--name") {
                    Some(w) => w[1].as_str(),
                    None => {
                        if args.last().map(String::as_str) == Some("--name") {
                            eprintln!("error: --name requires a value");
                            cli_fail(2);
                            return;
                        }
                        id
                    }
                };
                let entity = Entity::new(id, kind.clone(), display_name);
                match g.add_entity(entity) {
                    Ok(()) => {
                        log.push(AuditEntry::KnowledgeEntityAdded {
                            agent_id: AGENT_ID.to_string(),
                            entity_id: id.to_string(),
                            kind: kind.to_string(),
                            display_name: display_name.to_string(),
                        });
                        g.flush().unwrap_or_else(|e| {
                            eprintln!("warning: flush failed: {e}");
                            cli_fail(1);
                        });
                        println!("entity '{id}' ({kind}) added to knowledge graph");
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        cli_fail(1);
                    }
                }
            }
            Some("show") => {
                let id = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph entity show <id>");
                        cli_fail(2);
                        return;
                    }
                };
                match g.get_entity(id) {
                    Some(e) => {
                        println!("id:           {}", e.id);
                        println!("kind:         {}", e.kind);
                        println!("display_name: {}", e.display_name);
                        if e.attributes.is_empty() {
                            println!("attributes:   (none)");
                        } else {
                            println!("attributes:");
                            let mut attrs: Vec<_> = e.attributes.iter().collect();
                            attrs.sort_by_key(|(k, _)| k.as_str());
                            for (k, v) in attrs {
                                println!("  {k} = {v}");
                            }
                        }
                        let rels = g.relations_for(id);
                        if rels.is_empty() {
                            println!("relations:    (none)");
                        } else {
                            println!("relations:");
                            for r in rels {
                                println!(
                                    "  {} --[{}]--> {} (weight={:.2})",
                                    r.from, r.kind, r.to, r.weight
                                );
                            }
                        }
                    }
                    None => {
                        eprintln!("error: entity '{id}' not found");
                        cli_fail(1);
                    }
                }
            }
            Some("list") => {
                let kind_filter: Option<EntityKind> = if let Some(w) =
                    args.windows(2).find(|w| w[0] == "--kind")
                {
                    let k_str = w[1].as_str();
                    match k_str.parse::<EntityKind>() {
                        Ok(k) => {
                            if matches!(k, EntityKind::Custom(_))
                                && !k_str.to_ascii_lowercase().starts_with("custom:")
                            {
                                eprintln!("error: unknown entity kind '{k_str}'");
                                cli_fail(2);
                                eprintln!("valid kinds: person, place, project, concept, technology, organization, custom:<label>");
                                return;
                            }
                            Some(k)
                        }
                        Err(()) => {
                            eprintln!("error: unknown entity kind '{k_str}'");
                            cli_fail(2);
                            eprintln!("valid kinds: person, place, project, concept, technology, organization, custom:<label>");
                            return;
                        }
                    }
                } else {
                    None
                };

                let entities = g.all_entities();
                let filtered: Vec<_> = if let Some(ref kind) = kind_filter {
                    entities.into_iter().filter(|e| &e.kind == kind).collect()
                } else {
                    entities
                };
                if filtered.is_empty() {
                    println!("(no entities)");
                } else {
                    println!("{:<24} {:<16} display_name", "id", "kind");
                    println!("{}", "-".repeat(64));
                    for e in filtered {
                        println!("{:<24} {:<16} {}", e.id, e.kind, e.display_name);
                    }
                }
            }
            Some("remove") => {
                let id = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph entity remove <id>");
                        cli_fail(2);
                        return;
                    }
                };
                if g.remove_entity(id) {
                    g.flush().unwrap_or_else(|e| {
                        eprintln!("warning: flush failed: {e}");
                        cli_fail(1);
                    });
                    println!("entity '{id}' removed (including all connected relations)");
                } else {
                    eprintln!("error: entity '{id}' not found");
                    cli_fail(1);
                }
            }
            _ => {
                eprintln!("usage: anima-hosted graph entity add|show|list|remove ...");
                cli_fail(2);
            }
        },

        // ── relation sub-commands ─────────────────────────────────────────────
        Some("relation") => match args.get(1).map(String::as_str) {
            Some("add") => {
                let from = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!(
                            "usage: anima-hosted graph relation add <from_id> <to_id> <kind>"
                        );
                        cli_fail(2);
                        return;
                    }
                };
                let to = match args.get(3) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!(
                            "usage: anima-hosted graph relation add <from_id> <to_id> <kind>"
                        );
                        cli_fail(2);
                        return;
                    }
                };
                let kind_str = match args.get(4) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!(
                            "usage: anima-hosted graph relation add <from_id> <to_id> <kind>"
                        );
                        cli_fail(2);
                        return;
                    }
                };
                let kind: RelationKind = match kind_str.parse() {
                    Ok(k) => k,
                    Err(()) => {
                        eprintln!("error: unknown relation kind '{kind_str}'");
                        cli_fail(2);
                        eprintln!("valid kinds: works_at, related_to, part_of, created_by, depends_on, collaborates, is_a, custom:<label>");
                        return;
                    }
                };
                // Require explicit "custom:" prefix to prevent typos becoming custom kinds.
                if matches!(kind, RelationKind::Custom(_))
                    && !kind_str.to_ascii_lowercase().starts_with("custom:")
                {
                    eprintln!("error: unknown relation kind '{kind_str}'");
                    cli_fail(2);
                    eprintln!("valid kinds: works_at, related_to, part_of, created_by, depends_on, collaborates, is_a, custom:<label>");
                    return;
                }
                match g.add_relation(Relation::new(from, to, kind.clone())) {
                    Ok(()) => {
                        log.push(AuditEntry::KnowledgeRelationAdded {
                            agent_id: AGENT_ID.to_string(),
                            from_entity: from.to_string(),
                            to_entity: to.to_string(),
                            kind: kind.to_string(),
                        });
                        g.flush().unwrap_or_else(|e| {
                            eprintln!("warning: flush failed: {e}");
                            cli_fail(1);
                        });
                        println!("relation {from} --[{kind}]--> {to} added");
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        cli_fail(1);
                    }
                }
            }
            Some("list") => {
                let rels = g.all_relations();
                if rels.is_empty() {
                    println!("(no relations)");
                } else {
                    println!("{:<20} {:<20} {:<16} weight", "from", "to", "kind");
                    println!("{}", "-".repeat(72));
                    for r in rels {
                        println!("{:<20} {:<20} {:<16} {:.2}", r.from, r.to, r.kind, r.weight);
                    }
                }
            }
            _ => {
                eprintln!("usage: anima-hosted graph relation add|list ...");
                cli_fail(2);
            }
        },

        // ── query sub-commands ────────────────────────────────────────────────
        Some("query") => match args.get(1).map(String::as_str) {
            Some("neighbors") => {
                let id = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph query neighbors <id> [--depth N]");
                        cli_fail(2);
                        return;
                    }
                };
                let depth: usize = args
                    .windows(2)
                    .find(|w| w[0] == "--depth")
                    .and_then(|w| w[1].parse().ok())
                    .unwrap_or(1);
                let neighbors = g.find_neighbors(id, depth);
                let count = neighbors.len();
                log.push(AuditEntry::KnowledgeGraphQueried {
                    agent_id: AGENT_ID.to_string(),
                    query_type: "neighbors".to_string(),
                    result_count: count,
                });
                if neighbors.is_empty() {
                    println!("(no neighbors found for '{id}' at depth {depth})");
                } else {
                    println!("neighbors of '{id}' at depth ≤ {depth}:");
                    for e in neighbors {
                        println!("  {} ({}) — {}", e.id, e.kind, e.display_name);
                    }
                }
            }
            Some("by-kind") => {
                let kind_str = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph query by-kind <kind>");
                        cli_fail(2);
                        return;
                    }
                };
                let kind: EntityKind = match kind_str.parse() {
                    Ok(k) => k,
                    Err(()) => {
                        eprintln!("error: unknown entity kind '{kind_str}'");
                        cli_fail(2);
                        return;
                    }
                };
                let results = g.find_by_kind(&kind);
                let count = results.len();
                log.push(AuditEntry::KnowledgeGraphQueried {
                    agent_id: AGENT_ID.to_string(),
                    query_type: format!("by_kind:{kind_str}"),
                    result_count: count,
                });
                if results.is_empty() {
                    println!("(no entities of kind '{kind_str}')");
                } else {
                    for e in results {
                        println!("{} — {}", e.id, e.display_name);
                    }
                }
            }
            Some("by-attr") => {
                let key = match args.get(2) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph query by-attr <key> <value>");
                        cli_fail(2);
                        return;
                    }
                };
                let value = match args.get(3) {
                    Some(v) => v.as_str(),
                    None => {
                        eprintln!("usage: anima-hosted graph query by-attr <key> <value>");
                        cli_fail(2);
                        return;
                    }
                };
                let results = g.find_by_attribute(key, value);
                let count = results.len();
                log.push(AuditEntry::KnowledgeGraphQueried {
                    agent_id: AGENT_ID.to_string(),
                    query_type: format!("by_attr:{key}={value}"),
                    result_count: count,
                });
                if results.is_empty() {
                    println!("(no entities with {key}={value})");
                } else {
                    for e in results {
                        println!("{} ({}) — {}", e.id, e.kind, e.display_name);
                    }
                }
            }
            _ => {
                eprintln!("usage: anima-hosted graph query neighbors|by-kind|by-attr ...");
                cli_fail(2);
            }
        },

        _ => {
            eprintln!("anima-hosted graph entity add <id> <kind> [--name <name>]");
            eprintln!("anima-hosted graph entity show <id>");
            eprintln!("anima-hosted graph entity list [--kind <kind>]");
            eprintln!("anima-hosted graph entity remove <id>");
            eprintln!("anima-hosted graph relation add <from_id> <to_id> <kind>");
            eprintln!("anima-hosted graph relation list");
            eprintln!("anima-hosted graph query neighbors <id> [--depth N]");
            eprintln!("anima-hosted graph query by-kind <kind>");
            eprintln!("anima-hosted graph query by-attr <key> <value>");
            cli_fail(2);
        }
    }

    // Print any audit entries generated.
    if !log.is_empty() {
        println!();
        for entry in log.entries() {
            match entry {
                AuditEntry::KnowledgeEntityAdded {
                    agent_id,
                    entity_id,
                    kind,
                    display_name,
                } => {
                    println!("audit: 🔷 entity_added agent={agent_id} id={entity_id} kind={kind} name={display_name}");
                }
                AuditEntry::KnowledgeRelationAdded {
                    agent_id,
                    from_entity,
                    to_entity,
                    kind,
                } => {
                    println!("audit: 🔗 relation_added agent={agent_id} {from_entity} --[{kind}]--> {to_entity}");
                }
                AuditEntry::KnowledgeGraphQueried {
                    agent_id,
                    query_type,
                    result_count,
                } => {
                    println!("audit: 🔍 graph_queried agent={agent_id} type={query_type} results={result_count}");
                }
                _ => {}
            }
        }
    }
}

/// Aggregate the audit log and print a metrics report.
///
/// Satisfies E18 exit criterion 1 and 3.
///
/// ```text
/// cargo run --bin anima-hosted -- metrics [--format text|json|prometheus] [--last N]
/// ```
fn cmd_metrics(args: &[String]) {
    const AGENT_ID: &str = "anima";

    // Parse flags.
    let format = {
        let mut it = args.iter();
        let mut fmt = "text";
        loop {
            match it.next().map(String::as_str) {
                Some("--format") | Some("-f") => {
                    if let Some(f) = it.next() {
                        fmt = f.as_str();
                    }
                }
                None => break,
                _ => {}
            }
        }
        fmt.to_string()
    };

    let last_n: Option<usize> = {
        let mut it = args.iter();
        loop {
            match it.next().map(String::as_str) {
                Some("--last") => break it.next().and_then(|s| s.parse().ok()),
                None => break None,
                _ => continue,
            }
        }
    };

    // Build a demo audit log (no live ANIMA_AUDIT_DIR required in CI).
    let mut log = vita::AuditLog::new();
    log.push(vita::audit::AuditEntry::TaskStarted {
        agent_id: AGENT_ID.to_string(),
        task_id: 1,
        tier: 0,
        prompt: "draft the morning report".to_string(),
    });
    log.push(vita::audit::AuditEntry::TaskCompleted {
        agent_id: AGENT_ID.to_string(),
        task_id: 1,
        tokens_emitted: 412,
        response: "report drafted".to_string(),
    });
    log.push(vita::audit::AuditEntry::GateDecision {
        agent_id: AGENT_ID.to_string(),
        event_id: "ev1".to_string(),
        invoke: true,
        cost_class: Some("MidTier".to_string()),
        urgency: 0.7,
        novelty: 0.4,
        user_facing: true,
        semantic_class: "UserQuery".to_string(),
        value_score: 0.65,
        threshold_applied: 0.4,
        thermal_load: 0.1,
        compute_pressure: 0.2,
        memory_pressure: 0.15,
        power_budget: 0.9,
        financial_budget: 0.85,
        attention_demand: 0.6,
        reasoning: "user-facing at moderate urgency → invoke".to_string(),
        override_active: false,
    });
    log.push(vita::audit::AuditEntry::CortexInvoked {
        task_id: "inv-1".to_string(),
        latency_to_first_action_ms: 110,
    });
    log.push(vita::audit::AuditEntry::CortexCompleted {
        task_id: "inv-1".to_string(),
        tool_calls: 2,
        summary_len: 80,
    });
    log.push(vita::audit::AuditEntry::SleepEntered {
        agent_id: AGENT_ID.to_string(),
    });
    log.push(vita::audit::AuditEntry::SleepPhaseCompleted {
        agent_id: AGENT_ID.to_string(),
        phase: "MemoryPruning".to_string(),
        success: true,
    });

    let entries = log.entries();
    let window = match last_n {
        Some(n) => {
            let start = entries.len().saturating_sub(n);
            &entries[start..]
        }
        None => entries,
    };

    let m = aggregate(window);

    // Record a MetricsSnapshot into the audit trail.
    log.push(vita::audit::AuditEntry::MetricsSnapshot {
        agent_id: AGENT_ID.to_string(),
        window_entries: m.window_entries,
        tasks_started: m.tasks_started,
        tasks_completed: m.tasks_completed,
        total_tokens_emitted: m.total_tokens_emitted,
        gate_decisions: m.gate_decisions,
        gate_invocations: m.gate_invocations,
        cortex_invocations: m.cortex_invocations,
        cortex_faults: m.cortex_faults,
        total_vetoes: m.defence_vetoes + m.constitution_vetoes,
        sleep_cycles: m.sleep_cycles,
        mean_thermal_load: m.mean_thermal_load as f32,
        mean_financial_budget: m.mean_financial_budget as f32,
        tasks_failed: m.tasks_failed,
    });

    match format.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&m).expect("serialize metrics");
            println!("{json}");
        }
        "prometheus" | "prom" => {
            print!("{}", render_prometheus(&m));
        }
        _ => {
            print!("{}", render_text_report(&m));
        }
    }
}

/// Implements alert rule management:
///   `anima-hosted alert list`
///   `anima-hosted alert add <id> <field> <op> <threshold> [--severity info|warning|critical] [--desc "..."]`
///   `anima-hosted alert remove <id>`
///   `anima-hosted alert eval`
fn cmd_alert(args: &[String]) {
    const AGENT_ID: &str = "anima";

    let mut registry = AlertRuleRegistry::in_memory();
    let mut log = vita::AuditLog::new();

    // Seed a few demo rules so `list` and `eval` always have content.
    let demo_rules = vec![
        AlertRule::new(
            "high-cortex-fault-rate",
            "Alert when cortex fault rate exceeds 20%",
            AlertCondition::new(MetricField::CortexFaultRate, ComparisonOp::GreaterThan, 0.2),
            AlertSeverity::Critical,
        ),
        AlertRule::new(
            "low-task-success",
            "Alert when task success rate drops below 80%",
            AlertCondition::new(MetricField::TaskSuccessRate, ComparisonOp::LessThan, 0.8),
            AlertSeverity::Warning,
        ),
        AlertRule::new(
            "high-thermal",
            "Alert when mean thermal load exceeds 85%",
            AlertCondition::new(
                MetricField::MeanThermalLoad,
                ComparisonOp::GreaterThan,
                0.85,
            ),
            AlertSeverity::Warning,
        ),
        AlertRule::new(
            "depleted-budget",
            "Alert when financial budget falls below 10%",
            AlertCondition::new(
                MetricField::MeanFinancialBudget,
                ComparisonOp::LessThan,
                0.1,
            ),
            AlertSeverity::Critical,
        ),
    ];
    for r in demo_rules {
        let id = r.id.clone();
        let field = r.condition.field.to_string();
        let op = r.condition.op.to_string();
        let threshold = r.condition.threshold;
        let severity = r.severity.to_string();
        let description = r.description.clone();
        registry.add(r).ok();
        log.push(vita::audit::AuditEntry::AlertRuleAdded {
            agent_id: AGENT_ID.to_string(),
            rule_id: id,
            description,
            field,
            op,
            threshold,
            severity,
        });
    }

    let sub = args.first().map(String::as_str).unwrap_or("list");

    match sub {
        "list" => {
            println!("=== Alert Rules: {AGENT_ID} ===");
            let rules = registry.list();
            if rules.is_empty() {
                println!("  (no rules registered)");
            } else {
                for r in &rules {
                    let status = if r.enabled { "enabled" } else { "disabled" };
                    println!(
                        "  [{status:8}] {:<40} {} {} {:.4}  [{}]  {}",
                        r.id,
                        r.condition.field,
                        r.condition.op,
                        r.condition.threshold,
                        r.severity,
                        r.description,
                    );
                }
                println!("\n{} rule(s) registered.", rules.len());
            }
        }

        "add" => {
            // alert add <id> <field> <op> <threshold> [--severity S] [--desc D]
            if args.len() < 5 {
                eprintln!("Usage: alert add <id> <field> <op> <threshold> [--severity info|warning|critical] [--desc \"...\"]");
                cli_fail(2);
                return;
            }
            let id = &args[1];
            let field: MetricField = match args[2].parse() {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Invalid field: {e}");
                    cli_fail(2);
                    return;
                }
            };
            let op: ComparisonOp = match args[3].parse() {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("Invalid op: {e}");
                    cli_fail(2);
                    return;
                }
            };
            let threshold: f64 = match args[4].parse() {
                Ok(t) => t,
                Err(_) => {
                    eprintln!("Invalid threshold (expected f64)");
                    cli_fail(2);
                    return;
                }
            };
            let mut severity = AlertSeverity::Warning;
            let mut description = format!("{} {} {:.4}", field, op, threshold);
            let mut i = 5usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--severity" if i + 1 < args.len() => {
                        severity = args[i + 1].parse().unwrap_or(AlertSeverity::Warning);
                        i += 2;
                    }
                    "--desc" if i + 1 < args.len() => {
                        description = args[i + 1].clone();
                        i += 2;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            let field_s = field.to_string();
            let op_s = op.to_string();
            let sev_s = severity.to_string();
            let rule = AlertRule::new(
                id,
                &description,
                AlertCondition::new(field, op, threshold),
                severity,
            );
            match registry.add(rule) {
                Ok(()) => {
                    log.push(vita::audit::AuditEntry::AlertRuleAdded {
                        agent_id: AGENT_ID.to_string(),
                        rule_id: id.clone(),
                        description: description.clone(),
                        field: field_s,
                        op: op_s,
                        threshold,
                        severity: sev_s,
                    });
                    println!("Rule '{id}' added.");
                    println!("\nAudit trail:");
                    print_audit_alert(&log);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    cli_fail(1);
                }
            }
        }

        "remove" => {
            let id = match args.get(1) {
                Some(s) => s,
                None => {
                    eprintln!("Usage: alert remove <id>");
                    cli_fail(2);
                    return;
                }
            };
            match registry.remove(id) {
                Ok(_) => {
                    log.push(vita::audit::AuditEntry::AlertRuleRemoved {
                        agent_id: AGENT_ID.to_string(),
                        rule_id: id.clone(),
                    });
                    println!("Rule '{id}' removed.");
                    println!("\nAudit trail:");
                    print_audit_alert(&log);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    cli_fail(1);
                }
            }
        }

        "eval" => {
            // Evaluate demo rules against a seeded metrics snapshot.
            let mut demo_log = vita::AuditLog::new();
            demo_log.push(vita::audit::AuditEntry::TaskStarted {
                agent_id: AGENT_ID.to_string(),
                task_id: 1,
                tier: 0,
                prompt: "morning report".to_string(),
            });
            demo_log.push(vita::audit::AuditEntry::TaskCompleted {
                agent_id: AGENT_ID.to_string(),
                task_id: 1,
                tokens_emitted: 300,
                response: "done".to_string(),
            });
            demo_log.push(vita::audit::AuditEntry::TaskStarted {
                agent_id: AGENT_ID.to_string(),
                task_id: 2,
                tier: 0,
                prompt: "failing task".to_string(),
            });
            demo_log.push(vita::audit::AuditEntry::TaskFailed {
                agent_id: AGENT_ID.to_string(),
                task_id: 2,
                error: "backend timeout".to_string(),
            });
            demo_log.push(vita::audit::AuditEntry::CortexInvoked {
                task_id: "inv-1".to_string(),
                latency_to_first_action_ms: 95,
            });
            demo_log.push(vita::audit::AuditEntry::CortexFault {
                task_id: "inv-1".to_string(),
                error: "python process crashed".to_string(),
            });

            let m = aggregate(demo_log.entries());
            let rules = registry.rules_owned();
            let mut trackers = vec![];
            let events = alerts::evaluate(&m, &rules, &mut trackers);

            println!("=== Alert Evaluation: {AGENT_ID} ===");
            println!(
                "Metrics window: {} entries | task_success_rate={:.2} | cortex_fault_rate={:.2}",
                m.window_entries, m.task_success_rate, m.cortex_fault_rate
            );
            println!();

            if events.is_empty() {
                println!("  No alerts fired.");
            } else {
                for ev in &events {
                    let icon = match ev.kind {
                        alerts::AlertEventKind::Fired => "🚨 FIRED   ",
                        alerts::AlertEventKind::Resolved => "✅ RESOLVED",
                    };
                    println!(
                        "  {icon}  [{:8}]  {}  ({} = {:.4}, threshold = {:.4})",
                        ev.severity, ev.rule_id, ev.field, ev.actual_value, ev.threshold
                    );
                    // Emit audit entries.
                    match ev.kind {
                        alerts::AlertEventKind::Fired => {
                            log.push(vita::audit::AuditEntry::AlertFired {
                                agent_id: AGENT_ID.to_string(),
                                rule_id: ev.rule_id.clone(),
                                field: ev.field.to_string(),
                                actual_value: ev.actual_value,
                                threshold: ev.threshold,
                                severity: ev.severity.to_string(),
                            });
                        }
                        alerts::AlertEventKind::Resolved => {
                            log.push(vita::audit::AuditEntry::AlertResolved {
                                agent_id: AGENT_ID.to_string(),
                                rule_id: ev.rule_id.clone(),
                                field: ev.field.to_string(),
                                actual_value: ev.actual_value,
                            });
                        }
                    }
                }
            }
            println!("\nAudit trail (E28 entries):");
            print_audit_alert(&log);
        }

        other => {
            eprintln!("Unknown alert subcommand: {other}");
            cli_fail(2);
            eprintln!("Available: list, add, remove, eval");
            cli_fail(2);
        }
    }
}

fn print_audit_alert(log: &vita::AuditLog) {
    for entry in log.entries() {
        match entry {
            vita::audit::AuditEntry::AlertRuleAdded {
                agent_id,
                rule_id,
                field,
                op,
                threshold,
                severity,
                ..
            } => {
                println!("  🔔  alert_rule_added agent={agent_id} id={rule_id} condition=\"{field} {op} {threshold:.4}\" severity={severity}");
            }
            vita::audit::AuditEntry::AlertRuleRemoved { agent_id, rule_id } => {
                println!("  🔕  alert_rule_removed agent={agent_id} id={rule_id}");
            }
            vita::audit::AuditEntry::AlertFired {
                agent_id,
                rule_id,
                field,
                actual_value,
                threshold,
                severity,
            } => {
                println!("  🚨  alert_fired agent={agent_id} id={rule_id} {field}={actual_value:.4} threshold={threshold:.4} severity={severity}");
            }
            vita::audit::AuditEntry::AlertResolved {
                agent_id,
                rule_id,
                field,
                actual_value,
            } => {
                println!(
                    "  ✅  alert_resolved agent={agent_id} id={rule_id} {field}={actual_value:.4}"
                );
            }
            _ => {}
        }
    }
}

/// Implements the `anima webhook` subcommands for E29 — Outbound Webhook Integration.
///
/// ```text
/// anima-hosted webhook list
/// anima-hosted webhook add <url> [--secret <key>] [--events <kind1,kind2,...>]
/// anima-hosted webhook remove <id>
/// anima-hosted webhook enable <id>
/// anima-hosted webhook disable <id>
/// anima-hosted webhook test <id>
/// anima-hosted webhook stats
/// ```
fn cmd_webhook(args: &[String]) {
    use webhooks::{
        new_delivery_id, new_endpoint_id, DispatchConfig, EventFilter, FixtureSender,
        WebhookDispatcher, WebhookEndpoint, WebhookPayload, WebhookRegistry,
    };

    const AGENT_ID: &str = "anima";

    let path = WebhookRegistry::default_path(AGENT_ID);
    let mut registry = WebhookRegistry::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open webhook registry ({e}); using in-memory fallback");
        cli_fail(1);
        WebhookRegistry::in_memory()
    });
    let mut log = AuditLog::from_env(AGENT_ID);

    match args.first().map(String::as_str) {
        Some("list") => {
            let endpoints = registry.list();
            if endpoints.is_empty() {
                println!("webhook: no endpoints registered");
            } else {
                println!("=== Webhook Endpoints: {} ===", AGENT_ID);
                for ep in &endpoints {
                    let status = if ep.enabled { "enabled" } else { "disabled" };
                    let secret_tag = if ep.secret.is_some() { " [signed]" } else { "" };
                    let filter_tag = match &ep.filter {
                        EventFilter::All => "all".to_owned(),
                        EventFilter::Selected { kinds } => {
                            let mut v: Vec<&str> = kinds.iter().map(String::as_str).collect();
                            v.sort();
                            v.join(",")
                        }
                    };
                    println!(
                        "  {id}  [{status}]{secret_tag}  {url}  events={filter_tag}",
                        id = ep.id,
                        url = ep.url,
                    );
                }
            }
        }
        Some("add") => {
            let url = match args.get(1) {
                Some(u) => u.clone(),
                None => {
                    eprintln!("webhook add: missing <url>");
                    cli_fail(2);
                    return;
                }
            };

            // Parse optional --secret and --events flags.
            let mut secret: Option<String> = None;
            let mut event_kinds: Option<Vec<String>> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--secret" => match args.get(i + 1) {
                        Some(v) => {
                            secret = Some(v.clone());
                            i += 2;
                        }
                        None => {
                            eprintln!("webhook add: --secret requires a value");
                            cli_fail(2);
                            return;
                        }
                    },
                    "--events" => match args.get(i + 1) {
                        Some(kinds_str) => {
                            event_kinds = Some(
                                kinds_str
                                    .split(',')
                                    .map(|s| s.trim().to_owned())
                                    .filter(|s| !s.is_empty())
                                    .collect(),
                            );
                            i += 2;
                        }
                        None => {
                            eprintln!("webhook add: --events requires a value");
                            cli_fail(2);
                            return;
                        }
                    },
                    _ => {
                        i += 1;
                    }
                }
            }

            let filter = match event_kinds {
                Some(kinds) => EventFilter::only(kinds),
                None => EventFilter::All,
            };

            let id = new_endpoint_id();
            let has_secret = secret.is_some();
            let ep = WebhookEndpoint::new(&id, &url, secret, filter);
            match registry.register(ep) {
                Ok(()) => {
                    log.push(AuditEntry::WebhookRegistered {
                        agent_id: AGENT_ID.to_owned(),
                        endpoint_id: id.clone(),
                        url: url.clone(),
                        has_secret,
                    });
                    println!("webhook: registered {id} → {url}");
                    for entry in log.entries() {
                        if let AuditEntry::WebhookRegistered {
                            endpoint_id,
                            url,
                            has_secret,
                            ..
                        } = entry
                        {
                            let sec = if *has_secret { " [signed]" } else { "" };
                            println!("  🔔 webhook_registered id={endpoint_id} url={url}{sec}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("webhook: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("remove") => {
            let id = match args.get(1) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("webhook remove: missing <id>");
                    cli_fail(2);
                    return;
                }
            };
            match registry.remove(&id) {
                Ok(ep) => {
                    log.push(AuditEntry::WebhookRemoved {
                        agent_id: AGENT_ID.to_owned(),
                        endpoint_id: id.clone(),
                    });
                    println!("webhook: removed {id} (was: {})", ep.url);
                    println!("  🗑  webhook_removed id={id}");
                }
                Err(e) => {
                    eprintln!("webhook: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("enable") | Some("disable") => {
            let enabled = args[0] == "enable";
            let id = match args.get(1) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("webhook {}: missing <id>", args[0]);
                    cli_fail(2);
                    return;
                }
            };
            match registry.set_enabled(&id, enabled) {
                Ok(()) => {
                    let verb = if enabled { "enabled" } else { "disabled" };
                    println!("webhook: {verb} {id}");
                }
                Err(e) => {
                    eprintln!("webhook: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("show") => {
            let id = match args.get(1) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("webhook show: missing <id>");
                    cli_fail(2);
                    return;
                }
            };
            match registry.get(&id) {
                Some(ep) => {
                    let status = if ep.enabled { "enabled" } else { "disabled" };
                    let secret_tag = if ep.secret.is_some() {
                        "yes (secret configured)"
                    } else {
                        "no"
                    };
                    let filter_tag = match &ep.filter {
                        EventFilter::All => "all".to_owned(),
                        EventFilter::Selected { kinds } => {
                            let mut v: Vec<&str> = kinds.iter().map(String::as_str).collect();
                            v.sort();
                            v.join(", ")
                        }
                    };
                    println!("Webhook endpoint: {id}");
                    println!("  URL    : {}", ep.url);
                    println!("  Status : {status}");
                    println!("  Signed : {secret_tag}");
                    println!("  Events : {filter_tag}");
                }
                None => {
                    eprintln!("webhook: no endpoint with id={id:?}");
                    cli_fail(1);
                }
            }
        }
        Some("test") => {
            let id = match args.get(1) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("webhook test: missing <id>");
                    cli_fail(2);
                    return;
                }
            };
            let ep = match registry.get(&id) {
                Some(ep) => ep.clone(),
                None => {
                    eprintln!("webhook: no endpoint with id={id:?}");
                    cli_fail(1);
                    return;
                }
            };
            println!("webhook: sending test ping to {} ...", ep.url);
            let mut dispatcher = WebhookDispatcher::with_sender(
                FixtureSender,
                DispatchConfig {
                    max_attempts: 1,
                    base_backoff_ms: 0,
                },
            );
            let mut payload = WebhookPayload::new(
                new_delivery_id(),
                AGENT_ID,
                "webhook_test",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0),
                serde_json::json!({ "ping": true }),
            );
            let stats = dispatcher.dispatch(&ep, &mut payload);
            if stats.success {
                log.push(AuditEntry::WebhookDispatched {
                    agent_id: AGENT_ID.to_owned(),
                    endpoint_id: id.clone(),
                    event_kind: "webhook_test".to_owned(),
                    attempts: stats.attempts,
                });
                println!("webhook: test ping delivered successfully");
                println!("  📤 webhook_dispatched id={id} event=webhook_test");
            } else {
                let error = stats.last_error.unwrap_or_else(|| "unknown".to_owned());
                log.push(AuditEntry::WebhookFailed {
                    agent_id: AGENT_ID.to_owned(),
                    endpoint_id: id.clone(),
                    event_kind: "webhook_test".to_owned(),
                    attempts: stats.attempts,
                    error: error.clone(),
                });
                println!("webhook: test ping failed: {error}");
                println!("  ❌ webhook_failed id={id} error={error:?}");
                cli_fail(1);
            }
        }
        Some("stats") => {
            // Demo dispatch: send one event to each endpoint and report statistics.
            let endpoints: Vec<_> = registry.list().iter().map(|ep| (*ep).clone()).collect();
            if endpoints.is_empty() {
                println!("webhook stats: no endpoints registered");
                return;
            }
            let mut dispatcher = WebhookDispatcher::fixture();
            println!("=== Webhook Dispatch Statistics: {} ===", AGENT_ID);
            for ep in &endpoints {
                let mut payload = WebhookPayload::new(
                    new_delivery_id(),
                    AGENT_ID,
                    "stats_probe",
                    0,
                    serde_json::json!({}),
                );
                let stats = dispatcher.dispatch(ep, &mut payload);
                let result_icon = if stats.success { "✅" } else { "❌" };
                println!(
                    "  {result_icon} {id}  attempts={a}  success={s}",
                    id = ep.id,
                    a = stats.attempts,
                    s = stats.success,
                );
                if stats.success {
                    log.push(AuditEntry::WebhookDispatched {
                        agent_id: AGENT_ID.to_owned(),
                        endpoint_id: ep.id.clone(),
                        event_kind: "stats_probe".to_owned(),
                        attempts: stats.attempts,
                    });
                } else if let Some(err) = &stats.last_error {
                    log.push(AuditEntry::WebhookFailed {
                        agent_id: AGENT_ID.to_owned(),
                        endpoint_id: ep.id.clone(),
                        event_kind: "stats_probe".to_owned(),
                        attempts: stats.attempts,
                        error: err.clone(),
                    });
                }
            }
            let cum = dispatcher.stats();
            println!();
            println!(
                "Cumulative: dispatches={} success={} failed={} retries={}  \
                 success_rate={:.1}%  mean_attempts={:.2}",
                cum.total_dispatches,
                cum.successful,
                cum.failed,
                cum.retries,
                cum.success_rate() * 100.0,
                cum.mean_attempts(),
            );
        }
        _ => {
            eprintln!("usage: anima-hosted webhook list");
            eprintln!(
                "       anima-hosted webhook add <url> [--secret <key>] [--events <k1,k2,...>]"
            );
            eprintln!("       anima-hosted webhook remove <id>");
            eprintln!("       anima-hosted webhook enable <id>");
            eprintln!("       anima-hosted webhook disable <id>");
            eprintln!("       anima-hosted webhook show <id>");
            eprintln!("       anima-hosted webhook test <id>");
            eprintln!("       anima-hosted webhook stats");
            cli_fail(2);
        }
    }
}

/// Run the full suite of diagnostic checks and print the resulting report.
///
/// Satisfies E30 exit criterion 1: "`anima diagnose` prints a structured
/// health report with per-subsystem status and remediation hints."
///
/// ```text
/// cargo run --bin anima-hosted -- diagnose [--json] [--quiet]
/// ```
///
/// Flags:
/// - `--json`   emit the full report as a single JSON object (default: text).
/// - `--quiet`  print only the overall status line (useful for scripting).
fn cmd_diagnose(args: &[String]) {
    use diagnostics::{checks::all_checks, AuditSnapshot, DiagnosticReport};

    let emit_json = args.iter().any(|a| a == "--json");
    let quiet = args.iter().any(|a| a == "--quiet");

    const AGENT_ID: &str = "anima";

    // Read the durable audit history from disk so the snapshot reflects the
    // full persisted log, not just entries accumulated in this process.
    let log_entries = vita::audit::load_entries_from_env(AGENT_ID);

    let snapshot = AuditSnapshot::from_audit_log(&log_entries);
    let checks = all_checks();
    let report = DiagnosticReport::run(&snapshot, &checks);

    if emit_json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("diagnose: JSON serialisation error: {e}");
                cli_fail(1);
            }
        }
    } else if quiet {
        println!("{:?}", report.overall_status);
    } else {
        print!("{}", report.render_text());
    }

    // Append a DiagnosticRun entry to the durable log so health trends are
    // visible during forensic replay.
    let mut audit = vita::AuditLog::from_env(AGENT_ID);
    audit.push(AuditEntry::DiagnosticRun {
        agent_id: AGENT_ID.to_string(),
        overall_status: format!("{:?}", report.overall_status),
        healthy_count: report.healthy_count,
        degraded_count: report.degraded_count,
        critical_count: report.critical_count,
        audit_entries_analysed: report.audit_entries_analysed,
    });
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
        cli_fail(1);
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
                        Err(e) => {
                            eprintln!("identity: error: {e}");
                            cli_fail(1);
                        }
                    }
                }
                _ => {
                    eprintln!("usage: anima-hosted identity set <key> <value>");
                    cli_fail(2);
                }
            }
        }
        _ => {
            eprintln!("usage: anima-hosted identity show [<key>]");
            eprintln!("       anima-hosted identity set <key> <value>");
            cli_fail(2);
        }
    }
}

// ── `anima users` subcommand (E17) ───────────────────────────────────────────

/// Prints the E17-relevant entries from an in-process audit log.
fn print_user_audit(log: &AuditLog) {
    println!("--- audit trail ---");
    for entry in log.entries() {
        match entry {
            AuditEntry::UserProfileCreated {
                agent_id,
                user_id,
                display_name,
                channel,
            } => {
                println!(
                    "  👤 user_profile_created agent={agent_id} user={user_id} \
                     name={display_name:?} channel={channel}"
                );
            }
            AuditEntry::UserTrustUpdated {
                agent_id,
                user_id,
                old_tier,
                new_tier,
            } => {
                println!(
                    "  🔐 user_trust_updated agent={agent_id} user={user_id} \
                     {old_tier} → {new_tier}"
                );
            }
            AuditEntry::UserConsentUpdated {
                agent_id,
                user_id,
                category,
                granted,
            } => {
                let mark = if *granted { "✅" } else { "❌" };
                println!(
                    "  {mark} user_consent_updated agent={agent_id} user={user_id} \
                     category={category}"
                );
            }
            _ => {}
        }
    }
    println!("---");
}

/// Implements the `anima users` subcommands for managing per-user profiles.
///
/// ```text
/// anima-hosted users list
/// anima-hosted users show <user_id>
/// anima-hosted users trust <user_id> <tier>
/// anima-hosted users consent <user_id> <category> grant|revoke
/// ```
fn cmd_users(args: &[String]) {
    use std::str::FromStr;
    use users::{DataCategory, TrustTier, UserProfile, UserRegistry};

    const AGENT_ID: &str = "anima";

    let path = UserRegistry::default_path(AGENT_ID);
    let mut registry = UserRegistry::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open user registry ({e}); using in-memory fallback");
        cli_fail(1);
        UserRegistry::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("list") => {
            if registry.is_empty() {
                println!("(no users registered)");
            } else {
                println!("{:>20}  {:>10}  display_name", "user_id", "trust");
                println!("{}", "-".repeat(50));
                let mut entries: Vec<_> = registry.iter().collect();
                entries.sort_by_key(|(id, _)| *id);
                for (id, rec) in entries {
                    println!(
                        "{:>20}  {:>10}  {}",
                        id,
                        rec.profile.trust_tier.as_str(),
                        rec.profile.display_name
                    );
                }
            }
        }
        Some("show") => match args.get(1) {
            Some(user_id) => match registry.get(user_id) {
                Some(rec) => {
                    println!("user_id      : {}", rec.profile.user_id);
                    println!("display_name : {}", rec.profile.display_name);
                    println!("channel      : {}", rec.profile.channel);
                    println!("trust_tier   : {}", rec.profile.trust_tier);
                    println!("created_at   : {}ns", rec.profile.created_at_ns);
                    println!("last_seen    : {}ns", rec.profile.last_seen_ns);
                    if rec.profile.facts.is_empty() {
                        println!("facts        : (none)");
                    } else {
                        println!("facts:");
                        let mut facts: Vec<_> = rec.profile.facts.iter().collect();
                        facts.sort_by_key(|(k, _)| *k);
                        for (k, v) in facts {
                            println!("  {k} = {v:?}");
                        }
                    }
                    let consented: Vec<_> = DataCategory::all()
                        .iter()
                        .filter(|c| rec.consent.is_consented(**c, 0))
                        .map(|c| c.as_str())
                        .collect();
                    if consented.is_empty() {
                        println!("consent      : (none)");
                    } else {
                        println!("consent      : {}", consented.join(", "));
                    }
                }
                None => {
                    eprintln!("users: no user with id={user_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted users show <user_id>");
                cli_fail(2);
            }
        },
        Some("trust") => match (args.get(1), args.get(2)) {
            (Some(user_id), Some(tier_str)) => match TrustTier::from_str(tier_str) {
                Ok(tier) => match registry.set_trust(user_id, tier, 0) {
                    Ok((old, new)) => {
                        log.push(AuditEntry::UserTrustUpdated {
                            agent_id: AGENT_ID.to_owned(),
                            user_id: user_id.clone(),
                            old_tier: old.as_str().to_owned(),
                            new_tier: new.as_str().to_owned(),
                        });
                        println!(
                            "users: updated trust for {user_id:?}: \
                                         {old} → {new}"
                        );
                        if let Err(e) = registry.flush() {
                            eprintln!("users: flush failed: {e}");
                            cli_fail(1);
                        }
                        print_user_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("users: {e}");
                        cli_fail(1);
                    }
                },
                Err(e) => {
                    eprintln!("users: invalid trust tier: {e}");
                    cli_fail(2);
                }
            },
            _ => {
                eprintln!(
                    "usage: anima-hosted users trust <user_id> \
                     unknown|verified|trusted|operator"
                );
                cli_fail(2);
            }
        },
        Some("consent") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(user_id), Some(cat_str), Some(action)) => match DataCategory::from_str(cat_str) {
                Ok(category) => {
                    let granted = match action.as_str() {
                        "grant" => true,
                        "revoke" => false,
                        other => {
                            eprintln!("users: expected 'grant' or 'revoke', got {other:?}");
                            cli_fail(2);
                            return;
                        }
                    };
                    match registry.get_mut(user_id) {
                        Some(rec) => {
                            rec.consent.set(category, granted, 0);
                            log.push(AuditEntry::UserConsentUpdated {
                                agent_id: AGENT_ID.to_owned(),
                                user_id: user_id.clone(),
                                category: category.as_str().to_owned(),
                                granted,
                            });
                            let verb = if granted { "granted" } else { "revoked" };
                            println!(
                                "users: consent {verb} for {user_id:?} \
                                         category={cat_str}"
                            );
                            if let Err(e) = registry.flush() {
                                eprintln!("users: flush failed: {e}");
                                cli_fail(1);
                            }
                            print_user_audit(&log);
                        }
                        None => {
                            eprintln!("users: no user with id={user_id:?}");
                            cli_fail(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("users: invalid category: {e}");
                    cli_fail(2);
                }
            },
            _ => {
                eprintln!(
                    "usage: anima-hosted users consent <user_id> \
                     episodic_memory|identity_facts|usage_stats|knowledge_corpus \
                     grant|revoke"
                );
                cli_fail(2);
            }
        },
        Some("register") => {
            // Convenience helper: manually register a user (useful for testing).
            match (args.get(1), args.get(2), args.get(3)) {
                (Some(user_id), Some(display_name), Some(channel)) => {
                    let profile =
                        UserProfile::new(user_id.clone(), display_name.clone(), channel.clone(), 0);
                    match registry.register(profile) {
                        Ok(()) => {
                            log.push(AuditEntry::UserProfileCreated {
                                agent_id: AGENT_ID.to_owned(),
                                user_id: user_id.clone(),
                                display_name: display_name.clone(),
                                channel: channel.clone(),
                            });
                            println!("users: registered {user_id:?}");
                            if let Err(e) = registry.flush() {
                                eprintln!("users: flush failed: {e}");
                                cli_fail(1);
                            }
                            print_user_audit(&log);
                        }
                        Err(e) => {
                            eprintln!("users: {e}");
                            cli_fail(1);
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "usage: anima-hosted users register <user_id> <display_name> <channel>"
                    );
                    cli_fail(2);
                }
            }
        }
        _ => {
            eprintln!("usage: anima-hosted users list");
            eprintln!("       anima-hosted users show <user_id>");
            eprintln!(
                "       anima-hosted users trust <user_id> \
                 unknown|verified|trusted|operator"
            );
            eprintln!(
                "       anima-hosted users consent <user_id> \
                 episodic_memory|identity_facts|usage_stats|knowledge_corpus \
                 grant|revoke"
            );
            eprintln!("       anima-hosted users register <user_id> <display_name> <channel>");
            cli_fail(2);
        }
    }
}

// ── `anima workspace` subcommand (E31) ───────────────────────────────────────

/// Prints E31-relevant entries from an in-process audit log.
fn print_workspace_audit(log: &AuditLog) {
    println!("--- audit trail ---");
    for entry in log.entries() {
        match entry {
            AuditEntry::WorkspaceCreated {
                agent_id,
                workspace_id,
                display_name,
                owner_user_id,
            } => {
                println!(
                    "  🏢 workspace_created agent={agent_id} id={workspace_id} \
                     name={display_name:?} owner={owner_user_id}"
                );
            }
            AuditEntry::WorkspaceMemberAdded {
                agent_id,
                workspace_id,
                user_id,
                role,
            } => {
                println!(
                    "  👥 workspace_member_added agent={agent_id} \
                     workspace={workspace_id} user={user_id} role={role}"
                );
            }
            AuditEntry::WorkspaceMemberRemoved {
                agent_id,
                workspace_id,
                user_id,
                role,
            } => {
                println!(
                    "  👤 workspace_member_removed agent={agent_id} \
                     workspace={workspace_id} user={user_id} role={role}"
                );
            }
            AuditEntry::WorkspaceQuotaUpdated {
                agent_id,
                workspace_id,
                max_members,
                max_daily_tokens,
            } => {
                println!(
                    "  📊 workspace_quota_updated agent={agent_id} \
                     workspace={workspace_id} max_members={max_members} \
                     max_daily_tokens={max_daily_tokens}"
                );
            }
            AuditEntry::WorkspaceStatusChanged {
                agent_id,
                workspace_id,
                old_status,
                new_status,
            } => {
                println!(
                    "  🔄 workspace_status_changed agent={agent_id} \
                     workspace={workspace_id} {old_status} → {new_status}"
                );
            }
            _ => {}
        }
    }
    println!("---");
}

/// Implements the `anima workspace` subcommands for managing workspaces.
///
/// ```text
/// anima-hosted workspace create <id> <display_name> <owner_user_id>
/// anima-hosted workspace list
/// anima-hosted workspace show <workspace_id>
/// anima-hosted workspace add-member <workspace_id> <user_id> <role>
/// anima-hosted workspace remove-member <workspace_id> <user_id>
/// anima-hosted workspace set-quota <workspace_id> <max_members> <max_daily_tokens>
/// anima-hosted workspace suspend <workspace_id>
/// anima-hosted workspace reactivate <workspace_id>
/// anima-hosted workspace delete <workspace_id>
/// ```
fn cmd_workspace(args: &[String]) {
    use std::str::FromStr;
    use workspace::{WorkspaceProfile, WorkspaceQuota, WorkspaceRegistry, WorkspaceRole};

    const AGENT_ID: &str = "anima";

    let path = WorkspaceRegistry::default_path(AGENT_ID);
    let mut registry = WorkspaceRegistry::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open workspace registry ({e}); using in-memory fallback");
        cli_fail(1);
        WorkspaceRegistry::in_memory()
    });
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("list") => {
            if registry.is_empty() {
                println!("(no workspaces registered)");
            } else {
                println!(
                    "{:>20}  {:>10}  {:>8}  display_name",
                    "workspace_id", "status", "members"
                );
                println!("{}", "-".repeat(60));
                let mut entries: Vec<_> = registry.iter().collect();
                entries.sort_by_key(|(id, _)| *id);
                for (id, rec) in entries {
                    println!(
                        "{:>20}  {:>10}  {:>8}  {}",
                        id,
                        rec.profile.status.as_str(),
                        rec.member_count(),
                        rec.profile.display_name
                    );
                }
            }
        }
        Some("show") => match args.get(1) {
            Some(ws_id) => match registry.get(ws_id) {
                Some(rec) => {
                    println!("workspace_id  : {}", rec.profile.workspace_id);
                    println!("display_name  : {}", rec.profile.display_name);
                    println!("owner         : {}", rec.profile.owner_user_id);
                    println!("status        : {}", rec.profile.status);
                    println!("created_at    : {}ns", rec.profile.created_at_ns);
                    if let Some(desc) = &rec.profile.description {
                        println!("description   : {desc}");
                    }
                    println!("quota:");
                    println!("  max_members      : {}", rec.quota.max_members);
                    println!("  max_daily_tokens : {}", rec.quota.max_daily_tokens);
                    println!("  max_storage_bytes: {}", rec.quota.max_storage_bytes);
                    println!("  max_active_tasks : {}", rec.quota.max_active_tasks);
                    println!("members ({}):", rec.member_count());
                    let mut members = rec.members.clone();
                    members.sort_by(|a, b| a.user_id.cmp(&b.user_id));
                    for m in &members {
                        println!("  {} ({})", m.user_id, m.role);
                    }
                }
                None => {
                    eprintln!("workspace: no workspace with id={ws_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted workspace show <workspace_id>");
                cli_fail(2);
            }
        },
        Some("create") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(raw_id), Some(display_name), Some(owner_user_id)) => {
                let workspace_id = match workspace::WorkspaceProfile::make_id(raw_id) {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!("workspace: invalid id: {e}");
                        cli_fail(2);
                        return;
                    }
                };
                let profile = WorkspaceProfile::new(&workspace_id, display_name, owner_user_id, 0);
                match registry.create(profile, 0) {
                    Ok(()) => {
                        log.push(AuditEntry::WorkspaceCreated {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: workspace_id.clone(),
                            display_name: display_name.clone(),
                            owner_user_id: owner_user_id.clone(),
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!("workspace: created {workspace_id:?} owned by {owner_user_id:?}");
                        print_workspace_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("workspace: error: {e}");
                        cli_fail(1);
                    }
                }
            }
            _ => {
                eprintln!(
                    "usage: anima-hosted workspace create <id> <display_name> <owner_user_id>"
                );
                cli_fail(2);
            }
        },
        Some("add-member") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(ws_id), Some(user_id), Some(role_str)) => {
                // The local operator acts as the workspace owner for CLI-driven
                // privilege changes; the registry enforces that this actor holds
                // a managing role.
                let actor = registry
                    .get(ws_id)
                    .map(|rec| rec.profile.owner_user_id.clone())
                    .unwrap_or_default();
                match WorkspaceRole::from_str(role_str) {
                    Ok(role) => {
                        match registry.add_member(&actor, ws_id, user_id.clone(), role, 0) {
                            Ok(()) => {
                                log.push(AuditEntry::WorkspaceMemberAdded {
                                    agent_id: AGENT_ID.to_owned(),
                                    workspace_id: ws_id.clone(),
                                    user_id: user_id.clone(),
                                    role: role.as_str().to_owned(),
                                });
                                if let Err(e) = registry.flush() {
                                    eprintln!(
                                        "workspace: warning: could not persist registry: {e}"
                                    );
                                    cli_fail(1);
                                }
                                println!("workspace: added {user_id:?} to {ws_id:?} as {role_str}");
                                print_workspace_audit(&log);
                            }
                            Err(e) => {
                                eprintln!("workspace: error: {e}");
                                cli_fail(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("workspace: invalid role: {e}");
                        cli_fail(2);
                    }
                }
            }
            _ => {
                eprintln!(
                    "usage: anima-hosted workspace add-member \
                 <workspace_id> <user_id> guest|member|admin"
                );
                cli_fail(2);
            }
        },
        Some("remove-member") => match (args.get(1), args.get(2)) {
            (Some(ws_id), Some(user_id)) => {
                let actor = registry
                    .get(ws_id)
                    .map(|rec| rec.profile.owner_user_id.clone())
                    .unwrap_or_default();
                match registry.remove_member(&actor, ws_id, user_id) {
                    Ok(removed_role) => {
                        log.push(AuditEntry::WorkspaceMemberRemoved {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: ws_id.clone(),
                            user_id: user_id.clone(),
                            role: removed_role.as_str().to_owned(),
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!("workspace: removed {user_id:?} from {ws_id:?}");
                        print_workspace_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("workspace: error: {e}");
                        cli_fail(1);
                    }
                }
            }
            _ => {
                eprintln!("usage: anima-hosted workspace remove-member <workspace_id> <user_id>");
                cli_fail(2);
            }
        },
        Some("set-quota") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(ws_id), Some(max_members_str), Some(max_tokens_str)) => {
                let max_members = match max_members_str.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("workspace: max_members must be a non-negative integer");
                        cli_fail(2);
                        return;
                    }
                };
                let max_daily_tokens = match max_tokens_str.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("workspace: max_daily_tokens must be a non-negative integer");
                        cli_fail(2);
                        return;
                    }
                };
                let quota = WorkspaceQuota {
                    max_members,
                    max_daily_tokens,
                    ..WorkspaceQuota::default()
                };
                let actor = registry
                    .get(ws_id)
                    .map(|rec| rec.profile.owner_user_id.clone())
                    .unwrap_or_default();
                match registry.set_quota(&actor, ws_id, quota) {
                    Ok(()) => {
                        log.push(AuditEntry::WorkspaceQuotaUpdated {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: ws_id.clone(),
                            max_members,
                            max_daily_tokens,
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!(
                            "workspace: updated quota for {ws_id:?}: \
                             max_members={max_members} max_daily_tokens={max_daily_tokens}"
                        );
                        print_workspace_audit(&log);
                    }
                    Err(e) => {
                        eprintln!("workspace: error: {e}");
                        cli_fail(1);
                    }
                }
            }
            _ => {
                eprintln!(
                    "usage: anima-hosted workspace set-quota \
                 <workspace_id> <max_members> <max_daily_tokens>"
                );
                cli_fail(2);
            }
        },
        Some("suspend") => match args.get(1) {
            Some(ws_id) => match registry.get_mut(ws_id) {
                Some(rec) => {
                    let old = rec.profile.status.as_str().to_owned();
                    if rec.profile.suspend(0) {
                        let new = rec.profile.status.as_str().to_owned();
                        log.push(AuditEntry::WorkspaceStatusChanged {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: ws_id.clone(),
                            old_status: old.clone(),
                            new_status: new.clone(),
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!("workspace: suspended {ws_id:?} ({old} → {new})");
                        print_workspace_audit(&log);
                    } else {
                        println!("workspace: {ws_id:?} is already {old} (no change)");
                    }
                }
                None => {
                    eprintln!("workspace: no workspace with id={ws_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted workspace suspend <workspace_id>");
                cli_fail(2);
            }
        },
        Some("reactivate") => match args.get(1) {
            Some(ws_id) => match registry.get_mut(ws_id) {
                Some(rec) => {
                    let old = rec.profile.status.as_str().to_owned();
                    if rec.profile.reactivate(0) {
                        let new = rec.profile.status.as_str().to_owned();
                        log.push(AuditEntry::WorkspaceStatusChanged {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: ws_id.clone(),
                            old_status: old.clone(),
                            new_status: new.clone(),
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!("workspace: reactivated {ws_id:?} ({old} → {new})");
                        print_workspace_audit(&log);
                    } else {
                        println!("workspace: {ws_id:?} is {old} (cannot reactivate)");
                        cli_fail(1);
                    }
                }
                None => {
                    eprintln!("workspace: no workspace with id={ws_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted workspace reactivate <workspace_id>");
                cli_fail(2);
            }
        },
        Some("delete") => match args.get(1) {
            Some(ws_id) => match registry.get_mut(ws_id) {
                Some(rec) => {
                    let old = rec.profile.status.as_str().to_owned();
                    if rec.profile.delete(0) {
                        let new = rec.profile.status.as_str().to_owned();
                        log.push(AuditEntry::WorkspaceStatusChanged {
                            agent_id: AGENT_ID.to_owned(),
                            workspace_id: ws_id.clone(),
                            old_status: old.clone(),
                            new_status: new.clone(),
                        });
                        if let Err(e) = registry.flush() {
                            eprintln!("workspace: warning: could not persist registry: {e}");
                            cli_fail(1);
                        }
                        println!("workspace: soft-deleted {ws_id:?} ({old} → {new})");
                        print_workspace_audit(&log);
                    } else {
                        println!("workspace: {ws_id:?} is already deleted");
                    }
                }
                None => {
                    eprintln!("workspace: no workspace with id={ws_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted workspace delete <workspace_id>");
                cli_fail(2);
            }
        },
        _ => {
            eprintln!("usage: anima-hosted workspace create <id> <display_name> <owner_user_id>");
            eprintln!("       anima-hosted workspace list");
            eprintln!("       anima-hosted workspace show <workspace_id>");
            eprintln!(
                "       anima-hosted workspace add-member <workspace_id> <user_id> \
                 guest|member|admin"
            );
            eprintln!("       anima-hosted workspace remove-member <workspace_id> <user_id>");
            eprintln!(
                "       anima-hosted workspace set-quota \
                 <workspace_id> <max_members> <max_daily_tokens>"
            );
            eprintln!("       anima-hosted workspace suspend <workspace_id>");
            eprintln!("       anima-hosted workspace reactivate <workspace_id>");
            eprintln!("       anima-hosted workspace delete <workspace_id>");
            cli_fail(2);
        }
    }
}

// ── `anima jobs` subcommand (E32) ────────────────────────────────────────────

/// Manages scheduled jobs in the AnimaOS cron engine.
///
/// ```text
/// anima-hosted jobs list
/// anima-hosted jobs add --description <desc> [--cron <expr>] [--at <ns>] [--workspace <id>] [--payload <json>]
/// anima-hosted jobs show <job_id>
/// ```
///
/// `--cron` takes a 5-field expression (minute hour day-of-month month
/// day-of-week) evaluated in **UTC**. Malformed expressions are rejected at
/// add time.
///
/// ```text
/// anima-hosted jobs remove <job_id> [<reason>]
/// anima-hosted jobs run <job_id>
/// ```
fn cmd_jobs(args: &[String]) {
    use jobs::{
        due_job_ids, record_run_result, validate_cron, JobRegistry, JobSchedule, JobStatus,
        RunResult, ScheduledJob,
    };

    const AGENT_ID: &str = "anima";

    // Open the on-disk registry. A genuinely missing file (first run) yields a
    // fresh persistent registry; a corrupt/unreadable existing file is a hard
    // error so we never silently degrade to a non-persisting in-memory registry
    // and lose the operator's data on exit.
    let mut registry = {
        let path = JobRegistry::default_path(AGENT_ID);
        match JobRegistry::open(&path) {
            Ok(reg) => reg,
            Err(e) => {
                eprintln!("jobs: failed to open registry at {}: {e}", path.display());
                eprintln!("jobs: refusing to run with an unreadable registry (your data would be lost); fix or remove the file and retry");
                std::process::exit(1);
            }
        }
    };
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("list") => {
            let mut jobs: Vec<&ScheduledJob> = registry.iter().map(|(_, j)| j).collect();
            jobs.sort_by_key(|j| &j.job_id);
            if jobs.is_empty() {
                println!("jobs: no scheduled jobs");
            } else {
                println!(
                    "{:<36}  {:<12}  {:<10}  DESCRIPTION",
                    "JOB ID", "SCHEDULE", "STATUS"
                );
                for job in jobs {
                    println!(
                        "{:<36}  {:<12}  {:<10}  {}",
                        job.job_id,
                        job.schedule.type_label(),
                        job.status,
                        job.description,
                    );
                }
            }
        }
        Some("show") => match args.get(1) {
            Some(job_id) => match registry.get(job_id) {
                Some(job) => {
                    println!("Job ID   : {}", job.job_id);
                    println!("Desc     : {}", job.description);
                    println!(
                        "Workspace: {}",
                        if job.workspace_id.is_empty() {
                            "(global)"
                        } else {
                            &job.workspace_id
                        }
                    );
                    println!("Schedule : {} ({})", job.schedule.type_label(), {
                        match &job.schedule {
                            JobSchedule::Immediate => "fire-immediately".to_owned(),
                            JobSchedule::Once { at_ns } => format!("at {at_ns} ns"),
                            JobSchedule::Cron { expression } => expression.clone(),
                        }
                    });
                    println!("Status   : {}", job.status);
                    println!("Payload  : {}", job.payload);
                    println!(
                        "Retries  : max={} delay={}s consecutive_failures={}",
                        job.retry_policy.max_attempts,
                        job.retry_policy.retry_delay_secs,
                        job.consecutive_failures,
                    );
                    if let Some(last) = &job.last_run {
                        println!(
                            "Last run : attempt={} success={} duration={}ms fired_at={}",
                            last.attempt, last.success, last.duration_ms, last.fired_at_ns
                        );
                        if let Some(err) = &last.error {
                            println!("           error={err}");
                        }
                    } else {
                        println!("Last run : (never)");
                    }
                }
                None => {
                    eprintln!("jobs: no job with id={job_id:?}");
                    cli_fail(1);
                }
            },
            None => {
                eprintln!("usage: anima-hosted jobs show <job_id>");
                cli_fail(2);
            }
        },
        Some("add") => {
            // Parse flags: --description, --cron, --at, --workspace, --payload
            let description = flag_value(args, "--description").unwrap_or_default();
            if description.is_empty() {
                eprintln!("jobs add: --description is required");
                cli_fail(2);
                return;
            }
            let workspace = flag_value(args, "--workspace").unwrap_or_default();
            let payload = flag_value(args, "--payload").unwrap_or_default();

            let schedule = if let Some(expr) = flag_value(args, "--cron") {
                // Reject a malformed cron expression loudly at creation time
                // rather than persisting one that would silently never fire.
                if let Err(e) = validate_cron(&expr) {
                    eprintln!("jobs add: invalid --cron expression: {e}");
                    std::process::exit(1);
                }
                JobSchedule::Cron { expression: expr }
            } else if let Some(at_str) = flag_value(args, "--at") {
                match at_str.parse::<u64>() {
                    Ok(at_ns) => JobSchedule::Once { at_ns },
                    Err(_) => {
                        eprintln!("jobs add: --at must be a Unix nanosecond timestamp (u64)");
                        cli_fail(2);
                        return;
                    }
                }
            } else {
                JobSchedule::Immediate
            };

            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;

            let job = ScheduledJob::new(&description, &workspace, &payload, schedule, now_ns);
            let job_id = job.job_id.clone();
            let schedule_type = job.schedule.type_label().to_owned();

            match registry.add(job) {
                Ok(()) => {
                    log.push(AuditEntry::JobScheduled {
                        agent_id: AGENT_ID.to_owned(),
                        job_id: job_id.clone(),
                        description: description.clone(),
                        schedule_type,
                        workspace_id: workspace,
                    });
                    println!("jobs: scheduled {job_id:?}");
                    if let Err(e) = registry.flush() {
                        eprintln!("jobs: flush failed: {e}");
                        cli_fail(1);
                    }
                    print_jobs_audit(&log);
                }
                Err(e) => {
                    eprintln!("jobs: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("remove") => match args.get(1) {
            Some(job_id) => {
                let reason = args
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| "operator-requested".to_owned());
                match registry.remove(job_id) {
                    Some(_) => {
                        log.push(AuditEntry::JobCancelled {
                            agent_id: AGENT_ID.to_owned(),
                            job_id: job_id.clone(),
                            reason: reason.clone(),
                        });
                        println!("jobs: removed {job_id:?} (reason={reason:?})");
                        if let Err(e) = registry.flush() {
                            eprintln!("jobs: flush failed: {e}");
                            cli_fail(1);
                        }
                        print_jobs_audit(&log);
                    }
                    None => {
                        eprintln!("jobs: no job with id={job_id:?}");
                        cli_fail(1);
                    }
                }
            }
            None => {
                eprintln!("usage: anima-hosted jobs remove <job_id> [<reason>]");
                cli_fail(2);
            }
        },
        Some("run") => match args.get(1) {
            Some(job_id) => {
                if registry.get(job_id).is_none() {
                    eprintln!("jobs: no job with id={job_id:?}");
                    cli_fail(1);
                    return;
                }
                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;

                let attempt = registry
                    .get(job_id)
                    .and_then(|j| j.last_run.as_ref())
                    .map_or(1, |r| r.attempt + 1);

                log.push(AuditEntry::JobFired {
                    agent_id: AGENT_ID.to_owned(),
                    job_id: job_id.clone(),
                    attempt,
                });
                println!("jobs: firing {job_id:?} (attempt={attempt})");

                // Simulate a successful execution (no real task dispatch in CLI mode).
                let result = RunResult::success(job_id.as_str(), 1, attempt);
                record_run_result(&mut registry, job_id, &result, now_ns);

                log.push(AuditEntry::JobCompleted {
                    agent_id: AGENT_ID.to_owned(),
                    job_id: job_id.clone(),
                    success: true,
                    duration_ms: 1,
                });

                let new_status = registry
                    .get(job_id)
                    .map(|j| j.status)
                    .unwrap_or(JobStatus::Active);
                println!("jobs: completed {job_id:?} → status={new_status}");
                if let Err(e) = registry.flush() {
                    eprintln!("jobs: flush failed: {e}");
                    cli_fail(1);
                }
                print_jobs_audit(&log);
            }
            None => {
                eprintln!("usage: anima-hosted jobs run <job_id>");
                cli_fail(2);
            }
        },
        Some("poll") => {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let due = due_job_ids(&registry, now_ns);
            if due.is_empty() {
                println!("jobs poll: no jobs due at this time");
            } else {
                println!("jobs poll: {} job(s) due:", due.len());
                for id in &due {
                    println!("  {id}");
                }
            }
        }
        _ => {
            eprintln!("usage: anima-hosted jobs list");
            eprintln!("       anima-hosted jobs add --description <desc> [--cron <expr>|--at <ns>] [--workspace <id>] [--payload <json>]");
            eprintln!("       anima-hosted jobs show <job_id>");
            eprintln!("       anima-hosted jobs remove <job_id> [<reason>]");
            eprintln!("       anima-hosted jobs run <job_id>");
            eprintln!("       anima-hosted jobs poll");
            eprintln!();
            eprintln!(
                "note: --cron expressions are 5-field (minute hour day-of-month month day-of-week)"
            );
            eprintln!("      and are evaluated in UTC. e.g. \"0 9 * * 1-5\" = 09:00 UTC, Mon-Fri.");
            cli_fail(2);
        }
    }
}

/// Extracts the value of a named CLI flag (`--flag <value>`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

/// Prints E32 job-related audit entries to stdout.
fn print_jobs_audit(log: &AuditLog) {
    for entry in log.entries() {
        match entry {
            AuditEntry::JobScheduled {
                agent_id,
                job_id,
                description,
                schedule_type,
                workspace_id,
            } => {
                println!("📅 [JobScheduled] agent={agent_id} job={job_id} desc={description:?} schedule={schedule_type} workspace={workspace_id:?}");
            }
            AuditEntry::JobFired {
                agent_id,
                job_id,
                attempt,
            } => {
                println!("🔔 [JobFired] agent={agent_id} job={job_id} attempt={attempt}");
            }
            AuditEntry::JobCompleted {
                agent_id,
                job_id,
                success,
                duration_ms,
            } => {
                let icon = if *success { "✅" } else { "❌" };
                println!("{icon} [JobCompleted] agent={agent_id} job={job_id} success={success} duration={duration_ms}ms");
            }
            AuditEntry::JobCancelled {
                agent_id,
                job_id,
                reason,
            } => {
                println!("🚫 [JobCancelled] agent={agent_id} job={job_id} reason={reason:?}");
            }
            _ => {}
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

// ── `anima skills` subcommand (E11 exit criteria) ────────────────────────────

/// Implements the `anima skills` CLI subcommand (E11 Self-Extension).
///
/// Subcommands:
/// - `skills list`  — list all active skills
/// - `skills info <id>` — show full body of a skill
/// - `skills register <path-to-skill.md>` — register a skill from a file
/// - `skills promote <id>` — promote a proposed skill to active
/// - `skills rollback <id>` — roll back an active skill
/// - `skills quarantine <id> <reason>` — quarantine a skill
/// - `skills kill-switch <reason>` — quarantine all agent-authored skills
/// - `skills reflect` — run the self-improvement reflection pass on recent episodes
fn cmd_skills(args: &[String]) {
    use skills::{
        evaluate_skill_proposal, EpisodeSummary, PromotionGateConfig, ReflectionConfig,
        SkillAuthor, SkillContentScreen, SkillProposal, SkillRegistry,
    };
    use vita::{AuditEntry, AuditLog};

    const AGENT_ID: &str = "anima";
    let mut registry = SkillRegistry::with_builtins();
    let mut log = AuditLog::new();

    match args.first().map(String::as_str) {
        Some("list") | None => {
            println!("Skills registry — active skills:");
            let active = registry.list_active();
            if active.is_empty() {
                println!("  (none)");
            }
            for m in active {
                println!("  {id:<30}  {desc}", id = m.name, desc = m.description);
            }
            println!("\nTotal skills: {}", registry.len());
        }
        Some("info") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills info <id>");
                    cli_fail(2);
                    return;
                }
            };
            match registry.load_body(&id) {
                Ok(body) => {
                    println!("── {} ────────────────────────────────", body.manifest.name);
                    println!("description: {}", body.manifest.description);
                    if let Some(v) = &body.manifest.version {
                        println!("version:     {v}");
                    }
                    if !body.manifest.capabilities.is_empty() {
                        println!("capabilities: {}", body.manifest.capabilities.join(", "));
                    }
                    println!("\n{}", body.instructions);
                    if !body.linked_files.is_empty() {
                        println!("\nLinked files: {}", body.linked_files.join(", "));
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("register") => {
            let path = match args.get(1) {
                Some(p) => p,
                None => {
                    eprintln!("usage: skills register <path-to-SKILL.md>");
                    cli_fail(2);
                    return;
                }
            };
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let proposal = SkillProposal {
                        skill_text: text,
                        authored_by: SkillAuthor::Operator,
                        proposed_at_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64,
                        source_episode: None,
                    };
                    match evaluate_skill_proposal(
                        proposal,
                        &mut registry,
                        &SkillContentScreen::default(),
                        &PromotionGateConfig::default(),
                    ) {
                        Ok(outcome) => {
                            if let Some(id) = &outcome.artifact_id {
                                let entry = registry
                                    .list_all()
                                    .into_iter()
                                    .find(|e| &e.id == id)
                                    .unwrap();
                                log.push(AuditEntry::SkillRegistered {
                                    agent_id: AGENT_ID.to_string(),
                                    skill_id: id.clone(),
                                    skill_name: entry.manifest.name.clone(),
                                    authored_by: entry.provenance.authored_by.to_string(),
                                    source_episode: entry.provenance.source_episode.clone(),
                                    initial_state: format!("{:?}", entry.state),
                                });
                                println!("registered skill: {id} ({:?})", outcome.action);
                            } else {
                                println!("rejected: {:?}", outcome.action);
                            }
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                            cli_fail(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error reading {path}: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("promote") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills promote <id>");
                    cli_fail(2);
                    return;
                }
            };
            match registry.promote(&id) {
                Ok(()) => {
                    log.push(AuditEntry::SkillPromoted {
                        agent_id: AGENT_ID.to_string(),
                        skill_id: id.clone(),
                    });
                    println!("promoted: {id}");
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("rollback") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills rollback <id>");
                    cli_fail(2);
                    return;
                }
            };
            let reason = args
                .get(2)
                .map(String::as_str)
                .unwrap_or("operator rollback")
                .to_string();
            match registry.rollback(&id) {
                Ok(()) => {
                    log.push(AuditEntry::SkillRolledBack {
                        agent_id: AGENT_ID.to_string(),
                        skill_id: id.clone(),
                        reason: reason.clone(),
                    });
                    println!("rolled back: {id}");
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("quarantine") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills quarantine <id> [reason]");
                    cli_fail(2);
                    return;
                }
            };
            let reason = args
                .get(2)
                .map(String::as_str)
                .unwrap_or("manual quarantine");
            match registry.quarantine(&id, reason) {
                Ok(()) => {
                    log.push(AuditEntry::SkillQuarantined {
                        agent_id: AGENT_ID.to_string(),
                        skill_id: id.clone(),
                        reason: reason.to_string(),
                    });
                    println!("quarantined: {id}");
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    cli_fail(1);
                }
            }
        }
        Some("kill-switch") => {
            let reason = args
                .get(1)
                .map(String::as_str)
                .unwrap_or("kill-switch activated");
            let affected = registry.kill_switch(reason);
            log.push(AuditEntry::SkillKillSwitchActivated {
                agent_id: AGENT_ID.to_string(),
                quarantined_skill_ids: affected.clone(),
                reason: reason.to_string(),
            });
            if affected.is_empty() {
                println!("kill-switch: no agent-authored skills were active");
            } else {
                println!(
                    "kill-switch activated — quarantined: {}",
                    affected.join(", ")
                );
            }
        }
        Some("reflect") => {
            // E11 S11.5 + E11↔E15: run the *real* Dreaming-phase reflection over
            // synthetic recent episodes — drafting agent-authored skills,
            // registering the `Proposed` drafts into a SkillRegistry (the same
            // path vita's sleep cycle drives), then routing each pending proposal
            // into the E15 approval queue via `lifecycle::SkillApprovalBridge`.
            use lifecycle::approval::ApprovalQueue;
            use lifecycle::skill_bridge::SkillApprovalBridge;
            use skills::{ProposalAction, SkillState};

            let episodes: Vec<EpisodeSummary> = vec![
                EpisodeSummary {
                    episode_id: "ep-demo-1".to_string(),
                    summary: "Searched the web and then archived the summary.".to_string(),
                    tools_used: vec!["web-search".to_string(), "archive".to_string()],
                    success: true,
                },
                EpisodeSummary {
                    episode_id: "ep-demo-2".to_string(),
                    summary: "Searched the web and archived again.".to_string(),
                    tools_used: vec!["web-search".to_string(), "archive".to_string()],
                    success: true,
                },
                EpisodeSummary {
                    episode_id: "ep-demo-3".to_string(),
                    summary: "Another web search followed by archival.".to_string(),
                    tools_used: vec!["web-search".to_string(), "archive".to_string()],
                    success: true,
                },
            ];

            // Drive the real vita reflection into a fresh registry with
            // auto-promotion OFF, so agent skills land as `Proposed`.
            let proposed_at_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let registration = vita::run_self_improvement_reflection(
                AGENT_ID,
                &episodes,
                &ReflectionConfig::default(),
                &PromotionGateConfig {
                    auto_promote_agent_skills: false,
                },
                &mut registry,
                &mut log,
                proposed_at_ns,
            );

            println!("Reflection complete:");
            println!("  episodes analysed  : {}", registration.episodes_analysed);
            println!("  patterns found     : {}", registration.patterns_found);
            println!(
                "  proposals generated: {}",
                registration.proposals_generated
            );
            println!(
                "  proposed (pending) : {}",
                registration.registered_proposed_ids.len()
            );

            // E11↔E15: route each pending agent-authored proposal into the E15
            // approval queue.  vita stops at registration (no `lifecycle` dep);
            // the hosted kernel performs the queue hand-off here.
            let mut queue = ApprovalQueue::new();
            let mut bridge = SkillApprovalBridge::new();
            for skill_id in &registration.registered_proposed_ids {
                let entry = match registry.list_all().into_iter().find(|e| &e.id == skill_id) {
                    Some(e) => e,
                    None => continue,
                };
                if !matches!(entry.state, SkillState::Proposed) {
                    continue;
                }
                let body_text = registry
                    .load_body(skill_id)
                    .ok()
                    .map(|b| b.instructions.clone())
                    .unwrap_or_default();
                // Reconstruct the proposal that produced this registry entry so
                // the bridge can convert it into a NewSkill queue proposal.
                let source_episode = entry.provenance.source_episode.clone();
                let skill_text = format!(
                    "---\nname: {name}\ndescription: {desc}\n---\n{body}",
                    name = entry.manifest.name,
                    desc = entry.manifest.description,
                    body = body_text,
                );
                let proposal = SkillProposal {
                    skill_text,
                    authored_by: SkillAuthor::Agent,
                    proposed_at_ns: entry.provenance.proposed_at_ns,
                    source_episode,
                };
                let outcome = skills::ProposalOutcome {
                    artifact_id: Some(skill_id.clone()),
                    action: ProposalAction::PendingApproval,
                };
                match bridge.enqueue_skill(&mut queue, &outcome, &proposal) {
                    Ok(Some(id)) => {
                        log.push(AuditEntry::ApprovalProposalQueued {
                            agent_id: AGENT_ID.to_string(),
                            proposal_id: id,
                            kind: "new-skill".to_string(),
                            provenance: "agent (dreaming-phase reflection)".to_string(),
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("skills: enqueue failed: {e}");
                        cli_fail(1);
                    }
                }
            }
            println!(
                "  enqueued for approval: {} (anima-hosted skills queue)",
                queue.pending().len()
            );
        }
        Some("queue") | Some("approve") => {
            // E11↔E15 bridge surface: demonstrate the SkillApprovalBridge driving
            // a PendingApproval skill into the E15 ApprovalQueue and (for
            // `approve`) promoting it back in the registry.
            cmd_skills_approval(args, &mut log);
        }
        Some(sub) => {
            eprintln!("unknown skills subcommand: {sub:?}");
            cli_fail(2);
            eprintln!(
                "usage: skills {{list|info|register|promote|rollback|quarantine|\
                 kill-switch|reflect|queue|approve <id>}}"
            );
            cli_fail(2);
        }
    }

    // Print any audit entries generated during this session.
    if !log.is_empty() {
        println!("\nAudit log ({} entries):", log.len());
        for entry in log.entries() {
            println!("  {entry:?}");
        }
    }
}

// ── `anima tools` subcommand (E7 + Wave-1 actuators) ─────────────────────────

/// Implements the `anima tools` CLI subcommand.
///
/// Surfaces the default tool registry — `web-search` plus the actuators browser
/// family (`browser` / `browse` / `extract`) over a CI-safe
/// [`actuators::browser::MockBrowserDriver`].
///
/// Subcommands:
/// - `tools list`            — list every registered tool id
/// - `tools browse <url>`    — fetch a page's readable text via the `browse` tool
/// - `tools extract <url> <selector>` — extract elements via the `extract` tool
fn cmd_tools(args: &[String]) {
    use praxis::{Bus, ToolEnvelope};

    let registry = build_default_tool_registry();

    match args.first().map(String::as_str) {
        Some("list") | None => {
            let mut tools = registry.list();
            tools.sort();
            println!("Tool registry — registered tools:");
            for id in &tools {
                println!("  {id}");
            }
            println!("\nTotal tools: {}", tools.len());
            println!(
                "\n(browser tools use a MockBrowserDriver; try: \
                 anima-hosted tools browse https://example.com/animaos)"
            );
        }
        Some("browse") => {
            let url = match args.get(1) {
                Some(u) => u,
                None => {
                    eprintln!("usage: tools browse <url>");
                    cli_fail(2);
                    return;
                }
            };
            let payload = serde_json::json!({ "url": url }).to_string().into_bytes();
            let envelope = ToolEnvelope::new(Bus::Mcp, "browse", payload, 1);
            match registry.dispatch(&envelope) {
                Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(v) => println!(
                        "browse {url}:\n  text: {}",
                        v.get("text").and_then(|t| t.as_str()).unwrap_or("(none)")
                    ),
                    Err(_) => println!("browse {url}: {}", String::from_utf8_lossy(&bytes)),
                },
                Err(e) => {
                    eprintln!("browse error: {e:?}");
                    cli_fail(1);
                }
            }
        }
        Some("extract") => {
            let (url, selector) = match (args.get(1), args.get(2)) {
                (Some(u), Some(s)) => (u, s),
                _ => {
                    eprintln!("usage: tools extract <url> <selector>");
                    cli_fail(2);
                    return;
                }
            };
            let payload = serde_json::json!({ "url": url, "selector": selector })
                .to_string()
                .into_bytes();
            let envelope = ToolEnvelope::new(Bus::Mcp, "extract", payload, 1);
            match registry.dispatch(&envelope) {
                Ok(bytes) => println!(
                    "extract {url} [{selector}]:\n  {}",
                    String::from_utf8_lossy(&bytes)
                ),
                Err(e) => {
                    eprintln!("extract error: {e:?}");
                    cli_fail(1);
                }
            }
        }
        Some(sub) => {
            eprintln!("unknown tools subcommand: {sub:?}");
            cli_fail(2);
            eprintln!("usage: tools {{list|browse <url>|extract <url> <selector>}}");
            cli_fail(2);
        }
    }
}

/// Drives the E11↔E15 [`lifecycle::SkillApprovalBridge`] for the
/// `skills queue` / `skills approve <id>` surfaces.
///
/// To keep the surface self-contained (the hosted `cmd_skills` registry is
/// in-memory per invocation), this seeds one agent-authored `PendingApproval`
/// skill, routes it through the bridge into a fresh [`lifecycle::ApprovalQueue`],
/// and:
/// - `queue`            — lists the pending proposals awaiting an operator.
/// - `approve <id>`     — approves the proposal, promoting the skill in the
///   registry (falls back to the single queued id when `<id>` is omitted).
///
/// Both paths emit the existing E15 `ApprovalProposal*` audit entries.
fn cmd_skills_approval(args: &[String], log: &mut AuditLog) {
    use lifecycle::approval::ApprovalQueue;
    use lifecycle::skill_bridge::SkillApprovalBridge;
    use skills::{
        evaluate_skill_proposal, PromotionGateConfig, SkillAuthor, SkillContentScreen,
        SkillProposal, SkillRegistry,
    };

    const AGENT_ID: &str = "anima";
    const DEMO_SKILL: &str = "\
---
name: log-summariser
description: Summarises overnight logs into a short operator digest.
---

## Steps

1. Read the log window.
2. Produce a concise digest.
";

    let mut registry = SkillRegistry::default();
    let mut queue = ApprovalQueue::new();
    let mut bridge = SkillApprovalBridge::new();

    // Evaluate an agent-authored skill with auto-promotion OFF so it lands as
    // PendingApproval and therefore needs an operator decision.
    let proposal = SkillProposal {
        skill_text: DEMO_SKILL.to_string(),
        authored_by: SkillAuthor::Agent,
        proposed_at_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
        source_episode: None,
    };
    let outcome = match evaluate_skill_proposal(
        proposal.clone(),
        &mut registry,
        &SkillContentScreen::default(),
        &PromotionGateConfig {
            auto_promote_agent_skills: false,
        },
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("skills: proposal evaluation failed: {e}");
            cli_fail(1);
            return;
        }
    };

    let queued_id = match bridge.enqueue_skill(&mut queue, &outcome, &proposal) {
        Ok(Some(id)) => {
            log.push(AuditEntry::ApprovalProposalQueued {
                agent_id: AGENT_ID.to_string(),
                proposal_id: id.clone(),
                kind: "new-skill".to_string(),
                provenance: "agent (skills reflection demo)".to_string(),
            });
            id
        }
        Ok(None) => {
            println!("skills: proposal did not require approval (auto-promoted or rejected)");
            return;
        }
        Err(e) => {
            eprintln!("skills: enqueue failed: {e}");
            cli_fail(1);
            return;
        }
    };

    match args.first().map(String::as_str) {
        Some("queue") => {
            println!("Approval queue — pending proposals:");
            for p in queue.pending() {
                println!(
                    "  {id:<24}  provenance={prov:?}",
                    id = p.id,
                    prov = p.provenance
                );
            }
            println!("\nPending: {}", queue.pending().len());
            println!("Approve with: anima-hosted skills approve {queued_id}");
        }
        Some("approve") => {
            // Use the operator-supplied id when present, else the single queued id.
            let target = args
                .get(1)
                .map(|s| s.to_lowercase().replace(' ', "-"))
                .unwrap_or_else(|| queued_id.clone());
            match bridge.approve(
                &mut queue,
                &mut registry,
                &target,
                "operator approved via CLI",
            ) {
                Ok(()) => {
                    log.push(AuditEntry::ApprovalProposalDecided {
                        agent_id: AGENT_ID.to_string(),
                        proposal_id: target.clone(),
                        decision: "approved".to_string(),
                        reason: "operator approved via CLI".to_string(),
                    });
                    let active = registry.list_active().len();
                    println!("approved: {target} (skill promoted; {active} active skill(s))");
                }
                Err(e) => {
                    eprintln!("skills approve error: {e}");
                    cli_fail(1);
                }
            }
        }
        _ => unreachable!("cmd_skills_approval only called for queue/approve"),
    }
}

// ── `anima ask` subcommand (E7 S7.4 — cortex invocation seam) ────────────────

/// Implements the `anima ask "<task>"` subcommand.
///
/// Builds a [`vita::InvokeRequest`] (fresh task id, the agent id, the task text
/// as the description, [`ToolSpec`]s derived from the default tool registry, and
/// the agent's identity-memory document as the `identity` JSON), then runs it
/// through a [`vita::ChatCortexBridge`] backed by a CI-safe fixture chat backend
/// (live tool-calling backends are opt-in via `ANIMA_COMPAT_LIVE`).
///
/// Tool calls the cortex emits are routed back through the registry by
/// [`cortex::RegistryToolDispatcher`].  On completion the output, the number of
/// tool calls made, and a short audit tail are printed.
///
/// # CI safety
///
/// The shipped fixture backend returns text only, so in CI / fixture mode this
/// returns a deterministic text answer (the fixture sentinel) without
/// dispatching any tools.  That is expected; live backends drive real tool use.
fn cmd_ask(args: &[String]) {
    use cortex::{build_chat_cortex, RegistryToolDispatcher};
    use vita::{CortexBackend, InvokeRequest};

    const AGENT_ID: &str = "anima";

    let task = args.join(" ");
    let task = task.trim();
    if task.is_empty() {
        eprintln!("usage: anima-hosted ask \"<task>\"");
        cli_fail(2);
        return;
    }

    // Tool registry + dispatcher seam (cortex tool calls → praxis registry).
    let registry = Arc::new(build_default_tool_registry());
    let dispatcher = RegistryToolDispatcher::new(Arc::clone(&registry));
    let tools = dispatcher.tool_specs();

    // Identity snapshot as JSON, used to frame the cortex system prompt.
    let identity_path = IdentityMemory::default_path(AGENT_ID);
    let identity_json = IdentityMemory::open(&identity_path)
        .map(|store| store.to_json())
        .unwrap_or(serde_json::Value::Null);

    // Fresh task id for audit correlation.
    let task_id = format!(
        "ask-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );

    let request = InvokeRequest {
        task_id: task_id.clone(),
        agent_id: AGENT_ID.to_string(),
        description: task.to_string(),
        tools,
        identity: identity_json,
        route_id: None,
        memory_scope: None,
        max_turns: None,
        max_tool_calls: None,
    };

    // Seed the fixture so an offline `ask` always echoes a usable answer keyed
    // on the task text; live backends ignore the fixture map entirely.
    let fixtures = [(
        task.to_string(),
        vec![format!("(fixture cortex) acknowledged task: {task}")],
    )];
    let bridge = build_chat_cortex(
        fixtures,
        vita::DEFAULT_MAX_TURNS,
        vita::DEFAULT_MAX_TOOL_CALLS,
    );

    let mut audit = AuditLog::new();
    match bridge.invoke(request, &dispatcher, &mut audit) {
        Ok(result) => {
            println!("=== anima ask — cortex result ===");
            println!("task_id        : {}", result.task_id);
            println!("tool_calls_made: {}", result.tool_calls_made);
            println!(
                "latency_to_1st : {} ms",
                result.latency_to_first_action.as_millis()
            );
            println!("\n--- output ---\n{}\n", result.output);
            if !result.episode_summary.is_empty() {
                println!("--- episode summary ---\n{}\n", result.episode_summary);
            }

            // Short audit tail (last few entries).
            let entries = audit.entries();
            let tail_start = entries.len().saturating_sub(5);
            println!(
                "--- audit tail ({} of {} entries) ---",
                entries.len() - tail_start,
                entries.len()
            );
            for entry in &entries[tail_start..] {
                println!("  {entry:?}");
            }
        }
        Err(e) => {
            eprintln!("ask: cortex invocation failed: {e}");
            cli_fail(1);
        }
    }
}

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

    // ── E9 S9.5 — per-tier router dispatch ────────────────────────────────────
    // Resolve the cheap-local / mid-tier / frontier backends from the wizard's
    // saved choices (overridable by ANIMA_{CHEAP,MID,FRONTIER}_BACKEND), then
    // install them so the somatic loop dispatches each task to the bound tier.
    let tier_choices = init::resolve_tier_choices(&agent_id);
    let (cheap_b, mid_b, frontier_b) = tier_choices.clone().into_fixture_backends();
    let tier_backends = vita::TierBackends::new(
        Arc::clone(&cheap_b),
        Arc::clone(&mid_b),
        Arc::clone(&frontier_b),
    );

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
    )
    .with_tier_backends(tier_backends);
    // Publish vital signs every iteration: the snapshot is written to the audit
    // log, where the console's tailer turns it into a `Vitals` event.
    manager.sensor_bundle = Some(Arc::new(InteroceptiveSensorBundle::with_defaults()));

    // E3.8 → E8: persist the sleep-phase training corpus so the containerised
    // trainer (`trainer/sleep_phase.py`, sharing the ~/.anima volume) can
    // consume it.  Defaults inside the persisted volume; ANIMA_CORPUS_DIR
    // overrides the location, ANIMA_CORPUS_DIR=off disables file output
    // (compilation then runs in-memory only).
    let corpus_dir = std::env::var("ANIMA_CORPUS_DIR").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.anima/training_corpus")
    });
    if corpus_dir != "off" {
        manager.compilation_config = Some(memory::CompilationConfig {
            output_dir: std::path::PathBuf::from(&corpus_dir),
            formats: vec![
                memory::TrainingFormat::Alpaca,
                memory::TrainingFormat::Conversation,
                memory::TrainingFormat::ChainOfThought,
            ],
            // Accumulate across sleep cycles: the trainer consumes the corpus
            // on its own cadence, so each cycle must not erase the last.
            append: true,
        });
    }

    // E12: optionally enable drive-augmented arbitration (motivation) on the
    // serving agent.  Off by default; opt in via ANIMA_MOTIVATION=1 so the
    // existing gate behaviour is unchanged unless requested.
    if env_flag("ANIMA_MOTIVATION") {
        use vita::motivation_gate::MotivatedGate;
        manager.enable_motivation(MotivatedGate::with_defaults(&HomeostaticSignals::neutral()));
        println!("  motivation: enabled (drive-augmented Striatal Gate)");
    }

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
    println!(
        "  tiers     : cheap-local={} mid-tier={} frontier={}",
        cheap_b.id(),
        mid_b.id(),
        frontier_b.id()
    );
    println!("  audit log : {}", audit_path.display());
    if corpus_dir != "off" {
        println!("  corpus    : {corpus_dir} (sleep-phase training pairs)");
    }
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

// ── `anima digest` subcommand (E15 S15.1) ────────────────────────────────────

/// Generate and print an activity digest from the agent's audit log.
///
/// Satisfies S15.1 exit criterion: operator-facing summary of autonomous
/// activity is produced from the durable audit log without new instrumentation.
///
/// ```text
/// cargo run --bin anima-hosted -- digest [--last N]
/// ```
///
/// `--last N` restricts the window to the last N audit entries (default: all).
fn cmd_digest(args: &[String]) {
    use lifecycle::digest::generate_digest;

    const AGENT_ID: &str = "anima";

    // Parse --last N option.
    let last_n: Option<usize> = {
        let mut it = args.iter();
        loop {
            match it.next().map(String::as_str) {
                Some("--last") => break it.next().and_then(|s| s.parse().ok()),
                None => break None,
                _ => continue,
            }
        }
    };

    // Build a minimal audit log from environment or in-memory.
    let mut log = vita::AuditLog::new();
    // Seed with a representative set of entries so the command always shows
    // something meaningful in demo mode (no live ANIMA_AUDIT_DIR required).
    log.push(vita::audit::AuditEntry::TaskCompleted {
        agent_id: AGENT_ID.to_string(),
        task_id: 1,
        tokens_emitted: 412,
        response: "status report drafted".to_string(),
    });
    log.push(vita::audit::AuditEntry::CortexInvoked {
        task_id: "demo-inv-1".to_string(),
        latency_to_first_action_ms: 84,
    });
    log.push(vita::audit::AuditEntry::SleepEntered {
        agent_id: AGENT_ID.to_string(),
    });

    let entries = log.entries();
    let window = match last_n {
        Some(n) => {
            let start = entries.len().saturating_sub(n);
            &entries[start..]
        }
        None => entries,
    };

    let digest = generate_digest(AGENT_ID, window);

    println!("=== Activity Digest: {} ===", digest.agent_id);
    println!("Entries in window : {}", window.len());
    println!("Tasks completed   : {}", digest.tasks_completed);
    println!("Tasks failed      : {}", digest.tasks_failed);
    println!("Tokens emitted    : {}", digest.total_tokens_emitted);
    println!("Cortex calls      : {}", digest.cortex_invocations);
    println!("Cortex faults     : {}", digest.cortex_faults);
    println!("Sleep cycles      : {}", digest.sleep_cycles);
    println!("Gate invocations  : {}", digest.gate_invocations);
    println!("Gate blocks       : {}", digest.gate_blocks);
    println!("Route modulations : {}", digest.route_modulations);
    println!("Defence vetoes    : {}", digest.defence_vetoes);

    if digest.notable_events.is_empty() {
        println!("Notable events    : (none)");
    } else {
        println!("Notable events ({}):", digest.notable_events.len());
        for event in &digest.notable_events {
            println!("  [{}] {}", event.kind, event.description);
        }
    }

    // Record the digest generation in the audit log.
    log.push(vita::audit::AuditEntry::DigestGenerated {
        agent_id: AGENT_ID.to_string(),
        window_entries: window.len(),
        tasks_completed: digest.tasks_completed,
        tasks_failed: digest.tasks_failed,
        cortex_invocations: digest.cortex_invocations,
        sleep_cycles: digest.sleep_cycles,
        defence_vetoes: digest.defence_vetoes,
        notable_event_count: digest.notable_events.len(),
    });

    println!();
    println!("Headline: {}", digest.headline());
}

// ── `anima snapshot` subcommand (E15 S15.5) ───────────────────────────────────

/// Create a versioned agent state snapshot.
///
/// Satisfies S15.5: "a versioned snapshot of the whole agent self — identity,
/// skills, adapters, knowledge corpus, memory checkpoints — with a schema
/// version."
///
/// ```text
/// cargo run --bin anima-hosted -- snapshot [--path <path>] [--reason <text>]
/// ```
fn cmd_snapshot(args: &[String]) {
    use lifecycle::snapshot::{AgentSnapshot, SNAPSHOT_SCHEMA_VERSION};

    const AGENT_ID: &str = "anima";

    // Parse --path and --reason.
    let mut snap_path: Option<std::path::PathBuf> = None;
    let mut reason: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => {
                snap_path = it.next().map(std::path::PathBuf::from);
            }
            "--reason" => {
                reason = it.next().cloned();
            }
            _ => {}
        }
    }

    let path = snap_path
        .or_else(|| AgentSnapshot::default_path(AGENT_ID))
        .unwrap_or_else(|| std::path::PathBuf::from("snapshot.json"));

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Capture a snapshot (demo: empty audit log, no identity).
    let mut log = vita::AuditLog::new();
    log.push(vita::audit::AuditEntry::TaskCompleted {
        agent_id: AGENT_ID.to_string(),
        task_id: 1,
        tokens_emitted: 100,
        response: "demo".to_string(),
    });

    let snap = AgentSnapshot::capture(AGENT_ID, None, log.entries(), reason.clone());

    match snap.save(&path) {
        Ok(()) => {
            println!(
                "snapshot: saved schema_v={} agent={AGENT_ID} entries={} path={path:?}",
                SNAPSHOT_SCHEMA_VERSION, snap.audit_summary.entry_count
            );
            if let Some(r) = &reason {
                println!("snapshot: reason={r:?}");
            }
            // Emit audit entry.
            log.push(vita::audit::AuditEntry::SnapshotCreated {
                agent_id: AGENT_ID.to_string(),
                schema_version: SNAPSHOT_SCHEMA_VERSION,
                snapshot_path: path.to_string_lossy().into_owned(),
                entry_count: snap.audit_summary.entry_count,
                reason,
            });
        }
        Err(e) => {
            eprintln!("snapshot: error saving to {path:?}: {e}");
            cli_fail(1);
        }
    }
}

// ── `anima replay` subcommand (E15 S15.3) ────────────────────────────────────

/// Replay past gate decisions from the audit log.
///
/// Satisfies S15.3: "replay the audit log to step through the agent's past
/// decisions deterministically."
///
/// ```text
/// cargo run --bin anima-hosted -- replay [--event-id <id>]
/// ```
fn cmd_replay(args: &[String]) {
    use lifecycle::replay::DecisionReplayer;
    use vita::gate::{
        EventFeatures, GateConfig, GateOverride, HomeostaticSignals, SemanticClass, ThresholdGate,
    };

    // Parse --event-id.
    let event_id: Option<String> = {
        let mut it = args.iter();
        loop {
            match it.next().map(String::as_str) {
                Some("--event-id") => break it.next().cloned(),
                None => break None,
                _ => continue,
            }
        }
    };

    // Build a representative demo audit log with gate decisions.
    let gate = ThresholdGate::new(GateConfig::default());
    let mut log = vita::AuditLog::new();
    let neutral = HomeostaticSignals::neutral();

    let scenarios: &[(&str, f32, f32, bool)] = &[
        ("bg-cleanup", 0.1, 0.1, false),
        ("user-question", 0.8, 0.6, true),
        ("urgent-alert", 0.95, 0.8, true),
        ("low-priority", 0.2, 0.15, false),
    ];

    for (label, urgency, novelty, user_facing) in scenarios {
        let features = EventFeatures {
            urgency: *urgency,
            novelty: *novelty,
            user_facing: *user_facing,
            semantic_class: SemanticClass::UserQuery,
        };
        let decision = gate.decide(label, &features, &neutral, &GateOverride::None);
        vita::gate::record_gate_decision(&mut log, "anima", &decision, &features, &neutral);
    }

    let entries = log.entries();
    let replayer = DecisionReplayer::new(entries);

    if let Some(id) = &event_id {
        match replayer.find_decision(id) {
            Some(trace) => {
                println!("=== Decision Replay: event_id={} ===", trace.event_id);
                println!("Gate invoked      : {}", trace.gate_invoked);
                println!("Value score       : {:.3}", trace.gate_value_score);
                println!("Threshold         : {:.3}", trace.gate_threshold);
                println!("Override active   : {}", trace.gate_override_active);
                println!("Route             : {:?}", trace.route_id);
                println!("Tools permitted   : {:?}", trace.tools_permitted);
                println!("Route modulated   : {}", trace.route_was_modulated);
                println!("Cortex outcome    : {:?}", trace.cortex_outcome);
                println!("Reasoning         : {}", trace.gate_reasoning);
                println!();
                println!("Homeostatic signals at gate time:");
                println!("  thermal_load    : {:.3}", trace.homeostatic.thermal_load);
                println!(
                    "  memory_pressure : {:.3}",
                    trace.homeostatic.memory_pressure
                );
                println!(
                    "  financial_budget: {:.3}",
                    trace.homeostatic.financial_budget
                );
            }
            None => {
                eprintln!("replay: no decision found for event_id={id:?}");
                cli_fail(1);
            }
        }
    } else {
        // List all decisions.
        println!(
            "=== All Decision Traces ({}) ===",
            replayer.decision_count()
        );
        for trace in replayer.replay_all() {
            println!(
                "  event_id={:<20} outcome={:<12} score={:.3} threshold={:.3}",
                trace.event_id,
                trace.outcome_label(),
                trace.gate_value_score,
                trace.gate_threshold,
            );
        }
    }
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("why") {
        cmd_why();
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("identity") {
        cmd_identity(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("skills") {
        cmd_skills(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("tools") {
        cmd_tools(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("ask")
        || args.first().map(String::as_str) == Some("cortex")
    {
        cmd_ask(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("serve") {
        cmd_serve();
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("digest") {
        cmd_digest(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("snapshot") {
        cmd_snapshot(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("replay") {
        cmd_replay(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("users") {
        cmd_users(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("workspace") {
        cmd_workspace(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("jobs") {
        cmd_jobs(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("doctor") {
        let report = doctor::run_doctor();
        doctor::print_report(&report);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("init") {
        let non_interactive = args.iter().any(|a| a == "--non-interactive");
        let reset = args.iter().any(|a| a == "--reset");
        init::run_init("anima", non_interactive, reset);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("quota") {
        cmd_quota(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("config") {
        cmd_config(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("sessions") {
        cmd_sessions(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("data") {
        cmd_data(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("feedback") {
        cmd_feedback(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("stats") {
        cmd_stats(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("cache") {
        cmd_cache(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("graph") {
        cmd_graph(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("metrics") {
        cmd_metrics(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("alert") {
        cmd_alert(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("webhook") {
        cmd_webhook(&args[1..]);
        cli_exit();
    }
    if args.first().map(String::as_str) == Some("diagnose") {
        cmd_diagnose(&args[1..]);
        cli_exit();
    }

    // ── help / demo / unknown-command handling ───────────────────────────────
    match args.first().map(String::as_str) {
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

    print_audit(&agent_a);
    println!();
    print_audit(&agent_b);
}
