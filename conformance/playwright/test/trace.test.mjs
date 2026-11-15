// A pageon recording exported with `pageon replay --trace` opens in Playwright's own trace
// viewer: every step is an action, with the page after it, the network and the console.
// The viewer runs in the Chromium playwright-core installs (skipped when it is missing).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, extname, join } from 'node:path';
import { chromium } from 'playwright-core';
import { bin } from '../lib/helper.mjs';

const viewer = join(dirname(createRequire(import.meta.url).resolve('playwright-core/package.json')), 'lib/vite/traceViewer');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.ttf': 'font/ttf', '.woff2': 'font/woff2', '.json': 'application/json' };

const page = `<!doctype html><title>Sign up</title><h1>Sign up</h1>
  <label>Name <input id=n></label><button id=go>Send</button><p id=out></p>
  <script>go.onclick = () => { out.textContent = 'Hello ' + n.value; console.log('sent', n.value); };</script>`;

/** Serves the viewer's files and `zip` at /trace.zip. */
function serve(zip) {
  const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://x').pathname;
    const file = path === '/trace.zip' ? zip : join(viewer, path === '/' ? 'index.html' : path);
    if (!existsSync(file)) { res.writeHead(404).end(); return; }
    res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
    res.end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

test('Playwright\'s trace viewer opens a pageon trace', { skip: !existsSync(chromium.executablePath()) && 'no Chromium for the viewer' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'pageon-trace-'));
  const file = join(dir, 'page.html');
  writeFileSync(file, page);
  const ep = join(dir, 'ep.json');
  const zip = join(dir, 'trace.zip');
  execFileSync(bin(), ['fetch', `file://${file}`, '--act', 'type e1 "Ada"', '--act', 'click e2', '--record', ep], { stdio: 'ignore' });
  execFileSync(bin(), ['replay', ep, '--trace', zip], { stdio: 'ignore' });

  const server = await serve(zip);
  const { port } = server.address();
  const browser = await chromium.launch();
  try {
    const p = await browser.newPage();
    await p.goto(`http://127.0.0.1:${port}/index.html?trace=http://127.0.0.1:${port}/trace.zip`);
    const titles = p.locator('.action-title');
    await titles.first().waitFor({ timeout: 15000 });
    const listed = await titles.allTextContents();
    assert.equal(listed.length, 3, listed.join(' | '));
    ['open file://', 'type e1 "Ada"', 'click e2'].forEach((t, k) => assert.ok(listed[k].includes(t), listed.join(' | ')));
    // The last action's page shows the answer.
    await titles.last().click();
    const frame = p.frameLocator('iframe.snapshot-visible[name=snapshot]');
    await assert.doesNotReject(frame.getByText('Hello Ada').waitFor({ timeout: 10000 }));
    // Console and network tabs carry the step's message and the page request.
    await p.getByRole('tab', { name: /Console/ }).click();
    await p.getByText('sent Ada').first().waitFor({ timeout: 5000 });
    await p.getByRole('tab', { name: /Network/ }).click();
    await p.getByText('page.html').first().waitFor({ timeout: 5000 });
  } finally {
    await browser.close();
    server.close();
  }
});
