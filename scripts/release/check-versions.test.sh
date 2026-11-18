#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
export GIT_CONFIG_COUNT=2
export GIT_CONFIG_KEY_0=maintenance.auto GIT_CONFIG_VALUE_0=false
export GIT_CONFIG_KEY_1=gc.auto GIT_CONFIG_VALUE_1=0
trap 'rm -rf "${tmp_dir}"' EXIT

root="${tmp_dir}/repo"
mkdir -p \
  "${root}/.github" \
  "${root}/bin" \
  "${root}/crates/config/src/route" \
  "${root}/crates/example" \
  "${root}/crates/tui/src/commands/contract/fixtures/config_policy" \
  "${root}/docs/i18n" \
  "${root}/extensions/vscode" \
  "${root}/npm/codewhale" \
  "${root}/npm/deepseek-tui" \
  "${root}/npm/runtime-sdk" \
  "${root}/scripts/release" \
  "${root}/scripts/remote-smoke" \
  "${root}/vendor/codewhale-ratatui" \
  "${root}/web/lib"

cp "${repo_root}/scripts/release/check-versions.sh" "${root}/scripts/release/check-versions.sh"
cp "${repo_root}/scripts/release/check-npm-binary-version.sh" "${root}/scripts/release/check-npm-binary-version.sh"
cp "${repo_root}/scripts/release/check-feature-release-notes.sh" "${root}/scripts/release/check-feature-release-notes.sh"
cp "${repo_root}/scripts/sync-changelog.sh" "${root}/scripts/sync-changelog.sh"

cat >"${root}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "metadata" ]]; then
  printf '%s\n' '{"packages":[{"name":"codewhale-app-server","targets":[{"kind":["lib"],"name":"codewhale_app_server"}]}]}'
  exit 0
fi
exit 0
EOF
chmod +x "${root}/bin/cargo" \
  "${root}/scripts/release/check-versions.sh" \
  "${root}/scripts/release/check-npm-binary-version.sh" \
  "${root}/scripts/release/check-feature-release-notes.sh" \
  "${root}/scripts/sync-changelog.sh"

cat >"${root}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/example"]

[workspace.package]
version = "0.10.3"
EOF

cat >"${root}/crates/example/Cargo.toml" <<'EOF'
[package]
name = "codewhale-example"
version.workspace = true
rust-version.workspace = true

[dependencies]
codewhale-core = { path = "../core", version = "0.10.3" }
EOF

cat >"${root}/vendor/codewhale-ratatui/Cargo.toml" <<'EOF'
[package]
name = "codewhale-ratatui"
version = "0.2.0"

[dependencies]
codewhale-protocol = { path = "../../crates/protocol", version = "0.10.3" }
EOF

cat >"${root}/npm/codewhale/package.json" <<'EOF'
{"name":"codewhale","version":"0.10.3","codewhaleBinaryVersion":"0.10.3"}
EOF
cat >"${root}/npm/runtime-sdk/package.json" <<'EOF'
{"name":"@codewhale/runtime-sdk","version":"0.10.3"}
EOF
cat >"${root}/npm/deepseek-tui/package.json" <<'EOF'
{"name":"deepseek-tui","version":"0.8.49","private":true}
EOF
cat >"${root}/extensions/vscode/package.json" <<'EOF'
{"name":"codewhale-vscode","version":"0.10.3"}
EOF
cat >"${root}/extensions/vscode/package-lock.json" <<'EOF'
{"name":"codewhale-vscode","version":"0.10.3","packages":{"":{"version":"0.10.3"}}}
EOF
cat >"${root}/package-lock.json" <<'EOF'
{"name":"fixture","packages":{"npm/codewhale":{"version":"0.10.3"},"npm/runtime-sdk":{"version":"0.10.3"}}}
EOF
printf 'Security: hmbown@gmail.com\n' >"${root}/.github/SECURITY.md"
printf 'export const FACTS = { "version": "0.10.3" };\n' >"${root}/web/lib/facts.generated.ts"
printf 'Install Codewhale.\n' >"${root}/README.md"
printf 'Install Codewhale.\n' >"${root}/docs/i18n/README.de.md"
printf 'Install guide for v0.10.3 source candidate.\n' >"${root}/docs/INSTALL.md"
printf 'RELEASE_TAG="${RELEASE_TAG:-v0.10.3}"\n' >"${root}/scripts/remote-smoke/setup-vm.sh"
printf '{"runtimeVersion":"0.10.3"}\n' >"${root}/crates/config/src/route/providers-export.golden.json"
printf '{"message":"codewhale 0.10.3\\n"}\n' >"${root}/crates/tui/src/commands/contract/fixtures/config_policy/status.json"
printf 'Old archive\n' >"${root}/docs/CHANGELOG_ARCHIVE.md"

