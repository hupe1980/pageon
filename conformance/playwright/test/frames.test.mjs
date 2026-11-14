// Frames through Playwright.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start } from '../lib/helper.mjs';
import { framesSite } from '../../lib/frames.mjs';

let s, page;
before(async () => {
  s = await start(framesSite());
  page = await s.context.newPage();
  await page.goto('https://shop.test/checkout');
});
after(async () => s && s.stop());

test('page.frames(): main, same-origin and cross-origin, with names and parents', () => {
  assert.deepEqual(page.frames().map((f) => f.url()), [
    'https://shop.test/checkout', 'https://shop.test/help', 'https://pay.test/card',
  ]);
  const pay = page.frame({ name: 'pay' });
  assert.equal(pay.parentFrame(), page.mainFrame());
  assert.equal(pay.url(), 'https://pay.test/card');
});

test('frameLocator: read and click in a same-origin frame', async () => {
  const help = page.frameLocator('#help');
  assert.equal(await help.locator('#tip').textContent(), 'Need help?');
  await help.getByRole('button', { name: 'Ask' }).click();
  assert.equal(await help.locator('#ask').textContent(), 'asked');
});

test('frameLocator: fill and click in a cross-origin frame (with a border and padding)', async () => {
  const pay = page.frameLocator('#pay');
  await pay.getByLabel('Card').fill('4242 4242');
  await pay.getByRole('button', { name: 'Pay' }).click();
  assert.equal(await pay.locator('#out').textContent(), 'paid 4242 4242');
  assert.equal(await page.locator('h1').textContent(), 'Checkout');
});

test('frame.evaluate runs in the frame', async () => {
  assert.equal(await page.frame({ name: 'pay' }).evaluate(() => location.origin), 'https://pay.test');
  assert.equal(await page.frame({ name: 'help' }).evaluate(() => document.querySelector('#tip').id), 'tip');
});
