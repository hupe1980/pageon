"""pageon.Browser over `pageon mcp`, offline against a recording.

Needs the binary: PAGEON_BIN, or `pageon` on PATH (`cargo build -p pageon`).
"""

from __future__ import annotations

import json
import os
import shutil
from pathlib import Path

import pageon
import pytest
from pageon import ActionError, Browser, Outcome

ROOT = Path(__file__).resolve().parents[3]


def _bin() -> str | None:
    if os.environ.get("PAGEON_BIN"):
        return os.environ["PAGEON_BIN"]
    for profile in ("debug", "release"):
        p = ROOT / "target" / profile / "pageon"
        if p.exists():
            return str(p)
    return shutil.which("pageon")


BIN = _bin()
pytestmark = pytest.mark.skipif(BIN is None, reason="build the binary: cargo build -p pageon")


def _html(url: str, body: str, method: str = "GET") -> dict:
    return {"method": method, "url": url, "status": 200, "headers": [["content-type", "text/html"]], "body": body}


@pytest.fixture
def shop(tmp_path: Path) -> Path:
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [
            _html(
                "https://shop.test/",
                "<h1>Shop</h1><a href=/login>Sign in</a><button disabled>Buy</button>"
                "<select aria-label=Size><option>S<option>M</select>",
            ),
            _html(
                "https://shop.test/login",
                "<form method=post action=/session><input name=user aria-label=User>"
                "<input name=pw type=password aria-label=Password><button>Sign in</button></form>",
            ),
            _html("https://shop.test/session", "<h1>Welcome</h1><p>hello ada</p>", method="POST"),
        ],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    p = tmp_path / "shop.rec.json"
    p.write_text(json.dumps(log))
    return p


def ref(observation: str, role: str, name: str) -> str:
    for line in observation.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[0] == role and f'"{name}"' in line:
            return parts[1]
    raise AssertionError(f"no {role} {name!r} in\n{observation}")


