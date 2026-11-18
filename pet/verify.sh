#!/bin/sh
# Exit on every build/runner failure; compare the same tape across all cores.
set -eu
cd "$(dirname "$0")"
mkdir -p conformance-results
node_cmd=${PET_NODE:-node}
rust_cmd=${PET_RUSTC:-rustc}
cargo_cmd=${PET_CARGO:-cargo}

printf '%s\n' '── canonical TypeScript core ──'
"$node_cmd" scripts/build-pet.mjs

printf '%s\n' '── rust (zero deps) ──'
"$rust_cmd" --edition 2024 -O rs/main.rs -o rs/petsim
if [ "$#" -ne 0 ]; then printf 'Usage: %s\n' "$0" >&2; exit 2; fi

checkpoints=0
for expression in v1 v2; do
for tape in tape.tsv tapes/edge-cases.tsv; do
    case "$tape" in tape.tsv) name=baseline ;; *) name=edge-cases ;; esac
    for mode in animated reduced-motion; do
        set --
        if [ "$mode" = reduced-motion ]; then set -- --reduced-motion; fi
        if [ "$expression" = v1 ]; then set -- "$@" --legacy; fi
        stem="conformance-results/$expression-$name-$mode"
        "$node_cmd" run-tape.ts run "$@" < "$tape" > "$stem-ts.txt"
        (cd rs && ./petsim "$@" < "../$tape") > "$stem-rs.txt"
        if [ "$expression" = v1 ]; then diff -u "tests/fixtures/v1-$name-$mode.txt" "$stem-ts.txt"; fi
        diff -u "$stem-ts.txt" "$stem-rs.txt"
        count=$(wc -l < "$stem-ts.txt" | tr -d ' ')
        [ "$count" -gt 1 ]
        checkpoints=$((checkpoints + count))
        printf 'PASS %s %s %s: %s matching checkpoints\n' "$expression" "$name" "$mode" "$count"
    done
done
done
printf 'Core conformance: %s checkpoints across 2 expression versions and 4 tapes/modes\n' "$checkpoints"
printf '%s\n' '── ratatui widget (offline, locked) ──'
(cd tui && "$cargo_cmd" build --offline --locked -q)
printf '%s\n' 'ratatui crate: built'
printf 'PASS — all core checkpoints and the ratatui widget build\n'
