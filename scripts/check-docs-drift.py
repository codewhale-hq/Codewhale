#!/usr/bin/env python3
"""Fail when a feature row's owner changed without its summary or docs.

`docs/features.toml` is the machine-readable statement of what the product
does. The feature-registry test already checks that every `docs` and `owner`
path exists; this check adds the content half for the commits in a range:

- a row whose `owner` path changed must also change its `summary`, unless the
  range carries a `No-behaviour-change:` statement;
- a row whose `owner` changed must have a `docs` page that mentions the row's
  `flag` (or its `id` when the row has no flag).

Only rows touched by the range are checked; history is not back-filled. The
registry's own header describes the fields.

Usage:
    scripts/check-docs-drift.py [base] [head]
    scripts/check-docs-drift.py --range base..head

Defaults: base = BEFORE_SHA when set, else the merge base with origin/main;
head = HEAD.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

FEATURES = "docs/features.toml"
NO_CHANGE_RE = re.compile(r"^No-behaviour-change:\s*\S", re.MULTILINE)

ROOT = Path(
    os.environ.get("CODEWHALE_REPO_ROOT", Path(__file__).resolve().parents[1])
).resolve()


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        sys.exit(
            f"check-docs-drift: git {' '.join(args)} failed: {result.stderr.strip()}"
        )
    return result.stdout


def default_base() -> str:
    before = os.environ.get("BEFORE_SHA", "").strip()
    if before and set(before) != {"0"}:
        return before
    return git("merge-base", "HEAD", "origin/main").strip()


def load_rows(text: str) -> list[dict]:
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as err:
        sys.exit(f"check-docs-drift: {FEATURES} is not valid TOML: {err}")
    return data.get("feature", [])


def range_has_no_change_statement(base: str, head: str) -> bool:
    raw = git("log", "--no-merges", "--format=%b", f"{base}..{head}")
    return bool(NO_CHANGE_RE.search(raw))


def owner_touched(owner: str, changed: set[str]) -> bool:
    owner = owner.rstrip("/")
    return any(path == owner or path.startswith(owner + "/") for path in changed)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Check that a touched feature row updates its summary and docs."
    )
    parser.add_argument("base", nargs="?", help="range base (default: BEFORE_SHA or origin/main merge base)")
    parser.add_argument("head", nargs="?", default="HEAD", help="range head (default: HEAD)")
    parser.add_argument("--range", dest="range_spec", help="BASE..HEAD")
    args = parser.parse_args(argv)

    if args.range_spec:
        if ".." not in args.range_spec:
            parser.error("--range must be BASE..HEAD")
        base, head = args.range_spec.split("..", 1)
    else:
        base = args.base or default_base()
        head = args.head

    head_rows = load_rows((ROOT / FEATURES).read_text(encoding="utf-8"))
    try:
        base_rows = load_rows(git("show", f"{base}:{FEATURES}"))
    except SystemExit:
        base_rows = []
    base_by_id = {row.get("id"): row for row in base_rows}
    changed = set(git("diff", "--name-only", f"{base}..{head}").splitlines())
    no_change = range_has_no_change_statement(base, head)

    failures: list[str] = []
    checked = 0
    for row in head_rows:
        row_id = row.get("id", "<unnamed>")
        owner = row.get("owner")
        if not owner or not owner_touched(owner, changed):
            continue
        checked += 1
        old_summary = base_by_id.get(row_id, {}).get("summary")
        if row.get("summary") == old_summary and not no_change:
            failures.append(
                f"{row_id}: owner {owner} changed but the summary did not; "
                "update the summary or add a 'No-behaviour-change:' line"
            )
        docs = row.get("docs")
        if not docs:
            continue
        page = docs.split("#", 1)[0]
        page_path = ROOT / page
        if not page_path.is_file():
            failures.append(f"{row_id}: docs page {page} does not exist")
            continue
        token = row.get("flag") or row_id
        if token and token not in page_path.read_text(encoding="utf-8"):
            failures.append(
                f"{row_id}: docs page {page} does not mention {token}; "
                "the page no longer describes the feature the row names"
            )

    if failures:
        print(
            f"check-docs-drift: {len(failures)} feature row(s) changed without "
            f"their docs or summary ({base[:12]}..{head}):",
            file=sys.stderr,
        )
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(
        f"check-docs-drift: {checked} touched feature row(s) keep their summary "
        f"and docs aligned ({base[:12]}..{head})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
