use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::keys::{KeyRole, TrustedKey};
use super::types::{ModelFact, PricingFact};
use super::verify::{
    Authenticated, EnvelopeLimits, FUTURE_CLOCK_TOLERANCE_SECS, FactsRejection, authenticate,
    check_automation_window, parse_rfc3339_utc, sha256_hex,
};
use crate::catalog::{CatalogOffering, CatalogSource};
use crate::descriptors::{
    DESCRIPTORS_JSON, DescriptorDiscovery, DescriptorWire, ModelIdsPolicy, ProviderDescriptor,
    model_rules_for, normalize_model_lookup_key,
};
use crate::models_dev::{ModelsDevCost, ModelsDevLimit, ModelsDevModel, ModelsDevProviderModel};
use crate::route::{
    CapabilityState, ModelId, OPENCODE_ZEN_DEPRECATED_ENDPOINT_KEY, ProviderModelOffering,
    RouteCapabilities, RouteLimits, opencode_zen_endpoint_key_for_npm,
};

pub const REGISTRY_SCHEMA: &str = "codewhale.registry.v1";
pub const REGISTRY_CHANNEL: &str = "registry";
pub const MAX_REGISTRY_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_REGISTRY_ENVELOPE_BYTES: usize = MAX_REGISTRY_PAYLOAD_BYTES / 3 * 4 + 64 * 1024;
const OPENAI_COMPATIBLE_WIRE: &str = "openai-compatible";
const MAX_PROVIDER_ID_CHARS: usize = 48;

const PRODUCT_RESERVED_ENV_VARS: &[&str] = &[
    "ARK_API_KEY",
    "BAIDU_SEARCH_API_KEY",
    "BRAVE_API_KEY",
    "EXA_API_KEY",
    "FIRECRAWL_API_KEY",
    "GEMINI_API_KEY",
    "GLM_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "JINA_API_KEY",
    "KIMI_API_KEY",
    "METASO_API_KEY",
    "MIMO_API_KEY",
    "MIMO_TOKEN_PLAN_API_KEY",
    "MODEL_API_KEY",
    "NVIDIA_API_KEY",
    "OLLAMA_API_KEY",
    "OLLAMA_CLOUD_API_KEY",
    "OPENCODE_API_KEY",
    "PERPLEXITY_API_KEY",
    "SERPER_API_KEY",
    "SERPLY_API_KEY",
    "STEP_API_KEY",
    "TAVILY_API_KEY",
    "VOLCENGINE_ARK_API_KEY",
    "WANJIE_API_KEY",
    "WANJIE_MAAS_API_KEY",
    "XIAOMI_API_KEY",
    "XIAOMI_MIMO_TOKEN_PLAN_API_KEY",
    "ZHIPU_API_KEY",
    "Z_AI_API_KEY",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: String,
    pub revision: u64,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
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
    pub key_role: KeyRole,
    pub sha256: String,
    pub generated_at_unix: u64,
    pub not_after: Option<u64>,
}

#[derive(Debug, Clone)]
struct BundledProviderTrust {
    kind: Option<String>,
    base_url: Option<String>,
    wire_policy: Option<String>,
    wire: Option<String>,
    secret_slot: Option<String>,
    env_vars: Vec<String>,
    api_key_env: Option<String>,
    default_model: Option<String>,
    is_descriptor: bool,
    raw: Map<String, Value>,
}

static BUNDLED_PROVIDER_TRUST: OnceLock<BTreeMap<String, BundledProviderTrust>> = OnceLock::new();

fn bundled_provider_trust() -> &'static BTreeMap<String, BundledProviderTrust> {
    BUNDLED_PROVIDER_TRUST.get_or_init(|| {
        let parsed: Value = serde_json::from_str(DESCRIPTORS_JSON)
            .expect("committed provider_descriptors.json must parse");
        let mut map = BTreeMap::new();
        if let Some(providers) = parsed.get("providers").and_then(Value::as_array) {
            for entry in providers {
                let Some(id) = entry.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let env_vars = entry
                    .get("env_vars")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                map.insert(
                    id.to_string(),
                    BundledProviderTrust {
                        kind: entry.get("kind").and_then(Value::as_str).map(String::from),
                        base_url: entry
                            .get("base_url")
                            .and_then(Value::as_str)
                            .map(String::from),
                        wire_policy: entry
                            .get("wire_policy")
                            .and_then(Value::as_str)
                            .map(String::from),
                        wire: None,
                        secret_slot: entry
                            .get("secret_store_slot")
                            .and_then(Value::as_str)
                            .map(String::from),
                        env_vars,
                        api_key_env: None,
                        default_model: entry
                            .get("default_model")
                            .and_then(Value::as_str)
                            .map(String::from),
                        is_descriptor: false,
                        raw: entry.as_object().cloned().unwrap_or_default(),
                    },
                );
            }
        }
        if let Some(descriptors) = parsed.get("descriptors").and_then(Value::as_array) {
            for entry in descriptors {
                let Some(id) = entry.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let wire = entry.get("wire").and_then(Value::as_str).map(String::from);
                let api_key_env = entry
                    .get("api_key_env")
                    .and_then(Value::as_str)
                    .map(String::from);
                map.insert(
                    id.to_string(),
                    BundledProviderTrust {
                        kind: None,
                        base_url: entry
                            .get("base_url")
                            .and_then(Value::as_str)
                            .map(String::from),
                        wire_policy: wire
                            .clone()
                            .or_else(|| Some(OPENAI_COMPATIBLE_WIRE.to_string())),
                        wire,
                        secret_slot: Some(id.to_string()),
                        env_vars: api_key_env.iter().cloned().collect(),
                        api_key_env,
                        default_model: entry
                            .get("default_model")
                            .and_then(Value::as_str)
                            .map(String::from),
                        is_descriptor: true,
                        raw: entry.as_object().cloned().unwrap_or_default(),
                    },
                );
            }
        }
        map
    })
}

