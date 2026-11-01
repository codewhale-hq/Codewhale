//! A live roster and focused agent, over facts supplied by the host.
//! Selection and scroll are local; execution, history and permissions are not.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use super::workbench::row;
use crate::{
    AgentCard, CountBar, MotionMode, Paint, Role, State, Theme, VerificationSpinner, WhalePet,
    duration, glyphs, spin, text,
    whale_motion::{Inputs, Stage, Tier},
};
use unicode_segmentation::UnicodeSegmentation;

/// A reported event. Keep these in chronological order; the view follows the
/// newest event until the person scrolls back. Text is sanitized before paint.
#[derive(Clone, Debug)]
pub struct SubagentEvent<'a> {
    pub when: Cow<'a, str>,
    pub state: State,
    pub message: Cow<'a, str>,
}

impl<'a> SubagentEvent<'a> {
    #[must_use]
    pub fn new(
        when: impl Into<Cow<'a, str>>,
        state: State,
        message: impl Into<Cow<'a, str>>,
    ) -> Self {
        Self {
            when: when.into(),
            state,
            message: message.into(),
        }
    }
}

/// Capabilities the owner currently allows. An intent still needs the host's
/// normal validation and permission path; a component never executes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubagentControls {
    pub open: bool,
    pub message: bool,
    pub stop: bool,
}

/// Exact provider receipts. A missing field stays missing, including when
/// another field is explicitly zero. Costs are microdollars, never estimated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubagentUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
}

impl SubagentUsage {
    fn text(self, ascii: bool) -> String {
        let missing = if ascii { "-" } else { "—" };
        let number = |n: Option<u64>| n.map_or_else(|| missing.to_owned(), |n| n.to_string());
        let cost = self.cost_microusd.map_or_else(
            || missing.to_owned(),
            |n| format!("{}.{:06}", n / 1_000_000, n % 1_000_000),
        );
        format!(
            "{} {} / {} {} / ${cost}",
            if ascii { "in" } else { "↓" },
            number(self.input_tokens),
            if ascii { "out" } else { "↑" },
            number(self.output_tokens),
        )
    }
}

/// One retained roster entry. IDs must be nonempty and unique in this roster.
/// The view never expires a worker or infers success from its counts.
#[derive(Clone, Debug)]
pub struct Subagent<'a> {
    pub id: Cow<'a, str>,
    pub card: AgentCard<'a>,
    pub elapsed: Option<Duration>,
    pub tokens: Option<Cow<'a, str>>,
    /// Structured receipts take precedence over legacy token display text.
    pub usage: Option<SubagentUsage>,
    pub steps: Option<u32>,
    /// Full owner-reported result or error; never shortened into the task line.
    pub outcome: Option<Cow<'a, str>>,
    pub progress: Option<CountBar>,
    pub events: Vec<SubagentEvent<'a>>,
    pub performance: Option<Inputs>,
    pub checking: bool,
    pub controls: SubagentControls,
}

impl<'a> Subagent<'a> {
    #[must_use]
    pub fn new(id: impl Into<Cow<'a, str>>, card: AgentCard<'a>) -> Self {
        Self {
            id: id.into(),
            card,
            elapsed: None,
            tokens: None,
            usage: None,
            steps: None,
            outcome: None,
            progress: None,
            events: Vec::new(),
            performance: None,
            checking: false,
            controls: SubagentControls::default(),
        }
    }

