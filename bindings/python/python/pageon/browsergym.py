"""BrowserGym on pageon: BrowserGym's tasks and agents, unchanged, on pageon's engine.

::

    import gymnasium as gym
    import pageon.browsergym

    pageon.browsergym.register(replace=True)    # gym.make("browsergym/…") now runs on pageon
    env = gym.make("browsergym/miniwob.click-button")
    obs, info = env.reset(seed=0)
    obs, reward, terminated, truncated, info = env.step("click('e5')")

BrowserGym's task classes set up and score themselves through Playwright, which connects to
``pageon cdp`` instead of launching Chromium. Observations come from pageon: the AX tree and the
DOM snapshot in BrowserGym's formats, with pageon's refs as ``bid``s (held in pageon, never
written into the page), plus ``pageon_ax``, pageon's own view. Actions run through pageon's
verified actions; what pageon cannot do faithfully fails with what to do instead.
"""

from __future__ import annotations

import ast
import base64
import copy
import io
import logging
import os
import subprocess
import threading
import time
from collections.abc import Mapping, Iterable
from pathlib import Path
from typing import Any

try:
    import gymnasium as gym
    import numpy as np
    from browsergym.core import _get_global_playwright
    from browsergym.core.env import BrowserEnv
    from browsergym.core.task import AbstractBrowserTask
    from PIL import Image
except ImportError as e:  # pragma: no cover - depends on the environment
    raise ImportError(
        f"pageon.browsergym needs BrowserGym ({e.name} is missing): pip install 'pageon[browsergym]'"
    ) from e

from .browser import PageonError, _binary

logger = logging.getLogger(__name__)

__all__ = ["PageonEnv", "register"]

# BrowserGym/AgentLab arguments that have no meaning without Chromium.
_IGNORED = (
    "tags_to_mark",
    "resizeable_window",
    "record_video_dir",
    "pw_chromium_kwargs",
    "pw_context_kwargs",
    "use_raw_page_output",
    "pre_observation_delay",
    "slow_mo",
    "locale",
    "timezone_id",
    "headless",
)



def _file_flags(files: "Mapping[str, tuple[str | os.PathLike[str], Iterable[str]]]") -> list[str]:
    """``--file NAME@ORIGIN[,ORIGIN]=PATH`` for each staged file: the agent uploads by name."""
    out: list[str] = []
    for name, (path, origins) in files.items():
        out += ["--file", f"{name}@{','.join(origins)}={os.fspath(path)}"]
    return out

class _Chat:
    """BrowserGym's chat, without its Chromium window: the messages, in order."""

    def __init__(self) -> None:
        self.messages: list[dict[str, Any]] = []

    def add_message(self, role: str, msg: str) -> None:
        self.messages.append({"role": role, "timestamp": time.time(), "message": msg})

    def wait_for_user_message(self) -> None:  # interactive mode is not supported
        raise NotImplementedError("wait_for_user_message is not supported on pageon")

    def close(self) -> None:
        pass


def _drain(stream: Any) -> None:
    for _ in stream:
        pass


