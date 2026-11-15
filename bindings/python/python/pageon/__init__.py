"""pageon: compact, quoted, budgeted observations of web pages for agents.

    import pageon
    cap = pageon.Capture.load("page.capture.json.gz")
    print(pageon.ax(cap, budget=4000))

    ep = pageon.Episode(budget=4000, tokenizer="o200k", mode="diff")
    text = ep.observe(cap)
    target = ep.resolve("e3", cap)       # raises pageon.RefError if it cannot

Drive the browser itself (the ``pageon`` binary, sandboxed):

    with pageon.Browser() as browser:
        tab = browser.open("https://example.com")
        print(tab.click("e1"))
"""

from __future__ import annotations

from dataclasses import dataclass

from ._native import Capture, PageonError, __version__, _Episode, ax, count_tokens, cut_head_tail
from .browser import ActionError, Browser, Lookahead, Outcome, Screenshot, Tab

__all__ = [
    "ActionError",
    "Browser",
    "Capture",
    "Episode",
    "Lookahead",
    "Outcome",
    "PageonError",
    "RefError",
    "Relocation",
    "Screenshot",
    "Tab",
    "Target",
    "__version__",
    "ax",
    "count_tokens",
    "cut_head_tail",
]


@dataclass(frozen=True)
class Relocation:
    """A ref moved to a replacement element, with a confidence in [0, 1]."""

    ref: str
    confidence: float


@dataclass(frozen=True)
class Target:
    """The element an action targets."""

    ref: str
    backend_node_id: int
    frame_id: str
    bid: str | None
    relocated: Relocation | None


class RefError(PageonError):
    """A ref that cannot be acted on. ``str(err)`` is the text to show the agent."""

    def __init__(self, kind: str, message: str, since_step: int | None = None,
                 candidates: list[str] | None = None, covered_by: str | None = None) -> None:
        super().__init__(message)
        self.kind = kind
        self.since_step = since_step
        self.candidates = candidates or []
        self.covered_by = covered_by


class Episode:
    """One task played once: owns the refs and the last observation the agent saw."""

    def __init__(
        self, budget: int = 4000, tokenizer: str = "o200k", mode: str = "full", detail: str = "full"
    ) -> None:
        """``detail="compact"`` leaves text runs out of the view (elements, headings, alerts stay)."""
        self._native = _Episode(budget, tokenizer, mode, detail)

    @property
    def version(self) -> int:
        return self._native.version

    def observe(self, capture: Capture) -> str:
        """Full observation on the first step (or after navigation / in full mode), else a diff."""
        return self._native.observe(capture)

    def note_action(self, verb: str, ref: str, error: str | None = None) -> None:
        """Record the action just taken; its outcome line prefixes the next observation."""
        self._native.note_action(verb, ref, error)

    def resolve(self, ref: str, capture: Capture) -> Target:
        """The element ``ref`` names in ``capture``; raises :class:`RefError` if it cannot."""
        r = self._native.resolve_raw(ref, capture)
        if not r["ok"]:
            raise RefError(r["kind"], r["message"], r.get("since_step"), r.get("candidates"), r.get("covered_by"))
        reloc = Relocation(ref, r["relocated"]) if r["relocated"] is not None else None
        return Target(ref, r["backend_node_id"], r["frame_id"], r["bid"], reloc)
