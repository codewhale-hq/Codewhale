"""`move-modules.py --resolve-reexports`: retire a re-export facade.

For each facade module F (for example `config`) in the source crate, finds the
names its `pub use` lines re-export from other crates, enumerates glob
re-exports from the upstream sources (generated `include!(OUT_DIR/..)` modules
through their JSON data), and rewrites `crate::F::NAME` in the staying files to
the upstream path. Grouped `use crate::F::{..}` lists are split by upstream
parent. Relative `F::NAME` uses are reported, not rewritten.
"""

from __future__ import annotations

import collections
import json
import re
import sys
from pathlib import Path

from splitlib import attribute_lines_above


def flatten_groups_impl(mg, text: str, facades: list[str]) -> str:
    masked = mg.lex_mask(text)
    fac = "|".join(re.escape(f) for f in facades)
    group_re = re.compile(rf"(?m)^(?P<indent>[ \t]*)(?P<vis>pub(?:\([^)]*\))?[ \t]+)?use[ \t]+crate::\{{")
    out, pos = [], 0
    for m in group_re.finditer(masked):
        if m.start() < pos:
            continue
        open_i = m.end() - 1
        close_i = mg.match_brace(masked, open_i)
        semi = masked.find(";", close_i)
        depth, start, ents = 0, open_i + 1, []
        for i in range(open_i + 1, close_i):
            ch = masked[i]
            depth += (ch == "{") - (ch == "}")
            if ch == "," and depth == 0:
                ents.append((text[start:i].strip(), masked[start:i].strip()))
                start = i + 1
        ents.append((text[start:close_i].strip(), masked[start:close_i].strip()))
        ents = [(t, k) for t, k in ents if k]
        split = [t for t, k in ents if re.match(rf"(?:{fac})::", k)]
        if not split:
            continue
        kept = [t for t, k in ents if not re.match(rf"(?:{fac})::", k)]
        indent, vis = m.group("indent"), m.group("vis") or ""
        stmts = [f"{indent}{vis}use crate::{t};" for t in split]
        if kept:
            stmts.insert(0, f"{indent}{vis}use crate::{{{', '.join(kept)}}};")
        out.append(text[pos : m.start()])
        attrs = attribute_lines_above(text, m.start(), indent)
        out.append(("\n" + attrs).join(stmts))
        pos = semi + 1
    if not out:
        return text
    out.append(text[pos:])
    return "".join(out)


