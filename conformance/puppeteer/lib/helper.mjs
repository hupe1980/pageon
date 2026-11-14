// Starts `pageon cdp` on a replayed site and connects Puppeteer to it, over CDP or, with
// PAGEON_PROTOCOL=webDriverBiDi, over WebDriver BiDi.
import puppeteer from 'puppeteer-core';
import { start as startPageon } from '../../lib/pageon.mjs';

export { bin, html, site } from '../../lib/pageon.mjs';

/** The protocol Puppeteer speaks: `cdp` (default) or `webDriverBiDi`. */
export const protocol = process.env.PAGEON_PROTOCOL === 'webDriverBiDi' ? 'webDriverBiDi' : 'cdp';
export const bidi = protocol === 'webDriverBiDi';

/** Skips a test that is about CDP itself (discovery, CDP sessions) when running over BiDi. */
export const cdpOnly = bidi ? 'CDP only' : false;

/** Connects Puppeteer to a started pageon. */
export function connect(wsUrl, bidiUrl) {
  return bidi
    ? puppeteer.connect({ browserWSEndpoint: bidiUrl, protocol: 'webDriverBiDi' })
    : puppeteer.connect({ browserWSEndpoint: wsUrl });
}

const connectTo = connect;

/** Starts `pageon cdp` replaying `log`; `connect: false` leaves the client to the test. */
export async function start(log, { connect = true, ...options } = {}) {
  const s = await startPageon(log, {
    ...options,
    connect: connect ? connectTo : undefined,
  });
  // Puppeteer leaves the browser running on `disconnect` (the server then exits by itself).
  const stop = s.stop;
  s.stop = async () => {
    try { await s.browser?.disconnect(); } catch {}
    s.browser = null;
    await stop();
  };
  return s;
}
