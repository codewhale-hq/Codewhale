#!/usr/bin/env python3
"""Hermetic tests for migration gates and manual workspace qualification.

Verifies `.github/workflows/ci.yml` keeps both migration checker commands
(self-tests then live scan) under the same `heavy` condition as the existing
command-contract boundary step, and that the boundary step itself remains
intact. Exercise the GH6698 workflow shell with a fake Cargo and the real
HOME-isolation wrapper so these wiring tests never compile or run Rust.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CI_PATH = ROOT / ".github" / "workflows" / "ci.yml"
SHARED_PROCESS_MODE = (
    "github.event_name == 'workflow_dispatch' && "
    "inputs.workspace_test_mode == 'shared-process-twice'"
)
ISSUE_6698_TESTS = (
    "remote_control::tests::classic_recovery_uses_persisted_seq_floor_and_ignores_older_terminal",
    "remote_control::tests::actual_start_reclaims_runtime_chat_writer_before_worker_spawn",
    "remote_control::tests::separate_predispatch_crashes_on_one_run_get_distinct_recovery_turn_ids",
    "runtime_api::tests::events_endpoint_respects_since_seq_cursor",
    "runtime_threads::tests::approval_required_awaits_external_decision_allow",
    "runtime_threads::tests::approval_remember_grants_tool_class_without_changing_posture",
    "tools::subagent::budget_handback_tests::budget_handback_inflight_wall_timeout_persists_unreported_usage",
    "tools::subagent::tests::child_permission_gate::wall_deadline_ends_pending_wait_with_receipt",
    "tools::subagent::tests::resume_keeps_recorded_reasoning_in_manifest_and_request_after_parent_changes",
)


def load_ci() -> str:
    return CI_PATH.read_text(encoding="utf-8")


def boundary_step_block(ci: str) -> str:
    """Extract the 'Check command-contract prototype boundary' step block."""
    marker = "Check command-contract prototype boundary"
    start = ci.index(marker)
    step_start = ci.rindex("- name:", 0, start)
    # The step ends at the next "- name:" after the marker.
    next_step = ci.index("- name:", start + len(marker))
    return ci[step_start:next_step]


def migration_step_block(ci: str) -> str:
    marker = "Check command migration manifest"
    start = ci.index(marker)
    step_start = ci.rindex("- name:", 0, start)
    next_step = ci.index("- name:", start + len(marker))
    return ci[step_start:next_step]


def named_step(ci: str, name: str) -> str:
    start = ci.index(f"      - name: {name}\n")
    return ci[start:ci.index("      - name:", start + 1)]


def run_shared_process_fixture(mode: str = "pass", expected_sha: str | None = None) -> dict:
    """Execute the actual workflow shell and HOME wrapper, never real Cargo."""
    block = named_step(load_ci(), "Shared-process workspace qualification")
    script = textwrap.dedent(block.split("        run: |\n", 1)[1])
    with tempfile.TemporaryDirectory(prefix="shared-process-wiring-") as temp:
        fixture = Path(temp)
        repo = fixture / "repo"
        binary = fixture / "bin"
        (repo / "scripts").mkdir(parents=True)
        binary.mkdir()
        shutil.copyfile(
            ROOT / "scripts/with-hermetic-test-home.sh",
            repo / "scripts/with-hermetic-test-home.sh",
        )
        trace = fixture / "cargo-calls.jsonl"
        outside = fixture / "outside"
        outside.mkdir()
        outside_payload = outside / "payload.txt"
        outside_payload.write_text("outside fixture must remain unchanged", encoding="utf-8")
        outside_payload.chmod(0o400)
        outside.chmod(0o500)
        fake_cargo = f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
if sys.argv[1:] == ['-V']:
    print('cargo fixture (no compiler invoked)')
    raise SystemExit(0)
trace = Path(os.environ['FIXTURE_TRACE'])
calls = trace.read_text().splitlines() if trace.exists() else []
call = {{'args': sys.argv[1:], 'home': os.environ['HOME'],
        'tmpdir': os.environ['TMPDIR'], 'threads': os.environ.get('RUST_TEST_THREADS'),
        'stack': os.environ['RUST_MIN_STACK'],
        'credential_present': bool(os.environ.get('OPENAI_API_KEY'))}}
with trace.open('a') as out:
    out.write(json.dumps(call) + '\\n')
identities = {ISSUE_6698_TESTS!r}
if os.environ['FIXTURE_MODE'] == 'missing_identity':
    identities = identities[:-1]
for identity in identities:
    print('test ' + identity + ' ... ok')
print('fixture complete: integration targets and doctests reached')
if os.environ['FIXTURE_MODE'] == 'source_changed':
    Path('scripts/with-hermetic-test-home.sh').write_text('# changed fixture source\\n')
if os.environ['FIXTURE_MODE'] in ('readonly_cleanup', 'cleanup_failure'):
    root = Path(os.environ['TMPDIR']) / 'readonly-runtime'
    nested = root / 'nested'
    nested.mkdir(parents=True)
    (nested / 'payload.txt').write_text('read-only plugin fixture')
    (nested / 'payload.txt').chmod(0o400)
    outside = Path(os.environ['FIXTURE_OUTSIDE'])
    (root / 'external-directory').symlink_to(outside, target_is_directory=True)
    (root / 'external-file').symlink_to(outside / 'payload.txt')
    os.link(outside / 'payload.txt', root / 'external-hardlink')
    nested.chmod(0o500)
    root.chmod(0o500)
raise SystemExit(101 if os.environ['FIXTURE_MODE'] in ('first_failure', 'cleanup_failure') and not calls else 0)
"""
        fake_rm = f"""#!{sys.executable}
import os, sys
from pathlib import Path
target = Path(sys.argv[-1])
trace = Path(os.environ['FIXTURE_TRACE'])
if (os.environ['FIXTURE_MODE'] == 'cleanup_failure'
        and target.parent == Path('/tmp') and target.name.startswith('cw69.')
        and len(trace.read_text().splitlines()) == 1):
    print('fixture: qualification cleanup refused', file=sys.stderr)
    raise SystemExit(73)
os.execv({shutil.which('rm')!r}, ['rm', *sys.argv[1:]])
"""
        for name, content in {
            "cargo": fake_cargo,
            "rm": fake_rm,
            "rustc": "#!/bin/sh\nprintf '%s\\n' 'rustc fixture (no compiler invoked)'\n",
            "rustup": '#!/bin/sh\nprintf "%s\\n" "$FIXTURE_BIN/rustc"\n',
        }.items():
            path = binary / name
            path.write_text(content, encoding="utf-8")
            path.chmod(0o700)
        env = {
            "PATH": f"{binary}{os.pathsep}{os.environ['PATH']}",
            "HOME": str(fixture / "outer-home"),
            "CARGO_HOME": str(fixture / "cargo-home"),
            "RUSTUP_HOME": str(fixture / "rustup-home"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "RUNNER_TEMP": str(fixture / "evidence"),
            "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64",
            "RUST_MIN_STACK": "16777216", "RUST_TEST_THREADS": "1",
            "OPENAI_API_KEY": "synthetic-untrusted-fixture-key",
            "FIXTURE_BIN": str(binary), "FIXTURE_TRACE": str(trace),
            "FIXTURE_MODE": mode, "FIXTURE_OUTSIDE": str(outside),
        }
        for args in (
            ["init", "-q"], ["add", "scripts/with-hermetic-test-home.sh"],
            ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
             "commit", "-qm", "fixture", "--no-gpg-sign"],
        ):
            subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True)
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, env=env, text=True).strip()
        env.update(EXPECTED_SHA=expected_sha or sha, GITHUB_SHA=sha)
        result = subprocess.run(
            ["bash", "-c", script], cwd=repo, env=env, text=True, capture_output=True, timeout=30,
        )
        evidence = fixture / "evidence/shared-process-workspace"
        calls = [json.loads(line) for line in trace.read_text().splitlines()] if trace.exists() else []
        proof = {
            "result": result,
            "calls": calls,
            "evidence": {path.name: path.read_text() for path in evidence.iterdir()},
            "qualification_removed": [not Path(call["tmpdir"]).exists() for call in calls],
            "outside": {
                "directory_mode": outside.stat().st_mode & 0o777,
                "file_mode": outside_payload.stat().st_mode & 0o777,
                "contents": outside_payload.read_text(encoding="utf-8"),
            },
        }
        # Only the injected rm failure leaves a root; the workflow has already
        # made its directories writable. Do not leave the deliberate fixture.
        for call in calls:
            if Path(call["tmpdir"]).exists():
                shutil.rmtree(call["tmpdir"])
        outside.chmod(0o700)
        return proof


