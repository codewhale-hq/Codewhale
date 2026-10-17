from pathlib import Path
import argparse
import re
import tomllib

ROOT = Path(__file__).resolve().parent.parent
GROUP = "crates/tui/src/commands/groups/core"
ROOTS = (f"{GROUP}/navigation.rs",)
PROOF = "tests/portable-core-navigation/src/lib.rs"
MANIFEST = "tests/portable-core-navigation/Cargo.toml"
HOST = f"{GROUP}/mod.rs"
PAIRS = (
    ("anchor", "AnchorCmd"), ("help", "HelpCmd"), ("hotbar", "HotbarCmd"),
    ("links", "LinksCmd"), ("transcript", "TranscriptCmd"), ("turn", "TurnCmd"),
    ("home", "HomeCmd"), ("home", "OverviewCmd"), ("workspace", "WorkspaceCmd"),
    ("translate", "TranslateCmd"), ("voice", "VoiceCmd"),
    ("voice", "VoiceSendCmd"), ("voice", "VoiceControlCmd"),
)
NAMES = {
    "anchor": ("anchor",), "help": ("help",), "hotbar": ("hotbar",),
    "links": ("links",), "transcript": ("transcript",), "turn": ("turn",),
    "home": ("home", "overview"), "workspace": ("workspace",),
    "translate": ("translate",), "voice": ("voice", "voicesend", "voicecontrol"),
}
REQUIRED = {ROOTS[0]} | {f"{GROUP}/{name}.rs" for name in NAMES}
ALLOWED_NORMAL = {
    "codewhale-portable-core-navigation", "codewhale-command-contract", "codewhale-protocol",
    "chrono", "iana-time-zone", "android_system_properties", "libc", "core-foundation-sys",
    "iana-time-zone-haiku", "js-sys", "cfg-if", "futures-util", "futures-core", "futures-task",
    "pin-project-lite", "slab", "wasm-bindgen", "once_cell", "wasm-bindgen-macro", "quote",
    "proc-macro2", "unicode-ident", "wasm-bindgen-macro-support", "bumpalo", "syn",
    "wasm-bindgen-shared", "log", "windows-core", "windows-implement", "windows-interface",
    "windows-link", "windows-result", "windows-strings", "num-traits", "serde", "serde_core",
    "serde_derive", "serde_json", "indexmap", "equivalent", "hashbrown", "itoa", "memchr",
    "zmij", "uuid", "getrandom", "r-efi",
}
HOST_IMPORT = re.compile(
    r"\b(?:App|AppAction|codewhale_(?:tui|core|runtime|config|execpolicy|secrets|state|localization))\b"
    r"|\bcrate::(?:tui|core|config|commands)\b|\bsuper::core\b|\bBox\s*<"
    r"|\bstd::(?:fs|net|process|env)\b|\binclude(?:_str|_bytes)?\s*!"
)
MODULE = re.compile(r'(?:#\[path\s*=\s*"([^"]+)"\]\s*)?(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;')
PAIR = re.compile(r'\(\s*(\w+)::(\w+)::info\(\),\s*\1::\2::handler\(\),?\s*\)')


def source_text(path):
    return "\n".join(line.split("//")[0] for line in path.read_text().splitlines())


def conditional_source(text):
    without_test_module = re.sub(r'#\[cfg\(test\)\]\s*mod\s+tests\s*\{', 'mod tests {', text)
    return bool(re.search(r'#\s*\[\s*cfg(?:_attr)?\b', without_test_module))


def graph_violations(graph):
    names = {line.split()[0] for line in graph.splitlines() if line.strip()}
    errors = [f"unapproved normal dependency: {name}" for name in sorted(names - ALLOWED_NORMAL)]
    for required in ("codewhale-portable-core-navigation", "codewhale-command-contract", "codewhale-protocol"):
        if required not in names:
            errors.append(f"missing actual normal graph package: {required}")
    return errors


