#!/usr/bin/env python3
"""Generate the portable Codewhale design artifact. No sibling checkout is needed.
Edit tokens.json only in the codewhale-design repository. Consumers vendor the whole folder.
"""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
HERE = ROOT

# WCAG 2.x contrast every palette must hold, in both modes, before anything is
# generated: text roles at 4.5:1 and control edges at 3:1 on each ground a
# role can sit on, including a hovered or selected row and the deepest point
# of the ocean field behind the working area, and the added and removed rows
# of a diff, which carry code and their +/− marks.
GROUNDS = ["background", "surface", "sidebar", "hover", "selected", "ocean_depth", "diff_add", "diff_remove"]
# `primary` is a fill only; its text is `primary_foreground`, checked in FILLED.
TEXT = ["foreground", "muted_foreground", "accent", "live", "attention", "danger"]
# Secondary text carries labels, metadata and status words people must read at
# small sizes, so it holds 5.5:1 on every ground, above the AA floor.
TEXT_FLOORS = {"muted_foreground": 5.5}
EDGES = ["border_strong"]
# Disabled labels and icons stay readable while reading as off: a solid ink
# at the 3:1 floor of a user-interface component, never an opacity.
QUIET = ["disabled"]
FILLED = [("primary_foreground", "primary")]
# The ombre's deep stop can carry body text on the dark ground; both stops
# carry `primary_foreground` on the primary action in either mode.
OMBRE_TEXT = [("foreground", "deep", 4.5)]
LEVELS = ["clear", "frosted", "solid"]
HEX = "0123456789abcdef"

def color(value, name):
    """A token colour is a solid hex string or {"hex", "alpha"}; returns (hex, alpha)."""
    hex6, alpha = (value, 1) if isinstance(value, str) else (value.get("hex"), value.get("alpha"))
    if not (isinstance(hex6, str) and len(hex6) == 6 and all(c in HEX for c in hex6)):
        raise ValueError(f"{name} must be six lowercase hex digits")
    if isinstance(alpha, bool) or not isinstance(alpha, (int, float)) or not 0 <= alpha <= 1:
        raise ValueError(f"{name} alpha must be a number between 0 and 1")
    return hex6, alpha

def over(value, ground, name="colour"):
    hex6, alpha = color(value, name)
    top = [int(hex6[i:i + 2], 16) for i in (0, 2, 4)]
    under = [int(ground[i:i + 2], 16) for i in (0, 2, 4)]
    return "".join(f"{round(alpha * t + (1 - alpha) * u):02x}" for t, u in zip(top, under))

def css_color(value, name):
    hex6, alpha = color(value, name)
    if alpha == 1:
        return f"#{hex6}"
    r, g, b = (int(hex6[i:i + 2], 16) for i in (0, 2, 4))
    return f"rgb({r} {g} {b} / {alpha})"

