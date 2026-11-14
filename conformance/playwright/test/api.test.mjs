// The everyday Playwright API over connectOverCDP against `pageon cdp`.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright-core';
import { start, site, html } from '../lib/helper.mjs';

const form = `<title>Login</title>
  <form method=post action=/session>
    <label>User <input name=user></label>
    <label for=pw>Password</label><input id=pw name=pw type=password placeholder="Your password">
    <label><input type=checkbox name=remember> Remember me</label>
    <select name=lang aria-label=Language><option value=en>English</option><option value=de>Deutsch</option></select>
    <button>Sign in</button>
  </form>`;

const pages = () => site([
  html('https://shop.test/', `<!doctype html><title>Shop</title>
    <h1>Welcome</h1>
    <nav><a href=/login>Sign in</a> <a href=/about data-testid=about>About</a></nav>
    <ul><li class=item>apple</li><li class=item>pear <b>ripe</b></li><li class=item>plum</li></ul>
    <button id=b onclick="this.textContent='clicked'">Press</button>
    <div id=hover onmouseover="this.textContent='hovered'">hover me</div>
    <div id=hidden style=display:none>secret</div>
    <input id=q aria-label=Search>
    <div id=late></div>
    <script>
      setTimeout(() => { late.innerHTML = '<span class=late>late</span>' }, 400);
      document.cookie = 'seen=1; path=/';
      console.log('shop ready');
      fetch('/api/items').then(r => r.json()).then(j => { document.body.dataset.items = j.length });
    </script>`),
  { method: 'GET', url: 'https://shop.test/api/items', status: 200, headers: [['content-type', 'application/json']], body: '[1,2,3]' },
  html('https://shop.test/login', form),
  { method: 'POST', url: 'https://shop.test/session', status: 200, headers: [['content-type', 'text/html']], body: '<title>Account</title><h1>Hello</h1>' },
  html('https://shop.test/login', form),
  html('https://shop.test/session', '<title>Account</title><h1>Hello again</h1>'),
  html('https://shop.test/login', form),
  html('https://shop.test/session', '<title>Account</title><h1>Hello again</h1>'),
  html('https://shop.test/errors', `<title>Errors</title><script>
    console.warn('careful'); setTimeout(() => { throw new Error('later') }, 10);
  </script>`),
  html('https://shop.test/reload', '<title>R</title><h1>First</h1>'),
  html('https://shop.test/reload', '<title>R</title><h1>Second</h1>'),
  html('https://shop.test/init', `<p id=seen></p><script>seen.textContent = 'sees ' + window.stub</script>`),
  html('https://shop.test/init', `<p id=seen></p><script>seen.textContent = 'sees ' + window.stub</script>`),
]);

let s, page;
before(async () => {
  s = await start(pages(), { args: ['--secret', 'pw@https://shop.test'], env: { PAGEON_SECRET_PW: 'very-secret-pw' } });
  page = await s.context.newPage();
  await page.goto('https://shop.test/');
});
after(async () => s && s.stop());

test('connectOverCDP: one default context, version, pages', async () => {
  assert.equal(s.browser.contexts().length, 1);
  // Playwright reports the product's version part (`pageon/0.0.1` → `0.0.1`).
  assert.match(s.browser.version(), /^\d+\.\d+/);
  assert.ok(s.context.pages().includes(page));
});

test('connect by browserURL (http), with --discovery', async () => {
  const t = await start(site([html('https://a.test/', '<title>A</title>')]), { connect: false, args: ['--discovery'] });
  try {
    const b = await chromium.connectOverCDP(t.url);
    const p = await b.contexts()[0].newPage();
    await p.goto('https://a.test/');
    assert.equal(await p.title(), 'A');
    await b.close();
  } finally {
    await t.stop();
  }
});

test('without --discovery the endpoint is not discoverable over HTTP', async () => {
  const t = await start(site([html('https://a.test/', '<title>A</title>')]), { connect: false });
  try {
    const r = await fetch(`${t.url}/json/version`);
    assert.equal(r.status, 404);
    assert.match(await r.text(), /--discovery/);
  } finally {
    await t.stop();
  }
});

