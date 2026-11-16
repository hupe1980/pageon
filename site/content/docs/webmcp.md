+++
title = "WebMCP page tools"
description = "Tools pages declare with WebMCP's document.modelContext are listed first in the agent's view, with their provenance, and called as actions."
weight = 23
[extra]
seo_title = "WebMCP page tools for agents"
group = "Concepts"
nav = "WebMCP"
+++

[WebMCP](https://github.com/webmachinelearning/webmcp) lets a page declare tools for agents, so
an agent can call `searchFlights({from, to})` instead of operating three widgets. pageon
implements the page side and presents the tools to agents with provenance a page cannot forge.

```js
document.modelContext.registerTool({
  name: 'searchFlights',
  description: 'Find flights between two airports',
  inputSchema: { type: 'object', properties: { from: { type: 'string' }, to: { type: 'string' } } },
  execute: async ({ from, to }) => ({ flights: await search(from, to) }),
  annotations: { readOnlyHint: true },
});
```

## Forms as tools

A form with `toolname` and `tooldescription` is a tool too (WebMCP's declarative API, as Chrome
ships it). Its parameters are its named fields: text is a string, numbers keep `min`/`max`,
checkboxes are booleans, selects and radio groups list their values, `required` fields are
required, and `toolparamdescription` or the field's label describes each.

```html
<form toolname="search_flights" tooldescription="Find flights" toolautosubmit>
  <label>From <input name=from required></label> <label>To <input name=to required></label>
  <button>Search</button>
</form>
```

Calling it fills the fields as typing and selecting do (`secret://` handles work), fires
`toolactivated`, and then:

- with `toolautosubmit`, submits the form. The `submit` event's `agentInvoked` is `true`; the
  page can `preventDefault()` and `respondWith(promise)` to answer, otherwise the answer is the
  next page's first `application/ld+json` block. Such a call is labelled consequential;
- without it, the form is filled and the result names its submit button, which the agent
  clicks.

## In the view

Tools are listed first, whenever the set changes:

```text
tool "searchFlights" "Find flights between two airports" (from: string, to: string) origin https://air.example script https://air.example/app.js read-only
```

The **origin**, the **script** that registered the tool and whether it came from a
**third-party frame** are recorded by the engine at registration. A page cannot change them
later, which is what defeats tools injected by third-party scripts.

## Calling a tool

```text
tool searchFlights {"from": "TXL", "to": "SFO"}
```

A call is an action like a click: it is recorded, replays, compiles into
[workflows](@/docs/workflows.md) and can be [looked ahead](@/docs/lookahead.md). Over MCP use
`browser_call_tool`; in Python `tab.call_tool("searchFlights", **{"from": "TXL", "to": "SFO"})`.

## Hints only tighten

A tool's own annotations never relax anything:

- `consequentialHint` labels every request the call causes and sends them to the
  [guard](@/docs/security.md#guard);
- `readOnlyHint` changes nothing: a "read-only" tool's POST is still a write;
- results are always treated as untrusted page data.

pageon offers one generic `browser_call_tool` over MCP rather than an MCP tool per page tool:
putting names chosen by a page into the host's tool list is how tool poisoning reaches a model.

Registration follows the spec: secure contexts only, names of 1–128 characters, cross-origin
frames only with `<iframe allow="tools">`, and aborting the tool's `signal` unregisters it.
Form tools follow the same rules; at most 64 are listed per document.

## For automation clients

Over CDP, Chrome's `WebMCP` domain lists and runs a page's tools, so Puppeteer's `page.webmcp`
works as with Chrome:

```js
const tool = page.webmcp.tools().find((t) => t.name === 'search_flights');
const { status, output } = await tool.execute({ from: 'TXL', to: 'SFO' });
```

Calls run as the agent's do: through the session's origins, guard and labels.
