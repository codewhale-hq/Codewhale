#!/usr/bin/env python3
"""Native G1/G2 checkpoint, process exit, G3 resume, and identity rejections."""

import argparse
import json
import os
import re
import runpy
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).parent
    helper = runpy.run_path(str(source / "experimental-evolution-smoke.py"))
    support = runpy.run_path(str(source / "experimental-evolution-r3-native-isolation-e2e.py"))
    fixture = output / "fixture"
    target = fixture / "task" / "src" / "probe.py"
    target.parent.mkdir(parents=True)
    target.write_text("import json,sys\nprint(json.dumps({'ok':False}))\n", encoding="utf-8")
    (fixture / "task" / "README.md").write_text(
        "Implement src/probe.py as a JSON stdin/stdout program returning {\"ok\":true}.\n",
        encoding="utf-8")
    for gate, count in (("public_gate.py", 8), ("heldout_gate.py", 6)):
        (fixture / gate).write_text(
            "import sys\nPROGRAM = (sys.executable, 'src/probe.py')\n"
            f"CASES = [({{'case': i}}, {{'ok': True}}) for i in range({count})]\n",
            encoding="utf-8")
    workspace = output / "workspace"
    helper["make_baseline"](workspace, fixture)
    brief = "Implement src/probe.py according to the frozen public contract."
    policy = {"planning_budget": 1, "max_repair_rounds": 1,
              "extra_verification": False, "candidate_strategy": "direct",
              "trace_preference": "evidence_refs"}
    manifest = helper["write_manifest"](output, workspace, "evolution", 6, 3,
                                         fixture, brief, policy, 56)
    requests = []

    class Handler(BaseHTTPRequestHandler):
        writer_calls = {}

        def log_message(self, *_args):
            pass

        def do_GET(self):
            payload = b'{"object":"list","data":[]}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            messages = json.dumps(request.get("messages", []), ensure_ascii=False)
            matches = re.findall(r"CANDIDATE ID: (candidate_g[0-2]_[12])", messages)
            candidate = matches[-1] if matches else "NONE"
            if "Independent read-only review" in messages:
                category = "final_review"
                response = support["sse_text"]('{"verdict":"pass","reasons":["resume E2E"]}')
            elif "Review the DEV candidate's complete changed files" in messages:
                category = "dev_review"
                response = support["sse_text"]('{"verdict":"pass","reasons":["resume E2E"]}')
            elif "Plan one bounded implementation step" in messages:
                category = "planner"
                response = support["sse_text"]('{"plan":"Write the frozen probe."}')
            elif "You are one isolated Codewhale coding candidate" in messages:
                category = "writer"
                count = Handler.writer_calls.get(candidate, 0)
                generation = int(candidate.split("_")[1][1:]) if candidate != "NONE" else 0
                changed_path = "src/probe.py" if generation == 0 else "src/generated_note.txt"
                if count == 0:
                    read_path = changed_path if generation >= 2 else "src/probe.py"
                    response = support["sse_tool"]("read", {"path": read_path},
                                                   f"{candidate}-read")
                elif count == 1:
                    response = support["sse_tool"]("write", {"path": changed_path,
                                                       "content": ("import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'ok':True}))\n"
                                                                   if generation == 0 else f"generation={generation} candidate={candidate}\n")},
                                                   f"{candidate}-write")
                else:
                    response = support["sse_text"](f"CHANGES:\n- {changed_path}\n"
                                                   "Implemented the bounded probe.")
                Handler.writer_calls[candidate] = count + 1
            else:
                category = "unexpected"
                response = support["sse_text"]("Unexpected request")
            requests.append({"category": category, "candidate": candidate,
                             "model": request.get("model")})
            payload = response.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = output / "config.toml"
    config.write_text('provider = "deepseek"\n[providers.deepseek]\n'
                      f'model = "deepseek-flash"\nbase_url = "http://127.0.0.1:{server.server_port}/v1"\n',
                      encoding="utf-8")
    environment = os.environ.copy()
    environment.update({"DEEPSEEK_API_KEY": "local-resume-only",
                        "CODEWHALE_HOME": str(output / "host-home"),
                        "CODEWHALE_TELEMETRY": "0", "CODEWHALE_NO_UPDATE_CHECK": "1",
                        "HTTPS_PROXY": "http://127.0.0.1:9", "HTTP_PROXY": "http://127.0.0.1:9",
                        "NO_PROXY": "127.0.0.1,localhost"})
    before = set(Path(tempfile.gettempdir()).glob("codewhale-evolution-workflow_*"))

    def run(evolution, name):
        payload = {"action": "run", "source_path": "workflows/operate_best_of_n.workflow.js",
                   "args": {"strategy": "search", "brief": brief, "model": "deepseek-flash",
                            "n": 2, "writeRoots": ["src"],
                            "evolution": {"manifestPath": str(manifest), "arm": "evolution",
                                          "mode": "learn_policy", "executionBudget": 6,
                                          "generations": 3, **evolution}}}
        command = [str(args.codewhale.resolve()), "--config", str(config),
                   "--workspace", str(workspace), "--provider", "deepseek",
                   "--model", "deepseek-flash", "--sandbox-mode", "workspace-write",
                   "workflow-tool", "--approval-source", "explicit-workflow-command",
                   "--input-json", json.dumps(payload, separators=(",", ":"))]
        completed = subprocess.run(command, cwd=workspace, env=environment,
                                   capture_output=True, text=True, check=False, timeout=250)
        (output / f"{name}.stdout.log").write_text(completed.stdout, encoding="utf-8")
        (output / f"{name}.stderr.log").write_text(completed.stderr, encoding="utf-8")
        return completed

    try:
        first = run({"pauseAfterGeneration": 1}, "before-restart")
        roots = [path for path in set(Path(tempfile.gettempdir()).glob(
            "codewhale-evolution-workflow_*")) - before
            if (path / "checkpoint-g1.json").is_file()]
        if len(roots) != 1:
            raise AssertionError(f"native G2 checkpoint count: {len(roots)}; exit={first.returncode}")
        root = roots[0]
        checkpoint = root / "checkpoint-g1.json"
        saved = json.loads(checkpoint.read_text(encoding="utf-8"))
        if saved.get("next_generation") != 2:
            raise AssertionError("checkpoint did not preserve G3 frontier")
        manifest_path = root / "manifest.json"
        original_manifest = manifest_path.read_bytes()
        negative_rejections = {}
        for field in ("baseline_sha", "hidden_evaluator_sha256",
                      "native_binary_sha256", "shadow_policy"):
            altered = json.loads(original_manifest)
            if field == "shadow_policy":
                altered[field]["planning_budget"] = 2
            else:
                altered[field] = "0" * 64
            manifest_path.write_text(json.dumps(altered), encoding="utf-8")
            count = len(requests)
            rejected = run({"resumeFrom": str(checkpoint)}, f"reject-{field}")
            negative_rejections[field] = rejected.returncode != 0 and len(requests) == count
            manifest_path.write_bytes(original_manifest)
        unknown_work = root / "candidate_g2_1-applied-policy.json"
        unknown_work.write_text("{}\n", encoding="utf-8")
        count = len(requests)
        rejected = run({"resumeFrom": str(checkpoint)}, "reject-unknown-request")
        negative_rejections["unknown_inflight_request"] = rejected.returncode != 0 \
            and len(requests) == count
        unknown_work.unlink()
        before_resume = len(requests)
        second = run({"resumeFrom": str(checkpoint)}, "after-restart")
        after_resume = requests[before_resume:]
        journal = workspace / ".codewhale" / "workflow-runs.jsonl"
        runs = [json.loads(line)["run"] for line in journal.read_text().splitlines()
                if "run" in json.loads(line)]
        result = runs[-1].get("result") or {}
        duplicate = run({"resumeFrom": str(checkpoint)}, "duplicate-resume")
        third_receipts = [json.loads(path.read_text(encoding="utf-8"))
                          for path in sorted(root.glob("candidate_g2_*-evaluation.json"))]
        inherited_genomes = [item.get("shadow_policy") for item in third_receipts]
        receipt = {"schema_version": 1, "first_exit": first.returncode,
                   "resumed_exit": second.returncode, "duplicate_exit": duplicate.returncode,
                   "checkpoint": str(checkpoint), "checkpoint_id": saved.get("checkpoint_id"),
                   "next_generation": saved.get("next_generation"),
                   "negative_rejections": negative_rejections,
                   "first_generation_evaluations": len(list(root.glob("candidate_g[01]_*-evaluation.json"))),
                   "third_generation_evaluations": len(list(root.glob("candidate_g2_*-evaluation.json"))),
                   "third_generation_genomes": inherited_genomes,
                   "resume_requests": after_resume,
                   "winner_id": result.get("winner_id"),
                   "review_status": (result.get("review") or {}).get("review_status"),
                   "promotion": result.get("promotion"),
                   "model_turns_reserved": (result.get("resume_summary") or {}).get("model_turns_reserved")}
        receipt["pass"] = (first.returncode == 0 and second.returncode == 0
                           and duplicate.returncode != 0
                           and receipt["first_generation_evaluations"] == 4
                           and receipt["third_generation_evaluations"] == 2
                           and all(genome.get("max_repair_rounds") == 2
                                   and genome.get("extra_verification") is True
                                   for genome in inherited_genomes)
                           and receipt["model_turns_reserved"] == saved["model_turns_reserved"]
                           and all(negative_rejections.values())
                           and all(item["candidate"] in ("candidate_g2_1", "candidate_g2_2", "NONE")
                                   for item in after_resume)
                           and result.get("winner_id") != "NONE"
                           and receipt["review_status"] == "COMPLETE"
                           and receipt["promotion"] == "proposal_only")
        shutil.copytree(root, output / "evaluator",
                        ignore=shutil.ignore_patterns("*-eval", "*-replay"))
        (output / "mock_requests.json").write_text(json.dumps(requests, indent=2) + "\n",
                                                      encoding="utf-8")
        (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                              encoding="utf-8")
        print(json.dumps(receipt))
        return 0 if receipt["pass"] else 1
    finally:
        server.shutdown()
        thread.join(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
