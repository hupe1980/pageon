<div align="center">

# pageon

**The browser engine built for AI agents.**

Its own engine in Rust, no Chromium. Compact page views with stable refs, typed actions,
sandboxed pages, and sessions you can replay, rewind and fork.

[📖 Documentation](https://hupe1980.github.io/pageon/) ·
[🚀 Quick start](https://hupe1980.github.io/pageon/docs/quick-start/) ·
[🔌 MCP](https://hupe1980.github.io/pageon/docs/mcp/) ·
[🐍 Python](https://hupe1980.github.io/pageon/docs/python/) ·
[🎭 Puppeteer & Playwright](https://hupe1980.github.io/pageon/docs/cdp/)

[![CI](https://github.com/hupe1980/pageon/actions/workflows/ci.yml/badge.svg)](https://github.com/hupe1980/pageon/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/pageon.svg)](https://crates.io/crates/pageon)
[![PyPI](https://img.shields.io/pypi/v/pageon.svg)](https://pypi.org/project/pageon/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#-license)

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

## ✨ Why pageon

Agents usually drive Chromium through tools built for testing. pageon is a browser engine whose
output is made for a model, and whose safety does not depend on the model behaving.

- 🎯 **More success per token.** On MiniWoB++, pageon's view matches or beats Playwright MCP's
  snapshot with about a quarter of the tokens on real pages ([numbers](#-numbers)).
- 🔗 **Refs that stay put.** `e12` names the same element across steps, is never written into
  the page, and is relocated (and reported) when the page re-renders it.
- 🧾 **Quoted, marked, budgeted.** Page text is quoted, so a page cannot forge a view line.
  Hidden text, input traps and third-party frames are marked; every view fits a token budget.
- ⚡ **Actions that say what happened.** `click e12` checks the element against real layout,
  lets the page settle, and returns navigations, dialogs, downloads, requests and errors, or
  what to do instead.
- 🛡️ **Safe by construction.** Pages run in a sandboxed process with no files, network or
  programs. Origin sets, a secrets vault (`secret://shop` is typed, never shown) and an optional
  guard program decide what leaves.
- ⏪ **Replay, rewind, look ahead.** Sessions replay byte for byte on a virtual clock; agents go
  back to any step without re-sending anything, and try actions on a fork first.
- 🌐 **Real web apps.** Modules and bundlers, web components, forms, scrolling, several tabs,
  file uploads, drag and drop, WebSockets and server-sent events; PDFs, Markdown and plain text
  read as pages.

## 🚀 Quick start

Install the CLI (Linux x86_64/arm64, macOS arm64):

```sh
brew install hupe1980/tap/pageon   # or: cargo install pageon (Rust 1.99+)
```

Then:

```sh
pageon fetch https://example.com --dump ax                      # what an agent sees
pageon fetch https://example.com --act "click e1" --dump ax     # act, then look again
pageon fetch https://example.com --screenshot page.png --marks  # pixels, labelled with refs
```

## 🔌 Connect your agent

**🐚 The shell**, for coding agents (Claude Code, Codex, Copilot) and scripts: a named session
stays open between commands.

```sh
pageon skill --install                         # teaches the agent: .claude/skills/pageon/SKILL.md
pageon open http://localhost:3000 --private-network allow --origin http://localhost:3000
pageon click e5                                # what happened, then what changed
pageon find "sign in"
pageon close
```

**MCP** for Claude Code, Claude Desktop, Cursor or any MCP host (stdio, or Streamable HTTP with
`--http`; MCP `2026-07-28` and the earlier revisions):

```sh
claude mcp add pageon -- pageon mcp
PAGEON_SECRET_SHOP=… claude mcp add shop -- pageon mcp \
  --origin https://shop.example --secret shop@https://shop.example
```

**🐍 Python** (`pip install pageon`, Python 3.11+):

```python
from pageon import Browser

with Browser(origins=["https://shop.example"], secrets=["shop@https://shop.example"]) as browser:
    tab = browser.open("https://shop.example/login")
    print(tab.observe())
    out = tab.type("e4", "secret://shop", submit=True)  # PAGEON_SECRET_SHOP, never shown
    print(out)  # ok type e4 … · navigated to https://shop.example/account
```

**🎭 Puppeteer and Playwright** over CDP, or any **WebDriver BiDi** client, unchanged:

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

The URLs carry a per-run secret, so other local processes cannot take the browser.

**🦀 Rust**:

```rust
use pageon::{Live, LoadOptions, Session};

let session = Session::new(Live::default());
let mut tab = session.open("https://example.com", LoadOptions::default())?;
println!("{}", tab.observe());
println!("{}", tab.act("click e1".parse()?)?);
```

**🤝 Claude's and OpenAI's browser tools** run on pageon: declare its definition, hand it the
model's calls ([details](https://hupe1980.github.io/pageon/docs/toolsets/)).

```python
ts = browser.toolset("browser_toolset_20260801")   # or "computer_toolset_20260801", "computer"
r = client.messages.create(model="claude-opus-5-5", tools=[ts.definition], messages=messages, max_tokens=16000)
messages += [{"role": "assistant", "content": r.content}, {"role": "user", "content": ts.run(r.content)}]
```

**🧩 Embedded**: `pageon::Config` gives a Rust program the CLI's exact session (origins,
secrets, files, recording), and `pageon::mcp::serve_with` serves it.
[mrun](https://github.com/hupe1980/mrun) ships pageon this way (`mrun mcp serve browser`).

**🏋️ BrowserGym and AgentLab** run on pageon with one line:
`pageon.browsergym.register(replace=True)`.

## 📊 Numbers

Observation formats on MiniWoB++ (125 tasks × 3 runs, Qwen3.5-9B at temperature 0, 4,000-token
o200k budget, BrowserGym's action set for every format):

| pageon vs | Task success | 95 % CI | Tokens on real pages |
| --- | --- | --- | --- |
| Playwright MCP snapshot | +3.2 points | [−3.2, +9.9] | 0.27× |
| BrowserGym AXTree | +8.3 points | [+1.9, +14.9] | 0.16× |
| browser-use 1.0 | +18.7 points | [+12.8, +24.8] | 1.02× |

Engine figures from the test suite:

| | |
| --- | --- |
| Native `ax` view = Chromium-based view | 204/204 lines on the corpus pages |
| html5lib tree construction | 1,787/1,792 (the same as html5ever's reference) |
| WPT roles / names | 100 % / 94.2 % |
| Layout vs Chromium | 96.9 % of boxes within 2 px |
| Click (caged) · screenshot · tool call | 2.2 ms · 1.7 ms · 0.07 ms |
| Puppeteer, Playwright, BiDi suites | pass, caged and in-process |

The benchmark lives in [`bench/`](bench) and runs locally at no cost. Every cell is reported,
including the ones pageon loses.

## 🚧 Status

pageon is early: the API may still change between minor versions. The
[threat model](crates/pageon/THREAT_MODEL.md) lists what it defends against and every known gap; read it before pointing an agent at anything that matters. Report
vulnerabilities privately ([SECURITY.md](SECURITY.md)).

What pages can and cannot use is listed under
[web platform](https://hupe1980.github.io/pageon/docs/platform/). Missing APIs are absent, never
faked, so pages and clients can detect them.

## 🧱 Workspace

| Rust crate | |
| --- | --- |
| [`pageon`](crates/pageon) | The engine, the CLI, the sandbox and policy, the MCP, CDP and BiDi servers |
| [`pageon-agent`](crates/pageon-agent) | The agent views: refs, quoting, budgets, diffs |
| [`pageon-js`](crates/pageon-js) | V8, DOM bindings generated from WebIDL, the event loop on a virtual clock |
| [`pageon-layout`](crates/pageon-layout) | CSS, layout with bundled fonts, hit-testing, paint |
| [`pageon-ax`](crates/pageon-ax) | The accessibility tree (HTML-AAM, ARIA, AccName) |
| [`pageon-net`](crates/pageon-net) | Fetch, CORS, cookies, storage, the egress policy |
| [`pageon-dom`](crates/pageon-dom) | The document model and HTML parsing |
| [`pageon-env`](crates/pageon-env) | Network, clock and randomness: live, recorded or replayed |

The Python package `pageon`, with the BrowserGym backend, lives in
[`bindings/python`](bindings/python).

```sh
cargo test --workspace                              # offline: the engine, views and servers
cd conformance/puppeteer && npm ci && node --test   # Puppeteer against `pageon cdp`
```

## 📄 License

Licensed under either of the [MIT license](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option. Third-party material is listed in
[THIRD_PARTY.md](THIRD_PARTY.md).
