// The esbuild build: top-level await, import.meta.url, and a lazy route via import().
import { h, render } from './ui.js';
const config = await new Promise((r) => setTimeout(() => r({ title: 'Module shop' }), 10));
const logo = new URL('./logo.svg', import.meta.url).href;
const open = h('a', 'Open route', { href: '#route', id: 'open' });
open.addEventListener('click', async (e) => {
  e.preventDefault();
  const route = await import('./route.js');
  route.show();
});
render(h('h1', config.title), h('img', '', { src: logo, alt: 'Logo' }), open);
