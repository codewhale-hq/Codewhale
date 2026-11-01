# Site colors and the design tokens

Decision (founder, 2026-10-27): the website uses the same design system as the
desktop app. `codewhale-design` `tokens.json` (2.3.0, vendored at
`vendor/codewhale-design`) is the source of truth for color, type roles,
spacing, radius, motion and elevation. This supersedes the 2026-10-11 decision
that made the Moda brand kit "Codewhale" the site's color authority.

## How the values reach a page

1. `scripts/export-design-tokens.py` reads the vendored `tokens.json` and
   writes `app/tokens.css`: `--gpui-light-*` / `--gpui-dark-*` colors,
   `--gpui-type-*`, `--gpui-motion-*`, `--gpui-easing-*`,
   `--gpui-elevation-*`, `--gpui-radius-*`, `--gpui-spacing-*`,
   `--gpui-breakpoint-*` and `--gpui-ombre`. `npm run check:tokens` fails when
   it is stale.
2. `app/styles/tokens-roles.css` names the roles (`--bg`, `--text`,
   `--accent`, `--brand-fill`, `--text-ui`, `--dur-state` and so on). Every
   role is a `var(--gpui-*)`, never a literal.
3. Pages and components paint only with roles.

To change a color, change `tokens.json` in `codewhale-design`, bump its
version, run `generate.py`, sync it here with `scripts/sync-to.sh`, then run
`scripts/export-design-tokens.py`.

## Role mapping

| Role | Light | Dark (pinned) and the stage |
| --- | --- | --- |
| `--bg` | `background` #faf8f5 | `background` #202123 (stage: `ocean_depth` #202123) |
| `--surface` (recessed) | `sidebar` #f0ede8 | `sidebar` #191a1c |
| `--panel` (raised) | `surface` #ffffff | `surface` #2a2b2e |
| `--text` | `foreground` #28292b | `foreground` #efeeeb |
| `--muted` | `muted_foreground` #4f504d | `muted_foreground` #b8b8b4 |
| `--line` / `--line-strong` | `border` / `border_strong` | `border` / `border_strong` |
| `--accent`, `--ring` (links, focus, selection) | `accent` #425c82 | `accent` #a9c0de |
| `--brand-fill` / `--on-brand` (filled actions) | `primary` #2e2f31 / `primary_foreground` | `primary` #e6e4df / `primary_foreground` |
| `--hover` / `--selected` | `hover` / `selected` | `hover` / `selected` |
| `--live` / `--attention` / `--danger` | token roles | token roles |
| `--diff-add` / `--diff-del` | `diff_add` / `diff_remove` | `diff_add` / `diff_remove` |

The logo ombre (`#1e8fd8` → `#0b48bb`, `--ombre`, `--brand-light`,
`--brand-deep`) is used only on the whale and the mark.

The footer and the home install band (`.stage`, `.site-footer`) are painted
with the dark set in both appearances.

## Not from tokens

- `--whale-*`, `--light-*` and `--shoreline-*` in `app/tokens.css` are the
  terminal's palettes from `crates/palette/src/rgb.rs`. No page reads them.
- `#0a1e33` / `#f6f2e8` in `primitives.css` are the captured terminal frame's
  real colors and stay as captured.
- The fluid editorial sizes (`--text-section`, `--text-display`,
  `--text-display-lg`) are the website's own layout steps above the token
  `display` role.

`lib/gpui-role-tokens.test.ts` checks that every role resolves to its token
and that every ink clears WCAG AA on every ground in every scheme.
