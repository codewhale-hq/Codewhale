//! One theme: color roles from the Codewhale design tokens.
//!
//! Components name a [`Role`]; they never name a color. [`Theme`] resolves a
//! role for the terminal it is painting on, every time it paints, so a theme
//! or depth change reaches every component on the next frame.
//!
//! | Depth | Grounds | Ink | State hues |
//! |---|---|---|---|
//! | TrueColor | exact token RGB | exact RGB | exact RGB |
//! | Ansi256 | nearest fixed index | generated table, audited for contrast | generated table |
//! | Ansi16 | none (the terminal owns its ground) | `Reset`; `Muted`, `Hint` and `Dim` add `DIM` | Blue, Green, Yellow, Red |
//! | Monochrome (`NO_COLOR`) | none | `Reset`; hierarchy by `BOLD`/`DIM` only | `Reset`: the mark and word carry state |
//!
//! Dark terminals take the blue ombre by default ([`Ground::Ocean`]): the
//! token grounds and quiet lines tinted toward the logo's deep blue at the
//! same luminance, so every contrast the tokens audit still holds. It shows
//! at truecolor only; the 256-color cube has no navy fine enough, so 256
//! colors keep the token grounds. [`Ground::Graphite`] keeps the exact token
//! grounds everywhere.
//!
//! When nothing measured the terminal's ground ([`Appearance::Unknown`]), a
//! truecolor terminal is painted as at ANSI-16: its named colors were chosen
//! for its own ground, and ours were not. This is stricter than the engine's
//! palette, which assumes dark. A host that knows better (a theme setting the
//! person chose) sets [`Caps::appearance`] itself; a person can force it with
//! `CODEWHALE_APPEARANCE=light` or `=dark`.

use ratatui::style::{Color, Modifier, Style};

use crate::color::{ColorDepth, rgb};
use crate::detect::{Appearance, terminal_background};
use crate::roles;

pub use crate::roles::{Role, TOKENS_VERSION};

/// The logo's ombre, top-left to bottom-right (`#1E8FD8` to `#0B48BB`), from
/// the design direction. The whale wears it; [`Ground::Ocean`] grounds lean
/// toward its deep end.
pub const LOGO_TOP: u32 = 0x1e8fd8;
pub const LOGO_BOTTOM: u32 = 0x0b48bb;

/// How far [`Ground::Ocean`] moves each dark ground and quiet line toward
/// [`LOGO_BOTTOM`] before restoring its luminance.
pub const OCEAN_TINT: f64 = 0.5;

/// Which grounds a dark theme paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Ground {
    /// Deep navy rising into the logo's blue: the preferred look.
    #[default]
    Ocean,
    /// The exact token grounds (graphite), as the desktop app paints them.
    Graphite,
}

impl Role {
    /// Whether [`Ground::Ocean`] tints this role toward the logo's blue: the
    /// five grounds and quiet lines. Ink, state hues and control edges keep
    /// their token values; the diff tints are re-mixed over the ombre's own
    /// background instead ([`Role::is_content_tint`]).
    #[must_use]
    pub const fn ocean_tinted(self) -> bool {
        matches!(
            self,
            Role::Sidebar
                | Role::Background
                | Role::Surface
                | Role::Hover
                | Role::Selected
                | Role::Border
        )
    }

    /// A content tint: a ground behind a line of content, painted behind
    /// `Foreground` ink only. Where grounds do not paint (16 colors,
    /// `NO_COLOR`) the tint is dropped and a glyph carries the meaning.
    #[must_use]
    pub const fn is_content_tint(self) -> bool {
        matches!(self, Role::DiffAddedTint | Role::DiffRemovedTint)
    }

    /// Whether this role is painted as a background: the five grounds and
    /// the content tints.
    #[must_use]
    pub const fn is_ground(self) -> bool {
        matches!(
            self,
            Role::Sidebar | Role::Background | Role::Surface | Role::Hover | Role::Selected
        ) || self.is_content_tint()
    }

