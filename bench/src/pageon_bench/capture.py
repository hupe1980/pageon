"""CDP capture of a live Playwright page.

One capture = everything every observation format needs at one step: DOM snapshots with
layout, paint order and computed styles; the accessibility tree of every frame; the frame
tree with origins; the viewport. Cross-origin (out-of-process) iframes are captured through
their own CDP session.
"""

from __future__ import annotations

import datetime as _dt
import gzip
import json
from pathlib import Path
from urllib.parse import urlsplit

from . import __version__

STYLES = ["display", "visibility", "opacity", "pointer-events", "cursor", "position", "z-index", "overflow"]
SCHEMA = 1


def _origin(url: str) -> str:
    u = urlsplit(url)
    if not u.scheme or u.scheme in ("about", "data", "blob", "javascript"):
        return "null"
    port = f":{u.port}" if u.port else ""
    return f"{u.scheme}://{u.hostname}{port}"


def _snapshot(cdp) -> dict:
    return cdp.send(
        "DOMSnapshot.captureSnapshot",
        {"computedStyles": STYLES, "includePaintOrder": True, "includeDOMRects": True},
    )


def _ax(cdp, frame_id: str | None) -> list[dict]:
    params = {"frameId": frame_id} if frame_id else {}
    try:
        return cdp.send("Accessibility.getFullAXTree", params).get("nodes", [])
    except Exception:  # frame detached or not yet committed
        return []


def _frame_origin(frame: dict, parent_origin: str | None) -> str:
    """Origin of a frame; srcdoc/about:blank frames inherit their parent's origin."""
    url = frame.get("unreachableUrl") or frame.get("url", "")
    if url.startswith(("about:", "data:")) and parent_origin:
        return parent_origin
    o = frame.get("securityOrigin") or ""
    if not o or o in ("null", "://") or o.startswith("chrome-error"):
        o = _origin(url)
    return o if o not in ("null", "") else (parent_origin or "null")


def _frame_state(frame: dict, in_doc: bool) -> str:
    if frame.get("unreachableUrl") or frame.get("url", "").startswith("chrome-error:"):
        return "failed"
    return "loaded" if in_doc else "loading"


def _walk(tree: dict, parent: str | None = None):
    frame = tree["frame"]
    yield frame, parent
    for child in tree.get("childFrames", []) or []:
        yield from _walk(child, frame["id"])


def capture_page(
    page, *, step: int = 0, nav_counter: int = 0, baselines: dict | None = None, extras: dict | None = None
) -> dict:
    """Capture `page` (a Playwright sync Page) and return the capture dict."""
    context = page.context
    cdp = context.new_cdp_session(page)
    try:
        cdp.send("DOM.enable")
        snapshots = [_snapshot(cdp)]
        tree = cdp.send("Page.getFrameTree")["frameTree"]
        metrics = cdp.send("Page.getLayoutMetrics")
        version = cdp.send("Browser.getVersion").get("product", "")
        doc_index = {snapshots[0]["strings"][d["frameId"]]: i for i, d in enumerate(snapshots[0]["documents"])}

        frames: list[dict] = []
        seen: set[str] = set()
        origins: dict[str, str] = {}
        for frame, parent in _walk(tree):
            fid = frame["id"]
            seen.add(fid)
            owner = None
            if parent is not None:
                try:
                    owner = cdp.send("DOM.getFrameOwner", {"frameId": fid}).get("backendNodeId")
                except Exception:
                    owner = None
            in_doc = fid in doc_index
            frames.append(
                {
                    "frame_id": fid,
                    "url": frame.get("unreachableUrl") or frame.get("url", ""),
                    "parent_frame_id": parent,
                    "owner_backend_node_id": owner,
                    "origin": origins.setdefault(fid, _frame_origin(frame, origins.get(parent or ""))),
                    "state": _frame_state(frame, in_doc),
                    "document": {"snapshot": 0, "document": doc_index[fid]} if in_doc else None,
                    "ax_nodes": _ax(cdp, fid) if in_doc else [],
                }
            )

        # Out-of-process iframes: each has its own target and session.
        for pw_frame in page.frames[1:]:
            try:
                sub = context.new_cdp_session(pw_frame)
            except Exception:
                continue  # same-process frame, already captured
            try:
                sub_tree = sub.send("Page.getFrameTree")["frameTree"]
                fid = sub_tree["frame"]["id"]
                if fid in seen:
                    continue
                snap = _snapshot(sub)
                snapshots.append(snap)
                sidx = len(snapshots) - 1
                sub_docs = {snap["strings"][d["frameId"]]: i for i, d in enumerate(snap["documents"])}
                parent = None
                owner = None
                parent_frame = pw_frame.parent_frame
                for f, _ in _walk(tree):
                    if parent_frame is not None and f.get("url") == parent_frame.url:
                        parent = f["id"]
                try:
                    owner = cdp.send("DOM.getFrameOwner", {"frameId": fid}).get("backendNodeId")
                except Exception:
                    owner = None
                for f, p in _walk(sub_tree, parent):
                    sfid = f["id"]
                    seen.add(sfid)
                    in_doc = sfid in sub_docs
                    frames.append(
                        {
                            "frame_id": sfid,
                            "url": f.get("url", ""),
                            "parent_frame_id": p,
                            "owner_backend_node_id": owner if sfid == fid else None,
                            "origin": _frame_origin(f, origins.get(p or "")),
                            "state": _frame_state(f, in_doc),
                            "document": {"snapshot": sidx, "document": sub_docs[sfid]} if in_doc else None,
                            "ax_nodes": _ax(sub, sfid) if in_doc else [],
                        }
                    )
            finally:
                sub.detach()
    finally:
        cdp.detach()

    vv = metrics.get("cssVisualViewport") or metrics.get("visualViewport") or {}
    try:
        ready = page.evaluate("document.readyState")
    except Exception:
        ready = "complete"
    return {
        "schema": SCHEMA,
        "url": page.url,
        "title": page.title(),
        "load_state": ready if ready in ("loading", "interactive", "complete") else "complete",
        "viewport": {
            "x": vv.get("pageX", 0),
            "y": vv.get("pageY", 0),
            "width": vv.get("clientWidth", 0),
            "height": vv.get("clientHeight", 0),
        },
        "snapshots": snapshots,
        "frames": frames,
        "baselines": dict(baselines or {}),
        "extras": dict(extras or {}),
        "meta": {
            "captured_at": _dt.datetime.now(_dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "tool_version": __version__,
            "chromium_version": version,
            "step": step,
            "nav_counter": nav_counter,
        },
    }


def write_capture(capture: dict, path: str | Path) -> Path:
    """Write a capture as gzip JSON with sorted keys (stable bytes)."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    data = json.dumps(capture, sort_keys=True, separators=(",", ":")).encode()
    with gzip.GzipFile(filename="", mode="wb", fileobj=path.open("wb"), mtime=0) as f:
        f.write(data)
    return path


def read_capture(path: str | Path) -> dict:
    """Read a capture file into a dict."""
    with gzip.open(path, "rb") as f:
        return json.loads(f.read())
