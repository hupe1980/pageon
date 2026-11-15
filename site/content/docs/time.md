+++
title = "Time and determinism"
description = "Why pageon pages run on a virtual clock, how actions settle, when real time passes, and how one seam makes every session replayable."
weight = 24
[extra]
group = "Concepts"
seo_title = "Virtual time and deterministic browser sessions"
+++

A browser for agents should answer the same way twice. pageon gets there with two rules: pages
run on a virtual clock, and everything nondeterministic comes through one seam.

## The virtual clock

`Date`, `performance.now()`, timers and animation frames read the page's own clock. When nothing
is due now, the clock jumps to the next timer instead of waiting, so a 5-second spinner or a
debounced search settles in milliseconds of wall time, and always in the same order.

After every action pageon runs the page until it is idle (timers, requests and microtasks done)
or a time window passes, then reports how it settled (`settled idle after 120 ms`). `wait 500`
lets 500 ms of page time pass.

## One seam for the outside world

Network responses, the clock's starting point and random numbers come only through one
interface, `Env`, and a lint enforces it in the engine. Live, it talks to the world; recording,
it keeps everything it served; replaying, it serves only the recording. That is what makes
[replay, reset](@/docs/replay.md) and [lookahead](@/docs/lookahead.md) exact, and tests offline.

## When real time passes

Two things depend on the world rather than the page:

- **Realtime connections.** With a `WebSocket` or `EventSource` open, an action that sent a frame
  waits up to one real second for the first reply, and `wait` lets real time pass until a message
  arrives. Messages are delivered at the virtual time they arrived, so a replay delivers them at
  the same points.
- **Protocol clients that poll.** A Puppeteer or Playwright client that polls sees page time pass
  as the wall clock does; one that awaits a promise lets the page's time run ahead.

A person in a [takeover](@/docs/mcp.md#handing-over-to-a-person) works in real time too; their
input is theirs and is not replayed.
