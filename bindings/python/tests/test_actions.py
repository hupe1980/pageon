"""BrowserGym action strings are parsed, never executed: a hijacked model cannot run Python."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("browsergym.core")

from pageon.browsergym import ActionRefused, run_action


def namespace(log: list[tuple[str, tuple, dict]]) -> dict[str, Any]:
    def action(name: str) -> Any:
        return lambda *a, **k: log.append((name, a, k))

    return {n: action(n) for n in ("click", "fill", "select_option", "scroll", "noop", "send_msg_to_user")}


@pytest.mark.parametrize(
    ("action", "words"),
    [
        ("click('e5')\n__import__('os').system('touch {marker}')", "only action calls run"),
        ("import os\nos.system('touch {marker}')", "only action calls run"),
        ("click('e5'); open('{marker}', 'w')", "'open' is not an action"),
        ("click.__globals__['__builtins__']", "only action calls run"),
        ("click('e5').__class__", "only action calls run"),
        ("click(open('{marker}', 'w'))", "arguments must be literals"),
        ("fill('e3', (lambda: open('{marker}', 'w'))())", "arguments must be literals"),
        ("click(**{{'bid': 'e5'}})", "arguments must be literals"),
        ("x = click('e5')", "only action calls run"),
        ("for _ in range(9): click('e5')", "only action calls run"),
        ("click('e5'", "not an action"),
    ],
)
def test_anything_but_action_calls_runs_nothing(tmp_path: Path, action: str, words: str) -> None:
    marker = tmp_path / "pwned"
    log: list = []
    with pytest.raises(ActionRefused, match=words):
        run_action(action.format(marker=marker), namespace(log))
    assert log == [], "part of a refused action ran"
    assert not marker.exists()


def test_browsergym_action_strings_run_in_order() -> None:
    log: list = []
    run_action(
        'click("e5")  # the button\n'
        "fill('e3', \"it's\")\n\n"
        "select_option('e7', ['a', 'b'])\n"
        "scroll(0, -200.5)\n"
        "noop(wait_ms=500)\n"
        "send_msg_to_user(text='done')",
        namespace(log),
    )
    assert log == [
        ("click", ("e5",), {}),
        ("fill", ("e3", "it's"), {}),
        ("select_option", ("e7", ["a", "b"]), {}),
        ("scroll", (0, -200.5), {}),
        ("noop", (), {"wait_ms": 500}),
        ("send_msg_to_user", (), {"text": "done"}),
    ]


def test_an_empty_action_does_nothing() -> None:
    log: list = []
    run_action("  # thinking\n", namespace(log))
    assert log == []
