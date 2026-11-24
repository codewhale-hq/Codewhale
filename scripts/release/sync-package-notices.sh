#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."
source scripts/release/crates.sh
metadata="$(cargo metadata --offline --locked --format-version 1 --no-deps)"
packages=(npm/codewhale npm/runtime-sdk)
for name in "${release_crates[@]}"; do
  manifest="$(jq -er --arg name "$name" '.packages[] | select(.name == $name) | .manifest_path' <<< "$metadata")"
  packages+=("$(dirname "$manifest")")
done

for package in "${packages[@]}"; do
  mkdir -p "$package/codewhale-notices/docs"
  cp LICENSE THIRD_PARTY_NOTICES.md "$package/codewhale-notices/"
  cp docs/THIRD_PARTY_NOTICES.md "$package/codewhale-notices/docs/"
done
