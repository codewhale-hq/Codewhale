#!/bin/sh
# Map a workspace area or source path to the fastest cargo/nextest
# invocation for that area, and apply the portable cache topology so a
# new worktree actually gets isolated build-dir (+ sccache only when
# incremental is already off). Tests run under the shared temporary HOME
# boundary; compiler caches and toolchain homes remain persistent.
# A libtest run with an explicit filter refuses green when the filter
# matches zero tests (nextest already fails loud on empty selections).
#
# Usage:
#   scripts/dev-test.sh <area|path> [filter...]
#   scripts/dev-test.sh --list
#   scripts/dev-test.sh --status
#
# Examples:
#   scripts/dev-test.sh config
#   scripts/dev-test.sh command-contract elapsed::
#   scripts/dev-test.sh crates/command-contract/src/elapsed.rs
#
# Environment:
#   CODEWHALE_DEV_NEXTEST  auto|1|0  (default auto: use cargo-nextest when
#                          it is on PATH; 0 forces cargo test)
#   Cache knobs are documented in scripts/dev-cache.sh.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

# shellcheck source=scripts/dev-cache.sh
. "$repo_root/scripts/dev-cache.sh"

usage() {
  printf '%s\n' "usage: scripts/dev-test.sh <area|path> [filter...]" >&2
  printf '%s\n' "       scripts/dev-test.sh --list|--status|--self-check" >&2
  exit 2
}

list_areas() {
  cat <<'EOF'
area              command
----              -------
agent             cargo test -p codewhale-agent --lib --locked
app-server        cargo test -p codewhale-app-server --lib --locked
build-support     cargo test -p codewhale-build-support --lib --locked
cli               cargo test -p codewhale-cli --lib --locked
command-contract  cargo test -p codewhale-command-contract --lib --locked
commands          cargo test -p codewhale-commands --lib --locked
config            cargo test -p codewhale-config --lib --locked
core              cargo test -p codewhale-core --lib --locked
execpolicy        cargo test -p codewhale-execpolicy --lib --locked
hooks             cargo test -p codewhale-hooks --lib --locked
lane              cargo test -p codewhale-lane --lib --locked
mcp               cargo test -p codewhale-mcp --lib --locked
paths             cargo test -p codewhale-paths --lib --locked
portable-config cargo test -p codewhale-portable-config --lib --locked
portable-core-navigation cargo test -p codewhale-portable-core-navigation --lib --locked
protocol          cargo test -p codewhale-protocol --lib --locked
release           cargo test -p codewhale-release --lib --locked
runtime           cargo test -p codewhale-runtime --lib --locked
sanitize          cargo test -p codewhale-sanitize --lib --locked
secrets           cargo test -p codewhale-secrets --lib --locked
state             cargo test -p codewhale-state --lib --locked
telemetry         cargo test -p codewhale-telemetry --lib --locked
tools             cargo test -p codewhale-tools --lib --locked
tui               cargo test -p codewhale-tui --lib --locked
tui-integration   cargo test -p codewhale-tui --test integration --locked
tui-cucumber      cargo test -p codewhale-tui --test cucumber --locked
workflow          cargo test -p codewhale-workflow --lib --locked
workflow-js       cargo test -p codewhale-workflow-js --lib --locked

path prefix                         area / extra filter
-----------                         -------------------
crates/<crate>/                     <crate>
crates/tui/src/tui/                 tui  tui::
crates/tui/src/tools/               tui  tools::
crates/tui/src/core/                tui  core::
crates/tui/src/commands/            tui  commands::
crates/tui/src/<file>.rs            tui  <file>::
crates/runtime/src/<file>.rs        runtime  <file>::
crates/tui/tests/integration/       tui-integration  <stem>
crates/tui/tests/cucumber/          tui-cucumber  <stem>
crates/tui/tests/                  tui-integration

When cargo-nextest is on PATH, the run stage is `cargo nextest run`
instead of `cargo test` (same binaries; process per test). Set
CODEWHALE_DEV_NEXTEST=0 to force libtest. New worktrees get an isolated
Cargo build-dir via scripts/dev-cache.sh; sccache wraps rustc only when
incremental is already off. Test HOME and config paths are isolated by
scripts/with-hermetic-test-home.sh. Do not use cargo test --workspace for a
single-area edit. --lib and --tests are disjoint; a green --lib run does
not cover crates/tui/tests/.
EOF
}

