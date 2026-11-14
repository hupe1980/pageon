// Frames through Puppeteer.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start } from '../lib/helper.mjs';
import { framesSite } from '../../lib/frames.mjs';

test('page.frames(), frame.$eval, click and type in same- and cross-origin frames', async () => {
  const s = await start(framesSite());
  try {
    const page = await s.browser.newPage();
    await page.goto('https://shop.test/checkout');
    const urls = page.frames().map((f) => f.url());
    assert.deepEqual(urls, ['https://shop.test/checkout', 'https://shop.test/help', 'https://pay.test/card']);
    // By URL: BiDi has no frame names (Puppeteer's `name()` is '' there).
    const help = page.frames().find((f) => f.url() === 'https://shop.test/help');
    assert.equal(help.parentFrame(), page.mainFrame());
    assert.equal(await help.$eval('#tip', (e) => e.textContent), 'Need help?');
    await help.click('#ask');
    assert.equal(await help.$eval('#ask', (e) => e.textContent), 'asked');
    const pay = page.frames().find((f) => f.url() === 'https://pay.test/card');
    await pay.type('#card', '4242');
    await pay.click('#paybtn');
    assert.equal(await pay.$eval('#out', (e) => e.textContent), 'paid 4242');
    // The main frame is untouched by input in the frames.
    assert.equal(await page.$eval('h1', (e) => e.textContent), 'Checkout');
    // contentFrame(): over CDP Puppeteer asks DOM.describeNode; over BiDi it reads
    // `iframe.contentWindow` (a cross-origin window for #pay, the real one for #help).
    assert.equal(await (await page.$('#pay')).contentFrame(), pay);
    assert.equal(await (await page.$('#help')).contentFrame(), help);
  } finally {
    await s.stop();
  }
});
