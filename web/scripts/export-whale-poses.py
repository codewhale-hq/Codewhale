#!/usr/bin/env python3
"""Export the v2 whale character's still poses as web SVGs.

Source: `handoff/posters.json` from the whale-character-v2 kit (vendored in
codewhale-app under vendor/whale-character-v2/). Each poster is the one-ink,
reduced-motion pose of one of the 17 actions. This keeps only the visible
paths, rounds coordinates to 0.01, and fills everything with the logo
gradient (#1E8FD8 -> #0B48BB), so the files work as plain <img> sources in
both appearances. Eye and throat apertures are holes in the body path; like
the kit's colour version, a light underlay (the pouch and eye shapes, drawn
first) shows through them, so the belly reads light on any ground.

Usage:
    scripts/export-whale-poses.py <posters.json> [out-dir]   # default public/whale
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

GRADIENT = (
    '<defs><linearGradient id="g" gradientUnits="userSpaceOnUse" x1="-50" y1="-50" '
    'x2="50" y2="50"><stop stop-color="#1E8FD8"/><stop offset="1" stop-color="#0B48BB"/>'
    "</linearGradient></defs>"
)
PATH = re.compile(r'<path id="([^"]+)" d="([^"]+)"([^>]*)/>')
NUM = re.compile(r"-?\d+\.\d+")


def visible(attrs: str) -> float:
    m = re.search(r'opacity="([\d.]+)"', attrs)
    return float(m.group(1)) if m else 1.0


def compact(d: str) -> str:
    d = NUM.sub(lambda m: f"{float(m.group(0)):.2f}".rstrip("0").rstrip("."), d)
    return re.sub(r"\s+", " ", d).strip()


LIGHT = "#FFFFFF"
UNDERLAY = re.compile(r"(^|-)(pouch|eye)$")


def export(scene: str) -> str:
    view = re.search(r'viewBox="([^"]+)"', scene).group(1)
    under, paths = [], []
    for pid, d, attrs in PATH.findall(scene):
        if UNDERLAY.search(pid):
            under.append(f'<path d="{compact(d)}"/>')
        opacity = visible(attrs)
        if opacity <= 0.001:
            continue
        op = "" if opacity >= 0.999 else f' opacity="{opacity:.2f}"'
        paths.append(f'<path d="{compact(d)}"{op}/>')
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}">{GRADIENT}'
        f'<g fill="{LIGHT}">{"".join(under)}</g>'
        f'<g fill="url(#g)" fill-rule="evenodd">{"".join(paths)}</g></svg>\n'
    )


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    posters = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(__file__).resolve().parent.parent / "public/whale"
    out.mkdir(parents=True, exist_ok=True)
    for poster in posters:
        (out / f"{poster['state']}.svg").write_text(export(poster["scene"]), encoding="utf-8")
    print(f"wrote {len(posters)} poses to {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
