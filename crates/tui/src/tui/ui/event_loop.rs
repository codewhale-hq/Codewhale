//! The TUI event loops.
//!
//! Moved verbatim out of `ui.rs`, which had grown past 19k lines. `run_tui`
//! owns terminal setup and teardown; `run_event_loop` is the frame, input, and
//! engine-event pump it drives.

use super::clamp_event_poll_timeout;
use super::observer_hooks::{
    execute_session_error_hook, execute_session_state_transition_hooks,
    execute_turn_end_observer_hook, surface_observer_hook_submission_failure,
};
use super::task_projection::{
    AUTOMATION_SCAN_BUSY_INTERVAL, automation_scan_is_due, refresh_active_task_panel,
    refresh_automation_panel, refresh_automation_panel_blocking, refresh_shell_exec_live_output,
};
use super::*;
use crate::tui::shell_key_routing::ShellBindingId;
use codewhale_models::Role;

use crate::tui::control_socket::SessionControl;

type SkillCacheRefresh = (
    crate::tui::app::SkillCacheScope,
    tokio::task::JoinHandle<Vec<(String, String)>>,
);

pub(super) fn event_owner_is_active(
    current_session_id: Option<&str>,
    owner_session_id: &str,
) -> bool {
    !owner_session_id.is_empty() && current_session_id == Some(owner_session_id)
}

/// Apply only the projection owned by this host session. A delayed SetModel
/// receipt from the previous session cannot replace the current transcript.
pub(super) fn apply_engine_session_projection(
    app: &mut App,
    config: &Config,
    event: EngineEvent,
) -> bool {
    let EngineEvent::SessionUpdated {
        session_id,
        messages,
        system_prompt,
        model,
        workspace,
    } = event
    else {
        return false;
    };
    // SetModel can emit the old session while a host-owned
    // SyncSession is still queued. Reject that entire stale
    // projection before changing transcript or persistence.
    if !event_owner_is_active(app.current_session_id.as_deref(), &session_id) {
        tracing::debug!(
            expected = ?app.current_session_id,
            received = %session_id,
            "ignoring stale engine session projection"
        );
        return false;
    }
    if app.last_known_goal_state.is_some()
        && let Err(error) = persist_current_session_goal(app)
    {
        surface_goal_persistence_failure(app, &error);
    }
    app.context_token_cache.borrow_mut().clear();
    app.set_api_messages(messages);
    // #6190: the projection is the engine's own record, so it is where a
    // steer's acceptance becomes observable — and the only place the steer's
    // real message index is known. Promote before anything else reads the
    // transcript, so live order equals record order by construction.
    crate::tui::ui::dispatch::settle_accepted_steers(app);
    app.system_prompt = system_prompt;
    if app.auto_model {
        app.last_effective_model = Some(model);
    } else {
        app.set_model_selection(model);
    }
    app.update_model_compaction_budget();
    if app.workspace != workspace {
        apply_workspace_runtime_state(app, config, workspace);
    }
    if (app.is_loading || app.is_compacting || app.is_purging)
        && let Ok(manager) = SessionManager::default_location()
    {
        if let Ok(session) = build_session_snapshot(app, &manager) {
            app.session_title = Some(session.metadata.title.clone());
            // The engine's session id was pinned above, so
            // every checkpoint of this session lands in the
            // same per-session file.
            if let Err(err) =
                persist_with_pending_work_boundary(app, PersistRequest::SaveCheckpoint { session })
            {
                app.status_message = Some(format!(
                    "To-do list update pending: checkpoint could not be queued ({err})"
                ));
            }
        }
    } else if app.session_title.is_none() {
        // Never synchronously reload the growing session
        // JSON on the event-loop task just to recover a
        // title. The in-memory metadata cache is authoritative.
        let cached = app
            .current_session_metadata
            .as_ref()
            .filter(|metadata| metadata.id == session_id)
            .map(|metadata| metadata.title.clone());
        app.session_title = cached.or_else(|| derive_session_title(&app.api_messages));
    }
    true
}

fn current_session_fleet_workers_status(
    locale: codewhale_localization::Locale,
    count: usize,
) -> String {
    codewhale_localization::tr(
        locale,
        codewhale_localization::MessageId::SubagentsCurrentSessionFleetWorkersStatus,
    )
    .replace("{count}", &count.to_string())
}

/// A turn is unsettled until its authoritative terminal event lands. A local
/// cancel clears `is_loading` at once, but the engine's `TurnComplete` is still
/// owed: until it arrives the recovery checkpoint is the only complete record
/// of that turn, so shutdown must not declare the session settled (U02-09).
fn turn_unsettled_for_shutdown(app: &App) -> bool {
    app.is_loading || app.dispatch_in_flight || app.suppress_stream_events_until_turn_complete
}

/// Host state can change without a model turn, including learning the Runtime
/// binding of a resumed legacy session. Commit that state before clearing its
/// recovery checkpoint; an unfinished turn keeps its checkpoint untouched.
pub(super) fn persist_settled_session_on_shutdown(
    app: &mut App,
    handle: &persistence_actor::PersistActorHandle,
) -> Result<bool, String> {
    if turn_unsettled_for_shutdown(app) || app.current_session_id.is_none() {
        return Ok(false);
    }
    let manager = SessionManager::default_location().map_err(|error| error.to_string())?;
    let session = build_session_snapshot(app, &manager)?;
    if !handle.try_send(PersistRequest::CompletedCommit { session }) {
        return Err("persistence actor is unavailable during shutdown".into());
    }
    Ok(true)
}

#[derive(Debug)]
struct TranslationAccountingContext {
    cost_scope: crate::cost_status::CostScopeToken,
    origin_session_id: Option<String>,
    origin_turn_id: Option<String>,
    source_id: String,
}

struct SettledTranslation {
    translated: anyhow::Result<String>,
    usage: Option<codewhale_models::Usage>,
}

impl TranslationAccountingContext {
    fn capture(app: &App, kind: &str, sequence: u64) -> Self {
        let raw_source = format!(
            "translation:{}:{}:{kind}:{sequence}",
            app.current_session_id.as_deref().unwrap_or("no-session"),
            app.runtime_turn_id.as_deref().unwrap_or("no-turn")
        );
        Self {
            cost_scope: crate::cost_status::scope_token(),
            origin_session_id: app.current_session_id.clone(),
            origin_turn_id: app.runtime_turn_id.clone(),
            source_id: format!(
                "translation:{}",
                crate::cost_status::usage_source_fingerprint(&raw_source)
            ),
        }
    }

    fn settle(
        self,
        response: anyhow::Result<crate::client::TranslationProviderResponse>,
    ) -> SettledTranslation {
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                return SettledTranslation {
                    translated: Err(error),
                    usage: None,
                };
            }
        };
        if let Some(usage) = response.usage.as_ref() {
            if let (Some(session_id), Some(turn_id)) = (
                self.origin_session_id.as_deref(),
                self.origin_turn_id.as_deref(),
            ) {
                crate::cost_status::report_effective_route_for_interactive_origin(
                    self.cost_scope,
                    session_id,
                    turn_id,
                    &self.source_id,
                    &response.route,
                    usage,
                );
            } else {
                crate::cost_status::report_effective_route_for_runtime(
                    self.cost_scope,
                    None,
                    &self.source_id,
                    &response.route,
                    usage,
                );
            }
        } else {
            if let (Some(session_id), Some(turn_id)) = (
                self.origin_session_id.as_deref(),
                self.origin_turn_id.as_deref(),
            ) {
                crate::cost_status::report_unreceipted_for_interactive_origin(
                    self.cost_scope,
                    session_id,
                    turn_id,
                    &self.source_id,
                    &response.route,
                );
            } else {
                crate::cost_status::report_unreceipted_provider_success(
                    self.cost_scope,
                    None,
                    &self.source_id,
                    &response.route,
                );
            }
        }
        SettledTranslation {
            translated: response.translated,
            usage: response.usage,
        }
    }
}

fn accrue_translation_usage(app: &mut App, usage: &codewhale_models::Usage) {
    let turn_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    app.session.total_tokens = app.session.total_tokens.saturating_add(turn_tokens);
    app.session.total_conversation_tokens = app
        .session
        .total_conversation_tokens
        .saturating_add(turn_tokens);
    app.session.total_input_tokens = app
        .session
        .total_input_tokens
        .saturating_add(usage.input_tokens);
    app.session.total_output_tokens = app
        .session
        .total_output_tokens
        .saturating_add(usage.output_tokens);
    if usage.prompt_cache_hit_tokens.is_some()
        || usage.prompt_cache_miss_tokens.is_some()
        || usage.prompt_cache_write_tokens.is_some()
    {
        let classes = crate::pricing::token_usage_for_pricing(usage);
        app.session.total_cache_hit_tokens = app
            .session
            .total_cache_hit_tokens
            .saturating_add(u32::try_from(classes.cache_read).unwrap_or(u32::MAX));
        app.session.total_cache_miss_tokens = app
            .session
            .total_cache_miss_tokens
            .saturating_add(u32::try_from(classes.input).unwrap_or(u32::MAX));
        app.session.total_cache_write_tokens = app
            .session
            .total_cache_write_tokens
            .saturating_add(u32::try_from(classes.cache_write).unwrap_or(u32::MAX));
    }
}

fn translation_origin(app: &App) -> (Option<String>, Option<String>) {
    // Fixed-size one-way identities avoid retaining raw imported ids in a
    // detached completion envelope without introducing truncation aliases.
    let fingerprint = |value: Option<&str>| value.map(crate::cost_status::usage_source_fingerprint);
    (
        fingerprint(app.current_session_id.as_deref()),
        fingerprint(app.runtime_turn_id.as_deref()),
    )
}

fn translation_origin_is_current(
    app: &App,
    origin_session_fingerprint: Option<&str>,
    origin_turn_fingerprint: Option<&str>,
) -> bool {
    let current = translation_origin(app);
    current.0.as_deref() == origin_session_fingerprint
        && current.1.as_deref() == origin_turn_fingerprint
}

fn translation_session_is_current(app: &App, origin_session_fingerprint: Option<&str>) -> bool {
    translation_origin(app).0.as_deref() == origin_session_fingerprint
}

fn exact_translation_client(
    config: &Config,
    route: &crate::core::events::TurnRoute,
) -> anyhow::Result<Arc<CodewhaleClient>> {
    let identity = config
        .resolve_persisted_provider_identity(
            Some(route.provider.as_str()),
            Some(&route.provider_identity),
        )
        .map_err(anyhow::Error::msg)?;
    let validated = crate::route_runtime::resolve_runtime_route_for_identity(
        config,
        &identity,
        Some(&route.model),
    )
    .map_err(anyhow::Error::msg)?
    .validate()
    .map_err(anyhow::Error::msg)?;
    if validated.identity.key.as_str() != route.provider_identity
        || validated.model != route.model
        || validated.candidate.endpoint().base_url != route.base_url
    {
        anyhow::bail!(
            "translation route changed after turn dispatch; refusing to reuse a different provider client"
        );
    }
    if let Some(receipt) = route.receipt.as_ref()
        && &validated.client.turn_route_receipt() != receipt
    {
        anyhow::bail!(
            "translation credential or endpoint changed after turn dispatch; refusing stale completion ownership"
        );
    }
    Ok(Arc::new(validated.client))
}

/// Bind the Runtime thread store to a session before the process-owner lock
/// is taken, so a second Codewhale on the same machine does not collide on
/// the default root (#5630). This id is only the initial store anchor; saved
/// metadata retains the actual store binding when launch creates a new id.
pub(crate) fn ensure_runtime_session_id(app: &mut App) -> String {
    if let Some(existing) = app
        .current_session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return existing.to_string();
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    app.current_session_id = Some(session_id.clone());
    session_id
}

/// How long a startup-screen submit waits for the engine to install the
/// session it just began before dispatching the input anyway.
const LAUNCH_SESSION_SYNC_WAIT: Duration = Duration::from_secs(2);

/// Wait until the engine has processed every operation queued so far.
///
/// The App mints a session id and tells the engine with `Op::SyncSession`,
/// which the engine installs later, on its own task. Until then the two
/// disagree, and anything the App resolves by session id against engine-owned
/// state misses: the extension host identifies this caller by the engine's
/// session id, so a plugin command selected for the caller read as unknown.
/// The operation channel is FIFO, so the snapshot reply proves the sync ahead
/// of it was applied. Bounded: an engine that does not answer in time costs
/// only the old behaviour, never a stuck composer.
pub(crate) async fn await_engine_session_sync(engine_handle: &EngineHandle) {
    let _ = tokio::time::timeout(
        LAUNCH_SESSION_SYNC_WAIT,
        engine_handle.get_session_snapshot(),
    )
    .await;
}

fn persist_current_session_goal(app: &App) -> Result<(), String> {
    let session_id = app
        .current_session_id
        .as_deref()
        .ok_or_else(|| "session id is not established".to_string())?;
    let manager = SessionManager::default_location()
        .map_err(|error| format!("could not open the session store: {error}"))?;
    manager
        .save_session_goal(session_id, app.last_known_goal_state.as_ref())
        .map_err(|error| error.to_string())
}

pub(crate) fn surface_goal_persistence_failure(app: &mut App, error: &str) {
    app.push_status_toast(
        format!("Goal progress is not durable yet: {error}"),
        StatusToastLevel::Warning,
        None,
    );
}

/// Apply Space only to the owner stored by the final render pass.
pub(super) fn handle_transcript_space(app: &mut App) -> bool {
    let Some((owner, fold_target)) = app.viewport.transcript_cache.take_transcript_action() else {
        return false;
    };
    let idx = owner.cell_index;
    if owner.identity_epoch != app.transcript_identity_epoch {
        return false;
    }
    let Some(cell) = app.cell_at_virtual_index(idx) else {
        return false;
    };
    let is_thinking = matches!(cell, HistoryCell::Thinking { .. });
    let selected_first_line = app
        .viewport
        .transcript_selection
        .ordered_endpoints()
        .filter(|(start, _)| {
            app.viewport
                .transcript_cache
                .line_meta()
                .get(start.line_index)
                .and_then(|meta| meta.cell_line())
                .is_some_and(|(rendered, _)| app.original_cell_index_for_rendered(rendered) == idx)
        })
        .and_then(|_| {
            app.viewport
                .transcript_cache
                .line_meta()
                .iter()
                .position(|meta| {
                    meta.cell_line().is_some_and(|(rendered, _)| {
                        app.original_cell_index_for_rendered(rendered) == idx
                    })
                })
        });
    if let Some(target) = fold_target.filter(|_| !app.collapsed_cells.contains(&idx)) {
        if target.owner != owner {
            return false;
        }
        if is_thinking && !app.show_thinking {
            return false;
        }
        // The rendered action names the state the user is asking for, so
        // record that outright. A relative bit would be re-read as its
        // opposite the next time a display preference changed (#5847).
        let intent = match target.action {
            CellFoldAction::Expand => TranscriptFold::Expanded,
            CellFoldAction::Collapse => TranscriptFold::Collapsed,
        };
        app.cell_folds.insert(idx, intent);
    } else if app.toggle_tool_run_expansion_at(idx) {
        return true;
    } else if !app.collapsed_cells.remove(&idx) {
        if is_thinking {
            return false;
        }
        app.cell_folds.insert(idx, TranscriptFold::Collapsed);
    }
    if let Some(line_index) = selected_first_line {
        // A middle-body row may disappear or become another cell after the
        // fold. Keep the selected owner at its stable first row (#6876).
        let point = crate::tui::selection::TranscriptSelectionPoint {
            line_index,
            column: 0,
        };
        app.viewport.transcript_selection.clear();
        app.viewport.transcript_selection.anchor = Some(point);
        app.viewport.transcript_selection.head = Some(point);
    }
    app.mark_history_updated();
    true
}

/// Route plain input that must be decided before the composer sees it.
///
/// The raw-paste fallback intentionally holds the first ASCII character for
/// a few milliseconds. Space must use that same ambiguity window: a second
/// rapid character proves it was paste payload, while a lone held Space can
/// become the rendered transcript action when the hold expires.
pub(super) fn handle_plain_key_before_composer(
    app: &mut App,
    key: &KeyEvent,
    now: Instant,
) -> bool {
    crate::tui::paste::handle_paste_burst_key(app, key, now)
}

/// Flush a raw-paste ambiguity window without losing a leading Space.
///
/// `FlushResult::Paste` is always composer payload. A lone typed Space is a
/// transcript action only when the composer is still empty and the last
/// rendered owner accepts it; otherwise it remains ordinary input.
pub(super) fn flush_paste_burst_before_composer(app: &mut App, now: Instant) -> bool {
    if !app.view_stack.is_empty() {
        // One grammar buffer: a modal owns keys. Held burst must not leak
        // into the composer (leaky `/model` after the picker opens).
        app.paste_burst.clear_after_explicit_paste();
        return false;
    }
    match app.take_paste_burst_flush_if_enabled(now) {
        crate::tui::paste_burst::FlushResult::Paste(text) => {
            // Terminals without bracketed paste deliver a dropped file the
            // same way; attach it exactly as `insert_paste_text` would.
            if !app.attach_pasted_image_paths(&text) {
                app.insert_str(&text);
            }
            true
        }
        crate::tui::paste_burst::FlushResult::Typed(' ')
            if app.input.is_empty() && handle_transcript_space(app) =>
        {
            true
        }
        crate::tui::paste_burst::FlushResult::Typed(ch) => {
            app.insert_char(ch);
            true
        }
        crate::tui::paste_burst::FlushResult::SuppressionExpired => {
            app.needs_redraw = true;
            true
        }
        crate::tui::paste_burst::FlushResult::None => false,
    }
}

/// The shell's key admission, asked exactly as the event loop asks it: which
/// binding does this key press, for whoever owns the keyboard right now?
///
/// Every seam below calls this instead of re-deriving focus from
/// `view_stack`, `launch.visible`, or — the bug this replaces — whether the
/// composer happens to hold text.
pub(crate) fn shell_binding_for_key(app: &App, key: &KeyEvent) -> Option<ShellBindingId> {
    crate::tui::shell_key_routing::route(app.focus(), key)
}

/// What pressing Tab did — see [`dispatch_tab_key`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TabDispatch {
    /// One of the composer's own completions consumed the key.
    Completion,
    /// Nobody owns Tab in this focus state.
    Ignored,
    /// The session mode cycled. The caller syncs the engine.
    ModeCycled {
        prior_mode: AppMode,
        prior_model: String,
    },
}

/// Tab dispatch, lifted out of the event loop body so a test can press Tab.
///
/// The composer's completions get the key first: a mention menu, a slash
/// menu, an in-progress command or file mention, a waiting prompt
/// suggestion. Those are genuine composer *editing* questions about the
/// text. Once none of them claims the key, Tab is the shell's mode cycle,
/// admitted by [`App::focus`] alone — whether the composer holds text is not
/// part of that decision. It used to be: `if !app.input.is_empty()
/// { continue; }` killed Tab the moment the user typed anything.
pub(crate) fn dispatch_tab_key(
    app: &mut App,
    key: &KeyEvent,
    mention_menu_entries: &[String],
    slash_menu_entries: &[crate::tui::widgets::SlashMenuEntry],
) -> TabDispatch {
    if !mention_menu_entries.is_empty()
        && crate::tui::file_mention::apply_mention_menu_selection(app, mention_menu_entries)
    {
        return TabDispatch::Completion;
    }
    if !slash_menu_entries.is_empty() && apply_slash_menu_selection(app, slash_menu_entries, true) {
        return TabDispatch::Completion;
    }
    if try_autocomplete_slash_command(app) {
        return TabDispatch::Completion;
    }
    if crate::tui::file_mention::try_autocomplete_file_mention(app) {
        return TabDispatch::Completion;
    }
    if app.input.is_empty()
        && let Some(suggestion) = app.prompt_suggestion.take()
    {
        app.input = suggestion;
        app.cursor_position = app.input.chars().count();
        app.needs_redraw = true;
        return TabDispatch::Completion;
    }
    if shell_binding_for_key(app, key) != Some(ShellBindingId::ModeCycle) {
        return TabDispatch::Ignored;
    }
    // Sending or queueing input is reserved for Enter, so Tab never changes
    // roles based on whether a turn happens to be running.
    let prior_model = app.model.clone();
    let prior_mode = app.mode;
    app.cycle_mode();
    app.note_footer_hint_used(crate::tui::footer_hints::MODE_CYCLE);
    TabDispatch::ModeCycled {
        prior_mode,
        prior_model,
    }
}

/// Whether a mouse event is a wheel/trackpad scroll in any direction.
fn is_scroll_event(mouse: &crossterm::event::MouseEvent) -> bool {
    matches!(
        mouse.kind,
        crossterm::event::MouseEventKind::ScrollUp
            | crossterm::event::MouseEventKind::ScrollDown
            | crossterm::event::MouseEventKind::ScrollLeft
            | crossterm::event::MouseEventKind::ScrollRight
    )
}

/// Bound on how many scroll events one gesture may fold into a single frame,
/// so a stuck wheel cannot starve the draw.
const MAX_COALESCED_SCROLLS: usize = 64;

/// Apply every queued scroll event of the current gesture except the last,
/// and return that last one for the caller to handle normally.
///
/// A trackpad emits a burst of scroll events. Handling them one per loop
/// iteration meant one frame each, and the frame limiter then spaced those
/// frames out, so the scroll arrived as a slow crawl long after the fingers
/// stopped. Resize events have been coalesced this way since #65; scroll
/// never was. The scroll handlers only accumulate into
/// `viewport.pending_scroll_delta`, so folding the burst in costs one cheap
/// call each and exactly one draw for the whole gesture.
///
/// A non-scroll event ends the burst and is pushed back unread.
pub(crate) fn coalesce_scroll_burst(
    app: &mut App,
    first: crossterm::event::MouseEvent,
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
) -> std::io::Result<crossterm::event::MouseEvent> {
    if !is_scroll_event(&first) {
        return Ok(first);
    }
    let mut latest = first;
    for _ in 0..MAX_COALESCED_SCROLLS {
        let Some(next_observed) = try_next_terminal_event(input, pending)? else {
            break;
        };
        match &next_observed.event {
            Event::Mouse(next) if is_scroll_event(next) => {
                let _ = handle_mouse_event(app, latest);
                latest = *next;
            }
            _ => {
                // Back to the head, not the tail: `pending` may already hold
                // later input (typed text, Enter), and this event came first.
                pending.push_front(next_observed);
                break;
            }
        }
    }
    Ok(latest)
}

/// Wheel input and scrollbar dragging need the same cadence as text selection.
pub(crate) fn transcript_cadence_tier(
    app: &App,
    has_running_agents: bool,
) -> crate::tui::display_refresh::DrawCadenceTier {
    crate::tui::display_refresh::cadence_tier_from_signals(
        app.is_loading || has_running_agents,
        app.viewport.transcript_selection.is_active()
            || app.viewport.pending_scroll_delta != 0
            || app.viewport.transcript_scrollbar_dragging,
        !app.input.is_empty(),
        crate::tui::hover_layer::current_hover().is_some(),
    )
}

/// Fold every queued `Resize` behind the one in hand into the final size, so
/// one clear and redraw serves the whole drag (#65).
///
/// The first non-resize event ends the fold and goes back to the head of the
/// queue, ahead of any later input already drained into `pending`.
pub(crate) fn coalesce_resize_burst(
    width: u16,
    height: u16,
    input: &TerminalInputPump,
    pending: &mut VecDeque<ObservedTerminalEvent>,
) -> std::io::Result<(u16, u16)> {
    let (mut final_w, mut final_h) = (width, height);
    while let Some(next_observed) = try_next_terminal_event(input, pending)? {
        if let Event::Resize(w, h) = next_observed.event {
            final_w = w;
            final_h = h;
        } else {
            pending.push_front(next_observed);
            break;
        }
    }
    Ok((final_w, final_h))
}

/// Toast identity for a failing session save, so recovery can retire it.
const SESSION_SAVE_FAILURE_TOAST: &str = "session-save-failure";

/// Keep the save-failure notice in step with the persistence actor's session
/// save health. Saves run off the UI thread, so a full disk or an unwritable
/// sessions directory used to reach only the log while the user kept working
/// unsaved. The notice stays while a session's latest save is failing and is
/// withdrawn once a later save lands.
pub(crate) fn surface_session_save_health(
    app: &mut App,
    reading: Option<crate::tui::persistence_actor::SaveHealthReading>,
    seen: &mut u64,
) {
    let Some(reading) = reading.filter(|reading| reading.generation != *seen) else {
        return;
    };
    *seen = reading.generation;
    app.retire_event_notices(SESSION_SAVE_FAILURE_TOAST);
    if let Some((session_id, kind)) = reading.failing {
        let text = app
            .tr(MessageId::SessionSaveFailed)
            .replace("{id}", crate::session_manager::truncate_id(&session_id))
            .replace("{error}", &kind.to_string());
        // Standing, not timed: it must outlast the failure, and only the
        // recovery reading above withdraws it.
        app.push_status_toast_record(StatusToast::standing(
            text,
            StatusToastLevel::Error,
            SESSION_SAVE_FAILURE_TOAST,
        ));
    }
}

/// The exit line when a session's latest save failed and was never replaced
/// by a successful one.
pub(crate) fn shutdown_persistence_notice(
    locale: codewhale_localization::Locale,
    reading: &crate::tui::persistence_actor::SaveHealthReading,
) -> Option<String> {
    reading.failing.as_ref().map(|(session_id, kind)| {
        codewhale_localization::tr(locale, MessageId::SessionSaveFailedAtExit)
            .replace("{id}", session_id)
            .replace("{error}", &kind.to_string())
    })
}

