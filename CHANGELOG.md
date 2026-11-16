# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- WebMCP form tools (the declarative API): `<form toolname tooldescription toolautosubmit>`
  is listed and called like a registered tool, with `SubmitEvent.agentInvoked` and
  `respondWith`; autosubmitting calls are labelled consequential.
- Chrome's `WebMCP` DevTools domain: Puppeteer's `page.webmcp` lists and runs page tools.
- `SubmitEvent` with `submitter`.
- Sessions from the shell: `pageon open URL` starts a named session (`-s NAME`) that stays
  open between commands; `click`, `type`, `press`, `select`, `view`, `find`, `screenshot`,
  `tabs`, `reset`, `lookahead`, `call` and the rest act on it with MCP's replies; `sessions`,
  `close [--all]`, `--idle`. `pageon skill --install` writes an agent skill.
- The viewer: `--ui` on `pageon mcp` and `pageon open` serves a loopback page with every step
  in words, the page before and after it, and what the model read; `--approve` asks there
  before writes leave (Allow Once, Allow for This Session, Don't Allow); `Space` pauses the
  agent; `pageon ui` prints a session's URL.

- Sign-ins that renew: `--sign-in login.flow.json` on `pageon mcp` and `pageon open` (Python
  `sign_ins`): when an action is redirected to the sign-in page or answered 401, pageon runs the
  sign-in workflow and opens the page again; writes are never resent, and three renewals in a
  row that do not hold stop it.
- Python: `Browser(ui=True, approve=True)` and `browser.viewer_url`.
- Expectations: `url`, `see`, `no`, `request`, `download`, checked after an action with
  evidence: `expect` on MCP's acting tools, `--expect` on CLI actions and `pageon fetch` (exit
  8), `expect=` in Python (`ExpectationError`); saved into workflows as `check` and verified on
  every run.
- Cost: every MCP reply carries `cost` (o200k tokens of its text, wall and page time); spans
  carry `pageon.reply.tokens`; recordings keep it per step and `pageon replay FILE --report`
  prints totals and p50/p95/p99; the viewer shows tokens per step and in total.

- Platform: named form access (`form.email`, radio groups as `RadioNodeList`), `form.action`,
  `method` and `enctype`; the Popover API with `popovertarget` buttons and light dismiss;
  canvas text (`font`, `measureText`, `fillText`, `strokeText`) with the bundled fonts;
  `document.write` while parsing; `dir`, `draggable`, `spellcheck`, `translate`, `outerText`,
  `a.text`, `valueAsNumber`, `textarea.rows`, `table.rows`, `img.loading`.
- `pageon audit URL`: how ready a page is for agents. It scores names, actions, page tools,
  the view's token cost, access (refusals, content without scripts, robots.txt, llms.txt,
  Markdown on request) and health, with the fixes that gain the most first; `--json`,
  `--fail-under N` (exit 9).
- Platform: `select.add` and `remove`, `new EventTarget()` and classes that extend it (every
  constructible interface keeps a subclass's prototype), `new DOMRect()`, `Symbol.toStringTag`
  on every interface (`[object Event]`), and `document.fonts` (`ready`, `status`, `check`,
  `load`).

- Platform, from sites the audit ran on: `error` and `unhandledrejection` events at the window
  (`onerror` with its five arguments; a handled one is not a page error; other origins'
  errors muted), `PromiseRejectionEvent`; `document.styleSheets`, `style.sheet`,
  `insertRule`/`deleteRule` with the inserted rules in layout (CSS-in-JS); inserted `<link>`
  stylesheets, `preload` and `modulepreload` load and fire `load`/`error`; SVG element
  interfaces (`SVGAElement`, …); `link.as`. crates.io and BBC News now run without errors.

### Fixed

- `<noscript>` content no longer shows while scripts run.
- Event handlers run in the order they were set among listeners (they ran first).
- Streams work on pages with an AMD loader (`define`) or CommonJS globals.
- `performance.measure(name, 'navigationStart')` and the other timing names work.
- A thrown `DOMException` is reported as `Uncaught SyntaxError: …`, not a bare `Uncaught`.
- Pages that lay out many times (GitHub's home page) load about 45 % faster: stylesheet
  rewrites are remembered between layouts.
- `performance.measure(name, {}, endMark)` no longer throws; options follow Chrome's rules.
- Canvases appear in screenshots (they were painted blank and noted as not painted).
- Workflow URL checks keep the port, so two local apps on different ports are told apart.
- Buttons size to their content, so a page's padding and font apply (they were 21 px tall).

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

[Unreleased]: https://github.com/hupe1980/pageon/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/hupe1980/pageon/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/hupe1980/pageon/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/pageon/releases/tag/v0.1.0
