+++
title = "BrowserGym & AgentLab"
description = "Run BrowserGym's tasks and AgentLab's agents on pageon unchanged: one line switches the backend, bids are pageon's refs, and a MiniWoB++ step takes 9 ms."
weight = 16
[extra]
group = "Interfaces"
nav = "BrowserGym"
+++

`pageon.browsergym` runs BrowserGym's task classes on pageon's engine. Observations come in
BrowserGym's formats, and actions go through pageon.

```sh
pip install "…/bindings/python[browsergym]"
```

```python
import gymnasium as gym
import pageon.browsergym

pageon.browsergym.register(replace=True)        # browsergym/* now runs on pageon
env = gym.make("browsergym/miniwob.click-button")
obs, info = env.reset(seed=0)                   # axtree_object, dom_object, pageon_ax, …
obs, reward, done, *_ = env.step("click('e1')")
```

- **Bids are pageon's refs.** Nothing is written into the page; BrowserGym's `bid` attributes
  are never injected.
- **Unchanged agents.** AgentLab's preprocessing runs on every observation, and
  `register(replace=True)` is the one line an AgentLab experiment needs.
- **Fast.** A MiniWoB++ step takes about 9 ms, with no Chromium.

- **Actions are parsed, never executed.** BrowserGym runs a model's action string as Python;
  pageon parses it and runs only calls to action functions with literal arguments
  (`click('e5')`, `fill('e3', "it's")`, one per line). Anything else, such as an import, an
  attribute or a variable, runs nothing and comes back in `last_action_error`, so a model that a
  page has hijacked cannot run code on your machine.

`upload_file(bid, name)` uploads a file staged with `PageonEnv(files={"cv": (path, [origin])})` by
name, never a path. Actions pageon cannot perform yet (drag) raise an error that names the action, instead
of silently doing nothing. `scroll(dx, dy)` scrolls with the wheel where the pointer is, as in
Chromium.

MiniWoB's tasks are local files, so the environment lets pages open `file:` URLs by default;
pass `allow_files=False` (with `gym.make(..., allow_files=False)`) for tasks on the web.
