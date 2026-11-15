import { double } from './lib.js';
self.onmessage = (e) => postMessage({ op: 'module', value: double(e.data), meta: import.meta.url.endsWith('/module.js') });
