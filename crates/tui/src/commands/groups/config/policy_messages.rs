//! Typed, validated templates. Load before host observations or permission mutation.
use codewhale_command_contract::facets::CommandPresentationContext;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy)]
pub enum PermissionsText {
    AppliesHere,
    FileEmpty,
    FileMissing,
    FilePresent,
    InactiveHere,
    ListHeader,
    MatchAnyInvocation,
    MatchCommandPrefix,
    MatchExactCommand,
    MatchExactPath,
    NoRules,
    OperationFailed,
    PostureAsk,
    PostureAcceptEdits,
    PostureAuto,
    PostureBypass,
    PostureHeader,
    PostureNever,
    ReceiptsNote,
    RemovePreview,
    Removed,
    RuleEntry,
    RuleNotFound,
    ScopeGlobal,
    ScopeRepo,
    Usage,
}

pub struct PermissionsMessages {
    permissions_applies_here: String,
    permissions_file_empty: String,
    permissions_file_missing: String,
    permissions_file_present: String,
    permissions_inactive_here: String,
    permissions_list_header: String,
    permissions_match_any_invocation: String,
    permissions_match_command_prefix: String,
    permissions_match_exact_command: String,
    permissions_match_exact_path: String,
    permissions_no_rules: String,
    permissions_operation_failed: String,
    permissions_posture_ask: String,
    permissions_posture_accept_edits: String,
    permissions_posture_auto: String,
    permissions_posture_bypass: String,
    permissions_posture_header: String,
    permissions_posture_never: String,
    permissions_receipts_note: String,
    permissions_remove_preview: String,
    permissions_removed: String,
    permissions_rule_entry: String,
    permissions_rule_not_found: String,
    permissions_scope_global: String,
    permissions_scope_repo: String,
    permissions_usage: String,
}

