# pageon for Python

```sh
brew install hupe1980/tap/pageon   # the browser (or cargo install pageon; or set PAGEON_BIN)
pip install pageon
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
values from `PAGEON_SECRET_<NAME>`), `files` (staged uploads), `state`, `save_state`, `replay`,
`record`, `budget`, `guard`, `binary`, and the identity and policy options `profile`,
`platform`, `identity`, `signature_tag`, `passkeys`, `cross_site_writes` and `robots`. An invalid option raises `PageonError` with pageon's message.

| `Tab` | |
| --- | --- |
| `observe(detail=…)`, `view(kind)`, `find(query)` | The agent view (full, compact or what changed); `markdown`, `text`, `errors`, `requests`; a search of the whole page |
| `click`, `type`, `upload`, `press`, `select`, `paste`, `hover`, `drag`, `scroll`, `wait`, `back`, `forward` | Actions; each returns an `Outcome`, or raises `ActionError` with `kind` and `guidance` |
| `screenshot(marks=…)`, `crop(ref)` | Pixels, with set-of-marks or provenance |
| `reset(step)` | Go back to a step exactly, sending nothing again |
| `lookahead(actions)` | Try actions on a fork; the page and the network are untouched |
| `call_tool(name, **args)` | Call a tool the page registered (WebMCP) |
| `workflow_save()`, `workflow_run(wf, **params)` | Compile the session into a workflow; rerun one |
| `takeover(reason)` | A live view for a person (CAPTCHAs, second factors) |
| `close()` | Close the tab (`browser.new_tab(url)` opens another) |

BrowserGym and AgentLab run on pageon unchanged (`pip install "pageon[browsergym]"`):

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
