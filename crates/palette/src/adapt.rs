//! Color adaptation for palette mode, community themes, and terminal depth.

use ratatui::style::Color;

use super::detect::PaletteMode;
use super::ids::ThemeId;
use super::themes::{GRAYSCALE_UI_THEME, LIGHT_UI_THEME, SOLARIZED_LIGHT_UI_THEME, UiTheme};
use super::tokens::*;

#[must_use]
pub fn adapt_fg_for_palette_mode(color: Color, _bg: Color, mode: PaletteMode) -> Color {
    match mode {
        PaletteMode::Dark => color,
        PaletteMode::Light => adapt_fg_for_light_palette(color),
        PaletteMode::Grayscale => adapt_fg_for_grayscale_palette(color),
        PaletteMode::SolarizedLight => adapt_fg_for_solarized_light_palette(color),
    }
}

#[must_use]
pub fn adapt_bg_for_palette_mode(color: Color, mode: PaletteMode) -> Color {
    match mode {
        PaletteMode::Dark => color,
        PaletteMode::Light => adapt_bg_for_light_palette(color),
        PaletteMode::Grayscale => adapt_bg_for_grayscale_palette(color),
        PaletteMode::SolarizedLight => adapt_bg_for_solarized_light_palette(color),
    }
}

fn adapt_fg_for_light_palette(color: Color) -> Color {
    if color == TEXT_BODY || color == SELECTION_TEXT || color == Color::White {
        LIGHT_TEXT_BODY
    } else if color == TEXT_SECONDARY || color == TEXT_MUTED {
        LIGHT_TEXT_MUTED
    } else if color == TEXT_HINT || color == TEXT_DIM {
        LIGHT_TEXT_HINT
    } else if color == TEXT_SOFT || color == TEXT_TOOL_OUTPUT {
        LIGHT_TEXT_SOFT
    } else if color == BORDER_COLOR {
        LIGHT_BORDER
    } else if color == TEXT_ACCENT || color == ACCENT_TOOL_LIVE {
        LIGHT_LIVE
    } else if color == WHALE_ACTION {
        LIGHT_ACTION
    } else if color == MODE_AGENT {
        LIGHT_UI_THEME.mode_agent
    } else if color == WHALE_HUMAN {
        LIGHT_HUMAN
    } else if color == MODE_PLAN {
        LIGHT_UI_THEME.mode_plan
    } else if color == TEXT_REASONING || color == ACCENT_REASONING_LIVE {
        Color::Rgb(146, 64, 14)
    } else if color == ACCENT_TOOL_ISSUE || color == WHALE_ERROR || color == STATUS_ERROR {
        LIGHT_DANGER
    } else if color == MODE_YOLO {
        LIGHT_UI_THEME.mode_yolo
    } else if color == STATUS_WARNING {
        LIGHT_WARNING
    } else if color == STATUS_SUCCESS {
        LIGHT_SUCCESS_FG
    } else if color == MODE_OPERATE {
        LIGHT_OPERATE
    } else if color == DIFF_ADDED {
        Color::Rgb(22, 101, 52)
    } else if color == USER_BODY {
        LIGHT_USER_BODY
    } else {
        color
    }
}

fn adapt_bg_for_light_palette(color: Color) -> Color {
    if color == WHALE_BG || color == BACKGROUND_DARK {
        LIGHT_SURFACE
    } else if color == WHALE_PANEL
        || color == COMPOSER_BG
        || color == SURFACE_PANEL
        || color == SURFACE_TOOL
    {
        LIGHT_PANEL
    } else if color == SURFACE_ELEVATED || color == SURFACE_TOOL_ACTIVE {
        LIGHT_ELEVATED
    } else if color == SURFACE_REASONING
        || color == SURFACE_REASONING_TINT
        || color == SURFACE_REASONING_ACTIVE
    {
        LIGHT_REASONING
    } else if color == SURFACE_SUCCESS {
        LIGHT_SUCCESS
    } else if color == SURFACE_ERROR {
        LIGHT_ERROR
    } else if color == DIFF_ADDED_BG {
        LIGHT_SUCCESS
    } else if color == DIFF_DELETED_BG {
        LIGHT_ERROR
    } else if color == SELECTION_BG {
        LIGHT_SELECTION_BG
    } else {
        color
    }
}

