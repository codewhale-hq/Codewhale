# Codewhale: a whale living in code

The current live experience is [one durable pet with attached views](SHARED.md):
Ratatui Watch/full habitat and the owner's browser view; the Codewhale apps are
the only other clients. That guide contains build/run commands, source selection
and the ownership contract.


The dots are Codewhale's material. The whale is its recognizable home form;
real activity can reorganize that material into a living expression of the work.
This is an evolving audiovisual instrument, not a mascot that asks for attention.

The current implementation has a persistent 980-particle world, thirteen event
categories, deterministic audio, replay and checkpoints. Version 2 reorganizes
the same particles into knots for reasoning, woven strands for code, branches for
filesystem activity, scanning layers for browsing, and circulating paths for
network traffic. Human input opens a junction in the field. It does not steer
the creature toward its owner. The whale re-forms as activity settles.
These are authored expressions of measured activity, not generated pictures of
arbitrary task content. Missing telemetry stays visibly unknown.

## Build from this repository

Requires Node 22.13 or later.
No sibling repository, investor folder, credentials or provider call is needed.

```sh
npm --prefix pet ci --ignore-scripts
npm --prefix pet run check
```

```sh
# Regenerate the embedded QuickJS bundle from the same source.
npm --prefix pet run sync
# Full product terminal; use /pet and /pet export.
cargo build --locked -p codewhale-tui --bin codewhale-tui
# Small standalone braille renderer, without the full product link.
cargo fetch --locked --manifest-path pet/tui/Cargo.toml
cd pet
./verify.sh
```

`./verify.sh` compares the TypeScript and Rust cores and builds the ratatui
widget. The script fails for a missing tool or failed build. See [QA.md](QA.md).

In the full terminal, `/pet sound on` enables the shared score and
`/pet sound off` mutes it. Sound starts off each time the application
opens. Install FFmpeg with `ffplay` on PATH to use this optional output; Watch
and recording work without it. The terminal streams the core's stereo 48 kHz
PCM to one player process. Hiding Watch, opening a modal, quiet mode, or stale
telemetry presentation suspends output. Reopening starts at the current clock,
without playing the intervening history. Player failure mutes sound and reports
a warning while the world continues. `/pet sound` shows its status.

## One source of meaning

`src/core/pet-telemetry.ts` derives 400 ms buckets from normalized Whalesong
event-v1. Measured span occupancy selects the channel; onset counts, repeat
density, error receipts, human requests and agent identities carry other facts.
Container spans are excluded. Open-ended spans without observation coverage,
disconnects and missing intervals are unknown, not successful idle time.

`pet-world.ts` advances a 30 Hz creature clock and named seeded random streams.
It journals accepted telemetry, interactions, behaviors and persistent pod slots.
`pet-audio.ts` schedules voices from that same clock. Noise uses absolute sample
positions, so buffer partitioning cannot change the score. Audio mixing is a
host concern. `pet-native.ts` exposes this exact core to QuickJS.
The terminal host does not carry its own event bucketer or score scheduler.
Device or rendering failures mute sound and report a message while the world,
persistence and export continue.

The Rust particle implementation lives in the product's
`crates/tui/src/tui/ambient_life/pet_sim.rs`; this package's runner imports it.
The TypeScript core must match its conformance digests. The generated
native bundle is committed so the product Rust build needs no Node compiler.
Run `npm run sync` after changing core source; `npm run check` (run by CI)
rejects a stale bundle or `shared.html` copy.

The original Whalesong importer, signal model and schema
are included because they are actual dependencies of the pet. Their original
Apache-2.0 [license](LICENSE) and [notice](NOTICE) are retained. Rust files imported
from the product retain that repository's license.

## Live recording

The adapter only reads the existing Runtime journal endpoint
`GET /v1/threads/{id}/events`. It never starts a turn.

```sh
cd pet
node scripts/pet.mjs --runtime=http://127.0.0.1:7878 --thread=THREAD_ID --output=pet.jsonl
node scripts/pet.mjs --input=trace.jsonl --output=other.pet.jsonl --watch
# Restart an existing live recording at the same path:
node scripts/pet.mjs --runtime=http://127.0.0.1:7878 --thread=THREAD_ID --output=pet.jsonl --resume
node scripts/pet.mjs --demo --output=demo.pet.jsonl
```

