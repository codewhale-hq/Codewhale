#!/usr/bin/env python3
"""Measure codewhale-tui at unit granularity and check a target crate map.

    python3 scripts/split/crate_map.py graph [--split-above N] [--json out.json]
    python3 scripts/split/crate_map.py check docs/design/crate-map.toml [--json out.json]

`graph` cuts `crates/tui/src` into units: a module whose production subtree is
larger than `--split-above` (default 12,000 lines) is split into its own file
(`<module>/<self>`) plus one unit per child module, recursively. It then
resolves every `crate::`, `super::` and `self::` path in production code to the
deepest module it names and reports unit sizes and unit-to-unit references.

`check` reads a map that assigns every unit to a target crate and reports, from
the same graph: units left unassigned or assigned twice, each crate's production
lines against the 50,000 ceiling and 10,000 target, the crate dependency graph,
every crate-level cycle with the references that close it (lightest edges
first), and edges that point up the declared layer order. The references it
lists are the work: each must be moved, inverted or deleted before the map can
be executed as file moves.

Limits, stated so nobody mistakes this for the compiler: paths are resolved
textually. Second-hop names (`use super::spec;` then `spec::ToolSpec`), glob
re-exports, macros and `#[path]` includes are not followed, and an inline
non-test `mod` resolves `super::` one level too high. Production lines come
from `count_lines.py`'s strict counter, the one the size gate is defined by. Treat the output as a work list and a
ratchet; `cargo check` stays the authority.

Map format (TOML):

    ceiling = 50000
    target = 10000

    [crates.codewhale-catalog]
    layer = 2
    units = ["pricing", "provider_lake", "tools/*"]

A pattern ending in `/*` claims every unit under that module path, including
the module's own `<self>` unit. Exact names beat prefixes; longer prefixes beat
shorter ones.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import count_lines  # noqa: E402
import module_graph as mg  # noqa: E402

TUI_SRC = mg.TUI_SRC
ROOT = "<root>"
SELF = "<self>"
PATH_RE = re.compile(r"\b(crate|super|self)((?:\s*::\s*(?:super|[A-Za-z_][A-Za-z0-9_]*))+)")


def module_of(rel: str) -> str:
    stem = rel[:-3]
    if stem == "lib":
        return ""
    if stem.endswith("/mod"):
        stem = stem[: -len("/mod")]
    return stem


def parent(module: str) -> str:
    return module.rsplit("/", 1)[0] if "/" in module else ""


def load():
    tui, _, _ = mg.load_graph()
    exact, prefixes = mg.test_file_set(tui)
    files = {}
    for rel, (src, code, stripped) in tui.files.items():
        if rel == "main.rs" or not rel.endswith(".rs"):
            continue
        if mg.file_is_test(rel, exact, prefixes, tui.production_files):
            continue
        files[rel] = (src, code, stripped or code)
    modules = {module_of(rel) for rel in files}
    for m in list(modules):
        while m:
            m = parent(m)
            modules.add(m)
    return files, modules



def resolve(tokens: list[str], base: str, modules: set[str]) -> str:
    cur = base
    for t in tokens:
        if t == "super":
            cur = parent(cur)
            continue
        cand = f"{cur}/{t}" if cur else t
        if cand in modules:
            cur = cand
        else:
            break
    return cur


def build(split_above: int):
    files, modules = load()
    own = collections.Counter()
    _, measured = count_lines.classify_crate(str(TUI_SRC))
    for path, (prod, _, _) in measured.items():
        rel = os.path.relpath(path, TUI_SRC).replace(os.sep, "/")
        if rel in files:
            own[module_of(rel)] += prod
    children = collections.defaultdict(set)
    for m in modules:
        if m:
            children[parent(m)].add(m)

    def subtree(m: str) -> int:
        return own[m] + sum(subtree(c) for c in children[m])

    unit_of: dict[str, str] = {}

    def assign(m: str, top: bool) -> None:
        if top:
            unit_of[""] = ROOT
            for c in children[""]:
                assign(c, False)
            return
        if subtree(m) > split_above and children[m]:
            unit_of[m] = f"{m}/{SELF}"
            for c in children[m]:
                assign(c, False)
        else:
            stack = [m]
            while stack:
                x = stack.pop()
                unit_of[x] = m
                stack.extend(children[x])

    assign("", True)
    sizes = collections.Counter()
    for m, n in own.items():
        sizes[unit_of[m]] += n

    edges = collections.Counter()
    items = collections.defaultdict(collections.Counter)
    for rel, (_, code, stripped) in files.items():
        mod = module_of(rel)
        src_unit = unit_of[mod]
        for match in PATH_RE.finditer(code):
            pos = match.start()
            if stripped[pos] != code[pos]:
                continue
            head = match.group(1)
            tokens = [t.strip() for t in match.group(2).split("::") if t.strip()]
            base = "" if head == "crate" else (parent(mod) if head == "super" else mod)
            dst_mod = resolve(tokens, base, modules)
            dst_unit = unit_of.get(dst_mod, ROOT)
            if dst_unit == src_unit:
                continue
            edges[(src_unit, dst_unit)] += 1
            depth = len(dst_mod.split("/")) if dst_mod else 0
            rest = tokens[depth:] if head == "crate" else tokens
            leaf = next((t for t in rest if t not in ("super", "self")), dst_mod or ROOT)
            items[(src_unit, dst_unit)][leaf] += 1
    return sizes, edges, items


def sccs(nodes, edges):
    graph = collections.defaultdict(list)
    for a, b in edges:
        graph[a].append(b)
    index, low, stack, on, out, counter = {}, {}, [], set(), [], [0]
    sys.setrecursionlimit(1_000_000)

    def visit(v):
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on.add(v)
        for w in graph[v]:
            if w not in index:
                visit(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on.discard(w)
                comp.append(w)
                if w == v:
                    break
            out.append(sorted(comp))

    for v in sorted(nodes):
        if v not in index:
            visit(v)
    return out


def cmd_graph(args) -> int:
    sizes, edges, items = build(args.split_above)
    comps = sccs(set(sizes), edges)
    big = max(comps, key=len)
    report = {
        "split_above": args.split_above,
        "units": [{"unit": u, "prod_lines": n} for u, n in sizes.most_common()],
        "edges": [
            {"src": a, "dst": b, "refs": n, "items": dict(items[(a, b)].most_common(8))}
            for (a, b), n in edges.most_common()
        ],
        "largest_cycle": big,
    }
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=1))
    print(f"units: {len(sizes)}  production lines: {sum(sizes.values())}  unit edges: {len(edges)}  refs: {sum(edges.values())}")
    print(f"largest unit cycle: {len(big)} units, {sum(sizes[u] for u in big)} lines")
    for u, n in sizes.most_common(args.top):
        print(f"  {n:8d}  {u}")
    return 0


def match_units(patterns: list[str], units: set[str]):
    claims = {}
    for p in patterns:
        if p.endswith("/*"):
            base = p[:-2]
            hit = {u for u in units if u == base or u.startswith(base + "/")}
            rank = len(base)
        else:
            hit = {p} & units
            rank = 10**6
        if not hit:
            claims.setdefault(("missing", p), set())
        for u in hit:
            claims.setdefault(u, []).append((rank, p))
    return claims


def cmd_check(args) -> int:
    cfg = tomllib.loads(Path(args.map).read_text())
    ceiling, target = cfg.get("ceiling", 50_000), cfg.get("target", 10_000)
    sizes, edges, items = build(cfg.get("split_above", args.split_above))
    units = set(sizes)
    owner: dict[str, tuple[int, str]] = {}
    duplicates, missing_patterns = [], []
    layer = {}
    for name, spec in cfg.get("crates", {}).items():
        layer[name] = spec.get("layer")
        for key, hits in match_units(spec.get("units", []), units).items():
            if isinstance(key, tuple):
                missing_patterns.append(f"{name}: {key[1]}")
                continue
            rank = max(r for r, _ in hits)
            prev = owner.get(key)
            if prev is None or rank > prev[0]:
                owner[key] = (rank, name)
            elif rank == prev[0] and prev[1] != name:
                duplicates.append(f"{key}: {prev[1]} and {name}")
    crate_of = {u: c for u, (_, c) in owner.items()}
    unassigned = sorted(units - set(crate_of), key=lambda u: -sizes[u])

    crate_size = collections.Counter()
    for u, c in crate_of.items():
        crate_size[c] += sizes[u]
    cedges = collections.Counter()
    cvia = collections.defaultdict(collections.Counter)
    for (a, b), n in edges.items():
        ca, cb = crate_of.get(a), crate_of.get(b)
        if ca and cb and ca != cb:
            cedges[(ca, cb)] += n
            cvia[(ca, cb)][(a, b)] += n
    comps = [c for c in sccs(set(crate_size), cedges) if len(c) > 1]
    upward = [
        (a, b, n) for (a, b), n in cedges.items()
        if layer.get(a) is not None and layer.get(b) is not None and layer[b] > layer[a]
    ]
    oversize = {c: n for c, n in crate_size.items() if n > ceiling}

    print(f"crates: {len(crate_size)}  units: {len(units)}  assigned: {len(crate_of)}  unassigned: {len(unassigned)} ({sum(sizes[u] for u in unassigned)} lines)")
    for p in missing_patterns:
        print(f"  pattern matched nothing: {p}")
    for d in duplicates:
        print(f"  claimed twice: {d}")
    for u in unassigned[: args.top]:
        print(f"  unassigned {sizes[u]:7d}  {u}")
    print("crate sizes (production lines):")
    for c, n in crate_size.most_common():
        flag = "OVER CEILING" if n > ceiling else ("over target" if n > target else "")
        print(f"  {n:8d}  L{layer.get(c)}  {c}  {flag}")
    print(f"crate edges: {len(cedges)}  cross-crate refs: {sum(cedges.values())}")
    cut_total = 0
    for comp in comps:
        inside = {e: n for e, n in cedges.items() if e[0] in comp and e[1] in comp}
        print(f"CYCLE among {comp}:")
        for (a, b), n in sorted(inside.items(), key=lambda kv: kv[1]):
            top = ", ".join(f"{s}->{d}({k})" for (s, d), k in cvia[(a, b)].most_common(4))
            print(f"    {n:5d}  {a} -> {b}   via {top}")
    for a, b, n in sorted(upward, key=lambda t: t[2]):
        cut_total += n
        top = ", ".join(f"{s}->{d}({k})" for (s, d), k in cvia[(a, b)].most_common(4))
        print(f"  UP  {n:5d}  {a}(L{layer[a]}) -> {b}(L{layer[b]})   via {top}")
    print(f"summary: cycles={len(comps)} upward_edges={len(upward)} upward_refs={cut_total} oversize={len(oversize)} unassigned={len(unassigned)} duplicates={len(duplicates)}")
    if args.json:
        Path(args.json).write_text(json.dumps({
            "crate_sizes": dict(crate_size),
            "unassigned": unassigned,
            "duplicates": duplicates,
            "cycles": comps,
            "upward": [
                {"from": a, "to": b, "refs": n, "via": [
                    {"src": s, "dst": d, "refs": k, "items": dict(items[(s, d)].most_common(8))}
                    for (s, d), k in cvia[(a, b)].most_common()
                ]} for a, b, n in sorted(upward, key=lambda t: -t[2])
            ],
            "cycle_edges": [
                {"from": a, "to": b, "refs": n}
                for comp in comps for (a, b), n in cedges.items() if a in comp and b in comp
            ],
        }, indent=1))
    clean = not (comps or upward or oversize or unassigned or duplicates or missing_patterns)
    return 0 if clean else 1


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("graph")
    g.add_argument("--split-above", type=int, default=12_000)
    g.add_argument("--json")
    g.add_argument("--top", type=int, default=40)
    c = sub.add_parser("check")
    c.add_argument("map")
    c.add_argument("--split-above", type=int, default=12_000)
    c.add_argument("--json")
    c.add_argument("--top", type=int, default=25)
    args = ap.parse_args(argv)
    return cmd_graph(args) if args.cmd == "graph" else cmd_check(args)


if __name__ == "__main__":
    raise SystemExit(main())
