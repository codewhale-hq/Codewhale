//! Startup gate for the `[redaction] model_bound = "disabled"` opt-out.
//!
//! Setting `[redaction] model_bound = "disabled"` in `config.toml` only
//! records a request. Lowering the model-bound masking boundary is a security
//! decision, so the interactive TUI shows this full-screen gate on the next
//! launch and only applies the opt-out after the user confirms it here (see
//! [`codewhale_config::redaction`] for the effective-mode contract).
//!
//! The gate follows the onboarding visual grammar — one Underwater surface,
//! one bottom action rail — but it is **not** an onboarding step: returning
//! users see it, and answering it never touches the `.onboarded` marker. The
//! three actions mirror the workspace-trust screen's explicit-key discipline:
//! Enter never confirms by reflex, and each choice advertises its own key.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::tui::app::{App, RedactionGateNotice, StatusToastKind};
use crate::tui::onboarding::wrap_words;
use crate::tui::shell_key_routing::{ShellBindingId, binding};
use crate::tui::views::{ActionHint, render_modal_footer, render_underwater_surface};
use codewhale_localization::MessageId;
use codewhale_palette as palette;

/// Render the gate. Callers (the frame compositor) invoke this only while
/// `app.redaction_gate` is set. The gate has two stages: the first stage
/// explains the opt-out and its risk; pressing the confirm key moves to the
/// second, final-confirmation stage (`app.redaction_gate_confirming`), which
/// repeats the red warning and requires a second explicit confirm before the
/// opt-out is recorded.
pub fn render(f: &mut Frame, area: Rect, app: &App) {
    let title = if app.redaction_gate_confirming {
        app.tr(MessageId::RedactionGateConfirmTitle).into_owned()
    } else {
        app.tr(MessageId::RedactionGateTitle).into_owned()
    };
    let mut hints = action_hints(app);
    hints.push(ActionHint::new(
        binding(ShellBindingId::RedactionGateScroll).footer_chord,
        app.tr(MessageId::SetupActionScrollBody).to_string(),
    ));
    let buf = f.buffer_mut();
    let inner = render_underwater_surface(area, buf, &title);
    let content = render_modal_footer(inner, buf, &hints);
    let lines = screen_lines(app, usize::from(content.width), usize::from(content.height));
    if lines.is_empty() {
        return;
    }
    let body = center_vertically(content, lines.len());
    f.render_widget(Paragraph::new(lines), body);
}

fn center_vertically(area: Rect, rows: usize) -> Rect {
    let pad = (area
        .height
        .saturating_sub(u16::try_from(rows).unwrap_or(area.height)))
        / 2;
    Rect {
        y: area.y.saturating_add(pad),
        height: area.height.saturating_sub(pad),
        ..area
    }
}

fn action_hints(app: &App) -> Vec<ActionHint> {
    [
        (
            ShellBindingId::RedactionGateConfirm,
            MessageId::RedactionGateActionConfirm,
        ),
        (
            ShellBindingId::RedactionGateKeepOrBack,
            if app.redaction_gate_confirming {
                MessageId::RedactionGateActionBack
            } else {
                MessageId::RedactionGateActionKeep
            },
        ),
        (
            ShellBindingId::RedactionGateQuit,
            MessageId::RedactionGateActionQuit,
        ),
    ]
    .into_iter()
    .map(|(id, label)| ActionHint::new(binding(id).footer_chord, app.tr(label).to_string()))
    .collect()
}

fn screen_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let now = std::time::Instant::now();
    if let Some(toast) = [
        RedactionGateNotice::WriteFailure,
        RedactionGateNotice::EnterGuidance,
    ]
    .into_iter()
    .find_map(|notice| {
        app.status_toasts.iter().rev().find(|toast| {
            toast.kind == StatusToastKind::RedactionGate(notice) && !toast.is_expired(now)
        })
    }) {
        for line in wrap_words(&toast.text, width) {
            out.push(Line::from(Span::styled(
                line,
                Style::default().fg(toast.level.ink().color(&app.ui_theme)),
            )));
        }
        out.push(Line::from(""));
    }
    // Put the consequence first even when the rest needs scrolling.
    wrap_body_danger(&mut out, app, MessageId::RedactionGateDangerNotice, width);
    out.push(Line::from(""));
    if app.redaction_gate_confirming {
        wrap_body(
            &mut out,
            app,
            MessageId::RedactionGateConfirmQuestion,
            width,
        );
    } else {
        wrap_body(&mut out, app, MessageId::RedactionGateQuestion, width);
        out.push(Line::from(""));
        wrap_body_muted(&mut out, app, MessageId::RedactionGateRisk, width);
        wrap_body_muted(&mut out, app, MessageId::RedactionGateEffect, width);
        wrap_body_muted(&mut out, app, MessageId::RedactionGateRollbackHint, width);
    }
    let scroll = app
        .redaction_gate_scroll
        .get()
        .min(out.len().saturating_sub(height));
    app.redaction_gate_scroll.set(scroll);
    out.into_iter().skip(scroll).take(height).collect()
}

