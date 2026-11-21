#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo}"

template=".github/ISSUE_TEMPLATE/agent-task.yml"
retired_workflows=(
  ".github/workflows/v0868-milestone-sync.yml"
  ".github/workflows/agent-task-labels.yml"
)

fail() {
  echo "agent-task metadata contract failed: $*" >&2
  exit 1
}

[[ -f "${template}" ]] || fail "missing ${template}"
for retired in "${retired_workflows[@]}"; do
  [[ ! -e "${retired}" ]] || fail "retired workflow still exists: ${retired}"
done

if grep -nE '^(title|labels):.*v[0-9]+\.[0-9]+\.[0-9]+' "${template}"; then
  fail "agent-task defaults must not pin a release version"
fi

if grep -nE '^labels:.*agent-ready' "${template}"; then
  fail "agent-task template must not self-apply agent-ready; a maintainer grants it during triage"
fi

echo "Agent-task metadata is release-neutral, milestone-safe, and does not self-apply agent-ready."
