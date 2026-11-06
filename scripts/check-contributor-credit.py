#!/usr/bin/env python3
"""Fail when someone whose work landed in this release is not credited.

The three credit surfaces -- docs/CONTRIBUTORS.md, the CHANGELOG's Contributors
block, and web/lib/release-credits.ts -- were cross-checked only against
`requiredCandidateCredits` in docs/public-surface-facts.json, which is itself
hand-maintained. That gate proves the three files agree with each other; it
never asks whether the list is complete, so a contributor nobody remembered to
add was invisible to every check. Nine were missing from 0.10.0 when this was
written.

This derives the expected set from git history instead of from a list a human
curates: commit authors, `Co-authored-by:` trailers, and `Harvested from PR #N
by @handle` lines in the release window. Offline by design -- harvested handles
live in the commit bodies, so no network call is needed.

Usage: scripts/check-contributor-credit.py [<since-rev>]
"""
from __future__ import annotations
import re, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Bots and the maintainer account: real authors of most commits, never
# "contributors" in the credit sense this file guards.
SKIP = {
    "codewhale bot", "codewhalebot", "codewhale agent", "claude", "codex", "dependabot[bot]",
    "codewhale-maint", "copilot",
    "hunter bown", "hunter b", "hmbown", "deepseek-v41-flash", "devin",
}


def sh(*args: str) -> str:
    return subprocess.run(args, cwd=ROOT, capture_output=True, text=True, check=True).stdout


def author_map() -> dict[str, str]:
    """alias -> github handle, from .github/AUTHOR_MAP."""
    out: dict[str, str] = {}
    path = ROOT / ".github" / "AUTHOR_MAP"
    if not path.exists():
        return out
    for line in path.read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if "=" not in line:
            continue
        alias, target = (part.strip() for part in line.split("=", 1))
        m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", target)
        if m:
            out[alias.lower()] = m.group(1)
    return out


def expected(since: str) -> dict[str, str]:
    """handle -> why we think they are owed credit."""
    amap = author_map()
    found: dict[str, str] = {}

    def add(name: str, email: str, why: str) -> None:
        low = name.strip().lower()
        # Model co-authorship ("Claude Opus 5 (1M context)") and bot addresses
        # are not people owed credit; match on prefix since the model name and
        # its context window change release to release.
        if any(low.startswith(skip) for skip in SKIP):
            return
        if "[bot]" in email.lower() or email.lower().endswith(("noreply@anthropic.com", "noreply@openai.com")):
            return
        handle = amap.get(email.lower()) or amap.get(name.strip().lower())
        if not handle:
            m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", email)
            handle = m.group(1) if m else name.strip()
        if any(handle.lower().startswith(skip) for skip in SKIP):
            return
        found.setdefault(handle, why)

    for line in sh("git", "log", f"{since}..HEAD", "--format=%an\t%ae").splitlines():
        name, _, email = line.partition("\t")
        add(name, email, "commit author")

    body = sh("git", "log", f"{since}..HEAD", "--format=%b")
    for name, email in re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", body):
        add(name, email, "co-author trailer")
    for pr, handle in re.findall(r"(?i)harvested from PR #(\d+) by @([A-Za-z0-9-]+)", body):
        if handle.lower() not in SKIP:
            found.setdefault(handle, f"harvested PR #{pr}")
    return found


def main() -> int:
    since = sys.argv[1] if len(sys.argv) > 1 else sh("git", "describe", "--tags", "--abbrev=0").strip()
    owed = expected(since)
    if not owed:
        print(f"No external contributors found since {since}.")
        return 0

    surfaces = {
        "docs/CONTRIBUTORS.md": (ROOT / "docs/CONTRIBUTORS.md").read_text(),
        "CHANGELOG.md": (ROOT / "CHANGELOG.md").read_text(),
        "web/lib/release-credits.ts": (ROOT / "web/lib/release-credits.ts").read_text(),
    }

    missing: list[str] = []
    for handle, why in sorted(owed.items(), key=lambda kv: kv[0].lower()):
        absent = [name for name, text in surfaces.items() if handle not in text]
        if absent:
            missing.append(f"  @{handle} ({why}) -- not in: {', '.join(absent)}")

    print(f"Contributor credit since {since}: {len(owed)} contributor(s) found.")
    amap = author_map()
    lost = []
    for entry in sh("git", "log", f"{since}..HEAD", "--format=%h\t%an\t%ae\t%B%x00").split("\0"):
        parts = entry.strip().split("\t", 3)
        if len(parts) < 4:
            continue
        sha, name, email, body = parts
        m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", email)
        author = (amap.get(email.lower()) or amap.get(name.lower()) or (m.group(1) if m else name)).lower()
        for handle in re.findall(r"(?i)harvested from PR #\d+ by @([A-Za-z0-9-]+)", body):
            if handle.lower() != author:
                lost.append(f"  {sha} harvested @{handle} but is authored by {name}")
        if any(author.startswith(skip) for skip in SKIP):
            for co_name, co_email in re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", body):
                cm = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", co_email)
                human = amap.get(co_email.lower()) or amap.get(co_name.strip().lower()) or (cm.group(1) if cm else None)
                low = co_name.strip().lower()
                if human and not any(h.startswith(skip) for skip in SKIP for h in (human.lower(), low)) \
                        and "[bot]" not in co_email.lower() and not co_email.lower().endswith(("noreply@anthropic.com", "noreply@openai.com")):
                    lost.append(f"  {sha} credits @{human} only as a co-author; authored by {name}")
    if lost:
        print("\nCommits whose human contributor is not the commit author (GitHub's contributor list will miss them):\n")
        print("\n".join(lost))
    if missing:
        print("\nUncredited work landed in this release:\n")
        print("\n".join(missing))
        print(
            "\nAdd each to docs/CONTRIBUTORS.md (canonical), the CHANGELOG's\n"
            "Contributors block, and web/lib/release-credits.ts. Harvested work\n"
            "is still their work -- see AGENTS.md 'Landing other people's work'."
        )
        return 1
    print("Every contributor in the window is credited on all three surfaces.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
