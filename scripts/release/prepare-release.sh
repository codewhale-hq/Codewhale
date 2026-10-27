#!/usr/bin/env bash
# Bump every version-bearing file for a release in one shot.
#
# Usage: ./scripts/release/prepare-release.sh <new-version>
#
# Touches: Cargo.toml (workspace version), crates/*/Cargo.toml (internal
# codewhale-* dependency pins), npm/codewhale/package.json (version +
# codewhaleBinaryVersion), npm/runtime-sdk/package.json, the VS Code extension
# package and lock, the root npm lock workspace records, the remote-smoke default
# tag, README.md and docs/i18n/README*.md install-tag examples when present, the public fact matrix's
# source-candidate version, Cargo.lock, crates/tui/CHANGELOG.md (via
# sync-changelog.sh), web/lib/facts.generated.ts (via derive-facts.mjs), and
# the two version-stamped contract fixtures
# (crates/config/src/route/providers-export.golden.json and
# crates/tui/src/commands/contract/fixtures/config_policy/status.json).
# The website's changelog and install-guide modules are not tracked; the web
# build and test runs derive them from CHANGELOG.md and docs/INSTALL.md.
#
# It does NOT write the CHANGELOG entry — add the `## [X.Y.Z] - YYYY-MM-DD`
# section first (see docs/RELEASE_CHECKLIST.md), then run this script, then
# let check-versions.sh (run at the end here) confirm everything agrees.
set -euo pipefail

