//! One immutable authority for a compiled provider catalog and its route resolver.
//!
//! Before this seam, callers could independently project a
//! [`crate::catalog::CatalogSnapshot`] through
//! [`crate::catalog::CatalogSnapshot::to_offerings`] and create a
//! [`super::RouteResolver`]. That made it possible for a picker, readiness
//! view, and execution path to use different catalog snapshots without any
//! type-level signal. `RouteAuthoritySnapshot` replaces that ad-hoc pairing for
//! new consumers: it owns the exact compiled catalog and the resolver projected
//! from that catalog together.
//!
//! It deliberately does not claim that a catalog row is runnable. Calling
//! [`RouteAuthoritySnapshot::resolve`] still mints a
//! [`super::ReadyRouteCandidate`] through the sole resolver. The returned
//! receipt distinguishes an exact catalog row, a custom-endpoint route whose
//! provider facts are intentionally not reused, and an allowed pass-through
//! route with no catalog row. All state is secret-free.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{CatalogOffering, CatalogSnapshot, CatalogStatus};

use super::{
    ProviderId, ReadyRouteCandidate, RouteError, RouteRequest, RouteResolver, WireModelId,
};

/// A secret-free provider catalog cache scope.
///
/// The base URL is represented only by its already-redacted fingerprint. The
/// provider remains an open catalog string because a catalog can include a
/// discoverable provider that is not yet a built-in [`crate::ProviderKind`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteCatalogScope {
    provider: String,
    base_url_fingerprint: String,
}

impl RouteCatalogScope {
    /// Construct a normalized, secret-free cache scope.
    #[must_use]
    pub fn new(provider: impl Into<String>, base_url_fingerprint: impl Into<String>) -> Self {
        Self {
            provider: provider.into().trim().to_string(),
            base_url_fingerprint: base_url_fingerprint.into().trim().to_string(),
        }
    }

    /// Provider id associated with this cache scope.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Secret-free base URL fingerprint associated with this cache scope.
    #[must_use]
    pub fn base_url_fingerprint(&self) -> &str {
        &self.base_url_fingerprint
    }
}

/// Provenance receipt for a route resolved from a [`RouteAuthoritySnapshot`].
///
/// A catalog source describes metadata provenance, not account authorization or
/// endpoint health. Consumers must keep readiness/auth checks separate.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogOfferingReceipt {
    /// The resolved provider/wire pair exactly matches an offering in the
    /// compiled catalog owned by this authority snapshot.
    Catalog { offering: Box<CatalogOffering> },
    /// The caller selected an explicit base URL. The resolver deliberately
    /// clears catalog-owned capabilities and pricing for that custom endpoint,
    /// so it must not be presented as an exact catalog offering.
    CustomEndpoint {
        provider: ProviderId,
        wire_model_id: WireModelId,
    },
    /// The resolver permitted an unknown/pass-through wire model, but the
    /// compiled catalog did not assert facts for that provider/wire pair.
    NotCataloged {
        provider: ProviderId,
        wire_model_id: WireModelId,
    },
}

impl CatalogOfferingReceipt {
    /// Return the exact catalog offering, if this route was catalog-backed.
    #[must_use]
    pub fn offering(&self) -> Option<&CatalogOffering> {
        match self {
            Self::Catalog { offering } => Some(offering),
            Self::CustomEndpoint { .. } | Self::NotCataloged { .. } => None,
        }
    }
}

/// One route-resolution result plus honest catalog provenance.
#[derive(Debug, Clone)]
pub struct AuthorityResolution {
    candidate: ReadyRouteCandidate,
    receipt: CatalogOfferingReceipt,
}

impl AuthorityResolution {
    /// The executable candidate minted by the snapshot's resolver.
    #[must_use]
    pub fn candidate(&self) -> &ReadyRouteCandidate {
        &self.candidate
    }

    /// Honest catalog provenance for the candidate's provider/wire pair.
    #[must_use]
    pub fn receipt(&self) -> &CatalogOfferingReceipt {
        &self.receipt
    }
}

/// A compiled catalog and the resolver derived from that exact catalog.
///
/// The private fields prevent a consumer from retaining the snapshot while
/// silently replacing its resolver. Refreshers build a new immutable snapshot
/// when their catalog changes; existing consumers keep their coherent view.
#[derive(Debug, Clone)]
pub struct RouteAuthoritySnapshot {
    catalog: CatalogSnapshot,
    resolver: RouteResolver,
    scope_statuses: BTreeMap<RouteCatalogScope, CatalogStatus>,
}

impl RouteAuthoritySnapshot {
    /// Bind a compiled catalog to the resolver that consumes its offerings.
    #[must_use]
    pub fn new(catalog: CatalogSnapshot) -> Self {
        let resolver = RouteResolver::from_offerings(catalog.to_offerings());
        Self {
            catalog,
            resolver,
            scope_statuses: BTreeMap::new(),
        }
    }

    /// Record the cache health known for one provider/base-URL scope.
    ///
    /// An omitted status stays [`CatalogStatus::Unknown`]; this API never
    /// derives freshness from a model row or invents a successful refresh.
    #[must_use]
    pub fn with_scope_status(mut self, scope: RouteCatalogScope, status: CatalogStatus) -> Self {
        self.scope_statuses.insert(scope, status);
        self
    }

    /// The exact compiled catalog bound to this resolver.
    #[must_use]
    pub fn catalog(&self) -> &CatalogSnapshot {
        &self.catalog
    }

