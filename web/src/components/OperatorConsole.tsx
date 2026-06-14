import { useEffect, useState } from 'react';
import {
  subscribeConsole,
  type ConsoleSnapshot,
  type FeedRow,
  type VitalRow,
} from '../lib/consoleStream';

/**
 * OperatorConsole — a React island that renders the operator-console dashboard
 * mock (vital signs + telemetry feed) from a LIVE Server-Sent Events feed when a
 * running console (`crates/console`) is reachable, and from the page's static
 * seed data otherwise.
 *
 * It connects via {@link subscribeConsole}, which talks to `GET /events` on the
 * console crate's HTTP server (see consoleStream.ts for the full wire contract).
 * Any connection error simply leaves the seed data on screen, so the static
 * `astro build` renders the exact same documentation snapshot it always has.
 *
 * The `.cm-*` styles live in this component (the established island pattern,
 * cf. StressGauge / RoadmapTimeline) so the markup is styled whether it is the
 * live or the offline fallback render, with no reliance on Astro's scoped CSS.
 */

/** A static vital row, as seeded from operator-console.astro's `vitals` array. */
export interface SeedVital {
  label: string;
  v: number;
}

/** A static feed row, as seeded from operator-console.astro's `feed` array. */
export interface SeedFeed {
  cls: 'gate' | 'task' | 'msg' | 'audit' | 'veto';
  tag: string;
  body: string;
}

interface OperatorConsoleProps {
  /** Fallback vital rows rendered offline / before the first live Vitals event. */
  vitals: SeedVital[];
  /** Fallback feed rows rendered offline / before the first live event. */
  feed: SeedFeed[];
  /** Stress shown in the panel header when no live aggregate is available. */
  seedStress?: number;
  /** Console origin override; otherwise PUBLIC_ANIMA_CONSOLE_URL / loopback. */
  baseUrl?: string;
  /** Optional bearer token, passed as `?token=` (EventSource has no headers). */
  token?: string;
}

function pct(v: number): number {
  return Math.round(v * 100);
}

function barColor(v: number): string {
  return v > 0.85 ? '#ff6b6b' : v > 0.6 ? '#ffcf5c' : '#5cc8ff';
}

type Conn = 'offline' | 'connecting' | 'live';

