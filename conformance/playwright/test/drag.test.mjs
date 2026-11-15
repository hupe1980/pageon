// Drags and file lists through Playwright over connectOverCDP (drag interception, DataTransfer).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { dragSite } from '../../lib/drag.mjs';

let s, page;
before(async () => {
  s = await start(site(dragSite()));
  page = await s.context.newPage();
  await page.goto('https://board.test/');
});
after(async () => s && s.stop());

test('dragTo moves a card with HTML drag and drop', async () => {
  await page.locator('#c1').dragTo(page.locator('#done'));
  assert.equal(await page.locator('#done #c1').count(), 1);
  assert.match(await page.locator('#log').textContent(), /drop on done/);
});

test('dragTo reorders a pointer-driven list', async () => {
  await page.locator('#a').dragTo(page.locator('#c'));
  assert.match(await page.locator('#log').textContent(), /order bca/);
});

test('setInputFiles with buffers builds the files with DataTransfer', async () => {
  await page.locator('#f').setInputFiles([
    { name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('hello') },
    { name: 'b.csv', mimeType: 'text/csv', buffer: Buffer.from('x,y\n1,2') },
  ]);
  assert.match(await page.locator('#log').textContent(), /files a\.txt:5,b\.csv:7/);
});
