#!/bin/sh
# Run cargo with the portable Codewhale cache topology applied.
#
#   scripts/dev-cargo.sh check -p codewhale-config --lib --locked
#   scripts/dev-cargo.sh --status
#   scripts/dev-cargo.sh --self-check
#
# This is the everyday compile entry point. Check, test, and debug builds do not
# change product behavior; clean release and install builds stamp
# CODEWHALE_LOCAL_BUILD_SHA for --version and receipts without setting
# telemetry provenance. Use scripts/dev-test.sh <area> for tests under the shared temporary
# HOME boundary. Arbitrary Cargo commands here do not isolate product config.
# See scripts/dev-cache.sh and docs/BUILD_PERFORMANCE.md.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

# shellcheck source=scripts/dev-cache.sh
. "$repo_root/scripts/dev-cache.sh"

case ${1:-} in
  --status|--self-check|--print-exports|--help|-h)
    exec "$repo_root/scripts/dev-cache.sh" "$1"
    ;;
esac

codewhale_dev_cache_apply
# Match CI / the product's 16 MiB owner-thread stack so local cargo test
# and nextest do not abort on the default ~2 MiB (Windows ~1 MiB) stack.
if [ -z "${RUST_MIN_STACK:-}" ]; then
  RUST_MIN_STACK=16777216
  export RUST_MIN_STACK
fi
codewhale_dev_stamp_release_build() {
  [ -z "${CODEWHALE_BUILD_SHA:-}" ] || return 0
  [ -z "${DEEPSEEK_BUILD_SHA:-}" ] || return 0
  [ -z "${CODEWHALE_LOCAL_BUILD_SHA:-}" ] || return 0
  case ${1:-} in
    +*) shift ;;
  esac
  _cw_cmd=${1:-}
  _cw_release=0
  case $_cw_cmd in
    install) _cw_release=1 ;;
    build|b|run|r) ;;
    *) return 0 ;;
  esac
  shift || true
  _cw_previous=
  for _cw_arg in "$@"; do
    case $_cw_arg in
      --) break ;;
      --debug)
        [ "$_cw_cmd" = install ] && _cw_release=0
        ;;
      --release|-r|--profile=release|--profile=dist)
        _cw_release=1
        ;;
      --profile=*)
        [ "$_cw_cmd" = install ] && _cw_release=0
        ;;
      release|dist)
        [ "$_cw_previous" = --profile ] && _cw_release=1
        ;;
      *)
        [ "$_cw_previous" = --profile ] && [ "$_cw_cmd" = install ] && _cw_release=0
        ;;
    esac
    _cw_previous=$_cw_arg
  done
  [ "$_cw_release" = 1 ] || return 0
  _cw_head=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null) || return 0
  if git -C "$repo_root" diff --quiet HEAD -- 2>/dev/null; then
    CODEWHALE_LOCAL_BUILD_SHA=$_cw_head
    export CODEWHALE_LOCAL_BUILD_SHA
  else
    echo "dev-cargo: tracked changes present; this release build stays unstamped (dev). Commit first, or set CODEWHALE_BUILD_SHA." >&2
  fi
}

codewhale_dev_stamp_release_build "$@"
codewhale_dev_cache_exec_cargo "$@"
