# Dot-whale habitat and Watch

The original 980-point whale remains the canonical body of one persistent
world. Its owner now prepares a whale-forms presentation from admitted Engine
activity: the dots leave the whale, become the thing the work is, in motion,
then return to the whale. This presentation uses the existing world clock; it
does not replace the simulation, recording format or score. Original
recordings retain their expression-version behavior.

The underwater transcript's completion visitor remains a bounded decoration.
The explicit `/pet` habitat observes foreground activity through the shared
owner, with opt-in stereo PCM output through FFmpeg's `ffplay`. A reusable
painter or a captured preview alone does not establish a live client connection.

## Ownership

- `crates/tui/src/tui/ambient_life/pet_sim.rs` owns the dependency-free Rust port
  of the TypeScript reference core, with typed `ChannelId` and `Archetype`.
- `whale-points.tsv` is the embedded authored body. `PetSim::whale()` constructs
  it using the existing `mulberry32(0xC0FFEE)` particle stream.
- `pet_widget.rs` owns particle-to-braille rendering. The portable Rust runner
  and ratatui demo use these same product files, rather than maintaining copies.
- `pet_cameo.rs` is the completion presentation inside the existing habitat.
  It consumes `WhaleCameo` and `AmbientActivity`; it never reads a wall clock,
  schedules a frame, or classifies model/tool text.

## Whale forms

`pet/src/core/pet-native.ts` prepares live and Still views of the owner's same
980-point body. `dot-field.ts` projects those points and their materials from
the typed activity projection and the supplied world time, and blends changes
on that clock. It does not step the world, consume randomness or modify the
canonical checkpoint. The fixed `[-1, 1]` envelope keeps scale stable across
forms; consumers fit that envelope rather than fitting each changing shape.

The smooth presentation samples 980 interior points from the existing filled
pet mark, preserving its eye, mouth and curled negative space at home. The
optional pixel presentation derives 980 colored drone positions from the
approved 192-cell pixel whale. The browser paints its lossless pixel mark at
rest, fading it into those drones as they leave; terminal views paint the same
prepared drone geometry. Both styles share the world, the clock and the forms.

During activity all 980 points leave the whale and become the thing the work
is, in motion (CURRENT_DECISIONS §205.5). Shape, movement and colour carry the
meaning; `dot-forms.ts` owns the thirteen forms:

| Activity | Form | Hue |
| --- | --- | --- |
| executing, tool | a wrench turning a bolt | 198 |
| editing | a pencil writing a line | 172 |
| reading | an open book, a page turning | 186 |
| searching | a lens sweeping lines of text | 205 |
| testing | a flask bubbling | 150 |
| browsing | a globe turning | 212 |
| computer | a pointer clicking a button | 222 |
| memory | layers settling into a stack | 268 |
| thinking | a bulb lighting up | 48 |
| responding | a speech bubble typing | 190 |
| delegating | a pod of small whales: two for up to two parallel agents, otherwise three | 200 |
| waiting (needs you) | a bell ringing, in a warm hue | 38 |
| done | a check mark, then home | 160 |

Each form is a list of parts sampled with a seeded generator. Dots are
allocated to parts by area and assigned by rank, so each dot travels a short
way from its place in the whale. Both body styles are compiled once at module
load. A form is a pure function of its kind, the body style, the dot index and
the time: no `Math.random`, wall clock or DOM, and identical input gives
identical output. Forms are drawn at 1.22 times their authored size and every
point stays inside ±0.94. Observed failure tints the form hue 16.

An 8.4-second cycle returns all 980 points to the filled whale between
expressions, using the presentation's admission timestamp rather than its
heartbeats. Dots leave with a short stagger. Completion holds the check for
800 ms and settles into the whale within 1.7 seconds. Still presents one fixed
pose of the form, evaluated at 1.3 seconds of its motion, during activity and
the filled whale at rest, without the cycle or swim; it never shows a
mid-morph pose. Unobserved, stale and idle presentations stay the whale.
`build-dot-bodies.py --whale-body` reproducibly derives the interior sampling
from the pinned alpha mask in `pet/public/whale.png`.

Ordinary captions keep the meaning without relying on shape or colour. The
owner's `displayPhrase` is the activity label from the producer's interface
locale (English, Simplified Chinese, Traditional Chinese or Japanese, else
English), taken from the same locale-pack strings the terminal uses;
`displayLanguage` names that caption's language. A pet test compares those
labels with the locale packs.

