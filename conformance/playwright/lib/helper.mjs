// Starts `pageon cdp` on a replayed site and connects Playwright to it over CDP.
import { chromium } from 'playwright-core';
import { start as startPageon } from '../../lib/pageon.mjs';

export { bin, html, site } from '../../lib/pageon.mjs';

/** Starts `pageon cdp` replaying `log`, connected; `s.context` is the default context. */
export async function start(log, { connect = true, ...options } = {}) {
  const s = await startPageon(log, {
    ...options,
    connect: connect ? (ws) => chromium.connectOverCDP(ws) : undefined,
  });
  s.context = s.browser?.contexts()[0];
  return s;
}
