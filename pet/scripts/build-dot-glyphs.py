import argparse
import hashlib
import json
import xml.etree.ElementTree as ET
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser()
parser.add_argument("font", type=Path, nargs="?")
parser.add_argument("--output", type=Path)
parser.add_argument("--whale-body", action="store_true")
parser.add_argument("--pixel-body", action="store_true")
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
if args.font is None:
    parser.error("font is required unless --whale-body or --pixel-body is selected")
args.output = args.output or root / "src/core/dot-glyphs.ts"
font_hash = hashlib.sha256(args.font.read_bytes()).hexdigest()
if font_hash != "ca094f6b0001fb048ca39ddd797a0cdb0179e1e55c6561e111c49c3e6a61d7b7":
    raise ValueError("Expected the pinned Noto Sans CJK SC Medium font in dot-glyphs-LICENSE.txt")
messages = {
    "reading": "ToolFamilyRead", "editing": "ToolFamilyPatch",
    "searching": "ToolFamilyFind", "testing": "ToolFamilyVerify",
    "executing": "ToolFamilyRun", "browsing": "PetActivityBrowsing",
    "computer": "PetActivityComputer", "memory": "PetActivityMemory",
    "tool": "ToolFamilyGeneric", "thinking": "ToolFamilyThink",
    "responding": "PetModeReplyTitle", "delegating": "PhaseSubagents",
    "waiting": "PhaseWaitingOnYou", "done": "PhaseDone",
}
languages = ["en", "zh-Hans", "zh-Hant", "ja"]
labels = {language: {key: json.loads((root.parent / f"crates/localization/locales/{language}.json").read_text(encoding="utf-8"))[message]
                     for key, message in messages.items()} for language in languages}
lines = ["import type { OwnerActivityKind } from './pet-engine.js';", "",
         "export type GlyphLanguage = 'en' | 'zh-Hans' | 'zh-Hant' | 'ja';", "",
         "export const DOT_LABELS = " + json.dumps(labels, ensure_ascii=False, indent=2) + " as const;", "",
         "export const DOT_GLYPHS: Record<GlyphLanguage, Record<OwnerActivityKind | 'waiting' | 'done', string[]>> = {"]
sizes = set()
for language, characters in labels.items():
    lines.append(f"  '{language}': {{")
    for key, char in characters.items():
        split = len(char) // 2
        words = char.split() if language == "en" else [char] if len(char) <= 3 else [char[:split], char[split:]]
        if language == "ja" and "を" in char:
            at = char.index("を")
            words = [char[:at], char[at:]]
        elif language == "ja" and "エージェント" in char:
            at = char.index("エージェント")
            end = at + len("エージェント")
            words = [word for word in [char[:at], char[at:end], char[end:]] if word]
        for size in range(48, 0, -1):
            font = ImageFont.truetype(str(args.font), size)
            boxes = [font.getbbox(word) for word in words]
            line_height = max(bottom - top for left, top, right, bottom in boxes)
            height = line_height * len(words) + 2 * (len(words) - 1)
            if max(right - left for left, top, right, bottom in boxes) <= 44 and height <= 44:
                break
        sizes.add(size)
        image = Image.new("L", (48, 48), 0)
        draw = ImageDraw.Draw(image)
        y = (48 - height) // 2
        for word, (left, top, right, bottom) in zip(words, boxes, strict=True):
            draw.text(((48 - right + left) // 2 - left, y - top), word, font=font, fill=255)
            y += line_height + 2
        rows = ["".join("#" if image.getpixel((x, y)) >= 128 else "." for x in range(48)) for y in range(48)]
        lines.append(f"    {key}: [")
        lines.extend(f"      '{row}'," for row in rows)
        lines.append("    ],")
    lines.append("  },")
lines.append("};")
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"Generated {len(languages) * len(messages)} masks at 48 × 48, font sizes {sorted(sizes)}, threshold 128: {args.output}")
