#!/usr/bin/env python3
"""Ratchet on `#[allow(dead_code)]` so the wall can shrink but never regrow (#4785).

Why this exists rather than a one-time sweep:

    issue filed   2026-07-??   464 attributes / 143 files
    audit         2026-07-26   426 / 111
    audit         2026-07-28   481 / 155

The sweep was working and the count still went *up*, because two large landings
added state whose accessors only their own tests read. A sweep is a snapshot; a
budget is a direction. This gate makes the number a one-way door.

It deliberately does NOT judge whether any individual attribute is justified —
plenty are. It only refuses to let the total rise, which is the property the
issue actually needs and the only one that can be checked mechanically.

Note the blind spot this compensates for: CI's clippy runs without
`--all-targets`, so it never lints `cfg(test)` or integration-test code. A prior
strip-and-check measured 197 attributes alive *only* because a test references
them — exactly the ones a test-blind lint can never adjudicate.

Usage:
    python3 scripts/check-dead-code-budget.py           # enforce
    python3 scripts/check-dead-code-budget.py --update  # rewrite the budget file
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CRATES_DIR = REPO_ROOT / "crates"
BUDGET_PATH = REPO_ROOT / "scripts" / "dead-code-budget.json"

# Matches `#[allow(dead_code)]`, `#![allow(dead_code)]`, and combined forms with
# `dead_code` in any position of the lint list, including lists that span lines,
# like `#[allow(async_fn_in_trait, dead_code)]`.
ALLOW_PATTERN = re.compile(r"\ballow\(\s*(?:[\w:]+\s*,\s*)*dead_code\b")
# `#[expect(dead_code)]` suppresses exactly the same lint. Counting only the
# `allow` spelling made the gate blind: rewriting an `allow` as an `expect`
# lowered the headline number by the full count while removing no dead code
# at all (#6241). `137fb70a9` did this 92 times and the budget recorded a
# 115-point "improvement" for a real reduction of 21.
EXPECT_PATTERN = re.compile(r"\bexpect\(\s*(?:[\w:]+\s*,\s*)*dead_code\b")


def strip_line_comments(text: str) -> str:
    return "\n".join(
        line for line in text.split("\n") if not line.lstrip().startswith("//")
    )


def measure() -> tuple[int, int, dict[str, dict[str, int]]]:
    """Return (allow total, expect total, per-crate {allow, expect} counts).

    Both spellings suppress the same lint, so the gate ratchets on their sum.
    They are still reported separately because `expect` is the better
    attribute — it errors when the lint stops firing, so it cannot rot
    silently — and a sweep converting `allow` into `expect` is real progress
    even though it leaves the total unchanged.
    """
    per_crate: dict[str, dict[str, int]] = {}
    allow_total = 0
    expect_total = 0
    for path in sorted(CRATES_DIR.rglob("*.rs")):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        code = strip_line_comments(text)
        allows = len(ALLOW_PATTERN.findall(code))
        expects = len(EXPECT_PATTERN.findall(code))
        if not allows and not expects:
            continue
        crate = path.relative_to(CRATES_DIR).parts[0]
        entry = per_crate.setdefault(crate, {"allow": 0, "expect": 0})
        entry["allow"] += allows
        entry["expect"] += expects
        allow_total += allows
        expect_total += expects
    return allow_total, expect_total, per_crate


def load_budget() -> dict:
    if not BUDGET_PATH.exists():
        sys.exit(f"missing budget file: {BUDGET_PATH.relative_to(REPO_ROOT)}")
    return json.loads(BUDGET_PATH.read_text(encoding="utf-8"))


def write_budget(
    allow_total: int, expect_total: int, per_crate: dict[str, dict[str, int]]
) -> None:
    payload = {
        "_comment": (
            "Ceiling for dead-code suppressions across crates/, counting both "
            "`#[allow(dead_code)]` and `#[expect(dead_code)]`. `total` is the "
            "sum and is the ratcheted figure. It may go down freely; raising "
            "it needs a reviewer to say why in the PR. Regenerate with: "
            "python3 scripts/check-dead-code-budget.py --update"
        ),
        "_issue": "https://github.com/codewhale-hq/CodeWhale/issues/4785",
        "_expect_blind_spot": (
            "Until #6241 this gate counted only the `allow` spelling, so "
            "rewriting an allow as an expect lowered the number without "
            "removing any dead code. The ceiling was re-based to the true "
            "combined count when that was fixed; it is not a regression."
        ),
        "total": allow_total + expect_total,
        "allow_total": allow_total,
        "expect_total": expect_total,
        "per_crate": dict(sorted(per_crate.items())),
    }
    BUDGET_PATH.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the budget file from the working tree",
    )
    args = parser.parse_args()

    allow_total, expect_total, per_crate = measure()
    total = allow_total + expect_total

    if args.update:
        write_budget(allow_total, expect_total, per_crate)
        rel = BUDGET_PATH.relative_to(REPO_ROOT)
        print(
            f"[dead-code-budget] wrote {rel}: total={total} "
            f"(allow={allow_total}, expect={expect_total})"
        )
        return 0

    budget = load_budget()
    ceiling = int(budget["total"])

    if total > ceiling:
        print(
            f"[dead-code-budget] FAIL: {total} dead-code suppressions "
            f"(allow={allow_total}, expect={expect_total}), "
            f"budget is {ceiling} (+{total - ceiling}).",
            file=sys.stderr,
        )
        print("", file=sys.stderr)
        print("Per crate now vs. budget:", file=sys.stderr)
        recorded = budget.get("per_crate", {})
        for crate in sorted(set(per_crate) | set(recorded)):
            entry = per_crate.get(crate, {"allow": 0, "expect": 0})
            now = entry["allow"] + entry["expect"]
            was_entry = recorded.get(crate, 0)
            # Tolerate the pre-#6241 shape, where each crate mapped to a bare
            # `allow` count rather than an {allow, expect} pair.
            if isinstance(was_entry, dict):
                was = int(was_entry.get("allow", 0)) + int(was_entry.get("expect", 0))
            else:
                was = int(was_entry)
            marker = "  <-- grew" if now > was else ""
            print(
                f"  {crate:<16} {now:>4}  (allow {entry['allow']}, "
                f"expect {entry['expect']}; budget {was}){marker}",
                file=sys.stderr,
            )
        print("", file=sys.stderr)
        print(
            "Either delete the dead item, or narrow the attribute to the one item\n"
            "that needs it instead of a whole module. If the growth is genuinely\n"
            "justified, run `python3 scripts/check-dead-code-budget.py --update`\n"
            "and say why in the PR description — the point of this gate is that\n"
            "raising the number is a visible decision, not an accident.",
            file=sys.stderr,
        )
        return 1

    if total < ceiling:
        print(
            f"[dead-code-budget] {total} suppressions "
            f"(allow={allow_total}, expect={expect_total}), budget {ceiling} "
            f"({ceiling - total} under). Lower the budget to lock in the win:\n"
            f"  python3 scripts/check-dead-code-budget.py --update"
        )
        return 0

    print(
        f"[dead-code-budget] PASS: {total} suppressions "
        f"(allow={allow_total}, expect={expect_total}), exactly at budget."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
