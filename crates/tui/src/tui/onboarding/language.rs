//! Language picker for first-run onboarding (#566).
//!
//! Surfaces every locale the TUI ships translations for, plus an `auto`
//! option that defers to `LC_ALL` / `LANG`. Selection persists via
//! `Settings::save` immediately so the rest of onboarding (and every
//! subsequent session) reads the chosen tag.
//!
//! The screen appears only when the locale cannot be confidently inferred
//! (see `onboarding::locale_confidently_inferred`); most first runs never
//! see it.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::app::App;
use codewhale_localization::MessageId;
use codewhale_palette as palette;
use unicode_width::UnicodeWidthStr;

/// Locale options shown in the picker. Order matches the keyboard hotkeys.
/// Each entry is `(hotkey, settings_tag, native_name, english_label)`.
/// `settings_tag` is what `Settings::set("locale", …)` accepts and what
/// `localization::Locale` resolves on next read.
///
/// Hotkeys run `1..=9` then `a`, `b`, … so more than nine shipped locales
/// stay single-keystroke selectable.
pub const LANGUAGE_OPTIONS: &[(char, &str, &str, &str)] = &[
    ('1', "auto", "Auto-detect", "(LC_ALL / LANG)"),
    ('2', "en", "English", ""),
    ('3', "ja", "日本語", "(Japanese)"),
    ('4', "zh-Hans", "简体中文", "(Simplified Chinese)"),
    ('5', "zh-Hant", "繁體中文", "(Traditional Chinese)"),
    ('6', "pt-BR", "Português (Brasil)", "(Brazilian Portuguese)"),
    (
        '7',
        "es-419",
        "Español (Latinoamérica)",
        "(Latin American Spanish)",
    ),
    ('8', "vi", "Tiếng Việt", "(Vietnamese)"),
    ('9', "ko", "한국어", "(Korean)"),
    ('a', "ca", "Català", "(Catalan)"),
    ('b', "de", "Deutsch", "(German)"),
    ('c', "fr", "Français", "(French)"),
    ('d', "id", "Bahasa Indonesia", "(Indonesian)"),
    ('e', "hi", "हिन्दी", "(Hindi)"),
    ('f', "ru", "Русский", "(Russian)"),
    ('g', "uk", "Українська", "(Ukrainian)"),
];

/// Two columns keep every shipped locale visible at 80x24; below this width
/// the list falls back to one column and the English annotations return.
const TWO_COLUMN_MIN_WIDTH: usize = 56;

pub fn lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    let current_owned = app.current_locale_tag();
    let current = current_owned.as_str();

    let title = Line::from(Span::styled(
        app.tr(MessageId::OnboardLanguageTitle).to_string(),
        Style::default()
            .fg(palette::WHALE_ACTION)
            .add_modifier(Modifier::BOLD),
    ));

    // The action rail can leave only five body rows at 40x12. A compact grid
    // spends one on the title and derives enough columns to keep every
    // selectable hotkey visible in the remaining rows. Wider/taller terminals
    // keep the native names and explanatory sentence.
    if height < 12 {
        let option_rows = height.saturating_sub(1).max(1);
        return compact_grid(title, width, option_rows, current);
    }

    let mut out: Vec<Line<'static>> = vec![title, Line::from("")];
    for segment in super::wrap_words(&app.tr(MessageId::OnboardLanguageBlurb), width) {
        out.push(Line::from(Span::styled(
            segment,
            Style::default().fg(palette::TEXT_PRIMARY),
        )));
    }
    out.push(Line::from(""));

    let two_column = width >= TWO_COLUMN_MIN_WIDTH;
    if two_column {
        let column_width = (width - 3) / 2;
        let split = LANGUAGE_OPTIONS.len().div_ceil(2);
        let (left_column, right_column) = LANGUAGE_OPTIONS.split_at(split);
        for (idx, left) in left_column.iter().enumerate() {
            let mut spans = option_spans(left, current);
            if let Some(right) = right_column.get(idx) {
                let used: usize = spans
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                    .sum();
                let gap = (column_width + 1).saturating_sub(used).max(2);
                spans.push(Span::raw(" ".repeat(gap)));
                spans.extend(option_spans(right, current));
            }
            out.push(Line::from(spans));
        }
    } else {
        for option in LANGUAGE_OPTIONS {
            out.push(Line::from(option_spans_with_english(option, current)));
        }
    }

    out
}

fn compact_grid(
    title: Line<'static>,
    width: usize,
    rows: usize,
    current: &str,
) -> Vec<Line<'static>> {
    let columns = LANGUAGE_OPTIONS.len().div_ceil(rows);
    let gap = usize::from(columns > 1);
    let column_width = width
        .saturating_sub(gap.saturating_mul(columns.saturating_sub(1)))
        .checked_div(columns)
        .unwrap_or(1)
        .max(1);
    let mut out = vec![title];
    for row in 0..rows {
        let mut spans = Vec::new();
        for column in 0..columns {
            let index = row * columns + column;
            let Some(option) = LANGUAGE_OPTIONS.get(index) else {
                break;
            };
            if column > 0 {
                spans.push(Span::raw(" ".repeat(gap)));
            }
            spans.extend(compact_option_spans(option, current, column_width));
        }
        out.push(Line::from(spans));
    }
    out
}

