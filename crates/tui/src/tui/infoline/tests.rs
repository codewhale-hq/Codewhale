//! Golden-buffer contract for the metrics line — the row under the posture
//! bar.
//!
//! Goldens live in `crates/tui/src/tui/goldens/infoline_{screen}_{w}x{h}.txt`
//! for the two screens (startup, work) at the blocker sizes. Re-bless by
//! deleting the golden and running with `CODEWHALE_BLESS_GOLDENS=1`.

use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use unicode_width::UnicodeWidthStr;

use super::{InfoLine, InfoSegment, InfoSegmentId, context_meter_hitbox, infoline_hitboxes};
use codewhale_palette::{ChromeInk, UI_THEME, UiTheme};

/// The hint the live shell advertises, from the one binding module that owns
/// it — a fixture string here would let chrome and routing drift apart.
fn help_hint() -> String {
    crate::tui::shell_key_routing::info_help_hint(codewhale_localization::Locale::En)
}

const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

fn context(pct: u8) -> InfoSegment {
    InfoSegment::new(
        InfoSegmentId::Context,
        "ctx",
        format!("{pct}%"),
        if pct >= 80 {
            ChromeInk::Failure
        } else {
            ChromeInk::Info
        },
    )
}

/// Approved startup screen: no route yet, no metrics yet.
fn startup_segments() -> Vec<InfoSegment> {
    vec![
        InfoSegment::new(
            InfoSegmentId::Model,
            "",
            "model not connected",
            ChromeInk::Waiting,
        ),
        context(0),
    ]
}

/// Approved work screen: model, context, cost, then the session metrics.
fn work_segments() -> Vec<InfoSegment> {
    vec![
        InfoSegment::new(InfoSegmentId::Model, "", "deepseek-v4", ChromeInk::Identity),
        context(61),
        InfoSegment::new(InfoSegmentId::Cost, "", "$0.42", ChromeInk::MetadataValue),
        InfoSegment::new(
            InfoSegmentId::Ttft,
            "ttft",
            "400ms",
            ChromeInk::MetadataValue,
        ),
        InfoSegment::new(
            InfoSegmentId::Rate,
            "",
            "38 tok/s",
            ChromeInk::MetadataValue,
        ),
        InfoSegment::new(
            InfoSegmentId::OutputTokens,
            "↓",
            "1.2K",
            ChromeInk::MetadataValue,
        ),
    ]
}

fn fixtures() -> Vec<(&'static str, Vec<InfoSegment>)> {
    vec![("startup", startup_segments()), ("work", work_segments())]
}

