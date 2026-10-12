#!/usr/bin/env bash
# Fails CI if version state is inconsistent across the workspace, npm
# wrapper, and Cargo.lock. Run on every push/PR so silent drift can't ship.
#
# Checks performed:
#   1. No `crates/*/Cargo.toml` carries a literal `version = "x.y.z"`; every
#      crate must inherit `version.workspace = true`.
#   2. Every crate inherits the workspace MSRV through
#      `rust-version.workspace = true`.
#   3. Release package manifests, the npm wrapper's binary pin, and their
#      workspace lock records match the workspace `version` in the root
#      `Cargo.toml`.
#      (`npm/deepseek-tui/` still exists only as an unpublished compatibility
#      notice and must stay private.)
#   4. Internal `codewhale-*` path dependency pins match the workspace version.
#   5. The TUI crate's packaged changelog copy matches root `CHANGELOG.md`.
#   6. The current version has either an explicit source-candidate entry or a
#      dated Keep a Changelog release entry and a matching compare link.
#   7. README contributor additions are mentioned in the current release entry.
#   8. `.github/SECURITY.md` keeps the dedicated security contact.
#   9. Generated website facts carry the workspace version.
#  10. Public install and version snippets point at the current release.
#  11. `codewhale-app-server` stays library-only; the shipped app-server
#      entrypoint belongs to `codewhale-cli`.
#  12. Issue-linked feature commits have a durable changelog receipt.
#  13. `Cargo.lock` is in sync with the manifests (`cargo metadata --locked`
#      fails if not).
#  14. Version-stamped contract fixtures (`providers-export.golden.json` and
#      the config-policy status baseline) match the workspace version.
set -euo pipefail

require_dated_release=0
# Checks 7 and 12 audit a *commit range* (previous tag..HEAD), not the working
# tree. Debt left by an already-merged commit therefore reddens this gate for
# every unrelated open pull request, and the PR that gets blamed is innocent.
# The per-PR CI job passes --range-audit-advisory so that class of failure
# reports without blocking; every release path (release-candidate.yml,
# auto-tag.yml, release.yml, prepare-release.sh) still runs them blocking, so
# nothing can be published without its receipt.
range_audit_advisory=0
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --require-dated-release) require_dated_release=1; shift ;;
    --range-audit-advisory) range_audit_advisory=1; shift ;;
    *)
      echo "Usage: $0 [--require-dated-release] [--range-audit-advisory]" >&2
      exit 2
      ;;
  esac
done
if [[ "${require_dated_release}" == "1" && "${range_audit_advisory}" == "1" ]]; then
  echo "::error::--range-audit-advisory must not be combined with --require-dated-release; publication requires the range audit to block." >&2
  exit 2
fi

cd "$(dirname "$0")/../.."

fail=0

# 1) Literal versions in crate manifests.
literals="$(grep -nE '^version = "' crates/*/Cargo.toml || true)"
if [[ -n "${literals}" ]]; then
  echo "::error::Crate manifests must use 'version.workspace = true', not literal versions:" >&2
  echo "${literals}" >&2
  fail=1
fi

# 2) Workspace MSRV inheritance. A workspace value is not included in package
# metadata unless every member opts in, which would silently omit the MSRV from
# all crates.io manifests.
missing_rust_version=""
for manifest in crates/*/Cargo.toml; do
  if ! grep -qxF 'rust-version.workspace = true' "${manifest}"; then
    missing_rust_version+="${manifest}"$'\n'
  fi
done
if [[ -n "${missing_rust_version}" ]]; then
  echo "::error::Every crate manifest must inherit the workspace MSRV with 'rust-version.workspace = true':" >&2
  printf '%s' "${missing_rust_version}" >&2
  fail=1
fi

# 3) Workspace ↔ npm package.json/version + binary pin ↔ root package lock.
workspace_version="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*/\1/')"
npm_version="$(node -p "require('./npm/codewhale/package.json').version")"
npm_binary_version="$(node -p "require('./npm/codewhale/package.json').codewhaleBinaryVersion ?? ''")"
if [[ "${workspace_version}" != "${npm_version}" ]]; then
  echo "::error::npm/codewhale/package.json version (${npm_version}) does not match workspace Cargo.toml (${workspace_version})." >&2
  fail=1
