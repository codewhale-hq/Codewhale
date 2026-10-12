//! Opening, closing, and refreshing the overlay surfaces (pagers, inspectors,
//! backtrack, hotbar) layered over the transcript.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

pub(crate) fn open_setup_checkpoint_if_due(
    app: &mut App,
    config: &Config,
    skip_onboarding: bool,
) -> bool {
    if skip_onboarding {
        if crate::tui::setup::should_open_update_checkpoint(app, config)
            && let Err(err) = crate::tui::setup::defer_update_checkpoint_for_app(app, config)
        {
            tracing::warn!(
                target: "tui::setup",
                "failed to record deferred setup checkpoint: {err}"
            );
        }
        return false;
    }
    if app.onboarding != crate::tui::app::OnboardingState::None
        || app.view_stack.top_kind() == Some(ModalKind::SetupWizard)
        || !crate::tui::setup::should_open_update_checkpoint(app, config)
    {
        return false;
    }

    // A fresh wizard invalidates any in-flight model draft from a prior one.
    let _ = app.next_draft_gen();
    app.view_stack
        .push(crate::tui::setup::SetupWizardView::new_checkpoint_for_app(
            app, config,
        ));
    true
}

/// Ctrl+O — the one gesture that selects "explore offline".
///
/// A plain letter cannot be used: the provider picker key-entry stage is a
/// text field and would swallow it into the draft secret.
pub(crate) fn is_explore_offline_shortcut(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Open the one canonical theme surface (`/theme`) as an ordinary modal.
///
/// Onboarding's ready screen exposes this as the optional "customize later"
/// secondary action: onboarding is finished first, so the picker is a normal
/// modal over the live product. It reuses the same `ThemePickerView`, so the
/// preview is live and transactional (Enter persists, Escape reverts) and
/// there is no second theme registry.
pub(crate) fn open_theme_picker(app: &mut App) {
    if app.view_stack.top_kind() == Some(ModalKind::ThemePicker) {
        return;
    }
    let original = app.theme_id.name().to_string();
    app.view_stack
        .push_boxed(crate::tui::theme_picker::ThemePickerView::boxed(
            original,
            app.ui_locale,
            app.background_color_override,
        ));
    app.needs_redraw = true;
}

/// Toggle the one canonical keyboard-oriented Help index.
///
/// Launch, onboarding, and the live shell all route here so opening Help from
/// a startup row cannot drift into a second catalog or ordering policy.
pub(crate) fn toggle_help_view(app: &mut App) {
    if app.view_stack.top_kind() == Some(ModalKind::Help) {
        app.view_stack.pop();
    } else {
        let help = HelpView::new_for_app(app, true).with_groups_expanded(app.help_expand_groups);
        app.view_stack.push(help);
    }
    app.needs_redraw = true;
}

/// After a shared view closes over the launch screen, bring the launch card
/// back: Esc out of the resume picker or the changelog pager returns to the
/// card rather than stranding the user on an empty stage. A view that began
/// a session (`launch.visible == false`) or a draft in the composer leaves
/// the dissolved card alone.
pub(crate) fn restore_launch_card_after_view_close(app: &mut App) {
    if app.launch.visible
        && app.view_stack.is_empty()
        && app.launch.dissolve_started_ms.is_some()
        && app.input.is_empty()
    {
        app.launch.restore_card();
        app.needs_redraw = true;
    }
}

/// Choose which durable-task summaries should appear in the Work
/// sidebar's Tasks panel.
///
/// Tasks stamped with the current session owner stay visible on that session's
/// live surface. Tasks owned by a different session stay in explicit history
/// (`/tasks`) instead of appearing as live workspace work. Legacy unowned
/// records fall back to the v0.9.1 timestamp gate: active tasks remain visible,
/// while terminal receipts must have both creation and completion times inside
/// this TUI session. Durable tasks are stored per user rather than per TUI
/// process, and startup recovery can stamp an old running record with a fresh
/// `ended_at`. Treating that as a current receipt makes a new same-workspace
/// instance look failed (#4416).
///
/// A terminal task missing `ended_at` is treated as not current and
/// dropped: durable tasks always stamp `ended_at` when they reach a
/// terminal state, so absence of it indicates a record from a much
/// older schema and isn't worth surfacing.
pub(crate) fn select_work_sidebar_tasks(
    tasks: Vec<TaskSummary>,
    session_started_at: chrono::DateTime<chrono::Utc>,
    current_session_id: Option<&str>,
) -> Vec<TaskSummary> {
    tasks
        .into_iter()
        .filter(|task| {
            let owner_matches_current = current_session_id
                .zip(task.owner_session_id.as_deref())
                .is_some_and(|(current, owner)| current == owner);
            let owned_by_other_session = current_session_id.is_some()
                && task
                    .owner_session_id
                    .as_deref()
                    .is_some_and(|owner| Some(owner) != current_session_id);
            if owned_by_other_session {
                return false;
            }
            match task.status {
                TaskStatus::Queued | TaskStatus::Running => {
                    owner_matches_current || task.owner_session_id.is_none()
                }
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled => {
                    // A terminal task missing `ended_at` predates the schema
                    // that always stamps it; never surface it as a live
                    // receipt, even when it names this session as owner.
                    if task.ended_at.is_none() {
                        return false;
                    }
                    owner_matches_current
                        || (task.owner_session_id.is_none()
                            && task.created_at >= session_started_at
                            && task
                                .ended_at
                                .is_some_and(|ended_at| ended_at >= session_started_at))
                }
            }
        })
        .collect()
}

pub(crate) fn toggle_settings_view(app: &mut App) {
    if app.view_stack.contains_kind(ModalKind::Config) {
        app.view_stack.pop_through_kind(ModalKind::Config);
    } else {
        app.view_stack.push(ConfigView::new_for_app(app));
    }
    app.needs_redraw = true;
}

pub(crate) fn clear_work_inspector_after_pager_close(app: &mut App, was_work_inspector: bool) {
    if was_work_inspector && app.view_stack.top_kind() != Some(ModalKind::Pager) {
        app.work_surface.opened = None;
    }
}

pub(crate) fn hotbar_slot_from_key(app: &App, key: &event::KeyEvent) -> Option<u8> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if !('1'..='8').contains(&c) {
        return None;
    }
    let slot = c.to_digit(10).and_then(|digit| u8::try_from(digit).ok())?;

    if key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
    {
        if app.onboarding != OnboardingState::None
            || !app.view_stack.is_empty()
            || app.is_history_search_active()
            || !visible_slash_menu_entries(app, SLASH_MENU_LIMIT).is_empty()
        {
            return None;
        }

        return Some(slot);
    }

    None
}