cat >"${root}/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

- **New slice (#101):** shipped after the candidate date.

## [0.10.3] - 2026-11-08

This is the Codewhale v0.10.3 source candidate. It is not a published release.

### Work in progress and known limits

This source candidate is not the final 0.10.3 tag.

### Added

- Initial 0.10.3 item.

[Unreleased]: https://github.com/codewhale-hq/CodeWhale/compare/v0.10.2...HEAD
[0.10.3]: https://github.com/codewhale-hq/CodeWhale/compare/v0.10.2...v0.10.3
EOF

cat >"${root}/docs/CONTRIBUTORS.md" <<'EOF'
<details open>
<summary><strong>Unreleased — landed after v0.10.3</strong></summary>

- **[alice](https://github.com/alice)** — feature (#101).

</details>

<details open>
<summary><strong>v0.10.3 — unreleased candidate</strong></summary>

- **[bob](https://github.com/bob)** — fix (#100).

</details>
EOF

cat >"${root}/web/lib/release-credits.ts" <<'EOF'
export const RELEASE_CONTRIBUTORS: string[] = ["@bob"];
export const UNRELEASED_CONTRIBUTORS: string[] = ["@alice"];
export const RELEASE_HELPERS: string[] = [];
EOF

"${root}/scripts/sync-changelog.sh" >/dev/null

git -C "${root}" init -q -b main
git -C "${root}" config user.name "Release Test"
git -C "${root}" config user.email "release-test@example.invalid"
git -C "${root}" add .
git -C "${root}" commit -q -m "base"
git -C "${root}" tag v0.10.2
git -C "${root}" commit -q --allow-empty -m "feat: add new slice (#101)"

PATH="${root}/bin:${PATH}" "${root}/scripts/release/check-versions.sh" >/dev/null

set +e
dated_err="$(PATH="${root}/bin:${PATH}" "${root}/scripts/release/check-versions.sh" --require-dated-release 2>&1)"
dated_rc=$?
set -e
[[ "${dated_rc}" -ne 0 ]]
grep -Fq "CHANGELOG.md [Unreleased] to be folded" <<<"${dated_err}"
grep -Fq "replace source-candidate and work-in-progress text" <<<"${dated_err}"
grep -Fq "docs/CONTRIBUTORS.md to fold the Unreleased band" <<<"${dated_err}"
grep -Fq "UNRELEASED_CONTRIBUTORS to be moved" <<<"${dated_err}"

cat >"${root}/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

## [0.10.3] - 2026-11-18

Codewhale v0.10.3 ships the new slice and initial item.

### Added

- **New slice (#101):** shipped in 0.10.3.
- Initial 0.10.3 item.

[Unreleased]: https://github.com/codewhale-hq/CodeWhale/compare/v0.10.3...HEAD
[0.10.3]: https://github.com/codewhale-hq/CodeWhale/compare/v0.10.2...v0.10.3
EOF

cat >"${root}/docs/CONTRIBUTORS.md" <<'EOF'
<details open>
<summary><strong>v0.10.3 — plugins, permissions and registry</strong></summary>

- **[alice](https://github.com/alice)** — feature (#101).
- **[bob](https://github.com/bob)** — fix (#100).

</details>
EOF

cat >"${root}/web/lib/release-credits.ts" <<'EOF'
export const RELEASE_CONTRIBUTORS: string[] = ["@bob", "@alice"];
export const UNRELEASED_CONTRIBUTORS: string[] = [];
export const RELEASE_HELPERS: string[] = [];
EOF

"${root}/scripts/sync-changelog.sh" >/dev/null
PATH="${root}/bin:${PATH}" "${root}/scripts/release/check-versions.sh" --require-dated-release >/dev/null

echo "check-versions tests passed"
