#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


root = Path(__file__).resolve().parents[1]
policy = {
    "planning_budget": 2,
    "max_repair_rounds": 2,
    "extra_verification": True,
    "candidate_strategy": "direct",
    "trace_preference": "evidence_refs",
}
with tempfile.TemporaryDirectory(prefix="codewhale-r3-budget-") as temporary:
    path = Path(temporary) / "policy.json"
    path.write_text(json.dumps(policy), encoding="utf-8")
    environment = os.environ.copy()
    environment.pop("DEEPSEEK_API_KEY", None)
    result = subprocess.run(
        [sys.executable, str(root / "scripts/experimental-evolution-smoke.py"),
         "--plan-only", "--budget", "6", "--generations", "3",
         "--initial-policy-json", str(path)],
        cwd=root, env=environment, capture_output=True, text=True, check=False)
    if result.returncode:
        raise AssertionError(f"plan-only failed before model work: {result.stderr}")
    plan = json.loads(result.stdout)
    assert plan["candidate_execution_ceiling"] == 6, plan
    assert plan["initial_candidate_turns"] == 7, plan
    assert plan["final_review_turns"] == 2, plan
    assert plan["initial_required_turns"] == 44, plan
    assert plan["max_model_turns"] == 50, plan
    single = subprocess.run(
        [sys.executable, str(root / "scripts/experimental-evolution-smoke.py"),
         "--plan-only", "--budget", "1", "--generations", "1", "--arms", "single",
         "--initial-policy-json", str(path)],
        cwd=root, env=environment, capture_output=True, text=True, check=False)
    assert single.returncode == 0, single.stderr
    single_plan = json.loads(single.stdout)
    assert single_plan["candidate_execution_ceiling"] == 1, single_plan
    assert single_plan["initial_required_turns"] == 9, single_plan
    assert single_plan["max_model_turns"] == 10, single_plan
    print(json.dumps({"status": "PASS", "checks": 9, "plan": plan,
                      "single_plan": single_plan}))
