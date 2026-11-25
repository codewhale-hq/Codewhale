#!/usr/bin/env python3
"""Hermetic tests for scripts/check-dead-code-budget.py.

The property under test is the one #6241 reported missing: the gate must
ratchet on dead-code *suppression*, not on one spelling of it. Rewriting
`#[allow(dead_code)]` as `#[expect(dead_code)]` removes no dead code, so it
must not lower the headline number.
"""

from __future__ import annotations

import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check-dead-code-budget.py"

SPEC = importlib.util.spec_from_file_location("check_dead_code_budget", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class DeadCodeBudgetTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        self.crates = root / "crates"
        (self.crates / "demo" / "src").mkdir(parents=True)
        self.budget = root / "scripts" / "dead-code-budget.json"
        self.budget.parent.mkdir(parents=True)
        self._saved = (mod.REPO_ROOT, mod.CRATES_DIR, mod.BUDGET_PATH)
        mod.REPO_ROOT, mod.CRATES_DIR, mod.BUDGET_PATH = root, self.crates, self.budget

    def tearDown(self) -> None:
        mod.REPO_ROOT, mod.CRATES_DIR, mod.BUDGET_PATH = self._saved
        self._tmp.cleanup()

    def write_source(self, body: str) -> None:
        (self.crates / "demo" / "src" / "lib.rs").write_text(body, encoding="utf-8")

    def write_budget(self, total: int) -> None:
        self.budget.write_text(json.dumps({"total": total}) + "\n", encoding="utf-8")

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        saved = sys.argv
        sys.argv = ["check-dead-code-budget.py", *argv]
        try:
            with redirect_stdout(out), redirect_stderr(err):
                code = mod.main()
        finally:
            sys.argv = saved
        return code, out.getvalue(), err.getvalue()

    def test_expect_spelling_is_counted(self) -> None:
        """The blind spot itself: `expect(dead_code)` is a suppression too."""
        self.write_source("#[expect(dead_code)]\nfn a() {}\n")
        allow_total, expect_total, _ = mod.measure()
        self.assertEqual(allow_total, 0)
        self.assertEqual(expect_total, 1)

    def test_dead_code_in_any_lint_list_position_is_counted(self) -> None:
        self.write_source(
            "#[allow(async_fn_in_trait, dead_code)]\nfn a() {}\n"
            "#[allow(\n    unused_imports,\n    dead_code,\n)]\nfn b() {}\n"
            "#[expect(clippy::too_many_arguments, dead_code)]\nfn c() {}\n"
        )
        allow_total, expect_total, _ = mod.measure()
        self.assertEqual((allow_total, expect_total), (2, 1))

    def test_comment_prose_is_not_counted(self) -> None:
        self.write_source(
            "// the old #[allow(dead_code)] was replaced by expect(dead_code)\n"
            "    /// see allow(dead_code) here\n"
            "//! and expect(dead_code)\nfn a() {}\n"
        )
        self.assertEqual(mod.measure()[:2], (0, 0))

    def test_rewriting_allow_as_expect_does_not_lower_the_total(self) -> None:
        """#6241's exact regression: a spelling change is not progress."""
        self.write_source("#[allow(dead_code)]\nfn a() {}\n#[allow(dead_code)]\nfn b() {}\n")
        before = sum(mod.measure()[:2])
        self.write_source("#[expect(dead_code)]\nfn a() {}\n#[expect(dead_code)]\nfn b() {}\n")
        after = sum(mod.measure()[:2])
        self.assertEqual(
            before,
            after,
            "converting allow->expect removes no dead code and must not move the ratchet",
        )

    def test_gate_fails_when_expect_pushes_past_the_ceiling(self) -> None:
        self.write_source("#[allow(dead_code)]\nfn a() {}\n#[expect(dead_code)]\nfn b() {}\n")
        self.write_budget(1)
        code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("allow=1", err)
        self.assertIn("expect=1", err)

    def test_gate_passes_at_the_combined_ceiling(self) -> None:
        self.write_source("#[allow(dead_code)]\nfn a() {}\n#[expect(dead_code)]\nfn b() {}\n")
        self.write_budget(2)
        code, out, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("PASS", out)

    def test_update_records_both_spellings_separately(self) -> None:
        self.write_source("#[allow(dead_code)]\nfn a() {}\n#[expect(dead_code)]\nfn b() {}\n")
        self.write_budget(0)
        code, _, _ = self.run_main("--update")
        self.assertEqual(code, 0)
        payload = json.loads(self.budget.read_text(encoding="utf-8"))
        self.assertEqual(payload["total"], 2)
        self.assertEqual(payload["allow_total"], 1)
        self.assertEqual(payload["expect_total"], 1)
        self.assertEqual(payload["per_crate"]["demo"], {"allow": 1, "expect": 1})

    def test_legacy_per_crate_shape_still_renders(self) -> None:
        """A budget written before #6241 mapped each crate to a bare int."""
        self.write_source("#[allow(dead_code)]\nfn a() {}\n#[expect(dead_code)]\nfn b() {}\n")
        self.budget.write_text(
            json.dumps({"total": 1, "per_crate": {"demo": 1}}) + "\n", encoding="utf-8"
        )
        code, _, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("budget 1", err)


if __name__ == "__main__":
    unittest.main()