def luminance(hex6):
    channels = [int(hex6[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    channels = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]

def contrast(a, b):
    hi, lo = sorted([luminance(a), luminance(b)], reverse=True)
    return (hi + 0.05) / (lo + 0.05)

def composited_grounds(data, mode):
    """Every glass and sheet fill over each water stop and the surface-light peak,
    and every card fill over the sheet and each Glass level: the real grounds text sits on."""
    water = {f"water {s['at']}%": color(s["color"], f"water.{mode}")[0] for s in data["water"][mode]}
    first = data["water"][mode][0]["color"]
    water["surface-light peak"] = over(data["surface_light"][mode]["stops"][0]["color"], first, f"surface_light.{mode}")
    grounds = {}
    for under_name, under in water.items():
        for level in LEVELS:
            grounds[f"{level} glass over {under_name}"] = over(data["glass"][mode][level]["fill"], under, f"glass.{mode}.{level}")
        grounds[f"sheet over {under_name}"] = over(data["sheet"][mode]["fill"], under, f"sheet.{mode}")
    for name, ground in list(grounds.items()):
        grounds[f"card on {name}"] = over(data["card"][mode]["fill"], ground, f"card.{mode}")
    return grounds

def contrast_violations(data):
    colors, ombre = data["colors"], data["ombre"]
    pairs = [(fg, bg, TEXT_FLOORS.get(fg, 4.5)) for fg in TEXT for bg in GROUNDS]
    pairs += [(edge, bg, 3.0) for edge in EDGES for bg in GROUNDS]
    pairs += [(quiet, bg, 3.0) for quiet in QUIET for bg in GROUNDS]
    pairs += [(fg, bg, 4.5) for fg, bg in FILLED]
    found = []
    for mode, palette in colors.items():
        for fg, bg, floor in pairs:
            ratio = contrast(palette[fg], palette[bg])
            if ratio < floor:
                found.append(f"{mode} {fg} on {bg} is {ratio:.2f}:1, needs {floor}:1")
        for stop, value in ombre.items():
            ratio = contrast(palette["primary_foreground"], value)
            if ratio < 4.5:
                found.append(f"{mode} primary_foreground on ombre {stop} is {ratio:.2f}:1, needs 4.5:1")
        for ground_name, ground in composited_grounds(data, mode).items():
            for fg in TEXT + QUIET:
                floor = 3.0 if fg in QUIET else TEXT_FLOORS.get(fg, 4.5)
                ratio = contrast(palette[fg], ground)
                if ratio < floor:
                    found.append(f"{mode} {fg} on {ground_name} is {ratio:.2f}:1, needs {floor}:1")
    for fg, stop, floor in OMBRE_TEXT:
        ratio = contrast(colors["dark"][fg], ombre[stop])
        if ratio < floor:
            found.append(f"dark {fg} on ombre {stop} is {ratio:.2f}:1, needs {floor}:1")
    return found

def check_solid_roles(data):
    """colors.* keep solid hex: each translucent v5 value is stored as its result over `background`."""
    for mode, palette in data["colors"].items():
        ground = palette["background"]
        if ground != data["water"][mode][-1]["color"]:
            raise ValueError(f"colors.{mode}.background must be the bottom water stop")
        for role, value in [("surface", data["sheet"][mode]["fill"]), ("sidebar", data["glass"][mode]["frosted"]["fill"])]:
            if palette[role] != over(value, ground):
                raise ValueError(f"colors.{mode}.{role} must be {over(value, ground)}, its fill over background")

FILL_, EDGE_ = ["FRAME_FILL", "SHAPE_FILL"], ["STROKE_COLOR"]
TINT_SOURCES = {"accent_fill": "var(--accent)", "accent_border": "var(--accent)", "count_fill": "var(--text)",
                "danger_border": "var(--danger)", "scrim": "var(--surface)", "on_primary_border": "currentColor"}
TINT_FIGMA = {"accent_fill": ("effort/fill", "accent", FILL_), "accent_border": ("chip/accent-border", "accent", EDGE_),
              "count_fill": ("count/fill", "foreground", FILL_), "danger_border": ("tint/danger-border", "danger", EDGE_),
              "scrim": ("tint/scrim", "background", FILL_), "on_primary_border": ("kbd/on-primary-border", "primary_foreground", EDGE_)}
WEIGHT_STYLE = {400: "Regular", 500: "Medium", 550: "SemiBold", 600: "SemiBold", 700: "Bold"}
FILL, INK, EDGE = ["FRAME_FILL", "SHAPE_FILL"], ["SHAPE_FILL", "TEXT_FILL", "STROKE_COLOR"], ["STROKE_COLOR"]
FIGMA_SCOPES = {"text": INK, "muted": INK, "disabled": INK, "accent": FILL + ["TEXT_FILL", "STROKE_COLOR"], "button-text": INK,
                "live": INK, "attention": INK, "danger": INK, "line": FILL + EDGE, "border-strong": EDGE}

def figma_payload(data, aliases):
    modes = list(data["colors"])
    def both(get):
        return {mode.capitalize(): dict(zip(["hex", "alpha"], color(get(mode), "figma"))) for mode in modes}
    colors = [{"name": alias, "type": "COLOR", "scopes": FIGMA_SCOPES.get(alias, FILL), "web": f"var(--{alias})",
               "values": both(lambda mode, key=key: data["colors"][mode][key])} for alias, key in aliases.items()]
    for group in ["card", "sheet"]:
        for part in ["fill", "border"]:
            colors.append({"name": f"{group}/{part}", "type": "COLOR", "scopes": FILL if part == "fill" else EDGE,
                           "web": f"var(--{group}-{part})", "values": both(lambda mode, g=group, p=part: data[g][mode][p])})
    for level in LEVELS:
        colors.append({"name": f"glass/{level}-fill", "type": "COLOR", "scopes": FILL, "web": f"var(--glass-{level}-fill)",
                       "values": both(lambda mode, l=level: data["glass"][mode][l]["fill"])})
    for part in ["border", "highlight"]:
        colors.append({"name": f"glass/{part}", "type": "COLOR", "scopes": EDGE, "web": f"var(--glass-{part})",
                       "values": both(lambda mode, p=part: data["glass"][mode][p])})
    for name, value in data["ombre"].items():
        colors.append({"name": f"ombre/{name}", "type": "COLOR", "scopes": FILL, "web": f"var(--ombre-{name})",
                       "values": both(lambda mode, v=value: v)})
    for index, stop in enumerate(["top", "upper", "lower", "bottom"]):
        if all(len(data["water"][mode]) == 4 for mode in modes):
            colors.append({"name": f"water/{stop}", "type": "COLOR", "scopes": ["FRAME_FILL"], "web": f"var(--water) · {stop} stop",
                           "values": both(lambda mode, i=index: data["water"][mode][i]["color"])})
    for index, stop in enumerate(["peak", "mid", "edge"]):
        if all(len(data["surface_light"][mode]["stops"]) == 3 for mode in modes):
            colors.append({"name": f"surface-light/{stop}", "type": "COLOR", "scopes": ["FRAME_FILL"], "web": f"var(--surface-light) · {stop} stop",
                           "values": both(lambda mode, i=index: data["surface_light"][mode]["stops"][i]["color"])})
    for layer in ["popover", "dialog"]:
        colors.append({"name": f"shadow/{layer}", "type": "COLOR", "scopes": ["EFFECT_COLOR"], "web": f"var(--elevation-{layer})",
                       "values": both(lambda mode, l=layer: {"hex": "000000", "alpha": data["elevation"][f"{l}_alpha_{mode}"]})})
    for name, (figma_name, role, scopes) in TINT_FIGMA.items():
        web = f"var(--tint-{name.replace('_', '-')})"
        colors.append({"name": figma_name, "type": "COLOR", "scopes": scopes, "web": web,
                       "values": both(lambda mode, r=role, n=name: {"hex": data["colors"][mode][r], "alpha": data["tint"][n]})})
    whale = json.loads((HERE / "whale" / "source" / "spec.json").read_text())["palette"]
    for figma_name, pick in [("whale/rise", lambda p: p["body"][0]), ("whale/deep", lambda p: p["body"][1]),
                             ("whale/pouch", lambda p: p["throat"]), ("whale/eye", lambda p: p["eye"])]:
        colors.append({"name": figma_name, "type": "COLOR", "scopes": FILL, "web": "whale/source/spec.json palette",
                       "values": both(lambda mode, f=pick: f(whale[mode])[1:])})
    metric = lambda name, value, scopes, web: {"name": name, "type": "FLOAT", "scopes": scopes, "web": web, "values": {"Value": value}}
    metrics = [metric(f"spacing/{name}", value, ["GAP"], f"var(--spacing-{name})") for name, value in data["spacing"].items()]
    metrics += [metric(f"radius/{name}", value, ["CORNER_RADIUS"], f"var(--radius-{name})") for name, value in data["radius"].items()]
    metrics += [metric(f"size/{name[:-3].replace('_', '-')}", value, ["WIDTH_HEIGHT"], f"var(--size-{name[:-3].replace('_', '-')})")
                for name, value in data["size"].items()]
    metrics += [metric(f"focus/{name}", value, ["STROKE_FLOAT"] if name == "width" else ["GAP"], f"var(--focus-{name})")
                for name, value in data["focus"].items()]
    styles = []
    for name, role in data["type_roles"].items():
        if role["weight"] not in WEIGHT_STYLE:
            raise ValueError(f"type_roles.{name} weight {role['weight']} has no Figma font style")
        styles.append({"name": f"CW 5/{name.capitalize()}", "family": "Geist Mono" if name == "code" else "Geist",
                       "style": WEIGHT_STYLE[role["weight"]], "size": role["size_px"], "line": role["line_px"]})
    return {"version": data["version"], "textStyles": styles, "collections": [
        {"name": "CW 5 · Color", "modes": [mode.capitalize() for mode in modes], "variables": colors},
        {"name": "CW 5 · Metrics", "modes": ["Value"], "variables": metrics}]}

def dtcg(data):
    def hexa(value):
        hex6, alpha = color(value, "dtcg")
        return f"#{hex6}" if alpha == 1 else f"#{hex6}{round(alpha * 255):02x}"
    px = lambda group: {name: {"$value": f"{value}px"} for name, value in data[group].items()}
    out = {"$description": f"Codewhale design {data['version']}. Generated from tokens.json; do not edit.",
           "color": {mode: {"$type": "color", **{role: {"$value": f"#{value}"} for role, value in palette.items()}}
                     for mode, palette in data["colors"].items()},
           "ombre": {"$type": "color", **{name: {"$value": f"#{value}"} for name, value in data["ombre"].items()}},
           "spacing": {"$type": "dimension", **px("spacing")}, "radius": {"$type": "dimension", **px("radius")},
           "size": {"$type": "dimension", **{name[:-3]: {"$value": f"{value}px"} for name, value in data["size"].items()}},
           "duration": {"$type": "duration", **{name[9:-3]: {"$value": f"{value}ms"} for name, value in data["motion"].items()
                                                if name.startswith("duration_")}},
           "easing": {"$type": "cubicBezier", **{name: {"$value": curve} for name, curve in data["easing"].items()}},
           "type": {"$type": "typography", **{name: {"$value": {
               "fontFamily": data["typography"]["mono_family" if name == "code" else "family"], "fontSize": f"{role['size_px']}px",
               "lineHeight": round(role["line_px"] / role["size_px"], 4), "fontWeight": role["weight"]}}
               for name, role in data["type_roles"].items()}}}
    for group in ["glass", "sheet", "card"]:
        out[group] = {"$type": "color"}
        for mode, parts in data[group].items():
            out[group][mode] = {part: {"$value": hexa(value["fill"] if isinstance(value, dict) and "fill" in value else value)}
                                for part, value in parts.items()}
    return out

def render():
    source = (ROOT / "tokens.json").read_bytes()
    data = json.loads(source)
    unit = {name: data[name] for name in ["selection_opacity", "primary_hover_opacity"]}
    unit |= {f"glow.{name}": v for name, v in data["glow"].items() if name.endswith("_alpha")}
    unit |= {f"tint.{name}": v for name, v in data["tint"].items()}
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
    header = f"Generated from Codewhale design {data['version']}; sha256 {digest}. Do not edit."
    keys = list(data["colors"]["dark"])
    assert set(keys) == set(data["colors"]["light"])
    for palette in [*data["colors"].values(), data["ombre"]]:
        assert all(len(v) == 6 and all(c in HEX for c in v) for v in palette.values())
    modes = list(data["colors"])
    for group in ["water", "surface_light", "glass", "sheet", "card"]:
        if set(data[group]) != set(modes):
            raise ValueError(f"{group} needs exactly the modes {', '.join(modes)}")
    for mode in modes:
        stops = data["water"][mode]
        if not stops or [s["at"] for s in stops] != sorted(s["at"] for s in stops):
            raise ValueError(f"water.{mode} needs stops in order")
        if set(data["glass"][mode]) != {"border", "highlight", *LEVELS}:
            raise ValueError(f"glass.{mode} needs border, highlight, {', '.join(LEVELS)}")
    check_solid_roles(data)
    violations = contrast_violations(data)
    if violations:
        raise ValueError("Contrast below WCAG AA:\n  " + "\n  ".join(violations))

    def rust_color(prefix, value, name):
        hex6, alpha = color(value, name)
        return [f"pub const {prefix}: u32 = 0x{hex6};", f"pub const {prefix}_ALPHA: f32 = {float(alpha)};"]

    rust = [f"// {header}", "#![allow(dead_code)]", "", f'pub const VERSION: &str = "{data["version"]}";',
            "#[derive(Clone, Copy, Debug)]", "pub struct Colors {"]
    rust += [f"    pub {k}: u32," for k in keys]
    rust += ["}"]
    for name, palette in data["colors"].items():
        rust += [f"pub const {name.upper()}: Colors = Colors {{"]
        rust += [f"    {k}: 0x{v}," for k, v in palette.items()]
        rust += ["};"]
    rust += ["pub fn colors(dark: bool) -> Colors {", "    if dark { DARK } else { LIGHT }", "}"]
    for mode in modes:
        m = mode.upper()
        for stop in data["water"][mode]:
            rust += [f"pub const WATER_{m}_{stop['at']}: u32 = 0x{color(stop['color'], 'water')[0]};"]
        for level in LEVELS:
            glass = data["glass"][mode][level]
            rust += rust_color(f"GLASS_{m}_{level.upper()}_FILL", glass["fill"], "glass")
            rust += [f"pub const GLASS_{m}_{level.upper()}_BLUR_PX: f32 = {float(glass['blur_px'])};"]
        for part in ["border", "highlight"]:
            rust += rust_color(f"GLASS_{m}_{part.upper()}", data["glass"][mode][part], "glass")
        for group in ["sheet", "card"]:
            for part, value in data[group][mode].items():
                rust += rust_color(f"{group.upper()}_{m}_{part.upper()}", value, group)
    rust += [f'pub const FONT_FAMILY: &str = "{data["typography"]["family"]}";',
             f'pub const FONT_MONO_FAMILY: &str = "{data["typography"]["mono_family"]}";',
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
               "muted":"muted_foreground","disabled":"disabled","line":"border","accent":"accent","button":"primary",
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
                f'  --elevation-dialog: 0 {elevation["dialog_y_px"]}px {elevation["dialog_blur_px"]}px rgb(0 0 0 / {elevation[f"dialog_alpha_{mode}"]});']
        water = ", ".join(f"#{color(s['color'], 'water')[0]} {s['at']}%" for s in data["water"][mode])
        light = data["surface_light"][mode]
        glow = ", ".join(f"{css_color(s['color'], 'surface_light')} {s['at']}%" for s in light["stops"])
        css += [f"  --water: linear-gradient(180deg, {water});",
                f"  --surface-light: radial-gradient(ellipse at {light['x']}% {light['y']}%, {glow});"]
        css += [f"  --glass-{level}-fill: {css_color(data['glass'][mode][level]['fill'], 'glass')};" for level in LEVELS]
        css += [f"  --glass-{part}: {css_color(data['glass'][mode][part], 'glass')};" for part in ["border", "highlight"]]
        for group in ["sheet", "card"]:
            css += [f"  --{group}-{part}: {css_color(value, group)};" for part, value in data[group][mode].items()]
        css += [f"  color-scheme: {mode};", "}"]
    dark_glass = data["glass"][modes[0]]
    for index, level in enumerate(["frosted", "clear", "solid"]):
        selector = ":root, [data-theme], [data-glass]" if index == 0 else f'[data-glass="{level}"], [data-glass="{level}"] [data-theme]'
        css += [f"{selector} {{", f"  --glass-fill: var(--glass-{level}-fill);",
                f"  --glass-blur: {dark_glass[level]['blur_px']}px;", "}"]
    css += [":root, [data-theme] {"]
    glow = data["glow"]
    css_alias = {"primary": "accent", "live": "live", "attention": "attention"}
    css += [f'  --glow-{name[:-6]}: 0 0 {glow["blur_px"]}px color-mix(in srgb, var(--{css_alias[name[:-6]]}) {round(value * 100)}%, transparent);'
            for name, value in glow.items() if name.endswith("_alpha")]
    css += [f'  --tint-{name.replace("_", "-")}: color-mix(in srgb, {source} {round(data["tint"][name] * 100)}%, transparent);'
            for name, source in TINT_SOURCES.items()]
    css += ["  --focus-ring: var(--focus-width) solid var(--accent);", "  --glass-edge: inset 0 1px 0 var(--glass-highlight);"]
    css += ["}"]
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
    css += [f"  --ombre-{name}: #{value};" for name, value in data["ombre"].items()]
    css += [f'  --ombre: linear-gradient(135deg, #{data["ombre"]["rise"]}, #{data["ombre"]["deep"]});']
    css += [f'  --icon-stroke: {data["icons"]["stroke"]};',
            f'  --font-family: {json.dumps(data["typography"]["family"], ensure_ascii=False)};',
            f'  --font-mono-family: {json.dumps(data["typography"]["mono_family"], ensure_ascii=False)};',
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
    ts = [f"// {header}", f"export const tokens = {json.dumps(data, indent=2, ensure_ascii=False)} as const;",
          "export type Tokens = typeof tokens;"]
    figma = figma_payload(data, aliases)
    sync = f"// {header}\nconst PAYLOAD = {json.dumps(figma, indent=1)};\n" + (HERE / "figma" / "sync.template.js").read_text()
    return {"tokens.rs": "\n".join(rust) + "\n", "tokens.css": "\n".join(css) + "\n", "tokens.ts": "\n".join(ts) + "\n",
            "tokens.dtcg.json": json.dumps(dtcg(data), indent=2) + "\n",
            "figma/variables.json": json.dumps(figma, indent=2) + "\n", "figma/sync.js": sync}

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
            path.parent.mkdir(exist_ok=True)
            path.write_text(content)
    if stale:
        raise SystemExit("Stale design artifact: " + ", ".join(stale))
    print("Design artifact verified" if args.check else "Design artifact generated")

if __name__ == "__main__":
    main()
