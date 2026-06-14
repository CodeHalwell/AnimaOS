/// <reference path="../.astro/types.d.ts" />
/// <reference types="astro/client" />

interface ImportMetaEnv {
  /**
   * Base origin of a running AnimaOS operator console (the `console` crate),
   * e.g. `http://127.0.0.1:8088`. When unset, the live console stream falls
   * back to the loopback default and, if unreachable, to static sample data.
   * Must be prefixed `PUBLIC_` so Astro exposes it to client islands.
   */
  readonly PUBLIC_ANIMA_CONSOLE_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