fn render_buffer(theme: &UiTheme, width: u16, segments: &[InfoSegment]) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let hint = help_hint();
    terminal
        .draw(|frame| {
            let info = InfoLine::new(theme, &hint, segments);
            use ratatui::widgets::Widget;
            Widget::render(info, frame.area(), frame.buffer_mut());
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_row(theme: &UiTheme, width: u16, segments: &[InfoSegment]) -> String {
    render_cells(theme, width, segments).concat()
}

/// Per-cell symbols of one rendered row (the golden dump, before joining).
fn render_cells(theme: &UiTheme, width: u16, segments: &[InfoSegment]) -> Vec<String> {
    render_buffer(theme, width, segments)
        .content()
        .iter()
        .map(|cell| cell.symbol().to_string())
        .collect()
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tui/goldens")
        .join(format!("{name}.txt"))
}

fn bless(name: &str, text: &str) {
    let path = golden_path(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create goldens dir");
    }
    std::fs::write(path, text).expect("write golden");
}

fn golden_text(name: &str) -> Option<String> {
    // Normalize to LF; a Windows checkout can hand us CRLF while `render_row`
    // always terminates with LF. Cell symbols never contain CR.
    std::fs::read_to_string(golden_path(name))
        .ok()
        .map(|text| text.replace("\r\n", "\n"))
}

#[test]
fn infoline_matches_goldens_at_blocker_sizes() {
    for (screen, segments) in fixtures() {
        for (w, h) in BLOCKER_SIZES {
            let name = format!("infoline_{screen}_{w}x{h}");
            let rendered = render_row(&UI_THEME, w, &segments);
            let rendered = format!("{rendered}\n");
            match golden_text(&name) {
                Some(expected) => {
                    assert_eq!(
                        rendered, expected,
                        "info-line golden drift at {name}; re-bless only with an approved design change"
                    );
                }
                None => {
                    if std::env::var("CODEWHALE_BLESS_GOLDENS").is_ok() {
                        bless(&name, &rendered);
                    } else {
                        panic!(
                            "missing golden {name}; run with CODEWHALE_BLESS_GOLDENS=1 to write it"
                        );
                    }
                }
            }
        }
    }
}

/// The row states no time of day, carries no wordmark, and no longer names
/// the repository or branch: the launch header and the git bottom view own
/// those (2026-09-02).
#[test]
fn infoline_is_model_context_and_metrics_only() {
    for (_, segments) in fixtures() {
        for (w, _h) in BLOCKER_SIZES {
            let row = render_row(&UI_THEME, w, &segments);
            assert!(
                !row.contains(':'),
                "{w}: the metrics line carries no clock: {row:?}"
            );
            assert!(
                !row.contains("CODEWHALE") && !row.contains("codewhale"),
                "{w}: no wordmark or repository on this row: {row:?}"
            );
            assert!(!row.contains('⑂'), "{w}: no branch on this row: {row:?}");
        }
    }
    let work = render_row(&UI_THEME, 160, &work_segments());
    assert!(
        work.starts_with("deepseek-v4   ctx 61%   $0.42   ttft 400ms   38 tok/s   ↓ 1.2K  "),
        "{work:?}"
    );
    assert!(work.trim_end().ends_with("/help"), "{work:?}");
}

/// Secondary counts and help yield before performance readings and cost. The model and `ctx NN%` are the floor at every width.
#[test]
fn infoline_sheds_tokens_then_help_then_rate_then_ttft_then_cost() {
    let segments = work_segments();
    // The narrowest row that still shows a thing. A thing that sheds earlier
    // needs a wider row to survive, so these strictly decrease down the
    // declared order.
    let narrowest_showing = |needle: &str| -> u16 {
        (24..=180u16)
            .filter(|w| render_row(&UI_THEME, *w, &segments).contains(needle))
            .min()
            .unwrap_or_else(|| panic!("{needle} never painted at any width"))
    };
    let rate = narrowest_showing("tok/s");
    let ttft = narrowest_showing("ttft");
    let tokens = narrowest_showing("↓ 1.2K");
    let help = narrowest_showing("help");
    let cost = narrowest_showing("$0.42");
    assert!(
        tokens > help && help > rate && rate > ttft && ttft > cost,
        "shed order broke: rate@{rate} ttft@{ttft} tokens@{tokens} help@{help} cost@{cost}"
    );
    for w in 24..=180u16 {
        let row = render_row(&UI_THEME, w, &segments);
        assert!(
            row.contains("deepseek-v4") && row.contains("ctx 61%"),
            "{w}: the model and the context reading never shed: {row:?}"
        );
    }
}

/// `tui.metrics_line = "compact"` (#5950) is the row after its first shed
/// rungs, at any width: output counts and help are gone before width is
/// consulted; selected TTFT/rate survive when they fit. Hitboxes follow the same
/// pass so a click still lands on what painted.
#[test]
fn infoline_compact_keeps_performance_readings_without_extra_rows() {
    let segments = work_segments();
    let hint = help_hint();
    let compact_row = |width: u16| -> (String, Vec<InfoSegmentId>) {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut ids = Vec::new();
        terminal
            .draw(|frame| {
                let info = InfoLine::new(&UI_THEME, &hint, &segments).compact(true);
                ids = infoline_hitboxes(&info, frame.area())
                    .into_iter()
                    .map(|hitbox| hitbox.id)
                    .collect();
                use ratatui::widgets::Widget;
                Widget::render(info, frame.area(), frame.buffer_mut());
            })
            .expect("draw");
        let row = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect::<String>();
        (row, ids)
    };
    let (wide, ids) = compact_row(160);
    assert_eq!(
        wide.trim_end(),
        "deepseek-v4   ctx 61%   $0.42   ttft 400ms   38 tok/s",
        "compact keeps performance, route, context and price: {wide:?}"
    );
    assert_eq!(
        ids,
        vec![
            InfoSegmentId::Model,
            InfoSegmentId::Context,
            InfoSegmentId::Cost,
            InfoSegmentId::Ttft,
            InfoSegmentId::Rate,
        ]
    );
    for w in 24..=180u16 {
        let (row, _) = compact_row(w);
        for gone in ["1.2K", "help"] {
            assert!(
                !row.contains(gone),
                "{w}: compact never paints {gone}: {row:?}"
            );
        }
        assert!(
            row.contains("deepseek-v4") && row.contains("ctx 61%"),
            "{w}: the floor still never sheds: {row:?}"
        );
    }
    // The full row at the same width is the row the user had before.
    assert!(render_row(&UI_THEME, 160, &segments).contains("tok/s"));

    // Cache survives compact and is the first performance reading to shed
    // when the row is narrow (#6565).
    let mut with_cache = segments.clone();
    with_cache.insert(
        3,
        InfoSegment::new(
            InfoSegmentId::Cache,
            "cache",
            "85%",
            ChromeInk::MetadataValue,
        ),
    );
    let wide_cache = render_row_compact(160, &with_cache);
    assert!(wide_cache.contains("cache 85%"), "{wide_cache:?}");
    assert!(
        !wide_cache.contains("1.2K") && !wide_cache.contains("help"),
        "{wide_cache:?}"
    );
    let narrow_cache = render_row_compact(40, &with_cache);
    assert!(!narrow_cache.contains("cache"), "{narrow_cache:?}");
    assert!(
        narrow_cache.contains("deepseek-v4") && narrow_cache.contains("ctx 61%"),
        "{narrow_cache:?}"
    );
}

fn render_row_compact(width: u16, segments: &[InfoSegment]) -> String {
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let hint = help_hint();
            let info = InfoLine::new(&UI_THEME, &hint, segments).compact(true);
            use ratatui::widgets::Widget;
            Widget::render(info, frame.area(), frame.buffer_mut());
        })
        .expect("draw");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol().to_string())
        .collect()
}

