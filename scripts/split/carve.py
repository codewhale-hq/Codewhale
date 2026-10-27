#!/usr/bin/env python3


from __future__ import annotations

import argparse
import collections
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import splitlib as sl
from splitlib import REPO_ROOT, Change, Fingerprint, SplitError

mg = sl.load_module_graph()

ITEM_RE = re.compile(
    r"(?P<vis>pub(?:\s*\([^)]*\))?\s+)?(?:async\s+|unsafe\s+|const\s+|extern\s+(?:\"[^\"]*\"\s+)?)*"
    r"(?P<kind>fn|struct|enum|union|trait|type|static|const|mod|use|impl|macro_rules!)\b\s*(?P<name>[A-Za-z_]\w*)?"
)
FMT_NAME = re.compile(r"\{([A-Za-z_]\w*)(?::[^}]*)?\}")
TOK = re.compile(r"[A-Za-z_]\w*")
ATTR_LINE = re.compile(r"(?m)^[ \t]*(#\[[^\]\n]*\])[ \t]*$")


class Item:
    def __init__(self, start, end, kind, name, vis, first, last, has_body, raw_start):
        self.start, self.end, self.kind, self.name, self.vis = start, end, kind, name, vis
        self.first, self.last, self.has_body, self.raw_start = first, last, has_body, raw_start


