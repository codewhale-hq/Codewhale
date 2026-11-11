#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import re
import runpy
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = ROOT / "examples" / "experimental-evolution-smoke"
WORKFLOW = ROOT / "workflows" / "operate_best_of_n.workflow.js"


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, **kwargs)


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def make_baseline(destination, fixtures):
    destination.mkdir(parents=True)
    shutil.copytree(fixtures / "task", destination, dirs_exist_ok=True)
    target = destination / "workflows" / WORKFLOW.name
    target.parent.mkdir(parents=True)
    shutil.copy2(WORKFLOW, target)
    run(["git", "init", "-q", str(destination)])
    run(["git", "-C", str(destination), "add", "README.md", "src", "workflows"])
    env = os.environ.copy()
    env.update({
        "GIT_AUTHOR_NAME": "CodeWhale Experiment",
        "GIT_AUTHOR_EMAIL": "experiment@example.invalid",
        "GIT_COMMITTER_NAME": "CodeWhale Experiment",
        "GIT_COMMITTER_EMAIL": "experiment@example.invalid",
        "GIT_AUTHOR_DATE": "2026-11-10T00:00:00Z",
        "GIT_COMMITTER_DATE": "2026-11-10T00:00:00Z",
    })
    run(["git", "-C", str(destination), "commit", "-qm", "frozen coding task baseline"], env=env)
    return run(["git", "-C", str(destination), "rev-parse", "HEAD"], capture_output=True).stdout.strip()


def write_manifest(directory, workspace, arm, budget, generations, fixtures, brief, initial_policy,
                   model_turn_ceiling):
    public = (fixtures / "public_gate.py").resolve()
    hidden = (fixtures / "heldout_gate.py").resolve()
    gates = []
    for source, holdout, minimum in [(public, False, 8), (hidden, True, 6)]:
        cases = runpy.run_path(str(source))["CASES"]
        program = re.search(r'sys\.executable,\s*["\'](src/[^"\']+)["\']',
                            source.read_text(encoding="utf-8"))
        if program is None or not cases:
            raise ValueError(f"unsupported process-level gate: {source}")
        case_path = directory / ("heldout.cases.json" if holdout else "public.cases.json")
        case_path.write_text(json.dumps([
            {"input": request, "expected": expected} for request, expected in cases
        ], separators=(",", ":")) + "\n", encoding="utf-8")
        gates.append({
            "candidate_argv": [str(Path(sys.executable).resolve()), program.group(1)],
            "cases_path": str(case_path.resolve()),
            "timeout_secs": max(60, len(cases) * 8),
            "min_tests": minimum,
            "holdout": holdout,
        })
    manifest = {
        "version": 2,
        "objective": brief,
        "public_fixture_path": str((workspace / "README.md").resolve()),
        "hidden_evaluator_path": str(hidden),
        "allowed_write_roots": ["src"],
        "protected_paths": ["README.md", "workflows"],
        "hard_gates": gates,
        "requested_provider": "deepseek",
        "requested_model": "deepseek-flash",
        "resolved_provider": "deepseek",
        "resolved_model": "deepseek-flash",
        "max_candidates": 1 if arm == "single" else budget,
        "max_generations": budget if arm == "serial" else generations if arm in ("fixed_policy", "evolution") else 1,
        "max_model_turns": model_turn_ceiling,
        "max_wall_secs": 3600,
        "shadow_policy": initial_policy,
    }
    path = directory / "manifest.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return path


