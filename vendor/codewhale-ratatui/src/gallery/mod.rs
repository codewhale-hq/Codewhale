//! Every component with fixture data, for `examples/gallery.rs` and the
//! snapshot tests. The fixtures double as usage examples: the mode and
//! status-line pickers here are the engine's `/mode` and `/statusline`
//! pickers rebuilt from kit parts.
//!
//! One file per area, each exposing `pub(crate) fn entries() -> Vec<Entry>`.
//! [`entries`] concatenates them, so a package edits only its own file:
//!
//! | File | Holds |
//! |---|---|
//! | `hints`, `status`, `surface`, `picker`, `toast`, `icons`, `spinner`, `whale` | the components that exist |
//! | `input` | package Input: text input and form |
//! | `lists` | package Lists: list, fuzzy picker, empty state |
//! | `chrome` | package Chrome: heading, tabs, toggle, segmented, keymap |
//! | `display` | package Display: receipt, diff, tree, progress |
//! | `approval` | package Approval: approval card |
//! | `motion` | package Motion |
//! | `workspace` | messages, composer, tool and agent cards, fleets |
//! | `settings` | values, source, locks, apply timing and defaults |
//! | `atmosphere` | one workspace scene in each of the five ombre palettes |

use crossterm::event::KeyCode;
use ratatui::{buffer::Buffer, layout::Rect};

use crate::{
    Role, Theme,
    keys::{Platform, pair_label},
};

mod approval;
mod artifacts;
mod atmosphere;
mod attention;
mod chart;
mod chrome;
mod display;
mod habitat;
mod hints;
mod icons;
mod input;
mod lists;
mod motion;
mod native_chrome;
mod native_views;
mod ocean;
mod pending_input;
mod picker;
mod posture;
mod scenes;
mod settings;
pub mod showcase;
mod spinner;
mod status;
mod surface;
mod toast;
mod transcript;
mod tui_palettes;
mod verification;
mod whale;
mod workbar;
mod workbench;
mod workspace;

/// One gallery entry: a name, the size it is drawn at by default, and a
/// function that paints it. `draw` must paint inside the `Rect` it is given
/// (the gallery draws it at other widths too) and use only the `Theme`.
pub struct Entry {
    pub name: &'static str,
    pub width: u16,
    pub height: u16,
    pub draw: fn(Rect, &mut Buffer, &Theme),
}

/// `↑↓`, or `Up/Down` where marks are ASCII-safe: one spelling for the
/// gallery's key hints.
fn arrows(theme: &Theme) -> String {
    pair_label(KeyCode::Up, KeyCode::Down, Platform::current(theme.ascii()))
}

/// Every entry, in gallery order.
#[must_use]
pub fn entries() -> Vec<Entry> {
    [
        native_views::entries(),
        native_chrome::entries(),
        posture::entries(),
        workbar::entries(),
        tui_palettes::entries(),
        scenes::entries(),
        showcase::entries(),
        workbench::entries(),
        attention::entries(),
        artifacts::entries(),
        habitat::entries(),
        hints::entries(),
        status::entries(),
        surface::entries(),
        picker::entries(),
        toast::entries(),
        icons::entries(),
        spinner::entries(),
        verification::entries(),
        whale::entries(),
        input::entries(),
        lists::entries(),
        chrome::entries(),
        display::entries(),
        chart::entries(),
        approval::entries(),
        motion::entries(),
        workspace::entries(),
        settings::entries(),
        atmosphere::entries(),
        ocean::entries(),
        pending_input::entries(),
        transcript::entries(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Resolve the source palette used by a native preview. Named presets keep
/// their own background; terminal fallbacks still follow the input capabilities.
#[must_use]
pub fn theme_for(entry: &Entry, theme: &Theme) -> Theme {
    if let Some(palette) = tui_palettes::palette_for_name(entry.name) {
        return tui_palettes::theme_for(palette, theme);
    }
    if entry.name.starts_with("view-")
        || entry.name == "instrument-surface"
        || entry.name == "session-list"
        || entry.name.starts_with("native-")
        || entry.name.starts_with("workbar-")
        || entry.name.starts_with("workflow-")
        || entry.name.starts_with("posture-")
        || entry.name.starts_with("metrics-")
        || entry.name.starts_with("showcase-")
    {
        return theme.tui();
    }
    *theme
}

/// Render one entry for one theme at its own size.
#[must_use]
pub fn render(entry: &Entry, theme: &Theme) -> Buffer {
    render_at(entry, theme, entry.width, entry.height)
}

/// Render one entry for one theme at another size, on the theme's
/// `Background` as the gallery paints it.
#[must_use]
pub fn render_at(entry: &Entry, theme: &Theme, width: u16, height: u16) -> Buffer {
    let theme = theme_for(entry, theme);
    crate::testing::render(width, height, |area, buf| {
        buf.set_style(area, theme.bg(Role::Background));
        (entry.draw)(area, buf, &theme);
    })
}
