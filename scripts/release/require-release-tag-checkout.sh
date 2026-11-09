#!/usr/bin/env bash
# Fail closed before irreversible Cargo/npm publication unless this checkout is
# the clean, exact source commit anchored by the matching remote release tag.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"

version="${1:-}"
if [[ -z "${version}" ]]; then
  version="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*/\1/')"
fi
version="${version#v}"
if ! [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: release version '${version}' must use X.Y.Z or X.Y.Z-prerelease" >&2
  exit 2
fi

tag="v${version}"
head_sha="$(git rev-parse --verify 'HEAD^{commit}')"
tag_sha="$(git rev-parse --verify "refs/tags/${tag}^{commit}" 2>/dev/null || true)"
if [[ -z "${tag_sha}" ]]; then
  echo "::error::Local release tag ${tag} does not exist." >&2
  exit 1
fi
if [[ "${head_sha}" != "${tag_sha}" ]]; then
  echo "::error::Refusing registry publish from HEAD ${head_sha}; ${tag} is ${tag_sha}." >&2
  echo "Publication requires clean canonical main at the approved ${tag} commit; reconcile its state before retrying." >&2
  exit 1
fi

dirty="$(git status --porcelain=v1 --untracked-files=all)"
if [[ -n "${dirty}" ]]; then
  echo "::error::Refusing registry publish from a dirty ${tag} checkout:" >&2
  printf '%s\n' "${dirty}" >&2
  exit 1
fi

workspace_version="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*/\1/')"
npm_version="$(node -p "require('./npm/codewhale/package.json').version")"
binary_version="$(node -p "require('./npm/codewhale/package.json').codewhaleBinaryVersion")"
for pair in "workspace:${workspace_version}" "npm:${npm_version}"; do
  label="${pair%%:*}"
  actual="${pair#*:}"
  if [[ "${actual}" != "${version}" ]]; then
    echo "::error::${label} version ${actual} does not match ${tag}." >&2
    exit 1
  fi
done
if [[ "${binary_version}" != "${version}" ]]; then
  if [[ "${CODEWHALE_ALLOW_NPM_BINARY_MISMATCH:-0}" == "1" ]]; then
    echo "Packaging-only release: ${tag} points at binary release ${binary_version}."
  else
    echo "::error::npm binary version ${binary_version} does not match ${tag}." >&2
    echo "Set CODEWHALE_ALLOW_NPM_BINARY_MISMATCH=1 only for an intentional packaging-only npm release." >&2
    exit 1
  fi
fi

remote="${CODEWHALE_RELEASE_REMOTE:-origin}"
"${repo_root}/scripts/release/verify-remote-tag.sh" \
  "${remote}" \
  "${tag}" \
  "${head_sha}"

echo "Release checkout gate OK: clean ${tag} at ${head_sha}."