/// Run the interactive TUI event loop.
///
/// # Examples
///
/// ```ignore
/// # use crate::config::Config;
/// # use crate::tui::TuiOptions;
/// # async fn example(config: &Config, options: TuiOptions) -> anyhow::Result<()> {
/// crate::tui::run_tui(config, options).await
/// # }
/// ```
pub async fn run_tui(
    config: &Config,
    options: TuiOptions,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
    pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
) -> Result<()> {
    // Install notification, sound, category, and attention policy before any
    // producer (including the model-facing notify tool) can emit an event.
    let _ = crate::tui::notifications::settings(config);
    let startup_screen_mode = options.screen_mode;
    let use_alt_screen = startup_screen_mode.uses_alt_screen();
    let use_mouse_capture = options.use_mouse_capture;
    let use_bracketed_paste = options.use_bracketed_paste;

    // Apply OSC 8 hyperlink toggle from config.
    //
    // #3029: OSC 8 hyperlinks are emitted out-of-band. Markdown wrapping keeps
    // visible spans and per-line targets in separate structures; each render
    // seam translates those targets into absolute `LinkRegion`s without ever
    // placing an escape byte in a ratatui buffer cell. `ColorCompatBackend`
    // then emits the OSC 8 escapes through its `Write` impl around the matching
    // cell runs. Hyperlinks are on by default for terminals that handle the OSC
    // terminator (`ESC \`) cleanly. Windows legacy consoles (conhost) still
    // mishandle the terminator, so the default stays off there; opt in via
    // `[tui] osc8_links = true` on any platform.
    let osc8_default_on = !cfg!(target_os = "windows");
    crate::tui::osc8::set_enabled(
        config
            .tui
            .as_ref()
            .and_then(|tui| tui.osc8_links)
            .unwrap_or(osc8_default_on),
    );

    // Fail fast with a clear message when the interactive TUI is launched
    // without a controlling TTY (#4716). Without this, enable_raw_mode fails
    // with opaque "Device not configured" / "Input/output error" and some
    // terminal hosts surface only "[Process completed]".
    require_interactive_terminal(io::stdin().is_terminal(), io::stdout().is_terminal())?;
    require_foreground_terminal_owner()?;

    // The dispatcher resets SIGPIPE to SIG_DFL so `codewhale doctor | head`
    // exits quietly (#4030). A full-screen session is the opposite case: it
    // writes to pipes whose far end it does not own — stdio MCP servers, shell
    // tools, hooks, LSP — and a peer that exits first must surface as an
    // `EPIPE` error on that one write, not kill the whole TUI with the terminal
    // left in raw mode and nothing in the runtime log. Reproduced with a stdio
    // MCP server that exits before `initialize` is written: the process died
    // of SIGPIPE before its first frame, and the PTY harness reported it as a
    // plain exit 1. Children are unaffected: the standard library resets
    // SIGPIPE to SIG_DFL before exec, so `| head` inside a shell tool still
    // terminates the way a shell expects. Non-TUI subcommands keep SIG_DFL.
    // SAFETY: a plain disposition change, no handler; it runs before this
    // session spawns anything that writes to a pipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // #6169: install the suspend/resume handshake here — after the
    // foreground-ownership check (the termios snapshot needs the still-cooked
    // tty) and before raw mode, so every mode enabled below has a handler that
    // can undo it. Not in `lib.rs`: this must not run for the non-TUI
    // subcommands.
    job_control_guard::install_job_control_guard();

    // This sets local terminal attributes; it is not a terminal-response probe.
    // Do it on the owning thread, as on resume, so blocking-pool scheduling
    // cannot abort startup or leave a detached worker enabling raw mode later.
    enable_raw_mode().context("Failed to enable raw mode")?;

    #[cfg(target_os = "windows")]
    enable_windows_ime_console_mode();

    let mut stdout = io::stdout();
    // Initialize the file-backed TUI log and redirect raw stderr away from
    // the alt-screen for the lifetime of this guard. MUST run BEFORE
    // EnterAlternateScreen; otherwise logging between alt-screen entry and
    // redirect init leaks raw bytes into the TUI buffer, causing the "scroll
    // demon" on Windows (#1909) and garbled output on all platforms (#1085).
    // The guard is held until the function returns; dropping it after
    // LeaveAlternateScreen restores the original stderr handle/fd so shutdown
    // messages reach the user's terminal. We accept the init failing (e.g.,
    // read-only $HOME) and continue without the redirect rather than refusing
    // to start the TUI.
    let _tui_log_guard = match crate::runtime_log::init() {
        Ok(guard) => Some(guard),
        Err(err) => {
            tracing::warn!(target: "runtime_log", ?err, "TUI log init failed; stderr leaks may render as scroll-demon");
            None
        }
    };
    if use_alt_screen {
        enter_alt_screen(&mut stdout)?;
        // Windows also suppresses Codewhale's own verbose CLI logger while
        // the alt-screen is active. The stderr redirect above catches raw
        // writes; this prevents the known verbose source at the origin.
        #[cfg(windows)]
        crate::logging::snapshot_verbose_state();
        #[cfg(windows)]
        crate::logging::set_verbose(false);
    }
    // Mouse capture, bracketed paste, focus events, and the Kitty
    // keyboard-protocol escape-disambiguation flag (#442). Single source
    // of truth shared with the FocusGained recovery path and
    // resume_terminal — see recover_terminal_modes.
    //
    // Focus events are necessary for IME compositor re-activation on
    // macOS when the user switches away (Cmd+Tab) and returns. The Kitty
    // keyboard protocol opt-in is best-effort: terminals that don't
    // support it (iTerm2, Terminal.app, Windows 10 conhost) silently
    // discard the escape, while supporting terminals (Kitty, Ghostty,
    // Alacritty 0.13+, WezTerm, recent Konsole, recent xterm) report
    // unambiguous events for Option/Alt-modified keys and plain Esc.
    //
    // Only `DISAMBIGUATE_ESCAPE_CODES` is pushed — the higher tiers
    // (`REPORT_EVENT_TYPES`, `REPORT_ALL_KEYS_AS_ESCAPE_CODES`) emit
    // release events that the existing key handlers would mis-route
    // as duplicate presses.
    //
    // On Windows, crossterm's `PushKeyboardEnhancementFlags` command always
    // reports the terminal as unsupported (`is_ansi_code_supported` returns
    // false), so the escape is written directly instead. VSCode's integrated
    // terminal and Windows Terminal ≥1.17 honour the kitty keyboard protocol
    // and will correctly disambiguate Shift+Enter from plain Enter once this
    // sequence is received. Terminals that do not understand it silently
    // ignore it.
    recover_terminal_modes(&mut stdout, use_mouse_capture, use_bracketed_paste);
    // The guard reads the *live* screen and disables capture unconditionally,
    // so a runtime `/inline` or `/fullscreen` switch cannot leave it emitting
    // the wrong teardown escape.
    let mut cleanup_guard = TerminalCleanupGuard {
        use_bracketed_paste,
        defused: false,
    };
    let color_depth = palette::ColorDepth::detect();
    // Raw mode is on and the event loop has not started, which is the only
    // window where the OSC 11 background query is safe to issue — see
    // `palette::probe_terminal_background`. The result is cached process-wide,
    // so every later `PaletteMode::detect()` sees the same answer.
    let background = palette::probe_terminal_background();
    // Same window, same reason: the kitty graphics capability query answers
    // on stdin, so it is asked before the input pump exists.
    let kitty_graphics = crate::tui::mark::probe_kitty_graphics();
    // Same window again: the sixel probe is a primary-DA query whose reply
    // also arrives on stdin. Keep both capability receipts before input starts.
    let sixel_graphics = crate::tui::mark::probe_sixel_graphics();
    let palette_mode = background.mode();
    tracing::debug!(
        ?color_depth,
        ?palette_mode,
        background_source = ?background.source(),
        background_color = ?background.color(),
        kitty_graphics,
        sixel_graphics,
        "terminal color profile detected"
    );
    let mut backend = ColorCompatBackend::new(stdout, color_depth, palette_mode);
    backend.set_detected_background(background.color());
    let mut terminal = build_app_terminal(backend, startup_screen_mode)?;
    // At this point Settings hasn't loaded yet, so we can't read the
    // user's `synchronized_output` knob. Use the same env-based terminal
    // quirk detection that `Settings::apply_env_overrides` uses, so the
    // startup viewport reset matches what every later draw will do on
    // flicker-sensitive hosts. A user who has explicitly set
    // `synchronized_output = "on"` to override detection will get sync wrap
    // from the main draw loop onward; the one-time startup viewport reset
    // stays opt-out for them, which is the safe default because the cost is
    // at most brief tearing on the first frame.
    let sync_output_at_init = !crate::settings::detected_ptyxis_terminal()
        && !crate::settings::detected_legacy_windows_console_host();
    reset_terminal_viewport(&mut terminal, sync_output_at_init)?;
    let event_broker = EventBroker::new();

    // Local mutable copy so runtime config flips (e.g. `/provider` switch)
    // can rebuild the API client without restarting the process.
    let mut config = config.clone();
    let config = &mut config;
    let mut app = App::new_with_plugin_registry(options.clone(), config, plugin_registry);
    let _cursor_accent_guard = crate::tui::cursor_accent::CursorAccentGuard::install(
        app.low_motion || !app.fancy_animations,
        app.ui_theme.accent_primary,
    );
    crate::startup_trace::mark("app_constructed");
    sync_config_provider_from_app(config, &app);
    if let Err(error) = crate::tui::setup::record_configured_route(&app).await {
        app.push_status_toast(
            format!(
                "{} · {}: {error}",
                app.tr(MessageId::SetupStepProviderModelTitle),
                app.tr(MessageId::SetupStatusFailed),
            ),
            StatusToastLevel::Error,
            Some(App::STICKY_ERROR_TTL_MS),
        );
    }
    surface_prompt_override_notices(&mut app);

    if options.resume_session_id.is_none() && !app.launch.visible {
        // The one-time Fleet intro is no longer a launch push: it appears the
        // first time the user opens `/fleet` or enters Operate (apply.rs).
        let _ = open_setup_checkpoint_if_due(&mut app, config, options.skip_onboarding);
    }

    // Load existing session if resuming.
    if let Some(ref session_id) = options.resume_session_id
        && let Ok(manager) = SessionManager::default_location()
    {
        // Try to load by prefix or full ID
        let load_result: std::io::Result<
            Option<(
                crate::session_manager::SavedSession,
                crate::session_manager::SessionLease,
            )>,
        > =
            // `attach_*` reserves the session's live lease first, and refuses
            // a session another window has open instead of becoming its second
            // autosaving writer. The lease is committed once the session is
            // applied.
            if session_id == "latest" {
                // Special case: resume the most recent session in this workspace.
                match manager.get_latest_session_for_workspace(&options.workspace) {
                    Ok(Some(meta)) => manager
                        .attach_session(&meta.id)
                        .map(|(recovery, lease)| Some((recovery.session, lease))),
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                }
            } else {
                manager
                    .attach_session_by_prefix(session_id)
                    .map(|(recovery, lease)| Some((recovery.session, lease)))
            };

        match load_result {
            Ok(Some((saved, lease))) => match manager.load_session_goal(&saved.metadata.id) {
                Ok(goal) => {
                    let saved_id = saved.metadata.id.clone();
                    match apply_loaded_session_with_goal(&mut app, config, saved, goal.as_ref()) {
                        Ok(()) => {
                            lease.commit();
                            app.status_message = Some(format!(
                                "Resumed session: {}",
                                crate::session_manager::truncate_id(&saved_id)
                            ));
                        }
                        Err(err) => {
                            crate::tui::ui::session_state::surface_session_load_failure(
                                &mut app,
                                format!("Failed to restore session: {err}"),
                            );
                        }
                    }
                }
                Err(err) => {
                    crate::tui::ui::session_state::surface_session_load_failure(
                        &mut app,
                        format!("Failed to restore session goal: {err}"),
                    );
                }
            },
            Ok(None) => {
                crate::tui::ui::session_state::surface_session_load_failure(
                    &mut app,
                    "No sessions found to resume".to_string(),
                );
            }
            Err(e) => {
                crate::tui::ui::session_state::surface_session_load_failure(
                    &mut app,
                    format!("Failed to load session: {e}"),
                );
            }
        }
    }

    // Auto-resume's receipt (#2934). It overrides the generic resume message
    // because it is the more specific truth: it names what was reattached, or
    // why nothing was. It never overwrites a *failure* message from the load
    // path above — a real error outranks a decision receipt.
    if let Some(notice) = options.startup_notice.clone()
        && app
            .status_message
            .as_deref()
            .is_none_or(|current| !current.starts_with("Failed to"))
    {
        app.status_message = Some(notice);
    }

    let session_id = ensure_runtime_session_id(&mut app);
    let transition =
        prepare_offline_queue_transition(&app, &session_id).map_err(anyhow::Error::msg)?;
    let restored_offline_queue = install_offline_queue_transition(&mut app, transition)
        || !app.queued_messages.is_empty()
        || app.queued_draft.is_some();
    if restored_offline_queue && app.status_message.is_none() && app.queued_message_count() > 0 {
        app.status_message = Some(format!(
            "Restored {} queued message(s) from previous session — ↑ to edit, Ctrl+X to discard",
            app.queued_message_count()
        ));
    }

    let task_manager = TaskManager::start(
        TaskManagerConfig::from_runtime(
            config,
            app.workspace.clone(),
            Some(app.model.clone()),
            Some(app.max_subagents.clamp(1, 4)),
        ),
        config.clone(),
        std::sync::Arc::clone(&app.plugin_registry),
        &session_id,
        app.current_session_metadata
            .as_ref()
            .and_then(|metadata| metadata.runtime_store.as_ref()),
    )
    .await?;
    if let Some(saved) = app
        .current_session_metadata
        .as_ref()
        .and_then(|meta| meta.runtime_store.as_ref())
        && task_manager
            .session_store_binding()
            .as_ref()
            .is_some_and(|current| current != saved)
    {
        app.push_status_toast(
            app.tr(MessageId::RuntimeStoreRecovered).into_owned(),
            StatusToastLevel::Warning,
            None,
        );
    }
    let _task_shutdown = task_manager.shutdown_guard();
    // The store this host holds, remembered for exit (#6144 P1b).
    let own_store = task_manager.session_store_binding();
    // Repair the session store in the background now that this host holds
    // its own Runtime store — and any store it resumed or recovered into — so
    // those read as in use, never as candidates (#6144).
    crate::session_reconcile::spawn_background_reconcile(app.current_session_id.clone());
    let mut automation_service = AutomationManager::default_location()?;
    automation_service.bind_task_manager(&task_manager)?;
    let automations = std::sync::Arc::new(tokio::sync::Mutex::new(automation_service));
    let automation_cancel = tokio_util::sync::CancellationToken::new();
    let automation_scheduler = spawn_scheduler(
        automations.clone(),
        task_manager.clone(),
        automation_cancel.clone(),
        AutomationSchedulerConfig::default(),
    );
    let shell_manager = app
        .runtime_services
        .shell_manager
        .clone()
        .unwrap_or_else(|| crate::tools::shell::new_shared_shell_manager(app.workspace.clone()));
    // #2511: ensure hook_executor is initialized for fresh sessions — it is
    // only set by apply_workspace_runtime_state (session resume / workspace
    // switch), so a brand-new session would otherwise leave it None and both
    // exec_shell shell_env hooks and ToolCallBefore gate would silently no-op.
    if app.runtime_services.hook_executor.is_none() {
        app.runtime_services.hook_executor = Some(std::sync::Arc::new(app.hooks.clone()));
    }
    app.runtime_services = RuntimeToolServices {
        shell_manager: Some(shell_manager),
        persist_services_enabled: false,
        task_manager: Some(task_manager.clone()),
        automations: Some(automations),
        task_data_dir: Some(task_manager.data_dir()),
        active_task_id: None,
        active_thread_id: None,
        dynamic_tool_executor: None,
        work: app.runtime_services.work.clone(),
        // #456: plumb the App's HookExecutor so `exec_shell` can surface
        // the configured `shell_env` hooks. Clone the shared Arc.
        hook_executor: app.runtime_services.hook_executor.clone(),
        handle_store: app.runtime_services.handle_store.clone(),
        rlm_sessions: app.runtime_services.rlm_sessions.clone(),
        media_originals_dir: crate::media_originals::default_store_dir(),
    };
    crate::startup_trace::mark("task_manager_ready");
    refresh_active_task_panel(&mut app, &task_manager).await;
    refresh_automation_panel_blocking(&mut app).await;

    // A `[redaction] model_bound = "disabled"` request lowers the model-bound
    // masking boundary only after an explicit one-time confirmation on this
    // startup gate. Arm the gate before the engine spawns so it owns the first
    // screen; answering it rebuilds the engine with the confirmed mode.
    app.redaction_gate = crate::tui::redaction_gate::confirmation_required(config);

    // Restore before admitting initial input, including resumed conversations.
    let engine_handle = spawn_tui_engine_with_session(&mut app, config).await?;
    crate::startup_trace::mark("engine_spawned");
    // The translation client is optional: it never crashes the TUI on
    // startup, even when the API key is missing, the base URL is malformed,
    // or the network is unavailable.
    // Translations are skipped with a logged warning until a key is saved.
    let translation_client = match CodewhaleClient::new(config) {
        Ok(client) => Some(Arc::new(client)),
        Err(err) => {
            if app.onboarding == OnboardingState::None {
                tracing::warn!("Translation client initialization failed: {err}");
            }
            None
        }
    };

    // Fire session start hook
    {
        let context = app.base_hook_context();
        // Captured before the hook executor moves `context` into its blocking
        // task; the outbox emit below needs the same session identity.
        let outbox_thread_id = context.session_id.clone().unwrap_or_default();
        let outbox_mode = context.mode.clone();
        let outbox_model = context.model.clone();
        let outbox_workspace = context.workspace.clone();
        let hooks = app.hooks.clone();
        if let Err(error) =
            tokio::task::spawn_blocking(move || hooks.execute(HookEvent::SessionStart, &context))
                .await
        {
            tracing::error!(target: "hooks", %error, "session_start executor task was lost");
            app.status_message = Some("session_start hook executor did not run".to_string());
        }
        // Lifecycle outbox (`[lifecycle_outbox]`): fires alongside the
        // session_start hook, with the same session identity. No-op when
        // the feature is disabled.
        app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
            event: "session_start".to_string(),
            kind: "session.started".to_string(),
            thread_id: outbox_thread_id,
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({
                "mode": outbox_mode,
                "model": outbox_model,
                "workspace": outbox_workspace
                    .as_ref()
                    .map(|path| path.display().to_string()),
            }),
        });
    }

    // Spawn the persistence actor so checkpoint/session-save I/O stays off
    // the UI thread.  The actor serialises + writes to disk in a dedicated
    // task; the UI just `try_send`s a request and returns immediately.
    let persistence_runtime = SessionManager::default_location()
        .ok()
        .map(|persist_manager| {
            let (handle, task) = persistence_actor::spawn_persistence_actor(persist_manager);
            persistence_actor::init_actor(handle.clone());
            (handle, task)
        });

    // Re-park the queue restored above, now that the actor exists. Its clear
    // request carries no session id, so the actor learns which session owns
    // the parked file from a save — without this, draining a restored queue
    // to empty would leave the file behind and resend it on the next boot.
    if restored_offline_queue {
        persist_offline_queue_state(&app);
    }

    // A launch without a usable key opens the picker immediately (#6566).
    // A configured user's picker focuses the saved route so recovery cannot
    // silently replace it; an unconfigured user sees the provider list rather
    // than the built-in default's missing key.
    if app.onboarding == OnboardingState::Provider && app.onboarding_missing_key_recovery {
        let recover_configured_route = app.onboarding_recovers_configured_route();
        open_onboarding_provider_picker(&mut app, config, &engine_handle, recover_configured_route)
            .await;
    }

    // #4605: create the dispatch completion channel before any submit path so
    // initial input and queued follow-ups can dispatch without blocking the
    // startup sequence.
    // At most one user dispatch is allowed in flight. A two-slot completion
    // mailbox covers the hook stage plus the send stage without turning a
    // stalled UI into an unbounded queue of captured App mutations.
    let (dispatch_completion_tx, dispatch_completion_rx) =
        tokio::sync::mpsc::channel::<crate::tui::app::DispatchApplyFn>(2);
    app.dispatch_completion_tx = Some(dispatch_completion_tx);

    if std::mem::take(&mut app.start_remote_control_on_launch) {
        start_remote_control_session(&mut app, config);
    }
    submit_initial_input_if_ready(&mut app, config, &engine_handle).await?;

    crate::startup_trace::log_summary();
    // Pin the cold-start measurement at the same moment the summary is logged.
    // `log_summary` computes the same number into a local, emits it, clears its
    // buffer, and returns `()`, so this reads `PROCESS_START` directly rather
    // than through it. Only this path calls it, which is what keeps the
    // cold-start bucket absent on surfaces with no event loop.
    crate::startup_trace::mark_cold_start();
    let result = run_event_loop(
        &mut terminal,
        &mut app,
        config,
        engine_handle,
        task_manager.clone(),
        &event_broker,
        translation_client,
        pending_telemetry_notice,
        dispatch_completion_rx,
    )
    .await;
    automation_cancel.cancel();
    automation_scheduler.abort();
    if let Err(error) = task_manager.shutdown_and_wait().await {
        tracing::error!(%error, "Task manager shutdown remains incomplete");
    }

    // Join the startup-default writer before anything else tears down.
    //
    // The last thing a user does before quitting is very often the selection
    // they most want to survive — Tab into Operate, then Ctrl+C. Those writes
    // are queued off the event loop on purpose, so at this point one may still
    // be in flight or not yet started. Draining here is what makes "the last
    // immediate selection lands" true rather than a race against process exit.
    //
    // Failures are collected, not toasted: the event loop has already drawn its
    // final frame, so a toast would never be painted. They are printed below,
    // after the alternate screen is gone and stderr is back on the user's real
    // terminal.
    let startup_default_failures = app.startup_defaults.shutdown();
    for failure in &startup_default_failures {
        tracing::warn!(
            target: "settings",
            subjects = ?failure.subjects,
            detail = %failure.detail,
            "startup default was not persisted before shutdown",
        );
    }
    let startup_default_failures: Vec<String> = startup_default_failures
        .iter()
        .map(|failure| app.startup_default_failure_message(failure))
        .collect();

    // Fire session end hook
    {
        let context = app.base_hook_context();
        let hooks = app.hooks.clone();
        let hook_context = context.clone();
        if tokio::task::spawn_blocking(move || hooks.execute(HookEvent::SessionEnd, &hook_context))
            .await
            .is_err()
        {
            tracing::warn!(target:"hooks","session_end hook executor task was lost");
        }
        // Lifecycle outbox (`[lifecycle_outbox]`): fires alongside the
        // session_end hook, with the same session identity. No-op when
        // the feature is disabled.
        app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
            event: "session_end".to_string(),
            kind: "session.ended".to_string(),
            thread_id: context.session_id.clone().unwrap_or_default(),
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({
                "workspace": context.workspace
                    .as_ref()
                    .map(|path| path.display().to_string()),
                "total_tokens": context.total_tokens,
            }),
        });
    }

    // Keep the final session/turn receipts ahead of runtime teardown. A failed
    // observability sink must not prevent the user's session from shutting down.
    if let Err(error) = app.lifecycle_outbox.flush(Duration::from_secs(2)).await {
        tracing::warn!(target: "lifecycle_outbox", %error, "TUI lifecycle outbox did not drain before exit");
    }

    // Flush the persistence actor, collect the durability report (write
    // failures are surfaced, not discarded), then shut down gracefully.
    //
    // The session's crash-recovery checkpoint is cleared only for a settled
    // session. While a turn is in flight (or a spawned dispatch has not yet
    // applied), the checkpoint is the only durable record of that work:
    // clearing it here unconditionally could erase in-flight progress that
    // never reached a snapshot, so it survives for startup recovery review.
    let mut shutdown_save_health = None;
    if let Some((handle, task)) = persistence_runtime {
        // A quit key can leave the frame before its usual queue comparison.
        // Capture the final edited draft before the shutdown durability barrier.
        persist_offline_queue_state(&app);
        let turn_in_flight = turn_unsettled_for_shutdown(&app);
        if turn_in_flight {
            tracing::info!(
                target: "persistence",
                "shutdown preserves the in-flight checkpoint for recovery review"
            );
        } else if let Err(error) = persist_settled_session_on_shutdown(&mut app, &handle) {
            tracing::warn!(
                target: "persistence",
                %error,
                "session snapshot could not be queued during shutdown; checkpoint retained"
            );
        }
        let (report_tx, report_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: report_tx });
        if let Ok(report) = report_rx.await
            && !report.failures.is_empty()
        {
            tracing::warn!(
                target: "persistence",
                failures = ?report.failures,
                "session persistence reported write failures during shutdown",
            );
        }
        // Read after the final flush: whether each session's latest save
        // landed, not every failure this run has ever seen.
        shutdown_save_health = Some(handle.session_save_health());
        handle.try_send(PersistRequest::Shutdown);
        let _ = task.await;
    }

    // A host that never bound a document to its own store leaves it empty
    // (#6144 P1b). Set it aside on the way out. A document binding it, work
    // in it, or anything in this process still holding it keeps it; the next
    // launch's repair applies the same exact rule to whatever remains.
    if let Some(store) = own_store {
        app.runtime_services.task_manager = None;
        drop(task_manager);
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(manager) = SessionManager::default_location() {
                crate::session_reconcile::retire_unbound_store(
                    &manager,
                    &store.data_dir,
                    "host exited without binding its store",
                );
            }
        })
        .await;
    }

    cleanup_guard.defused = true;
    crate::tui::cursor_accent::restore_cursor_accent();
    pop_keyboard_enhancement_flags(terminal.backend_mut());
    disable_alternate_scroll_mode(terminal.backend_mut());
    execute!(terminal.backend_mut(), DisableFocusChange)?;
    disable_raw_mode()?;
    // `/inline` and `/fullscreen` can have moved the screen since startup; the
    // teardown must match the screen the terminal is actually on.
    if app.use_alt_screen() {
        leave_alt_screen(terminal.backend_mut())?;
        #[cfg(windows)]
        crate::logging::restore_verbose_state();
    }
    if app.use_mouse_capture {
        execute!(terminal.backend_mut(), DisableMouseCapture)?;
    }
    if use_bracketed_paste {
        disable_bracketed_paste_mode(terminal.backend_mut());
    }
    terminal.show_cursor()?;
    drop(terminal);

    // Back on the primary screen, so this is somewhere the user can actually
    // read. A settings write that did not land would otherwise be invisible
    // until the next launch quietly came up in the old mode.
    for failure in &startup_default_failures {
        tracing::error!(target: "settings", "{failure}");
        // Printed AFTER `LeaveAlternateScreen` / `drop(terminal)`, so this is on
        // the restored primary screen. The module-level
        // `#![deny(clippy::print_stderr)]` would otherwise refuse it.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("codewhale: {failure}");
        }
    }

    if let Some(notice) = shutdown_save_health
        .as_ref()
        .and_then(|reading| shutdown_persistence_notice(app.ui_locale, reading))
    {
        // Primary screen, like the settings failures above.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("{notice}");
        }
    }

    // `codewhale resume <id>` for a document that never reached disk (every
    // save failed, or nothing was ever saved) only fails with NotFound.
    let session_document_exists = app.current_session_id.as_deref().is_some_and(|id| {
        SessionManager::default_location().is_ok_and(|manager| manager.session_document_exists(id))
    });
    if result.is_ok()
        && session_document_exists
        && let Some(hint) = resume_hint_text(
            app.ui_locale,
            app.current_session_id.as_deref(),
            io::stdout().is_terminal(),
        )
    {
        // Printed AFTER `LeaveAlternateScreen` / `drop(terminal)` above,
        // so we're back on the primary screen — this is the one
        // legitimate stdout write in the TUI module tree. The
        // module-level `#![deny(clippy::print_stdout)]` would otherwise
        // refuse it.
        #[allow(clippy::print_stdout)]
        {
            println!("{hint}");
        }
    }

    result
}

/// Whether a composer guard owns this launch-screen Enter. Every guard is
/// applied here, before a session exists, so a held submit never leaves the
/// user in a new empty session.
pub(super) fn launch_submit_held(app: &mut App) -> bool {
    if app.startup_input_unproven || !app.composer_enter_would_submit() {
        // A paste burst, empty composer or startup integrity hold.
        app.handle_composer_enter();
        return true;
    }
    // An oversized draft is backed up to a paste file now; if that fails the
    // submit is held with the full text in the composer.
    !app.consolidate_large_input_if_oversized()
}

/// Submit the pre-session composer's message as the first message of a new
/// session.
///
/// The startup screen owns the keyboard until a real session exists, so a
/// send from its composer first begins the launch session through the same
/// `begin_launch_session` path the startup rows use, then hands the
/// submitted text to the ordinary composer dispatch branches (memory quick-
/// add, `!` shell, `/` command, message). There is still exactly one turn
/// loop: this only routes input into `Engine::run_turn` like any other
/// composer submit.
///
/// Ordering is draft-loss-proof: the composer draft is consumed only after
/// the launch transition has been applied. A paste-burst absorption never
/// begins a session, and if applying the transition fails after it began,
/// the draft is still sitting in the composer for the user to resubmit —
/// the failure can never erase it.
#[allow(clippy::too_many_arguments)]
async fn dispatch_launch_composer_submit(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    chord: ComposerSubmitChord,
) -> Result<bool> {
    if app.launch.return_to_session {
        app.launch.dismiss();
        return dispatch_session_composer_submit(
            terminal,
            app,
            engine_handle,
            task_manager,
            config,
            chord,
        )
        .await;
    }
    let action = app.decide_composer_submit(chord);
    if launch_submit_held(app) {
        return Ok(false);
    }
    let result = begin_launch_session(app, None);
    if apply_command_result(terminal, app, engine_handle, task_manager, config, result).await? {
        return Ok(true);
    }
    // The input below is dispatched in this same keypress. Let the engine
    // install the new session first, so a plugin command or skill typed on the
    // startup screen resolves for this session instead of reading as unknown.
    await_engine_session_sync(engine_handle).await;
    // The transition is applied; only now consume the draft it carries.
    let Some(input) = app.handle_composer_enter() else {
        return Ok(false);
    };
    if should_intercept_memory_quick_add(config, &input) {
        handle_memory_quick_add(app, &input, config);
        return Ok(false);
    }
    if handle_bang_shell_input(app, engine_handle, &input).await? {
        return Ok(false);
    }
    if looks_like_slash_command_input(&input) {
        // Commands own their output; only model-bound prompts become user turns.
        if execute_command_input(terminal, app, engine_handle, task_manager, config, &input).await?
        {
            return Ok(true);
        }
    } else {
        let (queued, recovery) = message_from_submitted_input(app, input);
        dispatch_composer_message(app, config, engine_handle, queued, recovery, action).await?;
    }
    Ok(false)
}

/// Show why a turn ended without success. The composer status line always
/// names it; a turn the Engine stopped itself (wall-clock or step budget, no
/// progress, an incomplete response) posts no error event, so its reason also
/// goes into the transcript — a footer line alone is replaced by the next
/// notice, and the session then reads as hung. When an error event already
/// put the message in the transcript, nothing is repeated.
pub(super) fn present_turn_failure(
    app: &mut App,
    status: crate::core::events::TurnOutcomeStatus,
    error: Option<&str>,
) {
    let failed = matches!(status, crate::core::events::TurnOutcomeStatus::Failed);
    // What the transcript shows for this turn's failure: the error cell an
    // earlier `Event::Error` already posted, or the notice added here.
    let shown = if app.turn_error_posted {
        app.turn_error_notice.clone()
    } else if let Some(error) = error {
        let notice = format!("{}: {error}", app.tr(MessageId::NotificationTurnFailed));
        if failed {
            app.add_message(HistoryCell::Error {
                message: notice.clone(),
                severity: crate::error_taxonomy::ErrorSeverity::Warning,
            });
        }
        app.set_sticky_status(notice.clone(), StatusToastLevel::Error, None);
        Some(notice)
    } else {
        None
    };
    // Persist the failure with the session (redacted), so resume, export,
    // and the Runtime API can say why the turn stopped after the TUI closes.
    if failed && let Some(shown) = shown {
        let outcome =
            crate::session_manager::SavedTurnOutcome::failed(&shown, app.api_messages.len());
        crate::session_manager::push_turn_outcome(&mut app.session_turn_outcomes, outcome);
    }
}

/// Submit the live-session composer through the same branches Enter uses.
///
/// Mouse `[↵]` sets `pending_composer_submit`; this consumes that chord without
/// duplicating draft consumption or opening transcript-only Enter shortcuts.
/// Its own gates (`SendQueuedNow`, the paste-burst probe) run here; everything
/// from slash-menu selection onward is the shared `submit_decided_composer_input`
/// tail the keyboard Enter arm also uses, so the two surfaces cannot drift.
#[allow(clippy::too_many_arguments)]
async fn dispatch_session_composer_submit(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    chord: ComposerSubmitChord,
) -> Result<bool> {
    if app.launch.return_to_session {
        app.launch.dismiss();
    }
    let action = app.decide_composer_submit(chord);
    if matches!(action, ComposerSubmitAction::SendQueuedNow) {
        let _ = send_next_queued_message_now(app, config, engine_handle).await?;
        return Ok(false);
    }
    if !app.composer_enter_would_submit() {
        return Ok(false);
    }
    submit_decided_composer_input(terminal, app, engine_handle, task_manager, config, action).await
}

/// Shared tail of a decided composer submit: slash-menu selection, draft
/// consumption, and the memory/`!`/`/`/message branches.
///
/// Keyboard Enter and the mouse `[↵]` dispatcher both end here. Each caller
/// keeps its own gates — transcript-only shortcuts and forced-submit chords
/// stay keyboard-only, `SendQueuedNow` and the paste-burst probe stay in the
/// dispatcher — so this tail is the one place either surface can change.
/// Returns `true` only when a command asked the event loop to exit.
#[allow(clippy::too_many_arguments)]
async fn submit_decided_composer_input(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    action: ComposerSubmitAction,
) -> Result<bool> {
    // #573: when the user typed a slash-command prefix that the popup is
    // matching (e.g. `/mo` → `/model`), submit runs the *highlighted match*
    // rather than sending the literal `/mo` text. Only kick in when the
    // popup has at least one entry; otherwise fall through to the legacy
    // submit path.
    let slash_menu_entries = visible_slash_menu_entries(app, SLASH_MENU_LIMIT);
    let slash_menu_open = !slash_menu_entries.is_empty();
    let selecting_inline_skill = slash_menu_open
        && partial_inline_skill_mention_at_cursor(&app.input, app.cursor_position).is_some();
    if slash_menu_open && apply_slash_menu_selection(app, &slash_menu_entries, false) {
        app.close_slash_menu();
        if selecting_inline_skill {
            return Ok(false);
        }
    }

    let Some(input) = app.handle_composer_enter() else {
        return Ok(false);
    };
    // `# foo` quick-add (#492) — when memory is enabled, a single line
    // starting with `#` (but not `##` / `#!` shebangs / Markdown headings
    // the user might be pasting in) is intercepted: the text is appended to
    // the user memory file and the input is consumed without firing a turn.
    // Disabled behaviour falls through to normal turn submit.
    if should_intercept_memory_quick_add(config, &input) {
        handle_memory_quick_add(app, &input, config);
        return Ok(false);
    }
    if handle_bang_shell_input(app, engine_handle, &input).await? {
        return Ok(false);
    }
    if looks_like_slash_command_input(&input) {
        // Opening a view is not a conversation turn. SendMessage actions
        // record their real prompt through dispatch_composer_message instead.
        if execute_command_input(terminal, app, engine_handle, task_manager, config, &input).await?
        {
            return Ok(true);
        }
    } else {
        // #383: /edit — if the user invoked /edit to revise the last
        // message, undo the last exchange before dispatching the
        // replacement. Sync the engine session so it also drops the old
        // exchange.
        if let Some(result) = edit_replacement_result(app, &input) {
            return apply_command_result(
                terminal,
                app,
                engine_handle,
                task_manager,
                config,
                result,
            )
            .await;
        }
        let (queued, recovery) = message_from_submitted_input(app, input);
        dispatch_composer_message(app, config, engine_handle, queued, recovery, action).await?;
    }
    Ok(false)
}

