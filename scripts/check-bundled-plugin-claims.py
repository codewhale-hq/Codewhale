#!/usr/bin/env python3
"""Fail when the changelog describes a bundled plugin we do not ship.

The 0.10.0 section claimed "The bundled plugin is 0.4.0" and pinned marketplace
revision `ca6be22` while the tree actually shipped 0.11.2 at revision
d8640b17f275 -- three upstream releases apart, with nothing comparing the prose
to the assets. Vendored bundles get bumped by a dependency sweep; the sentence
describing them is prose nobody re-reads.

Checks that every shipped bundled-plugin version, and the first-party
marketplace revision, literally appear in CHANGELOG.md. Cheap, offline, and it
fails on the bump rather than at release.
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHANGELOG = ROOT / "CHANGELOG.md"
MARKETPLACE = ROOT / "crates/plugins/assets/first-party-marketplace.json"
PLUGIN_DIR = ROOT / "crates/tui/plugins"


def main() -> int:
    text = CHANGELOG.read_text()
    problems: list[str] = []

    for manifest in sorted(PLUGIN_DIR.glob("*/plugin.json")):
        data = json.loads(manifest.read_text())
        name, version = data.get("name", manifest.parent.name), data.get("version")
        if not version:
            continue
        # Only demand a receipt for a plugin the changelog actually discusses;
        # a bundle it never mentions is not making a false claim about itself.
        if name not in text:
            print(f"note: {name} {version} is bundled but unmentioned in CHANGELOG.md")
            continue
        if version not in text:
            problems.append(
                f"  {name}: ships {version}, but CHANGELOG.md never says {version}"
            )

    if MARKETPLACE.exists():
        revision = json.loads(MARKETPLACE.read_text()).get("revision")
        if revision and revision not in text:
            short = revision[:7]
            if short not in text:
                problems.append(
                    f"  first-party marketplace: pinned at {revision}, "
                    "which CHANGELOG.md never names"
                )

    if problems:
        print("Bundled-plugin claims are stale:\n")
        print("\n".join(problems))
        print(
            "\nUpdate the prose in CHANGELOG.md to match the shipped assets, or\n"
            "bump the assets. A version in the changelog is a claim to users."
        )
        return 1

    print("Bundled-plugin versions and the marketplace revision match CHANGELOG.md.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
