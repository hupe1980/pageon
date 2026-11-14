// End-to-end speed through Puppeteer. Opt in: PAGEON_SPEED=1, with a release
// binary (PAGEON_BIN=target/release/pageon); caged unless PAGEON_CDP_ARGS says otherwise.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, connect, protocol } from '../lib/helper.mjs';
import { exchanges, ORIGIN, available } from '../../lib/miniwob.mjs';

const skip = !process.env.PAGEON_SPEED ? 'set PAGEON_SPEED=1 (release binary)' : !available() ? 'no MiniWoB++ cache' : false;

test('connect + newPage + goto, and page.click', { skip }, async () => {
  // Warm up: the OS checks a freshly built binary once, and Puppeteer loads its protocol
  // module on first connect (BiDi's takes ~20 ms, client-side).
  await (await start(site([]))).stop();
  const s = await start(site(exchanges()), { connect: false });
  try {
    let t = performance.now();
    const browser = await connect(s.wsUrl, s.bidiUrl);
    const page = await browser.newPage();
    await page.goto(`${ORIGIN}miniwob/click-button.html`);
    const open = performance.now() - t;
    const button = await page.$('#query'); // stable (a task button starts the next episode)
    await button.click();
    t = performance.now();
    const n = 10;
    for (let i = 0; i < n; i++) await button.click();
    const click = (performance.now() - t) / n;
    console.log(`${protocol}: connect + newPage + goto ${open.toFixed(1)} ms; page.click ${click.toFixed(1)} ms`);
    assert.ok(open < 150, `open: ${open.toFixed(1)} ms`);
    assert.ok(click < 25, `click: ${click.toFixed(1)} ms`);
    await browser.disconnect();
  } finally {
    await s.stop();
  }
});