/// The replacement for an exchange being revised with `/edit`: roll the last
/// exchange back through the same Engine-acknowledged, durably saved path as
/// `/retry`, then send `input` in its place. `None` when no edit is pending
/// or there is nothing to replace, so the input is sent as a normal turn.
///
/// The rollback is staged, not applied, by the command layer (#6788); running
/// `/undo` here and discarding its result left the old exchange in the
/// transcript, the model context and the saved session.
pub(super) fn edit_replacement_result(
    app: &mut App,
    input: &str,
) -> Option<commands::CommandResult> {
    if !std::mem::take(&mut app.edit_in_progress) {
        return None;
    }
    let sync = crate::commands::staged_conversation_undo(app)?;
    Some(commands::CommandResult {
        message: None,
        action: Some(AppAction::ConversationUndo {
            sync,
            retry_input: Some(input.to_string()),
            edit_replacement: true,
        }),
        is_error: false,
    })
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
/// Whether the git probe may run this tick: whenever the workspace-context
/// refresh may, and also during a live turn or agent run while the Git view
/// is the one showing (#6565). The probe is off-thread, on a 2s TTL, and
/// takes no optional locks, so running it mid-turn cannot block the user's
/// own git.
pub(crate) fn git_probe_allowed(app: &App, workspace_context_refresh_allowed: bool) -> bool {
    workspace_context_refresh_allowed || git_panel_visible(app)
}

/// The quiet time the git probe schedule sees: none at all while the Git
/// panel is showing, so that live view keeps the fast cadence (#6728).
pub(crate) fn git_probe_quiet_for(app: &App, quiet_for: Duration) -> Duration {
    if git_panel_visible(app) {
        Duration::ZERO
    } else {
        quiet_for
    }
}

/// The Git rail panel is showing: it is the live repository state, so the
/// probe keeps its fast cadence however quiet the session is (#6728).
pub(crate) fn git_panel_visible(app: &App) -> bool {
    app.work_surface.panel == crate::tui::work_surface::RailPanel::Git
        && app.work_surface.effective_placement()
            != crate::tui::work_surface::WorkSurfacePlacement::Off
}

pub(crate) async fn run_event_loop(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    mut engine_handle: EngineHandle,
    task_manager: SharedTaskManager,
    event_broker: &EventBroker,
    translation_client: Option<Arc<CodewhaleClient>>,
    mut pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
    mut dispatch_completion_rx: tokio::sync::mpsc::Receiver<crate::tui::app::DispatchApplyFn>,
) -> Result<()> {
    // Track streaming state
    let mut current_streaming_text = String::new();
    let mut stream_display_clock = StreamDisplayClock::default();
    let (translation_tx, mut translation_rx) =
        tokio::sync::mpsc::unbounded_channel::<TranslationEvent>();
    let fallback_translation_client = translation_client;
    // Set when the telemetry disclosure cell is queued; cleared (and the
    // disclosure recorded) by the first draw that paints it.
    let mut telemetry_notice_awaiting_render = false;
    let mut active_translation_client = fallback_translation_client.clone();
    let mut active_translation_route: Option<crate::core::events::TurnRoute> = None;
    let mut translation_sequence = 0_u64;
    let mut pending_translations = 0usize;
    // #5931: the background runtime's own store faults arrive on its event
    // channel, which nothing else in this loop reads.
    let mut runtime_event_rx = task_manager.subscribe_runtime_events();
    let mut pending_thinking_translations = 0usize;
    let mut last_queue_state = offline_queue_projection(app);
    let mut last_queue_was_empty = app.queued_messages.is_empty() && app.queued_draft.is_none();
    let mut last_task_refresh = Instant::now()
        .checked_sub(Duration::from_secs(2))
        .unwrap_or_else(Instant::now);
    let mut last_status_frame = Instant::now()
        .checked_sub(Duration::from_millis(UI_STATUS_ANIMATION_MS))
        .unwrap_or_else(Instant::now);
    // #6728: the last moment anything happened that wanted a prompt reaction:
    // a terminal event, an engine event, or any non-quiescent UI state. The
    // idle poll, the automation scan and the git probe all back off from it,
    // and all return to full cadence the moment it moves.
    let mut last_ui_activity = Instant::now();
    let mut skill_registry_epoch = None;
    let mut skill_cache_refresh: Option<SkillCacheRefresh> = None;
    // Whether the previous iteration found the UI quiescent and quiet (see
    // `ui_state_is_quiescent`). The 2.5 s task block runs before this
    // iteration's facts exist, so it reads the previous one.
    let mut ui_quiet = false;
    let mut last_automation_scan = Instant::now()
        .checked_sub(AUTOMATION_SCAN_BUSY_INTERVAL)
        .unwrap_or_else(Instant::now);
    // 120 FPS draw cap. Without this we redraw on every SSE chunk during a
    // long stream — wasted work the user can't perceive. See
    // `tui::frame_rate_limiter` for the rationale; ports the small piece of
    // codex's frame coalescing that maps cleanly onto our poll-based loop.
    // Measured display Hz may raise the floor toward the panel refresh rate
    // (still never faster than MIN_FRAME_INTERVAL); low_motion always wins.
    let mut frame_rate_limiter = crate::tui::frame_rate_limiter::FrameRateLimiter::default();
    {
        let probe = crate::tui::display_refresh::probe_display_refresh();
        frame_rate_limiter.set_adaptive_interval(Some(
            crate::tui::display_refresh::draw_min_interval_for_hz(probe.hz, false),
        ));
    }
    // Widgets request future animation frames here; the poll loop remains the
    // sole `terminal.draw` emitter (no competing animation loop).
    let mut frame_requester = FrameRequester::new();
    // Per-session control socket (`[control_socket]`): disabled unless the
    // config enables it; even then, nothing binds until the owned session id
    // appears (see the per-iteration reconcile below).
    let mut session_control = SessionControl::new(
        config
            .control_socket
            .as_ref()
            .is_some_and(|socket| socket.enabled),
    );
    let mut prev_input_snapshot = String::new();
    let mut terminal_paused_at: Option<Instant> = None;
    // Last observed coarse turn state for the session-state hook transitions
    // (#6004); `None` until the first publish records it without firing.
    let mut previous_turn_state = None;
    let mut force_terminal_repaint = false;
    // #6311: while the terminal reports unfocused, frames are pure backlog
    // (GTK3 defers all VTE damage on occlusion and replays it on return).
    // Event ingestion continues; only `terminal.draw` emission is gated.
    let mut terminal_unfocused = false;
    let defer_frames_on_focus_loss = focus_loss_defers_frames(
        std::env::var("VTE_VERSION").ok().as_deref(),
        std::env::var("TMUX").ok().as_deref(),
    );
    // FocusGained debounce: some terminal emulators (e.g. Tabby) re-trigger
    // FocusGained when we re-arm focus-change reporting inside
    // recover_terminal_modes, creating a tight repaint loop. Skip
    // mode recovery (but still mark a repaint) within the debounce window.
    const FOCUS_RECOVERY_DEBOUNCE: Duration = Duration::from_millis(200);
    let mut last_focus_recovery = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .unwrap_or_else(Instant::now);
    // #5925: the startup terminal probes (OSC 11 background, kitty graphics,
    // sixel primary-DA) were the only readers of the tty until now, and they
    // consumed whatever the user had already typed. Replay it into the same
    // queue the pump feeds — and do it *before* the pump is spawned, so those
    // keys are delivered ahead of anything still sitting in the tty rather
    // than behind it.
    let mut replayed_startup_events = VecDeque::new();
    let startup_input_receipt =
        crate::tui::startup_input::replay_into(&mut replayed_startup_events);
    let startup_input_observed_at = Instant::now();
    let mut pending_terminal_events: VecDeque<ObservedTerminalEvent> = replayed_startup_events
        .into_iter()
        .map(|event| ObservedTerminalEvent::new(event, startup_input_observed_at))
        .collect();
    // When startup could not account for every byte it consumed, the shell
    // cannot prove it saw the whole line. The composer holds the next submit
    // instead of sending text it cannot vouch for.
    app.startup_input_unproven = !startup_input_receipt.whole_line_proven();
    let mut terminal_input = TerminalInputPump::spawn()?;
    let mut last_terminal_input_recovery = Instant::now()
        .checked_sub(TERMINAL_INPUT_RECOVERY_COOLDOWN)
        .unwrap_or_else(Instant::now);
    let mut last_recovery_snapshot_at: Option<Instant> = None;
    // Fire-and-forget version check — runs once per session in the
    // background. On success, a short status toast advertises the update
    // without replacing the user's configured footer/status-line chips.
    let mut version_check: Option<tokio::task::JoinHandle<Option<UpdateNotice>>> =
        spawn_startup_version_check(config.update_config());
    // First-run / missing-key: if a live local Ollama catalog answers, adopt a
    // real /api/tags model into chrome instead of leaving the DeepSeek costume.
    let mut local_ollama_probe: Option<
        tokio::task::JoinHandle<Option<crate::local_ollama::LiveLocalOllamaCatalog>>,
    > = crate::local_ollama::spawn_local_ollama_adoption_probe(
        config,
        app.should_adopt_live_local_ollama(),
    );

    // Startup version-change hint: once per version, never on first run.
    // `record_launch` owns the semantics (strict semver forward move, corrupt
    // record = silent rewrite, downgrade records without hinting); this only
    // renders the outcome. Local bookkeeping — independent of the network
    // update check, and skipped entirely when home cannot be resolved.
    if let Ok(home) = codewhale_config::codewhale_home() {
        let outcome = codewhale_release::record_launch(&home, env!("CARGO_PKG_VERSION"));
        if let Some(record_error) = outcome.record_error {
            tracing::debug!(error = %record_error, "could not persist the last-launch record");
        }
        if let Some(change) = outcome.change {
            let content = app
                .tr(MessageId::UpdateChangedHint)
                .replace("{previous}", &change.previous)
                .replace("{current}", &change.current);
            app.add_message(HistoryCell::System { content });
            app.needs_redraw = true;
        }
    }

    // Fire a one-shot initial remaining-credit fetch for prepaid
    // providers so the footer chip can show on the first frame without
    // waiting for a turn to complete.
    if !app.balance_initiated {
        let api_key = config.active_route_api_key().unwrap_or_default();
        let base_url = config.active_route_base_url();
        schedule_balance_fetch(app, &api_key, &base_url, false);
        app.balance_initiated = true;
    }

    let mut pending_subagent_list_refresh = false;
    let mut session_save_health_seen = 0u64;

    loop {
        // #6169: first statement of every iteration. The job-control handler can
        // stop this process mid-turn (SIGTSTP, or SIGTTIN once the group is
        // backgrounded) after restoring the terminal from inside the handler.
        // SIGCONT only records that the stop happened; the rebuild happens here,
        // in normal context, where crossterm is safe to call.
        //
        // Two deferrals, both deliberate: a child owning the tty is handled by
        // the pause/resume block further down (it rebuilds the modes itself), and
        // a group that is still background (a plain `bg`) must not touch the
        // terminal at all — re-entering raw mode and the alternate screen would
        // steal the shell's tty. The state is left pending either way, so the
        // rebuild still runs on the iteration after `fg`.
        if job_control_guard::take_resume()
            && !event_broker.is_paused()
            && require_foreground_terminal_owner().is_ok()
        {
            job_control_guard::mark_resumed();
            resume_terminal(
                terminal,
                app.use_alt_screen(),
                app.use_mouse_capture,
                app.use_bracketed_paste,
                app.synchronized_output_enabled,
            )?;
            event_broker.resume_events();
            // The input pump is deliberately not told about this: it is only
            // ever gated by `pause_terminal_input_for_child` /
            // `resume_after_child_terminal`, and calling the latter here would
            // falsely clear a child's gate.
            app.status_message = Some("Resumed after suspend".to_string());
            app.needs_redraw = true;
            force_terminal_repaint = true;
        }

        // The background session-store repair's one-line result (#6144).
        if let Some(notice) = crate::session_reconcile::take_pending_notice() {
            app.push_status_toast(notice, StatusToastLevel::Info, None);
            app.needs_redraw = true;
        }

        // The disclosure is a transcript cell, not a toast: a 12 s toast
        // showed only its first sentence at 100 columns and hid the opt-out.
        // A transcript cell would also replace the launch card, whose
        // "no model connected" line is the first-run recovery, so the cell
        // waits until the card starts to leave. It counts as presented only
        // once a frame containing it was drawn; quitting first re-owes it.
        if app.onboarding == OnboardingState::None
            && telemetry_notice_may_enter_transcript(app)
            && pending_telemetry_notice.take().is_some()
        {
            let notice = app.tr(MessageId::TelemetryNoticeDefaultOn).into_owned();
            app.add_message(HistoryCell::System { content: notice });
            app.needs_redraw = true;
            telemetry_notice_awaiting_render = true;
        }

        // A manual compaction deferred by a full engine mailbox retries here
        // each iteration until a slot frees or a live pass supersedes it.
        flush_deferred_manual_compaction(app, config, &engine_handle);
        // Any fleet mutation since the last iteration (`/fleet add|remove`,
        // ⇧F, auto-enroll) reaches the engine here, through the one roster
        // path the saved-fleet views already use.
        flush_stale_fleet_roster(app, config, &engine_handle);
        // Goal controls are accepted only after their bounded sidecar is
        // durable. Mailbox backpressure must therefore defer delivery, never
        // block keyboard input or silently drop the accepted control.
        flush_pending_goal_controls(app, &engine_handle);

        // Per-session control socket: rebind when the owned session id
        // changes, republish the `status` snapshot, and execute queued
        // verbs on the UI thread.
        session_control.reconcile(app.current_session_id.as_deref());
        session_control.update_status(app);
        execute_session_state_transition_hooks(app, &mut previous_turn_state);
        session_control
            .drain(
                app,
                config,
                &engine_handle,
                &mut current_streaming_text,
                &mut stream_display_clock,
            )
            .await;

        while let Some(completion) = app.clipboard.poll_write_completion() {
            if let Err(err) = completion {
                tracing::warn!(error = %err, "background terminal clipboard write failed");
                app.push_status_toast(
                    format!("Clipboard copy failed: {err}"),
                    StatusToastLevel::Error,
                    None,
                );
                app.needs_redraw = true;
            }
        }

        // Drain dispatch completions from spawned send tasks (#4605). The
        // closure receives `&mut App` and applies success state or rollback.
        while let Ok(apply) = dispatch_completion_rx.try_recv() {
            let _ = apply(app, &engine_handle, &*config);
            // Drain this completion immediately: a later completion cannot replace an edit.
            super::feedback_host::dispatch_ready(app, config, &engine_handle).await?;
        }

        // Drain the version-check handle once; re-assign None so we
        // don't poll it again.
        let mut done = false;
        if let Some(ref handle) = version_check {
            done = handle.is_finished();
        }
        if done && let Ok(Some(notice)) = version_check.take().unwrap().await {
            // Transient toast for immediate visibility, plus a durable
            // in-transcript notice so the prompt survives the toast TTL and
            // stays actionable during a busy session (#3961). The persistent
            // header chip keeps a quiet affordance after both (#14).
            // Which command to advertise depends on who owns this binary on
            // disk, so resolve that here rather than hardcoding our own
            // updater into the wording.
            let install = codewhale_release::current_install_method();
            app.update_available = Some(notice.chip_label());
            app.push_status_toast(
                notice.toast_line(install),
                StatusToastLevel::Info,
                Some(VERSION_HINT_TOAST_TTL_MS),
            );
            app.add_message(HistoryCell::System {
                content: notice.notice_block(install),
            });
        }

        // Adopt a live local Ollama tag into first-run / missing-key chrome.
        let mut local_done = false;
        if let Some(ref handle) = local_ollama_probe {
            local_done = handle.is_finished();
        }
        if local_done && let Ok(Some(catalog)) = local_ollama_probe.take().unwrap().await {
            adopt_live_local_ollama_catalog(app, &mut engine_handle, config, catalog).await;
        }

        // Non-blocking startup-default writes (mode / thinking) report their
        // failures here rather than at the keystroke, so a settings file we
        // could not write is visible instead of silently reverting next launch.
        app.drain_startup_default_failures();

        while let Ok(event) = translation_rx.try_recv() {
            match event {
                TranslationEvent::AssistantMessage {
                    origin_session_fingerprint,
                    origin_turn_fingerprint,
                    history_index,
                    original_text,
                    translated,
                    usage,
                    thinking,
                    tool_uses,
                } => {
                    pending_translations = pending_translations.saturating_sub(1);
                    if translation_session_is_current(app, origin_session_fingerprint.as_deref())
                        && let Some(usage) = usage.as_ref()
                    {
                        accrue_translation_usage(app, usage);
                    }
                    if !translation_origin_is_current(
                        app,
                        origin_session_fingerprint.as_deref(),
                        origin_turn_fingerprint.as_deref(),
                    ) {
                        tracing::debug!(
                            "discarded assistant translation completed for a stale session/turn"
                        );
                        continue;
                    }
                    let text = match translated {
                        Ok(text) => {
                            app.status_message = Some(
                                codewhale_localization::tr(
                                    app.ui_locale,
                                    codewhale_localization::MessageId::TranslationComplete,
                                )
                                .to_string(),
                            );
                            text
                        }
                        Err(err) => {
                            tracing::warn!("assistant translation failed: {err}");
                            app.status_message = Some(format!(
                                "{}: {err}",
                                codewhale_localization::tr(
                                    app.ui_locale,
                                    codewhale_localization::MessageId::TranslationFailed,
                                )
                            ));
                            codewhale_localization::hidden_translation_failed(app.ui_locale)
                                .to_string()
                        }
                    };

                    if let Some(index) = history_index
                        && let Some(HistoryCell::Assistant { content, .. }) =
                            app.history.get_mut(index)
                    {
                        *content = text.clone();
                        app.record_completed_assistant_output(index, &text);
                        app.bump_history_cell(index);
                    }
                    if !replace_matching_assistant_text(app, &original_text, text.clone()) {
                        push_assistant_message(app, text, thinking, tool_uses);
                    }
                    if pending_translations == 0
                        && !matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
                    {
                        app.is_loading = pending_translations > 0;
                    }
                    app.needs_redraw = true;
                }
                TranslationEvent::Thinking {
                    origin_session_fingerprint,
                    origin_turn_fingerprint,
                    placeholder,
                    translated,
                    usage,
                } => {
                    pending_translations = pending_translations.saturating_sub(1);
                    pending_thinking_translations = pending_thinking_translations.saturating_sub(1);
                    if translation_session_is_current(app, origin_session_fingerprint.as_deref())
                        && let Some(usage) = usage.as_ref()
                    {
                        accrue_translation_usage(app, usage);
                    }
                    if !translation_origin_is_current(
                        app,
                        origin_session_fingerprint.as_deref(),
                        origin_turn_fingerprint.as_deref(),
                    ) {
                        tracing::debug!(
                            "discarded thinking translation completed for a stale session/turn"
                        );
                        continue;
                    }
                    let text = match translated {
                        Ok(text) => {
                            app.status_message = Some(
                                codewhale_localization::thinking_translation_complete(
                                    app.ui_locale,
                                )
                                .to_string(),
                            );
                            text
                        }
                        Err(err) => {
                            tracing::warn!("thinking translation failed: {err}");
                            app.status_message = Some(format!(
                                "{}: {err}",
                                codewhale_localization::thinking_translation_failed(app.ui_locale)
                            ));
                            codewhale_localization::hidden_translation_failed(app.ui_locale)
                                .to_string()
                        }
                    };
                    streaming_thinking::replace_pending_translation(app, &placeholder, text);
                    if pending_translations == 0
                        && !matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
                    {
                        app.is_loading = false;
                    }
                    app.needs_redraw = true;
                }
            }
        }

        if last_task_refresh.elapsed() >= Duration::from_millis(2500) {
            if refresh_active_task_panel(app, &task_manager).await {
                app.needs_redraw = true;
            }
            // Shells and tasks that finished join the batched notice; a batch
            // held for finite work or a busy parent turn goes out once that
            // work settles (#6565).
            flush_background_finished(app, config, false);
            // A finished scan is folded on every tick; the next one starts
            // only when due. A quiet UI with nothing scheduled or live
            // rescans on the long cadence (#6728).
            let automation_scan_due = automation_scan_is_due(
                app,
                ui_quiet,
                Instant::now().saturating_duration_since(last_automation_scan),
            );
            if refresh_automation_panel(app, automation_scan_due).await {
                app.needs_redraw = true;
            }
            if automation_scan_due {
                last_automation_scan = Instant::now();
            }
            if refresh_shell_exec_live_output(app) {
                app.needs_redraw = true;
            }
            if app
                .runtime_services
                .work
                .as_ref()
                .is_some_and(|work| work.has_pending_publish())
                && let Err(err) = persist_pending_work_checkpoint(app).await
            {
                tracing::warn!(error = %err, "background Work lifecycle checkpoint remains pending");
            }
            last_task_refresh = Instant::now();
        }

        // Clear suggestion when the user modifies the input.
        if app.input != prev_input_snapshot {
            app.prompt_suggestion = None;
            prev_input_snapshot = app.input.clone();
        }

        // Poll prompt suggestion cell from background generation task.
        // Discard stale results whose generation token no longer matches.
        if let Ok(mut guard) = app.prompt_suggestion_cell.try_lock()
            && let Some((gen_token, suggestion)) = guard.take()
            && gen_token
                == app
                    .prompt_suggestion_gen
                    .load(std::sync::atomic::Ordering::Relaxed)
        {
            app.prompt_suggestion = Some(suggestion);
        }

        // Poll the fleet-profile model-draft cell filled by the background
        // drafting task (#3757 review: the draft must not park the loop).
        let fleet_draft_delivery = app
            .fleet_draft_cell
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some((draft_gen, model_label, picked_route, reasoning_effort, outcome)) =
            fleet_draft_delivery
            && draft_gen == app.current_draft_gen()
        {
            deliver_fleet_draft_result(
                app,
                model_label,
                picked_route,
                reasoning_effort,
                outcome,
                app.ui_locale,
            );
        }

        // Poll the constitution model-draft cell (same background pattern).
        let constitution_draft_delivery = app
            .constitution_draft_cell
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some((draft_gen, model_label, draft_locale, outcome)) = constitution_draft_delivery
            && draft_gen == app.current_draft_gen()
        {
            deliver_constitution_draft_result(app, model_label, draft_locale, outcome);
        }

        surface_session_save_health(
            app,
            crate::tui::persistence_actor::session_save_health(),
            &mut session_save_health_seen,
        );

        // Discovery and callback delivery never park terminal input.
        poll_mcp_login(app);
        poll_mcp_retries(app);

        // #1830/#2317: service any already-arrived terminal keys before a
        // potentially long engine batch so composer/modal input stays live.
        collect_pending_terminal_events(&terminal_input, &mut pending_terminal_events)?;
        app.maybe_poll_plugin_catalog_idle();

        if drain_remote_control_events(app, config, &engine_handle).await? {
            app.needs_redraw = true;
        }

        // First, poll for engine events (non-blocking)
        let mut received_engine_event = false;
        // Any engine event at all, including ones that asked for no redraw:
        // it is activity for the idle backoff (#6728).
        let mut engine_event_seen = false;
        let mut transcript_batch_updated = false;
        // #freeze: coalesce per-event `Op::ListSubAgents` sends into a single
        // trailing-edge refresh per drain. At high fanout, many spawn/complete/
        // mailbox events in one drain otherwise each take the manager write
        // lock and trigger a full O(N) list reconcile.
        let mut subagent_list_refresh_requested = false;
        let mut queued_to_send: Option<QueuedMessage> = None;
        let mut respawn_after_provider_rollback: Option<String> = None;
        let mut fallback_after_engine_error: Option<ProviderFallbackRollback> = None;
        {
            let mut rx = engine_handle.rx_event.write().await;
            let mut progress_redraw_agents: HashSet<String> = HashSet::new();
            let drain_started = Instant::now();
            let mut events_drained = 0usize;
            loop {
                if events_drained > 0
                    && engine_drain_budget_exhausted(events_drained, drain_started, Instant::now())
                {
                    break;
                }
                let event = match rx.try_recv() {
                    Ok(event) => event,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        if recover_engine_event_disconnect(app) {
                            received_engine_event = true;
                            transcript_batch_updated = true;
                        }
                        break;
                    }
                };
                // Count every received event before any filter can `continue`
                // past it (U02-02). Filtered deltas, suppressed post-cancel
                // events and stale requests used to skip the counter, so a
                // producer flooding them never tripped the drain budget and
                // the loop never returned to render or read the cancel key.
                events_drained = events_drained.saturating_add(1);
                engine_event_seen = true;
                // #3033: remember whether an EARLIER event in this drain batch
                // already requested a redraw. The AgentProgress throttle below
                // may opt the current event out of repainting, but it must not
                // cancel redraws owed to other events in the same batch.
                let redraw_requested_before_event = received_engine_event;
                received_engine_event = true;
                capture_turn_started_metadata(app, &event);
                observe_human_request_settlement(app, &event);
                // Child approval bookkeeping runs before every filter: it is
                // keyed by approval id and agent, not by the active session,
                // so a withdrawal always retires its card (approvals M1).
                if crate::tui::pending_requests::observe_engine_event(app, &event) {
                    continue;
                }
                if app.suppress_stream_events_until_turn_complete {
                    if matches!(event, EngineEvent::TurnStarted { .. }) {
                        // Ctrl+C can race with the engine's per-turn token
                        // reset: the first cancel may hit the previous token
                        // if SendMessage is queued but TurnStarted has not
                        // arrived yet. Reassert cancellation once the real
                        // turn starts, then keep hiding its queued deltas.
                        engine_handle.cancel();
                        continue;
                    }
                    if suppress_engine_event_after_local_cancel(&event) {
                        continue;
                    }
                } else if !app.is_loading && ignore_stale_stream_event_while_idle(&event) {
                    continue;
                }
                if resolve_stale_parent_request(app, &engine_handle, &event).await {
                    continue;
                }
                if !matches!(event, EngineEvent::ApprovalRequired { .. }) {
                    app.remote_control.observe_engine_event(&event);
                    // A terminal boundary reached after deltas were shed under
                    // pressure repairs account truth with a bounded snapshot.
                    while let Some(resync_run) = app.remote_control.take_pending_resync() {
                        app.remote_control
                            .upload_resync_snapshot(&resync_run, &app.api_messages);
                    }
                }
                let pet_event_applies = match &event {
                    EngineEvent::AgentSpawned {
                        owner_session_id, ..
                    }
                    | EngineEvent::AgentProgress {
                        owner_session_id, ..
                    }
                    | EngineEvent::AgentComplete {
                        owner_session_id, ..
                    } => event_owner_is_active(app.current_session_id.as_deref(), owner_session_id),
                    EngineEvent::ApprovalRequired {
                        tool_name,
                        approval_grouping_key,
                        approval_key,
                        approval_force_prompt,
                        ..
                    } => {
                        matches!(
                            resolve_ui_approval_disposition(
                                app,
                                tool_name,
                                approval_grouping_key,
                                approval_key,
                                *approval_force_prompt
                            ),
                            crate::core::authority::ApprovalRequestDisposition::Prompt
                        )
                    }
                    _ => true,
                };
                if pet_event_applies {
                    crate::tui::pet_watch::observe(app, &event, Instant::now());
                }
                record_turn_activity(app, &event, Instant::now());
                match event {
                    EngineEvent::MessageStarted { .. } => {
                        // Assistant text starting after parallel tool work
                        // means the tool group is done. Flush the active
                        // cell first so the message lands BELOW the
                        // committed tool group (Codex pattern: streamed
                        // assistant content always flows after work).
                        app.flush_active_cell();
                        current_streaming_text.clear();
                        app.streaming_output_token_estimate = 0;
                        app.streaming_state.reset();
                        app.streaming_state.start_text(0);
                        app.streaming_message_index = None;
                        stream_display_clock.reset();
                    }
                    EngineEvent::MessageDelta { content, .. } => {
                        let sanitized = sanitize_stream_chunk(&content);
                        if sanitized.is_empty() {
                            continue;
                        }
                        // First delta of a fresh stream has no streaming
                        // cell yet; flush active so the tool group settles
                        // before the assistant prose appears below it.
                        if app.streaming_message_index.is_none() {
                            app.flush_active_cell();
                        }
                        current_streaming_text.push_str(&sanitized);
                        ensure_streaming_assistant_history_cell(app);
                        app.streaming_state.push_content(0, &sanitized);
                        stream_display_clock.note_delta(Instant::now());
                        received_engine_event = redraw_requested_before_event;
                    }
                    EngineEvent::MessageComplete { .. } => {
                        // #861 RC3: defensive drain of a still-active thinking
                        // entry. Normally `ThinkingComplete` arrives first and
                        // populates `last_reasoning` before we get here, but
                        // when the engine bursts events the channel can
                        // deliver `MessageComplete` first, in which case
                        // `last_reasoning.take()` below would be `None` and
                        // the thinking block would be dropped from
                        // `api_messages` — causing a DeepSeek HTTP 400 on the
                        // next turn (V4 thinking-mode requires
                        // `reasoning_content` replay). Inline-finalize the
                        // thinking entry here so this branch is order-
                        // independent.
                        if app.streaming_thinking_active_entry.is_some() {
                            if streaming_thinking::finalize_current(app) {
                                transcript_batch_updated = true;
                            }
                            streaming_thinking::stash_reasoning_buffer_into_last_reasoning(app);
                        }
                        let mut completed_message_index = None;
                        if let Some(index) = app.streaming_message_index.take() {
                            completed_message_index = Some(index);
                            stream_display_clock.flush_now(Instant::now());
                            let remaining = app.streaming_state.finalize_block_text(0);
                            if !remaining.is_empty() {
                                append_streaming_text(app, index, &remaining);
                                accrue_streaming_token_estimate(app, &remaining);
                            }
                            if let Some(HistoryCell::Assistant { streaming, .. }) =
                                app.history.get_mut(index)
                            {
                                *streaming = false;
                            }
                            // Streaming flag flipped — the cell's compact /
                            // transcript variants render slightly
                            // differently, so bump its revision so the cache
                            // refreshes this row only.
                            app.bump_history_cell(index);
                            transcript_batch_updated = true;
                            stream_display_clock.reset();
                        }

                        let thinking = app.last_reasoning.take();
                        let tool_uses = std::mem::take(&mut app.pending_tool_uses);
                        let history_index = completed_message_index;
                        if let Some(index) = history_index {
                            app.record_completed_assistant_output(index, &current_streaming_text);
                        }

                        if app.translation_enabled
                            && !current_streaming_text.is_empty()
                            && crate::tui::translation::needs_translation(&current_streaming_text)
                            && let Some(translation_client) = active_translation_client.as_ref()
                        {
                            app.status_message = Some(
                                codewhale_localization::tr(
                                    app.ui_locale,
                                    codewhale_localization::MessageId::TranslationInProgress,
                                )
                                .to_string(),
                            );
                            app.is_loading = true;
                            pending_translations = pending_translations.saturating_add(1);
                            let tx = translation_tx.clone();
                            let client = translation_client.clone();
                            let original_text = current_streaming_text.clone();
                            let translation_model = active_translation_route
                                .as_ref()
                                .map(|route| route.model.clone())
                                .or_else(|| app.last_effective_model.clone())
                                .unwrap_or_else(|| app.model.clone());
                            translation_sequence = translation_sequence.saturating_add(1);
                            let accounting = TranslationAccountingContext::capture(
                                app,
                                "assistant",
                                translation_sequence,
                            );
                            let (origin_session_fingerprint, origin_turn_fingerprint) =
                                translation_origin(app);
                            let target_language =
                                app.ui_locale.translation_target_name().to_string();
                            tokio::spawn(async move {
                                let settled = accounting.settle(
                                    client
                                        .translate_with_usage(
                                            &original_text,
                                            &translation_model,
                                            &target_language,
                                        )
                                        .await,
                                );
                                let _ = tx.send(TranslationEvent::AssistantMessage {
                                    origin_session_fingerprint,
                                    origin_turn_fingerprint,
                                    history_index,
                                    original_text,
                                    translated: settled.translated,
                                    usage: settled.usage,
                                    thinking,
                                    tool_uses,
                                });
                            });
                        } else {
                            push_assistant_message(
                                app,
                                current_streaming_text.clone(),
                                thinking,
                                tool_uses,
                            );
                        }
                    }
                    EngineEvent::ThinkingStarted { .. } => {
                        stream_display_clock.reset();
                        // P2.3: thinking lives in the active cell so it groups
                        // visually with the tool calls that follow until the
                        // next assistant prose chunk flushes the group.
                        if streaming_thinking::start_block(app) {
                            transcript_batch_updated = true;
                        }
                        if app.translation_enabled {
                            let entry_idx = streaming_thinking::ensure_active_entry(app);
                            streaming_thinking::set_placeholder(app, entry_idx);
                            transcript_batch_updated = true;
                        }
                    }
                    EngineEvent::ThinkingDelta { content, .. } => {
                        let sanitized = sanitize_stream_chunk(&content);
                        if sanitized.is_empty() {
                            continue;
                        }
                        app.reasoning_buffer.push_str(&sanitized);
                        if app.reasoning_header.is_none() {
                            app.reasoning_header = extract_reasoning_header(&app.reasoning_buffer);
                        }

                        streaming_thinking::ensure_active_entry(app);
                        app.streaming_state.push_content(0, &sanitized);
                        stream_display_clock.note_delta(Instant::now());
                        received_engine_event = redraw_requested_before_event;
                    }
                    EngineEvent::ThinkingComplete { .. } => {
                        stream_display_clock.flush_now(Instant::now());
                        if app.translation_enabled {
                            let original_thinking = app.reasoning_buffer.clone();
                            let _ = app.streaming_state.finalize_block_text(0);
                            let duration = app
                                .thinking_started_at
                                .take()
                                .map(|t| t.elapsed().as_secs_f32());
                            if streaming_thinking::finalize_active_entry(app, duration, "") {
                                transcript_batch_updated = true;
                            }
                            if !original_thinking.is_empty()
                                && crate::tui::translation::needs_translation(&original_thinking)
                                && let Some(translation_client) = active_translation_client.as_ref()
                            {
                                app.status_message = Some(
                                    codewhale_localization::thinking_translation_in_progress(
                                        app.ui_locale,
                                    )
                                    .to_string(),
                                );
                                app.is_loading = true;
                                pending_translations = pending_translations.saturating_add(1);
                                pending_thinking_translations =
                                    pending_thinking_translations.saturating_add(1);
                                let tx = translation_tx.clone();
                                let client = translation_client.clone();
                                let translation_model = active_translation_route
                                    .as_ref()
                                    .map(|route| route.model.clone())
                                    .or_else(|| app.last_effective_model.clone())
                                    .unwrap_or_else(|| app.model.clone());
                                translation_sequence = translation_sequence.saturating_add(1);
                                let accounting = TranslationAccountingContext::capture(
                                    app,
                                    "thinking",
                                    translation_sequence,
                                );
                                let (origin_session_fingerprint, origin_turn_fingerprint) =
                                    translation_origin(app);
                                let placeholder =
                                    codewhale_localization::thinking_translation_placeholder(
                                        app.ui_locale,
                                    )
                                    .to_string();
                                let target_language =
                                    app.ui_locale.translation_target_name().to_string();
                                tokio::spawn(async move {
                                    let settled = accounting.settle(
                                        client
                                            .translate_with_usage(
                                                &original_thinking,
                                                &translation_model,
                                                &target_language,
                                            )
                                            .await,
                                    );
                                    let _ = tx.send(TranslationEvent::Thinking {
                                        origin_session_fingerprint,
                                        origin_turn_fingerprint,
                                        placeholder,
                                        translated: settled.translated,
                                        usage: settled.usage,
                                    });
                                });
                            } else {
                                let placeholder =
                                    codewhale_localization::thinking_translation_placeholder(
                                        app.ui_locale,
                                    );
                                streaming_thinking::replace_pending_translation(
                                    app,
                                    placeholder,
                                    original_thinking,
                                );
                            }
                        } else if streaming_thinking::finalize_current(app) {
                            transcript_batch_updated = true;
                        }
                        streaming_thinking::stash_reasoning_buffer_into_last_reasoning(app);
                        stream_display_clock.reset();
                    }
                    EngineEvent::ToolCallStarted {
                        id,
                        name,
                        input,
                        model_call,
                    } => {
                        app.session_metrics.record_tool_started(&id);
                        if let Some(model_call) = model_call {
                            app.pending_tool_uses.push(ContentBlock::ToolUse {
                                id: model_call.provider_id,
                                execution_id: Some(id.clone()),
                                name: name.clone(),
                                input: input.clone(),
                                caller: model_call.caller,
                                thought_signature: model_call.thought_signature,
                            });
                        }
                        // Note this dispatch so the next sub-agent `Started`
                        // mailbox envelope routes into the right card kind
                        // (delegate vs fanout).
                        if matches!(
                            name.as_str(),
                            "agent" | "rlm_open" | "rlm_eval" | "rlm" | "delegate"
                        ) {
                            app.pending_subagent_dispatch = Some(name.clone());
                            if matches!(name.as_str(), "rlm_open" | "rlm_eval" | "rlm") {
                                // New fanout invocation — children should
                                // group under a fresh card, not the
                                // previous fanout's leftover.
                                app.last_fanout_card_index = None;
                            }
                        }
                        handle_tool_call_started(app, &id, &name, &input);
                    }
                    // Liveness only. `record_turn_activity` above consumes the
                    // pulse; it must not alter transcript or status copy.
                    EngineEvent::ToolExecutionStarted { .. }
                    | EngineEvent::ToolResultContent { .. } => {}
                    EngineEvent::ToolCallHeartbeat => {}
                    // Typed owner activity is a pet-facing projection;
                    // `pet_watch::observe` above already consumed it, and the
                    // transcript renders from the ToolCall* events.
                    EngineEvent::OperationActivityStarted { .. }
                    | EngineEvent::OperationActivityCompleted { .. } => {}
                    EngineEvent::ToolCallComplete {
                        id,
                        name,
                        result,
                        model_call,
                    } => {
                        if crate::tui::tool_routing::evidence_completion_should_be_ignored(
                            app, &id, &result,
                        ) {
                            tracing::debug!(tool_id = %id, tool_name = %name, "ignored foreign or replayed evidence completion");
                            continue;
                        }
                        app.session_metrics.record_tool_completed(&id);
                        if let Some(model_call) = model_call {
                            let tool_content = match &result {
                                Ok(output) => sanitize_stream_chunk(
                                    &tool_result_content_for_api_message(app, &name, output),
                                ),
                                Err(err) => sanitize_stream_chunk(&format!("Error: {err}")),
                            };
                            app.push_api_message(Message {
                                role: Role::User,
                                content: vec![ContentBlock::ToolResult {
                                    execution_id: Some(id.clone()),
                                    tool_use_id: model_call.provider_id,
                                    content: tool_content,
                                    is_error: None,
                                    content_blocks: None,
                                }],
                            });
                        } else {
                            app.pending_tool_uses.retain(|block| {
                                block.tool_call_key()
                                    != Some(codewhale_models::ToolCallKey::Execution(&id))
                            });
                        }
                        handle_tool_call_complete(app, &id, &name, &result);
                        if name
                            == crate::tools::request_plugin_install::REQUEST_PLUGIN_INSTALL_TOOL_NAME
                            && let Ok(output) = &result
                            && output.success
                            && let Some(meta) = output.metadata.as_ref()
                        {
                            let plugin = meta
                                .get("plugin")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            let command = meta
                                .get("command")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            app.surface_plugin_review_request(plugin, command);
                        }
                        if flush_gate_receipts_for(app, Some(&id)) {
                            transcript_batch_updated = true;
                        }
                        if crate::mcp::McpPool::is_mcp_tool(&name)
                            && match &result {
                                Ok(output) => !output.success,
                                Err(_) => true,
                            }
                        {
                            let _ = app.maybe_show_behavioral_tip(
                                crate::tui::behavioral_tips::BehavioralTip::McpValidation,
                            );
                        }

                        // Every `remember` action mutates durable memory, so a
                        // successful call is the moment the first-run tip
                        // points at /memory (one-shot per session, lifetime-capped).
                        if name == "remember" && matches!(&result, Ok(output) if output.success) {
                            let _ = app.maybe_show_behavioral_tip(
                                crate::tui::behavioral_tips::BehavioralTip::DurableStateWritten,
                            );
                        }

                        if result.is_ok()
                            && is_work_graph_mutation_tool(&name)
                            && let Err(err) = persist_pending_work_checkpoint(app).await
                        {
                            tracing::warn!(
                                tool = %name,
                                error = %err,
                                "Work Graph checkpoint was not enqueued; projections remain unpublished"
                            );
                            app.status_message = Some(format!(
                                "To-do list update pending: checkpoint could not be queued ({err})"
                            ));
                        }

                        // Immediately refresh the task panel sidebar when a
                        // tool that changes task state completes, so the
                        // Tasks panel stays in sync with tool execution
                        // rather than waiting up to 2.5 s for the periodic
                        // poll. Also merge shell jobs (#373).
                        // Only tools that actually change durable tasks or
                        // background shell jobs force a jobs-panel refresh.
                        // Checklist/todo/plan tools drive the To-do panel,
                        // which reads `app.todos` directly and repaints on the
                        // normal redraw — no forced refresh needed (avoids the
                        // old per-checklist Tasks-panel churn).
                        if matches!(
                            name.as_str(),
                            "agent"
                                | "task_shell_start"
                                | "exec_shell"
                                | "exec_shell_cancel"
                                | "exec_shell_wait"
                                | "task_cancel"
                                // Unified durable-task tool (piagent phase B):
                                // create/cancel actions mutate task state, so
                                // any `tasks` completion refreshes the panel.
                                | "tasks"
                        ) {
                            refresh_active_task_panel(app, &task_manager).await;
                            last_task_refresh = Instant::now();
                        }
                        if matches!(name.as_str(), "agent") {
                            subagent_list_refresh_requested = true;
                        }
                    }
                    EngineEvent::TurnStarted { turn_id, route, .. } => {
                        app.prune_settled_workflow_runs();
                        // A prior turn that died without its `TurnComplete`
                        // must not leak its provisional estimate into this one.
                        app.clear_pending_turn_cost();
                        // A Deny is scoped to the turn it answered (UX-8).
                        end_turn_scoped_denials(app);
                        app.goal_continuation_waiting = false;
                        app.session.last_tool_request_snapshot = None;
                        app.ocean_completion_started_at = None;
                        app.ocean_receipt_settle_start = None;
                        app.ocean_turn_history_start = app.history.len();
                        app.pending_plan_handoff = None;
                        app.suppress_stream_events_until_turn_complete = false;
                        app.is_loading = true;
                        app.offline_mode = false;
                        app.turn_error_posted = false;
                        app.turn_error_notice = None;
                        app.lsp_repair = crate::tui::app::LspRepairState::default();
                        app.prompt_suggestion = None;
                        app.prompt_suggestion_gen
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        app.dispatch_started_at = None;
                        current_streaming_text.clear();
                        app.streaming_output_token_estimate = 0;
                        app.streaming_state.reset();
                        app.streaming_message_index = None;
                        app.streaming_thinking_active_entry = None;
                        stream_display_clock.reset();
                        let now = Instant::now();
                        app.turn_started_at = Some(now);
                        app.turn_last_activity_at = Some(now);
                        app.session.clear_pending_turn_usage();
                        app.streaming_output_token_estimate = 0;
                        app.provider_wait_incident_logged = false;
                        // Discoverability hint for users who don't know how
                        // to interrupt a long-running turn (#1367). Only
                        // surface when the status_message slot is empty so
                        // we don't trample over a real transient message
                        // (e.g. "/queue saved", "Selection copied"); the
                        // hint then auto-clears as soon as anything else
                        // updates the slot.
                        if app.status_message.is_none() {
                            app.status_message = Some("Press Esc or Ctrl+C to cancel".to_string());
                        }
                        active_translation_client = match route.as_ref() {
                            Some(route) => match exact_translation_client(config, route) {
                                Ok(client) => Some(client),
                                Err(error) => {
                                    tracing::warn!(
                                        "translation client rejected the frozen turn route: {error}"
                                    );
                                    None
                                }
                            },
                            None => fallback_translation_client.clone(),
                        };
                        active_translation_route = route;
                        app.runtime_turn_id = Some(turn_id);
                        app.runtime_turn_status = Some("in_progress".to_string());
                        app.turn_counter = app.turn_counter.saturating_add(1);
                        app.reasoning_buffer.clear();
                        app.reasoning_header = None;
                        app.last_reasoning = None;
                        app.pending_tool_uses.clear();
                        last_status_frame = Instant::now();
                        // Lifecycle outbox (`[lifecycle_outbox]`): the turn
                        // boundary the shell-hook system deliberately lacks.
                        // No-op when the feature is disabled.
                        app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
                            event: "turn_start".to_string(),
                            kind: "turn.started".to_string(),
                            thread_id: app.hooks.session_id().to_string(),
                            turn_id: app.runtime_turn_id.clone(),
                            item_id: None,
                            payload: serde_json::json!({
                                "model": codewhale_hooks::bounded_text(
                                    &app.model,
                                    codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
                                ),
                                "workspace": app.workspace.display().to_string(),
                            }),
                        });
                    }
                    EngineEvent::ToolRequestSnapshot { snapshot } => {
                        app.session.last_tool_request_snapshot = Some(snapshot);
                    }
                    // Runtime-API hosts record restore-point receipts on their
                    // turn records; the TUI's `/undo` resolves its own session's
                    // snapshots from the store.
                    EngineEvent::WorkspaceSnapshotTaken { .. } => {}
                    EngineEvent::RouteDispatched { turn_id, route } => {
                        if app.runtime_turn_id.as_deref() == Some(turn_id.as_str()) {
                            active_translation_client = match exact_translation_client(
                                config, &route,
                            ) {
                                Ok(client) => Some(client),
                                Err(error) => {
                                    tracing::warn!(
                                        "translation client rejected the dispatched turn route: {error}"
                                    );
                                    None
                                }
                            };
                            active_translation_route = Some(route);
                        }
                    }
                    EngineEvent::TurnComplete {
                        usage,
                        parent_route_usage,
                        routed_usage_dropped_records,
                        status,
                        error,
                        tool_catalog,
                        base_url,
                    } => {
                        // A decision whose tool never reported completion
                        // still gets its receipt before the turn closes.
                        if flush_gate_receipts_for(app, None) {
                            transcript_batch_updated = true;
                        }
                        // A steer the turn never accepted was dropped by the
                        // engine. Report it instead of leaving it "sending"
                        // (#6190).
                        crate::tui::ui::dispatch::settle_unaccepted_steers_at_turn_end(app);
                        let completed_turn = app.active_turn.take();
                        // TurnComplete carries no turn id. After a local
                        // cancel the UI is idle, so `is_loading` can only be
                        // true again because a newer dispatch set it: this
                        // terminal event belongs to the cancelled turn and
                        // must not close the newer one's loading, dispatch,
                        // status or unanswered-submission state, nor drain
                        // the queue ahead of it (U02-07). The engine runs one
                        // turn at a time, so the newer turn's own TurnStarted
                        // and TurnComplete follow this event.
                        let newer_dispatch_owns_turn_state =
                            app.suppress_stream_events_until_turn_complete && app.is_loading;
                        if !newer_dispatch_owns_turn_state {
                            app.unanswered_submission = None;
                        }
                        // The in-flight provisional estimate hands off to the
                        // authoritative cumulative price accrued below; the
                        // high-water mark keeps the displayed total monotonic
                        // through the swap (#244).
                        app.clear_pending_turn_cost();
                        app.session.clear_pending_turn_usage();
                        app.session.last_tool_catalog = tool_catalog;
                        // The endpoint this turn's client actually used. Kept
                        // separately from the mutable session/config surfaces
                        // so the prompt-suggestion gate below can require it.
                        let turn_actual_base_url = base_url.clone();
                        app.session.last_base_url = base_url;
                        let was_locally_cancelled = app.suppress_stream_events_until_turn_complete;
                        app.suppress_stream_events_until_turn_complete = false;
                        app.active_allowed_tools = None;
                        if app.paused_goal_objective.is_none() {
                            app.pausable = false;
                            app.paused = false;
                        }
                        // Turn completion is an ordinary state transition.
                        // Clearing all 7,900 cells after a long stream was the
                        // visible end-of-turn flash in the rejected build.
                        // Ratatui's diff is sufficient here; full repaints stay
                        // reserved for real terminal boundary changes (resize,
                        // focus recovery, theme, child-terminal return).
                        // Finalize any in-flight tool group. Cancellation
                        // marks still-running entries as Failed so the user
                        // sees they were interrupted rather than the spinner
                        // hanging forever.
                        if matches!(
                            status,
                            crate::core::events::TurnOutcomeStatus::Interrupted
                                | crate::core::events::TurnOutcomeStatus::Failed
                        ) {
                            app.finalize_active_cell_as_interrupted();
                            // Also mark the streaming Assistant cell (if any)
                            // so partial reasoning/text isn't left with a
                            // permanent spinner. Idempotent with the
                            // optimistic call in the Esc handler.
                            app.finalize_streaming_assistant_as_interrupted();
                        } else {
                            app.flush_active_cell();
                        }
                        if !newer_dispatch_owns_turn_state {
                            app.is_loading = false;
                            app.dispatch_started_at = None;
                        }
                        app.pending_provider_switch = None;
                        app.offline_mode = false;
                        app.streaming_state.reset();
                        stream_display_clock.reset();
                        if was_locally_cancelled {
                            current_streaming_text.clear();
                        }
                        // Capture elapsed before clearing turn_started_at so
                        // notifications can use the real wall-clock duration.
                        let turn_elapsed =
                            app.turn_started_at.map(|t| t.elapsed()).unwrap_or_default();
                        app.turn_started_at = None;
                        app.turn_last_activity_at = None;
                        app.streaming_output_token_estimate = 0;
                        // Roll the just-finished turn's elapsed time into the
                        // cumulative session work-time (#448 follow-up). The
                        // footer's `worked Nh Mm` chip reads this so the
                        // label reflects actual model work, not idle
                        // uptime since launch.
                        app.cumulative_turn_duration =
                            app.cumulative_turn_duration.saturating_add(turn_elapsed);
                        // A turn that ended with tools still open (interrupt,
                        // failure) must not carry their timers forward.
                        app.session_metrics.clear_in_flight();
                        // Stream lock applies per-turn; clear it so the next
                        // turn's chunks pull the view down again until the
                        // user opts out by scrolling up.
                        app.user_scrolled_during_stream = false;
                        let turn_status_label = match status {
                            crate::core::events::TurnOutcomeStatus::Completed => {
                                app.ocean_completion_started_at = Some(Instant::now());
                                app.ocean_receipt_settle_start =
                                    Some(app.ocean_turn_history_start.min(app.history.len()));
                                "completed".to_string()
                            }
                            crate::core::events::TurnOutcomeStatus::Interrupted => {
                                app.ocean_completion_started_at = None;
                                app.ocean_receipt_settle_start = None;
                                "interrupted".to_string()
                            }
                            crate::core::events::TurnOutcomeStatus::Failed => {
                                app.ocean_completion_started_at = None;
                                app.ocean_receipt_settle_start = None;
                                "failed".to_string()
                            }
                        };
                        if !newer_dispatch_owns_turn_state {
                            app.runtime_turn_status = Some(turn_status_label.clone());
                        }
                        if matches!(
                            status,
                            crate::core::events::TurnOutcomeStatus::Interrupted
                                | crate::core::events::TurnOutcomeStatus::Failed
                        ) {
                            subagent_list_refresh_requested = true;
                        }
                        // #6004: only a turn that *ended* failed is a session
                        // error; transient tool failures the agent absorbed
                        // never fire it.
                        if matches!(status, crate::core::events::TurnOutcomeStatus::Failed) {
                            execute_session_error_hook(app, error.as_deref());
                        }
                        crate::tui::notifications::clear_taskbar_progress();
                        if status != crate::core::events::TurnOutcomeStatus::Completed {
                            crate::retry_status::clear();
                            crate::tui::notifications::stop_title_animation_quietly();
                        }
                        let turn_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
                        app.session.total_tokens =
                            app.session.total_tokens.saturating_add(turn_tokens);
                        app.session.total_conversation_tokens = app
                            .session
                            .total_conversation_tokens
                            .saturating_add(turn_tokens);
                        app.session.total_input_tokens = app
                            .session
                            .total_input_tokens
                            .saturating_add(usage.input_tokens);
                        app.session.total_output_tokens = app
                            .session
                            .total_output_tokens
                            .saturating_add(usage.output_tokens);
                        // Only accumulate cache telemetry when the provider
                        // reported at least one cache class. Use pricing's
                        // canonical mutually-exclusive hit/miss/write split so
                        // cache writes are never counted again as misses.
                        if usage.prompt_cache_hit_tokens.is_some()
                            || usage.prompt_cache_miss_tokens.is_some()
                            || usage.prompt_cache_write_tokens.is_some()
                        {
                            let classes = crate::pricing::token_usage_for_pricing(&usage);
                            let hit_tokens = u32::try_from(classes.cache_read).unwrap_or(u32::MAX);
                            let miss_tokens = u32::try_from(classes.input).unwrap_or(u32::MAX);
                            let write_tokens =
                                u32::try_from(classes.cache_write).unwrap_or(u32::MAX);
                            app.session.total_cache_hit_tokens = app
                                .session
                                .total_cache_hit_tokens
                                .saturating_add(hit_tokens);
                            app.session.total_cache_miss_tokens = app
                                .session
                                .total_cache_miss_tokens
                                .saturating_add(miss_tokens);
                            app.session.total_cache_write_tokens = app
                                .session
                                .total_cache_write_tokens
                                .saturating_add(write_tokens);
                        }
                        app.session.last_prompt_tokens = Some(usage.input_tokens);
                        app.session.last_completion_tokens = Some(usage.output_tokens);
                        app.session.last_prompt_cache_hit_tokens = usage.prompt_cache_hit_tokens;
                        app.session.last_prompt_cache_miss_tokens = usage.prompt_cache_miss_tokens;
                        app.session.last_reasoning_replay_tokens = usage.reasoning_replay_tokens;
                        let (provider, provider_identity, model, auto_model) = completed_turn
                            .as_ref()
                            .and_then(|turn| turn.route.as_ref())
                            .map(|route| {
                                (
                                    Some(route.provider),
                                    Some(route.provider_identity.clone()),
                                    Some(route.model.clone()),
                                    route.auto_model,
                                )
                            })
                            .unwrap_or((None, None, None, false));
                        let effective_turn_provider = provider.unwrap_or(app.api_provider);
                        let effective_turn_model = model
                            .as_deref()
                            .filter(|model| !model.trim().is_empty())
                            .unwrap_or_else(|| {
                                app.last_effective_model.as_deref().unwrap_or(&app.model)
                            })
                            .to_string();
                        app.last_effective_provider = Some(effective_turn_provider);
                        app.last_effective_provider_identity = provider_identity.clone();
                        if completed_turn
                            .as_ref()
                            .and_then(|turn| turn.route.as_ref())
                            .is_some_and(|route| route.auto_model)
                        {
                            app.last_auto_route_receipt = completed_turn
                                .as_ref()
                                .and_then(|turn| turn.auto_route_receipt.clone());
                        } else if completed_turn
                            .as_ref()
                            .is_some_and(|turn| turn.route.is_some())
                        {
                            app.last_auto_route_receipt = None;
                        }
                        if status == crate::core::events::TurnOutcomeStatus::Completed
                            && let Some(receipt) = completed_turn
                                .as_ref()
                                .and_then(|turn| turn.route.as_ref())
                                .and_then(|route| route.receipt.as_ref())
                            && config
                                .verify_provider_identity(receipt.admitted_identity())
                                .is_ok()
                            && receipt.endpoint_identity()
                                == crate::route_receipt::endpoint_identity(
                                    &config.base_url_for_route(receipt.admitted_identity()),
                                )
                        {
                            app.provider_health.record_success(
                                config,
                                receipt,
                                &effective_turn_model,
                            );
                        }
                        if auto_model {
                            app.last_effective_model = Some(effective_turn_model.clone());
                        }
                        // Price the turn exactly once. The same audit feeds the
                        // session total, the `/cache` row, and the `/cost`
                        // completeness counters, so those three surfaces can
                        // never disagree about what was counted (#4318).
                        let cost_audit = completed_turn
                            .as_ref()
                            .and_then(|turn| turn.route.as_ref())
                            .and_then(crate::core::events::TurnRoute::cost_envelope)
                            .map(|route| route.audit(&parent_route_usage));
                        app.push_turn_cache_record(crate::tui::app::TurnCacheRecord {
                            provider,
                            provider_identity,
                            model,
                            auto_model,
                            input_tokens: parent_route_usage.input_tokens,
                            output_tokens: parent_route_usage.output_tokens,
                            cache_hit_tokens: parent_route_usage.prompt_cache_hit_tokens,
                            cache_miss_tokens: parent_route_usage.prompt_cache_miss_tokens,
                            reasoning_replay_tokens: parent_route_usage.reasoning_replay_tokens,
                            cache_write_tokens: parent_route_usage.prompt_cache_write_tokens,
                            reasoning_tokens: parent_route_usage.reasoning_tokens,
                            cost_audit: cost_audit.clone(),
                            recorded_at: Instant::now(),
                        });
                        app.retire_action_notices(None);
                        present_turn_failure(app, status, error.as_deref());

                        // Update session cost, and record what the total does
                        // *not* cover so `/cost` can stay honest about it.
                        //
                        // `cost_audit` above came from `cost_envelope()`, i.e.
                        // the billing envelope frozen at CodeWhale's
                        // pre-permit application-dispatch boundary and
                        // classified from this turn's frozen receipt. It
                        // is `None` for a route that was never dispatched, and
                        // a route whose receipt named no product classified as
                        // Unknown — either way nothing accrues. A `/provider`
                        // or custom-table switch since dispatch cannot
                        // retro-bill this turn onto another route, because no
                        // ambient `Config` is read here at all.
                        let turn_cost = cost_audit.as_ref().and_then(|audit| audit.estimate);
                        if let Some(audit) = cost_audit.as_ref() {
                            app.record_turn_cost_audit(audit);
                            // Redacted receipt for the route this money came
                            // from: provider identity, wire model, billing
                            // surface, and the endpoint *fingerprint* — never the
                            // URL or any credential.
                            if let Some(receipt) =
                                completed_turn_cost_route_receipt(completed_turn.as_ref(), audit)
                            {
                                app.record_turn_cost_route_receipt(receipt);
                            }
                        }
                        if let Some(cost) = turn_cost {
                            app.accrue_session_cost_estimate(cost);
                        }
                        if routed_usage_dropped_records > 0 {
                            let dropped =
                                u32::try_from(routed_usage_dropped_records).unwrap_or(u32::MAX);
                            app.session.cost_unpriced_turns =
                                app.session.cost_unpriced_turns.saturating_add(dropped);
                            app.session.cost_cny_unpriced_turns =
                                app.session.cost_cny_unpriced_turns.saturating_add(dropped);
                            app.session
                                .cost_unpriced_reasons
                                .insert("routed_usage_receipt_missing".to_string());
                            app.session
                                .cost_cny_unpriced_reasons
                                .insert("routed_usage_receipt_missing".to_string());
                        }

                        // The parent turn is idle now (#6565).
                        settle_background_finished_at_turn_end(
                            app,
                            config,
                            status == crate::core::events::TurnOutcomeStatus::Completed,
                        );

                        // Emit OSC 9 / BEL desktop notification for long turns, and
                        // always stop the title animation that began on TurnStarted.
                        if status == crate::core::events::TurnOutcomeStatus::Completed {
                            if let Some((method, threshold, include_summary)) =
                                notifications::settings(config)
                            {
                                let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                                let payload = notifications::completed_turn_payload(
                                    app,
                                    &current_streaming_text,
                                    include_summary,
                                    turn_elapsed,
                                    turn_cost,
                                );
                                crate::tui::notifications::notify_done(
                                    method,
                                    in_tmux,
                                    &payload,
                                    threshold,
                                    turn_elapsed,
                                );
                                crate::tui::notifications::stop_title_animation();
                            } else {
                                crate::tui::notifications::stop_title_animation_quietly();
                            }
                        }

                        // Generate ghost-text follow-up suggestion asynchronously.
                        //
                        // Privacy (#4404/#4411): the request is anchored to the
                        // completed turn's route snapshot and to the receipt the
                        // engine minted from the client it installed for that
                        // turn — never to live UI selection, and never to
                        // authority re-derived from mutable config.
                        // Conversation context is only ever sent to that exact
                        // endpoint with that exact credential. Providers whose
                        // wire shape this helper does not speak produce no
                        // background request at all — and never reach another
                        // provider's credentials while deciding that.
                        let suggestion_launch = completed_turn
                            .as_ref()
                            .and_then(|turn| {
                                let route = turn.route.as_ref()?;
                                let authority = turn.suggestion_authority.as_ref()?;
                                Some(crate::tui::prompt_suggestion::SuggestionRouteSnapshot {
                                    provider: route.provider,
                                    provider_identity: route.provider_identity.as_str(),
                                    model: route.model.as_str(),
                                    authority,
                                    actual_base_url: turn_actual_base_url.as_deref(),
                                })
                            })
                            .and_then(|snapshot| {
                                crate::tui::prompt_suggestion::plan_suggestion_launch_with_config(
                                    config,
                                    status == crate::core::events::TurnOutcomeStatus::Completed,
                                    config.prompt_suggestion_enabled(),
                                    app.api_messages.len(),
                                    Some(snapshot),
                                )
                            });
                        if let Some(launch) = suggestion_launch {
                            let suggestion_cell = app.prompt_suggestion_cell.clone();
                            let messages: std::sync::Arc<Vec<codewhale_models::Message>> =
                                app.api_messages.clone();
                            let gen_token = app
                                .prompt_suggestion_gen
                                .load(std::sync::atomic::Ordering::Relaxed);
                            tokio::spawn(async move {
                                let summary =
                                    crate::tui::prompt_suggestion::summarize_recent_messages(
                                        &messages, 8,
                                    );
                                if let Some(suggestion) =
                                    crate::tui::prompt_suggestion::generate_suggestion(
                                        &launch.api_key,
                                        &launch.base_url,
                                        &launch.model,
                                        &summary,
                                        launch.openrouter_vendor.as_deref(),
                                    )
                                    .await
                                    && let Ok(mut guard) = suggestion_cell.lock()
                                {
                                    *guard = Some((gen_token, suggestion));
                                }
                            });
                        }

                        // Generate post-turn receipt for completed turns.
                        // Also push a persistent status toast so users always
                        // see the outcome in the footer (not just the 8-second
                        // composer receipt), regardless of notification method
                        // or platform.
                        if status == crate::core::events::TurnOutcomeStatus::Completed {
                            let tool_count = app.tool_evidence.len();
                            let mut receipt = "✓ turn completed".to_string();
                            if tool_count > 0 {
                                let _ = write!(receipt, " · {tool_count} tool(s) used");
                                for evidence in &app.tool_evidence {
                                    let summary = crate::utils::truncate_with_ellipsis(
                                        &evidence.summary,
                                        60,
                                        "…",
                                    );
                                    let _ = write!(receipt, " · {}: {summary}", evidence.tool_name);
                                }
                            }
                            app.set_receipt_text(receipt.clone());
                            // Mirror as a persistent status toast (10s TTL).
                            // The footer bar visibly shows status toasts,
                            // which is more glanceable than the composer
                            // border receipt alone.
                            app.push_status_toast(
                                receipt,
                                crate::tui::app::StatusToastLevel::Info,
                                Some(10_000),
                            );
                        }

                        // Auto-save completed turn and clear crash checkpoint.
                        // Offloaded to the persistence actor so the UI
                        // stays responsive.
                        if let Ok(manager) = SessionManager::default_location()
                            && let Ok(session) = build_session_snapshot(app, &manager)
                        {
                            app.current_session_id = Some(session.metadata.id.clone());
                            // Compound completion commit: the actor writes the
                            // completed snapshot and clears this session's
                            // crash checkpoint only after that write succeeds.
                            // A failed save now retains the checkpoint as the
                            // sole recovery record instead of erasing it.
                            let queued =
                                persistence_actor::try_persist(PersistRequest::CompletedCommit {
                                    session,
                                });
                            if queued {
                                if let Err(err) = publish_pending_work_projection(app).await {
                                    tracing::warn!(
                                        error = %err,
                                        "completed-turn Work projections remain unpublished"
                                    );
                                    app.status_message = Some(format!(
                                        "Session queued, but Work views could not publish ({err})"
                                    ));
                                }
                            } else if app
                                .runtime_services
                                .work
                                .as_ref()
                                .is_some_and(|work| work.has_pending_publish())
                            {
                                app.status_message = Some(
                                    "To-do list update pending: session snapshot could not be queued"
                                        .to_string(),
                                );
                            }
                        }
                        // The checkpoint clear is owned by the compound
                        // `CompletedCommit` above: it applies only after this
                        // session's snapshot safely landed. When the snapshot
                        // could not be built or queued, the in-flight
                        // checkpoint survives for startup recovery review.

                        // Refresh prepaid remaining credit after each completed
                        // turn so the footer balance chip stays current without
                        // adding latency to any request path.
                        let api_key = config.active_route_api_key().unwrap_or_default();
                        let base_url = config.active_route_base_url();
                        schedule_balance_fetch(app, &api_key, &base_url, false);

                        // Legacy pending-steer recovery. Current keyboard
                        // handling keeps Esc as cancel-only, but older saved
                        // state may still carry pending steers.
                        if status == crate::core::events::TurnOutcomeStatus::Interrupted
                            && app.submit_pending_steers_after_interrupt
                        {
                            if let Some(merged) = merge_pending_steers(&mut *app) {
                                queued_to_send = Some(merged);
                            }
                        } else if status == crate::core::events::TurnOutcomeStatus::Failed
                            && !app.pending_steers.is_empty()
                        {
                            // Hard-fail recovery: if the engine failed before
                            // a clean Interrupted landed, demote pending
                            // steers to the visible queue so they're not
                            // silently lost. User can /queue to inspect.
                            for msg in app.drain_pending_steers() {
                                app.queue_message(msg);
                            }
                        }

                        // Counted here, at the caller, never inside
                        // `execute_turn_end_observer_hook`: that function's
                        // first statement returns early for anyone with no
                        // TurnEnd hooks, and the natural future optimization
                        // hoists that check up to this call site — which would
                        // silently zero the counter for every user who does
                        // not use hooks.
                        {
                            let telemetry = codewhale_telemetry::session_counters();
                            telemetry.bump(codewhale_telemetry::Counter::Turns);
                            telemetry.observe_turn_secs(turn_elapsed.as_secs());
                        }

                        if let Err(error) = execute_turn_end_observer_hook(
                            app,
                            completed_turn.as_ref(),
                            &usage,
                            completed_turn
                                .as_ref()
                                .and_then(|turn| turn.route.as_ref())
                                .and_then(|route| route.billing.as_ref())
                                .and_then(|billing| billing.billing_surface.as_deref()),
                            turn_elapsed,
                            error.as_deref(),
                        ) {
                            surface_observer_hook_submission_failure(app, error);
                        }

                        // Lifecycle outbox (`[lifecycle_outbox]`): one
                        // `turn_end` event per completed turn, with the kind
                        // projected from the turn status — `turn.failed` for
                        // failed turns, `turn.completed` for completed ones,
                        // `turn.interrupted` for locally cancelled ones.
                        // No-op when the feature is disabled.
                        {
                            let outbox_status = turn_status_label.as_str();
                            let kind = match outbox_status {
                                "completed" => "turn.completed",
                                "failed" => "turn.failed",
                                "interrupted" => "turn.interrupted",
                                _ => "turn.ended",
                            };
                            app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
                                event: "turn_end".to_string(),
                                kind: kind.to_string(),
                                thread_id: app.hooks.session_id().to_string(),
                                turn_id: app.runtime_turn_id.clone(),
                                item_id: None,
                                payload: serde_json::json!({
                                    "status": outbox_status,
                                    "duration_ms": turn_elapsed.as_millis() as u64,
                                    "workspace": app.workspace.display().to_string(),
                                    "error": error
                                        .as_deref()
                                        .map(|message| codewhale_hooks::bounded_text(
                                            message,
                                            codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
                                        )),
                                }),
                            });
                        }

                        // Plan hand-off freezes this successful turn's exact
                        // completed response, including prose-only plans.
                        // Only on an idle screen — a modal that takes digit
                        // keys must never land on a draft, a queued follow-up
                        // or another open view, and the answer goes to this
                        // session, not to a focused agent.
                        if queued_to_send.is_none()
                            && !newer_dispatch_owns_turn_state
                            && !app.is_loading
                            && !app.dispatch_in_flight
                            && app.pending_steers.is_empty()
                            && !app.remote_control.runtime_chat_blocks_local_dispatch()
                            && app.queued_message_count() == 0
                            && app.queued_draft.is_none()
                            && app.input.is_empty()
                            && app.view_stack.is_empty()
                            && app.agent_focus.is_none()
                        {
                            let todos = app.todos.lock().await.snapshot();
                            let has_open_todos =
                                todos.items.iter().any(|item| !item.status.is_settled());
                            if !was_locally_cancelled
                                && let Some(request_id) = app.prepare_plan_handoff(
                                    status,
                                    completed_turn.as_ref().map(|turn| turn.turn_id.as_str()),
                                    has_open_todos,
                                )
                            {
                                app.view_stack.push(UserInputView::new(
                                    request_id,
                                    crate::tui::plan_handoff::request(app.ui_locale),
                                ));
                                app.needs_redraw = true;
                            }
                        }

                        if queued_to_send.is_none() && !newer_dispatch_owns_turn_state {
                            queued_to_send = app.pop_queued_message();
                        }
                    }
                    EngineEvent::Error {
                        envelope,
                        recoverable: _,
                    } => {
                        let provider_before_error = app.api_provider;
                        let identity_before_error = app.admitted_provider_identity().ok().cloned();
                        let fallback_chain_before_error = app.provider_chain.clone();
                        if let Some((identity, health_model)) = error_health_route(app)
                            && config.verify_provider_identity(&identity).is_ok()
                            && app
                                .active_turn
                                .as_ref()
                                .and_then(|turn| turn.route.as_ref())
                                .and_then(|route| route.receipt.as_ref())
                                .is_some_and(|receipt| {
                                    receipt.endpoint_identity()
                                        == crate::route_receipt::endpoint_identity(
                                            &config.base_url_for_route(&identity),
                                        )
                                })
                        {
                            app.provider_health.record_failure(
                                config,
                                app.active_turn
                                    .as_ref()
                                    .and_then(|turn| turn.route.as_ref())
                                    .and_then(|route| route.receipt.as_ref())
                                    .expect("health route has captured receipt"),
                                &health_model,
                                &envelope,
                            );
                        }
                        let rollback_after_auth_failure =
                            matches!(
                                envelope.category,
                                crate::error_taxonomy::ErrorCategory::Authentication
                            ) && app.pending_provider_switch.is_some();
                        apply_engine_error_to_app(app, envelope);
                        if app.api_provider != provider_before_error
                            && app.is_fallback_active()
                            && let Some(identity_before_error) = identity_before_error
                        {
                            // Several queued errors can be drained together.
                            // The first route remains the rollback authority;
                            // later chain advances must not overwrite it with
                            // an enum/key pair from the half-applied fallback.
                            fallback_after_engine_error.get_or_insert(ProviderFallbackRollback {
                                identity: identity_before_error,
                                chain: fallback_chain_before_error,
                            });
                        }
                        if rollback_after_auth_failure
                            && let Some(rollback_warning) =
                                rollback_provider_after_auth_failure(app, config)
                        {
                            respawn_after_provider_rollback = Some(rollback_warning);
                        }
                    }
                    EngineEvent::Status { message } => {
                        transcript_batch_updated |= apply_engine_status(app, message);
                    }
                    EngineEvent::ToolProjectionWarning {
                        provider,
                        omitted_tool_names,
                        omitted_tool_count,
                    } => {
                        let tools = crate::core::events::tool_projection_warning_tool_list(
                            &omitted_tool_names,
                            omitted_tool_count,
                        );
                        let message = app
                            .tr(MessageId::ToolProjectionWarning)
                            .replace("{provider}", &provider)
                            .replace("{tools}", &tools);
                        app.push_status_toast(message, StatusToastLevel::Warning, Some(12_000));
                    }
                    EngineEvent::SnapshotsDisabled { reason, .. } => {
                        // Undo is silently off otherwise: the engine's stderr
                        // notice never reaches the alternate screen (#5930).
                        // The engine already rendered the one localized line;
                        // show it once as a toast and leave the durable copy
                        // to `/status` rather than pinning it in the
                        // transcript too (#6042).
                        app.push_status_toast(reason, StatusToastLevel::Warning, Some(12_000));
                    }
                    EngineEvent::McpSessionBoot {
                        generation,
                        snapshot,
                        connecting,
                        finished,
                    } => {
                        apply_mcp_session_boot_event(
                            app, generation, snapshot, connecting, finished,
                        );
                    }
                    EngineEvent::RequestManifestReady { rendered } => {
                        // Typed manifest text, or the explicitly requested
                        // base-prompt-only disclosure. Rendered as a system cell.
                        app.add_message(HistoryCell::System { content: rendered });
                        transcript_batch_updated = true;
                    }
                    EngineEvent::GoalUpdated { snapshot } => {
                        if apply_goal_snapshot_to_app(app, &snapshot) {
                            transcript_batch_updated = true;
                            if let Err(error) = persist_current_session_goal(app) {
                                surface_goal_persistence_failure(app, &error);
                            }
                        }
                    }
                    EngineEvent::GoalContinuationWaiting { delay_seconds } => {
                        app.goal_continuation_waiting = true;
                        let delay =
                            codewhale_command_contract::elapsed::format_elapsed_secs(delay_seconds);
                        app.status_message = Some(
                            app.tr(MessageId::GoalContinuationWaiting)
                                .replace("{delay}", &delay),
                        );
                    }
                    EngineEvent::GoalContinuationWaitEnded { interrupted } => {
                        app.goal_continuation_waiting = false;
                        let message_id = if interrupted {
                            MessageId::GoalContinuationStopped
                        } else {
                            MessageId::GoalContinuationReady
                        };
                        app.status_message = Some(app.tr(message_id).to_string());
                    }
                    event @ EngineEvent::SessionUpdated { .. } => {
                        apply_engine_session_projection(app, config, event);
                    }
                    EngineEvent::CompactionStarted { id, auto, .. } => {
                        apply_compaction_started(app, id, auto);
                    }
                    EngineEvent::CompactionCompleted {
                        id,
                        auto,
                        message,
                        messages_before,
                        messages_after,
                        summary_prompt,
                        ..
                    } => {
                        apply_compaction_completed(
                            app,
                            &id,
                            auto,
                            message,
                            messages_before,
                            messages_after,
                            summary_prompt,
                        );
                    }
                    EngineEvent::CompactionCancelled { id, auto, message } => {
                        apply_compaction_cancelled(app, &id, auto, message);
                    }
                    EngineEvent::CompactionFailed { id, auto, message } => {
                        apply_compaction_failed(app, &id, auto, message);
                    }
                    EngineEvent::PurgeStarted { message } => {
                        app.is_purging = true;
                        app.status_message = Some(message);
                    }
                    EngineEvent::PurgeCompleted { message, .. } => {
                        app.is_purging = false;
                        app.status_message = Some(message);
                    }
                    EngineEvent::PurgeFailed { message } => {
                        app.is_purging = false;
                        app.status_message = Some(message);
                    }
                    EngineEvent::PrefixCacheChange {
                        description,
                        stability_pct,
                        changed,
                        pinned_combined_hash,
                        pin_reason,
                        last_miss_reason,
                        context_updates,
                        ..
                    } => {
                        app.prefix_context_updates = context_updates;
                        app.prefix_checks_total = app.prefix_checks_total.saturating_add(1);
                        app.prefix_stability_pct = Some(stability_pct);
                        app.last_pinned_prefix_hash =
                            (!pinned_combined_hash.is_empty()).then_some(pinned_combined_hash);
                        app.prefix_pin_reason = (!pin_reason.is_empty()).then_some(pin_reason);
                        // A declared re-pin or reset is an expected miss, not a
                        // silent-cache-death drift; only an undeclared drift is
                        // a real problem.
                        let is_drift = description.starts_with("drift");
                        app.prefix_last_miss_reason =
                            (!last_miss_reason.is_empty()).then_some(last_miss_reason);
                        if changed {
                            app.prefix_change_count = app.prefix_change_count.saturating_add(1);
                            if is_drift {
                                app.prefix_drift_count = app.prefix_drift_count.saturating_add(1);
                            }
                            if !description.is_empty() {
                                app.last_prefix_change_desc = Some(description);
                            }
                        }
                    }
                    EngineEvent::LspRepairUpdate {
                        diagnostics_found,
                        files,
                        injected,
                    } => {
                        let repair = &mut app.lsp_repair;
                        repair.diagnostics_found =
                            repair.diagnostics_found.saturating_add(diagnostics_found);
                        repair.files_touched = repair.files_touched.saturating_add(files);
                        if injected {
                            // Injection itself is not a repair attempt — the model
                            // has only been shown the diagnostics so far (#4107).
                            repair.injected = true;
                            if repair.latest == "unavailable" || repair.latest.is_empty() {
                                repair.latest = "unknown";
                            }
                        } else if repair.injected {
                            // Diagnostics after a prior injection imply the model
                            // edited again (a repair attempt). Zero findings = resolved.
                            repair.repair_attempted = true;
                            repair.latest = if diagnostics_found == 0 {
                                "resolved"
                            } else {
                                "still_failing"
                            };
                        } else {
                            repair.latest = "unknown";
                        }
                    }
                    EngineEvent::PauseEvents { ack } => {
                        if !event_broker.is_paused() {
                            let input_handoff =
                                match terminal_input.pause_for_child_terminal().await {
                                    Ok(()) => prepare_terminal_input_handoff(
                                        &terminal_input,
                                        &mut pending_terminal_events,
                                    ),
                                    Err(err) => Err(err),
                                };
                            match input_handoff {
                                Ok(true) => {}
                                Ok(false) => {
                                    terminal_input.resume_after_child_terminal();
                                    tracing::debug!(
                                        "refusing interactive child because cancellation input is pending"
                                    );
                                    // Preserve Esc/Ctrl+C for the ordinary
                                    // key path and withhold the ack so the
                                    // child cannot race ahead of cancellation.
                                    continue;
                                }
                                Err(err) => {
                                    terminal_input.resume_after_child_terminal();
                                    tracing::warn!(
                                        error = %err,
                                        "refusing interactive child after terminal input handoff failed"
                                    );
                                    let recovery = match terminal_input.restart_detached() {
                                        Ok(()) => "Terminal input recovered.".to_string(),
                                        Err(restart_err) => {
                                            tracing::warn!(
                                                error = %restart_err,
                                                "failed to restart terminal input after handoff refusal"
                                            );
                                            format!(
                                                "Terminal input recovery also failed ({restart_err}); restart Codewhale if keys stop responding."
                                            )
                                        }
                                    };
                                    app.push_status_toast(
                                        format!(
                                            "Interactive terminal handoff refused ({err}). {recovery}"
                                        ),
                                        StatusToastLevel::Error,
                                        None,
                                    );
                                    app.needs_redraw = true;
                                    last_terminal_input_recovery = Instant::now();
                                    // Do not acknowledge the pause. The
                                    // engine guard times out, refuses the
                                    // child, and queues a harmless resume.
                                    continue;
                                }
                            }
                            if let Err(err) = pause_terminal(
                                terminal,
                                app.use_alt_screen(),
                                app.use_mouse_capture,
                                app.use_bracketed_paste,
                            ) {
                                terminal_input.resume_after_child_terminal();
                                tracing::warn!(
                                    error = %err,
                                    "refusing interactive child after terminal mode handoff failed"
                                );
                                resume_terminal(
                                    terminal,
                                    app.use_alt_screen(),
                                    app.use_mouse_capture,
                                    app.use_bracketed_paste,
                                    app.synchronized_output_enabled,
                                )
                                .with_context(|| {
                                    format!(
                                        "terminal handoff failed ({err}) and Codewhale could not restore terminal controls"
                                    )
                                })?;
                                app.push_status_toast(
                                    format!("Interactive terminal handoff refused ({err})."),
                                    StatusToastLevel::Error,
                                    None,
                                );
                                app.needs_redraw = true;
                                force_terminal_repaint = true;
                                // As above, withholding the acknowledgement
                                // keeps the child from launching.
                                continue;
                            }
                            event_broker.pause_events();
                            terminal_paused_at = Some(Instant::now());
                        }
                        if let Some(ack) = ack {
                            ack.notify_one();
                        }
                    }
                    EngineEvent::ResumeEvents => {
                        if event_broker.is_paused() {
                            resume_terminal(
                                terminal,
                                app.use_alt_screen(),
                                app.use_mouse_capture,
                                app.use_bracketed_paste,
                                app.synchronized_output_enabled,
                            )?;
                            event_broker.resume_events();
                            terminal_input.resume_after_child_terminal();
                            terminal_paused_at = None;
                        }
                    }
                    EngineEvent::AgentSpawned {
                        owner_session_id,
                        id,
                        prompt,
                        worker_status,
                        parent_run_id,
                        spawn_depth,
                        model,
                        route_source: _,
                        display_name,
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        let prompt_summary = bound_agent_activity_text(&prompt);
                        app.agent_progress
                            .insert(id.clone(), format!("starting: {prompt_summary}"));
                        let meta = app.agent_progress_meta.entry(id.clone()).or_default();
                        meta.parent_run_id = parent_run_id;
                        meta.spawn_depth = spawn_depth;
                        // The engine's name for the child, before any snapshot
                        // arrives, so the first label is already the right one.
                        meta.display_name = display_name;
                        meta.current_activity = worker_status.map(|status| {
                            AgentCurrentActivity::bounded(
                                status.into(),
                                Some(prompt_summary.clone()),
                                None,
                                None,
                            )
                        });
                        meta.current_tool = None;
                        record_agent_spawned_route(app, &id, &model);
                        if app.agent_activity_started_at.is_none() {
                            app.agent_activity_started_at = Some(Instant::now());
                        }
                        // #3030: Assign a stable user-facing label for this
                        // agent and keep the raw id out of the status bar.
                        apply_agent_spawned_status_and_observer(app, &id, &prompt, &prompt_summary);
                        subagent_list_refresh_requested = true;
                    }
                    EngineEvent::AgentProgress {
                        owner_session_id,
                        id,
                        status,
                        activity,
                        parent_run_id,
                        spawn_depth,
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        let display = bound_agent_activity_text(&friendly_subagent_progress(
                            app,
                            &id,
                            &status,
                            activity.routine_wait,
                        ));
                        if activity.routine_wait {
                            app.agent_progress
                                .entry(id.clone())
                                .or_insert_with(|| display.clone());
                        } else {
                            app.agent_progress.insert(id.clone(), display.clone());
                        }
                        let meta = app.agent_progress_meta.entry(id.clone()).or_default();
                        meta.parent_run_id = parent_run_id;
                        meta.spawn_depth = spawn_depth;
                        let current_tool = activity
                            .tool_name
                            .as_deref()
                            .map(subagent_progress_tool_display_name)
                            .map(str::to_string);
                        meta.current_activity = Some(AgentCurrentActivity::bounded(
                            activity.worker_status.into(),
                            Some(display.clone()),
                            current_tool.clone(),
                            activity.step,
                        ));
                        meta.current_tool = current_tool;
                        if app.agent_activity_started_at.is_none() {
                            app.agent_activity_started_at = Some(Instant::now());
                        }
                        // #3030: progress can arrive before AgentSpawned is
                        // observed — assign the stable label on first sight.
                        // The label and the step stay on the agent row. They
                        // used to overwrite the parent status line, so every
                        // child tool call looked like the turn being watched
                        // (#6565).
                        let _ = app.ensure_agent_label(&id);
                        // A progress-first agent (its AgentSpawned was dropped
                        // under channel pressure) exists only in agent_progress
                        // until a ListSubAgents refresh promotes it into
                        // subagent_cache. Request that refresh like the
                        // AgentSpawned arm does, so the sidebar row survives
                        // reconciliation instead of flickering out.
                        if !app.subagent_cache.iter().any(|agent| agent.agent_id == id) {
                            subagent_list_refresh_requested = true;
                        }
                        // #3033: Throttle redraws from rapid AgentProgress events.
                        // When 4+ sub-agents are running concurrently, each firing
                        // progress events, the per-event `needs_redraw = true` saturates
                        // the render loop and starves terminal input.  Limit
                        // progress-driven repaints to at most one per 100ms; the
                        // status-animation timer (80ms cadence) provides a guaranteed
                        // floor for sidebar updates.  Data is still recorded immediately;
                        // the sidebar picks it up on the next permitted redraw.
                        if !agent_progress_redraw_permitted_for_drain(
                            &mut app.last_agent_progress_redraw,
                            &mut progress_redraw_agents,
                            &id,
                            Instant::now(),
                        ) {
                            // Restore the pre-event accumulator value: a
                            // throttled progress event contributes no redraw of
                            // its own, but earlier events' redraws survive.
                            received_engine_event = redraw_requested_before_event;
                        }
                    }
                    EngineEvent::AgentComplete {
                        owner_session_id,
                        id,
                        result,
                        outcome,
                        display_name,
                        ..
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        if display_name.is_some() {
                            app.agent_progress_meta
                                .entry(id.clone())
                                .or_default()
                                .display_name = display_name;
                        }
                        let subagent_elapsed = app
                            .agent_activity_started_at
                            .or(app.turn_started_at)
                            .map(|started| started.elapsed())
                            .unwrap_or_default();
                        let has_other_running_subagents =
                            app.agent_progress.keys().any(|agent_id| agent_id != &id)
                                || app.subagent_cache.iter().any(|agent| {
                                    agent.agent_id != id
                                        && matches!(agent.status, SubAgentStatus::Running)
                                });
                        app.agent_progress.remove(&id);
                        crate::tui::pending_requests::clear_for_agent(app, &id);
                        let terminal_status = outcome;
                        if let Some(terminal_status) = terminal_status.as_ref() {
                            apply_subagent_terminal_projection(
                                app,
                                &id,
                                terminal_status.clone(),
                                Some(bound_agent_activity_text(&result)),
                            );
                            apply_agent_complete_status_and_observer(
                                app,
                                &id,
                                &result,
                                terminal_status,
                            );
                        } else {
                            let label = app.ensure_agent_label(&id);
                            app.status_message = Some(format!(
                                "{label} settled; outcome unconfirmed. Refreshing worker state."
                            ));
                        }
                        let should_recapture_terminal =
                            !has_other_running_subagents && app.use_alt_screen();
                        // #6565: the finished child joins the batch under the
                        // name every surface shows; the notice names every
                        // child of the batch and waits only on finite work.
                        if let Some(terminal_status) = terminal_status.as_ref() {
                            let label = app.ensure_agent_label(&id);
                            app.background_finished.push(
                                crate::tui::background_finished::FinishedWork::agent(
                                    &label,
                                    terminal_status,
                                    &result,
                                    subagent_elapsed,
                                ),
                            );
                        }
                        flush_background_finished(app, config, false);
                        if should_recapture_terminal && event_broker.is_paused() {
                            resume_terminal(
                                terminal,
                                app.use_alt_screen(),
                                app.use_mouse_capture,
                                app.use_bracketed_paste,
                                app.synchronized_output_enabled,
                            )?;
                            event_broker.resume_events();
                            terminal_input.resume_after_child_terminal();
                            terminal_paused_at = None;
                            app.needs_redraw = true;
                        }
                        subagent_list_refresh_requested = true;
                    }
                    EngineEvent::SubAgentFollowUp {
                        owner_session_id,
                        agent_id,
                        outcome,
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        crate::tui::agent_focus::apply_follow_up_receipt(app, &agent_id, &outcome);
                    }
                    EngineEvent::AgentList {
                        owner_session_id,
                        agents,
                        coordination,
                        queued_follow_ups,
                        roster,
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        app.agent_queued_follow_ups = queued_follow_ups;
                        app.subagent_cache_received_at = Some(Instant::now());
                        app.agent_roster = roster;
                        app.agent_roster_session_id = Some(owner_session_id);
                        if std::mem::take(&mut app.agent_roster_print_requested) {
                            let content = crate::tui::agent_roster::render_agent_roster(
                                app.current_agent_roster(),
                                "main",
                            );
                            app.add_message(crate::tui::history::HistoryCell::System { content });
                        }
                        let mut sorted = agents.clone();
                        sort_subagents_in_place(&mut sorted);
                        sorted.retain(|a| !a.from_prior_session);
                        app.subagent_cache = sorted.clone();
                        apply_coordination_detail_projection(app, coordination);
                        reconcile_subagent_activity_state(app);
                        let view_agents = subagent_view_agents(app, &app.subagent_cache);
                        if app.view_stack.update_subagents(&view_agents) {
                            app.status_message = Some(current_session_fleet_workers_status(
                                app.ui_locale,
                                view_agents.len(),
                            ));
                        }
                        // Individual spawn/complete events already log to history;
                        // full list available via /agents command.
                    }
                    EngineEvent::AgentSpawned { .. }
                    | EngineEvent::AgentProgress { .. }
                    | EngineEvent::AgentComplete { .. }
                    | EngineEvent::SubAgentFollowUp { .. }
                    | EngineEvent::AgentList { .. } => {
                        // Process-local senders can outlive a session switch.
                        // A foreign event must not mutate the active transcript,
                        // sidebar, status, observer, or notification surface.
                        received_engine_event = redraw_requested_before_event;
                    }
                    EngineEvent::SubAgentMailbox {
                        owner_session_id,
                        turn_id,
                        seq,
                        message,
                    } if event_owner_is_active(
                        app.current_session_id.as_deref(),
                        &owner_session_id,
                    ) =>
                    {
                        let should_refresh_subagents =
                            subagent_message_refreshes_workspace_context(&message);
                        let updated_transcript =
                            handle_subagent_mailbox_for_turn(app, &turn_id, seq, &message);
                        if let Some((agent_id, status, result)) =
                            subagent_terminal_projection_from_mailbox(&message)
                        {
                            apply_subagent_terminal_projection(app, agent_id, status, result);
                            subagent_list_refresh_requested = true;
                        }
                        if should_refresh_subagents {
                            subagent_list_refresh_requested = true;
                        }
                        if updated_transcript {
                            transcript_batch_updated = true;
                        } else if !should_refresh_subagents
                            && matches!(
                                message,
                                crate::tools::subagent::MailboxMessage::Progress { .. }
                            )
                        {
                            // Progress mailbox envelopes mirror AgentProgress.
                            // When the card state did not visibly change, do
                            // not let the duplicate envelope bypass the
                            // AgentProgress redraw throttle.
                            received_engine_event = redraw_requested_before_event;
                        }
                    }
                    EngineEvent::SubAgentMailbox { .. } => {
                        received_engine_event = redraw_requested_before_event;
                    }
                    EngineEvent::WorkflowUi {
                        owner_session_id,
                        run_id,
                        event,
                    } => {
                        if !apply_owned_workflow_ui_event(app, &owner_session_id, &run_id, &event) {
                            tracing::debug!("discarding workflow UI event for an inactive session");
                            received_engine_event = redraw_requested_before_event;
                            continue;
                        }
                        // Coalesce progress (#4095): a 75-agent fan-out streams
                        // task and budget events far faster than a frame. The
                        // state is already applied; only a run's start and end
                        // paint at once, the rest share the AgentProgress pace.
                        // The transcript is not marked here — the only cells a
                        // workflow event touches mark it themselves.
                        let lifecycle =
                            event.get("type").and_then(|v| v.as_str()).is_some_and(|t| {
                                matches!(t, "run_started" | "run_completed" | "run_cancelled")
                            });
                        if lifecycle
                            || workflow_budget_redraw_permitted(
                                &mut app.last_workflow_budget_redraw,
                                Instant::now(),
                            )
                        {
                            app.needs_redraw = true;
                        } else {
                            received_engine_event = redraw_requested_before_event;
                        }
                    }
                    EngineEvent::ApprovalRequired {
                        id,
                        tool_name,
                        description,
                        input,
                        approval_key,
                        approval_grouping_key,
                        intent_summary,
                        approval_force_prompt,
                    } => {
                        handle_approval_required_event(
                            app,
                            &engine_handle,
                            config,
                            ApprovalRequiredEvent {
                                id,
                                tool_name,
                                description,
                                input,
                                approval_key,
                                approval_grouping_key,
                                intent_summary,
                                approval_force_prompt,
                            },
                        )
                        .await;
                    }
                    // Retired by pending_requests before session/idle filters.
                    EngineEvent::ApprovalWithdrawn { .. } => {}
                    EngineEvent::UserInputRequired { id, request } => {
                        app.pending_user_input_prompt = Some((id.clone(), request.clone()));
                        app.view_stack.push(UserInputView::new(id.clone(), request));
                        let payload = notifications::input_needed_payload(app.ui_locale);
                        if let Some((method, _, _)) = crate::tui::notifications::settings(config) {
                            let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                            crate::tui::notifications::notify_done(
                                method,
                                in_tmux,
                                &payload,
                                Duration::ZERO,
                                Duration::ZERO,
                            );
                        }
                        app.push_status_toast_record(
                            StatusToast::new(
                                payload.headline(),
                                StatusToastLevel::Warning,
                                Some(12_000),
                            )
                            .for_action(id.clone()),
                        );
                    }
                    EngineEvent::ElevationRequired {
                        tool_id,
                        tool_name,
                        command,
                        denial_reason,
                        blocked_network,
                        blocked_write,
                    } => {
                        // Auto-approved modes may retry denied tools without another prompt.
                        if app_auto_approve_enabled(app) {
                            log_sensitive_event(
                                "tool.sandbox.auto_elevate",
                                serde_json::json!({
                                    "tool_name": tool_name,
                                    "tool_id": tool_id,
                                    "reason": denial_reason,
                                    "session_id": app.current_session_id,
                                }),
                            );
                            app.add_message(HistoryCell::System {
                                content: format!(
                                    "Sandbox denied {tool_name}: {denial_reason} - auto-elevating to full access"
                                ),
                            });
                            // Auto-elevate to full access (no sandbox)
                            let policy = crate::sandbox::SandboxPolicy::DangerFullAccess;
                            let _ = engine_handle
                                .retry_tool_with_policy_by(
                                    tool_id,
                                    policy,
                                    crate::approval_log::ApprovalDecider::Posture,
                                )
                                .await;
                        } else {
                            log_sensitive_event(
                                "tool.sandbox.prompt_elevation",
                                serde_json::json!({
                                    "tool_name": tool_name,
                                    "tool_id": tool_id,
                                    "reason": denial_reason,
                                    "session_id": app.current_session_id,
                                }),
                            );
                            // Show elevation dialog
                            let request = ElevationRequest::for_shell(
                                &tool_id,
                                command.as_deref().unwrap_or(&tool_name),
                                &denial_reason,
                                blocked_network,
                                blocked_write,
                            );
                            app.view_stack
                                .push(ElevationView::new(request, app.ui_locale));
                            let payload = notifications::elevation_needed_payload(
                                app.ui_locale,
                                &tool_name,
                                &denial_reason,
                            );
                            if let Some((method, _, _)) =
                                crate::tui::notifications::settings(config)
                            {
                                let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                                crate::tui::notifications::notify_done(
                                    method,
                                    in_tmux,
                                    &payload,
                                    Duration::ZERO,
                                    Duration::ZERO,
                                );
                            }
                            app.push_status_toast_record(
                                StatusToast::new(
                                    payload.headline(),
                                    StatusToastLevel::Warning,
                                    Some(12_000),
                                )
                                .for_action(tool_id.clone()),
                            );
                        }
                    }
                    EngineEvent::TurnUsage {
                        max_output_tokens: _,
                        usage,
                        duration_ms,
                        first_token_ms,
                        request_ms,
                    } => {
                        // Per-step usage receipt. The session metrics strip
                        // folds each model call's timing (stream time, TTFT,
                        // whole-call time) here.
                        app.session_metrics.record_model_call(
                            usage.output_tokens,
                            duration_ms,
                            first_token_ms,
                            request_ms,
                        );
                        // Billed prompt receipt for the context meter: what
                        // the provider says the model actually processed
                        // (#5577). Reviewer/REPL child receipts also arrive
                        // here, but their prompt is never larger than the
                        // parent context, and the meter takes a max.
                        if usage.input_tokens > 0 {
                            app.last_billed_input_tokens = Some(
                                app.last_billed_input_tokens
                                    .map_or(usage.input_tokens, |prior| {
                                        prior.max(usage.input_tokens)
                                    }),
                            );
                        }
                        // Live cost: price this call against the route that
                        // was actually dispatched so the cost surfaces move
                        // during a long agentic turn instead of only at its
                        // end. Provisional by design — `TurnComplete` clears
                        // this and lands the authoritative cumulative price
                        // through the same audit path, so nothing counts
                        // twice and the two can never disagree on route.
                        let step_cost = app
                            .active_turn
                            .as_ref()
                            .and_then(|turn| turn.route.as_ref())
                            .and_then(crate::core::events::TurnRoute::cost_envelope)
                            .and_then(|route| route.audit(&usage).estimate);
                        if let Some(cost) = step_cost {
                            app.accrue_pending_turn_cost_estimate(cost);
                        }
                        app.session.accrue_pending_turn_usage(&usage);
                    }
                    EngineEvent::RoutedTurnUsage {
                        usage,
                        duration_ms,
                        first_token_ms,
                        request_ms,
                    } => {
                        // Routed calls own separate immutable cost receipts.
                        // Preserve model-call telemetry without pricing them
                        // provisionally under the active parent route or
                        // incrementally adding tokens that TurnComplete will
                        // reconcile authoritatively.
                        app.session_metrics.record_model_call(
                            usage.output_tokens,
                            duration_ms,
                            first_token_ms,
                            request_ms,
                        );
                    }
                    EngineEvent::AdvisoryNote { note, .. } => {
                        // Advisor background watcher note. Display as a
                        // concise system message in the transcript so the
                        // user can see it without it blocking the parent turn.
                        if note.trim() != "ok" {
                            app.add_message(HistoryCell::System {
                                content: format!("⚑ Advisor: {note}"),
                            });
                        }
                    }
                    EngineEvent::ToolGateDecision {
                        agent_id,
                        tool_id,
                        tool_name,
                        gate,
                        decision,
                        risk,
                        reason,
                    } => {
                        // A permission decision nobody was prompted for. It
                        // goes to `audit.log` (what `/permissions` promises;
                        // until 0.10.1 only `CODEWHALE_TOOL_AUDIT_LOG` got
                        // it), written off the event loop (#6149). The
                        // transcript gets a one-line receipt so the person
                        // can see who decided and why, without a modal. It is
                        // held until the tool card completes so it lands
                        // under that card rather than inside a running run.
                        let mut audit = crate::tui::gate_receipts::tool_gate_audit_record(
                            agent_id.as_deref(),
                            &tool_id,
                            &tool_name,
                            gate,
                            decision,
                            risk.as_deref(),
                            &reason,
                        );
                        audit["session_id"] = serde_json::json!(app.current_session_id);
                        tokio::task::spawn_blocking(move || {
                            log_sensitive_event("tool.gate.decision", audit);
                        });
                        let receipt = crate::tui::gate_receipts::tool_gate_receipt(
                            app.ui_locale,
                            &tool_name,
                            gate,
                            decision,
                            risk.as_deref(),
                            &reason,
                        );
                        if let Some(agent_id) = agent_id {
                            // A child's decision belongs to the child's
                            // conversation: it renders under that tool card
                            // in focus mode, not in the main transcript.
                            app.child_gate_receipts
                                .entry(agent_id.clone())
                                .or_default()
                                .push((tool_id, receipt));
                            if app
                                .agent_focus
                                .as_ref()
                                .is_some_and(|focus| focus.is(&agent_id))
                            {
                                crate::tui::agent_focus::refresh_focus(app);
                            }
                        } else {
                            app.pending_gate_receipts.push((tool_id, receipt));
                        }
                    }
                }
            }
        }
        if let Some(rollback) = fallback_after_engine_error {
            apply_provider_fallback_switch(app, &mut engine_handle, config, rollback).await;
        }
        if let Some(rollback_warning) = respawn_after_provider_rollback {
            let _ = engine_handle.send(Op::Shutdown).await;
            let engine_config = build_engine_config(app, config);
            engine_handle = spawn_tui_engine(engine_config, config);
            if !app.api_messages.is_empty() {
                let _ = engine_handle
                    .send(Op::SyncSession {
                        session_id: app.current_session_id.clone(),
                        messages: app.api_messages.as_ref().clone(),
                        system_prompt: app.system_prompt.clone(),
                        system_prompt_override: false,
                        model: app.model.clone(),
                        workspace: app.workspace.clone(),
                        mode: app.mode,
                    })
                    .await;
            }
            let _ = engine_handle
                .send(Op::SetCompaction {
                    config: app.compaction_config(),
                })
                .await;
            app.status_message = Some(rollback_warning);
        }
        let current_skill_epoch = crate::extension_host::command::epoch();
        if skill_cache_refresh
            .as_ref()
            .is_some_and(|(_, job)| job.is_finished())
        {
            let (scope, job) = skill_cache_refresh.take().expect("finished skill refresh");
            if let Ok(skills) = job.await
                && app.install_skill_cache_if_current(&scope, current_skill_epoch, skills)
            {
                skill_registry_epoch = Some(scope.epoch);
            }
        }
        if skill_cache_refresh.is_none() && skill_registry_epoch != Some(current_skill_epoch) {
            let scope = app.skill_cache_scope(current_skill_epoch);
            let scan = scope.clone();
            let policy = crate::plugins::activation::extension_host_policy_enabled();
            #[cfg(test)]
            let env_scope = crate::test_support::env_scope_ticket();
            #[cfg(test)]
            let manager = crate::extension_host::manager();
            let job = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                #[cfg(test)]
                let _manager = crate::extension_host::TestManagerGuard::install(manager);
                let _policy = crate::plugins::activation::PolicyScope::propagate(policy);
                crate::skills::clear_skill_discovery_cache();
                App::discover_cached_skills(
                    &scan.workspace,
                    &scan.skills_dir,
                    scan.mode,
                    &scan.plugins,
                )
            });
            skill_cache_refresh = Some((scope, job));
        }
        if commit_streaming_display_tick(app, &mut stream_display_clock, Instant::now()) {
            transcript_batch_updated = true;
        }
        // #4022: `/lane interrupt` answers immediately with a queued receipt,
        // which is not an outcome. The terminal receipt lands here, under the
        // ticket the composer printed, so a queued write is never left looking
        // like it succeeded. Drain is non-blocking: it only takes the queue
        // mutex, and a poisoned one yields nothing rather than panicking the
        // event loop.
        for receipt in app.lane_control.drain_completed() {
            app.add_message(HistoryCell::System {
                content: receipt.render(),
            });
            transcript_batch_updated = true;
        }
        if drain_runtime_store_failures(app, &mut runtime_event_rx) {
            transcript_batch_updated = true;
        }
        if transcript_batch_updated {
            app.mark_history_updated();
        }
        if received_engine_event {
            // ListSubAgents can wait behind the parent's active turn. The
            // open register must also reflect the already-received, session-
            // scoped lifecycle events, using the same projection as opening it.
            if app.view_stack.contains_kind(ModalKind::SubAgents) {
                let agents = subagent_view_agents(app, &app.subagent_cache);
                app.view_stack.update_subagents(&agents);
            }
            app.needs_redraw = true;
        }
        if subagent_list_refresh_requested {
            pending_subagent_list_refresh = true;
        }
        // #freeze: one trailing-edge sub-agent list refresh per drain, no
        // matter how many spawn/complete/mailbox events arrived this batch.
        // #3837: keep a sticky pending bit when the op channel is full so a
        // terminal lifecycle event cannot permanently lose the authoritative
        // ListSubAgents refresh.
        if pending_subagent_list_refresh {
            match engine_handle.try_send(Op::ListSubAgents) {
                Ok(()) => pending_subagent_list_refresh = false,
                Err(err) => {
                    if err
                        .downcast_ref::<tokio::sync::mpsc::error::TrySendError<Op>>()
                        .is_some_and(|send_err| {
                            matches!(send_err, tokio::sync::mpsc::error::TrySendError::Closed(_))
                        })
                    {
                        pending_subagent_list_refresh = false;
                    }
                }
            }
        }

        if let Some(next) = queued_to_send {
            let _ = dispatch_user_message_with_recovery(
                app,
                config,
                &engine_handle,
                next,
                DispatchRecovery::Queued {
                    restore_index: None,
                },
            )
            .await;

            app.needs_redraw = true;
        }

        // Avoid cloning the queued messages/draft every loop iteration
        // (~20-40 Hz) purely for change detection. When the queue is empty and
        // was empty last time — the overwhelmingly common case — there is
        // nothing to compare, so skip the clone entirely. A multi-KB queued
        // draft is only cloned while one is actually pending.
        let queue_now_empty = app.queued_messages.is_empty() && app.queued_draft.is_none();
        if !(queue_now_empty && last_queue_was_empty) {
            let queue_state = offline_queue_projection(app);
            if queue_state != last_queue_state {
                persist_offline_queue_state(app);
                last_queue_state = queue_state;
                app.needs_redraw = true;
            }
            last_queue_was_empty = queue_now_empty;
        }

        if !app.view_stack.is_empty() {
            let tick = app.view_stack.tick();
            if tick.redraw {
                app.needs_redraw = true;
            }
            if !tick.events.is_empty()
                && handle_view_events_boxed(
                    terminal,
                    app,
                    config,
                    &task_manager,
                    &mut engine_handle,
                    tick.events,
                )
                .await?
            {
                return Ok(());
            }
        }

        let has_running_agents = running_agent_count(app) > 0;
        let turn_heartbeat = engine_handle.turn_heartbeat().snapshot();
        if reconcile_turn_liveness_supervised(app, Instant::now(), &turn_heartbeat, &engine_handle)
        {
            app.needs_redraw = true;
        }
        maybe_throttled_recovery_snapshot(app, Instant::now(), &mut last_recovery_snapshot_at);
        let history_has_live_motion = history_has_live_motion(&app.history);
        crate::tui::pet_watch::tick(app, Instant::now());
        let active_cell_has_live_motion = active_cell_has_live_motion(app);
        let translation_placeholder_has_live_motion = app.translation_enabled
            && (pending_thinking_translations > 0 || app.streaming_thinking_active_entry.is_some());
        // The ordinary terminal stays quiet. Only the underwater theme earns
        // ambient redraws; its column can breathe at any usable size and its
        // life needs the collision-safe water budget.
        let underwater_atmosphere_enabled = app.theme_id == codewhale_palette::ThemeId::Underwater;
        let deepsea_field_breathes = underwater_atmosphere_enabled
            && crate::tui::ocean::OceanRamp::for_theme(&app.ui_theme).is_some();
        let browsing_history = !app.viewport.transcript_scroll.is_at_tail();
        let empty_water_visible = app.history.is_empty()
            && app
                .active_cell
                .as_ref()
                .is_none_or(crate::tui::active_cell::ActiveCell::is_empty)
            && !app.is_loading;
        // A paused terminal owns the eye. Modal/launch/onboarding visibility
        // and attention stillness are centralized in the shell motion gate.
        let underwater_surface_obscured = event_broker.is_paused();
        let underwater_motion_visible = underwater_motion_surface_visible(
            app.viewport.last_transcript_area,
            underwater_atmosphere_enabled,
            deepsea_field_breathes,
            empty_water_visible,
            underwater_surface_obscured,
        );
        let shell_motion_enabled = crate::tui::underwater::decorative_shell_motion_enabled(app);
        let shell_phase_working = matches!(
            crate::tui::underwater::ShellPhase::from_app(app),
            crate::tui::underwater::ShellPhase::Working
                | crate::tui::underwater::ShellPhase::Verifying
        );
        // A fully idle shell settles: no live turn, no sub-agents, no active
        // durable tasks, completion exhale finished, and the user isn't
        // browsing. After a short grace the aquarium stops requesting frames
        // and the scene is genuinely still until real activity resumes
        // (owner pain, captains-log #16).
        let durable_tasks_active = app
            .task_panel
            .iter()
            .any(|task| matches!(task.status.as_str(), "queued" | "running" | "waiting"));
        let ambient_busy = shell_phase_working
            || app.turn_started_at.is_some()
            || has_running_agents
            || durable_tasks_active
            || app.is_loading
            || browsing_history
            || app.ocean_completion_started_at.is_some_and(|started| {
                started.elapsed()
                    < Duration::from_millis(crate::tui::ocean::COMPLETION_SETTLE_MS as u64)
            });
        let ambient_settled = app.ambient_idle_settled(ambient_busy, Instant::now());
        let underwater_ambient_motion = shell_motion_enabled
            && underwater_motion_visible
            && !ambient_settled
            && (browsing_history || shell_phase_working || empty_water_visible);
        let underwater_completion_motion = shell_motion_enabled
            && underwater_atmosphere_enabled
            && !underwater_surface_obscured
            && matches!(app.runtime_turn_status.as_deref(), Some("completed"))
            && app.ocean_completion_started_at.is_some_and(|started| {
                started.elapsed()
                    < Duration::from_millis(crate::tui::ocean::COMPLETION_SETTLE_MS as u64)
            });
        // The launch screen has no transcript widget to drive the ambient
        // clock, so it asks for frames itself: while the mark surfaces or
        // the card dissolves, and while the underwater field is alive
        // (settling on the same idle grace as the transcript's empty water).
        let launch_motion = crate::tui::underwater::launch_motion_active(
            app,
            underwater_surface_obscured,
            ambient_settled,
        );
        let status_motion = should_tick_status_animation(
            app,
            has_running_agents,
            history_has_live_motion,
            active_cell_has_live_motion,
            translation_placeholder_has_live_motion,
        );
        // Content-driven cadence: atmosphere rate when only ocean life moves;
        // full interactive rate while streaming, selecting, typing, or hovering.
        // Read once here so the animation tick and the frame limiter below
        // agree on the same tier for this frame.
        let cadence_tier = transcript_cadence_tier(app, has_running_agents);
        let underwater_motion =
            underwater_ambient_motion || underwater_completion_motion || launch_motion;
        let animation_active = status_motion || underwater_motion;
        // #6728: what the loop knows about its own quiet. Anything that
        // wants a prompt reaction moves `last_ui_activity`; the idle poll,
        // the automation scan and the git probe all back off from it.
        let idle_facts = IdleFacts {
            has_running_agents,
            animation_active,
            durable_tasks_active,
            input_pending: !pending_terminal_events.is_empty(),
            pending_engine_op: pending_subagent_list_refresh,
        };
        {
            let tick_now = Instant::now();
            if engine_event_seen || !ui_state_is_quiescent(app, &idle_facts, tick_now) {
                last_ui_activity = tick_now;
            }
            let quiet_for = tick_now.saturating_duration_since(last_ui_activity);
            ui_quiet = quiet_for >= UI_QUIESCENT_AFTER
                && ui_state_is_quiescent(app, &idle_facts, tick_now);
            // The git cache TTL follows the same quiet clock, set every
            // iteration so a stale back-off can never outlive the activity
            // that ended it (a turn does not reach the probe block below).
            crate::git_status::set_probe_backoff(crate::git_status::probe_is_backed_off(
                git_probe_quiet_for(app, quiet_for),
            ));
        }
        let animation_interval = Duration::from_millis(animation_interval_ms(
            app,
            status_motion,
            underwater_motion,
            cadence_tier,
        ));
        let motion_policy = app.motion_policy();
        if animation_active && last_status_frame.elapsed() >= animation_interval {
            let translation_animated = streaming_thinking::animate_pending_translation(
                app,
                pending_thinking_translations > 0,
            );
            if !matches!(motion_policy.mode(), MotionMode::Still)
                && (history_has_live_motion || active_cell_has_live_motion)
            {
                if translation_animated {
                    if history_has_live_motion {
                        app.mark_live_history_motion_updated();
                    }
                } else {
                    app.mark_live_motion_updated();
                }
            }
            // Coalesce decorative animation wakes through the shared requester.
            // Reduced/Still drop these requests; state-change redraws still set
            // needs_redraw directly below for phase/working chrome.
            frame_requester.request_frame(Instant::now(), motion_policy);
            if frame_requester.take_due(Instant::now(), motion_policy)
                || !motion_policy.should_request_animation_frames()
            {
                // Full: emit only when the requester fires. Reduced/Still: keep
                // the existing calm redraw so working/phase chrome stays truthful
                // without decorative spin (TUI-DOG-008).
                app.needs_redraw = true;
            }
            last_status_frame = Instant::now();
        }
        if animation_active {
            // Aim the poll at the next tick. Without a deadline the tick only
            // ran when the idle/active poll happened to return, which
            // quantized an 80 ms cadence to 96 ms and a 120 ms one to 144 ms.
            frame_requester.request_at(
                Instant::now(),
                last_status_frame + animation_interval,
                motion_policy,
            );
        } else {
            // Consume a deadline armed before motion stopped so an orphaned
            // request cannot hold the poll timeout at zero.
            let _ = frame_requester.take_due(Instant::now(), motion_policy);
        }

        if event_broker.is_paused() {
            let grace_active = terminal_paused_at
                .map(|paused_at| paused_at.elapsed() < Duration::from_millis(500))
                .unwrap_or(false);
            if terminal_pause_has_live_owner(app) || grace_active {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
            resume_terminal(
                terminal,
                app.use_alt_screen(),
                app.use_mouse_capture,
                app.use_bracketed_paste,
                app.synchronized_output_enabled,
            )?;
            event_broker.resume_events();
            terminal_input.resume_after_child_terminal();
            terminal_paused_at = None;
            app.status_message = Some("Terminal controls restored".to_string());
            app.needs_redraw = true;
            force_terminal_repaint = true;
        }

        let now = Instant::now();
        flush_paste_burst_before_composer(app, now);
        crate::tui::work_surface::poll_terminal(app);
        app.sync_status_message_to_toasts();
        // Drain background-LLM cost (compaction summaries, seam
        // recompaction, cycle briefings) accumulated since the last
        // tick and fold it into the session-cost counter (#526).
        // Background callers populate `cost_status::report`; we sweep
        // the pool once per loop iteration so the footer chip matches
        // the DeepSeek website's billing.
        // Money and its completeness are drained as one value, so the footer
        // total and the `/cost` coverage line can never come from different
        // observations of the pool (#4318).
        let pending_bg = crate::cost_status::drain();
        if !pending_bg.is_empty() {
            let runtime_usage_arrived = app.absorb_pending_background_cost(&pending_bg);
            if pending_bg.estimate.is_positive() {
                app.needs_redraw = true;
            }
            // Runtime-owned child usage can land after the parent's
            // TurnComplete snapshot. Queue a fresh snapshot from the same
            // drained money+identity batch so an immediate reload agrees with
            // the live footer and worker record.
            if runtime_usage_arrived
                && let Ok(manager) = SessionManager::default_location()
                && let Ok(session) = build_session_snapshot(app, &manager)
            {
                app.current_session_id = Some(session.metadata.id.clone());
                persistence_actor::persist(PersistRequest::SessionSnapshot(session));
            }
        }
        // Drain completed file-tree walks (initial build / expands) so the
        // spliced children repaint without waiting for an input event (#3900).
        if let Some(tree) = app.file_tree.as_mut()
            && tree.poll_background()
        {
            app.needs_redraw = true;
        }
        // Completion discovery is serialized off-thread. Polling is
        // non-blocking and makes a finished initial `@` scan visible even
        // after the user stops typing (#4365).
        if crate::tui::file_mention::poll_background_mention_discovery(app) {
            app.needs_redraw = true;
        }
        // Expire the "Press Ctrl+C again to quit" prompt silently after its
        // window. Triggers a redraw if the prompt was visible.
        app.tick_quit_armed();
        app.tick_receipt();
        crate::tui::footer_ui::maybe_log_provider_wait_incident(app);
        // While the user is drag-selecting past the transcript edge, advance
        // the viewport on a fixed cadence and extend the selection head so a
        // long passage can be selected in one drag (#1163).
        tick_selection_autoscroll(app);
        let allow_workspace_context_refresh =
            !app.is_loading && !has_running_agents && !app.is_compacting && !app.is_purging;
        workspace_context::refresh_if_needed(app, now, allow_workspace_context_refresh);
        // Native git chrome: at most one background probe per interval, never
        // on the render path. While a turn is live it waits, unless the Git
        // view is showing: that view is the live repository state (#6565).
        // Every probe is about a dozen `git` processes, so an untouched
        // session backs off to a slow cadence and the next input or engine
        // event (a tool finishing, say) brings the fast one back (#6728).
        if git_probe_allowed(app, allow_workspace_context_refresh) {
            static GIT_PROBE_LOCK: std::sync::OnceLock<std::sync::Mutex<Option<Instant>>> =
                std::sync::OnceLock::new();
            let slot = GIT_PROBE_LOCK.get_or_init(|| std::sync::Mutex::new(None));
            let quiet_for =
                git_probe_quiet_for(app, now.saturating_duration_since(last_ui_activity));
            let should_probe = slot
                .lock()
                .map(|mut last| {
                    let due = crate::git_status::probe_due(last.map(|t| t.elapsed()), quiet_for);
                    if due {
                        *last = Some(Instant::now());
                    }
                    due
                })
                .unwrap_or(false);
            if should_probe {
                let workspace = app.workspace.clone();
                std::thread::spawn(move || {
                    crate::git_status::refresh_if_stale(&workspace);
                });
            }
        }

        // Draw is gated by the frame-rate limiter (120 FPS cap). When a
        // redraw is needed but the limiter says we're inside the cooldown
        // window, leave `needs_redraw = true` and shorten the poll timeout
        // so the loop wakes up exactly when drawing is allowed.

        // Central motion contract: frame cap and stream catch-up both read
        // from MotionPolicy so reduced motion stays semantically calm (not a
        // slow typewriter) and Full motion keeps the steady display clock.
        let motion_policy = app.motion_policy();
        frame_rate_limiter.set_low_motion(motion_policy.uses_constrained_frame_rate());
        stream_display_clock.set_allow_catch_up(motion_policy.allows_catch_up_bursts());

        // The draw limiter follows the same content-driven tier the
        // animation tick above read for this frame.
        {
            use crate::tui::display_refresh::{
                content_driven_draw_interval, probe_display_refresh,
            };
            let probe = probe_display_refresh();
            frame_rate_limiter.set_adaptive_interval(Some(content_driven_draw_interval(
                cadence_tier,
                probe.hz,
                motion_policy.uses_constrained_frame_rate(),
            )));
        }

        let draw_wait = if app.needs_redraw {
            frame_rate_limiter.time_until_next_draw(now)
        } else {
            None
        };
        // Merge the per-app full-repaint hint (set by theme switches)
        // into the loop-level flag before the draw decision.
        if app.force_next_full_repaint {
            force_terminal_repaint = true;
            app.force_next_full_repaint = false;
        }
        if app.needs_redraw && draw_wait.is_none() && !terminal_unfocused {
            draw_app_frame_inner(terminal, app, config, force_terminal_repaint)?;
            force_terminal_repaint = false;
            frame_rate_limiter.mark_emitted(Instant::now());
            app.needs_redraw = false;
            if std::mem::take(&mut telemetry_notice_awaiting_render) {
                crate::telemetry_notice::record_presented();
            }
        }

        let mut poll_timeout =
            if app.is_loading || has_running_agents || app.is_compacting || app.is_purging {
                Duration::from_millis(active_poll_ms(app))
            } else {
                // Relaxes only once the UI has been quiescent and quiet for
                // `UI_QUIESCENT_AFTER` (#6728); every deadline below still
                // shortens it, and input returns the loop at once.
                idle_poll_duration(
                    app,
                    &idle_facts,
                    now,
                    now.saturating_duration_since(last_ui_activity),
                )
            };
        if let Some(until_flush) = app.paste_burst_next_flush_delay_if_enabled(now) {
            poll_timeout = poll_timeout.min(until_flush);
        }
        if let Some(until_draw) = draw_wait {
            poll_timeout = poll_timeout.min(until_draw);
        }
        if let Some(until_stream_commit) = stream_display_clock.due_in(now) {
            poll_timeout = poll_timeout.min(until_stream_commit);
        }
        if let Some(until_anim) = frame_requester.due_in(now) {
            poll_timeout = poll_timeout.min(until_anim);
        }
        if let Some(until_pet) = app.pet_watch.next_frame_in(Instant::now()) {
            poll_timeout = poll_timeout.min(until_pet);
        }
        // While the quit-confirmation prompt is armed, ensure we wake up to
        // expire it on time even if no input event arrives.
        if let Some(deadline) = app.quit_armed_until {
            let remaining = deadline.saturating_duration_since(now);
            poll_timeout = poll_timeout.min(remaining.max(Duration::from_millis(50)));
        }
        // Drag-edge auto-scroll wakes the loop on its own cadence so the
        // viewport keeps advancing while the user holds the mouse outside
        // the transcript rect (#1163).
        if let Some(state) = app.viewport.selection_autoscroll {
            let remaining = state.next_tick.saturating_duration_since(now);
            poll_timeout = poll_timeout.min(remaining);
        }
        if app.work_surface.panel == crate::tui::work_surface::RailPanel::Terminal
            && app.work_surface.last_area.is_some()
        {
            poll_timeout = poll_timeout.min(Duration::from_millis(100));
        }
        poll_timeout = clamp_event_poll_timeout(poll_timeout);

        // #549/#3216: give the engine task a scheduler turn before waiting on
        // the terminal-input channel. Crossterm's blocking poll/read runs on
        // `TerminalInputPump`, so engine floods cannot pin the OS input read.
        tokio::task::yield_now().await;

        let maybe_terminal_event =
            next_terminal_event(&terminal_input, &mut pending_terminal_events, poll_timeout)?;
        if maybe_terminal_event.is_some() {
            last_ui_activity = Instant::now();
        }
        if maybe_terminal_event.is_none() {
            let now = Instant::now();
            let input_stalled_for = terminal_input.stalled_for(now);
            if terminal_input_recovery_relevant(app, has_running_agents)
                && input_stalled_for >= TERMINAL_INPUT_STALL_TIMEOUT
                && now.duration_since(last_terminal_input_recovery)
                    >= TERMINAL_INPUT_RECOVERY_COOLDOWN
            {
                tracing::warn!(
                    stalled_ms = input_stalled_for.as_millis(),
                    "terminal input pump heartbeat stalled; attempting terminal input recovery"
                );
                recover_terminal_modes(
                    terminal.backend_mut(),
                    app.use_mouse_capture,
                    app.use_bracketed_paste,
                );
                match terminal_input.restart_detached() {
                    Ok(()) => {
                        tracing::info!("terminal input pump recovered");
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to restart terminal input pump");
                        app.push_status_toast(
                            "Terminal input stalled; recovery failed. Restart Codewhale if keys stop responding.",
                            StatusToastLevel::Error,
                            None,
                        );
                    }
                }
                terminal_input.mark_alive();
                last_terminal_input_recovery = now;
                if app.is_loading
                    || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
                {
                    persist_recovery_snapshot(app);
                    last_recovery_snapshot_at = Some(now);
                }
                force_terminal_repaint = true;
                app.needs_redraw = true;
            }
        }

        if let Some(observed_terminal_event) = maybe_terminal_event {
            let event_observed_at = observed_terminal_event.observed_at;
            let evt = observed_terminal_event.event;
            if app.launch.mark_reveal_started_at.is_some()
                && matches!(&evt, Event::Key(_) | Event::Paste(_) | Event::Resize(_, _))
            {
                app.launch.mark_reveal_started_at = Some(
                    Instant::now() - Duration::from_millis(crate::tui::mark::REVEAL_MS as u64),
                );
            }
            app.needs_redraw = true;
            if defer_frames_on_focus_loss {
                terminal_unfocused = next_unfocused(terminal_unfocused, &evt);
            }

            // Handle bracketed paste events
            if app.redaction_gate && app.onboarding == OnboardingState::None {
                if let Event::Mouse(mouse) = &evt {
                    match mouse.kind {
                        event::MouseEventKind::ScrollDown => app
                            .redaction_gate_scroll
                            .set(app.redaction_gate_scroll.get().saturating_add(1)),
                        event::MouseEventKind::ScrollUp => app
                            .redaction_gate_scroll
                            .set(app.redaction_gate_scroll.get().saturating_sub(1)),
                        _ => {}
                    }
                    app.needs_redraw = true;
                    continue;
                }
                if matches!(&evt, Event::Paste(_)) {
                    continue;
                }
            }
            if let Event::Paste(text) = &evt {
                if crate::tui::work_surface::handle_terminal_paste(app, text) {
                    continue;
                }
                if app.launch.return_to_session && app.view_stack.is_empty() {
                    app.launch.dismiss();
                }
                handle_bracketed_paste(app, text);
                continue;
            }

            // Re-establish terminal mode flags on focus-gain and force a full
            // viewport reset before repainting. App-switching and interactive
            // handoffs can leave the host terminal scrolled away from row 0
            // and (on macOS) can drop the keyboard, mouse-tracking, or
            // bracketed-paste modes — recover_terminal_modes() is the
            // canonical place those flags live.
            if terminal_event_needs_viewport_recapture(&evt) {
                let now = Instant::now();
                if now.duration_since(last_focus_recovery) >= FOCUS_RECOVERY_DEBOUNCE {
                    recover_terminal_modes(
                        terminal.backend_mut(),
                        app.use_mouse_capture,
                        app.use_bracketed_paste,
                    );
                    last_focus_recovery = now;
                }
                force_terminal_repaint = true;
                app.needs_redraw = true;
            }
            if let Event::Resize(width, height) = evt {
                tracing::debug!(
                    width,
                    height,
                    use_alt_screen = app.use_alt_screen(),
                    "Event::Resize received; clearing terminal"
                );
                // Drain any further Resize events queued in this poll cycle so we
                // act on the final size only, then issue a single clear + redraw.
                // crossterm coalesces some resize events but rapid drag-resizes
                // can still queue several; processing them all here avoids the
                // common "stale art on the right edge" symptom (#65) caused by
                // the diff renderer skipping cells that match a stale back
                // buffer between intermediate sizes.
                let (final_w, final_h) = coalesce_resize_burst(
                    width,
                    height,
                    &terminal_input,
                    &mut pending_terminal_events,
                )?;

                if final_w == 0 || final_h == 0 {
                    tracing::debug!(
                        final_w,
                        final_h,
                        "zero-size Resize event ignored while terminal is hidden/minimized"
                    );
                    force_terminal_repaint = true;
                    app.needs_redraw = true;
                    continue;
                }

                // The event-reported size is authoritative (#582). Applying
                // it may clear the terminal, so defer it into the synchronized
                // draw instead of exposing an empty frame while resizing.
                app.handle_resize(final_w, final_h);
                // #6311: a resize that lands while unfocused records the size
                // but must not emit the frame — same deferral as zero-size.
                if terminal_unfocused {
                    force_terminal_repaint = true;
                    app.needs_redraw = true;
                    continue;
                }
                draw_app_frame_inner(terminal, app, config, true)?;
                app.needs_redraw = false;
                continue;
            }

            if app.use_mouse_capture
                && let Event::Mouse(mouse) = evt
            {
                // Mouse interaction clears the ✅ completion marker.
                crate::tui::notifications::reset_title_on_interaction();
                if should_drop_loading_mouse_motion(app, mouse) {
                    continue;
                }
                // Fold the rest of this wheel gesture into one frame.
                let mouse = coalesce_scroll_burst(
                    app,
                    mouse,
                    &terminal_input,
                    &mut pending_terminal_events,
                )?;
                let events = handle_mouse_event(app, mouse);
                if handle_view_events_boxed(
                    terminal,
                    app,
                    config,
                    &task_manager,
                    &mut engine_handle,
                    events,
                )
                .await?
                {
                    return Ok(());
                }
                if app.pending_launch_action.is_none() {
                    restore_launch_card_after_view_close(app);
                }
                if let Some(action) = app.pending_launch_action.take() {
                    match action {
                        crate::tui::underwater::LaunchAction::None => {}
                        crate::tui::underwater::LaunchAction::ReturnToSession => {
                            app.launch.dismiss()
                        }
                        crate::tui::underwater::LaunchAction::NewSession => {
                            let result = begin_launch_session(app, None);
                            if apply_command_result(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                result,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                        }
                        crate::tui::underwater::LaunchAction::ResumeSession(session_id) => {
                            let result = resume_launch_session(app, &session_id);
                            if apply_command_result(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                result,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                        }
                        crate::tui::underwater::LaunchAction::BrowseSessions => {
                            // A launched command dissolves the card; Esc
                            // out of the picker brings it back.
                            app.launch.dissolve_card(app.ambient_clock_ms);
                            app.view_stack.push(
                                SessionPickerView::new(&app.workspace, app.ui_locale)
                                    .with_current_session(app.current_session_id.as_deref()),
                            );
                        }
                        crate::tui::underwater::LaunchAction::McpRemedy => {
                            type_launch_mcp_remedy(app);
                        }
                        crate::tui::underwater::LaunchAction::McpManager => {
                            app.launch.dissolve_card(app.ambient_clock_ms);
                            open_mcp_extensions(app);
                        }
                        crate::tui::underwater::LaunchAction::Help => {
                            toggle_help_view(app);
                        }
                    }
                    app.needs_redraw = true;
                }
                if let Some(chord) = app.pending_composer_submit.take() {
                    if dispatch_session_composer_submit(
                        terminal,
                        app,
                        &mut engine_handle,
                        &task_manager,
                        config,
                        chord,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                    app.needs_redraw = true;
                }
                if let Some(slot) = app.pending_hotbar_slot.take()
                    && let Some(dispatch) = dispatch_hotbar_slot(app, config, slot)?
                {
                    match dispatch {
                        HotbarDispatch::Handled => app.needs_redraw = true,
                        HotbarDispatch::AppAction(action) => {
                            if apply_command_result(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                commands::CommandResult::action(action),
                            )
                            .await?
                            {
                                return Ok(());
                            }
                            if let Err(err) = persist_pending_work_checkpoint(app).await {
                                app.status_message = Some(format!(
                                    "Hotbar change applied, but its Work receipt is pending ({err})"
                                ));
                            }
                            app.needs_redraw = true;
                        }
                    }
                }
                continue;
            }

            // User interaction — clear the ✅ completion marker from the title.
            crate::tui::notifications::reset_title_on_interaction();

            let Event::Key(mut key) = evt else {
                continue;
            };

            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Normalize macOS modifiers: map SUPER (Cmd) to CONTROL so that
            // keyboard shortcuts work consistently across terminal emulators
            // (Terminal.app, iTerm2, Kitty, etc.) that may report different
            // modifier flags (#2938). The select-all chord is exempt: `Cmd+A`
            // must stay distinguishable from readline `Ctrl+A` (start of
            // input) on terminals that forward Cmd, so it keeps its SUPER
            // modifier and routes through `is_select_all_shortcut`.
            if !key_shortcuts::is_select_all_shortcut(&key) {
                let mapped = crate::tui::composer_ui::normalize_macos_modifiers(key.modifiers);
                key.modifiers = mapped;
            }

            // Normalize the raw Ctrl+C control byte (0x03) delivered in
            // PTY/raw-mode — and by some kitty-keyboard-protocol terminals —
            // to canonical Ctrl+C so the quit-arm flow always runs (#4090).
            normalize_raw_ctrl_c(&mut key);

            // The `[redaction] model_bound` opt-out gate owns every key until
            // it is answered, exactly like onboarding above. Enter never
            // confirms by reflex (same discipline as workspace trust): the
            // three explicit choices are advertised in the action rail.
            if app.redaction_gate && app.onboarding == OnboardingState::None {
                let gate_binding = shell_binding_for_key(app, &key);
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    _ if gate_binding == Some(ShellBindingId::RedactionGateConfirm) => {
                        if !app.redaction_gate_confirming {
                            // First confirm only advances to the final
                            // confirmation stage; nothing is persisted yet.
                            app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
                            app.redaction_gate_confirming = true;
                            app.redaction_gate_scroll.set(0);
                        } else {
                            match crate::tui::redaction_gate::record_confirmation(config) {
                                Ok(_) => {
                                    // The engine already spawned with masking on
                                    // (the unconfirmed safe default). Rebuild it so
                                    // its client picks up the confirmed opt-out.
                                    let _ = engine_handle.send(Op::Shutdown).await;
                                    engine_handle =
                                        spawn_tui_engine_with_session(app, config).await?;
                                    app.retire_redaction_gate_notice(
                                        RedactionGateNotice::EnterGuidance,
                                    );
                                    app.retire_redaction_gate_notice(
                                        RedactionGateNotice::WriteFailure,
                                    );
                                    app.retire_action_notices(None);
                                    app.redaction_gate = false;
                                    app.redaction_gate_confirming = false;
                                    app.needs_redraw = true;
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "redaction confirmation could not be saved: {err}"
                                    );
                                    app.push_status_toast_record(
                                        StatusToast::new(
                                            app.tr(MessageId::RedactionGateSaveFailed).into_owned(),
                                            StatusToastLevel::Error,
                                            None,
                                        )
                                        .for_redaction_gate(RedactionGateNotice::WriteFailure),
                                    );
                                    app.redaction_gate_scroll.set(0);
                                }
                            }
                        }
                    }
                    _ if gate_binding == Some(ShellBindingId::RedactionGateKeepOrBack) => {
                        if app.redaction_gate_confirming {
                            // Second-stage "back": return to the first stage
                            // without recording anything.
                            app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
                            app.redaction_gate_confirming = false;
                            app.redaction_gate_scroll.set(0);
                        } else {
                            // Keep masking on for this launch. Nothing is
                            // persisted and no config file is rewritten;
                            // because the config field still requests
                            // "disabled", the next launch asks again.
                            app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
                            app.retire_redaction_gate_notice(RedactionGateNotice::WriteFailure);
                            app.redaction_gate = false;
                            app.needs_redraw = true;
                        }
                    }
                    _ if gate_binding == Some(ShellBindingId::RedactionGateQuit) => {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    // Esc on the final-confirmation stage steps back to the
                    // first stage (the user was mid-decision); on the first
                    // stage it quits, matching the trust screen.
                    KeyCode::Esc if app.redaction_gate_confirming => {
                        app.retire_redaction_gate_notice(RedactionGateNotice::EnterGuidance);
                        app.redaction_gate_confirming = false;
                        app.redaction_gate_scroll.set(0);
                    }
                    KeyCode::Esc => {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        app.push_status_toast_record(
                            StatusToast::new(
                                app.tr(MessageId::RedactionGateEnterHint).into_owned(),
                                StatusToastLevel::Info,
                                Some(12_000),
                            )
                            .for_redaction_gate(RedactionGateNotice::EnterGuidance),
                        );
                        app.redaction_gate_scroll.set(0);
                    }
                    KeyCode::Down | KeyCode::PageDown
                        if gate_binding == Some(ShellBindingId::RedactionGateScroll) =>
                    {
                        app.redaction_gate_scroll
                            .set(app.redaction_gate_scroll.get().saturating_add(1))
                    }
                    KeyCode::Up | KeyCode::PageUp
                        if gate_binding == Some(ShellBindingId::RedactionGateScroll) =>
                    {
                        app.redaction_gate_scroll
                            .set(app.redaction_gate_scroll.get().saturating_sub(1))
                    }
                    KeyCode::Home => app.redaction_gate_scroll.set(0),
                    KeyCode::End => app.redaction_gate_scroll.set(usize::MAX),
                    _ => {}
                }
                app.needs_redraw = true;
                submit_initial_input_if_ready(app, config, &engine_handle).await?;
                continue;
            }

            // Login cancellation precedes modal/focus dispatch: Extensions
            // must not consume the Esc promised by the authorization notice.
            if handle_mcp_login_key(app, &key) {
                continue;
            }

            // A route change made in-session is temporary and stays that way
            // until the user EXPLICITLY persists it with a command
            // (/fleet save updates the selected Fleet, /fleet save-as saves a
            // new Fleet, /model save-default remembers the startup default).
            // Nothing here intercepts keys: a scripted or automated terminal
            // types exactly what it types, and plain typing can never trigger
            // a fleet write by accident.

            // Decision prompts keep their ordinary option/typing keys while
            // explicit transcript navigation reviews the evidence above them
            // (#4371, #6045). Bare arrows still belong to the question sheet.
            if handle_prompt_transcript_key(app, &key) {
                continue;
            }

            // The Ocean work surface is a real focus owner. Route its keys
            // before global transcript/composer navigation so PageUp/Down,
            // Home/End, arrows, and row actions stay panel-local.
            if app.view_stack.is_empty()
                && let Some(action) = crate::tui::work_surface::handle_key(app, key)
            {
                if let Some(action) = action {
                    match action {
                        crate::tui::app::SidebarRowAction::Command(command) => {
                            if execute_command_input(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                &command,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                        }
                        crate::tui::app::SidebarRowAction::CancelAgent { agent_id } => {
                            app.status_message = Some(format!("Cancelling {agent_id}..."));
                            if engine_handle
                                .send(Op::CancelSubAgent {
                                    agent_id: agent_id.clone(),
                                })
                                .await
                                .is_err()
                            {
                                app.status_message = Some(format!("Could not cancel {agent_id}"));
                            }
                        }
                        other => {
                            let _ = crate::tui::mouse_ui::apply_sidebar_row_action(app, other);
                        }
                    }
                }
                submit_initial_input_if_ready(app, config, &engine_handle).await?;
                continue;
            }

            if crate::tui::pet_watch::handle_inspect_key(app, &key) {
                continue;
            }

            // The shell's key admission runs through one table
            // (`shell_key_routing::SHELL_BINDINGS`) keyed by one focus owner
            // (`app.focus()`) — never by whether the composer happens to hold
            // text. Help and Settings are shell-global, including onboarding,
            // launch, and modal surfaces (`/help` and `/provider` remain the
            // guaranteed textual routes); Shift+Tab is a shell-level
            // permission control and is claimed here, before the launch
            // screen swallows it. The remaining bindings are admitted by the
            // same table at their owners' seams below, where the composer's
            // completions and the agent-focus projection get the key first.
            match shell_binding_for_key(app, &key) {
                Some(ShellBindingId::Help) => {
                    app.note_footer_hint_used(crate::tui::footer_hints::HELP_ROUTE);
                    toggle_help_view(app);
                    continue;
                }
                Some(ShellBindingId::Settings) => {
                    toggle_settings_view(app);
                    continue;
                }
                Some(ShellBindingId::PermissionCycle) => {
                    cycle_permission_posture(app, config, &engine_handle).await;
                    app.note_footer_hint_used(crate::tui::footer_hints::PERMISSION_CYCLE);
                    continue;
                }
                Some(ShellBindingId::ViewCycle) => {
                    crate::tui::work_surface::cycle_view(app, true);
                    app.note_footer_hint_used(crate::tui::footer_hints::DOCK_OPEN);
                    continue;
                }
                Some(ShellBindingId::ViewCycleBack) => {
                    crate::tui::work_surface::cycle_view(app, false);
                    continue;
                }
                _ => {}
            }

            // Provider onboarding is a real ProviderPickerView, not a
            // parallel ten-provider key handler. Route its keys before the
            // legacy onboarding switch so List/Key/Model/Confirm retain the
            // same behavior as `/provider` and `/setup`.
            match onboarding_key_route(app.onboarding, app.view_stack.top_kind(), &key) {
                // #4763: onboarding must never be a trap. Ctrl+C terminates
                // from every onboarding state, including while the picker
                // owns the keys — the legacy handler below is unreachable
                // once a modal is on the stack.
                OnboardingKeyRoute::Quit => {
                    let _ = engine_handle.send(Op::Shutdown).await;
                    return Ok(());
                }
                // #3927: no provider is selected and no route is activated.
                // The picker (a preview surface, never route authority) is
                // popped without applying anything it was showing.
                OnboardingKeyRoute::ExploreOffline => {
                    if app.view_stack.top_kind() == Some(ModalKind::ProviderPicker) {
                        let _ = app.view_stack.pop();
                    }
                    onboarding::choose_offline_explore(app);
                    continue;
                }
                // Every other key, Escape included, belongs to the picker.
                // The picker's own per-stage Escape walks key/OAuth entry
                // back to the list and only dismisses from the list, where
                // `ProviderPickerDismissed` runs the same non-mutating
                // onboarding back-transition the shell used to force.
                OnboardingKeyRoute::ProviderPicker => {
                    if key_shortcuts::is_paste_shortcut(&key)
                        && paste_provider_picker_from_clipboard(app)
                    {
                        app.needs_redraw = true;
                        continue;
                    }
                    let events = app.view_stack.handle_key(key);
                    app.needs_redraw = true;
                    if handle_view_events_boxed(
                        terminal,
                        app,
                        config,
                        &task_manager,
                        &mut engine_handle,
                        events,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                    continue;
                }
                OnboardingKeyRoute::Legacy => {}
            }

            // Handle onboarding flow
            if app.onboarding != OnboardingState::None {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    KeyCode::Esc if app.onboarding == OnboardingState::Provider => {
                        back_from_provider_onboarding(app);
                    }
                    KeyCode::Esc if app.onboarding == OnboardingState::Language => {
                        app.onboarding = OnboardingState::Welcome;
                        app.status_message = None;
                    }
                    // Language picker hotkeys select + persist (#566).
                    //
                    // Note: this used to be a single match-guard with `&& let`,
                    // but `if_let_guard` is a nightly-only feature on Rust
                    // before 1.94. Rewriting as a plain guard + nested `if let`
                    // keeps `cargo install` working on stable.
                    KeyCode::Char(c)
                        if app.onboarding == OnboardingState::Language
                            && (c.is_ascii_digit() || c.is_ascii_lowercase()) =>
                    {
                        if let Some((_, tag, _, _)) = onboarding::language::LANGUAGE_OPTIONS
                            .iter()
                            .find(|(hotkey, _, _, _)| *hotkey == c)
                        {
                            match app.set_locale_from_onboarding(tag) {
                                Ok(()) => {
                                    app.push_status_toast(
                                        format!("Language set to {tag}"),
                                        StatusToastLevel::Info,
                                        Some(2_500),
                                    );
                                    onboarding::advance_onboarding_after_language(app);
                                }
                                Err(err) => {
                                    app.status_message =
                                        Some(format!("Failed to save locale: {err}"));
                                }
                            }
                        }
                    }
                    KeyCode::Enter => match app.onboarding {
                        OnboardingState::Welcome => {
                            onboarding::advance_onboarding_from_welcome(app);
                        }
                        OnboardingState::Language => {
                            // Enter without a digit pick keeps the existing
                            // setting (which defaults to "auto").
                            onboarding::advance_onboarding_after_language(app);
                        }
                        OnboardingState::Provider => {
                            let recover_configured_route =
                                app.onboarding_recovers_configured_route();
                            open_onboarding_provider_picker(
                                app,
                                config,
                                &engine_handle,
                                recover_configured_route,
                            )
                            .await;
                        }
                        OnboardingState::TrustDirectory => {
                            // Trusting a workspace is a security boundary, so it
                            // must be a deliberate choice. Enter — the "advance"
                            // key on every other onboarding screen — must NOT
                            // grant trust by reflex (accidental-trust risk). Nor
                            // is it a silent dead key: point the user at the
                            // explicit keys the rail advertises.
                            app.status_message =
                                Some(app.tr(MessageId::OnboardTrustEnterHint).to_string());
                        }
                        OnboardingState::Ready => {
                            // Enter opens the product: the real composer,
                            // pre-seeded with a first task for this folder —
                            // never another educational surface.
                            onboarding::finish_ready_and_open_composer(app);
                        }
                        OnboardingState::None => {}
                    },
                    // "Customize later": the appearance choice from the ready
                    // screen, as an optional secondary action. Onboarding is
                    // finished first so the theme picker is an ordinary modal
                    // over the live product, not a required step.
                    KeyCode::Char('c') | KeyCode::Char('C')
                        if app.onboarding == OnboardingState::Ready =>
                    {
                        onboarding::finish_ready_and_open_composer(app);
                        open_theme_picker(app);
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1')
                        if app.onboarding == OnboardingState::TrustDirectory =>
                    {
                        if let Err(err) = complete_trust_directory_onboarding(app, config) {
                            app.status_message = Some(format!("Failed to trust workspace: {err}"));
                        }
                    }
                    // Number keys mirror the footer's reading order (1 trust,
                    // 2 continue untrusted, 3 quit) so the displayed digits
                    // are sequential instead of 1/3/2.
                    KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Char('2')
                        if app.onboarding == OnboardingState::TrustDirectory =>
                    {
                        continue_without_trusting_directory(app);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('3')
                        if app.onboarding == OnboardingState::TrustDirectory =>
                    {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    KeyCode::Esc if app.onboarding == OnboardingState::TrustDirectory => {
                        let _ = engine_handle.send(Op::Shutdown).await;
                        return Ok(());
                    }
                    _ => {}
                }
                continue;
            }

            // F3 is the non-printable keyboard counterpart to the clickable
            // route segment in the shared topbar. Route it through the same
            // typed event as mouse input; `/provider` remains the portable
            // direct command path for terminals that do not forward F-keys.
            if shell_binding_for_key(app, &key) == Some(ShellBindingId::ProviderRoute) {
                if handle_view_events_boxed(
                    terminal,
                    app,
                    config,
                    &task_manager,
                    &mut engine_handle,
                    vec![ViewEvent::TopbarRoutePickerRequested],
                )
                .await?
                {
                    return Ok(());
                }
                continue;
            }

            // The pre-session launch menu owns every key until the user has
            // chosen a real session/worktree action. Resume and changelog may
            // place a shared surface above it; those views keep their normal
            // handlers while the launch screen remains the stable backdrop.
            if app.launch.visible {
                if !app.view_stack.is_empty() {
                    let events = app.view_stack.handle_key(key);
                    app.needs_redraw = true;
                    if handle_view_events_boxed(
                        terminal,
                        app,
                        config,
                        &task_manager,
                        &mut engine_handle,
                        events,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                    restore_launch_card_after_view_close(app);
                    continue;
                }

                let launch_locale = app.ui_locale;
                // The pre-session composer is the session's own composer.
                // While it holds focus, this admission guard only claims the
                // launch-specific keys (list navigation/run, F1 help,
                // submit); every editing key falls through to the
                // conversation composer match below — the single composer
                // input authority — so word motion, selection, completion
                // menus, attachments, history, and vim behavior cannot drift
                // from the shell.
                let mut composer_authority = false;
                // A menu-run Enter defers its action to the chord match
                // below, which owns every launch action's execution.
                let mut menu_run_action: Option<crate::tui::underwater::LaunchAction> = None;
                if app.launch.composer_focus {
                    match crate::tui::underwater::handle_launch_composer_key(app, key) {
                        crate::tui::underwater::LaunchComposerKey::Consumed => {
                            app.needs_redraw = true;
                            continue;
                        }
                        crate::tui::underwater::LaunchComposerKey::MenuChord => {
                            // The same key then drives the launch chords.
                        }
                        crate::tui::underwater::LaunchComposerKey::ComposerAuthority => {
                            // Skip the menu handler; the conversation
                            // composer match below owns this key.
                            composer_authority = true;
                        }
                        crate::tui::underwater::LaunchComposerKey::MenuSelect => {
                            // A completion popup entry was applied; the key is
                            // consumed without submitting.
                            app.needs_redraw = true;
                            continue;
                        }
                        crate::tui::underwater::LaunchComposerKey::MenuNavigate(delta) => {
                            // The card is up: Up/Down move its row selection
                            // over the full row list (Enter still runs a row
                            // the plan shed on a tiny stage).
                            let rows = crate::tui::underwater::launch_rows_for_app(app);
                            let entries = rows.len().max(1) as i32;
                            // First arrow lands on the first (Up: last)
                            // row; from there it moves.
                            app.launch.menu_selected = Some(match app.launch.menu_selected {
                                None if delta < 0 => (entries - 1) as usize,
                                None => 0,
                                Some(current) => {
                                    (current as i32 + delta).rem_euclid(entries) as usize
                                }
                            });
                            app.needs_redraw = true;
                            continue;
                        }
                        crate::tui::underwater::LaunchComposerKey::MenuRun => {
                            // Enter with an empty composer while the card is
                            // up runs the highlighted row below, through the
                            // same arms clicks use.
                            let rows = crate::tui::underwater::launch_rows_for_app(app);
                            menu_run_action = Some(crate::tui::underwater::run_launch_card_row(
                                &rows,
                                app.launch.menu_selected,
                            ));
                        }
                        crate::tui::underwater::LaunchComposerKey::Submit => {
                            let chord = composer_submit_chord(key, app.composer_multiline_mode)
                                .unwrap_or(ComposerSubmitChord::Enter);
                            if dispatch_launch_composer_submit(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                chord,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                            app.needs_redraw = true;
                            continue;
                        }
                    }
                }
                if composer_authority {
                    // Fall out of the launch branch: the global chords and
                    // the conversation composer match below handle this key
                    // exactly as they would in a live session.
                } else {
                    // Ctrl+C on the launch screen follows the same two-tap
                    // contract as the session shell (`CtrlCDisposition`):
                    // first press arms the visible exit prompt, the second
                    // inside QUIT_CONFIRMATION_WINDOW exits. Selection
                    // copy and turn cancel cannot apply before a session
                    // exists, so every other disposition arms.
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        match ctrl_c_disposition(app) {
                            CtrlCDisposition::ConfirmExit => {
                                let _ = engine_handle.send(Op::Shutdown).await;
                                return Ok(());
                            }
                            _ => app.arm_quit(),
                        }
                        app.needs_redraw = true;
                        continue;
                    }
                    let action = menu_run_action.take().unwrap_or_else(|| {
                        crate::tui::underwater::handle_launch_key(
                            &mut app.launch,
                            key,
                            launch_locale,
                        )
                    });
                    match action {
                        crate::tui::underwater::LaunchAction::None => {}
                        crate::tui::underwater::LaunchAction::ReturnToSession => {
                            app.launch.dismiss()
                        }
                        crate::tui::underwater::LaunchAction::NewSession => {
                            let result = begin_launch_session(app, None);
                            if apply_command_result(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                result,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                        }
                        crate::tui::underwater::LaunchAction::ResumeSession(session_id) => {
                            crate::tui::underwater::open_launch_resume_confirm(app, &session_id);
                        }
                        crate::tui::underwater::LaunchAction::BrowseSessions => {
                            // A launched command dissolves the card; Esc
                            // out of the picker brings it back.
                            app.launch.dissolve_card(app.ambient_clock_ms);
                            app.view_stack.push(
                                SessionPickerView::new(&app.workspace, app.ui_locale)
                                    .with_current_session(app.current_session_id.as_deref()),
                            );
                        }
                        crate::tui::underwater::LaunchAction::McpRemedy => {
                            type_launch_mcp_remedy(app);
                        }
                        crate::tui::underwater::LaunchAction::McpManager => {
                            app.launch.dissolve_card(app.ambient_clock_ms);
                            open_mcp_extensions(app);
                        }
                        crate::tui::underwater::LaunchAction::Help => {
                            toggle_help_view(app);
                        } // `handle_launch_key` never yields this; the mouse send
                          // path above is the only producer. The arm keeps the
                          // match exhaustive.
                    }
                    app.needs_redraw = true;
                    continue;
                }
            }

            if key.code == KeyCode::Char('x')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && prefill_jobs_cancel_all_if_tasks_sidebar(app)
            {
                continue;
            }

            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
                // When the composer is the active input target (no modal/pager
                // intercepting keys), Ctrl+K performs an emacs-style kill to
                // end-of-line. If the kill is a no-op (cursor at end of empty
                // input), fall through to the existing command palette.
                if app.view_stack.is_empty() && app.kill_to_end_of_line() {
                    continue;
                }
                codewhale_telemetry::session_counters()
                    .bump(codewhale_telemetry::Counter::CommandPaletteOpen);
                app.view_stack.push(CommandPaletteView::new_for_locale(
                    app.ui_locale,
                    build_command_palette_entries(
                        app.ui_locale,
                        &app.skills_dir,
                        app.skills_discovery_mode,
                        &app.workspace,
                        &app.mcp_config_path,
                        app.mcp_snapshot.as_ref(),
                        app.extension_plugin_view().as_ref(),
                    ),
                ));
                continue;
            }

            // Shifted shortcuts toggle the file-tree pane. Keep plain Ctrl+E
            // reserved for the composer end-of-line binding used by shells.
            if key_shortcuts::is_file_tree_toggle_shortcut(&key) {
                if let Some(_state) = app.file_tree.as_mut() {
                    // File tree visible → hide it.
                    app.file_tree = None;
                    app.status_message = Some("File tree closed".to_string());
                } else {
                    // Build the file tree from the current workspace.
                    let state = crate::tui::file_tree::FileTreeState::new(&app.workspace);
                    app.file_tree = Some(state);
                    app.status_message = Some(
                        "File tree: \u{2191}/\u{2193} navigate  Enter select  Esc close"
                            .to_string(),
                    );
                }
                app.needs_redraw = true;
                continue;
            }

            // Ctrl+P opens the fuzzy file-picker overlay. Bound only when the
            // composer is focused (no other modal or inline popup on top) and the
            // engine is not actively streaming a turn.
            if key.code == KeyCode::Char('p')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && visible_slash_menu_entries(app, SLASH_MENU_LIMIT).is_empty()
                && app.view_stack.is_empty()
                && !app.is_loading
            {
                file_picker_relevance::open_file_picker(app);
                continue;
            }

            if matches!(key.code, KeyCode::Char('l') | KeyCode::Char('L'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && app.view_stack.is_empty()
            {
                try_queue_manual_compaction(app, config, &engine_handle, None);
                continue;
            }

            if matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
                && key_shortcuts::has_control_like_modifier(key.modifiers)
                && app.view_stack.is_empty()
            {
                // Release foreground or background shell waits in this session
                // without canceling their commands (#3032/#3859/#6909).
                request_shell_wait_detach(app);
                app.needs_redraw = true;
                continue;
            }

            if shell_binding_for_key(app, &key) == Some(ShellBindingId::ContextInspector) {
                open_context_inspector(app);
                continue;
            }

            if !app.view_stack.is_empty() {
                if key_shortcuts::is_paste_shortcut(&key)
                    && paste_provider_picker_from_clipboard(app)
                {
                    app.needs_redraw = true;
                    continue;
                }
                let closing_work_inspector = app.work_surface.opened.is_some()
                    && app.view_stack.top_kind() == Some(ModalKind::Pager);
                let Some(events) = route_key_to_view_stack(app, key, event_observed_at) else {
                    app.needs_redraw = true;
                    continue;
                };
                clear_work_inspector_after_pager_close(app, closing_work_inspector);
                app.needs_redraw = true;
                if handle_view_events_boxed(
                    terminal,
                    app,
                    config,
                    &task_manager,
                    &mut engine_handle,
                    events,
                )
                .await?
                {
                    return Ok(());
                }
                continue;
            }

            if let Some(slot) = hotbar_slot_from_key(app, &key) {
                if let Some(dispatch) = dispatch_hotbar_slot(app, config, slot)? {
                    match dispatch {
                        HotbarDispatch::Handled => {
                            app.needs_redraw = true;
                        }
                        HotbarDispatch::AppAction(action) => {
                            if apply_command_result(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                commands::CommandResult::action(action),
                            )
                            .await?
                            {
                                return Ok(());
                            }
                            if let Err(err) = persist_pending_work_checkpoint(app).await {
                                app.status_message = Some(format!(
                                    "Hotbar change applied, but its Work receipt is pending ({err})"
                                ));
                            }
                            app.needs_redraw = true;
                        }
                    }
                }
                continue;
            }

            // File-tree navigation: delegated to key_actions module.
            if key_actions::handle_file_tree_key(app, &key) {
                continue;
            }

            if app.is_history_search_active() {
                handle_history_search_key(app, key);
                continue;
            }

            if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'))
                && key.modifiers.contains(KeyModifiers::ALT)
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::SUPER)
            {
                app.start_history_search();
                continue;
            }

            let now = event_observed_at;
            flush_paste_burst_before_composer(app, now);

            // On Windows, AltGr is delivered as `Ctrl+Alt`; treat
            // AltGr-typed chars (e.g. European layouts producing `@`, `\`,
            // `|`) as plain text rather than swallowing them as a modified
            // shortcut. `key_hint::has_ctrl_or_alt` filters AltGr out.
            let has_ctrl_alt_or_super =
                crate::tui::widgets::key_hint::has_ctrl_or_alt(key.modifiers)
                    || key.modifiers.contains(KeyModifiers::SUPER);
            let is_plain_char = matches!(key.code, KeyCode::Char(_)) && !has_ctrl_alt_or_super;
            // Only bare Enter participates in trailing-newline paste-burst
            // protection. Modified Enter chords are deliberate composer
            // actions: flush any buffered text, then route the chord normally
            // so Shift/Alt+Enter newline and Ctrl+Enter steer are never eaten
            // after fast typing or an unbracketed paste.
            let is_plain_enter =
                matches!(key.code, KeyCode::Enter) && key.modifiers == KeyModifiers::NONE;

            // Tool details: Alt+V / Option+V only. Bare `v` always types `v`
            // in every focus state (TUI-DOG-002).
            if shell_binding_for_key(app, &key) == Some(ShellBindingId::ToolDetails) {
                // While a worker is focused the details chord is that
                // worker's bounded Agent Details projection.
                if let Some(agent_id) = app.agent_focus.as_ref().map(|f| f.agent_id.clone()) {
                    if !crate::tui::agent_details::open_agent_details(app, &agent_id) {
                        app.status_message = Some("Agent details are unavailable".to_string());
                    }
                    app.needs_redraw = true;
                    continue;
                }
                open_tool_details_pager(app);
                continue;
            }

            if !is_plain_char
                && !is_plain_enter
                && let Some(pending) = app.flush_paste_burst_before_modified_input_if_enabled()
            {
                app.insert_str(&pending);
            }

            if (is_plain_char || is_plain_enter) && handle_plain_key_before_composer(app, &key, now)
            {
                continue;
            }

            let slash_menu_entries = visible_slash_menu_entries(app, SLASH_MENU_LIMIT);
            let slash_menu_open = !slash_menu_entries.is_empty();
            if slash_menu_open && app.slash_menu_selected >= slash_menu_entries.len() {
                app.slash_menu_selected = slash_menu_entries.len().saturating_sub(1);
            }
            let mention_menu_limit = app.mention_menu_limit;
            let mention_menu_entries =
                crate::tui::file_mention::visible_mention_menu_entries(app, mention_menu_limit);
            let mention_menu_open = !mention_menu_entries.is_empty();
            if mention_menu_open && app.mention_menu_selected >= mention_menu_entries.len() {
                app.mention_menu_selected = mention_menu_entries.len().saturating_sub(1);
            }

            // Cancel a pending Esc-Esc prime as soon as any non-Esc key
            // arrives. Without this the prime would hang around for the
            // rest of the session and the user's next genuine Esc would
            // suddenly skip straight into the backtrack overlay.
            if !matches!(key.code, KeyCode::Esc)
                && matches!(
                    app.backtrack.phase,
                    crate::tui::backtrack::BacktrackPhase::Primed
                )
            {
                app.backtrack.reset();
            }

            // Global keybindings — voice first (⌥V) so it doesn't insert a char.
            if handle_voice_key(app, &key) {
                continue;
            }
            if handle_reasoning_effort_key(app, &key) {
                if let Err(err) = persist_pending_work_checkpoint(app).await {
                    app.status_message = Some(format!(
                        "Reasoning effort changed, but its Work receipt is pending ({err})"
                    ));
                }
                continue;
            }

            // A second, empty Enter after queueing is the portable steer
            // gesture. Handle it before transcript/detail Enter shortcuts so
            // it can never open an unrelated overlay instead (#382).
            let portable_submit_chord = composer_submit_chord(key, app.composer_multiline_mode);
            // Inside the double-tap window every queued message steers,
            // oldest first — the same path Ctrl+Enter takes (one steering
            // path). Outside it, an empty Enter still promotes the oldest
            // queued message.
            if matches!(portable_submit_chord, Some(ComposerSubmitChord::Enter))
                && app.input.trim().is_empty()
                && !slash_menu_open
                && !mention_menu_open
            {
                let steers = app.take_queued_for_double_tap_steer();
                if !steers.is_empty() {
                    let mut pending = steers.into_iter();
                    for message in pending.by_ref() {
                        let steered = attempt_steer_with_queue_fallback(
                            app,
                            config,
                            &engine_handle,
                            message,
                            DispatchRecovery::Queued {
                                restore_index: None,
                            },
                        )
                        .await;
                        if !steered {
                            // The failed message is already restored; the
                            // queue holds exactly it, so the unattempted
                            // remainder appends behind it in order.
                            for message in pending.by_ref() {
                                app.queue_message(message);
                            }
                            break;
                        }
                    }
                    persist_offline_queue_state(app);
                    app.note_footer_hint_used(crate::tui::footer_hints::ENTER_AGAIN);
                    continue;
                }
            }
            if matches!(portable_submit_chord, Some(ComposerSubmitChord::Enter))
                && matches!(
                    app.decide_composer_submit(ComposerSubmitChord::Enter),
                    ComposerSubmitAction::SendQueuedNow
                )
            {
                let _ = send_next_queued_message_now(app, config, &engine_handle).await?;
                continue;
            }

            if let Some(shortcut) = crate::tui::agent_focus::shell_shortcut(
                app,
                &key,
                slash_menu_open || mention_menu_open,
            ) {
                app.note_footer_hint_used(crate::tui::footer_hints::AGENT_ARROWS);
                match shortcut {
                    crate::tui::agent_focus::AgentShellShortcut::FocusAgents => {
                        if !crate::tui::work_surface::enter_agents(app) {
                            open_agents_register(app, &engine_handle).await;
                        }
                    }
                    // `↓ to manage` opens the workflows view while the workbar
                    // shows runs, else the agent register.
                    crate::tui::agent_focus::AgentShellShortcut::ManageAgents => {
                        if app.workflow_runs.is_empty() {
                            open_agents_register(app, &engine_handle).await;
                        } else {
                            crate::tui::views::workflows_manager::open(app);
                        }
                    }
                }
                continue;
            }

            match key.code {
                KeyCode::Enter
                    if key.modifiers == KeyModifiers::NONE
                        && app.input.is_empty()
                        && app.viewport.transcript_selection.is_active()
                        && open_pager_for_selection(app) =>
                {
                    continue;
                }
                KeyCode::Enter
                    if key.modifiers == KeyModifiers::NONE
                        && app.input.is_empty()
                        && detail_target_cell_index(app).is_some()
                        && open_focused_cell_pager(app) =>
                {
                    continue;
                }
                KeyCode::Enter
                    if key.modifiers == KeyModifiers::NONE
                        && app.input.is_empty()
                        && detail_target_cell_index(app)
                            .is_some_and(|idx| app.toggle_tool_run_expansion_at(idx)) =>
                {
                    continue;
                }
                KeyCode::Char('l')
                    if key_shortcuts::alt_nav_modifiers(key.modifiers)
                        && open_pager_for_last_message(app) =>
                {
                    continue;
                }
                _ if key_shortcuts::is_reasoning_detail_shortcut(&key)
                    && open_reasoning_detail_pager(app) =>
                {
                    continue;
                }
                _ if key_shortcuts::is_turn_inspector_shortcut(&key)
                    && open_turn_inspector_pager(app) =>
                {
                    continue;
                }
                // Space toggles fold/unfold of the focused thinking block
                // when the composer is empty. For thinking cells, toggles
                // between summary and full content; for other cells, toggles
                // visibility (#1972, #2348). Uses virtual-cell lookup so
                // in-flight active reasoning works too.
                KeyCode::Char(' ')
                    if key.modifiers == KeyModifiers::NONE && app.input.is_empty() =>
                {
                    let _ = handle_transcript_space(app);
                    continue;
                }
                KeyCode::Char('t') | KeyCode::Char('T')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    toggle_live_transcript_overlay(app);
                    continue;
                }
                KeyCode::Char('1')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && key_shortcuts::has_control_like_modifier(key.modifiers) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Tasks);
                    continue;
                }
                KeyCode::Char('2')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && key_shortcuts::has_control_like_modifier(key.modifiers) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Agents);
                    continue;
                }
                KeyCode::Char('3')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && key_shortcuts::has_control_like_modifier(key.modifiers) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Context);
                    continue;
                }
                KeyCode::Char('4')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && key_shortcuts::has_control_like_modifier(key.modifiers) =>
                {
                    apply_alt_4_shortcut(app, key.modifiers);
                    continue;
                }
                // Rail panel selection via Alt+! / Alt+@ / Alt+# / Alt+$ / Alt+%
                // AltGr on European keyboards emits Ctrl+Alt on Windows, so
                // exclude Ctrl to avoid swallowing AltGr-typed characters
                // like @ (AltGr+0 on French AZERTY) and # (AltGr+3). This
                // matches the has_ctrl_or_alt / is_altgr philosophy in
                // key_hint.rs: treat Ctrl+Alt as AltGr, not a shortcut.
                KeyCode::Char('!')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Tasks);
                    continue;
                }
                KeyCode::Char('@')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Agents);
                    continue;
                }
                KeyCode::Char('#')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Context);
                    continue;
                }
                KeyCode::Char('$') | KeyCode::Char('%')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Files);
                    continue;
                }
                KeyCode::Char('0')
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    apply_alt_0_shortcut(app, key.modifiers);
                    continue;
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Scope the picker to the current workspace so Ctrl+R
                    // never restores a different project's history by
                    // surprise (#1395). Press `a` inside the picker to
                    // broaden to every saved session.
                    app.view_stack.push(
                        SessionPickerView::new(&app.workspace, app.ui_locale)
                            .with_current_session(app.current_session_id.as_deref()),
                    );
                    continue;
                }
                KeyCode::Char('c') | KeyCode::Char('C')
                    if key_shortcuts::is_copy_shortcut(&key) =>
                {
                    let sel = app.selected_text();
                    if !sel.is_empty() {
                        if let Ok(transport) = app.clipboard.write_text_status(&sel) {
                            let receipt = copy_receipt(app, transport, "Copied to clipboard");
                            app.push_status_toast(receipt, StatusToastLevel::Info, None);
                            app.clear_selection();
                        } else {
                            app.push_status_toast("Copy failed", StatusToastLevel::Error, None);
                        }
                    } else {
                        copy_active_selection(app);
                    }
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Four behaviors layered on Ctrl+C in priority order — see
                    // `CtrlCDisposition` for the unit-tested decision table.
                    // 1. selection active → copy + clear (Windows convention,
                    //    #1337); 2. turn in flight → cancel; 3. quit-armed →
                    //    exit; 4. otherwise → arm the 2-second exit prompt.
                    match ctrl_c_disposition(app) {
                        CtrlCDisposition::CopySelection => {
                            copy_active_selection(app);
                            clear_transcript_selection(app);
                        }
                        CtrlCDisposition::CancelTurn => {
                            let compacting = app.is_compacting || app.manual_compaction_queued;
                            if compacting {
                                try_cancel_compaction(app, &engine_handle);
                                if !compact_interrupt_should_stop_turn(app) {
                                    app.disarm_quit();
                                    continue;
                                }
                            }
                            let was_waiting = app.goal_continuation_waiting;
                            engine_handle.cancel();
                            if was_waiting {
                                app.goal_continuation_waiting = false;
                                app.status_message =
                                    Some(app.tr(MessageId::GoalContinuationStopped).to_string());
                                app.disarm_quit();
                                continue;
                            }
                            mark_active_turn_cancelled_locally(app);
                            current_streaming_text.clear();
                            stream_display_clock.reset();
                            let prompt_restored = app.restore_last_submitted_prompt_if_empty();
                            let base = if prompt_restored {
                                "Request cancelled; prompt restored to composer"
                            } else {
                                "Request cancelled"
                            };
                            app.status_message = Some(parent_stop_status(app, base));
                            app.disarm_quit();
                        }
                        CtrlCDisposition::ConfirmExit => {
                            let _ = engine_handle.send(Op::Shutdown).await;
                            return Ok(());
                        }
                        CtrlCDisposition::ArmExit => {
                            app.arm_quit();
                        }
                    }
                }
                KeyCode::Char('d')
                    if key.modifiers.contains(KeyModifiers::CONTROL) && app.input.is_empty() =>
                {
                    let _ = engine_handle.send(Op::Shutdown).await;
                    return Ok(());
                }
                // Agent focus: Esc on an empty composer returns to the main
                // conversation before any other Esc meaning applies.
                KeyCode::Esc
                    if app.agent_focus.is_some()
                        && app.input.is_empty()
                        && !slash_menu_open
                        && !mention_menu_open =>
                {
                    crate::tui::agent_focus::exit_focus(app);
                    continue;
                }
                // Vim composer mode: Esc from Insert/Visual → Normal.
                // This arm runs before the generic Esc handler so Insert mode
                // Esc doesn't accidentally cancel an in-flight request.
                KeyCode::Esc
                    if app.composer.vim_enabled
                        && app.composer.vim_mode != crate::tui::app::VimMode::Normal =>
                {
                    app.vim_enter_normal();
                    continue;
                }
                KeyCode::Esc if app.clear_composer_attachment_selection() => {
                    continue;
                }
                // An idle operator can dismiss the persistent context warning
                // without affecting vim or attachment handling. While a turn
                // is active Esc retains its cancellation meaning.
                KeyCode::Esc
                    if !app.is_loading
                        && app.input.is_empty()
                        && !slash_menu_open
                        && !mention_menu_open
                        && app.dismiss_context_pressure_warning() =>
                {
                    continue;
                }
                KeyCode::Esc if mention_menu_open => {
                    app.mention_menu_hidden = true;
                    app.mention_menu_selected = 0;
                }
                KeyCode::Esc if app.sidebar_hover_tooltip.is_some() => {
                    app.sidebar_hover_tooltip = None;
                    app.needs_redraw = true;
                }
                KeyCode::Esc => {
                    match next_escape_action(app, slash_menu_open) {
                        EscapeAction::CloseSlashMenu => {
                            // A popup-style action wins over backtrack — clear
                            // any prime so a stale Primed state can't jump us
                            // straight into Selecting on the next Esc.
                            app.backtrack.reset();
                            app.close_slash_menu();
                        }
                        EscapeAction::CancelRequest => {
                            app.backtrack.reset();
                            app.note_footer_hint_used(crate::tui::footer_hints::ESC_INTERRUPT);
                            if escape_cancel_request(
                                app,
                                &engine_handle,
                                &mut current_streaming_text,
                                &mut stream_display_clock,
                            ) {
                                continue;
                            }
                        }
                        EscapeAction::PauseCommand => {
                            app.backtrack.reset();
                            pause_pausable_command(app, &engine_handle);
                        }
                        EscapeAction::DiscardQueuedDraft => {
                            app.backtrack.reset();
                            if app.cancel_queued_draft_edit() {
                                app.status_message =
                                    Some("Queued edit canceled; follow-up restored".to_string());
                            }
                        }
                        EscapeAction::DismissPluginCta => {
                            app.backtrack.reset();
                            let _ = app.dismiss_plugin_cta_for_session();
                        }
                        EscapeAction::ClearInput => {
                            app.backtrack.reset();
                            app.edit_in_progress = false;
                            app.clear_input_recoverable();
                            let _ = app.maybe_show_behavioral_tip(
                                crate::tui::behavioral_tips::BehavioralTip::ClearedInputRestore,
                            );
                        }
                        EscapeAction::Noop => {
                            // Nothing else cares about this Esc — route it
                            // through the backtrack state machine. While
                            // streaming or with the live transcript already
                            // open, fall through silently (#133 acceptance:
                            // "during streaming Esc-Esc is a silent no-op").
                            if app.is_loading
                                || app.view_stack.top_kind() == Some(ModalKind::LiveTranscript)
                            {
                                continue;
                            }
                            let total = count_user_history_cells(app);
                            match app.backtrack.handle_esc(total) {
                                crate::tui::backtrack::EscEffect::None => {}
                                crate::tui::backtrack::EscEffect::Prime => {
                                    app.status_message =
                                        Some("Press Esc again to backtrack".to_string());
                                    app.needs_redraw = true;
                                }
                                crate::tui::backtrack::EscEffect::Cancel => {
                                    app.status_message = Some("Backtrack canceled".to_string());
                                    app.needs_redraw = true;
                                }
                                crate::tui::backtrack::EscEffect::OpenOverlay => {
                                    open_backtrack_overlay(app);
                                }
                            }
                        }
                    }
                }
                KeyCode::Up if key.modifiers.contains(KeyModifiers::SUPER) => {
                    app.scroll_up(app.viewport.last_transcript_visible.max(3));
                }
                KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                    app.scroll_up(3);
                }
                KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    app.scroll_up(3);
                }
                KeyCode::Up
                    if key.modifiers.is_empty()
                        && mention_menu_open
                        && app.mention_menu_selected > 0 =>
                {
                    app.mention_menu_selected = app.mention_menu_selected.saturating_sub(1);
                }
                KeyCode::Up if key.modifiers.is_empty() && slash_menu_open => {
                    select_previous_slash_menu_entry(app, slash_menu_entries.len());
                }
                KeyCode::Char('p')
                    if key.modifiers.contains(KeyModifiers::CONTROL) && slash_menu_open =>
                {
                    select_previous_slash_menu_entry(app, slash_menu_entries.len());
                }
                KeyCode::Up
                    if key.modifiers.is_empty()
                        && app.selected_composer_attachment_index().is_some() =>
                {
                    let _ = app.select_previous_composer_attachment();
                }
                KeyCode::Up
                    if key.modifiers.is_empty()
                        && app.cursor_position == 0
                        && !mention_menu_open
                        && !slash_menu_open
                        && app.composer_attachment_count() > 0 =>
                {
                    let _ = app.select_previous_composer_attachment();
                    continue;
                }
                // #85: ↑ edits the most-recent queued message when the composer
                // is idle and the pending-input preview is showing queued work.
                KeyCode::Up
                    if key.modifiers.is_empty()
                        && app.input.is_empty()
                        && app.cursor_position == 0
                        && app.queued_draft.is_none()
                        && !app.queued_messages.is_empty()
                        && !mention_menu_open
                        && !slash_menu_open
                        && app.selected_composer_attachment_index().is_none() =>
                {
                    let _ = app.pop_last_queued_into_draft();
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::SUPER) => {
                    app.scroll_down(app.viewport.last_transcript_visible.max(3));
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                    app.scroll_down(3);
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    app.scroll_down(3);
                }
                KeyCode::Down if key.modifiers.is_empty() && mention_menu_open => {
                    app.mention_menu_selected = (app.mention_menu_selected + 1)
                        .min(mention_menu_entries.len().saturating_sub(1));
                }
                KeyCode::Down if key.modifiers.is_empty() && slash_menu_open => {
                    select_next_slash_menu_entry(app, slash_menu_entries.len());
                }
                KeyCode::Char('n')
                    if key.modifiers.contains(KeyModifiers::CONTROL) && slash_menu_open =>
                {
                    select_next_slash_menu_entry(app, slash_menu_entries.len());
                }
                // Paging and edge motions from the shared vocabulary (#6290),
                // claimed before the unconditional transcript-scroll arms.
                KeyCode::PageUp if key.modifiers.is_empty() && slash_menu_open => {
                    move_slash_menu_selection(
                        app,
                        slash_menu_entries.len(),
                        crate::tui::list_nav::Motion::PagePrev,
                    );
                }
                KeyCode::PageDown if key.modifiers.is_empty() && slash_menu_open => {
                    move_slash_menu_selection(
                        app,
                        slash_menu_entries.len(),
                        crate::tui::list_nav::Motion::PageNext,
                    );
                }
                // Home/End deliberately stay cursor keys while the menu is open:
                // the composer is still the focused input (same as Left/Right
                // and the mention menu), so only vertical travel belongs to
                // the popup.
                KeyCode::Down
                    if key.modifiers.is_empty()
                        && app.selected_composer_attachment_index().is_some() =>
                {
                    let _ = app.select_next_composer_attachment();
                }
                KeyCode::PageUp => {
                    let page = app.viewport.last_transcript_visible.max(1);
                    app.scroll_up(page);
                }
                KeyCode::PageDown => {
                    let page = app.viewport.last_transcript_visible.max(1);
                    app.scroll_down(page);
                }
                KeyCode::Tab => {
                    match dispatch_tab_key(app, &key, &mention_menu_entries, &slash_menu_entries) {
                        TabDispatch::Completion | TabDispatch::Ignored => continue,
                        TabDispatch::ModeCycled {
                            prior_mode,
                            prior_model,
                        } => {
                            if app.mode != prior_mode {
                                sync_mode_update(app, &engine_handle).await;
                            }
                            if app.model != prior_model {
                                let _ = engine_handle
                                    .send(Op::SetModel {
                                        model: app.model.clone(),
                                        mode: app.mode,
                                        route_limits: app.active_route_limits,
                                    })
                                    .await;
                            }
                        }
                    }
                }
                // Transcript-nav shortcuts now require Alt, leaving most bare
                // letters free to insert as text. Requiring Alt is also why
                // none of them asks whether the composer is empty: an Alt
                // chord is never composer text, so `input.is_empty()` there
                // was guessing at focus and only ever broke the shortcut for
                // anyone mid-draft. Before v0.8.30, bare `g`,
                // `G`, `[`, `]`, `?`, and `l` on an empty composer were
                // hijacked for navigation — typing "good" yielded "ood" with
                // no whale and no warning. The Alt-prefixed shortcuts mirror
                // the Alt+R / Alt+C pattern already in use. Shift is
                // permitted for most capital-letter forms.
                KeyCode::Char('g')
                    if key_shortcuts::alt_nav_modifiers(key.modifiers) && !slash_menu_open =>
                {
                    if let Some(anchor) =
                        TranscriptScroll::anchor_for(app.viewport.transcript_cache.line_meta(), 0)
                    {
                        app.viewport.transcript_scroll = anchor;
                    }
                }
                KeyCode::Char('G')
                    if key_shortcuts::alt_nav_modifiers(key.modifiers) && !slash_menu_open =>
                {
                    app.scroll_to_bottom();
                }
                KeyCode::Char('[')
                    if key_shortcuts::alt_nav_modifiers(key.modifiers)
                        && !slash_menu_open
                        && !jump_to_adjacent_tool_cell(app, SearchDirection::Backward) =>
                {
                    app.status_message = Some("No previous tool output".to_string());
                }
                KeyCode::Char(']')
                    if key_shortcuts::alt_nav_modifiers(key.modifiers)
                        && !slash_menu_open
                        && !jump_to_adjacent_tool_cell(app, SearchDirection::Forward) =>
                {
                    app.status_message = Some("No next tool output".to_string());
                }
                // Help chords (Alt+?, F1, Ctrl+/) are handled above via
                // shell_key_routing::is_help_shortcut so printable layout
                // characters stay text.
                // Input handling
                _ if is_composer_newline_key(key, app.composer_multiline_mode)
                    && !(is_plain_enter && (slash_menu_open || mention_menu_open)) =>
                {
                    app.insert_char('\n');
                }
                KeyCode::Enter
                    if key.modifiers == KeyModifiers::NONE
                        && mention_menu_open
                        && crate::tui::file_mention::apply_mention_menu_selection(
                            app,
                            &mention_menu_entries,
                        ) =>
                {
                    continue;
                }
                // Accept Ctrl+Enter when the terminal reports it distinctly.
                // It is deliberately not advertised because several common
                // terminals encode it exactly like bare Enter.
                _ if is_forced_submit_key(key) => {
                    let action = app.decide_composer_submit(ComposerSubmitChord::CtrlEnter);
                    if let Some(input) = app.submit_input() {
                        if handle_bang_shell_input(app, &engine_handle, &input).await? {
                            continue;
                        }
                        if looks_like_slash_command_input(&input) {
                            if execute_command_input(
                                terminal,
                                app,
                                &mut engine_handle,
                                &task_manager,
                                config,
                                &input,
                            )
                            .await?
                            {
                                return Ok(());
                            }
                        } else {
                            let (queued, recovery) = message_from_submitted_input(app, input);
                            dispatch_composer_message(
                                app,
                                config,
                                &engine_handle,
                                queued,
                                recovery,
                                action,
                            )
                            .await?;
                        }
                    }
                }
                KeyCode::Enter => {
                    let action = app.decide_composer_submit(
                        portable_submit_chord.unwrap_or(ComposerSubmitChord::Enter),
                    );
                    // Slash-menu selection, draft consumption, and the
                    // memory/`!`/`/`/message branches are the shared tail the
                    // mouse `[↵]` dispatcher also runs, so keyboard and pointer
                    // submit behavior cannot drift apart.
                    if submit_decided_composer_input(
                        terminal,
                        app,
                        &mut engine_handle,
                        &task_manager,
                        config,
                        action,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                }
                KeyCode::Backspace
                    if key.modifiers.contains(KeyModifiers::SUPER)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_to_start_of_line();
                }
                KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {}
                KeyCode::Backspace
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_word_backward();
                }
                KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {}
                KeyCode::Backspace
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_word_backward();
                }
                KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {}
                KeyCode::Delete
                    if key.modifiers.contains(KeyModifiers::ALT)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_word_forward();
                }
                KeyCode::Delete if key.modifiers.contains(KeyModifiers::ALT) => {}
                KeyCode::Delete
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_word_forward();
                }
                KeyCode::Delete if key.modifiers.contains(KeyModifiers::CONTROL) => {}
                KeyCode::Backspace if !app.remove_selected_composer_attachment() => {
                    app.delete_char();
                }
                KeyCode::Backspace => {}
                KeyCode::Char('h')
                    if key_shortcuts::is_ctrl_h_backspace(&key)
                        && !app.remove_selected_composer_attachment() =>
                {
                    app.delete_char();
                }
                KeyCode::Char('h') if key_shortcuts::is_ctrl_h_backspace(&key) => {}
                KeyCode::Delete if !app.remove_selected_composer_attachment() => {
                    app.delete_char_forward();
                }
                KeyCode::Delete => {}
                _ if key_shortcuts::is_select_all_shortcut(&key) => {
                    app.select_all();
                }
                KeyCode::Left
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && is_word_cursor_modifier(key.modifiers) =>
                {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_word_backward();
                }
                KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_left();
                }
                KeyCode::Left if is_word_cursor_modifier(key.modifiers) => {
                    app.clear_selection();
                    app.move_cursor_word_backward();
                }
                KeyCode::Left => {
                    app.clear_selection();
                    app.move_cursor_left();
                }
                KeyCode::Right
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && is_word_cursor_modifier(key.modifiers) =>
                {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_word_forward();
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_right();
                }
                KeyCode::Right if is_word_cursor_modifier(key.modifiers) => {
                    app.clear_selection();
                    app.move_cursor_word_forward();
                }
                KeyCode::Right => {
                    app.clear_selection();
                    app.move_cursor_right();
                }
                // Selection-extending Home/End. Ctrl+Shift extends to the
                // buffer edge, bare Shift to the logical line edge. These sit
                // above the Ctrl+Home/Ctrl+End transcript-scroll arms so the
                // shifted chords always edit the selection, never the
                // viewport.
                KeyCode::Home
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_start();
                }
                KeyCode::End
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_end();
                }
                KeyCode::Home if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_line_start();
                }
                KeyCode::End if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if app.selection_anchor.is_none() {
                        app.selection_anchor = Some(app.cursor_position);
                    }
                    app.move_cursor_line_end();
                }
                KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(anchor) =
                        TranscriptScroll::anchor_for(app.viewport.transcript_cache.line_meta(), 0)
                    {
                        app.viewport.transcript_scroll = anchor;
                    }
                }
                KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.scroll_to_bottom();
                }
                KeyCode::Home | KeyCode::Char('a')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    app.clear_selection();
                    app.move_cursor_start();
                }
                KeyCode::Home => {
                    app.clear_selection();
                    app.move_cursor_line_start();
                }
                KeyCode::End => {
                    app.clear_selection();
                    app.move_cursor_line_end();
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.clear_selection();
                    app.move_cursor_end();
                }
                _ if handle_composer_alt_word_motion_key(app, key) => {}
                _ if key_shortcuts::is_external_editor_shortcut(&key) => {
                    // Ctrl+Shift+O (or F4 on terminals that cannot report the
                    // shifted chord): spawn $EDITOR on the composer contents
                    // (#91). Plain Ctrl+O belongs exclusively to the Turn
                    // Inspector, even while the composer holds a draft (#4482).
                    // Only fires when no modal is active (the !view_stack
                    // branch above already returns early in that case) and
                    // the composer is the focused input target. We accept the
                    // shortcut whether or not a model turn is streaming —
                    // editing the buffer never disturbs in-flight work.
                    let seed = app.input.clone();
                    let editor_result = match terminal_input.pause_for_child_terminal().await {
                        Err(err) => Err(err),
                        Ok(()) => {
                            let result = prepare_terminal_input_handoff(
                                &terminal_input,
                                &mut pending_terminal_events,
                            )
                            .and_then(|ready| {
                                if ready {
                                    crate::tui::external_editor::spawn_editor_for_input(
                                        terminal,
                                        app.use_alt_screen(),
                                        app.use_mouse_capture,
                                        app.use_bracketed_paste,
                                        &seed,
                                    )
                                } else {
                                    Err(io::Error::new(
                                        io::ErrorKind::Interrupted,
                                        "editor handoff cancelled by pending terminal input",
                                    ))
                                }
                            });
                            terminal_input.resume_after_child_terminal();
                            force_terminal_repaint = true;
                            result
                        }
                    };
                    match editor_result {
                        Ok(crate::tui::external_editor::EditorOutcome::Edited(new)) => {
                            app.apply_external_edit(new);
                            let editor = std::env::var("VISUAL")
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .or_else(|| {
                                    std::env::var("EDITOR")
                                        .ok()
                                        .filter(|s| !s.trim().is_empty())
                                })
                                .unwrap_or_else(|| "vi".to_string());
                            app.status_message = Some(format!("Edited in {editor}"));
                        }
                        Ok(crate::tui::external_editor::EditorOutcome::Unchanged) => {
                            app.status_message = Some("Editor closed (no changes)".to_string());
                        }
                        Ok(crate::tui::external_editor::EditorOutcome::Cancelled) => {
                            app.status_message = Some("Editor cancelled".to_string());
                        }
                        Err(err) => {
                            app.status_message = Some(format!("Editor error: {err}"));
                        }
                    }
                    app.needs_redraw = true;
                }
                KeyCode::Up => {
                    let _ =
                        handle_composer_history_arrow(app, key, slash_menu_open, mention_menu_open);
                }
                KeyCode::Down => {
                    let _ =
                        handle_composer_history_arrow(app, key, slash_menu_open, mention_menu_open);
                }
                // Ctrl+Shift+U is the shifted-Ctrl chord for `/update install`
                // (same family as Ctrl+Shift+A/E/O). It routes through the
                // exact typed-command path, so the managed-install gate and
                // the "already up to date" outcome are inherited from
                // `commands::update` rather than reimplemented here. Placed
                // above the readline Ctrl+U arm so the shifted chord is never
                // swallowed by clear-input.
                _ if key_shortcuts::is_update_install_shortcut(&key) => {
                    if execute_command_input(
                        terminal,
                        app,
                        &mut engine_handle,
                        &task_manager,
                        config,
                        "/update install",
                    )
                    .await?
                    {
                        return Ok(());
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.clear_input_recoverable();
                    let _ = app.maybe_show_behavioral_tip(
                        crate::tui::behavioral_tips::BehavioralTip::ClearedInputRestore,
                    );
                }
                KeyCode::Char('z')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && app.restore_last_cleared_input_if_empty() =>
                {
                    app.status_message = Some("Restored cleared draft".to_string());
                }
                KeyCode::Char('w') | KeyCode::Char('W')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    app.delete_word_backward();
                }
                KeyCode::Char('s')
                | KeyCode::Char('S')
                | KeyCode::Char('g')
                | KeyCode::Char('G')
                    if key.modifiers == KeyModifiers::CONTROL =>
                {
                    // #440: park the current draft to the persistent stash and
                    // clear the composer. Ctrl+G is the terminal-safe alias for
                    // hosts such as Cursor/VS Code that reserve Ctrl+S for Save.
                    // Empty composers are a no-op so a stray shortcut cannot
                    // pollute the file. Surface a toast so the user sees the
                    // confirmation (no-op feels broken otherwise).
                    if !app.input.is_empty() {
                        crate::composer_stash::push_stash(&app.input);
                        if app.queued_draft.is_some() {
                            // Stash the edited text while preserving the
                            // original queued follow-up in its queue slot.
                            let _ = app.cancel_queued_draft_edit();
                        } else {
                            app.clear_input_recoverable();
                        }
                        app.push_status_toast(
                            "Draft stashed — `/stash pop` to restore",
                            StatusToastLevel::Info,
                            Some(3_000),
                        );
                    }
                }
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // #379: context-sensitive Ctrl+Y.
                    // When the composer has content → emacs-style yank
                    // from the kill buffer at the cursor.
                    // When the composer is empty (transcript focus) →
                    // copy the focused cell text to the system clipboard.
                    if app.input.is_empty() && app.view_stack.is_empty() {
                        // `copy_focused_cell` leaves its own receipt, which
                        // names the transport; a toast here said "Copied"
                        // even when only the terminal was asked to copy.
                        app.status_message = None;
                        if !copy_focused_cell(app) && app.status_message.is_none() {
                            app.status_message = Some("No transcript cell to copy".to_string());
                        }
                    } else {
                        app.yank();
                    }
                }
                KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    crate::tui::mouse_ui::cut_selection(app);
                }
                _ if key_shortcuts::is_paste_shortcut(&key) => {
                    app.paste_from_clipboard();
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_mode_update(app, &engine_handle, config, AppMode::Agent).await;
                    continue;
                }
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_yolo_compat_update(app, &engine_handle, config).await;
                    continue;
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_mode_update(app, &engine_handle, config, AppMode::Plan).await;
                    continue;
                }
                KeyCode::Char('A') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_mode_update(app, &engine_handle, config, AppMode::Agent).await;
                    continue;
                }
                KeyCode::Char('Y') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_yolo_compat_update(app, &engine_handle, config).await;
                    continue;
                }
                KeyCode::Char('P') if key.modifiers.contains(KeyModifiers::ALT) => {
                    apply_mode_update(app, &engine_handle, config, AppMode::Plan).await;
                    continue;
                }
                // Vim composer: Normal-mode motion / operator keys.
                // Only fires when vim is enabled, the input is focused (no modal
                // open on top), and the key has no modifier (pure char).
                KeyCode::Char(c)
                    if app.vim_is_normal_mode()
                        && key.modifiers.is_empty()
                        && !slash_menu_open
                        && !mention_menu_open
                        && app.view_stack.is_empty() =>
                {
                    vim_mode::handle_vim_normal_key(app, c);
                    continue;
                }
                // Vim composer: in Visual mode plain chars are ignored
                // (no text insertion until `i` / `a` enters Insert).
                KeyCode::Char(_)
                    if app.vim_is_visual_mode()
                        && key.modifiers.is_empty()
                        && app.view_stack.is_empty() =>
                {
                    // absorb — Visual mode not yet fully implemented
                }
                KeyCode::Char(c) if is_plain_char => {
                    app.insert_char(c);
                }
                KeyCode::Char(_) => {}
                _ => {}
            }

            if !is_plain_char && !is_plain_enter {
                app.paste_burst.deactivate_keep_window();
            }
        }
    }
}

