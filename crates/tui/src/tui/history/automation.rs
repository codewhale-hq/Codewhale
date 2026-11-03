//! Typed transcript receipt for durable scheduled automations
//! (AUTOMATION-VISIBILITY-SPEC §2.2).
//!
//! One-line bulleted card, mirroring kimi's `CronMessageComponent`:
//! `● {name} {verb}` plus an optional dim detail segment. Replaces the
//! bare-String `HistoryCell::System` receipts `automation_routing.rs` used to
//! emit, so the palette can color the event and the pager can navigate it.
//! Later slices add producers (engine fire/complete/coalesce/miss/expire
//! events); the kind vocabulary is the spec's full set.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette::ChromeInk;
use codewhale_tui_kit::glyphs;

/// What happened to the automation or its run. Drives the card's ink; the
/// visible verb phrase comes from the producer (localized at construction).
// Slice 1 produces Started / Completed / Failed / Mutated (the `/automation
// run` receipt and the projection's settled-run receipts); Fired / Coalesced
// / Missed / Expired arrive with their engine-side producers in Slice 4.
#[cfg_attr(not(test), expect(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationCellKind {
    /// The schedule fired and a run was enqueued.
    Fired,
    /// A run started in the background.
    Started,
    /// A run completed in the background.
    Completed,
    /// A run genuinely crashed — the only kind that may wear Failure red.
    Failed,
    /// A run was canceled by the operator, a cancel timeout or shutdown
    /// (#6162). Consequential enough to see, never a failure.
    Canceled,
    /// Missed slots collapsed into a single delivery.
    Coalesced,
    /// A scheduled run was missed while the app was down.
    Missed,
    /// An idle automation expired (paused, never deleted).
    Expired,
    /// The definition changed (pause / resume / delete).
    Mutated,
}

impl AutomationCellKind {
    /// Status-bar grammar ink (docs/design/STATUS_BAR_COLOR_GRAMMAR.md):
    /// Fired/Started → Active, Completed → Outcome, Coalesced/Missed/Expired
    /// → Attention ("consequential, needs your eye"), Mutated → Info. Only a
    /// genuinely crashed run takes Failure — a failed report job is not a
    /// product failure.
    #[must_use]
    pub const fn chrome_ink(self) -> ChromeInk {
        match self {
            Self::Fired | Self::Started => ChromeInk::Active,
            Self::Completed => ChromeInk::Outcome,
            Self::Failed => ChromeInk::Failure,
            Self::Canceled | Self::Coalesced | Self::Missed | Self::Expired => ChromeInk::Attention,
            Self::Mutated => ChromeInk::Info,
        }
    }

    /// Canonical verb phrase for run-lifecycle kinds. `Mutated` has none —
    /// the producer supplies the concrete word (`paused` / `resumed` /
    /// `deleted`) via [`AutomationCell::mutated`].
    #[must_use]
    pub fn canonical_verb(self, locale: Locale) -> Option<String> {
        let id = match self {
            Self::Fired => MessageId::AutomationReceiptFired,
            Self::Started => MessageId::AutomationReceiptStarted,
            Self::Completed => MessageId::AutomationReceiptCompleted,
            Self::Failed => MessageId::AutomationRunStatusFailed,
            Self::Canceled => MessageId::AutomationRunStatusCanceled,
            Self::Coalesced => MessageId::AutomationReceiptCoalesced,
            Self::Missed => MessageId::AutomationReceiptMissed,
            Self::Expired => MessageId::AutomationReceiptExpired,
            Self::Mutated => return None,
        };
        Some(tr(locale, id).into_owned())
    }
}

/// One-line automation receipt card. `name`, `verb`, and `detail` arrive
/// display-safe (ANSI-stripped, secret-redacted) and localized; the renderer
/// only truncates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationCell {
    pub kind: AutomationCellKind,
    pub name: String,
    pub verb: String,
    pub detail: Option<String>,
}

impl AutomationCell {
    /// Receipt for a run-lifecycle event; the kind supplies the verb.
    /// `Mutated` is not a run-lifecycle kind — use [`Self::mutated`].
    #[must_use]
    pub fn event(kind: AutomationCellKind, name: String, locale: Locale) -> Self {
        debug_assert!(
            !matches!(kind, AutomationCellKind::Mutated),
            "Mutated receipts name their mutation; use AutomationCell::mutated"
        );
        Self {
            kind,
            name,
            verb: kind.canonical_verb(locale).unwrap_or_default(),
            detail: None,
        }
    }

