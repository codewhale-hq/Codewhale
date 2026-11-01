//! Compositions of current Codewhale terminal view families.
//!
//! `InstrumentSurface` and `SessionList` are extracted renderers; the other
//! rooms below are examples composed from their source layout and host facts.
//! Input handling, configuration, catalogs, and session loading belong to the
//! application. Source paths are recorded beside each recipe and in VIEWS.md.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap},
};

use super::Entry;
use crate::{
    InstrumentSurface, KeyHint, List, ListRow, ListRowState, ListState, OceanRamp, Paint,
    PickerState, Role, SessionList, SessionRow, Theme, TuiGround, TuiInk, TuiPalette, WorkbarPanel,
    centered, text,
};

fn hint(key: &str, verb: &str) -> KeyHint {
    KeyHint::new(key.to_string(), verb.to_string())
}
fn move_hint(theme: &Theme) -> KeyHint {
    hint(if theme.ascii() { "Up/Dn" } else { "↑/↓" }, "move")
}
fn selected(theme: &Theme) -> Style {
    theme
        .fg(Role::Foreground)
        .patch(theme.bg(Role::Selected))
        .add_modifier(Modifier::BOLD)
}
fn row(area: Rect, y: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(y).min(area.bottom()),
        area.width,
        u16::from(y < area.height),
    )
}
fn below(area: Rect, rows: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(rows).min(area.bottom()),
        area.width,
        area.height.saturating_sub(rows),
    )
}
fn safe(value: &str, theme: &Theme) -> String {
    let value = text::display_safe(value);
    if !theme.ascii() {
        return value.into_owned();
    }
    value
        .chars()
        .map(|c| match c {
            '—' | '–' => "-".into(),
            '·' => ".".into(),
            '’' => "'".into(),
            '‹' => "<".into(),
            '›' => ">".into(),
            _ => c.to_string(),
        })
        .collect()
}
fn line(value: &str, role: Role, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let value = safe(value, theme);
    let value = text::truncate(&value, usize::from(area.width), theme.ascii());
    if !area.is_empty() {
        buf.set_line(
            area.x,
            area.y,
            &Line::styled(value.into_owned(), theme.fg(role)),
            area.width,
        );
    }
}

