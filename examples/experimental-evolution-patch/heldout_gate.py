import json
import subprocess
import sys


CASES = [
    ({"document": {"a/b": {"~": [1, 2]}},
      "operations": [{"op": "copy", "from": "/a~1b/~0/1", "path": "/a~1b/copied"}]},
     {"ok": True, "document": {"a/b": {"~": [1, 2], "copied": 2}}}),
    ({"document": {"a": {"n": [1]}, "b": {}},
      "operations": [{"op": "copy", "from": "/a", "path": "/b/c"},
                     {"op": "add", "path": "/b/c/n/-", "value": 2}]},
     {"ok": True, "document": {"a": {"n": [1]}, "b": {"c": {"n": [1, 2]}}}}),
    ({"document": {"a": 1, "b": 2},
      "operations": [{"op": "move", "from": "/a", "path": "/b"}]},
     {"ok": True, "document": {"b": 1}}),
    ({"document": [0, 1, 2, 3],
      "operations": [{"op": "move", "from": "/1", "path": "/3"}]},
     {"ok": True, "document": [0, 2, 3, 1]}),
    ({"document": {"x": [False, {"a": 1}]},
      "operations": [{"op": "test", "path": "/x", "value": [False, {"a": 1.0}]}]},
     {"ok": True, "document": {"x": [False, {"a": 1}]}}),
    ({"document": {"a": 1},
      "operations": [{"op": "add", "path": "/b", "value": 2},
                     {"op": "remove", "path": "/a~3"}]},
     {"ok": False, "error": {"index": 1, "code": "invalid_pointer"}}),
]


def main():
    failures = []
    for index, (request, expected) in enumerate(CASES, 1):
        completed = subprocess.run([sys.executable, "src/patch_cli.py"],
                                   input=json.dumps(request), text=True,
                                   capture_output=True, timeout=5, check=False)
        try:
            actual = json.loads(completed.stdout)
        except (ValueError, TypeError):
            actual = completed.stdout.strip()
        if completed.returncode != 0 or actual != expected:
            failures.append((index, expected, actual, completed.returncode, completed.stderr[-300:]))
    print(f"Ran {len(CASES)} tests in process-level held-out CLI gate")
    for index, expected, actual, code, stderr in failures:
        print(f"FAIL: case {index}: expected {expected!r}, got {actual!r}, exit={code}, stderr={stderr!r}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
