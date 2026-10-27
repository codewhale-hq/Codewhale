#!/usr/bin/env python3
"""Widen exactly the runtime items the compiler asks for (runtime/TUI split).

After `move-modules.py` moves code into `crates/runtime`, items that were
`pub(crate)` inside the TUI are private to the new crate. This loop runs
`cargo check --message-format=json` and rewrites only the definitions the
compiler names:

* E0603 / E0624 / E0616 / E0451 (private item, method, field, field in a
  struct literal) whose definition is in `crates/runtime/src`: the
  definition's `pub(crate)` / `pub(super)` / missing visibility becomes `pub`;
  a private `mod x;` becomes `pub mod x;`;
* a runtime `dead_code` warning on a `pub(crate)` item: the item is used only
  from the TUI, so it is widened the same way;
* `unfulfilled_lint_expectations` in the runtime: an `#[expect(dead_code)]`
  (or `cfg_attr(.., expect(..))`) the export made unfulfilled is deleted, but
  only when the attribute is alone on its line.

It never bulk-rewrites `pub(crate)`. Every widened item is printed; items in
security-relevant modules are flagged for human review.

While the loop runs, the destination manifest's `[lints] workspace = true` is
commented out (lints capped to warnings), because an item that is dead inside
the destination would otherwise fail its build under `warnings = "deny"`
before cargo ever reached the consumer's privacy errors. The stanza is
restored on exit, on SIGTERM, SIGHUP and SIGINT (build-lock.py forwards the
first two), and a manifest left capped by a killed earlier run is restored
before anything else; a final check runs with the stanza back.

`--prepass` widens, before the first compile, every `pub(crate)`/`pub(super)`
declaration in the destination whose name also appears in the consumer crates,
which saves most of the compile rounds (privacy errors only show when the
consumer is compiled). `unused_imports` warnings left by the move are removed
with the compiler's own machine-applicable suggestions, because
`warnings = "deny"` turns each one into a hard error.

Usage:
    python3 scripts/split/widen.py [--dest-crate runtime] [--src-crate tui]
        [--packages codewhale-runtime,codewhale-tui,codewhale-cli]
        [--cargo "scripts/dev-cargo.sh"] [--max-rounds N] [--prepass]
"""

from __future__ import annotations

import argparse
import atexit
import collections
import json
import os
import re
import shlex
import signal
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RUNTIME_SRC = REPO_ROOT / "crates" / "runtime" / "src"
RUNTIME_MANIFEST = REPO_ROOT / "crates" / "runtime" / "Cargo.toml"
CONSUMER_DIRS = ["tui", "cli"]
VIS = "pub"
COMMON_NAMES = {
    "new", "fmt", "from", "into", "default", "len", "get", "set", "build", "name", "id", "path", "run",
    "load", "save", "parse", "take", "text", "kind", "value", "data", "state", "config", "error",
}


class Interrupted(BaseException):
    pass


def configure(dest: str, src: str) -> None:
    global RUNTIME_SRC, RUNTIME_MANIFEST, CONSUMER_DIRS
    RUNTIME_SRC = REPO_ROOT / "crates" / dest / "src"
    RUNTIME_MANIFEST = REPO_ROOT / "crates" / dest / "Cargo.toml"
    CONSUMER_DIRS = [src, "cli"] if src != "cli" else ["cli"]
LINTS_ON = "[lints]\nworkspace = true\n"
LINTS_CAPPED = "# [lints] capped by scripts/split/widen.py\n# workspace = true\n"
PRIVACY_CODES = {"E0603", "E0624", "E0616", "E0451"}
SECURITY_MODULES = (
    "sandbox",
    "core/authority",
    "network_policy",
    "repo_law",
    "workspace_trust",
    "oauth",
    "credentials",
    "mcp",
    "runtime_api",
    "execpolicy",
)
ITEM_KW = r"(?:async\s+|unsafe\s+|const\s+|extern\s+\"[^\"]*\"\s+)*(?:fn|struct|enum|union|trait|type|const|static|mod|use)\b"


