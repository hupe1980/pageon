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