new="${1:?usage: $0 <new-version>}"
if ! [[ "${new}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: '${new}' is not a plain X.Y.Z version" >&2
  exit 1
fi

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo}"
readmes=(README.md docs/i18n/README.*.md)

# Release preparation spans generated files and two package managers. Preserve
# the exact starting bytes so any validation, generator, or downstream gate
# failure cannot strand a half-bumped checkout. The backup lives outside the
# repository and is removed on both success and failure.
transaction_dir="$(mktemp -d)"
transaction_active=1
transaction_existing="${transaction_dir}/existing"
transaction_missing="${transaction_dir}/missing"
: >"${transaction_existing}"
: >"${transaction_missing}"

transaction_paths=(
  Cargo.toml
  Cargo.lock
  package-lock.json
  npm/codewhale/package.json
  npm/runtime-sdk/package.json
  extensions/vscode/package.json
  extensions/vscode/package-lock.json
  scripts/remote-smoke/setup-vm.sh
  docs/public-surface-facts.json
  docs/INSTALL.md
  "${readmes[@]}"
  crates/tui/CHANGELOG.md
  web/lib/facts.generated.ts
  crates/config/src/route/providers-export.golden.json
  crates/tui/src/commands/contract/fixtures/config_policy/status.json
)
for manifest in crates/*/Cargo.toml; do
  transaction_paths+=("${manifest}")
done
for path in "${transaction_paths[@]}"; do
  if [[ -e "${path}" ]]; then
    mkdir -p "${transaction_dir}/files/$(dirname "${path}")"
    cp -p "${path}" "${transaction_dir}/files/${path}"
    printf '%s\n' "${path}" >>"${transaction_existing}"
  else
    printf '%s\n' "${path}" >>"${transaction_missing}"
  fi
done

finish_transaction() {
  status=$?
  trap - EXIT
  if [[ "${transaction_active}" == "1" && "${status}" != "0" ]]; then
    while IFS= read -r path; do
      [[ -n "${path}" ]] || continue
      cp -p "${transaction_dir}/files/${path}" "${path}"
    done <"${transaction_existing}"
    while IFS= read -r path; do
      [[ -n "${path}" ]] || continue
      rm -f -- "${path}"
    done <"${transaction_missing}"
    echo "Release preparation failed; restored the checkout's pre-run release files." >&2
  fi
  rm -rf -- "${transaction_dir}"
  exit "${status}"
}
trap finish_transaction EXIT

old="$(grep -E '^version = "' Cargo.toml | head -n1 | sed -E 's/^version = "([^"]+)".*/\1/')"
if ! grep -q "^## \[${new}\]" CHANGELOG.md; then
  echo "warning: CHANGELOG.md has no '## [${new}]' entry yet — add it before tagging" >&2
fi

if [[ "${old}" != "${new}" ]]; then
  echo "Bumping ${old} -> ${new}"

  OLD_VERSION="${old}" NEW_VERSION="${new}" python3 - "${readmes[@]}" <<'PY'
import json, os, pathlib, re, sys

old, new = os.environ["OLD_VERSION"], os.environ["NEW_VERSION"]
old_re = re.escape(old)
readmes = sys.argv[1:]

def bump(path, pattern, repl, minimum):
    p = pathlib.Path(path)
    text = p.read_text()
    out, n = re.subn(pattern, repl, text, flags=re.MULTILINE)
    if n < minimum:
        sys.exit(f"error: expected >= {minimum} replacement(s) in {path}, made {n}")
    p.write_text(out)
    print(f"  {path}: {n} replacement(s)")

# Validate every versioned README install tag before writing any file. A README
# with no pinned tag is valid; if a tag exists, it must match the workspace so
# the release helper cannot silently preserve stale public install instructions.
release_tag_pattern = re.compile(r"--tag v([0-9]+\.[0-9]+\.[0-9]+)\b")
for readme in readmes:
    versions = sorted(set(release_tag_pattern.findall(pathlib.Path(readme).read_text())))
    stale = [version for version in versions if version != old]
    if stale:
        found = ", ".join(stale)
        sys.exit(
            f"error: {readme} has release tag version(s) {found}; expected {old}"
        )

# 1) Workspace version.
bump("Cargo.toml", rf'^version = "{old_re}"$', f'version = "{new}"', 1)

# 2) Internal codewhale-* dependency pins in every crate manifest.
total = 0
for manifest in sorted(pathlib.Path("crates").glob("*/Cargo.toml")):
    text = manifest.read_text()
    out, n = re.subn(
        rf'(codewhale-[a-z0-9-]+\s*=\s*\{{[^}}]*version = "){old_re}(")',
        rf"\g<1>{new}\g<2>",
        text,
    )
    if n:
        manifest.write_text(out)
        print(f"  {manifest}: {n} pin(s)")
        total += n
if total == 0:
    sys.exit("error: no internal dependency pins were bumped — wrong old version?")

# 3) npm wrapper.
bump(
    "npm/codewhale/package.json",
    rf'("(?:version|codewhaleBinaryVersion)": "){old_re}(")',
    rf"\g<1>{new}\g<2>",
    2,
)

# The runtime SDK and VS Code extension are versioned release artifacts too.
bump(
    "npm/runtime-sdk/package.json",
    rf'^(  "version": "){old_re}(",?)$',
    rf"\g<1>{new}\g<2>",
    1,
)
bump(
    "extensions/vscode/package.json",
    rf'^(  "version": "){old_re}(",?)$',
    rf"\g<1>{new}\g<2>",
    1,
)

# 4) README install-tag examples (all translations, when present).
for readme in readmes:
    p = pathlib.Path(readme)
    text = p.read_text()
    out, n = re.subn(rf"--tag v{old_re}\b", f"--tag v{new}", text)
    if n:
        p.write_text(out)
        print(f"  {readme}: {n} install-tag replacement(s)")
    else:
        print(f"  {readme}: no versioned install-tag example; skipped")

# 5) Legacy numeric install/version snippets, if a branch still carries them.
#    Current docs deliberately describe installed output generically, so zero
#    matches is valid. If numeric forms exist, validate that they agree with
#    the old workspace version before replacing them.
version_doc_files = [*readmes, "docs/INSTALL.md"]
for doc in version_doc_files:
    p = pathlib.Path(doc)
    text = p.read_text()
    versions = sorted(set(re.findall(r"codewhale --version\s+#\s*([0-9]+\.[0-9]+\.[0-9]+)\b", text)))
    stale = [version for version in versions if version != old]
    if stale:
        sys.exit(
            f"error: {doc} has version-comment value(s) {', '.join(stale)}; expected {old}"
        )
    out, n = re.subn(
        rf"(codewhale --version\s+#\s*){old_re}\b", rf"\g<1>{new}", text
    )
    if n:
        p.write_text(out)
        print(f"  {doc}: {n} version-comment replacement(s)")

install = pathlib.Path("docs/INSTALL.md")
install_text = install.read_text()
pointer_versions = sorted(set(re.findall(r"wrapper is published at\s+v([0-9]+\.[0-9]+\.[0-9]+)\b", install_text)))
stale_pointers = [version for version in pointer_versions if version != old]
if stale_pointers:
    sys.exit(
        "error: docs/INSTALL.md has npm-wrapper publish pointer version(s) "
        f"{', '.join(stale_pointers)}; expected {old}"
    )
install_out, pointer_hits = re.subn(
    rf"(wrapper is published at\s+)v{old_re}\b",
    rf"\g<1>v{new}",
    install_text,
)
# web/lib/public-surface-contract.test.ts asserts docs/INSTALL.md contains
# `v${FACTS.version} source candidate`, so the phrase has to track the bump or
# Web Frontend goes red on the next push.
install_out, candidate_hits = re.subn(
    rf"\bv{old_re}( source candidate)",
    rf"v{new}\g<1>",
    install_out,
)
if candidate_hits > 1:
    sys.exit(
        "error: docs/INSTALL.md names 'v{0} source candidate' {1} times; "
        "exactly one occurrence is expected".format(old, candidate_hits)
    )
if pointer_hits or candidate_hits:
    install.write_text(install_out)
    print(
        f"  docs/INSTALL.md: {pointer_hits} publish-pointer replacement(s), "
        f"{candidate_hits} source-candidate replacement(s)"
    )

# 6) npm lock workspace records. Keep dependency records byte-stable.
lock = pathlib.Path("package-lock.json")
lock_text = lock.read_text()
lock_out, wrapper_lock_hits = re.subn(
    rf'("npm/codewhale"\s*:\s*\{{[\s\S]*?"version"\s*:\s*"){old_re}(")',
    rf"\g<1>{new}\g<2>",
    lock_text,
    count=1,
)
if wrapper_lock_hits != 1:
    sys.exit(
        "error: expected package-lock.json packages['npm/codewhale'].version "
        f"to be {old}; made {wrapper_lock_hits} replacement(s)"
    )
lock_out, sdk_lock_hits = re.subn(
    rf'("npm/runtime-sdk"\s*:\s*\{{[\s\S]*?"version"\s*:\s*"){old_re}(")',
    rf"\g<1>{new}\g<2>",
    lock_out,
    count=1,
)
if sdk_lock_hits != 1:
    sys.exit(
        "error: expected package-lock.json packages['npm/runtime-sdk'].version "
        f"to be {old}; made {sdk_lock_hits} replacement(s)"
    )
lock.write_text(lock_out)
print("  package-lock.json: 2 npm workspace replacements")

vscode_lock = pathlib.Path("extensions/vscode/package-lock.json")
vscode_lock_text = vscode_lock.read_text()
vscode_lock_json = json.loads(vscode_lock_text)
if vscode_lock_json.get("version") != old:
    sys.exit(
        "error: extensions/vscode/package-lock.json root version is "
        f"{vscode_lock_json.get('version')!r}; expected {old!r}"
    )
workspace_record = vscode_lock_json.get("packages", {}).get("", {})
if workspace_record.get("version") != old:
    sys.exit(
        "error: extensions/vscode/package-lock.json packages[''].version is "
        f"{workspace_record.get('version')!r}; expected {old!r}"
    )
vscode_lock_out, vscode_lock_hits = re.subn(
    rf'("version"\s*:\s*"){old_re}(")',
    rf"\g<1>{new}\g<2>",
    vscode_lock_text,
    count=2,
)
if vscode_lock_hits != 2:
    sys.exit(
        "error: expected two VS Code package-lock version replacements; "
        f"made {vscode_lock_hits}"
    )
vscode_lock_after = json.loads(vscode_lock_out)
if (
    vscode_lock_after.get("version") != new
    or vscode_lock_after.get("packages", {}).get("", {}).get("version") != new
):
    sys.exit("error: VS Code package-lock version replacement hit wrong records")
vscode_lock.write_text(vscode_lock_out)
print("  extensions/vscode/package-lock.json: 2 workspace replacements")

# 7) Remote published-asset smoke defaults to the version being prepared.
bump(
    "scripts/remote-smoke/setup-vm.sh",
    rf'(RELEASE_TAG="\$\{{RELEASE_TAG:-v){old_re}(\}}")',
    rf"\g<1>{new}\g<2>",
    1,
)

# 8) Public facts distinguish source candidate from latest published release.
#    Change only sourceCandidate.version; the published object and screenshot
#    provenance must remain untouched until separately evidenced.
facts = pathlib.Path("docs/public-surface-facts.json")
facts_text = facts.read_text()
facts_json = json.loads(facts_text)
candidate = facts_json.get("sourceCandidate", {})
if candidate.get("version") != old:
    sys.exit(
        "error: docs/public-surface-facts.json sourceCandidate.version is "
        f"{candidate.get('version')!r}; expected {old!r}"
    )
published_before = facts_json.get("latestPublishedRelease")
facts_out, facts_hits = re.subn(
    rf'("sourceCandidate"\s*:\s*\{{[\s\S]*?"version"\s*:\s*"){old_re}(")',
    rf"\g<1>{new}\g<2>",
    facts_text,
    count=1,
)
if facts_hits != 1:
    sys.exit("error: failed to update sourceCandidate.version exactly once")
# The trust matrix states the telemetry posture in prose that names the source
# candidate, and web/lib/public-surface-contract.test.ts asserts the two agree
# (`Codewhale ${matrix.sourceCandidate.version} counts anonymous usage by
# default`). Bumping the field without the sentence reddens Web Frontend on the
# next push, which is how it broke at 0.9.14.
facts_out, telemetry_hits = re.subn(
    rf"(Codewhale ){old_re}( counts anonymous usage by default)",
    rf"\g<1>{new}\g<2>",
    facts_out,
)
if telemetry_hits > 1:
    sys.exit(
        "error: the trust.telemetry source-candidate sentence appears {0} times; "
        "exactly one occurrence is expected".format(telemetry_hits)
    )
facts_after = json.loads(facts_out)
if facts_after.get("latestPublishedRelease") != published_before:
    sys.exit("error: release preparation must not change latestPublishedRelease")
facts.write_text(facts_out)
print("  docs/public-surface-facts.json: 1 source-candidate replacement")

# 7) Version-stamped contract fixtures. The golden export and the config
#    policy status baseline are compared against the workspace version by
#    their own tests; check-versions.sh (run below) guards them too. Move
#    them with every bump or CI's test job reddens on the new version.
golden_fixture = pathlib.Path("crates/config/src/route/providers-export.golden.json")
if golden_fixture.exists():
    text = golden_fixture.read_text()
    out, n = re.subn(rf'("runtimeVersion"\s*:\s*"){old_re}(")', rf"\g<1>{new}\g<2>", text)
    if n != 1:
        sys.exit(f"error: expected exactly one runtimeVersion in {golden_fixture}, made {n}")
    golden_fixture.write_text(out)
    print(f"  {golden_fixture}: 1 runtimeVersion replacement")
status_fixture = pathlib.Path(
    "crates/tui/src/commands/contract/fixtures/config_policy/status.json"
)
if status_fixture.exists():
    text = status_fixture.read_text()
    out, n = re.subn(rf"\b(codewhale\s+){old_re}\b", rf"\g<1>{new}", text, count=1)
    if n != 1:
        sys.exit(
            f"error: expected the {status_fixture} header to carry {old}, "
            f"made {n} replacement(s)"
        )
    status_fixture.write_text(out)
    print(f"  {status_fixture}: 1 version replacement")
PY

  echo "Refreshing Cargo.lock..."
  cargo update --workspace --offline >/dev/null
else
  echo "Workspace is already at ${new}; refreshing generated release state and rerunning gates."
  NEW_VERSION="${new}" python3 - <<'PY'
import json, os, pathlib, sys

expected = os.environ["NEW_VERSION"]
facts = pathlib.Path("docs/public-surface-facts.json")
candidate = json.loads(facts.read_text()).get("sourceCandidate", {})
actual = candidate.get("version")
if actual != expected:
    sys.exit(
        "error: docs/public-surface-facts.json sourceCandidate.version is "
        f"{actual!r}; expected {expected!r}"
    )
PY
fi

echo "Regenerating crates/tui/CHANGELOG.md slice..."
./scripts/sync-changelog.sh

echo "Regenerating web/lib/facts.generated.ts..."
node web/scripts/derive-facts.mjs

echo "Validating..."
./scripts/release/check-versions.sh
./scripts/release/check-ohos-deps.sh
transaction_active=0
echo "Done. Review 'git diff', commit, and follow docs/RELEASE_CHECKLIST.md."
