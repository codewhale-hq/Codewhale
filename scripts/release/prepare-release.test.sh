#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

make_fixture() {
  local root="$1"
  mkdir -p \
    "${root}/bin" \
    "${root}/crates/example" \
    "${root}/crates/tui" \
    "${root}/docs/i18n" \
    "${root}/extensions/vscode" \
    "${root}/npm/codewhale" \
    "${root}/npm/runtime-sdk" \
    "${root}/scripts/remote-smoke" \
    "${root}/scripts/release" \
    "${root}/web/lib" \
    "${root}/web/scripts"

  cp "${repo_root}/scripts/release/prepare-release.sh" \
    "${root}/scripts/release/prepare-release.sh"

  cat >"${root}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/example"]

[workspace.package]
version = "0.8.68"
EOF

  cat >"${root}/crates/example/Cargo.toml" <<'EOF'
[package]
name = "codewhale-example"
version.workspace = true

[dependencies]
codewhale-core = { path = "../core", version = "0.8.68" }
EOF

  cat >"${root}/npm/codewhale/package.json" <<'EOF'
{
  "name": "codewhale",
  "version": "0.8.68",
  "codewhaleBinaryVersion": "0.8.68"
}
EOF

  cat >"${root}/npm/runtime-sdk/package.json" <<'EOF'
{
  "name": "@codewhale/runtime-sdk",
  "version": "0.8.68"
}
EOF

  cat >"${root}/extensions/vscode/package.json" <<'EOF'
{
  "name": "codewhale-vscode",
  "version": "0.8.68"
}
EOF

  cat >"${root}/extensions/vscode/package-lock.json" <<'EOF'
{
  "name": "codewhale-vscode",
  "version": "0.8.68",
  "lockfileVersion": 3,
  "packages": {
    "": {
      "name": "codewhale-vscode",
      "version": "0.8.68"
    }
  }
}
EOF

  cat >"${root}/package-lock.json" <<'EOF'
{
  "name": "fixture",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "fixture" },
    "npm/codewhale": {
      "version": "0.8.68",
      "license": "MIT"
    },
    "npm/runtime-sdk": {
      "version": "0.8.68",
      "license": "MIT"
    }
  }
}
EOF

  cat >"${root}/Cargo.lock" <<'EOF'
# fixture lock
EOF

  cat >"${root}/CHANGELOG.md" <<'EOF'
## [Unreleased]

## [0.9.0] - 2026-07-15

### Changed

- Test release.
EOF

  cat >"${root}/docs/INSTALL.md" <<'EOF'
The npm wrapper installs the matching published binaries.

codewhale --version   # prints the published version that was installed
EOF

  cat >"${root}/docs/public-surface-facts.json" <<'EOF'
{
  "sourceCandidate": { "version": "0.8.68" },
  "latestPublishedRelease": {
    "tag": "v0.8.67",
    "version": "0.8.67",
    "url": "https://example.invalid/v0.8.67"
  },
  "screenshot": { "sourceVersion": "0.8.67" }
}
EOF

  cat >"${root}/scripts/remote-smoke/setup-vm.sh" <<'EOF'
#!/usr/bin/env bash
RELEASE_TAG="${RELEASE_TAG:-v0.8.68}"
EOF

  printf 'fixture packaged changelog\n' >"${root}/crates/tui/CHANGELOG.md"
  printf 'fixture generated facts\n' >"${root}/web/lib/facts.generated.ts"

  mkdir -p \
    "${root}/crates/config/src/route" \
    "${root}/crates/tui/src/commands/contract/fixtures/config_policy"
  cat >"${root}/crates/config/src/route/providers-export.golden.json" <<'EOF'
{
  "schemaVersion": 1,
  "runtimeVersion": "0.8.68",
  "routes": []
}
EOF
  cat >"${root}/crates/tui/src/commands/contract/fixtures/config_policy/status.json" <<'EOF'
{
  "message": "codewhale 0.8.68\n\n  Route: fixture\n"
}
EOF

  for readme in README.md docs/i18n/README.{zh-CN,ja-JP,vi,ko-KR,de}.md; do
    printf 'Install Codewhale from the package manager.\n' >"${root}/${readme}"
  done

  cat >"${root}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${PREPARE_RELEASE_TEST_MARKERS}/cargo"
EOF

  cat >"${root}/scripts/sync-changelog.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${PREPARE_RELEASE_TEST_MARKERS}/sync-changelog"
EOF

  cat >"${root}/scripts/release/check-versions.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${PREPARE_RELEASE_TEST_MARKERS}/check-versions"
EOF

  cat >"${root}/scripts/release/check-ohos-deps.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${PREPARE_RELEASE_TEST_MARKERS}/check-ohos-deps"
EOF

  cat >"${root}/web/scripts/derive-facts.mjs" <<'EOF'
