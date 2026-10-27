//! Value-only projection owned by the authoritative source module.
//! No command, App or rendering operation is reachable from these conversions.

use codewhale_command_contract::facets::*;

pub(crate) fn source_map(report: crate::context_report::PromptSourceMap) -> DebugPromptSourceMap {
    use crate::context_report::{ActivationReason as A, CountingConfidence as C, SourceKind as S};
    use crate::route_runtime::ContextWindowSource;
    use codewhale_runtime::context_budget::PressureLevel;
    let pressure_label = report
        .budget_used_percent
        .map(|percent| PressureLevel::from_usage_percent(percent).label())
        .unwrap_or("unknown")
        .to_string();
    let source_label = report
        .context_window_source
        .as_deref()
        .unwrap_or_else(|| ContextWindowSource::Fallback.label());
    let context_window_verified =
        ContextWindowSource::from_label(source_label).is_some_and(ContextWindowSource::is_verified);
    let kind = |kind| match kind {
        S::Constitution => DebugSourceKind::Constitution,
        S::UserConstitution => DebugSourceKind::UserConstitution,
        S::RepoConstitution => DebugSourceKind::RepoConstitution,
        S::ProjectContext => DebugSourceKind::ProjectContext,
        S::ProjectContextWarning => DebugSourceKind::ProjectContextWarning,
        S::ProjectContextPack => DebugSourceKind::ProjectContextPack,
        S::SkillsBlock => DebugSourceKind::SkillsBlock,
        S::ContextManagement => DebugSourceKind::ContextManagement,
        S::CompactionRelayTemplate => DebugSourceKind::CompactionRelayTemplate,
        S::RuntimePolicy => DebugSourceKind::RuntimePolicy,
        S::AuthorityRecap => DebugSourceKind::AuthorityRecap,
        S::EnvironmentBlock => DebugSourceKind::EnvironmentBlock,
        S::UserMemory => DebugSourceKind::UserMemory,
        S::SessionGoal => DebugSourceKind::SessionGoal,
        S::HandoffRelay => DebugSourceKind::HandoffRelay,
        S::ToolSchemas => DebugSourceKind::ToolSchemas,
        S::UserRequest => DebugSourceKind::UserRequest,
        S::ConversationHistory => DebugSourceKind::ConversationHistory,
        S::ToolResult => DebugSourceKind::ToolResult,
        S::ModelProviderFact => DebugSourceKind::ModelProviderFact,
    };
    DebugPromptSourceMap {
        entries: report
            .entries
            .into_iter()
            .map(|entry| DebugSourceEntry {
                source_kind: kind(entry.source_kind),
                label: entry.label,
                source_path: entry.source_path,
                activation_reason: match entry.activation_reason {
                    A::AlwaysOn => DebugActivationReason::AlwaysOn,
                    A::FilePresent => DebugActivationReason::FilePresent,
                    A::ConfigEnabled => DebugActivationReason::ConfigEnabled,
                    A::RuntimeState => DebugActivationReason::RuntimeState,
                    A::PerRequest => DebugActivationReason::PerRequest,
                    A::Omitted => DebugActivationReason::Omitted,
                },
                estimated_tokens: entry.estimated_tokens,
                counting_confidence: match entry.counting_confidence {
                    C::High => DebugCountingConfidence::High,
                    C::Approximate => DebugCountingConfidence::Approximate,
                },
                authority_tier: entry.authority_tier,
                truncation_reason: entry.truncation_reason,
            })
            .collect(),
        total_estimated_tokens: report.total_estimated_tokens,
        active_context_estimated_tokens: report.active_context_estimated_tokens,
        overflow_guard_estimated_tokens: report.overflow_guard_estimated_tokens,
        context_window_tokens: report.context_window_tokens,
        context_window_source: report.context_window_source,
        budget_used_percent: report.budget_used_percent,
        pressure_label,
        context_window_verified,
        generated_at: report.generated_at,
        note: report.note,
    }
}
