"""Visual parity of pageon's paint with Chromium.

Each static corpus page is screenshotted by Chromium (Playwright's build, 1280×720, scale 1) and
by ``pageon fetch --screenshot``; the two are compared with SSIM on luma (7×7 uniform windows,
the standard constants). Chromium is given pageon's bundled Liberation fonts for the families
pages and form controls use, so the figure measures rendering, not font choice. Results go to
``specs/012-paint/results.md``.
"""

from __future__ import annotations

import io
import os
import subprocess
import tempfile
from pathlib import Path

import numpy as np

from .corpus import BENCH

ROOT = BENCH.parent
FONTS = ROOT / "crates" / "pageon-layout" / "fonts"
PAGES = BENCH / "fixtures" / "pages"
OUT = ROOT / "specs" / "012-paint" / "results.md"
# Pages whose content lives in cross-origin frames served from *.localhost: as files, the
# frames do not load in either engine, so they are compared without their frames.
TARGET = 0.90


def _font_css() -> str:
    faces = []
    for family, file_prefix, aliases in (
        ("Liberation Serif", "LiberationSerif", ["Times New Roman", "Times"]),
        ("Liberation Sans", "LiberationSans", ["Arial", "Helvetica", "system-ui"]),
        ("Liberation Mono", "LiberationMono", ["Courier New", "Courier", "monospace"]),
    ):
        for name in [family, *aliases]:
            for weight, style, suffix in (
                ("normal", "normal", "Regular"),
                ("bold", "normal", "Bold"),
                ("normal", "italic", "Italic"),
                ("bold", "italic", "BoldItalic"),
            ):
                src = (FONTS / f"{file_prefix}-{suffix}.ttf").as_uri()
                faces.append(
                    f'@font-face{{font-family:"{name}";src:url("{src}");font-weight:{weight};font-style:{style}}}'
                )
    # The default family and form controls, as pageon's user-agent sheet sets them.
    faces.append('html{font-family:"Liberation Serif"}')
    faces.append('input,select,button{font-family:"Liberation Sans"}textarea{font-family:"Liberation Mono"}')
    return "".join(faces)


def _luma(png: bytes) -> np.ndarray:
    from PIL import Image

    img = Image.open(io.BytesIO(png)).convert("RGB")
    a = np.asarray(img, dtype=np.float64)
    return 0.299 * a[..., 0] + 0.587 * a[..., 1] + 0.114 * a[..., 2]


def _box(a: np.ndarray, k: int) -> np.ndarray:
    """Mean over k×k windows (valid region), by integral images."""
    s = np.cumsum(np.cumsum(np.pad(a, ((1, 0), (1, 0))), axis=0), axis=1)
    return (s[k:, k:] - s[:-k, k:] - s[k:, :-k] + s[:-k, :-k]) / (k * k)


def ssim(a: np.ndarray, b: np.ndarray, k: int = 7) -> float:
    h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
    a, b = a[:h, :w], b[:h, :w]
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    ma, mb = _box(a, k), _box(b, k)
    va = _box(a * a, k) - ma * ma
    vb = _box(b * b, k) - mb * mb
    cov = _box(a * b, k) - ma * mb
    m = ((2 * ma * mb + c1) * (2 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2))
    return float(m.mean())


def _pageon(binary: str, page: Path) -> bytes:
    with tempfile.TemporaryDirectory() as d:
        out = Path(d) / "p.png"
        subprocess.run(
            [binary, "fetch", str(page), "--no-sandbox", "--screenshot", str(out)],
            check=True,
            capture_output=True,
        )
        return out.read_bytes()


def main(_args) -> int:
    from playwright.sync_api import sync_playwright

    binary = os.environ.get("PAGEON_BIN") or str(ROOT / "target" / "release" / "pageon")
    css = _font_css()
    rows = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1280, "height": 720}, device_scale_factor=1)
        for page_file in sorted(PAGES.glob("*.html")):
            page = ctx.new_page()
            page.goto(page_file.as_uri(), wait_until="load")
            page.add_style_tag(content=css)
            page.evaluate("document.fonts.ready")
            chromium = page.screenshot()
            page.close()
            score = ssim(_luma(chromium), _luma(_pageon(binary, page_file)))
            rows.append((page_file.stem, score))
            print(f"{page_file.stem:20} SSIM {score:.3f}")
        version = browser.version
        browser.close()
    mean = sum(s for _, s in rows) / len(rows)
    lines = [
        "# Visual parity with Chromium",
        "",
        f"Mean SSIM **{mean:.3f}** over {len(rows)} corpus pages (target ≥ {TARGET:.2f}; "
        f"{'met' if mean >= TARGET else 'not met'}).",
        "",
        f"Method: `pageon-bench paint-parity`. Chromium {version} (Playwright) and `pageon fetch "
        "--screenshot`, 1280×720 at scale 1, the page files loaded directly (cross-origin frames "
        "served from `*.localhost` do not load in either). Chromium gets pageon's bundled "
        "Liberation fonts for the default family, the web families and form controls, so the "
        "figure measures rendering rather than font choice. SSIM on luma, 7×7 uniform windows, "
        "standard constants.",
        "",
        "| Page | SSIM |",
        "| --- | --- |",
        *[f"| {n} | {s:.3f}{'' if s >= TARGET else ' ⚠'} |" for n, s in rows],
        "",
    ]
    OUT.write_text("\n".join(lines))
    print(f"mean SSIM {mean:.3f} → {OUT}")
    return 0