    /// Receipt for a definition change; `verb` is the concrete mutation word
    /// (already localized by the producer).
    #[must_use]
    pub fn mutated(name: String, verb: String) -> Self {
        Self {
            kind: AutomationCellKind::Mutated,
            name,
            verb,
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.filter(|detail| !detail.trim().is_empty());
        self
    }

    /// Plain-text form for the pager/clipboard/inspection surfaces — the same
    /// line the card paints, minus ink.
    #[must_use]
    pub fn plain_summary(&self) -> String {
        let mut line = format!("{} {}", self.name, self.verb);
        if let Some(detail) = &self.detail {
            line.push_str("  ");
            line.push_str(detail);
        }
        line.trim_end().to_string()
    }

    /// Render the one-line card at `width`. The card never wraps: the detail
    /// segment sheds first, then the name/verb truncate.
    pub(crate) fn render(&self, width: u16) -> Vec<Line<'static>> {
        let color = self.kind.chrome_ink().color(&codewhale_palette::UI_THEME);
        let bullet_width = 2usize; // `● ` — the charter's current marker + space
        let budget = usize::from(width).saturating_sub(bullet_width);
        let mut text = self.name.clone();
        if !self.verb.is_empty() {
            text.push(' ');
            text.push_str(&self.verb);
        }
        let text = codewhale_localization::truncate_to_width(&text, budget);
        let used = unicode_width::UnicodeWidthStr::width(text.as_str());
        let detail = self.detail.as_deref().and_then(|detail| {
            let remaining = budget.saturating_sub(used + 2);
            (remaining > 0).then(|| codewhale_localization::truncate_to_width(detail, remaining))
        });
        let mut spans = vec![
            Span::styled(
                format!("{} ", glyphs::CURRENT),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(color)),
        ];
        if let Some(detail) = detail {
            spans.push(Span::styled(
                format!("  {detail}"),
                Style::default().fg(codewhale_palette::TEXT_DIM),
            ));
        }
        vec![Line::from(spans)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::golden_harness::{assert_matches_golden, render_golden_text};

    fn cell_text(cell: &AutomationCell, width: u16) -> String {
        cell.render(width)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn receipt_kinds_follow_the_color_grammar() {
        assert_eq!(AutomationCellKind::Fired.chrome_ink(), ChromeInk::Active);
        assert_eq!(AutomationCellKind::Started.chrome_ink(), ChromeInk::Active);
        assert_eq!(
            AutomationCellKind::Completed.chrome_ink(),
            ChromeInk::Outcome
        );
        assert_eq!(AutomationCellKind::Failed.chrome_ink(), ChromeInk::Failure);
        for kind in [
            AutomationCellKind::Canceled,
            AutomationCellKind::Coalesced,
            AutomationCellKind::Missed,
            AutomationCellKind::Expired,
        ] {
            assert_eq!(kind.chrome_ink(), ChromeInk::Attention, "{kind:?}");
        }
        assert_eq!(AutomationCellKind::Mutated.chrome_ink(), ChromeInk::Info);
    }

    /// The spec's reservation check (§6 Slice 1 accept): no automation
    /// receipt ink resolves to the failure color in any selectable preset —
    /// except `Failed`, which pins Failure deliberately and is reserved for a
    /// genuinely crashed run, never a report job.
    #[test]
    fn no_automation_receipt_ink_spends_failure_red_but_the_crashed_run() {
        for theme_id in codewhale_palette::SELECTABLE_THEMES {
            let theme = theme_id.ui_theme();
            for kind in [
                AutomationCellKind::Fired,
                AutomationCellKind::Started,
                AutomationCellKind::Completed,
                AutomationCellKind::Canceled,
                AutomationCellKind::Coalesced,
                AutomationCellKind::Missed,
                AutomationCellKind::Expired,
                AutomationCellKind::Mutated,
            ] {
                assert_ne!(
                    kind.chrome_ink().color(&theme),
                    theme.error_fg,
                    "theme '{}' spends Failure red on automation receipt {kind:?}",
                    theme_id.name()
                );
            }
        }
    }

    #[test]
    fn cards_are_one_line_and_shed_detail_before_the_name() {
        let cell = AutomationCell::event(
            AutomationCellKind::Fired,
            "Release Manager".to_string(),
            Locale::En,
        )
        .with_detail(Some("run r-8f21 · task t-9de".to_string()));
        let full = cell_text(&cell, 80);
        assert_eq!(full, "● Release Manager fired  run r-8f21 · task t-9de");
        let narrow = cell_text(&cell, 24);
        assert!(
            !narrow.contains("run r-8f21"),
            "detail sheds first: {narrow}"
        );
        assert!(narrow.starts_with("● "), "{narrow}");
        assert_eq!(narrow.lines().count(), 1, "the card never wraps");
    }

    /// Golden contract for the fired/completed cards (spec §6 Slice 1
    /// accept), following `golden_harness.rs`. Re-bless with
    /// `CODEWHALE_BLESS_GOLDENS=1`.
    #[test]
    fn receipt_cards_match_goldens() {
        let fired = AutomationCell::event(
            AutomationCellKind::Fired,
            "Release Manager".to_string(),
            Locale::En,
        )
        .with_detail(Some("run r-8f21 · task t-9de".to_string()));
        let completed = AutomationCell::event(
            AutomationCellKind::Completed,
            "Documentation".to_string(),
            Locale::En,
        )
        .with_detail(Some("42s · run r-8f19".to_string()));
        let mutated =
            AutomationCell::mutated("Market Intelligence".to_string(), "paused".to_string());
        for (w, h) in [(80u16, 3u16), (40, 3)] {
            let rendered = render_golden_text(w, h, |buf| {
                for (y, cell) in [&fired, &completed, &mutated].into_iter().enumerate() {
                    for line in cell.render(w) {
                        buf.set_line(0, y as u16, &line, w);
                    }
                }
            });
            assert_matches_golden(&format!("automation_receipts_{w}x{h}"), &rendered);
        }
    }
}