Choose an existing thread and an unused output path, or use `--resume` to restart
a stopped live recorder at its existing path. Optional authentication
comes from `CODEWHALE_RUNTIME_TOKEN`; tokens are rejected in URLs. Only plain
HTTP loopback IP origins are accepted. Redirects, invalid envelopes and cursor
holes are rejected; reconnects resume from the last accepted Runtime cursor.
The recorder requests the Runtime stream's opt-in replay-progress capability.
It remains unknown until durable replay and the queued live tail have drained;
receiving the first historical event does not make it current. Broadcast-lag
recovery returns the stream to replaying. Older Runtime/SDK combinations without
this capability stop input explicitly and leave the recording unobserved; use
the Runtime built from this source alongside the recorder.
The recorder seals the preceding observation interval against a fixed clock.
It retains request lifetimes and counts a delayed error once at receipt time.
Raw prompts, arguments, results and tokens do not enter the pet recording.
Live recording continues in segments. At 216,000 buckets (24 hours) or 64 MiB,
the recorder syncs the completed file, preserves it as
`OUTPUT.segment-000001.jsonl` (then `000002`, etc.), and atomically replaces the
same live pathname. Each segment starts at sequence zero and replays independently.
Use `--segment-buckets=N` to rotate sooner. A stall or suspension shorter than
one segment is recorded as unknown buckets; a longer one starts a new segment
whose first bucket is unknown, as `--resume` does, and only reports its length. Followers establish a new baseline
after replacement, then accept subsequent appends as current observations.

The live importer retains unfinished lifetimes and 16 seconds of completed
events for the bucketer's recurrence window. It removes raw payloads immediately;
250,000 events and 64 MiB bound retained metadata, not total session history.
Completed output segments remain on disk, so disk use grows with recorded history.
Rotation requires same-directory hard links and atomic replacement. Unsupported
storage, an archive-name collision or an external replacement stops recording
without overwriting the existing files. With `--resume`, the recorder validates
the previous complete tape, preserves its exact bytes in the next numbered
archive, and starts a new segment at the same live path. The first bucket is
unknown; fresh source observations follow. It never invents events or estimates
the duration of an outage from file timestamps. The companion's separately saved
habitat preserves its particles and clock across attachment.

A private, empty `OUTPUT.writer-lock` sidecar uses Node's built-in SQLite OS lock
to exclude simultaneous recorders. Keep this file in place; its lock is released
on close or process death without deleting a stale PID file. It contains no
events. Use local storage with working OS locks, hard links and atomic rename.
Malformed, incomplete, oversized or non-file previous tapes are preserved and
rejected; use a new output path while retaining the original for recovery.
`--resume` applies only to live recording, and can also create an unused path.

The thin wire is JSONL, one flat version-1 `PetBucket` per line: PetState plus
`sequence`, `simTimeMs`, `durationMs`, thirteen-element `onsets` and `activeMs`,
`errors`, `agentIds` and `waiting`. Sequence starts at zero in 400 ms steps.
Saved replay JSON contains this accepted tape and the interaction journal;
a versioned checkpoint also contains particle, random-stream and score cursors.
Recordings also store `expressionVersion`: new worlds use version 2; recordings
without this field retain the original version 1 particle and behavior rules.
Checkpoints must agree with the recording's expression version. Unsupported or
mismatched versions are rejected before changing the current world. This keeps
old saved habitats replayable while letting new worlds change their visual form.
The older TSV is a particle conformance tape and cannot preserve audio onsets.

## Persistence and current limits

The owner commits its checkpoint and recording together. Legacy TUI
`artifacts/pet/habitat.json` files remain recoverable; current Watch attaches to
the companion. Writes are private and atomic, use a writer lock and content
revision, and preserve corrupt files or external edits. Modern checkpoints
hydrate without replaying historical simulation. A crash may lose work since the
last successful checkpoint.

New worlds use version 2 recordings with an exact starting checkpoint and
absolute telemetry sequences. After 1,024 consumed buckets or 4,096 applied
inputs, completed history is saved as an immutable segment before replacing the
active habitat. Only then does the running world retire that history. Its
particles, random streams and score continue without a reset. Gaps remain unknown;
future imported events stay in the active segment. Version 1 imports still work.

TUI segments are JSON files beside `habitat.json` in the session's
`artifacts/pet` directory. Each segment replays independently from its starting
checkpoint; seeking cannot precede that point. Archived files are retained, so
disk use grows with recorded history even though active history is bounded.
Autosaves and imports are limited to 8 MiB; the QuickJS worker has a 64 MiB
memory limit, and exports stream in chunks up to 64 MiB.

A 30-hour synthetic Still run of the shared core rotated 263 segments and kept
the active file below 0.45 MB, with exact checkpoint continuation after every
rotation. Replay, live file delivery and measured audio output have separate
receipts in [QA.md](QA.md). The source is available as a development build.
