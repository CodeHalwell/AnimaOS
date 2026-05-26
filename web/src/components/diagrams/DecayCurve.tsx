import { useMemo, useState } from 'react';

export default function DecayCurve() {
  const [arousal, setArousal] = useState(0.3);
  const [surprise, setSurprise] = useState(0.5);

  const lambda = 0.02;
  const alpha = 1.5;
  const sigma = 2.0;
  const floor = 0.3;

  const data = useMemo(() => {
    const points: { t: number; s: number; baseline: number }[] = [];
    for (let t = 0; t <= 120; t += 2) {
      const mod = 1 + alpha * arousal + sigma * surprise;
      const s = Math.max(floor, Math.exp(-lambda * t) * mod);
      const baseline = Math.max(floor, Math.exp(-lambda * t));
      points.push({ t, s, baseline });
    }
    return points;
  }, [arousal, surprise]);

  const W = 600;
  const H = 240;
  const PAD_L = 50;
  const PAD_B = 40;
  const PAD_T = 20;
  const PAD_R = 20;
  const innerW = W - PAD_L - PAD_R;
  const innerH = H - PAD_T - PAD_B;
  const yMax = 2.0;

  const x = (t: number) => PAD_L + (t / 120) * innerW;
  const y = (s: number) => PAD_T + (1 - s / yMax) * innerH;

  const pathFor = (key: 's' | 'baseline') =>
    data.map((p, i) => `${i === 0 ? 'M' : 'L'} ${x(p.t).toFixed(1)} ${y(p[key]).toFixed(1)}`).join(' ');

  return (
    <figure className="diag">
      <svg viewBox={`0 0 ${W} ${H + 20}`} className="diag-svg" role="img" aria-label="Memory activation decay curve">
        {/* grid */}
        {[0.0, 0.3, 0.5, 1.0, 1.5, 2.0].map((v) => (
          <g key={v}>
            <line x1={PAD_L} y1={y(v)} x2={W - PAD_R} y2={y(v)} stroke="#1f2a35" strokeDasharray={v === 0.3 ? '4 3' : '1 4'} />
            <text x={PAD_L - 8} y={y(v) + 4} textAnchor="end" fontFamily="ui-monospace, monospace" fontSize="9" fill="#5a6675">{v.toFixed(1)}</text>
          </g>
        ))}
        {[0, 24, 48, 72, 96, 120].map((t) => (
          <g key={t}>
            <line x1={x(t)} y1={PAD_T} x2={x(t)} y2={H - PAD_B} stroke="#1f2a35" strokeDasharray="1 4" />
            <text x={x(t)} y={H - PAD_B + 14} textAnchor="middle" fontFamily="ui-monospace, monospace" fontSize="9" fill="#5a6675">{t}h</text>
          </g>
        ))}

        {/* semantic floor label */}
        <text x={W - PAD_R - 4} y={y(0.3) - 6} textAnchor="end" fontFamily="ui-monospace, monospace" fontSize="9" fill="#ffb454">S_floor = 0.3</text>

        {/* baseline curve */}
        <path d={pathFor('baseline')} fill="none" stroke="#5a6675" strokeWidth="1.5" strokeDasharray="3 4" />

        {/* modulated curve */}
        <path d={pathFor('s')} fill="none" stroke="#7df9c8" strokeWidth="2.5" />

        {/* axis labels */}
        <text x={PAD_L} y={14} fontFamily="ui-monospace, monospace" fontSize="10" fill="#7df9c8">activation S(t)</text>
        <text x={W - PAD_R} y={H + 14} textAnchor="end" fontFamily="ui-monospace, monospace" fontSize="9" fill="#5a6675">wall-time hours</text>
      </svg>

      <div className="dc-controls">
        <label>
          <span>arousal · α={alpha}</span>
          <input type="range" min="0" max="1" step="0.05" value={arousal} onChange={(e) => setArousal(parseFloat(e.target.value))} />
          <code>{arousal.toFixed(2)}</code>
        </label>
        <label>
          <span>surprise · σ={sigma}</span>
          <input type="range" min="0" max="1" step="0.05" value={surprise} onChange={(e) => setSurprise(parseFloat(e.target.value))} />
          <code>{surprise.toFixed(2)}</code>
        </label>
      </div>

      <figcaption>
        Dashed line: the unmodulated baseline (e<sup>−λt</sup>, λ = 0.02 / hr). Solid line: with
        arousal × {alpha} and surprise × {sigma} multiplying the decay. The semantic floor
        at S = 0.3 prevents distilled knowledge from being erased.
      </figcaption>

      <style>{`
        .dc-controls {
          display: grid;
          grid-template-columns: 1fr 1fr;
          gap: 1rem;
          margin-top: 1rem;
          padding: 1rem;
          background: var(--bg-2);
          border: 1px solid var(--border-soft);
          border-radius: 8px;
        }
        @media (max-width: 600px) { .dc-controls { grid-template-columns: 1fr; } }
        label {
          display: grid;
          grid-template-columns: 1fr 1fr 40px;
          gap: 0.75rem;
          align-items: center;
          font-family: var(--mono);
          font-size: 0.8rem;
          color: var(--fg-2);
        }
        label code {
          background: var(--bg-1);
          padding: 0.15em 0.4em;
          border-radius: 4px;
          color: var(--accent);
          font-size: 0.78rem;
          text-align: center;
        }
        input[type='range'] {
          accent-color: var(--accent);
          width: 100%;
        }
      `}</style>
    </figure>
  );
}
