importScripts('comlink.min.js');
Comlink.expose({
  add(a, b) { return a + b; },
  async later(x) { await new Promise((r) => setTimeout(r, 30)); return x.toUpperCase(); },
  fail() { throw new Error('nope'); },
});