fn adapt_fg_for_solarized_light_palette(color: Color) -> Color {
    if color == TEXT_BODY || color == SELECTION_TEXT || color == Color::White {
        SOLARIZED_TEXT_BODY
    } else if color == TEXT_SECONDARY || color == TEXT_MUTED {
        SOLARIZED_TEXT_MUTED
    } else if color == TEXT_HINT || color == TEXT_DIM {
        SOLARIZED_TEXT_HINT
    } else if color == TEXT_SOFT || color == TEXT_TOOL_OUTPUT {
        SOLARIZED_TEXT_SOFT
    } else if color == BORDER_COLOR {
        SOLARIZED_BORDER
    } else if color == TEXT_ACCENT || color == ACCENT_TOOL_LIVE {
        SOLARIZED_CYAN
    } else if color == WHALE_ACTION {
        SOLARIZED_BLUE
    } else if color == MODE_AGENT {
        SOLARIZED_LIGHT_UI_THEME.mode_agent
    } else if color == WHALE_HUMAN {
        SOLARIZED_ORANGE
    } else if color == MODE_PLAN {
        SOLARIZED_LIGHT_UI_THEME.mode_plan
    } else if color == STATUS_WARNING || color == TEXT_REASONING || color == ACCENT_REASONING_LIVE {
        SOLARIZED_ORANGE
    } else if color == ACCENT_TOOL_ISSUE || color == WHALE_ERROR || color == STATUS_ERROR {
        SOLARIZED_RED
    } else if color == MODE_YOLO {
        SOLARIZED_LIGHT_UI_THEME.mode_yolo
    } else if color == DIFF_ADDED || color == USER_BODY || color == STATUS_SUCCESS {
        SOLARIZED_GREEN
    } else if color == MODE_OPERATE {
        Color::Rgb(0x6C, 0x71, 0xC4)
    } else {
        color
    }
}

fn adapt_bg_for_solarized_light_palette(color: Color) -> Color {
    if color == WHALE_BG || color == BACKGROUND_DARK {
        SOLARIZED_SURFACE
    } else if color == WHALE_PANEL
        || color == COMPOSER_BG
        || color == SURFACE_PANEL
        || color == SURFACE_TOOL
    {
        SOLARIZED_PANEL
    } else if color == SURFACE_ELEVATED || color == SURFACE_TOOL_ACTIVE {
        SOLARIZED_ELEVATED
    } else if color == SURFACE_REASONING
        || color == SURFACE_REASONING_TINT
        || color == SURFACE_REASONING_ACTIVE
    {
        SOLARIZED_PANEL
    } else if color == SURFACE_SUCCESS || color == DIFF_ADDED_BG {
        SOLARIZED_DIFF_ADDED_BG
    } else if color == SURFACE_ERROR {
        SOLARIZED_ERROR_SURFACE
    } else if color == DIFF_DELETED_BG {
        SOLARIZED_DIFF_DELETED_BG
    } else if color == SELECTION_BG {
        SOLARIZED_SELECT_BG
    } else {
        color
    }
}

// === Community-theme remap ===
//
// The vast majority of render sites in this crate reach for `palette::TEXT_*`,
// `palette::WHALE_BG`, `palette::BORDER_COLOR`, etc. directly rather than
// looking up `app.ui_theme`. To make community theme presets (Catppuccin,
// Tokyo Night, …) actually move the needle visually we intercept colors at
// the backend layer (see `tui::color_compat::ColorCompatBackend`) and remap
// every well-known dark-palette constant to the equivalent UiTheme slot for
// the active preset. For `System`, `Whale`, and `WhaleLight` the remap is a
// no-op — the existing dark/light pipeline handles those.

/// Per-preset green accent used for things that semantically *should* stay
/// green even after theming (diff "+" lines, user-input body). Now delegates
/// to the active UiTheme's diff_added_fg.
#[must_use]
const fn theme_green(ui: &UiTheme) -> Color {
    ui.diff_added_fg
}

/// Per-preset dark-green diff-added background tint.
#[must_use]
const fn theme_diff_added_bg(ui: &UiTheme) -> Color {
    ui.diff_added_bg
}

