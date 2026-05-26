import { useEffect, useState } from 'react';

const STATUS_LINES = [
  'PID 1: vita::somatic_execution_loop ready',
  'corpus::FrameAllocator initialised (audited unsafe)',
  'memory::VirtualContextManager — L1 0% / L2 0% / L3 archive: open',
  'scheduler::IterationAwareMlfq — 3 tiers, 0 tasks queued',
  'interoception::HomeostaticMonitor — stress index 0.04',
  'praxis::ToolRegistry — 0 tools, circuit breakers closed',
  '/dev/anima/senses/human — waiting for input',
  'system: waking. listening.',
];

export default function Hero() {
  const [visible, setVisible] = useState<number>(0);

  useEffect(() => {
    if (visible >= STATUS_LINES.length) return;
    const t = setTimeout(() => setVisible((v) => v + 1), 280);
    return () => clearTimeout(t);
  }, [visible]);

  return (
    <div className="hero-wrap">
      <div className="hero-left">
        <span className="eyebrow">
          <span className="dot pulse" /> phase 1 · partially complete
        </span>
        <h1>
          A somatic substrate for an{' '}
          <span className="grad">autonomous LLM agent.</span>
        </h1>
        <p className="lead">
          AnimaOS is a bare-metal, cloud-isolated framekernel operating system designed
          as the body, autonomic nervous system, and reflex arcs for an LLM agent that
          runs as <code>init</code> and supervises itself. The agent and the OS are
          one organism — and Anima is what makes that organism alive.
        </p>
        <div className="hero-actions">
          <a className="btn btn-primary" href="#design-principles">Read the design</a>
          <a className="btn" href="https://github.com/codehalwell/animaos" target="_blank" rel="noopener noreferrer">
            View on GitHub
          </a>
        </div>
        <div className="hero-stats">
          <Stat label="Crates" value="10" sub="single Cargo workspace" />
          <Stat label="TCB crates" value="1" sub="corpus — audited unsafe" />
          <Stat label="Sleep phases" value="4" sub="prune · replay · dream · compile" />
          <Stat label="Memory tiers" value="3" sub="L1 · L2 · L3 (CLS hierarchy)" />
        </div>
      </div>

      <div className="hero-right" aria-hidden="true">
        <div className="terminal">
          <div className="term-bar">
            <span className="term-dot red" />
            <span className="term-dot yellow" />
            <span className="term-dot green" />
            <span className="term-title">anima-hosted :: pid 1</span>
          </div>
          <div className="term-body">
            {STATUS_LINES.slice(0, visible).map((line, i) => (
              <div key={i} className="term-line">
                <span className="term-prompt">[{String(i).padStart(2, '0')}]</span>{' '}
                <span className={line.includes('waking') ? 'term-ok' : 'term-fg'}>{line}</span>
              </div>
            ))}
            {visible < STATUS_LINES.length && <span className="caret" />}
            {visible >= STATUS_LINES.length && (
              <div className="term-line term-prompt-final">
                <span className="term-prompt-mark">›</span> <span className="caret" />
              </div>
            )}
          </div>
        </div>
      </div>

      <style>{`
        .hero-wrap {
          display: grid;
          grid-template-columns: 1.05fr 1fr;
          gap: 4rem;
          align-items: center;
          padding: 5rem 0 4rem;
        }

        @media (max-width: 1000px) {
          .hero-wrap { grid-template-columns: 1fr; gap: 3rem; padding: 3rem 0; }
        }

        .hero-left h1 {
          margin: 1.25rem 0 1.25rem;
        }

        .grad {
          background: linear-gradient(120deg, #7df9c8, #5dd1ff 60%, #c084fc);
          -webkit-background-clip: text;
          background-clip: text;
          color: transparent;
          font-style: italic;
        }

        .lead {
          font-size: 1.125rem;
          line-height: 1.7;
          color: var(--fg-1);
          max-width: 580px;
        }

        .hero-actions {
          display: flex;
          gap: 0.75rem;
          margin-top: 1.5rem;
          flex-wrap: wrap;
        }

        .hero-stats {
          display: grid;
          grid-template-columns: repeat(4, 1fr);
          gap: 1rem;
          margin-top: 3rem;
          padding-top: 2rem;
          border-top: 1px solid var(--border-soft);
        }
        @media (max-width: 720px) { .hero-stats { grid-template-columns: repeat(2, 1fr); } }

        .terminal {
          background: linear-gradient(180deg, #0a0e13, #0d1218);
          border: 1px solid var(--border);
          border-radius: 14px;
          overflow: hidden;
          font-family: var(--mono);
          box-shadow:
            0 30px 80px -30px rgba(0, 0, 0, 0.8),
            0 0 0 1px rgba(125, 249, 200, 0.06) inset,
            var(--shadow-glow);
        }

        .term-bar {
          display: flex;
          align-items: center;
          gap: 0.4rem;
          padding: 0.65rem 1rem;
          background: #0a0e13;
          border-bottom: 1px solid var(--border-soft);
        }

        .term-dot {
          width: 11px;
          height: 11px;
          border-radius: 999px;
          background: #2a323d;
        }
        .term-dot.red { background: #ff5f56; }
        .term-dot.yellow { background: #ffbd2e; }
        .term-dot.green { background: #27c93f; }

        .term-title {
          margin-left: auto;
          font-size: 0.72rem;
          color: var(--fg-3);
          letter-spacing: 0.05em;
        }

        .term-body {
          padding: 1.5rem 1.5rem 2.5rem;
          font-size: 0.78rem;
          line-height: 1.85;
          min-height: 320px;
        }

        .term-line { display: flex; gap: 0.5rem; }
        .term-prompt { color: var(--fg-3); flex-shrink: 0; }
        .term-fg { color: var(--fg-1); }
        .term-ok { color: var(--accent); }
        .term-prompt-mark { color: var(--accent); }
        .term-prompt-final { color: var(--accent); }

        .caret {
          display: inline-block;
          width: 0.6em;
          height: 1em;
          background: var(--accent);
          vertical-align: text-bottom;
          animation: blink 1s steps(2) infinite;
        }
        @keyframes blink { 0%, 49% { opacity: 1; } 50%, 100% { opacity: 0; } }
      `}</style>
    </div>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub: string }) {
  return (
    <div className="hero-stat">
      <div className="hero-stat-value">{value}</div>
      <div className="hero-stat-label">{label}</div>
      <div className="hero-stat-sub">{sub}</div>
      <style>{`
        .hero-stat-value {
          font-family: var(--mono);
          font-size: 1.75rem;
          font-weight: 600;
          color: var(--fg-0);
          letter-spacing: -0.02em;
        }
        .hero-stat-label {
          font-size: 0.8rem;
          color: var(--fg-2);
          margin-top: 0.25rem;
          font-weight: 600;
        }
        .hero-stat-sub {
          font-family: var(--mono);
          font-size: 0.7rem;
          color: var(--fg-3);
          margin-top: 0.25rem;
        }
      `}</style>
    </div>
  );
}