fi
if ! ./scripts/release/check-npm-binary-version.sh \
  "${workspace_version}" "${npm_binary_version}"; then
  fail=1
fi
lock_npm_version="$(node -p "require('./package-lock.json').packages?.['npm/codewhale']?.version ?? ''")"
if [[ "${workspace_version}" != "${lock_npm_version}" ]]; then
  echo "::error::package-lock.json npm/codewhale version (${lock_npm_version:-<missing>}) does not match workspace Cargo.toml (${workspace_version})." >&2
  echo "Run: npm install --package-lock-only --ignore-scripts" >&2
  fail=1
fi
runtime_sdk_version="$(node -p "require('./npm/runtime-sdk/package.json').version")"
lock_runtime_sdk_version="$(node -p "require('./package-lock.json').packages?.['npm/runtime-sdk']?.version ?? ''")"
vscode_version="$(node -p "require('./extensions/vscode/package.json').version")"
vscode_lock_version="$(node -p "require('./extensions/vscode/package-lock.json').version ?? ''")"
vscode_lock_workspace_version="$(node -p "require('./extensions/vscode/package-lock.json').packages?.['']?.version ?? ''")"
for pair in \
  "npm/runtime-sdk/package.json:${runtime_sdk_version}" \
  "package-lock.json npm/runtime-sdk:${lock_runtime_sdk_version}" \
  "extensions/vscode/package.json:${vscode_version}" \
  "extensions/vscode/package-lock.json:${vscode_lock_version}" \
  "extensions/vscode/package-lock.json workspace:${vscode_lock_workspace_version}"; do
  label="${pair%%:*}"
  actual="${pair#*:}"
  if [[ "${actual}" != "${workspace_version}" ]]; then
    echo "::error::${label} version (${actual:-<missing>}) does not match workspace Cargo.toml (${workspace_version})." >&2
    fail=1
  fi
done
if [[ -f npm/deepseek-tui/package.json ]]; then
  legacy_private="$(node -p "Boolean(require('./npm/deepseek-tui/package.json').private)")"
  legacy_publish_config="$(node -p "Boolean(require('./npm/deepseek-tui/package.json').publishConfig)")"
  if [[ "${legacy_private}" != "true" ]]; then
    echo "::error::npm/deepseek-tui/package.json must stay private so the legacy package is not republished." >&2
    fail=1
  fi
  if [[ "${legacy_publish_config}" == "true" ]]; then
    echo "::error::npm/deepseek-tui/package.json must not define publishConfig; the legacy package is deprecated." >&2
    fail=1
  fi
fi