[ $# -ge 1 ] || usage

if [ "$1" = "--list" ] || [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  list_areas
  exit 0
fi

if [ "$1" = "--status" ] || [ "$1" = "--self-check" ]; then
  exec "$repo_root/scripts/dev-cache.sh" "$1"
fi

area=$1
shift

# Path form: map a source path onto an area, and invent a filter only when
# the caller did not already pass one.
if [ -e "$area" ] || printf '%s' "$area" | grep -q /; then
  rel=${area#./}
  extra=
  case $rel in
    tests/portable-config/*|tests/portable-config)
      area=portable-config
      ;;
    tests/portable-core-navigation/*|tests/portable-core-navigation)
      area=portable-core-navigation
      ;;
    crates/command-contract/src/navigation.rs)
      area=command-contract
      ;;
    crates/tui/tests/integration/*)
      area=tui-integration
      extra=$(basename "$rel" .rs)
      ;;
    crates/tui/tests/cucumber/*)
      area=tui-cucumber
      extra=$(basename "$rel" .rs)
      ;;
    crates/tui/tests/*)
      area=tui-integration
      extra=$(basename "$rel" .rs)
      ;;
    crates/tui/src/tui/*)
      area=tui
      extra=tui::
      ;;
    crates/tui/src/tools/*)
      area=tui
      extra=tools::
      ;;
    crates/tui/src/core/*)
      area=tui
      extra=core::
      ;;
    crates/tui/src/commands/*)
      area=tui
      extra=commands::
      ;;
    crates/tui/src/*)
      area=tui
      extra=$(basename "$rel" .rs)::
      ;;
    crates/tui/*|crates/tui)
      area=tui
      ;;
    crates/runtime/src/*|crates/commands/src/*|crates/command-contract/src/*)
      area=$(printf '%s' "$rel" | awk -F/ '{print $2}')
      extra=$(printf '%s' "${rel#crates/*/src/}" | awk -F/ '{print $1}')
      extra=${extra%.rs}::
      ;;
    crates/*)
      crate=$(printf '%s' "$rel" | awk -F/ '{print $2}')
      area=$crate
      ;;
    *)
      printf '%s\n' "dev-test: no area mapping for path: $area" >&2
      exit 2
      ;;
  esac
  case $extra in
    ''|main|main::|lib::|mod|mod::) extra= ;;
  esac
  if [ $# -eq 0 ] && [ -n "${extra:-}" ]; then
    set -- "$extra"
  fi
fi

pkg=
target=--lib
case $area in
  agent) pkg=codewhale-agent ;;
  app-server) pkg=codewhale-app-server ;;
  build-support) pkg=codewhale-build-support ;;
  cli) pkg=codewhale-cli ;;
  command-contract) pkg=codewhale-command-contract ;;
  commands) pkg=codewhale-commands ;;
  config) pkg=codewhale-config ;;
  core) pkg=codewhale-core ;;
  execpolicy) pkg=codewhale-execpolicy ;;
  hooks) pkg=codewhale-hooks ;;
  lane) pkg=codewhale-lane ;;
  mcp) pkg=codewhale-mcp ;;
  paths) pkg=codewhale-paths ;;
  portable-config) pkg=codewhale-portable-config ;;
  portable-core-navigation) pkg=codewhale-portable-core-navigation ;;
  protocol) pkg=codewhale-protocol ;;
  release) pkg=codewhale-release ;;
  runtime) pkg=codewhale-runtime ;;
  sanitize) pkg=codewhale-sanitize ;;
  secrets) pkg=codewhale-secrets ;;
  state) pkg=codewhale-state ;;
  telemetry) pkg=codewhale-telemetry ;;
  tools) pkg=codewhale-tools ;;
  tui) pkg=codewhale-tui ;;
  workflow) pkg=codewhale-workflow ;;
  workflow-js) pkg=codewhale-workflow-js ;;
  tui-integration)
    pkg=codewhale-tui
    target=--test
    harness=integration
    ;;
  tui-cucumber)
    pkg=codewhale-tui
    target=--test
    harness=cucumber
    ;;
  *)
    printf '%s\n' "dev-test: unknown area: $area (try --list)" >&2
    exit 2
    ;;
esac

use_nextest=0
_cw_nextest=${CODEWHALE_DEV_NEXTEST:-auto}
if codewhale_dev_cache_falsey "$_cw_nextest"; then
  use_nextest=0
elif command -v cargo-nextest >/dev/null 2>&1; then
  use_nextest=1
elif codewhale_dev_cache_truthy "$_cw_nextest"; then
  printf '%s\n' "dev-test: CODEWHALE_DEV_NEXTEST=${_cw_nextest} but cargo-nextest is not on PATH" >&2
  exit 2
fi

if [ "$target" = "--test" ]; then
  if [ "$use_nextest" -eq 1 ]; then
    set -- nextest run -p "$pkg" --test "$harness" --locked "$@"
  else
    set -- test -p "$pkg" --test "$harness" --locked "$@"
  fi
else
  if [ "$use_nextest" -eq 1 ]; then
    set -- nextest run -p "$pkg" --lib --locked "$@"
  else
    set -- test -p "$pkg" --lib --locked "$@"
  fi
fi

printf '+ cargo %s\n' "$*"
# Resolve the persistent cache before replacing HOME; dev-cargo applies the
# topology once, retaining Cargo's build-dir template and any caller overrides.
CODEWHALE_CACHE_ROOT=$(codewhale_dev_cache_root)
export CODEWHALE_CACHE_ROOT
# Engine acceptance launches the canonical CLI, which is not a binary target
# of the Engine library package. Build it with the same cache/lock topology.
if [ "$target" = "--test" ]; then
  "$repo_root/scripts/with-hermetic-test-home.sh" "$repo_root/scripts/dev-cargo.sh" \
    build -p codewhale-cli --bin codewhale --locked
fi
# libtest exits 0 when a filter matches nothing, which has been mistaken for
# a pass. With an explicit filter, refuse that green. (nextest already fails
# loud on an empty selection, so the guard only wraps libtest.)
#
# pipefail is not POSIX and probing it outside a subshell is fatal where it
# is unsupported: `set` is a special builtin, so an illegal option exits the
# shell outright with status 2 instead of returning a status a `&&` list can
# absorb. That killed this script on every filtered libtest run under dash
# (Ubuntu's /bin/sh, which is what CI and Debian users get) while passing on
# macOS. Probe in a subshell, and on shells without pipefail capture the run
# and replay it so the refusal below still applies; those shells lose live
# streaming for the duration of the filtered run, not the guard.
base_args=5
if [ "$target" = "--test" ]; then
  base_args=6
fi
if [ "$use_nextest" -eq 0 ] && [ "$#" -gt "$base_args" ]; then
  tmp_log=$(mktemp -t dev-test-log.XXXXXX)
  trap 'rm -f "$tmp_log"' EXIT INT TERM
  set +e
  if (set -o pipefail) 2>/dev/null; then
    set -o pipefail
    "$repo_root/scripts/with-hermetic-test-home.sh" "$repo_root/scripts/dev-cargo.sh" "$@" 2>&1 | tee "$tmp_log"
    test_status=$?
  else
    "$repo_root/scripts/with-hermetic-test-home.sh" "$repo_root/scripts/dev-cargo.sh" "$@" > "$tmp_log" 2>&1
    test_status=$?
    cat "$tmp_log"
  fi
  set -e
  if [ "$test_status" -eq 0 ]; then
    if grep -q 'test result:' "$tmp_log"; then
      if grep 'test result:' "$tmp_log" | grep -Eqv '(^|[^0-9])0 passed;'; then
        : # at least one binary ran tests
      else
        printf '%s\n' "dev-test: filter matched zero tests (every binary reports 0 passed); refusing green." >&2
        test_status=1
      fi
    else
      printf '%s\n' "dev-test: no 'test result:' lines in output; refusing green." >&2
      test_status=1
    fi
  fi
  rm -f "$tmp_log"
  trap - EXIT INT TERM
  exit "$test_status"
fi
exec "$repo_root/scripts/with-hermetic-test-home.sh" "$repo_root/scripts/dev-cargo.sh" "$@"
