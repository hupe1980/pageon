import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, html } from '../lib/helper.mjs';

test('connect, newPage, goto, title, evaluate', async () => {
  const s = await start(site([html('https://shop.test/', '<title>Shop</title><h1>Hi</h1>')]));
  try {
    const page = await s.browser.newPage();
    const res = await page.goto('https://shop.test/');
    assert.equal(res.status(), 200);
    assert.equal(await page.title(), 'Shop');
    assert.equal(await page.evaluate((a) => a + 1, 1), 2);
  } finally {
    await s.stop();
  }
});
