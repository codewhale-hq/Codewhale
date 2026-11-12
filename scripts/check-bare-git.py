#!/usr/bin/env python3
"""Refuse a bare `Command::new("git")` in product code.

Repository config can make even a read such as `git status` run a program the
workspace chose (core.fsmonitor, clean and smudge filters, hooks), and a bare
command hands that program the parent's whole environment. Every product git
spawn therefore starts from `codewhale_dependencies::Git` (`command`,
`review_base`, `review_command`, `status_command`, `workspace_command`), which
scrubs the environment and disables those helpers. Crates below that owner
receive their command from the caller (see `codewhale_lane::set_git_command`).

Test code is exempt, and it is found structurally, never by file name: a file
under a `tests` or `benches` directory, a file only reachable through
`#[cfg(test)] mod x;` or an `include!` inside a `#[cfg(test)]` item, and the
`#[cfg(test)]` items themselves (a `mod`, `fn`, `impl`, `use` or other item,
skipped to its closing brace or semicolon). An attribute on a field,
expression or statement hides nothing. A `Command` imported under another name
is matched too. The few product files below only ask git for a repository's top
level, remote URL or HEAD, which starts no repository-chosen program; each entry
names its reason and its exact count.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CRATES = ROOT / "crates"

ALLOWED = {
    "crates/cli/src/lib.rs": (1, "rev-parse --show-toplevel before the dependency owner is reachable"),
    "crates/memory/src/workspace.rs": (1, "rev-parse --verify HEAD in the standalone memory binary"),
    "crates/runtime/src/native_memory.rs": (2, "config --get and rev-parse below the dependency owner"),
}

TOKEN_RE = re.compile(
    r"""
    (?P<line>//[^\n]*)
  | (?P<block>/\*)
  | (?P<raw>(?<![A-Za-z0-9_])b?r(?P<hashes>\#*)")
  | (?P<string>(?<![A-Za-z0-9_])b?"(?:\\.|[^"\\])*")
  | (?P<char>b?'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'\n])')
    """,
    re.VERBOSE | re.DOTALL,
)
CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
INNER_CFG_TEST_RE = re.compile(r"#\s*!\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
ATTRIBUTE_RE = re.compile(r"\s*#\s*\[")
ITEM_RE = re.compile(
    r"""
    (?:pub(?:\s*\([^)]*\))?\s+)?
    (?:(?:unsafe|async|const|default|extern(?:\s+"?\s*"?)?)\s+)*
    (?:mod|fn|impl|struct|enum|trait|type|use|static|const|union|macro_rules\s*!|extern\s+crate)\b
    """,
    re.VERBOSE,
)
MOD_DECL_RE = re.compile(r"(?<![A-Za-z0-9_])mod\s+([A-Za-z_]\w*)\s*;")
VISIBILITY_RE = re.compile(r"(?<![A-Za-z0-9_])pub(?:\s*\([^)]*\))?\s*$")
PATH_ATTR_RE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]')
INCLUDE_RE = re.compile(r'(?<![A-Za-z0-9_])include\s*!\s*\(\s*"([^"]+)"\s*\)')
ALIAS_RE = re.compile(r"(?<![A-Za-z0-9_])Command\s+as\s+([A-Za-z_]\w*)")
GIT_LITERAL_RE = re.compile(r'"git(?:\.exe)?"')
BRACKETS = {"(": ")", "[": "]"}


def blank(text: str) -> str:
    return re.sub(r"[^\n]", " ", text)


def lex(text: str) -> tuple[str, str]:
    """Return `(plain, shape)`: same length and line layout as `text`.

    `plain` blanks comments; `shape` also blanks string and character
    literals, so braces and attributes inside them never count.
    """
    plain: list[str] = []
    shape: list[str] = []
    cursor = 0
    position = 0
    while True:
        match = TOKEN_RE.search(text, position)
        if match is None:
            break
        start = match.start()
        if match.lastgroup == "block" or match.group("block"):
            depth = 1
            end = start + 2
            while depth and end < len(text):
                if text.startswith("/*", end):
                    depth += 1
                    end += 2
                elif text.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            kind = "comment"
        elif match.group("raw"):
            closing = '"' + match.group("hashes")
            found = text.find(closing, match.end())
            end = len(text) if found < 0 else found + len(closing)
            kind = "literal"
        elif match.group("line"):
            end = match.end()
            kind = "comment"
        else:
            end = match.end()
            kind = "literal"
        plain.append(text[cursor:start])
        shape.append(text[cursor:start])
        chunk = text[start:end]
        plain.append(blank(chunk) if kind == "comment" else chunk)
        shape.append(blank(chunk))
        cursor = end
        position = end
    plain.append(text[cursor:])
    shape.append(text[cursor:])
    return "".join(plain), "".join(shape)


def match_close(shape: str, start: int, opener: str, closer: str) -> int:
    depth = 0
    for index in range(start, len(shape)):
        character = shape[index]
        if character == opener:
            depth += 1
        elif character == closer:
            depth -= 1
            if depth == 0:
                return index
    return len(shape) - 1


def item_end(shape: str, start: int) -> int:
    depth = 0
    index = start
    while index < len(shape):
        character = shape[index]
        if character in BRACKETS:
            depth += 1
        elif character in ")]":
            depth = max(0, depth - 1)
        elif depth == 0 and character == ";":
            return index
        elif depth == 0 and character == "{":
            return match_close(shape, index, "{", "}")
        index += 1
    return len(shape) - 1


def test_spans(shape: str) -> list[tuple[int, int]]:
    if INNER_CFG_TEST_RE.search(shape):
        return [(0, len(shape))]
    spans: list[tuple[int, int]] = []
    for match in CFG_TEST_RE.finditer(shape):
        position = match.end()
        while True:
            attribute = ATTRIBUTE_RE.match(shape, position)
            if attribute is None:
                break
            position = match_close(shape, attribute.end() - 1, "[", "]") + 1
        while position < len(shape) and shape[position].isspace():
            position += 1
        if shape.startswith("{", position):
            spans.append((match.start(), match_close(shape, position, "{", "}") + 1))
        elif ITEM_RE.match(shape, position):
            spans.append((match.start(), item_end(shape, position) + 1))
    return spans


def inside(spans: list[tuple[int, int]], offset: int) -> bool:
    return any(start <= offset < end for start, end in spans)


def module_dir(path: Path) -> Path:
    if path.name in ("lib.rs", "main.rs", "mod.rs"):
        return path.parent
    return path.parent / path.stem


def attributes_before(plain: str, start: int) -> str:
    end = start
    head = plain[max(0, start - 64) : start]
    visibility = VISIBILITY_RE.search(head)
    if visibility:
        end = start - len(head) + visibility.start()
    attributes: list[str] = []
    while True:
        while end > 0 and plain[end - 1].isspace():
            end -= 1
        if end == 0 or plain[end - 1] != "]":
            break
        depth = 0
        index = end - 1
        while index >= 0:
            if plain[index] == "]":
                depth += 1
            elif plain[index] == "[":
                depth -= 1
                if depth == 0:
                    break
            index -= 1
        if index <= 0 or plain[index - 1] != "#":
            break
        attributes.append(plain[index - 1 : end])
        end = index - 1
    return " ".join(attributes)


def declared_files(path: Path, plain: str, spans: list[tuple[int, int]], only_test: bool) -> set[Path]:
    found: set[Path] = set()
    for match in MOD_DECL_RE.finditer(plain):
        attributes = attributes_before(plain, match.start())
        marked = bool(CFG_TEST_RE.search(attributes)) or inside(spans, match.start())
        if only_test and not marked:
            continue
        explicit = PATH_ATTR_RE.search(attributes)
        if explicit:
            found.add(path.parent / explicit.group(1))
            continue
        base = module_dir(path)
        name = match.group(1)
        for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
            found.add(candidate)
    for match in INCLUDE_RE.finditer(plain):
        if only_test and not inside(spans, match.start()):
            continue
        found.add(path.parent / match.group(1))
    return found


class Source:
    def __init__(self, path: Path) -> None:
        self.path = path
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            text = ""
        self.plain, self.shape = lex(text)
        self.spans = test_spans(self.shape)


def test_only_files(files: list[Path], raws: dict[Path, str]) -> set[Path]:
    known = set(files)
    queue: list[Path] = []
    marked: set[Path] = set()
    for path in files:
        raw = raws[path]
        if not (
            INNER_CFG_TEST_RE.search(raw)
            or (CFG_TEST_RE.search(raw) and (MOD_DECL_RE.search(raw) or INCLUDE_RE.search(raw)))
        ):
            continue
        source = Source(path)
        if not source.spans:
            continue
        if source.spans == [(0, len(source.shape))]:
            marked.add(path)
            queue.append(path)
            continue
        for declared in declared_files(path, source.plain, source.spans, True):
            if declared in known and declared not in marked:
                marked.add(declared)
                queue.append(declared)
    while queue:
        path = queue.pop()
        raw = raws[path]
        if not (MOD_DECL_RE.search(raw) or INCLUDE_RE.search(raw)):
            continue
        source = Source(path)
        for declared in declared_files(path, source.plain, source.spans, False):
            if declared in known and declared not in marked:
                marked.add(declared)
                queue.append(declared)
    return marked


def in_test_directory(path: Path, root: Path) -> bool:
    return any(part in ("tests", "benches") for part in path.relative_to(root).parts[:-1])


def spawn_pattern(shape: str) -> re.Pattern[str]:
    names = ["Command", *ALIAS_RE.findall(shape)]
    alternatives = "|".join(re.escape(name) for name in dict.fromkeys(names))
    return re.compile(
        rf'(?<![A-Za-z0-9_])(?:{alternatives})\s*::\s*new\s*\(\s*"git(?:\.exe)?"\s*\)'
    )


def source_hits(source: Source) -> list[int]:
    pattern = spawn_pattern(source.shape)
    lines: list[int] = []
    for match in pattern.finditer(source.plain):
        if source.shape[match.start()] == " ":
            continue
        if inside(source.spans, match.start()):
            continue
        lines.append(source.plain.count("\n", 0, match.start()) + 1)
    return lines


def file_hits(path: Path) -> list[int]:
    try:
        raw = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    if not GIT_LITERAL_RE.search(raw):
        return []
    return source_hits(Source(path))


def find_violations(
    root: Path = CRATES, base: Path = ROOT, allowlist: dict = ALLOWED
) -> list[str]:
    files = [
        path
        for path in sorted(root.rglob("*.rs"))
        if "target" not in path.relative_to(root).parts
    ]
    candidates = [path for path in files if not in_test_directory(path, root)]
    raws = {
        path: path.read_text(encoding="utf-8", errors="replace") for path in candidates
    }
    suspects = [path for path in candidates if GIT_LITERAL_RE.search(raws[path])]
    if not suspects:
        return []
    skipped = test_only_files(candidates, raws)
    violations: list[str] = []
    for path in suspects:
        if path in skipped:
            continue
        hits = file_hits(path)
        if not hits:
            continue
        rel = path.relative_to(base).as_posix()
        allowed = allowlist.get(rel)
        if allowed is not None and len(hits) == allowed[0]:
            continue
        for number in hits:
            violations.append(f"{rel}:{number}")
    return violations


def main() -> int:
    violations = find_violations()
    if violations:
        print(
            'bare Command::new("git") in product code (start from '
            "codewhale_dependencies::Git instead):",
            file=sys.stderr,
        )
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print('bare git check OK: no bare Command::new("git") outside the allowlist.')
    return 0


if __name__ == "__main__":
    sys.exit(main())
