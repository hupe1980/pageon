"""BrowserGym on pageon: unchanged BrowserGym tasks and observation code, run through
``gym.make("browsergym/…")`` with pageon as the browser."""

from __future__ import annotations

import os
import re
import statistics
import time
from pathlib import Path

import pytest

gym = pytest.importorskip("gymnasium")
pytest.importorskip("browsergym.miniwob")

from browsergym.utils.obs import (  # noqa: E402
    flatten_axtree_to_str,
    flatten_dom_to_str,
    overlay_som,
    prune_html,
)

ROOT = Path(__file__).resolve().parents[3]
MINIWOB = ROOT / "bench" / ".cache" / "miniwob" / "miniwob" / "html" / "miniwob"
BIN = os.environ.get("PAGEON_BIN") or str(ROOT / "target" / "debug" / "pageon")

pytestmark = pytest.mark.skipif(
    not MINIWOB.is_dir() or not os.path.isfile(BIN),
    reason="needs MiniWoB++ (uv run pageon-bench setup) and the binary (cargo build -p pageon)",
)

TASKS = ["click-button", "enter-text", "login-user", "choose-list", "click-checkboxes", "click-tab", "scroll-text-2"]
STEPS: list[float] = []


@pytest.fixture(autouse=True)
def _env(monkeypatch: pytest.MonkeyPatch) -> None:
    import pageon.browsergym
    import playwright.sync_api

    monkeypatch.setenv("MINIWOB_URL", MINIWOB.as_uri() + "/")
    monkeypatch.setenv("PAGEON_BIN", BIN)

    def no_chromium(*_a: object, **_k: object) -> None:
        raise AssertionError("Chromium was launched")

    monkeypatch.setattr(playwright.sync_api.BrowserType, "launch", no_chromium)
    pageon.browsergym.register(replace=True)


def bid(ax: str, role: str, name: str) -> str:
    m = re.search(rf"\[(e\d+)\] {role} '{re.escape(name)}'", ax)
    assert m, f"no {role} {name!r} in\n{ax}"
    return m.group(1)


def oracle(task: str, goal: str, ax: str) -> list[str]:
    """The actions that solve `task`, read from the goal and BrowserGym's AX text only."""
    if task == "click-button":
        label = re.search(r'"(.+)" button', goal).group(1)
        return [f"click('{bid(ax, 'button', label)}')"]
    if task == "click-tab":
        tab = re.search(r"Click on (Tab #\d+)", goal).group(1)
        return [f"click('{bid(ax, 'tab', tab)}')"]
    boxes = re.findall(r"\[(e\d+)\] textbox", ax)
    if task == "scroll-text-2":
        # The wheel scrolls what the pointer is over: point at the textarea first.
        down = "bottom" in re.search(r"to the (top|bottom)", goal).group(1)
        submit = bid(ax, "button", "Submit")
        return [f"hover('{boxes[0]}')", f"scroll(0, {10000 if down else -10000})", f"click('{submit}')"]
    if task == "enter-text":
        text = re.search(r'Enter "(.*)" into', goal).group(1)
        return [f"fill('{boxes[0]}', {text!r})", f"click('{bid(ax, 'button', 'Submit')}')"]
    if task == "login-user":
        user, pw = re.search(r'username "(.*)" and the password "(.*)" into', goal).groups()
        login = bid(ax, "button", "Login")
        return [f"fill('{boxes[0]}', {user!r})", f"fill('{boxes[1]}', {pw!r})", f"click('{login}')"]
    if task == "choose-list":
        option = re.search(r"Select (.*) from the list", goal).group(1)
        combo = re.search(r"\[(e\d+)\] combobox", ax).group(1)
        return [f"select_option('{combo}', {option!r})", f"click('{bid(ax, 'button', 'Submit')}')"]
    if task == "click-checkboxes":
        names = re.search(r"Select (.*) and click Submit", goal).group(1)
        wanted = [] if names == "nothing" else [n.strip() for n in re.split(r",| and ", names) if n.strip()]
        return [f"click('{bid(ax, 'checkbox', n)}')" for n in wanted] + [f"click('{bid(ax, 'button', 'Submit')}')"]
    raise AssertionError(task)


