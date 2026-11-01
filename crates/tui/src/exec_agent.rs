//! Non-interactive exec agent assembly: the `run_exec_agent` pipeline
//! that resolves the CLI route, builds the engine configuration, spawns
//! the engine, and drives the exec output stream to completion.
//!
//! Extracted verbatim from `lib.rs` (#5586, the issue's prescribed
//! engine-config-assembly cut). The two functions were crate-private in
//! the root and are `pub(crate)` here purely so the root's glob re-export
//! keeps the dispatch site and tests resolving unchanged.

use super::*;
use codewhale_engine::core::ops::TurnSpec;
use codewhale_engine::exec_events::ExecAgentEvents;

/// Resolve the headless `exec` model-step ceiling.
///
/// Omission leaves model steps uncapped. Clap rejects `--max-turns 0`;
/// explicit positive values retain the documented finite range.
pub(crate) fn exec_max_steps(max_turns: Option<u32>) -> u32 {
    codewhale_runtime::turn_budget::resolve_max_model_steps(max_turns)
}

/// Model-step ceiling for a plain (zero-tool) `exec` run without
/// `--max-turns`. Its only extra steps are output-limit continuations, which
/// have no progress signal of their own; without this a model stuck at the
/// output limit would be re-asked, with growing history, until the turn wall
/// clock (#6510 review).
pub(crate) const ONE_SHOT_DEFAULT_MAX_STEPS: u32 = 8;

/// Default-denied tools for headless `exec`, on top of the operator's own
/// `--disallowed-tools` flag.
///
/// A headless run has no responder for `request_user_input`, so offering the
/// tool can only stall the run until the turn wall clock, or forever with
/// `[tools] user_input_timeout_seconds = 0`. Withholding it is the default
/// form of the operator workaround (`--disallowed-tools request_user_input`):
/// the model reports the tool absent and finishes instead of parking. This
/// stays unconditional: there is no channel on which a one-shot CLI run
/// could answer, so advertising the tool cannot work.
pub(crate) fn exec_disallowed_tools(disallowed_tools: Option<Vec<String>>) -> Option<Vec<String>> {
    use codewhale_tools::names::REQUEST_USER_INPUT_NAME;
    let mut disallowed = disallowed_tools.unwrap_or_default();
    if !disallowed
        .iter()
        .any(|tool| tool.as_str() == REQUEST_USER_INPUT_NAME)
    {
        disallowed.push(REQUEST_USER_INPUT_NAME.to_string());
    }
    Some(disallowed)
}

/// Attach the durable automation store headless `exec` inspects.
///
/// Headless exec builds its catalog from the same tool surface the TUI and the
/// Runtime host do, so it advertises `automation` and `send_later` whether or
/// not the store behind them is attached. Left unattached, every call failed
/// "AutomationManager is not attached" — the tool was real and the service was
/// missing. This opens the same store those two hosts open: a shared directory
/// guarded per transaction by its own file locks (`AutomationManager::open`),
/// so it adds no second store, no scheduler, and no second scheduling
/// authority.
///
/// What exec deliberately does not take is the Runtime's task-execution lease.
/// That lease is exclusive (`TaskExecutionLease::new`) and a one-shot host must
/// neither contend with it nor recover work from the process that holds it. So
/// the manager returned here is *unbound*: inspection works, and everything
/// that would promise dispatch is refused by the tool's own admission check
/// (`tools::automation::require_dispatch_owner`) rather than persisting a
/// schedule nothing would honor.
///
/// Fleet worker subprocesses get nothing, keeping the narrowed envelope they
/// were launched with alongside the empty plugin registry and disabled
/// subagents. A store that cannot be opened is reported, never swallowed:
/// both other hosts fail startup on it, so exec does too instead of
/// advertising an automation surface it silently cannot serve.
pub(crate) fn exec_automation_services(
    fleet_authority_active: bool,
) -> Result<Option<codewhale_engine::automation_manager::SharedAutomationManager>> {
    if fleet_authority_active {
        return Ok(None);
    }
    let service = codewhale_engine::automation_manager::AutomationManager::default_location()
        .context("open the automation store for headless exec")?;
    Ok(Some(std::sync::Arc::new(tokio::sync::Mutex::new(service))))
}

/// Printed to stderr when a tool-less one-shot `exec` answer contained
/// tool-call markup that the engine stripped from the visible output.
pub(crate) const ONE_SHOT_TOOL_CALL_NOTICE: &str = "codewhale exec: the model tried to call a tool, but this run offers none, so the tool call was removed from the answer. Re-run with --auto (or --allowed-tools) to let it use tools.";

