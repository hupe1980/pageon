// End-to-end speed through Playwright. Opt in: PAGEON_SPEED=1, with a release
// binary (PAGEON_BIN=target/release/pageon); caged unless PAGEON_CDP_ARGS says otherwise.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright-core';
import { start, site, html } from '../lib/helper.mjs';
import { exchanges, ORIGIN, available } from '../../lib/miniwob.mjs';

const skip = !process.env.PAGEON_SPEED ? 'set PAGEON_SPEED=1 (release binary)' : !available() ? 'no MiniWoB++ cache' : false;

test('connectOverCDP + newPage + goto, and locator.click', { skip }, async () => {
  // Warm up: the OS checks a freshly built binary once.
  await (await start(site([]), { connect: false })).stop();
  const counter = html('https://speed.test/', '<button id=c onclick="this.textContent = +this.textContent + 1">0</button>');
  const s = await start(site([...exchanges(), counter]), { connect: false });
  try {
    let t = performance.now();
    const browser = await chromium.connectOverCDP(s.wsUrl);
    const page = await browser.contexts()[0].newPage();
    await page.goto(`${ORIGIN}miniwob/click-button.html`);
    const open = performance.now() - t;
    // Clicks on a stable page (a MiniWoB click may end the episode and show its cover).
    await page.goto('https://speed.test/');
    const button = page.locator('#c');
    await button.click();
    t = performance.now();
    const n = 10;
    for (let i = 0; i < n; i++) await button.click();
    const click = (performance.now() - t) / n;
    assert.equal(await button.textContent(), String(n + 1));
    console.log(`connectOverCDP + newPage + goto ${open.toFixed(1)} ms; locator.click ${click.toFixed(1)} ms`);
    assert.ok(open < 200, `open: ${open.toFixed(1)} ms`);
    assert.ok(click < 30, `click: ${click.toFixed(1)} ms`);
    await browser.close();
  } finally {
    await s.stop();
  }
});
