const RAW = import.meta.env.BASE_URL ?? '/';
const BASE = RAW.endsWith('/') ? RAW.slice(0, -1) : RAW;

export function withBase(path: string): string {
  if (!path.startsWith('/')) path = '/' + path;
  if (path === '/') return BASE === '' ? '/' : BASE + '/';
  return BASE + path;
}
