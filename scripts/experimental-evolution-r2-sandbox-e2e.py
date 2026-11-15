#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path


PROGRAM = r'''
import json, os, socket, subprocess, sys, time
mode = sys.argv[1]
if mode == 'paths':
    outside = sys.argv[2]
    read_blocked = False
    write_blocked = False
    try:
        open(outside).read()
    except (OSError, IOError):
        read_blocked = True
    try:
        open(outside, 'w').write('tampered')
    except (OSError, IOError):
        write_blocked = True
    open('inside-scratch', 'w').write('ok')
    print(json.dumps({'read_blocked': read_blocked, 'write_blocked': write_blocked}))
elif mode == 'network':
    try:
        socket.create_connection(('127.0.0.1', int(sys.argv[2])), 1)
        print(json.dumps({'connected': True}))
    except OSError:
        print(json.dumps({'connected': False}))
elif mode == 'private_paths':
    outcomes = {}
    for path in json.loads(sys.argv[2]):
        try:
            open(path).read()
            outcomes[path] = 'VISIBLE'
        except (OSError, IOError):
            outcomes[path] = 'BLOCKED'
    print(json.dumps(outcomes))
elif mode == 'build_hook':
    outside = sys.argv[2]
    hook = "import sys; open(sys.argv[1], 'w').write('hook escaped')"
    child = subprocess.run([sys.executable, '-c', hook, outside],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    print(json.dumps({'hook_exit_code': child.returncode}))
elif mode == 'unix_socket':
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(sys.argv[2])
        print(json.dumps({'connected': True}))
    except OSError:
        print(json.dumps({'connected': False}))
elif mode == 'environment':
    inherited = any(key.startswith('CODEWHALE_SECRET') for key in os.environ)
    fds = os.listdir('/proc/self/fd')
    print(json.dumps({'secret_inherited': inherited, 'fd_count': len(fds)}))
elif mode == 'forge':
    print('PASS\nRan 999 tests in 0.001s')
elif mode == 'descendant':
    if os.fork() == 0:
        os.setsid()
        if os.fork() == 0:
            time.sleep(2)
            open('delayed-marker', 'w').write('escaped')
        os._exit(0)
    time.sleep(10)
'''


