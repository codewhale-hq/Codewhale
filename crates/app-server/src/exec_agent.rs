//! Non-interactive exec agent assembly: the `run_exec_agent` pipeline
//! that resolves the CLI route, builds the engine configuration, spawns
//! the engine, and drives the exec output stream to completion.
//!

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::ValueEnum;
use codewhale_engine::core::ops::TurnSpec;
use codewhale_engine::exec_events::ExecAgentEvents;
use codewhale_engine::exec_events::exec_error_event_is_fatal;
use codewhale_engine::fleet::executor::exec_stream_final_answer_excerpt;
use codewhale_engine::model_routing;
use codewhale_engine::session_manager;
use codewhale_engine::session_manager::SessionManager;
use codewhale_engine::session_manager::truncate_id;
use codewhale_models::ContentBlock;
use codewhale_models::Message;
use codewhale_models::Role;
use codewhale_models::SystemPrompt;
use codewhale_runtime::config::Config;
use codewhale_runtime::config::MAX_SUBAGENTS;
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

/// Resolve the headless `exec` model-step ceiling.
///
/// Omission leaves model steps uncapped. Clap rejects `--max-turns 0`;
/// explicit positive values retain the documented finite range.
pub fn exec_max_steps(max_turns: Option<u32>) -> u32 {
    codewhale_runtime::turn_budget::resolve_max_model_steps(max_turns)
}

/// Model-step ceiling for a plain (zero-tool) `exec` run without
/// `--max-turns`. Its only extra steps are output-limit continuations, which
/// have no progress signal of their own; without this a model stuck at the
/// output limit would be re-asked, with growing history, until the turn wall
/// clock (#6510 review).
pub const ONE_SHOT_DEFAULT_MAX_STEPS: u32 = 8;

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

