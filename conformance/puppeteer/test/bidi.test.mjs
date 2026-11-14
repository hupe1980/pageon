// A plain WebDriver BiDi client: a WebSocket and JSON, no library.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { start, site, html } from '../lib/helper.mjs';

/** A minimal BiDi client: `send` resolves with the reply; events are collected. */
async function client(url) {
  const ws = new WebSocket(url);
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail; });
  const waiting = new Map();
  const events = [];
  let next = 0;
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.type === 'event') events.push(msg);
    else waiting.get(msg.id)?.(msg);
  };
  return {
    events,
    send(method, params = {}) {
      const id = ++next;
      return new Promise((ok) => {
        waiting.set(id, ok);
        ws.send(JSON.stringify({ id, method, params }));
      });
    },
    close: () => ws.close(),
  };
}

test('a raw BiDi client: session, events, navigation, scripts, the pageon extension', async () => {
  const s = await start(site([
    html('https://shop.test/', '<title>Shop</title><button onclick="this.textContent=\'done\'">Buy</button>'),
  ]), { connect: false });
  const c = await client(s.bidiUrl);
  try {
    assert.equal((await c.send('session.status')).result.ready, true);
    const n = await c.send('session.new', { capabilities: {} });
    assert.equal(n.result.capabilities.browserName, 'pageon');
    await c.send('session.subscribe', { events: ['browsingContext.load'] });
    const tree = await c.send('browsingContext.getTree');
    const context = tree.result.contexts[0].context;
    const nav = await c.send('browsingContext.navigate', { context, url: 'https://shop.test/', wait: 'complete' });
    assert.equal(nav.type, 'success');
    assert.ok(c.events.some((e) => e.method === 'browsingContext.load' && e.params.navigation === nav.result.navigation));
    const r = await c.send('script.evaluate', { expression: 'document.title', target: { context }, awaitPromise: false });
    assert.deepEqual(r.result.result, { type: 'string', value: 'Shop' });
    // The agent's view and actions, on the page the client sees.
    const o = await c.send('pageon:observe', { context });
    const ref = /button (e\d+) "Buy"/.exec(o.result.observation)?.[1];
    assert.ok(ref, o.result.observation);
    await c.send('pageon:act', { context, action: `click ${ref}` });
    const t = await c.send('script.evaluate', { expression: 'document.querySelector("button").textContent', target: { context }, awaitPromise: false });
    assert.equal(t.result.result.value, 'done');
    // What pageon lacks fails at once, saying what to use.
    const e = await c.send('network.addIntercept', { phases: ['beforeRequestSent'] });
    assert.equal(e.error, 'unsupported operation');
    assert.match(e.message, /--guard/);
    await c.send('session.end');
  } finally {
    c.close();
    await s.stop();
  }
});
