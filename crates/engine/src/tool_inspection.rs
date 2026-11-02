//! Truthful, bounded inspection of a prepared model-client request's tool field.
//!
//! Capture happens at the request-construction seam and retains only a bounded
//! projection. It never claims that the prepared request was delivered.
//!
//! Two kinds of fact live here, and they are kept apart on purpose:
//!
//! * **Wire facts** come from the prepared request itself — names, schemas,
//!   descriptions, per-tool transport flags, byte accounting, and the
//!   active-tool-catalog digest. The digest is not defined here; it is
//!   [`crate::core::engine::preview::active_tool_catalog_sha256`], the same
//!   function the request manifest publishes, so `/tools` and `/request` cannot
//!   report two different hashes of one catalog.
//! * **Surface facts** come from a [`ToolSurfaceContext`] the engine resolves
//!   once per turn: flattened registry facts, the MCP pool's resolved server
//!   attributions, the engine-injected catalog names, and the provider receipt
//!   taken from the *resolved model client*. When that context is present,
//!   provenance, MCP server identity, capabilities, approval requirement, and
//!   model visibility become available and true. When it is absent they stay
//!   explicitly unknown — the context is optional, never faked.
//!
//! What stays unknowable stays unknown regardless: nothing here observes the
//! provider adapter's wire payload, so it is always reported as unavailable.

mod portable_projection;
pub use portable_projection::tool_snapshot as project_snapshot;

use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use codewhale_models::Tool;

const MAX_RENDERED_TOOLS: usize = 32;
const MAX_NAME_CHARS: usize = 256;
const MAX_DESCRIPTION_CHARS: usize = 512;
const MAX_SCHEMA_BYTES: usize = 2_048;
const MAX_AUXILIARY_CHARS: usize = 512;
const MAX_ALLOWED_CALLERS: usize = 16;
const MAX_ALLOWED_CALLER_CHARS: usize = 128;
const MAX_PAYLOAD_MEASUREMENT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoundedString {
    pub value: String,
    pub truncated: bool,
}

/// Observed engine exit boundary. This never classifies the assistant's prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStopReason {
    ProviderNoToolCall,
    ProviderToolCallMissing,
    StepBudgetExhausted,
    NoProgress,
    Interrupted,
    Failed,
}

/// Terminal facts attached to the existing request inspector, without adding
/// conversation input or a notice to an ordinary successful response.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TurnStopDiagnostics {
    pub status: Option<crate::core::events::TurnOutcomeStatus>,
    /// None means the precise runtime exit boundary was not observed.
    pub reason: Option<TurnStopReason>,
    /// None means the caller did not install a model-step ceiling.
    pub effective_max_steps: Option<u32>,
    pub step_budget_source: &'static str,
    /// Existing zero-based scheduler step; transport retries do not advance it.
    pub model_step_index: u32,
    /// Parent streaming ModelClient calls, including stream retries. Excludes
    /// HTTP retries inside the client, compaction and child calls; not invoices.
    pub model_requests_started: u32,
    /// HTTP retries actually entered inside the captured parent request.
    /// This is dispatch evidence, not provider usage or a bill.
    pub transport_retries: u32,
    pub transparent_stream_retries: u32,
    pub stream_resumes: u32,
    pub reasoning_only_reprompts: u32,
    /// Re-requests after a clean terminal stop that carried no text, no
    /// reasoning and no tool call (#6310): an exact-prefix retry, then a
    /// nudged one, before the turn fails visibly.
    pub empty_stop_retries: u32,
    pub final_report_requested: bool,
    pub permission_strategy_switches: u32,
    /// Denied provider-response batches since the latest useful progress.
    pub permission_denial_rounds_without_progress: u32,
    pub last_provider_finish_reason: Option<BoundedString>,
    /// Structured calls decoded from the stream, before legacy text-call parsing.
    pub last_response_tool_calls: Option<usize>,
    /// None means suppression was not counted at this exit boundary.
    pub last_response_tool_calls_suppressed: Option<usize>,
    /// Last parent response's reported input tokens, not cumulative billing.
    pub last_reported_input_tokens: Option<u32>,
    pub route_context_window_tokens: Option<u64>,
    /// Engine-prepared output allowance. A transport may omit the field;
    /// this is budget evidence, not a provider-published capability ceiling.
    pub last_prepared_output_limit_tokens: Option<u32>,
    pub automatic_compaction_attempts: u32,
    pub emergency_compaction_attempts: u32,
}