def run_cargo(cargo: list[str], packages: list[str], extra: list[str]) -> list[dict]:
    cmd = [*cargo, "check", "--locked", "--message-format=json", *extra]
    for p in packages:
        cmd += ["-p", p]
    proc = subprocess.run(cmd, cwd=REPO_ROOT, capture_output=True, text=True)
    messages = []
    for line in proc.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") == "compiler-message":
            messages.append(msg["message"])
    if proc.returncode != 0 and not messages:
        sys.stderr.write(proc.stderr[-4000:])
    return messages


def runtime_spans(message: dict) -> list[dict]:
    spans = list(message.get("spans", []))
    for child in message.get("children", []):
        spans.extend(child.get("spans", []))
    out = []
    for span in spans:
        path = (REPO_ROOT / span["file_name"]).resolve()
        if str(path).startswith(str(RUNTIME_SRC)):
            out.append(span)
    return out


def widen_line(line: str, name: str | None) -> str | None:
    """Rewrite one definition line to `pub`; None when it cannot be done safely."""
    m = re.match(r"^(\s*)pub\s*\((?:crate|super|in [^)]*)\)\s+", line)
    if m:
        new = m.group(1) + VIS + " " + line[m.end() :]
        return None if new == line else new
    if re.match(r"^\s*pub\s", line):
        return None  # already public: the error is about a parent module
    m = re.match(rf"^(\s*)(?={ITEM_KW})", line)
    if m:
        return m.group(1) + VIS + " " + line[m.end() :]
    if name:
        m = re.match(rf"^(\s*)(?={re.escape(name)}\s*:)", line)
        if m:
            return m.group(1) + VIS + " " + line[m.end() :]
    return None


def find_field(struct: str, field: str) -> tuple[Path, int] | None:
    pat = re.compile(rf"\bstruct\s+{re.escape(struct)}\b")
    for path in RUNTIME_SRC.rglob("*.rs"):
        lines = path.read_text(encoding="utf-8").split("\n")
        for i, line in enumerate(lines):
            if pat.search(line):
                for j in range(i + 1, min(i + 400, len(lines))):
                    if re.match(rf"^\s*(pub(\([^)]*\))?\s+)?{re.escape(field)}\s*:", lines[j]):
                        return path, j + 1
                    if lines[j].startswith("}"):
                        break
    return None


def item_line(path: Path, line_no: int) -> int:
    """Walk down from an attribute or doc line to the item line itself."""
    lines = path.read_text(encoding="utf-8").split("\n")
    i = line_no - 1
    while i < len(lines) and re.match(r"^\s*(#\[|///|//!)", lines[i]):
        i += 1
    return i + 1


def apply(edits: dict[tuple[Path, int], str | None]) -> list[str]:
    """Apply per-line edits: a string replaces the line, None deletes it."""
    by_file: dict[Path, list[tuple[int, str | None]]] = {}
    for (path, line_no), new in edits.items():
        by_file.setdefault(path, []).append((line_no, new))
    report = []
    for path, changes in by_file.items():
        lines = path.read_text(encoding="utf-8").split("\n")
        for line_no, new in sorted(changes, reverse=True):
            old = lines[line_no - 1]
            rel = path.relative_to(REPO_ROOT)
            flag = " [security review]" if any(f"/{m}" in f"/{rel}" for m in SECURITY_MODULES) else ""
            if new is None:
                del lines[line_no - 1]
                report.append(f"removed {rel}:{line_no}: {old.strip()}")
            else:
                lines[line_no - 1] = new
                report.append(f"widened {rel}:{line_no}: {new.strip()}{flag}")
        atomic_write(path, "\n".join(lines).encode("utf-8"))
    return report


def atomic_write(path: Path, data: bytes) -> None:
    tmp = path.with_name(path.name + ".widen-tmp")
    tmp.write_bytes(data)
    os.chmod(tmp, path.stat().st_mode & 0o7777)
    os.replace(tmp, path)


def unused_import_suggestions(messages: list[dict]) -> dict[Path, list[tuple[int, int, str]]]:
    out: dict[Path, set[tuple[int, int, str]]] = collections.defaultdict(set)
    for msg in messages:
        if (msg.get("code") or {}).get("code") != "unused_imports":
            continue
        for child in msg.get("children", []):
            for span in child.get("spans", []):
                if span.get("suggested_replacement") is None or span.get("suggestion_applicability") != "MachineApplicable":
                    continue
                path = (REPO_ROOT / span["file_name"]).resolve()
                if str(path).startswith(str(REPO_ROOT / "crates")):
                    out[path].add((span["byte_start"], span["byte_end"], span["suggested_replacement"]))
    return {p: sorted(v) for p, v in out.items()}


