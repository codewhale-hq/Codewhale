//! Views of one durable local pet. Engine events are projected here once;
//! the companion owns simulation, persistence and the sole audio output.
use crate::core::events::{Event, TurnOutcomeStatus};
use crate::tui::{
    app::{App, StatusToastLevel},
    underwater::ShellPhase,
    views::ModalKind,
};
use codewhale_localization::{MessageId, tr};
use codewhale_palette::{ChromeInk, chrome_style};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Paragraph},
};
use serde_json::json;
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
mod appearance;
pub use appearance::{BodyStyle, TextLanguage};
mod audio;
mod audio_cursor;
mod avatars;
mod full;
mod graphics;
mod habitat;
pub use full::{render_full, render_main};
mod live;
pub(crate) mod owner;
mod persistence;
#[cfg(test)]
mod worker;
use live::{Command, Notice, Presentation, Worker};
#[derive(Clone, Copy)]
pub enum Control {
    Appearance {
        body: Option<BodyStyle>,
        language: Option<TextLanguage>,
    },
    Sound,
    Browser,
    Window,
    Select,
    Scroll(i16),
    FocusAgents,
    ScrollEnd(bool),
    Mouse(crossterm::event::MouseEvent),
}
#[derive(Default)]
pub struct PetWatch {
    avatars: avatars::Avatars,
    worker: Option<Worker>,
    session: Option<String>,
    active_turn_id: Option<String>,
    last_tick: Option<Instant>,
    failed: bool,
    /// The companion said it cannot be reached. Cleared by the next frame.
    unavailable: bool,
    exporting: bool,
    sound_requested: bool,
    pub(crate) area: Option<Rect>,
    pub(crate) inspect_area: Option<Rect>,
    copy_area: Arc<Mutex<Option<Rect>>>,
    raster: Option<Presentation>,
    controls: Arc<Mutex<Vec<Control>>>,
    desired: Option<Rect>,
    painted: Option<Rect>,
    sent: Option<Instant>,
    started: Option<Instant>,
    frames: u64,
    bytes: u64,
    render_ms: f64,
    output_ms: f64,
    /// `/pet on`: the native pet replaces the ordinary shell backdrop.
    pub(crate) enabled: bool,
    work_history_start: usize,
    full: codewhale_ratatui::PetModeState,
    full_next: Option<Instant>,
    full_response: Option<crate::tui::transcript::TranscriptViewCache>,
    full_response_key: Option<(u64, usize, bool)>,
    full_inputs: Option<codewhale_ratatui::whale_motion::Inputs>,
    selection: Arc<Mutex<Option<String>>>,
}
impl PetWatch {
    pub fn avatar_choices(&self) -> String {
        self.avatars.choices()
    }
    pub fn select_avatar(&mut self, key: &str) -> bool {
        self.avatars.select(key)
    }
    pub fn preview_avatar(&mut self, name: &str, view: bool) -> bool {
        self.avatars.preview(name, view)
    }
    pub fn set_sound(&mut self, enabled: bool) {
        self.sound_requested = enabled;
    }
    pub fn sound_label(&self) -> MessageId {
        if !self.sound_requested {
            MessageId::PetWatchSoundOff
        } else if self.raster.as_ref().is_some_and(|r| {
            r.scene.audio_owner.as_deref() == Some(r.client.as_str()) && !r.scene.audio_unavailable
        }) {
            MessageId::PetWatchSoundOn
        } else {
            MessageId::PetWatchSoundPaused
        }
    }
    fn reset(&mut self, session: Option<String>) {
        self.worker = None;
        self.session = session;
        self.active_turn_id = None;
        self.raster = None;
        self.failed = false;
        self.unavailable = false;
        self.last_tick = None;
        self.work_history_start = 0;
        self.full = codewhale_ratatui::PetModeState::default();
        self.full_next = None;
        self.full_response = None;
        self.full_response_key = None;
        self.full_inputs = None;
        if let Ok(mut selection) = self.selection.lock() {
            *selection = None;
        }
        self.avatars.view = None;
        self.avatars.action = None;
    }
    fn ensure(&mut self, session: Option<String>) {
        if self.session != session {
            self.reset(session)
        }
        if self.worker.is_none() && !self.failed {
            match Worker::start(self.session.clone()) {
                Ok(worker) => self.worker = Some(worker),
                Err(_) => self.failed = true,
            }
        }
    }
    pub fn export(&mut self) -> bool {
        if self.worker.is_none() || self.exporting {
            return false;
        }
        self.send(Command::Export);
        self.exporting = !self.failed;
        self.exporting
    }
    /// Tests drive the shell without a companion: never start a view worker.
    #[cfg(test)]
    pub(crate) fn detach_for_test(&mut self) {
        self.worker = None;
        self.failed = true;
    }
    pub fn observe(&mut self, event: &Event, session: Option<&str>, _now: Instant) {
        if self.session.as_deref() != session {
            // A live view follows the session switch: restart it for the new
            // session and forward the event that switched, so the reducer
            // sees the turn that opened it. Without a live view, the next
            // render starts one; turn tracking below still records the turn.
            let had_worker = self.worker.is_some();
            self.reset(session.map(str::to_owned));
            if had_worker {
                self.ensure(self.session.clone());
            }
        }
        if let Event::TurnStarted { turn_id, .. } = event {
            self.active_turn_id = Some(turn_id.clone());
        }
        if let Some(text) = metadata(event, self.active_turn_id.as_deref()) {
            self.send(Command::Observe(text));
        }
        if matches!(event, Event::TurnComplete { .. }) {
            self.active_turn_id = None;
        }
    }
    fn send(&mut self, command: Command) {
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.tx.try_send(command).is_err())
        {
            // A gap drops this producer lease. Restart from a fresh unobserved
            // handshake; never infer continuity from events we could not queue.
            self.worker = None;
            self.raster = None;
            self.failed = true;
        }
    }
    /// Wake the existing terminal loop for the native stage, even at idle.
    /// Hidden and reduced/still stages clear this deadline in `full::tick`.
    pub fn next_frame_in(&self, now: Instant) -> Option<Duration> {
        self.full_next
            .map(|deadline| deadline.saturating_duration_since(now))
    }
    pub fn prepare_frame(&mut self) {
        self.inspect_area = None;
        if let Ok(mut area) = self.copy_area.lock() {
            *area = None;
        }
        self.desired = None;
    }
    pub fn present(&mut self, output: &mut impl Write) -> io::Result<()> {
        let raster = self
            .raster
            .as_ref()
            .filter(|r| r.frame_changed.elapsed() < Duration::from_millis(800));
        let desired = self.desired.filter(|a| {
            raster.is_some_and(|r| r.image.is_some() && r.width == a.width && r.height == a.height)
        });
        if desired.is_none() {
            if self.painted.take().is_some() {
                graphics::clear(output)?;
            }
            self.sent = None;
            return Ok(());
        }
        let area = desired.unwrap();
        let raster = raster.unwrap();
        if self.painted == Some(area) && self.sent == Some(raster.created) {
            return Ok(());
        }
        let began = Instant::now();
        // Within the existing synchronized frame: delete this process's one
        // image, replace it, restore the cursor. No terminal-side frame queue.
        graphics::clear(output)?;
        write!(output, "\x1b7\x1b[{};{}H", area.y + 1, area.x + 1)?;
        output.write_all(raster.image.as_ref().unwrap())?;
        output.write_all(b"\x1b8")?;
        self.painted = Some(area);
        self.sent = Some(raster.created);
        self.started.get_or_insert(began);
        self.frames += 1;
        self.bytes += raster.bytes as u64;
        self.render_ms += raster.render_ms;
        self.output_ms += began.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }
    pub fn status(&self) -> String {
        let seconds = self.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let identity = self
            .raster
            .as_ref()
            .map(|r| {
                format!(
                    "{} · tick {} · {} · {}",
                    r.scene.identity, r.scene.tick, r.scene.digest, r.scene.source
                )
            })
            .unwrap_or_default();
        format!(
            "{identity} · {} pixel frames / {:.1}s · {:.1} fps · {:.2} MiB/s · render {:.2}ms · write {:.2}ms",
            self.frames,
            seconds,
            self.frames as f64 / seconds.max(0.001),
            self.bytes as f64 / 1048576.0 / seconds.max(0.001),
            self.render_ms / self.frames.max(1) as f64,
            self.output_ms / self.frames.max(1) as f64
        )
    }
}
/// Existing protocol projection defines the variant names. This allowlist
/// removes payloads before the bounded worker queue; no model text escapes.
fn metadata(event: &Event, turn_id: Option<&str>) -> Option<String> {
    let mut value = match event {
        Event::TurnStarted { turn_id, .. } => json!({"event":"turn_started","turn_id":turn_id}),
        Event::TurnComplete { status, .. } => {
            let mut value = json!({
                "event":"turn_complete",
                "turn_outcome":match status {
                    TurnOutcomeStatus::Completed => "completed",
                    TurnOutcomeStatus::Interrupted => "interrupted",
                    TurnOutcomeStatus::Failed => "failed",
                }
            });
            // An untracked turn omits the id; the reducer rejects `null`, and
            // without an id a completed turn cannot claim Done.
            if let Some(turn_id) = turn_id {
                value["turn_id"] = json!(turn_id);
            }
            value
        }
        Event::MessageStarted { index } => json!({"event":"message_started","index":index}),
        Event::MessageDelta { index, .. } => {
            json!({"event":"response_delta","index":index,"channel":"text"})
        }
        Event::MessageComplete { index } => json!({"event":"message_complete","index":index}),
        Event::ThinkingStarted { index } => json!({"event":"thinking_started","index":index}),
        Event::ThinkingDelta { index, .. } => {
            json!({"event":"response_delta","index":index,"channel":"reasoning"})
        }
        Event::ThinkingComplete { index } => json!({"event":"thinking_complete","index":index}),
        Event::OperationActivityStarted {
            span_id,
            activity_kind,
            action_id,
        } => json!({
            "event":"operation_activity_started",
            "span_id":span_id,
            "action_id":action_id,
            "activity_kind":activity_kind
        }),
        Event::OperationActivityCompleted {
            span_id,
            activity_kind,
            action_id,
            outcome,
        } => json!({
            "event":"operation_activity_completed",
            "span_id":span_id,
            "action_id":action_id,
            "activity_kind":activity_kind,
            "outcome":outcome
        }),
        Event::ToolCallHeartbeat => json!({"event":"tool_call_heartbeat"}),
        // A denied (by a human or by policy) or cancelled call ends any wait
        // on it. Only the Engine call id and typed outcome cross; the tool
        // name and error text stay in the transcript. Approvals that run are
        // cleared by their operation start and by the shell's typed
        // `waiting` flag on every tick.
        Event::ToolCallComplete {
            id,
            result: Err(crate::tools::spec::ToolError::PermissionDenied { .. }),
            ..
        } => json!({
            "event":"approval_resolved","id":id,"outcome":"denied"
        }),
        Event::ToolCallComplete {
            id,
            result: Err(crate::tools::spec::ToolError::Cancelled { .. }),
            ..
        } => json!({
            "event":"approval_resolved","id":id,"outcome":"cancelled"
        }),
        Event::AgentSpawned { id, .. } => json!({"event":"agent_spawned","id":id}),
        Event::AgentProgress { id, activity, .. } => json!({
            "event":"agent_progress",
            "id":id,
            "worker_status":crate::core::protocol_parity::worker_status_str(activity.worker_status)
        }),
        Event::AgentComplete { id, .. } => json!({"event":"agent_complete","id":id}),
        Event::ApprovalRequired { id, .. } | Event::UserInputRequired { id, .. } => json!({
            "event":if matches!(event, Event::ApprovalRequired { .. }) {"approval_required"} else {"user_input_required"},
            "id":id
        }),
        _ => return None,
    };
    if value
        .get("action_id")
        .is_some_and(serde_json::Value::is_null)
    {
        value.as_object_mut()?.remove("action_id");
    }
    let json = serde_json::to_string(&value).ok()?;
    (json.len() <= 16_384).then_some(json)
}

