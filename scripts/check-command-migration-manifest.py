#!/usr/bin/env python3
"""Deterministic command migration manifest gate for EPIC-006 (FEAT-015).

Enforces the staged-migration contract:

1. The checked-in migration topology document (`scripts/command-migration-topology.json`)
   is versioned and fail-closed: `schema_version` must be 1; unknown versions,
   tags, fields, selector kinds, type/trait node tags, and const atoms are rejected.
2. The pending frontier is a valid topology frontier: sorted, unique, containing
   only known leaves, and reachable from the roots by parent-to-all-children
   replacements (documented splits) or leaf removals (migrations). Arbitrary
   additions, partial splits, and stale entries fail closed.
3. The frontier exactly equals the set of groups/slices whose handlers still
   contain concrete-`App` signatures (`&mut App` / `&mut crate::tui::app::App`)
   within `crates/tui/src/commands/groups/` (bidirectional source scan; the
   AST/selector resolution part lands in Phase 3, `scan_and_check`).

The guard is hermetic for its data parts: it reads the topology artifact and
optionally the source tree; it never starts the TUI and makes no network calls.

Usage:
    python3 scripts/check-command-migration-manifest.py           # enforce
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TOPOLOGY_PATH = REPO_ROOT / "scripts" / "command-migration-topology.json"
CONTRACT_PATH = REPO_ROOT / "crates" / "tui" / "src" / "commands" / "contract.rs"
TOPOLOGY_REPO_PATH = "scripts/command-migration-topology.json"
SUPPORTED_SCHEMA_VERSION = 1

# Closed set of Rust integer suffixes accepted by selector const atoms.
INTEGER_SUFFIXES = {
    "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "i128", "u128",
    "isize", "usize",
}

# Closed set of primitive type names accepted by the type algebra (v1).
PRIMITIVE_TYPES = {
    "u8", "u16", "u32", "u64", "u128", "usize",
    "i8", "i16", "i32", "i64", "i128", "isize",
    "f32", "f64", "bool", "char", "str",
}

VALID_SELECTOR_KINDS = {"free", "inherent", "trait_impl"}
VALID_TYPE_TAGS = {
    "path", "qualified", "tuple", "reference", "pointer", "slice", "array",
    "primitive", "never",
}
VALID_GENERIC_ARG_TAGS = {"lifetime", "type", "const"}
VALID_CONST_TAGS = {"bool", "int", "char", "path"}


class ManifestViolation:
    """One deterministic manifest violation with an actionable diagnostic."""

    def __init__(self, category: str, location: str, detail: str) -> None:
        self.category = category
        self.location = location
        self.detail = detail

    def __str__(self) -> str:
        return f"{self.category}: {self.location}: {self.detail}"


# ---------------------------------------------------------------------------
# Const atom validation (Deep-Dive: closed four-tag records, canonical decimal
# magnitudes, byte bounds, fail-closed unknown forms).
# ---------------------------------------------------------------------------

def _canonical_magnitude(value: str, location: str) -> list[ManifestViolation]:
    """Validate `0|[1-9][0-9]*` and return violations (never leading zeros)."""
    if value == "0":
        return []
    if not value.isdigit() or value[0] == "0":
        return [ManifestViolation(
            "const-atom", location,
            f"integer magnitude must be canonical unsigned decimal without leading zeros "
            f"(0|[1-9][0-9]*), got {value!r}",
        )]
    return []


def validate_const_atom(atom, location: str) -> list[ManifestViolation]:
    """Validate one const atom record; reject unknown tags/fields/forms."""
    if not isinstance(atom, dict) or "tag" not in atom:
        return [ManifestViolation("const-atom", location, "const atom must be an object with a tag")]
    tag = atom["tag"]
    if tag not in VALID_CONST_TAGS:
        return [ManifestViolation(
            "const-atom", location, f"unknown const atom tag {tag!r}; expected one of {sorted(VALID_CONST_TAGS)}",
        )]
    violations: list[ManifestViolation] = []
    if tag == "bool":
        if set(atom) != {"tag", "value"} or not isinstance(atom.get("value"), bool):
            violations.append(ManifestViolation(
                "const-atom", location, "bool const atom must be {{tag: bool, value: <bool>}}",
            ))
    elif tag == "int":
        allowed = {"tag", "negative", "magnitude", "suffix"}
        if set(atom) != allowed:
            violations.append(ManifestViolation(
                "const-atom", location,
                f"int const atom must have exactly {{tag, negative, magnitude, suffix}}, got {sorted(atom)}",
            ))
            return violations
        if not isinstance(atom.get("negative"), bool):
            violations.append(ManifestViolation("const-atom", location, "int.negative must be a bool"))
        magnitude = atom.get("magnitude")
        if not isinstance(magnitude, str):
            violations.append(ManifestViolation("const-atom", location, "int.magnitude must be a string"))
        else:
            violations.extend(_canonical_magnitude(magnitude, location))
        suffix = atom.get("suffix")
        if suffix is not None and suffix not in INTEGER_SUFFIXES:
            violations.append(ManifestViolation(
                "const-atom", location, f"int.suffix must be null or a Rust integer suffix, got {suffix!r}",
            ))
        # Negative zero is noncanonical: normalize to nonnegative.
        if atom.get("negative") and magnitude == "0":
            violations.append(ManifestViolation(
                "const-atom", location, "negative zero must be normalized to nonnegative",
            ))
        # Decoded byte values (u8 suffix) must be within 0-255 inclusive.
        if suffix == "u8" and magnitude is not None and magnitude.isdigit():
            if int(magnitude) > 255:
                violations.append(ManifestViolation(
                    "const-atom", location,
                    f"decoded byte value {magnitude} exceeds the inclusive u8 range 0-255",
                ))
    elif tag == "char":
        if set(atom) != {"tag", "scalar"} or not isinstance(atom.get("scalar"), str):
            violations.append(ManifestViolation(
                "const-atom", location, "char const atom must be {{tag: char, scalar: <single Unicode scalar>}}",
            ))
        else:
            scalar = atom["scalar"]
            if len(scalar) != 1:
                violations.append(ManifestViolation(
                    "const-atom", location, "char.scalar must be exactly one Unicode scalar",
                ))
    elif tag == "path":
        if set(atom) != {"tag", "absolute", "segments"}:
            violations.append(ManifestViolation(
                "const-atom", location,
                "path const atom must be {{tag: path, absolute: <bool>, segments: [...]}}",
            ))
            return violations
        if not isinstance(atom.get("absolute"), bool):
            violations.append(ManifestViolation("const-atom", location, "path.absolute must be a bool"))
        segments = atom.get("segments")
        if not isinstance(segments, list) or not segments:
            violations.append(ManifestViolation("const-atom", location, "path.segments must be a nonempty array"))
        elif not all(isinstance(s, str) and s for s in segments):
            violations.append(ManifestViolation("const-atom", location, "path.segments must be nonempty strings"))
    return violations


# ---------------------------------------------------------------------------
# Type algebra validation (Deep-Dive: closed v1 algebra, fail-closed).
# ---------------------------------------------------------------------------

def validate_type_node(node, location: str) -> list[ManifestViolation]:
    """Validate a recursive type/trait node; reject unsupported syntax."""
    if not isinstance(node, dict) or "tag" not in node:
        return [ManifestViolation("type-algebra", location, "type node must be an object with a tag")]
    tag = node["tag"]
    if tag not in VALID_TYPE_TAGS:
        return [ManifestViolation(
            "type-algebra", location,
            f"unknown type node tag {tag!r}; expected one of {sorted(VALID_TYPE_TAGS)}",
        )]
    violations: list[ManifestViolation] = []

    if tag == "path":
        allowed = {"tag", "absolute", "segments"}
        if set(node) != allowed:
            return [ManifestViolation(
                "type-algebra", location, f"path node must have exactly {{tag, absolute, segments}}, got {sorted(node)}",
            )]
        if not isinstance(node.get("absolute"), bool):
            violations.append(ManifestViolation("type-algebra", location, "path.absolute must be a bool"))
        segments = node.get("segments")
        if not isinstance(segments, list) or not segments:
            return violations + [ManifestViolation("type-algebra", location, "path.segments must be a nonempty array")]
        for i, seg in enumerate(segments):
            seg_loc = f"{location}.segments[{i}]"
            if not isinstance(seg, dict) or "name" not in seg or not isinstance(seg.get("name"), str) or not seg["name"]:
                violations.append(ManifestViolation("type-algebra", seg_loc, "segment must be {{name: <string>, args?: [...]}}"))
                continue
            if "args" in seg:
                if not isinstance(seg["args"], list):
                    violations.append(ManifestViolation("type-algebra", seg_loc, "segment.args must be an array"))
                    continue
                for j, arg in enumerate(seg["args"]):
                    violations.extend(validate_generic_arg(arg, f"{seg_loc}.args[{j}]"))
    elif tag == "qualified":
        allowed = {"tag", "self", "assoc"}
        if set(node) != allowed:
            return [ManifestViolation("type-algebra", location, "qualified node must have exactly {tag, self, assoc}")]
        violations.extend(validate_type_node(node.get("self"), f"{location}.self"))
        if not isinstance(node.get("assoc"), str) or not node["assoc"]:
            violations.append(ManifestViolation("type-algebra", location, "qualified.assoc must be a nonempty string"))
    elif tag == "tuple":
        if set(node) != {"tag", "elems"} or not isinstance(node.get("elems"), list):
            return [ManifestViolation("type-algebra", location, "tuple node must be {{tag: tuple, elems: [...]}}")]
        for i, elem in enumerate(node["elems"]):
            violations.extend(validate_type_node(elem, f"{location}.elems[{i}]"))
    elif tag in ("reference", "pointer"):
        allowed = {"tag", "mut", "inner"}
        if set(node) != allowed or not isinstance(node.get("mut"), bool):
            return [ManifestViolation("type-algebra", location, f"{tag} node must be {{tag, mut, inner}}")]
        violations.extend(validate_type_node(node.get("inner"), f"{location}.inner"))
    elif tag == "slice":
        if set(node) != {"tag", "inner"}:
            return [ManifestViolation("type-algebra", location, "slice node must be {{tag: slice, inner}}")]
        violations.extend(validate_type_node(node.get("inner"), f"{location}.inner"))
    elif tag == "array":
        allowed = {"tag", "inner", "len"}
        if set(node) != allowed:
            return [ManifestViolation("type-algebra", location, "array node must be {{tag: array, inner, len}}")]
        violations.extend(validate_type_node(node.get("inner"), f"{location}.inner"))
        violations.extend(validate_const_atom(node.get("len"), f"{location}.len"))
    elif tag == "primitive":
        if set(node) != {"tag", "name"} or node.get("name") not in PRIMITIVE_TYPES:
            return [ManifestViolation(
                "type-algebra", location,
                f"primitive node must be {{tag: primitive, name}} with name in {sorted(PRIMITIVE_TYPES)}",
            )]
    elif tag == "never":
        if set(node) != {"tag"}:
            return [ManifestViolation("type-algebra", location, "never node must be {{tag: never}}")]
    return violations


def validate_generic_arg(arg, location: str) -> list[ManifestViolation]:
    """Validate one path-segment generic argument (type/lifetime/const)."""
    if not isinstance(arg, dict) or "tag" not in arg:
        return [ManifestViolation("type-algebra", location, "generic argument must be an object with a tag")]
    tag = arg["tag"]
    if tag not in VALID_GENERIC_ARG_TAGS:
        return [ManifestViolation(
            "type-algebra", location,
            f"unknown generic argument tag {tag!r}; expected one of {sorted(VALID_GENERIC_ARG_TAGS)}",
        )]
    if tag == "lifetime":
        if set(arg) != {"tag", "name"} or not isinstance(arg.get("name"), str) or not arg["name"]:
            return [ManifestViolation("type-algebra", location, "lifetime argument must be {{tag: lifetime, name}}")]
    elif tag == "type":
        return validate_type_node(arg.get("node"), f"{location}.node")
    else:  # const
        if set(arg) != {"tag", "atom"}:
            return [ManifestViolation("type-algebra", location, "const argument must be {{tag: const, atom}}")]
        return validate_const_atom(arg.get("atom"), f"{location}.atom")
    return []


# ---------------------------------------------------------------------------
# Selector validation (Deep-Dive: tagged structural records).
# ---------------------------------------------------------------------------

def validate_selector(selector, location: str) -> list[ManifestViolation]:
    """Validate one handler selector record (free / inherent / trait_impl)."""
    if not isinstance(selector, dict) or "kind" not in selector:
        return [ManifestViolation("selector", location, "selector must be an object with a kind")]
    kind = selector["kind"]
    if kind not in VALID_SELECTOR_KINDS:
        return [ManifestViolation(
            "selector", location,
            f"unknown selector kind {kind!r}; expected one of {sorted(VALID_SELECTOR_KINDS)}",
        )]
    violations: list[ManifestViolation] = []
    if kind == "free":
        allowed = {"kind", "item"}
        if set(selector) != allowed:
            return [ManifestViolation("selector", location, f"free selector must have exactly {{kind, item}}, got {sorted(selector)}")]
        item = selector.get("item")
        if not isinstance(item, list) or len(item) < 2:
            return [ManifestViolation("selector", location, "free.item must be a module path array ending with the function name")]
        if not all(isinstance(s, str) and s for s in item):
            return [ManifestViolation("selector", location, "free.item entries must be nonempty strings")]
    elif kind == "inherent":
        allowed = {"kind", "self_type", "method"}
        if "method" not in selector:
            return [ManifestViolation("selector", location, "inherent.method must be a nonempty string")]
        if set(selector) != allowed:
            return [ManifestViolation("selector", location, f"inherent selector must have exactly {{kind, self_type, method}}, got {sorted(selector)}")]
        violations.extend(validate_type_node(selector.get("self_type"), f"{location}.self_type"))
        if not isinstance(selector.get("method"), str) or not selector["method"]:
            violations.append(ManifestViolation("selector", location, "inherent.method must be a nonempty string"))
    else:  # trait_impl
        allowed = {"kind", "self_type", "trait_path", "method"}
        if set(selector) != allowed:
            return [ManifestViolation("selector", location, f"trait_impl selector must have exactly {{kind, self_type, trait_path, method}}, got {sorted(selector)}")]
        violations.extend(validate_type_node(selector.get("self_type"), f"{location}.self_type"))
        violations.extend(validate_type_node(selector.get("trait_path"), f"{location}.trait_path"))
        if not isinstance(selector.get("method"), str) or not selector["method"]:
            violations.append(ManifestViolation("selector", location, "trait_impl.method must be a nonempty string"))
    return violations


# ---------------------------------------------------------------------------
# Topology / frontier validation.
# ---------------------------------------------------------------------------

def all_leaves(topology: dict) -> dict[str, str]:
    """Map leaf name -> owning root for every group and predeclared slice."""
    leaves: dict[str, str] = {}
    for root, node in topology.items():
        leaves[root] = root
        for slice_node in node.get("slices", []):
            leaves[slice_node["name"]] = root
    return leaves


def validate_frontier(topology: dict, frontier: list[str]) -> list[ManifestViolation]:
    """Frontier must be sorted, unique, and reference only known leaves."""
    violations: list[ManifestViolation] = []
    if not isinstance(frontier, list):
        return [ManifestViolation("frontier", "frontier", "frontier must be an array")]
    if frontier != sorted(frontier):
        violations.append(ManifestViolation("frontier", "frontier", "frontier must be sorted"))
    if len(frontier) != len(set(frontier)):
        violations.append(ManifestViolation("frontier", "frontier", "frontier must contain no duplicates"))
    leaves = all_leaves(topology)
    for entry in frontier:
        if entry not in leaves:
            violations.append(ManifestViolation(
                "frontier", entry, f"frontier entry {entry!r} is not a declared topology leaf",
            ))
    return violations


def is_documented_split(topology: dict, old: set[str], new: set[str]) -> bool:
    """A parent was replaced by ALL of its declared children (1->N, no growth).

    Exactly one entry is removed; it is a group with declared slices; the added
    entries are exactly those declared children; nothing else changed.
    """
    removed = old - new
    added = new - old
    if len(removed) != 1:
        return False
    parent = next(iter(removed))
    for root, node in topology.items():
        children = {s["name"] for s in node.get("slices", [])}
        if parent == root and children:
            return children == added and len(added) == len(children) and added == new - old
    return False


def is_valid_frontier_transition(topology: dict, old: list[str], new: list[str]) -> list[ManifestViolation]:
    """Permit shrink (removal of migrated leaves) or documented parent-to-all-children split."""
    old_set = set(old)
    new_set = set(new)
    violations: list[ManifestViolation] = []
    removed = old_set - new_set
    added = new_set - old_set

    # Pure shrink: removed leaves only, nothing added.
    if not added:
        return violations  # any removal is a shrink (gate re-checks source later)

    # Documented split: exactly one parent removed, all its children added.
    if is_documented_split(topology, old_set, new_set):
        return violations

    # Everything else is growth or partial split: fail closed.
    if removed:
        violations.append(ManifestViolation(
            "frontier-transition", ", ".join(sorted(removed)),
            "frontier shrank AND grew; only pure shrink or documented parent-to-all-children split is allowed",
        ))
    else:
        violations.append(ManifestViolation(
            "frontier-transition", ", ".join(sorted(added)),
            "frontier grew without a documented parent-to-all-children split; arbitrary growth is forbidden",
        ))
    return violations


def validate_topology_document(doc: dict) -> list[ManifestViolation]:
    """Validate the whole topology document (schema, topology, frontier, slices)."""
    violations: list[ManifestViolation] = []
    if not isinstance(doc, dict):
        return [ManifestViolation("schema", "document", "topology document must be an object")]
    if doc.get("schema_version") != SUPPORTED_SCHEMA_VERSION:
        return [ManifestViolation(
            "schema", "schema_version",
            f"unsupported schema_version {doc.get('schema_version')!r}; only {SUPPORTED_SCHEMA_VERSION} is supported",
        )]
    topology = doc.get("topology")
    if not isinstance(topology, dict) or not topology:
        return [ManifestViolation("schema", "topology", "topology must be a nonempty object")]
    for root, node in topology.items():
        if not isinstance(node, dict):
            violations.append(ManifestViolation("schema", root, "group node must be an object"))
            continue
        allowed_group_fields = {"kind", "scope", "slices"}
        unknown = set(node) - allowed_group_fields
        if unknown:
            violations.append(ManifestViolation(
                "schema", root, f"unknown group field(s) {sorted(unknown)}; expected {sorted(allowed_group_fields)}",
            ))
        if "kind" not in node or "scope" not in node or "slices" not in node:
            violations.append(ManifestViolation(
                "schema", root, "group node must be {{kind, scope, slices}}",
            ))
            continue
        if node.get("kind") != "group":
            violations.append(ManifestViolation("schema", root, f"group {root!r} kind must be 'group'"))
        scope = node.get("scope")
        if not isinstance(scope, list) or not scope or not all(isinstance(s, str) and s for s in scope):
            violations.append(ManifestViolation("schema", root, "group scope must be a nonempty array of strings"))
        slices = node.get("slices")
        if not isinstance(slices, list):
            violations.append(ManifestViolation("schema", root, "group slices must be an array"))
            continue
        seen: set[str] = set()
        for i, slice_node in enumerate(slices):
            loc = f"{root}.slices[{i}]"
            if not isinstance(slice_node, dict):
                violations.append(ManifestViolation("schema", loc, "slice must be an object"))
                continue
            allowed_slice_fields = {"name", "kind", "scope", "handlers"}
            unknown = set(slice_node) - allowed_slice_fields
            if unknown:
                violations.append(ManifestViolation(
                    "schema", loc, f"unknown slice field(s) {sorted(unknown)}; expected {sorted(allowed_slice_fields)}",
                ))
            if "name" not in slice_node:
                violations.append(ManifestViolation("schema", loc, "slice must be an object with a name"))
                continue
            name = slice_node["name"]
            if name in seen:
                violations.append(ManifestViolation("schema", loc, f"duplicate slice name {name!r}"))
            seen.add(name)
            if not name.startswith(f"{root}::"):
                violations.append(ManifestViolation("schema", loc, f"slice name {name!r} must start with {root!r}::"))
            if slice_node.get("kind") != "slice":
                violations.append(ManifestViolation("schema", loc, f"slice {name!r} kind must be 'slice'"))
            slice_scope = slice_node.get("scope")
            if not isinstance(slice_scope, list) or not slice_scope or not all(isinstance(s, str) and s for s in slice_scope):
                violations.append(ManifestViolation("schema", loc, f"slice {name!r} scope must be a nonempty array of strings"))
            for selector in slice_node.get("handlers", []):
                violations.extend(validate_selector(selector, f"{loc}.handlers"))
    violations.extend(validate_frontier(topology, doc.get("frontier", [])))
    return violations


def load_topology(path: Path = TOPOLOGY_PATH) -> dict:
    with path.open(encoding="utf-8") as fh:
        return json.load(fh)


def load_pending_groups(path: Path = CONTRACT_PATH) -> list[str]:
    """Read the TUI frontier projection from `PENDING_GROUPS` fail-closed."""
    source = path.read_text(encoding="utf-8")
    match = re.search(
        r"pub\(crate\)\s+const\s+PENDING_GROUPS\s*:\s*&\[&str\]\s*=\s*&\[(.*?)\];",
        source,
        re.DOTALL,
    )
    if match is None:
        raise ValueError(f"could not locate PENDING_GROUPS in {path}")
    body = match.group(1)
    stripped = re.sub(r'"(?:\\.|[^"\\])*"', "", body)
    if re.fullmatch(r"[\s,]*", stripped) is None:
        raise ValueError("PENDING_GROUPS may contain string literals only")
    return re.findall(r'"((?:\\.|[^"\\])*)"', body)


def validate_pending_projection(doc: dict, pending: list[str]) -> list[ManifestViolation]:
    if pending != doc.get("frontier"):
        return [ManifestViolation(
            "frontier-projection",
            "PENDING_GROUPS",
            f"TUI projection {pending!r} does not equal JSON frontier {doc.get('frontier')!r}",
        )]
    return []


def load_topology_at_ref(ref: str, root: Path = REPO_ROOT) -> dict | None:
    """Load the topology from a Git revision; return None before its introduction."""
    commit = subprocess.run(
        ["git", "rev-parse", "--verify", f"{ref}^{{commit}}"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if commit.returncode != 0:
        raise ValueError(f"baseline ref {ref!r} is unavailable: {commit.stderr.strip()}")
    shown = subprocess.run(
        ["git", "show", f"{ref}:{TOPOLOGY_REPO_PATH}"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if shown.returncode != 0:
        return None
    return json.loads(shown.stdout)


def validate_baseline_transition(current: dict, previous: dict | None) -> list[ManifestViolation]:
    """Enforce immutable topology and shrink-or-declared-split across revisions."""
    if validate_topology_document(current):
        return [ManifestViolation(
            "current",
            "topology",
            "current topology is invalid; cannot validate a monotonic transition",
        )]
    if previous is None:
        roots = sorted(current["topology"])
        if current.get("frontier") != roots:
            return [ManifestViolation(
                "frontier-initialization",
                "frontier",
                f"first manifest revision must start at all topology roots {roots!r}",
            )]
        return []

    violations = validate_topology_document(previous)
    if violations:
        return [ManifestViolation(
            "baseline",
            "topology",
            "baseline topology is invalid; cannot validate a monotonic transition",
        )]
    if previous.get("topology") != current.get("topology"):
        violations.append(ManifestViolation(
            "topology-transition",
            "topology",
            "migration topology is immutable; only the frontier may change",
        ))
        return violations
    violations.extend(
        is_valid_frontier_transition(
            previous["topology"], previous["frontier"], current["frontier"]
        )
    )
    return violations


def detect_local_baseline_ref(root: Path = REPO_ROOT) -> str | None:
    """Use the feature-branch merge-base, or the first parent on main."""
    process = subprocess.run(
        ["git", "merge-base", "HEAD", "origin/main"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    baseline = process.stdout.strip() if process.returncode == 0 else None
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if baseline and baseline != head:
        return baseline
    # A main checkout aligned with origin/main still has migration history.
    # Comparing its parent preserves the ratchet after push and in manual CI.
    parent = subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD^"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    return parent.stdout.strip() if parent.returncode == 0 else None


# ---------------------------------------------------------------------------
# Source scan (Task 3.5): structural Rust item resolution and bidirectional
# frontier check. Pure-stdlib parser: brace/paren aware, resolves qualified
# item paths for free functions, inherent methods, and trait-impl methods.
# ---------------------------------------------------------------------------

GROUPS_ROOT = REPO_ROOT / "crates" / "tui" / "src" / "commands" / "groups"
CONCRETE_APP_PARAM = re.compile(
    r"&\s*(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?(?:crate::tui::app::|crate::app::|tui::app::)?App\b"
)
CFG_TEST_ATTR = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
MOD_TESTS_HEAD = re.compile(r"\bmod\s+tests\s*\{")
ATTRIBUTE_HEAD = re.compile(r"#\s*!?\s*\[")
CONST_FN_TAIL = r"(?:(?:async|unsafe)\s+|extern\s*(?:\"[^\"]*\"\s*)?)*fn\b"
SEMICOLON_ITEM = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?(?:use|static|type|const(?!\s+" + CONST_FN_TAIL + r"))\b"
)
ITEM_HEAD = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?"
    r"(?:fn|async|unsafe|extern|mod|use|impl|struct|enum|union|trait|const|static|type|macro_rules)\b"
)


class RustItem:
    """One parsed Rust item relevant to the migration scan."""

    def __init__(self, kind: str, name: str, qual_path: str, file: Path,
                 line: int, param_types: list[str], is_concrete_app: bool) -> None:
        self.kind = kind  # 'free' | 'inherent' | 'trait_impl'
        self.name = name
        self.qual_path = qual_path
        self.file = file
        self.line = line
        self.param_types = param_types
        self.is_concrete_app = is_concrete_app

    def __repr__(self) -> str:
        return f"RustItem({self.kind}, {self.qual_path}, app={self.is_concrete_app})"


class SourceScanViolation:
    """One deterministic source-scan failure with an actionable diagnostic."""

    def __init__(self, category: str, location: str, detail: str) -> None:
        self.category = category
        self.location = location
        self.detail = detail

    def __str__(self) -> str:
        return f"{self.category}: {self.location}: {self.detail}"


def _strip_comments_and_strings(text: str) -> str:
    """Replace comments and string/char literals with spaces so the structural
    scanner sees only code. Keeps raw strings and byte/char literals intact
    enough for delimiter counting (content is blanked)."""
    out = list(text)
    i = 0
    n = len(text)
    in_line_comment = False
    in_block_comment = 0
    while i < n:
        ch = text[i]
        if in_line_comment:
            if ch == "\n":
                in_line_comment = False
            else:
                out[i] = " "
            i += 1
            continue
        if in_block_comment:
            if text.startswith("*/", i):
                in_block_comment -= 1
                out[i] = out[i + 1] = " "
                i += 2
            else:
                out[i] = " "
                i += 1
            continue
        if text.startswith("//", i):
            in_line_comment = True
            out[i] = out[i + 1] = " "
            i += 2
            continue
        if text.startswith("/*", i):
            in_block_comment += 1
            out[i] = out[i + 1] = " "
            i += 2
            continue
        if ch == '"':
            # String literal (possibly raw r#"..."#). Blank until closing quote.
            out[i] = " "
            i += 1
            if text.startswith('#"', i - 1):
                while i < n and text[i] == "#":
                    out[i] = " "
                    i += 1
            while i < n:
                if text[i] == "\\" and i + 1 < n:
                    out[i] = out[i + 1] = " "
                    i += 2
                    continue
                if text[i] == '"':
                    out[i] = " "
                    i += 1
                    break
                out[i] = " "
                i += 1
            continue
        if ch == "'":
            # Char or lifetime. Blank the atom conservatively (lifetimes are
            # single-quote-prefixed identifiers; chars are 'x' or '\\x').
            if i + 1 < n and text[i + 1] == "'":
                out[i] = out[i + 1] = " "
                i += 2
                continue
            out[i] = " "
            i += 1
            if i < n and text[i] == "\\":
                out[i] = " "
                i += 1
            if i < n:
                out[i] = " "
                i += 1
            if i < n and text[i] == "'":
                out[i] = " "
                i += 1
            continue
        i += 1
    return "".join(out)


def _split_top_level(text: str, sep: str) -> list[str]:
    """Split on a separator character outside (), [], {} and strings."""
    parts: list[str] = []
    start = 0
    depth = 0
    for i, ch in enumerate(text):
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == sep and depth == 0:
            parts.append(text[start:i])
            start = i + 1
    parts.append(text[start:])
    return parts


def _module_path_from_file(file: Path, root: Path, prefix: str = "crate::commands::groups::") -> str:
    """Derive the crate-relative module path for a file under the groups root.

    `mod.rs` resolves to its directory name; other files to
    `dirname.file_name` (snake_case). Path segments are joined with `::`.
    """
    rel = file.relative_to(root)
    parts = list(rel.parts)
    if parts[-1] == "mod.rs":
        parts = parts[:-1]
    else:
        parts[-1] = parts[-1].removesuffix(".rs")
    return prefix + "::".join(parts)


def _first_param_type(fn_sig: str) -> str | None:
    """Extract the first parameter's type from a `fn name(...)` signature text."""
    open_idx = fn_sig.find("(")
    if open_idx < 0:
        return None
    close_idx = fn_sig.rfind(")")
    if close_idx < open_idx:
        return None
    params = _split_top_level(fn_sig[open_idx + 1:close_idx], ",")
    params = [p.strip() for p in params if p.strip()]
    if not params:
        return None
    first = params[0]
    if first == "self" or first.startswith("self:") or first.startswith("&self"):
        return None
    # `name: Type` or `name: Type` with generic default: take after the first top-level ':'.
    depth = 0
    for idx, ch in enumerate(first):
        if ch in "<([":
            depth += 1
        elif ch in ">)]":
            depth -= 1
        elif ch == ":" and depth == 0:
            return first[idx + 1:].strip()
    return None