@pytest.mark.parametrize("sandbox", [True, False])
def test_browse_and_log_in(shop: Path, sandbox: bool, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("PAGEON_SECRET_PW", "very-secret-pw")
    with Browser(replay=shop, sandbox=sandbox, secrets=["pw@https://shop.test"], binary=BIN) as browser:
        tab = browser.open("https://shop.test/")
        assert str(tab.opened).startswith("ok open https://shop.test/")
        page = tab.observe(full=True)
        assert 'link e1 "Sign in"' in page

        with pytest.raises(ActionError) as err:
            tab.click(ref(page, "button", "Buy"))
        assert err.value.kind == "not_actionable"
        assert err.value.guidance
        assert isinstance(err.value, pageon.PageonError)

        out = tab.select(ref(page, "combobox", "Size"), "M")
        assert isinstance(out, Outcome)
        assert 'ok select e3 "M"' in str(out)
        assert "Shop" in tab.view("text")

        out = tab.click("e1")
        assert out.navigated == "https://shop.test/login"
        assert "@page" in out.observation and "@page" not in str(out)
        form = tab.observe(full=True)
        tab.type(ref(form, "textbox", "User"), "ada")
        out = tab.type(ref(form, "textbox", "Password"), "secret://pw", submit=True)
        assert out.navigated == "https://shop.test/session"
        assert '"Welcome"' in out.observation
        everything = repr(out) + out.observation + tab.view("requests") + tab.view("markdown")
        assert "very-secret-pw" not in everything


def test_errors_are_clear(shop: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("PAGEON_BIN", raising=False)
    monkeypatch.setenv("PATH", str(tmp_path))
    with pytest.raises(pageon.PageonError, match="PAGEON_BIN"):
        Browser()
    with pytest.raises(pageon.PageonError, match="not found"):
        Browser(binary=tmp_path / "missing")
    monkeypatch.undo()
    # A typo cannot turn the origin policy off: the CLI's fix reaches Python.
    with pytest.raises(pageon.PageonError, match=r"https://shop\.example"):
        Browser(replay=shop, binary=BIN, origins=["shop.example"])
    with Browser(replay=shop, binary=BIN) as browser:
        with pytest.raises(ActionError) as err:
            browser.open("https://elsewhere.test/")
        assert err.value.kind == "navigation"
        assert "--record" in str(err.value)


def test_state_is_saved_on_close(shop: Path, tmp_path: Path) -> None:
    saved = tmp_path / "state.json"
    with Browser(replay=shop, save_state=saved, binary=BIN) as browser:
        browser.open("https://shop.test/")
        assert not saved.exists()
    state = json.loads(saved.read_text())
    assert set(state) >= {"cookies", "origins"}
    with Browser(replay=shop, state=saved, binary=BIN) as browser:
        assert "Shop" in browser.open("https://shop.test/").opened.observation


@pytest.mark.parametrize("sandbox", [True, False])
def test_screenshots(shop: Path, sandbox: bool, tmp_path: Path) -> None:
    with Browser(replay=shop, sandbox=sandbox, binary=BIN) as browser:
        tab = browser.open("https://shop.test/")
        shot = tab.screenshot(marks=True)
        assert bytes(shot).startswith(b"\x89PNG")
        assert (shot.width, shot.height, shot.mime) == (1280, 720, "image/png")
        assert 'link e1 "Sign in"' in (shot.legend or "")
        assert shot.save(tmp_path / "shop.png").stat().st_size == len(shot.data)
        crop = tab.crop("e1", format="jpeg")
        assert crop.data.startswith(b"\xff\xd8") and crop.mime == "image/jpeg"
        assert crop.provenance and crop.provenance["tier"] == "first-party"
        with pytest.raises(ActionError) as err:
            tab.crop("e99")
        assert err.value.kind == "unknown_ref"


def test_takeover_hands_the_page_to_a_person(shop: Path) -> None:
    import threading
    import urllib.request

    with Browser(replay=shop, sandbox=False, binary=BIN) as browser:
        tab = browser.open("https://shop.test/")

        def person(url: str) -> None:
            def act() -> None:
                with urllib.request.urlopen(url) as r:  # opens the view; the cookie binds the viewer
                    cookie = r.headers["Set-Cookie"].split(";")[0]
                req = urllib.request.Request(url + "/done", method="POST", headers={"Cookie": cookie})
                urllib.request.urlopen(req).close()

            threading.Thread(target=act).start()

        seen: list[str] = []

        def checked(url: str) -> None:
            seen.append(url)
            person(url)

        # A reason that carries page text with a URL in it never becomes the live view's URL.
        out = tab.takeover("check http://evil.example/x the page", timeout_s=30, on_url=checked)
        assert len(seen) == 1 and seen[0].startswith("http://127.0.0.1:"), seen
        assert out.line == "ok takeover: the person handed the page back after 0 inputs"
        assert out.observation.startswith(("@page", "@diff"))
        with pytest.raises(ActionError) as err:
            tab.takeover("nobody", timeout_s=1, on_url=lambda _u: None)
        assert err.value.kind == "timeout"


@pytest.mark.parametrize("sandbox", [True, False])
def test_reset_goes_back(shop: Path, sandbox: bool) -> None:
    with Browser(replay=shop, sandbox=sandbox, binary=BIN) as browser:
        tab = browser.open("https://shop.test/")
        assert tab.opened.step == 0
        page = tab.observe(full=True)
        out = tab.select(ref(page, "combobox", "Size"), "M")
        assert out.step == 1 and "step 1" not in out.line
        back = tab.reset()
        assert back.startswith("ok reset to step 0")
        with pytest.raises(ActionError) as err:
            tab.reset(step=7)
        assert err.value.kind == "bad_step"


def test_refusals_are_typed(tmp_path: Path) -> None:
    challenge = {
        "method": "GET",
        "url": "https://shop.test/cf",
        "status": 403,
        "headers": [["content-type", "text/html"], ["cf-mitigated", "challenge"]],
        "body": "<p>Checking your browser",
    }
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [_html("https://shop.test/", "<a href=/cf>Deals</a>"), challenge],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    p = tmp_path / "refusal.rec.json"
    p.write_text(json.dumps(log))
    with Browser(replay=p, binary=BIN) as browser:
        tab = browser.open("https://shop.test/")
        assert tab.opened.refusal is None
        out = tab.click(ref(tab.observe(full=True), "link", "Deals"))
        assert out.refusal is not None
        assert (out.refusal["kind"], out.refusal["vendor"], out.refusal["status"]) == ("challenge", "cloudflare", 403)
        assert out.observation.startswith("refused: challenge by cloudflare")


def test_lookahead_sees_without_doing(shop: Path) -> None:
    with Browser(replay=shop, binary=BIN) as browser:
        tab = browser.open("https://shop.test/")
        page = tab.observe(full=True)
        la = tab.lookahead([f"click {ref(page, 'link', 'Sign in')}"])
        assert str(la).startswith("lookahead (the page is unchanged; writes were held")
        assert la.pending is None and la.error is None
        assert la.observation and "Password" in la.observation
        assert "no change" in tab.observe()
        la = tab.lookahead(["click e999"])
        assert la.error is not None and la.outcomes == ()


def test_guard_and_mask(shop: Path) -> None:
    # A guard echoes each effect's id (the first field of the line) with its verdict.
    deny_posts = (
        'while read -r l; do id=$(printf %s "$l" | sed \'s/^{"id":\\([0-9]*\\).*/\\1/\'); '
        'case "$l" in *\'"method":"POST"\'*) '
        'echo "{\\"id\\":$id,\\"verdict\\":\\"deny\\",\\"reason\\":\\"read-only task\\"}";; '
        '*) echo "{\\"id\\":$id,\\"verdict\\":\\"allow\\"}";; esac; done'
    )
    with Browser(replay=shop, guard=deny_posts, binary=BIN) as browser:
        tab = browser.open("https://shop.test/login")
        page = tab.observe(full=True)
        tab.type(ref(page, "textbox", "User"), "ada")
        with pytest.raises(ActionError) as err:
            tab.click(ref(page, "button", "Sign in"))
        assert "guard: read-only task" in str(err.value)
        shot = tab.screenshot(mask=True)
        assert shot.width > 0


def test_workflows_save_run_and_drift(tmp_path: Path) -> None:
    login = _html("https://shop.test/login", "<label>User <input name=user></label><a href=/next>Next</a>")
    nxt = _html("https://shop.test/next", "<h1>Next</h1>")
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [login, nxt, login, nxt],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    p = tmp_path / "wf.rec.json"
    p.write_text(json.dumps(log))
    with Browser(replay=p, binary=BIN) as browser:
        tab = browser.open("https://shop.test/login")
        page = tab.observe(full=True)
        tab.type(ref(page, "textbox", "User"), "ada")
        tab.click(ref(page, "link", "Next"))
        wf = tab.workflow_save()
        assert [s["do"] for s in wf["steps"]] == ["type", "click"]
        assert wf["params"] == [{"name": "user", "default": "ada"}]
        report = tab.workflow_run(wf, user="bob")
        assert report["status"] == "done", report
        with pytest.raises(ActionError) as err:
            tab.workflow_run(wf, nobody="x")
        assert err.value.kind == "invalid_workflow"


def test_call_tool(tmp_path: Path) -> None:
    page = _html(
        "https://air.test/",
        "<script>document.modelContext.registerTool({name: 'searchFlights', description: 'Find flights',"
        " execute: async ({from}) => ({from, price: 99})})</script>",
    )
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [page],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    p = tmp_path / "air.rec.json"
    p.write_text(json.dumps(log))
    with Browser(replay=p, binary=BIN) as browser:
        tab = browser.open("https://air.test/")
        assert 'tool "searchFlights"' in tab.observe(full=True)
        assert tab.call_tool("searchFlights", **{"from": "TXL"}) == {"from": "TXL", "price": 99}
        with pytest.raises(ActionError) as err:
            tab.call_tool("nope")
        assert err.value.kind == "tool"


def test_scroll(tmp_path: Path) -> None:
    long = "<body style='margin:0'><div style='height:3000px'>Top</div><button>Deep</button></body>"
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [_html("https://long.test/", long)],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(64)],
        "random": list(range(1, 65)),
    }
    rec = tmp_path / "long.rec.json"
    rec.write_text(json.dumps(log))
    with Browser(replay=rec, binary=BIN) as browser:
        tab = browser.open("https://long.test/")
        assert "scroll:0/" in tab.observe(full=True)
        out = tab.scroll("down")
        assert "scrolled to 0,630" in str(out)
        out = tab.scroll(None, ref=ref(tab.observe(full=True), "button", "Deep"))
        assert "scrolled to 0," in str(out)
        tab.scroll("down", pixels=100000)
        assert "already at the end" in str(tab.scroll("down"))


