import { useState } from 'react';
import { crates, type Crate } from '../data/crates';

export default function CrateGrid() {
  const [active, setActive] = useState<Crate>(crates[0]!);

  return (
    <div className="crate-grid-wrap">
      <div className="crate-list">
        {crates.map((c) => (
          <button
            key={c.name}
            className={`crate-row ${active.name === c.name ? 'active' : ''}`}
            onClick={() => setActive(c)}
            style={{ ['--row-accent' as string]: c.color }}
          >
            <span className="crate-row-name mono">
              <span className="crate-row-dot" />
              {c.pkg ?? c.name}
            </span>
            <span className="crate-row-role">{c.role}</span>
          </button>
        ))}
      </div>

      <div className="crate-detail card">
        <div className="detail-head">
          <span className="badge mono" style={{ color: active.color, borderColor: active.color + '55' }}>
            crates/{active.name}
          </span>
          {active.pkg && (
            <span className="badge mono">package: {active.pkg}</span>
          )}
          <span className={`badge ${active.verification === 'TCB (audited unsafe)' ? 'badge-warn' : 'badge-accent'}`}>
            {active.verification}
          </span>
        </div>

        <h3 className="detail-title">{active.role}</h3>
        <p className="detail-metaphor">{active.metaphor}</p>
        <p className="detail-mech">{active.mechanism}</p>

        <div className="detail-highlights">
          <h4>Implemented surface</h4>
          <ul>
            {active.highlights.map((h, i) => (
              <li key={i}>{h}</li>
            ))}
          </ul>
        </div>
      </div>

      <style>{`
        .crate-grid-wrap {
          display: grid;
          grid-template-columns: 320px 1fr;
          gap: 1.5rem;
          align-items: start;
        }
        @media (max-width: 900px) { .crate-grid-wrap { grid-template-columns: 1fr; } }

        .crate-list {
          display: flex;
          flex-direction: column;
          gap: 0.4rem;
          border: 1px solid var(--border);
          border-radius: var(--radius);
          padding: 0.5rem;
          background: var(--bg-1);
        }

        .crate-row {
          display: flex;
          flex-direction: column;
          gap: 0.2rem;
          align-items: flex-start;
          padding: 0.7rem 0.85rem;
          background: transparent;
          border: 1px solid transparent;
          border-radius: 8px;
          color: var(--fg-1);
          cursor: pointer;
          text-align: left;
          transition: all 0.15s ease;
          font-family: inherit;
        }

        .crate-row:hover {
          background: var(--bg-2);
        }

        .crate-row.active {
          background: var(--bg-2);
          border-color: color-mix(in srgb, var(--row-accent) 35%, transparent);
        }

        .crate-row-name {
          display: flex;
          align-items: center;
          gap: 0.55rem;
          font-size: 0.95rem;
          color: var(--fg-0);
          font-weight: 600;
        }

        .crate-row-dot {
          width: 8px;
          height: 8px;
          border-radius: 999px;
          background: var(--row-accent);
          box-shadow: 0 0 8px 0 color-mix(in srgb, var(--row-accent) 70%, transparent);
        }

        .crate-row-role {
          font-size: 0.78rem;
          color: var(--fg-3);
          padding-left: 1.1rem;
        }

        .crate-row.active .crate-row-role { color: var(--fg-2); }

        .crate-detail {
          min-height: 380px;
        }

        .detail-head {
          display: flex;
          flex-wrap: wrap;
          gap: 0.5rem;
          margin-bottom: 1.25rem;
        }

        .detail-title {
          font-size: 1.5rem;
          margin-bottom: 0.5rem;
        }

        .detail-metaphor {
          font-style: italic;
          color: var(--fg-2);
          margin-bottom: 1rem;
        }

        .detail-mech {
          color: var(--fg-1);
          margin-bottom: 1.5rem;
        }

        .detail-highlights h4 {
          font-size: 0.75rem;
          text-transform: uppercase;
          letter-spacing: 0.1em;
          color: var(--fg-3);
          margin-bottom: 0.75rem;
          font-weight: 600;
        }

        .detail-highlights ul {
          list-style: none;
          padding-left: 0;
        }

        .detail-highlights li {
          padding: 0.5rem 0.75rem 0.5rem 1.5rem;
          background: var(--bg-2);
          border: 1px solid var(--border-soft);
          border-radius: 6px;
          margin-bottom: 0.4rem;
          font-family: var(--mono);
          font-size: 0.82rem;
          color: var(--fg-1);
          position: relative;
        }

        .detail-highlights li::before {
          content: '›';
          position: absolute;
          left: 0.6rem;
          top: 0.5rem;
          color: var(--accent);
        }
      `}</style>
    </div>
  );
}
