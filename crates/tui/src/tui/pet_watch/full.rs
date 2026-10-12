//! The actual `/pet on` surface over the foreground Engine session. This
//! replaces the old full-screen tank layout; the companion still owns its
//! durable world and audio. No prompt, worker or execution authority lives here.
use super::{PetWatch, habitat};
use crate::{
    agent_roster::{AgentRosterRow, RosterState},
    tools::subagent::AgentWorkerStatus,
    tui::{
        app::App,
        history::HistoryCell,
        shell_key_routing::{self, ShellBindingId as Id},
        underwater::ShellPhase,
    },
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerFreshness, OwnerPresence};
use codewhale_ratatui::{
    AgentCard, Caps, DotWhale, MotionMode, PetMode, PetModeAreas, State, StatusMark, Subagent,
    SubagentControls, SubagentUsage, SubagentViewWords, Theme,
    whale_motion::{Activity, Context, Inputs, Presence, Span},
};
use ratatui::{Frame, layout::Rect, text::Line};
use std::time::{Duration, Instant};

pub(super) fn canonical(state: &PetWatch) -> bool {
    matches!(state.avatars.key.as_str(), "" | "whale")
        && state.avatars.action.is_none()
        && state.avatars.view.is_none()
}
fn theme(app: &App) -> Theme {
    let mut caps = Caps::detect();
    if let Some(appearance) =
        codewhale_ratatui::detect::appearance_for_background(app.ui_theme.surface_bg)
    {
        caps.appearance = appearance;
    }
    let theme = Theme::new(caps);
    codewhale_ratatui::TuiPalette::ALL
        .into_iter()
        .find(|palette| palette.name() == app.ui_theme.name)
        .map_or(theme, |palette| theme.tui_palette(palette))
}

fn live_rows(app: &App) -> Vec<AgentRosterRow> {
    use crate::tui::subagent_routing::{subagent_is_parked, worker_status_for_terminal_projection};
    let mut rows = app.current_agent_roster().to_vec();
    let name = |id: &str| app.agent_label_map.get(id).cloned();
    let blank = |id: &str, display_name: String| AgentRosterRow {
        worker_id: id.to_string(),
        display_name,
        model: String::new(),
        state: RosterState::Running,
        status: AgentWorkerStatus::Running,
        activity: None,
        outcome: None,
        millis: None,
        input_tokens: None,
        output_tokens: None,
        cost_microusd: None,
        steps_taken: 0,
        parent_run_id: None,
        run_id: id.to_string(),
    };
    for agent in &app.subagent_cache {
        let index = rows
            .iter()
            .position(|row| row.worker_id == agent.agent_id)
            .unwrap_or_else(|| {
                let display_name = name(&agent.agent_id)
                    .or_else(|| agent.nickname.clone())
                    .unwrap_or_else(|| agent.name.clone());
                let usage = agent.usage.as_ref();
                rows.push(AgentRosterRow {
                    model: agent.model.clone(),
                    millis: Some(agent.duration_ms),
                    input_tokens: usage.and_then(|usage| usage.input_tokens),
                    output_tokens: usage.and_then(|usage| usage.output_tokens),
                    cost_microusd: usage.and_then(|usage| usage.cost_microusd),
                    steps_taken: agent.steps_taken,
                    parent_run_id: agent.parent_run_id.clone(),
                    ..blank(&agent.agent_id, display_name)
                });
                rows.len() - 1
            });
        let row = &mut rows[index];
        row.status = agent
            .worker_status
            .unwrap_or_else(|| worker_status_for_terminal_projection(&agent.status));
        row.state = if subagent_is_parked(agent) {
            RosterState::Parked
        } else {
            RosterState::from_worker(row.status)
        };
        if row.state == RosterState::Running
            && let Some(started) = agent.started_at
        {
            row.millis = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        }
    }
    for (id, progress) in &app.agent_progress {
        match rows.iter_mut().find(|row| &row.worker_id == id) {
            Some(row) => row.activity = Some(progress.clone()),
            None => rows.push(AgentRosterRow {
                activity: Some(progress.clone()),
                ..blank(id, name(id).unwrap_or_else(|| id.clone()))
            }),
        }
    }
    rows
}

