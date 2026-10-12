use std::cell::RefCell;
use std::path::{Path, PathBuf};

use codewhale_protocol::request::{ContentBlock, Message, SystemPrompt};
use codewhale_protocol::role::Role;

use crate::*;

struct Session;
impl CommandSessionContext for Session {
    fn session_id(&self) -> Option<String> {
        Some("session".into())
    }
    fn api_messages(&self) -> Vec<Message> {
        vec![]
    }
    fn add_message(&mut self, _message: Message) {}
    fn queued_message_count(&self) -> usize {
        0
    }
    fn remove_queued_message(&mut self, _index: usize) -> Result<(), String> {
        Ok(())
    }
    fn total_tokens(&self) -> u64 {
        42
    }
}

struct Model;
impl CommandModelContext for Model {
    fn current_model(&self) -> String {
        "auto".into()
    }
    fn auto_model(&self) -> bool {
        true
    }
    fn set_model_selection(&mut self, _model: String, _provider: Option<CommandProviderId>) {}
    fn provider_identity(&self) -> Option<CommandProviderId> {
        None
    }
    fn fallback_chain(&self) -> Vec<CommandProviderId> {
        vec![]
    }
}

struct Cost;
impl CommandCostContext for Cost {
    fn display_currency(&self) -> CommandCurrency {
        CommandCurrency::Usd
    }
    fn session_cost_for_currency(&self, _currency: CommandCurrency) -> f64 {
        1.0
    }
    fn subagent_cost_for_currency(&self, _currency: CommandCurrency) -> f64 {
        0.5
    }
    fn accrue_cost_estimate(&mut self, _amount: f64, _currency: CommandCurrency) {}
    fn record_turn_cost(
        &mut self,
        _amount: f64,
        _currency: CommandCurrency,
        _receipt: Option<String>,
    ) {
    }
}

struct Policy;
impl CommandModePolicyContext for Policy {
    fn mode(&self) -> CommandMode {
        CommandMode::Plan
    }
    fn set_mode(&mut self, _mode: CommandMode) {}
    fn approval_mode(&self) -> CommandApprovalMode {
        CommandApprovalMode::Suggest
    }
    fn allow_shell(&self) -> bool {
        false
    }
    fn set_shell_access(&mut self, _allow: bool) {}
    fn policy_locked(&self) -> bool {
        false
    }
}

struct Prompt;
impl CommandSystemPromptContext for Prompt {
    fn system_prompt(&self) -> Option<SystemPrompt> {
        None
    }
}

struct Skills;
impl CommandSkillsContext for Skills {
    fn active_skill(&self) -> Option<String> {
        None
    }
    fn active_skill_provenance(&self) -> Option<String> {
        None
    }
    fn refresh_skill_cache(&mut self) {}
}

struct Workspace;
impl CommandWorkspaceContext for Workspace {
    fn workspace(&self) -> PathBuf {
        PathBuf::from(".")
    }
    fn work_state_snapshot(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn operation_digest(&mut self) -> Result<String, String> {
        Ok("No active operations or to-do items.".to_string())
    }
}

#[test]
fn all_seven_shapes_are_object_safe() {
    fn session(_: &dyn CommandSessionContext) {}
    fn model(_: &dyn CommandModelContext) {}
    fn cost(_: &dyn CommandCostContext) {}
    fn policy(_: &dyn CommandModePolicyContext) {}
    fn prompt(_: &dyn CommandSystemPromptContext) {}
    fn skills(_: &dyn CommandSkillsContext) {}
    fn workspace(_: &dyn CommandWorkspaceContext) {}

    session(&Session);
    model(&Model);
    cost(&Cost);
    policy(&Policy);
    prompt(&Prompt);
    skills(&Skills);
    workspace(&Workspace);
}

#[test]
fn envelope_carries_independent_facets() {
    let mut session = Session;
    let mut model = Model;
    let parts = CommandContexts::empty()
        .with_session(&mut session)
        .with_model(&mut model)
        .into_parts();
    assert_eq!(parts.session.expect("session").total_tokens(), 42);
    assert!(parts.model.expect("model").auto_model());
    assert!(parts.cost.is_none());
}

struct DebugDiagnostics;
impl CommandDebugDiagnosticsContext for DebugDiagnostics {
    fn balance_projection(&self) -> DebugBalanceProjection {
        DebugBalanceProjection {
            provider_display_name: "Example".into(),
            supports_balance_api: false,
        }
    }
    fn system_projection(&self) -> DebugSystemProjection {
        DebugSystemProjection {
            mode_label: "plan".into(),
            prompt: DebugSystemPrompt::Blocks(vec!["first".into(), "second".into()]),
        }
    }
    fn token_projection(&self) -> DebugTokenProjection {
        DebugTokenProjection {
            active_context_used: 0,
            context_window: 8192,
            last_input: None,
            last_output: Some(0),
            cache_hit: None,
            cache_miss: Some(0),
            total_tokens: 0,
            cache_write_tokens: 0,
            api_message_count: 0,
            chat_message_count: 0,
            model: "example".into(),
            cost: self.cost_projection(),
        }
    }
    fn cost_projection(&self) -> DebugCostProjection {
        DebugCostProjection {
            currency: CommandCurrency::Usd,
            total: 0.0,
            parent_turns: 0.0,
            subagents: 0.0,
            display_floor: 0.0,
            priced_turns: 0,
            unpriced_turns: 0,
            legacy_coverage_unknown: false,
            user_declared_estimates: false,
            itemized_turns: 0,
            route_amounts: vec![],
            turn_history_capacity: 10,
            unpriced_reason_labels: vec![],
            unpriced_classes: vec![],
            pricing_provenances: vec![],
            live_pricing_defects: vec![],
            unusable_pricing_defects: vec![],
            route_receipts: vec![],
        }
    }
    fn cache_telemetry(&self) -> DebugCacheTelemetry {
        DebugCacheTelemetry {
            model: "example".into(),
            session_cache_rates: DebugCacheRates::default(),
            history: vec![],
            history_capacity: 50,
            prefix_stability_pct: None,
            prefix_checks_total: 0,
            prefix_change_count: 0,
            prefix_drift_count: 0,
            prefix_context_updates: 0,
            prefix_pin_reason: None,
            prefix_last_miss_reason: None,
            last_prefix_change_desc: None,
            last_pinned_prefix_hash: None,
            api_message_count: 0,
            non_system_message_count: 0,
        }
    }
    fn context_source_map(&self) -> DebugPromptSourceMap {
        DebugPromptSourceMap {
            entries: vec![],
            total_estimated_tokens: 0,
            active_context_estimated_tokens: 0,
            overflow_guard_estimated_tokens: None,
            context_window_tokens: None,
            context_window_source: None,
            budget_used_percent: None,
            pressure_label: "unknown".into(),
            context_window_verified: false,
            generated_at: "2026-01-01T00:00:00Z".into(),
            note: String::new(),
        }
    }
    fn prompt_context(&self) -> DebugPromptContext {
        DebugPromptContext {
            schema_version: 1,
            provider: "example".into(),
            model: "model".into(),
            system_prompt_state: "unknown".into(),
            tool_catalog_state: "absent".into(),
            sections: vec![],
            tools: vec![],
            source_map: self.context_source_map(),
        }
    }
    fn tool_snapshot(&self) -> Option<DebugToolSnapshot> {
        None
    }
    fn inspect_cache(
        &self,
    ) -> Result<DebugCacheInspectionObservation, DebugCacheInspectionUnavailable> {
        Err(DebugCacheInspectionUnavailable::NoConcreteRoute)
    }
    fn remember_cache_inspection(&mut self, _inspection: DebugPromptInspection) {}
}

#[test]
fn debug_inspection_schema_preserves_order_and_absence() {
    let inspection = DebugPromptInspection {
        base_static_prefix_hash: "base".into(),
        full_request_prefix_hash: "full".into(),
        tool_catalog_hash: "".into(),
        layers: vec![DebugPromptLayer {
            name: "history".into(),
            stability: DebugPromptLayerStability::History,
            char_len: 0,
            byte_len: 0,
            token_estimate: 0,
            sha256: "digest".into(),
            tool_result: None,
            turn_meta: None,
        }],
    };
    assert_eq!(
        serde_json::to_string(&inspection).expect("structured inspection"),
        r#"{"base_static_prefix_hash":"base","full_request_prefix_hash":"full","tool_catalog_hash":"","layers":[{"name":"history","stability":"History","char_len":0,"byte_len":0,"token_estimate":0,"sha256":"digest","tool_result":null,"turn_meta":null}]}"#,
    );
    assert_eq!(DebugPromptLayerStability::Static.label(), "static");
    assert_eq!(DebugPromptLayerStability::Dynamic.label(), "dynamic");
    assert_ne!(
        DebugCacheInspectionUnavailable::NoConcreteRoute,
        DebugCacheInspectionUnavailable::MissingCapturedEndpoint,
    );
    let key = DebugWarmupKey {
        provider: "provider".into(),
        model: "model".into(),
        base_url: "local".into(),
        static_prefix_hash: "static".into(),
        tool_catalog_hash: "".into(),
        project_pack_hash: "".into(),
        skills_hash: "".into(),
    };
    assert_eq!(
        serde_json::to_string(&key).expect("structured key"),
        r#"{"provider":"provider","model":"model","base_url":"local","static_prefix_hash":"static","tool_catalog_hash":"","project_pack_hash":"","skills_hash":""}"#,
    );
    assert_eq!(
        DebugDiagnostics.inspect_cache(),
        Err(DebugCacheInspectionUnavailable::NoConcreteRoute)
    );
}

#[test]
fn debug_context_schema_preserves_explicit_nulls_and_omits_optional_tool_fields() {
    let mut context = DebugDiagnostics.prompt_context();
    context.tools.push(DebugPromptTool {
        tool_type: None,
        name: "search".into(),
        description: "Search".into(),
        input_schema: serde_json::json!({"type": "object"}),
        allowed_callers: None,
        defer_loading: Some(false),
        input_examples: None,
        strict: None,
        cache_control: None,
    });
    let value = serde_json::to_value(&context).expect("semantic prompt context");
    assert_eq!(
        value["source_map"]["context_window_tokens"],
        serde_json::Value::Null
    );
    assert_eq!(value["system_prompt_state"], "unknown");
    assert_eq!(value["tool_catalog_state"], "absent");
    assert_eq!(value["tools"][0]["defer_loading"], false);
    assert!(value["tools"][0].get("type").is_none());
    assert!(value["tools"][0].get("input_examples").is_none());
    assert_eq!(
        serde_json::to_value(DebugSourceKind::ProjectContextWarning).unwrap(),
        "project_context_warning"
    );
    assert_eq!(
        serde_json::to_value(DebugActivationReason::PerRequest).unwrap(),
        "per_request"
    );
}

#[test]
fn debug_tool_schema_keeps_unknown_distinct_from_known_empty() {
    let unknown: DebugEvidence<DebugBoundedList> = DebugEvidence::Unknown {
        reason: "no surface".into(),
    };
    let empty = DebugEvidence::Known {
        value: DebugBoundedList {
            count: 0,
            rendered: vec![],
            omitted: 0,
        },
    };
    assert_eq!(
        serde_json::to_value(unknown).unwrap(),
        serde_json::json!({"status":"unknown", "reason":"no surface"}),
    );
    assert_eq!(
        serde_json::to_value(empty).unwrap(),
        serde_json::json!({"status":"known", "value":{"count":0,"rendered":[],"omitted":0}}),
    );
    assert_eq!(
        serde_json::to_value(DebugProviderAvailability::Unknown).unwrap(),
        serde_json::json!({"status":"unknown"}),
    );
    assert_eq!(
        serde_json::to_value(DebugToolVisibility::InRequest).unwrap(),
        "in_request"
    );
}

#[test]
fn debug_tool_snapshot_schema_preserves_unobserved_and_absent_states() {
    let snapshot = DebugToolSnapshot {
        schema_version: 1,
        capture_source: "prepared model-client request".into(),
        delivery_status: "unknown (capture does not prove provider delivery)".into(),
        turn_id: DebugBoundedString {
            value: "turn".into(),
            truncated: false,
        },
        step: 0,
        terminal: None,
        tools_field_present: false,
        tool_count: 0,
        rendered_tool_count: 0,
        omitted_tool_count: 0,
        payload_json_bytes: None,
        payload_measurement_status: "unavailable".into(),
        active_tool_catalog_sha256: None,
        unavailable_for_this_request: vec!["provider_wire_payload".into()],
        provider: DebugProviderAvailability::Unknown,
        registry_facts_present: false,
        registry_tool_count: DebugEvidence::Unknown {
            reason: "not captured".into(),
        },
        registry_only_tools: DebugEvidence::Unknown {
            reason: "not captured".into(),
        },
        tools: vec![],
    };
    let value = serde_json::to_value(&snapshot).expect("bounded snapshot");
    assert!(
        value.get("terminal").is_none(),
        "absent terminal is omitted"
    );
    assert!(
        value["payload_json_bytes"].is_null(),
        "unmeasured is not zero"
    );
    assert!(value["active_tool_catalog_sha256"].is_null());
    assert_eq!(value["registry_tool_count"]["status"], "unknown");
    assert_eq!(value["provider"]["status"], "unknown");
    assert_eq!(
        value["unavailable_for_this_request"],
        serde_json::json!(["provider_wire_payload"])
    );
    assert_eq!(snapshot.tools, vec![]);
}

#[test]
fn debug_cache_observation_keeps_current_and_previous_distinct() {
    let previous = DebugPromptInspection {
        base_static_prefix_hash: "before".into(),
        full_request_prefix_hash: "before".into(),
        tool_catalog_hash: "".into(),
        layers: vec![],
    };
    let mut current = previous.clone();
    current.base_static_prefix_hash = "after".into();
    let observation = DebugCacheInspectionObservation {
        current: current.clone(),
        previous: Some(previous),
        current_warmup_key: DebugWarmupKey {
            provider: "provider".into(),
            model: "model".into(),
            base_url: "endpoint".into(),
            static_prefix_hash: "after".into(),
            tool_catalog_hash: "".into(),
            project_pack_hash: "".into(),
            skills_hash: "".into(),
        },
        last_warmup_key: None,
        current_warmup_hash_short: "digest".into(),
        last_warmup_hash_short: None,
    };
    assert_eq!(
        observation
            .previous
            .as_ref()
            .unwrap()
            .base_static_prefix_hash,
        "before"
    );
    assert_eq!(observation.current.base_static_prefix_hash, "after");
    assert_ne!(observation.previous.as_ref(), Some(&observation.current));
    // Contract data alone cannot prove a host write; Phase 3 adapter tests
    // must assert that the synchronous post-render commit stores `current`.
}

#[test]
fn debug_diagnostics_facet_is_object_safe_and_independently_transportable() {
    fn object_safe(_: &dyn CommandDebugDiagnosticsContext) {}
    object_safe(&DebugDiagnostics);

    let mut diagnostics = DebugDiagnostics;
    let parts = CommandContexts::empty()
        .with_debug_diagnostics(&mut diagnostics)
        .into_parts();
    assert_eq!(
        parts
            .debug_diagnostics
            .expect("declared diagnostics facet")
            .balance_projection(),
        DebugBalanceProjection {
            provider_display_name: "Example".into(),
            supports_balance_api: false,
        }
    );
    assert_eq!(
        DebugDiagnostics.system_projection(),
        DebugSystemProjection {
            mode_label: "plan".into(),
            prompt: DebugSystemPrompt::Blocks(vec!["first".into(), "second".into()]),
        }
    );
    assert_ne!(
        DebugSystemPrompt::None,
        DebugSystemPrompt::Text(String::new())
    );
    assert_ne!(DebugSystemPrompt::Blocks(vec![]), DebugSystemPrompt::None);
    let usage = DebugDiagnostics.token_projection();
    assert_eq!(
        DebugDiagnostics.cache_telemetry().prefix_stability_pct,
        None
    );
    assert_eq!(
        DebugDiagnostics.prompt_context().tool_catalog_state,
        "absent"
    );
    assert!(DebugDiagnostics.tool_snapshot().is_none());
    assert_eq!(usage.last_input, None);
    assert_eq!(usage.last_output, Some(0));
    assert_eq!(usage.cache_miss, Some(0));
    assert_eq!(usage.cost, DebugDiagnostics.cost_projection());
    for absent in [
        parts.session.is_none(),
        parts.model.is_none(),
        parts.cost.is_none(),
        parts.mode_policy.is_none(),
        parts.system_prompt.is_none(),
        parts.skills.is_none(),
        parts.workspace.is_none(),
        parts.presentation.is_none(),
        parts.media.is_none(),
        parts.memory.is_none(),
        parts.project.is_none(),
        parts.skill_group.is_none(),
        parts.plugin.is_none(),
        parts.lifecycle.is_none(),
        parts.control.is_none(),
        parts.export.is_none(),
    ] {
        assert!(absent, "diagnostics must not expose another facet");
    }
    assert!(
        CommandContexts::empty()
            .into_parts()
            .debug_diagnostics
            .is_none()
    );
}

#[test]
#[should_panic(expected = "debug diagnostics facet already set")]
fn debug_diagnostics_envelope_rejects_duplicate_authority() {
    let mut first = DebugDiagnostics;
    let mut second = DebugDiagnostics;
    let _ = CommandContexts::empty()
        .with_debug_diagnostics(&mut first)
        .with_debug_diagnostics(&mut second);
}

fn pure(value: Option<&str>) -> String {
    value.unwrap_or_default().to_owned()
}
fn contextual(_contexts: CommandContexts<'_>, value: Option<&str>) -> String {
    value.unwrap_or_default().to_owned()
}

#[test]
fn handlers_are_plain_function_pointers() {
    let pure_handler = CommandHandler::Pure(pure);
    let contextual_handler = CommandHandler::Contextual {
        capabilities: CommandCapabilities::NONE,
        handler: contextual,
    };
    match pure_handler {
        CommandHandler::Pure(handler) => assert_eq!(handler(Some("x")), "x"),
        _ => unreachable!(),
    }
    match contextual_handler {
        CommandHandler::Contextual {
            capabilities,
            handler,
        } => {
            assert!(capabilities.is_empty());
            assert_eq!(handler(CommandContexts::empty(), Some("y")), "y")
        }
        _ => unreachable!(),
    }
}

struct Sample;
impl RegisterCommand<String> for Sample {
    fn info() -> &'static CommandInfo {
        static INFO: CommandInfo = CommandInfo {
            name: "sample",
            aliases: &["s"],
            usage: "/sample",
            description_key: "command.sample",
        };
        &INFO
    }
    fn handler() -> CommandHandler<String> {
        CommandHandler::Pure(pure)
    }
}