async fn shutdown_exec_engine(
    handle: &codewhale_engine::core::engine::EngineHandle,
    deadline: tokio::time::Instant,
) -> Option<codewhale_engine::core::engine::EngineRunCompletion> {
    tokio::time::timeout_at(deadline, async {
        let _ = handle.send(codewhale_engine::core::ops::Op::Shutdown).await;
        handle.wait_run_completion().await
    })
    .await
    .ok()
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
    source: &mut Option<codewhale_engine::session_manager::SessionWriteReceipt>,
) {
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
    let Some(captured_source) = source.clone() else {
        request.fail("exec checkpoint has no admitted original session source");
        return;
    };
    let Some(captured_owner) = request.snapshot.source.clone() else {
        request.fail("exec checkpoint has no native Engine source");
        return;
    };
    let write_request = Arc::clone(&request);
    let mut writer = tokio::task::spawn_blocking(
        move || -> Result<codewhale_engine::session_manager::SessionPublication> {
            let snapshot = &write_request.snapshot;
            let publication = persist_exec_session(
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
                &captured_source,
                &captured_owner,
            )?;
            Ok(publication)
        },
    );
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
        Ok(Ok(publication)) => {
            *source = Some(publication.write_receipt());
            request.succeed(publication.checkpoint_receipt);
        }
        Ok(Err(error)) => request.fail(format!("exec session checkpoint failed: {error:#}")),
        Err(error) => request.fail(format!("exec session checkpoint writer failed: {error}")),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_exec_agent(
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

    let mut exec_write_receipt = resume_session
        .as_ref()
        .and_then(|saved| saved.write_receipt.clone());
    let exec_is_fresh = resume_session.is_none();

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
    let _exec_operation_lease = if exec_is_fresh {
        let (manager, source, lease) = admit_cli_saved_operation(
            exec_session_manager.clone(),
            exec_session_id.clone(),
            exec_session_workspace.clone(),
            model.to_string(),
            config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?,
            prompt.to_string(),
        )
        .await
        .context("could not admit exec's saved accounting operation")?;
        exec_session_manager = Some(manager);
        exec_write_receipt = Some(source);
        Some(lease)
    } else {
        None
    };
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

    let accounting_origin = exec_write_receipt
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("exec has no admitted original session source"))?
        .usage_origin(codewhale_engine::cost_status::scope_token())?;
    let route = resolve_cli_exec_route(
        config,
        model,
        prompt,
        force_configured_route,
        Some(accounting_origin.clone()),
    )
    .await?;
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
    .validate_for_async(codewhale_engine::route_runtime::RouteErrorSurface::Headless)
    .await
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
        accounting_origin: Some(accounting_origin),
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
            instrs.push(codewhale_engine::prompts::InstructionSource::Inline {
                name: "exec:no-user-present".into(),
                content:
                    "No user is present; choose the recommended default and state the assumption."
                        .into(),
            });
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
        goal_complete_judge: execution_config.goal_complete_judge(),
        goal_blocked_judge: execution_config.goal_blocked_judge(),
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
        tool_launcher: execution_config.tool_launcher.clone(),
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
        extra_trusted_paths: execution_config.extra_trusted_paths.clone(),
        exec_policy_engine: execution_config.exec_policy_engine.clone(),
        terminal_chrome_enabled: false,
        advisor_config: execution_config
            .advisor
            .as_ref()
            .map(codewhale_engine::tools::subagent::AdvisorConfig::from_toml)
            .unwrap_or_else(codewhale_engine::tools::subagent::AdvisorConfig::disabled),
    };

    let engine_handle = spawn_engine(engine_config, &execution_config, None).await;
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
                expected: None,
                control: codewhale_engine::core::ops::SessionControlId::new(),
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
    let mut provider_request_receipts = Vec::new();
    let mut latest_session_id = loaded_session_id;
    let mut latest_messages: Arc<Vec<Message>> = Arc::new(Vec::new());
    let mut latest_source: Option<codewhale_engine::core::ops::SessionSnapshotSource> = None;
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
                let image_projection = receipt
                    .get("wire_observations")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|observations| {
                        observations
                            .iter()
                            .any(|observation| observation.get("image_projection").is_some())
                    });
                let coverage = receipt.get("coverage").cloned().and_then(|value| {
                    serde_json::from_value::<codewhale_engine::llm_client::ProviderUsageCoverage>(
                        value,
                    )
                    .ok()
                });
                let unknown_primary = receipt["primary"].as_bool() == Some(true)
                    && matches!(
                        coverage,
                        Some(
                            codewhale_engine::llm_client::ProviderUsageCoverage::Missing
                                | codewhale_engine::llm_client::ProviderUsageCoverage::Incomplete
                        )
                    );
                if image_projection || unknown_primary {
                    should_persist_session = true;
                    provider_request_receipts.push(receipt.clone());
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
                turn_id,
                submission_id,
                compaction_id,
                status,
                error,
                usage,
                tool_catalog,
                ..
            } => {
                let _timing = codewhale_runtime::logging::PhaseTimer::start("exec_postprocessing");
                let terminal_source = codewhale_engine::core::ops::SessionSnapshotSource::terminal(
                    turn_id,
                    submission_id,
                    compaction_id,
                );
                let terminal_snapshot = match engine_handle.get_session_snapshot().await {
                    Ok(snapshot) => Some(snapshot),
                    Err(error) => {
                        tracing::warn!(%error, "exec terminal session snapshot could not be read; checkpoint retained");
                        None
                    }
                }.filter(|snapshot| {
                            snapshot.source.as_ref() == terminal_source.as_ref()
                                && terminal_source.is_some()
                                && snapshot.session_id == exec_session_id
                                && snapshot.workspace == exec_session_workspace
                                && latest_source.as_ref() == terminal_source.as_ref()
                                && snapshot.messages.as_slice() == latest_messages.as_slice()
                        });
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
                        if exec_write_receipt.is_none() && exec_is_fresh {
                            exec_write_receipt =
                                Some(manager.expect_absent_session(&exec_session_id)?);
                        }
                        let source = exec_write_receipt.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("exec original source was not admitted")
                        })?;
                        let owner = terminal_snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.source.as_ref())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "exec terminal snapshot belongs to another native turn"
                                )
                            })?;
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
                            &provider_request_receipts,
                            None,
                            source,
                            owner,
                        )
                    });
                    match saved {
                        Ok(publication) => {
                            let id = publication.metadata.id.clone();
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
                break;
            }
            Event::CompactionStarted { .. } => {
                // The Engine writes recovery artifacts under its session ID.
                // Keep the owning session discoverable even in text output.
                should_persist_session = true;
            }
            Event::SessionUpdated {
                source,
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
                        open_exec_session_manager().and_then(|manager| {
                            if exec_write_receipt.is_none() && exec_is_fresh {
                                exec_write_receipt =
                                    Some(manager.expect_absent_session(&exec_session_id)?);
                            }
                            Ok(Some(manager))
                        })
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
                                &mut exec_write_receipt,
                            )
                            .await
                        }
                        Err(error) => checkpoint
                            .fail(format!("could not open exec checkpoint store: {error:#}")),
                    }
                }
                latest_source = source;
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

    let shutdown_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let completion = shutdown_exec_engine(&engine_handle, shutdown_deadline).await;
    if completion != Some(codewhale_engine::core::engine::EngineRunCompletion::NormalReturn) {
        tracing::warn!(target: "engine", ?completion, "exec engine run completion was not confirmed");
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

    // Drain the terminal receipt before returning. Outbox failures cannot change the
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
    use super::{
        ExecAgentEvents, exec_automation_services, exec_disallowed_tools, shutdown_exec_engine,
    };

    use codewhale_engine::core::engine::mock_engine_handle;
    use codewhale_engine::core::events::{Event, TurnOutcomeStatus};
    use codewhale_engine::core::ops::{Op, SubAgentSettlement};
    use codewhale_models::Usage;
    use codewhale_tools::names::REQUEST_USER_INPUT_NAME;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn run_completion_exec_shutdown_waits_after_mailbox_delivery() {
        let mut engine = mock_engine_handle();
        let (release, receiver) = tokio::sync::oneshot::channel();
        let run = engine.observe_run_for_test(async move {
            receiver.await.unwrap();
        });
        let task = tokio::spawn(run);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let wait = shutdown_exec_engine(&engine.handle, deadline);
        tokio::pin!(wait);
        tokio::select! {
            biased;
            result = &mut wait => panic!("send alone is not completion: {result:?}"),
            op = engine.rx_op.recv() => assert!(matches!(op, Some(Op::Shutdown))),
        }
        assert!(
            tokio::time::timeout_at(tokio::time::Instant::now(), &mut wait)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(
            wait.await,
            Some(codewhale_engine::core::engine::EngineRunCompletion::NormalReturn)
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn run_completion_exec_shutdown_has_one_deadline_for_send_and_wait() {
        let mut engine = mock_engine_handle();
        let _run = engine.observe_run_for_test(std::future::pending());
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        tokio::time::sleep_until(deadline).await;
        {
            let mut shutdown = Box::pin(shutdown_exec_engine(&engine.handle, deadline));
            std::future::poll_fn(|cx| {
                assert!(matches!(
                    std::future::Future::poll(shutdown.as_mut(), cx),
                    std::task::Poll::Ready(None)
                ));
                std::task::Poll::Ready(())
            })
            .await;
        }
        assert!(matches!(engine.rx_op.recv().await, Some(Op::Shutdown)));
        while engine.handle.try_send(Op::Shutdown).is_ok() {}
        let mut shutdown = Box::pin(shutdown_exec_engine(&engine.handle, deadline));
        std::future::poll_fn(|cx| {
            assert!(matches!(
                std::future::Future::poll(shutdown.as_mut(), cx),
                std::task::Poll::Ready(None)
            ));
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn run_completion_exec_shutdown_uses_observed_exit_when_mailbox_closed() {
        use codewhale_engine::core::engine::EngineRunCompletion;
        for normal in [true, false] {
            let mut engine = mock_engine_handle();
            let run = engine.observe_run_for_test(async {});
            let expected = if normal {
                run.await;
                EngineRunCompletion::NormalReturn
            } else {
                drop(run);
                EngineRunCompletion::AbnormalTermination
            };
            engine.rx_op.close();
            assert_eq!(
                shutdown_exec_engine(
                    &engine.handle,
                    tokio::time::Instant::now() + Duration::from_secs(2)
                )
                .await,
                Some(expected)
            );
        }
    }

    #[test]
    fn run_completion_exec_shutdown_consumes_real_spawned_engine() {
        let _env = codewhale_engine::test_support::lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = codewhale_engine::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut config = codewhale_engine::core::engine::EngineConfig {
                workspace: home.path().into(),
                snapshots_enabled: false,
                memory_enabled: false,
                subagents_enabled: false,
                ..Default::default()
            };
            config
                .features
                .disable(codewhale_runtime::features::Feature::Mcp);
            let handle = codewhale_engine::core::engine::spawn_engine(
                config,
                &codewhale_runtime::config::Config::default(),
                None,
            )
            .await;
            assert_eq!(
                shutdown_exec_engine(
                    &handle,
                    tokio::time::Instant::now() + Duration::from_secs(5)
                )
                .await,
                Some(codewhale_engine::core::engine::EngineRunCompletion::NormalReturn)
            );
        });
    }

    fn completed_parent(input_tokens: u32) -> Event {
        let usage = Usage {
            input_tokens,
            ..Usage::default()
        };
        Event::TurnComplete {
            source_accounting: false,

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
            source: Some(codewhale_engine::core::ops::SessionSnapshotSource::turn(
                "exec-turn".into(),
                None,
            )),

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
        let mut source = Some(manager.expect_absent_session(&snapshot.session_id).unwrap());
        let (request, receive) = checkpoint(caller.clone(), Arc::clone(&snapshot));
        super::persist_exec_checkpoint(
            Arc::clone(&request),
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
            &mut source,
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
        let publication = crate::exec_agent::persist_exec_session(
            &completed,
            &snapshot.model,
            crate::exec_agent::PersistedProviderRoute {
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
            source.as_ref().unwrap(),
            snapshot.source.as_ref().unwrap(),
        )
        .unwrap();
        source = Some(publication.write_receipt());
        super::persist_exec_checkpoint(
            request,
            Some(Arc::clone(&manager)),
            &snapshot.session_id,
            &workspace,
            Some("exec-turn"),
            &engine.handle,
            Instant::now() + Duration::from_secs(3),
            &mut source,
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
                &mut source,
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
            &mut source,
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
            &mut source,
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
            &mut source,
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
            &mut source,
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
        let _lock = codewhale_engine::test_support::lock_test_env();
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
        let _lock = codewhale_engine::test_support::lock_test_env();
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
            let value = crate::exec_agent::exec_stream_value(&event).unwrap();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExecOutputFormat {
    Text,
    #[value(name = "stream-json")]
    StreamJson,
}

pub async fn admit_cli_saved_operation(
    manager: Option<std::sync::Arc<session_manager::SessionManager>>,
    id: String,
    workspace: PathBuf,
    model: String,
    provider: codewhale_runtime::config::ProviderIdentity,
    prompt: String,
) -> Result<(
    std::sync::Arc<session_manager::SessionManager>,
    session_manager::SessionWriteReceipt,
    session_manager::SessionLease,
)> {
    codewhale_runtime::runtime_control::owner_work(move || {
        let manager = match manager {
            Some(manager) => manager,
            None => std::sync::Arc::new(session_manager::SessionManager::default_location()?),
        };
        let lease = manager.reserve_session_for_external_write(&id)?;
        let source = manager.expect_absent_session(&id)?;
        let messages = vec![codewhale_models::Message {
            role: codewhale_models::Role::User,
            content: vec![codewhale_models::ContentBlock::Text {
                text: prompt,
                cache_control: None,
            }],
        }];
        let mut saved = session_manager::create_saved_session_with_id_and_mode(
            id,
            &messages,
            &model,
            &workspace,
            0,
            None,
            Some("exec"),
        );
        let (kind, provider_id) = exec_stream_provider_route(&provider);
        saved.metadata.model_provider = kind;
        saved.metadata.model_provider_id = provider_id;
        saved.metadata.cost.coverage_recorded = true;
        saved.write_receipt = Some(source);
        saved.capture_publication(session_manager::SessionWriteOwner::control())?;
        let publication = manager.publish_session(&saved)?;
        Ok((manager, publication.write_receipt(), lease))
    })
    .await
}

#[cfg(windows)]
pub fn paths_equal_for_config(left: &Path, right: &Path) -> bool {
    normalize_windows_config_path_for_compare(left)
        == normalize_windows_config_path_for_compare(right)
}

#[cfg(not(windows))]
pub fn paths_equal_for_config(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn normalize_windows_config_path_for_compare(path: &Path) -> String {
    normalize_windows_config_path_str(&path.to_string_lossy())
}

#[cfg(any(windows, test))]
fn normalize_windows_config_path_str(path: &str) -> String {
    let mut normalized = path.replace('/', "\\");
    if let Some(rest) = normalized.strip_prefix(r"\\?\UNC\") {
        normalized = format!("\\\\{rest}");
    } else if let Some(rest) = normalized.strip_prefix(r"\\?\") {
        normalized = rest.to_string();
    }
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized.to_ascii_lowercase()
}

#[derive(Debug)]
pub struct CliAutoRoute {
    pub provider: codewhale_runtime::config::ProviderIdentity,
    pub model: String,
    pub reasoning_effort: Option<codewhale_engine::reasoning_preference::ReasoningEffort>,
    /// Whether the runtime should continue resolving reasoning per prompt.
    ///
    /// This is independent from `auto_model`: an Auto model can carry a fixed
    /// saved effort, while a fixed Fleet model can still request Auto effort.
    pub auto_controls_reasoning: bool,
    pub auto_model: bool,
}

pub fn cli_reasoning_effort_value(
    config: &Config,
    model: &str,
    effort: codewhale_engine::reasoning_preference::ReasoningEffort,
) -> Option<String> {
    let identity = config.active_provider_identity().ok()?;
    effort
        .normalize_for_route_identity(
            identity.provider,
            identity.key.as_str(),
            &config.base_url_for_route(&identity),
            model,
        )
        .api_value_for_provider(identity.provider)
        .map(str::to_string)
}

fn cli_reasoning_effort_value_for_prompt(
    config: &Config,
    model: &str,
    effort: codewhale_engine::reasoning_preference::ReasoningEffort,
) -> Option<String> {
    let resolved = if effort == codewhale_engine::reasoning_preference::ReasoningEffort::Auto {
        codewhale_engine::auto_reasoning::select()
    } else {
        effort
    };
    cli_reasoning_effort_value(config, model, resolved)
}

pub fn config_for_cli_route(config: &Config, route: &CliAutoRoute) -> Result<Config> {
    let mut execution_config = config.clone();
    execution_config
        .scope_to_provider_identity(&route.provider)
        .map_err(anyhow::Error::msg)?;
    execution_config.set_provider_model_override(&route.provider, Some(route.model.clone()))?;
    if route.provider.provider == codewhale_config::ProviderKind::Deepseek {
        execution_config.default_text_model = Some(route.model.clone());
    }
    Ok(execution_config)
}

async fn resolve_cli_auto_route(
    config: &Config,
    model: &str,
    prompt: &str,
    accounting_origin: Option<codewhale_engine::cost_status::NativeUsageOrigin>,
) -> Result<CliAutoRoute> {
    let active = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if model.trim().eq_ignore_ascii_case("auto") {
        let selection =
            model_routing::resolve_auto_route_with_inventory_for_session_and_cache_policy(
                config,
                prompt,
                "",
                "agent",
                "auto",
                "auto",
                true,
                accounting_origin.map(|origin| {
                    codewhale_engine::cost_status::NativeUsageSource::capture_with_origin(
                        format!("cli-auto-router:{}", uuid::Uuid::new_v4()),
                        codewhale_engine::cost_status::UsageSpendOwner::Auxiliary,
                        Some(origin),
                    )
                }),
            )
            .await?;
        let preference = config
            .reasoning_effort()
            .filter(|_| config.reasoning_effort_is_explicit())
            .map(codewhale_engine::reasoning_preference::ReasoningEffort::from_setting);
        let (reasoning_effort, auto_controls_reasoning) =
            model_routing::resolve_auto_model_reasoning(preference, selection.reasoning_effort);
        Ok(CliAutoRoute {
            provider: selection.provider,
            model: selection.model,
            reasoning_effort,
            auto_controls_reasoning,
            auto_model: true,
        })
    } else {
        if let Some(selection) = model_routing::resolve_explicit_route_with_inventory(config, model)
        {
            let auto_controls_reasoning = matches!(
                selection.reasoning_effort,
                Some(codewhale_engine::reasoning_preference::ReasoningEffort::Auto)
            );
            return Ok(CliAutoRoute {
                provider: selection.provider,
                model: selection.model,
                reasoning_effort: selection.reasoning_effort,
                auto_controls_reasoning,
                auto_model: false,
            });
        }

        let candidate_providers = model_routing::explicit_route_candidate_providers(config, model);
        if !candidate_providers.is_empty() && !candidate_providers.contains(&active) {
            let providers = candidate_providers
                .iter()
                .map(|provider| provider.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "model `{model}` is available from configured provider route(s): {providers}. \
                 Pass `--provider <provider>` with `--model {model}` to choose one explicitly. \
                 In the TUI, use `/provider`, `/model`, or `/setup` to resolve the route before sending."
            );
        }

        // When --model is not `auto`, fall back to the reasoning_effort
        // declared in the user's config.toml. The previous hard-coded `None`
        // silently dropped the user's setting on every non-auto-route exec
        // call, which (for example) prevented vllm + Qwen3 users from
        // disabling thinking via `reasoning_effort = "off"` and caused
        // 30+ second SSE idle timeouts on trivial prompts.
        let reasoning_effort = config
            .reasoning_effort()
            .map(codewhale_engine::reasoning_preference::ReasoningEffort::from_setting);
        Ok(CliAutoRoute {
            provider: active,
            model: model.to_string(),
            auto_controls_reasoning: matches!(
                reasoning_effort,
                Some(codewhale_engine::reasoning_preference::ReasoningEffort::Auto)
            ),
            reasoning_effort,
            auto_model: false,
        })
    }
}

pub async fn resolve_cli_exec_route(
    config: &Config,
    model: &str,
    prompt: &str,
    force_configured_route: bool,
    accounting_origin: Option<codewhale_engine::cost_status::NativeUsageOrigin>,
) -> Result<CliAutoRoute> {
    let mut local_default = None;
    if let Ok(identity) = config.active_provider_identity()
        && identity.provider == codewhale_config::ProviderKind::Ollama
    {
        codewhale_engine::provider_lake::refresh_local_ollama_catalog(config, &identity).await;
        if codewhale_runtime::config::is_unresolved_local_ollama_model(model)
            || (force_configured_route && model.trim().eq_ignore_ascii_case("auto"))
        {
            local_default = Some(
                codewhale_engine::route_runtime::resolve_runtime_route_for_identity(
                    config, &identity, None,
                )
                .map_err(anyhow::Error::msg)?
                .model,
            );
        }
    }
    let model = local_default.as_deref().unwrap_or(model);
    if force_configured_route && !model.trim().eq_ignore_ascii_case("auto") {
        let reasoning_effort = config
            .reasoning_effort()
            .map(codewhale_engine::reasoning_preference::ReasoningEffort::from_setting);
        return Ok(CliAutoRoute {
            provider: config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?,
            model: model.to_string(),
            auto_controls_reasoning: matches!(
                reasoning_effort,
                Some(codewhale_engine::reasoning_preference::ReasoningEffort::Auto)
            ),
            reasoning_effort,
            auto_model: false,
        });
    }
    resolve_cli_auto_route(config, model, prompt, accounting_origin).await
}

pub fn exec_stream_provider_route(
    identity: &codewhale_runtime::config::ProviderIdentity,
) -> (String, Option<String>) {
    let provider = identity.persisted_kind().to_string();
    let provider_id = if identity.provider == codewhale_config::ProviderKind::Custom {
        identity.persisted_id().map(str::to_string)
    } else {
        None
    };
    (provider, provider_id)
}

#[derive(serde::Serialize)]
pub struct ExecStreamMeta {
    pub receipt_kind: &'static str,
    pub provider: String,
    /// Exact configured provider-table id, when one selected the route.
    /// `None` deliberately distinguishes the legacy idless root custom route
    /// from literal `[providers.custom]`, whose exact id is `"custom"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub model: String,
    pub route_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_write_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Resolved output ceiling the route actually requested (post-catalogue).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codewhale_max_output_tokens: Option<u32>,
    /// Provenance of that ceiling: `documented`, `uncatalogued`, or
    /// `route-declared`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codewhale_max_output_tokens_source: Option<&'static str>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    pub approval_posture: String,
    pub sandbox_posture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_sha256: Option<String>,
    pub prompt_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_catalog_sha256: Option<String>,
    pub input_analysis: ExecStreamInputAnalysis,
    /// Real character count of the visible final answer, before any bound.
    pub visible_final_answer_chars: usize,
    /// Bounded, secret-redacted excerpt of the visible final answer (see
    /// [`exec_stream_final_answer_excerpt`]). Omitted when the run produced
    /// no visible answer.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub visible_final_answer_excerpt: String,
    pub session_id: String,
    pub resume_command: String,
    pub workspace: String,
    pub message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ExecStreamInputAnalysis {
    estimated_request_tokens: usize,
    estimated_message_content_tokens: usize,
    estimated_system_tokens: usize,
    estimated_framing_tokens: usize,
    user_message_count: usize,
    assistant_message_count: usize,
    tool_message_count: usize,
    tool_use_count: usize,
    tool_result_count: usize,
    text_chars: usize,
    thinking_chars: usize,
    tool_use_input_chars: usize,
    tool_result_chars: usize,
    text_estimated_tokens: usize,
    thinking_estimated_tokens: usize,
    tool_use_input_estimated_tokens: usize,
    tool_result_estimated_tokens: usize,
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
// Keep receipts flat for stable JSONL consumers. Boxing the whole tool_result
// payload would introduce a nested object and break the stream schema.
#[allow(clippy::large_enum_variant)]
pub enum ExecStreamEvent {
    #[serde(rename = "status")]
    Status { message: String },
    #[serde(rename = "request_image_window")]
    RequestImageWindow {
        provider: String,
        model: String,
        total: usize,
        elided: usize,
        cap: usize,
        cap_source: String,
        message: String,
    },
    #[serde(rename = "content")]
    Content { content: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        name: String,
        id: String,
        input: serde_json::Value,
        started_at: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        name: String,
        output: String,
        status: String,
        started_at: String,
        completed_at: String,
        duration_ms: u64,
        side_effect_status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_category: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result_metadata: Option<serde_json::Value>,
    },
    /// A sub-agent was launched, and the model it was launched on.
    ///
    /// Without this, a delegated child is invisible to anything reading the
    /// stream: a parent turn on one route could spawn children billed on
    /// another and the only place it surfaced was the invoice. That is not
    /// hypothetical — the `Fast` loadout re-priced scout children onto a
    /// cheaper sibling until it was fixed, and nothing in the output said so.
    #[serde(rename = "agent_spawned")]
    AgentSpawned {
        id: String,
        model: String,
        spawn_depth: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        /// Why the child got this route, when the spawn path resolved one.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_source: Option<String>,
    },
    #[serde(rename = "sandbox_denied")]
    SandboxDenied {
        tool_id: String,
        tool_name: String,
        reason: String,
        outcome: String,
    },
    #[serde(rename = "workflow_event")]
    WorkflowEvent {
        run_id: String,
        event: serde_json::Value,
    },
    #[serde(rename = "session_capture")]
    SessionCapture {
        /// Redacted fingerprint for logs/forensics, the same value the
        /// terminal `metadata.session_id` carries; never the recoverable id.
        content: String,
        /// The real saved-session id a caller can resolve via
        /// `GET /v1/sessions/{id}` to read the worker's full transcript. This
        /// is the only place the exec stream carries the raw id: `metadata`
        /// stays fingerprint-only so a captured terminal receipt is safe to
        /// log on its own.
        saved_session_id: String,
    },
    #[serde(rename = "service_released")]
    #[cfg(unix)]
    ServiceReleased {
        task_id: String,
        pid: u32,
        process_group_id: u32,
        ownership: String,
    },
    #[serde(rename = "provider_request_receipt")]
    ProviderRequestReceipt { receipt: serde_json::Value },
    /// Per-model-call usage receipt. Field names mirror the terminal
    /// `metadata` receipt (`prompt_cache_hit_tokens` is the provider's
    /// cache-read count, `prompt_cache_write_tokens` the cache-creation
    /// count). Optional fields are omitted — never emitted as null or zero —
    /// when the provider does not report them; the whole event is skipped
    /// for model calls whose provider reported no usage at all.
    #[serde(rename = "turn_usage")]
    TurnUsage {
        /// 1-based index of the model call within this exec run.
        turn: u32,
        input_tokens: u32,
        output_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_hit_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_miss_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_write_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_replay_tokens: Option<u32>,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    #[serde(rename = "metadata")]
    Metadata { meta: Box<ExecStreamMeta> },
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "error")]
    Error { error: String },
}

pub fn exec_sandbox_elevation_authorized(
    allow_sandbox_elevation: bool,
    explicit_sandbox: Option<&str>,
) -> bool {
    allow_sandbox_elevation
        || explicit_sandbox.is_some_and(|policy| policy.eq_ignore_ascii_case("danger-full-access"))
}

pub fn emit_exec_stream_event(event: &ExecStreamEvent) -> Result<()> {
    let mut line = serde_json::to_string(&exec_stream_value(event)?)?;
    line.push('\n');
    write_exec_stdout(&line)
}

/// Headless `exec` ignores SIGPIPE while it runs, because it writes to pipes
/// it does not own: a stdio MCP server, LSP, hook or shell child that exits
/// early must fail that one write with `EPIPE`, not kill the run with no
/// output. Under the default disposition, an MCP server whose interpreter
/// could not start (a broken `node` on PATH for the built-in Computer Use
/// plugin) made `exec --auto` exit 141 before printing anything.
fn ignore_sigpipe_for_headless_exec() {
    // SAFETY: a plain disposition change with no handler. Children still start
    // with SIGPIPE at SIG_DFL: the standard library resets it before exec.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Write exec output to stdout. SIGPIPE is ignored during exec (see
/// [`ignore_sigpipe_for_headless_exec`]), so a reader that closed stdout
/// (`codewhale exec ... | head -1`) surfaces here as `BrokenPipe`. End the
/// process the way the default disposition would have (#4030) instead of
/// panicking inside `print!`.
fn write_exec_stdout(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
            // SAFETY: restores the default disposition and re-raises the
            // signal the write would have delivered without SIG_IGN.
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::raise(libc::SIGPIPE);
            }
            std::process::exit(141);
        }
        result => result.map_err(Into::into),
    }
}

fn exec_stream_value(event: &ExecStreamEvent) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(event)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("schema_version".to_string(), serde_json::json!(1));
        object.insert(
            "schema".to_string(),
            serde_json::json!("codewhale.exec-stream"),
        );
    }
    Ok(value)
}

fn tool_error_receipt_category(error: &codewhale_engine::tools::spec::ToolError) -> &'static str {
    use codewhale_engine::tools::spec::ToolError;
    match error {
        ToolError::InvalidInput { .. } => "invalid_input",
        ToolError::MissingField { .. } => "missing_field",
        ToolError::PathEscape { .. } => "path_escape",
        ToolError::ExecutionFailed { .. } => "execution_failed",
        ToolError::Timeout { .. } => "timeout",
        ToolError::Cancelled { .. } => "cancelled",
        ToolError::NotAvailable { .. } => "not_available",
        ToolError::PermissionDenied { .. } => "permission_denied",
    }
}

pub fn tool_artifact_receipt(metadata: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let object = metadata?.as_object()?;
    let mut artifact = serde_json::Map::new();
    for key in [
        "artifact_id",
        "artifact_path",
        "artifact_relative_path",
        "artifact_byte_size",
        "spillover_path",
        "content_digest",
        "original_byte_count",
        "retained_head_bytes",
        "retained_tail_bytes",
    ] {
        if let Some(value) = object.get(key) {
            artifact.insert(key.to_string(), value.clone());
        }
    }
    (!artifact.is_empty()).then_some(serde_json::Value::Object(artifact))
}

pub fn start_binary_sha256() -> tokio::task::JoinHandle<Option<String>> {
    let file = std::env::current_exe().and_then(std::fs::File::open);
    tokio::task::spawn_blocking(move || {
        let _timing = codewhale_runtime::logging::PhaseTimer::start("binary_identity");
        let hash = codewhale_release::installs::reader_sha256(file.ok()?).ok()?;
        Some(format!("sha256:{hash}"))
    })
}

fn exec_stream_input_analysis(
    messages: &[Message],
    system: Option<&SystemPrompt>,
) -> ExecStreamInputAnalysis {
    let mut analysis = ExecStreamInputAnalysis {
        estimated_request_tokens: codewhale_engine::compaction::estimate_input_tokens_conservative(
            messages, system,
        ),
        estimated_message_content_tokens: codewhale_engine::compaction::estimate_tokens(messages),
        estimated_system_tokens: exec_stream_estimate_system_tokens(system),
        estimated_framing_tokens: messages.len().saturating_mul(12).saturating_add(48),
        ..ExecStreamInputAnalysis::default()
    };

    for message in messages {
        match message.role.as_str() {
            "user" => analysis.user_message_count += 1,
            "assistant" => analysis.assistant_message_count += 1,
            "tool" => analysis.tool_message_count += 1,
            _ => {}
        }

        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    exec_stream_add_text_estimate(
                        text,
                        &mut analysis.text_chars,
                        &mut analysis.text_estimated_tokens,
                    );
                }
                ContentBlock::Thinking { thinking, .. } => {
                    exec_stream_add_text_estimate(
                        thinking,
                        &mut analysis.thinking_chars,
                        &mut analysis.thinking_estimated_tokens,
                    );
                }
                ContentBlock::ToolUse { input, .. } | ContentBlock::ServerToolUse { input, .. } => {
                    analysis.tool_use_count += 1;
                    exec_stream_add_json_estimate(
                        input,
                        &mut analysis.tool_use_input_chars,
                        &mut analysis.tool_use_input_estimated_tokens,
                    );
                }
                ContentBlock::ToolResult {
                    content,
                    content_blocks,
                    ..
                } => {
                    analysis.tool_result_count += 1;
                    exec_stream_add_text_estimate(
                        content,
                        &mut analysis.tool_result_chars,
                        &mut analysis.tool_result_estimated_tokens,
                    );
                    if let Some(blocks) = content_blocks {
                        exec_stream_add_json_estimate(
                            blocks,
                            &mut analysis.tool_result_chars,
                            &mut analysis.tool_result_estimated_tokens,
                        );
                    }
                }
                ContentBlock::ToolSearchToolResult { content, .. }
                | ContentBlock::CodeExecutionToolResult { content, .. } => {
                    analysis.tool_result_count += 1;
                    exec_stream_add_json_estimate(
                        content,
                        &mut analysis.tool_result_chars,
                        &mut analysis.tool_result_estimated_tokens,
                    );
                }
                ContentBlock::ImageUrl { .. } => {}
            }
        }
    }

    analysis
}

fn exec_stream_add_text_estimate(text: &str, chars: &mut usize, tokens: &mut usize) {
    *chars = chars.saturating_add(text.chars().count());
    *tokens = tokens
        .saturating_add(codewhale_engine::compaction::estimate_text_tokens_conservative(text));
}

fn exec_stream_add_json_estimate<T: serde::Serialize>(
    value: &T,
    chars: &mut usize,
    tokens: &mut usize,
) {
    let text = serde_json::to_string(value).unwrap_or_default();
    exec_stream_add_text_estimate(&text, chars, tokens);
}

fn exec_stream_estimate_system_tokens(system: Option<&SystemPrompt>) -> usize {
    match system {
        Some(SystemPrompt::Text(text)) => {
            codewhale_engine::compaction::estimate_text_tokens_conservative(text)
        }
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| {
                codewhale_engine::compaction::estimate_text_tokens_conservative(&block.text)
            })
            .sum(),
        None => 0,
    }
}

fn exec_saved_session_line(session_id: &str) -> String {
    format!("session: {}", truncate_id(session_id))
}

fn exec_resumed_session_line(session_id: &str) -> String {
    format!("resumed session: {}", truncate_id(session_id))
}

pub fn exec_stream_session_ref(session_id: &str) -> String {
    codewhale_util::utils::redacted_identifier_for_log(session_id)
}

/// Resume hint for the terminal `metadata` receipt. `metadata` carries only
/// the session fingerprint, so the hint names the `session_capture` field
/// that holds the recoverable id instead of pretending to redact one.
fn exec_stream_resume_hint(session_id: &str) -> String {
    if session_id.trim().is_empty() {
        String::new()
    } else {
        "codewhale exec --resume <session_capture.saved_session_id>".to_string()
    }
}

/// The final visible assistant reply for the terminal receipt: the text
/// blocks of the last assistant-like message after the current user prompt.
/// Tool results also use the user role, so they must not start a new turn.
/// A resumed session can be synchronized before its new prompt is accepted;
/// without current output its old answer must never become a new deliverable.
fn exec_stream_final_answer_text(
    messages: &[Message],
    current_turn_has_output: bool,
) -> Option<String> {
    if !current_turn_has_output {
        return None;
    }
    let turn_start = messages.iter().rposition(|message| {
        message.role == Role::User
            && !codewhale_models::turn_classify::is_runtime_owned_user_message(message)
            && !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    })?;
    let text = messages
        .iter()
        .skip(turn_start + 1)
        .rev()
        .find(|message| message.role.is_assistant_like())?
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

#[derive(Clone, Copy)]
struct PersistedProviderRoute<'a> {
    kind: &'a str,
    id: Option<&'a str>,
}

fn persist_exec_session(
    messages: &[Message],
    model: &str,
    provider_route: PersistedProviderRoute<'_>,
    workspace: &Path,
    system_prompt: &Option<SystemPrompt>,
    session_id: Option<&str>,
    total_tokens: u64,
    turn_failure: Option<&str>,
    manager: &SessionManager,
    provider_request_receipts: &[serde_json::Value],
    checkpoint: Option<&codewhale_engine::core::events::SessionCheckpointRequest>,
    source: &session_manager::SessionWriteReceipt,
    owner: &codewhale_engine::core::ops::SessionSnapshotSource,
) -> Result<session_manager::SessionPublication> {
    let captured = source.capture(session_manager::SessionWriteOwner::Engine(owner.clone()))?;
    if checkpoint.is_some() {
        captured.require_checkpoint_acknowledgement();
    }
    let native_accounting = session_id
        .map(|id| manager.has_native_usage_records(id))
        .transpose()?
        .unwrap_or(false)
        || provider_request_receipts
            .iter()
            .any(|receipt| receipt["source_accounting"].as_bool() == Some(true));
    let mut saved = if let Some(id) = session_id.filter(|id| !id.trim().is_empty()) {
        match manager.load_session_snapshot(id) {
            Ok(existing) => {
                let tokens = if native_accounting {
                    existing.metadata.total_tokens
                } else {
                    total_tokens
                };
                session_manager::update_session(existing, messages, tokens, system_prompt.as_ref())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                session_manager::create_saved_session_with_id_and_mode(
                    id.to_string(),
                    messages,
                    model,
                    workspace,
                    if native_accounting { 0 } else { total_tokens },
                    system_prompt.as_ref(),
                    Some("exec"),
                )
            }
            Err(err) => return Err(err).context("could not load existing exec session"),
        }
    } else {
        session_manager::create_saved_session_with_mode(
            messages,
            model,
            workspace,
            total_tokens,
            system_prompt.as_ref(),
            Some("exec"),
        )
    };
    saved.write_receipt = Some(captured);
    stamp_exec_session_metadata(
        &mut saved,
        model,
        provider_route.kind,
        provider_route.id,
        workspace,
    );
    if let Some(error) = turn_failure {
        session_manager::push_turn_outcome(
            &mut saved.turn_outcomes,
            session_manager::SavedTurnOutcome::failed(error, messages.len()),
        );
    }
    if !provider_request_receipts.is_empty() {
        saved.ensure_journal();
        if let Some(journal) = saved.journal.as_mut() {
            let leaf = journal.leaf_id.clone();
            for receipt in provider_request_receipts {
                let label = if receipt
                    .get("wire_observations")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| value.get("image_projection").is_some())
                    }) {
                    "Provider request image projection"
                } else {
                    "Provider request receipt"
                };
                journal.append(codewhale_runtime::session_tree::SessionEntryKind::System {
                    content: format!("{label}: {receipt}"),
                });
                journal.leaf_id.clone_from(&leaf);
            }
        }
    }
    for receipt in provider_request_receipts.iter().filter(|receipt| {
        receipt["primary"].as_bool() == Some(true)
            && receipt["source_accounting"].as_bool() != Some(true)
    }) {
        let Some(source) = receipt["source_id"].as_str() else {
            continue;
        };
        if receipt["usage_reported"].as_bool() == Some(true) {
            saved.metadata.cost.usage_source_fingerprints.insert(
                codewhale_engine::cost_status::usage_source_fingerprint(source),
            );
        }
        let coverage = receipt.get("coverage").cloned().and_then(|value| {
            serde_json::from_value::<codewhale_engine::llm_client::ProviderUsageCoverage>(value)
                .ok()
        });
        if matches!(
            coverage,
            Some(
                codewhale_engine::llm_client::ProviderUsageCoverage::Missing
                    | codewhale_engine::llm_client::ProviderUsageCoverage::Incomplete
            )
        ) && let Some(route) = receipt.get("route").cloned().and_then(|value| {
            serde_json::from_value::<codewhale_engine::cost_status::EffectiveRouteEnvelope>(value)
                .ok()
        }) {
            let reason =
                if receipt["outcome"] == "completed" || receipt["outcome"] == "review_response" {
                    codewhale_engine::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage
                } else {
                    codewhale_engine::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
                };
            let pool = codewhale_engine::cost_status::background_cost_for_runtime_drop(
                &codewhale_engine::cost_status::RuntimeUsageDropRecord {
                    source_id: source.to_string(),
                    route,
                    reason,
                },
            );
            let cost = &mut saved.metadata.cost;
            let pool = codewhale_engine::cost_status::project_missing_usage_ledger(
                &mut cost.missing_usage_sources,
                &mut cost.missing_usage_overflowed,
                &mut cost.unpriced_turns,
                &mut cost.cny_unpriced_turns,
                &pool,
            );
            cost.unpriced_turns = cost.unpriced_turns.saturating_add(pool.unpriced_turns);
            cost.cny_unpriced_turns = cost
                .cny_unpriced_turns
                .saturating_add(pool.cny_unpriced_turns);
            cost.unpriced_reasons
                .extend(pool.unpriced_reasons.into_iter().map(str::to_string));
            cost.cny_unpriced_reasons
                .extend(pool.cny_unpriced_reasons.into_iter().map(str::to_string));
        }
    }
    saved.make_storage_compatible();
    if let Some(checkpoint) = checkpoint {
        checkpoint
            .validate_prefix(
                &saved.metadata.id,
                &saved.metadata.workspace,
                &saved.messages,
            )
            .map_err(anyhow::Error::msg)?;
    }
    manager
        .publish_session(&saved)
        .context("could not save exec session")
}