/// Apply one MCP session-boot event. Failures stay on the snapshot (and
/// therefore the session page) rather than as toast-only Status copy.
/// A direct `/mcp` snapshot invalidates only the event generation it
/// superseded. Older spawn-time updates cannot overwrite it, while a later
/// engine-authored generation can continue updating the live surface.
pub(crate) fn apply_mcp_session_boot_event(
    app: &mut App,
    generation: u64,
    snapshot: crate::mcp::McpManagerSnapshot,
    connecting: Vec<String>,
    finished: bool,
) {
    if generation < app.mcp_snapshot_generation
        || (generation == app.mcp_snapshot_generation && app.mcp_snapshot_generation_invalidated)
    {
        return;
    }
    app.mcp_snapshot_generation = generation;
    app.mcp_snapshot_generation_invalidated = false;
    app.mcp_configured_count = snapshot.servers.len();
    app.hotbar_actions.replace_mcp_tools(Some(&snapshot));
    if finished && app.mcp_reload_in_flight {
        // One completion receipt for the explicit reload that started this
        // pass; session boot never sets the flag.
        app.mcp_reload_in_flight = false;
        crate::tui::mcp_routing::add_mcp_message(
            app,
            crate::tui::ui::provider_routes::mcp_reload_summary(&snapshot),
        );
    }
    app.mcp_snapshot = Some(snapshot);
    app.mcp_connecting = connecting;
    app.mcp_initializing = !finished;
    app.needs_redraw = true;
}

