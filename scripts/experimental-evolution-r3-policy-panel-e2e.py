#!/usr/bin/env python3
"""Exercise the policy panel CLI with fixed receipt fixtures, without a model."""

import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SELECTOR = ROOT / "scripts" / "experimental-evolution-r3-policy-panel.py"
GENES = ("planning_budget", "max_repair_rounds", "extra_verification",
         "candidate_strategy", "trace_preference")


def digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, separators=(",", ":")) + "\n", encoding="utf-8")


def make_cell(root, name, task, genome, passed, price):
    directory = root / name
    evaluator = directory / "evaluator" / f"codewhale-evolution-{name}"
    identifier = digest(json.dumps({key: genome[key] for key in GENES}, separators=(",", ":")))
    baseline = digest(f"baseline:{task}")
    artifact = digest(f"artifact:{name}")
    tree = digest(f"tree:{name}")
    material = digest(f"material:{name}")
    candidate = "candidate_g0_1"
    manifest = {"shadow_policy": genome, "max_candidates": 1,
                "max_model_turns": 10, "baseline_sha": baseline,
                "objective_sha256": digest(f"objective:{task}"),
                "public_fixture_sha256": digest(f"fixture:{task}"),
                "hidden_evaluator_sha256": digest(f"hidden:{task}"),
                "scorer_sha256": digest(f"scorer:{task}"),
                "case_bundle_sha256": [digest(f"public:{task}"), digest(f"holdout:{task}")]}
    write(evaluator / "manifest.json", manifest)
    receipt = {"candidate_id": candidate, "policy_id": identifier,
               "shadow_policy": genome, "baseline_sha": baseline,
               "evaluation_id": digest(f"evaluation:{name}"), "artifact_id": artifact,
               "candidate_tree_hash": tree, "review_material_sha256": material,
               "review_material_bytes": 50, "review_complete": True,
               "status": "PASS" if passed else "FAIL"}
    write(evaluator / f"{candidate}-evaluation.json", receipt)
    if passed:
        write(evaluator / f"{candidate}-replay.json", receipt)
    review = {"verdict": "pass", "review_status": "COMPLETE",
              "artifact_id": artifact, "tree_hash": tree,
              "review_material_sha256": material, "available_bytes": 50,
              "unreviewed_scope": []} if passed else None
    events = []
    labels = [f"planner_{candidate}_1", candidate]
    if passed:
        labels.append("evolution_reviewer")
    for index, label in enumerate(labels):
        task_id = f"agent_{index}"
        events.extend([{"type": "task_started", "task_id": task_id, "label": label},
                       {"type": "task_completed", "task_id": task_id,
                        "status": "succeeded", "usage": {"cost_microusd": price[index],
                            "input_tokens": price[index] * 4,
                            "output_tokens": price[index] * 6,
                            "total_tokens": price[index] * 10}}])
    workflow = {"run_id": f"workflow_{name}", "status": "completed",
                "dispatch_failure_count": 0,
                "events": events,
                "usage": {"cost_microusd": sum(price[:len(labels)]),
                          "total_tokens": sum(price[:len(labels)]) * 10},
                "result": {"baseline_sha": baseline,
                           "winner_id": candidate if passed else "NONE", "review": review}}
    write(directory / "workspace" / ".codewhale" / "workflow-runs.jsonl", {"run": workflow})
    write(directory / "run.json", {"arm": "single", "execution_budget": 1,
                                   "initial_policy": genome, "baseline_sha": baseline})
    return {"task_id": task, "seed": "replicate_0", "run_dir": str(directory)}


def invoke(plan_path, output_path):
    return subprocess.run([sys.executable, str(SELECTOR), "--plan", str(plan_path),
                           "--output", str(output_path)], cwd=ROOT,
                          capture_output=True, text=True, check=False)


