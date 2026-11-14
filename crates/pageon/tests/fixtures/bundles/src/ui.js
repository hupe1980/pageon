// Shared by the entry and the lazy route: bundlers put it in its own chunk.
export function h(tag, text, attrs = {}) {
  const el = document.createElement(tag);
  el.textContent = text;
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}
export function render(...els) {
  const root = document.getElementById('app');
  for (const el of els) root.append(el);
}