pub(crate) async fn cycle_permission_posture(
    app: &mut App,
    config: &mut Config,
    engine_handle: &EngineHandle,
) {
    let control = config.approval_policy_control(
        app.config_path.as_deref(),
        app.config_profile.as_deref(),
        &app.workspace,
    );
    let changed = if control == crate::config::ApprovalPolicyControl::RootConfig {
        app.cycle_root_approval_posture()
    } else {
        app.cycle_approval_posture()
    };
    if changed {
        if control == crate::config::ApprovalPolicyControl::RootConfig {
            config.approval_policy = None;
        }
        sync_mode_update(app, engine_handle).await;
        refresh_config_view_if_open(app, "permission_posture");
    }
}

/// Open the one canonical provider setup surface for onboarding. Fresh
/// onboarding opens on the full provider catalog, hosted providers included
/// (#5563). Local runtimes are listed, and `L` filters to them, only once one
/// answered the startup probe or is already configured. Missing-key
/// recovery instead focuses the already-configured route so an exact Kimi
/// Code K3 configuration can expose its plan route before a secret is entered.
/// Either way the picker opens on the navigable list (#4763): onboarding never
/// drops a user straight into a key/OAuth prompt for a route they were not
/// shown.
pub(crate) async fn open_onboarding_provider_picker(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    recover_configured_route: bool,
) {
    if app.onboarding != OnboardingState::Provider
        || app.view_stack.top_kind() == Some(ModalKind::ProviderPicker)
    {
        return;
    }
    let runtime_status = query_provider_runtime_status(engine_handle).await;
    app.view_stack.push(
        crate::tui::provider_picker::ProviderPickerView::new_for_onboarding(
            app.api_provider,
            recover_configured_route.then(|| app.onboarding_provider.as_str().into()),
            config,
            runtime_status,
        )
        .with_locale(app.ui_locale)
        .with_local_runtime_detected(app.local_runtime_detected)
        .with_provider_health(&app.provider_health),
    );
    app.needs_redraw = true;
}

