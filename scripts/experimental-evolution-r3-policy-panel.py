#!/usr/bin/env python3
"""Select a frozen shadow policy from a common two-task DEV receipt panel."""

import argparse
import hashlib
import json
from pathlib import Path


GENES = ("planning_budget", "max_repair_rounds", "extra_verification",
         "candidate_strategy", "trace_preference")
TASK_IDENTITY = ("baseline_sha", "objective_sha256", "public_fixture_sha256",
                 "hidden_evaluator_sha256", "case_bundle_sha256",
                 "requested_provider", "requested_model", "resolved_provider",
                 "resolved_model")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def policy_id(genome):
    if set(genome) != set(GENES):
        raise ValueError("panel policy uses an unknown or missing gene")
    ordered = {gene: genome[gene] for gene in GENES}
    return hashlib.sha256(json.dumps(ordered, separators=(",", ":"),
                                     ensure_ascii=False).encode("utf-8")).hexdigest()


def last_run(path):
    result = None
    with path.open(encoding="utf-8") as stream:
        for line in stream:
            value = json.loads(line)
            if "run" in value:
                result = value["run"]
    if not result or result.get("status") not in ("completed", "degraded", "failed"):
        raise ValueError(f"no terminal Workflow run in {path}")
    return result


def full_cost(run):
    if run.get("events_dropped", 0):
        raise ValueError("Workflow child ledger is incomplete")
    starts = {}
    completions = {}
    for event in run.get("events", []):
        if event.get("type") == "task_started":
            task_id = event.get("task_id")
            if not task_id or task_id in starts:
                raise ValueError("duplicate or missing task-start identity")
            starts[task_id] = event.get("label")
        elif event.get("type") == "task_completed":
            task_id = event.get("task_id")
            if not task_id or task_id in completions:
                raise ValueError("duplicate or missing task-completion identity")
            completions[task_id] = event
    unknown = []
    unknown_tokens = []
    subtotal = 0
    token_total = 0
    input_total = 0
    output_total = 0
    for task_id, label in starts.items():
        event = completions.get(task_id)
        usage = (event or {}).get("usage") or {}
        charge = usage.get("cost_microusd")
        if not isinstance(charge, int) or isinstance(charge, bool) or charge < 0:
            unknown.append(label or task_id)
        else:
            subtotal += charge
        values = [usage.get(key) for key in ("input_tokens", "output_tokens", "total_tokens")]
        if any(not isinstance(value, int) or isinstance(value, bool) or value < 0
               for value in values) or values[0] + values[1] != values[2]:
            unknown_tokens.append(label or task_id)
        else:
            input_total += values[0]
            output_total += values[1]
            token_total += values[2]
    if set(completions) - set(starts):
        raise ValueError("completion without an admitted task")
    if run.get("child_ids") is not None and set(run["child_ids"]) != set(starts):
        raise ValueError("Workflow child identities differ from event ledger")
    reported = (run.get("usage") or {}).get("cost_microusd")
    if not unknown and reported is not None and reported != subtotal:
        raise ValueError("Workflow usage total disagrees with child ledger")
    reported_tokens = (run.get("usage") or {}).get("total_tokens")
    if not unknown_tokens and reported_tokens is not None and reported_tokens != token_total:
        raise ValueError("Workflow token total disagrees with child ledger")
    return {"known_cost_microusd": subtotal, "complete": not unknown,
            "unknown_liability_labels": unknown,
            "known_provider_tokens": token_total,
            "known_input_tokens": input_total,
            "known_output_tokens": output_total,
            "token_usage_complete": not unknown_tokens,
            "unknown_token_usage_labels": unknown_tokens,
            "started_tasks": len(starts),
            "dispatch_failure_count": run.get("dispatch_failure_count", 0)}


