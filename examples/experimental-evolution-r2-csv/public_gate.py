import csv
import io
import json
import subprocess
import sys


def expected(request):
    if not isinstance(request, dict) or set(request) != {"text", "delimiter"} \
            or not isinstance(request["text"], str) \
            or request["delimiter"] not in (",", ";"):
        return {"ok": False, "error": "invalid_request"}
    try:
        rows = list(csv.reader(io.StringIO(request["text"], newline=""),
                               delimiter=request["delimiter"], strict=True))
    except csv.Error:
        return {"ok": False, "error": "invalid_csv"}
    return {"ok": True, "rows": rows}


REQUESTS = [
    {"text": "a,b\n1,2\n", "delimiter": ","},
    {"text": "", "delimiter": ","},
    {"text": "\n", "delimiter": ","},
    {"text": "a,,c", "delimiter": ","},
    {"text": '"a,b",c', "delimiter": ","},
    {"text": '"a""b",c', "delimiter": ","},
    {"text": '"a\nb",c', "delimiter": ","},
    {"text": "a;b\n1;2", "delimiter": ";"},
    {"text": '"a;b";c', "delimiter": ";"},
    {"text": "a\r\nb\r\n", "delimiter": ","},
    {"text": "é,東京", "delimiter": ","},
    {"text": '"unterminated', "delimiter": ","},
    {"text": "a,b", "delimiter": "|"},
    {"text": "a,b", "delimiter": 1},
    {"text": 4, "delimiter": ","},
    {"text": "a,b", "delimiter": ",", "extra": True},
    {"text": "a,b"},
    [],
    None,
    {"text": '""', "delimiter": ","},
]

CASES = [(request, expected(request)) for request in REQUESTS]


def main():
    failed = []
    for index, (request, answer) in enumerate(CASES, 1):
        completed = subprocess.run([sys.executable, "src/csv_cli.py"],
                                   input=json.dumps(request), text=True,
                                   capture_output=True, timeout=5, check=False)
        try:
            actual = json.loads(completed.stdout)
        except ValueError:
            actual = completed.stdout
        if completed.returncode != 0 or actual != answer:
            failed.append(index)
    print("Ran {} tests in process-level CSV gate".format(len(CASES)))
    for index in failed:
        print("FAIL: case {}".format(index))
    return bool(failed)


if __name__ == "__main__":
    sys.exit(main())
