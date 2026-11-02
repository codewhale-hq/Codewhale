#!/usr/bin/env python3
"""Require the complete production config group and a service-free normal graph.

The adjacent normal library is the Rust authority: no fake outcomes, selected
leaf or cfg(test)-only harness. Follow local module includes (including tests),
then audit all-target transitive normal Cargo edges supplied by the runner.

FEAT-028 completes the whole config group, so this proof covers both the
portable main handlers (`config.rs`) and the policy slice (`policy.rs`).
"""
from pathlib import Path
import argparse
import re
import tomllib

ROOT = Path(__file__).resolve().parent.parent
GROUP_DIR = "crates/tui/src/commands/groups/config"
ROOTS = ("crates/tui/src/commands/groups/config/mod.rs",)
PROOF = "tests/portable-config/src/lib.rs"
MANIFEST = "tests/portable-config/Cargo.toml"
REQUIRED = {
    ROOTS[0],
    "crates/tui/src/commands/groups/config/config.rs",
    "crates/tui/src/commands/groups/config/import_claude.rs",
    "crates/tui/src/commands/groups/config/permissions.rs",
    "crates/tui/src/commands/groups/config/policy.rs",
    "crates/tui/src/commands/groups/config/status.rs",
    "crates/tui/src/commands/groups/config/policy_messages.rs",
    "crates/tui/src/commands/groups/config/policy_tests.rs",
    "crates/command-contract/src/money.rs",
}
ALLOWED_WORKSPACE = {
    "codewhale-portable-config",
    "codewhale-command-contract",
    "codewhale-protocol", "codewhale-app-server-protocol",
    "codewhale-sanitize",
}
FORBIDDEN_SERVICES = {
    "tokio", "reqwest", "hyper", "rusqlite", "sqlx", "keyring", "dbus", "zbus",
    "secret-service", "ratatui", "crossterm", "ureq", "mio", "native-tls", "openssl",
}
HOST_IMPORT = re.compile(
    r"\b(?:App|AppAction|codewhale_(?:tui|core|runtime|config|execpolicy|secrets|state|localization))\b"
    r"|\bcrate::(?:tui|core|config|commands::(?:traits|contract|CommandResult))\b"
    r"|\bstd::(?:fs|net|process|env)\b"
)
MODULE = re.compile(r'(?:#\[path\s*=\s*"([^"]+)"\]\s*)?(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;')


def graph_violations(graph):
    names = {line.split()[0] for line in graph.splitlines() if line.strip()}
    return [f"unapproved normal dependency: {name}" for name in sorted(names)
            if (name.startswith("codewhale-") and name not in ALLOWED_WORKSPACE)
            or name in FORBIDDEN_SERVICES]


def source_closure(root, roots):
    pending = [root / path for path in roots]
    seen = set()
    errors = []
    while pending:
        path = pending.pop().resolve()
        if path in seen:
            continue
        if not path.is_relative_to(root.resolve()) or not path.is_file():
            errors.append(f"missing or out-of-tree config module: {path}")
            continue
        seen.add(path)
        source = "\n".join(line.split("//")[0] for line in path.read_text().splitlines())
        if HOST_IMPORT.search(source):
            errors.append(f"host service in config source: {path.relative_to(root)}")
        if "use codewhale_command_contract::money;" in source:
            pending.append(root / "crates/command-contract/src/money.rs")
        for explicit, name in MODULE.findall(source):
            if explicit:
                child = path.parent / explicit
            else:
                base = path.parent if path.name in {"mod.rs", "lib.rs"} else path.with_suffix("")
                child = base / f"{name}.rs"
                if not child.is_file():
                    child = base / name / "mod.rs"
            pending.append(child)
    return {str(path.relative_to(root)) for path in seen}, errors


def directory_violations(root):
    """Reject a host import in ANY file of the movable group directory.

    The mounted-closure scan only sees files reachable from the proof roots; the
    group root (`mod.rs`) and any future sibling would escape it. This scans the
    whole directory so no file can be omitted, which is the FEAT-026 lesson.
    """
    errors = []
    for path in sorted((root / GROUP_DIR).glob("*.rs")):
        source = "\n".join(line.split("//")[0] for line in path.read_text().splitlines())
        if HOST_IMPORT.search(source):
            errors.append(f"host service in movable group file: {path.relative_to(root)}")
    return errors


def violations(root):
    root = root.resolve()
    errors = []
    wrapper = root / PROOF
    text = wrapper.read_text()
    includes = re.findall(r'#\[path\s*=\s*"([^"]+)"\]\s*pub mod (\w+);', text)
    resolved = {(wrapper.parent / inc).resolve() for inc, _ in includes}
    if "#[cfg" in text or resolved != {(root / path).resolve() for path in ROOTS}:
        errors.append("proof must include the actual config and policy roots as a normal library")
    manifest = tomllib.loads((root / MANIFEST).read_text())
    if manifest.get("package", {}).get("publish") is not False:
        errors.append("proof must remain unpublished")
    if not {"codewhale-command-contract", "codewhale-protocol"} <= set(manifest.get("dependencies", {})):
        errors.append("shared shapes must be normal dependencies, not test-only substitutes")
    closure, source_errors = source_closure(root, ROOTS)
    errors.extend(source_errors)
    errors.extend(directory_violations(root))
    for missing in sorted(REQUIRED - closure):
        errors.append(f"incomplete config source closure: {missing}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--graph", type=Path, required=True)
    args = parser.parse_args()
    errors = violations(ROOT) + graph_violations(args.graph.read_text())
    for error in errors:
        print(f"[config-proof] FAIL: {error}")
    if not errors:
        print("[config-proof] PASS: actual complete group and approved normal dependency graph")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
