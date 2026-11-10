#!/usr/bin/env bash
# Prove that a remote release tag still points at the immutable source SHA
# resolved before a public release write.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <remote-or-url> <vX.Y.Z> <expected-commit-sha>" >&2
  exit 2
fi

remote="$1"
tag="$2"
expected_sha="$3"

if ! [[ "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: release tag '${tag}' must use vX.Y.Z or vX.Y.Z-prerelease" >&2
  exit 2
fi
if ! [[ "${expected_sha}" =~ ^[0-9a-fA-F]{40}$ ]]; then
  echo "error: expected commit SHA must be a full 40-character SHA-1" >&2
  exit 2
fi

remote_sha="$(
  git ls-remote --tags "${remote}" "refs/tags/${tag}^{}" \
    | awk 'NR == 1 {print $1}'
)"
if [[ -z "${remote_sha}" ]]; then
  remote_sha="$(
    git ls-remote --tags "${remote}" "refs/tags/${tag}" \
      | awk 'NR == 1 {print $1}'
  )"
fi
if [[ -z "${remote_sha}" ]]; then
  echo "::error::Remote tag ${tag} does not exist on ${remote}." >&2
  exit 1
fi
if [[ "${remote_sha}" != "${expected_sha}" ]]; then
  echo "::error::Remote tag ${tag} moved before publication." >&2
  echo "  expected: ${expected_sha}" >&2
  echo "  remote  : ${remote_sha}" >&2
  exit 1
fi

echo "Remote tag check OK: ${tag} -> ${remote_sha}"
