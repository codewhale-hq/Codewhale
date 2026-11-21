#!/usr/bin/env python3
"""Tests for scripts/check-docs-drift.py."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

CHECKER = Path(__file__).resolve().with_name("check-docs-drift.py")

REGISTRY = """\
[[feature]]
id = "skills"
name = "Skill plugins"
summary = "Trusted instruction packages."
status = "stable"
flag = "skills"
docs = "docs/SKILLS.md"
owner = "crates/skills/src"
"""


class DocsDriftTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Docs Drift Test")
        self.git("config", "user.email", "docs-drift@example.com")
        self.write("docs/features.toml", REGISTRY)
        self.write("docs/SKILLS.md", "# Skills\n\nThe `skills` flag enables skill plugins.\n")
        self.write("crates/skills/src/lib.rs", "pub fn skills() {}\n")
        self.commit("chore: seed")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def git(self, *args: str) -> str:
        return subprocess.run(
            ["git", *args], cwd=self.repo, check=True, capture_output=True, text=True
        ).stdout

    def write(self, rel: str, text: str) -> None:
        path = self.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def commit(self, message: str, body: str | None = None) -> None:
        self.git("add", "-A")
        args = ["commit", "-qm", message]
        if body:
            args.extend(["-m", body])
        self.git(*args)

    def head(self) -> str:
        return self.git("rev-parse", "HEAD").strip()

    def run_checker(self, base: str) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["CODEWHALE_REPO_ROOT"] = str(self.repo)
        return subprocess.run(
            [sys.executable, str(CHECKER), base, "HEAD"],
            cwd=self.repo,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_owner_change_without_summary_fails(self) -> None:
        base = self.head()
        self.write("crates/skills/src/lib.rs", "pub fn skills() { let _ = 1; }\n")
        self.commit("feat(skills): tighten the loader")
        result = self.run_checker(base)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("changed but the summary did not", result.stderr)

    def test_summary_update_passes(self) -> None:
        base = self.head()
        self.write("crates/skills/src/lib.rs", "pub fn skills() { let _ = 1; }\n")
        self.write(
            "docs/features.toml",
            REGISTRY.replace(
                "Trusted instruction packages.",
                "Trusted instruction packages, with a tighter loader.",
            ),
        )
        self.commit("feat(skills): tighten the loader")
        result = self.run_checker(base)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_no_behaviour_change_escape_passes(self) -> None:
        base = self.head()
        self.write("crates/skills/src/lib.rs", "pub fn skills() { let _ = 1; }\n")
        self.commit(
            "refactor(skills): move the loader",
            "No-behaviour-change: file move only",
        )
        result = self.run_checker(base)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_docs_page_missing_the_flag_fails(self) -> None:
        base = self.head()
        self.write("crates/skills/src/lib.rs", "pub fn skills() { let _ = 1; }\n")
        self.write(
            "docs/features.toml",
            REGISTRY.replace(
                "Trusted instruction packages.",
                "Trusted instruction packages, with a tighter loader.",
            ),
        )
        self.write("docs/SKILLS.md", "# Skills\n\nNo flag named here.\n")
        self.commit("feat(skills): tighten the loader")
        result = self.run_checker(base)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("does not mention skills", result.stderr)


if __name__ == "__main__":
    unittest.main()