pub fn stamp_exec_session_metadata(
    saved: &mut session_manager::SavedSession,
    model: &str,
    model_provider_kind: &str,
    model_provider_id: Option<&str>,
    workspace: &Path,
) {
    saved.metadata.model = model.to_string();
    saved
        .metadata
        .set_model_provider_route(model_provider_kind, model_provider_id);
    saved.metadata.workspace = workspace.to_path_buf();
    saved.metadata.mode = Some("exec".to_string());
}

#[derive(serde::Serialize)]
struct ExecToolEntry {
    name: String,
    success: bool,
    output: String,
}

#[derive(serde::Serialize)]
struct ExecOutcome {
    kind: String,
    outcome: String,
    tool_name: String,
    reason: String,
}

#[derive(serde::Serialize, Default)]
struct ExecSummary {
    mode: String,
    provider: String,
    model: String,
    prompt: String,
    output: String,
    tools: Vec<ExecToolEntry>,
    outcomes: Vec<ExecOutcome>,
    status: Option<String>,
    termination_reason: Option<String>,
    error_category: Option<String>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    released_services: Vec<codewhale_engine::tools::shell::PersistentServiceReceipt>,
    /// One-shot (`mode: "one-shot"`) receipt fields kept from the pre-#6510
    /// direct-call path: whether the turn completed without error, and its
    /// provider-reported usage. Absent on agent receipts.
    #[serde(skip_serializing_if = "Option::is_none")]
    success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<codewhale_models::Usage>,
}

