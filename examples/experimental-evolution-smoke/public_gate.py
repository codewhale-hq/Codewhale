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
    ({"header": "en;q=.5,fr;q=0.4", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=0.1234,fr;q=0.2", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=01,fr;q=0.3", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=1.001,fr;q=0.9", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=1e0,fr;q=0.8", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=0.2;foo=bar,fr;q=0.1", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=0.2;q=0.8,fr;q=0.1", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=0,en;q=1,fr;q=0.2", "available": ["en", "fr"]}, "fr"),
    ({"header": "*;q=0.8,en;q=0", "available": ["en", "fr"]}, "fr"),
    ({"header": "en--US,fr;q=0.2", "available": ["en-US", "fr"]}, "fr"),
    ({"header": "en_US,fr;q=0.2", "available": ["en-US", "fr"]}, "fr"),
    ({"header": "en;q=0.5, fr;q=0.5", "available": ["en", "fr"]}, "en"),
    ({"header": "en-UK;q=0.5,en;q=0.3", "available": ["en-US", "en-UK"]}, "en-UK"),
    ({"header": " EN ; Q = 0.7 , fr;q=0.6", "available": ["en", "fr"]}, "en"),
    ({"header": "en;q=0.000,fr;q=0.001", "available": ["en", "fr"]}, "fr"),
    ({"header": "en;q=1.000,fr;q=0.9", "available": ["en", "fr"]}, "en"),
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
