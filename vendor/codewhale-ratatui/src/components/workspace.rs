//! Transcript, composer and fleet surfaces over caller-owned data.
//!
//! These are renderers, not another session or input runtime. Hosts own the
//! author, task, route, state, elapsed words and text editing. The visual
//! grammar comes from the Engine's `history/message.rs`, `tool_card.rs`,
//! `agent_card.rs` and `composer_chrome.rs`: a speaker anchor, continuation
//! rails, semantic tool summaries, agent identity and a quiet input ledge.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Depth, HorizonRule, KeyHints, Paint, Panel, Role, State, StatusMark, Theme, glyphs, text,
};

/// A plain-text transcript entry. The host supplies its author and marker;
/// this component neither parses Markdown nor guesses who sent the message.
/// [`Message::native`] uses the current TUI's inline speaker mark and dim
/// continuation rail. A nonempty author adds the optional heading variation.
#[derive(Clone, Debug)]
pub struct Message<'a> {
    pub author: Cow<'a, str>,
    pub body: Cow<'a, str>,
    pub author_role: Role,
    pub marker: &'static str,
}

impl<'a> Message<'a> {
    #[must_use]
    pub fn new(author: impl Into<Cow<'a, str>>, body: impl Into<Cow<'a, str>>) -> Self {
        Self {
            author: author.into(),
            body: body.into(),
            author_role: Role::Foreground,
            marker: glyphs::CURRENT,
        }
    }

    /// The current Codewhale transcript's plain-text message decoration:
    /// `● body` first, then `▏ ` before each continuation. The host can choose
    /// another speaker with [`Self::marker`] and its ink with [`Self::role`].
    /// Whitespace-only assistant entries render nothing. No Markdown parser,
    /// streaming clock, or session state is introduced by this constructor.
    #[must_use]
    pub fn native(body: impl Into<Cow<'a, str>>) -> Self {
        Self::new("", body).role(Role::Accent)
    }

    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.author_role = role;
        self
    }

    /// Use a charter mark such as [`glyphs::USER`] for a user-authored entry.
    #[must_use]
    pub fn marker(mut self, marker: &'static str) -> Self {
        self.marker = marker;
        self
    }

    fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        if self.author.is_empty() {
            return self.native_lines(width, theme);
        }
        let mut lines = vec![Line::from(vec![
            Span::styled(
                safe(glyphs::pick(self.marker, theme.ascii())),
                theme.fg(self.author_role),
            ),
            Span::raw(" "),
            Span::styled(
                safe(&self.author),
                theme.fg(self.author_role).add_modifier(Modifier::BOLD),
            ),
        ])];
        let rail = width >= 3;
        let body = wrapped(
            &self.body,
            width.saturating_sub(if rail { 2 } else { 0 }),
            theme.ascii(),
        );
        lines.extend(body_lines(&body, rail, theme, Role::Foreground));
        lines
    }

    fn native_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        // The native assistant skips stray blank streaming cells, including
        // caller text whose only visible content disappears during sanitizing.
        if self.marker == glyphs::CURRENT
            && self
                .body
                .split('\n')
                .all(|line| text::display_safe(line).trim().is_empty())
        {
            return Vec::new();
        }
        let marker = safe(glyphs::pick(self.marker, theme.ascii()));
        let marker_width = text::width(&marker);
        // Native message.rs reserves prefix + two cells before wrapping,
        // while the painted prefix itself occupies prefix + one cell.
        let reserved = u16::try_from(marker_width.saturating_add(2)).unwrap_or(u16::MAX);
        let mut body = wrapped(
            &self.body,
            width.saturating_sub(reserved).max(1),
            theme.ascii(),
        );
        if body.lines.is_empty() {
            body.lines.push(String::new());
        }
        body.lines
            .into_iter()
            .enumerate()
            .map(|(index, body)| {
                let mut spans = Vec::new();
                if !marker.is_empty() {
                    if index == 0 {
                        spans.push(Span::styled(
                            marker.clone(),
                            theme.fg(self.author_role).add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::raw(" "));
                    } else {
                        let rail = glyphs::pick(glyphs::TRANSCRIPT_RAIL, theme.ascii());
                        spans.push(Span::styled(
                            format!("{}{}", rail.trim_end(), " ".repeat(marker_width)),
                            theme.fg(Role::Dim),
                        ));
                    }
                }
                spans.push(Span::styled(body, theme.fg(Role::Foreground)));
                Line::from(spans)
            })
            .collect()
    }
}