impl TurnStopDiagnostics {
    pub fn observe_provider_response(&mut self, finish_reason: Option<&str>, tool_calls: usize) {
        self.last_provider_finish_reason =
            finish_reason.map(|reason| bounded_chars(reason, MAX_NAME_CHARS));
        self.last_response_tool_calls = Some(tool_calls);
        self.last_response_tool_calls_suppressed = None;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Evidence<T> {
    Known { value: T },
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoundedList {
    pub count: usize,
    pub rendered: Vec<BoundedString>,
    pub omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CountOnly {
    pub count: usize,
    pub values: &'static str,
}

/// One registry tool flattened to the exact facts this projection may report.
///
/// The engine fills this from `ToolSpec`, so this module never holds a tool
/// object and therefore cannot execute one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryFacts {
    pub name: String,
    pub description: String,
    pub model_visible: bool,
    pub capabilities: Vec<String>,
    pub approval: String,
    /// `true` when the tool came from the plugin surface rather than the
    /// built-in registry builder.
    pub plugin: bool,
}

/// Where a tool in the prepared request came from.
///
/// `Unknown` is a real answer, not a fallback guess: it means the surface
/// context resolved no origin for that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProvenance {
    /// Registered by the built-in registry builder.
    Builtin,
    /// Loaded from the plugin/tools surface or `config.toml` overrides.
    Plugin,
    /// Contributed by the MCP pool, as attributed by the pool itself.
    Mcp,
    /// Injected into the request catalog by the engine rather than registered
    /// (`tool_search` and its legacy spellings, `code_execution`,
    /// `js_execution`).
    Synthetic,
    /// Present in the request with no resolved origin.
    Unknown,
}

/// A tool's state relative to the request that was prepared for this step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolVisibility {
    /// In this step's request with its schema included.
    Active,
    /// In this step's request, marked deferred (schema loads on demand).
    Deferred,
    /// In this step's request, with no transport flag to say which.
    InRequest,
}

impl ToolVisibility {
    /// Whether this state means the tool's bytes are carried by the prepared
    /// request. The only honest source of this answer is the request itself.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub const fn in_request(self) -> bool {
        matches!(self, Self::Active | Self::Deferred | Self::InRequest)
    }
}

/// Provider/route availability, derived from the resolved model client taken at
/// the request seam — never from the existence of a tool registry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderAvailability {
    /// No client receipt was taken, because no surface context was captured.
    #[default]
    Unknown,
    /// A model client was resolved and this request was built for it.
    Available { provider: String, model: String },
    /// The seam was reached with no resolved model client. The reason is a safe
    /// label, never a URL or credential.
    Unavailable { reason: String },
}

