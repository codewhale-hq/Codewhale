//! OMP-style provider descriptors: how to talk to a host.
//!
//! One committed data file owns built-in/legacy defaults and compatible hosts.
//! Model rosters are **not** compiled here. A descriptor names the wire, URL, env
//! var, and whether authenticated `GET /v1/models` is the catalog authority
//! for that host. Offerings come from the Codewhale catalog layers and live
//! provider `/models` refreshes.

use std::sync::OnceLock;

use serde::Deserialize;

const DESCRIPTORS_JSON: &str = include_str!("../assets/provider_descriptors.json");

/// Immutable built-in view compiled from the same descriptor data file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BuiltinProviderDescriptor {
    pub kind: crate::ProviderKind,
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub env_vars: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub config_key: &'static str,
    pub secret_store_slot: &'static str,
    pub family: &'static str,
    pub selectable: bool,
    pub retired: bool,
    pub wire_policy: crate::provider::WirePolicy,
    pub credential_help: crate::provider::CredentialHelp,
}

/// Exact compatibility identity for the TUI-only legacy DeepSeek China table.
#[derive(Debug, Clone, Copy)]
pub struct LegacyProviderDescriptor {
    /// Canonical historical identity.
    pub id: &'static str,
    /// Legacy display label.
    pub label: &'static str,
    /// Historical route seed.
    pub base_url: &'static str,
    /// Historical model seed.
    pub default_model: &'static str,
    /// Config table key; never collapsed into an alias during lookup.
    pub config_key: &'static str,
    /// Existing shared durable credential slot.
    pub secret_store_slot: &'static str,
}

/// Pure identity/presentation compatibility from the existing descriptor owner.
/// This is a name projection, never credential or route admission authority.
#[derive(Debug, Clone, Copy)]
pub struct ProviderCompatibility {
    pub kind: crate::ProviderKind,
    pub id: &'static str,
    pub tui_wire_tag: &'static str,
    pub config_key: &'static str,
    pub base_url_config_key: &'static str,
    pub catalog_id: &'static str,
    pub catalog_source_id: &'static str,
    pub subagent_aliases: &'static [&'static str],
    pub selector_aliases: &'static [&'static str],
    pub label: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
}

/// Released presentation rows in the descriptor owner's stable order.
#[must_use]
pub fn provider_compatibility() -> &'static [ProviderCompatibility] {
    PROVIDER_COMPATIBILITY
}

/// Exact canonical configured name; custom table names are not classified here.
#[must_use]
pub fn compatibility_for_id(id: &str) -> Option<&'static ProviderCompatibility> {
    PROVIDER_COMPATIBILITY.iter().find(|row| row.id == id)
}

/// Intrinsic built-in's canonical metadata. No active config is consulted.
#[must_use]
pub fn compatibility_for_kind(kind: crate::ProviderKind) -> &'static ProviderCompatibility {
    PROVIDER_COMPATIBILITY
        .iter()
        .find(|row| row.id == kind.as_str())
        .expect("generated compatibility covers every intrinsic kind")
}

/// Decode a released presentation tag; callers still admit the resulting name.
#[must_use]
pub fn compatibility_from_wire_tag(tag: &str) -> Option<&'static ProviderCompatibility> {
    PROVIDER_COMPATIBILITY
        .iter()
        .find(|row| row.tui_wire_tag == tag)
}

/// Explicit legacy aliases are checked before ordinary kind alias grouping.
#[must_use]
pub fn compatibility_for_selector(value: &str) -> Option<&'static ProviderCompatibility> {
    let name = value.trim();
    PROVIDER_COMPATIBILITY
        .iter()
        .find(|row| {
            row.selector_aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .or_else(|| crate::ProviderKind::parse_config_identity(name).map(compatibility_for_kind))
}

/// Released TUI wire spelling, paired with an exact non-secret route key.
/// This projection does not admit a route or infer a custom table's kind.
#[must_use]
pub fn tui_wire_tag_for_route(kind: crate::ProviderKind, id: &str) -> Option<&'static str> {
    if id.trim().is_empty() || id.trim() != id {
        return None;
    }
    if kind == crate::ProviderKind::Custom {
        return Some(compatibility_for_kind(kind).tui_wire_tag);
    }
    let row = compatibility_for_id(id)?;
    (row.kind == kind).then_some(row.tui_wire_tag)
}