def apply_suggestions(suggestions: dict[Path, list[tuple[int, int, str]]]) -> list[str]:
    report = []
    for path, spans in suggestions.items():
        data = path.read_bytes()
        last = len(data) + 1
        applied = 0
        for start, end, repl in sorted(spans, reverse=True):
            if end > last:
                continue
            data = data[:start] + repl.encode("utf-8") + data[end:]
            last = start
            applied += 1
        atomic_write(path, data)
        report.append(f"unused import: {applied} removal(s) in {path.relative_to(REPO_ROOT)}")
    return report


def static_prepass() -> list[str]:
    decl = re.compile(
        r"^[ \t]*pub\((?:crate|super)\)[ \t]+(?:async\s+|unsafe\s+|const\s+)*(?:(?:fn|struct|enum|const|static|type|trait|mod)[ \t]+([A-Za-z_]\w*))",
        re.M,
    )
    tok = re.compile(r"[A-Za-z_]\w*")
    consumer_tokens: set[str] = set()
    for crate in CONSUMER_DIRS:
        for path in (REPO_ROOT / "crates" / crate).rglob("*.rs"):
            if "target" in path.parts:
                continue
            consumer_tokens.update(tok.findall(path.read_text(encoding="utf-8", errors="replace")))
    edits: dict[tuple[Path, int], str | None] = {}
    skipped = 0
    for path in RUNTIME_SRC.rglob("*.rs"):
        lines = path.read_text(encoding="utf-8").split("\n")
        for i, line in enumerate(lines):
            m = decl.match(line)
            if not m:
                continue
            name = m.group(1)
            if name in COMMON_NAMES or (len(name) < 6 and "_" not in name):
                skipped += 1
                continue
            if name in consumer_tokens:
                new = widen_line(line, name)
                if new is not None:
                    edits[(path, i + 1)] = new
    log = apply(edits) if edits else []
    log.append(f"prepass: {len(edits)} declaration(s) widened, {skipped} common/short name(s) left to the compiler")
    return log


def plan_edits(messages: list[dict]) -> tuple[dict, list[str]]:
    edits: dict[tuple[Path, int], str | None] = {}
    unresolved: list[str] = []
    for msg in messages:
        code = (msg.get("code") or {}).get("code") or ""
        level = msg.get("level")
        text = msg.get("message", "")
        if code in PRIVACY_CODES:
            name_m = re.search(r"`([^`]+)`", text)
            name = name_m.group(1).split("::")[-1] if name_m else None
            targets = [s for s in runtime_spans(msg) if not s.get("is_primary") or code == "E0603"]
            done = False
            for span in targets:
                path = (REPO_ROOT / span["file_name"]).resolve()
                line_no = item_line(path, span["line_start"])
                line = path.read_text(encoding="utf-8").split("\n")[line_no - 1]
                new = widen_line(line, name)
                if new is not None:
                    edits[(path, line_no)] = new
                    done = True
            if not done and code == "E0616":
                m = re.search(r"field `(\w+)` of struct `(?:[\w:]+::)?(\w+)", text)
                hit = m and find_field(m.group(2), m.group(1))
                if hit:
                    path, line_no = hit
                    line = path.read_text(encoding="utf-8").split("\n")[line_no - 1]
                    new = widen_line(line, m.group(1))
                    if new is not None:
                        edits[(path, line_no)] = new
                        done = True
            if not done:
                unresolved.append(msg.get("rendered") or text)
            continue
        if code == "dead_code" and level in ("warning", "error"):
            for span in runtime_spans(msg):
                path = (REPO_ROOT / span["file_name"]).resolve()
                line_no = span["line_start"]
                line = path.read_text(encoding="utf-8").split("\n")[line_no - 1]
                if re.match(r"^\s*pub\s*\(", line):
                    new = widen_line(line, None)
                    if new is not None:
                        edits[(path, line_no)] = new
                        continue
                unresolved.append(msg.get("rendered") or text)
            continue
        if code == "unfulfilled_lint_expectations":
            for span in runtime_spans(msg):
                path = (REPO_ROOT / span["file_name"]).resolve()
                line_no = span["line_start"]
                line = path.read_text(encoding="utf-8").split("\n")[line_no - 1].strip()
                if line.startswith("#[") and line.endswith("]") and "expect" in line:
                    edits[(path, line_no)] = None
                else:
                    unresolved.append(msg.get("rendered") or text)
            continue
        if level == "error":
            unresolved.append(msg.get("rendered") or text)
    return edits, unresolved


