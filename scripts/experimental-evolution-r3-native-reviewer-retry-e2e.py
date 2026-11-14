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
    workflow = cas["last_run"](output / "retry_dev_review" / "workspace" /
                                ".codewhale" / "workflow-runs.jsonl")
    expected_labels = ("dev_review_candidate_g0_1",
                       "dev_review_retry_candidate_g0_1", "evolution_reviewer")
    receipt["reviewer_roles"] = {
        event.get("label"): event.get("resolved_role")
        for event in workflow.get("events", [])
        if event.get("type") == "task_started" and event.get("label") in expected_labels
    }
    requests = json.loads((output / "retry_dev_review" / "mock_requests.json").read_text(
        encoding="utf-8"))
    reviews = [request for request in requests if request["category"] in
               ("dev_review", "final_review")]
    receipt["review_system_contract"] = {
        "request_count": len(reviews),
        "all_present": all(request["system_present"] for request in reviews),
        "all_no_tools": all(request["system_no_tools"] for request in reviews),
        "none_has_tool_guidance": all(not request["system_tool_guidance"] for request in reviews),
        "all_schema": all(request["system_schema"] for request in reviews),
        "none_offers_tools": all(not request["tools"] for request in reviews),
    }
    receipt["pass"] = receipt["pass"] and all(
        receipt["reviewer_roles"].get(label) == "reviewer" for label in expected_labels)
    receipt["pass"] = receipt["pass"] and len(reviews) >= 3 and all(
        value for name, value in receipt["review_system_contract"].items()
        if name != "request_count")
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n",
                                         encoding="utf-8")
    print(json.dumps(receipt))
    return 0 if receipt["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