    /// Cache health for one provider/base-URL scope, or honestly unknown.
    #[must_use]
    pub fn scope_status(&self, scope: &RouteCatalogScope) -> CatalogStatus {
        self.scope_statuses
            .get(scope)
            .cloned()
            .unwrap_or(CatalogStatus::Unknown)
    }

    /// Resolve through the authority-bound resolver and retain catalog receipt.
    ///
    /// # Errors
    /// Returns the route resolver's validation error when a request cannot
    /// produce an executable candidate.
    pub fn resolve(&self, request: &RouteRequest) -> Result<AuthorityResolution, RouteError> {
        let candidate = self.resolver.resolve(request)?;
        let receipt = if request.base_url_override.is_some() {
            CatalogOfferingReceipt::CustomEndpoint {
                provider: candidate.provider_id().clone(),
                wire_model_id: candidate.wire_model_id().clone(),
            }
        } else if let Some(offering) = self.catalog.offerings.iter().find(|offering| {
            offering.provider == candidate.provider_id().as_str()
                && offering.wire_model_id == candidate.wire_model_id().as_str()
        }) {
            CatalogOfferingReceipt::Catalog {
                offering: Box::new(offering.clone()),
            }
        } else {
            CatalogOfferingReceipt::NotCataloged {
                provider: candidate.provider_id().clone(),
                wire_model_id: candidate.wire_model_id().clone(),
            }
        };

        Ok(AuthorityResolution { candidate, receipt })
    }
}

#[cfg(test)]
mod tests {
    use crate::ProviderKind;
    use crate::catalog::{CatalogCompiler, CatalogSource};
    use crate::models_dev::ModelsDevLimit;
    use crate::route::{LogicalModelRef, RouteRequest};

    use super::{CatalogOfferingReceipt, RouteAuthoritySnapshot, RouteCatalogScope};

    fn request(model: &str) -> RouteRequest {
        RouteRequest {
            explicit_provider: Some(ProviderKind::Deepseek),
            model_selector: Some(LogicalModelRef::from(model)),
            saved_provider_model: None,
            base_url_override: None,
            limit_overrides: Vec::new(),
            wire_override: None,
            responses_profile_override: None,
        }
    }

    fn offering(source: CatalogSource, context: u64) -> crate::catalog::CatalogOffering {
        crate::catalog::CatalogOffering {
            provider: "deepseek".to_string(),
            wire_model_id: "deepseek-v4-flash-vision-exp".to_string(),
            endpoint_key: "responses".to_string(),
            default_for_provider: true,
            limit: Some(ModelsDevLimit {
                context: Some(context),
                ..Default::default()
            }),
            source,
            ..Default::default()
        }
    }

    #[test]
    fn one_compiled_snapshot_drives_candidate_and_catalog_receipt() {
        let snapshot = CatalogCompiler::new()
            .with_bundled(vec![offering(CatalogSource::Bundled, 32_000)])
            .with_config(vec![offering(CatalogSource::ConfigOverride, 64_000)])
            .compile();
        let authority = RouteAuthoritySnapshot::new(snapshot);

        let resolved = authority
            .resolve(&request("deepseek-v4-flash-vision-exp"))
            .expect("catalog-backed direct route resolves");

        assert_eq!(resolved.candidate().limits().context_tokens, Some(64_000));
        let CatalogOfferingReceipt::Catalog { offering } = resolved.receipt() else {
            panic!("compiled catalog row must be retained as the receipt");
        };
        assert_eq!(offering.source, CatalogSource::ConfigOverride);
        assert_eq!(
            offering.limit.as_ref().and_then(|limit| limit.context),
            Some(64_000)
        );
    }

    #[test]
    fn custom_endpoint_never_claims_catalog_offering_facts() {
        let snapshot = CatalogCompiler::new()
            .with_bundled(vec![offering(CatalogSource::Bundled, 32_000)])
            .compile();
        let authority = RouteAuthoritySnapshot::new(snapshot);
        let mut request = request("deepseek-v4-flash-vision-exp");
        request.base_url_override = Some("https://compatible.example/v1".to_string());

        let resolved = authority
            .resolve(&request)
            .expect("custom compatible endpoint still resolves");

        assert!(matches!(
            resolved.receipt(),
            CatalogOfferingReceipt::CustomEndpoint { .. }
        ));
        assert!(resolved.receipt().offering().is_none());
    }

    #[test]
    fn direct_provider_pass_through_is_explicitly_not_cataloged() {
        let authority = RouteAuthoritySnapshot::new(CatalogCompiler::new().compile());

        let resolved = authority
            .resolve(&request("future-deepseek-model"))
            .expect("direct-provider pass-through remains executable");

        assert!(matches!(
            resolved.receipt(),
            CatalogOfferingReceipt::NotCataloged { .. }
        ));
        assert!(resolved.receipt().offering().is_none());
    }

    #[test]
    fn scope_status_is_explicit_and_defaults_to_unknown() {
        let snapshot = CatalogCompiler::new().compile();
        let fresh_scope = RouteCatalogScope::new("deepseek", "fingerprint-a");
        let absent_scope = RouteCatalogScope::new("deepseek", "fingerprint-b");
        let authority = RouteAuthoritySnapshot::new(snapshot)
            .with_scope_status(fresh_scope.clone(), crate::catalog::CatalogStatus::Fresh);

        assert_eq!(
            authority.scope_status(&fresh_scope),
            crate::catalog::CatalogStatus::Fresh
        );
        assert_eq!(
            authority.scope_status(&absent_scope),
            crate::catalog::CatalogStatus::Unknown
        );
    }
}
