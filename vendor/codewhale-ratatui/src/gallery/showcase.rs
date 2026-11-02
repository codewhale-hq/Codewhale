//! An authored, interactive specimen of the public kit. All state is supplied
//! by a host; no provider, filesystem, execution or timer lives in this view.
use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
    style::Modifier,
    text::{Line, Span},
};

use super::Entry;
use crate::{
    ApprovalCard, ApprovalChoice, ApprovalEffect, ApprovalKey, ApprovalKind, ApprovalScope,
    ApprovalState, ApprovalSubject, ChoiceId, Depth, Diff, DiffGutter, DiffWrap, Form, FormField,
    FormState, Habitat, HabitatDensity, HorizonRule, Message, MetricKind, MetricSegment,
    MetricsLine, MotionMode, NativeComposer, NativeComposerDensity, OceanColumn, OceanPhase, Ombre,
    OmbreDirection, Paint, PaneHeader, Picker, PickerItem, PickerMatches, PickerState, PostureBar,
    Receipt, ReceiptValue, Role, Segmented, SegmentedState, SettingDetail, SettingRow, Spinner,
    State, StatusMark, TerminalShell, TextInputState, Theme, Toggle, ToggleState, TuiGround,
    TuiInk, TuiPalette, VerificationSpinner, WaterPalette, Whale, WhaleState, Workbar,
    WorkbarAgent, WorkbarPanel, WorkbarRow, WorkbarState, WorkbarTab, WorkbarTone,
    WorkflowProgress, WorkflowRun, WorkflowRunState, parse_unified,
    testing::Profile,
    text,
    whale_motion::{Activity, ColoredGrid, Context, Inputs, Presence, Stage, colored_braille},
};

/// Rows for the standalone workbar gallery. The Work view uses
/// [`ShowcaseState::workbar`] for its phase-aware fixture.
#[must_use]
pub fn workbar_rows(panel: WorkbarPanel) -> Vec<crate::WorkbarRow> {
    super::workbar::sample(panel).rows
}

/// The six destinations of the demonstration. The last exposes every
/// existing catalogue entry, rather than selecting a second component list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShowcaseSection {
    #[default]
    Work,
    Decisions,
    Controls,
    Color,
    Life,
    Components,
}
impl ShowcaseSection {
    pub const ALL: [Self; 6] = [
        Self::Work,
        Self::Decisions,
        Self::Controls,
        Self::Color,
        Self::Life,
        Self::Components,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Decisions => "Decisions",
            Self::Controls => "Controls",
            Self::Color => "Color",
            Self::Life => "Life",
            Self::Components => "Components",
        }
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// Explicit host-owned fixture phases. Advancing this never runs a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShowcasePhase {
    #[default]
    Working,
    NeedsYou,
    Verifying,
    Done,
}
impl ShowcasePhase {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::Verifying => "Verifying",
            Self::Done => "Done",
        }
    }
    pub const fn state(self) -> State {
        match self {
            Self::Working | Self::Verifying => State::Working,
            Self::NeedsYou => State::NeedsYou,
            Self::Done => State::Done,
        }
    }
    pub const fn whale(self) -> WhaleState {
        match self {
            Self::Working => WhaleState::Write,
            Self::NeedsYou => WhaleState::NeedsYou,
            Self::Verifying => WhaleState::Run,
            Self::Done => WhaleState::Done,
        }
    }
}

/// Native permission labels for the specimen. These never grant the gallery
/// host access to a command, provider or filesystem operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShowcasePermission {
    #[default]
    Ask,
    AutoReview,
    FullAccess,
}
impl ShowcasePermission {
    pub const fn next(self) -> Self {
        match self {
            Self::Ask => Self::AutoReview,
            Self::AutoReview => Self::FullAccess,
            Self::FullAccess => Self::Ask,
        }
    }
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoReview => "auto-review",
            Self::FullAccess => "full access",
        }
    }
    pub const fn ink(self) -> TuiInk {
        match self {
            Self::Ask => TuiInk::PermissionAsk,
            Self::AutoReview => TuiInk::PermissionAutoReview,
            Self::FullAccess => TuiInk::PermissionFullAccess,
        }
    }
}

/// State kept by the example host. Its clock is an elapsed Duration, supplied
/// to the renderer; the fields do not infer account, cost or execution state.
#[derive(Clone, Debug)]
pub struct ShowcaseState {
    pub section: ShowcaseSection,
    pub phase: ShowcasePhase,
    pub phase_started: Duration,
    pub palette: WaterPalette,
    pub direction: OmbreDirection,
    pub profile: Profile,
    pub motion: MotionMode,
    pub focus: usize,
    pub editing: bool,
    pub permission: ShowcasePermission,
    pub queued: Vec<String>,
    pub followups: Vec<String>,
    pub details: ToggleState,
    pub readouts: ToggleState,
    pub draft: TextInputState,
    pub form: FormState,
    pub approval: ApprovalState,
    pub approval_open: bool,
    pub note: String,
    pub action: usize,
    pub action_playing: bool,
    pub query: TextInputState,
    pub picker: PickerState,
    pub preview_scroll: u16,
    pub dock: WorkbarState,
    pub native_palette: TuiPalette,
}
impl Default for ShowcaseState {
    fn default() -> Self {
        Self::new()
    }
}
impl ShowcaseState {
    /// Resolve one palette for the character, surface and exported frame.
    #[must_use]
    pub fn theme_for(&self, input: &Theme) -> Theme {
        let palette = if self.native_palette == TuiPalette::Underwater
            && input.caps().appearance == crate::detect::Appearance::Light
        {
            TuiPalette::WhaleLight
        } else {
            self.native_palette
        };
        super::tui_palettes::theme_for(palette, input)
    }

