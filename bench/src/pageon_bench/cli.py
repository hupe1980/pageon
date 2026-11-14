"""`pageon-bench`: the serializer proof's command line.

Exit codes: 0 ok · 1 usage · 2 environment · 3 gate FAIL · 4 gate INCONCLUSIVE.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import urllib.robotparser
from pathlib import Path
from urllib.parse import urlsplit

from . import __version__

EXIT_ENV = 2
MINIWOB_COMMIT = "7fd85d71a4b60325c6585396ec4f48377d049838"  # pinned MiniWoB++ (Farama)
CACHE = Path(__file__).resolve().parents[2] / ".cache"
UA_TOKEN = f"pageon-bench/{__version__}"


class EnvError(Exception):
    """A missing prerequisite; the message says how to fix it."""


def default_endpoint() -> str:
    return f"http://127.0.0.1:{os.environ.get('MRUN_PORT', '8484')}/v1"


def miniwob_url() -> str:
    url = os.environ.get("MINIWOB_URL")
    if url:
        return url
    html = CACHE / "miniwob" / "miniwob" / "html" / "miniwob"
    if not html.is_dir():
        raise EnvError("MiniWoB++ pages not found\n  run `uv run pageon-bench setup` once, or set MINIWOB_URL")
    return html.as_uri() + "/"


def check_endpoint(endpoint: str) -> None:
    import urllib.request

    try:
        urllib.request.urlopen(endpoint.rstrip("/") + "/models", timeout=3).read()
    except Exception:
        raise EnvError(
            f"no model server at {endpoint}\n"
            "  start one with `mrun serve` (or any OpenAI-compatible server) and retry, or pass --endpoint"
        ) from None


def _browser(p):
    try:
        return p.chromium.launch()
    except Exception as e:
        if "Executable doesn't exist" in str(e):
            raise EnvError("Chromium for Playwright is missing\n  run `uv run pageon-bench setup` once") from None
        raise


# ---------------------------------------------------------------- commands


def cmd_setup(args) -> int:
    print("· installing Chromium for Playwright (idempotent)")
    subprocess.run([sys.executable, "-m", "playwright", "install", "chromium"], check=True)
    dest = CACHE / "miniwob"
    if not (dest / "miniwob").is_dir():
        print(f"· fetching MiniWoB++ at {MINIWOB_COMMIT[:12]}")
        if not shutil.which("git"):
            raise EnvError("git is required to fetch MiniWoB++\n  install git and retry")
        dest.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["git", "clone", "--quiet", "https://github.com/Farama-Foundation/miniwob-plusplus", str(dest)], check=True
        )
        subprocess.run(["git", "-C", str(dest), "checkout", "--quiet", MINIWOB_COMMIT], check=True)
    print(f"✓ ready  MINIWOB_URL={miniwob_url()}")
    return 0


def _robots_ok(url: str) -> bool:
    u = urlsplit(url)
    if u.hostname in ("localhost", "127.0.0.1") or (u.hostname or "").endswith(".localhost"):
        return True
    rp = urllib.robotparser.RobotFileParser(f"{u.scheme}://{u.netloc}/robots.txt")
    try:
        rp.read()
    except Exception:
        return True  # unreachable robots.txt: no rules
    return rp.can_fetch(UA_TOKEN, url)


def _capture_url(url: str, out: Path, wait: str = "load", step: int = 0, script: str | None = None) -> dict:
    from playwright.sync_api import sync_playwright

    from .capture import capture_page, write_capture
    from .formats import live_baselines

    with sync_playwright() as p:
        browser = _browser(p)
        ua = p.devices["Desktop Chrome"]["user_agent"] + " " + UA_TOKEN
        page = browser.new_page(viewport={"width": 1280, "height": 720}, user_agent=ua)
        page.goto(url, wait_until=wait, timeout=45_000)
        page.wait_for_timeout(300)
        if script:
            page.evaluate(script)
            page.wait_for_timeout(200)
        baselines, extras = live_baselines(page)
        cap = capture_page(page, step=step, baselines=baselines, extras=extras)
        browser.close()
    write_capture(cap, out)
    return cap


def cmd_capture(args) -> int:
    if not _robots_ok(args.url):
        print(
            f"error: robots.txt of {urlsplit(args.url).netloc} disallows {UA_TOKEN}; pick another page", file=sys.stderr
        )
        return 1
    _capture_url(args.url, Path(args.output), wait=args.wait)
    print(f"✓ {args.output}")
    return 0


def cmd_corpus(args) -> int:
    """Re-capture every constructed fixture and sequence into the golden corpus."""
    from playwright.sync_api import sync_playwright

    from .capture import capture_page, write_capture
    from .corpus import CORPUS, FIXTURES, fixture_url, serve
    from .formats import live_baselines

    only = set(args.only or [])
    with serve() as port, sync_playwright() as p:
        browser = _browser(p)
        for html in sorted((FIXTURES / "pages").glob("*.html")):
            if html.stem in ("widget", "inner") or (only and html.stem not in only):
                continue
            page = browser.new_page(viewport={"width": 1280, "height": 720})
            page.goto(fixture_url(port, f"pages/{html.name}"), wait_until="load")
            page.wait_for_timeout(400)
            b, e = live_baselines(page)
            write_capture(capture_page(page, baselines=b, extras=e), CORPUS / f"{html.stem}.capture.json.gz")
            page.close()
            print(f"✓ {html.stem}")
        for seq in sorted((FIXTURES / "sequences").iterdir()):
            if not seq.is_dir() or (only and seq.name not in only):
                continue
            page = browser.new_page(viewport={"width": 1280, "height": 720})
            page.goto(fixture_url(port, f"sequences/{seq.name}/index.html"), wait_until="load")
            nav, url, k = 0, page.url, 0
            out = CORPUS / "sequences" / seq.name
            while True:
                if page.url != url:
                    nav, url = nav + 1, page.url
                b, e = live_baselines(page)
                write_capture(
                    capture_page(page, step=k, nav_counter=nav, baselines=b, extras=e),
                    out / f"step-{k}.capture.json.gz",
                )
                n = page.evaluate("(window.steps || []).length")
                if k >= n:
                    break
                with page.expect_navigation(wait_until="load", timeout=2000) if seq.name == "navigate" else _null():
                    page.evaluate(f"window.steps[{k}]()")
                page.wait_for_timeout(200)
                k += 1
            page.close()
            print(f"✓ sequences/{seq.name} ({k + 1} steps)")
        browser.close()
    return 0


class _null:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def cmd_observe(args) -> int:
    import json
    import tempfile

    import pageon

    from .capture import read_capture
    from .formats import make, observe_all

    target = args.target
    if Path(target).exists():
        path = Path(target)
    else:
        if not _robots_ok(target):
            print(f"error: robots.txt disallows {UA_TOKEN} on this page", file=sys.stderr)
            return 1
        path = Path(tempfile.mkdtemp()) / "page.capture.json.gz"
        _capture_url(target, path)
    capture = read_capture(path)
    if args.all:
        for name, text in observe_all(capture, path, args.budget).items():
            print(f"\n━━ {name} · {pageon.count_tokens(text)} tokens (o200k) ━━")
            print(text)
        return 0
    if args.format in ("pageon", "pageon_full"):
        print(pageon.ax(pageon.Capture.load(str(path)), budget=args.budget))
    else:
        print(make(args.format, args.budget).observe(capture, path))
    if args.json:
        print(json.dumps({"tokens": pageon.count_tokens(capture["title"])}))
    return 0


ORACLE_JS = """() => [...document.body.querySelectorAll('*')].map(e => {
  const r = e.getBoundingClientRect();
  return {tag: e.localName, rendered: e.getClientRects().length > 0,
          x: r.x, y: r.y, w: r.width, h: r.height};
})"""


def cmd_geometry_oracle(args) -> int:
    """Chromium's boxes for every geometry fixture (JS off, 1280x720): the layout oracle."""
    import json

    from playwright.sync_api import sync_playwright

    from .corpus import BENCH

    root = BENCH.parent / "crates" / "pageon-layout" / "tests" / "geometry"
    pages = sorted(root.glob("*.html"))
    with sync_playwright() as p:
        browser = _browser(p)
        ctx = browser.new_context(
            viewport={"width": 1280, "height": 720}, java_script_enabled=False, device_scale_factor=1
        )
        for html in pages:
            page = ctx.new_page()
            page.goto(html.as_uri(), wait_until="load")
            boxes = page.evaluate(ORACLE_JS)
            out = {"viewport": [1280, 720], "chromium": browser.version, "elements": boxes}
            html.with_suffix(".boxes.json").write_text(json.dumps(out, indent=1) + "\n")
            page.close()
            print(f"✓ {html.name}: {len(boxes)} elements")
        browser.close()
    return 0


