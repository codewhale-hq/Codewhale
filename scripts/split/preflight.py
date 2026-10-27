#!/usr/bin/env python3
"""G11 path-coupling preflight for the crate split (whole repository, cargo free).

    python3 scripts/split/preflight.py config oauth settings          # one window
    python3 scripts/split/preflight.py --json config oauth
    python3 scripts/split/preflight.py                                # inventory only

Resolves every coupling that `cargo check` cannot see against the old-to-new
path map of the named modules (`crates/<src>/src/<m>.rs` and `<m>/` move to
`crates/<dest>/src/`):

* `include_str!` / `include_bytes!` / `include!` and `#[path]` anywhere in the
  repository (`crates`, `pet`, `tests`, ...): the target must still resolve
  from the includer, whether one, both or neither moves;
* `docs/features.toml` owner paths (`tests/feature_registry.rs` requires them
  to exist);
* path strings in tracked text files (`crates/tui/src/<m>`, `tui/src/<m>`,
  segmented `"tui" / "src" / "<m>"`): fixed by the mover's sweep, except in the
  sweep's skip list, which are reported as residue;
* manifest-relative and `../src/<m>` references from inside the source crate
  (tests, scripts, assets) that no sweep rewrites;
* CI test identities (`<m>::` names in workflows and scripts).

Exit status is 1 when any residue remains, so the run is a gate; the residue
lines say which file to edit by hand for the window.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import splitlib as sl
from splitlib import REPO_ROOT

INCLUDE_RE = re.compile(r"\b(include_str|include_bytes|include)!\s*\(")
PATH_ATTR_RE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]')
STRING_RE = re.compile(r'"((?:\\.|[^"\\])*)"')


def balanced(text: str, open_i: int) -> int:
    depth = 0
    for i in range(open_i, len(text)):
        if text[i] == "(":
            depth += 1
        elif text[i] == ")":
            depth -= 1
            if depth == 0:
                return i
    return len(text) - 1


class Preflight:
    def __init__(self, mods: list[str], src: str, dest: str):
        self.mods, self.src, self.dest = mods, src, dest
        self.map: dict[str, str] = {}
        src_dir = f"crates/{src}/src"
        for m in mods:
            for rel in sl.tracked_files(f"{src_dir}/{m}.rs", f"{src_dir}/{m}"):
                if rel == f"{src_dir}/{m}.rs" or rel.startswith(f"{src_dir}/{m}/"):
                    self.map[rel] = f"crates/{dest}/src" + rel[len(src_dir) :]
        self.rows: list[dict] = []

    def add(self, kind: str, status: str, file: str, line: int, detail: str, residue: bool) -> None:
        self.rows.append({"kind": kind, "status": status, "file": file, "line": line, "detail": detail, "residue": residue})

    def resolve(self, includer: str, literal: str, manifest_dir: str | None = None) -> str:
        base = manifest_dir if manifest_dir is not None else os.path.dirname(includer)
        return os.path.normpath(os.path.join(base, literal)) if not literal.startswith("/") else literal

    def includes(self) -> None:
        files = set()
        for pat in ("include_str!", "include_bytes!", "include!(", "#[path", "# [path"):
            files.update(sl.grep_files(pat, "*.rs"))
        for rel in sorted(files):
            if rel.startswith("vendor/"):
                continue
            try:
                text = sl.read_text(rel)
            except UnicodeDecodeError:
                continue
            masked = None
            crate_dir = None
            parts = rel.split("/")
            if parts[0] == "crates" and len(parts) > 2:
                crate_dir = "/".join(parts[:2])
            entries: list[tuple[int, str, str | None, str]] = []
            for m in INCLUDE_RE.finditer(text):
                prefix = text[text.rfind("\n", 0, m.start()) + 1 : m.start()]
                if prefix.lstrip().startswith("//"):
                    continue
                end = balanced(text, m.end() - 1)
                args = text[m.end() : end]
                lits = STRING_RE.findall(args)
                line = text.count("\n", 0, m.start()) + 1
                if "OUT_DIR" in args:
                    self.add("include", "generated-out-dir", rel, line, args.strip()[:90], False)
                    continue
                if not lits:
                    self.add("include", "dynamic", rel, line, args.strip()[:90], False)
                    continue
                if "CARGO_MANIFEST_DIR" in args and crate_dir:
                    entries.append((line, lits[-1].lstrip("/"), crate_dir, m.group(1)))
                else:
                    entries.append((line, "".join(lits), None, m.group(1)))
            for m in PATH_ATTR_RE.finditer(text):
                line = text.count("\n", 0, m.start()) + 1
                entries.append((line, m.group(1), None, "path"))
            for line, literal, manifest_dir, kind in entries:
                target = self.resolve(rel, literal, manifest_dir)
                self.classify_include(rel, line, kind, literal, target)

    def classify_include(self, rel: str, line: int, kind: str, literal: str, target: str) -> None:
        includer_moves, target_moves = rel in self.map, target in self.map
        if not (REPO_ROOT / target).exists() and not target_moves:
            self.add("include", "unresolvable-today", rel, line, f"{kind}!({literal}) -> {target}", False)
            return
        if not includer_moves and not target_moves:
            self.add("include", "unaffected", rel, line, f"{kind} {literal}", False)
        elif includer_moves and target_moves:
            new_inc, new_tgt = self.map[rel], self.map[target]
            if os.path.normpath(os.path.join(os.path.dirname(new_inc), literal)) == new_tgt:
                self.add("include", "both-move-relative-kept", rel, line, f"{kind} {literal}", False)
            else:
                self.add("include", "BROKEN-relative-changed", rel, line, f"{kind} {literal}", True)
        elif target_moves:
            new_tgt = self.map[target]
            hint = os.path.relpath(new_tgt, os.path.dirname(rel))
            self.add("include", "BROKEN-target-moved", rel, line, f"{kind} {literal} -> {hint}", True)
        else:
            new_inc = self.map[rel]
            if os.path.normpath(os.path.join(os.path.dirname(new_inc), literal)) == target:
                self.add("include", "includer-moves-relative-kept", rel, line, f"{kind} {literal}", False)
            else:
                hint = os.path.relpath(target, os.path.dirname(new_inc))
                self.add("include", "BROKEN-includer-moved", rel, line, f"{kind} {literal} -> {hint}", True)

    def features_owners(self) -> None:
        path = "docs/features.toml"
        if not (REPO_ROOT / path).is_file():
            return
        swept = not sl.sweep_skipped(path)
        for lineno, line in enumerate(sl.read_text(path).splitlines(), 1):
            m = re.match(r'\s*owner\s*=\s*"([^"]+)"', line)
            if not m:
                continue
            owner = m.group(1)
            if owner in self.map:
                self.add("features-owner", "moves-and-swept" if swept else "moves-NOT-swept", path, lineno, f"{owner} -> {self.map[owner]}", not swept)
            elif not (REPO_ROOT / owner).exists():
                self.add("features-owner", "missing-today", path, lineno, owner, True)

    def strings(self) -> None:
        if not self.mods:
            return
        regexes = sl.sweep_regexes(self.src, self.dest, self.mods)
        cands: set[str] = set()
        for m in self.mods:
            cands.update(sl.grep_files(f"{self.src}/src/{m}"))
        for q in ('"', "'"):
            cands.update(sl.grep_files(f"{q}{self.src}{q}", "*.py", "*.mjs", "*.js", "*.ts", "*.sh", "*.rs"))
        for rel in sorted(cands):
            try:
                text = sl.read_text(rel)
            except UnicodeDecodeError:
                continue
            hits = [(rx, mo) for rx, _ in regexes for mo in rx.finditer(text)]
            if not hits:
                continue
            first = min(text.count("\n", 0, mo.start()) + 1 for _, mo in hits)
            if sl.SWEEP_SKIP_GENERATED.match(rel):
                self.add("path-string", "generated-run-derive-facts", rel, first, f"{len(hits)} hits", False)
            elif Path(rel).name.startswith("CHANGELOG"):
                self.add("path-string", "history-left-alone", rel, first, f"{len(hits)} hits", False)
            elif sl.sweep_skipped(rel):
                snippet = text.splitlines()[first - 1].strip()[:90]
                self.add("path-string", "SKIPPED-BY-SWEEP", rel, first, f"{len(hits)} hits: {snippet}", True)
            else:
                self.add("path-string", "swept", rel, first, f"{len(hits)} hits", False)

    def relative_refs(self) -> None:
        if not self.mods:
            return
        alt = "|".join(re.escape(m) for m in self.mods)
        parent = re.compile(rf"(?:\.\./)+src/({alt})(?:\.rs|/)")
        manifest = re.compile(rf"""["']/?src/({alt})(?:\.rs|/)""")
        base = f"crates/{self.src}"
        for rel in sl.tracked_files(base):
            if rel.startswith(f"{base}/src/"):
                continue
            try:
                text = sl.read_text(rel)
            except UnicodeDecodeError:
                continue
            use_manifest = rel.endswith(".rs") and "CARGO_MANIFEST_DIR" in text
            for rx in (parent, manifest) if use_manifest else (parent,):
                for mo in rx.finditer(text):
                    line = text.count("\n", 0, mo.start()) + 1
                    self.add("relative-ref", "UNRESOLVED-relative", rel, line, mo.group(0), True)

    def test_identities(self) -> None:
        if not self.mods:
            return
        alt = "|".join(re.escape(m) for m in self.mods)
        pat = re.compile(rf"(?<![\w:$])({alt})::\w")
        for rel in sl.grep_files("::", ".github", "scripts/*.sh"):
            try:
                text = sl.read_text(rel)
            except UnicodeDecodeError:
                continue
            for mo in pat.finditer(text):
                line = text.count("\n", 0, mo.start()) + 1
                ctx = text[text.rfind("\n", 0, mo.start()) + 1 : text.find("\n", mo.end())].strip()[:110]
                self.add("test-identity", "REVIEW", rel, line, ctx, True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("modules", nargs="*")
    parser.add_argument("--src-crate", default="tui")
    parser.add_argument("--dest-crate", default="runtime")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--list", action="store_true", help="print every row, not only the residue")
    args = parser.parse_args(argv)
    sl.require_git_repo()
    pf = Preflight(args.modules, args.src_crate, args.dest_crate)
    pf.includes()
    pf.features_owners()
    pf.strings()
    pf.relative_refs()
    pf.test_identities()
    residue = [r for r in pf.rows if r["residue"]]
    summary = collections.Counter((r["kind"], r["status"]) for r in pf.rows)
    if args.json:
        json.dump(
            {
                "modules": args.modules,
                "files_in_move_map": len(pf.map),
                "summary": {f"{k}:{s}": n for (k, s), n in sorted(summary.items())},
                "residue_count": len(residue),
                "residue": residue,
                "rows": pf.rows,
            },
            sys.stdout,
            indent=1,
        )
        print()
    else:
        print(f"G11 path-coupling preflight: modules {' '.join(args.modules) or '(inventory)'}; {len(pf.map)} files in the move map")
        for (kind, status), n in sorted(summary.items()):
            print(f"  {kind:15} {status:28} {n}")
        shown = pf.rows if args.list else residue
        for r in shown:
            print(f"  {'RESIDUE' if r['residue'] else 'ok     '} {r['kind']}:{r['status']} {r['file']}:{r['line']} {r['detail']}")
        print(f"unresolved: {len(residue)}")
    return 1 if residue else 0


if __name__ == "__main__":
    sys.exit(sl.run_main(main))
