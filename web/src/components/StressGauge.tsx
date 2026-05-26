import { useEffect, useState } from 'react';

export default function StressGauge() {
  const [stress, setStress] = useState(0.32);

  useEffect(() => {
    const t = setInterval(() => {
      setStress((s) => {
        const drift = (Math.random() - 0.5) * 0.1;
        const next = Math.max(0, Math.min(1, s + drift));
        return next;
      });
    }, 1200);
    return () => clearInterval(t);
  }, []);

  const state =
    stress >= 0.9 ? { label: 'EMERGENCY', color: '#ff6b6b', action: 'emergency_consolidate()' } :
    stress >= 0.75 ? { label: 'PRESSURE', color: '#ffb454', action: 'L1 → L2 demotion' } :
    stress >= 0.4 ? { label: 'NOMINAL', color: '#5dd1ff', action: 'select_optimal_task()' } :
    { label: 'IDLE-CAPABLE', color: '#7df9c8', action: 'agenda empty → sleep' };

  const pct = Math.round(stress * 100);
  const dash = 2 * Math.PI * 50;
  const offset = dash * (1 - stress);

  return (
    <div className="gauge">
      <div className="gauge-svg-wrap">
        <svg viewBox="0 0 120 120" className="gauge-svg">
          <circle cx="60" cy="60" r="50" fill="none" stroke="#1f2a35" strokeWidth="6" />
          <circle
            cx="60"
            cy="60"
            r="50"
            fill="none"
            stroke={state.color}
            strokeWidth="6"
            strokeLinecap="round"
            strokeDasharray={dash}
            strokeDashoffset={offset}
            transform="rotate(-90 60 60)"
            style={{ transition: 'stroke-dashoffset 1s ease, stroke 0.4s ease' }}
          />
          <text x="60" y="58" textAnchor="middle" fontFamily="ui-monospace, monospace" fontSize="20" fontWeight="600" fill="#e6edf3">
            {pct}
          </text>
          <text x="60" y="74" textAnchor="middle" fontFamily="ui-monospace, monospace" fontSize="8" fill="#8590a0">
            STRESS
          </text>
        </svg>
      </div>

      <div className="gauge-meta">
        <div className="gauge-state mono" style={{ color: state.color }}>
          {state.label}
        </div>
        <div className="gauge-thresh">
          <ThreshRow label="< 0.40" desc="agenda empty → sleep" active={stress < 0.4} color="#7df9c8" />
          <ThreshRow label="0.40 – 0.75" desc="normal waking loop" active={stress >= 0.4 && stress < 0.75} color="#5dd1ff" />
          <ThreshRow label="0.75 – 0.90" desc="L1 → L2 demotion" active={stress >= 0.75 && stress < 0.9} color="#ffb454" />
          <ThreshRow label="≥ 0.90" desc="emergency consolidate" active={stress >= 0.9} color="#ff6b6b" />
        </div>
        <div className="gauge-action">
          <span className="gauge-action-label">vita action</span>
          <code className="gauge-action-code">{state.action}</code>
        </div>
      </div>

      <style>{`
        .gauge {
          display: grid;
          grid-template-columns: 140px 1fr;
          gap: 1.5rem;
          align-items: center;
          padding: 1.5rem;
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-radius: var(--radius);
        }
        @media (max-width: 600px) { .gauge { grid-template-columns: 1fr; justify-items: center; text-align: center; } }

        .gauge-svg { width: 140px; height: 140px; }

        .gauge-state {
          font-weight: 700;
          font-size: 1rem;
          letter-spacing: 0.05em;
          margin-bottom: 0.75rem;
        }

        .gauge-thresh { display: flex; flex-direction: column; gap: 0.25rem; margin-bottom: 0.75rem; }

        .gauge-action {
          padding-top: 0.75rem;
          border-top: 1px solid var(--border-soft);
        }

        .gauge-action-label {
          display: block;
          font-size: 0.65rem;
          text-transform: uppercase;
          letter-spacing: 0.1em;
          color: var(--fg-3);
          margin-bottom: 0.25rem;
        }
        .gauge-action-code {
          background: var(--bg-2);
          padding: 0.3rem 0.6rem;
          border-radius: 6px;
          border: 1px solid var(--border-soft);
          font-size: 0.82rem;
          color: var(--accent);
        }
      `}</style>
    </div>
  );
}

function ThreshRow({ label, desc, active, color }: { label: string; desc: string; active: boolean; color: string }) {
  return (
    <div className={`tr ${active ? 'active' : ''}`}>
      <span className="tr-dot" style={{ background: active ? color : '#2a323d' }} />
      <span className="tr-label mono">{label}</span>
      <span className="tr-desc">{desc}</span>
      <style>{`
        .tr {
          display: grid;
          grid-template-columns: 8px 100px 1fr;
          gap: 0.5rem;
          align-items: center;
          padding: 0.25rem 0;
          font-size: 0.78rem;
          opacity: 0.55;
          transition: opacity 0.3s ease;
        }
        .tr.active { opacity: 1; }
        .tr-dot { width: 8px; height: 8px; border-radius: 999px; transition: background 0.3s; }
        .tr-label { color: var(--fg-1); font-weight: 600; }
        .tr-desc { color: var(--fg-3); }
        .tr.active .tr-desc { color: var(--fg-1); }
      `}</style>
    </div>
  );
}
