// A page that poisons the built-ins automation helpers use, and the same page clean. Client
// helpers run in isolated worlds (utility worlds, BiDi sandboxes), so they must give the same
// answers on both.
import { html } from './pageon.mjs';

const body = `<h1>Checkout</h1><p id=t>Real text</p><button id=b onclick="this.firstChild.data='clicked'">Pay</button>`;

const poison = `<script>
  Array.prototype.map = function () { return ['poisoned'] };
  Array.prototype.filter = function () { return [] };
  Element.prototype.getBoundingClientRect = function () { return { x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 } };
  Element.prototype.getClientRects = function () { return [] };
  Object.defineProperty(Node.prototype, 'textContent', { get() { return 'lie' }, set() {} });
  window.getComputedStyle = () => ({ display: 'none', visibility: 'hidden', getPropertyValue: () => 'none' });
  JSON.stringify = () => '"lie"';
</script>`;

export const worldsSite = () => [
  html('https://clean.test/', `<!doctype html><title>Clean</title>${body}`),
  html('https://poisoned.test/', `<!doctype html><title>Poisoned</title>${body}${poison}`),
];
