//! `handle_*` helpers: turning one input event, view event, or external
//! action into `App` state changes.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

/// How long the picker's ⇧F receipt stays in the footer: long enough to read
/// a route and its roles, short enough to leave the chrome still.
const FLEET_TOGGLE_TOAST_TTL_MS: u64 = 6_000;

/// Push the effective roster (saved fleet + config + plugins) to the running
/// engine through `Op::SetFleetRoster`. The one path every fleet mutation
/// takes: the saved-fleet views call it directly, and `/fleet add|remove`,
/// ⇧F, and auto-enroll reach it through `App::fleet_roster_stale`.
pub(crate) fn sync_fleet_roster(app: &mut App, config: &Config, engine_handle: &EngineHandle) {
    let roster = crate::fleet::identity::load_effective_roster(
        &config.fleet_config(),
        &app.workspace,
        Some(app.extension_plugin_view().as_ref()),
    );
    if let Some(error) = roster.load_error() {
        app.set_sticky_status(error.to_string(), StatusToastLevel::Error, None);
    }
    let _ = engine_handle.try_send(Op::SetFleetRoster {
        roster: std::sync::Arc::new(roster),
    });
}

/// Refresh the Fleet roster when it is parked on top of the stack after a
/// store mutation (#5954).
///
/// The roster now stays open underneath the saved-teams list, so selecting or
/// deleting a team has to update the view the user pops back to — otherwise
/// it keeps painting the pre-change team. Cursor and detail scroll survive,
/// because losing them is the disruption the back path exists to avoid.
pub(crate) fn refresh_parked_fleet_roster(app: &mut App, config: &Config) {
    if app.view_stack.top_kind() != Some(ModalKind::FleetRoster) {
        return;
    }
    let Some(mut view) = app.view_stack.pop() else {
        return;
    };
    if let Some(roster) = view
        .as_any_mut()
        .downcast_mut::<crate::tui::views::fleet_roster::FleetRosterView>()
    {
        roster.reload(app, config);
    }
    app.view_stack.push_boxed(view);
}

/// Rebuild the open model picker from live state and show `notice` inside
/// it. The picker covers the status line, so a receipt written only there
/// made ⇧P and ⇧F look like they did nothing (#6500). Returns whether a
/// picker was open.
pub(super) fn refresh_open_model_picker(
    app: &mut App,
    config: &Config,
    notice: Option<(String, StatusToastLevel)>,
) -> bool {
    if app.view_stack.top_kind() != Some(ModalKind::ModelPicker) {
        return false;
    }
    let Some(mut boxed) = app.view_stack.pop() else {
        return false;
    };
    if let Some(picker) = boxed
        .as_any_mut()
        .downcast_mut::<crate::tui::model_picker::ModelPickerView>()
    {
        picker.re_resolve_from_app(app, config);
        if let Some((text, level)) = notice {
            picker.set_notice(text, level);
        }
    }
    app.view_stack.push_boxed(boxed);
    true
}

/// The picker's ⇧P: toggle the exact route in `settings.toml`'s pins.
pub(super) fn toggle_model_picker_pin(
    app: &mut App,
    config: &Config,
    provider_key: &str,
    model: &str,
) {
    let locale = app.ui_locale;
    let route = format!("{provider_key}/{model}");
    let (receipt, level) = match crate::settings::Settings::transact(|settings| {
        Ok(settings.toggle_pinned_model(provider_key, model))
    }) {
        Ok(true) => (
            tr(locale, MessageId::ModelPickerPinned).replace("{route}", &route),
            StatusToastLevel::Success,
        ),
        Ok(false) => (
            tr(locale, MessageId::ModelPickerUnpinned).replace("{route}", &route),
            StatusToastLevel::Success,
        ),
        Err(error) => (
            tr(locale, MessageId::ModelPickerPinFailed).replace("{error}", &error.to_string()),
            StatusToastLevel::Error,
        ),
    };
    if let Ok(settings) = crate::settings::Settings::load_persisted() {
        app.pinned_models = settings.pinned_models;
    }
    app.status_message = Some(receipt.clone());
    refresh_open_model_picker(app, config, Some((receipt, level)));
    app.needs_redraw = true;
}

/// The picker's ⇧F: add the exact route to the selected Fleet, or remove the
/// shortlist row it added. Same provider gate as `/fleet add`.
pub(super) fn toggle_model_picker_fleet(
    app: &mut App,
    config: &Config,
    provider_key: &str,
    model: &str,
) {
    use crate::fleet::members::{FleetModelChange, change_receipt, toggle_fleet_model};
    let locale = app.ui_locale;
    let (receipt, level) = if let Some(rejection) =
        crate::commands::fleet_provider_rejection(app, config, provider_key)
    {
        app.set_sticky_status(rejection.clone(), StatusToastLevel::Error, None);
        (rejection, StatusToastLevel::Error)
    } else {
        match toggle_fleet_model(&app.workspace, provider_key, model) {
            Ok(change) => {
                let level = if matches!(change, FleetModelChange::Unchanged { .. }) {
                    StatusToastLevel::Info
                } else {
                    app.fleet_roster_stale = true;
                    StatusToastLevel::Success
                };
                let receipt = change_receipt(locale, provider_key, model, &change);
                app.push_status_toast(receipt.clone(), level, Some(FLEET_TOGGLE_TOAST_TTL_MS));
                (receipt, level)
            }
            Err(error) => {
                let message = tr(locale, MessageId::FleetToggleFailed)
                    .replace("{error}", &error.message(locale));
                app.set_sticky_status(message.clone(), StatusToastLevel::Error, None);
                (message, StatusToastLevel::Error)
            }
        }
    };
    refresh_open_model_picker(app, config, Some((receipt, level)));
    app.needs_redraw = true;
}

pub(super) fn dismiss_fleet_assignment(app: &mut App, editor_id: uuid::Uuid) {
    if let Some(mut boxed) = app.view_stack.pop() {
        let remove = if let Some(view) = boxed
            .as_any_mut()
            .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>(
        ) {
            view.route_selection(editor_id).is_some()
        } else if let Some(view) = boxed
            .as_any_mut()
            .downcast_mut::<crate::tui::views::fleet_detail::FleetDetailView>(
        ) {
            view.is_direct_assignment(editor_id)
        } else {
            false
        };
        if !remove {
            app.view_stack.push_boxed(boxed);
        }
    }
}

/// Once per event-loop iteration: deliver a pending fleet mutation to the
/// engine and clear the flag.
pub(crate) fn flush_stale_fleet_roster(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) {
    if std::mem::take(&mut app.fleet_roster_stale) {
        sync_fleet_roster(app, config, engine_handle);
    }
}

/// Persist a `# foo` quick-add through the native memory store and surface
/// a status note to the user. Errors land in the same status channel so a
/// missing memory directory becomes visible without crashing the composer.
pub(crate) fn handle_memory_quick_add(app: &mut App, input: &str, config: &Config) {
    let path = config.memory_path();
    let note = input.trim_start_matches('#').trim();
    let result = crate::native_memory::NativeMemoryStore::from_global_path(&path)
        .ok_or_else(|| format!("{} is not a native memory path", path.display()))
        .and_then(|store| {
            store
                .remember(crate::native_memory::MemoryScope::Global, None, note)
                .map(|hit| hit.source)
                .map_err(|err| err.to_string())
        });
    match result {
        Ok(source) => {
            app.status_message = Some(format!("memory: appended to {}", source.display()));
        }
        Err(err) => {
            app.status_message = Some(format!(
                "memory: failed to write {}: {}",
                path.display(),
                err
            ));
        }
    }
}

/// Route one terminal bracketed-paste event without exposing its contents.
///
/// Keeping the routing in one function makes the credential and ordinary
/// composer paths exercise the same observability boundary.
pub(crate) fn handle_bracketed_paste(app: &mut App, text: &str) {
    tracing::debug!(
        paste_bytes = text.len(),
        paste_chars = text.chars().count(),
        "Received bracketed paste event"
    );
    // Once a real bracketed-paste event has been observed in this session,
    // the rapid-keystroke heuristic in paste_burst is redundant — disable it
    // so fast typing / IME commits / autocomplete bursts don't get
    // mis-classified as a paste.
    app.bracketed_paste_seen = true;
    if app.is_history_search_active() {
        app.history_search_insert_str(text);
    } else if paste_text_into_provider_picker(app, text) || app.view_stack.handle_paste(text) {
        // Modal consumed the paste (e.g. provider picker key entry).
    } else if !app.view_stack.is_empty() {
        // A non-consumed modal is open — don't leak paste into composer.
    } else {
        // Main-input paste takes the same keyboard ownership as typed text.
        // Otherwise the visible composer command's Enter stays with the dock.
        crate::tui::work_surface::release_focus(app);
        app.insert_paste_text(text);
    }
}

/// Voice input toggle via Option+V (⌥V) — matches Muse Spark UX:
/// "Recording (⌥V to finish)" with a transient voice indicator, no slash
/// command needed. Handles both Alt+V and the macOS ⌥V glyph.
pub(crate) fn handle_voice_key(app: &mut App, key: &event::KeyEvent) -> bool {
    let is_alt_v = matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER);
    // Some terminals emit the literal "√" (Option+V on macOS) instead of Alt+V.
    let is_glyph = matches!(key.code, KeyCode::Char('√') | KeyCode::Char('∫'));
    if !is_alt_v && !is_glyph {
        return false;
    }
    // Toggle voice capture — same path as /voice but via hotkey.
    let result = crate::commands::voice::voice(app);
    // Surface a Spark-style transient hint; the capture itself is async.
    if app.voice_enabled {
        app.status_message = Some("● Recording  (⌥V to finish)".to_string());
    }
    // Suppress the default char insertion for this combo.
    let _ = result;
    true
}

/// The event-loop seam for Ctrl+T. Keeping the `KeyEvent` predicate and App
/// mutation together makes the real terminal route directly testable rather
/// than testing `cycle_effort` in isolation.
pub(crate) fn handle_reasoning_effort_key(app: &mut App, key: &event::KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
        || key.modifiers != KeyModifiers::CONTROL
    {
        return false;
    }
    let _ = app.cycle_effort();
    true
}

/// Let the transcript remain reviewable while a decision prompt owns focus.
pub(crate) fn handle_prompt_transcript_key(app: &mut App, key: &event::KeyEvent) -> bool {
    if !matches!(
        app.view_stack.top_kind(),
        Some(ModalKind::Approval | ModalKind::UserInput)
    ) {
        return false;
    }

    let page = app.viewport.last_transcript_visible.max(1);
    match key.code {
        KeyCode::PageUp => app.scroll_up(page),
        KeyCode::PageDown => app.scroll_down(page),
        KeyCode::Up
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL) =>
        {
            app.scroll_up(3);
        }
        KeyCode::Down
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL) =>
        {
            app.scroll_down(3);
        }
        KeyCode::Home => app.scroll_up(usize::MAX),
        KeyCode::End => app.scroll_to_bottom(),
        _ => return false,
    }
    true
}

/// One-shot "draft my constitution" call against the user's first configured
/// model, requested by `A` on the setup Constitution card. Runs inline in the
/// event loop like [`fetch_available_models`] (the wizard modal stays open
/// underneath) with a hard timeout so a slow provider cannot wedge setup.
///
/// On success the sanitized, bounded draft is installed into the open wizard
/// and its ratification preview opens on top — nothing persists until the
/// user ratifies with `G`. Every failure (no client, timeout, request error,
/// invalid or empty JSON) is a status line, never an error state: the
/// deterministic guided draft remains the standing fallback.
pub(crate) async fn handle_setup_constitution_model_draft(
    app: &mut App,
    config: &Config,
    draft: crate::tui::setup::GuidedConstitutionDraft,
    freeform_note: Option<String>,
    locale: codewhale_localization::Locale,
) {
    // Spawn the draft off the event loop (same pattern as the fleet drafter,
    // #3757 review): awaiting it inline parked the whole TUI for up to the
    // timeout. The loop polls constitution_draft_cell and delivers the result.
    const DRAFT_TIMEOUT: Duration = Duration::from_secs(20);
    let model_label = app.model_display_label();
    let client = match CodewhaleClient::new(config) {
        Ok(client) => client,
        Err(err) => {
            deliver_constitution_draft_result(
                app,
                model_label.clone(),
                locale,
                Err(format!("provider not ready: {err:#}")),
            );
            return;
        }
    };
    let request_model = app.model.clone();
    let cell = app.constitution_draft_cell.clone();
    let spawn_label = model_label.clone();
    let request_gen = app.next_draft_gen();
    app.status_message = Some(match locale {
        codewhale_localization::Locale::ZhHans => {
            format!(
                "{model_label} 正在生成协作准则草案……（最多 {}s）",
                DRAFT_TIMEOUT.as_secs()
            )
        }
        _ => format!(
            "{model_label} is drafting your constitution… (up to {}s)",
            DRAFT_TIMEOUT.as_secs()
        ),
    });
    app.needs_redraw = true;
    tokio::spawn(async move {
        let outcome = match tokio::time::timeout(
            DRAFT_TIMEOUT,
            crate::tui::setup::draft_constitution_with_model(
                &client,
                &request_model,
                draft,
                freeform_note,
                locale,
            ),
        )
        .await
        {
            Err(_) => Err(format!("timed out after {}s", DRAFT_TIMEOUT.as_secs())),
            Ok(result) => result,
        };
        if let Ok(mut guard) = cell.lock() {
            *guard = Some((request_gen, spawn_label, locale, outcome));
        }
    });
}

