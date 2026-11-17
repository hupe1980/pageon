"""`pageon-bench engines`: pageon beside Chromium's headless shell on the same pages, offline.

The pages are pageon recordings (the `pageon bench` cache, or files made with `--record`). pageon
replays each one; Chromium gets the same responses as a HAR through Playwright's
`route_from_har` (a request the recording does not have fails, for both).

Each run is a fresh process tree. First view: pageon's `fetch --dump ax` from start to exit;
Chromium from launch to the load event and Playwright's AI snapshot (the view Playwright MCP
sends). CPU and memory are summed over the engine's processes (pageon's broker and cage;
Chromium's browser, renderers, GPU and network processes), sampled every 10 ms; the harness
(Python, Playwright's driver) is not counted.
"""

from __future__ import annotations

import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CHROMIUM = """
import json, sys, time
from playwright.sync_api import sync_playwright
har, url = sys.argv[1], sys.argv[2]
with sync_playwright() as p:
    t = time.perf_counter()
    b = p.chromium.launch()
    ctx = b.new_context(viewport={"width": 1280, "height": 720})
    ctx.route_from_har(har, not_found="abort")
    page = ctx.new_page()
    page.goto(url, wait_until="load", timeout=60000)
    view = page.locator("body").aria_snapshot(mode="ai")
    ms = (time.perf_counter() - t) * 1000
    b.close()
print(json.dumps({"ms": ms, "chars": len(view)}))
"""

DROP = {"content-encoding", "content-length", "transfer-encoding"}


def har_of(recording: Path) -> tuple[str, dict]:
    """The recording's opening URL and its exchanges as a HAR 1.2 log."""
    log = json.loads(recording.read_text())
    url = log["steps"][0]["input"]["open"]["url"]
    entries = []
    for e in log["exchanges"]:
        headers = [{"name": k, "value": v} for k, v in e.get("headers", []) if k.lower() not in DROP]
        mime = next((v for k, v in e.get("headers", []) if k.lower() == "content-type"), "")
        if e.get("body_base64"):
            content = {"size": -1, "mimeType": mime, "text": e["body_base64"], "encoding": "base64"}
        else:
            text = e.get("body") or ""
            content = {"size": len(text.encode()), "mimeType": mime, "text": text}
        location = next((v for k, v in e.get("headers", []) if k.lower() == "location"), "")
        entries.append(
            {
                "startedDateTime": "2026-01-01T00:00:00.000Z",
                "time": 0,
                "request": {
                    "method": e.get("method", "GET"),
                    "url": e["url"],
                    "httpVersion": "HTTP/1.1",
                    "headers": [],
                    "queryString": [],
                    "cookies": [],
                    "headersSize": -1,
                    "bodySize": -1,
                },
                "response": {
                    "status": e.get("status", 200),
                    "statusText": "",
                    "httpVersion": "HTTP/1.1",
                    "headers": headers,
                    "cookies": [],
                    "content": content,
                    "redirectURL": location,
                    "headersSize": -1,
                    "bodySize": -1,
                },
                "cache": {},
                "timings": {"send": 0, "wait": 0, "receive": 0},
            }
        )
    return url, {"log": {"version": "1.2", "creator": {"name": "pageon-bench", "version": "1"}, "entries": entries}}


def _sample(root, counted) -> tuple[float, float]:
    """Waits for `root` to exit; the peak of the counted processes' summed resident memory (MB)
    and their CPU seconds, sampled every 10 ms."""
    import psutil

    peak, cpu = 0.0, {}
    proc = psutil.Process(root.pid)
    while root.poll() is None:
        try:
            tree = [proc, *proc.children(recursive=True)]
        except psutil.NoSuchProcess:
            break
        rss = 0
        for q in tree:
            try:
                if counted(q):
                    rss += q.memory_info().rss
                    t = q.cpu_times()
                    cpu[q.pid] = t.user + t.system
            except psutil.Error:
                pass
        peak = max(peak, rss / 2**20)
        time.sleep(0.01)
    return peak, sum(cpu.values())


def _pageon(binary: str, url: str, recording: Path) -> dict:
    t = time.perf_counter()
    p = subprocess.Popen(
        [binary, "fetch", "--dump", "ax", "--replay", str(recording), "--", url],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    peak, cpu = _sample(p, lambda q: "pageon" in q.name())
    out = p.stdout.read() if p.stdout else b""
    ms = (time.perf_counter() - t) * 1000
    if p.returncode != 0:
        raise RuntimeError(f"pageon failed on {url} (exit {p.returncode})")
    return {"ms": ms, "cpu_s": cpu, "memory_mb": peak, "chars": len(out)}


def _chromium(url: str, har: Path) -> dict:
    p = subprocess.Popen(
        [sys.executable, "-I", "-c", CHROMIUM, str(har), url],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    peak, cpu = _sample(p, lambda q: "chrom" in q.name().lower() or "headless" in q.name().lower())
    out, err = p.communicate()
    if p.returncode != 0:
        raise RuntimeError(f"Chromium failed on {url}: {err.decode(errors='replace').strip().splitlines()[-1:]}")
    r = json.loads(out)
    return {"ms": r["ms"], "cpu_s": cpu, "memory_mb": peak, "chars": r["chars"]}


def _median(rows: list[dict], key: str) -> float:
    return statistics.median(r[key] for r in rows)


def main(args) -> int:
    binary = args.pageon or os.environ.get("PAGEON", "pageon")
    results = []
    with tempfile.TemporaryDirectory() as tmp:
        for rec in map(Path, args.recordings):
            url, har = har_of(rec)
            har_path = Path(tmp) / (rec.stem + ".har")
            har_path.write_text(json.dumps(har))
            row = {"page": url, "recording": str(rec)}
            engines = (
                ("pageon", _pageon, (binary, url, rec)),
                ("chromium", _chromium, (url, har_path)),
            )
            for name, run, run_args in engines:
                try:
                    runs = [run(*run_args) for _ in range(args.runs)]
                    row[name] = {k: _median(runs, k) for k in ("ms", "cpu_s", "memory_mb", "chars")}
                except Exception as e:
                    row[name] = {"error": str(e)}
                print(f"{url} {name} done", file=sys.stderr)
            results.append(row)
    if args.json:
        print(json.dumps(results, indent=2))
        return 0
    print("| Page | Engine | First view | CPU | Memory |")
    print("| --- | --- | --- | --- | --- |")
    for r in results:
        for name in ("pageon", "chromium"):
            m = r[name]
            if "error" in m:
                print(f"| {r['page']} | {name} | error: {m['error']} | | |")
                continue
            print(f"| {r['page']} | {name} | {m['ms']:.0f} ms | {m['cpu_s']:.2f} s | {m['memory_mb']:.0f} MB |")
    return 0
