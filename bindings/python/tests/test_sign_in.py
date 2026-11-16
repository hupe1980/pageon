"""Sign-ins that renew, and the viewer URL, from Python against a live local portal."""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest
from pageon import Browser, ExpectationError

from test_browser import BIN

pytestmark = pytest.mark.skipif(BIN is None, reason="build the binary: cargo build -p pageon")

SECRET = "py-portal-secret-42"


class Portal(BaseHTTPRequestHandler):
    token = 1
    logins = 0

    def _send(self, status: int, body: str = "", headers: dict[str, str] | None = None) -> None:
        data = body.encode()
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("content-type", "text/html")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        if self.path.startswith("/login"):
            self._send(200, "<form method=post action=/login><label>User <input name=user></label>"
                       "<label>Password <input type=password name=pw></label><button>Sign in</button></form>")
        elif f"sid=t{Portal.token}" in (self.headers.get("cookie") or ""):
            self._send(200, "<title>Orders</title><h1>Orders</h1>")
        else:
            self._send(302, headers={"location": "/login"})

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("content-length") or 0)).decode()
        Portal.logins += 1
        if f"pw={SECRET}" in body:
            self._send(302, headers={"set-cookie": f"sid=t{Portal.token}; Path=/", "location": "/orders"})
        else:
            self._send(200, "wrong")

    def log_message(self, *args: object) -> None:
        pass


@pytest.fixture
def portal():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Portal)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_address[1]}"
    server.shutdown()


def test_an_expired_sign_in_renews(portal: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("PAGEON_SECRET_SHOP", SECRET)
    common = dict(binary=BIN, sandbox=False, private_network=True, robots="ignore",
                  origins=[portal], secrets=[f"shop@{portal}"])
    with Browser(**common) as browser:
        tab = browser.open(f"{portal}/login")
        tab.type("e1", "ada")
        tab.type("e2", "secret://shop")
        tab.click("e3")
        flow = tmp_path / "login.flow.json"
        flow.write_text(json.dumps(tab.workflow_save()))
    Portal.token += 1
    with Browser(**common, sign_ins=[flow], ui=True) as browser:
        assert browser.viewer_url and browser.viewer_url.startswith("http://127.0.0.1:")
        tab = browser.open(f"{portal}/orders")
        text = str(tab.observe(full=True))
        assert "Orders" in text
        # Expectations: checked with evidence; a failed one raises.
        ok = tab.press("Tab", expect=['see "Orders"', "url /orders"])
        assert "✓ see" in str(ok)
        with pytest.raises(ExpectationError) as e:
            tab.press("Tab", expect=['see "Invoices"'])
        assert e.value.checks[0]["ok"] is False and "not on the page" in str(e.value)
    assert SECRET not in flow.read_text()