async fn persist_exec_checkpoint(
    request: Arc<codewhale_engine::core::events::SessionCheckpointRequest>,
    manager: Option<Arc<SessionManager>>,
    session_id: &str,
    workspace: &Path,
    turn_id: Option<&str>,
    engine: &codewhale_engine::core::engine::EngineHandle,
    deadline: Instant,
) {
    use codewhale_engine::core::events::{
        SessionCheckpointProjection, SessionCheckpointReceipt, SessionCheckpointRequest,
    };

    if turn_id != Some(request.caller.turn_id.as_str()) {
        request.fail("exec session checkpoint does not match its active turn");
        return;
    }
    if let Err(error) = request.validate_prefix(session_id, workspace, &request.snapshot.messages) {
        request.fail(error);
        return;
    }
    let turn_cancel = engine.captured_turn_cancel();
    if turn_cancel.is_cancelled() {
        request.fail("exec session checkpoint cancelled before persistence");
        return;
    }
    if Instant::now() >= deadline {
        request.fail("exec session checkpoint deadline expired before persistence");
        engine.cancel_with_reason(codewhale_engine::core::engine::CancelReason::External);
        return;
    }
    let Some(manager) = manager else {
        request.no_backend();
        return;
    };
    let write_request = Arc::clone(&request);
    let mut writer = tokio::task::spawn_blocking(move || -> Result<SessionCheckpointReceipt> {
        let snapshot = &write_request.snapshot;
        let id = persist_exec_session(
            &snapshot.messages,
            &snapshot.model,
            PersistedProviderRoute {
                kind: &snapshot.model_provider,
                id: snapshot.model_provider_id.as_deref(),
            },
            &snapshot.workspace,
            &snapshot.system_prompt,
            Some(&snapshot.session_id),
            snapshot.total_tokens,
            None,
            manager.as_ref(),
            &[],
            Some(write_request.as_ref()),
        )?;
        let persisted = manager
            .load_session_snapshot(&id)
            .context("could not read back exec session checkpoint")?;
        write_request
            .validate_prefix(
                &persisted.metadata.id,
                &persisted.metadata.workspace,
                &persisted.messages,
            )
            .map_err(anyhow::Error::msg)?;
        Ok(SessionCheckpointReceipt {
            persisted_sha256: SessionCheckpointRequest::prefix_hash(&persisted.messages)
                .map_err(anyhow::Error::msg)?,
            messages_len: persisted.messages.len(),
            projection: SessionCheckpointProjection::Exact,
        })
    });
    let result = tokio::select! {
        biased;
        () = request.withdraw.cancelled() => {
            request.fail("exec session checkpoint was withdrawn; its write may have completed");
            writer.await
        }
        () = turn_cancel.cancelled() => {
            request.fail("exec session checkpoint was cancelled; its write may have completed");
            request.withdraw.cancel();
            writer.await
        }
        () = tokio::time::sleep_until(deadline.into()) => {
            request.fail("exec session checkpoint deadline expired; its write may have completed");
            request.withdraw.cancel();
            engine.cancel_with_reason(codewhale_engine::core::engine::CancelReason::External);
            writer.await
        }
        result = &mut writer => result,
    };
    match result {
        Ok(Ok(receipt)) => request.succeed(receipt),
        Ok(Err(error)) => request.fail(format!("exec session checkpoint failed: {error:#}")),
        Err(error) => request.fail(format!("exec session checkpoint writer failed: {error}")),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_exec_agent(
    config: &Config,
    model: &str,
    prompt: &str,
    workspace: PathBuf,
    max_subagents: usize,
    auto_approve: bool,
    allow_sandbox_elevation: bool,
    explicit_sandbox: Option<&str>,
    json_output: bool,
    resume_session: Option<codewhale_engine::session_manager::SavedSession>,
    force_configured_route: bool,
    output_format: ExecOutputFormat,
    max_turns: u32,
    max_tool_calls: Option<u32>,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Option<Vec<String>>,
    append_system_prompt: Option<String>,
    tool_authority_json: Option<String>,
    exec_hooks_enabled: bool,
    plugin_registry: std::sync::Arc<codewhale_plugins::PluginRegistry>,
    // #6510: plain `exec` (no tool-surface flag). The caller passes an empty
    // allowlist; this also skips workspace snapshots, LSP and the automation
    // store, and writes the one-shot `--json` receipt shape.
    one_shot: bool,
) -> Result<()> {
    use codewhale_config::AppMode;
    use codewhale_engine::compaction::CompactionConfig;
    use codewhale_engine::core::engine::{EngineConfig, spawn_engine};
    use codewhale_engine::core::events::Event;
    use codewhale_engine::core::ops::Op;
    use codewhale_engine::tools::plan::new_shared_plan_state;
    use codewhale_engine::tools::todo::new_shared_todo_list;
    use codewhale_execpolicy::ApprovalMode;

    let _timing = codewhale_runtime::logging::PhaseTimer::start("exec_total");
    let startup_timing = codewhale_runtime::logging::PhaseTimer::start("exec_startup");
    ignore_sigpipe_for_headless_exec();

    // Withhold `request_user_input`; a headless run has no responder.
    let disallowed_tools = exec_disallowed_tools(disallowed_tools);

    // Headless exec registers the model-facing notify tool too. Project the
    // final merged config before tool setup so `off`, quiet/category gates,
    // and explicit `always` are truthful outside the interactive TUI. With no
    // focus-reporting channel, fail closed to focused; only explicit `always`
    // may authorize a headless desktop notification.
    let terminal = codewhale_runtime::host_terminal::host();
    terminal.set_terminal_focused(true);
    terminal.apply_notification_settings(&config.notifications_config());

    validate_exec_tool_authority_resume(tool_authority_json.as_deref(), resume_session.is_some())?;
    let fleet_authority = tool_authority_json
        .as_deref()
        .map(codewhale_engine::tools::spec::ToolAuthorityEnvelope::from_json)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let fleet_authority_active = fleet_authority.is_some();
    let outer_network_access = fleet_authority
        .as_ref()
        .and_then(|authority| authority.network_access);
    let outer_shell_authority = fleet_authority
        .as_ref()
        .map(|authority| authority.shell)
        .unwrap_or_default();
    if let Some(envelope) = fleet_authority {
        codewhale_engine::tools::spec::install_process_tool_authority(envelope)
            .map_err(anyhow::Error::msg)?;
    }

    let fleet_capture = match (
        std::env::var("CODEWHALE_FLEET_CAPTURE_ID").ok(),
        std::env::var_os("CODEWHALE_FLEET_CAPTURE_DIR"),
    ) {
        (Some(id), Some(dir)) => {
            uuid::Uuid::parse_str(&id).context("invalid Fleet session capture id")?;
            anyhow::ensure!(
                resume_session.is_none(),
                "Fleet capture cannot resume a session"
            );
            let manager = SessionManager::new(PathBuf::from(dir))?;
            match manager.load_session(&id) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                _ => anyhow::bail!("Fleet session capture id is already in use or unavailable"),
            }
            Some((id, manager))
        }
        (None, None) => None,
        _ => anyhow::bail!("incomplete Fleet session capture destination"),
    };

    let exec_session_id = fleet_capture
        .as_ref()
        .map(|(id, _)| id.clone())
        .or_else(|| {
            resume_session
                .as_ref()
                .map(|saved| saved.metadata.id.clone())
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let exec_session_workspace = resume_session
        .as_ref()
        .map(|saved| saved.metadata.workspace.clone())
        .unwrap_or_else(|| workspace.clone());
    let mut should_persist_session = !one_shot
        || resume_session.is_some()
        || fleet_capture.is_some()
        || output_format == ExecOutputFormat::StreamJson;
    let mut exec_session_manager = fleet_capture.map(|(_, manager)| Arc::new(manager));
    let mut open_exec_session_manager = || -> Result<Arc<SessionManager>> {
        if exec_session_manager.is_none() {
            exec_session_manager = Some(Arc::new(
                SessionManager::default_location().context("could not open exec session store")?,
            ));
        }
        Ok(Arc::clone(
            exec_session_manager
                .as_ref()
                .expect("opened exec session store"),
        ))
    };
    if should_persist_session {
        open_exec_session_manager()?;
    }

    let route = resolve_cli_exec_route(config, model, prompt, force_configured_route).await?;
    let execution_config = config_for_cli_route(config, &route)?;
    codewhale_engine::provider_lake::refresh_account_catalog_before_first_request(
        &execution_config,
    )
    .await;
    let auto_model = route.auto_model;
    let effective_identity = execution_config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    let effective_provider = effective_identity.provider;
    let effective_model = route.model;
    let validated_route = codewhale_engine::route_runtime::resolve_runtime_route_for_identity(
        &execution_config,
        &effective_identity,
        Some(&effective_model),
    )
    .map_err(anyhow::Error::msg)?
    .validate_for(codewhale_engine::route_runtime::RouteErrorSurface::Headless)
    .map_err(anyhow::Error::msg)?;
    let effective_identity = validated_route.identity.clone();
    let effective_provider_name = effective_identity.key.to_string();
    let (effective_provider_kind, effective_stream_provider_id) =
        exec_stream_provider_route(&validated_route.identity);
    let route_source = if auto_model {
        "auto_resolver"
    } else {
        "explicit_or_configured"
    }
    .to_string();
    let exec_started = Instant::now();
    let prompt_sha256 = format!(
        "sha256:{}",
        codewhale_runtime::hashing::sha256_hex(prompt.as_bytes())
    );
    let mut binary_identity =
        (output_format == ExecOutputFormat::StreamJson).then(start_binary_sha256);
    let approval_posture = if auto_approve { "auto_tools" } else { "ask" }.to_string();
    // The Full Access posture travels in the op's auto_approve/approval_mode
    // fields; modes no longer carry permission.
    let mode = AppMode::Agent;
    let turn_approval_mode = if auto_approve {
        ApprovalMode::Bypass
    } else {
        execution_config
            .approval_policy
            .as_deref()
            .and_then(ApprovalMode::from_config_value)
            .unwrap_or_default()
    };
    let turn_sandbox_policy = codewhale_engine::core::authority::sandbox_policy_for_turn(
        mode,
        turn_approval_mode,
        execution_config.sandbox_mode.as_deref(),
        &workspace,
        codewhale_engine::core::authority::SandboxNetworkAccess::from_config(
            execution_config.sandbox_network_access,
        ),
    );
    let trust_mode = auto_approve
        && matches!(
            turn_sandbox_policy,
            codewhale_sandbox::SandboxPolicy::DangerFullAccess
        );
    let active_route_limits =
        codewhale_engine::route_budget::known_route_limits(validated_route.candidate.limits());
    let effective_context_window = validated_route.context_window;
    let max_subagents = if max_subagents
        == config.max_subagents_for_provider(
            &config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?,
        ) {
        execution_config
            .max_subagents_for_provider(&effective_identity)
            .clamp(1, MAX_SUBAGENTS)
    } else {
        max_subagents
    };
    // A FIXED model with `--reasoning-effort auto` (the exact shape a Fleet
    // worker subprocess launches with: `--model <exact> --reasoning-effort
    // auto`) is still Auto. `auto_model` is a *model* decision and is false
    // here, so deriving the auto flag from it left this path both raw and
    // non-auto: the literal string `"auto"` travelled to the engine while the
    // receipt claimed no Auto was in play.
    let reasoning_effort_auto = route.auto_controls_reasoning;
    // Resolve Auto against this run's prompt at the CLI boundary, exactly like
    // the interactive launch path does, so the tier the engine (and the
    // receipt below) sees is concrete.
    let effective_reasoning_effort = route.reasoning_effort.and_then(|effort| {
        cli_reasoning_effort_value_for_prompt(&execution_config, &effective_model, effort)
    });

    let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
    let auto_compact_enabled =
        if codewhale_runtime::settings::Settings::auto_compact_explicitly_configured() {
            settings.auto_compact
        } else {
            codewhale_engine::route_budget::auto_compact_default_for_route(
                effective_provider,
                &effective_model,
                active_route_limits,
            )
        };
    let compaction = CompactionConfig {
        enabled: auto_compact_enabled,
        model: effective_model.clone(),
        effective_context_window: Some(validated_route.context_window.tokens),
        token_threshold:
            codewhale_engine::route_budget::compaction_threshold_for_route_at_percent_with_window(
                effective_provider,
                &effective_model,
                active_route_limits,
                Some(validated_route.context_window.tokens),
                settings.auto_compact_threshold_percent,
            ),
        summary_instructions: execution_config.compaction_summary_instructions(),
        retained_user_message_tokens: execution_config.compaction_retained_user_message_tokens(),
        ..Default::default()
    };

    let network_policy = exec_network_policy(&execution_config, outer_network_access);

    let lsp_config = (!fleet_authority_active && !one_shot)
        .then(|| {
            execution_config
                .lsp
                .clone()
                .map(codewhale_lsp::LspConfig::from)
        })
        .flatten();
    let mut engine_features = execution_config.features();
    apply_fleet_engine_feature_caps(
        &mut engine_features,
        fleet_authority_active,
        outer_network_access,
        outer_shell_authority,
    );
    if codewhale_engine::core::engine::tool_catalog::allowlist_is_native_file_and_shell_only(
        allowed_tools.as_deref(),
    ) {
        engine_features.disable(codewhale_runtime::features::Feature::Mcp);
    }
    let engine_plugin_registry = if fleet_authority_active {
        std::sync::Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace))
    } else {
        plugin_registry
    };
    // `exec --hooks` (#6099) is the operator's explicit opt-in: headless runs
    // fire no hooks by default. When armed, the executor is the same one the
    // TUI builds — global config, reviewed plugin snapshots, then trusted
    // project `.codewhale/hooks.toml` — so `tool_call_before` can still deny
    // and `shell_env` still applies. It is shared with the engine config, the
    // turn's SendMessage op (which re-installs it into the engine), and the
    // tool runtime services. Fleet workers never opt in: the narrowed
    // authority envelope does not carry the operator's hook set into a child.
    let exec_hook_executor = (exec_hooks_enabled && !fleet_authority_active).then(|| {
        let hooks_config = codewhale_engine::hooks::config::load_with_project_and_plugins(
            execution_config.hooks_config(),
            &workspace,
            Some(engine_plugin_registry.as_ref()),
        );
        std::sync::Arc::new(codewhale_engine::hooks::HookExecutor::new(
            hooks_config,
            workspace.clone(),
        ))
    });
    let exec_allow_shell = codewhale_engine::tools::spec::fleet_exec_shell_enabled(
        fleet_authority_active,
        outer_shell_authority,
        disallowed_tools.as_deref(),
    ) || (!fleet_authority_active
        && (auto_approve || execution_config.allow_shell()));
    let persist_services_enabled = cfg!(unix)
        && !fleet_authority_active
        && exec_allow_shell
        && explicit_sandbox
            .is_some_and(|sandbox| sandbox.eq_ignore_ascii_case("danger-full-access"));
    let exec_shell_manager =
        codewhale_engine::tools::shell::new_shared_shell_manager(workspace.clone());
    let exec_automations = exec_automation_services(fleet_authority_active || one_shot)?;
    let runtime_services = codewhale_engine::tools::spec::RuntimeToolServices {
        shell_manager: Some(exec_shell_manager.clone()),
        persist_services_enabled,
        automations: exec_automations,
        media_originals_dir: codewhale_runtime::media_originals::default_store_dir(),
        hook_executor: exec_hook_executor.clone(),
        ..codewhale_engine::tools::spec::RuntimeToolServices::default()
    };

    let engine_config = EngineConfig {
        model: effective_model.clone(),
        active_route_limits,
        workspace: workspace.clone(),
        session_id: Some(exec_session_id.clone()),
        session_checkpoint_backend: Some(
            codewhale_engine::core::events::SessionCheckpointProjection::Exact,
        ),
        subagent_state_root: None,
        plugin_registry: Some(std::sync::Arc::clone(&engine_plugin_registry)),
        allow_shell: exec_allow_shell,
        trust_mode,
        notes_path: execution_config.notes_path(),
        mcp_config_path: execution_config.mcp_config_path(),
        mcp_auth_context: Default::default(),
        // Non-interactive exec has no user-level MCP OAuth callback
        // overrides; the loopback default applies.
        mcp_oauth_callback_port: None,
        mcp_oauth_callback_url: None,
        skills_dir: execution_config.skills_dir(),
        skills_config: std::sync::Arc::new(execution_config.skills_config()),
        instructions: {
            let mut instrs: Vec<codewhale_engine::prompts::InstructionSource> = execution_config
                .instructions_paths()
                .into_iter()
                .map(Into::into)
                .collect();
            if let Some(ref extra) = append_system_prompt {
                instrs.push(codewhale_engine::prompts::InstructionSource::Inline {
                    name: "cli:append-system-prompt".into(),
                    content: extra.clone(),
                });
            }
            instrs
        },
        project_context_pack_enabled: execution_config.project_context_pack_enabled(),
        translation_enabled: false,
        max_steps: max_turns,
        max_subagents,
        max_admitted_subagents: execution_config
            .max_admitted_subagents_for_provider(&effective_identity)
            .max(max_subagents),
        launch_concurrency: execution_config.launch_concurrency_for_provider(&effective_identity),
        subagents_enabled: !fleet_authority_active
            && execution_config.subagents_enabled_for_provider(&effective_identity),
        features: engine_features,
        auto_review_policy:
            codewhale_engine::core::authority::auto_review::AutoReviewPolicy::from_config(
                &execution_config,
            ),
        compaction: compaction.clone(),
        todos: new_shared_todo_list(),
        plan_state: new_shared_plan_state(),
        goal_state: codewhale_engine::tools::goal::new_shared_goal_state(),
        max_spawn_depth: if fleet_authority_active {
            0
        } else {
            execution_config.subagent_max_spawn_depth_for_provider(&effective_identity)
        },
        network_policy,
        snapshots_enabled: !fleet_authority_active
            && !one_shot
            && execution_config.snapshots_config().enabled,
        snapshots_max_workspace_bytes: execution_config
            .snapshots_config()
            .max_workspace_gb
            .saturating_mul(1024 * 1024 * 1024),
        // No host here records snapshot receipts.
        record_restore_points: false,
        lsp_config,
        runtime_services,
        subagent_model_overrides: execution_config.subagent_model_overrides(),
        fleet_roster: std::sync::Arc::new(
            codewhale_engine::fleet::identity::load_effective_roster(
                &execution_config.fleet_config(),
                &workspace,
                Some(engine_plugin_registry.as_ref()),
            ),
        ),
        subagent_api_timeout: std::time::Duration::from_secs(
            execution_config.subagent_api_timeout_secs_for_provider(&effective_identity),
        ),
        stream_chunk_timeout: std::time::Duration::from_secs(
            execution_config.stream_chunk_timeout_secs(),
        ),
        turn_wall_clock: execution_config.turn_wall_clock(),
        stream_max_content_bytes: execution_config.stream_max_content_bytes(),
        stream_max_duration: execution_config.stream_max_duration(),
        stream_retry_limits: execution_config.stream_retry_limits(),
        stream_open_timeout: execution_config.stream_open_timeout(),
        subagent_heartbeat_timeout: std::time::Duration::from_secs(
            execution_config.subagent_heartbeat_timeout_secs_for_provider(&effective_identity),
        ),
        prefer_bwrap: execution_config.prefers_bwrap(),
        bwrap_extensions: codewhale_sandbox::BwrapMountExtensions {
            read_only_roots: execution_config.bwrap_ro_roots.clone(),
            device_roots: execution_config.bwrap_dev_roots.clone(),
        },
        read_denylist: codewhale_sandbox::read_guard::ReadDenylist::from_config(&execution_config),
        memory_enabled: execution_config.memory_enabled(),
        memory_path: execution_config.memory_path(),
        speech_output_dir: execution_config.speech_output_dir(),
        vision_config: execution_config.vision_model_config(),
        strict_tool_mode: execution_config.strict_tool_mode.unwrap_or(false),
        goal_objective: None,
        goal_token_budget: None,
        goal_status: codewhale_engine::tools::goal::GoalStatus::Active,
        goal_max_continuations: execution_config.goal_max_continuations(),
        goal_continuation_delay_seconds: execution_config.goal_continuation_delay_seconds(),
        goal_enforce_token_budget: execution_config.goal_enforce_token_budget(),
        reasoning_only_max_reprompts: execution_config.reasoning_only_max_reprompts(),
        reasoning_only_on_output_limit: execution_config.reasoning_only_on_output_limit(),
        reasoning_only_reprompt_message: Some(
            execution_config
                .reasoning_only_reprompt_message()
                .to_string(),
        ),
        allowed_tools: allowed_tools.clone(),
        disallowed_tools: disallowed_tools.clone(),
        max_tool_calls,
        hook_executor: exec_hook_executor.clone(),
        locale_tag: codewhale_localization::resolve_locale(&settings.locale)
            .tag()
            .to_string(),
        workshop: {
            codewhale_runtime::active_workshop::install_active_workshop(config.workshop.as_ref());
            config.workshop.clone()
        },
        search_provider: execution_config.search_provider(),
        search_api_key: execution_config
            .search
            .as_ref()
            .and_then(|s| s.api_key.clone()),
        search_native: execution_config.search_native(),
        search_base_url: execution_config
            .search
            .as_ref()
            .and_then(|s| s.base_url.clone()),
        tools_always_load: if fleet_authority_active {
            std::collections::HashSet::new()
        } else {
            execution_config.tools_always_load()
        },
        user_input_limits: codewhale_engine::tools::user_input::UserInputLimits::from_config(
            &execution_config,
        ),
        user_input_timeout: execution_config.user_input_timeout(),
        user_input_responder: false,
        goal_max_steps: None,
        tools: if fleet_authority_active {
            None
        } else {
            execution_config.tools.clone()
        },
        verbosity: execution_config.verbosity.clone(),
        workspace_follow_symlinks: settings.workspace_follow_symlinks,
        exec_policy_engine: execution_config.exec_policy_engine.clone(),
        terminal_chrome_enabled: false,
        advisor_config: execution_config
            .advisor
            .as_ref()
            .map(codewhale_engine::tools::subagent::AdvisorConfig::from_toml)
            .unwrap_or_else(codewhale_engine::tools::subagent::AdvisorConfig::disabled),
    };

    let engine_handle = spawn_engine(engine_config, &execution_config, None);
    // A restricted posture that resolves to no enforcing sandbox must reach
    // the person running exec, not only the model's posture line: policy-only
    // is accepted (read-only already fails closed in the shell tool), but it
    // is never silent. Uses the enforcement pinned by `Engine::new`.
    let sandbox_enforcement = engine_handle.sandbox_enforcement;
    let sandbox_posture = turn_sandbox_policy.posture_label_with_enforcement(sandbox_enforcement);
    if sandbox_enforcement == codewhale_sandbox::policy::SandboxEnforcement::Unavailable
        && turn_sandbox_policy.should_sandbox()
    {
        eprintln!("warning: {sandbox_posture} — shell commands will run unrestricted");
    }

    let mut loaded_session_id = None;
    if let Some(saved) = resume_session {
        let saved_id = saved.metadata.id.clone();
        let saved_workspace = tokio::fs::canonicalize(&saved.metadata.workspace)
            .await
            .unwrap_or_else(|_| saved.metadata.workspace.clone());
        let launch_workspace = tokio::fs::canonicalize(&workspace)
            .await
            .unwrap_or_else(|_| workspace.clone());
        if !paths_equal_for_config(&saved_workspace, &launch_workspace)
            && output_format == ExecOutputFormat::Text
        {
            eprintln!(
                "Warning: session {} was created in a different workspace ({}). Resuming anyway.",
                truncate_id(&saved_id),
                saved.metadata.workspace.display(),
            );
        }

        engine_handle
            .send(Op::SyncSession {
                session_id: Some(saved_id.clone()),
                messages: saved.messages,
                system_prompt: saved.system_prompt.map(SystemPrompt::Text),
                system_prompt_override: false,
                model: saved.metadata.model,
                workspace: saved.metadata.workspace,
                mode,
            })
            .await?;
        loaded_session_id = Some(saved_id.clone());
        if output_format == ExecOutputFormat::Text && !json_output {
            eprintln!("{}", exec_resumed_session_line(&saved_id));
        }
    }

    // Lifecycle outbox (`[lifecycle_outbox]`): headless `codewhale exec`
    // gets the same turn boundaries as the interactive TUI. Disabled
    // (all emits no-op) when the config has no path.
    let lifecycle_outbox = config
        .lifecycle_outbox
        .as_ref()
        .map(|outbox| {
            codewhale_hooks::LifecycleOutbox::new(
                outbox.path.clone(),
                outbox.webhook_url.clone(),
                outbox.webhook_token.clone(),
            )
        })
        .unwrap_or_else(codewhale_hooks::LifecycleOutbox::disabled);
    // Wall clock for the outbox `turn_end` duration starts at the same
    // `Op::SendMessage` boundary where `turn_start` is emitted below.
    drop(startup_timing);
    let exec_turn_started_at = Instant::now();

    engine_handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: prompt.to_string(),
            images: Vec::new(),
            mode,
            route: Box::new(validated_route.into_resolved()),
            compaction: Box::new(compaction.clone()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: codewhale_engine::tools::goal::GoalStatus::Active,
            allowed_tools: allowed_tools.clone(),
            dynamic_tools: Vec::new(),
            hook_executor: exec_hook_executor.clone(),
            reasoning_effort: effective_reasoning_effort,
            reasoning_effort_auto,
            auto_model,
            allow_shell: auto_approve || execution_config.allow_shell(),
            trust_mode,
            auto_approve,
            translation_enabled: false,
            approval_mode: turn_approval_mode,
            verbosity: execution_config.verbosity.clone(),
            provenance: codewhale_engine::core::ops::UserInputProvenance::ExternalUser,
            // Headless exec does not correlate submissions.
            submission_id: None,
        }))
        .await?;

    // Lifecycle outbox: the headless submission boundary, before the Engine
    // reports TurnStarted. No-op when the feature is disabled.
    lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
        event: "turn_start".to_string(),
        kind: "turn.started".to_string(),
        thread_id: loaded_session_id.clone().unwrap_or_default(),
        turn_id: None,
        item_id: None,
        payload: serde_json::json!({
            "model": codewhale_hooks::bounded_text(
                &effective_model,
                codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
            ),
            "workspace": workspace.display().to_string(),
        }),
    });

    let mut summary = ExecSummary {
        mode: if one_shot { "one-shot" } else { "agent" }.to_string(),
        provider: effective_provider_name.clone(),
        model: effective_model.clone(),
        prompt: prompt.to_string(),
        ..ExecSummary::default()
    };
    let can_elevate_sandbox =
        exec_sandbox_elevation_authorized(allow_sandbox_elevation, explicit_sandbox);
    let mut sandbox_denied = false;
    let mut approval_required = false;
    let mut tool_error_seen = false;
    let mut last_error_category = None;
    let mut reported_sandbox_contract = false;

    let mut active_turn_id = None;
    let mut image_request_receipts = Vec::new();
    let mut latest_session_id = loaded_session_id;
    let mut latest_messages: Arc<Vec<Message>> = Arc::new(Vec::new());
    let mut latest_system_prompt: Option<SystemPrompt> = None;
    let mut latest_model = effective_model.clone();
    let mut latest_workspace = workspace.clone();
    let mut tool_starts: HashMap<String, (Instant, String)> = HashMap::new();
    let mut turn_usage_seq: u32 = 0;
    // None means no actual terminal request snapshot was observed. A known
    // zero must remain distinguishable from that missing receipt.
    let mut observed_retry_count = None;
    let mut settled_usage: Option<codewhale_models::Usage> = None;

    let mut ends_with_newline = false;
    // One absolute host deadline includes every autonomous child fan-in turn;
    // child-specific shorter deadlines remain enforced by their runtime.
    // The default wall clock is unbounded (`Duration::MAX`); a century
    // stands in for "never" without overflowing `Instant`.
    let exec_deadline = exec_turn_started_at
        .checked_add(execution_config.turn_wall_clock())
        .unwrap_or_else(|| exec_turn_started_at + Duration::from_secs(100 * 365 * 86_400));
    let mut events = ExecAgentEvents::new(engine_handle.clone(), exec_deadline);
    loop {
        let Some(event) = events.next().await else {
            break;
        };

        match event {
            Event::TurnStarted { turn_id, .. } => {
                active_turn_id = Some(turn_id);
            }
            Event::MessageDelta { content, .. } => {
                summary.output.push_str(&content);
                if output_format == ExecOutputFormat::StreamJson {
                    emit_exec_stream_event(&ExecStreamEvent::Content { content })?;
                } else if !json_output {
                    write_exec_stdout(&content)?;
                }
                ends_with_newline = summary.output.ends_with('\n');
            }
            Event::MessageComplete { .. }
                if output_format == ExecOutputFormat::Text
                    && !json_output
                    && !ends_with_newline =>
            {
                write_exec_stdout("\n")?;
            }
            Event::ThinkingDelta { .. } => {
                // Exec stream-json intentionally omits reasoning deltas; the
                // TUI transcript retains its existing Activity Detail surface.
            }
            Event::ToolProjectionWarning {
                provider,
                omitted_tool_names,
                omitted_tool_count,
            } if !json_output => {
                eprintln!(
                    "{}",
                    codewhale_engine::core::events::tool_projection_warning_message(
                        &provider,
                        &omitted_tool_names,
                        omitted_tool_count,
                    )
                );
            }
            Event::RequestImageWindow {
                provider,
                model,
                total,
                elided,
                cap,
                cap_source,
                message,
            } => {
                if output_format == ExecOutputFormat::StreamJson {
                    emit_exec_stream_event(&ExecStreamEvent::RequestImageWindow {
                        provider,
                        model,
                        total,
                        elided,
                        cap,
                        cap_source,
                        message,
                    })?;
                } else if !json_output {
                    eprintln!("{message}");
                }
            }
            Event::ToolCallStarted {
                id, name, input, ..
            } => {
                let started_at = chrono::Utc::now().to_rfc3339();
                tool_starts.insert(id.clone(), (Instant::now(), started_at.clone()));
                if output_format == ExecOutputFormat::StreamJson {
                    emit_exec_stream_event(&ExecStreamEvent::ToolUse {
                        name,
                        id,
                        input,
                        started_at,
                    })?;
                } else if !json_output {
                    let summary = summarize_tool_args(&input);
                    if let Some(summary) = summary {
                        eprintln!("tool: {name} ({summary})");
                    } else {
                        eprintln!("tool: {name}");
                    }
                }
            }
            Event::ToolCallComplete {
                id, name, result, ..
            } => {
                let (duration_ms, started_at) = tool_starts
                    .remove(&id)
                    .map(|(started, timestamp)| {
                        (
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                            timestamp,
                        )
                    })
                    .unwrap_or_else(|| (0, chrono::Utc::now().to_rfc3339()));
                let receipt_name = name.clone();
                match result {
                    Ok(output) => {
                        tool_error_seen |= !output.success;
                        summary.tools.push(ExecToolEntry {
                            name: name.clone(),
                            success: output.success,
                            output: output.content.clone(),
                        });
                        if output_format == ExecOutputFormat::StreamJson {
                            emit_exec_stream_event(&ExecStreamEvent::ToolResult {
                                id,
                                name: receipt_name,
                                output: output.content,
                                status: if output.success {
                                    "success".to_string()
                                } else {
                                    "error".to_string()
                                },
                                started_at,
                                completed_at: chrono::Utc::now().to_rfc3339(),
                                duration_ms,
                                side_effect_status: output
                                    .metadata
                                    .as_ref()
                                    .and_then(|metadata| metadata.get("side_effect_status"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_string(),
                                error_category: (!output.success).then(|| {
                                    output
                                        .metadata
                                        .as_ref()
                                        .and_then(|metadata| metadata.get("error_category"))
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("tool_reported_failure")
                                        .to_string()
                                }),
                                truncated: output
                                    .metadata
                                    .as_ref()
                                    .and_then(|metadata| metadata.get("truncated"))
                                    .and_then(serde_json::Value::as_bool),
                                artifact: tool_artifact_receipt(output.metadata.as_ref()),
                                result_metadata: output.metadata,
                            })?;
                        } else if !json_output {
                            if name == "exec_shell" && !output.content.trim().is_empty() {
                                eprintln!("tool {name} completed");
                                eprintln!(
                                    "--- stdout/stderr ---\n{}\n---------------------",
                                    output.content
                                );
                            } else {
                                eprintln!(
                                    "tool {name} completed: {}",
                                    summarize_tool_output(&output.content)
                                );
                            }
                        }
                    }
                    Err(err) => {
                        tool_error_seen = true;
                        let error_text = err.to_string();
                        summary.tools.push(ExecToolEntry {
                            name: name.clone(),
                            success: false,
                            output: error_text.clone(),
                        });
                        if output_format == ExecOutputFormat::StreamJson {
                            emit_exec_stream_event(&ExecStreamEvent::ToolResult {
                                id,
                                name: receipt_name,
                                output: error_text,
                                status: "error".to_string(),
                                started_at,
                                completed_at: chrono::Utc::now().to_rfc3339(),
                                duration_ms,
                                side_effect_status: "not_started_or_unknown".to_string(),
                                error_category: Some(tool_error_receipt_category(&err).to_string()),
                                truncated: None,
                                artifact: None,
                                result_metadata: None,
                            })?;
                        } else if !json_output {
                            eprintln!("tool {name} failed: {err}");
                        }
                    }
                }
            }
            Event::AgentSpawned { id, prompt, .. }
                if output_format == ExecOutputFormat::Text && !json_output =>
            {
                eprintln!("sub-agent {id} spawned: {}", summarize_tool_output(&prompt));
            }
            Event::AgentProgress { id, status, .. }
                if output_format == ExecOutputFormat::Text && !json_output =>
            {
                eprintln!("sub-agent {id}: {status}");
            }
            Event::AgentComplete {
                id,
                result,
                outcome,
                ..
            } if output_format == ExecOutputFormat::Text && !json_output => {
                eprintln!(
                    "sub-agent {id} {}: {}",
                    outcome
                        .as_ref()
                        .map(codewhale_engine::tools::subagent::subagent_status_name)
                        .unwrap_or("settled (outcome unconfirmed)"),
                    summarize_tool_output(&result)
                );
            }
            Event::AgentSpawned {
                id,
                parent_run_id,
                spawn_depth,
                model,
                route_source,
                ..
            } if output_format == ExecOutputFormat::StreamJson => {
                emit_exec_stream_event(&ExecStreamEvent::AgentSpawned {
                    id,
                    model,
                    spawn_depth,
                    parent_run_id,
                    route_source,
                })?;
            }
            Event::AgentSpawned { .. }
            | Event::AgentProgress { .. }
            | Event::AgentComplete { .. } => {}
            Event::WorkflowUi { run_id, event, .. }
                if output_format == ExecOutputFormat::StreamJson =>
            {
                emit_exec_stream_event(&ExecStreamEvent::WorkflowEvent { run_id, event })?;
            }
            // Headless runs have no person at the prompt: the run's flags
            // (the posture) answer every request.
            Event::ApprovalRequired {
                id,
                approval_force_prompt,
                ..
            } => {
                // An exact user decision (including extension-sourced shell
                // and network calls) cannot be supplied by a headless posture.
                if auto_approve && !approval_force_prompt {
                    let _ = engine_handle
                        .approve_tool_call_by(
                            id,
                            codewhale_engine::approval_log::ApprovalDecider::Posture,
                        )
                        .await;
                } else {
                    approval_required = true;
                    let _ = engine_handle
                        .deny_tool_call_by(
                            id,
                            codewhale_engine::approval_log::ApprovalDecider::Posture,
                        )
                        .await;
                }
            }
            Event::ElevationRequired {
                tool_id,
                tool_name,
                denial_reason,
                ..
            } => {
                if can_elevate_sandbox {
                    let policy = codewhale_sandbox::SandboxPolicy::DangerFullAccess;
                    let _ = engine_handle
                        .retry_tool_with_policy_by(
                            tool_id,
                            policy,
                            codewhale_engine::approval_log::ApprovalDecider::Posture,
                        )
                        .await;
                } else {
                    sandbox_denied = true;
                    approval_required = true;
                    summary.outcomes.push(ExecOutcome {
                        kind: "sandbox_denied".to_string(),
                        outcome: "approval_required".to_string(),
                        tool_name: tool_name.clone(),
                        reason: denial_reason.clone(),
                    });
                    if !reported_sandbox_contract {
                        eprintln!(
                            "sandbox denied {tool_name}: {denial_reason}; --auto approves tools but does not elevate sandbox access — use --sandbox danger-full-access or --allow-sandbox-elevation to opt in"
                        );
                        reported_sandbox_contract = true;
                    }
                    if output_format == ExecOutputFormat::StreamJson {
                        emit_exec_stream_event(&ExecStreamEvent::SandboxDenied {
                            tool_id: tool_id.clone(),
                            tool_name,
                            reason: denial_reason,
                            outcome: "approval_required".to_string(),
                        })?;
                    }
                    let _ = engine_handle
                        .deny_tool_call_by(
                            tool_id,
                            codewhale_engine::approval_log::ApprovalDecider::Posture,
                        )
                        .await;
                }
            }
            Event::Error {
                envelope,
                recoverable: _,
            } => {
                // Only a non-recoverable envelope may force the run summary
                // into failure. Recoverable warnings (stream-stall notices,
                // transient retry noise) are still streamed for visibility,
                // but the terminal TurnComplete event carries the
                // authoritative turn outcome — letting a warning set
                // `summary.error` here would exit an otherwise-successful
                // `exec` run non-zero.
                if exec_error_event_is_fatal(&envelope) {
                    last_error_category = Some(envelope.category);
                    summary.error_category = Some(envelope.category.to_string());
                    summary.error = Some(envelope.message.clone());
                }
                if output_format == ExecOutputFormat::StreamJson {
                    emit_exec_stream_event(&ExecStreamEvent::Error {
                        error: envelope.message,
                    })?;
                } else if !json_output {
                    eprintln!("error: {}", envelope.message);
                }
            }
            Event::ProviderRequestReceipt { receipt } => {
                if receipt
                    .get("wire_observations")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|observations| {
                        observations
                            .iter()
                            .any(|observation| observation.get("image_projection").is_some())
                    })
                {
                    should_persist_session = true;
                    image_request_receipts.push(receipt.clone());
                }
                if output_format == ExecOutputFormat::StreamJson {
                    emit_exec_stream_event(&ExecStreamEvent::ProviderRequestReceipt { receipt })?;
                }
            }
            Event::TurnUsage {
                usage,
                duration_ms,
                finish_reason,
                ..
            } => {
                if output_format == ExecOutputFormat::StreamJson {
                    turn_usage_seq = turn_usage_seq.saturating_add(1);
                    emit_exec_stream_event(&ExecStreamEvent::TurnUsage {
                        turn: turn_usage_seq,
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        reasoning_tokens: usage.reasoning_tokens,
                        prompt_cache_hit_tokens: usage.prompt_cache_hit_tokens,
                        prompt_cache_miss_tokens: usage.prompt_cache_miss_tokens,
                        prompt_cache_write_tokens: usage.prompt_cache_write_tokens,
                        reasoning_replay_tokens: usage.reasoning_replay_tokens,
                        duration_ms,
                        finish_reason,
                    })?;
                }
            }
            Event::TurnComplete {
                status,
                error,
                usage,
                tool_catalog,
                ..
            } => {
                let _timing = codewhale_runtime::logging::PhaseTimer::start("exec_postprocessing");
                let mut terminal_status = status;
                let terminal_error = error;
                #[cfg(unix)]
                let mut terminal_error = terminal_error;
                settled_usage = Some(usage.clone());
                if matches!(
                    terminal_status,
                    codewhale_engine::core::events::TurnOutcomeStatus::Completed
                ) && terminal_error.is_none()
                {
                    #[cfg(unix)]
                    match exec_shell_manager.lock() {
                        Ok(mut manager) => match manager.commit_persistent_services() {
                            Ok(receipts) => {
                                for receipt in &receipts {
                                    if output_format == ExecOutputFormat::StreamJson {
                                        emit_exec_stream_event(
                                            &ExecStreamEvent::ServiceReleased {
                                                task_id: receipt.task_id.clone(),
                                                pid: receipt.pid,
                                                process_group_id: receipt.process_group_id,
                                                ownership: receipt.ownership.clone(),
                                            },
                                        )?;
                                    } else if !json_output {
                                        eprintln!(
                                            "persistent service released: {} pid={} pgid={} ownership={}",
                                            receipt.task_id,
                                            receipt.pid,
                                            receipt.process_group_id,
                                            receipt.ownership
                                        );
                                    }
                                }
                                summary.released_services.extend(receipts);
                            }
                            Err(error) => {
                                manager.abort_persistent_services();
                                terminal_status =
                                    codewhale_engine::core::events::TurnOutcomeStatus::Failed;
                                terminal_error = Some(format!(
                                    "Persistent service ownership transfer failed: {error}"
                                ));
                            }
                        },
                        Err(_) => {
                            terminal_status =
                                codewhale_engine::core::events::TurnOutcomeStatus::Failed;
                            terminal_error = Some(
                                "Persistent service ownership transfer failed: shell manager lock poisoned"
                                    .to_string(),
                            );
                        }
                    }
                } else if let Ok(mut manager) = exec_shell_manager.lock() {
                    manager.abort_persistent_services();
                }
                summary.status = Some(format!("{terminal_status:?}").to_lowercase());
                if terminal_error.is_some() {
                    summary.error = terminal_error;
                }
                if sandbox_denied
                    && summary.error.is_none()
                    && matches!(
                        terminal_status,
                        codewhale_engine::core::events::TurnOutcomeStatus::Failed
                    )
                {
                    summary.error = Some(
                        "exec turn failed after sandbox denial; explicit sandbox elevation was not authorized"
                            .to_string(),
                    );
                }
                if last_error_category.is_none() {
                    last_error_category = summary
                        .error
                        .as_deref()
                        .map(codewhale_error_taxonomy::classify_error_message);
                    summary.error_category =
                        last_error_category.map(|category| category.to_string());
                }
                let saved_session_id = if should_persist_session && !latest_messages.is_empty() {
                    let saved = open_exec_session_manager().and_then(|manager| {
                        persist_exec_session(
                            &latest_messages,
                            &latest_model,
                            PersistedProviderRoute {
                                kind: effective_identity.persisted_kind(),
                                id: effective_identity.persisted_id(),
                            },
                            &latest_workspace,
                            &latest_system_prompt,
                            latest_session_id.as_deref(),
                            u64::from(usage.input_tokens) + u64::from(usage.output_tokens),
                            (terminal_status
                                == codewhale_engine::core::events::TurnOutcomeStatus::Failed)
                                .then_some(summary.error.as_deref().unwrap_or_else(|| {
                                    codewhale_engine::core::termination::classify_turn_termination(
                                        terminal_status,
                                        last_error_category,
                                        tool_error_seen,
                                        approval_required,
                                    )
                                    .as_str()
                                })),
                            manager.as_ref(),
                            &image_request_receipts,
                            None,
                        )
                    });
                    match saved {
                        Ok(id) => {
                            if output_format == ExecOutputFormat::Text && !json_output {
                                eprintln!("{}", exec_saved_session_line(&id));
                            }
                            Some(id)
                        }
                        Err(err) => {
                            let error = format!(
                                "Failed to save exec session: {err:#}. The task may already have run; do not retry automatically."
                            );
                            terminal_status =
                                codewhale_engine::core::events::TurnOutcomeStatus::Failed;
                            summary.status = Some("failed".to_string());
                            summary.error = Some(match summary.error.take() {
                                Some(previous) => format!("{previous}; {error}"),
                                None => error.clone(),
                            });
                            last_error_category =
                                Some(codewhale_error_taxonomy::ErrorCategory::State);
                            summary.error_category =
                                last_error_category.map(|category| category.to_string());
                            if output_format == ExecOutputFormat::StreamJson {
                                emit_exec_stream_event(&ExecStreamEvent::Error { error })?;
                            }
                            None
                        }
                    }
                } else {
                    None
                };
                // Lifecycle outbox: the clean headless turn-end boundary.
                // `terminal_status` includes persistent-service handoff and
                // session-save failures above, and
                // `summary.error` includes the sandbox-denial augmentation.
                // No-op when the feature is disabled.
                {
                    let outbox_status = format!("{terminal_status:?}").to_lowercase();
                    let kind = match terminal_status {
                        codewhale_engine::core::events::TurnOutcomeStatus::Completed => {
                            "turn.completed"
                        }
                        codewhale_engine::core::events::TurnOutcomeStatus::Failed => "turn.failed",
                        codewhale_engine::core::events::TurnOutcomeStatus::Interrupted => {
                            "turn.interrupted"
                        }
                    };
                    lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
                        event: "turn_end".to_string(),
                        kind: kind.to_string(),
                        thread_id: latest_session_id.clone().unwrap_or_default(),
                        turn_id: None,
                        item_id: None,
                        payload: serde_json::json!({
                            "status": outbox_status,
                            "duration_ms": exec_turn_started_at.elapsed().as_millis() as u64,
                            "workspace": latest_workspace.display().to_string(),
                            "error": summary.error.as_deref().map(|message| {
                                codewhale_hooks::bounded_text(
                                    message,
                                    codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
                                )
                            }),
                        }),
                    });
                }
                let termination_reason =
                    codewhale_engine::core::termination::classify_turn_termination(
                        terminal_status,
                        last_error_category,
                        tool_error_seen,
                        approval_required,
                    );
                summary.termination_reason = Some(termination_reason.as_str().to_string());
                // State the exit class here rather than inferring it later
                // from the process exit code: `Canceled` exits 130, the same
                // value the SIGINT path uses, so a code-based derivation would
                // report every Esc-cancelled turn as a signal. A no-op unless
                // this process was armed.
                if !termination_reason.is_success() {
                    codewhale_telemetry::set_exit_class(codewhale_telemetry::ExitClass::Error);
                }
                if output_format == ExecOutputFormat::StreamJson {
                    if let Some(id) = saved_session_id.as_ref() {
                        emit_exec_stream_event(&ExecStreamEvent::SessionCapture {
                            content: exec_stream_session_ref(id),
                            saved_session_id: id.clone(),
                        })?;
                    }
                    // Resolved output ceiling and its provenance, surfaced so a
                    // wrong ceiling is visible in the receipt rather than
                    // requiring packet capture.
                    let codewhale_max_output_tokens =
                        codewhale_engine::route_budget::effective_max_output_tokens_for_turn_with_window(
                            effective_provider,
                            &latest_model,
                            active_route_limits,
                            (latest_model == effective_model)
                                .then_some(effective_context_window.tokens),
                            None,
                            None,
                        );
                    let codewhale_max_output_tokens_source =
                        codewhale_engine::route_budget::output_ceiling_source(
                            effective_provider,
                            &latest_model,
                        )
                        .as_str();
                    // The deliverable is the final assistant reply of the
                    // session, not the cumulative stream output: a
                    // multi-step turn streams pre-tool commentary first,
                    // and that commentary is not part of the answer.
                    let final_answer = exec_stream_final_answer_text(
                        &latest_messages,
                        !summary.output.trim().is_empty(),
                    )
                    .unwrap_or_default();
                    let binary_sha256 = match binary_identity.take() {
                        Some(worker) => worker.await.ok().flatten(),
                        None => None,
                    };
                    emit_exec_stream_event(&ExecStreamEvent::Metadata {
                        meta: Box::new(ExecStreamMeta {
                            receipt_kind: "terminal",
                            provider: effective_provider_kind.clone(),
                            provider_id: effective_stream_provider_id.clone(),
                            model: latest_model.clone(),
                            route_source: route_source.clone(),
                            input_tokens: Some(usage.input_tokens),
                            output_tokens: Some(usage.output_tokens),
                            prompt_cache_hit_tokens: usage.prompt_cache_hit_tokens,
                            prompt_cache_miss_tokens: usage.prompt_cache_miss_tokens,
                            prompt_cache_write_tokens: usage.prompt_cache_write_tokens,
                            reasoning_tokens: usage.reasoning_tokens,
                            codewhale_max_output_tokens: Some(codewhale_max_output_tokens),
                            codewhale_max_output_tokens_source: Some(
                                codewhale_max_output_tokens_source,
                            ),
                            duration_ms: u64::try_from(exec_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                            retry_count: observed_retry_count,
                            approval_posture: approval_posture.clone(),
                            sandbox_posture: sandbox_posture.clone(),
                            binary_sha256,
                            config_sha256: None,
                            prompt_sha256: prompt_sha256.clone(),
                            tool_catalog_sha256: tool_catalog.as_ref().and_then(|catalog| {
                                serde_json::to_vec(catalog).ok().map(|bytes| {
                                    format!(
                                        "sha256:{}",
                                        codewhale_runtime::hashing::sha256_hex(&bytes)
                                    )
                                })
                            }),
                            input_analysis: exec_stream_input_analysis(
                                &latest_messages,
                                latest_system_prompt.as_ref(),
                            ),
                            visible_final_answer_chars: final_answer.chars().count(),
                            visible_final_answer_excerpt: exec_stream_final_answer_excerpt(
                                &final_answer,
                            ),
                            resume_command: saved_session_id
                                .as_deref()
                                .map(exec_stream_resume_hint)
                                .unwrap_or_default(),
                            session_id: saved_session_id
                                .as_deref()
                                .map(exec_stream_session_ref)
                                .unwrap_or_default(),
                            workspace: latest_workspace.display().to_string(),
                            message_count: latest_messages.len(),
                            status: summary.status.clone(),
                            termination_reason: summary.termination_reason.clone(),
                            error_category: summary.error_category.clone(),
                            error: summary.error.clone(),
                        }),
                    })?;
                    emit_exec_stream_event(&ExecStreamEvent::Done)?;
                }
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), engine_handle.send(Op::Shutdown))
                        .await;
                break;
            }
            Event::CompactionStarted { .. } => {
                // The Engine writes recovery artifacts under its session ID.
                // Keep the owning session discoverable even in text output.
                should_persist_session = true;
            }
            Event::SessionUpdated {
                session_id,
                messages,
                system_prompt,
                instruction_assembly: _,
                model,
                workspace,
                checkpoint,
            } => {
                if let Some(checkpoint) = checkpoint {
                    if session_id != exec_session_id
                        || workspace != exec_session_workspace
                        || active_turn_id.as_deref() != Some(checkpoint.caller.turn_id.as_str())
                        || model != checkpoint.snapshot.model
                        || system_prompt != checkpoint.snapshot.system_prompt
                    {
                        checkpoint
                            .fail("exec checkpoint event does not match its captured root turn");
                        continue;
                    }
                    if let Err(error) =
                        checkpoint.validate_prefix(&session_id, &workspace, &messages)
                    {
                        checkpoint.fail(error);
                        continue;
                    }
                    let manager = if should_persist_session {
                        open_exec_session_manager().map(Some)
                    } else {
                        Ok(None)
                    };
                    match manager {
                        Ok(manager) => {
                            persist_exec_checkpoint(
                                checkpoint,
                                manager,
                                &exec_session_id,
                                &exec_session_workspace,
                                active_turn_id.as_deref(),
                                &engine_handle,
                                exec_deadline,
                            )
                            .await
                        }
                        Err(error) => checkpoint
                            .fail(format!("could not open exec checkpoint store: {error:#}")),
                    }
                }
                latest_session_id = Some(session_id);
                latest_messages = messages;
                latest_system_prompt = system_prompt;
                latest_model = model;
                latest_workspace = workspace;
            }
            // A tool-less one-shot run has no tool channel, so a model that
            // still tries to call a tool writes the call as text. The engine
            // strips that markup from the answer; say why the answer is short
            // and how to give the model tools, instead of exiting on nothing.
            Event::Status { message }
                if one_shot && message == codewhale_engine::core::engine::FAKE_WRAPPER_NOTICE =>
            {
                eprintln!("{ONE_SHOT_TOOL_CALL_NOTICE}");
            }
            // #3027: surface the engine's max-steps notice in text mode so a
            // --max-turns run that stops early says why instead of going quiet.
            Event::Status { message }
                if output_format == ExecOutputFormat::Text
                    && !json_output
                    && message.contains("Maximum model steps") =>
            {
                eprintln!("{message}");
            }
            Event::ToolRequestSnapshot { snapshot } => {
                observed_retry_count =
                    accumulate_exec_retry_count(observed_retry_count, snapshot.terminal.as_ref());
            }
            Event::Status { message } => {
                if let Some(receipt) = exec_retry_status(&message) {
                    if output_format == ExecOutputFormat::StreamJson {
                        emit_exec_stream_event(&receipt)?;
                    } else if output_format == ExecOutputFormat::Text && !json_output {
                        eprintln!("{message}");
                    }
                }
            }
            _ => {}
        }
    }

    if summary.status.is_none() {
        if let Ok(mut manager) = exec_shell_manager.lock() {
            manager.abort_persistent_services();
        }
        let error = summary.error.clone().unwrap_or_else(|| {
            "Engine event channel closed before a terminal turn receipt".to_string()
        });
        let category = last_error_category
            .unwrap_or_else(|| codewhale_error_taxonomy::classify_error_message(&error));
        let termination_reason = codewhale_engine::core::termination::classify_turn_termination(
            codewhale_engine::core::events::TurnOutcomeStatus::Failed,
            Some(category),
            tool_error_seen,
            approval_required,
        );
        summary.status = Some("failed".to_string());
        summary.error_category = Some(category.to_string());
        summary.termination_reason = Some(termination_reason.as_str().to_string());
        summary.error = Some(error.clone());
        // Lifecycle outbox: the engine channel closed before a terminal
        // turn receipt. Every emitted `turn_start` still gets its matching
        // `turn_end` so a supervisor never sees an orphaned in-progress
        // turn. No-op when the feature is disabled.
        lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
            event: "turn_end".to_string(),
            kind: "turn.failed".to_string(),
            thread_id: latest_session_id.clone().unwrap_or_default(),
            turn_id: None,
            item_id: None,
            payload: serde_json::json!({
                "status": "failed",
                "duration_ms": exec_turn_started_at.elapsed().as_millis() as u64,
                "workspace": latest_workspace.display().to_string(),
                "error": codewhale_hooks::bounded_text(
                    &error,
                    codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
                ),
            }),
        });
        if output_format == ExecOutputFormat::StreamJson {
            emit_exec_stream_event(&ExecStreamEvent::Error { error })?;
        }
    }

    // Drain the terminal receipt before either returning or taking the explicit
    // retryable-failure process exit below. Outbox failures cannot change the
    // authoritative turn outcome.
    if let Err(error) = lifecycle_outbox.flush(Duration::from_secs(2)).await {
        tracing::warn!(target: "lifecycle_outbox", %error, "exec lifecycle outbox did not drain before exit");
    }

    if one_shot {
        summary.record_one_shot_outcome(settled_usage);
    }
    if json_output {
        write_exec_stdout(&format!("{}\n", serde_json::to_string_pretty(&summary)?))?;
    }

    if let Some(error) = summary.error.as_ref()
        && !error.trim().is_empty()
    {
        // Distinguish retryable infrastructure failures (provider/transport,
        // after all in-session retries are exhausted) from genuine task
        // failures so supervisors and bench harnesses can tell them apart at
        // the process level without parsing the stream. Genuine failures
        // keep the historical `bail!` → exit 1 path.
        let exit_code = exec_failure_exit_code(summary.error_category.as_deref());
        // The final line always carries the message: automation greps it and
        // a caller may keep only the last stderr line, even when the stream
        // already printed the same error above.
        if exit_code != 1 {
            eprintln!("Error: exec turn failed: {error}");
            let _ = io::stdout().flush();
            std::process::exit(exit_code);
        }
        bail!("exec turn failed: {error}");
    }

    if matches!(
        summary.status.as_deref(),
        Some("failed" | "canceled" | "interrupted")
    ) {
        let status = summary.status.as_deref().unwrap_or("unknown");
        bail!("exec turn ended with status {status}");
    }

    Ok(())
}

