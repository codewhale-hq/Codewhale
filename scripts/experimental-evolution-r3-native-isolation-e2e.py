#!/usr/bin/env python3
"""Drive the native Workflow, CAS and sandbox with a local scripted provider."""

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


def make_handler(program, requests, on_dev_review=None):
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
            elif "Review the DEV candidate's complete changed files" in messages:
                category = "dev_review"
                if on_dev_review is not None:
                    on_dev_review()
                response = sse_text('{"verdict":"pass","reasons":["local DEV fixture"]}')
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
                elif Handler.writer_calls == 2:
                    response = sse_tool("write", {"path": "src/generated_note.txt",
                                                  "content": "new sealed file\n"}, "local-write-2")
                else:
                    response = sse_text("CHANGES:\n- src/probe.py\n- src/generated_note.txt\n"
                                        "Implemented the probe in the candidate worktree.")
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
    source.write_text("import json,sys\nprint(json.dumps({'read_blocked':False}))\n",
                      encoding="utf-8")
    (fixture / "task" / "README.md").write_text(
        "Read src/probe.py, then implement a JSON stdin/stdout program. For each input, "
        "attempt to read and write the host-private marker, a sibling candidate worktree, "
        "and hidden gate material. Repeat the host marker checks in a child process. "
        "Return whether every attempt was blocked. Do not run the program yourself.\n",
        encoding="utf-8")
    private = output / "host-private"
    private.mkdir()
    marker = private / "marker.txt"
    marker.write_text("synthetic marker\n", encoding="utf-8")
    sibling = output / "sibling-candidate"
    sibling_marker = sibling / "other-candidate-secret.txt"
    hidden_gate = fixture / "heldout_gate.py"
    targets = {"host": str(marker), "sibling": str(sibling_marker),
               "hidden": str(hidden_gate)}
    program = ("import json,subprocess,sys\n"
               "json.load(sys.stdin)\n"
               f"targets = {targets!r}\n"
               "def blocked(path, mode):\n"
               "    try:\n"
               "        with open(path, mode) as stream:\n"
               "            stream.read() if mode == 'r' else stream.write('tampered')\n"
               "        return False\n"
               "    except OSError:\n"
               "        return True\n"
               "result = {}\n"
               "for label, path in targets.items():\n"
               "    result[label + '_read_blocked'] = blocked(path, 'r')\n"
               "    result[label + '_write_blocked'] = blocked(path, 'w')\n"
               "for mode, action in [('r', 'read()'), ('w', \"write('tampered')\")]:\n"
               "    child = subprocess.run([sys.executable, '-c', "
               "f\"open(sys.argv[1], '{mode}').{action}\", targets['host']], "
               "capture_output=True, timeout=3)\n"
               "    result['child_' + mode + '_blocked'] = child.returncode != 0\n"
               "print(json.dumps(result))\n")
    expected = {label + "_" + action + "_blocked": True
                for label in ("host", "sibling", "hidden")
                for action in ("read", "write")}
    expected.update({"child_r_blocked": True, "child_w_blocked": True})
    for name, count in (("public_gate.py", 8), ("heldout_gate.py", 6)):
        text = ("import sys\nPROGRAM = (sys.executable, \"src/probe.py\")\n"
                f"CASES = [({{'case': index}}, {expected!r}) "
                f"for index in range({count})]\n")
        (fixture / name).write_text(text, encoding="utf-8")
    hidden_gate_hash = hashlib.sha256(hidden_gate.read_bytes()).hexdigest()
    workspace = output / "workspace"
    helper["make_baseline"](workspace, fixture)
    subprocess.run(["git", "-C", str(workspace), "worktree", "add", "--detach",
                    str(sibling), "HEAD"], check=True, capture_output=True, text=True)
    sibling_marker.write_text("other candidate secret\n", encoding="utf-8")
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
    artifact = None
    new_file_replayed = False
    if len(evaluator) == 1:
        original_replayed = evaluator[0] / "candidate_g0_1-replay" / "src" / "generated_note.txt"
        if original_replayed.is_file():
            new_file_replayed = original_replayed.read_bytes() == b"new sealed file\n"
        shutil.copytree(evaluator[0], output / "evaluator",
                        ignore=shutil.ignore_patterns("*-eval", "*-replay"))
        evaluation_path = output / "evaluator" / "candidate_g0_1-evaluation.json"
        replay_path = output / "evaluator" / "candidate_g0_1-replay.json"
        if evaluation_path.is_file():
            evaluation = json.loads(evaluation_path.read_text(encoding="utf-8"))
        if replay_path.is_file():
            replay = json.loads(replay_path.read_text(encoding="utf-8"))
        artifact_path = output / "evaluator" / "artifacts" / "candidate_g0_1.json"
        if artifact_path.is_file():
            artifact = json.loads(artifact_path.read_text(encoding="utf-8"))
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
               "promotion": result.get("promotion"),
               "review_identity_bound": identity_bound,
               "marker_unchanged": marker.read_text(encoding="utf-8") == "synthetic marker\n",
               "sibling_unchanged": sibling_marker.read_text(encoding="utf-8") == "other candidate secret\n",
               "hidden_gate_unchanged": hashlib.sha256(hidden_gate.read_bytes()).hexdigest()
                                        == hidden_gate_hash,
               "mock_request_count": len(requests), "evaluator_dirs": len(evaluator)}
    receipt["new_file_sealed"] = bool(artifact and any(
        entry.get("path") == "src/generated_note.txt" for entry in artifact.get("entries", [])))
    receipt["new_file_replayed"] = new_file_replayed
    receipt["pass"] = (receipt["winner_id"] == "candidate_g0_1"
                       and receipt["public_status"] == "PASS"
                       and receipt["replay_status"] == "PASS"
                       and receipt["review_status"] == "COMPLETE"
                       and receipt["review_verdict"] == "pass"
                       and receipt["promotion"] == "proposal_only"
                       and receipt["marker_unchanged"] and receipt["sibling_unchanged"]
                       and receipt["hidden_gate_unchanged"] and identity_bound
                       and receipt["new_file_sealed"] and receipt["new_file_replayed"]
                       and len(evaluator) == 1)
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
