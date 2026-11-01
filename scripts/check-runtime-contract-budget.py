#!/usr/bin/env python3
"""Enforce one-way ceilings on Codewhale's provider-free runtime contract.

The default invocation runs ``measure-runtime-contract.py`` with Cargo forced
offline. Pass ``--receipt`` to check an existing JSON receipt without compiling,
which also keeps this checker's unit tests hermetic.

Usage:
    python3 scripts/check-runtime-contract-budget.py
    python3 scripts/check-runtime-contract-budget.py --receipt receipt.json
    python3 scripts/check-runtime-contract-budget.py --update
    python3 scripts/check-runtime-contract-budget.py --update --allow-increase

``--update`` alone only locks in decreases and refuses growth. A PR whose
change intentionally grows the contract, or changes a structural identity,
runs ``--update --allow-increase`` to rewrite the budget from its own
measurement so the fix lands in the same PR instead of turning main red after
merge. Existing ``_``-prefixed history notes are preserved either way.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import shlex
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Sequence

REPO_ROOT = Path(__file__).resolve().parent.parent
BUDGET_PATH = REPO_ROOT / "scripts" / "runtime-contract-budget.json"
MEASURE_SCRIPT = REPO_ROOT / "scripts" / "measure-runtime-contract.py"
RECEIPT_KIND = "codewhale.runtime_contract_receipt"
BUDGET_KIND = "codewhale.runtime_contract_budget"
SCHEMA_VERSION = 1
REPRESENTATIVE_FIXTURE_ID = "representative-v1"
TOOL_SURFACE_PROFILE = "production-default-builtins-no-mcp-no-host-interpreters-bash-v2"

MetricPath = tuple[str, ...]
MetricResult = tuple[str, str, int, int]

VISIBLE_MODES = (("plan", "Plan"), ("act", "Act"), ("operate", "Operate"))
REPRESENTATIVE_STAGES = (
    ("base", "base"),
    ("project", "project-authority"),
    ("instructions", "configured-instructions"),
    ("skill", "skill"),
    ("memory", "memory"),
    ("goal", "goal"),
    ("handoff", "handoff"),
)
TOOL_SURFACES = (("full", "full"), ("active", "active"))

METRICS: tuple[tuple[MetricPath, str], ...] = (
    *(
        (("system_prompt", "modes", mode, field), f"{label} {description}")
        for mode, label in VISIBLE_MODES
        for field, description in (
            ("system_prompt_bytes", "system-prompt bytes"),
            ("system_prompt_tokens_est", "system-prompt estimated tokens"),
            ("system_prompt_blocks", "system-prompt blocks"),
            ("mode_instructions_bytes", "mode-instruction bytes"),
            ("mode_instructions_tokens_est", "mode-instruction estimated tokens"),
        )
    ),
    *(
        (
            ("representative_context", "stages", stage, "bytes"),
            f"representative {label} stage bytes",
        )
        for stage, label in REPRESENTATIVE_STAGES
    ),
    *(
        (
            ("representative_context", "stages", stage, "delta_bytes"),
            f"representative {label} stage delta bytes",
        )
        for stage, label in REPRESENTATIVE_STAGES[1:]
    ),
    (
        ("representative_context", "total_bytes"),
        "representative total bytes",
    ),
    (
        ("representative_context", "total_tokens_est"),
        "representative estimated tokens",
    ),
    (
        ("representative_context", "system_prompt_blocks"),
        "representative system-prompt blocks",
    ),
    *(
        (
            ("tool_catalog", "modes", mode, surface, field),
            f"{label} {surface_label} {description}",
        )
        for mode, label in VISIBLE_MODES
        for surface, surface_label in TOOL_SURFACES
        for field, description in (
            ("tools", "tool count"),
            ("bytes", "tool-schema bytes"),
            ("tokens_est", "tool-schema estimated tokens"),
        )
    ),
    (
        ("skill_discovery", "first_delta", "root_discovery_calls"),
        "first unchanged-turn root discovery calls",
    ),
    (
        ("skill_discovery", "first_delta", "directories_visited"),
        "first unchanged-turn directories visited",
    ),
    (
        ("skill_discovery", "first_delta", "skill_md_read_attempts"),
        "first unchanged-turn SKILL.md read attempts",
    ),
    (
        ("skill_discovery", "second_delta", "root_discovery_calls"),
        "second unchanged-turn root discovery calls",
    ),
    (
        ("skill_discovery", "second_delta", "directories_visited"),
        "second unchanged-turn directories visited",
    ),
    (
        ("skill_discovery", "second_delta", "skill_md_read_attempts"),
        "second unchanged-turn SKILL.md read attempts",
    ),
)

IDENTITIES: tuple[tuple[MetricPath, str], ...] = (
    (("tool_catalog", "surface_profile"), "tool surface profile"),
    (("tool_catalog", "execution_shell"), "tool fixture shell"),
    *(
        (
            ("tool_catalog", "modes", mode, surface, field),
            f"{label} {surface_label} tool {description}",
        )
        for mode, label in VISIBLE_MODES
        for surface, surface_label in TOOL_SURFACES
        for field, description in (
            ("tool_names", "names"),
            ("identity_sha256", "identity digest"),
        )
    ),
    *(
        (
            ("representative_context", "stages", stage, "identity_sha256"),
            f"representative {label} stage identity digest",
        )
        for stage, label in REPRESENTATIVE_STAGES
    ),
)


class RuntimeContractError(ValueError):
    """A receipt or budget is missing a required, well-typed metric."""


def load_json(path: Path, kind: str) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise RuntimeContractError(f"missing {kind}: {path}") from error
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeContractError(f"invalid {kind} {path}: {error}") from error
    if not isinstance(document, dict):
        raise RuntimeContractError(f"invalid {kind} {path}: top level must be an object")
    return document


def validate_document(
    document: dict[str, Any], expected_kind: str, source: str
) -> None:
    actual_kind = document.get("document_kind")
    if actual_kind != expected_kind:
        raise RuntimeContractError(
            f"{source} document_kind must be `{expected_kind}`, got {actual_kind!r}"
        )
    version = document.get("schema_version")
    if (
        isinstance(version, bool)
        or not isinstance(version, int)
        or version != SCHEMA_VERSION
    ):
        raise RuntimeContractError(
            f"{source} schema_version must be {SCHEMA_VERSION}, got {version!r}"
        )


def required_value(document: dict[str, Any], path: MetricPath, kind: str) -> Any:
    value: Any = document
    dotted = ".".join(path)
    for part in path:
        if not isinstance(value, dict) or part not in value:
            raise RuntimeContractError(f"{kind} is missing required field `{dotted}`")
        value = value[part]
    return value


def tool_identity_digest(names: list[str]) -> str:
    return hashlib.sha256("\0".join(names).encode("utf-8")).hexdigest()


def validate_identity_structure(document: dict[str, Any], kind: str) -> None:
    profile = required_value(document, ("tool_catalog", "surface_profile"), kind)
    if profile != TOOL_SURFACE_PROFILE:
        raise RuntimeContractError(
            f"{kind} tool surface_profile must be `{TOOL_SURFACE_PROFILE}`, "
            f"got {profile!r}"
        )

    shell = required_value(document, ("tool_catalog", "execution_shell"), kind)
    if shell != "bash":
        raise RuntimeContractError(
            f"{kind} tool execution_shell must be `bash`, got {shell!r}"
        )

    for mode, _label in VISIBLE_MODES:
        for surface, _surface_label in TOOL_SURFACES:
            base = ("tool_catalog", "modes", mode, surface)
            names = required_value(document, (*base, "tool_names"), kind)
            dotted_names = ".".join((*base, "tool_names"))
            if (
                not isinstance(names, list)
                or any(not isinstance(name, str) or not name for name in names)
                or names != sorted(set(names))
            ):
                raise RuntimeContractError(
                    f"{kind} field `{dotted_names}` must be sorted unique non-empty strings"
                )
            count = metric_value(document, (*base, "tools"), kind)
            if count != len(names):
                raise RuntimeContractError(
                    f"{kind} metric `{'.'.join((*base, 'tools'))}` must equal the "
                    f"owned tool_names length ({len(names)})"
                )
            digest = required_value(document, (*base, "identity_sha256"), kind)
            expected = tool_identity_digest(names)
            if digest != expected:
                raise RuntimeContractError(
                    f"{kind} field `{'.'.join((*base, 'identity_sha256'))}` must "
                    "match the owned sorted tool_names"
                )

    for stage, _label in REPRESENTATIVE_STAGES:
        path = ("representative_context", "stages", stage, "identity_sha256")
        digest = required_value(document, path, kind)
        if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise RuntimeContractError(
                f"{kind} field `{'.'.join(path)}` must be a lowercase SHA-256 digest"
            )


def validate_receipt(receipt: dict[str, Any]) -> None:
    validate_document(receipt, RECEIPT_KIND, "receipt")
    skill_discovery = receipt.get("skill_discovery")
    identical = (
        skill_discovery.get("prompts_byte_identical")
        if isinstance(skill_discovery, dict)
        else None
    )
    if identical is not True:
        raise RuntimeContractError(
            "receipt metric `skill_discovery.prompts_byte_identical` must be true"
        )
    representative = receipt.get("representative_context")
    fixture_id = (
        representative.get("fixture_id")
        if isinstance(representative, dict)
        else None
    )
    if fixture_id != REPRESENTATIVE_FIXTURE_ID:
        raise RuntimeContractError(
            "receipt metric `representative_context.fixture_id` must be "
            f"`{REPRESENTATIVE_FIXTURE_ID}`, got {fixture_id!r}"
        )
    representative_identical = representative.get("prompts_byte_identical")
    if representative_identical is not True:
        raise RuntimeContractError(
            "receipt metric `representative_context.prompts_byte_identical` must be true"
        )
    validate_identity_structure(receipt, "receipt")


def validate_budget(budget: dict[str, Any]) -> None:
    validate_document(budget, BUDGET_KIND, "budget")
    representative = budget.get("representative_context")
    fixture_id = (
        representative.get("fixture_id")
        if isinstance(representative, dict)
        else None
    )
    if fixture_id != REPRESENTATIVE_FIXTURE_ID:
        raise RuntimeContractError(
            "budget metric `representative_context.fixture_id` must be "
            f"`{REPRESENTATIVE_FIXTURE_ID}`, got {fixture_id!r}"
        )
    validate_identity_structure(budget, "budget")


def metric_value(document: dict[str, Any], path: MetricPath, kind: str) -> int:
    value = required_value(document, path, kind)
    dotted = ".".join(path)
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise RuntimeContractError(
            f"{kind} metric `{dotted}` must be a non-negative integer"
        )
    return value


def compare(
    receipt: dict[str, Any], budget: dict[str, Any]
) -> tuple[list[MetricResult], list[MetricResult]]:
    """Return (increases, decreases) as path/label/current/ceiling tuples."""
    validate_receipt(receipt)
    validate_budget(budget)
    for path, label in IDENTITIES:
        receipt_value = required_value(receipt, path, "receipt")
        budget_value = required_value(budget, path, "budget")
        if receipt_value != budget_value:
            detail = ""
            if isinstance(receipt_value, list) and isinstance(budget_value, list):
                added = [str(item) for item in receipt_value if item not in budget_value]
                removed = [
                    str(item) for item in budget_value if item not in receipt_value
                ]
                detail = f" (added={added} removed={removed})"
            raise RuntimeContractError(
                f"identity changed for {label} [`{'.'.join(path)}`]{detail}"
            )
    increases: list[MetricResult] = []
    decreases: list[MetricResult] = []
    for path, label in METRICS:
        current = metric_value(receipt, path, "receipt")
        ceiling = metric_value(budget, path, "budget")
        result = (".".join(path), label, current, ceiling)
        if current > ceiling:
            increases.append(result)
        elif current < ceiling:
            decreases.append(result)
    return increases, decreases


def set_path_value(document: dict[str, Any], path: MetricPath, value: Any) -> None:
    target = document
    for part in path[:-1]:
        target = target.setdefault(part, {})
    target[path[-1]] = value


def budget_from_receipt(receipt: dict[str, Any]) -> dict[str, Any]:
    validate_receipt(receipt)
    budget: dict[str, Any] = {
        "_comment": (
            "One-way numeric ceilings and exact structural identities for the "
            "provider-free runtime contract. Decreases pass; increases or identity "
            "changes fail. Lock in decreases with: python3 "
            "scripts/check-runtime-contract-budget.py --update"
        ),
        "document_kind": BUDGET_KIND,
        "schema_version": SCHEMA_VERSION,
        "representative_context": {
            "fixture_id": REPRESENTATIVE_FIXTURE_ID,
        },
    }
    for path, _label in METRICS:
        set_path_value(budget, path, metric_value(receipt, path, "receipt"))
    for path, _label in IDENTITIES:
        set_path_value(
            budget,
            path,
            copy.deepcopy(required_value(receipt, path, "receipt")),
        )
    return budget


def write_budget_atomic(path: Path, budget: dict[str, Any]) -> None:
    """Replace an existing budget atomically without changing its mode bits."""
    original_mode = stat.S_IMODE(path.stat().st_mode)
    payload = json.dumps(budget, indent=2, sort_keys=True) + "\n"
    file_descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_path, original_mode)
        os.replace(temporary_path, path)
    except BaseException:
        temporary_path.unlink(missing_ok=True)
        raise


def run_measurement() -> dict[str, Any]:
    env = os.environ.copy()
    env["CARGO_NET_OFFLINE"] = "true"
    proc = subprocess.run(
        [sys.executable, str(MEASURE_SCRIPT)],
        cwd=REPO_ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    sys.stderr.write(proc.stderr)
    if proc.returncode != 0:
        sys.stdout.write(proc.stdout)
        raise RuntimeContractError(
            f"runtime-contract measurement failed with exit code {proc.returncode}"
        )
    try:
        receipt = json.loads(proc.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeContractError(f"measurement emitted invalid JSON: {error}") from error
    if not isinstance(receipt, dict):
        raise RuntimeContractError("measurement top level must be an object")
    validate_receipt(receipt)
    return receipt


def update_command(
    receipt_path: Path | None, budget_path: Path, *, allow_increase: bool = False
) -> str:
    parts = ["python3", "scripts/check-runtime-contract-budget.py"]
    if receipt_path is not None:
        parts.extend(["--receipt", str(receipt_path)])
    if budget_path != BUDGET_PATH:
        parts.extend(["--budget", str(budget_path)])
    parts.append("--update")
    if allow_increase:
        parts.append("--allow-increase")
    return shlex.join(parts)


def rebased_budget(receipt: dict[str, Any], previous: dict[str, Any]) -> dict[str, Any]:
    """Budget from ``receipt`` that keeps ``previous``'s ``_`` history notes."""
    budget = budget_from_receipt(receipt)
    for key, value in previous.items():
        if key.startswith("_"):
            budget[key] = copy.deepcopy(value)
    return budget


