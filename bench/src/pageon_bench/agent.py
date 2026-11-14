"""The agent loop's model side: one prompt for every format, action parsing, the model call."""

from __future__ import annotations

import re
import time
from dataclasses import dataclass, field

SYSTEM = """You control a web browser to complete a task. Each turn you see the page as text.
End your reply with one or more actions, one per line, executed in order:

  click {x}
  fill {x} "text"
  select {x} "option"
  press {x} Enter
  scroll down
  done

Element ids are exactly as shown in the page text (for example {x}). Say done when the task is complete."""

VERBS = ("click", "fill", "select", "press", "scroll", "done")
MAX_ACTIONS = 5


@dataclass
class Action:
    verb: str
    ident: str = ""
    arg: str = ""


@dataclass
class Usage:
    prompt_tokens: int = 0
    cached_tokens: int = 0
    completion_tokens: int = 0
    latency_ms: int = 0
    extra: dict = field(default_factory=dict)


class InvalidAction(Exception):
    pass


def _ident(tok: str) -> str:
    return tok.strip().strip("\"'`").replace("[ref=", "").strip("[]@*:,.")


def _quoted(rest: str) -> str | None:
    m = re.search(r'"((?:[^"\\]|\\.)*)"', rest)
    return m.group(1).replace('\\"', '"') if m else None


def _parse_line(line: str) -> Action | None:
    words = line.strip().strip("`").strip().split()
    if not words or words[0].lower().rstrip(":") not in VERBS:
        return None
    verb = words[0].lower().rstrip(":")
    rest = words[1:]
    if rest and rest[0].upper() == "ID":  # the model echoed the placeholder word
        rest = rest[1:]
    if verb == "done":
        return Action("done")
    if verb == "scroll":
        return Action("scroll", arg="up" if rest and rest[0].lower().startswith("up") else "down")
    if not rest:
        return None
    ident = _ident(rest[0])
    if not ident:
        return None
    tail = " ".join(rest[1:])
    if verb in ("fill", "select"):
        arg = _quoted(tail)
        if arg is None:
            return None
        return Action(verb, ident, arg)
    if verb == "press":
        return Action(verb, ident, rest[1].strip('"') if len(rest) > 1 else "Enter")
    return Action(verb, ident)


def parse_all(reply: str) -> list[Action]:
    """Every action line in the reply, in order (at most MAX_ACTIONS); raises if none."""
    actions = []
    for line in reply.splitlines():
        a = _parse_line(line)
        if a:
            actions.append(a)
            if a.verb == "done" or len(actions) >= MAX_ACTIONS:
                break
    if not actions:
        lines = [ln for ln in reply.strip().splitlines() if ln.strip()]
        raise InvalidAction(f"no valid action in reply: {lines[-1] if lines else '(empty)'}")
    return actions


def parse(reply: str) -> Action:
    """The first action of the reply."""
    return parse_all(reply)[0]


def _q(s: str | None) -> str:
    return '"' + (s or "").replace("\\", "\\\\").replace('"', '\\"') + '"'


def to_browsergym(action: Action, bid: str | tuple[float, float] | None) -> str:
    """BrowserGym high-level action string: bid actions, or coordinate actions when the
    target has no bid (``bid`` is then the element's viewport centre)."""
    q = _q
    if isinstance(bid, tuple):
        x, y = round(bid[0], 1), round(bid[1], 1)
        match action.verb:
            case "click":
                return f"mouse_click({x}, {y})"
            case "fill":
                return f"mouse_click({x}, {y})\nkeyboard_press('ControlOrMeta+a')\nkeyboard_type({q(action.arg)})"
            case "press":
                return f"mouse_click({x}, {y})\nkeyboard_press({q(action.arg)})"
        return "noop()"
    match action.verb:
        case "click":
            return f"click({q(bid)})"
        case "fill":
            return f"fill({q(bid)}, {q(action.arg)})"
        case "select":
            return f"select_option({q(bid)}, {q(action.arg)})"
        case "press":
            return f"press({q(bid)}, {q(action.arg)})"
        case "scroll":
            return f"scroll(0, {300 if action.arg == 'down' else -300})"
    return "noop()"


class Model:
    """An OpenAI-compatible chat endpoint (mrun by default)."""

    def __init__(self, endpoint: str, model: str, seed: int) -> None:
        from openai import OpenAI

        self.client = OpenAI(base_url=endpoint, api_key="local")
        self.model = model
        self.seed = seed

    def model_hash(self) -> str:
        try:
            m = self.client.models.retrieve(self.model)
            return str(getattr(m, "id", self.model)) + ":" + str(getattr(m, "created", ""))
        except Exception:
            return self.model

    def act(self, messages: list[dict]) -> tuple[str, Usage]:
        t0 = time.monotonic()
        r = self.client.chat.completions.create(
            model=self.model,
            messages=messages,
            temperature=0,
            seed=self.seed,
            max_tokens=400,
            extra_body={"chat_template_kwargs": {"enable_thinking": False}},
        )
        u = r.usage
        details = getattr(u, "prompt_tokens_details", None)
        cached = getattr(details, "cached_tokens", 0) if details else 0
        usage = Usage(
            prompt_tokens=getattr(u, "prompt_tokens", 0) or 0,
            cached_tokens=cached or 0,
            completion_tokens=getattr(u, "completion_tokens", 0) or 0,
            latency_ms=int((time.monotonic() - t0) * 1000),
        )
        return r.choices[0].message.content or "", usage