#[test]
fn registration_shape_has_no_app_dependency() {
    assert_eq!(Sample::info().name, "sample");
    assert!(matches!(Sample::handler(), CommandHandler::Pure(_)));
}

// ---------------------------------------------------------------------------
// FEAT-018: presentation, media, and digest capabilities (D2-D5)
// ---------------------------------------------------------------------------

struct Presentation;
impl CommandPresentationContext for Presentation {
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
        if key == "automation_usage" {
            return Ok("Usage: /automation [list|show <id>]".to_string());
        }
        if key == "mcp_recommended_unknown_id" {
            let command = replacements
                .iter()
                .find(|(name, _)| *name == "recommendations_command")
                .map(|(_, value)| *value)
                .unwrap_or("/mcp recommendations");
            return Ok(format!("Unknown recommended MCP ID (try {command})"));
        }
        // D3: unknown keys fail safely without echoing the raw lookup key.
        Err("unknown translation key".to_string())
    }
}

struct Media;
impl CommandMediaContext for Media {
    fn attach_media(&mut self, path: &Path) -> Result<MediaAttachmentReceipt, String> {
        if path.extension().and_then(|ext| ext.to_str()) == Some("png") {
            Ok(MediaAttachmentReceipt {
                kind: "image".to_string(),
                path: path.to_path_buf(),
            })
        } else {
            Err("Unsupported attachment type".to_string())
        }
    }
}

struct DigestWorkspace;
impl CommandWorkspaceContext for DigestWorkspace {
    fn workspace(&self) -> PathBuf {
        PathBuf::from(".")
    }
    fn work_state_snapshot(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn operation_digest(&mut self) -> Result<String, String> {
        Ok("No active operations or to-do items.".to_string())
    }
}

#[test]
fn new_capabilities_are_object_safe_and_independently_transportable() {
    fn presentation(_: &dyn CommandPresentationContext) {}
    fn media(_: &dyn CommandMediaContext) {}
    fn digest_workspace(_: &dyn CommandWorkspaceContext) {}

    presentation(&Presentation);
    media(&Media);
    digest_workspace(&DigestWorkspace);
    fn export(_: &dyn CommandSessionExportContext) {}
    export(&FakeExport::default());

    let mut presentation = Presentation;
    let mut media = Media;
    let mut export = FakeExport::default();
    let parts = CommandContexts::empty()
        .with_presentation(&mut presentation)
        .with_media(&mut media)
        .with_export(&mut export)
        .into_parts();
    assert!(parts.presentation.is_some());
    assert!(parts.media.is_some());
    assert!(parts.export.is_some());
    assert!(parts.session.is_none());
}

#[test]
fn translation_contract_resolves_known_keys_and_fails_safely() {
    let presentation = Presentation;
    assert_eq!(
        presentation
            .translate("automation_usage", &[])
            .expect("known key"),
        "Usage: /automation [list|show <id>]"
    );
    assert_eq!(
        presentation
            .translate(
                "mcp_recommended_unknown_id",
                &[("recommendations_command", "/mcp recommendations")],
            )
            .expect("known key with named replacement"),
        "Unknown recommended MCP ID (try /mcp recommendations)"
    );
    let unknown = presentation.translate("no_such_key", &[]);
    assert!(unknown.is_err(), "unknown key must fail safely");
    let err = unknown.unwrap_err();
    assert!(
        !err.contains("no_such_key"),
        "no raw lookup key exposure (D3)"
    );
}

#[test]
fn media_contract_is_atomic_and_returns_only_portable_data() {
    let mut media = Media;
    let ok = media
        .attach_media(Path::new("/tmp/photo.png"))
        .expect("png");
    assert_eq!(ok.kind, "image");
    assert_eq!(ok.path, PathBuf::from("/tmp/photo.png"));

    let err = media.attach_media(Path::new("/tmp/notes.txt")).unwrap_err();
    assert!(!err.is_empty(), "safe error string");
}

#[test]
fn digest_operation_returns_final_text_and_safe_errors() {
    let mut workspace = DigestWorkspace;
    assert_eq!(
        workspace.operation_digest().expect("digest"),
        "No active operations or to-do items."
    );
}

#[test]
fn envelope_rejects_duplicate_new_slots_deterministically() {
    struct SecondPresentation;
    impl CommandPresentationContext for SecondPresentation {
        fn translate(&self, _key: &str, _r: &[(&str, &str)]) -> Result<String, String> {
            Ok(String::new())
        }
    }
    struct SecondMedia;
    impl CommandMediaContext for SecondMedia {
        fn attach_media(&mut self, _p: &Path) -> Result<MediaAttachmentReceipt, String> {
            Err("unused".to_string())
        }
    }

    let mut a = Presentation;
    let mut b = SecondPresentation;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_presentation(&mut a)
            .with_presentation(&mut b);
    }));
    assert!(result.is_err(), "duplicate presentation slot must assert");

    let mut a = Media;
    let mut b = SecondMedia;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_media(&mut a)
            .with_media(&mut b);
    }));
    assert!(result.is_err(), "duplicate media slot must assert");
}

// ---------------------------------------------------------------------------
// Project facet (FEAT-021 D1/D4)
// ---------------------------------------------------------------------------

/// Deterministic fake project facet over portable values only.
struct FakeProject {
    lsp_enabled: bool,
    goal: ProjectGoalState,
}

impl FakeProject {
    fn new() -> Self {
        Self {
            lsp_enabled: false,
            goal: ProjectGoalState {
                objective: Some("Ship FEAT-021".to_string()),
                status: ProjectGoalStatus::Active,
                pause_reason: None,
                started_at_elapsed_seconds: Some(42),
                time_used_seconds: 42,
                token_budget: Some(50_000),
                tokens_used: 1_000,
                session_total_tokens: 2_000,
                continuation_count: 3,
                pending_controls: false,
                last_known_objective: None,
                last_known_status: None,
                conversation_present: true,
                is_loading: false,
                goal_continuation_waiting: false,
            },
        }
    }
}

impl CommandProjectContext for FakeProject {
    fn lsp_enabled(&self) -> bool {
        self.lsp_enabled
    }

    fn lsp_set(&mut self, enabled: bool) -> Result<(), String> {
        self.lsp_enabled = enabled;
        Ok(())
    }

    fn goal_state(&self) -> ProjectGoalState {
        self.goal.clone()
    }
}

// ---------------------------------------------------------------------------
// FEAT-019: memory capability, typed outcomes, and workspace scoping (D1-D9)
// ---------------------------------------------------------------------------

/// Deterministic fake memory facet over portable values only. Tracks the
/// workspace argument discipline (D8): only workspace-scoped methods receive
/// the workspace path.
struct FakeMemory {
    hits: Vec<MemoryHit>,
    remembered_result: Option<MemoryRemembered>,
    workspace_id_result: Result<String, String>,
}

impl FakeMemory {
    fn new() -> Self {
        Self {
            hits: vec![MemoryHit {
                source: PathBuf::from("/mem/source.md"),
                line_start: 3,
                line_end: 5,
                text: "reviewed note".to_string(),
            }],
            remembered_result: Some(MemoryRemembered {
                source: PathBuf::from("/mem/global.md"),
                line_start: 7,
            }),
            workspace_id_result: Ok("owner/repo".to_string()),
        }
    }
}

impl CommandMemoryContext for FakeMemory {
    fn memory_path(&self) -> PathBuf {
        PathBuf::from("/mem/user-memory.md")
    }

    fn memory_enabled(&self) -> bool {
        true
    }

    fn status(&self) -> Result<MemoryStatus, String> {
        Ok(MemoryStatus {
            root: PathBuf::from("/mem/memory"),
            source: PathBuf::from("/mem/memory/global/global.md"),
            index: PathBuf::from("/mem/memory/index.db"),
        })
    }

    fn path(&self) -> Result<PathBuf, String> {
        Ok(PathBuf::from("/mem/memory"))
    }

    fn workspace_id(&self, _workspace: &Path) -> Result<String, String> {
        self.workspace_id_result.clone()
    }

    fn search(
        &self,
        _workspace: &Path,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryHit>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.hits.iter().take(limit).cloned().collect())
    }

    fn remember(
        &self,
        _target: MemoryRememberTarget,
        note: &str,
    ) -> Result<MemoryRemembered, String> {
        if note.is_empty() {
            return Err("empty note".to_string());
        }
        Ok(self.remembered_result.clone().unwrap_or(MemoryRemembered {
            source: PathBuf::from("/mem/global.md"),
            line_start: 1,
        }))
    }

    fn import(&self) -> Result<MemoryImportOutcome, String> {
        Ok(MemoryImportOutcome::Skipped)
    }

    fn get(&self, _workspace: &Path, id: i64) -> Result<MemoryGetOutcome, String> {
        if id == 42 {
            Ok(MemoryGetOutcome::Found(self.hits[0].clone()))
        } else {
            Ok(MemoryGetOutcome::NotFound)
        }
    }

    fn export(&self) -> Result<MemoryExport, String> {
        Ok(MemoryExport {
            content: "# memory\n\n- bullet".to_string(),
        })
    }

    fn reindex(&self) -> Result<MemoryReindex, String> {
        Ok(MemoryReindex { entry_count: 3 })
    }

    fn delete(&self, scope: MemoryDeleteScope) -> Result<MemoryDelete, String> {
        match scope {
            MemoryDeleteScope::All => Ok(MemoryDelete),
            MemoryDeleteScope::Global => Ok(MemoryDelete),
        }
    }

    fn delete_workspace(&self, _workspace: &Path) -> Result<MemoryDelete, String> {
        Ok(MemoryDelete)
    }
}

/// Recording fake that captures remember targets and delete scopes to prove
/// the typed target/scope discipline (D2/D8/D9). Interior mutability lets the
/// contract-level test assert exactly which operations the handler drives.
#[derive(Default)]
struct RecordingMemory {
    remembered_targets: std::cell::RefCell<Vec<MemoryRememberTarget>>,
    delete_scopes: std::cell::RefCell<Vec<String>>,
    workspace_deletes: std::cell::Cell<usize>,
}

impl RecordingMemory {
    fn new() -> Self {
        Self::default()
    }

    fn recorded_targets(&self) -> Vec<MemoryRememberTarget> {
        self.remembered_targets.borrow().clone()
    }

    fn recorded_delete_scopes(&self) -> Vec<String> {
        self.delete_scopes.borrow().clone()
    }

    fn recorded_workspace_deletes(&self) -> usize {
        self.workspace_deletes.get()
    }
}

impl CommandMemoryContext for RecordingMemory {
    fn memory_path(&self) -> PathBuf {
        PathBuf::from("/mem/user-memory.md")
    }

    fn memory_enabled(&self) -> bool {
        true
    }

    fn status(&self) -> Result<MemoryStatus, String> {
        unreachable!("recording fake")
    }

    fn path(&self) -> Result<PathBuf, String> {
        unreachable!("recording fake")
    }

    fn workspace_id(&self, _workspace: &Path) -> Result<String, String> {
        Ok("owner/repo".to_string())
    }

    fn search(
        &self,
        _workspace: &Path,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<MemoryHit>, String> {
        unreachable!("recording fake")
    }

    fn remember(
        &self,
        target: MemoryRememberTarget,
        _note: &str,
    ) -> Result<MemoryRemembered, String> {
        self.remembered_targets.borrow_mut().push(target);
        Ok(MemoryRemembered {
            source: PathBuf::from("/mem/global.md"),
            line_start: 1,
        })
    }

    fn import(&self) -> Result<MemoryImportOutcome, String> {
        unreachable!("recording fake")
    }

    fn get(&self, _workspace: &Path, _id: i64) -> Result<MemoryGetOutcome, String> {
        unreachable!("recording fake")
    }

    fn export(&self) -> Result<MemoryExport, String> {
        unreachable!("recording fake")
    }

    fn reindex(&self) -> Result<MemoryReindex, String> {
        unreachable!("recording fake")
    }

    fn delete(&self, scope: MemoryDeleteScope) -> Result<MemoryDelete, String> {
        self.delete_scopes.borrow_mut().push(match scope {
            MemoryDeleteScope::All => "all".to_string(),
            MemoryDeleteScope::Global => "global".to_string(),
        });
        Ok(MemoryDelete)
    }

    fn delete_workspace(&self, _workspace: &Path) -> Result<MemoryDelete, String> {
        self.workspace_deletes.set(self.workspace_deletes.get() + 1);
        Ok(MemoryDelete)
    }
}

#[test]
fn project_facet_is_object_safe_and_typed() {
    fn project(_: &dyn CommandProjectContext) {}
    project(&FakeProject::new());

    let mut project = FakeProject::new();
    assert!(!project.lsp_enabled());
    project.lsp_set(true).unwrap();
    assert!(project.lsp_enabled());
    project.lsp_set(false).unwrap();
    assert!(!project.lsp_enabled());
}

#[test]
fn project_goal_state_preserves_semantic_values() {
    let project = FakeProject::new();
    let goal = project.goal_state();
    assert_eq!(goal.objective.as_deref(), Some("Ship FEAT-021"));
    assert_eq!(goal.status, ProjectGoalStatus::Active);
    assert_eq!(goal.pause_reason, None);
    assert_eq!(goal.started_at_elapsed_seconds, Some(42));
    assert_eq!(goal.time_used_seconds, 42);
    assert_eq!(goal.token_budget, Some(50_000));
    assert_eq!(goal.tokens_used, 1_000);
    assert_eq!(goal.session_total_tokens, 2_000);
    assert_eq!(goal.continuation_count, 3);
    assert!(!goal.pending_controls);
    assert_eq!(goal.last_known_objective, None);
    assert_eq!(goal.last_known_status, None);
    assert!(goal.conversation_present);
    assert!(!goal.is_loading);
    assert!(!goal.goal_continuation_waiting);
}

#[test]
fn project_goal_status_variants_are_distinguishable() {
    let paused = ProjectGoalState {
        status: ProjectGoalStatus::Paused,
        pause_reason: Some("user".to_string()),
        ..FakeProject::new().goal
    };
    assert_eq!(paused.status, ProjectGoalStatus::Paused);
    assert_eq!(paused.pause_reason.as_deref(), Some("user"));

    let complete = ProjectGoalState {
        status: ProjectGoalStatus::Complete,
        ..paused
    };
    assert_eq!(complete.status, ProjectGoalStatus::Complete);
    assert_ne!(complete.status, ProjectGoalStatus::Blocked);
}

#[test]
fn project_facet_transports_through_envelope_when_declared() {
    let mut project = FakeProject::new();
    let parts = CommandContexts::empty()
        .with_project(&mut project)
        .into_parts();
    assert!(parts.project.is_some());
    assert!(parts.session.is_none());

    // PROJECT combined with WORKSPACE (init) and PRESENTATION (goal).
    let mut workspace = Workspace;
    let parts = CommandContexts::empty()
        .with_project(&mut project)
        .with_workspace(&mut workspace)
        .into_parts();
    assert!(parts.project.is_some());
    assert!(parts.workspace.is_some());
    assert!(parts.presentation.is_none());
}

#[test]
fn envelope_rejects_duplicate_project_slot_deterministically() {
    let mut a = FakeProject::new();
    let mut b = FakeProject::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_project(&mut a)
            .with_project(&mut b);
    }));
    assert!(result.is_err(), "duplicate project slot must assert");
}

