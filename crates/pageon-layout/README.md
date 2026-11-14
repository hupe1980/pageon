# pageon-layout

pageon's geometry. It lays out a `pageon-dom` document at a viewport and answers the questions an
agent asks before acting: where an element is, whether it is rendered and visible, whether it is
in the viewport, what covers it, and which element is at a point.

Styling and layout run on [Blitz](https://github.com/DioxusLabs/blitz) (Stylo for the cascade,
Taffy for block, flex and grid, Parley for text). Blitz works on a derived copy of the document;
`pageon-dom` remains the source of truth, and results are keyed by its node ids. Text is
measured only with the bundled Liberation fonts, never host fonts, so boxes are the same on every
machine. Liberation is metric-compatible with Arial, Times New Roman and Courier New.

```rust
let layout = Layout::new(&dom, Viewport::default(), &stylesheets, Some(url));
layout.box_of(id);      // border box, document coordinates
layout.visibility(id);  // NotRendered | Hidden | Visible
layout.covered_by(id);  // the element painted over its centre, if any
layout.element_at(x, y);
```

`paint` draws the laid-out page on the CPU (Blitz's painter on vello_cpu), in about 2 ms for a
viewport.

`Layout::new` never panics. If the layout engine fails on a document, every node reports
*not rendered* and `Layout::error` says why.

## Accuracy

Boxes are compared with Chromium (JavaScript off, 1280×720) on 19 constructed pages:
96.9 % of elements match within 2 px and 98.2 % within 8 px. Flex, grid, floats, positioning,
typography and long lists match exactly. Known gaps:

- **Inline elements beside form controls**: a `<label>` or `<span>` takes the height of its line
  (for example 27 px where Chromium has 18), and lines holding controls are a few px taller.
- **Tables**: `<caption>` gets no box, which shifts the rows below it. Rows and row groups take
  the union of their cells' boxes.
- **CJK and other scripts** with no bundled font.
- **`<img>`** uses its attributes or a placeholder size for layout; image data is fetched and
  decoded on first paint.
- **`@import`** and web fonts are not loaded; `position: sticky` lays out as `relative`.
- Shadow trees render as the composed tree. Blitz does not style shadow trees, so their
  selectors are rewritten to mirror-only scope markers (`:host`, `::slotted`, `::part`
  included); pages without shadow trees are not rewritten.
- Scroll positions (the viewport's and scroll containers') are kept by the caller and applied
  to each layout; the viewport's costs no re-layout. Blitz's own viewport scroll is used only
  while painting, because its geometry for fixed boxes under scroll is wrong.

MIT OR Apache-2.0. The bundled fonts are under the SIL Open Font License (see `fonts/`).
