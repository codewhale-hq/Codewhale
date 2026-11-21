#!/usr/bin/env python3
import argparse
import ctypes
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import secrets
import socket
import subprocess
import sys
import traceback
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlencode

parser = argparse.ArgumentParser()
parser.add_argument("--engine", required=True, type=Path)
parser.add_argument("--sha", required=True)
parser.add_argument("--agent", required=True, type=Path)
parser.add_argument("--out", required=True, type=Path)
args = parser.parse_args()
engine, agent, out = args.engine.resolve(), args.agent.resolve(), args.out.resolve()
out.mkdir(mode=0o700)
for name in ("home", "tmp", "config", "cache", "data", "state", "runtime", "work"):
    (out / name).mkdir(mode=0o700)
keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE"}
env = {name: value for name, value in os.environ.items() if name.upper() in keep}
env.update({
    "PATH": str(agent.parent) + os.pathsep + os.environ["PATH"],
    "HOME": str(out / "home"), "USERPROFILE": str(out / "home"),
    "TMP": str(out / "tmp"), "TEMP": str(out / "tmp"), "TMPDIR": str(out / "tmp"),
    "APPDATA": str(out / "config"), "LOCALAPPDATA": str(out / "data"),
    "XDG_CONFIG_HOME": str(out / "config"), "XDG_CACHE_HOME": str(out / "cache"),
    "XDG_DATA_HOME": str(out / "data"), "XDG_STATE_HOME": str(out / "state"),
    "CODEWHALE_HOME": str(out / "runtime"), "CODEWHALE_TELEMETRY": "0",
    "CODEWHALE_NO_UPDATE_CHECK": "1", "CODEWHALE_DISABLE_MODELS_DEV_FETCH": "1",
    "OPENCODE_DISABLE_AUTOUPDATE": "true", "OPENCODE_DISABLE_LSP_DOWNLOAD": "true",
    "OPENCODE_DISABLE_MODELS_FETCH": "true", "HTTP_PROXY": "http://127.0.0.1:9",
    "HTTPS_PROXY": "http://127.0.0.1:9", "NO_PROXY": "localhost,127.0.0.1",
})
os.environ.clear()
os.environ.update(env)

def write(name, data):
    (out / name).write_text(json.dumps(data, indent=2) + "\n")

def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def wait_for(label, probe, limit=25):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        if value := probe():
            return value
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {label}")

def socket_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]

def stop_process(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=12)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

selected = "chosen"
default = "default-model"
provider = "qualification"
expected_note = "OpenCode ACP fixture write\n"
work = out / "work"
engine_home = Path(os.environ["CODEWHALE_HOME"])
requests = []
calls = {}
script = {
    "CW_ALLOW_WRITE": ("write", {"filePath": str(work / "allowed.txt"), "content": expected_note}),
    "CW_ALLOW_SHELL": ("bash", {
        "command": """python -c "import os,pathlib; assert not any(os.getenv(k) for k in ('CODEWHALE_RUNTIME_TOKEN','OPENAI_API_KEY','CW_ACP_CANARY')); p=pathlib.Path('allowed.txt'); pathlib.Path('shell-receipt.txt').write_bytes(p.read_bytes()); print(p.read_text())" """.strip(),
        "description": "Read the fixture file and check synthetic environment canaries",
        "workdir": str(work),
    }),
    "CW_DENY": ("write", {"filePath": str(work / "denied.txt"), "content": "would be denied\n"}),
    "CW_INTERRUPT": ("bash", {
        "command": """python -c "from pathlib import Path; Path('interrupted.txt').touch()" """,
        "description": "Create the interrupt sentinel",
        "workdir": str(work),
    }),
    "CW_REOPEN": None,
}

