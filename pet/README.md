# The dot whale's core

This package holds the source the Codewhale terminal draws its whale from, and
the conformance check for the Rust particle simulation used by the ambient
cameo. There is no companion process: each terminal draws the whale locally
from its own session (founder decision 290).

## What is here

| Path | What it is |
| --- | --- |
| `src/core/dot-field.ts`, `dot-forms.ts`, `dot-body.ts`, `dot-pixel-body.ts` | The whale: activity, time and options in, 980 dots and their colours out. A pure function, with no clock or random numbers of its own. |
| `src/core/pet-engine.ts` | The reducer that turns a session's typed activity metadata into the activity the dot field draws. It accepts no prompt, reply or tool text. |
| `src/core/pet-telemetry.ts`, `signal.ts`, `model.ts`, `pet-sim.ts` | What the reducer imports: 400 ms activity buckets, the event model, and the TypeScript particle simulation. |
| `scripts/build-pet.mjs` | Type-checks the sources and writes the bundle the terminal embeds. |
| `scripts/build-dot-bodies.py` | Regenerates the two authored bodies from `public/whale.png` and `assets/pixel-whale.svg`. |
| `tape.tsv`, `tapes/`, `rs/`, `tui/`, `run-tape.ts`, `verify.sh` | Conformance between `pet-sim.ts` and the Rust simulation in `crates/tui/src/tui/ambient_life/pet_sim.rs`. |

The terminal loads the bundle in an embedded QuickJS runtime
(`crates/tui/src/tui/pet_watch/dots.rs`) and reads two exports from it:
`PetDotField` and `PetEngineTelemetry`.

## Build and check

Requires Node 22.13 or later. No network call, credential or provider is used.

```sh
npm --prefix pet ci --ignore-scripts
npm --prefix pet run check
```

`check` type-checks the sources, regenerates the bundle in memory, fails if
the committed copy at `crates/tui/src/tui/pet_watch/pet-native.js` differs, and
runs the tests. After changing anything under `src/core`, regenerate the
committed copy:

```sh
npm --prefix pet run sync
```

The bundle is committed so the Rust build needs no Node compiler.

```sh
cargo fetch --locked --manifest-path pet/tui/Cargo.toml
cd pet
./verify.sh
```

`./verify.sh` compares the TypeScript and Rust simulations checkpoint by
checkpoint and builds the small ratatui widget. It fails for a missing tool or
a failed build.

## What was removed

The shared companion (`codewhale pet serve`), its HTTP protocol, sound, replay
recording and export, the browser appearance studio, source selection, the
session importer and the live recorder are gone. Files an older version left
under `~/.codewhale/pet-shared` are not read and are not deleted.

## Licence

The signal model and telemetry bucketer come from Whalesong. Their Apache-2.0
[license](LICENSE) and [notice](NOTICE) are retained. Rust files imported from
the product keep that repository's licence.
