use crate::catalog;
use crate::extension_toml::{
    ControlSocketToml, HookSinksToml, LifecycleOutboxToml, SkillsToml, SnapshotsToml, ToolsToml,
};
use crate::fleet::FleetConfigToml;
use crate::hotbar::HotbarBindingToml;
use crate::provider_kind::ProviderKind;
use crate::provider_toml::ProvidersToml;
use crate::provider_toml::{deserialize_root_provider, http_headers_are_effectively_empty};
use crate::service_toml::{
    LspConfigToml, NetworkPolicyToml, VerifierConfigToml, WorkflowConfigToml,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigToml {
    // There is no top-level `api_key` or `base_url`: every parse moves the
    // legacy root keys into `[providers.<name>]` first (`legacy_root`, #6394).
    /// Optional extra HTTP headers forwarded to model API requests.
    #[serde(default, skip_serializing_if = "http_headers_are_effectively_empty")]
    pub http_headers: BTreeMap<String, String>,
    /// TUI-compatible default DeepSeek model.
    pub default_text_model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_root_provider")]
    pub provider: ProviderKind,
    /// Exact saved selector for a named custom provider or a built-in alias.
    ///
    /// This is runtime parse state rather than a second on-disk key. The
    /// serialized `provider` value is restored by `ConfigStore` so a typed
    /// dispatcher read/write cannot collapse a named route to `custom` or a
    /// regional selector to its catalog parent.
    #[doc(hidden)]
    #[serde(skip)]
    pub selected_provider_id: Option<String>,
    pub model: Option<String>,
    pub auth_mode: Option<String>,
    pub verbosity: Option<String>,
    pub log_level: Option<String>,
    pub telemetry: Option<bool>,
    /// Where telemetry batches are sent, when telemetry is enabled at all.
    ///
    /// Unset here means "take the shipped default",
    /// `DEFAULT_TELEMETRY_ENDPOINT` — not "send nowhere". Setting it to the
    /// empty string is the way to say send nowhere: that resolves to no
    /// endpoint, which appends batches to `dryrun.jsonl` and constructs no HTTP
    /// client. Either way a persistent or run-scoped opt-out still prevents any
    /// batch from being constructed.
    ///
    /// Kept as a scalar sibling of `telemetry` rather than folded into a
    /// `[telemetry]` table. `telemetry` is already a scalar and every section
    /// table is declared after it, so a table of that name would be a hard
    /// `toml::from_str` failure — and one whose cause `ConfigStore::load`
    /// deliberately hides, leaving the user with an unloadable config and no
    /// explanation. It would also be a `ValueAfterTable` serialization hazard
    /// against the scalars that follow.
    pub telemetry_endpoint: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    /// Native tool catalog controls shared with `codewhale-tui`.
    #[serde(default)]
    pub tools: Option<ToolsToml>,
    #[serde(default, skip_serializing_if = "ProvidersToml::is_empty")]
    pub providers: ProvidersToml,
    /// Operator declarations for exact provider/endpoint/model tuples.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "catalog::configured::deserialize_configured_models"
    )]
    pub custom_models: Option<Vec<catalog::configured::ConfiguredModel>>,
    /// Provider fallback chain (#2574). TUI runtime code may advance through
    /// these providers after recoverable provider errors; config resolution
    /// itself still reports the selected primary provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_providers: Vec<ProviderKind>,
    /// Per-domain network policy (#135). When absent, network tools fall back
    /// to a permissive default that mirrors pre-v0.7.0 behavior.
    #[serde(default)]
    pub network: Option<NetworkPolicyToml>,
    /// Verifier-preview behavior (#2093). When absent, verifier tools keep the
    /// shipped defaults: disabled automatic preview and hunt verdict mapping.
    #[serde(default)]
    pub verifier: Option<VerifierConfigToml>,
    /// Community skill installer settings (#140). Mirrors
    /// [`SkillsToml`] from the TUI side; the dispatcher consults
    /// `registry_url` when running `deepseek skill install`.
    #[serde(default)]
    pub skills: Option<SkillsToml>,
    /// Workspace side-git snapshots (#137). The live TUI defaults this to
    /// enabled with 7-day retention when absent.
    #[serde(default)]
    pub snapshots: Option<SnapshotsToml>,
    /// Post-edit LSP diagnostics injection (#136). When absent, the engine
    /// applies the defaults documented in [`LspConfigToml`].
    #[serde(default)]
    pub lsp: Option<LspConfigToml>,
    /// Optional 1-8 hotbar slot bindings (#2064). When absent, the TUI falls
    /// back to the built-in default slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotbar: Option<Vec<HotbarBindingToml>>,
    /// App-server hook sink configuration. Kept separate from the TUI
    /// lifecycle `[hooks]` table so config rewrites preserve existing hooks.
    #[serde(default)]
    pub hook_sinks: Option<HookSinksToml>,
    /// Lifecycle event outbox (`[lifecycle_outbox]`). Opt-in: an unset or
    /// empty `path` disables the feature and leaves behavior unchanged.
    #[serde(default)]
    pub lifecycle_outbox: Option<LifecycleOutboxToml>,
    /// Per-session control socket (`[control_socket]`). Opt-in: an absent
    /// table or `enabled = false` (the default) leaves the feature off and
    /// behavior unchanged.
    #[serde(default)]
    pub control_socket: Option<ControlSocketToml>,
    /// Agent Fleet trust and security policy (#3165). When absent, fleet
    /// workers inherit conservative Sandbox defaults.
    #[serde(default)]
    pub fleet: Option<FleetConfigToml>,
    /// Workflow automatic-launch, approval, isolation, and activity
    /// persistence knobs (#4128 / Section 2.11). When absent, consumers use
    /// [`WorkflowConfigToml::default`].
    #[serde(default)]
    pub workflow: Option<WorkflowConfigToml>,
    /// Model-bound credential redaction policy (`[redaction]`). When absent,
    /// masking is enabled — the shipped security default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<crate::redaction::RedactionToml>,
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

impl ConfigToml {
    /// The requested model-bound masking mode, defaulting to enabled.
    ///
    /// The request only takes effect once the interactive TUI has recorded a
    /// confirmation on its startup gate; see
    /// [`crate::redaction::effective_masking`].
    #[must_use]
    pub fn redaction_model_bound_masking(&self) -> crate::redaction::ModelBoundMasking {
        self.redaction
            .as_ref()
            .map(crate::redaction::RedactionToml::model_bound_masking)
            .unwrap_or_default()
    }
}