def cmd_scroll_oracle(args) -> int:
    """Chromium's values for the scrolling fixture (JS on, 1280x720): the CSSOM View oracle."""
    import json

    from playwright.sync_api import sync_playwright

    from .corpus import BENCH

    html = BENCH.parent / "crates" / "pageon" / "tests" / "fixtures" / "scroll" / "cssom.html"
    with sync_playwright() as p:
        browser = _browser(p)
        ctx = browser.new_context(viewport={"width": 1280, "height": 720}, device_scale_factor=1)
        page = ctx.new_page()
        page.goto(html.as_uri(), wait_until="load")
        results = page.evaluate("() => window.results")
        out = {"viewport": [1280, 720], "chromium": browser.version, "results": results}
        html.with_suffix(".chromium.json").write_text(json.dumps(out, indent=1) + "\n")
        browser.close()
    print(f"✓ {html.name}: {len(results)} values")
    return 0


def cmd_bench(args) -> int:
    from . import runner

    return runner.main(args)


def cmd_report(args) -> int:
    from . import report

    report.write(Path(args.run_dir))
    print(f"✓ {Path(args.run_dir) / 'report.md'}")
    return 0


def cmd_gate(args) -> int:
    from . import report

    verdict = report.gate_verdict(Path(args.run_dir))
    print(verdict)
    return {"PASS": 0, "FAIL": 3}.get(verdict, 4)


