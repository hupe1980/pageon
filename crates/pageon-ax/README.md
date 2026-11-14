# pageon-ax

pageon's accessibility tree: what an assistive technology would get for each node of a
`pageon-dom` document.

- **Roles**: implicit roles per HTML-AAM, including the context rules (scoped `header`, `footer`
  and `aside`; `section` and `img` that depend on a name; table cells). Explicit ARIA roles take
  the first valid token, and `region`/`form` fall back when they have no name. Presentational
  roles go through conflict resolution.
- **Names**: AccName 1.2:
  - `aria-labelledby`, including hidden referenced content;
  - `aria-label` and the host-language alternatives (`label`, `alt`, `legend`, `caption`,
    button values);
  - embedded controls and name from content, with block spacing and `text-transform`;
  - then `title`, then `placeholder`.
- **States**: checked, disabled (including disabled fieldsets), expanded, selected, required,
  invalid, level, modal; values of form controls and range widgets. Password values are masked.
- **Ignored nodes**: not rendered, `visibility: hidden`, `aria-hidden` and presentational nodes
  stay in the tree, marked with the reason, so their text can still be reached.

```rust
let layout = pageon_layout::Layout::new(&dom, Viewport::default(), &[], None);
let tree = pageon_ax::Tree::build(&dom, Some(&layout)); // or None: judge by markup only
tree.role(id); tree.name(id); tree.node(id)
```

## Conformance

`cargo test -p pageon-ax --test wpt -- --nocapture` runs the static role and name tests of the
Web Platform Tests, pinned at one commit (`tests/wpt/SOURCE`):

- roles: 323/323 (100 %);
- names: 539/572 (94.2 %).

Of the 33 failing names, 27 need CSS generated content (`::before`, `::after`, counters), which
pageon does not include in names yet. Every failure is listed in `tests/wpt/KNOWN.md`, and a new
failure fails the test.

MIT OR Apache-2.0.
