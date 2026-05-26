import { useMemo, useState } from 'react';
import { glossary, categories } from '../data/glossary';

export default function GlossarySearch() {
  const [q, setQ] = useState('');
  const [cat, setCat] = useState<string>('All');

  const filtered = useMemo(() => {
    const needle = q.toLowerCase().trim();
    return glossary.filter((t) => {
      const matchesCat = cat === 'All' || t.category === cat;
      if (!matchesCat) return false;
      if (!needle) return true;
      return (
        t.term.toLowerCase().includes(needle) ||
        t.meaning.toLowerCase().includes(needle) ||
        (t.location ?? '').toLowerCase().includes(needle)
      );
    });
  }, [q, cat]);

  return (
    <div className="glossary">
      <div className="g-controls">
        <input
          type="search"
          placeholder="Search terms…"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          className="g-search"
          aria-label="Search glossary"
        />
        <div className="g-cats">
          <button
            className={`g-cat ${cat === 'All' ? 'active' : ''}`}
            onClick={() => setCat('All')}
          >
            All
          </button>
          {categories.map((c) => (
            <button
              key={c}
              className={`g-cat ${cat === c ? 'active' : ''}`}
              onClick={() => setCat(c)}
            >
              {c}
            </button>
          ))}
        </div>
      </div>

      <div className="g-results">
        {filtered.length === 0 && (
          <div className="g-empty">No terms match — file an issue on the repo.</div>
        )}
        {filtered.map((t) => (
          <div key={t.term} className="g-row">
            <div className="g-term-col">
              <div className="g-term">{t.term}</div>
              <div className="g-cat-tag">{t.category}</div>
              {t.location && <div className="g-loc mono">{t.location}</div>}
            </div>
            <div className="g-meaning">{t.meaning}</div>
          </div>
        ))}
      </div>

      <style>{`
        .glossary {
          display: flex;
          flex-direction: column;
          gap: 1.5rem;
        }

        .g-controls {
          display: flex;
          flex-direction: column;
          gap: 1rem;
          padding: 1.25rem;
          background: var(--bg-1);
          border: 1px solid var(--border);
          border-radius: var(--radius);
          position: sticky;
          top: 70px;
          z-index: 10;
        }

        .g-search {
          width: 100%;
          padding: 0.75rem 1rem;
          background: var(--bg-2);
          border: 1px solid var(--border);
          border-radius: 8px;
          color: var(--fg-0);
          font-family: var(--mono);
          font-size: 0.92rem;
        }
        .g-search:focus {
          outline: none;
          border-color: var(--accent);
        }

        .g-cats {
          display: flex;
          gap: 0.4rem;
          flex-wrap: wrap;
        }

        .g-cat {
          background: var(--bg-2);
          border: 1px solid var(--border);
          color: var(--fg-2);
          padding: 0.4rem 0.85rem;
          border-radius: 999px;
          font-size: 0.8rem;
          cursor: pointer;
          font-family: var(--sans);
        }
        .g-cat:hover { color: var(--fg-0); }
        .g-cat.active {
          background: var(--accent);
          color: #07090c;
          border-color: var(--accent);
          font-weight: 600;
        }

        .g-results {
          display: flex;
          flex-direction: column;
        }

        .g-row {
          display: grid;
          grid-template-columns: 220px 1fr;
          gap: 1.5rem;
          padding: 1rem 0;
          border-bottom: 1px solid var(--border-soft);
          align-items: start;
        }
        @media (max-width: 700px) {
          .g-row { grid-template-columns: 1fr; gap: 0.5rem; }
        }

        .g-term { font-weight: 600; color: var(--fg-0); }
        .g-cat-tag {
          display: inline-block;
          font-size: 0.7rem;
          color: var(--fg-3);
          font-family: var(--mono);
          margin-top: 0.25rem;
        }
        .g-loc {
          font-size: 0.72rem;
          color: var(--accent);
          margin-top: 0.25rem;
        }

        .g-meaning { color: var(--fg-1); font-size: 0.95rem; }

        .g-empty {
          padding: 3rem;
          text-align: center;
          color: var(--fg-3);
          background: var(--bg-1);
          border: 1px dashed var(--border);
          border-radius: var(--radius);
        }
      `}</style>
    </div>
  );
}