impl ExecSummary {
    /// Fill the one-shot receipt fields from the settled turn: success means a
    /// completed status with no error; usage is what the provider reported.
    fn record_one_shot_outcome(&mut self, usage: Option<codewhale_models::Usage>) {
        self.success = Some(self.error.is_none() && self.status.as_deref() == Some("completed"));
        self.usage = usage;
    }
}

pub fn validate_exec_tool_authority_resume(
    tool_authority_json: Option<&str>,
    resuming: bool,
) -> Result<()> {
    if tool_authority_json.is_some() && resuming {
        bail!(
            "Fleet tool authority cannot be combined with exec --resume, --session-id, or --continue"
        );
    }
    Ok(())
}

fn exec_network_policy(
    config: &Config,
    outer_network_access: Option<bool>,
) -> Option<codewhale_net::network_policy::NetworkPolicyDecider> {
    // Fleet caps are an outer authority boundary: user configuration may
    // narrow them further, but it may never widen an explicit network denial.
    if outer_network_access == Some(false) {
        // A Fleet denial is an outer authority the user's document may never
        // widen, so mark the decider authoritative: a mid-session re-read of
        // that document folds onto it instead of replacing it.
        return Some(
            codewhale_net::network_policy::NetworkPolicyDecider::new(
                codewhale_net::network_policy::NetworkPolicy {
                    default: codewhale_net::network_policy::DecisionToml::Deny,
                    ..codewhale_net::network_policy::NetworkPolicy::default()
                },
                None,
            )
            .with_authoritative(),
        );
    }
    config.network.clone().map(|toml_cfg| {
        codewhale_net::network_policy::NetworkPolicyDecider::with_default_audit(
            codewhale_net::network_policy::NetworkPolicy::from(toml_cfg),
        )
    })
}

