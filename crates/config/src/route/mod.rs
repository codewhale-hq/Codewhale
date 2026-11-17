//! Route Contract: the runtime path for provider/model identity (Phase 1).
//!
//! `RouteResolver` is the sole producer of [`ReadyRouteCandidate`]. Config and
//! CLI resolution now go through it. Catalog rows are keyed by open kebab
//! [`ids::RouteId`] strings; `ProviderKind` remains the bespoke-transport
//! classification plus serde aliases for retired spellings.
//!
//! Layering:
//! - [`ids`] — provider/model/wire string newtypes + namespace hints.
//! - [`descriptor`] — route-facing view over the static provider registry.
//! - [`offering`] — provider/model offering seam (wire-id binding).
//! - [`capabilities`] — three-state provider/model capability facts.
//! - [`candidate`] — the runtime-resolved executable route + its parts.
//! - [`errors`] — route resolution errors.
//! - [`resolver`] — the sole producer of [`candidate::ReadyRouteCandidate`].
//!
//! Naming: the request/response wire shape is spelled [`RequestProtocol`],
//! which is a re-export alias of [`crate::provider::WireFormat`] rather than a
//! fourth protocol synonym.

/// The selected endpoint's request/response wire shape.
///
/// Alias of [`crate::provider::WireFormat`]; intentionally NOT a new enum, to
/// avoid introducing yet another protocol synonym.
pub use crate::provider::WireFormat as RequestProtocol;

pub mod auth;
pub mod authority;
pub mod candidate;
pub mod capabilities;
pub mod descriptor;
pub mod errors;
pub mod export;
pub mod ids;
pub mod offering;
pub mod policy;
pub mod resolver;

pub use auth::{
    AuthKind, AuthMethod, AuthMethodExport, Choice, ChoiceExport, Op, Prompt, PromptExport, When,
    WhenExport,
};
pub use authority::{
    AuthorityResolution, CatalogOfferingReceipt, RouteAuthoritySnapshot, RouteCatalogScope,
};
pub use candidate::{
    LimitField, OverrideSource, PricingSku, ReadyRouteCandidate, ResolvedAuthSource,
    ResolvedEndpoint, SourcedLimitOverride, ValidationReport,
};
pub(crate) use capabilities::documented_server_side_web_search;
pub use capabilities::{CapabilityState, RouteCapabilities};
pub use descriptor::{
    EndpointDescriptor, ProviderDescriptor, TransportKind, auth_methods_for, family_for,
};
pub use errors::RouteError;
pub use export::{
    PROVIDERS_EXPORT_SCHEMA_VERSION, ProvidersExport, RouteExport, RouteModelExport,
    parse_route_kind, route_id_for,
};
pub use ids::{LogicalModelRef, ModelId, NamespaceHint, ProviderId, RouteId, WireModelId};
pub use offering::{
    CODEWHALE_FALLBACK_MODELS, OPENCODE_ZEN_DEPRECATED_ENDPOINT_KEY, ProviderModelOffering,
    RouteLimits, bundled_offerings, codewhale_endpoint_key_for_model,
    opencode_zen_endpoint_key_for_npm, opencode_zen_picker_models,
};
pub use policy::{CatalogPolicy, PolicyAction, PolicyEffect, PolicyRule};
pub use resolver::{
    ResolvedModel, RouteRequest, RouteResolver, bundled_offering_wire_id, resolve_model,
};

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod tests;
