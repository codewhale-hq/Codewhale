import importlib.util
from pathlib import Path
import re
import tempfile
import unittest

path = Path(__file__).with_name("check-command-core-navigation-proof.py")
spec = importlib.util.spec_from_file_location("navigation_proof", path)
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class NavigationProofTests(unittest.TestCase):
    def fixture(self, root):
        for relative in proof.REQUIRED | {proof.PROOF, proof.MANIFEST, proof.HOST}:
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text((proof.ROOT / relative).read_text())

    def test_actual_complete_group_is_included(self):
        self.assertEqual(proof.violations(proof.ROOT), [])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            host = root / proof.HOST
            source = host.read_text()
            unrelated = 'Box::new(FunctionCommand::new(extra::ExtraCmd::info(), extra::ExtraCmd::execute)),\n'
            host.write_text(source.replace(
                'crate::commands::core_navigation_host::registration::<0>(),',
                unrelated + 'crate::commands::core_navigation_host::registration::<0>(),',
            ))
            self.assertEqual(proof.violations(root), [])

    def test_selected_leaf_stub_and_test_only_root_are_rejected(self):
        actual = (proof.ROOT / proof.PROOF).read_text()
        for source in [
            "pub mod navigation {}",
            actual.replace("navigation.rs", "hotbar.rs"),
            "#[cfg(test)]\n" + actual,
            actual + "struct App;\n",
        ]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                (root / proof.PROOF).write_text(source)
                self.assertTrue(proof.violations(root))
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            source = root / proof.ROOTS[0]
            source.write_text("#[cfg(test)]\n" + source.read_text())
            self.assertTrue(any("conditional" in error for error in proof.violations(root)))

    def test_omitted_root_or_helper_is_rejected(self):
        for case in ["anchor", "voice", "inventory", "host_order", "host_duplicate", "host_reorder", "metadata", "include"]:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                source = root / proof.ROOTS[0]
                if case in {"anchor", "voice"}:
                    source.write_text(re.sub(r'#\[path = "' + case + r'\.rs"\]\s*pub mod ' + case + r';', '', source.read_text()))
                elif case == "inventory":
                    source.write_text(source.read_text().replace("(anchor::AnchorCmd::info(), anchor::AnchorCmd::handler()),", ""))
                elif case == "host_order":
                    host = root / proof.HOST
                    host.write_text(host.read_text().replace("registration::<0>()", "registration::<99>()"))
                elif case in {"host_duplicate", "host_reorder"}:
                    host = root / proof.HOST
                    text = host.read_text()
                    if case == "host_duplicate":
                        text = text.replace('registration::<1>()', 'registration::<0>()')
                    else:
                        text = text.replace('registration::<0>()', 'registration::<TEMP>()')
                        text = text.replace('registration::<1>()', 'registration::<0>()')
                        text = text.replace('registration::<TEMP>()', 'registration::<1>()')
                    host.write_text(text)
                    self.assertTrue(any('relative order' in error for error in proof.violations(root)))
                elif case == "include":
                    source.write_text(source.read_text() + '\ninclude!("untracked-helper.rs");\n')
                else:
                    leaf = root / proof.GROUP / "anchor.rs"
                    leaf.write_text(leaf.read_text().replace('name: "anchor"', 'name: "wrong"'))
                self.assertTrue(proof.violations(root))

    def test_host_import_in_any_mounted_file_is_rejected(self):
        for name in sorted(proof.REQUIRED):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                self.fixture(root)
                leaf = root / name
                leaf.write_text(leaf.read_text() + "\nuse crate::tui::app::App;\n")
                self.assertTrue(proof.violations(root))

    def test_host_import_in_unmounted_group_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            source = root / proof.ROOTS[0]
            source.write_text(re.sub(r'#\[path = "voice\.rs"\]\s*pub mod voice;', '', source.read_text()))
            leaf = root / proof.GROUP / "voice.rs"
            leaf.write_text(leaf.read_text() + "\nuse codewhale_runtime::Runtime;\n")
            errors = proof.violations(root)
            self.assertTrue(any("host/service in declared movable source" in error for error in errors))

    def test_shapes_cannot_move_to_dev_dependencies(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            manifest = root / proof.MANIFEST
            manifest.write_text(manifest.read_text().replace("[dependencies]", "[dev-dependencies]"))
            self.assertTrue(proof.violations(root))

    def test_direct_and_transitive_runtime_or_storage_edges_are_rejected(self):
        allowed = "codewhale-portable-core-navigation v0.10.2\ncodewhale-command-contract v0.10.2\ncodewhale-protocol v0.10.2\nserde v1.0.0\n"
        self.assertEqual(proof.graph_violations(allowed), [])
        for package in ["codewhale-tui", "codewhale-core", "codewhale-runtime", "codewhale-config",
                        "codewhale-secrets", "codewhale-state", "codewhale-localization", "tokio",
                        "reqwest", "rusqlite", "keyring", "ratatui", "crossterm", "unreviewed-helper"]:
            with self.subTest(package=package):
                self.assertEqual(len(proof.graph_violations(allowed + f"{package} v1.0.0\n")), 1)
        self.assertTrue(proof.graph_violations("serde v1.0.0\n"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
