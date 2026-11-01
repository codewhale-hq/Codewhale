#!/usr/bin/env python3
"""Export the Codewhale palettes to the other Codewhale clients.

The versioned `vendor/codewhale-design/tokens.json` snapshot owns the
website's colors, type roles, spacing, radii, motion, easing, elevation,
breakpoints and focus geometry; `web/app/styles/tokens-roles.css` names the
site's roles over them (web/docs/BRAND_COLOR_MAPPING.md). The portable artifact's
own generator checks its digest before exporting. Update the entire vendored
folder from the app's design package; never edit its JSON here.

`crates/palette/src/rgb.rs` still owns the terminal's WHALE, LIGHT and
SHORELINE presets. They remain available to terminal-specific illustrations;
the public site's GPUI aliases no longer copy the Rust palette's mirrors.

Target: <repo>/web/app/tokens.css. This script writes nothing outside this
repository.

Usage:
    scripts/export-design-tokens.py            # write
    scripts/export-design-tokens.py --check    # exit 1 if any target is stale
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOKENS_RS = REPO / "crates/palette/src/rgb.rs"
SOURCE_LABEL = "crates/palette/src/rgb.rs + vendor/codewhale-design/tokens.json"
GPUI_DIR = REPO / "vendor/codewhale-design"

CONST_RE = re.compile(
    r"^pub const ((?:SHORELINE_LIGHT|SHORELINE|WHALE|LIGHT)_[A-Z0-9_]+)_RGB: \(u8, u8, u8\) = "
    r"(?:\((\d+), (\d+), (\d+)\)|((?:SHORELINE_LIGHT|SHORELINE|WHALE|LIGHT)_[A-Z0-9_]+)_RGB);",
    re.MULTILINE,
)


def parse_tokens(text: str) -> list[tuple[str, tuple[int, int, int] | str]]:
    """Return [(NAME, (r, g, b) | alias-NAME)] in source order."""
    tokens: list[tuple[str, tuple[int, int, int] | str]] = []
    known: set[str] = set()
    for m in CONST_RE.finditer(text):
        name = m.group(1)
        if m.group(5) is not None:
            target = m.group(5)
            if target not in known:
                raise SystemExit(f"{name} aliases unknown token {target}")
            tokens.append((name, target))
        else:
            tokens.append((name, (int(m.group(2)), int(m.group(3)), int(m.group(4)))))
        known.add(name)
    if not tokens:
        raise SystemExit(f"no palette RGB consts found in {TOKENS_RS}")
    return tokens


def css_name(name: str) -> str:
    """Keep the website's existing names while replacing their source."""
    if name.startswith("GPUI_LIGHT_"):
        return "--gpui-light-" + name.removeprefix("GPUI_LIGHT_").lower().replace("_", "-")
    if name.startswith("GPUI_"):
        return "--gpui-dark-" + name.removeprefix("GPUI_").lower().replace("_", "-")
    if name.startswith("SHORELINE_LIGHT_"):
        return "--shoreline-light-" + name.removeprefix("SHORELINE_LIGHT_").lower().replace(
            "_", "-"
        )
    if name.startswith("SHORELINE_"):
        return "--shoreline-" + name.removeprefix("SHORELINE_").lower().replace("_", "-")
    if name.startswith("LIGHT_"):
        return "--light-" + name.removeprefix("LIGHT_").lower().replace("_", "-")
    return "--whale-" + name.removeprefix("WHALE_").lower().replace("_", "-")


def render_css(tokens) -> str:
    lines = [
        f"/* generated from {SOURCE_LABEL} — do not edit */",
        "/* regenerate: scripts/export-design-tokens.py (in the codewhale repo) */",
        ":root {",
    ]
    for name, value in tokens:
        prop = css_name(name)
        if isinstance(value, str):
            ref = css_name(value)
            lines.append(f"  {prop}: var({ref});")
            lines.append(f"  {prop}-rgb: var({ref}-rgb);")
        else:
            r, g, b = value
            lines.append(f"  {prop}: #{r:02x}{g:02x}{b:02x};")
            lines.append(f"  {prop}-rgb: {r} {g} {b};")
    lines.append("}")
    return "\n".join(lines) + "\n"