/// Decode a released tag while retaining its exact identity provenance.
#[must_use]
pub fn kind_from_tui_wire_tag(tag: &str, id: &str) -> Option<crate::ProviderKind> {
    let row = compatibility_from_wire_tag(tag)?;
    (tui_wire_tag_for_route(row.kind, id) == Some(tag)).then_some(row.kind)
}

/// Compile-time compatibility constants, generated from descriptor data.
pub mod defaults {
    include!(concat!(env!("OUT_DIR"), "/provider_defaults.rs"));
}

include!(concat!(env!("OUT_DIR"), "/provider_descriptors.rs"));

/// How this host's model list is discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorDiscovery {
    /// Authenticated `GET {base_url}/models` is authoritative for this credential.
    ModelsEndpoint,
    /// No live discovery; only catalog/config rows.
    None,
}

/// Transport used to send turns. Not a brand enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DescriptorWire {
    OpenaiCompatible,
    AnthropicMessages,
}

#[derive(Debug, Deserialize)]
struct DescriptorFile {
    descriptors: Vec<ProviderDescriptor>,
}

/// Data row describing a hosted OpenAI-compatible (or Anthropic Messages) gateway.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub label: String,
    pub wire: DescriptorWire,
    pub base_url: String,
    pub api_key_env: String,
    pub default_model: String,
    pub discovery: DescriptorDiscovery,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub credential_url: Option<String>,
    #[serde(default)]
    pub guidance: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl ProviderDescriptor {
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_ascii_lowercase().replace('_', "-");
        if needle.is_empty() {
            return false;
        }
        self.id == needle
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&needle))
    }
}

static DESCRIPTORS: OnceLock<Vec<ProviderDescriptor>> = OnceLock::new();

/// Bundled compatible-host descriptors. Panics only if the committed JSON is invalid.
#[must_use]
pub fn bundled_provider_descriptors() -> &'static [ProviderDescriptor] {
    DESCRIPTORS
        .get_or_init(|| {
            let file: DescriptorFile = serde_json::from_str(DESCRIPTORS_JSON)
                .expect("committed provider_descriptors.json must parse");
            file.descriptors
        })
        .as_slice()
}