fn automation_provider_violation(
    registry: &Registry,
    raw_providers: Option<&Value>,
) -> Option<String> {
    if registry.providers.is_empty() {
        return None;
    }
    let Some(raw_list) = raw_providers.and_then(Value::as_array) else {
        return Some("providers do not match signed bytes".into());
    };
    if raw_list.len() != registry.providers.len() {
        return Some("providers do not match signed bytes".into());
    }
    let trust = bundled_provider_trust();
    let mut seen = BTreeSet::new();
    for (row, raw_row) in registry.providers.iter().zip(raw_list.iter()) {
        let Some(obj) = raw_row.as_object() else {
            return Some("malformed provider row".into());
        };
        if !seen.insert(row.id.as_str()) {
            return Some(format!("duplicate provider {:?}", row.id));
        }
        if let Some(adm) = obj.get("admission")
            && !adm.is_null()
        {
            let Some(adm_obj) = adm.as_object() else {
                return Some(format!("provider {:?} has malformed admission", row.id));
            };
            if adm_obj
                .get("founder_signature")
                .is_some_and(|v| !v.is_null())
            {
                return Some(format!(
                    "provider {:?} requires a founder signature",
                    row.id
                ));
            }
        }
        let Some(bundled) = trust.get(&row.id) else {
            return Some(format!(
                "provider {:?} requires a founder signature",
                row.id
            ));
        };
        if let Some(v) = obj.get("kind")
            && v.as_str() != bundled.kind.as_deref()
        {
            return Some(format!("provider {:?} changes kind", row.id));
        }
        if let Some(v) = obj.get("base_url")
            && v.as_str() != bundled.base_url.as_deref()
        {
            return Some(format!("provider {:?} changes base_url", row.id));
        }
        if let Some(v) = obj.get("wire_policy")
            && v.as_str() != bundled.wire_policy.as_deref()
        {
            return Some(format!("provider {:?} changes wire_policy", row.id));
        }
        if let Some(v) = obj.get("wire")
            && v.as_str() != bundled.wire.as_deref()
        {
            return Some(format!("provider {:?} changes wire", row.id));
        }
        if let Some(v) = obj.get("api_key_env")
            && v.as_str() != bundled.api_key_env.as_deref()
        {
            return Some(format!("provider {:?} changes api_key_env", row.id));
        }
        if let Some(v) = obj.get("secret_store_slot")
            && v.as_str() != bundled.secret_slot.as_deref()
        {
            return Some(format!("provider {:?} changes secret_store_slot", row.id));
        }
        if let Some(v) = obj.get("env_vars") {
            let got = v
                .as_array()
                .and_then(|arr| arr.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
            let expected: Vec<&str> = bundled.env_vars.iter().map(String::as_str).collect();
            if got.as_deref() != Some(expected.as_slice()) {
                return Some(format!("provider {:?} changes env_vars", row.id));
            }
        }
        if let Some(v) = obj.get("auth") {
            let Some(auth_obj) = v.as_object() else {
                return Some(format!("provider {:?} changes auth", row.id));
            };
            let Some(auth) = &row.auth else {
                return Some(format!("provider {:?} changes auth", row.id));
            };
            if !auth.rest.is_empty()
                || auth_obj.is_empty()
                || (auth_obj.contains_key("secret_slot")
                    && auth.secret_slot.as_deref() != bundled.secret_slot.as_deref())
                || (auth_obj.contains_key("env_vars") && auth.env_vars != bundled.env_vars)
            {
                return Some(format!("provider {:?} changes auth", row.id));
            }
        }
        for (key, value) in obj {
            if value.is_null()
                || matches!(
                    key.as_str(),
                    "id" | "admission"
                        | "auth"
                        | "kind"
                        | "base_url"
                        | "wire_policy"
                        | "wire"
                        | "api_key_env"
                        | "secret_store_slot"
                        | "env_vars"
                )
            {
                continue;
            }
            if key == "models" {
                let Some(models) = value.as_object() else {
                    return Some(format!("provider {:?} has malformed models", row.id));
                };
                let bundled_models = bundled.raw.get("models").and_then(Value::as_object);
                for (rule, rule_value) in models {
                    if rule_value.is_null() || matches!(rule.as_str(), "aliases" | "wire_slugs") {
                        continue;
                    }
                    if bundled_models.and_then(|m| m.get(rule)) != Some(rule_value) {
                        return Some(format!(
                            "provider {:?} changes models.{rule}, which requires a founder signature",
                            row.id
                        ));
                    }
                }
                continue;
            }
            if bundled.raw.get(key) != Some(value) {
                return Some(format!(
                    "provider {:?} changes {key}, which requires a founder signature",
                    row.id
                ));
            }
        }
    }
    None
}

fn automation_offering_violation(offerings: &[Value]) -> Option<String> {
    let is_true = |obj: Option<&Map<String, Value>>, key: &str| {
        obj.and_then(|o| o.get(key)).and_then(Value::as_bool) == Some(true)
    };
    for offering in offerings {
        let Some(obj) = offering.as_object() else {
            continue;
        };
        let label = format!(
            "offering {:?}/{:?}",
            obj.get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            obj.get("id")
                .or_else(|| obj.get("wire_model_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        if obj.get("pricing_withheld").is_some_and(|v| !v.is_null()) {
            return Some(format!(
                "{label} sets pricing_withheld, which requires a founder signature"
            ));
        }
        if obj
            .get("corrections")
            .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Some(format!(
                "{label} carries corrections, which require a founder signature"
            ));
        }
        let catalog = obj.get("catalog").and_then(Value::as_object);
        let transport = obj.get("transport").and_then(Value::as_object);
        if is_true(Some(obj), "default")
            || is_true(Some(obj), "default_for_provider")
            || is_true(catalog, "default")
            || is_true(catalog, "default_for_provider")
            || is_true(transport, "default_for_provider")
        {
            return Some(format!(
                "{label} changes the provider default, which requires a founder signature"
            ));
        }
    }
    None
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
    let raw_providers = raw.get("providers").cloned();
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
    if envelope.not_after.is_some() && envelope.not_after != registry.not_after {
        return Err(FactsRejection::Mismatch("not_after".into()));
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
    let not_after = registry
        .not_after
        .as_deref()
        .map(|value| {
            parse_rfc3339_utc(value)
                .ok_or_else(|| FactsRejection::BadPayload("invalid not_after timestamp".into()))
        })
        .transpose()?;
    if let Some(expires) = not_after
        && expires <= generated_at
    {
        return Err(FactsRejection::BadPayload(
            "not_after does not follow generated_at".into(),
        ));
    }
    match key_role {
        KeyRole::Founder => {
            if not_after.is_some_and(|expires| now_unix >= expires) {
                return Err(FactsRejection::BadPayload("not_after has expired".into()));
            }
        }
        KeyRole::Automation => {
            check_automation_window(generated_at, not_after, now_unix)?;
            if let Some(violation) =
                automation_provider_violation(&registry, raw_providers.as_ref())
                    .or_else(|| automation_offering_violation(&registry.offerings))
            {
                return Err(FactsRejection::OutOfScope(violation));
            }
        }
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
        key_role,
        sha256,
        generated_at_unix: generated_at,
        not_after,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub default_model: Option<String>,
    pub aliases: Vec<String>,
    pub discovery: DescriptorDiscovery,
    pub docs_url: Option<String>,
    pub credential_url: Option<String>,
    pub guidance: Option<String>,
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

    #[must_use]
    pub fn to_descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.id.clone(),
            label: self.label.clone(),
            wire: DescriptorWire::OpenaiCompatible,
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env(),
            default_model: self
                .default_model
                .clone()
                .unwrap_or_else(|| self.id.clone()),
            discovery: self.discovery,
            docs_url: self.docs_url.clone(),
            credential_url: self.credential_url.clone(),
            guidance: self.guidance.clone(),
            aliases: self.aliases.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedProvider {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompiledNamingRules {
    pub strip_prefixes: Vec<String>,
    pub accept: Vec<String>,
    pub aliases: BTreeMap<String, String>,
    pub wire_slugs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CompiledRegistry {
    pub naming: BTreeMap<String, CompiledNamingRules>,
    pub provider_defaults: BTreeMap<String, String>,
    pub descriptor_defaults: BTreeMap<String, String>,
    pub intrinsic_models: BTreeMap<String, crate::catalog::reviewed::IntrinsicModel>,
    pub catalog_offerings: Vec<CatalogOffering>,
    pub route_offerings: Vec<ProviderModelOffering>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedRegistry {
    pub revision: u64,
    pub key_id: String,
    pub sha256: String,
    pub not_after: Option<u64>,
    pub registry: Registry,
    pub providers: Vec<PulledProvider>,
    pub builtin_rows: Vec<String>,
    pub refused: Vec<RefusedProvider>,
    pub compiled: CompiledRegistry,
}

impl AdmittedRegistry {
    #[must_use]
    pub fn is_expired_at(&self, now_unix: u64) -> bool {
        self.not_after.is_some_and(|expires| now_unix >= expires)
    }

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
        out.env_vars
            .extend(PRODUCT_RESERVED_ENV_VARS.iter().map(|v| (*v).to_string()));
        out
    }

    fn reserved(&self, name: &str) -> bool {
        self.names.contains(name) || crate::ProviderKind::parse_config_identity(name).is_some()
    }

    fn reserved_env(&self, env_var: &str) -> bool {
        env_var.starts_with("CODEWHALE_")
            || env_var.starts_with("DEEPSEEK_")
            || self.env_vars.contains(env_var)
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

fn valid_model_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
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

fn https_doc_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let parsed = url::Url::parse(trimmed).ok()?;
    (parsed.scheme() == "https"
        && public_domain(parsed.host())
        && parsed.port().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && trimmed.len() <= 512
        && !trimmed.chars().any(char::is_control))
    .then(|| trimmed.to_string())
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
    if row
        .rest
        .get("wire")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(OPENAI_COMPATIBLE_WIRE))
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
    if builtins.reserved_env(&env_var) {
        return Err(format!("{env_var} belongs to a built-in provider"));
    }
    if row
        .rest
        .get("api_key_env")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(env_var.as_str()))
    {
        return Err(format!("api_key_env is not {env_var}"));
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
    let discovery = match row.rest.get("discovery").and_then(Value::as_str) {
        Some("models_endpoint") => DescriptorDiscovery::ModelsEndpoint,
        _ => DescriptorDiscovery::None,
    };
    let docs_url = row
        .rest
        .get("docs_url")
        .and_then(Value::as_str)
        .and_then(https_doc_url);
    let credential_url = row
        .rest
        .get("credential_url")
        .and_then(Value::as_str)
        .and_then(https_doc_url);
    let guidance = row
        .rest
        .get("guidance")
        .and_then(Value::as_str)
        .filter(|g| plain(g, 300))
        .map(|g| g.trim().to_string());
    Ok(PulledProvider {
        id: id.to_string(),
        label: label.trim().to_string(),
        base_url,
        default_model: row.default_model.as_deref().map(|m| m.trim().to_string()),
        aliases,
        discovery,
        docs_url,
        credential_url,
        guidance,
    })
}

fn cloud_default_eligible(provider: &str) -> bool {
    crate::ProviderKind::parse_config_identity(provider).is_some_and(|kind| {
        kind.as_str() == provider
            && !matches!(
                kind,
                crate::ProviderKind::Custom
                    | crate::ProviderKind::Ollama
                    | crate::ProviderKind::Sglang
                    | crate::ProviderKind::Vllm
                    | crate::ProviderKind::OpenaiCodex
                    | crate::ProviderKind::Antigravity
            )
    })
}

fn parse_naming_block(
    provider_id: &str,
    block: &Map<String, Value>,
    out: &mut CompiledNamingRules,
) {
    let builtin_verbatim =
        crate::ProviderKind::parse_config_identity(provider_id).is_some_and(|kind| {
            kind.as_str() == provider_id && model_rules_for(kind).ids == ModelIdsPolicy::Verbatim
        });
    if builtin_verbatim {
        return;
    }
    let block_verbatim = block
        .get("ids")
        .and_then(Value::as_str)
        .is_some_and(|ids| ids == "verbatim");
    if let Some(prefixes) = block.get("strip_prefixes").and_then(Value::as_array) {
        for item in prefixes.iter().filter_map(Value::as_str) {
            let trimmed = item.trim();
            if !trimmed.is_empty()
                && trimmed.len() <= 64
                && !trimmed.chars().any(char::is_control)
                && !out
                    .strip_prefixes
                    .iter()
                    .any(|existing| existing == trimmed)
            {
                out.strip_prefixes.push(trimmed.to_string());
            }
        }
    }
    if let Some(accepts) = block.get("accept").and_then(Value::as_array) {
        for item in accepts.iter().filter_map(Value::as_str) {
            let trimmed = item.trim();
            if !trimmed.is_empty()
                && trimmed.len() <= 128
                && !trimmed.chars().any(char::is_control)
                && !out.accept.iter().any(|existing| existing == trimmed)
            {
                out.accept.push(trimmed.to_string());
            }
        }
    }
    if !block_verbatim && let Some(aliases) = block.get("aliases").and_then(Value::as_object) {
        for (alias, target) in aliases {
            let Some(target) = target.as_str() else {
                continue;
            };
            let key = normalize_model_lookup_key(alias);
            let target = target.trim();
            if !key.is_empty() && valid_model_identifier(target) {
                out.aliases.insert(key, target.to_string());
            }
        }
    }
    if let Some(slugs) = block.get("wire_slugs").and_then(Value::as_object) {
        for (model, slug) in slugs {
            let Some(slug) = slug.as_str() else {
                continue;
            };
            let key = normalize_model_lookup_key(model);
            let slug = slug.trim();
            if !key.is_empty() && valid_model_identifier(slug) {
                out.wire_slugs.insert(key, slug.to_string());
            }
        }
    }
}

fn fill_limit_shorthands(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    if !target.contains_key("limit") {
        let context = source
            .get("context_window")
            .and_then(Value::as_u64)
            .or_else(|| {
                source
                    .get("limits")
                    .and_then(Value::as_object)
                    .and_then(|l| l.get("context_tokens"))
                    .and_then(Value::as_u64)
            });
        let input = source
            .get("limits")
            .and_then(Value::as_object)
            .and_then(|l| l.get("input_tokens"))
            .and_then(Value::as_u64);
        let output = source
            .get("max_output")
            .and_then(Value::as_u64)
            .or_else(|| {
                source
                    .get("limits")
                    .and_then(Value::as_object)
                    .and_then(|l| l.get("output_tokens"))
                    .and_then(Value::as_u64)
            });
        if context.is_some() || input.is_some() || output.is_some() {
            let mut limit = Map::new();
            if let Some(v) = context {
                limit.insert("context".into(), Value::from(v));
            }
            if let Some(v) = input {
                limit.insert("input".into(), Value::from(v));
            }
            if let Some(v) = output {
                limit.insert("output".into(), Value::from(v));
            }
            target.insert("limit".into(), Value::Object(limit));
        }
    }
    if !target.contains_key("name")
        && let Some(display) = source.get("display_name").and_then(Value::as_str)
    {
        target.insert("name".into(), Value::String(display.to_string()));
    }
}

fn parse_registry_model(
    raw_model: &Value,
    revision: u64,
) -> Option<(
    String,
    ModelsDevModel,
    crate::catalog::reviewed::IntrinsicModel,
)> {
    let obj = raw_model.as_object()?;
    let id = obj.get("id").and_then(Value::as_str)?.trim();
    if !plain(id, 512) {
        return None;
    }
    let mut cat_map = match obj.get("catalog").and_then(Value::as_object) {
        Some(cat) => cat.clone(),
        None => obj.clone(),
    };
    cat_map.insert("id".into(), Value::String(id.to_string()));
    fill_limit_shorthands(obj, &mut cat_map);
    let md_model: ModelsDevModel = serde_json::from_value(Value::Object(cat_map)).ok()?;
    if md_model
        .limit
        .as_ref()
        .is_some_and(|l| l.context == Some(0) || l.output == Some(0))
    {
        return None;
    }
    let mut intrinsic = crate::catalog::reviewed::from_canonical(&md_model);
    intrinsic.source = format!("registry v{revision}");
    if let Some(intr_obj) = obj.get("intrinsic").and_then(Value::as_object) {
        let mut intr_copy = intr_obj.clone();
        intr_copy
            .entry("source".to_string())
            .or_insert_with(|| Value::String(format!("registry v{revision}")));
        if let Ok(mut parsed_intr) = serde_json::from_value::<
            crate::catalog::reviewed::IntrinsicModel,
        >(Value::Object(intr_copy))
            && parsed_intr.context_window != Some(0)
            && parsed_intr.max_output != Some(0)
            && parsed_intr.generation_default != Some(0)
        {
            crate::catalog::reviewed::fill_missing(&mut parsed_intr, &intrinsic);
            intrinsic = parsed_intr;
        }
    }
    Some((id.to_ascii_lowercase(), md_model, intrinsic))
}

#[derive(Debug, Deserialize, Default)]
struct OfferingTransportOverlay {
    #[serde(default)]
    endpoint_key: Option<String>,
    #[serde(default)]
    canonical_model: Option<String>,
    #[serde(default)]
    default_for_provider: bool,
    #[serde(default)]
    limits: RouteLimits,
    #[serde(default)]
    capabilities: RouteCapabilities,
}

fn merge_canonical_into_offering(md: &mut ModelsDevProviderModel, canonical: &ModelsDevModel) {
    if md.name.is_none() {
        md.name.clone_from(&canonical.name);
    }
    if md.family.is_none() {
        md.family.clone_from(&canonical.family);
    }
    if md.description.is_none() {
        md.description.clone_from(&canonical.description);
    }
    if md.model_type.is_none() {
        md.model_type.clone_from(&canonical.model_type);
    }
    if md.knowledge.is_none() {
        md.knowledge.clone_from(&canonical.knowledge);
    }
    if md.release_date.is_none() {
        md.release_date.clone_from(&canonical.release_date);
    }
    if md.last_updated.is_none() {
        md.last_updated.clone_from(&canonical.last_updated);
    }
    md.attachment = md.attachment.or(canonical.attachment);
    md.reasoning = md.reasoning.or(canonical.reasoning);
    md.tool_call = md.tool_call.or(canonical.tool_call);
    md.structured_output = md.structured_output.or(canonical.structured_output);
    md.temperature = md.temperature.or(canonical.temperature);
    md.open_weights = md.open_weights.or(canonical.open_weights);
    if md.modalities.is_none() {
        md.modalities.clone_from(&canonical.modalities);
    }
    if let Some(canon_limit) = &canonical.limit {
        let mut limit = md.limit.clone().unwrap_or_default();
        limit.context = limit.context.or(canon_limit.context);
        limit.input = limit.input.or(canon_limit.input);
        limit.output = limit.output.or(canon_limit.output);
        if md.limit.is_some() || limit != ModelsDevLimit::default() {
            md.limit = Some(limit);
        }
    }
}

fn merge_capability(primary: CapabilityState, fallback: CapabilityState) -> CapabilityState {
    if primary != CapabilityState::Unknown {
        primary
    } else {
        fallback
    }
}

fn compile_registry(
    verified: &VerifiedRegistry,
    admitted_providers: &[PulledProvider],
) -> CompiledRegistry {
    let trust = bundled_provider_trust();
    let mut allowed_providers: BTreeSet<String> = trust
        .keys()
        .filter(|id| !matches!(id.as_str(), "antigravity" | "custom"))
        .cloned()
        .collect();
    for pulled in admitted_providers {
        allowed_providers.insert(pulled.id.clone());
    }

    let mut naming: BTreeMap<String, CompiledNamingRules> = BTreeMap::new();
    let mut provider_defaults: BTreeMap<String, String> = BTreeMap::new();
    let mut descriptor_defaults: BTreeMap<String, String> = BTreeMap::new();
    let mut provider_headers: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut provider_withheld: BTreeMap<String, String> = BTreeMap::new();

    for row in &verified.registry.providers {
        let id = row.id.trim();
        if !allowed_providers.contains(id) {
            continue;
        }
        if let Some(models_obj) = row.rest.get("models").and_then(Value::as_object) {
            let entry = naming.entry(id.to_string()).or_default();
            parse_naming_block(id, models_obj, entry);
        }
        if let Some(cat_obj) = row.rest.get("catalog").and_then(Value::as_object) {
            let api = cat_obj
                .get("api")
                .and_then(Value::as_str)
                .map(str::to_string);
            let npm = cat_obj
                .get("npm")
                .and_then(Value::as_str)
                .map(str::to_string);
            provider_headers.insert(id.to_string(), (api, npm));
        }
        if let Some(corrs) = row.rest.get("corrections").and_then(Value::as_array) {
            for corr in corrs {
                if let Some(reason) = corr
                    .as_object()
                    .and_then(|c| c.get("pricing_withheld"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    provider_withheld.insert(id.to_string(), reason.to_string());
                }
            }
        }
        if let Some(default_model) = row.default_model.as_deref().map(str::trim).filter(|m| {
            valid_model_identifier(m)
                && !m.eq_ignore_ascii_case("auto")
                && !m.eq_ignore_ascii_case("unknown")
        }) && let Some(bundled) = trust.get(id)
        {
            if bundled.is_descriptor {
                if bundled.default_model.as_deref() != Some(default_model) {
                    descriptor_defaults.insert(id.to_string(), default_model.to_string());
                }
            } else if cloud_default_eligible(id)
                && bundled.default_model.as_deref() != Some(default_model)
            {
                provider_defaults.insert(id.to_string(), default_model.to_string());
            }
        }
    }

    let mut canonical_models: BTreeMap<String, ModelsDevModel> = BTreeMap::new();
    let mut intrinsic_models: BTreeMap<String, crate::catalog::reviewed::IntrinsicModel> =
        BTreeMap::new();
    for raw_model in &verified.registry.models {
        if let Some((key, md_model, intrinsic)) =
            parse_registry_model(raw_model, verified.registry.revision)
        {
            canonical_models.insert(key.clone(), md_model);
            intrinsic_models.insert(key, intrinsic);
        }
    }

    let source = CatalogSource::CloudFacts {
        facts_version: verified.registry.revision,
        key_id: verified.key_id.clone(),
        fetched_at: verified.generated_at_unix.min(crate::catalog::now_unix()),
        valid_until: verified.not_after,
    };

    let mut pulled_catalog_rows: BTreeMap<(String, String), CatalogOffering> = BTreeMap::new();
    let mut pulled_route_rows: BTreeMap<(String, String), ProviderModelOffering> = BTreeMap::new();
    let mut providers_with_pulled_default: BTreeSet<String> = BTreeSet::new();

    for raw_off in &verified.registry.offerings {
        let Some(obj) = raw_off.as_object() else {
            continue;
        };
        let Some(provider) = obj
            .get("provider")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| allowed_providers.contains(*p))
        else {
            continue;
        };
        let Some(wire_id) = obj
            .get("id")
            .or_else(|| obj.get("wire_model_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| valid_model_identifier(id))
        else {
            continue;
        };

        if let Some(sel_aliases) = obj
            .get("selection")
            .and_then(Value::as_object)
            .and_then(|s| s.get("aliases"))
            .and_then(Value::as_array)
        {
            let builtin_verbatim = crate::ProviderKind::parse_config_identity(provider)
                .is_some_and(|kind| {
                    kind.as_str() == provider
                        && model_rules_for(kind).ids == ModelIdsPolicy::Verbatim
                });
            if !builtin_verbatim {
                let entry = naming.entry(provider.to_string()).or_default();
                for alias in sel_aliases.iter().filter_map(Value::as_str) {
                    let key = normalize_model_lookup_key(alias);
                    if !key.is_empty() {
                        entry
                            .aliases
                            .entry(key)
                            .or_insert_with(|| wire_id.to_string());
                    }
                }
            }
        }

        let transport_overlay: Option<OfferingTransportOverlay> = obj
            .get("transport")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let has_catalog_block = obj.get("catalog").is_some_and(Value::is_object);
        let has_flat_facts = !has_catalog_block
            && (obj.contains_key("limit")
                || obj.contains_key("limits")
                || obj.contains_key("context_window")
                || obj.contains_key("max_output")
                || obj.contains_key("cost")
                || obj.contains_key("pricing")
                || obj.contains_key("reasoning")
                || obj.contains_key("tool_call")
                || obj.contains_key("modalities")
                || obj.contains_key("name")
                || obj.contains_key("display_name")
                || obj.contains_key("default")
                || obj.contains_key("default_for_provider")
                || transport_overlay.is_none());

        let mut md_model = if has_catalog_block || has_flat_facts {
            let mut cat_map = match obj.get("catalog").and_then(Value::as_object) {
                Some(cat) => cat.clone(),
                None => {
                    let mut copy = obj.clone();
                    if copy.get("provider").is_some_and(|v| !v.is_object()) {
                        copy.remove("provider");
                    }
                    copy.remove("transport");
                    copy.remove("selection");
                    copy.remove("corrections");
                    copy
                }
            };
            cat_map.insert("id".into(), Value::String(wire_id.to_string()));
            fill_limit_shorthands(obj, &mut cat_map);
            if !cat_map.contains_key("cost")
                && let Some(pricing) = obj
                    .get("pricing")
                    .and_then(|v| serde_json::from_value::<PricingFact>(v.clone()).ok())
            {
                let cost = ModelsDevCost {
                    input: pricing.input_per_m,
                    output: pricing.output_per_m,
                    cache_read: pricing.cache_read_per_m,
                    ..ModelsDevCost::default()
                };
                if let Ok(val) = serde_json::to_value(cost) {
                    cat_map.insert("cost".into(), val);
                }
            }
            match serde_json::from_value::<ModelsDevProviderModel>(Value::Object(cat_map)) {
                Ok(parsed) => Some(parsed),
                Err(_) => continue,
            }
        } else {
            None
        };

        let explicit_canonical = obj
            .get("model")
            .or_else(|| obj.get("canonical_model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                transport_overlay
                    .as_ref()
                    .and_then(|t| t.canonical_model.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .or_else(|| {
                md_model
                    .as_ref()
                    .and_then(ModelsDevProviderModel::canonical_id)
                    .map(str::to_string)
            });

        if let (Some(md), Some(canon_id)) = (&mut md_model, explicit_canonical.as_deref())
            && let Some(canon_row) = canonical_models.get(&canon_id.to_ascii_lowercase())
        {
            merge_canonical_into_offering(md, canon_row);
        }

        if let Some(md) = &mut md_model {
            if !md.supports_text_chat() {
                continue;
            }
            if md
                .limit
                .as_ref()
                .is_some_and(|l| l.context == Some(0) || l.output == Some(0))
            {
                continue;
            }
            if md.cost.as_ref().is_some_and(|c| c.validate().is_err()) {
                md.cost = None;
            }
        }

        if let Some(t) = &transport_overlay
            && (t.limits.context_tokens == Some(0) || t.limits.output_tokens == Some(0))
        {
            continue;
        }

        let (provider_api, provider_npm) = provider_headers
            .get(provider)
            .cloned()
            .unwrap_or((None, None));

        let endpoint_key = transport_overlay
            .as_ref()
            .and_then(|t| t.endpoint_key.as_deref())
            .or_else(|| obj.get("endpoint_key").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if provider != crate::ProviderKind::OpencodeZen.as_str() {
                    "chat".to_string()
                } else if md_model
                    .as_ref()
                    .is_some_and(ModelsDevProviderModel::is_deprecated)
                {
                    OPENCODE_ZEN_DEPRECATED_ENDPOINT_KEY.to_string()
                } else {
                    opencode_zen_endpoint_key_for_npm(
                        md_model
                            .as_ref()
                            .and_then(|m| m.provider.as_ref())
                            .and_then(|p| p.npm.as_deref())
                            .or(provider_npm.as_deref()),
                    )
                    .to_string()
                }
            });

        let default_for_provider = md_model.as_ref().is_some_and(|m| m.default_for_provider)
            || transport_overlay
                .as_ref()
                .is_some_and(|t| t.default_for_provider);

        let key = (provider.to_string(), wire_id.to_string());

        let catalog_row = if let Some(md) = md_model {
            let mut row = CatalogOffering {
                provider: provider.to_string(),
                wire_model_id: wire_id.to_string(),
                canonical_model: explicit_canonical.clone(),
                endpoint_key: endpoint_key.clone(),
                default_for_provider,
                family: md.family,
                name: md.name,
                description: md.description,
                model_type: md.model_type,
                knowledge: md.knowledge,
                release_date: md.release_date,
                last_updated: md.last_updated,
                temperature: md.temperature,
                open_weights: md.open_weights,
                interleaved: md.interleaved,
                transport: md.provider,
                provider_api,
                provider_npm,
                status: md.status,
                experimental: md.experimental,
                limit: md.limit,
                cost: md.cost,
                modalities: md.modalities,
                attachment: md.attachment,
                reasoning: md.reasoning,
                tool_call: md.tool_call,
                structured_output: md.structured_output,
                reasoning_options: md.reasoning_options.clone().unwrap_or_default(),
                reasoning_options_present: md.reasoning_options.is_some(),
                source: source.clone(),
                ..CatalogOffering::default()
            };
            let mut patches = Vec::new();
            if let Some(reason) = provider_withheld.get(provider) {
                patches.push(ModelFact {
                    provider: provider.to_string(),
                    id: wire_id.to_string(),
                    pricing_withheld: Some(reason.clone()),
                    ..ModelFact::default()
                });
            }
            if let Some(reason) = obj
                .get("pricing_withheld")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                patches.push(ModelFact {
                    provider: provider.to_string(),
                    id: wire_id.to_string(),
                    pricing_withheld: Some(reason.to_string()),
                    ..ModelFact::default()
                });
            }
            if let Some(corrs) = obj.get("corrections").and_then(Value::as_array) {
                for corr in corrs {
                    if let Some(corr_obj) = corr.as_object() {
                        let mut copy = corr_obj.clone();
                        copy.remove("position");
                        copy.remove("reason");
                        copy.insert("provider".into(), Value::String(provider.to_string()));
                        copy.insert("id".into(), Value::String(wire_id.to_string()));
                        if let Ok(fact) = serde_json::from_value::<ModelFact>(Value::Object(copy)) {
                            patches.push(fact);
                        }
                    }
                }
            }
            if !patches.is_empty() {
                let mut single = BTreeMap::from([(key.clone(), row)]);
                super::catalog_patch::apply_patches(&mut single, &patches, &source, false);
                let Some(patched) = single.remove(&key) else {
                    continue;
                };
                row = patched;
            }
            Some(row)
        } else {
            None
        };

        let route_row = match (&catalog_row, &transport_overlay) {
            (Some(cat), Some(trans)) => {
                let mut offering = cat.to_offering();
                offering.endpoint_key.clone_from(&endpoint_key);
                offering.default_for_provider = default_for_provider;
                if let Some(canon) = &explicit_canonical {
                    offering.canonical_model = Some(ModelId::from(canon.clone()));
                }
                if trans.limits.has_known_limit() && provider != "opencode-go" {
                    offering.limits = RouteLimits {
                        context_tokens: trans
                            .limits
                            .context_tokens
                            .or(offering.limits.context_tokens),
                        input_tokens: trans.limits.input_tokens.or(offering.limits.input_tokens),
                        output_tokens: trans.limits.output_tokens.or(offering.limits.output_tokens),
                        output_declared: offering.limits.output_declared,
                    };
                }
                offering.capabilities = RouteCapabilities {
                    streaming: merge_capability(
                        trans.capabilities.streaming,
                        offering.capabilities.streaming,
                    ),
                    native_tool_calls: merge_capability(
                        trans.capabilities.native_tool_calls,
                        offering.capabilities.native_tool_calls,
                    ),
                    reasoning: merge_capability(
                        trans.capabilities.reasoning,
                        offering.capabilities.reasoning,
                    ),
                    structured_output: merge_capability(
                        trans.capabilities.structured_output,
                        offering.capabilities.structured_output,
                    ),
                    attachments: merge_capability(
                        trans.capabilities.attachments,
                        offering.capabilities.attachments,
                    ),
                    image_input: merge_capability(
                        trans.capabilities.image_input,
                        offering.capabilities.image_input,
                    ),
                    parallel_tool_calls: merge_capability(
                        trans.capabilities.parallel_tool_calls,
                        offering.capabilities.parallel_tool_calls,
                    ),
                    server_side_web_search: merge_capability(
                        trans.capabilities.server_side_web_search,
                        offering.capabilities.server_side_web_search,
                    ),
                    prompt_caching: merge_capability(
                        trans.capabilities.prompt_caching,
                        offering.capabilities.prompt_caching,
                    ),
                    files_api: merge_capability(
                        trans.capabilities.files_api,
                        offering.capabilities.files_api,
                    ),
                };
                Some(offering)
            }
            (Some(cat), None) => Some(cat.to_offering()),
            (None, Some(trans)) => Some(ProviderModelOffering {
                provider: crate::route::ProviderId::from(provider.to_string()),
                canonical_model: explicit_canonical.map(ModelId::from),
                wire_model_id: crate::route::WireModelId::from(wire_id.to_string()),
                endpoint_key,
                default_for_provider,
                limits: trans.limits,
                capabilities: trans.capabilities,
                pricing: crate::route::PricingSku::UnknownOrStale,
            }),
            (None, None) => None,
        };

        if default_for_provider {
            providers_with_pulled_default.insert(provider.to_string());
        }
        if let Some(cat) = catalog_row {
            pulled_catalog_rows.insert(key.clone(), cat);
        }
        if let Some(route) = route_row {
            pulled_route_rows.insert(key, route);
        }
    }

    let seed_catalog = crate::catalog::seed_catalog_offerings();
    let mut catalog_offerings = Vec::with_capacity(seed_catalog.len() + pulled_catalog_rows.len());
    for seed in seed_catalog {
        let key = (seed.provider.clone(), seed.wire_model_id.clone());
        if let Some(pulled) = pulled_catalog_rows.remove(&key) {
            catalog_offerings.push(pulled);
        } else {
            let mut cloned = seed.clone();
            if providers_with_pulled_default.contains(&cloned.provider) {
                cloned.default_for_provider = false;
            }
            catalog_offerings.push(cloned);
        }
    }
    catalog_offerings.extend(pulled_catalog_rows.into_values());

    let seed_routes = crate::route::resolver::seed_route_offerings();
    let mut route_offerings = Vec::with_capacity(seed_routes.len() + pulled_route_rows.len());
    for seed in seed_routes {
        let key = (
            seed.provider.as_str().to_string(),
            seed.wire_model_id.as_str().to_string(),
        );
        if let Some(pulled) = pulled_route_rows.remove(&key) {
            route_offerings.push(pulled);
        } else {
            let mut cloned = seed.clone();
            if providers_with_pulled_default.contains(cloned.provider.as_str()) {
                cloned.default_for_provider = false;
            }
            route_offerings.push(cloned);
        }
    }
    route_offerings.extend(pulled_route_rows.into_values());

    CompiledRegistry {
        naming,
        provider_defaults,
        descriptor_defaults,
        intrinsic_models,
        catalog_offerings,
        route_offerings,
    }
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
        if verified.key_role != KeyRole::Founder {
            refused.push(RefusedProvider {
                id: row.id.chars().take(64).collect(),
                reason: "a pulled provider requires a founder signature".into(),
            });
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
    let compiled = compile_registry(&verified, &providers);
    AdmittedRegistry {
        revision: verified.registry.revision,
        key_id: verified.key_id,
        sha256: verified.sha256,
        not_after: verified.not_after,
        registry: verified.registry,
        providers,
        builtin_rows,
        refused,
        compiled,
    }
}

#[cfg(test)]
mod tests;
