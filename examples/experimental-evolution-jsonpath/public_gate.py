import json
import subprocess
import sys


DOC = {
    "store": {
        "book": [
            {"title": "A", "price": 8, "tags": ["x", "y"], "author": {"name": "Ann"}},
            {"title": "B", "price": 12, "tags": ["y"], "author": {"name": "Bob"}},
            {"title": "C", "price": 8.0, "tags": [], "author": {"name": "Cara"}},
        ],
        "bike": {"price": 20, "color": "red"},
    },
    "items": [{"id": 1, "active": True}, {"id": 2, "active": False},
              {"id": 3, "active": True}],
    "a.b": {"x/y": {"~": 5}},
    "nested": {"left": {"leaf": 1}, "right": [{"leaf": 2}, {"leaf": 3}]},
}


def good(path, matches, document=DOC):
    return ({"document": document, "path": path}, {"ok": True, "matches": matches})


def bad(path, document=DOC):
    return ({"document": document, "path": path}, {"ok": False, "error": "invalid_path"})


CASES = [
    good("$", [DOC]),
    good("$.store.book[0].title", ["A"]),
    good('$["a.b"]["x/y"]["~"]', [5]),
    good("$.store.book[-1].title", ["C"]),
    good("$.store.book[9]", []),
    good("$.store.book[*].title", ["A", "B", "C"]),
    good("$.store.book.*.price", [8, 12, 8.0]),
    good("$.store.*", [DOC["store"]["book"], DOC["store"]["bike"]]),
    good("$.store.book[0:2].title", ["A", "B"]),
    good("$.store.book[::-1].title", ["C", "B", "A"]),
    good("$.store.book[::2].title", ["A", "C"]),
    good("$.store.book[2:0:-1].title", ["C", "B"]),
    good("$.store.book[?(@.price==8)].title", ["A", "C"]),
    good("$.items[?(@.active==true)].id", [1, 3]),
    good("$.items[?(@.active==1)].id", []),
    good('$.store.book[?(@.tags==["y"])].title', ["B"]),
    good('$.store.book[?(@.author=={"name":"Bob"})].title', ["B"]),
    good("$..title", ["A", "B", "C"]),
    good("$..price", [8, 12, 8.0, 20]),
    good("$..leaf", [1, 2, 3]),
    good("$..author.name", ["Ann", "Bob", "Cara"]),
    good("$.nested..*", [DOC["nested"]["left"], 1, DOC["nested"]["right"],
                         DOC["nested"]["right"][0], 2,
                         DOC["nested"]["right"][1], 3]),
    good("$.items[*].*", [1, True, 2, False, 3, True]),
    good("$.store.book[*].tags[*]", ["x", "y", "y"]),
    good("$.store.book[*].missing", []),
    good("$.store.book[0].price.title", []),
    good("$[0]", []),
    good("$..*", [1, {"x": 2}, 2], {"a": 1, "b": {"x": 2}}),
    bad("$.."),
    bad("$["),
    bad("$[0:3:0]"),
    bad("$[?(@.id=1)]"),
    bad("$."),
    bad('$["bad\\q"]'),
    bad("$..9"),
    bad("$[?(@.x==NaN)]"),
    bad("foo"),
    bad("$[*]garbage"),
    bad("$.missing[?(@.x=1)]"),
    ({"document": DOC, "path": 3}, {"ok": False, "error": "invalid_request"}),
    ({"document": DOC, "path": "$", "extra": True},
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
    print(f"Ran {len(CASES)} tests in process-level JSONPath gate")
    for index, path, expected, actual, code, stderr in failures:
        print(f"FAIL: case {index}: path={path!r}, expected={expected!r}, got={actual!r}, exit={code}, stderr={stderr!r}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