/// One-shot fleet-profile draft: same contract as the constitution drafter —
/// minimal payload out, untrusted gate in, preview before ratify, degrade to
/// the manual authoring flow on any failure.
pub(crate) async fn handle_fleet_profile_model_draft(
    app: &mut App,
    config: &Config,
    role: String,
    model: String,
    provider: Option<String>,
    reasoning_effort: Option<String>,
    locale: codewhale_localization::Locale,
) {
    // The route the operator actually picked at `m`-press time (#4093). A
    // model draft always comes back `provider: None` (the untrusted gate
    // strips any provider), so this captured `(provider, model)` is what the
    // ratified profile is pinned to — immune to the model omitting/altering
    // the route AND to the selection changing while the draft is in flight.
    // `None` for an `inherit` pick (no concrete route to keep).
    let picked_route = provider.map(|provider| (provider, model.clone()));
    // Do NOT await the network call on the event loop — that parks the whole
    // TUI for up to the timeout (#3757 review). Spawn it into the shared
    // fleet_draft_cell and let the loop poll + deliver the result, keeping
    // the wizard interactive with a drafting status.
    const DRAFT_TIMEOUT: Duration = Duration::from_secs(20);
    let model_label = app.model_display_label();
    let client = match CodewhaleClient::new(config) {
        Ok(client) => client,
        Err(err) => {
            deliver_fleet_draft_result(
                app,
                model_label.clone(),
                picked_route.clone(),
                reasoning_effort.clone(),
                Err(format!("provider not ready: {err:#}")),
                locale,
            );
            return;
        }
    };
    let request_model = app.model.clone();
    let cell = app.fleet_draft_cell.clone();
    let spawn_label = model_label.clone();
    let request_gen = app.next_draft_gen();
    let workspace = app.workspace.clone();
    app.status_message = Some(match locale {
        codewhale_localization::Locale::ZhHans => {
            format!(
                "{model_label} 正在起草配置……（最多 {}s）",
                DRAFT_TIMEOUT.as_secs()
            )
        }
        _ => format!(
            "{model_label} is drafting the profile… (up to {}s)",
            DRAFT_TIMEOUT.as_secs()
        ),
    });
    app.needs_redraw = true;
    tokio::spawn(async move {
        // Redacted, bounded workspace fingerprint (manifest names, test
        // commands, branch + dirty count — no contents, secrets, or absolute
        // paths). Computed off the event loop; the untrusted-output gate on
        // the reply is unchanged.
        let fingerprint = tokio::task::spawn_blocking(move || {
            crate::tui::setup::workspace_fingerprint(&workspace)
        })
        .await
        .unwrap_or_default();
        let outcome = match tokio::time::timeout(
            DRAFT_TIMEOUT,
            crate::tui::setup::draft_fleet_profile_with_model(
                &client,
                &request_model,
                &role,
                &model,
                locale,
                &fingerprint,
            ),
        )
        .await
        {
            Err(_) => Err(format!("timed out after {}s", DRAFT_TIMEOUT.as_secs())),
            Ok(result) => result,
        };
        if let Ok(mut guard) = cell.lock() {
            *guard = Some((
                request_gen,
                spawn_label,
                picked_route,
                reasoning_effort,
                outcome,
            ));
        }
    });
}

pub(crate) async fn handle_bang_shell_input(
    app: &mut App,
    engine_handle: &EngineHandle,
    input: &str,
) -> Result<bool> {
    let command = match shell_command_from_bang_input(input) {
        Ok(Some(command)) => command,
        Ok(None) => return Ok(false),
        Err(message) => {
            app.status_message = Some(format!("Error: {message}"));
            return Ok(true);
        }
    };

    // #6150: composer input never awaits a full op channel — a saturated
    // engine reports busy instead of freezing the loop.
    match engine_handle.tx_op.clone().try_reserve_owned() {
        Ok(permit) => {
            engine_handle.send_reserved_op(
                permit,
                Op::RunShellCommand {
                    command: command.to_string(),
                    mode: app.mode,
                    allow_shell: app.allow_shell,
                    trust_mode: app.trust_mode,
                    auto_approve: app_auto_approve_enabled(app),
                    approval_mode: app.approval_mode,
                },
            );
            app.status_message = Some(format!("Shell command submitted: {command}"));
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            app.status_message =
                Some("Engine busy — shell command not sent; try again".to_string());
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            return Err(anyhow::anyhow!("engine channel closed"));
        }
    }
    Ok(true)
}

fn report_mcp_login(app: &mut App, message: String, level: StatusToastLevel) {
    app.push_status_toast(message.clone(), level, Some(12_000));
    add_mcp_message(app, message);
    app.needs_redraw = true;
}

fn start_mcp_login(app: &mut App, config: &Config, name: String, scopes: Vec<String>) {
    use crate::tui::app::{McpLoginProgress, PendingMcpLogin};

    if let Some(pending) = &app.mcp_login {
        let server = pending.server.clone();
        report_mcp_login(
            app,
            app.tr(MessageId::McpLoginInProgress)
                .replace("{server}", &server)
                .replace("{cancel_key}", "Esc"),
            StatusToastLevel::Info,
        );
        return;
    }

    let path = app.mcp_config_path.clone();
    let workspace = app.workspace.clone();
    let plugin_registry = Arc::clone(&app.plugin_registry);
    let network_policy = config.network.clone().map(|network| {
        crate::network_policy::NetworkPolicyDecider::with_default_audit(network.into_runtime())
    });
    let callback_port = config.mcp_oauth_callback_port;
    let callback_url = config.mcp_oauth_callback_url.clone();
    let locale = app.ui_locale;
    let pending = PendingMcpLogin {
        server: name.clone(),
        cancel: tokio_util::sync::CancellationToken::new(),
        progress: Arc::new(std::sync::Mutex::new(None)),
    };
    let cancel = pending.cancel.clone();
    let progress = Arc::clone(&pending.progress);
    app.mcp_login = Some(pending);
    report_mcp_login(
        app,
        app.tr(MessageId::McpLoginStarting)
            .replace("{server}", &name)
            .replace("{cancel_key}", "Esc"),
        StatusToastLevel::Info,
    );

    tokio::spawn(async move {
        let handshake = async {
            let cfg = crate::mcp::load_config_with_workspace_and_plugins(
                &path,
                &workspace,
                plugin_registry.as_ref(),
            )?;
            let server = cfg.servers.get(&name).ok_or_else(|| {
                anyhow::anyhow!(
                    codewhale_localization::tr(locale, MessageId::McpLoginServerNotFound)
                        .replace("{server}", &name)
                )
            })?;
            crate::mcp::oauth::begin_oauth_login_for_server_tool(
                &name,
                server,
                (!scopes.is_empty()).then_some(scopes),
                callback_port,
                callback_url.as_deref(),
                network_policy.as_ref(),
            )
            .await
        };
        let operation = async {
            // Bound the whole handshake as well as each guarded HTTP request.
            // The timeout is in the background: even an unresponsive issuer
            // cannot delay redraw, input or cancellation.
            let login = tokio::time::timeout(Duration::from_secs(15), handshake)
                .await
                .with_context(|| {
                    codewhale_localization::tr(locale, MessageId::McpLoginHandshakeTimeout)
                        .into_owned()
                })??;
            if let Ok(mut cell) = progress.lock() {
                *cell = Some(McpLoginProgress::AuthorizationUrl(
                    login.authorization_url().to_string(),
                ));
            }
            login.finish().await
        };
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            result = operation => result.map_err(|error| {
                crate::mcp::oauth::mask_oauth_secrets(&format!("{error:#}"))
            }),
        };
        if let Ok(mut cell) = progress.lock() {
            *cell = Some(McpLoginProgress::Finished(outcome));
        }
    });
}

pub(crate) fn poll_mcp_login(app: &mut App) {
    use crate::tui::app::McpLoginProgress;

    let delivery = app.mcp_login.as_ref().and_then(|pending| {
        pending
            .progress
            .try_lock()
            .ok()
            .and_then(|mut cell| cell.take())
            .map(|progress| (pending.server.clone(), progress))
    });
    let Some((server, progress)) = delivery else {
        return;
    };
    let (message, level) = match progress {
        McpLoginProgress::AuthorizationUrl(url) => (
            app.tr(MessageId::McpLoginBrowser)
                .replace("{server}", &server)
                .replace("{cancel_key}", "Esc")
                .replace("{url}", &url),
            StatusToastLevel::Info,
        ),
        McpLoginProgress::Finished(outcome) => {
            app.mcp_login = None;
            match outcome {
                Ok(()) => (
                    app.tr(MessageId::McpLoginStored)
                        .replace("{server}", &server)
                        .replace("{command}", "/mcp reload"),
                    StatusToastLevel::Success,
                ),
                Err(error) => (
                    app.tr(MessageId::McpLoginFailed)
                        .replace("{server}", &server)
                        .replace("{error}", &error),
                    StatusToastLevel::Error,
                ),
            }
        }
    };
    report_mcp_login(app, message, level);
}

pub(crate) fn handle_mcp_login_key(app: &mut App, key: &KeyEvent) -> bool {
    if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc && app.mcp_login.is_some() {
        cancel_mcp_login(app);
        true
    } else {
        false
    }
}

pub(crate) fn cancel_mcp_login(app: &mut App) {
    if let Some(pending) = app.mcp_login.take() {
        // Drop cancels before the next input event; future writes belong only
        // to this abandoned mailbox, even if the same server starts again.
        let server = pending.server.clone();
        drop(pending);
        report_mcp_login(
            app,
            app.tr(MessageId::McpLoginCancelled)
                .replace("{server}", &server),
            StatusToastLevel::Info,
        );
    }
}

/// The config file that owns `name`, for a mutation that must land where the
/// server is actually declared.
///
/// A plugin-contributed server has no config file: it is switched off by
/// disabling the plugin that carries it, so say that instead of writing a
/// stray entry into the user's file under the synthesized name.
fn mcp_scoped_config_path(
    app: &App,
    global_path: &std::path::Path,
    name: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let scope = crate::mcp::resolve_server_scope(global_path, &app.workspace, name);
    scope.config_path(global_path).ok_or_else(|| {
        anyhow::anyhow!(
            "MCP server '{name}' is provided by a plugin, not by a config file. \
             Disable the plugin that contributes it from /plugins."
        )
    })
}

/// Whether a turn (or its compaction work) owns the engine. The engine
/// services ops only between turns, so an op sent now waits for that turn.
fn mcp_engine_busy(app: &App) -> bool {
    app.is_loading
        || app.dispatch_in_flight
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
        || app.is_compacting
        || app.manual_compaction_queued
}

/// Rebuild the open Extensions panel from live state, so a row's pending or
/// settled retry shows without waiting for the next MCP generation.
fn refresh_open_extensions(app: &mut App) {
    if app.view_stack.extensions_is_top() {
        let snapshot = crate::tui::views::extensions::ExtensionsSnapshot::from_app(app);
        app.view_stack.refresh_extensions(snapshot);
    }
    app.needs_redraw = true;
}

/// Reconnect one MCP server through the engine-owned pool without awaiting it
/// on the UI loop. While a turn runs, the op waits in the engine mailbox and
/// runs as soon as the turn ends — the person asked once, so they are not
/// told to ask again.
fn start_mcp_retry(app: &mut App, engine_handle: &EngineHandle, name: String) {
    use crate::tui::app::PendingMcpRetry;

    let already = app
        .mcp_retries
        .iter()
        .find(|pending| pending.server == name);
    let queued = already.map_or_else(|| mcp_engine_busy(app), |pending| pending.queued);
    if already.is_none() {
        tracing::info!(target: "mcp", server = %name, queued, "MCP server retry requested");
        let result = Arc::new(std::sync::Mutex::new(None));
        app.mcp_retries.push(PendingMcpRetry {
            server: name.clone(),
            queued,
            result: Arc::clone(&result),
        });
        let handle = engine_handle.clone();
        let server = name.clone();
        tokio::spawn(async move {
            let outcome = handle
                .retry_mcp_server(server)
                .await
                .map_err(|error| crate::mcp::format_mcp_error_for_display(&error));
            if let Ok(mut cell) = result.lock() {
                *cell = Some(outcome);
            }
        });
    }
    let message = if queued {
        app.tr(MessageId::McpRetryDeferredWhileTurnRuns)
    } else {
        app.tr(MessageId::McpRetryStarted)
    }
    .replace("{server}", &name);
    report_mcp_login(app, message, StatusToastLevel::Info);
    refresh_open_extensions(app);
}

/// Deliver every settled `/mcp retry`: apply its snapshot and say what
/// happened to that server — connected, needs a login, or why it failed.
pub(crate) fn poll_mcp_retries(app: &mut App) {
    if app.mcp_retries.is_empty() {
        return;
    }
    let mut settled = Vec::new();
    app.mcp_retries.retain(|pending| {
        match pending
            .result
            .try_lock()
            .ok()
            .and_then(|mut cell| cell.take())
        {
            Some(outcome) => {
                settled.push((pending.server.clone(), outcome));
                false
            }
            None => true,
        }
    });
    if settled.is_empty() {
        return;
    }
    for (server, outcome) in settled {
        let (message, level) = match outcome {
            Ok(update) => {
                let receipt = mcp_retry_receipt(app, &server, &update.snapshot);
                apply_mcp_session_boot_event(
                    app,
                    update.generation,
                    update.snapshot,
                    Vec::new(),
                    true,
                );
                receipt
            }
            Err(error) => {
                tracing::warn!(target: "mcp", server = %server, error = %error, "MCP server retry could not run");
                (
                    app.tr(MessageId::McpRetryFailed)
                        .replace("{server}", &server)
                        .replace("{error}", &error),
                    StatusToastLevel::Error,
                )
            }
        };
        report_mcp_login(app, message, level);
    }
    refresh_open_extensions(app);
}