def summarize_receipts(evaluator_directory):
    evaluations = []
    replays = []
    for path in sorted(evaluator_directory.glob("*-evaluation.json")):
        receipt = json.loads(path.read_text(encoding="utf-8"))
        evaluations.append({
            "candidate_id": receipt.get("candidate_id"),
            "generation": receipt.get("generation"),
            "status": receipt.get("status"),
            "reason": receipt.get("reason"),
            "patch_sha256": receipt.get("patch_sha256"),
            "test_count": receipt.get("test_count"),
            "lineage": receipt.get("lineage"),
            "shadow_policy": receipt.get("shadow_policy"),
            "usage": receipt.get("usage"),
            "receipt_path": str(path),
        })
    for path in sorted(evaluator_directory.glob("*-replay.json")):
        receipt = json.loads(path.read_text(encoding="utf-8"))
        holdout = [gate for gate in receipt.get("gates", []) if gate.get("holdout")]
        replays.append({
            "candidate_id": receipt.get("candidate_id"),
            "status": receipt.get("status"),
            "test_count": receipt.get("test_count"),
            "heldout_gate_count": len(holdout),
            "heldout_pass": bool(holdout) and all(gate.get("status") == "PASS" for gate in holdout),
            "receipt_path": str(path),
        })
    return {
        "evaluations": evaluations,
        "replays": replays,
        "public_pass_count": sum(item["status"] == "PASS" for item in evaluations),
        "heldout_replay_pass": any(item["status"] == "PASS" and item["heldout_pass"]
                                   for item in replays),
        "mutant_evaluation_count": sum((item["generation"] or 0) > 0 for item in evaluations),
    }


