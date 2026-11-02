#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 || ! "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 X.Y.Z [release-assets-dir]" >&2
  exit 2
fi

version="$1"
checksums="${2:-release-assets}/codewhale-artifacts-sha256.txt"
asset=codewhale-tui-windows-x64.exe
sha=$(awk -v name="$asset" '$2 == name && NF == 2 { print tolower($1) }' "$checksums")
if [[ ! "$sha" =~ ^[0-9a-f]{64}$ ]]; then
  echo "expected one SHA-256 for $asset in $checksums" >&2
  exit 1
fi

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
filename=HunterBown.CodeWhale.yaml
primary="$repo_root/packaging/winget/$filename"
mirror="$repo_root/.winget/$filename"
primary_tmp=$(mktemp "$primary.XXXXXX")
trap 'rm -f -- "$primary_tmp"' EXIT
mirror_tmp=$(mktemp "$mirror.XXXXXX")
trap 'rm -f -- "$primary_tmp" "$mirror_tmp"' EXIT

sed -E \
  -e "s/^PackageVersion: .*/PackageVersion: $version/" \
  -e "s#(releases/(tag|download)/v)[0-9]+\.[0-9]+\.[0-9]+#\\1$version#g" \
  -e "s/^(    InstallerSha256: ).*/\\1$sha/" \
  "$primary" > "$primary_tmp"
cp "$primary_tmp" "$mirror_tmp"
chmod 644 "$primary_tmp" "$mirror_tmp"
mv -f "$primary_tmp" "$primary"
mv -f "$mirror_tmp" "$mirror"

echo "Prepared $filename for $version ($asset $sha)"