def main():
    base = {"planning_budget": 1, "max_repair_rounds": 1,
            "extra_verification": False, "candidate_strategy": "direct",
            "trace_preference": "evidence_refs"}
    evolved = {**base, "planning_budget": 2}
    with tempfile.TemporaryDirectory(prefix="codewhale-r3-panel-") as temporary:
        root = Path(temporary)
        plan = {"schema_version": 1, "policies": []}
        for name, genome, outcomes, prices in (
            ("a", base, (True, False), ((2, 3, 1), (2, 3))),
            ("b", evolved, (True, True), ((3, 6, 1), (3, 6, 1))),
        ):
            cells = [make_cell(root, f"{name}-{task}", task, genome, passed, price)
                     for task, passed, price in zip(("alpha", "beta"), outcomes, prices)]
            identifier = digest(json.dumps({key: genome[key] for key in GENES}, separators=(",", ":")))
            plan["policies"].append({"policy_id": identifier, "genome": genome, "cells": cells})
        plan_path = root / "plan.json"
        output = root / "selection.json"
        write(plan_path, plan)
        first = invoke(plan_path, output)
        assert first.returncode == 0, first.stderr
        assert json.loads(output.read_text())["selected_policy_id"] == plan["policies"][1]["policy_id"]
        b_beta = root / "b-beta"
        evaluation = b_beta / "evaluator" / "codewhale-evolution-b-beta" / "candidate_g0_1-evaluation.json"
        changed = json.loads(evaluation.read_text())
        changed["status"] = "FAIL"
        write(evaluation, changed)
        second = invoke(plan_path, output)
        assert second.returncode == 0, second.stderr
        assert json.loads(output.read_text())["selected_policy_id"] == plan["policies"][0]["policy_id"]
        manifest = b_beta / "evaluator" / "codewhale-evolution-b-beta" / "manifest.json"
        changed = json.loads(manifest.read_text())
        changed["case_bundle_sha256"][0] = digest("tampered")
        write(manifest, changed)
        third = invoke(plan_path, output)
        assert third.returncode != 0 and "different task or oracle identities" in third.stderr
        fallback = {"schema_version": 1, "policies": []}
        for name, genome, prices in (
            ("c", base, ((1, 2, 1), (1, 2, 1))),
            ("d", evolved, ((3, 5, 2), (3, 5, 2))),
        ):
            cells = [make_cell(root, f"{name}-{task}", task, genome, True, price)
                     for task, price in zip(("alpha", "beta"), prices)]
            identifier = digest(json.dumps({key: genome[key] for key in GENES},
                                           separators=(",", ":")))
            fallback["policies"].append({"policy_id": identifier,
                                         "genome": genome, "cells": cells})
            for cell in cells:
                journal = Path(cell["run_dir"]) / "workspace" / ".codewhale" / "workflow-runs.jsonl"
                saved = json.loads(journal.read_text(encoding="utf-8"))
                saved["run"]["usage"].pop("cost_microusd")
                for event in saved["run"]["events"]:
                    if event["type"] == "task_completed":
                        event["usage"].pop("cost_microusd")
                write(journal, saved)
        write(plan_path, fallback)
        fourth = invoke(plan_path, output)
        assert fourth.returncode == 0, fourth.stderr
        selected = json.loads(output.read_text())
        assert selected["selected_policy_id"] == fallback["policies"][0]["policy_id"]
        assert selected["ranking"][0]["token_usage_complete"] is True
        assert selected["ranking"][0]["cost_complete"] is False
        missing = root / "c-beta" / "evaluator" / "codewhale-evolution-c-beta" \
            / "candidate_g0_1-evaluation.json"
        missing.unlink()
        fifth = invoke(plan_path, output)
        assert fifth.returncode == 0, fifth.stderr
        absent = json.loads(output.read_text())
        assert absent["selected_policy_id"] == fallback["policies"][1]["policy_id"]
        assert any(row["public_status"] == "MISSING" and not row["task_success"]
                   for row in absent["rows"])
        print(json.dumps({"status": "PASS", "checks": 5,
                          "scope": "receipt-contract-fixture-only"}))


if __name__ == "__main__":
    main()