def budget_plan(budget, initial_policy):
    if not isinstance(initial_policy["planning_budget"], int) or not 0 <= initial_policy["planning_budget"] <= 3 \
            or not isinstance(initial_policy["extra_verification"], bool):
        raise ValueError("invalid effective policy budget genes")
    writer_turns = 4
    final_review_turns = 2
    initial_candidate_turns = initial_policy["planning_budget"] + writer_turns \
        + int(initial_policy["extra_verification"])
    max_candidate_turns = max(range(4)) + writer_turns + int(True)
    ceiling = budget * max_candidate_turns + final_review_turns
    return {"candidate_execution_ceiling": budget,
            "initial_candidate_turns": initial_candidate_turns,
            "initial_required_turns": budget * initial_candidate_turns + final_review_turns,
            "max_candidate_turns": max_candidate_turns,
            "final_review_turns": final_review_turns,
            "max_model_turns": ceiling}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--plan-only", action="store_true")
    parser.add_argument("--budget", type=int, default=6)
    parser.add_argument("--generations", type=int, choices=[1, 2, 3], default=3)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--arms", nargs="+", choices=["single", "independent", "serial", "fixed_policy", "evolution"],
                        default=["independent", "serial", "fixed_policy", "evolution"])
    parser.add_argument("--initial-policy-json", type=Path,
                        help="Frozen genome from a selected DEV receipt for a new-task run")
    parser.add_argument("--transfer-source-evaluation-id",
                        help="The verified DEV evaluation that selected the transferred genome")
    args = parser.parse_args()
    single_panel_cell = args.arms == ["single"] and args.budget == 1 and args.generations == 1
    if not single_panel_cell and (not 2 * args.generations <= args.budget <= 16
                                  or args.budget % args.generations):
        parser.error("budget must provide at least two candidates per generation, up to 16")
    initial_policy = {
        "planning_budget": 1,
        "max_repair_rounds": 1,
        "extra_verification": False,
        "candidate_strategy": "direct",
        "trace_preference": "evidence_refs",
    }
    if args.initial_policy_json:
        initial_policy = json.loads(args.initial_policy_json.read_text(encoding="utf-8"))
        if set(initial_policy) != {"planning_budget", "max_repair_rounds", "extra_verification",
                                   "candidate_strategy", "trace_preference"}:
            parser.error("transferred genome must contain only the frozen five-gene schema")
    try:
        plan = budget_plan(args.budget, initial_policy)
    except ValueError as error:
        parser.error(str(error))
    if args.plan_only:
        print(json.dumps(plan, sort_keys=True))
        return
    if args.codewhale is None or not args.codewhale.is_file():
        parser.error("--codewhale must name a built binary")
    if args.output is None:
        parser.error("--output is required for a provider run")
    if not os.environ.get("DEEPSEEK_API_KEY"):
        parser.error("DEEPSEEK_API_KEY must be exported for real provider runs")
    binary_sha256 = file_sha256(args.codewhale.resolve())
    fixtures = args.fixture if args.fixture.is_absolute() else ROOT / args.fixture
    fixtures = fixtures.resolve()
    for required in ("task/README.md", "public_gate.py", "heldout_gate.py"):
        if not (fixtures / required).is_file():
            parser.error(f"fixture missing {required}: {fixtures}")
    brief = "Implement the CLI exactly as specified below.\nPUBLIC CONTRACT:\n" \
        + (fixtures / "task" / "README.md").read_text(encoding="utf-8")
    for starter in sorted((fixtures / "task" / "src").glob("*.py")):
        brief += f"\nSTARTER {starter.name}:\n" + starter.read_text(encoding="utf-8")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    baseline = output / "baseline"
    baseline_sha = make_baseline(baseline, fixtures)
    config = output / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\nmodel = "deepseek-flash"\n', encoding="utf-8")
    results = []
    for arm in args.arms:
        directory = output / arm
        directory.mkdir()
        workspace = directory / "workspace"
        run(["git", "clone", "-q", "--local", str(baseline), str(workspace)])
        manifest = write_manifest(directory, workspace, arm, args.budget,
                                  args.generations, fixtures, brief, initial_policy,
                                  plan["max_model_turns"])
        payload = {
            "action": "run",
            "source_path": "workflows/operate_best_of_n.workflow.js",
            "args": {
                "strategy": "search",
                "brief": brief,
                "rubric": "Follow README.md. Keep the CLI deterministic and change only src/.",
                "model": "deepseek-flash",
                "n": args.budget // args.generations,
                "writeRoots": ["src"],
                "evolution": {
                    "manifestPath": str(manifest),
                    "arm": arm,
                    "mode": "solve_task" if arm == "serial" else "learn_policy",
                    "executionBudget": args.budget,
                    "generations": args.generations,
                },
            },
        }
        environment = os.environ.copy()
        environment.update({
            "CODEWHALE_HOME": str(directory / "host-home"),
            "CODEWHALE_TELEMETRY": "0",
            "CODEWHALE_NO_UPDATE_CHECK": "1",
        })
        command = [str(args.codewhale.resolve()), "--config", str(config), "--workspace", str(workspace),
                   "--provider", "deepseek", "--model", "deepseek-flash", "--sandbox-mode", "workspace-write",
                   "workflow-tool", "--approval-source", "explicit-workflow-command",
                   "--input-json", json.dumps(payload, separators=(",", ":"))]
        known_evaluators = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))
        start = time.monotonic()
        completed = subprocess.run(command, cwd=workspace, env=environment,
                                   capture_output=True, text=True, check=False, timeout=3900)
        elapsed = time.monotonic() - start
        key = environment["DEEPSEEK_API_KEY"]
        (directory / "stdout.log").write_text(completed.stdout.replace(key, "[REDACTED]"), encoding="utf-8")
        (directory / "stderr.log").write_text(completed.stderr.replace(key, "[REDACTED]"), encoding="utf-8")
        evidence = []
        receipt_summaries = []
        for candidate in Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"):
            if candidate in known_evaluators:
                continue
            data = candidate / "manifest.json"
            if data.is_file():
                frozen = json.loads(data.read_text(encoding="utf-8"))
                if frozen.get("baseline_sha") == baseline_sha and frozen.get("objective_sha256"):
                    destination = directory / "evaluator" / candidate.name
                    if not destination.exists():
                        shutil.copytree(candidate, destination, ignore=shutil.ignore_patterns("*-eval", "*-replay"))
                        evidence.append(str(destination))
                        receipt_summaries.append(summarize_receipts(destination))
        row = {"arm": arm, "exit_code": completed.returncode, "wall_seconds": elapsed,
               "binary_sha256": binary_sha256,
               "baseline_sha": baseline_sha, "execution_budget": 1 if arm == "single" else args.budget,
               "budget_plan": plan,
               "initial_policy": initial_policy,
               "transfer_source_evaluation_id": args.transfer_source_evaluation_id,
               "evidence": evidence, "receipt_summaries": receipt_summaries,
               "heldout_replay_pass": any(item["heldout_replay_pass"] for item in receipt_summaries)}
        (directory / "run.json").write_text(json.dumps(row, indent=2) + "\n", encoding="utf-8")
        results.append(row)
        print(json.dumps(row), flush=True)
    (output / "comparison.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