pub(crate) async fn run_cache_warmup(app: &App, config: &Config) -> Result<CacheWarmupOutcome> {
    let route = resolve_cache_replay_route(app, config)?
        .validate()
        .map_err(anyhow::Error::msg)?;
    let base_url = route.client.base_url().to_string();
    let reasoning_effort = app
        .reasoning_effort_api_value_for_replay(route.identity.provider, &base_url, &route.model)
        .map(str::to_string);
    let request = MessageRequest {
        model: route.model.clone(),
        messages: app.api_messages.as_ref().clone(),
        max_tokens: CACHE_WARMUP_MAX_TOKENS,
        system: app.system_prompt.clone(),
        tools: app.session.last_tool_catalog.clone(),
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort,
        stream: None,
        temperature: None,
        top_p: None,
    };
    let warmup = build_cache_warmup_request(&request);
    let inspection = inspect_prompt_for_request(&warmup);
    let response =
        tokio::time::timeout(Duration::from_secs(45), route.client.create_message(warmup))
            .await??;
    Ok(CacheWarmupOutcome {
        usage: response.usage,
        provider_identity: route.identity.key.to_string(),
        model: route.model,
        base_url,
        inspection,
    })
}

/// Whether the telemetry disclosure may become a transcript cell now: never
/// while the launch card can still come back, since a cell hides the card,
/// and never under a live active cell, whose tool indices address
/// `history ++ active_cell`.
///
/// A dissolving card is not a departed one: Esc on an empty composer, or
/// leaving the session picker, restores it, and it only renders over an
/// empty history. So the cell waits until the card is dismissed or the
/// conversation has its first entry.
fn telemetry_notice_may_enter_transcript(app: &App) -> bool {
    let card_leaving = !app.launch.visible || !app.history.is_empty();
    let no_live_cell = app
        .active_cell
        .as_ref()
        .is_none_or(crate::tui::active_cell::ActiveCell::is_empty);
    card_leaving && no_live_cell
}

