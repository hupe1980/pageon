+++
title = "Claude & OpenAI browser tools"
description = "Run Anthropic's browser and computer toolsets and OpenAI's computer tool on pageon: declare one tool, hand pageon the model's calls, return its results."
weight = 17
[extra]
seo_title = "Claude browser toolset and OpenAI computer use on pageon"
group = "Interfaces"
nav = "Claude & OpenAI tools"
+++

Anthropic and OpenAI define browser tools of their own, and leave running them to you. pageon
runs them. You declare pageon's definition as a tool, give pageon the model's calls, and send
back what it returns. Refs, tabs, recording, replay and the egress policy are the session's
own, as for every other interface.

| Tool | Models | What the model sends |
| --- | --- | --- |
| `browser_toolset_20260801` (Anthropic) | Claude 4.8 and 5.x | 31 members: reads of the page with refs, clicks on refs or points, typing, keys, tabs |
| `computer_toolset_20260801` (Anthropic) | Claude 5.x (Opus 5.5 and Sonnet 5.5 accept only this form) | 17 members: screenshots and coordinates |
| `computer` (OpenAI) | OpenAI's computer-use models | batches of pointer and keyboard actions; a screenshot comes back |

## Python

```python
from anthropic import Anthropic
from pageon import Browser

client = Anthropic()
with Browser(origins=["https://shop.example"]) as browser:
    ts = browser.toolset("browser_toolset_20260801")
    messages = [{"role": "user", "content": "Find the cheapest blue mug on shop.example"}]
    while True:
        r = client.messages.create(model="claude-opus-5-5", max_tokens=16000,
                                   tools=[ts.definition], messages=messages)
        messages.append({"role": "assistant", "content": r.content})
        if r.stop_reason != "tool_use":
            break
        messages.append({"role": "user", "content": ts.run(r.content)})
```

OpenAI is the same with `browser.toolset("computer")`: `tools=[ts.definition]`, and
`ts.run(response.output)` returns the `computer_call_output` items.

`run(blocks, confirm=fn)` asks `fn(member, input)` before each call that may change a page or
send something, and stops the turn when it answers `False`. For OpenAI, `confirm` decides calls
with pending safety checks; without approval they do not run. Writes also go through the
[egress policy](@/docs/security.md) whatever the model asked.

## Any language

`pageon mcp` serves two JSON-RPC methods beside MCP. They are never in `tools/list`, so no MCP
host offers them to a model.

```json
{"jsonrpc": "2.0", "id": 1, "method": "pageon/toolset/definition",
 "params": {"toolset": "browser_toolset_20260801"}}
{"jsonrpc": "2.0", "id": 2, "method": "pageon/toolset/run",
 "params": {"toolset": "browser_toolset_20260801", "calls": [ …the response's tool_use blocks… ]}}
```

`run` answers `{"results": [...]}`, one result per call, in the vendor's format. Over
`--http`, the methods sit behind the endpoint's token like MCP.

## What pageon does with each member

- **Refs** are pageon's own (`e12`): stable across actions, relocated when the page re-renders an
  element, reissued on navigation. A ref pageon no longer knows is answered with the vendor's
  stale-ref error, so the model reads the page again.
- **`read_page`** is the [agent view](@/docs/views.md): the elements in the viewport by default,
  only the interactive ones, or the whole page (`filter: "all"`); `depth` levels of the tree;
  `ref` for one element's subtree. Page text is quoted and hidden text marked, as everywhere. The
  output fits a 12,000-token budget and the vendor's 50,000-character cap, and says when it was
  cut.
- **`find`** matches the words of the query against each element's role, name, value and the
  text before it, and returns at most 20 lines with refs. pageon runs no model, and the result
  says it matched words.
- **Coordinates** are viewport pixels, the same as screenshot pixels. A coordinate click is
  hit-tested like a person's, and the result names what it reached
  (`Clicked button e12 "Pay now" at 640,380.`), marked when it lies in another origin's frame.
- **`zoom`** paints the region magnified up to 2×; coordinates stay in the full screenshot's
  space.
- **Keys** take chords and sequences (`ctrl+a`, `shift+Tab`, `Down Down Return`) and the names
  computer-use models send (`Return`, `BackSpace`, `Page_Down`, `super`). `ctrl+a` in a field
  selects its value, so typing replaces it. `hold_key` holds on the virtual clock, with Chrome's
  key repeat.
- **`form_input`** sets a field as a person would: text is replaced, a `<select>` gets the
  option by label or value, a checkbox is clicked only if its state differs.
- **`file_upload`** takes staged files by name as `document_ids` (`--file NAME@ORIGIN=PATH`). It
  refuses `paths`: the model never names files on your disk. It is offered only when files are
  staged.
- **`read_console`** and **`read_network`** return what the page logged and requested since the
  last read, redacted like every view. Ask for them with `toolset(name, console=True,
  network=True)`.
- **`javascript_exec`** is off unless the operator allows it (`--allow-javascript-exec`,
  `Browser(allow_javascript_exec=True)`). It runs in an isolated world over the page's document:
  its DOM, cookies and same-origin requests, not its script globals, so a page cannot falsify
  what the model reads.
- **Tabs** are pageon's tab handles (`t1`, `t2`, …), at most 8. A call with a `tab_id` runs in
  that tab and leaves the active tab active. Titles and URLs in the inventory are cleaned of
  control and line-separator characters.
- **Batches** follow the vendor's contract: calls run in order, and the first failure stops the
  turn; every later call is answered with the vendor's not-executed text.
- **Recording**: each acting call is a step, so sessions replay and
  [reset](@/docs/replay.md) as any other; reads are not steps.
