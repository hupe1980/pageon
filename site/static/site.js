// pageon site: theme, copy buttons, tabs, search, docs menu and TOC highlighting. No dependencies
// except Zola's elasticlunr search index.
(() => {
  const $ = (s, el = document) => el.querySelector(s);
  const $$ = (s, el = document) => [...el.querySelectorAll(s)];

  // Theme: follows the system until toggled; the choice is remembered.
  $('[data-theme-toggle]')?.addEventListener('click', () => {
    const root = document.documentElement;
    const dark = root.dataset.theme
      ? root.dataset.theme === 'dark'
      : matchMedia('(prefers-color-scheme: dark)').matches;
    root.dataset.theme = dark ? 'light' : 'dark';
    try { localStorage.setItem('theme', root.dataset.theme); } catch (e) {}
  });

  // Copy buttons: on the install line and on every code block.
  const copy = async (btn, text) => {
    try {
      await navigator.clipboard.writeText(text.trim());
      const label = btn.textContent;
      btn.textContent = 'Copied';
      setTimeout(() => { btn.textContent = label; }, 1400);
    } catch (e) {}
  };
  $$('[data-copy]').forEach((b) => b.addEventListener('click', () => {
    copy(b, b.closest('[data-copy-wrap]').querySelector('code').textContent);
  }));
  $$('pre > code').forEach((code) => {
    const b = document.createElement('button');
    b.className = 'copy';
    b.type = 'button';
    b.textContent = 'Copy';
    b.setAttribute('aria-label', 'Copy code');
    b.addEventListener('click', () => copy(b, code.textContent));
    code.parentElement.append(b);
  });

  // Tabs (the WAI-ARIA tabs pattern).
  $$('[data-tabs]').forEach((tabs) => {
    const list = $$('[role="tab"]', tabs);
    const select = (t) => {
      list.forEach((x) => {
        const on = x === t;
        x.setAttribute('aria-selected', on);
        x.tabIndex = on ? 0 : -1;
        document.getElementById(x.getAttribute('aria-controls')).hidden = !on;
      });
      t.focus();
    };
    list.forEach((t, i) => {
      t.addEventListener('click', () => select(t));
      t.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowRight') select(list[(i + 1) % list.length]);
        if (e.key === 'ArrowLeft') select(list[(i - 1 + list.length) % list.length]);
      });
    });
  });

  // The docs menu on small screens.
  const menu = $('[data-menu]');
  menu?.addEventListener('click', () => {
    const open = $('#sidebar').classList.toggle('is-open');
    menu.setAttribute('aria-expanded', open);
  });

  // Highlight the section being read in the table of contents.
  const toc = $$('.toc a');
  if (toc.length && 'IntersectionObserver' in window) {
    const byId = new Map(toc.map((a) => [decodeURIComponent(a.hash.slice(1)), a]));
    const io = new IntersectionObserver((entries) => {
      entries.forEach((e) => {
        if (e.isIntersecting) {
          toc.forEach((a) => a.classList.remove('is-active'));
          byId.get(e.target.id)?.classList.add('is-active');
        }
      });
    }, { rootMargin: '0px 0px -70% 0px' });
    byId.forEach((_, id) => {
      const h = document.getElementById(id);
      if (h) io.observe(h);
    });
  }

  // Search: Zola's elasticlunr index, opened with "/" or the button.
  const dialog = $('[data-search]');
  const input = $('[data-search-input]');
  const results = $('[data-search-results]');
  let index;
  const open = () => { dialog.showModal(); input.focus(); input.select(); };
  $('[data-search-open]')?.addEventListener('click', open);
  addEventListener('keydown', (e) => {
    if (e.key === '/' && !/input|textarea/i.test(document.activeElement.tagName)) {
      e.preventDefault();
      open();
    }
  });
  dialog?.addEventListener('click', (e) => { if (e.target === dialog) dialog.close(); });
  const escape = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  input?.addEventListener('input', () => {
    if (!index && window.searchIndex && window.elasticlunr) index = elasticlunr.Index.load(window.searchIndex);
    const q = input.value.trim();
    if (!index || q.length < 2) { results.innerHTML = ''; return; }
    const fields = { title: { boost: 3 }, description: { boost: 2 }, body: { boost: 1 } };
    const hits = index.search(q, { bool: 'AND', expand: true, fields }).slice(0, 10);
    results.innerHTML = hits.length
      ? hits.map((h) => {
          const d = index.documentStore.getDoc(h.ref);
          const text = escape((d.description || d.body || '').slice(0, 140));
          return '<li><a href="' + h.ref + '">' + escape(d.title || h.ref) + '<small>' + text + '…</small></a></li>';
        }).join('')
      : '<li class="search__empty">No results for “' + escape(q) + '”.</li>';
  });
})();