def run(args, sl, mg) -> int:
    flatten_groups = lambda text, facades: flatten_groups_impl(mg, text, facades)
    REPO = sl.REPO_ROOT
    sl.require_git_repo()
    src = args.src_crate
    src_dir = f"crates/{src}/src"
    facades = list(args.modules)
    crate_dirs = {sl.crate_ident(p.name): p.name for p in (REPO / "crates").iterdir() if (p / "Cargo.toml").is_file()}
    warnings: list[str] = []

    def generated_names(file_name: str, owner_dir: str) -> set[str]:
        base = REPO / "crates" / owner_dir
        names: set[str] = set()
        if file_name == "catalog_constants.rs":
            rev = json.loads((base / "assets/catalog_corrections.json").read_text(encoding="utf-8"))["reviewed"]
            names |= set(rev["constants"]) | set(rev["numeric_refs"]) | set(rev["groups"])
            build = (base / "build.rs").read_text(encoding="utf-8")
            names |= set(re.findall(r'\(\s*"([A-Z][A-Z0-9_]+_MODELS)"\s*,\s*"[a-z-]+"\s*,', build))
            names |= {"compatibility_alias", "completion_names"}
        elif file_name == "provider_defaults.rs":
            data = json.loads((base / "assets/provider_descriptors.json").read_text(encoding="utf-8"))
            names |= set(data["constant_refs"]) | set(data["compatibility_constants"])
        else:
            warnings.append(f"cannot enumerate generated names of {file_name} in crates/{owner_dir}")
        return names

    def module_text(crate_ident: str, segs: tuple[str, ...]):
        owner = crate_dirs.get(crate_ident)
        if owner is None:
            return None, None
        mod_dir = REPO / "crates" / owner / "src"
        text = (mod_dir / "lib.rs").read_text(encoding="utf-8")
        for seg in segs:
            masked = mg.lex_mask(text)
            m = re.search(rf"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+{re.escape(seg)}\s*(;|\{{)", masked)
            if not m:
                return None, owner
            if m.group(1) == "{":
                text = text[m.end() : mg.match_brace(masked, m.end() - 1)]
            else:
                path = next((c for c in (mod_dir / f"{seg}.rs", mod_dir / seg / "mod.rs") if c.is_file()), None)
                if path is None:
                    return None, owner
                text = path.read_text(encoding="utf-8")
            mod_dir = mod_dir / seg
        return text, owner

    def pub_names(crate_ident: str, segs: tuple[str, ...]) -> set[str]:
        text, owner = module_text(crate_ident, segs)
        if text is None:
            warnings.append(f"cannot find {crate_ident}::{'::'.join(segs)}")
            return set()
        masked = mg.lex_mask(text)
        names: set[str] = set()
        depth = 0
        for m in re.finditer(r"[{}]|(?m:^)[ \t]*pub(?:\([^)]*\))?\s+(?:async\s+|unsafe\s+|const\s+)*(?:const|static|fn|struct|enum|type|trait|mod|union)\s+(\w+)|(?m:^)[ \t]*pub\s+use\s+([^;]*);", masked):
            tok = m.group(0)
            if tok == "{":
                depth += 1
            elif tok == "}":
                depth -= 1
            elif depth == 0:
                if m.group(1):
                    names.add(m.group(1))
                elif m.group(2) is not None:
                    for bound, path in mg.use_leaves(m.group(2)):
                        if bound != "*":
                            names.add(bound)
        for inc in re.finditer(r'include!\s*\(\s*concat!\s*\(\s*env!\s*\(\s*"OUT_DIR"\s*\)\s*,\s*"/([\w.]+)"\s*\)\s*\)', text):
            names |= generated_names(inc.group(1), owner)
        return names

    exports: dict[str, dict[str, tuple[str, ...]]] = {}

    def facade_files(facade: str):
        out = {}
        for rel in sl.tracked_files(f"{src_dir}/{facade}.rs", f"{src_dir}/{facade}"):
            if not rel.endswith(".rs") or mg.is_test_path(rel):
                continue
            sub = rel[len(src_dir) + 1 :]
            parts = sub[:-3].split("/")
            if parts[-1] == "mod":
                parts = parts[:-1]
            out["::".join(parts)] = rel
        return out

    local_children: dict[str, set[str]] = {}
    for facade in facades:
        files = facade_files(facade)
        for modpath, rel in files.items():
            text = sl.read_text(rel)
            masked = mg.lex_mask(text)
            ex = exports.setdefault(modpath, {})
            child_names = {k.split("::")[-1] for k in files if k.startswith(modpath + "::") and k.count("::") == modpath.count("::") + 1}
            local_children[modpath] = child_names
            for m in re.finditer(r"(?m)^(?P<attrs>(?:[ \t]*#\[[^\n]*\]\s*\n)*)pub(?:\([^)]*\))?[ \t]+use[ \t]+", masked):
                end = masked.find(";", m.end())
                for bound, path in mg.use_leaves(masked[m.end() : end]):
                    head = path[0]
                    if head in ("self", "super", "crate") or head in child_names:
                        continue
                    if bound == "*":
                        for name in pub_names(head, path[1:-1]):
                            ex[name] = path[:-1] + (name,)
                    else:
                        ex[bound] = path
        for modpath in sorted(files, key=lambda p: -p.count("::")):
            text = sl.read_text(files[modpath])
            masked = mg.lex_mask(text)
            for m in re.finditer(r"(?m)^pub(?:\([^)]*\))?[ \t]+use[ \t]+(\w+)::\*[ \t]*;", masked):
                child = f"{modpath}::{m.group(1)}"
                if child in exports:
                    exports[modpath].update(exports[child])

    alt_by_module = {
        mp: re.compile(
            r"(?<![\$\w])crate::" + mp.replace("::", "::") + r"::(" + "|".join(re.escape(n) for n in sorted(ex, key=lambda n: -len(n))) + r")(?![A-Za-z0-9_])"
        )
        for mp, ex in exports.items()
        if ex
    }

    all_src = [f for f in sl.tracked_files(src_dir) if f.endswith(".rs")]
    facade_rels = set()
    for facade in facades:
        facade_rels |= set(facade_files(facade).values())
    new: dict[str, str] = {}
    fps: dict[str, object] = {}

    def read(rel: str) -> str:
        p = REPO / rel
        fps[rel] = sl.Fingerprint.of(p)
        return p.read_text(encoding="utf-8")

    direct_lines = direct_files = 0
    grouped = []
    relative_uses = collections.Counter()
    direct_files_set = set()
    for rel in all_src:
        if rel in facade_rels:
            continue
        text = read(rel)
        if "crate::" not in text and not any(f + "::" in text for f in facades):
            continue
        out = flatten_groups(text, facades)
        masked = mg.lex_mask(out)
        for facade in facades:
            group_re = re.compile(rf"(?m)^(?P<indent>[ \t]*)(?P<vis>pub(?:\([^)]*\))?[ \t]+)?use[ \t]+crate::(?P<path>{re.escape(facade)}(?:::\w+)*)::\{{")
            pieces, pos = [], 0
            for m in group_re.finditer(masked):
                if m.start() < pos:
                    continue
                ex = exports.get(m.group("path").replace("::", "::"))
                if not ex:
                    continue
                open_i = m.end() - 1
                close_i = mg.match_brace(masked, open_i)
                semi = masked.find(";", close_i)
                depth, start, ents = 0, open_i + 1, []
                for i in range(open_i + 1, close_i):
                    ch = masked[i]
                    depth += (ch == "{") - (ch == "}")
                    if ch == "," and depth == 0:
                        ents.append((out[start:i].strip(), masked[start:i].strip()))
                        start = i + 1
                ents.append((out[start:close_i].strip(), masked[start:close_i].strip()))
                ents = [(t, k) for t, k in ents if k]
                moved, kept = [], []
                for t, k in ents:
                    name = re.match(r"(\w+)", k)
                    (moved if name and name.group(1) in ex else kept).append((t, name.group(1) if name else None))
                if not moved:
                    continue
                by_parent = collections.defaultdict(list)
                for t, name in moved:
                    up = ex[name]
                    rest = t[len(name) :]
                    by_parent["::".join(up[:-1])].append(up[-1] + rest)
                indent, vis = m.group("indent"), m.group("vis") or ""
                stmts = []
                if kept:
                    stmts.append(f"{indent}{vis}use crate::{m.group('path')}::{{{', '.join(t for t, _ in kept)}}};")
                for parent, leaves in by_parent.items():
                    body = leaves[0] if len(leaves) == 1 else "{" + ", ".join(leaves) + "}"
                    stmts.append(f"{indent}{vis}use {parent}::{body};")
                pieces.append(out[pos : m.start()])
                attrs = attribute_lines_above(out, m.start(), indent)
                pieces.append(("\n" + attrs).join(stmts))
                pos = semi + 1
                grouped.append(f"{rel}:{masked.count(chr(10), 0, m.start()) + 1}")
            if pieces:
                pieces.append(out[pos:])
                out = "".join(pieces)
                masked = mg.lex_mask(out)
        lines = set()

        def sub(mp, rx):
            nonlocal out
            def repl(mo):
                lines.add(out.count("\n", 0, mo.start()) + 1)
                return "::".join(exports[mp][mo.group(1)])
            out = rx.sub(repl, out)

        for mp in sorted(alt_by_module, key=lambda p: -p.count("::")):
            sub(mp, alt_by_module[mp])
        if lines:
            direct_lines += len(lines)
            direct_files_set.add(rel)
        for facade in facades:
            if rel in (f"{src_dir}/lib.rs", f"{src_dir}/main.rs") or re.search(rf"\buse\s+crate::{re.escape(facade)}\s*(;|as\b)|crate::\{{[^}}]*\b{re.escape(facade)}\b[^:]", masked):
                ex = exports.get(facade, {})
                for mo in re.finditer(rf"(?<![\w:$]){re.escape(facade)}::(\w+)", masked):
                    if mo.group(1) in ex:
                        relative_uses[rel] += 1
        if out != text:
            new[rel] = out
    sweep = []
    report = {
        "facades": facades,
        "reexported_names": {mp: len(ex) for mp, ex in exports.items()},
        "direct_lines": direct_lines,
        "direct_files": len(direct_files_set),
        "grouped_lists": len(grouped),
        "grouped_files": len({g.split(":")[0] for g in grouped}),
        "files_rewritten": len(new),
        "relative_uses_not_rewritten": {"sites": sum(relative_uses.values()), "files": len(relative_uses)},
        "warnings": warnings,
        "upstream_paths": {mp: {n: "::".join(p) for n, p in sorted(ex.items())} for mp, ex in exports.items()},
    }
    if args.json:
        json.dump({**report, "rewrite_set": sorted(new)}, sys.stdout, indent=1)
        print()
    else:
        print(f"re-exported names: {report['reexported_names']}")
        print(f"direct crate::F::NAME: {direct_lines} lines in {len(direct_files_set)} files; grouped use lists split: {len(grouped)} in {report['grouped_files']} files")
        print(f"relative F::NAME uses (not rewritten): {report['relative_uses_not_rewritten']}; files rewritten: {len(new)}")
        for w in warnings:
            print(f"warning: {w}")
    if args.dry_run:
        return 0
    if args.require_clean:
        dirty = sorted(sl.dirty_paths() & set(new))
        young = sl.recently_modified(sorted(new), args.quiet_minutes)
        if dirty or young:
            raise sl.SplitError(
                "--require-clean refused; the rewrite set overlaps live work:\n  "
                + "\n  ".join([f"dirty: {p}" for p in dirty] + [f"modified {a:.1f} min ago: {p}" for p, a in young]),
                5,
            )
    changes, fmt = [], {}
    originals = {rel: read(rel) if rel not in fps else None for rel in new}
    originals = {rel: (sl.REPO_ROOT / rel).read_text(encoding="utf-8") for rel in new}
    if not args.no_format:
        formatted, _ = sl.format_many({rel: (originals[rel], text) for rel, text in new.items()})
    else:
        formatted = new
    for rel, text in sorted(formatted.items()):
        changes.append(sl.Change(rel, text, fps[rel]))
    jdir = Path(args.journal_dir) if args.journal_dir else sl.default_journal_dir("reexports-" + "-".join(facades))
    sl.apply_changes(changes, jdir, args.pause_after_plan, args.pause_mid_write)
    print(f"applied {len(changes)} change(s); journal {jdir}", file=sys.stderr)
    return 0
