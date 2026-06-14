/**
 * consoleStream — a tiny browser client for the AnimaOS operator console's
 * live Server-Sent Events (SSE) feed.
 *
 * ── Where this contract comes from ──────────────────────────────────────────
 * The live console is a hand-rolled, zero-dependency HTTP/SSE server in the
 * Rust crate `crates/console`. This module mirrors exactly what that server
 * emits — no invented fields.
 *
 *   • Endpoint:        `GET /events`   (text/event-stream)
 *                      — see `crates/console/src/server.rs::serve_events`
 *   • Default origin:  `http://127.0.0.1:8088`
 *                      — `ServerConfig::default()` / `from_env()` in server.rs
 *   • Auth (optional): a bearer token may be required; EventSource cannot set
 *                      headers, so it is passed as the `?token=` query param —
 *                      `ConsoleServer::authorised` in server.rs.
 *   • Frame shape:     each event is one `data: <ndjson>` line; the NDJSON is an
 *                      internally-tagged object `{"type": "...", ...}` —
 *                      `write_sse` in server.rs + `serde(tag = "type")` on
 *                      `OperatorEvent` in `crates/console-proto/src/lib.rs`.
 *
 * ── The event variants (from `OperatorEvent` in console-proto/src/lib.rs) ────
 * Every variant carries `type`. Numeric vitals/gate fields are f32 in `[0,1]`.
 *
 *   type "Vitals"       thermal_load, compute_pressure, memory_pressure,
 *                       power_budget, financial_budget, attention_demand,
 *                       aggregate_stress                              (all f32)
 *   type "State"        lifecycle: string ("Awake"|"Sleep"),
 *                       sleep_phase: string|null, agenda_depth: u32
 *   type "Gate"         invoke: bool, cost_class: string|null,
 *                       value_score: f32, threshold: f32,
 *                       override_active: bool, reasoning: string
 *   type "Audit"        kind: string (AuditEntry variant name),
 *                       detail: string
 *   type "TaskStarted"  task_id: u64, prompt: string
 *   type "AgentMessage" task_id: u64, tokens: u32, text: string
 *   type "Heartbeat"    uptime_secs: u64
 *
 * Notes mirrored from `crates/console/src/dashboard.html`:
 *   • `task_id` is a u64; values above 2^53 lose precision through JSON.parse,
 *     so we recover the exact digits from the raw frame as `task_key`.
 *   • The feed CSS classes (gate/task/msg/audit/veto) map the same way the
 *     dashboard maps them; veto = Audit events whose `kind` is one of
 *     DefenceVeto / AttentionDemandEscalated / TaskFailed / CortexFault.
 *
 * The base URL is configurable via the `baseUrl` option or, when omitted, the
 * `PUBLIC_ANIMA_CONSOLE_URL` build-time env var, defaulting to the loopback
 * address the container publishes. If no server is reachable, `onError` fires
 * and callers simply keep rendering their static fallback data.
 */

// ── Wire types (1:1 with console-proto OperatorEvent) ────────────────────────

export interface VitalsEvent {
  type: 'Vitals';
  thermal_load: number;
  compute_pressure: number;
  memory_pressure: number;
  power_budget: number;
  financial_budget: number;
  attention_demand: number;
  aggregate_stress: number;
}

export interface StateEvent {
  type: 'State';
  lifecycle: string;
  sleep_phase: string | null;
  agenda_depth: number;
}

export interface GateEvent {
  type: 'Gate';
  invoke: boolean;
  cost_class: string | null;
  value_score: number;
  threshold: number;
  override_active: boolean;
  reasoning: string;
}

export interface AuditEvent {
  type: 'Audit';
  kind: string;
  detail: string;
}

export interface TaskStartedEvent {
  type: 'TaskStarted';
  task_id: number;
  prompt: string;
  /** Exact u64 digits recovered from the raw frame (precision-safe key). */
  task_key?: string;
}

export interface AgentMessageEvent {
  type: 'AgentMessage';
  task_id: number;
  tokens: number;
  text: string;
  /** Exact u64 digits recovered from the raw frame (precision-safe key). */
  task_key?: string;
}

export interface HeartbeatEvent {
  type: 'Heartbeat';
  uptime_secs: number;
}

export type OperatorEvent =
  | VitalsEvent
  | StateEvent
  | GateEvent
  | AuditEvent
  | TaskStartedEvent
  | AgentMessageEvent
  | HeartbeatEvent;

