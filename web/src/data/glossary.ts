export type Term = {
  term: string;
  meaning: string;
  location?: string;
  category: string;
};

export const glossary: Term[] = [
  // Anatomical
  { term: 'Anima', meaning: 'The whole system; the autonomous-agent OS as a single organism.', location: 'project', category: 'Anatomical' },
  { term: 'Corpus', meaning: 'The Trusted Computing Base: privileged kernel code with audited unsafe.', location: 'crates/corpus', category: 'Anatomical' },
  { term: 'Vita', meaning: 'The lifecycle director — wake/sleep state machine and policy interpretation.', location: 'crates/vita', category: 'Anatomical' },
  { term: 'Praxis', meaning: 'The efferent (output) subsystem: tool dispatch, MCP/A2A buses, sandboxes.', location: 'crates/praxis', category: 'Anatomical' },
  { term: 'Senses', meaning: 'The afferent (input) subsystem: stream parsers for text, voice, RPC.', location: 'crates/senses', category: 'Anatomical' },
  { term: 'Self', meaning: 'The capability and identity system; the immune system equivalent.', location: 'crates/self', category: 'Anatomical' },
  { term: 'Interoception', meaning: 'Real-time internal-state monitoring; the stress index.', location: 'crates/interoception', category: 'Anatomical' },
  { term: 'Memory', meaning: 'The three-tier context/cache/archive system.', location: 'crates/memory', category: 'Anatomical' },
  { term: 'Scheduler', meaning: 'The MLFQ task scheduler and bounded token pipe.', location: 'crates/scheduler', category: 'Anatomical' },

  // Lifecycle
  { term: 'Waking', meaning: 'Macro-state during which the agent dispatches tasks and responds to input.', category: 'Lifecycle' },
  { term: 'Sleeping', meaning: 'Macro-state during which the agent performs internal maintenance.', category: 'Lifecycle' },
  { term: 'Pruning', meaning: 'First sleep phase: applies emotional decay to L1/L2, evicts or compresses below-threshold entries.', category: 'Lifecycle' },
  { term: 'Replay', meaning: 'Second sleep phase: re-runs sampled past questions to validate that pruning has not degraded knowledge.', category: 'Lifecycle' },
  { term: 'Dreaming', meaning: 'Third sleep phase: random graph walks across L3 to discover new associative edges.', category: 'Lifecycle' },
  { term: 'Compilation', meaning: 'Fourth sleep phase: compiles waking-state traces into training data formats.', category: 'Lifecycle' },
  { term: 'Emergency consolidation', meaning: 'Stress-triggered rapid pruning during the Waking state; bypasses the full sleep cycle.', category: 'Lifecycle' },
  { term: 'Homeostatic loop', meaning: 'The continuous Waking-state loop integrating sensory input, stress monitoring, and task dispatch.', category: 'Lifecycle' },

  // Memory
  { term: 'L1 / Working Context', meaning: 'Tokens mapped into the model\'s active attention field.', category: 'Memory' },
  { term: 'L2 / Warm Memory Cache', meaning: 'RAM-resident concurrent hashmap of recent tokens and KV-cache blocks.', category: 'Memory' },
  { term: 'L3 / Cerebral Archival Store', meaning: 'Embedded LanceDB vector store; persistent across restarts.', category: 'Memory' },
  { term: 'Semantic floor', meaning: 'Minimum activation value (default 0.3) below which decay does not pull entries.', category: 'Memory' },
  { term: 'Arousal', meaning: 'Emotional weighting scalar [0,1] assigned at memory formation; modulates decay rate.', category: 'Memory' },
  { term: 'Surprise', meaning: 'Emotional weighting scalar [0,1] assigned at memory formation; weighted more heavily than arousal by default.', category: 'Memory' },
  { term: 'Associative edge', meaning: 'A connection between two L3 entries discovered during Dreaming and validated during Pruning.', category: 'Memory' },
  { term: 'Audit stream', meaning: 'Dedicated L3 namespace containing capability operations and lifecycle events; emotional weighting prevents decay.', category: 'Memory' },

  // Praxis
  { term: 'Tool driver', meaning: 'A handler for a single tool, exposed as a file under /dev/anima/praxis/tools/.', category: 'Praxis' },
  { term: 'Circuit breaker', meaning: 'Per-tool state monitor that blocks invocation after repeated failures.', category: 'Praxis' },
  { term: 'Length-robust relative routing', meaning: 'Filter that admits tools by relative score (τ_rel × max) rather than absolute threshold.', category: 'Praxis' },
  { term: 'MCP', meaning: 'Model Context Protocol — exposed as remote tools under /dev/anima/praxis/tools/mcp/<server>/.', category: 'Praxis' },
  { term: 'A2A', meaning: 'Agent-to-Agent protocol — peer agents exposed as remote tools under /dev/anima/praxis/tools/a2a/<peer>/.', category: 'Praxis' },
  { term: 'Sandbox', meaning: 'A wasmtime instance with gas metering, memory bounds, and capability-typed imports.', category: 'Praxis' },

  // Interoception
  { term: 'Stress index', meaning: 'Composite scalar [0,1] combining latency degradation and context saturation.', category: 'Interoception' },
  { term: 'TTFT', meaning: 'Time to First Token; the primary latency signal.', category: 'Interoception' },
  { term: 'Baseline TTFT', meaning: 'The reference latency value used to compute the latency ratio.', category: 'Interoception' },
  { term: 'β (beta)', meaning: 'Weighting parameter balancing latency pressure against memory pressure in the stress index.', category: 'Interoception' },
  { term: 'Telemetry stream', meaning: 'Continuous output of system metrics, primarily consumed by vita for policy decisions.', category: 'Interoception' },

  // Self
  { term: 'Capability', meaning: 'A typestate-pattern Rust value granting a specific permission.', category: 'Self' },
  { term: 'Role', meaning: 'A build-time-fixed identity (e.g., consolidator, responder) that determines which capabilities are issued.', category: 'Self' },
  { term: 'Elevation token', meaning: 'A single-use value that upgrades a restricted capability to an unrestricted one.', category: 'Self' },
  { term: 'Self/non-self barrier', meaning: 'The capability system as a whole; prevents tasks from acting outside their granted permissions.', category: 'Self' },

  // Sensory
  { term: 'Afferent', meaning: 'Input direction: from the environment into the agent.', category: 'Sensory' },
  { term: 'Efferent', meaning: 'Output direction: from the agent into the environment.', category: 'Sensory' },
  { term: 'Sensory event', meaning: 'A parsed input wrapped in a common envelope with source, timestamp, priority, and payload.', category: 'Sensory' },
  { term: 'Sensory node', meaning: 'A mount point under /dev/anima/senses/ corresponding to one input source.', category: 'Sensory' },
  { term: 'Priority', meaning: 'A driver-level tag determining how aggressively the agent should attend to an event.', category: 'Sensory' },
];

export const categories = [
  'Anatomical',
  'Lifecycle',
  'Memory',
  'Praxis',
  'Interoception',
  'Self',
  'Sensory',
] as const;
