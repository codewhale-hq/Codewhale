#!/usr/bin/env python3
"""Drive the native Workflow, CAS and sandbox with a local scripted provider."""

import argparse
import json
import os
import runpy
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def sse_text(value):
    return "".join("data: " + json.dumps(chunk, separators=(",", ":")) + "\n\n"
                   for chunk in (
                       {"id": "local-e2e", "choices": [{"index": 0,
                           "delta": {"content": value}, "finish_reason": None}]},
                       {"id": "local-e2e", "choices": [{"index": 0,
                           "delta": {}, "finish_reason": "stop"}]},
                       {"id": "local-e2e", "choices": [],
                           "usage": {"prompt_tokens": 12, "completion_tokens": 12,
                                     "total_tokens": 24}},
                   )) + "data: [DONE]\n\n"


def sse_tool(name, arguments, call_id):
    call = {"index": 0, "id": call_id, "type": "function",
            "function": {"name": name, "arguments": json.dumps(arguments,
                                                               separators=(",", ":"))}}
    return "".join("data: " + json.dumps(chunk, separators=(",", ":")) + "\n\n"
                   for chunk in (
                       {"id": "local-e2e", "choices": [{"index": 0,
                           "delta": {"tool_calls": [call]}, "finish_reason": None}]},
                       {"id": "local-e2e", "choices": [{"index": 0,
                           "delta": {}, "finish_reason": "tool_calls"}]},
                   )) + "data: [DONE]\n\n"


def make_handler(program, requests):
    class Handler(BaseHTTPRequestHandler):
        writer_calls = 0

        def log_message(self, *_args):
            pass

        def do_GET(self):
            payload = json.dumps({"object": "list", "data": []}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(length))
            messages = json.dumps(request.get("messages", []), ensure_ascii=False)
            tools = [tool.get("function", {}).get("name") for tool in request.get("tools", [])]
            if "Independent read-only review" in messages or "Review this verified coding patch" in messages:
                category = "final_review"
                response = sse_text('{"verdict":"pass","reasons":["local isolation fixture"]}')
            elif "Plan one bounded implementation step" in messages:
                category = "planner"
                response = sse_text('{"plan":"Read starter, then write the bounded probe."}')
            elif "You are one isolated Codewhale coding candidate" in messages:
                category = "writer"
                if Handler.writer_calls == 0:
                    response = sse_tool("read", {"path": "src/probe.py"}, "local-read-1")
                elif Handler.writer_calls == 1:
                    response = sse_tool("write", {"path": "src/probe.py", "content": program},
                                        "local-write-1")
                else:
                    response = sse_text("Implemented the probe in the candidate worktree.")
                Handler.writer_calls += 1
            else:
                category = "unexpected"
                response = sse_text("Unexpected model request in local fixture.")
            requests.append({"category": category, "path": self.path,
                             "model": request.get("model"), "tools": tools,
                             "writer_call": Handler.writer_calls,
                             "stream": request.get("stream")})
            payload = response.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    return Handler


