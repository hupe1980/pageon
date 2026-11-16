// WebMCP tools through Puppeteer's own `page.webmcp` (Chrome's WebMCP DevTools domain): a
// registered tool and a form tool (WebMCP's declarative API), listed and run. CDP only.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, html, cdpOnly } from '../lib/helper.mjs';

const page = `<!doctype html><title>Air</title>
<form id=f toolname="search_flights" tooldescription="Find flights between two airports" toolautosubmit>
  <label>From <input name=from required></label>
  <label>To <input name=to required></label>
  <button>Search</button>
</form>
<script>
document.modelContext.registerTool({
  name: 'add', description: 'Add two numbers',
  inputSchema: { type: 'object', properties: { a: { type: 'number' }, b: { type: 'number' } } },
  execute: ({ a, b }) => ({ sum: a + b }),
  annotations: { readOnlyHint: true },
});
f.addEventListener('submit', (e) => {
  e.preventDefault();
  if (e.agentInvoked) { const d = new FormData(f); e.respondWith(Promise.resolve({ route: d.get('from') + '-' + d.get('to') })); }
});
</script>`;

test('page.webmcp lists and runs registered and form tools', { skip: cdpOnly }, async () => {
  const s = await start(site([html('https://air.test/', page)]));
  try {
    const p = await s.browser.newPage();
    await p.goto('https://air.test/');
    const tools = p.webmcp.tools();
    const names = tools.map((t) => t.name).sort();
    assert.deepEqual(names, ['add', 'search_flights']);
    const add = tools.find((t) => t.name === 'add');
    assert.equal(add.annotations.readOnly, true);
    const r = await add.execute({ a: 2, b: 3 });
    assert.equal(r.status, 'Completed');
    assert.deepEqual(r.output, { sum: 5 });
    const form = tools.find((t) => t.name === 'search_flights');
    assert.equal(form.annotations.autosubmit, true);
    assert.deepEqual(Object.keys(form.inputSchema.properties), ['from', 'to']);
    assert.deepEqual(form.inputSchema.required, ['from', 'to']);
    const el = await form.formElement;
    assert.equal(await el.evaluate((e) => e.id), 'f');
    const f = await form.execute({ from: 'TXL', to: 'SFO' });
    assert.equal(f.status, 'Completed');
    assert.deepEqual(f.output, { route: 'TXL-SFO' });
    // A tool registered later is reported.
    const added = new Promise((resolve) => p.webmcp.once('toolsadded', resolve));
    await p.evaluate(() => document.modelContext.registerTool({ name: 'late', description: 'Later', execute: () => 'ok' }));
    const ev = await added;
    assert.deepEqual(ev.tools.map((t) => t.name), ['late']);
  } finally {
    await s.stop();
  }
});