def command(source, scratch, mode, *args):
    grants = ["/usr", "/lib", "/lib64", "/etc/alternatives"]
    argv = ["/usr/bin/bwrap", "--unshare-all", "--die-with-parent",
            "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]
    for grant in grants:
        if Path(grant).exists():
            argv.extend(["--ro-bind", grant, grant])
    argv.extend(["--ro-bind", str(source), str(source),
                 "--bind", str(scratch), str(scratch), "--chdir", str(scratch),
                 "--", "/usr/bin/python3", str(source / "candidate.py"), mode,
                 *map(str, args)])
    return argv


def run_case(source, scratch, mode, *args):
    argv = command(source, scratch, mode, *args)
    completed = subprocess.run(argv, env={"PATH": "/usr/bin:/bin", "HOME": str(scratch)},
                               capture_output=True, text=True, timeout=12, check=False)
    return {"argv": argv, "exit_code": completed.returncode,
            "stdout": completed.stdout, "stderr": completed.stderr}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source = output / "sealed-source"
    scratch = output / "scratch"
    private = output / "host-private"
    for path in (source, scratch, private):
        path.mkdir()
    (source / "candidate.py").write_text(PROGRAM, encoding="utf-8")
    outside = private / "marker.txt"
    outside.write_text("synthetic marker\n", encoding="utf-8")
    rows = {}
    rows["paths"] = run_case(source, scratch, "paths", outside)
    rows["paths"]["pass"] = (
        rows["paths"]["exit_code"] == 0
        and json.loads(rows["paths"]["stdout"]) ==
        {"read_blocked": True, "write_blocked": True}
        and outside.read_text(encoding="utf-8") == "synthetic marker\n"
        and (scratch / "inside-scratch").read_text(encoding="utf-8") == "ok"
    )
    private_paths = [private / name for name in
                     ("holdout.json", "candidate-sibling.patch", "parent-checkout.txt",
                      "credentials.env", "receipt.json")]
    for path in private_paths:
        path.write_text("synthetic marker\n", encoding="utf-8")
    rows["private_paths"] = run_case(source, scratch, "private_paths",
                                     json.dumps([str(path) for path in private_paths]))
    rows["private_paths"]["pass"] = (
        rows["private_paths"]["exit_code"] == 0
        and all(value == "BLOCKED" for value in
                json.loads(rows["private_paths"]["stdout"]).values())
    )
    rows["build_hook"] = run_case(source, scratch, "build_hook", outside)
    rows["build_hook"]["pass"] = (
        rows["build_hook"]["exit_code"] == 0
        and json.loads(rows["build_hook"]["stdout"])["hook_exit_code"] != 0
        and outside.read_text(encoding="utf-8") == "synthetic marker\n"
    )
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(2)
    contacted = []

    def accept():
        try:
            connection, _ = listener.accept()
            contacted.append(True)
            connection.close()
        except socket.timeout:
            pass

    thread = threading.Thread(target=accept)
    thread.start()
    rows["network"] = run_case(source, scratch, "network", listener.getsockname()[1])
    thread.join()
    listener.close()
    rows["network"]["pass"] = (
        rows["network"]["exit_code"] == 0
        and json.loads(rows["network"]["stdout"]) == {"connected": False}
        and not contacted
    )
    unix_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    unix_path = private / "local.sock"
    unix_listener.bind(str(unix_path))
    unix_listener.listen(1)
    unix_listener.settimeout(0.5)
    rows["unix_socket"] = run_case(source, scratch, "unix_socket", unix_path)
    try:
        accepted, _ = unix_listener.accept()
        accepted.close()
        unix_contacted = True
    except socket.timeout:
        unix_contacted = False
    unix_listener.close()
    rows["unix_socket"]["pass"] = (
        rows["unix_socket"]["exit_code"] == 0
        and json.loads(rows["unix_socket"]["stdout"]) == {"connected": False}
        and not unix_contacted
    )
    rows["environment"] = run_case(source, scratch, "environment")
    environment = json.loads(rows["environment"]["stdout"])
    rows["environment"]["pass"] = (
        rows["environment"]["exit_code"] == 0
        and environment["secret_inherited"] is False
        and environment["fd_count"] <= 8
    )
    rows["forged_output"] = run_case(source, scratch, "forge")
    try:
        json.loads(rows["forged_output"]["stdout"])
        forged_parsed = True
    except ValueError:
        forged_parsed = False
    rows["forged_output"]["pass"] = (
        rows["forged_output"]["exit_code"] == 0 and not forged_parsed
    )
    argv = command(source, scratch, "descendant")
    started = subprocess.Popen(argv, env={"PATH": "/usr/bin:/bin", "HOME": str(scratch)},
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.6)
    started.kill()
    started.wait(timeout=3)
    time.sleep(2.5)
    rows["descendant"] = {"argv": argv, "outer_exit_code": started.returncode,
                           "delayed_marker": str(scratch / "delayed-marker"),
                           "pass": not (scratch / "delayed-marker").exists()}
    version = subprocess.run(["/usr/bin/bwrap", "--version"], capture_output=True,
                             text=True, check=False).stdout.strip()
    report = {"schema_version": 1, "backend": version,
              "candidate_sha256": hashlib.sha256(PROGRAM.encode()).hexdigest(),
              "case_count": len(rows), "passed_count": sum(row["pass"] for row in rows.values()),
              "cases": rows}
    (output / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"result": str(output / "result.json"),
                      "passed_count": report["passed_count"],
                      "case_count": report["case_count"]}))
    return 0 if report["passed_count"] == report["case_count"] else 1


if __name__ == "__main__":
    sys.exit(main())