def last_workflow(path):
    last = None
    if path.is_file():
        for line in path.read_text(encoding="utf-8").splitlines():
            value = json.loads(line)
            if "run" in value:
                last = value["run"]
    return last


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    helper = runpy.run_path(str(Path(__file__).with_name("experimental-evolution-smoke.py")))
    fixture = output / "fixture"
    source = fixture / "task" / "src" / "probe.py"
    source.parent.mkdir(parents=True)
    source.write_text("import json,sys\nprint(json.dumps({'read_blocked':False,'write_blocked':False}))\n",
                      encoding="utf-8")
    (fixture / "task" / "README.md").write_text(
        "Read src/probe.py, then implement a JSON stdin/stdout program. For each input, "
        "attempt to read and write the host-private marker, and return whether both actions "
        "were blocked. Do not run the program yourself.\n", encoding="utf-8")
    private = output / "host-private"
    private.mkdir()
    marker = private / "marker.txt"
    marker.write_text("synthetic marker\n", encoding="utf-8")
    program = ("import json,sys\n"
               "data=json.load(sys.stdin)\n"
               "read_blocked=False\nwrite_blocked=False\n"
               "try:\n    open(" + repr(str(marker)) + ").read()\n"
               "except OSError:\n    read_blocked=True\n"
               "try:\n    open(" + repr(str(marker)) + ", 'w').write('tampered')\n"
               "except OSError:\n    write_blocked=True\n"
               "print(json.dumps({'read_blocked':read_blocked,'write_blocked':write_blocked}))\n")
    for name, count in (("public_gate.py", 8), ("heldout_gate.py", 6)):
        text = ("import sys\nPROGRAM = (sys.executable, \"src/probe.py\")\n"
                "CASES = [({'case': index}, {'read_blocked': True, 'write_blocked': True}) "
                f"for index in range({count})]\n")
        (fixture / name).write_text(text, encoding="utf-8")
    workspace = output / "workspace"
    helper["make_baseline"](workspace, fixture)
    brief = "Implement the CLI exactly as specified below.\nPUBLIC CONTRACT:\n" \
        + (fixture / "task" / "README.md").read_text(encoding="utf-8") \
        + "\nSTARTER probe.py:\n" + source.read_text(encoding="utf-8")
    policy = {"planning_budget": 1, "max_repair_rounds": 1,
              "extra_verification": False, "candidate_strategy": "direct",
              "trace_preference": "evidence_refs"}
    manifest = helper["write_manifest"](output, workspace, "single", 1, 1,
                                         fixture, brief, policy, 10)
    requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(program, requests))
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
    environment.update({"DEEPSEEK_API_KEY": "local-e2e-only",
                        "CODEWHALE_HOME": str(output / "host-home"),
                        "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1"})
    environment.update({"HTTPS_PROXY": "http://127.0.0.1:9",
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
                                   capture_output=True, text=True, check=False, timeout=180)
    finally:
        server.shutdown()
        thread.join(timeout=5)
    (output / "stdout.log").write_text(completed.stdout, encoding="utf-8")
    (output / "stderr.log").write_text(completed.stderr, encoding="utf-8")
    (output / "mock_requests.json").write_text(json.dumps(requests, indent=2) + "\n",
                                               encoding="utf-8")
    workflow = last_workflow(workspace / ".codewhale" / "workflow-runs.jsonl")
    after = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))
    evaluator = [path for path in after - before if (path / "manifest.json").is_file()]
    evaluation = None
    replay = None
    if len(evaluator) == 1:
        shutil.copytree(evaluator[0], output / "evaluator",
                        ignore=shutil.ignore_patterns("*-eval", "*-replay"))
        evaluation_path = output / "evaluator" / "candidate_g0_1-evaluation.json"
        replay_path = output / "evaluator" / "candidate_g0_1-replay.json"
        if evaluation_path.is_file():
            evaluation = json.loads(evaluation_path.read_text(encoding="utf-8"))
        if replay_path.is_file():
            replay = json.loads(replay_path.read_text(encoding="utf-8"))
    result = (workflow or {}).get("result") or {}
    review = result.get("review") or {}
    identity_bound = bool(evaluation and replay
                          and evaluation.get("artifact_id") == replay.get("artifact_id")
                          and evaluation.get("candidate_tree_hash") == replay.get("candidate_tree_hash")
                          and evaluation.get("review_material_sha256") == replay.get("review_material_sha256")
                          and review.get("artifact_id") == evaluation.get("artifact_id")
                          and review.get("tree_hash") == evaluation.get("candidate_tree_hash")
                          and review.get("review_material_sha256") == evaluation.get("review_material_sha256")
                          and review.get("available_bytes") == evaluation.get("review_material_bytes"))
    receipt = {"schema_version": 1, "scope": "native_workflow_with_local_scripted_provider",
               "exit_code": completed.returncode, "workflow_status": (workflow or {}).get("status"),
               "winner_id": result.get("winner_id"),
               "public_status": (result.get("candidates") or [{}])[0].get("status"),
               "replay_status": (result.get("replay") or {}).get("status"),
               "review_status": (result.get("review") or {}).get("review_status"),
               "review_verdict": (result.get("review") or {}).get("verdict"),
               "review_identity_bound": identity_bound,
               "marker_unchanged": marker.read_text(encoding="utf-8") == "synthetic marker\n",
               "mock_request_count": len(requests), "evaluator_dirs": len(evaluator)}
    receipt["pass"] = (receipt["winner_id"] == "candidate_g0_1"
                       and receipt["public_status"] == "PASS"
                       and receipt["replay_status"] == "PASS"
                       and receipt["review_status"] == "COMPLETE"
                       and receipt["review_verdict"] == "pass"
                       and receipt["marker_unchanged"] and identity_bound
                       and len(evaluator) == 1)
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
