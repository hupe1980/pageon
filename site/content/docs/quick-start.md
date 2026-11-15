+++
title = "Quick start"
description = "Install pageon, see a page the way an agent sees it, act on it, and connect your agent in five minutes."
weight = 1
[extra]
seo_title = "Quick start: install pageon and connect your agent"
group = "Start"
+++

## Install

pageon is pre-release and builds from source with Rust 1.99 or newer:

```sh
git clone https://github.com/hupe1980/pageon
cargo install --path pageon/crates/pageon
pageon --version
```

pageon runs on Linux (x86_64, aarch64) and macOS. Pages run in a sandbox by default: Landlock,
seccomp and namespaces on Linux, Seatbelt on macOS. No root and no helper binaries.

## See a page as an agent does

```sh
pageon fetch https://shop.example/checkout --dump ax
```

```text
@page v1 "Checkout – Acme" https://shop.example/checkout load:complete budget:4000/o200k
## main
# "Checkout"
textbox e5 "Email" ="ann@example.com" required
combobox e6 "Country" ="Germany" {"Germany"|"France"|"Spain"|+2}
checkbox e7 "Save address" checked
button e12 "Pay now"
```

This is the **`ax` view**: landmarks, headings, quoted text and every element an agent can act on,
with a short ref (`e12`). The header line says which page this is, whether it finished loading, and
the token budget the view was cut to. [The agent view](@/docs/views.md) explains every part.

Other views of the same page:

```sh
pageon fetch https://example.com --dump markdown   # the main content as Markdown, links with refs
pageon fetch https://example.com --dump errors     # script errors, with locations
pageon fetch https://example.com --dump requests   # every request, and why any was blocked
```

## Act

Actions use the refs from the view. Each one prints what happened.

```sh
pageon fetch https://shop.example/checkout --act "type e5 bob@example.com" --act "click e12" --dump ax
```

```text
ok type e5 "bob@example.com" · settled idle after 0 ms
ok click e12 · navigated to "https://shop.example/confirm" · 2 request(s) · settled idle after 120 ms
```

See [actions](@/docs/actions.md) for `type`, `press`, `select`, `hover`, `wait`, `goto`, `back`
and the errors they can return.

## Take a screenshot

```sh
pageon fetch https://example.com --screenshot page.png           # the viewport
pageon fetch https://example.com --screenshot som.png --marks    # every element labelled with its ref
```

## Connect your agent

| Your agent speaks | Start here |
| --- | --- |
| MCP (Claude Code, Claude Desktop, Cursor, …) | [MCP server](@/docs/mcp.md): `claude mcp add pageon -- pageon mcp` |
| Python | [Python](@/docs/python.md): `pageon.Browser` |
| Puppeteer or Playwright | [Puppeteer & Playwright](@/docs/cdp.md): `pageon cdp` |
| WebDriver BiDi | [WebDriver BiDi](@/docs/bidi.md): `ws://127.0.0.1:9222/session` |
| BrowserGym or AgentLab | [BrowserGym](@/docs/browsergym.md): `pageon.browsergym.register()` |
| Rust | [Rust API](@/docs/rust.md): `pageon::Session` |

## Keep it safe

Before you point an agent at a site that matters, read the [security model](@/docs/security.md).
In short: limit the origins (`--origin https://shop.example`), keep credentials in the vault
(`--secret shop@https://shop.example`), and let a person or a [guard](@/docs/security.md#guard)
decide about writes.
