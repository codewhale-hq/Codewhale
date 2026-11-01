#!/usr/bin/env bash
# Sign codewhale-artifacts-sha256.txt for one version with the release
# Ed25519 key (PEM, from $CODEWHALE_RELEASE_SIGNING_KEY), writing the base64
# signature the Engine updater verifies (crates/release/src/signing.rs):
#
#   codewhale-release-manifest-v1\n<version without v>\n<exact manifest bytes>
#
#   scripts/release/sign-release-manifest.sh VERSION MANIFEST SIGNATURE_OUT
#   scripts/release/sign-release-manifest.sh --public-key   # print the hex key to compile in
#
# The key never touches the checkout: it is written to a private temp file
# that is removed on exit, and nothing derived from it but the signature and
# the public key is printed.
set -euo pipefail

[[ -n "${CODEWHALE_RELEASE_SIGNING_KEY:-}" ]] || { echo "CODEWHALE_RELEASE_SIGNING_KEY is not set" >&2; exit 2; }
openssl version | grep -Eq '^OpenSSL [3-9]\.' || { echo "OpenSSL 3 or newer is required for Ed25519 signing" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
umask 077
printf '%s\n' "$CODEWHALE_RELEASE_SIGNING_KEY" > "$work/key.pem"
openssl pkey -in "$work/key.pem" -pubout -out "$work/pub.pem" 2>/dev/null || { echo "CODEWHALE_RELEASE_SIGNING_KEY is not a PEM private key" >&2; exit 2; }
public_hex="$(openssl pkey -pubin -in "$work/pub.pem" -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')"
[[ "${#public_hex}" -eq 64 ]] || { echo "CODEWHALE_RELEASE_SIGNING_KEY is not an Ed25519 key" >&2; exit 2; }

if [[ "${1:-}" == "--public-key" ]]; then
  printf '%s\n' "$public_hex"
  exit 0
fi

[[ $# -eq 3 ]] || { echo "usage: $0 VERSION MANIFEST SIGNATURE_OUT | --public-key" >&2; exit 2; }
version="${1#v}"
manifest="$2"
out="$3"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || { echo "invalid version: $1" >&2; exit 2; }
[[ -f "$manifest" && ! -L "$manifest" ]] || { echo "manifest is not a regular file: $manifest" >&2; exit 2; }

{ printf 'codewhale-release-manifest-v1\n%s\n' "$version"; cat "$manifest"; } > "$work/statement"
openssl pkeyutl -sign -rawin -inkey "$work/key.pem" -in "$work/statement" -out "$work/sig.bin"
openssl pkeyutl -verify -rawin -pubin -inkey "$work/pub.pem" -in "$work/statement" -sigfile "$work/sig.bin" >/dev/null
openssl base64 -A -in "$work/sig.bin" > "$out"
printf '\n' >> "$out"
echo "Signed $(basename "$manifest") for $version with public key $public_hex"
