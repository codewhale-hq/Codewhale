#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="${repo_root}/scripts/release/check-ohos-deps.sh"
tmp_base="${TMPDIR:-/tmp}"
tmp_base="${tmp_base%/}"
tmp_base="${tmp_base:-/}"
fixture="$(mktemp -d "${tmp_base}/check-ohos-deps-test.XXXXXX")"
unwritable_dir="${fixture}/unwritable-tmp"
cleanup() {
  if [[ -d "${unwritable_dir}" ]]; then
    chmod 700 "${unwritable_dir}" || true
  fi
  rm -rf "${fixture}"
}
trap cleanup EXIT

mkdir -p "${fixture}/bin" "${fixture}/grep-fail-bin" "${fixture}/tmp" "${unwritable_dir}"
chmod 500 "${unwritable_dir}"

cat >"${fixture}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
mode="${FAKE_CARGO_MODE:-ok}"
pkg=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --package)
      pkg="${2:-}"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

case "${mode}" in
  ok)
    if [[ "${pkg}" == "codewhale-tui" ]]; then
      printf 'codewhale-tui v0.10.3\ntokio v1.40.0\n'
    else
      printf 'codewhale-workflow-js v0.10.3\nrquickjs feature "bindgen"\nrquickjs-sys feature "bindgen"\n'
    fi
    ;;
  disallowed)
    if [[ "${pkg}" == "codewhale-tui" ]]; then
      printf 'codewhale-tui v0.10.3\nportable-pty v0.8.1\n'
    else
      printf 'codewhale-workflow-js v0.10.3\nrquickjs feature "bindgen"\nrquickjs-sys feature "bindgen"\n'
    fi
    ;;
  empty)
    exit 0
    ;;
  missing_root)
    printf 'tokio v1.40.0\n'
    ;;
  missing_bindgen)
    if [[ "${pkg}" == "codewhale-tui" ]]; then
      printf 'codewhale-tui v0.10.3\ntokio v1.40.0\n'
    else
      printf 'codewhale-workflow-js v0.10.3\nrquickjs feature "bindgen"\n'
    fi
    ;;
  *)
    echo "unknown FAKE_CARGO_MODE: ${mode}" >&2
    exit 2
    ;;
esac
EOF
chmod +x "${fixture}/bin/cargo"

real_grep="$(command -v grep)"
cat >"${fixture}/grep-fail-bin/grep" <<EOF
#!/usr/bin/env bash
for arg in "\$@"; do
  if [[ "\${arg}" == *'portable-pty'* ]]; then
    echo "simulated grep internal failure" >&2
    exit 2
  fi
done
exec "${real_grep}" "\$@"
EOF
chmod +x "${fixture}/grep-fail-bin/grep"

run_case() {
  local expected="$1"
  local label="$2"
  shift 2
  local out_file="${fixture}/${label}.out"
  local rc=0
  "$@" >"${out_file}" 2>&1 || rc=$?
  if [[ "${expected}" == "pass" ]]; then
    if ((rc != 0)); then
      echo "FAIL [${label}]: expected exit 0, got ${rc}" >&2
      cat "${out_file}" >&2
      exit 1
    fi
  else
    if ((rc == 0)); then
      echo "FAIL [${label}]: expected non-zero exit, got 0" >&2
      cat "${out_file}" >&2
      exit 1
    fi
  fi
}

run_case pass clean \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=ok "${checker}"
grep -Fq 'OHOS dependency graph OK for codewhale-tui' "${fixture}/clean.out"
grep -Fq 'OHOS rquickjs-sys bindgen feature edge OK for codewhale-workflow-js' "${fixture}/clean.out"

run_case fail disallowed \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=disallowed "${checker}"
grep -Fq 'portable-pty v0.8.1' "${fixture}/disallowed.out"

run_case fail empty \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=empty "${checker}"
grep -Fq 'returned empty output' "${fixture}/empty.out"

run_case fail missing_root \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=missing_root "${checker}"
grep -Fq 'did not contain root package codewhale-tui' "${fixture}/missing_root.out"

run_case fail unwritable_tmpdir \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${unwritable_dir}" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=ok "${checker}"
grep -Fq 'Failed to create temporary file' "${fixture}/unwritable_tmpdir.out"

run_case fail grep_internal_error \
  env PATH="${fixture}/grep-fail-bin:${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=ok "${checker}"
grep -Fq 'Failed to inspect OHOS dependency graph' "${fixture}/grep_internal_error.out"

run_case fail missing_bindgen \
  env PATH="${fixture}/bin:${PATH}" TMPDIR="${fixture}/tmp" CODEWHALE_OHOS_DEP_RETRIES=1 \
  FAKE_CARGO_MODE=missing_bindgen "${checker}"
grep -Fq 'lost the rquickjs bindgen feature edges' "${fixture}/missing_bindgen.out"

echo "check-ohos-deps tests passed (7/7)"
