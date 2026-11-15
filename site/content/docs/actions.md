+++
title = "Actions and outcomes"
description = "pageon's typed actions (click, type, upload, drag, scroll, …): how they check actionability, settle on virtual time, and report outcomes and errors."
weight = 21
[extra]
seo_title = "Browser actions for AI agents: outcomes and errors"
group = "Concepts"
nav = "Actions"
+++

An action names an element by its ref and does what a person would: trusted pointer, keyboard
and input events, then the browser's default behaviour (following links, submitting forms,
toggling labels).

## The actions

| Action | Example |
| --- | --- |
| Click an element, or a viewport point; another button, a double or triple click, modifiers | `click e5` · `click 120,40` · `click e5 right` · `click e5 double` · `click e2 ctrl` |
| Type into a field, key by key; or at the end of the focused one | `type e3 hello world` · `type e4 secret://shop` · `type "hello"` |
| Upload staged files into a file input, the chooser a button opens, or a drop zone | `upload e9 cv` · `upload e9 cv,photo` · `upload e9 download:1` |
| Press a key, a chord or a sequence on the focused element | `press Enter` · `press ctrl+a` · `press shift+Tab` · `press Down Down Enter` · `press Tab x3` |
| Hold a key (ms of virtual time, with key repeat) | `hold shift 2000` |
| Select options by value or label, or text in any element | `select e7 Germany` · `select e5 quick brown` |
| Paste text into an element, or at the focused caret | `paste e9 Thanks for the review` · `paste "text"` |
| Hover | `hover e2` · `hover 120,40` |
| Drag an element onto another | `drag e3 e7` |
| Scroll the page, a container, what is at a point, or to an element | `scroll down` · `scroll down 300` · `scroll e5 down` · `scroll 640,380 down 300` · `scroll e12` |
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

Pages with open `WebSocket` or `EventSource` connections also get real time: an action that sent
a frame waits up to one second for the first reply, and `wait` lets real time pass (up to the
requested time, ending early when a message arrives). The outcome counts the frames it sent
(`1 frame(s) sent`); messages that arrive later are in the next observation.

## Outcomes

```text
ok click e12 · navigated to "https://shop.example/confirm" · 2 request(s) · settled idle after 120 ms
```

An outcome reports what actually happened, not what was attempted:

- the navigation, if any, and refusals by the site (`refused: challenge by …`); single-page
  apps that change the URL in place (`history.pushState`, fragments) report
  `navigated to "…" (same page)`;
- dialogs the page opened (answered by the dialog policy), popups, downloads;
- what the action put on the clipboard, as page data (`copied (page data) "sk-…"`);
- requests made, and requests held or blocked by the policy;
- new script errors;
- a form the page refused to submit, with each invalid field's ref, name and reason;
- a tool's result, for tool calls;
- how settling ended.

When an action changed nothing yet and a timer it set (directly or through its own timers) is
still due, within 10 seconds of virtual time, the outcome is `unknown` instead of `ok`, with
the wait that lets the page finish (`status: "unknown"`, `due_ms` in JSON):

```text
unknown click e4 · nothing changed yet; the page set a timer due in 3000 ms (browser_wait 3000)
```

Timers the page set before the action (carousels, polling) never make an action unknown, and
an action that sent a request or changed the page is `ok`.

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
| `file` | stage the file for this site (`--file NAME@ORIGIN=PATH`) |
| `tool` | the page's tool failed: its message, quoted |
| `script` | an `eval` (the toolsets' `javascript_exec`) threw: its message, quoted |
| `unsupported` | use another action |

## Editing

`type e3 text` selects what the field or editor holds and types over it, key by key, through
`beforeinput` and `input` as a keyboard does, so rich-text editors (ProseMirror, Lexical, Quill)
update their own documents. `type "text"` types at the caret. The keys edit as in Chrome: the
arrows (with Shift to select), Home and End, Backspace and Delete (with Ctrl, by word), Enter
for a new paragraph, Shift+Enter for a line break, `ctrl+a`, and the editor's own shortcuts
(`press ctrl+b`).

`select e5 quick brown` selects that text inside `e5` (on a `<select>` it chooses options). Then
`press ctrl+b` formats it, `type "…"` replaces it, `press ctrl+c` copies it.

The clipboard is the session's, shared by its tabs and never the host's. `paste e9 text` puts the
text on it and pastes; `press ctrl+v` pastes what is there. Pages receive pasted data in their
`paste` event and cannot read the clipboard otherwise. Password fields and secrets are never
copied.

## Files

Files are uploaded by name, never by path: the operator stages each one for the sites it may go
to (`--file cv@https://jobs.example=./cv.pdf`; `files=` in Python), and the broker hands it out
only for the agent's own `upload` naming it. A click that opens a file chooser says so
(`file chooser for e9 (upload e9 NAME)`). Downloads come back as handles (`download:1`, with
the file's name, type and size) and can be uploaded again to the site they came from.

## Drag and drop

`drag e3 e7` presses on e3, moves in a few steps and releases on e7, as a person does: pages
that drag with pointer events (sortable lists, sliders) see trusted events, and draggable
elements (`draggable`, links, images) run HTML drag and drop, with the data readable in
`dragstart` and `drop` only. Elements that accept drops are listed as `droptarget eN "Done"`,
so columns and drop zones have refs. The outcome says what happened: `dragged (dropped)`,
`dragged (not accepted: no drop)` when the target never accepted it, or `pointer drag`.
`upload e9 cv` on a drop zone drops the staged file there. Drags stay within one frame and are
not part of [workflows](@/docs/workflows.md).
