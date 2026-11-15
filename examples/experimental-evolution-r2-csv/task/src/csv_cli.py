import json
import sys


def main():
    try:
        json.loads(sys.stdin.read())
    except (ValueError, TypeError):
        pass
    print(json.dumps({"ok": False, "error": "not_implemented"}))


if __name__ == "__main__":
    main()