impl ProviderAvailability {
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Available { .. } => "available",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// Everything outside the prepared request that this projection is allowed to
/// report, resolved once per turn by the engine.
///
/// Plain data only: no registry, no client, no credentials. Resolving it once
/// per turn is what keeps the per-step seam from re-locking the MCP pool.
#[derive(Debug, Clone, Default)]
pub struct ToolSurfaceContext {
    /// The real registry, flattened. Sorted by name by the producer.
    pub registry: Vec<RegistryFacts>,
    /// Model tool name -> MCP server name, only for names the real pool
    /// resolved. Absent names stay unknown rather than being split apart.
    pub mcp_servers: BTreeMap<String, String>,
    /// Request-catalog names the engine injects rather than registering.
    pub synthetic_names: Vec<String>,
    /// Receipt from the resolved model client for this turn.
    pub provider: ProviderAvailability,
}

impl ToolSurfaceContext {
    fn provenance(&self, name: &str) -> ToolProvenance {
        if let Some(facts) = self.registry.iter().find(|facts| facts.name == name) {
            if facts.plugin {
                return ToolProvenance::Plugin;
            }
            if self.mcp_servers.contains_key(name) {
                return ToolProvenance::Mcp;
            }
            return ToolProvenance::Builtin;
        }
        if self.mcp_servers.contains_key(name) {
            return ToolProvenance::Mcp;
        }
        if self.synthetic_names.iter().any(|entry| entry == name) {
            return ToolProvenance::Synthetic;
        }
        ToolProvenance::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolProjection {
    pub ordinal: usize,
    pub name: BoundedString,
    pub tool_type: Evidence<BoundedString>,
    pub description: BoundedString,
    pub input_schema_json: BoundedString,
    pub allowed_callers: Evidence<BoundedList>,
    pub defer_loading: Evidence<bool>,
    pub input_examples: Evidence<CountOnly>,
    pub strict: Evidence<bool>,
    pub cache_control_type: Evidence<BoundedString>,
    /// Where this tool came from. Known only when a surface context was
    /// captured; `ToolProvenance::Unknown` inside `Known` means the context was
    /// captured and still resolved no origin.
    pub provenance: Evidence<ToolProvenance>,
    /// Owning MCP server, only when the real pool attributed this exact model
    /// tool name.
    pub mcp_server: Evidence<BoundedString>,
    /// Declared capabilities from the registry, sorted. Known-and-empty means
    /// "declares none"; unknown means "not in the registry".
    pub capabilities: Evidence<BoundedList>,
    /// Declared approval requirement from the registry.
    pub approval: Evidence<BoundedString>,
    /// Registry model visibility. A tool can be registered but hidden.
    pub model_visible: Evidence<bool>,
    /// State relative to this prepared request. Always known: the request is
    /// the evidence.
    pub visibility: ToolVisibility,
}

/// Bounded evidence from a prepared model-client request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolInspectionSnapshot {
    pub schema_version: u32,
    pub capture_source: &'static str,
    pub delivery_status: &'static str,
    pub turn_id: BoundedString,
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TurnStopDiagnostics>,
    pub tools_field_present: bool,
    pub tool_count: usize,
    pub rendered_tool_count: usize,
    pub omitted_tool_count: usize,
    pub payload_json_bytes: Option<usize>,
    pub payload_measurement_status: String,
    /// The active-tool-catalog digest, computed by the *same* function the
    /// request manifest uses for `active_tool_catalog_sha256`. Absent only when
    /// the request carried no tools field at all. Covers tool name,
    /// description, and canonical input schema — not transport-only fields —
    /// exactly as the manifest does.
    pub active_tool_catalog_sha256: Option<String>,
    /// Facts nothing on this path can observe for this request. Shrinks when a
    /// surface context supplies registry- and client-derived truth; never
    /// empties, because the provider adapter's wire payload is never visible
    /// here.
    pub unavailable_for_this_request: Vec<&'static str>,
    /// Provider receipt from the resolved model client, or `Unknown` when no
    /// surface context was captured.
    pub provider: ProviderAvailability,
    /// Whether registry-derived facts were captured at all. Absent stays
    /// distinct from an empty registry.
    pub registry_facts_present: bool,
    /// Size of the flattened registry, when it was captured.
    pub registry_tool_count: Evidence<usize>,
    /// Registered, model-visible tools this request does *not* carry. Bounded,
    /// with an explicit omission count.
    pub registry_only_tools: Evidence<BoundedList>,
    pub tools: Vec<ToolProjection>,
}

impl ToolInspectionSnapshot {
    /// Wire facts only. Provenance, attribution, capabilities, approval, and
    /// provider identity stay explicitly unknown.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_prepared_request(turn_id: &str, step: u32, tools: Option<&[Tool]>) -> Self {
        Self::from_prepared_request_with_surface(turn_id, step, tools, None)
    }

