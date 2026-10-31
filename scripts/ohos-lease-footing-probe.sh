#!/usr/bin/env sh
# Report what a HarmonyOS directory does with ownership, mode and advisory
# locks, and say what the workspace drive lease will make of it.
#
# The lease admits its state directory as a private directory, which requires
# current-user ownership and a mode with no group or other bits. A mount that
# synthesises those attributes instead of storing them can never satisfy that
# proof, so `private_directory::private_footing` probes the mount once and the
# directory is admitted with that guard recorded as unverifiable. The lease is
# still taken: exclusivity is the kernel lock, a different capability.
#
# What this script reports is therefore the *proof*, not the lease: a
# synthesised footing means the ownership guard is relaxed and the log says so,
# not that exclusivity is lost. Exclusivity is lost only where `flock` itself is
# refused, which the engine's own warning names at startup. This script runs the
# same probe by hand, on the directory you name, and then exercises the real
# binary on it.
#
# Run it in the shell whose uid the engine will have: the important comparison
# is `id -u` against the owner the mount reports for a directory that uid just
# created. On a 2in1 the user directory (/storage/Users/currentUser, hmdfs)
# reports the logged-in user's uid for every object, so an app whose uid differs
# never sees a match there. The app sandbox (/data/storage/el2/base, reached
# inside the app as $HOME-like Context paths) does report the app's own uid.
#
#   # inside the CodeArts integrated terminal, so uid is the app's:
#   sh ohos-lease-footing-probe.sh "$HOME" /data/storage/el2/base/files
#
#   # from hdc shell, to compare what the debug shell sees:
#   hdc shell "sh /data/local/tmp/ohos-lease-footing-probe.sh $HOME"
#
# Exit status:
#   0  every root named can prove ownership (the full private-directory guard)
#   4  at least one root cannot (the guard is relaxed there, and the log says so)
#   2  bad usage
set -eu

usage() {
  cat >&2 <<'USAGE'
usage: ohos-lease-footing-probe.sh <directory>...

  <directory>  a directory the engine's state would live in. Nothing outside it
               is touched; the probe creates and removes one 0o700 directory.

environment:
  CODEWHALE_BIN  the engine binary. When set, the probe also runs it against
                 the first directory so the lease outcome is observed rather
                 than predicted.
USAGE
  exit 2
}

[ $# -ge 1 ] || usage

uid=$(id -u)

mode_of() {
  # Toybox stat prints the octal mode for -c '%a'; BSD stat uses -f.
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1" 2>/dev/null || echo '?'
}

uid_of() {
  stat -c '%u' "$1" 2>/dev/null || stat -f '%u' "$1" 2>/dev/null || echo '?'
}

echo "process uid: $uid"
echo "user:        $(id 2>/dev/null || true)"
echo

degraded=0
for dir in "$@"; do
  dir=${dir%/}
  if [ ! -d "$dir" ]; then
    echo "== $dir"
    echo "   missing; skipping (create it first, or point the probe at an ancestor)"
    echo
    continue
  fi

  probe=$dir/.codewhale-owner-probe-$$
  rm -rf "$probe"
  if ! mkdir -m 700 "$probe" 2>/dev/null; then
    # Toybox mkdir has no -m on some builds.
    mkdir "$probe" 2>/dev/null || {
      echo "== $dir"
      echo "   cannot create a directory here; the engine will report a refusal,"
      echo "   not a mount limitation, for this root"
      echo
      degraded=1
      continue
    }
    chmod 700 "$probe" 2>/dev/null || true
  fi
  owner=$(uid_of "$probe")
  mode=$(mode_of "$probe")
  rm -rf "$probe"

  echo "== $dir"
  echo "   created at 0700 -> owner $owner, mode $mode"

  owner_ok=no
  [ "$owner" = "$uid" ] && owner_ok=yes
  mode_ok=no
  case $mode in
    ?0 | ?00) mode_ok=yes ;; # e.g. 700, 0700, 0
  esac

  if [ "$owner_ok" = yes ] && [ "$mode_ok" = yes ]; then
    echo '   verdict: native footing — the ownership guard holds here'
  else
    echo "   verdict: synthesised footing — owner matches: $owner_ok, mode owner-only: $mode_ok"
    echo '            Directories here are admitted with the ownership guard'
    echo '            recorded as unverifiable: the engine logs it once per'
    echo '            directory and relies on the mount'"'"'s access control.'
    echo '            The workspace lease is still taken — exclusivity is the'
    echo '            kernel lock, which this probe does not measure. If the'
    echo '            engine also reports that advisory locks are refused, that'
    echo '            is when it runs without exclusivity.'
    degraded=1
  fi
  echo
done

if [ -n "${CODEWHALE_BIN:-}" ]; then
  first=${1%/}
  echo "== engine check on $first"
  if [ ! -x "$CODEWHALE_BIN" ]; then
    echo "   CODEWHALE_BIN is not executable: $CODEWHALE_BIN"
    degraded=1
  else
    log=$first/.codewhale-lease-probe-$$.log
    store=$first/.codewhale-lease-probe-$$.store
    echo "   version: $("$CODEWHALE_BIN" --version 2>/dev/null | head -1)"
    # `serve --http` is the invocation the editor extension and any other
    # client use, so it is the one whose lease outcome matters. It is a
    # long-running server: bound it with a timeout and read what it said.
    set +e
    CODEWHALE_HOME="$store" timeout 20 "$CODEWHALE_BIN" --workspace "$first" \
      serve --http --host 127.0.0.1 --port 47899 >"$log" 2>&1
    status=$?
    set -e
    if grep -q 'Runtime API listening on' "$log"; then
      echo '   ok: the engine started'
    else
      echo "   engine did not reach its listener (exit $status); last lines:"
      tail -12 "$log" | sed 's/^/     /'
    fi
    if grep -q 'cannot provide workspace exclusivity' "$log"; then
      echo '   lease: UNHELD — this filesystem refuses advisory locks'
      echo '   message:'
      grep 'cannot provide workspace exclusivity' "$log" | head -2 | sed 's/^/     /'
    elif grep -q 'already driving this workspace' "$log"; then
      echo '   lease: refused — another client holds this workspace'
    elif grep -q 'Runtime API listening on' "$log"; then
      echo '   lease: held (the filesystem provides locks)'
    fi
    if grep -q 'ownership cannot be verified' "$log"; then
      echo '   guard: relaxed here, as the footing probe predicted'
    fi
    rm -f "$log"
    rm -rf "$store"
  fi
  echo
fi

if [ "$degraded" -eq 0 ]; then
  echo 'PASS: every directory named can carry a private lease state root'
  exit 0
fi
echo 'RELAXED: at least one directory cannot prove current-user ownership.'
echo 'The engine still runs there and still holds the workspace lease; it logs'
echo 'the relaxed guard once per directory. Move the state to a root that can'
echo 'store ownership (see docs/HarmonyOS.md, "Workspace Exclusivity On The User'
echo 'Directory") if you want the full private-directory guard back.'
exit 4