fn exec_retry_status(message: &str) -> Option<ExecStreamEvent> {
    (codewhale_engine::core::events::is_retry_status_receipt(message)
        || codewhale_engine::core::events::is_request_image_status_receipt(message))
    .then(|| ExecStreamEvent::Status {
        message: message.to_string(),
    })
}

fn accumulate_exec_retry_count(
    previous: Option<u32>,
    terminal: Option<&codewhale_engine::tool_inspection::TurnStopDiagnostics>,
) -> Option<u32> {
    let Some(terminal) = terminal.filter(|facts| facts.status.is_some()) else {
        return previous;
    };
    let retries = terminal
        .transport_retries
        .saturating_add(terminal.transparent_stream_retries)
        .saturating_add(terminal.stream_resumes)
        .saturating_add(terminal.empty_stop_retries)
        .saturating_add(terminal.reasoning_only_reprompts);
    Some(previous.unwrap_or(0).saturating_add(retries))
}

#[cfg(test)]
mod tests {
    use super::{ExecAgentEvents, exec_automation_services, exec_disallowed_tools};

    use codewhale_engine::core::engine::mock_engine_handle;
    use codewhale_engine::core::events::{Event, TurnOutcomeStatus};
    use codewhale_engine::core::ops::{Op, SubAgentSettlement};
    use codewhale_models::Usage;
    use codewhale_tools::names::REQUEST_USER_INPUT_NAME;
    use std::time::{Duration, Instant};

