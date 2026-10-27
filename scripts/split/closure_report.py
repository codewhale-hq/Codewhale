#!/usr/bin/env python3
"""Per-crate dependency-closure size report for the crate split.

For every workspace crate: its own Rust lines, the lines in its workspace
dependency closure (what an agent sandbox must hold to build it), and the
lines in its reverse closure (what a change to it can rebuild).

    python3 scripts/split/closure_report.py
    python3 scripts/split/closure_report.py --json
    python3 scripts/split/closure_report.py --sort closure --top 15
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

_spec = importlib.util.spec_from_file_location("count_lines", Path(__file__).with_name("count_lines.py"))
cl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cl)

REPO_ROOT = Path(__file__).resolve().parents[2]


def metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", "--locked"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        sys.exit(out.stderr.strip() or "cargo metadata failed")
    return json.loads(out.stdout)


def rust_lines(root: Path, tests: bool) -> int:
    total = 0
    for path in root.rglob("*.rs"):
        rel = path.relative_to(root).parts
        is_test = (
            "tests" in rel
            or path.name in ("tests.rs", "test_support.rs")
            or path.name.endswith("_tests.rs")
            or path.name.startswith("test_cases_")
        )
        if is_test and not tests:
            continue
        try:
            total += sum(1 for _ in path.open("rb"))
        except OSError:
            pass
    return total


def closure(graph: dict[str, set[str]], start: str) -> set[str]:
    seen: set[str] = set()
    stack = [start]
    while stack:
        node = stack.pop()
        for nxt in graph.get(node, ()):
            if nxt not in seen:
                seen.add(nxt)
                stack.append(nxt)
    return seen


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--sort", choices=["name", "own", "closure", "reverse"], default="closure")
    parser.add_argument("--top", type=int, default=0)
    parser.add_argument("--tests", action="store_true", help="count test files too")
    parser.add_argument(
        "--non-test",
        action="store_true",
        help="size crates by strict production lines (inline cfg(test) code excluded) instead of file-level lines",
    )
    args = parser.parse_args()

    meta = metadata()
    packages = {p["name"]: p for p in meta["packages"] if p["id"] in set(meta["workspace_members"])}
    forward: dict[str, set[str]] = {}
    reverse: dict[str, set[str]] = {name: set() for name in packages}
    for name, pkg in packages.items():
        deps = {
            d["name"]
            for d in pkg["dependencies"]
            if d["name"] in packages and d.get("kind") != "dev"
        }
        forward[name] = deps
        for dep in deps:
            reverse[dep].add(name)

    own = {}
    for name, pkg in packages.items():
        crate_dir = Path(pkg["manifest_path"]).parent
        src = crate_dir / "src"
        if args.non_test:
            _, res = cl.classify_crate(str(crate_dir))
            own[name] = sum(prod for (prod, _t, _o) in res.values())
        else:
            own[name] = rust_lines(src, args.tests) if src.is_dir() else 0

    rows = []
    for name in packages:
        down = closure(forward, name)
        up = closure(reverse, name)
        rows.append(
            {
                "crate": name,
                "own": own[name],
                "closure": own[name] + sum(own[d] for d in down),
                "closure_crates": len(down),
                "reverse": sum(own[u] for u in up),
                "reverse_crates": len(up),
            }
        )
    rows.sort(key=lambda r: r["crate"] if args.sort == "name" else -r[args.sort])
    if args.top:
        rows = rows[: args.top]

    if args.json:
        json.dump(rows, sys.stdout, indent=2)
        print()
        return
    print(f"{'crate':32} {'own':>9} {'closure':>10} {'deps':>5} {'reverse':>10} {'users':>6}")
    for r in rows:
        print(
            f"{r['crate']:32} {r['own']:>9} {r['closure']:>10} {r['closure_crates']:>5} "
            f"{r['reverse']:>10} {r['reverse_crates']:>6}"
        )


if __name__ == "__main__":
    main()
