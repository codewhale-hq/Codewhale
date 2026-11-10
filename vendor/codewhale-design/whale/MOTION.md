# Whale motion

The founder’s direction is a whale made of code: the same lights form a
collective, change their relationship and become the whale. The existing kit
supplies its shapes. The approved 13-state specification and the existing app
and September pet motion are the starting point; no replacement state model or
whale studio is authorized. Every whale shape comes from `generate.mjs`.

## Who appears where

| Form | What it is | Where |
| --- | --- | --- |
| **Mark** | The C-curl in one flat ink (`source/mark.svg`) | The app icon (black and white), the brand, the first frame of the launch |
| **Drones** | Identical lights on a braille lattice. One shape and one size: radius 0.32 × pitch. Each surface uses the densest generated lattice whose drones stay at least 3 device pixels apart | Launch card at 104 pt: 48 across, 1,059 drones on Retina. Web hero: up to 96 across, 4,233. Sidebar: 24 across, 265 on Retina. Terminal launch: braille, 32×15 cells |
| **Fluke indicator** | The whale silhouette in about 215 drones; its tail strokes | Inline before the agent's live status line on desktop and web. Terminal: 6 braille cells |
| **Colored whale** (whale B) | The rorqual with a pale pleated throat, flipper, dorsal fin and flukes | **Character moments only:** pet mode, Customize › Whales and the whale editor, the Deep Dive game, character guidelines. Never in the automatic launch, the sidebar or a hover |

A drone never changes size or shape. Depth and life come only from which drones
are lit, their colour and brightness, and their movement. The throat is lit pale,
and the eye is one missing drone.

## Launch card and web hero

1. **Mark holds** for 650 ms. Nothing else moves.
2. **Mark to drones**, 960 ms. A wedge sweeps head to flukes, and each drone lights as the wedge passes it.
3. **Rest**:
   - Brightness breathes ±13% on the whale's own rhythm: 3.6–4.8 s, set by its ID.
   - A light crest runs head to tail every 5.2–6.8 s.
   - Each drone hovers within ±0.12 pitch.

The intro plays once per window. Later launch cards assemble the drones from the centre in 720 ms. Pointing at or focusing the card runs one crest and brightens the drones for 300 ms. It does not bring out the colored whale.

## Sidebar: reuse the approved state specification

The founder approved the state-sheet structure in the prior Claude conversation,
then rejected all three rendered studies. The approved vocabulary is **Resting,
Listening, Thinking, Taking in, Writing, Running, Helpers, Needs you, Blocked,
Finished, Stopped reporting, Asleep, and the coloured whale**. Do not substitute
one state per Engine activity enum or promote the rejected drawings to approved
art. Engine activity kinds map into these states; they are not the design itself.

The existing owners remain `codewhale-app/packages/ui/src/whale/WhaleFormation.tsx`
and the September grammar in `codewhale/pet/src/core/pet-sim.ts`. Improve these
against the accepted specification. The old linked reference artifact was
wrong or lost; the founder supplied the whole prior conversation on 2026-11-09
and explicitly told the takeover agent to stop recreating the work.

The same drones may become any formation and the whale. They must not stay in a
fixed body, form pictograms or imitate literal marching formations. The cartoon
is retained as the acting model and a form the drones can become. Name and colour
already exist; identity-derived fluke spots, brighter points and breathing pace
are existing behavior. The proposed whale studio was explicitly declined.

The state sheet specified a short tell, common landing, chained work moves and
priority-based interruption. Needs you and Stopped reporting interrupt directly.
Stopped reporting uses hollow, stationary points; it is distinct from Asleep.
Those rules describe the accepted structure, not approval of the three studies’
poses or timing. Drawings and live motion still need the founder’s visual verdict.

The takeover’s replacement app study, added endpoint mapping and alternative state
vocabulary were archived after that correction. They are not active kit or app
source. Preserve existing geometry and behavior while fixing concrete integration
gaps; do not add another state framework or authoring tool.

Legacy sign output remains only for existing consumers and is rejected direction.
It is integration debt, not a design instruction. No part travels across app text.

## Working indicator (Fluke)

- **Working:** the tail strokes up and down, ±9° along the rear 45% of the spine, once per `loop_breathe_ms` (2400 ms). That is 12 lattice frames per stroke (`out/whale-drones.json` `indicator.frames`), flipped like braille.
- **Waiting on you:** the tail rests and the body dims to 50%. Seven amber drones (the lantern) sit above the head, beating once per `loop_beat_ms` (1200 ms).
- **Done:** it holds still at full brightness, then leaves with the status line.
- **Terminal:** six braille cells, using the frames in `out/whale-braille.json` at 2 frames a second. Waiting adds an amber lantern cell after the whale.

## Colored whale (character)

- **Arrival:** it arrives by the uncurl. The C bends along one spine into the whale over 840 ms: ease-out-back after an 18% anticipation, with a decaying 18° fluke stroke as follow-through.
- **Idle:**
  - a fluke stroke of ±7°
  - a bob of 0.9 units
  - a pitch of 1.2°
  - blinks every 2.6–6.4 s

## Budgets

- Rest life is status, not decoration: it says the whale is ready.
- It paints only while someone can see it: it holds 2.5 s after the window loses focus, then stops, and stops while the window is hidden.
- Loops paint at no more than `loop_fps_cap` (15). Transitions may paint at display rate for their duration.
- One canvas draws every drone on a surface, at device pixel ratio. There is never one SVG element per drone.
- Everything visible together stays within 5% CPU on the reference Mac.

## Reduced motion and Hold still

- **Launch card and web hero:** the drone formation, still.
- **Sidebar:** a still formation for the existing state; Hold still and the system motion preference remain supported. Rejected legacy signs are still migration debt.
- **Indicator:** the whale silhouette, still. Waiting shows the lantern steady.
- **Colored whale:** the still whale. The uncurl becomes a cut.
- **Brightness:** never more than three changes a second, each at most 15% luminance (WCAG 2.3.1).

## Not decided

- **Terminal indicator width.** It needs six braille cells for a legible tail stroke; the brief asked for one or two.
- **Launch card hover.** It brightens the drones rather than showing the colored whale. That follows "real whale shape sparingly"; the founder has not seen it.