    pub fn new() -> Self {
        Self {
            section: ShowcaseSection::Work,
            phase: ShowcasePhase::Working,
            phase_started: Duration::ZERO,
            palette: WaterPalette::Ocean,
            direction: OmbreDirection::Diagonal,
            profile: Profile::DarkTrue,
            motion: MotionMode::Full,
            focus: 0,
            editing: true,
            permission: ShowcasePermission::Ask,
            queued: Vec::new(),
            followups: Vec::new(),
            details: ToggleState::from(false),
            readouts: ToggleState::from(true),
            draft: TextInputState::with_text("Also preserve the search shortcut."),
            form: FormState::new(vec![
                FormField::text("Project")
                    .value("codewhale-ratatui")
                    .validate(project_name),
                FormField::text("Result title")
                    .value("A quieter workspace")
                    .validate(project_name),
                FormField::check("Keep the receipt", true),
                FormField::read_only("Execution", "Preview"),
            ]),
            approval: approval_choices(),
            approval_open: false,
            note: "Choose a component to explore.".into(),
            action: 0,
            action_playing: false,
            query: TextInputState::new(),
            picker: PickerState::default(),
            preview_scroll: 0,
            dock: WorkbarState::default(),
            native_palette: TuiPalette::Underwater,
        }
    }
    pub fn set_phase(&mut self, phase: ShowcasePhase, elapsed: Duration) {
        self.phase = phase;
        self.phase_started = elapsed;
        self.approval_open = phase == ShowcasePhase::NeedsYou;
        if self.approval_open {
            self.approval = approval_choices();
        }
    }
    pub fn phase_elapsed(&self, elapsed: Duration) -> Duration {
        elapsed.saturating_sub(self.phase_started)
    }

    /// The work view and its keyboard host consume the same phase-aware rows.
    #[must_use]
    pub fn workbar(&self) -> Workbar {
        let mut dock = super::workbar::sample(self.dock.panel);
        let done = self.phase == ShowcasePhase::Done;
        let live = if done {
            WorkbarTone::Success
        } else {
            WorkbarTone::Live
        };
        let mark = if done { "✓" } else { "●" };
        dock.rows = match self.dock.panel {
            WorkbarPanel::Tasks => vec![
                WorkbarRow::new("session:selection", "Preserve session selection")
                    .mark("✓")
                    .tone(WorkbarTone::Success),
                WorkbarRow::new("session:layout", "Refine the narrow layout")
                    .mark(mark)
                    .tone(live),
            ],
            WorkbarPanel::Fleet => vec![
                WorkbarRow::new("worker:build", "Builder").mark(mark).agent(
                    WorkbarAgent::new(
                        "builder",
                        if done { "completed" } else { "running" },
                        "Keep the session list readable in compact windows",
                    )
                    .elapsed_seconds(6),
                ),
                WorkbarRow::new("worker:review", "Reviewer")
                    .mark("✓")
                    .agent(
                        WorkbarAgent::new(
                            "reviewer",
                            "completed",
                            "Check search and keyboard selection",
                        )
                        .elapsed_seconds(3),
                    ),
            ],
            WorkbarPanel::Terminal => vec![WorkbarRow::new(
                "terminal:host",
                "Terminal sessions are supplied by the host",
            )],
            WorkbarPanel::Jobs => vec![
                WorkbarRow::new("shell:layout", "Check the compact session layout")
                    .mark(mark)
                    .tone(live),
            ],
            WorkbarPanel::Files => vec![
                WorkbarRow::new("files:edited", "Edited 2")
                    .mark("▾")
                    .tone(WorkbarTone::Heading)
                    .selectable(false),
                WorkbarRow::new("files:list", "src/components/native_views.rs")
                    .mark("✎")
                    .tone(WorkbarTone::Success)
                    .detail("+18 -8"),
                WorkbarRow::new("files:gallery", "src/gallery/native_views.rs")
                    .mark("✎")
                    .tone(WorkbarTone::Success)
                    .detail("+6 -2"),
            ],
            WorkbarPanel::Notes => vec![
                WorkbarRow::new("notes:1", "Keep search visible at every width")
                    .mark("▪")
                    .tone(WorkbarTone::Live),
                WorkbarRow::new("notes:2", "Preserve selection while filtering")
                    .mark("▪")
                    .tone(WorkbarTone::Live),
            ],
            WorkbarPanel::Context => vec![
                WorkbarRow::new("context:budget", "30.7k of 128k · 24% · compacts at 90%")
                    .mark("◔")
                    .tone(WorkbarTone::Live),
                WorkbarRow::new("context:system", "system + tools · 8.4k")
                    .mark("·")
                    .selectable(false),
                WorkbarRow::new("context:conversation", "conversation + output · 22.3k")
                    .mark("·")
                    .selectable(false),
                WorkbarRow::new("context:compact", "compact now")
                    .mark("▸")
                    .tone(WorkbarTone::Live),
            ],
            WorkbarPanel::Git => vec![
                WorkbarRow::new("git:branch", "session-layout · up to date")
                    .mark("⎇")
                    .tone(WorkbarTone::Live)
                    .detail("codewhale-ratatui"),
                WorkbarRow::new("git:changes", "2 modified files")
                    .mark("±")
                    .tone(WorkbarTone::Live)
                    .detail("/diff"),
            ],
            WorkbarPanel::Cost => dock.rows,
        };
        if self.dock.panel == WorkbarPanel::Tasks {
            dock.goal = Some("Make session navigation feel effortless".into());
            dock.progress = Some(
                if done {
                    "TODO · 2/2 · done"
                } else {
                    "TODO · 1/2 · 1 left"
                }
                .into(),
            );
        }
        dock.tabs = WorkbarPanel::ORDER
            .into_iter()
            .map(|panel| match panel {
                WorkbarPanel::Tasks
                | WorkbarPanel::Fleet
                | WorkbarPanel::Files
                | WorkbarPanel::Notes => WorkbarTab::new(panel).count(2),
                WorkbarPanel::Jobs => WorkbarTab::new(panel).count(1),
                _ => WorkbarTab::new(panel),
            })
            .collect();
        self.dock.apply(dock)
    }
    pub fn active_motion(&self) -> bool {
        self.motion == MotionMode::Full
            && !self.approval_open
            && match self.section {
                ShowcaseSection::Work => matches!(
                    self.phase,
                    ShowcasePhase::Working | ShowcasePhase::Verifying
                ),
                ShowcaseSection::Decisions => self.phase == ShowcasePhase::Verifying,
                ShowcaseSection::Life => self.action_playing,
                _ => false,
            }
    }
    pub fn catalogue(&self) -> (Vec<Entry>, Vec<PickerItem>, PickerMatches) {
        let entries: Vec<_> = super::entries()
            .into_iter()
            .filter(|e| !e.name.starts_with("showcase-"))
            .collect();
        let items: Vec<_> = entries
            .iter()
            .map(|e| PickerItem::new(e.name).detail(format!("{} x {}", e.width, e.height)))
            .collect();
        let matches = PickerMatches::rank(&items, self.query.text(), None);
        (entries, items, matches)
    }
}
fn project_name(value: &str) -> Result<(), std::borrow::Cow<'static, str>> {
    if value.trim().is_empty() {
        Err("Give this example a name.".into())
    } else {
        Ok(())
    }
}

