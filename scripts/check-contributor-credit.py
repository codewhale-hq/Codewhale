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
import functools, re, subprocess, sys
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


@functools.cache
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


NOREPLY = re.compile(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com")


def not_a_person(name: str, email: str) -> bool:
    low, addr = name.strip().lower(), email.lower()
    return any(low.startswith(skip) for skip in SKIP) or "[bot]" in addr \
        or addr.endswith(("noreply@anthropic.com", "noreply@openai.com"))


def maintainer(name: str, email: str) -> bool:
    handle = author_map().get(email.lower()) or author_map().get(name.strip().lower()) or name.strip()
    return any(h.lower().startswith(skip) for skip in SKIP for h in (name.strip(), handle))


def restoring(subject: str, name: str, email: str) -> bool:
    return subject.startswith("chore(credit):") and maintainer(name, email)


def noreply_ids() -> dict[str, str]:
    out: dict[str, str] = {}
    path = ROOT / ".github" / "AUTHOR_MAP"
    if path.exists():
        for login_id, login in re.findall(r"(\d+)\+([^@<>\s]+)@users\.noreply\.github\.com", path.read_text()):
            out[login.lower()] = login_id
    return out


def expected(since: str) -> dict[str, str]:
    """handle -> why we think they are owed credit."""
    amap = author_map()
    found: dict[str, str] = {}

    def add(name: str, email: str, why: str) -> None:
        # Model co-authorship ("Claude Opus 5 (1M context)") and bot addresses
        # are not people owed credit; match on prefix since the model name and
        # its context window change release to release.
        if not_a_person(name, email):
            return
        handle = amap.get(email.lower()) or amap.get(name.strip().lower())
        if not handle:
            m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", email)
            handle = m.group(1) if m else name.strip()
        if any(handle.lower().startswith(skip) for skip in SKIP):
            return
        found.setdefault(handle, why)

    log = sh("git", "log", f"{since}..HEAD", "--format=%an%x1f%ae%x1f%s%x1f%b%x00")
    work = [parts for parts in (entry.lstrip("\n").split("\x1f", 3) for entry in log.split("\0"))
            if len(parts) == 4 and not restoring(parts[2], parts[0], parts[1])]
    for author, email, _, _ in work:
        add(author, email, "commit author")
    for _, _, _, body in work:
        for name, addr in re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", body):
            add(name, addr, "co-author trailer")
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
    ids = noreply_ids()
    lost = []
    broken: list[tuple[str, str]] = []
    credited: set[str] = set()

    def login(name: str, addr: str) -> str:
        nm = NOREPLY.search(addr)
        return (amap.get(addr.lower()) or amap.get(name.strip().lower()) or (nm.group(1) if nm else name.strip())).lower()

    def who(name: str, addr: str) -> str:
        nm = re.search(r"(\d+)\+([^@<>\s]+)@users\.noreply\.github\.com", addr)
        if not nm:
            return amap.get(addr.lower(), "").lower() if "@" in addr else ""
        known = ids.get(nm.group(2).lower())
        if known is not None and known != nm.group(1):
            broken.append((nm.group(2).lower(), f"uses {addr}, but @{nm.group(2)} is id {known} "
                                                 f"in AUTHOR_MAP; GitHub credits whoever owns id {nm.group(1)}"))
        return nm.group(2).lower() if known == nm.group(1) else ""

    log = sh("git", "log", f"{since}..HEAD",
             "--format=%h%x1f%an%x1f%ae%x1f%(trailers:key=Co-authored-by,unfold)%x1f%B%x00")
    for entry in log.split("\0"):
        parts = entry.strip().split("\x1f", 4)
        if len(parts) < 5:
            continue
        sha, name, email, parsed, body = parts
        m = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", email)
        author = (amap.get(email.lower()) or amap.get(name.lower()) or (m.group(1) if m else name)).lower()
        trailers = re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", parsed)
        mark = len(broken)
        credited.add(who(name, email))
        credited.update(who(n, e) for n, e in trailers)
        broken[mark:] = [(login, f"{sha} {why}") for login, why in broken[mark:]]
        for co_name, co_email in re.findall(r"(?im)^\s*co-authored-by:\s*(.+?)\s*<([^>]+)>", body):
            if (co_name, co_email) not in trailers and not not_a_person(co_name, co_email):
                broken.append((login(co_name, co_email),
                               f"{sha} has 'Co-authored-by: {co_name}' outside the final trailer block; GitHub ignores it"))
        if restoring(body.split("\n", 1)[0], name, email):
            continue
        for handle in re.findall(r"(?i)harvested from PR #\d+ by @([A-Za-z0-9-]+)", body):
            if handle.lower() != author:
                lost.append(f"  {sha} harvested @{handle} but is authored by {name}")
            broken.append((amap.get(handle.lower(), handle).lower(),
                           f"{sha} harvested @{handle} with neither authorship nor a Co-authored-by trailer for them"))
        if any(author.startswith(skip) for skip in SKIP):
            for co_name, co_email in re.findall(r"(?im)^co-authored-by:\s*(.+?)\s*<([^>]+)>", body):
                cm = re.search(r"\d+\+([^@<>\s]+)@users\.noreply\.github\.com", co_email)
                human = amap.get(co_email.lower()) or amap.get(co_name.strip().lower()) or (cm.group(1) if cm else None)
                low = co_name.strip().lower()
                if human and not any(h.startswith(skip) for skip in SKIP for h in (human.lower(), low)) \
                        and "[bot]" not in co_email.lower() and not co_email.lower().endswith(("noreply@anthropic.com", "noreply@openai.com")):
                    lost.append(f"  {sha} credits @{human} only as a co-author; authored by {name}")
    if lost:
        print("\nCommits whose human contributor is not the commit author (GitHub counts them only through a\n"
              "well-formed Co-authored-by trailer whose address is linked to their account):\n")
        print("\n".join(lost))
    broken = [(login, why) for login, why in broken if login.lower() not in credited]
    if broken:
        print("\nCredit GitHub cannot see:\n")
        print("\n".join(f"  {why}" for _, why in broken))
        print("\nPut each contributor in the final trailer block as\n"
              "'Co-authored-by: Name <ID+login@users.noreply.github.com>', with no blank line between trailers,\n"
              "using the id from `gh api users/<login>`. A later commit can carry the trailer when history cannot change.\n"
              "Credit is taken only from such an address, or from a raw email listed in .github/AUTHOR_MAP; add the\n"
              "email a contributor commits with there once it is linked to their account.")
    if missing:
        print("\nUncredited work landed in this release:\n")
        print("\n".join(missing))
        print(
            "\nAdd each to docs/CONTRIBUTORS.md (canonical), the CHANGELOG's\n"
            "Contributors block, and web/lib/release-credits.ts. Harvested work\n"
            "is still their work -- see AGENTS.md 'Landing other people's work'."
        )
        return 1
    if broken:
        return 1
    print("Every contributor in the window is credited on all three surfaces.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
