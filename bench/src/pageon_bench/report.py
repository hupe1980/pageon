"""report.json + report.md and the gate verdict."""

from __future__ import annotations

import json
import statistics
from pathlib import Path

from . import stats
from .corpus import CORPUS

BASELINES = ("playwright_mcp", "browsergym", "browser_use")
PAGEON = ("pageon_full", "pageon_diff")


def _episodes(run: Path) -> list[dict]:
    out = []
    for line in (run / "episodes.jsonl").read_text().splitlines() if (run / "episodes.jsonl").exists() else []:
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def token_sizes() -> list[dict]:
    """Untruncated o200k sizes of every format on the real corpus pages, offline."""
    from .capture import read_capture
    from .formats import full_sizes

    rows = []
    for p in sorted(CORPUS.glob("real-*.capture.json.gz")):
        rows.append({"page": p.name.removesuffix(".capture.json.gz"), **full_sizes(read_capture(p), p)})
    return rows


def _cell(fmt: str, eps: list[dict]) -> dict:
    mine = [e for e in eps if e["format"] == fmt]
    tasks = stats.per_task(mine)
    mean, lo, hi = stats.ci(list(tasks.values()))
    steps = [s for e in mine for s in e["steps"]]
    toks = [s.get("observation_tokens", 0) for s in steps]
    prompt = sum(s.get("prompt_tokens", 0) for s in steps)
    cached = sum(s.get("cached_tokens", 0) for s in steps)
    invalid = sum(1 for s in steps if s.get("outcome") in ("invalid_action", "unknown_ref"))
    return {
        "format": fmt,
        "episodes": len(mine),
        "success": round(mean, 4),
        "ci95": [round(lo, 4), round(hi, 4)],
        "tokens_per_step": {
            "mean": round(statistics.mean(toks), 1) if toks else 0,
            "median": statistics.median(toks) if toks else 0,
        },
        "steps_per_task": round(len(steps) / len(mine), 2) if mine else 0,
        "invalid_action_rate": round(invalid / len(steps), 4) if steps else 0,
        "cache_hit_rate": round(cached / prompt, 4) if prompt else 0,
        "infra_failures": sum(1 for e in mine if e.get("failure_cause")),
    }


def build(run: Path) -> dict:
    cfg = json.loads((run / "config.json").read_text())
    eps = _episodes(run)
    planned = len(cfg["tasks"]) * len(cfg["formats"]) * cfg["runs"]
    # A verdict needs every planned episode and at least 3 runs per task.
    complete = len(eps) >= planned and cfg["runs"] >= 3
    cells = [_cell(f, eps) for f in cfg["formats"]]
    by = {c["format"]: c for c in cells}
    sizes = token_sizes()
    pt = {f: stats.per_task([e for e in eps if e["format"] == f]) for f in cfg["formats"]}
    gate: dict = {"verdict": "INCONCLUSIVE", "rule": "gate-v1", "extra_runs_needed": None}
    sensitivity = []
    p_arms = [f for f in PAGEON if f in by]
    b_arms = [f for f in BASELINES if f in by]
    if p_arms and b_arms:

        def ratio(base: str) -> float | None:
            rs = [s["pageon"] / s[base] for s in sizes if s.get(base, 0) > 0]
            return round(statistics.median(rs), 3) if rs else None

        def size(base: str) -> float:
            return statistics.median([s[base] for s in sizes if s.get(base, 0) > 0] or [float("inf")])

        pmode = max(p_arms, key=lambda f: by[f]["success"])
        # Conservative: among equally successful baselines, compare against the most compact one.
        best = max(b_arms, key=lambda f: (by[f]["success"], -size(f)))

        for base in b_arms:
            d, lo, hi = stats.paired_diff(pt[pmode], pt[base])
            sensitivity.append(
                {
                    "baseline": base,
                    "paired_diff": round(d, 4),
                    "ci95": [round(lo, 4), round(hi, 4)],
                    "token_ratio_real_pages": ratio(base),
                    "verdict": stats.verdict(lo, hi, ratio(base), complete),
                }
            )
        d, lo, hi = stats.paired_diff(pt[pmode], pt[best])
        r = ratio(best)
        v = stats.verdict(lo, hi, r, complete)
        gate = {
            "verdict": v,
            "pageon_mode": pmode,
            "best_baseline": best,
            "paired_diff": round(d, 4),
            "ci95": [round(lo, 4), round(hi, 4)],
            "token_ratio_real_pages": r,
            "rule": "gate-v1",
            "complete": complete,
            "extra_runs_needed": stats.extra_runs_needed(d, lo, hi, cfg["runs"]) if v == "INCONCLUSIVE" else None,
        }
    import importlib.metadata as md

    def ver(p: str) -> str:
        try:
            return md.version(p)
        except md.PackageNotFoundError:
            return "absent"

    config = {k: cfg[k] for k in ("model", "model_hash", "runs", "seed", "max_steps", "retries", "suite")}
    config["budget"] = {"tokenizer": "o200k", "max_tokens": cfg["budget"]}
    config["versions"] = {
        p: ver(p)
        for p in ("pageon-bench", "pageon", "browsergym-core", "browsergym-miniwob", "playwright", "browser-use")
    }
    config["episodes"] = {"planned": planned, "done": len(eps)}
    return {
        "schema": 1,
        "config": config,
        "cells": cells,
        "gate": gate,
        "sensitivity": sensitivity,
        "token_sizes": sizes,
    }


