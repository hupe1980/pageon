# pageon-bench

Compares observation formats for browser agents on MiniWoB++ (through BrowserGym), using a
local model, at no cost.

| Format | Source |
| --- | --- |
| `pageon_full`, `pageon_diff` | pageon's `ax` view (`crates/pageon-agent`) |
| `playwright_mcp` | Playwright's `page.aria_snapshot(mode="ai")`, the snapshot Playwright MCP sends |
| `browsergym` | BrowserGym's `flatten_axtree_to_str` |
| `browser_use` | browser-use 1.0's own `state()` code |

The comparison is controlled:
- **Same page state.** At every step, every format is computed from the same page.
- **Same model and prompt.** One model and one prompt serve every format.
- **Same budget.** Every format is cut to the same o200k budget.
- **Same actions.** Actions run through BrowserGym's own action set, so execution is identical.

## Run

```sh
uv run pageon-bench setup                       # once: Chromium + MiniWoB++ (pinned)
mrun serve --model qwen3.5                      # or any OpenAI-compatible local server
uv run pageon-bench smoke                       # 5 tasks × 5 formats × 1 run, ~2 minutes
uv run pageon-bench run --runs 3                # every task; resumes if interrupted
uv run pageon-bench gate runs/<id>              # exit 0 PASS · 3 FAIL · 4 INCONCLUSIVE
```

`runs/<id>/report.md` reports:
- success per format with 95 % bootstrap intervals;
- tokens per step;
- invalid actions;
- prompt-cache hits;
- token sizes on real pages;
- the verdict.

Every cell is reported, including the ones where pageon loses. A run with fewer than 3 runs per
task is never decisive.

## Other commands

| Command | Purpose |
| --- | --- |
| `observe <url or capture> --all` | Every format side by side, with token counts |
| `capture <url> -o file` | Capture a real page for the corpus (honours robots.txt) |
| `corpus` | Rebuild the constructed corpus from `fixtures/` |
| `fidelity` | Check the baseline reproductions against their reference implementations |

## Development

```sh
uv run pytest           # statistics, gate rule, action parsing; no browser, no model
uv run ruff check src tests
```
