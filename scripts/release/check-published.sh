#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
# shellcheck source=scripts/release/crates.sh
source "${script_dir}/crates.sh"

usage() {
  cat <<'EOF'
usage: scripts/release/check-published.sh [--allow-npm-binary-mismatch] [VERSION]

Verifies that a release version is visible on both npm and crates.io.
Defaults VERSION to the workspace version in Cargo.toml.

Use --allow-npm-binary-mismatch only for npm packaging-only releases where
the npm package intentionally points at an older GitHub binary release.
EOF
}

allow_npm_binary_mismatch=0
version=""

while (($# > 0)); do
  case "$1" in
    --allow-npm-binary-mismatch)
      allow_npm_binary_mismatch=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [[ -n "${version}" ]]; then
        usage >&2
        exit 2
      fi
      version="$1"
      ;;
  esac
  shift
done

cd "${repo_root}"

if [[ -z "${version}" ]]; then
  version="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*/\1/')"
fi

if [[ -z "${version}" ]]; then
  echo "Could not determine release version." >&2
  exit 1
fi

fail=0

echo "Checking published release ${version}..."

# Canonical post-rebrand npm package.
if npm_version="$(npm view "codewhale@${version}" version 2>/dev/null)"; then
  echo "npm codewhale@${npm_version} is published."
else
  echo "npm codewhale@${version} is not published." >&2
  fail=1
fi

# `codewhaleBinaryVersion` is the new internal version-pin field. Fall back
# to the legacy `deepseekBinaryVersion` field for old/transition packages.
binary_field=""
npm_binary_version=""
if value="$(npm view "codewhale@${version}" codewhaleBinaryVersion 2>/dev/null)" && [[ -n "${value}" ]]; then
  binary_field="codewhaleBinaryVersion"
  npm_binary_version="${value}"
elif value="$(npm view "codewhale@${version}" deepseekBinaryVersion 2>/dev/null)" && [[ -n "${value}" ]]; then
  binary_field="deepseekBinaryVersion"
  npm_binary_version="${value}"
fi

if [[ -n "${binary_field}" ]]; then
  if [[ "${npm_binary_version}" == "${version}" ]]; then
    echo "npm ${binary_field}=${npm_binary_version}."
  elif [[ "${allow_npm_binary_mismatch}" == "1" ]]; then
    echo "npm ${binary_field}=${npm_binary_version} (allowed packaging-only mismatch)."
  else
    echo "npm ${binary_field}=${npm_binary_version}, expected ${version}." >&2
    fail=1
  fi
elif [[ "${allow_npm_binary_mismatch}" == "1" ]]; then
  echo "npm codewhaleBinaryVersion is absent (allowed packaging-only mismatch)."
else
  echo "npm codewhaleBinaryVersion is absent for codewhale@${version}." >&2
  fail=1
fi

# Legacy `deepseek-tui` npm package. It is deprecated and must not be
# republished under the release version.
if legacy_version="$(npm view "deepseek-tui@${version}" version 2>/dev/null)"; then
  echo "npm deepseek-tui@${legacy_version} exists, but the legacy npm package must not be republished." >&2
  fail=1
fi
if legacy_deprecated="$(npm view deepseek-tui deprecated 2>/dev/null)" && [[ -n "${legacy_deprecated}" ]]; then
  echo "npm deepseek-tui is deprecated: ${legacy_deprecated}"
else
  echo "npm deepseek-tui is not marked deprecated." >&2
  fail=1
fi

own_versions="$(
  python3 "${script_dir}/validate-crate-publish-order.py" "${release_crates[@]}" |
    awk -F'\t' '$1 == "own-version" { print $2 "=" $3 }'
)"

crate_version() {
  local own
  own="$(awk -F= -v name="$1" '$1 == name { print $2 }' <<<"${own_versions}")"
  echo "${own:-${version}}"
}

for crate in "${release_crates[@]}"; do
  crate_release_version="$(crate_version "${crate}")"
  if crate_index_has "${crate}" "${crate_release_version}"; then
    echo "crates.io ${crate}@${crate_release_version} is published."
  else
    echo "crates.io ${crate}@${crate_release_version} is not published." >&2
    fail=1
  fi
done

if [[ "${fail}" == "0" ]]; then
  echo "Published release OK: npm codewhale@${version} and ${#release_crates[@]} crates are visible."
fi

exit "${fail}"