pub(super) fn agents(app: &App) -> Vec<Subagent<'static>> {
    live_rows(app)
        .iter()
        .map(|row| {
            let (state, label) = match row.state {
                RosterState::Running if row.status == AgentWorkerStatus::Queued => {
                    (State::Ready, MessageId::AgentRailQueuedCount)
                }
                RosterState::Running => (State::Working, MessageId::SubagentsStatusRunning),
                RosterState::Waiting => (State::NeedsYou, MessageId::PhaseWaitingOnYou),
                RosterState::Parked => (State::Stopped, MessageId::AgentStatusParked),
                RosterState::Done => (State::Done, MessageId::SubagentsStatusCompleted),
                RosterState::Failed => (State::Failed, MessageId::SubagentsStatusFailed),
                RosterState::Cancelled => (State::Stopped, MessageId::SubagentsStatusCancelled),
            };
            let mut card = AgentCard::new(row.display_name.clone(), state)
                .status_word(tr(app.ui_locale, label).replace("{count}", "1"))
                .task(row.activity.clone().unwrap_or_default());
            if !row.model.trim().is_empty() {
                card = card.route(row.model.clone());
            }
            let mut agent = Subagent::new(row.worker_id.clone(), card);
            agent.elapsed = row.millis.map(Duration::from_millis);
            agent.usage = Some(SubagentUsage {
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cost_microusd: row.cost_microusd,
            });
            agent.steps = Some(row.steps_taken);
            // This pane is the retained roster. Enter opens the existing full
            // agent transcript, so long outcomes are not cloned on animation frames.
            agent.controls = SubagentControls {
                open: true,
                ..Default::default()
            };
            agent
        })
        .collect()
}
fn presentation<'a>(
    raster: Option<&'a super::live::Presentation>,
    session: Option<&str>,
    now: Instant,
) -> Option<&'a super::live::Presentation> {
    let source = session.map(super::live::session_source);
    raster.filter(|r| {
        r.scene.producer_connected
            && now.saturating_duration_since(r.frame_changed) < Duration::from_millis(800)
            && Some(r.scene.source.as_str()) == source.as_deref()
    })
}
fn activity_words(kind: OwnerActivityKind, locale: Locale) -> std::borrow::Cow<'static, str> {
    tr(
        locale,
        match kind {
            OwnerActivityKind::Reading => MessageId::ToolFamilyRead,
            OwnerActivityKind::Editing => MessageId::ToolFamilyPatch,
            OwnerActivityKind::Searching => MessageId::ToolFamilyFind,
            OwnerActivityKind::Testing => MessageId::ToolFamilyVerify,
            OwnerActivityKind::Executing => MessageId::ToolFamilyRun,
            OwnerActivityKind::Browsing => MessageId::PetActivityBrowsing,
            OwnerActivityKind::Computer => MessageId::PetActivityComputer,
            OwnerActivityKind::Memory => MessageId::PetActivityMemory,
            OwnerActivityKind::Tool => MessageId::ToolFamilyGeneric,
            OwnerActivityKind::Thinking => MessageId::ToolFamilyThink,
            OwnerActivityKind::Responding => MessageId::PetModeReplyTitle,
            OwnerActivityKind::Delegating => MessageId::PhaseSubagents,
        },
    )
}
fn inputs(app: &App, now: Instant) -> Inputs {
    let presence = match ShellPhase::from_app(app) {
        ShellPhase::Working | ShellPhase::Verifying => Presence::Working,
        ShellPhase::Waiting | ShellPhase::Approval => Presence::NeedsYou,
        ShellPhase::Typing => Presence::Listening,
        ShellPhase::Done => Presence::Done,
        ShellPhase::Idle | ShellPhase::Failed => Presence::Idle,
    };
    let raster = presentation(
        app.pet_watch.raster.as_ref(),
        app.current_session_id.as_deref(),
        now,
    );
    let owner = raster.and_then(|r| {
        r.scene.activity.as_ref().filter(|a| {
            a.observed
                && a.freshness == OwnerFreshness::Fresh
                && a.session_id.as_deref() == Some(r.scene.source.as_str())
        })
    });
    Inputs {
        presence,
        activity: owner.map(|a| Activity {
            kind: a.activity_kind.map(|k| k.as_str().into()),
            observed: true,
            parallel: Some(f64::from(a.parallel_agent_count)),
            active: a
                .active_spans
                .iter()
                .map(|s| Span {
                    kind: s.activity_kind.as_str().into(),
                    since_ms: s.started_at_ms,
                })
                .collect(),
        }),
        context: Context {
            live: owner.is_some(),
            turn_id: app.runtime_turn_id.clone(),
            status: app.runtime_turn_status.clone(),
            // Only owner time, never a local timer pretending to be an error receipt.
            now_ms: raster.map(|r| r.scene.time_ms),
            ..Default::default()
        },
    }
}
pub(super) fn motion(app: &App) -> MotionMode {
    // The habitat owns the viewport through the modal stack. The ordinary
    // shell's ambient helper intentionally stops under every modal, so using
    // it here would freeze the pet simply because its own view is open.
    match app.motion_policy().mode() {
        crate::tui::motion::MotionMode::Still => MotionMode::Still,
        crate::tui::motion::MotionMode::Reduced => MotionMode::Reduced,
        crate::tui::motion::MotionMode::Full
            if matches!(
                ShellPhase::from_app(app),
                ShellPhase::Approval | ShellPhase::Waiting
            ) =>
        {
            MotionMode::Reduced
        }
        crate::tui::motion::MotionMode::Full => MotionMode::Full,
    }
}
pub(super) fn tick(app: &mut App, visible: bool, now: Instant) {
    app.pet_watch.full.set_visible(visible);
    if !visible {
        app.pet_watch.full_next = None;
        return;
    }
    let agents = agents(app);
    // Revalidate the painted identity against the current session before the
    // modal can return an open intent. A removed/ambiguous worker never turns
    // into the new fallback selection before it has actually been painted.
    if let Ok(mut selected) = app.pet_watch.selection.lock()
        && selected.as_deref().is_some_and(|id| {
            agents
                .iter()
                .filter(|agent| agent.id == id && agent.controls.open)
                .count()
                != 1
        })
    {
        *selected = None;
    }
    let inputs = inputs(app, now);
    let changed = app.pet_watch.full_inputs.as_ref().is_none_or(|old| {
        old.presence != inputs.presence
            || old.activity != inputs.activity
            || old.context.live != inputs.context.live
            || old.context.turn_id != inputs.context.turn_id
            || old.context.status != inputs.context.status
    });
    let motion = motion(app);
    let theme = theme(app);
    let prepared = presentation(
        app.pet_watch.raster.as_ref(),
        app.current_session_id.as_deref(),
        now,
    )
    .is_some_and(|r| r.scene.materials.is_some());
    let state = &mut app.pet_watch;
    state.full.update(
        app.current_session_id.as_deref(),
        inputs.clone(),
        &agents,
        now,
        motion,
    );
    state.full_inputs = Some(inputs);
    if changed {
        state.full_next = None;
        app.needs_redraw = true;
    }
    if prepared {
        state.full_next = None;
    } else if let Some(interval) = state.full.next_frame_in(&agents, &theme) {
        if state.full_next.is_none_or(|deadline| now >= deadline) {
            state.full_next = now.checked_add(interval);
            app.needs_redraw = true;
        }
    } else {
        state.full_next = None;
    }
}

