//! Provider-configuration support: runtime-preset file snapshots with
//! rollback, and the provider key verification seam
//! (TUI_MODULARIZATION.md slice 8).

use super::*;

pub(crate) trait ProviderKeyVerifier {
    fn verify<'a>(
        &'a self,
        provider: ProviderKind,
        api_key: &'a str,
        base_url: &'a str,
    ) -> ProviderKeyVerification<'a>;
}

pub(crate) struct LiveProviderKeyVerifier;

impl ProviderKeyVerifier for LiveProviderKeyVerifier {
    fn verify<'a>(
        &'a self,
        provider: ProviderKind,
        api_key: &'a str,
        base_url: &'a str,
    ) -> ProviderKeyVerification<'a> {
        Box::pin(codewhale_engine::client::verify_provider_api_key(
            provider, api_key, base_url,
        ))
    }
}

/// Publish the `/models` roster a successful key probe already downloaded as
/// this exact route's live catalog (the #3385 cache and its provider-lake
/// partition). A route roster is authoritative for the ids it lists and for
/// its omissions, so the model pick that follows — and `/provider` and
/// `/model` — offer what this key can call today instead of bundled or
/// Models.dev rows the provider has retired. The probe scopes rows to the
/// provider kind; ownership here is the exact route identity, so a named or
/// regional table keeps its own partition.
///
/// The endpoint decides availability; the catalog still decides which listed
/// ids are chat models. Ollama publishes only through its capability-aware
/// native refresh. A first roster for another route keeps only the ids the
/// catalog already offers there (case-insensitively, in the provider's own
/// spelling), so an OpenAI-style `/models` that also lists embedding, speech
/// and image models does not flood setup. When the catalog knows none of the
/// ids, or the route already has a roster, the probe's roster is kept whole.
///
/// Does not: list a served chat model the catalog does not know yet (until
/// the catalog or `codewhale models --update` lists it — the same as before
/// this probe was read), refresh on its own schedule, or remove the roster if
/// the user abandons setup. The rows are secret-free facts about this
/// endpoint; account-scoped endpoints are re-fenced by
/// `begin_refresh_for_identity`.
pub(crate) fn publish_verified_roster(
    identity: &codewhale_runtime::config::ProviderIdentity,
    base_url: &str,
    roster: Option<codewhale_config::catalog::ProviderCatalogDelta>,
) {
    if identity.provider == ProviderKind::Ollama {
        return;
    }
    let Some(mut roster) = roster else {
        return;
    };
    let key = identity.key.as_str();
    let has_route_roster = codewhale_engine::provider_catalog_live::cached_entry_for_route(
        identity.provider,
        key,
        base_url,
    )
    .ok()
    .flatten()
    .is_some_and(|entry| entry.fetched_at > 0);
    if !has_route_roster {
        // Without a route roster this is the catalog view: bundled, Models.dev
        // and signed rows for this provider.
        let offered = codewhale_engine::provider_lake::catalog_models_for_route(
            identity.provider,
            key,
            base_url,
        );
        let chat: Vec<_> = roster
            .offerings
            .iter()
            .filter(|row| {
                offered
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(&row.wire_model_id))
            })
            .cloned()
            .collect();
        if !chat.is_empty() {
            roster.offerings = chat;
        }
    }
    let owner = identity.key.to_string();
    for row in &mut roster.offerings {
        row.provider.clone_from(&owner);
    }
    roster.provider = owner;
    let ticket = codewhale_engine::provider_catalog_live::begin_refresh_for_identity(
        identity.provider,
        key,
        base_url,
    );
    // `None` means a newer refresh for this route superseded the probe; its
    // rows win, which is the failure-preserving outcome we want.
    let _ = codewhale_engine::provider_catalog_live::record_success_if_current(&ticket, roster);
}

pub(crate) struct RuntimePresetFileSnapshot {
    pub(crate) path: PathBuf,
    pub(crate) contents: Option<Vec<u8>>,
}

impl RuntimePresetFileSnapshot {
    pub(crate) fn capture(path: PathBuf) -> Result<Self> {
        let contents = match std::fs::read(&path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to snapshot {}", path.display()));
            }
        };
        Ok(Self { path, contents })
    }

    fn restore(&self) -> Result<()> {
        match &self.contents {
            Some(contents) => codewhale_util::utils::write_atomic(&self.path, contents)
                .with_context(|| format!("failed to restore {}", self.path.display())),
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("failed to remove {}", self.path.display()))
                }
            },
        }
    }
}

pub(crate) fn runtime_preset_error_with_rollback(
    error: anyhow::Error,
    snapshots: &[&RuntimePresetFileSnapshot],
) -> anyhow::Error {
    let rollback_errors = snapshots
        .iter()
        .filter_map(|snapshot| snapshot.restore().err())
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if rollback_errors.is_empty() {
        error
    } else {
        anyhow::anyhow!(
            "{error:#}; runtime preset rollback also failed: {}",
            rollback_errors.join("; ")
        )
    }
}
