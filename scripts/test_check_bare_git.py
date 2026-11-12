#!/usr/bin/env python3
"""Hermetic tests for the bare-git gate."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check-bare-git.py"
SPEC = importlib.util.spec_from_file_location("bare_git", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


def scan(files: dict[str, str], allowlist: dict | None = None) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        root = base / "crates"
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return mod.find_violations(root, base, allowlist or {})


class BareGitTests(unittest.TestCase):
    def test_product_spawn_fails(self) -> None:
        hits = scan({"a/src/lib.rs": 'fn f() {\n    Command::new("git");\n}\n'})
        self.assertEqual(hits, ["crates/a/src/lib.rs:2"])

    def test_qualified_and_exe_spellings_fail(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": 'fn f() {\n    std::process::Command::new("git");\n    Command::new("git.exe");\n}\n'
            }
        )
        self.assertEqual(len(hits), 2)

    def test_cfg_test_module_and_test_files_pass(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": '#[cfg(test)]\nmod tests;\n#[cfg(test)]\n#[path = "x/delivery_tests.rs"]\nmod delivery;\n#[cfg(test)]\nmod inline {\n    fn f() {\n        Command::new("git");\n    }\n}\n',
                "a/src/tests.rs": 'mod deeper;\nfn f() { Command::new("git"); }\n',
                "a/src/tests/deeper.rs": 'fn f() { Command::new("git"); }\n',
                "a/src/x/delivery_tests.rs": 'fn f() { Command::new("git"); }\n',
                "a/tests/it.rs": 'fn f() { Command::new("git"); }\n',
                "a/src/inner.rs": '#![cfg(test)]\nfn f() { Command::new("git"); }\n',
            }
        )
        self.assertEqual(hits, [])

    def test_file_names_do_not_exempt_product_code(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": "pub mod test_runner;\npub mod test_support;\nmod tests;\n",
                "a/src/test_runner.rs": 'fn f() {\n    Command::new("git");\n}\n',
                "a/src/test_support.rs": 'pub fn f() {\n    Command::new("git");\n}\n',
                "a/src/tests.rs": 'fn f() {\n    Command::new("git");\n}\n',
            }
        )
        self.assertEqual(
            hits,
            [
                "crates/a/src/test_runner.rs:2",
                "crates/a/src/test_support.rs:2",
                "crates/a/src/tests.rs:2",
            ],
        )

    def test_a_cfg_test_module_declaration_exempts_only_that_file(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": '#[cfg(test)]\nmod test_support;\nmod other;\n',
                "a/src/test_support.rs": 'fn f() { Command::new("git"); }\n',
                "a/src/other.rs": 'fn f() {\n    Command::new("git");\n}\n',
            }
        )
        self.assertEqual(hits, ["crates/a/src/other.rs:2"])

    def test_include_exempts_only_inside_cfg_test(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": '#[cfg(test)]\nmod tests {\n    include!("cases.rs");\n}\ninclude!("shared.rs");\n',
                "a/src/cases.rs": 'fn f() { Command::new("git"); }\n',
                "a/src/shared.rs": 'fn f() {\n    Command::new("git");\n}\n',
            }
        )
        self.assertEqual(hits, ["crates/a/src/shared.rs:2"])

    def test_one_line_cfg_test_items_do_not_swallow_product_code(self) -> None:
        for gate in (
            "#[cfg(test)] impl Foo for Bar {}",
            "#[cfg(test)]\nimpl Foo for Bar {}",
            "#[cfg(test)]\nuse std::process::Command as Unused;",
            "#[cfg(test)]\nstatic TABLE: [u8; 4] = [0; 4];",
        ):
            hits = scan(
                {
                    "a/src/lib.rs": f'mod m {{\n    {gate}\n    fn f() {{\n        Command::new("git");\n    }}\n}}\n'
                }
            )
            self.assertEqual(len(hits), 1, gate)

    def test_cfg_test_on_a_field_or_expression_does_not_swallow_product_code(self) -> None:
        text = (
            "struct S {\n    #[cfg(test)]\n    seen: u8,\n}\n"
            "fn make() -> S {\n    S {\n        #[cfg(test)]\n        seen: 0,\n    }\n}\n"
            'fn f() {\n    Command::new("git");\n}\n'
        )
        self.assertEqual(scan({"a/src/lib.rs": text}), ["crates/a/src/lib.rs:12"])

    def test_cfg_test_block_ignores_braces_in_literals(self) -> None:
        text = (
            '#[cfg(test)]\nmod tests {\n    fn f() {\n        let _ = "{";\n        let _ = \'{\';\n        // {\n        Command::new("git");\n    }\n}\n'
            'fn g() {\n    Command::new("git");\n}\n'
        )
        self.assertEqual(scan({"a/src/lib.rs": text}), ["crates/a/src/lib.rs:11"])

    def test_cfg_not_test_is_product_code(self) -> None:
        hits = scan(
            {"a/src/lib.rs": '#[cfg(not(test))]\nfn f() {\n    Command::new("git");\n}\n'}
        )
        self.assertEqual(hits, ["crates/a/src/lib.rs:3"])

    def test_renamed_command_import_fails(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": 'use std::process::Command as Spawn;\nfn f() {\n    Spawn::new("git");\n}\n',
                "a/src/b.rs": 'use tokio::process::{Child, Command as Run};\nfn f() {\n    Run::new("git.exe");\n}\n',
            }
        )
        self.assertEqual(hits, ["crates/a/src/b.rs:3", "crates/a/src/lib.rs:3"])

    def test_string_literals_do_not_count_as_spawns(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": 'const SCRIPT: &str = r#"Command::new("git")"#;\nfn f() { let _ = "Command::new(\\"git\\")"; }\n'
            }
        )
        self.assertEqual(hits, [])

    def test_comments_and_other_programs_pass(self) -> None:
        hits = scan(
            {
                "a/src/lib.rs": '// Command::new("git")\nfn f() { Command::new("tar"); Command::new(path); }\n'
            }
        )
        self.assertEqual(hits, [])

    def test_allowlist_matches_only_its_exact_count(self) -> None:
        files = {"a/src/lib.rs": 'fn f() {\n    Command::new("git");\n}\n'}
        allowlist = {"crates/a/src/lib.rs": (1, "reason")}
        self.assertEqual(scan(files, allowlist), [])
        files["a/src/lib.rs"] += 'fn g() {\n    Command::new("git");\n}\n'
        self.assertEqual(len(scan(files, allowlist)), 2)

    def test_repository_has_no_violations(self) -> None:
        self.assertEqual(mod.find_violations(), [])


if __name__ == "__main__":
    unittest.main()
