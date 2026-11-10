# codewhale-design

One source for Codewhale's design tokens, icons and whale assets. Use the
existing design and components; start with the authority for the part you are
changing.

## Start here

| Need | Read |
| --- | --- |
| Design rules and current look | [SYSTEM.md](SYSTEM.md); [DIRECTION.md](DIRECTION.md) describes how it should feel |
| Token generation and propagation | [SYSTEM.md §5](SYSTEM.md#5-mediums) owns the procedure |
| Current design library | [Figma · Start here](https://www.figma.com/design/4JlhR4mjtaginZwjaqZ0NU?node-id=245-2) |
| Work, scope and current evidence | [Linear CW-7930](https://linear.app/shannonlabs/issue/CW-7930) |
| Whale sources and motion | [whale/README.md](whale/README.md) and [whale/MOTION.md](whale/MOTION.md) |
| Maintaining the existing Figma library | [figma/README.md](figma/README.md) |
| Every element: status, intention, dates, Figma and Moda links | [registry/INDEX.md](registry/INDEX.md) (schema in [registry/README.md](registry/README.md); `python3 scripts/dissect.py <id>`) |
| Trying a design change | [experiments/README.md](experiments/README.md) |
| Where the look came from (deepseek-tui.com to today) | [vintage/README.md](vintage/README.md), history only |

`SYSTEM.md` wins when design documents disagree. Linear owns the queue and
current completion claims; screenshots and component counts are evidence at
a point in time, not a second tracker.

## What to edit

| Inputs | Derived output — regenerate instead of editing |
| --- | --- |
| [tokens.json](tokens.json), [generate.py](generate.py), [figma/sync.template.js](figma/sync.template.js) | `tokens.css`, `tokens.ts`, `tokens.rs`, `tokens.dtcg.json`, `figma/variables.json`, `figma/sync.js` |
| [whale/source/](whale/source/) and [whale/generate.mjs](whale/generate.mjs) | `whale/svg/` and `whale/out/`; see the [kit guide](whale/README.md) |
| [icons/](icons/) and [fonts/](fonts/) | Copies vendored by consumers |
| Figma control specifications and templates | [figma/README.md](figma/README.md) distinguishes editable inputs from generated transport batches |

The desktop, web, terminal and mobile consumers vendor their runtime inputs.
Their `vendor/codewhale-design` directories are copies, not authoring sources.
If this README is in a vendored copy, return to the `codewhale-design` checkout
for the complete authority and maintenance guides.

Follow [SYSTEM.md §5](SYSTEM.md#5-mediums) when changing or propagating a value.
Propagate only to consumers authorized by the current task. The
[scripts/check-consumers.sh](scripts/check-consumers.sh) inventory reports
drift; it does not authorize upgrading every consumer.

## Historical material

[deprecated/](deprecated/) and Figma's Archive pages preserve earlier work,
including obsolete client choices and rejected studies. They are not current
design guidance or a list of things to rebuild. The approved whale assets
remain in [whale/](whale/README.md); its motion guide records the current
direction and rejected legacy signs.
