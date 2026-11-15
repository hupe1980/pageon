"""Observation formats compared by the benchmark.

Every format reads the same page state at the same step. Baselines that need the live page
are computed at the capture instant (``live_baselines``) and stored in the capture, so every
format can be recomputed offline. Every format's element ids map to BrowserGym's ``bid`` so
actions execute identically.
"""

from __future__ import annotations

import json
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pageon

FORMATS = ["pageon_full", "pageon_diff", "playwright_mcp", "browsergym", "browser_use"]
# Not in the default comparison: run it with --formats to measure the compact view's success.
EXTRA_FORMATS = ["pageon_compact"]
CHARS_PER_TOKEN = 4  # browser-use's native budget is in characters


class ActionError(Exception):
    """An element id that cannot be acted on; the message is shown to the agent."""


# ---------------------------------------------------------------- live baselines


def live_baselines(page, axtree_object: dict | None = None) -> tuple[dict, dict]:
    """Baseline texts and extras for the page as it is now: (baselines, extras)."""
    from browsergym.utils.obs import flatten_axtree_to_str

    baselines: dict[str, str] = {}
    extras: dict[str, str] = {}
    try:
        baselines["playwright_mcp"] = page.aria_snapshot(mode="ai")
    except Exception as e:
        baselines["playwright_mcp"] = f"(snapshot failed: {e})"
    if axtree_object is None:
        from browsergym.core.observation import _post_extract, _pre_extract, extract_merged_axtree

        _pre_extract(page, lenient=True)
        try:
            axtree_object = extract_merged_axtree(page)
        finally:
            _post_extract(page)
    baselines["browsergym"] = flatten_axtree_to_str(axtree_object)
    try:
        from browser_use.ax import VISIBLE_JS

        extras["browser_use_visible"] = page.evaluate(VISIBLE_JS)
    except Exception:
        pass
    return baselines, extras


# ---------------------------------------------------------------- helpers


def bid_for_backend(capture: dict, backend_id: int) -> str | None:
    """BrowserGym's bid attribute of the DOM node with ``backend_id`` (any frame)."""
    for snap in capture["snapshots"]:
        strings = snap["strings"]
        for doc in snap["documents"]:
            ids = doc["nodes"]["backendNodeId"]
            if backend_id in ids:
                i = ids.index(backend_id)
                attrs = doc["nodes"]["attributes"][i]
                for k in range(0, len(attrs) - 1, 2):
                    if strings[attrs[k]] in ("bid", "browsergym_id"):
                        return strings[attrs[k + 1]]
    return None


# ---------------------------------------------------------------- formats


class Format:
    """One arm of the benchmark: renders observations and maps ids to bids."""

    name = ""
    example_id = ""  # shown to the model in the prompt

    def __init__(self, budget: int) -> None:
        self.budget = budget

    def observe(self, capture: dict, path: Path) -> str:
        raise NotImplementedError

    def to_bid(self, ident: str, capture: dict, path: Path, page) -> str | tuple[float, float]:
        """BrowserGym bid of the element, or its viewport centre when it has no bid."""
        raise NotImplementedError

    def note_action(self, verb: str, ident: str, error: str | None) -> str:
        """Outcome line for the next prompt (baselines: a plain line; pageon: built in)."""
        return f"{'ok' if error is None else 'err'} {verb} {ident}" + (f": {error}" if error else "")


class Pageon(Format):
    example_id = "e12"

    def __init__(self, budget: int, mode: str, detail: str = "full") -> None:
        super().__init__(budget)
        self.name = f"pageon_{mode}" if detail == "full" else f"pageon_{detail}"
        self.episode = pageon.Episode(budget=budget, tokenizer="o200k", mode=mode, detail=detail)
        self._cap = None

    def observe(self, capture: dict, path: Path) -> str:
        self._cap = pageon.Capture.load(str(path))
        return self.episode.observe(self._cap)

    def to_bid(self, ident: str, capture: dict, path: Path, page) -> str:
        try:
            t = self.episode.resolve(ident, self._cap or pageon.Capture.load(str(path)))
        except pageon.RefError as e:
            raise ActionError(str(e)) from None
        if t.bid:
            return t.bid
        center = (self._cap or pageon.Capture.load(str(path))).center(t.backend_node_id, t.frame_id)
        if center is None:
            raise ActionError(f"{ident} is outside the viewport; scroll first")
        return center

    def note_action(self, verb: str, ident: str, error: str | None) -> str:
        self.episode.note_action(verb, ident, error)
        return ""  # pageon prefixes the outcome to its next observation


class BrowserGym(Format):
    name = "browsergym"
    example_id = "12"

    def observe(self, capture: dict, path: Path) -> str:
        return pageon.cut_head_tail(capture["baselines"]["browsergym"], self.budget)

    def to_bid(self, ident: str, capture: dict, path: Path, page) -> str:
        return ident.strip("[]'\" ")


