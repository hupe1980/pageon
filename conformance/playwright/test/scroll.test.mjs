// Scrolling through Playwright: locators below the fold, the wheel, scrollIntoViewIfNeeded,
// and the viewport screenshot at the scroll position.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { scrollSite } from '../../lib/scroll.mjs';
import { decodePng, near } from '../../lib/png.mjs';

let s, page;
before(async () => {
  s = await start(site(scrollSite()));
  page = await s.context.newPage();
  await page.goto('https://scroll.test/');
});
after(async () => s && s.stop());

test('locator.click scrolls the element into view and clicks it there', async () => {
  await page.locator('#deep').click();
  const [clientY, pageY] = (await page.locator('#deep').textContent())
    .replace('clicked at ', '').split(',').map(Number);
  assert.ok(clientY >= 0 && clientY < 720, `clientY ${clientY}`);
  assert.ok(pageY > 3000, `pageY ${pageY}`);
});

test('mouse.wheel and scrollIntoViewIfNeeded', async () => {
  await page.evaluate(() => scrollTo(0, 0));
  await page.mouse.move(600, 300);
  await page.mouse.wheel(0, 700);
  await page.waitForFunction(() => scrollY === 700);
  await page.locator('#inner').scrollIntoViewIfNeeded();
  const top = await page.locator('#inner').evaluate((e) => e.getBoundingClientRect().top);
  assert.ok(top >= 0 && top < 720, `${top}`);
});

test('the viewport screenshot is where the page is scrolled to', async () => {
  await page.evaluate(() => scrollTo(0, 4000));
  const png = decodePng(await page.screenshot());
  assert.ok(near(png.pixel(10, 10), [255, 0, 0]), 'the fixed header');
  assert.ok(near(png.pixel(600, 400), [0, 0, 255]), 'the blue section below the fold');
});