FRAGMENT_MODULE = REPO_ROOT / "crates" / "core" / "src" / "fragments.rs"
FRAGMENT_MAX_TOKENS_CEILING = 10_000
FRAGMENT_MAX_BYTES_CEILING = FRAGMENT_MAX_TOKENS_CEILING * 4
FRAGMENT_DEFAULT_MAX_BYTES_CEILING = 4 * 1024
FRAGMENT_MAX_COUNT_CEILING = 16


def check_fragment_caps() -> None:
    """Gate the bounded fragment hard caps (issue #5264).

    Static check — no cargo needed. Fails closed if the fragment module is
    missing, if any cap has been raised without review, or if the
    project-instruction import is absent.
    """
    try:
        text = FRAGMENT_MODULE.read_text(encoding="utf-8")
    except FileNotFoundError as error:
        raise RuntimeContractError(
            f"missing bounded fragment module: {FRAGMENT_MODULE} ({error})"
        ) from error

    def const_value(pattern: str) -> int:
        match = re.search(pattern, text)
        if not match:
            raise RuntimeContractError(f"fragment cap missing: {pattern}")
        try:
            return int(match.group(1).replace("_", ""))
        except ValueError as error:
            raise RuntimeContractError(f"fragment cap not an int: {pattern}") from error

    max_tokens = const_value(r"pub const MAX_FRAGMENT_TOKENS:\s*usize\s*=\s*([0-9_]+)")
    if max_tokens != FRAGMENT_MAX_TOKENS_CEILING:
        raise RuntimeContractError(
            f"MAX_FRAGMENT_TOKENS must be {FRAGMENT_MAX_TOKENS_CEILING}, got {max_tokens}"
        )
    # MAX_FRAGMENT_BYTES must be defined as MAX_FRAGMENT_TOKENS * 4 (canonical)
    # or as a literal 40000. Either way the derived ceiling is 40_000.
    has_multiplication = re.search(
        r"pub const MAX_FRAGMENT_BYTES:\s*usize\s*=\s*MAX_FRAGMENT_TOKENS\s*\*\s*4", text
    )
    bytes_literal = re.search(
        r"pub const MAX_FRAGMENT_BYTES:\s*usize\s*=\s*([0-9_]+)", text
    )
    if bytes_literal:
        literal = int(bytes_literal.group(1).replace("_", ""))
        if literal != FRAGMENT_MAX_BYTES_CEILING:
            raise RuntimeContractError(
                f"MAX_FRAGMENT_BYTES must be {FRAGMENT_MAX_BYTES_CEILING}, got {literal}"
            )
    elif not has_multiplication:
        raise RuntimeContractError(
            "MAX_FRAGMENT_BYTES must be defined as MAX_FRAGMENT_TOKENS * 4 or as 40000"
        )
    # DEFAULT is defined as 4 * 1024 (canonical) or 4096 literal
    has_default_multiplication = re.search(
        r"pub const DEFAULT_FRAGMENT_MAX_BYTES:\s*usize\s*=\s*4\s*\*\s*1024", text
    )
    default_literal = re.search(
        r"pub const DEFAULT_FRAGMENT_MAX_BYTES:\s*usize\s*=\s*([0-9_]+)", text
    )
    if has_default_multiplication:
        # canonical 4*1024 == 4096, which equals ceiling
        pass
    elif default_literal:
        default_bytes = int(default_literal.group(1).replace("_", ""))
        if default_bytes != FRAGMENT_DEFAULT_MAX_BYTES_CEILING:
            raise RuntimeContractError(
                f"DEFAULT_FRAGMENT_MAX_BYTES must be {FRAGMENT_DEFAULT_MAX_BYTES_CEILING}, got {default_bytes}"
            )
        if default_bytes > FRAGMENT_MAX_BYTES_CEILING:
            raise RuntimeContractError(
                f"DEFAULT_FRAGMENT_MAX_BYTES ({default_bytes}) must not exceed MAX_FRAGMENT_BYTES ({FRAGMENT_MAX_BYTES_CEILING})"
            )
    else:
        raise RuntimeContractError("DEFAULT_FRAGMENT_MAX_BYTES definition not found")

    max_count = const_value(
        r"pub const MAX_FRAGMENTS_PER_CONTEXT:\s*usize\s*=\s*([0-9_]+)"
    )
    if max_count != FRAGMENT_MAX_COUNT_CEILING:
        raise RuntimeContractError(
            f"MAX_FRAGMENTS_PER_CONTEXT must be {FRAGMENT_MAX_COUNT_CEILING}, got {max_count}"
        )
    if max_count > FRAGMENT_MAX_COUNT_CEILING:
        raise RuntimeContractError(
            f"MAX_FRAGMENTS_PER_CONTEXT ({max_count}) must not exceed {FRAGMENT_MAX_COUNT_CEILING}"
        )

    # Ensure every injection type is in FragmentId::all() and the
    # project-instruction import is present as a typed fragment.
    required_fragments = [
        "Workspace",
        "Permissions",
        "Route",
        "AgentTopology",
        "SkillsTools",
        "TokenBudget",
        "ProjectInstructions",
        "Constitution",
    ]
    for name in required_fragments:
        if f"Self::{name}" not in text and f"{name} =>" not in text and f'"{name.lower()}"' not in text.lower():
            # Fallback: search for enum variant declaration
            if not re.search(rf"\b{name}\b", text):
                raise RuntimeContractError(
                    f"FragmentId missing required variant {name}"
                )
    # Marker stability — these strings are pinned by tests / prefix cache
    required_markers = [
        "<!-- cw:ctx:workspace -->",
        "<!-- cw:ctx:project_instructions -->",
        "<!-- cw:ctx:constitution -->",
    ]
    for marker in required_markers:
        if marker not in text:
            raise RuntimeContractError(
                f"bounded fragment module missing required marker {marker!r}"
            )

    # Project-instruction import must be a typed fragment, not ad-hoc
    if "load_project_instruction_fragment" not in text:
        raise RuntimeContractError(
            "bounded fragment module must expose load_project_instruction_fragment (project-instruction import as typed fragment)"
        )
    if "PROJECT_INSTRUCTION_CANDIDATES" not in text:
        raise RuntimeContractError(
            "bounded fragment module must define PROJECT_INSTRUCTION_CANDIDATES"
        )
    # Required candidate files from #3978
    required_candidates = [
        ".cursorrules",
        ".clinerules",
        ".windsurf/rules",
        ".gemini",
        ".github/copilot-instructions.md",
    ]
    for candidate in required_candidates:
        if candidate not in text:
            raise RuntimeContractError(
                f"PROJECT_INSTRUCTION_CANDIDATES missing required entry {candidate!r}"
            )

    # matches_text recognizer must exist on the fragment trait
    if "fn matches_text" not in text:
        raise RuntimeContractError(
            "bounded fragment module must define a matches_text recognizer on the fragment trait"
        )
    if "trait ContextFragment" not in text:
        raise RuntimeContractError(
            "bounded fragment module must define trait ContextFragment with matches_text"
        )

    # No unbounded fragment — enforce that creation clamps to MAX_FRAGMENT_BYTES
    if "MAX_FRAGMENT_BYTES" not in text or "enforce_byte_cap" not in text:
        raise RuntimeContractError(
            "bounded fragment module must enforce byte caps via enforce_byte_cap and MAX_FRAGMENT_BYTES"
        )

    # TUI must be unified with the core boundary (shared crates/core module)
    tui_fragment = REPO_ROOT / "crates" / "runtime" / "src" / "model_context" / "fragment.rs"
    try:
        tui_text = tui_fragment.read_text(encoding="utf-8")
    except FileNotFoundError as error:
        raise RuntimeContractError(
            f"missing TUI fragment module: {tui_fragment} ({error})"
        ) from error
    if "codewhale_core::fragments" not in tui_text:
        raise RuntimeContractError(
            "TUI model_context/fragment.rs must re-export caps from codewhale_core::fragments (shared crates/core boundary)"
        )
    if "ProjectInstructions" not in tui_text:
        raise RuntimeContractError(
            "TUI fragment module must include ProjectInstructions variant (unified with core)"
        )
    if "MAX_FRAGMENT_BYTES" not in tui_text:
        raise RuntimeContractError(
            "TUI fragment module must enforce MAX_FRAGMENT_BYTES (10K-token ceiling)"
        )
    if "matches_text" not in tui_text:
        raise RuntimeContractError(
            "TUI fragment module must expose a matches_text recognizer"
        )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--receipt",
        type=Path,
        help="check an existing measurement JSON instead of compiling",
    )
    parser.add_argument(
        "--budget",
        type=Path,
        default=BUDGET_PATH,
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help="tighten all ceilings to the current receipt; refuses increases",
    )
    parser.add_argument(
        "--allow-increase",
        action="store_true",
        help=(
            "with --update, also accept increases and identity changes: rewrite "
            "the budget from the receipt so the change lands in the same PR"
        ),
    )
    args = parser.parse_args(argv)
    if args.allow_increase and not args.update:
        parser.error("--allow-increase requires --update")
    grow_command = update_command(args.receipt, args.budget, allow_increase=True)

    try:
        check_fragment_caps()
        if args.receipt is not None and args.receipt.resolve() == args.budget.resolve():
            raise RuntimeContractError(
                "receipt and budget must resolve to distinct filesystem paths"
            )
        budget = load_json(args.budget, "budget")
        receipt = (
            load_json(args.receipt, "receipt")
            if args.receipt is not None
            else run_measurement()
        )
        if args.allow_increase:
            validate_receipt(receipt)
            validate_budget(budget)
            write_budget_atomic(args.budget, rebased_budget(receipt, budget))
            print(
                f"[runtime-contract-budget] wrote {args.budget} from the current "
                f"measurement ({len(METRICS)} metrics, {len(IDENTITIES)} identities). "
                "Record why it grew in the budget's _comment or the PR description."
            )
            return 0
        increases, decreases = compare(receipt, budget)
    except RuntimeContractError as error:
        print(f"[runtime-contract-budget] ERROR: {error}", file=sys.stderr)
        if str(error).startswith("identity changed"):
            print(
                "[runtime-contract-budget] receipt=" + json.dumps(receipt, sort_keys=True),
                file=sys.stderr,
            )
            print(
                "If the identity change is intended, land the new budget in this PR:\n"
                f"  {grow_command}",
                file=sys.stderr,
            )
        return 2
    except OSError as error:
        print(
            f"[runtime-contract-budget] ERROR: failed to update budget: {error}",
            file=sys.stderr,
        )
        return 2

    if increases:
        print("[runtime-contract-budget] FAIL: runtime contract grew:", file=sys.stderr)
        for path, label, current, ceiling in increases:
            print(
                f"  {label}: {current} > {ceiling} (+{current - ceiling}) [{path}]",
                file=sys.stderr,
            )
        print(
            "\nReduce the model-facing surface, or if the growth is intended land "
            f"the higher ceiling in this PR:\n  {grow_command}",
            file=sys.stderr,
        )
        return 1

    if args.update:
        try:
            write_budget_atomic(args.budget, rebased_budget(receipt, budget))
        except OSError as error:
            print(
                f"[runtime-contract-budget] ERROR: failed to update budget: {error}",
                file=sys.stderr,
            )
            return 2
        print(
            f"[runtime-contract-budget] wrote {args.budget}: "
            f"{len(decreases)} decreased ceilings locked in "
            f"({len(METRICS)} total)"
        )
        return 0

    if decreases:
        print(
            f"[runtime-contract-budget] PASS: {len(METRICS)} ceilings respected; "
            f"{len(decreases)} can be tightened."
        )
        for path, label, current, ceiling in decreases:
            print(f"  {label}: {current} < {ceiling} (-{ceiling - current}) [{path}]")
        print(f"Tighten with:\n  {update_command(args.receipt, args.budget)}")
        return 0

    print(
        f"[runtime-contract-budget] PASS: all {len(METRICS)} metrics are exactly at budget."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
