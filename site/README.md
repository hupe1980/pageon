# pageon website

The landing page and documentation at <https://hupe1980.github.io/pageon/>, built with
[Zola](https://www.getzola.org) (0.23). No Node, no frameworks: one stylesheet, one small
script, Zola's search index.

```sh
cd site
zola serve      # http://127.0.0.1:1111, live reload
zola build      # → public/
zola check --skip-external-links
```

| Path | |
| --- | --- |
| `content/_index.md` | The landing page; its sections are data in the front matter |
| `content/docs/*.md` | The documentation; `extra.group` places a page in the sidebar, `weight` orders it |
| `templates/` | `base.html` (head, SEO metadata, nav), `index.html`, `page.html`, `section.html` |
| `sass/main.scss` | All styles, light and dark |
| `static/` | `site.js` (theme, copy, tabs, search), the logo, `og.png` |
| `og/og.html` | The social card; render it with `pageon fetch site/og/og.html --no-sandbox --screenshot site/static/og.png` |

`.github/workflows/site.yml` builds every change and deploys `main` to GitHub Pages (enable
Pages with "GitHub Actions" as the source).
