use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::keys::{KeyRole, TrustedKey};
use super::verify::{
    Authenticated, EnvelopeLimits, FUTURE_CLOCK_TOLERANCE_SECS, FactsRejection, authenticate,
    parse_rfc3339_utc, sha256_hex,
};

pub const REGISTRY_SCHEMA: &str = "codewhale.registry.v1";
pub const REGISTRY_CHANNEL: &str = "registry";
pub const MAX_REGISTRY_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_REGISTRY_ENVELOPE_BYTES: usize = MAX_REGISTRY_PAYLOAD_BYTES / 3 * 4 + 64 * 1024;
const OPENAI_COMPATIBLE_WIRE: &str = "openai-compatible";
const MAX_PROVIDER_ID_CHARS: usize = 48;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: String,
    pub revision: u64,
    pub generated_at: String,
    #[serde(default)]
    pub providers: Vec<RegistryProvider>,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub offerings: Vec<Value>,
    #[serde(default)]
    pub dialects: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryProvider {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<RegistryAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RegistryAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_slot: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_vars: Vec<String>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedRegistry {
    pub registry: Registry,
    pub key_id: String,
    pub sha256: String,
}

pub fn verify_registry_envelope(
    bytes: &[u8],
    floor: Option<u64>,
    keys: &[TrustedKey],
    now_unix: u64,
) -> Result<VerifiedRegistry, FactsRejection> {
    let Authenticated {
        envelope,
        payload,
        key_id,
        key_role,
    } = authenticate(
        bytes,
        REGISTRY_CHANNEL,
        keys,
        EnvelopeLimits {
            envelope_bytes: MAX_REGISTRY_ENVELOPE_BYTES,
            payload_bytes: MAX_REGISTRY_PAYLOAD_BYTES,
            facts_metadata: false,
        },
    )?;
    if key_role != KeyRole::Founder {
        return Err(FactsRejection::OutOfScope(
            "the registry needs a founder signature".into(),
        ));
    }
    let (Some(outer_published), Some(outer_sha)) = (&envelope.published_at, &envelope.sha256)
    else {
        return Err(FactsRejection::BadEnvelope(
            "missing signed-payload metadata".into(),
        ));
    };
    let raw: Value = serde_json::from_slice(&payload)
        .map_err(|err| FactsRejection::BadPayload(err.to_string()))?;
    let schema = raw
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != REGISTRY_SCHEMA {
        return Err(FactsRejection::WrongSchema {
            got: schema.chars().take(64).collect(),
        });
    }
    let registry: Registry =
        serde_json::from_value(raw).map_err(|err| FactsRejection::BadPayload(err.to_string()))?;
    if envelope.channel != REGISTRY_CHANNEL {
        return Err(FactsRejection::WrongChannel {
            expected: REGISTRY_CHANNEL.to_string(),
            got: envelope.channel,
        });
    }
    if envelope.facts_version != registry.revision {
        return Err(FactsRejection::Mismatch(format!(
            "revision {} vs {}",
            envelope.facts_version, registry.revision
        )));
    }
    if outer_published != &registry.generated_at {
        return Err(FactsRejection::Mismatch("generated_at".into()));
    }
    if registry.revision == 0 {
        return Err(FactsRejection::BadPayload(
            "revision must be positive".into(),
        ));
    }
    let generated_at = parse_rfc3339_utc(&registry.generated_at)
        .ok_or_else(|| FactsRejection::BadPayload("invalid generated_at timestamp".into()))?;
    if generated_at > now_unix.saturating_add(FUTURE_CLOCK_TOLERANCE_SECS) {
        return Err(FactsRejection::BadPayload(
            "generated_at is in the future".into(),
        ));
    }
    if let Some(highest) = floor
        && registry.revision < highest
    {
        return Err(FactsRejection::Rollback {
            got: registry.revision,
            highest_seen: highest,
        });
    }
    let sha256 = sha256_hex(&payload);
    if outer_sha != &sha256 {
        return Err(FactsRejection::Mismatch("sha256".into()));
    }
    Ok(VerifiedRegistry {
        registry,
        key_id,
        sha256,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub default_model: Option<String>,
    pub aliases: Vec<String>,
}

impl PulledProvider {
    #[must_use]
    pub fn secret_slot(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn api_key_env(&self) -> String {
        derived_env_var(&self.id)
    }

    #[must_use]
    pub fn has_credential(
        &self,
        secret_present: impl Fn(&str) -> bool,
        env: impl Fn(&str) -> Option<String>,
    ) -> bool {
        secret_present(self.secret_slot())
            || env(&self.api_key_env()).is_some_and(|value| !value.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedProvider {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedRegistry {
    pub revision: u64,
    pub key_id: String,
    pub sha256: String,
    pub registry: Registry,
    pub providers: Vec<PulledProvider>,
    pub builtin_rows: Vec<String>,
    pub refused: Vec<RefusedProvider>,
}

impl AdmittedRegistry {
    #[must_use]
    pub fn usable_providers(
        &self,
        secret_present: impl Fn(&str) -> bool,
        env: impl Fn(&str) -> Option<String>,
    ) -> Vec<&PulledProvider> {
        self.providers
            .iter()
            .filter(|provider| provider.has_credential(&secret_present, &env))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct BuiltinNames {
    pub names: BTreeSet<String>,
    pub env_vars: BTreeSet<String>,
}

impl BuiltinNames {
    #[must_use]
    pub fn bundled() -> Self {
        let mut out = Self::default();
        for provider in crate::provider::all_providers() {
            out.names.insert(normalize(provider.id()));
            out.names.insert(normalize(provider.provider_config_key()));
            out.names
                .insert(normalize(provider.kind().secret_store_slot()));
            out.names
                .extend(provider.aliases().iter().map(|a| normalize(a)));
            out.env_vars
                .extend(provider.env_vars().iter().map(|v| v.to_string()));
        }
        for descriptor in crate::descriptors::bundled_provider_descriptors() {
            out.names.insert(normalize(&descriptor.id));
            out.names
                .extend(descriptor.aliases.iter().map(|a| normalize(a)));
            out.env_vars.insert(descriptor.api_key_env.clone());
        }
        for row in crate::descriptors::provider_compatibility() {
            out.names.extend(
                [
                    row.id,
                    row.config_key,
                    row.base_url_config_key,
                    row.catalog_id,
                    row.catalog_source_id,
                    row.tui_wire_tag,
                ]
                .into_iter()
                .chain(row.subagent_aliases.iter().copied())
                .chain(row.selector_aliases.iter().copied())
                .map(normalize),
            );
        }
        out
    }

    fn reserved(&self, name: &str) -> bool {
        self.names.contains(name) || crate::ProviderKind::parse_config_identity(name).is_some()
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['_', '.'], "-")
}

#[must_use]
pub fn derived_env_var(id: &str) -> String {
    format!("{}_API_KEY", id.to_ascii_uppercase().replace('-', "_"))
}

fn valid_slug(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=MAX_PROVIDER_ID_CHARS).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !value.ends_with('-')
}

fn plain(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

fn public_domain(host: Option<url::Host<&str>>) -> bool {
    let Some(url::Host::Domain(domain)) = host else {
        return false;
    };
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    domain.contains('.')
        && !domain.ends_with(".localhost")
        && !domain.ends_with(".local")
        && !domain.ends_with(".internal")
}

fn https_base_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    (parsed.scheme() == "https"
        && public_domain(parsed.host())
        && parsed.port().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && value.len() <= 512)
        .then(|| value.trim().trim_end_matches('/').to_string())
}

fn admit_row(
    row: &RegistryProvider,
    builtins: &BuiltinNames,
    taken: &BTreeSet<String>,
) -> Result<PulledProvider, String> {
    let id = row.id.trim();
    if !valid_slug(id) {
        return Err("id is not a lowercase slug".into());
    }
    if row.kind.is_some() {
        return Err("a provider kind needs a release".into());
    }
    if !row
        .wire_policy
        .as_deref()
        .is_some_and(|wire| wire == OPENAI_COMPATIBLE_WIRE)
    {
        return Err("only OpenAI-compatible providers can be pulled".into());
    }
    let base_url = row
        .base_url
        .as_deref()
        .and_then(https_base_url)
        .ok_or_else(|| {
            "base_url must be https on a public host name and the default port, without credentials, query or fragment".to_string()
        })?;
    let mut aliases = Vec::with_capacity(row.aliases.len());
    for alias in &row.aliases {
        let alias = normalize(alias);
        if !valid_slug(&alias) {
            return Err(format!("alias {alias:?} is not a lowercase slug"));
        }
        if alias != id {
            aliases.push(alias);
        }
    }
    for name in std::iter::once(id).chain(aliases.iter().map(String::as_str)) {
        if builtins.reserved(name) {
            return Err(format!("{name:?} names a built-in provider"));
        }
        if taken.contains(name) {
            return Err(format!("{name:?} is already taken"));
        }
    }
    let env_var = derived_env_var(id);
    if builtins.env_vars.contains(&env_var) {
        return Err(format!("{env_var} belongs to a built-in provider"));
    }
    if let Some(auth) = &row.auth {
        if auth.secret_slot.as_deref().is_some_and(|slot| slot != id) {
            return Err("secret slot must be the provider's own id".into());
        }
        if let Some(other) = auth.env_vars.iter().find(|var| **var != env_var) {
            return Err(format!("env var {other} is not {env_var}"));
        }
    }
    let label = row.label.as_deref().unwrap_or(id);
    if !plain(label, 80) {
        return Err("label is empty, too long or not plain".into());
    }
    if row
        .default_model
        .as_deref()
        .is_some_and(|model| !plain(model, 200))
    {
        return Err("default_model is empty, too long or not plain".into());
    }
    Ok(PulledProvider {
        id: id.to_string(),
        label: label.trim().to_string(),
        base_url,
        default_model: row.default_model.as_deref().map(|m| m.trim().to_string()),
        aliases,
    })
}

#[must_use]
pub fn admit_registry(verified: VerifiedRegistry, builtins: &BuiltinNames) -> AdmittedRegistry {
    let mut providers = Vec::new();
    let mut builtin_rows = Vec::new();
    let mut refused = Vec::new();
    let mut taken = BTreeSet::new();
    for row in &verified.registry.providers {
        let normalized = normalize(&row.id);
        if builtins.reserved(&normalized) {
            builtin_rows.push(normalized);
            continue;
        }
        match admit_row(row, builtins, &taken) {
            Ok(provider) => {
                taken.insert(provider.id.clone());
                taken.extend(provider.aliases.iter().cloned());
                providers.push(provider);
            }
            Err(reason) => refused.push(RefusedProvider {
                id: row.id.chars().take(64).collect(),
                reason,
            }),
        }
    }
    AdmittedRegistry {
        revision: verified.registry.revision,
        key_id: verified.key_id,
        sha256: verified.sha256,
        registry: verified.registry,
        providers,
        builtin_rows,
        refused,
    }
}

#[cfg(test)]
mod tests;