    /// The named color a 16-color terminal shows for this role, or `None`
    /// when the role takes the terminal's own foreground.
    const fn ansi16(self) -> Option<Color> {
        match self {
            Role::Accent => Some(Color::Blue),
            Role::Live => Some(Color::Green),
            Role::Attention => Some(Color::Yellow),
            Role::Danger => Some(Color::Red),
            _ => None,
        }
    }

    /// Ink that recedes. Where color is unavailable it recedes with `DIM`.
    const fn recedes(self) -> bool {
        matches!(self, Role::Muted | Role::Hint | Role::Dim | Role::Border)
    }
}

/// What the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Caps {
    pub depth: ColorDepth,
    /// `CODEWHALE_ASCII_SAFE`: draw marks from the ASCII fallbacks.
    pub ascii: bool,
    pub appearance: Appearance,
}

impl Caps {
    /// Read depth, ASCII-safety and appearance from the environment and any
    /// completed [`crate::detect::probe_terminal_background`].
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with(
            |key| std::env::var_os(key),
            || terminal_background().appearance(),
        )
    }

    /// [`Caps::detect`] with an injected environment reader and ground
    /// detector, so every branch is testable without the process state.
    ///
    /// `CODEWHALE_APPEARANCE` (`light` or `dark`) overrides detection: the
    /// person's word beats a measurement, and the detector is not called.
    #[must_use]
    pub fn detect_with(
        get: impl Fn(&str) -> Option<std::ffi::OsString>,
        detect_ground: impl FnOnce() -> Appearance,
    ) -> Self {
        let ascii =
            get("CODEWHALE_ASCII_SAFE").is_some_and(|v| !v.is_empty() && v != "0" && v != "false");
        let forced = get("CODEWHALE_APPEARANCE").and_then(|v| {
            match v.to_string_lossy().trim().to_ascii_lowercase().as_str() {
                "light" => Some(Appearance::Light),
                "dark" => Some(Appearance::Dark),
                _ => None,
            }
        });
        Self {
            depth: ColorDepth::detect_with(&get),
            ascii,
            appearance: forced.unwrap_or_else(detect_ground),
        }
    }

    /// Whether this terminal shows token colors at all.
    #[must_use]
    pub const fn paints_tokens(&self) -> bool {
        matches!(self.depth, ColorDepth::TrueColor | ColorDepth::Ansi256)
            && !matches!(self.appearance, Appearance::Unknown)
    }
}

/// The Codewhale theme resolved for one terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    caps: Caps,
    grounds: bool,
    ground: Ground,
    native: Option<crate::TuiPalette>,
}

impl Theme {
    #[must_use]
    pub const fn new(caps: Caps) -> Self {
        Self {
            caps,
            grounds: true,
            ground: Ground::Ocean,
            native: None,
        }
    }

    /// Choose the dark grounds: the blue ombre (default) or graphite. Light
    /// terminals always paint the light token grounds.
    #[must_use]
    pub const fn ground(mut self, ground: Ground) -> Self {
        self.ground = ground;
        self.native = None;
        self
    }

    /// Use one of the palettes shipped by the current Codewhale TUI.
    /// Terminal capability and unknown-ground fallbacks remain in force.
    #[must_use]
    pub const fn tui_palette(mut self, palette: crate::TuiPalette) -> Self {
        self.native = Some(palette);
        self
    }

    #[must_use]
    pub const fn native_palette(&self) -> Option<crate::TuiPalette> {
        self.native
    }

    /// A distinct native TUI semantic ink, adapted by the same terminal rules.
    #[must_use]
    pub fn tui_ink(&self, ink: crate::TuiInk) -> Style {
        if self.caps.paints_tokens()
            && let Some(palette) = self.native
        {
            let color = match (self.depth(), palette.ink(ink)) {
                (ColorDepth::Ansi256, Color::Rgb(r, g, b)) => {
                    Color::Indexed(crate::color::rgb_to_ansi256(r, g, b))
                }
                (_, color) => color,
            };
            Style::default().fg(color)
        } else {
            self.fg(ink.fallback_role())
        }
    }