/// Open the existing provider picker from the post-onboarding launch screen.
///
/// The launch row contributes only a `ProviderSetupIntent`; provider catalog,
/// health, selection memory, and apply semantics remain owned by
/// `ProviderPickerView` and the normal provider event handlers.
pub(crate) async fn open_launch_provider_picker(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) {
    if app.view_stack.top_kind() == Some(ModalKind::ProviderPicker) {
        return;
    }
    let runtime_status = query_provider_runtime_status(engine_handle).await;
    app.view_stack.push(
        crate::tui::provider_picker::ProviderPickerView::new_with_runtime_status_and_memory(
            app.api_provider,
            config,
            runtime_status,
            app.provider_picker_memory.as_ref(),
        )
        .with_locale(app.ui_locale)
        .with_provider_health(&app.provider_health),
    );
    app.needs_redraw = true;
}

/// Open the existing provider/route surface from any shell entry point.
///
/// The chrome and `/provider` command both delegate here, so they expose the
/// same picker without duplicating catalog or runtime-readiness facts. A
/// picker preview remains non-authoritative until its normal apply handler
/// commits a route.
pub(crate) async fn open_provider_picker(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) {
    if app.onboarding == OnboardingState::Provider {
        let recover_configured_route = app.onboarding_recovers_configured_route();
        open_onboarding_provider_picker(app, config, engine_handle, recover_configured_route).await;
    } else {
        open_launch_provider_picker(app, config, engine_handle).await;
    }
}

pub(crate) fn open_text_pager(app: &mut App, title: String, content: String) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    app.view_stack.push(PagerView::from_text(
        title,
        &content,
        width.saturating_sub(2),
    ));
}

/// Open a unified diff in a pager, rendered like an edit in the transcript.
pub(crate) fn open_diff_pager(app: &mut App, title: String, diff: &str) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    // A patch is file content. Keep the terminal-injection boundary
    // `PagerView::from_text` applies to every other pager body.
    let mut sanitized = String::with_capacity(diff.len());
    crate::tui::osc8::strip_ansi_into(diff, &mut sanitized);
    let lines = crate::tui::diff_render::render_diff(&sanitized, width.saturating_sub(2));
    app.view_stack
        .push(PagerView::new(title, lines).with_copy_text(sanitized));
}

pub(crate) fn open_context_inspector(app: &mut App) {
    app.view_stack.push(ContextInspectorView::new(app));
}

pub(crate) fn open_external_url(url: &str) -> Result<()> {
    crate::utils::open_url(url)
}

/// Pull the latest snapshot of cells / revisions / render options into the
/// live transcript overlay sitting on top of the view stack. No-op if the
/// top view isn't a `LiveTranscriptOverlay`.
pub(crate) fn refresh_live_transcript_overlay(app: &mut App) {
    // Pop+push lets us hold &mut to the overlay while also borrowing `app`
    // mutably for the snapshot — direct re-borrow through `view_stack`
    // would otherwise alias `app`.
    let Some(mut overlay) = app.view_stack.pop() else {
        return;
    };
    if let Some(typed) = overlay.as_any_mut().downcast_mut::<LiveTranscriptOverlay>() {
        typed.refresh_from_app(app);
    }
    app.view_stack.push_boxed(overlay);
}

pub(crate) fn refresh_context_inspector_overlay(app: &mut App) {
    let Some(mut overlay) = app.view_stack.pop() else {
        return;
    };
    if let Some(typed) = overlay.as_any_mut().downcast_mut::<ContextInspectorView>() {
        typed.refresh_from_app(app);
    }
    app.view_stack.push_boxed(overlay);
}