def read_cell(cell, genome, expected_policy_id):
    directory = Path(cell["run_dir"]).resolve()
    run_meta = read_json(directory / "run.json")
    binary_sha256 = run_meta.get("binary_sha256")
    if not isinstance(binary_sha256, str) or len(binary_sha256) != 64 \
            or any(character not in "0123456789abcdef" for character in binary_sha256):
        raise ValueError("panel cell lacks a pinned native CLI binary hash")
    if run_meta.get("arm") != "single" or run_meta.get("execution_budget") != 1:
        raise ValueError("panel cell must be a one-candidate frozen-policy run")
    if run_meta.get("initial_policy") != genome:
        raise ValueError("run initial policy differs from frozen panel policy")
    evaluator_roots = list((directory / "evaluator").glob("codewhale-evolution-*"))
    if len(evaluator_roots) != 1:
        raise ValueError("panel cell requires exactly one native evaluator directory")
    evaluator = evaluator_roots[0]
    manifest = read_json(evaluator / "manifest.json")
    if manifest.get("shadow_policy") != genome or manifest.get("max_candidates") != 1:
        raise ValueError("native evaluator policy or candidate ceiling changed")
    if manifest.get("baseline_sha") != run_meta.get("baseline_sha"):
        raise ValueError("native baseline differs from runner baseline")
    if (run_meta.get("budget_plan") is not None
            and run_meta["budget_plan"].get("max_model_turns") != manifest.get("max_model_turns")):
        raise ValueError("runner and native budget ceilings differ")
    evaluations = list(evaluator.glob("*-evaluation.json"))
    if len(evaluations) > 1:
        raise ValueError("panel cell contains multiple candidate evaluations")
    evaluation = read_json(evaluations[0]) if evaluations else None
    if evaluation and (evaluation.get("policy_id") != expected_policy_id
                       or evaluation.get("shadow_policy") != genome):
        raise ValueError("evaluated policy identity differs from panel genome")
    if evaluation and evaluation.get("baseline_sha") != manifest.get("baseline_sha"):
        raise ValueError("candidate baseline identity changed")
    workflow = last_run(directory / "workspace" / ".codewhale" / "workflow-runs.jsonl")
    outcome = workflow.get("result") or {}
    if outcome.get("baseline_sha") != manifest.get("baseline_sha"):
        raise ValueError("Workflow baseline differs from evaluator")
    replay_path = evaluator / f"{evaluation['candidate_id']}-replay.json" if evaluation else None
    replay = read_json(replay_path) if replay_path and replay_path.is_file() else None
    review = outcome.get("review") or {}
    identity = bool(evaluation and replay and isinstance(evaluation.get("review_material_bytes"), int)
                    and evaluation["review_material_bytes"] > 0
                    and evaluation.get("review_complete") is True
                    and replay.get("review_complete") is True
                    and evaluation.get("artifact_id") == replay.get("artifact_id")
                    and evaluation.get("candidate_tree_hash") == replay.get("candidate_tree_hash")
                    and evaluation.get("review_material_sha256") == replay.get("review_material_sha256")
                    and review.get("artifact_id") == evaluation.get("artifact_id")
                    and review.get("tree_hash") == evaluation.get("candidate_tree_hash")
                    and review.get("review_material_sha256") == evaluation.get("review_material_sha256")
                    and review.get("available_bytes") == evaluation.get("review_material_bytes")
                    and review.get("unreviewed_scope") == [])
    cost = full_cost(workflow)
    success = bool(identity and evaluation.get("status") == "PASS"
                   and replay.get("status") == "PASS" and review.get("verdict") == "pass"
                   and review.get("review_status") == "COMPLETE"
                   and outcome.get("winner_id") == evaluation.get("candidate_id")
                   and not cost["dispatch_failure_count"])
    task_identity = {key: manifest.get(key) for key in TASK_IDENTITY}
    task_identity["gate_contract"] = [{key: gate.get(key) for key in
                                       ("candidate_argv", "timeout_secs", "min_tests", "holdout")}
                                      for gate in manifest.get("hard_gates", [])]
    if any(value is None for value in task_identity.values()):
        raise ValueError("native task identity is incomplete")
    return {"task_id": cell["task_id"], "seed": cell["seed"],
            "run_dir": str(directory), "run_id": workflow.get("run_id"),
            "binary_sha256": binary_sha256,
            "workflow_status": workflow.get("status"),
            "evaluation_id": evaluation.get("evaluation_id") if evaluation else None,
            "artifact_id": evaluation.get("artifact_id") if evaluation else None,
            "task_identity": task_identity,
            "public_status": evaluation.get("status") if evaluation else "MISSING",
            "replay_status": replay.get("status") if replay else "MISSING",
            "review_status": review.get("review_status", "MISSING"),
            "task_success": success, "full_cost": cost,
            "budget_ceiling_model_turns": manifest.get("max_model_turns")}


