// Isolated worlds through Puppeteer: a page that poisons built-ins cannot falsify what
// Puppeteer's helpers compute (they run in the utility world or a BiDi sandbox).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { worldsSite } from '../../lib/worlds.mjs';

let s, page;
before(async () => {
  s = await start(site(worldsSite()));
  page = await s.browser.newPage();
});
after(async () => s && s.stop());

async function answers(url) {
  await page.goto(url);
  const b = await page.waitForSelector('#b', { visible: true });
  const box = await b.boundingBox();
  const text = await page.$('::-p-text(Real text)');
  await b.click();
  return {
    visible: Boolean(b),
    width: box && Math.round(box.width) > 0,
    text: Boolean(text),
    clicked: await page.$eval('#b', (e) => e.firstChild.data),
  };
}

test('helpers give the same answers on a poisoned page as on a clean one', async () => {
  const clean = await answers('https://clean.test/');
  assert.deepEqual(clean, { visible: true, width: true, text: true, clicked: 'clicked' });
  assert.deepEqual(await answers('https://poisoned.test/'), clean);
  // The page's own world keeps its lies.
  assert.equal(await page.evaluate(() => document.getElementById('t').textContent), 'lie');
});
