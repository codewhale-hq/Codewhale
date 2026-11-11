#!/usr/bin/env python3
"""Native CAS replay after source deletion, plus sealed manifest/blob tamper rejection."""

import argparse
import hashlib
import json
import os
import runpy
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import ThreadingHTTPServer
from pathlib import Path


def last_run(path):
    if not path.is_file():
        return {}
    values = [json.loads(line)["run"] for line in path.read_text(encoding="utf-8").splitlines()
              if "run" in json.loads(line)]
    return values[-1] if values else {}


def case(root, mode, binary, helper, support):
    root.mkdir()
    fixture = root / "fixture"
    source = fixture / "task" / "src" / "probe.py"
    source.parent.mkdir(parents=True)
    source.write_text("import json,sys\nprint(json.dumps({'ok':False}))\n", encoding="utf-8")
    (fixture / "task" / "README.md").write_text(
        "Implement the JSON stdin/stdout probe in src/probe.py. Return {\"ok\":true}.\n",
        encoding="utf-8")
    for name, count in (("public_gate.py", 8), ("heldout_gate.py", 6)):
        (fixture / name).write_text(
            "import sys\nPROGRAM = (sys.executable, 'src/probe.py')\n"
            f"CASES = [({{'case': index}}, {{'ok': True}}) for index in range({count})]\n",
            encoding="utf-8")
    workspace = root / "workspace"
    helper["make_baseline"](workspace, fixture)
    brief = "Implement the frozen probe in src/probe.py. Do not inspect trusted cases."
    policy = {"planning_budget": 1, "max_repair_rounds": 1,
              "extra_verification": True, "candidate_strategy": "direct",
              "trace_preference": "evidence_refs"}
    manifest = helper["write_manifest"](root, workspace, "single", 1, 1,
                                          fixture, brief, policy,
                                          11 if mode == "retry_dev_review" else 10)
    program = "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'ok':True}))\n"
    before = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))
    intervention = {}

    def on_dev_review():
        try:
            if mode == "retry_dev_review":
                return
            roots = [path for path in
                     set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*")) - before
                     if (path / "artifacts" / "candidate_g0_1.json").is_file()]
            if len(roots) != 1:
                raise ValueError(f"expected one sealed native artifact; found {len(roots)}")
            artifact_path = roots[0] / "artifacts" / "candidate_g0_1.json"
            artifact = json.loads(artifact_path.read_text(encoding="utf-8"))
            if mode == "delete_original":
                listing = subprocess.run(["git", "-C", str(workspace), "worktree", "list",
                                          "--porcelain"], check=True, capture_output=True,
                                         text=True).stdout
                candidates = [Path(line.removeprefix("worktree "))
                              for line in listing.splitlines() if line.startswith("worktree ")
                              and ".codewhale-worktrees" in line]
                if len(candidates) != 1:
                    raise ValueError(f"expected one generated worktree; found {len(candidates)}")
                subprocess.run(["git", "-C", str(workspace), "worktree", "remove", "--force",
                                str(candidates[0])], check=True, capture_output=True, text=True)
                intervention.update({"deleted_worktree": str(candidates[0]),
                                     "original_absent": not candidates[0].exists()})
            else:
                if mode == "tamper_manifest":
                    target = artifact_path
                else:
                    changed = next(entry for entry in artifact["entries"]
                                   if entry["path"] == "src/probe.py")
                    target = roots[0] / "blobs" / changed["blob_hash"]
                old = target.read_bytes()
                target.write_bytes(old + b"\nTAMPERED\n")
                intervention.update({"tampered_path": str(target),
                                     "before_sha256": hashlib.sha256(old).hexdigest(),
                                     "after_sha256": hashlib.sha256(target.read_bytes()).hexdigest()})
        except Exception as error:
            intervention["error"] = str(error)

    requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0),
                                 support["make_handler"](
                                     program, requests, on_dev_review,
                                     tool_only_dev_requests=7 if mode == "retry_dev_review" else 0))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = root / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\n'
                      f'model = "deepseek-flash"\nbase_url = "http://127.0.0.1:{server.server_port}/v1"\n',
                      encoding="utf-8")
    payload = {"action": "run", "source_path": "workflows/operate_best_of_n.workflow.js",
               "args": {"strategy": "search", "brief": brief, "model": "deepseek-flash",
                        "n": 1, "writeRoots": ["src"],
                        "evolution": {"manifestPath": str(manifest), "arm": "single",
                                      "mode": "learn_policy", "executionBudget": 1,
                                      "generations": 1}}}
    environment = os.environ.copy()
    environment.update({"DEEPSEEK_API_KEY": "local-cas-only",
                        "CODEWHALE_HOME": str(root / "host-home"),
                        "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1",
                        "HTTPS_PROXY": "http://127.0.0.1:9",
                        "HTTP_PROXY": "http://127.0.0.1:9",
                        "NO_PROXY": "127.0.0.1,localhost"})
    command = [str(binary), "--config", str(config), "--workspace", str(workspace),
               "--provider", "deepseek", "--model", "deepseek-flash",
               "--sandbox-mode", "workspace-write", "workflow-tool",
               "--approval-source", "explicit-workflow-command", "--input-json",
               json.dumps(payload, separators=(",", ":"))]
    try:
        completed = subprocess.run(command, cwd=workspace, env=environment,
                                   capture_output=True, text=True, check=False, timeout=120)
    finally:
        server.shutdown()
        thread.join(timeout=5)
    (root / "stdout.log").write_text(completed.stdout, encoding="utf-8")
    (root / "stderr.log").write_text(completed.stderr, encoding="utf-8")
    (root / "mock_requests.json").write_text(json.dumps(requests, indent=2) + "\n",
                                             encoding="utf-8")
    roots = [path for path in
             set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*")) - before
             if (path / "manifest.json").is_file()]
    if len(roots) == 1:
        shutil.copytree(roots[0], root / "evaluator",
                        ignore=shutil.ignore_patterns("*-eval", "*-replay"))
    workflow = last_run(workspace / ".codewhale" / "workflow-runs.jsonl")
    outcome = workflow.get("result") or {}
    evaluation_path = root / "evaluator" / "candidate_g0_1-evaluation.json"
    evaluation = json.loads(evaluation_path.read_text(encoding="utf-8")) \
        if evaluation_path.is_file() else {}
    replay_path = root / "evaluator" / "candidate_g0_1-replay.json"
    replay = json.loads(replay_path.read_text(encoding="utf-8")) \
        if replay_path.is_file() else {}
    error_text = str(workflow.get("error") or "") + completed.stdout
    receipt = {"schema_version": 1, "mode": mode, "exit_code": completed.returncode,
               "workflow_status": workflow.get("status"),
               "public_status": evaluation.get("status"),
               "replay_status": replay.get("status"),
               "winner_id": outcome.get("winner_id"),
               "review_status": (outcome.get("review") or {}).get("review_status"),
               "intervention": intervention,
               "native_evaluator_count": len(roots),
               "dev_review_task_labels": [event.get("label") for event in workflow.get("events", [])
                                          if event.get("type") == "task_started"
                                          and str(event.get("label", "")).startswith("dev_review_")],
               "dev_review_requests": sum(item["category"] == "dev_review" for item in requests),
               "final_review_requests": sum(item["category"] == "final_review" for item in requests)}
    if mode == "delete_original":
        receipt["pass"] = (evaluation.get("status") == "PASS"
                           and replay.get("status") == "PASS"
                           and outcome.get("winner_id") == "candidate_g0_1"
                           and outcome.get("promotion") == "proposal_only"
                           and intervention.get("original_absent") is True
                           and "src/generated_note.txt" in replay.get("changed_paths", [])
                           and receipt["dev_review_requests"] == 1)
    elif mode == "retry_dev_review":
        receipt["pass"] = (evaluation.get("status") == "PASS"
                           and replay.get("status") == "PASS"
                           and outcome.get("winner_id") == "candidate_g0_1"
                           and outcome.get("promotion") == "proposal_only"
                           and receipt["review_status"] == "COMPLETE"
                           and receipt["dev_review_task_labels"] == [
                               "dev_review_candidate_g0_1", "dev_review_retry_candidate_g0_1"]
                           and receipt["dev_review_requests"] == 8
                           and receipt["final_review_requests"] == 1)
    else:
        expected = "sealed artifact manifest changed" if mode == "tamper_manifest" \
            else "review blob hash mismatch"
        receipt["pass"] = (evaluation.get("status") == "PASS" and not replay_path.exists()
                           and outcome.get("winner_id") != "candidate_g0_1"
                           and expected in error_text and "error" not in intervention
                           and receipt["final_review_requests"] == 0)
    (root / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                       encoding="utf-8")
    return receipt


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    helper = runpy.run_path(str(Path(__file__).with_name("experimental-evolution-smoke.py")))
    support = runpy.run_path(str(Path(__file__).with_name(
        "experimental-evolution-r3-native-isolation-e2e.py")))
    rows = [case(output / mode, mode, args.codewhale.resolve(), helper, support)
            for mode in ("delete_original", "tamper_manifest", "tamper_blob")]
    summary = {"schema_version": 1, "scope": "native_capture_delete_replay_and_tamper",
               "cases": rows, "pass": all(row["pass"] for row in rows)}
    (output / "receipt.json").write_text(json.dumps(summary, indent=2) + "\n",
                                         encoding="utf-8")
    print(json.dumps(summary))
    return 0 if summary["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
