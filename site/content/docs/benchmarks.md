+++
title = "Benchmarks"
description = "pageon's agent view against Playwright MCP, BrowserGym and browser-use on MiniWoB++: task success, tokens and confidence intervals."
weight = 50
[extra]
seo_title = "Benchmarks: pageon vs Playwright MCP, BrowserGym, browser-use"
group = "Reference"
+++

pageon's claims are measured with an in-repo benchmark that anyone can rerun locally at no
cost. Every cell is published, including the ones pageon loses.

## Observation formats on MiniWoB++

The question: given the same page, model, prompt, budget and actions, which observation format
lets an agent succeed more often, with fewer tokens?

| Setting | |
| --- | --- |
| Tasks | MiniWoB++ through BrowserGym, 125 tasks × 3 runs = 1,875 episodes per format |
| Model | Qwen3.5-9B (4-bit), temperature 0, fixed seed, local |
| Budget | 4,000 tokens (o200k) for every format |
| Actions | BrowserGym's action set for every format, so execution is identical |
| Page state | All formats computed from the same page at every step |

| Format | Success | 95 % CI | Tokens/step | Invalid actions |
| --- | --- | --- | --- | --- |
| **pageon (full views)** | **0.427** | [0.355, 0.501] | 155.6 | 0.198 |
| pageon (diffs) | 0.408 | [0.336, 0.483] | **106.9** | 0.173 |
| Playwright MCP snapshot | 0.395 | [0.320, 0.472] | 157.9 | 0.111 |
| BrowserGym AXTree | 0.344 | [0.272, 0.419] | 347.3 | **0.042** |
| browser-use 1.0 | 0.240 | [0.173, 0.309] | 93.5 | 0.296 |

Paired differences (pageon full views minus the baseline), with tokens measured on real pages:

| Baseline | Difference | 95 % CI | Tokens on real pages |
| --- | --- | --- | --- |
| Playwright MCP | +3.2 points | [−3.2, +9.9] | 0.27× |
| BrowserGym | +8.3 points | [+1.9, +14.9] | 0.16× |
| browser-use 1.0 | +18.7 points | [+12.8, +24.8] | 1.02× |

**What this shows.** pageon is significantly better than BrowserGym's AXTree and browser-use
1.0, and at least on par with Playwright MCP, the strongest baseline, with about a quarter of its
tokens on real pages. A significant margin over Playwright MCP is not established. Diffs cut
tokens per step by 31 % and raise prompt-cache hits, within noise of full views' success.

**Caveats.** One small local model; MiniWoB++ pages are small, so token savings show on real
pages rather than in the per-step figures; pageon's invalid-action rate is higher than
BrowserGym's.

### Reproduce

```sh
cd bench
uv run pageon-bench setup            # once: Chromium and MiniWoB++, pinned
mrun serve --model qwen3.5           # or any OpenAI-compatible local server
uv run pageon-bench run --runs 3     # every task; resumes if interrupted
```

The report gives success with bootstrap intervals, tokens, invalid actions, cache hits and the
verdict for every format.

## Engine conformance

| Suite | Result |
| --- | --- |
| html5lib tree construction | 1,787/1,792, the same as html5ever's reference |
| Web Platform Tests: roles · names | 323/323 · 539/572 |
| WPT cookies (`http-state`) | 161/161 |
| Native `ax` view vs the Chromium-based view | 204/204 lines on the corpus pages |
| Layout vs Chromium (19 pages) | 96.9 % of boxes within 2 px, 98.2 % within 8 px |
| Screenshots vs Chromium | SSIM 0.980 |
| MiniWoB++ task pages | 124/130 run their scripts without an error |
| Puppeteer · Playwright · BiDi suites | pass in the sandbox and in-process |

## Loading pages

Eight public pages (the [audit](@/docs/audit.md)'s), recorded once and replayed offline, so
every engine gets the same bytes and the network is not measured. Apple M4 Pro, release build,
median of 5 runs (`pageon bench`) or 3 (`pageon-bench engines`).

| Page | First view | Warm | CPU | Memory | Layouts |
| --- | --- | --- | --- | --- | --- |
| github.com | 1.83 s | 1.58 s | 1.82 s | 282 MB | 80 |
| bbc.com/news | 866 ms | 780 ms | 857 ms | 231 MB | 29 |
| crates.io search | 135 ms | 33 ms | 131 ms | 115 MB | 2 |
| todomvc.com | 130 ms | 38 ms | 125 ms | 110 MB | 1 |
| gov.uk | 124 ms | 59 ms | 119 ms | 105 MB | 0 |
| news.ycombinator.com | 113 ms | 22 ms | 108 ms | 96 MB | 0 |
| docs.python.org | 99 ms | 12 ms | 96 ms | 99 MB | 2 |
| rust-lang.org | 93 ms | 6 ms | 88 ms | 95 MB | 0 |

First view is a `pageon fetch --dump ax` process, caged, start to exit; warm opens the page in
a started process; memory is the largest process; layouts are those the page's scripts forced.

Beside Chromium's headless shell (Playwright 1.65, the same recordings served as a HAR),
from launch to Playwright's AI snapshot, CPU and memory summed over each engine's processes:

| Page | pageon first view · CPU · memory | Chromium first view · CPU · memory |
| --- | --- | --- |
| github.com | 3.30 s · 2.19 s · 346 MB | **1.09 s · 1.03 s** · 501 MB |
| bbc.com/news | 2.22 s · 1.18 s · **262 MB** | **808 ms · 0.82 s** · 423 MB |
| crates.io search | **228 ms · 0.15 s · 134 MB** | 323 ms · 0.28 s · 279 MB |
| todomvc.com | **297 ms · 0.14 s · 128 MB** | 418 ms · 0.27 s · 291 MB |
| gov.uk | **205 ms · 0.14 s · 125 MB** | 459 ms · 0.31 s · 325 MB |
| news.ycombinator.com | **259 ms · 0.12 s · 112 MB** | 487 ms · 0.30 s · 301 MB |
| docs.python.org | **174 ms · 0.12 s · 115 MB** | 308 ms · 0.27 s · 321 MB |
| rust-lang.org | **151 ms · 0.09 s · 112 MB** | 341 ms · 0.27 s · 305 MB |

**What this shows.** On documents and light apps pageon reaches its view in about half
Chromium's time, CPU and memory. On heavy script apps Chromium is 2–3× faster: its JIT tiers and
threads pay off, and GitHub forces 80 layouts while loading. Memory stays lower on every page.
These runs time a sampled process tree, so times are higher than `pageon bench`'s.

```sh
pageon bench https://github.com https://news.ycombinator.com      # recorded once, then offline
cd bench && uv run pageon-bench engines ~/.cache/pageon/bench/*.rec.json --pageon "$(which pageon)"
```

## Speed

Release build on an Apple M4 Pro (lookahead on Linux), in the sandbox unless noted:

| Operation | Time |
| --- | --- |
| Click (action, settle, outcome) | 2.2 ms |
| Viewport screenshot (PNG, in-process) | 1.7 ms |
| Lookahead on a fork (Linux) | 5.2 ms |
| Reset to step 3 | 11 ms |
| Workflow step (in-process) | 1.0 ms |
| WebMCP tool call (in-process) | 0.07 ms |
| MiniWoB++ step through BrowserGym | 9 ms |
