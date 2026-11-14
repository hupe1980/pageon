// The History API through Puppeteer: page.url() follows pushState, goBack stays in the page.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { historySite } from '../../lib/history.mjs';

let s, page;
before(async () => {
  s = await start(site(historySite()));
  page = await s.browser.newPage();
  await page.goto('https://spa.test/');
});
after(async () => s && s.stop());

test('pushState navigations are seen, and goBack returns in place', async () => {
  await page.click('#orders');
  assert.equal(page.url(), 'https://spa.test/orders');
  assert.equal(await page.$eval('#view', (e) => e.textContent), 'Page /orders');
  await page.click('#settings');
  assert.equal(page.url(), 'https://spa.test/settings');
  await page.goBack();
  assert.equal(page.url(), 'https://spa.test/orders');
  assert.equal(await page.$eval('#view', (e) => e.textContent), 'Page /orders');
  assert.equal(await page.evaluate(() => history.state.path), '/orders');
  assert.equal(await page.evaluate(() => history.length), 3);
});