class PageonEnv(BrowserEnv):
    """A BrowserGym environment on pageon: BrowserGym's spaces and ``reset``/``step``/``close``.

    Takes BrowserGym's arguments (``task_entrypoint``, ``task_kwargs``, ``timeout``,
    ``action_mapping``, …; Chromium-only ones are accepted and ignored) and pageon's session
    flags: ``sandbox``, ``private_network``, ``origins``, ``read_origins``, ``secrets``,
    ``record``, ``profile``, ``identity``, ``robots``, ``binary``. ``allow_files`` (on by default,
    since MiniWoB's tasks are local files) lets pages be opened from ``file:`` URLs; turn it off
    for tasks on the web.

    The agent's ``action_mapping`` (Python driving Playwright by ``bid`` attributes) cannot work
    without attributes in the page: the action string itself is run with pageon's functions.
    """

    def __init__(
        self,
        task_entrypoint: type[AbstractBrowserTask],
        task_kwargs: dict | None = None,
        viewport: dict | None = None,
        timeout: int | None = None,
        wait_for_user_message: bool = False,
        terminate_on_infeasible: bool = True,
        action_mapping: Any = None,
        *,
        sandbox: bool = True,
        private_network: bool = False,
        allow_files: bool = True,
        origins: Iterable[str] = (),
        read_origins: Iterable[str] = (),
        secrets: Iterable[str] = (),
        files: "Mapping[str, tuple[str | os.PathLike[str], Iterable[str]]]" = {},
        record: str | os.PathLike[str] | None = None,
        profile: str = "agent",
        identity: str | os.PathLike[str] | None = None,
        robots: str = "respect",
        binary: str | os.PathLike[str] | None = None,
        **ignored: Any,
    ) -> None:
        super().__init__(
            task_entrypoint=task_entrypoint,
            task_kwargs=task_kwargs or {},
            viewport=viewport,
            timeout=timeout,
            wait_for_user_message=wait_for_user_message,
            terminate_on_infeasible=terminate_on_infeasible,
            action_mapping=action_mapping,
        )
        from browsergym.core.spaces import Unicode

        # pageon's own view, next to BrowserGym's keys.
        self.observation_space = gym.spaces.Dict({**self.observation_space.spaces, "pageon_ax": Unicode()})
        unknown = sorted(k for k in ignored if k not in _IGNORED)
        if unknown:
            raise TypeError(f"PageonEnv got unexpected arguments: {', '.join(unknown)}")
        if ignored:
            logger.debug("pageon ignores Chromium-only arguments: %s", ", ".join(sorted(ignored)))
        self._args = [_binary(binary), "cdp", "--port", "0", "--profile", profile, "--robots", robots]
        if not sandbox:
            self._args.append("--no-sandbox")
        if private_network:
            self._args += ["--private-network", "allow"]
        if allow_files:
            self._args.append("--allow-files")
        self._args += _file_flags(files)
        for flag, values in (("--origin", origins), ("--read-origin", read_origins), ("--secret", secrets)):
            for v in values:
                self._args += [flag, v]
        for flag, path in (("--record", record), ("--identity", identity)):
            if path is not None:
                self._args += [flag, os.fspath(Path(path))]
        self._proc: subprocess.Popen[str] | None = None
        self._cdp: Any = None
        self._last_obs_ax: str = ""

    # ----- lifecycle -----

    def _start(self) -> str:
        self._proc = subprocess.Popen(
            self._args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
            text=True, encoding="utf-8",
        )
        lines: list[str] = []
        assert self._proc.stderr is not None
        for line in self._proc.stderr:
            lines.append(line.rstrip())
            if line.startswith("DevTools listening on "):
                url = line.split("DevTools listening on ", 1)[1].strip()
                # Keep draining stderr so pageon never blocks on a full pipe.
                threading.Thread(target=_drain, args=(self._proc.stderr,), daemon=True).start()
                return url
        raise PageonError("pageon cdp did not start:\n" + "\n".join(lines[-10:]))

    def _stop(self) -> None:
        if self.browser is not None:
            try:
                self.browser.close()
            except Exception:
                pass
        self.browser = self.context = self.page = self._cdp = None
        if self._proc is not None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            self._proc = None

    def close(self) -> None:
        if self.task:
            self.task.teardown()
            self.task = None
        self._stop()

    def reset(self, seed: int | None = None, *args: Any, **kwargs: Any) -> tuple[dict, dict]:
        gym.Env.reset(self, *args, seed=seed, **kwargs)
        self.np_random = None  # every source of randomness is the task's
        if self.task:
            self.task.teardown()
        self._stop()
        self.task = self.task_entrypoint(seed=seed, **self.task_kwargs)
        ws = self._start()
        pw = _get_global_playwright()
        self.browser = pw.chromium.connect_over_cdp(ws)
        self.context = self.browser.contexts[0] if self.browser.contexts else self.browser.new_context()
        timeout = self.timeout if self.timeout is not None else self.task.timeout
        self.context.set_default_timeout(timeout)
        self.page = self.context.pages[0] if self.context.pages else self.context.new_page()
        self._cdp = self.context.new_cdp_session(self.page)
        self.chat = _Chat()
        start = time.time()
        goal, task_info = self.task.setup(page=self.page)
        if goal is None:
            self.goal_object = []
        elif isinstance(goal, str):
            self.goal_object = [{"type": "text", "text": goal}]
        else:
            self.goal_object = list(goal)
        self.chat.add_message(
            role="assistant",
            msg="Hi! I am your UI assistant, I can perform web tasks for you. What can I help you with?",
        )
        for message in self.goal_object:
            if message["type"] == "text":
                self.chat.add_message(role="user", msg=message["text"])
            else:
                self.chat.add_message(role="user_image", msg=str(message.get("image_url")))
        self.start_time = start
        self.last_action = ""
        self.last_action_error = ""
        self.infeasible_message_received = False
        return self._get_obs(), {"task_info": task_info}

    def step(self, action: str) -> tuple[dict, float, bool, bool, dict]:
        self.last_action = action
        info: dict[str, Any] = {"action_exec_start": time.time(), "action_exec_timeout": 0}
        try:
            run_action(action, _namespace(self))
            self.last_action_error = ""
        except Exception as e:
            self.last_action_error = f"{type(e).__name__}: {e}"
        info["action_exec_stop"] = time.time()
        reward, done, user_message, task_info = self.task.validate(self.page, self.chat.messages)
        info["task_info"] = task_info
        if user_message:
            self.chat.add_message(role="user", msg=user_message)
        obs = self._get_obs()
        terminated = bool(done) or (self.terminate_on_infeasible and self.infeasible_message_received)
        return obs, float(reward), terminated, False, info

    # ----- observations -----

    def _get_obs(self) -> dict:
        bg = self._cdp.send("Pageon.browsergym")
        shot = self._cdp.send("Pageon.screenshot", {})
        image = Image.open(io.BytesIO(base64.b64decode(shot["data"]))).convert("RGB")
        return {
            "chat_messages": tuple(dict(m) for m in self.chat.messages),
            "goal": "\n".join(m["text"] for m in self.goal_object if m["type"] == "text"),
            "goal_object": tuple(copy.deepcopy(self.goal_object)),
            "open_pages_urls": (self.page.url,),
            "open_pages_titles": (self.page.title(),),
            "active_page_index": np.asarray([0]),
            "url": self.page.url,
            "screenshot": np.asarray(image, dtype=np.uint8),
            "dom_object": bg["dom_object"],
            "axtree_object": bg["axtree_object"],
            "extra_element_properties": bg["extra_element_properties"],
            "focused_element_bid": bg["focused_element_bid"],
            "last_action": self.last_action,
            "last_action_error": self.last_action_error,
            "elapsed_time": np.asarray([time.time() - self.start_time]),
            "pageon_ax": bg["pageon_ax"],
        }


