#!/usr/bin/env python3
"""Extraction order for the crate split: which module can leave `crates/tui` next.

Reuses the lexer and module graph from `module_graph.py` and condenses the
production edges into strongly connected components. A module's wave is the
longest chain of components it depends on; wave 0 modules depend on nothing
still waiting to move, so they can be claimed as their own crate now. The
blockers column names what keeps a module out of wave 0.

    python3 scripts/split/partition_report.py            # table
    python3 scripts/split/partition_report.py --json     # machine-readable
    python3 scripts/split/partition_report.py --module sandbox
"""

from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import re
import sys
from pathlib import Path

_spec = importlib.util.spec_from_file_location("module_graph", Path(__file__).with_name("module_graph.py"))
mg = importlib.util.module_from_spec(_spec)
sys.modules["module_graph"] = mg
_spec.loader.exec_module(mg)

_cl_spec = importlib.util.spec_from_file_location("count_lines", Path(__file__).with_name("count_lines.py"))
cl = importlib.util.module_from_spec(_cl_spec)
_cl_spec.loader.exec_module(cl)

ROOT = mg.ROOT_ITEMS


def strict_lines(src_dirs: list[Path]) -> collections.Counter:
    strict: collections.Counter = collections.Counter()
    for src in src_dirs:
        crate_dir = src.parent
        if not crate_dir.is_dir():
            continue
        _, res = cl.classify_crate(str(crate_dir))
        for path, (prod, _test, _other) in res.items():
            rel = Path(path).relative_to(src) if Path(path).is_relative_to(src) else None
            if rel is not None:
                strict[mg.top_module(rel.as_posix())] += prod
    return strict


def analyse(tui_src: Path = mg.TUI_SRC, runtime_src: Path = mg.RUNTIME_SRC, non_test: bool = False) -> dict:
    tui, runtime, refs = mg.load_graph(tui_src, runtime_src)
    resident = set(runtime.modules)
    modules = (tui.modules | runtime.modules) | {ROOT}
    prod_lines: collections.Counter = collections.Counter()
    test_lines: collections.Counter = collections.Counter()
    for crate in (tui, runtime):
        exact, prefixes = mg.test_file_set(crate)
        for rel, (src, _, _) in crate.files.items():
            module = mg.top_module(rel)
            bucket = test_lines if mg.file_is_test(rel, exact, prefixes, crate.production_files) else prod_lines
            bucket[module] += src.count("\n") + 1
    strict = strict_lines([tui_src, runtime_src]) if non_test else None
    out: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    inbound: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for ref in refs:
        if ref.kind != "prod" or ref.src == ref.dst or ref.dst == "extern":
            continue
        out[ref.src][ref.dst] += 1
        inbound[ref.dst][ref.src] += 1
    graph = {m: set(out[m]) & modules for m in modules}
    component = strongly_connected(graph)
    comp_members: dict[int, list[str]] = collections.defaultdict(list)
    for m, c in component.items():
        comp_members[c].append(m)
    comp_edges = {c: {component[d] for m in members for d in graph[m]} - {c} for c, members in comp_members.items()}
    wave: dict[int, int] = {}

    def depth(c: int) -> int:
        if c not in wave:
            wave[c] = 0
            wave[c] = 1 + max((depth(d) for d in comp_edges[c]), default=-1)
        return wave[c]

    for c in comp_members:
        depth(c)
    rows = []
    for m in sorted(modules - {ROOT}):
        c = component[m]
        blockers = [(d, n) for d, n in out[m].most_common() if d in modules and component[d] != c]
        rows.append(
            {
                "module": m,
                "ui": mg.is_ui(m),
                "resident": m in resident,
                "prod_lines": prod_lines[m],
                "test_lines": test_lines[m],
                **({"strict_lines": strict[m]} if strict is not None else {}),
                "out": len(out[m]),
                "in": len(inbound[m]),
                "scc": len(comp_members[c]),
                "wave": wave[c],
                "to_root": out[m].get(ROOT, 0),
                "blockers": blockers,
            }
        )
    sizes = sorted((len(v) for v in comp_members.values()), reverse=True)
    largest = sorted(max(comp_members.values(), key=len))
    return {"rows": rows, "scc_sizes": sizes[:5], "largest_scc": largest, "refs": refs}


