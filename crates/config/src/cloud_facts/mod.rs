//! Cloud facts (`facts/v1`): a signed, versioned, off-by-default overlay of
//! facts CodeWhale wants to move faster than binary releases — model catalog
//! deltas, provider defaults, release truth, and one-line announcements.
//!
//! This module is **network-free and tokio-free**. It owns the payload types,
//! the pinned trust anchors, envelope verification, version scoping, the
//! process-wide overlay, and the catalog patch semantics. Fetching and the
//! disk cache live in the `codewhale-cloud-facts` crate; the TUI wires both.
//!
//! Invariants:
//! - Bundled facts are the floor. Off / unreachable / rejected / inapplicable
//!   payloads leave the binary exactly as it ships.
//! - Nothing here can change an explicitly configured or session-selected
//!   model, point a provider at a non-official host, or touch safety policy.
//! - Verification happens before any payload byte is interpreted.

pub mod catalog_patch;
pub mod keys;
pub mod overlay;
pub mod provenance;
pub mod registry;
pub mod scope;
pub mod types;
pub mod verify;

pub use keys::{
    AUTOMATION_CHANNEL_SUFFIX, AUTOMATION_MAX_MODELS, AUTOMATION_MAX_VALIDITY_SECS, DOMAIN,
    ENVELOPE_VERSION, KeyRole, KeyStatus, MAX_ENVELOPE_BYTES, MAX_PAYLOAD_BYTES,
    SUPPORTED_SCHEMA_VERSION, TRUSTED_KEYS, TrustedKey,
};
pub use overlay::{
    DefaultSource, cloud_default_base_url, cloud_default_model, cloud_default_model_for_route,
    pulled_registry,
};
pub use provenance::{CloudFactsState, CloudFactsStatus, FactsOrigin};
pub use registry::{
    AdmittedRegistry, BuiltinNames, PulledProvider, REGISTRY_CHANNEL, REGISTRY_SCHEMA, Registry,
    VerifiedRegistry, admit_registry, verify_registry_envelope,
};
pub use scope::{ScopedFacts, scoped_view};
pub use types::{
    Announcement, AnnouncementLevel, CloudFacts, ModelFact, ModelOp, PricingFact,
    ProviderDefaultFact, ReleaseFact, Surface,
};
pub use verify::{
    Envelope, FactsRejection, VerifiedFacts, automation_scope_violation, verify_envelope,
};

/// Whether any pinned key can authenticate a release. With none, the layer is
/// inert regardless of the feature flag.
#[must_use]
pub fn has_active_trusted_key() -> bool {
    TRUSTED_KEYS
        .iter()
        .any(|key| key.status == KeyStatus::Active)
}

/// The running binary's version as semver, for `applies_to` evaluation.
#[must_use]
pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

#[cfg(test)]
mod tests;
