import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import sitemap from '@astrojs/sitemap';

const SITE = 'https://codehalwell.github.io';
const BASE = '/AnimaOS';

export default defineConfig({
  site: SITE,
  base: BASE,
  trailingSlash: 'ignore',
  integrations: [react(), sitemap()],
  build: {
    assets: 'assets',
  },
  vite: {
    ssr: {
      noExternal: ['react', 'react-dom'],
    },
  },
});