pub fn command(app: &mut App, control: Control) {
    app.pet_watch.ensure(app.current_session_id.clone());
    let agents = full::agents(app);
    apply(&mut app.pet_watch, control, &agents);
    app.needs_redraw = true;
}
fn apply(state: &mut PetWatch, control: Control, agents: &[codewhale_ratatui::Subagent<'_>]) {
    match control {
        Control::Appearance { body, language } => {
            state.send(Command::Appearance { body, language });
        }
        Control::Browser => state.send(Command::Browser),
        Control::Window => state.send(Command::Window),
        Control::Select => state.send(Command::Select),
        Control::Sound => state.sound_requested = !state.sound_requested,
        Control::Scroll(delta) => state.full.scroll(agents, delta),
        Control::FocusAgents => state.full.toggle_agents(agents),
        Control::ScrollEnd(end) => state.full.scroll_end(agents, end),
        Control::Mouse(mouse) => {
            state.full.handle_mouse(agents, mouse);
        }
    }
}
pub fn open_habitat(app: &mut App) {
    app.pet_watch.ensure(app.current_session_id.clone());
    if app.view_stack.top_kind() != Some(ModalKind::PetHabitat) {
        app.view_stack.push(habitat::Habitat::new(
            app.pet_watch.controls.clone(),
            app.pet_watch.selection.clone(),
            app.pet_watch.copy_area.clone(),
        ));
    }
    app.needs_redraw = true;
}
/// Explicit tools and focused workers remain available on demand.
pub fn main_view(app: &App) -> bool {
    app.pet_watch.enabled
        && app.agent_focus.is_none()
        && !app.work_surface.focused
        && app.file_tree.is_none()
}
pub fn is_open(app: &App) -> bool {
    app.view_stack.top_kind() == Some(ModalKind::PetHabitat)
        || (main_view(app) && app.view_stack.is_empty())
}
/// `/pet on|off` selects the shell backdrop without taking composer focus.
/// Drafts, history, selection and the active Engine turn retain their owners.
pub fn set_enabled(app: &mut App, enabled: bool) {
    app.pet_watch.enabled = enabled;
    if app.view_stack.top_kind() == Some(ModalKind::PetHabitat) {
        app.view_stack.pop();
    }
    if enabled {
        crate::tui::work_surface::release_focus(app);
        app.launch.visible = false;
        crate::tui::agent_focus::exit_focus(app);
        app.file_tree = None;
        app.file_tree_visible = false;
        app.pet_watch.ensure(app.current_session_id.clone());
    } else {
        let session = app.pet_watch.session.clone();
        app.pet_watch.reset(session);
    }
    app.needs_redraw = true;
}
/// F5 and `/pet inspect` enter the same existing modal stack. Printable keys,
/// paste, submit and editing remain with the normal composer on the main view.
pub fn handle_inspect_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    use crate::tui::shell_key_routing::{self, ShellBindingId};
    if app.pet_watch.enabled
        && shell_key_routing::route(app.focus(), key) == Some(ShellBindingId::PetInspect)
    {
        open_habitat(app);
        true
    } else {
        false
    }
}
/// The existing Engine determines work boundaries. Only the shell reads the
/// answer; no conversation text enters the pet owner or recording.
pub fn observe(app: &mut App, event: &Event, now: Instant) {
    app.pet_watch
        .observe(event, app.current_session_id.as_deref(), now);
    if matches!(event, Event::TurnStarted { .. }) {
        app.pet_watch.work_history_start = app.history.len();
        app.pet_watch.full.reset_output();
    } else if matches!(event, Event::TurnComplete { .. }) {
        app.needs_redraw = true;
    }
}
pub fn tick(app: &mut App, now: Instant) {
    // Only the visible native backdrop or its inspector advances the stage.
    // Other modal owners pause it; the existing composer never becomes a modal.
    let visible = !app.redaction_gate
        && app.onboarding == crate::tui::app::OnboardingState::None
        && is_open(app);
    let motion = visible && full::motion(app).animates();
    if visible && app.pet_watch.avatars.refresh(&app.workspace, now) {
        app.needs_redraw = true;
    }
    let sound_allowed = visible
        && app.onboarding == crate::tui::app::OnboardingState::None
        && !app.notification_settings.quiet
        && !app.notification_settings.event_sound.quiet;
    let controls = app
        .pet_watch
        .controls
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    let agents = if controls.is_empty() {
        Vec::new()
    } else {
        full::agents(app)
    };
    let state = &mut app.pet_watch;
    if state.session != app.current_session_id {
        state.reset(app.current_session_id.clone());
    }
    if visible {
        state.ensure(app.current_session_id.clone());
    }
    for control in controls {
        apply(state, control, &agents);
        app.needs_redraw = true;
    }
    if let Some(update) = state
        .worker
        .as_ref()
        .and_then(|w| w.latest.lock().ok().and_then(|mut s| s.take()))
    {
        if update.scene.audio_unavailable {
            state.sound_requested = false;
        }
        state.raster = Some(update);
        state.unavailable = false;
        if visible {
            app.needs_redraw = true;
        }
    }
    if state.raster.as_ref().is_some_and(|r| {
        now.saturating_duration_since(r.frame_changed) > Duration::from_millis(800)
    }) {
        state.raster = None;
        if visible {
            app.needs_redraw = true;
        }
    }
    if state
        .last_tick
        .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(30))
    {
        state.last_tick = Some(now);
        let state = &app.pet_watch;
        if let Some(worker) = &state.worker {
            let waiting = matches!(
                ShellPhase::from_app(app),
                ShellPhase::Waiting | ShellPhase::Approval
            );
            let a = state.area.unwrap_or(Rect::new(0, 0, 40, 8));
            let cell = crossterm::terminal::window_size()
                .ok()
                .filter(|s| s.columns > 0 && s.rows > 0 && s.width > 0 && s.height > 0)
                .map(|s| {
                    (
                        f64::from(s.width) / f64::from(s.columns),
                        f64::from(s.height) / f64::from(s.rows),
                    )
                })
                .unwrap_or((8.0, 16.0));
            let next = live::View {
                locale: app.ui_locale.tag().into(),
                width: a.width.clamp(1, 512),
                height: a.height.saturating_sub(1).clamp(1, 256),
                cell_width: cell.0,
                cell_height: cell.1,
                motion,
                pixels: !full::canonical(state)
                    && matches!(state.avatars.key.as_str(), "" | "whale")
                    && crate::tui::mark::kitty_graphics_supported()
                    && app.synchronized_output_enabled
                    && std::env::var("CODEWHALE_PET_GRAPHICS").as_deref() != Ok("braille"),
                visible,
                waiting,
                sound: state.sound_requested && sound_allowed,
            };
            if let Ok(mut view) = worker.view.lock() {
                *view = next;
            }
        }
    }
    let notices: Vec<_> = app
        .pet_watch
        .worker
        .as_ref()
        .map(|w| w.notices.try_iter().take(16).collect())
        .unwrap_or_default();
    for notice in notices {
        app.pet_watch.exporting = false;
        let (text, level) = match notice {
            Notice::AppearanceLegacy => (
                tr(app.ui_locale, MessageId::PetAppearanceLegacy).into_owned(),
                StatusToastLevel::Warning,
            ),
            Notice::Exported(path) => (
                tr(app.ui_locale, MessageId::PetWatchExported)
                    .replace("{path}", &path.display().to_string()),
                StatusToastLevel::Info,
            ),
            // A refused action (select, export, open) leaves a reachable
            // companion and the habitat as they are.
            Notice::Message(message) => (
                format!(
                    "{} · {message}",
                    tr(app.ui_locale, MessageId::PetWatchUnavailable)
                ),
                StatusToastLevel::Warning,
            ),
            Notice::Unreachable(message) => {
                app.pet_watch.unavailable = true;
                if app.is_loading
                    && app.view_stack.top_kind() == Some(ModalKind::PetHabitat)
                    && !full::canonical(&app.pet_watch)
                {
                    app.view_stack.pop();
                }
                (
                    format!(
                        "{} · {message}",
                        tr(app.ui_locale, MessageId::PetWatchUnavailable)
                    ),
                    StatusToastLevel::Warning,
                )
            }
        };
        app.add_message(crate::tui::history::HistoryCell::System {
            content: text.clone(),
        });
        app.push_status_toast(text, level, Some(12000));
        app.needs_redraw = true;
    }
    full::tick(app, visible, now);
}
fn render_tank(frame: &mut Frame, area: Rect, app: &mut App) {
    app.pet_watch.area = Some(area);
    let raster = app.pet_watch.raster.as_ref();
    let hollow = raster.is_none_or(|r| !r.scene.producer_connected || r.scene.style.hollow);
    let scene = raster
        .map(|r| {
            let mut text = format!(
                "{} · {} · {}",
                r.scene.style.channel, r.scene.style.arch, r.scene.behaviour
            );
            if let Some(activity) = &r.scene.activity
                && activity.observed
                && activity.freshness == codewhale_protocol::engine_owner::OwnerFreshness::Fresh
                && r.frame_changed.elapsed().as_millis() < 800
            {
                if let Some(action) = activity.action_id.as_deref() {
                    text = format!("{action} · {text}");
                } else if let Some(kind) = activity.activity_kind {
                    text = format!("{} · {}", kind.as_str(), text);
                }
                if activity.parallel_agent_count > 0 {
                    text.push_str(&format!(" · ×{}", activity.parallel_agent_count));
                }
            }
            text
        })
        .unwrap_or_default();
    // With no companion frame the tank still paints the resting whale and
    // says why, instead of a blank tank under an orphan separator.
    let label = if raster.is_none() && app.pet_watch.unavailable {
        tr(app.ui_locale, MessageId::PetOffline).into_owned()
    } else {
        let presence = hollow.then(|| tr(app.ui_locale, MessageId::PetUnobserved));
        let sound = tr(app.ui_locale, app.pet_watch.sound_label());
        [
            Some(scene.as_str()),
            presence.as_deref(),
            Some(sound.as_ref()),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    };
    let image = (app.view_stack.is_empty()
        || app.view_stack.top_kind() == Some(ModalKind::PetHabitat))
        && raster.is_some_and(|r| {
            r.image.is_some() && r.width == area.width && r.height == area.height.saturating_sub(1)
        })
        && area.height >= 4;
    let bg = raster
        .map(|r| r.scene.appearance.background)
        .unwrap_or([8, 15, 21]);
    let ink = raster
        .map(|r| {
            Color::Rgb(
                r.scene.style.r.clamp(0.0, 255.0) as u8,
                r.scene.style.g.clamp(0.0, 255.0) as u8,
                r.scene.style.b.clamp(0.0, 255.0) as u8,
            )
        })
        .unwrap_or(Color::Rgb(180, 210, 216));
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Rgb(bg[0], bg[1], bg[2]))),
        area,
    );
    let (act, clock) = avatars::act(raster, app.is_loading);
    let reduced = !full::motion(app).animates();
    let tank = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    if app
        .pet_watch
        .avatars
        .paint(tank, frame.buffer_mut(), act, clock, reduced)
    {
        let preview = app.pet_watch.avatars.action.as_deref().or(app
            .pet_watch
            .avatars
            .view
            .as_deref());
        let label = preview.map_or(label.clone(), |p| format!("/pet action {p} · {label}"));
        frame.render_widget(
            Paragraph::new(label).style(chrome_style(&app.ui_theme, ChromeInk::Metadata)),
            Rect {
                y: area.bottom().saturating_sub(1),
                height: 1,
                ..area
            },
        );
        return;
    }
    if image {
        let tank = Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        app.pet_watch.desired = Some(tank);
        frame.render_widget(
            Paragraph::new(label).style(chrome_style(&app.ui_theme, ChromeInk::Metadata)),
            Rect {
                y: area.bottom() - 1,
                height: 1,
                ..area
            },
        );
    } else {
        crate::tui::ambient_life::pet_widget::render_grid(
            area,
            frame.buffer_mut(),
            raster
                .filter(|r| r.width == area.width && r.height == area.height.saturating_sub(1))
                .map_or(&[], |r| r.cells.as_slice()),
            &label,
            Style::default().fg(ink),
        );
        if let Some(raster) = raster.filter(|r| r.width == area.width && r.height == tank.height)
            && area.height >= 4
            && unicode_width::UnicodeWidthStr::width(label.as_str()) <= usize::from(area.width)
        {
            for y in tank.y..tank.bottom() {
                for x in tank.x..tank.right() {
                    let index =
                        usize::from(y - tank.y) * usize::from(tank.width) + usize::from(x - tank.x);
                    if raster.cells.get(index).is_some_and(|bits| *bits != 0)
                        && let Some(rgb) = raster.colors.get(index)
                        && let Some(cell) = frame.buffer_mut().cell_mut((x, y))
                    {
                        cell.set_fg(Color::Rgb(rgb[0], rgb[1], rgb[2]));
                    }
                }
            }
        }
        if raster.is_none() {
            paint_resting_whale(frame, area, Style::default().fg(ink));
        }
    }
}