#[test]
fn memory_facet_is_object_safe_and_typed() {
    fn memory(_: &dyn CommandMemoryContext) {}
    let fake = FakeMemory::new();
    memory(&fake);

    assert_eq!(fake.memory_path(), PathBuf::from("/mem/user-memory.md"));
    assert!(fake.memory_enabled());
    let status = fake.status().expect("status");
    assert_eq!(status.root, PathBuf::from("/mem/memory"));
    assert_eq!(status.source, PathBuf::from("/mem/memory/global/global.md"));
    assert_eq!(status.index, PathBuf::from("/mem/memory/index.db"));
}

#[test]
fn memory_typed_results_preserve_semantic_distinctions() {
    let fake = FakeMemory::new();

    // Search returns semantic hits, never preformatted messages.
    let hits = fake.search(Path::new("/ws"), "note", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, PathBuf::from("/mem/source.md"));
    assert_eq!(hits[0].line_start, 3);
    assert_eq!(hits[0].line_end, 5);
    assert_eq!(hits[0].text, "reviewed note");
    assert!(
        fake.search(Path::new("/ws"), "", 10)
            .expect("empty")
            .is_empty()
    );

    // Get distinguishes found from not-found without an error string.
    assert!(matches!(
        fake.get(Path::new("/ws"), 42),
        Ok(MemoryGetOutcome::Found(_))
    ));
    assert_eq!(
        fake.get(Path::new("/ws"), 1).expect("get"),
        MemoryGetOutcome::NotFound
    );

    // Export carries the raw document, not a command response.
    let exported = fake.export().expect("export");
    assert_eq!(exported.content, "# memory\n\n- bullet");

    // Reindex carries the typed count.
    assert_eq!(fake.reindex().expect("reindex").entry_count, 3);

    // Remember distinguishes global from workspace via the typed target.
    let global = fake
        .remember(MemoryRememberTarget::Global, "note")
        .expect("global remember");
    assert_eq!(global.source, PathBuf::from("/mem/global.md"));
    assert_eq!(global.line_start, 7);
    let workspace = fake
        .remember(
            MemoryRememberTarget::Workspace {
                workspace_id: "owner/repo".to_string(),
            },
            "note",
        )
        .expect("workspace remember");
    assert_eq!(workspace.source, PathBuf::from("/mem/global.md"));

    // Import distinguishes imported from skipped.
    assert_eq!(fake.import().expect("import"), MemoryImportOutcome::Skipped);
    assert_eq!(
        MemoryImportOutcome::Imported {
            destination: PathBuf::from("/mem/global.md")
        },
        MemoryImportOutcome::Imported {
            destination: PathBuf::from("/mem/global.md")
        }
    );

    // Remember rejects empty notes with a safe error, never a panic.
    assert!(fake.remember(MemoryRememberTarget::Global, "").is_err());

    // Zero-field delete outcome stays distinguishable.
    assert_eq!(fake.delete(MemoryDeleteScope::All), Ok(MemoryDelete));
}

#[test]
fn memory_delete_and_remember_targets_are_typed_and_scoped() {
    let memory = RecordingMemory::new();
    let _ = memory.delete(MemoryDeleteScope::All);
    let _ = memory.delete(MemoryDeleteScope::Global);
    let _ = memory.delete_workspace(Path::new("/ws"));
    let _ = memory.remember(MemoryRememberTarget::Global, "a");
    let _ = memory.remember(
        MemoryRememberTarget::Workspace {
            workspace_id: "owner/repo".to_string(),
        },
        "b",
    );

    // The non-workspace delete method receives exactly the all/global scopes;
    // workspace deletion goes through the distinct typed method (D8/D9).
    assert_eq!(memory.recorded_delete_scopes(), vec!["all", "global"]);
    assert_eq!(memory.recorded_workspace_deletes(), 1);

    // Remember targets preserve the typed global/workspace distinction.
    assert_eq!(
        memory.recorded_targets(),
        vec![
            MemoryRememberTarget::Global,
            MemoryRememberTarget::Workspace {
                workspace_id: "owner/repo".to_string(),
            },
        ]
    );
}

#[test]
fn capabilities_declare_exact_memory_authority() {
    let workspace = CommandCapabilities::WORKSPACE;
    let memory = CommandCapabilities::MEMORY;
    let workspace_memory = workspace.union(memory);

    assert_eq!(
        workspace_memory,
        CommandCapabilities::WORKSPACE | CommandCapabilities::MEMORY
    );
    assert_ne!(workspace_memory, workspace);
    assert_ne!(workspace_memory, memory);
    assert!(workspace_memory.contains(CommandCapabilities::WORKSPACE));
    assert!(workspace_memory.contains(CommandCapabilities::MEMORY));
    assert!(!workspace.contains(CommandCapabilities::MEMORY));
    assert!(!memory.contains(CommandCapabilities::WORKSPACE));
    assert!(CommandCapabilities::NONE.is_empty());
    assert!(!workspace_memory.contains(CommandCapabilities::NONE));
    assert!(!CommandCapabilities::NONE.contains(CommandCapabilities::NONE));
    // No presentation or media authority is declared for the memory group.
    assert!(!workspace_memory.contains(CommandCapabilities::PRESENTATION));
    assert!(!workspace_memory.contains(CommandCapabilities::MEDIA));
    // Existing capability identities stay stable.
    assert_ne!(CommandCapabilities::SESSION, CommandCapabilities::MODEL);
}

#[test]
fn memory_facet_transports_through_envelope_when_declared() {
    let mut memory = FakeMemory::new();
    let parts = CommandContexts::empty()
        .with_memory(&mut memory)
        .into_parts();
    assert!(parts.memory.is_some());
    assert!(parts.session.is_none());
    assert!(parts.workspace.is_none());

    // Undeclared slots stay absent when the memory facet is carried alone.
    let mut workspace = Workspace;
    let parts = CommandContexts::empty()
        .with_memory(&mut memory)
        .with_workspace(&mut workspace)
        .into_parts();
    assert!(parts.memory.is_some());
    assert!(parts.workspace.is_some());
    assert!(parts.presentation.is_none());
    assert!(parts.media.is_none());
}

#[test]
fn envelope_rejects_duplicate_memory_slot_deterministically() {
    let mut a = FakeMemory::new();
    let mut b = FakeMemory::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_memory(&mut a)
            .with_memory(&mut b);
    }));
    assert!(result.is_err(), "duplicate memory slot must assert");
}

// ---------------------------------------------------------------------------
// FEAT-020: plugin capability, portable DTOs, and envelope slot (D1-D11)
// ---------------------------------------------------------------------------

/// Deterministic fake plugin facet over portable values only.
struct FakePlugin {
    summaries: Vec<PluginSummary>,
    detail: Option<PluginDetail>,
    installed: bool,
    managed_candidates: Vec<PluginManagedCandidate>,
}

impl FakePlugin {
    fn new() -> Self {
        Self {
            summaries: vec![PluginSummary {
                name: "demo".to_string(),
                id: "demo@1.0.0".to_string(),
                state_label: "active".to_string(),
                scope: "user".to_string(),
                trust_status: "trusted".to_string(),
                compatibility: "full".to_string(),
                inventory: "skills=1 mcp=0".to_string(),
                active: true,
                trusted: true,
                enabled: true,
            }],
            detail: Some(PluginDetail {
                name: "demo".to_string(),
                id: "demo@1.0.0".to_string(),
                inventory_summary: "skills=1 mcp=0".to_string(),
                version: "1.0.0".to_string(),
                origin: "local".to_string(),
                scope: "user".to_string(),
                state_label: "active".to_string(),
                trust_status: "trusted".to_string(),
                compatibility: "full".to_string(),
                content_hash: "abc".to_string(),
                capability_hash: "def".to_string(),
                canonical_root: PathBuf::from("/plugins/demo"),
                active: true,
                trusted: true,
                enabled: true,
                unsupported_labels: Vec::new(),
                supported_labels: vec!["skills".to_string()],
                skills: vec!["demo:demo-skill".to_string()],
                filesystem_roots: Vec::new(),
                network_hosts: Vec::new(),
                stdio_mcp_servers: 0,
                lifecycle_mutation: false,
                mcp_servers: Vec::new(),
                diagnostics: Vec::new(),
            }),
            installed: false,
            managed_candidates: Vec::new(),
        }
    }
}

impl CommandPluginContext for FakePlugin {
    fn summaries(&self) -> Result<Vec<PluginSummary>, String> {
        Ok(self.summaries.clone())
    }

    fn detail(&self, selector: &str) -> Result<PluginDetail, String> {
        if selector == "demo" {
            self.detail
                .clone()
                .ok_or_else(|| "missing detail".to_string())
        } else {
            Err(format!("no plugin named {selector}"))
        }
    }

    fn registry_diagnostics(&self) -> Vec<PluginDiagnostic> {
        Vec::new()
    }

    fn validation_is_clean(&self) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.summaries.len()
    }

    fn reload(&mut self) -> Result<usize, String> {
        Ok(self.summaries.len())
    }

    fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    fn reload_nudge(&mut self) -> Option<String> {
        None
    }

    fn state_path(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/plugins/state.json"))
    }

    fn suggest(&self, task: &str) -> Result<Vec<PluginSuggestion>, String> {
        if task.len() < 3 {
            return Err("task too short".to_string());
        }
        Ok(vec![PluginSuggestion {
            name: "demo".to_string(),
            state_label: "active".to_string(),
            description: "Demo bundle".to_string(),
            why: vec![task.to_string()],
            next_step: "Already active: /plugin show demo".to_string(),
        }])
    }

    fn trust(&mut self, _selector: &str, token: &str) -> Result<(), String> {
        if token == "abc.def" {
            Ok(())
        } else {
            Err("Review token does not match this bundle content and capability set".to_string())
        }
    }

    fn enable(&mut self, _selector: &str) -> Result<(), String> {
        Ok(())
    }

    fn disable(&mut self, _selector: &str) -> Result<(), String> {
        Ok(())
    }

    fn revoke_trust(&mut self, _selector: &str) -> Result<(), String> {
        Ok(())
    }

    fn install(
        &mut self,
        _source: &str,
        expected_content_hash: Option<&str>,
    ) -> Result<PluginMutationReceipt, String> {
        if let Some(expected) = expected_content_hash
            && expected != "abc"
        {
            return Err("content hash mismatch".to_string());
        }
        self.installed = true;
        Ok(PluginMutationReceipt {
            name: "demo".to_string(),
            path: Some(PathBuf::from("/plugins/demo")),
            content_hash: Some("abc".to_string()),
            installed_content_hash: Some("abc".to_string()),
            outcome: PluginMutationOutcome::Installed,
        })
    }

    fn update(&mut self, _selector: &str) -> Result<PluginMutationReceipt, String> {
        Ok(PluginMutationReceipt {
            name: "demo".to_string(),
            path: None,
            content_hash: None,
            installed_content_hash: None,
            outcome: PluginMutationOutcome::NoChange,
        })
    }

    fn uninstall(&mut self, _selector: &str) -> Result<PluginMutationReceipt, String> {
        Ok(PluginMutationReceipt {
            name: "demo".to_string(),
            path: None,
            content_hash: None,
            installed_content_hash: None,
            outcome: PluginMutationOutcome::Uninstalled,
        })
    }

    fn uninstall_path(&mut self, _name: &str, _plugins_dir: &Path) -> Result<(), String> {
        Ok(())
    }

    fn export(&self, _selector: &str, target: &Path) -> Result<PluginExportReceipt, String> {
        Ok(PluginExportReceipt {
            exported_name: "demo".to_string(),
            target: target.to_path_buf(),
            display_name: Some("Demo Bundle".to_string()),
            wrote_mcp_json: false,
            files_copied: 2,
            skills_normalized: false,
        })
    }

    fn legacy_scan(&self) -> Result<Option<PluginLegacyScan>, String> {
        Ok(None)
    }

    fn managed_scan(&self, _home_override: Option<&Path>) -> Result<PluginManagedScan, String> {
        Ok(PluginManagedScan {
            root: PathBuf::from("/kimi/managed"),
            candidates: self.managed_candidates.clone(),
            rejected: Vec::new(),
        })
    }

    fn managed_install(
        &mut self,
        canonical_path: &Path,
        expected_content_hash: &str,
    ) -> Result<PluginMutationReceipt, String> {
        if expected_content_hash != "abc" {
            return Err("Kimi candidate changed".to_string());
        }
        Ok(PluginMutationReceipt {
            name: "kimi-demo".to_string(),
            path: Some(canonical_path.to_path_buf()),
            content_hash: Some("abc".to_string()),
            installed_content_hash: Some("abc".to_string()),
            outcome: PluginMutationOutcome::Installed,
        })
    }

    fn marketplace_state(&self) -> Result<PluginMarketplaceState, String> {
        Ok(PluginMarketplaceState {
            official: Some(PluginMarketplaceCatalog {
                id: "official".to_string(),
                source_path: None,
                display_name: None,
                description: Some("Built into this release".to_string()),
                format: "codewhale".to_string(),
                tier: "official".to_string(),
                publisher: Some("Codewhale".to_string()),
                total_candidates: 1,
                warning_count: 0,
                candidates: Vec::new(),
                diagnostics: Vec::new(),
            }),
            stored: Vec::new(),
        })
    }

    fn marketplace_add(
        &mut self,
        name: &str,
        _path: &Path,
    ) -> Result<PluginMarketplaceAddReceipt, String> {
        if name == "official" {
            return Err(
                "`official` is the catalog built into Codewhale; pick another name.".to_string(),
            );
        }
        Ok(PluginMarketplaceAddReceipt {
            name: name.to_string(),
            candidate_count: 0,
            warning_count: 0,
            catalog: PluginMarketplaceCatalog {
                id: name.to_string(),
                source_path: None,
                display_name: None,
                description: None,
                format: "kimi".to_string(),
                tier: "community".to_string(),
                publisher: None,
                total_candidates: 0,
                warning_count: 0,
                candidates: Vec::new(),
                diagnostics: Vec::new(),
            },
        })
    }

    fn marketplace_remove(&mut self, _name: &str) -> Result<bool, String> {
        Ok(true)
    }

    fn marketplace_install(
        &mut self,
        _catalog: &str,
        _candidate: &str,
    ) -> Result<PluginMutationReceipt, String> {
        Ok(PluginMutationReceipt {
            name: "market-demo".to_string(),
            path: None,
            content_hash: None,
            installed_content_hash: None,
            outcome: PluginMutationOutcome::Installed,
        })
    }

    fn suggestion_dismissals(&self) -> Result<PluginSuggestionDismissals, String> {
        Ok(PluginSuggestionDismissals::default())
    }

    fn reset_suggestion_dismissals(&mut self, _name: Option<&str>) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn plugin_facet_is_object_safe_and_typed() {
    fn plugin(_: &dyn CommandPluginContext) {}
    plugin(&FakePlugin::new());

    let plugin = FakePlugin::new();
    assert_eq!(plugin.len(), 1);
    assert!(!plugin.is_empty());
    assert!(plugin.validation_is_clean());
    let summaries = plugin.summaries().unwrap();
    assert_eq!(summaries[0].name, "demo");
    assert_eq!(summaries[0].state_label, "active");
}

