//! Audit-trail rendering for the `anima-hosted` CLI (KERN-8/KERN-9).
//!
//! These `print_*_audit` renderers were extracted verbatim from `main.rs`,
//! which had grown past 7,500 lines. They format a `vita::AuditLog` (or a whole
//! `LifecycleManager`) as human-readable operator output for the various
//! subcommands. Grouping them here keeps the audit-formatting surface in one
//! place instead of scattered across the binary's command handlers.

use vita::{AuditEntry, AuditLog, LifecycleManager};

pub(crate) fn print_audit(manager: &LifecycleManager) {
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

/// Print E22-relevant audit entries from an in-process log.
pub(crate) fn print_session_audit(log: &AuditLog) {
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

/// Prints E23 audit entries to stdout (same style as the main `print_audit`).
pub(crate) fn print_data_audit(log: &AuditLog) {
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

pub(crate) fn print_audit_alert(log: &vita::AuditLog) {
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

/// Prints the E17-relevant entries from an in-process audit log.
pub(crate) fn print_user_audit(log: &AuditLog) {
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

/// Prints E31-relevant entries from an in-process audit log.
pub(crate) fn print_workspace_audit(log: &AuditLog) {
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

/// Prints E32 job-related audit entries to stdout.
pub(crate) fn print_jobs_audit(log: &AuditLog) {
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
