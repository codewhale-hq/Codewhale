#!/usr/bin/env python3
"""Function-level test partitioner for a module move (S09 pre-split helper).

    python3 scripts/split/testsplit.py config settings oauth config_persistence
    python3 scripts/split/testsplit.py config --file <tests file under the source crate> --json
    python3 scripts/split/testsplit.py config --emit-dir /tmp/split-out

For the files of the named modules that hold tests (`tests.rs`, `*_tests.rs`
and the inline `mod tests` of the module files), classifies every top-level
item as `move` or `stay`:

* an item STAYS when it names a staying source-crate module (`crate::M` with M
  neither moved nor already resident in the destination) or an identifier that
  a `use crate::M::..` import binds from one;
* staying propagates through the in-file call graph: a test that calls a
  helper that stays, stays with it.

`#[test]` functions are counted individually, including those inside a nested
test module (which is classified as a whole). `--emit-dir` writes
`<stem>.move.rs` and `<stem>.stay.rs`: every `use` is kept in both, support
items needed by both sides are duplicated and listed as `shared`, so the
caller decides whether to widen them with `test-support` instead.
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import splitlib as sl

mg = sl.load_module_graph()

TEST_ATTR = re.compile(r"#\[\s*(?:tokio::test|test|rstest|async_std::test)\b")
ITEM_NAME = re.compile(r"\b(?:fn|struct|enum|const|static|type|trait|mod|union)\s+(\w+)")


def items_of(text: str, masked: str, start: int, end: int):
    out, depth, item_start, i = [], 0, start, start
    while i < end:
        c = masked[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                out.append((item_start, i + 1))
                item_start = i + 1
        elif c == ";" and depth == 0:
            out.append((item_start, i + 1))
            item_start = i + 1
        i += 1
    return [(a, b) for a, b in out if masked[a:b].strip()]


def analyse_file(rel: str, stay_modules: set[str], inline_body: tuple[int, int] | None = None):
    text = sl.read_text(rel)
    masked = mg.lex_mask(text)
    a, b = inline_body or (0, len(text))
    spans = items_of(text, masked, a, b)
    items = []
    imports: dict[str, str] = {}
    for sa, sb in spans:
        code = masked[sa:sb]
        stripped = code.strip()
        head = re.sub(r"(?s)^(?:\s*#\[[^\]]*\])*\s*", "", stripped)
        kind = "use" if re.match(r"(?:pub(?:\([^)]*\))?\s+)?use\b", head) else "item"
        name_m = ITEM_NAME.search(head) if kind == "item" else None
        item = {
            "a": sa, "b": sb, "kind": kind, "name": name_m.group(1) if name_m else None,
            "tests": len(TEST_ATTR.findall(code)), "lines": text.count("\n", sa, sb) + 1,
            "tokens": set(re.findall(r"[A-Za-z_]\w*", code)),
            "direct": set(re.findall(r"(?<![\w$])crate::(\w+)", code)) & stay_modules,
        }
        if kind == "use":
            body = head[head.index("use") + 3 : head.rindex(";")] if ";" in head else head
            for bound, path in mg.use_leaves(body):
                if path[0] == "crate" and len(path) > 1:
                    imports[bound] = path[1]
        items.append(item)
    defined = {it["name"]: i for i, it in enumerate(items) if it["name"]}
    for it in items:
        it["via_import"] = {imports[t] for t in it["tokens"] if t in imports} & stay_modules
        it["deps"] = {defined[t] for t in it["tokens"] if t in defined and t != it["name"]}
        it["stay"] = bool(it["direct"] or it["via_import"])
        it["why"] = sorted(it["direct"] | it["via_import"])
    changed = True
    while changed:
        changed = False
        for i, it in enumerate(items):
            if not it["stay"]:
                for d in it["deps"]:
                    if items[d]["stay"]:
                        it["stay"] = True
                        it["why"] = [f"via {items[d]['name']}"] + items[d]["why"][:2]
                        changed = True
                        break
    return text, items


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("modules", nargs="+")
    ap.add_argument("--src-crate", default="tui")
    ap.add_argument("--dest-crate", default="runtime")
    ap.add_argument("--file", action="append", help="classify only these files")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--emit-dir")
    args = ap.parse_args(argv)
    sl.require_git_repo()
    src_dir = REPO = sl.REPO_ROOT / "crates" / args.src_crate / "src"
    dest_dir = sl.REPO_ROOT / "crates" / args.dest_crate / "src"
    moved = set(args.modules)

    def top_modules(d: Path) -> set[str]:
        return {p.name if p.is_dir() else p.stem for p in d.iterdir() if p.is_dir() or p.suffix == ".rs"} - {"lib", "main"}

    stay_modules = top_modules(src_dir) - moved - top_modules(dest_dir)
    files = args.file or []
    inline: dict[str, tuple[int, int]] = {}
    if not files:
        for m in args.modules:
            for rel in sl.tracked_files(f"crates/{args.src_crate}/src/{m}.rs", f"crates/{args.src_crate}/src/{m}"):
                if not rel.endswith(".rs"):
                    continue
                name = Path(rel).name
                if name == "tests.rs" or name.endswith(("_tests.rs", "_test.rs")) or name.startswith("test_cases"):
                    files.append(rel)
                else:
                    text = sl.read_text(rel)
                    masked = mg.lex_mask(text)
                    mo = re.search(r"#\[cfg\(test\)\]\s*mod\s+tests\s*\{", masked)
                    if mo:
                        files.append(rel)
                        inline[rel] = (mo.end(), mg.match_brace(masked, mo.end() - 1))
    report = []
    for rel in files:
        text, items = analyse_file(rel, stay_modules, inline.get(rel))
        tests = [it for it in items if it["tests"]]
        move_tests = sum(it["tests"] for it in tests if not it["stay"])
        stay_tests = sum(it["tests"] for it in tests if it["stay"])
        blockers = collections.Counter(w for it in tests if it["stay"] for w in it["why"][:1])
        helpers_stay = [it["name"] for it in items if it["stay"] and not it["tests"] and it["name"]]
        report.append(
            {
                "file": rel,
                "inline": rel in inline,
                "tests_total": move_tests + stay_tests,
                "tests_move": move_tests,
                "tests_stay": stay_tests,
                "stay_helpers": helpers_stay,
                "blocked_by": dict(blockers.most_common()),
                "lines_move": sum(it["lines"] for it in items if not it["stay"]),
                "lines_stay": sum(it["lines"] for it in items if it["stay"]),
            }
        )
        if args.emit_dir:
            out = Path(args.emit_dir)
            out.mkdir(parents=True, exist_ok=True)
            uses = [it for it in items if it["kind"] == "use"]
            stem = Path(rel).stem
            for side, want in (("move", False), ("stay", True)):
                needed: set[int] = set()
                body = [i for i, it in enumerate(items) if it["kind"] != "use" and it["stay"] == want and it["tests"]]
                stack = list(body)
                while stack:
                    i = stack.pop()
                    if i in needed:
                        continue
                    needed.add(i)
                    stack.extend(items[i]["deps"])
                chosen = [it for it in uses] + [items[i] for i in sorted(needed)]
                (out / f"{stem}.{side}.rs").write_text("".join(text[it["a"] : it["b"]] for it in chosen), encoding="utf-8")
    total = {k: sum(r[k] for r in report) for k in ("tests_total", "tests_move", "tests_stay")}
    if args.json:
        json.dump({"stay_modules": sorted(stay_modules), "files": report, "total": total}, sys.stdout, indent=1)
        print()
    else:
        print(f"staying source-crate modules considered: {len(stay_modules)}")
        for r in report:
            print(f"{r['file']}: {r['tests_total']} tests, move {r['tests_move']}, stay {r['tests_stay']}; stay helpers {len(r['stay_helpers'])}; blocked by {dict(list(r['blocked_by'].items())[:5])}")
        print(f"total: {total['tests_total']} tests, move {total['tests_move']}, stay {total['tests_stay']}")
    return 0


if __name__ == "__main__":
    sys.exit(sl.run_main(main))