fn top_section(title: &str, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
    let mut block = Block::default()
        .title(Line::styled(
            safe(title, theme),
            theme.fg(Role::Accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::TOP)
        .border_style(theme.fg(Role::Border))
        .style(theme.tui_ground(TuiGround::Surface))
        .padding(Padding::uniform(1));
    if theme.ascii() {
        block = block.border_set(border::Set {
            horizontal_top: "-",
            ..border::PLAIN
        });
    }
    let inner = block.inner(area);
    if !area.is_empty() {
        block.render(area, buf);
    }
    inner
}

/// Native centered choice shape from views/{mode_picker,status_picker}.rs
/// and command_palette.rs. The instrument and centered shapes stay distinct.
fn choice_card(
    title: &str,
    area: Rect,
    preferred: (u16, u16),
    minimum: (u16, u16),
    actions: &[KeyHint],
    buf: &mut Buffer,
    theme: &Theme,
) -> Rect {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return area;
    }
    let popup = centered(area, preferred.0, preferred.1, minimum.0, minimum.1);
    let shadow = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup
            .width
            .min(area.right().saturating_sub(popup.x.saturating_add(1))),
        popup
            .height
            .min(area.bottom().saturating_sub(popup.y.saturating_add(1))),
    );
    if !shadow.is_empty() {
        Block::default()
            .style(theme.tui_ground(TuiGround::Elevated))
            .render(shadow, buf);
    }
    Clear.render(popup, buf);
    Block::default()
        .style(theme.tui_ground(TuiGround::Surface))
        .render(popup, buf);
    let mut block = Block::default()
        .title(Line::styled(
            safe(title, theme),
            theme.fg(Role::Accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(theme.fg(Role::Border))
        .style(theme.tui_ground(TuiGround::Surface))
        .padding(Padding::uniform(1));
    if theme.ascii() {
        block = block.border_set(border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            horizontal_top: "-",
            horizontal_bottom: "-",
            vertical_left: "|",
            vertical_right: "|",
        });
    }
    let inner = block.inner(popup);
    block.render(popup, buf);
    InstrumentSurface::draw_footer(inner, buf, actions, theme, false)
}

fn instrument(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = InstrumentSurface::new("context")
        .actions(vec![hint("Enter", "details"), hint("Esc", "close")])
        .draw(area, buf, theme);
    line(
        "48k of 128k · 38% · compacts at 90%",
        Role::Foreground,
        row(body, 0),
        buf,
        theme,
    );
    line(
        "system + tools · 8.4k · 24 tools",
        Role::Muted,
        row(body, 2),
        buf,
        theme,
    );
    line(
        "conversation · 18 messages · 29.2k",
        Role::Muted,
        row(body, 3),
        buf,
        theme,
    );
    line("tool output · 10.4k", Role::Muted, row(body, 4), buf, theme);
}

pub(crate) fn sample_sessions() -> SessionList<'static> {
    SessionList::new(vec![
        SessionRow::new(
            "4a80c752-28b1-4a96-9638-11a6ac1e394d",
            "Review authentication",
        )
        .messages(18)
        .mode("Work")
        .updated("2026-10-01 14:12 (2m ago)")
        .current(true),
        SessionRow::new("b419de17-939c-4d82-af11-698ae29b449e", "Plan the release")
            .messages(12)
            .mode("Plan")
            .updated("2026-10-01 13:40 (34m ago)"),
        SessionRow::new(
            "936efb90-55a6-4b3d-b223-b8279d319589",
            "Review the alternate approach",
        )
        .messages(8)
        .mode("Work")
        .updated("2026-10-01 12:53 (1h ago)")
        .fork(true),
        SessionRow::new(
            "ff82d346-410e-4e69-b63a-cf5208c58180",
            "Add keyboard navigation",
        )
        .messages(24)
        .mode("Operate")
        .updated("2026-09-30 17:30 (20h ago)"),
        SessionRow::new("3f473bc6-3aee-45a4-9510-6a599fb75452", "Session")
            .messages(1)
            .mode("Work")
            .updated("2026-09-30 11:09 (1d ago)")
            .archived(true),
    ])
    .state(PickerState::new(0))
}

// session_picker.rs::render, section_block, and build_preview_lines:
// history is left 56%, sessions right 44%; narrow rooms stack list above history.
fn sessions(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let full = vec![
        hint("Enter", "resume"),
        hint("/", "search"),
        hint("s", "sort"),
        hint("r", "rename"),
        hint("a", "all workspaces"),
        hint("e", "archive"),
        hint("x", "archived"),
        hint("d", "delete"),
        hint("Esc", "close"),
    ];
    let shell = InstrumentSurface::new("sessions").actions(full);
    let compact = shell.areas(area, theme).body.height < 12;
    let body = if compact {
        InstrumentSurface::new("sessions")
            .actions(vec![
                hint("Enter", "resume"),
                hint("/", "search"),
                hint("Esc", "close"),
            ])
            .draw(area, buf, theme)
    } else {
        shell.draw(area, buf, theme)
    };
    if compact {
        sample_sessions().paint(body, buf, theme);
        return;
    }
    let narrow = body.width < 95;
    let parts = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if narrow {
            [Constraint::Percentage(42), Constraint::Percentage(58)]
        } else {
            [Constraint::Percentage(56), Constraint::Percentage(44)]
        })
        .split(body);
    let (history, list) = if narrow {
        (parts[1], parts[0])
    } else {
        (parts[0], parts[1])
    };
    let list = top_section(" sessions (1-9, PgUp/PgDn) ", list, buf, theme);
    sample_sessions().paint(list, buf, theme);
    let history = top_section(" history (Shift+PgUp/PgDn) ", history, buf, theme);
    let lines = [
        "Title: Review authentication",
        "ID: 4a80c752-28b1-4a96-9638-11a6ac1e394d",
        "Updated: 2026-10-01 14:12",
        "Messages: 18 | Model: deepseek-v4",
        "Mode: Work",
        "",
        "USER:",
        "  Check the sign-in flow and keyboard navigation.",
        "",
        "ASSISTANT:",
        "  The sign-in screen now returns focus to the account button.",
        "  I also updated the tab order through the provider choices.",
    ];
    Paragraph::new(
        lines
            .into_iter()
            .map(|v| Line::styled(v, theme.fg(Role::Foreground)))
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .render(history, buf);
}

fn session_list(area: Rect, buf: &mut Buffer, theme: &Theme) {
    sample_sessions().paint(area, buf, theme);
}
fn sessions_empty(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = InstrumentSurface::new("sessions")
        .actions(vec![hint("/", "search"), hint("Esc", "close")])
        .draw(area, buf, theme);
    SessionList::new(Vec::new()).paint(body, buf, theme);
}

// views/mode_picker.rs::render: one line per mode, eight-cell name column.
struct ModeRow {
    number: char,
    name: &'static str,
    hint: &'static str,
}
impl ListRow for ModeRow {
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        let prefix = format!(
            "{}. {}",
            self.number,
            text::pad(self.name, 8, theme.ascii())
        );
        let help = safe(self.hint, theme);
        let hint = text::truncate_words(
            &help,
            usize::from(area.width).saturating_sub(text::width(&prefix)),
            theme.ascii(),
        );
        let hint_style = if state.selected {
            theme.fg(Role::Foreground).patch(theme.bg(Role::Selected))
        } else {
            theme.fg(Role::Muted)
        };
        buf.set_line(
            area.x,
            area.y,
            &Line::from(vec![
                Span::styled(prefix, state.ink(theme)),
                Span::styled(hint.into_owned(), hint_style),
            ]),
            area.width,
        );
    }
}
fn modes(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = choice_card(
        " Mode ",
        area,
        (68, 10),
        (44, 8),
        &[
            move_hint(theme),
            hint("Enter", "select"),
            hint("Esc", "cancel"),
        ],
        buf,
        theme,
    );
    let rows = [
        ModeRow {
            number: '1',
            name: "Work",
            hint: "Direct work in this session — edits and shell ask for approval",
        },
        ModeRow {
            number: '2',
            name: "Plan",
            hint: "Read-only research first — present a plan before acting",
        },
        ModeRow {
            number: '3',
            name: "Operate",
            hint: "Turns your prompt into a goal: parallel agents, verified work",
        },
    ];
    if !body.is_empty() {
        List::new(&rows, ListState::new(0)).paint(body, buf, theme);
    }
}

