//! An opt-in atmosphere: one spatial ombre painted over a finished scene.
//!
//! A host paints its components through [`Theme`] as usual and then calls
//! [`Ombre::apply`] once, last, over the whole scene. That order is the whole
//! contract: every ground a component paints is already on the buffer, so
//! the pass repaints the cells whose background is one of the five structural
//! grounds (Sidebar, Background, Surface, Hover, Selected) and leaves glyphs,
//! every foreground, content and diff tints, action fills and any color the
//! tokens do not name exactly as they were painted.
//!
//! The wash is spatial, not a flat theme swatch: each cell is tinted toward
//! the blend of two token hues at its own position along the requested
//! [`Rect`], anchored to that rect even when only its visible intersection is
//! painted. The luminance the source ground was audited with is restored
//! afterwards ([`crate::color::tint_keeping_luminance`]), so every contrast
//! the tokens passed still holds. A nonblank cell is repainted only when the
//! result keeps its ink at `min(original contrast, the ink's audited floor)`;
//! blank cells carry the gradient without that proof, so the wash stays
//! continuous where nothing is written on it.
//!
//! | Palette | Runs between | Reads as |
//! |---|---|---|
//! | [`WaterPalette::Ocean`] | the logo ombre `#1E8FD8` → `#0B48BB` | deep water |
//! | [`WaterPalette::Lagoon`] | `Live` → `Primary` | the shallows |
//! | [`WaterPalette::Dusk`] | `Primary` → `Attention` | blue hour into a warm horizon |
//! | [`WaterPalette::Coral`] | `Danger` → `Attention` | a reef |
//! | [`WaterPalette::Graphite`] | the exact token grounds, no tint | graphite |
//!
//! In a light appearance the hues for Lagoon, Dusk and Coral are read from
//! the light token table, so paper gets pale washes of its own tokens; Ocean
//! keeps the logo pair, as a pale wash. Where a ground cannot hold a hue and
//! keep the luminance it was audited with (paper's pure white surface), it is
//! left exactly as the theme painted it.
//!
//! The pass runs at truecolor on a measured appearance only. At 256 colors,
//! 16 colors, `NO_COLOR`, ASCII-safe output and an unmeasured ground the
//! audited [`Theme`] is painted exactly as everywhere else.
//!
//! [`WaterPalette::Graphite`] does not tint: on a dark truecolor terminal it
//! remaps the five structural grounds to the graphite token grounds (the ones
//! the desktop app paints), so an Ocean-themed host can show real graphite.
//! On a light terminal it is pass-through, because paper already is the light
//! token table. A theme already at [`crate::Ground::Graphite`] is left alone.
//!
//! The palette is atmosphere, never state: no state word or mark is given a
//! new hue, and nothing here runs on a clock or animates by itself.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::{
    Paint, Role, Theme,
    color::{ColorDepth, blend, relative_luminance, rgb, tint_keeping_luminance},
    detect::Appearance,
    theme::{Ground, LOGO_BOTTOM, LOGO_TOP},
};

/// The five structural grounds this pass may repaint, in elevation order.
const GROUNDS: [Role; 5] = [
    Role::Sidebar,
    Role::Background,
    Role::Surface,
    Role::Hover,
    Role::Selected,
];

/// The dark wash: the audited strength [`crate::theme::OCEAN_TINT`] uses.
const OMBRE_TINT: f64 = 0.5;

/// Paper washes, strongest first. The pass takes the strongest one whose
/// result keeps the source ground's luminance; `0.0` means "leave it alone".
const PAPER_TINTS: [f64; 3] = [0.16, 0.10, 0.06];

/// How far a wash may move a ground's WCAG luminance: one 8-bit channel step
/// near the middle of the range. A wash that cannot stay inside this is not
/// offered for that ground.
const LUMINANCE_ROUNDING: f32 = 0.005;

/// Floating-point comparison tolerance; channel rounding never permits a
/// wash to cross an audited contrast floor.
const CONTRAST_ROUNDING: f32 = 1e-5;

/// The body-text floor the tokens audit (`generate.py`).
const TEXT_FLOOR: f32 = 4.5;

/// The control-edge floor the tokens audit.
const CONTROL_FLOOR: f32 = 3.0;

/// Samples across the ramp used to prove a wash strength before it is used.
const STRENGTH_SAMPLES: u32 = 32;

/// The five atmospheres a scene can be finished with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaterPalette {
    /// The logo ombre: `#1E8FD8` into `#0B48BB`.
    Ocean,
    /// The shallows: `Live` into `Primary`.
    Lagoon,
    /// Blue hour: `Primary` into `Attention`.
    Dusk,
    /// A reef: `Danger` into `Attention`.
    Coral,
    /// The exact token grounds, no tint.
    Graphite,
}

