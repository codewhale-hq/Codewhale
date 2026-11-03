#!/usr/bin/env python3
"""Hermetic tests for the FEAT-014 command-contract boundary gate."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check-command-crate-boundaries.py"
SPEC = importlib.util.spec_from_file_location("command_boundary", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


def valid_graph() -> dict[str, set[str]]:
    return {
        "codewhale-command-contract": {"codewhale-core"},
        "codewhale-core": set(),
        "codewhale-secrets": {"codewhale-paths"},
        "codewhale-paths": set(),
        "codewhale-tui": set(),
    }


class DependencyTests(unittest.TestCase):
    def test_leaf_graph_passes(self) -> None:
        self.assertEqual(mod.check_dependency_graph(valid_graph()), [])

    def test_direct_tui_edge_fails(self) -> None:
        graph = valid_graph()
        graph["codewhale-command-contract"].add("codewhale-tui")
        self.assertEqual(len(mod.check_dependency_graph(graph)), 1)

    def test_transitive_tui_edge_fails(self) -> None:
        graph = valid_graph()
        graph["codewhale-core"].add("codewhale-tui")
        self.assertEqual(len(mod.check_dependency_graph(graph)), 1)

    def test_missing_contract_fails(self) -> None:
        graph = valid_graph()
        del graph["codewhale-command-contract"]
        violations = mod.check_dependency_graph(graph)
        self.assertEqual(len(violations), 1)
        self.assertIn("missing", str(violations[0]))

    def test_missing_sanitizer_fails(self) -> None:
        graph = valid_graph()
        del graph["codewhale-secrets"]
        violations = mod.check_dependency_graph(graph)
        self.assertEqual(len(violations), 1)
        self.assertIn("codewhale-secrets", str(violations[0]))

    def test_sanitizer_reaching_tui_fails(self) -> None:
        # The shared sanitizer is consumed by portable command helpers, so a TUI
        # edge would pull the whole TUI into the extracted command crate.
        graph = valid_graph()
        graph["codewhale-paths"].add("codewhale-tui")
        violations = mod.check_dependency_graph(graph)
        self.assertEqual(len(violations), 1)
        self.assertIn("codewhale-secrets", str(violations[0]))

    def test_both_packages_reaching_tui_fails_twice(self) -> None:
        graph = valid_graph()
        graph["codewhale-core"].add("codewhale-tui")
        graph["codewhale-paths"].add("codewhale-tui")
        self.assertEqual(len(mod.check_dependency_graph(graph)), 2)

    def test_dev_dependency_is_not_a_normal_edge(self) -> None:
        metadata = {"packages": [
            {"name": "codewhale-command-contract", "dependencies": [
                {"name": "codewhale-tui", "kind": "dev"},
                {"name": "codewhale-core", "kind": None},
            ]},
            {"name": "codewhale-core", "dependencies": []},
            {"name": "codewhale-secrets", "dependencies": []},
            {"name": "codewhale-tui", "dependencies": []},
        ]}
        graph = mod.dependency_graph(metadata)
        self.assertEqual(graph["codewhale-command-contract"], {"codewhale-core"})
        self.assertEqual(mod.check_dependency_graph(graph), [])


class SourceTests(unittest.TestCase):
    def test_clean_shapes_pass(self) -> None:
        source = "pub struct CommandContexts<'a> {}\npub trait CommandModelContext {}\n"
        self.assertEqual(mod.check_contract_source_text(source, "clean.rs"), [])

    def test_forbidden_edges_fail(self) -> None:
        cases = [
            "use codewhale_tui::tui::app::App;",
            "use ratatui::widgets::Paragraph;",
            "use crate::tui::App;",
            "pub struct CommandContext {}",
            "let handler: Box<dyn Fn()> = value;",
        ]
        for source in cases:
            with self.subTest(source=source):
                self.assertTrue(mod.check_contract_source_text(source, "sample.rs"))

    def test_comments_and_plural_envelope_pass(self) -> None:
        source = (
            "// Never import codewhale_tui or define CommandContext here.\n"
            "pub struct CommandContexts<'a> { marker: &'a str }\n"
        )
        self.assertEqual(mod.check_contract_source_text(source, "safe.rs"), [])


class TreeModeTests(unittest.TestCase):
    RULE = mod.BoundaryRule(
        "codewhale-runtime",
        "tree",
        ("codewhale-tui", "ratatui", "crossterm"),
        "the runtime must stay UI-free",
    )

    def test_parse_cargo_tree_keeps_names(self) -> None:
        text = "codewhale-runtime v0.10.0 (/x)\nanyhow v1.0.100\nratatui v0.30.2 (*)\n"
        self.assertEqual(mod.parse_cargo_tree(text), {"codewhale-runtime", "anyhow", "ratatui"})

    def test_clean_tree_passes(self) -> None:
        self.assertEqual(mod.check_tree_packages(self.RULE, {"anyhow", "serde"}), [])

    def test_runtime_rule_is_tree_mode(self) -> None:
        rule = next(r for r in mod.BOUNDARY_RULES if r.package == "codewhale-runtime")
        self.assertEqual(rule.mode, "tree")
        self.assertIn("ratatui", rule.forbidden_packages)
        self.assertIn("crossterm", rule.forbidden_packages)

    def test_runtime_source_scan(self) -> None:
        self.assertEqual(
            mod.check_runtime_source_text("// ratatui::Frame is not used here\nfn f() {}\n", "a.rs"),
            [],
        )
        for source in (
            "use crossterm::terminal;",
            "let _ = ratatui::style::Color::Reset;",
            'const X: &str = include_str!("../../tui/assets/x.json");',
        ):
            with self.subTest(source=source):
                self.assertTrue(mod.check_runtime_source_text(source, "a.rs"))

    def test_ui_library_in_tree_fails(self) -> None:
        violations = mod.check_tree_packages(self.RULE, {"anyhow", "ratatui", "crossterm"})
        self.assertEqual(len(violations), 2)
        self.assertIn("ratatui", str(violations[1]) + str(violations[0]))


def write_tree(root: Path, files: dict[str, str]) -> None:
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


class RatchetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.graph = mod.load_runtime_ratchet()

    def report(self, tui: dict[str, str], runtime: dict[str, str] | None = None):
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp, "tui"), tui)
            write_tree(Path(tmp, "runtime"), runtime or {})
            return self.graph.build_report(Path(tmp, "tui"), Path(tmp, "runtime"))

    def test_counts_grouped_imports_and_masks_comments_and_strings(self) -> None:
        report = self.report({
            "lib.rs": "mod core; mod tui;\n",
            "core.rs": (
                "use crate::{tui::App, tui::views::{A, B}};\n"
                "// crate::tui::ignored\n"
                "const S: &str = \"crate::tui::ignored\";\n"
                "fn f() { crate::tui::draw(); }\n"
                "#[cfg(test)]\nmod tests { fn t() { crate::tui::fixture(); } }\n"
            ),
            "tui.rs": "",
        })
        self.assertEqual(report.counts["prod"], {"core|tui": 4})
        self.assertEqual(report.counts["test"], {"core|tui": 1})

    def test_ui_library_and_late_edges_are_counted(self) -> None:
        report = self.report({
            "lib.rs": "mod core; mod exec_agent;\n",
            "core.rs": "fn f() { crossterm::terminal::enable_raw_mode(); }\n"
            "#[cfg(test)]\nmod tests { fn t() { crate::exec_agent::run(); } }\n",
            "exec_agent.rs": "",
        })
        self.assertEqual(report.counts["uilib"], {"core|crossterm": 1})
        self.assertEqual(report.counts["late"], {"core|exec_agent": 1})

    def test_runtime_crate_modules_join_the_closure(self) -> None:
        report = self.report(
            {"lib.rs": "mod core;\nuse codewhale_runtime::{elapsed};\n", "core.rs": ""},
            {"lib.rs": "pub mod elapsed;\n", "elapsed.rs": "fn f() { ratatui::x(); }\n"},
        )
        self.assertIn("codewhale_runtime::elapsed", report.closure)
        self.assertEqual(report.counts["uilib"], {"codewhale_runtime::elapsed|ratatui": 1})

    def test_rise_and_unrecorded_drop_both_fail(self) -> None:
        report = self.report({
            "lib.rs": "mod core; mod tui;\n",
            "core.rs": "fn f() { crate::tui::a(); crate::tui::b(); }\n",
            "tui.rs": "",
        })
        rises, drops = self.graph.compare({"counts": {"prod": {"core|tui": 1}}}, report)
        self.assertTrue(rises and rises[0].startswith("prod core|tui: 1 -> 2"))
        self.assertEqual(drops, [])
        rises, drops = self.graph.compare({"counts": {"prod": {"core|tui": 3}}}, report)
        self.assertEqual((rises, drops), ([], ["prod core|tui: 3 -> 2"]))

    def test_super_chains_that_reach_the_root_count(self) -> None:
        report = self.report({
            "lib.rs": "mod core; mod tui;\n",
            "core/mod.rs": (
                "mod inner;\n"
                "fn f() { super::tui::a(); }\n"
                "pub(in super::super) fn g() {}\n"
                "#[cfg(test)]\nmod tests { use super::super::{tui::B, x}; use super::*; }\n"
            ),
            # Depth 2: two `super`s reach the root, one stays inside `core`.
            "core/inner.rs": "use super::super::tui as ui;\nfn f() { super::tui::local(); }\n",
            "tui.rs": "",
        })
        self.assertEqual(report.counts["prod"], {"core|tui": 2})
        self.assertEqual(report.counts["test"], {"core|tui": 1})

    def test_hand_raised_baseline_fails_against_the_base(self) -> None:
        previous = {"counts": {"prod": {"tools|tui": 8}}}
        self.assertEqual(self.graph.baseline_raises(previous, previous), [])
        self.assertEqual(
            self.graph.baseline_raises(previous, {"counts": {"prod": {"tools|tui": 7}}}), []
        )
        self.assertEqual(
            self.graph.baseline_raises(previous, {"counts": {"prod": {"tools|tui": 9}}}),
            ["prod tools|tui: 8 -> 9"],
        )
        self.assertEqual(
            self.graph.baseline_raises(previous, {"counts": {"test": {"core|tui": 1}}}),
            ["test core|tui: 0 -> 1 (new pair)"],
        )

    def test_same_scope_split_preserves_original_test_and_production_counts(self) -> None:
        original = self.report({
            "lib.rs": 'mod client; mod tui;',
            "client.rs": 'fn run() { crate::tui::real(); } #[cfg(test)] mod suite { fn probe() { crate::tui::one(); crate::tui::two(); } }',
            "tui.rs": '',
        })
        split = self.report({
            "lib.rs": 'mod client; mod tui;',
            "client.rs": 'fn run() { crate::tui::real(); } #[cfg(test)] mod suite { include!("client/first.rs"); }',
            "client/first.rs": 'fn probe() { crate::tui::one(); crate::tui::two(); }',
            "tui.rs": '',
        })
        self.assertEqual(split.counts, original.counts)
        self.assertEqual(split.closure, original.closure)
        self.assertEqual(split.counts["prod"], {"client|tui": 1})
        self.assertEqual(split.counts["test"], {"client|tui": 2})

    def test_literal_path_and_recursive_raw_includes_reuse_test_scope(self) -> None:
        report = self.report({
            "lib.rs": 'mod session_manager; mod tui;',
            "session_manager.rs": '#[cfg(test)] #[path="pieces/suite.rs"] mod verification;',
            "pieces/suite.rs": 'include!(r"first.rs");',
            "pieces/first.rs": 'include!("second.rs");',
            "pieces/second.rs": 'fn probe() { crate::tui::one(); }',
            "tui.rs": '',
        })
        # The graph knows these are test-only even though none has a test name.
        crate = self.graph.load_crate("fixture", Path("/not-a-crate"))
        self.assertEqual(report.counts["prod"], {})
        # Independent top-level pieces are outside the runtime closure; inspect
        # the actual graph for scope below rather than manufacturing a seed.
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "src"
            write_tree(source, {
                "lib.rs": 'mod client;',
                "client.rs": '#[cfg(test)] #[path="pieces/suite.rs"] mod verification;',
                "pieces/suite.rs": 'include!(r"first.rs");',
                "pieces/first.rs": 'include!("second.rs");',
                "pieces/second.rs": 'fn probe() {}',
            })
            crate = self.graph.load_crate("fixture", source)
            exact, prefixes = self.graph.test_file_set(crate)
            self.assertEqual(exact, {"pieces/suite.rs", "pieces/first.rs", "pieces/second.rs"})
            self.assertEqual(prefixes, set())

    def test_shared_production_includes_override_test_filename_and_descendants(self) -> None:
        report = self.report({
            "lib.rs": 'mod client; mod tui;',
            "client.rs": 'include!("client/shared_tests.rs"); #[cfg(test)] mod suite { include!("client/shared_tests.rs"); }',
            "client/shared_tests.rs": 'fn real() { crate::tui::one(); } include!("child_test.rs");',
            "client/child_test.rs": 'fn real_child() { crate::tui::two(); }',
            "tui.rs": '',
        })
        self.assertEqual(report.counts["prod"], {"client|tui": 2})
        self.assertEqual(report.counts["test"], {})

    def test_cfg_test_module_does_not_exempt_a_guessed_directory_prefix(self) -> None:
        report = self.report({
            "lib.rs": 'mod client; mod tui;',
            "client.rs": '#[cfg(test)] mod suite; #[path="client/suite/shipping.rs"] mod shipping;',
            "client/suite.rs": 'fn test_only() {}',
            "client/suite/shipping.rs": 'fn real() { crate::tui::one(); }',
            "tui.rs": '',
        })
        self.assertEqual(report.counts["prod"], {"client|tui": 1})
        self.assertEqual(report.counts["test"], {})

    def test_comment_dynamic_and_mixed_cfg_cannot_exempt_production_fragments(self) -> None:
        report = self.report({
            "lib.rs": 'mod client; mod tui;',
            "client.rs": '// #[cfg(test)] mod fake { include!("client/part.rs"); }\n'
                '#[cfg(test)] mod suite { include!(concat!("client/", "part.rs")); }'
                '#[cfg(any(test, feature="shipping"))] mod shipped { include!("client/part.rs"); }',
            "client/part.rs": 'fn real() { crate::tui::one(); }',
            "tui.rs": '',
        })
        self.assertEqual(report.counts["prod"], {"client|tui": 1})
        self.assertEqual(report.counts["test"], {})

    def test_real_custom_cargo_root_outside_src_keeps_test_named_child_production(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            crate_root = Path(tmp) / "demo"
            source = crate_root / "src"
            write_tree(crate_root, {
                "Cargo.toml": '[package]\nname="demo"\nversion="0.1.0"\n[lib]\npath="front/entry.rs"\n',
                "src/lib.rs": '#[cfg(test)] mod suite { include!("../front/entry.rs"); }',
                "front/entry.rs": 'include!("../src/entry_tests.rs");',
                "src/entry_tests.rs": 'fn real() { crate::tui::one(); }',
            })
            crate = self.graph.load_crate("fixture", source)
            exact, prefixes = self.graph.test_file_set(crate)
            self.assertFalse(self.graph.file_is_test("entry_tests.rs", exact, prefixes, crate.production_files))
            self.assertIn("entry_tests.rs", crate.production_files)

    def test_scan_roots_do_not_consult_a_neighboring_crates_test_edges(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            left = Path(tmp) / "left"
            right = Path(tmp) / "right"
            write_tree(left, {"lib.rs": 'mod client;', "client.rs": 'fn real() {}'})
            write_tree(right, {"lib.rs": '#[cfg(test)] mod suite { include!("../left/client.rs"); }'})
            crate = self.graph.load_crate("fixture", left)
            exact, _prefixes = self.graph.test_file_set(crate)
            self.assertNotIn("client.rs", exact)

    def test_checked_in_baseline_holds(self) -> None:
        self.assertEqual(self.graph.check(), [])


if __name__ == "__main__":
    unittest.main()