@pytest.mark.parametrize("sandbox", [True, False])
def test_upload_a_staged_file(tmp_path: Path, sandbox: bool) -> None:
    cv = tmp_path / "cv.txt"
    cv.write_text("hello cv")
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [
            _html(
                "https://jobs.test/",
                "<input type=file id=f aria-label=CV><p id=out></p>"
                "<script>f.onchange = () => out.textContent = 'got ' + f.files[0].name</script>",
            ),
        ],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    rec = tmp_path / "jobs.rec.json"
    rec.write_text(json.dumps(log))
    staged = {"cv": (cv, ["https://jobs.test"])}
    with Browser(replay=rec, sandbox=sandbox, files=staged, robots="ignore", binary=BIN) as browser:
        tab = browser.open("https://jobs.test/")
        page = tab.observe(full=True)
        tab.upload(ref(page, "button", "CV"), "cv")
        assert "got cv.txt" in tab.observe(full=True)
        with pytest.raises(ActionError) as err:
            tab.upload(ref(page, "button", "CV"), "/etc/passwd")
        assert "is not staged" in str(err.value)


@pytest.mark.parametrize("sandbox", [True, False])
def test_tabs_keep_their_pages(shop: Path, sandbox: bool) -> None:
    with Browser(replay=shop, sandbox=sandbox, binary=BIN) as browser:
        home = browser.open("https://shop.test/")
        assert home.id == "t1"
        size = ref(home.observe(full=True), "combobox", "Size")
        home.select(size, "M")
        login = browser.new_tab("https://shop.test/login")
        assert login.id == "t2"
        assert [t.id for t in browser.tabs] == ["t1", "t2"]
        # Each Tab acts on its own page, whichever is current.
        assert "combobox" in home.observe(full=True) and '"M"' in home.observe(full=True)
        user = ref(login.observe(full=True), "textbox", "User")
        login.type(user, "ada")
        assert "ada" in login.observe(full=True)
        login.close()
        assert [t.id for t in browser.tabs] == ["t1"]
        with pytest.raises(ActionError) as e:
            login.observe()
        assert e.value.kind == "unknown_tab"
        assert "Shop" in home.observe(full=True)


