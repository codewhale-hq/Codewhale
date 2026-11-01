use crate::provider_kind::ProviderKind;
use crate::telemetry::TelemetrySource;
use anyhow::Result;
use std::collections::BTreeMap;

pub(crate) fn base_url_uses_local_host(base_url: &str) -> bool {
    let Some(host) = base_url_host(base_url) else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "0.0.0.0") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|addr| addr.is_loopback() || addr.is_unspecified())
}

pub fn base_url_host(base_url: &str) -> Option<&str> {
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next()?.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host);
    }
    authority.split(':').next().filter(|host| !host.is_empty())
}

#[derive(Debug, Clone, Default)]
pub struct CliRuntimeOverrides {
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub auth_mode: Option<String>,
    pub log_level: Option<String>,
    pub telemetry: Option<bool>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    pub yolo: Option<bool>,
    pub verbosity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeApiKeySource {
    Cli,
    ConfigFile,
    Keyring,
    Env,
}

impl RuntimeApiKeySource {
    #[must_use]
    pub fn as_env_value(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::ConfigFile => "config",
            Self::Keyring => "keyring",
            Self::Env => "env",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    Cli,
    Env(&'static str),
    Config,
}

/// Where the resolved runtime model id came from.
///
/// This mirrors the precedence chain in
/// `ConfigToml::resolve_runtime_options_with_secrets` so diagnostics can say
/// *why* a model was chosen instead of presenting a built-in default as if the
/// user had asked for it. [`Self::ProviderDefault`] is the only variant that
/// means "nothing was configured".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `--model` on the command line.
    Cli,
    /// A `CODEWHALE_*` environment variable.
    Env,
    /// `[providers.<name>].model`.
    ProviderConfig,
    /// The root `default_text_model` key, which is DeepSeek-scoped.
    RootDefaultTextModel,
    /// The provider-neutral root `model` key.
    RootModel,
    /// Nothing was configured; this is the built-in default for the provider.
    ProviderDefault,
}

impl ModelSource {
    /// Whether the id was chosen by the user rather than substituted by us.
    #[must_use]
    pub fn is_explicit(self) -> bool {
        !matches!(self, Self::ProviderDefault)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "--model",
            Self::Env => "environment",
            Self::ProviderConfig => "config [providers.*].model",
            Self::RootDefaultTextModel => "config default_text_model",
            Self::RootModel => "config model",
            Self::ProviderDefault => "provider default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntimeOptions {
    pub provider: ProviderKind,
    pub provider_source: ProviderSource,
    pub model: String,
    pub model_source: ModelSource,
    pub api_key: Option<String>,
    pub api_key_source: Option<RuntimeApiKeySource>,
    pub base_url: String,
    pub auth_mode: Option<String>,
    pub insecure_skip_tls_verify: bool,
    pub log_level: Option<String>,
    pub telemetry: bool,
    /// Where the resolved telemetry consent came from (cli | env | config |
    /// default), so doctor and config displays can state the truth about a
    /// machine that never opted in (#5441).
    pub telemetry_source: TelemetrySource,
    /// A human wrote `telemetry = false` into the config file.
    ///
    /// This is the *persistent* opt-out, and it is deliberately narrower than
    /// "telemetry resolved to false". A run-scoped kill switch also resolves
    /// false; treating that as a revocation would destroy the identity and
    /// buffered events of a user who merely set `CODEWHALE_TELEMETRY=0` for one
    /// command. Run-scoped kill switches
    /// (`--telemetry false`, the environment variable) stop the run and leave
    /// every byte on disk alone; only this flag authorizes the wipe.
    pub telemetry_explicit_off: bool,
    /// Where a telemetry batch would be sent, if telemetry were on.
    ///
    /// Already resolved: `DEFAULT_TELEMETRY_ENDPOINT` when nobody configured
    /// one, the configured value when somebody did, and `None` when somebody
    /// configured an empty one — which means the dry-run sink, not "unset".
    /// Which schemes are actually contactable is decided where a batch would be
    /// sent, not here — a user must be able to stage a value.
    pub telemetry_endpoint: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    pub yolo: Option<bool>,
    pub verbosity: Option<String>,
    pub http_headers: BTreeMap<String, String>,
    /// Executable route minted by [`crate::route::RouteResolver`].
    ///
    /// `Err` carries the resolver's rejection (foreign model on a strict
    /// direct provider, empty model, unsupported protocol). `model` and
    /// `base_url` above are still the requested values in that case, so a
    /// caller that reports them as a resolved route must check this first.
    /// Auth/key fields above are independent: the resolver never inspects
    /// credentials.
    pub route: Result<crate::route::ReadyRouteCandidate, crate::route::RouteError>,
}