    /// Native background slots such as composer and footer. Limited color
    /// and unknown-ground terminals keep the existing capability fallbacks.
    #[must_use]
    pub fn tui_ground(&self, ground: crate::TuiGround) -> Style {
        if self.paints_grounds()
            && let Some(palette) = self.native
        {
            let color = match (self.depth(), palette.ground(ground)) {
                (ColorDepth::Ansi256, Color::Rgb(r, g, b)) => {
                    Color::Indexed(crate::color::rgb_to_ansi256(r, g, b))
                }
                (_, color) => color,
            };
            Style::default().bg(color)
        } else {
            self.bg(ground.fallback_role())
        }
    }

    /// Codewhale's default Underwater, or its light palette on a light terminal.
    #[must_use]
    pub const fn tui(self) -> Self {
        self.tui_palette(if self.light() {
            crate::TuiPalette::WhaleLight
        } else {
            crate::TuiPalette::Underwater
        })
    }

    /// The dark grounds this theme was built with.
    #[must_use]
    pub const fn ground_kind(&self) -> Ground {
        self.ground
    }

    /// Detect everything from the environment.
    #[must_use]
    pub fn detect() -> Self {
        Self::new(Caps::detect())
    }

    /// Leave the terminal's own ground alone, even where token grounds would
    /// render. Raised panels and selection still paint.
    #[must_use]
    pub const fn without_base_ground(mut self) -> Self {
        self.grounds = false;
        self
    }

    #[must_use]
    pub const fn caps(&self) -> Caps {
        self.caps
    }

    #[must_use]
    pub const fn ascii(&self) -> bool {
        self.caps.ascii
    }

    #[must_use]
    pub const fn depth(&self) -> ColorDepth {
        self.caps.depth
    }

    /// Which token table this theme reads. Unknown reads the dark table,
    /// but [`Caps::paints_tokens`] is false then, so no token color reaches
    /// the screen.
    const fn light(&self) -> bool {
        matches!(self.caps.appearance, Appearance::Light)
    }

    /// The exact token color (`0xRRGGBB`), before depth adaptation.
    #[must_use]
    pub const fn token_hex(&self, role: Role) -> u32 {
        if let Some(palette) = self.native
            && let Color::Rgb(r, g, b) = palette.color(role)
        {
            return ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
        }
        if self.light() {
            roles::LIGHT[role.index()]
        } else if matches!(self.ground, Ground::Ocean) {
            roles::OCEAN[role.index()]
        } else {
            roles::DARK[role.index()]
        }
    }

    /// The exact token color, before depth adaptation. Under
    /// [`Ground::Ocean`] on a dark ground, grounds and quiet lines are the
    /// tinted values; 256 colors still show the token grounds.
    #[must_use]
    pub const fn token(&self, role: Role) -> Color {
        match self.native {
            Some(palette) => palette.color(role),
            None => rgb(self.token_hex(role)),
        }
    }

    /// The color this terminal shows for `role`, or `None` when it shows the
    /// terminal's own color.
    #[must_use]
    pub fn color(&self, role: Role) -> Option<Color> {
        if self.caps.paints_tokens() {
            if self.native.is_some() {
                return Some(match (self.caps.depth, self.token(role)) {
                    (ColorDepth::Ansi256, Color::Rgb(r, g, b)) => {
                        Color::Indexed(crate::color::rgb_to_ansi256(r, g, b))
                    }
                    (_, color) => color,
                });
            }
            return Some(match self.caps.depth {
                ColorDepth::TrueColor => self.token(role),
                _ if self.light() => Color::Indexed(roles::LIGHT_256[role.index()]),
                _ => Color::Indexed(roles::DARK_256[role.index()]),
            });
        }
        match self.caps.depth {
            ColorDepth::Monochrome => None,
            _ => role.ansi16(),
        }
    }

    /// Ink: `role` as a foreground.
    #[must_use]
    pub fn fg(&self, role: Role) -> Style {
        let style = Style::default();
        match self.color(role) {
            Some(color) => style.fg(color),
            None if role.recedes() => style.add_modifier(Modifier::DIM),
            None => style,
        }
    }

