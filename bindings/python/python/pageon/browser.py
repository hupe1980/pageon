"""Drive the pageon browser from Python, through ``pageon mcp``.

    from pageon import Browser

    with Browser() as browser:
        tab = browser.open("https://example.com")
        print(tab.observe())
        print(tab.click("e1"))          # the outcome line; .observation has the page after it

The browser is the ``pageon`` binary (``PAGEON_BIN``, else ``pageon`` on ``PATH``), run as a
child process: the page's code runs in its sandbox, exactly as it does for MCP hosts.
"""

from __future__ import annotations

import base64
import json
import os
import shutil
import subprocess
import sys
import threading
from collections import deque
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ._native import PageonError

__all__ = ["ActionError", "Browser", "Lookahead", "Outcome", "Screenshot", "Tab", "Toolset"]

_PROTOCOL = "2025-11-25"


def _file_flags(files: Mapping[str, tuple[str | os.PathLike[str], Iterable[str]]] | None) -> list[str]:
    """``--file NAME@ORIGIN[,ORIGIN]=PATH`` for each staged file: the agent uploads by name."""
    out: list[str] = []
    for name, (path, origins) in (files or {}).items():
        out += ["--file", f"{name}@{','.join(origins)}={os.fspath(path)}"]
    return out


class ActionError(PageonError):
    """An action the page refused. ``str(err)`` is the text to show the agent.

    ``kind`` is one of ``unknown_ref``, ``stale_ref``, ``not_actionable``, ``no_such_option``,
    ``navigation``, ``no_page``, ``unsupported``, ``secret`` or ``engine``; ``guidance`` says
    what to do next.
    """

    def __init__(self, kind: str, message: str, guidance: str = "") -> None:
        super().__init__(f"{message}\n{guidance}" if guidance else message)
        self.kind = kind
        self.message = message
        self.guidance = guidance


@dataclass(frozen=True)
class Outcome:
    """What an action did. ``str(outcome)`` is the outcome line; ``observation`` is the page after it."""

    line: str
    observation: str
    navigated: str | None = None
    data: dict[str, Any] = field(default_factory=dict, repr=False, compare=False)
    step: int | None = None
    """The step this action made (``tab.reset(step)`` goes back to it)."""
    refusal: dict[str, Any] | None = None
    """The site refused the agent on the page: ``{"kind": "blocked" | "challenge" |
    "rate_limited", "vendor", "status", "retry_after", "detail"}``. pageon never solves a
    challenge; hand the page to a person (``tab.takeover()``) or ask your user."""

    def __str__(self) -> str:
        return self.line


@dataclass(frozen=True)
class Lookahead:
    """What actions would do, seen on a fork of the page: the page is unchanged, writes were
    held, and reads were sent (``str(lookahead)`` counts them).

    ``observation`` is what you would see after them (a diff against your last observation);
    ``pending`` is the request a branch was stopped at, never sent (``{"method", "url"}``);
    ``error`` is the first action that failed. ``str(lookahead)`` is pageon's summary.
    """

    text: str
    outcomes: tuple[dict[str, Any], ...] = ()
    observation: str | None = None
    pending: dict[str, str] | None = None
    error: dict[str, Any] | None = None
    ms: int = 0

    def __str__(self) -> str:
        return self.text


@dataclass(frozen=True)
class Screenshot:
    """A painted image. ``bytes(shot)`` or ``shot.data`` are the encoded bytes.

    ``legend`` lists the marks (one ``ax`` line per labelled element) when taken with
    ``marks=True``; ``provenance`` says where a crop's pixels come from (``tier`` is
    ``first-party`` or ``third-party``); ``notes`` say what the image does not show.
    """

    data: bytes = field(repr=False)
    mime: str
    width: int
    height: int
    tokens: int
    notes: tuple[str, ...] = ()
    legend: str | None = None
    provenance: dict[str, Any] | None = None

    def __bytes__(self) -> bytes:
        return self.data

    def save(self, path: str | os.PathLike[str]) -> Path:
        """Writes the image to ``path`` and returns it."""
        p = Path(path)
        p.write_bytes(self.data)
        return p


