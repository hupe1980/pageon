// Shadow DOM through Puppeteer: `>>>` and `pierce/` selectors, clicks inside shadow roots.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { shadowSite } from '../../lib/shadow.mjs';

let s, page;
before(async () => {
  s = await start(site(shadowSite()));
  page = await s.browser.newPage();
  await page.goto('https://shadow.test/');
});
after(async () => s && s.stop());

test('>>> pierces open shadow roots and clicks land inside', async () => {
  const button = await page.$('my-counter >>> button');
  assert.ok(button, 'found through the shadow root');
  await button.click();
  await button.click();
  assert.equal(await button.evaluate((b) => b.textContent), 'Clicked 2 times');
});

test('pierce/ and shadowRoot from scripts', async () => {
  const body = await page.$('pierce/.body');
  assert.equal(await body.evaluate((p) => p.textContent), 'card body');
  const titled = await page.$eval('my-card', (c) => c.shadowRoot.querySelector('slot').assignedNodes()[0].textContent);
  assert.equal(titled, 'Card title');
  assert.equal(await page.$eval('x-closed', (c) => c.shadowRoot), null, 'closed roots stay closed');
});