# ---------------------------------------------------------------------------
# Retained host machinery (FEAT-042 tracking)
# ---------------------------------------------------------------------------
#
# The migration topology is immutable, so the dispatcher-only host helpers that
# intentionally keep `&mut App` after a group migrates are declared here — the
# gate's own enforcement home. Each entry maps a migrated group to selectors
# that must keep their concrete-App signature until FEAT-042 extracts them to a
# host-side module; a missing or refactored-away signature fails the gate, so
# the tracking cannot silently go stale. FEAT-022: the skills group retains the
# unified slash-command fallback and its activation helpers co-located with the
# portable handlers (D7).
RETAINED_HOST_MACHINERY: dict[str, list[dict]] = {
    "skills": [
        {"kind": "free", "item": ["crate", "commands", "groups", "skills", "skills", "discover_visible_skills"]},
        {"kind": "free", "item": ["crate", "commands", "groups", "skills", "skills", "run_skill_by_name"]},
        {"kind": "free", "item": ["crate", "commands", "groups", "skills", "skills", "activate_skill_with_task"]},
        {"kind": "free", "item": ["crate", "commands", "groups", "skills", "skills", "activate_skill"]},
    ],
}


def _is_concrete_app_type(param_type: str | None) -> bool:
    if param_type is None:
        return False
    # Match `&mut App` / `&mut crate::tui::app::App` with optional spaces
    # around `mut`; no whitespace normalization so `\s*` can bind.
    return bool(CONCRETE_APP_PARAM.search(param_type))


