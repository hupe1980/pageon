// MiniWoB++ through Playwright locators.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { exchanges, ORIGIN, available } from '../../lib/miniwob.mjs';

const skip = available() ? false : 'run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++';
const quoted = (q) => [...q.matchAll(/"([^"]*)"/g)].map((m) => m[1].trim());

async function task(name, solve) {
  const s = await start(site(exchanges()));
  try {
    const page = await s.context.newPage();
    await page.goto(`${ORIGIN}miniwob/${name}.html`);
    const query = await page.locator('#query').textContent();
    await solve(page, query);
    // The next episode starts (and resets WOB_REWARD_GLOBAL) after a timer; the display keeps it.
    const reward = Number.parseFloat(await page.locator('#reward-last').textContent());
    assert.ok(reward > 0, `${name}: reward ${reward} for "${query}"`);
  } finally {
    await s.stop();
  }
}

test('click-button', { skip }, () => task('click-button', async (page, q) => {
  const [label] = quoted(q);
  // Episodes may repeat a label; any button with it earns the reward.
  await page.locator('#area').getByRole('button', { name: label, exact: true }).first().click();
}));

test('enter-text', { skip }, () => task('enter-text', async (page, q) => {
  await page.locator('#tt').fill(quoted(q)[0]);
  await page.getByRole('button', { name: 'Submit' }).click();
}));

test('login-user', { skip }, () => task('login-user', async (page, q) => {
  const [user, pass] = quoted(q);
  await page.locator('#username').fill(user);
  await page.locator('#password').fill(pass);
  await page.getByRole('button', { name: 'Login' }).click();
}));

test('click-checkboxes', { skip }, () => task('click-checkboxes', async (page, q) => {
  const want = q.replace(/^Select\s+/, '').replace(/\s+and click Submit\.?$/, '');
  for (const name of want === 'nothing' ? [] : want.split(/,\s*/)) {
    await page.locator('#boxes label', { hasText: name.trim() }).click();
  }
  await page.getByRole('button', { name: 'Submit' }).click();
}));

test('choose-list', { skip }, () => task('choose-list', async (page, q) => {
  const item = q.replace(/^Select\s+/, '').replace(/\s+from the list and click Submit\.?$/, '').trim();
  await page.locator('#options').selectOption({ label: item });
  await page.getByRole('button', { name: 'Submit' }).click();
}));
