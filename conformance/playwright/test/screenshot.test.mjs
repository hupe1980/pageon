// Screenshots through Playwright.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { paintSite } from '../../lib/paint.mjs';
import { decodePng, near } from '../../lib/png.mjs';

let s, page;
before(async () => {
  s = await start(site(paintSite()));
  page = await s.context.newPage();
  await page.goto('https://paint.test/');
});
after(async () => s && s.stop());

test('page.screenshot(): the viewport as PNG, with images', async () => {
  const png = decodePng(await page.screenshot());
  assert.deepEqual([png.width, png.height], [1280, 720]);
  assert.ok(near(png.pixel(120, 65), [255, 0, 0]), 'the red box');
  assert.ok(near(png.pixel(310, 60), [255, 0, 0]), 'the image');
});

test('fullPage, clip and JPEG', async () => {
  const full = decodePng(await page.screenshot({ fullPage: true }));
  assert.ok(full.height >= 2000, `${full.height}`);
  assert.ok(near(full.pixel(30, 1930), [0, 255, 0]), 'below the fold');
  const clip = decodePng(await page.screenshot({ clip: { x: 90, y: 40, width: 60, height: 50 } }));
  assert.deepEqual([clip.width, clip.height], [60, 50]);
  const jpeg = await page.screenshot({ type: 'jpeg', quality: 80 });
  assert.equal(jpeg.readUInt16BE(0), 0xffd8);
});

test('locator.screenshot() is the element', async () => {
  const png = decodePng(await page.locator('#red').screenshot());
  assert.deepEqual([png.width, png.height], [40, 30]);
  assert.ok(near(png.pixel(20, 15), [255, 0, 0]));
});
