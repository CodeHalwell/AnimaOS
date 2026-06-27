# 23 — Production Readiness Tracker

> **Status:** Living tracker. The four pillars of "production grade":
> a fully functional Docker MVP, a bare-metal-ready kernel, an operator UI
> worth living in, and a self-extending / self-tuning agent. Each section
> lists what is shipped (with evidence) and exactly what remains. Sibling
> trackers: `docs/22` (hardware-gated + software tails).

## Pillar 1 — Docker MVP

**Shipped.**
- Zero-dependency MVP: `docker compose -f docker-compose.mock.yml up --build`
  runs the full organism (lifecycle, sleep cycle, gate/router, console)
  against the deterministic mock backend on any Docker host.
- CI enforcement: every image build runs the container and asserts the full
  afferent→efferent round-trip (`docker.yml` smoke test).
- Operational posture: image `HEALTHCHECK` (verified healthy live),
  `restart: unless-stopped` on all hosted variants, named-volume state with
  correct uid-1000 ownership (regression reproduced + fixed), `.env.example`
  for the whole env surface, GHCR pull-without-build quickstart.
- Real-inference stacks defined for NVIDIA (`docker-compose.yml`), CPU
  (`+ docker-compose.cpu.yml`), and Apple Silicon (`docker-compose.apple.yml`).

**Remaining.**
- [ ] Validate the Ollama stacks end-to-end on real hosts (GPU + CPU): model
      pulls, tier routing, sustained sessions. (Hermetic CI cannot do this.)
      *Progress:* the live OpenAI-compatible HTTP path — request construction
      with tool schemas, response/tool_calls parsing, and the full
      plan→tool→observe→answer loop — has been exercised against a real local
      HTTP server (`ANIMA_COMPAT_LIVE=1`), so what remains on a real host is
      Ollama-specific behaviour, not the wire plumbing.
- [~] Exposed-deployment story: the non-empty `ANIMA_CONSOLE_TOKEN` requirement
      outside loopback is now **enforced** — `ConsoleServer::bind` refuses
      (`PermissionDenied`) to bind any non-loopback address (including the
      `0.0.0.0` / `::` wildcards) without a token, and `anima-hosted serve`
      surfaces the reason instead of starting unauthenticated; `.env.example`
      documents it. Still open: reverse-proxy/TLS deployment guidance doc.
- [ ] Publish a versioned release image (`:v*`) once the above is validated.

## Pillar 2 — Bare-metal ready

**Shipped.** UEFI framekernel boots under QEMU/OVMF in CI with the full
marker sequence (Embassy, smoltcp, TLS 1.3, sleep-cycle soak, console
Phase 0); ≤ 1 MiB release image enforced (current release EFI ≈ 200 KiB);
Kani + Miri nightly; soak harness + manifest schema in-tree. The boot is
also reproducible outside CI: nightly + `x86_64-unknown-uefi` +
QEMU/OVMF brings the same marker sequence up green on a stock dev box,
so kernel work (the tails below) does not depend on CI round-trips.

**Remaining** (tracked in detail in `docs/22`):
- [x] **`vita` in the kernel** (`docs/22` §1a) — DONE: the lifecycle
      director runs in-kernel (E4.5b phase: guidance → MLFQ dispatch →
      audited four-phase sleep), CI-gated via `E4.5B_VITA_DONE`. The
      bare-metal target is now an organism, not a substrate.
- [x] **virtio-net driver** — DONE (PCI/modern via `virtio-drivers`, ECAM
      scan, identity-DMA Hal, smoltcp glue). The console protocol runs over
      real TCP, CI-gated end-to-end against a host listener
      (`E6.5_NET_DONE`/`E6.5_GUIDANCE_OK`). Follow-ups in docs/22: ACPI
      MCFG parse, Firecracker virtio-mmio flip, TLS on the socket.
- [ ] **30-day soak** on Firecracker / Cloud Hypervisor; commit the manifest
      under `artifacts/soak/`; assert the 2 s boot budget there. (Harness
      proof banked: 20/20 sandbox QEMU iterations committed — wall-clock
      time is the only remaining ingredient.)

## Pillar 3 — Operator UI

**Shipped.** Chat-first operator console (zero-dependency, served by the
embedded HTTP server): conversation bubbles for every guidance channel with
typing indicators and inline gate/veto notices; lifecycle chip + sleep-phase
stepper; six vitals bars + aggregate-stress ring; Striatal-Gate card (value
vs threshold, rationale); filterable telemetry feed. Plus the `anima-console`
TUI and COM1 serial bridge for the kernel.

