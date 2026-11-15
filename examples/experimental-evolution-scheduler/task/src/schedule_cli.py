import json
import sys


if __name__ == "__main__":
    json.load(sys.stdin)
    print(json.dumps({"ok": False, "error": "invalid_request"}))
