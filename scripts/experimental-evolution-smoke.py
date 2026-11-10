#!/usr/bin/env python3
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "examples" / "experimental-evolution-smoke"
WORKFLOW = ROOT / "workflows" / "operate_best_of_n.workflow.js"
BRIEF = "Implement the Accept-Language negotiation CLI exactly as specified in README.md."


def run(command, **kwargs):
    return subprocess.run(command, check=True, text=True, **kwargs)


def make_baseline(destination):
    destination.mkdir(parents=True)
    shutil.copytree(FIXTURES / "task", destination, dirs_exist_ok=True)
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


def write_manifest(directory, workspace, arm, budget):
    public = (FIXTURES / "public_gate.py").resolve()
    hidden = (FIXTURES / "heldout_gate.py").resolve()
    manifest = {
        "version": 1,
        "objective": BRIEF,
        "public_fixture_path": str((workspace / "README.md").resolve()),
        "hidden_evaluator_path": str(hidden),
        "allowed_write_roots": ["src"],
        "protected_paths": ["README.md", "workflows"],
        "hard_gates": [
            {"argv": [sys.executable, str(public)], "timeout_secs": 60, "min_tests": 8, "holdout": False},
            {"argv": [sys.executable, str(hidden)], "timeout_secs": 60, "min_tests": 6, "holdout": True},
        ],
        "requested_provider": "deepseek",
        "requested_model": "deepseek-flash",
        "resolved_provider": "deepseek",
        "resolved_model": "deepseek-flash",
        "max_candidates": budget,
        "max_generations": 1,
        "max_wall_secs": 3600,
    }
    path = directory / "manifest.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--budget", type=int, default=4)
    parser.add_argument("--arms", nargs="+", choices=["independent"], default=["independent"])
    args = parser.parse_args()
    if not 2 <= args.budget <= 16 or args.budget % 2:
        parser.error("budget must be an even integer from 2 to 16")
    if not args.codewhale.is_file():
        parser.error("--codewhale must name a built binary")
    if not os.environ.get("DEEPSEEK_API_KEY"):
        parser.error("DEEPSEEK_API_KEY must be exported for real provider runs")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    baseline = output / "baseline"
    baseline_sha = make_baseline(baseline)
    config = output / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\nmodel = "deepseek-flash"\n', encoding="utf-8")
    results = []
    for arm in args.arms:
        directory = output / arm
        directory.mkdir()
        workspace = directory / "workspace"
        run(["git", "clone", "-q", "--local", str(baseline), str(workspace)])
        manifest = write_manifest(directory, workspace, arm, args.budget)
        payload = {
            "action": "run",
            "source_path": "workflows/operate_best_of_n.workflow.js",
            "token_budget": args.budget * 30000,
            "args": {
                "strategy": "search",
                "brief": BRIEF,
                "rubric": "Follow README.md. Keep the CLI deterministic and change only src/.",
                "model": "deepseek-flash",
                "n": args.budget // 2,
                "writeRoots": ["src"],
                "evaluator": {"manifestPath": str(manifest)},
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
        row = {"arm": arm, "exit_code": completed.returncode, "wall_seconds": elapsed,
               "baseline_sha": baseline_sha, "execution_budget": args.budget,
               "evidence": evidence}
        (directory / "run.json").write_text(json.dumps(row, indent=2) + "\n", encoding="utf-8")
        results.append(row)
        print(json.dumps(row), flush=True)
    (output / "comparison.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
