+++
title = "Security model"
description = "How pageon keeps agents safe on hostile pages: a sandboxed page process, origin sets enforced at egress, approvals, a guard program, quoted views and masked screenshots."
weight = 30
[extra]
group = "Safety"
nav = "Security model"
+++

pageon assumes two things go wrong: pages are hostile (prompt injection, exploits), and the
model sometimes does what a page tells it to. Its defences therefore sit in the engine and the
broker, where neither the page nor the model can reach them.

## The process model

```text
agent         MCP · Python · CDP · BiDi · Rust
  │
broker        network · origin policy · vault · guard · recorder
  │           (one versioned JSON-lines channel)
page process  DOM · V8 · layout   [sandboxed: no files, network or programs]
```

Every page runs in a **caged content process** built with
[procage](https://github.com/hupe1980/procage): Landlock, seccomp and namespaces on Linux,
Seatbelt on macOS. The cage can run JavaScript and nothing else: it cannot read your files,
open a socket or start a program. Its only way out is the channel to the broker, which performs
every request on its behalf.

Files work like secrets: the operator stages each by name for the origins it may be uploaded
to (`--file NAME@ORIGIN=PATH`), the agent uploads by name, and the broker reads the file only
for an `upload` that names it, once, into a page of a bound origin. No agent, page or protocol
client names a host path (CDP and BiDi clients may pass a staged path). A download uploads only
back to its own site.

The broker does not take the page process's word for what matters. Whether a request may reach
the private network comes from the options the broker itself sent; a secret is handed out only
while the agent's own `type … secret://name` runs, once; a recorded step is the broker's
description of the command it sent. `HttpOnly` cookies — the session cookies, which scripts
never see — live only in the broker: it adds them to requests and takes them out of
responses, so even a compromised page process cannot read them. Saved state and the
Puppeteer, Playwright and BiDi cookie views include them as usual.

`--no-sandbox` runs pages in-process for debugging; they then run with your rights.

## Origin sets

```sh
pageon mcp --origin https://shop.example --read-origin https://cdn.shop.example
```

With origin sets, every request is classified where it leaves:

| Class | Request | Default |
| --- | --- | --- |
| `read` | `GET`, `HEAD`, `OPTIONS` to a task origin | Sent |
| `write` | Any other method to a read-write origin | Sent, or asked if an approver is configured |
| `outside` | Anything to an origin outside the sets, or a write to a read-only origin | Refused |

`https://*.example.com` admits subdomains, and an origin needs its scheme: `--origin
shop.example` is an error that names the fix (`https://shop.example`). The refusal is reported in the action's outcome and
the request log, and the page sees an ordinary network error.

## Approvals

`pageon fetch --approve` asks on the terminal before every write. From Rust, `Policy::approver`
takes a closure. An approver sees structured metadata about the request (method, URL, origin,
class, labels, the action that caused it), never page text, so a page cannot talk it into
anything.

## Guard

A guard is any program that decides about requests: a script, a rules engine, a local model, a
remote critic.

```sh
pageon mcp --origin https://shop.example --guard ./guard.py
```

pageon writes one JSON effect per line to its stdin, for every request except reads of origins
already contacted:

```json
{"id": 7, "method": "POST", "url": "https://shop.example/orders", "origin": "https://shop.example",
 "class": "write", "labels": ["first-contact"],
 "cause": {"action": "click e12", "target": "button e12 \"Pay now\"", "target_untrusted": true}}
```

It answers one verdict per line, echoing the effect's `id`: `{"id": 7, "verdict": "allow"}`,
`{"id": 7, "verdict": "deny", "reason": "…"}` or `{"id": 7, "verdict": "ask"}` (defer to the
approver). It **fails closed**: a crash, malformed output, a verdict without the asked `id` (an
extra or late answer can never approve another request) or no answer within 10 seconds denies,
from then on. `id` comes first in each line, so a shell guard can read it with
`sed 's/^{"id":\([0-9]*\).*/\1/'`.

What the guard sees is clipped: URLs at 2,048 characters, with vault secrets shown as their
`secret://name` handles; the target line (page text) at 300 characters, marked
`target_untrusted`. A lookahead's reads do not count as contact, so the real first request to
an origin still carries `first-contact`.

Labels help guards decide: `secret:<name>` (the request carries that secret to its own origin),
`first-contact` (no request went to this origin before), `tool:<name>` and `consequential` (a
[WebMCP](@/docs/webmcp.md) tool caused it).

## The private network

Pages cannot reach `localhost`, private ranges or link-local addresses (cloud metadata
endpoints), in any IP encoding, through redirects or through DNS answers. `--private-network
allow` lifts this for local development.

## Local files

A web page cannot load `file:` URLs: frames, scripts, stylesheets, images, `fetch` and XHR to
the disk are refused and logged as `blocked … file URL from a web page`. Only a page opened
from a file loads the files next to it.

Agents cannot open local pages either: `pageon mcp`, `cdp` and `bidi` refuse `file:` URLs
unless you start them with `--allow-files`. `pageon fetch ./page.html`, typed by you, works as
always.

## Takeover

When an agent hands the page to a person (`browser_takeover`), what the person clicks and
types is theirs: it reaches the page as trusted input but is not reported to the agent or
recorded as a step. The agent sees the page as it is afterwards. The page's own requests are
recorded as usual, so `pageon replay` of such a session diverges at the takeover.

## Frames

A page's documents of one origin share a JavaScript engine and reach each other's windows,
documents and nodes, as in a browser. Documents of other origins run in other engines, so no
object crosses between origins. They see each other's windows only as cross-origin windows,
where `postMessage`, `parent`, `top`, `frames`, `length`, `closed`, `focus`, `blur` and `close`
work and anything else throws `SecurityError`. A frame cannot navigate its parent or the top
page. `postMessage` delivers only to the origin the sender named, with the sender's real origin
on the event. At most 1024 messages wait at once.

## Protocol clients

Puppeteer's and Playwright's helpers (utility worlds) and WebDriver BiDi sandboxes run in
isolated worlds: their own JavaScript over the page's document, so a page that redefines
built-ins cannot change what a client computes.

## What the model sees

- **Quoted text.** All page text in a view is quoted, and so are the URLs, file names, page
  errors and dialog messages in outcomes; a page cannot forge a view or outcome line.
  `pageon fetch` escapes control characters, so page text cannot drive your terminal either.
- **Marks.** Hidden text, invisible input traps and third-party frames are labelled; pageon
  refuses to type into a hidden field.
- **Masked pixels.** `--mask` paints over third-party frames and text a person cannot read.
- **No secrets.** [Secrets](@/docs/secrets.md) never appear in views, screenshots, outcomes,
  protocol messages or recordings. Recordings keep each `HttpOnly` cookie a server set with its
  value replaced (`sid=pageon-redacted; Path=/`), so a shared recording carries no session.

## Known limits

pageon is pre-release, and its [threat model](https://github.com/hupe1980/pageon/blob/main/THREAT_MODEL.md)
lists every known threat class with its status, including the open ones. The most important
today:

- **Defaults are permissive.** Without `--origin`, an approver or a guard, every request is
  sent. Configure origin sets for every task that matters.
- **Local endpoints.** `pageon cdp` has no token: any local process can connect while it runs.
  An idle connection cannot block other clients, and only a real close command ends it.
- **Files at rest.** Recordings and saved state hold cookies and page content. pageon writes
  them readable by you only, but does not encrypt them; keep them private.
- **A compromised page process.** It still decides which origin a typed secret is for (the
  secrets check at egress bounds where it can go), the SameSite context of its requests (a lie
  sends a cookie cross-site, never reveals it), and everything a script can see. The cage is
  the boundary that holds.

Origin sets are the hard boundary. The secrets check at egress catches common encodings, not a
page that splits, reverses or re-encodes a value before sending it to an allowed origin; keep
untrusted third parties out of the origin sets.

Report a vulnerability privately through
[GitHub's advisories](https://github.com/hupe1980/pageon/security/advisories/new).
