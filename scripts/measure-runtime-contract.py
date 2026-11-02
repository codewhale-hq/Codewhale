#!/usr/bin/env python3
"""Measure the provider-free model-facing runtime contract.

Combines the serialized tool catalog and the rendered system prompt into a
single reproducible receipt. No API keys or live providers are required.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys


METRIC_TEST_MODULE = "core::engine::tests"


def metric_test_name(test_name: str) -> str:
    return f"{METRIC_TEST_MODULE}::{test_name}"


def metric_command(test_name: str) -> list[str]:
    exact_test_name = metric_test_name(test_name)
    return [
        "cargo",
        "test",
        "--locked",
        "-p",
        "codewhale-engine",
        "--lib",
        exact_test_name,
        "--",
        "--ignored",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]


def run_metric(test_name: str, marker: str) -> dict:
    cmd = metric_command(test_name)
    proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
    sys.stderr.write(proc.stderr)
    if proc.returncode != 0:
        sys.stdout.write(proc.stdout)
        proc.check_returncode()

    combined = proc.stdout.splitlines() + proc.stderr.splitlines()
    if re.search(r"\brunning\s+0\s+tests?\b", "\n".join(combined)):
        sys.stdout.write(proc.stdout)
        raise RuntimeError(
            f"exact library metric test {metric_test_name(test_name)} ran zero tests"
        )
    for line in combined:
        if marker in line:
            return json.loads(line.split(marker, 1)[1])

    sys.stdout.write(proc.stdout)
    raise RuntimeError(
        f"missing {marker.rstrip()} marker from exact library metric test "
        f"{metric_test_name(test_name)}"
    )


def main() -> int:
    tool_metrics = run_metric(
        "print_mode_tool_catalog_metrics",
        "TOOL_CATALOG_METRICS ",
    )
    prompt_metrics = run_metric(
        "print_mode_runtime_contract_metrics",
        "RUNTIME_CONTRACT_METRICS ",
    )
    representative_context_metrics = run_metric(
        "print_representative_runtime_context_metrics",
        "REPRESENTATIVE_CONTEXT_METRICS ",
    )
    skill_discovery_metrics = run_metric(
        "print_skill_discovery_turn_metrics",
        "SKILL_DISCOVERY_METRICS ",
    )

    receipt = {
        "document_kind": "codewhale.runtime_contract_receipt",
        "schema_version": 1,
        "representative_context": representative_context_metrics,
        "skill_discovery": skill_discovery_metrics,
        "tool_catalog": tool_metrics,
        "system_prompt": prompt_metrics,
    }
    print(json.dumps(receipt, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
