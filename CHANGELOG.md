# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Editing: selection and live ranges, `contenteditable` editing through `beforeinput`, text
  selection in fields, and the clipboard (one per session, never the host's). ProseMirror,
  Lexical and Quill take typing, formatting shortcuts and paste.
- `select REF TEXT` selects text in any element; `paste` pastes; outcomes report what an action
  copied.
- Passkeys: sites' WebAuthn sign-in and registration answered from a store the broker holds
  (`--passkeys FILE`); keys never reach the page process.
- `--profile chrome` in the release binaries, on the machine's own platform (`--platform` picks
  another); `--signature-tag` for agent programs that verify their own tag; a site asking for a
  signature (`Accept-Signature`) is reported as `signature required`.
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

### Changed

- `type REF` into an editor types over its content key by key instead of replacing it.
- `--profile browser-compatible` is now `--profile chrome` (the old name still works) and
  reports the host's platform instead of Linux.

### Fixed

- `pageon mcp --budget` applies in the sandbox (caged tabs kept the default 4,000).
- `DOMException` is an `Error` (`instanceof Error`, `String(e)` gives its name and message).

## [0.2.0]

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

## [0.1.0]

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

[Unreleased]: https://github.com/hupe1980/pageon/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/hupe1980/pageon/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/hupe1980/pageon/releases/tag/v0.1.0