What this design does not do: forms carry no text, so they do not say which
file, command or page is involved; that stays in captions and the inspector.
Forms are not chosen from tool names or arguments, only from the typed
activity kind. The pod shows at most three whales however many agents run.
Forms do not follow the interface language, have no user-selectable variants,
and do not change the Engine language, session, score, checkpoint, recording or
audio. In the terminal the forms appear only in the `whale` view, which draws
the companion's prepared points; `cove` and `braille` draw the local whale.
All thirteen forms were checked there at 140x40 and 80x24; in smaller
terminals thin parts can thin out in Braille, and the caption is the legible
fallback.

Whale style is a saved appearance setting. `/pet style smooth|pixel` selects
the body, and the appearance studio exposes the same choice. Appearance files
saved with the removed large-text language field still load; the field is
ignored and dropped on the next save. An older running owner keeps its existing
producer contract; a style change reports that the companion needs to be
updated and restarted.

`pet/assets/pixel-whale.svg` is the approved exploration 34 (192-cell grid),
with the same aurora palette and silhouette as the smooth brand mark. Its
SHA-256 is `9d3742fbf325aedc18435d7b15e89178b8684ccbb40cd55a32e60bed156de700`.
`build-dot-bodies.py --pixel-body` verifies that source and derives the drone
positions. The lossless transparent 1024px WebP is generated from that SVG
with ImageMagick: `magick -background none -density 384 pet/assets/pixel-whale.svg
-filter point -resize 1024x1024 -define webp:lossless=true pet/public/pixel-whale.webp`.
`build-pet.mjs` checks that the embedded terminal image and the embedded native
core match their canonical sources. Custom particle colors also tint the
browser's pixel mark; selecting a color preset preserves the style.

The Engine admits `activity_kind` and optional `action_id` on operation start
and completion. The projection exposes `activityKind` and `actionId`, including
active spans and the latest failed-tool receipt. An action ID is a bounded
canonical or admitted integration identifier, at most 256 UTF-8 bytes without
control characters. It is not a command, path, argument, result or text-derived
guess. The owner's presentation layer uses the typed kind for the form and the
safe exact ID for variation; views can show that ID in a caption. Neither layer
classifies tool names. Unknown or stale observation keeps an honest unobserved
presentation.

Both prepared poses carry one `[r, g, b, alpha]` material per point. The owner in
`pet_watch/owner.rs` applies saved appearance, brightness and lighting once.
These are final RGBA values: consumers must not multiply them by style alpha or
apply the appearance settings again. A carrier may still dim a stale/offline
view and adapt ink to terminal capabilities. Reduced motion selects the owner's
`still` pose rather than running a second simulation or pausing mid-morph.

GPUI and the Ratatui `DotWhale` component consume prepared geometry. The 0.10.2
terminal's `PetMode` now passes its fresh, session-bound prepared pose into
`DotWhale`; reduced motion selects the prepared Still pose. A missing or stale
pose falls back to the native whale, with the current localized shell status.
The normal caption and streamed-reply inspector remain available. The product
web app has a guarded prepared-frame adapter and an opt-in particle painter,
but its Runtime/control-plane transport is not wired to those frames yet. The
owner's authenticated loopback browser companion is a separate connected
surface; it is not evidence of live product-web integration.

## Completion is a bounded observation

A completion does not identify which Whalesong category was last active. The
single visitor therefore uses `other · drift`, with observed coverage. Existing
`AmbientActivity::Subagents` presents three fixed, staggered peer slots, each
labelled `agent · pod`. At rest the three peers remain three animals; the core's
six-satellite working gait is not multiplied into eighteen animals.

The complete creature and its caption are preflighted against `is_open_water`.
A text collision with either withholds the entire creature. Off-screen peers
are withheld instead of clamping them into an overlapping stack. On a 40-column
surface the middle peer remains visible. Content always owns its cells.

The core is stepped at fixed 30 Hz into two bounded raster caches. A frame is
selected solely from the supplied completion age, so skipped draws, rewind,
resizes, and rendering another session do not alter its pose. There are no new
random choices. The existing theme supplies the habitat ink; the cameo adds no
status-bar colour meaning and never spends Failure red.

