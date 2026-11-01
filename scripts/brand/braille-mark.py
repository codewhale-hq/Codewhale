#!/usr/bin/env python3
"""Derive the TUI launch-mark assets from the founder raster (PRD section 6).

The launch mark in `crates/tui-kit/src/mark.rs` is generated here, never
hand-drawn. The canonical product mark is the founder-supplied raster
`brand/codewhalemarkfinal.png` (1254 x 1254 brand sheet: navy hero whale on
white, sizing row, the white-on-navy app icon, colour/mono/reversed rows).
Both TUI tiers are proportional/braille derivatives of that file — no
redraws, no traced SVG:

- braille rows <- the hero whale (navy on white, top of the sheet),
  navy darkness box-filtered to a dot grid, aspect preserved and centred
  in the rung's box, all-blank edge columns trimmed, the eye carved as one
  cleared dot;
- kitty/sixel PNGs <- the app-icon panel (white whale on the navy rounded
  square), auto-located as the largest navy blob in the sheet's right
  middle band, squared, sheet-white keyed to transparent, proportionally
  resized.

    scripts/brand/braille-mark.py                 # print + Rust consts
    scripts/brand/braille-mark.py --png crates/tui/assets/mark-96.png --px 96

Requires `pillow` (`pip install pillow`). No other dependencies.
"""

from __future__ import annotations

import argparse
import collections
import pathlib
import sys

try:
    from PIL import Image
except ImportError:
    raise SystemExit("braille-mark.py requires pillow (`pip install pillow`)")

ROOT = pathlib.Path(__file__).resolve().parents[2]
RASTER = ROOT / "brand" / "codewhalemarkfinal.png"

# Search bands as fractions of the sheet, so the boxes track the layout
# rather than absolute pixels. The app-icon caption ("APP ICON", navy text)
# sits above the icon band; the band starts below it.
HERO_BAND = (0.0, 1.0, 0.0, 0.52)  # x0, x1, y0, y1
ICON_BAND = (0.65, 1.0, 0.55, 0.78)
HERO_MARGIN = 12
ICON_PAD = 10
# Sheet background (and the icon's drop shadow, darkest ~211) keys out;
# founder navy (~15,33,65) never approaches this.
BG_CUTOFF = 200
# Braille dot bit for (dot_row, dot_col) inside one cell — U+2800 layout:
# dots 1,2,3 are column 0 rows 0..2 (bits 0..2), dots 4,5,6 column 1 rows
# 0..2 (bits 3..5), dots 7,8 are row 3 (bits 6,7).
DOT_BITS = {
    (0, 0): 0x01,
    (1, 0): 0x02,
    (2, 0): 0x04,
    (0, 1): 0x08,
    (1, 1): 0x10,
    (2, 1): 0x20,
    (3, 0): 0x40,
    (3, 1): 0x80,
}


def is_navy(pixel: tuple[int, int, int]) -> bool:
    r, g, b = pixel
    return b > 60 and b > r + 25 and r < 110 and g < 150


def load_sheet() -> Image.Image:
    if not RASTER.exists():
        raise SystemExit(f"founder raster missing: {RASTER}")
    image = Image.open(RASTER).convert("RGB")
    width, height = image.size
    if width != height or width < 800:
        raise SystemExit(f"unexpected founder sheet geometry: {image.size}")
    return image


def band_box(image: Image.Image, band: tuple[float, float, float, float]):
    width, height = image.size
    return (
        int(band[0] * width),
        int(band[1] * width),
        int(band[2] * height),
        int(band[3] * height),
    )


def hero_coverage(image: Image.Image) -> tuple[int, int, list[list[float]]]:
    """Navy-darkness coverage of the hero whale crop, each in 0..1."""
    width, height = image.size
    x0, x1, y0, _ = band_box(image, HERO_BAND)
    pixels = image.load()
    xs, ys = [], []
    for y in range(y0, int(HERO_BAND[3] * height)):
        for x in range(x0, x1):
            if is_navy(pixels[x, y]):
                xs.append(x)
                ys.append(y)
    if not xs:
        raise SystemExit("no navy hero whale found in the founder sheet")
    box = (
        max(0, min(xs) - HERO_MARGIN),
        max(0, min(ys) - HERO_MARGIN),
        min(width, max(xs) + HERO_MARGIN + 1),
        min(height, max(ys) + HERO_MARGIN + 1),
    )
    crop = image.crop(box)
    cover = crop.load()
    cw, ch = crop.size
    coverage = []
    for y in range(ch):
        row = []
        for x in range(cw):
            r, g, b = cover[x, y]
            row.append(max(0.0, min(1.0, (180.0 - (r + g + b) / 3.0) / 120.0)))
        coverage.append(row)
    print(f"// hero whale box {box[0]},{box[1]}-{box[2]},{box[3]}", file=sys.stderr)
    return cw, ch, coverage