/// Per-preset dark-red diff-deleted background tint.
#[must_use]
const fn theme_diff_deleted_bg(ui: &UiTheme) -> Color {
    ui.diff_deleted_bg
}

/// Returns `true` if the preset participates in the cell-level remap. The
/// default Whale and System themes pass through unchanged so this whole
/// stage compiles down to a single load+compare on the hot path. Shoreline is
/// listed because it is a full re-ink — warm charcoal instead of the navy the
/// direct terminal constants were tuned for — so every one of those call
/// sites has to land on the preset's slots.
#[inline]
#[must_use]
pub const fn theme_remap_active(theme: ThemeId) -> bool {
    matches!(
        theme,
        ThemeId::Terminal
            | ThemeId::Shoreline
            | ThemeId::ShorelineLight
            | ThemeId::CatppuccinMocha
            | ThemeId::TokyoNight
            | ThemeId::Dracula
            | ThemeId::GruvboxDark
            | ThemeId::Claude
            | ThemeId::Matrix
            | ThemeId::SolarizedLight
            | ThemeId::Uwu
    )
}

/// Remap a foreground color for a community theme preset. Mirrors the
/// structure of [`adapt_fg_for_palette_mode`] — same source set, different
/// destinations sourced from the preset's [`UiTheme`].
///
/// The `ui` argument is the *active* UiTheme as carried on `App` —
/// `ThemeId.ui_theme()` with the user's `background_color` override
/// already applied. Passing it through (rather than re-resolving from
/// `theme` inside this function) preserves that override; otherwise a
/// user combining `background_color = "#..."` with a community theme
/// would see their override silently overwritten by the preset's
/// surface_bg on every cell remap.
#[must_use]
pub fn adapt_fg_for_theme(color: Color, theme: ThemeId, ui: &UiTheme) -> Color {
    if !theme_remap_active(theme) {
        return color;
    }

    if color == TEXT_BODY || color == SELECTION_TEXT || color == Color::White {
        ui.text_body
    } else if color == TEXT_SECONDARY || color == TEXT_MUTED {
        ui.text_muted
    } else if color == TEXT_HINT || color == TEXT_DIM {
        ui.text_hint
    } else if color == TEXT_SOFT || color == TEXT_TOOL_OUTPUT {
        ui.text_soft
    } else if color == BORDER_COLOR {
        ui.border
    } else if color == TEXT_ACCENT || color == ACCENT_TOOL_LIVE {
        ui.status_working
    } else if color == WHALE_ACTION {
        ui.accent_primary
    } else if color == MODE_AGENT {
        ui.mode_agent
    } else if color == WHALE_HUMAN {
        ui.accent_action
    } else if color == MODE_PLAN {
        ui.mode_plan
    } else if color == TEXT_REASONING || color == ACCENT_REASONING_LIVE {
        if theme == ThemeId::Matrix {
            Color::Rgb(0x00, 0x55, 0x00) // #005500
        } else {
            ui.mode_plan
        }
    } else if color == ACCENT_TOOL_ISSUE || color == STATUS_ERROR || color == WHALE_ERROR {
        ui.error_fg
    } else if color == MODE_YOLO {
        ui.mode_yolo
    } else if color == STATUS_WARNING {
        ui.warning
    } else if color == STATUS_SUCCESS {
        ui.success
    } else if color == MODE_OPERATE {
        ui.mode_operate
    } else if color == DIFF_ADDED || color == USER_BODY {
        theme_green(ui)
    } else {
        color
    }
}

