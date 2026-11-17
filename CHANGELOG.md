# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- Computed styles (`getComputedStyle`, except sizes and shorthands) are computed on pageon's
  own document and kept up to date from its changes: after a few class changes on GitHub's home
  page, 20× less work than restyling through the layout engine. Large stylesheets are parsed
  once per process.
- Layout uses the same styles: one cascade per change instead of two. Shadow trees, slots,
  `::part` and font-relative units (`ch`, `ex`, `cap`, `ic`) are styled on pageon's document too.

### Fixed

- Inserting a shadow host into its own shadow tree hung the page; it now throws
  `HierarchyRequestError`.
- Custom elements hidden until defined (`x-el:not(:defined)`) stayed hidden after their
  definition ran; `:open`, `:modal`, `:placeholder-shown`, `:read-write` and `:read-only` now
  match in layout and computed styles.

## [0.5.0] - 2026-11-17

### Added

- `pageon bench URL…`: how fast pages open (first view, warm opens, CPU, memory, layouts,
  tokens), recorded once and replayed offline; `--json` and `--against` for before and after.
- A diverging replay names what differs: the URL, the HTML, the view or storage.

### Changed

- Pages that change and measure themselves are faster: a layout keeps its tree and restyles
  only what changed, computed styles that do not depend on layout skip it, and
  `IntersectionObserver` and `ResizeObserver` report once per frame. GitHub's home page takes
  less than half the CPU of 0.4.1.
- An element under `display: none` has an all-zero box, as in Chromium.

### Fixed

- Recordings redacted text in scripts that looked like credential parameters
  (`;this.sessionX=function`), so replays of some sites (BBC) failed; scripts and styles are now
  kept byte for byte.
- An `<input>`'s children (added by a script) were laid out.

## [0.4.1] - 2026-11-16

### Fixed

- Page loads settled their timers twice (0.4.0), doubling the settle time of timer-heavy pages.

## [0.4.0] - 2026-11-16

### Added

- Sessions from the shell: `pageon open URL` keeps a named session (`-s NAME`) open between
  commands (`click`, `type`, `view`, `find`, `screenshot`, `call`, …); `pageon skill --install`
  writes an agent skill.
- The viewer: `--ui` serves a loopback page with every step in words, the page before and
  after, and what the model read; `--approve` asks there before writes leave; `Space` pauses.
- Sign-ins that renew: `--sign-in login.flow.json` runs the sign-in workflow when a page asks
  for it again; writes are never resent.
- Expectations: `url`, `see`, `no`, `request`, `download`, checked after an action with
  evidence (`expect` on MCP, `--expect` on the CLI with exit 8, `expect=` in Python).
- Cost: every MCP reply carries `cost` (tokens, wall and page time); `pageon replay FILE
  --report` prints totals and p50/p95/p99.
- `pageon audit URL`: how ready a page is for agents (names, actions, page tools, view cost,
  access, health, layout thrashing), with the fixes worth the most first; `--json`,
  `--fail-under N` (exit 9).
- WebMCP form tools (`<form toolname>`) and Chrome's `WebMCP` DevTools domain.
- Platform: named form access and `RadioNodeList`, the Popover API, canvas text,
  `document.write` while parsing, `error` and `unhandledrejection` events (other origins
  muted), `document.styleSheets` and `insertRule` with the rules in layout (CSS-in-JS),
  inserted `<link>` loads with events, `document.fonts`, `select.add`, `new EventTarget()`
  and subclasses, `DOMRect`, `Symbol.toStringTag`, SVG element interfaces.
- Iframes scripts create: an `about:blank` window at once, then the `src` loads in its place;
  a sandbox without `allow-same-origin` gives an opaque window. The window's `load` waits for
  the page's frames.

### Changed

- Layouts of the same page reuse their layout document: stylesheets stay parsed and rules no
  class or id of the page can match are left out. GitHub's home page uses a third of the CPU.

### Fixed

- `<noscript>` content no longer shows while scripts run.
- Event handlers run in the order they were set; `event.source` of messages posted through a
  frame's window is the page.
- Inline script errors carry the document's line numbers and URL.
- Streams work on pages with an AMD loader or CommonJS globals.
- `performance.measure` accepts empty options and timing names (`navigationStart`).
- A thrown `DOMException` is reported by its name.
- Canvases appear in screenshots.
- Workflow URL checks keep the port.
- Buttons size to their content.

## [0.3.0] - 2026-11-16

### Added

