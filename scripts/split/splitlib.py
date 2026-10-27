#!/usr/bin/env python3
"""Shared safety layer for the crate-split tools.

Everything here exists so a mechanical rewrite on the shared checkout can fail
visibly and leave the tree as it found it:

* git failures are fatal, and the repository root must itself hold `.git`
  (a `git archive` export would otherwise silently do nothing);
* every write is a compare-and-swap against the bytes the plan was computed
  from, through a temp file and `os.replace`, with a pre-image journal;
* any exception or SIGTERM/SIGHUP/SIGINT during the apply phase restores the
  journalled pre-images in reverse order;
* rewritten Rust goes through `rustfmt --edition 2024 --emit stdout` on stdin,
  one file at a time, so no `mod` child and no neighbouring writer's file is
  reformatted.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


class SplitError(SystemExit):
    def __init__(self, message: str, code: int = 2):
        self.message = f"split: {message}"
        self.exit_code = code
        super().__init__(self.message)

    def __str__(self) -> str:
        return self.message


def run_main(fn) -> int:
    try:
        return fn()
    except SplitError as exc:
        print(exc.message, file=sys.stderr)
        return exc.exit_code


def attribute_lines_above(text: str, pos: int, indent: str) -> str:
    lines = text[:pos].split("\n")
    if lines and lines[-1] == "":
        lines = lines[:-1]
    block: list[str] = []
    while lines and re.match(r"^\s*#\[.*\]\s*$", lines[-1]):
        block.append(lines.pop().strip())
    return "".join(f"{indent}{line}\n" for line in reversed(block))


def git(*args: str, input_text: str | None = None) -> str:
    proc = subprocess.run(
        ["git", *args], cwd=REPO_ROOT, capture_output=True, text=True, input=input_text
    )
    if proc.returncode != 0:
        raise SplitError(f"`git {' '.join(args)}` failed ({proc.returncode}): {proc.stderr.strip()}")
    return proc.stdout


def require_git_repo() -> None:
    if not (REPO_ROOT / ".git").exists():
        raise SplitError(f"{REPO_ROOT} has no .git; refusing to run on a tree git cannot see")
    top = Path(git("rev-parse", "--show-toplevel").strip()).resolve()
    if top != REPO_ROOT.resolve():
        raise SplitError(f"git toplevel {top} is not {REPO_ROOT}; refusing to run")


def tracked_files(*pathspecs: str) -> list[str]:
    out = git("ls-files", "-z", "--", *pathspecs)
    return [p for p in out.split("\0") if p]


def grep_files(pattern: str, *pathspecs: str, fixed: bool = True) -> list[str]:
    args = ["grep", "-I", "-l", "-z"]
    if fixed:
        args.append("-F")
    args += ["-e", pattern, "--", *(pathspecs or (".",))]
    proc = subprocess.run(["git", *args], cwd=REPO_ROOT, capture_output=True, text=True)
    if proc.returncode not in (0, 1):
        raise SplitError(f"`git {' '.join(args)}` failed ({proc.returncode}): {proc.stderr.strip()}")
    return [p for p in proc.stdout.split("\0") if p]


def dirty_paths() -> set[str]:
    out = git("status", "--porcelain=v1", "-z", "--untracked-files=all")
    paths: set[str] = set()
    entries = out.split("\0")
    i = 0
    while i < len(entries):
        entry = entries[i]
        i += 1
        if len(entry) < 4:
            continue
        status, path = entry[:2], entry[3:]
        paths.add(path)
        if status[0] in "RC":
            if i < len(entries) and entries[i]:
                paths.add(entries[i])
            i += 1
    return paths


def recently_modified(rels: list[str], minutes: float) -> list[tuple[str, float]]:
    if minutes <= 0:
        return []
    now = time.time()
    out = []
    for rel in rels:
        try:
            age = (now - (REPO_ROOT / rel).stat().st_mtime) / 60.0
        except OSError:
            continue
        if age < minutes:
            out.append((rel, age))
    return out


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


@dataclass(frozen=True)
class Fingerprint:
    size: int
    mtime_ns: int
    ino: int
    sha: str

    @staticmethod
    def of(path: Path) -> "Fingerprint":
        st = path.stat()
        return Fingerprint(st.st_size, st.st_mtime_ns, st.st_ino, sha256_bytes(path.read_bytes()))

    def matches(self, path: Path) -> bool:
        try:
            st = path.stat()
        except OSError:
            return False
        if (st.st_size, st.st_mtime_ns, st.st_ino) == (self.size, self.mtime_ns, self.ino):
            return True
        return st.st_size == self.size and sha256_bytes(path.read_bytes()) == self.sha


def read_text(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


def rustfmt_text(text: str, label: str) -> str:
    proc = subprocess.run(
        ["rustfmt", "--edition", "2024", "--emit", "stdout"],
        cwd=REPO_ROOT,
        input=text,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise SplitError(f"rustfmt failed on {label}: {proc.stderr.strip()[:600]}")
    return proc.stdout


def format_many(
    items: dict[str, tuple[str | None, str]], workers: int = 8, strict: bool = False
) -> tuple[dict[str, str], list[str]]:
    """Format rewritten Rust in memory with rustfmt on stdin; every failure is fatal.

    `items` maps a label to (base_text_or_None, new_text). rustfmt's exit status
    is never trusted (stdin mode exits 0 over a diff): the formatted bytes are
    what gets written, a rustfmt error aborts, and the formatted text must be a
    fixed point of a second pass. A file whose base text is not already
    rustfmt-stable is left exactly as rewritten so no foreign line is reflowed;
    those labels are returned, and `strict` turns them into an error.
    """
    result: dict[str, str] = {}
    unstable: list[str] = []

    def one(label: str) -> tuple[str, str, bool]:
        base, new = items[label]
        formatted = rustfmt_text(new, label)
        if base is not None and rustfmt_text(base, label) != base:
            return label, new, True
        if rustfmt_text(formatted, label) != formatted:
            raise SplitError(f"rustfmt output for {label} is not a fixed point; refusing to write it")
        return label, formatted, False

    with ThreadPoolExecutor(max_workers=workers) as pool:
        for label, text, was_unstable in pool.map(one, sorted(items)):
            result[label] = text
            if was_unstable:
                unstable.append(label)
    if strict and unstable:
        raise SplitError(
            "--strict-format: these files were not rustfmt-stable before the rewrite:\n  " + "\n  ".join(unstable)
        )
    return result, unstable


@dataclass
class Change:
    rel: str
    text: str | None  # None deletes the file
    base: Fingerprint | None = None  # None means the file must not exist yet
    mode_from: str | None = None  # copy permission bits from this tracked path


class Interrupted(Exception):
    pass


class Journal:
    def __init__(self, root: Path):
        self.root = root
        self.pre = root / "pre"
        self.ops: list[dict] = []
        self.created_dirs: list[str] = []
        self.pre.mkdir(parents=True, exist_ok=True)

    def _flush(self) -> None:
        tmp = self.root / "manifest.json.tmp"
        tmp.write_text(
            json.dumps({"ops": self.ops, "created_dirs": self.created_dirs}, indent=1), encoding="utf-8"
        )
        os.replace(tmp, self.root / "manifest.json")

    def record(self, op: dict, pre_bytes: bytes | None) -> None:
        if pre_bytes is not None:
            name = f"{len(self.ops):06d}"
            (self.pre / name).write_bytes(pre_bytes)
            op["pre"] = name
        self.ops.append(op)
        self._flush()

    def note_dir(self, rel: str) -> None:
        self.created_dirs.append(rel)
        self._flush()


def default_journal_dir(label: str) -> Path:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    backups = REPO_ROOT.parent / "backups"
    if not backups.is_dir():
        raise SplitError(f"{backups} does not exist; pass --journal-dir")
    return backups / f"crate-split-{label}-{stamp}"


def rollback(journal_root: Path, repo: Path = REPO_ROOT) -> list[str]:
    manifest = json.loads((journal_root / "manifest.json").read_text(encoding="utf-8"))
    problems: list[str] = []
    for op in reversed(manifest["ops"]):
        path = repo / op["path"]
        try:
            if op["op"] in ("write", "delete") and "pre" in op:
                data = (journal_root / "pre" / op["pre"]).read_bytes()
                path.parent.mkdir(parents=True, exist_ok=True)
                tmp = path.with_name(path.name + ".split-restore")
                tmp.write_bytes(data)
                if "mode" in op:
                    os.chmod(tmp, op["mode"])
                os.replace(tmp, path)
            elif op["op"] == "write":
                if path.exists():
                    path.unlink()
        except OSError as exc:
            problems.append(f"{op['path']}: {exc}")
    for rel in reversed(manifest["created_dirs"]):
        try:
            (repo / rel).rmdir()
        except OSError:
            pass
    return problems


def apply_changes(
    changes: list[Change],
    journal_dir: Path,
    pause_after_plan: float = 0.0,
    pause_mid_write: float = 0.0,
    log=lambda msg: print(msg, file=sys.stderr),
) -> None:
    """Compare-and-swap every change in, or restore every pre-image and raise."""
    journal_dir.mkdir(parents=True, exist_ok=False)
    journal = Journal(journal_dir)
    if pause_after_plan > 0:
        log(f"plan frozen; writing in {pause_after_plan:g}s (journal {journal_dir})")
        time.sleep(pause_after_plan)

    stale = []
    for ch in changes:
        path = REPO_ROOT / ch.rel
        if ch.base is None:
            if path.exists() and ch.text is not None:
                stale.append(f"{ch.rel}: appeared after the plan was made")
        elif not ch.base.matches(path):
            stale.append(f"{ch.rel}: changed after the plan was made")
    if stale:
        shutil.rmtree(journal_dir)
        raise SplitError("compare-and-swap refused, nothing written:\n  " + "\n  ".join(stale), 3)

    def on_signal(signum, _frame):
        raise Interrupted(f"signal {signum}")

    previous = {}
    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        previous[sig] = signal.signal(sig, on_signal)
    try:
        for index, ch in enumerate(changes):
            if pause_mid_write > 0 and index == len(changes) // 2:
                log(f"{index} of {len(changes)} written; pausing {pause_mid_write:g}s (test hook)")
                time.sleep(pause_mid_write)
            path = REPO_ROOT / ch.rel
            if ch.base is not None and not ch.base.matches(path):
                raise SplitError(f"{ch.rel} changed while writing", 3)
            if ch.base is None and ch.text is not None and path.exists():
                raise SplitError(f"{ch.rel} appeared while writing", 3)
            pre = path.read_bytes() if ch.base is not None else None
            mode = path.stat().st_mode & 0o7777 if ch.base is not None else None
            if ch.text is None:
                journal.record({"op": "delete", "path": ch.rel, "mode": mode}, pre)
                path.unlink()
                continue
            op = {"op": "write", "path": ch.rel, "post_sha": sha256_bytes(ch.text.encode("utf-8"))}
            if mode is not None:
                op["mode"] = mode
            journal.record(op, pre)
            parent = path.parent
            missing = []
            while not parent.exists():
                missing.append(parent)
                parent = parent.parent
            for d in reversed(missing):
                d.mkdir()
                journal.note_dir(str(d.relative_to(REPO_ROOT)))
            tmp = path.with_name(path.name + ".split-tmp")
            tmp.write_bytes(ch.text.encode("utf-8"))
            if mode is None and ch.mode_from:
                try:
                    mode = (REPO_ROOT / ch.mode_from).stat().st_mode & 0o7777
                except OSError:
                    mode = None
            if mode is not None:
                os.chmod(tmp, mode)
            os.replace(tmp, path)
    except BaseException as exc:
        log(f"aborting: {exc}; restoring pre-images from {journal_dir}")
        for sig, handler in previous.items():
            signal.signal(sig, signal.SIG_IGN)
        problems = rollback(journal_dir)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        for ch in changes:
            stray = (REPO_ROOT / ch.rel).with_name(Path(ch.rel).name + ".split-tmp")
            if stray.exists():
                stray.unlink()
        if problems:
            log("restore problems:\n  " + "\n  ".join(problems))
        (journal_dir / "ROLLED_BACK").write_text(str(exc) + "\n", encoding="utf-8")
        if isinstance(exc, SplitError):
            raise
        raise SplitError(f"{exc}; tree restored from the journal", 4) from exc
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    (journal_dir / "COMMITTED").write_text(f"{len(changes)} change(s)\n", encoding="utf-8")


def load_module_graph():
    path = Path(__file__).with_name("module_graph.py")
    spec = importlib.util.spec_from_file_location("module_graph", path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("module_graph", mod)
    spec.loader.exec_module(mod)
    return mod


def crate_ident(crate_dir: str) -> str:
    manifest = (REPO_ROOT / "crates" / crate_dir / "Cargo.toml").read_text(encoding="utf-8")
    in_package = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package and stripped.startswith("name"):
            return stripped.split("=", 1)[1].strip().strip('"').replace("-", "_")
    raise SplitError(f"no [package] name in crates/{crate_dir}/Cargo.toml")


SWEEP_SKIP_PREFIXES = ("vendor/", "crates/config/", "scripts/split/")
SWEEP_SKIP_GENERATED = re.compile(r"^web/data/[^/]+\.generated\.json$")


def sweep_skipped(rel: str) -> bool:
    return (
        rel.startswith(SWEEP_SKIP_PREFIXES)
        or Path(rel).name.startswith("CHANGELOG")
        or bool(SWEEP_SKIP_GENERATED.match(rel))
    )


def sweep_regexes(src: str, dest: str, mods: list[str]) -> list[tuple[re.Pattern, object]]:
    """Path-string rewrites shared by the mover and the preflight."""
    out: list[tuple[re.Pattern, object]] = []
    for m in mods:
        em = re.escape(m)
        out.append((re.compile(rf"crates/{src}/src/{em}(?![A-Za-z0-9_])"), f"crates/{dest}/src/{m}"))
        out.append((re.compile(rf"(?<![A-Za-z0-9_/]){src}/src/{em}(?![A-Za-z0-9_])"), f"{dest}/src/{m}"))
        segmented = re.compile(
            rf"""(?P<a>(["'])){src}(?P=a)(?P<s1>\s*(?:,|/)\s*)(?P<b>(["']))src(?P=b)(?P<s2>\s*(?:,|/)\s*)(?P<c>(["'])){em}(?:\.rs)?(?P=c)"""
        )
        out.append(
            (
                segmented,
                lambda mo, dest=dest, src=src: mo.group(0).replace(
                    f"{mo.group('a')}{src}{mo.group('a')}", f"{mo.group('a')}{dest}{mo.group('a')}", 1
                ),
            )
        )
    return out
