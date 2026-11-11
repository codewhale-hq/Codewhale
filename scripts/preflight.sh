#!/usr/bin/env bash
# One local preflight for the generated files and ratchets that otherwise turn
# main red after a green PR: run it before you push.
#
#   scripts/preflight.sh            regenerate what can be regenerated, check the rest
#   scripts/preflight.sh --check    change nothing; fail if anything is stale
#   scripts/preflight.sh --full     also run the cargo-backed runtime-contract and
#                                   persistence-backlog ratchets and the two
#                                   version-stamped fixture tests (minutes, offline)
#   scripts/preflight.sh --clippy   also run the Lint job's clippy step exactly as
#                                   CI runs it. Separate from --full because a
#                                   workspace --all-targets --all-features clippy
#                                   is far slower than everything else here, and it
#                                   is the one Lint step nothing else covered.
#   scripts/preflight.sh --base REF compare feature receipts against REF
#                                   (default: merge base with origin/main)
#
# As a pre-push hook it runs in --check mode:
#   ln -s ../../scripts/preflight.sh "$(git rev-parse --git-path hooks)/pre-push"
#
# What it covers, and the command that fixes each one:
#   - rustfmt (the Lint job's first    cargo fmt --all (written here)
#     step, required on every PR)
#   - crates/tui/CHANGELOG.md slice    scripts/sync-changelog.sh (written here)
#   - README locale stamps and links    retranslate; check-readme-translations.py
#                                       prints the new sha256 stamp to use
#   - product lexicon (warn-only)      python3 scripts/check-lexicon.py lists each hit
#   - website generated companions     cd web && npm run prebuild (check-facts.mjs and
#     (facts, agent docs, features,     check-cloud-facts.mjs); node web/scripts/
#     release baseline, cloud facts,    check-runtime-routes.mjs --write; skipped with
#     docs/RUNTIME_ROUTES.md)           a notice when node is not on PATH
#   - dead-code / blocking-calls         each ratchet's --update, committed in the
#     (and with --full runtime-contract, same PR with the reason in the PR body
#     persistence-backlog)
#   - feature release-note receipts      add each feat commit's #issue to
#     for this branch's own commits      CHANGELOG.md in the same PR
#   - clippy (with --clippy)            the Lint job's exact command; fix the lint
set -uo pipefail

mode="write"
full=0
clippy=0
base=""
if [[ "$(basename "$0")" == "pre-push" ]]; then
  # git passes <remote> <url>; the hook only ever checks.
  mode="check"
  set --
fi
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --check) mode="check"; shift ;;
    --full) full=1; shift ;;
    --clippy) clippy=1; shift ;;
    --base) base="${2:?--base needs a ref}"; shift 2 ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "usage: $0 [--check] [--full] [--clippy] [--base REF]" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || echo "${BASH_SOURCE[0]}")")/.." && pwd)"
cd "${root}" || exit 2

failed=()
step() {
  local label="$1" fix="$2"
  shift 2
  echo "== ${label}"
  if "$@"; then
    return 0
  fi
  failed+=("${label}: ${fix}")
}

if [[ "${mode}" == "write" ]]; then
  step "rustfmt" "cargo fmt --all" cargo fmt --all
  step "TUI changelog slice" "scripts/sync-changelog.sh" ./scripts/sync-changelog.sh
else
  step "rustfmt" "cargo fmt --all" cargo fmt --all -- --check
  step "TUI changelog slice" "scripts/sync-changelog.sh" ./scripts/sync-changelog.sh --check
fi
step "README translations in sync" \
  "retranslate the changed README sections, then update each stamp to the sha256 printed above" \
  python3 scripts/check-readme-translations.py
step "README locale link symmetry" "link every README.<locale>.md from README.md" \
  bash scripts/check-readme-locales.sh
# Warn-only: lists retired product words and engineering notes in English
# copy; never fails the preflight.
step "product lexicon (warn-only)" "python3 scripts/check-lexicon.py" \
  python3 scripts/check-lexicon.py --summary
step "dead-code budget" "python3 scripts/check-dead-code-budget.py --update" \
  python3 scripts/check-dead-code-budget.py
step "blocking-calls budget" "python3 scripts/check-blocking-calls-budget.py --update" \
  python3 scripts/check-blocking-calls-budget.py
if command -v node >/dev/null 2>&1; then
  step "website generated facts" \
    "cd web && npm ci (if node_modules is missing) && npm run prebuild, then commit web/lib/facts.generated.ts and web/data/*.generated.json and web/data/agent-docs.release.json and agent-docs.release-index.json (check-facts.mjs and check-release-docs.mjs name the stale file); for a stale docs/RUNTIME_ROUTES.md run node web/scripts/check-runtime-routes.mjs --write, then node web/scripts/derive-facts.mjs" \
    bash -c 'rc=0; node web/scripts/check-facts.mjs || rc=1; node web/scripts/check-cloud-facts.mjs || rc=1; node web/scripts/check-release-docs.mjs || rc=1; node web/scripts/check-runtime-routes.mjs || rc=1; exit "${rc}"'
else
  echo "== website generated facts: SKIPPED, node is not on PATH; run cd web && npm run check:facts && node scripts/check-runtime-routes.mjs before pushing" >&2
fi
if [[ "${full}" == "1" ]]; then
  step "runtime-contract budget" \
    "python3 scripts/check-runtime-contract-budget.py --update --allow-increase" \
    python3 scripts/check-runtime-contract-budget.py
  step "persistence-backlog budget" \
    "python3 scripts/check-persistence-backlog-budget.py --update" \
    python3 scripts/check-persistence-backlog-budget.py
  step "version-stamped fixtures" \
    "regenerate the stale fixture the failing test names, then commit it" \
    scripts/dev-cargo.sh nextest run --locked --lib -p codewhale-config -p codewhale-tui \
    -E 'test(golden_providers_export_matches_registry) | test(status_public_output_and_read_only_state_match_baseline)'
fi
if [[ "${clippy}" == "1" ]]; then
  # Exactly the Lint job's clippy step. Two main-side failures in one night were
  # a redundant closure and an unformatted test that this would have caught;
  # the Lint job is what turns main red, so run its command, not a subset.
  step "clippy (Lint job's command)" \
    "fix the lint, or run the same command to see it in full: cargo clippy --workspace --all-targets --all-features --locked" \
    cargo clippy --workspace --all-targets --all-features --locked -- \
    -D warnings \
    -A clippy::uninlined_format_args \
    -A clippy::too_many_arguments \
    -A clippy::unnecessary_map_or
fi

if [[ -z "${base}" ]]; then
  base="$(git merge-base HEAD origin/main 2>/dev/null || true)"
fi
if [[ -n "${base}" ]]; then
  step "feature release-note receipts (${base:0:12}..HEAD)" \
    "add each feat commit's #issue to CHANGELOG.md in this branch" \
    ./scripts/release/check-feature-release-notes.sh "${base}" HEAD
else
  echo "== feature release-note receipts: no base to compare against (no origin/main; pass --base REF)" >&2
  failed+=("feature release-note receipts: no base to compare against; pass --base REF")
fi

echo
if [[ "${#failed[@]}" -gt 0 ]]; then
  echo "preflight: ${#failed[@]} check(s) failed. Fix, commit the result, and re-run:" >&2
  for line in "${failed[@]}"; do
    echo "  - ${line}" >&2
  done
  exit 1
fi
if [[ "${mode}" == "write" ]] && ! git diff --quiet -- crates/tui/CHANGELOG.md; then
  echo "preflight: OK, and crates/tui/CHANGELOG.md was regenerated -- commit it."
else
  echo "preflight: OK"
fi
