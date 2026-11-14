// MiniWoB++ through Puppeteer's own selectors and input.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { exchanges, ORIGIN, available } from '../../lib/miniwob.mjs';

const skip = available() ? false : 'run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++';
const quoted = (q) => [...q.matchAll(/"([^"]*)"/g)].map((m) => m[1].trim());

async function task(name, solve) {
  const s = await start(site(exchanges()));
  try {
    const page = await s.browser.newPage();
    await page.goto(`${ORIGIN}miniwob/${name}.html`);
    const query = await page.$eval('#query', (q) => q.textContent);
    await solve(page, query);
    // The next episode starts (and resets WOB_REWARD_GLOBAL) after a timer; the display keeps it.
    const reward = Number.parseFloat(await page.$eval('#reward-last', (r) => r.textContent));
    assert.ok(reward > 0, `${name}: reward ${reward} for "${query}"`);
  } finally {
    await s.stop();
  }
}

test('click-button', { skip }, () => task('click-button', async (page, q) => {
  const [label] = quoted(q);
  // The button whose own text is the label (a text selector would also match the query).
  for (const b of await page.$$('#area button')) {
    if ((await b.evaluate((x) => x.textContent.trim())) === label) await b.click();
  }
}));

test('enter-text', { skip }, () => task('enter-text', async (page, q) => {
  const [text] = quoted(q);
  await page.type('#tt', text);
  await page.click('#subbtn');
}));

test('login-user', { skip }, () => task('login-user', async (page, q) => {
  const [user, pass] = quoted(q);
  await page.type('#username', user);
  await page.type('#password', pass);
  await page.click('#subbtn');
}));

test('click-checkboxes', { skip }, () => task('click-checkboxes', async (page, q) => {
  const want = q.replace(/^Select\s+/, '').replace(/\s+and click Submit\.?$/, '');
  const names = want === 'nothing' ? [] : want.split(/,\s*/).map((w) => w.trim());
  for (const label of await page.$$('#boxes label')) {
    const text = (await label.evaluate((l) => l.textContent)).trim();
    if (names.includes(text)) await label.click();
  }
  await page.click('#subbtn');
}));

test('choose-list', { skip }, () => task('choose-list', async (page, q) => {
  const item = q.replace(/^Select\s+/, '').replace(/\s+from the list and click Submit\.?$/, '').trim();
  const value = await page.$$eval('#options option', (os, t) => os.find((o) => o.textContent.trim() === t)?.value, item);
  await page.select('#options', value);
  await page.click('.secondary-action');
}));
