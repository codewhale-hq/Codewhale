#!/usr/bin/env sh
# Cross-build and self-sign the Codewhale Engine for a HarmonyOS / OpenHarmony
# device, using the signing tools inside the DevEco Studio SDK.
#
# A binary only runs on device when it carries a code signature: the loader
# refuses an unsigned ELF, and the failure it reports ("Permission denied") is
# indistinguishable from a plain missing exec bit. Native HarmonyOS therefore
# needs both this step and `strip` before the file is useful.
#
# The signature is SELF-SIGNED. `binary-sign-tool` writes a descriptor digest
# and sets FLAG_SELF_SIGN; it does not embed a certificate and needs no private
# key. That is the same shape as the binary already running on the device —
# `display-sign` reports "code signature is self-sign" for both. It is not an
# AppGallery-signed build and carries no bundle identity.
#
# Usage:
#   scripts/ohos-sign-binary.sh [--strip] [--out PATH]
#
# Requires:
#   OHOS_NATIVE_SDK   the OpenHarmony native SDK (llvm/, sysroot/, build-tools/)
#
# Steps this runs:
#   1. cargo build --release --target aarch64-unknown-linux-ohos -p codewhale-cli
#   2. llvm-strip the copy (optional, ~2.6 MiB smaller)
#   3. binary-sign-tool sign -selfSign 1
#   4. binary-sign-tool display-sign, so a failure is not silent
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

strip_binary=no
out=
while [ $# -gt 0 ]; do
  case $1 in
    --strip) strip_binary=yes; shift ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "${OHOS_NATIVE_SDK:-}" ]; then
  echo "error: set OHOS_NATIVE_SDK to the OpenHarmony native SDK directory." >&2
  echo "       e.g. /Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native" >&2
  exit 1
fi

sdk=$(cd "$OHOS_NATIVE_SDK" && pwd)
toolchains=$sdk/../toolchains
sign_jar=$toolchains/lib/binary-sign-tool.jar
# The keystore is only consulted for its alias name in self-sign mode; the
# password is the published one that ships with the SDK sample material.
keystore=$toolchains/lib/OpenHarmony.p12
keystore_pwd=123456
key_alias='openharmony application release'
strip_tool=$sdk/llvm/bin/llvm-strip

for file in "$sign_jar" "$keystore" "$strip_tool"; do
  [ -f "$file" ] || { echo "error: missing SDK file: $file" >&2; exit 1; }
done
command -v java >/dev/null 2>&1 || { echo "error: java not found; install a JDK or use DevEco's jbr." >&2; exit 1; }

# 1. Build. The environment is shared with the documented manual flow so there
#    is one place that decides the target flags.
# shellcheck disable=SC1091
. ./scripts/ohos-env.sh >/dev/null

echo "== building codewhale-cli for aarch64-unknown-linux-ohos"
cargo build --locked --release --target aarch64-unknown-linux-ohos -p codewhale-cli

built=target/aarch64-unknown-linux-ohos/release/codewhale
[ -f "$built" ] || { echo "error: build produced no binary at $built" >&2; exit 1; }

out=${out:-target/aarch64-unknown-linux-ohos/release/codewhale-signed}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM

# 2. Strip a copy. Never strip the build output in place: cargo would consider
#    the artifact current and leave a stripped binary where it expects an
#    unstripped one.
cp "$built" "$work/unsigned"
if [ "$strip_binary" = yes ]; then
  echo "== stripping"
  "$strip_tool" --strip-unneeded "$work/unsigned"
fi

# 3. Sign.
echo "== signing (self-sign)"
java -jar "$sign_jar" sign \
  -mode localSign \
  -keyAlias "$key_alias" \
  -keystoreFile "$keystore" \
  -keystorePwd "$keystore_pwd" \
  -inFile "$work/unsigned" \
  -signAlg SHA256withECDSA \
  -outFile "$work/signed" \
  -selfSign 1 2>&1 | sed -n 's/^.*INFO - /  /p'

# 4. Verify, and refuse to hand back a file that did not sign.
echo "== verifying"
verify=$(java -jar "$sign_jar" display-sign -inFile "$work/signed" 2>&1) || {
  echo "$verify" >&2
  echo "error: display-sign failed" >&2
  exit 1
}
case $verify in
  *"code signature is self-sign"*) echo "  code signature is self-sign" ;;
  *) echo "$verify" >&2; echo "error: the output is not self-signed" >&2; exit 1 ;;
esac

cp "$work/signed" "$out"
chmod +x "$out"
echo
echo "signed: $out"
echo "size  : $(wc -c < "$out") bytes"
echo "sha256: $(shasum -a 256 "$out" | cut -d' ' -f1)"
cat <<'NOTE'

To run it on a device, note that two separate gates apply:

  * the file needs its exec bits. HarmonyOS hmdfs (the user directory) refuses
    `chmod`, and `hdc file send` creates files at 0660, so copy on device from
    something already executable:
        cp ~/.cargo/bin/codewhale ~/codewhale-new   # keeps 0750
        # then overwrite its content with the new build
  * it must be executed from an app process (HiShell, or the CodeArts
    integrated terminal). The hdc debug shell cannot exec a file on hmdfs at
    all — it reports "Permission denied" even for a binary that runs fine
    elsewhere.
NOTE