/// The one-line outcome of a retry for `server`, read from the snapshot the
/// engine returned for it.
fn mcp_retry_receipt(
    app: &App,
    server: &str,
    snapshot: &crate::mcp::McpManagerSnapshot,
) -> (String, StatusToastLevel) {
    let Some(observed) = snapshot.servers.iter().find(|row| row.name == server) else {
        return (
            app.tr(MessageId::McpRetryFailed)
                .replace("{server}", server)
                .replace(
                    "{error}",
                    &app.tr(MessageId::McpLoginServerNotFound)
                        .replace("{server}", server),
                ),
            StatusToastLevel::Error,
        );
    };
    if observed.connected {
        (
            app.tr(MessageId::McpRetryConnected)
                .replace("{server}", server)
                .replace("{tools}", &observed.tools.len().to_string()),
            StatusToastLevel::Success,
        )
    } else if observed.auth_required {
        (
            app.tr(MessageId::McpRetryNeedsLogin)
                .replace("{server}", server)
                .replace(
                    "{command}",
                    &crate::mcp::McpRecoveryKind::Reauth.slash_command(server),
                ),
            StatusToastLevel::Warning,
        )
    } else {
        let error = observed
            .error
            .clone()
            .unwrap_or_else(|| app.tr(MessageId::ExtensionsStateDisconnected).into_owned());
        (
            app.tr(MessageId::McpRetryFailed)
                .replace("{server}", server)
                .replace("{error}", &error),
            StatusToastLevel::Error,
        )
    }
}

pub(crate) async fn handle_mcp_ui_action(
    app: &mut App,
    engine_handle: &EngineHandle,
    config: &Config,
    action: crate::tui::app::McpUiAction,
) {
    use crate::mcp::{self, McpWriteStatus};

    let path = app.mcp_config_path.clone();
    let mut changed = false;
    let mut message = None;
    let is_reload = matches!(&action, crate::tui::app::McpUiAction::Reload);
    // A reload already running owns the live surface, and starting a second
    // pass restarts every server the first one is still connecting. `Extensions`
    // rows read `[connecting]` while that happens and answer no key, so a user
    // who presses Enter again gets another full reconnect and another receipt —
    // four presses became four overlapping 12-server reloads and a wall of
    // duplicate notes. The flag was already tracked; nothing ever read it.
    if is_reload && app.mcp_reload_in_flight {
        add_mcp_message(app, app.tr(MessageId::McpReloadAlreadyRunning).into_owned());
        return;
    }
    // A retry never runs on this path: it is an engine op, and awaiting it
    // here parked input behind the connect (or behind a running turn,
    // #6159). It goes to the engine from a background task instead, so a
    // running turn simply queues it; `poll_mcp_retries` reports the outcome.
    if let crate::tui::app::McpUiAction::Retry { name } = &action {
        start_mcp_retry(app, engine_handle, name.clone());
        return;
    }
    let snapshot_live_pool = matches!(&action, crate::tui::app::McpUiAction::Show);
    let discover = mcp_ui_action_refreshes_discovery(&action);

    let approve_import = matches!(&action, crate::tui::app::McpUiAction::ImportApprove { .. });
    let action_result = match action {
        crate::tui::app::McpUiAction::Diagnose { name } => {
            let receipt = mcp_server_diagnosis(app, &name);
            report_mcp_login(app, receipt, StatusToastLevel::Info);
            return;
        }
        crate::tui::app::McpUiAction::Show => Ok(()),
        crate::tui::app::McpUiAction::Init { force } => {
            changed = true;
            match mcp::init_config(&path, force) {
                Ok(McpWriteStatus::Created) => {
                    message = Some(format!("Created MCP config at {}", path.display()));
                    Ok(())
                }
                Ok(McpWriteStatus::Overwritten) => {
                    message = Some(format!("Overwrote MCP config at {}", path.display()));
                    Ok(())
                }
                Ok(McpWriteStatus::SkippedExists) => {
                    changed = false;
                    message = Some(format!(
                        "MCP config already exists at {} (use /mcp init --force to overwrite)",
                        path.display()
                    ));
                    Ok(())
                }
                Err(err) => Err(err),
            }
        }
        crate::tui::app::McpUiAction::AddStdio {
            name,
            command,
            args,
        } => {
            changed = true;
            mcp::add_server_config(&path, name.clone(), Some(command), None, args, None)
                .map(|()| message = Some(format!("Added MCP stdio server '{name}'")))
        }
        crate::tui::app::McpUiAction::AddHttp {
            name,
            url,
            transport,
        } => {
            changed = true;
            mcp::add_server_config(&path, name.clone(), None, Some(url), Vec::new(), transport)
                .map(|()| message = Some(format!("Added MCP HTTP/SSE server '{name}'")))
        }
        // Write where the server actually lives. `path` is the user's global
        // file; a workspace-scoped server is declared in the trusted
        // workspace's own file and overrides a same-named global entry, so
        // editing the global file here reported success on the wrong server
        // or failed with "not found" on a row the panel had just offered.
        crate::tui::app::McpUiAction::Enable { name } => {
            changed = true;
            mcp_scoped_config_path(app, &path, &name)
                .and_then(|owner| mcp::set_server_enabled(&owner, &name, true))
                .map(|()| message = Some(format!("Enabled MCP server '{name}'")))
        }
        crate::tui::app::McpUiAction::Disable { name } => {
            changed = true;
            mcp_scoped_config_path(app, &path, &name)
                .and_then(|owner| mcp::set_server_enabled(&owner, &name, false))
                .map(|()| message = Some(format!("Disabled MCP server '{name}'")))
        }
        crate::tui::app::McpUiAction::Remove { name } => {
            changed = true;
            mcp_scoped_config_path(app, &path, &name)
                .and_then(|owner| mcp::remove_server_config(&owner, &name))
                .map(|()| message = Some(format!("Removed MCP server '{name}'")))
        }
        crate::tui::app::McpUiAction::Login { name, scopes } => {
            start_mcp_login(app, config, name, scopes);
            // Login owns its background discovery. Do not start a second
            // discovery here or await network work on the input loop.
            return;
        }
        crate::tui::app::McpUiAction::Logout { name } => {
            let result = (|| {
                let cfg = mcp::load_config_with_workspace_and_plugins(
                    &path,
                    &app.workspace,
                    app.plugin_registry.as_ref(),
                )?;
                let server = cfg
                    .servers
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("MCP server '{name}' not found"))?;
                mcp::oauth::delete_oauth_tokens_for_server(&name, server)
            })();
            result.map(|deleted| {
                changed = deleted;
                message = Some(if deleted {
                    format!(
                        "Deleted locally stored OAuth credentials for MCP server '{name}'. That clears this machine only — the provider may keep its grant; the next /mcp login re-prompts for consent. Run /mcp reload to reconnect."
                    )
                } else {
                    format!("No stored OAuth credentials found for MCP server '{name}'.")
                });
            })
        }
        crate::tui::app::McpUiAction::ImportList => {
            let path = path.clone();
            let workspace = app.workspace.clone();
            let plugins = app.plugin_registry.clone();
            #[cfg(test)]
            let ticket = crate::test_support::env_scope_ticket();
            match tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _membership = crate::test_support::join_env_scope(ticket);
                mcp_external_import_status_text(&workspace, &path, plugins.as_ref())
            })
            .await
            {
                Ok(text) => {
                    message = Some(text);
                    Ok(())
                }
                Err(_) => Err(anyhow::anyhow!("MCP import preview failed")),
            }
        }
        crate::tui::app::McpUiAction::ImportApprove { name }
        | crate::tui::app::McpUiAction::ImportDecline { name } => {
            let approve = approve_import;
            let path = path.clone();
            let workspace = app.workspace.clone();
            let plugins = app.plugin_registry.clone();
            #[cfg(test)]
            let ticket = crate::test_support::env_scope_ticket();
            match tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                let _membership = crate::test_support::join_env_scope(ticket);
                mcp_import_apply(&workspace, &path, plugins.as_ref(), &name, approve)
            })
            .await
            {
                Ok(Ok(msg)) => {
                    changed = approve;
                    message = Some(msg);
                    Ok(())
                }
                Ok(Err(err)) => Err(err),
                Err(_) => Err(anyhow::anyhow!("MCP import failed")),
            }
        }
        crate::tui::app::McpUiAction::Validate | crate::tui::app::McpUiAction::Reload => Ok(()),
        // Dispatched before this match.
        crate::tui::app::McpUiAction::Retry { .. } => Ok(()),
    };

    if let Err(err) = action_result {
        add_mcp_message(app, format!("MCP action failed: {err}"));
        return;
    }

    if changed {
        app.mcp_reload_required = true;
    }
    if let Some(message) = message {
        add_mcp_message(app, message);
    }

    // Every branch below is an engine round-trip, and the engine services ops
    // only between turns (`Engine::run` runs a turn inline and never polls
    // `rx_op` mid-turn): awaiting one from this UI path parked every keypress
    // and repaint behind the running turn — a full console freeze (#6159).
    // While a turn (or its compaction work) owns the engine, serve the last
    // known snapshot and say so; mutations name the deferral instead of
    // freezing. `reject_inline_inference_while_runtime_chat_owns_run`
    // (apply.rs) is the same fail-closed rule for inline inference.
    let engine_busy = mcp_engine_busy(app);
    if engine_busy && (snapshot_live_pool || is_reload || changed) {
        if snapshot_live_pool {
            match app.mcp_snapshot.clone() {
                Some(snapshot) => {
                    app.mcp_configured_count = snapshot.servers.len();
                    app.mcp_snapshot = Some(snapshot);
                    app.mcp_initializing = false;
                    app.mcp_connecting.clear();
                    app.hotbar_actions
                        .replace_mcp_tools(app.mcp_snapshot.as_ref());
                    add_mcp_message(
                        app,
                        app.tr(MessageId::McpShowCachedWhileTurnRuns).into_owned(),
                    );
                    open_mcp_extensions(app);
                }
                None => add_mcp_message(
                    app,
                    app.tr(MessageId::McpShowUnavailableWhileTurnRuns)
                        .into_owned(),
                ),
            }
        } else {
            add_mcp_message(
                app,
                app.tr(MessageId::McpLivePoolRefreshDeferredWhileTurnRuns)
                    .into_owned(),
            );
        }
        return;
    }

    // A successful MCP mutation is an explicit request to change the tools
    // available to this running session. Apply it to the engine-owned pool in
    // the same operation instead of leaving Extensions and `/mcp` users on a
    // second, easy-to-miss reload step. The standalone reload action remains
    // the retry/compatibility path for externally edited configuration.
    let rebuild_live_pool = is_reload || changed;
    let snapshot_result = if snapshot_live_pool {
        engine_handle
            .bootstrap_mcp()
            .await
            .map(|update| (update.snapshot, Some(update.generation)))
    } else if rebuild_live_pool {
        match engine_handle.reload_mcp(path.clone()).await {
            Ok(update) => {
                // The reload no longer waits for the connect batch. The
                // engine's supervised pass owns the live surface from here:
                // apply the interim snapshot without invalidating its own
                // generation, leave connecting/initializing to the pass's
                // progress events, and let its finished event post the
                // counts. A config mutation keeps its own receipt instead of
                // the reload-started line.
                app.mcp_reload_required = false;
                app.mcp_reload_in_flight = true;
                if is_reload {
                    add_mcp_message(
                        app,
                        format!(
                            "MCP reload started in the background: {} configured server(s) reconnecting. The status bar tracks progress; the next model turn uses the catalog as it settles.",
                            update.snapshot.servers.len()
                        ),
                    );
                }
                app.mcp_configured_count = update.snapshot.servers.len();
                app.mcp_snapshot_generation = update.generation;
                app.mcp_snapshot_generation_invalidated = false;
                app.hotbar_actions.replace_mcp_tools(Some(&update.snapshot));
                app.mcp_snapshot = Some(update.snapshot);
                open_mcp_extensions(app);
                return;
            }
            Err(error) => {
                app.mcp_reload_required = true;
                Err(error)
            }
        }
    } else if discover {
        let network_policy = config.network.clone().map(|toml_cfg| {
            crate::network_policy::NetworkPolicyDecider::with_default_audit(toml_cfg.into_runtime())
        });
        mcp::discover_manager_snapshot_with_workspace_and_plugins(
            &path,
            &app.workspace,
            network_policy,
            app.mcp_reload_required,
            std::sync::Arc::clone(&app.plugin_registry),
            mcp::McpBackend::from_config(config),
        )
        .await
        .map(|snapshot| (snapshot, None))
    } else {
        mcp::manager_snapshot_from_config_with_workspace_and_plugins(
            &path,
            &app.workspace,
            app.mcp_reload_required,
            app.plugin_registry.as_ref(),
        )
        .map(|snapshot| (snapshot, None))
    };

    match snapshot_result {
        Ok((snapshot, generation)) => {
            if discover {
                add_mcp_message(
                    app,
                    "MCP discovery refreshed for the UI. Run /mcp reload after config or credential edits to rebuild the live model-visible tool pool.".to_string(),
                );
            }
            // Keep the boot-time MCP-count chip in sync with the live
            // snapshot so footers and panels reflect post-/mcp edits
            // (#502).
            app.mcp_configured_count = snapshot.servers.len();
            if let Some(generation) = generation {
                app.mcp_snapshot_generation = generation;
                app.mcp_snapshot_generation_invalidated = true;
            }
            app.mcp_snapshot = Some(snapshot.clone());
            app.mcp_initializing = false;
            app.mcp_connecting.clear();
            // #2068: keep the hotbar's MCP-tool actions in sync with the tools
            // that are actually loaded; the hotbar never connects on its own.
            app.hotbar_actions.replace_mcp_tools(Some(&snapshot));
            open_mcp_extensions(app);
        }
        Err(err) if rebuild_live_pool => add_mcp_message(
            app,
            format!("MCP reload failed; the live tool pool is unchanged: {err}"),
        ),
        Err(err) => add_mcp_message(app, format!("MCP snapshot failed: {err}")),
    }
}