// ── Derived UI shapes (what the React islands consume) ───────────────────────

/** A single vital-sign row, matching the dashboard's vitals ordering/labels. */
export interface VitalRow {
  /** Wire key, e.g. `thermal_load`. */
  key: string;
  /** Short label, e.g. `thermal`. */
  label: string;
  /** Current value in `[0, 1]`. */
  v: number;
}

/** A telemetry feed line, matching the `.cm-ev` / `.ev` CSS classes. */
export interface FeedRow {
  /** CSS class: gate | task | msg | audit | veto. */
  cls: 'gate' | 'task' | 'msg' | 'audit' | 'veto';
  /** Short tag rendered in the `.cm-tag` span. */
  tag: string;
  /** The line body. */
  body: string;
}

/** The whole live snapshot the operator-console island renders. */
export interface ConsoleSnapshot {
  /** Vitals rows in the canonical order; null until first Vitals event. */
  vitals: VitalRow[] | null;
  /** Aggregate stress in `[0, 1]`; null until first Vitals event. */
  aggregateStress: number | null;
  /** Lifecycle state, e.g. `Awake`; null until first State event. */
  lifecycle: string | null;
  /** Current sleep phase, when sleeping. */
  sleepPhase: string | null;
  /** Agenda depth; null until first State event. */
  agendaDepth: number | null;
  /** Most-recent-first telemetry feed. */
  feed: FeedRow[];
}

/** Canonical vitals order + labels (from dashboard.html `VITALS`). */
export const VITAL_LABELS: ReadonlyArray<readonly [string, string]> = [
  ['thermal_load', 'thermal'],
  ['compute_pressure', 'compute'],
  ['memory_pressure', 'memory'],
  ['power_budget', 'power'],
  ['financial_budget', 'budget'],
  ['attention_demand', 'attention'],
];

/** Audit `kind`s the dashboard surfaces as vetoes rather than plain audits. */
const VETO_KINDS = new Set([
  'DefenceVeto',
  'AttentionDemandEscalated',
  'TaskFailed',
  'CortexFault',
]);

const DEFAULT_BASE_URL = 'http://127.0.0.1:8088';

/** Resolve the console base URL: explicit arg → env → loopback default. */
export function resolveBaseUrl(explicit?: string): string {
  if (explicit && explicit.trim()) return explicit.trim();
  const fromEnv = import.meta.env.PUBLIC_ANIMA_CONSOLE_URL;
  if (fromEnv && fromEnv.trim()) return fromEnv.trim();
  return DEFAULT_BASE_URL;
}

const MAX_FEED = 200;

export interface SubscribeOptions {
  /** Console origin, e.g. `http://127.0.0.1:8088`. Defaults via env. */
  baseUrl?: string;
  /** Optional bearer token (passed as `?token=` since EventSource has no headers). */
  token?: string;
  /** Called whenever the rolled-up snapshot changes. */
  onSnapshot: (snapshot: ConsoleSnapshot) => void;
  /** Called once the stream is live (EventSource `open`). */
  onOpen?: () => void;
  /** Called on connection error — caller should stay on fallback data. */
  onError?: () => void;
}

/** Handle returned by {@link subscribeConsole}; call to tear the stream down. */
export interface Subscription {
  close: () => void;
}

/**
 * Fold a single wire event into the running snapshot. Returns the next
 * snapshot (a new object when anything changed, else the same reference).
 */
