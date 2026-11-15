// A classic worker: computation, timers, fetch, importScripts, transfer, errors, close.
importScripts('helpers.js');
const facts = {
  global: typeof WorkerGlobalScope === 'function' && self instanceof WorkerGlobalScope && self instanceof DedicatedWorkerGlobalScope,
  noDocument: typeof document === 'undefined' && typeof window === 'undefined',
  noNestedWorker: typeof Worker === 'undefined',
  name: self.name,
  location: location.href,
  origin: self.origin,
  helper: typeof helperSquare === 'function' && helperSquare(7) === 49,
  ua: typeof navigator.userAgent === 'string' && navigator.userAgent.length > 0,
  apis: typeof indexedDB.open === 'function' && typeof ReadableStream === 'function'
    && new TextEncoder().encode('ab').length === 2 && crypto.getRandomValues(new Uint8Array(2)).length === 2
    && typeof structuredClone === 'function' && typeof performance.now() === 'number',
};
onmessage = async (e) => {
  const m = e.data;
  if (m.op === 'facts') postMessage({ op: 'facts', facts });
  else if (m.op === 'sum') postMessage({ op: 'sum', value: m.values.reduce((a, b) => a + b, 0) });
  else if (m.op === 'timer') setTimeout(() => postMessage({ op: 'timer', at: Date.now() - m.at >= 50 }), 50);
  else if (m.op === 'fetch') postMessage({ op: 'fetch', text: await (await fetch('data.txt')).text() });
  else if (m.op === 'buffer') {
    const u8 = new Uint8Array(m.buffer);
    for (let i = 0; i < u8.length; i++) u8[i] *= 2;
    postMessage({ op: 'buffer', buffer: m.buffer }, [m.buffer]);
  } else if (m.op === 'throw') setTimeout(() => { throw new Error('worker boom'); });
  else if (m.op === 'close') { close(); postMessage({ op: 'after-close' }); }
};
