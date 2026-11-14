import json
import subprocess
import sys


def ok(document):
    return {"ok": True, "document": document}


def err(index, code):
    return {"ok": False, "error": {"index": index, "code": code}}


def task(document, *operations):
    return {"document": document, "operations": list(operations)}


CASES = [
    (task({"a": 1}, {"op": "add", "path": "/b", "value": 2}), ok({"a": 1, "b": 2})),
    (task({"a": 1}, {"op": "add", "path": "/a", "value": 2}), ok({"a": 2})),
    (task(["a", "c"], {"op": "add", "path": "/1", "value": "b"}), ok(["a", "b", "c"])),
    (task([1], {"op": "add", "path": "/-", "value": 2}), ok([1, 2])),
    (task({"a": 1, "b": 2}, {"op": "remove", "path": "/a"}), ok({"b": 2})),
    (task([1, 2, 3], {"op": "remove", "path": "/1"}), ok([1, 3])),
    (task({"a": 1}, {"op": "replace", "path": "/a", "value": 7}), ok({"a": 7})),
    (task([1, 2], {"op": "replace", "path": "/0", "value": 9}), ok([9, 2])),
    (task({"x": [1, 2]}, {"op": "test", "path": "/x", "value": [1, 2]}), ok({"x": [1, 2]})),
    (task({"x": 1}, {"op": "test", "path": "/x", "value": 2}), err(0, "failed_test")),
    (task({"x": {"a": 1}}, {"op": "copy", "from": "/x", "path": "/y"}),
     ok({"x": {"a": 1}, "y": {"a": 1}})),
    (task({"x": {"a": 1}}, {"op": "copy", "from": "/x", "path": "/y"},
          {"op": "replace", "path": "/y/a", "value": 2}),
     ok({"x": {"a": 1}, "y": {"a": 2}})),
    (task({"a": 1}, {"op": "move", "from": "/a", "path": "/b"}), ok({"b": 1})),
    (task(["a", "b", "c"], {"op": "move", "from": "/0", "path": "/2"}),
     ok(["b", "c", "a"])),
    (task(["a", "b", "c"], {"op": "move", "from": "/2", "path": "/0"}),
     ok(["c", "a", "b"])),
    (task({"a": {"b": 1}}, {"op": "move", "from": "/a", "path": "/a/c"}),
     err(0, "move_into_child")),
    (task({"a": 1}, {"op": "add", "path": "", "value": [3]}), ok([3])),
    (task({"a": 1}, {"op": "replace", "path": "", "value": False}), ok(False)),
    (task({"a": 1}, {"op": "remove", "path": ""}), ok(None)),
    (task({"a": 1}, {"op": "copy", "from": "/a", "path": ""}), ok(1)),
    (task({"a/b": {"~": 1}}, {"op": "replace", "path": "/a~1b/~0", "value": 2}),
     ok({"a/b": {"~": 2}})),
    (task({}, {"op": "add", "path": "bad", "value": 1}), err(0, "invalid_pointer")),
    (task({}, {"op": "add", "path": "/a~2b", "value": 1}), err(0, "invalid_pointer")),
    (task([1], {"op": "replace", "path": "/00", "value": 2}), err(0, "invalid_index")),
    (task([1], {"op": "remove", "path": "/-1"}), err(0, "invalid_index")),
    (task([1], {"op": "test", "path": "/-", "value": 1}), err(0, "invalid_index")),
    (task({"a": 1}, {"op": "remove", "path": "/b"}), err(0, "missing_path")),
    (task({"a": 1}, {"op": "add", "path": "/a/b", "value": 2}), err(0, "missing_path")),
    (task({"a": 1}, {"op": "add", "path": "/b/c", "value": 2}), err(0, "missing_path")),
    (task({"a": 1}, {"op": "replace", "path": "/a", "value": 2},
          {"op": "test", "path": "/a", "value": 1}), err(1, "failed_test")),
    (task({"a": 1}, {"op": "merge", "path": "/a", "value": 2}), err(0, "invalid_operation")),
    (task({"a": 1}, {"op": "add", "path": "/b"}), err(0, "invalid_operation")),
    ({"document": {}, "operations": "bad"}, err(-1, "invalid_request")),
    (task([1], {"op": "add", "path": "/2", "value": 3}), err(0, "invalid_index")),
    (task({"0": 1}, {"op": "replace", "path": "/0", "value": 2}), ok({"0": 2})),
    (task({"a": 1}, {"op": "move", "from": "/a", "path": "/a"}), ok({"a": 1})),
    (task({"x": True}, {"op": "test", "path": "/x", "value": 1}), err(0, "failed_test")),
    (task({"": 1}, {"op": "replace", "path": "/", "value": 2}), ok({"": 2})),
    ({"document": {}, "operations": [], "trace": True}, err(-1, "invalid_request")),
    (task({}, {"op": "add", "path": "/x", "value": 1, "trace": True}),
     err(0, "invalid_operation")),
    (task({"x": 1}, {"op": "remove", "path": "/x", "value": 2}),
     err(0, "invalid_operation")),
    (task({"x": 1}, {"op": "test", "path": "/x", "value": 1, "from": "/x"}),
     err(0, "invalid_operation")),
    (task({"x": 1}, {"op": "copy", "from": "/x", "path": "/y", "value": 2}),
     err(0, "invalid_operation")),
    (task({"x": 1}, {"op": "move", "from": "/missing", "path": "/missing"}),
     err(0, "missing_path")),
    (task([0], {"op": "move", "from": "/2", "path": "/2"}),
     err(0, "invalid_index")),
    (task({"x": 1}, {"op": "move", "from": "/x/y", "path": "/x/y"}),
     err(0, "missing_path")),
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
    print(f"Ran {len(CASES)} tests in process-level CLI gate")
    for index, expected, actual, code, stderr in failures:
        print(f"FAIL: case {index}: expected {expected!r}, got {actual!r}, exit={code}, stderr={stderr!r}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
