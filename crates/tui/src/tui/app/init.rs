//! Application initialization: `App` construction lives here so the central
//! `app.rs` module holds state and behavior rather than a ~830-line
//! constructor. `App::new` remains a thin test-only shim over
//! [`App::new_with_plugin_registry`]; all callers construct `App` exactly as
//! before.

use super::*;

impl App {
    /// Install the Config owner's current policy and refresh its read-only UI projection.
    pub(crate) fn refresh_notification_settings(&mut self, config: &Config) {
        self.notification_settings = config.notifications_config();
        let _ = crate::tui::notifications::settings(config);
    }

    #[cfg(test)]
    pub fn new(options: TuiOptions, config: &Config) -> Self {
        let workspace = options.workspace.clone();
        Self::new_with_plugin_registry(
            options,
            config,
            std::sync::Arc::new(crate::plugins::PluginRegistry::empty(&workspace)),
        )
    }

    #[allow(clippy::too_many_lines)]
    pub fn new_with_plugin_registry(
        options: TuiOptions,
        config: &Config,
        plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
    ) -> Self {
        let TuiOptions {
            model,
            workspace,
            config_path,
            config_profile,
            allow_shell,
            screen_mode,
            use_mouse_capture,
            mouse_capture_preference,
            use_bracketed_paste,
            max_subagents,
            skills_dir: global_skills_dir,
            memory_path,
            notes_path: _,
            mcp_config_path,
            use_memory,
            start_in_agent_mode,
            skip_onboarding,
            yolo,
            resume_session_id,
            initial_input,
            // Consumed by `run_app` after the App exists, so it can be shown
            // alongside (or instead of) the resume receipt.
            startup_notice: _,
        } = options;

        // Start from disk-only preferences so one-time migrations can never
        // persist terminal/environment overlays such as NO_ANIMATIONS. Apply
        // those overlays only after any normalized settings write succeeds.
        let mut settings = Settings::load_persisted().unwrap_or_else(|_| Settings::default());
        let legacy_yolo_default = settings.legacy_yolo_default_detected();
        let legacy_yolo_full_access = if legacy_yolo_default {
            let control = config.approval_policy_control(
                config_path.as_deref(),
                config_profile.as_deref(),
                &workspace,
            );
            match control {
                crate::config::ApprovalPolicyControl::Unset => {
                    if let Err(error) = normalize_legacy_yolo_settings() {
                        tracing::warn!(
                            "failed to normalize legacy YOLO settings; retrying next launch: {error:#}"
                        );
                    }
                    true
                }
                crate::config::ApprovalPolicyControl::RootConfig => {
                    let active_config_path = match crate::config::resolve_load_config_path(
                        config_path.clone(),
                    ) {
                        Ok(path) => path,
                        Err(error) => {
                            tracing::error!(
                                error = %error,
                                "could not resolve the active config path for legacy policy migration"
                            );
                            None
                        }
                    };
                    match crate::config_persistence::persist_unset_root_key(
                        active_config_path.as_deref(),
                        "approval_policy",
                    ) {
                        Ok(_) => {
                            if let Err(error) = normalize_legacy_yolo_settings() {
                                tracing::warn!(
                                    "removed legacy approval_policy but could not normalize settings; retrying next launch: {error:#}"
                                );
                            }
                            true
                        }
                        Err(error) => {
                            tracing::warn!(
                                "could not migrate legacy YOLO approval policy; keeping the controlling policy: {error:#}"
                            );
                            false
                        }
                    }
                }
                source => {
                    tracing::warn!(
                        "legacy YOLO setting was not allowed to override {}",
                        source.label()
                    );
                    false
                }
            }
        } else {
            false
        };
        settings.apply_env_overrides();
        // Config::load resolves this once for every runtime. Direct in-memory
        // callers use the same policy here, before any startup route is used.
        let mut startup_config = config.clone();
        if config_profile.is_some()
            || (startup_config.remembered_selection_scope.is_none()
                && (crate::config::explicit_launch_provider_override().is_some()
                    || crate::config::explicit_launch_model_override().is_some()))
        {
            startup_config.remembered_selection_scope = Some(false);
        }
        let selected = startup_config.apply_saved_selection(&settings);
        let config = &startup_config;
        // First launch writes `default_text_model = DEFAULT_TEXT_MODEL` into
        // the generated config.toml; that line is the template, not a choice,
        // so it must not turn off local discovery on a later launch.
        let generated_default_model = config.provider.is_none()
            && config.default_text_model.as_deref() == Some(DEFAULT_TEXT_MODEL);
        let startup_route_configured = config.provider.is_some()
            || (config.default_text_model.is_some() && !generated_default_model)
            || config.legacy_model.is_some()
            || crate::config::explicit_launch_provider_override().is_some()
            || crate::config::explicit_launch_model_override().is_some()
            || config
                .active_provider_identity()
                .ok()
                .as_ref()
                .and_then(|identity| config.provider_config_for(identity))
                .is_some_and(|entry| entry.model.is_some())
            || config.active_route_endpoint_configured();
        // Provider and model come from the same resolved config, even on the
        // first run. An options default must not replace a configured model.
        let model = if selected || startup_route_configured {
            config.default_model()
        } else {
            model
        };
        // Tideline Startup is the fresh interactive landing surface. It must
        // not be bypassed by a stale historical `launch_screen = false`, a
        // provider/config notice, or a previous session record: only an
        // intentional resume or explicit initial input enters the live session
        // path directly.
        let launch_visible = resume_session_id.is_none() && initial_input.is_none();
        let launch = LaunchState::new(launch_visible, &workspace);

        // If settings.toml exists on disk but couldn't be parsed (we fell back
        // to defaults), surface a warning in the TUI so the user knows their
        // file is broken instead of silently losing all settings.
        let settings_parse_warning = crate::settings::Settings::path().ok().and_then(|p| {
            if p.exists() {
                std::fs::read_to_string(&p).ok().and_then(|raw| {
                    ::toml::from_str::<::toml::Value>(&raw)
                        .err()
                        .map(|e| format!("⚠ settings.toml is malformed — using defaults ({e})"))
                })
            } else {
                None
            }
        });
        let admission = config.active_provider_identity();
        let admission_error = admission.as_ref().err().cloned();
        let provider_identity = admission.ok();
        // An unadmitted selection remains visible as a refusal; this inert
        // display kind cannot be used as request/config authority.
        let provider = provider_identity
            .as_ref()
            .map_or(ProviderKind::Custom, |identity| identity.provider);
        let mut effective_auth_config = config.clone();

        // #5032: a stale `[providers.xai] oauth_credential_generation` pointer
        // whose owned credential file is gone makes `credentials_valid` return
        // false with no recovery, so the generic provider picker reopened on
        // EVERY launch (the dogfood bricked state). Detect that specific
        // corrupted state, best-effort clear the stale pointer from the
        // persisted config, and surface a truthful xAI-specific message. The
        // repair never blocks or aborts launch; after it the state is the
        // normal "needs auth", not a bricked loop.
        // #5032: an onboarded user whose active xAI OAuth credential is missing
        // must be guided to re-authenticate THAT provider — not be re-run through
        // the generic provider picker on every launch. Detect the missing-cred
        // state (broader than a dangling pointer: it also covers a repaired
        // pointer, an expired/revoked token, or a never-completed login), repair
        // a stale pointer once, surface a truthful xAI message, and suppress the
        // picker-recovery path below.
        let xai_oauth_needs_reauth = provider == ProviderKind::Xai
            && provider_identity
                .as_ref()
                .and_then(|identity| effective_auth_config.provider_config_for(identity))
                .and_then(|entry| entry.auth_mode.as_deref())
                .is_some_and(crate::oauth::auth_mode_uses_xai_oauth)
            && !crate::oauth::credentials_present(
                crate::oauth::OAuthProvider::Xai,
                &effective_auth_config,
            );
        let xai_dangling_repair_message = if xai_oauth_needs_reauth {
            if crate::oauth::owned_generation_is_dangling(
                crate::oauth::OAuthProvider::Xai,
                &effective_auth_config,
            ) {
                match crate::oauth::clear_dangling_generation(
                    crate::oauth::OAuthProvider::Xai,
                    config_path.as_deref(),
                ) {
                    Ok(()) => {
                        // Keep the in-memory route consistent with the repaired
                        // persisted file so the running app never reaches for
                        // the missing generation.
                        if let Some(identity) = provider_identity.as_ref()
                            && let Ok(entry) =
                                effective_auth_config.provider_config_for_mut(identity)
                        {
                            entry.oauth_credential_generation = None;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "codewhale::xai_oauth",
                            error = %error,
                            "could not clear the dangling xAI OAuth generation pointer; continuing launch"
                        );
                    }
                }
            }
            Some(
                "⚠ xAI OAuth credentials are missing. Re-authenticate with \
                 `codewhale auth xai-device` or the in-app login, or switch providers."
                    .to_string(),
            )
        } else {
            None
        };
        let model_ids_passthrough = effective_auth_config.model_ids_pass_through();
        let provider_chain = provider_identity
            .as_ref()
            .filter(|identity| {
                identity.key.as_str() == identity.provider.as_str()
                    && identity.provider != ProviderKind::Antigravity
            })
            .map(|identity| ProviderChain::new(identity.provider, &config.fallback_providers))
            .filter(|chain| chain.providers().len() > 1);
        let provider_readiness = provider_chain
            .as_ref()
            .map(|chain| {
                chain
                    .providers()
                    .iter()
                    .filter_map(|kind| config.builtin_provider_identity(*kind).ok())
                    .map(|identity| {
                        let ready = has_api_key_for(config, &identity);
                        (identity, ready)
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Check if the effective provider has an API key. This must happen
        // after settings.default_provider is applied; otherwise a saved
        // third-party provider can be pushed back into DeepSeek onboarding.
        let needs_api_key = !has_api_key(&effective_auth_config);
        let api_key_env_only =
            crate::config::active_provider_uses_env_only_api_key(&effective_auth_config);
        let was_onboarded = crate::tui::onboarding::is_onboarded();
        let settings_auto_compact = settings.auto_compact;
        let auto_compact_user_configured = Settings::auto_compact_explicitly_configured();
        let auto_compact_threshold_percent = settings.auto_compact_threshold_percent;
        let compaction_summary_instructions = config.compaction_summary_instructions();
        let compaction_retained_user_message_tokens =
            config.compaction_retained_user_message_tokens();
        let calm_mode = settings.calm_mode;
        let low_motion = settings.low_motion;
        let constrained_frame_rate = settings.constrained_frame_rate;
        let fancy_animations = settings.fancy_animations;
        let focus_texture =
            crate::tui::focus_texture::FocusTextureMode::parse(&settings.focus_texture)
                .unwrap_or_default();
        let work_surface_placement =
            crate::tui::work_surface::WorkSurfacePlacement::parse(&settings.work_surface_placement);
        let work_surface_top_height = settings.work_surface_top_height;
        let work_surface_side_width = settings.work_surface_side_width;
        let synchronized_output_enabled = settings.synchronized_output_enabled();
        let status_indicator = settings.status_indicator.clone();
        let show_thinking = settings.show_thinking;
        let thinking_highlight = settings.thinking_highlight;
        let thinking_default_expanded = settings.thinking_default_expanded;
        let thinking_preview_lines = settings.thinking_preview_lines;
        let help_expand_groups = settings.help_expand_groups;
        let pin_last_prompt = settings.pin_last_prompt;
        let show_tool_details = settings.show_tool_details;
        let inline_diff_mode = InlineDiffMode::parse(&settings.inline_diffs);
        let ui_locale = resolve_locale(&settings.locale);
        // The dead `tui.toml` store was folded into settings.toml on load.
        // Say so once, in the user's language, rather than letting a theme
        // move under them unexplained.
        let tui_prefs_migration_notice = settings
            .tui_prefs_migration()
            .map(|receipt| receipt.lines(ui_locale).join(" "))
            .filter(|line| !line.is_empty());
        let cost_currency = match (settings.cost_currency.as_str(), ui_locale.tag()) {
            ("usd", "zh-Hans") => CostCurrency::Cny,
            _ => CostCurrency::from_setting(&settings.cost_currency).unwrap_or(CostCurrency::Usd),
        };
        let composer_density = ComposerDensity::from_setting(&settings.composer_density);
        let composer_border = settings.composer_border;
        let composer_multiline_mode = settings.composer_multiline_mode;
        let composer_vim_enabled = settings
            .composer_vim_mode
            .trim()
            .eq_ignore_ascii_case("vim");
        let transcript_spacing = TranscriptSpacing::from_setting(&settings.transcript_spacing);
        let max_input_history = settings.max_input_history;
        // Requesting bracketed paste does not prove the terminal delivers it.
        // Keep the fallback until handle_paste_burst_key observes a real paste
        // via bracketed_paste_seen; otherwise raw pasted newlines can submit.
        let use_paste_burst_detection = settings.paste_burst_detection;
        // Resolve the named theme from settings; unknown values were already
        // normalised to the underwater default in Settings::load. The
        // background_color setting still overlays on top.
        let background_color_override = settings
            .background_color
            .as_deref()
            .and_then(palette::parse_hex_rgb_color);
        let background_setting = background_color_override.and_then(palette::hex_rgb_string);
        let resolved_theme =
            palette::resolve_theme_setting(&settings.theme, background_setting.as_deref());
        let theme_warning = resolved_theme.as_ref().err().map(|error| {
            format!(
                "⚠ configured theme '{}' could not be loaded — using System ({error})",
                settings.theme
            )
        });
        let (theme_name, theme_id, ui_theme) = resolved_theme.unwrap_or_else(|_| {
            let id = palette::ThemeId::System;
            let mut theme = id.ui_theme();
            if let Some(background) = background_color_override {
                theme = theme.with_background_color(background);
            }
            (id.name().to_string(), id, theme)
        });
        // Remembered route choices were resolved once into Config. The
        // chooser and hotbar must not revive archived Settings values.
        let mut provider_models = HashMap::new();
        for identity in config.provider_identities() {
            if let Some(model) = config
                .provider_config_for(&identity)
                .and_then(|entry| entry.model.as_ref())
            {
                provider_models.insert(identity.key.to_string(), model.clone());
            }
        }
        if let Some(identity) = &provider_identity {
            provider_models.insert(identity.key.to_string(), model.clone());
        }
        let auto_model = model.trim().eq_ignore_ascii_case("auto");
        // `settings.toml [enabled_models]` is no longer read (#6533): the
        // picker ranks by use, which this index derives from saved sessions.
        let route_usage = crate::model_relevance::SharedRouteUsage::default();
        crate::model_relevance::spawn_build(route_usage.clone());
        let active_context_window_override = provider_identity
            .as_ref()
            .and_then(|identity| config.context_window_for_provider_config(identity));
        let active_model_context_windows = provider_identity
            .as_ref()
            .and_then(|identity| config.model_context_windows_for(identity).cloned());
        let configured_route_base_url = effective_auth_config.active_route_base_url();
        let (active_route_limits, active_route_base_url, active_context_window_source) =
            if auto_model {
                (
                    active_context_window_override.map(|window| RouteLimits {
                        context_tokens: Some(u64::from(window)),
                        ..RouteLimits::default()
                    }),
                    configured_route_base_url,
                    if active_context_window_override.is_some() {
                        crate::route_runtime::ContextWindowSource::Configured
                    } else {
                        crate::route_runtime::ContextWindowSource::Fallback
                    },
                )
            } else {
                provider_identity
                    .as_ref()
                    .ok_or_else(|| "provider identity unavailable".to_string())
                    .and_then(|identity| {
                        crate::route_runtime::resolve_runtime_route_for_identity(
                            &effective_auth_config,
                            identity,
                            Some(&model),
                        )
                    })
                    .map(|resolution| {
                        (
                            crate::route_budget::known_route_limits(resolution.candidate.limits()),
                            resolution.candidate.endpoint().base_url.clone(),
                            resolution.context_window.source,
                        )
                    })
                    .unwrap_or((
                        None,
                        configured_route_base_url,
                        crate::route_runtime::ContextWindowSource::Fallback,
                    ))
            };
        let reasoning_effort_explicit = config.fleet_operator_reasoning_applied
            || settings.reasoning_effort.is_some()
            || config.reasoning_effort_is_explicit();
        let configured_reasoning_effort = if config.fleet_operator_reasoning_applied {
            config.reasoning_effort()
        } else {
            settings
                .reasoning_effort
                .as_deref()
                .or_else(|| config.reasoning_effort())
        };
        let reasoning_effort_preference = configured_reasoning_effort
            .filter(|_| reasoning_effort_explicit)
            .map(ReasoningEffort::from_setting);
        let threshold_model = if auto_model {
            DEFAULT_TEXT_MODEL
        } else {
            model.as_str()
        };
        let compact_threshold = crate::route_budget::compaction_threshold_for_route_at_percent(
            provider,
            threshold_model,
            active_route_limits,
            auto_compact_threshold_percent,
        );
        let auto_compact = if auto_compact_user_configured {
            settings_auto_compact
        } else {
            crate::route_budget::auto_compact_default_for_route(
                provider,
                threshold_model,
                active_route_limits,
            )
        };
        let mut reasoning_effort = if auto_model && !reasoning_effort_explicit {
            // A retired fixed-model alias can infer a compatibility effort in
            // Config. That is route metadata, not an explicit user preference,
            // so it must not silently constrain unresolved auto routing.
            ReasoningEffort::Auto
        } else {
            configured_reasoning_effort.map_or_else(
                || {
                    if auto_model {
                        ReasoningEffort::Auto
                    } else {
                        ReasoningEffort::default()
                    }
                },
                |setting| {
                    if auto_model {
                        ReasoningEffort::from_setting(setting)
                    } else {
                        ReasoningEffort::from_setting_for_provider(setting, provider)
                    }
                },
            )
        };
        if !auto_model
            && !reasoning_effort_explicit
            && let Some(effort) = crate::config::legacy_deepseek_alias_effort_for_route(
                provider,
                &effective_auth_config.active_route_base_url(),
                &model,
            )
        {
            reasoning_effort = ReasoningEffort::from_setting_for_provider(effort, provider);
        }
        if !auto_model
            && crate::config::is_exact_direct_moonshot_k3_route(
                provider,
                &active_route_base_url,
                &model,
            )
        {
            // Keep the visible/effective tier truthful on first launch too;
            // direct K3 cannot honor a persisted `off` setting.
            reasoning_effort =
                reasoning_effort.normalize_for_route(provider, &active_route_base_url, &model);
        } else if !auto_model && !reasoning_effort_explicit {
            if let Some(default) =
                ReasoningEffort::catalog_default(provider, &active_route_base_url, &model)
            {
                reasoning_effort = default;
            }
        } else if !auto_model
            && ReasoningEffort::catalog_effort_values(provider, &active_route_base_url, &model)
                .is_some()
        {
            reasoning_effort =
                reasoning_effort.normalize_for_route(provider, &active_route_base_url, &model);
        }

        // Resolve the saved mode separately from the permission posture.
        let preferred_mode = AppMode::from_setting(&settings.default_mode);
        // Legacy `default_mode = "yolo"` was split into Act plus the
        // full-access posture at the settings edge, so only the CLI flag
        // requests the compat elevation here.
        let yolo_requested = yolo;
        let initial_mode = if yolo_requested || start_in_agent_mode {
            AppMode::Agent
        } else {
            preferred_mode
        };

        // Durable Agent-era permission baseline (#3386). Plan/YOLO derive from
        // and restore to this. When the user starts in YOLO the live shell
        // flag is force-enabled below, so
        // the baseline shell value is taken from the interactive default (the
        // pre-mode Agent surface) rather than the YOLO-forced live mirror;
        // otherwise it mirrors the resolved `allow_shell` option, which already
        // carries that same interactive default. Using `interactive_allow_shell()`
        // here keeps the Agent baseline identical regardless of launch mode, so
        // a YOLO -> Agent downshift exposes shell (approval-gated) exactly as
        // documented, while an explicit `allow_shell = false` still hides it.
        // Trust is never part of the Agent baseline (it is YOLO-only authority).
        // Approval mirrors the configured policy.
        let explicit_approval_mode = (!legacy_yolo_full_access)
            .then_some(config.approval_policy.as_deref())
            .flatten()
            .and_then(ApprovalMode::from_config_value);
        let approval_policy_control = if legacy_yolo_full_access {
            ApprovalPolicyControl::Unset
        } else {
            config.approval_policy_control(
                config_path.as_deref(),
                config_profile.as_deref(),
                &workspace,
            )
        };
        let approval_policy_locked = approval_policy_control != ApprovalPolicyControl::Unset;
        let approval_policy_root_editable =
            approval_policy_control == ApprovalPolicyControl::RootConfig;
        let approval_policy_requirements_managed =
            approval_policy_control == ApprovalPolicyControl::Requirements;
        let shell_access_editable = config
            .allow_shell_control(
                config_path.as_deref(),
                config_profile.as_deref(),
                &workspace,
            )
            .editable_root();
        // YOLO is a permission change. A locked policy must not be sidestepped
        // by --yolo, default_mode=yolo, /zidong, or Alt+Y.
        let yolo_compat = yolo_requested && !approval_policy_locked;
        let needs_workspace_trust = !yolo_compat && crate::config::needs_trust(&workspace);
        // The language screen is required only when the locale cannot be
        // confidently inferred from settings or the environment; returning
        // users never see it.
        let onboarding_needs_language = !was_onboarded
            && !crate::tui::onboarding::locale_confidently_inferred(&settings.locale);
        // Suppress the missing-key provider picker for the xAI-OAuth-missing-
        // credential case: the user already chose xAI and just needs to
        // re-authenticate it, not re-pick a provider every launch.
        let (onboarding, onboarding_missing_key_recovery) = launch_onboarding_decision(
            skip_onboarding,
            was_onboarded,
            onboarding_needs_language,
            needs_api_key,
            needs_workspace_trust,
            xai_oauth_needs_reauth,
        );
        let onboarding_workspace_trust_gate = onboarding_is_workspace_trust_gate(
            skip_onboarding,
            was_onboarded,
            needs_api_key,
            needs_workspace_trust,
        );
        let saved_permission_posture = if approval_policy_locked {
            None
        } else {
            settings
                .permission_posture
                .as_deref()
                .and_then(ApprovalMode::from_config_value)
        };
        let configured_approval_mode = explicit_approval_mode
            .or(saved_permission_posture)
            .unwrap_or_default();
        let configured_trust_mode = configured_approval_mode == ApprovalMode::Bypass;
        let mode_prefs = ModeSessionPrefs {
            agent_allow_shell: if yolo_compat {
                config.interactive_allow_shell()
            } else {
                allow_shell
            },
            agent_trust_mode: configured_trust_mode,
            // The YOLO-compat launch elevates the *live* approval mirror to
            // Bypass below; the durable Agent baseline keeps the configured
            // policy so a YOLO -> Agent downshift restores it.
            agent_approval_mode: configured_approval_mode,
        };
        let allow_shell = if yolo_compat {
            allow_shell || shell_access_editable
        } else {
            allow_shell
        };
        let shell_manager = new_shared_shell_manager(workspace.clone());

        for error in crate::commands::user_registry::install_plugin_registry(
            &workspace,
            plugin_registry.as_ref(),
        ) {
            tracing::warn!(target: "plugins", "{error}");
        }

        // Initialize hooks executor from config, reviewed plugin snapshots,
        // then project-local `.codewhale/hooks.toml` (#3026).
        let hooks_config = crate::hooks::HooksConfig::load_with_project_and_plugins(
            config.hooks_config(),
            &workspace,
            Some(plugin_registry.as_ref()),
        );
        let hooks = HookExecutor::new(hooks_config, workspace.clone());

        // Initialize the lifecycle event outbox (`[lifecycle_outbox]`).
        // Disabled (all emits no-op) when the config has no path.
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

        // Initialize plan state
        let plan_state = new_shared_plan_state();
        let todos = new_shared_todo_list();
        let work_runtime =
            crate::work_graph::new_shared_work_runtime(todos.clone(), plan_state.clone());

        let skills_discovery_mode =
            crate::skills::SkillDiscoveryMode::from_config(&config.skills_config());
        let skills_dir = resolve_skills_dir(&workspace, &global_skills_dir, config);
        let cached_skills = Self::discover_cached_skills(
            &workspace,
            &skills_dir,
            skills_discovery_mode,
            plugin_registry.as_ref(),
        );

        // The recall cap applies from the first keystroke, not only after the
        // first submit: the persisted file keeps its own larger cap (U01-m2).
        let mut input_history = crate::composer_history::load_history();
        input_history.drain(..input_history.len().saturating_sub(max_input_history));
        let mention_cwd = std::env::current_dir().ok();
        let start_remote_control = matches!(initial_input, Some(InitialInput::RemoteControl));
        let (initial_input_text, initial_input_cursor, auto_submit_initial_input) =
            match initial_input {
                // #451: pre-populate the composer when invoked via
                // `deepseek pr <N>` (or any future caller that wants to
                // drop the model into a session with context already
                // typed). Cursor lands at the end so Enter sends as-is.
                Some(InitialInput::Prefill(text)) if !text.is_empty() => {
                    let cursor = text.chars().count();
                    (text, cursor, false)
                }
                Some(InitialInput::Submit(text)) if !text.is_empty() => {
                    let cursor = text.chars().count();
                    (text, cursor, true)
                }
                Some(InitialInput::RemoteControl) => (String::new(), 0, false),
                _ => (String::new(), 0, false),
            };
        let (mcp_configured_count, mcp_connecting) =
            crate::mcp::load_config_with_workspace_and_plugins(
                &mcp_config_path,
                &workspace,
                plugin_registry.as_ref(),
            )
            .map(|cfg| {
                // The pre-event "connecting" prediction names only servers a
                // session must start — `required` ones plus those the user's
                // `tools.always_load` selection covers — not every server the
                // boot pass starts. The engine's first boot event replaces
                // this with the real in-flight set.
                let requested = config
                    .tools
                    .as_ref()
                    .map(|tools| {
                        tools
                            .always_load
                            .iter()
                            .map(|name| name.trim().to_ascii_lowercase())
                            .filter(|name| name.starts_with("mcp_"))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut connecting = cfg
                    .servers
                    .iter()
                    .filter(|(_, server)| server.is_enabled())
                    .filter(|(name, server)| {
                        server.required
                            || crate::mcp::tool_selection_covers_server(&requested, name)
                    })
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                connecting.sort();
                (cfg.servers.len(), connecting)
            })
            .unwrap_or((0, Vec::new()));
        let mut hotbar_actions = HotbarActionRegistry::with_configured_routes(
            config,
            provider_identity.as_ref(),
            &model,
            &provider_models,
        );
        // #2069: expose the already-discovered skills as bindable hotbar
        // actions. Reuses the startup skill cache, so no extra filesystem I/O.
        hotbar_actions.register_skills(&cached_skills);
        let composer_arrows_scroll_explicit = config
            .tui
            .as_ref()
            .and_then(|tui| tui.composer_arrows_scroll)
            .is_some();
        let mut app = Self {
            mode: initial_mode,
            hotbar_actions,
            composer: ComposerState {
                input: initial_input_text,
                cursor_position: initial_input_cursor,
                kill_buffer: String::new(),
                paste_burst: PasteBurst::default(),
                pending_paste_reference: None,
                oversized_paste_full_text: None,
                input_history,
                draft_history: VecDeque::new(),
                clear_undo_buffer: None,
                history_index: None,
                history_navigation_draft: None,
                composer_history_search: None,
                selected_attachment_index: None,
                slash_menu_selected: 0,
                slash_menu_hidden: false,
                mention_menu_selected: 0,
                mention_menu_hidden: false,
                mention_completion_cache: None,
                mention_discovery: crate::tui::mention_completion::MentionDiscovery::default(),
                mention_cwd,
                vim_enabled: composer_vim_enabled,
                vim_mode: VimMode::Normal,
                vim_pending_d: false,
                selection_anchor: None,
                // Seeded text was not typed, so it makes no command claim;
                // startup integrity is decided by the replay receipt (#5925).
                line_began_with_slash: false,
                startup_input_unproven: false,
            },
            viewport: ViewportState {
                selection_copy_markdown: config
                    .tui
                    .as_ref()
                    .and_then(|tui| tui.selection_copy_markdown)
                    .unwrap_or(true),
                ..ViewportState::default()
            },
            pet_watch: crate::tui::pet_watch::PetWatch::default(),
            work_surface: {
                let mut state = crate::tui::work_surface::WorkSurfaceState::with_layout(
                    work_surface_placement,
                    work_surface_top_height,
                    work_surface_side_width,
                );
                state.panel = crate::tui::work_surface::RailPanel::parse(&settings.rail_panel);
                state
            },
            goal: HostGoalState::default(),
            session: SessionState::default(),
            last_billed_input_tokens: None,
            last_compaction: None,
            active_allowed_tools: None,
            pausable: false,
            pending_route_save: None,
            paused: false,
            paused_goal_objective: None,
            history: Vec::new(),
            history_version: 0,
            details_epoch: 0,
            transcript_identity_epoch: 0,
            history_revisions: Vec::new(),
            tool_run_cache: ToolRunCache::default(),
            next_history_revision: 1,
            api_messages: Arc::new(Vec::new()),
            api_message_stamps: Vec::new(),
            session_journal: crate::session_tree::SessionJournal::new(),
            completed_assistant_outputs: Vec::new(),
            pending_plan_handoff: None,
            context_token_cache: std::cell::RefCell::new(Default::default()),
            remote_control: crate::remote_control::RemoteControlController::default(),
            start_remote_control_on_launch: start_remote_control,
            is_loading: false,
            feedback_dispatch: None,
            dispatch_completion_tx: None,
            dispatch_in_flight: false,
            dispatch_cancel: None,
            last_enter_instant: None,
            provider_wait_incident_logged: false,
            prompt_suggestion: None,
            notification_settings: config.notifications_config(),
            prompt_suggestion_gen: std::sync::atomic::AtomicU64::new(0),
            offline_mode: false,
            turn_error_posted: false,
            turn_error_notice: None,
            // Surface parse warnings so the user knows their config file is
            // broken instead of silently losing all settings.
            status_message: admission_error
                .or(xai_dangling_repair_message)
                .or(settings_parse_warning)
                .or(tui_prefs_migration_notice)
                .or(theme_warning),
            status_toasts: VecDeque::new(),
            update_available: None,
            sticky_status: None,
            last_status_message_seen: None,
            context_pressure_warning_dismissed: None,
            plugin_reload_nudge_stamp: None,
            last_plugin_catalog_poll: None,
            plugin_cta: crate::tui::plugin_suggestions::PluginCtaState::from_settings(&settings),
            model,
            provider_models,
            route_usage,
            configured_models: config.custom_models.clone().unwrap_or_default(),
            pinned_models: settings.pinned_models.clone(),
            auto_model,
            last_effective_model: None,
            last_effective_provider: None,
            last_effective_provider_identity: None,
            last_auto_route_receipt: None,
            pending_turn_route: None,
            pending_auto_route_receipt: None,
            active_turn: None,
            api_provider: provider,
            provider_identity: provider_identity.clone(),
            provider_chain,
            provider_readiness,
            provider_health: crate::provider_readiness::ProviderReadinessSnapshot::default(),
            last_fallback_reason: None,
            model_ids_passthrough,
            active_route_limits,
            active_route_base_url,
            active_context_window_source,
            active_context_window_override,
            active_model_context_windows,
            pending_provider_switch: None,
            reasoning_effort,
            reasoning_effort_preference,
            last_effective_reasoning_effort: None,
            workspace,
            workflow_config: config.workflow_config(),
            goal_max_continuations: config.goal_max_continuations(),
            goal_enforce_token_budget: config.goal_enforce_token_budget(),
            goal_continuation_waiting: false,
            configured_sandbox_mode: config.sandbox_mode.clone(),
            configured_sandbox_network: config.sandbox_network_access,
            sandbox_backend: crate::sandbox::get_platform_sandbox_with_bwrap_preference(
                config.prefers_bwrap(),
            ),
            // #4022: the worker thread is spawned lazily on first submit, so
            // constructing an App never costs a thread.
            lane_control: crate::lane_control::LaneControlQueue::new(),
            plugin_registry,
            config_path,
            config_profile,
            legacy_plugin_tools_dir: config
                .tools
                .as_ref()
                .and_then(|tools| tools.plugin_dir.as_deref())
                .map(PathBuf::from),
            mcp_config_path: mcp_config_path.clone(),
            skills_dir,
            skills_discovery_mode,
            project_context_pack_enabled: config.project_context_pack_enabled(),
            memory_path,
            use_memory,
            screen_mode,
            use_mouse_capture,
            mouse_capture_preference,
            use_bracketed_paste,
            use_paste_burst_detection,
            bracketed_paste_seen: false,
            bracketed_paste_trusted: crate::tui::paste::terminal_delivers_bracketed_paste(),
            system_prompt: None,
            auto_compact,
            auto_compact_user_configured,
            auto_compact_threshold_percent,
            compaction_summary_instructions,
            compaction_retained_user_message_tokens,
            stopped_turn: false,
            calm_mode,
            low_motion,
            constrained_frame_rate,
            ambient_clock_ms: 0,
            ambient_clock_sampled_at: None,
            ambient_idle_since: None,
            ocean_completion_started_at: None,
            ocean_turn_history_start: 0,
            ocean_receipt_settle_start: None,
            fancy_animations,
            focus_texture,
            launch,
            pending_launch_action: None,
            pending_claude_login: None,
            pending_composer_submit: None,
            pending_hotbar_slot: None,
            synchronized_output_enabled,
            status_indicator,
            show_thinking,
            thinking_highlight,
            thinking_default_expanded,
            thinking_preview_lines,
            help_expand_groups,
            pin_last_prompt,
            verbose_transcript: false,
            show_tool_details,
            inline_diff_mode,
            ui_locale,
            constitution_reviewed_digest: None,
            cost_currency,
            billing_presentation: provider_identity.as_ref().map_or(
                crate::route_billing::BillingPresentation::Unknown,
                |identity| crate::route_billing::for_route(config, identity),
            ),
            composer_density,
            composer_border,
            composer_multiline_mode,
            voice_enabled: false,
            voice_send_enabled: false,
            voice_control_enabled: false,
            transcript_spacing,
            sidebar_hover: SidebarHoverState::default(),
            sidebar_hover_tooltip: None,
            model_picker_memory: None,
            provider_picker_memory: None,
            last_mouse_pos: None,
            context_panel: settings.context_panel,
            sessions_rail: settings.sessions_rail,
            tool_collapse_threshold: 3,
            expanded_tool_runs: HashSet::new(),
            tool_collapse_mode: ToolCollapseMode::from_setting(&settings.tool_collapse_mode),
            file_tree: None,
            file_tree_visible: false,
            compact_threshold,
            max_input_history,
            allow_shell,
            verbosity: config.verbosity.clone(),
            max_subagents,
            stream_chunk_timeout_secs: config.stream_chunk_timeout_secs(),
            subagent_cache: Vec::new(),
            subagent_terminal_seen_at: HashMap::new(),
            agent_progress: HashMap::new(),
            agent_progress_meta: HashMap::new(),
            subagent_card_index: HashMap::new(),
            last_fanout_card_index: None,
            pending_subagent_dispatch: None,
            agent_activity_started_at: None,
            agent_counter: 0,
            agent_label_map: HashMap::new(),
            background_finished: Vec::new(),
            finished_shell_ids: HashMap::new(),
            notified_shell_ids: HashSet::new(),
            notified_task_ids: HashSet::new(),
            subagent_cache_received_at: None,
            agent_focus: None,
            agent_queued_follow_ups: HashMap::new(),
            last_agent_progress_redraw: None,
            last_workflow_budget_redraw: None,
            ui_theme,
            background_color_override,
            theme_id,
            theme_name,
            onboarding,
            redaction_gate: false,
            redaction_gate_confirming: false,
            redaction_gate_scroll: std::cell::Cell::new(0),
            onboarding_needs_api_key: needs_api_key,
            startup_route_configured,
            onboarding_provider: provider,
            onboarding_workspace_trust_gate,
            onboarding_missing_key_recovery,
            onboarding_key_rejected: None,
            onboarding_explore_offline: false,
            // Language is asked in /setup, never at launch: counting it here
            // made the one launch screen read "Getting started · 2/3" with no
            // step 1 in sight (#6566).
            onboarding_had_language_step: onboarding_needs_language
                && onboarding == OnboardingState::Language,
            onboarding_had_provider_step: !was_onboarded && needs_api_key,
            onboarding_had_trust_step: !was_onboarded && needs_workspace_trust,
            local_runtime_detected: false,
            api_key_env_only,
            hooks,
            lifecycle_outbox,
            yolo: yolo_compat,
            yolo_compat_notified: false,
            startup_defaults: Default::default(),
            keybinding_migration_notified: false,
            mode_prefs,
            approval_policy_locked,
            approval_policy_root_editable,
            approval_policy_requirements_managed,
            shell_access_editable,
            clipboard: ClipboardHandler::new(),
            approval_session_approved: HashSet::new(),
            approval_session_denied: HashSet::new(),
            approval_mode: if yolo_compat {
                ApprovalMode::Bypass
            } else {
                configured_approval_mode
            },
            view_stack: ViewStack::new(),
            pending_user_input_prompt: None,
            pending_child_requests: std::collections::BTreeMap::new(),
            child_agent_sessions: std::collections::HashMap::new(),
            backtrack: crate::tui::backtrack::BacktrackState::new(),
            current_session_id: None,
            offline_queue_lease: None,
            last_known_work_state: None,
            last_known_goal_state: None,
            pending_goal_controls: VecDeque::new(),
            current_session_metadata: None,
            session_artifacts: Vec::new(),
            session_turn_outcomes: Vec::new(),
            trust_mode: yolo_compat || configured_trust_mode,
            translation_enabled: false,
            mini_window: config.mini_window.clone().unwrap_or_default(),
            status_items: config
                .tui
                .as_ref()
                .and_then(|tui| tui.status_items.clone())
                .unwrap_or_else(crate::config::StatusItem::default_footer),
            posture_bar: config
                .tui
                .as_ref()
                .and_then(|tui| tui.posture_bar)
                .unwrap_or_default(),
            metrics_line: config
                .tui
                .as_ref()
                .and_then(|tui| tui.metrics_line)
                .unwrap_or(crate::config::ChromeRowPreset::Compact),
            // Prose wrap cap (`[transcript] prose_measure`, #5436). Resolved
            // once here so every render pass — main cache and full-screen
            // overlay — shares one effective width; `None` = full width.
            prose_measure: config.prose_measure(),
            project_doc: None,
            plan_state,
            todos,
            runtime_services: RuntimeToolServices {
                shell_manager: Some(shell_manager),
                work: Some(work_runtime),
                media_originals_dir: crate::media_originals::default_store_dir(),
                ..RuntimeToolServices::default()
            },
            coordination_detail: None,
            mcp_snapshot: None,
            mcp_initializing: !mcp_connecting.is_empty()
                && config.features().enabled(crate::features::Feature::Mcp),
            mcp_snapshot_generation: 0,
            mcp_snapshot_generation_invalidated: false,
            mcp_connecting,
            // Read the MCP config once at boot to know how many servers
            // the user has declared. The footer chip uses this even when
            // no live snapshot is available (#502). Cheap (just reads
            // the JSON files); errors fall through to zero so a missing
            // or malformed config simply hides the chip.
            mcp_configured_count,
            mcp_reload_required: false,
            mcp_reload_in_flight: false,
            tool_log: Vec::new(),
            active_skill: None,
            active_skill_provenance: None,
            cached_skills,
            tool_cells: HashMap::new(),
            tool_details_by_cell: HashMap::new(),
            context_references_by_cell: HashMap::new(),
            session_context_references: Vec::new(),
            active_cell: None,
            active_cell_revision: 0,
            active_tool_details: HashMap::new(),
            agent_roster: Vec::new(),
            agent_roster_session_id: None,
            agent_roster_print_requested: false,
            active_tool_entry_completed_at: HashMap::new(),
            exploring_entries: HashMap::new(),
            ignored_tool_calls: HashSet::new(),
            last_exec_wait_command: None,
            streaming_message_index: None,
            streaming_source_receipt: None,
            suppress_stream_events_until_turn_complete: false,
            streaming_thinking_active_entry: None,
            pending_thinking_translation_cells: HashMap::new(),
            thinking_revision_last_bump_at: None,
            streaming_state: StreamingState::new(),
            streaming_output_token_estimate: 0,
            reasoning_buffer: String::new(),
            reasoning_header: None,
            last_reasoning: None,
            pending_tool_uses: Vec::new(),
            pending_gate_receipts: Vec::new(),
            child_gate_receipts: std::collections::HashMap::new(),
            queued_messages: VecDeque::new(),
            held_restored_queue: 0,
            queued_draft: None,
            pending_steers: VecDeque::new(),
            inflight_steers: VecDeque::new(),
            submit_pending_steers_after_interrupt: false,
            turn_started_at: None,
            turn_last_activity_at: None,
            cumulative_turn_duration: std::time::Duration::ZERO,
            session_metrics: crate::tui::session_metrics::SessionMetrics::default(),
            balance_cell: std::sync::Arc::new(std::sync::Mutex::new(None)),
            balance_route: None,
            draft_gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            fleet_draft_cell: std::sync::Arc::new(std::sync::Mutex::new(None)),
            constitution_draft_cell: std::sync::Arc::new(std::sync::Mutex::new(None)),
            mcp_login: None,
            mcp_retries: Vec::new(),
            mcp_login_reconnect: None,
            prompt_suggestion_cell: std::sync::Arc::new(std::sync::Mutex::new(None)),
            balance_initiated: false,
            last_balance_fetch: None,
            runtime_turn_id: None,
            runtime_turn_status: None,
            turn_counter: 0,
            dispatch_started_at: None,
            workspace_context: None,
            workspace_is_linked_worktree: false,
            workspace_context_cell: std::sync::Arc::new(std::sync::Mutex::new(None)),
            workspace_context_refreshed_at: None,
            memory_size_hint: None,
            workspace_notes: Vec::new(),
            task_panel: Vec::new(),
            task_panel_session_id: None,
            task_panel_unavailable: false,
            automation_panel: crate::tui::automation_panel::AutomationPanelState::default(),
            automation_scan: None,
            behavioral_tips: crate::tui::behavioral_tips::BehavioralTipState::new(
                settings.contextual_tips,
            ),
            footer_hint_uses: settings.footer_hint_uses.clone(),
            workflow_runs: Vec::new(),
            session_started_at: chrono::Utc::now(),
            needs_redraw: true,
            fleet_roster_stale: false,
            force_next_full_repaint: false,
            thinking_started_at: None,
            is_compacting: false,
            active_compaction: None,
            manual_compaction_queued: false,
            manual_compaction_id: None,
            deferred_manual_compaction: None,
            is_purging: false,
            user_scrolled_during_stream: false,
            last_send_at: None,
            last_submitted_prompt: None,
            unanswered_submission: None,
            auto_submit_initial_input,
            quit_armed_until: None,
            prefix_change_count: 0,
            prefix_checks_total: 0,
            prefix_stability_pct: None,
            last_prefix_change_desc: None,
            last_pinned_prefix_hash: None,
            prefix_pin_reason: None,
            prefix_last_miss_reason: None,
            prefix_drift_count: 0,
            prefix_context_updates: 0,
            collapsed_cells: HashSet::new(),
            cell_folds: HashMap::new(),
            collapsed_cell_map: Vec::new(),
            edit_in_progress: false,
            lsp_enabled: config.lsp.as_ref().and_then(|l| l.enabled).unwrap_or(true),
            lsp_repair: LspRepairState::default(),
            composer_arrows_scroll: config
                .tui
                .as_ref()
                .and_then(|tui| tui.composer_arrows_scroll)
                .unwrap_or_else(|| default_composer_arrows_scroll(use_mouse_capture)),
            composer_arrows_scroll_explicit,
            mention_menu_limit: settings.mention_menu_limit,
            mention_walk_depth: settings.mention_walk_depth,
            mention_menu_behavior: settings.mention_menu_behavior.clone(),
            workspace_follow_symlinks: settings.workspace_follow_symlinks,
            session_title: None,
            window_title: None,
            composer_color: None,
            title_default: config
                .title
                .as_deref()
                .map(crate::session_manager::sanitize_session_title)
                .map(|title| title.trim().to_string())
                .filter(|title| !title.is_empty()),
            receipt_text: None,
            receipt_started_at: None,
            tool_evidence: Vec::new(),
        };
        // Restore presentation only; the visible-frame tick owns companion startup.
        app.pet_watch.enabled = settings.pet_mode;
        if settings.pet_mode {
            app.launch.visible = false;
        }
        if yolo_compat {
            app.notify_yolo_compat_once();
        }
        app
    }
}

/// Rewrite `settings.toml` with the legacy `default_mode = "yolo"` value
/// normalized away.
///
/// The normalization happens during parsing, so an empty transaction *is* the
/// migration: load (which normalizes), then save. Doing it as its own
/// [`crate::settings::Settings::transact`] rather than saving the snapshot
/// `App::new` already loaded matters twice over. It cannot write back a stale
/// pre-image, and — because `App::new` runs on the same hot path as several
/// hundred tests — it keeps the transaction lock out of the common construction
/// path entirely, taking it only when a legacy file actually needs migrating.
fn normalize_legacy_yolo_settings() -> anyhow::Result<()> {
    crate::settings::Settings::transact(|_normalized_on_load| Ok(()))
}
