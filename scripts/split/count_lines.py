#!/usr/bin/env python3
"""Production/test line counter for the crate split (read-only).

    python3 scripts/split/count_lines.py [crates_dir] [--json out.json] [--crate tui]

Two counters are printed for every crate and gated side by side (FD-07):

* [M] strict production: physical lines of production code, test files and
  `cfg(test)`-only items excluded;
* [F] file level: physical lines of every non-test file, inline `cfg(test)`
  modules included (the design document's method; [F] = [M] + inline test).

A file is TEST when any of:
  A. a path component is `tests`
  B. the basename contains `tests` (tests.rs, foo_tests.rs, tests_foo.rs)
  C. it is declared by a `#[cfg(test)]`-gated `mod x;` (resolved to x.rs or
     x/mod.rs next to the declaring module, recursively everything below)
  D. it carries an inner `#![cfg(test)]` attribute
  E. a string-literal `include!` from test code pulls it in
Inside non-test files, an item annotated with a test-only cfg
(`#[cfg(test)]`, `#[cfg(all(test, ..))]`, `#[cfg(any(test, ..))]`,
`#[cfg(feature = "test-support")]`) is TEST up to the end of its braces/semicolon.
`benches/`, `examples/` and `build.rs` are counted as OTHER (neither).
Lines are physical lines (newline count). Accuracy on tui is about +/-2%.
"""
import json
import os
import re
import sys
from collections import defaultdict

TOKEN = re.compile(
    r"""//[^\n]*"""
    r"""|/\*.*?\*/"""
    r"""|b?r(?P<h>\#*)".*?"(?P=h)"""
    r"""|b?"(?:\\.|[^"\\])*\""""
    r"""|'(?:\\.[^']*|[^\\'])'""",
    re.S,
)


def blank(s: str) -> str:
    return re.sub(r"[^\n]", " ", s)


def mask(src: str):
    def comments_only(m):
        t = m.group(0)
        if t.startswith("//") or t.startswith("/*"):
            return blank(t)
        return t

    def everything(m):
        t = m.group(0)
        if t.startswith("//") or t.startswith("/*"):
            return blank(t)
        return t[0] + blank(t[1:-1]) + t[-1] if len(t) >= 2 else t

    return TOKEN.sub(comments_only, src), TOKEN.sub(everything, src)


CFG_OPEN = re.compile(r"#\s*\[\s*cfg\s*\(")
CFG_TOK = re.compile(r'\s*(?:(?P<id>[A-Za-z_][A-Za-z0-9_]*)|(?P<str>"[^"]*")|(?P<p>[(),=]))')


def cfg_possible(expr: str):
    toks = [m for m in CFG_TOK.finditer(expr)]
    pos = [0]

    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None

    def parse():
        t = toks[pos[0]]
        pos[0] += 1
        name = t.group("id")
        n = peek()
        if n is not None and n.group("p") == "(":
            pos[0] += 1
            kids = []
            while True:
                n = peek()
                if n is None:
                    break
                if n.group("p") == ")":
                    pos[0] += 1
                    break
                if n.group("p") == ",":
                    pos[0] += 1
                    continue
                kids.append(parse())
            if name == "not":
                return {not v for v in kids[0]} if kids else {True, False}
            if name == "all":
                r = set()
                if all(True in k for k in kids):
                    r.add(True)
                if any(False in k for k in kids):
                    r.add(False)
                return r or {False}
            if name == "any":
                r = set()
                if any(True in k for k in kids):
                    r.add(True)
                if all(False in k for k in kids):
                    r.add(False)
                return r or {False}
            return {True, False}
        if n is not None and n.group("p") == "=":
            pos[0] += 1
            v = peek()
            pos[0] += 1
            if name == "feature" and v is not None and v.group("str") == '"test-support"':
                return {False}
            return {True, False}
        if name == "test":
            return {False}
        return {True, False}

    try:
        return parse()
    except Exception:
        return {True, False}


