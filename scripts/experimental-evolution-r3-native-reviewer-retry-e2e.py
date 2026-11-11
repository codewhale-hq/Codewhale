#!/usr/bin/env python3
"""A tool-only DEV review must consume one bounded retry before code selection."""

import argparse
import json
import runpy
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--codewhale", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).parent
    helper = runpy.run_path(str(source / "experimental-evolution-smoke.py"))
    support = runpy.run_path(str(source / "experimental-evolution-r3-native-isolation-e2e.py"))
    cas = runpy.run_path(str(source / "experimental-evolution-r3-native-cas-e2e.py"))
    receipt = cas["case"](output / "retry_dev_review", "retry_dev_review",
                          args.codewhale.resolve(), helper, support)
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                         encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
