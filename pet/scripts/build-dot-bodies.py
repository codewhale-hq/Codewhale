import argparse
import hashlib
import xml.etree.ElementTree as ET
from pathlib import Path

from PIL import Image

parser = argparse.ArgumentParser()
parser.add_argument("--output", type=Path)
group = parser.add_mutually_exclusive_group(required=True)
group.add_argument("--whale-body", action="store_true")
group.add_argument("--pixel-body", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
if args.pixel_body:
    source = root / "assets/pixel-whale.svg"
    if hashlib.sha256(source.read_bytes()).hexdigest() != "9d3742fbf325aedc18435d7b15e89178b8684ccbb40cd55a32e60bed156de700":
        raise ValueError("Expected the approved 192-cell Codewhale pixel whale")
    step = 512 / 192
    candidates = []
    for rect in ET.fromstring(source.read_text()):
        color = rect.attrib["fill"].lstrip("#")
        rgb = tuple(int(color[i:i + 2], 16) for i in (0, 2, 4))
        for col in range(max(1, round(float(rect.attrib["width"]) / step))):
            candidates.append((float(rect.attrib["x"]) + (col + .5) * step,
                               float(rect.attrib["y"]) + step / 2, *rgb))
    distances = [float("inf")] * len(candidates)
    selected = []
    at = 0
    for _ in range(980):
        x, y, *color = candidates[at]
        selected.append((x, y, *color))
        distances = [min(d, (x - p[0]) ** 2 + (y - p[1]) ** 2)
                     for d, p in zip(distances, candidates, strict=True)]
        at = max(range(len(distances)), key=distances.__getitem__)
    output = args.output or root / "src/core/dot-pixel-body.ts"
    lines = ["export const DOT_PIXEL_BODY: readonly (readonly [number, number, number, number, number])[] = ["]
    lines.extend(f"  [{(x - 256) / 512 * .878:.6f}, {(y - 256) / 512 * .878:.6f}, {r}, {g}, {b}]," for x, y, r, g, b in selected)
    lines.append("];")
    output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"Generated 980 drones from the approved 192-cell pixel whale: {output}")
    raise SystemExit(0)
if args.whale_body:
    source = root / "public/whale.png"
    if hashlib.sha256(source.read_bytes()).hexdigest() != "dc180fe5bde033f55bed5c7c7c1ab81182f31dd5913df237fb23e2a2da5098df":
        raise ValueError("Expected the reviewed filled Codewhale pet mark")
    mask = Image.open(source).getchannel("A")
    left, top, right, bottom = mask.getbbox()
    candidates = [(x, y) for row, y in enumerate(range(top + 4, bottom, 8))
                  for x in range(left + 4 + (row % 2) * 4, right, 8)
                  if mask.getpixel((x, y)) >= 128]
    distances = [float("inf")] * len(candidates)
    selected = []
    at = 0
    for _ in range(980):
        x, y = candidates[at]
        selected.append((x, y))
        distances = [min(d, (x - px) ** 2 + (y - py) ** 2)
                     for d, (px, py) in zip(distances, candidates, strict=True)]
        at = max(range(len(distances)), key=distances.__getitem__)
    scale = .878 / (right - left)
    cx, cy = (left + right) / 2, (top + bottom) / 2
    output = args.output or root / "src/core/dot-body.ts"
    lines = ["export const DOT_BODY: readonly (readonly [number, number])[] = ["]
    lines.extend(f"  [{(x - cx) * scale:.6f}, {(y - cy) * scale:.6f}]," for x, y in selected)
    lines.append("]; ")
    output.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")
    print(f"Generated 980 interior dots from the filled pet mark: {output}")
    raise SystemExit(0)
