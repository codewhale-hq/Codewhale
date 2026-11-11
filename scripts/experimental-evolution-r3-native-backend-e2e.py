#!/usr/bin/env python3
"""Fail closed when the native bubblewrap backend cannot pass its capability probe."""

import argparse
import json
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    bwrap = Path("/usr/bin/bwrap")
    before = subprocess.run([str(bwrap), "--version"], capture_output=True,
                            text=True, check=True).stdout
    inner = output / "inner"
    fixture = Path(__file__).with_name("experimental-evolution-r3-native-isolation-e2e.py")
    shell = ('mount --bind /usr/bin/false /usr/bin/bwrap && '
             'exec /usr/bin/python3.11 "$1" --codewhale "$2" --output "$3"')
    result = subprocess.run(["unshare", "--mount", "--propagation", "private", "--",
                             "/bin/bash", "-c", shell, "backend-e2e", str(fixture),
                             str(args.codewhale.resolve()), str(inner)],
                            capture_output=True, text=True, check=False, timeout=60)
    (output / "stdout.log").write_text(result.stdout, encoding="utf-8")
    (output / "stderr.log").write_text(result.stderr, encoding="utf-8")
    after = subprocess.run([str(bwrap), "--version"], capture_output=True,
                           text=True, check=True).stdout
    inner_stdout = (inner / "stdout.log").read_text(encoding="utf-8") \
        if (inner / "stdout.log").is_file() else ""
    requests = json.loads((inner / "mock_requests.json").read_text(encoding="utf-8")) \
        if (inner / "mock_requests.json").is_file() else []
    journal = inner / "workspace" / ".codewhale" / "workflow-runs.jsonl"
    receipt = {"schema_version": 1, "scope": "native_backend_capability_failure",
               "inner_exit_code": result.returncode,
               "unsupported_isolation": "UNSUPPORTED_ISOLATION" in inner_stdout,
               "model_requests": len(requests), "workflow_journal_exists": journal.exists(),
               "host_bwrap_unchanged": before == after}
    receipt["pass"] = (result.returncode != 0 and receipt["unsupported_isolation"]
                       and receipt["model_requests"] == 0
                       and not receipt["workflow_journal_exists"]
                       and receipt["host_bwrap_unchanged"])
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                         encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
