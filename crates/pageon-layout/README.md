# pageon-layout

pageon's geometry. It lays out a `pageon-dom` document at a viewport and answers the questions an
agent asks before acting: where an element is, whether it is rendered and visible, whether it is
in the viewport, what covers it, and which element is at a point.

Styles come from `pageon-style` (Stylo on pageon's own document). Layout builds pageon's box
tree from them (elements, text, pseudo-elements, anonymous blocks) and lays it out with
[Taffy](https://github.com/DioxusLabs/taffy) (block, flex, grid, tables, floats) and
[Parley](https://github.com/linebender/parley) (text); results are keyed by `pageon-dom` node
ids. Pass the styles a page keeps up to date (`LayoutOptions::styles`), or the layout styles the
document itself and keeps its styler for `Layout::again`. `LayoutOptions::styles_only` computes
styles without boxes. Text is measured only with the bundled Liberation fonts, never host
fonts, so boxes are the same on every machine. Liberation is metric-compatible with Arial,
Times New Roman and Courier New.

```rust
let layout = Layout::new(&dom, Viewport::default(), &stylesheets, Some(url));
layout.box_of(id);      // border box, document coordinates
layout.visibility(id);  // NotRendered | Hidden | Visible
layout.covered_by(id);  // the element painted over its centre, if any
layout.element_at(x, y);
```

`paint` draws the box tree on the CPU (vello_cpu), in about 2 ms for a viewport. Paint order,
stacking contexts and transforms are built on the first paint or hit test, not per layout.

`Layout::new` never panics. If layout fails on a document, every node reports *not rendered*
and `Layout::error` says why.

## Accuracy

Boxes are compared with Chromium (JavaScript off, 1280×720) on 19 constructed pages:
96.8 % of elements match within 2 px and 98.0 % within 8 px. Every box is also pinned
exactly (`tests/geometry/boxes.golden`). Flex, grid, floats, positioning,
typography and long lists match exactly. Known gaps:

- **Inline elements beside form controls**: a `<label>` or `<span>` takes the height of its line
  (for example 27 px where Chromium has 18), and lines holding controls are a few px taller.
- **Tables**: `<caption>` gets no box, which shifts the rows below it. Rows and row groups take
  the union of their cells' boxes.
- **CJK and other scripts** with no bundled font.
- **`<img>`** uses its attributes or a placeholder size for layout; image data is fetched and
  decoded on first paint.
- **`@import`** and web fonts are not loaded; `position: sticky` lays out as `relative`.
- Shadow trees render as the flat tree (slots, `:host`, `::slotted`, `::part`).
- Scroll positions (the viewport's and scroll containers') are kept by the caller and applied
  to each layout; the viewport's costs no re-layout.

The box construction, Taffy integration, inline layout, painter and hit testing are adapted from
[Blitz](https://github.com/DioxusLabs/blitz) (MIT OR Apache-2.0).

MIT OR Apache-2.0. The bundled fonts are under the SIL Open Font License (see `fonts/`).