class _Unsupported(NotImplementedError):
    pass


class ActionRefused(ValueError):
    """An action string that is not a sequence of action calls with literal arguments."""


def run_action(action: str, namespace: dict[str, Any]) -> None:
    """Runs a BrowserGym action string without executing Python.

    Every statement must be a call to a function of ``namespace``, by name, with literal
    arguments (``click('e5')``, ``fill('e3', "it's")``, one call per line). The whole string is
    checked before anything runs: a string with anything else runs nothing and raises
    :class:`ActionRefused` naming the construct, because a page can make a model write code.
    """
    try:
        tree = ast.parse(action, mode="exec")
    except SyntaxError as e:
        raise ActionRefused(f"not an action: {e.msg} (line {e.lineno})") from None
    calls: list[tuple[Any, list[Any], dict[str, Any]]] = []
    for stmt in tree.body:
        call = stmt.value if isinstance(stmt, ast.Expr) else None
        if not isinstance(call, ast.Call) or not isinstance(call.func, ast.Name):
            got = _shown(action, stmt)
            raise ActionRefused(f"line {stmt.lineno}: only action calls run, like click('e5'); got {got}")
        fn = namespace.get(call.func.id)
        if fn is None:
            raise ActionRefused(f"line {stmt.lineno}: {call.func.id!r} is not an action")
        try:
            args = [ast.literal_eval(a) for a in call.args]
            kwargs = {k.arg: ast.literal_eval(k.value) for k in call.keywords if k.arg is not None}
        except ValueError:
            raise ActionRefused(f"line {stmt.lineno}: arguments must be literals; got {_shown(action, stmt)}") from None
        if len(kwargs) != len(call.keywords):
            raise ActionRefused(f"line {stmt.lineno}: arguments must be literals; got {_shown(action, stmt)}")
        calls.append((fn, args, kwargs))
    for fn, args, kwargs in calls:
        fn(*args, **kwargs)


