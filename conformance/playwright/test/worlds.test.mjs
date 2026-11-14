// Isolated worlds through Playwright: locators run in Playwright's utility world, so a page
// that poisons built-ins cannot falsify visibility, text or geometry.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { worldsSite } from '../../lib/worlds.mjs';

let s, page;
before(async () => {
  s = await start(site(worldsSite()));
  page = await s.context.newPage();
});
after(async () => s && s.stop());

async function answers(url) {
  await page.goto(url);
  const b = page.locator('#b');
  const box = await b.boundingBox();
  const out = {
    visible: await b.isVisible(),
    width: Boolean(box && Math.round(box.width) > 0),
    byText: await page.getByText('Real text').count(),
    text: await page.locator('#t').textContent(),
  };
  await b.click();
  out.clicked = await page.locator('#b').textContent();
  return out;
}

test('locators give the same answers on a poisoned page as on a clean one', async () => {
  const clean = await answers('https://clean.test/');
  assert.deepEqual(clean, { visible: true, width: true, byText: 1, text: 'Real text', clicked: 'clicked' });
  assert.deepEqual(await answers('https://poisoned.test/'), clean);
  assert.equal(await page.evaluate(() => document.getElementById('t').textContent), 'lie');
});
