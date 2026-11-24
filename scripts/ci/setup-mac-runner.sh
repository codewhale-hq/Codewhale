#!/usr/bin/env bash
# Register this Mac as a self-hosted GitHub Actions runner for the macOS legs.
#
# WHY: GitHub's hosted macOS queue routinely takes 50+ minutes, which is the
# single slowest gate on every PR. This machine finishes the same work in a
# fraction of that.
#
# SAFETY — read this before running it.
#
# codewhale-hq/CodeWhale is a PUBLIC repository with thousands of forks. A
# self-hosted runner that accepts pull requests from forks lets anyone who
# opens a PR execute arbitrary code on this machine, as this user. That is the
# one CI configuration GitHub explicitly warns against, and on this machine it
# would expose ~/.codewhale/secrets.json, the gh token, and the whole CW
# workspace.
#
# .github/workflows/ci.yml therefore routes jobs to this runner ONLY for a
# workflow_dispatch in codewhale-hq/Codewhale ("Choose the macOS suite
# runner"); every pull request, fork or not, stays on GitHub-hosted runners.
# That guard cannot stop a fork PR that edits a workflow to name this runner's
# labels: only requiring approval for all outside contributors' workflow runs,
# or a runner group restricted to selected workflows, closes that path.
#
# Each job gets a freshly registered --ephemeral runner: the registration is
# consumed by one job and then discarded, so a job cannot persist a runner
# registration for later use. Note this is NOT filesystem isolation — an
# ephemeral runner still runs as this user on this disk. For real isolation,
# run this inside a throwaway VM (Tart, UTM, or a dedicated macOS user with no
# access to the secrets above).
#
# Usage:  scripts/ci/setup-mac-runner.sh [--once]
# Requires: gh, authenticated with admin rights on the repo.

set -euo pipefail

REPO="${RUNNER_REPO:-codewhale-hq/CodeWhale}"
RUNNER_VERSION="${RUNNER_VERSION:-2.337.0}"
RUNNER_HOME="${RUNNER_HOME:-$HOME/actions-runner}"
LABELS="${RUNNER_LABELS:-self-hosted,macOS,ARM64,codewhale-mac}"
RUNNER_NAME="${RUNNER_NAME:-$(scutil --get LocalHostName 2>/dev/null || hostname -s)-cw}"

case "$(uname -m)" in
  arm64) ARCH="osx-arm64" ;;
  x86_64) ARCH="osx-x64" ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

command -v gh >/dev/null || { echo "gh is required" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "run: gh auth login" >&2; exit 1; }

if [ ! -x "$RUNNER_HOME/run.sh" ]; then
  echo "==> installing actions-runner $RUNNER_VERSION ($ARCH) into $RUNNER_HOME"
  mkdir -p "$RUNNER_HOME"
  TARBALL="actions-runner-${ARCH}-${RUNNER_VERSION}.tar.gz"
  curl -fsSL -o "$RUNNER_HOME/$TARBALL" \
    "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/${TARBALL}"
  tar -xzf "$RUNNER_HOME/$TARBALL" -C "$RUNNER_HOME"
  rm -f "$RUNNER_HOME/$TARBALL"
fi

register_and_run() {
  # Registration tokens are single-use and expire in ~1 hour, so mint a fresh
  # one per job rather than caching it.
  local token
  token="$(gh api -X POST "repos/${REPO}/actions/runners/registration-token" --jq .token)"
  [ -n "$token" ] || { echo "failed to mint a registration token" >&2; return 1; }

  ( cd "$RUNNER_HOME" && ./config.sh \
      --url "https://github.com/${REPO}" \
      --token "$token" \
      --name "$RUNNER_NAME" \
      --labels "$LABELS" \
      --work _work \
      --ephemeral \
      --unattended \
      --replace >/dev/null )

  # --ephemeral: run.sh exits after exactly one job, leaving no registration.
  ( cd "$RUNNER_HOME" && ./run.sh )
}

if [ "${1:-}" = "--once" ]; then
  register_and_run
  exit 0
fi

echo "==> serving jobs for ${REPO} as '${RUNNER_NAME}' [${LABELS}]"
echo "    one ephemeral registration per job; Ctrl-C to stop"
while true; do
  register_and_run || { echo "runner exited abnormally; retrying in 15s" >&2; sleep 15; }
done
