#!/usr/bin/env python3
"""Generate the portable GPUI design artifact. No sibling checkout is needed.
Edit tokens.json only in the codewhale-design repository. Consumers vendor the whole folder.
"""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent

# WCAG 2.x contrast every palette must hold, in both modes, before anything is
# generated: text roles at 4.5:1 and control edges at 3:1 on each ground a
# role can sit on, including a hovered or selected row and the deepest point
# of the ocean field behind the working area, and the added and removed rows
# of a diff, which carry code and their +/− marks.
GROUNDS = ["background", "surface", "sidebar", "hover", "selected", "ocean_depth", "diff_add", "diff_remove"]
TEXT = ["foreground", "muted_foreground", "primary", "accent", "live", "attention", "danger"]
# Secondary text carries labels, metadata and status words people must read at
# small sizes, so it holds 5.5:1 on every ground, above the AA floor.
TEXT_FLOORS = {"muted_foreground": 5.5}
EDGES = ["border_strong"]
FILLED = [("primary_foreground", "primary"), ("primary_foreground", "accent")]
# The ombre's deep stop can carry body text on the dark ground; its rising
# stop is a brand colour only and never a text ground.
OMBRE_TEXT = [("foreground", "deep", 4.5)]

def luminance(hex6):
    channels = [int(hex6[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    channels = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]

def contrast(a, b):
    hi, lo = sorted([luminance(a), luminance(b)], reverse=True)
    return (hi + 0.05) / (lo + 0.05)

def contrast_violations(colors, ombre):
    pairs = [(fg, bg, TEXT_FLOORS.get(fg, 4.5)) for fg in TEXT for bg in GROUNDS]
    pairs += [(edge, bg, 3.0) for edge in EDGES for bg in GROUNDS]
    pairs += [(fg, bg, 4.5) for fg, bg in FILLED]
    found = []
    for mode, palette in colors.items():
        for fg, bg, floor in pairs:
            ratio = contrast(palette[fg], palette[bg])
            if ratio < floor:
                found.append(f"{mode} {fg} on {bg} is {ratio:.2f}:1, needs {floor}:1")
    for fg, stop, floor in OMBRE_TEXT:
        ratio = contrast(colors["dark"][fg], ombre[stop])
        if ratio < floor:
            found.append(f"dark {fg} on ombre {stop} is {ratio:.2f}:1, needs {floor}:1")
    return found

def render():
    source = (ROOT / "tokens.json").read_bytes()
    data = json.loads(source)
    unit = {name: data[name] for name in ["selection_opacity", "primary_hover_opacity"]}
    unit |= {f"glow.{name}": v for name, v in data["glow"].items() if name.endswith("_alpha")}
    unit |= {f"elevation.{name}": v for name, v in data["elevation"].items() if "_alpha" in name}
    for name, value in unit.items():
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 <= value <= 1:
            raise ValueError(f"{name} must be a number between 0 and 1")
    # No overshoot or undershoot: a curve's x and y control points stay in [0, 1].
    for name, curve in data["easing"].items():
        if len(curve) != 4 or not all(isinstance(v, (int, float)) and 0 <= v <= 1 for v in curve):
            raise ValueError(f"easing.{name} must be four control points between 0 and 1")
    stream = data["stream"]
    if not all(isinstance(v, int) and v > 0 for v in stream.values()) or stream["reveal_first_ms"] > stream["reveal_lag_ms"]:
        raise ValueError("stream values must be positive integers, with reveal_first_ms no longer than reveal_lag_ms")
    digest = hashlib.sha256(source).hexdigest()
    header = f"Generated from Codewhale GPUI design {data['version']}; sha256 {digest}. Do not edit."
    keys = list(data["colors"]["dark"])
    assert set(keys) == set(data["colors"]["light"])
    for palette in [*data["colors"].values(), data["ombre"]]:
        assert all(len(v) == 6 and all(c in "0123456789abcdef" for c in v) for v in palette.values())
    violations = contrast_violations(data["colors"], data["ombre"])
    if violations:
        raise ValueError("Contrast below WCAG AA:\n  " + "\n  ".join(violations))
    rust = [f"// {header}", "#![allow(dead_code)]", "", f'pub const VERSION: &str = "{data["version"]}";',
            "#[derive(Clone, Copy, Debug)]", "pub struct Colors {"]
    rust += [f"    pub {k}: u32," for k in keys]
    rust += ["}"]
    for name, palette in data["colors"].items():
        rust += [f"pub const {name.upper()}: Colors = Colors {{"]
        rust += [f"    {k}: 0x{v}," for k, v in palette.items()]
        rust += ["};"]
    rust += ["pub fn colors(dark: bool) -> Colors {", "    if dark { DARK } else { LIGHT }", "}"]
    rust += [f'pub const FONT_FAMILY: &str = "{data["typography"]["family"]}";',
             "pub const FONT_FALLBACKS: &[&str] = &["]
    rust += [f"    {json.dumps(v)}," for v in data["typography"]["fallbacks"]]
    rust += ["];"]
    for section in ["spacing", "radius", "focus", "icons", "motion", "stream", "glow", "elevation"]:
        for name, value in data[section].items():
            rust += [f"pub const {section.upper()}_{name.upper()}: f32 = {float(value)};"]
    for name, curve in data["easing"].items():
        rust += [f"pub const EASING_{name.upper()}: [f32; 4] = [{', '.join(str(float(v)) for v in curve)}];"]
    for name, value in data["ombre"].items():
        rust += [f"pub const OMBRE_{name.upper()}: u32 = 0x{value};"]
    for name in ["selection_opacity", "primary_hover_opacity"]:
        rust += [f"pub const {name.upper()}: f32 = {float(data[name])};"]
    rust += [f'pub const MONO_PX: f32 = {float(data["typography"]["mono_px"])};']
    roles = data["type_roles"]
    for name, role in roles.items():
        if not (isinstance(role["size_px"], (int, float)) and role["line_px"] >= role["size_px"] and role["weight"] in range(100, 1000)):
            raise ValueError(f"type_roles.{name} needs size_px, a line_px no smaller than it, and a weight from 100 to 900")
        for key in ["size_px", "line_px", "weight"]:
            rust += [f"pub const TYPE_ROLE_{name.upper()}_{key.upper()}: f32 = {float(role[key])};"]
    for section in ["size", "breakpoints"]:
        for name, value in data[section].items():
            rust += [f"pub const {section.upper()}_{name.upper()}: f32 = {float(value)};"]
    if data["size"]["touch_target_px"] < 44:
        raise ValueError("size.touch_target_px must be at least 44")
    for name in ["body_px", "caption_px", "prose_px", "heading_px", "title_px"]:
        if name in data["typography"]:
            rust += [f'pub const TYPE_{name.upper()}: f32 = {float(data["typography"][name])};']
    # Existing web names remain aliases, so every current component migrates together.
    aliases = {"outside":"sidebar","surface":"background","panel":"surface","text":"foreground",
               "muted":"muted_foreground","line":"border","accent":"accent","button":"primary",
               "button-text":"primary_foreground","attention":"attention","hover":"hover","selected":"selected",
               "live":"live","danger":"danger","border-strong":"border_strong","ocean-depth":"ocean_depth",
               "diff-add":"diff_add","diff-remove":"diff_remove"}
    css = [f"/* {header} */"]
    for mode, palette in data["colors"].items():
        css += [":root {" if mode == "dark" else '[data-theme="light"] {']
        css += [f"  --{alias}: #{palette[key]};" for alias, key in aliases.items()]
        elevation = data["elevation"]
        css += [f'  --selection: #{palette["accent"]}{round(data["selection_opacity"] * 255):02x};',
                f'  --elevation-popover: 0 {elevation["popover_y_px"]}px {elevation["popover_blur_px"]}px rgb(0 0 0 / {elevation[f"popover_alpha_{mode}"]});',
                f'  --elevation-dialog: 0 {elevation["dialog_y_px"]}px {elevation["dialog_blur_px"]}px rgb(0 0 0 / {elevation[f"dialog_alpha_{mode}"]});',
                f"  color-scheme: {mode};", "}"]
    css += [":root {"]
    for section in ["spacing", "radius", "focus"]:
        css += [f"  --{section}-{name}: {value}px;" for name, value in data[section].items()]
    motion = data["motion"]
    css += [f"  --motion-duration-{name[9:-3]}: {value}ms;" for name, value in motion.items()
            if name.startswith("duration_")]
    css += [f"  --motion-{name[:-3].replace('_', '-')}: {value}ms;" for name, value in motion.items()
            if name.startswith("loop_") and name.endswith("_ms")]
    css += [f'  --motion-arrive-rise: {motion["arrive_rise_px"]}px;']
    css += [f"  --motion-ease-{name}: cubic-bezier({', '.join(str(v) for v in curve)});"
            for name, curve in data["easing"].items()]
    glow = data["glow"]
    css_alias = {"primary": "accent", "live": "live", "attention": "attention"}
    css += [f'  --glow-{name[:-6]}: 0 0 {glow["blur_px"]}px color-mix(in srgb, var(--{css_alias[name[:-6]]}) {round(value * 100)}%, transparent);'
            for name, value in glow.items() if name.endswith("_alpha")]
    css += [f"  --ombre-{name}: #{value};" for name, value in data["ombre"].items()]
    css += [f'  --icon-stroke: {data["icons"]["stroke"]};',
            f'  --font-family: {json.dumps(data["typography"]["family"], ensure_ascii=False)};',
            f'  --font-fallbacks: {", ".join(json.dumps(v, ensure_ascii=False) for v in data["typography"]["fallbacks"])};',
            f'  --font-body-size: {data["typography"]["body_px"]}px;',
            f'  --font-code-size: {data["typography"]["mono_px"]}px;']
    for name in ["caption_px", "prose_px", "heading_px", "title_px"]:
        if name in data["typography"]:
            css += [f'  --font-{name[:-3]}-size: {data["typography"][name]}px;']
    for name, role in data["type_roles"].items():
        css += [f'  --type-{name}-size: {role["size_px"]}px;', f'  --type-{name}-line: {role["line_px"]}px;',
                f'  --type-{name}-weight: {role["weight"]};']
    css += [f'  --size-{name[:-3].replace("_", "-")}: {value}px;' for name, value in data["size"].items()]
    css += [f'  --breakpoint-{name[:-3]}: {value}px;' for name, value in data["breakpoints"].items()]
    css += ["}"]
    return {"tokens.rs":"\n".join(rust) + "\n", "tokens.css":"\n".join(css) + "\n"}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stale = []
    for name, content in render().items():
        path = ROOT / name
        if args.check:
            if not path.exists() or path.read_text() != content:
                stale.append(name)
        else:
            path.write_text(content)
    if stale:
        raise SystemExit("Stale GPUI design artifact: " + ", ".join(stale))
    print("GPUI design artifact verified" if args.check else "GPUI design artifact generated")

if __name__ == "__main__":
    main()
