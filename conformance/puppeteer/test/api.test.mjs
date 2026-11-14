// The everyday Puppeteer API against `pageon cdp`.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import puppeteer from 'puppeteer-core';
import { start, site, html, cdpOnly, bidi } from '../lib/helper.mjs';

const pages = () => site([
  html('https://shop.test/', `<!doctype html><title>Shop</title>
    <h1 id=h>Welcome</h1>
    <a id=login href=/login>Sign in</a>
    <ul><li class=item>apple</li><li class=item>pear</li><li class=item>plum</li></ul>
    <select id=size><option value=s>Small</option><option value=m>Medium</option></select>
    <button id=b onclick="this.textContent='clicked'">Press</button>
    <input id=q aria-label=Search>
    <div id=hover onmouseover="this.textContent='hovered'">hover me</div>
    <div id=late></div>
    <script>
      setTimeout(() => { late.innerHTML = '<span class=late>late</span>' }, 5000);
      document.cookie = 'seen=1; path=/';
      fetch('/api/items').then(r => r.json()).then(j => { document.body.dataset.items = j.length });
    </script>`),
  { method: 'GET', url: 'https://shop.test/api/items', status: 200, headers: [['content-type', 'application/json']], body: '[1,2,3]' },
  html('https://shop.test/login', `<title>Login</title>
    <form method=post action=/session><input id=user name=user><input id=pw name=pw type=password>
    <button id=go>Sign in</button></form>`),
  { method: 'POST', url: 'https://shop.test/session', status: 200, headers: [['content-type', 'text/html']], body: '<title>Account</title><h1>Hello</h1>' },
  html('https://shop.test/login', '<title>Login</title><h1>Login again</h1>'),
  html('https://shop.test/session', '<title>Account</title><h1>Hello</h1>'),
  html('https://shop.test/login', `<title>Login</title>
    <form method=post action=/session><input id=user name=user aria-label=User>
    <input id=pw name=pw type=password aria-label=Password><button id=go>Sign in</button></form>`),
  html('https://shop.test/', '<title>Shop</title><h1>Again</h1>'),
  html('https://shop.test/', '<title>Shop</title><h1>Reloaded</h1>'),
  html('https://shop.test/console', `<title>Console</title><script>
    console.log('hello', 1); console.warn('careful');
    setTimeout(() => { throw new Error('later') }, 10);
  </script>`),
  html('https://shop.test/hide', `<div id=spin>loading</div>
    <script>setTimeout(() => { spin.style.display = 'none'; window.ready = true }, 3000)</script>`),
  html('https://shop.test/form', `<form action=/done><input id=f name=f></form>`),
  html('https://shop.test/done?f=hi', '<title>Done</title><p id=r>done</p>'),
]);

let s, page;
before(async () => {
  s = await start(pages(), { args: ['--secret', 'pw@https://shop.test'], env: { PAGEON_SECRET_PW: 'very-secret-pw' } });
  page = await s.browser.newPage();
});
after(async () => s && s.stop());