// command_palette.rs::render: query/count/scope header and sectioned entries.
fn commands(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = choice_card(
        " Commands — Find and run one action ",
        area,
        (90, 22),
        (44, 8),
        &[
            move_hint(theme),
            hint("Enter", "select"),
            hint("Esc", "cancel"),
        ],
        buf,
        theme,
    );
    line("Type to filter", Role::Muted, row(body, 0), buf, theme);
    line("6 shown / 6 entries", Role::Dim, row(body, 1), buf, theme);
    line(
        if theme.ascii() {
            "scope: c:cmd . s:skill . t:tool . m:mcp"
        } else {
            "scope: c:cmd · s:skill · t:tool · m:mcp"
        },
        Role::Dim,
        row(body, 2),
        buf,
        theme,
    );
    let entries = [
        ("/model", "Choose a model"),
        ("/theme", "Preview a theme"),
        ("/context", "Inspect the context window"),
        ("/sessions", "Browse saved sessions"),
        ("/config", "Open settings"),
        ("/fleet", "Open the fleet roster"),
    ];
    let label_width = 24usize.min(usize::from(body.width).saturating_sub(22));
    let heading = row(body, 4);
    if !heading.is_empty() {
        buf.set_line(
            heading.x,
            heading.y,
            &Line::styled(
                "  Commands (6)  ",
                theme.fg(Role::Accent).add_modifier(Modifier::BOLD),
            ),
            heading.width,
        );
    }
    for (index, (label, description)) in entries.into_iter().enumerate() {
        let target = row(body, index as u16 + 5);
        if target.is_empty() {
            break;
        }
        let chosen = index == 0;
        let pointer = if chosen {
            if theme.ascii() { ">" } else { "▸" }
        } else {
            " "
        };
        let prefix = format!(
            "{pointer} {}  ",
            text::pad(label, label_width, theme.ascii())
        );
        let desc = text::truncate(
            description,
            usize::from(target.width).saturating_sub(text::width(&prefix)),
            theme.ascii(),
        );
        buf.set_line(
            target.x,
            target.y,
            &Line::styled(
                format!("{prefix}{desc}"),
                if chosen {
                    selected(theme)
                } else {
                    theme.fg(Role::Foreground)
                },
            ),
            target.width,
        );
    }
}

// views/status_picker.rs::render; labels/hints from config.rs::StatusItem.
fn status_items(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = choice_card(
        " Status line ",
        area,
        (64, 16),
        (40, 8),
        &[
            hint("Space", "toggle"),
            hint("a", "all"),
            hint("n", "none"),
            hint("Enter", "save"),
            hint("Esc", "cancel"),
        ],
        buf,
        theme,
    );
    line(
        "Choose what the footer shows:",
        Role::Muted,
        row(body, 0),
        buf,
        theme,
    );
    let rows = [
        ("Mode", "plan · act · operate", true),
        ("Model", "the model id you'll send to", true),
        (
            "Context window %",
            "tokens used / model context window",
            true,
        ),
        ("Session cost", "running total for this session", true),
        (
            "Account balance",
            "remaining prepaid credit from the active provider",
            false,
        ),
        (
            "Prompt cache hit rate",
            "% of prompt served from cache",
            true,
        ),
        (
            "Output tokens",
            "output tokens of the live or last turn",
            true,
        ),
        (
            "Time to first token",
            "average wait for the first token",
            true,
        ),
        (
            "Output rate",
            "average tok/s, including first-token wait",
            true,
        ),
        ("Workspace", "directory this session writes to", false),
        ("Git branch", "branch the next commit lands on", false),
    ];
    for (index, (label, help, checked)) in rows.into_iter().enumerate() {
        let target = row(body, index as u16 + 2);
        if target.is_empty() {
            break;
        }
        let pointer = if index == 0 {
            if theme.ascii() { ">" } else { "▸" }
        } else {
            " "
        };
        let mark = if checked {
            if theme.ascii() { "[x]" } else { "[✓]" }
        } else {
            "[ ]"
        };
        let prefix = format!(" {pointer} {mark} {label}  (");
        let help_text = safe(help, theme);
        let help = text::truncate_words(
            &help_text,
            usize::from(target.width).saturating_sub(text::width(&prefix) + 1),
            theme.ascii(),
        );
        let display = text::pad(
            &format!("{prefix}{help})"),
            usize::from(target.width),
            theme.ascii(),
        );
        buf.set_line(
            target.x,
            target.y,
            &Line::styled(
                display,
                if index == 0 {
                    selected(theme)
                } else {
                    theme.fg(if checked {
                        Role::Foreground
                    } else {
                        Role::Muted
                    })
                },
            ),
            target.width,
        );
    }
}

// views/mod.rs::ListDetailLayout::split. Model picker widens the list and
// retains a 30-cell effort pane; provider picker retains the ordinary split.
fn list_detail(area: Rect, min_detail: u16) -> (Rect, Rect, bool) {
    if area.is_empty() {
        return (area, area, true);
    }
    let min_list = 30.min(area.width);
    if area.width >= 96 && area.width.saturating_sub(1 + min_list) >= min_detail {
        let list_width = (area.width.saturating_mul(42) / 100)
            .clamp(min_list, area.width.saturating_sub(1 + min_detail).min(52));
        return (
            Rect::new(area.x, area.y, list_width, area.height),
            Rect::new(
                area.x + list_width + 1,
                area.y,
                area.width - list_width - 1,
                area.height,
            ),
            false,
        );
    }
    let gap = u16::from(area.height >= 8);
    let max_list = area.height.saturating_sub(gap + 4.min(area.height));
    let list_height = (area.height.saturating_mul(3) / 5).clamp(1, max_list.max(1));
    (
        Rect::new(area.x, area.y, area.width, list_height),
        Rect::new(
            area.x,
            area.y + list_height + gap,
            area.width,
            area.height.saturating_sub(list_height + gap),
        ),
        true,
    )
}

