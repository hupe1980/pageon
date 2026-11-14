// Form validation and form-associated custom elements through Playwright.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { formsSite } from '../../lib/forms.mjs';

let s, page;
before(async () => {
  s = await start(site(formsSite()));
  page = await s.context.newPage();
  await page.goto('https://forms.test/');
});
after(async () => s && s.stop());

test('validity, :invalid and a component form', async () => {
  assert.equal(await page.locator('#email').evaluate((e) => e.validity.valid), false);
  assert.equal(await page.locator('input:invalid').count(), 1);
  await page.locator('#go').click();
  assert.equal(page.url(), 'https://forms.test/');
  await page.locator('#email').fill('a@b.test');
  await page.locator('#agree').click();
  await page.locator('#go').click();
  await page.waitForURL('https://forms.test/done?email=a%40b.test&agree=yes');
});
