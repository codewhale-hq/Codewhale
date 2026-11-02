# codewhale-design

Keep every Codewhale surface on one set of design tokens.

[SYSTEM.md](SYSTEM.md) is the design system: the product model, how tokens are used, components per
medium, patterns and the current audit. [DIRECTION.md](DIRECTION.md) is how it should feel.

This repository is the source of truth for Codewhale's colours, type, spacing,
radii and motion constants. The desktop app, the website, the terminal UI
(`codewhale-ratatui`) and the mobile app each vendor a copy and verify it at
build time, so no consumer needs the network or a sibling checkout.

Update tokens: edit `tokens.json`, bump its version, run `python3 generate.py`,
then run `scripts/sync-to.sh <consumer-repo>` for each consumer.

## Details

`tokens.json` is the versioned semantic design authority. Desktop consumes
`tokens.rs`; GPUI mobile vendors this folder and consumes the same Rust data;
web-next vendors this folder and imports `tokens.css`. No network access or
sibling checkout is required to build a consumer.

Change the JSON here, increment its version, run `python3 generate.py`,
and use `scripts/sync-to.sh` to copy the runtime token files into each consumer's
`vendor/codewhale-design`. Design studies and visual proofs stay in this repository.
Run the generator with `--check` in each repository. Generated files include the
source digest; consumer tests reject drift. Layout remains native to each
surface. Focus and reduced-motion settings must remain accessible on each host.

Typography and palette come from the shipping desktop theme. `primary` fills
actions and switches in neutral ink; `accent` is the one quiet blue, for focus
rings, links, text selection and the caret; the logo ombre stays on the whale
and the sign-in page. Icons retain the
24-unit, 1.7-pixel rounded stroke family; semantic names and accessible labels
stay with the host controls. Motion is shared too: durations (state, arrive,
exit, panel, linger), the arrive rise, the two loop periods (`beat` for
something that needs you, `breathe` for work in progress), the loop frame cap,
three easing curves (`out`, `in`, `panel`), state glows, the logo ombre and the
popover elevation reach Rust as constants and CSS as `--motion-*`, `--glow-*`,
`--ombre-*` and `--elevation-popover`. Desktop spring constants and the
reduced-motion poll cadence stay Rust-only. Under reduced motion every host
runs durations at zero, stops loops and keeps glows static.

`stream` paces how received reply text is uncovered, the same way on every
client: it follows the stream's arrival rate averaged over `rate_window_ms`,
uncovers the first receipt within `reveal_first_ms`, never lets hidden text
trail the received text by more than `reveal_lag_ms`, shows a receipt of at
most `instant_graphemes` at once when nothing is waiting, and shows a backlog
over `max_pending_bytes` (UTF-16 units on the web) at once. Rust reads them as
`STREAM_*`; the web reads `tokens.json`. Completion, corrections and reduced
motion always show the exact received text immediately.

`icons/` holds the sea glyphs DIRECTION.md names, on the same 24-unit grid at a
1.7-pixel round stroke, each with its word as `aria-label`: sonar (searching,
thinking), tide (real progress, waking), shell (receipt) and kelp (divider).
A consumer copies the file it uses beside its own icons; a glyph never stands
without its word on screen.

CSS consumers use `font-family: var(--font-family), var(--font-fallbacks), sans-serif`
to keep the shared family and CJK fallbacks together. The generator rejects
selection and primary-hover opacity values outside the inclusive 0–1 range,
and any palette that misses WCAG AA contrast in either mode: every text role
at 4.5:1 (`muted_foreground` at 5.5:1) and `border_strong` at 3:1 on
`background`, `surface`, `sidebar`, `hover`, `selected`, `ocean_depth` and the diff rows `diff_add` and `diff_remove`, `primary_foreground` on `primary` and on `accent` at 4.5:1, and dark
`foreground` on the ombre's deep stop at 4.5:1 (its rising stop is never a text ground). It also rejects
glow and elevation alphas outside 0–1 and any easing curve whose control points leave [0, 1], so nothing
overshoots. A new
colour, such as a named blue, enters the tokens only once it passes.

## Design studies and implementation handoffs

[Claude desktop → Codewhale, October 9, 2026](studies/claude-parity-20261009/README.md)
contains the editable Figma screen map, reusable component inventory, visual
proofs and frame-specific implementation packets. Its observed reference
behavior and proposed states are recorded separately. The study uses the
existing tokens; it does not introduce another palette or change shipped defaults.