/// Remap a background color for a community theme preset. See the
/// `ui` note on [`adapt_fg_for_theme`] — same contract here.
#[must_use]
pub fn adapt_bg_for_theme(color: Color, theme: ThemeId, ui: &UiTheme) -> Color {
    // The field follows the active shell for every preset, not only the
    // remapped ones: the Whale pair leaves `surface_bg` at `Color::Reset` so
    // the terminal owns the ground, and a direct `bg(WHALE_BG)` paint must
    // not lay a navy patch over it. Deepsea repaints Reset cells through the
    // ocean column afterwards; a user `background_color` override lands here
    // too.
    if color == WHALE_BG || color == BACKGROUND_DARK {
        return ui.surface_bg;
    }
    if !theme_remap_active(theme) {
        return color;
    }

    if color == WHALE_PANEL
        || color == COMPOSER_BG
        || color == SURFACE_PANEL
        || color == SURFACE_TOOL
    {
        ui.panel_bg
    } else if color == SURFACE_ELEVATED || color == SURFACE_TOOL_ACTIVE {
        ui.elevated_bg
    } else if color == SURFACE_REASONING
        || color == SURFACE_REASONING_TINT
        || color == SURFACE_REASONING_ACTIVE
    {
        ui.panel_bg
    } else if color == SURFACE_SUCCESS {
        ui.diff_added_bg
    } else if color == SURFACE_ERROR {
        ui.error_surface
    } else if color == SELECTION_BG {
        ui.selection_bg
    } else if color == DIFF_ADDED_BG {
        theme_diff_added_bg(ui)
    } else if color == DIFF_DELETED_BG {
        theme_diff_deleted_bg(ui)
    } else {
        color
    }
}

fn adapt_fg_for_grayscale_palette(color: Color) -> Color {
    if color == Color::Reset {
        return color;
    }
    // Resolved grayscale mode slots are already final palette colors. Keep
    // this branch ahead of the luma buckets so a direct `UiTheme` call site is
    // idempotent instead of being adapted a second time.
    if color == GRAYSCALE_UI_THEME.mode_agent
        || color == GRAYSCALE_UI_THEME.mode_plan
        || color == GRAYSCALE_UI_THEME.mode_operate
        || color == GRAYSCALE_UI_THEME.mode_yolo
    {
        color
    } else if color == MODE_AGENT {
        GRAYSCALE_UI_THEME.mode_agent
    } else if color == MODE_PLAN {
        GRAYSCALE_UI_THEME.mode_plan
    } else if color == MODE_OPERATE {
        GRAYSCALE_UI_THEME.mode_operate
    } else if color == MODE_YOLO {
        GRAYSCALE_UI_THEME.mode_yolo
    } else if color == TEXT_BODY
        || color == SELECTION_TEXT
        || color == LIGHT_TEXT_BODY
        || color == Color::White
        || color == WHALE_ERROR
        || color == STATUS_ERROR
    {
        GRAYSCALE_TEXT_BODY
    } else if color == TEXT_SOFT
        || color == TEXT_TOOL_OUTPUT
        || color == LIGHT_TEXT_SOFT
        || color == TEXT_ACCENT
        || color == WHALE_ACTION
        || color == WHALE_HUMAN
        || color == ACCENT_TOOL_LIVE
        || color == STATUS_SUCCESS
    {
        GRAYSCALE_TEXT_SOFT
    } else if color == TEXT_SECONDARY
        || color == TEXT_MUTED
        || color == LIGHT_TEXT_MUTED
        || color == TEXT_REASONING
        || color == ACCENT_REASONING_LIVE
        || color == STATUS_WARNING
        || color == USER_BODY
        || color == LIGHT_USER_BODY
        || color == DIFF_ADDED
    {
        GRAYSCALE_TEXT_MUTED
    } else if color == TEXT_HINT
        || color == TEXT_DIM
        || color == LIGHT_TEXT_HINT
        || color == BORDER_COLOR
        || color == LIGHT_BORDER
        || color == ACCENT_TOOL_ISSUE
    {
        GRAYSCALE_TEXT_HINT
    } else {
        match color {
            Color::Black => GRAYSCALE_TEXT_BODY,
            Color::Gray | Color::DarkGray => GRAYSCALE_TEXT_HINT,
            Color::Red
            | Color::LightRed
            | Color::Green
            | Color::LightGreen
            | Color::Yellow
            | Color::LightYellow
            | Color::Blue
            | Color::LightBlue
            | Color::Magenta
            | Color::LightMagenta
            | Color::Cyan
            | Color::LightCyan => GRAYSCALE_TEXT_SOFT,
            Color::Rgb(r, g, b) => grayscale_fg_from_luma(luma(r, g, b)),
            Color::Indexed(_) => color,
            _ => color,
        }
    }
}

