use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::tui::color_compat::ascii_safe_enabled;
use codewhale_palette as palette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalReceiptOutcome {
    AllowedOnce,
    AllowedInConversation,
    AlwaysAllowedInRepo,
    Denied,
}

impl ApprovalReceiptOutcome {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowedOnce => "Allowed once",
            Self::AllowedInConversation => "Allowed in this conversation",
            Self::AlwaysAllowedInRepo => "Always allowed in this repo",
            Self::Denied => "Denied",
        }
    }

    fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (Self::Denied, true) => "x",
            (Self::Denied, false) => "✕",
            (_, true) => "v",
            (_, false) => "✓",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalReceiptCell {
    pub outcome: ApprovalReceiptOutcome,
    pub subject: String,
}

impl ApprovalReceiptCell {
    #[must_use]
    pub fn new(outcome: ApprovalReceiptOutcome, subject: impl Into<String>) -> Self {
        Self {
            outcome,
            subject: subject.into(),
        }
    }

    #[must_use]
    pub fn plain_summary(&self) -> String {
        let subject = codewhale_ratatui::visible_text(&self.subject, false);
        format!("{} · {subject}", self.outcome.label())
    }

    pub(crate) fn render(&self, width: u16) -> Vec<Line<'static>> {
        let ascii = ascii_safe_enabled();
        let leader = if ascii { "- " } else { "─ " };
        let sep = if ascii { " - " } else { " · " };
        let prefix = format!(
            "{leader}{} {}{sep}",
            self.outcome.glyph(ascii),
            self.outcome.label()
        );
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        let budget = usize::from(width);
        if budget <= prefix_width {
            let truncated = codewhale_localization::truncate_to_width(&prefix, budget);
            return vec![Line::from(Span::styled(
                truncated,
                Style::default().fg(palette::TEXT_MUTED),
            ))];
        }
        let subject_budget = budget.saturating_sub(prefix_width);
        let visible_subject = codewhale_ratatui::visible_text(&self.subject, ascii);
        let subject = codewhale_localization::truncate_to_width(&visible_subject, subject_budget);
        vec![Line::from(vec![
            Span::styled(prefix, Style::default().fg(palette::TEXT_MUTED)),
            Span::styled(subject, Style::default().fg(palette::TEXT_PRIMARY)),
        ])]
    }
}
