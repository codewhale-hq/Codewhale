import json
import subprocess
import sys


DOC = {
    "x": [{"v": 1, "children": [{"v": 2}]},
          {"v": True, "children": []}],
    "space key": {"a": [0, 1, 2, 3]},
    "meta": {"x": None},
}


def good(path, matches, document=DOC):
    return ({"document": document, "path": path}, {"ok": True, "matches": matches})


def bad(path, document=DOC):
    return ({"document": document, "path": path}, {"ok": False, "error": "invalid_path"})


CASES = [
    good('$["space key"].a[-3:-1]', [1, 2]),
    good("$.x[?(@.v==true)].children", [[]]),
    good("$.x[?(@.v==1)].children", [[{"v": 2}]]),
    good("$..v", [1, 2, True]),
    good("$..*", [[1, {"b": 2}], 1, {"b": 2}, 2], {"a": [1, {"b": 2}]}),
    good("$.x[*].children[*].v", [2]),
    good('$.x[?(@.children==[{"v":2}])].v', [1]),
    good("$.meta.x", [None]),
    good("$.x[2].v", []),
    bad("$.x[?(@.v=={)]"),
    bad("$.meta.x[::0]"),
    bad("$.x[0]oops"),
    ({"document": DOC, "path": "$", "debug": False},
     {"ok": False, "error": "invalid_request"}),
]


def main():
    failures = []
    for index, (request, expected) in enumerate(CASES, 1):
        completed = subprocess.run([sys.executable, "src/jsonpath_cli.py"],
                                   input=json.dumps(request), text=True,
                                   capture_output=True, timeout=5, check=False)
        try:
            actual = json.loads(completed.stdout)
        except ValueError:
            actual = completed.stdout.strip()
        if completed.returncode != 0 or actual != expected:
            failures.append((index, request["path"], expected, actual,
                             completed.returncode, completed.stderr[-200:]))
    print(f"Ran {len(CASES)} tests in process-level held-out JSONPath gate")
    for index, path, expected, actual, code, stderr in failures:
        print(f"FAIL: case {index}: path={path!r}, expected={expected!r}, got={actual!r}, exit={code}, stderr={stderr!r}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
