//! The whale view of one terminal. Engine events from this terminal's own
//! session are projected here once and drawn locally as the dot whale; no
//! other process takes part.
//!
//! Known limits: hold-still lasts until this terminal closes. The name and
//! colour live in settings.toml beside `pet_mode`; a change another process
//! makes is picked up within two seconds while the whale is kept.
use crate::tui::{app::App, underwater::ShellPhase, views::ModalKind};
use codewhale_engine::core::events::{Event, TurnOutcomeStatus};
use ratatui::layout::Rect;
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
const IDENTITY_POLL: Duration = Duration::from_secs(2);
type IdentityStamp = Option<(SystemTime, u64)>;
type DiskIdentity = (Option<String>, Option<String>);
mod dots;
mod full;
mod habitat;
pub use full::{render_full, render_main};
#[derive(Clone, Copy)]
pub enum Control {
    Scroll(i16),
    FocusAgents,
    ScrollEnd(bool),
    Mouse(crossterm::event::MouseEvent),
}
#[derive(Default)]
pub struct PetWatch {
    session: Option<String>,
    active_turn_id: Option<String>,
    pub(crate) area: Option<Rect>,
    pub(crate) inspect_area: Option<Rect>,
    copy_area: Arc<Mutex<Option<Rect>>>,
    dots: Option<dots::Dots>,
    dots_frame: Option<dots::Frame>,
    dots_area: Option<Rect>,
    controls: Arc<Mutex<Vec<Control>>>,
    /// `/whale`: the whale replaces the ordinary shell backdrop.
    pub(crate) enabled: bool,
    still: bool,
    name: Option<String>,
    color: Option<[u8; 3]>,
    identity_checked: Option<Instant>,
    identity_poll: Arc<Mutex<(IdentityStamp, Option<DiskIdentity>, bool)>>,
    work_history_start: usize,
    full: codewhale_ratatui::PetModeState,
    full_next: Option<Instant>,
    full_response: Option<crate::tui::transcript::TranscriptViewCache>,
    full_response_key: Option<(u64, usize, bool)>,
    full_inputs: Option<codewhale_ratatui::whale_motion::Inputs>,
    selection: Arc<Mutex<Option<String>>>,
    #[cfg(test)]
    detached: bool,
}
impl PetWatch {
    pub fn still(&self) -> bool {
        self.still
    }
    pub fn set_still(&mut self, still: bool) {
        self.still = still;
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
        self.identity_checked = Some(Instant::now());
    }
    pub fn color(&self) -> Option<[u8; 3]> {
        self.color
    }
    pub fn set_color(&mut self, color: Option<&str>) {
        self.color = color.and_then(codewhale_config::whale_identity::whale_color_rgb);
        self.identity_checked = Some(Instant::now());
    }
    pub fn apply_identity(&mut self, name: Option<&str>, color: Option<&str>) {
        self.name = name.map(str::to_owned);
        self.color = color.and_then(codewhale_config::whale_identity::whale_color_rgb);
    }
    fn refresh_identity(&mut self, now: Instant) -> bool {
        let pending = self
            .identity_poll
            .lock()
            .ok()
            .and_then(|mut poll| poll.1.take());
        let applied = pending.is_some_and(|(name, color)| {
            let changed = self.name != name
                || self.color
                    != color
                        .as_deref()
                        .and_then(codewhale_config::whale_identity::whale_color_rgb);
            self.apply_identity(name.as_deref(), color.as_deref());
            changed
        });
        let due = self
            .identity_checked
            .is_none_or(|at| now.saturating_duration_since(at) >= IDENTITY_POLL);
        if due && tokio::runtime::Handle::try_current().is_ok() {
            self.identity_checked = Some(now);
            let poll = Arc::clone(&self.identity_poll);
            if poll
                .lock()
                .is_ok_and(|mut state| !std::mem::replace(&mut state.2, true))
            {
                codewhale_util::utils::spawn_blocking_supervised("whale-identity", move || {
                    read_identity(&poll);
                });
            }
        }
        applied
    }
    fn reset(&mut self, session: Option<String>) {
        self.session = session;
        self.active_turn_id = None;
        if let Some(dots) = &self.dots {
            dots.reset();
        }
        self.dots_frame = None;
        self.work_history_start = 0;
        self.full = codewhale_ratatui::PetModeState::default();
        self.full_next = None;
        self.full_response = None;
        self.full_response_key = None;
        self.full_inputs = None;
        if let Ok(mut selection) = self.selection.lock() {
            *selection = None;
        }
    }
    fn start_dots(&mut self) {
        #[cfg(test)]
        if self.detached {
            return;
        }
        self.dots = Some(dots::Dots::start());
    }
    /// Tests drive the shell without the dot whale's thread.
    #[cfg(test)]
    pub(crate) fn detach_for_test(&mut self) {
        self.dots = None;
        self.detached = true;
    }
    pub fn observe(&mut self, event: &Event, session: Option<&str>) {
        if self.session.as_deref() != session {
            self.reset(session.map(str::to_owned));
        }
        if let Event::TurnStarted { turn_id, .. } = event {
            self.active_turn_id = Some(turn_id.clone());
        }
        if let Some(text) = metadata(event, self.active_turn_id.as_deref())
            && let Some(dots) = &self.dots
        {
            dots.observe(text);
        }
        if matches!(event, Event::TurnComplete { .. }) {
            self.active_turn_id = None;
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
    }
}
fn read_identity(poll: &Mutex<(IdentityStamp, Option<DiskIdentity>, bool)>) {
    use codewhale_runtime::settings::Settings;
    let stamp = Settings::path()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
    let seen = poll.lock().map(|state| state.0).unwrap_or(None);
    let identity = (stamp != seen)
        .then(Settings::load_persisted)
        .and_then(Result::ok)
        .map(|settings| (settings.whale_name, settings.whale_color));
    if let Ok(mut state) = poll.lock() {
        if identity.is_some() {
            state.0 = stamp;
            state.1 = identity;
        }
        state.2 = false;
    }
}
/// Existing protocol projection defines the variant names. This allowlist
/// removes payloads before the dot whale's bounded queue; no model text escapes.
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
            result: Err(codewhale_engine::tools::spec::ToolError::PermissionDenied { .. }),
            ..
        } => json!({
            "event":"approval_resolved","id":id,"outcome":"denied"
        }),
        Event::ToolCallComplete {
            id,
            result: Err(codewhale_engine::tools::spec::ToolError::Cancelled { .. }),
            ..
        } => json!({
            "event":"approval_resolved","id":id,"outcome":"cancelled"
        }),
        Event::UserInputClosed { id, outcome } => json!({
            "event":"approval_resolved", "id":id,
            "outcome":if matches!(outcome, codewhale_engine::core::events::UserInputClosedOutcome::Answered) { "approved" } else { "cancelled" }
        }),
        Event::AgentSpawned { id, .. } => json!({"event":"agent_spawned","id":id}),
        Event::AgentProgress { id, activity, .. } => json!({
            "event":"agent_progress",
            "id":id,
            "worker_status":codewhale_engine::core::protocol_parity::worker_status_str(activity.worker_status)
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

pub fn command(app: &mut App, control: Control) -> bool {
    let agents = full::agents(app);
    let state = &mut app.pet_watch;
    match control {
        Control::Scroll(delta) => state.full.scroll(&agents, delta),
        Control::FocusAgents => state.full.toggle_agents(&agents),
        Control::ScrollEnd(end) => state.full.scroll_end(&agents, end),
        Control::Mouse(mouse) => {
            state.full.handle_mouse(&agents, mouse);
        }
    }
    app.needs_redraw = true;
    true
}
pub fn open_habitat(app: &mut App) {
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
/// `/whale` selects the shell backdrop without taking composer focus.
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
    } else {
        let session = app.pet_watch.session.clone();
        app.pet_watch.reset(session);
        app.pet_watch.dots = None;
        app.pet_watch.dots_frame = None;
    }
    app.needs_redraw = true;
}
/// F5 enters the existing modal stack. Printable keys, paste, submit and
/// editing remain with the normal composer on the main view.
pub fn handle_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    use crate::tui::shell_key_routing::{self, ShellBindingId as Id};
    if !app.pet_watch.enabled {
        return false;
    }
    if shell_key_routing::route(app.focus(), key) != Some(Id::PetInspect) {
        return false;
    }
    open_habitat(app);
    true
}
/// The existing Engine determines work boundaries. Only the shell reads the
/// answer; no conversation text enters the whale's projection.
pub fn observe(app: &mut App, event: &Event) {
    app.pet_watch
        .observe(event, app.current_session_id.as_deref());
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
    let waiting = matches!(
        ShellPhase::from_app(app),
        ShellPhase::Waiting | ShellPhase::Approval
    );
    let keep_dots = app.pet_watch.enabled || app.view_stack.contains_kind(ModalKind::PetHabitat);
    let controls = app
        .pet_watch
        .controls
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    for control in controls {
        command(app, control);
    }
    let state = &mut app.pet_watch;
    if keep_dots && state.refresh_identity(now) {
        app.needs_redraw = true;
    }
    let state = &mut app.pet_watch;
    if state.session != app.current_session_id {
        state.reset(app.current_session_id.clone());
    }
    let want = visible && state.dots_area.is_some();
    if !keep_dots {
        state.dots = None;
        state.dots_frame = None;
    } else if want && state.dots.is_none() {
        state.start_dots();
    }
    if let Some(dots) = &state.dots {
        dots.set_view(want, motion, waiting);
        if dots.failed() {
            state.dots_frame = None;
        } else if let Some(frame) = dots.take() {
            state.dots_frame = Some(frame);
            if visible {
                app.needs_redraw = true;
            }
        }
    }
    full::tick(app, visible, now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn app() -> App {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options("."),
        );
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.redaction_gate = false;
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.detach_for_test();
        app
    }
    fn turn_started(turn_id: &str) -> Event {
        Event::TurnStarted {
            previous_source: None,
            turn_id: turn_id.into(),
            created_at: chrono::Utc::now(),
            route: None,
            submission_id: None,
        }
    }
    fn turn_completed() -> Event {
        Event::TurnComplete {
            source_accounting: false,

            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: Default::default(),
            parent_route_usage: Default::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        }
    }

    #[test]
    fn work_completion_reveals_existing_answer_and_keeps_the_draft() {
        let mut app = app();
        assert!(app.view_stack.is_empty());
        app.input = "retained draft".into();
        app.pet_watch.enabled = true;
        observe(&mut app, &turn_started("preview-turn"));
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
        observe(&mut app, &turn_completed());
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
    }

    #[test]
    fn the_whale_view_draws_from_this_terminal_and_the_inspector_needs_no_other_process() {
        let mut app = app();
        set_enabled(&mut app, true);
        assert!(is_open(&app));
        assert!(command(&mut app, Control::FocusAgents));
        open_habitat(&mut app);
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::PetHabitat));
        assert!(is_open(&app));
        tick(&mut app, Instant::now());
        assert!(
            app.pet_watch.dots.is_none(),
            "detached tests start no thread"
        );
        set_enabled(&mut app, false);
        assert!(app.view_stack.is_empty());
        assert!(!is_open(&app));
    }

    #[test]
    fn whale_off_restores_the_shell_and_keeps_the_draft() {
        let mut app = app();
        app.input = "kept draft".into();
        app.pet_watch.enabled = true;
        observe(&mut app, &turn_started("turn"));
        assert!(main_view(&app));
        assert!(app.view_stack.is_empty());
        set_enabled(&mut app, false);
        tick(&mut app, Instant::now());
        assert!(!app.pet_watch.enabled);
        assert!(app.view_stack.is_empty());
        assert_eq!(app.input, "kept draft");
        assert!(app.pet_watch.dots.is_none());
    }

    #[test]
    fn name_colour_and_stillness_survive_a_session_switch() {
        let mut watch = PetWatch::default();
        watch.detach_for_test();
        watch.set_name(Some("Moby".into()));
        watch.set_color(Some("#0a141e"));
        watch.set_still(true);
        watch.observe(&turn_started("turn"), Some("another-session"));
        assert_eq!(watch.name(), Some("Moby"));
        assert_eq!(watch.color(), Some([10, 20, 30]));
        assert!(watch.still());
    }

    #[test]
    fn session_switch_resets_the_view_and_tracks_the_switching_turn() {
        let mut watch = PetWatch::default();
        watch.detach_for_test();
        watch.session = Some("session-a".into());
        watch.active_turn_id = Some("turn-a".into());
        watch.full.output_scroll = 100;
        watch.work_history_start = 50;

        watch.observe(&turn_started("turn-b"), Some("session-b"));
        assert_eq!(watch.session.as_deref(), Some("session-b"));
        // The event that switched sessions is not dropped: its turn is the
        // one a later `turn_complete` must carry to claim Done.
        assert_eq!(watch.active_turn_id.as_deref(), Some("turn-b"));
        assert_eq!(watch.full.output_scroll, 0);
        assert_eq!(watch.work_history_start, 0);

        let done = metadata(&turn_completed(), watch.active_turn_id.as_deref()).unwrap();
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
                source_accounting: false,

                turn_id: None,
                submission_id: None,
                compaction_id: None,
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
        use codewhale_engine::core::events::AgentProgressEventMeta;
        use codewhale_engine::tools::subagent::{AgentWorkerStatus, SubAgentStatus};

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
