"""Golden-corpus tooling: serve local fixtures on two origins and capture them.

Fixtures live in ``bench/fixtures/pages/*.html`` and ``bench/fixtures/sequences/<name>/``.
They are served on ``http://a.localhost:<port>`` and ``http://b.localhost:<port>`` (both
loopback in Chromium), so a fixture can embed a cross-origin iframe from the other origin.
"""

from __future__ import annotations

import contextlib
import functools
import http.server
import threading
from pathlib import Path

BENCH = Path(__file__).resolve().parents[2]
FIXTURES = BENCH / "fixtures"
CORPUS = BENCH.parent / "crates" / "pageon-agent" / "tests" / "corpus"


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args) -> None:
        pass


PORT = 47811  # fixed, so golden snapshots do not depend on the port


@contextlib.contextmanager
def serve(root: Path = FIXTURES, port: int = PORT):
    """Serve ``root`` on a fixed loopback port; yields the port."""
    handler = functools.partial(_Quiet, directory=str(root))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address[1]
    finally:
        server.shutdown()


def fixture_url(port: int, rel: str, origin: str = "a") -> str:
    return f"http://{origin}.localhost:{port}/{rel}"