def _binary(binary: str | os.PathLike[str] | None) -> str:
    found = binary or os.environ.get("PAGEON_BIN") or shutil.which("pageon")
    if found and not os.path.isfile(found):
        raise PageonError(f"the pageon binary was not found at {os.fspath(found)}")
    if not found:
        raise PageonError(
            "the pageon binary was not found\n"
            "build it (https://hupe1980.github.io/pageon/docs/quick-start/), "
            "or set PAGEON_BIN to its path"
        )
    return os.fspath(found)


class Browser:
    """One browser session: a ``pageon mcp`` child process. Use it as a context manager."""

    def __init__(
        self,
        *,
        sandbox: bool = True,
        private_network: bool = False,
        allow_files: bool = False,
        origins: Iterable[str] = (),
        read_origins: Iterable[str] = (),
        secrets: Iterable[str] = (),
        files: Mapping[str, tuple[str | os.PathLike[str], Iterable[str]]] | None = None,
        state: str | os.PathLike[str] | None = None,
        save_state: str | os.PathLike[str] | None = None,
        replay: str | os.PathLike[str] | None = None,
        record: str | os.PathLike[str] | None = None,
        budget: int = 4000,
        profile: str = "agent",
        platform: str | None = None,
        signature_tag: str | None = None,
        cross_site_writes: str = "allow",
        identity: str | os.PathLike[str] | None = None,
        passkeys: str | os.PathLike[str] | None = None,
        robots: str = "respect",
        guard: str | None = None,
        allow_javascript_exec: bool = False,
        binary: str | os.PathLike[str] | None = None,
    ) -> None:
        args = [_binary(binary), "mcp", "--budget", str(budget), "--profile", profile, "--robots", robots]
        if not sandbox:
            args.append("--no-sandbox")
        if allow_javascript_exec:
            args.append("--allow-javascript-exec")
        if guard is not None:
            args += ["--guard", guard]
        if platform is not None:
            args += ["--platform", platform]
        if signature_tag is not None:
            args += ["--signature-tag", signature_tag]
        if cross_site_writes != "allow":
            args += ["--cross-site-writes", cross_site_writes]
        if private_network:
            args += ["--private-network", "allow"]
        if allow_files:
            args.append("--allow-files")
        args += _file_flags(files)
        for flag, values in (("--origin", origins), ("--read-origin", read_origins), ("--secret", secrets)):
            for v in values:
                args += [flag, v]
        files = (
            ("--state", state),
            ("--save-state", save_state),
            ("--replay", replay),
            ("--record", record),
            ("--identity", identity),
            ("--passkeys", passkeys),
        )
        for flag, path in files:
            if path is not None:
                args += [flag, os.fspath(Path(path))]
        self._proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        # pageon's stderr still reaches yours; its last lines explain an exit.
        self._stderr: deque[str] = deque(maxlen=20)
        self._stderr_done = threading.Thread(target=self._drain_stderr, daemon=True)
        self._stderr_done.start()
        self._next = 0
        # The open tabs by handle (t1, t2, …) and the one pageon acts on.
        self._tabs: dict[str, Tab] = {}
        self._current: str | None = None
        self._request(
            "initialize",
            {
                "protocolVersion": _PROTOCOL,
                # The takeover's live-view URL comes as a URL-mode elicitation: for the
                # person (``on_url``), never in a tool result.
                "capabilities": {"elicitation": {"url": {}}},
                "clientInfo": {"name": "pageon-python", "version": _version()},
            },
        )
        self._notify("notifications/initialized")

    def open(self, url: str) -> Tab:
        """Load ``url`` in the current tab (the first one when none is open) and return it."""
        result = self._call("browser_open", {"url": url})
        tab = self._tabs[result["structuredContent"]["tab"]]
        tab.opened = _outcome(result)
        return tab

    def new_tab(self, url: str) -> Tab:
        """Open ``url`` in a new tab (at most 8) and return it; the other tabs keep their pages."""
        result = self._call("browser_tabs", {"action": "new", "url": url})
        tab = self._tabs[result["structuredContent"]["tab"]]
        tab.opened = _outcome(result)
        return tab

    @property
    def tabs(self) -> list[Tab]:
        """The open tabs, in opening order."""
        return list(self._tabs.values())

    def close(self) -> None:
        """End the session; ``save_state`` is written now."""
        if self._proc.poll() is None:
            assert self._proc.stdin is not None
            self._proc.stdin.close()
            try:
                self._proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait()

    def __enter__(self) -> Browser:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def __del__(self) -> None:
        proc = getattr(self, "_proc", None)
        if proc is not None and proc.poll() is None:
            proc.kill()

    # JSON-RPC over the child's stdio.

    def _drain_stderr(self) -> None:
        assert self._proc.stderr is not None
        for line in self._proc.stderr:
            self._stderr.append(line)
            sys.stderr.write(line)

    def _exited(self) -> PageonError:
        status = self._proc.wait()
        self._stderr_done.join(timeout=5)
        said = "".join(self._stderr).strip()
        return PageonError(f"pageon exited with status {status}" + (f": {said}" if said else ""))

    def _send(self, msg: dict[str, Any]) -> None:
        if self._proc.poll() is not None:
            raise self._exited()
        assert self._proc.stdin is not None
        try:
            self._proc.stdin.write(json.dumps(msg) + "\n")
            self._proc.stdin.flush()
        except BrokenPipeError:
            raise self._exited() from None

    def toolset(self, name: str, *, console: bool = False, network: bool = False) -> Toolset:
        """A vendor's browser tool executed on this browser: ``"browser_toolset_20260801"`` and
        ``"computer_toolset_20260801"`` (Anthropic), ``"computer"`` (OpenAI). Send
        ``toolset.definition`` as a tool and give ``toolset.run(...)`` each response; its results
        are the next user message (Anthropic) or input items (OpenAI). ``console`` and
        ``network`` offer ``read_console`` and ``read_network``."""
        return Toolset(self, name, console=console, network=network)

    def _notify(self, method: str) -> None:
        self._send({"jsonrpc": "2.0", "method": method})

    def _request(
        self, method: str, params: dict[str, Any], on_progress: Callable[[dict[str, Any]], None] | None = None
    ) -> Any:
        self._next += 1
        self._send({"jsonrpc": "2.0", "id": self._next, "method": method, "params": params})
        assert self._proc.stdout is not None
        while True:
            line = self._proc.stdout.readline()
            if not line:
                raise self._exited()
            reply = json.loads(line)
            if "method" in reply and "id" in reply:  # a request from pageon
                self._answer(reply, on_progress)
                continue
            if "id" not in reply:  # a notification, e.g. a takeover's progress
                if on_progress and reply.get("method") == "notifications/progress":
                    on_progress(reply.get("params", {}))
                continue
            if "error" in reply:
                raise PageonError(reply["error"].get("message", "pageon error"))
            return reply["result"]

    def _answer(self, req: dict[str, Any], on_progress: Callable[[dict[str, Any]], None] | None) -> None:
        """Answers pageon's requests: a URL-mode elicitation (the takeover's live view) goes to the
        takeover's ``on_url``; anything else is declined."""
        params = req.get("params", {})
        url = params.get("url")
        if req["method"] == "elicitation/create" and params.get("mode") == "url" and on_progress and url:
            on_progress({"_meta": {"url": url}})
            result: dict[str, Any] = {"action": "accept"}
        else:
            result = {"action": "decline"}
        self._send({"jsonrpc": "2.0", "id": req["id"], "result": result})

    def _call(
        self,
        tool: str,
        args: dict[str, Any],
        on_progress: Callable[[dict[str, Any]], None] | None = None,
        tab: Tab | None = None,
    ) -> dict[str, Any]:
        if tab is not None and tab.id != self._current:
            if tab.id not in self._tabs:
                raise ActionError("unknown_tab", f"tab {tab.id} is closed", "open one with browser.new_tab(url)")
            self._call("browser_tabs", {"action": "select", "tab": tab.id})
        params: dict[str, Any] = {"name": tool, "arguments": args}
        if on_progress:
            params["_meta"] = {"progressToken": self._next + 1}
        result = self._request("tools/call", params, on_progress)
        self._track(result)
        if result.get("isError"):
            e = result.get("structuredContent", {}).get("error", {})
            raise ActionError(e.get("kind", "engine"), e.get("message", _text(result)), e.get("guidance", ""))
        return result

    def _track(self, result: dict[str, Any]) -> None:
        """Follows pageon's tabs: every reply names the current one; a list names them all."""
        sc = result.get("structuredContent", {})
        if "tabs" in sc:
            self._tabs = {t["id"]: self._tabs.get(t["id"]) or Tab(self, t["id"]) for t in sc["tabs"]}
            self._current = next((t["id"] for t in sc["tabs"] if t["current"]), None)
        if "tab" in sc:
            self._tabs.setdefault(sc["tab"], Tab(self, sc["tab"]))
            self._current = sc["tab"]


