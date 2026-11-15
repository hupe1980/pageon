# Editor fixtures

Real rich-text editors for the editing tests (`crates/pageon/tests/editing.rs`), each one
minified IIFE built with esbuild 0.28.2 from `src/`:

- `pm.min.js`: ProseMirror (`prosemirror-view` 1.42.9, `-state` 1.4.4, `-model` 1.25.13,
  `-schema-basic` 1.2.5, `-example-setup` 1.2.4), MIT.
- `lexical.min.js`: Lexical 0.53.0 with `@lexical/rich-text` and `@lexical/history`, MIT.
- `quill.min.js`: Quill 2.0.3 core with bold and italic, BSD-3-Clause.

Regenerate (from an empty directory with those packages and `esbuild@0.28.2` installed,
`F` = this directory):

```sh
for f in pm quill; do
  npx esbuild $F/src/$f.js --bundle --minify --format=iife --legal-comments=none \
    '--define:process.env.NODE_ENV="production"' --outfile=$F/$f.min.js
done
npx esbuild $F/src/lexical.js --bundle --minify --format=iife --legal-comments=none \
  --conditions=production '--define:process.env.NODE_ENV="production"' --outfile=$F/lexical.min.js
```
