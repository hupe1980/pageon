# Third-party material

pageon's code is MIT OR Apache-2.0. These files in the repository come from others, under their
own licences.

| Material | Where | Licence |
| --- | --- | --- |
| Liberation fonts 2.1.5 (Sans, Serif, Mono) | `crates/pageon-layout/fonts/` (built into `pageon-layout`) | SIL Open Font License 1.1 (`fonts/LICENSE`) |
| web-streams-polyfill 4.5.1 (`dist/ponyfill.js`, unmodified, SHA-256 pinned by a test) | `crates/pageon-js/src/streams/` (built into `pageon-js`) | MIT (header in the file) |
| html5lib tree-construction tests | `crates/pageon-dom/tests/html5lib/` (tests only) | MIT |
| WebIDL from `@webref/idl` 3.86.0 and the WebMCP draft (the specifications listed in `idl/SOURCE`) | `crates/pageon-js/idl/` (generator input) | WHATWG: CC BY 4.0; W3C: W3C Software and Document License (see `idl/SOURCE`) |
| Web Platform Tests: html-aam, wai-aria role, accname files | `crates/pageon-ax/tests/wpt/` (tests only) | W3C 3-clause BSD (see `tests/wpt/SOURCE`) |
| Web Platform Tests: cookies `http-state` parser cases | `crates/pageon-net/tests/http-state/` (tests only) | W3C 3-clause BSD (see `tests/http-state/SOURCE`) |
| webpack 5.112.2 runtime and esbuild 0.25.12 output | `crates/pageon/tests/fixtures/bundles/` (tests only) | MIT (webpack); the app source is pageon's |
| Vue 3.5.13 and Vue Router 4.5.0 browser ES module builds | `crates/pageon/tests/fixtures/vue/` (tests only) | MIT (`fixtures/vue/LICENSE`) |
| Floating UI (`@floating-ui/dom` 1.6.13, `@floating-ui/core` 1.6.9) browser builds | `crates/pageon/tests/fixtures/floating-ui/` (tests only) | MIT (`fixtures/floating-ui/LICENSE`) |
| DOMPurify 3.4.18 (`purify.min.js`) | `crates/pageon/tests/fixtures/platform2/` (tests only) | Apache-2.0 or MPL-2.0 (header in the file) |
| Lit 3 production modules (lit 3.3.3, lit-element 4.2.2, lit-html 3.3.3, @lit/reactive-element 2.1.2) | `crates/pageon/tests/fixtures/lit/` (tests only) | BSD-3-Clause (`fixtures/lit/LICENSE`) |

Rust dependencies and their licences are checked by `cargo deny` (`deny.toml`). The Puppeteer
conformance harness (`conformance/puppeteer/`) installs `puppeteer-core` 25.17.0 (Apache-2.0) from
npm at test time, and the Playwright harness (`conformance/playwright/`) `playwright-core` 1.65.0
(Apache-2.0); nothing of either is vendored.
