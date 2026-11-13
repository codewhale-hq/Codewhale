#!/usr/bin/env python3
"""Count nextest tests so a sharded run can prove it ran every test once.

  list LISTING_JSON         tests the archive selects (nextest list --message-format json)
  junit JUNIT_XML           tests one nextest invocation ran
  check DIRECTORY           per OS, the shard files must cover the expected total exactly

The sharded CI suite splits one archive across jobs. A shard that is skipped,
runs a different filter, or double-counts a partition changes the sum, so the
suite gate fails instead of reporting a smaller run as a pass.
"""
from __future__ import annotations

import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def listed(path: str) -> int:
    suites = json.loads(Path(path).read_text(encoding="utf-8"))["rust-suites"]
    return sum(
        1
        for suite in suites.values()
        for case in suite["testcases"].values()
        if case["filter-match"]["status"] == "matches"
    )


def ran(path: str) -> int:
    return int(ET.parse(path).getroot().get("tests", "0"))


def check(directory: str) -> int:
    files = sorted(Path(directory).rglob("*.json"))
    expected = {}
    shards: dict[str, dict[int, int]] = {}
    for file in files:
        record = json.loads(file.read_text(encoding="utf-8"))
        if file.name.startswith("expected-"):
            expected[record["os"]] = record
        elif file.name.startswith("shard-"):
            slot = shards.setdefault(record["os"], {})
            if record["shard"] in slot:
                print(f"{record['os']}: shard {record['shard']} reported twice")
                return 1
            slot[record["shard"]] = record["tests"]
    if not expected:
        print("no expected-<os>.json found")
        return 1
    failed = False
    for os_name, record in sorted(expected.items()):
        got = shards.get(os_name, {})
        want = set(range(1, record["shards"] + 1))
        total = sum(got.values())
        print(f"{os_name}: archive lists {record['tests']}, shards ran {total} {dict(sorted(got.items()))}")
        if set(got) != want:
            print(f"{os_name}: expected shards {sorted(want)}, found {sorted(got)}")
            failed = True
        if total != record["tests"]:
            print(f"{os_name}: shard total {total} differs from the archive's {record['tests']}")
            failed = True
        if any(count <= 0 for count in got.values()):
            print(f"{os_name}: a shard ran no tests")
            failed = True
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    if len(argv) == 3 and argv[1] == "list":
        print(listed(argv[2]))
        return 0
    if len(argv) == 3 and argv[1] == "junit":
        print(ran(argv[2]))
        return 0
    if len(argv) == 3 and argv[1] == "check":
        return check(argv[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
