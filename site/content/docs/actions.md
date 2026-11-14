+++
title = "Actions and outcomes"
description = "pageon's typed actions (click, type, press, select, hover, scroll, wait, goto, back, tool calls), how they check actionability and settle, and the outcomes and errors they return."
weight = 21
[extra]
group = "Concepts"
nav = "Actions"
+++

An action names an element by its ref and does what a person would: trusted pointer, keyboard
and input events, then the browser's default behaviour (following links, submitting forms,
toggling labels).

## The actions

| Action | Example |
| --- | --- |
| Click an element, or a viewport point | `click e5` · `click 120,40` |
| Type into a field, key by key | `type e3 hello world` · `type e4 secret://shop` |
| Press a key on the focused element | `press Enter` · `press Escape` |
| Select options by value or label | `select e7 Germany` |
| Hover | `hover e2` · `hover 120,40` |
| Scroll the page, a container, or to an element | `scroll down` · `scroll down 300` · `scroll e5 down` · `scroll e12` |
| Let time pass | `wait 500` |
| Navigate | `goto https://example.com` · `back` · `forward` |
| Call a page tool ([WebMCP](@/docs/webmcp.md)) | `tool searchFlights {"from": "TXL"}` |

## Scrolling

Every element in the view is reachable: an action on one below the fold first scrolls it into
view, centred, through any scroll containers, as a person would (`ok click e40 · scrolled to
0,2640 · …`). Scroll yourself to load more of a feed or a lazy section, or to look further
down in a screenshot:

- `scroll down` (or `up`, `left`, `right`) moves one page, 87.5% of what scrolls, as Page Down
  does; `scroll down 300` moves 300 CSS pixels;
- `scroll e5 down` scrolls the container `e5`; `scroll e12` brings `e12` into view;
- the page gets a trusted `wheel` event first, and a page that cancels it is not scrolled.

The outcome says where it went, `already at the end`, or `the page prevented scrolling`. Keys
scroll too (`press End`, `press PageDown`), and in-page links scroll to their target. The view's
header shows the position (`scroll:2640/5000`).

## Before acting: actionability

pageon checks the target against real layout first. An element that is hidden, disabled,
covered by another element, inert behind a modal dialog, or not editable is not acted on; the
error says which and what to do instead.

## After acting: settling

The page runs on a virtual clock. After an action, pageon runs the page until it is idle (timers,
requests and microtasks done) or a time window passes, then reports how it settled. A 5-second
spinner settles in milliseconds of wall time, deterministically.

## Outcomes

```text
ok click e12 · navigated to "https://shop.example/confirm" · 2 request(s) · settled idle after 120 ms
```

An outcome reports what actually happened, not what was attempted:

- the navigation, if any, and refusals by the site (`refused: challenge by …`); single-page
  apps that change the URL in place (`history.pushState`, fragments) report
  `navigated to "…" (same page)`;
- dialogs the page opened (answered by the dialog policy), popups, downloads;
- requests made, and requests held or blocked by the policy;
- new script errors;
- a form the page refused to submit, with each invalid field's ref, name and reason;
- a tool's result, for tool calls;
- how settling ended.

Forms validate as in Chrome: a required field left empty, an email without `@` or a number out
of range blocks the submission, and the outcome says which fields and why (at most five):

```text
ok click e9 · not submitted: e3 "Email" (Please fill out this field.), e5 "Password" (Please lengthen this text to 8 characters or more (you are currently using 5 characters).) · settled idle after 0 ms
```

Messages a page sets itself (`setCustomValidity`, a component's `setValidity`) are marked
`(page data)` and quoted, like other page text. From then on the next observation marks those
fields `invalid` and shows error text the page reveals with CSS; a form the agent has not
touched shows no errors. Component libraries' form controls (form-associated custom elements)
take part like native ones.

`back` and `forward` move through the tab's history like a browser's buttons: within a
single-page app they stay in the page (the app gets `popstate`), elsewhere they load the page
with its saved state. Entries a page added without a click or key from you are skipped, as in
Chrome, so a page cannot trap the agent with `history.pushState`.

## Errors

When an action cannot be done, the error has a kind and guidance the model can follow:

| Kind | Example guidance |
| --- | --- |
| `unknown_ref` | use a ref from the latest observation (observe first) |
| `stale_ref` | the element is gone: observe again and use the new ref |
| `not_actionable` | wait until it is enabled (complete the required fields) or pick another element |
| `no_such_option` | use one of: Small, Medium, Large |
| `navigation` | the page did not load: why, and what to try |
| `secret` | bind the secret to this page's origin |
| `tool` | the page's tool failed: its message, quoted |
| `unsupported` | use another action |

Not yet supported: drag and drop, and file uploads. They fail with `unsupported` rather than
doing something else.
