# Soak record — 2026-06-11, sandboxed QEMU, 20 iterations

**This is a harness-proof record, not the E4.7 30-day criterion.**
20 consecutive QEMU/OVMF boots of the release EFI (vita lifecycle phase
included) driven by `cargo xtask soak --iterations 20` on a development
sandbox: 20/20 successful, 0 timeouts, 0 unscheduled exits,
mean boot 5.9 s / p95 8.8 s (QEMU+OVMF wall-clock, dominated by firmware
POST — not representative of Firecracker / Cloud Hypervisor latency).

The 720-hour production run on microVM hardware remains open (docs/22 §1);
its manifest will live alongside this one.