    fn usage_text(&self, ascii: bool, steps_label: Option<&str>) -> String {
        [
            self.elapsed
                .map(duration)
                .or_else(|| self.card.elapsed.as_deref().map(str::to_owned)),
            self.usage
                .map(|usage| usage.text(ascii))
                .or_else(|| self.tokens.as_deref().map(str::to_owned)),
            self.steps.map(|n| {
                steps_label.map_or_else(
                    || format!("{n} {}", if n == 1 { "step" } else { "steps" }),
                    |label| format!("{label}: {n}"),
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" / ")
    }
}

/// Returned to the host only. In particular, Stop is a request, not a state
/// transition: the reported worker stays Working until its owner says otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentIntent {
    Open(String),
    Message(String),
    Stop(String),
}

#[derive(Default)]
struct OutputWindow {
    source: String,
    geometry: Option<(u16, u16, usize)>,
    total: usize,
    rows: Vec<String>,
}
impl OutputWindow {
    fn prepare(&mut self, source: &str, width: u16, height: u16, top: usize) {
        if self.source == source && self.geometry == Some((width, height, top)) {
            return;
        }
        self.source.clear();
        self.source.push_str(source);
        self.geometry = Some((width, height, top));
        self.rows.clear();
        self.total = 0;
        for logical in source.split('\n') {
            let safe = text::display_safe(logical);
            let mut rest = safe.as_ref();
            if rest.is_empty() {
                self.push("", top, height);
            }
            while !rest.is_empty() {
                let mut end = 0;
                let mut cells = 0;
                let mut space = None;
                for (byte, grapheme) in rest.grapheme_indices(true) {
                    let count = text::width(grapheme);
                    if cells + count > usize::from(width.max(1)) && cells != 0 {
                        break;
                    }
                    end = byte + grapheme.len();
                    cells += count;
                    if grapheme == " " && !rest[..end].trim_start().is_empty() {
                        space = Some(end);
                    }
                    if cells >= usize::from(width.max(1)) {
                        break;
                    }
                }
                if end < rest.len() {
                    end = space.unwrap_or(end);
                }
                self.push(&rest[..end], top, height);
                rest = &rest[end..];
            }
        }
    }
    fn push(&mut self, row: &str, top: usize, height: u16) {
        if self.total >= top && self.rows.len() < usize::from(height) {
            self.rows.push(row.to_owned());
        }
        self.total += 1;
    }
}

/// Local presentation state for one roster. Feed a new roster when the session
/// changes (or replace this state), retaining completed entries in owner data.
/// Deliberately not Clone: the canonical Stage owns a unique performance clock.
/// Keep one state per surface instead of copying an in-flight Director.
pub struct SubagentViewState {
    selected: Option<String>,
    index: usize,
    offset: usize,
    event_top: Option<usize>,
    event_end: usize,
    output: bool,
    output_top: usize,
    output_end: usize,
    had_outcome: bool,
    output_window: OutputWindow,
    detail: bool,
    visible: bool,
    now: Option<Instant>,
    working: HashMap<String, (bool, Instant)>,
    motion: MotionMode,
    stage: Stage,
    observed: Option<(String, State)>,
    flourish_until: Option<Instant>,
    shown: usize,
    pet_visible: bool,
    rows: Vec<(Rect, String)>,
    detail_area: Rect,
}

impl Default for SubagentViewState {
    fn default() -> Self {
        Self {
            selected: None,
            index: 0,
            offset: 0,
            event_top: None,
            event_end: 0,
            output: false,
            output_top: 0,
            output_end: 0,
            had_outcome: false,
            output_window: OutputWindow::default(),
            detail: false,
            visible: true,
            now: None,
            working: HashMap::new(),
            motion: MotionMode::Full,
            stage: Stage::new(),
            observed: None,
            flourish_until: None,
            shown: 0,
            pet_visible: false,
            rows: Vec::new(),
            detail_area: Rect::default(),
        }
    }
}

impl SubagentViewState {
    #[must_use]
    pub fn selected_id(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        self.selected.as_ref().map(|_| self.index)
    }

    /// Select by identity, preserving focus across owner reordering. Returns
    /// false for an absent, empty or ambiguous ID rather than choosing a worker.
    pub fn select(&mut self, agents: &[Subagent<'_>], id: &str) -> bool {
        if id.is_empty() || agents.iter().filter(|a| a.id == id).count() != 1 {
            return false;
        }
        let index = agents.iter().position(|a| a.id == id).unwrap_or(0);
        if self.selected.as_deref() != Some(id) {
            self.event_top = None;
            self.event_end = 0;
            self.output = agents[index].outcome.is_some();
            self.had_outcome = self.output;
            self.output_window = OutputWindow::default();
            self.output_top = 0;
            self.output_end = 0;
        }
        self.selected = Some(id.to_owned());
        self.index = index;
        true
    }

    /// Observe explicit owner inputs and advance the existing whale clock once.
    /// Paint never reads or advances time. Reduced/Still and terminal states
    /// settle; an observed Working -> Done earns one brief native flourish.
    pub fn update(&mut self, agents: &[Subagent<'_>], now: Instant, motion: MotionMode) {
        self.now = Some(now);
        self.motion = motion;
        // Presentation onsets only: no worker lifecycle or elapsed-time claims
        // are derived from this cache. Drop entries as soon as work stops.
        let active: HashSet<_> = agents
            .iter()
            .filter(|a| a.card.status.state == State::Working)
            .map(|a| a.id.as_ref())
            .collect();
        self.working
            .retain(|id, _| self.visible && active.contains(id.as_str()));
        if self.visible {
            for agent in agents
                .iter()
                .filter(|a| a.card.status.state == State::Working)
            {
                let phase = self
                    .working
                    .entry(agent.id.to_string())
                    .or_insert((agent.checking, now));
                if phase.0 != agent.checking {
                    *phase = (agent.checking, now);
                }
            }
        }
        let retained = self
            .selected
            .clone()
            .is_some_and(|id| self.select(agents, &id));
        if !retained {
            self.selected = None;
            if let Some(agent) = agents.get(self.index.min(agents.len().saturating_sub(1))) {
                self.select(agents, &agent.id);
            }
        }
        let Some(agent) = self.agent(agents) else {
            self.stage.set_visible(false);
            self.observed = None;
            self.flourish_until = None;
            return;
        };
        if agent.outcome.is_some() && !self.had_outcome {
            self.output = true;
            self.output_top = 0;
        } else if agent.outcome.is_none() {
            self.output = false;
        }
        self.had_outcome = agent.outcome.is_some();
        let identity = (agent.id.to_string(), agent.card.status.state);
        if self.observed.as_ref() != Some(&identity) {
            self.flourish_until = self
                .observed
                .as_ref()
                .filter(|(id, status)| {
                    id == &identity.0 && *status == State::Working && identity.1 == State::Done
                })
                .and_then(|_| now.checked_add(Duration::from_millis(1400)));
            self.observed = Some(identity);
        }
        let active = agent.card.status.state == State::Working
            || self.flourish_until.is_some_and(|end| now < end);
        self.stage
            .set_visible(self.visible && agent.performance.is_some());
        if let Some(inputs) = &agent.performance {
            self.stage.observe(
                Some(&agent.id),
                inputs.clone(),
                !motion.animates() || !active,
            );
            self.stage.advance(now);
        }
    }

    fn agent<'a, 'b>(&self, agents: &'a [Subagent<'b>]) -> Option<&'a Subagent<'b>> {
        let agent = agents.get(self.index)?;
        let id = self.selected.as_deref()?;
        (agent.id == id && !id.is_empty() && agents.iter().filter(|a| a.id == id).count() == 1)
            .then_some(agent)
    }

    /// Hiding stops animation; resuming discards the hidden interval.
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.stage.set_visible(visible);
        self.working.clear();
        self.rows.clear();
        self.detail_area = Rect::default();
    }

    fn mark_elapsed(&self, agent: &Subagent<'_>) -> Duration {
        self.now
            .zip(self.working.get(agent.id.as_ref()).map(|(_, start)| *start))
            .map_or(Duration::ZERO, |(now, start)| {
                now.saturating_duration_since(start)
            })
    }

    /// The host waits for this duration or input, whichever arrives first.
    /// Only visible active rows/the focused pet ask for frames. Settled,
    /// hidden, empty and Reduced/Still views ask for no timed redraws.
    #[must_use]
    pub fn next_frame_in(&self, agents: &[Subagent<'_>], theme: &Theme) -> Option<Duration> {
        if !self.visible || !self.motion.animates() {
            return None;
        }
        let marks = agents
            .iter()
            .skip(self.offset)
            .take(self.shown)
            .filter(|a| a.card.status.state == State::Working)
            .filter_map(|agent| spin::next_frame_in(self.mark_elapsed(agent), self.motion))
            .min();
        let pet = self
            .pet_visible
            .then(|| {
                self.stage.cadence(if theme.paints_grounds() {
                    Tier::Hero
                } else {
                    Tier::Terminal
                })
            })
            .flatten();
        [marks, pet].into_iter().flatten().min()
    }

    /// Arrow/Home/End navigation, Tab to the next NeedsYou/Failed worker,
    /// Left/Right for narrow details, O for output/activity, PageUp/PageDown
    /// for the visible content. Full output opens at its beginning.
    /// Enter/M/X return capability-gated Open/Message/Stop intents.
    pub fn handle_key(&mut self, agents: &[Subagent<'_>], key: KeyEvent) -> Option<SubagentIntent> {
        if !self.visible
            || key.kind == KeyEventKind::Release
            || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            return None;
        }
        let index = self
            .selected
            .as_deref()
            .and_then(|id| agents.iter().position(|a| a.id == id))
            .unwrap_or(0);
        if let Some(id) = self.selected.clone() {
            self.select(agents, &id);
        }
        let target = match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(index.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => {
                Some((index + 1).min(agents.len().saturating_sub(1)))
            }
            KeyCode::Home => Some(0),
            KeyCode::End => Some(agents.len().saturating_sub(1)),
            KeyCode::Tab | KeyCode::BackTab => (1..=agents.len())
                .map(|step| {
                    if key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT) {
                        (index + agents.len() - step) % agents.len()
                    } else {
                        (index + step) % agents.len()
                    }
                })
                .find(|i| {
                    matches!(
                        agents[*i].card.status.state,
                        State::NeedsYou | State::Failed
                    )
                }),
            KeyCode::Right => {
                self.detail = true;
                None
            }
            KeyCode::Char(' ') => {
                self.detail = !self.detail;
                None
            }
            KeyCode::Left => {
                self.detail = false;
                None
            }
            KeyCode::Char('o') if self.agent(agents).is_some_and(|a| a.outcome.is_some()) => {
                self.output = !self.output;
                self.detail = true;
                None
            }
            KeyCode::PageUp => {
                self.scroll_content(-5);
                None
            }
            KeyCode::PageDown => {
                self.scroll_content(5);
                None
            }
            _ => None,
        };
        if let Some(agent) = target.and_then(|i| agents.get(i)) {
            self.select(agents, &agent.id);
        }
        let agent = self.agent(agents)?;
        // Holding an action key must not repeat a stop/message request.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match key.code {
            KeyCode::Enter if agent.controls.open => {
                Some(SubagentIntent::Open(agent.id.to_string()))
            }
            KeyCode::Char('m') if agent.controls.message => {
                Some(SubagentIntent::Message(agent.id.to_string()))
            }
            KeyCode::Char('x') if agent.controls.stop => {
                Some(SubagentIntent::Stop(agent.id.to_string()))
            }
            _ => None,
        }
    }

    fn scroll_content(&mut self, lines: isize) {
        if self.output {
            self.output_top = self
                .output_top
                .saturating_add_signed(lines)
                .min(self.output_end);
        } else {
            let top = self
                .event_top
                .unwrap_or(self.event_end)
                .saturating_add_signed(lines);
            self.event_top = (top < self.event_end).then_some(top);
        }
    }

    /// Click selects the painted ID, even if the owner reordered its roster
    /// after paint. Wheel scrolls the pane under the pointer. No mouse action
    /// executes an intent; the host owns opening, messaging and stopping.
    pub fn handle_mouse(&mut self, agents: &[Subagent<'_>], mouse: MouseEvent) -> bool {
        if !self.visible || !mouse.modifiers.is_empty() {
            return false;
        }
        let point = (mouse.column, mouse.row).into();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let id = self
                    .rows
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    .map(|(_, id)| id.clone());
                id.is_some_and(|id| self.select(agents, &id))
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if self.detail_area.contains(point) {
                    self.scroll_content(if up { -3 } else { 3 });
                    true
                } else if self.rows.iter().any(|(area, _)| area.contains(point)) {
                    self.handle_key(
                        agents,
                        KeyEvent::new(
                            if up { KeyCode::Up } else { KeyCode::Down },
                            KeyModifiers::NONE,
                        ),
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Caller-owned labels and hints, including localization. State labels remain
/// those on each existing AgentCard; count wording can be supplied separately.
#[derive(Clone, Debug)]
pub struct SubagentViewWords<'a> {
    pub title: Cow<'a, str>,
    pub empty: Cow<'a, str>,
    pub activity: Cow<'a, str>,
    pub output: Cow<'a, str>,
    pub toggle_output: Cow<'a, str>,
    pub no_events: Cow<'a, str>,
    pub hints: Cow<'a, str>,
    pub open: Cow<'a, str>,
    pub message: Cow<'a, str>,
    pub stop: Cow<'a, str>,
    /// Optional localized receipt label, painted as `label: count`.
    pub steps_label: Option<Cow<'a, str>>,
    /// Override the default English summary with the owner's localized counts.
    pub summary: Option<Cow<'a, str>>,
}

impl Default for SubagentViewWords<'_> {
    fn default() -> Self {
        Self {
            title: "Subagents".into(),
            empty: "No agents reported".into(),
            activity: "Activity".into(),
            output: "Output".into(),
            toggle_output: "O output/activity".into(),
            no_events: "No events reported".into(),
            hints: "Up/Down select   Tab attention   Space details   PgUp/PgDn scroll".into(),
            open: "Enter open".into(),
            message: "M message".into(),
            stop: "X stop".into(),
            steps_label: None,
            summary: None,
        }
    }
}

/// A dynamic extension of the existing Fleet/AgentCard presentation. Wide
/// terminals show roster + detail; narrow terminals switch between them.
///
/// ```no_run
/// use codewhale_ratatui::{AgentCard, MotionMode, State, Subagent, SubagentView, SubagentViewState, Theme};
/// let agents = vec![Subagent::new("review", AgentCard::new("Review", State::Working).task("Check the patch"))];
/// let mut state = SubagentViewState::default();
/// let theme = Theme::detect();
/// state.update(&agents, std::time::Instant::now(), MotionMode::Full);
/// # fn draw(frame: &mut ratatui::Frame<'_>, agents: &[Subagent<'_>], theme: &Theme, state: &mut SubagentViewState) {
/// frame.render_stateful_widget(SubagentView::new(agents, theme), frame.area(), state);
/// # }
/// ```
pub struct SubagentView<'a> {
    agents: &'a [Subagent<'a>],
    theme: &'a Theme,
    words: SubagentViewWords<'a>,
}

impl<'a> SubagentView<'a> {
    #[must_use]
    pub fn new(agents: &'a [Subagent<'a>], theme: &'a Theme) -> Self {
        Self {
            agents,
            theme,
            words: SubagentViewWords::default(),
        }
    }

    #[must_use]
    pub fn words(mut self, words: SubagentViewWords<'a>) -> Self {
        self.words = words;
        self
    }

    /// Pure layout also used by host mouse routing. Detail replaces the roster
    /// on narrow screens; neither is squeezed into an unreadable column.
    #[must_use]
    pub fn areas(area: Rect, detail: bool) -> (Rect, Rect) {
        let body = Rect::new(
            area.x,
            area.y.saturating_add(area.height.min(3)),
            area.width,
            area.height.saturating_sub(5),
        );
        if body.width >= 88 && body.height >= 9 {
            let width = (u32::from(body.width) * 2 / 5).clamp(34, 48) as u16;
            (
                Rect { width, ..body },
                Rect::new(
                    body.x + width + 3,
                    body.y,
                    body.width - width - 3,
                    body.height,
                ),
            )
        } else if detail && body.height >= 5 {
            (Rect::default(), body)
        } else {
            (body, Rect::default())
        }
    }

    fn line(&self, area: Rect, y: u16, value: &str, role: Role, buf: &mut Buffer) {
        if y >= area.height {
            return;
        }
        row(
            Rect {
                y: area.y + y,
                height: 1,
                ..area
            },
            buf,
            &Line::styled(
                text::truncate(
                    &text::display_safe(value),
                    usize::from(area.width),
                    self.theme.ascii(),
                )
                .into_owned(),
                self.theme.fg(role),
            ),
        );
    }

    fn mark(&self, agent: &Subagent<'_>, state: &SubagentViewState) -> &'static str {
        if agent.card.status.state != State::Working {
            agent.card.status.glyph(self.theme)
        } else if agent.checking {
            VerificationSpinner::frame(state.mark_elapsed(agent), state.motion, self.theme.ascii())
        } else {
            spin::frame(state.mark_elapsed(agent), state.motion, self.theme.ascii())
        }
    }

    fn roster(&self, area: Rect, buf: &mut Buffer, state: &mut SubagentViewState) {
        if area.is_empty() {
            state.shown = 0;
            return;
        }
        let size = if area.height >= 8 { 4 } else { 2 };
        // Reserve a final row for the range whenever entries overflow.
        let capacity = usize::from(area.height / size).max(1);
        let clipped = self.agents.len() > capacity;
        state.shown = usize::from(area.height.saturating_sub(u16::from(clipped)) / size).max(1);
        state.offset = state
            .offset
            .min(self.agents.len().saturating_sub(state.shown));
        if state.index < state.offset {
            state.offset = state.index;
        }
        if state.index >= state.offset + state.shown {
            state.offset = state.index + 1 - state.shown;
        }
        let rows = Rect {
            height: area.height.saturating_sub(u16::from(clipped)),
            ..area
        };
        for (index, agent) in self
            .agents
            .iter()
            .enumerate()
            .skip(state.offset)
            .take(state.shown)
        {
            let y = ((index - state.offset) as u16) * size;
            if y >= rows.height {
                break;
            }
            let entry = Rect::new(rows.x, rows.y + y, rows.width, size.min(rows.height - y));
            let selected = state.selected_id() == Some(&*agent.id);
            state.rows.push((entry, agent.id.to_string()));
            if selected {
                buf.set_style(entry, self.theme.bg(Role::Selected));
            }
            let marker = if selected {
                glyphs::pick(glyphs::SELECTION, self.theme.ascii())
            } else {
                " "
            };
            let status = format!(
                "{} {}",
                self.mark(agent, state),
                text::display_safe(&agent.card.status.word)
            );
            let tail = text::width(&status).min(usize::from(entry.width.saturating_sub(4))) as u16;
            let name_width = entry.width.saturating_sub(tail + 3);
            row(
                Rect {
                    width: name_width,
                    height: 1,
                    ..entry
                },
                buf,
                &Line::styled(
                    format!(
                        "{marker} {}",
                        text::truncate(
                            &text::display_safe(&agent.card.title),
                            usize::from(name_width.saturating_sub(2)),
                            self.theme.ascii()
                        )
                    ),
                    self.theme
                        .fg(if selected {
                            Role::Accent
                        } else {
                            Role::Foreground
                        })
                        .add_modifier(Modifier::BOLD),
                ),
            );
            self.line(
                Rect::new(entry.right() - tail, entry.y, tail, 1),
                0,
                &status,
                agent.card.status.state.role(),
                buf,
            );
            let body = Rect::new(
                entry.x + entry.width.min(2),
                entry.y,
                entry.width.saturating_sub(2),
                entry.height,
            );
            self.line(body, 1, &agent.card.task, Role::Foreground, buf);
            if size == 4 {
                let usage = agent.usage_text(self.theme.ascii(), self.words.steps_label.as_deref());
                let facts: Vec<&str> = [
                    agent.card.role.as_deref(),
                    Some(usage.as_str()).filter(|s| !s.is_empty()),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.line(body, 2, &facts.join(" / "), Role::Muted, buf);
            }
        }
        if clipped {
            self.line(
                area,
                area.height - 1,
                &format!(
                    "{}-{} / {}",
                    state.offset + 1,
                    (state.offset + state.shown).min(self.agents.len()),
                    self.agents.len()
                ),
                Role::Muted,
                buf,
            );
        }
    }

    fn wrapped(
        &self,
        area: Rect,
        y: u16,
        value: &str,
        max: u16,
        role: Role,
        buf: &mut Buffer,
    ) -> u16 {
        let height = max.min(area.height.saturating_sub(y));
        if height == 0 || area.width == 0 || value.is_empty() {
            return 0;
        }
        let paragraph = Paragraph::new(text::display_safe(value).into_owned())
            .style(self.theme.fg(role))
            .wrap(Wrap { trim: false });
        let height = paragraph.line_count(area.width).min(usize::from(height)) as u16;
        paragraph.render(Rect::new(area.x, area.y + y, area.width, height), buf);
        height
    }

    fn details(&self, area: Rect, buf: &mut Buffer, state: &mut SubagentViewState) {
        state.pet_visible = false;
        state.detail_area = area;
        if area.is_empty() {
            return;
        }
        let Some(agent) = state.agent(self.agents) else {
            return;
        };
        self.line(area, 0, &agent.card.title, Role::Accent, buf);
        if area.height > 1 {
            row(
                Rect::new(area.x, area.y + 1, area.width, 1),
                buf,
                &Line::from(agent.card.status.spans(self.theme)),
            );
        }
        let identity: Vec<_> = [agent.card.role.as_deref(), agent.card.route.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let mut y = 2;
        y += self.wrapped(area, y, &identity.join(" / "), 2, Role::Muted, buf);
        y += self.wrapped(area, y, &agent.card.task, 3, Role::Foreground, buf);
        y += self.wrapped(
            area,
            y,
            &agent.usage_text(self.theme.ascii(), self.words.steps_label.as_deref()),
            area.height.saturating_sub(y),
            Role::Muted,
            buf,
        );
        if y < area.height
            && let Some(progress) = &agent.progress
        {
            progress.paint(
                Rect::new(area.x, area.y + y, area.width, 1),
                buf,
                self.theme,
            );
            y += 1;
        }
        // Keep at least nine lines for content. The pet is a small companion,
        // never the reason a result or error becomes unreadable.
        if area.height.saturating_sub(y) >= 18
            && area.width >= 26
            && agent.performance.is_some()
            && !self.theme.ascii()
        {
            let width = area.width.min(32);
            WhalePet::new(self.theme)
                .words(agent.card.status.word.to_string())
                .paint(
                    Rect::new(area.x + (area.width - width) / 2, area.y + y, width, 9),
                    buf,
                    &mut state.stage,
                );
            state.pet_visible = true;
            y += 9;
        } else if y < area.height {
            y += 1;
        }
        if y >= area.height {
            return;
        }
        let output = state.output && agent.outcome.is_some();
        let label = if output {
            &self.words.output
        } else {
            &self.words.activity
        };
        let title = if agent.outcome.is_some() {
            format!("{label}   /   {}", self.words.toggle_output)
        } else {
            label.to_string()
        };
        self.line(area, y, &title, Role::Accent, buf);
        y += 1;
        if y >= area.height {
            return;
        }
        let body = Rect::new(area.x, area.y + y, area.width, area.height - y);
        // Cache only the visible window, never a String per physical row.
        // Long results keep bounded paint memory, including newline-heavy data.
        if output
            && (state.output_window.source != agent.outcome.as_deref().unwrap_or("")
                || state
                    .output_window
                    .geometry
                    .is_none_or(|g| g.0 != body.width))
        {
            state.output_window.prepare(
                agent.outcome.as_deref().unwrap_or(""),
                body.width,
                body.height,
                state.output_top,
            );
        }
        let paragraph = if output {
            None
        } else {
            if agent.events.is_empty() {
                self.line(body, 0, &self.words.no_events, Role::Muted, buf);
                state.event_end = 0;
                state.event_top = None;
                return;
            }
            let lines: Vec<_> = agent
                .events
                .iter()
                .map(|event| {
                    Line::from(vec![
                        Span::styled(
                            format!("{} ", text::display_safe(&event.when)),
                            self.theme.fg(Role::Muted),
                        ),
                        Span::styled(
                            format!("{} ", glyphs::pick(event.state.glyph(), self.theme.ascii())),
                            self.theme.fg(event.state.role()),
                        ),
                        Span::styled(
                            text::display_safe(&event.message).into_owned(),
                            self.theme.fg(Role::Foreground),
                        ),
                    ])
                })
                .collect();
            Some(Paragraph::new(lines).wrap(Wrap { trim: false }))
        };
        let total = if output {
            state.output_window.total
        } else {
            paragraph.as_ref().map_or(0, |p| p.line_count(body.width))
        };
        // Reserve a range indicator only when there is content to scroll.
        let clipped = total > usize::from(body.height) && body.height > 1;
        let content = Rect {
            height: body.height.saturating_sub(u16::from(clipped)),
            ..body
        };
        let end = total.saturating_sub(usize::from(content.height));
        let top = if output {
            state.output_end = end;
            state.output_top = state.output_top.min(end);
            state.output_top
        } else {
            state.event_end = end;
            state.event_top = state.event_top.map(|top| top.min(end));
            state.event_top.unwrap_or(end)
        };
        if output {
            state.output_window.prepare(
                agent.outcome.as_deref().unwrap_or(""),
                body.width,
                content.height,
                top,
            );
            for (y, line) in state
                .output_window
                .rows
                .iter()
                .take(usize::from(content.height))
                .enumerate()
            {
                self.line(content, y as u16, line, Role::Foreground, buf);
            }
        } else if let Some(paragraph) = paragraph {
            paragraph
                .scroll((top.min(usize::from(u16::MAX)) as u16, 0))
                .render(content, buf);
        }
        if clipped {
            self.line(
                body,
                body.height - 1,
                &format!(
                    "{}-{} / {}",
                    top + 1,
                    (top + usize::from(content.height)).min(total),
                    total
                ),
                Role::Muted,
                buf,
            );
        }
    }
}

impl StatefulWidget for SubagentView<'_> {
    type State = SubagentViewState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let area = area.intersection(buf.area);
        state.shown = 0;
        state.pet_visible = false;
        state.rows.clear();
        state.detail_area = Rect::default();
        if area.is_empty() || !state.visible {
            return;
        }
        buf.set_style(
            area,
            self.theme
                .bg(Role::Background)
                .patch(self.theme.fg(Role::Foreground)),
        );
        self.line(area, 0, &self.words.title, Role::Accent, buf);
        let count = |status| {
            self.agents
                .iter()
                .filter(|a| a.card.status.state == status)
                .count()
        };
        let summary = self
            .words
            .summary
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let mut counts: Vec<_> = State::ALL
                    .into_iter()
                    .filter_map(|status| {
                        let n = count(status);
                        (n > 0).then(|| format!("{n} {}", status.word().to_lowercase()))
                    })
                    .collect();
                counts.push(format!("{} total", self.agents.len()));
                counts.join(" / ")
            });
        self.line(area, 1, &summary, Role::Muted, buf);
        let (roster, detail) = Self::areas(area, state.detail);
        if self.agents.is_empty() {
            self.line(
                if roster.is_empty() { detail } else { roster },
                0,
                &self.words.empty,
                Role::Muted,
                buf,
            );
        } else {
            self.roster(roster, buf, state);
            self.details(detail, buf, state);
        }
        if area.height >= 3 {
            self.line(area, area.height - 1, &self.words.hints, Role::Muted, buf);
            let mut actions = Vec::new();
            if let Some(agent) = state.agent(self.agents) {
                if agent.controls.open {
                    actions.push(self.words.open.as_ref());
                }
                if agent.controls.message {
                    actions.push(self.words.message.as_ref());
                }
                if agent.controls.stop {
                    actions.push(self.words.stop.as_ref());
                }
            }
            self.line(
                area,
                area.height - 2,
                &actions.join("   "),
                Role::Accent,
                buf,
            );
        }
    }
}

#[cfg(test)]
mod output_window_tests {
    use super::OutputWindow;
    #[test]
    fn newline_heavy_output_retains_only_the_viewport_and_reuses_it() {
        let source = "\n".repeat(8 * 1024 * 1024);
        let mut window = OutputWindow::default();
        window.prepare(&source, 30, 12, 0);
        assert_eq!(window.total, source.len() + 1);
        assert_eq!(window.rows.len(), 12);
        let allocation = window.rows.as_ptr();
        window.prepare(&source, 30, 12, 0);
        assert_eq!(window.rows.as_ptr(), allocation);
    }
    #[test]
    fn exact_width_output_preserves_only_source_blank_lines() {
        let mut window = OutputWindow::default();
        window.prepare("12345\ndone\n\n", 5, 10, 0);
        assert_eq!(window.rows, ["12345", "done", "", ""]);
    }
}
