# Pet review packet

Review the exact commit checked out (`git rev-parse HEAD`). Report the source SHA,
OS/toolchain, commands, actual nonzero pass/fail counts, and reproducible findings.
Synthetic fixtures prove local contracts, not provider or customer acceptance.

## Shared pet acceptance

See [Shared habitat](SHARED.md) for the ownership diagram, commands and bounded
local transport. Live terminal and browser views attach to one companion;
isolated worlds and replay remain explicit.
The companion is a presentation process and does not create an Engine turn.

`check-shared.py` starts only disposable loopback owners and checks ten contracts:
common identity and immutable frame, competing-owner exclusion, authorization and
origin validation, durable interaction deduplication, producer gaps and atomic
validation, source changes and view closure, one sound lease, crash restoration,
storage conflicts, and appearance persistence without simulation inputs. No provider is used by these checks.

Visual acceptance requires the actual Ratatui application in a terminal that
answers the existing Kitty probe successfully, plus a fallback terminal. Inspect
`/pet`, `/pet on`, F6/F8, Escape, resize, hidden
composer/history/selection, approvals, reduced motion, and image cleanup. Compare the
browser view to the same identity and checkpoint. A CPU
raster benchmark or PTY byte stream does not prove terminal appearance or input
latency. Record measured FPS, CPU/RSS and output bandwidth separately from the
60 fps presentation target. The canonical world remains fixed at 30 Hz.

## Fast Linux or macOS review

```sh
npm --prefix pet ci --ignore-scripts
npm --prefix pet run check
npm --prefix pet run sync
git diff --exit-code -- crates/tui/src/tui/pet_watch/pet-native.js
cargo fetch --locked --manifest-path pet/tui/Cargo.toml
./pet/verify.sh
npm test
npm run check:web
```

The pet suite currently contains 72 tests: event occupancy/unknown coverage,
late failures, human request pairing, read-only local SSE reconnect/cursor
recovery, replay readiness and cancellation after garbage collection, bounded
long sessions and recorder process restart, deterministic world/score/PCM,
checkpoint integrity and continuation, immutable segment boundaries, pending input
retention, live-file freshness/restarts, expression-version validation and legacy replay.
The standalone verifier compares 380 checkpoints across baseline and edge tapes,
each animated and still, under expression versions 1 and 2. Version 1 must also
match pinned pre-transformation golden digests.
`npm test` requires the root and web dependencies described by the product.
`check:web` fetches public GitHub release metadata and requires network access.

For the full terminal, start with one build job on memory-limited machines:

```sh
cargo build --locked -j 1 -p codewhale-tui --bin codewhale-tui
cargo test --locked -j 1 -p codewhale-tui --lib tui::pet_watch::
python3 pet/scripts/check-shared.py target/debug/codewhale-tui
```

Do not rerun a full library test link that is already exhausting the machine.
Report build/resource blockers and continue independent source/browser QA.
Manually inspect `/pet`, narrow layouts, paste/Enter focus, session
save/reopen and immutable `/pet export`. Unknown must remain legible; stale approvals must not resurrect.
Use a disposable session and synthetic/loopback telemetry for this assignment.

With FFmpeg's `ffplay` on PATH, explicitly enable `/pet sound on`.
Check sound off/on, switching away from Watch and back, quiet mode, and a missing
or failed player. These must not stop the world or its checkpoint. No historical
sound should play after resuming. Unit tests use a capture sink or a disposable
child process; they never open an audio device. Actual output and listening
quality need separate manual evidence.

## Review direction

Codewhale is a whale living in code. The dots can reorganize to show the work;
the whale is its home form, not a permanent silhouette restriction. Avoid the
old framing of a needy creature turning toward its owner. Evaluate readable,
causal transformations, continuity of particle identity, accessibility and
honest uncertainty. Version 2 supplies work-driven knots, strands, branches, layers, circulation and
open junctions; the same particle identities return to the whale at rest.

## Recorder process restart — September 12, 2026

`--resume` restarts a live recorder at its existing watched pathname. It validates
and archives the previous complete tape without changing its bytes, continues
numbered archives, and starts a fresh unknown segment. The existing host habitat
owns world continuation. No outage duration or activity is inferred from mtime.
A private empty sidecar uses Node SQLite's OS lock to exclude simultaneous
writers and release ownership on close or process death. It stores no events.