fn apply_fleet_engine_feature_caps(
    features: &mut codewhale_runtime::features::Features,
    fleet_authority_active: bool,
    outer_network_access: Option<bool>,
    shell_authority: codewhale_engine::tools::spec::ToolShellAuthority,
) {
    if fleet_authority_active {
        features.disable(codewhale_runtime::features::Feature::Subagents);
        features.disable(codewhale_runtime::features::Feature::Mcp);
        if shell_authority != codewhale_engine::tools::spec::ToolShellAuthority::ReadOnly {
            features.disable(codewhale_runtime::features::Feature::ShellTool);
        }
    }
    if outer_network_access == Some(false) {
        features.disable(codewhale_runtime::features::Feature::WebSearch);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn custom_exec_config(active: &str) -> Config {
    let mut custom = std::collections::HashMap::new();
    for (name, base_url, model) in [
        (
            "custom-a",
            "http://127.0.0.1:18181/v1",
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
        ),
        ("custom-b", "http://127.0.0.1:18182/v1", "model-b"),
    ] {
        custom.insert(
            name.to_string(),
            codewhale_runtime::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some(base_url.to_string()),
                model: Some(model.to_string()),
                api_key: Some("local-test-key".to_string()),
                ..Default::default()
            },
        );
    }
    {
        let __cw_config_init_1_0 = Some(active.to_string());
        let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
            custom,
            ..Default::default()
        });
        let mut __cw_config_init_1_value: Config = Default::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    }
}

const TOOL_TEXT_LIMIT: usize = 300;

fn summarize_string_value(text: &str, max_len: usize, count_only: bool) -> String {
    let trimmed = text.trim();
    let len = trimmed.chars().count();
    if count_only || len > max_len {
        return format!("<{len} chars>");
    }
    codewhale_tui_kit::ui_text::truncate_line_to_width(trimmed, max_len)
}

fn summarize_inline_value(value: &Value, max_len: usize, count_only: bool) -> String {
    match value {
        Value::String(s) => summarize_string_value(s, max_len, count_only),
        Value::Array(items) => format!("<{} items>", items.len()),
        Value::Object(map) => format!("<{} keys>", map.len()),
        Value::Bool(b) => b.to_string(),
        Value::Number(num) => num.to_string(),
        Value::Null => "null".to_string(),
    }
}

fn is_noisy_tool_arg_key(key: &str) -> bool {
    matches!(
        key,
        "limit"
            | "max_count"
            | "max_output_tokens"
            | "offset"
            | "page"
            | "page_size"
            | "per_page"
            | "response_length"
            | "timeout_ms"
            | "yield_time_ms"
    )
}

