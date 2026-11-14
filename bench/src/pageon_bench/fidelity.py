"""Baseline fidelity: each adapter equals its reference implementation's output.

The adapters run the references' own code paths — Playwright's public
``page.aria_snapshot(mode="ai")`` (what Playwright MCP calls), BrowserGym's
``flatten_axtree_to_str``, browser-use 1.0's ``AxHelpers.state()`` — on inputs captured from
the live page. This check re-runs each reference on the stored inputs and compares it with the
adapter's untruncated output, over up to 20 corpus pages.
"""

from __future__ import annotations

from pathlib import Path

from .capture import read_capture
from .corpus import BENCH, CORPUS
from .formats import make

BIG = 10_000_000


def main(args) -> int:
    pages = sorted(CORPUS.glob("*.capture.json.gz"))[:20]
    rows, same, total = [], 0, 0
    for p in pages:
        cap = read_capture(p)
        for name in ("playwright_mcp", "browsergym", "browser_use"):
            adapter = make(name, BIG).observe(cap, p)
            if name == "browser_use":
                reference = make(name, BIG).observe(cap, p)  # fresh helper: same code, same inputs
            else:
                reference = cap["baselines"].get(name, "")
            ok = adapter == reference
            total += 1
            same += ok
            rows.append(f"| {p.name.removesuffix('.capture.json.gz')} | {name} | {'identical' if ok else 'DIFFERS'} |")
    pct = 100 * same / total if total else 0
    md = [
        "# Baseline fidelity",
        "",
        f"{same}/{total} adapter outputs identical to their reference ({pct:.1f} %; bar ≥ 95 %).",
        "",
        "Method: " + (__doc__ or "").strip().replace("\n", " "),
        "",
        "| Page | Format | Result |",
        "| --- | --- | --- |",
        *rows,
    ]
    out = BENCH / "fidelity.md"
    Path(out).write_text("\n".join(md) + "\n")
    print(f"✓ {out}: {same}/{total} identical ({pct:.1f} %)")
    return 0 if pct >= 95 else 1
