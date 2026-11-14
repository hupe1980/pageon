+++
title = "Record, replay and reset"
description = "pageon records sessions step by step and replays them byte for byte on a virtual clock: offline debugging, regression checks against live sites, and exact reset to any step."
weight = 40
[extra]
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

A recording is readable JSON: every response, the clock and random draws, and the agent's steps
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

`pageon fetch URL --replay ep.json` serves a recording to any command, and `pageon cdp
--replay` serves it to Puppeteer and Playwright: deterministic test fixtures from real sites.

## Reset to a step

`browser_reset` (MCP) and `tab.reset()` (Python) take an agent back to any step:

- the page is rebuilt from the session's own recording, so **nothing is sent again** (an order
  placed after that step is not placed twice);
- the refs are the ones the agent had at that step;
- it takes about 11 ms to step 3 in the sandbox.

Use it for undo, for backtracking search, or to retry a step with another action.
