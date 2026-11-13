#!/usr/bin/env bash
# Refresh or check the first-party catalog snapshot embedded in the Engine
# (crates/plugins/assets/first-party-marketplace.json) against the head of the
# public marketplace repository.
#
#   scripts/refresh-first-party-marketplace.sh            rewrite the snapshot from the marketplace head
#   scripts/refresh-first-party-marketplace.sh --check    fail when the snapshot is older than the head
#   --marketplace DIR                                     use this checkout instead of cloning the head
#
# Nothing is installed, trusted or executed. "Older" means the files the
# catalog installs from (marketplace.json, plugins/, skills/) differ between the
# snapshot's revision and the head; unrelated commits do not make it stale.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
snapshot="${root}/crates/plugins/assets/first-party-marketplace.json"
repo_url="https://github.com/codewhale-hq/codewhale-plugin-marketplace"
mode=refresh
checkout=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) mode=check; shift ;;
    --marketplace)
      [[ $# -ge 2 ]] || { echo "--marketplace needs a directory" >&2; exit 2; }
      checkout="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "${checkout}" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT
  git clone --quiet --filter=blob:none --no-checkout "${repo_url}" "${tmp}/marketplace"
  checkout="${tmp}/marketplace"
fi

head_rev="$(git -C "${checkout}" rev-parse HEAD)"

if [[ "${mode}" == refresh ]]; then
  validate="$(cat <<'PY'
import json, re, sys
catalog = json.load(sys.stdin)
problems = []
for plugin in catalog["plugins"]:
    name, source = plugin["name"], plugin["source"]
    if source.startswith("path:") or re.fullmatch(r"github:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", source):
        continue
    if "/latest/" in source:
        problems.append(f"{name}: follows a floating release ({source})")
    if not re.fullmatch(r"[0-9a-f]{64}", plugin.get("sha256", "")):
        problems.append(f"{name}: remote source has no sha256")
    for field in ("version", "platforms"):
        if not plugin.get(field):
            problems.append(f"{name}: remote source has no {field}")
if problems:
    sys.exit("marketplace head is not publishable as a snapshot:\n  " + "\n  ".join(problems))
PY
)"
  git -C "${checkout}" show HEAD:marketplace.json | python3 -c "${validate}"
  python3 "${root}/scripts/sync-marketplace.py" --marketplace "${checkout}"
  exit 0
fi

snapshot_rev="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["revision"])' "${snapshot}")"
if [[ "${snapshot_rev}" == "${head_rev}" ]]; then
  echo "First-party catalog snapshot is at the marketplace head ${head_rev}."
  exit 0
fi
if ! git -C "${checkout}" cat-file -e "${snapshot_rev}^{commit}" 2>/dev/null; then
  echo "::error::Snapshot revision ${snapshot_rev} is not in the marketplace history; run scripts/refresh-first-party-marketplace.sh." >&2
  exit 1
fi
tree_ids() {
  local rev="$1" path
  for path in marketplace.json plugins skills; do
    git -C "${checkout}" rev-parse "${rev}:${path}" 2>/dev/null || echo "missing:${path}"
  done
}
if [[ "$(tree_ids "${snapshot_rev}")" == "$(tree_ids "${head_rev}")" ]]; then
  echo "First-party catalog snapshot (${snapshot_rev}) is current: nothing it installs from changed up to ${head_rev}."
  exit 0
fi
echo "::error::The first-party catalog snapshot (${snapshot_rev}) is older than the marketplace head (${head_rev}). Run scripts/refresh-first-party-marketplace.sh, review the diff and commit it." >&2
git -C "${checkout}" diff --stat "${snapshot_rev}" "${head_rev}" -- marketplace.json plugins skills >&2 || true
exit 1