test('discovery: /json/version, /json/list, browserURL', { skip: cdpOnly }, async () => {
  const v = await (await fetch(`${s.url}/json/version`)).json();
  assert.match(v.Browser, /^pageon\//);
  assert.equal(v.webSocketDebuggerUrl, s.wsUrl);
  const list = await (await fetch(`${s.url}/json/list`)).json();
  assert.ok(list.length >= 1 && list.every((t) => t.type === 'page'));
  assert.match(await s.browser.version(), /^pageon\//);
  assert.match(await s.browser.userAgent(), /pageon/);
});

test('goto returns the response; url, title, content', async () => {
  const res = await page.goto('https://shop.test/');
  assert.equal(res.status(), 200);
  assert.equal(res.url(), 'https://shop.test/');
  assert.ok(res.ok());
  assert.equal(page.url(), 'https://shop.test/');
  assert.equal(await page.title(), 'Shop');
  assert.match(await page.content(), /<h1 id="h">Welcome<\/h1>/);
});

test('evaluate: values, by value and unserializable', async () => {
  assert.equal(await page.evaluate(() => 1 + 2), 3);
  assert.equal(await page.evaluate(() => 'x'.repeat(3)), 'xxx');
  assert.deepEqual(await page.evaluate(() => ({ a: [1, { b: true }], c: null })), { a: [1, { b: true }], c: null });
  assert.equal(await page.evaluate(() => undefined), undefined);
  assert.ok(Number.isNaN(await page.evaluate(() => NaN)));
  assert.ok(Object.is(await page.evaluate(() => -0), -0));
  assert.equal(await page.evaluate(() => Infinity), Infinity);
  assert.equal(await page.evaluate(() => 2n ** 70n), 2n ** 70n);
  assert.equal(await page.evaluate((a, b) => a * b, 6, 7), 42);
  assert.equal(await page.evaluate((x) => typeof x, undefined), 'undefined');
  assert.equal(await page.evaluate('document.title'), 'Shop');
});

test('evaluate: promises, async functions and errors', async () => {
  assert.equal(await page.evaluate(async () => { await null; return 'done'; }), 'done');
  assert.equal(await page.evaluate(() => new Promise((r) => setTimeout(() => r('timer'), 300))), 'timer');
  await assert.rejects(page.evaluate(() => { throw new Error('boom'); }), /boom/);
  await assert.rejects(page.evaluate(() => Promise.reject(new TypeError('nope'))), /nope/);
});

test('handles: evaluateHandle, getProperty, jsonValue, dispose', async () => {
  const h = await page.evaluateHandle(() => ({ x: 1, y: [2, 3] }));
  assert.equal(await (await h.getProperty('x')).jsonValue(), 1);
  const props = await h.getProperties();
  assert.deepEqual([...props.keys()].sort(), ['x', 'y']);
  assert.deepEqual(await h.jsonValue(), { x: 1, y: [2, 3] });
  await h.dispose();
  const doc = await page.evaluateHandle(() => document);
  assert.equal(await page.evaluate((d) => d.title, doc), 'Shop');
});

test('selectors: $, $$, $eval, $$eval, text', async () => {
  const h1 = await page.$('#h');
  assert.ok(h1);
  assert.equal(await h1.evaluate((e) => e.textContent), 'Welcome');
  assert.equal((await page.$$('.item')).length, 3);
  assert.equal(await page.$eval('#login', (a) => a.getAttribute('href')), '/login');
  assert.deepEqual(await page.$$eval('.item', (els) => els.map((e) => e.textContent)), ['apple', 'pear', 'plum']);
  assert.equal(await page.$('#missing'), null);
  const pear = await page.$('::-p-text(pear)');
  assert.equal(await pear.evaluate((e) => e.textContent), 'pear');
});

test('waitForSelector: present at once; a 5 s timer resolves in virtual time', async () => {
  assert.ok(await page.waitForSelector('#h'));
  const t = Date.now();
  const late = await page.waitForSelector('.late');
  assert.equal(await late.evaluate((e) => e.textContent), 'late');
  assert.ok(Date.now() - t < 1000, `took ${Date.now() - t} ms`);
  assert.equal(await page.waitForFunction(() => document.body.dataset.items === '3').then((h) => h.jsonValue()), true);
});

test('click, hover, focus, type, select', async () => {
  await page.click('#b');
  assert.equal(await page.$eval('#b', (e) => e.textContent), 'clicked');
  await page.hover('#hover');
  assert.equal(await page.$eval('#hover', (e) => e.textContent), 'hovered');
  await page.focus('#q');
  assert.equal(await page.evaluate(() => document.activeElement.id), 'q');
  await page.type('#q', 'pears');
  assert.equal(await page.$eval('#q', (e) => e.value), 'pears');
  await page.keyboard.press('Backspace');
  assert.equal(await page.$eval('#q', (e) => e.value), 'pear');
  assert.deepEqual(await page.select('#size', 'm'), ['m']);
  assert.equal(await page.$eval('#size', (e) => e.value), 'm');
});

test('mouse at coordinates and bounding boxes', async () => {
  await page.evaluate(() => { document.querySelector('#b').textContent = 'Press'; });
  const box = await (await page.$('#b')).boundingBox();
  assert.ok(box && box.width > 0 && box.height > 0);
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  assert.equal(await page.$eval('#b', (e) => e.textContent), 'clicked');
});

test('cookies: from the page, set, delete', async () => {
  const names = (await page.cookies()).map((c) => c.name);
  assert.ok(names.includes('seen'));
  await page.setCookie({ name: 'k', value: 'v' });
  assert.equal((await page.cookies()).find((c) => c.name === 'k')?.value, 'v');
  assert.equal(await page.evaluate(() => document.cookie.includes('k=v')), true);
  await page.deleteCookie({ name: 'k' });
  assert.equal((await page.cookies()).find((c) => c.name === 'k'), undefined);
});

test('setViewport is accepted', async () => {
  await page.setViewport({ width: 800, height: 600 });
});

test('navigation: click a link, form with Enter, back, forward, reload', async () => {
  await Promise.all([page.waitForNavigation(), page.click('#login')]);
  assert.equal(page.url(), 'https://shop.test/login');
  assert.equal(await page.title(), 'Login');
  await page.type('#user', 'ada');
  await page.focus('#pw');
  await Promise.all([page.waitForNavigation(), page.keyboard.press('Enter')]);
  assert.equal(page.url(), 'https://shop.test/session');
  assert.equal(await page.$eval('h1', (e) => e.textContent), 'Hello');
  await page.goBack();
  assert.equal(page.url(), 'https://shop.test/login');
  await page.goForward();
  assert.equal(page.url(), 'https://shop.test/session');
});

test('request and response events', async () => {
  const p = await s.browser.newPage();
  const seen = [];
  p.on('request', (r) => seen.push(`${r.method()} ${r.url()}`));
  const statuses = [];
  p.on('response', (r) => statuses.push(r.status()));
  await p.goto('https://shop.test/');
  assert.ok(seen.includes('GET https://shop.test/'), seen.join('\n'));
  assert.ok(statuses.includes(200));
  await p.reload();
  assert.equal(await p.$eval('h1', (e) => e.textContent), 'Reloaded');
  await p.close();
});

test('pages: newPage, pages, close', async () => {
  const before = (await s.browser.pages()).length;
  const p = await s.browser.newPage();
  assert.equal((await s.browser.pages()).length, before + 1);
  assert.equal(p.url(), 'about:blank');
  assert.equal(await p.evaluate(() => 1), 1);
  await p.close();
  assert.equal((await s.browser.pages()).length, before);
});

test('unsupported methods fail at once and say what to do', { skip: cdpOnly }, async () => {
  const t = Date.now();
  await assert.rejects(page.setUserAgent('Mozilla/5.0'), /pageon/);
  await assert.rejects(page.evaluateOnNewDocument(() => { window.x = 1; }), /before the page's own scripts/);
  const cdp = await page.createCDPSession();
  await assert.rejects(cdp.send('Page.printToPDF'), /printToPDF.*captureScreenshot/s);
  await assert.rejects(cdp.send('Animation.enable'), /'Animation\.enable' wasn't found/);
  assert.ok(Date.now() - t < 1000);
});

test('Pageon.observe and Pageon.act share the page with Puppeteer', { skip: cdpOnly }, async () => {
  const p = await s.browser.newPage();
  await p.goto('https://shop.test/login');
  const cdp = await p.createCDPSession();
  const { observation } = await cdp.send('Pageon.observe');
  // Outcomes since the last observation come first, then the page.
  assert.match(observation, /^ok goto https:\/\/shop\.test\/login/);
  assert.match(observation, /^@page v1 "Login" https:\/\/shop\.test\/login/m);
  const user = /textbox (e\d+)/.exec(observation)[1];
  const { outcome } = await cdp.send('Pageon.act', { action: `type ${user} ada` });
  assert.match(outcome, /^ok type/);
  assert.equal(await p.$eval('#user', (e) => e.value), 'ada');
  const err = await cdp.send('Pageon.act', { action: 'click e999' }).catch((e) => e);
  assert.match(err.message, /e999/);
  // A secret typed through pageon never shows in what pageon sends.
  const pw = /textbox (e\d+) "pw"|textbox (e\d+)\s*$/m.exec(observation);
  const ref = (await cdp.send('Pageon.observe', { full: true })).observation.match(/textbox (e\d+)/g)[1].split(' ')[1];
  const typed = await cdp.send('Pageon.act', { action: `type ${ref} secret://pw` });
  const after = await cdp.send('Pageon.observe', { full: true });
  assert.ok(!JSON.stringify([typed, after]).includes('very-secret-pw'), 'the secret never reaches the client');
  assert.ok(pw !== undefined);
  await p.close();
});

test('browser.close ends the server', async () => {
  const t = await start(site([html('https://a.test/', '<p>a</p>')]));
  await t.browser.close();
  assert.equal(await t.exited, 0);
});

test('a second client is refused while one is connected', async () => {
  // Puppeteer rejects with the WebSocket's error event, not an `Error`.
  const err = await puppeteer.connect({ browserWSEndpoint: s.wsUrl }).then(() => null, (e) => e);
  assert.ok(err, 'the second connect must fail');
  assert.match(String(err.message), /409/);
});

test('console and pageerror events', async () => {
  const p = await s.browser.newPage();
  const logs = [];
  const errors = [];
  p.on('console', (m) => logs.push(`${m.type()} ${m.text()}`));
  p.on('pageerror', (e) => errors.push(e.message));
  await p.goto('https://shop.test/console');
  assert.deepEqual(logs, ['log hello 1', 'warn careful']);
  assert.deepEqual(errors, ['later']);
  await p.close();
});

test('waitForSelector hidden, waitForFunction with raf polling', async () => {
  const p = await s.browser.newPage();
  await p.goto('https://shop.test/hide');
  const t = Date.now();
  await p.waitForSelector('#spin', { hidden: true });
  assert.equal(await p.evaluate(() => window.ready), true);
  assert.ok(Date.now() - t < 1000, `took ${Date.now() - t} ms`);
  await p.close();
});

test('keyboard.type and ElementHandle.type/press submit a form', async () => {
  const p = await s.browser.newPage();
  await p.goto('https://shop.test/form');
  const f = await p.$('#f');
  await f.focus();
  await p.keyboard.type('h');
  await f.type('i');
  assert.equal(await f.evaluate((e) => e.value), 'hi');
  await Promise.all([p.waitForNavigation(), f.press('Enter')]);
  assert.equal(p.url(), 'https://shop.test/done?f=hi');
  await p.close();
});

test('nodes by value and as handles', async () => {
  // Puppeteer's own deserializers differ: CDP gives {}, BiDi undefined (as with Chrome).
  assert.deepEqual(await page.evaluate(() => document.body), bidi ? undefined : {});
  const h = await page.evaluateHandle(() => document.querySelector('h1'));
  assert.ok(h.asElement());
  assert.equal(String(h), 'JSHandle@node');
  const arr = await page.evaluateHandle(() => [1, 2]);
  assert.equal(String(arr), 'JSHandle@array');
});

test('$eval on a missing element and goto to an unknown page reject clearly', async () => {
  await assert.rejects(page.$eval('#nope', (e) => e.id), /failed to find element matching selector "#nope"/);
  const p = await s.browser.newPage();
  await assert.rejects(p.goto('https://nowhere.test/'), /net::ERR_FAILED.*nowhere\.test/s);
  await p.close();
});

test('frames: one main frame with the page URL', async () => {
  const frames = page.frames();
  assert.equal(frames.length, 1);
  assert.equal(frames[0], page.mainFrame());
  assert.equal(page.mainFrame().url(), page.url());
});
