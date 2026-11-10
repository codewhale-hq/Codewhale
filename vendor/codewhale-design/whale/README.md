# The whale set

This folder is the single source for every whale Codewhale draws: the mark, the identical-drone light show, the Fluke working indicator, the colored whale (a character asset) and the app icon. [MOTION.md](MOTION.md) owns when each appears and how it moves.

The existing kit is on [Figma · Whale 5](https://www.figma.com/design/4JlhR4mjtaginZwjaqZ0NU?node-id=267-8). Read the [repository guide](../README.md) for design authority and the current work item. Figma stills document the kit; they do not approve motion or make archived studies current.

## Source (edit these)

| File | What it is |
| --- | --- |
| `source/mark.svg`, `source/mark-reversed.svg` | Byte copies of `codewhale/brand/mark.svg` (`899fd7f0e8b2…`) and `mark-reversed.svg` (`ab0d1c15aac1…`). They are the brand's canonical marks. |
| `source/mark-parts.json` | The same mark split into movable parts (body, flukes, throat, pleats, eye, flipper), in the 124 × 130 mark box. These are the exact cubic contours of the brand mark, exported from the app's kit at codewhale-app `a9ef492f`. |
| `source/spec.json` | The colored whale's anatomy (spine, profiles, throat, pleats, eye, flipper, dorsal fin, flukes), the drone lattices for each surface, the palettes and the app icon. |

## Generate

Run from the repository root after editing the inputs above.

```sh
node whale/generate.mjs           # write svg/ and out/
node whale/generate.mjs --check   # fail if anything generated is stale
node whale/generate.mjs --icons   # also build out/icon/ (icns, ico, PNGs); needs rsvg-convert, iconutil, magick
```

Consumers read generated copies under `vendor/codewhale-design/whale/`. Make changes in this authoring folder, then follow [SYSTEM.md §5](../SYSTEM.md#5-mediums) for generation and propagation to the consumers in scope. Do not edit a vendored copy or use a drift report to expand that scope.

## Generated (do not edit)

- **`svg/`** holds the canonical SVGs, in light and dark:
  - the mark in parts and the colored whale
  - the drone formations: launch, sidebar, whale
  - the 12 Fluke indicator frames, plus waiting
  - the app icon in light, dark and tinted
  - the colored whale's poses on one shared box, so they register as frames: the uncurl at 0, 25, 50, 75 and 100%, the fluke stroke up and down, and a blink (`pose-*.svg`; angles and the blink in `spec.json` `poses`)
  - legacy sidebar signs (`sign-waiting`, `sign-blocked`, `sign-complete`) remain generated for consumers not yet migrated; they are rejected product direction, not the current motion vocabulary
- **`out/whale-geometry.json`** is the morph data for the colored whale (app), in four parts:
  - the two spines: the C traced through the mark (165 units) and the whale's own (124). Each has 240 curvature samples, an anchor, a heading and a length.
  - the tail-stroke weights
  - for every part, the mark's commands, plus eight numbers per point: its position along the spine and offset across it in the mark and in the whale, and a residual for each, which keeps swim 0 identical to the brand mark.
- **`out/whale-drones.json`** holds every formation at several densities, as rows of lattice cells in mark units. A renderer picks the densest one whose drones stay at least 3 device pixels apart.
  - launch C: 16–96 across, 119 to 4,233 drones
  - sidebar C: 10–40 across, 47 to 735; legacy sign data remains for unmigrated consumers and is rejected direction
  - whale B: 24–64 across
  - the Fluke indicator: 14–44 across, 12 frames per stroke, plus seven lantern drones
- **`out/whale-braille.json`** holds the terminal frames: the launch C (32×15 cells) and the Fluke indicator (6 cells × 4 frames).

The generator reproduces the app's current colored whale within 2 × 10⁻⁶ mark units, which is rounding in `mark-parts.json`.