# The members that may change a page, or send something: confirmed before they run.
_WRITES = frozenset(
    {
        "navigate",
        "left_click",
        "right_click",
        "middle_click",
        "double_click",
        "triple_click",
        "left_click_drag",
        "left_mouse_down",
        "left_mouse_up",
        "type",
        "key",
        "hold_key",
        "form_input",
        "file_upload",
        "javascript_exec",
        "scroll",
    }
)


def _plain(block: Any) -> Any:
    """A response block as a dict (SDK objects have ``model_dump``)."""
    dump = getattr(block, "model_dump", None)
    return dump(exclude_none=True) if callable(dump) else block


def _stopped(calls: list[dict[str, Any]], toolset: str, first: str | None) -> list[dict[str, Any]]:
    """Results for calls a stopped turn does not run (the first one with ``first`` if given)."""
    skipped = (
        "Not executed: an earlier action in this turn failed."
        if toolset == "browser"
        else "Not executed: an earlier computer action in this turn failed."
    )
    return [
        {
            "type": "tool_result",
            "tool_use_id": c["id"],
            "toolset_name": toolset,
            "content": [{"type": "text", "text": first if first is not None and i == 0 else skipped}],
            "is_error": True,
        }
        for i, c in enumerate(calls)
    ]


class Toolset:
    """A vendor's browser tool run by pageon. Results follow the vendor's contract: calls run
    in order and the first failure stops the turn.

        ts = browser.toolset("browser_toolset_20260801")
        r = client.messages.create(..., tools=[ts.definition], messages=messages)
        messages += [{"role": "assistant", "content": r.content},
                     {"role": "user", "content": ts.run(r.content)}]
    """

    def __init__(self, browser: Browser, name: str, *, console: bool, network: bool) -> None:
        self._browser = browser
        self.name = name
        self._flags = {"console": console, "network": network}
        self.definition: dict[str, Any] = browser._request(
            "pageon/toolset/definition", {"toolset": name, **self._flags}
        )

    def run(
        self,
        blocks: Iterable[Any],
        confirm: Callable[[str, dict[str, Any]], bool] | None = None,
    ) -> list[dict[str, Any]]:
        """Runs this toolset's calls among ``blocks`` (a response's content or output) and returns
        their results. ``confirm(member, input)`` is asked before each call that may write (for
        OpenAI: before calls with pending safety checks); ``False`` stops the turn there."""
        items = [_plain(b) for b in blocks]
        if self.name == "computer":
            calls = [c for c in items if c.get("type") == "computer_call"]
            confirmed = [
                c["call_id"] for c in calls if c.get("pending_safety_checks") and confirm and confirm("computer", c)
            ]
            return self._run(calls, confirmed)
        toolset = "browser" if self.name.startswith("browser") else "computer"
        calls = [c for c in items if c.get("type") == "tool_use" and c.get("toolset_name", toolset) == toolset]
        if confirm is None:
            return self._run(calls, [])
        # Ask before each writing call; a refusal stops the turn as a failure does.
        results: list[dict[str, Any]] = []
        for i, c in enumerate(calls):
            if c.get("name") in _WRITES and not confirm(c["name"], c.get("input", {})):
                return results + _stopped(calls[i:], toolset, "Error: the user declined this action.")
            out = self._run([c], [])
            results += out
            if out and out[0].get("is_error"):
                return results + _stopped(calls[i + 1 :], toolset, None)
        return results

    def _run(self, calls: list[dict[str, Any]], confirmed: list[str]) -> list[dict[str, Any]]:
        if not calls:
            return []
        r = self._browser._request(
            "pageon/toolset/run", {"toolset": self.name, "calls": calls, "confirmed": confirmed, **self._flags}
        )
        results: list[dict[str, Any]] = r["results"]
        # Keep the browser's tabs in step with what the toolset opened or closed.
        for res in results:
            for block in res.get("content", []) if isinstance(res.get("content"), list) else []:
                if block.get("type") == "browser_state":
                    b = self._browser
                    b._tabs = {t["tab_id"]: b._tabs.get(t["tab_id"]) or Tab(b, t["tab_id"]) for t in block["tabs"]}
                    b._current = next((t["tab_id"] for t in block["tabs"] if t.get("active")), None)
        return results