/// Typed inputs for the explicitly labeled action study. These describe
/// authored specimen facts; they are never inferred from a command or log.
pub fn showcase_inputs(action: WhaleState, identity: &str) -> Inputs {
    let (presence, kind) = match action {
        WhaleState::Rest => (Presence::Idle, None),
        WhaleState::Listen => (Presence::Listening, None),
        WhaleState::Think => (Presence::Thinking, None),
        WhaleState::NeedsYou => (Presence::NeedsYou, None),
        WhaleState::Done => (Presence::Done, None),
        WhaleState::Stuck => (Presence::Stuck, None),
        WhaleState::Asleep => (Presence::Offline, None),
        WhaleState::Read => (Presence::Working, Some("reading")),
        WhaleState::Search => (Presence::Working, Some("searching")),
        WhaleState::Write => (Presence::Working, Some("editing")),
        WhaleState::Run => (Presence::Working, Some("executing")),
        WhaleState::Browse => (Presence::Working, Some("browsing")),
        WhaleState::Talk => (Presence::Working, Some("responding")),
        WhaleState::Pod { .. } => (Presence::Working, Some("delegating")),
        WhaleState::Computer => (Presence::Working, Some("computer")),
        WhaleState::Connect => (Presence::Working, Some("network")),
        WhaleState::Busy => (Presence::Working, None),
    };
    Inputs {
        presence,
        activity: Some(Activity {
            kind: kind.map(str::to_owned),
            observed: kind.is_some(),
            parallel: if matches!(action, WhaleState::Pod { .. }) {
                Some(3.0)
            } else {
                None
            },
            active: vec![],
        }),
        context: Context {
            live: true,
            turn_id: Some(identity.to_owned()),
            status: Some(
                if action == WhaleState::Stuck {
                    "failed"
                } else {
                    "completed"
                }
                .into(),
            ),
            now_ms: Some(1_000.0),
            failed_at_ms: Some(0.0),
        },
    }
}
fn approval_choices() -> ApprovalState {
    ApprovalState::new(vec![
        ApprovalChoice::new(ChoiceId(1), "Allow once", ApprovalEffect::Grants).char_key('y'),
        ApprovalChoice::new(ChoiceId(2), "Keep waiting", ApprovalEffect::Refuses).char_key('n'),
    ])
    .reveal(ApprovalKey::char('o'))
}

