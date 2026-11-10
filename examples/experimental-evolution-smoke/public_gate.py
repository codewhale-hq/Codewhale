import json
import subprocess
import sys


CASES = [
    ({"header": "en-US,en;q=0.8", "available": ["en", "en-US"]}, "en-US"),
    ({"header": "fr-CA,fr;q=0.9", "available": ["fr", "en"]}, "fr"),
    ({"header": "de;q=0,*;q=0.2", "available": ["de", "en"]}, "en"),
    ({"header": "fr;q=0.5,en;q=0.5", "available": ["en", "fr"]}, "fr"),
    ({"header": "es;q=invalid,en", "available": ["es", "en"]}, "en"),
    ({"header": "EN-us;q=0.7", "available": ["en-US"]}, "en-US"),
    ({"header": "*;q=0.3", "available": ["ja", "en"]}, "ja"),
    ({"header": "", "available": ["en"]}, None),
]


def main():
    failures = []
    for index, (request, expected) in enumerate(CASES, 1):
        result = subprocess.run(
            [sys.executable, "src/accept_language.py"],
            input=json.dumps(request),
            text=True,
            capture_output=True,
            timeout=5,
            check=False,
        )
        try:
            actual = json.loads(result.stdout)
        except json.JSONDecodeError:
            actual = result.stdout.strip()
        if result.returncode != 0 or actual != expected:
            failures.append(
                f"case {index}: expected {expected!r}, got {actual!r}, "
                f"exit={result.returncode}, stderr={result.stderr[:300]!r}"
            )
    print(f"Ran {len(CASES)} tests in process-level CLI gate")
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        sys.exit(1)
    print("OK")


if __name__ == "__main__":
    main()