def icon_square(image: Image.Image) -> Image.Image:
    """The app-icon panel squared: white whale on the navy rounded square
    with the sheet background keyed to transparent. Located as the largest
    navy blob in the icon band, so the caption text (separate small blobs)
    can never be mistaken for the mark."""
    width, height = image.size
    x0, x1, y0, y1 = band_box(image, ICON_BAND)
    pixels = image.load()
    seen = bytearray(width * height)
    best: list[tuple[int, int]] = []
    for sy in range(y0, y1):
        for sx in range(x0, x1):
            if not is_navy(pixels[sx, sy]) or seen[sy * width + sx]:
                continue
            blob, stack = [], collections.deque([(sx, sy)])
            seen[sy * width + sx] = 1
            while stack:
                x, y = stack.pop()
                blob.append((x, y))
                for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                    if (
                        x0 <= nx < x1
                        and y0 <= ny < y1
                        and not seen[ny * width + nx]
                        and is_navy(pixels[nx, ny])
                    ):
                        seen[ny * width + nx] = 1
                        stack.append((nx, ny))
            if len(blob) > len(best):
                best = blob
    if len(best) < 10_000:
        raise SystemExit("app-icon blob not found in the founder sheet")
    bx0 = min(x for x, _ in best)
    bx1 = max(x for x, _ in best)
    by0 = min(y for _, y in best)
    by1 = max(y for _, y in best)
    # The white whale cuts the blob's left side, but its full height shows:
    # the icon is square, so the edge is the height.
    edge = by1 - by0 + 1
    if not 150 <= edge <= 260:
        raise SystemExit(f"app-icon blob has unexpected height: {edge}")
    cx = (bx0 + bx1) // 2
    cy = (by0 + by1) // 2
    half = edge // 2 + ICON_PAD
    box = (cx - half, cy - half, cx + half, cy + half)
    print(
        f"// app-icon navy blob x {bx0}-{bx1} y {by0}-{by1}, "
        f"square crop {box[0]},{box[1]}-{box[2]},{box[3]}",
        file=sys.stderr,
    )
    square = image.crop(box).convert("RGBA")
    sw, sh = square.size
    ink = square.load()
    flood = bytearray(sw * sh)

    def is_bg(x: int, y: int) -> bool:
        r, g, b = ink[x, y][:3]
        return min(r, g, b) > BG_CUTOFF

    queue = collections.deque()
    for x in range(sw):
        for y in (0, sh - 1):
            if is_bg(x, y):
                queue.append((x, y))
                flood[y * sw + x] = 1
    for y in range(sh):
        for x in (0, sw - 1):
            if is_bg(x, y) and not flood[y * sw + x]:
                queue.append((x, y))
                flood[y * sw + x] = 1
    while queue:
        x, y = queue.popleft()
        r, g, b, _ = ink[x, y]
        ink[x, y] = (r, g, b, 0)
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < sw and 0 <= ny < sh and not flood[ny * sw + nx] and is_bg(nx, ny):
                flood[ny * sw + nx] = 1
                queue.append((nx, ny))
    return square


def downsample(
    coverage: list[list[float]], width: int, height: int, dots_w: int, dots_h: int
) -> list[list[float]]:
    """Box-filter coverage into a dots_w x dots_h grid, aspect preserved and
    centred. Cells outside the glyph read 0."""
    scale = min(dots_w / width, dots_h / height)
    glyph_w = max(1, round(width * scale))
    glyph_h = max(1, round(height * scale))
    off_x = (dots_w - glyph_w) // 2
    off_y = (dots_h - glyph_h) // 2
    grid = [[0.0] * dots_w for _ in range(dots_h)]
    for gy in range(glyph_h):
        y0 = int(gy * height / glyph_h)
        y1 = max(y0 + 1, int((gy + 1) * height / glyph_h))
        for gx in range(glyph_w):
            x0 = int(gx * width / glyph_w)
            x1 = max(x0 + 1, int((gx + 1) * width / glyph_w))
            total = 0.0
            for y in range(y0, min(y1, height)):
                row = coverage[y]
                for x in range(x0, min(x1, width)):
                    total += row[x]
            grid[off_y + gy][off_x + gx] = total / ((y1 - y0) * (x1 - x0))
    return grid