/// The same pure renderer is used by the live example and frame exports.
/// Native whale geometry is supplied by the host's one Stage; absent geometry
/// uses the kit's still pose. Rendering never reads an Instant or advances it.
pub struct ShowcaseFrame<'a> {
    pub state: &'a ShowcaseState,
    pub elapsed: Duration,
    pub whale: Option<&'a ColoredGrid>,
}
impl<'a> ShowcaseFrame<'a> {
    pub const fn new(state: &'a ShowcaseState, elapsed: Duration) -> Self {
        Self {
            state,
            elapsed,
            whale: None,
        }
    }
    pub const fn whale(mut self, grid: &'a ColoredGrid) -> Self {
        self.whale = Some(grid);
        self
    }
    /// The caller can rasterize its one native Stage at the actual viewport
    /// size. Hidden side panes request no art and therefore no actor clock.
    pub fn whale_size(&self, area: Rect) -> Option<(usize, usize)> {
        if self.state.section != ShowcaseSection::Life {
            return None;
        }
        let art = Self::life_art(area);
        let rows = art.height.saturating_sub(1).min(16);
        (art.width >= 16 && rows >= 8).then_some((usize::from(art.width), usize::from(rows)))
    }
}
impl Paint for ShowcaseFrame<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        // Kit widgets can use ordinary local coordinates even when the host
        // paints into an offset buffer at the last representable coordinate.
        let mut local = Buffer::empty(Rect::new(0, 0, area.width, area.height));
        self.paint_local(local.area, &mut local, theme);
        for y in 0..area.height {
            for x in 0..area.width {
                buf[(area.x + x, area.y + y)] = local[(x, y)].clone();
            }
        }
    }
}

fn band(area: Rect, offset: u16, height: u16) -> Rect {
    let offset = offset.min(area.height);
    Rect::new(
        area.x,
        area.y.saturating_add(offset),
        area.width,
        height.min(area.height.saturating_sub(offset)),
    )
}
fn row(area: Rect, buf: &mut Buffer, line: Line<'_>) {
    let area = area.intersection(buf.area);
    if !area.is_empty() {
        buf.set_line(area.x, area.y, &line, area.width);
    }
}
fn caption(area: Rect, buf: &mut Buffer, theme: &Theme, value: &str, role: Role) {
    row(
        area,
        buf,
        Line::styled(
            text::truncate_words(
                &text::display_safe(value),
                usize::from(area.width),
                theme.ascii(),
            )
            .into_owned(),
            theme.fg(role),
        ),
    );
}
fn title(area: Rect, buf: &mut Buffer, theme: &Theme, value: &str) {
    row(
        area,
        buf,
        Line::styled(
            text::truncate_words(
                &text::display_safe(value),
                usize::from(area.width),
                theme.ascii(),
            )
            .into_owned(),
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        ),
    );
}
fn mode_word(mode: MotionMode) -> &'static str {
    match mode {
        MotionMode::Full => "Full",
        MotionMode::Reduced => "Reduced",
        MotionMode::Still => "Still",
    }
}

