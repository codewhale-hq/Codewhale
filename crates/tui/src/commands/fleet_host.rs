use crate::tui::app::App;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_runtime::config::Config;

fn provider_id_is_configured(app: &App, config: &Config, provider_id: &str) -> bool {
    let Some(active) = app
        .provider_identity
        .as_ref()
        .filter(|identity| config.verify_provider_identity(identity).is_ok())
    else {
        return false;
    };
    let Ok(identity) = config.resolve_provider_selection_identity(provider_id) else {
        return false;
    };
    codewhale_engine::provider_readiness::provider_is_configured_for_active(
        config, &identity, active,
    )
}

#[must_use]
pub(crate) fn fleet_provider_rejection(
    app: &App,
    config: &Config,
    provider: &str,
) -> Option<String> {
    (!provider_id_is_configured(app, config, provider)).then(|| {
        tr(app.ui_locale, MessageId::FleetAddProviderUnconfigured).replace("{provider}", provider)
    })
}

#[must_use]
pub(crate) fn fleet_catalog_rejection(
    locale: Locale,
    provider: &str,
    model: &str,
) -> Option<String> {
    let known = codewhale_config::ProviderKind::parse(provider)?;
    let served = codewhale_engine::provider_lake::all_catalog_models_for_provider(known);
    (!served.is_empty() && !served.iter().any(|id| id.eq_ignore_ascii_case(model))).then(|| {
        tr(locale, MessageId::FleetAddModelNotServed)
            .replace("{provider}", provider)
            .replace("{model}", model)
    })
}
