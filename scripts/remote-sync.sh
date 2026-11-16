#!/usr/bin/env bash
# remote-sync.sh — run on the Mac. Overlays the local Codewhale worktree onto
# whale-h100's clone at ~/CodeWhale (local → remote, one direction only).
#
# .git is excluded, so the remote clone keeps its git state and `git status`
# there shows exactly the local diff. The remaining excludes are heavy
# *ignored* directories that contain zero tracked files (verified with
# `git ls-files`): build output and dependency caches that no cargo build
# needs. Without them the first sync would push ~4.4G over the WAN for nothing.
#
# One-time remote setup: whale-h100:~/lane-setup.sh (rustup stable, mold,
# sccache, apt deps). Remote artifacts are x86_64 — CI-lane evidence only,
# never a local test result or a shipped artifact.
#
# .devin/ is hidden from local status by .git/info/exclude, which does not
# sync — without this exclude it lands untracked on the remote and breaks
# the local-diff parity check.
set -euo pipefail

SRC="${SRC:-/Volumes/VIXinSSD/CW/codewhale/}"
DEST="${DEST:-whale-h100:~/CodeWhale/}"
SSH_HOST="${SSH_HOST:-whale-h100}"

rsync -az --delete --itemize-changes \
  --exclude .git \
  --exclude target \
  --exclude node_modules \
  --exclude .entire \
  --exclude .mimosa \
  --exclude codewhale-inference \
  --exclude pet/conformance-results \
  --exclude extensions/vscode/out \
  --exclude npm/codewhale/bin/downloads \
  --exclude .devin \
  "$SRC" "$DEST"

echo '--- remote git status:'
ssh -o BatchMode=yes "$SSH_HOST" 'cd ~/CodeWhale && git status --short'