export function reduceEvent(prev: ConsoleSnapshot, ev: OperatorEvent): ConsoleSnapshot {
  switch (ev.type) {
    case 'Vitals': {
      const vitals: VitalRow[] = VITAL_LABELS.map(([key, label]) => ({
        key,
        label,
        v: clamp01(numberOr(ev[key as keyof VitalsEvent] as number, 0)),
      }));
      return {
        ...prev,
        vitals,
        aggregateStress: clamp01(numberOr(ev.aggregate_stress, 0)),
      };
    }
    case 'State':
      return {
        ...prev,
        lifecycle: ev.lifecycle,
        sleepPhase: ev.sleep_phase ?? null,
        agendaDepth: ev.agenda_depth,
      };
    case 'Gate': {
      const body =
        `${ev.invoke ? 'INVOKE' : 'block'} ${ev.cost_class ?? ''}` +
        ` · value ${fixed(ev.value_score)} vs ${fixed(ev.threshold)}` +
        (ev.override_active ? ' · OVERRIDE' : '') +
        (ev.reasoning ? ` — ${ev.reasoning}` : '');
      return pushFeed(prev, { cls: 'gate', tag: 'gate', body });
    }
    case 'TaskStarted':
      return pushFeed(prev, {
        cls: 'task',
        tag: 'task→',
        body: `#${ev.task_key ?? ev.task_id} ${ev.prompt}`,
      });
    case 'AgentMessage':
      return pushFeed(prev, {
        cls: 'msg',
        tag: 'agent',
        body: `#${ev.task_key ?? ev.task_id} (${ev.tokens} tok) ${ev.text}`,
      });
    case 'Audit':
      return pushFeed(prev, {
        cls: VETO_KINDS.has(ev.kind) ? 'veto' : 'audit',
        tag: ev.kind,
        body: ev.detail,
      });
    case 'Heartbeat':
    default:
      return prev; // heartbeats carry no snapshot state
  }
}

/** The empty starting snapshot (all-null until live data arrives). */
export function emptySnapshot(): ConsoleSnapshot {
  return {
    vitals: null,
    aggregateStress: null,
    lifecycle: null,
    sleepPhase: null,
    agendaDepth: null,
    feed: [],
  };
}

/**
 * Open an SSE connection to the console and stream rolled-up snapshots.
 *
 * Errors (no server, refused, dropped) surface via `onError`; the connection
 * is left to EventSource's own auto-reconnect. If `EventSource` is unavailable
 * (SSR / very old browser) this is a no-op that immediately reports an error,
 * so callers fall back cleanly.
 */
export function subscribeConsole(opts: SubscribeOptions): Subscription {
  if (typeof EventSource === 'undefined') {
    // SSR or unsupported environment — never throws; caller keeps fallback.
    queueMicrotask(() => opts.onError?.());
    return { close: () => {} };
  }

  const base = resolveBaseUrl(opts.baseUrl).replace(/\/+$/, '');
  const url =
    `${base}/events` + (opts.token ? `?token=${encodeURIComponent(opts.token)}` : '');

  let snapshot = emptySnapshot();
  let closed = false;
  let es: EventSource;

  try {
    es = new EventSource(url);
  } catch {
    queueMicrotask(() => opts.onError?.());
    return { close: () => {} };
  }

  es.onopen = () => {
    if (!closed) opts.onOpen?.();
  };
  es.onerror = () => {
    if (!closed) opts.onError?.();
  };
  es.onmessage = (m: MessageEvent<string>) => {
    if (closed) return;
    const ev = parseFrame(m.data);
    if (!ev) return; // tolerate partial / unknown lines
    const next = reduceEvent(snapshot, ev);
    // reduceEvent returns the same reference when nothing changed (Heartbeat /
    // unknown frames); skip the callback so heartbeats don't trigger needless
    // React state updates / rerenders.
    if (next === snapshot) return;
    snapshot = next;
    opts.onSnapshot(snapshot);
  };

  return {
    close: () => {
      closed = true;
      es.close();
    },
  };
}

/**
 * Parse one SSE `data:` payload into a typed event, recovering the exact u64
 * `task_id` digits as `task_key` (JSON.parse loses precision above 2^53),
 * mirroring `dashboard.html`. Returns null on malformed lines.
 */
export function parseFrame(data: string): OperatorEvent | null {
  let ev: unknown;
  try {
    ev = JSON.parse(data);
  } catch {
    return null;
  }
  if (!ev || typeof ev !== 'object' || typeof (ev as { type?: unknown }).type !== 'string') {
    return null;
  }
  const raw = /"task_id":\s*(\d+)/.exec(data);
  if (raw) (ev as { task_key?: string }).task_key = raw[1];
  return ev as OperatorEvent;
}

// ── small helpers ────────────────────────────────────────────────────────────

function pushFeed(prev: ConsoleSnapshot, row: FeedRow): ConsoleSnapshot {
  const feed = [row, ...prev.feed].slice(0, MAX_FEED);
  return { ...prev, feed };
}

function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

function numberOr(v: unknown, fallback: number): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}

function fixed(v: number): string {
  return numberOr(v, 0).toFixed(2);
}
