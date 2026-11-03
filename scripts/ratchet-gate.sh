#!/usr/bin/env bash
# Run one whole-repo ratchet so a pull request is blocked by the debt it adds,
# not by the debt it inherits.
#
# Before 0.10.1 the four budget ratchets in ci.yml were advisory on every pull
# request and fatal on push. Every PR looked green, the regression surfaced only
# after merge, and main went red (38 of 154 main-push runs green, 09-16..09-22).
# This wrapper makes the ratchet block the PR and keeps exactly one escape
# hatch: when the base the PR merges into fails the same check, the failure is
# inherited debt, so it reports a warning instead of blocking the innocent PR.
#
# Usage:
#   scripts/ratchet-gate.sh --name NAME --update "CMD" [--base SHA] -- CHECK...
#
#   --name    label used in annotations (e.g. blocking-calls)
#   --update  the exact receipt command that lands the fix in this PR; printed
#             on failure so the author can regenerate and commit the budget
#   --base    commit to re-run the check on when it fails (the PR's merge
#             base). Defaults to $RATCHET_BASE_SHA; empty means no base
#             re-check, so the failure blocks (push, schedule, dispatch).
#   CHECK...  the checker command, run from the repository root, and again
#             from a detached checkout of --base when a base re-check runs.
#
# The base re-check needs a second checkout of an older commit. It uses a
# throwaway `git worktree` under $RUNNER_TEMP (or mktemp), removed on exit.
# That is a CI mechanism: locally, run the checker or scripts/preflight.sh.
# Cargo-backed base checks use an isolated target directory so an older
# dependency image cannot replace artifacts needed by later PR checks.
set -euo pipefail

name=""
update_cmd=""
base="${RATCHET_BASE_SHA:-}"
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --name) name="${2:?--name needs a value}"; shift 2 ;;
    --update) update_cmd="${2:?--update needs a value}"; shift 2 ;;
    --base) base="${2-}"; shift 2 ;;
    --) shift; break ;;
    *)
      echo "usage: $0 --name NAME --update CMD [--base SHA] -- CHECK..." >&2
      exit 2
      ;;
  esac
done
if [[ -z "${name}" || -z "${update_cmd}" || "$#" -eq 0 ]]; then
  echo "usage: $0 --name NAME --update CMD [--base SHA] -- CHECK..." >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${root}"

status=0
"$@" || status=$?
if [[ "${status}" -eq 0 ]]; then
  exit 0
fi

receipt() {
  echo "" >&2
  echo "To land the fix in this PR: remove the new sites, or if the growth is intended run" >&2
  echo "  ${update_cmd}" >&2
  echo "and commit the regenerated budget with the reason in the PR description." >&2
}

if [[ -z "${base}" ]]; then
  echo "::error title=${name} ratchet::${name} budget check failed (exit ${status}). Fix: ${update_cmd}" >&2
  receipt
  exit 1
fi

if ! git rev-parse -q --verify "${base}^{commit}" >/dev/null; then
  git fetch --no-tags --quiet origin "${base}" || true
fi
if ! git rev-parse -q --verify "${base}^{commit}" >/dev/null; then
  # Failing closed: without the base we cannot prove the debt is inherited.
  echo "::error title=${name} ratchet::${name} failed and base ${base} could not be resolved to prove the debt is inherited. Fix: ${update_cmd}" >&2
  receipt
  exit 1
fi

echo "[ratchet-gate] ${name} failed on this tree; re-running on base ${base} to tell added debt from inherited debt." >&2
scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/ratchet-base.XXXXXX")"
base_tree="${scratch}/tree"
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  git -C "${root}" worktree remove --force "${base_tree}" >/dev/null 2>&1 || true
  rm -rf "${scratch}"
}
trap cleanup EXIT
git worktree add --quiet --detach "${base_tree}" "${base}"

base_status=0
(
  cd "${base_tree}"
  export CARGO_TARGET_DIR="${scratch}/target"
  "$@"
) || base_status=$?

if [[ "${base_status}" -ne 0 ]]; then
  echo "::warning title=${name} ratchet (inherited)::${name} also fails on base ${base} (exit ${base_status}), so this is inherited debt, not added by this PR. Not blocking; main must be fixed with: ${update_cmd}" >&2
  exit 0
fi

echo "::error title=${name} ratchet::${name} passes on base ${base} but fails with this PR (exit ${status}): this change adds the debt. Fix: ${update_cmd}" >&2
receipt
exit 1
