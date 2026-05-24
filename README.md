# AnimaOS

AnimaOS is a bare-metal, cloud-isolated framekernel OS intended to act as the somatic architecture for an autonomous LLM agent process.

## Workspace Layout

```
anima-os/
├── Cargo.toml
├── crates/
│   ├── kernel-core/
│   ├── lifecycle/
│   ├── scheduler/
│   ├── memory/
│   ├── toolbus/
│   ├── security/
│   ├── observe/
│   └── sensory-bridge/
└── kernels/
    ├── hosted/
    └── microvm/
```

## Implemented Core Interfaces

- `observe::HomeostaticMonitor::compute_systemic_stress_index`
- `toolbus::CircuitBreaker::verify_pathway_health`
- `lifecycle::somatic_execution_loop`

The non-TCB crates explicitly enforce `#![forbid(unsafe_code)]`.