    /// Ground: `role` as a background, or nothing where grounds do not paint.
    /// The base `Background` also stays unpainted after
    /// [`Theme::without_base_ground`].
    #[must_use]
    pub fn bg(&self, role: Role) -> Style {
        if !self.caps.paints_tokens() || (!self.grounds && role == Role::Background) {
            return Style::default();
        }
        match self.color(role) {
            Some(color) => Style::default().bg(color),
            None => Style::default(),
        }
    }

    /// Whether grounds paint at all. Where they do not, components must draw
    /// an edge or a mark instead of relying on a fill.
    #[must_use]
    pub fn paints_grounds(&self) -> bool {
        self.caps.paints_tokens()
    }

    /// Whether grounds `a` and `b` look different on this terminal. False
    /// where grounds do not paint, and where the two roles quantize to the
    /// same color (light 256-color `Surface` and `Background`, for one), so
    /// a component relying on the difference must draw an edge instead.
    #[must_use]
    pub fn grounds_differ(&self, a: Role, b: Role) -> bool {
        self.bg(a) != self.bg(b)
    }

    /// Whether the base `Background` ground is painted by this theme (false
    /// without grounds and after [`Theme::without_base_ground`]).
    #[must_use]
    pub fn paints_base_ground(&self) -> bool {
        self.bg(Role::Background).bg.is_some()
    }

    /// Every role that resolves to `color` here, grounds first for
    /// backgrounds and ink first for foregrounds. More than one means those
    /// roles look the same at this depth.
    #[must_use]
    pub fn roles_of(&self, color: Color, as_ground: bool) -> Vec<Role> {
        let (mut preferred, rest): (Vec<Role>, Vec<Role>) = Role::ALL
            .iter()
            .copied()
            .filter(|r| self.color(*r) == Some(color))
            .partition(|r| r.is_ground() == as_ground);
        if preferred.is_empty() {
            preferred = rest;
        }
        preferred
    }

