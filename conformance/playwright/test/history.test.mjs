// The History API through Playwright: waitForURL and goBack on a single-page app.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { historySite } from '../../lib/history.mjs';

let s, page;
before(async () => {
  s = await start(site(historySite()));
  page = await s.context.newPage();
  await page.goto('https://spa.test/');
});
after(async () => s && s.stop());

test('pushState navigations are seen, and goBack returns in place', async () => {
  await page.getByText('Orders').click();
  await page.waitForURL('**/orders');
  assert.equal(await page.locator('#view').textContent(), 'Page /orders');
  await page.getByText('Settings').click();
  await page.waitForURL('**/settings');
  await page.goBack();
  assert.equal(page.url(), 'https://spa.test/orders');
  assert.equal(await page.locator('#view').textContent(), 'Page /orders');
});
