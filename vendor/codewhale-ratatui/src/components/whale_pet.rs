//! The fully animated GPUI pet as a stateful Ratatui component. The host
//! observes and advances one shared Stage; painting never advances time.

use std::borrow::Cow;

use ratatui::{buffer::Buffer, layout::Rect, widgets::StatefulWidget};

use crate::whale_motion::{CoveScene, Stage, View, pixels, rasterize_colored, rig};
use crate::{Paint, Theme, Whale, WhaleState, avatar_sprite::Sprite};

/// The native pet's rendering style. Both read the same live contours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetStyle {
    /// Full color curves, apertures, props and cove, using terminal half-blocks.
    #[default]
    Color,
    /// The canonical colored Braille geometry, without scenery.
    Braille,
}

/// A live whale in its GPUI cove: springs, fins, eyes, blinks, props, particles
/// and calves all come from the existing canonical performance. Pointer input
/// is decorative and enters through the host's [`Stage::cove_observe`] and
/// [`Stage::cove_tap`]; it never changes Engine state.
///
/// The host supplies authoritative [`crate::whale_motion::Inputs`], calls
/// [`Stage::advance`] once per frame, and schedules with
/// [`Stage::cadence`] (Hero for color, Terminal for Braille). Reduced motion
/// paints a settled poster and schedules no animation. Unsupported color
/// profiles use Braille; ASCII and small areas show the status words alone.
///
/// ```no_run
/// use codewhale_ratatui::{Theme, WhalePet, whale_motion::Stage};
/// # fn draw(frame: &mut ratatui::Frame<'_>, stage: &mut Stage) {
/// let theme = Theme::detect();
/// stage.advance(std::time::Instant::now());
/// frame.render_stateful_widget(WhalePet::new(&theme), frame.area(), stage);
/// # }
/// ```
pub struct WhalePet<'a> {
    theme: &'a Theme,
    words: Option<Cow<'a, str>>,
    cove: bool,
    style: PetStyle,
    direction: rig::Direction,
}

impl<'a> WhalePet<'a> {
    #[must_use]
    pub const fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            words: None,
            cove: true,
            style: PetStyle::Color,
            direction: rig::MARK_DIRECTION,
        }
    }

    /// Localized words from the host. The default names the current action;
    /// empty words hide the caption when the host already presents the state.
    #[must_use]
    pub fn words(mut self, words: impl Into<Cow<'a, str>>) -> Self {
        self.words = Some(words.into());
        self
    }

    #[must_use]
    pub const fn cove(mut self, shown: bool) -> Self {
        self.cove = shown;
        self
    }

    #[must_use]
    pub const fn style(mut self, style: PetStyle) -> Self {
        self.style = style;
        self
    }

    /// The shared rig's mark, cruise or open view; never a separate animation.
    #[must_use]
    pub const fn direction(mut self, direction: rig::Direction) -> Self {
        self.direction = direction;
        self
    }

    /// The bounded square art rectangle, reserving one row for state words.
    /// Hosts use this same rectangle to map mouse input with [`Self::point`].
    #[must_use]
    pub fn art_area(area: Rect) -> Rect {
        let side = area
            .width
            .min(area.height.saturating_sub(1).saturating_mul(2))
            .min(pixels::MAX_SIDE as u16);
        let side = side - side % 2;
        Rect::new(
            area.x.saturating_add((area.width - side) / 2),
            area.y
                .saturating_add((area.height.saturating_sub(1) - side / 2) / 2),
            side,
            side / 2,
        )
    }

    /// Convert a terminal cell to the GPUI cove's −62…62 design coordinates.
    /// Outside points return `None` so the host can release the resting gaze.
    #[must_use]
    pub fn point(area: Rect, column: u16, row: u16) -> Option<(f64, f64)> {
        let art = Self::art_area(area);
        if art.is_empty() || !art.contains((column, row).into()) {
            return None;
        }
        Some((
            (f64::from(column - art.x) + 0.5) / f64::from(art.width) * 124. - 62.,
            (f64::from(row - art.y) + 0.5) / f64::from(art.height) * 124. - 62.,
        ))
    }

    /// Paint the current shared frame. The only mutable decoration is the
    /// cove's idempotent gaze sample; time and owner truth remain host-owned.
    pub fn paint(&self, area: Rect, buf: &mut Buffer, stage: &mut Stage) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let mut state = WhaleState::from_key(stage.acting().id()).unwrap_or(WhaleState::Rest);
        let mut words = None;
        if matches!(state, WhaleState::Pod { .. }) {
            let calves = stage
                .director()
                .activity
                .as_ref()
                .and_then(|a| a.parallel)
                .filter(|n| n.is_finite() && *n >= 0. && n.fract() == 0.)
                .unwrap_or(0.);
            state = WhaleState::Pod {
                calves: calves.min(f64::from(u8::MAX)) as u8,
            };
            if calves > f64::from(u8::MAX) {
                words = Some(format!("Working with {calves:.0} agents"));
            }
        }
        let mut whale = Whale::new(state);
        if let Some(words) = words {
            whale = whale.words(words);
        }
        if let Some(words) = &self.words {
            whale = whale.words(words.to_string());
        }
        let art = Self::art_area(area);
        let words_only = crate::whale::Grid {
            cols: 0,
            rows: 0,
            cells: Vec::new(),
        };
        if self.theme.ascii() || art.width < 16 || art.height < 8 {
            // A deliberately invalid frame asks the shared painter for words.
            if self.words.as_deref() != Some("") {
                whale.paint_frame(area, buf, self.theme, &words_only);
            }
            return;
        }
        let dark = self.theme.caps().appearance != crate::detect::Appearance::Light;
        let view = View {
            dir: self.direction,
            ..View::hero(f64::from(art.width), 1.)
        };
        if self.style == PetStyle::Braille || !self.theme.paints_grounds() {
            let parts = stage.scene(view);
            rasterize_colored(
                &parts,
                usize::from(art.width),
                usize::from(art.height),
                0.5,
                dark,
            )
            .paint_with_contrast(&whale, area, buf, self.theme, 3.);
            return;
        }
        let scene = if self.cove {
            stage.cove_scene(view, dark)
        } else {
            CoveScene {
                behind: Vec::new(),
                parts: stage.scene(view),
                front: Vec::new(),
            }
        };
        let pixels = pixels::render(&scene, usize::from(art.width), dark);
        if let Ok(sprite) = Sprite::image(&pixels, usize::from(art.width), usize::from(art.width)) {
            sprite.paint(art, buf, self.theme);
        }
        if self.words.as_deref() != Some("") {
            whale.paint_frame(
                Rect::new(area.x, art.bottom(), area.width, 1),
                buf,
                self.theme,
                &words_only,
            );
        }
    }
}

impl StatefulWidget for WhalePet<'_> {
    type State = Stage;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Stage) {
        self.paint(area, buf, state);
    }
}

impl StatefulWidget for &WhalePet<'_> {
    type State = Stage;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Stage) {
        self.paint(area, buf, state);
    }
}