impl ShowcaseFrame<'_> {
    fn native_composer(&self, theme: &Theme) -> NativeComposer<'_> {
        let mut composer = NativeComposer::new(self.state.draft.text())
            .density(NativeComposerDensity::Compact)
            .cursor(self.state.draft.cursor())
            .placeholder("Write a task or use /.")
            .focused(self.state.editing && !self.state.dock.focused)
            .can_submit(!self.state.draft.text().is_empty());
        if matches!(
            self.state.phase,
            ShowcasePhase::Working | ShowcasePhase::Verifying
        ) {
            composer = composer.submit_hint(if theme.ascii() {
                "Enter send after this turn"
            } else {
                "↵ send after this turn"
            });
        }
        composer
    }

    /// Native slot geometry shared with the terminal cursor owner.
    #[must_use]
    pub fn work_areas(&self, area: Rect, theme: &Theme) -> crate::ShellAreas {
        let dock = self.state.workbar();
        let dock_rows = if self.state.readouts.on {
            dock.height(area.width, theme)
        } else {
            0
        };
        let pending_rows = u16::from(!self.state.queued.is_empty());
        let workflow_rows = u16::from(self.state.details.on);
        let composer_budget = area
            .height
            .saturating_sub(3 + 2 + dock_rows + pending_rows + workflow_rows)
            .max(1)
            .min(area.height);
        let composer_height = self
            .native_composer(theme)
            .desired_height(area.width, composer_budget);
        TerminalShell::new(composer_height)
            .pending_rows(pending_rows)
            .workflow_rows(workflow_rows)
            .workbar_rows(dock_rows)
            .areas(area)
    }

    #[must_use]
    pub fn composer_cursor(&self, area: Rect, theme: &Theme) -> Option<ratatui::layout::Position> {
        self.native_composer(theme)
            .cursor_position(self.work_areas(area, theme).composer)
    }

    fn paint_local(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let theme = self.state.theme_for(theme);
        buf.set_style(
            area,
            theme.bg(Role::Background).patch(theme.fg(Role::Foreground)),
        );
        match self.state.section {
            ShowcaseSection::Work => self.work(area, buf, &theme),
            ShowcaseSection::Decisions => self.decisions(area, buf, &theme),
            ShowcaseSection::Controls => self.controls(area, buf, &theme),
            ShowcaseSection::Color => self.colors(area, buf, &theme),
            ShowcaseSection::Life => self.life(area, buf, &theme),
            ShowcaseSection::Components => self.components(area, buf, &theme),
        }
        let phase = if self.state.section == ShowcaseSection::Life {
            match WhaleState::ALL[self.state.action.min(16)].state() {
                State::Working => OceanPhase::Working,
                State::NeedsYou => OceanPhase::Waiting,
                State::Failed => OceanPhase::Failed,
                State::Done => OceanPhase::Done,
                _ => OceanPhase::Idle,
            }
        } else if self.state.approval_open {
            OceanPhase::Approval
        } else {
            match self.state.phase {
                ShowcasePhase::Working => OceanPhase::Working,
                ShowcasePhase::NeedsYou => OceanPhase::Waiting,
                ShowcasePhase::Verifying => OceanPhase::Verifying,
                ShowcasePhase::Done => OceanPhase::Done,
            }
        };
        if self.state.palette == WaterPalette::Ocean {
            let column = OceanColumn::new(
                self.elapsed,
                if self.state.active_motion() {
                    self.state.motion
                } else {
                    MotionMode::Still
                },
            )
            .phase(phase)
            .context_percent(if self.state.section == ShowcaseSection::Work {
                24
            } else {
                0
            })
            .completion_elapsed(self.state.phase_elapsed(self.elapsed))
            .viewport(area);
            column.apply(area, buf, &theme);
            if self.state.section == ShowcaseSection::Work {
                let regions = self.work_areas(area, &theme);
                for (region, ground) in [
                    (regions.composer, TuiGround::Composer),
                    (regions.posture, TuiGround::Footer),
                    (regions.metrics, TuiGround::Header),
                ] {
                    if let Some(ground) = theme.tui_ground(ground).bg {
                        column.apply_matching(region, buf, &theme, ground);
                    }
                }
            }
        } else {
            Ombre::new(self.state.palette)
                .direction(self.state.direction)
                .apply(area, buf, &theme);
        }
        if self.state.section == ShowcaseSection::Life {
            self.paint_whale(
                Self::life_art(area),
                buf,
                &theme,
                WhaleState::ALL[self.state.action.min(16)],
            );
        }
    }

    fn marker(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let elapsed = self.state.phase_elapsed(self.elapsed);
        match self.state.phase {
            ShowcasePhase::Working => {
                Spinner::new("Checking the narrow layout", elapsed, self.state.motion)
                    .paint(area, buf, theme)
            }
            ShowcasePhase::Verifying => {
                VerificationSpinner::new("Checking the changes", elapsed, self.state.motion)
                    .paint(area, buf, theme)
            }
            ShowcasePhase::NeedsYou => StatusMark::new(State::NeedsYou)
                .word("Needs you")
                .paint(area, buf, theme),
            ShowcasePhase::Done => Receipt::new(State::Done, "Done")
                .value(ReceiptValue::Duration(Some(Duration::from_secs(6))))
                .paint(area, buf, theme),
        }
    }
    fn paint_whale(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: WhaleState) {
        let whale = Whale::new(state).words(state.words());
        if let Some(grid) = self.whale {
            grid.paint_with_contrast(&whale, area, buf, theme, 3.0);
        } else {
            whale.paint(area, buf, theme);
        }
    }
    fn split(&self, area: Rect, side_width: u16) -> (Rect, Option<Rect>) {
        if area.width >= 96 {
            (
                Rect::new(
                    area.x,
                    area.y,
                    area.width.saturating_sub(side_width + 3),
                    area.height,
                ),
                Some(Rect::new(
                    area.right().saturating_sub(side_width),
                    area.y,
                    side_width,
                    area.height,
                )),
            )
        } else {
            (area, None)
        }
    }
    fn composer(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.native_composer(theme).paint(area, buf, theme);
    }

    fn work(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let dock = self.state.workbar();
        let regions = self.work_areas(area, theme);
        TerminalShell::new(0).paint(area, buf, theme);
        let user = Message::native("Make the session picker readable in narrow windows.")
            .role(Role::Accent)
            .marker(crate::glyphs::USER);
        let mut next = 1;
        let height = user.height(regions.conversation.width, theme);
        user.paint(band(regions.conversation, next, height), buf, theme);
        next += height + 1;
        let reply = match self.state.phase {
            ShowcasePhase::Working => {
                "The list now keeps its search field, numbering and current-session marker.\nLong titles wrap cleanly, and the selection stays visible as the window narrows.\n\nI'm checking the compact layout before wrapping up."
            }
            ShowcasePhase::NeedsYou if self.state.approval_open => {
                "Review the command below before continuing."
            }
            ShowcasePhase::NeedsYou => "The turn is paused. Your draft is still here.",
            ShowcasePhase::Verifying => {
                "The list, search field and current-session marker keep their place.\nI'm checking the compact layout and keyboard navigation."
            }
            ShowcasePhase::Done => {
                "The session picker is ready for review.\nSearch, numbering and selection remain visible in narrow windows."
            }
        };
        let reply = Message::native(reply);
        let height = reply.height(regions.conversation.width, theme);
        reply.paint(band(regions.conversation, next, height), buf, theme);
        next += height + 1;
        for followup in &self.state.followups {
            let message = Message::native(followup).marker(crate::glyphs::USER);
            let height = message.height(regions.conversation.width, theme);
            message.paint(band(regions.conversation, next, height), buf, theme);
            next = next.saturating_add(height).saturating_add(1);
        }
        if next < regions.conversation.height {
            self.marker(band(regions.conversation, next, 1), buf, theme);
        }
        if theme.native_palette() == Some(TuiPalette::Underwater) {
            Habitat::new(
                self.elapsed,
                if self.state.active_motion() {
                    self.state.motion
                } else {
                    MotionMode::Still
                },
            )
            .density(HabitatDensity::Sparse)
            .paint(regions.conversation, buf, theme);
        }
        if let Some(queued) = self.state.queued.last() {
            caption(
                regions.pending,
                buf,
                theme,
                &format!(
                    "{} queued {} {}",
                    self.state.queued.len(),
                    if theme.ascii() { "." } else { "·" },
                    queued
                ),
                Role::Accent,
            );
        }
        self.composer(regions.composer, buf, theme);
        buf.set_style(regions.posture, theme.tui_ground(TuiGround::Footer));
        let mut posture = PostureBar::new(self.state.permission.word())
            .permission_ink(self.state.permission.ink())
            .permission_key("Shift+Tab")
            .mode_ink("work", TuiInk::ModeWork)
            .context_percent(24);
        if self.state.phase == ShowcasePhase::Done {
            posture = posture.session_clock("worked 6 s", Role::Muted);
        } else {
            posture = posture.turn_clock(
                format!(
                    "{} {}",
                    self.state.phase.word().to_lowercase(),
                    crate::duration(self.state.phase_elapsed(self.elapsed))
                ),
                self.state.phase.state().role(),
            );
        }
        if self.state.editing
            && matches!(
                self.state.phase,
                ShowcasePhase::Working | ShowcasePhase::Verifying
            )
        {
            posture = posture.hint("Esc to interrupt", Role::Hint);
        }
        posture.paint(regions.posture, buf, theme);
        WorkflowProgress::new(vec![
            WorkflowRun::new(
                "Update the component library",
                if self.state.phase == ShowcasePhase::Done {
                    WorkflowRunState::Succeeded
                } else {
                    WorkflowRunState::Running
                },
            )
            .outcomes(
                if self.state.phase == ShowcasePhase::Done {
                    3
                } else {
                    1
                },
                0,
                0,
                3,
            )
            .elapsed(if self.state.phase == ShowcasePhase::Done {
                self.state.phase_started
            } else {
                self.elapsed
            }),
        ])
        .paint(regions.workflows, buf, theme);
        buf.set_style(regions.metrics, theme.tui_ground(TuiGround::Header));
        MetricsLine::new(vec![
            MetricSegment::new(MetricKind::Model, "", "deepseek-v4"),
            MetricSegment::new(MetricKind::Context, "ctx", "24%"),
            MetricSegment::new(MetricKind::Workspace, "", "codewhale-ratatui"),
            MetricSegment::new(MetricKind::GitBranch, "", "session-layout"),
        ])
        .help_hint("F6 components")
        .paint(
            regions
                .metrics
                .inner(Margin::new(u16::from(regions.metrics.width >= 8), 0)),
            buf,
            theme,
        );
        dock.paint(regions.workbar, buf, theme);
    }

    fn decisions(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if self.state.approval_open {
            self.approval(area, buf, theme);
            return;
        }
        title(band(area, 1, 1), buf, theme, "Review changes");
        const PATCH: &str = "--- a/workspace.rs\n+++ b/workspace.rs\n@@ -1,3 +1,4 @@\n fn draw() {\n-    draw_all();\n+    composer.paint(area, buffer, theme);\n+    workbar.paint(dock, buffer, theme);\n }\n";
        Diff::new(parse_unified(PATCH))
            .gutter(DiffGutter::Both)
            .wrap(DiffWrap::Wrap)
            .paint(band(area, 4, area.height.saturating_sub(7)), buf, theme);
        self.marker(band(area, area.height.saturating_sub(2), 1), buf, theme);
        caption(
            band(area, area.height.saturating_sub(1), 1),
            buf,
            theme,
            "F1 conversation / F10 continue",
            Role::Hint,
        );
    }

    fn approval(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        // A full reading surface at narrow widths; no creature or decoration
        // can enter even the blank space of this decision.
        let width = area.width.min(72);
        let modal = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y,
            width,
            area.height,
        );
        buf.set_style(
            modal,
            theme.bg(Role::Surface).patch(theme.fg(Role::Foreground)),
        );
        for y in modal.top()..modal.bottom() {
            for x in modal.left()..modal.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        title(band(modal, 0, 1), buf, theme, "Run a command");
        let subject = ApprovalSubject::new(ApprovalKind::Command, "git diff --check")
            .cwd("codewhale-ratatui")
            .scope(ApprovalScope::Inside)
            .agent("Verifier", false)
            .risk_note("Checks whitespace in the current changes.");
        ApprovalCard::new(&subject, &self.state.approval).paint(
            band(modal, 2, modal.height.saturating_sub(3)),
            buf,
            theme,
        );
        caption(
            band(modal, modal.height.saturating_sub(1), 1),
            buf,
            theme,
            "y advance / n wait / Esc preserve draft",
            Role::Attention,
        );
    }
    fn controls(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let (main, side) = self.split(area, 34);
        title(band(main, 0, 1), buf, theme, "Settings");
        caption(
            band(main, 2, 2),
            buf,
            theme,
            if self.state.editing {
                "Editing / Tab moves between fields; Enter validates"
            } else {
                "Enter edits the form. F7-F9 change appearance."
            },
            Role::Muted,
        );
        Form::new(&self.state.form)
            .focused(self.state.editing)
            .paint(band(main, 5, 11), buf, theme);
        Toggle::new("Workflow progress", self.state.details)
            .focused(!self.state.editing && self.state.focus == 0)
            .paint(band(main, 17, 1), buf, theme);
        Toggle::new("Workbar", self.state.readouts)
            .focused(!self.state.editing && self.state.focus == 1)
            .paint(band(main, 19, 1), buf, theme);
        caption(band(main, 21, 2), buf, theme, &self.state.note, Role::Live);
        if let Some(side) = side {
            self.settings(side, buf, theme);
        }
    }
    fn settings(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        PaneHeader::new("Appearance").paint(band(area, 0, 2), buf, theme);
        let rows = [
            SettingRow::new("Palette", self.state.palette.name()).source("your choice"),
            SettingRow::new("Profile", self.state.profile.name()).source("terminal"),
            SettingRow::new("Motion", mode_word(self.state.motion)).source("your choice"),
        ];
        let mut top = 3;
        for setting in rows {
            let h = setting.height(area.width, theme);
            setting.paint(band(area, top, h), buf, theme);
            top += h;
        }
        SettingDetail::new("Appearance", "Palette changes the atmosphere. State marks, action ink and decision scope keep their meaning.").default_value("Ocean / Full").source("workspace").paint(band(area, top + 2, area.height.saturating_sub(top + 2)), buf, theme);
    }
    fn colors(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        title(band(area, 0, 1), buf, theme, "Appearance");
        let palette = WaterPalette::ALL
            .iter()
            .position(|p| *p == self.state.palette)
            .unwrap_or(0);
        Segmented::new(
            WaterPalette::ALL.map(WaterPalette::name),
            SegmentedState::new(palette),
        )
        .focused(self.state.focus == 0)
        .paint(band(area, 2, 1), buf, theme);
        Segmented::new(
            ["Full", "Reduced", "Still"],
            SegmentedState::new(match self.state.motion {
                MotionMode::Full => 0,
                MotionMode::Reduced => 1,
                MotionMode::Still => 2,
            }),
        )
        .focused(self.state.focus == 1)
        .paint(band(area, 4, 1), buf, theme);
        caption(
            band(area, 6, 1),
            buf,
            theme,
            &format!("Profile {} / Left, Right", self.state.profile.name()),
            if self.state.focus == 2 {
                Role::Accent
            } else {
                Role::Muted
            },
        );
        Segmented::new(
            ["Vertical", "Diagonal"],
            SegmentedState::new(usize::from(
                self.state.direction == OmbreDirection::Diagonal,
            )),
        )
        .focused(self.state.focus == 3)
        .paint(band(area, 8, 1), buf, theme);
        let columns = if area.width >= 72 { 3 } else { 2 };
        let cell_width = area.width / columns;
        for (index, role) in Role::ALL.into_iter().enumerate() {
            let x = area.x + (index as u16 % columns) * cell_width;
            let y = area.y + 11 + index as u16 / columns;
            let cell = Rect::new(x, y, cell_width.saturating_sub(1), 1).intersection(area);
            if cell.is_empty() {
                continue;
            }
            let label = format!("{:?}", role);
            let sample = if role.is_ground() {
                theme.bg(role).patch(theme.fg(Role::Foreground))
            } else if role == Role::PrimaryForeground {
                theme.bg(Role::Primary).patch(theme.fg(role))
            } else {
                theme.fg(role)
            };
            let label_width = cell.width.saturating_sub(3);
            row(
                cell,
                buf,
                Line::from(vec![
                    Span::styled(
                        text::pad(&label, usize::from(label_width), theme.ascii()),
                        theme.fg(Role::Foreground),
                    ),
                    Span::styled(" Aa", sample),
                ]),
            );
        }
        let start = if columns == 3 { 19 } else { 22 };
        if area.height > start + 2 {
            let width = area.width / 4;
            for (index, depth) in [Depth::Deep, Depth::Stage, Depth::Raised, Depth::Overlay]
                .into_iter()
                .enumerate()
            {
                let cell = Rect::new(
                    area.x + index as u16 * width,
                    area.y + start,
                    width.saturating_sub(1),
                    2,
                )
                .intersection(area);
                buf.set_style(cell, theme.bg(depth.ground()));
                caption(cell, buf, theme, &format!("{depth:?}"), Role::Foreground);
            }
            HorizonRule::new().label("One spatial wash").paint(
                band(area, start + 3, 1),
                buf,
                theme,
            );
        }
    }
    fn life_art(area: Rect) -> Rect {
        let water = band(area, 4, area.height.saturating_sub(4));
        let art_width = water.width.min(40);
        Rect::new(
            water.x + water.width.saturating_sub(art_width) / 2,
            water.y + u16::from(water.height >= 19) * 2,
            art_width,
            water.height.min(17),
        )
    }
    fn life(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let action = WhaleState::ALL[self.state.action.min(WhaleState::ALL.len() - 1)];
        title(
            band(area, 0, 1),
            buf,
            theme,
            "One whale. Seventeen native actions.",
        );
        caption(
            band(area, 2, 1),
            buf,
            theme,
            &format!(
                "Action study / {} / {} of 17 / Left, Right / Space play",
                action.words(),
                self.state.action.min(16) + 1
            ),
            Role::Accent,
        );
        let water = band(area, 4, area.height.saturating_sub(4));
        let art = Self::life_art(area);
        if !self.state.approval_open {
            Habitat::new(
                self.elapsed,
                if self.state.action_playing {
                    self.state.motion
                } else {
                    MotionMode::Still
                },
            )
            .density(HabitatDensity::Rich)
            .hold_jellyfish_visit(true)
            .protected(vec![art])
            .paint(water, buf, theme);
        }
        if water.height >= 20 {
            caption(
                band(water, water.height - 2, 1),
                buf,
                theme,
                "Native dots, colored ink, fish, one jellyfish and bubbles",
                Role::Muted,
            );
            caption(
                band(water, water.height - 1, 1),
                buf,
                theme,
                if self.state.active_motion() {
                    "Space pause / Left, Right action / F8 motion"
                } else {
                    "Space play / Left, Right action / F8 motion"
                },
                Role::Muted,
            );
        }
    }
    fn components(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let (entries, items, matches) = self.state.catalogue();
        let narrow = area.width < 76;
        let list_width = if narrow { area.width } else { 28 };
        let list_height = if narrow {
            area.height.min(8)
        } else {
            area.height
        };
        let list = Rect::new(area.x, area.y, list_width, list_height);
        let picker = Picker::new(&items, self.state.picker)
            .query(
                self.state.query.text(),
                text::width(self.state.query.text()),
            )
            .matches(&matches);
        picker.paint(list, buf, theme);
        let Some(selected) = matches
            .get(self.state.picker.selected)
            .and_then(|hit| entries.get(hit.index))
        else {
            return;
        };
        let preview = if narrow {
            band(
                area,
                list_height + 1,
                area.height.saturating_sub(list_height + 1),
            )
        } else {
            Rect::new(
                area.x + 31,
                area.y,
                area.width.saturating_sub(31),
                area.height,
            )
        };
        PaneHeader::new(selected.name)
            .meta(format!(
                "{} x {} / PgUp, PgDn",
                selected.width, selected.height
            ))
            .paint(band(preview, 0, 2), buf, theme);
        let body = band(preview, 3, preview.height.saturating_sub(3));
        if body.is_empty() {
            return;
        }
        // Tall components scroll, rather than being silently clipped to the
        // viewport. Width belongs to the current terminal specimen.
        let source_height = selected.height.max(body.height);
        let mut source = Buffer::empty(Rect::new(0, 0, body.width, source_height));
        source.set_style(source.area, theme.bg(Role::Background));
        (selected.draw)(source.area, &mut source, theme);
        let top = self
            .state
            .preview_scroll
            .min(source_height.saturating_sub(body.height));
        for y in 0..body.height {
            for x in 0..body.width {
                buf[(body.x + x, body.y + y)] = source[(x, y + top)].clone();
            }
        }
    }
}

