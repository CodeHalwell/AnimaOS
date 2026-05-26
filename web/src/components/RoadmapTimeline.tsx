import { useState } from 'react';
import { phases } from '../data/roadmap';

export default function RoadmapTimeline() {
  const [open, setOpen] = useState<string>(phases[0]!.id);

  return (
    <div className="timeline">
      {phases.map((phase, i) => {
        const isOpen = open === phase.id;
        const statusColor =
          phase.status === 'partially complete'
            ? 'badge-accent'
            : phase.status === 'in design'
              ? 'badge-blue'
              : 'badge';

        return (
          <article key={phase.id} className={`tl-card ${isOpen ? 'open' : ''}`} style={{ ['--accent' as string]: phase.accent }}>
            <header className="tl-head" onClick={() => setOpen(isOpen ? '' : phase.id)} role="button" tabIndex={0}>
              <div className="tl-num-wrap">
                <div className="tl-num">{String(phase.number).padStart(2, '0')}</div>
                {i < phases.length - 1 && <div className="tl-line" />}
              </div>
              <div className="tl-meta">
                <div className="tl-meta-top">
                  <span className="mono dim small">phase {phase.number} · {phase.months}</span>
                  <span className={`badge ${statusColor}`}>{phase.status}</span>
                </div>
                <h3>{phase.name}</h3>
                <p className="tl-focus">{phase.focus}</p>
              </div>
              <button className="tl-toggle" aria-label={isOpen ? 'Collapse' : 'Expand'}>
                {isOpen ? '−' : '+'}
              </button>
            </header>

            {isOpen && (
              <div className="tl-body">
                <p className="tl-long">{phase.longFocus}</p>

                <h4 className="tl-sub">Milestones</h4>
                <div className="tl-milestones">
                  {phase.milestones.map((m) => (
                    <div className="tl-mile" key={m.id}>
                      <div className="tl-mile-id mono">{m.id}</div>
                      <div className="tl-mile-name">{m.name}</div>
                      <div className="tl-mile-time mono dim small">{m.weeks ?? m.months}</div>
                      <div className="tl-mile-desc">{m.description}</div>
                    </div>
                  ))}
                </div>

                <div className="tl-two">
                  <div>
                    <h4 className="tl-sub">Exit criteria</h4>
                    <ol className="tl-exit">
                      {phase.exitCriteria.map((c, i) => <li key={i}>{c}</li>)}
                    </ol>
                  </div>

                  <div>
                    <h4 className="tl-sub">Risks &amp; mitigations</h4>
                    <div className="tl-risks">
                      {phase.risks.map((r, i) => (
                        <div key={i} className="tl-risk">
                          <div className="tl-risk-label small">RISK</div>
                          <div className="tl-risk-text">{r.risk}</div>
                          <div className="tl-risk-label small">MITIGATION</div>
                          <div className="tl-risk-mit">{r.mitigation}</div>
                        </div>
                      ))}
                    </div>
                  </div>
                </div>
              </div>
            )}
          </article>
        );
      })}

      <style>{`
        .timeline { display: flex; flex-direction: column; gap: 0; }

        .tl-card {
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-radius: var(--radius);
          margin-bottom: 1rem;
          overflow: hidden;
          transition: border-color 0.2s ease;
        }
        .tl-card.open { border-color: color-mix(in srgb, var(--accent) 40%, var(--border)); }

        .tl-head {
          display: grid;
          grid-template-columns: 60px 1fr auto;
          gap: 1.5rem;
          padding: 1.5rem;
          cursor: pointer;
          align-items: start;
        }
        .tl-head:hover { background: var(--bg-2); }

        .tl-num-wrap { position: relative; }
        .tl-num {
          width: 44px;
          height: 44px;
          border-radius: 999px;
          background: var(--bg-2);
          border: 1.5px solid var(--accent);
          color: var(--accent);
          font-family: var(--mono);
          font-weight: 700;
          display: flex;
          align-items: center;
          justify-content: center;
          font-size: 0.85rem;
          flex-shrink: 0;
        }

        .tl-meta-top {
          display: flex;
          gap: 0.75rem;
          align-items: center;
          margin-bottom: 0.5rem;
          flex-wrap: wrap;
        }

        .tl-meta h3 {
          margin: 0.25rem 0 0.5rem;
        }

        .tl-focus {
          color: var(--fg-2);
          margin: 0;
          font-size: 0.95rem;
        }

        .tl-toggle {
          background: var(--bg-2);
          border: 1px solid var(--border);
          color: var(--fg-1);
          width: 36px;
          height: 36px;
          border-radius: 8px;
          font-size: 1.3rem;
          cursor: pointer;
          flex-shrink: 0;
        }

        .tl-body {
          padding: 0 1.5rem 1.5rem 4.5rem;
          border-top: 1px solid var(--border-soft);
          padding-top: 1.5rem;
          margin-left: 0;
        }
        @media (max-width: 700px) { .tl-body { padding-left: 1.5rem; } }

        .tl-long { color: var(--fg-1); margin-bottom: 1.5rem; }

        .tl-sub {
          font-size: 0.7rem;
          text-transform: uppercase;
          letter-spacing: 0.12em;
          color: var(--fg-3);
          font-weight: 700;
          margin: 1.5rem 0 0.75rem;
        }

        .tl-milestones {
          display: grid;
          grid-template-columns: 1fr;
          gap: 0.5rem;
        }

        .tl-mile {
          display: grid;
          grid-template-columns: 50px 1fr auto;
          gap: 0.75rem;
          padding: 0.75rem 1rem;
          background: var(--bg-2);
          border: 1px solid var(--border-soft);
          border-radius: 8px;
          font-size: 0.88rem;
          align-items: center;
        }
        .tl-mile-id { color: var(--accent); font-weight: 700; font-size: 0.78rem; }
        .tl-mile-name { color: var(--fg-0); font-weight: 600; }
        .tl-mile-desc {
          grid-column: 2 / -1;
          color: var(--fg-2);
          font-size: 0.82rem;
        }
        @media (max-width: 700px) {
          .tl-mile { grid-template-columns: 1fr; }
          .tl-mile-desc { grid-column: 1; }
        }

        .tl-two {
          display: grid;
          grid-template-columns: 1fr 1fr;
          gap: 2rem;
          margin-top: 0.5rem;
        }
        @media (max-width: 800px) { .tl-two { grid-template-columns: 1fr; } }

        .tl-exit { padding-left: 1.25rem; color: var(--fg-1); }

        .tl-risk {
          padding: 0.75rem 1rem;
          background: var(--bg-2);
          border: 1px solid var(--border-soft);
          border-left: 2px solid var(--warn);
          border-radius: 6px;
          margin-bottom: 0.6rem;
        }
        .tl-risk-label {
          font-family: var(--mono);
          letter-spacing: 0.1em;
          color: var(--fg-3);
          font-size: 0.65rem;
          margin-top: 0.4rem;
        }
        .tl-risk-text { color: var(--fg-0); font-size: 0.88rem; }
        .tl-risk-mit { color: var(--fg-2); font-size: 0.85rem; }
      `}</style>
    </div>
  );
}