def _shown(source: str, node: ast.AST) -> str:
    text = ast.get_source_segment(source, node) or type(node).__name__
    return repr(text if len(text) <= 80 else text[:79] + "…")


def _namespace(env: PageonEnv) -> dict[str, Any]:
    """BrowserGym's action functions, run by pageon."""

    def act(action: Any) -> None:
        try:
            env._cdp.send("Pageon.act", {"action": action})
        except Exception as e:  # playwright.Error: "Protocol error (Pageon.act): …"
            msg = str(e).split("): ", 1)[-1].strip()
            raise ValueError(msg) from None

    def focused() -> str:
        return env._cdp.send("Pageon.browsergym")["focused_element_bid"]

    def unsupported(name: str, instead: str) -> Any:
        def f(*_args: Any, **_kwargs: Any) -> None:
            raise _Unsupported(f"{name} is not supported by pageon yet; {instead}")

        return f

    def click(bid: str, button: str = "left", modifiers: list | tuple = ()) -> None:
        if button != "left" or modifiers:
            raise _Unsupported("only a plain left click is supported by pageon; use click(bid)")
        act({"Click": bid})

    def fill(bid: str, value: str) -> None:
        act({"Type": [bid, value]})

    def clear(bid: str) -> None:
        act({"Type": [bid, ""]})

    def select_option(bid: str, options: str | list[str]) -> None:
        act({"Select": [bid, [options] if isinstance(options, str) else list(options)]})

    def hover(bid: str) -> None:
        act({"Hover": bid})

    def drag_and_drop(from_bid: str, to_bid: str) -> None:
        act({"Drag": [from_bid, to_bid]})

    def upload_file(bid: str, file: str | list[str]) -> None:
        # Staged file names (``PageonEnv(files=...)``), never host paths.
        act({"Upload": [bid, [file] if isinstance(file, str) else list(file)]})

    def scroll(delta_x: float, delta_y: float) -> None:
        # The wheel, where the pointer is (as Playwright's mouse.wheel).
        act({"Scroll": {"by": {"pixels": [float(delta_x), float(delta_y)]}}})

    def press(bid: str, key_comb: str) -> None:
        if focused() != bid:
            act({"Click": bid})
        act({"Press": key_comb})

    def keyboard_press(key: str) -> None:
        act({"Press": key})

    def keyboard_type(text: str) -> None:
        bid = focused()
        if not bid:
            raise ValueError("no element has focus; use fill(bid, text)")
        act({"Type": [bid, text]})

    def goto(url: str) -> None:
        act({"Goto": url})

    def go_back() -> None:
        act("Back")

    def go_forward() -> None:
        act("Forward")

    def noop(wait_ms: float = 1000) -> None:
        act({"Wait": int(wait_ms)})

    def send_msg_to_user(text: str) -> None:
        if not isinstance(text, str):
            raise ValueError(f"Forbidden value: {text} is not a string")
        env.chat.add_message(role="assistant", msg=text)

    def report_infeasible(reason: str) -> None:
        if not isinstance(reason, str):
            raise ValueError(f"Forbidden value: {reason} is not a string")
        env.chat.add_message(role="infeasible", msg=reason)
        env.infeasible_message_received = True

    by_bid = "every element is reachable by its bid"
    ns: dict[str, Any] = {
        "click": click,
        "fill": fill,
        "clear": clear,
        "select_option": select_option,
        "hover": hover,
        "press": press,
        "keyboard_press": keyboard_press,
        "keyboard_type": keyboard_type,
        "goto": goto,
        "go_back": go_back,
        "go_forward": go_forward,
        "noop": noop,
        "send_msg_to_user": send_msg_to_user,
        "report_infeasible": report_infeasible,
        "scroll": scroll,
        "dblclick": unsupported("dblclick", "use click(bid)"),
        "focus": unsupported("focus", "use click(bid), or press(bid, key) which focuses first"),
        "drag_and_drop": drag_and_drop,
        "upload_file": upload_file,
        "check": unsupported("check", "use click(bid) on the checkbox"),
        "uncheck": unsupported("uncheck", "use click(bid) on the checkbox"),
        "new_tab": unsupported("new_tab", "pageon environments have one tab; use goto(url)"),
        "tab_focus": unsupported("tab_focus", "pageon environments have one tab"),
        "tab_close": unsupported("tab_close", "pageon environments have one tab; use go_back()"),
    }
    for name in ("mouse_move", "mouse_up", "mouse_down", "mouse_click", "mouse_dblclick",
                 "mouse_drag_and_drop", "mouse_upload_file", "keyboard_down", "keyboard_up",
                 "keyboard_insert_text"):
        ns[name] = unsupported(name, f"coordinates are not supported; {by_bid}: use click(bid)")
    return ns