/// Open the live transcript overlay in backtrack-preview mode (#133).
/// The overlay starts highlighting the most recent user message
/// (`selected_idx = 0`) and routes Left/Right/Enter/Esc through
/// `ViewEvent::Backtrack*` so the main key dispatcher can advance the
/// `BacktrackState` and apply the rewind on confirm.
pub(crate) fn open_backtrack_overlay(app: &mut App) {
    let mut overlay = LiveTranscriptOverlay::new();
    overlay.refresh_from_app(app);
    overlay.set_backtrack_preview(0);
    app.view_stack.push(overlay);
    app.status_message =
        Some("Backtrack: \u{2190}/\u{2192} step  Enter rewind  Esc cancel".to_string());
    app.needs_redraw = true;
}

/// Open a fresh live transcript overlay in sticky-tail mode.
pub(crate) fn open_live_transcript_overlay(app: &mut App) {
    if app.view_stack.top_kind() == Some(ModalKind::LiveTranscript) {
        return;
    }
    let mut overlay = LiveTranscriptOverlay::new();
    overlay.refresh_from_app(app);
    app.view_stack.push(overlay);
    app.status_message = Some("Live transcript: tailing (Esc to close)".to_string());
    app.needs_redraw = true;
}

/// Toggle the live transcript overlay on `Ctrl+Shift+T`. Closes the overlay if it's
/// already on top; otherwise uses the same open path as `/transcript`.
pub(crate) fn toggle_live_transcript_overlay(app: &mut App) {
    if app.view_stack.top_kind() == Some(ModalKind::LiveTranscript) {
        app.view_stack.pop();
        app.needs_redraw = true;
        return;
    }
    open_live_transcript_overlay(app);
}

/// Open the `/model` picker pre-filtered to `provider` (#3083). The model
/// picker's search already scopes rows by provider display name, so we reuse
/// the standard "open model picker" path and seed its query by replaying the
/// provider's display name as character input through the public view-stack
/// key path — no model-picker internals are touched.
pub(crate) fn open_model_picker_for_provider(
    app: &mut App,
    config: &Config,
    identity: &crate::config::ProviderIdentity,
) {
    if let Err(reason) = config.verify_provider_identity(identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    if app.view_stack.top_kind() != Some(ModalKind::ModelPicker) {
        app.view_stack
            .push(crate::tui::model_picker::ModelPickerView::new(app, config));
    }
    for ch in identity.key.as_str().chars() {
        // Char input updates the query and never emits a ViewEvent, so the
        // returned (empty) event list is safe to drop.
        let _ = app.view_stack.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        ));
    }
    app.needs_redraw = true;
}

/// Hide the Hotbar: persist `hotbar = []` (the canonical "disabled" state) and
/// clear the live in-memory slots so the panel disappears immediately. The
/// explicit empty array — not a missing key — is what disables defaults, so we
/// store `Some(vec![])` rather than `None`.
pub(crate) fn disable_hotbar(app: &mut App, config: &mut Config) {
    match crate::config_persistence::persist_hotbar_bindings(app.config_path.as_deref(), &[]) {
        Ok(path) => {
            config.hotbar = Some(Vec::new());
            app.status_message = Some(format!(
                "Hotbar hidden (hotbar = [] in {}). Bring it back with `/hotbar on`.",
                path.display()
            ));
        }
        Err(err) => {
            app.status_message = Some(format!("Failed to hide Hotbar: {err}"));
            app.add_message(HistoryCell::System {
                content: format!("Failed to hide Hotbar: {err}"),
            });
        }
    }
    app.needs_redraw = true;
}

pub(crate) fn refresh_config_view_if_open(app: &mut App, focus_key: &str) {
    refresh_config_view_after_commit(app, focus_key, false);
}

