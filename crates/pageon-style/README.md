# pageon-style

pageon's CSS styling: [Stylo](https://github.com/servo/stylo) runs directly on a `pageon-dom`
document. Style data sits beside the nodes, keyed by node id, so no second tree is built.

```rust
let mut styler = Styler::new(1280.0, 720.0, "https://example.com/");
styler.set_sheets(&[(text_hash(css), css)]);
styler.sync(&dom, &dom.take_style_changes(), Page::default());
styler.value(&dom, id, "display");     // "block"
```

`sync` reads the document's change log: changed attributes get a snapshot of their values
before, so Stylo restyles only the elements a change can affect; inserted elements are styled;
siblings are restyled when the parent's selectors depend on position. Form pseudo-classes
(`:invalid`, `:user-invalid`, `:required`, `:placeholder-shown`, `:read-write`), `:state()`,
`:popover-open`, `:modal` and `:defined` match from pageon's own state.

Shadow trees are styled on the flat tree (per-root sheets, slots, `::part`, custom properties
through hosts). Legacy attributes (`width`, `bgcolor`, `hidden`, `<svg>` sizes …) are mapped as
browsers map them. Parsed sheets are shared between pages in a process. The Stylo binding is
adapted from [Blitz](https://github.com/DioxusLabs/blitz) (MIT OR Apache-2.0); the HTML
user-agent sheet is Servo's (MPL-2.0).

MIT OR Apache-2.0.