export default function OperatorConsole({
  vitals,
  feed,
  seedStress = 0.18,
  baseUrl,
  token,
}: OperatorConsoleProps) {
  const [snapshot, setSnapshot] = useState<ConsoleSnapshot | null>(null);
  const [conn, setConn] = useState<Conn>('offline');

  useEffect(() => {
    setConn('connecting');
    const sub = subscribeConsole({
      baseUrl,
      token,
      onOpen: () => setConn('live'),
      onSnapshot: (snap) => {
        setConn('live');
        setSnapshot(snap);
      },
      // Stay on seed data; EventSource keeps trying to reconnect underneath.
      onError: () => setConn('offline'),
    });
    return () => sub.close();
  }, [baseUrl, token]);

  // Prefer live data when present, otherwise fall back to the static seed.
  const liveVitals: VitalRow[] | null = snapshot?.vitals ?? null;
  const shownVitals: SeedVital[] = liveVitals
    ? liveVitals.map((r) => ({ label: r.label, v: r.v }))
    : vitals;

  const liveFeed: FeedRow[] | null =
    snapshot && snapshot.feed.length > 0 ? snapshot.feed : null;
  const shownFeed: SeedFeed[] = liveFeed
    ? liveFeed.map((r) => ({ cls: r.cls, tag: r.tag, body: r.body }))
    : feed;

  const stress = snapshot?.aggregateStress ?? seedStress;
  const lifecycle = snapshot?.lifecycle ?? 'Awake';
  const sleepPhase = snapshot?.sleepPhase ?? '—';
  const agendaDepth = snapshot?.agendaDepth ?? 1;
  const isLive = conn === 'live';

  const connLabel =
    conn === 'live' ? '● live' : conn === 'connecting' ? '○ connecting…' : '● sample data';
  const connColor =
    conn === 'live' ? '#54d685' : conn === 'connecting' ? '#ffcf5c' : '#8a93a6';

  return (
    <div className="console-mock">
      <div className="cm-head">
        <span className="cm-brand">
          ANIMA<span style={{ color: '#5cc8ff' }}>OS</span>
        </span>
        <span className="cm-sub">operator console</span>
        <span
          className="cm-conn"
          style={{ color: connColor }}
          title={
            isLive
              ? 'streaming live telemetry'
              : 'no console server reachable — showing sample data'
          }
        >
          {connLabel}
        </span>
      </div>
      <div className="cm-body">
        <div className="cm-left">
          <div className="cm-panel">
            <h4>
              Vital signs{' '}
              <span style={{ float: 'right', color: '#e6e9ef' }}>stress {stress.toFixed(2)}</span>
            </h4>
            {shownVitals.map((row) => (
              <div className="cm-vital" key={row.label}>
                <div className="cm-row">
                  <span>{row.label}</span>
                  <span>{row.v.toFixed(2)}</span>
                </div>
                <div className="cm-bar">
                  <span style={{ width: `${pct(row.v)}%`, background: barColor(row.v) }} />
                </div>
              </div>
            ))}
          </div>
          <div className="cm-panel">
            <h4>Lifecycle</h4>
            <div className="cm-sl">
              <span>state</span>
              <b>{lifecycle}</b>
            </div>
            <div className="cm-sl">
              <span>sleep phase</span>
              <b>{sleepPhase}</b>
            </div>
            <div className="cm-sl">
              <span>agenda depth</span>
              <b>{agendaDepth}</b>
            </div>
            <div className="cm-sl">
              <span>source</span>
              <b>{isLive ? 'live SSE' : 'static snapshot'}</b>
            </div>
          </div>
        </div>
        <div className="cm-right cm-panel">
          <h4>Event stream</h4>
          <div className="cm-feed">
            {shownFeed.map((e, i) => (
              <div className={`cm-ev ${e.cls}`} key={`${e.tag}-${i}`}>
                <span className="cm-tag">{e.tag}</span>
                {e.body}
              </div>
            ))}
          </div>
          <div className="cm-input">
            <span className="cm-prompt mono">send guidance ▸</span>
            <span className="cm-caret">summarise the overnight logs</span>
            <span className="cm-pill mono">High</span>
          </div>
        </div>
      </div>

      <style>{`
        .console-mock {
          border: 1px solid var(--border); border-radius: 12px; overflow: hidden;
          background: #0b0e14; font-family: var(--mono); font-size: 0.8rem; color: #e6e9ef;
        }
        .cm-head {
          display: flex; align-items: center; gap: 0.75rem;
          padding: 0.7rem 1rem; border-bottom: 1px solid #232a3a;
        }
        .cm-brand { font-weight: 700; letter-spacing: 0.04em; }
        .cm-sub { color: #8a93a6; font-size: 0.72rem; }
        .cm-conn { margin-left: auto; font-size: 0.72rem; }
        .cm-body { display: grid; grid-template-columns: 280px 1fr; gap: 1rem; padding: 1rem; }
        @media (max-width: 760px) { .cm-body { grid-template-columns: 1fr; } }
        .cm-panel {
          background: #141925; border: 1px solid #232a3a; border-radius: 10px; padding: 0.9rem 1rem;
        }
        .cm-left { display: flex; flex-direction: column; gap: 1rem; }
        .cm-panel h4 {
          font-size: 0.65rem; text-transform: uppercase; letter-spacing: 0.12em;
          color: #8a93a6; margin: 0 0 0.75rem;
        }
        .cm-vital { margin-bottom: 0.55rem; }
        .cm-row { display: flex; justify-content: space-between; font-size: 0.72rem; }
        .cm-bar { height: 6px; background: #0a0d13; border-radius: 4px; overflow: hidden; margin-top: 3px; }
        .cm-bar > span { display: block; height: 100%; transition: width 0.5s ease, background 0.5s ease; }
        .cm-sl { display: flex; justify-content: space-between; padding: 0.3rem 0; border-bottom: 1px dashed #232a3a; }
        .cm-sl:last-child { border-bottom: 0; }
        .cm-sl b { color: #5cc8ff; }
        .cm-feed { display: flex; flex-direction: column; gap: 0.3rem; }
        .cm-ev {
          padding: 0.35rem 0.55rem; border-left: 3px solid #232a3a; border-radius: 0 6px 6px 0;
          background: #10141e; font-size: 0.74rem; line-height: 1.5;
        }
        .cm-tag {
          font-size: 0.62rem; text-transform: uppercase; letter-spacing: 0.06em;
          color: #8a93a6; margin-right: 0.5rem;
        }
        .cm-ev.msg { border-left-color: #b794f6; } .cm-ev.msg .cm-tag { color: #b794f6; }
        .cm-ev.gate { border-left-color: #5cc8ff; } .cm-ev.gate .cm-tag { color: #5cc8ff; }
        .cm-ev.task { border-left-color: #54d685; } .cm-ev.task .cm-tag { color: #54d685; }
        .cm-ev.audit { border-left-color: #ffcf5c; } .cm-ev.audit .cm-tag { color: #ffcf5c; }
        .cm-ev.veto { border-left-color: #ff6b6b; } .cm-ev.veto .cm-tag { color: #ff6b6b; }
        .cm-input {
          display: flex; align-items: center; gap: 0.6rem; margin-top: 0.9rem;
          background: #0a0d13; border: 1px solid #232a3a; border-radius: 8px; padding: 0.55rem 0.75rem;
        }
        .cm-prompt { color: #8a93a6; font-size: 0.72rem; }
        .cm-caret { color: #e6e9ef; flex: 1; }
        .cm-pill {
          color: #04121c; background: #5cc8ff; border-radius: 6px; padding: 0.1rem 0.5rem; font-size: 0.7rem;
        }
      `}</style>
    </div>
  );
}
