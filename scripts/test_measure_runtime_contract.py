#!/usr/bin/env python3
"""Hermetic tests for scripts/measure-runtime-contract.py."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "measure-runtime-contract.py"

SPEC = importlib.util.spec_from_file_location("measure_runtime_contract", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class RuntimeContractMeasurementTests(unittest.TestCase):
    def test_metric_runner_targets_exact_library_test(self) -> None:
        short_name = "print_mode_tool_catalog_metrics"
        exact_name = f"core::engine::tests::{short_name}"
        marker = "TOOL_CATALOG_METRICS "
        payload = {"modes": {}, "surface_profile": "fixture"}
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=(
                "running 1 test\n"
                f"test {exact_name} ... {marker}{json.dumps(payload)}\n"
                "ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out\n"
            ),
            stderr="",
        )

        with mock.patch.object(mod.subprocess, "run", return_value=completed) as run:
            measured = mod.run_metric(short_name, marker)

        self.assertEqual(measured, payload)
        self.assertEqual(
            run.call_args.args[0],
            [
                "cargo",
                "test",
                "--locked",
                "-p",
                "codewhale-engine",
                "--lib",
                exact_name,
                "--",
                "--ignored",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ],
        )

    def test_successful_zero_test_run_is_rejected(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=(
                "running 0 tests\n\n"
                "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out\n"
            ),
            stderr="",
        )

        with (
            mock.patch.object(mod.subprocess, "run", return_value=completed),
            redirect_stdout(io.StringIO()),
            self.assertRaisesRegex(RuntimeError, "ran zero tests"),
        ):
            mod.run_metric(
                "print_mode_tool_catalog_metrics", "TOOL_CATALOG_METRICS "
            )

    def test_successful_test_without_expected_marker_is_rejected(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout=(
                "running 1 test\n"
                "test core::engine::tests::print_mode_tool_catalog_metrics ... ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
                "0 filtered out\n"
            ),
            stderr="",
        )

        with (
            mock.patch.object(mod.subprocess, "run", return_value=completed),
            redirect_stdout(io.StringIO()),
            self.assertRaisesRegex(
                RuntimeError,
                "missing TOOL_CATALOG_METRICS marker from exact library metric test",
            ),
        ):
            mod.run_metric(
                "print_mode_tool_catalog_metrics", "TOOL_CATALOG_METRICS "
            )


if __name__ == "__main__":
    raise SystemExit(unittest.main())
