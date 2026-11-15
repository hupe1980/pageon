+++
title = "MCP server"
description = "pageon as an MCP browser server for Claude Code, Claude Desktop, Cursor and any MCP host: typed outcomes, tabs, reset, lookahead and takeover."
weight = 11
[extra]
seo_title = "MCP browser server for Claude Code, Cursor and any MCP host"
group = "Interfaces"
nav = "MCP server"
+++

`pageon mcp` is an MCP server over stdio, or over Streamable HTTP with `--http`. Pages run in
the sandbox, and every option of the CLI applies: origins, secrets, guard, recording. Local
`file:` pages are off unless you pass `--allow-files`.

It speaks MCP `2026-07-28`, where every request carries its own version and capabilities and
`server/discover` replaces the handshake, and the `initialize` revisions from `2024-11-05` to
`2025-11-25`, in the same process. Clients pick by what they send; nothing to configure.

## Add it to your host

```sh
# Claude Code
claude mcp add pageon -- pageon mcp

# Scoped to one site, with a credential the model never sees
PAGEON_SECRET_SHOP=… claude mcp add shop -- pageon mcp \
  --origin https://shop.example --secret shop@https://shop.example
```

Claude Desktop, Cursor and other hosts take the same command in their JSON config:

```json
{
  "mcpServers": {
    "pageon": { "command": "pageon", "args": ["mcp", "--origin", "https://shop.example"] }
  }
}
```

[mrun](https://github.com/hupe1980/mrun), a runner for local models, has pageon built in:
`mrun mcp add browser` gives its chats a browser, and `mrun mcp serve browser` serves it to any
MCP client. It embeds pageon as a library (see [Rust](@/docs/rust.md#embedding-a-configured-browser)).

### Over HTTP

```sh
pageon mcp --http 8931 --origin https://shop.example
# MCP endpoint: http://127.0.0.1:8931/mcp/3f9c…   (on stderr)
claude mcp add --transport http pageon http://127.0.0.1:8931/mcp/3f9c…
```

The endpoint serves `2026-07-28` on 127.0.0.1 only, at a path with a per-run token: keep the
printed URL private. Requests from web pages (with an `Origin`) are refused unless
`--allow-origin` names them. Replies are JSON, or a short event stream when a call sends
progress. Clients on older revisions use stdio. `--save-state` needs stdio.

## Tools

| Group | Tools |
| --- | --- |
| Read | `browser_open`, `browser_observe`, `browser_view` (`markdown`, `text`, `requests`, `errors`), `browser_screenshot` |
| Tabs | `browser_tabs` (`new`, `select`, `close`, `list`) |
| Act | `browser_click`, `browser_type`, `browser_upload`, `browser_press`, `browser_select`, `browser_paste`, `browser_hover`, `browser_drag`, `browser_scroll`, `browser_wait`, `browser_back`, `browser_forward`, `browser_call_tool` |
| Explore | `browser_lookahead`, `browser_reset` |
| Reuse | `browser_workflow_save`, `browser_workflow_run` |
| People | `browser_takeover` |

Every action answers with its [outcome](@/docs/actions.md#outcomes), what changed on the page,
and its step number. A failure is an `isError` result with a `kind` and guidance on what to do
next, so the model can recover by itself.

```text
ok click e12 · 1 request(s) · settled idle after 40 ms
@diff v3→v4
+ # "Your cart" after:e8
+ button e31 "Checkout" after:e30
~ link e9 "Cart (1)"
step 4
```

## Several tabs

Every tool acts on the current tab. `browser_tabs {"action": "new", "url": …}` opens another
(at most 8) and makes it current; `select` switches back with what changed there since the agent
last looked; `close` closes one; `list` shows them. Handles are `t1`, `t2`, … and never reused.
While several tabs are open, every reply ends with its tab (`step 7 · tab t2`), and
`structuredContent.tab` always names the current one. Refs belong to their tab.

Popups (`window.open`, `target=_blank`) are reported, not opened: the reply says how to open
one as a tab, so nothing loads that the agent did not choose. `browser_reset` and
`browser_workflow_save` rebuild one tab, so they refuse once a session has had several tabs.

## Going back and looking ahead

- **`browser_reset {}`** undoes the last step; `{"step": k}` goes back to any step. The page is
  rebuilt exactly from the session's own recording: nothing is sent again, so an order placed
  later is not placed twice, and the refs are the ones the agent had then.
- **`browser_lookahead {"actions": ["click e12"]}`** tries actions on a fork of the page and
  returns what the agent would see. The page, its storage and the recording are untouched, and
  any request that could change something is held, not sent. See
  [lookahead](@/docs/lookahead.md).

## Screenshots for vision models

`browser_screenshot {"marks": true}` returns the image with every visible element labelled by
its ref, plus a legend, so a vision model can answer `click e14`. `{"mask": true}` paints over
third-party frames and text a person cannot read.

## Handing over to a person

`browser_takeover {"reason": "Please solve the CAPTCHA"}` serves a live view of the tab at a
loopback URL and returns when the person presses Done. What the person types stays theirs: it
is not shown to the agent or recorded as a step. pageon never solves CAPTCHAs itself.

The URL goes to the person, never to the model: a client that supports MCP URL elicitation
shows the link to its user (on `2026-07-28` as a multi-round-trip request: the call returns
`input_required`, and the client retries once its user has the link; declining closes the view);
other clients get it in the progress notification's `_meta.url`, and the operator on stderr.
Tool results never contain it, so a hijacked model cannot hand the live view to a page. While a
person has the page, the agent's other calls wait; a call made before the person starts closes
the view.

## Claude's and OpenAI's tools

Applications that call a model themselves can have `pageon mcp` run Anthropic's browser and
computer toolsets and OpenAI's computer tool: `pageon/toolset/definition` and
`pageon/toolset/run` sit beside MCP and are never in `tools/list`. See
[Claude & OpenAI tools](@/docs/toolsets.md).

## Page tools (WebMCP)

Tools a page registers with `document.modelContext.registerTool` appear first in the
observation, with the origin and script that registered them. `browser_call_tool {"name":
"searchFlights", "arguments": {...}}` calls one. Page tools are deliberately not turned into MCP
tools named by the page; see [WebMCP](@/docs/webmcp.md).
