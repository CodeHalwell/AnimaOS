import { useEffect, useState } from 'react';

type Phase = 'waking' | 'pruning' | 'replay' | 'dreaming' | 'compilation';

const ORDER: Phase[] = ['waking', 'pruning', 'replay', 'dreaming', 'compilation'];

const PHASE_INFO: Record<Phase, { title: string; trigger: string; description: string }> = {
  waking: {
    title: 'Waking',
    trigger: 'stress < 0.9 AND agenda not empty',
    description: 'The agent runs its homeostatic loop: read guidance, query stress, select task, dispatch. Long tasks are not preempted; new tasks are selected against the current stress envelope.',
  },
  pruning: {
    title: 'Sleep · Pruning',
    trigger: 'agenda empty AND stress < 0.4',
    description: 'Emotional decay is applied to L1 and L2. Below-threshold entries are evicted or compressed. The semantic floor protects high-generation knowledge from erasure.',
  },
  replay: {
    title: 'Sleep · Replay',
    trigger: 'after Pruning',
    description: 'Sampled past questions are re-run against the pruned memory. If accuracy drops below threshold, the pruning is rolled back.',
  },
  dreaming: {
    title: 'Sleep · Dreaming',
    trigger: 'after Replay',
    description: 'Random graph walks across L3 produce candidate associative edges. Yield is variable by design; candidates are validated in the next pruning cycle.',
  },
  compilation: {
    title: 'Sleep · Compilation',
    trigger: 'after Dreaming',
    description: 'Waking-state traces are compiled into training-data formats and persisted under training_corpus/ in L3. The sleep cycle closes; the agent transitions back to Waking.',
  },
};

export default function LifecycleDiagram() {
  const [active, setActive] = useState<Phase>('waking');
  const [auto, setAuto] = useState(true);

  useEffect(() => {
    if (!auto) return;
    const t = setInterval(() => {
      setActive((p) => ORDER[(ORDER.indexOf(p) + 1) % ORDER.length]!);
    }, 3200);
    return () => clearInterval(t);
  }, [auto]);

  const info = PHASE_INFO[active];

  return (
    <div className="lifecycle">
      <div className="cycle-wrap">
        <svg viewBox="0 0 400 400" className="cycle-svg" role="img" aria-label="Sleep cycle">
          <defs>
            <radialGradient id="lc-glow" cx="0.5" cy="0.5">
              <stop offset="0%" stopColor="#7df9c8" stopOpacity="0.15" />
              <stop offset="100%" stopColor="#7df9c8" stopOpacity="0" />
            </radialGradient>
          </defs>
          <circle cx="200" cy="200" r="180" fill="url(#lc-glow)" />
          <circle cx="200" cy="200" r="140" fill="none" stroke="#1f2a35" strokeWidth="1" strokeDasharray="3 5" />

          {ORDER.map((phase, i) => {
            const angle = (i / ORDER.length) * Math.PI * 2 - Math.PI / 2;
            const x = 200 + Math.cos(angle) * 140;
            const y = 200 + Math.sin(angle) * 140;
            const isActive = phase === active;
            return (
              <g key={phase} onClick={() => { setAuto(false); setActive(phase); }} style={{ cursor: 'pointer' }}>
                <circle
                  cx={x}
                  cy={y}
                  r={isActive ? 26 : 18}
                  fill={isActive ? '#7df9c8' : '#11161c'}
                  stroke={isActive ? '#7df9c8' : '#1f2a35'}
                  strokeWidth="1.5"
                  style={{ transition: 'all 0.4s ease' }}
                />
                <text
                  x={x}
                  y={y + 4}
                  textAnchor="middle"
                  fontSize="10"
                  fontFamily="ui-monospace, monospace"
                  fill={isActive ? '#07090c' : '#5a6675'}
                  style={{ pointerEvents: 'none', fontWeight: 600 }}
                >
                  {phase.slice(0, 4)}
                </text>
              </g>
            );
          })}

          <circle cx="200" cy="200" r="60" fill="#0c1014" stroke="#1f2a35" strokeWidth="1" />
          <text x="200" y="195" textAnchor="middle" fontSize="11" fontFamily="ui-monospace, monospace" fill="#7df9c8" fontWeight="600">vita</text>
          <text x="200" y="212" textAnchor="middle" fontSize="9" fontFamily="ui-monospace, monospace" fill="#5a6675">homeostatic loop</text>
        </svg>
      </div>

      <div className="lifecycle-info">
        <div className="info-head">
          <span className="badge badge-accent mono">{active}</span>
          <button className="auto-btn" onClick={() => setAuto((a) => !a)}>
            {auto ? '⏸ pause' : '▶ resume'}
          </button>
        </div>
        <h3>{info.title}</h3>
        <div className="trigger">
          <span className="trigger-label">Trigger</span>
          <code>{info.trigger}</code>
        </div>
        <p>{info.description}</p>
      </div>

      <style>{`
        .lifecycle {
          display: grid;
          grid-template-columns: 1fr 1fr;
          gap: 2rem;
          align-items: center;
        }

        @media (max-width: 900px) { .lifecycle { grid-template-columns: 1fr; } }

        .cycle-wrap {
          display: flex;
          justify-content: center;
        }

        .cycle-svg {
          width: 100%;
          max-width: 400px;
          height: auto;
        }

        .lifecycle-info {
          padding: 1.5rem;
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-radius: var(--radius);
        }

        .info-head {
          display: flex;
          justify-content: space-between;
          align-items: center;
          margin-bottom: 1rem;
        }

        .auto-btn {
          background: transparent;
          border: 1px solid var(--border);
          color: var(--fg-2);
          padding: 0.3rem 0.7rem;
          border-radius: 6px;
          font-family: var(--mono);
          font-size: 0.75rem;
          cursor: pointer;
        }
        .auto-btn:hover { background: var(--bg-2); color: var(--fg-0); }

        .lifecycle-info h3 { margin-bottom: 1rem; }

        .trigger {
          background: var(--bg-2);
          padding: 0.75rem 1rem;
          border-radius: 8px;
          border: 1px solid var(--border-soft);
          margin-bottom: 1rem;
        }
        .trigger-label {
          display: block;
          font-size: 0.7rem;
          text-transform: uppercase;
          letter-spacing: 0.1em;
          color: var(--fg-3);
          margin-bottom: 0.25rem;
        }
        .trigger code {
          background: none;
          border: none;
          padding: 0;
          color: var(--accent);
        }
      `}</style>
    </div>
  );
}
