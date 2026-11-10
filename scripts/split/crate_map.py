#!/usr/bin/env python3
"""Measure codewhale-tui, codewhale-engine and codewhale-app-server at unit granularity and check a target crate map.

    python3 scripts/split/crate_map.py graph [--split-above N] [--json out.json]
    python3 scripts/split/crate_map.py check docs/design/crate-map.toml [--json out.json]

`graph` cuts `crates/tui/src`, `crates/engine/src` and `crates/app-server/src` into units: a module whose production subtree is
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

`check MAP --baseline FILE` turns the upward references into a ratchet. Every
crate-crossing unit pair that points up the layer order is keyed
`lower->upper|src_unit->dst_unit`; the check fails on a new pair, a raised
count, or a count that dropped without the baseline being lowered, and still
fails on any unassigned, doubly claimed or unmatched unit and any crate over
the ceiling. `--update` rewrites the baseline and refuses to raise it.
`--baseline-ref REV` (CI: the PR base) also fails when the committed baseline
holds a count above the one at REV, so a hand-edited JSON cannot raise it.

Grouped imports (`use crate::a::{b::C, d}`) are expanded into one path per
leaf. Limits, stated so nobody mistakes this for the compiler: paths are resolved
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
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import count_lines  # noqa: E402
import module_graph as mg  # noqa: E402

SOURCE_ROOTS = (mg.TUI_SRC, mg.ENGINE_SRC, mg.APP_SERVER_SRC)
ROOT = "<root>"
SELF = "<self>"
PATH_RE = re.compile(r"\b(crate|super|self|codewhale_engine|codewhale_app_server|codewhale_tui)((?:\s*::\s*(?:super\b|[A-Za-z_][A-Za-z0-9_]*))+)")
GROUP_RE = re.compile(r"\b(crate|super|self|codewhale_engine|codewhale_app_server|codewhale_tui)((?:\s*::\s*(?:super\b|[A-Za-z_][A-Za-z0-9_]*))*)\s*::\s*\{")


def group_paths(code: str, open_brace: int) -> list[list[str]]:
    depth, i = 0, open_brace
    while i < len(code):
        if code[i] == "{":
            depth += 1
        elif code[i] == "}":
            depth -= 1
            if depth == 0:
                break
        i += 1
    body = code[open_brace + 1 : i]
    out, part, level, start = [], [], 0, 0
    for j, ch in enumerate(body + ","):
        if ch == "{":
            level += 1
        elif ch == "}":
            level -= 1
        elif ch == "," and level == 0:
            part.append(body[start:j].strip())
            start = j + 1
    for p in part:
        if not p or p == "self":
            continue
        brace = p.find("{")
        if brace >= 0:
            prefix = [t.strip() for t in p[:brace].split("::") if t.strip()]
            for sub in group_paths(p, brace):
                out.append(prefix + sub)
        else:
            p = p.split(" as ")[0]
            out.append([t.strip() for t in p.split("::") if t.strip() and t.strip() != "*"])
    return out


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
    files = {}
    for root in SOURCE_ROOTS:
        crate = mg.load_crate(root.parent.name, root)
        exact, prefixes = mg.test_file_set(crate)
        for rel, (src, code, stripped) in crate.files.items():
            if rel == "main.rs" or not rel.endswith(".rs"):
                continue
            if mg.file_is_test(rel, exact, prefixes, crate.production_files):
                continue
            key = {mg.ENGINE_SRC: "engine_root.rs", mg.APP_SERVER_SRC: "app_server_root.rs"}.get(root, rel) if rel == "lib.rs" else rel
            if key in files:
                raise ValueError(f"duplicate module path across source roots: {key}")
            files[key] = (src, code, stripped or code, root / rel)
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
    measured = {}
    for root in SOURCE_ROOTS:
        _, counts = count_lines.classify_crate(str(root))
        measured.update({Path(path): values for path, values in counts.items()})
    for rel, (_, _, _, path) in files.items():
        own[module_of(rel)] += measured.get(path, (0, 0, 0))[0]
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
    for rel, (_, code, stripped, source_path) in files.items():
        mod = module_of(rel)
        src_unit = unit_of[mod]
        found = []
        grouped = set()
        for match in GROUP_RE.finditer(code):
            pos = match.start()
            if stripped[pos] != code[pos]:
                continue
            grouped.add(pos)
            prefix = [t.strip() for t in match.group(2).split("::") if t.strip()]
            for sub in group_paths(code, match.end() - 1):
                found.append((match.group(1), prefix + sub))
        for match in PATH_RE.finditer(code):
            pos = match.start()
            if stripped[pos] != code[pos] or pos in grouped:
                continue
            found.append((match.group(1), [t.strip() for t in match.group(2).split("::") if t.strip()]))
        for head, tokens in found:
            root_names = {"codewhale_engine": "engine_root", "codewhale_app_server": "app_server_root", "codewhale_tui": ""}
            local_root = "engine_root" if source_path.is_relative_to(mg.ENGINE_SRC) else ("app_server_root" if source_path.is_relative_to(mg.APP_SERVER_SRC) else "")
            base = "" if head == "crate" or head in root_names else (parent(mod) if head == "super" else mod)
            dst_mod = resolve(tokens, base, modules) or root_names.get(head, local_root)
            dst_unit = unit_of.get(dst_mod, ROOT)
            if dst_unit == src_unit:
                continue
            edges[(src_unit, dst_unit)] += 1
            depth = len(dst_mod.split("/")) if dst_mod else 0
            rest = tokens[depth:] if head == "crate" or head in root_names else tokens
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


def evaluate(cfg: dict, sizes, edges, items) -> dict:
    ceiling, target = cfg.get("ceiling", 50_000), cfg.get("target", 10_000)
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
    unassigned = sorted(units - set(crate_of), key=lambda u: (-sizes[u], u))

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
    pairs = {
        f"{a}->{b}|{s}->{d}": k
        for a, b, _ in upward for (s, d), k in cvia[(a, b)].items()
    }
    return {
        "ceiling": ceiling, "target": target, "units": units, "sizes": sizes, "items": items,
        "layer": layer, "crate_of": crate_of, "unassigned": unassigned, "duplicates": duplicates,
        "missing_patterns": missing_patterns, "crate_size": crate_size, "cedges": cedges, "cvia": cvia,
        "cycles": comps, "upward": upward, "upward_pairs": pairs,
        "oversize": {c: n for c, n in crate_size.items() if n > ceiling},
    }


def summary(r: dict) -> dict:
    return {
        "cycles": len(r["cycles"]),
        "upward_edges": len(r["upward"]),
        "upward_pairs": len(r["upward_pairs"]),
        "upward_refs": sum(n for _, _, n in r["upward"]),
        "oversize": len(r["oversize"]),
        "unassigned": len(r["unassigned"]),
        "duplicates": len(r["duplicates"]),
        "missing_patterns": len(r["missing_patterns"]),
    }


def structural_problems(r: dict) -> list[str]:
    out = [f"pattern matched nothing: {p}" for p in r["missing_patterns"]]
    out += [f"claimed twice: {d}" for d in r["duplicates"]]
    out += [f"unassigned unit: {u} ({r['sizes'][u]} lines)" for u in r["unassigned"]]
    out += [f"crate over ceiling: {c} {n} > {r['ceiling']}" for c, n in sorted(r["oversize"].items())]
    return out


def compare_upward(baseline: dict, current: dict) -> tuple[list[str], list[str]]:
    rises, drops = [], []
    for key, n in sorted(current.items()):
        b = baseline.get(key)
        if b is None:
            rises.append(f"{key}: new upward reference pair ({n} refs)")
        elif n > b:
            rises.append(f"{key}: {b} -> {n}")
    for key, b in sorted(baseline.items()):
        n = current.get(key, 0)
        if n < b:
            drops.append(f"{key}: {b} -> {n}")
    return rises, drops


def baseline_document(map_path: str, r: dict) -> dict:
    return {
        "_comment": (
            "Coarse-split upward-reference ratchet (docs/design/COARSE_SPLIT.md). Keys are "
            "lower-crate->upper-crate|src-unit->dst-unit. Counts may only go down and no new pair may appear. "
            "Regenerate with python3 scripts/split/crate_map.py check MAP --baseline FILE --update after "
            "removing references; never raise a count by hand."
        ),
        "map": map_path,
        "totals": {"upward_pairs": len(r["upward_pairs"]), "upward_refs": sum(r["upward_pairs"].values())},
        "upward": dict(sorted(r["upward_pairs"].items())),
    }


def git_show(ref: str, path: str) -> str | None:
    commit = subprocess.run(
        ["git", "rev-parse", "--verify", f"{ref}^{{commit}}"],
        cwd=mg.REPO_ROOT, capture_output=True, text=True, check=False,
    )
    if commit.returncode != 0:
        raise ValueError(f"baseline ref {ref!r} is unavailable: {commit.stderr.strip()}")
    rel = Path(path).resolve().relative_to(mg.REPO_ROOT)
    shown = subprocess.run(
        ["git", "show", f"{ref}:{rel.as_posix()}"],
        cwd=mg.REPO_ROOT, capture_output=True, text=True, check=False,
    )
    return shown.stdout if shown.returncode == 0 else None


def ratchet_problems(r: dict, baseline: dict, previous: dict | None = None, ref: str | None = None) -> list[str]:
    problems = []
    if previous is not None:
        raised, _ = compare_upward(previous.get("upward", {}), baseline.get("upward", {}))
        if raised:
            problems.append(
                f"the baseline was raised relative to {ref} (counts only go down; "
                "remove the reference instead of editing the JSON):"
            )
            problems.extend(f"  {x}" for x in raised)
    rises, drops = compare_upward(baseline.get("upward", {}), r["upward_pairs"])
    if rises:
        problems.append("upward references rose (the ratchet only goes down):")
        problems.extend(f"  {x}" for x in rises)
    if drops:
        problems.append("upward references dropped but the baseline was not lowered; rerun with --update and commit it:")
        problems.extend(f"  {x}" for x in drops)
    return problems


def print_report(r: dict, top: int) -> None:
    units, sizes, layer = r["units"], r["sizes"], r["layer"]
    print(f"crates: {len(r['crate_size'])}  units: {len(units)}  assigned: {len(r['crate_of'])}  unassigned: {len(r['unassigned'])} ({sum(sizes[u] for u in r['unassigned'])} lines)")
    for p in r["missing_patterns"]:
        print(f"  pattern matched nothing: {p}")
    for d in r["duplicates"]:
        print(f"  claimed twice: {d}")
    for u in r["unassigned"][:top]:
        print(f"  unassigned {sizes[u]:7d}  {u}")
    print("crate sizes (production lines):")
    for c, n in r["crate_size"].most_common():
        flag = "OVER CEILING" if n > r["ceiling"] else ("over target" if n > r["target"] else "")
        print(f"  {n:8d}  L{layer.get(c)}  {c}  {flag}")
    cedges, cvia = r["cedges"], r["cvia"]
    print(f"crate edges: {len(cedges)}  cross-crate refs: {sum(cedges.values())}")
    for comp in r["cycles"]:
        inside = {e: n for e, n in cedges.items() if e[0] in comp and e[1] in comp}
        print(f"CYCLE among {comp}:")
        for (a, b), n in sorted(inside.items(), key=lambda kv: kv[1]):
            via = ", ".join(f"{s}->{d}({k})" for (s, d), k in cvia[(a, b)].most_common(4))
            print(f"    {n:5d}  {a} -> {b}   via {via}")
    for a, b, n in sorted(r["upward"], key=lambda t: t[2]):
        via = ", ".join(f"{s}->{d}({k})" for (s, d), k in cvia[(a, b)].most_common(4))
        print(f"  UP  {n:5d}  {a}(L{layer[a]}) -> {b}(L{layer[b]})   via {via}")
    s = summary(r)
    print(
        f"summary: cycles={s['cycles']} upward_edges={s['upward_edges']} upward_refs={s['upward_refs']} "
        f"oversize={s['oversize']} unassigned={s['unassigned']} duplicates={s['duplicates']}"
    )


def report_json(r: dict, map_path: str, ratchet: dict | None, clean: bool) -> dict:
    items, cvia = r["items"], r["cvia"]
    return {
        "map": map_path,
        "pass": clean,
        "summary": summary(r),
        "ratchet": ratchet,
        "crate_sizes": dict(r["crate_size"]),
        "crate_edges": [{"from": a, "to": b, "refs": n} for (a, b), n in sorted(r["cedges"].items())],
        "unassigned": r["unassigned"],
        "duplicates": r["duplicates"],
        "missing_patterns": r["missing_patterns"],
        "cycles": r["cycles"],
        "upward": [
            {"from": a, "to": b, "refs": n, "via": [
                {"src": s, "dst": d, "refs": k, "items": dict(items[(s, d)].most_common(8))}
                for (s, d), k in cvia[(a, b)].most_common()
            ]} for a, b, n in sorted(r["upward"], key=lambda t: -t[2])
        ],
        "cycle_edges": [
            {"from": a, "to": b, "refs": n}
            for comp in r["cycles"] for (a, b), n in r["cedges"].items() if a in comp and b in comp
        ],
    }


def cmd_check(args) -> int:
    cfg = tomllib.loads(Path(args.map).read_text())
    sizes, edges, items = build(cfg.get("split_above", args.split_above))
    r = evaluate(cfg, sizes, edges, items)
    print_report(r, args.top)
    structural = structural_problems(r)
    ratchet = None
    problems = list(structural)
    if args.baseline:
        path = Path(args.baseline)
        if args.update:
            if structural:
                print("[crate-map] refusing to write a baseline for a map with structural problems:", file=sys.stderr)
                for p in structural:
                    print(f"  {p}", file=sys.stderr)
                return 1
            if path.is_file():
                rises, _ = compare_upward(json.loads(path.read_text()).get("upward", {}), r["upward_pairs"])
                if rises:
                    print("[crate-map] refusing to raise the baseline:", file=sys.stderr)
                    for x in rises:
                        print(f"  {x}", file=sys.stderr)
                    return 1
            path.write_text(json.dumps(baseline_document(args.map, r), indent=2) + "\n", encoding="utf-8")
            print(f"[crate-map] baseline written: {len(r['upward_pairs'])} pairs, {sum(r['upward_pairs'].values())} refs")
            return 0
        if not path.is_file():
            problems.append(f"missing baseline {path}; run with --update")
        else:
            baseline = json.loads(path.read_text())
            previous = None
            if args.baseline_ref:
                try:
                    text = git_show(args.baseline_ref, str(path))
                except ValueError as error:
                    text = None
                    problems.append(str(error))
                previous = json.loads(text) if text else None
            found = ratchet_problems(r, baseline, previous, args.baseline_ref)
            problems.extend(found)
            ratchet = {
                "baseline": str(path),
                "baseline_ref": args.baseline_ref,
                "baseline_pairs": len(baseline.get("upward", {})),
                "baseline_refs": sum(baseline.get("upward", {}).values()),
                "problems": found,
            }
    else:
        if r["cycles"]:
            problems.append(f"crate cycles: {r['cycles']}")
        if r["upward"]:
            problems.append(f"upward crate edges: {len(r['upward'])}")
    clean = not problems
    if args.json:
        Path(args.json).write_text(json.dumps(report_json(r, args.map, ratchet, clean), indent=1))
    if problems:
        print("[crate-map] FAIL", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    held = " (upward references within the ratchet baseline)" if args.baseline else ""
    print(f"[crate-map] PASS{held}")
    return 0


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
    c.add_argument("--baseline", help="ratchet JSON: upward reference pairs may only go down")
    c.add_argument("--baseline-ref", help="git revision whose committed baseline the current one may not exceed (CI: the PR base)")
    c.add_argument("--update", action="store_true", help="lower the --baseline to the current upward references")
    args = ap.parse_args(argv)
    return cmd_graph(args) if args.cmd == "graph" else cmd_check(args)


if __name__ == "__main__":
    raise SystemExit(main())