INCLUDE = re.compile(r'\binclude!\s*\(\s*"([^"]+)"\s*\)')
PATH_ATTR = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]')
INNER_CFG = re.compile(r"#\s*!\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
ATTR_CLOSE = re.compile(r"\s*\]")
ATTR = re.compile(r"\s*#\s*\[[^\]]*\]")
MODDECL = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
BR = re.compile(r"[{}]")


ITEM_KW = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+|unsafe\s+|const\s+|extern\s+(?:\"[^\"]*\"\s+)?)*"
    r"(?:mod|fn|impl|use|struct|enum|trait|type|static|const|macro_rules|extern|let|union)\b"
)
FALLBACK = []


def item_end(m2: str, j: int) -> int:
    depth = 0
    n = len(m2)
    k = j
    comma_ends = not ITEM_KW.match(m2, j) and not m2.startswith("{", j)
    if comma_ends:
        FALLBACK.append(j)
    while k < n:
        c = m2[k]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth <= 0 and c == ";":
            return k
        elif depth <= 0 and comma_ends and c == ",":
            return k
        elif depth <= 0 and c == "{":
            d = 0
            for b in BR.finditer(m2, k):
                if b.group(0) == "{":
                    d += 1
                else:
                    d -= 1
                    if d == 0:
                        return b.start()
            return n - 1
        k += 1
    return n - 1


def analyse_file(path: str, src: str):
    """returns (test_ranges[(a,b) char offsets], cfg_test_mod_names, whole_file_test)"""
    m1, m2 = mask(src)
    if INNER_CFG.search(m2) or any(
        INNER_CFG.match(m1, mm.start()) for mm in re.finditer(r"#\s*!", m2)
    ):
        return [], [], True, [(m.start(), m.group(1)) for m in INCLUDE.finditer(m1) if m2[m.start()] == "i"]
    ranges = []
    mods = []
    covered = -1
    spans = []
    for m in CFG_OPEN.finditer(m1):
        if m2[m.start()] != "#":
            continue
        depth = 1
        k = m.end()
        while k < len(m1) and depth:
            if m1[k] == "(":
                depth += 1
            elif m1[k] == ")":
                depth -= 1
            k += 1
        close = ATTR_CLOSE.match(m1, k)
        if not close:
            continue
        expr = m1[m.end():k - 1]
        if not re.search(r"\btest\b|test-support", expr):
            continue
        if True in cfg_possible(expr):
            continue
        spans.append((m.start(), close.end()))
    for ms, me in spans:
        if ms <= covered:
            continue
        j = me
        path_attr = None
        while True:
            a = ATTR.match(m2, j)
            if not a:
                break
            pm = PATH_ATTR.match(m1, j + (len(m1[j:]) - len(m1[j:].lstrip())))
            if pm:
                path_attr = pm.group(1)
            j = a.end()
        md = MODDECL.match(m2, j)
        if md:
            mods.append(("path:" + path_attr) if path_attr else md.group(1))
            end = md.end()
        else:
            while j < len(m2) and m2[j].isspace():
                j += 1
            end = item_end(m2, j)
        ranges.append((ms, end))
        covered = end
    incs = [(m.start(), m.group(1)) for m in INCLUDE.finditer(m1) if m2[m.start()] == "i"]
    return ranges, mods, False, incs


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def collect(crate_dir):
    files = []
    for dp, dn, fn in os.walk(crate_dir):
        dn[:] = [d for d in dn if d not in ("target", "node_modules", ".git")]
        for f in fn:
            if f.endswith(".rs"):
                files.append(os.path.join(dp, f))
    return sorted(files)


def classify_crate(crate_dir, tui_style=False):
    files = collect(crate_dir)
    info = {}
    for p in files:
        src = read(p)
        rel = os.path.relpath(p, crate_dir)
        info[p] = dict(rel=rel, src=src, nl=src.count("\n") + (0 if src.endswith("\n") or not src else 1))
    test_files = set()
    other_files = set()
    cfg_mods = {}
    for p, d in info.items():
        rel = d["rel"]
        parts = rel.split("/")
        base = parts[-1]
        if parts[0] in ("benches", "examples") or rel == "build.rs":
            other_files.add(p)
            continue
        if "tests" in parts[:-1] or "tests" in base:
            test_files.add(p)
    for p, d in info.items():
        if p in other_files:
            continue
        ranges, mods, whole, incs = analyse_file(p, d["src"])
        d["ranges"] = ranges
        d["incs"] = incs
        if whole:
            test_files.add(p)
        base = os.path.basename(p)
        stem = base[:-3]
        mdir = os.path.dirname(p) if base in ("mod.rs", "lib.rs", "main.rs") else os.path.join(os.path.dirname(p), stem)
        for name in mods:
            if name.startswith("path:"):
                cands = (os.path.normpath(os.path.join(os.path.dirname(p), name[5:])),)
            else:
                cands = (os.path.join(mdir, name + ".rs"), os.path.join(mdir, name, "mod.rs"))
            for cand in cands:
                if cand in info:
                    test_files.add(cand)
    changed = True
    while changed:
        changed = False
        for p, d in info.items():
            for off, rel in d.get("incs", []):
                tgt = os.path.normpath(os.path.join(os.path.dirname(p), rel))
                if tgt not in info or tgt in test_files:
                    continue
                if p in test_files or any(a <= off <= b for a, b in d.get("ranges", [])):
                    test_files.add(tgt)
                    changed = True
        for p in list(test_files):
            base = os.path.basename(p)
            stem = base[:-3]
            if base == "mod.rs":
                sub = os.path.dirname(p) + os.sep
            else:
                sub = os.path.join(os.path.dirname(p), stem) + os.sep
            for q in info:
                if q.startswith(sub) and q not in test_files and q not in other_files:
                    test_files.add(q)
                    changed = True
    res = {}
    for p, d in info.items():
        nl = d["nl"]
        if p in other_files:
            res[p] = (0, 0, nl)
        elif p in test_files:
            res[p] = (0, nl, 0)
        else:
            src = d["src"]
            tl = 0
            for a, b in d.get("ranges", []):
                tl += src.count("\n", a, b) + 1
            tl = min(tl, nl)
            res[p] = (nl - tl, tl, 0)
    return info, res


def summarise(crates_dir):
    out = {}
    tui_detail = None
    for c in sorted(os.listdir(crates_dir)):
        cd = os.path.join(crates_dir, c)
        if not os.path.isfile(os.path.join(cd, "Cargo.toml")):
            continue
        info, res = classify_crate(cd)
        prod = sum(v[0] for v in res.values())
        test = sum(v[1] for v in res.values())
        other = sum(v[2] for v in res.values())
        inline_test = sum(tl for (pl, tl, ol) in res.values() if pl or ol)
        big = []
        for p, d in info.items():
            if res[p][0] or (res[p][1] == 0 and res[p][2] == 0):
                for a, b in d.get("ranges", []):
                    big.append((d["src"].count("\n", a, b) + 1, d["rel"], " ".join(d["src"][a:b + 1].split())[:90]))
        big.sort(reverse=True)
        out[c] = dict(production_lines=prod, file_level_lines=prod + inline_test, test_lines=test,
                      other_lines=other, inline_cfg_test_lines=inline_test, files=len(res),
                      inline_ranges=len(big), inline_top=big[:6])
        if c == "tui":
            tui_detail = (cd, info, res)
    tui_mods = {}
    if tui_detail:
        cd, info, res = tui_detail
        src_root = os.path.join(cd, "src") + os.sep
        mods = defaultdict(lambda: dict(production_lines=0, file_level_lines=0, test_lines=0, files=0, biggest=[]))
        for p, (pl, tl, ol) in res.items():
            if not p.startswith(src_root):
                continue
            rel = p[len(src_root):]
            parts = rel.split("/")
            if len(parts) == 1:
                stem = parts[0][:-3]
                mod = {"lib": "<lib.rs>", "main": "<main.rs>"}.get(stem, stem)
            else:
                mod = parts[0]
            m = mods[mod]
            m["production_lines"] += pl
            m["file_level_lines"] += pl + (tl if (pl or ol) else 0)
            m["test_lines"] += tl
            m["files"] += 1
            if pl:
                m["biggest"].append((pl, rel))
        for m in mods.values():
            m["biggest"] = sorted(m["biggest"], reverse=True)[:3]
        tui_mods = dict(mods)
        out["tui"]["production_outside_src"] = sum(v[0] for p, v in res.items() if not p.startswith(src_root))
    return dict(crates=out, tui_modules=tui_mods, fallback_uses=len(FALLBACK))


def main():
    import argparse

    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("crates_dir", nargs="?", default=os.path.join(repo, "crates"))
    ap.add_argument("--json", metavar="FILE", help="write the full result as JSON ('-' for stdout)")
    ap.add_argument("--crate", action="append", help="print only these crates")
    ap.add_argument("--top", type=int, default=0, help="also list the N largest tui modules")
    args = ap.parse_args()
    result = summarise(args.crates_dir)
    if args.json:
        text = json.dumps(result, indent=1)
        if args.json == "-":
            print(text)
        else:
            with open(args.json, "w", encoding="utf-8") as fh:
                fh.write(text + "\n")
    rows = result["crates"]
    names = args.crate or sorted(rows, key=lambda n: -rows[n]["production_lines"])
    print(f"{'crate':22}{'[M] prod':>11}{'[F] file':>11}{'test':>11}{'inline test':>13}{'files':>7}")
    for n in names:
        r = rows[n]
        print(f"{n:22}{r['production_lines']:>11,}{r['file_level_lines']:>11,}{r['test_lines']:>11,}{r['inline_cfg_test_lines']:>13,}{r['files']:>7}")
    tot_m = sum(r["production_lines"] for r in rows.values())
    tot_f = sum(r["file_level_lines"] for r in rows.values())
    print(f"{'all crates':22}{tot_m:>11,}{tot_f:>11,}")
    if args.top and result["tui_modules"]:
        print("\nlargest tui modules ([M] / [F]):")
        for name, m in sorted(result["tui_modules"].items(), key=lambda kv: -kv[1]["production_lines"])[: args.top]:
            print(f"  {name:28}{m['production_lines']:>9,}{m['file_level_lines']:>10,}")


if __name__ == "__main__":
    main()
