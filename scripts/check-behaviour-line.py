#!/usr/bin/env python3
"""Require a behaviour statement in every behaviour-changing commit.

A commit whose subject starts with feat/fix/perf/refactor must say what changed
for users, either:

    Behaviour: skills index budget default 25600 -> 2400 chars (0.05 -> 0.002)

or, deliberately:

    No-behaviour-change: refactor only; no default, limit or rendered block changed

Merge, revert, chore, docs, test, ci, build and style commits are exempt: they
do not ask a user to expect anything different. This is a statement check, not
a semantic one. It makes silence a decision; the regression pass reads the
statement next to the diff.

Usage:
    scripts/check-behaviour-line.py [--warn-only] [base] [head]
    scripts/check-behaviour-line.py [--warn-only] --range base..head

Defaults: base = BEFORE_SHA when set, else the merge base with origin/main;
head = HEAD.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

BEHAVIOUR_RE = re.compile(r"^Behaviour:\s*\S", re.MULTILINE)
NO_CHANGE_RE = re.compile(r"^No-behaviour-change:\s*\S", re.MULTILINE)
SUBJECT_RE = re.compile(r"^(feat|fix|perf|refactor)(\([^)]*\))?!?:")


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        sys.exit(
            f"check-behaviour-line: git {' '.join(args)} failed: "
            f"{result.stderr.strip()}"
        )
    return result.stdout


def default_base() -> str:
    before = os.environ.get("BEFORE_SHA", "").strip()
    if before and set(before) != {"0"}:
        return before
    return git("merge-base", "HEAD", "origin/main").strip()


def commits(base: str, head: str) -> list[tuple[str, str, str]]:
    raw = git("log", "--no-merges", "--format=%H%x1f%s%x1f%b%x1e", f"{base}..{head}")
    records: list[tuple[str, str, str]] = []
    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record.strip():
            continue
        parts = record.split("\x1f", 2)
        if len(parts) == 3:
            records.append((parts[0], parts[1], parts[2]))
    return records


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Require Behaviour: or No-behaviour-change: in behaviour-changing commits."
    )
    parser.add_argument("base", nargs="?", help="range base (default: BEFORE_SHA or origin/main merge base)")
    parser.add_argument("head", nargs="?", default="HEAD", help="range head (default: HEAD)")
    parser.add_argument("--range", dest="range_spec", help="BASE..HEAD")
    parser.add_argument(
        "--warn-only",
        action="store_true",
        help="print the findings but exit 0 (preflight and the release-candidate report)",
    )
    args = parser.parse_args(argv)

    if args.range_spec:
        if ".." not in args.range_spec:
            parser.error("--range must be BASE..HEAD")
        base, head = args.range_spec.split("..", 1)
    else:
        base = args.base or default_base()
        head = args.head

    missing: list[tuple[str, str]] = []
    checked = 0
    for sha, subject, body in commits(base, head):
        if not SUBJECT_RE.match(subject):
            continue
        checked += 1
        if BEHAVIOUR_RE.search(body) or NO_CHANGE_RE.search(body):
            continue
        missing.append((sha, subject))

    if missing:
        print(
            f"check-behaviour-line: {len(missing)} of {checked} behaviour-changing "
            f"commit(s) in {base[:12]}..{head} do not state what changed:",
            file=sys.stderr,
        )
        for sha, subject in missing:
            print(f"  - {sha[:12]} {subject}", file=sys.stderr)
        print(
            "Add a body line: 'Behaviour: <old> -> <new>' or "
            "'No-behaviour-change: <why not>'",
            file=sys.stderr,
        )
        return 0 if args.warn_only else 1

    print(
        f"check-behaviour-line: {checked} behaviour-changing commit(s) carry a "
        f"statement ({base[:12]}..{head})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