    /// Reverse lookup for snapshots: the first of [`Theme::roles_of`].
    #[must_use]
    pub fn role_of(&self, color: Color, as_ground: bool) -> Option<Role> {
        self.roles_of(color, as_ground).first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::contrast_ratio;

    fn theme(depth: ColorDepth, appearance: Appearance) -> Theme {
        Theme::new(Caps {
            depth,
            ascii: false,
            appearance,
        })
    }

    #[test]
    fn truecolor_reads_the_token_tables() {
        let dark = theme(ColorDepth::TrueColor, Appearance::Dark);
        assert_eq!(
            dark.color(Role::Primary),
            Some(rgb(crate::tokens::DARK.primary))
        );
        let light = theme(ColorDepth::TrueColor, Appearance::Light);
        assert_eq!(
            light.color(Role::Danger),
            Some(rgb(crate::tokens::LIGHT.danger))
        );
    }

    /// The blue ombre tints dark grounds and the quiet line toward the
    /// logo's deep blue, keeps every ink, and shows only at truecolor.
    #[test]
    fn ocean_tints_dark_grounds_only() {
        let ocean = theme(ColorDepth::TrueColor, Appearance::Dark);
        let graphite = ocean.ground(Ground::Graphite);
        assert_eq!(ocean.ground_kind(), Ground::Ocean, "the preferred look");
        for role in Role::ALL {
            let (o, g) = (ocean.token_hex(role), graphite.token_hex(role));
            if role.is_content_tint() {
                // Mixed over the ombre's own background; the contrast test
                // holds them to their floors.
                assert_ne!(o, g, "{role:?} sits on the ombre, not on graphite");
            } else if role.ocean_tinted() {
                let (r, gr, b) = ((o >> 16) & 0xff, (o >> 8) & 0xff, o & 0xff);
                assert!(b > r && b > gr, "{role:?} #{o:06x} leans blue");
                let lum = |c| crate::color::relative_luminance(rgb(c)).unwrap();
                assert!((lum(o) - lum(g)).abs() < 0.002, "{role:?} keeps luminance");
            } else {
                assert_eq!(o, g, "{role:?} keeps its token ink");
            }
        }
        for depth in [
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
            ColorDepth::Monochrome,
        ] {
            let o = theme(depth, Appearance::Dark);
            let g = o.ground(Ground::Graphite);
            for role in Role::ALL {
                assert_eq!(o.color(role), g.color(role), "{depth:?} {role:?}");
            }
        }
        let light = theme(ColorDepth::TrueColor, Appearance::Light);
        for role in Role::ALL {
            assert_eq!(
                light.color(role),
                light.ground(Ground::Graphite).color(role),
                "light {role:?}"
            );
        }
    }

    #[test]
    fn unknown_ground_never_paints_token_colors() {
        let t = theme(ColorDepth::TrueColor, Appearance::Unknown);
        assert_eq!(t.bg(Role::Background), Style::default());
        assert_eq!(t.color(Role::Foreground), None);
        assert_eq!(t.color(Role::Live), Some(Color::Green));
        assert!(t.fg(Role::Muted).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn no_color_uses_only_modifiers() {
        let t = theme(ColorDepth::Monochrome, Appearance::Dark);
        for role in Role::ALL {
            let fg = t.fg(role);
            assert_eq!(fg.fg, None, "{role:?}");
            assert_eq!(t.bg(role).bg, None, "{role:?}");
        }
        assert!(t.fg(Role::Muted).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn ansi16_state_hues_are_distinct() {
        let t = theme(ColorDepth::Ansi16, Appearance::Dark);
        let hues: Vec<_> = [Role::Accent, Role::Live, Role::Attention, Role::Danger]
            .iter()
            .map(|r| t.color(*r).unwrap())
            .collect();
        for (i, a) in hues.iter().enumerate() {
            for b in &hues[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(t.bg(Role::Selected), Style::default());
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.into())
    }

    #[test]
    fn caps_read_ascii_safety_from_the_environment() {
        let on = Caps::detect_with(env(&[("CODEWHALE_ASCII_SAFE", "1")]), || Appearance::Dark);
        assert!(on.ascii);
        for off in ["", "0", "false"] {
            let caps =
                Caps::detect_with(env(&[("CODEWHALE_ASCII_SAFE", off)]), || Appearance::Dark);
            assert!(!caps.ascii, "{off:?}");
        }
        assert!(!Caps::detect_with(env(&[]), || Appearance::Dark).ascii);
    }

    #[test]
    fn a_forced_appearance_beats_detection() {
        let light = Caps::detect_with(
            env(&[
                ("CODEWHALE_APPEARANCE", "Light"),
                ("COLORTERM", "truecolor"),
            ]),
            || panic!("forced appearance must not probe"),
        );
        assert_eq!(light.appearance, Appearance::Light);
        assert!(light.paints_tokens());
        let dark = Caps::detect_with(env(&[("CODEWHALE_APPEARANCE", "dark")]), || {
            Appearance::Unknown
        });
        assert_eq!(dark.appearance, Appearance::Dark);
        let junk = Caps::detect_with(env(&[("CODEWHALE_APPEARANCE", "sepia")]), || {
            Appearance::Unknown
        });
        assert_eq!(junk.appearance, Appearance::Unknown);
    }

    /// Profiles the snapshots do not render separately, because they must
    /// look exactly like one that is rendered.
    #[test]
    fn unsnapshotted_profiles_match_their_twins() {
        let dark16 = theme(ColorDepth::Ansi16, Appearance::Dark);
        let light16 = theme(ColorDepth::Ansi16, Appearance::Light);
        let unknown256 = theme(ColorDepth::Ansi256, Appearance::Unknown);
        for role in Role::ALL {
            assert_eq!(light16.fg(role), dark16.fg(role), "{role:?}");
            assert_eq!(light16.bg(role), Style::default(), "{role:?}");
            assert_eq!(unknown256.fg(role), dark16.fg(role), "{role:?}");
            assert_eq!(unknown256.bg(role), Style::default(), "{role:?}");
        }
    }

    /// Grounds that quantize to one color at 256 colors. Each one needs an
    /// edge or a mark wherever a component relies on the difference; a new
    /// collapse fails here so it gets that review.
    #[test]
    fn collapsed_grounds_are_known() {
        let grounds = [
            Role::Sidebar,
            Role::Background,
            Role::Surface,
            Role::Hover,
            Role::Selected,
        ];
        let mut collapsed = Vec::new();
        for appearance in [Appearance::Dark, Appearance::Light] {
            let t = theme(ColorDepth::Ansi256, appearance);
            for (i, a) in grounds.iter().enumerate() {
                for b in &grounds[i + 1..] {
                    if !t.grounds_differ(*a, *b) {
                        collapsed.push(format!("{appearance:?} {a:?}={b:?}"));
                    }
                }
            }
        }
        assert_eq!(
            collapsed,
            ["Dark Surface=Hover", "Light Background=Surface"],
            "Panel edges raised cards where Surface=Background; nothing paints Hover yet"
        );
    }

    /// Contrast audit at the two depths that paint token colors, with the
    /// design's own floors (`generate.py`): every text role clears WCAG AA
    /// (4.5:1) on every ground it can sit on, and the strong border 3:1.
    #[test]
    fn contrast_holds_at_truecolor_and_ansi256() {
        let grounds = [
            Role::Background,
            Role::Surface,
            Role::Sidebar,
            Role::Hover,
            Role::Selected,
        ];
        let mut failures = Vec::new();
        for (appearance, ground) in [
            (Appearance::Dark, Ground::Ocean),
            (Appearance::Dark, Ground::Graphite),
            (Appearance::Light, Ground::Ocean),
        ] {
            for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
                let t = theme(depth, appearance).ground(ground);
                for ground in grounds {
                    let bg = t.color(ground).unwrap();
                    for (ink, floor) in [
                        (Role::Foreground, 4.5),
                        (Role::Muted, 4.5),
                        (Role::Primary, 4.5),
                        (Role::Accent, 4.5),
                        (Role::Live, 4.5),
                        (Role::Attention, 4.5),
                        (Role::Danger, 4.5),
                        (Role::Hint, 4.5),
                        (Role::Dim, 3.0),
                        (Role::BorderStrong, 3.0),
                    ] {
                        let ratio = contrast_ratio(t.color(ink).unwrap(), bg).unwrap();
                        if ratio < floor {
                            failures.push(format!(
                                "{appearance:?} {depth:?} {ink:?} on {ground:?} ({:?}): {ratio:.2} < {floor}",
                                t.ground_kind()
                            ));
                        }
                    }
                }
                // Diff tints sit behind body ink and their own state hue;
                // line numbers (`Muted`) only need the 3:1 the 256-color
                // cube can give a green.
                for (tint, hue) in [
                    (Role::DiffAddedTint, Role::Live),
                    (Role::DiffRemovedTint, Role::Danger),
                ] {
                    let bg = t.color(tint).unwrap();
                    for (ink, floor) in [(Role::Foreground, 4.5), (hue, 4.5), (Role::Muted, 3.0)] {
                        let ratio = contrast_ratio(t.color(ink).unwrap(), bg).unwrap();
                        if ratio < floor {
                            failures.push(format!(
                                "{appearance:?} {depth:?} {ink:?} on {tint:?} ({:?}): {ratio:.2} < {floor}",
                                t.ground_kind()
                            ));
                        }
                    }
                }
                let on_primary = contrast_ratio(
                    t.color(Role::PrimaryForeground).unwrap(),
                    t.color(Role::Primary).unwrap(),
                )
                .unwrap();
                if on_primary < 4.5 {
                    failures.push(format!(
                        "{appearance:?} {depth:?} PrimaryForeground on Primary: {on_primary:.2} < 4.5"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "contrast failures:\n{}",
            failures.join("\n")
        );
    }

    /// Foreground, Muted, Hint and Dim step down against every ground, in
    /// every appearance and depth that paints token colors, and no two of
    /// them (or the strong border) show the same color.
    #[test]
    fn the_ink_ladder_steps_down() {
        let ladder = [Role::Foreground, Role::Muted, Role::Hint, Role::Dim];
        for (appearance, ground) in [
            (Appearance::Dark, Ground::Ocean),
            (Appearance::Dark, Ground::Graphite),
            (Appearance::Light, Ground::Ocean),
        ] {
            for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
                let t = theme(depth, appearance).ground(ground);
                for ground in [Role::Background, Role::Surface, Role::Selected] {
                    let bg = t.color(ground).unwrap();
                    let ratios: Vec<f32> = ladder
                        .iter()
                        .map(|r| contrast_ratio(t.color(*r).unwrap(), bg).unwrap())
                        .collect();
                    assert!(
                        ratios.windows(2).all(|w| w[0] > w[1]),
                        "{appearance:?} {depth:?} on {ground:?}: {ratios:?}"
                    );
                }
                let inks = [
                    Role::Foreground,
                    Role::Muted,
                    Role::Hint,
                    Role::Dim,
                    Role::BorderStrong,
                ];
                for (i, a) in inks.iter().enumerate() {
                    for b in &inks[i + 1..] {
                        if depth == ColorDepth::Ansi256
                            || (*a, *b) != (Role::Dim, Role::BorderStrong)
                        {
                            assert_ne!(
                                t.color(*a),
                                t.color(*b),
                                "{appearance:?} {depth:?}: {a:?} and {b:?} look the same"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The diff tints are grounds that paint only where grounds paint, and
    /// they differ from every other ground, so an added line never reads as
    /// a selected row.
    #[test]
    fn diff_tints_are_distinct_grounds_that_drop_where_grounds_cannot_paint() {
        let others = [
            Role::Sidebar,
            Role::Background,
            Role::Surface,
            Role::Hover,
            Role::Selected,
        ];
        for appearance in [Appearance::Dark, Appearance::Light] {
            for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
                let t = theme(depth, appearance);
                for tint in [Role::DiffAddedTint, Role::DiffRemovedTint] {
                    assert!(tint.is_ground() && tint.is_content_tint() && !tint.is_derived());
                    for other in others {
                        assert!(
                            t.grounds_differ(tint, other),
                            "{appearance:?} {depth:?}: {tint:?} looks like {other:?}"
                        );
                    }
                }
                assert!(t.grounds_differ(Role::DiffAddedTint, Role::DiffRemovedTint));
            }
            for depth in [ColorDepth::Ansi16, ColorDepth::Monochrome] {
                let t = theme(depth, appearance);
                for tint in [Role::DiffAddedTint, Role::DiffRemovedTint] {
                    assert_eq!(t.bg(tint), Style::default(), "{appearance:?} {depth:?}");
                }
            }
        }
        let unknown = theme(ColorDepth::TrueColor, Appearance::Unknown);
        assert_eq!(unknown.bg(Role::DiffAddedTint), Style::default());
    }

    /// Without color, the quiet inks recede by `DIM`, as `Muted` does.
    #[test]
    fn hint_and_dim_recede_without_color() {
        for depth in [ColorDepth::Ansi16, ColorDepth::Monochrome] {
            let t = theme(depth, Appearance::Dark);
            for role in [Role::Muted, Role::Hint, Role::Dim] {
                assert!(
                    t.fg(role).add_modifier.contains(Modifier::DIM),
                    "{depth:?} {role:?}"
                );
            }
        }
    }

    #[test]
    fn nineteen_roles_two_of_them_derived() {
        assert_eq!(Role::COUNT, 19);
        assert_eq!(Role::ALL.len(), 19);
        let derived: Vec<Role> = Role::ALL.into_iter().filter(|r| r.is_derived()).collect();
        assert_eq!(derived, [Role::Hint, Role::Dim]);
        for role in Role::ALL {
            assert_eq!(role.token_name().is_none(), role.is_derived(), "{role:?}");
        }
    }
}
