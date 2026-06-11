//! E4.5b — the lifecycle director runs **in-kernel**.
//!
//! Until this phase, the microVM exercised vita's *building blocks*
//! (scheduler, memory, interoception) without the organism that sequences
//! them. Here the real [`vita::LifecycleManager`] + [`somatic_execution_loop`]
//! run on the Embassy executor against a no_std mock backend:
//!
//! 1. one guidance packet enters through the real [`SensoryBridge`]
//!    (policy-checked, priority-tagged) while the agent is asleep;
//! 2. the somatic loop wakes, the agenda dispatches the task through the
//!    MLFQ scheduler to the backend, and the response is audited;
//! 3. the agenda drains and the loop re-enters sleep, sequencing the
//!    maintenance phases (pruning runs against real L1 state; replay /
//!    dream / compilation report as hosted-only stubs in no_std);
//! 4. the audit log — the same `AuditEntry` stream the operator console
//!    tails on the hosted target — is asserted to contain the full
//!    wake → dispatch → sleep arc.
//!
//! On success the caller prints `E4.5B_VITA_DONE` to COM1 for the CI gate.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use memory::VirtualContextManager;
use scheduler::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};
use senses::{HumanGuidance, SensoryBridge, SensoryPriority};
use vita::{somatic_execution_loop, AuditEntry, LifecycleConfig, LifecycleManager};

/// Deterministic no_std backend: echoes the prompt back token-by-token.
///
/// Mirrors `scheduler::MockLlmBackend` (std-gated, so not linkable here);
/// kept minimal on purpose — the phase exercises the *lifecycle*, not
/// inference.
struct KernelEchoBackend;

impl LlmBackend for KernelEchoBackend {
    fn id(&self) -> &'static str {
        "kernel-echo"
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let mut emitted: Vec<StreamingCompletion> = Vec::new();
            for word in prompt.split_whitespace() {
                if cancel.is_cancelled() {
                    emitted.push(StreamingCompletion::Cancelled);
                    return Err(LlmBackendError::Cancelled);
                }
                emitted.push(StreamingCompletion::Token(format!("{word} ")));
            }
            emitted.push(StreamingCompletion::Done);
            Ok(emitted)
        })
    }
}

/// Drive a bounded somatic lifecycle: guidance in → wake → dispatch →
/// sleep cycle → audit assertions.
pub async fn run_vita_lifecycle_soak(serial: impl Fn(&str)) -> Result<(), &'static str> {
    serial("[E4.5b] constructing LifecycleManager (somatic loop, no_std)\n");

    // The same afferent seam the operator console uses on the hosted target.
    let bridge = SensoryBridge::new(HumanGuidance::new("kernel-operator"));
    bridge
        .packetize_text_checked(
            "summarise the boot telemetry for the operator",
            SensoryPriority::High,
        )
        .map_err(|_| "guidance rejected by policy bounds")?;

    let mut manager = LifecycleManager::new(
        "anima-kernel",
        bridge,
        VirtualContextManager::with_capacity(0, 2048),
        LifecycleConfig { max_context: 2048 },
        HumanGuidance::new("kernel-boot"),
        Arc::new(KernelEchoBackend),
        // Bounded run: enough iterations to ingest, dispatch, complete the
        // task, and re-enter sleep — then the loop returns.
        Some(48),
    );

    let mut monitor = interoception::HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0);

    serial("[E4.5b] somatic_execution_loop: awaiting bounded run\n");
    somatic_execution_loop(&mut manager, &monitor)
        .await
        .map_err(|_| "somatic execution loop returned an error")?;

    // ── Assert the full arc in the audit log ────────────────────────────
    let entries = manager.audit.entries();
    let mut woke = false;
    let mut completed_tasks = 0usize;
    let mut slept = 0usize;
    let mut phases_ok = 0usize;
    let mut response: Option<String> = None;

    for entry in entries {
        match entry {
            AuditEntry::WakeEntered { .. } => woke = true,
            AuditEntry::TaskCompleted { response: r, .. } => {
                completed_tasks += 1;
                response = Some(r.clone());
            }
            AuditEntry::SleepEntered { .. } => slept += 1,
            AuditEntry::SleepPhaseCompleted { success: true, .. } => phases_ok += 1,
            _ => {}
        }
    }

    serial(&format!(
        "[E4.5b] audit: {} entries, woke={}, tasks_completed={}, sleeps={}, phases_ok={}\n",
        entries.len(),
        woke,
        completed_tasks,
        slept,
        phases_ok
    ));
    if let Some(r) = &response {
        serial(&format!("[E4.5b] agent response: {r}\n"));
    }

    if !woke {
        return Err("agent never woke from guidance");
    }
    if completed_tasks == 0 {
        return Err("no task completed through the scheduler/backend path");
    }
    if slept == 0 {
        return Err("agent never re-entered sleep after the agenda drained");
    }
    if phases_ok < 4 {
        return Err("sleep maintenance did not sequence all four phases");
    }
    Ok(())
}