# 4) Internal path dependency pins.
internal_dep_drift="$(
  grep -nE 'codewhale-[a-z-]+[[:space:]]*=[[:space:]]*\{[^}]*version[[:space:]]*=[[:space:]]*"' crates/*/Cargo.toml \
    | grep -vF 'codewhale-ratatui = { path = "../../vendor/codewhale-ratatui", version = "0.1.2" }' \
    | grep -v "version[[:space:]]*=[[:space:]]*\"${workspace_version}\"" || true
)"
if [[ -n "${internal_dep_drift}" ]]; then
  echo "::error::Internal codewhale-* path dependency versions must match workspace version ${workspace_version}:" >&2
  echo "${internal_dep_drift}" >&2
  fail=1
fi

# 5) Packaged TUI changelog slice (recent releases embedded in the binary).
if ! ./scripts/sync-changelog.sh --check >/dev/null 2>&1; then
  echo "::error::crates/tui/CHANGELOG.md is out of date with the root CHANGELOG.md slice." >&2
  echo "Run: ./scripts/sync-changelog.sh" >&2
  fail=1
fi

# 6) Current candidate/release-note shape. Normal branch CI must accept an
# honest source candidate. The release candidate, tag creation and public
# release workflows pass --require-dated-release so publication cannot proceed
# until the same entry has a real release date and tag-based compare link (an
# RC receipt only counts for the exact tag SHA, so it must see the dated entry).
current_section="$(
  awk -v version="${workspace_version}" '
    index($0, "## [" version "] - ") == 1 { in_section = 1; print; next }
    in_section && /^## \[/ { exit }
    in_section { print }
  ' CHANGELOG.md
)"
if [[ -z "${current_section}" ]]; then
  echo "::error::CHANGELOG.md must contain a section for ${workspace_version}." >&2
  fail=1
else
  dated_heading="## [${workspace_version}] - YYYY-MM-DD"
  candidate_heading="## [${workspace_version}] - Unreleased candidate"
  if [[ "${require_dated_release}" == "1" ]]; then
    if ! grep -qE "^## \\[${workspace_version}\\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" <<<"${current_section}"; then
      echo "::error::Publication requires CHANGELOG.md section ${workspace_version} to use '${dated_heading}'." >&2
      fail=1
    fi
  elif ! grep -qE "^## \\[${workspace_version}\\] - ([0-9]{4}-[0-9]{2}-[0-9]{2}|Unreleased candidate)$" <<<"${current_section}"; then
    echo "::error::CHANGELOG.md section ${workspace_version} must use '${candidate_heading}' or '${dated_heading}'." >&2
    fail=1
  fi
  if ! grep -qE "^### (Added|Changed|Deprecated|Removed|Fixed|Security)$" <<<"${current_section}"; then
    echo "::error::CHANGELOG.md section ${workspace_version} must contain at least one Keep a Changelog subsection." >&2
    fail=1
  fi
fi

compare_line="$(grep -E "^\\[${workspace_version}\\]: " CHANGELOG.md || true)"
if [[ -z "${compare_line}" ]]; then
  echo "::error::CHANGELOG.md must include a compare link for ${workspace_version}." >&2
  fail=1
elif [[ "${require_dated_release}" == "1" ]] &&
  ! grep -qE "^\\[${workspace_version}\\]: https://github.com/codewhale-hq/CodeWhale/compare/v[0-9]+\\.[0-9]+\\.[0-9]+\\.\\.\\.v${workspace_version}$" <<<"${compare_line}"; then
  echo "::error::Publication requires the ${workspace_version} compare link to end at v${workspace_version}." >&2
  fail=1
fi

unreleased_section="$(
  awk '
    index($0, "## [Unreleased]") == 1 { in_section = 1; print; next }
    in_section && /^## \[/ { exit }
    in_section { print }
  ' CHANGELOG.md
)"
credit_sections="${current_section}
${unreleased_section}"

# 7) Contributor-credit cross-check for README additions on the release branch.
# This cannot prove every external PR author has been credited, but it does
# catch the common release-polish failure mode: adding a README contributor row
# without mentioning that credit/correction in the current release entry. While
# a release branch is still unbumped, `[Unreleased]` is also a valid credit
# surface.
previous_tag=""
current_tag="v${workspace_version}"
if [[ "${compare_line}" =~ compare/(v[0-9]+\.[0-9]+\.[0-9]+)\.\.\.${current_tag} ]]; then
  previous_tag="${BASH_REMATCH[1]}"
fi
if [[ -n "${previous_tag}" ]]; then
  if ! git rev-parse -q --verify "refs/tags/${previous_tag}" >/dev/null; then
    git fetch --quiet --depth=1 origin "refs/tags/${previous_tag}:refs/tags/${previous_tag}" || true
  fi
  if git rev-parse -q --verify "refs/tags/${previous_tag}" >/dev/null; then
    if ! ./scripts/release/check-feature-release-notes.sh "${previous_tag}" HEAD; then
      if [[ "${range_audit_advisory}" == "1" ]]; then
        echo "::warning::Missing feature release-note receipt(s) above. Advisory here because this audits already-merged commits in ${previous_tag}..HEAD, not this change. It blocks every release path; fix it before the next release." >&2
      else
        fail=1
      fi
    fi
    while IFS= read -r line; do
      [[ -z "${line}" ]] && continue
      handle="$(sed -E 's#.*github.com/([^)/]+).*#\1#' <<<"${line}")"
      if [[ -n "${handle}" && "${handle}" != "${line}" ]]; then
        if ! grep -Fq "github.com/${handle}" <<<"${credit_sections}" && ! grep -Fq "@${handle}" <<<"${credit_sections}"; then
          if [[ "${range_audit_advisory}" == "1" ]]; then
            echo "::warning::README.md adds contributor @${handle}, but CHANGELOG.md ${workspace_version} or [Unreleased] does not mention that credit. Advisory here; blocking on every release path." >&2
          else
            echo "::error::README.md adds contributor @${handle}, but CHANGELOG.md ${workspace_version} or [Unreleased] does not mention that credit." >&2
            fail=1
          fi
        fi
      fi
    done < <(
      git diff "${previous_tag}..HEAD" -- README.md \
        | grep -E '^\+[-*] \*\*\[[^]]+\]\(https://github.com/[^)]+\)\*\*' || true
    )
  else
    # A gate that silently no-ops is worse than no gate: it reports success and
    # is read as evidence. If the tag cannot be resolved -- the fetch above is
    # `|| true` and a network blip is enough -- then the feature release-note
    # receipt check and the contributor-credit check did not run at all, and
    # this script must not imply that they passed.
    echo "::error::Cannot resolve refs/tags/${previous_tag}, so the feature release-note receipt and contributor-credit checks did not run. Fetch the tag and re-run; do not treat this as a pass." >&2
    if [[ "${CWC_ALLOW_MISSING_PREVIOUS_TAG:-}" == "1" ]]; then
      echo "::warning::CWC_ALLOW_MISSING_PREVIOUS_TAG=1 set, continuing with those two checks UNRUN." >&2
    else
      fail=1
    fi
  fi
fi

# 8) Security contact guard.
security_email="hmbown@gmail.com"
if ! grep -qF "${security_email}" .github/SECURITY.md; then
  echo "::error::.github/SECURITY.md must list ${security_email} as the security contact." >&2
  fail=1
fi
if grep -qF "hmbown.dev@gmail.com" .github/SECURITY.md; then
  echo "::error::.github/SECURITY.md must not use the alternate personal fallback email; use ${security_email}." >&2
  fail=1
fi

# 9) Generated web facts carry the workspace version. The file is ignored and
# generated during web builds, so a clean CI checkout must derive it before this
# release guard can inspect it.
if [[ ! -f web/lib/facts.generated.ts ]]; then
  node web/scripts/derive-facts.mjs
fi
facts_version="$(grep -oE '"version": "[0-9]+\.[0-9]+\.[0-9]+"' web/lib/facts.generated.ts | head -n1 | sed -E 's/.*"([0-9.]+)".*/\1/')"
if [[ "${facts_version}" != "${workspace_version}" ]]; then
  node web/scripts/derive-facts.mjs
  facts_version="$(grep -oE '"version": "[0-9]+\.[0-9]+\.[0-9]+"' web/lib/facts.generated.ts | head -n1 | sed -E 's/.*"([0-9.]+)".*/\1/')"
  if [[ "${facts_version}" != "${workspace_version}" ]]; then
    echo "::error::web/lib/facts.generated.ts version (${facts_version}) does not match workspace (${workspace_version}). Run: node web/scripts/derive-facts.mjs" >&2
    fail=1
  fi