fn adapt_bg_for_grayscale_palette(color: Color) -> Color {
    // Direct UiTheme paints have already resolved these slots. Bucketing
    // their luminance again collapses selection into panel and raised
    // surfaces into the field, so preserve the authored grayscale ladder.
    if color == Color::Reset
        || color == GRAYSCALE_SURFACE
        || color == GRAYSCALE_PANEL
        || color == GRAYSCALE_ELEVATED
        || color == GRAYSCALE_REASONING
        || color == GRAYSCALE_SELECTION_BG
        || color == GRAYSCALE_SUCCESS
        || color == GRAYSCALE_ERROR
    {
        return color;
    }
    if color == WHALE_BG || color == BACKGROUND_DARK || color == LIGHT_SURFACE {
        GRAYSCALE_SURFACE
    } else if color == WHALE_PANEL
        || color == COMPOSER_BG
        || color == SURFACE_PANEL
        || color == SURFACE_TOOL
        || color == LIGHT_PANEL
    {
        GRAYSCALE_PANEL
    } else if color == SELECTION_BG || color == LIGHT_SELECTION_BG {
        GRAYSCALE_SELECTION_BG
    } else if color == SURFACE_ELEVATED || color == SURFACE_TOOL_ACTIVE || color == LIGHT_ELEVATED {
        GRAYSCALE_ELEVATED
    } else if color == SURFACE_REASONING
        || color == SURFACE_REASONING_TINT
        || color == SURFACE_REASONING_ACTIVE
        || color == LIGHT_REASONING
    {
        GRAYSCALE_REASONING
    } else if color == SURFACE_SUCCESS || color == DIFF_ADDED_BG || color == LIGHT_SUCCESS {
        GRAYSCALE_SUCCESS
    } else if color == SURFACE_ERROR || color == DIFF_DELETED_BG || color == LIGHT_ERROR {
        GRAYSCALE_ERROR
    } else {
        match color {
            Color::Black => GRAYSCALE_SURFACE,
            Color::White | Color::Gray => GRAYSCALE_ELEVATED,
            Color::DarkGray => GRAYSCALE_PANEL,
            Color::Red
            | Color::LightRed
            | Color::Green
            | Color::LightGreen
            | Color::Yellow
            | Color::LightYellow
            | Color::Blue
            | Color::LightBlue
            | Color::Magenta
            | Color::LightMagenta
            | Color::Cyan
            | Color::LightCyan => GRAYSCALE_ELEVATED,
            Color::Rgb(r, g, b) => grayscale_bg_from_luma(luma(r, g, b)),
            Color::Indexed(_) => color,
            _ => color,
        }
    }
}

fn grayscale_fg_from_luma(luma: u8) -> Color {
    match luma {
        0..=95 => GRAYSCALE_TEXT_HINT,
        96..=155 => GRAYSCALE_TEXT_MUTED,
        156..=215 => GRAYSCALE_TEXT_SOFT,
        _ => GRAYSCALE_TEXT_BODY,
    }
}

fn grayscale_bg_from_luma(luma: u8) -> Color {
    match luma {
        0..=28 => GRAYSCALE_SURFACE,
        29..=95 => GRAYSCALE_PANEL,
        96..=185 => GRAYSCALE_ELEVATED,
        _ => GRAYSCALE_REASONING,
    }
}

pub(crate) fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000) as u8
}
// === Color depth + brightness helpers (v0.6.6 UI redesign) ===

/// Terminal color depth, used to gate truecolor surfaces (e.g. reasoning bg
/// tints) on terminals that can't render them faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// Explicit NO_COLOR: terminal-owned foreground/background, with text
    /// modifiers and semantic symbols retained.
    Monochrome,
    /// 16-color terminals (macOS Terminal.app default, dumb tmux setups).
    /// Background tints distort the named-palette mapping, so we drop them.
    Ansi16,
    /// 256-color terminals — RGB→256 fallback is faithful enough.
    Ansi256,
    /// True-color (24-bit) — render the palette verbatim.
    TrueColor,
}

/// Foreground roles that must remain distinct after the terminal reduces the
/// palette. RGB proximity is deliberately irrelevant here: action and Operate,
/// or a human ask and a warning, are different product states even when their
/// source hues happen to share a nearest ANSI color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticForegroundRole {
    Action,
    Live,
    Human,
    Warning,
    Danger,
    Success,
    ModeAgent,
    ModePlan,
    ModeOperate,
    ModeYolo,
}