#[test]
fn plugin_detail_preserves_semantic_values() {
    let plugin = FakePlugin::new();
    let detail = plugin.detail("demo").unwrap();
    assert_eq!(detail.content_hash, "abc");
    assert_eq!(detail.capability_hash, "def");
    assert_eq!(detail.compatibility, "full");
    assert!(detail.active);
    assert_eq!(detail.skills, vec!["demo:demo-skill"]);
    // Unknown selector fails safely.
    assert!(plugin.detail("nope").is_err());
}

#[test]
fn plugin_mutation_receipts_distinguish_outcomes() {
    let mut plugin = FakePlugin::new();
    let installed = plugin.install("path:/demo", Some("abc")).unwrap();
    assert_eq!(installed.outcome, PluginMutationOutcome::Installed);
    assert_eq!(installed.installed_content_hash.as_deref(), Some("abc"));

    // Exact-hash mismatch fails before any install side effect.
    let err = plugin.install("path:/demo", Some("wrong")).unwrap_err();
    assert!(err.contains("content hash mismatch"));

    let uninstalled = plugin.uninstall("demo").unwrap();
    assert_eq!(uninstalled.outcome, PluginMutationOutcome::Uninstalled);

    plugin.trust("demo", "abc.def").unwrap();
    let trust_err = plugin.trust("demo", "bad.token").unwrap_err();
    assert!(trust_err.contains("Review token does not match"));
}

#[test]
fn plugin_managed_and_marketplace_values_are_portable() {
    let mut plugin = FakePlugin::new();
    let scan = plugin.managed_scan(None).unwrap();
    assert_eq!(scan.root, PathBuf::from("/kimi/managed"));
    assert!(scan.candidates.is_empty());

    plugin.managed_candidates.push(PluginManagedCandidate {
        name: "kimi-demo".to_string(),
        version: "1.0.0".to_string(),
        license: Some("MIT".to_string()),
        canonical_path: PathBuf::from("/kimi/managed/kimi-demo"),
        content_hash: "abc".to_string(),
        capability_hash: "def".to_string(),
        inventory: "skills=1".to_string(),
        applicable: true,
    });
    let scan = plugin.managed_scan(None).unwrap();
    assert_eq!(scan.candidates[0].name, "kimi-demo");
    assert_eq!(scan.candidates[0].license.as_deref(), Some("MIT"));

    let state = plugin.marketplace_state().unwrap();
    let official = state.official.as_ref().expect("fake official catalog");
    assert_eq!(official.id, "official");
    assert_eq!(official.tier, "official");
    assert!(state.stored.is_empty());

    let add = plugin
        .marketplace_add("custom", Path::new("/catalog.json"))
        .unwrap();
    assert_eq!(add.name, "custom");
    assert_eq!(add.catalog.format, "kimi");

    let err = plugin
        .marketplace_add("official", Path::new("/x.json"))
        .unwrap_err();
    assert!(err.contains("built into Codewhale"));
}

#[test]
fn plugin_suggest_is_read_only_and_safe() {
    let plugin = FakePlugin::new();
    let err = plugin.suggest("ab").unwrap_err();
    assert!(err.contains("too short"));
    let suggestions = plugin.suggest("translate").unwrap();
    assert_eq!(suggestions[0].name, "demo");
    assert_eq!(
        suggestions[0].next_step,
        "Already active: /plugin show demo"
    );
}

#[test]
fn plugin_facet_transports_through_envelope_when_declared() {
    let mut plugin = FakePlugin::new();
    let parts = CommandContexts::empty()
        .with_plugin(&mut plugin)
        .into_parts();
    assert!(parts.plugin.is_some());
    assert!(parts.session.is_none());
    assert!(parts.memory.is_none());

    // Undeclared slots stay absent when the plugin facet is carried alone.
    let mut workspace = Workspace;
    let parts = CommandContexts::empty()
        .with_plugin(&mut plugin)
        .with_workspace(&mut workspace)
        .into_parts();
    assert!(parts.plugin.is_some());
    assert!(parts.workspace.is_some());
    assert!(parts.presentation.is_none());
}

#[test]
fn envelope_rejects_duplicate_plugin_slot_deterministically() {
    let mut a = FakePlugin::new();
    let mut b = FakePlugin::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_plugin(&mut a)
            .with_plugin(&mut b);
    }));
    assert!(result.is_err(), "duplicate plugin slot must assert");
}

#[test]
fn plugin_capability_bit_is_stable_and_distinct() {
    let plugin = CommandCapabilities::PLUGIN;
    assert_eq!(plugin, CommandCapabilities::PLUGIN);
    assert!(plugin.contains(CommandCapabilities::PLUGIN));
    assert!(!plugin.contains(CommandCapabilities::MEMORY));
    assert!(!plugin.contains(CommandCapabilities::PROJECT));
    assert!(!plugin.contains(CommandCapabilities::SKILL_GROUP));
    assert!(!plugin.contains(CommandCapabilities::WORKSPACE));

    let plugin_workspace = CommandCapabilities::PLUGIN.union(CommandCapabilities::WORKSPACE);
    assert!(plugin_workspace.contains(CommandCapabilities::PLUGIN));
    assert!(plugin_workspace.contains(CommandCapabilities::WORKSPACE));
    assert!(!plugin_workspace.contains(CommandCapabilities::MEMORY));

    // The plugin group declares exactly WORKSPACE | PRESENTATION | PLUGIN.
    let exact = CommandCapabilities::WORKSPACE
        .union(CommandCapabilities::PRESENTATION)
        .union(CommandCapabilities::PLUGIN);
    assert!(exact.contains(CommandCapabilities::PLUGIN));
    assert!(exact.contains(CommandCapabilities::PRESENTATION));
    assert!(!exact.contains(CommandCapabilities::MEDIA));
    assert!(!exact.contains(CommandCapabilities::MEMORY));
    assert!(!exact.contains(CommandCapabilities::PROJECT));
    assert!(!exact.contains(CommandCapabilities::SKILL_GROUP));
    assert!(!exact.contains(CommandCapabilities::SKILLS));
}

// FEAT-022: skill-group facet (CommandSkillGroupContext)
// ---------------------------------------------------------------------------

struct FakeSkillGroup {
    projection: SkillRegistryProjection,
    activation_result: Result<SkillActivationOutcome, SkillActivationError>,
    receipt: SkillMutationReceipt,
    remote: Result<RemoteRegistryOutcome, String>,
    sync: Result<SkillSyncOutcome, String>,
    review: Result<ReviewOutcome, String>,
    snapshots: Vec<SnapshotEntry>,
    restore_ok: bool,
    approval: CommandApprovalState,
}

impl FakeSkillGroup {
    fn new() -> Self {
        Self {
            projection: SkillRegistryProjection {
                workspace: "/ws".into(),
                skills_dir: "/ws/.codewhale/skills".into(),
                mode_label: "compatible".into(),
                dirs: vec!["/ws/.codewhale/skills".into()],
                entries: vec![SkillEntry {
                    name: "demo".into(),
                    description: "Demo skill".into(),
                    source: SkillSourceKind::Native,
                    path: Some("/ws/.codewhale/skills/demo/SKILL.md".into()),
                    bundled_tier: None,
                }],
                warnings: vec!["one warning".into()],
                total: 1,
            },
            activation_result: Ok(SkillActivationOutcome {
                name: "demo".into(),
                description: "Demo skill".into(),
            }),
            receipt: SkillMutationReceipt {
                name: "demo".into(),
                safe_target_path: "/ws/.codewhale/skills/demo".into(),
                outcome: SkillMutationOutcome::Installed,
            },
            remote: Ok(RemoteRegistryOutcome::Loaded {
                entries: vec![RemoteSkillEntry {
                    name: "demo".into(),
                    description: Some("Remote demo".into()),
                    source: "github.com/acme/skills".into(),
                }],
            }),
            sync: Ok(SkillSyncOutcome::Done {
                total: 1,
                downloaded: 1,
                fresh: 0,
                failed: 0,
                entries: vec![SkillSyncEntry::Downloaded {
                    name: "demo".into(),
                    path: "/cache/demo".into(),
                }],
            }),
            review: Ok(ReviewOutcome::Ready),
            snapshots: vec![SnapshotEntry {
                id: "abcdef123456".into(),
                label: "pre-turn:1".into(),
                timestamp: 1_700_000_000,
            }],
            restore_ok: true,
            approval: CommandApprovalState {
                yolo: true,
                trust_mode: false,
            },
        }
    }
}

impl CommandSkillGroupContext for FakeSkillGroup {
    fn skill_registry_projection(&self) -> SkillRegistryProjection {
        self.projection.clone()
    }

    fn activate_skill(
        &mut self,
        _name: &str,
    ) -> Result<SkillActivationOutcome, SkillActivationError> {
        self.activation_result.clone()
    }

    fn install_skill(
        &mut self,
        _scope: Option<SkillTargetScope>,
        _spec: &str,
    ) -> Result<SkillMutationReceipt, String> {
        Ok(self.receipt.clone())
    }

    fn update_skill(
        &mut self,
        _scope: Option<SkillTargetScope>,
        _name: &str,
    ) -> Result<SkillMutationReceipt, String> {
        Ok(self.receipt.clone())
    }

    fn uninstall_skill(
        &mut self,
        _scope: Option<SkillTargetScope>,
        _name: &str,
    ) -> Result<SkillMutationReceipt, String> {
        Ok(self.receipt.clone())
    }

    fn trust_skill(
        &mut self,
        _scope: Option<SkillTargetScope>,
        _name: &str,
    ) -> Result<SkillMutationReceipt, String> {
        Ok(self.receipt.clone())
    }

    fn fetch_remote_registry(&mut self) -> Result<RemoteRegistryOutcome, String> {
        self.remote.clone()
    }

    fn recommend_skills(&mut self, task: &str) -> Result<Vec<SkillRecommendation>, String> {
        Ok(vec![SkillRecommendation {
            name: format!("rec-{task}"),
            description: Some("Recommended".into()),
            matched_terms: vec!["term".into()],
        }])
    }

    fn sync_registry(&mut self) -> Result<SkillSyncOutcome, String> {
        self.sync.clone()
    }

    fn run_review(&mut self) -> Result<ReviewOutcome, String> {
        self.review.clone()
    }

    fn snapshot_list(&mut self, _limit: usize) -> Result<Vec<SnapshotEntry>, String> {
        Ok(self.snapshots.clone())
    }

    fn restore_snapshot(&mut self, _id: &str) -> Result<(), String> {
        if self.restore_ok {
            Ok(())
        } else {
            Err("Restore failed: boom".into())
        }
    }

    fn approval_state(&self) -> CommandApprovalState {
        self.approval
    }
}

#[test]
fn skill_group_facet_is_object_safe_and_typed() {
    fn project(_: &dyn CommandSkillGroupContext) {}
    project(&FakeSkillGroup::new());

    let group = FakeSkillGroup::new();
    let projection = group.skill_registry_projection();
    assert_eq!(projection.total, 1);
    assert_eq!(projection.entries[0].name, "demo");
    assert!(group.approval_state().yolo);
}

#[test]
fn skill_registry_projection_preserves_semantic_values() {
    let group = FakeSkillGroup::new();
    let projection = group.skill_registry_projection();
    assert_eq!(projection.workspace, "/ws");
    assert_eq!(projection.skills_dir, "/ws/.codewhale/skills");
    assert_eq!(projection.mode_label, "compatible");
    assert_eq!(projection.dirs, vec!["/ws/.codewhale/skills"]);
    assert_eq!(projection.warnings, vec!["one warning"]);
    assert_eq!(projection.entries.len(), 1);
    let entry = &projection.entries[0];
    assert_eq!(entry.name, "demo");
    assert_eq!(entry.description, "Demo skill");
    assert_eq!(entry.source, SkillSourceKind::Native);
    assert_eq!(
        entry.path.as_deref(),
        Some("/ws/.codewhale/skills/demo/SKILL.md")
    );
    assert_eq!(entry.bundled_tier, None);
}

#[test]
fn skill_bundled_tier_headings_are_stable() {
    assert_eq!(SkillBundledTier::CoreAgentic.heading(), "Core agentic");
    assert_eq!(
        SkillBundledTier::FormatTooling.heading(),
        "Format & tooling"
    );
}

#[test]
fn skill_mutation_receipt_preserves_outcome_variants() {
    let installed = FakeSkillGroup::new().receipt;
    assert_eq!(installed.name, "demo");
    assert_eq!(installed.outcome, SkillMutationOutcome::Installed);

    let denied = SkillMutationReceipt {
        outcome: SkillMutationOutcome::NetworkDenied("acme.com".into()),
        ..installed.clone()
    };
    assert_eq!(
        denied.outcome,
        SkillMutationOutcome::NetworkDenied("acme.com".into())
    );

    let approval = SkillMutationReceipt {
        outcome: SkillMutationOutcome::NeedsApproval("acme.com".into()),
        ..installed.clone()
    };
    assert_eq!(
        approval.outcome,
        SkillMutationOutcome::NeedsApproval("acme.com".into())
    );

    assert_ne!(installed.outcome, denied.outcome);
    assert_ne!(installed.outcome, approval.outcome);
    assert_ne!(denied.outcome, approval.outcome);
}

#[test]
fn skill_source_kind_variants_are_distinguishable() {
    let native = SkillSourceKind::Native;
    let plugin = SkillSourceKind::Plugin {
        plugin_name: "acme".into(),
        plugin_id: "acme-1".into(),
    };
    assert_ne!(native, plugin);
    assert_eq!(
        plugin,
        SkillSourceKind::Plugin {
            plugin_name: "acme".into(),
            plugin_id: "acme-1".into(),
        }
    );
}

#[test]
fn remote_registry_outcome_variants_are_distinguishable() {
    let loaded = RemoteRegistryOutcome::Loaded {
        entries: vec![RemoteSkillEntry {
            name: "demo".into(),
            description: None,
            source: "acme".into(),
        }],
    };
    let approval = RemoteRegistryOutcome::NeedsApproval("acme.com".into());
    let denied = RemoteRegistryOutcome::Denied("acme.com".into());
    assert_ne!(loaded, approval);
    assert_ne!(loaded, denied);
    assert_ne!(approval, denied);
}

#[test]
fn skill_sync_outcome_preserves_all_entry_variants() {
    let outcome = SkillSyncOutcome::Done {
        total: 4,
        downloaded: 1,
        fresh: 1,
        failed: 2,
        entries: vec![
            SkillSyncEntry::Downloaded {
                name: "a".into(),
                path: "/cache/a".into(),
            },
            SkillSyncEntry::Fresh { name: "b".into() },
            SkillSyncEntry::Failed {
                name: "c".into(),
                reason: "boom".into(),
            },
            SkillSyncEntry::Denied {
                name: "d".into(),
                host: "acme.com".into(),
            },
            SkillSyncEntry::NeedsApproval {
                name: "e".into(),
                host: "acme.com".into(),
            },
        ],
    };
    let SkillSyncOutcome::Done {
        total,
        downloaded,
        fresh,
        failed,
        entries,
    } = &outcome
    else {
        panic!("expected Done");
    };
    assert_eq!(*total, 4);
    assert_eq!(*downloaded, 1);
    assert_eq!(*fresh, 1);
    assert_eq!(*failed, 2);
    assert_eq!(entries.len(), 5);
    assert!(matches!(entries[0], SkillSyncEntry::Downloaded { .. }));
    assert!(matches!(entries[1], SkillSyncEntry::Fresh { .. }));
    assert!(matches!(entries[2], SkillSyncEntry::Failed { .. }));
    assert!(matches!(entries[3], SkillSyncEntry::Denied { .. }));
    assert!(matches!(entries[4], SkillSyncEntry::NeedsApproval { .. }));
}