class Tab:
    """A page in its own tab. Actions return an :class:`Outcome` or raise :class:`ActionError`."""

    opened: Outcome

    def __init__(self, browser: Browser, id: str) -> None:
        self._b = browser
        self.id = id
        """The tab's handle (``t1``, ``t2``, …), never reused in a session."""

    def __repr__(self) -> str:
        return f"<Tab {self.id}>"

    def close(self) -> None:
        """Close the tab; the most recently used other tab becomes current."""
        if self.id in self._b._tabs:
            self._b._call("browser_tabs", {"action": "close", "tab": self.id})

    def _call(
        self, tool: str, args: dict[str, Any], on_progress: Callable[[dict[str, Any]], None] | None = None
    ) -> dict[str, Any]:
        return self._b._call(tool, args, on_progress, tab=self)

    def observe(self, full: bool = False, detail: str | None = None) -> str:
        """What changed since the last observation (``full=True``: the whole page).

        ``detail="compact"`` leaves text runs out of this and later views; ``"full"`` brings
        them back."""
        args: dict[str, Any] = {"full": full}
        if detail is not None:
            args["detail"] = detail
        return _text(self._call("browser_observe", args))

    def find(self, query: str, limit: int = 20) -> str:
        """Elements matching the query's words anywhere on the page, equal ones told apart."""
        return _text(self._call("browser_find", {"query": query, "limit": limit}))

    def click(self, ref: str) -> Outcome:
        return self._act("browser_click", {"ref": ref})

    def type(self, ref: str, text: str, submit: bool = False) -> Outcome:
        """Replace the field's value; ``secret://name`` types a secret the agent never sees."""
        return self._act("browser_type", {"ref": ref, "text": text, "submit": submit})

    def upload(self, ref: str, *names: str) -> Outcome:
        """Put files the operator staged (``files={"cv": ("./cv.pdf", ["https://jobs.example"])}``)
        into a file input, or into the file chooser ``ref`` opens; ``download:N`` uploads a download
        back to its own site. Never a path."""
        return self._act("browser_upload", {"ref": ref, "files": list(names)})

    def press(self, key: str) -> Outcome:
        return self._act("browser_press", {"key": key})

    def select(self, ref: str, *values: str) -> Outcome:
        """Choose options of a ``<select>``; on any other element, select that text in it."""
        return self._act("browser_select", {"ref": ref, "values": list(values)})

    def paste(self, text: str, ref: str | None = None) -> Outcome:
        """Paste ``text`` into ``ref`` (at the end of its content) or the focused element; the
        page gets a ``paste`` event, as from Ctrl+V."""
        args = {"text": text} if ref is None else {"ref": ref, "text": text}
        return self._act("browser_paste", args)

    def hover(self, ref: str) -> Outcome:
        return self._act("browser_hover", {"ref": ref})

    def drag(self, ref: str, to: str) -> Outcome:
        """Drag ``ref`` onto ``to`` (centre to centre), as a person does; the outcome says whether
        the page ran drag and drop and whether the drop was accepted."""
        return self._act("browser_drag", {"ref": ref, "to": to})

    def scroll(self, direction: str | None = "down", pixels: float | None = None, ref: str | None = None) -> Outcome:
        """Scroll as a person does: ``direction`` (down, up, left, right) by a page or ``pixels``,
        of the page or the container ``ref``; ``scroll(None, ref="e12")`` brings e12 into view.
        Actions already scroll their target into view."""
        args: dict[str, Any] = {}
        if direction is not None:
            args["direction"] = direction
        if pixels is not None:
            args["pixels"] = pixels
        if ref is not None:
            args["ref"] = ref
        return self._act("browser_scroll", args)

    def wait(self, ms: int) -> Outcome:
        return self._act("browser_wait", {"ms": ms})

    def back(self) -> Outcome:
        return self._act("browser_back", {})

    def forward(self) -> Outcome:
        return self._act("browser_forward", {})

    def reset(self, step: int | None = None) -> str:
        """Goes back to an earlier step (default: undo the last one), exactly and without sending
        anything again; returns the observation there, with the refs you had then."""
        args: dict[str, Any] = {} if step is None else {"step": step}
        return _text(self._call("browser_reset", args))

    def call_tool(self, name: str, origin: str | None = None, **arguments: Any) -> Any:
        """Calls a tool the page declares (WebMCP), listed as ``tool "NAME"`` lines at
        the top of observations; returns its result (parsed JSON, page data)."""
        args: dict[str, Any] = {"name": name, "arguments": arguments}
        if origin is not None:
            args["origin"] = origin
        r = self._call("browser_call_tool", args)
        if r.get("isError"):
            e = r.get("structuredContent", {}).get("error", {})
            raise ActionError(e.get("kind", "engine"), e.get("message", _text(r)), e.get("guidance", ""))
        result = r.get("structuredContent", {}).get("outcome", {}).get("tool_result")
        if result is None:
            return None
        try:
            return json.loads(result)
        except ValueError:  # cut at 32 KB: the text as it is
            return result

    def workflow_save(self) -> dict[str, Any]:
        """This session's steps as a workflow: elements named by role and name, typed
        values as parameters. Store it, then run it with :meth:`workflow_run`."""
        return self._call("browser_workflow_save", {}).get("structuredContent", {}).get("workflow", {})

    def workflow_run(self, workflow: dict[str, Any], **params: str) -> dict[str, Any]:
        """Runs a workflow without deciding each step; returns the report (``status`` is
        ``done`` or ``drifted``, with ``stop`` saying where, why and what the page offers).
        After a drift the page stays there: finish by hand, then :meth:`workflow_save`."""
        r = self._call("browser_workflow_run", {"workflow": workflow, "params": params})
        if r.get("isError"):
            e = r.get("structuredContent", {}).get("error", {})
            raise ActionError(e.get("kind", "engine"), e.get("message", _text(r)), e.get("guidance", ""))
        return r.get("structuredContent", {})

    def lookahead(self, actions: Iterable[str]) -> Lookahead:
        """Runs ``actions`` (pageon's action text: ``click e5``, ``type e3 "ada"``) on a fork of
        the page and returns what they would do; the page, its storage and your refs stay as
        they are, and a request that could change something is held, never sent."""
        r = self._call("browser_lookahead", {"actions": list(actions)})
        d = r.get("structuredContent", {})
        return Lookahead(
            _text(r),
            tuple(d.get("outcomes", ())),
            d.get("observation"),
            d.get("pending"),
            d.get("error"),
            d.get("ms", 0),
        )

    def view(self, kind: str = "markdown") -> str:
        """The page as ``markdown``, ``text``, ``requests`` or ``errors``."""
        return _text(self._call("browser_view", {"kind": kind}))

    def screenshot(
        self, marks: bool = False, full_page: bool = False, format: str = "png", mask: bool = False
    ) -> Screenshot:
        """The page as an image; ``marks=True`` labels every visible element with its ref;
        ``mask=True`` paints over third-party frames and text a person cannot read."""
        args = {"marks": marks, "full_page": full_page, "format": format, "mask": mask}
        return _screenshot(self._call("browser_screenshot", args))

    def takeover(self, reason: str, timeout_s: int = 600, on_url: Callable[[str], None] | None = None) -> Outcome:
        """Hands the page to a person (CAPTCHA, login, second factor) and waits until they
        press Done. ``on_url`` gets the live view's URL (default: printed); the returned
        outcome's ``observation`` is the page afterwards. Raises ``ActionError`` (``timeout``)
        if nobody takes over in time."""

        def announce(progress: dict[str, Any]) -> None:
            # The URL is pageon's, in a field of its own; the message may carry page text.
            url = progress.get("_meta", {}).get("url")
            if isinstance(url, str) and url.startswith("http://127.0.0.1:"):
                (on_url or (lambda u: print(f"pageon needs you ({reason!r}): open {u}", flush=True)))(url)

        return _outcome(self._call("browser_takeover", {"reason": reason, "timeout_s": timeout_s}, announce))

    def crop(self, ref: str, format: str = "png") -> Screenshot:
        """One element's pixels, with ``provenance`` (where they come from)."""
        return _screenshot(self._call("browser_screenshot", {"ref": ref, "format": format}))

    def _act(self, tool: str, args: dict[str, Any]) -> Outcome:
        return _outcome(self._call(tool, args))


