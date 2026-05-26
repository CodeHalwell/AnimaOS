export default function MemoryHierarchy() {
  const tiers = [
    {
      tier: 'L1',
      name: 'Working Context',
      cost: 'near zero',
      capacity: 'model context window',
      lifetime: 'Minutes',
      backing: 'PagedAttention',
      content: 'Current task prompt, recent tool outputs, scratchpad, system directives.',
      colour: '#7df9c8',
    },
    {
      tier: 'L2',
      name: 'Warm Memory Cache',
      cost: 'low',
      capacity: 'host / microVM RAM',
      lifetime: 'Hours',
      backing: 'scc::HashMap + ARC',
      content: 'Recent conversation turns, retrieved L3 items, computed embeddings.',
      colour: '#5dd1ff',
    },
    {
      tier: 'L3',
      name: 'Cerebral Archival Store',
      cost: 'vector-similarity lookup',
      capacity: 'storage-bound',
      lifetime: 'Indefinite',
      backing: 'embedded LanceDB',
      content: 'All past conversations, learned tool schemas, dream-discovered associations, training data.',
      colour: '#c084fc',
    },
  ];

  return (
    <div className="mh">
      {tiers.map((t, i) => (
        <div key={t.tier} className="tier" style={{ ['--c' as string]: t.colour }}>
          <div className="tier-head">
            <div className="tier-mark">
              <span className="tier-name mono">{t.tier}</span>
              <span className="tier-sub">{t.name}</span>
            </div>
            <div className="tier-backing mono">{t.backing}</div>
          </div>
          <div className="tier-body">
            <p>{t.content}</p>
            <div className="tier-meta">
              <Meta label="cost" value={t.cost} />
              <Meta label="capacity" value={t.capacity} />
              <Meta label="lifetime" value={t.lifetime} />
            </div>
          </div>
          {i < tiers.length - 1 && (
            <div className="tier-arrow" aria-hidden="true">
              <span>↑ promotion · demotion ↓</span>
            </div>
          )}
        </div>
      ))}

      <div className="decay-box">
        <div className="decay-label">emotionally-modulated decay</div>
        <code className="decay-formula">
          S(t) = max(S<sub>floor</sub>, S₀ · e<sup>−λt</sup> · (1 + α · arousal + σ · surprise))
        </code>
        <p className="decay-note">
          λ = 0.02 / hr waking, 0.005 / hr sleep · α = 1.5 · σ = 2.0 · S<sub>floor</sub> = 0.3.
          The floor prevents distilled, high-generation knowledge from being erased by the decay loop.
        </p>
      </div>

      <style>{`
        .mh {
          display: flex;
          flex-direction: column;
          gap: 0;
        }

        .tier {
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-left: 3px solid var(--c);
          border-radius: var(--radius);
          padding: 1.5rem;
          margin-bottom: 1rem;
          position: relative;
        }

        .tier-head {
          display: flex;
          justify-content: space-between;
          align-items: baseline;
          margin-bottom: 1rem;
        }

        .tier-mark { display: flex; align-items: baseline; gap: 0.75rem; }
        .tier-name {
          font-size: 1.75rem;
          font-weight: 700;
          color: var(--c);
          letter-spacing: -0.02em;
        }
        .tier-sub {
          color: var(--fg-0);
          font-size: 1.1rem;
          font-weight: 600;
        }

        .tier-backing {
          font-size: 0.75rem;
          color: var(--fg-3);
          background: var(--bg-2);
          padding: 0.2rem 0.5rem;
          border-radius: 4px;
          border: 1px solid var(--border-soft);
        }

        .tier-body p { margin-bottom: 1rem; }

        .tier-meta {
          display: grid;
          grid-template-columns: repeat(3, 1fr);
          gap: 1rem;
        }
        @media (max-width: 600px) { .tier-meta { grid-template-columns: 1fr; } }

        .tier-arrow {
          display: flex;
          justify-content: center;
          font-family: var(--mono);
          font-size: 0.7rem;
          color: var(--fg-3);
          margin: -0.5rem 0 0.5rem;
        }

        .decay-box {
          margin-top: 1.5rem;
          padding: 1.5rem;
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-radius: var(--radius);
        }
        .decay-label {
          font-size: 0.7rem;
          text-transform: uppercase;
          letter-spacing: 0.12em;
          color: var(--fg-3);
          margin-bottom: 0.75rem;
        }
        .decay-formula {
          display: block;
          background: var(--bg-2);
          padding: 1rem;
          border-radius: 8px;
          border: 1px solid var(--border-soft);
          color: var(--accent);
          font-size: 0.95rem;
          margin-bottom: 0.75rem;
        }
        .decay-note { color: var(--fg-2); margin: 0; font-size: 0.88rem; }
      `}</style>
    </div>
  );
}

function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div className="meta">
      <div className="meta-label">{label}</div>
      <div className="meta-value">{value}</div>
      <style>{`
        .meta {
          padding: 0.6rem 0.85rem;
          background: var(--bg-2);
          border: 1px solid var(--border-soft);
          border-radius: 6px;
        }
        .meta-label {
          font-family: var(--mono);
          font-size: 0.62rem;
          text-transform: uppercase;
          letter-spacing: 0.1em;
          color: var(--fg-3);
          margin-bottom: 0.2rem;
        }
        .meta-value { font-size: 0.85rem; color: var(--fg-0); }
      `}</style>
    </div>
  );
}
