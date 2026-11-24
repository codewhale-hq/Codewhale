#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="${repo_root}/scripts/release/require-full-ci-receipt.sh"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

sha="0123456789abcdef0123456789abcdef01234567"

fake_gh="${tmp_dir}/gh"
cat > "${fake_gh}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "api" ]] || { echo "unexpected: $*" >&2; exit 9; }
path="$2"
filter="$4"
case "${path}" in
  */workflows/ci.yml/runs\?*) fixture="${FIXTURE_DIR}/runs.json" ;;
  */actions/runs/*/jobs\?*)
    run_id="${path#*/actions/runs/}"
    run_id="${run_id%%/*}"
    fixture="${FIXTURE_DIR}/jobs-${run_id}.json"
    ;;
  *) echo "unexpected path: ${path}" >&2; exit 9 ;;
esac
[[ -f "${fixture}" ]] || { echo "API error" >&2; exit 1; }
jq -r "${filter}" "${fixture}"
EOF
chmod +x "${fake_gh}"
export FULL_CI_RECEIPT_GH="${fake_gh}"
export FIXTURE_DIR="${tmp_dir}"

expect_pass() {
  local label="$1"
  if ! "${script}" owner/Repo "${sha}" >"${tmp_dir}/out" 2>&1; then
    echo "FAIL (${label}): expected a receipt" >&2
    cat "${tmp_dir}/out" >&2
    exit 1
  fi
}
expect_fail() {
  local label="$1"
  if "${script}" owner/Repo "${2:-${sha}}" >"${tmp_dir}/out" 2>&1; then
    echo "FAIL (${label}): expected refusal" >&2
    cat "${tmp_dir}/out" >&2
    exit 1
  fi
}

run() {
  local id="$1" event="${2:-workflow_dispatch}" conclusion="${3:-success}" head="${4:-${sha}}" owner="${5:-owner/repo}"
  printf '{"id":%s,"head_sha":"%s","event":"%s","status":"completed","conclusion":"%s","path":".github/workflows/ci.yml","head_repository":{"full_name":"%s"},"html_url":"https://example.invalid/%s"}' \
    "${id}" "${head}" "${event}" "${conclusion}" "${owner}" "${id}"
}
runs() {
  local IFS=,
  echo "{\"workflow_runs\":[$*]}" > "${tmp_dir}/runs.json"
}
full_jobs='{"name":"Change detection","conclusion":"success"},
  {"name":"Lint","conclusion":"success"},
  {"name":"Shared-process tests","conclusion":"skipped"},
  {"name":"Test (windows-latest)","conclusion":"skipped"},
  {"name":"Scoped test (${{ matrix.os }})","conclusion":"skipped"},
  {"name":"Sharded tests (ubuntu-latest) / Suite check","conclusion":"success"},
  {"name":"Sharded tests (macos-latest) / Suite check","conclusion":"success"},
  {"name":"Sharded tests (windows-latest) / Suite check","conclusion":"success"}'
jobs() {
  local id="$1" body="$2" total="${3:-}"
  local count
  count="$(echo "[${body}]" | jq length)"
  echo "{\"total_count\":${total:-${count}},\"jobs\":[${body}]}" > "${tmp_dir}/jobs-${id}.json"
}

# 1. No run at all.
runs
expect_fail "no runs"
grep -q "No completed, green, full ci.yml run" "${tmp_dir}/out"

# 2. A green full dispatch for the exact SHA is a receipt.
runs "$(run 21)"
jobs 21 "${full_jobs}"
expect_pass "full dispatch"
grep -q "Full CI receipt for ${sha}: https://example.invalid/21" "${tmp_dir}/out"

# 3. A green scoped dispatch (crate/filter set) never counts.
runs "$(run 22)"
jobs 22 '{"name":"Change detection","conclusion":"success"},
  {"name":"Lint","conclusion":"skipped"},
  {"name":"Sharded tests (ubuntu-latest)","conclusion":"skipped"},
  {"name":"Sharded tests (macos-latest)","conclusion":"skipped"},
  {"name":"Sharded tests (windows-latest)","conclusion":"skipped"},
  {"name":"Scoped test (ubuntu-latest)","conclusion":"success"}'
expect_fail "scoped run"
grep -q "(scoped run)" "${tmp_dir}/out"

# 4. A scoped job that ran alongside green suite checks still disqualifies.
jobs 22 "${full_jobs},{\"name\":\"Scoped test (macos-latest)\",\"conclusion\":\"success\"}"
expect_fail "scoped job present"

# 5. A shared-process qualification run is not the three-OS suite.
runs "$(run 23)"
jobs 23 '{"name":"Change detection","conclusion":"success"},
  {"name":"Lint","conclusion":"success"},
  {"name":"Shared-process tests","conclusion":"success"},
  {"name":"Sharded tests (ubuntu-latest)","conclusion":"skipped"},
  {"name":"Sharded tests (macos-latest)","conclusion":"skipped"},
  {"name":"Sharded tests (windows-latest)","conclusion":"skipped"}'
expect_fail "shared-process run"

# 6. A missing suite check (renamed job, skipped OS) is not a receipt.
runs "$(run 24)"
jobs 24 "$(echo "${full_jobs}" | grep -v 'macos-latest) / Suite check' | sed '$ s/,$//')"
expect_fail "missing macOS suite"
grep -q "full-suite job did not succeed" "${tmp_dir}/out"

# 7. A failed, cancelled or still-running job disqualifies even a green run.
runs "$(run 25)"
jobs 25 "${full_jobs},{\"name\":\"Docs\",\"conclusion\":\"failure\"}"
expect_fail "failed job"
jobs 25 "${full_jobs},{\"name\":\"Docs\",\"conclusion\":null}"
expect_fail "unfinished job"

# 8. A truncated job list is refused rather than trusted.
jobs 25 "${full_jobs}" 150
expect_fail "truncated jobs"
grep -q "incomplete job list" "${tmp_dir}/out"

# 9. Wrong event, conclusion, SHA or repository never counts, even with full jobs.
for spec in "26 pull_request" "27 workflow_dispatch failure" "28 workflow_dispatch success ffffffffffffffffffffffffffffffffffffffff" "29 workflow_dispatch success ${sha} fork/repo"; do
  read -r id event conclusion head owner <<< "${spec}"
  runs "$(run "${id}" "${event}" "${conclusion:-success}" "${head:-${sha}}" "${owner:-owner/repo}")"
  jobs "${id}" "${full_jobs}"
  expect_fail "run filter ${spec}"
done

# 10. An unreadable job list skips that run; a later full run still counts.
runs "$(run 30)" "$(run 21)"
expect_pass "unreadable first run"
grep -q "https://example.invalid/30 not counted" "${tmp_dir}/out"

# 11. An API failure listing runs is a refusal.
rm -f "${tmp_dir}/runs.json"
expect_fail "api failure"
grep -q "Actions API call failed" "${tmp_dir}/out"

# 12. Malformed input is rejected before any API call.
expect_fail "short sha" "abc123"
expect_fail "uppercase sha" "0123456789ABCDEF0123456789ABCDEF01234567"
if "${script}" "bad repo" "${sha}" >/dev/null 2>&1; then
  echo "FAIL (bad repo): expected refusal" >&2
  exit 1
fi

echo "require-full-ci-receipt tests passed"