#[test]
fn skill_sync_registry_policy_variants_are_distinguishable() {
    let approval = SkillSyncOutcome::RegistryNeedsApproval("acme.com".into());
    let denied = SkillSyncOutcome::RegistryDenied("acme.com".into());
    assert_ne!(approval, denied);
    assert!(matches!(
        approval,
        SkillSyncOutcome::RegistryNeedsApproval(host) if host == "acme.com"
    ));
    assert!(matches!(
        denied,
        SkillSyncOutcome::RegistryDenied(host) if host == "acme.com"
    ));
}

#[test]
fn skill_activation_error_variants_are_distinguishable() {
    let mut group = FakeSkillGroup::new();
    group.activation_result = Err(SkillActivationError::NotFound {
        requested: "missing".into(),
        available: vec!["demo".into()],
        warnings: vec![],
    });
    let not_found = group.activate_skill("missing").unwrap_err();
    match &not_found {
        SkillActivationError::NotFound {
            requested,
            available,
            ..
        } => {
            assert_eq!(requested, "missing");
            assert_eq!(available, &vec!["demo".to_string()]);
        }
        _ => panic!("expected NotFound"),
    }

    let mut group = FakeSkillGroup::new();
    group.activation_result = Err(SkillActivationError::PluginRejected {
        name: "plug".into(),
        reason: "authority revoked".into(),
    });
    let rejected = group.activate_skill("plug").unwrap_err();
    match rejected {
        SkillActivationError::PluginRejected { name, reason } => {
            assert_eq!(name, "plug");
            assert_eq!(reason, "authority revoked");
        }
        _ => panic!("expected PluginRejected"),
    }
}

#[test]
fn review_outcome_variants_are_distinguishable() {
    let mut group = FakeSkillGroup::new();
    group.review = Ok(ReviewOutcome::NotFound {
        skills_dir: "/ws/skills".into(),
        global_dir: "/home/u/.codewhale/skills".into(),
        warnings: vec!["w".into()],
    });
    let outcome = group.run_review().unwrap();
    match outcome {
        ReviewOutcome::NotFound {
            skills_dir,
            global_dir,
            warnings,
        } => {
            assert_eq!(skills_dir, "/ws/skills");
            assert_eq!(global_dir, "/home/u/.codewhale/skills");
            assert_eq!(warnings, vec!["w".to_string()]);
        }
        _ => panic!("expected NotFound"),
    }
}

#[test]
fn snapshot_and_approval_values_preserve_semantics() {
    let mut group = FakeSkillGroup::new();
    let snapshots = group.snapshot_list(20).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, "abcdef123456");
    assert_eq!(snapshots[0].label, "pre-turn:1");
    assert_eq!(snapshots[0].timestamp, 1_700_000_000);

    let approval = group.approval_state();
    assert!(approval.yolo);
    assert!(!approval.trust_mode);
}

#[test]
fn skill_group_facet_transports_through_envelope_when_declared() {
    let mut group = FakeSkillGroup::new();
    let parts = CommandContexts::empty()
        .with_skill_group(&mut group)
        .into_parts();
    assert!(parts.skill_group.is_some());
    assert!(parts.session.is_none());
    assert!(parts.project.is_none());

    // /skill combines skill_group with SKILLS for baseline cache refreshes.
    let mut skills = Skills;
    let parts = CommandContexts::empty()
        .with_skill_group(&mut group)
        .with_skills(&mut skills)
        .into_parts();
    assert!(parts.skill_group.is_some());
    assert!(parts.skills.is_some());
    assert!(parts.workspace.is_none());
}

#[test]
fn envelope_rejects_duplicate_skill_group_slot_deterministically() {
    let mut a = FakeSkillGroup::new();
    let mut b = FakeSkillGroup::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_skill_group(&mut a)
            .with_skill_group(&mut b);
    }));
    assert!(result.is_err(), "duplicate skill_group slot must assert");
}

/// Regression: the shared FEAT-015 `CommandSkillsContext` surface is unchanged
/// (getters + cache refresh only, no setter) and still transports through the
/// envelope alongside the new skill-group facet (D2).
#[test]
fn shared_skills_facet_surface_remains_read_only_and_transportable() {
    let mut skills = Skills;
    let active = skills.active_skill();
    assert_eq!(active, None);
    assert_eq!(skills.active_skill_provenance(), None);
    skills.refresh_skill_cache();

    let mut group = FakeSkillGroup::new();
    let parts = CommandContexts::empty()
        .with_skills(&mut skills)
        .with_skill_group(&mut group)
        .into_parts();
    assert!(parts.skills.is_some());
    assert!(parts.skill_group.is_some());
}

// ---------------------------------------------------------------------------
// FEAT-023: session lifecycle contract (D2/D3/D6).
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_capability_is_stable_distinct_and_non_conflicting() {
    let lifecycle = CommandCapabilities::SESSION_LIFECYCLE;
    for existing in [
        CommandCapabilities::NONE,
        CommandCapabilities::SESSION,
        CommandCapabilities::MODEL,
        CommandCapabilities::COST,
        CommandCapabilities::MODE_POLICY,
        CommandCapabilities::SYSTEM_PROMPT,
        CommandCapabilities::SKILLS,
        CommandCapabilities::WORKSPACE,
        CommandCapabilities::PRESENTATION,
        CommandCapabilities::MEDIA,
        CommandCapabilities::MEMORY,
        CommandCapabilities::PROJECT,
        CommandCapabilities::SKILL_GROUP,
        CommandCapabilities::PLUGIN,
    ] {
        assert_ne!(lifecycle, existing, "SESSION_LIFECYCLE must not collide");
    }
    assert!(!CommandCapabilities::NONE.contains(lifecycle));
    assert!(lifecycle.contains(lifecycle));
    assert!(
        lifecycle
            .union(CommandCapabilities::SESSION)
            .contains(lifecycle)
    );
    assert!(
        lifecycle
            .union(CommandCapabilities::SESSION)
            .contains(CommandCapabilities::SESSION)
    );
}

/// Deterministic fake lifecycle facet: every delegate returns canned portable
/// values or error text so the contract transport is exercised exactly.
#[derive(Default)]
struct FakeLifecycle {
    blocked: bool,
    leaf_hint: Option<String>,
    branch_outcome: Option<SessionBranchOutcome>,
    branch_error: Option<String>,
    tree: Option<Result<TreeBodyProjection, String>>,
    save: Option<Result<SessionSaveReceipt, String>>,
    fork_active: Option<Result<SessionForkReceipt, String>>,
    fork_from: Option<Result<SessionForkFromReceipt, String>>,
    fresh: Option<Result<SessionNewReceipt, String>>,
    load: Option<Result<PathBuf, String>>,
    picker: Option<String>,
    archived: Option<Result<SessionArchiveReceipt, String>>,
    prune: Option<Result<usize, String>>,
}

impl CommandSessionLifecycleContext for FakeLifecycle {
    fn transition_blocked(&self) -> bool {
        self.blocked
    }
    fn transition_blockers(&self) -> Vec<String> {
        Vec::new()
    }
    fn branch_current_leaf_hint(&self) -> Option<String> {
        self.leaf_hint.clone()
    }
    fn branch_to(&mut self, entry_id: &str) -> Result<SessionBranchOutcome, String> {
        if let Some(err) = &self.branch_error {
            return Err(err.clone());
        }
        self.branch_outcome
            .clone()
            .ok_or_else(|| format!("unexpected branch_to({entry_id}) on empty fake"))
    }
    fn tree_body(&self) -> Result<TreeBodyProjection, String> {
        self.tree
            .clone()
            .unwrap_or(Ok(TreeBodyProjection::NoSession))
    }
    fn save_session(
        &mut self,
        explicit_path: Option<String>,
    ) -> Result<SessionSaveReceipt, String> {
        self.save
            .clone()
            .ok_or_else(|| format!("unexpected save_session({explicit_path:?}) on empty fake"))?
    }
    fn fork_active(&mut self) -> Result<SessionForkReceipt, String> {
        self.fork_active
            .clone()
            .ok_or_else(|| "unexpected fork_active() on empty fake".to_string())?
    }
    fn fork_from(&mut self, id: &str) -> Result<SessionForkFromReceipt, String> {
        self.fork_from
            .clone()
            .ok_or_else(|| format!("unexpected fork_from({id}) on empty fake"))?
    }
    fn fresh_session(&mut self, force: bool) -> Result<SessionNewReceipt, String> {
        self.fresh
            .clone()
            .ok_or_else(|| format!("unexpected fresh_session({force}) on empty fake"))?
    }
    fn load_session(&mut self, path: &str) -> Result<PathBuf, String> {
        self.load
            .clone()
            .ok_or_else(|| format!("unexpected load_session({path}) on empty fake"))?
    }
    fn open_picker(&mut self, preselected: Option<String>) {
        self.picker = preselected;
    }
    fn set_archived(
        &mut self,
        session_id: &str,
        archived: bool,
    ) -> Result<SessionArchiveReceipt, String> {
        self.archived.clone().ok_or_else(|| {
            format!("unexpected set_archived({session_id}, {archived}) on empty fake")
        })?
    }
    fn prune_sessions(&mut self, days: u64) -> Result<usize, String> {
        self.prune
            .clone()
            .ok_or_else(|| format!("unexpected prune_sessions({days}) on empty fake"))?
    }
}

fn lifecycle_sync_payload(session_id: Option<&str>) -> SessionSyncPayload {
    SessionSyncPayload {
        session_id: session_id.map(str::to_string),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello lifecycle".to_string(),
                cache_control: None,
            }],
        }],
        system_prompt: Some(SystemPrompt::Text("prompt".to_string())),
        model: "lifecycle-model".to_string(),
        workspace: PathBuf::from("/workspace/lifecycle"),
        mode: CommandMode::Plan,
    }
}

#[test]
fn lifecycle_facet_is_object_safe_and_transports_every_outcome() {
    // Object safety: usable behind a single `dyn` reference.
    fn accepts_dyn(_: &dyn CommandSessionLifecycleContext) {}
    fn accepts_dyn_mut(_: &mut dyn CommandSessionLifecycleContext) {}

    let mut fake = FakeLifecycle {
        blocked: true,
        leaf_hint: Some("entry-42".to_string()),
        branch_outcome: Some(SessionBranchOutcome {
            leaf_display: "entry-43".to_string(),
            journal_entries_before: 7,
            sync: lifecycle_sync_payload(Some("branched-session")),
        }),
        tree: Some(Ok(TreeBodyProjection::Journal {
            rendered: "rendered journal".to_string(),
        })),
        save: Some(Ok(SessionSaveReceipt {
            display_path: "/tmp/session.json".to_string(),
            truncated_id: "abc123".to_string(),
        })),
        fork_active: Some(Ok(SessionForkReceipt {
            parent_label: "parent".to_string(),
            fork_label: "child".to_string(),
            sync: lifecycle_sync_payload(Some("child")),
        })),
        fork_from: Some(Ok(SessionForkFromReceipt {
            parent_label: "source".to_string(),
            fork_label: "sibling".to_string(),
            spawn_depth: 3,
            sync: lifecycle_sync_payload(Some("sibling")),
        })),
        fresh: Some(Ok(SessionNewReceipt {
            truncated_id: "new-id".to_string(),
            sync: lifecycle_sync_payload(Some("new-id")),
        })),
        load: Some(Ok(PathBuf::from("/tmp/loaded.json"))),
        archived: Some(Ok(SessionArchiveReceipt {
            truncated_id: "arch-1".to_string(),
            title: "Archive Title".to_string(),
        })),
        prune: Some(Ok(3)),
        ..FakeLifecycle::default()
    };
    accepts_dyn(&fake);
    accepts_dyn_mut(&mut fake);

    assert!(fake.transition_blocked());
    assert_eq!(fake.branch_current_leaf_hint().as_deref(), Some("entry-42"));
    let branch = fake.branch_to("entry-43").expect("branch ok");
    assert_eq!(branch.leaf_display, "entry-43");
    assert_eq!(branch.journal_entries_before, 7);
    assert_eq!(branch.sync.session_id.as_deref(), Some("branched-session"));
    assert_eq!(branch.sync.messages.len(), 1);
    assert_eq!(branch.sync.mode, CommandMode::Plan);
    match fake.tree_body().expect("tree ok") {
        TreeBodyProjection::Journal { rendered } => assert_eq!(rendered, "rendered journal"),
        other => panic!("expected Journal projection, got {other:?}"),
    }
    let save = fake
        .save_session(Some("/tmp/session.json".to_string()))
        .expect("save ok");
    assert_eq!(save.display_path, "/tmp/session.json");
    assert_eq!(save.truncated_id, "abc123");
    let active = fake.fork_active().expect("active fork ok");
    assert_eq!(active.parent_label, "parent");
    assert_eq!(active.fork_label, "child");
    assert_eq!(active.sync.session_id.as_deref(), Some("child"));
    assert_eq!(active.sync.messages.len(), 1);
    assert_eq!(active.sync.mode, CommandMode::Plan);
    let explicit = fake.fork_from("source").expect("explicit fork ok");
    assert_eq!(explicit.spawn_depth, 3);
    assert_eq!(
        explicit.sync.workspace,
        PathBuf::from("/workspace/lifecycle")
    );
    let fresh = fake.fresh_session(true).expect("fresh ok");
    assert_eq!(fresh.truncated_id, "new-id");
    assert_eq!(fresh.sync.messages.len(), 1);
    let loaded = fake.load_session("loaded.json").expect("load ok");
    assert_eq!(loaded, PathBuf::from("/tmp/loaded.json"));
    fake.open_picker(Some("arch-1".to_string()));
    assert_eq!(fake.picker.as_deref(), Some("arch-1"));
    let archived = fake.set_archived("arch-1", true).expect("archive ok");
    assert_eq!(archived.truncated_id, "arch-1");
    assert_eq!(archived.title, "Archive Title");
    assert_eq!(fake.prune_sessions(30).expect("prune ok"), 3);
}

#[test]
fn lifecycle_error_text_and_empty_states_transport_exactly() {
    let mut fake = FakeLifecycle {
        branch_error: Some("could not load session x: boom".to_string()),
        tree: Some(Err("could not open sessions directory: boom".to_string())),
        save: Some(Err("Failed to save session: boom".to_string())),
        load: Some(Err("Failed to read session file: boom".to_string())),
        archived: Some(Err("archive failed: boom".to_string())),
        prune: Some(Err("prune failed: boom".to_string())),
        ..FakeLifecycle::default()
    };
    assert_eq!(
        fake.branch_to("x").unwrap_err(),
        "could not load session x: boom"
    );
    assert_eq!(
        fake.tree_body().unwrap_err(),
        "could not open sessions directory: boom"
    );
    assert_eq!(
        fake.save_session(None).unwrap_err(),
        "Failed to save session: boom"
    );
    assert_eq!(
        fake.load_session("missing.json").unwrap_err(),
        "Failed to read session file: boom"
    );
    assert_eq!(
        fake.set_archived("a", false).unwrap_err(),
        "archive failed: boom"
    );
    assert_eq!(fake.prune_sessions(7).unwrap_err(), "prune failed: boom");

    let mut empty = FakeLifecycle::default();
    assert!(!empty.transition_blocked());
    assert_eq!(empty.branch_current_leaf_hint(), None);
    assert!(matches!(
        empty.tree_body().expect("default tree"),
        TreeBodyProjection::NoSession
    ));
    empty.open_picker(None);
    assert_eq!(empty.picker, None);
}

