#!/usr/bin/env python3
"""Move top-level modules from one workspace crate into another (crate split).

    python3 scripts/split/move-modules.py --dry-run --json config oauth
    python3 scripts/split/move-modules.py --require-clean config oauth
    python3 scripts/split/move-modules.py --resolve-reexports --dry-run config
    python3 scripts/split/move-modules.py --restore <journal-dir>

For each named module, moves `crates/<src>/src/<m>.rs` and `<m>/` into
`crates/<dest>/src/` (default tui -> runtime) by plain file operations, never
`git mv`, so the shared index is not touched; and rewrites:

* the `mod <m>;` line (removed from the source lib, `pub mod <m>;` added to the
  destination lib, sorted);
* every `crate::<m>` in the staying source files, plain paths and grouped
  `use crate::{..}` lists, to `<dest crate>::<m>`; unqualified `<m>::` in the
  source lib gets a `use <dest crate>::<m>;`;
* every `<src crate>::<m>` in any other Rust file of the repository;
* `<dest crate>::` to `crate::` inside the moved files;
* path strings in tracked text files (`crates/<src>/src/<m>`, `<src>/src/<m>`
  and segmented `"tui" / "src" / "<m>"`), except `vendor/`, `crates/config/`,
  `CHANGELOG*` and `web/data/*.generated.json` (regenerate those with
  `--derive-facts`).

Safety: git failures are fatal and the root must hold `.git`; `--require-clean`
refuses when any file of the rewrite set is dirty or younger than
`--quiet-minutes`; writes are compare-and-swap with a pre-image journal under
`CW/backups/` and any change between plan and write restores everything;
rewritten Rust is formatted in memory with `rustfmt --edition 2024` on stdin.

`--resolve-reexports` does not move anything: for each facade module it
rewrites `crate::<facade>::NAME` to the upstream path that `pub use` hides.
Manifest and `cfg` planning is report-only; `--apply-cfg` flips the
`#[cfg(test)]` items that code outside the moved set uses to
`any(test, feature = "test-support")`. It never touches `cfg(not(test))`
behaviour forks (test isolation is a decision, not a rewrite).
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
import splitlib as sl
from splitlib import REPO_ROOT, Change, Fingerprint, SplitError

mg = sl.load_module_graph()

MUST_SWEEP = (".gitattributes", "scripts/command-migration-topology.json", "docs/features.toml")
GENERATED_BY_DERIVE_FACTS = (
    "web/lib/facts.generated.ts",
    "web/data/features.generated.json",
    "web/data/agent-docs.generated.json",
)
BUILTIN_STD = {"crate", "self", "super", "std", "core", "alloc", "Self"}


def log(msg: str) -> None:
    print(msg, file=sys.stderr)


def first_segment(masked_entry: str) -> str | None:
    m = re.match(r"\s*(?:::)?([A-Za-z_]\w*)", masked_entry)
    return m.group(1) if m else None


def split_entries(text: str, masked: str, a: int, b: int) -> list[tuple[str, str]]:
    entries, depth, start = [], 0, a
    for i in range(a, b):
        ch = masked[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif ch == "," and depth == 0:
            entries.append((text[start:i], masked[start:i]))
            start = i + 1
    entries.append((text[start:b], masked[start:b]))
    return [(t.strip(), m.strip()) for t, m in entries if m.strip()]


class Stats:
    def __init__(self) -> None:
        self.plain: dict[str, int] = {}
        self.comment_hits = 0
        self.string_hits: list[str] = []
        self.grouped: list[dict] = []
        self.unqualified: list[dict] = []
        self.self_sites = 0
        self.self_skipped = 0
        self.self_files = 0
        self.residue = collections.defaultdict(list)
        self.super_root: list[dict] = []


def split_groups(text: str, rel: str, bases: list[str], mods: set[str], dest_ident: str, stats: Stats) -> str:
    base_alt = "|".join(re.escape(b) for b in bases)
    group_re = re.compile(rf"(?m)^(?P<indent>[ \t]*)(?P<vis>pub(?:\([^)]*\))?[ \t]+)?use[ \t]+(?P<base>{base_alt})::\{{")
    if not group_re.search(text):
        return text
    masked = mg.lex_mask(text)
    out, pos, changed = [], 0, False
    for m in group_re.finditer(masked):
        if m.start() < pos:
            continue
        open_i = m.end() - 1
        close_i = mg.match_brace(masked, open_i)
        semi = masked.find(";", close_i)
        if semi < 0:
            continue
        entries = split_entries(text, masked, open_i + 1, close_i)
        moved = [t for t, mk in entries if first_segment(mk) in mods]
        if not moved:
            continue
        kept = [t for t, mk in entries if first_segment(mk) not in mods]
        indent, vis, base = m.group("indent"), m.group("vis") or "", m.group("base")
        line = masked.count("\n", 0, m.start()) + 1
        stats.grouped.append({"file": rel, "line": line, "moved": [first_segment(mk) for t, mk in entries if t in moved]})
        moved_stmt = f"{indent}{vis}use {dest_ident}::{{{', '.join(moved)}}};"
        if kept:
            attrs = sl.attribute_lines_above(text, m.start(), indent)
            new = f"{indent}{vis}use {base}::{{{', '.join(kept)}}};\n{attrs}{moved_stmt}"
        else:
            new = moved_stmt
        out.append(text[pos : m.start()])
        out.append(new)
        pos = semi + 1
        changed = True
    if not changed:
        return text
    out.append(text[pos:])
    return "".join(out)


def rewrite_paths(text: str, rel: str, bases: list[str], mods: set[str], dest_ident: str, stats: Stats) -> str:
    if not any(b + "::" in text for b in bases):
        return text
    text = split_groups(text, rel, bases, mods, dest_ident, stats)
    alt = "|".join(re.escape(m) for m in sorted(mods))
    base_alt = "|".join(re.escape(b) for b in bases)
    pat = re.compile(rf"(?<![\$\w])(?:{base_alt})::({alt})(?![A-Za-z0-9_])")
    hits = list(pat.finditer(text))
    if not hits:
        return text
    lines = set()
    for m in hits:
        lineno = text.count("\n", 0, m.start()) + 1
        lines.add(lineno)
        prefix = text[text.rfind("\n", 0, m.start()) + 1 : m.start()]
        if prefix.lstrip().startswith(("//", "*", "/*")) or "//" in prefix:
            stats.comment_hits += 1
        elif prefix.count('"') % 2 == 1:
            stats.string_hits.append(f"{rel}:{lineno}")
    stats.plain[rel] = len(lines)
    return pat.sub(lambda m: f"{dest_ident}::{m.group(1)}", text)


def add_unqualified_imports(text: str, rel: str, mods: set[str], dest_ident: str, stats: Stats) -> str:
    masked = mg.lex_mask(text)
    prod = mg.strip_cfg_test(masked, [])
    alt = "|".join(re.escape(m) for m in sorted(mods))
    pat = re.compile(rf"(?<![\w:$])({alt})::")
    seen: dict[str, str] = {}
    for m in pat.finditer(masked):
        name = m.group(1)
        if f"use {dest_ident}::{name};" in text:
            continue
        line = masked.count("\n", 0, m.start()) + 1
        in_prod = prod[m.start() : m.end()] == m.group(0)
        stats.unqualified.append({"file": rel, "line": line, "module": name, "production": in_prod})
        if in_prod or name not in seen:
            seen[name] = "prod" if in_prod else "test"
        elif seen.get(name) != "prod":
            seen[name] = "test"
    if not seen:
        return text
    anchor = re.search(r"(?m)^use .*", masked)
    if not anchor:
        raise SplitError(f"{rel}: no top-level `use` to anchor imports for {sorted(seen)}")
    block = ""
    for name in sorted(seen):
        gate = "" if seen[name] == "prod" else "#[cfg(test)]\n"
        block += f"{gate}use {dest_ident}::{name};\n"
    return text[: anchor.start()] + block + text[anchor.start() :]


def rewrite_moved(
    text: str, rel: str, dest_ident: str, resident: set[str], src_ident: str, mods: set[str], stats: Stats
) -> str:
    masked = mg.lex_mask(text)
    pat = re.compile(rf"(?<![\w:$]){re.escape(dest_ident)}::")
    out, pos, sites = [], 0, 0
    for m in pat.finditer(text):
        if masked[m.start() : m.end()] != m.group(0):
            stats.self_skipped += 1
            continue
        out.append(text[pos : m.start()])
        out.append("crate::")
        pos = m.end()
        sites += 1
    if sites:
        out.append(text[pos:])
        text = "".join(out)
        stats.self_sites += sites
        stats.self_files += 1
        masked = mg.lex_mask(text)
    for m in re.finditer(r"(?<![\$\w])crate::(\w+)", masked):
        target = m.group(1)
        if target not in resident:
            line = masked.count("\n", 0, m.start()) + 1
            stats.residue[target].append(f"{rel}:{line}")
    for m in re.finditer(r"\buse\s+crate::\{", masked):
        close = mg.match_brace(masked, m.end() - 1)
        for _, path in mg.use_leaves(masked[m.end() : close]):
            if path[0] not in resident and path[0] not in BUILTIN_STD:
                line = masked.count("\n", 0, m.start()) + 1
                stats.residue[path[0]].append(f"{rel}:{line}")
    for pos_, name in mg.super_root_targets(masked, rel.split("/src/", 1)[-1]):
        if name not in resident and name not in BUILTIN_STD and name in mods | {"lib"}:
            stats.super_root.append({"file": rel, "line": masked.count("\n", 0, pos_) + 1, "target": name})
    return text


class Mover:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.mods = list(args.modules)
        self.modset = set(self.mods)
        self.src, self.dest = args.src_crate, args.dest_crate
        self.src_dir = f"crates/{self.src}/src"
        self.dest_dir = f"crates/{self.dest}/src"
        self.src_ident = sl.crate_ident(self.src)
        self.dest_ident = sl.crate_ident(self.dest)
        self.texts: dict[str, str] = {}
        self.fps: dict[str, Fingerprint] = {}
        self.new: dict[str, str | None] = {}
        self.stats = Stats()
        self.moves: dict[str, str] = {}
        self.other_crate_files: list[str] = []
        self.sweep_files: list[str] = []
        self.sweep_skipped: list[str] = []
        self.sweep_segmented = 0
        self.cfg_sites: list[dict] = []
        self.manifest: dict = {}
        self.extra_changes: list[str] = []
        self.unformatted_base = None

    def load(self, rel: str) -> str:
        if rel in self.texts:
            return self.texts[rel]
        path = REPO_ROOT / rel
        with open(path, "rb") as fh:
            st = os.fstat(fh.fileno())
            data = fh.read()
        self.fps[rel] = Fingerprint(st.st_size, st.st_mtime_ns, st.st_ino, sl.sha256_bytes(data))
        self.texts[rel] = data.decode("utf-8")
        return self.texts[rel]

    def plan_moves(self) -> None:
        for m in self.mods:
            files = sl.tracked_files(f"{self.src_dir}/{m}.rs", f"{self.src_dir}/{m}")
            files = [f for f in files if f == f"{self.src_dir}/{m}.rs" or f.startswith(f"{self.src_dir}/{m}/")]
            if not files:
                raise SplitError(f"no tracked {self.src_dir}/{m}.rs or {m}/")
            disk = REPO_ROOT / self.src_dir / m
            if disk.is_dir():
                tracked = set(files)
                loose = [
                    str(p.relative_to(REPO_ROOT))
                    for p in disk.rglob("*")
                    if p.is_file() and str(p.relative_to(REPO_ROOT)) not in tracked
                ]
                if loose:
                    raise SplitError(f"untracked files under {self.src_dir}/{m}: {loose[:5]}")
            for f in files:
                dest = self.dest_dir + f[len(self.src_dir) :]
                if (REPO_ROOT / dest).exists():
                    raise SplitError(f"destination {dest} already exists")
                self.moves[f] = dest

    def resident_modules(self) -> set[str]:
        out = set(self.modset)
        base = REPO_ROOT / self.dest_dir
        for p in base.iterdir():
            if p.is_dir():
                out.add(p.name)
            elif p.suffix == ".rs" and p.stem not in ("lib", "main"):
                out.add(p.stem)
        return out

    def run_plan(self) -> None:
        sl.require_git_repo()
        src_lib, dest_lib = f"{self.src_dir}/lib.rs", f"{self.dest_dir}/lib.rs"
        if not (REPO_ROOT / dest_lib).is_file():
            raise SplitError(f"{dest_lib} does not exist")
        self.plan_moves()
        resident = self.resident_modules()
        all_src = [f for f in sl.tracked_files(self.src_dir) if f.endswith(".rs")]
        staying = [f for f in all_src if f not in self.moves]
        bases = ["crate", self.src_ident]

        lib_text = self.load(src_lib)
        for m in self.mods:
            lib_text = self.remove_mod_line(lib_text, m)
        dest_text = self.load(dest_lib)
        for m in self.mods:
            dest_text = self.add_dest_mod(dest_text, m)
        self.new[dest_lib] = dest_text

        for rel in staying:
            text = lib_text if rel == src_lib else self.load(rel)
            new = rewrite_paths(text, rel, bases, self.modset, self.dest_ident, self.stats)
            if rel == src_lib:
                new = add_unqualified_imports(new, rel, self.modset, self.dest_ident, self.stats)
            masked_needed = "super::" in new and any(m in new for m in self.mods)
            if masked_needed:
                masked = mg.lex_mask(new)
                for pos_, name in mg.super_root_targets(masked, rel[len(self.src_dir) + 1 :]):
                    if name in self.modset:
                        self.stats.super_root.append({"file": rel, "line": masked.count("\n", 0, pos_) + 1, "target": name})
            if new != self.load(rel):
                self.new[rel] = new

        internal_lines = internal_files = 0
        pat = re.compile(rf"(?<![\$\w])crate::({'|'.join(re.escape(m) for m in self.mods)})(?![A-Za-z0-9_])")
        for src_rel, dest_rel in self.moves.items():
            if not src_rel.endswith(".rs"):
                self.new[dest_rel] = self.load(src_rel)
                continue
            text = self.load(src_rel)
            hit_lines = {text.count("\n", 0, m.start()) + 1 for m in pat.finditer(text)}
            if hit_lines:
                internal_files += 1
                internal_lines += len(hit_lines)
            self.new[dest_rel] = rewrite_moved(
                text, dest_rel, self.dest_ident, resident, self.src_ident, self.modset, self.stats
            )
        self.internal = (internal_files, internal_lines)

        self.rewrite_other_crates(set(staying) | set(self.moves) | {dest_lib})
        self.sweep()
        self.plan_cfg()
        self.plan_manifests()

    def remove_mod_line(self, text: str, module: str) -> str:
        pattern = re.compile(rf"^(?:pub(?:\([^)]*\))?\s+)?mod\s+{re.escape(module)}\s*;[ \t]*(?://[^\n]*)?\n", re.M)
        match = pattern.search(text)
        if not match:
            raise SplitError(f"no `mod {module};` line in {self.src_dir}/lib.rs")
        before = text[: match.start()].rstrip("\n").rsplit("\n", 1)[-1].strip()
        if before.startswith("#["):
            raise SplitError(f"`mod {module};` carries an attribute ({before}); move it by hand")
        return text[: match.start()] + text[match.end() :]

    @staticmethod
    def add_dest_mod(text: str, module: str) -> str:
        line = f"pub mod {module};\n"
        if line in text:
            raise SplitError(f"destination lib already declares `pub mod {module};`")
        lines = text.splitlines(keepends=True)
        mods = [i for i, l in enumerate(lines) if re.match(r"pub mod \w+;\n", l)]
        if not mods:
            return text.rstrip("\n") + "\n\n" + line
        insert = mods[-1] + 1
        for i in mods:
            if lines[i] > line:
                insert = i
                break
        lines.insert(insert, line)
        return "".join(lines)

    def rewrite_other_crates(self, already: set[str]) -> None:
        base = self.src_ident
        files: set[str] = set()
        for m in self.mods:
            files.update(sl.grep_files(f"{base}::{m}", "*.rs"))
        files = {f for f in files if f not in already and not f.startswith("vendor/")}
        bases = [base]
        for rel in sorted(files):
            text = self.load(rel)
            new = rewrite_paths(text, rel, bases, self.modset, self.dest_ident, self.stats)
            if new != text:
                self.new[rel] = new
                self.other_crate_files.append(rel)

    def sweep(self) -> None:
        regexes = sl.sweep_regexes(self.src, self.dest, self.mods)
        patterns = []
        for m in self.mods:
            patterns += [f"{self.src}/src/{m}"]
        candidates: set[str] = set()
        for pat in patterns:
            candidates.update(sl.grep_files(pat))
        for quote in ('"', "'"):
            candidates.update(
                sl.grep_files(f"{quote}{self.src}{quote}", "*.py", "*.mjs", "*.js", "*.ts", "*.sh", "*.rs")
            )
        for rel in sorted(candidates):
            if sl.sweep_skipped(rel):
                self.sweep_skipped.append(rel)
                continue
            if rel in self.moves:
                key, text = self.moves[rel], self.new[self.moves[rel]]
            elif rel in self.new:
                key, text = rel, self.new[rel]
            else:
                key = rel
                try:
                    text = self.load(rel)
                except UnicodeDecodeError:
                    self.sweep_skipped.append(rel)
                    continue
            updated = text
            for rx, repl in regexes:
                updated = rx.sub(repl, updated)
            if updated != text:
                self.new[key] = updated
                self.sweep_files.append(rel)
        self.sweep_files = sorted(set(self.sweep_files))
        self.sweep_skipped = sorted(set(self.sweep_skipped))
        regenerated = [p for p in GENERATED_BY_DERIVE_FACTS if (REPO_ROOT / p).exists()]
        self.derive_needed = any(f.startswith(("docs/", "web/")) for f in self.sweep_files)
        self.derive_outputs = regenerated

    def plan_cfg(self) -> None:
        item_re = re.compile(
            r"(?:pub(?:\([^)]*\)\s+)?)?(?:async\s+|unsafe\s+|const\s+|extern\s+(?:\"[^\"]*\"\s+)?)*"
            r"(?P<kind>mod|fn|struct|enum|trait|type|static|const|use|impl|macro_rules!)\s*(?P<name>\w+)?"
        )
        sites: list[dict] = []
        wanted: set[str] = set()
        edits_by_file: dict[str, list[tuple[int, int, dict]]] = {}
        for dest_rel in sorted(self.moves.values()):
            if not dest_rel.endswith(".rs"):
                continue
            text = self.new[dest_rel]
            if "cfg(" not in text:
                continue
            masked = mg.lex_mask(text)
            for m in re.finditer(r"#\[cfg\(", masked):
                depth, k = 1, m.end()
                while k < len(text) and depth:
                    depth += {"(": 1, ")": -1}.get(masked[k], 0)
                    k += 1
                if masked[k : k + 1] != "]":
                    continue
                expr = text[m.end() : k - 1].strip()
                atoms = set(re.findall(r"[A-Za-z_][\w-]*", expr)) | set(re.findall(r'"([^"]*)"', expr))
                if "test" not in atoms and "test-support" not in atoms:
                    continue
                line = text.count("\n", 0, m.start()) + 1
                j = k + 1
                while True:
                    skip = re.match(r"\s*(?:#\[[^\]]*\]|///[^\n]*|//![^\n]*)", text[j:])
                    if not skip:
                        break
                    j += skip.end()
                j += len(masked[j:]) - len(masked[j:].lstrip())
                im = item_re.match(masked, j)
                kind = im.group("kind") if im else "?"
                name = im.group("name") if im else None
                names: list[str] = []
                if kind == "impl":
                    brace = masked.find("{", j)
                    names = re.findall(r"\bfn\s+(\w+)", masked[j : mg.match_brace(masked, brace)]) if brace >= 0 else []
                    name = "impl{" + ",".join(names[:4]) + ("..." if len(names) > 4 else "") + "}"
                elif kind == "use":
                    stmt_end = masked.find(";", j)
                    names = [leaf for leaf, _ in mg.use_leaves(masked[j + 3 : stmt_end])]
                    name = ",".join(names[:4])
                elif name:
                    names = [name]
                if expr == "test":
                    cls = "test_mod" if kind == "mod" else "test_item"
                elif expr == "not(test)":
                    cls = "fork_not_test"
                else:
                    cls = "compound"
                site = {"file": dest_rel, "line": line, "cfg": expr, "class": cls, "item": f"{kind} {name or ''}".strip(), "used_outside": False, "_names": names}
                sites.append(site)
                if cls == "test_item":
                    wanted.update(names)
                    edits_by_file.setdefault(dest_rel, []).append((m.start(), k + 1, site))
        used: set[str] = set()
        if wanted:
            rx = re.compile(r"\b(?:" + "|".join(re.escape(n) for n in sorted(wanted)) + r")\b")
            moved_dests = set(self.moves.values())
            for f in sl.tracked_files("crates", "tests", "pet"):
                if not f.endswith(".rs") or f in self.moves or f in moved_dests:
                    continue
                try:
                    used.update(rx.findall(self.load(f)))
                except UnicodeDecodeError:
                    continue
        for site in sites:
            site["used_outside"] = any(n in used for n in site.pop("_names"))
        self.cfg_sites = sites
        if self.args.apply_cfg:
            for dest_rel, edits in edits_by_file.items():
                text = self.new[dest_rel]
                for a_, b_, site in sorted(edits, reverse=True):
                    if site["used_outside"]:
                        text = text[:a_] + '#[cfg(any(test, feature = "test-support"))]' + text[b_:]
                self.new[dest_rel] = text

    def plan_manifests(self) -> None:
        def deps_of(crate_dir: str) -> tuple[set[str], set[str], set[str]]:
            data = tomllib.loads((REPO_ROOT / "crates" / crate_dir / "Cargo.toml").read_text(encoding="utf-8"))
            normal, dev, feats = set(), set(), set(data.get("features", {}))
            def collect(tbl, into):
                for k, v in tbl.get("dependencies", {}).items():
                    into.add(k.replace("-", "_"))
                    if isinstance(v, dict) and "package" in v:
                        into.add(v["package"].replace("-", "_"))
                for k, v in tbl.get("build-dependencies", {}).items():
                    into.add(k.replace("-", "_"))
            collect(data, normal)
            for tbl in data.get("target", {}).values():
                collect(tbl, normal)
            for k in data.get("dev-dependencies", {}):
                dev.add(k.replace("-", "_"))
            return normal, dev, feats

        src_normal, src_dev, _ = deps_of(self.src)
        dest_normal, dest_dev, dest_feats = deps_of(self.dest)
        used_prod: set[str] = set()
        used_any: set[str] = set()
        features: set[str] = set()
        for dest_rel in self.moves.values():
            if not dest_rel.endswith(".rs"):
                continue
            text = self.new[dest_rel]
            masked = mg.lex_mask(text)
            prod = mg.strip_cfg_test(masked, [])
            roots = set(re.findall(r"(?<![\w:$])([a-z_][a-z0-9_]*)::", masked))
            prod_roots = set(re.findall(r"(?<![\w:$])([a-z_][a-z0-9_]*)::", prod))
            is_test_file = mg.is_test_path(dest_rel)
            used_any |= roots
            if not is_test_file:
                used_prod |= prod_roots
            features |= set(re.findall(r'feature\s*=\s*"([^"]+)"', text))
        resident_names = self.resident_modules()
        wanted_prod = {r for r in used_prod if r in src_normal and r not in resident_names and r not in BUILTIN_STD}
        wanted_any = {r for r in used_any if (r in src_normal or r in src_dev) and r not in resident_names and r not in BUILTIN_STD}
        self.manifest = {
            "dest_missing_normal": sorted(wanted_prod - dest_normal),
            "dest_missing_dev_or_normal": sorted((wanted_any - wanted_prod) - dest_normal - dest_dev),
            "features_used_not_declared_in_dest": sorted(f for f in features if f not in dest_feats),
            "features_used": sorted(features),
        }
        missing_consumers = []
        for rel in self.other_crate_files:
            parts = rel.split("/")
            if parts[0] != "crates" or len(parts) < 3:
                continue
            crate = parts[1]
            if crate == self.dest:
                continue
            normal, dev, _ = deps_of(crate)
            ident = self.dest_ident
            kind = "dev" if len(parts) > 2 and parts[2] in ("tests", "benches", "examples") else "normal"
            have = ident in normal or (kind == "dev" and ident in dev)
            if not have:
                missing_consumers.append({"crate": crate, "file": rel, "needs": kind})
        self.manifest["consumers_missing_dependency"] = sorted(
            {(c["crate"], c["needs"]) for c in missing_consumers}
        )

    def rewrite_set(self) -> list[str]:
        paths = set(self.new) | set(self.moves)
        return sorted(paths)

    def check_clean(self) -> dict:
        rewrite = self.rewrite_set()
        existing = [p for p in rewrite if (REPO_ROOT / p).exists()]
        dirty = sorted(sl.dirty_paths() & set(rewrite))
        young = [(p, round(a, 1)) for p, a in sl.recently_modified(existing, self.args.quiet_minutes)]
        return {"dirty": dirty, "recent": young}

    def changes(self) -> list[Change]:
        out: list[Change] = []
        fmt_items: dict[str, tuple[str | None, str]] = {}
        final: dict[str, str | None] = dict(self.new)
        for src in self.moves:
            final[src] = None
        for rel, text in final.items():
            if text is None or not rel.endswith(".rs") or self.args.no_format:
                continue
            base_src = next((s for s, d in self.moves.items() if d == rel), rel)
            base = self.texts.get(base_src)
            if base is not None and base == text:
                continue
            fmt_items[rel] = (base, text)
        if fmt_items:
            formatted, unstable = sl.format_many(fmt_items, strict=self.args.strict_format)
            self.unformatted_base = unstable
            for rel, text in formatted.items():
                final[rel] = text
        else:
            self.unformatted_base = []
        for rel, text in sorted(final.items()):
            if rel in self.moves.values():
                src = next(s for s, d in self.moves.items() if d == rel)
                out.append(Change(rel, text, None, mode_from=src))
            else:
                out.append(Change(rel, text, self.fps.get(rel)))
        return out

    def report(self, clean: dict | None) -> dict:
        s = self.stats
        internal_files, internal_lines = self.internal
        staying = {r: n for r, n in s.plain.items() if r.startswith(self.src_dir + "/")}
        other = {r: n for r, n in s.plain.items() if not r.startswith(self.src_dir + "/")}
        residue = {k: {"sites": len(v), "first": v[:3]} for k, v in sorted(s.residue.items(), key=lambda kv: -len(kv[1]))}
        return {
            "modules": self.mods,
            "src_crate": self.src,
            "dest_crate": self.dest,
            "files_moved": len(self.moves),
            "caller_rewrites": {
                "files": len(staying),
                "lines": sum(staying.values()),
                "in_comments": s.comment_hits,
                "in_string_literals": s.string_hits,
            },
            "other_crate_rewrites": {"files": len(other), "lines": sum(other.values())},
            "internal_refs_not_rewritten": {"files": internal_files, "lines": internal_lines},
            "legacy_dry_run_equivalent": {
                "files": len(staying) + internal_files,
                "lines": sum(staying.values()) + internal_lines,
            },
            "grouped_imports": s.grouped,
            "unqualified_in_source_lib": s.unqualified,
            "self_paths": {"sites": s.self_sites, "files": s.self_files, "skipped_in_comments_or_strings": s.self_skipped},
            "other_crate_files": self.other_crate_files,
            "sweep": {
                "files": self.sweep_files,
                "skipped": self.sweep_skipped,
                "must_sweep_covered": {p: (p in self.sweep_files) for p in MUST_SWEEP},
                "run_derive_facts": self.derive_needed,
                "derive_facts_outputs": self.derive_outputs,
            },
            "residue_moved_to_staying": residue,
            "super_root_residue": s.super_root,
            "manifest_plan": self.manifest,
            "cfg_plan": {
                "applied": bool(self.args.apply_cfg),
                "sites": len(self.cfg_sites),
                "by_class": dict(collections.Counter(c["class"] for c in self.cfg_sites)),
                "used_outside": sum(1 for c in self.cfg_sites if c["used_outside"] and c["class"] == "test_item"),
                "detail": self.cfg_sites,
            },
            "rustfmt_unstable_base": self.unformatted_base,
            "rewrite_set": self.rewrite_set(),
            "clean": clean,
        }


def run_mover(args: argparse.Namespace) -> int:
    mover = Mover(args)
    mover.run_plan()
    clean = mover.check_clean()
    if args.require_clean and (clean["dirty"] or clean["recent"]):
        lines = [f"dirty: {p}" for p in clean["dirty"]] + [f"modified {a} min ago: {p}" for p, a in clean["recent"]]
        raise SplitError("--require-clean refused; the rewrite set overlaps live work:\n  " + "\n  ".join(lines), 5)
    changes = None if args.dry_run else mover.changes()
    rep = mover.report(clean)
    if args.json:
        json.dump(rep, sys.stdout, indent=1)
        print()
    else:
        print_report(rep, args.dry_run)
    if args.dry_run:
        return 0
    jdir = Path(args.journal_dir) if args.journal_dir else sl.default_journal_dir("move-" + "-".join(args.modules)[:40])
    sl.apply_changes(changes, jdir, args.pause_after_plan, args.pause_mid_write, log)
    log(f"applied {len(changes)} change(s); journal {jdir}")
    if args.derive_facts:
        derive_facts(jdir)
    return 0


def derive_facts(jdir: Path) -> None:
    before = {p: (REPO_ROOT / p).read_bytes() if (REPO_ROOT / p).exists() else None for p in GENERATED_BY_DERIVE_FACTS}
    proc = subprocess.run(["node", "web/scripts/derive-facts.mjs"], cwd=REPO_ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        log(f"derive-facts failed: {proc.stderr.strip()[:500]}; generated files may be stale")
        return
    pre = jdir / "derive-pre"
    pre.mkdir(exist_ok=True)
    changed = []
    for p, old in before.items():
        new = (REPO_ROOT / p).read_bytes() if (REPO_ROOT / p).exists() else None
        if new != old:
            changed.append(p)
            if old is not None:
                (pre / p.replace("/", "__")).write_bytes(old)
    log("derive-facts regenerated: " + (", ".join(changed) if changed else "nothing changed") + " (add these to the rewrite set; pre-images in " + str(pre) + ")")


def print_report(rep: dict, dry: bool) -> None:
    cr = rep["caller_rewrites"]
    print(f"modules: {' '.join(rep['modules'])}  {rep['src_crate']} -> {rep['dest_crate']}  files moved: {rep['files_moved']}{' (dry run)' if dry else ''}")
    print(f"callers rewritten: {cr['files']} files, {cr['lines']} lines (comments {cr['in_comments']}, string literals {len(cr['in_string_literals'])})")
    ir = rep["internal_refs_not_rewritten"]
    leg = rep["legacy_dry_run_equivalent"]
    print(f"inside the moved set (kept as crate::): {ir['files']} files, {ir['lines']} lines; old-tool dry-run equivalent: {leg['files']} files, {leg['lines']} lines")
    print(f"grouped imports split: {len(rep['grouped_imports'])}; unqualified in source lib: {len(rep['unqualified_in_source_lib'])}")
    sp = rep["self_paths"]
    print(f"self paths rewritten to crate::: {sp['sites']} in {sp['files']} files (skipped in comments/strings: {sp['skipped_in_comments_or_strings']})")
    oc = rep["other_crate_rewrites"]
    print(f"other crates' files rewritten: {oc['files']} files, {oc['lines']} lines")
    sw = rep["sweep"]
    print(f"path sweep: {len(sw['files'])} files ({len(sw['skipped'])} skipped by the skip list); derive-facts needed: {sw['run_derive_facts']}")
    print(f"residue moved->staying: {sum(v['sites'] for v in rep['residue_moved_to_staying'].values())} sites in {len(rep['residue_moved_to_staying'])} modules; super-root residue: {len(rep['super_root_residue'])}")
    print(f"cfg sites: {rep['cfg_plan']['sites']} {rep['cfg_plan']['by_class']}; manifest: {json.dumps(rep['manifest_plan'])}")
    c = rep["clean"]
    if rep["rustfmt_unstable_base"]:
        print(f"left unformatted (base not rustfmt-stable): {len(rep['rustfmt_unstable_base'])} files")
    print(f"rewrite set: {len(rep['rewrite_set'])} paths; dirty overlap: {len(c['dirty'])}; recent (<min): {len(c['recent'])}")


def restore(path: str) -> int:
    sl.require_git_repo()
    problems = sl.rollback(Path(path))
    for p in problems:
        log(f"restore problem: {p}")
    log(f"restored pre-images from {path}")
    return 1 if problems else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("modules", nargs="*")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--require-clean", action="store_true")
    parser.add_argument("--quiet-minutes", type=float, default=15.0)
    parser.add_argument("--dest-crate", default="runtime")
    parser.add_argument("--src-crate", default="tui")
    parser.add_argument("--apply-cfg", action="store_true")
    parser.add_argument("--resolve-reexports", action="store_true")
    parser.add_argument("--journal-dir")
    parser.add_argument("--pause-after-plan", type=float, default=0.0, help="seconds to wait between plan and first write (lets a test change a file)")
    parser.add_argument("--pause-mid-write", type=float, default=0.0, help="seconds to wait once half the writes are done (exercises the restore path)")
    parser.add_argument("--restore", metavar="JOURNAL_DIR")
    parser.add_argument("--no-format", action="store_true")
    parser.add_argument("--strict-format", action="store_true", help="fail when a file to rewrite was not rustfmt-stable before the rewrite")
    parser.add_argument("--derive-facts", action="store_true")
    args = parser.parse_args(argv)
    if args.restore:
        return restore(args.restore)
    if not args.modules:
        parser.error("name at least one module")
    if args.resolve_reexports:
        import reexports

        return reexports.run(args, sl, mg)
    return run_mover(args)


if __name__ == "__main__":
    sys.exit(sl.run_main(main))