    fn completed_parent(input_tokens: u32) -> Event {
        let usage = Usage {
            input_tokens,
            ..Usage::default()
        };
        Event::TurnComplete {
            turn_id: None,
            submission_id: None,
            compaction_id: None,
            usage: usage.clone(),
            parent_route_usage: usage,
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        }
    }

    async fn reply_to_probe(
        operations: &mut tokio::sync::mpsc::Receiver<Op>,
        snapshot: SubAgentSettlement,
    ) {
        let Op::GetSubAgentSettlement { tx } = operations.recv().await.expect("host probe") else {
            panic!("host must not shut down while child work remains");
        };
        tx.lock().unwrap().take().unwrap().send(snapshot).unwrap();
    }

    #[test]
    fn headless_exec_withholds_request_user_input_without_a_responder() {
        // No responder exists on a one-shot CLI run, so the tool is
        // withheld by default rather than offered and stalled on.
        let disallowed = exec_disallowed_tools(None).expect("withhold list");
        assert!(
            disallowed
                .iter()
                .any(|tool| tool.as_str() == REQUEST_USER_INPUT_NAME),
            "request_user_input must be withheld by default: {disallowed:?}"
        );
        // An operator-passed entry is kept exactly once, not duplicated.
        let disallowed = exec_disallowed_tools(Some(vec![REQUEST_USER_INPUT_NAME.to_string()]))
            .expect("withhold list");
        assert_eq!(
            disallowed
                .iter()
                .filter(|tool| tool.as_str() == REQUEST_USER_INPUT_NAME)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn headless_success_waits_for_children_workflow_phases_and_parent_fan_in() {
        let mut engine = mock_engine_handle();
        let mut events = ExecAgentEvents::new(
            engine.handle.clone(),
            Instant::now() + Duration::from_secs(3),
        );
        engine.tx_event.send(completed_parent(11)).await.unwrap();
        let actor = async {
            reply_to_probe(
                &mut engine.rx_op,
                SubAgentSettlement {
                    running_children: 1,
                    running_workflows: 1,
                    pending_completions: 0,
                },
            )
            .await;
            reply_to_probe(
                &mut engine.rx_op,
                SubAgentSettlement {
                    running_children: 0,
                    running_workflows: 1,
                    pending_completions: 0,
                },
            )
            .await;
            reply_to_probe(
                &mut engine.rx_op,
                SubAgentSettlement {
                    running_children: 0,
                    running_workflows: 0,
                    pending_completions: 1,
                },
            )
            .await;
            engine
                .tx_event
                .send(Event::MessageDelta {
                    content: "child findings reviewed".into(),
                    index: 0,
                })
                .await
                .unwrap();
            engine.tx_event.send(completed_parent(7)).await.unwrap();
            reply_to_probe(&mut engine.rx_op, SubAgentSettlement::default()).await;
        };
        let host = async {
            assert!(
                matches!(events.next().await, Some(Event::MessageDelta { content, .. }) if content == "child findings reviewed")
            );
            let Some(Event::TurnComplete { usage, status, .. }) = events.next().await else {
                panic!("settled parent receipt")
            };
            assert_eq!(status, TurnOutcomeStatus::Completed);
            assert_eq!(
                usage.input_tokens, 18,
                "both parent turns are accounted once"
            );
        };
        tokio::time::timeout(Duration::from_secs(4), async { tokio::join!(actor, host) })
            .await
            .unwrap();
        assert!(
            !engine.handle.is_cancelled(),
            "ordinary success must not cancel children"
        );
        assert!(
            engine.rx_op.try_recv().is_err(),
            "the event reader does not send early Shutdown"
        );

        use codewhale_engine::core::events::{
            SessionCheckpointCaller, SessionCheckpointProjection, SessionCheckpointRequest,
        };
        use codewhale_engine::core::ops::SessionSnapshot;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        let tmp = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            codewhale_engine::session_manager::SessionManager::new(
                tmp.path().join("fleet-capture"),
            )
            .unwrap(),
        );
        let workspace = tmp.path().join("workspace");
        let messages: Vec<codewhale_models::Message> = serde_json::from_value(serde_json::json!([
            {"role":"user","content":[{"type":"text","text":"review the changes"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"provider-call","execution_id":"exec-call","name":"review","input":{}}]}
        ])).unwrap();
        let snapshot = Arc::new(SessionSnapshot {
            session_id: "exec-checkpoint".into(),
            messages: messages.clone(),
            total_tokens: 23,
            model: "review-model".into(),
            model_provider: "custom".into(),
            model_provider_id: Some("review-route".into()),
            workspace: workspace.clone(),
            system_prompt: None,
            instruction_assembly: None,
            mode: "exec".into(),
        });
        let caller = SessionCheckpointCaller {
            session_id: snapshot.session_id.clone(),
            agent_id: None,
            turn_id: "exec-turn".into(),
            call_id: "exec-call".into(),
            workspace: workspace.clone(),
        };
        let checkpoint = |caller: SessionCheckpointCaller, snapshot: Arc<SessionSnapshot>| {
            let (reply, receive) = tokio::sync::oneshot::channel();
            let hash = SessionCheckpointRequest::prefix_hash(&snapshot.messages).unwrap();
            (
                SessionCheckpointRequest::new(
                    caller,
                    snapshot,
                    hash,
                    SessionCheckpointProjection::Exact,
                    CancellationToken::new(),
                    reply,
                ),
                receive,
            )
        };
        let (request, receive) = checkpoint(caller.clone(), Arc::clone(&snapshot));
        super::persist_exec_checkpoint(
            Arc::clone(&request),
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert!(receive.await.unwrap().unwrap());
        let persisted = manager.load_session_snapshot(&snapshot.session_id).unwrap();
        assert_eq!(persisted.messages, messages);
        assert_eq!(persisted.metadata.total_tokens, 23);
        assert_eq!(
            persisted.metadata.model_provider_id.as_deref(),
            Some("review-route")
        );
        assert_eq!(persisted.metadata.workspace, workspace);
        assert_eq!(persisted.metadata.mode.as_deref(), Some("exec"));

        let mut completed = messages.clone();
        completed.push(serde_json::from_value(serde_json::json!({
            "role":"user","content":[{"type":"tool_result","tool_use_id":"provider-call","execution_id":"exec-call","content":"review complete"}]
        })).unwrap());
        crate::persist_exec_session(
            &completed,
            &snapshot.model,
            crate::PersistedProviderRoute {
                kind: "custom",
                id: Some("review-route"),
            },
            &workspace,
            &None,
            Some(&snapshot.session_id),
            42,
            None,
            manager.as_ref(),
            &[],
            None,
        )
        .unwrap();
        super::persist_exec_checkpoint(
            request,
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            manager
                .load_session_snapshot(&snapshot.session_id)
                .unwrap()
                .messages,
            completed
        );

        for mismatch in ["session", "workspace", "child", "turn"] {
            let mut foreign = caller.clone();
            match mismatch {
                "session" => foreign.session_id = "foreign-session".into(),
                "workspace" => foreign.workspace = tmp.path().join("foreign-workspace"),
                "child" => foreign.agent_id = Some("child-agent".into()),
                "turn" => foreign.turn_id = "foreign-turn".into(),
                _ => unreachable!(),
            }
            let (request, receive) = checkpoint(foreign, Arc::clone(&snapshot));
            super::persist_exec_checkpoint(
                request,
                Some(Arc::clone(&manager)),
                &snapshot.session_id,
                &workspace,
                Some("exec-turn"),
                &engine.handle,
                Instant::now() + Duration::from_secs(3),
            )
            .await;
            assert!(receive.await.unwrap().is_err(), "{mismatch}");
        }
        assert_eq!(
            manager
                .load_session_snapshot(&snapshot.session_id)
                .unwrap()
                .messages,
            completed
        );
        assert!(!manager.sessions_dir().join("foreign-session.json").exists());

        let (request, receive) = checkpoint(caller.clone(), Arc::clone(&snapshot));
        request.withdraw.cancel();
        super::persist_exec_checkpoint(
            request,
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert!(receive.await.unwrap().is_err());
        let (request, receive) = checkpoint(caller.clone(), Arc::clone(&snapshot));
        super::persist_exec_checkpoint(
            request,
            None,
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert!(!receive.await.unwrap().unwrap());

        let failed_store = Arc::new(
            codewhale_engine::session_manager::SessionManager::new(tmp.path().join("failed-store"))
                .unwrap(),
        );
        std::fs::create_dir(failed_store.sessions_dir().join("exec-checkpoint.json")).unwrap();
        let (request, receive) = checkpoint(caller.clone(), Arc::clone(&snapshot));
        super::persist_exec_checkpoint(
            request,
            Some(failed_store),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert!(receive.await.unwrap().is_err());
        let (request, receive) = checkpoint(caller, Arc::clone(&snapshot));
        super::persist_exec_checkpoint(
            request,
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now(),
        )
        .await;
        assert!(receive.await.unwrap().is_err());
        assert!(engine.handle.is_cancelled());
        assert_eq!(
            manager
                .load_session_snapshot(&snapshot.session_id)
                .unwrap()
                .messages,
            completed
        );
    }

    #[tokio::test]
    async fn headless_child_settlement_deadline_bounds_a_stalled_engine_probe() {
        let mut engine = mock_engine_handle();
        let mut events = ExecAgentEvents::new(
            engine.handle.clone(),
            Instant::now() + Duration::from_millis(30),
        );
        engine.tx_event.send(completed_parent(13)).await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap();
        let Some(Event::TurnComplete {
            status,
            error,
            usage,
            routed_usage_dropped_records,
            ..
        }) = event
        else {
            panic!("bounded failure receipt")
        };
        assert_eq!(status, TurnOutcomeStatus::Failed);
        assert!(error.unwrap().contains("wall-clock budget exhausted"));
        assert_eq!(usage.input_tokens, 13);
        assert_eq!(routed_usage_dropped_records, 1);
        assert!(engine.handle.is_cancelled());
        let mut shutdown = false;
        while let Ok(op) = engine.rx_op.try_recv() {
            shutdown |= matches!(op, Op::Shutdown);
        }
        assert!(shutdown, "shutdown must also cancel detached session tasks");
    }

    #[tokio::test]
    async fn headless_failed_or_interrupted_parent_skips_child_settlement() {
        for terminal_status in [TurnOutcomeStatus::Failed, TurnOutcomeStatus::Interrupted] {
            let mut engine = mock_engine_handle();
            let mut events = ExecAgentEvents::new(
                engine.handle.clone(),
                Instant::now() + Duration::from_secs(30),
            );
            let mut terminal = completed_parent(3);
            if let Event::TurnComplete { status, .. } = &mut terminal {
                *status = terminal_status;
            }
            engine.tx_event.send(terminal).await.unwrap();
            assert!(
                matches!(events.next().await, Some(Event::TurnComplete { status, .. }) if status == terminal_status)
            );
            assert!(
                engine.rx_op.try_recv().is_err(),
                "failure cannot admit another settling turn"
            );
        }
    }

    #[tokio::test]
    async fn headless_fatal_fan_in_error_cancels_before_releasing_terminal_receipt() {
        let engine = mock_engine_handle();
        let mut events = ExecAgentEvents::new(
            engine.handle.clone(),
            Instant::now() + Duration::from_secs(30),
        );
        engine.tx_event.send(completed_parent(5)).await.unwrap();
        engine
            .tx_event
            .send(Event::error(
                codewhale_error_taxonomy::ErrorEnvelope::fatal("fan-in route unavailable"),
            ))
            .await
            .unwrap();
        assert!(matches!(events.next().await, Some(Event::Error { .. })));
        assert!(engine.handle.is_cancelled());
        assert!(
            matches!(events.next().await, Some(Event::TurnComplete { status: TurnOutcomeStatus::Failed, error: Some(error), .. }) if error.contains("fan-in route unavailable"))
        );
    }

    #[tokio::test]
    async fn headless_cancel_during_child_wait_returns_interrupted() {
        let mut engine = mock_engine_handle();
        let mut events = ExecAgentEvents::new(
            engine.handle.clone(),
            Instant::now() + Duration::from_secs(30),
        );
        engine.tx_event.send(completed_parent(5)).await.unwrap();
        let cancel = async {
            reply_to_probe(
                &mut engine.rx_op,
                SubAgentSettlement {
                    running_children: 1,
                    running_workflows: 0,
                    pending_completions: 0,
                },
            )
            .await;
            engine.handle.cancel();
        };
        let host = async {
            assert!(matches!(
                events.next().await,
                Some(Event::TurnComplete {
                    status: TurnOutcomeStatus::Interrupted,
                    ..
                })
            ));
        };
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(cancel, host) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn headless_closed_engine_cannot_reuse_an_earlier_success_receipt() {
        let mut engine = mock_engine_handle();
        let mut events = ExecAgentEvents::new(
            engine.handle.clone(),
            Instant::now() + Duration::from_secs(30),
        );
        engine.tx_event.send(completed_parent(5)).await.unwrap();
        engine.close_event_stream();
        assert!(
            matches!(events.next().await, Some(Event::TurnComplete { status: TurnOutcomeStatus::Failed, error: Some(error), .. }) if error.contains("channel closed"))
        );
    }

    /// The reproduced defect: headless exec advertised `automation` while
    /// attaching no store, so every call — including the read-only `list` and
    /// `read` — failed "AutomationManager is not attached". Exec must attach
    /// the same durable store the TUI and the Runtime open.
    #[test]
    fn headless_exec_attaches_the_shared_automation_store() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("CODEWHALE_AUTOMATIONS_DIR", tmp.path());
        }
        let attached = exec_automation_services(false).expect("open store");
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("CODEWHALE_AUTOMATIONS_DIR");
        }
        let attached = attached.expect("exec attaches the automation store");
        let manager = attached.blocking_lock();
        // Reads work against the shared store...
        assert!(
            manager.list_automations().is_ok(),
            "an attached store must serve inspection"
        );
        // ...while the exec host stays outside the Runtime's exclusive
        // task-execution lease, so it claims no dispatch ownership.
        assert!(
            manager.execution_scope().is_none(),
            "a one-shot host must not claim an execution scope"
        );
    }

    /// A Fleet worker keeps the narrowed envelope it was launched with.
    #[test]
    fn fleet_workers_get_no_automation_store() {
        assert!(
            exec_automation_services(true)
                .expect("no store to open")
                .is_none()
        );
    }

    /// A store that cannot be opened is reported, not swallowed into a silent
    /// "not attached" at the first tool call.
    #[test]
    fn an_unopenable_store_fails_loudly() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        // A regular file cannot host the store's directories.
        let blocked = tmp.path().join("automations");
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("CODEWHALE_AUTOMATIONS_DIR", &blocked);
        }
        let result = exec_automation_services(false);
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("CODEWHALE_AUTOMATIONS_DIR");
        }
        let err = result.expect_err("opening the store must fail");
        assert!(
            format!("{err:#}").contains("automation store for headless exec"),
            "the failure must name what could not be opened: {err:#}"
        );
    }