impl PermissionsMessages {
    pub fn load(presentation: &dyn CommandPresentationContext) -> Result<Self, String> {
        Ok(Self {
            permissions_applies_here: presentation.translate("permissions_applies_here", &[])?,
            permissions_file_empty: presentation.translate("permissions_file_empty", &[])?,
            permissions_file_missing: presentation.translate("permissions_file_missing", &[])?,
            permissions_file_present: presentation.translate("permissions_file_present", &[])?,
            permissions_inactive_here: presentation.translate("permissions_inactive_here", &[])?,
            permissions_list_header: presentation.translate(
                "permissions_list_header",
                &[
                    ("count", "{count}"),
                    ("file_state", "{file_state}"),
                    ("path", "{path}"),
                ],
            )?,
            permissions_match_any_invocation: presentation
                .translate("permissions_match_any_invocation", &[])?,
            permissions_match_command_prefix: presentation.translate(
                "permissions_match_command_prefix",
                &[("command", "{command}")],
            )?,
            permissions_match_exact_command: presentation.translate(
                "permissions_match_exact_command",
                &[("command", "{command}")],
            )?,
            permissions_match_exact_path: presentation
                .translate("permissions_match_exact_path", &[("path", "{path}")])?,
            permissions_no_rules: presentation.translate("permissions_no_rules", &[])?,
            permissions_operation_failed: presentation
                .translate("permissions_operation_failed", &[("error", "{error}")])?,
            permissions_posture_ask: presentation.translate("permissions_posture_ask", &[])?,
            permissions_posture_accept_edits: presentation
                .translate("permissions_posture_accept_edits", &[])?,
            permissions_posture_auto: presentation.translate("permissions_posture_auto", &[])?,
            permissions_posture_bypass: presentation
                .translate("permissions_posture_bypass", &[])?,
            permissions_posture_header: presentation
                .translate("permissions_posture_header", &[("posture", "{posture}")])?,
            permissions_posture_never: presentation.translate("permissions_posture_never", &[])?,
            permissions_receipts_note: presentation.translate(
                "permissions_receipts_note",
                &[("audit_path", "{audit_path}")],
            )?,
            permissions_remove_preview: presentation.translate(
                "permissions_remove_preview",
                &[
                    ("command", "{command}"),
                    ("index", "{index}"),
                    ("rule", "{rule}"),
                ],
            )?,
            permissions_removed: presentation.translate(
                "permissions_removed",
                &[
                    ("action", "{action}"),
                    ("index", "{index}"),
                    ("tool", "{tool}"),
                ],
            )?,
            permissions_rule_entry: presentation.translate(
                "permissions_rule_entry",
                &[
                    ("action", "{action}"),
                    ("applicability", "{applicability}"),
                    ("index", "{index}"),
                    ("matcher", "{matcher}"),
                    ("scope", "{scope}"),
                    ("tool", "{tool}"),
                ],
            )?,
            permissions_rule_not_found: presentation
                .translate("permissions_rule_not_found", &[("index", "{index}")])?,
            permissions_scope_global: presentation.translate("permissions_scope_global", &[])?,
            permissions_scope_repo: presentation
                .translate("permissions_scope_repo", &[("workspace", "{workspace}")])?,
            permissions_usage: presentation.translate("permissions_usage", &[])?,
        })
    }
    pub fn text(&self, id: PermissionsText) -> Cow<'static, str> {
        Cow::Owned(
            match id {
                PermissionsText::AppliesHere => &self.permissions_applies_here,
                PermissionsText::FileEmpty => &self.permissions_file_empty,
                PermissionsText::FileMissing => &self.permissions_file_missing,
                PermissionsText::FilePresent => &self.permissions_file_present,
                PermissionsText::InactiveHere => &self.permissions_inactive_here,
                PermissionsText::ListHeader => &self.permissions_list_header,
                PermissionsText::MatchAnyInvocation => &self.permissions_match_any_invocation,
                PermissionsText::MatchCommandPrefix => &self.permissions_match_command_prefix,
                PermissionsText::MatchExactCommand => &self.permissions_match_exact_command,
                PermissionsText::MatchExactPath => &self.permissions_match_exact_path,
                PermissionsText::NoRules => &self.permissions_no_rules,
                PermissionsText::OperationFailed => &self.permissions_operation_failed,
                PermissionsText::PostureAsk => &self.permissions_posture_ask,
                PermissionsText::PostureAcceptEdits => &self.permissions_posture_accept_edits,
                PermissionsText::PostureAuto => &self.permissions_posture_auto,
                PermissionsText::PostureBypass => &self.permissions_posture_bypass,
                PermissionsText::PostureHeader => &self.permissions_posture_header,
                PermissionsText::PostureNever => &self.permissions_posture_never,
                PermissionsText::ReceiptsNote => &self.permissions_receipts_note,
                PermissionsText::RemovePreview => &self.permissions_remove_preview,
                PermissionsText::Removed => &self.permissions_removed,
                PermissionsText::RuleEntry => &self.permissions_rule_entry,
                PermissionsText::RuleNotFound => &self.permissions_rule_not_found,
                PermissionsText::ScopeGlobal => &self.permissions_scope_global,
                PermissionsText::ScopeRepo => &self.permissions_scope_repo,
                PermissionsText::Usage => &self.permissions_usage,
            }
            .clone(),
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub enum StatusText {
    AppModeAgent,
    AppModePlan,
    SessionMetricsCache,
    SessionMetricsInput,
    SessionMetricsLlm,
    SessionMetricsStatusLine,
    SessionMetricsStep,
    SessionMetricsSteps,
    SessionMetricsTokensPerSecond,
    SessionMetricsTools,
    SessionMetricsTtft,
    SessionMetricsTurn,
    SessionMetricsTurns,
    SnapshotsDisabledTooLarge,
    SnapshotsDisabledTooManyFiles,
    SnapshotsDisabledUnsafeLocation,
    SnapshotsFailing,
    SnapshotsHistoryRepaired,
    StatusApprovalAsk,
    StatusApprovalAcceptEdits,
    StatusApprovalAuto,
    StatusApprovalFullAccess,
    StatusApprovalNever,
    StatusCacheNotReported,
    StatusCacheSummary,
    StatusContextSourceCatalog,
    StatusContextSourceConfigured,
    StatusContextSourceConfiguredModel,
    StatusContextSourceFallback,
    StatusContextSourceKimiSafeFloor,
    StatusContextSourceModelHint,
    StatusContextSourceProviderReported,
    StatusContextUsage,
    StatusFleetDrifted,
    StatusLabelCatalog,
    StatusLabelCloudFacts,
    StatusLabelContextWindow,
    StatusLabelDirectory,
    StatusLabelFleet,
    StatusLabelMcp,
    StatusLabelMode,
    StatusLabelProjectDocs,
    StatusLabelRoute,
    StatusLabelSafety,
    StatusLabelSession,
    StatusLabelSessionCost,
    StatusLabelSessionTokens,
    StatusLabelToolOutputs,
    StatusLabelWindowOverride,
    StatusLabelWindowSource,
    StatusMcpConfigured,
    StatusModelNotInRoster,
    StatusPointers,
    StatusPostureSummary,
    StatusProjectDocsNone,
    StatusRouteSummary,
    StatusSafetyDisabled,
    StatusSafetyDisabledSetuidAllowed,
    StatusSafetyDisabledSetuidBlocked,
    StatusSafetyExternal,
    StatusSafetyReadOnly,
    StatusSafetyReadOnlyUnenforced,
    StatusSafetyWorkspaceWriteNetworkOff,
    StatusSafetyWorkspaceWriteNetworkOn,
    StatusSafetyWorkspaceWriteUnenforcedNetworkOff,
    StatusSafetyWorkspaceWriteUnenforcedNetworkOn,
    StatusSessionNotSaved,
    StatusSessionSummary,
    StatusSessionTokensSummary,
    StatusShellOff,
    StatusShellOn,
    StatusToolArtifacts,
    StatusToolCompactReceipts,
    StatusToolNone,
    StatusToolRawPressure,
    StatusTrustedWorkspace,
    StatusWindowOverrideActiveProvider,
    StatusWindowOverrideProvider,
    StatusWorkspace,
}

pub struct StatusMessages {
    app_mode_agent: String,
    app_mode_plan: String,
    session_metrics_cache: String,
    session_metrics_input: String,
    session_metrics_llm: String,
    session_metrics_status_line: String,
    session_metrics_step: String,
    session_metrics_steps: String,
    session_metrics_tokens_per_second: String,
    session_metrics_tools: String,
    session_metrics_ttft: String,
    session_metrics_turn: String,
    session_metrics_turns: String,
    snapshots_disabled_too_large: String,
    snapshots_disabled_too_many_files: String,
    snapshots_disabled_unsafe_location: String,
    snapshots_failing: String,
    snapshots_history_repaired: String,
    status_approval_ask: String,
    status_approval_accept_edits: String,
    status_approval_auto: String,
    status_approval_full_access: String,
    status_approval_never: String,
    status_cache_not_reported: String,
    status_cache_summary: String,
    status_context_source_catalog: String,
    status_context_source_configured: String,
    status_context_source_configured_model: String,
    status_context_source_fallback: String,
    status_context_source_kimi_safe_floor: String,
    status_context_source_model_hint: String,
    status_context_source_provider_reported: String,
    status_context_usage: String,
    status_fleet_drifted: String,
    status_label_catalog: String,
    status_label_cloud_facts: String,
    status_label_context_window: String,
    status_label_directory: String,
    status_label_fleet: String,
    status_label_mcp: String,
    status_label_mode: String,
    status_label_project_docs: String,
    status_label_route: String,
    status_label_safety: String,
    status_label_session: String,
    status_label_session_cost: String,
    status_label_session_tokens: String,
    status_label_tool_outputs: String,
    status_label_window_override: String,
    status_label_window_source: String,
    status_mcp_configured: String,
    status_model_not_in_roster: String,
    status_pointers: String,
    status_posture_summary: String,
    status_project_docs_none: String,
    status_route_summary: String,
    status_safety_disabled: String,
    status_safety_disabled_setuid_allowed: String,
    status_safety_disabled_setuid_blocked: String,
    status_safety_external: String,
    status_safety_read_only: String,
    status_safety_read_only_unenforced: String,
    status_safety_workspace_write_network_off: String,
    status_safety_workspace_write_network_on: String,
    status_safety_workspace_write_unenforced_network_off: String,
    status_safety_workspace_write_unenforced_network_on: String,
    status_session_not_saved: String,
    status_session_summary: String,
    status_session_tokens_summary: String,
    status_shell_off: String,
    status_shell_on: String,
    status_tool_artifacts: String,
    status_tool_compact_receipts: String,
    status_tool_none: String,
    status_tool_raw_pressure: String,
    status_trusted_workspace: String,
    status_window_override_active_provider: String,
    status_window_override_provider: String,
    status_workspace: String,
}

impl StatusMessages {
    pub fn load(presentation: &dyn CommandPresentationContext) -> Result<Self, String> {
        Ok(Self {
            app_mode_agent: presentation.translate("app_mode_agent", &[])?,
            app_mode_plan: presentation.translate("app_mode_plan", &[])?,
            session_metrics_cache: presentation.translate("session_metrics_cache", &[])?,
            session_metrics_input: presentation.translate("session_metrics_input", &[])?,
            session_metrics_llm: presentation.translate("session_metrics_llm", &[])?,
            session_metrics_status_line: presentation
                .translate("session_metrics_status_line", &[("metrics", "{metrics}")])?,
            session_metrics_step: presentation.translate("session_metrics_step", &[])?,
            session_metrics_steps: presentation.translate("session_metrics_steps", &[])?,
            session_metrics_tokens_per_second: presentation
                .translate("session_metrics_tokens_per_second", &[])?,
            session_metrics_tools: presentation.translate("session_metrics_tools", &[])?,
            session_metrics_ttft: presentation.translate("session_metrics_ttft", &[])?,
            session_metrics_turn: presentation.translate("session_metrics_turn", &[])?,
            session_metrics_turns: presentation.translate("session_metrics_turns", &[])?,
            snapshots_disabled_too_large: presentation.translate(
                "snapshots_disabled_too_large",
                &[
                    ("config_key", "{config_key}"),
                    ("limit", "{limit}"),
                    ("workspace", "{workspace}"),
                ],
            )?,
            snapshots_disabled_too_many_files: presentation.translate(
                "snapshots_disabled_too_many_files",
                &[("limit", "{limit}"), ("workspace", "{workspace}")],
            )?,
            snapshots_disabled_unsafe_location: presentation.translate(
                "snapshots_disabled_unsafe_location",
                &[("workspace", "{workspace}")],
            )?,
            snapshots_failing: presentation.translate(
                "snapshots_failing",
                &[("limit", "{limit}"), ("workspace", "{workspace}")],
            )?,
            snapshots_history_repaired: presentation.translate(
                "snapshots_history_repaired",
                &[("workspace", "{workspace}")],
            )?,
            status_approval_ask: presentation.translate("status_approval_ask", &[])?,
            status_approval_accept_edits: presentation
                .translate("status_approval_accept_edits", &[])?,
            status_approval_auto: presentation.translate("status_approval_auto", &[])?,
            status_approval_full_access: presentation
                .translate("status_approval_full_access", &[])?,
            status_approval_never: presentation.translate("status_approval_never", &[])?,
            status_cache_not_reported: presentation.translate("status_cache_not_reported", &[])?,
            status_cache_summary: presentation.translate(
                "status_cache_summary",
                &[("hit", "{hit}"), ("miss", "{miss}")],
            )?,
            status_context_source_catalog: presentation
                .translate("status_context_source_catalog", &[])?,
            status_context_source_configured: presentation
                .translate("status_context_source_configured", &[])?,
            status_context_source_configured_model: presentation
                .translate("status_context_source_configured_model", &[])?,
            status_context_source_fallback: presentation
                .translate("status_context_source_fallback", &[])?,
            status_context_source_kimi_safe_floor: presentation
                .translate("status_context_source_kimi_safe_floor", &[])?,
            status_context_source_model_hint: presentation
                .translate("status_context_source_model_hint", &[])?,
            status_context_source_provider_reported: presentation
                .translate("status_context_source_provider_reported", &[])?,
            status_context_usage: presentation.translate(
                "status_context_usage",
                &[
                    ("max", "{max}"),
                    ("percent", "{percent}"),
                    ("used", "{used}"),
                ],
            )?,
            status_fleet_drifted: presentation.translate(
                "status_fleet_drifted",
                &[("count", "{count}"), ("fleet", "{fleet}"), ("ids", "{ids}")],
            )?,
            status_label_catalog: presentation.translate("status_label_catalog", &[])?,
            status_label_cloud_facts: presentation.translate("status_label_cloud_facts", &[])?,
            status_label_context_window: presentation
                .translate("status_label_context_window", &[])?,
            status_label_directory: presentation.translate("status_label_directory", &[])?,
            status_label_fleet: presentation.translate("status_label_fleet", &[])?,
            status_label_mcp: presentation.translate("status_label_mcp", &[])?,
            status_label_mode: presentation.translate("status_label_mode", &[])?,
            status_label_project_docs: presentation.translate("status_label_project_docs", &[])?,
            status_label_route: presentation.translate("status_label_route", &[])?,
            status_label_safety: presentation.translate("status_label_safety", &[])?,
            status_label_session: presentation.translate("status_label_session", &[])?,
            status_label_session_cost: presentation.translate("status_label_session_cost", &[])?,
            status_label_session_tokens: presentation
                .translate("status_label_session_tokens", &[])?,
            status_label_tool_outputs: presentation.translate("status_label_tool_outputs", &[])?,
            status_label_window_override: presentation
                .translate("status_label_window_override", &[])?,
            status_label_window_source: presentation
                .translate("status_label_window_source", &[])?,
            status_mcp_configured: presentation
                .translate("status_mcp_configured", &[("count", "{count}")])?,
            status_model_not_in_roster: presentation.translate(
                "status_model_not_in_roster",
                &[("model", "{model}"), ("provider", "{provider}")],
            )?,
            status_pointers: presentation.translate("status_pointers", &[])?,
            status_posture_summary: presentation.translate(
                "status_posture_summary",
                &[
                    ("approval", "{approval}"),
                    ("mode", "{mode}"),
                    ("shell", "{shell}"),
                    ("trust", "{trust}"),
                ],
            )?,
            status_project_docs_none: presentation.translate("status_project_docs_none", &[])?,
            status_route_summary: presentation.translate(
                "status_route_summary",
                &[
                    ("model", "{model}"),
                    ("provider", "{provider}"),
                    ("reasoning", "{reasoning}"),
                ],
            )?,
            status_safety_disabled: presentation.translate("status_safety_disabled", &[])?,
            status_safety_disabled_setuid_allowed: presentation
                .translate("status_safety_disabled_setuid_allowed", &[])?,
            status_safety_disabled_setuid_blocked: presentation
                .translate("status_safety_disabled_setuid_blocked", &[])?,
            status_safety_external: presentation.translate("status_safety_external", &[])?,
            status_safety_read_only: presentation.translate("status_safety_read_only", &[])?,
            status_safety_read_only_unenforced: presentation
                .translate("status_safety_read_only_unenforced", &[])?,
            status_safety_workspace_write_network_off: presentation
                .translate("status_safety_workspace_write_network_off", &[])?,
            status_safety_workspace_write_network_on: presentation
                .translate("status_safety_workspace_write_network_on", &[])?,
            status_safety_workspace_write_unenforced_network_off: presentation
                .translate("status_safety_workspace_write_unenforced_network_off", &[])?,
            status_safety_workspace_write_unenforced_network_on: presentation
                .translate("status_safety_workspace_write_unenforced_network_on", &[])?,
            status_session_not_saved: presentation.translate("status_session_not_saved", &[])?,
            status_session_summary: presentation.translate(
                "status_session_summary",
                &[
                    ("cells", "{cells}"),
                    ("messages", "{messages}"),
                    ("session", "{session}"),
                ],
            )?,
            status_session_tokens_summary: presentation.translate(
                "status_session_tokens_summary",
                &[
                    ("cache", "{cache}"),
                    ("input", "{input}"),
                    ("output", "{output}"),
                    ("total", "{total}"),
                ],
            )?,
            status_shell_off: presentation.translate("status_shell_off", &[])?,
            status_shell_on: presentation.translate("status_shell_on", &[])?,
            status_tool_artifacts: presentation.translate(
                "status_tool_artifacts",
                &[("bytes", "{bytes}"), ("count", "{count}")],
            )?,
            status_tool_compact_receipts: presentation
                .translate("status_tool_compact_receipts", &[("count", "{count}")])?,
            status_tool_none: presentation.translate("status_tool_none", &[])?,
            status_tool_raw_pressure: presentation.translate(
                "status_tool_raw_pressure",
                &[("chars", "{chars}"), ("count", "{count}")],
            )?,
            status_trusted_workspace: presentation.translate("status_trusted_workspace", &[])?,
            status_window_override_active_provider: presentation
                .translate("status_window_override_active_provider", &[])?,
            status_window_override_provider: presentation
                .translate("status_window_override_provider", &[("table", "{table}")])?,
            status_workspace: presentation.translate("status_workspace", &[])?,
        })
    }
    pub fn text(&self, id: StatusText) -> Cow<'static, str> {
        Cow::Owned(
            match id {
                StatusText::AppModeAgent => &self.app_mode_agent,
                StatusText::AppModePlan => &self.app_mode_plan,
                StatusText::SessionMetricsCache => &self.session_metrics_cache,
                StatusText::SessionMetricsInput => &self.session_metrics_input,
                StatusText::SessionMetricsLlm => &self.session_metrics_llm,
                StatusText::SessionMetricsStatusLine => &self.session_metrics_status_line,
                StatusText::SessionMetricsStep => &self.session_metrics_step,
                StatusText::SessionMetricsSteps => &self.session_metrics_steps,
                StatusText::SessionMetricsTokensPerSecond => {
                    &self.session_metrics_tokens_per_second
                }
                StatusText::SessionMetricsTools => &self.session_metrics_tools,
                StatusText::SessionMetricsTtft => &self.session_metrics_ttft,
                StatusText::SessionMetricsTurn => &self.session_metrics_turn,
                StatusText::SessionMetricsTurns => &self.session_metrics_turns,
                StatusText::SnapshotsDisabledTooLarge => &self.snapshots_disabled_too_large,
                StatusText::SnapshotsDisabledTooManyFiles => {
                    &self.snapshots_disabled_too_many_files
                }
                StatusText::SnapshotsDisabledUnsafeLocation => {
                    &self.snapshots_disabled_unsafe_location
                }
                StatusText::SnapshotsFailing => &self.snapshots_failing,
                StatusText::SnapshotsHistoryRepaired => &self.snapshots_history_repaired,
                StatusText::StatusApprovalAsk => &self.status_approval_ask,
                StatusText::StatusApprovalAcceptEdits => &self.status_approval_accept_edits,
                StatusText::StatusApprovalAuto => &self.status_approval_auto,
                StatusText::StatusApprovalFullAccess => &self.status_approval_full_access,
                StatusText::StatusApprovalNever => &self.status_approval_never,
                StatusText::StatusCacheNotReported => &self.status_cache_not_reported,
                StatusText::StatusCacheSummary => &self.status_cache_summary,
                StatusText::StatusContextSourceCatalog => &self.status_context_source_catalog,
                StatusText::StatusContextSourceConfigured => &self.status_context_source_configured,
                StatusText::StatusContextSourceConfiguredModel => {
                    &self.status_context_source_configured_model
                }
                StatusText::StatusContextSourceFallback => &self.status_context_source_fallback,
                StatusText::StatusContextSourceKimiSafeFloor => {
                    &self.status_context_source_kimi_safe_floor
                }
                StatusText::StatusContextSourceModelHint => &self.status_context_source_model_hint,
                StatusText::StatusContextSourceProviderReported => {
                    &self.status_context_source_provider_reported
                }
                StatusText::StatusContextUsage => &self.status_context_usage,
                StatusText::StatusFleetDrifted => &self.status_fleet_drifted,
                StatusText::StatusLabelCatalog => &self.status_label_catalog,
                StatusText::StatusLabelCloudFacts => &self.status_label_cloud_facts,
                StatusText::StatusLabelContextWindow => &self.status_label_context_window,
                StatusText::StatusLabelDirectory => &self.status_label_directory,
                StatusText::StatusLabelFleet => &self.status_label_fleet,
                StatusText::StatusLabelMcp => &self.status_label_mcp,
                StatusText::StatusLabelMode => &self.status_label_mode,
                StatusText::StatusLabelProjectDocs => &self.status_label_project_docs,
                StatusText::StatusLabelRoute => &self.status_label_route,
                StatusText::StatusLabelSafety => &self.status_label_safety,
                StatusText::StatusLabelSession => &self.status_label_session,
                StatusText::StatusLabelSessionCost => &self.status_label_session_cost,
                StatusText::StatusLabelSessionTokens => &self.status_label_session_tokens,
                StatusText::StatusLabelToolOutputs => &self.status_label_tool_outputs,
                StatusText::StatusLabelWindowOverride => &self.status_label_window_override,
                StatusText::StatusLabelWindowSource => &self.status_label_window_source,
                StatusText::StatusMcpConfigured => &self.status_mcp_configured,
                StatusText::StatusModelNotInRoster => &self.status_model_not_in_roster,
                StatusText::StatusPointers => &self.status_pointers,
                StatusText::StatusPostureSummary => &self.status_posture_summary,
                StatusText::StatusProjectDocsNone => &self.status_project_docs_none,
                StatusText::StatusRouteSummary => &self.status_route_summary,
                StatusText::StatusSafetyDisabled => &self.status_safety_disabled,
                StatusText::StatusSafetyDisabledSetuidAllowed => {
                    &self.status_safety_disabled_setuid_allowed
                }
                StatusText::StatusSafetyDisabledSetuidBlocked => {
                    &self.status_safety_disabled_setuid_blocked
                }
                StatusText::StatusSafetyExternal => &self.status_safety_external,
                StatusText::StatusSafetyReadOnly => &self.status_safety_read_only,
                StatusText::StatusSafetyReadOnlyUnenforced => {
                    &self.status_safety_read_only_unenforced
                }
                StatusText::StatusSafetyWorkspaceWriteNetworkOff => {
                    &self.status_safety_workspace_write_network_off
                }
                StatusText::StatusSafetyWorkspaceWriteNetworkOn => {
                    &self.status_safety_workspace_write_network_on
                }
                StatusText::StatusSafetyWorkspaceWriteUnenforcedNetworkOff => {
                    &self.status_safety_workspace_write_unenforced_network_off
                }
                StatusText::StatusSafetyWorkspaceWriteUnenforcedNetworkOn => {
                    &self.status_safety_workspace_write_unenforced_network_on
                }
                StatusText::StatusSessionNotSaved => &self.status_session_not_saved,
                StatusText::StatusSessionSummary => &self.status_session_summary,
                StatusText::StatusSessionTokensSummary => &self.status_session_tokens_summary,
                StatusText::StatusShellOff => &self.status_shell_off,
                StatusText::StatusShellOn => &self.status_shell_on,
                StatusText::StatusToolArtifacts => &self.status_tool_artifacts,
                StatusText::StatusToolCompactReceipts => &self.status_tool_compact_receipts,
                StatusText::StatusToolNone => &self.status_tool_none,
                StatusText::StatusToolRawPressure => &self.status_tool_raw_pressure,
                StatusText::StatusTrustedWorkspace => &self.status_trusted_workspace,
                StatusText::StatusWindowOverrideActiveProvider => {
                    &self.status_window_override_active_provider
                }
                StatusText::StatusWindowOverrideProvider => &self.status_window_override_provider,
                StatusText::StatusWorkspace => &self.status_workspace,
            }
            .clone(),
        )
    }
}