def eye_hole(coverage: list[list[float]], width: int, height: int) -> tuple[float, float] | None:
    """Locate the eye: the smallest enclosed hole in the glyph above the
    raster-speck noise floor (the belly white is the other, far larger,
    hole). Returns its centroid as a fraction of the glyph's width and
    height, or None when nothing is enclosed. Works on a coarse copy so
    the flood fill stays cheap."""
    scale = max(1, width // 220)
    cw, ch = width // scale, height // scale
    solid = [
        [coverage[y * scale][x * scale] >= 0.5 for x in range(cw)] for y in range(ch)
    ]
    seen = [[False] * cw for _ in range(ch)]
    holes = []
    for sy in range(ch):
        for sx in range(cw):
            if solid[sy][sx] or seen[sy][sx]:
                continue
            stack, cells, touches_edge = [(sx, sy)], [], False
            seen[sy][sx] = True
            while stack:
                x, y = stack.pop()
                cells.append((x, y))
                if x in (0, cw - 1) or y in (0, ch - 1):
                    touches_edge = True
                for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                    if 0 <= nx < cw and 0 <= ny < ch and not solid[ny][nx] and not seen[ny][nx]:
                        seen[ny][nx] = True
                        stack.append((nx, ny))
            if not touches_edge and len(cells) >= 4:
                holes.append(cells)
    if not holes:
        return None
    eye = min(holes, key=len)
    cx = sum(x for x, _ in eye) / len(eye) + 0.5
    cy = sum(y for _, y in eye) / len(eye) + 0.5
    return cx / cw, cy / ch


def carve_eye(
    grid: list[list[float]], width: int, height: int, dots_w: int, dots_h: int, eye: tuple[float, float]
) -> None:
    """Clear the one dot under the eye's centroid so the eye survives rungs
    where it is smaller than a dot. Same geometry as `downsample`."""
    scale = min(dots_w / width, dots_h / height)
    glyph_w = max(1, round(width * scale))
    glyph_h = max(1, round(height * scale))
    off_x = (dots_w - glyph_w) // 2
    off_y = (dots_h - glyph_h) // 2
    x = min(dots_w - 1, off_x + int(eye[0] * glyph_w))
    y = min(dots_h - 1, off_y + int(eye[1] * glyph_h))
    grid[y][x] = 0.0


def to_braille(grid: list[list[float]], cols: int, rows: int, threshold: float) -> list[str]:
    lines = []
    for cy in range(rows):
        line = []
        for cx in range(cols):
            bits = 0
            for (dy, dx), bit in DOT_BITS.items():
                if grid[cy * 4 + dy][cx * 2 + dx] >= threshold:
                    bits |= bit
            line.append(chr(0x2800 + bits) if bits else " ")
        lines.append("".join(line))
    return lines


def trim_columns(lines: list[str]) -> list[str]:
    width = max(len(line) for line in lines)
    padded = [line.ljust(width) for line in lines]
    blank = [all(line[x] == " " for line in padded) for x in range(width)]
    first = next((x for x in range(width) if not blank[x]), 0)
    last = next((x for x in range(width - 1, -1, -1) if not blank[x]), width - 1)
    return [line[first : last + 1] for line in padded]


def rust_const(name: str, lines: list[str]) -> str:
    body = "\n".join(f'    "{line}",' for line in lines)
    return f"const {name}: [&str; {len(lines)}] = [\n{body}\n];"


def write_png(out: pathlib.Path, px: int) -> None:
    square = icon_square(load_sheet())
    square.resize((px, px), Image.LANCZOS).save(out)
    print(f"wrote {out} ({px}x{px}, founder app-icon derivative)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument(
        "--rung",
        action="append",
        default=None,
        metavar="NAME:COLSxROWS",
        help="cell box to render, e.g. SMALL:11x3 (default: SMALL:11x3 TINY:8x2)",
    )
    parser.add_argument("--threshold", type=float, default=0.3, help="dot coverage threshold")
    parser.add_argument("--no-eye", action="store_true", help="do not carve the eye dot")
    parser.add_argument("--png", type=pathlib.Path, help="write an app-icon PNG instead")
    parser.add_argument("--px", type=int, default=96, help="PNG edge in pixels")
    args = parser.parse_args()

    if args.png:
        write_png(args.png, args.px)
        return 0

    rungs = args.rung or ["SMALL:11x3", "TINY:8x2"]
    width, height, coverage = hero_coverage(load_sheet())
    eye = None if args.no_eye else eye_hole(coverage, width, height)
    print("// generated by scripts/brand/braille-mark.py from brand/codewhalemarkfinal.png")
    print(
        f"// (founder hero whale {width}x{height}px, "
        f"threshold {args.threshold}, aspect preserved, edge columns trimmed, "
        f"eye {'carved' if eye else 'not found'})"
    )
    for spec in rungs:
        name, box = spec.split(":")
        cols, rows = (int(v) for v in box.lower().split("x"))
        grid = downsample(coverage, width, height, cols * 2, rows * 4)
        if eye is not None:
            carve_eye(grid, width, height, cols * 2, rows * 4, eye)
        lines = trim_columns(to_braille(grid, cols, rows, args.threshold))
        print(f"\n// {name}: box {cols}x{rows} -> ink {len(lines[0])}x{len(lines)}")
        print(rust_const(f"{name}_ROWS", lines))
        print("//", file=sys.stderr)
        for line in lines:
            print(f"//  |{line}|", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
