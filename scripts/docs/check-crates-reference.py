#!/usr/bin/env python3
"""check-crates-reference.py — keep docs/reference/crates.md equal to the workspace.

The page is one table row per `crates/*/Cargo.toml`: package name, path and
the manifest's `description`. It fails when the committed page differs, so a
new crate, a removed crate or a changed description shows up in review.

    python3 scripts/docs/check-crates-reference.py           # check
    python3 scripts/docs/check-crates-reference.py --write   # regenerate
"""
from __future__ import annotations

import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
PAGE = REPO_ROOT / "docs" / "reference" / "crates.md"

HEADER = """# Workspace crates

Generated from each `crates/*/Cargo.toml` by
`python3 scripts/docs/check-crates-reference.py --write`. Do not edit the
table by hand; change the crate's `description` and regenerate. How the crates
fit together is in [Architecture](../ARCHITECTURE.md).

| Crate | Path | What it is |
| --- | --- | --- |
"""


def render() -> str:
    rows = []
    for manifest in sorted((REPO_ROOT / "crates").glob("*/Cargo.toml")):
        package = tomllib.loads(manifest.read_text(encoding="utf8")).get("package", {})
        name = package.get("name")
        description = package.get("description")
        if not isinstance(name, str) or not isinstance(description, str):
            raise SystemExit(f"{manifest.relative_to(REPO_ROOT)}: package name and description are required")
        text = " ".join(description.split()).replace("|", "\\|")
        rows.append(f"| `{name}` | `crates/{manifest.parent.name}` | {text} |")
    return HEADER + "\n".join(rows) + "\n"


def main() -> int:
    expected = render()
    if "--write" in sys.argv[1:]:
        PAGE.parent.mkdir(parents=True, exist_ok=True)
        PAGE.write_text(expected, encoding="utf8")
        print(f"check-crates-reference: wrote {PAGE.relative_to(REPO_ROOT)}")
        return 0
    current = PAGE.read_text(encoding="utf8") if PAGE.exists() else ""
    if current != expected:
        print(
            "check-crates-reference: docs/reference/crates.md is out of date; run "
            "python3 scripts/docs/check-crates-reference.py --write"
        )
        return 1
    print(f"check-crates-reference: {expected.count(chr(10)) - HEADER.count(chr(10))} crates, page is current")
    return 0


if __name__ == "__main__":
    sys.exit(main())