**Remaining.**
- [x] Approval-queue surface: `GET /approval-queue` lists all proposals as JSON;
      `POST /approval-queue/{id}/approve` and `POST /approval-queue/{id}/reject`
      let the operator act; the dashboard polls the endpoint and renders a live
      approval-queue panel with per-proposal Approve / Reject buttons. Wire
      `Console::with_approval_queue(Arc<Mutex<ApprovalQueue>>)` from the hosted
      serve path to activate. (`crates/console/src/server.rs` + `dashboard.html`;
      9 unit tests covering 404-when-not-wired, listing, approve, reject, 422 on
      unknown id, skills listing, adapters listing)
- [x] Skills/tools registry view: `GET /skills` returns all `SkillEntry` values
      as JSON; `GET /adapters` returns all `AdapterArtifact` values. The
      dashboard polls these endpoints and renders a Skills panel (coloured state
      dots: active/proposed/quarantined/rolled-back) and hides both panels when
      the endpoints are not wired. Wire `Console::with_skill_registry` /
      `Console::with_adapter_library` to activate.
- [x] "While you were away" digest (E15 S15.1) rendered on connect.
      `GET /digest` endpoint reads the audit JSONL, calls `lifecycle::generate_digest`,
      returns JSON; the browser dashboard fetches it on connect and shows a
      dismissible summary panel (suppressed when the agent has no recorded history).
- [ ] Auth beyond the bearer token for non-loopback deployments (per-user
      identity exists in `crates/users` / E17; wire it to the console).

## Pillar 4 — Self-extending, self-tuning agent

**Shipped.**
- Skills & tools self-extension machinery (E11): manifests, proposal
  evaluation, promotion gates, WASM sandbox, defence + constitution checks.
- The fine-tuning data loop, end-to-end in software: the serving agent
  persists its sleep-phase corpus (`ANIMA_CORPUS_DIR`, in the shared
  volume) → `trainer/sleep_phase.py` consumes all three formats and runs
  QLoRA (Unsloth) with GGUF + Ollama Modelfile export; `--dry-run`
  (corpus validation + provenance manifest) verified against a live-agent
  corpus. Manifest mirrors `finetune::AdapterArtifact` for library ingestion.

**Remaining.**
- [ ] Execute the live QLoRA path on a CUDA host; fix what reality finds.
- [ ] Point `finetune::UnslothFineTuner`'s `live` gate at the same flow and
      ingest the manifest into the adapter library (E8 S8.4.8), giving the
      Rust side custody of provenance + eval.
- [x] Eval gate before adoption — DONE: `anima_finetune::decide_adoption`
      fuses the E8 eval harness (S8.4.7 adoption rule + S8.4.6 merge-fidelity
      floor) with the E13 alignment outcome into an `AdoptionDecision`. The
      library now distinguishes *registered* from *adopted*:
      `AdapterLibrary::mount_gated` refuses any adapter that has not cleared the
      gate (`MountError::NotAdopted`), and re-training or eviction revokes the
      clearance. Covered by unit tests + a `train → register → evaluate →
      decide → mount_gated` integration test. The cleared decision is routed
      to the operator approval queue via
      `lifecycle::adapter_adoption_to_proposal` (a `WeightUpdate` proposal,
      symmetric to the E11 skill/tool bridge). Still open: rendering the queue
      in the console UI (Pillar 3).
- [x] Schedule: let the agent propose a fine-tune run (E32 jobs engine) when
      the corpus crosses a size threshold — DONE, closing the autonomy loop.
      `jobs::FineTuneTrigger` is the policy (corpus-pair threshold + cooldown);
      `evaluate()` emits a one-shot `ScheduledJob` carrying a
      `FineTuneProposalPayload` (base model, pair count, reason). The hosted
      `anima jobs propose-finetune` command wires it: it counts the live corpus
      (`alpaca.jsonl`), anchors the cooldown on the most recent proposal already
      in the registry, and enqueues the job when the threshold is crossed —
      operator-gated downstream via the E8 adoption gate + E15 approval queue, so
      it *proposes*, it does not silently adopt. Schedule the command (a cron
      `jobs` entry or external scheduler) to make it autonomous. Verified
      end-to-end (propose → cooldown → below-threshold) plus unit tests.

## Definition of done

Production grade means: a release-tagged image anyone can run in one command
(mock) or three (real inference); the microVM boots a full organism and has
a committed 30-day soak; the console is the daily surface for conversing
with, supervising, and approving the agent; and the agent demonstrably
improves itself — new skills and a mounted adapter — without ever bypassing
the gate, the constitution, or the operator's approval queue.
