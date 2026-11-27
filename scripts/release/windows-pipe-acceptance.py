import argparse
import ctypes
from ctypes import wintypes
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import msvcrt
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import threading
import time


def available(fd):
    peek = ctypes.WinDLL("kernel32", use_last_error=True).PeekNamedPipe
    peek.argtypes = [wintypes.HANDLE, wintypes.LPVOID, wintypes.DWORD,
                     ctypes.POINTER(wintypes.DWORD), ctypes.POINTER(wintypes.DWORD),
                     ctypes.POINTER(wintypes.DWORD)]
    size = wintypes.DWORD()
    if not peek(msvcrt.get_osfhandle(fd), None, 0, None, ctypes.byref(size), None):
        raise ctypes.WinError(ctypes.get_last_error())
    return size.value


def check(exe, case):
    dispatched = threading.Event()
    release = threading.Event()
    prompts = []
    model = "prompt-input-model"

    class Handler(BaseHTTPRequestHandler):
        def send(self, mime, data):
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            self.send("application/json", json.dumps({"object": "list", "data": [
                {"id": model, "object": "model"}]}).encode())

        def do_POST(self):
            if self.path != "/v1/chat/completions":
                self.send_error(404)
                return
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            prompts.extend(m["content"] for m in body["messages"] if m["role"] == "user")
            dispatched.set()
            release.wait(45)
            chunks = [json.dumps({"id": "c1", "object": "chat.completion.chunk", "model": model,
                       "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]})
                      for delta, reason in [({"content": "ok"}, None), ({}, "stop")]]
            self.send("text/event-stream", "".join("data: " + s + "\n\n" for s in chunks + ["[DONE]"]).encode())

    late = b"late producer payload 123\n"
    prompt = "pipe acceptance prompt"
    context = "context from pipe"
    with tempfile.TemporaryDirectory() as td, ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        cfg = Path(td, ".codewhale", "config.toml")
        cfg.parent.mkdir()
        url = f"http://127.0.0.1:{server.server_port}"
        env = dict(os.environ, HOME=td, USERPROFILE=td, APPDATA=td, LOCALAPPDATA=td,
                   XDG_CONFIG_HOME=td, CODEWHALE_CONFIG_PATH=str(cfg), CODEWHALE_BASE_URL=url,
                   DEEPSEEK_BASE_URL=url, DEEPSEEK_API_KEY="ci-test-key-not-real", DEEPSEEK_MODEL=model,
                   CODEWHALE_MODEL=model, RUST_LOG="warn")
        read_fd, write_fd = os.pipe()
        started = time.monotonic()
        proc = subprocess.Popen([exe, "--workspace", td, "--no-project-config", "exec"] +
                                ([] if case == "stdin-only" else [prompt]), cwd=td, env=env,
                                stdin=read_fd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            if case in ("context", "stdin-only"):
                os.write(write_fd, context.encode())
            if case != "late":
                os.close(write_fd)
                write_fd = None
            deadline = started + 60
            while not dispatched.wait(0.05):
                if proc.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("CLI failed to dispatch to loopback")
            dispatch_seconds = time.monotonic() - started
            retained = None
            if case == "late":
                assert dispatch_seconds >= 2, dispatch_seconds
                os.write(write_fd, late)
                time.sleep(1)
                assert proc.poll() is None
                assert available(read_fd) == len(late), "late bytes missing from pipe"
                retained = os.read(read_fd, len(late))
                assert retained == late
            release.set()
            stdout, stderr = proc.communicate(timeout=30)
            assert proc.returncode == 0, (stdout, stderr)
            contents = json.dumps(prompts, ensure_ascii=False)
            expected = {"late": prompt, "eof": prompt, "context": prompt + "\n\n" + context,
                        "stdin-only": context}[case]
            def strings(value):
                if isinstance(value, str):
                    yield value
                elif isinstance(value, dict):
                    for item in value.values():
                        yield from strings(item)
                elif isinstance(value, list):
                    for item in value:
                        yield from strings(item)
            assert any(expected in s for s in strings(prompts)), (expected, contents)
            note = b"stdin produced no data within 2s" in stderr
            assert note == (case == "late"), stderr
            assert late.decode().strip() not in contents
            return dict(case=case, dispatch_seconds=round(dispatch_seconds, 3), timeout_note=note,
                        late_pipe_readback=retained.decode() if retained else None, exit=proc.returncode)
        finally:
            release.set()
            if proc.poll() is None:
                proc.kill()
            out, err = proc.communicate()
            if proc.returncode:
                print(out.decode(errors="replace"), err.decode(errors="replace"), flush=True)
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)
            server.shutdown()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", type=Path)
    exe = parser.parse_args().exe.resolve()
    print(json.dumps(dict(binary_sha256=hashlib.sha256(exe.read_bytes()).hexdigest(),
                          host=platform.platform())), flush=True)
    for case in ("late", "context", "stdin-only", "eof"):
        print(json.dumps(check(exe, case)), flush=True)