def _screenshot(result: dict[str, Any]) -> Screenshot:
    image = next(c for c in result.get("content", []) if c.get("type") == "image")
    s = result.get("structuredContent", {})
    return Screenshot(
        data=base64.b64decode(image["data"]),
        mime=image["mimeType"],
        width=s.get("width", 0),
        height=s.get("height", 0),
        tokens=s.get("tokens", 0),
        notes=tuple(s.get("notes") or ()),
        legend=s.get("legend"),
        provenance=s.get("provenance"),
    )


def _text(result: dict[str, Any]) -> str:
    return "".join(c.get("text", "") for c in result.get("content", []) if c.get("type") == "text")


def _outcome(result: dict[str, Any]) -> Outcome:
    structured = result.get("structuredContent", {})
    text = _text(result)
    observation = structured.get("observation") or text
    lines = []
    for line in text.splitlines():
        if line.startswith(("@page", "@diff")):
            break
        if line.strip():
            lines.append(line)
    data = structured.get("outcome") or {}
    if lines and lines[-1].startswith("step ") and lines[-1][5:].isdigit():
        lines.pop()
    return Outcome(
        "\n".join(lines),
        observation,
        data.get("navigated"),
        data,
        structured.get("step"),
        structured.get("refusal") or data.get("refusal"),
    )


def _version() -> str:
    from ._native import __version__

    return __version__