def set_lints(capped: bool) -> None:
    text = RUNTIME_MANIFEST.read_text(encoding="utf-8")
    old, new = (LINTS_ON, LINTS_CAPPED) if capped else (LINTS_CAPPED, LINTS_ON)
    if old in text:
        atomic_write(RUNTIME_MANIFEST, text.replace(old, new).encode("utf-8"))


def install_signal_handlers() -> None:
    def handler(signum, _frame):
        raise Interrupted(signal.Signals(signum).name)

    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(sig, handler)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--cargo", default="scripts/dev-cargo.sh")
    parser.add_argument("--max-rounds", type=int, default=12)
    parser.add_argument("--dest-crate", default="runtime")
    parser.add_argument("--src-crate", default="tui")
    parser.add_argument("--packages", help="comma separated; default codewhale-<dest>,codewhale-<src>")
    parser.add_argument("--extra", default="--lib --tests")
    parser.add_argument("--vis", choices=("pub", "pub(crate)"), default="pub", help="visibility given to widened items (pub, or pub(crate) inside one crate)")
    parser.add_argument("--prepass", action="store_true", help="widen declarations the consumers name before the first compile")
    parser.add_argument("--keep-unused-imports", action="store_true", help="do not apply the compiler's unused-import removals")
    args = parser.parse_args(argv)
    configure(args.dest_crate, args.src_crate)
    global VIS
    VIS = args.vis
    cargo = shlex.split(args.cargo)
    packages = (args.packages or f"codewhale-{args.dest_crate},codewhale-{args.src_crate}").split(",")
    extra = shlex.split(args.extra)

    if LINTS_CAPPED in RUNTIME_MANIFEST.read_text(encoding="utf-8"):
        print(f"{RUNTIME_MANIFEST.relative_to(REPO_ROOT)} was left capped by an earlier run; restoring it first", flush=True)
        set_lints(capped=False)

    log: list[str] = []
    install_signal_handlers()
    atexit.register(lambda: set_lints(capped=False) if RUNTIME_MANIFEST.exists() else None)
    set_lints(capped=True)
    try:
        if args.prepass:
            log.extend(static_prepass())
        for round_no in range(1, args.max_rounds + 1):
            messages = run_cargo(cargo, packages, extra)
            edits, unresolved = plan_edits(messages)
            suggestions = {} if args.keep_unused_imports else unused_import_suggestions(messages)
            if not edits and not suggestions:
                break
            changes = apply(edits) if edits else []
            changes += apply_suggestions(suggestions)
            log.extend(changes)
            print(f"round {round_no}: {len(changes)} edit(s)", flush=True)
    except Interrupted as exc:
        print(f"interrupted by {exc}; manifest restored, edits so far stay in the tree", flush=True)
        return 130
    finally:
        set_lints(capped=False)
    for line in log:
        print(line)
    messages = run_cargo(cargo, packages, extra)
    edits, unresolved = plan_edits(messages)
    remaining = [m for m in messages if m.get("level") == "error"]
    if edits or remaining:
        print(f"\n{len(remaining)} error(s) and {len(edits)} further edit(s) remain with lints on:")
        for m in remaining[:40]:
            print(m.get("rendered") or m.get("message"))
        for (path, line_no), new in list(edits.items())[:40]:
            print(f"pending {path.relative_to(REPO_ROOT)}:{line_no}: {new}")
        return 1
    print(f"clean: {len(log)} edit(s)")
    return 0


if __name__ == "__main__":
    os.environ.setdefault("CARGO_TERM_COLOR", "never")
    sys.exit(main())
