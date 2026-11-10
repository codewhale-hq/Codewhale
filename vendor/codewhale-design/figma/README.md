# CW 5 library maintenance

Maintain the [existing Figma library](https://www.figma.com/design/4JlhR4mjtaginZwjaqZ0NU?node-id=245-2). The [repository guide](../README.md) points to design authority and the current Linear work item; [SYSTEM.md §5](../SYSTEM.md#5-mediums) owns token generation and propagation. Inspect the existing canvas before reconciling it with source.

Use the existing direct Figma connection or the native Figma app when needed;
do not open browser Figma sessions. Source generation and provenance checks
need no Figma session. See [operator decision §112](https://github.com/codewhale-hq/codewhale-ops/blob/main/CURRENT_DECISIONS.md#112-use-figma-directly-without-browser-sessions--2026-11-09).

## Inputs and generated files

| Edit | Generated — do not edit |
| --- | --- |
| [tokens.json](../tokens.json), [generate.py](../generate.py), [sync.template.js](sync.template.js) | `variables.json` and `sync.js` for variables and canonical text styles |
| [controls.spec.mjs](controls.spec.mjs), [controls.template.js](controls.template.js), [generate-controls.mjs](generate-controls.mjs), and the app component sources | `controls.manifest.json`, `controls.js` and `controls-N.js` |

`review-controls.js` and `repair-foundations.js` are maintenance operations for existing node IDs, not routine sync steps. Inspect their targets and the current task before running them.

## Existing controls

`controls.spec.mjs` describes representative content and states of the existing React controls. `generate-controls.mjs` resolves every referenced selector from the real app CSS, follows CSS Module composition, records source digests, and fails if a selector disappears. TextArea also reads its default row count from `TextArea.tsx`; the specimen uses three lines and a 74px control height. Other controls can require React behavior as well as CSS, so these are representative state specimens, not an exhaustive interaction specification.

Generate against the app checkout whose UI is being represented:

```sh
node figma/generate-controls.mjs --app /Volumes/VIXinSSD/CW/codewhale-app
node figma/generate-controls.mjs --app /Volumes/VIXinSSD/CW/codewhale-app --check
```

The generated `controls-N.js` transport batches stay below Figma's 50,000-character call limit. They and the combined payload are ignored build output. `controls.manifest.json` tracks the source inventory and digests.

Run each batch through the Figma Plugin API in the current library file. It defaults to a dry run. Prefix `const APPLY_CONTROLS = true;` to create missing families. Updates additionally need `const UPDATE_CONTROLS = true;`; replacement is limited to the exact sets this importer created. Existing unrelated families are skipped. Inspect the diff and current canvas before any update that could replace a designer's later edits.

The importer uses native editable nodes, CW 5 color and metric bindings, shared text styles, and the existing Button components for dialog actions. CSS weight or line-height combinations absent from the canonical nine roles use shared `CW 5/Source/…` styles with those exact source values. Component IDs survive updates. Fonts, opacity, inline control sizing and cell borders are handled explicitly.

`review-controls.js` creates light and dark instance sheets. `repair-foundations.js` updates the existing metrics board's version, makes the scrim visible over contrasting grounds and repairs the clipped focus specimen. Both target the existing library's known node IDs and avoid unrelated work.

## Status and historical evidence

[CW-7930](https://linear.app/shannonlabs/issue/CW-7930) owns current status and
links to the existing library work. Dated screenshots and readback receipts
are under `CW/artifacts/design-takeover-20261109/`; use the workspace artifact
index for the latest evidence rather than copying counts into more READMEs.

The control specimens establish representative Figma coverage and source
metrics. Runtime keyboard, accessibility, provider and installed-app
acceptance require their own evidence. Figma Archive pages preserve prior
work; they are not instructions to recreate it.