#[test]
fn envelope_lifecycle_slot_is_independent_and_rejects_duplicates() {
    let mut first = FakeLifecycle::default();
    let mut second = FakeLifecycle::default();

    let parts = CommandContexts::empty()
        .with_lifecycle(&mut first)
        .into_parts();
    assert!(
        parts.lifecycle.is_some(),
        "lifecycle slot must be present when declared"
    );
    assert!(
        parts.session.is_none() && parts.plugin.is_none() && parts.skill_group.is_none(),
        "unrelated slots must stay absent (exact exposure)"
    );

    let bare = CommandContexts::empty().into_parts();
    assert!(
        bare.lifecycle.is_none(),
        "undeclared lifecycle stays absent"
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_lifecycle(&mut first)
            .with_lifecycle(&mut second);
    }));
    assert!(
        result.is_err(),
        "duplicate lifecycle slot must assert deterministically"
    );

    // Reading through the dyn facet works after insertion.
    first.blocked = true;
    let inserted = CommandContexts::empty().with_lifecycle(&mut first);
    let lifecycle = inserted.into_parts().lifecycle.expect("inserted lifecycle");
    assert!(lifecycle.transition_blocked());
}

// ---------------------------------------------------------------------------
// FEAT-024: session control contract (D2/D3/D6/D7).
// ---------------------------------------------------------------------------

#[test]
fn control_capability_is_stable_distinct_and_non_conflicting() {
    let control = CommandCapabilities::SESSION_CONTROL;
    for existing in [
        CommandCapabilities::NONE,
        CommandCapabilities::SESSION,
        CommandCapabilities::MODEL,
        CommandCapabilities::COST,
        CommandCapabilities::MODE_POLICY,
        CommandCapabilities::SYSTEM_PROMPT,
        CommandCapabilities::SKILLS,
        CommandCapabilities::WORKSPACE,
        CommandCapabilities::PRESENTATION,
        CommandCapabilities::MEDIA,
        CommandCapabilities::MEMORY,
        CommandCapabilities::PROJECT,
        CommandCapabilities::SKILL_GROUP,
        CommandCapabilities::PLUGIN,
        CommandCapabilities::SESSION_LIFECYCLE,
    ] {
        assert_ne!(control, existing, "SESSION_CONTROL must not collide");
    }
    assert!(!CommandCapabilities::NONE.contains(control));
    assert!(control.contains(control));
    assert!(
        control
            .union(CommandCapabilities::PRESENTATION)
            .contains(control)
    );
    assert!(
        control
            .union(CommandCapabilities::PRESENTATION)
            .contains(CommandCapabilities::PRESENTATION)
    );
    assert!(!CommandCapabilities::SESSION_LIFECYCLE.contains(control));
    assert!(!control.contains(CommandCapabilities::SESSION_LIFECYCLE));
    // Storage remains u16-backed by construction: bit 14 (1 << 14 = 16384)
    // fits the backing `u16` without the speculative widening FEAT-023's
    // maintainer review ruled out.
}

/// Deterministic fake control facet: every delegate returns canned portable
/// values or error text so the contract transport is exercised exactly.
#[derive(Default)]
struct FakeControl {
    blocked: bool,
    relay: Option<RelayProjection>,
    resume: Option<Result<ResumeSource, String>>,
    import: Option<Result<ResumeImportReceipt, String>>,
    sanitized_title: Option<String>,
    rename: Option<Result<SessionTitleReceipt, String>>,
    title_report: Option<TitleReport>,
    set_title: Option<Result<(), String>>,
    clear_title: Option<Result<(), String>>,
    remote_status: Option<String>,
    remote_link: Option<Option<RemoteLink>>,
    browser_open: Option<RemoteOpenOutcome>,
    start_info: Option<RemoteStartInfo>,
    stop_refusal: Option<Option<String>>,
    hosted: Option<Option<HostedWorkTarget>>,
}

impl CommandSessionControlContext for FakeControl {
    fn transition_blocked(&self) -> bool {
        self.blocked
    }
    fn transition_blockers(&self) -> Vec<String> {
        Vec::new()
    }
    fn relay_projection(&self) -> RelayProjection {
        self.relay
            .clone()
            .expect("unexpected relay_projection() on empty fake")
    }
    fn open_resume_picker(&mut self) {}
    fn resolve_resume_source(&mut self, raw: &str) -> Result<ResumeSource, String> {
        self.resume.clone().unwrap_or_else(|| {
            Err(format!(
                "unexpected resolve_resume_source({raw}) on empty fake"
            ))
        })
    }
    fn import_session_file(&mut self, path: PathBuf) -> Result<ResumeImportReceipt, String> {
        self.import.clone().unwrap_or_else(|| {
            Err(format!(
                "unexpected import_session_file({path:?}) on empty fake"
            ))
        })
    }
    fn sanitize_session_title(&self, raw: &str) -> String {
        self.sanitized_title
            .clone()
            .unwrap_or_else(|| raw.to_string())
    }
    fn rename_session(&mut self, title: &str) -> Result<SessionTitleReceipt, String> {
        self.rename
            .clone()
            .unwrap_or_else(|| Err(format!("unexpected rename_session({title}) on empty fake")))
    }
    fn title_report(&self) -> TitleReport {
        self.title_report
            .clone()
            .expect("unexpected title_report() on empty fake")
    }
    fn set_window_title(&mut self, title: String) -> Result<(), String> {
        self.set_title.clone().unwrap_or_else(|| {
            Err(format!(
                "unexpected set_window_title({title}) on empty fake"
            ))
        })
    }
    fn clear_window_title(&mut self) -> Result<(), String> {
        self.clear_title
            .clone()
            .unwrap_or_else(|| Err("unexpected clear_window_title() on empty fake".to_string()))
    }
    fn remote_status(&self) -> String {
        self.remote_status
            .clone()
            .expect("unexpected remote_status() on empty fake")
    }
    fn remote_link(&self) -> Option<RemoteLink> {
        self.remote_link
            .clone()
            .expect("unexpected remote_link() on empty fake")
    }
    fn remote_browser_open(&self) -> RemoteOpenOutcome {
        self.browser_open
            .clone()
            .expect("unexpected remote_browser_open() on empty fake")
    }
    fn remote_start_info(&self) -> RemoteStartInfo {
        self.start_info
            .clone()
            .expect("unexpected remote_start_info() on empty fake")
    }
    fn remote_stop_refusal(&self) -> Option<String> {
        self.stop_refusal
            .clone()
            .expect("unexpected remote_stop_refusal() on empty fake")
    }
    fn resolve_hosted_work_target(&self) -> Option<HostedWorkTarget> {
        self.hosted
            .clone()
            .expect("unexpected resolve_hosted_work_target() on empty fake")
    }
}

fn control_relay_projection() -> RelayProjection {
    RelayProjection {
        compact_template: "# Session relay".to_string(),
        handoff_path: ".codewhale/handoff.md".to_string(),
        workspace: "/workspace/control".to_string(),
        mode: "operate".to_string(),
        model: "control-model".to_string(),
        goal_objective: Some("ship the slice".to_string()),
        goal_token_budget: Some(42_000),
        todos: TodoProjection::Body("- [ ] port relay".to_string()),
        plan: PlanProjection::Sections(PlanSections {
            title: Some("Plan title".to_string()),
            items: vec![PlanStep {
                status: PlanStepStatus::InProgress,
                text: "port the control slice".to_string(),
            }],
            ..PlanSections::default()
        }),
    }
}

#[test]
fn control_facet_is_object_safe_and_transports_every_outcome() {
    // Object safety: usable behind a single `dyn` reference.
    fn accepts_dyn(_: &dyn CommandSessionControlContext) {}
    fn accepts_dyn_mut(_: &mut dyn CommandSessionControlContext) {}

    let mut fake = FakeControl {
        blocked: true,
        relay: Some(control_relay_projection()),
        resume: Some(Ok(ResumeSource::Session {
            load_path: Some(PathBuf::from("/tmp/sessions/abc123.json")),
            truncated_id: "abc123".to_string(),
            title: "Control Session".to_string(),
        })),
        import: Some(Ok(ResumeImportReceipt {
            truncated_id: "imp-9".to_string(),
            entry_count: 12,
            leaf_display: "leaf-3".to_string(),
            sync: lifecycle_sync_payload(Some("imp-9")),
        })),
        sanitized_title: Some("Renamed".to_string()),
        rename: Some(Ok(SessionTitleReceipt {
            title: "Renamed".to_string(),
        })),
        title_report: Some(TitleReport {
            effective: "task-7".to_string(),
            source: TitleSource::Session,
        }),
        set_title: Some(Ok(())),
        clear_title: Some(Ok(())),
        remote_status: Some("live".to_string()),
        remote_link: Some(Some(RemoteLink {
            url: "https://remote.example/s".to_string(),
            computer_url: Some("https://remote.example/c".to_string()),
        })),
        browser_open: Some(RemoteOpenOutcome::Opened {
            url: "https://remote.example/s".to_string(),
        }),
        start_info: Some(RemoteStartInfo { connecting: true }),
        stop_refusal: Some(None),
        hosted: Some(Some(HostedWorkTarget {
            url: "https://app.codewhale.net/work?repo=A%2FB".to_string(),
            repo: "A/B".to_string(),
            branch: "main".to_string(),
        })),
    };
    accepts_dyn(&fake);
    accepts_dyn_mut(&mut fake);

    assert!(fake.transition_blocked());
    let relay = fake.relay_projection();
    assert_eq!(relay.model, "control-model");
    assert_eq!(relay.goal_token_budget, Some(42_000));
    assert!(matches!(relay.todos, TodoProjection::Body(_)));
    match relay.plan {
        PlanProjection::Sections(sections) => {
            assert_eq!(sections.title.as_deref(), Some("Plan title"));
            assert_eq!(sections.items.len(), 1);
            assert_eq!(sections.items[0].status, PlanStepStatus::InProgress);
        }
        other => panic!("expected Sections plan, got {other:?}"),
    }
    let resolved = fake
        .resolve_resume_source("abc123")
        .expect("resume resolution ok");
    match resolved {
        ResumeSource::Session {
            load_path, title, ..
        } => {
            assert_eq!(load_path, Some(PathBuf::from("/tmp/sessions/abc123.json")));
            assert_eq!(title, "Control Session");
        }
        other => panic!("expected Session resolution, got {other:?}"),
    }
    let imported = fake
        .import_session_file(PathBuf::from("/tmp/import.json"))
        .expect("import ok");
    assert_eq!(imported.truncated_id, "imp-9");
    assert_eq!(imported.entry_count, 12);
    assert_eq!(imported.leaf_display, "leaf-3");
    assert_eq!(fake.sanitize_session_title("raw"), "Renamed");
    let renamed = fake.rename_session("Renamed").expect("rename ok");
    assert_eq!(renamed.title, "Renamed");
    let report = fake.title_report();
    assert_eq!(report.effective, "task-7");
    assert!(matches!(report.source, TitleSource::Session));
    fake.set_window_title("task-7".to_string()).expect("set ok");
    fake.clear_window_title().expect("clear ok");
    assert_eq!(fake.remote_status(), "live");
    let link = fake.remote_link().expect("link present");
    assert_eq!(link.url, "https://remote.example/s");
    assert!(matches!(
        fake.remote_browser_open(),
        RemoteOpenOutcome::Opened { .. }
    ));
    assert!(fake.remote_start_info().connecting);
    assert_eq!(fake.remote_stop_refusal(), None);
    let hosted = fake.resolve_hosted_work_target().expect("target present");
    assert_eq!(hosted.repo, "A/B");
    assert_eq!(hosted.branch, "main");
}

#[test]
fn control_error_and_empty_states_transport_exactly() {
    let mut fake = FakeControl {
        blocked: false,
        resume: Some(Err("could not open sessions directory: boom".to_string())),
        import: Some(Err(
            "File x.json is not a recognized session export".to_string()
        )),
        rename: Some(Err("Could not save session: boom".to_string())),
        set_title: Some(Err("Could not save session: boom".to_string())),
        clear_title: Some(Err("Could not save session: boom".to_string())),
        remote_link: Some(None),
        browser_open: Some(RemoteOpenOutcome::NoLink),
        stop_refusal: Some(Some(
            "stop refused while a remote turn is active".to_string(),
        )),
        hosted: Some(None),
        ..FakeControl::default()
    };
    assert!(!fake.transition_blocked());
    assert_eq!(
        fake.resolve_resume_source("x").unwrap_err(),
        "could not open sessions directory: boom"
    );
    assert_eq!(
        fake.import_session_file(PathBuf::from("x.json"))
            .unwrap_err(),
        "File x.json is not a recognized session export"
    );
    assert_eq!(
        fake.rename_session("t").unwrap_err(),
        "Could not save session: boom"
    );
    assert_eq!(
        fake.set_window_title("task".to_string()).unwrap_err(),
        "Could not save session: boom"
    );
    assert_eq!(
        fake.clear_window_title().unwrap_err(),
        "Could not save session: boom"
    );
    assert_eq!(fake.remote_link(), None);
    assert!(matches!(
        fake.remote_browser_open(),
        RemoteOpenOutcome::NoLink
    ));
    assert_eq!(
        fake.remote_stop_refusal().as_deref(),
        Some("stop refused while a remote turn is active")
    );
    assert_eq!(fake.resolve_hosted_work_target(), None);

    // Empty-state variants: absent to-do/plan and no effective title transport.
    fake.relay = Some(RelayProjection {
        todos: TodoProjection::Absent,
        plan: PlanProjection::Absent,
        ..control_relay_projection()
    });
    let relay = fake.relay_projection();
    assert!(matches!(relay.todos, TodoProjection::Absent));
    assert!(matches!(relay.plan, PlanProjection::Absent));
    fake.title_report = Some(TitleReport {
        effective: "unset".to_string(),
        source: TitleSource::None,
    });
    assert!(matches!(fake.title_report().source, TitleSource::None));
    fake.clear_title = Some(Ok(()));
    fake.clear_window_title().expect("cleared");
}

#[test]
fn envelope_control_slot_is_independent_and_rejects_duplicates() {
    let mut first = FakeControl::default();
    let mut second = FakeControl::default();
    let mut lifecycle = FakeLifecycle::default();

    let parts = CommandContexts::empty()
        .with_control(&mut first)
        .with_lifecycle(&mut lifecycle)
        .into_parts();
    assert!(
        parts.control.is_some(),
        "control slot must be present when declared"
    );
    assert!(
        parts.lifecycle.is_some(),
        "lifecycle slot may coexist with control"
    );
    assert!(
        parts.session.is_none()
            && parts.plugin.is_none()
            && parts.skill_group.is_none()
            && parts.presentation.is_none(),
        "unrelated slots must stay absent (exact exposure)"
    );

    let bare = CommandContexts::empty().into_parts();
    assert!(bare.control.is_none(), "undeclared control stays absent");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_control(&mut first)
            .with_control(&mut second);
    }));
    assert!(
        result.is_err(),
        "duplicate control slot must assert deterministically"
    );

    // Reading through the dyn facet works after insertion.
    first.blocked = true;
    let inserted = CommandContexts::empty().with_control(&mut first);
    let control = inserted.into_parts().control.expect("inserted control");
    assert!(control.transition_blocked());
}

#[test]
fn control_surface_does_not_widen_session_or_lifecycle_facets() {
    // The basic session and lifecycle facets still expose exactly their own
    // method surface alongside the new control slot: all three may populate an
    // envelope at once without colliding, and control does not add behavior to
    // the existing facets.
    let mut session = Session;
    let mut lifecycle = FakeLifecycle::default();
    let mut control = FakeControl {
        blocked: true,
        ..FakeControl::default()
    };

    let mut parts = CommandContexts::empty()
        .with_session(&mut session)
        .with_lifecycle(&mut lifecycle)
        .with_control(&mut control)
        .into_parts();
    assert_eq!(
        parts.session.as_deref().unwrap().session_id().as_deref(),
        Some("session")
    );
    assert!(!parts.lifecycle.as_deref_mut().unwrap().transition_blocked());
    assert!(parts.control.as_deref_mut().unwrap().transition_blocked());
}

// ---------------------------------------------------------------------------
// FEAT-025: session export contract (D1/D3/D5/D6/D7/D8/D9).
// ---------------------------------------------------------------------------

