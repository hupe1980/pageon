# pageon for Python

```sh
git clone https://github.com/hupe1980/procage
git clone https://github.com/hupe1980/pageon
cargo install --path pageon/crates/pageon      # the browser (or set PAGEON_BIN)
pip install ./pageon/bindings/python          # needs Rust
```

Drive the browser. Each `Browser` runs `pageon mcp` as a child process, and the page's code runs
in its sandbox:

```python
from pageon import Browser, ActionError

with Browser() as browser:
    tab = browser.open("https://example.com")
    print(tab.observe())                 # the ax view, with refs
    try:
        out = tab.click("e1")
        print(out)                       # ok click e1 · navigated to …
        print(out.observation)           # what changed
    except ActionError as e:
        print(e.kind, e.guidance)        # e.g. not_actionable: wait until it is enabled
```

`Browser` takes the CLI's options as arguments: `sandbox`, `private_network`, `allow_files`
(off: agents cannot open `file:` pages), `origins`, `read_origins`, `secrets` (`"name@origin"`;
values from `PAGEON_SECRET_<NAME>`), `state`, `save_state`, `replay`, `record`, `budget`, `guard`
and `binary`. An invalid option raises `PageonError` with pageon's message.

| `Tab` | |
| --- | --- |
| `observe()`, `view(kind)` | The agent view (full or what changed); `markdown`, `text`, `errors`, `requests` |
| `click`, `type`, `press`, `select`, `hover`, `wait`, `back`, `forward` | Actions; each returns an `Outcome`, or raises `ActionError` with `kind` and `guidance` |
| `screenshot(marks=…)`, `crop(ref)` | Pixels, with set-of-marks or provenance |
| `reset(step)` | Go back to a step exactly, sending nothing again |
| `lookahead(actions)` | Try actions on a fork; the page and the network are untouched |
| `call_tool(name, **args)` | Call a tool the page registered (WebMCP) |
| `workflow_save()`, `workflow_run(wf, **params)` | Compile the session into a workflow; rerun one |
| `takeover(reason)` | A live view for a person (CAPTCHAs, second factors) |

BrowserGym and AgentLab run on pageon unchanged (`pip install "…[browsergym]"`):

```python
import gymnasium as gym, pageon.browsergym
pageon.browsergym.register(replace=True)
env = gym.make("browsergym/miniwob.click-button")
```

Serialize captured pages (no browser needed):

```python
import pageon
cap = pageon.Capture.load("page.capture.json.gz")
print(pageon.ax(cap, budget=4000))
```