The shell already owns an 800 ms completion light pulse and a 600 ms life fade.
It now passes the motion-gated clock through that full 1.4-second interval. The
ocean still clips its light pulse at 800 ms. The low-level cameo retains the
2.4-second safety limit from its predecessor; the shell's life fade ends first.
Reduced motion exits before constructing marks or initializing the caches.

The generic widget retains the core's unknown rendering and the literal
`· unobserved` cue. If the whole cue cannot fit, it withholds the whole widget.
Sleep dimming does not set unknown coverage.

## Evidence boundaries

### `/pet` habitat

`/pet` (or `/pet on`) makes the pet the main view above the existing composer;
F5 or `/pet inspect` opens replies and agents over it. The pet has no workbar
panel: no placement or dismissal applies, and on the pet main view `Ctrl+]`
cycles the pet's views instead of the dock. The pet never overlays transcript
cells; narrow views keep the text cue. Opening it starts a read-only observer; it does not start an Engine
turn or silently substitute demonstration data. It begins with unknown coverage
and does not backfill events from before attachment.

`ui/event_loop.rs` passes accepted foreground Engine events through the existing
`core::protocol_parity::event_to_protocol` projection. The pet allowlist keeps
only lifecycle kind, stable ids, admitted operation kind/action identity,
channel and outcome. Prompt text, reasoning, arguments, results, paths and
routing credentials never enter the
worker queue or pet tape. No session context or KV-cache prefix is changed.

The generated `pet_watch/pet-native.js` comes from the canonical source in `pet/src/core`:

```sh
npm --prefix pet ci --ignore-scripts
npm --prefix pet run sync
```

The Engine adapter emits normalized event-v1 observations, then uses the existing
`compilePetTelemetry` function. A sealed 400 ms observation bucket appears at
the next world boundary; the world's accepted tape remains the replay authority.
Liveness pulses do not count as repeated tool calls. Silence does not stretch
occupancy across gaps. Error receipts create a current onset, rather than moving
a failure back to the operation's start. Typed `ShellPhase::Waiting/Approval`
can extend an already witnessed human request, preserving the outstanding request without approach-to-owner steering.

The presentation companion now owns the one persistent world. Terminal Watch,
full-screen habitat and the owner's browser companion attach to its
authenticated loopback snapshots. View
interpolation and stillness never advance or modify the world. The existing Ratatui draw loop owns pixel
placement and cleanup. Only the opt-in `pixels` view draws an image, after a
successful Kitty query, with synchronized output and a known cell size in
pixels; otherwise it shows the Braille particles. `CODEWHALE_PET_GRAPHICS=braille`
also disables that image. Full habitat focus uses the existing modal stack.

The complete ownership, source selection, storage/reconnect, graphics, audio
and legacy habitat contracts live in [pet/SHARED.md](../../pet/SHARED.md).
That document supersedes the former per-session Watch worker and app-local
live-world ownership. Completion cameos remain bounded transcript decorations.

The live Runtime SSE adapter and this foreground protocol adapter consume
different existing input contracts; they share event-v1 projection semantics,
the bucketer, world and score. The Runtime stream now has opt-in progress frames
at its own journal cursor. The standalone file recorder waits for durable replay and queued
live delivery to finish before extending any request as current. These frames
are transport metadata; they do not enter the event journal or pet tape.
The Runtime importer separately pairs request
lifecycles and preserves failure receipt times. Its local SSE/CLI
fixtures pass. The source and liveness contracts are documented in
`pet/README.md`.

Earlier phase evidence: the full TUI binary built offline, its foreground metadata projection test
passes, and the real PTY walkthrough covers Watch selection, keyboard, mouse
and resizing down to 40x12. Receipts and source hashes are in
`CW/artifacts/pet-review-20260912/`. Hosted macOS testing on the initial branch ran 15,418 tests with one Watch-tab
golden mismatch; its header is corrected in the transformation update. The
subsequent `e69e99b` passed the applicable Linux, macOS and Windows product tests,
lint and security checks. Later changes require their own hosted verdict.

Local render fixtures exercise the actual ocean, glyph and ambient-life modules
with literal shell enums extracted from `underwater.rs`; they do not run an
Engine session. The portable conformance runner compares TypeScript and
Rust across the original tape and an additional all-channel/error-burst/sleep
tape, each with motion enabled and disabled. A missing tool or failed widget
build now fails that runner instead of printing a false success.

Full product builds, real session acceptance, audio
and hosted CI remain separate proof requirements. The local
fixture and conformance checks do not establish those outcomes.