/// Switch a first-run / missing-key session onto a live local Ollama tag.
pub(super) async fn adopt_live_local_ollama_catalog(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    catalog: crate::local_ollama::LiveLocalOllamaCatalog,
) {
    if !app.should_adopt_live_local_ollama() {
        return;
    }
    let Some(tag) = catalog.preferred_tag().map(str::to_string) else {
        return;
    };
    // switch_provider resolves against the lake we just refreshed.
    let identity = match config.builtin_provider_identity(ProviderKind::Ollama) {
        Ok(identity) => identity,
        Err(error) => {
            app.push_status_toast(error, StatusToastLevel::Error, None);
            return;
        }
    };
    let switched = switch_provider(app, engine_handle, config, identity, Some(tag.clone())).await;
    if !switched {
        return;
    }
    app.onboarding_needs_api_key = false;
    app.onboarding_missing_key_recovery = false;
    // A launch with no key opens the provider picker (#6566). The local model
    // just answered that question, so close the picker and its onboarding
    // step rather than leave a stale "connect a model" screen whose Esc would
    // now walk back to the welcome screen.
    if app.onboarding == OnboardingState::Provider {
        if app.view_stack.top_kind() == Some(ModalKind::ProviderPicker) {
            app.view_stack.pop();
        }
        app.onboarding = OnboardingState::None;
    }
    // Say plainly which model is in use and how to change it, instead of the
    // endpoint it was discovered from (#6566).
    let adopted = app
        .tr(MessageId::LocalModelAdopted)
        .replace("{model}", &tag);
    app.status_message = Some(adopted);
    app.needs_redraw = true;
}

