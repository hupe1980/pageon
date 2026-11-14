// Form validation and form-associated custom elements through Puppeteer (CDP and BiDi).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site } from '../lib/helper.mjs';
import { formsSite } from '../../lib/forms.mjs';

test('validity, :invalid and a blocked submission, then a component form submits', async () => {
  const s = await start(site(formsSite()));
  try {
    const page = await s.browser.newPage();
    await page.goto('https://forms.test/');
    assert.equal(await page.$eval('#email', (e) => e.validity.valueMissing), true);
    assert.equal(await page.$eval('#email', (e) => e.validationMessage), 'Please fill out this field.');
    assert.deepEqual(await page.$$eval(':invalid', (l) => l.map((e) => e.id)), ['f', 'email', 'agree']);
    await page.click('#go');
    assert.equal(page.url(), 'https://forms.test/', 'the invalid form is not submitted');
    assert.equal(await page.$eval('#email', (e) => e.matches(':user-invalid')), true);
    await page.type('#email', 'a@b.test');
    await page.click('#agree');
    assert.equal(await page.$eval('#agree', (e) => e.matches(':state(on)')), true);
    await Promise.all([page.waitForNavigation(), page.click('#go')]);
    assert.equal(page.url(), 'https://forms.test/done?email=a%40b.test&agree=yes');
  } finally {
    await s.stop();
  }
});

test('uploadFile with a staged file; other paths are refused', async () => {
  const { mkdtempSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'pageon-up-'));
  const cv = join(dir, 'cv.txt');
  writeFileSync(cv, 'hello cv');
  const s = await start(site(formsSite()), { args: ['--file', `cv@https://forms.test=${cv}`] });
  try {
    const page = await s.browser.newPage();
    await page.goto('https://forms.test/upload');
    const input = await page.$('#f');
    await input.uploadFile(cv);
    await page.waitForFunction(() => document.querySelector('#out').textContent !== '');
    assert.equal(await page.$eval('#out', (e) => e.textContent), 'cv.txt:hello cv');
    await assert.rejects(input.uploadFile(join(dir, 'other.txt')), /not staged|--file/);
  } finally {
    await s.stop();
  }
});