struct RouteRow {
    id: &'static str,
    route: &'static str,
    detail: &'static str,
}
impl ListRow for RouteRow {
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        let id_width = 24.min(usize::from(area.width));
        let route_width = 15.min(usize::from(area.width).saturating_sub(id_width + 2));
        let prefix = format!(
            "{}  {}  ",
            text::pad(self.id, id_width, theme.ascii()),
            text::pad(self.route, route_width, theme.ascii())
        );
        let detail = text::truncate(
            self.detail,
            usize::from(area.width).saturating_sub(text::width(&prefix)),
            theme.ascii(),
        );
        buf.set_line(
            area.x,
            area.y,
            &Line::styled(format!("{prefix}{detail}"), state.ink(theme)),
            area.width,
        );
    }
}

// model_picker.rs::render_route / render_pane / widen_model_pane.
fn models(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let shell = InstrumentSurface::new(if theme.ascii() {
        "route . configured"
    } else {
        "route · configured"
    })
    .actions(vec![
        move_hint(theme),
        hint("Tab", "switch"),
        hint("Type", "search any model"),
        hint("Enter", "apply"),
        hint(
            if theme.ascii() { "Shift+A" } else { "⇧A" },
            "browse catalog",
        ),
        hint("Esc", "cancel"),
    ]);
    let regions = shell.areas(area, theme);
    let body = shell.draw(area, buf, theme);
    title_action(
        "browse catalog (3)",
        regions.surface,
        regions.body.right(),
        buf,
        theme,
    );
    line("Provider: DeepSeek", Role::Muted, row(body, 0), buf, theme);
    let (mut list, mut effort, stacked) = list_detail(below(body, 1), 24);
    if !stacked {
        let width = effort.width.min(30);
        list.width = list
            .width
            .saturating_add(effort.width.saturating_sub(width));
        effort.x = list.right() + 1;
        effort.width = width;
    }
    line(
        if theme.ascii() {
            "  Model . configured"
        } else {
            "  Model · configured"
        },
        Role::Foreground,
        row(list, 0),
        buf,
        theme,
    );
    line(
        "  Model                     Provider         Details",
        Role::Muted,
        row(list, 1),
        buf,
        theme,
    );
    let models = [
        RouteRow {
            id: "deepseek-v4",
            route: "DeepSeek",
            detail: "active",
        },
        RouteRow {
            id: "deepseek-v4-flash",
            route: "DeepSeek",
            detail: "saved",
        },
        RouteRow {
            id: "qwen3.5:35b",
            route: "Ollama",
            detail: "local",
        },
    ];
    if list.height > 2 {
        List::new(&models, ListState::new(0)).paint(below(list, 2), buf, theme);
    }
    line("  Reasoning", Role::Muted, row(effort, 0), buf, theme);
    let levels = ["low", "medium", "high"];
    if effort.height > 1 {
        List::new(&levels, ListState::new(2)).paint(below(effort, 1), buf, theme);
    }
}

fn title_action(label: &str, surface: Rect, right: u16, buf: &mut Buffer, theme: &Theme) {
    if surface.width < 28 || surface.height == 0 {
        return;
    }
    let width = u16::try_from(text::width(label))
        .unwrap_or(u16::MAX)
        .min(surface.width.saturating_sub(18));
    let x = right.saturating_sub(width).max(surface.x);
    buf.set_stringn(
        x,
        surface.y,
        text::truncate(label, usize::from(width), theme.ascii()),
        usize::from(width),
        theme.fg(Role::Accent).add_modifier(Modifier::UNDERLINED),
    );
}

// provider_picker.rs::render_dashboard / render_provider_detail.
fn providers(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let shell = InstrumentSurface::new("Provider").actions(vec![
        move_hint(theme),
        hint("/", "search"),
        hint("Enter", "use"),
        hint("A", "browse all"),
        hint("R", "edit key"),
        hint("M", "models"),
        hint("C-t", "test connection"),
        hint("Esc", "cancel"),
    ]);
    let regions = shell.areas(area, theme);
    let body = shell.draw(area, buf, theme);
    title_action(
        "browse all",
        regions.surface,
        regions.body.right(),
        buf,
        theme,
    );
    let (list, detail, _) = list_detail(body, 34);
    let rows = [
        ("DeepSeek", " *", "key saved"),
        ("Ollama", "  ", "local"),
        ("OpenAI", "  ", "key saved"),
    ];
    for (index, (name, active, state)) in rows.into_iter().enumerate() {
        let target = row(list, index as u16);
        if target.is_empty() {
            break;
        }
        let marker = if index == 0 {
            if theme.ascii() { ">" } else { "▸" }
        } else {
            " "
        };
        let prefix = format!(" {marker} {name}{active}  ");
        let state = text::truncate_words(
            state,
            usize::from(target.width).saturating_sub(text::width(&prefix)),
            theme.ascii(),
        );
        buf.set_line(
            target.x,
            target.y,
            &Line::styled(
                format!("{prefix}{state}"),
                if index == 0 {
                    selected(theme)
                } else {
                    theme.fg(Role::Foreground)
                },
            ),
            target.width,
        );
    }
    line("DeepSeek", Role::Foreground, row(detail, 0), buf, theme);
    line("key saved", Role::Muted, row(detail, 2), buf, theme);
    line(
        "Model: deepseek-v4 · saved",
        Role::Foreground,
        row(detail, 3),
        buf,
        theme,
    );
    line(
        "Endpoint: https://api.deepseek.com",
        Role::Muted,
        row(detail, 4),
        buf,
        theme,
    );
    line(
        "Credential: environment",
        Role::Muted,
        row(detail, 5),
        buf,
        theme,
    );
}