def scc_edges(result: dict, top: int, items: int) -> str:
    members = set(result["largest_scc"])
    heavy: collections.Counter = collections.Counter()
    names: dict[tuple[str, str], collections.Counter] = collections.defaultdict(collections.Counter)
    for ref in result["refs"]:
        if ref.kind != "prod" or ref.src == ref.dst or ref.src not in members or ref.dst not in members:
            continue
        key = (ref.src, ref.dst)
        heavy[key] += 1
        for group in re.findall(r"crate::%s::\{([^}]*)\}" % re.escape(ref.dst), ref.text):
            for name in group.split(","):
                if name.strip():
                    names[key][name.strip().split("::")[0].split(" as ")[0]] += 1
        for name in re.findall(r"crate::%s::(\w+)" % re.escape(ref.dst), ref.text):
            names[key][name] += 1
    lines = [f"heaviest production edges inside the {len(members)}-module cycle (src -> dst, refs; most-named items):"]
    for (src, dst), count in heavy.most_common(top):
        shown = ", ".join(f"{n}" for n, _ in names[(src, dst)].most_common(items))
        lines.append(f"  {src:>18} -> {dst:<18}{count:>5}   {shown}")
    both = sum(1 for (a, b) in heavy if (b, a) in heavy) // 2
    lines.append(f"  {len(heavy)} directed module pairs, {both} of them in both directions")
    return "\n".join(lines)


def strongly_connected(graph: dict[str, set[str]]) -> dict[str, int]:
    index: dict[str, int] = {}
    low: dict[str, int] = {}
    on: set[str] = set()
    stack: list[str] = []
    component: dict[str, int] = {}
    counter = 0
    next_component = 0
    for start in sorted(graph):
        if start in index:
            continue
        work = [(start, iter(sorted(graph[start])))]
        index[start] = low[start] = counter
        counter += 1
        stack.append(start)
        on.add(start)
        while work:
            node, children = work[-1]
            advanced = False
            for child in children:
                if child not in index:
                    index[child] = low[child] = counter
                    counter += 1
                    stack.append(child)
                    on.add(child)
                    work.append((child, iter(sorted(graph[child]))))
                    advanced = True
                    break
                if child in on:
                    low[node] = min(low[node], index[child])
            if advanced:
                continue
            work.pop()
            if work:
                parent = work[-1][0]
                low[parent] = min(low[parent], low[node])
            if low[node] == index[node]:
                while True:
                    member = stack.pop()
                    on.discard(member)
                    component[member] = next_component
                    if member == node:
                        break
                next_component += 1
    return component


def render(result: dict, only: str | None) -> str:
    rows = [r for r in result["rows"] if not only or r["module"] == only]
    rows.sort(key=lambda r: (r["ui"], r["wave"], -r["prod_lines"]))
    has_strict = any("strict_lines" in r for r in rows)
    strict_head = f"{'strict':>9}" if has_strict else ""
    lines = [
        f"largest strongly connected components: {result['scc_sizes']}",
        f"largest SCC ({len(result['largest_scc'])} modules): {', '.join(result['largest_scc'])}",
        "",
        f"{'module':<26}{'wave':>5}{'prod':>9}{strict_head}{'test':>9}{'out':>5}{'in':>5}{'scc':>5} {'root':>5}  blockers (module:refs)",
    ]
    for r in rows:
        tag = "UI " if r["ui"] else ("rt " if r["resident"] else "   ")
        blockers = ", ".join(f"{d}:{n}" for d, n in r["blockers"][:5])
        strict_cell = f"{r.get('strict_lines', 0):>9}" if has_strict else ""
        lines.append(
            f"{tag}{r['module']:<23}{r['wave']:>5}{r['prod_lines']:>9}{strict_cell}{r['test_lines']:>9}"
            f"{r['out']:>5}{r['in']:>5}{r['scc']:>5} {r['to_root']:>5}  {blockers}"
        )
    free = [r for r in result["rows"] if not r["ui"] and r["wave"] == 0 and not r["resident"] and r["to_root"] == 0]
    lines += ["", f"claimable now (wave 0, not UI, no crate-root refs): {len(free)} modules, {sum(r['prod_lines'] for r in free)} production lines"]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--module")
    parser.add_argument("--scc-edges", type=int, metavar="N", help="print the N heaviest edges inside the largest cycle")
    parser.add_argument("--non-test", action="store_true", help="add a strict production column (inline cfg(test) code excluded)")
    parser.add_argument("--tui-src", type=Path, default=mg.TUI_SRC, help="source directory of the crate being split")
    parser.add_argument("--runtime-src", type=Path, default=mg.RUNTIME_SRC, help="source directory of the destination crate")
    args = parser.parse_args()
    result = analyse(args.tui_src, args.runtime_src, args.non_test)
    if args.scc_edges:
        print(scc_edges(result, args.scc_edges, 5))
        return 0
    result.pop("refs")
    print(json.dumps(result, indent=1) if args.json else render(result, args.module))
    return 0


if __name__ == "__main__":
    sys.exit(main())