def _skip_balanced(code: str, start: int, opener: str, closer: str) -> int:
    depth = 0
    i = start
    n = len(code)
    while i < n:
        if code[i] == opener:
            depth += 1
        elif code[i] == closer:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _item_end(code: str, start: int) -> int:
    """Index just past the Rust item starting at `start` (attributes already
    skipped): the matching `}` of its body, or its terminating `;`."""
    n = len(code)
    semicolon_only = bool(SEMICOLON_ITEM.match(code, start))
    depth = 0
    i = start
    while i < n:
        ch = code[i]
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif ch == "{" and depth == 0:
            if semicolon_only:
                i = _skip_balanced(code, i, "{", "}")
                continue
            return _skip_balanced(code, i, "{", "}")
        elif ch == ";" and depth == 0:
            return i + 1
        i += 1
    return n


def _blank_span(chars: list[str], start: int, end: int) -> None:
    for k in range(start, min(end, len(chars))):
        if chars[k] != "\n":
            chars[k] = " "


def _blank_test_only_items(code: str) -> str:
    """Blank `#[cfg(test)]` items and `mod tests { .. }` bodies so test-only
    helpers are never counted as handlers. Offsets and newlines are kept.
    On a field, variant, parameter or match arm only the attribute is blanked,
    so the scan never runs past the enclosing item."""
    chars = list(code)
    for match in CFG_TEST_ATTR.finditer(code):
        if chars[match.start()] == " ":
            continue
        j = match.end()
        while True:
            while j < len(code) and code[j].isspace():
                j += 1
            attr = ATTRIBUTE_HEAD.match(code, j)
            if not attr:
                break
            j = _skip_balanced(code, code.index("[", j), "[", "]")
        end = _item_end(code, j) if ITEM_HEAD.match(code, j) else j
        _blank_span(chars, match.start(), end)
    for match in MOD_TESTS_HEAD.finditer(code):
        if chars[match.start()] == " ":
            continue
        brace = code.index("{", match.start())
        _blank_span(chars, match.start(), _skip_balanced(code, brace, "{", "}"))
    return "".join(chars)


