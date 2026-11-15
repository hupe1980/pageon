"""Browser.toolset: the vendors' browser tools run by pageon, offline against a recording."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from pageon import Browser
from test_browser import BIN, _html

pytestmark = pytest.mark.skipif(BIN is None, reason="build the binary: cargo build -p pageon")


@pytest.fixture
def page(tmp_path: Path) -> Path:
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [
            _html(
                "https://shop.test/",
                "<h1>Shop</h1><label>Email <input name=email></label><a href=/help>Help center</a>",
            ),
            _html("https://shop.test/help", "<h1>Help</h1>"),
        ],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    p = tmp_path / "toolset.json"
    p.write_text(json.dumps(log))
    return p


class Block:
    """An SDK-like content block (``model_dump``)."""

    def __init__(self, **kw: object) -> None:
        self._kw = kw

    def model_dump(self, exclude_none: bool = False) -> dict:
        return dict(self._kw)


def _use(i: int, name: str, **input: object) -> Block:
    return Block(type="tool_use", id=f"toolu_{i}", name=name, input=input, toolset_name="browser")


@pytest.mark.parametrize("sandbox", [True, False])
def test_a_claude_turn(page: Path, sandbox: bool) -> None:
    with Browser(replay=page, sandbox=sandbox, binary=BIN) as browser:
        ts = browser.toolset("browser_toolset_20260801")
        assert ts.definition == {"type": "browser_toolset_20260801"}
        content = [
            Block(type="text", text="Let me look."),
            _use(0, "navigate", url="shop.test"),
            _use(1, "find", query="help"),
        ]
        results = ts.run(content)
        assert [r["tool_use_id"] for r in results] == ["toolu_0", "toolu_1"]
        assert results[0]["content"][0]["text"].startswith("Navigated to https://shop.test/.")
        link = results[1]["content"][0]["text"].splitlines()[1]
        assert "Help center" in link
        ref = link.split()[1]
        results = ts.run([_use(2, "left_click", target={"type": "ref", "ref": ref})])
        assert "help" in results[0]["content"][0]["text"]


def test_confirm_stops_the_turn(page: Path) -> None:
    with Browser(replay=page, binary=BIN) as browser:
        ts = browser.toolset("browser_toolset_20260801")
        asked: list[str] = []

        def no(member: str, input: dict) -> bool:
            asked.append(member)
            return False

        results = ts.run([_use(0, "navigate", url="https://shop.test/"), _use(1, "screenshot")], confirm=no)
        assert asked == ["navigate"]
        assert results[0]["is_error"] and "declined" in results[0]["content"][0]["text"]
        assert results[1]["content"][0]["text"] == "Not executed: an earlier action in this turn failed."


def test_new_tabs_are_the_browsers_tabs(page: Path) -> None:
    with Browser(replay=page, binary=BIN) as browser:
        ts = browser.toolset("browser_toolset_20260801")
        ts.run([_use(0, "navigate", url="https://shop.test/"), _use(1, "new_tab")])
        assert [t.id for t in browser.tabs] == ["t1", "t2"]
