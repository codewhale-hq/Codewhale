use codewhale_config::{SetupState, SetupStep, StepEntry, StepStatus};

use crate::tui::app::App;
use codewhale_engine::provider_readiness::ResolvedProviderReadiness;

pub(super) fn step_status(readiness: &ResolvedProviderReadiness) -> StepStatus {
    if matches!(readiness, ResolvedProviderReadiness::Ready) {
        StepStatus::Verified
    } else if readiness.can_attempt()
        && !matches!(
            readiness,
            ResolvedProviderReadiness::SavedLastCheckFailed { .. }
        )
    {
        StepStatus::Configured
    } else {
        StepStatus::NeedsAction
    }
}

pub(super) fn step_entry(
    status: StepStatus,
    checkpoint_version: &str,
    result: impl Into<String>,
) -> StepEntry {
    StepEntry::new(status, true, checkpoint_version).with_result(result.into())
}

/// First launch uses the App's resolved route and credential decision, never a
/// sidecar's age or the built-in provider default. This records configuration
/// only: no connection/model probe is performed and no prior review is erased.
pub(crate) async fn record_configured_route(app: &App) -> anyhow::Result<()> {
    if !app.startup_route_configured || app.onboarding_needs_api_key || app.model.trim().is_empty()
    {
        return Ok(());
    }
    let entry = step_entry(
        StepStatus::Configured,
        codewhale_engine::prompts::CONSTITUTION_CHECKPOINT_VERSION,
        format!(
            "provider={}, model={}; configured, not checked",
            app.provider_identity_for_persistence(),
            app.model,
        ),
    );
    tokio::task::spawn_blocking(move || {
        // Existing users keep the inherited setup derivation until they
        // explicitly review a step; do not create an otherwise empty sidecar.
        if crate::tui::onboarding::is_onboarded() {
            return Ok(());
        }
        SetupState::update_at(&SetupState::path()?, |state| {
            if state.status(SetupStep::ProviderModel) == StepStatus::NotStarted {
                state.set_step(SetupStep::ProviderModel, entry);
            }
        })
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_provider_receipt_only_verifies_observed_success() {
        for readiness in [
            ResolvedProviderReadiness::SavedUnchecked,
            ResolvedProviderReadiness::ImportedTokenUnchecked,
            ResolvedProviderReadiness::LocalUnchecked,
            ResolvedProviderReadiness::NoAuthUnchecked,
        ] {
            assert_eq!(step_status(&readiness), StepStatus::Configured);
        }
        assert_eq!(
            step_status(&ResolvedProviderReadiness::Ready),
            StepStatus::Verified
        );
        for readiness in [
            ResolvedProviderReadiness::MissingKey,
            ResolvedProviderReadiness::InvalidRoute,
            ResolvedProviderReadiness::SavedLastCheckFailed {
                category: codewhale_error_taxonomy::ErrorCategory::Authentication,
                message: "fixture rejection".to_string(),
            },
        ] {
            assert_eq!(step_status(&readiness), StepStatus::NeedsAction);
        }
    }
}