impl Paint for Message<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        for (index, line) in self
            .lines(area.width, theme)
            .iter()
            .take(usize::from(area.height))
            .enumerate()
        {
            super::workbench::row(
                Rect {
                    y: area.y.saturating_add(rows(index)),
                    height: 1,
                    ..area
                },
                buf,
                line,
            );
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        rows(self.lines(width, theme).len())
    }
}

/// A tool preview with a semantic title and subject supplied by the host.
///
/// `omitted_lines` counts logical output lines the host removed before
/// supplying `output`. If the viewport clips output, the footer also counts
/// logical lines whose full content is not visible (including a partially
/// visible final line). A wrapped line is never counted twice.
#[derive(Clone, Debug)]
pub struct ToolCard<'a> {
    pub title: Cow<'a, str>,
    pub subject: Cow<'a, str>,
    pub status: StatusMark,
    pub elapsed: Option<Cow<'a, str>>,
    pub output: Cow<'a, str>,
    pub omitted_lines: usize,
    /// Words after the count. Hosts can supply translated copy.
    pub omission_label: Cow<'a, str>,
}

impl<'a> ToolCard<'a> {
    #[must_use]
    pub fn new(
        title: impl Into<Cow<'a, str>>,
        subject: impl Into<Cow<'a, str>>,
        state: State,
    ) -> Self {
        Self {
            title: title.into(),
            subject: subject.into(),
            status: StatusMark::new(state),
            elapsed: None,
            output: Cow::Borrowed(""),
            omitted_lines: 0,
            omission_label: Cow::Borrowed("output lines not fully shown"),
        }
    }

    #[must_use]
    pub fn output(mut self, output: impl Into<Cow<'a, str>>) -> Self {
        self.output = output.into();
        self
    }

    #[must_use]
    pub fn elapsed(mut self, elapsed: impl Into<Cow<'a, str>>) -> Self {
        self.elapsed = Some(elapsed.into());
        self
    }

    #[must_use]
    pub fn omitted_lines(mut self, count: usize) -> Self {
        self.omitted_lines = count;
        self
    }

    #[must_use]
    pub fn omission_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.omission_label = label.into();
        self
    }

    #[must_use]
    pub fn status_word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.status = self.status.word(word);
        self
    }

    fn heading(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = vec![activity_header(
            &self.title,
            &self.status,
            self.elapsed.as_deref(),
            theme,
        )];
        let subject = wrapped(&self.subject, width, theme.ascii());
        lines.extend(
            subject
                .lines
                .into_iter()
                .map(|line| Line::from(Span::styled(line, theme.fg(Role::Accent)))),
        );
        lines
    }
}

impl Paint for ToolCard<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let inner = Panel::new(Depth::Raised).draw(area, buf, theme);
        if inner.is_empty() {
            return;
        }
        let heading = self.heading(inner.width, theme);
        paint_lines(&heading, inner, buf);
        let output_area = below(inner, rows(heading.len()));
        if output_area.is_empty() {
            return;
        }
        let rail = output_area.width >= 3;
        let output = wrapped(
            &self.output,
            output_area.width.saturating_sub(if rail { 2 } else { 0 }),
            theme.ascii(),
        );
        let clipped = output.lines.len() > usize::from(output_area.height);
        let needs_footer =
            self.omitted_lines > 0 || clipped || output.hidden_lines(output.lines.len()) > 0;
        let visible = usize::from(output_area.height.saturating_sub(u16::from(needs_footer)));
        let shown = visible.min(output.lines.len());
        let omitted = self
            .omitted_lines
            .saturating_add(output.hidden_lines(shown));
        let output_lines = body_lines(&output, rail, theme, Role::Muted);
        paint_lines(
            &output_lines,
            Rect {
                height: rows(shown),
                ..output_area
            },
            buf,
        );
        if needs_footer && omitted > 0 {
            let footer = Line::from(Span::styled(
                format!("{omitted} {}", safe(&self.omission_label)),
                theme.fg(Role::Hint),
            ));
            footer.render(
                Rect {
                    y: output_area.y.saturating_add(rows(shown)),
                    height: 1,
                    ..output_area
                },
                buf,
            );
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        let inner = panel_width(width, theme);
        if inner == 0 {
            return panel_height(0, width, theme);
        }
        let output = wrapped(
            &self.output,
            inner.saturating_sub(if inner >= 3 { 2 } else { 0 }),
            theme.ascii(),
        );
        let footer =
            usize::from(self.omitted_lines > 0 || output.hidden_lines(output.lines.len()) > 0);
        panel_height(
            rows(
                self.heading(inner, theme)
                    .len()
                    .saturating_add(output.lines.len())
                    .saturating_add(footer),
            ),
            width,
            theme,
        )
    }
}