// theme_picker.rs::render; native selectable order and five two-cell swatches.
fn themes(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = InstrumentSurface::new(if theme.ascii() {
        "theme . live preview"
    } else {
        "theme · live preview"
    })
    .actions(vec![
        hint(if theme.ascii() { "Up/Dn" } else { "↑/↓" }, "preview"),
        hint("Enter", "save"),
        hint("Esc", "revert"),
    ])
    .draw(area, buf, theme);
    let options = [
        (
            "System",
            None,
            "Follow terminal background (COLORFGBG / macOS appearance)",
        ),
        (
            "Terminal",
            Some(TuiPalette::Terminal),
            "Inherit terminal colors fully (transparent surfaces, ANSI accents)",
        ),
        (
            "Shoreline",
            Some(TuiPalette::Shoreline),
            "Warm charcoal, one restrained blue — the desktop client's palette",
        ),
        (
            "Shoreline Light",
            Some(TuiPalette::ShorelineLight),
            "Shoreline on warm paper — the desktop client's light mode",
        ),
        (
            "Underwater",
            Some(TuiPalette::Underwater),
            "The painted ocean field: ombre water, ambient life, the whale",
        ),
        (
            "Underwater Retro",
            Some(TuiPalette::UnderwaterRetro),
            "Flat phosphor-teal ocean: the legacy deepsea look, no ombre",
        ),
        (
            "Blue Stage",
            Some(TuiPalette::Whale),
            "Stage black, action blue, and one Signal Gold human beacon",
        ),
        (
            "Blue Stage Light",
            Some(TuiPalette::WhaleLight),
            "Paper, cobalt action, and one Signal Gold human beacon",
        ),
        (
            "Grayscale",
            Some(TuiPalette::Grayscale),
            "Color-minimal high contrast",
        ),
        (
            "Catppuccin Mocha",
            Some(TuiPalette::CatppuccinMocha),
            "Soft pastels on warm dark",
        ),
        (
            "Tokyo Night",
            Some(TuiPalette::TokyoNight),
            "Deep blue/violet night palette",
        ),
        (
            "Dracula",
            Some(TuiPalette::Dracula),
            "Classic high-contrast purple",
        ),
        (
            "Gruvbox Dark",
            Some(TuiPalette::GruvboxDark),
            "Vintage warm earth tones",
        ),
        ("Claude", Some(TuiPalette::Claude), "Warm navy & coral"),
        (
            "Matrix",
            Some(TuiPalette::Matrix),
            "The Matrix films inspired theme",
        ),
        (
            "Solarized Light",
            Some(TuiPalette::SolarizedLight),
            "Solarized light — Light, calming palette on warm ivory — easy on the eyes",
        ),
        (
            "Uwu",
            Some(TuiPalette::Uwu),
            "Soft kawaii night — sakura, mint, and peach",
        ),
    ];
    for (index, (name, palette, tagline)) in options.into_iter().enumerate() {
        let target = row(body, index as u16 + 1);
        if target.is_empty() {
            break;
        }
        let is_selected = index == 4;
        let style = if is_selected {
            selected(theme)
        } else {
            theme.fg(Role::Foreground)
        };
        let candidate = palette.map_or(*theme, |palette| theme.tui_palette(palette));
        let pointer = if is_selected {
            if theme.ascii() { ">" } else { "▸" }
        } else {
            " "
        };
        let mut spans = vec![
            Span::styled(format!(" {pointer} "), style),
            Span::styled(
                format!("{}. ", index + 1),
                if is_selected {
                    theme
                        .tui_ink(TuiInk::Working)
                        .patch(theme.bg(Role::Selected))
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme.fg(Role::Hint)
                },
            ),
            Span::styled(text::pad(name, 22, theme.ascii()), style),
        ];
        if palette == Some(TuiPalette::Underwater) && OceanRamp::for_theme(&candidate).is_some() {
            for color in [
                OceanRamp::SURFACE,
                OceanRamp::MIDDLE,
                OceanRamp::DEEP,
                OceanRamp::AMBIENT,
            ] {
                spans.push(Span::styled("  ", Style::default().bg(color)));
            }
            spans.push(Span::styled(
                "  ",
                Style::default().bg(candidate.tui_ink(TuiInk::Working).fg.unwrap_or_default()),
            ));
        } else {
            spans.extend(
                [
                    Role::Background,
                    Role::Surface,
                    Role::Live,
                    Role::Attention,
                    Role::Accent,
                ]
                .map(|role| Span::styled("  ", candidate.bg(role))),
            );
        }
        spans.push(Span::raw("  "));
        let prefix = Line::from(spans.clone()).width();
        let tagline_text = safe(tagline, theme);
        let tagline = text::truncate_words(
            &tagline_text,
            usize::from(target.width).saturating_sub(prefix),
            theme.ascii(),
        );
        spans.push(Span::styled(
            tagline.into_owned(),
            theme.fg(Role::Dim).patch(if is_selected {
                theme.bg(Role::Selected)
            } else {
                Style::default()
            }),
        ));
        buf.set_line(target.x, target.y, &Line::from(spans), target.width);
    }
}