def cmd_paint_parity(args) -> int:
    from . import parity

    return parity.main(args)


def cmd_fidelity(args) -> int:
    from . import fidelity

    return fidelity.main(args)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="pageon-bench", description="pageon's serializer proof (the serializer proof)")
    ap.add_argument("--version", action="version", version=__version__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("setup", help="install Chromium and MiniWoB++ (once, idempotent)").set_defaults(fn=cmd_setup)

    c = sub.add_parser("capture", help="capture one real page for the corpus (honours robots.txt)")
    c.add_argument("url")
    c.add_argument("-o", "--output", required=True)
    c.add_argument("--wait", choices=["load", "networkidle"], default="load")
    c.set_defaults(fn=cmd_capture)

    c = sub.add_parser("corpus", help="re-capture the constructed fixtures into the golden corpus")
    c.add_argument("--only", nargs="*", help="fixture or sequence names")
    c.set_defaults(fn=cmd_corpus)

    c = sub.add_parser("observe", help="print a format for a URL or a stored capture")
    c.add_argument("target", help="URL or .capture.json.gz")
    c.add_argument(
        "--format", default="pageon", choices=["pageon", "pageon_full", "playwright_mcp", "browsergym", "browser_use"]
    )
    c.add_argument("--budget", type=int, default=4000)
    c.add_argument("--all", action="store_true", help="every format side by side, with token counts")
    c.add_argument("--json", action="store_true")
    c.set_defaults(fn=cmd_observe)

    for name, help_ in (("smoke", "5 tasks × 5 formats × 1 run"), ("run", "the full benchmark (resumes)")):
        c = sub.add_parser(name, help=help_)
        c.add_argument("--endpoint", default=default_endpoint())
        c.add_argument("--model", default="qwen3.6")
        c.add_argument("--runs", type=int, default=1 if name == "smoke" else 3)
        c.add_argument("--tasks", default="smoke" if name == "smoke" else "all")
        c.add_argument("--formats", nargs="*", default=None)
        c.add_argument("--budget", type=int, default=4000)
        c.add_argument("--max-hours", type=float, default=24.0)
        c.add_argument("--max-steps", type=int, default=10)
        c.add_argument("--seed", type=int, default=1)
        c.add_argument("--run-dir", default=None)
        c.set_defaults(fn=cmd_bench, smoke=(name == "smoke"))

    c = sub.add_parser("report", help="write report.json and report.md for a run")
    c.add_argument("run_dir")
    c.set_defaults(fn=cmd_report)

    c = sub.add_parser("gate", help="print the verdict; exit 0 PASS, 3 FAIL, 4 INCONCLUSIVE")
    c.add_argument("run_dir")
    c.set_defaults(fn=cmd_gate)

    c = sub.add_parser("geometry-oracle", help="Chromium boxes for the layout geometry fixtures (JS off)")
    c.set_defaults(fn=cmd_geometry_oracle)
    c = sub.add_parser("scroll-oracle", help="Chromium values for the CSSOM View scrolling fixture")
    c.set_defaults(fn=cmd_scroll_oracle)

    c = sub.add_parser("paint-parity", help="pageon screenshots vs Chromium (SSIM) on the corpus pages")
    c.set_defaults(fn=cmd_paint_parity)

    c = sub.add_parser("fidelity", help="compare baseline reproductions with their references")
    c.set_defaults(fn=cmd_fidelity)

    args = ap.parse_args(argv)
    try:
        return args.fn(args)
    except EnvError as e:
        print(f"error: {e}", file=sys.stderr)
        return EXIT_ENV
    except KeyboardInterrupt:
        print("\ninterrupted; run the same command again to resume", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