/// Display-only input chrome. Text, placeholder, context and hint words are
/// caller-owned. The host continues to own editing, cursor placement, history,
/// completion, submission predicates and input handling.
///
/// ```no_run
/// use codewhale_ratatui::{Composer, Paint, Theme};
/// # fn draw(frame: &mut ratatui::Frame<'_>) {
/// let theme = Theme::detect();
/// let composer = Composer::new("Review the changes")
///     .context("codewhale-ratatui / main");
/// frame.render_widget(composer.themed(&theme), frame.area());
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Composer<'a> {
    pub text: Cow<'a, str>,
    pub placeholder: Cow<'a, str>,
    pub context: Option<Cow<'a, str>>,
    pub hints: Option<&'a KeyHints>,
    pub focused: bool,
    pub prompt: &'static str,
}

impl<'a> Composer<'a> {
    #[must_use]
    pub fn new(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            placeholder: Cow::Borrowed("Ask Codewhale to do something"),
            context: None,
            hints: None,
            focused: false,
            prompt: "›",
        }
    }

    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'a, str>>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// A host-provided repository, branch, mode or route summary. Nothing is
    /// discovered or inferred by the component.
    #[must_use]
    pub fn context(mut self, context: impl Into<Cow<'a, str>>) -> Self {
        self.context = Some(context.into());
        self
    }

    #[must_use]
    pub fn hints(mut self, hints: &'a KeyHints) -> Self {
        self.hints = Some(hints);
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn prompt(mut self, prompt: &'static str) -> Self {
        self.prompt = prompt;
        self
    }

    fn input_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let prompt = safe(glyphs::pick(self.prompt, theme.ascii()));
        let lead = rows(text::width(&prompt).saturating_add(1)).min(width.saturating_sub(1));
        let placeholder = self.text.is_empty();
        let content = if placeholder {
            &self.placeholder
        } else {
            &self.text
        };
        let input = wrapped(content, width.saturating_sub(lead), theme.ascii());
        let body_role = if placeholder {
            Role::Hint
        } else {
            Role::Foreground
        };
        let prompt_role = if self.focused {
            Role::Accent
        } else {
            Role::Muted
        };
        let mut lines: Vec<Line<'static>> = input
            .lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let prefix = if index == 0 {
                    text::pad(&prompt, usize::from(lead), theme.ascii())
                } else {
                    " ".repeat(usize::from(lead))
                };
                Line::from(vec![
                    Span::styled(prefix, theme.fg(prompt_role)),
                    Span::styled(line, theme.fg(body_role)),
                ])
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines
    }
}

impl Paint for Composer<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let mut input_area = area;
        // Compact viewports give their sole row to text before chrome.
        if area.height >= 2 {
            let mut horizon = HorizonRule::new();
            if let Some(context) = &self.context {
                horizon = horizon.label(context.clone());
            }
            horizon.paint(Rect { height: 1, ..area }, buf, theme);
            input_area = below(area, 1);
        }
        let hints = self
            .hints
            .map(|hints| hints.lines(area.width, theme))
            .unwrap_or_default();
        let input = self.input_lines(input_area.width, theme);
        // Reserve a hints rail only when a row of caller text still fits.
        let hints_height = rows(hints.len()).min(input_area.height.saturating_sub(1));
        let text_area = Rect {
            height: input_area.height.saturating_sub(hints_height),
            ..input_area
        };
        paint_lines(&input, text_area, buf);
        if hints_height > 0 {
            paint_lines(
                &hints,
                Rect {
                    y: text_area.bottom(),
                    height: hints_height,
                    ..input_area
                },
                buf,
            );
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        let hints = self.hints.map_or(0, |hints| hints.height(width, theme));
        1_u16
            .saturating_add(rows(self.input_lines(width, theme).len()))
            .saturating_add(hints)
    }
}

/// One agent's own identity and reported work. The component has no mailbox,
/// timers, provider routing, state transitions or inherited parent task.
#[derive(Clone, Debug)]
pub struct AgentCard<'a> {
    pub title: Cow<'a, str>,
    pub status: StatusMark,
    pub role: Option<Cow<'a, str>>,
    pub route: Option<Cow<'a, str>>,
    pub task: Cow<'a, str>,
    pub elapsed: Option<Cow<'a, str>>,
}

impl<'a> AgentCard<'a> {
    #[must_use]
    pub fn new(title: impl Into<Cow<'a, str>>, state: State) -> Self {
        Self {
            title: title.into(),
            status: StatusMark::new(state),
            role: None,
            route: None,
            task: Cow::Borrowed(""),
            elapsed: None,
        }
    }

