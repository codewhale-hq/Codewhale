import json
import subprocess
import sys


CASES = [
    ({"header": "en;q=0,en-US;q=0.8", "available": ["en", "en-US"]}, "en-US"),
    ({"header": "zh-Hant-TW;q=0.9", "available": ["zh", "zh-Hant"]}, None),
    ({"header": "fr;q=0.4,en;q=0.9", "available": ["fr", "en"]}, "en"),
    ({"header": "en;q=1.1,fr;q=0.6", "available": ["en", "fr"]}, "fr"),
    ({"header": "de;q=0,*;q=0", "available": ["de", "en"]}, None),
    ({"header": "pt-BR;q=0.3,ja;q=0.4", "available": ["pt-BR", "ja"]}, "ja"),
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
    print(f"Ran {len(CASES)} tests in held-out CLI gate")
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        sys.exit(1)
    print("OK")


if __name__ == "__main__":
    main()
