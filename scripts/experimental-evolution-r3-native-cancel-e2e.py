#!/usr/bin/env python3
"""Exercise a timed-out candidate's detached grandchild through native Workflow."""

import argparse
import json
import os
import runpy
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    support = runpy.run_path(str(Path(__file__).with_name(
        "experimental-evolution-r3-native-isolation-e2e.py")))
    helper = runpy.run_path(str(Path(__file__).with_name("experimental-evolution-smoke.py")))
    fixture = output / "fixture"
    source = fixture / "task" / "src" / "probe.py"
    source.parent.mkdir(parents=True)
    source.write_text("import json,sys\nprint(json.dumps({'ok':False}))\n", encoding="utf-8")
    (fixture / "task" / "README.md").write_text(
        "Write a JSON stdin/stdout probe in src/probe.py. The host owns the evaluation.\n",
        encoding="utf-8")
    for name, count in (("public_gate.py", 8), ("heldout_gate.py", 6)):
        (fixture / name).write_text(
            "import sys\nPROGRAM = (sys.executable, 'src/probe.py')\n"
            f"CASES = [({{'case': index}}, {{'ok': True}}) for index in range({count})]\n",
            encoding="utf-8")
    workspace = output / "workspace"
    helper["make_baseline"](workspace, fixture)
    brief = "Implement the local probe in src/probe.py. Do not run it yourself."
    policy = {"planning_budget": 1, "max_repair_rounds": 1,
              "extra_verification": False, "candidate_strategy": "direct",
              "trace_preference": "evidence_refs"}
    manifest = helper["write_manifest"](output, workspace, "single", 1, 1,
                                          fixture, brief, policy, 10)
    child = "import time;time.sleep(12);open('delayed-marker','w').write('escaped')"
    program = ("import json,subprocess,sys,time\n"
               "json.load(sys.stdin)\n"
               "open('spawned-marker','w').write('started')\n"
               f"subprocess.Popen([sys.executable, '-c', {child!r}], "
               "start_new_session=True, stdin=subprocess.DEVNULL, "
               "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
               "time.sleep(30)\n")
    requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), support["make_handler"](program, requests))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = output / "config.toml"
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
    environment.update({"DEEPSEEK_API_KEY": "local-cancel-only",
                        "CODEWHALE_HOME": str(output / "host-home"),
                        "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1",
                        "HTTPS_PROXY": "http://127.0.0.1:9",
                        "HTTP_PROXY": "http://127.0.0.1:9",
                        "NO_PROXY": "127.0.0.1,localhost"})
    before = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))
    command = [str(args.codewhale.resolve()), "--config", str(config),
               "--workspace", str(workspace), "--provider", "deepseek",
               "--model", "deepseek-flash", "--sandbox-mode", "workspace-write",
               "workflow-tool", "--approval-source", "explicit-workflow-command",
               "--input-json", json.dumps(payload, separators=(",", ":"))]
    try:
        completed = subprocess.run(command, cwd=workspace, env=environment,
                                   capture_output=True, text=True, check=False, timeout=120)
    finally:
        server.shutdown()
        thread.join(timeout=5)
    (output / "stdout.log").write_text(completed.stdout, encoding="utf-8")
    (output / "stderr.log").write_text(completed.stderr, encoding="utf-8")
    (output / "mock_requests.json").write_text(json.dumps(requests, indent=2) + "\n",
                                               encoding="utf-8")
    workflow = support["last_workflow"](workspace / ".codewhale" / "workflow-runs.jsonl")
    after = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))
    evaluator = [path for path in after - before if (path / "manifest.json").is_file()]
    time.sleep(5)
    if len(evaluator) == 1:
        shutil.copytree(evaluator[0], output / "evaluator")
    scratch = output / "evaluator" / "candidate_g0_1-eval-g0-c0-scratch"
    evaluation_path = output / "evaluator" / "candidate_g0_1-evaluation.json"
    evaluation = json.loads(evaluation_path.read_text(encoding="utf-8")) \
        if evaluation_path.is_file() else {}
    outcome = (workflow or {}).get("result") or {}
    receipt = {"schema_version": 1, "scope": "native_workflow_detached_child_timeout",
               "workflow_status": (workflow or {}).get("status"),
               "candidate_status": evaluation.get("status"),
               "execution_status": evaluation.get("execution_status"),
               "winner_id": outcome.get("winner_id"),
               "spawned_marker": (scratch / "spawned-marker").is_file(),
               "delayed_marker": (scratch / "delayed-marker").is_file(),
               "evaluator_dirs": len(evaluator), "mock_request_count": len(requests)}
    receipt["pass"] = (receipt["candidate_status"] == "UNKNOWN"
                       and receipt["execution_status"] == "STOPPED"
                       and receipt["winner_id"] == "NONE"
                       and receipt["spawned_marker"] and not receipt["delayed_marker"]
                       and len(evaluator) == 1)
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                         encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
