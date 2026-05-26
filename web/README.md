# AnimaOS — site

Astro + React project for the AnimaOS GitHub Pages site.

```sh
cd web
npm install
npm run dev      # local dev server on http://localhost:4321/AnimaOS/
npm run build    # static output to web/dist/
npm run preview  # preview the build
```

## Deployment

`.github/workflows/pages.yml` builds and publishes `web/dist` to GitHub Pages on every push to `main` that touches `web/**`. The first deployment requires GitHub Pages to be set to **Source: GitHub Actions** in repo settings (Settings → Pages).

## Layout

- `src/pages/` — one route per Astro page
- `src/components/` — Astro and React components
- `src/components/diagrams/` — SVG architecture diagrams
- `src/data/` — typed content (crates, roadmap phases, glossary)
- `src/lib/url.ts` — `withBase()` helper for the `/AnimaOS` base path
- `src/styles/global.css` — design tokens and base styles