pub(crate) fn handle_shell_job_action(app: &mut App, action: crate::tui::app::ShellJobAction) {
    let Some(shell_manager) = app.runtime_services.shell_manager.clone() else {
        add_shell_job_message(app, "No shell session is active.".to_string());
        return;
    };

    let mut manager = match shell_manager.lock() {
        Ok(manager) => manager,
        Err(_) => {
            add_shell_job_message(
                app,
                "Shell tracking hit an internal error — restart Codewhale to recover.".to_string(),
            );
            return;
        }
    };
    let active_session_id = app.current_session_id.clone().unwrap_or_default();

    match action {
        crate::tui::app::ShellJobAction::List => {
            let jobs = manager.list_jobs_for_session(&active_session_id);
            let mut text = format_shell_job_list(&jobs);
            if let Ok(cloud) =
                crate::cloud_dispatch::CloudJobStore::from_env().and_then(|store| store.list())
                && !cloud.is_empty()
            {
                text.push_str("\n\n");
                text.push_str(&crate::cloud_dispatch::format_job_list(&cloud));
            }
            add_shell_job_message(app, text);
        }
        crate::tui::app::ShellJobAction::Show { id } => {
            match manager.inspect_job_for_session(&active_session_id, &id) {
                Ok(detail) => open_shell_job_pager(app, &detail),
                Err(err) => add_shell_job_message(app, format!("Command lookup failed: {err}")),
            }
        }
        crate::tui::app::ShellJobAction::Poll { id, wait } => {
            match manager.poll_delta_for_session(
                &active_session_id,
                &id,
                wait,
                if wait { 5_000 } else { 1_000 },
            ) {
                Ok(delta) => add_shell_job_message(app, format_shell_poll(&delta.result)),
                Err(err) => add_shell_job_message(app, format!("Command poll failed: {err}")),
            }
        }
        crate::tui::app::ShellJobAction::SendStdin { id, input, close } => {
            match manager.write_stdin_for_session(&active_session_id, &id, &input, close) {
                Ok(()) => {
                    match manager.poll_delta_for_session(&active_session_id, &id, false, 1_000) {
                        Ok(delta) => add_shell_job_message(app, format_shell_poll(&delta.result)),
                        Err(err) => {
                            add_shell_job_message(
                                app,
                                format!("Command input sent; poll failed: {err}"),
                            );
                        }
                    }
                }
                Err(err) => add_shell_job_message(app, format!("Command input failed: {err}")),
            }
        }
        crate::tui::app::ShellJobAction::Cancel { id } => {
            match manager.kill_for_session(&active_session_id, &id) {
                Ok(result) => add_shell_job_message(app, format_shell_poll(&result)),
                Err(err) => add_shell_job_message(app, format!("Command cancel failed: {err}")),
            }
        }
        crate::tui::app::ShellJobAction::CancelAll => {
            match manager.kill_running_for_session(&active_session_id) {
                Ok(results) => {
                    let count = results.len();
                    if count == 0 {
                        add_shell_job_message(app, "No running commands to cancel.".to_string());
                    } else {
                        let tasks: Vec<String> = results
                            .iter()
                            .filter_map(|result| result.task_id.clone())
                            .collect();
                        add_shell_job_message(
                            app,
                            format!("Canceled {count} command(s): {}", tasks.join(", ")),
                        );
                    }
                }
                Err(err) => add_shell_job_message(app, format!("Command cancel-all failed: {err}")),
            }
        }
    }
}