def render_md(rep: dict) -> str:
    g = rep["gate"]
    out = ["# pageon serializer proof — report", ""]
    out.append(f"**Gate: {g['verdict']}**")
    if not g.get("complete", True):
        out.append("- not decidable: the run is incomplete or has fewer than 3 runs per task")
    if "pageon_mode" in g:
        out.append(f"- pageon arm: `{g['pageon_mode']}` vs best baseline `{g['best_baseline']}`")
        out.append(
            f"- paired success difference {g['paired_diff']:+.3f}, 95 % CI [{g['ci95'][0]:+.3f}, "
            f"{g['ci95'][1]:+.3f}]; real-page token ratio r = {g['token_ratio_real_pages']}"
        )
    if g.get("extra_runs_needed"):
        out.append(f"- about {g['extra_runs_needed']} more runs per task would decide it")
    c = rep["config"]
    out += [
        "",
        f"Model `{c['model']}` ({c['model_hash']}), budget {c['budget']['max_tokens']}/o200k, "
        f"{c['runs']} runs, seed {c['seed']}, max {c['max_steps']} steps; "
        f"{c['episodes']['done']}/{c['episodes']['planned']} episodes.",
        "",
        "| Format | Success | 95 % CI | Tokens/step (mean) | Steps/task "
        "| Invalid actions | Cache hits | Infra failures |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for x in rep["cells"]:
        out.append(
            f"| {x['format']} | {x['success']:.3f} | [{x['ci95'][0]:.3f}, {x['ci95'][1]:.3f}] | "
            f"{x['tokens_per_step']['mean']} | {x['steps_per_task']} | {x['invalid_action_rate']:.3f} | "
            f"{x['cache_hit_rate']:.3f} | {x['infra_failures']} |"
        )
    if rep["sensitivity"]:
        out += [
            "",
            "Sensitivity (pageon vs every baseline):",
            "",
            "| Baseline | Paired diff | 95 % CI | Token ratio (real pages) | Verdict |",
            "| --- | --- | --- | --- | --- |",
        ]
        for s in rep["sensitivity"]:
            out.append(
                f"| {s['baseline']} | {s['paired_diff']:+.3f} | [{s['ci95'][0]:+.3f}, {s['ci95'][1]:+.3f}] | "
                f"{s['token_ratio_real_pages']} | {s['verdict']} |"
            )
    out += [
        "",
        "Untruncated o200k tokens on the real corpus pages:",
        "",
        "| Page | pageon | Playwright MCP | BrowserGym | browser-use 1.0 |",
        "| --- | --- | --- | --- | --- |",
    ]
    for s in rep["token_sizes"]:
        out.append(f"| {s['page']} | {s['pageon']} | {s['playwright_mcp']} | {s['browsergym']} | {s['browser_use']} |")
    out += ["", "Versions: " + ", ".join(f"{k} {v}" for k, v in c["versions"].items())]
    return "\n".join(out) + "\n"


def write(run: Path) -> dict:
    rep = build(run)
    (run / "report.json").write_text(json.dumps(rep, indent=2, ensure_ascii=False))
    (run / "report.md").write_text(render_md(rep))
    return rep


def gate_verdict(run: Path) -> str:
    p = run / "report.json"
    rep = json.loads(p.read_text()) if p.exists() else write(run)
    return rep["gate"]["verdict"]
