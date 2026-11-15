# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Anthropic's browser and computer toolsets and OpenAI's computer tool run on pageon:
  `Browser.toolset(...)` in Python, `pageon/toolset/*` on `pageon mcp`.
- Right, middle, double and triple clicks, modifier keys, key chords and sequences, held keys,
  typing into the focused field, and the wheel at a point, for every client (Playwright's
  `dblclick`, right clicks and `Control+A` over CDP).
- `pageon::Config` and `pageon::mcp::serve_with`: embed a session configured as the CLI's
  flags configure it.
- `pageon::threat_model` on docs.rs.

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

[Unreleased]: https://github.com/hupe1980/pageon/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hupe1980/pageon/releases/tag/v0.1.0