pub(crate) async fn handle_skill_mutation_requested(
    app: &mut App,
    request: crate::skills::mutation::SkillMutationRequest,
) {
    use crate::skills::install::{DEFAULT_MAX_SIZE_BYTES, DEFAULT_REGISTRY_URL};
    use crate::skills::mutation::{MutationContext, SkillMutationOutcome, SkillMutationRequest};

    let focus = match &request {
        SkillMutationRequest::ImportExternal { source_id, .. } => Some(source_id.clone()),
        SkillMutationRequest::Update { skill_id, .. }
        | SkillMutationRequest::Remove { skill_id, .. }
        | SkillMutationRequest::Trust { skill_id, .. } => Some(skill_id.clone()),
        SkillMutationRequest::InstallRemote { .. }
        | SkillMutationRequest::UpdateByName { .. }
        | SkillMutationRequest::RemoveByName { .. }
        | SkillMutationRequest::TrustByName { .. } => None,
    };

    let workspace = app.workspace.clone();
    let home = crate::config::effective_home_dir();
    let cfg = crate::config::Config::load(None, None).unwrap_or_default();
    let network = cfg
        .network
        .clone()
        .map(|policy| policy.into_runtime())
        .unwrap_or_default();
    let skills_cfg = cfg.skills.as_ref();
    let max_size = skills_cfg
        .and_then(|s| s.max_install_size_bytes)
        .unwrap_or(DEFAULT_MAX_SIZE_BYTES);
    let registry_url = skills_cfg
        .and_then(|s| s.registry_url.clone())
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());

    let skills_dir = app.skills_dir.clone();
    let result = {
        let ctx = MutationContext {
            workspace: &workspace,
            home: home.as_deref(),
            configured_skills_dir: Some(skills_dir.as_path()),
            network: &network,
            max_size,
            registry_url: &registry_url,
        };
        crate::skills::mutation::execute(request, &ctx).await
    };

    let (status, refresh_skills) = match result {
        Ok(receipt) => {
            let msg = match &receipt.outcome {
                SkillMutationOutcome::Installed => {
                    format!(
                        "Installed '{}' → {}",
                        receipt.name, receipt.safe_target_path
                    )
                }
                SkillMutationOutcome::Updated => format!("Updated '{}'", receipt.name),
                SkillMutationOutcome::NoChange => {
                    format!("'{}': no upstream change", receipt.name)
                }
                SkillMutationOutcome::Removed => format!("Removed '{}'", receipt.name),
                SkillMutationOutcome::Trusted => format!("Trusted '{}'", receipt.name),
                SkillMutationOutcome::Imported => {
                    format!("Imported '{}' → {}", receipt.name, receipt.safe_target_path)
                }
                SkillMutationOutcome::AlreadyPresent => {
                    format!("'{}' already present (exact duplicate)", receipt.name)
                }
                SkillMutationOutcome::NeedsApproval(host) => {
                    format!("Needs network approval for {host}")
                }
                SkillMutationOutcome::NetworkDenied(host) => {
                    format!("Network denied for {host}")
                }
            };
            let refresh = !matches!(
                receipt.outcome,
                SkillMutationOutcome::NeedsApproval(_) | SkillMutationOutcome::NetworkDenied(_)
            );
            (msg, refresh)
        }
        Err(err) => (format!("Skill mutation failed: {err:#}"), false),
    };

    app.status_message = Some(status.clone());
    if refresh_skills {
        app.refresh_skill_cache();
    }
    refresh_skills_manager_if_open(app, Some(status), focus.as_ref());
    app.needs_redraw = true;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_config_updated(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    task_manager: &SharedTaskManager,
    engine_handle: &mut EngineHandle,
    key: String,
    value: String,
    persist: bool,
) -> Result<bool> {
    let result = prepare_config_update_result(
        commands::set_config_value(app, &key, &value, persist),
        persist,
    );
    let telemetry_toast = (key == "telemetry")
        .then(|| {
            result.message.clone().map(|message| {
                let level = if result.is_error {
                    StatusToastLevel::Error
                } else {
                    StatusToastLevel::Success
                };
                (message, level)
            })
        })
        .flatten();
    let normalized_value = value.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    let cleared_root_approval = !result.is_error
        && persist
        && key == "approval_policy"
        && matches!(
            normalized_value.as_str(),
            "default" | "tui-default" | "use-tui-default"
        );
    // Theme / background changes require a full terminal repaint because
    // ratatui's incremental diff cannot see colors remapped by the backend.
    if matches!(
        key.as_str(),
        "theme" | "ui_theme" | "background_color" | "background" | "bg"
    ) {
        app.force_next_full_repaint = true;
    }
    let rejected = result.is_error;
    if apply_command_result(terminal, app, engine_handle, task_manager, config, result).await? {
        return Ok(true);
    }

    let focus_key = if cleared_root_approval {
        "permission_posture"
    } else {
        &key
    };
    refresh_config_view_after_commit(app, focus_key, rejected);
    if let Some((message, level)) = telemetry_toast {
        // The modal stays open, so a transcript-only command receipt would be
        // invisible. Keep the durable disk truth in the rebuilt row and show
        // the localized result above it.
        app.push_status_toast(message, level, Some(12_000));
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
async fn handle_theme_selection_updated(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    task_manager: &SharedTaskManager,
    engine_handle: &mut EngineHandle,
    theme: String,
    persist: bool,
) -> Result<bool> {
    let result = prepare_config_update_result(
        commands::set_config_value(app, "theme", &theme, persist),
        persist,
    );
    // The theme owns the shell paint and must bypass ratatui's incremental
    // cell diff, including an Esc rollback.
    app.force_next_full_repaint = true;
    if apply_command_result(terminal, app, engine_handle, task_manager, config, result).await? {
        return Ok(true);
    }
    refresh_config_view_if_open(app, "theme");
    Ok(false)
}

/// Whether a settled question is the host's own Plan hand-off rather than an
/// engine `request_user_input` call. The engine's request is recorded in
/// `pending_user_input_prompt` before its view opens, so a provider-chosen
/// tool-call id equal to the hand-off id still reaches the engine.
pub(crate) fn is_plan_handoff_request(app: &App, tool_id: &str) -> bool {
    (tool_id == crate::tui::plan_handoff::REQUEST_ID
        || tool_id.starts_with(&format!("{}:", crate::tui::plan_handoff::REQUEST_ID)))
        && app
            .pending_user_input_prompt
            .as_ref()
            .is_none_or(|(id, _)| id != tool_id)
}

fn plan_handoff_seed_title(plan: &crate::tui::plan_handoff::PendingPlanHandoff) -> String {
    // Match the graph's trimmed title without changing the approved message.
    plan.text
        .trim()
        .chars()
        .take(1024)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Stage one graph-owned item for a prose plan without replacing existing work.
async fn seed_plan_handoff_todos(
    app: &App,
    plan: &mut crate::tui::plan_handoff::PendingPlanHandoff,
) -> Result<(), String> {
    if plan.has_current_checklist {
        return Ok(());
    }
    let work = app
        .runtime_services
        .work
        .as_ref()
        .ok_or_else(|| "Work state is unavailable".to_string())?;
    let mut todos = work.current_todos().await?;
    let content = plan_handoff_seed_title(plan);
    if let Some(id) = plan.seeded_todo_id {
        return if todos.items.iter().any(|item| {
            item.id == id
                && item.content == content
                && item.status == crate::tools::todo::TodoStatus::Pending
        }) {
            Ok(())
        } else {
            Err("The approved plan's To-do changed; review the plan again".to_string())
        };
    }
    let id = todos
        .items
        .iter()
        .map(|item| item.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "To-do item IDs are exhausted".to_string())?;
    todos.items.push(crate::tools::todo::TodoItem {
        id,
        content,
        status: crate::tools::todo::TodoStatus::Pending,
    });
    work.apply_todo_update(&plan.session_id, "todo_write", &todos)
        .await?;
    plan.seeded_todo_id = Some(id);
    Ok(())
}

/// Remove only this unchanged seed from the latest projection. Unprojected
/// graph history remains; operation_parent cannot adopt an unprojected step.
async fn rollback_plan_handoff_seed(
    app: &App,
    plan: &mut crate::tui::plan_handoff::PendingPlanHandoff,
) -> Result<(), String> {
    let Some(id) = plan.seeded_todo_id else {
        return Ok(());
    };
    if app.current_session_id.as_deref() != Some(plan.session_id.as_str()) {
        return Err("The plan belongs to another session".to_string());
    }
    let work = app
        .runtime_services
        .work
        .as_ref()
        .ok_or_else(|| "Work state is unavailable".to_string())?;
    let mut todos = work.current_todos().await?;
    if let Some(item) = todos.items.iter().find(|item| item.id == id) {
        if item.content != plan_handoff_seed_title(plan)
            || item.status != crate::tools::todo::TodoStatus::Pending
        {
            return Err(
                "The approved plan's To-do changed; review it before continuing".to_string(),
            );
        }
        todos.items.retain(|item| item.id != id);
        work.apply_todo_update(&plan.session_id, "todo_write", &todos)
            .await?;
    }
    plan.seeded_todo_id = None;
    Ok(())
}

fn reopen_plan_handoff(
    app: &mut App,
    plan: &crate::tui::plan_handoff::PendingPlanHandoff,
    reason: &str,
) {
    app.view_stack.push(UserInputView::new(
        plan.request_id.clone(),
        crate::tui::plan_handoff::request(app.ui_locale),
    ));
    let notice = app
        .tr(MessageId::SessionSaveFailed)
        .replace("{id}", &plan.session_id)
        .replace("{error}", reason);
    app.push_status_toast(notice, StatusToastLevel::Warning, Some(8_000));
}

/// Carry out only the answer bound to this exact completed Plan response.
pub(crate) async fn apply_plan_handoff(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    request_id: &str,
    choice: crate::tui::plan_handoff::PlanHandoffChoice,
) -> Result<()> {
    apply_plan_handoff_with_checkpoint(app, config, engine_handle, request_id, choice, |app| {
        Box::pin(persist_pending_work_checkpoint(app))
    })
    .await
}

type PlanHandoffCheckpoint =
    for<'a> fn(&'a mut App) -> Pin<Box<dyn Future<Output = Result<bool, String>> + 'a>>;

/// The checkpoint callback keeps the existing admission boundary explicit;
/// tests can refuse admission without mutating the global persistence actor.
pub(crate) async fn apply_plan_handoff_with_checkpoint(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    request_id: &str,
    choice: crate::tui::plan_handoff::PlanHandoffChoice,
    checkpoint: PlanHandoffCheckpoint,
) -> Result<()> {
    use crate::tui::plan_handoff::PlanHandoffChoice;

    let Some(mut plan) = app
        .pending_plan_handoff
        .clone()
        .filter(|plan| app.plan_handoff_is_current(plan, request_id))
    else {
        return Ok(());
    };
    if !matches!(&choice, PlanHandoffChoice::Work(_))
        && let Err(reason) = rollback_plan_handoff_seed(app, &mut plan).await
    {
        app.pending_plan_handoff = Some(plan.clone());
        reopen_plan_handoff(app, &plan, &reason);
        return Ok(());
    }
    let message = match choice {
        PlanHandoffChoice::KeepPlanning => {
            app.pending_plan_handoff = None;
            return Ok(());
        }
        PlanHandoffChoice::Revise(feedback) => QueuedMessage::new(feedback, None),
        PlanHandoffChoice::Work(posture) => {
            if !enter_work_for_plan(app, config, engine_handle, posture).await {
                return Ok(());
            }
            let staged = seed_plan_handoff_todos(app, &mut plan).await;
            app.pending_plan_handoff = Some(plan.clone());
            let checkpoint = match staged {
                Ok(()) => checkpoint(app).await.map(|_| ()),
                Err(reason) => Err(reason),
            };
            if let Err(reason) = checkpoint {
                // Undo our unpublished projection before the event loop can
                // retry an ordinary checkpoint. Never roll back other work.
                // An already queued write cannot be recalled by this repair.
                let reason = match rollback_plan_handoff_seed(app, &mut plan).await {
                    Ok(()) => reason,
                    Err(cleanup) => format!("{reason}; {cleanup}"),
                };
                app.pending_plan_handoff = Some(plan.clone());
                apply_mode_update(app, engine_handle, config, AppMode::Plan).await;
                reopen_plan_handoff(app, &plan, &reason);
                return Ok(());
            }
            QueuedMessage::new(
                format!("{}\n\n{}", app.tr(MessageId::PlanHandoffProceed), plan.text),
                None,
            )
        }
    };
    // Consume before ordinary composer admission. Its existing recovery owns
    // the exact user message on an offline/failed send; a repeated answer
    // cannot enqueue another execution.
    app.pending_plan_handoff = None;
    let action = ComposerSubmitAction::Submit(app.decide_submit_disposition());
    dispatch_composer_message(
        app,
        config,
        engine_handle,
        message,
        DispatchRecovery::Immediate,
        action,
    )
    .await
}

/// Leave Plan for Work with the chosen permission. Returns `false`, with the
/// reason on screen, when the permission or the mode could not be applied;
/// the session then stays where it was and nothing is sent.
pub(crate) async fn enter_work_for_plan(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    posture: ApprovalMode,
) -> bool {
    if engine_handle.tx_op.is_closed() {
        return false;
    }
    if app.agent_approval_baseline() != posture
        && let Err(reason) = app.apply_agent_posture(posture)
    {
        app.push_status_toast(reason, StatusToastLevel::Warning, Some(8_000));
        return false;
    }
    apply_mode_update(app, engine_handle, config, AppMode::Agent).await;
    if engine_handle.tx_op.is_closed() {
        app.set_mode(AppMode::Plan);
        return false;
    }
    app.mode == AppMode::Agent
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_view_events(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    task_manager: &SharedTaskManager,
    engine_handle: &mut EngineHandle,
    events: Vec<ViewEvent>,
) -> Result<bool> {
    for event in events {
        match event {
            ViewEvent::CommandPaletteSelected { action } => match action {
                crate::tui::views::CommandPaletteAction::ExecuteCommand { command } => {
                    if execute_command_input(
                        terminal,
                        app,
                        engine_handle,
                        task_manager,
                        config,
                        &command,
                    )
                    .await?
                    {
                        return Ok(true);
                    }
                    // A command review confirmed over the Extensions panel
                    // (the plugin trust digest) closes its pager and lands
                    // back on the list, which must show the state the
                    // confirmation just changed.
                    if app.view_stack.extensions_is_top() {
                        let snapshot =
                            crate::tui::views::extensions::ExtensionsSnapshot::from_app(app);
                        app.view_stack.refresh_extensions(snapshot);
                    }
                }
                crate::tui::views::CommandPaletteAction::InsertText { text } => {
                    app.input = text;
                    app.cursor_position = app.input.chars().count();
                    app.status_message = Some(
                        "Inserted into composer. Finish the input or press Enter.".to_string(),
                    );
                }
                crate::tui::views::CommandPaletteAction::OpenTextPager { title, content } => {
                    open_text_pager(app, title, content);
                }
            },
            ViewEvent::ExecutePanelCommand {
                command,
                pager_title,
            } => {
                // The Extensions panel stays open for this command. Inspect
                // rows divert their text output into a pager stacked on the
                // panel instead of a transcript dump; mutations keep their
                // transcript receipt either way.
                let mut result = crate::commands::execute_with_config(&command, app, config);
                if let Some(title) = pager_title
                    && let Some(text) = result.message.take()
                {
                    open_text_pager(app, title, text);
                }
                if apply_command_result(terminal, app, engine_handle, task_manager, config, result)
                    .await?
                {
                    return Ok(true);
                }
                // The row the user just changed re-reads live state, and so
                // does every sibling — a plugin enable, an MCP retry, or an
                // install lands on the still-open list instead of leaving it
                // stale until reopen.
                let snapshot = crate::tui::views::extensions::ExtensionsSnapshot::from_app(app);
                app.view_stack.refresh_extensions(snapshot);
            }
            ViewEvent::RefreshExtensions {
                mcp_generation,
                mcp_initializing,
            } => {
                // Bounded poll from the open panel: rebuild only when the
                // MCP generation or the initializing flag moved past what
                // the panel's snapshot last saw.
                if app.view_stack.extensions_is_top()
                    && (mcp_generation != app.mcp_snapshot_generation
                        || app.mcp_snapshot_generation_invalidated
                        || mcp_initializing != app.mcp_initializing)
                {
                    let snapshot = crate::tui::views::extensions::ExtensionsSnapshot::from_app(app);
                    app.view_stack.refresh_extensions(snapshot);
                }
            }
            ViewEvent::OpenTextPager { title, content } => {
                open_text_pager(app, title, content);
            }
            ViewEvent::CopyToClipboard { text, label } => {
                if text.is_empty() {
                    app.status_message = Some(format!("{label} is empty"));
                } else {
                    app.status_message = Some(match app.clipboard.write_text_status(&text) {
                        Ok(transport) => copy_receipt(app, transport, format!("{label} copied")),
                        Err(_) => format!("Copy failed ({label})"),
                    });
                }
            }
            ViewEvent::ApprovalDecision {
                tool_id,
                tool_name,
                decision,
                timed_out,
                approval_key,
                approval_grouping_key,
                persistent_rules,
            } => {
                apply_approval_decision(
                    app,
                    engine_handle,
                    config,
                    ApprovalDecisionEvent {
                        tool_id,
                        tool_name,
                        decision,
                        timed_out,
                        approval_key,
                        approval_grouping_key,
                        persistent_rules,
                    },
                )
                .await;

                if timed_out {
                    app.add_message(HistoryCell::System {
                        content: app.tr(MessageId::ApprovalTimedOutDenied).into_owned(),
                    });
                }
            }
            ViewEvent::ElevationDecision {
                tool_id,
                tool_name,
                option,
            } => {
                use crate::tui::approval::ElevationOption;
                let result = match option {
                    ElevationOption::Abort => {
                        app.add_message(HistoryCell::System {
                            content: format!("Sandbox elevation aborted for {tool_name}"),
                        });
                        engine_handle.deny_tool_call(tool_id.clone()).await
                    }
                    ElevationOption::WithNetwork => {
                        app.add_message(HistoryCell::System {
                            content: format!("Retrying {tool_name} with network access enabled"),
                        });
                        let policy = option.to_policy(&app.workspace);
                        engine_handle
                            .retry_tool_with_policy(tool_id.clone(), policy)
                            .await
                    }
                    ElevationOption::WithWriteAccess(_) => {
                        app.add_message(HistoryCell::System {
                            content: format!("Retrying {tool_name} with write access enabled"),
                        });
                        let policy = option.to_policy(&app.workspace);
                        engine_handle
                            .retry_tool_with_policy(tool_id.clone(), policy)
                            .await
                    }
                    ElevationOption::FullAccess => {
                        app.add_message(HistoryCell::System {
                            content: format!("Retrying {tool_name} with full access (no sandbox)"),
                        });
                        let policy = option.to_policy(&app.workspace);
                        engine_handle
                            .retry_tool_with_policy(tool_id.clone(), policy)
                            .await
                    }
                };
                if result.is_ok() {
                    note_human_decision_delivered(app, &tool_id);
                    app.retire_action_notices(Some(&tool_id));
                }
            }
            ViewEvent::UserInputSubmitted { tool_id, response }
                if is_plan_handoff_request(app, &tool_id) =>
            {
                let choice = crate::tui::plan_handoff::choice(app.ui_locale, &response);
                apply_plan_handoff(app, config, engine_handle, &tool_id, choice).await?;
            }
            // Esc follows the same owned-seed cleanup as Keep planning.
            ViewEvent::UserInputCancelled { tool_id } if is_plan_handoff_request(app, &tool_id) => {
                apply_plan_handoff(
                    app,
                    config,
                    engine_handle,
                    &tool_id,
                    crate::tui::plan_handoff::PlanHandoffChoice::KeepPlanning,
                )
                .await?;
            }
            ViewEvent::UserInputSubmitted { tool_id, response } => {
                let result = engine_handle
                    .submit_user_input(tool_id.clone(), response)
                    .await;
                apply_user_input_submission_result(app, &tool_id, result);
            }
            ViewEvent::UserInputCancelled { tool_id } => {
                // A cancel is an answer too: when it cannot reach the engine
                // the question is still pending there, so reopen it the way a
                // failed submit does instead of recording a cancel (U02-04).
                let result = engine_handle.cancel_user_input(tool_id.clone()).await;
                let delivered = result.is_ok();
                apply_user_input_submission_result(app, &tool_id, result);
                if delivered {
                    app.add_message(HistoryCell::System {
                        content: "User input cancelled".to_string(),
                    });
                }
            }
            ViewEvent::SessionSelected { session_id } => {
                let manager = match SessionManager::default_location() {
                    Ok(manager) => manager,
                    Err(err) => {
                        app.status_message =
                            Some(format!("Failed to open sessions directory: {err}"));
                        continue;
                    }
                };

                // Another window's open session is refused, not attached
                // as a second autosaving writer.
                match manager.attach_session(&session_id) {
                    Ok((recovery, lease)) => {
                        let session = recovery.session;
                        let next_config = config.clone();
                        let message_count = session.metadata.message_count;
                        // Keep the saved-store confinement check a pure
                        // comparison on this runtime (#6522).
                        crate::runtime_threads::prepare_canonical_sessions_root().await;
                        let respawn = match apply_loaded_session_config_snapshot(
                            app,
                            config,
                            session,
                            next_config,
                            false,
                        ) {
                            Ok(outcome) => {
                                // Only now does this window give up the
                                // session it had open; a failed restore
                                // above keeps that session's lease.
                                lease.commit();
                                outcome
                            }
                            Err(err) => {
                                crate::tui::ui::session_state::surface_session_load_failure(
                                    app,
                                    format!("Failed to restore session: {err}"),
                                );
                                continue;
                            }
                        };
                        sync_runtime_workspace_state(task_manager, app.workspace.clone()).await;
                        // #6150 audit: these sends may await a full op channel,
                        // and that await is load-bearing — the session switch
                        // is already committed UI-side, so each op must land in
                        // order (drop = engine/UI desync). A wedge is possible
                        // only while a saturated engine finishes its turn.
                        if respawn {
                            let _ = engine_handle.send(Op::Shutdown).await;
                            *engine_handle =
                                spawn_tui_engine(build_engine_config(app, config), config);
                        } else {
                            let _ = engine_handle
                                .send(Op::SetModel {
                                    model: app.model.clone(),
                                    mode: app.mode,
                                    route_limits: app.active_route_limits,
                                })
                                .await;
                        }
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
                        let _ = engine_handle
                            .send(Op::SetCompaction {
                                config: app.compaction_config(),
                            })
                            .await;
                        // Durable receipt, matching `/load`: the status toast
                        // alone is replaced by the next footer update, leaving
                        // no findable record that the resume happened.
                        let loaded_message = format!(
                            "Session loaded (ID: {}, {} messages)",
                            crate::session_manager::truncate_id(&session_id),
                            message_count
                        );
                        app.add_message(HistoryCell::System {
                            content: loaded_message.clone(),
                        });
                        app.status_message = Some(loaded_message);
                        app.launch.dismiss();
                        app.launch.status = None;
                    }
                    Err(err) => {
                        crate::tui::ui::session_state::surface_session_load_failure(
                            app,
                            format!(
                                "Failed to load session {}: {err}",
                                crate::session_manager::truncate_id(&session_id)
                            ),
                        );
                    }
                }
            }
            ViewEvent::SessionRenamed { metadata } => {
                let session_id = metadata.id.clone();
                let title = metadata.title.clone();
                let mut work_snapshot_warning = None;
                if apply_picker_session_rename_to_active_app(app, *metadata)
                    && let Ok(manager) = SessionManager::default_location()
                {
                    match build_session_snapshot(app, &manager) {
                        Ok(session) => {
                            if let Err(err) = persist_with_pending_work_boundary(
                                app,
                                PersistRequest::SessionSnapshot(session),
                            ) {
                                tracing::warn!(
                                    session_id = %session_id,
                                    error = %err,
                                    "Could not queue active session rename Work snapshot"
                                );
                                work_snapshot_warning = Some(format!(
                                    "Session renamed, but Work snapshot is pending ({err})"
                                ));
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %err,
                                "Could not queue active session rename snapshot"
                            );
                        }
                    }
                }
                app.status_message = Some(work_snapshot_warning.unwrap_or_else(|| {
                    format!(
                        "Renamed session {} to \"{}\"",
                        crate::session_manager::truncate_id(&session_id),
                        title
                    )
                }));
            }
            ViewEvent::SessionArchived { metadata } => {
                // The manager already wrote the flag. Keep the active app's
                // cached metadata in step so the next autosave carries the new
                // state forward instead of reverting it, and drop the rail
                // cache so the row disappears (or returns) immediately.
                if let Some(cached) = app.current_session_metadata.as_mut()
                    && cached.id == metadata.id
                {
                    cached.archived = metadata.archived;
                }
                app.status_message = Some(format!(
                    "{} session {} ({})",
                    if metadata.archived {
                        "Archived"
                    } else {
                        "Restored"
                    },
                    crate::session_manager::truncate_id(&metadata.id),
                    metadata.title
                ));
            }
            ViewEvent::SessionDeleted { session_id, title } => {
                app.status_message = Some(format!(
                    "Deleted session {} ({})",
                    crate::session_manager::truncate_id(&session_id),
                    title
                ));
            }
            ViewEvent::ConfigUpdated {
                key,
                value,
                persist,
            } => {
                if handle_config_updated(
                    terminal,
                    app,
                    config,
                    task_manager,
                    engine_handle,
                    key,
                    value,
                    persist,
                )
                .await?
                {
                    return Ok(true);
                }
            }
            ViewEvent::ThemeSelectionUpdated { theme, persist } => {
                if handle_theme_selection_updated(
                    terminal,
                    app,
                    config,
                    task_manager,
                    engine_handle,
                    theme,
                    persist,
                )
                .await?
                {
                    return Ok(true);
                }
            }
            ViewEvent::StatusItemsUpdated { items, final_save } => {
                // Apply to the live App immediately so the footer reflects
                // every keystroke (live preview).
                app.status_items = items.clone();
                app.needs_redraw = true;
                if final_save {
                    match crate::config_persistence::persist_status_items(&items) {
                        Ok(path) => {
                            app.status_message =
                                Some(format!("Status line saved to {}", path.display()));
                        }
                        Err(err) => {
                            app.add_message(HistoryCell::System {
                                content: format!("Failed to save status line: {err}"),
                            });
                        }
                    }
                }
            }
            ViewEvent::HotbarSetupSaved { bindings } => {
                apply_hotbar_setup_saved(app, config, bindings);
            }
            ViewEvent::SetupStateCommitRequested { state, message } => match state.save() {
                Ok(()) => {
                    app.status_message = Some(message);
                }
                Err(err) => {
                    app.status_message = Some(format!("Setup state could not be saved: {err}"));
                }
            },
            ViewEvent::SetupConstitutionCommitRequested {
                constitution,
                state,
                message,
            } => match crate::tui::setup::persist_user_constitution_choice(&constitution, &state) {
                Ok(()) => {
                    app.status_message = Some(message);
                }
                Err(err) => {
                    app.status_message =
                        Some(format!("User constitution could not be saved: {err}"));
                }
            },
            ViewEvent::SetupConstitutionModelDraftRequested {
                draft,
                freeform_note,
                locale,
            } => {
                handle_setup_constitution_model_draft(app, config, draft, freeform_note, locale)
                    .await;
            }
            ViewEvent::FleetProfileModelDraftRequested {
                role,
                model,
                provider,
                reasoning_effort,
                locale,
            } => {
                handle_fleet_profile_model_draft(
                    app,
                    config,
                    role,
                    model,
                    provider,
                    reasoning_effort,
                    locale,
                )
                .await;
            }
            ViewEvent::FleetRosterOpenCoordinatorRequested => {
                app.view_stack.push(
                    crate::tui::model_picker::ModelPickerView::new(app, config)
                        .with_assignment_context("Coordinator", "Current session"),
                );
            }
            ViewEvent::FleetProfileRoutePickRequested { editor_id } => {
                if app.view_stack.top_kind() == Some(ModalKind::FleetSetup)
                    && let Some(mut boxed) = app.view_stack.pop()
                {
                    let selection = boxed
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>()
                        .and_then(|view| {
                            view.route_selection(editor_id)
                                .map(|selection| (selection, view.assignment_context()))
                        });
                    app.view_stack.push_boxed(boxed);
                    if let Some((selection, (role, scope))) = selection {
                        app.view_stack.push(
                            crate::tui::model_picker::ModelPickerView::new_for_fleet_profile(
                                app, config, editor_id, selection,
                            )
                            .with_assignment_context(role, scope),
                        );
                    }
                }
            }
            ViewEvent::FleetProfileRoutePicked {
                editor_id,
                provider,
                provider_id,
                model,
                reasoning,
            } => {
                if app.view_stack.top_kind() == Some(ModalKind::FleetSetup)
                    && let Some(mut boxed) = app.view_stack.pop()
                {
                    if let Some(view) = boxed
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>(
                    ) {
                        view.accept_route(
                            editor_id,
                            provider_id.unwrap_or_else(|| provider.as_str().into()),
                            model,
                            reasoning,
                        );
                    }
                    app.view_stack.push_boxed(boxed);
                }
            }
            ViewEvent::FleetProfileRouteCommitRequested { editor_id } => {
                if app.view_stack.top_kind() == Some(ModalKind::FleetSetup)
                    && let Some(mut boxed) = app.view_stack.pop()
                {
                    let result = boxed
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>()
                        .map(|view| view.commit_route_assignment(editor_id, app, config));
                    match result {
                        Some(Ok(message)) => {
                            sync_fleet_roster(app, config, engine_handle);
                            refresh_parked_fleet_roster(app, config);
                            app.push_status_toast(message, StatusToastLevel::Success, Some(8_000));
                        }
                        Some(Err(reason)) => {
                            app.view_stack.push_boxed(boxed);
                            app.set_sticky_status(reason, StatusToastLevel::Error, None);
                        }
                        None => app.view_stack.push_boxed(boxed),
                    }
                }
            }
            ViewEvent::FleetAssignmentPickerDismissed { editor_id } => {
                dismiss_fleet_assignment(app, editor_id);
                refresh_parked_fleet_roster(app, config);
            }
            ViewEvent::FleetRosterOpenSetupRequested { member_id } => {
                // The shared router opens the selected v2 Fleet's exact editor
                // (focused on this member) or the legacy wizard when no named
                // Fleet is selected.
                open_fleet_setup_target(app, config, Some(&member_id));
            }
            ViewEvent::FleetListOpenDetailRequested { name, scope } => {
                if app.view_stack.top_kind() != Some(ModalKind::FleetDetail) {
                    if let Some(view) = crate::tui::views::fleet_detail::FleetDetailView::open(
                        app, config, &name, scope,
                    ) {
                        app.view_stack.push(view);
                    } else {
                        app.set_sticky_status(
                            format!(
                                "Could not open team `{name}` ({}) — the file may have moved or become unreadable.",
                                scope.label()
                            ),
                            crate::tui::app::StatusToastLevel::Error,
                            None,
                        );
                    }
                }
            }
            // Enter on a Fleet editor row: the standard `/model` picker opens
            // on top of the editor, and its pick comes back below as
            // `FleetRoutePicked` to land on the editor still on the stack.
            ViewEvent::FleetDetailRoutePickRequested { target, editor_id } => {
                let selection = if app.view_stack.top_kind() == Some(ModalKind::FleetDetail)
                    && let Some(mut editor) = app.view_stack.pop()
                {
                    let selection = editor
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_detail::FleetDetailView>()
                        .and_then(|view| {
                            view.route_selection(editor_id, target)
                                .map(|selection| (selection, view.assignment_context()))
                        });
                    app.view_stack.push_boxed(editor);
                    selection
                } else {
                    None
                };
                if let Some((selection, (role, scope))) = selection {
                    app.view_stack.push(
                        crate::tui::model_picker::ModelPickerView::new_for_fleet_route(
                            app, config, target, editor_id, selection,
                        )
                        .with_assignment_context(role, scope),
                    );
                }
            }
            ViewEvent::FleetRoutePicked {
                target,
                editor_id,
                provider,
                provider_id,
                model,
                reasoning,
            } => {
                let provider_key = provider_id.unwrap_or_else(|| provider.as_str().to_string());
                // The picker's `auto` row is "inherit": the Fleet row follows
                // the session route again.
                let pin = (model != "auto").then_some((provider_key, model));
                if let Some((provider_key, _)) = &pin
                    && let Some(rejection) =
                        crate::commands::fleet_provider_rejection(app, config, provider_key)
                {
                    // Same gate as `/fleet add` and ⇧F: an unconfigured route
                    // never enters a team from the picker.
                    app.set_sticky_status(rejection, StatusToastLevel::Error, None);
                } else if app.view_stack.top_kind() == Some(ModalKind::FleetDetail)
                    && let Some(mut boxed) = app.view_stack.pop()
                {
                    let outcome = boxed
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_detail::FleetDetailView>()
                        .map(|view| {
                            let (provider, model) = match pin {
                                Some((provider, model)) => (Some(provider), Some(model)),
                                None => (None, None),
                            };
                            view.apply_picked_route(editor_id, target, provider, model, reasoning)
                        });
                    app.view_stack.push_boxed(boxed);
                    match outcome {
                        Some(Ok(message)) => {
                            if let Some(mut editor) = app.view_stack.pop() {
                                let direct = editor.as_any_mut().downcast_mut::<crate::tui::views::fleet_detail::FleetDetailView>()
                                    .is_some_and(|view| view.is_direct_assignment(editor_id));
                                if !direct {
                                    app.view_stack.push_boxed(editor);
                                }
                            }
                            app.push_status_toast(message, StatusToastLevel::Success, Some(8_000));
                            sync_fleet_roster(app, config, engine_handle);
                            refresh_parked_fleet_roster(app, config);
                        }
                        Some(Err(reason)) => {
                            app.set_sticky_status(reason, StatusToastLevel::Error, None);
                        }
                        None => {}
                    }
                } else {
                    app.set_sticky_status(
                        codewhale_localization::tr(
                            app.ui_locale,
                            codewhale_localization::MessageId::FleetRoutePickUnavailable,
                        )
                        .into_owned(),
                        StatusToastLevel::Error,
                        None,
                    );
                }
                app.needs_redraw = true;
            }
            ViewEvent::FleetStoreChanged { message } => {
                app.status_message = Some(message);
                sync_fleet_roster(app, config, engine_handle);
                refresh_parked_fleet_roster(app, config);
            }
            // #5954: the roster emits (rather than emit-and-closes) these, so
            // it is still on the stack right underneath. Pushing on top makes
            // the three Fleet views one stack: `Esc` pops back to the roster,
            // and only closes the window at the root.
            ViewEvent::FleetRosterOpenFleetsRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::FleetList) {
                    let over_roster = app.view_stack.top_kind() == Some(ModalKind::FleetRoster);
                    let mut view = crate::tui::views::fleet_list::FleetListView::new(app, config);
                    if over_roster {
                        view = view.over_fleet_roster();
                    }
                    app.view_stack.push(view);
                }
            }
            ViewEvent::FleetRosterOpenWorkersRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::SubAgents) {
                    let over_roster = app.view_stack.top_kind() == Some(ModalKind::FleetRoster);
                    let agents = subagent_view_agents(app, &app.subagent_cache);
                    let mut view = crate::tui::views::SubAgentsView::for_app(app, agents);
                    if over_roster {
                        view = view.over_fleet_roster();
                    }
                    app.view_stack.push(view);
                }
                app.status_message =
                    Some(tr(app.ui_locale, MessageId::SubagentsFetching).to_string());
                let _ = engine_handle.try_send(Op::ListSubAgents);
            }
            ViewEvent::FleetSetupExternalConsentActivationRequested { provider_id, model } => {
                // Validate the selected Fleet route by minting the read-only
                // external credential capability only for this exact
                // provider/source/path. The check is route-scoped: a cloned
                // config has the target provider active so credential discovery
                // succeeds, but the parent session provider/model are never
                // mutated.
                let identity = match config.resolve_provider_selection_identity(&provider_id) {
                    Ok(identity) => identity,
                    Err(error) => {
                        app.set_sticky_status(
                            format!("Team route activation failed: {error}"),
                            crate::tui::app::StatusToastLevel::Error,
                            None,
                        );
                        app.needs_redraw = true;
                        continue;
                    }
                };
                let provider_label = identity
                    .compatibility()
                    .map_or(identity.key.as_str(), |row| row.label);
                let mut scoped = config.clone();
                let validation = scoped
                    .scope_to_provider_identity(&identity)
                    .and_then(|()| {
                        crate::route_runtime::resolve_runtime_route_for_identity(
                            &scoped,
                            &identity,
                            Some(&model),
                        )
                    })
                    .and_then(|route| route.validate().map_err(|err| err.to_string()));
                match validation {
                    Ok(validated) => {
                        app.provider_health.record_success(
                            &scoped,
                            &validated.client.turn_route_receipt(),
                            &validated.model,
                        );
                        app.push_status_toast(
                            format!(
                                "{provider_label} route activated for team: {}",
                                validated.model
                            ),
                            crate::tui::app::StatusToastLevel::Success,
                            Some(5_000),
                        );
                    }
                    Err(error) => {
                        let envelope = ErrorEnvelope::new(
                            ErrorCategory::Authentication,
                            ErrorSeverity::Error,
                            false,
                            "route_validation_failed",
                            &error,
                        );
                        if let Some(generation) =
                            scoped.readonly_health_credential_generation(&identity)
                        {
                            app.provider_health.record_models_probe_failure(
                                &scoped,
                                &identity,
                                &model,
                                generation,
                                envelope.category,
                                &envelope.message,
                            );
                        }
                        app.push_status_toast(
                            format!("{provider_label} route activation failed: {error}"),
                            crate::tui::app::StatusToastLevel::Error,
                            None,
                        );
                    }
                }
                // Refresh the Fleet setup view from a snapshot built against the
                // updated health state so the activated row becomes Ready
                // without closing the modal.
                if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::FleetSetup)
                    && let Some(view) = app.view_stack.pop()
                {
                    let mut restored = view;
                    if let Some(fleet_setup) = restored
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::fleet_setup::FleetSetupView>(
                    ) {
                        let fresh = crate::tui::views::fleet_setup::FleetSetupSnapshot::from_app(
                            app, config,
                        );
                        fleet_setup.refresh_from_snapshot(fresh);
                    }
                    app.view_stack.push_boxed(restored);
                }
                app.needs_redraw = true;
            }
            ViewEvent::FleetProfileDraftCommitRequested { draft, scope } => {
                // A project-scope save is refused (never silently redirected)
                // when project profiles are disabled for this launch: the file
                // would be written where nothing loads it.
                if scope == crate::fleet::profile::FleetProfileScope::Project
                    && !crate::fleet::roster::project_agent_profiles_enabled()
                {
                    app.set_sticky_status(
                        tr(app.ui_locale, MessageId::FleetDestProjectDisabledSave).into_owned(),
                        StatusToastLevel::Error,
                        None,
                    );
                    app.needs_redraw = true;
                    continue;
                }
                // The TOML is rendered deterministically from the validated
                // draft and written atomically; the target path is derived
                // from the sanitized id, never model-chosen.
                let profile_dir =
                    match crate::fleet::profile::agent_profile_dir_for_scope(scope, &app.workspace)
                    {
                        Ok(dir) => dir,
                        Err(err) => {
                            app.set_sticky_status(
                                format!("Team {} scope is unavailable: {err:#}", scope.label()),
                                StatusToastLevel::Error,
                                None,
                            );
                            app.needs_redraw = true;
                            continue;
                        }
                    };
                let target = profile_dir.join(draft.file_name());
                // A ratified profile must not silently clobber a differently
                // named existing profile that shares this id (which would also
                // make the whole agents dir fail to load on the duplicate).
                // Overwriting the SAME file is fine — that is an intentional
                // re-draft of this profile.
                // The collision gate only needs file identities. Accept
                // otherwise legacy profile fields here so an old, unrelated
                // profile cannot block saving a current one. Malformed TOML,
                // unreadable files, and invalid ids still fail closed because
                // then we cannot prove there is no collision.
                let existing_profiles =
                    crate::fleet::profile::load_agent_profile_identities_from_dir(&profile_dir);
                if let Err(err) = &existing_profiles {
                    let message = tr(app.ui_locale, MessageId::FleetProfileIdentityVerifyFailed)
                        .replace("{error}", &format!("{err:#}"));
                    app.set_sticky_status(message, StatusToastLevel::Error, None);
                    app.needs_redraw = true;
                    continue;
                }
                let id_conflict = existing_profiles
                    .into_iter()
                    .flatten()
                    .find(|p| p.id.eq_ignore_ascii_case(&draft.id) && p.source != target);
                if let Some(existing) = id_conflict {
                    let message = tr(app.ui_locale, MessageId::FleetProfileIdConflict)
                        .replace("{id}", &draft.id)
                        .replace("{path}", &existing.source.display().to_string());
                    app.set_sticky_status(message, StatusToastLevel::Error, None);
                    app.needs_redraw = true;
                    continue;
                }
                // #4093 AC #5: a profile may only pin a provider the operator
                // has actually configured/credentialed. The picker already
                // offers models only from configured providers, but a
                // model-drafted or hand-edited route (or credentials removed
                // after the pick) could still name an unconfigured one — which
                // would fail loudly at launch. Catch it at save time with a
                // clear message, reusing the SAME predicate the picker uses.
                if let Some(provider_id) = draft.provider.as_deref() {
                    let checked =
                        config
                            .resolve_provider_pin_identity(provider_id)
                            .and_then(|identity| {
                                let active = app.admitted_provider_identity()?;
                                if crate::config::provider_is_configured_for_active(
                                    config, &identity, active,
                                ) {
                                    Ok(())
                                } else {
                                    Err(tr(
                                        app.ui_locale,
                                        MessageId::FleetProfileProviderUnconfigured,
                                    )
                                    .replace("{provider}", provider_id)
                                    .replace(
                                        "{env}",
                                        &identity.provider.provider().env_vars().join(" / "),
                                    ))
                                }
                            });
                    if let Err(message) = checked {
                        app.set_sticky_status(message, StatusToastLevel::Error, None);
                        app.needs_redraw = true;
                        continue;
                    }
                }
                let mut txn = codewhale_config::persistence::SetupTransaction::new();
                txn.stage(target.clone(), draft.render_toml().into_bytes());
                match txn.commit() {
                    Ok(()) => {
                        let roster =
                            std::sync::Arc::new(crate::fleet::identity::load_effective_roster(
                                &config.fleet_config(),
                                &app.workspace,
                                Some(app.extension_plugin_view().as_ref()),
                            ));
                        let roster_refresh_failed = engine_handle
                            .try_send(Op::SetFleetRoster { roster })
                            .is_err();
                        let zh = app.ui_locale == codewhale_localization::Locale::ZhHans;
                        app.add_message(HistoryCell::System {
                            content: if zh {
                                format!("已保存团队配置：{}", target.display())
                            } else {
                                format!(
                                    "Team {} profile saved: {}",
                                    scope.label(),
                                    target.display()
                                )
                            },
                        });
                        app.status_message = Some(if zh {
                            format!("已保存团队配置：{}", draft.file_name())
                        } else if roster_refresh_failed {
                            format!(
                                "Team {} profile saved, but the live roster could not refresh; restart before dispatching {}",
                                scope.label(),
                                draft.id
                            )
                        } else {
                            format!(
                                "Team {} profile saved: {}",
                                scope.label(),
                                draft.file_name()
                            )
                        });
                    }
                    Err(err) => {
                        app.status_message =
                            Some(if app.ui_locale == codewhale_localization::Locale::ZhHans {
                                format!("无法保存团队配置：{err:#}")
                            } else {
                                format!("Team profile could not be saved: {err:#}")
                            });
                    }
                }
                app.needs_redraw = true;
            }
            ViewEvent::SetupRuntimePresetApplyRequested {
                preset,
                state,
                message,
            } => match apply_setup_runtime_preset(app, config, preset, state) {
                Ok(summary) => {
                    sync_mode_update(app, engine_handle).await;
                    app.status_message = Some(format!("{message} {summary}"));
                }
                Err(err) => {
                    app.status_message =
                        Some(format!("Runtime preset could not be applied: {err:#}"));
                }
            },
            ViewEvent::SetupOpenProviderRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::ProviderPicker) {
                    let runtime_status = query_provider_runtime_status(engine_handle).await;
                    app.view_stack.push(
                        crate::tui::provider_picker::ProviderPickerView::new_for_setup(
                            app.api_provider,
                            app.admitted_provider_identity()
                                .ok()
                                .map(|identity| identity.key.clone()),
                            config,
                            runtime_status,
                        )
                        .with_locale(app.ui_locale)
                        .with_provider_health(&app.provider_health),
                    );
                    app.status_message =
                        Some("Provider setup opened from /setup readiness.".to_string());
                }
            }
            ViewEvent::SetupOpenModelRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::ModelPicker) {
                    if let Ok(identity) = app.admitted_provider_identity().cloned() {
                        open_model_picker_for_provider(app, config, &identity);
                    }
                    app.status_message =
                        Some("Model route picker opened from /setup readiness.".to_string());
                }
            }
            ViewEvent::SetupOpenFleetRequested => {
                open_fleet_setup_target(app, config, None);
            }
            ViewEvent::SetupOpenHotbarRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::HotbarSetup) {
                    app.view_stack
                        .push(crate::tui::hotbar::setup::HotbarSetupView::new(app, config));
                    app.status_message =
                        Some("Hotbar setup opened from /setup Hotbar readiness.".to_string());
                }
            }
            ViewEvent::SetupOpenModeRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::ModePicker) {
                    app.view_stack
                        .push(crate::tui::views::mode_picker::ModePickerView::new(
                            app.mode,
                            app.ui_locale,
                        ));
                    app.status_message =
                        Some("Work mode picker opened from /setup runtime posture.".to_string());
                }
            }
            ViewEvent::SetupOpenConfigRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::Config) {
                    app.view_stack.push(ConfigView::new_for_app(app));
                    app.status_message =
                        Some("Config view opened from /setup runtime posture.".to_string());
                }
            }
            ViewEvent::SetupOpenRemoteControlRequested => {
                start_remote_control_session(app, config);
            }
            ViewEvent::HotbarDisableRequested => {
                disable_hotbar(app, config);
            }
            ViewEvent::SubAgentsRefresh => {
                app.status_message = Some("Refreshing sub-agents...".to_string());
                // #3802: non-blocking send — refresh op, safe to drop.
                let _ = engine_handle.try_send(Op::ListSubAgents);
            }
            ViewEvent::SidebarAgentCancel { agent_id } => {
                app.status_message = Some(format!("Cancelling {agent_id}..."));
                // #6150: the input path never awaits a full op channel. The
                // cancel is retryable; a rejected send surfaces immediately.
                if engine_handle
                    .try_send(Op::CancelSubAgent {
                        agent_id: agent_id.clone(),
                    })
                    .is_err()
                {
                    app.status_message = Some(format!("Could not cancel {agent_id}"));
                }
            }
            ViewEvent::OpenAgentTranscript { agent_id } => {
                open_agent_transcript(app, config, &agent_id);
            }
            ViewEvent::AgentDetailsClosed { agent_id } => {
                crate::tui::work_surface::agent_details_closed(app, &agent_id);
            }
            ViewEvent::FilePickerSelected { path } => {
                // Insert `@<path>` at the composer's cursor with surrounding
                // whitespace so the existing `@`-mention parser picks it up.
                let cursor = app.cursor_position;
                let needs_leading_space = cursor > 0
                    && !app
                        .input
                        .chars()
                        .nth(cursor.saturating_sub(1))
                        .is_some_and(|c| c.is_whitespace());
                let mut insertion = String::new();
                if needs_leading_space {
                    insertion.push(' ');
                }
                insertion.push('@');
                insertion.push_str(&crate::tui::file_mention::file_mention_body(&path));
                insertion.push(' ');
                app.insert_str(&insertion);
                app.status_message = Some(format!("Attached @{path}"));
            }
            ViewEvent::ModelPickerApplied {
                model,
                identity,
                effort,
                previous_model,
                previous_effort,
                save_as_startup_default,
            } => {
                apply_model_picker_choice(
                    app,
                    engine_handle,
                    config,
                    model,
                    identity,
                    effort,
                    previous_model,
                    previous_effort,
                    save_as_startup_default,
                )
                .await;
                refresh_parked_fleet_roster(app, config);
            }
            ViewEvent::ModelPickerDismissed {
                catalog_view,
                view,
                selected_row_id,
            } => {
                sync_config_provider_from_app(config, app);
                app.model_picker_memory = Some(crate::tui::app::ModelPickerMemory {
                    catalog_view,
                    view: Some(view),
                    selected_row_id,
                });
                refresh_parked_fleet_roster(app, config);
            }
            ViewEvent::ModelPickerRefresh => {
                // Re-resolve readiness from the live credential state and
                // rebuild catalog rows. Non-destructive: never clears the list
                // when a refresh fails; just re-project from current config.
                sync_config_provider_from_app(config, app);
                let refreshed =
                    tr(app.ui_locale, MessageId::ModelPickerReadinessRefreshed).into_owned();
                let notice = Some((refreshed.clone(), StatusToastLevel::Info));
                app.status_message = Some(if refresh_open_model_picker(app, config, notice) {
                    refreshed
                } else {
                    tr(app.ui_locale, MessageId::ModelPickerOpenToRefresh).into_owned()
                });
                app.needs_redraw = true;
            }
            ViewEvent::ModelPickerToggleFleet {
                provider,
                provider_id,
                model,
            } => {
                let provider_key = provider_id.unwrap_or_else(|| provider.as_str().to_string());
                toggle_model_picker_fleet(app, config, &provider_key, &model);
            }
            ViewEvent::ModelPickerTogglePin {
                provider,
                provider_id,
                model,
            } => {
                let provider_key = provider_id.unwrap_or_else(|| provider.as_str().to_string());
                toggle_model_picker_pin(app, config, &provider_key, &model);
            }
            ViewEvent::ModelPickerMovePin {
                provider,
                provider_id,
                model,
                delta,
            } => {
                let provider_key = provider_id.unwrap_or_else(|| provider.as_str().to_string());
                let reordered = crate::settings::Settings::transact_opt(|settings| {
                    if !settings.move_pinned_model(&provider_key, &model, delta) {
                        return Ok(None);
                    }
                    Ok(Some(settings.pinned_models.clone()))
                });
                match reordered {
                    Ok(None) => {}
                    Ok(Some(pinned_models)) => {
                        app.pinned_models = pinned_models;
                        let receipt =
                            tr(app.ui_locale, MessageId::ModelPickerPinOrderUpdated).into_owned();
                        app.status_message = Some(receipt.clone());
                        refresh_open_model_picker(
                            app,
                            config,
                            Some((receipt, StatusToastLevel::Success)),
                        );
                    }
                    Err(error) => {
                        let receipt = tr(app.ui_locale, MessageId::ModelPickerPinReorderFailed)
                            .replace("{error}", &error.to_string());
                        app.status_message = Some(receipt.clone());
                        refresh_open_model_picker(
                            app,
                            config,
                            Some((receipt, StatusToastLevel::Error)),
                        );
                    }
                }
                app.needs_redraw = true;
            }
            ViewEvent::ModelPickerNeedsAuth {
                identity,
                model,
                reason,
            } => {
                app.status_message = Some(reason);
                // Close the model picker if it is still open, then hand off to
                // the provider auth flow for the locked model's provider.
                while app.view_stack.top_kind() == Some(ModalKind::ModelPicker) {
                    let _ = app.view_stack.pop();
                }
                if let Some(picker) =
                    crate::tui::provider_picker::ProviderPickerView::new_for_missing_auth(
                        app.api_provider,
                        &identity,
                        config,
                        None,
                    )
                {
                    app.view_stack.push(picker);
                } else {
                    app.status_message = Some(format!(
                        "🔒 {model} needs {} credentials — open /provider to authenticate.",
                        identity.key
                    ));
                }
                app.needs_redraw = true;
            }
            ViewEvent::StatusMessage { message } => {
                app.status_message = Some(message);
                app.needs_redraw = true;
            }
            ViewEvent::TopbarRoutePickerRequested => {
                open_provider_picker(app, config, engine_handle).await;
            }
            ViewEvent::TopbarModelPickerRequested => {
                if app.view_stack.top_kind() != Some(ModalKind::ModelPicker) {
                    app.view_stack
                        .push(crate::tui::model_picker::ModelPickerView::new(app, config));
                }
            }
            ViewEvent::ProviderPickerDismissed {
                catalog_view,
                selected_provider_id,
            } => {
                let onboarding_provider_picker = app.onboarding == OnboardingState::Provider;
                // A picker preview must never become route authority. During
                // onboarding Esc is deliberately non-mutating: it returns to
                // Language without touching config or the onboarding marker.
                if !onboarding_provider_picker {
                    sync_config_provider_from_app(config, app);
                }
                app.provider_picker_memory = Some(crate::tui::app::ProviderPickerMemory {
                    catalog_view,
                    selected_provider_id,
                });
                if onboarding_provider_picker {
                    back_from_provider_onboarding(app);
                }
            }
            ViewEvent::ProviderPickerApplied { identity } => {
                let provider = identity.provider;
                let model_override = provider_picker_model_override(app, config, &identity);
                let switched =
                    switch_provider(app, engine_handle, config, identity, model_override).await;
                if switched && app.onboarding == OnboardingState::Provider {
                    complete_provider_picker_onboarding(app, provider);
                }
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ProviderPickerApiKeySubmitted {
                identity,
                api_key,
                base_url,
            } => {
                config
                    .verify_provider_identity(&identity)
                    .map_err(anyhow::Error::msg)?;
                apply_provider_picker_api_key(
                    app,
                    engine_handle,
                    config,
                    identity,
                    api_key,
                    base_url,
                )
                .await;
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ProviderPickerSetupConfirmed {
                identity,
                api_key,
                model,
                context_window,
                base_url,
            } => {
                config
                    .verify_provider_identity(&identity)
                    .map_err(anyhow::Error::msg)?;
                let provider = identity.provider;
                let completed = apply_provider_picker_setup_confirmed(
                    app,
                    engine_handle,
                    config,
                    identity,
                    api_key,
                    model,
                    context_window,
                    base_url,
                )
                .await;
                if completed && app.onboarding == OnboardingState::Provider {
                    complete_provider_picker_onboarding(app, provider);
                }
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ProviderPickerCustomProviderSubmitted {
                provider_id,
                base_url,
                model,
                api_key_env,
            } => {
                let switched = apply_provider_picker_custom_provider(
                    app,
                    engine_handle,
                    config,
                    provider_id,
                    base_url,
                    model,
                    api_key_env,
                )
                .await;
                complete_provider_picker_onboarding_if_switched(
                    app,
                    ProviderKind::Custom,
                    switched,
                );
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ProviderPickerClaudeOAuthRequested => {
                let switched =
                    run_claude_login_from_tui(terminal, app, engine_handle, config).await?;
                complete_provider_picker_onboarding_if_switched(
                    app,
                    ProviderKind::Anthropic,
                    switched,
                );
            }
            ViewEvent::ProviderPickerXaiOAuthRequested => {
                let switched =
                    run_xai_device_login_from_tui(terminal, app, engine_handle, config).await?;
                complete_provider_picker_onboarding_if_switched(app, ProviderKind::Xai, switched);
            }
            ViewEvent::ProviderPickerChatgptOAuthRequested => {
                let switched =
                    run_chatgpt_pkce_login_from_tui(terminal, app, engine_handle, config).await?;
                complete_provider_picker_onboarding_if_switched(
                    app,
                    ProviderKind::OpenaiCodex,
                    switched,
                );
            }
            ViewEvent::ProviderPickerPluginOAuthRequested { provider } => {
                run_plugin_oauth_from_tui(terminal, app, config, provider, false).await?;
            }
            ViewEvent::ProviderPickerOrcarouterOAuthRequested => {
                let switched =
                    run_orcarouter_pkce_login_from_tui(terminal, app, engine_handle, config)
                        .await?;
                complete_provider_picker_onboarding_if_switched(
                    app,
                    ProviderKind::Orcarouter,
                    switched,
                );
            }
            ViewEvent::ProviderPickerExternalConsentConfirmed {
                provider,
                consent_provider,
                source,
                path,
            } => {
                let identity = config
                    .builtin_provider_identity(provider)
                    .map_err(anyhow::Error::msg)?;
                match persist_external_credential_consent_for_at(
                    app.config_path.as_deref(),
                    config,
                    &identity,
                    consent_provider,
                    source,
                    &path,
                ) {
                    Ok(_) => {
                        let toast = app
                            .tr(MessageId::ProviderExternalGrantedToast)
                            .replace("{owner}", source.owner_label())
                            .replace("{provider}", provider.as_str());
                        app.push_status_toast(toast, StatusToastLevel::Success, Some(8_000));
                        let model_override = provider_picker_model_override(app, config, &identity);
                        let switched =
                            switch_provider(app, engine_handle, config, identity, model_override)
                                .await;
                        // #4763: reusing an external CLI grant completes provider
                        // onboarding exactly like a submitted key or an applied
                        // route. Without this the picker closes on success and
                        // the user is returned to the provider step they just
                        // satisfied — the second half of the reported loop.
                        if switched && app.onboarding == OnboardingState::Provider {
                            complete_provider_picker_onboarding(app, provider);
                        }
                        refresh_config_view_if_open(app, "provider");
                    }
                    Err(error) => app.push_status_toast(
                        app.tr(MessageId::ProviderExternalSaveFailedToast)
                            .replace("{error}", &error.to_string()),
                        StatusToastLevel::Error,
                        None,
                    ),
                }
            }
            ViewEvent::ProviderPickerExternalConsentRevoked { provider } => {
                let identity = config
                    .builtin_provider_identity(provider)
                    .map_err(anyhow::Error::msg)?;
                match revoke_external_credential_consent_for_at(
                    app.config_path.as_deref(),
                    config,
                    &identity,
                ) {
                    Ok(_) => app.push_status_toast(
                        app.tr(MessageId::ProviderExternalRevokedToast)
                            .replace("{provider}", provider.as_str()),
                        StatusToastLevel::Success,
                        Some(5_000),
                    ),
                    Err(error) => app.push_status_toast(
                        app.tr(MessageId::ProviderExternalRevokeFailedToast)
                            .replace("{error}", &error.to_string()),
                        StatusToastLevel::Error,
                        None,
                    ),
                }
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ProviderPickerOpenModels { identity } => {
                open_model_picker_for_provider(app, config, &identity);
            }
            ViewEvent::ProviderPickerTestConnection {
                identity,
                catalog_view,
            } => {
                match config.verify_provider_identity(&identity) {
                    Ok(()) => {
                        apply_provider_picker_test_connection(
                            app,
                            engine_handle,
                            config,
                            identity,
                            catalog_view,
                        )
                        .await
                    }
                    Err(error) => {
                        app.push_status_toast(error, StatusToastLevel::Error, Some(8_000))
                    }
                }
                refresh_config_view_if_open(app, "provider");
            }
            ViewEvent::ModeSelected { mode } => {
                let prior_mode = app.mode;
                let msg = commands::switch_mode(app, mode);
                if app.mode != prior_mode {
                    sync_mode_update(app, engine_handle).await;
                }
                app.add_message(HistoryCell::System { content: msg });
            }
            ViewEvent::BacktrackStep { direction } => {
                app.backtrack.step(direction);
                if let Some(idx) = app.backtrack.selected_idx() {
                    update_backtrack_overlay_selection(app, idx);
                }
            }
            // Apply the accepted choice now, for keyboard and mouse alike.
            // Parking it in pending_launch_action left keyboard confirmation
            // waiting for an unrelated mouse event to drain that queue.
            ViewEvent::LaunchResumeConfirmed { session_id } => {
                let result = resume_launch_session(app, &session_id);
                if apply_command_result(terminal, app, engine_handle, task_manager, config, result)
                    .await?
                {
                    return Ok(true);
                }
                app.needs_redraw = true;
            }
            ViewEvent::BacktrackConfirm => {
                if let Some(depth) = app.backtrack.confirm() {
                    // Reserve the slot before mutating history (#6150): the
                    // loop must not await a full op channel, and applying the
                    // backtrack without delivering SyncSession would desync
                    // the engine's messages from ours.
                    match engine_handle.tx_op.clone().try_reserve_owned() {
                        Ok(permit) => {
                            apply_backtrack(app, depth);
                            engine_handle.send_reserved_op(
                                permit,
                                Op::SyncSession {
                                    session_id: app.current_session_id.clone(),
                                    messages: app.api_messages.as_ref().clone(),
                                    system_prompt: app.system_prompt.clone(),
                                    system_prompt_override: false,
                                    model: app.model.clone(),
                                    workspace: app.workspace.clone(),
                                    mode: app.mode,
                                },
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            app.status_message = Some(
                                "Engine busy — backtrack not applied; try again in a moment"
                                    .to_string(),
                            );
                            app.needs_redraw = true;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            app.status_message =
                                Some("Engine stopped — backtrack not applied".to_string());
                            app.needs_redraw = true;
                        }
                    }
                }
            }
            ViewEvent::BacktrackCancel => {
                app.backtrack.reset();
                app.status_message = Some("Backtrack canceled".to_string());
                app.needs_redraw = true;
            }
            ViewEvent::ContextMenuSelected { action } => {
                match apply_context_menu_action(app, action) {
                    ContextMenuOutcome::Done => {}
                    ContextMenuOutcome::Events(events) => {
                        if handle_view_events_boxed(
                            terminal,
                            app,
                            config,
                            task_manager,
                            engine_handle,
                            events,
                        )
                        .await?
                        {
                            return Ok(true);
                        }
                    }
                    ContextMenuOutcome::OpenInEditor { path, line } => {
                        open_file_in_editor(terminal, app, &path, line);
                    }
                }
            }
            ViewEvent::OpenContextMenu {
                title,
                entries,
                column,
                row,
            } => {
                push_context_menu(app, entries, column, row, title);
            }
            ViewEvent::SkillMutationRequested { request } => {
                handle_skill_mutation_requested(app, request).await;
            }
            ViewEvent::SkillsManagerToggleCompatible => {
                if app.view_stack.top_kind() == Some(ModalKind::SkillsManager)
                    && let Some(mut boxed) = app.view_stack.pop()
                {
                    if let Some(view) = boxed
                        .as_any_mut()
                        .downcast_mut::<crate::tui::views::skills_manager::SkillsManagerView>(
                    ) {
                        crate::tui::views::skills_manager::apply_toggle_compatible(view, app);
                    }
                    app.view_stack.push_boxed(boxed);
                }
            }
        }
    }

    Ok(false)
}

/// Keep the very large modal-event dispatcher out of the already-large TUI
/// loop future. Config previews take a dedicated small path: polling the full
/// dispatcher on top of the event loop exceeds the macOS main-thread stack in
/// debug builds before a theme preview can reach its next frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_view_events_boxed<'a>(
    terminal: &'a mut AppTerminal,
    app: &'a mut App,
    config: &'a mut Config,
    task_manager: &'a SharedTaskManager,
    engine_handle: &'a mut EngineHandle,
    events: Vec<ViewEvent>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + 'a>> {
    Box::pin(async move {
        for event in events {
            match event {
                ViewEvent::ConfigUpdated {
                    key,
                    value,
                    persist,
                } => {
                    if handle_config_updated(
                        terminal,
                        app,
                        config,
                        task_manager,
                        engine_handle,
                        key,
                        value,
                        persist,
                    )
                    .await?
                    {
                        return Ok(true);
                    }
                }
                ViewEvent::ThemeSelectionUpdated { theme, persist } => {
                    if handle_theme_selection_updated(
                        terminal,
                        app,
                        config,
                        task_manager,
                        engine_handle,
                        theme,
                        persist,
                    )
                    .await?
                    {
                        return Ok(true);
                    }
                }
                other => {
                    if Box::pin(handle_view_events(
                        terminal,
                        app,
                        config,
                        task_manager,
                        engine_handle,
                        vec![other],
                    ))
                    .await?
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    })
}

/// `/agents` → an agent, or "Go to agent" on its card. One agent, one
/// destination: focus the worker so its full transcript owns the main area
/// and the composer addresses its fork; the register modal closes so the
/// focus is visible. A hidden approval card for this agent comes back on top
/// of its transcript so the person can answer it (approvals C1).
pub(super) fn open_agent_transcript(app: &mut App, config: &Config, agent_id: &str) {
    if app.view_stack.top_kind() == Some(ModalKind::SubAgents) {
        app.view_stack.pop();
    }
    crate::tui::agent_focus::focus_agent(app, agent_id);
    crate::tui::pending_requests::repush_for_agent(
        app,
        agent_id,
        config.approval_default_selection(),
        config.approval_timeout(),
    );
    app.needs_redraw = true;
}
