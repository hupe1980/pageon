// Screenshots through Puppeteer.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, cdpOnly } from '../lib/helper.mjs';
import { paintSite } from '../../lib/paint.mjs';
import { decodePng, near } from '../../lib/png.mjs';

let s, page;
before(async () => {
  s = await start(site(paintSite()));
  page = await s.browser.newPage();
  await page.goto('https://paint.test/');
});
after(async () => s && s.stop());

test('page.screenshot(): the viewport as PNG, painted from layout, with images', async () => {
  const png = decodePng(Buffer.from(await page.screenshot()));
  assert.deepEqual([png.width, png.height], [1280, 720]);
  assert.ok(near(png.pixel(120, 65), [255, 0, 0]), 'the red box');
  assert.ok(near(png.pixel(310, 60), [255, 0, 0]), 'the image');
  assert.ok(near(png.pixel(5, 5), [255, 255, 255]), 'white page');
});

test('fullPage reaches below the fold; clip; JPEG', async () => {
  const full = decodePng(Buffer.from(await page.screenshot({ fullPage: true })));
  assert.equal(full.width, 1280);
  assert.ok(full.height >= 2000, `${full.height}`);
  assert.ok(near(full.pixel(30, 1930), [0, 255, 0]), 'below the fold');
  const clip = decodePng(Buffer.from(await page.screenshot({ clip: { x: 90, y: 40, width: 60, height: 50 } })));
  assert.deepEqual([clip.width, clip.height], [60, 50]);
  assert.ok(near(clip.pixel(30, 25), [255, 0, 0]));
  const jpeg = Buffer.from(await page.screenshot({ type: 'jpeg', quality: 80 }));
  assert.equal(jpeg.readUInt16BE(0), 0xffd8);
  await assert.rejects(page.screenshot({ type: 'webp' }), /png or jpeg/);
});

test('elementHandle.screenshot() is the element', async () => {
  const el = await page.$('#red');
  const png = decodePng(Buffer.from(await el.screenshot()));
  assert.deepEqual([png.width, png.height], [40, 30]);
  assert.ok(near(png.pixel(20, 15), [255, 0, 0]));
});

test('Pageon.screenshot: set-of-marks with the agent refs, and crops with provenance', { skip: cdpOnly }, async () => {
  const cdp = await page.createCDPSession();
  const { observation } = await cdp.send('Pageon.observe', { full: true });
  const buy = /button (e\d+) "Buy"/.exec(observation)[1];
  const marked = await cdp.send('Pageon.screenshot', { marks: true });
  assert.ok(marked.legend.includes(`button ${buy} "Buy"`), marked.legend);
  const crop = await cdp.send('Pageon.screenshot', { ref: buy });
  assert.equal(crop.provenance.tier, 'first-party');
  assert.equal(crop.provenance.reference, buy);
  await cdp.detach();
});
