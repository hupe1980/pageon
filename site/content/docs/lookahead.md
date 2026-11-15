+++
title = "Lookahead"
description = "Try actions on a fork of the live page before committing: writes are held, nothing is recorded, and the agent sees what would happen."
weight = 41
[extra]
seo_title = "Lookahead: try browser actions before committing"
group = "Reproducible sessions"
+++

An agent that is unsure what a button does can look before it leaps.

```json
{"name": "browser_lookahead", "arguments": {"actions": ["select e7 Express", "click e12"]}}
```

pageon forks the sandboxed page process, runs the actions on the copy and returns the
observation the agent would get. Then the copy is gone:

- the real page, its storage, its refs and the recording are untouched;
- reads are sent, so the fork sees real content, and the summary counts them
  (`lookahead (the page is unchanged; writes were held, 2 read(s) sent)`);
- **any request that could change something** (anything but `GET`, `HEAD` and `OPTIONS`) is
  held and reported as pending, never sent.

```python
look = tab.lookahead(["click e12"])
print(look.observation)   # what would happen
print(look.pending)       # the request it stopped at, never sent
```

## Why fork instead of act and undo

Act-then-reset costs a real request and a rebuild, and cannot take back what a write did. A fork
copies the live process in milliseconds: on MiniWoB++ a lookahead takes 5.2 ms on Linux against
14.4 ms for act-then-reset. It is the building block for agents that search over actions.

Lookahead needs the sandbox (the default); with `--no-sandbox` it fails with guidance.
