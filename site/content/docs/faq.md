+++
title = "FAQ"
description = "pageon compared with headless Chrome and Playwright MCP, which sites work, bot detection and CAPTCHAs, platforms and contributing."
weight = 52
[extra]
group = "Reference"
+++

## How is pageon different from headless Chrome with Playwright MCP?

Playwright MCP drives Chromium, a browser built for people and testing, and serializes its
accessibility snapshot for the model. pageon is an engine built for agents: its view uses about
a quarter of the tokens on real pages with at least the same task success, its refs survive
re-renders, its actions report what happened, pages run in a sandbox with an egress policy, and
sessions replay, rewind and fork. It also speaks CDP, so Playwright code can run on it.

## Will my site work?

Most do. Tested: server-rendered sites and forms, script-heavy pages (MiniWoB++'s jQuery and
d3), apps bundled by Vite, esbuild or webpack, web components (Lit runs as is), WebSockets and
server-sent events, drag and drop. Pages that need workers or canvas readback do not work
fully; the [web platform](@/docs/platform.md) page has the full list, and `pageon fetch URL
--dump errors` shows what a page tried and failed to use.

## Does pageon get past bot detection?

By default it does not try: it says it is an agent, honours robots.txt and can sign its requests
with Web Bot Auth, so site operators can recognise and allow it. When a site refuses it, the
outcome says so (blocked, challenge, rate limited). An opt-in Chrome profile exists for sessions
a person stands behind. pageon never solves CAPTCHAs; a person can take over through a live
view.

## Can a page steal my credentials through the agent?

Secrets are typed as `secret://name` handles. The model never sees the value, views and
screenshots never show it, and pageon refuses to send it to any origin it is not bound to. See
[secrets](@/docs/secrets.md) and the [security model](@/docs/security.md).

## Is it deterministic?

Yes. Pages run on a virtual clock with seeded randomness, and every response can be recorded.
A recorded session replays to the same page at every step, in the sandbox or in-process.

## Which platforms are supported?

Linux (x86_64, aarch64) and macOS. Not Windows.

## How do I contribute?

The repository is on [GitHub](https://github.com/hupe1980/pageon). `cargo test --workspace`
runs the engine, view and server tests offline; the Puppeteer and Playwright suites live in
`conformance/`.