class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        assert self.path == "/v1/chat/completions", self.path
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        messages = payload["messages"]
        prompt = next((str(message["content"]) for message in reversed(messages) if message["role"] == "user"), "")
        phase = next((name for name in script if name in prompt), "unexpected")
        tools = [entry["function"]["name"] for entry in payload.get("tools", []) if entry["type"] == "function"]
        tool_result = next((str(message.get("content", "")) for message in reversed(messages)
                            if message["role"] == "tool"), "")
        requests.append({"phase": phase, "model": payload["model"], "tool_names": tools,
                         "tool_result": tool_result[:250]})
        key = (phase, payload["model"])
        count = calls.get(key, 0)
        calls[key] = count + 1

        def chunk(delta, finish=None):
            value = {"id": f"fixture-{phase}", "object": "chat.completion.chunk", "created": 1,
                     "model": payload["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            return "data: " + json.dumps(value) + "\n\n"

        tool = script.get(phase)
        if tool and count == 0 and tool[0] in tools:
            name, arguments = tool
            delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": f"call_{phase}",
                     "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            response = chunk(delta) + chunk({}, "tool_calls")
        else:
            response = chunk({"role": "assistant", "content": f"Fixture reply {phase}"}) + chunk({}, "stop")

        body = (response + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

token = secrets.token_hex(24)
server = None
result = {"candidate_sha": args.sha, "agent_invocation": [str(agent), "acp"],
          "host": platform.platform(), "inference": "scripted loopback Chat Completions"}
model = ThreadingHTTPServer(("127.0.0.1", 0), ModelHandler)
model.daemon_threads = True
threading.Thread(target=model.serve_forever, daemon=True).start()
port = 0

def api(method, path, body=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
    try:
        connection.request(method, path, None if body is None else json.dumps(body),
                           {"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
        response = connection.getresponse()
        raw = response.read()
        try:
            value = json.loads(raw)
        except ValueError:
            value = raw.decode(errors="replace")
        assert response.status < 300, (method, path, response.status, value)
        return value
    finally:
        connection.close()

def start_engine(log):
    global port
    port = socket_port()
    env = dict(os.environ, CODEWHALE_RUNTIME_TOKEN=token, OPENAI_API_KEY="synthetic-cw-fixture-key",
               CW_ACP_CANARY="synthetic-foreign-agent-canary")
    with (out / log).open("w") as stream:
        process = subprocess.Popen([engine, "app-server", "--http", "--host", "127.0.0.1",
                                    "--port", str(port)], cwd=work, env=env,
                                   stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.STDOUT)

    def healthy():
        assert process.poll() is None, (log, process.returncode)
        try:
            return api("GET", "/health")
        except OSError:
            return None

    try:
        wait_for("Runtime health", healthy, limit=15)
    except BaseException:
        stop_process(process)
        raise
    return process

def installed_agent_pids(parent_pid):
    if os.name == "nt":
        expression = f"Get-CimInstance Win32_Process -Filter 'ParentProcessId={parent_pid}' | Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress -AsArray"
        raw = subprocess.check_output(["pwsh", "-NoProfile", "-Command", expression], text=True).strip()
        return [item["ProcessId"] for item in json.loads(raw or "[]")
                if item["ExecutablePath"] and Path(item["ExecutablePath"]).resolve() == agent]
    matches = []
    for proc in Path("/proc").glob("[0-9]*"):
        try:
            status = (proc / "status").read_text()
            if f"PPid:\t{parent_pid}\n" in status and (proc / "exe").resolve() == agent:
                matches.append(int(proc.name))
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
    return matches

def pid_gone(pid):
    if os.name == "nt":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.restype = ctypes.c_void_p
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            return ctypes.get_last_error() == 87
        status = ctypes.c_ulong()
        try:
            return bool(kernel.GetExitCodeProcess(ctypes.c_void_p(handle), ctypes.byref(status))) and status.value != 259
        finally:
            kernel.CloseHandle(ctypes.c_void_p(handle))
    try:
        return (Path("/proc") / str(pid) / "stat").read_text().split(") ")[-1].startswith("Z ")
    except FileNotFoundError:
        return True

def approval_probe(get_detail):
    detail = get_detail()
    if detail["pending_approvals"]:
        return detail["pending_approvals"][0]
    turn = detail["turns"][-1]
    assert turn["status"] == "in_progress", ("Turn ended before approval", turn, requests[-2:])
    return None

try:
    result["network_guard"] = "isolated Linux network namespace" if os.name != "nt" else "scoped Windows outbound firewall rules"
    result["engine_version"] = subprocess.check_output([engine, "--version"], text=True, timeout=10).strip()
    assert len(args.sha) == 40 and f"({args.sha[:12]})" in result["engine_version"], result["engine_version"]
    result["engine_sha256"] = digest(engine)
    result["agent_version"] = subprocess.check_output([agent, "--version"], text=True, timeout=10).strip()
    assert result["agent_version"] == "1.19.6", result["agent_version"]
    result["agent_sha256"] = digest(agent)
    (engine_home / "config.toml").write_text("telemetry = false\n")
    config = {
        "autoupdate": False,
        "model": f"{provider}/{default}",
        "small_model": f"{provider}/{default}",
        "enabled_providers": [provider],
        "provider": {provider: {"npm": "@ai-sdk/openai-compatible", "name": "Qualification fixture",
                      "options": {"baseURL": f"http://127.0.0.1:{model.server_port}/v1", "apiKey": "fixture-only"},
                      "models": {name: {"name": name} for name in (selected, default)}}},
        "permission": {"edit": "ask", "bash": "ask"},
    }
    (work / "opencode.json").write_text(json.dumps(config, indent=2) + "\n")
    server = start_engine("engine-initial.log")
    roster = api("GET", "/v1/agents/external")
    entry = next(item for item in roster["agents"] if item["id"] == "opencode")
    write("discovery.json", entry)
    assert entry["runnable"] is True, entry
    options = api("GET", "/v1/agents/external/opencode/options?" + urlencode({"cwd": str(work)}))
    write("options.json", options)
    assert options["agent"]["protocol_version"] == 1, options
    assert f"{provider}/{selected}" in json.dumps(options), options
    created = api("POST", "/v1/threads", {"workspace": str(work), "permission_posture": "ask",
                  "executor": {"kind": "acp", "agent_id": "opencode", "config": {"model": f"{provider}/{selected}"}}})
    thread = f"/v1/threads/{created['id']}"

    try:
        api("POST", thread + "/turns", {"prompt": "unsupported model control", "model": "other"})
        raise RuntimeError("unexpectedly admitted unsupported control")
    except AssertionError as refusal:
        _, _, code, response = refusal.args[0]
        assert (code, response["error"]["code"]) == (424, "unsupported_control"), refusal
        result["unsupported_control"] = {"http": code, "code": response["error"]["code"]}

    def detail():
        return api("GET", thread)

    def begin(phase):
        def admit():
            try:
                return api("POST", thread + "/turns", {"prompt": phase})["turn"]["id"]
            except AssertionError as error:
                context = error.args[0]
                if isinstance(context, tuple) and context[2] == 409 and context[3]["error"]["message"] == "Thread already has an active turn":
                    return None
                raise
        return wait_for(phase + " admission", admit)

    def settle(phase, status):
        def settled():
            snapshot = detail()
            turn = snapshot["turns"][-1]
            return snapshot if turn["input_summary"] == phase and turn["status"] != "in_progress" else None
        snapshot = wait_for(f"{phase} settled", settled)
        write(phase + ".json", snapshot)
        assert snapshot["turns"][-1]["status"] == status, snapshot["turns"][-1]
        assert not snapshot["pending_approvals"], snapshot["pending_approvals"]
        return snapshot

    def decide(phase, decision, absent=None):
        begin(phase)
        approval = wait_for(f"{phase} approval", lambda: approval_probe(detail), limit=45)
        if absent:
            assert not (work / absent).exists(), absent
        delivered = api("POST", "/v1/approvals/" + approval["id"], {"decision": decision})
        assert delivered["delivered"], delivered
        return settle(phase, "completed")

    allowed = decide("CW_ALLOW_WRITE", "allow", "allowed.txt")
    assert (work / "allowed.txt").read_text() == expected_note
    result["allow_write"] = "durable approval observed before change; file contents verified"
    foreign_session = allowed["thread"]["executor"]["foreign_session_id"]
    assert foreign_session
    decide("CW_ALLOW_SHELL", "allow")
    assert (work / "shell-receipt.txt").read_text() == expected_note
    result["allow_shell"] = "approved shell copied the expected fixture contents; isolated shell did not see injected canaries"
    decide("CW_DENY", "deny", "denied.txt")
    assert not (work / "denied.txt").exists()
    result["deny_write"] = "no file before or after denial"

    turn_id = begin("CW_INTERRUPT")
    wait_for("interrupt approval", lambda: approval_probe(detail))
    assert not (work / "interrupted.txt").exists()
    api("POST", thread + "/turns/" + turn_id + "/interrupt", {})
    try:
        settle("CW_INTERRUPT", "interrupted")
        assert not (work / "interrupted.txt").exists()
        result["interrupt"] = "pending approval cleared with no sentinel"
    except AssertionError as interrupt_error:
        result["interrupt_failure"] = repr(interrupt_error)
        result["interrupt_sentinel_exists"] = (work / "interrupted.txt").exists()

    original_pids = installed_agent_pids(server.pid)
    assert original_pids, "No installed OpenCode process was identified."
    stop_process(server)
    server = None
    try:
        wait_for("original OpenCode process exit", lambda: all(pid_gone(pid) for pid in original_pids))
        result["process_teardown"] = True
    except AssertionError:
        result["process_teardown"] = False
        for pid in original_pids:
            if not pid_gone(pid):
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], check=True)
                else:
                    os.kill(pid, 15)
    server = start_engine("engine-reopen.log")
    api("POST", thread + "/resume", {})
    begin("CW_REOPEN")
    reopened = settle("CW_REOPEN", "completed")
    assert reopened["thread"]["executor"]["foreign_session_id"] == foreign_session
    result["same_foreign_session_reopened"] = True

    journal = next(engine_home.rglob(f"{created['id']}.jsonl"))
    events = [json.loads(line) for line in journal.read_text().splitlines()]
    sequences = [event["seq"] for event in events]
    assert sequences == sorted(set(sequences)) and len(sequences) > 5, sequences
    result["durable_journal_sequences"] = len(sequences)
    observed = {item["model"] for item in requests}
    assert selected in observed, observed
    for name in ("CW_ALLOW_WRITE", "CW_ALLOW_SHELL", "CW_DENY", "CW_INTERRUPT", "CW_REOPEN"):
        assert any(item["phase"] == name and item["model"] == selected for item in requests), name
    result["explicit_route"] = f"{provider}/{selected} observed in all five fixture phases"
    interrupted_before = json.loads((out / "CW_INTERRUPT.json").read_text())
    result["cancellation"] = {
        "live": next(t["status"] for t in interrupted_before["turns"] if t["id"] == turn_id),
        "restored": next(t["status"] for t in reopened["turns"] if t["id"] == turn_id),
        "pending_after_restart": len(reopened["pending_approvals"]),
        "sentinel_after_restart": (work / "interrupted.txt").exists()
    }
    assert result["process_teardown"], "external process outlived Engine"
    assert result["cancellation"]["restored"] == "interrupted", result["cancellation"]
    assert "interrupt_failure" not in result, "turn interrupt acceptance failed; see interrupt_failure"
    result["status"] = "pass"
except BaseException as error:
    result["status"] = "fail"
    result["error"] = repr(error)
    result["traceback"] = traceback.format_exc()
    raise
finally:
    if server is not None:
        stop_process(server)
    model.shutdown()
    model.server_close()
    write("model-requests.json", requests)
    write("receipt.json", result)
