# The dot whale

The whale is one drawing function, not a server: activity, time and options
in, 980 dots and their colours out. Each terminal runs it locally over its own
session (founder decision 290). The companion process that once owned a shared
world, with its recording, sound, browser view and extra terminal views, is
retired.

The underwater transcript's completion visitor is a separate, bounded
decoration and is described at the end.

## Ownership

- `pet/src/core/dot-field.ts`, `dot-forms.ts`, `dot-body.ts` and
  `dot-pixel-body.ts` own the whale and its activity forms.
- `pet/src/core/pet-engine.ts` owns the reducer from typed Engine metadata to
  the activity the dot field draws.
- `crates/tui/src/tui/pet_watch/pet-native.js` is the committed bundle of those
  sources; `pet/scripts/build-pet.mjs` writes it and fails `npm --prefix pet run
  check` when the committed copy is stale.
- `crates/tui/src/tui/pet_watch/dots.rs` runs the bundle in an embedded QuickJS
  runtime on its own thread, feeds it this terminal's metadata and hands
  validated frames to the view.
- `crates/tui/src/tui/pet_watch/full.rs` owns the `/whale` layout: the whale,
  the status mark and word, and the reply and agents inspector.
- `vendor/codewhale-ratatui` owns `DotWhale`, which paints a prepared frame in
  Braille cells, and the half-block whale `PetMode` draws when no frame exists.
- `crates/tui/src/tui/ambient_life/pet_sim.rs` owns the Rust particle
  simulation used by the completion cameo, with `whale-points.tsv` as its
  authored body, `pet_widget.rs` for particle-to-Braille rendering and
  `pet_cameo.rs` for the completion presentation.

## Whale forms

`projectDotField` projects the 980 points and their materials from the typed
activity projection and a supplied time. It keeps no clock, consumes no
randomness and holds no state. The fixed `[-1, 1]` envelope keeps scale stable
across forms; consumers fit that envelope rather than fitting each changing
shape. Each point carries one `[r, g, b, alpha]` material.

The body samples 980 interior points from the filled whale mark, preserving its
eye, mouth and curled negative space at home. A second, pixel body derived from
`pet/assets/pixel-whale.svg` remains in the shared core; the terminal draws the
smooth body only.

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

What this design does not do: forms carry no text, so they do not say which
file, command or page is involved; that stays in the caption and the inspector.
Forms are not chosen from tool names or arguments, only from the typed activity
kind. The pod shows at most three whales however many agents run. Forms do not
follow the interface language and have no user-selectable variants. In small
terminals thin parts can thin out in Braille, and the caption is the legible
fallback.

## The `/whale` view

`/whale` makes the whale the main view above the existing composer; `F5` opens
replies and agents over it through the existing modal stack. The view starts no
Engine turn and substitutes no demonstration data. It begins unobserved and
does not backfill events from before the first `/whale` in a process.

`ui/event_loop.rs` passes accepted foreground Engine events to
`pet_watch::observe`. Its allowlist keeps only lifecycle kind, stable ids,
admitted operation kind and action identity, channel and outcome. Prompt text,
reasoning, tool names, arguments, results, paths and routing credentials never
enter the dot whale's queue. No session context or KV-cache prefix is changed.

The Engine admits `activity_kind` and an optional `action_id` on operation
start and completion. An action id is a bounded canonical or admitted
integration identifier, at most 256 UTF-8 bytes without control characters. It
is not a command, path, argument, result or text-derived guess. The reducer
uses the typed kind for the form and the id for variation. Unknown or stale
observation keeps an honest unobserved whale.

The thread wakes at most every 66 ms while a form is moving, every 250 ms
otherwise and every 500 ms when held still, and sleeps when the view is hidden
or a still pose has settled. A frame that fails validation is dropped; three
failures in a row stop the thread and the view falls back to the half-block
whale with a notice. `/whale still` and reduced motion select the still pose.
`/whale color` recolours the frame in the terminal, keeping each dot's
brightness; it does not change the dot field.

What the view does not do: it keeps no recording, plays no sound and opens no
port. Its only shared state is the name and colour in `settings.toml`
(`whale_name`, `whale_color`), which the desktop app reads and writes too;
hold-still is not saved.

To regenerate the bundle after changing `pet/src/core`:

```sh
npm --prefix pet ci --ignore-scripts
npm --prefix pet run sync
```

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
