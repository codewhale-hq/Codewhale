#!/usr/bin/env sh
# Verify the file-backed secret store against a real directory, including mounts
# that cannot hold a 0600 mode or an advisory lock.
#
# The unit tests in crates/secrets cover both branches of the mode check, but no
# portable filesystem synthesises mode bits or refuses flock, so a test cannot
# build the branch that matters on HarmonyOS hmdfs. This script covers it by
# driving the real binary against a real directory instead of stubbing one, and
# it says which layer refused when something fails.
#
# Run it against each filesystem you care about and compare:
#
#   scripts/ohos-secrets-store-smoke.sh /tmp
#   scripts/ohos-secrets-store-smoke.sh "$HOME"
#
# On a HarmonyOS 2in1, the user directory (/storage/Users/currentUser, hmdfs)
# exercises the mode-ignoring and no-advisory-lock fallbacks, and /data (hmfs)
# proves the strict path still holds where mode bits are stored.
#
# Over a device connection, run it inside the device shell:
#
#   hdc file send target/aarch64-unknown-linux-ohos/release/codewhale /data/local/tmp/codewhale
#   hdc file send scripts/ohos-secrets-store-smoke.sh /data/local/tmp/smoke.sh
#   hdc shell "chmod +x /data/local/tmp/codewhale /data/local/tmp/smoke.sh; \
#     CODEWHALE_BIN=/data/local/tmp/codewhale sh /data/local/tmp/smoke.sh /data/local/tmp"
#
# Exit status:
#   0  every check passed
#   1  the secret store failed
#   2  bad usage
#   3  a different layer refused before the secret store was reached
set -eu

usage() {
  cat >&2 <<'USAGE'
usage: scripts/ohos-secrets-store-smoke.sh <directory> [binary]

  <directory>  an existing, writable directory. The script creates and removes
               its own subdirectory there; nothing else is touched.
  [binary]     the binary to test. Defaults to $CODEWHALE_BIN, then
               target/debug/codewhale, then target/release/codewhale.
USAGE
  exit 2
}

[ $# -ge 1 ] || usage
dir=${1%/}
bin=${2:-${CODEWHALE_BIN:-}}

if [ -z "$bin" ]; then
  for candidate in target/debug/codewhale target/release/codewhale; do
    if [ -x "$candidate" ]; then
      bin=$candidate
      break
    fi
  done
fi

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

mode_of() {
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1" 2>/dev/null || echo '?'
}

[ -n "$bin" ] || fail "no binary found; pass one or set CODEWHALE_BIN"
[ -x "$bin" ] || fail "binary is not executable: $bin"
[ -d "$dir" ] || fail "not a directory: $dir"

home=$dir/codewhale-secrets-smoke.$$
log=$dir/codewhale-secrets-smoke.$$.log
trap 'rm -rf "$home"; rm -f "$log"' EXIT HUP INT TERM

mkdir -p "$home" || fail "cannot create $home"
echo "binary : $bin"
echo "version: $("$bin" --version 2>/dev/null | head -1)"
echo "home   : $home"

# What this mount does with a mode it is asked to store. Create a file, try to
# narrow it to 0600, and read back what the mount kept. A filesystem that stores
# mode bits lands on 0600; a mount that synthesises them stays wider and either
# refuses the chmod or ignores it.
probe=$home/.mode-probe
: >"$probe" || fail "cannot create a file in $home"
probe_before=$(mode_of "$probe")
chmod 600 "$probe" 2>/dev/null || true
probe_after=$(mode_of "$probe")
rm -f "$probe"
echo "mount  : $dir"
echo "         file created at $probe_before; after chmod 600 it is $probe_after"
case $probe_after in
  600 | 400 | 0)
    mount_note='stores mode bits'
    echo '         -> stores mode bits; the strict path is expected'
    ;;
  *)
    mount_note='synthesises mode bits'
    echo '         -> does not store mode bits; the fallback path is expected'
    ;;
esac
echo

# A key that is obviously not a credential, and never leaves this host.
api_key='[redacted]'

echo '--- 1. auth set writes the key'
if ! printf '%s\n' "$api_key" | CODEWHALE_HOME="$home" "$bin" auth set --provider deepseek >"$log" 2>&1; then
  if grep -q 'config lock' "$log"; then
    cat "$log" >&2
    echo 'BLOCKED: the config layer refused before the secret store was reached.' >&2
    echo '         That is the crates/config write lock, not this store.' >&2
    exit 3
  fi
  cat "$log" >&2
  fail 'auth set failed; the store could not be written on this mount'
fi
grep -q 'saved API key' "$log" || {
  cat "$log" >&2
  fail 'auth set did not report saving the key'
}
echo 'ok: key accepted'

echo '--- 2. the key is readable back through the store'
# `auth status` names the layer that answered, so this proves the store supplied
# the key rather than the environment or config.toml.
if ! CODEWHALE_HOME="$home" "$bin" auth status --provider deepseek >"$log" 2>&1; then
  cat "$log" >&2
  fail 'auth status failed'
fi
grep -q 'active source: secret store' "$log" || {
  cat "$log" >&2
  fail 'the stored key is not the active source'
}
grep -q 'secret store: file-based.*(set,' "$log" || {
  cat "$log" >&2
  fail 'the secret store does not report the key as set'
}
if grep -q 'env var: DEEPSEEK_API_KEY (set' "$log"; then
  cat "$log" >&2
  fail 'DEEPSEEK_API_KEY is set in this shell and would mask the store; unset it and retry'
fi
echo 'ok: auth status resolves the key from the secret store'

echo '--- 3. the config file holds no plaintext key'
config=$home/config.toml
[ -f "$config" ] || fail "no config.toml written at $config"
if grep -qF -- "$api_key" "$config"; then
  fail 'the API key was written to config.toml in plaintext'
fi
echo 'ok: config.toml has no plaintext key'

echo '--- 4. the secrets file exists, and is owner-only where the mount can express that'
secrets=$home/secrets/secrets.json
[ -f "$secrets" ] || fail "no secrets.json written at $secrets"
secrets_mode=$(mode_of "$secrets")
echo "     secrets.json mode: $secrets_mode"
if [ "$mount_note" = 'stores mode bits' ]; then
  case $secrets_mode in
    600 | 400 | 0) echo 'ok: owner-only' ;;
    *) fail "a mount that stores mode bits left secrets.json at $secrets_mode, not 0600" ;;
  esac
else
  echo 'ok: the mount reports its own mode, and the store accepted it deliberately'
fi

echo
echo "PASS: the secret store works on $dir"