@pytest.mark.parametrize("sandbox", [True, False])
def test_drag_moves_a_card(tmp_path: Path, sandbox: bool) -> None:
    board = (
        "<style>section{display:block;min-height:80px}</style>"
        "<section aria-label=Todo><div role=button id=c draggable=true>Card</div></section>"
        "<section id=done aria-label=Done></section><script>"
        "c.addEventListener('dragstart', e => e.dataTransfer.setData('text/plain', 'c'));"
        "done.addEventListener('dragover', e => e.preventDefault());"
        "done.addEventListener('drop', e =>"
        " done.append(document.getElementById(e.dataTransfer.getData('text/plain'))));"
        "</script>"
    )
    log = {
        "version": 1,
        "user_agent": "",
        "exchanges": [_html("https://board.test/", board)],
        "clock": [1_800_000_000_000 + i * 1000 for i in range(256)],
        "random": list(range(1, 65)),
    }
    rec = tmp_path / "board.rec.json"
    rec.write_text(json.dumps(log))
    with Browser(replay=rec, sandbox=sandbox, binary=BIN) as browser:
        tab = browser.open("https://board.test/")
        obs = tab.observe(full=True)
        out = tab.drag(ref(obs, "button", "Card"), ref(obs, "droptarget", "Done"))
        assert "dragged (dropped)" in str(out)
        after = tab.observe(full=True)
        assert '"Card"' in after.split('"Done"')[-1], after