def gpui_tokens(data):
    aliases = {
        "BG": "background", "TEXT": "foreground", "PANEL": "surface",
        "TEXT_MUTED": "muted_foreground", "BORDER": "border", "SIDEBAR": "sidebar",
        "PRIMARY": "primary", "ON_PRIMARY": "primary_foreground", "ACCENT": "accent",
        "HOVER": "hover", "LIST_ACTIVE": "selected", "ATTENTION": "attention", "LIVE": "live",
        "DANGER": "danger", "BORDER_STRONG": "border_strong", "OCEAN_DEPTH": "ocean_depth",
        "DIFF_ADD": "diff_add", "DIFF_REMOVE": "diff_remove",
    }
    return [
        (f"{prefix}_{name}", tuple(bytes.fromhex(palette[key])))
        for mode, prefix in [("dark", "GPUI"), ("light", "GPUI_LIGHT")]
        for palette in [data["colors"][mode]]
        for name, key in aliases.items()
    ]


def gpui_geometry(data, digest):
    lines = [f"/* GPUI design {data['version']}; sha256 {digest}. */", ":root {"]
    for section in ["radius", "focus", "spacing"]:
        for name, value in data[section].items():
            lines.append(f"  --gpui-{section}-{name}: {value}px;")
    for name in ["selection_opacity", "primary_hover_opacity"]:
        lines.append(f"  --gpui-{name.replace('_', '-')}: {data[name]};")
    for role, spec in data["type_roles"].items():
        lines += [f"  --gpui-type-{role}-size: {spec['size_px'] / 16:g}rem;",
                  f"  --gpui-type-{role}-line: {spec['line_px'] / spec['size_px']:.4g};",
                  f"  --gpui-type-{role}-weight: {spec['weight']};"]
    for name, value in data["motion"].items():
        if name.startswith("duration_") or name.startswith("loop_") and name.endswith("_ms"):
            lines.append(f"  --gpui-motion-{name.removesuffix('_ms').replace('_', '-')}: {value}ms;")
    lines.append(f"  --gpui-motion-arrive-rise: {data['motion']['arrive_rise_px']}px;")
    for name, curve in data["easing"].items():
        lines.append(f"  --gpui-easing-{name}: cubic-bezier({', '.join(f'{v:g}' for v in curve)});")
    e = data["elevation"]
    for kind in ["popover", "dialog"]:
        for mode in ["light", "dark"]:
            lines.append(f"  --gpui-elevation-{kind}-{mode}: 0 {e[kind + '_y_px']}px "
                         f"{e[kind + '_blur_px']}px rgb(0 0 0 / {e[kind + '_alpha_' + mode]:g});")
    for name, value in data["breakpoints"].items():
        lines.append(f"  --gpui-breakpoint-{name.removesuffix('_px')}: {value}px;")
    rise, deep = data["ombre"]["rise"], data["ombre"]["deep"]
    lines.append(f"  --gpui-ombre: linear-gradient(135deg, #{rise}, #{deep});")
    fallbacks = ", ".join(json.dumps(v) for v in data["typography"]["fallbacks"])
    lines += [f"  --gpui-font-fallbacks: {fallbacks};",
              f"  --gpui-mono-size: {data['typography']['mono_px'] / 16:g}rem;", "}"]
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="verify instead of write")
    args = ap.parse_args()

    subprocess.run([sys.executable, str(GPUI_DIR / "generate.py"), "--check"], check=True)
    source = (GPUI_DIR / "tokens.json").read_bytes()
    data = json.loads(source)
    tokens = parse_tokens(TOKENS_RS.read_text(encoding="utf-8")) + gpui_tokens(data)
    css = render_css(tokens) + gpui_geometry(data, hashlib.sha256(source).hexdigest())

    targets: list[tuple[Path, str]] = [(REPO / "web/app/tokens.css", css)]

    stale = []
    for path, content in targets:
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if current == content:
            continue
        if args.check:
            stale.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
            print(f"wrote {path}")

    if stale:
        for path in stale:
            print(f"stale: {path}", file=sys.stderr)
        print(
            "run scripts/export-design-tokens.py to regenerate from "
            f"{SOURCE_LABEL}",
            file=sys.stderr,
        )
        return 1
    if args.check:
        print(f"design tokens up to date ({len(targets)} file(s), {len(tokens)} tokens)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
