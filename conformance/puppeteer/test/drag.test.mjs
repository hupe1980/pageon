// Mouse drags through Puppeteer (CDP and BiDi): press, move, release run HTML drag and drop.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { dragSite } from '../../lib/drag.mjs';

const centre = async (page, sel) => {
  const b = await (await page.$(sel)).boundingBox();
  return [b.x + b.width / 2, b.y + b.height / 2];
};

test('mouse down, move and up drag a card and reorder a list; a click stays a click', async () => {
  const s = await start(site(dragSite()));
  try {
    const page = await s.browser.newPage();
    await page.goto('https://board.test/');
    const [x0, y0] = await centre(page, '#c1');
    const [x1, y1] = await centre(page, '#done');
    await page.mouse.move(x0, y0);
    await page.mouse.down();
    await page.mouse.move(x1, y1, { steps: 3 });
    await page.mouse.up();
    assert.equal(await page.$eval('#done', (e) => e.querySelector('#c1') !== null), true);
    const [a0, b0] = await centre(page, '#a');
    const [a1, b1] = await centre(page, '#c');
    await page.mouse.move(a0, b0);
    await page.mouse.down();
    await page.mouse.move(a1, b1, { steps: 2 });
    await page.mouse.up();
    assert.match(await page.$eval('#log', (e) => e.textContent), /drop on done;order bca;/);
    // Pressed and released in place: a click, as before.
    await page.click('#b');
    assert.match(await page.$eval('#log', (e) => e.textContent), /order bca;order bca;$/);
  } finally {
    await s.stop();
  }
});
