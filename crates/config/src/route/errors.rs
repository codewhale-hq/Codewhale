//! Route resolution errors (#3384).
//!
//! `thiserror` is not a dependency of this crate, so [`std::fmt::Display`] and
//! [`std::error::Error`] are hand-implemented. No new dependency is added.

use std::fmt;

use super::ids::ProviderId;

/// Why a [`super::resolver::RouteResolver`] could not produce a candidate.
#[derive(Debug, Clone)]
pub enum RouteError {
    /// The requested model selector was empty.
    EmptyModel,
    /// The named provider could not be resolved.
    InvalidProvider(String),
    /// A model matched multiple providers; the caller must disambiguate.
    AmbiguousModel(Vec<ProviderId>),
    /// A clearly-foreign model was requested for a strict direct provider.
    ForeignModelForDirectProvider {
        /// The strict direct provider that rejected the model.
        provider: ProviderId,
        /// The foreign model selector that was rejected.
        model: String,
    },
    /// The resolved route has no unambiguous Responses dialect profile.
    ResponsesProfile {
        /// Provider whose route was being resolved.
        provider: ProviderId,
        /// Profile selection failure.
        detail: String,
    },
    /// A model-aware provider did not prove a supported request protocol for
    /// the selected model/endpoint.
    UnsupportedModelProtocol {
        /// Provider whose catalog row was incomplete or unsupported.
        provider: ProviderId,
        /// Selected provider-owned model id.
        model: String,
        /// Catalog endpoint key, when one was present.
        endpoint_key: String,
    },
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModel => write!(f, "model selector was empty"),
            Self::InvalidProvider(name) => write!(f, "invalid provider: {name}"),
            Self::AmbiguousModel(providers) => {
                let names: Vec<&str> = providers.iter().map(ProviderId::as_str).collect();
                write!(
                    f,
                    "model matches multiple providers ({}); specify a provider",
                    names.join(", ")
                )
            }
            Self::ForeignModelForDirectProvider { provider, model } => write!(
                f,
                "model {model:?} is not served by direct provider {}",
                provider.as_str()
            ),
            Self::ResponsesProfile { provider, detail } => write!(
                f,
                "provider {} responses profile: {detail}",
                provider.as_str()
            ),
            Self::UnsupportedModelProtocol {
                provider,
                model,
                endpoint_key,
            } => {
                write!(
                    f,
                    "model {model:?} on provider {} has unsupported or unproven endpoint {endpoint_key:?}",
                    provider.as_str()
                )?;
                // #6705: say which case this is, so a catalog Codewhale has not
                // caught up with does not read as a broken install. The
                // refresh remedy belongs to callers whose resolver actually
                // reads the refreshed catalog, so it is not stated here.
                match endpoint_key.as_str() {
                    "unproven" => write!(
                        f,
                        ": no catalog Codewhale has loaded proves this model's wire protocol. \
                         Reach it through a `kind = \"openai-compatible\"` provider with an \
                         explicit `wire`"
                    ),
                    super::OPENCODE_ZEN_DEPRECATED_ENDPOINT_KEY => write!(
                        f,
                        ": the provider's catalog marks this model deprecated; choose a current model"
                    ),
                    _ => write!(
                        f,
                        ": the provider serves this model over a protocol Codewhale does not speak"
                    ),
                }
            }
        }
    }
}

impl std::error::Error for RouteError {}
