use super::ModelAdapter;
use crate::tui::app::{App, AppAction};
use codewhale_command_contract::model_setup::{
    CommandModelSelectionContext, CommandProviderRoutesContext, ModelAction, ModelSelection,
    ModelSelectionReceipt, ProviderAction, ProviderFallback, ProviderSelection, SetupAction,
};
use codewhale_config::ProviderKind;
use codewhale_localization::{MessageId, tr};
use codewhale_runtime::config::normalize_custom_model_id;

impl<'a> TryFrom<ModelAction<'a>> for AppAction {
    type Error = String;
    fn try_from(action: ModelAction<'a>) -> Result<Self, String> {
        Ok(match action {
            ModelAction::OpenPicker => Self::OpenModelPicker,
            ModelAction::RefreshCatalog => Self::RefreshModelsDevCatalog,
            ModelAction::Router(rest) => Self::RouterSetup {
                request: crate::tui::views::router_setup::parse_router_args(Some(rest))?,
            },
            ModelAction::UpdateCompaction(config) => {
                Self::UpdateCompaction(crate::commands::config_host::to_compaction_config(config))
            }
        })
    }
}

impl<'a> TryFrom<ProviderAction<'a>> for AppAction {
    type Error = String;
    fn try_from(action: ProviderAction<'a>) -> Result<Self, String> {
        Ok(match action {
            ProviderAction::OpenPicker => Self::OpenProviderPicker,
            ProviderAction::Setup(None) => Self::OpenProviderSetup { provider: None },
            ProviderAction::Setup(Some(name)) => provider_setup_action_for_name(name)?,
            ProviderAction::Switch { provider, model } => Self::SwitchProvider {
                provider: provider.0.into(),
                model,
            },
        })
    }
}

impl<'a> TryFrom<SetupAction<'a>> for AppAction {
    type Error = String;
    fn try_from(action: SetupAction<'a>) -> Result<Self, String> {
        Ok(match action {
            SetupAction::OpenWizard(None) => Self::OpenSetupWizard,
            SetupAction::OpenWizard(Some(step)) => Self::OpenSetupWizardAt { step },
            SetupAction::Provider(name) => provider_setup_action_for_name(name)?,
        })
    }
}

impl CommandModelSelectionContext for ModelAdapter<'_> {
    fn select_session_model(
        &mut self,
        selection: ModelSelection<'_>,
    ) -> Result<ModelSelectionReceipt, String> {
        select_model(&mut self.host.app.borrow_mut(), selection)
    }
    fn save_session_route_default(&mut self) -> String {
        self.host.app.borrow_mut().apply_route_save_choice(
            crate::tui::views::route_save_prompt::RouteSaveChoice::SaveAsDefault,
        )
    }
}

fn select_model(
    app: &mut App,
    selection: ModelSelection<'_>,
) -> Result<ModelSelectionReceipt, String> {
    let auto = matches!(selection, ModelSelection::Auto);
    let (model_id, route_resolution) = match selection {
        ModelSelection::Auto => ("auto".to_string(), None),
        ModelSelection::Named(name) => {
            let declared = app.api_provider != ProviderKind::OpenaiCodex
                && codewhale_config::catalog::configured::validate_configured_models(
                    &app.configured_models,
                )
                .is_ok()
                && app.configured_models.iter().any(|model| {
                    model.id == name
                        && model.matches_route(
                            app.provider_identity_for_persistence(),
                            &app.active_route_base_url,
                        )
                });
            let model_id = if declared {
                name.to_string()
            } else if app.accepts_custom_model_ids() {
                let Some(model_id) = normalize_custom_model_id(name) else {
                    return Err(format!(
                        "Invalid model '{name}'. Expected a non-empty model ID."
                    ));
                };
                model_id
            } else {
                let Ok(resolved) =
                    codewhale_config::route::resolve_model(app.api_provider, name, None)
                else {
                    return Err(format!(
                        "Invalid model '{name}'. Expected auto or a model for the active provider ({}).",
                        app.api_provider.as_str()
                    ));
                };
                resolved.wire_id
            };
            let strict_direct_custom_endpoint = app.accepts_custom_model_ids()
                && matches!(app.api_provider, ProviderKind::Deepseek | ProviderKind::Zai);
            let route_resolution = if declared {
                Some(
                    codewhale_engine::route_runtime::resolve_declared_model_candidate(
                        app.api_provider,
                        app.provider_identity_for_persistence(),
                        &model_id,
                        &app.active_route_base_url,
                        app.active_context_window_override,
                        app.active_model_context_windows.as_ref(),
                        &app.configured_models,
                    )?,
                )
            } else if strict_direct_custom_endpoint {
                None
            } else {
                // `/model` normally resolves against the active provider's
                // catalog-default endpoint so it retains the existing local
                // provider/model validation. The one endpoint-sensitive model
                // selection is Kimi Code's bare `k3`: resolve it against the
                // committed Kimi Code route rather than silently falling back to
                // Moonshot's direct API route. Do not pass an unrelated stale
                // endpoint through here; doing so would turn a foreign provider
                // model into an apparent custom-route selection.
                let route_base_url = codewhale_runtime::config::is_exact_kimi_code_k3_route(
                    app.api_provider,
                    &app.active_route_base_url,
                    &model_id,
                )
                .then(|| app.active_route_base_url.clone());
                Some(
                    codewhale_engine::route_runtime::resolve_route_candidate_with_context_metadata(
                        app.api_provider,
                        Some(&model_id),
                        None,
                        route_base_url,
                        app.active_context_window_override,
                        app.active_model_context_windows.as_ref(),
                        None,
                    )?,
                )
            };
            (model_id, route_resolution)
        }
    };
    let old_model = app.model_display_label();
    let model_changed = app.auto_model != auto || app.model != model_id;
    app.set_model_selection(model_id.clone());
    if let Some(resolution) = route_resolution {
        app.set_active_route_resolution(
            resolution.candidate.endpoint().base_url.clone(),
            resolution.candidate.limits(),
            resolution.context_window,
        );
    } else {
        app.active_route_limits = app.context_window_override_limits();
        app.active_context_window = app.configured_context_window_for(&app.model);
        app.active_context_window_source = app
            .active_context_window
            .map(|resolution| resolution.source)
            .unwrap_or(codewhale_engine::route_runtime::ContextWindowSource::Fallback);
    }
    app.update_model_compaction_budget();
    if model_changed {
        app.clear_model_scoped_telemetry();
    } else {
        app.session.last_prompt_tokens = None;
        app.session.last_completion_tokens = None;
    }
    let provider_identity = app.provider_identity_for_persistence().to_string();
    app.provider_models
        .insert(provider_identity.clone(), model_id.clone());
    if !auto {
        app.note_route_used(&provider_identity, &model_id);
    }
    // Route changes are temporary by default: nothing is written here.
    // The route-save prompt offers the explicit persistence choices.
    app.note_session_route_change(&provider_identity, &model_id);
    let mut message = tr(app.ui_locale, MessageId::ModelChanged)
        .replace("{old}", &old_model)
        .replace("{new}", &model_id);
    message.push_str(&tr(app.ui_locale, MessageId::ModelChangedSessionNote));
    Ok(ModelSelectionReceipt {
        message,
        compaction: crate::commands::config_host::from_compaction_config(app.compaction_config()),
    })
}