import { writeFileSync } from "node:fs";

writeFileSync(`${process.env.PREPARE_RELEASE_TEST_MARKERS}/derive-facts`, "");
EOF

  chmod +x \
    "${root}/bin/cargo" \
    "${root}/scripts/release/prepare-release.sh" \
    "${root}/scripts/release/check-ohos-deps.sh" \
    "${root}/scripts/release/check-versions.sh" \
    "${root}/scripts/sync-changelog.sh"
}

success_root="${tmp_dir}/success"
success_markers="${tmp_dir}/success-markers"
make_fixture "${success_root}"
mkdir -p "${success_markers}"
printf 'Install from a release tag: --tag v0.8.68\n' >"${success_root}/README.md"

PREPARE_RELEASE_TEST_MARKERS="${success_markers}" \
  PATH="${success_root}/bin:${PATH}" \
  "${success_root}/scripts/release/prepare-release.sh" 0.9.0 >/dev/null

grep -Fq 'version = "0.9.0"' "${success_root}/Cargo.toml"
grep -Fq 'version = "0.9.0"' "${success_root}/crates/example/Cargo.toml"
grep -Fq '"version": "0.9.0"' "${success_root}/npm/codewhale/package.json"
grep -Fq '"codewhaleBinaryVersion": "0.9.0"' \
  "${success_root}/npm/codewhale/package.json"
grep -Fq '"version": "0.9.0"' "${success_root}/npm/runtime-sdk/package.json"
grep -Fq '"version": "0.9.0"' "${success_root}/extensions/vscode/package.json"
[[ "$(grep -Fc '"version": "0.9.0"' "${success_root}/extensions/vscode/package-lock.json")" == "2" ]]
[[ "$(grep -Fc '"version": "0.9.0"' "${success_root}/package-lock.json")" == "2" ]]
grep -Fq 'RELEASE_TAG="${RELEASE_TAG:-v0.9.0}"' \
  "${success_root}/scripts/remote-smoke/setup-vm.sh"
grep -Fq '"sourceCandidate": { "version": "0.9.0" }' \
  "${success_root}/docs/public-surface-facts.json"
grep -Fq '"runtimeVersion": "0.9.0"' \
  "${success_root}/crates/config/src/route/providers-export.golden.json"
grep -Fq 'codewhale 0.9.0' \
  "${success_root}/crates/tui/src/commands/contract/fixtures/config_policy/status.json"
grep -Fq '"tag": "v0.8.67"' \
  "${success_root}/docs/public-surface-facts.json"
grep -Fq '"sourceVersion": "0.8.67"' \
  "${success_root}/docs/public-surface-facts.json"
grep -Fq 'prints the published version that was installed' \
  "${success_root}/docs/INSTALL.md"
grep -Fq -- '--tag v0.9.0' "${success_root}/README.md"
if grep -R -E -- '--tag v[0-9]+\.[0-9]+\.[0-9]+' \
  "${success_root}"/docs/i18n/README.*.md; then
  echo "tag-free localized README unexpectedly gained a release tag" >&2
  exit 1
fi
for marker in cargo sync-changelog derive-facts check-versions check-ohos-deps; do
  [[ -f "${success_markers}/${marker}" ]] || {
    echo "prepare-release did not reach ${marker}" >&2
    exit 1
  }
done

same_root="${tmp_dir}/same"
same_markers="${tmp_dir}/same-markers"
same_log="${tmp_dir}/same.log"
make_fixture "${same_root}"
mkdir -p "${same_markers}"
cat >"${same_root}/CHANGELOG.md" <<'EOF'
## [Unreleased]

## [0.8.68] - 2026-07-18

### Changed

- Test already-prepared release.
EOF

PREPARE_RELEASE_TEST_MARKERS="${same_markers}" \
  PATH="${same_root}/bin:${PATH}" \
  "${same_root}/scripts/release/prepare-release.sh" 0.8.68 \
  >"${same_log}"

grep -Fq \
  'Workspace is already at 0.8.68; refreshing generated release state and rerunning gates.' \
  "${same_log}"
for marker in sync-changelog derive-facts check-versions check-ohos-deps; do
  [[ -f "${same_markers}/${marker}" ]] || {
    echo "same-version prepare-release did not reach ${marker}" >&2
    exit 1
  }
done
if [[ -f "${same_markers}/cargo" ]]; then
  echo "same-version prepare-release unexpectedly mutated Cargo.lock" >&2
  exit 1
fi
grep -Fq 'version = "0.8.68"' "${same_root}/Cargo.toml"
grep -Fq 'version = "0.8.68"' "${same_root}/crates/example/Cargo.toml"
grep -Fq '"version": "0.8.68"' "${same_root}/npm/codewhale/package.json"

