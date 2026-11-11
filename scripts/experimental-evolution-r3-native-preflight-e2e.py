#!/usr/bin/env python3
"""Exercise the real Workflow CLI admission path without a provider request."""

import argparse
import json
import os
import runpy
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    helper = runpy.run_path(str(Path(__file__).with_name("experimental-evolution-smoke.py")))
    fixtures = helper["DEFAULT_FIXTURES"].resolve()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    workspace = root / "workspace"
    helper["make_baseline"](workspace, fixtures)
    policy = {
        "planning_budget": 2,
        "max_repair_rounds": 2,
        "extra_verification": True,
        "candidate_strategy": "direct",
        "trace_preference": "evidence_refs",
    }
    brief = "Implement the CLI exactly as specified below.\nPUBLIC CONTRACT:\n" \
        + (fixtures / "task" / "README.md").read_text(encoding="utf-8")
    for starter in sorted((fixtures / "task" / "src").glob("*.py")):
        brief += f"\nSTARTER {starter.name}:\n" + starter.read_text(encoding="utf-8")
    manifest = helper["write_manifest"](root, workspace, "independent", 6, 3,
                                         fixtures, brief, policy, 37)
    config = root / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\nmodel = "deepseek-flash"\n',
                      encoding="utf-8")
    payload = {
        "action": "run",
        "source_path": "workflows/operate_best_of_n.workflow.js",
        "args": {
            "strategy": "search", "brief": brief,
            "model": "deepseek-flash", "n": 6, "writeRoots": ["src"],
            "evolution": {"manifestPath": str(manifest), "arm": "independent",
                          "mode": "learn_policy", "executionBudget": 6,
                          "generations": 3},
        },
    }
    environment = os.environ.copy()
    environment["DEEPSEEK_API_KEY"] = "local-preflight-only"
    environment.update({"CODEWHALE_HOME": str(root / "host-home"),
                        "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1"})
    environment.update({"HTTPS_PROXY": "http://127.0.0.1:9",
                        "HTTP_PROXY": "http://127.0.0.1:9",
                        "NO_PROXY": "127.0.0.1,localhost"})
    command = [str(args.codewhale.resolve()), "--config", str(config),
               "--workspace", str(workspace), "--provider", "deepseek",
               "--model", "deepseek-flash", "--sandbox-mode", "workspace-write",
               "workflow-tool", "--approval-source", "explicit-workflow-command",
               "--input-json", json.dumps(payload, separators=(",", ":"))]
    completed = subprocess.run(command, cwd=workspace, env=environment,
                               capture_output=True, text=True, timeout=90, check=False)
    (root / "stdout.log").write_text(completed.stdout, encoding="utf-8")
    (root / "stderr.log").write_text(completed.stderr, encoding="utf-8")
    combined = completed.stdout + completed.stderr
    assert "infeasible experimental budget plan" in combined, combined[-2000:]
    assert "required 44 model turns, available 37" in combined, combined[-2000:]
    assert not list(workspace.glob(".codewhale/workflow-runs.jsonl")), "Workflow started despite rejected preflight"
    receipt = {"status": "PASS", "checks": 3, "exit_code": completed.returncode,
               "required_model_turns": 44, "available_model_turns": 37,
               "credential": "synthetic_local_only", "cli": str(args.codewhale.resolve())}
    (root / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
