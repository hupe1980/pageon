// The webpack build: the lazy route is a JSONP chunk loaded by an inserted <script>.
import { h, render } from './ui.js';
const open = h('a', 'Open route', { href: '#route', id: 'open' });
open.addEventListener('click', (e) => {
  e.preventDefault();
  import(/* webpackChunkName: "route" */ './route.js').then(
    (route) => route.show(),
    (err) => render(h('p', `Could not load the route: ${err.message}`, { id: 'failed' })),
  );
});
render(h('h1', 'Webpack shop'), open);