/// Body sentence in the primary lane.
fn wrap_body(lines: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let text = app.tr(id);
    for segment in wrap_words(&text, width) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
}

/// The red, bold warning shown on both gate stages. Wrap on display width
/// exactly like the other lanes so no locale clips mid-word.
fn wrap_body_danger(lines: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let text = app.tr(id);
    for segment in wrap_words(&text, width) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default()
                .fg(palette::STATUS_ERROR)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )));
    }
}

/// Supporting hint in the muted lane.
fn wrap_body_muted(lines: &mut Vec<Line<'static>>, app: &App, id: MessageId, width: usize) {
    let text = app.tr(id);
    for segment in wrap_words(&text, width) {
        lines.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_MUTED),
        )));
    }
}

// The "keep masking" answer persists nothing and rewrites no file: the
// current launch stays on the safe default, and because the config field
// still requests `"disabled"`, the gate asks again on the next launch until
// the user confirms or edits the field back to `"enabled"`. The event loop
// implements this inline (it only clears the gate flag); this module-level
// contract comment is where the semantics live.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use crate::tui::views::action_footer_lines;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn app_fixture() -> App {
        let options = TuiOptions {
            model: "test-model".to_string(),
            ..crate::tui::test_support::test_tui_options(PathBuf::from("workspace-fixture"))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        app.redaction_gate = true;
        app
    }

    #[test]
    fn gate_names_the_boundary_and_the_three_explicit_actions() {
        let app = app_fixture();
        let body = screen_lines(&app, 70, 24)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("model-bound"), "{body}");
        assert!(flat.contains("API keys"), "{body}");

        let rail = action_hints(&app)
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
        assert!(rail.contains("confirm"), "{rail}");
        assert!(rail.contains("keep"), "{rail}");
        assert!(rail.contains("quit"), "{rail}");
    }

    #[test]
    fn gate_renders_without_panicking_on_short_screens() {
        // The gate must survive very narrow terminals without clipping the
        // question (see the trust screen's narrow-terminal discipline).
        for width in [40usize, 60, 80, 120] {
            for locale in [
                codewhale_localization::Locale::En,
                codewhale_localization::Locale::ZhHans,
            ] {
                let mut app = app_fixture();
                app.ui_locale = locale;
                let _ = screen_lines(&app, width, 24);
                // Both stages must survive the same narrow lanes.
                app.redaction_gate_confirming = true;
                let _ = screen_lines(&app, width, 24);
            }
        }
    }

    #[test]
    fn gate_scroll_reaches_every_body_line_with_actions_visible_in_all_locales() {
        use ratatui::{
            Terminal,
            backend::TestBackend,
            buffer::{Buffer, CellWidth},
        };
        use std::collections::HashSet;
        let visible_row = |buffer: &Buffer, area: Rect, y| {
            let mut row = String::new();
            let mut x = area.x;
            while x < area.right() {
                let cell = &buffer[(x, y)];
                row.push_str(cell.symbol());
                // TestBackend retains hidden cells beneath wide characters
                // between draws. Read the terminal-visible graphemes only.
                x += cell.cell_width().max(1);
            }
            row
        };
        let compact = |text: &str| {
            text.chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        };
        for &(width, height) in &[(40, 12), (60, 16), (80, 24)] {
            for &locale in codewhale_localization::Locale::shipped() {
                for confirming in [false, true] {
                    let mut app = app_fixture();
                    app.ui_locale = locale;
                    app.redaction_gate_confirming = confirming;
                    let area = Rect::new(0, 0, width, height);
                    let mut buffer = Buffer::empty(area);
                    let inner = render_underwater_surface(area, &mut buffer, "");
                    let mut hints = action_hints(&app);
                    hints.push(ActionHint::new(
                        "↑/↓",
                        app.tr(MessageId::SetupActionScrollBody).to_string(),
                    ));
                    let content = render_modal_footer(inner, &mut buffer, &hints);
                    assert!(
                        content.height > 0,
                        "no reading space at {width}x{height}, {locale:?}"
                    );
                    let expected = screen_lines(&app, content.width as usize, usize::MAX);
                    let mut seen = HashSet::new();
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    for scroll in 0..=expected.len() {
                        app.redaction_gate_scroll.set(scroll);
                        let rendered = terminal.draw(|frame| render(frame, area, &app)).unwrap();
                        let buffer = rendered.buffer;
                        let mut whole = String::new();
                        for y in 0..height {
                            let row = visible_row(buffer, area, y);
                            whole.push_str(&row);
                        }
                        for key in ["1/Y", "2/U", "3/N", "↑/↓"] {
                            assert!(
                                whole.contains(key),
                                "missing action {key} at {width}x{height}, {locale:?}"
                            );
                        }
                        for y in content.y..content.y + content.height {
                            let row = visible_row(buffer, content, y);
                            seen.insert(compact(&row));
                        }
                    }
                    for line in expected {
                        let text = line
                            .spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>();
                        assert!(
                            unicode_width::UnicodeWidthStr::width(text.as_str())
                                <= usize::from(content.width),
                            "overflow at {width}x{height}, {locale:?}: {text}"
                        );
                        assert!(
                            seen.contains(&compact(&text)),
                            "unreachable or clipped body at {width}x{height}, {locale:?}, stage {confirming}: {text}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn gate_notice_survives_unrelated_status_refresh_and_receipt_failure_keeps_masking() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected.toml");
        std::fs::write(&path, "[redaction]\nmodel_bound = \"disabled\"\n").unwrap();
        let config = codewhale_plugins::providers::load_config(Some(path.clone()), None).unwrap();
        // An existing directory makes the write fail on every supported OS.
        std::fs::create_dir(codewhale_config::redaction::model_bound_state_path(&path)).unwrap();
        assert!(config.record_redaction_confirmation().is_err());
        assert!(config.redaction_confirmation_required());
        let mut app = app_fixture();
        let notice = app.tr(MessageId::RedactionGateSaveFailed).into_owned();
        app.push_status_toast_record(
            crate::tui::app::StatusToast::new(
                notice.clone(),
                crate::tui::app::StatusToastLevel::Error,
                None,
            )
            .for_redaction_gate(RedactionGateNotice::WriteFailure),
        );
        let hint = app.tr(MessageId::RedactionGateEnterHint).into_owned();
        app.push_status_toast_record(
            crate::tui::app::StatusToast::new(
                hint.clone(),
                crate::tui::app::StatusToastLevel::Info,
                Some(12_000),
            )
            .for_redaction_gate(RedactionGateNotice::EnterGuidance),
        );
        app.push_status_toast(
            "Unrelated runtime error",
            crate::tui::app::StatusToastLevel::Error,
            None,
        );
        app.status_message = Some("Unrelated runtime status".to_string());
        let lines = screen_lines(&app, 38, 6)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(lines.contains("Could not save confirmation"));
        assert!(!lines.contains("Unrelated runtime status"));
        assert!(!lines.contains("Unrelated runtime error"));
        assert!(!lines.contains(&hint));
        assert_eq!(
            screen_lines(&app, 38, 6)[0].spans[0].style.fg,
            Some(app.ui_theme.error_fg)
        );
        assert!(app.redaction_gate);
        assert_eq!(
            codewhale_config::redaction::effective_masking(
                config.model_bound_redaction(),
                config.loaded_config_path.as_deref()
            ),
            codewhale_config::redaction::ModelBoundMasking::Enabled
        );
        app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
        assert!(screen_lines(&app, 100, 24)[0].to_string().contains(&notice));
        app.retire_redaction_gate_notice(RedactionGateNotice::WriteFailure);
        let remaining = screen_lines(&app, 100, 24)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!remaining.contains(&notice));
        assert!(!remaining.contains("Unrelated runtime error"));
        assert_eq!(app.status_toasts.len(), 1);
        assert_eq!(app.status_toasts[0].text, "Unrelated runtime error");
    }

    /// The red warning is part of both stages, and the second stage swaps the
    /// "keep" action for a "back" action: you can only move forward with an
    /// explicit second confirm.
    #[test]
    fn both_stages_show_the_danger_warning_and_second_stage_offers_back() {
        let first = app_fixture();
        let first_body = screen_lines(&first, 70, 24)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(first_body.contains("Caution"), "{first_body}");

        let mut confirming = app_fixture();
        confirming.redaction_gate_confirming = true;
        let confirm_body = screen_lines(&confirming, 70, 24)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(confirm_body.contains("really sure"), "{confirm_body}");
        assert!(confirm_body.contains("Caution"), "{confirm_body}");

        let rail = action_hints(&confirming)
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
        assert!(rail.contains("back"), "{rail}");
        assert!(
            !rail.contains("keep"),
            "second stage must not offer keep: {rail}"
        );
        assert!(rail.contains("quit"), "{rail}");
    }
}
