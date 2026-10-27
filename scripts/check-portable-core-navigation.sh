#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
graph=$(mktemp)
trap 'rm -f "$graph"' EXIT HUP INT TERM
cargo tree --locked -p codewhale-portable-core-navigation --edges normal --all-features --target all --prefix none > "$graph"
cat "$graph"
python3 -B scripts/check-command-core-navigation-proof.py --graph "$graph"
env CARGO_INCREMENTAL=0 RUSTFLAGS=-Dwarnings cargo check --locked -p codewhale-portable-core-navigation --lib --no-default-features
env CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0 RUSTFLAGS=-Dwarnings RUST_MIN_STACK=16777216 CODEWHALE_EXT_HOST_TESTS=1 CODEWHALE_EXT_HOST_BUN_TESTS=1 sh scripts/with-hermetic-test-home.sh cargo nextest run -p codewhale-portable-core-navigation --lib --all-features --locked --profile ci --no-tests=fail
