+++
title = "Record, replay and reset"
description = "Record sessions step by step and replay them byte for byte on a virtual clock: offline debugging, checks against live sites, exact reset."
weight = 40
[extra]
seo_title = "Record, replay and reset browser sessions"
group = "Reproducible sessions"
nav = "Record & replay"
+++

Everything nondeterministic in pageon (the network, the clock, random numbers) goes through one
seam. Record it, and a session runs again exactly, offline.

## Record

```sh
pageon fetch https://shop.example/login --act "type e3 ada" --act "click e5" --record ep.json
pageon mcp --record session.json         # every tool call becomes a step
```

A recording is readable JSON: every response, realtime messages (WebSocket and server-sent
events, with the virtual time each was delivered at, and the frames the page sent), the clock
and random draws, and the agent's steps
(the action, its target as the agent saw it, and a hash of the page after it). Secrets and
signatures are never written.

## Replay

```sh
pageon replay ep.json            # ✓ per step; exit 5 at the first divergence
pageon replay ep.json --to 1     # the agent view as it was at step 1
pageon replay ep.json --live     # the same steps against the live site: has it changed?
pageon replay ep.json --live --approve   # …and ask before re-sending each write
```

A replay re-runs every step offline and checks the page against the recording. When a step
diverges, it says how: a request the recording does not have, an action that failed, or a page
that differs, with a diff of the view.

A live replay runs under your options, not the recording's: `--private-network`, `--robots`,
`--identity` and `--profile` come from the command line. Recorded writes (a form post, an
order) are held, so a replay never places an order twice by accident; the divergence names the
held request, and `--approve` asks for each one instead.

Realtime connections replay offline too: messages arrive at their recorded times, and a reply
waits until the replayed page has sent its question. A connection still open when a reset or
replay ends is closed: a live socket cannot be resumed.

## Open a run in Playwright's trace viewer

```sh
pageon replay ep.json --trace trace.zip
npx playwright show-trace trace.zip      # or drop it on trace.playwright.dev
```

The trace shows each step as an action, the page after it (DOM snapshot and screenshot), its
console messages and page errors, and every recorded request with its body. Times are the
page's virtual time. It is written from the replay, which the recording makes identical to the
original run, so a diverging replay ends with the divergence as the last action's error.
Secrets stay redacted, as in the recording.

`pageon fetch URL --replay ep.json` serves a recording to any command, and `pageon cdp
--replay` serves it to Puppeteer and Playwright: deterministic test fixtures from real sites.

## Reset to a step

`browser_reset` (MCP) and `tab.reset()` (Python) take an agent back to any step:

- the page is rebuilt from the session's own recording, so **nothing is sent again** (an order
  placed after that step is not placed twice);
- the refs are the ones the agent had at that step;
- it takes about 11 ms to step 3 in the sandbox.

Use it for undo, for backtracking search, or to retry a step with another action.