#[test]
fn export_capability_is_stable_distinct_and_non_conflicting() {
    let export = CommandCapabilities::SESSION_EXPORT;
    let existing = [
        CommandCapabilities::SESSION,
        CommandCapabilities::MODEL,
        CommandCapabilities::COST,
        CommandCapabilities::MODE_POLICY,
        CommandCapabilities::SYSTEM_PROMPT,
        CommandCapabilities::SKILLS,
        CommandCapabilities::WORKSPACE,
        CommandCapabilities::PRESENTATION,
        CommandCapabilities::MEDIA,
        CommandCapabilities::MEMORY,
        CommandCapabilities::PROJECT,
        CommandCapabilities::SKILL_GROUP,
        CommandCapabilities::PLUGIN,
        CommandCapabilities::SESSION_LIFECYCLE,
        CommandCapabilities::SESSION_CONTROL,
    ];
    let mut union = CommandCapabilities::NONE;
    for capability in existing {
        assert_ne!(
            export, capability,
            "SESSION_EXPORT must not collide with an existing capability"
        );
        union = union.union(capability);
    }
    assert!(
        !union.contains(export),
        "SESSION_EXPORT must be a bit outside every existing capability (bits 0-14)"
    );
    assert!(!CommandCapabilities::NONE.contains(export));
    assert!(!CommandCapabilities::NONE.contains(CommandCapabilities::NONE));
    assert!(export.contains(export));
    assert!(
        export
            .union(CommandCapabilities::SESSION_CONTROL)
            .contains(export)
    );
    assert!(
        export
            .union(CommandCapabilities::SESSION_CONTROL)
            .contains(CommandCapabilities::SESSION_CONTROL)
    );
    assert!(!CommandCapabilities::SESSION_CONTROL.contains(export));
    assert!(!export.contains(CommandCapabilities::SESSION_CONTROL));
    // FEAT-029 widened storage after bit 15 filled the original space; export
    // retains its exact published identity.
    assert_eq!(
        std::mem::size_of::<CommandCapabilities>(),
        std::mem::size_of::<u32>(),
        "CommandCapabilities storage must be widened for diagnostics"
    );
}

/// Canary: FEAT-029 widened the previously full 16-bit capability space.
///
/// The first sixteen identities stay published as before; diagnostics takes
/// bit 16 and later slices can allocate independently without renumbering.
#[test]
fn debug_diagnostics_capability_preserves_published_bits() {
    let all = [
        CommandCapabilities::SESSION,
        CommandCapabilities::MODEL,
        CommandCapabilities::COST,
        CommandCapabilities::MODE_POLICY,
        CommandCapabilities::SYSTEM_PROMPT,
        CommandCapabilities::SKILLS,
        CommandCapabilities::WORKSPACE,
        CommandCapabilities::PRESENTATION,
        CommandCapabilities::MEDIA,
        CommandCapabilities::MEMORY,
        CommandCapabilities::PROJECT,
        CommandCapabilities::SKILL_GROUP,
        CommandCapabilities::PLUGIN,
        CommandCapabilities::SESSION_LIFECYCLE,
        CommandCapabilities::SESSION_CONTROL,
        CommandCapabilities::SESSION_EXPORT,
        CommandCapabilities::DEBUG_DIAGNOSTICS,
    ];

    let mut union = CommandCapabilities::NONE;
    for (index, capability) in all.iter().enumerate() {
        assert_eq!(
            capability.bits_for_test(),
            1u32 << index,
            "capability {index} must occupy exactly bit {index}"
        );
        union = union.union(*capability);
    }

    assert_eq!(
        all.len(),
        u16::BITS as usize + 1,
        "diagnostics is the first bit after the original u16 space"
    );
    assert_eq!(
        union.bits_for_test(),
        u32::from(u16::MAX) | (1u32 << 16),
        "bits 0-15 retain their published values and diagnostics occupies bit 16"
    );
    assert!(union.contains(CommandCapabilities::DEBUG_DIAGNOSTICS));
    assert!(!CommandCapabilities::SESSION_EXPORT.contains(CommandCapabilities::DEBUG_DIAGNOSTICS));
    assert!(!CommandCapabilities::DEBUG_DIAGNOSTICS.contains(CommandCapabilities::SESSION_EXPORT));
    assert!(CommandCapabilities::NONE.is_empty());
    assert!(!CommandCapabilities::NONE.contains(CommandCapabilities::DEBUG_DIAGNOSTICS));
    assert!(!CommandCapabilities::DEBUG_DIAGNOSTICS.contains(CommandCapabilities::NONE));
}

/// Deterministic fake export facet: every delegate returns canned portable
/// values or host error text, and effectful delegates record their calls so a
/// later phase can assert sequencing without a real host.
#[derive(Default)]
struct FakeExport {
    projection: Option<ConversationExportProjection>,
    turn: Option<TurnHandoffProjection>,
    terminal_paste: bool,
    recovery: Option<Option<PathBuf>>,
    clipboard: Option<Result<(), String>>,
    resolved: Option<Result<PathBuf, String>>,
    write: Option<Result<(), String>>,
    calls: RefCell<Vec<String>>,
}

impl CommandSessionExportContext for FakeExport {
    fn conversation_projection(&self) -> ConversationExportProjection {
        self.calls
            .borrow_mut()
            .push("conversation_projection".to_string());
        self.projection
            .clone()
            .expect("unexpected conversation_projection() on empty fake")
    }
    fn turn_handoff_projection(&self) -> TurnHandoffProjection {
        self.calls
            .borrow_mut()
            .push("turn_handoff_projection".to_string());
        self.turn
            .clone()
            .expect("unexpected turn_handoff_projection() on empty fake")
    }
    fn clipboard_requires_terminal_paste(&self) -> bool {
        self.calls
            .borrow_mut()
            .push("clipboard_requires_terminal_paste".to_string());
        self.terminal_paste
    }
    fn write_recovery_copy(&self, markdown: &str) -> Option<PathBuf> {
        self.calls
            .borrow_mut()
            .push(format!("write_recovery_copy:{markdown}"));
        self.recovery
            .clone()
            .expect("unexpected write_recovery_copy() on empty fake")
    }
    fn write_clipboard(&self, markdown: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("write_clipboard:{markdown}"));
        self.clipboard
            .clone()
            .unwrap_or_else(|| Err("unexpected write_clipboard() on empty fake".to_string()))
    }
    fn resolve_export_path(&self, raw: &str) -> Result<PathBuf, String> {
        self.calls
            .borrow_mut()
            .push(format!("resolve_export_path:{raw}"));
        self.resolved.clone().unwrap_or_else(|| {
            Err(format!(
                "unexpected resolve_export_path({raw}) on empty fake"
            ))
        })
    }
    fn write_export_file(&self, path: &Path, contents: &[u8], force: bool) -> Result<(), String> {
        self.calls.borrow_mut().push(format!(
            "write_export_file:{}:{}:{force}",
            path.display(),
            contents.len()
        ));
        self.write
            .clone()
            .unwrap_or_else(|| Err("unexpected write_export_file() on empty fake".to_string()))
    }
}

fn export_metadata() -> ExportMetadata {
    ExportMetadata {
        session_label: "abc123".to_string(),
        provider: "deepseek".to_string(),
        model: "deepseek-chat".to_string(),
        mode: "ACT".to_string(),
        workspace_name: "workspace".to_string(),
        message_count: 2,
        exported_at_unix: 1_760_000_000,
    }
}

fn export_recorded_snapshot() -> RestoreSnapshot {
    RestoreSnapshot {
        id: "0123456789abcdef".to_string(),
        label: "pre-turn:3: fix parser".to_string(),
        timestamp_unix: 1_759_999_000,
        kind: "pre-turn".to_string(),
        sequence: Some(3),
        prompt_snippet: Some("fix parser".to_string()),
    }
}

#[test]
fn export_facet_is_object_safe_and_transports_every_outcome() {
    // Object safety: usable behind a single `dyn` reference.
    fn accepts_dyn(_: &dyn CommandSessionExportContext) {}
    fn accepts_dyn_mut(_: &mut dyn CommandSessionExportContext) {}

    let mut fake = FakeExport {
        projection: Some(ConversationExportProjection {
            metadata: export_metadata(),
            transcript: TranscriptProjection::Authoritative(vec![ExportMessage {
                is_user_role: false,
                role: "assistant".to_string(),
                prompt_snippet: Some("fix parser".to_string()),
                blocks: vec![
                    ExportBlock::Text {
                        text: "visible".to_string(),
                    },
                    ExportBlock::ImageReference {
                        url: "https://example.test/a.png".to_string(),
                    },
                    ExportBlock::ImageOmitted,
                    ExportBlock::InternalReasoning,
                    ExportBlock::ToolCall {
                        id: "tool-1".to_string(),
                        name: "read".to_string(),
                        caller: Some(ToolCallerProjection {
                            caller_type: "direct".to_string(),
                            tool_id: Some("caller-1".to_string()),
                        }),
                        input: serde_json::json!({"path": "a.txt"}),
                    },
                    ExportBlock::ToolResult {
                        tool_use_id: "tool-1".to_string(),
                        content: "ok".to_string(),
                        is_error: false,
                        structured: Some(serde_json::json!([{"type": "text", "text": "ok"}])),
                    },
                    ExportBlock::ServerToolCall {
                        id: "server-1".to_string(),
                        name: "web_search".to_string(),
                        input: serde_json::json!({"q": "rust"}),
                    },
                    ExportBlock::ToolSearchResult {
                        tool_use_id: "search-1".to_string(),
                        content: serde_json::json!({"results": []}),
                    },
                    ExportBlock::CodeExecutionResult {
                        tool_use_id: "code-1".to_string(),
                        content: serde_json::json!({"stdout": "hi"}),
                    },
                ],
            }]),
            restore_points: RestorePointProjection::Recorded {
                snapshots: vec![export_recorded_snapshot()],
            },
        }),
        turn: Some(TurnHandoffProjection {
            markdown: "# turn handoff".to_string(),
            workspace_path: "/workspace/example".to_string(),
        }),
        terminal_paste: true,
        recovery: Some(Some(PathBuf::from(
            "/home/u/.codewhale/exports/last-copy.md",
        ))),
        clipboard: Some(Ok(())),
        resolved: Some(Ok(PathBuf::from("/workspace/example/out.md"))),
        write: Some(Ok(())),
        ..FakeExport::default()
    };
    accepts_dyn(&fake);
    accepts_dyn_mut(&mut fake);

    let projection = fake.conversation_projection();
    assert_eq!(projection.metadata.session_label, "abc123");
    assert_eq!(projection.metadata.provider, "deepseek");
    assert_eq!(projection.metadata.model, "deepseek-chat");
    assert_eq!(projection.metadata.mode, "ACT");
    assert_eq!(projection.metadata.workspace_name, "workspace");
    assert_eq!(projection.metadata.message_count, 2);
    assert_eq!(projection.metadata.exported_at_unix, 1_760_000_000);
    let TranscriptProjection::Authoritative(messages) = projection.transcript else {
        panic!("expected authoritative transcript");
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "assistant");
    assert_eq!(messages[0].prompt_snippet.as_deref(), Some("fix parser"));
    assert_eq!(messages[0].blocks.len(), 9);
    let ExportBlock::ToolCall {
        caller: Some(caller),
        input,
        ..
    } = &messages[0].blocks[4]
    else {
        panic!("expected tool call with caller");
    };
    assert_eq!(caller.caller_type, "direct");
    assert_eq!(caller.tool_id.as_deref(), Some("caller-1"));
    assert_eq!(input["path"], "a.txt");
    let ExportBlock::ToolResult {
        is_error,
        structured,
        ..
    } = &messages[0].blocks[5]
    else {
        panic!("expected tool result");
    };
    assert!(!is_error);
    assert!(structured.is_some());
    let RestorePointProjection::Recorded { snapshots } = projection.restore_points else {
        panic!("expected recorded restore points");
    };
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].id, "0123456789abcdef");
    assert_eq!(snapshots[0].label, "pre-turn:3: fix parser");
    assert_eq!(snapshots[0].timestamp_unix, 1_759_999_000);
    assert_eq!(snapshots[0].kind, "pre-turn");
    assert_eq!(snapshots[0].sequence, Some(3));
    assert_eq!(snapshots[0].prompt_snippet.as_deref(), Some("fix parser"));

    let turn = fake.turn_handoff_projection();
    assert_eq!(turn.markdown, "# turn handoff");
    assert_eq!(turn.workspace_path, "/workspace/example");

    assert!(fake.clipboard_requires_terminal_paste());
    assert_eq!(
        fake.write_recovery_copy("# md"),
        Some(PathBuf::from("/home/u/.codewhale/exports/last-copy.md"))
    );
    assert!(fake.write_clipboard("# md").is_ok());
    assert_eq!(
        fake.resolve_export_path("out.md").expect("resolved"),
        PathBuf::from("/workspace/example/out.md")
    );
    assert!(
        fake.write_export_file(Path::new("/workspace/example/out.md"), b"# md", false)
            .is_ok()
    );
    // Effectful delegates were exercised exactly once each, in call order.
    let expected: Vec<String> = [
        "conversation_projection",
        "turn_handoff_projection",
        "clipboard_requires_terminal_paste",
        "write_recovery_copy:# md",
        "write_clipboard:# md",
        "resolve_export_path:out.md",
        "write_export_file:/workspace/example/out.md:4:false",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(fake.calls.borrow().as_slice(), expected.as_slice());
}

#[test]
fn export_error_and_empty_states_transport_exactly() {
    let fake = FakeExport {
        projection: Some(ConversationExportProjection {
            metadata: export_metadata(),
            transcript: TranscriptProjection::HistoryFallback(vec![
                HistoryEntry::Sanitized {
                    role: "user".to_string(),
                    body: "visible history".to_string(),
                },
                HistoryEntry::Literal {
                    role: "system".to_string(),
                    body: "[internal context omitted]".to_string(),
                },
            ]),
            restore_points: RestorePointProjection::Unreadable {
                reason: "permission denied".to_string(),
            },
        }),
        recovery: Some(None),
        clipboard: Some(Err("clipboard unavailable".to_string())),
        resolved: Some(Err("export paths may not contain `..`".to_string())),
        write: Some(Err("destination already exists".to_string())),
        ..FakeExport::default()
    };

    let projection = fake.conversation_projection();
    let TranscriptProjection::HistoryFallback(entries) = projection.transcript else {
        panic!("expected history fallback");
    };
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        &entries[0],
        HistoryEntry::Sanitized { role, body }
            if role == "user" && body == "visible history"
    ));
    assert!(matches!(
        &entries[1],
        HistoryEntry::Literal { role, body }
            if role == "system" && body == "[internal context omitted]"
    ));
    let RestorePointProjection::Unreadable { reason } = projection.restore_points else {
        panic!("expected unreadable restore points");
    };
    assert_eq!(reason, "permission denied");

    assert!(!fake.clipboard_requires_terminal_paste());
    assert_eq!(fake.write_recovery_copy("# md"), None);
    assert_eq!(
        fake.write_clipboard("# md").unwrap_err(),
        "clipboard unavailable"
    );
    assert_eq!(
        fake.resolve_export_path("../out.md").unwrap_err(),
        "export paths may not contain `..`"
    );
    assert_eq!(
        fake.write_export_file(Path::new("/tmp/out.md"), b"x", false)
            .unwrap_err(),
        "destination already exists"
    );
}