// file_picker.rs::render: explicit query, blank row, marker field, paths.
fn files(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = InstrumentSurface::new("@ attach · 5 matches")
        .actions(vec![
            move_hint(theme),
            hint("Enter", "insert @path"),
            hint("Esc", "cancel"),
        ])
        .draw(area, buf, theme);
    let query = row(body, 0);
    if !query.is_empty() {
        buf.set_line(
            query.x,
            query.y,
            &Line::from(vec![
                Span::styled("> ", theme.fg(Role::Accent).add_modifier(Modifier::BOLD)),
                Span::styled("tui/", theme.fg(Role::Foreground)),
                Span::styled(
                    " ",
                    theme.bg(Role::Accent).patch(theme.fg(Role::Background)),
                ),
            ]),
            query.width,
        );
    }
    for (index, path) in [
        "crates/tui/src/tui/ui/frame.rs",
        "crates/tui/src/tui/widgets/mod.rs",
        "crates/tui/src/tui/work_surface/render/mod.rs",
        "crates/tui/src/tui/session_picker.rs",
        "crates/tui/src/tui/theme_picker.rs",
    ]
    .into_iter()
    .enumerate()
    {
        let target = row(body, index as u16 + 2);
        if target.is_empty() {
            break;
        }
        let pointer = if index == 0 {
            if theme.ascii() { ">" } else { "▸" }
        } else {
            " "
        };
        let marker = if target.width >= 18 { "    " } else { "" };
        let prefix = format!("{pointer} {marker}");
        let path = text::truncate(
            path,
            usize::from(target.width).saturating_sub(text::width(&prefix)),
            theme.ascii(),
        );
        buf.set_line(
            target.x,
            target.y,
            &Line::styled(
                format!("{prefix}{path}"),
                if index == 0 {
                    theme.fg(Role::Foreground).patch(theme.bg(Role::Selected))
                } else {
                    theme.fg(Role::Foreground)
                },
            ),
            target.width,
        );
    }
}