def parse_rust_file(file: Path, root: Path) -> list[RustItem]:
    """Structurally parse one Rust file for handler-relevant items.

    Handles `fn name(...) -> R { ... }` free functions at top level and
    `impl Trait for Type { fn ... }` / `impl Type { fn ... }` blocks. The
    parser tracks braces to skip bodies and resolves qualified paths from the
    module layout. It is deliberately narrow: it looks for function items with
    a first parameter typed `&mut App` (the concrete-App handler signature).

    Module paths use the owning commands crate or TUI groups directory.
    Hermetic fixtures may supply their own module root.
    """
    raw = file.read_text(encoding="utf-8")
    code = _blank_test_only_items(_strip_comments_and_strings(raw))
    # Module paths derive from the source owner; hermetic tests use a synthetic
    # root, in which case the passed root is the module-path base.
    commands_root = root / "crates/commands/src"
    try:
        module_path = _module_path_from_file(file, commands_root, "crate::")
    except ValueError:
        try:
            module_path = _module_path_from_file(file, GROUPS_ROOT)
        except ValueError:
            module_path = _module_path_from_file(file, root)
    items: list[RustItem] = []
    lines = raw.splitlines()

    i = 0
    n = len(code)
    # Walk top-level items: skip until we find `fn` or `impl` at depth 0.
    while i < n:
        # find next top-level keyword occurrence
        while i < n and code[i].isspace():
            i += 1
        if i >= n:
            break
        if code.startswith("fn", i) and (i == 0 or not (code[i - 1].isalnum() or code[i - 1] == "_")):
            # free function
            sig_end = _find_fn_signature_end(code, i + 2)
            if sig_end is None:
                i += 2
                continue
            sig_text = code[i:sig_end]
            name_match = re.match(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)", sig_text)
            line_no = raw.count("\n", 0, code.find(sig_text[:20], 0)) + 1 if sig_text[:20] else 0
            if name_match:
                name = name_match.group(1)
                param_type = _first_param_type(sig_text)
                items.append(RustItem(
                    kind="free",
                    name=name,
                    qual_path=f"{module_path}::{name}",
                    file=file,
                    line=line_no,
                    param_types=[param_type] if param_type else [],
                    is_concrete_app=_is_concrete_app_type(param_type),
                ))
            # advance past the signature and body
            i = sig_end
            # skip the block body if present (or the `;` for a declaration)
            while i < n and code[i].isspace():
                i += 1
            if i < n and code[i] == "{":
                depth = 0
                while i < n:
                    if code[i] == "{":
                        depth += 1
                    elif code[i] == "}":
                        depth -= 1
                        if depth == 0:
                            i += 1
                            break
                    i += 1
            else:
                i += 1  # consume the `;` (or advance past a non-body token)
            continue
        if code.startswith("impl", i) and (i == 0 or not (code[i - 1].isalnum() or code[i - 1] == "_")):
            # impl block: header until '{'
            brace_idx = code.find("{", i)
            if brace_idx < 0:
                i += 4
                continue
            header = code[i + 4:brace_idx].strip()
            # `impl Trait for Type` vs `impl Type`
            trait_impl = " for " in header
            self_type = header.split(" for ")[-1].strip() if trait_impl else header.strip()
            # methods inside
            depth = 1
            j = brace_idx + 1
            while j < n and depth > 0:
                if code[j] == "{":
                    depth += 1
                elif code[j] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                elif code[j].isspace():
                    j += 1
                    continue
                elif code.startswith("fn", j) and not (code[j - 1].isalnum() or code[j - 1] == "_"):
                    fn_start = j
                    sig_end = _find_fn_signature_end(code, j + 2)
                    if sig_end is None:
                        j += 2
                        continue
                    sig_text = code[fn_start:sig_end]
                    name_match = re.match(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)", sig_text)
                    if name_match:
                        name = name_match.group(1)
                        param_type = _first_param_type(sig_text)
                        self_qual = _self_type_qual(self_type, module_path)
                        if trait_impl:
                            trait_path = header.split(" for ")[0].strip()
                            kind = "trait_impl"
                            qual = f"{self_qual}::{name} [{trait_path}]"
                        else:
                            kind = "inherent"
                            qual = f"{self_qual}::{name}"
                        line_no = raw.count("\n", 0, code.find(sig_text[:20], 0)) + 1 if sig_text[:20] else 0
                        items.append(RustItem(
                            kind=kind,
                            name=name,
                            qual_path=qual,
                            file=file,
                            line=line_no,
                            param_types=[param_type] if param_type else [],
                            is_concrete_app=_is_concrete_app_type(param_type),
                        ))
                    j = sig_end
                    # skip the method body (or consume `;` for a declaration)
                    while j < n and code[j].isspace():
                        j += 1
                    if j < n and code[j] == "{":
                        body_depth = 0
                        while j < n:
                            if code[j] == "{":
                                body_depth += 1
                            elif code[j] == "}":
                                body_depth -= 1
                                if body_depth == 0:
                                    j += 1
                                    break
                            j += 1
                    else:
                        j += 1
                    continue
                else:
                    j += 1
                    continue
            i = j + 1 if j < n else n
            continue
        i += 1
    return items