#[test]
fn export_projection_distinguishes_restore_states() {
    let states = [
        RestorePointProjection::None,
        RestorePointProjection::Unreadable {
            reason: "boom".to_string(),
        },
        RestorePointProjection::Recorded { snapshots: vec![] },
        RestorePointProjection::Recorded {
            snapshots: vec![export_recorded_snapshot()],
        },
    ];
    assert!(matches!(&states[0], RestorePointProjection::None));
    assert!(matches!(
        &states[1],
        RestorePointProjection::Unreadable { reason } if reason == "boom"
    ));
    let RestorePointProjection::Recorded { snapshots } = &states[2] else {
        panic!("expected recorded state");
    };
    assert!(snapshots.is_empty(), "existing-but-empty stays distinct");
    let RestorePointProjection::Recorded { snapshots } = &states[3] else {
        panic!("expected recorded state");
    };
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn export_projection_omission_markers_carry_no_hidden_payload() {
    // D9: the projection has no field for a reasoning body, reasoning
    // signature, or inline/local image payload. Omission markers are data-free
    // unit variants, so prohibited payloads cannot be transported even by
    // accident.
    let block = ExportBlock::InternalReasoning;
    let ExportBlock::InternalReasoning = block else {
        panic!("internal reasoning must be a payload-free marker");
    };
    let block = ExportBlock::ImageOmitted;
    let ExportBlock::ImageOmitted = block else {
        panic!("omitted image must be a payload-free marker");
    };

    const HIDDEN_REASONING: &str = "signed-thinking-secret-body";
    const HIDDEN_SIGNATURE: &str = "sig_1234567890";
    const HIDDEN_IMAGE: &str = "data:image/png;base64,QUJD";

    let projection = ConversationExportProjection {
        metadata: export_metadata(),
        transcript: TranscriptProjection::Authoritative(vec![ExportMessage {
            is_user_role: false,
            role: "assistant".to_string(),
            prompt_snippet: None,
            blocks: vec![ExportBlock::InternalReasoning, ExportBlock::ImageOmitted],
        }]),
        restore_points: RestorePointProjection::None,
    };
    let rendered = format!("{projection:?}");
    assert!(!rendered.contains(HIDDEN_REASONING));
    assert!(!rendered.contains(HIDDEN_SIGNATURE));
    assert!(!rendered.contains(HIDDEN_IMAGE));
    assert!(rendered.contains("InternalReasoning"));
    assert!(rendered.contains("ImageOmitted"));
}

#[test]
fn envelope_export_slot_is_independent_and_rejects_duplicates() {
    let mut first = FakeExport::default();
    let mut second = FakeExport::default();
    let mut control = FakeControl::default();

    let parts = CommandContexts::empty()
        .with_export(&mut first)
        .with_control(&mut control)
        .into_parts();
    assert!(
        parts.export.is_some(),
        "export slot must be present when declared"
    );
    assert!(
        parts.control.is_some(),
        "control slot may coexist with export"
    );
    assert!(
        parts.session.is_none()
            && parts.lifecycle.is_none()
            && parts.plugin.is_none()
            && parts.skill_group.is_none(),
        "unrelated slots must stay absent (exact exposure)"
    );

    let bare = CommandContexts::empty().into_parts();
    assert!(bare.export.is_none(), "undeclared export stays absent");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CommandContexts::empty()
            .with_export(&mut first)
            .with_export(&mut second);
    }));
    assert!(
        result.is_err(),
        "duplicate export slot must assert deterministically"
    );

    // Reading through the dyn facet works after insertion.
    let mut projection = FakeExport {
        terminal_paste: true,
        ..FakeExport::default()
    };
    let inserted = CommandContexts::empty().with_export(&mut projection);
    let export = inserted.into_parts().export.expect("inserted export");
    assert!(export.clipboard_requires_terminal_paste());
}

#[test]
fn whole_debug_capabilities_extend_published_bits_without_aliasing_authority() {
    let old = (1u32 << 17) - 1;
    let capabilities = [
        CommandCapabilities::DEBUG_RECEIPTS,
        CommandCapabilities::DEBUG_CHANGE,
        CommandCapabilities::DEBUG_HISTORY,
        CommandCapabilities::DEBUG_DIFF,
        CommandCapabilities::DEBUG_UNDO,
    ];
    let mut seen = old;
    for (index, capability) in capabilities.into_iter().enumerate() {
        assert_eq!(capability.bits_for_test(), 1 << (17 + index));
        assert_eq!(seen & capability.bits_for_test(), 0);
        assert!(!capability.contains(CommandCapabilities::DEBUG_DIAGNOSTICS));
        seen |= capability.bits_for_test();
    }
    assert_eq!(seen, (1u32 << 22) - 1);
}

struct StructcopyFixture {
    delivery: Result<StructcopyTransport, String>,
    writes: RefCell<Vec<String>>,
}
impl CommandSessionStructcopyContext for StructcopyFixture {
    fn transcript_item(&self, index: usize) -> Result<StructcopyTranscript, StructcopyError> {
        Ok(StructcopyTranscript {
            index,
            role: "system".into(),
            content: StructcopyContent::InternalContext,
        })
    }
    fn tool_pair(&self, _: &str) -> Result<StructcopyToolPair, StructcopyError> {
        Ok(StructcopyToolPair {
            name: "tool".into(),
            input: serde_json::json!({}),
            result: None,
        })
    }
    fn plan_snapshot(&self) -> Result<StructcopyPlan, StructcopyError> {
        Err(StructcopyError::Busy)
    }
    fn workflow_projection(&self, _: &str) -> Result<StructcopyWorkflow, StructcopyError> {
        Err(StructcopyError::Unavailable)
    }
    fn path_roots(&self) -> StructcopyPathRoots {
        StructcopyPathRoots::default()
    }
    fn write_clipboard(&self, text: &str) -> Result<StructcopyTransport, String> {
        self.writes.borrow_mut().push(text.into());
        self.delivery.clone()
    }
}

#[test]
fn structcopy_capability_preserves_all_published_identities() {
    let capabilities = [
        CommandCapabilities::SESSION,
        CommandCapabilities::MODEL,
        CommandCapabilities::COST,
        CommandCapabilities::MODE_POLICY,
        CommandCapabilities::SYSTEM_PROMPT,
        CommandCapabilities::SKILLS,
        CommandCapabilities::WORKSPACE,
        CommandCapabilities::PRESENTATION,
        CommandCapabilities::MEDIA,
        CommandCapabilities::MEMORY,
        CommandCapabilities::PROJECT,
        CommandCapabilities::SKILL_GROUP,
        CommandCapabilities::PLUGIN,
        CommandCapabilities::SESSION_LIFECYCLE,
        CommandCapabilities::SESSION_CONTROL,
        CommandCapabilities::SESSION_EXPORT,
        CommandCapabilities::DEBUG_DIAGNOSTICS,
        CommandCapabilities::DEBUG_RECEIPTS,
        CommandCapabilities::DEBUG_CHANGE,
        CommandCapabilities::DEBUG_HISTORY,
        CommandCapabilities::DEBUG_DIFF,
        CommandCapabilities::DEBUG_UNDO,
        CommandCapabilities::SESSION_STRUCTCOPY,
        CommandCapabilities::PERMISSIONS,
        CommandCapabilities::CONFIG_STATUS,
    ];
    let exact = CommandCapabilities::SESSION_STRUCTCOPY | CommandCapabilities::PRESENTATION;
    for (index, capability) in capabilities.into_iter().enumerate() {
        assert_eq!(capability.bits_for_test(), 1u32 << index);
        assert_eq!(exact.contains(capability), index == 7 || index == 22);
    }
    assert!(!exact.contains(CommandCapabilities::NONE));
}

#[test]
fn structcopy_slot_is_optional_and_does_not_grant_other_authority() {
    assert!(CommandContexts::empty().into_parts().structcopy.is_none());
    let mut fixture = StructcopyFixture {
        delivery: Ok(StructcopyTransport::Native),
        writes: RefCell::default(),
    };
    let mut presentation = Presentation;
    let ContextParts {
        session,
        model,
        cost,
        mode_policy,
        system_prompt,
        skills,
        workspace,
        presentation,
        media,
        memory,
        project,
        skill_group,
        plugin,
        lifecycle,
        control,
        export,
        structcopy,
        debug_receipts,
        debug_change,
        debug_history,
        debug_diff,
        debug_undo,
        debug_diagnostics,
        permissions,
        config_status,
    } = CommandContexts::empty()
        .with_structcopy(&mut fixture)
        .with_presentation(&mut presentation)
        .into_parts();
    assert!(presentation.is_some());
    let copy = structcopy.expect("selected structcopy facet");
    assert_eq!(
        copy.transcript_item(3).unwrap(),
        StructcopyTranscript {
            index: 3,
            role: "system".into(),
            content: StructcopyContent::InternalContext
        }
    );
    assert_eq!(copy.tool_pair("id").unwrap().result, None);
    assert_eq!(copy.plan_snapshot(), Err(StructcopyError::Busy));
    assert_eq!(
        copy.workflow_projection("id"),
        Err(StructcopyError::Unavailable)
    );
    for present in [
        session.is_some(),
        model.is_some(),
        cost.is_some(),
        mode_policy.is_some(),
        system_prompt.is_some(),
        skills.is_some(),
        workspace.is_some(),
        media.is_some(),
        memory.is_some(),
        project.is_some(),
        skill_group.is_some(),
        plugin.is_some(),
        lifecycle.is_some(),
        control.is_some(),
        export.is_some(),
        debug_receipts.is_some(),
        debug_change.is_some(),
        debug_history.is_some(),
        debug_diff.is_some(),
        debug_undo.is_some(),
        debug_diagnostics.is_some(),
        permissions.is_some(),
        config_status.is_some(),
    ] {
        assert!(!present);
    }
    assert!(
        fixture.writes.borrow().is_empty(),
        "observations do not write clipboard"
    );
}

#[test]
#[should_panic(expected = "structcopy facet already set")]
fn structcopy_duplicate_slot_fails_loudly() {
    let mut a = StructcopyFixture {
        delivery: Ok(StructcopyTransport::Native),
        writes: RefCell::default(),
    };
    let mut b = StructcopyFixture {
        delivery: Ok(StructcopyTransport::Native),
        writes: RefCell::default(),
    };
    let _ = CommandContexts::empty()
        .with_structcopy(&mut a)
        .with_structcopy(&mut b);
}

#[test]
fn structcopy_transport_and_unknown_observations_remain_distinct() {
    for delivery in [
        Ok(StructcopyTransport::Native),
        Ok(StructcopyTransport::TerminalQueued),
        Err("original host error".into()),
    ] {
        let fixture = StructcopyFixture {
            delivery: delivery.clone(),
            writes: RefCell::default(),
        };
        assert_eq!(fixture.write_clipboard("exact bytes"), delivery);
        assert_eq!(*fixture.writes.borrow(), ["exact bytes"]);
    }
    let unknown = StructcopyToolResult {
        content: "recorded".into(),
        is_error: None,
        content_blocks: None,
    };
    assert_ne!(
        unknown,
        StructcopyToolResult {
            is_error: Some(false),
            ..unknown.clone()
        }
    );
    assert_ne!(
        StructcopyError::Preparation("detail".into()),
        StructcopyError::Unavailable
    );
    let result = crate::outcome::StructcopyCommandResult::error("detail");
    assert_eq!(result.message.as_deref(), Some("Error: detail"));
    assert!(result.action.is_none());
}

#[test]
fn structcopy_known_projection_fields_preserve_nulls_and_omission_rules() {
    let plan: StructcopyPlan = serde_json::from_value(
        serde_json::json!({"title":"plan","items":[{"step":"one","status":"in_progress"}]}),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(plan).unwrap(),
        serde_json::json!({"title":"plan","items":[{"step":"one","status":"in_progress"}]})
    );
    let workflow = serde_json::json!({
        "run_id":"run","status":"degraded","lifecycle_seq":1,"started_at_ms":2,"completed_at_ms":null,
        "source_file":"flow.js","workflow_id":null,"workflow_goal":null,"token_budget":null,
        "child_count":0,"schema_error_count":0,"schema_repair_count":0,"dispatch_failure_count":0,
        "progress_count":0,"last_progress":null,"event_count":0,"last_event_type":null,
        "leaf_count":null,"branch_count":null,"control_count":null,"execution_status":null,
        "gate_count":1,"blocked_gate_count":0,"gate_status":[{"gate_id":"gate","state":"pending"}],
        "error":null,"usage":{"tasks_reported":0,"input_tokens":0},"events_dropped":4
    });
    let parsed: StructcopyWorkflow = serde_json::from_value(workflow.clone()).unwrap();
    assert_eq!(parsed.status, StructcopyWorkflowStatus::Degraded);
    assert_eq!(parsed.leaf_count, None);
    assert_eq!(parsed.usage.as_ref().unwrap().input_tokens, Some(0));
    assert_eq!(parsed.usage.as_ref().unwrap().output_tokens, None);
    assert_eq!(serde_json::to_value(parsed).unwrap(), workflow);
}

#[test]
fn config_policy_authorities_append_without_widening_each_other() {
    assert_eq!(
        CommandCapabilities::SESSION_STRUCTCOPY.bits_for_test(),
        1 << 22
    );
    assert_eq!(CommandCapabilities::PERMISSIONS.bits_for_test(), 1 << 23);
    assert_eq!(CommandCapabilities::CONFIG_STATUS.bits_for_test(), 1 << 24);
    let permissions = CommandCapabilities::PERMISSIONS | CommandCapabilities::PRESENTATION;
    let status = CommandCapabilities::CONFIG_STATUS | CommandCapabilities::PRESENTATION;
    assert!(!permissions.contains(CommandCapabilities::CONFIG_STATUS));
    assert!(!status.contains(CommandCapabilities::PERMISSIONS));
    for caps in [permissions, status] {
        assert!(caps.contains(CommandCapabilities::PRESENTATION));
        assert!(!caps.contains(CommandCapabilities::MODE_POLICY));
        assert!(!caps.contains(CommandCapabilities::SESSION));
        assert!(!caps.contains(CommandCapabilities::WORKSPACE));
        assert!(!caps.contains(CommandCapabilities::NONE));
    }
    let empty = CommandContexts::empty().into_parts();
    assert!(empty.permissions.is_none());
    assert!(empty.config_status.is_none());
}

#[test]
fn permission_facet_is_object_safe_and_keeps_removal_token_with_rule() {
    use crate::config_policy::*;
    struct Permissions;
    impl CommandPermissionsContext for Permissions {
        fn snapshot(&self) -> Result<PermissionsView, String> {
            Ok(PermissionsView {
                path: PathBuf::from("permissions.toml"),
                file_state: CommandPermissionsFileState::Present,
                rules: vec![PermissionRule {
                    action: CommandPermissionAction::Ask,
                    tool: "exec_shell".into(),
                    command: Some("cargo test".into()),
                    command_exact: true,
                    path: None,
                    workspace: None,
                    applies_here: true,
                    removal_token: "opaque-token".into(),
                }],
                approval_mode: CommandApprovalMode::Suggest,
                audit_path: None,
            })
        }
        fn remove_rule(
            &mut self,
            index: usize,
            expected_token: &str,
        ) -> Result<RemovedPermissionRule, String> {
            if index != 0 || expected_token != "opaque-token" {
                return Err("stale".into());
            }
            Ok(RemovedPermissionRule {
                action: CommandPermissionAction::Ask,
                tool: "exec_shell".into(),
            })
        }
    }
    let mut fixture = Permissions;
    let parts = CommandContexts::empty()
        .with_permissions(&mut fixture)
        .into_parts();
    assert!(parts.config_status.is_none());
    assert!(parts.mode_policy.is_none());
    assert!(parts.session.is_none());
    let facet = parts.permissions.unwrap();
    let view = facet.snapshot().unwrap();
    assert_eq!(view.rules[0].removal_token, "opaque-token");
    assert_eq!(facet.remove_rule(0, "wrong"), Err("stale".into()));
    assert_eq!(
        facet
            .remove_rule(0, &view.rules[0].removal_token)
            .unwrap()
            .tool,
        "exec_shell"
    );
}

#[test]
fn status_facet_is_object_safe_and_construction_never_observes_or_grants_mutation() {
    struct Status;
    impl CommandConfigStatusContext for Status {
        fn snapshot(&self) -> crate::config_policy::ConfigStatusView {
            panic!("envelope construction must not observe status")
        }
    }
    let mut status = Status;
    let parts = CommandContexts::empty()
        .with_config_status(&mut status)
        .into_parts();
    assert!(parts.config_status.is_some());
    assert!(parts.permissions.is_none());
    assert!(parts.presentation.is_none());
    assert!(parts.mode_policy.is_none());
    assert!(parts.session.is_none());
    assert!(parts.workspace.is_none());
}