/// At the 80% cap the context reading takes the error token — the caller
/// picks the ink, and the row paints it on both the label and the value.
#[test]
fn infoline_context_takes_the_error_token_at_eighty() {
    let theme = &UI_THEME;
    let failure = codewhale_palette::grammar::chrome_style(theme, ChromeInk::Failure)
        .fg
        .expect("failure ink has a colour");
    for (pct, expect_failure) in [(79u8, false), (80, true), (99, true)] {
        let segments = vec![
            InfoSegment::new(InfoSegmentId::Model, "", "deepseek-v4", ChromeInk::Identity),
            context(pct),
        ];
        let buf = render_buffer(theme, 80, &segments);
        let row = render_row(theme, 80, &segments);
        let start = row.find("ctx").expect("context reading painted");
        let value_fg = buf[(u16::try_from(start + 4).unwrap(), 0)].fg;
        let label_fg = buf[(u16::try_from(start).unwrap(), 0)].fg;
        assert_eq!(value_fg == failure, expect_failure, "{pct}%: value ink");
        assert_eq!(label_fg == failure, expect_failure, "{pct}%: label ink");
    }
}

/// The hint must name a route that actually opens help in this shell. `F1`
/// is eaten by tmux and several emulators, bare `?` is composer text, and how
/// a terminal encodes `Ctrl+/` varies enough that printing it was a promise
/// the product could not keep. `/help` reaches the same view through the
/// composer in every terminal.
#[test]
fn infoline_help_hint_names_a_route_that_opens_help() {
    let hint = help_hint();
    assert_eq!(hint, "/help", "a slash command names itself: {hint}");
    assert!(!hint.contains("F1"), "terminals eat F1: {hint}");
    assert!(!hint.starts_with('?'), "bare ? is composer text: {hint}");
    // The chord stays accepted for the terminals that do deliver it; it is
    // only no longer what chrome promises.
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('/'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    assert!(crate::tui::shell_key_routing::is_help_shortcut(&key));
    let row = render_row(&UI_THEME, 120, &work_segments());
    assert!(row.trim_end().ends_with(&hint), "pinned right: {row:?}");
}

/// Every recorded hitbox covers exactly the cells its segment painted, at
/// every width — the hitbox pass and the paint pass share one shed pass.
#[test]
fn infoline_hitboxes_match_painted_cells() {
    let segments = work_segments();
    let hint = help_hint();
    for w in 24..=180u16 {
        let area = Rect::new(0, 0, w, 1);
        let info = InfoLine::new(&UI_THEME, &hint, &segments);
        let hitboxes = infoline_hitboxes(&info, area);
        let cells = render_cells(&UI_THEME, w, &segments);
        for hitbox in &hitboxes {
            let segment = segments.iter().find(|s| s.id == hitbox.id).unwrap();
            let painted: String = cells
                [usize::from(hitbox.area.x)..usize::from(hitbox.area.x + hitbox.area.width)]
                .concat();
            let expected = if segment.label.is_empty() {
                segment.value.clone()
            } else {
                format!("{} {}", segment.label, segment.value)
            };
            assert!(
                expected.starts_with(painted.trim_end()),
                "{w}: {:?} hitbox {:?} covers {painted:?}, expected {expected:?}",
                hitbox.id,
                hitbox.area
            );
        }
        // No two hitboxes overlap.
        for (i, a) in hitboxes.iter().enumerate() {
            for b in &hitboxes[i + 1..] {
                assert!(
                    a.area.right() <= b.area.x || b.area.right() <= a.area.x,
                    "{w}: hitboxes overlap: {a:?} {b:?}"
                );
            }
        }
    }
}

/// The context reading's hitbox is exactly the painted `ctx NN%` span.
#[test]
fn context_meter_hitbox_covers_exactly_the_painted_reading() {
    let segments = work_segments();
    let hint = help_hint();
    for w in 24..=180u16 {
        let area = Rect::new(0, 0, w, 1);
        let info = InfoLine::new(&UI_THEME, &hint, &segments);
        let hitbox = context_meter_hitbox(&info, area).expect("the reading never sheds");
        let cells = render_cells(&UI_THEME, w, &segments);
        let painted: String =
            cells[usize::from(hitbox.x)..usize::from(hitbox.x + hitbox.width)].concat();
        assert!(
            "ctx 61%".starts_with(painted.trim_end()),
            "{w}: context hitbox {hitbox:?} covers {painted:?}"
        );
    }
}

/// ASCII-safe mode projects every glyph to a single-width ASCII cell.
#[test]
fn infoline_ascii_safe_has_no_wide_or_unsupported_glyphs() {
    let segments = work_segments();
    let hint = help_hint();
    for (w, _) in BLOCKER_SIZES {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let info = InfoLine::new(&UI_THEME, &hint, &segments).ascii_safe(true);
        ratatui::widgets::Widget::render(info, area, &mut buf);
        for x in 0..w {
            let symbol = buf[(x, 0)].symbol();
            assert!(symbol.is_ascii(), "{w}: cell {x} {symbol:?} is not ASCII");
            assert_eq!(
                symbol.width(),
                1,
                "{w}: cell {x} {symbol:?} is not one cell"
            );
        }
    }
}

/// Hover and degenerate sizes never panic, and hover only brightens the
/// model — the one segment with an action.
#[test]
fn infoline_hover_and_narrow_do_not_panic() {
    let segments = work_segments();
    let hint = help_hint();
    for (w, h) in [(0u16, 0u16), (1, 1), (5, 1), (24, 1), (300, 1)] {
        let area = Rect::new(0, 0, w, h);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let info = InfoLine::new(&UI_THEME, &hint, &segments).hovered(Some(InfoSegmentId::Model));
        ratatui::widgets::Widget::render(info, area, &mut buf);
        let info = InfoLine::new(&UI_THEME, &hint, &segments);
        let _ = infoline_hitboxes(&info, area);
        let _ = context_meter_hitbox(&info, area);
    }
    let area = Rect::new(0, 0, 120, 1);
    let mut plain = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(InfoLine::new(&UI_THEME, &hint, &segments), area, &mut plain);
    let mut hovered = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(
        InfoLine::new(&UI_THEME, &hint, &segments).hovered(Some(InfoSegmentId::Model)),
        area,
        &mut hovered,
    );
    assert_ne!(plain[(0, 0)].modifier, hovered[(0, 0)].modifier);
    let ctx_x = u16::try_from(render_row(&UI_THEME, 120, &segments).find("ctx").unwrap()).unwrap();
    assert_eq!(plain[(ctx_x, 0)], hovered[(ctx_x, 0)]);
}

/// Slice G: the context reading owns the inspector click action, so it
/// brightens on hover exactly like the model segment; status-only facts
/// (cost) never do.
#[test]
fn infoline_context_hover_brightens_only_the_context_reading() {
    let segments = work_segments();
    let hint = help_hint();
    let area = Rect::new(0, 0, 120, 1);
    let mut plain = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(InfoLine::new(&UI_THEME, &hint, &segments), area, &mut plain);
    let mut hovered = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(
        InfoLine::new(&UI_THEME, &hint, &segments).hovered(Some(InfoSegmentId::Context)),
        area,
        &mut hovered,
    );
    let row = render_row(&UI_THEME, 120, &segments);
    // Hover feedback lands on the value cells (`61%`); the dim label prefix
    // (`ctx`) keeps its reading ink, mirroring the model segment's probe.
    let ctx_x = u16::try_from(row.find("61%").unwrap()).unwrap();
    assert_ne!(
        plain[(ctx_x, 0)].modifier,
        hovered[(ctx_x, 0)].modifier,
        "hovered context reading must respond visibly"
    );
    // Model (actionable but not hovered) and cost (status-only) stay clean.
    assert_eq!(plain[(0, 0)], hovered[(0, 0)]);
    let cost_x = u16::try_from(row.find("$0.42").unwrap()).unwrap();
    assert_eq!(plain[(cost_x, 0)], hovered[(cost_x, 0)]);
}

#[test]
fn infoline_preserves_every_live_chrome_ink_and_untouched_host_style() {
    use ratatui::style::{Color, Modifier, Style};
    let theme = UiTheme {
        status_working: Color::Rgb(1, 2, 3),
        permission_ask: Color::Rgb(2, 3, 4),
        permission_auto_review: Color::Rgb(3, 4, 5),
        permission_full_access: Color::Rgb(4, 5, 6),
        accent_action: Color::Rgb(5, 6, 7),
        warning: Color::Rgb(6, 7, 8),
        accent_primary: Color::Rgb(7, 8, 9),
        text_soft: Color::Rgb(8, 9, 10),
        text_muted: Color::Rgb(9, 10, 11),
        text_hint: Color::Rgb(10, 11, 12),
        text_dim: Color::Rgb(11, 12, 13),
        error_fg: Color::Rgb(12, 13, 14),
        ..UI_THEME
    };
    for ink in [
        ChromeInk::Outcome,
        ChromeInk::PermissionAsk,
        ChromeInk::PermissionAutoReview,
        ChromeInk::PermissionFullAccess,
        ChromeInk::Waiting,
        ChromeInk::Attention,
        ChromeInk::Active,
        ChromeInk::PolicyAct,
        ChromeInk::PolicyPlan,
        ChromeInk::PolicyOperate,
        ChromeInk::Identity,
        ChromeInk::Info,
        ChromeInk::MetadataValue,
        ChromeInk::Metadata,
        ChromeInk::MetadataHint,
        ChromeInk::MetadataDim,
        ChromeInk::Failure,
    ] {
        let segments = [InfoSegment::new(
            InfoSegmentId::Model,
            "reading",
            "value",
            ink,
        )];
        let area = Rect::new(3, 2, 32, 2);
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 40, 5));
        let existing = Style::default()
            .fg(super::source_theme(false)
                .color(codewhale_ratatui::Role::Accent)
                .unwrap())
            .bg(Color::Rgb(30, 40, 50))
            .add_modifier(Modifier::ITALIC);
        buf.set_style(buf.area, existing);
        let before = buf.clone();
        let info = InfoLine::new(&theme, "/help", &segments).hovered(Some(InfoSegmentId::Model));
        ratatui::widgets::Widget::render(info, area, &mut buf);
        let label_ink = if matches!(ink, ChromeInk::Failure | ChromeInk::Attention) {
            ink
        } else {
            ChromeInk::Metadata
        };
        assert_eq!(buf[(3, 2)].fg, label_ink.color(&theme), "{ink:?}: label");
        for x in 11..16 {
            let cell = &buf[(x, 2)];
            assert_eq!(cell.fg, ink.color(&theme), "{ink:?}: value");
            assert_eq!(cell.bg, Color::Rgb(30, 40, 50), "host owns the ground");
            assert!(
                cell.modifier
                    .contains(Modifier::ITALIC | Modifier::BOLD | Modifier::UNDERLINED)
            );
        }
        assert_eq!(
            buf[(30, 2)].fg,
            theme.text_hint,
            "help uses its own live slot"
        );
        for position in [(0, 2), (16, 2), (20, 2), (3, 3), (39, 4)] {
            assert_eq!(
                buf[position], before[position],
                "{ink:?}: untouched {position:?}"
            );
        }
    }
}

