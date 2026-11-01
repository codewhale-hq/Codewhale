#!/usr/bin/env python3
"""check-lexicon.py — warn when user-facing English copy drifts from the lexicon.

The product vocabulary is one word per concept across the TUI, the app, the
site and the docs: Agent and Fleet; Plan, Work and Operate; Permissions
(Ask, Auto-Review, Full Access); Tasks panel; Making room; Settings; the
presence words; Thinking. This script greps the English sources a person
actually reads for the words that decision retires, plus the engineering notes experience mark 5 keeps off product
surfaces (issue keys, HTTP routes, "Ctrl/Cmd").

Scanned:
  - crates/localization/locales/en.json            (values only, never keys)
  - web/lib/i18n/dictionaries/en/*.ts              (string literals)
  - web/lib/content/*.ts                           (string literals)
  - crates/tui/src/**/*.rs                         (string literals)

The Rust scan reads string literals outside comments, test files and
`#[cfg(test)]` / `#[cfg(all(test, ...))]` modules and functions. It skips
literals passed to log, tracing, panic, assert and expect calls (developer
text, not copy), literals without a space (identifiers, keys, paths),
model-facing text (tools/, prompts/, runtime-event envelopes), and code
inside copy: slash commands, --flags, config keys and usage alternatives.

Known limits: it does not see copy assembled outside crates/tui/src or split
across push_str calls, nor clap `--help` text written as `///` doc comments
(comments are masked). It cannot tell an error the user reads from one only a
log shows, so expect some findings to be internal text worth an allow entry.
The subcommand names in RUST_SUBCOMMANDS are skipped after a slash command,
so a retired term used as one of those subcommands is not reported. The Rust
scan adds about 2.5 s to a run.

It is WARN-ONLY: it prints findings and exits 0 so it can run from
scripts/preflight.sh without turning a push red while the sweep finishes.
Pass --strict to exit 1 on any finding (for a local ratchet or a future CI
gate). Parser aliases, config keys and locale message identifiers are code,
not copy, and are out of scope.

    python3 scripts/check-lexicon.py            # warn
    python3 scripts/check-lexicon.py --strict   # fail on findings
    python3 scripts/check-lexicon.py --summary  # counts only
    python3 scripts/check-lexicon.py --rust-only  # skip locale and web
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EN_LOCALE = ROOT / "crates" / "localization" / "locales" / "en.json"
WEB_GLOBS = (
    "web/lib/i18n/dictionaries/en/*.ts",
    "web/lib/content/*.ts",
)

# (label, use-instead, compiled pattern). Case matters where the retired word
# is also an ordinary English word ("Act" the mode vs. "act" the verb).
RULES: list[tuple[str, str, re.Pattern[str]]] = [
    # §19 modes
    ("Act", "Work", re.compile(r"\bAct\b|\bACT\b")),
    ("read-only lane", "Plan", re.compile(r"read-only lane", re.I)),
    ("agent mode", "Work", re.compile(r"\bagent mode\b", re.I)),
    ("Fleet mode", "Operate", re.compile(r"\bfleet mode\b", re.I)),
    ("operator", "Coordinator (or Operate for the mode)", re.compile(r"\boperator\b", re.I)),
    # §19 permissions
    ("posture", "Permissions", re.compile(r"\bpostures?\b", re.I)),
    ("approval policy", "Permissions", re.compile(r"\b(approval|permission) policy\b", re.I)),
    # §16 / §19 Fleet and agents
    ("roster", "Fleet", re.compile(r"\broster\b", re.I)),
    ("worker", "agent", re.compile(r"(?<!Cloudflare )\bworkers?\b", re.I)),
    ("sub-agent", "agent", re.compile(r"\bsub-?agents?\b", re.I)),
    ("lane", "agent", re.compile(r"\blanes?\b", re.I)),
    ("leader", "Coordinator", re.compile(r"\bleader\b", re.I)),
    ("consultant", "Advisor", re.compile(r"\bconsultants?\b", re.I)),
    # §19 surfaces and states
    ("Work bar", "Tasks panel", re.compile(r"\bwork ?bar\b|\bwork dock\b|\brail panel\b", re.I)),
    ("Bash", "Command", re.compile(r"\bBash\b")),
    ("MCP Read/Action", "Connected app", re.compile(r"\bMCP (Read|Action)\b")),
    ("Deny this call", "Don't allow", re.compile(r"Deny this call|\(this kind\)")),
    ("abort", "Stop", re.compile(r"\babort(ed|s|ing)?\b", re.I)),
    ("compaction", "Making room", re.compile(r"\bauto-?compact\w*|\bcompaction\b", re.I)),
    ("waiting on you", "needs you", re.compile(r"waiting on you", re.I)),
    ("unobserved", "resting", re.compile(r"\bunobserved\b", re.I)),
    ("Reasoning", "Thinking", re.compile(r"\bReasoning\b")),
    ("charter", "Constitution", re.compile(r"\bcharter\b", re.I)),
    # Experience mark 5: no engineering notes on product surfaces
    ("issue key", "(remove)", re.compile(r"\bAPPS-\d+\b|\bSHA-(?!(1|256|384|512)\b)\d+\b|\(#\d{3,}\)")),
    ("HTTP route", "(describe what works)", re.compile(r"\b(GET|POST|PUT|DELETE|PATCH) /")),
    ("Ctrl/Cmd", "one key notation", re.compile(r"Ctrl/Cmd")),
]

# A settings screen titled "Config" (§19: Settings). Exact values only, so
# "Config file:" and "config.toml" stay legal.
CONFIG_TITLE = re.compile(r"^\s*Config\s*$")

# Deliberate, reviewed exceptions: {source-relative path: {key or literal: {labels}}}.
# Keep this short; every entry is a promise that the word is the right one.
ALLOW: dict[str, dict[str, set[str]]] = {
    "crates/localization/locales/en.json": {
        # Names the compatibility slash command the user typed.
        "CmdSubagentsDescription": {"worker", "sub-agent"},
        "HomeQuickSubagents": {"worker"},
    },
}

# Double-quoted, single-quoted or template string literals in TS sources.
TS_STRING = re.compile(r'"((?:[^"\\\n]|\\.)*)"|\'((?:[^\'\\\n]|\\.)*)\'|`((?:[^`\\]|\\.)*)`')


PLACEHOLDER = re.compile(r"\{[A-Za-z_][A-Za-z0-9_]*\}")


def findings_for(text: str) -> list[tuple[str, str]]:
    # `{posture}` is a substitution slot, not a word the reader sees.
    text = PLACEHOLDER.sub("", text)
    hits = [(label, instead) for label, instead, rx in RULES if rx.search(text)]
    if CONFIG_TITLE.match(text):
        hits.append(("Config", "Settings"))
    return hits


def scan_locale(path: Path):
    rel = str(path.relative_to(ROOT))
    allow = ALLOW.get(rel, {})
    data = json.loads(path.read_text(encoding="utf-8"))
    for key, value in data.items():
        if not isinstance(value, str):
            continue
        for label, instead in findings_for(value):
            if label in allow.get(key, set()):
                continue
            yield rel, key, label, instead, value


def scan_ts(path: Path):
    rel = str(path.relative_to(ROOT))
    allow = ALLOW.get(rel, {})
    text = path.read_text(encoding="utf-8")
    for lineno, line in enumerate(text.splitlines(), 1):
        stripped = line.lstrip()
        if stripped.startswith(("//", "*", "/*", "import ", "export type", "type ")):
            continue
        for m in TS_STRING.finditer(line):
            value = next(g for g in m.groups() if g is not None)
            # Skip identifiers, paths and URLs; copy has a space in it.
            if " " not in value:
                continue
            for label, instead in findings_for(value):
                if label in allow.get(value, set()):
                    continue
                yield rel, f"L{lineno}", label, instead, value


# --- Rust string literals -------------------------------------------------

RUST_ROOTS = [ROOT / "crates" / name / "src" for name in ("tui", "runtime")]

# Calls whose string arguments are developer text: logs, panics, assertions.
RUST_INTERNAL_CALLEE = re.compile(
    r"(\b(trace|debug|info|warn|error|panic|unreachable|todo|unimplemented"
    r"|assert\w*|debug_assert\w*|log_\w+)!"
    r"|\b(tracing|log|logging)::\w+(::\w+)*!?"
    r"|\.(expect|expect_err|context|with_context)"
    r"|#\[(?!(error|arg|command|value)\b)\w+(::\w+)*)\s*$"
)
RUST_RAW_START = re.compile(r'b?r(#*)"')
# `#[cfg(test)]` or `#[cfg(all(test, ...))]`, any further attributes, then a
# module or function whose body is test code.
RUST_TEST_MOD = re.compile(
    r"#\[cfg\((?:test|all\((?:[^\]]*,\s*)?test\b[^\]]*\))\)\]"
    r"(?:\s*#\[[^\]]*\])*\s*(?:pub(?:\([\w:]+\))?\s+)?"
    r"(?:mod\s+\w+|(?:const\s+|async\s+|unsafe\s+)*fn\s+\w+[^{;]*)\s*\{"
)

# Compatibility subcommand names that keep a retired word: `/config subagents`,
# `/constitution posture`, `/fleet workers`. They are typed, not read.
RUST_SUBCOMMANDS = ("subagents", "posture", "workers")

# Code inside copy: `backticks`, /commands (with a known subcommand or an
# a|b alternative list), --flags, [sections], [a|b] usage alternatives, dotted
# or snake_case keys (including `key.{slot}`), <!-- markers --> and
# <placeholders> name things the user types, not words they read. A path
# after an HTTP verb is left in place so the HTTP-route rule still sees it.
RUST_CODE_TOKEN = re.compile(
    r"`[^`]*`|<!--.*?-->|<[\w-]+>"
    r"|(?<![\w/])(?<!GET )(?<!PUT )(?<!POST )(?<!PATCH )(?<!DELETE )/[a-z][\w-]*"
    rf"(?: (?:{'|'.join(RUST_SUBCOMMANDS)})\b| [a-z][\w-]*(?:\|[\w-]+)+)?"
    r"|(?<![\w-])--[a-z][\w-]*"
    r"|\[[\w.\-]+\]|\[[^\]\n]*\|[^\]\n]*\]"
    r"|\b\w+(?:[._](?:\w+|\{\w*\}))+"
)

# Model-facing text: tool descriptions and prompts are read by the model, not
# the person. Their vocabulary follows the tool contract, not §19.
RUST_MODEL_FACING = (
    "crates/engine/src/tools/",
    "crates/engine/src/prompts/",
    # Runtime-event envelopes and their restore projection for the model.
    "crates/engine/src/runtime_handoff.rs",
)
# A literal carrying a runtime-event envelope is model-facing wherever it lives.
RUST_MODEL_MARKER = "<codewhale:"

# Deliberate, reviewed Rust exceptions: {path: {literal-prefix: {labels}}}.
# A literal matches an entry when it starts with the entry's text. Keep each
# entry honest: an ordinary English word, not a retired product term.
RUST_ALLOW: dict[str, dict[str, set[str]]] = {
    # "roster" is a provider's model list here, not a Fleet.
    "crates/runtime/src/config.rs": {"Model '{trimmed}' is not in OpenCode Go": {"roster"}},
    "crates/tui/src/lib.rs": {"pinned id `{model}` is absent from": {"roster"}},
    "crates/tui/src/tui/model_picker.rs": {"custom · OAuth roster": {"roster"}},
    # A tool name in an error, not the retired Bash label.
    "crates/engine/src/core/engine.rs": {"tool 'Bash' is not registered": {"Bash"}},
    # A network error, not the user stopping a turn.
    "crates/tui/src/commands/contract.rs": {"connection aborted": {"abort"}},
    # Background threads, not agents.
    "crates/tui/src/tui/window_control.rs": {"window worker panicked": {"worker"}},
    "crates/tui/src/tui/ui/apply.rs": {"the persistence worker is unavailable": {"worker"}},
    # Prompts sent to the model from UI code.
    "crates/tui/src/tui/setup/fleet_draft.rs": {"": {"worker", "posture"}},
    "crates/tui/src/tui/setup/model_draft.rs": {"": {"approval policy"}},
    "crates/tui/src/commands/groups/core/agent.rs": {"Launch one sub-agent": {"sub-agent"}},
    "crates/tui/src/commands/groups/core/workflow.rs": {"{WORKFLOW_DRAFT_INSTRUCTION_PREFIX}": {"worker"}},
    "crates/engine/src/operate.rs": {"Keep Operate alive": {"worker"}},
    "crates/engine/src/fleet/worker_runtime.rs": {"- Use the policy-gated tools": {"worker"}},
    "crates/engine/src/fleet/roster.rs": {"Give the operator a direct second opinion": {"operator"}},
    "crates/engine/src/core/engine/context.rs": {
        "[sub-agent result summarized for parent context]": {"sub-agent"},
        "- ... {} more sub-agent result(s)": {"sub-agent"},
    },
    "crates/engine/src/compaction.rs": {
        "--- Additional instructions from the operator": {"operator"},
    },
}


def is_rust_test_file(path: Path) -> bool:
    name = path.name
    return (
        name == "tests.rs"
        or name.endswith("_tests.rs")
        or name.startswith("test_")
        or "tests" in path.relative_to(ROOT).parts[:-1]
    )


def rust_tokens(text: str):
    """Yield (kind, start, end, value) for comments and string literals."""
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            yield "comment", i, j, ""
            i = j
            continue
        if text.startswith("/*", i):
            start, depth, i = i, 1, i + 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif text.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            yield "comment", start, i, ""
            continue
        prev = text[i - 1] if i else " "
        m = RUST_RAW_START.match(text, i) if c in "br" else None
        if m and not (prev.isalnum() or prev == "_"):
            close = '"' + m.group(1)
            j = text.find(close, m.end())
            j = n if j < 0 else j
            yield "string", i, j + len(close), text[m.end():j]
            i = j + len(close)
            continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            yield "string", i, j + 1, text[i + 1:j]
            i = j + 1
            continue
        if c == "'":
            # Char literals ('"', '\'', '→'); anything else is a lifetime.
            if text.startswith("\\", i + 1):
                j = text.find("'", i + 3)
                j = (j + 1) if j > 0 else n
                yield "char", i, j, ""
                i = j
                continue
            if i + 2 < n and text[i + 2] == "'":
                yield "char", i, i + 3, ""
                i += 3
                continue
        i += 1


def rust_internal(masked: str, start: int) -> bool:
    """True when the literal at `start` is an argument of a developer-text call."""
    depth, j = 0, start - 1
    while j >= 0:
        ch = masked[j]
        if ch in ")]":
            depth += 1
        elif ch in "([":
            if depth:
                depth -= 1
            elif RUST_INTERNAL_CALLEE.search(masked[max(0, j - 60):j]):
                return True
        elif ch in ";{}" and depth == 0:
            return False
        j -= 1
    return False


def scan_rust(path: Path):
    rel = str(path.relative_to(ROOT))
    if rel.startswith(RUST_MODEL_FACING):
        return
    allow = RUST_ALLOW.get(rel, {})
    text = path.read_text(encoding="utf-8")
    tokens = list(rust_tokens(text))
    chars = list(text)
    for _, start, end, _ in tokens:
        for k in range(start, min(end, len(chars))):
            if chars[k] != "\n":
                chars[k] = " "
    masked = "".join(chars)
    # Drop inline `#[cfg(test)] mod name { ... }` blocks.
    test_spans = []
    for m in RUST_TEST_MOD.finditer(masked):
        depth, j = 1, m.end()
        while j < len(masked) and depth:
            depth += {"{": 1, "}": -1}.get(masked[j], 0)
            j += 1
        test_spans.append((m.start(), j))
    for kind, start, _, value in tokens:
        if kind != "string" or " " not in value or RUST_MODEL_MARKER in value:
            continue
        if any(a <= start < b for a, b in test_spans):
            continue
        if rust_internal(masked, start):
            continue
        for label, instead in findings_for(RUST_CODE_TOKEN.sub(" ", value)):
            if any(value.startswith(k) and label in v for k, v in allow.items()):
                continue
            lineno = text.count("\n", 0, start) + 1
            yield rel, f"L{lineno}", label, instead, value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--strict", action="store_true", help="exit 1 on any finding")
    parser.add_argument("--summary", action="store_true", help="print counts only")
    parser.add_argument("--rust-only", action="store_true", help="scan only the Rust sources")
    args = parser.parse_args()

    found = []
    if not args.rust_only:
        if EN_LOCALE.exists():
            found.extend(scan_locale(EN_LOCALE))
        for pattern in WEB_GLOBS:
            for path in sorted(ROOT.glob(pattern)):
                if path.name.endswith(".test.ts"):
                    continue
                found.extend(scan_ts(path))
    for path in sorted(path for root in RUST_ROOTS for path in root.rglob("*.rs")):
        if is_rust_test_file(path):
            continue
        found.extend(scan_rust(path))

    counts = Counter(label for _, _, label, _, _ in found)
    if not args.summary:
        for rel, where, label, instead, value in found:
            excerpt = value if len(value) <= 110 else value[:107] + "..."
            print(f"{rel}:{where}: '{label}' -> {instead}: {excerpt!r}")
    if found:
        tally = ", ".join(f"{label} {n}" for label, n in counts.most_common())
        print(f"[lexicon] {len(found)} finding(s): {tally}")
        if args.strict:
            return 1
        print("[lexicon] warn-only; pass --strict to fail")
    else:
        print("[lexicon] OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