fn fixture(
    section: ShowcaseSection,
    phase: ShowcasePhase,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    let mut state = ShowcaseState::new();
    state.section = section;
    state.phase = phase;
    state.approval_open = section == ShowcaseSection::Decisions;
    state.action = 8;
    state.action_playing = true;
    let frame = ShowcaseFrame::new(&state, Duration::from_millis(6_800));
    if let Some((cols, rows)) = frame.whale_size(area) {
        let action = if section == ShowcaseSection::Life {
            WhaleState::ALL[state.action]
        } else {
            phase.whale()
        };
        let mut stage = Stage::new();
        stage.observe(
            Some("gallery-action-study"),
            showcase_inputs(action, "gallery-action-study"),
            theme.ascii(),
        );
        let base = std::time::Instant::now();
        // Fixed caller-supplied steps sample a living native pose. The pure
        // renderer merely consumes this one fixture-owned Stage's output.
        for index in 0..=8 {
            stage.advance(base + Duration::from_millis(index * 200));
        }
        let native = colored_braille(
            stage.director(),
            cols,
            rows,
            theme.caps().appearance != crate::detect::Appearance::Light,
        );
        frame.whale(&native).paint(area, buf, theme);
    } else {
        frame.paint(area, buf, theme);
    }
}
pub(crate) fn entries() -> Vec<Entry> {
    vec![
        Entry {
            name: "showcase-work",
            width: 104,
            height: 30,
            draw: |a, b, t| fixture(ShowcaseSection::Work, ShowcasePhase::Working, a, b, t),
        },
        Entry {
            name: "showcase-decision",
            width: 104,
            height: 30,
            draw: |a, b, t| fixture(ShowcaseSection::Decisions, ShowcasePhase::NeedsYou, a, b, t),
        },
        Entry {
            name: "showcase-color",
            width: 112,
            height: 38,
            draw: |a, b, t| fixture(ShowcaseSection::Color, ShowcasePhase::Done, a, b, t),
        },
        Entry {
            name: "showcase-life",
            width: 112,
            height: 38,
            draw: |a, b, t| fixture(ShowcaseSection::Life, ShowcasePhase::Working, a, b, t),
        },
        Entry {
            name: "showcase-narrow",
            width: 40,
            height: 28,
            draw: |a, b, t| fixture(ShowcaseSection::Work, ShowcasePhase::Done, a, b, t),
        },
    ]
}
