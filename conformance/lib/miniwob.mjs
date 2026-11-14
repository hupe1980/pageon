// The MiniWoB++ pages of the BrowserGym copy as a replay log (like the Rust tests' `miniwob_log`).
import { readdirSync, readFileSync, statSync, existsSync } from 'node:fs';
import { join, resolve, dirname, extname } from 'node:path';
import { fileURLToPath } from 'node:url';

export const base = resolve(dirname(fileURLToPath(import.meta.url)), '../../bench/.cache/miniwob/miniwob/html');
export const ORIGIN = 'http://miniwob.test/';
const START = "<script>addEventListener('load', function () { core.startEpisodeReal(); });</script>";
const TYPES = { html: 'text/html; charset=utf-8', js: 'text/javascript; charset=utf-8', css: 'text/css; charset=utf-8', json: 'application/json' };

export function exchanges(rel = '') {
  const out = [];
  for (const name of readdirSync(join(base, rel))) {
    const path = join(base, rel, name);
    const r = rel ? `${rel}/${name}` : name;
    if (statSync(path).isDirectory()) { out.push(...exchanges(r)); continue; }
    const ext = extname(name).slice(1);
    const ct = TYPES[ext] ?? 'application/octet-stream';
    const data = readFileSync(path);
    const e = { method: 'GET', url: ORIGIN + r, status: 200, headers: [['content-type', ct]] };
    if (ext === 'html' && r.startsWith('miniwob')) e.body = data.toString('utf8').replace('</body>', `${START}</body>`);
    else e.body_base64 = data.toString('base64');
    out.push(e);
  }
  return out;
}

export const available = () => existsSync(join(base, 'miniwob'));
