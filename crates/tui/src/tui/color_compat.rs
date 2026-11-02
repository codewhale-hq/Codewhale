//! Terminal color compatibility shim.
//!
//! Ratatui's crossterm backend emits truecolor SGR for every `Color::Rgb`
//! cell. That is correct for truecolor terminals, but macOS Terminal.app often
//! advertises only `xterm-256color`; sending `38;2` / `48;2` there can render
//! as stray green/cyan backgrounds. This backend adapts every cell to the
//! detected color depth before handing it to crossterm.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};

use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};

use codewhale_palette::{self as palette, ColorDepth, PaletteMode, ThemeId, UiTheme};

const RENDER_DEBUG_ENV: &str = "CODEWHALE_TUI_DEBUG";
const ASCII_SAFE_ENV: &str = "CODEWHALE_ASCII_SAFE";
const RENDER_DEBUG_SAMPLE_LIMIT: usize = 24;

#[derive(Debug)]
pub(crate) struct ColorCompatBackend<W: Write> {
    inner: CrosstermBackend<W>,
    depth: ColorDepth,
    palette_mode: PaletteMode,
    /// Currently active named theme. `System`/`Whale`/`WhaleLight` make the
    /// theme remap a no-op (those rely on the dark/light pipeline); the
    /// community presets (Catppuccin, Tokyo Night, Dracula, Gruvbox) trigger
    /// a per-cell rewrite of dark-palette constants → preset slots.
    theme_id: ThemeId,
    /// Resolved active `UiTheme`, *including* any user `background_color`
    /// override (`UiTheme::with_background_color`). The cell remap reads
    /// target slots from this struct, not from `theme_id.ui_theme()`, so
    /// `theme = "tokyo-night"` + `background_color = "#000000"` lands as a
    /// pure-black surface instead of being overwritten back to
    /// tokyo-night's `#16161e` by the remap.
    active_ui_theme: UiTheme,
    /// During a resize event the terminal emulator may report stale dimensions
    /// for a brief window (observed on macOS Terminal.app and Windows ConHost).
    /// Forcing the expected size prevents ratatui's internal `autoresize` from
    /// shrinking the viewport back to the stale dimension inside `draw()`.
    forced_size: Option<Size>,
    /// Cached terminal size from `crossterm::terminal::size()`, set after
    /// re-entering alt-screen to avoid stale buffer dimensions on Windows.
    /// Used as the primary fallback in `size()` before falling through to
    /// the live crossterm query.
    terminal_size: Option<Size>,
    /// The last position the cursor was explicitly moved to.
    ///
    /// ratatui-core >= 0.1.1 issues a CPR query inside `Terminal::clear()`
    /// (`backend.get_cursor_position()` → `ESC[6n`) to snapshot and restore
    /// the cursor. With our input event loop already reading stdin, the
    /// reply is consumed as input and the query times out —
    /// ratatui/ratatui#2483, #2640. #2640's workaround for apps with a live
    /// event loop is to answer from tracked state, which this backend does:
    /// `get_cursor_position()` never touches the terminal. Only
    /// `set_cursor_position()` updates the tracker; raw writes that move
    /// the cursor out-of-band leave it behind, but every such path is
    /// followed by a full repaint that repositions the cursor itself.
    tracked_cursor: Position,
    render_debug: Option<RenderDebugLog>,
    ascii_safe: bool,
    /// The terminal's own background, when detection measured one
    /// (`BackgroundSource::Osc11` or a resolvable `COLORFGBG` index). This is
    /// the surface a `Color::Reset` cell is really drawn on, so it is what the
    /// contrast floor reasons against. `None` means "no evidence": an
    /// unpainted cell then keeps the terminal's own ink, and a painted cell is
    /// left alone rather than guessed at.
    detected_background: Option<ratatui::style::Color>,
}

impl<W: Write> ColorCompatBackend<W> {
    pub(crate) fn new(writer: W, depth: ColorDepth, palette_mode: PaletteMode) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            depth,
            palette_mode,
            theme_id: ThemeId::System,
            // Default to whatever System resolves to right now — it stays a
            // no-op for the remap since `theme_id` is also System, so this
            // initial value only matters once `set_theme` flips both fields
            // to a community preset.
            active_ui_theme: UiTheme::detect(),
            forced_size: None,
            terminal_size: None,
            tracked_cursor: Position::ORIGIN,
            render_debug: RenderDebugLog::from_env(),
            ascii_safe: ascii_safe_enabled(),
            detected_background: None,
        }
    }

    /// Build a second backend over `writer`, carrying every terminal fact this
    /// one already negotiated (colour depth, palette, theme, measured
    /// background, cached size).
    ///
    /// The screen-mode switch needs this: stock ratatui cannot change an
    /// existing `Terminal`'s viewport, so `/inline` and `/fullscreen` rebuild
    /// the terminal — and the rebuilt one must not re-detect a palette or
    /// forget an in-session `/theme` choice.
    pub(crate) fn respawn<W2: Write>(&self, writer: W2) -> ColorCompatBackend<W2> {
        ColorCompatBackend {
            inner: CrosstermBackend::new(writer),
            depth: self.depth,
            palette_mode: self.palette_mode,
            theme_id: self.theme_id,
            active_ui_theme: self.active_ui_theme,
            // Deliberately not carried: a size forced for one resize frame is
            // scoped to that frame, and the new terminal measures its own.
            forced_size: None,
            terminal_size: self.terminal_size,
            tracked_cursor: self.tracked_cursor,
            render_debug: RenderDebugLog::from_env(),
            ascii_safe: self.ascii_safe,
            detected_background: self.detected_background,
        }
    }

    /// Immutable frame input from this backend's already negotiated facts.
    /// Unknown terminal background stays unknown; an explicitly painted pane
    /// may separately establish its own known ground at the finishing boundary.
    pub(crate) fn native_ocean_caps(&self) -> codewhale_ratatui::Caps {
        use codewhale_ratatui::{Caps, color::ColorDepth as NativeDepth, detect::Appearance};
        Caps {
            depth: match self.depth {
                ColorDepth::Monochrome => NativeDepth::Monochrome,
                ColorDepth::Ansi16 => NativeDepth::Ansi16,
                ColorDepth::Ansi256 => NativeDepth::Ansi256,
                ColorDepth::TrueColor => NativeDepth::TrueColor,
            },
            ascii: self.ascii_safe,
            appearance: self
                .detected_background
                .and_then(codewhale_ratatui::detect::appearance_for_background)
                .unwrap_or(Appearance::Unknown),
        }
    }

    /// Record the measured terminal background. See the field docs.
    pub(crate) fn set_detected_background(&mut self, color: Option<ratatui::style::Color>) {
        self.detected_background = color;
    }

    pub(crate) fn force_size(&mut self, size: Size) {
        self.forced_size = Some(size);
    }

    pub(crate) fn clear_forced_size(&mut self) {
        self.forced_size = None;
    }

    pub(crate) fn set_terminal_size(&mut self, size: Size) {
        self.terminal_size = Some(size);
    }

    pub(crate) fn set_palette_mode(&mut self, palette_mode: PaletteMode) {
        self.palette_mode = palette_mode;
    }

    pub(crate) fn set_theme(&mut self, theme_id: ThemeId, ui_theme: UiTheme) {
        self.theme_id = theme_id;
        self.active_ui_theme = ui_theme;
    }
}

