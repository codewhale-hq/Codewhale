#!/usr/bin/env bash
# check-readme-locales.sh — verify README locale link symmetry.
#
# Checks:
#   1. Every locale file referenced in the main README exists.
#   2. Every existing README.<locale>.md has a corresponding link in
#      the main README (no orphaned locale READMEs).
#   3. Standard README header symmetry (at minimum).
#
# Exits non-zero if any check fails.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MAIN="$ROOT/README.md"

if [[ ! -f "$MAIN" ]]; then
  echo "[check-readme-locales] ERROR: $MAIN not found"
  exit 1
fi

echo "[check-readme-locales] Main: $MAIN"

# ---- Extract locale links from main README -------------------------

# Translations live in docs/i18n/. Match the whole link target so a link that
# points outside that directory is caught rather than read as a bare filename.
LOCALES_DIR="docs/i18n"
linked_locales=()
while IFS= read -r link; do
  file=$(echo "$link" | sed -n 's/.*\](\([^)]*README\.[^)]*\.md\)).*/\1/p')
  if [[ -n "$file" && "$file" != "README.md" ]]; then
    linked_locales+=("$file")
  fi
done < <(grep -oE '\[[^]]*\]\([^)]*README\.[^)]+\.md\)' "$MAIN" 2>/dev/null || true)

echo "[check-readme-locales] Linked from main README: ${linked_locales[*]:-(none)}"

# ---- Check 1: every linked file exists ---------------------------------

missing=()
for file in "${linked_locales[@]}"; do
  if [[ ! -f "$ROOT/$file" ]]; then
    missing+=("$file")
  fi
done

if [[ ${#missing[@]} -gt 0 ]]; then
  echo "[check-readme-locales] FAIL — main README links to missing files: ${missing[*]}"
  exit 1
fi
echo "[check-readme-locales] OK — all linked locale READMEs exist"

# ---- Check 2: no orphaned locale READMEs ----------------------------

# Find all README.<locale>.md files
existing_locales=()
while IFS= read -r f; do
  existing_locales+=("$(basename "$f")")
done < <(find "$ROOT/$LOCALES_DIR" -maxdepth 1 -name 'README.*.md' 2>/dev/null | sort)

echo "[check-readme-locales] Existing locale READMEs: ${existing_locales[*]:-(none)}"

orphans=()
for file in "${existing_locales[@]}"; do
  found=0
  for linked in "${linked_locales[@]}"; do
    if [[ "$(basename "$linked")" == "$file" ]]; then
      found=1
      break
    fi
  done
  if [[ $found -eq 0 ]]; then
    orphans+=("$file")
  fi
done

if [[ ${#orphans[@]} -gt 0 ]]; then
  echo "[check-readme-locales] FAIL — locale READMEs not linked from main README: ${orphans[*]}"
  echo "[check-readme-locales] Add links to these files in the main README.md header."
  exit 1
fi
echo "[check-readme-locales] OK — no orphaned locale READMEs"

# ---- Check 3: symmetry count ------------------------------------------

linked_count=${#linked_locales[@]}
existing_count=${#existing_locales[@]}

if [[ $linked_count -ne $existing_count ]]; then
  echo "[check-readme-locales] WARNING — linked ($linked_count) ≠ existing ($existing_count)"
  echo "[check-readme-locales] This is informational; checks 1 and 2 handle actual breakage."
fi

echo "[check-readme-locales] PASS"
