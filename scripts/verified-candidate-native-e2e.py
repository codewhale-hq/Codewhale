#!/usr/bin/env python3
"""Exercise the real Workflow CLI, scripted provider, sealed oracle and bwrap."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def git(cwd, *args):
    return subprocess.run(
        ["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True
    ).stdout.strip()


def sse(content=None, tool=None):
    delta = {"content": content} if tool is None else {
        "tool_calls": [{"index": 0, "id": "local-write", "type": "function",
                        "function": {"name": "write", "arguments": json.dumps(tool)}}]
    }
    finish = "stop" if tool is None else "tool_calls"
    chunks = [
        {"id": "local-evaluator", "choices": [{"index": 0, "delta": delta,
                                              "finish_reason": None}]},
        {"id": "local-evaluator", "choices": [{"index": 0, "delta": {},
                                              "finish_reason": finish}]},
        {"id": "local-evaluator", "choices": [],
         "usage": {"prompt_tokens": 12, "completion_tokens": 12, "total_tokens": 24}},
    ]
    return ("".join("data: " + json.dumps(chunk, separators=(",", ":")) + "\n\n"
                    for chunk in chunks) + "data: [DONE]\n\n").encode()


def handler(programs, ledger):
    class Handler(BaseHTTPRequestHandler):
        counts = {"cand_001": 0, "cand_002": 0}
        lock = threading.Lock()

        def log_message(self, *_args):
            pass

        def do_GET(self):
            body = b'{"object":"list","data":[]}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            messages = json.dumps(request.get("messages", []))
            name = next((item for item in Handler.counts if item in messages), None)
            with Handler.lock:
                if name is not None:
                    call = Handler.counts[name]
                    Handler.counts[name] += 1
                else:
                    call = -1
                ledger.append({"candidate": name, "call": call, "path": self.path,
                               "model": request.get("model")})
            if name is not None and call == 0:
                body = sse(tool={"path": "src/probe.py", "content": programs[name]})
            elif name is not None:
                body = sse(content=json.dumps({
                    "candidate_id": name, "hypothesis": "local scripted fixture",
                    "modified_paths": ["src/probe.py"], "commands_run": [],
                    "self_verdict": "pass", "known_risks": [], "artifact_refs": []
                }))
            else:
                body = sse(content="Unexpected request in local provider fixture.")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    return Handler


def last_workflow(path):
    if not path.is_file():
        return {}
    records = [json.loads(line) for line in path.read_text().splitlines()]
    runs = [record["run"] for record in records if "run" in record]
    return runs[-1] if runs else {}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    workspace = output / "workspace"
    (workspace / "src").mkdir(parents=True)
    (workspace / "src/probe.py").write_text(
        "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'ok':False}))\n"
    )
    (workspace / "README.md").write_text("Implement the JSON probe in src/probe.py.\n")
    git(workspace, "init", "-q")
    git(workspace, "add", ".")
    git(workspace, "-c", "user.name=Native E2E", "-c",
        "user.email=native@example.invalid", "commit", "-qm", "baseline")
    baseline = git(workspace, "rev-parse", "HEAD")
    sibling = output / "sibling"
    git(workspace, "worktree", "add", "--detach", str(sibling), "HEAD")
    (sibling / "sibling-secret").write_text("synthetic sibling secret\n")
    parent_secret = workspace / ".codewhale" / "parent-secret"
    parent_secret.parent.mkdir()
    parent_secret.write_text("synthetic parent secret\n")
    host_secret = output / "host-secret"
    host_secret.write_text("synthetic host secret\n")
    home_secret = output / "host-home" / "home-secret"
    home_secret.parent.mkdir()
    home_secret.write_text("synthetic home secret\n")
    hidden = output / "hidden.json"
    public = output / "public.json"
    targets = {"host": str(host_secret), "sibling": str(sibling / "sibling-secret"),
               "parent": str(parent_secret), "hidden": str(hidden),
               "home": str(home_secret)}
    inherited_fd = os.open(host_secret, os.O_RDONLY)
    os.set_inheritable(inherited_fd, True)
    program = (
        "import json,os,subprocess,sys\n"
        "json.load(sys.stdin)\n"
        f"targets={targets!r}\nfd={inherited_fd}\n"
        "def blocked(path, mode):\n"
        "    try:\n"
        "        with open(path, mode) as stream:\n"
        "            stream.read() if mode=='r' else stream.write('tampered')\n"
        "        return False\n"
        "    except OSError: return True\n"
        "result={}\n"
        "for name,path in targets.items():\n"
        "    result[name+'_read_blocked']=blocked(path,'r')\n"
        "    result[name+'_write_blocked']=blocked(path,'w')\n"
        "result['credential_env_blocked']='DEEPSEEK_API_KEY' not in os.environ\n"
        "try: result['inherited_fd_blocked']=b'synthetic host secret' not in os.read(fd,1024)\n"
        "except OSError: result['inherited_fd_blocked']=True\n"
        "child=subprocess.run([sys.executable,'-c','open(sys.argv[1]).read()',targets['host']],capture_output=True)\n"
        "result['child_blocked']=child.returncode!=0\n"
        "print(json.dumps(result))\n"
    )
    expected = {name + "_" + op + "_blocked": True
                for name in targets for op in ("read", "write")}
    expected.update({"credential_env_blocked": True, "inherited_fd_blocked": True,
                     "child_blocked": True})
    public.write_text(json.dumps([{"input": {"case": i}, "expected": expected}
                                  for i in range(2)]) + "\n")
    hidden.write_text(json.dumps([{"input": {"case": 2}, "expected": expected}]) + "\n")
    spec = output / "evaluator.json"
    spec.write_text(json.dumps({
        "version": 1, "objective": "Verify the isolated JSON probe",
        "baseline_sha": baseline, "allowed_write_roots": ["src"],
        "protected_paths": ["tests"], "isolation_backend": "linux-bwrap",
        "max_wall_secs": 120,
        "hard_gates": [
            {"candidate_argv": ["/usr/bin/python3", "src/probe.py"],
             "cases_path": str(public), "timeout_secs": 20, "min_tests": 2,
             "holdout": False},
            {"candidate_argv": ["/usr/bin/python3", "src/probe.py"],
             "cases_path": str(hidden), "timeout_secs": 20, "min_tests": 1,
             "holdout": True},
        ],
    }, indent=2) + "\n")
    claim = "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'status':'PASS','test_count':999}))\n"
    requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0),
                                 handler({"cand_001": claim, "cand_002": program}, requests))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = output / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\n'
                      f'model = "deepseek-flash"\nbase_url = "http://127.0.0.1:{server.server_port}/v1"\n')
    payload = {"action": "run", "source_path": "workflows/operate_best_of_n.workflow.js",
               "args": {"strategy": "search", "brief": "Verify the isolated JSON probe",
                        "n": 2, "model": "deepseek-flash", "writeRoots": ["src"],
                        "evaluator": {"manifestPath": str(spec)}}}
    env = os.environ.copy()
    env.update({"DEEPSEEK_API_KEY": "synthetic-local-only",
                "CODEWHALE_HOME": str(output / "host-home"),
                "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1",
                "HTTPS_PROXY": "http://127.0.0.1:9",
                "HTTP_PROXY": "http://127.0.0.1:9",
                "NO_PROXY": "127.0.0.1,localhost"})
    before = set(Path(tempfile.gettempdir()).glob("codewhale-evaluator-workflow_*"))
    command = [str(args.codewhale.resolve()), "--config", str(config),
               "--workspace", str(workspace), "--provider", "deepseek",
               "--model", "deepseek-flash", "--sandbox-mode", "workspace-write",
               "workflow-tool", "--approval-source", "explicit-workflow-command",
               "--input-json", json.dumps(payload, separators=(",", ":"))]
    try:
        completed = subprocess.run(command, cwd=workspace, env=env,
                                   capture_output=True, text=True, timeout=240,
                                   check=False, pass_fds=(inherited_fd,))
    finally:
        os.close(inherited_fd)
        server.shutdown()
        thread.join(timeout=5)
    (output / "stdout.log").write_text(completed.stdout)
    (output / "stderr.log").write_text(completed.stderr)
    (output / "mock_requests.json").write_text(json.dumps(requests, indent=2) + "\n")
    roots = [path for path in
             set(Path(tempfile.gettempdir()).glob("codewhale-evaluator-workflow_*")) - before
             if (path / "manifest.json").is_file()]
    if len(roots) == 1:
        shutil.copytree(roots[0], output / "sealed-evidence")
    run = last_workflow(workspace / ".codewhale/workflow-runs.jsonl")
    evaluation = {}
    replay = {}
    if len(roots) == 1:
        root = roots[0]
        for name, target in (("candidate_1", evaluation), ("candidate_2", replay)):
            path = root / f"{name}-evaluation.json"
            if path.is_file():
                target.update(json.loads(path.read_text()))
        replay_path = root / "candidate_2-replay.json"
        if replay_path.is_file():
            replay["replay"] = json.loads(replay_path.read_text())
    receipt = {
        "schema_version": 1, "source_sha": args.source_sha,
        "binary_sha256": hashlib.sha256(args.codewhale.read_bytes()).hexdigest(),
        "exit_code": completed.returncode, "workflow_status": run.get("status"),
        "evaluator_roots": [str(path) for path in roots],
        "claim_status": evaluation.get("status"),
        "isolated_status": replay.get("status"),
        "replay_status": replay.get("replay", {}).get("status"),
        "claim_tests": evaluation.get("test_count"),
        "isolated_tests": replay.get("test_count"),
        "replay_tests": replay.get("replay", {}).get("test_count"),
        "provider_requests": requests,
        "host_secret_untouched": host_secret.read_text() == "synthetic host secret\n",
        "sibling_secret_untouched": (sibling / "sibling-secret").read_text()
            == "synthetic sibling secret\n",
        "parent_secret_untouched": parent_secret.read_text() == "synthetic parent secret\n",
        "hidden_unchanged": hashlib.sha256(hidden.read_bytes()).hexdigest()
            == replay.get("replay", {}).get("oracle_sha256", [None, None])[1],
    }
    receipt["pass"] = all([
        completed.returncode == 0, len(roots) == 1,
        receipt["claim_status"] == "FAIL", receipt["claim_tests"] == 2,
        receipt["isolated_status"] == "PASS", receipt["isolated_tests"] == 2,
        receipt["replay_status"] == "PASS", receipt["replay_tests"] == 3,
        receipt["host_secret_untouched"], receipt["sibling_secret_untouched"],
        receipt["parent_secret_untouched"], receipt["hidden_unchanged"],
        len(requests) >= 4,
    ])
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