test('goto returns the response; title, url, content', async () => {
  assert.equal(page.url(), 'https://shop.test/');
  assert.equal(await page.title(), 'Shop');
  assert.match(await page.content(), /<h1>Welcome<\/h1>/);
});

test('evaluate and evaluateHandle', async () => {
  assert.equal(await page.evaluate(([a, b]) => a * b, [6, 7]), 42);
  assert.deepEqual(await page.evaluate(() => ({ n: null, d: [1, 'x'] })), { n: null, d: [1, 'x'] });
  assert.ok(Number.isNaN(await page.evaluate(() => NaN)));
  const h = await page.evaluateHandle(() => document.body);
  assert.equal(await h.evaluate((b) => b.tagName), 'BODY');
  await h.dispose();
  await assert.rejects(page.evaluate(() => { throw new Error('boom'); }), /boom/);
});

test('locator: textContent, innerText, count, nth, filter', async () => {
  const items = page.locator('.item');
  assert.equal(await items.count(), 3);
  assert.equal(await items.nth(1).textContent(), 'pear ripe');
  assert.equal(await items.filter({ hasText: 'plum' }).textContent(), 'plum');
  assert.equal(await page.locator('h1').innerText(), 'Welcome');
  assert.deepEqual(await items.allTextContents(), ['apple', 'pear ripe', 'plum']);
});

test('getByRole, getByText, getByTestId', async () => {
  assert.equal(await page.getByRole('heading', { name: 'Welcome' }).count(), 1);
  assert.equal(await page.getByRole('link', { name: 'Sign in' }).getAttribute('href'), '/login');
  assert.equal(await page.getByText('ripe').textContent(), 'ripe');
  assert.equal(await page.getByTestId('about').textContent(), 'About');
});

test('isVisible and isHidden from layout', async () => {
  assert.equal(await page.locator('h1').isVisible(), true);
  assert.equal(await page.locator('#hidden').isVisible(), false);
  assert.equal(await page.locator('#hidden').isHidden(), true);
});

test('click and hover', async () => {
  await page.locator('#b').click();
  assert.equal(await page.locator('#b').textContent(), 'clicked');
  await page.locator('#hover').hover();
  assert.equal(await page.locator('#hover').textContent(), 'hovered');
});

test('fill, pressSequentially, press, inputValue', async () => {
  const q = page.getByLabel('Search');
  await q.fill('pears');
  assert.equal(await q.inputValue(), 'pears');
  await q.press('Backspace');
  await q.pressSequentially('rs');
  assert.equal(await q.inputValue(), 'pearrs');
  await q.fill('');
  assert.equal(await q.inputValue(), '');
});

test('click on a hidden element fails with an actionability error', async () => {
  await assert.rejects(page.locator('#hidden').click({ timeout: 300 }), /not visible|Timeout/);
});

test('waitForSelector: polling sees the timers fire as the wall clock passes', async () => {
  // Playwright polls from the driver; while it waits, the page's clock follows real time.
  const t = Date.now();
  await page.waitForSelector('.late');
  assert.ok(Date.now() - t < 2000, `took ${Date.now() - t} ms`);
  assert.equal(await page.locator('.late').textContent(), 'late');
});

test('waitForFunction', async () => {
  const h = await page.waitForFunction(() => document.body.dataset.items === '3');
  assert.equal(await h.jsonValue(), true);
});

test('cookies: from the page, add, clear', async () => {
  assert.ok((await s.context.cookies()).some((c) => c.name === 'seen'));
  await s.context.addCookies([{ name: 'k', value: 'v', url: 'https://shop.test/' }]);
  assert.equal(await page.evaluate(() => document.cookie.includes('k=v')), true);
  await s.context.addCookies([{ name: 'sid', value: 'hidden', url: 'https://shop.test/', httpOnly: true }]);
  assert.equal((await s.context.cookies()).find((c) => c.name === 'sid')?.httpOnly, true);
  assert.equal(await page.evaluate(() => document.cookie.includes('sid')), false);
  await s.context.clearCookies();
  assert.equal((await s.context.cookies()).length, 0);
});

test('setViewportSize is accepted', async () => {
  await page.setViewportSize({ width: 800, height: 600 });
});

