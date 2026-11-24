#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

metadata_file="${tmp_dir}/metadata.json"
cat >"${metadata_file}" <<'JSON'
{
  "workspace_members": ["build", "core", "tui", "app", "cli"],
  "packages": [
    {
      "id": "build",
      "name": "codewhale-build-support",
      "version": "0.9.5",
      "dependencies": []
    },
    {
      "id": "core",
      "name": "codewhale-core",
      "version": "0.9.5",
      "dependencies": [
        {"name": "codewhale-cli", "path": "/workspace/cli", "kind": "dev"}
      ]
    },
    {
      "id": "tui",
      "name": "codewhale-tui",
      "version": "0.9.5",
      "dependencies": [
        {"name": "codewhale-build-support", "path": "/workspace/build", "kind": "build"},
        {"name": "codewhale-core", "path": "/workspace/core", "kind": null}
      ]
    },
    {
      "id": "app",
      "name": "codewhale-app-server",
      "version": "0.9.5",
      "dependencies": [
        {"name": "codewhale-core", "path": "/workspace/core", "kind": null}
      ]
    },
    {
      "id": "cli",
      "name": "codewhale-cli",
      "version": "0.9.5",
      "dependencies": [
        {"name": "codewhale-tui", "path": "/workspace/tui", "kind": null},
        {"name": "codewhale-app-server", "path": "/workspace/app", "kind": null}
      ]
    }
  ]
}
JSON

expect_validation_failure() {
  local label="$1"
  local expected="$2"
  local fixture="$3"
  shift 3

  local output="${tmp_dir}/${label}.txt"
  if python3 "${script_dir}/validate-crate-publish-order.py" \
    --metadata-file "${fixture}" "$@" >"${output}" 2>&1; then
    echo "${label} unexpectedly passed" >&2
    exit 1
  fi
  grep -F "${expected}" "${output}" >/dev/null
  if grep -F "Traceback" "${output}" >/dev/null; then
    echo "${label} emitted an unhandled traceback" >&2
    exit 1
  fi
}

expect_validation_failure \
  duplicate-crate \
  "publish package list contains duplicates: codewhale-core" \
  "${metadata_file}" \
  codewhale-build-support \
  codewhale-core \
  codewhale-core \
  codewhale-tui \
  codewhale-app-server \
  codewhale-cli

expect_validation_failure \
  missing-crate \
  "publish package list is missing workspace crates: codewhale-app-server" \
  "${metadata_file}" \
  codewhale-build-support \
  codewhale-core \
  codewhale-tui \
  codewhale-cli

expect_validation_failure \
  extra-crate \
  "publish package list contains non-workspace crates: codewhale-extra" \
  "${metadata_file}" \
  codewhale-build-support \
  codewhale-core \
  codewhale-tui \
  codewhale-app-server \
  codewhale-cli \
  codewhale-extra

mixed_metadata_file="${tmp_dir}/mixed-metadata.json"
python3 - "${metadata_file}" "${mixed_metadata_file}" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
mixed = source.replace('"version": "0.9.5"', '"version": "0.9.4"', 1)
if mixed == source:
    raise SystemExit("failed to create mixed-version Cargo metadata fixture")
pathlib.Path(sys.argv[2]).write_text(mixed, encoding="utf-8")
PY
expect_validation_failure \
  mixed-versions \
  "workspace packages have mixed versions: 0.9.4, 0.9.5" \
  "${mixed_metadata_file}" \
  codewhale-build-support \
  codewhale-core \
  codewhale-tui \
  codewhale-app-server \
  codewhale-cli

nonrelease_metadata_file="${tmp_dir}/nonrelease-metadata.json"
cat >"${nonrelease_metadata_file}" <<'JSON'
{
  "workspace_members": ["helper", "core"],
  "packages": [
    {
      "id": "helper",
      "name": "internal-helper",
      "version": "0.9.5",
      "dependencies": []
    },
    {
      "id": "core",
      "name": "codewhale-core",
      "version": "0.9.5",
      "dependencies": [
        {"name": "internal-helper", "path": "/workspace/helper", "kind": null}
      ]
    }
  ]
}
JSON
expect_validation_failure \
  nonrelease-workspace-dependency \
  "codewhale-core depends on workspace crate internal-helper [normal], which is not in the codewhale-* release inventory" \
  "${nonrelease_metadata_file}" \
  codewhale-core

bad_output="${tmp_dir}/bad-order.txt"
if python3 "${script_dir}/validate-crate-publish-order.py" \
  --metadata-file "${metadata_file}" \
  codewhale-build-support \
  codewhale-tui \
  codewhale-core \
  codewhale-app-server \
  codewhale-cli >"${bad_output}" 2>&1; then
  echo "v0.9.5 publication order unexpectedly passed" >&2
  exit 1
fi
grep -F \
  "codewhale-tui (position 2) depends on codewhale-core (position 3) [normal]" \
  "${bad_output}" >/dev/null

good_output="${tmp_dir}/good-order.txt"
python3 "${script_dir}/validate-crate-publish-order.py" \
  --metadata-file "${metadata_file}" \
  codewhale-build-support \
  codewhale-core \
  codewhale-tui \
  codewhale-app-server \
  codewhale-cli >"${good_output}"
grep -F $'version\t0.9.5\t' "${good_output}" >/dev/null
grep -F $'crate\tcodewhale-core\t1' "${good_output}" >/dev/null

independent_metadata_file="${tmp_dir}/independent-metadata.json"
python3 - "${metadata_file}" "${independent_metadata_file}" <<'PY'
import json
import pathlib
import sys

metadata = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
metadata["workspace_members"].append("ratatui")
metadata["packages"].append(
    {"id": "ratatui", "name": "codewhale-ratatui", "version": "0.2.0", "dependencies": []}
)
pathlib.Path(sys.argv[2]).write_text(json.dumps(metadata), encoding="utf-8")
PY
independent_output="${tmp_dir}/independent-order.txt"
python3 "${script_dir}/validate-crate-publish-order.py" \
  --metadata-file "${independent_metadata_file}" \
  codewhale-build-support \
  codewhale-ratatui \
  codewhale-core \
  codewhale-tui \
  codewhale-app-server \
  codewhale-cli >"${independent_output}"
grep -F $'version\t0.9.5\t' "${independent_output}" >/dev/null
grep -F $'own-version\tcodewhale-ratatui\t0.2.0' "${independent_output}" >/dev/null

# Keep the checked-in order synchronized with the live locked workspace graph.
# shellcheck source=scripts/release/crates.sh
source "${script_dir}/crates.sh"
python3 "${script_dir}/validate-crate-publish-order.py" \
  "${release_crates[@]}" >"${tmp_dir}/workspace-order.txt"

echo "crate publication order validation tests passed"