#[must_use]
pub fn summarize_tool_args(input: &Value) -> Option<String> {
    let obj = input.as_object()?;
    if obj.is_empty() {
        return None;
    }

    let mut parts = Vec::new();

    if let Some(value) = obj.get("path") {
        parts.push(format!(
            "path: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("command") {
        parts.push(format!(
            "command: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("query") {
        parts.push(format!(
            "query: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("prompt") {
        parts.push(format!(
            "prompt: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("text") {
        parts.push(format!(
            "text: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("pattern") {
        parts.push(format!(
            "pattern: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("model") {
        parts.push(format!(
            "model: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("profile") {
        parts.push(format!(
            "profile: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("level") {
        parts.push(format!(
            "level: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("file_id") {
        parts.push(format!(
            "file_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("task_id") {
        parts.push(format!(
            "task_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("voice_id") {
        parts.push(format!(
            "voice_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("content") {
        parts.push(format!(
            "content: {}",
            summarize_inline_value(value, 0, true)
        ));
    }

    if parts.is_empty()
        && let Some((key, value)) = obj
            .iter()
            .find(|(key, _)| !is_noisy_tool_arg_key(key.as_str()))
    {
        return Some(format!(
            "{}: {}",
            key,
            summarize_inline_value(value, 80, false)
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

#[must_use]
pub fn summarize_tool_output(output: &str) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(output) {
        if let Some(obj) = json.as_object() {
            if let Some(error) = obj.get("error").or(obj.get("status_msg")) {
                return format!("Error: {}", summarize_inline_value(error, 120, false));
            }

            let mut parts = Vec::new();

            if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("status: {status}"));
            }
            if let Some(message) = obj.get("message").and_then(|v| v.as_str()) {
                parts.push(codewhale_tui_kit::ui_text::truncate_line_to_width(
                    message,
                    TOOL_TEXT_LIMIT,
                ));
            }
            if let Some(task_id) = obj.get("task_id").and_then(|v| v.as_str()) {
                parts.push(format!("task_id: {task_id}"));
            }
            if let Some(file_id) = obj.get("file_id").and_then(|v| v.as_str()) {
                parts.push(format!("file_id: {file_id}"));
            }
            if let Some(url) = obj
                .get("file_url")
                .or_else(|| obj.get("url"))
                .and_then(|v| v.as_str())
            {
                parts.push(format!(
                    "url: {}",
                    codewhale_tui_kit::ui_text::truncate_line_to_width(url, 120)
                ));
            }
            if let Some(data) = obj.get("data") {
                parts.push(format!("data: {}", summarize_inline_value(data, 80, true)));
            }

            if !parts.is_empty() {
                return parts.join(" | ");
            }

            if let Some(content) = obj
                .get("content")
                .or(obj.get("result"))
                .or(obj.get("output"))
            {
                return summarize_inline_value(content, TOOL_TEXT_LIMIT, false);
            }
        }

        return summarize_inline_value(&json, TOOL_TEXT_LIMIT, true);
    }

    codewhale_tui_kit::ui_text::truncate_line_to_width(output, TOOL_TEXT_LIMIT)
}

#[cfg(test)]
mod route_tests {
    use super::*;
    #[test]
    fn windows_config_path_compare_normalizes_mixed_separators() {
        assert_eq!(
            normalize_windows_config_path_str(r"C:\Users\me\repo"),
            normalize_windows_config_path_str(r"C:/Users/me/repo/")
        );
    }

    #[test]
    fn windows_config_path_compare_normalizes_verbatim_and_unc_prefixes() {
        assert_eq!(
            normalize_windows_config_path_str(r"\\?\C:\Users\me\repo"),
            normalize_windows_config_path_str(r"C:/Users/me/repo")
        );
        assert_eq!(
            normalize_windows_config_path_str(r"\\?\UNC\server\share\repo"),
            normalize_windows_config_path_str(r"\\server/share/repo/")
        );
    }

    #[test]
    fn headless_consultant_authority_overrides_network_allow_and_disables_web_search() {
        let config = {
            let __cw_config_init_0_0 = Some(codewhale_config::NetworkPolicyToml {
                default: "allow".to_string(),
                audit: false,
                ..codewhale_config::NetworkPolicyToml::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.network = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        let authority = codewhale_engine::tools::spec::ToolAuthorityEnvelope {
            schema_version: 1,
            owner: "consultant-1".to_string(),
            authority: codewhale_engine::tools::spec::ToolMutationAuthority::ReadOnly,
            network_access: Some(false),
            shell: codewhale_engine::tools::spec::ToolShellAuthority::None,
            verification: codewhale_engine::tools::spec::ToolVerificationAuthority::None,
            writable_roots: Vec::new(),
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
        }
        .normalized()
        .expect("Consultant authority");

        let policy = exec_network_policy(&config, authority.network_access)
            .expect("explicit network=false always installs a policy");
        assert_eq!(
            policy.evaluate("example.com", "web_search"),
            codewhale_net::network_policy::Decision::Deny,
            "the permissive user config must not widen Consultant network authority"
        );
        let mut features = codewhale_runtime::features::Features::default();
        features.enable(codewhale_runtime::features::Feature::ShellTool);
        features.enable(codewhale_runtime::features::Feature::WebSearch);
        apply_fleet_engine_feature_caps(
            &mut features,
            true,
            authority.network_access,
            authority.shell,
        );
        assert!(!features.enabled(codewhale_runtime::features::Feature::WebSearch));
        assert!(!features.enabled(codewhale_runtime::features::Feature::ShellTool));

        let worker_policy = exec_network_policy(&config, Some(true)).expect("configured policy");
        assert_eq!(
            worker_policy.evaluate("example.com", "web_search"),
            codewhale_net::network_policy::Decision::Allow,
            "a network-capable role keeps the configured policy"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn explicit_exec_model_routes_to_unique_authenticated_provider_candidate() {
        let _env_lock = codewhale_engine::test_support::lock_test_env();
        let _zai = codewhale_engine::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let _openrouter = codewhale_engine::test_support::EnvVarGuard::remove("OPENROUTER_API_KEY");
        let config = {
            let __cw_config_init_11_0 = Some("deepseek".to_string());
            let __cw_config_init_11_1 =
                Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
            let mut __cw_config_init_11_value: Config = Default::default();
            __cw_config_init_11_value.provider = __cw_config_init_11_0;
            __cw_config_init_11_value.default_text_model = __cw_config_init_11_1;
            __cw_config_init_11_value
        };

        let route = resolve_cli_auto_route(
            &config,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            "pong",
            None,
        )
        .await
        .expect("explicit GLM should route to the configured Z.ai provider");

        assert_eq!(route.provider.provider, codewhale_config::ProviderKind::Zai);
        assert_eq!(
            route.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );
        assert!(!route.auto_model);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn explicit_exec_model_reports_ambiguous_authenticated_provider_candidates() {
        let _env_lock = codewhale_engine::test_support::lock_test_env();
        let _zai = codewhale_engine::test_support::EnvVarGuard::set("ZAI_API_KEY", "zai-key");
        let _openrouter =
            codewhale_engine::test_support::EnvVarGuard::set("OPENROUTER_API_KEY", "or-key");
        let config = {
            let __cw_config_init_12_0 = Some("deepseek".to_string());
            let __cw_config_init_12_1 =
                Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
            let mut __cw_config_init_12_value: Config = Default::default();
            __cw_config_init_12_value.provider = __cw_config_init_12_0;
            __cw_config_init_12_value.default_text_model = __cw_config_init_12_1;
            __cw_config_init_12_value
        };

        let err = resolve_cli_auto_route(
            &config,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            "pong",
            None,
        )
        .await
        .expect_err("ambiguous GLM route should ask for an explicit provider");
        let message = err.to_string();

        assert!(message.contains("model `GLM-5.2` is available"));
        assert!(message.contains("openrouter"));
        assert!(message.contains("zai"));
        assert!(message.contains("--provider"));
        assert!(message.contains("/provider"));
        assert!(message.contains("/model"));
        assert!(message.contains("/setup"));
    }

    #[tokio::test]
    async fn cli_auto_model_honors_a_fixed_reasoning_preference() {
        let config = {
            let __cw_config_init_13_0 = Some("vllm".to_string());
            let __cw_config_init_13_1 = Some("low".to_string());
            let __cw_config_init_13_2 = Some(codewhale_runtime::config::ProvidersConfig {
                vllm: codewhale_runtime::config::ProviderConfig {
                    base_url: Some("http://127.0.0.1:18190/v1".to_string()),
                    model: Some("local-auto-model".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_13_value: Config = Default::default();
            __cw_config_init_13_value.provider = __cw_config_init_13_0;
            __cw_config_init_13_value.reasoning_effort = __cw_config_init_13_1;
            __cw_config_init_13_value.providers = __cw_config_init_13_2;
            __cw_config_init_13_value
        };

        let route = resolve_cli_auto_route(&config, "auto", "debug a failing test", None)
            .await
            .expect("Auto model route");

        assert!(route.auto_model);
        assert_eq!(
            route.reasoning_effort,
            Some(codewhale_engine::reasoning_preference::ReasoningEffort::Low)
        );
        assert!(
            !route.auto_controls_reasoning,
            "a fixed saved tier must not be replaced per prompt"
        );
    }

    #[test]
    fn cli_route_execution_config_stamps_routed_model_into_provider_slot() {
        let mut providers = codewhale_runtime::config::ProvidersConfig::default();
        providers.deepseek.model = Some("deepseek-v4-pro".to_string());
        let config = {
            let __cw_config_init_14_0 = Some("deepseek".to_string());
            let __cw_config_init_14_1 = Some(providers);
            let mut __cw_config_init_14_value: Config = Default::default();
            __cw_config_init_14_value.provider = __cw_config_init_14_0;
            __cw_config_init_14_value.providers = __cw_config_init_14_1;
            __cw_config_init_14_value
        };
        let route = CliAutoRoute {
            provider: Config::default()
                .test_identity_for_kind(codewhale_config::ProviderKind::Deepseek),
            model: "deepseek-v4-flash".to_string(),
            reasoning_effort: None,
            auto_controls_reasoning: true,
            auto_model: true,
        };

        let execution_config =
            config_for_cli_route(&config, &route).expect("admitted execution route");

        assert_eq!(execution_config.default_model(), "deepseek-v4-flash");
        assert_eq!(
            execution_config
                .provider_config_for(
                    &execution_config
                        .test_identity_for_kind(codewhale_config::ProviderKind::Deepseek)
                )
                .and_then(|entry| entry.model.as_deref()),
            Some("deepseek-v4-flash")
        );
    }

    #[test]
    fn cli_route_execution_config_preserves_the_legacy_literal_custom_route() {
        let _lock = codewhale_engine::test_support::lock_test_env();
        let _source =
            codewhale_engine::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _cli_key = codewhale_engine::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let config = {
            let __cw_config_init_15_0 = Some("custom".to_string());
            let __cw_config_init_15_1 = Some("legacy-model".to_string());
            let mut __cw_config_init_15_value: Config = Default::default();
            __cw_config_init_15_value.provider = __cw_config_init_15_0;
            __cw_config_init_15_value.default_text_model = __cw_config_init_15_1;
            __cw_config_init_15_value
        }
        .with_legacy_root(
            Some("legacy-root-key".to_string()),
            Some("http://127.0.0.1:18183/v1".to_string()),
        );
        let route = CliAutoRoute {
            provider: config
                .active_provider_identity()
                .expect("captured literal root identity"),
            model: "routed-legacy-model".to_string(),
            reasoning_effort: None,
            auto_controls_reasoning: false,
            auto_model: false,
        };

        let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

        // The literal route's top-level fields live in `[providers.custom]`
        // (#6394), and the routed model lands there too.
        assert!(execution.selects_literal_custom_provider());
        assert_eq!(execution.provider.as_deref(), Some("custom"));
        assert_eq!(execution.default_model(), "routed-legacy-model");
        assert_eq!(
            execution.active_route_base_url(),
            "http://127.0.0.1:18183/v1"
        );
        assert_eq!(execution.active_route_api_key().unwrap(), "legacy-root-key");
        for _ in 0..2 {
            let identity = execution
                .resolve_provider_identity("custom")
                .expect("legacy identity remains repeatedly resolvable");
            assert_eq!(identity.key.as_str(), "custom");
        }
        let client = codewhale_engine::client::CodewhaleClient::new(&execution)
            .expect("legacy execution client");
        assert_eq!(client.base_url(), "http://127.0.0.1:18183/v1");
    }

    #[tokio::test]
    async fn forced_exec_route_keeps_custom_provider_when_model_matches_builtin_catalog() {
        let config = custom_exec_config("custom-a");

        let route = resolve_cli_exec_route(
            &config,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            "audit",
            true,
            None,
        )
        .await
        .expect("forced route");
        let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

        assert_eq!(
            route.provider.provider,
            codewhale_config::ProviderKind::Custom
        );
        assert_eq!(
            route.model,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
        );
        assert_eq!(execution.provider.as_deref(), Some("custom-a"));
    }

    #[tokio::test]
    async fn configured_workflow_default_keeps_named_custom_route() {
        let config = custom_exec_config("custom-a");
        let model = config.default_model();

        let route = resolve_cli_exec_route(
            &config,
            &model,
            "Run a checked-in Workflow through the host runtime",
            true,
            None,
        )
        .await
        .expect("configured workflow route");
        let execution = config_for_cli_route(&config, &route).expect("admitted execution route");

        assert_eq!(
            route.provider.provider,
            codewhale_config::ProviderKind::Custom
        );
        assert_eq!(
            execution.active_provider_identity().unwrap().key.as_str(),
            "custom-a"
        );
        assert_eq!(
            execution.active_route_base_url(),
            "http://127.0.0.1:18181/v1"
        );
        let client =
            codewhale_engine::client::CodewhaleClient::new(&execution).expect("workflow client");
        assert_eq!(client.base_url(), "http://127.0.0.1:18181/v1");
    }

    #[test]
    fn exec_json_receipts_keep_exact_named_custom_provider() {
        let config = custom_exec_config("custom-a");
        let identity = config.active_provider_identity().unwrap();
        let provider = identity.key.as_str();
        // #6510: plain exec is an Engine turn now; its `--json` receipt keeps
        // the documented one-shot fields (docs/LIVE_SMOKE.md step 5).
        let mut one_shot = ExecSummary {
            mode: "one-shot".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            output: "done".to_string(),
            status: Some("completed".to_string()),
            ..ExecSummary::default()
        };
        one_shot.record_one_shot_outcome(Some(codewhale_models::Usage {
            input_tokens: 12,
            output_tokens: 3,
            ..Default::default()
        }));
        let one_shot = serde_json::to_value(&one_shot).expect("one-shot receipt");
        assert_eq!(one_shot["mode"], "one-shot");
        assert_eq!(one_shot["provider"], "custom-a");
        assert_eq!(one_shot["model"], "model-a");
        assert_eq!(one_shot["output"], "done");
        assert_eq!(one_shot["success"], true);
        assert_eq!(one_shot["usage"]["input_tokens"], 12);
        assert_eq!(one_shot["usage"]["output_tokens"], 3);

        let mut failed = ExecSummary {
            mode: "one-shot".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            status: Some("failed".to_string()),
            error: Some("Model response incomplete".to_string()),
            ..ExecSummary::default()
        };
        failed.record_one_shot_outcome(None);
        let failed = serde_json::to_value(&failed).expect("failed one-shot receipt");
        assert_eq!(failed["success"], false);
        assert!(
            failed.get("usage").is_none(),
            "no usage is never zero usage"
        );

        let agent = serde_json::to_value(ExecSummary {
            mode: "agent".to_string(),
            provider: provider.to_string(),
            model: "model-a".to_string(),
            ..ExecSummary::default()
        })
        .expect("agent exec JSON receipt");
        assert_eq!(agent["provider"], "custom-a");
        assert!(
            agent.get("success").is_none() && agent.get("usage").is_none(),
            "agent receipts keep their pre-#6510 shape"
        );
        let serialized = serde_json::to_string(&agent).expect("serialize receipt");
        assert!(!serialized.contains("127.0.0.1"));
        assert!(!serialized.contains("local-test-key"));
    }

    #[test]
    fn exec_stream_provider_pair_preserves_named_literal_and_root_custom_provenance() {
        let mut config = Config::default();
        for key in ["lm-studio", "custom"] {
            config
                .providers
                .get_or_insert_with(Default::default)
                .custom
                .insert(
                    key.to_string(),
                    codewhale_runtime::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("http://localhost:1234/v1".to_string()),
                        ..Default::default()
                    },
                );
        }
        let named = config
            .resolve_provider_identity("lm-studio")
            .expect("exact named table");
        let literal = config
            .resolve_provider_identity("custom")
            .expect("exact literal table");
        let root_config = {
            let __cw_config_init_1_0 = Some("custom".to_string());
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value
        }
        .with_legacy_root(None, Some("http://localhost:1234/v1".to_string()));
        let root = root_config
            .active_provider_identity()
            .expect("captured root migration");
        let built_in = Config::default()
            .active_provider_identity()
            .expect("built-in fixture");

        assert_eq!(
            exec_stream_provider_route(&named),
            ("custom".to_string(), Some("lm-studio".to_string()))
        );
        assert_eq!(
            exec_stream_provider_route(&literal),
            ("custom".to_string(), Some("custom".to_string()))
        );
        assert_eq!(
            exec_stream_provider_route(&root),
            ("custom".to_string(), None)
        );
        assert_eq!(
            exec_stream_provider_route(&built_in),
            ("deepseek".to_string(), None)
        );
    }

    #[test]
    fn exec_persistence_omits_id_for_legacy_root_custom_route() {
        let mut saved = session_manager::create_saved_session_with_mode(
            &[],
            "legacy-root-model",
            Path::new("/tmp/exec-root"),
            0,
            None,
            Some("exec"),
        );
        stamp_exec_session_metadata(
            &mut saved,
            "legacy-root-model",
            codewhale_config::ProviderKind::Custom.as_str(),
            None,
            Path::new("/tmp/exec-root"),
        );

        assert_eq!(saved.metadata.model_provider, "custom");
        assert_eq!(saved.metadata.model_provider_id, None);
        assert!(
            !serde_json::to_string(&saved)
                .expect("serialize exec session")
                .contains("model_provider_id")
        );
    }

    #[test]
    fn cli_auto_resolves_to_the_declared_default_before_k3_route_normalization() {
        let config = {
            let __cw_config_init_2_0 = Some("moonshot".to_string());
            let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
                moonshot: codewhale_runtime::config::ProviderConfig {
                    base_url: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL
                            .to_string(),
                    ),
                    model: Some(
                        codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL
                            .to_string(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };

        // #6290 rework: Auto no longer classifies the prompt. Any wording
        // resolves the declared policy tier, normalized for the K3 route.
        assert_eq!(
            cli_reasoning_effort_value_for_prompt(
                &config,
                codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
                codewhale_engine::reasoning_preference::ReasoningEffort::Auto,
            )
            .as_deref(),
            Some("high"),
            "Auto resolves the declared default, not a classification"
        );

        assert_eq!(
            cli_reasoning_effort_value_for_prompt(
                &config,
                codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
                codewhale_engine::reasoning_preference::ReasoningEffort::Off,
            )
            .as_deref(),
            Some("low"),
            "membership K3 still applies its exact-route always-thinking floor"
        );
    }

    #[test]
    fn cli_route_tracks_auto_reasoning_independently_from_auto_model() {
        use codewhale_engine::reasoning_preference::ReasoningEffort;

        let fixed_model_auto_reasoning = CliAutoRoute {
            provider: Config::default()
                .test_identity_for_kind(codewhale_config::ProviderKind::Deepseek),
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            reasoning_effort: Some(ReasoningEffort::Auto),
            auto_controls_reasoning: true,
            auto_model: false,
        };
        let auto_model_fixed_reasoning = CliAutoRoute {
            provider: Config::default()
                .test_identity_for_kind(codewhale_config::ProviderKind::OpenaiCodex),
            model: codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_MODEL.to_string(),
            reasoning_effort: Some(ReasoningEffort::High),
            auto_controls_reasoning: false,
            auto_model: true,
        };

        assert!(fixed_model_auto_reasoning.auto_controls_reasoning);
        assert!(!fixed_model_auto_reasoning.auto_model);
        assert!(!auto_model_fixed_reasoning.auto_controls_reasoning);
        assert!(auto_model_fixed_reasoning.auto_model);
    }

    /// `run_exec_agent` must hand the engine a concrete tier, never the literal
    /// `"auto"` sentinel, for a fixed-model Auto launch.
    #[test]
    fn fixed_model_exec_auto_resolves_to_a_concrete_tier_not_the_auto_sentinel() {
        let config = {
            let __cw_config_init_5_0 = Some("zai".to_string());
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value
        };

        let resolved = cli_reasoning_effort_value_for_prompt(
            &config,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            codewhale_engine::reasoning_preference::ReasoningEffort::Auto,
        )
        .expect("Auto must resolve to a concrete tier");

        assert_ne!(
            resolved, "auto",
            "the literal auto sentinel must never reach a provider"
        );
        assert!(
            matches!(resolved.as_str(), "off" | "low" | "medium" | "high" | "max"),
            "unexpected resolved tier: {resolved}"
        );
    }

    #[test]
    fn fleet_tool_authority_cannot_cross_an_exec_resume_boundary() {
        assert!(validate_exec_tool_authority_resume(None, true).is_ok());
        assert!(validate_exec_tool_authority_resume(Some("{}"), false).is_ok());
        let error = validate_exec_tool_authority_resume(Some("{}"), true)
            .expect_err("authority must remain bound to its fresh Fleet launch")
            .to_string();
        assert!(error.contains("cannot be combined with exec --resume"));
    }

    #[test]
    fn exec_sandbox_denial_stream_event_is_typed() {
        let event = ExecStreamEvent::SandboxDenied {
            tool_id: "call_1".to_string(),
            tool_name: "exec_shell".to_string(),
            reason: "write blocked".to_string(),
            outcome: "approval_required".to_string(),
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&event).expect("serializes"))
                .expect("valid json");
        assert_eq!(value["type"], "sandbox_denied");
        assert_eq!(value["outcome"], "approval_required");
    }

    #[test]
    fn exec_stream_turn_usage_event_serializes_reported_fields() {
        let event = ExecStreamEvent::TurnUsage {
            turn: 2,
            input_tokens: 1200,
            output_tokens: 180,
            reasoning_tokens: Some(90),
            prompt_cache_hit_tokens: Some(900),
            prompt_cache_miss_tokens: Some(300),
            prompt_cache_write_tokens: Some(0),
            reasoning_replay_tokens: Some(40),
            duration_ms: 1834,
            finish_reason: None,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "turn_usage");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["turn"], 2);
        assert_eq!(parsed["input_tokens"], 1200);
        assert_eq!(parsed["output_tokens"], 180);
        assert_eq!(parsed["reasoning_tokens"], 90);
        assert_eq!(parsed["prompt_cache_hit_tokens"], 900);
        assert_eq!(parsed["prompt_cache_miss_tokens"], 300);
        assert_eq!(parsed["prompt_cache_write_tokens"], 0);
        assert_eq!(parsed["reasoning_replay_tokens"], 40);
        assert_eq!(parsed["duration_ms"], 1834);
    }

    #[test]
    fn exec_stream_turn_usage_event_omits_unreported_fields() {
        // Honest absence: optional token fields the provider did not report
        // are dropped from the object entirely — never emitted as null and
        // never backfilled with fabricated zeros.
        let event = ExecStreamEvent::TurnUsage {
            turn: 1,
            input_tokens: 11,
            output_tokens: 3,
            reasoning_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            prompt_cache_write_tokens: None,
            reasoning_replay_tokens: None,
            duration_ms: 250,
            finish_reason: None,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let parsed = value;
        assert_eq!(parsed["type"], "turn_usage");
        assert_eq!(parsed["input_tokens"], 11);
        assert_eq!(parsed["output_tokens"], 3);
        assert_eq!(parsed["duration_ms"], 250);
        let object = parsed.as_object().expect("event object");
        for absent in [
            "reasoning_tokens",
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
            "prompt_cache_write_tokens",
            "reasoning_replay_tokens",
        ] {
            assert!(!object.contains_key(absent), "{absent} leaked: {parsed}");
        }
    }

    #[test]
    fn exec_stream_pre_existing_event_type_tags_are_unchanged() {
        // Contract guard for existing stream consumers (bench harness, fleet
        // ledger): the pre-turn_usage event vocabulary keeps its exact tags.
        let cases: Vec<(ExecStreamEvent, &str)> = vec![
            (
                ExecStreamEvent::Content {
                    content: "hi".to_string(),
                },
                "content",
            ),
            (
                ExecStreamEvent::ToolUse {
                    name: "read_file".to_string(),
                    id: "call_1".to_string(),
                    input: serde_json::json!({}),
                    started_at: "2026-08-03T00:00:00Z".to_string(),
                },
                "tool_use",
            ),
            (
                ExecStreamEvent::ToolResult {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    output: "ok".to_string(),
                    status: "success".to_string(),
                    started_at: "2026-08-03T00:00:00Z".to_string(),
                    completed_at: "2026-08-03T00:00:01Z".to_string(),
                    duration_ms: 1,
                    side_effect_status: "unknown".to_string(),
                    error_category: None,
                    truncated: None,
                    artifact: None,
                    result_metadata: None,
                },
                "tool_result",
            ),
            (
                ExecStreamEvent::SandboxDenied {
                    tool_id: "call_1".to_string(),
                    tool_name: "exec_shell".to_string(),
                    reason: "denied".to_string(),
                    outcome: "approval_required".to_string(),
                },
                "sandbox_denied",
            ),
            (
                ExecStreamEvent::WorkflowEvent {
                    run_id: "workflow_1".to_string(),
                    event: serde_json::json!({"type": "task_completed"}),
                },
                "workflow_event",
            ),
            (
                ExecStreamEvent::SessionCapture {
                    content: "x".to_string(),
                    saved_session_id: "session-x".to_string(),
                },
                "session_capture",
            ),
            (
                ExecStreamEvent::Error {
                    error: "boom".to_string(),
                },
                "error",
            ),
            (ExecStreamEvent::Done, "done"),
        ];

        for (event, expected_type) in cases {
            let value = exec_stream_value(&event).expect("serializes");
            assert_eq!(value["type"], expected_type, "event tag drifted");
            assert_eq!(value["schema"], "codewhale.exec-stream");
            assert_eq!(value["schema_version"], 1);
        }
    }

    #[test]
    fn exec_stream_events_are_json_lines() {
        let event = ExecStreamEvent::ToolResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            output: "line 1\nline 2".to_string(),
            status: "success".to_string(),
            started_at: "2026-07-13T00:00:00Z".to_string(),
            completed_at: "2026-07-13T00:00:01Z".to_string(),
            duration_ms: 1000,
            side_effect_status: "not_started".to_string(),
            error_category: None,
            truncated: Some(false),
            artifact: None,
            result_metadata: None,
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["duration_ms"], 1000);
        assert_eq!(parsed["side_effect_status"], "not_started");
    }

    #[test]
    fn workflow_receipt_stream_event_is_one_json_line() {
        let event = ExecStreamEvent::WorkflowEvent {
            run_id: "workflow_1234".to_string(),
            event: serde_json::json!({
                "type": "handoff_promoted",
                "artifact_id": "workflow_1234:agent_1:review-gate:review_report",
                "gate_id": "review-gate",
                "kind": "review_report",
                "from_role": "reviewer",
                "to_role": "verifier",
                "producer_task_id": "agent_1"
            }),
        };

        let value = exec_stream_value(&event).expect("serializes");
        let json = serde_json::to_string(&value).expect("serializes");
        assert!(!json.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "workflow_event");
        assert_eq!(parsed["schema"], "codewhale.exec-stream");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["run_id"], "workflow_1234");
        assert_eq!(parsed["event"]["type"], "handoff_promoted");
        assert_eq!(
            parsed["event"]["artifact_id"],
            "workflow_1234:agent_1:review-gate:review_report"
        );
        assert_eq!(parsed["event"]["gate_id"], "review-gate");
        assert_eq!(parsed["event"]["kind"], "review_report");
        assert_eq!(parsed["event"]["from_role"], "reviewer");
        assert_eq!(parsed["event"]["to_role"], "verifier");
        assert_eq!(parsed["event"]["producer_task_id"], "agent_1");
        assert!(parsed["event"].get("payload").is_none(), "{parsed}");

        let consumed = ExecStreamEvent::WorkflowEvent {
            run_id: "workflow_1234".to_string(),
            event: serde_json::json!({
                "type": "handoff_consumed",
                "artifact_id": "workflow_1234:agent_1:review-gate:review_report",
                "kind": "review_report",
                "from_role": "reviewer",
                "to_role": "verifier",
                "consumer_task_id": "agent_2"
            }),
        };
        let consumed = exec_stream_value(&consumed).expect("serializes consumed receipt");
        assert_eq!(consumed["type"], "workflow_event");
        assert_eq!(consumed["schema"], "codewhale.exec-stream");
        assert_eq!(consumed["schema_version"], 1);
        assert_eq!(consumed["event"]["type"], "handoff_consumed");
        assert_eq!(
            consumed["event"]["artifact_id"],
            "workflow_1234:agent_1:review-gate:review_report"
        );
        assert_eq!(consumed["event"]["consumer_task_id"], "agent_2");
        assert!(consumed["event"].get("payload").is_none(), "{consumed}");
    }

    #[test]
    fn exec_stream_metadata_redacts_resume_breadcrumbs() {
        let raw_session_id = "abc123fullsecret";
        let event = ExecStreamEvent::Metadata {
            meta: Box::new(ExecStreamMeta {
                receipt_kind: "terminal",
                provider: "deepseek".to_string(),
                provider_id: None,
                model: "deepseek-v4-flash".to_string(),
                route_source: "explicit_or_configured".to_string(),
                input_tokens: Some(123),
                output_tokens: Some(45),
                prompt_cache_hit_tokens: Some(10),
                prompt_cache_miss_tokens: None,
                prompt_cache_write_tokens: None,
                reasoning_tokens: Some(3),
                codewhale_max_output_tokens: Some(384_000),
                codewhale_max_output_tokens_source: Some("documented"),
                duration_ms: 2500,
                retry_count: None,
                approval_posture: "ask".to_string(),
                sandbox_posture: "workspace-write".to_string(),
                binary_sha256: Some("sha256:binary".to_string()),
                config_sha256: None,
                prompt_sha256: "sha256:prompt".to_string(),
                tool_catalog_sha256: Some("sha256:tools".to_string()),
                input_analysis: ExecStreamInputAnalysis::default(),
                visible_final_answer_chars: 17,
                visible_final_answer_excerpt: "the visible reply".to_string(),
                session_id: exec_stream_session_ref(raw_session_id),
                resume_command: exec_stream_resume_hint(raw_session_id),
                workspace: "/tmp/work".to_string(),
                message_count: 4,
                status: Some("completed".to_string()),
                termination_reason: Some("resolved".to_string()),
                error_category: None,
                error: None,
            }),
        };

        let json = serde_json::to_string(&event).expect("serializes");
        assert!(!json.contains('\n'));
        assert!(!json.contains(raw_session_id));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["type"], "metadata");
        assert_ne!(parsed["meta"]["session_id"], raw_session_id);
        assert!(
            parsed["meta"]["session_id"]
                .as_str()
                .unwrap()
                .starts_with("<redacted:")
        );
        assert_eq!(
            parsed["meta"]["resume_command"],
            "codewhale exec --resume <session_capture.saved_session_id>"
        );
        assert_eq!(parsed["meta"]["workspace"], "/tmp/work");
        assert_eq!(parsed["meta"]["message_count"], 4);
        assert_eq!(parsed["meta"]["visible_final_answer_chars"], 17);
        assert_eq!(
            parsed["meta"]["visible_final_answer_excerpt"],
            "the visible reply"
        );

        // Contract (#5946): the raw saved-session id is carried by exactly one
        // field, `session_capture.saved_session_id`. The `metadata` receipt
        // above stays fingerprint-only, and the capture's own `content` keeps
        // the same fingerprint so both surfaces can be correlated in a log.
        let capture = ExecStreamEvent::SessionCapture {
            content: exec_stream_session_ref(raw_session_id),
            saved_session_id: raw_session_id.to_string(),
        };
        let capture_json = serde_json::to_string(&capture).expect("serializes");
        let parsed_capture: serde_json::Value =
            serde_json::from_str(&capture_json).expect("valid json");
        assert_eq!(parsed_capture["type"], "session_capture");
        assert_eq!(parsed_capture["content"], parsed["meta"]["session_id"]);
        assert_ne!(parsed_capture["content"], raw_session_id);
        assert_eq!(parsed_capture["saved_session_id"], raw_session_id);
        assert!(parsed_capture.get("session_id").is_none(), "{capture_json}");
    }

    #[test]
    fn exec_stream_final_answer_text_is_the_last_assistant_reply() {
        // Multi-step turn: pre-tool commentary, a tool result, then a
        // distinct final answer. The receipt must carry only the final
        // reply, not the cumulative stream output.
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "write the report".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "let me check the workspace first".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-1".to_string(),
                    content: "listed files".to_string(),
                    is_error: Some(false),
                    content_blocks: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::thinking("final reasoning"),
                    ContentBlock::Text {
                        text: "the final report".to_string(),
                        cache_control: None,
                    },
                ],
            },
        ];
        assert_eq!(
            exec_stream_final_answer_text(&messages, true),
            Some("the final report".to_string())
        );
    }

    #[test]
    fn exec_stream_final_answer_text_never_reuses_a_resumed_turn_reply() {
        let mut messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "old prompt".into(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "old reply must not be delivered again".into(),
                    cache_control: None,
                }],
            },
        ];
        // The synchronization event can precede acceptance of the new prompt.
        assert_eq!(exec_stream_final_answer_text(&messages, false), None);
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "new prompt that fails before an answer".into(),
                cache_control: None,
            }],
        });
        assert_eq!(exec_stream_final_answer_text(&messages, true), None);
        messages.push(Message {
            role: Role::InterruptedAssistant,
            content: vec![ContentBlock::Text {
                text: "current partial reply".into(),
                cache_control: None,
            }],
        });
        assert_eq!(
            exec_stream_final_answer_text(&messages, true).as_deref(),
            Some("current partial reply")
        );
        // Tool results are user-role records inside this same turn.
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call-current".into(),
                content: "result".into(),
                is_error: None,
                content_blocks: None,
            }],
        });
        assert_eq!(
            exec_stream_final_answer_text(&messages, true).as_deref(),
            Some("current partial reply")
        );
    }

    #[test]
    fn exec_stream_final_answer_text_requires_assistant_text() {
        assert_eq!(exec_stream_final_answer_text(&[], true), None);
        let user_only = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "prompt".to_string(),
                cache_control: None,
            }],
        }];
        assert_eq!(exec_stream_final_answer_text(&user_only, true), None);
        let textless_assistant = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::thinking("reasoning only")],
        }];
        assert_eq!(
            exec_stream_final_answer_text(&textless_assistant, true),
            None
        );
    }

    #[test]
    fn exec_stream_input_analysis_reports_prompt_composition() {
        let system = SystemPrompt::Text("system rules".to_string());
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "run tests".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::thinking("checking context"),
                    ContentBlock::Text {
                        text: "working".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call-1".to_string(),
                        name: "exec_shell".to_string(),
                        input: serde_json::json!({"command": "cargo test"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-1".to_string(),
                    content: "stdout line\nstderr line".to_string(),
                    is_error: Some(false),
                    content_blocks: Some(vec![serde_json::json!({
                        "type": "text",
                        "text": "structured output"
                    })]),
                }],
            },
        ];

        let analysis = exec_stream_input_analysis(&messages, Some(&system));

        assert_eq!(analysis.user_message_count, 2);
        assert_eq!(analysis.assistant_message_count, 1);
        assert_eq!(analysis.tool_message_count, 0);
        assert_eq!(analysis.tool_use_count, 1);
        assert_eq!(analysis.tool_result_count, 1);
        assert_eq!(analysis.thinking_chars, "checking context".chars().count());
        assert!(analysis.text_chars >= "run testsworking".chars().count());
        assert!(analysis.tool_use_input_chars > 0);
        assert!(analysis.tool_result_chars >= "stdout line\nstderr line".chars().count());
        assert!(analysis.estimated_system_tokens > 0);
        assert!(analysis.estimated_message_content_tokens > 0);
        assert!(
            analysis.estimated_request_tokens
                >= analysis.estimated_system_tokens
                    + analysis.estimated_message_content_tokens
                    + analysis.estimated_framing_tokens
        );
    }

    #[test]
    fn exec_text_session_breadcrumbs_use_compact_ids() {
        let session_id = "1234567890abcdef";

        assert_eq!(exec_saved_session_line(session_id), "session: 12345678");
        assert_eq!(
            exec_resumed_session_line(session_id),
            "resumed session: 12345678"
        );
        assert!(!exec_saved_session_line(session_id).contains(session_id));
        assert!(!exec_resumed_session_line(session_id).contains(session_id));
    }
}
