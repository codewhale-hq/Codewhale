# Dot-whale habitat and Watch

The original 980-point whale remains the canonical body of one persistent
world. Its owner now prepares a living-glyph presentation from admitted Engine
activity: the dots form the selected language’s existing activity phrases, then
return to the whale. This presentation uses the existing world clock; it does
not replace the simulation, recording format or score. Original recordings
retain their expression-version behavior.

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

## Living-glyph presentation

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
rest, fading it into those drones as they travel into words; terminal views
paint the same prepared drone geometry. Both styles share the world and clock. During activity,
all 980 points become the prepared activity phrase in the whale's fixed
footprint. Chinese phrases of up to three characters use one line; longer
phrases use two lines. English wraps at spaces; Japanese wraps at phrase
boundaries. Every language keeps the same envelope. Ordinary captions retain
the full meaning in the interface language.
Every occupied glyph-mask pixel receives a particle, preserving stroke detail
in compact views. No separate tool icon overlays the character.

An 8.4-second cycle returns all 980 points to the filled whale between
expressions, using the presentation's admission timestamp rather than its
heartbeats. Completion holds its character for 800 ms and settles into the
whale within 1.7 seconds. The materials stay within an ocean palette, with warm glyph accents for waiting or
observed failure. Still presents the full-size phrase during activity and
the filled whale at rest, without the cycle or swim.
`build-dot-glyphs.py --whale-body` reproducibly derives the
interior sampling from the pinned alpha mask in `pet/public/whale.png`.

The generator reads the existing tool and phase labels from the English,
Simplified Chinese, Traditional Chinese and Japanese locale packs, using the
same typed activity mapping as the terminal caption. The Simplified Chinese
phrases are: `读取 编辑 搜索 验证 运行 使用浏览器 操作电脑 检索上下文
调用工具 思考 回复 智能体工作中 等你处理 完成`. Updating that locale and regenerating
the masks keeps the pet's wording consistent with the interface. Labels never
fall back to a single-character abbreviation. Flowing ink and light
express the action within the phrase: reading scans rows, searching moves a
spotlight, writing traces across strokes, memory draws light inward and
delegating runs concurrent bands. Glyph masks in `dot-glyphs.ts` are generated
by `pet/scripts/build-dot-glyphs.py`; font provenance and license are in
`pet/assets/dot-glyphs-LICENSE.txt`. Captions carry the meaning independently of
color, motion and the viewer's ability to read a small glyph.

Whale style and large-text language are independent saved appearance settings.
`/pet style smooth|pixel` selects the body; `/pet language auto|en|zh-Hans|zh-Hant|ja`
selects the phrases. The appearance studio exposes the same choices. `auto`
follows the active producer’s interface locale; other interface languages use
English large text until masks are available. An English reader can select
Chinese or Japanese large text while keeping an English caption for its meaning.
These preferences do not change the Engine language, session, score, checkpoint,
recording or audio. Old appearance files default to smooth and auto. An older
running owner keeps its existing producer contract; style and language changes
report that the companion needs to be updated and restarted.

`pet/assets/pixel-whale.svg` is the approved exploration 34 (192-cell grid),
with the same aurora palette and silhouette as the smooth brand mark. Its
SHA-256 is `9d3742fbf325aedc18435d7b15e89178b8684ccbb40cd55a32e60bed156de700`.
`build-dot-glyphs.py --pixel-body` verifies that source and derives the drone
positions. The lossless transparent 1024px WebP is generated from that SVG
with ImageMagick: `magick -background none -density 384 pet/assets/pixel-whale.svg
-filter point -resize 1024x1024 -define webp:lossless=true pet/public/pixel-whale.webp`.
`build-pet.mjs` checks that the embedded terminal image and native cores match
their canonical sources. Custom particle colors also tint the browser’s pixel
mark; selecting a color preset preserves style and language.

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

`/pet` (or `/pet on`) opens the full habitat over the existing shell. The pet
has no workbar panel: no dock cycling, tab, placement or dismissal applies. The
tank uses the whole content viewport and never overlays transcript cells; narrow
views keep the text cue. Opening it starts a read-only observer; it does not start an Engine
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
full-screen habitat, the owner's browser companion, macOS window, Apple Shared
and Android Shared attach to its authenticated loopback snapshots. View
interpolation and stillness never advance or modify the world. The existing Ratatui draw loop owns pixel
placement and cleanup; a successful Kitty query selects pixels, otherwise the
view uses braille. Full habitat focus uses the existing modal stack.

The complete ownership, source selection, storage/reconnect, graphics, audio,
legacy habitat and mobile contracts live in [pet/SHARED.md](../../pet/SHARED.md).
That document supersedes the former per-session Watch worker and app-local
live-world ownership. Completion cameos remain bounded transcript decorations.

The live Runtime SSE adapter and this foreground protocol adapter consume
different existing input contracts; they share event-v1 projection semantics,
the bucketer, world and score. The Runtime stream now has opt-in progress frames
at its own journal cursor. The standalone file recorder waits for durable replay and queued
live delivery to finish before extending any request as current. These frames
are transport metadata; they do not enter the event journal or pet tape.
The Runtime importer separately pairs request
lifecycles and preserves failure receipt times. Its local SSE/CLI and browser
fixtures pass. Typed synthetic Engine events have also passed through the actual
Runtime journal/SSE endpoint, recorder process and Apple file watcher/PetHost:
a fresh request appeared at 1,233 ms and cleared after its answer at 2,000 ms.
That provider-free fixture proves active native file delivery; physical-phone
transport and visual popover inspection are separate evidence. The source and
liveness contracts are documented in `pet/README.md`.

Earlier phase evidence: the full TUI binary built offline, its foreground metadata projection test
passes, and the real PTY walkthrough covers Watch selection, keyboard, mouse
and resizing down to 40x12. Receipts and source hashes are in
`CW/artifacts/pet-review-20260912/`. Hosted macOS testing on the initial branch ran 15,418 tests with one Watch-tab
golden mismatch; its header is corrected in the transformation update. The
subsequent `e69e99b` passed the applicable Linux, macOS and Windows product tests,
lint and security checks. Later changes require their own hosted verdict.

Local render fixtures exercise the actual ocean, glyph and ambient-life modules
with literal shell enums extracted from `underwater.rs`; they do not run an
Engine session. The portable conformance runner compares TypeScript, Rust and
Swift across the original tape and an additional all-channel/error-burst/sleep
tape, each with motion enabled and disabled. A missing tool or failed widget
build now fails that runner instead of printing a false success.

Full product builds, real session acceptance, packaged applications, audio,
mobile builds and hosted CI remain separate proof requirements. The local
fixture and conformance checks do not establish those outcomes.