class CiWiringTests(unittest.TestCase):
    def test_boundary_step_still_present(self) -> None:
        ci = load_ci()
        self.assertIn("Check command-contract prototype boundary", ci)
        block = boundary_step_block(ci)
        self.assertIn("test_check_command_crate_boundaries.py", block)
        self.assertIn("check-command-crate-boundaries.py", block)

    def test_migration_self_test_present(self) -> None:
        ci = load_ci()
        block = migration_step_block(ci)
        self.assertIn("test_check_command_migration_manifest.py", block)

    def test_migration_live_scan_present(self) -> None:
        ci = load_ci()
        block = migration_step_block(ci)
        self.assertIn("check-command-migration-manifest.py", block)

    def test_migration_live_scan_receives_fetched_baseline(self) -> None:
        block = migration_step_block(load_ci())
        self.assertIn("PR_BASE_SHA", block)
        self.assertIn("PUSH_BEFORE_SHA", block)
        self.assertIn("git fetch --no-tags origin", block)
        self.assertNotIn(
            "--depth",
            block,
            "baseline fetch must preserve the full ancestry used by later CI range checks",
        )
        self.assertIn('--baseline-ref "${baseline}"', block)

    def test_migration_commands_are_ordered_self_test_first(self) -> None:
        ci = load_ci()
        block = migration_step_block(ci)
        self.assertLess(
            block.index("test_check_command_migration_manifest.py"),
            block.index("check-command-migration-manifest.py"),
            "checker self-tests must run before the live migration scan",
        )

    def test_migration_step_uses_heavy_condition(self) -> None:
        ci = load_ci()
        block = migration_step_block(ci)
        self.assertIn("needs.changes.outputs.heavy == 'true'", block)

    def test_boundary_step_condition_matches_migration_step(self) -> None:
        ci = load_ci()
        boundary = boundary_step_block(ci)
        migration = migration_step_block(ci)
        self.assertIn("needs.changes.outputs.heavy == 'true'", boundary)
        self.assertEqual(
            "needs.changes.outputs.heavy == 'true'" in boundary,
            "needs.changes.outputs.heavy == 'true'" in migration,
        )

    def test_migration_step_does_not_remove_boundary_step(self) -> None:
        ci = load_ci()
        # Both steps must coexist (the migration step is added beside, never
        # replacing, the boundary step).
        self.assertLess(
            ci.index("Check command-contract prototype boundary"),
            ci.index("Check command migration manifest"),
        )

    def test_main_ci_concurrency_is_keyed_by_sha(self) -> None:
        ci = load_ci()
        self.assertIn("github.workflow, github.sha", ci)
        self.assertIn("ci-pr-{0}", ci)
        self.assertNotIn(
            "github.event.pull_request.number || github.ref",
            ci,
            "main must not share one concurrency group per ref — pending runs get cancelled",
        )
        self.assertIn("name: Safety gate", ci)
        self.assertIn("Hermetic safety and authorization tests", ci)

    def test_safety_gate_is_hermetic_for_config_home(self) -> None:
        ci = load_ci()
        start = ci.index("Hermetic safety and authorization tests")
        next_step = ci.index("- name:", start + 1)
        block = ci[start:next_step]
        self.assertIn(
            "sh scripts/with-hermetic-test-home.sh cargo nextest run "
            "-p codewhale-engine -p codewhale-tui -p codewhale-runtime --lib --locked --no-tests=fail "
            "-E 'test(auto_review) | test(authority) | test(sandbox)'", block
        )
        self.assertIn(
            "sh scripts/with-hermetic-test-home.sh cargo test -p codewhale-execpolicy --locked",
            block,
        )
        self.assertNotIn("CODEWHALE_HOME:", block)
        self.assertIn("sh scripts/with-hermetic-test-home.test.sh", ci)

    def test_valid_wiring_passes_all_assertions(self) -> None:
        # The live workflow must satisfy every structural invariant above.
        ci = load_ci()
        self.assertIn("test_check_command_migration_manifest.py", ci)
        self.assertIn("check-command-migration-manifest.py", ci)
        self.assertIn("needs.changes.outputs.heavy == 'true'", ci)

    def test_shared_process_mode_is_manual_hosted_ubuntu_only_and_replaces_the_sharded_suite(self) -> None:
        ci = load_ci()
        self.assertIn("default: nextest", ci)
        job = ci.split("  test:\n", 1)[1].split("    steps:\n", 1)[0]
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertIn(SHARED_PROCESS_MODE, job)
        self.assertNotIn("pull_request", job)
        for name in ("test-ubuntu", "test-macos", "test-windows"):
            caller = ci.split(f"  {name}:\n", 1)[1].split("    uses:", 1)[0]
            self.assertIn(f"!({SHARED_PROCESS_MODE})", caller)
        qualification = named_step(ci, "Shared-process workspace qualification")
        self.assertIn("if: " + SHARED_PROCESS_MODE, qualification)
        upload = named_step(ci, "Retain shared-process qualification evidence")
        self.assertIn("if: always() && " + SHARED_PROCESS_MODE, upload)
        self.assertIn("if-no-files-found: error", upload)
        suite = (ROOT / ".github" / "workflows" / "ci-test-suite.yml").read_text(encoding="utf-8")
        for name, command in (
            ("Run tests", "cargo nextest run"),
            ("Run doctests", "cargo test --workspace --all-features --locked --doc"),
        ):
            self.assertIn(command, named_step(suite, name))

    def test_shared_process_runs_exact_command_twice_with_fresh_hermetic_homes(self) -> None:
        proof = run_shared_process_fixture()
        self.assertEqual(proof["result"].returncode, 0, proof["result"].stderr)
        self.assertEqual(len(proof["calls"]), 2)
        self.assertEqual(set(proof["evidence"]["required-tests.txt"].splitlines()), set(ISSUE_6698_TESTS))
        for call in proof["calls"]:
            self.assertEqual(call["args"], ["test", "--workspace", "--all-features", "--locked", "--", "--format=pretty"])
            self.assertIsNone(call["threads"])
            self.assertEqual(call["stack"], "16777216")
            self.assertFalse(call["credential_present"])
            self.assertTrue(call["tmpdir"].startswith("/tmp/cw69."))
            self.assertLess(len(call["tmpdir"]), 30)
            self.assertFalse(Path(call["home"]).exists(), "the actual wrapper must clean up its private home")
        self.assertNotEqual(proof["calls"][0]["home"], proof["calls"][1]["home"])
        self.assertNotEqual(proof["calls"][0]["tmpdir"], proof["calls"][1]["tmpdir"])
        for run in (1, 2):
            self.assertIn(f"run={run} cargo_exit=0 log_exit=0", proof["evidence"]["results.txt"])
            for identity in ISSUE_6698_TESTS:
                self.assertIn(f"test {identity} ... ok", proof["evidence"][f"run-{run}.log"])

    def test_macos_budget_runs_on_every_heavy_dispatch_and_never_on_pull_requests(self) -> None:
        job = load_ci().split("  macos-budget:\n", 1)[1].split("    steps:\n", 1)[0]
        condition = next(line.strip()[4:] for line in job.splitlines() if line.strip().startswith("if: "))
        self.assertIn("runs-on: macos-latest", job)
        for event in ("pull_request", "push", "schedule", "workflow_dispatch"):
            for heavy in (True, False):
                for scoped in (True, False):
                    with self.subTest(event=event, heavy=heavy, scoped=scoped):
                        expression = condition
                        for name, value in {
                            "github.event_name": event,
                            "needs.changes.outputs.heavy": str(heavy).lower(),
                            "needs.changes.outputs.scoped": str(scoped).lower(),
                        }.items():
                            expression = expression.replace(name, repr(value))
                        expression = re.sub(r"!(?!=)", "not ", expression.replace("&&", "and").replace("||", "or"))
                        actual = eval(expression, {"__builtins__": {}}, {})
                        self.assertEqual(actual, heavy and not scoped and event != "pull_request")

    def test_shared_process_retains_failed_first_run_and_still_runs_second_once(self) -> None:
        proof = run_shared_process_fixture("first_failure")
        self.assertNotEqual(proof["result"].returncode, 0)
        self.assertEqual(len(proof["calls"]), 2)
        self.assertIn("run=1 cargo_exit=101 log_exit=0", proof["evidence"]["results.txt"])
        self.assertIn("run=2 cargo_exit=0 log_exit=0", proof["evidence"]["results.txt"])
        self.assertIn("run-1.log", proof["evidence"])
        self.assertIn("run-2.log", proof["evidence"])

    def test_shared_process_cleans_readonly_directories_without_following_links(self) -> None:
        proof = run_shared_process_fixture("readonly_cleanup")
        self.assertEqual(proof["result"].returncode, 0, proof["result"].stderr)
        self.assertEqual(len(proof["calls"]), 2)
        self.assertEqual(proof["qualification_removed"], [True, True])
        self.assertEqual(proof["outside"], {
            "directory_mode": 0o500, "file_mode": 0o400,
            "contents": "outside fixture must remain unchanged",
        })
        for run in (1, 2):
            self.assertIn(f"run={run} permission_exit=0 cleanup_exit=0", proof["evidence"]["results.txt"])

    def test_shared_process_cleanup_failure_retains_cargo_failure_and_runs_second(self) -> None:
        proof = run_shared_process_fixture("cleanup_failure")
        self.assertNotEqual(proof["result"].returncode, 0)
        self.assertEqual(len(proof["calls"]), 2)
        self.assertEqual(proof["qualification_removed"], [False, True])
        results = proof["evidence"]["results.txt"]
        self.assertIn("run=1 cargo_exit=101 log_exit=0", results)
        self.assertIn("run=1 permission_exit=0 cleanup_exit=73", results)
        self.assertIn("run=2 cargo_exit=0 log_exit=0", results)
        self.assertIn("run=2 permission_exit=0 cleanup_exit=0", results)
        self.assertIn("run-1.log", proof["evidence"])
        self.assertIn("run-2.log", proof["evidence"])

    def test_shared_process_missing_identity_is_not_a_pass(self) -> None:
        proof = run_shared_process_fixture("missing_identity")
        self.assertNotEqual(proof["result"].returncode, 0)
        self.assertEqual(len(proof["calls"]), 2)
        self.assertEqual(proof["evidence"]["results.txt"].count("result=missing_pass"), 2)

    def test_shared_process_refuses_wrong_revision_and_changed_source(self) -> None:
        for wrong_sha in ("not-a-sha", "0" * 40):
            with self.subTest(expected_sha=wrong_sha):
                proof = run_shared_process_fixture(expected_sha=wrong_sha)
                self.assertNotEqual(proof["result"].returncode, 0)
                self.assertEqual(proof["calls"], [])
                self.assertIn("preflight-error.txt", proof["evidence"])
        proof = run_shared_process_fixture("source_changed")
        self.assertNotEqual(proof["result"].returncode, 0)
        self.assertEqual(len(proof["calls"]), 1)
        self.assertIn("run=1 source_changed=true", proof["evidence"]["results.txt"])
        self.assertIn("run=2 not_run=source_changed", proof["evidence"]["results.txt"])


if __name__ == "__main__":
    unittest.main()
