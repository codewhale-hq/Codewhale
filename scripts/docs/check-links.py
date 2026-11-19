#!/usr/bin/env python3
"""check-links.py — report Markdown links that point at nothing.

Reads every tracked Markdown file and checks each relative link, each
`https://github.com/codewhale-hq/Codewhale/blob/main/...` link and each
`#anchor` against the tracked tree and the headings of the target page.
External URLs are not fetched.

It is WARN-ONLY: it prints findings and exits 0. Pass --strict to exit 1 on
any finding, and --orphans to also list docs pages no Markdown file links.

    python3 scripts/docs/check-links.py
    python3 scripts/docs/check-links.py --strict
    python3 scripts/docs/check-links.py --orphans
"""
from __future__ import annotations

import argparse
import collections
import os
import re
import subprocess
import sys
import unicodedata
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SKIP_PREFIXES = ("node_modules/", "vendor/", "web/node_modules/")
SKIP_PARTS = ("/fixtures/", "/upstream/")
LINK = re.compile(r'(?<!\!)\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)')
IMAGE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)")
FENCE = re.compile(r"```.*?```", re.S)
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
EXPLICIT_ANCHOR = re.compile(r'<a\s+(?:id|name)="([^"]+)"')
REPO_URL = re.compile(r"https://github\.com/codewhale-hq/[Cc]ode[Ww]hale/(?:blob|tree)/main/([^#?]+)")
EXTERNAL = re.compile(r"^(https?:|mailto:|<|data:)")


def tracked_files() -> set[str]:
    out = subprocess.run(
        ["git", "-C", str(REPO_ROOT), "ls-files"], capture_output=True, text=True, check=True
    ).stdout
    return {line for line in out.split("\n") if line}


def slugify(heading: str) -> str:
    heading = heading.strip().lower().replace("`", "")
    heading = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    kept = (ch for ch in heading if ch in "-_ " or unicodedata.category(ch)[0] in "LNM")
    return "".join(kept).replace(" ", "-")


def anchors_of(path: str, cache: dict[str, set[str]]) -> set[str]:
    if path in cache:
        return cache[path]
    found: set[str] = set()
    counts: collections.Counter[str] = collections.Counter()
    try:
        text = (REPO_ROOT / path).read_text(encoding="utf8")
    except OSError:
        text = ""
    in_fence = False
    for line in text.split("\n"):
        if line.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = HEADING.match(line)
        if match:
            slug = slugify(match.group(2))
            seen = counts[slug]
            counts[slug] += 1
            found.add(slug if seen == 0 else f"{slug}-{seen}")
        found.update(EXPLICIT_ANCHOR.findall(line))
    cache[path] = found
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--strict", action="store_true", help="exit 1 on any finding")
    parser.add_argument("--orphans", action="store_true", help="list docs pages with no inbound link")
    args = parser.parse_args()

    tracked = tracked_files()
    dirs: set[str] = set()
    for name in tracked:
        parent = os.path.dirname(name)
        while parent:
            dirs.add(parent)
            parent = os.path.dirname(parent)
    pages = sorted(
        name
        for name in tracked
        if name.endswith(".md")
        and not name.startswith(SKIP_PREFIXES)
        and not any(part in name for part in SKIP_PARTS)
    )

    cache: dict[str, set[str]] = {}
    broken: list[tuple[str, str]] = []
    bad_anchor: list[tuple[str, str]] = []
    inbound: dict[str, set[str]] = collections.defaultdict(set)

    def has_anchor(page: str, fragment: str) -> bool:
        known = anchors_of(page, cache)
        return fragment in known or fragment.lower() in known

    for page in pages:
        text = FENCE.sub("", (REPO_ROOT / page).read_text(encoding="utf8"))
        for match in list(LINK.finditer(text)) + list(IMAGE.finditer(text)):
            target = match.group(1)
            if target.startswith("#"):
                if target[1:] and not has_anchor(page, target[1:]):
                    bad_anchor.append((page, target))
                continue
            if EXTERNAL.match(target):
                repo_match = REPO_URL.match(target)
                if repo_match:
                    path = repo_match.group(1).rstrip("/")
                    if path in tracked:
                        inbound[path].add(page)
                    elif path not in dirs:
                        broken.append((page, target))
                continue
            path, _, fragment = target.partition("#")
            path = path.split("?")[0]
            if not path:
                continue
            resolved = (
                path.lstrip("/")
                if path.startswith("/")
                else os.path.normpath(os.path.join(os.path.dirname(page), path))
            )
            if resolved in tracked:
                inbound[resolved].add(page)
                if fragment and resolved.endswith(".md") and not has_anchor(resolved, fragment):
                    bad_anchor.append((page, target))
            elif resolved.rstrip("/") not in dirs and resolved != ".":
                broken.append((page, target))

    for page, target in broken:
        print(f"{page}: broken link {target}")
    for page, target in bad_anchor:
        print(f"{page}: missing anchor {target}")
    orphans: list[str] = []
    if args.orphans:
        orphans = [
            page
            for page in pages
            if page.startswith("docs/")
            and page.count("/") == 1
            and page != "docs/README.md"
            and not (inbound.get(page, set()) - {page})
        ]
        for page in orphans:
            print(f"{page}: no Markdown file links this page")

    print(
        f"check-links: {len(pages)} Markdown files, {len(broken)} broken links, "
        f"{len(bad_anchor)} missing anchors"
        + (f", {len(orphans)} unlinked docs pages" if args.orphans else "")
    )
    if args.strict and (broken or bad_anchor):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