pub fn render_full(frame: &mut Frame, app: &mut App) {
    render(frame, frame.area(), app, true);
}
/// The same stage becomes the shell backdrop. Inspection is opt-in so streamed
/// replies and a retained roster never displace the whale or the composer.
pub fn render_main(frame: &mut Frame, area: Rect, app: &mut App) {
    render(frame, area, app, false);
}
fn render(frame: &mut Frame, area: Rect, app: &mut App, inspect: bool) {
    if inspect {
        app.resync_history_revisions();
    }
    let agents = agents(app);
    let theme = theme(app);
    let phase = ShellPhase::from_app(app);
    let notice = app.active_status_toast(phase).map(|toast| {
        Line::styled(
            toast.text,
            codewhale_palette::chrome_style(&app.ui_theme, toast.level.ink()),
        )
    });
    let now = Instant::now();
    let prepared = presentation(
        app.pet_watch.raster.as_ref(),
        app.current_session_id.as_deref(),
        now,
    )
    .filter(|r| r.scene.materials.is_some());
    let activity = prepared.and_then(|r| {
        r.scene.activity.as_ref().filter(|a| {
            a.observed
                && a.freshness == OwnerFreshness::Fresh
                && a.session_id.as_deref() == Some(r.scene.source.as_str())
        })
    });
    let words = if matches!(phase, ShellPhase::Working | ShellPhase::Verifying) {
        activity
            .filter(|a| a.authoritative_presence == OwnerPresence::Working)
            .and_then(|a| a.activity_kind)
            .map(|kind| activity_words(kind, app.ui_locale))
            .unwrap_or_else(|| phase.label(app.ui_locale))
    } else {
        phase.label(app.ui_locale)
    };
    let mark = StatusMark::new(match phase {
        ShellPhase::Working | ShellPhase::Verifying => State::Working,
        ShellPhase::Approval | ShellPhase::Waiting => State::NeedsYou,
        ShellPhase::Done => State::Done,
        ShellPhase::Failed => State::Failed,
        ShellPhase::Idle | ShellPhase::Typing => State::Ready,
    })
    .word(words.clone().into_owned());
    // ActiveCell is the same live transcript the normal shell reads. Include
    // streamed replies and terminal errors, without waiting for a successful turn.
    let response_index = inspect
        .then(|| {
            (app.pet_watch.work_history_start..app.virtual_cell_count()).rfind(|&index| {
                matches!(
                    app.cell_at_virtual_index(index),
                    Some(HistoryCell::Assistant { .. } | HistoryCell::Error { .. })
                )
            })
        })
        .flatten();
    let active = app.active_cell.as_ref();
    let response = response_index.and_then(|index| {
        if index < app.history.len() {
            app.history.get(index)
        } else {
            active.and_then(|cell| cell.entries().get(index - app.history.len()))
        }
    });
    let plan = PetModeAreas::new(
        area,
        inspect && !agents.is_empty(),
        response.is_some(),
        app.pet_watch.full.focus_agents,
    );
    let owner = inputs(app, Instant::now());
    let motion = motion(app);
    // Revisions invalidate the selected cell inside the existing incremental
    // cache. Only source replacement or a destructive history change discards
    // its parser; streaming chunks and ambient frames keep the stable prefix.
    let key = response_index.map(|index| {
        (
            app.transcript_identity_epoch,
            index,
            index >= app.history.len(),
        )
    });
    if inspect && app.pet_watch.full_response_key != key {
        app.pet_watch.full_response = None;
        app.pet_watch.full_response_key = key;
    }
    let mut options = app.transcript_render_options();
    options.low_motion = true;
    options.motion_mode = crate::tui::motion::MotionMode::Reduced;
    let copy_hint = (inspect && app.completed_assistant_output_receipt().is_some()).then(|| {
        tr(app.ui_locale, MessageId::PetModeCopyHint).replace(
            "{copy}",
            shell_key_routing::binding(Id::PetCopyReply).footer_chord,
        )
    });
    let lines = if inspect {
        let revision = response_index.map_or(0, |index| {
            if index < app.history.len() {
                app.history_revisions[index]
            } else {
                active.map_or(0, |cell| cell.revision())
            }
        });
        let revisions = [revision];
        let cache = app
            .pet_watch
            .full_response
            .get_or_insert_with(crate::tui::transcript::TranscriptViewCache::new);
        cache.set_streaming_source_receipt(
            app.streaming_source_receipt
                .filter(|receipt| Some(receipt.cell_index) == response_index),
        );
        cache.ensure_split(
            &[response.map_or(&[], std::slice::from_ref)],
            if response.is_some() { &revisions } else { &[] },
            plan.output.width.max(1),
            options,
            &std::collections::HashMap::new(),
            response_index.as_ref().map(std::slice::from_ref),
            None,
        );
        if let Some(receipt) = app.streaming_source_receipt.as_mut()
            && Some(receipt.cell_index) == response_index
        {
            receipt.from_revision = receipt.to_revision;
        }
        cache.lines()
    } else {
        &[]
    };
    let mut hints = tr(app.ui_locale, MessageId::PetModeMessageHint).replace(
        "{back}",
        shell_key_routing::binding(Id::PetBack).footer_chord,
    );
    if !agents.is_empty() {
        hints.push_str(&format!(
            " · {}",
            tr(app.ui_locale, MessageId::PetModePaneHint).replace(
                "{switch}",
                shell_key_routing::binding(Id::PetFocusAgents).footer_chord,
            )
        ));
    }
    if area.width >= 100 {
        hints.push_str(&format!(" · {}", habitat::companion_hints(app.ui_locale)));
    }
    let arrows = format!(
        "{}{}",
        shell_key_routing::binding(Id::PetResultUp).footer_chord,
        shell_key_routing::binding(Id::PetResultDown).footer_chord,
    );
    let mut pane_hints = if app.pet_watch.full.focus_agents && !agents.is_empty() {
        tr(app.ui_locale, MessageId::PetModeAgentHints)
            .replace("{select}", &arrows)
            .replace(
                "{open}",
                shell_key_routing::binding(Id::PetOpenAgent).footer_chord,
            )
    } else if !lines.is_empty() {
        tr(app.ui_locale, MessageId::PetModeReplyHints)
            .replace("{scroll}", &arrows)
            .replace(
                "{latest}",
                shell_key_routing::binding(Id::PetResultEnd).footer_chord,
            )
    } else {
        String::new()
    };
    if let Some(copy_hint) = &copy_hint {
        pane_hints = if pane_hints.is_empty() {
            copy_hint.clone()
        } else {
            format!("{copy_hint} · {pane_hints}")
        };
    }
    let mut view = PetMode::new(&theme, mark);
    view.dot_whale = prepared.and_then(|r| {
        let (points, materials, hollow) = if motion.animates() {
            (
                &r.scene.points,
                r.scene.materials.as_ref(),
                r.scene.style.hollow,
            )
        } else {
            (
                &r.scene.still.points,
                r.scene.still.materials.as_ref(),
                r.scene.still.style.hollow,
            )
        };
        materials.map(|materials| {
            DotWhale::new(points, materials)
                .caption(&words)
                .hollow(hollow)
        })
    });
    view.notice = notice;
    view.title = "Codewhale".into();
    view.agents = if inspect { &agents } else { &[] };
    view.agent_words = SubagentViewWords {
        title: tr(app.ui_locale, MessageId::ConfigChoiceRailAgents),
        empty: tr(
            app.ui_locale,
            MessageId::SubagentsNoCurrentSessionFleetWorkers,
        ),
        activity: tr(app.ui_locale, MessageId::AutomationStatusLabel),
        output: tr(app.ui_locale, MessageId::AgentFocusResult),
        // The retained roster supplies no event log or copied outcome.
        // Enter opens those through the existing localized transcript view.
        toggle_output: "".into(),
        no_events: "".into(),
        hints: "".into(),
        open: "".into(),
        message: "".into(),
        stop: "".into(),
        summary: Some(
            tr(app.ui_locale, MessageId::SubagentsSummaryItem)
                .replace(
                    "{label}",
                    tr(app.ui_locale, MessageId::CtxInspTotal).as_ref(),
                )
                .replace("{count}", &agents.len().to_string())
                .into(),
        ),
        steps_label: Some(tr(app.ui_locale, MessageId::SessionMetricsSteps)),
    };
    view.output = lines;
    view.output_title = tr(
        app.ui_locale,
        match response {
            Some(HistoryCell::Error { .. }) => MessageId::ExtensionsStateError,
            _ => MessageId::PetModeReplyTitle,
        },
    );
    view.pane_hints = if inspect {
        pane_hints.into()
    } else {
        "".into()
    };
    let inspect_hint = tr(app.ui_locale, MessageId::PetModeInspectHint).replace(
        "{inspect}",
        shell_key_routing::binding(Id::PetInspect).footer_chord,
    );
    view.hints = if inspect {
        hints.into()
    } else {
        inspect_hint.clone().into()
    };
    app.pet_watch
        .full
        .set_visible(inspect || app.view_stack.is_empty());
    app.pet_watch.full.update(
        app.current_session_id.as_deref(),
        owner,
        &agents,
        Instant::now(),
        motion,
    );
    frame.render_stateful_widget(view, area, &mut app.pet_watch.full);
    if let Ok(mut copy_area) = app.pet_watch.copy_area.lock() {
        *copy_area = copy_hint.as_ref().and_then(|hint| {
            let width = codewhale_ratatui::text::width(hint);
            // Match the actual second-row truncation; a hidden/truncated label
            // never leaves a clickable target behind.
            (plan.footer.height > 1 && width + 4 <= usize::from(plan.footer.width))
                .then(|| Rect::new(plan.footer.x, plan.footer.y + 1, width as u16, 1))
        });
    }
    if let Ok(mut selection) = app.pet_watch.selection.lock() {
        *selection = inspect
            .then(|| app.pet_watch.full.selected_agent_id().map(str::to_owned))
            .flatten();
    }
    app.pet_watch.inspect_area = (!inspect).then(|| {
        Rect::new(
            plan.footer.x,
            plan.footer.y,
            (codewhale_ratatui::text::width(inspect_hint.split(" · ").next().unwrap_or_default())
                as u16)
                .min(plan.footer.width),
            plan.footer.height.min(1),
        )
    });
    if canonical(&app.pet_watch) {
        app.pet_watch.area = Some(plan.pet);
    } else {
        // Existing reviewed avatar packs and explicit action/view previews keep
        // their renderer and permissions; only the full-screen layout is shared.
        super::render_tank(frame, plan.pet, app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{active_cell::ActiveCell, app::OnboardingState};
    fn app() -> App {
        let mut app =
            crate::test_support::test_app_with_options(crate::test_support::test_tui_options("."));
        app.onboarding = OnboardingState::None;
        app.redaction_gate = false;
        app.pet_watch.session = app.current_session_id.clone();
        app.pet_watch.detach_for_test();
        app
    }
    fn paint(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render_full(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
    #[test]
    fn native_main_stage_wakes_the_existing_idle_loop_and_stops_when_hidden() {
        let mut app = app();
        app.low_motion = false;
        app.fancy_animations = true;
        app.pet_watch.enabled = true;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal
            .draw(|frame| render_main(frame, frame.area(), &mut app))
            .unwrap();
        let now = Instant::now();
        super::super::tick(&mut app, now);
        assert!(app.pet_watch.next_frame_in(now).is_some());
        app.view_stack
            .push(crate::tui::views::HelpView::new_for_locale(app.ui_locale));
        super::super::tick(&mut app, now + Duration::from_secs(2));
        assert!(
            app.pet_watch
                .next_frame_in(now + Duration::from_secs(2))
                .is_none()
        );
    }
    #[test]
    fn full_mode_animates_under_its_own_modal_and_respects_motion_preferences() {
        let mut app = app();
        app.is_loading = true;
        app.low_motion = false;
        app.fancy_animations = true;
        super::super::open_habitat(&mut app);
        assert!(super::super::is_open(&app));
        assert_eq!(motion(&app), MotionMode::Full);
        paint(&mut app, 100, 32);
        tick(&mut app, true, Instant::now());
        assert!(app.pet_watch.full_next.is_some());
        app.low_motion = true;
        tick(&mut app, true, Instant::now());
        assert!(app.pet_watch.full_next.is_none());
        app.low_motion = false;
        app.fancy_animations = false;
        assert_eq!(motion(&app), MotionMode::Still);
    }
    #[test]
    fn roster_scope_and_missing_vs_zero_receipts_come_from_the_existing_owner_guard() {
        let mut app = app();
        app.current_session_id = Some("foreground".into());
        app.agent_roster_session_id = Some("other-session".into());
        app.agent_roster = vec![crate::agent_roster::AgentRosterRow {
            worker_id: "retained-worker".into(),
            display_name: "Retained worker".into(),
            model: "reported-model".into(),
            state: RosterState::Waiting,
            status: AgentWorkerStatus::WaitingForUser,
            activity: Some("Owner activity".into()),
            outcome: None,
            millis: None,
            input_tokens: Some(0),
            output_tokens: None,
            cost_microusd: Some(1),
            steps_taken: 0,
            parent_run_id: None,
            run_id: "actual-run".into(),
        }];
        assert!(agents(&app).is_empty());
        *app.pet_watch.selection.lock().unwrap() = Some("retained-worker".into());
        tick(&mut app, true, Instant::now());
        assert!(app.pet_watch.selection.lock().unwrap().is_none());
        app.agent_roster_session_id = app.current_session_id.clone();
        let rows = agents(&app);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "retained-worker");
        assert_eq!(rows[0].card.status.state, State::NeedsYou);
        assert_eq!(rows[0].usage.unwrap().input_tokens, Some(0));
        assert_eq!(rows[0].usage.unwrap().output_tokens, None);
        assert_eq!(rows[0].usage.unwrap().cost_microusd, Some(1));
        assert_eq!(rows[0].elapsed, None);
        app.agent_roster[0].state = RosterState::Parked;
        assert_eq!(
            agents(&app)[0].card.status.word,
            tr(app.ui_locale, MessageId::AgentStatusParked)
        );
        app.ui_locale = codewhale_localization::Locale::Fr;
        app.pet_watch.full.focus_agents = true;
        let detail = paint(&mut app, 95, 40);
        assert!(detail.contains(tr(app.ui_locale, MessageId::AutomationStatusLabel).as_ref()));
        assert!(!detail.contains("No events reported"));
        assert!(!detail.contains("Activity"));
    }
    #[test]
    fn pet_reply_reuses_incremental_markdown_and_rejects_unproven_edits() {
        let mut app = app();
        app.is_loading = true;
        let index = crate::tui::ui::ensure_streaming_assistant_history_cell(&mut app);
        let initial: String = (0..120)
            .map(|row| format!("Cached row {row}\n\n"))
            .collect();
        crate::tui::ui::append_streaming_text(&mut app, index, &initial);
        paint(&mut app, 80, 24);
        let work = |app: &App| {
            app.pet_watch
                .full_response
                .as_ref()
                .unwrap()
                .streaming_render_work(0)
                .unwrap()
        };
        assert_eq!(work(&app).invalidations, 1);
        for row in 0..20 {
            let before = work(&app);
            let tail = format!("APPENDED_ROW_{row}");
            crate::tui::ui::append_streaming_text(&mut app, index, &format!("{tail}\n\n"));
            assert!(paint(&mut app, 80, 24).contains(&tail));
            let after = work(&app);
            assert_eq!(after.invalidations, 1);
            assert!(after.classified_lines - before.classified_lines <= 2);
            let receipt = app.streaming_source_receipt.unwrap();
            assert_eq!(receipt.from_revision, receipt.to_revision);
        }
        // Returning to the main view must preserve the inspector's parser.
        let before = work(&app);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render_main(frame, frame.area(), &mut app))
            .unwrap();
        assert_eq!(work(&app), before);
        paint(&mut app, 80, 24);
        assert_eq!(work(&app), before);

        let HistoryCell::Assistant { content, .. } = &mut app.history[index] else {
            panic!("expected the real streaming history cell");
        };
        content.insert_str(0, "REWRITTEN_PREFIX\n\n");
        app.bump_history_cell(index);
        assert!(app.streaming_source_receipt.is_none());
        paint(&mut app, 80, 24);
        assert_eq!(work(&app).invalidations, 2);
        assert!(
            app.pet_watch
                .full_response
                .as_ref()
                .unwrap()
                .lines()
                .iter()
                .any(|line| line.to_string().contains("REWRITTEN_PREFIX"))
        );
        crate::tui::ui::append_streaming_text(&mut app, index, "AFTER_REWRITE\n\n");
        assert!(paint(&mut app, 80, 24).contains("AFTER_REWRITE"));
        assert_eq!(work(&app).invalidations, 2);

        let mut options = app.transcript_render_options();
        options.low_motion = true;
        options.motion_mode = crate::tui::motion::MotionMode::Reduced;
        let width = PetModeAreas::new(Rect::new(0, 0, 80, 24), false, true, false)
            .output
            .width;
        let mut cold = crate::tui::transcript::TranscriptViewCache::new();
        cold.ensure(
            &app.history[index..=index],
            &[app.history_revisions[index]],
            width,
            options,
        );
        assert_eq!(
            app.pet_watch.full_response.as_ref().unwrap().lines(),
            cold.lines()
        );
    }
    #[test]
    fn streamed_tail_follows_until_the_reader_scrolls_back() {
        let mut app = app();
        app.is_loading = true;
        app.runtime_turn_id = Some("streamed-turn".into());
        let mut active = ActiveCell::new();
        active.push_untracked(HistoryCell::Assistant {
            content: (0..30).map(|n| format!("Stream row {n}\n\n")).collect(),
            streaming: true,
        });
        app.active_cell = Some(active);
        assert!(paint(&mut app, 40, 12).contains("Stream row 29"));
        app.pet_watch.full.scroll(&[], -3);
        let top = app.pet_watch.full.output_scroll;
        let Some(HistoryCell::Assistant { content, .. }) =
            app.active_cell.as_mut().unwrap().entry_mut(0)
        else {
            panic!("expected the live assistant response");
        };
        content.push_str("Appended streamed tail\n\n");
        assert!(!paint(&mut app, 40, 12).contains("Appended streamed tail"));
        assert_eq!(app.pet_watch.full.output_scroll, top);
        app.pet_watch.full.scroll_end(&[], true);
        assert!(paint(&mut app, 40, 12).contains("Appended streamed tail"));
    }
    #[test]
    fn full_mode_reads_streaming_response_then_failure_and_preserves_the_session() {
        let mut app = app();
        app.is_loading = true;
        app.input = "retained draft".into();
        let session = app.current_session_id.clone();
        let mut active = ActiveCell::new();
        active.push_untracked(HistoryCell::Assistant {
            content: "Streaming response from this session".into(),
            streaming: true,
        });
        app.active_cell = Some(active);
        assert!(paint(&mut app, 80, 24).contains("Streaming response from this session"));
        app.active_cell = None;
        app.is_loading = false;
        app.runtime_turn_status = Some("failed".into());
        app.add_message(HistoryCell::Error {
            message: "Actual failure receipt".into(),
            severity: crate::error_taxonomy::ErrorSeverity::Error,
        });
        assert!(paint(&mut app, 40, 12).contains("Actual failure receipt"));
        assert_eq!(app.input, "retained draft");
        assert_eq!(app.current_session_id, session);
        assert!(app.pet_watch.worker.is_none());
    }
    #[test]
    fn reduced_completed_view_reuses_the_response_and_does_not_schedule_paints() {
        let mut app = app();
        app.runtime_turn_status = Some("completed".into());
        app.add_message(HistoryCell::Assistant {
            content: "Finished answer".into(),
            streaming: false,
        });
        let first = paint(&mut app, 100, 32);
        let lines = app
            .pet_watch
            .full_response
            .as_ref()
            .unwrap()
            .lines()
            .as_ptr();
        assert_eq!(paint(&mut app, 100, 32), first);
        assert_eq!(
            app.pet_watch
                .full_response
                .as_ref()
                .unwrap()
                .lines()
                .as_ptr(),
            lines
        );
        tick(&mut app, true, Instant::now() + Duration::from_secs(2));
        assert!(app.pet_watch.full_next.is_none());
        tick(&mut app, false, Instant::now() + Duration::from_secs(3));
        assert!(app.pet_watch.full_next.is_none());
    }
    #[test]
    fn pet_copy_footer_tracks_completed_output_and_only_keeps_fully_painted_hitboxes() {
        let mut app = app();
        assert!(!paint(&mut app, 100, 32).contains("copy finished reply"));
        assert!(app.pet_watch.copy_area.lock().unwrap().is_none());
        let index = app.history.len();
        app.add_message(HistoryCell::Assistant {
            content: "finished answer".into(),
            streaming: false,
        });
        app.record_completed_assistant_output(index, "finished answer");
        app.add_message(HistoryCell::Assistant {
            content: "current partial reply".into(),
            streaming: true,
        });
        assert!(paint(&mut app, 100, 32).contains("c copy finished reply"));
        assert!(app.pet_watch.copy_area.lock().unwrap().is_some());
        paint(&mut app, 16, 8);
        assert!(
            app.pet_watch.copy_area.lock().unwrap().is_none(),
            "truncated labels have no invisible click area"
        );
        paint(&mut app, 100, 32);
        app.pet_watch.prepare_frame();
        assert!(
            app.pet_watch.copy_area.lock().unwrap().is_none(),
            "another focus owner clears old targets before painting"
        );
        paint(&mut app, 100, 32);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal
            .draw(|frame| render_main(frame, frame.area(), &mut app))
            .unwrap();
        assert!(
            app.pet_watch.copy_area.lock().unwrap().is_none(),
            "the composer backdrop does not expose an inspector action"
        );
    }
}