impl WaterPalette {
    /// Every palette, in the order the gallery shows them.
    pub const ALL: [WaterPalette; 5] = [
        WaterPalette::Ocean,
        WaterPalette::Lagoon,
        WaterPalette::Dusk,
        WaterPalette::Coral,
        WaterPalette::Graphite,
    ];

    /// The palette's name, as a label reads it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            WaterPalette::Ocean => "Ocean",
            WaterPalette::Lagoon => "Lagoon",
            WaterPalette::Dusk => "Dusk",
            WaterPalette::Coral => "Coral",
            WaterPalette::Graphite => "Graphite",
        }
    }

    /// The two hues this ombre runs between, read from the theme's own token
    /// table so paper gets light-token washes. `None` for graphite, which
    /// maps grounds instead of tinting them.
    fn stops(self, theme: &Theme) -> Option<(u32, u32)> {
        Some(match self {
            WaterPalette::Graphite => return None,
            // The logo pair is the brand ombre in both appearances.
            WaterPalette::Ocean => (LOGO_TOP, LOGO_BOTTOM),
            WaterPalette::Lagoon => (theme.token_hex(Role::Live), theme.token_hex(Role::Accent)),
            WaterPalette::Dusk => (
                theme.token_hex(Role::Accent),
                theme.token_hex(Role::Attention),
            ),
            WaterPalette::Coral => (
                theme.token_hex(Role::Danger),
                theme.token_hex(Role::Attention),
            ),
        })
    }
}

/// Which way the wash travels across the requested area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OmbreDirection {
    /// Runs down the area: the same wash on every column.
    Vertical,
    /// Runs from the top-left corner to the bottom-right corner (the
    /// default).
    #[default]
    Diagonal,
}

/// A finishing pass: one palette, one direction.
///
/// ```no_run
/// use codewhale_ratatui::{Ombre, Theme, WaterPalette};
/// # fn draw(area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
/// let theme = Theme::detect();
/// // ... the host paints its scene through `theme` first ...
/// Ombre::new(WaterPalette::Ocean).apply(area, buf, &theme);
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ombre {
    pub palette: WaterPalette,
    pub direction: OmbreDirection,
}

impl Ombre {
    /// The palette at its default [`OmbreDirection::Diagonal`].
    #[must_use]
    pub const fn new(palette: WaterPalette) -> Self {
        Self {
            palette,
            direction: OmbreDirection::Diagonal,
        }
    }

    /// Choose the direction the wash travels.
    #[must_use]
    pub const fn direction(mut self, direction: OmbreDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Repaint the recognized structural grounds in the buffer.
    ///
    /// Call this last, after the scene has been painted through `theme`.
    /// `area` is the area the gradient is anchored to: callers that clip a
    /// larger scene still pass the full rect, and the visible part keeps the
    /// colors it would have had at full size. Areas that do not intersect the
    /// buffer, zero-sized areas and `u16::MAX` extents are all safe.
    pub fn apply(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        // Only truecolor on a measured appearance has audited grounds to
        // move; every other profile keeps the theme exactly as it is.
        if theme.depth() != ColorDepth::TrueColor || !theme.caps().paints_tokens() {
            return;
        }
        let visible = area.intersection(*buf.area());
        if visible.is_empty() {
            return;
        }
        match self.palette {
            // Paper is already the light token table.
            WaterPalette::Graphite if matches!(theme.caps().appearance, Appearance::Light) => {}
            WaterPalette::Graphite if theme.ground_kind() == Ground::Graphite => {}
            WaterPalette::Graphite => remap_graphite(area, visible, buf, theme),
            _ => self.wash(area, visible, buf, theme),
        }
    }

    /// Tint each structural ground toward the palette hue at its own ramp
    /// position.
    fn wash(&self, requested: Rect, visible: Rect, buf: &mut Buffer, theme: &Theme) {
        let Some((start, end)) = self.palette.stops(theme) else {
            return;
        };
        let light = matches!(theme.caps().appearance, Appearance::Light);
        let mut present = [false; GROUNDS.len()];
        for y in visible.top()..visible.bottom() {
            for x in visible.left()..visible.right() {
                if let Some(ground) = ground_index(buf[(x, y)].bg, theme) {
                    present[ground] = true;
                }
            }
        }
        let mut sources = [0_u32; GROUNDS.len()];
        let mut grounds = [0.0_f32; GROUNDS.len()];
        let mut strengths = [0.0_f64; GROUNDS.len()];
        for (ground, on) in present.iter().enumerate() {
            if !*on {
                continue;
            }
            sources[ground] = theme.token_hex(GROUNDS[ground]);
            grounds[ground] = relative_luminance(rgb(sources[ground])).unwrap_or(0.0);
            strengths[ground] = wash_strength(sources[ground], start, end, light);
        }
        let (base, rows) = ramp_rows(visible, requested, self.direction);
        let den = ramp_den(requested, self.direction);
        let mut table: Vec<[Option<Wash>; GROUNDS.len()]> = vec![[None; GROUNDS.len()]; rows];
        for (ground, on) in present.iter().enumerate() {
            if !*on || strengths[ground] == 0.0 {
                continue;
            }
            for (row, washes) in table.iter_mut().enumerate() {
                let sample = base + row as u32;
                let t = if den == 0 {
                    0.0
                } else {
                    sample.min(den) as f32 / den as f32
                };
                let Some(hue) = hex_of(blend(rgb(end), rgb(start), t)) else {
                    continue;
                };
                washes[ground] = Wash::of(tint_keeping_luminance(
                    sources[ground],
                    hue,
                    strengths[ground],
                ));
            }
        }
        repaint(
            visible,
            requested,
            self.direction,
            buf,
            theme,
            &Washes {
                base,
                rows: table,
                grounds,
            },
        );
    }
}

impl Paint for Ombre {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.apply(area, buf, theme);
    }
}