def _find_fn_signature_end(code: str, start: int) -> int | None:
    """Find the index just after a `fn` signature (before `{` or `;`),
    respecting nested generics and parens. The `->` arrow's `>` is not a
    generic close delimiter."""
    depth = 0
    generic = 0
    i = start
    n = len(code)
    while i < n:
        ch = code[i]
        if ch in "([":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif ch == "<":
            generic += 1
        elif ch == ">":
            # `->` arrow: previous char is '-'; not a generic close.
            if i > 0 and code[i - 1] == "-":
                pass
            elif generic > 0:
                generic -= 1
        elif ch in "{;" and depth == 0 and generic == 0:
            return i
        i += 1
    return None


def _self_type_qual(self_type: str, module_path: str) -> str:
    """Qualify a bare self type with the module path (e.g. `BranchCmd` ->
    `crate::commands::groups::session::branch::BranchCmd`). Already-qualified
    paths pass through."""
    cleaned = re.sub(r"\s+", "", self_type)
    if cleaned.startswith("crate::") or cleaned.startswith("super::"):
        return cleaned
    # Strip generic args for qualification purposes (path part only).
    base = re.split(r"[<({]", cleaned)[0]
    return f"{module_path}::{base}"


def _selector_matches(selector: dict, item: RustItem) -> bool:
    """Match one RustItem against a checked-in selector (shared by
    `resolve_selector` and the retained-host source scan)."""
    kind = selector["kind"]
    if kind == "free":
        target = "::".join(selector["item"])
        return item.kind == "free" and item.qual_path == target
    if kind == "inherent":
        self_qual = _selector_type_to_text(selector["self_type"])
        return item.kind == "inherent" and item.name == selector["method"] \
            and item.qual_path.startswith(f"{self_qual}::")
    self_qual = _selector_type_to_text(selector["self_type"])
    trait_qual = _selector_type_to_text(selector["trait_path"])
    return item.kind == "trait_impl" and item.name == selector["method"] \
        and item.qual_path.startswith(f"{self_qual}::") \
        and f"[{trait_qual}]" in item.qual_path