_BENCHMARKS = (
    "browsergym.miniwob",
    "browsergym.webarena",
    "browsergym.visualwebarena",
    "browsergym.workarena",
    "browsergym.assistantbench",
    "browsergym.weblinx",
)


def register(replace: bool = False) -> list[str]:
    """Registers a pageon environment for every installed BrowserGym task.

    ``pageon/<task>`` is added for each ``browsergym/<task>``. With ``replace=True``,
    ``browsergym/<task>`` itself builds a :class:`PageonEnv`, including tasks registered later
    (benchmark packages AgentLab imports lazily): the one line AgentLab needs. Returns the task
    names registered under ``pageon/``.
    """
    import importlib
    import inspect

    from browsergym.core import registration

    for name in _BENCHMARKS:
        try:
            importlib.import_module(name)
        except Exception:
            continue
    if replace:
        # BrowserGym's entry points call `BrowserEnv(task_entrypoint, …)` by this name.
        registration.BrowserEnv = PageonEnv
    tasks = []
    for env_id, spec in list(gym.envs.registry.items()):
        if not env_id.startswith("browsergym/") or not callable(spec.entry_point):
            continue
        entry = inspect.getclosurevars(spec.entry_point).nonlocals.get("task_entrypoint")
        if entry is None:
            continue
        task = env_id.split("/", 1)[1]
        target = f"pageon/{task}"
        if target in gym.envs.registry:
            del gym.envs.registry[target]
        gym.register(
            id=target,
            entry_point=lambda *a, _entry=entry, **kw: PageonEnv(_entry, *a, **kw),
            nondeterministic=spec.nondeterministic,
            kwargs=dict(spec.kwargs or {}),
        )
        tasks.append(task)
    return tasks
