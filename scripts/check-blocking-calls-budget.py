#!/usr/bin/env python3
"""Ratchet for blocking calls that could land on Tokio workers (#6149).

Codewhale's convention: async code must not run blocking operations inline.
`std::fs`/`thread::sleep` (and friends) are fine inside `spawn_blocking`,
on dedicated `std::thread`s, and in synchronous entry points — but every
unprotected call site is one careless caller away from parking a runtime
worker. This check counts the sites that are NOT already inside a blocking
scope (`spawn_blocking`, `spawn_blocking_supervised`, `std::thread::spawn`,
`thread::Builder`) or test code, and fails if any file exceeds its recorded
budget in `check-blocking-calls-budget.json`.

Fix the call site — wrap the work in `spawn_blocking` (the established
pattern, ~80 sites) or switch to `tokio::fs`/`tokio::time` — or, if the site
is genuinely only reachable from synchronous code, acknowledge the debt by
raising the file's budget.

Run `python3 scripts/check-blocking-calls-budget.py --update` to regenerate
the budget after removing sites or after an intentional addition.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CRATES = ROOT / "crates"
BUDGET_PATH = Path(__file__).with_suffix(".json")

PATTERNS = {
    "thread_sleep": re.compile(r"\bthread::sleep\s*\("),
    "std_fs": re.compile(
        r"\bstd::fs::(?:read|read_to_string|write|create_dir|create_dir_all|"
        r"remove_file|remove_dir|remove_dir_all|copy|rename|metadata|"
        r"symlink_metadata|read_dir|canonicalize|exists|set_permissions|"
        r"hard_link|soft_link|symlink|(?:File|OpenOptions|DirBuilder)(?=\s*::))\b"
    ),
    # Method form (`path.canonicalize()`) resolves the path on the calling
    # thread exactly like `std::fs::canonicalize` (#6522 review).
    "path_canonicalize": re.compile(r"\.canonicalize\s*\(\s*\)"),
}

ATTR_RE = re.compile(r"#\s*\[([^\]]*)\]")
FN_RE = re.compile(
    r"\b(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?"
    r"(async\s+)?fn\s+([A-Za-z_][\w]*)"
)
MOD_RE = re.compile(r"\bmod\s+([A-Za-z_][\w]*)")

TOKEN_RE = re.compile(
    r"#\s*\[[^\]]*\]"
    r"|\bmod\s+\w+\s*;?"
    r"|\bimpl\b"
    r"|\basync\s+move\s*\{"
    r"|\basync\s*\{"
    r"|\b(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:async\s+)?fn\s+\w+"
    r"|spawn_blocking(?:_supervised)?"
    r"|thread::spawn"
    r"|thread::Builder::new"
    r"|[{}]"
)


def strip_comments_and_strings(text: str, *, module_literals: bool = False) -> str:
    """Blank out comments and string/char literal contents, keeping newlines."""
    out = list(text)
    i, n = 0, len(text)
    line_comment = block_comment = in_str = in_char = in_raw = False
    block_depth = 0
    raw_hashes = 0
    while i < n:
        c = text[i]
        if line_comment:
            if c == "\n":
                line_comment = False
            else:
                out[i] = " "
            i += 1
            continue
        if block_comment:
            if text[i : i + 2] == "/*":
                block_depth += 1
                out[i] = out[i + 1] = " "
                i += 2
                continue
            if text[i : i + 2] == "*/":
                block_depth -= 1
                out[i] = out[i + 1] = " "
                i += 2
                if block_depth == 0:
                    block_comment = False
                continue
            if c != "\n":
                out[i] = " "
            i += 1
            continue
        if in_str:
            if c == "\\":
                out[i] = out[i + 1] = " "
                i += 2
                continue
            if c == '"':
                in_str = False
            elif c != "\n":
                out[i] = " "
            i += 1
            continue
        if in_char:
            if c == "\\":
                out[i] = out[i + 1] = " "
                i += 2
                continue
            if c == "'":
                in_char = False
            elif c != "\n":
                out[i] = " "
            i += 1
            continue
        if in_raw:
            if c == '"' and text[i + 1 : i + 1 + raw_hashes] == "#" * raw_hashes:
                for j in range(1 + raw_hashes):
                    out[i + j] = " "
                i += 1 + raw_hashes
                in_raw = False
                continue
            if c != "\n":
                out[i] = " "
            i += 1
            continue
        if text[i : i + 2] == "//":
            line_comment = True
            out[i] = out[i + 1] = " "
            i += 2
            continue
        if text[i : i + 2] == "/*":
            block_comment = True
            block_depth = 1
            out[i] = out[i + 1] = " "
            i += 2
            continue
        if c == "r":
            # Production counting keeps its baseline masking unchanged. The
            # module-edge reader additionally recognises zero-hash raw paths.
            m = re.match(r'r(#*)"' if module_literals else r'r(#+)"', text[i:])
            if m:
                raw_hashes = len(m.group(1))
                in_raw = True
                for j in range(2 + raw_hashes):
                    out[i + j] = " "
                i += 2 + raw_hashes
                continue
        if c == '"':
            in_str = True
            out[i] = " "
            i += 1
            continue
        if c == "'" and re.match(r"'(?:\\.|[^'\\])'", text[i:]):
            in_char = True
            out[i] = " "
            i += 1
            continue
        i += 1
    return "".join(out)


def file_counts(path: Path) -> dict[str, int]:
    """Count unprotected blocking-call sites in one Rust source file."""
    text = path.read_text(encoding="utf-8", errors="replace")
    code = strip_comments_and_strings(text)
    counts = {name: 0 for name in PATTERNS}
    # Scope stack: entries are dicts {kind, open_depth} where kind is
    # 'test', 'blocking', 'fn', 'mod', or 'impl'. A hit counts only when the
    # innermost enclosing scope is neither test code nor a blocking pool /
    # dedicated-thread closure.
    stack: list[dict] = []
    pending_attr_test = False
    pending_blocking = False
    depth = 0
    for line in code.split("\n"):
        # Interleave pattern hits and scope tokens in column order so a
        # one-liner like `fn f() { thread::sleep(..) }` sees the fn scope.
        events: list[tuple[int, str, object]] = []
        for name, pat in PATTERNS.items():
            for m in pat.finditer(line):
                events.append((m.start(), "hit", name))
        for m in TOKEN_RE.finditer(line):
            events.append((m.start(), "tok", m.group(0)))
        events.sort(key=lambda e: e[0])
        for _col, kind, payload in events:
            if kind == "hit":
                if not any(s["kind"] in ("test", "blocking") for s in stack):
                    counts[payload] += 1  # type: ignore[index]
                continue
            tok = payload  # type: ignore[assignment]
            if tok.startswith("#"):
                inner = tok[tok.index("[") + 1 : -1]
                if "test" in inner:
                    pending_attr_test = True
                continue
            if tok == "{":
                depth += 1
                if pending_blocking:
                    stack.append({"kind": "blocking", "open": depth})
                elif stack and stack[-1]["open"] is None:
                    stack[-1]["open"] = depth
                pending_blocking = False
                continue
            if tok == "}":
                while stack and stack[-1]["open"] == depth:
                    stack.pop()
                depth -= 1
                continue
            if "spawn_blocking" in tok or tok in ("thread::spawn", "thread::Builder::new"):
                pending_blocking = True
                continue
            if tok.startswith("async") and tok.endswith("{"):
                stack.append({"kind": "fn", "open": depth + 1})
                depth += 1
                pending_attr_test = False
                pending_blocking = False
                continue
            fm = FN_RE.match(tok)
            if fm:
                kind = "test" if pending_attr_test else "fn"
                stack.append({"kind": kind, "open": None})
                pending_attr_test = False
                pending_blocking = False
                continue
            mm = MOD_RE.match(tok)
            if mm:
                name = mm.group(1)
                kind = "test" if (pending_attr_test or name.startswith("test")) else "mod"
                if not tok.rstrip().endswith(";"):
                    stack.append({"kind": kind, "open": None})
                pending_attr_test = False
                pending_blocking = False
                continue
            if tok == "impl":
                stack.append({"kind": "impl", "open": None})
                pending_attr_test = False
                pending_blocking = False
                continue
    return {k: v for k, v in counts.items() if v}


def _rust_path_literal(text: str) -> tuple[str, int] | None:
    """Only a literal include/path: never evaluate concat or a Rust expression."""
    normal = re.match(r'"(?:[^"\\]|\\.)*"', text)
    if normal:
        try:
            value = json.loads(normal.group())
        except ValueError:
            return None
        return value, normal.end()
    raw = re.match(r'r(#+|)"', text)
    if raw:
        end = text.find('"' + raw.group(1), raw.end())
        if end >= 0:
            return text[raw.end():end], end + 1 + len(raw.group(1))
    return None


def _test_module_edges(path: Path, text: str, *, crate_root: bool = False) -> list[tuple[Path, bool]]:
    """Literal item edges and their test-only module scope."""
    code = strip_comments_and_strings(text, module_literals=True)
    closes: dict[int, int] = {}
    braces: list[int] = []
    for position, char in enumerate(code):
        if char == "{":
            braces.append(position)
        elif char == "}" and braces:
            closes[braces.pop()] = position
    modules = list(re.finditer(
        r"(?P<attrs>(?:#\s*\[[^\]]*\]\s*)*)"
        r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>[A-Za-z_]\w*)\s*(?P<end>[{;])",
        code,
    ))
    inline = [entry for entry in modules if entry.group("end") == "{" and entry.end() - 1 in closes]
    def test_attribute(entry: re.Match[str]) -> bool:
        attrs = text[entry.start("attrs"):entry.end("attrs")]
        return any(re.fullmatch(
            r'#\s*\[\s*cfg\s*\(\s*(?:test|any\s*\(\s*test\s*,\s*feature\s*=\s*"test-support"\s*\))\s*\)\s*\]',
            attr.group(),
        ) for attr in re.finditer(r"#\s*\[[^\]]*\]", attrs))
    def ancestors(position: int) -> list[re.Match[str]]:
        return [entry for entry in inline if entry.end() <= position < closes[entry.end() - 1]]
    def under_test(position: int) -> bool:
        return any(test_attribute(entry) for entry in ancestors(position))
    base = path.parent if crate_root or path.name in ("mod.rs", "lib.rs", "main.rs") else path.parent / path.stem
    edges: list[tuple[Path, bool]] = []
    for entry in modules:
        if entry.group("end") != ";":
            continue
        parents = ancestors(entry.start())
        # Path attributes on an inline module can change its directory owner;
        # refuse to infer such a layout instead of exempting a guessed file.
        if any(re.search(r"#\s*\[\s*path\s*=", parent.group("attrs")) for parent in parents):
            continue
        directory = base.joinpath(*(parent.group("name") for parent in parents))
        attrs = text[entry.start("attrs"):entry.end("attrs")]
        configured = re.search(r"#\s*\[\s*path\s*=\s*", attrs)
        if configured:
            literal = _rust_path_literal(attrs[configured.end():])
            if not literal:
                continue
            directory = directory if parents else path.parent
            candidates = [directory / literal[0]]
        else:
            name = entry.group("name")
            candidates = [directory / f"{name}.rs", directory / name / "mod.rs"]
        found = [candidate.resolve() for candidate in candidates if candidate.is_file()]
        if len(found) == 1:
            edges.append((found[0], test_attribute(entry) or under_test(entry.start())))
    for entry in re.finditer(r"(?<![\w:])include\s*!\s*\(\s*", code):
        # String contents are masked, so whitespace in `code` also covers the
        # path. Recover only the original argument immediately after `(`.
        opening = code.index("(", entry.start(), entry.end())
        argument = text[opening + 1:].lstrip()
        literal = _rust_path_literal(argument)
        if not literal or not re.match(r"\s*,?\s*\)", argument[literal[1]:]):
            continue
        target = (path.parent / literal[0]).resolve()
        if target.is_file():
            edges.append((target, under_test(entry.start())))
    return edges


def _cargo_crate_roots(scan_root: Path) -> tuple[set[Path], set[Path]]:
    """Classify actual Cargo entry points; production always wins a conflict."""
    roots: set[Path] = set()
    test_roots: set[Path] = set()
    # Preserve conventional roots even in hermetic scopes without a manifest.
    for source in (scan_root, *scan_root.rglob("src")):
        if not source.is_dir():
            continue
        candidates = [source / "lib.rs", source / "main.rs"]
        candidates.extend((source / "bin").glob("*.rs"))
        candidates.extend((source / "bin").glob("*/main.rs"))
        roots.update(path.resolve() for path in candidates if path.is_file())
    for manifest in scan_root.rglob("Cargo.toml"):
        try:
            config = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            # A malformed/unreadable manifest must not exempt an unknown root.
            roots.update(path.resolve() for path in manifest.parent.rglob("*.rs"))
            continue
        if not isinstance(config.get("package"), dict):
            continue
        targets = [config.get("lib", {}), *config.get("bin", [])]
        for target in targets:
            if isinstance(target, dict) and isinstance(target.get("path"), str):
                path = manifest.parent / target["path"]
                if path.is_file():
                    roots.add(path.resolve())
        declared_names = set()
        for target in config.get("test", []):
            if not isinstance(target, dict):
                continue
            name, path = target.get("name"), target.get("path")
            if isinstance(name, str):
                declared_names.add(name)
            if isinstance(path, str):
                candidates = [manifest.parent / path]
            elif isinstance(name, str):
                candidates = [manifest.parent / "tests" / f"{name}.rs",
                              manifest.parent / "tests" / name / "main.rs"]
            else:
                continue
            found = [candidate.resolve() for candidate in candidates if candidate.is_file()]
            if len(found) == 1:
                test_roots.add(found[0])
            else:
                roots.update(found)  # Invalid/ambiguous target cannot exempt code.
        if config["package"].get("autotests", True):
            automatic: dict[str, list[Path]] = {}
            directory = manifest.parent / "tests"
            for candidate in (*directory.glob("*.rs"), *directory.glob("*/main.rs")):
                name = candidate.parent.name if candidate.name == "main.rs" else candidate.stem
                if name not in declared_names and candidate.is_file():
                    automatic.setdefault(name, []).append(candidate.resolve())
            for found in automatic.values():
                if len(found) == 1:
                    test_roots.add(found[0])
                else:
                    roots.update(found)
    return roots, test_roots


def module_file_scopes(scan_root: Path | None = None) -> tuple[set[Path], set[Path]]:
    """Test-only files and production conflicts from the same literal graph.

    Follow literal includes recursively from actual cfg(test) inline or
    external modules. A file also included by production remains counted,
    as do its unguarded descendants. Comments, strings and dynamic include
    expressions are never an exemption, nor is a `tests`-looking file name.
    """
    scan_root = (CRATES if scan_root is None else scan_root).resolve()
    cargo_production, cargo_tests = _cargo_crate_roots(scan_root)
    graph: dict[Path, list[tuple[Path, bool]]] = {}
    for path in scan_root.rglob("*.rs"):
        try:
            graph[path.resolve()] = _test_module_edges(
                path, path.read_text(encoding="utf-8", errors="ignore"),
                crate_root=path.resolve() in cargo_production | cargo_tests,
            )
        except OSError:
            continue
    test_reachable = {target for edges in graph.values() for target, test in edges if test}
    test_reachable.update(cargo_tests & set(graph))
    pending = list(test_reachable)
    while pending:
        for target, _test in graph.get(pending.pop(), []):
            if target not in test_reachable:
                test_reachable.add(target)
                pending.append(target)
    crate_roots = cargo_production & set(graph)
    production = (set(graph) - test_reachable) | crate_roots
    pending = list(production)
    while pending:
        for target, test in graph.get(pending.pop(), []):
            if not test and target not in production:
                production.add(target)
                pending.append(target)
    # A caller with legacy test-path heuristics must not exempt a known
    # production/test conflict or a real Cargo entry point by its filename.
    return test_reachable - production, (test_reachable & production) | crate_roots


def cfg_test_module_files(scan_root: Path | None = None) -> set[Path]:
    """Reuse the literal graph with an explicit scope; default remains CRATES."""
    return module_file_scopes(scan_root)[0]


def collect_current() -> dict[str, dict[str, int]]:
    budget: dict[str, dict[str, int]] = {}
    test_only = cfg_test_module_files()
    for path in sorted(CRATES.rglob("*.rs")):
        if path.resolve() in test_only:
            continue
        try:
            counts = file_counts(path)
        except OSError:
            continue
        if counts:
            rel = str(path.relative_to(ROOT))
            budget[rel] = counts
    return budget


def main() -> int:
    update = "--update" in sys.argv
    current = collect_current()
    if update:
        BUDGET_PATH.write_text(
            json.dumps(current, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        total = sum(sum(v.values()) for v in current.values())
        print(f"wrote {BUDGET_PATH.name}: {total} sites across {len(current)} files")
        return 0

    if not BUDGET_PATH.exists():
        print(f"missing {BUDGET_PATH.name}; run with --update to create it", file=sys.stderr)
        return 2
    budget = json.loads(BUDGET_PATH.read_text(encoding="utf-8"))

    failures: list[str] = []
    savings: list[str] = []
    for path, counts in sorted(current.items()):
        allowed = budget.get(path, {})
        for name, count in counts.items():
            limit = allowed.get(name, 0)
            if count > limit:
                failures.append(
                    f"{path}: {name} sites {count} > budget {limit}"
                )
            elif count < limit:
                savings.append(
                    f"{path}: {name} sites {count} < budget {limit} — tighten with --update"
                )
    for path, counts in sorted(budget.items()):
        if path not in current:
            savings.append(f"{path}: file clean — tighten with --update")
        else:
            for name in counts:
                if name not in current[path]:
                    savings.append(
                        f"{path}: {name} sites 0 < budget {counts[name]} — tighten with --update"
                    )

    for line in savings:
        print(line)
    if failures:
        print(
            "\nBlocking-call budget exceeded — new `thread::sleep`/`std::fs` call "
            "sites appeared outside spawn_blocking/dedicated-thread/test scopes:",
            file=sys.stderr,
        )
        for line in failures:
            print(f"  {line}", file=sys.stderr)
        print(
            "Move the work into `tokio::task::spawn_blocking` (or use tokio::fs "
            "/ tokio::time). If the site can only run on synchronous code, land "
            "the raised budget in this PR and say why in the PR description:\n"
            "  python3 scripts/check-blocking-calls-budget.py --update\n"
            "See #6149.",
            file=sys.stderr,
        )
        return 1
    total = sum(sum(v.values()) for v in current.values())
    print(f"blocking-call budget: {total} sites across {len(current)} files, within budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