def check_observation(env: object, obs: dict) -> None:
    """AgentLab's preprocessing runs, and bids are refs that never touch the page."""
    ep = obs["extra_element_properties"]
    kw = dict(extra_properties=ep, with_visible=True, with_clickable=True)
    ax = flatten_axtree_to_str(obs["axtree_object"], **kw)
    html = prune_html(flatten_dom_to_str(obs["dom_object"], **kw))
    assert overlay_som(obs["screenshot"], extra_properties=ep).shape == obs["screenshot"].shape
    for text in (ax, html, obs["pageon_ax"]):
        for b in re.findall(r"\b(e\d+)\b", re.sub(r"'[^']*'|\"[^\"]*\"", "", text)):
            assert b in ep, f"{b} is not an element with properties"
    page = env.unwrapped.page
    marks = page.evaluate(
        "document.querySelectorAll('[bid],[browsergym_set_of_marks],[browsergym_visibility_ratio]').length"
    )
    assert marks == 0, "BrowserGym attributes were written into the page"


def play(task: str, seed: int, **kwargs: object) -> float:
    env = gym.make(f"browsergym/miniwob.{task}", **kwargs)
    try:
        obs, _ = env.reset(seed=seed)
        check_observation(env, obs)
        reward = 0.0
        for action in oracle(task, obs["goal"], flatten_axtree_to_str(obs["axtree_object"])):
            start = time.perf_counter()
            obs, reward, terminated, _, _ = env.step(action)
            STEPS.append(time.perf_counter() - start)
            assert obs["last_action_error"] == "", obs["last_action_error"]
            check_observation(env, obs)
            if terminated:
                break
        return reward
    finally:
        env.close()


@pytest.mark.parametrize("task", TASKS)
def test_unchanged_browsergym_tasks_run_on_pageon(task: str) -> None:
    rewards = [play(task, seed) for seed in (0, 1, 2)]
    assert rewards == [1.0, 1.0, 1.0]


def test_caged_and_in_process_agree() -> None:
    assert play("login-user", 3, sandbox=False) == play("login-user", 3) == 1.0


def test_bids_stay_and_every_bid_acts() -> None:
    env = gym.make("browsergym/miniwob.click-checkboxes")
    try:
        obs, _ = env.reset(seed=0)
        ax = flatten_axtree_to_str(obs["axtree_object"])
        boxes = re.findall(r"\[(e\d+)\] checkbox '([^']*)'", ax)
        obs, *_ = env.step(f"click('{boxes[0][0]}')")
        after = flatten_axtree_to_str(obs["axtree_object"])
        assert re.findall(r"\[(e\d+)\] checkbox '([^']*)'", after) == boxes, "bids changed"
        html = prune_html(flatten_dom_to_str(obs["dom_object"]))
        for b in sorted(set(re.findall(r'bid="(e\d+)"', html))):
            obs, *_ = env.step(f"hover('{b}')")
            assert "unknown ref" not in obs["last_action_error"], (b, obs["last_action_error"])
    finally:
        env.close()


def test_limits_are_honest() -> None:
    env = gym.make("browsergym/miniwob.click-button")
    try:
        env.reset(seed=0)
        for action, words in [
            ("dblclick('e1')", "use click(bid)"),
            ("new_tab()", "one tab"),
            ("mouse_click(10, 10)", "coordinates"),
            ("click('e999')", "unknown ref e999"),
        ]:
            obs, *_ = env.step(action)
            assert words in obs["last_action_error"], (action, obs["last_action_error"])
        obs, *_ = env.step("scroll(0, 200)")  # supported now (not an error)
        assert obs["last_action_error"] == ""
        obs, *_ = env.step("send_msg_to_user('done')")
        assert obs["last_action_error"] == ""
        assert obs["chat_messages"][-1]["message"] == "done"
    finally:
        env.close()


def test_steps_are_fast() -> None:
    if len(STEPS) < 10:
        for task in ("click-button", "enter-text"):
            play(task, 4)
    median = statistics.median(STEPS) * 1000
    print(f"\nBrowserGym step on pageon: median {median:.0f} ms over {len(STEPS)} steps")
    if os.environ.get("PAGEON_SPEED"):
        assert median <= 100, f"{median:.0f} ms"
