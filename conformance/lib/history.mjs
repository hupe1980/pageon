// A single-page app that routes with the History API (as React Router and Vue Router do).
import { html } from './pageon.mjs';

const app = `<!doctype html><title>Shop</title>
<nav><a id=orders href="/orders" data-link>Orders</a> <a id=settings href="/settings" data-link>Settings</a></nav>
<main id=view>Home</main>
<script>
  const render = () => { view.textContent = location.pathname === '/' ? 'Home' : 'Page ' + location.pathname; };
  document.addEventListener('click', (e) => {
    const a = e.target.closest('a[data-link]');
    if (!a) return;
    e.preventDefault();
    history.pushState({ path: a.getAttribute('href') }, '', a.getAttribute('href'));
    render();
  });
  addEventListener('popstate', render);
  render();
</script>`;

export const historySite = () => [html('https://spa.test/', app)];