fi

# 10) README install-tag examples point at the current release.
for readme in README.md README.zh-CN.md README.ja-JP.md README.vi.md README.ko-KR.md; do
  stale_tags="$(grep -nE -- "--tag v[0-9]+\.[0-9]+\.[0-9]+" "${readme}" | grep -v -- "--tag v${workspace_version}" || true)"
  if [[ -n "${stale_tags}" ]]; then
    echo "::error::${readme} has install examples pinned to an old tag (want v${workspace_version}):" >&2
    echo "${stale_tags}" >&2
    fail=1
  fi
done

# 10b) Public install/version snippets stay on the current release (#3767).
# `codewhale --version   # X.Y.Z` verify-your-install lines across README
# locales and docs/INSTALL.md, plus the docs/INSTALL.md npm-wrapper publish
# pointer ("published at vX.Y.Z"). These drifted while this gate still passed
# on a prior lane, so guard them explicitly. Narrowly scoped to those two
# snippet shapes to avoid flagging unrelated prose.
for doc in README.md README.zh-CN.md README.ja-JP.md README.vi.md README.ko-KR.md docs/INSTALL.md; do
  [[ -f "${doc}" ]] || continue
  stale_version_comments="$(grep -nE -- "codewhale --version[[:space:]]+#[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+" "${doc}" | grep -vE -- "#[[:space:]]*${workspace_version}([^0-9]|$)" || true)"
  if [[ -n "${stale_version_comments}" ]]; then
    echo "::error::${doc} has 'codewhale --version # X' snippet(s) not on ${workspace_version}:" >&2
    echo "${stale_version_comments}" >&2
    fail=1
  fi
