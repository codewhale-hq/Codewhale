//! Workspace trust prompt for onboarding.
//!
//! One decision: trust the instructions and files in this folder, or
//! continue without trust. The explicit 1/Y · 2/U · 3/N keys stay — trusting
//! a workspace is a security boundary and must never happen by reflex.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::app::App;
use codewhale_localization::MessageId;
use codewhale_palette as palette;

/// Wrap a path-bearing line at `/` boundaries so a deep workspace never
/// hard-splits mid-component under ratatui's whitespace-only `Wrap`.
/// Continuation lines are indented to read as one location.
fn wrap_on_path_separators(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chunk = String::new();
    let flush = |current: &mut String, chunk: &mut String, out: &mut Vec<String>| {
        if chunk.is_empty() {
            return;
        }
        let candidate_len = current.chars().count() + chunk.chars().count();
        if candidate_len > width && !current.is_empty() {
            out.push(std::mem::take(current));
            current.push_str("  ");
        }
        current.push_str(chunk);
        chunk.clear();
    };
    for ch in text.chars() {
        chunk.push(ch);
        if ch == '/' {
            flush(&mut current, &mut chunk, &mut out);
        }
    }
    flush(&mut current, &mut chunk, &mut out);
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        vec![String::new()]
    } else {
        out
    }
}

pub fn lines(app: &App, content_width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        app.tr(MessageId::OnboardTrustTitle).to_string(),
        Style::default()
            .fg(palette::WHALE_ACTION)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    // Prose on this screen wraps like prose on every other onboarding screen.
    // It used to be pushed as one unwrapped line, so at 40 columns the trust
    // question rendered as "Should Codewhale work with the instruc" — severed
    // mid-word, with nothing marking the cut. Asking someone to grant
    // filesystem trust while the question itself is truncated is the worst
    // place in the product for this to happen.
    for segment in super::wrap_words(
        app.tr(MessageId::OnboardTrustQuestion).as_ref(),
        content_width,
    ) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
    let location = format!(
        "{}{}",
        app.tr(MessageId::OnboardTrustLocationPrefix),
        codewhale_util::utils::display_path(&app.workspace)
    );
    for segment in wrap_on_path_separators(&location, content_width) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_MUTED),
        )));
    }
    lines.push(Line::from(""));
    for id in [
        MessageId::OnboardTrustRiskHint,
        MessageId::OnboardTrustEffectHint,
    ] {
        for segment in super::wrap_words(app.tr(id).as_ref(), content_width) {
            lines.push(Line::from(Span::styled(
                segment,
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
    }
    if let Some(message) = app.status_message.as_deref() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(palette::STATUS_WARNING),
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::views::action_footer_lines;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    #[test]
    fn prompt_names_the_workspace_boundary_and_effects() {
        let options = TuiOptions {
            model: "test-model".to_string(),
            ..crate::tui::test_support::test_tui_options(PathBuf::from("workspace-fixture"))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        let body = lines(&app, 70)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        // The prose wraps, so a phrase can straddle a line break. Match on
        // collapsed whitespace: this test is about which facts the screen
        // states, not about where the lane happens to break them.
        let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(flat.contains("Know this workspace"), "{body}");
        assert!(flat.contains("instructions and files"), "{body}");
        assert!(flat.contains("prompt injection"), "{body}");
        assert!(flat.contains("tools and hooks"), "{body}");
    }

    /// Trust keys stay explicit and in the action rail: Enter must never
    /// grant trust, and the three choices must each advertise their own key.
    #[test]
    fn trust_actions_are_explicit_keys_in_the_action_rail() {
        let mut app = App::new(
            TuiOptions {
                model: "test-model".to_string(),
                ..crate::tui::test_support::test_tui_options(PathBuf::from("workspace-fixture"))
            },
            &Config::default(),
        );
        app.ui_locale = codewhale_localization::Locale::En;
        app.onboarding = crate::tui::app::OnboardingState::TrustDirectory;

        let rail = super::super::action_hints(&app)
            .iter()
            .flat_map(|hint| action_footer_lines(std::slice::from_ref(hint), 60))
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join(" ");

        for expected in ["1/Y", "2/U", "3/N"] {
            assert!(
                rail.contains(expected),
                "missing {expected} in rail: {rail}"
            );
        }
        assert!(rail.contains("trust and continue"), "{rail}");
        assert!(rail.contains("continue without trusting"), "{rail}");
        assert!(rail.contains("quit Codewhale"), "{rail}");
    }
}

#[cfg(test)]
mod narrow_terminal_tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_localization::{Locale, MessageId, tr};
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    fn app_at(workspace: &str) -> App {
        let options = TuiOptions {
            model: "test-model".to_string(),
            ..crate::tui::test_support::test_tui_options(PathBuf::from(workspace))
        };
        App::new(options, &Config::default())
    }

    /// The trust screen asks for filesystem trust. Every locale's prose has to
    /// survive a small terminal intact: a question cut mid-word is not a
    /// question the reader can answer.
    #[test]
    fn every_locale_keeps_the_trust_prose_whole_at_forty_columns() {
        let mut app = app_at("/tmp/probe/ws");
        for locale in Locale::shipped().iter().copied() {
            app.ui_locale = locale;
            for width in [40usize, 60, 80, 120] {
                let rendered: Vec<String> = lines(&app, width)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect();

                for row in &rendered {
                    assert!(
                        row.width() <= width,
                        "{locale:?} at {width}: row overflows the lane: {row:?}",
                    );
                }

                // The question must be present in full, not clipped. Compare on
                // the rejoined prose so a wrap is fine and a cut is not.
                let joined = rendered.join(" ");
                let question = tr(locale, MessageId::OnboardTrustQuestion);
                // Compare with whitespace removed entirely, not collapsed.
                // Japanese and Chinese wrap between characters that have no
                // space between them in the source, so rejoining with a space
                // would make a correctly wrapped line look like a changed one.
                let normalize =
                    |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                assert!(
                    normalize(&joined).contains(&normalize(question.as_ref()))
                        || question.as_ref().chars().all(|c| c.is_whitespace()),
                    "{locale:?} at {width}: the trust question was cut.\nwanted: {question}\ngot: {joined}",
                );
            }
        }
    }
}