impl SemanticForegroundRole {
    #[must_use]
    const fn ansi16(self) -> Color {
        match self {
            Self::Action => Color::LightBlue,
            Self::Live => Color::LightCyan,
            Self::Human => Color::LightYellow,
            Self::Warning => Color::Yellow,
            Self::Danger => Color::LightRed,
            Self::Success => Color::LightGreen,
            Self::ModeAgent => Color::Blue,
            Self::ModePlan => Color::Magenta,
            Self::ModeOperate => Color::LightMagenta,
            Self::ModeYolo => Color::Red,
        }
    }
}

fn raw_semantic_foreground_role(color: Color) -> Option<SemanticForegroundRole> {
    if color == MODE_AGENT {
        Some(SemanticForegroundRole::ModeAgent)
    } else if color == MODE_PLAN {
        Some(SemanticForegroundRole::ModePlan)
    } else if color == MODE_OPERATE {
        Some(SemanticForegroundRole::ModeOperate)
    } else if color == MODE_YOLO {
        Some(SemanticForegroundRole::ModeYolo)
    } else if color == WHALE_ACTION {
        Some(SemanticForegroundRole::Action)
    } else if color == WHALE_LIVE || color == TEXT_ACCENT || color == ACCENT_TOOL_LIVE {
        Some(SemanticForegroundRole::Live)
    } else if color == WHALE_HUMAN {
        Some(SemanticForegroundRole::Human)
    } else if color == STATUS_WARNING {
        Some(SemanticForegroundRole::Warning)
    } else if color == WHALE_ERROR || color == STATUS_ERROR || color == ACCENT_TOOL_ISSUE {
        Some(SemanticForegroundRole::Danger)
    } else if color == STATUS_SUCCESS || color == USER_BODY || color == DIFF_ADDED {
        Some(SemanticForegroundRole::Success)
    } else {
        None
    }
}

fn theme_semantic_foreground_role(color: Color, ui: &UiTheme) -> Option<SemanticForegroundRole> {
    // Mode slots come first. Shipped themes keep these source colors distinct
    // from the general semantic lanes so direct `app.ui_theme.mode_*` call
    // sites retain the same identity as raw `MODE_*` call sites.
    if color == ui.mode_agent {
        Some(SemanticForegroundRole::ModeAgent)
    } else if color == ui.mode_plan {
        Some(SemanticForegroundRole::ModePlan)
    } else if color == ui.mode_operate {
        Some(SemanticForegroundRole::ModeOperate)
    } else if color == ui.mode_yolo {
        Some(SemanticForegroundRole::ModeYolo)
    } else if color == ui.accent_primary {
        Some(SemanticForegroundRole::Action)
    } else if color == ui.status_working
        || color == ui.accent_secondary
        || color == ui.tool_running
        // `UiTheme::info` is the sky/worker lane used by ambient and live
        // surfaces. Several shipped themes intentionally alias it to their
        // working color, so it must not precede the live buckets.
        || color == ui.info
    {
        Some(SemanticForegroundRole::Live)
    } else if color == ui.accent_action {
        Some(SemanticForegroundRole::Human)
    } else if color == ui.warning || color == ui.status_warning {
        Some(SemanticForegroundRole::Warning)
    } else if color == ui.error_fg || color == ui.tool_failed || color == ui.diff_deleted_fg {
        Some(SemanticForegroundRole::Danger)
    } else if color == ui.success || color == ui.tool_success || color == ui.diff_added_fg {
        Some(SemanticForegroundRole::Success)
    } else {
        None
    }
}

/// Adapt a resolved foreground to terminal depth while retaining the semantic
/// role carried by the original cell color. Truecolor and ANSI-256 preserve the
/// resolved theme value; ANSI-16 uses a fixed, injective role matrix instead of
/// an arbitrary nearest-color guess.
#[must_use]
pub fn adapt_fg_for_depth(
    source: Color,
    resolved: Color,
    depth: ColorDepth,
    ui: &UiTheme,
) -> Color {
    if depth == ColorDepth::Ansi16
        && let Some(role) = raw_semantic_foreground_role(source)
            .or_else(|| theme_semantic_foreground_role(source, ui))
    {
        role.ansi16()
    } else {
        adapt_color(resolved, depth)
    }
}

