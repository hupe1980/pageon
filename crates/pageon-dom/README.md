# pageon-dom

pageon's document model:
- an arena DOM whose `NodeId` handles **never alias**: a handle to a removed node never resolves
  again, even after its storage is reused;
- built by html5ever's standards-compliant tree builder;
- decoded by the HTML standard's encoding sniffing (BOM, HTTP charset, `<meta>` prescan, default);
- serialized by the HTML fragment serialization algorithm.

```rust
use pageon_dom::{ParseOptions, parse_document};

let dom = parse_document("<p>Hello <b>world</b></p>", ParseOptions::default());
assert_eq!(dom.text(dom.root()), "Hello world");
```

**Shadow trees.** Shadow roots are parentless nodes attached to their hosts, scripted
(`attach_shadow`) or declarative (`<template shadowrootmode>`, documents only). Slots are
assigned on demand. `composed()` gives the flat tree with the same ids, for layout, the
accessibility tree and views.

**Conformance.** The html5lib tree-construction suite (1,792 cases) runs through this DOM,
differentially against html5ever's reference DOM. 1,787 cases pass, the same as the reference,
and `tests/html5lib/KNOWN.md` lists the rest.

MIT OR Apache-2.0.
