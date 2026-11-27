# Whale avatar Native plugin

This example uses the current reviewed Native plugin system. Install the folder
through Codewhale's normal plugin install/review/enable flow. Review includes
`index.mjs`, `avatars/avatar.json` and its PNG page. It asks for no network,
secret or custom renderer API. The Engine binds the contribution to this live
Native owner and automatically withdraws it when disabled or disposed.

Select **Studio whale girl** in GPUI's whale habitat. The Engine TUI draws
only the dot whale and no longer shows avatar packs.

The `greet` clip demonstrates an author-defined action name. Add your own PNG
pages, clips, state mappings or views to this manifest. Frame indices span
pages in order, then row-major cells. All pages share the declared dimensions.
`rest` must be bound; other unknown states fall back to it. Each action has
frame durations, a reduced-motion poster and a repeat policy. A pack adds no
state classifier, animation loop or executable code to the rendering client.

`ctx.avatars.registerPack({path})` returns a Cordis effect disposer. Invoke it
when withdrawing a contribution early; ordinary plugin/scope teardown also
cleans it up. Use existing `commands`, `tools` and `events` services for other
behaviors. Avatar custom actions are visual clips, not arbitrary execution.

Art is derived from Codewhale's whale-girl pack. The exact generation prompt
is embedded in the PNG; the full provenance lives in the vendored Ratatui
`assets/whale-girl/PROMPT.md`. The web currently includes the built-in girl;
Native plugin catalog transport is available in GPUI.
