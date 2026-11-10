#!/usr/bin/env python3
"""Deterministic crate-boundary gate (FEAT-014, runtime split RS-0).

The gate is table-driven: `BOUNDARY_RULES` names each guarded package, how its
dependency graph is read (`metadata` or `tree`, see below), the packages it
may never reach, and the source scan for its crate. The runtime/TUI split adds
the runtime -> UI reference ratchet (`scripts/split/module_graph.py`).

Dependency modes:

* ``metadata`` reads `cargo metadata --no-deps` and walks normal edges between
  workspace packages. Cheap and exact for "never reach this workspace crate".
* ``tree`` runs `cargo tree -p <package> -e normal,build --prefix none`, which
  resolves features for that package alone. `cargo metadata` unifies features
  across the workspace, so a rule like "the runtime never reaches ratatui"
  must use this mode once the TUI enables palette's `ratatui` feature.

Enforces the EPIC-006 boundary contract:

1. `codewhale-command-contract` and `codewhale-secrets` may not transitively
   depend on `codewhale-tui` (normal edges, via `cargo metadata`).
   Pure portable sanitization lives in `codewhale-sanitize`; contract, protocol
   and sanitizer also reject runtime and storage dependencies through tree rules.
2. `codewhale-command-contract` source may not import the concrete `App`,
   widget/renderer/view/event-loop surfaces, or `ratatui`/`crossterm`.
3. No composite `CommandContext` symbol (supertrait/struct/enum) may exist in
   the contract — the deep-dive D2 "no super-context" rule.
4. No boxed handler storage (`Box<`) in the contract — the D1/D4 fn-pointer
   transport rule.

The guard is hermetic: it reads `cargo metadata` and the contract source only;
it never starts the TUI and makes no network calls.

Usage:
    python3 scripts/check-command-crate-boundaries.py           # enforce
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

REPO_ROOT = Path(__file__).resolve().parent.parent
CONTRACT_DIR = REPO_ROOT / "crates" / "command-contract" / "src"
CONTRACT_PACKAGE = "codewhale-command-contract"
FORBIDDEN_TUI_PACKAGE = "codewhale-tui"


@dataclass(frozen=True)
class BoundaryRule:
    """One guarded package: how to read its graph and what it may not reach."""

    package: str
    mode: str  # "metadata" or "tree"
    forbidden_packages: tuple[str, ...]
    reason: str
    source_dir: Path | None = None
    source_scan: Callable[[str, str], list] | None = None


RUNTIME_PACKAGE = "codewhale-runtime"
RUNTIME_DIR = REPO_ROOT / "crates" / "runtime" / "src"
RUNTIME_FORBIDDEN_PACKAGES = (
    "codewhale-tui",
    "codewhale-cli",
    "ratatui",
    "ratatui-core",
    "ratatui-widgets",
    "crossterm",
    "ansi-to-tui",
    "codewhale-tui-kit",
    "codewhale-ratatui",
)
RUNTIME_FORBIDDEN_SOURCE = [
    (re.compile(r"\b(ratatui|crossterm|codewhale_tui)::"), "terminal UI path"),
    (re.compile(r"^\s*(pub\s+)?use\s+(ratatui|crossterm|codewhale_tui)\b"), "terminal UI import"),
    (re.compile(r"include_(str|bytes)!\(\s*\"[^\"]*\.\./tui/"), "include reaching into crates/tui"),
]

# Filled in below once the scan functions exist.
BOUNDARY_RULES: tuple[BoundaryRule, ...] = ()

# Import lines that must never appear in the contract (narrowly scoped: real
# imports only, comments never match because they do not start with `use`).
FORBIDDEN_IMPORT_PATTERNS = [
    (re.compile(r"^\s*(pub\s+)?use\s+codewhale_tui\b"), "codewhale-tui import"),
    (re.compile(r"^\s*(pub\s+)?use\s+ratatui\b"), "ratatui (widget) import"),
    (re.compile(r"^\s*(pub\s+)?use\s+crossterm\b"), "crossterm (terminal) import"),
    (re.compile(r"^\s*(pub\s+)?use\s+.*\bApp\b"), "concrete App import"),
    (re.compile(r"^\s*(pub\s+)?use\s+.*\bBuffer\b"), "render buffer import"),
    (re.compile(r"^\s*(pub\s+)?use\s+.*\bWidget\b"), "widget import"),
    (re.compile(r"^\s*(pub\s+)?use\s+.*\bViewStack\b"), "view-stack import"),
    (re.compile(r"^\s*(pub\s+)?use\s+.*\bEventLoop\b"), "event-loop import"),
]

# Composite super-context symbols (D2: exactly `CommandContext`, not the
# plural envelope `CommandContexts` nor facet names like `CommandModelContext`).
COMPOSITE_SYMBOL_PATTERN = re.compile(
    r"^\s*(pub\s+)?(trait|struct|enum)\s+CommandContext\b"
)
# Boxed handler/closure storage (D1: fn pointers only).
BOXED_STORAGE_PATTERN = re.compile(r"\bBox\s*<")


class BoundaryViolation:
    """One deterministic boundary violation with an actionable diagnostic."""

    def __init__(self, category: str, location: str, detail: str) -> None:
        self.category = category
        self.location = location
        self.detail = detail

    def __str__(self) -> str:
        return f"{self.category}: {self.location}: {self.detail}"


def load_workspace_metadata() -> dict:
    """Load the locked workspace dependency graph via cargo metadata."""
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--no-deps",
        ],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def dependency_graph(metadata: dict) -> dict[str, set[str]]:
    """Map package name -> set of direct NORMAL dependency package names.

    Dev- and build-dependencies are excluded: the gate contract checks normal
    transitive edges (a dev-dependency on the TUI, e.g. for acceptance
    harnesses, must not trip the boundary).
    """
    graph: dict[str, set[str]] = {}
    for package in metadata["packages"]:
        deps = set()
        for dep in package.get("dependencies", []):
            # kind is None for normal dependencies, "dev" or "build" otherwise.
            if dep.get("kind") is not None:
                continue
            name = dep.get("name")
            if name:
                deps.add(name)
        graph[package["name"]] = deps
    return graph


def reaches(package: str, forbidden: set[str], graph: dict[str, set[str]]) -> str | None:
    """The first forbidden package `package` transitively reaches, if any."""
    seen: set[str] = set()
    stack = list(graph.get(package, set()))
    while stack:
        name = stack.pop()
        if name in forbidden:
            return name
        if name in seen:
            continue
        seen.add(name)
        stack.extend(graph.get(name, set()))
    return None


def reaches_tui(package: str, graph: dict[str, set[str]]) -> bool:
    """Whether `package` transitively reaches the forbidden TUI package."""
    return reaches(package, {FORBIDDEN_TUI_PACKAGE}, graph) is not None


def metadata_rules() -> tuple[BoundaryRule, ...]:
    return tuple(rule for rule in BOUNDARY_RULES if rule.mode == "metadata")


def tree_rules() -> tuple[BoundaryRule, ...]:
    return tuple(rule for rule in BOUNDARY_RULES if rule.mode == "tree")


def check_dependency_graph(graph: dict[str, set[str]]) -> list[BoundaryViolation]:
    """No metadata-mode package may reach a forbidden package through normal edges."""
    violations: list[BoundaryViolation] = []
    for rule in metadata_rules():
        package = rule.package
        if package not in graph:
            violations.append(
                BoundaryViolation(
                    "dependency-graph",
                    package,
                    "workspace package missing from the cargo metadata graph",
                )
            )
            continue
        hit = reaches(package, set(rule.forbidden_packages), graph)
        if hit:
            violations.append(
                BoundaryViolation(
                    "dependency-graph",
                    package,
                    f"transitively depends on {hit} ({rule.reason})",
                )
            )
    return violations


def cargo_tree_packages(package: str) -> set[str]:
    """Package names in `cargo tree -p <package> -e normal,build` (per-package features)."""
    result = subprocess.run(
        # `--target all`: a dependency behind `cfg(windows)` counts too, not
        # only the ones the host target resolves.
        [
            "cargo", "tree", "-p", package, "-e", "normal,build", "--target", "all",
            "--prefix", "none", "--locked",
        ],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return parse_cargo_tree(result.stdout)


def parse_cargo_tree(text: str) -> set[str]:
    names: set[str] = set()
    for line in text.splitlines():
        parts = line.split()
        if parts:
            names.add(parts[0])
    return names


def check_tree_packages(rule: BoundaryRule, names: set[str]) -> list[BoundaryViolation]:
    """A tree-mode package's resolved dependency set must avoid its forbidden list."""
    return [
        BoundaryViolation(
            "dependency-tree",
            rule.package,
            f"cargo tree reaches {name} ({rule.reason})",
        )
        for name in sorted(names & set(rule.forbidden_packages))
    ]