test('forms: getByLabel, getByPlaceholder, check, selectOption, submit, waitForURL', async () => {
  const p = await s.context.newPage();
  await p.goto('https://shop.test/login');
  await p.getByLabel('User').fill('ada');
  await p.getByPlaceholder('Your password').fill('pw');
  await p.getByLabel('Remember me').check();
  assert.equal(await p.getByLabel('Remember me').isChecked(), true);
  assert.deepEqual(await p.getByLabel('Language').selectOption('de'), ['de']);
  await p.getByRole('button', { name: 'Sign in' }).click();
  await p.waitForURL('**/session');
  assert.equal(await p.locator('h1').textContent(), 'Hello');
  await p.close();
});

test('history: goBack, goForward', async () => {
  const p = await s.context.newPage();
  await p.goto('https://shop.test/login');
  await p.goto('https://shop.test/session');
  await p.goBack();
  assert.equal(p.url(), 'https://shop.test/login');
  await p.goForward();
  assert.equal(p.url(), 'https://shop.test/session');
  await p.close();
});

test('reload and waitForLoadState', async () => {
  const p = await s.context.newPage();
  await p.goto('https://shop.test/reload');
  await p.reload();
  await p.waitForLoadState('networkidle');
  assert.equal(await p.locator('h1').textContent(), 'Second');
  await p.close();
});

test('request, response, console and pageerror events', async () => {
  const p = await s.context.newPage();
  const seen = [];
  const logs = [];
  const errors = [];
  p.on('request', (r) => seen.push(r.url()));
  p.on('response', (r) => seen.push(`${r.status()} ${r.url()}`));
  p.on('console', (m) => logs.push(`${m.type()} ${m.text()}`));
  p.on('pageerror', (e) => errors.push(e.message));
  const res = await p.goto('https://shop.test/errors');
  assert.equal(res.status(), 200);
  assert.ok(seen.includes('https://shop.test/errors') && seen.includes('200 https://shop.test/errors'), seen.join('\n'));
  assert.deepEqual(logs, ['warning careful']);
  assert.deepEqual(errors, ['later']);
  await p.close();
});

test('newPage and close', async () => {
  const before = s.context.pages().length;
  const p = await s.context.newPage();
  assert.equal(s.context.pages().length, before + 1);
  assert.equal(p.url(), 'about:blank');
  await p.close();
  assert.equal(s.context.pages().length, before);
});

test('unsupported: pdf fails at once and says what to do', async () => {
  const t = Date.now();
  await assert.rejects(page.pdf(), /printToPDF.*captureScreenshot/s);
  assert.ok(Date.now() - t < 1000);
});

test('Pageon.observe and Pageon.act through a CDP session', async () => {
  const cdp = await s.context.newCDPSession(page);
  const { observation } = await cdp.send('Pageon.observe', { full: true });
  assert.match(observation, /^@page v\d+ "Shop" https:\/\/shop\.test\//m);
  const search = /textbox (e\d+) "Search"/.exec(observation)[1];
  await cdp.send('Pageon.act', { action: `type ${search} secret://pw` });
  const after = await cdp.send('Pageon.observe');
  assert.ok(!JSON.stringify(after).includes('very-secret-pw'));
  await cdp.detach();
});

test('fill replaces a value; keyboard.type types into the focused field', async () => {
  const q = page.getByLabel('Search');
  await q.fill('first');
  await q.fill('second');
  assert.equal(await q.inputValue(), 'second');
  await q.fill('');
  await q.focus();
  await page.keyboard.type('typed');
  assert.equal(await q.inputValue(), 'typed');
});

test('locator.evaluate and evaluateAll', async () => {
  assert.equal(await page.locator('h1').evaluate((e) => e.tagName), 'H1');
  assert.deepEqual(await page.locator('.item').evaluateAll((els) => els.length), 3);
});

test('addInitScript runs before the page', async () => {
  const p = await page.context().newPage();
  await p.addInitScript((v) => { window.stub = v; }, 'stubbed');
  await p.goto('https://shop.test/init');
  assert.equal(await p.locator('#seen').textContent(), 'sees stubbed');
  await p.close();
});