    #[test]
    fn exec_retry_receipts_keep_the_existing_jsonl_schema_and_text() {
        for message in [
            "Retry attempt: transport 1/2; upstream 503; waiting 0.00s",
            "Retry recovery: transport request recovered after 1 retries",
            "Retry exhaustion: stream-resume stopped after 2 retries; stream interrupted",
            "Retry stopped: transparent stream completion was not observed",
            "Image request budget: omitted 4 older image-bearing blocks; retained 12 newest blocks.",
            "HTTP 413 recovery: retrying once without image attachments.",
        ] {
            let event = super::exec_retry_status(message).expect("request receipt projection");
            let value = crate::exec_stream_value(&event).unwrap();
            assert_eq!(value["type"], "status");
            assert_eq!(value["message"], message);
            assert_eq!(value["schema"], "codewhale.exec-stream");
            assert_eq!(value["schema_version"], 1);
        }
        assert!(super::exec_retry_status("Executing tools sequentially").is_none());
        assert!(super::exec_retry_status("Goal set; starting goal work.").is_none());
    }

    #[test]
    fn exec_retry_count_requires_terminal_facts_and_keeps_unknown_distinct_from_zero() {
        use codewhale_engine::tool_inspection::TurnStopDiagnostics;
        assert_eq!(super::accumulate_exec_retry_count(None, None), None);
        assert_eq!(
            super::accumulate_exec_retry_count(None, Some(&TurnStopDiagnostics::default())),
            None
        );
        let zero = TurnStopDiagnostics {
            status: Some(TurnOutcomeStatus::Completed),
            ..Default::default()
        };
        assert_eq!(
            super::accumulate_exec_retry_count(None, Some(&zero)),
            Some(0)
        );
        let retries = TurnStopDiagnostics {
            status: Some(TurnOutcomeStatus::Failed),
            transport_retries: 2,
            stream_resumes: 3,
            transparent_stream_retries: 1,
            empty_stop_retries: 1,
            reasoning_only_reprompts: 2,
            ..Default::default()
        };
        assert_eq!(
            super::accumulate_exec_retry_count(Some(0), Some(&retries)),
            Some(9)
        );
        assert_eq!(super::accumulate_exec_retry_count(Some(9), None), Some(9));
        assert_eq!(
            super::accumulate_exec_retry_count(Some(u32::MAX), Some(&retries)),
            Some(u32::MAX)
        );
    }
}
