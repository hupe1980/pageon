// Scrolling through Puppeteer: clicks below the fold, the wheel, scrollIntoView, and the
// viewport screenshot at the scroll position.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { scrollSite } from '../../lib/scroll.mjs';
import { decodePng, near } from '../../lib/png.mjs';

let s, page;
before(async () => {
  s = await start(site(scrollSite()));
  page = await s.browser.newPage();
  await page.goto('https://scroll.test/');
});
after(async () => s && s.stop());

test('page.click scrolls the element into view and clicks it there', async () => {
  await page.click('#deep');
  const [clientY, pageY] = (await page.$eval('#deep', (b) => b.textContent))
    .replace('clicked at ', '').split(',').map(Number);
  assert.ok(clientY >= 0 && clientY < 720, `clientY ${clientY}`);
  assert.ok(pageY > 3000, `pageY ${pageY}`);
  assert.ok((await page.evaluate(() => scrollY)) > 2000);
});

test('mouse.wheel scrolls what is under the pointer', async () => {
  await page.evaluate(() => scrollTo(0, 0));
  await page.mouse.move(600, 300);
  await page.mouse.wheel({ deltaY: 500 });
  assert.equal(await page.evaluate(() => scrollY), 500);
});

test('scrollIntoView and element scrolling', async () => {
  await page.evaluate(() => scrollTo(0, 0));
  await page.$eval('#inner', (e) => e.scrollIntoView({ block: 'center' }));
  assert.ok((await page.$eval('#box', (b) => b.scrollTop)) > 300);
  const top = await page.$eval('#inner', (e) => e.getBoundingClientRect().top);
  assert.ok(top > 0 && top < 720, `${top}`);
});

test('the viewport screenshot is where the page is scrolled to', async () => {
  await page.evaluate(() => scrollTo(0, 4000));
  const png = decodePng(Buffer.from(await page.screenshot()));
  assert.ok(near(png.pixel(10, 10), [255, 0, 0]), 'the fixed header');
  assert.ok(near(png.pixel(600, 400), [0, 0, 255]), 'the blue section below the fold');
});
