//! Calm loading placeholders with motion-gated shimmer.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{MotionMode, Paint, Role, Theme};

/// Cadence of one shimmer step when [`MotionMode::Full`] is active.
pub const SKELETON_FRAME_INTERVAL: Duration = Duration::from_millis(120);

/// Layout shape of a [`Skeleton`] placeholder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SkeletonKind {
    /// Uniform full-width placeholder rows.
    #[default]
    Rows,
    /// Staggered prose-like line widths with a shorter trailing line.
    Text,
}

/// Calm loading placeholder matching Desktop `packages/ui` `<Skeleton>`.
///
/// Under [`MotionMode::Full`], a gentle highlight band steps across each bar
/// according to the caller-supplied `tick` (or `elapsed` duration). Under
/// [`MotionMode::Reduced`] and [`MotionMode::Still`], the placeholder stays
/// strictly static.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skeleton {
    /// Number of placeholder rows to paint.
    pub rows: u16,
    /// Visual shape (`Rows` vs `Text`).
    pub kind: SkeletonKind,
    /// Motion policy governing shimmer animation.
    pub motion: MotionMode,
    /// Caller-supplied animation tick.
    pub tick: u64,
}

impl Skeleton {
    /// Create a block of `rows` uniform loading bars.
    #[must_use]
    pub const fn rows(rows: u16) -> Self {
        Self {
            rows,
            kind: SkeletonKind::Rows,
            motion: MotionMode::Full,
            tick: 0,
        }
    }

    /// Create `lines` staggered text-shaped placeholder lines.
    #[must_use]
    pub const fn text(lines: u16) -> Self {
        Self {
            rows: lines,
            kind: SkeletonKind::Text,
            motion: MotionMode::Full,
            tick: 0,
        }
    }

    /// Set the [`MotionMode`] governing whether the shimmer animates.
    #[must_use]
    pub const fn motion(mut self, motion: MotionMode) -> Self {
        self.motion = motion;
        self
    }

    /// Set the caller-driven shimmer frame tick.
    #[must_use]
    pub const fn tick(mut self, tick: u64) -> Self {
        self.tick = tick;
        self
    }

    /// Derive the shimmer tick from an `elapsed` duration.
    #[must_use]
    pub fn elapsed(mut self, elapsed: Duration) -> Self {
        let interval_ms = SKELETON_FRAME_INTERVAL.as_millis();
        self.tick = u64::try_from(elapsed.as_millis() / interval_ms).unwrap_or(u64::MAX);
        self
    }

    fn line_width(&self, row_index: u16, max_width: u16) -> usize {
        let max = usize::from(max_width);
        if max == 0 {
            return 0;
        }
        match self.kind {
            SkeletonKind::Rows => max,
            SkeletonKind::Text => {
                const RATIOS: [usize; 4] = [100, 88, 94, 72];
                let pct = if row_index + 1 == self.rows && self.rows > 1 {
                    60
                } else {
                    RATIOS[usize::from(row_index) % RATIOS.len()]
                };
                ((max * pct) / 100).max(1).min(max)
            }
        }
    }
}

impl Paint for Skeleton {
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        self.rows
    }

    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() || self.rows == 0 {
            return;
        }

        let ascii = theme.ascii();
        let base_glyph = if ascii { '-' } else { '░' };
        let shimmer_glyph = if ascii { '=' } else { '▒' };
        let base_style = theme.fg(Role::Dim);
        let shimmer_style = theme.fg(Role::Muted);
        let animates = self.motion.animates();

        let count = self.rows.min(area.height);
        for row_idx in 0..count {
            let width = self.line_width(row_idx, area.width);
            if width == 0 {
                continue;
            }

            let y = area.y + row_idx;
            let row_rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };

            if !animates || width < 3 {
                let bar: String = std::iter::repeat_n(base_glyph, width).collect();
                Line::from(Span::styled(bar, base_style)).render(row_rect, buf);
                continue;
            }

            let band = (width / 5).clamp(2, 5);
            let cycle = width + band;
            let phase = usize::try_from(
                (self.tick.saturating_add(u64::from(row_idx) * 2)) % (cycle as u64),
            )
            .unwrap_or(0);
            let hi_start = phase.saturating_sub(band);
            let hi_end = phase.min(width);

            let mut spans = Vec::with_capacity(3);
            if hi_start > 0 {
                let left: String = std::iter::repeat_n(base_glyph, hi_start).collect();
                spans.push(Span::styled(left, base_style));
            }
            if hi_end > hi_start {
                let mid: String = std::iter::repeat_n(shimmer_glyph, hi_end - hi_start).collect();
                spans.push(Span::styled(mid, shimmer_style));
            }
            if hi_end < width {
                let right: String = std::iter::repeat_n(base_glyph, width - hi_end).collect();
                spans.push(Span::styled(right, base_style));
            }
            Line::from(spans).render(row_rect, buf);
        }
    }
}