def parse_items(text: str, masked: str, a: int = 0, b: int | None = None) -> list[Item]:
    b = len(text) if b is None else b
    spans, depth, item_start, i = [], 0, a, a
    while i < b:
        c = masked[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                j = i + 1
                while j < b and masked[j] in " \t":
                    j += 1
                if j < b and masked[j] == ";":
                    i = j
                spans.append((item_start, i + 1))
                item_start = i + 1
        elif c == ";" and depth == 0:
            spans.append((item_start, i + 1))
            item_start = i + 1
        i += 1
    items = []
    for s, e in spans:
        seg = masked[s:e]
        lead = len(seg) - len(seg.lstrip())
        if s + lead >= e:
            continue
        head = re.sub(r"(?s)^(?:\s*#\[[^\]]*\])*\s*", "", seg[lead:])
        m = ITEM_RE.match(head)
        kind = m.group("kind") if m else "?"
        name = m.group("name") if m and kind not in ("use", "impl") else None
        vis = (m.group("vis") or "").strip() if m else ""
        first = text.count("\n", 0, s + lead) + 1
        last = text.count("\n", 0, e - 1) + 1
        items.append(Item(s + lead, e, kind, name, vis, first, last, "{" in head, s))
    return items


def select(items: list[Item], ranges: list[list[int]], label: str) -> list[Item]:
    chosen: dict[int, Item] = {}
    for a, b in ranges:
        if a < 1 or b < a:
            raise SplitError(f"{label}: invalid range {a}-{b}")
        selected_before = len(chosen)
        for it in items:
            first_in = a <= it.first <= b
            last_in = a <= it.last <= b
            if first_in and last_in:
                chosen[it.start] = it
            elif first_in or last_in or (it.first < a and it.last > b):
                raise SplitError(f"{label}: range {a}-{b} cuts the item at lines {it.first}-{it.last} ({it.kind} {it.name})")
        if len(chosen) == selected_before:
            raise SplitError(f"{label}: range {a}-{b} selects no new items")
    return [chosen[k] for k in sorted(chosen)]


def code_tokens(text: str, masked: str) -> set[str]:
    toks = set(TOK.findall(masked))
    for m in re.finditer(r'"((?:\\.|[^"\\])*)"', text):
        toks.update(FMT_NAME.findall(m.group(1)))
    return toks


def leaf_spec(bound: str, path: tuple[str, ...]) -> tuple[tuple[str, ...], str]:
    if path[-1] == "self":
        path = path[:-1]
    leaf = path[-1] if bound == path[-1] else f"{path[-1]} as {bound}"
    return tuple(path[:-1]), leaf


def widen_vis(item_text: str, it: Item, target: str) -> str:
    if it.kind in ("use", "impl", "mod", "macro_rules!", "?") or it.vis:
        return item_text
    m = re.search(
        r"(?m)^(?P<kw>(?:async\s+|unsafe\s+|const\s+|extern\s+(?:\"[^\"]*\"\s+)?)*(?:fn|struct|enum|union|trait|type|static|const)\b)",
        item_text,
    )
    if not m:
        return item_text
    return item_text[: m.start("kw")] + target + " " + item_text[m.start("kw") :]


class Carver:
    def __init__(self, rel: str, plan: dict):
        self.rel, self.plan = rel, plan
        source = REPO_ROOT / rel
        if Path(rel).is_absolute() or not source.resolve().is_relative_to(REPO_ROOT.resolve()):
            raise SplitError("source file must be inside the repository")
        self.target_vis = plan.get("vis", "pub(crate)")
        if self.target_vis not in ("pub", "pub(crate)"):
            raise SplitError("visibility must be pub or pub(crate)")
        self.text = sl.read_text(rel)
        self.fp = Fingerprint.of(REPO_ROOT / rel)
        self.masked = mg.lex_mask(self.text)
        self.items = parse_items(self.text, self.masked)
        p = Path(rel)
        self.mod_dir = p.parent if p.name in ("lib.rs", "main.rs", "mod.rs") else p.parent / p.stem
        self.report: dict = {"file": rel, "modules": {}, "dropped_parent_uses": []}

    def item_text(self, it: Item) -> str:
        a = it.raw_start
        if a > 0 and self.text[a - 1] != "\n":
            nl = self.text.find("\n", a)
            a = it.start if nl < 0 or nl >= it.start else nl + 1
        b = self.text.find("\n", it.end - 1)
        text = self.text[a : (len(self.text) if b < 0 else b + 1)]
        if a == 0:
            text = re.sub(r"\A(?:[ \t]*//![^\n]*\n)+", "", text)
        return text.lstrip("\n")

    def tokens(self, it: Item) -> set[str]:
        return code_tokens(self.text[it.start : it.end], self.masked[it.start : it.end])

    def parent_use(self, it: Item) -> dict:
        raw = self.text[it.start : it.end]
        seg = self.masked[it.start : it.end]
        attrs = ATTR_LINE.findall(raw.split("use ", 1)[0])
        body = re.sub(r"^(?:pub(?:\s*\([^)]*\))?\s+)?use\s+", "", re.sub(r"(?s)^(?:\s*#\[[^\]]*\])*\s*", "", seg.strip())).rstrip(";").strip()
        leaves = [(bound, path) for bound, path in mg.use_leaves(body) if bound != "*"]
        return {"item": it, "leaves": leaves, "attrs": attrs, "glob": body.endswith("*")}

    def run(self) -> None:
        modules, test_plan = self.plan["modules"], self.plan.get("tests")
        groups = [modules + ([test_plan] if test_plan else [])]
        if test_plan:
            groups.append(test_plan["children"])
        for group in groups:
            names = [entry["name"] for entry in group]
            if len(names) != len(set(names)) or any(not re.fullmatch(r"[A-Za-z_]\w*", name) for name in names):
                raise SplitError("module names must be unique Rust identifiers")
        taken: dict[int, str] = {}
        by_module: dict[str, list[Item]] = {}
        for m in modules:
            chosen = select(self.items, m["ranges"], m["name"])
            for it in chosen:
                if it.start in taken:
                    raise SplitError(f"item at line {it.first} is in both {taken[it.start]} and {m['name']}")
                taken[it.start] = m["name"]
            by_module[m["name"]] = chosen

        tests_item, children, root_items = None, {}, []
        if test_plan:
            cands = [i for i in self.items if i.kind == "mod" and i.name == test_plan["name"] and i.start not in taken]
            if len(cands) != 1:
                raise SplitError(f"expected one inline `mod {test_plan['name']}`, found {len(cands)}")
            tests_item = cands[0]
            seg = self.masked[tests_item.start : tests_item.end]
            open_i = tests_item.start + seg.index("{")
            inner = parse_items(self.text, self.masked, open_i + 1, tests_item.end - 1)
            child_owner: dict[int, str] = {}
            for child in test_plan["children"]:
                selected = select(inner, child["ranges"], f"tests::{child['name']}")
                for it in selected:
                    if it.start in child_owner:
                        raise SplitError(f"test item at line {it.first} is assigned more than once")
                    child_owner[it.start] = child["name"]
                children[child["name"]] = selected
            used = {i.start for v in children.values() for i in v}
            root_items = [i for i in inner if i.start not in used]

        parent_items = [i for i in self.items if i.start not in taken and i is not tests_item]
        uses = [self.parent_use(i) for i in parent_items if i.kind == "use"]
        parent_rest = [i for i in parent_items if i.kind != "use"]

        defined: dict[str, str] = {}
        for name, items in by_module.items():
            for it in items:
                if it.name:
                    if it.name in defined:
                        raise SplitError(f"{it.name} defined in both {defined[it.name]} and {name}")
                    defined[it.name] = name
        parent_defined = {i.name for i in parent_rest if i.name}
        tests_root_defined = {i.name for i in root_items if i.name}

        def import_block(tokens: set[str], own: set[str], here: str, external: bool = True) -> str:
            lines: list[str] = []
            by_mod: dict[str, list[str]] = collections.defaultdict(list)
            for tok in sorted(tokens - own):
                if tok in defined and defined[tok] != here:
                    by_mod[f"crate::{defined[tok]}"].append(tok)
                elif tok in tests_root_defined and here.startswith("tests::"):
                    by_mod["crate::tests"].append(tok)
                elif tok in parent_defined and here != "":
                    by_mod["crate"].append(tok)
            for path, names in sorted(by_mod.items()):
                lines.append(f"use {path}::{{{', '.join(sorted(set(names)))}}};")
            if external:
                ext: dict[tuple, list[str]] = collections.defaultdict(list)
                for u in uses:
                    for bound, path in u["leaves"]:
                        if bound in tokens and bound not in own:
                            prefix, leaf = leaf_spec(bound, path)
                            ext[(tuple(u["attrs"]), prefix)].append(leaf)
                for (attrs, prefix), leaves in sorted(ext.items()):
                    body = leaves[0] if len(leaves) == 1 else "{" + ", ".join(sorted(set(leaves))) + "}"
                    stmt = f"use {'::'.join(prefix)}::{body};" if prefix else f"use {body};"
                    lines.append("".join(a + "\n" for a in attrs) + stmt)
            return "\n".join(lines) + ("\n\n" if lines else "")

        files: dict[str, str] = {}
        for name, items in by_module.items():
            body, toks = "", set()
            for it in items:
                body += widen_vis(self.item_text(it), it, self.target_vis).rstrip("\n") + "\n\n"
                toks |= self.tokens(it)
            imports = import_block(toks, {i.name for i in items if i.name}, name)
            files[str(self.mod_dir / f"{name}.rs")] = imports + body.rstrip("\n") + "\n"
            self.report["modules"][name] = {"items": len(items), "lines": body.count("\n"), "imports": imports.count("use ")}

        if test_plan:
            tests_dir = self.mod_dir / test_plan["name"]
            root_body, root_toks = "", set()
            for it in root_items:
                root_body += widen_vis(self.item_text(it), it, self.target_vis).rstrip("\n") + "\n\n"
                root_toks |= self.tokens(it)
            for cname, items in children.items():
                body, toks = "", set()
                for it in items:
                    body += self.item_text(it).rstrip("\n") + "\n\n"
                    toks |= self.tokens(it)
                imports = import_block(toks, {i.name for i in items if i.name}, f"tests::{cname}")
                files[str(tests_dir / f"{cname}.rs")] = imports + body.rstrip("\n") + "\n"
                self.report["modules"][f"tests::{cname}"] = {"items": len(items), "lines": body.count("\n"), "imports": imports.count("use ")}
            decl = "".join(f"mod {c['name']};\n" for c in test_plan["children"])
            imports = import_block(root_toks, {i.name for i in root_items if i.name}, "tests")
            files[str(self.mod_dir / f"{test_plan['name']}.rs")] = imports + decl + "\n" + root_body.rstrip("\n") + "\n"

        parent_tokens: set[str] = set()
        for it in parent_rest:
            parent_tokens |= self.tokens(it)
        pieces: list[tuple[Item, str]] = []
        for it in sorted([*parent_items, *([tests_item] if tests_item else [])], key=lambda i: i.start):
            if it is tests_item:
                attrs = ATTR_LINE.findall(self.item_text(it).split("mod ", 1)[0])
                pieces.append((it, "".join(a + "\n" for a in attrs) + f"mod {test_plan['name']};\n"))
            elif it.kind == "use":
                u = next(u for u in uses if u["item"] is it)
                dropped = [("::".join(p)) for b, p in u["leaves"] if b not in parent_tokens and not u["glob"]]
                if dropped and len(dropped) == len(u["leaves"]):
                    self.report["dropped_parent_uses"].extend(dropped)
                    continue
                if dropped:
                    self.report["dropped_parent_uses"].extend(dropped)
                    keep = [(b, p) for b, p in u["leaves"] if b in parent_tokens]
                    grouped: dict[tuple, list[str]] = collections.defaultdict(list)
                    for b, p in keep:
                        prefix, leaf = leaf_spec(b, p)
                        grouped[prefix].append(leaf)
                    vis = (it.vis + " ") if it.vis else ""
                    stmts = []
                    for prefix, leaves in grouped.items():
                        body = leaves[0] if len(leaves) == 1 else "{" + ", ".join(sorted(leaves)) + "}"
                        stmts.append("".join(a + "\n" for a in u["attrs"]) + f"{vis}use {'::'.join(prefix)}::{body};\n")
                    pieces.append((it, "".join(stmts)))
                else:
                    pieces.append((it, self.item_text(it)))
            else:
                pieces.append((it, self.item_text(it)))

        mod_lines = "".join(f"mod {n};\n" for n in sorted(by_module))
        head = import_block(parent_tokens, parent_defined, "", external=False)
        block = mod_lines + head
        out, inserted, idx = [], False, 0
        lead_end = 0
        for i, (it, _) in enumerate(pieces):
            if it.kind == "use" or (it.kind == "mod" and not it.has_body) or it.kind == "?":
                lead_end = i + 1
            else:
                break
        for i, (it, text) in enumerate(pieces):
            out.append(text.rstrip("\n") + "\n")
            if i + 1 == lead_end:
                out.append(block)
                inserted = True
        if not inserted:
            out.insert(0, block)
        preamble = re.match(r"(?:[ \t]*//![^\n]*\n)+", self.text)
        files[self.rel] = (preamble.group(0) if preamble else "") + "\n".join(out)
        self.files = files


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Carve whole Rust items into child modules with journalled writes.")
    ap.add_argument("file")
    ap.add_argument("--plan", required=True)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--no-format", action="store_true")
    ap.add_argument("--journal-dir")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    sl.require_git_repo()
    carver = Carver(args.file, json.loads(Path(args.plan).read_text(encoding="utf-8")))
    carver.run()
    for rel in carver.files:
        if not (REPO_ROOT / rel).resolve().is_relative_to(REPO_ROOT.resolve()):
            raise SplitError(f"{rel} resolves outside the repository")
        if rel != args.file and (REPO_ROOT / rel).exists():
            raise SplitError(f"{rel} already exists")
    if args.no_format:
        formatted = dict(carver.files)
    else:
        formatted, _ = sl.format_many({rel: (carver.text if rel == args.file else None, text) for rel, text in carver.files.items()})
    changes = [Change(rel, text, carver.fp if rel == args.file else None) for rel, text in sorted(formatted.items())]
    carver.report["files"] = {r: t.count("\n") for r, t in formatted.items()}
    if args.json:
        json.dump(carver.report, sys.stdout, indent=1)
        print()
    else:
        for r, n in sorted(carver.report["files"].items()):
            print(f"{n:6} lines  {r}")
        print(f"dropped parent use leaves: {len(carver.report['dropped_parent_uses'])}")
    if args.dry_run:
        return 0
    jdir = Path(args.journal_dir) if args.journal_dir else sl.default_journal_dir("carve")
    sl.apply_changes(changes, jdir)
    print(f"applied {len(changes)} change(s); journal {jdir}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(sl.run_main(main))