def select(plan):
    if plan.get("schema_version") != 1 or len(plan.get("policies", [])) != 2:
        raise ValueError("panel requires exactly two frozen policies")
    rows = []
    identities = {}
    budget_ceiling = None
    binary_sha256 = None
    for policy in plan["policies"]:
        genome = policy["genome"]
        identifier = policy_id(genome)
        if policy.get("policy_id") != identifier:
            raise ValueError("panel policy hash does not match frozen genome")
        cells = policy.get("cells", [])
        if len(cells) != 2:
            raise ValueError("each policy needs the same two DEV task and seed cells")
        seen = set()
        for cell in cells:
            key = (cell["task_id"], cell["seed"])
            if key in seen:
                raise ValueError("duplicate task and seed within a policy")
            seen.add(key)
            row = read_cell(cell, genome, identifier)
            identity = row["task_identity"]
            if key in identities and identities[key] != identity:
                raise ValueError("policies used different task or oracle identities")
            identities[key] = identity
            if budget_ceiling is not None and row["budget_ceiling_model_turns"] != budget_ceiling:
                raise ValueError("panel cells used unequal model-turn ceilings")
            budget_ceiling = row["budget_ceiling_model_turns"]
            if binary_sha256 is not None and row["binary_sha256"] != binary_sha256:
                raise ValueError("panel cells used different native CLI binaries")
            binary_sha256 = row["binary_sha256"]
            rows.append({"policy_id": identifier, **row})
        if len(identities) != 2:
            raise ValueError("panel must cover two distinct DEV task and seed cells")
    if len({row["policy_id"] for row in rows}) != 2:
        raise ValueError("competing policies have the same genome")
    ranks = []
    for policy in plan["policies"]:
        identifier = policy["policy_id"]
        subset = [row for row in rows if row["policy_id"] == identifier]
        successes = sum(row["task_success"] for row in subset)
        complete_cost = all(row["full_cost"]["complete"] for row in subset)
        complete_tokens = all(row["full_cost"]["token_usage_complete"] for row in subset)
        cost = sum(row["full_cost"]["known_cost_microusd"] for row in subset)
        tokens = sum(row["full_cost"]["known_provider_tokens"] for row in subset)
        rejects = sum(row["full_cost"]["dispatch_failure_count"] for row in subset)
        ranks.append({"policy_id": identifier, "task_successes": successes,
                      "task_count": len(subset), "cost_complete": complete_cost,
                      "known_cost_microusd": cost,
                      "token_usage_complete": complete_tokens,
                      "known_provider_tokens": tokens,
                      "dispatch_failures": rejects})
    ranks.sort(key=lambda row: (-row["task_successes"], row["dispatch_failures"],
                                0 if row["cost_complete"] else
                                1 if row["token_usage_complete"] else 2,
                                row["known_cost_microusd"] if row["cost_complete"] else
                                row["known_provider_tokens"] if row["token_usage_complete"]
                                else float("inf"), row["policy_id"]))
    selected = ranks[0]["policy_id"] if ranks[0]["task_successes"] else None
    body = {"schema_version": 1, "panel": "two_policy_two_task_dev",
            "provider_seed_control": "unavailable", "task_seed_labels": sorted(identities),
            "equal_model_turn_ceiling": budget_ceiling,
            "binary_sha256": binary_sha256, "rows": rows,
            "ranking": ranks, "selected_policy_id": selected,
            "policy_promotion_proposal": {"status": "NOT_QUALIFIED",
                "reason": "A frozen new-task holdout run is required after DEV policy selection."}}
    encoded = json.dumps(body, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return {**body, "selection_id": hashlib.sha256(encoded.encode("utf-8")).hexdigest()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    plan = read_json(args.plan)
    result = select(plan)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n",
                           encoding="utf-8")
    if result["selected_policy_id"]:
        genome = next(policy["genome"] for policy in plan["policies"]
                      if policy["policy_id"] == result["selected_policy_id"])
        args.output.with_name("selected_policy.json").write_text(
            json.dumps(genome, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: result[key] for key in ("selection_id", "selected_policy_id",
                                               "ranking")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