def source_closure(root):
    pending = [root / name for name in ROOTS]
    seen, errors = set(), []
    while pending:
        path = pending.pop().resolve()
        if path in seen:
            continue
        if not path.is_relative_to(root) or not path.is_file():
            errors.append(f"missing or out-of-tree navigation source: {path}")
            continue
        seen.add(path)
        text = source_text(path)
        if HOST_IMPORT.search(text):
            errors.append(f"host/service in portable navigation source: {path.relative_to(root)}")
        if conditional_source(text):
            errors.append(f"conditional production source is not a normal proof: {path.relative_to(root)}")
        for explicit, name in MODULE.findall(text):
            if explicit:
                child = path.parent / explicit
            else:
                base = path.parent if path.name in {"mod.rs", "lib.rs"} else path.with_suffix("")
                child = base / f"{name}.rs"
                if not child.is_file():
                    child = base / name / "mod.rs"
            pending.append(child)
    return {str(path.relative_to(root)) for path in seen}, errors


def violations(root):
    root = root.resolve()
    errors = []
    wrapper = root / PROOF
    mount = re.fullmatch(r'\s*#\[path\s*=\s*"([^"]+)"\]\s*pub mod navigation;\s*', wrapper.read_text())
    if not mount or (wrapper.parent / mount.group(1)).resolve() != (root / ROOTS[0]).resolve():
        errors.append("proof must mount only the actual complete production navigation root normally")
    manifest = tomllib.loads((root / MANIFEST).read_text())
    if manifest.get("package", {}).get("publish") is not False:
        errors.append("proof must remain unpublished")
    deps = manifest.get("dependencies", {})
    contract = deps.get("codewhale-command-contract", {})
    if set(deps) != {"codewhale-command-contract"} or not isinstance(contract, dict) or (root / MANIFEST).parent.joinpath(contract.get("path", "")).resolve() != (root / "crates/command-contract").resolve():
        errors.append("actual shared contract must be the sole normal direct dependency")
    if manifest.get("dev-dependencies") or manifest.get("build-dependencies") or manifest.get("target"):
        errors.append("proof cannot hide a host or substitute behind test/build/target dependencies")
    closure, source_errors = source_closure(root)
    errors.extend(source_errors)
    for name in sorted(REQUIRED - closure):
        errors.append(f"incomplete navigation source closure: {name}")
    for name in sorted(REQUIRED):
        path = root / name
        if path.is_file():
            text = source_text(path)
            if HOST_IMPORT.search(text):
                errors.append(f"host/service in declared movable source: {name}")
            if conditional_source(text):
                errors.append(f"conditional declared movable source: {name}")
    inventory = source_text(root / ROOTS[0])
    if tuple(PAIR.findall(inventory)) != PAIRS or not re.search(r';\s*13\s*\]', inventory):
        errors.append("production inventory must contain all13 real matching registrations in original filtered order")
    for module, expected in NAMES.items():
        path = root / GROUP / f"{module}.rs"
        if path.is_file() and tuple(re.findall(r'\bname:\s*"([^"]+)"', source_text(path))) != expected:
            errors.append(f"canonical metadata mismatch: {module}")
    actual = [int(index) for index in re.findall(
        r'core_navigation_host::registration::<(\d+)>\(\)', source_text(root / HOST)
    )]
    if actual != list(range(len(PAIRS))):
        errors.append("real host core registry must contain all 13 inventory entries exactly once in relative order")
    return errors


def main():
    parser = argparse.ArgumentParser(description="Verify the real complete core-navigation slice and its all-target normal graph")
    parser.add_argument("--graph", type=Path, required=True)
    args = parser.parse_args()
    errors = violations(ROOT) + graph_violations(args.graph.read_text())
    for error in errors:
        print(f"[core-navigation-proof] FAIL: {error}")
    if not errors:
        print("[core-navigation-proof] PASS: all13 actual registrations, complete source closure, host parity and approved normal graph; core remains pending")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