/// Map the five structural grounds to the graphite token grounds, keeping
/// any cell whose ink would not survive the move.
fn remap_graphite(requested: Rect, visible: Rect, buf: &mut Buffer, theme: &Theme) {
    let graphite = theme.ground(Ground::Graphite);
    let mut entries: [Option<Wash>; GROUNDS.len()] = [None; GROUNDS.len()];
    let mut grounds = [0.0_f32; GROUNDS.len()];
    for (index, role) in GROUNDS.iter().enumerate() {
        entries[index] = graphite.color(*role).and_then(hex_of).and_then(Wash::of);
        grounds[index] = relative_luminance(rgb(theme.token_hex(*role))).unwrap_or(0.0);
    }
    let (base, rows) = ramp_rows(visible, requested, OmbreDirection::Vertical);
    repaint(
        visible,
        requested,
        OmbreDirection::Vertical,
        buf,
        theme,
        &Washes {
            base,
            rows: vec![entries; rows],
            grounds,
        },
    );
}

/// One repainted ground color and the luminance it carries.
#[derive(Clone, Copy)]
struct Wash {
    hex: u32,
    luminance: f32,
}

impl Wash {
    fn of(hex: u32) -> Option<Self> {
        Some(Self {
            hex,
            luminance: relative_luminance(rgb(hex))?,
        })
    }
}

/// The washes for one scene: one row per ramp sample, one entry per ground.
struct Washes {
    /// The ramp sample row `0` stands for.
    base: u32,
    rows: Vec<[Option<Wash>; GROUNDS.len()]>,
    /// The luminance each source ground was audited with.
    grounds: [f32; GROUNDS.len()],
}

impl Washes {
    /// The wash row for a cell inside `requested`.
    fn at(&self, x: u16, y: u16, area: Rect, direction: OmbreDirection) -> Option<&[Option<Wash>]> {
        let sample = ramp_offset(x, y, area, direction).checked_sub(self.base)?;
        self.rows
            .get(usize::try_from(sample).ok()?)
            .map(|row| row.as_slice())
    }
}

/// Repaint every visible cell of a recognized ground, keeping any cell whose
/// ink the wash would make unreadable.
fn repaint(
    visible: Rect,
    requested: Rect,
    direction: OmbreDirection,
    buf: &mut Buffer,
    theme: &Theme,
    washes: &Washes,
) {
    let mut ink = Ink::default();
    for y in visible.top()..visible.bottom() {
        for x in visible.left()..visible.right() {
            let Some(ground) = ground_index(buf[(x, y)].bg, theme) else {
                continue;
            };
            let Some(row) = washes.at(x, y, requested, direction) else {
                continue;
            };
            let Some(wash) = row.get(ground).copied().flatten() else {
                continue;
            };
            let cell = &buf[(x, y)];
            // Reversed cells display the foreground as their background.
            // Preserve that caller-authored selection instead of proving
            // contrast against the wrong side of the swap.
            if cell.modifier.contains(Modifier::REVERSED) {
                continue;
            }
            if !cell.symbol().trim().is_empty()
                && !ink.allows(theme, cell.fg, washes.grounds[ground], wash.luminance)
            {
                continue;
            }
            buf[(x, y)].set_style(Style::default().bg(rgb(wash.hex)));
        }
    }
}

/// The last ink examined: paintings come in runs, so one slot is enough and
/// the per-cell work stays a comparison.
#[derive(Default)]
struct Ink {
    color: Option<Color>,
    luminance: Option<f32>,
    floor: f32,
}