// views/mod.rs::ConfigView::render_settings_shell / config_shell_panes.
fn settings(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let body = InstrumentSurface::new("Settings")
        .actions(vec![
            hint("type", "filter"),
            move_hint(theme),
            hint("Enter", "edit"),
            hint("PgUp/PgDn", "scroll"),
            hint("Esc", "close"),
        ])
        .draw(area, buf, theme);
    let nav = row(body, 0);
    let categories = [
        "Appearance",
        "Models & providers",
        "Fleet",
        "Work",
        "Tools & MCP",
        "Trust",
        "Motion",
        "Advanced",
    ];
    let mut x = nav.x;
    for (index, category) in categories
        .into_iter()
        .enumerate()
        .filter(|_| !nav.is_empty())
    {
        let shown = format!(" {category} ");
        let width = u16::try_from(text::width(&shown)).unwrap_or(u16::MAX);
        if x.saturating_add(width).saturating_add(2) > nav.right() {
            if x < nav.right() {
                buf.set_stringn(
                    nav.right().saturating_sub(2),
                    nav.y,
                    if theme.ascii() { " >" } else { " ›" },
                    2,
                    theme.fg(Role::Hint),
                );
            }
            break;
        }
        if !nav.is_empty() {
            buf.set_stringn(
                x,
                nav.y,
                &shown,
                usize::from(width),
                if index == 0 {
                    selected(theme)
                } else {
                    theme.fg(Role::Muted)
                },
            );
        }
        x = x.saturating_add(width);
    }
    line(
        " Search: type to filter  (5/5)",
        Role::Muted,
        row(body, 1),
        buf,
        theme,
    );
    let has_detail = body.width >= 100;
    let preview = u16::from(body.height >= 12);
    let band = if body.height >= 11 {
        if has_detail { 1 } else { 3 }
    } else {
        u16::from(body.height >= 5)
    };
    let list_area = Rect::new(
        body.x,
        body.y.saturating_add(2).min(body.bottom()),
        body.width,
        body.height.saturating_sub(2 + preview + band),
    );
    let groups_width = if has_detail && list_area.height >= 3 {
        18
    } else {
        0
    };
    let detail_width = if has_detail {
        (list_area.width.saturating_mul(34) / 100).clamp(28, 44)
    } else {
        0
    };
    let left_gap = u16::from(groups_width > 0);
    let right_gap = u16::from(detail_width > 0);
    let list = Rect::new(
        list_area.x + groups_width + left_gap,
        list_area.y,
        list_area
            .width
            .saturating_sub(groups_width + left_gap + detail_width + right_gap),
        list_area.height,
    );
    if groups_width > 0 {
        for (index, group) in ["Display"].into_iter().enumerate() {
            line(
                group,
                if index == 0 {
                    Role::Accent
                } else {
                    Role::Muted
                },
                row(
                    Rect::new(list_area.x, list_area.y, groups_width, list_area.height),
                    index as u16,
                ),
                buf,
                theme,
            );
        }
        divider(
            Rect::new(list_area.x + groups_width, list_area.y, 1, list_area.height),
            buf,
            theme,
        );
    }
    let settings = [
        ("Theme", "underwater", "‹ ›"),
        ("Thinking background highlight", "true", "[x]"),
        ("Contextual tips", "true", "[x]"),
        ("Inline file changes", "full", "‹ ›"),
        ("Transcript spacing", "comfortable", "‹ ›"),
    ];
    let caption = row(list, 0);
    if !caption.is_empty() {
        buf.set_line(
            caption.x,
            caption.y,
            &Line::styled(
                "  Display",
                theme
                    .fg(Role::Hint)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            caption.width,
        );
    }
    let scope_width = if list.width >= 60 { 7 } else { 0 };
    let available = usize::from(list.width).saturating_sub(11 + scope_width);
    let key_width = 19.min(available.saturating_sub(10)).max(19.min(available));
    let value_width = available.saturating_sub(key_width).min(44);
    for (index, (key, value, affordance)) in settings.into_iter().enumerate() {
        let target = row(list, index as u16 + 1);
        if target.is_empty() {
            break;
        }
        let chosen = index == 0;
        let pointer = if chosen {
            if theme.ascii() { ">" } else { "❯" }
        } else {
            " "
        };
        let affordance = if theme.ascii() && affordance == "‹ ›" {
            "< >"
        } else {
            affordance
        };
        let content = format!(
            "{pointer}{}  {}  {affordance:<3}  {}",
            text::pad(key, key_width, theme.ascii()),
            text::pad(value, value_width, theme.ascii()),
            if scope_width > 0 { "user" } else { "" }
        );
        buf.set_line(
            target.x,
            target.y,
            &Line::styled(
                content,
                if chosen {
                    selected(theme)
                } else {
                    theme.fg(Role::Foreground)
                },
            ),
            target.width,
        );
    }
    if detail_width > 0 {
        let detail = Rect::new(list.right() + 1, list.y, detail_width, list.height);
        divider(Rect::new(list.right(), list.y, 1, list.height), buf, theme);
        line("Theme", Role::Accent, row(detail, 0), buf, theme);
        line("theme", Role::Dim, row(detail, 1), buf, theme);
        for (index, (name, value)) in [
            ("current", "underwater"),
            ("saved", "underwater"),
            ("startup", "underwater"),
            ("source", "settings.toml"),
            ("scope", "user"),
            ("apply", "applies on save"),
            ("kind", "choice"),
        ]
        .into_iter()
        .enumerate()
        {
            line(
                &format!("{name:<10}{value}"),
                Role::Foreground,
                row(detail, index as u16 + 3),
                buf,
                theme,
            );
        }
    }
    let band_area = Rect::new(
        body.x,
        list_area.bottom(),
        body.width,
        band.min(body.bottom().saturating_sub(list_area.bottom())),
    );
    line(
        "Theme: underwater — Enter to edit",
        Role::Muted,
        row(band_area, 0),
        buf,
        theme,
    );
    if band > 1 {
        line(
            "current: underwater  saved: underwater  startup: underwater",
            Role::Muted,
            row(band_area, 1),
            buf,
            theme,
        );
    }
    if band > 2 {
        line(
            "source: settings.toml  scope: user  apply: applies on save",
            Role::Muted,
            row(band_area, 2),
            buf,
            theme,
        );
    }
    if preview > 0 {
        super::posture::working_posture().paint(
            row(body, body.height.saturating_sub(1)),
            buf,
            theme,
        );
    }
}

fn divider(area: Rect, buf: &mut Buffer, theme: &Theme) {
    for y in area.y..area.bottom() {
        if area.width > 0 {
            buf[(area.x, y)]
                .set_symbol(if theme.ascii() { "|" } else { "│" })
                .set_style(theme.fg(Role::Border));
        }
    }
}

// work_surface/render/{mod,rows}.rs: reuse the extracted dock, never turn
// its panels into unrelated modal cards. Full-size recipes show side placement.
fn dock(panel: WorkbarPanel, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let area = area.intersection(buf.area);
    let dock = super::workbar::sample(panel).placement(crate::WorkbarPlacement::Right);
    let regions = dock.regions(area);
    if let Some(dock_area) = regions.dock {
        dock.placement(regions.placement)
            .paint(dock_area, buf, theme);
    }
    if !regions.content.is_empty() {
        let content = regions.content;
        line("USER", Role::Muted, row(content, 1), buf, theme);
        line(
            "Review the changes and continue.",
            Role::Foreground,
            row(content, 2),
            buf,
            theme,
        );
        line("ASSISTANT", Role::Muted, row(content, 5), buf, theme);
        line(
            "I’m checking the selected files and the running tasks.",
            Role::Foreground,
            row(content, 6),
            buf,
            theme,
        );
        if content.height > 12 {
            let footer = Rect::new(content.x, content.bottom() - 5, content.width, 5);
            crate::NativeComposer::new("Keep the changes focused.")
                .focused(true)
                .can_submit(true)
                .submit_hint("Enter send")
                .paint(Rect::new(footer.x, footer.y, footer.width, 3), buf, theme);
            super::posture::working_posture().paint(row(footer, 3), buf, theme);
            super::posture::working_metrics().paint(row(footer, 4), buf, theme);
        }
    }
}
fn fleet(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Fleet, area, buf, theme);
}
fn jobs(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Jobs, area, buf, theme);
}
fn changed_files(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Files, area, buf, theme);
}
fn context(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Context, area, buf, theme);
}
fn git(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Git, area, buf, theme);
}
fn cost(area: Rect, buf: &mut Buffer, theme: &Theme) {
    dock(WorkbarPanel::Cost, area, buf, theme);
}