- Editing: selection and live ranges, `contenteditable` editing through `beforeinput`, text
  selection in fields, and the clipboard (one per session, never the host's). ProseMirror,
  Lexical and Quill take typing, formatting shortcuts and paste.
- `select REF TEXT` selects text in any element; `paste` pastes; outcomes report what an action
  copied.
- Passkeys: sites' WebAuthn sign-in and registration answered from a store the broker holds
  (`--passkeys FILE`); keys never reach the page process.
- `--profile chrome` in the release binaries; `--platform` picks the platform it reports;
  `--signature-tag` for agent programs that verify their own tag; a site asking for a signature
  (`Accept-Signature`) is reported as `signature required`.
- Writes carry `read-from:<site>` labels for guards and approvers; `--cross-site-writes refuse`
  refuses a write to a site after the agent read another site's content, unless approved.
- IndexedDB, kept per origin for the session across reloads and tabs.
- `pageon replay ep.json --trace trace.zip`: a recorded run in Playwright's trace viewer
  (actions, the page after each, console, network).
- `pageon mcp --otlp URL`: one OpenTelemetry span per tool call, under the client's
  `traceparent`.
- Dedicated workers (classic and module) on the page's virtual clock, with `ArrayBuffer`
  transfer, `importScripts` and `blob:` worker scripts (`URL.createObjectURL`).
- `<iframe sandbox>`: sandboxed frames get an opaque origin without `allow-same-origin`, run no
  script without `allow-scripts`, and need `allow-forms`, `allow-popups` and `allow-modals` for
  forms, popups and dialogs.
- `browser_find`: searches the whole page, including what the view's budget left out, and tells
  elements with the same role and name apart (`(1 of 2, under "Ada")`).
- Outcomes are `unknown` when nothing changed yet and a timer the action set is still due,
  with the wait that lets the page finish.
- Detail levels: `browser_observe {"detail": "compact"}` (and `pageon mcp --detail`,
  `Episode(detail=)` in Python) leaves text runs out of the view.
- Credential URL parameters (`token`, `code`, `sig`, session ids, …) are `[redacted]` in views,
  outcomes, errors and saved recordings; recordings still replay exactly.
- PDFs, Markdown (including `llms.txt`) and plain text open as pages: headings, paragraphs and
  links with refs; hidden PDF text is marked; no scripts or source markup.
- `goto download:N` (over MCP, `browser_open` with `download:N`) opens a download as a page.
- `pageon fetch --prefer-markdown` asks sites for their Markdown rendition, labelled
  `source:server-markdown`; `Content-Usage` preferences (header or robots.txt) are shown.
- Web Animations and `scheduler.postTask`/`yield` on the virtual clock; `navigator.permissions`
  and `Notification.permission` (never granted), `userActivation`, `sendBeacon`,
  `hardwareConcurrency`, the PDF viewer lists, `screen.orientation`, `screenX`/`screenY`.

### Changed

- `type REF` into an editor types over its content key by key instead of replacing it.
- `--profile browser-compatible` is now `--profile chrome` (the old name still works) and
  reports the host's platform instead of Linux.

### Fixed

- Pages see UTC caged and in-process; the sandbox showed a wrong zone, in-process pages the
  host's.
- `pageon mcp --budget` applies in the sandbox (caged tabs kept the default 4,000).
- `DOMException` is an `Error` (`instanceof Error`, `String(e)` gives its name and message).

## [0.2.0] - 2026-11-15

### Added

- Anthropic's browser and computer toolsets and OpenAI's computer tool run on pageon:
  `Browser.toolset(...)` in Python, `pageon/toolset/*` on `pageon mcp`.
- Right, middle, double and triple clicks, modifier keys, key chords and sequences, held keys,
  typing into the focused field, and the wheel at a point, for every client (Playwright's
  `dblclick`, right clicks and `Control+A` over CDP).
- `pageon::Config` and `pageon::mcp::serve_with`: embed a session configured as the CLI's
  flags configure it.
- `pageon::threat_model` on docs.rs.
- Web Crypto (`crypto.subtle`: SHA, HMAC, ECDSA P-256, Ed25519, AES-GCM), streams
  (`ReadableStream`, `TransformStream`, `TextDecoderStream`, `Response.body`), `DOMParser`,
  `NodeIterator`, `MessageChannel`, user timing, `screen`, `CSS.supports` and `InputEvent`.

### Changed

- Secrets are filled into fields at once, as a password manager fills them, never key by key.
- Chinese, Japanese, Thai, Lao, Khmer and Burmese break lines by dictionary, without ICU4X
  warnings.
- Release binaries are built with LTO and without symbols: smaller downloads.

## [0.1.0] - 2026-11-15

The first release.

### Added

- 🧠 **The engine**: its own DOM, V8 with bindings generated from the WebIDL standards, Stylo
  and Taffy layout with bundled fonts, an accessibility tree, the HTML event loop on a virtual
  clock, and networking with the Fetch rules, CORS and cookies.
- 👀 **The agent view**: compact, quoted, token-budgeted pages with stable refs, diffs between
  steps, marks for hidden text and input traps, and screenshots with set-of-marks.
- ⚡ **Actions**: click, type, upload, press, select, hover, drag, scroll, wait and navigation,
  each with a typed outcome or an error that says what to do next.
- 🛡️ **Safety**: pages in a sandboxed content process, origin sets, approvals, a guard program,
  a secrets vault and staged files released by the broker, an honest identity.
- ⏪ **Reproducible sessions**: recording and replay on the virtual clock, exact reset to any
  step, lookahead on a fork of the page, and workflows that rerun a task without a model.
- 🔌 **Interfaces**: the `pageon` CLI, an MCP server (stdio and Streamable HTTP; MCP
  `2026-07-28` and earlier), CDP for Puppeteer and Playwright, WebDriver BiDi, a Python package
  with a BrowserGym backend, and the Rust crates.

[Unreleased]: https://github.com/hupe1980/pageon/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/hupe1980/pageon/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/hupe1980/pageon/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/hupe1980/pageon/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/hupe1980/pageon/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/hupe1980/pageon/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/pageon/releases/tag/v0.1.0