#[test]
fn infoline_cjk_and_combining_text_keep_projected_pointer_geometry_when_clipped() {
    use ratatui::style::{Color, Style};
    let segments = [
        InfoSegment::new(
            InfoSegmentId::Model,
            "",
            "模型-e\u{301}↓",
            ChromeInk::Identity,
        ),
        InfoSegment::new(InfoSegmentId::Context, "上下文", "61%", ChromeInk::Info),
    ];
    for ascii in [false, true] {
        for width in 0..=40 {
            let area = Rect::new(3, 2, width, 1);
            let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 46, 4));
            buf.set_style(buf.area, Style::default().bg(Color::Rgb(30, 40, 50)));
            let before = buf.clone();
            let info = InfoLine::new(&UI_THEME, "", &segments).ascii_safe(ascii);
            let hitboxes = infoline_hitboxes(&info, area);
            let context = context_meter_hitbox(&info, area);
            ratatui::widgets::Widget::render(info, area, &mut buf);
            assert_eq!(buf[(2, 2)], before[(2, 2)], "left clipping");
            assert_eq!(
                buf[(area.right(), 2)],
                before[(area.right(), 2)],
                "right clipping"
            );
            assert_eq!(buf[(3, 3)], before[(3, 3)], "the row never wraps");
            for (index, hitbox) in hitboxes.iter().enumerate() {
                assert!(hitbox.area.x >= area.x && hitbox.area.right() <= area.right());
                assert_eq!(hitbox.area.y, area.y);
                assert_eq!(hitbox.area.height, 1);
                if index > 0 {
                    assert!(hitboxes[index - 1].area.right() <= hitbox.area.x);
                }
            }
            if width >= 7 {
                assert_eq!(hitboxes[0].area, Rect::new(3, 2, 7, 1));
                assert_eq!(buf[(3, 2)].symbol(), "模");
                assert_eq!(buf[(5, 2)].symbol(), "型");
                assert_eq!(buf[(8, 2)].symbol(), "e\u{301}");
                assert_eq!(buf[(9, 2)].symbol(), if ascii { "v" } else { "↓" });
            }
            if width >= 20 {
                assert_eq!(context, Some(Rect::new(13, 2, 10, 1)));
                assert_eq!(buf[(13, 2)].symbol(), "上");
                assert_eq!(buf[(15, 2)].symbol(), "下");
                assert_eq!(buf[(17, 2)].symbol(), "文");
                assert_eq!(buf[(20, 2)].symbol(), "6");
            }
        }
    }
}

