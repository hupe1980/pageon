// Shadow DOM through Playwright: locators pierce open shadow roots.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { shadowSite } from '../../lib/shadow.mjs';

let s, page;
before(async () => {
  s = await start(site(shadowSite()));
  page = await s.context.newPage();
  await page.goto('https://shadow.test/');
});
after(async () => s && s.stop());

test('locators pierce open shadow roots', async () => {
  await page.locator('my-counter button').click();
  await page.getByRole('button', { name: 'Clicked 1 times' }).waitFor();
  assert.equal(await page.locator('my-card .body').textContent(), 'card body');
  assert.equal(await page.getByText('Card title').count(), 1);
});