done

# The publish pointer can wrap onto the next line; also scan the line after the lead-in.
wrapper_pointer_version="$(grep -A1 -E -- "wrapper is published at" docs/INSTALL.md | grep -oE -- "v[0-9]+\.[0-9]+\.[0-9]+" | head -n1 || true)"
if [[ -n "${wrapper_pointer_version}" && "${wrapper_pointer_version}" != "v${workspace_version}" ]]; then
  echo "::error::docs/INSTALL.md npm-wrapper publish pointer is ${wrapper_pointer_version}, want v${workspace_version}." >&2
  fail=1
fi

remote_smoke_tag="$(grep -oE 'RELEASE_TAG:-v[0-9]+\.[0-9]+\.[0-9]+' scripts/remote-smoke/setup-vm.sh | head -n1 | sed 's/.*:-//' || true)"
if [[ "${remote_smoke_tag}" != "v${workspace_version}" ]]; then
  echo "::error::scripts/remote-smoke/setup-vm.sh defaults to ${remote_smoke_tag:-<missing>}, want v${workspace_version}." >&2
  fail=1
fi

# 11) App-server is not a standalone binary.
app_server_bins="$(
  cargo metadata --locked --format-version 1 --no-deps \
    | node -e '
const fs = require("fs");
const metadata = JSON.parse(fs.readFileSync(0, "utf8"));
const pkg = metadata.packages.find((p) => p.name === "codewhale-app-server");
if (!pkg) {
  process.exit(2);
}
const bins = pkg.targets
  .filter((target) => target.kind.includes("bin"))
  .map((target) => target.name);
process.stdout.write(bins.join("\n"));
'
)"
if [[ -n "${app_server_bins}" ]]; then
  echo "::error::codewhale-app-server must stay library-only; use the codewhale-cli-owned 'codewhale app-server' entrypoint instead. Unexpected binary target(s):" >&2
  echo "${app_server_bins}" >&2
  fail=1
fi

# 13) Cargo.lock in sync.
if ! cargo metadata --locked --format-version 1 --no-deps >/dev/null 2>&1; then
  echo "::error::Cargo.lock is out of sync with the manifests. Run 'cargo update -p codewhale-tui' or 'cargo build' and commit the result." >&2
  fail=1
fi

# 14) Version-stamped contract fixtures match the workspace version. The
# 0.10.2 bump left providers-export.golden.json and the config-policy status
# baseline stamped 0.10.1; the test job caught both in CI while this gate said
# OK, at the cost of a full CI round. Two reads and a compare instead.
golden_fixture="crates/config/src/route/providers-export.golden.json"
if [[ -f "${golden_fixture}" ]]; then
  golden_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("runtimeVersion",""))' "${golden_fixture}" 2>/dev/null || true)"
  if [[ "${golden_version}" != "${workspace_version}" ]]; then
    echo "::error::${golden_fixture} is stamped '${golden_version:-<missing>}', want '${workspace_version}'. Regenerate with: WRITE_GOLDEN=1 cargo test -p codewhale-config --lib -- --ignored write_golden_providers_export_when_requested" >&2
    fail=1
  fi
else
  echo "::error::${golden_fixture} is missing, so its version stamp cannot be checked against '${workspace_version}'." >&2
  fail=1
fi
status_fixture="crates/tui/src/commands/contract/fixtures/config_policy/status.json"
if [[ -f "${status_fixture}" ]]; then
  status_fixture_line="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["message"].splitlines()[0])' "${status_fixture}" 2>/dev/null || true)"
  if [[ "${status_fixture_line}" != "codewhale ${workspace_version}" ]]; then
    echo "::error::${status_fixture} first line is '${status_fixture_line:-<missing>}', want 'codewhale ${workspace_version}'." >&2
    fail=1
  fi
else
  echo "::error::${status_fixture} is missing, so its version stamp cannot be checked against '${workspace_version}'." >&2
  fail=1
fi

if [[ "${fail}" -eq 0 ]]; then
  echo "Version state OK: workspace=${workspace_version}, npm=${npm_version}, npm-binary=${npm_binary_version}, lockfile in sync."
fi

exit "${fail}"
