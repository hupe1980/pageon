+++
title = "Python"
description = "Drive pageon from Python: agent views with refs, typed actions and outcomes, tabs, screenshots, reset, lookahead and workflows."
weight = 12
[extra]
seo_title = "Python browser automation for AI agents"
group = "Interfaces"
+++

The Python package runs `pageon mcp` as a child process and gives it a small, typed API. Pages
run in the sandbox.

```sh
# the browser (see the quick start), then the package; building it needs Rust
pip install "git+https://github.com/hupe1980/pageon#subdirectory=bindings/python"
```

Set `PAGEON_BIN` if the `pageon` binary is not on your `PATH`.

## A session

```python
from pageon import Browser, ActionError

with Browser(origins=["https://shop.example"], secrets=["shop@https://shop.example"]) as browser:
    tab = browser.open("https://shop.example/login")
    print(tab.observe())                       # the agent view, with refs
    tab.type("e3", "ada")
    tab.upload("e9", "cv")              # Browser(files={"cv": ("./cv.pdf", ["https://shop.example"])})
    out = tab.type("e4", "secret://shop", submit=True)
    print(out)                                 # ok type e4 … · navigated to …/account
    print(out.observation)                     # what changed
    try:
        tab.click("e99")
    except ActionError as e:
        print(e.kind, e.guidance)              # unknown_ref, use a ref from the latest observation
```

## Browser options

`Browser(...)` takes the CLI's options as keyword arguments:

| Argument | |
| --- | --- |
| `origins`, `read_origins` | Where the task may write and read |
| `secrets` | `["name@https://origin"]`; values from `PAGEON_SECRET_<NAME>` |
| `files` | `{"cv": ("./cv.pdf", ["https://jobs.example"])}`: files the agent may upload by name |
| `guard` | A [guard](@/docs/security.md#guard) command |
| `state`, `save_state` | Playwright `storageState` files |
| `record`, `replay` | Record the session, or serve one offline |
| `budget` | Token budget of the views |
| `sandbox`, `private_network`, `binary` | Process and network settings |
| `allow_files` | Let the agent open `file:` pages (off by default) |
| `allow_javascript_exec` | Let the vendor toolsets run `javascript_exec` (off by default; see [Claude & OpenAI tools](@/docs/toolsets.md)) |

An invalid argument (an origin without its scheme, say) raises `PageonError` with pageon's
message.

## Claude's and OpenAI's tools

`browser.toolset(name)` runs a vendor's browser tool on this browser: `definition` is the tool to
send, `run(response_content)` returns the results to send back. See
[Claude & OpenAI tools](@/docs/toolsets.md).

## Tab

| Method | Returns |
| --- | --- |
| `observe(full=False)` | The agent view (what changed since the last one, unless `full`) |
| `view(kind)` | `"markdown"`, `"text"`, `"requests"` or `"errors"` |
| `click`, `type`, `upload(ref, *names)`, `press`, `select`, `hover`, `drag(ref, to)`, `wait`, `back`, `forward` | An `Outcome`, or raises `ActionError` |
| `scroll(direction="down", pixels=None, ref=None)` | Scroll the page or the container `ref` (`scroll(None, ref="e12")` brings e12 into view) |
| `screenshot(marks, full_page, format, mask)`, `crop(ref)` | A `Screenshot` (`.save(path)`, `.legend`) |
| `reset(step=None)` | Back to a step exactly, sending nothing again |
| `lookahead(actions)` | What the agent would see after the actions, on a fork |
| `call_tool(name, **arguments)` | The result of a page's [WebMCP](@/docs/webmcp.md) tool |
| `workflow_save()`, `workflow_run(workflow, **params)` | [Workflows](@/docs/workflows.md) |
| `takeover(reason)` | Waits while a person uses a live view |
| `close()` | Closes the tab |

## Several tabs

`browser.open(url)` loads into the current tab; `browser.new_tab(url)` opens another (at most
8) and returns it. Each `Tab` acts on its own page whichever was used last; `tab.id` is its
handle (`t1`, `t2`, …) and `browser.tabs` lists the open ones.

```python
results = browser.open("https://shop.example/search?q=lamp")
item = browser.new_tab("https://shop.example/item/42")
print(item.observe())
results.click("e7")                  # back on the results page, its refs intact
item.close()
```

`reset` and `workflow_save` rebuild one tab, so they refuse once a session has had several tabs
open.

## Without a browser

The package also serializes pages captured over CDP, for research on observation formats:

```python
import pageon
capture = pageon.Capture.load("page.capture.json.gz")
print(pageon.ax(capture, budget=4000))
```