/// The launch screen's braille whale, centred in the tank above its caption,
/// at the largest rung that fits. Static: it rests until the companion's own
/// frames take over the tank.
fn paint_resting_whale(frame: &mut Frame, area: Rect, style: Style) {
    use crate::tui::mark::MarkSize;
    let tank_height = area.height.saturating_sub(1);
    let Some(size) = [MarkSize::Large, MarkSize::Small, MarkSize::Tiny]
        .into_iter()
        .find(|size| {
            let (cols, rows) = size.cells();
            cols <= area.width && rows <= tank_height
        })
    else {
        return;
    };
    let (cols, rows) = size.cells();
    let x0 = area.x + (area.width - cols) / 2;
    let y0 = area.y + (tank_height - rows) / 2;
    let buf = frame.buffer_mut();
    for (dy, row) in size.rows().iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x0 + dx as u16, y0 + dy as u16)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }
}
pub(crate) fn clear_images(output: &mut impl Write) -> io::Result<()> {
    graphics::clear(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn work_completion_reveals_existing_answer_without_sending_text_to_owner() {
        use crate::core::events::TurnOutcomeStatus;
        let mut app =
            crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.redaction_gate = false;
        assert!(app.view_stack.is_empty());
        app.input = "retained draft".into();
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.failed = true; // No connection or provider for this shell test.
        app.pet_watch.enabled = true;
        observe(
            &mut app,
            &Event::TurnStarted {
                turn_id: "preview-turn".into(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            },
            Instant::now(),
        );
        tick(&mut app, Instant::now());
        assert!(
            app.view_stack.is_empty(),
            "accepted work keeps the composer focused"
        );
        assert!(main_view(&app));
        app.add_message(crate::tui::history::HistoryCell::Assistant {
            content: "Prepared result stays in the transcript".into(),
            streaming: false,
        });
        observe(
            &mut app,
            &Event::TurnComplete {
                usage: Default::default(),
                parent_route_usage: Default::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            },
            Instant::now(),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| render_full(frame, &mut app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Prepared result stays in the transcript"));
        assert_eq!(app.input, "retained draft");
        assert!(app.pet_watch.worker.is_none());
    }

    #[test]
    fn unavailable_companion_keeps_legacy_fallback_and_native_response_available() {
        let mut app =
            crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.redaction_gate = false;
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.detach_for_test();
        app.pet_watch.unavailable = true;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal
            .draw(|frame| render_tank(frame, frame.area(), &mut app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains(crate::tui::mark::MarkSize::Large.rows()[3].trim()),
            "the tank paints the resting whale: {text}"
        );
        assert!(
            text.contains("offline — codewhale pet serve wakes it"),
            "{text}"
        );
        assert!(!text.contains(" · offline"), "no orphan separator: {text}");

        app.pet_watch.enabled = true;
        app.is_loading = true;
        observe(
            &mut app,
            &Event::TurnStarted {
                turn_id: "turn".into(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            },
            Instant::now(),
        );
        app.add_message(crate::tui::history::HistoryCell::Assistant {
            content: "Fixture response while telemetry reconnects".into(),
            streaming: true,
        });
        tick(&mut app, Instant::now());
        assert!(
            is_open(&app),
            "native pet mode keeps the actual turn available without a companion raster"
        );
        terminal.draw(|frame| render_full(frame, &mut app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Fixture response while telemetry reconnects"));
        assert!(app.pet_watch.worker.is_none());
    }

    #[test]
    fn a_refused_pet_action_keeps_the_habitat_and_only_unreachable_marks_offline() {
        let mut app =
            crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.redaction_gate = false;
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.detach_for_test();
        let (tx, _commands) = std::sync::mpsc::sync_channel(4);
        let (notices_tx, notices) = std::sync::mpsc::sync_channel(4);
        app.pet_watch.worker = Some(Worker {
            tx,
            latest: std::sync::Arc::new(std::sync::Mutex::new(None)),
            view: std::sync::Arc::new(std::sync::Mutex::new(live::View::default())),
            notices,
        });
        open_habitat(&mut app);
        app.is_loading = true;

        notices_tx
            .send(Notice::Message(
                "Save the terminal session before exporting".into(),
            ))
            .unwrap();
        tick(&mut app, Instant::now());
        assert!(is_open(&app), "a refused export must not close pet mode");
        assert!(
            !app.pet_watch.unavailable,
            "a refused export is not offline"
        );

        notices_tx
            .send(Notice::Unreachable("Shared pet reconnecting".into()))
            .unwrap();
        tick(&mut app, Instant::now());
        assert!(app.pet_watch.unavailable);
        assert!(
            is_open(&app),
            "the canonical whale and Engine response remain available while telemetry reconnects"
        );
    }

    #[test]
    fn pet_off_restores_the_shell_and_keeps_the_draft() {
        let mut app =
            crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.redaction_gate = false;
        app.input = "kept draft".into();
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.detach_for_test();
        app.pet_watch.enabled = true;
        observe(
            &mut app,
            &Event::TurnStarted {
                turn_id: "turn".into(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            },
            Instant::now(),
        );
        assert!(main_view(&app));
        assert!(app.view_stack.is_empty());
        set_enabled(&mut app, false);
        tick(&mut app, Instant::now());
        assert!(!app.pet_watch.enabled);
        assert!(app.view_stack.is_empty());
        assert_eq!(app.input, "kept draft");
        assert!(app.pet_watch.worker.is_none());
    }

    #[test]
    fn session_switch_resets_the_view_and_tracks_the_switching_turn() {
        let mut watch = PetWatch::default();
        watch.detach_for_test();
        watch.session = Some("session-a".into());
        watch.active_turn_id = Some("turn-a".into());
        watch.full.output_scroll = 100;
        watch.work_history_start = 50;

        watch.observe(
            &Event::TurnStarted {
                turn_id: "turn-b".into(),
                created_at: chrono::Utc::now(),
                route: None,
                submission_id: None,
            },
            Some("session-b"),
            Instant::now(),
        );
        assert_eq!(watch.session.as_deref(), Some("session-b"));
        // The event that switched sessions is not dropped: its turn is the
        // one a later `turn_complete` must carry to claim Done.
        assert_eq!(watch.active_turn_id.as_deref(), Some("turn-b"));
        assert_eq!(watch.full.output_scroll, 0);
        assert_eq!(watch.work_history_start, 0);
        assert!(!watch.failed, "a switch clears a prior failure");
        assert!(
            watch.worker.is_none(),
            "with no live view running, an Engine event never starts one"
        );

        let done = metadata(
            &Event::TurnComplete {
                usage: Default::default(),
                parent_route_usage: Default::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            },
            watch.active_turn_id.as_deref(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&done).unwrap(),
            json!({"event":"turn_complete","turn_id":"turn-b","turn_outcome":"completed"})
        );
    }

    #[test]
    fn foreground_projection_forwards_only_typed_owner_metadata() {
        let call = metadata(
            &Event::ToolCallStarted {
                model_call: None,
                id: "call-a".into(),
                name: "exec_command".into(),
                input: json!({"command":"PRIVATE TOOL INPUT"}),
            },
            Some("turn-a"),
        );
        assert!(
            call.is_none(),
            "tool names and inputs are not activity facts"
        );

        let thought = metadata(
            &Event::ThinkingDelta {
                index: 2,
                content: "PRIVATE REASONING".into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&thought).unwrap(),
            json!({"event":"response_delta","index":2,"channel":"reasoning"})
        );
        let message = metadata(
            &Event::MessageDelta {
                index: 3,
                content: "PRIVATE MESSAGE".into(),
            },
            None,
        )
        .unwrap();

        let operation = metadata(
            &Event::OperationActivityStarted {
                span_id: "private-internal-span".into(),
                activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind::Computer,
                action_id: Some("mcp_computer_use_click".into()),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&operation).unwrap(),
            json!({
                "event":"operation_activity_started",
                "span_id":"private-internal-span",
                "activity_kind":"computer",
                "action_id":"mcp_computer_use_click"
            })
        );
        let completed = metadata(
            &Event::OperationActivityCompleted {
                span_id: "private-internal-span".into(),
                activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind::Editing,
                action_id: Some("edit_file".into()),
                outcome: codewhale_protocol::engine_owner::OwnerOperationOutcome::Denied,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&completed).unwrap(),
            json!({
                "event":"operation_activity_completed",
                "span_id":"private-internal-span",
                "activity_kind":"editing",
                "action_id":"edit_file",
                "outcome":"denied"
            })
        );

        let turn = metadata(
            &Event::TurnComplete {
                usage: Default::default(),
                parent_route_usage: Default::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Completed,
                error: None,
                tool_catalog: None,
                base_url: None,
            },
            Some("stable-turn-a"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&turn).unwrap(),
            json!({
                "event":"turn_complete",
                "turn_id":"stable-turn-a",
                "turn_outcome":"completed"
            })
        );

        assert!(!message.contains("PRIVATE"));
        assert!(call.is_none());
        assert!(!thought.contains("PRIVATE"));
        assert!(!operation.contains("tool_name"));
        assert!(!operation.contains("PRIVATE"));
    }

    /// The pet's multi-agent count is derived on the JS side from
    /// `agent:`-prefixed spans, keyed by the `id` this projection forwards
    /// (app-side issue #12). The counting itself is proven in
    /// `pet/tests/pet-engine.test.mjs`; the handoff is the half that fails
    /// silently — a trimmed allowlist or a dropped id zeroes `parallel` with
    /// no error and no log line, and nothing else here covers agent events.
    /// Pin the wire shape the JS dispatches on, and keep child text off it.
    #[test]
    fn agent_events_forward_span_identity_without_child_text() {
        use crate::core::events::AgentProgressEventMeta;
        use crate::tools::subagent::{AgentWorkerStatus, SubAgentStatus};

        let spawned = metadata(
            &Event::AgentSpawned {
                display_name: None,
                owner_session_id: "session-a".into(),
                id: "agent-1".into(),
                prompt: "PRIVATE CHILD PROMPT".into(),
                worker_status: Some(AgentWorkerStatus::Running),
                parent_run_id: Some("run-9".into()),
                spawn_depth: 2,
                model: "PRIVATE CHILD MODEL".into(),
                route_source: Some("task.model".into()),
            },
            None,
        )
        .expect("agent spawns are observed");
        assert_eq!(
            serde_json::from_str::<Value>(&spawned).unwrap(),
            // A spawn opens the span; the reducer reads no status from it.
            json!({"event":"agent_spawned","id":"agent-1"})
        );

        // The JS finishes a span when progress reports a terminal status.
        let progress = metadata(
            &Event::AgentProgress {
                owner_session_id: "session-a".into(),
                id: "agent-1".into(),
                status: "PRIVATE PROGRESS TEXT".into(),
                activity: AgentProgressEventMeta {
                    worker_status: AgentWorkerStatus::Completed,
                    step: Some(3),
                    tool_name: Some("exec_command".into()),
                    routine_wait: false,
                    approval_id: None,
                },
                parent_run_id: Some("run-9".into()),
                spawn_depth: 2,
            },
            None,
        )
        .expect("agent progress is observed");
        assert_eq!(
            serde_json::from_str::<Value>(&progress).unwrap(),
            json!({"event":"agent_progress","id":"agent-1","worker_status":"completed"})
        );

        let complete = metadata(
            &Event::AgentComplete {
                display_name: None,
                owner_session_id: "session-a".into(),
                id: "agent-1".into(),
                result: "PRIVATE CHILD RESULT".into(),
                outcome: Some(SubAgentStatus::Completed),
                parent_run_id: Some("run-9".into()),
                spawn_depth: Some(2),
                continuable: Some(false),
                usage: None,
            },
            None,
        )
        .expect("agent completions are observed");
        assert_eq!(
            serde_json::from_str::<Value>(&complete).unwrap(),
            json!({"event":"agent_complete","id":"agent-1"})
        );

        for (label, payload) in [
            ("spawned", &spawned),
            ("progress", &progress),
            ("complete", &complete),
        ] {
            assert!(
                !payload.contains("PRIVATE"),
                "{label} leaked child text: {payload}"
            );
        }
    }
}
