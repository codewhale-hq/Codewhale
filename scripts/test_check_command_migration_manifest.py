#!/usr/bin/env python3
"""Hermetic tests for the FEAT-015 command migration manifest gate.

Covers the Deep-Dive schema and frontier rules:

- valid root frontier; valid parent-to-all-children replacement; valid removal
- rejected partial split; rejected arbitrary addition
- duplicate/unsorted entry; unknown schema/tag/field; unsupported syntax
- missing/overlapping selector; const normalization
- integer/character/byte/path const atoms; out-of-range byte diagnostics
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check-command-migration-manifest.py"
SPEC = importlib.util.spec_from_file_location("command_migration", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


def sample_topology() -> dict:
    """A minimal two-group topology with one documented slice per group."""
    return {
        "schema_version": 1,
        "topology": {
            "utility": {
                "kind": "group",
                "scope": ["crates/tui/src/commands/groups/utility/mod.rs"],
                "slices": [],
            },
            "session": {
                "kind": "group",
                "scope": ["crates/tui/src/commands/groups/session/mod.rs"],
                "slices": [
                    {
                        "name": "session::lifecycle",
                        "kind": "slice",
                        "scope": ["crates/tui/src/commands/groups/session/branch.rs"],
                    },
                    {
                        "name": "session::control",
                        "kind": "slice",
                        "scope": ["crates/tui/src/commands/groups/session/relay.rs"],
                    },
                ],
            },
        },
        "frontier": ["session", "utility"],
    }


class SchemaTests(unittest.TestCase):
    def test_valid_document_passes(self) -> None:
        self.assertEqual(mod.validate_topology_document(sample_topology()), [])

    def test_unknown_schema_version_fails(self) -> None:
        doc = sample_topology()
        doc["schema_version"] = 2
        violations = mod.validate_topology_document(doc)
        self.assertEqual(len(violations), 1)
        self.assertIn("schema_version", str(violations[0]))

    def test_missing_schema_version_fails(self) -> None:
        doc = sample_topology()
        del doc["schema_version"]
        self.assertTrue(mod.validate_topology_document(doc))

    def test_unknown_group_field_fails(self) -> None:
        doc = sample_topology()
        doc["topology"]["utility"]["extra"] = True
        violations = mod.validate_topology_document(doc)
        self.assertTrue(any("extra" in str(v) for v in violations))

    def test_slice_without_group_prefix_fails(self) -> None:
        doc = sample_topology()
        doc["topology"]["session"]["slices"][0]["name"] = "other::slice"
        violations = mod.validate_topology_document(doc)
        self.assertTrue(any("must start with" in str(v) for v in violations))

    def test_duplicate_slice_name_fails(self) -> None:
        doc = sample_topology()
        doc["topology"]["session"]["slices"].append(
            doc["topology"]["session"]["slices"][0]
        )
        violations = mod.validate_topology_document(doc)
        self.assertTrue(any("duplicate slice" in str(v) for v in violations))


class FrontierTests(unittest.TestCase):
    def test_valid_root_frontier_passes(self) -> None:
        doc = sample_topology()
        self.assertEqual(mod.validate_frontier(doc["topology"], doc["frontier"]), [])

    def test_unsorted_frontier_fails(self) -> None:
        doc = sample_topology()
        violations = mod.validate_frontier(doc["topology"], ["utility", "session"])
        self.assertTrue(any("sorted" in str(v) for v in violations))

    def test_duplicate_frontier_entry_fails(self) -> None:
        doc = sample_topology()
        violations = mod.validate_frontier(doc["topology"], ["session", "session"])
        self.assertTrue(any("duplicates" in str(v) for v in violations))

    def test_unknown_frontier_leaf_fails(self) -> None:
        doc = sample_topology()
        violations = mod.validate_frontier(doc["topology"], ["session", "ghost"])
        self.assertTrue(any("not a declared topology leaf" in str(v) for v in violations))

    def test_removal_is_valid_transition(self) -> None:
        doc = sample_topology()
        old = ["session", "utility"]
        new = ["utility"]
        self.assertEqual(mod.is_valid_frontier_transition(doc["topology"], old, new), [])

    def test_parent_to_all_children_is_valid_transition(self) -> None:
        doc = sample_topology()
        old = ["session", "utility"]
        new = ["session::control", "session::lifecycle", "utility"]
        self.assertEqual(mod.is_valid_frontier_transition(doc["topology"], old, new), [])

    def test_partial_split_is_rejected(self) -> None:
        doc = sample_topology()
        old = ["session", "utility"]
        new = ["session::lifecycle", "utility"]
        violations = mod.is_valid_frontier_transition(doc["topology"], old, new)
        self.assertTrue(violations, "partial split must be rejected")

    def test_arbitrary_addition_is_rejected(self) -> None:
        doc = sample_topology()
        old = ["session", "utility"]
        new = ["ghost", "session", "utility"]
        violations = mod.is_valid_frontier_transition(doc["topology"], old, new)
        self.assertTrue(violations, "arbitrary growth must be rejected")


class LiveTransitionTests(unittest.TestCase):
    def test_invalid_current_manifest_fails_closed_before_transition(self) -> None:
        violations = mod.validate_baseline_transition({}, None)
        self.assertTrue(any("current topology is invalid" in str(v) for v in violations))

    def test_initial_manifest_must_start_at_all_roots(self) -> None:
        doc = sample_topology()
        self.assertEqual(mod.validate_baseline_transition(doc, None), [])
        doc["frontier"] = ["utility"]
        violations = mod.validate_baseline_transition(doc, None)
        self.assertTrue(any("first manifest revision" in str(v) for v in violations))

    def test_live_transition_rejects_topology_mutation(self) -> None:
        previous = sample_topology()
        current = json.loads(json.dumps(previous))
        current["topology"]["utility"]["scope"].append("new.rs")
        violations = mod.validate_baseline_transition(current, previous)
        self.assertTrue(any("topology is immutable" in str(v) for v in violations))

    def test_live_transition_accepts_documented_split(self) -> None:
        previous = sample_topology()
        current = json.loads(json.dumps(previous))
        current["frontier"] = ["session::control", "session::lifecycle", "utility"]
        self.assertEqual(mod.validate_baseline_transition(current, previous), [])

    def test_live_transition_rejects_arbitrary_growth(self) -> None:
        previous = sample_topology()
        previous["frontier"] = ["utility"]
        current = json.loads(json.dumps(previous))
        current["frontier"] = ["session", "utility"]
        violations = mod.validate_baseline_transition(current, previous)
        self.assertTrue(any("arbitrary growth" in str(v) for v in violations))

    def test_pending_projection_must_equal_json_frontier(self) -> None:
        doc = sample_topology()
        self.assertEqual(
            mod.validate_pending_projection(doc, ["session", "utility"]), []
        )
        violations = mod.validate_pending_projection(doc, ["utility"])
        self.assertTrue(any("does not equal JSON frontier" in str(v) for v in violations))

    def test_pending_groups_parser_is_string_only_and_order_preserving(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "contract.rs"
            path.write_text(
                'pub(crate) const PENDING_GROUPS: &[&str] = &["session", "utility"];\n',
                encoding="utf-8",
            )
            self.assertEqual(mod.load_pending_groups(path), ["session", "utility"])


class GitBaselineTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.git("init", "-q", "--template=", "-b", "main")
        self.git("config", "user.name", "Manifest test")
        self.git("config", "user.email", "manifest@example.invalid")
        self.git("config", "commit.gpgsign", "false")

    def git(self, *args: str) -> str:
        return subprocess.check_output(
            ["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE,
        ).strip()

    def commit(self, frontier: list[str]) -> str:
        doc = sample_topology()
        doc["frontier"] = frontier
        target = self.root / mod.TOPOLOGY_REPO_PATH
        target.parent.mkdir(exist_ok=True)
        target.write_text(json.dumps(doc), encoding="utf-8")
        self.git("add", mod.TOPOLOGY_REPO_PATH)
        self.git("commit", "-qm", "Update frontier", "--allow-empty")
        return self.git("rev-parse", "HEAD")

    def test_aligned_main_uses_parent_and_still_rejects_growth(self) -> None:
        previous = self.commit(["utility"])
        head = self.commit(["session", "utility"])
        self.git("update-ref", "refs/remotes/origin/main", head)
        baseline = mod.detect_local_baseline_ref(self.root)
        self.assertEqual(baseline, previous)
        violations = mod.validate_baseline_transition(
            mod.load_topology_at_ref(head, self.root),
            mod.load_topology_at_ref(baseline, self.root),
        )
        self.assertTrue(any("arbitrary growth" in str(v) for v in violations))

    def test_feature_branch_keeps_merge_base_across_multiple_commits(self) -> None:
        base = self.commit(["session", "utility"])
        self.git("update-ref", "refs/remotes/origin/main", base)
        self.commit(["utility"])
        self.commit(["utility"])
        self.assertEqual(mod.detect_local_baseline_ref(self.root), base)

    def test_history_without_remote_uses_parent(self) -> None:
        previous = self.commit(["session", "utility"])
        self.commit(["utility"])
        self.assertEqual(mod.detect_local_baseline_ref(self.root), previous)

    def test_initial_commit_has_no_baseline(self) -> None:
        head = self.commit(["session", "utility"])
        self.git("update-ref", "refs/remotes/origin/main", head)
        self.assertIsNone(mod.detect_local_baseline_ref(self.root))


class SelectorTests(unittest.TestCase):
    def test_free_selector_passes(self) -> None:
        selector = {"kind": "free", "item": ["crate", "commands", "groups", "session", "run_save"]}
        self.assertEqual(mod.validate_selector(selector, "s"), [])

    def test_inherent_selector_passes(self) -> None:
        selector = {
            "kind": "inherent",
            "self_type": {
                "tag": "path",
                "absolute": True,
                "segments": [
                    {"name": "crate"},
                    {"name": "commands", "args": []},
                    {"name": "groups", "args": []},
                    {"name": "session", "args": []},
                    {"name": "branch", "args": []},
                    {"name": "BranchCmd", "args": []},
                ],
            },
            "method": "execute",
        }
        self.assertEqual(mod.validate_selector(selector, "s"), [])

    def test_trait_impl_selector_passes(self) -> None:
        selector = {
            "kind": "trait_impl",
            "self_type": {"tag": "path", "absolute": True, "segments": [{"name": "BranchCmd"}]},
            "trait_path": {"tag": "path", "absolute": True, "segments": [{"name": "RegisterCommand"}]},
            "method": "execute",
        }
        self.assertEqual(mod.validate_selector(selector, "s"), [])

    def test_unknown_selector_kind_fails(self) -> None:
        selector = {"kind": "static", "item": ["a", "b"]}
        violations = mod.validate_selector(selector, "s")
        self.assertTrue(any("unknown selector kind" in str(v) for v in violations))

    def test_free_selector_missing_function_fails(self) -> None:
        selector = {"kind": "free", "item": ["crate"]}
        violations = mod.validate_selector(selector, "s")
        self.assertTrue(any("module path array" in str(v) for v in violations))

    def test_inherent_selector_missing_method_fails(self) -> None:
        selector = {"kind": "inherent", "self_type": {"tag": "never"}}
        violations = mod.validate_selector(selector, "s")
        self.assertTrue(any("inherent.method" in str(v) for v in violations))

    def test_unknown_self_type_tag_fails(self) -> None:
        selector = {"kind": "inherent", "self_type": {"tag": "fn_ptr"}, "method": "execute"}
        violations = mod.validate_selector(selector, "s")
        self.assertTrue(any("unknown type node tag" in str(v) for v in violations))


class TypeAlgebraTests(unittest.TestCase):
    def test_primitive_and_never_pass(self) -> None:
        self.assertEqual(mod.validate_type_node({"tag": "primitive", "name": "u8"}, "t"), [])
        self.assertEqual(mod.validate_type_node({"tag": "never"}, "t"), [])

    def test_unknown_primitive_fails(self) -> None:
        violations = mod.validate_type_node({"tag": "primitive", "name": "u24"}, "t")
        self.assertTrue(violations)

    def test_tuple_and_slice_pass(self) -> None:
        self.assertEqual(
            mod.validate_type_node({"tag": "tuple", "elems": [{"tag": "never"}]}, "t"), []
        )
        self.assertEqual(
            mod.validate_type_node({"tag": "slice", "inner": {"tag": "primitive", "name": "u8"}}, "t"),
            [],
        )

    def test_reference_with_mut_passes(self) -> None:
        self.assertEqual(
            mod.validate_type_node(
                {"tag": "reference", "mut": True, "inner": {"tag": "primitive", "name": "str"}},
                "t",
            ),
            [],
        )

    def test_array_with_byte_len_passes(self) -> None:
        node = {
            "tag": "array",
            "inner": {"tag": "primitive", "name": "u8"},
            "len": {"tag": "int", "negative": False, "magnitude": "8", "suffix": None},
        }
        self.assertEqual(mod.validate_type_node(node, "t"), [])

    def test_generic_const_argument_normalization(self) -> None:
        # byte literal 255 with u8 suffix passes; 256 fails with field diagnostic
        ok = {"tag": "const", "atom": {"tag": "int", "negative": False, "magnitude": "255", "suffix": "u8"}}
        self.assertEqual(mod.validate_generic_arg(ok, "g"), [])
        bad = {"tag": "const", "atom": {"tag": "int", "negative": False, "magnitude": "256", "suffix": "u8"}}
        violations = mod.validate_generic_arg(bad, "g")
        self.assertTrue(any("byte value 256 exceeds" in str(v) for v in violations))


class ConstAtomTests(unittest.TestCase):
    def test_bool_passes(self) -> None:
        self.assertEqual(mod.validate_const_atom({"tag": "bool", "value": True}, "c"), [])

    def test_bool_non_bool_value_fails(self) -> None:
        violations = mod.validate_const_atom({"tag": "bool", "value": 1}, "c")
        self.assertTrue(violations)

    def test_integer_canonical_magnitude(self) -> None:
        self.assertEqual(
            mod.validate_const_atom(
                {"tag": "int", "negative": False, "magnitude": "0", "suffix": None}, "c"
            ),
            [],
        )
        self.assertEqual(
            mod.validate_const_atom(
                {"tag": "int", "negative": False, "magnitude": "42", "suffix": "u32"}, "c"
            ),
            [],
        )
        violations = mod.validate_const_atom(
            {"tag": "int", "negative": False, "magnitude": "042", "suffix": None}, "c"
        )
        self.assertTrue(any("leading zeros" in str(v) for v in violations))

    def test_negative_zero_fails(self) -> None:
        violations = mod.validate_const_atom(
            {"tag": "int", "negative": True, "magnitude": "0", "suffix": None}, "c"
        )
        self.assertTrue(any("negative zero" in str(v) for v in violations))

    def test_unknown_suffix_fails(self) -> None:
        violations = mod.validate_const_atom(
            {"tag": "int", "negative": False, "magnitude": "1", "suffix": "u33"}, "c"
        )
        self.assertTrue(any("suffix" in str(v) for v in violations))

    def test_char_scalar_passes(self) -> None:
        self.assertEqual(mod.validate_const_atom({"tag": "char", "scalar": "x"}, "c"), [])

    def test_char_multi_scalar_fails(self) -> None:
        violations = mod.validate_const_atom({"tag": "char", "scalar": "xy"}, "c")
        self.assertTrue(any("exactly one" in str(v) for v in violations))

    def test_path_const_passes(self) -> None:
        atom = {"tag": "path", "absolute": True, "segments": ["SIZE"]}
        self.assertEqual(mod.validate_const_atom(atom, "c"), [])

    def test_unknown_tag_fails(self) -> None:
        violations = mod.validate_const_atom({"tag": "float", "value": 1.0}, "c")
        self.assertTrue(any("unknown const atom tag" in str(v) for v in violations))

    def test_extra_field_fails(self) -> None:
        violations = mod.validate_const_atom(
            {"tag": "int", "negative": False, "magnitude": "1", "suffix": None, "extra": True}, "c"
        )
        self.assertTrue(any("exactly" in str(v) for v in violations))


class LiveGateTests(unittest.TestCase):
    def test_real_topology_passes_live_gate(self) -> None:
        doc = mod.load_topology()
        self.assertEqual(mod.validate_topology_document(doc), [])

    def test_topology_artifact_is_sorted_unique(self) -> None:
        doc = mod.load_topology()
        frontier = doc["frontier"]
        self.assertEqual(frontier, sorted(frontier))
        self.assertEqual(len(frontier), len(set(frontier)))
        # FEAT-018 removed utility, FEAT-019 removed memory, FEAT-020 removed plugins,
        # FEAT-021 removed project, FEAT-022 removed skills, FEAT-029 completed debug,
        # FEAT-026 completed session, and FEAT-028 completed config; core remains.
        self.assertEqual(
            set(frontier),
            {"core"},
        )


class ModelAndSetupFrontierTests(unittest.TestCase):
    def test_complete_model_and_setup_slice_uses_portable_source_files(self) -> None:
        core = mod.load_topology()["topology"]["core"]
        node = next(node for node in core["slices"] if node["name"] == "core::model-and-setup")
        scope = mod.slice_source_scope("core", node)
        owners = {path.relative_to(ROOT).as_posix() for path in
                  (ROOT / "crates/commands/src/model_setup").glob("*.rs") if path.name != "mod.rs"}
        self.assertEqual(set(scope), owners)
        self.assertTrue(set(scope).issubset(mod.group_source_scope("core", core, ROOT)))
        self.assertFalse(any((ROOT / path).exists() for path in node["scope"]))
        items, errors = mod.scan_leaf_handlers(scope, ROOT)
        self.assertEqual(errors, [])
        self.assertEqual([item.qual_path for item in items if item.is_concrete_app], [])


class DebugGroupFrontierTests(unittest.TestCase):
    def test_complete_debug_inventory_has_no_pending_host_handlers(self) -> None:
        doc = mod.load_topology()
        debug = doc["topology"]["debug"]
        actual = {p.relative_to(ROOT).as_posix() for p in
                  (ROOT / "crates/commands/src/debug").rglob("*.rs")}
        self.assertEqual(set(mod.group_source_scope("debug", debug, ROOT)), actual)
        self.assertNotIn("debug", doc["frontier"])
        self.assertNotIn("debug", mod.load_pending_groups())
        self.assertEqual(mod.check_source_frontier({"debug": debug}, [], ROOT), [])
        self.assertIn("crates/commands/src/debug/receipts.rs", actual)

    def test_omitted_receipts_and_new_nested_commands_fail_inventory(self) -> None:
        for filename in ["receipts.rs", "new/command.rs"]:
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                directory = root / "crates/commands/src/debug"
                directory.mkdir(parents=True)
                (directory / "mod.rs").write_text("// complete portable module\n")
                omitted = directory / filename
                omitted.parent.mkdir(parents=True, exist_ok=True)
                omitted.write_text("pub fn receipt(app: &mut App) {}\n")
                scope = [(directory / "mod.rs").relative_to(root).as_posix()]
                topology = {"debug": {"kind": "group", "scope": scope, "slices": []}}
                violations = mod.check_source_frontier(topology, [], root)
                self.assertTrue(any(v.category == "stale-removal" and
                                    v.location.endswith("::receipt")
                                    for v in violations), violations)
                # Adding the historical scope entry cannot declare the handler portable.
                scope.append(omitted.relative_to(root).as_posix())
                violations = mod.check_source_frontier(topology, [], root)
                self.assertTrue(any(v.category == "stale-removal" for v in violations), violations)

    def test_omitted_pure_file_is_automatically_included_in_scan(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = root / "crates/commands/src/debug"
            directory.mkdir(parents=True)
            (directory / "future.rs").write_text("pub fn future() {}\n")
            node = {"kind": "group", "scope": [], "slices": []}
            self.assertEqual(mod.group_source_scope("debug", node, root),
                             ["crates/commands/src/debug/future.rs"])
            self.assertEqual(mod.check_source_frontier({"debug": node}, [], root), [])


class GateScopeTests(unittest.TestCase):
    def _group_root(self, tmp: str, group: str, files: dict[str, str]) -> Path:
        root = Path(tmp)
        base = root / "crates/tui/src/commands/groups" / group
        for name, text in files.items():
            (base / name).parent.mkdir(parents=True, exist_ok=True)
            (base / name).write_text(text, encoding="utf-8")
        return root

    def test_cfg_test_items_and_tests_modules_are_not_handlers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._group_root(tmp, "alpha", {"mod.rs": (
                "use a::{b, c};\n"
                "#[cfg(test)]\nuse x::{y, z};\n"
                "#[cfg(test)]\nfn helper(app: &mut App) -> R { app.go() }\n"
                "#[cfg(test)]\n#[allow(dead_code)]\nfn helper2(app: &mut App) {}\n"
                "mod tests {\n    fn inner(app: &mut App) {}\n}\n"
                "fn real(app: &mut App) {}\n"
            )})
            items, _ = mod.scan_leaf_handlers(["crates/tui/src/commands/groups/alpha/mod.rs"], root)
            self.assertEqual([i.name for i in items if i.is_concrete_app], ["real"])

    def test_cfg_test_on_a_non_item_does_not_hide_later_handlers(self) -> None:
        fixtures = {
            "field": ("struct S { #[cfg(test)] probe: u8, b: u8 }\n"
                      "fn real(app: &mut App) { app.go() }\n", ["real"]),
            "variant": "enum E { A, #[cfg(test)] B }\nfn real(app: &mut App) {}\n",
            "parameter": "fn pre(#[cfg(test)] a: u8) {}\nfn real(app: &mut App) {}\n",
            "match_arm": ("fn pre(e: E) { match e { E::A => {}, #[cfg(test)] E::B => {} } }\n"
                          "fn real(app: &mut App) {}\n"),
            "const_fn": ("#[cfg(test)]\nconst fn h(app: &mut App) -> u8 { 1 }\n"
                         "fn real(app: &mut App) { app.go() }\n"
                         "fn real2(app: &mut App) {}\nuse a::b;\n", ["real", "real2"]),
            "const_unsafe_fn": ("#[cfg(test)]\npub(crate) const unsafe fn h(app: &mut App) {}\n"
                                "fn real(app: &mut App) {}\nuse a::b;\n"),
            "const_item": ("#[cfg(test)]\nconst H: fn(&mut App) = { fn h(app: &mut App) {} h };\n"
                           "fn real(app: &mut App) {}\n"),
        }
        for name, fixture in fixtures.items():
            text, expected = fixture if isinstance(fixture, tuple) else (fixture, ["real"])
            with self.subTest(name), tempfile.TemporaryDirectory() as tmp:
                root = self._group_root(tmp, "alpha", {"mod.rs": text})
                items, _ = mod.scan_leaf_handlers(["crates/tui/src/commands/groups/alpha/mod.rs"], root)
                self.assertEqual([i.name for i in items if i.is_concrete_app], expected)

    def test_every_group_directory_is_scanned_and_unlisted_handler_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._group_root(tmp, "alpha", {
                "mod.rs": "fn ok() {}\n",
                "extra.rs": "fn sneaky(app: &mut App) {}\n",
            })
            node = {"kind": "group", "scope": ["crates/tui/src/commands/groups/alpha/mod.rs"], "slices": []}
            self.assertIn("crates/tui/src/commands/groups/alpha/extra.rs",
                          mod.group_source_scope("alpha", node, root))
            violations = mod.check_source_frontier({"alpha": node}, [], root)
            self.assertTrue(any(v.category == "stale-removal" and v.location.endswith("::sneaky")
                                for v in violations), violations)

    def test_unlisted_core_files_are_assigned_to_slices(self) -> None:
        navigation = {"name": "core::navigation", "scope": ["a.rs"]}
        self.assertEqual(mod.slice_source_scope("core", navigation),
                         ["a.rs", "crates/tui/src/commands/groups/core/copy.rs"])
        self.assertEqual(mod.slice_source_scope("core", {"name": "core::policy", "scope": ["b.rs"]}),
                         ["b.rs"])

    def test_immutable_and_qualified_app_references_are_concrete(self) -> None:
        for ty in ["&mut App", "&crate::tui::app::App", "&App", "&mut crate::tui::app::App"]:
            self.assertTrue(mod._is_concrete_app_type(ty), ty)
        for ty in ["Option<&str>", "&AppConfig", "&mut Vec<App>"]:
            self.assertFalse(mod._is_concrete_app_type(ty), ty)

    def test_discover_visible_skills_is_retained_host_machinery(self) -> None:
        names = [sel["item"][-1] for sel in mod.RETAINED_HOST_MACHINERY["skills"]]
        self.assertIn("discover_visible_skills", names)


class SourceScanTests(unittest.TestCase):
    """Hermetic fixtures for the AST-resolved source scan (Task 3.5/3.6)."""

    def _write_group(self, tmpdir: Path, group: str, files: dict[str, str]) -> Path:
        """Write a synthetic group tree under a temp groups root."""
        base = tmpdir / group
        base.mkdir(parents=True, exist_ok=True)
        for name, content in files.items():
            (base / name).write_text(content, encoding="utf-8")
        return tmpdir

    def test_parse_finds_concrete_app_free_fn(self) -> None:
        source = (
            "use crate::tui::app::App;\n"
            "pub fn run_config(app: &mut App, arg: Option<&str>) -> CommandResult {\n"
            "    CommandResult::ok()\n"
            "}\n"
        )
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "config").mkdir()
            (root / "config" / "mod.rs").write_text(source, encoding="utf-8")
            items = mod.parse_rust_file(root / "config" / "mod.rs", root)
            apps = [it for it in items if it.is_concrete_app]
            self.assertEqual(len(apps), 1)
            self.assertEqual(apps[0].kind, "free")
            self.assertEqual(apps[0].qual_path, "crate::commands::groups::config::run_config")

    def test_parse_ignores_non_app_fns(self) -> None:
        source = (
            "fn helper(value: u32) -> u32 { value }\n"
            "fn run(app: &mut crate::tui::app::App, arg: Option<&str>) -> CommandResult {\n"
            "    CommandResult::ok()\n"
            "}\n"
        )
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "core").mkdir()
            (root / "core" / "mod.rs").write_text(source, encoding="utf-8")
            items = mod.parse_rust_file(root / "core" / "mod.rs", root)
            self.assertEqual(sum(1 for it in items if it.is_concrete_app), 1)

    def test_parse_finds_trait_impl_and_inherent_methods(self) -> None:
        source = (
            "pub struct BranchCmd;\n"
            "impl RegisterCommand for BranchCmd {\n"
            "    fn info() -> &'static CommandInfo { &INFO }\n"
            "    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {\n"
            "        branch(app, arg)\n"
            "    }\n"
            "}\n"
            "impl BranchCmd {\n"
            "    pub fn helper(&self) -> u32 { 1 }\n"
            "}\n"
        )
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "session").mkdir()
            (root / "session" / "mod.rs").write_text(source, encoding="utf-8")
            items = mod.parse_rust_file(root / "session" / "mod.rs", root)
            trait_exec = [it for it in items if it.kind == "trait_impl" and it.name == "execute"]
            self.assertEqual(len(trait_exec), 1)
            self.assertTrue(trait_exec[0].is_concrete_app)
            inherent = [it for it in items if it.kind == "inherent"]
            self.assertEqual(len(inherent), 1)
            self.assertEqual(inherent[0].name, "helper")
            self.assertFalse(inherent[0].is_concrete_app)

    def test_scope_file_missing_fails(self) -> None:
        violations = mod.scan_leaf_handlers(["crates/tui/src/commands/groups/core/ghost.rs"], Path("/nonexistent"))[1]
        self.assertTrue(any("missing" in str(v) for v in violations))

    def test_frontier_matches_group_source(self) -> None:
        doc = sample_topology()
        # utility scope has one concrete-App handler; session scope has one.
        doc["topology"]["utility"]["scope"] = ["utility/mod.rs"]
        doc["topology"]["session"]["scope"] = ["session/mod.rs"]
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "utility").mkdir(parents=True)
            (root / "session").mkdir(parents=True)
            (root / "utility" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn run_util(app: &mut App, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            (root / "session" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn run_save(app: &mut App, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            violations = mod.check_source_frontier(doc["topology"], doc["frontier"], root)
            self.assertEqual(violations, [])

    def test_cheating_removal_fails(self) -> None:
        doc = sample_topology()
        doc["topology"]["utility"]["scope"] = ["utility/mod.rs"]
        doc["topology"]["session"]["scope"] = ["session/mod.rs"]
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "utility").mkdir(parents=True)
            (root / "session").mkdir(parents=True)
            (root / "utility" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn run_util(app: &mut App, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            (root / "session" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn run_save(app: &mut App, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            # Cheat: remove utility from the frontier while its handler remains.
            violations = mod.check_source_frontier(doc["topology"], ["session"], root)
            self.assertTrue(any("stale-removal" in str(v) for v in violations))

    def test_stale_frontier_entry_fails(self) -> None:
        doc = sample_topology()
        doc["topology"]["utility"]["scope"] = ["utility/mod.rs"]
        doc["topology"]["session"]["scope"] = ["session/mod.rs"]
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "utility").mkdir(parents=True)
            (root / "session").mkdir(parents=True)
            # utility has NO concrete-App handler (migrated: uses a context)
            (root / "utility" / "mod.rs").write_text(
                "fn run_util(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            (root / "session" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn run_save(app: &mut App, arg: Option<&str>) -> CommandResult { CommandResult::ok() }\n",
                encoding="utf-8",
            )
            violations = mod.check_source_frontier(doc["topology"], ["session", "utility"], root)
            self.assertTrue(any("stale-entry" in str(v) for v in violations))

    def test_retained_host_exempts_declared_machinery_from_stale_removal(self) -> None:
        """A migrated group may declare dispatcher host machinery (FEAT-042)
        that keeps `&mut App`; the gate exempts it and flags the rest."""
        topology = {
            "alpha": {
                "kind": "group",
                "scope": ["alpha/mod.rs"],
                "slices": [],
            }
        }
        frontier: list[str] = []
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "alpha").mkdir(parents=True, exist_ok=True)
            (root / "alpha" / "mod.rs").write_text(
                "use crate::tui::app::App;\n"
                "fn retained(app: &mut App, arg: Option<&str>) {} \n"
                "fn stale(app: &mut App, arg: Option<&str>) {} \n",
                encoding="utf-8",
            )
            # stub RETAINED_HOST_MACHINERY for the hermetic group
            original = mod.RETAINED_HOST_MACHINERY
            try:
                mod.RETAINED_HOST_MACHINERY = {
                    "alpha": [
                        {"kind": "free", "item": ["crate", "commands", "groups", "alpha", "retained"]}
                    ]
                }
                violations = mod.check_source_frontier(topology, frontier, root)
            finally:
                mod.RETAINED_HOST_MACHINERY = original
            kinds = [v.category for v in violations]
            self.assertNotIn("retained-host", kinds)
            self.assertEqual(kinds.count("stale-removal"), 1, violations)

    def test_retained_host_fails_closed_when_signature_lost(self) -> None:
        """If retained machinery loses its concrete-App signature, the gate fails."""
        topology = {
            "alpha": {
                "kind": "group",
                "scope": ["alpha/mod.rs"],
                "slices": [],
            }
        }
        frontier: list[str] = []
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "alpha").mkdir(parents=True, exist_ok=True)
            (root / "alpha" / "mod.rs").write_text(
                "fn retained(arg: Option<&str>) {} \n",
                encoding="utf-8",
            )
            original = mod.RETAINED_HOST_MACHINERY
            try:
                mod.RETAINED_HOST_MACHINERY = {
                    "alpha": [
                        {"kind": "free", "item": ["crate", "commands", "groups", "alpha", "retained"]}
                    ]
                }
                violations = mod.check_source_frontier(topology, frontier, root)
            finally:
                mod.RETAINED_HOST_MACHINERY = original
            self.assertTrue(
                any(v.category == "retained-host" for v in violations),
                f"expected retained-host violation, got {violations}",
            )

    def test_live_source_gate_passes(self) -> None:
        doc = mod.load_topology()
        violations = mod.check_source_frontier(doc["topology"], doc["frontier"])
        self.assertEqual(violations, [])


if __name__ == "__main__":
    unittest.main()