    #[must_use]
    pub fn role(mut self, role: impl Into<Cow<'a, str>>) -> Self {
        self.role = Some(role.into());
        self
    }

    #[must_use]
    pub fn route(mut self, route: impl Into<Cow<'a, str>>) -> Self {
        self.route = Some(route.into());
        self
    }

    #[must_use]
    pub fn task(mut self, task: impl Into<Cow<'a, str>>) -> Self {
        self.task = task.into();
        self
    }

    #[must_use]
    pub fn elapsed(mut self, elapsed: impl Into<Cow<'a, str>>) -> Self {
        self.elapsed = Some(elapsed.into());
        self
    }

    #[must_use]
    pub fn status_word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.status = self.status.word(word);
        self
    }

    fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let mut lines = vec![activity_header(
            &self.title,
            &self.status,
            self.elapsed.as_deref(),
            theme,
        )];
        let identity: Vec<_> = [self.role.as_deref(), self.route.as_deref()]
            .into_iter()
            .flatten()
            .map(safe)
            .collect();
        if !identity.is_empty() {
            let identity = wrapped(&identity.join(" / "), width, theme.ascii());
            lines.extend(
                identity
                    .lines
                    .into_iter()
                    .map(|line| Line::from(Span::styled(line, theme.fg(Role::Muted)))),
            );
        }
        let rail = width >= 3;
        let task = wrapped(
            &self.task,
            width.saturating_sub(if rail { 2 } else { 0 }),
            theme.ascii(),
        );
        lines.extend(body_lines(&task, rail, theme, Role::Foreground));
        lines
    }
}

impl Paint for AgentCard<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let inner = Panel::new(Depth::Raised).draw(area, buf, theme);
        paint_lines(&self.lines(inner.width, theme), inner, buf);
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        panel_height(
            rows(self.lines(panel_width(width, theme), theme).len()),
            width,
            theme,
        )
    }
}

/// A compact fleet rail. Each entry retains its own reported status, role,
/// route, task and elapsed text; no fleet-wide success is inferred.
#[derive(Clone, Debug)]
pub struct Fleet<'a> {
    pub title: Cow<'a, str>,
    pub agents: Vec<AgentCard<'a>>,
    pub empty_label: Cow<'a, str>,
    pub omission_label: Cow<'a, str>,
}

impl<'a> Fleet<'a> {
    #[must_use]
    pub fn new(agents: Vec<AgentCard<'a>>) -> Self {
        Self {
            title: Cow::Borrowed("Fleet"),
            agents,
            empty_label: Cow::Borrowed("No agents reported"),
            omission_label: Cow::Borrowed("agents not fully shown"),
        }
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn empty_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.empty_label = label.into();
        self
    }

    #[must_use]
    pub fn omission_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.omission_label = label.into();
        self
    }
}

impl Paint for Fleet<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let inner = Panel::new(Depth::Deep).draw(area, buf, theme);
        if inner.is_empty() {
            return;
        }
        Line::from(Span::styled(
            safe(&self.title),
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        ))
        .render(Rect { height: 1, ..inner }, buf);
        let body = below(inner, 1);
        if body.is_empty() {
            return;
        }
        if self.agents.is_empty() {
            Line::from(Span::styled(safe(&self.empty_label), theme.fg(Role::Hint)))
                .render(body, buf);
            return;
        }
        let mut lines = Vec::new();
        let mut ends = Vec::new();
        for agent in &self.agents {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.extend(agent.lines(body.width, theme));
            ends.push(lines.len());
        }
        let clipped = lines.len() > usize::from(body.height);
        let visible = usize::from(body.height.saturating_sub(u16::from(clipped)));
        paint_lines(
            &lines,
            Rect {
                height: rows(visible),
                ..body
            },
            buf,
        );
        if clipped {
            let hidden = ends
                .len()
                .saturating_sub(ends.partition_point(|end| *end <= visible));
            Line::from(Span::styled(
                format!("{hidden} {}", safe(&self.omission_label)),
                theme.fg(Role::Hint),
            ))
            .render(
                Rect {
                    y: body.y.saturating_add(rows(visible)),
                    height: 1,
                    ..body
                },
                buf,
            );
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        let inner_width = width.saturating_sub(if width >= 8 { 2 } else { 0 });
        let content = if self.agents.is_empty() {
            1
        } else {
            self.agents
                .iter()
                .map(|agent| agent.lines(inner_width, theme).len())
                .sum::<usize>()
                .saturating_add(self.agents.len().saturating_sub(1))
        };
        rows(content.saturating_add(1))
    }
}

