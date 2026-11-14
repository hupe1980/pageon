"""Statistics for the report and the gate.

Success per task is the mean over its runs; intervals bootstrap over tasks (runs nested),
10,000 resamples with a fixed seed.
"""

from __future__ import annotations

import math

import numpy as np

RESAMPLES = 10_000
SEED = 1
PASS_MARGIN = -0.02  # lower CI bound above -2 points counts as "not significantly lower"
TOKEN_RATIO = 0.60


def per_task(episodes: list[dict]) -> dict[str, float]:
    """Mean success per task over its runs."""
    by: dict[str, list[float]] = {}
    for e in episodes:
        by.setdefault(e["task"], []).append(1.0 if e["success"] else 0.0)
    return {t: float(np.mean(v)) for t, v in by.items()}


def ci(values: list[float], seed: int = SEED) -> tuple[float, float, float]:
    """Mean and 95 % bootstrap interval over tasks."""
    if not values:
        return (0.0, 0.0, 0.0)
    a = np.asarray(values, dtype=float)
    rng = np.random.default_rng(seed)
    means = a[rng.integers(0, len(a), size=(RESAMPLES, len(a)))].mean(axis=1)
    lo, hi = np.percentile(means, [2.5, 97.5])
    return (float(a.mean()), float(lo), float(hi))


def paired_diff(a: dict[str, float], b: dict[str, float], seed: int = SEED) -> tuple[float, float, float]:
    """Mean of per-task (a - b) over shared tasks, with its 95 % bootstrap interval."""
    tasks = sorted(set(a) & set(b))
    return ci([a[t] - b[t] for t in tasks], seed)


def extra_runs_needed(d: float, lo: float, hi: float, runs: int) -> int | None:
    """Runs at which the CI half-width would fall below |d| (normal approximation)."""
    half = (hi - lo) / 2
    if d == 0 or half <= abs(d):
        return None
    factor = (half / abs(d)) ** 2
    return max(1, math.ceil(runs * factor) - runs)


def verdict(lo: float, hi: float, r: float | None, complete: bool = True) -> str:
    """The gate rule. An incomplete run is always INCONCLUSIVE; otherwise the
    first matching row wins: PASS, INCONCLUSIVE, FAIL."""
    if not complete:
        return "INCONCLUSIVE"
    token_ok = r is not None and r <= TOKEN_RATIO
    if lo > 0 or (lo > PASS_MARGIN and token_ok):
        return "PASS"
    if lo <= PASS_MARGIN and hi > 0:
        return "INCONCLUSIVE"
    if hi < 0 or (not token_ok and PASS_MARGIN < lo <= 0):
        return "FAIL"
    return "INCONCLUSIVE"
