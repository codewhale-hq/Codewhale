# Site colors and the design tokens

Founder clarification, 2026-11-06: recover the earlier warm-paper website,
clear blue actions, and drawn water accents. The retained Moda homepage
capture, original brand board and Claude-authored Tidal Folio direction
anchor this restoration. The website uses its earlier paper neutrals and
Codewhale blue, with current shared type, spacing, radius, motion and
elevation tokens.

This supersedes the October 27 website color mapping. It does not change
the desktop app's palette or the generated design artifact.

## How the values reach a page

1. `scripts/export-design-tokens.py` reads the vendored `tokens.json` and
   writes `app/tokens.css`. It supplies shared typography, geometry, motion
   and elevation; `npm run check:tokens` detects stale generated output.
2. `app/styles/tokens-roles.css` defines the website's `--kit-*` colors and
   maps them to the roles pages paint with. Site colors live here; shared
   dimensions continue to use the generated `--gpui-*` tokens.
3. Pages and components use roles such as `--bg`, `--text`, `--accent` and
   `--brand-fill`. They do not import desktop colors directly.

## Role mapping

| Role | Light | Dark (pinned) and the stage |
| --- | --- | --- |
| `--bg` | Warm paper #faf8f5 | Deep navy #142638 |
| `--surface` (recessed) | Warm inset #f0ede8 | Abyss #0f1e2d |
| `--panel` (raised) | White #ffffff | Ocean depth #173b50 |
| `--text` | Ink #263844 | Foam #e6eef7 |
| `--muted` | Slate #495a63 | Mist #bbc9d7 |
| `--line` / `--line-strong` | Warm rule #d9d5cf / slate #495a63 | Night border #31495e / mist #bbc9d7 |
| `--accent`, `--ring` | Whale blue #0b48bb | Bioluminescence #8fbdff |
| `--brand-fill` / `--on-brand` | Contrast-adjusted current blue → whale blue / white | Contrast-adjusted current blue → whale blue / white |
| `--hover` / `--selected` | Warm inset #f0ede8 / shallows #dcefeb | Night surface #20374e / night border #31495e |
| `--live` / `--attention` / `--danger` | Kelp deep #396554 / amber #84530d / rust #9e3f36 | Kelp #9ecbbd / lantern #eab982 / coral #eea59d |

The logo ombre remains `#1e8fd8` → `#0b48bb`. Filled actions recover the earlier
#1877c9 → #0b48bb ramp so the blue is clear and white text remains readable. The sea
ramps from #0f3991 through #142352 to #142638; the footer and home install
band remain in that sea in both appearances.

The homepage reads on paper. Static SVG water rises behind the whale and
real terminal, using the original hero paths from `48140294df0` with low
opacity blue washes and fine current lines. The terminal frame keeps its
own dark roles. Computer Use and release updates sit on paper; deep water
is reserved for the closing action and footer. The footer recovers the
original tiny, fading SVG current texture. Decorative water ignores pointer
input, has no animation, and disappears in forced colors.

The original retained website neutral values are from `168c3c2658c`; the
water geometry and current texture are from `48140294df0`. These are visual
references. Current typography, product instructions and captured media
retain their current owners; historical copy and interface mockups are not
release evidence.

## Verification

`lib/gpui-role-tokens.test.ts` checks every text ink at WCAG AA against every
ground in each scheme, control edges at 3:1, filled-action contrast, and
text over the sea and deep tide stops. `lib/ocean-contract.test.ts` checks
the shared ocean presentation, accessible horizon and compact navigation.

The `--whale-*`, `--light-*` and `--shoreline-*` exports are terminal
palettes. Captured terminal colors in `primitives.css` remain as captured;
the site's fluid editorial display sizes remain its own layout roles.