class PlaywrightMcp(Format):
    name = "playwright_mcp"
    example_id = "e12"

    def observe(self, capture: dict, path: Path) -> str:
        return pageon.cut_head_tail(capture["baselines"]["playwright_mcp"], self.budget)

    def to_bid(self, ident: str, capture: dict, path: Path, page) -> str:
        ref = ident.replace("[ref=", "").strip("[]@ ")
        loc = page.locator(f"aria-ref={ref}")
        try:
            bid = loc.get_attribute("bid", timeout=2000)
        except Exception:
            raise ActionError(f"ref {ref} not found; use a ref from the current snapshot") from None
        if bid:
            return bid
        box = loc.bounding_box(timeout=2000)
        if not box:
            raise ActionError(f"ref {ref} has no box; scroll first")
        return (box["x"] + box["width"] / 2, box["y"] + box["height"] / 2)


class BrowserUse(Format):
    """browser-use 1.0's own ``state()`` code, fed from the capture."""

    name = "browser_use"
    example_id = "1234"

    def __init__(self, budget: int) -> None:
        super().__init__(budget)
        self.helper = _offline_browser_use()

    def observe(self, capture: dict, path: Path) -> str:
        h = self.helper
        top = capture["frames"][0]
        h._frames = [{"url": "", "nodes": top["ax_nodes"]}] + [
            {"url": f.get("url", ""), "nodes": f["ax_nodes"]} for f in capture["frames"][1:]
        ]
        h._info_v = {"url": capture["url"], "title": capture["title"]}
        h._visible_v = capture.get("extras", {}).get("browser_use_visible")
        text = str(h.state(max=self.budget * CHARS_PER_TOKEN))
        return pageon.cut_head_tail(text, self.budget)

    def to_bid(self, ident: str, capture: dict, path: Path, page) -> str:
        try:
            backend = int(ident.strip("[]* "))
        except ValueError:
            raise ActionError(f"{ident} is not an id; use a number from the state") from None
        bid = bid_for_backend(capture, backend)
        if bid:
            return bid
        center = pageon.Capture.load(str(path)).center(backend)
        if center is None:
            raise ActionError(f"id {backend} not found; use an id from the current state")
        return center


def _offline_browser_use():
    from browser_use.ax import AxHelpers

    class Offline(AxHelpers):
        def __init__(self) -> None:  # bypass the live CDP daemon; only state() is used
            self.last = {}
            self.acted = False
            self.acted_at = 0.0
            self.dialogs = []
            self.nav_errors = {}
            self._pool = ThreadPoolExecutor(4)
            self.log = lambda _s: None
            self._frames, self._info_v, self._visible_v = [], {}, None

        def settle(self, cap_ms: float = 800, quiet_ms: float = 80, since: float = 0) -> dict:
            return {"probe": {**self._info_v, "files": False}, "pending": 0}

        def _capture(self) -> list[dict]:
            return self._frames

        def _info(self) -> dict:
            return self._info_v

        def _visible_text(self):
            return self._visible_v

        def _file_inputs(self) -> list[dict]:
            return []

        def _nav_error(self, url):
            return None

        def _flush_dialogs(self) -> str:
            return ""

    return Offline()


def make(name: str, budget: int) -> Format:
    match name:
        case "pageon_full":
            return Pageon(budget, "full")
        case "pageon_diff":
            return Pageon(budget, "diff")
        case "pageon_compact":
            return Pageon(budget, "diff", "compact")
        case "browsergym":
            return BrowserGym(budget)
        case "playwright_mcp":
            return PlaywrightMcp(budget)
        case "browser_use":
            return BrowserUse(budget)
    raise ValueError(f"unknown format {name!r}; choose from {', '.join(FORMATS + EXTRA_FORMATS)}")


def observe_all(capture: dict, path: Path, budget: int) -> dict[str, str]:
    """Every format (pageon once) for one stored capture: for `observe --all` and token sizes."""
    out = {"pageon": pageon.ax(pageon.Capture.load(str(path)), budget=budget)}
    for name in ("playwright_mcp", "browsergym", "browser_use"):
        try:
            out[name] = make(name, budget).observe(capture, path)
        except Exception as e:
            out[name] = f"(unavailable: {e})"
    return out


def full_sizes(capture: dict, path: Path) -> dict[str, int]:
    """Untruncated o200k sizes of every format for one page."""
    big = 10_000_000
    sizes = {"pageon": pageon.count_tokens(pageon.ax(pageon.Capture.load(str(path)), budget=big))}
    for name in ("playwright_mcp", "browsergym"):
        sizes[name] = pageon.count_tokens(capture["baselines"].get(name, ""))
    try:
        sizes["browser_use"] = pageon.count_tokens(make("browser_use", big).observe(capture, path))
    except Exception:
        sizes["browser_use"] = -1
    return sizes


def dumps(obj) -> str:
    return json.dumps(obj, ensure_ascii=False)
