// Shared by the conformance harnesses: starts `pageon cdp` on a replayed site.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

/** The `pageon` binary: PAGEON_BIN, else target/debug, else target/release. */
export function bin() {
  if (process.env.PAGEON_BIN) return process.env.PAGEON_BIN;
  for (const p of ['debug', 'release']) {
    const b = join(root, 'target', p, 'pageon');
    if (existsSync(b)) return b;
  }
  throw new Error('build the binary first: cargo build -p pageon (or set PAGEON_BIN)');
}

/** A recorded HTML response. */
export const html = (url, body, extra = {}) => ({
  method: 'GET', url, status: 200, headers: [['content-type', 'text/html']], body, ...extra,
});

/** A recording (pageon's replay log) from exchanges. */
export function site(exchanges) {
  return {
    version: 1, user_agent: '', exchanges,
    clock: Array.from({ length: 4096 }, (_, i) => 1_800_000_000_000 + i * 1000),
    random: Array.from({ length: 64 }, (_, i) => i + 1),
  };
}

const extra = () => (process.env.PAGEON_CDP_ARGS ?? '').split(' ').filter(Boolean);

/**
 * Starts `pageon cdp --port 0` replaying `log` (plus PAGEON_CDP_ARGS) and, given `connect`,
 * connects a client with it. Resolves to { browser, url, wsUrl, child, exited, stderr, stop }.
 */
export async function start(log, { args = [], env = {}, connect } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'pageon-cdp-'));
  const rec = join(dir, 'site.rec.json');
  writeFileSync(rec, JSON.stringify(log));
  const child = spawn(bin(), ['cdp', '--port', '0', '--replay', rec, ...extra(), ...args], {
    stdio: ['ignore', 'ignore', 'pipe'], env: { ...process.env, ...env },
  });
  let stderr = '';
  const wsUrl = await new Promise((ok, fail) => {
    child.stderr.on('data', (d) => {
      stderr += d;
      const m = /DevTools listening on (ws:\/\/\S+)/.exec(stderr);
      if (m) ok(m[1]);
    });
    child.on('exit', (code) => fail(new Error(`pageon cdp exited (${code}):\n${stderr}`)));
  });
  const url = `http://127.0.0.1:${new URL(wsUrl).port}`;
  const bidiUrl = `ws://127.0.0.1:${new URL(wsUrl).port}/session`;
  const exited = new Promise((ok) => child.on('exit', (code) => ok(code)));
  const browser = connect ? await connect(wsUrl, bidiUrl) : null;
  return {
    browser, url, wsUrl, bidiUrl, child, dir, exited, stderr: () => stderr,
    async stop() {
      try { await browser?.close?.(); } catch {}
      // The server exits when its client disconnects; without one, end it.
      const t = setTimeout(() => child.kill(), browser ? 5000 : 0);
      await exited;
      clearTimeout(t);
    },
  };
}
