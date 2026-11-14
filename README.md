<div align="center">

# pageon

**The browser engine built for AI agents.**

Its own engine in Rust, no Chromium. Compact, budgeted page views with stable refs, typed
actions, sandboxed pages, and sessions you can replay, rewind and fork.

[Documentation](https://hupe1980.github.io/pageon/) ·
[Quick start](https://hupe1980.github.io/pageon/docs/quick-start/) ·
[MCP](https://hupe1980.github.io/pageon/docs/mcp/) ·
[Python](https://hupe1980.github.io/pageon/docs/python/) ·
[Puppeteer & Playwright](https://hupe1980.github.io/pageon/docs/cdp/)

[![CI](https://github.com/hupe1980/pageon/actions/workflows/ci.yml/badge.svg)](https://github.com/hupe1980/pageon/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

</div>

```text
$ pageon fetch https://shop.example/checkout --dump ax
@page v1 "Checkout – Acme" https://shop.example/checkout load:complete budget:4000/o200k
## main
# "Checkout"
textbox e5 "Email" ="ann@example.com" required
combobox e6 "Country" ="Germany" {"Germany"|"France"|"Spain"|+2}
checkbox e7 "Save address" checked
button e12 "Pay now" disabled
```

## Why pageon

Agents drive Chromium through tools built for testing and debugging. pageon is a browser engine
whose output is made for a model, and whose safety does not depend on the model behaving.

- **More success per token.** On MiniWoB++, pageon's view matches or beats Playwright MCP's
  snapshot with a quarter of the tokens on real pages, and beats BrowserGym and browser-use
  significantly ([numbers](#numbers)).
- **Refs that stay put.** `e12` names the same element across steps, is never written into the
  page, and is relocated (and reported) when the page replaces the element.
- **Quoted, marked, budgeted.** All page text is quoted, so a page cannot forge a view line.
  Hidden text, invisible input traps and third-party frames are marked, and every view fits a
  token budget that names its tokenizer.
- **Actions that say what happened.** `click e12` checks actionability against real layout, waits
  for the page to settle, and returns navigations, dialogs, downloads, requests and errors, or an
  error that says what to do next.
- **Safe by construction.** Pages run in a sandboxed process with no files, no network and no
  programs. An origin policy, a secrets vault (`secret://shop` is typed, never shown) and an
  optional guard program decide what leaves.
- **Replay, rewind, look ahead.** Sessions record and replay byte for byte on a virtual clock.
  Agents go back to any step without re-sending anything, and try actions on a fork of the page
  before committing.
- **Fits your stack.** MCP for Claude, Cursor and any MCP host; Python; Puppeteer and Playwright
  over CDP; WebDriver BiDi; BrowserGym; and a Rust API.

## Quick start

pageon is pre-release; build it from source (Rust 1.99+). Its sandbox,
[procage](https://github.com/hupe1980/procage), is checked out next to it:

```sh
git clone https://github.com/hupe1980/procage
git clone https://github.com/hupe1980/pageon
cargo install --path pageon/crates/pageon

pageon fetch https://example.com --dump ax                     # what an agent sees
pageon fetch https://example.com --act "click e1" --dump ax    # act, then look again
pageon fetch https://example.com --screenshot page.png --marks # pixels with the same refs
```

## Connect your agent

**MCP**, for Claude Code, Claude Desktop, Cursor or any MCP host:

```sh
claude mcp add pageon -- pageon mcp
claude mcp add shop -- pageon mcp --origin https://shop.example --secret shop@https://shop.example
```

**Python**:

```python
from pageon import Browser

with Browser(origins=["https://shop.example"]) as browser:
    tab = browser.open("https://shop.example/login")
    print(tab.observe())
    out = tab.type("e4", "secret://shop", submit=True)  # PAGEON_SECRET_SHOP, never shown
    print(out)  # ok type e4 … · navigated to https://shop.example/account
```

**Puppeteer and Playwright** (CDP) or any **WebDriver BiDi** client, unchanged:

```sh
pageon cdp --port 9222
# DevTools listening on ws://127.0.0.1:9222/devtools/browser/<secret>
# WebDriver BiDi listening on ws://127.0.0.1:9222/session/<secret>
```

```js
const browser = await puppeteer.connect({ browserWSEndpoint: cdpUrl });   // the printed URLs
const pw = await chromium.connectOverCDP(cdpUrl);
const bidi = await puppeteer.connect({ browserWSEndpoint: bidiUrl, protocol: 'webDriverBiDi' });
```

The URLs carry a per-run secret, so other local processes cannot take the browser. Clients
that discover the endpoint over HTTP (`browserURL`, `connectOverCDP('http://…')`) need
`pageon cdp --discovery`.

**Rust**:

```rust
use pageon::{Live, LoadOptions, Session};

let session = Session::new(Live::default());
let mut tab = session.open("https://example.com", LoadOptions::default())?;
println!("{}", tab.observe());
println!("{}", tab.act("click e1".parse()?)?);
```

The [documentation](https://hupe1980.github.io/pageon/) covers every surface, the view format,
actions, the security model, recording and replay, workflows, WebMCP and identity.

## Numbers

Observation formats on MiniWoB++ (125 tasks × 3 runs, Qwen3.5-9B at temperature 0, 4,000-token
o200k budget, BrowserGym's action set for every format):

| pageon vs | Task success | 95 % CI | Tokens on real pages |
| --- | --- | --- | --- |
| Playwright MCP snapshot | +3.2 points | [−3.2, +9.9] | 0.27× |
| BrowserGym AXTree | +8.3 points | [+1.9, +14.9] | 0.16× |
| browser-use 1.0 | +18.7 points | [+12.8, +24.8] | 1.02× |

Engine figures, measured in the test suite:

| | |
| --- | --- |
| Native `ax` view = Chromium-based view | 204/204 lines on the corpus pages, scripted ones included |
| html5lib tree construction | 1,787/1,792 (the same as html5ever's reference) |
| WPT roles / names | 100 % / 94.2 % |
| Layout vs Chromium | 96.9 % of boxes within 2 px |
| Click (caged) · screenshot · tool call | 2.2 ms · 1.7 ms · 0.07 ms |
| Puppeteer, Playwright, BiDi suites | pass caged and in-process |

The benchmark is in [`bench/`](bench) and runs locally at no cost; every cell is reported,
including the ones pageon loses.

## Status

pageon is pre-release. Its [threat model](THREAT_MODEL.md) lists what it defends against and
every known gap, open ones included; read it before pointing an agent at anything that
matters. Report vulnerabilities privately ([SECURITY.md](SECURITY.md)).

Not yet supported: `MessageChannel`, scripted iframes added after load, file uploads (`Blob`,
`File`), `WebSocket` and `EventSource`, `IndexedDB`, workers, Selection and `Range`, the
Navigation API, `crypto.subtle`, WebDriver classic, request interception. Unsupported APIs are absent rather than faked, so
pages and clients can detect them, and protocol methods pageon lacks fail at once with what to
use instead.

## Workspace

| Crate | |
| --- | --- |
| [`pageon`](crates/pageon) | The engine, the CLI, the sandbox and policy, and the MCP, CDP and BiDi servers |
| [`pageon-agent`](crates/pageon-agent) | The agent views: refs, quoting, budgets, diffs |
| [`pageon-js`](crates/pageon-js) | V8, DOM bindings generated from WebIDL, the event loop on a virtual clock |
| [`pageon-layout`](crates/pageon-layout) | CSS, layout with bundled fonts, hit-testing, paint |
| [`pageon-ax`](crates/pageon-ax) | The accessibility tree (HTML-AAM, ARIA, AccName) |
| [`pageon-net`](crates/pageon-net) | Fetch, CORS, cookies, storage, the egress policy |
| [`pageon-dom`](crates/pageon-dom) | The document model and HTML parsing |
| [`pageon-env`](crates/pageon-env) | Network, clock and randomness: live, recorded or replayed |
| [`pageon-py`](bindings/python) | The Python package, including the BrowserGym backend |

```sh
cargo test --workspace                              # offline: the engine, views, servers
cd conformance/puppeteer && npm ci && node --test   # Puppeteer against `pageon cdp`
```

## License

Licensed under either of the [MIT license](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option. Third-party material is listed in
[THIRD_PARTY.md](THIRD_PARTY.md).