fn rich_composer_facts(theme: &Theme) -> crate::NativeComposerFrame<'static> {
    use crate::{
        NativeComposerDensity, NativeComposerFrame, NativeComposerMenu, NativeComposerMenuItem,
        NativeComposerStyles,
    };
    let background = theme.tui_ground(crate::TuiGround::Composer);
    let plain = theme.fg(crate::Role::Foreground);
    let muted = theme.fg(crate::Role::Muted);
    let primary = theme.fg(crate::Role::Accent);
    let mark = if theme.ascii() { ">" } else { "▸" };
    NativeComposerFrame {
        text: if theme.ascii() {
            "Review the source and keep the change focused"
        } else {
            "Review 鲸鱼 cafe\u{0301} and keep the change focused"
        }
        .into(),
        cursor: 12,
        selection: Some((7, 12)),
        placeholder: Line::styled("Write a task or use /.", muted),
        enclosed: true,
        density: NativeComposerDensity::Comfortable,
        history_search: false,
        focused: true,
        can_submit: true,
        ascii: theme.ascii(),
        top_title: None,
        top_right: Some(Line::styled(
            " Builder ",
            theme
                .fg(crate::Role::Attention)
                .add_modifier(Modifier::BOLD),
        )),
        hint: Some(Line::styled(" Enter chooses; Esc closes menu ", muted)),
        quiet_hint: None,
        styles: NativeComposerStyles {
            background,
            border: primary,
            quiet_border: theme.fg(crate::Role::Border),
            text: plain,
            selection: plain.patch(theme.bg(crate::Role::Selected)).add_modifier(
                if theme.color(crate::Role::Foreground).is_none() {
                    Modifier::REVERSED
                } else {
                    Modifier::empty()
                },
            ),
            prompt: primary,
            submit: theme
                .tui_ink(crate::TuiInk::Info)
                .add_modifier(Modifier::BOLD),
        },
        menu: NativeComposerMenu {
            selected: 1,
            reserved_rows: 3,
            pointer_rows: true,
            items: vec![
                NativeComposerMenuItem::Columns {
                    name: "/review".into(),
                    description: "Review the current changes and show findings".into(),
                    prefix: Span::styled(" ", plain),
                    marker: Span::styled(" ", plain),
                    name_style: plain,
                    description_style: muted,
                },
                NativeComposerMenuItem::Columns {
                    name: "/test or /check".into(),
                    description: "Run focused checks and preserve their receipt".into(),
                    prefix: Span::styled(" ", primary),
                    marker: Span::styled(mark, primary),
                    name_style: primary,
                    description_style: plain,
                },
            ],
        },
    }
}
fn rich_composer(area: Rect, buf: &mut Buffer, theme: &Theme) {
    rich_composer_facts(theme).paint(area, buf, theme);
}
fn rich_composer_search(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let mut composer = rich_composer_facts(theme);
    composer.text = "".into();
    composer.cursor = 0;
    composer.selection = None;
    composer.history_search = true;
    composer.top_title = Some(Line::styled(
        " Search history ",
        theme.fg(crate::Role::Muted),
    ));
    composer.menu.items = vec![crate::NativeComposerMenuItem::Line(Line::styled(
        if theme.ascii() {
            "> Review this project"
        } else {
            "▸ Review this project"
        },
        theme.fg(crate::Role::Foreground),
    ))];
    composer.menu.selected = 0;
    composer.menu.reserved_rows = 1;
    composer.menu.pointer_rows = false;
    composer.paint(area, buf, theme);
}

pub(crate) fn entries() -> Vec<Entry> {
    [
        (
            "native-composer-rich-selection",
            72,
            9,
            rich_composer as fn(Rect, &mut Buffer, &Theme),
        ),
        ("native-composer-rich-search", 72, 7, rich_composer_search),
        (
            "instrument-surface",
            100,
            24,
            instrument as fn(Rect, &mut Buffer, &Theme),
        ),
        ("session-list", 140, 8, session_list),
        ("view-sessions", 160, 28, sessions),
        ("view-sessions-narrow", 80, 24, sessions),
        ("view-sessions-compact", 44, 12, sessions),
        ("view-sessions-empty", 80, 12, sessions_empty),
        ("view-settings", 132, 28, settings),
        ("view-settings-narrow", 80, 24, settings),
        ("view-commands", 112, 28, commands),
        ("view-models", 132, 28, models),
        ("view-models-narrow", 80, 24, models),
        ("view-providers", 120, 28, providers),
        ("view-theme", 120, 28, themes),
        ("view-mode", 80, 16, modes),
        ("view-status", 90, 24, status_items),
        ("view-file-picker", 100, 24, files),
        ("view-fleet-dock", 120, 24, fleet),
        ("view-jobs-dock", 120, 24, jobs),
        ("view-files-dock", 120, 24, changed_files),
        ("view-context-dock", 120, 24, context),
        ("view-git-dock", 120, 24, git),
        ("view-cost-dock", 120, 24, cost),
    ]
    .into_iter()
    .map(|(name, width, height, draw)| Entry {
        name,
        width,
        height,
        draw,
    })
    .collect()
}