def resolve_selector(selector: dict, items: list[RustItem]) -> list[SourceScanViolation]:
    """Resolve one checked-in handler selector against parsed items.

    Returns violations for missing, ambiguous, or non-concrete-App targets.
    """
    violations: list[SourceScanViolation] = []
    kind = selector["kind"]
    if kind == "free":
        target = "::".join(selector["item"])
        matches = [it for it in items if it.kind == "free" and it.qual_path == target]
    elif kind == "inherent":
        self_type = selector["self_type"]
        method = selector["method"]
        self_qual = _selector_type_to_text(self_type)
        matches = [
            it for it in items
            if it.kind == "inherent" and it.name == method and it.qual_path.startswith(f"{self_qual}::")
        ]
    else:  # trait_impl
        self_type = selector["self_type"]
        trait_path = selector["trait_path"]
        method = selector["method"]
        self_qual = _selector_type_to_text(self_type)
        trait_qual = _selector_type_to_text(trait_path)
        matches = [
            it for it in items
            if it.kind == "trait_impl" and it.name == method
            and it.qual_path.startswith(f"{self_qual}::") and f"[{trait_qual}]" in it.qual_path
        ]
    if not matches:
        violations.append(SourceScanViolation(
            "selector-resolve", str(selector),
            "selector resolved to no source item; handler may have moved, been renamed, or the path is stale",
        ))
    elif len(matches) > 1:
        violations.append(SourceScanViolation(
            "selector-resolve", str(selector),
            f"selector resolved to {len(matches)} items (ambiguous): " + "; ".join(m.qual_path for m in matches),
        ))
    elif not matches[0].is_concrete_app:
        violations.append(SourceScanViolation(
            "selector-resolve", str(selector),
            f"resolved handler {matches[0].qual_path} no longer has a concrete-App signature",
        ))
    return violations