pub(crate) async fn run_prepared_dispatch(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    prepare: UserDispatchPrepare,
    recovery: DispatchRecovery,
) -> Result<()> {
    // Unit tests that intentionally omit the production completion mailbox
    // apply the result inline. Run the owned async phase as a task just like
    // production does so its large future is polled from a clean executor
    // stack instead of nesting under the test helper's call chain.
    let apply = tokio::spawn(spawned_dispatch_inner(
        prepare,
        recovery,
        engine_handle.clone(),
    ))
    .await
    .map_err(|err| anyhow::anyhow!("dispatch task was lost: {err}"))?;
    apply(app, engine_handle, config)
}

pub(crate) async fn run_xai_device_login_from_tui(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
) -> Result<bool> {
    pause_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
    )?;
    let login_result = crate::oauth::login(crate::oauth::OAuthProvider::Xai).await;
    resume_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        app.synchronized_output_enabled,
    )?;

    let switched = match login_result {
        Ok(pending) => {
            apply_codewhale_owned_xai_login(
                app,
                engine_handle,
                config,
                pending,
                "xAI device login complete",
            )
            .await
        }
        Err(err) => {
            let message = format!("xAI device login failed: {err}");
            app.add_message(HistoryCell::System {
                content: message.clone(),
            });
            app.status_message = Some(message);
            false
        }
    };
    app.needs_redraw = true;
    Ok(switched)
}