#[test]
fn infoline_sanitizes_hidden_controls_before_painting_and_pointer_measurement() {
    let segments = [
        InfoSegment::new(
            InfoSegmentId::Model,
            "",
            "model\u{202e}",
            ChromeInk::Identity,
        ),
        InfoSegment::new(
            InfoSegmentId::Context,
            "ct\0x",
            "6\u{1b}1%",
            ChromeInk::Info,
        ),
        InfoSegment::new(
            InfoSegmentId::OutputTokens,
            "↓",
            "1.2K",
            ChromeInk::MetadataValue,
        ),
    ];
    let area = Rect::new(0, 0, 60, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let info = InfoLine::new(&UI_THEME, "", &segments).ascii_safe(true);
    let hitboxes = infoline_hitboxes(&info, area);
    ratatui::widgets::Widget::render(info, area, &mut buf);
    let row: String = buf.content().iter().map(|cell| cell.symbol()).collect();
    assert_eq!(row.trim_end(), "model   ctx 61%   v 1.2K");
    assert_eq!(
        hitboxes.iter().map(|hit| hit.area).collect::<Vec<_>>(),
        [
            Rect::new(0, 0, 5, 1),
            Rect::new(8, 0, 7, 1),
            Rect::new(18, 0, 6, 1),
        ]
    );
}
