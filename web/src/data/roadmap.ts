export type Milestone = {
  id: string;
  name: string;
  weeks?: string;
  months?: string;
  description: string;
};

export type Phase = {
  id: string;
  number: number;
  name: string;
  months: string;
  focus: string;
  longFocus: string;
  status: 'partially complete' | 'in design' | 'planned';
  exitCriteria: string[];
  risks: { risk: string; mitigation: string }[];
  milestones: Milestone[];
  accent: string;
};

export const phases: Phase[] = [
  {
    id: 'phase-1',
    number: 1,
    name: 'Waking Hosted Core',
    months: 'Months 1–3',
    focus: 'Multiple agents executing tasks with fair token allocation.',
    longFocus:
      'Establish the core task execution architecture, state representation, and the hosted kernel target. At the end of this phase, Anima runs as an ordinary Linux process that demonstrates the core architectural patterns even though it does not yet do anything biologically interesting.',
    status: 'partially complete',
    accent: '#7df9c8',
    milestones: [
      { id: 'M1.1', name: 'Workspace skeleton', weeks: 'Wk 1–2', description: 'Cargo workspace, eight crates, CI pipeline with fmt/clippy/unsafe quarantine.' },
      { id: 'M1.2', name: 'Core abstractions', weeks: 'Wk 3–4', description: 'AgentPCB, SyscallEnum, TaskId, Priority, Capability typestate.' },
      { id: 'M1.3', name: 'Provider-agnostic LLM backend', weeks: 'Wk 5–6', description: 'LlmBackend trait, Anthropic + OpenAI implementations, mock backend.' },
      { id: 'M1.4', name: 'MLFQ scheduler', weeks: 'Wk 7–8', description: 'Three priority levels, boost-and-decay, iteration-aware continuous batching.' },
      { id: 'M1.5', name: 'Bounded token pipes', weeks: 'Wk 9–10', description: 'Crossbeam ring buffers with credit-based backpressure.' },
      { id: 'M1.6', name: 'First end-to-end run', weeks: 'Wk 11–12', description: 'Single agent task through senses → vita → scheduler → LlmBackend → response.' },
    ],
    exitCriteria: [
      'Workspace builds clean (fmt, clippy, unsafe quarantine).',
      'Unit test coverage targets met for corpus, vita, scheduler.',
      'Two concurrent agents with fair token-slice allocation, verified by integration test.',
      'End-to-end trace visible in audit log for each completed task.',
    ],
    risks: [
      { risk: 'LLM backend rate limits during development.', mitigation: 'Heavy use of the mock backend; real backends only for end-to-end verification.' },
      { risk: 'Scheduler complexity creep.', mitigation: 'Resist optimisation before there is workload data.' },
    ],
  },
  {
    id: 'phase-2',
    number: 2,
    name: 'Somatic Memory & Tool Bus',
    months: 'Months 4–6',
    focus: 'Dynamic tool routing without context pollution.',
    longFocus:
      'Build the memory hierarchy and the praxis subsystem. The agent can manage its own context across L1/L2/L3 and dynamically route tool calls through circuit breakers and sandboxes.',
    status: 'in design',
    accent: '#5dd1ff',
    milestones: [
      { id: 'M2.1', name: 'L1 block-structured tracking', weeks: 'Wk 13–14', description: 'PagedAttention-style block tracking, memory pressure events on the bus.' },
      { id: 'M2.2', name: 'L2 concurrent cache', weeks: 'Wk 15–16', description: 'scc::HashMap-backed L2, ARC eviction, promotion path to L1.' },
      { id: 'M2.3', name: 'Praxis tool driver framework', weeks: 'Wk 17–18', description: '/dev/anima/praxis/tools/ namespace, length-robust relative routing.' },
      { id: 'M2.4', name: 'Circuit breakers', weeks: 'Wk 19', description: 'Per-tool Closed/Open/HalfOpen states, exposed via interoception telemetry.' },
      { id: 'M2.5', name: 'Wasmtime sandbox integration', weeks: 'Wk 20–21', description: 'Gas metering, memory limits, capability-based imports.' },
      { id: 'M2.6', name: 'LanceDB L3 archive', weeks: 'Wk 22–24', description: 'Embedded LanceDB under /dev/anima/memory/l3, demotion/retrieval paths.' },
    ],
    exitCriteria: [
      'Memory tier transitions verified by integration test in both directions.',
      'Tool routing across 20+ registered tools without context pollution.',
      'Wasmtime sandbox bounds a misbehaving tool within gas/memory limits.',
      'L3 archive survives a process restart with consistent retrieval.',
    ],
    risks: [
      { risk: 'LanceDB integration friction under no_std-adjacent constraints.', mitigation: 'Keep LanceDB behind a small interface so it can be swapped.' },
      { risk: 'Wasmtime compilation cost.', mitigation: 'Lazy initialisation, single shared runtime, careful feature flags.' },
    ],
  },
  {
    id: 'phase-3',
    number: 3,
    name: 'Interoception & the Sleep Cycle',
    months: 'Months 7–12',
    focus: 'Clean wake/sleep transitions driven by stress.',
    longFocus:
      'Real-time feedback monitoring and the full sleep cycle. This phase makes Anima distinctively alive rather than merely well-architected.',
    status: 'planned',
    accent: '#c084fc',
    milestones: [
      { id: 'M3.1', name: 'Kernel trace hooks', months: 'M7', description: 'Latency tracking across hot paths, rolling-window TTFT.' },
      { id: 'M3.2', name: 'Stress index calculation', months: 'M7', description: 'HomeostaticMonitor at 1 Hz, threshold-driven events.' },
      { id: 'M3.3', name: 'Sensory bridge', months: 'M8', description: 'Unix socket text input, PCM voice pipeline through VAD + STT.' },
      { id: 'M3.4', name: 'Sleep state transitions', months: 'M9', description: 'Pruning → Replay → Dreaming → Compilation phase progression.' },
      { id: 'M3.5', name: 'Pruning phase', months: 'M9', description: 'Emotional decay, L1/L2 pruning, semantic floor enforcement.' },
      { id: 'M3.6', name: 'Replay validation', months: 'M10', description: 'Generative replay from audit stream, rollback on degradation.' },
      { id: 'M3.7', name: 'Dreaming phase', months: 'M11', description: 'Random graph walks across L3, associative edge candidates.' },
      { id: 'M3.8', name: 'Compilation phase', months: 'M12', description: 'Trace-to-training-pair compilation, persistence under training_corpus/.' },
    ],
    exitCriteria: [
      'Clean transitions between Waking and Sleeping based on stress and agenda.',
      'Each sleep phase runs to completion on 100 consecutive cycles without error.',
      'Generative replay rolls back at least one pruning change in soak test.',
      'Emergency consolidation triggers and recovers under deliberate stress.',
      'Audit log shows complete lifecycle history with no gaps.',
    ],
    risks: [
      { risk: 'Sleep cycle tuning under real workloads.', mitigation: 'Extensive soak testing with telemetry export, parameter sweeps in CI.' },
      { risk: 'Dreaming quality (random walks yield useless edges).', mitigation: 'Validation in the next pruning cycle filters bad candidates.' },
    ],
  },
  {
    id: 'phase-4',
    number: 4,
    name: 'Bare-Metal & Verification',
    months: 'Months 13–24',
    focus: 'MicroVM booting natively with all subsystems.',
    longFocus:
      'Port to bare-metal microVM, integrate smoltcp and rustls, complete the formal verification surface, and prepare for production deployment.',
    status: 'planned',
    accent: '#ffb454',
    milestones: [
      { id: 'M4.1', name: 'corpus no_std port', months: 'M13–14', description: 'no_std build, custom allocator, UEFI boot trampoline.' },
      { id: 'M4.2', name: 'Embassy runtime in corpus', months: 'M15', description: 'Async executor at the kernel level.' },
      { id: 'M4.3', name: 'smoltcp integration', months: 'M16–17', description: 'TCP/IP stack at boot, virtio-net driver, first TCP from microVM.' },
      { id: 'M4.4', name: 'rustls integration', months: 'M18', description: 'TLS over smoltcp, outbound TLS to an LLM provider API.' },
      { id: 'M4.5', name: 'Higher crates ported', months: 'M19–20', description: 'All crates running in microVM; hosted retained for dev.' },
      { id: 'M4.6', name: 'Formal verification rollout', months: 'M21–22', description: 'Kani proofs for scheduler invariants, ring buffer, rate limiters; miri on corpus.' },
      { id: 'M4.7', name: 'Production hardening', months: 'M23–24', description: 'Boot time < 1 s, 30-day soak test, perf regression benchmarks.' },
    ],
    exitCriteria: [
      'Anima boots as a microVM under Firecracker and Cloud Hypervisor in under 2 seconds.',
      'Full subsystem behaviour matches the hosted target.',
      'All Kani proofs pass; miri clean on corpus.',
      '30-day soak test completes without unscheduled restart.',
      'Documentation updated: microVM primary, hosted documented as dev-only.',
    ],
    risks: [
      { risk: 'Bare-metal driver work for smoltcp + virtio.', mitigation: 'Budget includes 2 months for the network stack alone.' },
      { risk: 'Formal verification scope creep.', mitigation: 'The verification doc lists what we prove; expansions require explicit scope approval.' },
      { risk: 'Performance regressions at the bare-metal boundary.', mitigation: 'Per-PR benchmark in Phase 4, regression alerts.' },
    ],
  },
];
