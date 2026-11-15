// Mouse buttons, click counts, modifiers and key chords through Playwright over connectOverCDP.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, html } from '../lib/helper.mjs';

const PAGE = `<button id=b>Target</button><a id=l href=/next>Next</a><input id=f value=old>
<p id=log></p>
<script>
const log = t => document.getElementById('log').textContent += t + ';';
for (const t of ['click','dblclick','auxclick','contextmenu'])
  b.addEventListener(t, e => log(t + ':' + e.button + ':' + e.detail + (e.ctrlKey ? ':ctrl' : '') + (e.shiftKey ? ':shift' : '')));
b.addEventListener('contextmenu', e => e.preventDefault());
</script>`;

let s, page;
before(async () => {
  s = await start(site([html('https://input.test/', PAGE)]));
  page = await s.context.newPage();
  await page.goto('https://input.test/');
});
after(async () => s && s.stop());

const clear = () => page.evaluate(() => { document.getElementById('log').textContent = ''; });
const log = () => page.locator('#log').textContent();

test('dblclick fires two clicks and a dblclick', async () => {
  await clear();
  await page.locator('#b').dblclick();
  assert.equal(await log(), 'click:0:1;click:0:2;dblclick:0:2;');
});

test('right and middle clicks', async () => {
  await clear();
  await page.locator('#b').click({ button: 'right' });
  await page.locator('#b').click({ button: 'middle' });
  assert.equal(await log(), 'contextmenu:2:1;auxclick:2:1;auxclick:1:1;');
});

test('modifiers reach the events', async () => {
  await clear();
  await page.locator('#b').click({ modifiers: ['Control', 'Shift'] });
  assert.equal(await log(), 'click:0:1:ctrl:shift;');
});

test('Control+A selects a field so typing replaces it', async () => {
  await page.locator('#f').click();
  await page.keyboard.press('Control+A');
  await page.keyboard.type('new');
  assert.equal(await page.locator('#f').inputValue(), 'new');
});
