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
                None => eprintln!("users: no user with id={user_id:?}"),
            },
            None => eprintln!("usage: anima-hosted users show <user_id>"),
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
                        }
                        print_user_audit(&log);
                    }
                    Err(e) => eprintln!("users: {e}"),
                },
                Err(e) => eprintln!("users: invalid trust tier: {e}"),
            },
            _ => eprintln!(
                "usage: anima-hosted users trust <user_id> \
                     unknown|verified|trusted|operator"
            ),
        },
        Some("consent") => match (args.get(1), args.get(2), args.get(3)) {
            (Some(user_id), Some(cat_str), Some(action)) => match DataCategory::from_str(cat_str) {
                Ok(category) => {
                    let granted = match action.as_str() {
                        "grant" => true,
                        "revoke" => false,
                        other => {
                            eprintln!("users: expected 'grant' or 'revoke', got {other:?}");
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
                            }
                            print_user_audit(&log);
                        }
                        None => eprintln!("users: no user with id={user_id:?}"),
                    }
                }
                Err(e) => eprintln!("users: invalid category: {e}"),
            },
            _ => eprintln!(
                "usage: anima-hosted users consent <user_id> \
                     episodic_memory|identity_facts|usage_stats|knowledge_corpus \
                     grant|revoke"
            ),
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
                            }
                            print_user_audit(&log);
                        }
                        Err(e) => eprintln!("users: {e}"),
                    }
                }
                _ => eprintln!(
                    "usage: anima-hosted users register <user_id> <display_name> <channel>"
                ),
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
/// anima-hosted jobs remove <job_id> [<reason>]
/// anima-hosted jobs run <job_id>
/// ```
fn cmd_jobs(args: &[String]) {
    use jobs::{
        due_job_ids, record_run_result, JobRegistry, JobSchedule, JobStatus, RunResult,
        ScheduledJob,
    };

    const AGENT_ID: &str = "anima";

    let mut registry = {
        let path = JobRegistry::default_path(AGENT_ID);
        JobRegistry::open(&path).unwrap_or_else(|_| JobRegistry::in_memory())
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
                None => eprintln!("jobs: no job with id={job_id:?}"),
            },
            None => eprintln!("usage: anima-hosted jobs show <job_id>"),
        },
        Some("add") => {
            // Parse flags: --description, --cron, --at, --workspace, --payload
            let description = flag_value(args, "--description").unwrap_or_default();
            if description.is_empty() {
                eprintln!("jobs add: --description is required");
                return;
            }
            let workspace = flag_value(args, "--workspace").unwrap_or_default();
            let payload = flag_value(args, "--payload").unwrap_or_default();

            let schedule = if let Some(expr) = flag_value(args, "--cron") {
                JobSchedule::Cron { expression: expr }
            } else if let Some(at_str) = flag_value(args, "--at") {
                match at_str.parse::<u64>() {
                    Ok(at_ns) => JobSchedule::Once { at_ns },
                    Err(_) => {
                        eprintln!("jobs add: --at must be a Unix nanosecond timestamp (u64)");
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
                    }
                    print_jobs_audit(&log);
                }
                Err(e) => eprintln!("jobs: {e}"),
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
                        }
                        print_jobs_audit(&log);
                    }
                    None => eprintln!("jobs: no job with id={job_id:?}"),
                }
            }
            None => eprintln!("usage: anima-hosted jobs remove <job_id> [<reason>]"),
        },
        Some("run") => match args.get(1) {
            Some(job_id) => {
                if registry.get(job_id).is_none() {
                    eprintln!("jobs: no job with id={job_id:?}");
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
                }
                print_jobs_audit(&log);
            }
            None => eprintln!("usage: anima-hosted jobs run <job_id>"),
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
                Err(e) => eprintln!("error: {e}"),
            }
        }
        Some("register") => {
            let path = match args.get(1) {
                Some(p) => p,
                None => {
                    eprintln!("usage: skills register <path-to-SKILL.md>");
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
                        Err(e) => eprintln!("error: {e}"),
                    }
                }
                Err(e) => eprintln!("error reading {path}: {e}"),
            }
        }
        Some("promote") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills promote <id>");
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
                Err(e) => eprintln!("error: {e}"),
            }
        }
        Some("rollback") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills rollback <id>");
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
                Err(e) => eprintln!("error: {e}"),
            }
        }
        Some("quarantine") => {
            let id = match args.get(1) {
                Some(s) => s.to_lowercase().replace(' ', "-"),
                None => {
                    eprintln!("usage: skills quarantine <id> [reason]");
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
                Err(e) => eprintln!("error: {e}"),
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
                    Err(e) => eprintln!("skills: enqueue failed: {e}"),
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
            eprintln!(
                "usage: skills {{list|info|register|promote|rollback|quarantine|\
                 kill-switch|reflect|queue|approve <id>}}"
            );
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
                Err(e) => eprintln!("browse error: {e:?}"),
            }
        }
        Some("extract") => {
            let (url, selector) = match (args.get(1), args.get(2)) {
                (Some(u), Some(s)) => (u, s),
                _ => {
                    eprintln!("usage: tools extract <url> <selector>");
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
                Err(e) => eprintln!("extract error: {e:?}"),
            }
        }
        Some(sub) => {
            eprintln!("unknown tools subcommand: {sub:?}");
            eprintln!("usage: tools {{list|browse <url>|extract <url> <selector>}}");
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
                Err(e) => eprintln!("skills approve error: {e}"),
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
        Err(e) => eprintln!("snapshot: error saving to {path:?}: {e}"),
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
            None => eprintln!("replay: no decision found for event_id={id:?}"),
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
    if args.first().map(String::as_str) == Some("skills") {
        cmd_skills(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("tools") {
        cmd_tools(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("ask")
        || args.first().map(String::as_str) == Some("cortex")
    {
        cmd_ask(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("serve") {
        cmd_serve();
        return;
    }
    if args.first().map(String::as_str) == Some("digest") {
        cmd_digest(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("snapshot") {
        cmd_snapshot(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("replay") {
        cmd_replay(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("users") {
        cmd_users(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("jobs") {
        cmd_jobs(&args[1..]);
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
