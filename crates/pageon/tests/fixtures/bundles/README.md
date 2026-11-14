# Bundler fixtures

Real bundler output for the script-loading tests (`crates/pageon/tests/modules.rs`). The tests
replay these files; they never run the bundlers.

- `src/`: the app (a shared `ui.js`, a lazy `route.js`).
- `esbuild/`: esbuild 0.25.12, ESM with code splitting: the entry uses top-level `await`,
  `import.meta.url` and `import()` of the route, which esbuild puts in a chunk; `ui.js` is a
  shared chunk.
- `iife/`: esbuild 0.25.12, the same UI as one classic script (`main-webpack.js`, `--format=iife`),
  for the speed comparison.
- `webpack/`: webpack 5.112.2 (webpack-cli 6.0.1), production mode: the route is a JSONP chunk
  that webpack's runtime loads by inserting a `<script>` and waiting for `onload`/`onerror`.

Regenerate (from an empty directory with `npm i esbuild@0.25.12 webpack@5.112.2 webpack-cli@6.0.1`,
`F` = this directory):

```sh
npx esbuild $F/src/main.js --bundle --splitting --format=esm --minify --outdir=$F/esbuild '--chunk-names=chunk-[hash]'
npx esbuild $F/src/main-webpack.js --bundle --format=iife --minify --outfile=$F/iife/main.js
cat > webpack.config.js <<CFG
module.exports = { mode: 'production', context: '$F/src', entry: './main-webpack.js',
  module: { rules: [{ test: /\.js$/, type: 'javascript/esm' }] },
  output: { path: '$F/webpack', filename: '[name].js', chunkFilename: '[name].chunk.js', clean: true } };
CFG
npx webpack --config webpack.config.js
```

The output contains webpack's runtime (MIT, see THIRD_PARTY.md).