#[must_use]
pub fn provider_descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    bundled_provider_descriptors()
        .iter()
        .find(|descriptor| descriptor.matches(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_parse_and_command_code_is_a_row_not_a_kind() {
        let rows = bundled_provider_descriptors();
        assert!(
            rows.len() >= 6,
            "expected compatible hosts plus command-code and dashscope"
        );
        for row in rows {
            assert!(row.base_url.starts_with("https://"), "{}", row.id);
            assert!(!row.api_key_env.is_empty(), "{}", row.id);
            assert!(!row.default_model.is_empty(), "{}", row.id);
            assert_eq!(row.discovery, DescriptorDiscovery::ModelsEndpoint);
            assert_eq!(row.wire, DescriptorWire::OpenaiCompatible);
        }
        let cmd = provider_descriptor("command-code").expect("command-code");
        assert_eq!(cmd.base_url, "https://api.commandcode.ai/provider/v1");
        assert_eq!(cmd.api_key_env, "COMMAND_CODE_API_KEY");
        assert_eq!(
            provider_descriptor("cmd-code").map(|row| row.id.as_str()),
            Some("command-code")
        );
        // Alibaba Model Studio is a data-driven row: live /v1/models is the
        // Qwen model authority, never a compiled roster.
        let dashscope = provider_descriptor("dashscope").expect("dashscope");
        assert_eq!(
            dashscope.base_url,
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(dashscope.api_key_env, "DASHSCOPE_API_KEY");
        assert_eq!(
            provider_descriptor("qwen").map(|row| row.id.as_str()),
            Some("dashscope"),
            "the founder's `qwen` name resolves to the DashScope row"
        );
    }

    /// #6616: AICraft carries the console, docs and guidance its neighbours
    /// do, and every link a descriptor publishes is HTTPS.
    #[test]
    fn aicraft_carries_console_docs_and_guidance() {
        let aicraft = provider_descriptor("ai-craft").expect("aicraft");
        assert_eq!(aicraft.id, "aicraft");
        assert_eq!(
            aicraft.docs_url.as_deref(),
            Some("https://aicraftapi.com/docs.html#codewhale")
        );
        assert_eq!(
            aicraft.credential_url.as_deref(),
            Some("https://aicraftapi.com/dashboard.html")
        );
        let guidance = aicraft.guidance.as_deref().expect("aicraft guidance");
        assert!(guidance.contains("Store AICRAFT_API_KEY"), "{guidance}");
        for row in bundled_provider_descriptors() {
            for url in [&row.docs_url, &row.credential_url].into_iter().flatten() {
                assert!(url.starts_with("https://"), "{}: {url}", row.id);
            }
        }
    }

    /// #6695: Tsubasa is a data row on the existing compatible transport with
    /// its own key env. The row carries no context field, so the guidance is
    /// where the 32K window and the second public model id reach the user.
    #[test]
    fn tsubasa_is_a_compatible_row_with_its_own_key() {
        let tsubasa = provider_descriptor("tsubasa").expect("tsubasa");
        assert_eq!(tsubasa.wire, DescriptorWire::OpenaiCompatible);
        assert_eq!(tsubasa.base_url, "https://api.tsubasa.sh/v1");
        assert_eq!(tsubasa.api_key_env, "TSUBASA_API_KEY");
        assert_eq!(tsubasa.default_model, "tsubasa-pro");
        let guidance = tsubasa.guidance.as_deref().expect("tsubasa guidance");
        for needle in [
            "tsubasa-fast",
            "context_window = 32768",
            "Store TSUBASA_API_KEY",
        ] {
            assert!(guidance.contains(needle), "{needle}: {guidance}");
        }
    }

    #[test]
    fn cheaper_inference_is_a_descriptor_row() {
        let row = provider_descriptor("cheaper-inference").expect("cheaperinference");
        assert_eq!(row.id, "cheaperinference");
        assert_eq!(row.base_url, "https://api.cheaperinference.com/v1");
        assert_eq!(row.api_key_env, "CHEAPER_INFERENCE_API_KEY");
        assert_eq!(row.default_model, "gpt-5.4-mini");
        assert_eq!(
            provider_descriptor("cheaper_inference").map(|row| row.id.as_str()),
            Some("cheaperinference")
        );
    }

    #[test]
    fn standard_compute_is_a_descriptor_row() {
        let row = provider_descriptor("standard-compute").expect("standardcompute");
        assert_eq!(row.id, "standardcompute");
        assert_eq!(row.wire, DescriptorWire::OpenaiCompatible);
        assert_eq!(row.base_url, "https://api.stdcmpt.com/v1");
        assert_eq!(row.api_key_env, "STANDARDCOMPUTE_API_KEY");
        assert_eq!(row.default_model, "standardcompute");
        assert_eq!(
            provider_descriptor("standard_compute").map(|row| row.id.as_str()),
            Some("standardcompute")
        );
    }

    #[test]
    fn opper_is_a_descriptor_row() {
        let row = provider_descriptor("opper").expect("opper");
        assert_eq!(row.id, "opper");
        assert_eq!(row.wire, DescriptorWire::OpenaiCompatible);
        assert_eq!(row.base_url, "https://api.opper.ai/v3/compat");
        assert_eq!(row.api_key_env, "OPPER_API_KEY");
        assert_eq!(row.default_model, "claude-sonnet-4-6");
        assert_eq!(
            provider_descriptor("opper_ai").map(|row| row.id.as_str()),
            Some("opper")
        );
    }

    #[test]
    fn descriptors_do_not_embed_model_rosters() {
        let raw = DESCRIPTORS_JSON;
        assert!(
            !raw.contains("moonshotai/Kimi-K2.7-Code"),
            "do not compile a Baseten/Kimi roster into descriptors"
        );
        assert!(
            !raw.contains("openai/gpt-oss-120b"),
            "do not compile a Groq roster into descriptors"
        );
    }
    #[test]
    fn metadata_preserves_distinct_registry_and_selector_orders() {
        use crate::ProviderKind;
        let registry: Vec<_> = crate::provider::all_providers()
            .iter()
            .map(|row| row.kind())
            .collect();
        let index = |rows: &[ProviderKind], kind| rows.iter().position(|row| *row == kind).unwrap();
        assert_eq!(registry.len(), 52);
        assert_eq!(ProviderKind::ALL.len(), 46);
        assert!(
            index(&registry, ProviderKind::Modelscope) < index(&registry, ProviderKind::Together)
        );
        assert!(
            index(&ProviderKind::ALL, ProviderKind::ModelstudioTokenPlan)
                < index(&ProviderKind::ALL, ProviderKind::Modelscope)
        );
        assert_eq!(ProviderKind::ALL.last(), Some(&ProviderKind::Custom));
        assert_eq!(ProviderKind::parse("agy"), None);
        assert_eq!(
            ProviderKind::parse_config_identity("agy"),
            Some(ProviderKind::Antigravity)
        );
        assert!(
            crate::provider::providers_sorted_for_display()
                .iter()
                .all(|row| row.kind() != ProviderKind::Antigravity)
        );
    }

    #[test]
    fn grouped_secret_slots_do_not_collapse_config_or_wire_identity() {
        use crate::ProviderKind;
        use crate::provider::{CredentialAcquisition, WireFormat, WirePolicy};
        let token = ProviderKind::ModelstudioTokenPlan.provider();
        let coding = ProviderKind::ModelstudioCodingPlanAnthropic.provider();
        assert_ne!(token.provider_config_key(), coding.provider_config_key());
        assert_ne!(token.default_base_url(), coding.default_base_url());
        assert_eq!(
            ProviderKind::ModelstudioCodingPlanAnthropic.secret_store_slot(),
            "modelstudio-token-plan"
        );
        assert_eq!(
            ProviderKind::SiliconflowCN.secret_store_slot(),
            "siliconflow"
        );
        assert_eq!(
            ProviderKind::parse_config_identity(coding.id()),
            Some(ProviderKind::ModelstudioCodingPlanAnthropic)
        );
        assert_eq!(
            token.wire_policy(),
            WirePolicy::Fixed(WireFormat::ChatCompletions)
        );
        assert_eq!(
            coding.wire_policy(),
            WirePolicy::Fixed(WireFormat::AnthropicMessages)
        );
        assert_eq!(
            ProviderKind::Codewhale.provider().wire_policy(),
            WirePolicy::ModelAware
        );
        // Preserve the existing public const metadata accessor as well as
        // the ordinary trait facade used by runtime consumers.
        const CODEX_HELP: crate::provider::CredentialHelp =
            crate::provider::credential_help(ProviderKind::OpenaiCodex);
        assert_eq!(CODEX_HELP.acquisition, CredentialAcquisition::OAuth);
        assert_eq!(
            ProviderKind::Sglang
                .provider()
                .credential_help()
                .acquisition,
            CredentialAcquisition::LocalOptional
        );
        assert_eq!(LEGACY_DEEPSEEK_CN.secret_store_slot, "deepseek");
        assert_eq!(LEGACY_DEEPSEEK_CN.config_key, "deepseek_cn");
        // A named custom table must never obtain built-in authority just by
        // reusing its name; compatible-host lookup remains separately scoped.
        assert!(provider_descriptor("openai").is_none());
        assert!(provider_descriptor("deepseek").is_none());
        assert_eq!(bundled_provider_descriptors().len(), 13);
    }
}