impl Ink {
    /// Whether replacing `old` with `new` keeps this ink at
    /// `min(original contrast, the ink's floor)`, within rounding.
    fn allows(&mut self, theme: &Theme, ink: Color, old: f32, new: f32) -> bool {
        if self.color != Some(ink) {
            self.color = Some(ink);
            self.luminance = relative_luminance(ink);
            self.floor = ink_floor(theme, ink);
        }
        // Ink the terminal owns (`Reset`, indices 0..=15, `None`) cannot be
        // measured; its cell keeps the screen it already had.
        let Some(luminance) = self.luminance else {
            return false;
        };
        ratio(luminance, new) + CONTRAST_ROUNDING >= ratio(luminance, old).min(self.floor)
    }
}

/// The floor an ink was audited against: body text, or a control edge for
/// the roles the tokens hold to 3:1. Ink the tokens do not name is held to
/// body text, the strictest default.
fn ink_floor(theme: &Theme, ink: Color) -> f32 {
    let roles = theme.roles_of(ink, false);
    if roles.is_empty() {
        return TEXT_FLOOR;
    }
    roles
        .iter()
        .copied()
        .map(|role| match role {
            Role::Dim | Role::Border | Role::BorderStrong => CONTROL_FLOOR,
            _ => TEXT_FLOOR,
        })
        .fold(0.0_f32, f32::max)
}

/// WCAG contrast from two relative luminances, as
/// [`crate::color::contrast_ratio`] computes it from colors; the pass already
/// holds both, so no channel math repeats per cell.
fn ratio(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// The strongest wash `source` can take toward the `start` → `end` hues while
/// keeping the luminance it was audited with. `0.0` means the ground cannot
/// hold a hue at all and is left exactly as painted.
fn wash_strength(source: u32, start: u32, end: u32, light: bool) -> f64 {
    let Some(want) = relative_luminance(rgb(source)) else {
        return 0.0;
    };
    let ladder: &[f64] = if light { &PAPER_TINTS } else { &[OMBRE_TINT] };
    for amount in ladder {
        let keeps = (0..=STRENGTH_SAMPLES).all(|i| {
            let t = i as f32 / STRENGTH_SAMPLES as f32;
            let Some(hue) = hex_of(blend(rgb(end), rgb(start), t)) else {
                return false;
            };
            let tinted = tint_keeping_luminance(source, hue, *amount);
            relative_luminance(rgb(tinted))
                .is_some_and(|have| (have - want).abs() <= LUMINANCE_ROUNDING)
        });
        if keeps {
            return *amount;
        }
    }
    0.0
}

/// The ramp rows the visible intersection needs, and the sample its first
/// row stands for. Anchored to `requested`, so clipping never re-scales the
/// gradient.
fn ramp_rows(visible: Rect, requested: Rect, direction: OmbreDirection) -> (u32, usize) {
    let first = ramp_offset(visible.x, visible.y, requested, direction);
    let last_x = u32::from(visible.x) + u32::from(visible.width).saturating_sub(1);
    let last_y = u32::from(visible.y) + u32::from(visible.height).saturating_sub(1);
    let last = ramp_offset_value(last_x, last_y, requested, direction);
    (first, usize::try_from(last - first).unwrap_or(0) + 1)
}

/// How far along the requested area's ramp a coordinate is.
fn ramp_offset(x: u16, y: u16, area: Rect, direction: OmbreDirection) -> u32 {
    ramp_offset_value(u32::from(x), u32::from(y), area, direction)
}

/// [`ramp_offset`] for coordinates that may exceed `u16` during the
/// arithmetic (the last cell of a clipped intersection).
fn ramp_offset_value(x: u32, y: u32, area: Rect, direction: OmbreDirection) -> u32 {
    let dx = x.saturating_sub(u32::from(area.x));
    let dy = y.saturating_sub(u32::from(area.y));
    match direction {
        OmbreDirection::Vertical => dy,
        OmbreDirection::Diagonal => dx + dy,
    }
}

/// The largest ramp sample the requested area spans; `0` when it cannot vary.
fn ramp_den(area: Rect, direction: OmbreDirection) -> u32 {
    match direction {
        OmbreDirection::Vertical => u32::from(area.height.saturating_sub(1)),
        OmbreDirection::Diagonal => {
            u32::from(area.width.saturating_sub(1)) + u32::from(area.height.saturating_sub(1))
        }
    }
}

/// Which structural ground a background is, exactly as the theme resolves
/// it. `Reset`, content tints, action fills and custom colors match nothing.
fn ground_index(bg: Color, theme: &Theme) -> Option<usize> {
    GROUNDS
        .iter()
        .position(|role| theme.color(*role) == Some(bg))
}

/// `0xRRGGBB` for an RGB color; `None` for anything the terminal owns.
fn hex_of(color: Color) -> Option<u32> {
    match color {
        Color::Rgb(r, g, b) => Some(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)),
        _ => None,
    }
}