impl ColorDepth {
    /// Detect the active terminal's color depth. Honors `COLORTERM`
    /// (truecolor / 24bit) first, then falls back to `TERM`. Defaults to
    /// `TrueColor` because most modern terminals support it; the conservative
    /// fallback is `Ansi16` so background tints disappear safely.
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with(|key| std::env::var_os(key))
    }

    /// Pure decision core over an injected environment reader, so the
    /// `NO_COLOR` contract is testable without mutating process env.
    ///
    /// `NO_COLOR` (no-color.org): present and non-empty ⇒ suppress color.
    /// Monochrome is distinct from a terminal that supports ANSI-16 hues.
    /// Text modifiers and Unicode symbols remain independent preferences.
    #[must_use]
    pub(crate) fn detect_with(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        if let Some(no_color) = get("NO_COLOR")
            && !no_color.is_empty()
        {
            return Self::Monochrome;
        }
        if let Some(ct) = get("COLORTERM") {
            let ct = ct.to_string_lossy().to_ascii_lowercase();
            if ct.contains("truecolor") || ct.contains("24bit") {
                return Self::TrueColor;
            }
        }
        if get("WT_SESSION").is_some() {
            return Self::TrueColor;
        }
        if let Some(term_program) = get("TERM_PROGRAM") {
            let term_program = term_program.to_string_lossy().to_ascii_lowercase();
            if term_program.contains("iterm")
                || term_program.contains("wezterm")
                || term_program.contains("vscode")
                || term_program.contains("warp")
            {
                return Self::TrueColor;
            }
        }
        let term = get("TERM")
            .map(|t| t.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if term.contains("truecolor") || term.contains("24bit") {
            Self::TrueColor
        } else if term.contains("256") {
            Self::Ansi256
        } else if term.is_empty() || term == "dumb" {
            Self::Ansi16
        } else {
            // Unknown TERM strings should not receive 24-bit SGR by default.
            // Older macOS/remote terminals can render truecolor backgrounds as
            // bright cyan blocks; 256-color output is the safer compromise.
            Self::Ansi256
        }
    }
}

/// Adapt a foreground color to the terminal's color depth.
///
/// On TrueColor, `color` passes through. ANSI-256 uses the stable extended
/// palette; ANSI-16 uses a generic nearest named color. Rendered semantic
/// foregrounds must go through [`adapt_fg_for_depth`] so role identity is not
/// inferred from RGB proximity.
#[must_use]
pub fn adapt_color(color: Color, depth: ColorDepth) -> Color {
    match (color, depth) {
        (_, ColorDepth::Monochrome) => Color::Reset,
        (_, ColorDepth::TrueColor) => color,
        (Color::Rgb(r, g, b), ColorDepth::Ansi256) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        (Color::Rgb(r, g, b), ColorDepth::Ansi16) => nearest_ansi16(r, g, b),
        _ => color,
    }
}

/// Adapt a background color. On Ansi16 terminals background tints are noisy,
/// so we drop them to `Color::Reset` rather than attempt a coarse named-color
/// match — a quiet background reads cleaner than a wrong one.
#[must_use]
pub fn adapt_bg(color: Color, depth: ColorDepth) -> Color {
    match (color, depth) {
        (_, ColorDepth::TrueColor) => color,
        (Color::Rgb(r, g, b), ColorDepth::Ansi256) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        (_, ColorDepth::Ansi256) => color,
        (_, ColorDepth::Ansi16 | ColorDepth::Monochrome) => Color::Reset,
    }
}

/// Mix two RGB colors at `alpha` (0.0 = `bg`, 1.0 = `fg`). Anything that's not
/// RGB falls back to `fg` — there's no meaningful alpha blend on a named
/// palette entry.
#[must_use]
pub fn blend(fg: Color, bg: Color, alpha: f32) -> Color {
    let alpha = alpha.clamp(0.0, 1.0);
    match (fg, bg) {
        (Color::Rgb(fr, fg_, fb), Color::Rgb(br, bg_, bb)) => {
            let mix = |a: u8, b: u8| -> u8 {
                let a = f32::from(a);
                let b = f32::from(b);
                (b + (a - b) * alpha).round().clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(fr, br), mix(fg_, bg_), mix(fb, bb))
        }
        _ => fg,
    }
}