impl<W: Write> Write for ColorCompatBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl<W: Write> Backend for ColorCompatBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let adapted = content
            .map(|(x, y, cell)| {
                let mut cell = cell.clone();
                adapt_cell_colors(
                    &mut cell,
                    self.depth,
                    self.palette_mode,
                    self.theme_id,
                    &self.active_ui_theme,
                    self.detected_background,
                );
                if self.ascii_safe {
                    adapt_cell_symbol_for_ascii(&mut cell);
                }
                (x, y, cell)
            })
            .collect::<Vec<_>>();
        let viewport = if self.render_debug.is_some() {
            self.size().ok()
        } else {
            None
        };
        if let Some(render_debug) = &mut self.render_debug {
            render_debug.record(viewport, &adapted);
        }
        // #3029: Emit OSC 8 hyperlinks out-of-band through the backend's
        // Write impl.  ratatui's buffer pipeline strips ESC bytes, so the
        // open/close sequences must be interleaved with the cell stream
        // here.  OSC 8 is stateful and last-writer-wins: every cell painted
        // between an open and the next close links to that open's target,
        // so each region's cells must be bracketed by their OWN open/close
        // pair — never batched.
        let mut frame_links = codewhale_tui_kit::osc8::take_frame_links();
        if frame_links.is_empty() || !codewhale_tui_kit::osc8::enabled() {
            self.inner
                .draw(adapted.iter().map(|(x, y, cell)| (*x, *y, cell)))?;
            return Ok(());
        }
        // Deterministic region lookup when regions are adjacent/overlapping:
        // the first (top-left-most) region wins.
        frame_links.sort_unstable_by_key(|link| (link.row, link.col_start));
        let region_for = |x: u16, y: u16| -> Option<usize> {
            frame_links
                .iter()
                .position(|link| y == link.row && x >= link.col_start && x <= link.col_end)
        };

        // Walk the diff in its original order and split it into runs at
        // region boundaries, so the visible byte stream stays identical to
        // a no-link render apart from the inserted OSC 8 sequences.
        let mut idx = 0;
        while idx < adapted.len() {
            let current_region = region_for(adapted[idx].0, adapted[idx].1);
            let run_start = idx;
            while idx < adapted.len()
                && region_for(adapted[idx].0, adapted[idx].1) == current_region
            {
                idx += 1;
            }
            let run = &adapted[run_start..idx];
            if let Some(region_idx) = current_region {
                codewhale_tui_kit::osc8::write_osc8_open(self, &frame_links[region_idx].target)?;
                self.inner
                    .draw(run.iter().map(|(x, y, cell)| (*x, *y, cell)))?;
                codewhale_tui_kit::osc8::write_osc8_close(self)?;
            } else {
                self.inner
                    .draw(run.iter().map(|(x, y, cell)| (*x, *y, cell)))?;
            }
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // Answer from tracked state instead of issuing a CPR query that
        // races the input event loop — see `tracked_cursor`.
        Ok(self.tracked_cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.tracked_cursor = position;
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        // forced_size takes priority: it is set during resize events to prevent
        // ratatui's autoresize from shrinking the viewport back to a stale
        // dimension. terminal_size is the cached real terminal size used as a
        // fallback after alt-screen re-entry (Windows buffer width workaround).
        if let Some(size) = self.forced_size.or(self.terminal_size) {
            return Ok(size);
        }
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[derive(Debug)]
struct RenderDebugLog {
    file: File,
    frame: u64,
}

impl RenderDebugLog {
    fn from_env() -> Option<Self> {
        if !render_debug_enabled_from_value(std::env::var(RENDER_DEBUG_ENV).ok().as_deref()) {
            return None;
        }

        let log_dir = codewhale_runtime::runtime_log::log_directory()?;
        if let Err(err) = fs::create_dir_all(&log_dir) {
            tracing::debug!(?err, "failed to create TUI render debug log directory");
            return None;
        }
        let path = log_dir.join("tui-render.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| {
                tracing::debug!(?err, path = %path.display(), "failed to open TUI render debug log");
                err
            })
            .ok()?;

        Some(Self { file, frame: 0 })
    }

    fn record(&mut self, viewport: Option<Size>, diff: &[(u16, u16, Cell)]) {
        self.frame = self.frame.saturating_add(1);
        let sample = diff
            .iter()
            .take(RENDER_DEBUG_SAMPLE_LIMIT)
            .map(|(x, y, _)| (*x, *y))
            .collect::<Vec<_>>();
        let line = render_debug_line(self.frame, viewport, diff.len(), &sample);
        let _ = self.file.write_all(line.as_bytes());
    }
}

fn render_debug_enabled_from_value(value: Option<&str>) -> bool {
    env_flag_enabled(value)
}

fn env_flag_enabled(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Whether terminal chrome must use portable ASCII spellings. Text producers
/// that would otherwise compose multi-cell Unicode labels share this decision
/// with the backend's single-cell glyph adapter.
#[must_use]
pub(crate) fn ascii_safe_enabled() -> bool {
    env_flag_enabled(std::env::var(ASCII_SAFE_ENV).ok().as_deref())
}

/// Narrow every CodeWhale-authored decorative glyph to a semantic ASCII
/// alternative. Scope is deliberate: box drawing, block elements (whale
/// mark, meters, rails), braille state markers, geometric role/state marks,
/// arrows, and typographic chrome. Language text — CJK labels, accented
/// letters, user and model content outside those decorative classes —
/// passes through untouched.
pub(crate) fn adapt_cell_symbol_for_ascii(cell: &mut Cell) {
    // Braille: preserve the rising-fill signal instead of collapsing every
    // working/verifying frame to one glyph.
    let mut chars = cell.symbol().chars();
    if let (Some(ch), None) = (chars.next(), chars.next())
        && let Some(replacement) = codewhale_tui_kit::glyphs::braille_ascii_fallback(ch)
    {
        cell.set_symbol(replacement);
        return;
    }
    if let Some(replacement) = codewhale_tui_kit::glyphs::ascii_fallback(cell.symbol()) {
        cell.set_symbol(replacement);
    }
}

fn render_debug_line(
    frame: u64,
    viewport: Option<Size>,
    diff_cells: usize,
    sample: &[(u16, u16)],
) -> String {
    let mut line = String::new();
    match viewport {
        Some(size) => {
            let _ = write!(
                &mut line,
                "frame={frame} size={}x{} diff_cells={diff_cells} sample=",
                size.width, size.height
            );
        }
        None => {
            let _ = write!(
                &mut line,
                "frame={frame} size=unknown diff_cells={diff_cells} sample="
            );
        }
    }
    for (index, (x, y)) in sample.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        let _ = write!(&mut line, "{x}:{y}");
    }
    line.push('\n');
    line
}

/// Apply the WCAG contrast floor to a cell that is about to be drawn.
///
/// This runs *after* the palette-mode remap and *before* depth downsampling,
/// because the floor has to reason about the color the user will actually see
/// while it is still full-precision RGB.
///
/// Unset text ink on painted ground uses the active theme body color.
///
/// - Explicit preset colors (`theme_remap_active`: Terminal,
///   Catppuccin, Matrix, …) are exempt. Their authors tuned those pairs, some
///   deliberately below 4.5:1, and a user who typed `/theme matrix` asked for
///   that. The floor guards the auto-detected default path.
/// - Only text cells are clamped; frame chrome keeps its intended weight.
///   See [`palette::symbol_needs_text_contrast`].
///
/// A third case has no surface to measure against: an unpainted cell on a
/// terminal that reported no background keeps the terminal's own ink, because
/// that is the one colour guaranteed to read on the ground the terminal drew.
fn enforce_cell_contrast(
    cell: &mut Cell,
    depth: ColorDepth,
    theme_id: ThemeId,
    ui_theme: &UiTheme,
    detected_background: Option<ratatui::style::Color>,
) {
    if !palette::symbol_needs_text_contrast(cell.symbol()) {
        return;
    }
    if matches!(depth, ColorDepth::TrueColor | ColorDepth::Ansi256)
        && cell.fg == ratatui::style::Color::Reset
        && !cell.modifier.contains(ratatui::style::Modifier::REVERSED)
        && palette::resolvable_rgb(cell.bg).is_some()
    {
        cell.fg = if palette::theme_remap_active(theme_id) {
            ui_theme.text_body
        } else {
            palette::enforce_contrast(ui_theme.text_body, cell.bg, palette::AA_BODY_CONTRAST)
        };
        return;
    }
    if palette::theme_remap_active(theme_id) {
        return;
    }
    let Some(surface) = palette::effective_surface(cell.bg, detected_background) else {
        if cell.bg == ratatui::style::Color::Reset && cell.fg == palette::TEXT_BODY {
            cell.fg = ratatui::style::Color::Reset;
        }
        return;
    };
    cell.fg = palette::enforce_contrast(cell.fg, surface, palette::AA_BODY_CONTRAST);
}

pub(crate) fn adapt_cell_colors(
    cell: &mut Cell,
    depth: ColorDepth,
    palette_mode: PaletteMode,
    theme_id: ThemeId,
    ui_theme: &UiTheme,
    detected_background: Option<ratatui::style::Color>,
) {
    let source_fg = cell.fg;
    // Stage 1: community-theme remap (dark palette → preset slots). No-op
    // for System / Whale / WhaleLight so legacy dark/light flows are
    // untouched. Runs *before* the palette-mode remap so a light terminal
    // running e.g. Catppuccin still routes the preset colors through the
    // light adaptation below (rare combo, but the sequencing is the same).
    cell.fg = palette::adapt_fg_for_theme(cell.fg, theme_id, ui_theme);
    cell.bg = palette::adapt_bg_for_theme(cell.bg, theme_id, ui_theme);
    // Stage 2: legacy dark↔light remap.
    let original_bg = cell.bg;
    cell.fg = palette::adapt_fg_for_palette_mode(cell.fg, original_bg, palette_mode);
    cell.bg = palette::adapt_bg_for_palette_mode(cell.bg, palette_mode);
    // Stage 2.5: contrast floor. Stages 1 and 2 are equality whitelists — a
    // token nobody listed reaches here unadapted, which is exactly how
    // near-white body text ends up on a near-white terminal (#4833). This
    // stage is membership-independent: it looks at the pair that will be
    // rendered and lifts it if the numbers fail.
    enforce_cell_contrast(cell, depth, theme_id, ui_theme, detected_background);
    // Stage 3: depth (truecolor / 256 / 16) downsampling.
    cell.fg = palette::adapt_fg_for_depth(source_fg, cell.fg, depth, ui_theme);
    cell.bg = palette::adapt_bg(cell.bg, depth);
    if depth == ColorDepth::Monochrome {
        cell.underline_color = ratatui::style::Color::Reset;
    }
}

/// Exact backend-visible proposed ink, reused by native Ocean's guard.
pub(crate) fn project_ocean_ink(
    cell: &Cell,
    water: ratatui::style::Color,
    depth: codewhale_ratatui::color::ColorDepth,
    theme: &UiTheme,
) -> ratatui::style::Color {
    use codewhale_ratatui::color::ColorDepth as NativeDepth;
    let depth = match depth {
        NativeDepth::Monochrome => ColorDepth::Monochrome,
        NativeDepth::Ansi16 => ColorDepth::Ansi16,
        NativeDepth::Ansi256 => ColorDepth::Ansi256,
        NativeDepth::TrueColor => ColorDepth::TrueColor,
    };
    let mut proposed = cell.clone();
    proposed.set_bg(water);
    adapt_cell_colors(
        &mut proposed,
        depth,
        theme.mode,
        ThemeId::Underwater,
        theme,
        None,
    );
    proposed.fg
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, env, ffi::OsString, fs, io::Write, rc::Rc};

    use ratatui::backend::Backend;
    use ratatui::{buffer::Cell, style::Color};

    use super::*;
    use crate::test_support::lock_test_env;

    #[derive(Clone, Default)]
    struct SharedWriter(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct EnvRestore {
        key: &'static str,
        value: Option<OsString>,
    }

    impl EnvRestore {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                value: env::var_os(key),
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            // SAFETY: environment mutation is serialized by lock_test_env.
            unsafe {
                match &self.value {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn adapts_rgb_cells_to_indexed_on_ansi256() {
        let mut cell = Cell::default();
        cell.set_fg(Color::Rgb(53, 120, 229));
        cell.set_bg(Color::Rgb(11, 21, 38));

        adapt_cell_colors(
            &mut cell,
            ColorDepth::Ansi256,
            PaletteMode::Dark,
            ThemeId::System,
            &palette::UI_THEME,
            None,
        );

        assert!(matches!(cell.fg, Color::Indexed(_)));
        assert!(matches!(cell.bg, Color::Indexed(_)));
    }

    #[test]
    fn leaves_truecolor_cells_unchanged() {
        let mut cell = Cell::default();
        cell.set_fg(Color::Rgb(53, 120, 229));
        cell.set_bg(Color::Rgb(11, 21, 38));

        adapt_cell_colors(
            &mut cell,
            ColorDepth::TrueColor,
            PaletteMode::Dark,
            ThemeId::System,
            &palette::UI_THEME,
            None,
        );

        assert_eq!(cell.fg, Color::Rgb(53, 120, 229));
        assert_eq!(cell.bg, Color::Rgb(11, 21, 38));
    }

    #[test]
    fn ascii_safe_symbol_adapter_preserves_meaning_with_narrow_glyphs() {
        for (rich, safe) in [
            ("─", "-"),
            ("│", "|"),
            ("┌", "+"),
            ("▶", ">"),
            ("▷", ">"),
            ("▼", "v"),
            ("✓", "Y"),
            ("✕", "X"),
        ] {
            let mut cell = Cell::default();
            cell.set_symbol(rich);
            adapt_cell_symbol_for_ascii(&mut cell);
            assert_eq!(cell.symbol(), safe, "{rich} should map to {safe}");
            assert!(cell.symbol().is_ascii());
        }
    }

    #[test]
    fn monochrome_backend_suppresses_every_color_but_keeps_text_modifiers() {
        use ratatui::style::{Modifier, Style};

        let sgr = regex::Regex::new(r"\x1b\[([0-9;:]*)m").unwrap();
        for theme_id in palette::SELECTABLE_THEMES {
            let theme = theme_id.ui_theme();
            let writer = SharedWriter::default();
            let capture = writer.0.clone();
            let mut backend =
                ColorCompatBackend::new(writer.clone(), ColorDepth::Monochrome, theme.mode);
            backend.set_theme(*theme_id, theme);
            let modifiers = Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED;
            let mut cell = Cell::default();
            cell.set_symbol("x").set_style(
                Style::default()
                    .fg(theme.accent_primary)
                    .bg(Color::Indexed(4))
                    .underline_color(Color::Red)
                    .add_modifier(modifiers),
            );
            let mut adapted = cell.clone();
            adapt_cell_colors(
                &mut adapted,
                ColorDepth::Monochrome,
                theme.mode,
                *theme_id,
                &theme,
                None,
            );
            assert_eq!(
                (adapted.fg, adapted.bg, adapted.underline_color),
                (Color::Reset, Color::Reset, Color::Reset)
            );
            assert_eq!(adapted.modifier, modifiers);

            // Screen-mode switches must carry the same color policy.
            let mut backend = backend.respawn(writer);
            backend.draw(std::iter::once((0, 0, &cell))).unwrap();
            let output = String::from_utf8_lossy(&capture.borrow()).to_string();
            assert!(output.contains('x'), "{theme_id:?}: {output:?}");
            for codes in sgr.captures_iter(&output) {
                for code in codes[1]
                    .split([';', ':'])
                    .filter_map(|code| code.parse::<u16>().ok())
                {
                    assert!(
                        !matches!(code, 30..=38 | 40..=48 | 58 | 90..=97 | 100..=107),
                        "{theme_id:?} emitted color SGR: {output:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ansi256_backend_output_does_not_emit_truecolor_sgr() {
        let writer = SharedWriter::default();
        let capture = writer.0.clone();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::Ansi256, PaletteMode::Dark);
        let mut cell = Cell::default();
        cell.set_symbol("x")
            .set_fg(Color::Rgb(53, 120, 229))
            .set_bg(Color::Rgb(11, 21, 38));

        backend.draw(std::iter::once((0, 0, &cell))).unwrap();

        let output = String::from_utf8_lossy(&capture.borrow()).to_string();
        assert!(!output.contains("38;2;"), "{output:?}");
        assert!(!output.contains("48;2;"), "{output:?}");
    }

    #[test]
    fn light_palette_maps_dark_cells_before_depth_adaptation() {
        let mut cell = Cell::default();
        cell.set_fg(Color::White);
        cell.set_bg(palette::WHALE_BG);

        adapt_cell_colors(
            &mut cell,
            ColorDepth::TrueColor,
            PaletteMode::Light,
            ThemeId::WhaleLight,
            &palette::LIGHT_UI_THEME,
            None,
        );

        assert_eq!(cell.fg, palette::LIGHT_TEXT_BODY);
        // The whale pair's shell is terminal-owned: a direct WHALE_BG paint
        // follows LIGHT_UI_THEME.surface_bg (Reset), not a painted surface.
        assert_eq!(cell.bg, Color::Reset);
    }

    #[test]
    fn grayscale_palette_maps_hued_cells_before_depth_adaptation() {
        let mut cell = Cell::default();
        cell.set_fg(palette::WHALE_ACTION);
        cell.set_bg(palette::WHALE_BG);

        adapt_cell_colors(
            &mut cell,
            ColorDepth::TrueColor,
            PaletteMode::Grayscale,
            ThemeId::Grayscale,
            &palette::GRAYSCALE_UI_THEME,
            None,
        );

        assert_eq!(cell.fg, palette::GRAYSCALE_TEXT_SOFT);
        assert_eq!(cell.bg, palette::GRAYSCALE_SURFACE);
    }

    #[test]
    fn community_theme_remap_honors_background_color_override() {
        // Tokyo Night + a custom black surface: the remap must rewrite
        // `palette::WHALE_BG` to the *active* UiTheme's overridden
        // surface, not to tokyo-night's default surface.
        let active = palette::TOKYO_NIGHT_UI_THEME.with_background_color(Color::Rgb(0, 0, 0));
        let mut cell = Cell::default();
        cell.set_bg(palette::WHALE_BG);

        adapt_cell_colors(
            &mut cell,
            ColorDepth::TrueColor,
            PaletteMode::Dark,
            ThemeId::TokyoNight,
            &active,
            None,
        );

        assert_eq!(cell.bg, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn terminal_and_matrix_cells_keep_effective_mode_colors() {
        for (theme_id, theme) in [
            (ThemeId::Terminal, palette::TERMINAL_UI_THEME),
            (ThemeId::Matrix, palette::MATRIX_UI_THEME),
        ] {
            for (source, expected, role) in [
                (palette::MODE_AGENT, theme.mode_agent, "agent"),
                (palette::MODE_PLAN, theme.mode_plan, "plan"),
                (palette::MODE_OPERATE, theme.mode_operate, "operate"),
                (palette::MODE_YOLO, theme.mode_yolo, "full access"),
            ] {
                let mut cell = Cell::default();
                cell.set_fg(source);
                adapt_cell_colors(
                    &mut cell,
                    ColorDepth::TrueColor,
                    theme.mode,
                    theme_id,
                    &theme,
                    None,
                );
                assert_eq!(
                    cell.fg,
                    expected,
                    "theme '{}' rendered the {role} token through the wrong slot",
                    theme_id.name(),
                );
            }
        }
    }

    fn rendered_foreground(
        source: Color,
        depth: ColorDepth,
        theme_id: ThemeId,
        theme: &UiTheme,
    ) -> Color {
        let mut cell = Cell::default();
        cell.set_fg(source);
        adapt_cell_colors(&mut cell, depth, theme.mode, theme_id, theme, None);
        cell.fg
    }

    #[test]
    fn grayscale_modes_are_identity_safe_for_raw_and_direct_cells() {
        let theme = palette::GRAYSCALE_UI_THEME;
        let roles = [
            ("act", palette::MODE_AGENT, theme.mode_agent, Color::Blue),
            ("plan", palette::MODE_PLAN, theme.mode_plan, Color::Magenta),
            (
                "operate",
                palette::MODE_OPERATE,
                theme.mode_operate,
                Color::LightMagenta,
            ),
            (
                "full access",
                palette::MODE_YOLO,
                theme.mode_yolo,
                Color::Red,
            ),
        ];

        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ] {
            let mut outputs = Vec::new();
            for (name, raw, direct, ansi16) in roles {
                let expected = if depth == ColorDepth::Ansi16 {
                    ansi16
                } else {
                    palette::adapt_color(direct, depth)
                };
                let raw_output = rendered_foreground(raw, depth, ThemeId::Grayscale, &theme);
                let direct_output = rendered_foreground(direct, depth, ThemeId::Grayscale, &theme);
                assert_eq!(raw_output, expected, "raw {name} at {depth:?}");
                assert_eq!(direct_output, expected, "direct {name} at {depth:?}");
                outputs.push((name, raw_output));
            }
            for (index, (left_name, left)) in outputs.iter().enumerate() {
                for (right_name, right) in outputs.iter().skip(index + 1) {
                    assert_ne!(
                        left, right,
                        "grayscale {depth:?} merged {left_name} and {right_name}"
                    );
                }
            }
        }
    }

    #[test]
    fn ansi16_uses_complete_semantic_role_matrix_for_whale_dark_and_light() {
        let expected = [
            ("action", Color::LightBlue),
            ("live", Color::LightCyan),
            ("human", Color::LightYellow),
            ("warning", Color::Yellow),
            ("danger", Color::LightRed),
            ("success", Color::LightGreen),
            ("act mode", Color::Blue),
            ("plan mode", Color::Magenta),
            ("operate mode", Color::LightMagenta),
            ("full-access mode", Color::Red),
        ];
        let raw = [
            palette::WHALE_ACTION,
            palette::WHALE_LIVE,
            palette::WHALE_HUMAN,
            palette::STATUS_WARNING,
            palette::WHALE_ERROR,
            palette::STATUS_SUCCESS,
            palette::MODE_AGENT,
            palette::MODE_PLAN,
            palette::MODE_OPERATE,
            palette::MODE_YOLO,
        ];

        for (theme_id, theme) in [
            (ThemeId::Whale, palette::UI_THEME),
            (ThemeId::WhaleLight, palette::LIGHT_UI_THEME),
        ] {
            let direct = [
                theme.accent_primary,
                theme.status_working,
                theme.accent_action,
                theme.warning,
                theme.error_fg,
                theme.success,
                theme.mode_agent,
                theme.mode_plan,
                theme.mode_operate,
                theme.mode_yolo,
            ];
            for (source_kind, sources) in [("raw", raw), ("direct", direct)] {
                let outputs = sources
                    .into_iter()
                    .zip(expected)
                    .map(|(source, (name, expected_color))| {
                        let output =
                            rendered_foreground(source, ColorDepth::Ansi16, theme_id, &theme);
                        assert_eq!(
                            output,
                            expected_color,
                            "{} {source_kind} {name}",
                            theme_id.name(),
                        );
                        (name, output)
                    })
                    .collect::<Vec<_>>();
                for (index, (left_name, left)) in outputs.iter().enumerate() {
                    for (right_name, right) in outputs.iter().skip(index + 1) {
                        assert_ne!(
                            left,
                            right,
                            "{} {source_kind} matrix merged {left_name} and {right_name}",
                            theme_id.name(),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn backend_palette_mode_can_follow_runtime_theme_changes() {
        let writer = SharedWriter::default();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);

        assert_eq!(backend.palette_mode, PaletteMode::Dark);
        backend.set_palette_mode(PaletteMode::Light);
        assert_eq!(backend.palette_mode, PaletteMode::Light);
        backend.set_palette_mode(PaletteMode::Grayscale);
        assert_eq!(backend.palette_mode, PaletteMode::Grayscale);
    }

    #[test]
    fn render_debug_env_parser_accepts_truthy_values_only() {
        assert!(!render_debug_enabled_from_value(None));
        assert!(!render_debug_enabled_from_value(Some("")));
        assert!(!render_debug_enabled_from_value(Some("0")));
        assert!(!render_debug_enabled_from_value(Some("false")));
        assert!(render_debug_enabled_from_value(Some("1")));
        assert!(render_debug_enabled_from_value(Some("true")));
        assert!(render_debug_enabled_from_value(Some("YES")));
        assert!(render_debug_enabled_from_value(Some("on")));
    }

    #[test]
    fn render_debug_line_records_frame_size_and_diff_sample() {
        let line = render_debug_line(7, Some(Size::new(80, 24)), 42, &[(0, 0), (12, 3), (79, 23)]);

        assert_eq!(
            line,
            "frame=7 size=80x24 diff_cells=42 sample=0:0,12:3,79:23\n"
        );
    }

    #[test]
    fn backend_writes_render_debug_log_when_enabled() {
        let _lock = lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvRestore::capture("HOME");
        let _userprofile = EnvRestore::capture("USERPROFILE");
        let _debug = EnvRestore::capture(RENDER_DEBUG_ENV);

        // SAFETY: environment mutation is serialized by lock_test_env.
        unsafe {
            env::set_var("HOME", tmp.path());
            env::set_var("USERPROFILE", "");
            env::set_var(RENDER_DEBUG_ENV, "1");
        }

        let writer = SharedWriter::default();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);
        let mut cell = Cell::default();
        cell.set_symbol("x");
        backend.draw(std::iter::once((3, 4, &cell))).unwrap();

        let log_path = tmp
            .path()
            .join(".codewhale")
            .join("logs")
            .join("tui-render.log");
        let body = fs::read_to_string(log_path).expect("render debug log");
        assert!(body.contains("frame=1"), "{body}");
        assert!(body.contains("diff_cells=1"), "{body}");
        assert!(body.contains("sample=3:4"), "{body}");
    }

    #[test]
    fn size_returns_terminal_size_when_set() {
        let writer = SharedWriter::default();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);

        backend.set_terminal_size(Size::new(120, 40));
        assert_eq!(backend.size().unwrap(), Size::new(120, 40));
    }

    #[test]
    fn forced_size_takes_priority_over_terminal_size() {
        let writer = SharedWriter::default();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);

        // forced_size is set during resize events to temporarily override the
        // cached terminal_size — it must win to prevent viewport shrinking.
        backend.set_terminal_size(Size::new(120, 40));
        backend.force_size(Size::new(80, 25));
        assert_eq!(backend.size().unwrap(), Size::new(80, 25));
    }

    #[test]
    fn size_falls_back_to_forced_size_when_terminal_size_unset() {
        let writer = SharedWriter::default();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);

        backend.force_size(Size::new(80, 25));
        assert_eq!(backend.size().unwrap(), Size::new(80, 25));
    }

    // ── #3029: OSC 8 emission through the backend byte stream ──────────────

    fn row_cells(symbols: &str) -> Vec<(u16, u16, Cell)> {
        symbols
            .chars()
            .enumerate()
            .map(|(i, ch)| {
                let mut cell = Cell::default();
                cell.set_symbol(&ch.to_string());
                (u16::try_from(i).unwrap(), 0u16, cell)
            })
            .collect()
    }

    #[test]
    fn osc8_open_close_bracket_only_their_region_cells() {
        use codewhale_tui_kit::osc8::LinkRegion;

        // Baseline: identical cells, no link regions.
        let baseline_writer = SharedWriter::default();
        let baseline_capture = baseline_writer.0.clone();
        let mut baseline =
            ColorCompatBackend::new(baseline_writer, ColorDepth::TrueColor, PaletteMode::Dark);
        let cells = row_cells("ABCDE");
        baseline
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();
        let baseline_out = String::from_utf8_lossy(&baseline_capture.borrow()).to_string();

        // Linked render: columns 2..=3 ("CD") carry one link region.
        codewhale_tui_kit::osc8::set_frame_links(vec![LinkRegion {
            row: 0,
            col_start: 2,
            col_end: 3,
            target: "https://example.test/1".to_string(),
        }]);
        let writer = SharedWriter::default();
        let capture = writer.0.clone();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);
        let cells = row_cells("ABCDE");
        backend
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();
        let out = String::from_utf8_lossy(&capture.borrow()).to_string();

        let open = "\x1b]8;;https://example.test/1\x1b\\";
        let close = "\x1b]8;;\x1b\\";
        assert_eq!(out.matches(open).count(), 1, "exactly one open: {out:?}");
        assert_eq!(out.matches(close).count(), 1, "exactly one close: {out:?}");

        // The open must precede the first linked glyph and the close must sit
        // between the last linked glyph and the first glyph after the region.
        let open_at = out.find(open).expect("open present");
        let close_at = out.find(close).expect("close present");
        let c_at = out.find('C').expect("glyph C");
        let d_at = out.find('D').expect("glyph D");
        let e_at = out.find('E').expect("glyph E");
        assert!(open_at < c_at, "open before linked cells: {out:?}");
        assert!(d_at < close_at, "close after linked cells: {out:?}");
        assert!(
            close_at < e_at,
            "cells after the region must not inherit the link: {out:?}"
        );

        // Visible glyph stream is unchanged by link insertion.
        let mut baseline_visible = String::new();
        codewhale_tui_kit::osc8::strip_ansi_into(&baseline_out, &mut baseline_visible);
        let mut linked_visible = String::new();
        codewhale_tui_kit::osc8::strip_ansi_into(&out, &mut linked_visible);
        assert_eq!(
            baseline_visible, linked_visible,
            "link emission must not move or alter visible cells"
        );
    }

    #[test]
    fn osc8_two_regions_link_to_their_own_targets() {
        use codewhale_tui_kit::osc8::LinkRegion;

        codewhale_tui_kit::osc8::set_frame_links(vec![
            LinkRegion {
                row: 0,
                col_start: 0,
                col_end: 1,
                target: "https://example.test/first".to_string(),
            },
            LinkRegion {
                row: 0,
                col_start: 3,
                col_end: 4,
                target: "https://example.test/second".to_string(),
            },
        ]);
        let writer = SharedWriter::default();
        let capture = writer.0.clone();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);
        let cells = row_cells("ABZCD");
        backend
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();
        let out = String::from_utf8_lossy(&capture.borrow()).to_string();

        let first = "\x1b]8;;https://example.test/first\x1b\\";
        let second = "\x1b]8;;https://example.test/second\x1b\\";
        let close = "\x1b]8;;\x1b\\";
        assert_eq!(out.matches(first).count(), 1, "{out:?}");
        assert_eq!(out.matches(second).count(), 1, "{out:?}");
        assert_eq!(out.matches(close).count(), 2, "{out:?}");

        // Pre-#3029-audit bug: both opens were emitted before any cell, so
        // the whole frame linked to the LAST region's target. Each region's
        // open must close before the next region's open begins.
        let first_at = out.find(first).expect("first open");
        let first_close_at = out[first_at..].find(close).expect("first close") + first_at;
        let second_at = out.find(second).expect("second open");
        assert!(
            first_close_at < second_at,
            "region one must close before region two opens: {out:?}"
        );
        // The unlinked middle glyph sits between the two link spans.
        let z_at = out.find('Z').expect("unlinked glyph");
        assert!(first_close_at < z_at && z_at < second_at, "{out:?}");
    }

    /// #3029 end-to-end: a long bare URL hard-wraps at narrow width while its
    /// full target travels beside every visible chunk. No escape payload enters
    /// the buffer, and the backend re-emits one OSC 8 pair per row without
    /// altering the visible byte stream.
    #[test]
    fn osc8_metadata_feeds_backend_for_every_wrapped_url_chunk() {
        use codewhale_tui_kit::{markdown_render, osc8};
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Style;
        use ratatui::widgets::{Paragraph, Widget};
        use unicode_width::UnicodeWidthStr;

        let target = "https://example.test/a/very/long/path/that/wraps/across/rows";
        let rendered = markdown_render::render_markdown_tagged(target, 12, Style::default());
        assert!(rendered.len() > 2, "fixture must wrap at narrow width");
        let lines = rendered
            .iter()
            .map(|rendered| rendered.line.clone())
            .collect::<Vec<_>>();
        let line_links = rendered
            .iter()
            .map(|rendered| rendered.links.clone())
            .collect::<Vec<_>>();
        let visible = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(visible.concat(), target);

        let area = Rect::new(3, 2, 12, u16::try_from(lines.len()).unwrap());
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);

        // Visible cells start at the area's real x offset and contain exactly
        // the URL chunks — never the historical `]8;;` payload bytes.
        for (row_index, text) in visible.iter().enumerate() {
            let y = area.y + u16::try_from(row_index).unwrap();
            let row = (0..u16::try_from(text.width()).unwrap())
                .map(|offset| buf[(area.x + offset, y)].symbol().to_string())
                .collect::<String>();
            assert_eq!(row, *text);
        }
        assert!((area.y..area.bottom()).all(|y| {
            (area.x..area.right()).all(|x| {
                let symbol = buf[(x, y)].symbol();
                !symbol.contains('\x1b') && !symbol.contains("]8;;")
            })
        }));

        let regions = osc8::link_regions_for_lines(area, &line_links);
        assert_eq!(regions.len(), rendered.len());
        for ((region, text), row_index) in regions.iter().zip(&visible).zip(0u16..) {
            assert_eq!(region.row, area.y + row_index);
            assert_eq!(region.col_start, area.x);
            assert_eq!(
                region.col_end,
                area.x + u16::try_from(text.width()).unwrap() - 1
            );
            assert_eq!(region.target, target);
        }

        let buf_ref = &buf;
        let cells = (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y, buf_ref[(x, y)].clone())))
            .collect::<Vec<_>>();

        // Capture an unlinked baseline from the exact same cells.
        let _ = osc8::take_frame_links();
        let baseline_writer = SharedWriter::default();
        let baseline_capture = baseline_writer.0.clone();
        let mut baseline =
            ColorCompatBackend::new(baseline_writer, ColorDepth::TrueColor, PaletteMode::Dark);
        baseline
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();

        osc8::set_frame_links(regions);
        let writer = SharedWriter::default();
        let capture = writer.0.clone();
        let mut backend = ColorCompatBackend::new(writer, ColorDepth::TrueColor, PaletteMode::Dark);
        backend
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
            .unwrap();
        let out = String::from_utf8_lossy(&capture.borrow()).to_string();

        let open = format!("\x1b]8;;{target}\x1b\\");
        let close = "\x1b]8;;\x1b\\";
        assert_eq!(
            out.matches(open.as_str()).count(),
            rendered.len(),
            "each row reopens the full target: {out:?}"
        );
        assert_eq!(out.matches(close).count(), rendered.len());

        let baseline_out = String::from_utf8_lossy(&baseline_capture.borrow()).to_string();
        let mut baseline_visible = String::new();
        osc8::strip_ansi_into(&baseline_out, &mut baseline_visible);
        let mut linked_visible = String::new();
        osc8::strip_ansi_into(&out, &mut linked_visible);
        assert_eq!(
            linked_visible, baseline_visible,
            "OSC 8 insertion must not move or alter any rendered cell"
        );
    }

    /// Render one cell the way `draw()` does and hand back the foreground.
    fn cell_fg_after_adaptation(
        symbol: &str,
        fg: Color,
        bg: Color,
        palette_mode: PaletteMode,
        theme_id: ThemeId,
        detected_background: Option<Color>,
    ) -> Color {
        let mut cell = Cell::default();
        cell.set_symbol(symbol).set_fg(fg).set_bg(bg);
        adapt_cell_colors(
            &mut cell,
            ColorDepth::TrueColor,
            palette_mode,
            theme_id,
            &theme_id.ui_theme(),
            detected_background,
        );
        cell.fg
    }

    /// #4833. A white terminal that reports no `COLORFGBG` was detected as
    /// Dark, so the light whitelist never ran and ivory body text landed on a
    /// near-white surface. With the background measured, the contrast floor
    /// catches it even when the palette mode is still Dark.
    #[test]
    fn measured_light_background_lifts_body_text_off_the_surface() {
        let white = Color::Rgb(0xFF, 0xFF, 0xFF);
        let rendered = cell_fg_after_adaptation(
            "x",
            palette::TEXT_BODY,
            Color::Reset,
            PaletteMode::Dark,
            ThemeId::System,
            Some(white),
        );

        assert_ne!(
            rendered,
            palette::TEXT_BODY,
            "body text must not pass through unadapted onto a white surface"
        );
        let ratio = palette::contrast_ratio(rendered, white).unwrap();
        assert!(
            ratio >= palette::AA_BODY_CONTRAST,
            "rendered body text is {ratio}:1 against the measured surface"
        );
    }

    /// The no-regression half of #4833: on a measured dark terminal every
    /// rendered cell comes out byte-identical to what v0.9.1 emitted.
    #[test]
    fn measured_dark_background_changes_nothing() {
        for surface in [
            Color::Rgb(0x00, 0x00, 0x00),
            Color::Rgb(0x1E, 0x1E, 0x1E),
            palette::WHALE_BG,
        ] {
            for token in [
                palette::TEXT_BODY,
                palette::TEXT_HINT,
                palette::TEXT_TOOL_OUTPUT,
                palette::WHALE_ACTION,
                palette::WHALE_HUMAN,
                palette::STATUS_ERROR,
                palette::DIFF_ADDED,
                // Frame chrome sits below 4.5:1 by design and must survive.
                palette::BORDER_COLOR,
            ] {
                let symbol = if token == palette::BORDER_COLOR {
                    "\u{2500}"
                } else {
                    "x"
                };
                assert_eq!(
                    cell_fg_after_adaptation(
                        symbol,
                        token,
                        Color::Reset,
                        PaletteMode::Dark,
                        ThemeId::System,
                        Some(surface),
                    ),
                    token,
                    "{token:?} was rewritten on measured dark surface {surface:?}"
                );
            }
        }
    }

    /// No measurement and no painted ground means the terminal's own ink is
    /// the only colour guaranteed to read on the ground it actually drew.
    #[test]
    fn unknown_background_gives_unpainted_body_text_the_terminal_ink() {
        assert_eq!(
            cell_fg_after_adaptation(
                "x",
                palette::TEXT_BODY,
                Color::Reset,
                PaletteMode::Dark,
                ThemeId::System,
                None,
            ),
            Color::Reset
        );
    }

    /// Frame chrome keeps its intended weight even where the floor is active.
    #[test]
    fn contrast_floor_skips_frame_chrome_on_a_light_surface() {
        let white = Color::Rgb(0xFF, 0xFF, 0xFF);
        assert_eq!(
            cell_fg_after_adaptation(
                "\u{2502}",
                palette::LIGHT_BORDER,
                Color::Reset,
                PaletteMode::Light,
                ThemeId::WhaleLight,
                Some(white),
            ),
            palette::LIGHT_BORDER
        );
    }

    /// Presets own their palette. A user who chose Matrix asked for its
    /// deliberately dim greens; the floor must not repaint them.
    #[test]
    fn explicit_presets_are_exempt_from_the_floor() {
        let matrix = ThemeId::Matrix.ui_theme();
        assert_eq!(
            cell_fg_after_adaptation(
                "x",
                matrix.text_muted,
                Color::Reset,
                PaletteMode::Dark,
                ThemeId::Matrix,
                Some(Color::Rgb(0x00, 0x00, 0x00)),
            ),
            matrix.text_muted
        );
    }
}