def _selector_type_to_text(node) -> str:
    """Render a type-algebra node back to Rust text for matching."""
    if not isinstance(node, dict):
        return str(node)
    tag = node["tag"]
    if tag == "path":
        segments = []
        for seg in node.get("segments", []):
            name = seg["name"] if isinstance(seg, dict) else str(seg)
            segments.append(name)
        prefix = "" if node.get("absolute") else ""
        return prefix + "::".join(segments)
    if tag == "primitive":
        return node["name"]
    if tag == "never":
        return "!"
    if tag == "tuple":
        return "(" + ",".join(_selector_type_to_text(e) for e in node.get("elems", [])) + ")"
    if tag == "reference":
        return "&" + ("mut " if node.get("mut") else "") + _selector_type_to_text(node.get("inner"))
    if tag == "pointer":
        return "*" + ("mut " if node.get("mut") else "const ") + _selector_type_to_text(node.get("inner"))
    if tag == "slice":
        return "[" + _selector_type_to_text(node.get("inner")) + "]"
    if tag == "array":
        return "[" + _selector_type_to_text(node.get("inner")) + "; " + _selector_type_to_text(node.get("len")) + "]"
    return ""


def scan_leaf_handlers(leaf_scope: list[str], root: Path) -> tuple[list[RustItem], list[SourceScanViolation]]:
    """Scan one leaf's source scope for concrete-App handler items.

    `root` is the repo root (scope paths are repo-relative); module paths are
    derived against the groups root internally.
    """
    items: list[RustItem] = []
    violations: list[SourceScanViolation] = []
    for rel in leaf_scope:
        path = root / rel
        if not path.is_file():
            violations.append(SourceScanViolation(
                "source-scan", rel, "scope file missing from the source tree",
            ))
            continue
        try:
            items.extend(parse_rust_file(path, root))
        except Exception as exc:  # pragma: no cover - defensive
            violations.append(SourceScanViolation(
                "source-scan", rel, f"failed to parse: {exc}",
            ))
    return items, violations


UNLISTED_SLICE_ASSIGNMENT: dict[str, dict[str, list[str]]] = {
    "core": {
        "core::navigation": ["crates/tui/src/commands/groups/core/copy.rs"],
        "core::coordination": ["crates/tui/src/commands/groups/core/router.rs"],
    },
}


