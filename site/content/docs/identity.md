+++
title = "Identity and robots.txt"
description = "pageon says it is an agent: honest user agent, robots.txt, Web Bot Auth signatures, refusals reported instead of CAPTCHAs solved."
weight = 32
[extra]
seo_title = "Honest agent identity, Web Bot Auth and robots.txt"
group = "Safety"
nav = "Identity"
+++

pageon tells sites who it is. That keeps agents on the right side of site operators, and it lets
operators allow good agents instead of blocking all of them.

## The default: an honest agent

- The user agent says `pageon`, and `navigator.webdriver` is `true`.
- **robots.txt** is honoured (RFC 9309, product token `pageon`). `--robots ignore` is for
  sessions a person directs.
- When a site turns the agent away, the outcome says so instead of returning a challenge page:
  `refused: challenge by cloudflare …`, with the kind (blocked, challenge, rate limited) and the
  vendor. The CLI exits with 6.
- pageon never solves CAPTCHAs. A person can step in through
  [takeover](@/docs/mcp.md#handing-over-to-a-person).

## Signed requests: Web Bot Auth

With an identity, every request carries an HTTP Message Signature (RFC 9421, Ed25519) that
sites and CDNs can verify against a key directory you host.

```sh
pageon identity new --agent https://bot.example         # writes pageon-identity.json (0600)
pageon identity directory > http-message-signatures-directory   # host it; headers on stderr
pageon mcp --identity pageon-identity.json                # or PAGEON_IDENTITY=…
```

Documents, scripts, styles, frames, `fetch` and redirects are all signed, in the sandbox too.
Signatures never end up in recordings.

## Opt-in: a coherent Chrome

Some sites only work with a mainstream browser. A build with `--features chrome` adds
`--profile browser-compatible`: one pinned Chrome version on Linux, coherent at every layer (the
TLS ClientHello, HTTP/2 settings, headers, client hints and `navigator.userAgentData`), and
tested for that coherence. It is a choice for sessions a person stands behind; the default stays
honest.