pub(crate) async fn run_claude_login_from_tui(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
) -> Result<bool> {
    pause_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
    )?;
    let result = crate::oauth::login(crate::oauth::OAuthProvider::Claude).await;
    resume_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        app.synchronized_output_enabled,
    )?;
    let switched = match result {
        Ok(pending) => {
            apply_codewhale_owned_login(
                app,
                engine_handle,
                config,
                ProviderKind::Anthropic,
                pending,
                "Claude sign-in complete",
                "Claude sign-in",
            )
            .await
        }
        Err(error) => {
            app.push_status_toast(
                format!("Claude sign-in failed: {error}"),
                StatusToastLevel::Error,
                Some(App::STICKY_ERROR_TTL_MS),
            );
            false
        }
    };
    app.needs_redraw = true;
    Ok(switched)
}

pub(crate) async fn run_plugin_oauth_from_tui(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    provider: String,
    logout: bool,
) -> Result<()> {
    let entry = match crate::plugins::providers::plugin_auth_entry(config, &provider) {
        Ok(entry) => entry,
        Err(error) => {
            app.push_status_toast(
                error.to_string(),
                StatusToastLevel::Error,
                Some(App::STICKY_ERROR_TTL_MS),
            );
            return Ok(());
        }
    };
    let identity = config
        .resolve_provider_pin_identity(&provider)
        .map_err(anyhow::Error::msg)?;
    pause_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
    )?;
    let result: Result<()> = async {
        let base_url = entry.base_url.clone().context("Missing plugin endpoint")?;
        let oauth = entry
            .oauth
            .clone()
            .context("Missing plugin OAuth declaration")?;
        let authority = entry
            .plugin_authority
            .clone()
            .context("Missing plugin review")?;
        if logout {
            let provider = provider.clone();
            let policy = crate::plugins::activation::extension_host_policy_enabled();
            tokio::task::spawn_blocking(move || {
                let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
                crate::plugins::providers::verify_provider_binding(
                    &authority, &provider, &base_url, &oauth, None,
                )
                .map_err(anyhow::Error::msg)?;
                crate::oauth::plugin_oauth_logout(&provider, &base_url, &oauth)
            })
            .await??;
        } else {
            crate::oauth::plugin_oauth_login(provider.clone(), base_url, oauth, authority).await?;
        }
        let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
            identity.provider,
            &provider,
            &config.base_url_for_route(&identity),
        );
        if !logout {
            let mut scoped = config.clone();
            scoped
                .scope_to_provider_identity(&identity)
                .map_err(anyhow::Error::msg)?;
            let client = crate::client::CodewhaleClient::for_catalog_refresh(&scoped)?;
            let delta = client
                .fetch_catalog_delta()
                .await
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            crate::provider_catalog_live::record_success_if_current(&ticket, delta)
                .context("Catalog refresh superseded")?;
        }
        Ok(())
    }
    .await;
    resume_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        app.synchronized_output_enabled,
    )?;
    match result {
        Ok(()) => {
            let message = if logout {
                MessageId::PluginOAuthLocalLogout
            } else {
                MessageId::PluginOAuthReady
            };
            app.push_status_toast(
                tr(app.ui_locale, message).replace("{provider}", &provider),
                StatusToastLevel::Success,
                Some(8_000),
            );
            if !logout {
                open_model_picker_for_provider(app, config, &identity);
            }
        }
        Err(error) => app.push_status_toast(
            format!("{provider}: {error}"),
            StatusToastLevel::Error,
            Some(App::STICKY_ERROR_TTL_MS),
        ),
    }
    app.needs_redraw = true;
    Ok(())
}

pub(crate) async fn run_chatgpt_pkce_login_from_tui(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
) -> Result<bool> {
    pause_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
    )?;
    let login_result =
        crate::oauth::login_with_config(crate::oauth::OAuthProvider::Chatgpt, config).await;
    resume_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        app.synchronized_output_enabled,
    )?;

    let switched = match login_result {
        Ok(pending) => {
            apply_codewhale_owned_chatgpt_login(
                app,
                engine_handle,
                config,
                pending,
                "ChatGPT sign-in complete",
            )
            .await
        }
        Err(err) => {
            let message = format!("ChatGPT sign-in failed: {err}");
            app.add_message(HistoryCell::System {
                content: message.clone(),
            });
            app.status_message = Some(message);
            false
        }
    };
    app.needs_redraw = true;
    Ok(switched)
}

/// OrcaRouter PKCE sign-in from the `/auth orcarouter` command and the provider
/// picker's "Connect with OrcaRouter" option.
///
/// The TUI is suspended for the same reason as ChatGPT/Xai sign-in: the flow
/// prints the consent URL and blocks on a loopback callback, so it must own the
/// terminal. Unlike those flows it returns an [`crate::oauth::OrcaCredential`] —
/// a durable API key — which is stored through the ordinary provider credential
/// path, so the live route ends up identical to the API-key adapter's.
pub(crate) async fn run_orcarouter_pkce_login_from_tui(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
) -> Result<bool> {
    pause_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
    )?;
    let login_result = tokio::task::spawn_blocking(|| {
        let inputs = crate::oauth::OrcaLoginInputs::from_env();
        let mut challenge = crate::oauth::cli_challenge_writer()?;
        crate::oauth::orcarouter_pkce_login(&inputs, challenge.as_mut())
    })
    .await
    .context("OrcaRouter PKCE login worker failed")
    .and_then(|result| result);
    resume_terminal(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        app.synchronized_output_enabled,
    )?;

    let mut login_message = "OrcaRouter sign-in complete".to_string();
    let switched = match login_result {
        Ok(credential) => {
            let scope_note = (!credential.scope_satisfies_purpose()).then(|| {
                format!(
                    "OrcaRouter granted scope \"{}\" while this client asked for \"{}\"; the narrower grant is reused as-is.",
                    credential.granted_scope(),
                    crate::oauth::ORCAROUTER_SCOPE
                )
            });
            match crate::oauth::activate_orcarouter_credential(
                &credential,
                app.config_path.as_deref(),
            ) {
                Ok(saved) => {
                    login_message = format!(
                        "OrcaRouter is ready; stored the key in {}",
                        saved.describe()
                    );
                    if let Some(note) = scope_note {
                        login_message.push('\n');
                        login_message.push_str(&note);
                    }
                    apply_orcarouter_credential_login(app, engine_handle, config).await
                }
                Err(err) => {
                    let message = format!("OrcaRouter sign-in failed: {err:#}");
                    app.add_message(HistoryCell::System {
                        content: message.clone(),
                    });
                    app.status_message = Some(message);
                    false
                }
            }
        }
        Err(err) => {
            let message = format!("OrcaRouter sign-in failed: {err:#}");
            app.add_message(HistoryCell::System {
                content: message.clone(),
            });
            app.status_message = Some(message);
            false
        }
    };
    app.needs_redraw = true;
    if switched {
        app.add_message(HistoryCell::System {
            content: login_message,
        });
    }
    Ok(switched)
}

/// Switch the live route onto OrcaRouter after its credential landed, using the
/// same store the API-key adapter wrote to. The key itself never passes through
/// here — only the identity.
async fn apply_orcarouter_credential_login(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
) -> bool {
    let identity = match config.builtin_provider_identity(ProviderKind::Orcarouter) {
        Ok(identity) => identity,
        Err(reason) => {
            app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
            return false;
        }
    };
    switch_provider(app, engine_handle, config, identity, None).await
}

/// Move held permission receipts into the transcript: those for `tool_id`
/// when given, otherwise every remaining one. Returns whether anything moved.
pub(super) fn flush_gate_receipts_for(app: &mut App, tool_id: Option<&str>) -> bool {
    let (ready, held): (Vec<_>, Vec<_>) = std::mem::take(&mut app.pending_gate_receipts)
        .into_iter()
        .partition(|(id, _)| tool_id.is_none_or(|wanted| id == wanted));
    app.pending_gate_receipts = held;
    let moved = !ready.is_empty();
    for (_, content) in ready {
        app.add_message(HistoryCell::System { content });
    }
    moved
}

/// Open the `/agents` register (the manage view: focus, stop, refresh) and ask
/// the engine for a fresh listing.
async fn open_agents_register(app: &mut App, engine_handle: &EngineHandle) {
    if app.view_stack.top_kind() != Some(ModalKind::SubAgents) {
        let agents = subagent_view_agents(app, &app.subagent_cache);
        app.view_stack
            .push(crate::tui::views::SubAgentsView::for_app(app, agents));
    }
    let _ = engine_handle.send(Op::ListSubAgents).await;
    app.needs_redraw = true;
}

#[cfg(test)]
mod session_boot_event_tests {
    use super::*;
    use crate::mcp::{McpManagerSnapshot, McpServerCapabilityMetadata, McpServerSnapshot};
    use std::path::PathBuf;

    fn server(name: &str, connected: bool) -> McpServerSnapshot {
        McpServerSnapshot {
            name: name.to_string(),
            enabled: true,
            required: false,
            transport: "stdio".to_string(),
            command_or_url: format!("cmd-{name}"),
            connect_timeout: 5,
            execute_timeout: 5,
            read_timeout: 5,
            connected,
            error: None,
            auth_required: false,
            capability_metadata: McpServerCapabilityMetadata::NotObserved,
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }

    fn snapshot(servers: Vec<McpServerSnapshot>) -> McpManagerSnapshot {
        McpManagerSnapshot {
            config_path: PathBuf::from("mcp.json"),
            config_exists: true,
            reload_required: false,
            servers,
        }
    }

    fn test_app() -> App {
        crate::test_support::test_app_with_options(crate::test_support::test_tui_options(
            PathBuf::from("."),
        ))
    }

    #[test]
    fn boot_event_names_every_connecting_server_on_the_app() {
        let mut app = test_app();
        apply_mcp_session_boot_event(
            &mut app,
            1,
            snapshot(vec![server("alpha", false), server("beta", false)]),
            vec!["alpha".into(), "beta".into()],
            false,
        );
        assert!(app.mcp_initializing);
        assert_eq!(app.mcp_connecting, vec!["alpha", "beta"]);
        assert_eq!(app.mcp_configured_count, 2);
        let surface = crate::tui::session_boot::SessionBootSurface::from_app(&app);
        let chip = surface
            .activity_notice(codewhale_localization::Locale::En, 80)
            .map(|notice| notice.text)
            .expect("chip");
        assert!(chip.contains("alpha"), "{chip}");
        assert!(chip.contains("beta"), "{chip}");
        assert!(!chip.to_ascii_lowercase().contains("slack"), "{chip}");
    }

    #[test]
    fn direct_mcp_snapshot_rejects_an_unseen_older_boot_generation() {
        let mut app = test_app();
        assert_eq!(app.mcp_snapshot_generation, 0);
        app.mcp_snapshot = Some(snapshot(vec![server("direct", true)]));
        // The direct engine response carries generation 2 even though the UI
        // has not rendered queued boot generation 1 yet.
        app.mcp_snapshot_generation = 2;
        app.mcp_snapshot_generation_invalidated = true;
        app.mcp_connecting = vec!["alpha".into()];
        apply_mcp_session_boot_event(
            &mut app,
            1,
            snapshot(vec![server("stale", true)]),
            vec!["stale".into()],
            true,
        );
        assert_eq!(app.mcp_connecting, vec!["alpha"]);
        assert_eq!(
            app.mcp_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.servers.first())
                .map(|server| server.name.as_str()),
            Some("direct")
        );

        // A queued event emitted by the direct operation itself is the same
        // generation and cannot replace the already-applied response.
        apply_mcp_session_boot_event(
            &mut app,
            2,
            snapshot(vec![server("same-pass", true)]),
            Vec::new(),
            true,
        );
        assert_eq!(
            app.mcp_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.servers.first())
                .map(|server| server.name.as_str()),
            Some("direct")
        );

        apply_mcp_session_boot_event(
            &mut app,
            3,
            snapshot(vec![server("fresh", true)]),
            Vec::new(),
            true,
        );
        assert_eq!(app.mcp_snapshot_generation, 3);
        assert!(!app.mcp_snapshot_generation_invalidated);
        assert_eq!(
            app.mcp_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.servers.first())
                .map(|server| server.name.as_str()),
            Some("fresh")
        );
    }

    fn translation_test_route() -> crate::cost_status::EffectiveRouteEnvelope {
        crate::cost_status::EffectiveRouteEnvelope {
            provider: crate::config::ProviderKind::Deepseek,
            provider_identity: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
            openrouter_vendor: None,
            billing_surface: crate::pricing::billing_surface_for_route(
                crate::config::ProviderKind::Deepseek,
                Some("https://api.deepseek.com/v1"),
            )
            .map(str::to_string),
            endpoint_fingerprint: crate::cost_status::endpoint_fingerprint(
                "https://api.deepseek.com/v1",
            ),
            provider_live_pricing: None,
            billing_mode: crate::cost_status::RouteBillingMode::Metered,
            dispatched_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn assistant_and_thinking_translation_usage_each_accrue_once() {
        let _scope = crate::cost_status::test_scope();
        let mut app = test_app();
        app.current_session_id = Some("session-translation".to_string());
        app.runtime_turn_id = Some("turn-translation".to_string());
        let usage_a = codewhale_models::Usage {
            input_tokens: 5,
            output_tokens: 2,
            ..codewhale_models::Usage::default()
        };
        let usage_b = codewhale_models::Usage {
            input_tokens: 3,
            output_tokens: 1,
            ..codewhale_models::Usage::default()
        };

        let assistant = TranslationAccountingContext::capture(&app, "assistant", 1).settle(Ok(
            crate::client::TranslationProviderResponse {
                translated: Ok("助理".to_string()),
                route: translation_test_route(),
                usage: Some(usage_a.clone()),
            },
        ));
        let thinking = TranslationAccountingContext::capture(&app, "thinking", 2).settle(Ok(
            crate::client::TranslationProviderResponse {
                translated: Err(anyhow::anyhow!("incomplete: max_tokens")),
                route: translation_test_route(),
                usage: Some(usage_b.clone()),
            },
        ));

        assert_eq!(assistant.usage.as_ref(), Some(&usage_a));
        assert_eq!(thinking.usage.as_ref(), Some(&usage_b));
        assert!(
            thinking.translated.is_err(),
            "semantic rejection is preserved"
        );
        accrue_translation_usage(&mut app, assistant.usage.as_ref().expect("assistant usage"));
        accrue_translation_usage(&mut app, thinking.usage.as_ref().expect("thinking usage"));
        assert_eq!(app.session.total_input_tokens, 8);
        assert_eq!(app.session.total_output_tokens, 3);
        assert_eq!(app.session.total_tokens, 11);

        let pending = crate::cost_status::drain();
        assert_eq!(
            pending.priced_turns.saturating_add(pending.unpriced_turns),
            2,
            "each decoded provider response is audited exactly once"
        );
    }

    #[test]
    fn translation_unreceipted_success_is_marked_once_but_transport_failure_is_not() {
        let _scope = crate::cost_status::test_scope();
        let mut app = test_app();
        app.current_session_id = Some("session-translation-missing-usage".to_string());
        app.runtime_turn_id = Some("turn-translation-missing-usage".to_string());

        for _ in 0..2 {
            let settled = TranslationAccountingContext::capture(&app, "assistant", 7).settle(Ok(
                crate::client::TranslationProviderResponse {
                    translated: Ok("translation remains usable".to_string()),
                    route: translation_test_route(),
                    usage: None,
                },
            ));
            assert_eq!(
                settled.translated.expect("semantic output remains usable"),
                "translation remains usable"
            );
            assert_eq!(settled.usage, None);
        }
        let transport = TranslationAccountingContext::capture(&app, "assistant", 8)
            .settle(Err(anyhow::anyhow!("HTTP 429")));
        assert!(transport.translated.is_err());
        assert_eq!(transport.usage, None);

        let pending = crate::cost_status::drain();
        assert_eq!(pending.priced_turns, 0);
        assert_eq!(
            pending.unpriced_turns, 1,
            "stable response id dedupes replay"
        );
        assert_eq!(pending.cny_unpriced_turns, 1);
        assert!(
            pending
                .unpriced_reasons
                .contains("provider_success_missing_usage")
        );
    }

    #[test]
    fn late_translation_delivery_isolated_from_new_session_or_turn() {
        let mut app = test_app();
        app.current_session_id = Some("session-a".to_string());
        app.runtime_turn_id = Some("turn-a".to_string());
        let (session, turn) = translation_origin(&app);
        assert!(translation_origin_is_current(
            &app,
            session.as_deref(),
            turn.as_deref()
        ));

        app.current_session_id = Some("session-b".to_string());
        assert!(!translation_session_is_current(&app, session.as_deref()));
        assert!(!translation_origin_is_current(
            &app,
            session.as_deref(),
            turn.as_deref()
        ));
        app.current_session_id = Some("session-a".to_string());
        app.runtime_turn_id = Some("turn-b".to_string());
        assert!(
            translation_session_is_current(&app, session.as_deref()),
            "same-session late usage still belongs in session totals"
        );
        assert!(!translation_origin_is_current(
            &app,
            session.as_deref(),
            turn.as_deref()
        ));

        let usage = codewhale_models::Usage {
            input_tokens: 4,
            output_tokens: 2,
            ..codewhale_models::Usage::default()
        };
        if translation_session_is_current(&app, session.as_deref()) {
            accrue_translation_usage(&mut app, &usage);
        }
        assert_eq!(app.session.total_tokens, 6);
        app.current_session_id = Some("session-b".to_string());
        if translation_session_is_current(&app, session.as_deref()) {
            accrue_translation_usage(&mut app, &usage);
        }
        assert_eq!(
            app.session.total_tokens, 6,
            "cross-session late usage must not pollute the new session"
        );

        let shared_prefix = "x".repeat(300);
        app.current_session_id = Some(format!("{shared_prefix}:old"));
        app.runtime_turn_id = Some("turn-long".to_string());
        let (long_session, long_turn) = translation_origin(&app);
        app.current_session_id = Some(format!("{shared_prefix}:new"));
        assert!(
            !translation_origin_is_current(&app, long_session.as_deref(), long_turn.as_deref()),
            "fixed fingerprints must distinguish ids with the same long prefix"
        );
    }
}

#[cfg(test)]
mod launch_session_sync_tests {
    use super::await_engine_session_sync;
    use crate::config::Config;
    use crate::core::engine::{Engine, EngineConfig};
    use crate::core::ops::Op;
    use crate::extension_host::{
        ExtensionHostManager, ExtensionHostOptions, TestManagerGuard, caller_view,
    };
    use crate::features::Feature;
    use crate::plugins::PluginRegistry;
    use crate::plugins::activation::TestPolicyGuard;
    use codewhale_config::AppMode;
    use std::sync::Arc;

    /// A submit on the startup screen begins a session and runs its input in
    /// one keypress. The extension host knows this caller by the engine's
    /// session id, which only moves when the engine processes the sync: before
    /// the wait the App's new id finds no caller (the 0.10.1 "Unknown command"
    /// on a plugin command's first use), after it the caller is found.
    #[tokio::test]
    async fn launch_submit_waits_for_the_engine_to_install_the_new_session() {
        // No native code runs here: the caller identity is all this needs.
        let _policy = TestPolicyGuard::extension_host(false);
        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
        let _manager = TestManagerGuard::install(Arc::clone(&manager));
        let workspace = tempfile::tempdir().expect("tempdir");
        let mut engine_config = EngineConfig {
            workspace: workspace.path().to_path_buf(),
            session_id: Some("startup-session".to_string()),
            plugin_registry: Some(Arc::new(PluginRegistry::empty(workspace.path()))),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        };
        engine_config.features.enable(Feature::ExtensionHost);
        let model = engine_config.model.clone();
        let (engine, handle) = Engine::new(engine_config, &Config::default());
        let run = tokio::spawn(engine.run());
        assert!(
            caller_view(workspace.path(), Some("startup-session"), None).is_some(),
            "the engine attaches under the session it was started with"
        );

        handle
            .send(Op::SyncSession {
                session_id: Some("launch-session".to_string()),
                messages: Vec::new(),
                system_prompt: None,
                system_prompt_override: false,
                model,
                workspace: workspace.path().to_path_buf(),
                mode: AppMode::Agent,
            })
            .await
            .expect("sync session");
        assert!(
            caller_view(workspace.path(), Some("launch-session"), None).is_none(),
            "queued is not installed: the engine has not run the sync yet"
        );

        await_engine_session_sync(&handle).await;
        assert!(
            caller_view(workspace.path(), Some("launch-session"), None).is_some(),
            "after the wait the new session resolves to this engine's caller"
        );
        assert!(caller_view(workspace.path(), Some("startup-session"), None).is_none());

        run.abort();
    }
}

#[cfg(test)]
mod telemetry_notice_tests {
    use super::telemetry_notice_may_enter_transcript;

    #[test]
    fn telemetry_notice_waits_for_the_launch_card_to_leave() {
        let mut app = crate::test_support::test_app_with_options(
            crate::test_support::test_tui_options(std::env::temp_dir()),
        );
        app.launch.visible = true;
        app.launch.dissolve_started_ms = None;
        assert!(
            !telemetry_notice_may_enter_transcript(&app),
            "a transcript cell would hide the launch card's no-model line"
        );
        // A first keystroke only starts the dissolve; Esc on an empty
        // composer (or leaving the picker) restores the card, which renders
        // only over an empty history. A cell now would strand it.
        app.launch.dissolve_card(0);
        assert!(!telemetry_notice_may_enter_transcript(&app));
        app.launch.restore_card();
        assert!(crate::tui::widgets::should_render_empty_state(&app));
        // Once the conversation has an entry, the card cannot come back.
        app.launch.dissolve_card(0);
        app.add_message(super::HistoryCell::System {
            content: "first entry".to_string(),
        });
        assert!(telemetry_notice_may_enter_transcript(&app));
        app.history.clear();
        app.launch.visible = false;
        app.launch.dissolve_started_ms = None;
        assert!(telemetry_notice_may_enter_transcript(&app));
    }
}

#[cfg(test)]
mod fleet_workers_status_tests {
    use super::current_session_fleet_workers_status;
    use codewhale_localization::Locale;

    #[test]
    fn current_session_fleet_worker_status_keeps_the_english_session_boundary() {
        assert_eq!(
            current_session_fleet_workers_status(Locale::En, 3),
            "Agents in this session: 3 total"
        );
    }
}

/// Per-tick budget for the runtime store-failure tap. These events are rare;
/// the bound only keeps a burst from starving the frame.
const RUNTIME_STORE_FAILURE_DRAIN_BUDGET: usize = 64;

/// Drain the background runtime's event tap and show every
/// `runtime.store_failure` (#5931). Other runtime events keep their own
/// consumers (the task timeline, SSE); this reads only the operator's fault.
fn drain_runtime_store_failures(
    app: &mut App,
    rx: &mut Option<tokio::sync::broadcast::Receiver<crate::runtime_threads::RuntimeEventRecord>>,
) -> bool {
    use tokio::sync::broadcast::error::TryRecvError;
    let Some(receiver) = rx.as_mut() else {
        return false;
    };
    let mut shown = false;
    for _ in 0..RUNTIME_STORE_FAILURE_DRAIN_BUDGET {
        match receiver.try_recv() {
            Ok(event) => shown |= show_runtime_store_failure(app, &event),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped,
                    "runtime event tap lagged; a store-failure notice may have been missed"
                );
            }
            Err(TryRecvError::Closed) => {
                *rx = None;
                break;
            }
        }
    }
    shown
}

/// Show one `runtime.store_failure` event as a warning toast and a transcript
/// line that names the file and the next action. Any other event is ignored.
pub(crate) fn show_runtime_store_failure(
    app: &mut App,
    event: &crate::runtime_threads::RuntimeEventRecord,
) -> bool {
    if event.event != crate::runtime_threads::RUNTIME_STORE_FAILURE_EVENT {
        return false;
    }
    let notice = match serde_json::from_value::<crate::runtime_threads::RuntimeStoreFailureNotice>(
        event.payload.clone(),
    ) {
        Ok(notice) => notice,
        Err(error) => {
            tracing::warn!(%error, "runtime store failure notice had an unreadable payload");
            return false;
        }
    };
    let message = runtime_store_failure_notice(app, &notice);
    app.push_status_toast(message.clone(), StatusToastLevel::Warning, Some(12_000));
    app.add_message(HistoryCell::System { content: message });
    true
}

/// Text for a runtime store fault: the record, the file, the root cause, and
/// the remedy the failed operation calls for.
pub(crate) fn runtime_store_failure_notice(
    app: &App,
    notice: &crate::runtime_threads::RuntimeStoreFailureNotice,
) -> String {
    use crate::runtime_threads::RuntimeStoreOperation;
    let id = match notice.failure.operation {
        RuntimeStoreOperation::Write => MessageId::RuntimeStoreUnwritableNotice,
        RuntimeStoreOperation::Read | RuntimeStoreOperation::Parse => {
            MessageId::RuntimeStoreUnreadableNotice
        }
    };
    app.tr(id)
        .replace(
            "{record}",
            &format!(
                "{} {}",
                notice.failure.record_kind, notice.failure.record_id
            ),
        )
        .replace("{path}", &notice.failure.path.display().to_string())
        .replace("{reason}", &notice.reason)
}

/// The fields of one [`EngineEvent::ApprovalRequired`], moved out of the
/// drain so the handler can be driven directly by tests.
pub(super) struct ApprovalRequiredEvent {
    pub id: String,
    pub tool_name: String,
    pub description: String,
    pub input: serde_json::Value,
    pub approval_key: String,
    pub approval_grouping_key: String,
    pub intent_summary: Option<String>,
    pub approval_force_prompt: bool,
}

/// Handle one approval request from the engine: resolve its disposition and,
/// when a person must decide, raise the card. A child agent's card is also
/// recorded in the pending store so hiding it never loses it (approvals C1).
pub(super) async fn handle_approval_required_event(
    app: &mut App,
    engine_handle: &EngineHandle,
    config: &Config,
    event: ApprovalRequiredEvent,
) {
    let ApprovalRequiredEvent {
        id,
        tool_name,
        description,
        input,
        approval_key,
        approval_grouping_key,
        intent_summary,
        approval_force_prompt,
    } = event;
    // A count and nothing else. The tool name, the
    // description, the input, and the matched rule are all
    // user- or model-authored strings.
    codewhale_telemetry::session_counters().bump(codewhale_telemetry::Counter::ApprovalModalShown);
    // Mirror semantics: the approval is always shown
    // locally. When the web mirror is attached to this
    // turn, ALSO record it so the web can answer; the
    // first decision wins (`resolve_pending_approval`
    // vs `take_pending_approval`).
    let shared_with_web = if app.remote_control.can_share_approval_with_web() {
        app.remote_control.record_remote_approval(
            &id,
            &tool_name,
            &description,
            &input,
            &approval_key,
            intent_summary.as_deref(),
        );
        true
    } else {
        false
    };
    use crate::core::authority::ApprovalRequestDisposition;
    // One disposition path for every ApprovalRequired (#4412):
    // session denial, Full Access policy hold, session/FA
    // auto-approve, Never posture, or modal prompt.
    match resolve_ui_approval_disposition(
        app,
        &tool_name,
        &approval_grouping_key,
        &approval_key,
        approval_force_prompt,
    ) {
        ApprovalRequestDisposition::AutoDenySessionDenied => {
            // The user already denied a matching approval key
            // during this process; auto-deny so the
            // model's retry loop doesn't keep re-prompting
            // (#360).
            auto_deny_session_approval(app, engine_handle, &id, &tool_name, &approval_key).await;
        }
        ApprovalRequestDisposition::AutoDenyFullAccessPolicyHold => {
            log_sensitive_event(
                "tool.approval.auto_deny_full_access_policy",
                serde_json::json!({
                    "tool_name": tool_name,
                    "session_id": app.current_session_id,
                    "mode": app.mode.label(),
                }),
            );
            let _ = engine_handle
                .deny_tool_call_by(id.clone(), crate::approval_log::ApprovalDecider::Posture)
                .await;
            let notice = app
                .tr(MessageId::ApprovalFullAccessPolicyBlocked)
                .replace("{tool}", &tool_name);
            app.push_status_toast(notice, StatusToastLevel::Warning, Some(12_000));
        }
        ApprovalRequestDisposition::AutoApprove => {
            log_sensitive_event(
                "tool.approval.auto_approve_session",
                serde_json::json!({
                    "tool_name": tool_name,
                    "approval_key": approval_key,
                    "session_id": app.current_session_id,
                    "mode": app.mode.label(),
                }),
            );
            let by = auto_approval_decider(app, approval_force_prompt);
            let _ = engine_handle.approve_tool_call_by(id.clone(), by).await;
        }
        ApprovalRequestDisposition::AutoDenyAutoReview => {
            log_sensitive_event(
                "tool.approval.auto_deny_auto_review",
                serde_json::json!({
                    "tool_name": tool_name,
                    "session_id": app.current_session_id,
                    "mode": app.mode.label(),
                }),
            );
            let _ = engine_handle
                .deny_tool_call_by(id.clone(), crate::approval_log::ApprovalDecider::Posture)
                .await;
            let held =
                crate::tui::gate_receipts::auto_review_held_receipt(app.ui_locale, &tool_name);
            app.add_message(HistoryCell::System {
                content: held.clone(),
            });
            app.push_status_toast_record(
                StatusToast::new(held, StatusToastLevel::Warning, Some(12_000))
                    .for_event(format!("approval-held:{id}")),
            );
        }
        ApprovalRequestDisposition::AutoDenyNeverPosture => {
            log_sensitive_event(
                "tool.approval.auto_deny",
                serde_json::json!({
                    "tool_name": tool_name,
                    "session_id": app.current_session_id,
                    "mode": app.mode.label(),
                }),
            );
            let _ = engine_handle
                .deny_tool_call_by(id.clone(), crate::approval_log::ApprovalDecider::Posture)
                .await;
            app.push_status_toast_record(
                StatusToast::new(
                    app.tr(MessageId::ApprovalNeverPostureBlocked)
                        .replace("{tool}", &tool_name),
                    StatusToastLevel::Warning,
                    Some(12_000),
                )
                .for_event(format!("approval-blocked:{id}")),
            );
        }
        ApprovalRequestDisposition::Prompt => {
            let tool_input = input;

            push_approval_request_view(
                app,
                &id,
                &tool_name,
                &description,
                &tool_input,
                &approval_key,
                &approval_grouping_key,
                intent_summary.as_deref(),
                config.approval_default_selection(),
                config.approval_timeout(),
            );
            if let Some(agent_id) = crate::tui::pending_requests::child_agent_id(&id) {
                crate::tui::pending_requests::record(
                    app,
                    &id,
                    crate::tui::pending_requests::PendingChildRequest {
                        agent_id: agent_id.to_string(),
                        tool_name: tool_name.clone(),
                        description: description.clone(),
                        input: tool_input.clone(),
                        approval_key: approval_key.clone(),
                        approval_grouping_key: approval_grouping_key.clone(),
                        intent_summary: intent_summary.clone(),
                        requested_at: Instant::now(),
                    },
                );
            }
            log_sensitive_event(
                "tool.approval.prompted",
                serde_json::json!({
                    "tool_name": tool_name,
                    "description": description,
                    "session_id": app.current_session_id,
                    "mode": app.mode.label(),
                }),
            );
            let payload = notifications::approval_needed_payload(app.ui_locale, &tool_name);
            if let Some((method, _, _)) = crate::tui::notifications::settings(config) {
                let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                // #4834: the tool *description* is the
                // pending command. It stays in the
                // terminal, where the user can read it
                // in context; the banner names only the
                // tool. Copy is centralized (#5041) so
                // the action-first phrasing is tested.
                crate::tui::notifications::notify_done(
                    method,
                    in_tmux,
                    &payload,
                    Duration::ZERO,
                    Duration::ZERO,
                );
            }
            let mut notice = payload.headline().to_string();
            if shared_with_web {
                notice.push_str(" · ");
                notice.push_str(&app.tr(MessageId::NotificationDecisionWebHint));
            }
            app.push_status_toast_record(
                StatusToast::new(notice, StatusToastLevel::Warning, Some(12_000))
                    .for_action(id.clone()),
            );
        }
    }
}

/// Route one key to the view stack, unless the terminal saw it before the
/// approval card on top became visible (approvals M2): such a key was typed
/// at something else — often the card that was just answered above this
/// one — and must never answer this card. `None` means it was discarded.
pub(super) fn route_key_to_view_stack(
    app: &mut App,
    key: KeyEvent,
    observed_at: Instant,
) -> Option<Vec<ViewEvent>> {
    if app.view_stack.key_predates_top_approval(observed_at) {
        return None;
    }
    Some(app.view_stack.handle_key(key))
}

/// Keep only the Engine's retry receipts in the existing transcript. Ordinary
/// status/footer behavior and internal/model-only status projection stay intact.
pub(super) fn apply_engine_status(app: &mut App, message: String) -> bool {
    // The account-profile fallback notice is sent once per engine; keep it in
    // the transcript so the next turn's status line cannot erase it.
    let retain = crate::core::events::is_retry_status_receipt(&message)
        || app.tr(MessageId::ProfileConstitutionUnavailableLocal) == message.as_str();
    if retain {
        app.add_message(HistoryCell::System {
            content: message.clone(),
        });
    }
    app.status_message = Some(message);
    retain
}
