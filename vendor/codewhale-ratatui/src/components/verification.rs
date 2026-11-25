//! The Engine's verification tick, distinct from its working swell.
//!
//! The frame table and cadence come from `Hmbown/CodeWhale`
//! `a79ce5c4d5ed1a5f7032185710c27343a900351c`,
//! `crates/tui/src/tui/spinner.rs::verification_tick_frame`. Earned delay,
//! ASCII rotation, elapsed words and quiet-mode marks share this kit's
//! [`Spinner`]. The caller supplies elapsed time and owns all scheduling.

use std::time::Duration;

use crate::{MotionMode, Paint, Role, Spinner, Theme, spin, text};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
};

/// A checking phase uses a round tick while working uses a swell.
///
/// Construct it only for verification the host has actually observed. The
/// verb is caller-owned, and the elapsed time is measured rather than a
/// progress estimate. Reduced and still motion show a static mark and word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationSpinner {
    pub verb: String,
    pub elapsed: Duration,
    pub motion: MotionMode,
    /// Optional detail label shown after the verb (`Checking · cargo test`).
    pub detail: Option<String>,
}

impl VerificationSpinner {
    /// The Engine's eight adjacent verification frames.
    pub const FRAMES: [&'static str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

    #[must_use]
    pub fn new(verb: impl Into<String>, elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            verb: verb.into(),
            elapsed,
            motion,
            detail: None,
        }
    }

    /// Attach an optional detail label shown after the verb (`Checking · cargo test`).
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let value = detail.into();
        self.detail = (!value.is_empty()).then_some(value);
        self
    }

    /// Sample a verification frame without reading a clock.
    #[must_use]
    pub fn frame(elapsed: Duration, motion: MotionMode, ascii: bool) -> &'static str {
        if !motion.animates() || ascii || elapsed < spin::EARN_DELAY {
            return spin::frame(elapsed, motion, ascii);
        }
        let step = (elapsed - spin::EARN_DELAY).as_millis() / spin::FRAME_INTERVAL.as_millis();
        Self::FRAMES[(step % Self::FRAMES.len() as u128) as usize]
    }

    /// The shared five-step cadence; quiet motion asks for no redraws.
    #[must_use]
    pub fn next_frame_in(elapsed: Duration, motion: MotionMode) -> Option<Duration> {
        spin::next_frame_in(elapsed, motion)
    }

    /// A tick, the caller's sanitized verb, optional detail and measured-time words.
    #[must_use]
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        self.spans_for_width(None, theme)
    }

    fn spans_for_width(&self, max_width: Option<usize>, theme: &Theme) -> Vec<Span<'static>> {
        let mut spans = Spinner::new(&self.verb, self.elapsed, self.motion).spans(theme);
        spans[0].content = Self::frame(self.elapsed, self.motion, theme.ascii()).into();
        let Some(raw_detail) = self.detail.as_deref() else {
            return spans;
        };
        let detail = text::display_safe(raw_detail);
        if detail.is_empty() {
            return spans;
        }
        let sep = if theme.ascii() { " - " } else { " · " };
        let sep_w = text::width(sep);
        let fitted = match max_width {
            Some(limit) => {
                let base_w: usize = spans.iter().map(Span::width).sum();
                let room = limit.saturating_sub(base_w.saturating_add(sep_w));
                if room == 0 {
                    return spans;
                }
                text::truncate(&detail, room, theme.ascii()).into_owned()
            }
            None => detail.into_owned(),
        };
        if fitted.is_empty() {
            return spans;
        }
        let insert_at = 3.min(spans.len());
        let muted = theme.fg(Role::Muted);
        spans.insert(insert_at, Span::styled(fitted, muted));
        spans.insert(insert_at, Span::styled(sep, muted));
        spans
    }
}

impl Paint for VerificationSpinner {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if !area.is_empty() {
            // Buffer::set_line also handles a right edge of u16::MAX, where
            // the widget Line renderer can overflow while clipping a span.
            let spans = self.spans_for_width(Some(usize::from(area.width)), theme);
            super::workbench::row(area, buf, &Line::from(spans));
        }
    }
}