/// Rebuild an open settings view from disk truth after a commit. When the host
/// rejected the value, the editor reopens with what the user typed.
pub(crate) fn refresh_config_view_after_commit(app: &mut App, focus_key: &str, rejected: bool) {
    if app.view_stack.top_kind() != Some(ModalKind::Config) {
        return;
    }
    let Some(mut boxed) = app.view_stack.pop() else {
        return;
    };
    let rebuilt = match boxed.as_any_mut().downcast_ref::<ConfigView>() {
        Some(previous) => {
            let mut view = ConfigView::rebuild_preserving(app, previous, focus_key);
            if rejected {
                view.restore_rejected_commit(previous);
            }
            view
        }
        // Not a `ConfigView`: rebuild from scratch rather than restoring an
        // unknown modal, matching how the stack got here.
        None => {
            let mut fresh = ConfigView::new_for_app(app);
            fresh.focus_key(focus_key);
            fresh
        }
    };
    app.view_stack.push(rebuilt);
}

pub(crate) fn refresh_skills_manager_if_open(
    app: &mut App,
    status: Option<String>,
    focus: Option<&crate::skills::audit::AuditedSkillId>,
) {
    if app.view_stack.top_kind() != Some(ModalKind::SkillsManager) {
        return;
    }
    let Some(mut boxed) = app.view_stack.pop() else {
        return;
    };
    let rebuilt = if let Some(prev) = boxed
        .as_any_mut()
        .downcast_mut::<crate::tui::views::skills_manager::SkillsManagerView>(
    ) {
        crate::tui::views::skills_manager::SkillsManagerView::rebuild_preserving(
            app, prev, status, focus,
        )
    } else {
        crate::tui::views::skills_manager::SkillsManagerView::new(app)
    };
    app.view_stack.push(rebuilt);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_approval_request_view(
    app: &mut App,
    id: &str,
    tool_name: &str,
    description: &str,
    tool_input: &serde_json::Value,
    approval_key: &str,
    approval_grouping_key: &str,
    intent_summary: Option<&str>,
    default_selection: crate::config::ApprovalDefaultSelection,
    timeout: Option<std::time::Duration>,
) {
    let mut request = ApprovalRequest::new_with_intent(
        id,
        tool_name,
        description,
        tool_input,
        approval_key,
        intent_summary,
        &app.workspace,
    );
    // The engine owns the grant scope: a child's grouping key is prefixed
    // with its agent so "allow for this conversation" never covers the
    // parent or a sibling (C3). Keep the card's own key only when the
    // event carries none.
    if !approval_grouping_key.is_empty() {
        request.approval_grouping_key = approval_grouping_key.to_string();
    }
    // A child's gate never consults saved repo allow rules, so a child card
    // must not offer "Always allow in this repo" — it would do nothing. The
    // card names its agent (approvals C1).
    if crate::tools::subagent::SubAgentManager::is_child_approval_id(id) {
        request.persistent_allow_rules.clear();
        if let Some(agent_id) = crate::tui::pending_requests::child_agent_id(id) {
            request.owner = Some(crate::tui::pending_requests::owner_for(app, agent_id));
        }
    } else if !request.is_repo_law_prompt() {
        // The same edit runs without a card inside a git repository. Say why
        // this folder asks, so two folders behaving differently is explained
        // on the card. The engine already decided to ask; this only labels it.
        request.asks_without_git = crate::core::authority::file_write_asks_without_git(
            app.mode,
            app.approval_mode,
            &app.workspace,
            tool_name,
            tool_input,
        );
    }
    app.view_stack.push(
        ApprovalView::new_with_default_selection(request, app.ui_locale, default_selection)
            .with_timeout(timeout),
    );
}

/// Push the new `selected_idx` into the live transcript overlay so the
/// highlight follows the user's Left/Right input. No-op if the overlay is
/// no longer on top (e.g. it was closed underneath us).
pub(crate) fn update_backtrack_overlay_selection(app: &mut App, selected_idx: usize) {
    if app.view_stack.top_kind() != Some(ModalKind::LiveTranscript) {
        return;
    }
    let Some(mut overlay) = app.view_stack.pop() else {
        return;
    };
    if let Some(typed) = overlay.as_any_mut().downcast_mut::<LiveTranscriptOverlay>() {
        typed.set_backtrack_preview(selected_idx);
    }
    app.view_stack.push_boxed(overlay);
    app.needs_redraw = true;
}

/// Apply the user's backtrack selection: trim `app.history` and
/// `app.api_messages` so everything from the chosen user message onward
/// is dropped, populate the composer with the dropped user text, close
/// the overlay, and surface a status hint. The cycle counter is bumped
/// so any persistent indices clear; the engine's in-flight context is
/// re-synced via `Op::SyncSession` so the next turn starts fresh.
/// Index in `api_messages` to truncate to for a backtrack of `depth` visible
/// user prompts from the tail. Counts only messages that yield a
/// `HistoryCell::User` (a real prompt), NOT tool-result messages which are
/// also stored with `role == "user"`. Returns `None` if fewer than `depth`
/// user prompts exist.
pub(crate) fn backtrack_api_cut_index(api_messages: &[Message], depth: usize) -> Option<usize> {
    let mut user_seen = 0usize;
    for (idx, msg) in api_messages.iter().enumerate().rev() {
        let yields_user = history_cells_from_message(msg)
            .iter()
            .any(|cell| matches!(cell, HistoryCell::User { .. }));
        if yields_user {
            if user_seen == depth {
                return Some(idx);
            }
            user_seen += 1;
        }
    }
    None
}

pub(crate) fn jump_to_adjacent_tool_cell(app: &mut App, direction: SearchDirection) -> bool {
    let line_meta = app.viewport.transcript_cache.line_meta();
    if line_meta.is_empty() {
        return false;
    }

    let top = app
        .viewport
        .last_transcript_top
        .min(line_meta.len().saturating_sub(1));
    let current_cell = line_meta
        .get(top)
        .and_then(crate::tui::scrolling::TranscriptLineMeta::cell_line)
        .map(|(cell_index, _)| app.original_cell_index_for_rendered(cell_index));

    let mut scan_indices = Vec::new();
    match direction {
        SearchDirection::Forward => {
            scan_indices.extend((top.saturating_add(1))..line_meta.len());
        }
        SearchDirection::Backward => {
            scan_indices.extend((0..top).rev());
        }
    }

    for idx in scan_indices {
        let Some((cell_index, _)) = line_meta[idx].cell_line() else {
            continue;
        };
        let cell_index = app.original_cell_index_for_rendered(cell_index);
        if current_cell.is_some_and(|current| current == cell_index) {
            continue;
        }
        if !matches!(app.history.get(cell_index), Some(HistoryCell::Tool(_))) {
            continue;
        }
        if let Some(anchor) = TranscriptScroll::anchor_for(line_meta, idx) {
            app.viewport.transcript_scroll = anchor;
            app.viewport.pending_scroll_delta = 0;
            app.needs_redraw = true;
            return true;
        }
    }

    false
}

pub(crate) fn open_pager_for_selection(app: &mut App) -> bool {
    let Some(text) = selection_to_text(app) else {
        return false;
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let pager = PagerView::from_text("Selection", &text, width.saturating_sub(2));
    app.view_stack.push(pager);
    true
}

pub(crate) fn open_pager_for_last_message(app: &mut App) -> bool {
    let Some(cell) = app.history.last() else {
        return false;
    };
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let text = history_cell_to_text(cell, width);
    let mut pager = PagerView::from_text("Message", &text, width.saturating_sub(2));
    // When the last message is a completed assistant answer, expose the
    // clean answer-only payload via `a` (copy answer) — the rendered body
    // that `c`/`y` copies still carries the glyph/label line.
    if let Some(answer) = completed_assistant_answer_text(cell, width) {
        pager = pager.with_copy_answer(answer);
    }
    app.view_stack.push(pager);
    true
}

/// Compatibility wrapper for tests that exercise Ctrl+O on a thinking cell.
/// The user-facing Ctrl+O surface is now the turn-scoped Reasoning Detail
/// pager (#v092-reasoning-fix).
#[cfg(test)]
pub(crate) fn open_thinking_pager(app: &mut App) -> bool {
    open_reasoning_detail_pager(app)
}