same_stale_root="${tmp_dir}/same-stale"
same_stale_markers="${tmp_dir}/same-stale-markers"
same_stale_log="${tmp_dir}/same-stale.log"
make_fixture "${same_stale_root}"
mkdir -p "${same_stale_markers}"
python3 - "${same_stale_root}/docs/public-surface-facts.json" <<'PY'
import pathlib, sys

path = pathlib.Path(sys.argv[1])
path.write_text(path.read_text().replace(
    '"sourceCandidate": { "version": "0.8.68" }',
    '"sourceCandidate": { "version": "0.8.67" }',
))
PY

if PREPARE_RELEASE_TEST_MARKERS="${same_stale_markers}" \
  PATH="${same_stale_root}/bin:${PATH}" \
  "${same_stale_root}/scripts/release/prepare-release.sh" 0.8.68 \
  >"${same_stale_log}" 2>&1; then
  echo "same-version stale source candidate unexpectedly passed" >&2
  exit 1
fi
grep -Fq \
  "sourceCandidate.version is '0.8.67'; expected '0.8.68'" \
  "${same_stale_log}"
if find "${same_stale_markers}" -type f -print -quit | grep -q .; then
  echo "same-version stale source candidate reached downstream release steps" >&2
  exit 1
fi

legacy_root="${tmp_dir}/legacy"
legacy_markers="${tmp_dir}/legacy-markers"
make_fixture "${legacy_root}"
mkdir -p "${legacy_markers}"
cat >"${legacy_root}/docs/INSTALL.md" <<'EOF'
The npm wrapper is published at v0.8.68.
codewhale --version   # 0.8.68
EOF

PREPARE_RELEASE_TEST_MARKERS="${legacy_markers}" \
  PATH="${legacy_root}/bin:${PATH}" \
  "${legacy_root}/scripts/release/prepare-release.sh" 0.9.0 >/dev/null

grep -Fq 'wrapper is published at v0.9.0' "${legacy_root}/docs/INSTALL.md"
grep -Fq 'codewhale --version   # 0.9.0' "${legacy_root}/docs/INSTALL.md"

stale_root="${tmp_dir}/stale"
stale_markers="${tmp_dir}/stale-markers"
stale_log="${tmp_dir}/stale.log"
make_fixture "${stale_root}"
mkdir -p "${stale_markers}"
printf 'Stale install example: --tag v0.8.67\n' >"${stale_root}/docs/i18n/README.de.md"

if PREPARE_RELEASE_TEST_MARKERS="${stale_markers}" \
  PATH="${stale_root}/bin:${PATH}" \
  "${stale_root}/scripts/release/prepare-release.sh" 0.9.0 \
  >"${stale_log}" 2>&1; then
  echo "stale README release tag unexpectedly passed" >&2
  exit 1
fi

grep -Fq \
  'docs/i18n/README.de.md has release tag version(s) 0.8.67; expected 0.8.68' \
  "${stale_log}"
grep -Fq 'version = "0.8.68"' "${stale_root}/Cargo.toml"
grep -Fq 'version = "0.8.68"' "${stale_root}/crates/example/Cargo.toml"
grep -Fq '"version": "0.8.68"' "${stale_root}/npm/codewhale/package.json"
if find "${stale_markers}" -type f -print -quit | grep -q .; then
  echo "stale README validation mutated downstream release state" >&2
  exit 1
fi

# A downstream command failure must restore every release-bearing file, not
# strand the fixture after the text mutation phase.
failure_root="${tmp_dir}/failure"
failure_snapshot="${tmp_dir}/failure-before"
failure_markers="${tmp_dir}/failure-markers"
failure_log="${tmp_dir}/failure.log"
make_fixture "${failure_root}"
mkdir -p "${failure_markers}"
printf 'Install from a release tag: --tag v0.8.68\ncodewhale --version # 0.8.68\n' \
  >"${failure_root}/docs/i18n/README.de.md"
cp -R "${failure_root}" "${failure_snapshot}"
cat >"${failure_root}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 42
EOF
chmod +x "${failure_root}/bin/cargo"
# Snapshot the intentional failing cargo shim too; only release preparation's
# effects should be absent after rollback.
cp "${failure_root}/bin/cargo" "${failure_snapshot}/bin/cargo"

if PREPARE_RELEASE_TEST_MARKERS="${failure_markers}" \
  PATH="${failure_root}/bin:${PATH}" \
  "${failure_root}/scripts/release/prepare-release.sh" 0.9.0 \
  >"${failure_log}" 2>&1; then
  echo "failing cargo refresh unexpectedly passed" >&2
  exit 1
fi

grep -Fq "restored the checkout's pre-run release files" "${failure_log}"
diff -ru "${failure_snapshot}" "${failure_root}"

echo "prepare-release tests passed"
