export type Crate = {
  name: string;
  pkg?: string;
  role: string;
  metaphor: string;
  mechanism: string;
  verification: 'TCB (audited unsafe)' | '#![forbid(unsafe_code)]';
  color: string;
  highlights: string[];
};

export const crates: Crate[] = [
  {
    name: 'corpus',
    role: 'Autonomic substrate',
    metaphor: 'The body — brainstem and spinal cord',
    mechanism: 'Frame allocator, page tables, boot trampoline, context switching, syscall enum.',
    verification: 'TCB (audited unsafe)',
    color: '#ff6b6b',
    highlights: [
      'FrameAllocator (bump-style, atomic, audited)',
      'AgentPcb, AgentPid, AgentState',
      'SyscallEnum for inter-crate calls',
    ],
  },
  {
    name: 'vita',
    role: 'Self-preservation plane',
    metaphor: 'Lifecycle director — the will to stay alive',
    mechanism: 'Autonomous state machine, sleep triggers, wake transitions, policy interpretation.',
    verification: '#![forbid(unsafe_code)]',
    color: '#7df9c8',
    highlights: [
      'somatic_execution_loop (waking / sleep transitions)',
      'Sleep phases: pruning, replay, dreaming, compilation',
      'Emergency consolidation under stress',
    ],
  },
  {
    name: 'scheduler',
    role: 'Reflex loop control',
    metaphor: 'Iteration-aware continuous batching',
    mechanism: 'Three-tier MLFQ, per-task token-slice tracking, credit-based backpressure.',
    verification: '#![forbid(unsafe_code)]',
    color: '#5dd1ff',
    highlights: [
      'Three-tier TaskAgenda (High / Medium / Low)',
      'IterationAwareMlfq::dispatch_task',
      'BoundedTokenPipe with credit backpressure',
      'LlmBackend trait + CancellationToken',
    ],
  },
  {
    name: 'memory',
    role: 'Synaptic memory layer',
    metaphor: 'Complementary Learning Systems hierarchy',
    mechanism: 'L1 working context, L2 ARC warm cache, L3 LanceDB archive, emotional decay.',
    verification: '#![forbid(unsafe_code)]',
    color: '#c084fc',
    highlights: [
      'VirtualContextManager (L1)',
      'ArcCache (L2 warm cache)',
      'ArchivalStore (L3 vector-similarity)',
      'Emotionally-modulated decay S(t)',
    ],
  },
  {
    name: 'praxis',
    role: 'Efferent actuator core',
    metaphor: 'Motor cortex — outbound action',
    mechanism: 'Length-robust relative routing, circuit breakers, MCP/A2A buses, wasmtime sandboxes.',
    verification: '#![forbid(unsafe_code)]',
    color: '#ffb454',
    highlights: [
      'length_robust_filter (τ_rel × max scoring)',
      'CircuitBreaker (Closed / Open / HalfOpen)',
      'ToolDriver trait + ToolEnvelope',
      'Wasmtime sandbox with gas metering',
    ],
  },
  {
    name: 'self',
    pkg: 'anima-self',
    role: 'Self / non-self barrier',
    metaphor: 'Immune system — capability tokens',
    mechanism: 'Typestate capability tokens, role-based issuance, elevation tokens.',
    verification: '#![forbid(unsafe_code)]',
    color: '#7df9c8',
    highlights: [
      'Capability<Unverified> → Capability<Verified>',
      'Role-bound issuance at build time',
      'Single-use elevation tokens',
    ],
  },
  {
    name: 'interoception',
    role: 'Interoceptive feedback',
    metaphor: 'Felt sense of internal state',
    mechanism: 'Rolling TTFT window, composite stress index, threshold-driven events.',
    verification: '#![forbid(unsafe_code)]',
    color: '#5dd1ff',
    highlights: [
      'HomeostaticMonitor::compute_systemic_stress_index',
      'Rolling TTFT window via record_ttft',
      'Stress index visible on telemetry at 1 Hz',
    ],
  },
  {
    name: 'senses',
    role: 'Afferent input vector',
    metaphor: 'Sensory bridge — text, voice, RPC',
    mechanism: 'PCM packetisation, text buffer parsing, sensory event envelopes, priority assignment.',
    verification: '#![forbid(unsafe_code)]',
    color: '#c084fc',
    highlights: [
      'HumanGuidance policy bounds',
      'Text / PCM packetisation via SensoryPacket',
      'Priority-tagged sensory events',
    ],
  },
  {
    name: 'defence',
    role: 'Adversarial defence layer',
    metaphor: 'Immune response to hostile input',
    mechanism: 'Input filtering, prompt-injection detection, output gating.',
    verification: '#![forbid(unsafe_code)]',
    color: '#ff6b6b',
    highlights: [
      'Pre-cortex input filter',
      'Post-cortex output gate',
      'Quarantine and rate-limiting',
    ],
  },
  {
    name: 'kv-controller',
    role: 'Learned KV-cache controller',
    metaphor: 'Selective forgetting at the attention layer',
    mechanism: 'Stateful gating policy for KV-cache eviction during long-horizon reasoning.',
    verification: '#![forbid(unsafe_code)]',
    color: '#ffb454',
    highlights: [
      'Learned eviction policy',
      'Episode-level compression',
      'Long-context retention without degradation',
    ],
  },
];
