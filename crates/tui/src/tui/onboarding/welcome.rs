//! Calm welcome and ready screens for first run.
//!
//! The welcome is unnumbered and asks nothing: one headline, one supporting
//! sentence, one primary action, one exit. The ready screen closes the flow
//! by handing the user the composer pre-seeded with a first task — Enter
//! opens the product, never another educational surface.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::app::App;
use codewhale_localization::MessageId;
use codewhale_palette as palette;

pub fn lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    headline(&mut out, app, MessageId::OnboardWelcomeTitle, width);
    accent_rule(&mut out, app, width);
    body(&mut out, app, MessageId::OnboardWelcomeLead, width);
    out
}

pub fn ready_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    headline(&mut out, app, MessageId::OnboardReadyTitle, width);
    accent_rule(&mut out, app, width);
    body(&mut out, app, MessageId::OnboardReadyLead, width);
    let notice = if app.onboarding_explore_offline {
        Some(app.tr(MessageId::OnboardOfflineNotice).into_owned())
    } else {
        app.setup_notice().map(|notice| notice.text.clone())
    };
    if let Some(message) = notice {
        let warn_fg = super::theme_or(app.ui_theme.warning, palette::STATUS_WARNING);
        out.push(Line::from(""));
        for segment in super::wrap_words(&message, width) {
            out.push(Line::from(Span::styled(
                segment,
                Style::default().fg(warn_fg),
            )));
        }
    }
    out
}

fn accent_rule(out: &mut Vec<Line<'static>>, app: &App, width: usize) {
    let rule_width = 6.min(width);
    if rule_width == 0 {
        out.push(Line::from(""));
        return;
    }
    let ch = if crate::tui::color_compat::ascii_safe_enabled() {
        "-"
    } else {
        "─"
    };
    let color = super::theme_or(app.ui_theme.border, palette::BORDER_COLOR);
    out.push(Line::from(Span::styled(
        ch.repeat(rule_width),
        Style::default().fg(color),
    )));
}

fn headline(out: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let fg = super::theme_or(app.ui_theme.accent_action, palette::WHALE_HUMAN);
    for segment in super::wrap_words(&app.tr(id), width) {
        out.push(Line::from(Span::styled(
            segment,
            Style::default().fg(fg).add_modifier(Modifier::BOLD),
        )));
    }
}

fn body(out: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let fg = super::theme_or(app.ui_theme.text_body, palette::TEXT_PRIMARY);
    for segment in super::wrap_words(&app.tr(id), width) {
        out.push(Line::from(Span::styled(segment, Style::default().fg(fg))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_localization::Locale;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_app_with_locale(locale: Locale) -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = locale;
        app
    }

    fn body(_app: &App, lines: Vec<Line<'static>>) -> String {
        lines
            .into_iter()
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ready_screen_reads_offline_truth_from_typed_state() {
        let mut app = test_app_with_locale(Locale::En);
        app.onboarding_explore_offline = true;
        app.show_onboarding_notice(
            "Workspace trust was not changed.",
            crate::tui::app::StatusToastLevel::Warning,
        );
        let text = body(&app, ready_lines(&app, 70));
        assert!(
            text.contains(app.tr(MessageId::OnboardOfflineNotice).as_ref()),
            "{text}"
        );
        assert!(!text.contains("Workspace trust was not changed."), "{text}");
    }
}

#[cfg(test)]
mod narrow_locale_tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_localization::{Locale, MessageId, tr};
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    fn probe_app() -> App {
        let options = TuiOptions {
            model: "test-model".to_string(),
            ..crate::tui::test_support::test_tui_options(PathBuf::from("workspace-fixture"))
        };
        App::new(options, &Config::default())
    }

    fn flatten(lines: Vec<Line<'static>>) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The welcome headline is a whole sentence in most locales. It used to be
    /// emitted as one unwrapped line, so the first screen of first run read
    /// "Codewhale arbeitet mit dir in diesem O" in German at 40 columns.
    #[test]
    fn the_welcome_headline_survives_a_small_terminal_in_every_locale() {
        let mut app = probe_app();
        for locale in Locale::shipped().iter().copied() {
            app.ui_locale = locale;
            for width in [40usize, 60, 80] {
                let rendered = flatten(lines(&app, width));
                for row in &rendered {
                    assert!(
                        row.width() <= width,
                        "{locale:?} at {width}: row overflows: {row:?}",
                    );
                }
                // Whitespace removed, not collapsed: Japanese and Chinese wrap
                // between characters with no space between them in the source.
                let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                let joined = squash(&rendered.join(" "));
                let title = tr(locale, MessageId::OnboardWelcomeTitle);
                assert!(
                    joined.contains(&squash(title.as_ref())),
                    "{locale:?} at {width}: headline was cut.\nwanted: {title}\ngot: {rendered:?}",
                );
            }
        }
    }
}