    /// Wire facts joined against the turn's resolved surface context.
    ///
    /// The context is what turns "unavailable" into truth: it is derived from
    /// the real registry, the real MCP pool's own attribution, the engine's own
    /// synthetic-name list, and the resolved model client. Passing `None`
    /// reproduces the wire-only projection exactly.
    #[must_use]
    pub fn from_prepared_request_with_surface(
        turn_id: &str,
        step: u32,
        tools: Option<&[Tool]>,
        surface: Option<&ToolSurfaceContext>,
    ) -> Self {
        let tool_count = tools.map_or(0, <[Tool]>::len);
        let projected = tools
            .unwrap_or_default()
            .iter()
            .take(MAX_RENDERED_TOOLS)
            .enumerate()
            .map(|(index, tool)| project_tool(index, tool, surface))
            .collect::<Vec<_>>();
        let (payload_json_bytes, payload_measurement_status) = measure_payload(tools);

        let request_names = tools
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let registry_only_tools = surface.map_or_else(
            || unknown("registry facts not captured for this request"),
            |surface| {
                let names = surface
                    .registry
                    .iter()
                    .filter(|facts| {
                        facts.model_visible && !request_names.contains(facts.name.as_str())
                    })
                    .map(|facts| facts.name.as_str())
                    .collect::<Vec<_>>();
                let rendered = names
                    .iter()
                    .take(MAX_RENDERED_TOOLS)
                    .map(|name| bounded_chars(name, MAX_NAME_CHARS))
                    .collect::<Vec<_>>();
                Evidence::Known {
                    value: BoundedList {
                        count: names.len(),
                        omitted: names.len().saturating_sub(rendered.len()),
                        rendered,
                    },
                }
            },
        );

        let mut unavailable_for_this_request = vec!["provider_wire_payload"];
        if surface.is_none() {
            unavailable_for_this_request.extend([
                "provider",
                "model",
                "approval",
                "provenance",
                "capabilities",
            ]);
        } else if !surface.is_some_and(|surface| surface.provider.is_available()) {
            unavailable_for_this_request.extend(["provider", "model"]);
        }

        Self {
            schema_version: 1,
            capture_source: "prepared model-client request",
            delivery_status: "unknown (capture does not prove provider delivery)",
            turn_id: bounded_chars(turn_id, MAX_AUXILIARY_CHARS),
            step,
            terminal: None,
            tools_field_present: tools.is_some(),
            tool_count,
            rendered_tool_count: projected.len(),
            omitted_tool_count: tool_count.saturating_sub(projected.len()),
            payload_json_bytes,
            payload_measurement_status,
            active_tool_catalog_sha256: tools
                .map(crate::core::engine::preview::active_tool_catalog_sha256),
            unavailable_for_this_request,
            provider: surface.map_or(ProviderAvailability::Unknown, |surface| {
                surface.provider.clone()
            }),
            registry_facts_present: surface.is_some(),
            registry_tool_count: surface.map_or_else(
                || unknown("registry facts not captured for this request"),
                |surface| Evidence::Known {
                    value: surface.registry.len(),
                },
            ),
            registry_only_tools,
            tools: projected,
        }
    }

    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_text(&self) -> String {
        codewhale_commands::diagnostics_reports::render_tool_snapshot_text(&project_snapshot(self))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        codewhale_commands::diagnostics_reports::render_tool_snapshot_json(&project_snapshot(self))
    }
}

fn project_tool(index: usize, tool: &Tool, surface: Option<&ToolSurfaceContext>) -> ToolProjection {
    let facts = surface.and_then(|surface| {
        surface
            .registry
            .iter()
            .find(|facts| facts.name == tool.name)
    });
    let no_surface = "surface context not captured for this request";
    let not_registered = "tool is not in the registry";
    ToolProjection {
        ordinal: index + 1,
        name: bounded_chars(&tool.name, MAX_NAME_CHARS),
        tool_type: optional_bounded(tool.tool_type.as_deref()),
        description: bounded_chars(&tool.description, MAX_DESCRIPTION_CHARS),
        input_schema_json: bounded_json(&tool.input_schema, MAX_SCHEMA_BYTES),
        allowed_callers: tool.allowed_callers.as_ref().map_or_else(
            || unknown("request field absent"),
            |values| {
                let rendered = values
                    .iter()
                    .take(MAX_ALLOWED_CALLERS)
                    .map(|value| bounded_chars(value, MAX_ALLOWED_CALLER_CHARS))
                    .collect::<Vec<_>>();
                Evidence::Known {
                    value: BoundedList {
                        count: values.len(),
                        omitted: values.len().saturating_sub(rendered.len()),
                        rendered,
                    },
                }
            },
        ),
        defer_loading: optional_copy(tool.defer_loading.as_ref()),
        input_examples: tool.input_examples.as_ref().map_or_else(
            || unknown("request field absent"),
            |values| Evidence::Known {
                value: CountOnly {
                    count: values.len(),
                    values: "values omitted from bounded projection",
                },
            },
        ),
        strict: optional_copy(tool.strict.as_ref()),
        cache_control_type: optional_bounded(
            tool.cache_control
                .as_ref()
                .map(|value| value.cache_type.as_str()),
        ),
        provenance: surface.map_or_else(
            || unknown(no_surface),
            |surface| Evidence::Known {
                value: surface.provenance(&tool.name),
            },
        ),
        mcp_server: surface.map_or_else(
            || unknown(no_surface),
            |surface| {
                surface.mcp_servers.get(&tool.name).map_or_else(
                    || unknown("the MCP pool did not attribute this tool name"),
                    |server| Evidence::Known {
                        value: bounded_chars(server, MAX_AUXILIARY_CHARS),
                    },
                )
            },
        ),
        capabilities: match (surface, facts) {
            (None, _) => unknown(no_surface),
            (Some(_), None) => unknown(not_registered),
            (Some(_), Some(facts)) => {
                let rendered = facts
                    .capabilities
                    .iter()
                    .take(MAX_ALLOWED_CALLERS)
                    .map(|value| bounded_chars(value, MAX_ALLOWED_CALLER_CHARS))
                    .collect::<Vec<_>>();
                Evidence::Known {
                    value: BoundedList {
                        count: facts.capabilities.len(),
                        omitted: facts.capabilities.len().saturating_sub(rendered.len()),
                        rendered,
                    },
                }
            }
        },
        approval: match (surface, facts) {
            (None, _) => unknown(no_surface),
            (Some(_), None) => unknown(not_registered),
            (Some(_), Some(facts)) => Evidence::Known {
                value: bounded_chars(&facts.approval, MAX_AUXILIARY_CHARS),
            },
        },
        model_visible: match (surface, facts) {
            (None, _) => unknown(no_surface),
            (Some(_), None) => unknown(not_registered),
            (Some(_), Some(facts)) => Evidence::Known {
                value: facts.model_visible,
            },
        },
        // Every projected tool is carried by this prepared request; the
        // transport flag only says whether its schema rides along now.
        visibility: match tool.defer_loading {
            Some(true) => ToolVisibility::Deferred,
            Some(false) => ToolVisibility::Active,
            None => ToolVisibility::InRequest,
        },
    }
}

/// Byte accounting only. The digest is deliberately *not* computed here: it is
/// the request path's [`crate::core::engine::preview::active_tool_catalog_sha256`],
/// so this projection never defines a second catalog hash.
fn measure_payload(tools: Option<&[Tool]>) -> (Option<usize>, String) {
    let Some(tools) = tools else {
        return (None, "unavailable (tools field absent)".to_string());
    };
    let mut writer = BoundedWriter::new(MAX_PAYLOAD_MEASUREMENT_BYTES);
    match serde_json::to_writer(&mut writer, tools) {
        Ok(()) => (
            Some(writer.bytes.len()),
            "exact (within 1048576-byte measurement bound)".to_string(),
        ),
        Err(_) if writer.exceeded => (
            None,
            "unavailable (payload exceeds 1048576-byte measurement bound)".to_string(),
        ),
        Err(_) => (None, "unavailable (serialization failed)".to_string()),
    }
}

fn bounded_json(value: &Value, limit: usize) -> BoundedString {
    let mut writer = BoundedWriter::new(limit);
    let result = serde_json::to_writer(&mut writer, value);
    BoundedString {
        value: String::from_utf8_lossy(&writer.bytes).into_owned(),
        truncated: result.is_err() && writer.exceeded,
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8_192)),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let accepted = buffer.len().min(remaining);
        self.bytes.extend_from_slice(&buffer[..accepted]);
        if accepted < buffer.len() {
            self.exceeded = true;
            return Err(io::Error::other("inspection bound exceeded"));
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn bounded_chars(value: &str, limit: usize) -> BoundedString {
    let mut chars = value.chars();
    let value = chars.by_ref().take(limit).collect::<String>();
    BoundedString {
        value,
        truncated: chars.next().is_some(),
    }
}

fn optional_bounded(value: Option<&str>) -> Evidence<BoundedString> {
    value.map_or_else(
        || unknown("request field absent"),
        |value| Evidence::Known {
            value: bounded_chars(value, MAX_AUXILIARY_CHARS),
        },
    )
}

fn optional_copy<T: Copy>(value: Option<&T>) -> Evidence<T> {
    value.map_or_else(
        || unknown("request field absent"),
        |value| Evidence::Known { value: *value },
    )
}

fn unknown<T>(reason: &str) -> Evidence<T> {
    Evidence::Unknown {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> Tool {
        Tool {
            tool_type: Some("function".to_string()),
            name: name.to_string(),
            description: "Read a file".to_string(),
            input_schema: json!({"type": "object"}),
            allowed_callers: None,
            defer_loading: Some(false),
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        }
    }

    #[test]
    fn absent_field_stays_distinct_from_present_empty_array() {
        let absent = ToolInspectionSnapshot::from_prepared_request("turn", 1, None);
        let empty = ToolInspectionSnapshot::from_prepared_request("turn", 1, Some(&[]));
        assert!(!absent.tools_field_present);
        assert_eq!(absent.payload_json_bytes, None);
        assert!(absent.active_tool_catalog_sha256.is_none());
        assert!(empty.tools_field_present);
        assert_eq!(empty.payload_json_bytes, Some(2));
        assert!(empty.active_tool_catalog_sha256.is_some());
    }

    #[test]
    fn catalog_digest_is_the_request_manifest_digest_not_a_second_definition() {
        let tools = vec![tool("read_file"), tool("write_file")];
        let snapshot = ToolInspectionSnapshot::from_prepared_request("turn", 1, Some(&tools));

        // Same prepared request, same accounting object: the value the request
        // manifest publishes as `active_tool_catalog_sha256`.
        assert_eq!(
            snapshot.active_tool_catalog_sha256.as_deref(),
            Some(crate::core::engine::preview::active_tool_catalog_sha256(&tools).as_str()),
        );

        // And it is a catalog digest, not an incidental byte hash: reordering
        // the same tools changes it.
        let reordered = vec![tools[1].clone(), tools[0].clone()];
        let reordered = ToolInspectionSnapshot::from_prepared_request("turn", 1, Some(&reordered));
        assert_ne!(
            snapshot.active_tool_catalog_sha256,
            reordered.active_tool_catalog_sha256
        );
    }

    #[test]
    fn projection_preserves_known_false_and_marks_unknown() {
        let snapshot =
            ToolInspectionSnapshot::from_prepared_request("turn", 3, Some(&[tool("read_file")]));
        let text = snapshot.render_text();
        assert!(text.contains("deferred loading: false"), "{text}");
        assert!(text.contains("strict: true"), "{text}");
        assert!(text.contains("allowed callers: unknown (request field absent)"));
        assert!(text.contains("Delivery: unknown"));
        assert!(text.contains("Provider-wire tool payload: unavailable"));
    }

    fn facts(name: &str, plugin: bool, model_visible: bool) -> RegistryFacts {
        RegistryFacts {
            name: name.to_string(),
            description: format!("{name} registry description"),
            model_visible,
            capabilities: vec!["ReadOnly".to_string()],
            approval: "Auto".to_string(),
            plugin,
        }
    }

    fn surface() -> ToolSurfaceContext {
        ToolSurfaceContext {
            registry: vec![
                facts("read_file", false, true),
                facts("plugin_tool", true, true),
                facts("hidden_alias", false, false),
                facts("not_sent", false, true),
            ],
            mcp_servers: BTreeMap::from([(
                "mcp_my_server_read_file".to_string(),
                "my_server".to_string(),
            )]),
            synthetic_names: vec!["tool_search".to_string()],
            provider: ProviderAvailability::Available {
                provider: "Deepseek".to_string(),
                model: "deepseek-chat".to_string(),
            },
        }
    }

    #[test]
    fn surface_context_turns_provenance_and_attribution_into_truth() {
        let tools = vec![
            tool("read_file"),
            tool("plugin_tool"),
            tool("mcp_my_server_read_file"),
            tool("tool_search"),
            tool("stranger"),
        ];
        let surface = surface();
        let snapshot = ToolInspectionSnapshot::from_prepared_request_with_surface(
            "turn",
            1,
            Some(&tools),
            Some(&surface),
        );

        let provenance = |name: &str| {
            snapshot
                .tools
                .iter()
                .find(|entry| entry.name.value == name)
                .map(|entry| entry.provenance.clone())
                .expect("projected tool")
        };
        for (name, expected) in [
            ("read_file", ToolProvenance::Builtin),
            ("plugin_tool", ToolProvenance::Plugin),
            ("mcp_my_server_read_file", ToolProvenance::Mcp),
            ("tool_search", ToolProvenance::Synthetic),
            // Captured context, no resolved origin: unknown is the answer.
            ("stranger", ToolProvenance::Unknown),
        ] {
            assert_eq!(
                provenance(name),
                Evidence::Known { value: expected },
                "provenance for {name}"
            );
        }

        let mcp = snapshot
            .tools
            .iter()
            .find(|entry| entry.name.value == "mcp_my_server_read_file")
            .expect("mcp tool");
        // Attribution comes from the pool, not from splitting on `_`.
        assert_eq!(
            mcp.mcp_server,
            Evidence::Known {
                value: BoundedString {
                    value: "my_server".to_string(),
                    truncated: false,
                }
            }
        );

        let read_file = snapshot
            .tools
            .iter()
            .find(|entry| entry.name.value == "read_file")
            .expect("read_file");
        assert!(matches!(read_file.capabilities, Evidence::Known { .. }));
        assert_eq!(
            read_file.approval,
            Evidence::Known {
                value: BoundedString {
                    value: "Auto".to_string(),
                    truncated: false,
                }
            }
        );
        assert_eq!(read_file.model_visible, Evidence::Known { value: true });

        // Unregistered tools stay unknown rather than being reported as "none".
        let stranger = snapshot
            .tools
            .iter()
            .find(|entry| entry.name.value == "stranger")
            .expect("stranger");
        assert!(matches!(stranger.capabilities, Evidence::Unknown { .. }));
        assert!(matches!(stranger.approval, Evidence::Unknown { .. }));

        // The unavailable set shrinks to what nothing here can observe.
        assert_eq!(
            snapshot.unavailable_for_this_request,
            vec!["provider_wire_payload"]
        );
        assert!(snapshot.provider.is_available());
        assert!(snapshot.registry_facts_present);
        assert_eq!(snapshot.registry_tool_count, Evidence::Known { value: 4 });

        let text = snapshot.render_text();
        assert!(text.contains("provenance: synthetic"), "{text}");
        assert!(text.contains("MCP server: \"my_server\""), "{text}");
        assert!(text.contains("Provider: \"Deepseek\""), "{text}");
        assert!(
            text.contains("Provider-wire tool payload: unavailable"),
            "{text}"
        );
    }

    #[test]
    fn registry_only_tools_are_counted_without_expanding_the_projection() {
        let tools = vec![tool("read_file")];
        let surface = surface();
        let snapshot = ToolInspectionSnapshot::from_prepared_request_with_surface(
            "turn",
            1,
            Some(&tools),
            Some(&surface),
        );

        // Only the request's tools are projected; the rest are counted.
        assert_eq!(snapshot.tools.len(), 1);
        let Evidence::Known { value } = &snapshot.registry_only_tools else {
            panic!("registry-only tools must be known when facts were captured");
        };
        // `hidden_alias` is not model-visible, so it is not a missing tool.
        assert_eq!(value.count, 2);
        let rendered = value
            .rendered
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>();
        // Order follows the registry facts as supplied; the producer sorts.
        assert_eq!(rendered, vec!["plugin_tool", "not_sent"]);

        // Everything projected is in the request; the request is the evidence.
        assert!(snapshot.tools.iter().all(|entry| {
            entry.visibility.in_request() && entry.visibility == ToolVisibility::Active
        }));
    }

    #[test]
    fn absent_surface_keeps_every_registry_derived_field_unknown() {
        let tools = vec![tool("read_file")];
        let snapshot = ToolInspectionSnapshot::from_prepared_request("turn", 1, Some(&tools));

        assert_eq!(snapshot.provider, ProviderAvailability::Unknown);
        assert!(!snapshot.registry_facts_present);
        assert!(matches!(
            snapshot.registry_tool_count,
            Evidence::Unknown { .. }
        ));
        assert!(matches!(
            snapshot.registry_only_tools,
            Evidence::Unknown { .. }
        ));
        assert_eq!(
            snapshot.unavailable_for_this_request,
            vec![
                "provider_wire_payload",
                "provider",
                "model",
                "approval",
                "provenance",
                "capabilities",
            ]
        );
        let entry = &snapshot.tools[0];
        for evidence in [
            matches!(entry.provenance, Evidence::Unknown { .. }),
            matches!(entry.mcp_server, Evidence::Unknown { .. }),
            matches!(entry.capabilities, Evidence::Unknown { .. }),
            matches!(entry.approval, Evidence::Unknown { .. }),
            matches!(entry.model_visible, Evidence::Unknown { .. }),
        ] {
            assert!(evidence);
        }
        // Wire facts are still exact without a surface context.
        assert!(entry.visibility.in_request());
        assert!(snapshot.active_tool_catalog_sha256.is_some());
    }

    #[test]
    fn provider_receipt_records_an_unresolved_client_without_borrowing_registry_truth() {
        let tools = vec![tool("read_file")];
        let surface = ToolSurfaceContext {
            registry: vec![facts("read_file", false, true)],
            provider: ProviderAvailability::Unavailable {
                reason: "no model client resolved for this turn".to_string(),
            },
            ..ToolSurfaceContext::default()
        };
        let snapshot = ToolInspectionSnapshot::from_prepared_request_with_surface(
            "turn",
            1,
            Some(&tools),
            Some(&surface),
        );

        // A full registry does not make a provider available.
        assert!(!snapshot.provider.is_available());
        assert_eq!(snapshot.provider.label(), "unavailable");
        assert!(snapshot.unavailable_for_this_request.contains(&"provider"));
        // Registry-derived truth is unaffected by the missing client.
        assert!(
            !snapshot
                .unavailable_for_this_request
                .contains(&"provenance")
        );
        assert_eq!(
            snapshot.tools[0].provenance,
            Evidence::Known {
                value: ToolProvenance::Builtin
            }
        );
    }

    #[test]
    fn capture_and_rendering_are_bounded_with_explicit_receipts() {
        let mut tools = (0..40)
            .map(|index| {
                let mut value = tool(&format!("tool_{index}"));
                value.description = "x".repeat(MAX_DESCRIPTION_CHARS + 10);
                value.input_schema = json!({"large": "y".repeat(MAX_SCHEMA_BYTES * 600)});
                value
            })
            .collect::<Vec<_>>();
        tools[0].allowed_callers = Some(
            (0..20)
                .map(|caller| format!("caller-{caller}-{}", "z".repeat(200)))
                .collect(),
        );
        let snapshot = ToolInspectionSnapshot::from_prepared_request(
            &"t".repeat(MAX_AUXILIARY_CHARS + 1),
            1,
            Some(&tools),
        );
        assert_eq!(snapshot.rendered_tool_count, MAX_RENDERED_TOOLS);
        assert_eq!(snapshot.omitted_tool_count, 8);
        assert!(snapshot.turn_id.truncated);
        assert!(snapshot.tools[0].description.truncated);
        assert!(snapshot.tools[0].input_schema_json.truncated);
        assert_eq!(snapshot.payload_json_bytes, None);
        assert!(snapshot.payload_measurement_status.contains("exceeds"));
        // The catalog digest is fixed-width, so it survives the byte bound.
        assert!(snapshot.active_tool_catalog_sha256.is_some());
        let json = snapshot.render_json().expect("bounded JSON");
        // 160 KiB: the per-tool evidence fields (provenance, MCP server,
        // capabilities, approval, visibility) each carry an explicit reason
        // string when unresolved, which is the point — the cap moved, the
        // bound did not disappear.
        assert!(
            json.len() < 163_840,
            "projection grew to {} bytes",
            json.len()
        );
    }
}
