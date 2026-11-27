#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

cat >"${tmp_dir}/CHANGELOG.md" <<'EOF'
## [Unreleased]

## [1.2.3] - 2026-07-14

Release summary and highlights.

### Upgrade notes

- Run the required migration once.

### Fixed

- A release fix.

### Contributors

- [@example](https://github.com/example) — report and implementation.

## [1.2.2] - 2026-07-01
EOF

body="$("${repo_root}/scripts/release/generate-release-body.sh" v1.2.3 "${tmp_dir}/CHANGELOG.md")"

[[ "${body%%$'\n'*}" == "## What's in v1.2.3" ]]
[[ "${body%%## Install*}" == *"Release summary and highlights."* ]]
[[ "${body%%Release summary and highlights.*}" == *$'### Upgrade notes\n\n- Run the required migration once.'* ]]
[[ "$(grep -c -- '- Run the required migration once.' <<<"${body}")" == 1 ]]
grep -Fq -- '<summary>Full release notes</summary>' <<<"${body}"
grep -Fq -- "- A release fix." <<<"${body}"
grep -Fq -- "## Contributors" <<<"${body}"
grep -Fq -- "[@example](https://github.com/example)" <<<"${body}"
grep -Fq -- 'codewhale-home:/home/codewhale/.codewhale' <<<"${body}"
grep -Fq -- 'codewhale-android-arm64.tar.gz' <<<"${body}"
grep -Fq -- 'codewhale-windows-arm64.zip' <<<"${body}"
grep -Fq -- 'codewhale.bat' <<<"${body}"
grep -Fq -- 'The image exposes the same runtime as both `codewhale` and `codew`.' <<<"${body}"
grep -Fq -- '### Recommended — official GitHub release' <<<"${body}"
grep -Fq -- 'curl -fsSL https://codewhale.net/install.sh | CODEWHALE_VERSION="v1.2.3" sh' <<<"${body}"
grep -Fq -- '### Secondary packaging — npm and Cargo' <<<"${body}"
grep -Fq -- 'byte-identical compatibility copies' <<<"${body}"
grep -Fq -- 'sha256sum -c codewhale-bundles-sha256.txt --ignore-missing' <<<"${body}"
grep -Fq -- 'sha256sum -c codewhale-artifacts-sha256.txt --ignore-missing' <<<"${body}"
grep -Fq -- 'shasum -a 256 -c codewhale-bundles-sha256.txt --ignore-missing' <<<"${body}"
grep -Fq -- 'shasum -a 256 -c codewhale-artifacts-sha256.txt --ignore-missing' <<<"${body}"

checksum_dir="${tmp_dir}/checksums"
mkdir -p "${checksum_dir}"
printf 'downloaded platform\n' >"${checksum_dir}/codewhale-linux-x64.tar.gz"
present_hash="$(sha256sum "${checksum_dir}/codewhale-linux-x64.tar.gz" | awk '{print $1}')"
{
  printf '%s  %s\n' "${present_hash}" "codewhale-linux-x64.tar.gz"
  printf '%064d  %s\n' 0 "codewhale-windows-x64.zip"
} >"${checksum_dir}/codewhale-bundles-sha256.txt"
(
  cd "${checksum_dir}"
  sha256sum -c codewhale-bundles-sha256.txt --ignore-missing >/dev/null
)
if grep -Fq -- "### Contributors" <<<"${body}"; then
  echo "nested contributor heading leaked into generated release body" >&2
  exit 1
fi

awk 'BEGIN {
  print "## [Unreleased]\n"
  print "## [2.0.0] - 2026-11-18\n"
  for (i = 0; i < 2200; i++) printf "Summary paragraph with spaces and CJK text. "
  print "\n\n### Fixed\n"
  for (i = 0; i < 1200; i++) printf "- Detailed release fix bullet with context. "
  print "\n\n### Contributors\n\n- [@large](https://github.com/large) — stress fixture.\n"
  print "## [1.9.9] - 2026-11-01"
}' >"${tmp_dir}/LARGE_CHANGELOG.md"

awk 'BEGIN {
  print "## [2.0.1] - 2026-11-18\n"
  for (i = 0; i < 3600; i++) printf "- Unclassified release line with spaces. "
  print "\n\n### Contributors\n\n- [@flat](https://github.com/flat) — fixture."
}' >"${tmp_dir}/FLAT_LARGE_CHANGELOG.md"

shells=("${BASH}")
if [[ -x /bin/bash && "${BASH}" != "/bin/bash" ]]; then
  shells+=("/bin/bash")
fi

for shell_bin in "${shells[@]}"; do
  SECONDS=0
  large_body="$("${shell_bin}" "${repo_root}/scripts/release/generate-release-body.sh" v2.0.0 "${tmp_dir}/LARGE_CHANGELOG.md")"
  (( SECONDS < 5 ))
  grep -Fq -- "Summary paragraph with spaces" <<<"${large_body}"
  grep -Fq -- "The full notes exceed GitHub's release-description limit." <<<"${large_body}"
  grep -Fq -- "[@large](https://github.com/large)" <<<"${large_body}"
  (( ${#large_body} < 125000 ))

  SECONDS=0
  flat_body="$("${shell_bin}" "${repo_root}/scripts/release/generate-release-body.sh" v2.0.1 "${tmp_dir}/FLAT_LARGE_CHANGELOG.md")"
  (( SECONDS < 5 ))
  grep -Fq -- "The full notes exceed GitHub's release-description limit." <<<"${flat_body}"
  grep -Fq -- "[@flat](https://github.com/flat)" <<<"${flat_body}"
  (( ${#flat_body} < 125000 ))
done

echo "generate-release-body tests passed"
