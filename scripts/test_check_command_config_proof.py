"""Executable negative controls for the complete config-group extraction proof.

FEAT-028 proves the WHOLE config group, so the controls must reject a
selected-leaf proof, a cfg(test)-only mount, a missing root or helper, a host
import anywhere in the mounted closure (including `config.rs`), a dev-dependency
swap, and forbidden normal graph edges.
"""
import importlib.util
from pathlib import Path
import re
import tempfile
import unittest

path = Path(__file__).with_name("check-command-config-proof.py")
spec = importlib.util.spec_from_file_location("config_proof", path)
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class ConfigProofTests(unittest.TestCase):
    def fixture(self, root):
        for relative in proof.REQUIRED | {proof.PROOF, proof.MANIFEST}:
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text((proof.ROOT / relative).read_text())

    def test_actual_complete_group_is_included(self):
        self.assertEqual(proof.violations(proof.ROOT), [])

    def test_selected_leaf_stub_and_test_only_root_are_rejected(self):
        for source in [
            'pub mod config_group {}',
            '#[path = "config.rs"] pub mod config_group;',
            '#[cfg(test)]\n' + (proof.ROOT / proof.PROOF).read_text(),
        ]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                (root / proof.PROOF).write_text(source)
                self.assertTrue(proof.violations(root))

    def test_omitted_root_or_helper_is_rejected(self):
        group_dir = "crates/tui/src/commands/groups/config"
        for name in ["config", "status", "policy_messages", "money", "tests"]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                if name == "config":
                    # The group root must include its children.
                    group_root = root / proof.ROOTS[0]
                    group_root.write_text(
                        group_root.read_text().replace("pub mod config;\n", ""))
                elif name == "money":
                    policy = root / group_dir / "policy.rs"
                    policy.write_text(policy.read_text().replace(
                        "use codewhale_command_contract::money;", ""))
                else:
                    policy = root / group_dir / "policy.rs"
                    policy.write_text(re.sub(
                        r'#\[path = "[^"]+"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod ' + name + r';',
                        '', policy.read_text()))
                self.assertTrue(proof.violations(root))

    def test_host_import_in_any_mounted_file_is_rejected(self):
        for source in [
            'use crate::tui::app::App;',
            'use codewhale_config::Config;',
            'use crate::commands::CommandResult;',
            'use std::fs;',
            'use codewhale_secrets::Store;',
        ]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                group_root = root / proof.ROOTS[0]
                group_root.write_text(group_root.read_text() + "\n" + source + "\n")
                self.assertTrue(proof.violations(root))

    def test_host_import_in_unmounted_group_file_is_rejected(self):
        # A new file in the group directory that is not reachable from the
        # mounted root must still be rejected by the whole-directory scan, which
        # is the selected-source gap FEAT-026 corrected.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            stray = root / "crates/tui/src/commands/groups/config/stray_helper.rs"
            stray.write_text("use crate::tui::app::App;\n")
            self.assertTrue(proof.violations(root))

    def test_shapes_cannot_move_to_dev_dependencies(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            manifest = root / proof.MANIFEST
            manifest.write_text(
                manifest.read_text().replace("[dev-dependencies]", "[build-dependencies]")
                .replace("[dependencies]", "[dev-dependencies]"))
            self.assertTrue(proof.violations(root))

    def test_direct_and_transitive_runtime_or_storage_edges_are_rejected(self):
        allowed = ("codewhale-portable-config v0.10.2\ncodewhale-command-contract v0.10.2\n"
                   "codewhale-protocol v0.10.2\ncodewhale-sanitize v0.10.2\nserde v1.0.0\n")
        self.assertEqual(proof.graph_violations(allowed), [])
        for package in ["codewhale-tui", "codewhale-config", "codewhale-core", "codewhale-runtime",
                        "codewhale-execpolicy", "codewhale-secrets", "codewhale-state",
                        "codewhale-localization", "tokio", "reqwest", "rusqlite", "keyring",
                        "ratatui", "crossterm"]:
            with self.subTest(package=package):
                self.assertEqual(
                    len(proof.graph_violations(allowed + f"{package} v1.0.0\n")), 1)


if __name__ == '__main__':
    unittest.main(verbosity=2)