/// Return the dedicated reasoning surface tint for terminals that can render
/// background colors faithfully. ANSI-16 terminals disable the tint because
/// the nearest named background is too coarse for this subtle treatment.
#[must_use]
pub fn reasoning_surface_tint(depth: ColorDepth) -> Option<Color> {
    match depth {
        ColorDepth::Ansi16 | ColorDepth::Monochrome => None,
        _ => Some(adapt_bg(SURFACE_REASONING_TINT, depth)),
    }
}

/// Pulse `color` between 30% and 100% brightness on a 2s cycle keyed off
/// `now_ms` (epoch ms). The minimum keeps the glyph readable at trough; the
/// maximum is the source color verbatim. Linear interpolation between them
/// reads as a slow heartbeat.
#[must_use]
pub fn pulse_brightness(color: Color, now_ms: u64) -> Color {
    // 2 s = 2000 ms full cycle; sin gives a smooth 0..1..0 swing.
    let phase = (now_ms % 2000) as f32 / 2000.0;
    let t = (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5; // 0..1
    let alpha = 0.30 + t * 0.70; // 30%..100%
    match color {
        Color::Rgb(r, g, b) => {
            let s = |c: u8| -> u8 { ((f32::from(c)) * alpha).round().clamp(0.0, 255.0) as u8 };
            Color::Rgb(s(r), s(g), s(b))
        }
        other => other,
    }
}

/// Map an RGB triple to its closest ANSI-16 named color. Only used by
/// `adapt_color` on Ansi16 terminals; we lean on hue dominance + lightness so
/// brand colors land on the obviously-related named entry (sky → cyan, blue →
/// blue, red → red, etc.) rather than dithering around grey.
pub(crate) fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    let lum = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    if lum < 24 {
        return Color::Black;
    }
    if r > 220 && g > 220 && b > 220 {
        return Color::White;
    }
    let bright = lum > 144;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max.saturating_sub(min) < 16 {
        return if bright { Color::Gray } else { Color::DarkGray };
    }
    if r >= g && r >= b {
        if g > b + 24 {
            if bright {
                Color::LightYellow
            } else {
                Color::Yellow
            }
        } else if b > r.saturating_sub(24) {
            if bright {
                Color::LightMagenta
            } else {
                Color::Magenta
            }
        } else if bright {
            Color::LightRed
        } else {
            Color::Red
        }
    } else if g >= r && g >= b {
        if b > r + 24 {
            if bright {
                Color::LightCyan
            } else {
                Color::Cyan
            }
        } else if bright {
            Color::LightGreen
        } else {
            Color::Green
        }
    } else if r.saturating_add(48) >= b && r > g + 24 {
        if bright {
            Color::LightMagenta
        } else {
            Color::Magenta
        }
    } else if g.saturating_add(48) >= b && g > r + 24 {
        if bright {
            Color::LightCyan
        } else {
            Color::Cyan
        }
    } else if bright {
        Color::LightBlue
    } else {
        Color::Blue
    }
}

/// Map an RGB color to the nearest xterm 256-color palette index. We use only
/// the stable 6x6x6 cube and grayscale ramp (16..255), not the terminal's
/// user-configurable 0..15 colors.
pub(crate) fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    fn nearest_cube_level(channel: u8) -> usize {
        CUBE_LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| channel.abs_diff(**level))
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn dist_sq(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
        let dr = i32::from(a.0) - i32::from(b.0);
        let dg = i32::from(a.1) - i32::from(b.1);
        let db = i32::from(a.2) - i32::from(b.2);
        (dr * dr + dg * dg + db * db) as u32
    }

    let ri = nearest_cube_level(r);
    let gi = nearest_cube_level(g);
    let bi = nearest_cube_level(b);
    let cube_rgb = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube_index = 16 + (36 * ri) as u8 + (6 * gi) as u8 + bi as u8;

    let avg = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
    let gray_i = if avg <= 8 {
        0
    } else if avg >= 238 {
        23
    } else {
        ((u16::from(avg) - 8 + 5) / 10).min(23) as u8
    };
    let gray = 8 + 10 * gray_i;
    let gray_index = 232 + gray_i;

    if dist_sq((r, g, b), (gray, gray, gray)) < dist_sq((r, g, b), cube_rgb) {
        gray_index
    } else {
        cube_index
    }
}