def check_contract_source_text(text: str, display_path: str) -> list[BoundaryViolation]:
    """Scan one source text for forbidden imports/symbols (hermetic test hook)."""
    violations: list[BoundaryViolation] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        for pattern, label in FORBIDDEN_IMPORT_PATTERNS:
            if pattern.match(stripped):
                violations.append(
                    BoundaryViolation(
                        "source-scan",
                        f"{display_path}:{line_no}",
                        f"forbidden {label}: {stripped}",
                    )
                )
        if COMPOSITE_SYMBOL_PATTERN.match(stripped):
            violations.append(
                BoundaryViolation(
                    "source-scan",
                    f"{display_path}:{line_no}",
                    f"composite CommandContext symbol (D2 forbids super-contexts): {stripped}",
                )
            )
        if BOXED_STORAGE_PATTERN.search(stripped):
            violations.append(
                BoundaryViolation(
                    "source-scan",
                    f"{display_path}:{line_no}",
                    f"boxed storage in the contract (D1 requires fn pointers): {stripped}",
                )
            )
    return violations


def check_source_dir(rule: BoundaryRule) -> list[BoundaryViolation]:
    """Scan one rule's production source for forbidden imports and symbols."""
    assert rule.source_dir is not None and rule.source_scan is not None
    if not rule.source_dir.is_dir():
        return [
            BoundaryViolation(
                "source-scan",
                str(rule.source_dir),
                f"{rule.package} src directory missing",
            )
        ]
    violations: list[BoundaryViolation] = []
    for path in sorted(rule.source_dir.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        rel = path.relative_to(REPO_ROOT)
        violations.extend(rule.source_scan(text, str(rel)))
    return violations


def check_runtime_source_text(text: str, display_path: str) -> list[BoundaryViolation]:
    """Runtime source may not name a terminal UI crate (comments ignored)."""
    violations: list[BoundaryViolation] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        code = line.split("//", 1)[0]
        if not code.strip():
            continue
        for pattern, label in RUNTIME_FORBIDDEN_SOURCE:
            if pattern.search(code):
                violations.append(
                    BoundaryViolation(
                        "source-scan",
                        f"{display_path}:{line_no}",
                        f"forbidden {label} in codewhale-runtime: {line.strip()}",
                    )
                )
    return violations


def check_contract_source() -> list[BoundaryViolation]:
    """Scan contract production source for forbidden imports and symbols."""
    return check_source_dir(next(r for r in BOUNDARY_RULES if r.package == CONTRACT_PACKAGE))


def load_runtime_ratchet():
    """Import scripts/split/module_graph.py (the runtime -> UI ratchet)."""
    path = REPO_ROOT / "scripts" / "split" / "module_graph.py"
    spec = importlib.util.spec_from_file_location("runtime_module_graph", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def check_runtime_ratchet(baseline_ref: str | None = None) -> list[BoundaryViolation]:
    """Runtime -> UI references may only go down (docs/design/TUI_DECONSTRUCTION.md, runtime split)."""
    problems = load_runtime_ratchet().check(baseline_ref=baseline_ref)
    return [BoundaryViolation("runtime-ratchet", "scripts/runtime-boundary-baseline.json", p) for p in problems]


def run_checks(
    metadata: dict | None = None,
    tree: Callable[[str], set[str]] | None = None,
    ratchet: bool = True,
    baseline_ref: str | None = None,
) -> list[BoundaryViolation]:
    """Run all boundary checks; return the collected violations."""
    graph = dependency_graph(metadata) if metadata is not None else dependency_graph(
        load_workspace_metadata()
    )
    violations = check_dependency_graph(graph)
    tree = tree or cargo_tree_packages
    for rule in tree_rules():
        violations.extend(check_tree_packages(rule, tree(rule.package)))
    for rule in BOUNDARY_RULES:
        if rule.source_dir is not None:
            violations.extend(check_source_dir(rule))
    if ratchet:
        violations.extend(check_runtime_ratchet(baseline_ref))
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n\n")[0])
    parser.add_argument(
        "--baseline-ref",
        help="git revision (CI: the PR base) whose runtime ratchet baseline may not be exceeded",
    )
    args = parser.parse_args(argv)
    violations = run_checks(baseline_ref=args.baseline_ref)
    if violations:
        print("[command-crate-boundaries] FAIL", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print(
        f"[command-crate-boundaries] PASS: "
        f"{', '.join(rule.package for rule in BOUNDARY_RULES)} avoid their forbidden "
        "packages; no forbidden import, composite context, or boxed handler in the "
        "contract; runtime -> UI ratchet holds"
    )
    return 0


BOUNDARY_RULES = (
    # The contract carries the portable command shapes (EPIC-006).
    BoundaryRule(
        CONTRACT_PACKAGE,
        "metadata",
        (FORBIDDEN_TUI_PACKAGE,),
        "the command contract must stay UI-free",
        CONTRACT_DIR,
        check_contract_source_text,
    ),
    # Storage remains UI-free; portable callers use codewhale-sanitize directly.
    BoundaryRule(
        "codewhale-secrets",
        "metadata",
        (FORBIDDEN_TUI_PACKAGE,),
        "secret storage must stay UI-free",
    ),
    *(BoundaryRule(
        package, "tree",
        ("codewhale-tui", "codewhale-core", "codewhale-runtime", "codewhale-config",
         "codewhale-state", "codewhale-mcp", "codewhale-hooks", "codewhale-secrets",
         "reqwest", "tokio", "rusqlite", "keyring", "dbus", "zbus", "ratatui", "crossterm"),
        "portable shapes and sanitization must not import host services",
    ) for package in ("codewhale-command-contract", "codewhale-commands", "codewhale-protocol", "codewhale-app-server-protocol", "codewhale-sanitize")),
    # The headless runtime split out of the TUI (docs/design/TUI_DECONSTRUCTION.md):
    # never a terminal UI crate or library, checked with per-package feature
    # resolution because the TUI turns on palette's `ratatui` feature.
    BoundaryRule(
        RUNTIME_PACKAGE,
        "tree",
        RUNTIME_FORBIDDEN_PACKAGES,
        "the runtime must not link terminal UI code",
        RUNTIME_DIR,
        check_runtime_source_text,
    ),
)
TUI_FREE_PACKAGES = tuple(rule.package for rule in metadata_rules())


if __name__ == "__main__":
    sys.exit(main())
