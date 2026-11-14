+++
title = "MCP server"
description = "Use pageon as an MCP browser for Claude Code, Claude Desktop, Cursor or any MCP host: nineteen browser tools, typed outcomes, step numbers, reset, lookahead and human takeover."
weight = 11
[extra]
group = "Interfaces"
nav = "MCP server"
+++

`pageon mcp` is an MCP server over stdio. Pages run in the sandbox, and every option of the CLI
applies: origins, secrets, guard, recording. Local `file:` pages are off unless you pass
`--allow-files`.

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

## Tools

| Group | Tools |
| --- | --- |
| Read | `browser_open`, `browser_observe`, `browser_view` (`markdown`, `text`, `requests`, `errors`), `browser_screenshot` |
| Act | `browser_click`, `browser_type`, `browser_upload`, `browser_press`, `browser_select`, `browser_hover`, `browser_scroll`, `browser_wait`, `browser_back`, `browser_forward`, `browser_call_tool` |
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
(protocol `2025-11-25`) shows the link to its user; other clients get it in the progress
notification's `_meta.url`, and the operator on stderr. Tool results never contain it, so a
hijacked model cannot hand the live view to a page.

## Page tools (WebMCP)

Tools a page registers with `document.modelContext.registerTool` appear first in the
observation, with the origin and script that registered them. `browser_call_tool {"name":
"searchFlights", "arguments": {...}}` calls one. Page tools are deliberately not turned into MCP
tools named by the page; see [WebMCP](@/docs/webmcp.md).