// Splitting logical lines before display_safe preserves authored line breaks
// while rejecting terminal controls and bidi overrides within each line.
fn safe(value: &str) -> String {
    text::display_safe(value).into_owned()
}

#[derive(Default)]
struct Wrapped {
    lines: Vec<String>,
    /// End-exclusive row index and whether every grapheme fitted.
    ends: Vec<(usize, bool)>,
}

impl Wrapped {
    fn hidden_lines(&self, visible: usize) -> usize {
        self.ends
            .iter()
            .filter(|(end, complete)| *end > visible || !complete)
            .count()
    }
}

fn wrapped(value: &str, width: u16, ascii: bool) -> Wrapped {
    let mut result = Wrapped::default();
    if value.is_empty() || width == 0 {
        return result;
    }
    let width = usize::from(width);
    for source in value.split('\n') {
        let source = safe(source);
        let mut current = String::new();
        let mut cells = 0usize;
        let mut complete = true;
        for grapheme in source.graphemes(true) {
            let size = text::width(grapheme);
            if cells.saturating_add(size) > width && !current.is_empty() {
                // Prefer an authored whitespace boundary. Long words still
                // wrap between whole graphemes; indentation stays intact.
                let boundary = current
                    .grapheme_indices(true)
                    .filter(|(_, cluster)| cluster.chars().all(char::is_whitespace))
                    .map(|(index, cluster)| index + cluster.len())
                    .next_back()
                    .filter(|end| {
                        current[..*end]
                            .chars()
                            .any(|character| !character.is_whitespace())
                    });
                if let Some(end) = boundary {
                    let tail = current.split_off(end);
                    result.lines.push(std::mem::replace(&mut current, tail));
                    cells = text::width(&current);
                } else {
                    result.lines.push(std::mem::take(&mut current));
                    cells = 0;
                }
                if cells.saturating_add(size) > width && !current.is_empty() {
                    result.lines.push(std::mem::take(&mut current));
                    cells = 0;
                }
            }
            if size > width {
                // A wide grapheme cannot fit in a one-cell viewport. Show an
                // explicit cut mark, never split it or overwrite the neighbor.
                result
                    .lines
                    .push(text::truncate(grapheme, width, ascii).into_owned());
                complete = false;
            } else {
                current.push_str(grapheme);
                cells = cells.saturating_add(size);
            }
        }
        if !current.is_empty() || source.is_empty() {
            result.lines.push(current);
        }
        result.ends.push((result.lines.len(), complete));
    }
    result
}

fn body_lines(body: &Wrapped, rail: bool, theme: &Theme, role: Role) -> Vec<Line<'static>> {
    body.lines
        .iter()
        .map(|line| {
            let mut spans = Vec::new();
            if rail {
                spans.push(Span::styled(
                    glyphs::pick(glyphs::TRANSCRIPT_RAIL, theme.ascii()),
                    theme.fg(Role::Border),
                ));
            }
            spans.push(Span::styled(line.clone(), theme.fg(role)));
            Line::from(spans)
        })
        .collect()
}

fn activity_header(
    title: &str,
    status: &StatusMark,
    elapsed: Option<&str>,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(status.glyph(theme), theme.fg(status.state.role())),
        Span::raw(" "),
        Span::styled(
            safe(title),
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(safe(&status.word), theme.fg(status.state.role())),
    ];
    if let Some(elapsed) = elapsed {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(safe(elapsed), theme.fg(Role::Muted)));
    }
    Line::from(spans)
}

fn paint_lines(lines: &[Line<'_>], area: Rect, buf: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    for (index, line) in lines.iter().take(usize::from(area.height)).enumerate() {
        line.render(
            Rect {
                y: area.y.saturating_add(rows(index)),
                height: 1,
                ..area
            },
            buf,
        );
    }
}

fn below(area: Rect, used: u16) -> Rect {
    let used = used.min(area.height);
    Rect {
        y: area.y.saturating_add(used),
        height: area.height.saturating_sub(used),
        ..area
    }
}

fn rows(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

fn panel_width(width: u16, theme: &Theme) -> u16 {
    let inner = width.saturating_sub(if raised_edged(theme) { 2 } else { 0 });
    inner.saturating_sub(if inner >= 8 { 2 } else { 0 })
}

fn panel_height(content: u16, width: u16, theme: &Theme) -> u16 {
    if width == 0 {
        return 0;
    }
    if !raised_edged(theme) {
        return content;
    }
    let bordered = content.saturating_add(2);
    bordered.saturating_add(if bordered >= 6 { 2 } else { 0 })
}

fn raised_edged(theme: &Theme) -> bool {
    !theme.grounds_differ(Role::Surface, Role::Background)
}