fn compact_option_spans(
    option: &(char, &str, &str, &str),
    current: &str,
    width: usize,
) -> Vec<Span<'static>> {
    let (hotkey, tag, native, _) = *option;
    let prefix = format!("[{hotkey}]");
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let label_width = width.saturating_sub(prefix_width);
    let label = codewhale_tui_kit::ui_text::semantic_truncate(native, label_width);
    let style = if current == tag {
        Style::default()
            .fg(palette::WHALE_ACTION)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette::TEXT_PRIMARY)
    };
    let used = prefix_width + UnicodeWidthStr::width(label.as_str());
    vec![
        Span::styled(prefix, style),
        Span::styled(label, style),
        Span::raw(" ".repeat(width.saturating_sub(used))),
    ]
}

fn option_spans(option: &(char, &str, &str, &str), current: &str) -> Vec<Span<'static>> {
    option_spans_inner(option, current, false)
}

fn option_spans_with_english(
    option: &(char, &str, &str, &str),
    current: &str,
) -> Vec<Span<'static>> {
    option_spans_inner(option, current, true)
}

fn option_spans_inner(
    option: &(char, &str, &str, &str),
    current: &str,
    with_english: bool,
) -> Vec<Span<'static>> {
    let (hotkey, tag, native, english) = *option;
    let is_current = current == tag;
    let bullet = if is_current {
        codewhale_tui_kit::glyphs::CURRENT
    } else {
        codewhale_tui_kit::glyphs::AVAILABLE
    };
    let bullet_color = if is_current {
        palette::WHALE_ACTION
    } else {
        palette::TEXT_MUTED
    };
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(format!("{bullet} "), Style::default().fg(bullet_color)),
        Span::styled(
            format!("[{hotkey}] "),
            Style::default()
                .fg(palette::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            native.to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        ),
    ];
    if with_english && !english.is_empty() {
        spans.push(Span::styled(
            format!(" {english}"),
            Style::default().fg(palette::TEXT_MUTED),
        ));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiOptions;
    use codewhale_localization::Locale;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = Locale::En;
        app
    }

    fn row_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    /// Every locale we ship translations for must be offered in the picker,
    /// otherwise the footer advertises hotkeys that select nothing and users
    /// can never reach a supported UI language (#3929).
    #[test]
    fn picker_offers_every_shipped_locale() {
        let offered: Vec<&str> = LANGUAGE_OPTIONS.iter().map(|(_, tag, _, _)| *tag).collect();
        assert!(
            offered.contains(&"auto"),
            "picker must keep the auto-detect entry"
        );
        for locale in Locale::shipped() {
            let tag = locale.tag();
            assert!(
                offered.contains(&tag),
                "shipped locale {tag} is not offered in the language picker"
            );
        }
    }

    /// Hotkeys must be the contiguous run `1..=9` followed by contiguous
    /// lowercase letters `a`, `b`, … so the footer hint stays truthful and
    /// `KeyCode::Char` lookups resolve for every option.
    #[test]
    fn picker_hotkeys_are_contiguous_digits_then_letters() {
        for (idx, (hotkey, tag, _, _)) in LANGUAGE_OPTIONS.iter().enumerate() {
            let expected = if idx < 9 {
                char::from_digit((idx + 1) as u32, 10).expect("digit")
            } else {
                char::from_u32('a' as u32 + (idx - 9) as u32).expect("letter")
            };
            assert_eq!(
                *hotkey, expected,
                "option {tag} should use hotkey {expected}, not {hotkey}"
            );
        }
    }

    /// At 80 columns the surface body is ~72 wide, so every option must stay
    /// on one row and the whole list must fit a 24-row terminal.
    #[test]
    fn every_option_is_visible_at_80_columns() {
        let rows = lines(&app(), 72, 17);
        let text = rows.iter().map(row_text).collect::<Vec<_>>();

        for (_, tag, native, _) in LANGUAGE_OPTIONS {
            let shown = text.iter().any(|row| row.contains(native));
            assert!(shown, "option {tag} ({native}) missing from the picker");
        }
        // 17 options in 9 rows + 5 header rows fits the ~17 usable rows of a
        // 24-line terminal under the onboarding surface.
        assert!(
            rows.len() <= 15,
            "picker is {} rows; it must fit 80x24",
            rows.len()
        );
    }

    #[test]
    fn compact_grid_keeps_every_language_hotkey_reachable() {
        let rows = lines(&app(), 36, 5);
        let text = rows.iter().map(row_text).collect::<Vec<_>>().join("\n");

        assert_eq!(rows.len(), 5, "one title plus four option rows");
        for (hotkey, tag, _, _) in LANGUAGE_OPTIONS {
            assert!(text.contains(&format!("[{hotkey}]")), "missing {tag}");
        }
    }
}