The local pet check passed 61 tests with zero failures. The focused recorder
checks cover repeated restarts, archive gaps, baseline/fresh live delivery,
invalid and incomplete source preservation, a competing writer, changed locks,
same-size external changes, SIGKILL followed by restart, and clean CLI shutdown.
Against the previous `6d960525` recorder, resuming rejects with EEXIST and a
same-size external rewrite is incorrectly accepted; the new guards reject it.
The required `npm test && npm run check:web` gate passed 66 package, 12 SDK and
446 web tests, with zero failures and two existing web-check warnings.

This changes the standalone recorder, its tests and documentation. World, score,
TUI and native bundle bytes are unchanged from the previously verified build.
Grokbot review and current-head Windows/macOS recorder CI are separate receipts.
No release or deployment is implied.


The first restart commit `11c6fbfc` passed all 61 tests independently in
Codewhalebot on Node 22.19.0 with no remaining concrete source finding. macOS
recorder CI also passed. Windows passed 60 tests and canceled one on timeout:
the test-only IPC message listener kept a correctly rejected startup alive.
The same preload reproduced the hang locally; unreferencing its IPC channel
lets the rejected CLI exit with code 1. The process-death/restart test now uses
that IPC path on every host, while other POSIX cases still use actual SIGINT.
The fixture also waits for CLI readiness before shutdown: the archive link may
appear before replacement and before the signal handler is installed. A broad
local run exposed this early-stop race after the IPC fix. Production recorder
and native bundle bytes are unchanged in this follow-up.


## Runtime replay-to-live handoff — September 12, 2026

An old human request must stay unknown while its journal answer is still in the
replay backlog. The Runtime event endpoint now offers opt-in `stream.progress`
frames and advertises that capability. It declares live observation only after
both durable replay and the queued live tail have drained; broadcast lag returns
the stream to replaying while durable history catches up. These frames are
transport metadata, not journal events, and allocate no new sequence numbers.
Default SDK callers keep the existing event-only stream.

The pet requires that explicit handoff. Journal packets alone cannot establish
current observation. An older Runtime or SDK without progress support stops
input with a clear diagnostic and leaves the recorder unknown. In particular,
the earlier read-only Runtime 0.9.13 receipt above is historical evidence, not
compatibility evidence for this new live transport.

The local pet check passed 63 tests, with zero failures or cancellations. The
required `npm test && npm run check:web` gate passed 66 package, 14 SDK and 446
web tests; web checks reported zero errors and two existing warnings. Three
actual Runtime Rust tests passed for opt-in endpoint negotiation, queued answers
at handoff, and broadcast-lag recovery; the existing event-only handoff test also
passed. The TypeScript SDK declarations passed their compiler check.

A real loopback HTTP fixture delays a 60-second-old request's answer. The
published `c342329f` transport marks that pending historical request current;
the corrected transport keeps it unknown. Further fixtures verify that the old
answer arrives before readiness, a fresh request becomes visible, replay reentry
suppresses observation, and unsupported streams cancel before accepting input.
The 260,001-record bounded-input fixture still passes with readiness markers.
These are synthetic event fixtures with real transports, not provider calls.

World, score and native bundle bytes are unchanged. Hosted CI, independent
Codewhalebot QA remain separate evidence.


## 0.10.2 real-terminal and shared-owner run sheet — 2026-10-12

Founder decision F6: these human checks carry to the next milestone and do not
block the 0.10.2 tag. Items 3–5 are source requirements; #6155 remains open.
No terminal or shared-owner result is asserted by this blank run sheet.

| Receipt | Result |
| --- | --- |
| Candidate source SHA and binary SHA-256 | Not recorded |
| OS and terminal versions | Not recorded |
| Ghostty/Kitty live `whale` view, resize and F5 → Esc image cleanup | Not run |
| Still/reduced motion; Terminal.app fallback and Ctrl+] | Not run |
| tmux and SSH transport | Not run |
| 60-second `/pet status` frame rate, bandwidth, render and write timing | Not measured |
| 60 samples of TUI/companion CPU and RSS | Not measured |
| Same retained identity/tick/digest in TUI and desktop/browser | Not run |
| Interactions from both views, TUI close/reopen and second TUI as a view | Not run |
| One audio lease; source change; competing owner refusal | Not run |
| Owner restart preserves identity; storage failure refuses action | Not run |
| Desktop/app-server turn follows idle companion; active TUI retains source | Human check not run |

Item 5 is implemented: `pet/src/core/pet-engine.ts` and the bundled
`crates/tui/src/tui/pet_watch/pet-native.js` grant Done and its effect ID only
when `turnOutcome === 'completed'`. Failed and interrupted turns cannot claim
Done. Existing Rust owner projection coverage includes completed/interrupted;
this code inspection does not claim a new failed-turn terminal qualification.
