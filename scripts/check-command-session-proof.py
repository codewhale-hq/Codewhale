#!/usr/bin/env python3
"""Require the actual whole session group and a normal graph without host services.

Source inclusion compiles all transitive helpers and inline tests, without stubs.
Cargo compilation remains the authority for Rust name/type resolution. This guard
also catches architecture drift which compiles but imports runtime infrastructure.
"""
from pathlib import Path
import argparse
import re

ROOT = Path(__file__).resolve().parent.parent
GROUP = "crates/tui/src/commands/groups/session/mod.rs"
PROOF = "tests/portable-session/src/commands/groups/mod.rs"
ALLOWED_WORKSPACE = {
    "codewhale-portable-session", "codewhale-command-contract",
    "codewhale-protocol", "codewhale-app-server-protocol", "codewhale-sanitize",
}
FORBIDDEN_SERVICES = {
    "tokio", "reqwest", "hyper", "rusqlite", "sqlx", "keyring", "dbus",
    "zbus", "secret-service", "ratatui", "crossterm",
}


def graph_violations(graph):
    names = {line.split()[0] for line in graph.splitlines() if line.strip()}
    return [f"unapproved normal dependency: {name}" for name in sorted(names)
            if (name.startswith("codewhale-") and name not in ALLOWED_WORKSPACE)
            or name in FORBIDDEN_SERVICES]


def violations(root):
    errors = []
    wrapper = root / PROOF
    text = wrapper.read_text()
    includes = re.findall(r'#\[path\s*=\s*"([^"]+)"\]\s*pub mod session;', text)
    if len(includes) != 1 or (wrapper.parent / includes[0]).resolve() != (root / GROUP).resolve():
        errors.append("proof must include the actual session group root, not a selected leaf or stub")
    for path in sorted((root / GROUP).parent.rglob("*.rs")):
        # Exclude comments, but retain cfg(test) code: its dependency closure matters.
        source = "\n".join(line.split("//")[0] for line in path.read_text().splitlines())
        if re.search(r'\b(?:crate::(?:tui|remote_control|commands::(?:traits|contract|CommandResult))|AppAction|codewhale_(?:core|secrets|tui|runtime))\b', source):
            errors.append(f"host dependency in session closure: {path.relative_to(root)}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--graph", type=Path)
    args = parser.parse_args()
    errors = violations(ROOT)
    if args.graph:
        errors += graph_violations(args.graph.read_text())
    for error in errors:
        print(f"[session-proof] FAIL: {error}")
    if not errors:
        print("[session-proof] PASS: actual complete group and approved dependency boundary")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
