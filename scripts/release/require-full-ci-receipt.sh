#!/usr/bin/env bash
# Succeed only when a completed, green, full ci.yml run exists for an exact
# commit, and print its URL. Exit 0 with a receipt, 1 without one (including
# any API failure), 2 on bad usage.
#
# Usage: require-full-ci-receipt.sh <owner/repo> <40-char sha>
#
# The receipt is a workflow_dispatch ci.yml run on this repository for <sha>
# that concluded success and whose latest job attempts show the full suite:
# every job succeeded or was skipped, no scoped or shared-process job ran, and
# Change detection, Lint and the three `Suite check` jobs succeeded.
#
# FULL_CI_RECEIPT_GH overrides the gh executable (tests only).
set -euo pipefail

repo="${1:-}"
sha="${2:-}"
gh_bin="${FULL_CI_RECEIPT_GH:-gh}"

if [[ -z "${repo}" || -z "${sha}" ]]; then
  echo "usage: $0 <owner/repo> <sha>" >&2
  exit 2
fi
if ! [[ "${repo}" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
  echo "::error::Repository '${repo}' must be owner/name." >&2
  exit 2
fi
if [[ "${#sha}" -ne 40 || "${sha}" =~ [^0-9a-f] ]]; then
  echo "::error::SHA must be a full 40-character lowercase commit SHA, got '${sha}'." >&2
  exit 2
fi

repo_lower="$(printf '%s' "${repo}" | tr '[:upper:]' '[:lower:]')"
if ! runs="$("${gh_bin}" api \
  "repos/${repo}/actions/workflows/ci.yml/runs?head_sha=${sha}&event=workflow_dispatch&status=success&per_page=100" \
  --jq ".workflow_runs[] | select(.head_sha == \"${sha}\" and .event == \"workflow_dispatch\" and .status == \"completed\" and .conclusion == \"success\" and .path == \".github/workflows/ci.yml\" and ((.head_repository.full_name // \"\") | ascii_downcase) == \"${repo_lower}\") | [.id, .html_url] | @tsv")"; then
  echo "No full CI receipt for ${sha}: the Actions API call failed." >&2
  exit 1
fi

jobs_verdict='. as $run
  | def ok($name): any($run.jobs[]; .name == $name and .conclusion == "success");
  if (.jobs | length) != .total_count then "incomplete job list"
  elif any(.jobs[]; .conclusion != "success" and .conclusion != "skipped") then "a job did not succeed"
  elif any(.jobs[]; (.name | startswith("Scoped test")) and .conclusion != "skipped") then "scoped run"
  elif any(.jobs[]; .name == "Shared-process tests" and .conclusion != "skipped") then "shared-process run"
  elif ([
      "Change detection",
      "Lint",
      "Sharded tests (ubuntu-latest) / Suite check",
      "Sharded tests (macos-latest) / Suite check",
      "Sharded tests (windows-latest) / Suite check"
    ] | all(.[]; ok(.))) | not then "a full-suite job did not succeed"
  else "full"
  end'

while IFS=$'\t' read -r run_id run_url; do
  [[ -n "${run_id}" ]] || continue
  if ! [[ "${run_id}" =~ ^[0-9]+$ ]]; then
    echo "No full CI receipt for ${sha}: unexpected run id '${run_id}' from the Actions API." >&2
    exit 1
  fi
  if ! verdict="$("${gh_bin}" api \
    "repos/${repo}/actions/runs/${run_id}/jobs?filter=latest&per_page=100" \
    --jq "${jobs_verdict}")"; then
    echo "Run ${run_url} not counted: its job list could not be read." >&2
    continue
  fi
  if [[ "${verdict}" == "full" ]]; then
    echo "Full CI receipt for ${sha}: ${run_url}"
    exit 0
  fi
  echo "Run ${run_url} is green but not a full-suite receipt (${verdict})." >&2
done <<< "${runs}"

echo "No completed, green, full ci.yml run for ${sha}." >&2
exit 1