impl CommandProviderRoutesContext for ModelAdapter<'_> {
    fn resolve_switch(
        &self,
        name: &str,
        model_arg: Option<&str>,
    ) -> Result<ProviderSelection, String> {
        let app = self.host.app.borrow();
        if codewhale_runtime::config::is_legacy_antigravity_identity(name) {
            return Err(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string());
        }

        let Some(row) = codewhale_config::descriptors::compatibility_for_selector(name) else {
            return Err(format!(
                "Unknown provider '{name}'. Expected: {}.",
                ProviderKind::names_hint()
            ));
        };

        let target = row.kind;
        let selected_key: codewhale_config::ProviderId = row.id.into();

        let model = match model_arg {
            None => None,
            Some(raw) => {
                let trimmed = raw.trim();
                if is_route_ambiguous_deepseek_alias(target, trimmed) {
                    Some(trimmed.to_string())
                } else {
                    match codewhale_config::route::resolve_model(target, trimmed, None) {
                        Ok(resolved) => Some(resolved.logical),
                        Err(_) => {
                            return Err(format!(
                                "Invalid model '{raw}'. Provide a non-empty model id."
                            ));
                        }
                    }
                }
            }
        };

        let already_active = app
            .provider_identity
            .as_ref()
            .is_some_and(|identity| identity.key == selected_key)
            && model.is_none()
            && !(target == ProviderKind::Ollama
                && codewhale_runtime::config::is_unresolved_local_ollama_model(&app.model));
        Ok(ProviderSelection {
            id: super::to_provider_id(selected_key.as_str()),
            model,
            already_active,
        })
    }
    fn fallback(&self) -> ProviderFallback {
        let app = self.host.app.borrow();
        ProviderFallback {
            current: app.provider_identity_for_persistence().to_string(),
            entries: app
                .fallback_chain_entries()
                .into_iter()
                .map(|(index, provider, active)| {
                    (index, super::to_provider_id(provider.as_str()), active)
                })
                .collect(),
            last_reason: app.last_fallback_reason.clone(),
        }
    }
}

fn provider_setup_action_for_name(raw: &str) -> Result<AppAction, String> {
    if codewhale_runtime::config::is_legacy_antigravity_identity(raw) {
        return Err(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string());
    }
    if raw.eq_ignore_ascii_case("ds4") || raw.eq_ignore_ascii_case("dwarfstar") {
        return Ok(AppAction::OpenDs4Setup);
    }
    // First-class aliases (zen, opencode-zen, …) resolve through the provider
    // registry. There are no setup templates anymore: named custom hosts are
    // configured with `/provider setup` and the blank custom form (#6289).
    match codewhale_config::descriptors::compatibility_for_selector(raw) {
        Some(provider) => Ok(AppAction::OpenProviderSetup {
            provider: Some(provider.id.into()),
        }),
        None => Err(format!(
            "Unknown provider '{raw}'. Expected: {}.",
            ProviderKind::names_hint()
        )),
    }
}

fn is_route_ambiguous_deepseek_alias(provider: ProviderKind, model: &str) -> bool {
    matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) && (model.eq_ignore_ascii_case("deepseek-chat")
        || model.eq_ignore_ascii_case("deepseek-reasoner"))
}