def slice_source_scope(group_name: str, slice_node: dict) -> list[str]:
    """A slice's historical scope plus the unlisted files the immutable
    topology never assigned to it."""
    extra = UNLISTED_SLICE_ASSIGNMENT.get(group_name, {}).get(slice_node["name"], [])
    return sorted(set(slice_node.get("scope", [])) | set(extra))


def group_source_scope(group_name: str, node: dict, root: Path) -> list[str]:
    """Preserve the immutable historical topology and scan every group source.

    Recursive discovery includes receipts, module roots, helpers and new
    nested files even when the historical manifest never listed them, so a
    handler added outside the frozen scope cannot pass unseen. Relocating
    declared debug paths preserves missing-file diagnostics after extraction.
    """
    scope = set(node.get("scope", []))
    directories = [root / "crates/tui/src/commands/groups" / group_name]
    if group_name == "debug":
        scope = {path.replace("crates/tui/src/commands/groups/debug/",
                              "crates/commands/src/debug/", 1) for path in scope}
        directories.append(root / "crates/commands/src/debug")
    for directory in directories:
        scope.update(path.relative_to(root).as_posix() for path in directory.rglob("*.rs"))
    return sorted(scope)


def check_source_frontier(topology: dict, frontier: list[str], root: Path = REPO_ROOT) -> list[SourceScanViolation]:
    """Bidirectional scan: the frontier must exactly equal the leaves whose
    scopes still contain concrete-App handlers.

    The frontier may name a whole group (all its files pending) or a group's
    declared slices (after a documented split). For every group:

    - group pending: all concrete-App handlers in the group scope are covered.
    - group split: the frontier must name ALL its slices, and every handler
      in the group scope must fall inside a pending slice's scope.
    - otherwise: any concrete-App handler in the group is a stale-removal.
    """
    violations: list[SourceScanViolation] = []
    frontier_set = set(frontier)

    for group_name, node in topology.items():
        group_items, scan_violations = scan_leaf_handlers(
            group_source_scope(group_name, node, root), root
        )
        violations.extend(scan_violations)
        handlers = [it for it in group_items if it.is_concrete_app]

        slices = node.get("slices", [])
        if group_name in frontier_set:
            # Whole group pending: every handler is covered; a pending group
            # with no concrete-App handler is a stale entry.
            if not handlers:
                violations.append(SourceScanViolation(
                    "stale-entry", group_name,
                    "frontier group is pending but its scope has no concrete-App handler",
                ))
            for selector in node.get("handlers", []):
                violations.extend(resolve_selector(selector, group_items))
            continue

        # Validate retained host machinery declarations first so the tracking
        # stays fail-closed even when the group has no other concrete-App
        # handlers (e.g. every retained helper lost its signature at once).
        retained_names: set[str] = set()
        for selector in RETAINED_HOST_MACHINERY.get(group_name, []):
            matches = [it for it in group_items if _selector_matches(selector, it)]
            if not matches:
                violations.append(SourceScanViolation(
                    "retained-host", json.dumps(selector, sort_keys=True),
                    f"retained host machinery selector resolves to no source item in {group_name!r}",
                ))
            for match in matches:
                if not match.is_concrete_app:
                    violations.append(SourceScanViolation(
                        "retained-host", match.qual_path,
                        "retained host machinery must keep its concrete-App signature until FEAT-042 extracts it",
                    ))
                retained_names.add(match.qual_path)

        if not handlers:
            continue

        if slices:
            slice_names = {s["name"] for s in slices}
            slice_pending = slice_names & frontier_set
            if slice_pending == slice_names:
                # Documented split: verify each slice scope holds its handlers.
                for slice_node in slices:
                    slice_items, slice_violations = scan_leaf_handlers(
                        slice_source_scope(group_name, slice_node), root
                    )
                    violations.extend(slice_violations)
                    slice_handlers = [it for it in slice_items if it.is_concrete_app]
                    if not slice_handlers:
                        violations.append(SourceScanViolation(
                            "stale-entry", slice_node["name"],
                            "frontier slice is pending but its scope has no concrete-App handler",
                        ))
                    for selector in slice_node.get("handlers", []):
                        violations.extend(resolve_selector(selector, slice_items))
                continue
            if slice_pending:
                # Partial split: some slices pending, some not.
                missing = sorted(slice_names - slice_pending)
                violations.append(SourceScanViolation(
                    "partial-split", group_name,
                    f"group is neither wholly pending nor fully split; pending slices "
                    f"{sorted(slice_pending)} but missing {missing}",
                ))
                continue

        # Not pending and not split: every remaining handler is a stale removal,
        # except the retained host machinery resolved above.
        stale = [h for h in handlers if h.qual_path not in retained_names]
        for handler in stale[:5]:
            violations.append(SourceScanViolation(
                "stale-removal", handler.qual_path,
                f"handler still uses concrete App but group {group_name!r} is not pending",
            ))
        if len(stale) > 5:
            violations.append(SourceScanViolation(
                "stale-removal", group_name,
                f"... and {len(stale) - 5} more concrete-App handlers in this group",
            ))

    return violations


def _leaf_node(topology: dict, leaf_name: str) -> dict | None:
    if leaf_name in topology:
        return topology[leaf_name]
    for root_name, node in topology.items():
        for slice_node in node.get("slices", []):
            if slice_node["name"] == leaf_name:
                return slice_node
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--baseline-ref",
        help="Git revision whose topology/frontier must transition monotonically to the current one",
    )
    args = parser.parse_args(argv)

    try:
        doc = load_topology()
        pending = load_pending_groups()
        baseline_ref = args.baseline_ref or detect_local_baseline_ref()
        previous = load_topology_at_ref(baseline_ref) if baseline_ref else None
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"[command-migration-manifest] FAIL: {error}", file=sys.stderr)
        return 1

    violations = validate_topology_document(doc)
    violations.extend(validate_pending_projection(doc, pending))
    violations.extend(validate_baseline_transition(doc, previous))
    if violations:
        print("[command-migration-manifest] FAIL", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    source_violations = check_source_frontier(doc["topology"], doc["frontier"])
    if source_violations:
        print("[command-migration-manifest] FAIL (source scan)", file=sys.stderr)
        for violation in source_violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    frontier = doc["frontier"]
    transition = f"; baseline {baseline_ref}" if baseline_ref else "; initialization"
    print(
        f"[command-migration-manifest] PASS: schema v{SUPPORTED_SCHEMA_VERSION}; "
        f"frontier [{', '.join(frontier)}] matches TUI projection and source{transition}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
