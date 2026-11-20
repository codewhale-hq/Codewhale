#!/bin/sh
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
status=0
for check in "$here"/check-*; do
  [ -f "$check" ] || continue
  echo "== ${check##*/}"
  case "$check" in
    *.py) python3 "$check" "$@" || status=1 ;;
    *.sh) sh "$check" "$@" || status=1 ;;
    *.mjs) node "$check" "$@" || status=1 ;;
    *) echo "skipped: no runner for ${check##*/}" ;;
  esac
done
exit "$status"
