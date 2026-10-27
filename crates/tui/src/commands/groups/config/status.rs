//! Runtime status command.

use super::policy_messages::{StatusMessages as Messages, StatusText as MessageId};
use codewhale_command_contract::config_policy::*;
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};
use codewhale_command_contract::outcome::ConfigStatusCommandResult as CommandResult;
use codewhale_command_contract::types::{CommandApprovalMode, CommandMode};
use std::borrow::Cow;
use std::fmt::Write as _;

pub const CAPABILITIES: CommandCapabilities =
    CommandCapabilities::CONFIG_STATUS.union(CommandCapabilities::PRESENTATION);
pub struct StatusCmd;
impl RegisterCommand<CommandResult> for StatusCmd {
    fn info() -> &'static CommandInfo {
        &CommandInfo {
            name: "status",
            aliases: &[],
            usage: "/status",
            description_key: "cmd_status_description",
        }
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CAPABILITIES,
            handler: execute,
        }
    }
}
fn tr(messages: &Messages, id: MessageId) -> Cow<'static, str> {
    messages.text(id)
}

/// Show a compact runtime status report for the current TUI session.
pub fn execute(contexts: CommandContexts<'_>, _arg: Option<&str>) -> CommandResult {
    let parts = contexts.into_parts();
    let Some(status) = parts.config_status else {
        return CommandResult::error("Command capability unavailable: config_status");
    };
    let Some(presentation) = parts.presentation else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let messages = match Messages::load(presentation) {
        Ok(messages) => messages,
        Err(error) => return CommandResult::error(error),
    };
    CommandResult::message(format_status(&status.snapshot(), &messages))
}

/// Models.dev live-layer freshness: source, row count, and age (#4187).
fn catalog_summary(view: &ConfigStatusView) -> String {
    let st = &view.catalog;
    let now = view.observed_at;
    let mut out = match st.freshness {
        StatusCatalogFreshness::Bundled => "bundled".to_string(),
        StatusCatalogFreshness::Live => "models.dev live".to_string(),
        StatusCatalogFreshness::Stale => "models.dev stale".to_string(),
        StatusCatalogFreshness::Failed => "models.dev refresh failed".to_string(),
    };
    if st.offering_count > 0 {
        let _ = write!(out, " · {} offerings", st.offering_count);
    }
    if let Some(fetched_at) = st.fetched_at {
        let _ = write!(
            out,
            " · fetched {}",
            codewhale_protocol::cloud_facts::age_label(fetched_at, now)
        );
    }
    if let Some(err) = st.last_error.as_deref().filter(|e| !e.is_empty())
        && st.freshness == StatusCatalogFreshness::Failed
    {
        let _ = write!(out, " ({err})");
    }
    out
}

/// Cloud facts provenance: channel, version, key, age, origin — or why the
/// bundled facts are in use. Off by default.
fn cloud_facts_summary(view: &ConfigStatusView) -> String {
    if view.cloud_facts == codewhale_protocol::cloud_facts::CloudFactsState::Off {
        "off".into()
    } else {
        view.cloud_facts.label(view.observed_at)
    }
}

/// Row label column, in columns. English's widest label is `Context window:`
/// (15); the tail space in [`push_row`] makes its value start at column 19.
/// Longer localized labels extend naturally rather than being truncated.
const LABEL_WIDTH: usize = 16;

fn format_status(view: &ConfigStatusView, locale: &Messages) -> String {
    let mut out = String::new();
    let (context_used, context_max, context_percent) = context_usage(view);

    // A transcript cell has no ink and no rules, so the only grouping mark
    // available is a blank row. It is spent on the two group boundaries and
    // nowhere else: standing facts about the route and the machine first,
    // then everything that accumulates as the session runs.
    let _ = writeln!(out, "codewhale {}", view.version);
    let _ = writeln!(out);

    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelRoute,
        &route_summary(view, locale),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelDirectory,
        &codewhale_protocol::display::display_path_with_home(&view.workspace, view.home.as_deref()),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelProjectDocs,
        &project_docs(&view.project_docs, locale),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelMode,
        &posture_summary(view, locale),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelSafety,
        safety_summary(view, locale).as_ref(),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelMcp,
        &localized(
            locale,
            MessageId::StatusMcpConfigured,
            &[("{count}", &view.mcp_configured_count.to_string())],
        ),
    );
    if let Some(model) = &view.model_pin_drift {
        let notice = localized(
            locale,
            MessageId::StatusModelNotInRoster,
            &[("{model}", model), ("{provider}", &view.provider)],
        );
        let _ = writeln!(out, "  {notice}");
    }
    if let Some(drift) = &view.fleet_drift {
        let value = localized(
            locale,
            MessageId::StatusFleetDrifted,
            &[
                ("{fleet}", &drift.name),
                ("{count}", &drift.ids.len().to_string()),
                ("{ids}", &drift.ids.join(", ")),
            ],
        );
        push_row(&mut out, locale, MessageId::StatusLabelFleet, &value);
    }
    if let Some(notice) = &view.snapshot_notice {
        let _ = writeln!(out, "  {}", snapshot_notice(notice, locale));
    }
    let _ = writeln!(out);

    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelContextWindow,
        &localized(
            locale,
            MessageId::StatusContextUsage,
            &[
                ("{percent}", &format!("{context_percent:.1}")),
                ("{used}", &context_used.to_string()),
                ("{max}", &context_max.to_string()),
            ],
        ),
    );
    let mut source_summary =
        context_window_source_label(context_window_source(view), locale).into_owned();
    // The default bundled source needs no second catalog label. Keeping it
    // compact preserves the 80-column budget as well as the report's row count.
    if view.catalog.freshness != StatusCatalogFreshness::Bundled {
        let _ = write!(
            source_summary,
            " · {}: {}",
            tr(locale, MessageId::StatusLabelCatalog),
            catalog_summary(view)
        );
    }
    let _ = write!(
        source_summary,
        " · {}: {}",
        tr(locale, MessageId::StatusLabelCloudFacts),
        cloud_facts_summary(view)
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelWindowSource,
        &source_summary,
    );
    if let Some(key) = context_window_override_key(view, locale) {
        push_row(&mut out, locale, MessageId::StatusLabelWindowOverride, &key);
    }
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelSession,
        &session_summary(view, locale),
    );
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelSessionTokens,
        &session_tokens(view, locale),
    );
    let cost = view
        .cost_label
        .clone()
        .unwrap_or_else(|| super::money::format_cost_amount_precise(view.cost, view.currency));
    push_row(&mut out, locale, MessageId::StatusLabelSessionCost, &cost);
    // The full, untrimmed session metrics strip (the footer sheds groups to
    // fit; here every group that has evidence is printed). It keeps its own
    // template because the label and metrics form one localized sentence.
    let snapshot = view.metrics;
    if !snapshot.is_empty() {
        let metrics = codewhale_command_contract::metrics::RenderedStrip {
            groups: codewhale_command_contract::metrics::build_groups(
                snapshot,
                &metric_labels(locale),
            ),
            separators: codewhale_command_contract::metrics::Separators::for_ascii(view.ascii_safe),
        }
        .text();
        let _ = writeln!(
            out,
            "  {}",
            tr(locale, MessageId::SessionMetricsStatusLine).replace("{metrics}", &metrics)
        );
    }
    let tool_output_status = &view.tool_outputs;
    push_row(
        &mut out,
        locale,
        MessageId::StatusLabelToolOutputs,
        &codewhale_command_contract::tool_outputs::format_tool_output_status(
            tool_output_status,
            &tool_output_labels(locale),
        ),
    );
    let _ = writeln!(out);
    // Two whole fields left this report rather than being printed at the same
    // weight as everything else: the per-turn token ledger, which `/tokens`
    // already prints in full, and the list of enabled footer item keys, which
    // is `/statusline`'s own subject. The pointer costs one row; they cost
    // seven.
    let _ = writeln!(out, "  {}", tr(locale, MessageId::StatusPointers));

    out
}

/// Provider, model, and effort as one lockup, matching the header rail.
///
/// These were three rows (`Provider:`, `Model:` with the effort parenthesised)
/// for one fact — which route is this turn going to. The header already joins
/// them with a middle dot; `/status` now agrees with it.
fn route_summary(view: &ConfigStatusView, locale: &Messages) -> String {
    let model = view.model.clone();
    let reasoning = view.reasoning.clone();
    localized(
        locale,
        MessageId::StatusRouteSummary,
        &[
            ("{provider}", &view.provider),
            ("{model}", &model),
            ("{reasoning}", &reasoning),
        ],
    )
}

/// Mode and the permissions that qualify it, as one statement of posture.
fn posture_summary(view: &ConfigStatusView, locale: &Messages) -> String {
    let trust = if view.trusted {
        tr(locale, MessageId::StatusTrustedWorkspace)
    } else {
        tr(locale, MessageId::StatusWorkspace)
    };
    let shell = if view.allow_shell {
        tr(locale, MessageId::StatusShellOn)
    } else {
        tr(locale, MessageId::StatusShellOff)
    };
    let mode = tr(
        locale,
        match view.mode {
            CommandMode::Agent => MessageId::AppModeAgent,
            CommandMode::Plan => MessageId::AppModePlan,
            CommandMode::Operate => MessageId::AppModeOperate,
        },
    );
    let approval = approval_summary(view.approval_mode, locale);
    localized(
        locale,
        MessageId::StatusPostureSummary,
        &[
            ("{mode}", mode.as_ref()),
            ("{approval}", approval.as_ref()),
            ("{shell}", shell.as_ref()),
            ("{trust}", trust.as_ref()),
        ],
    )
}

fn approval_summary(mode: CommandApprovalMode, locale: &Messages) -> Cow<'static, str> {
    tr(
        locale,
        match mode {
            CommandApprovalMode::Suggest => MessageId::StatusApprovalAsk,
            CommandApprovalMode::Auto => MessageId::StatusApprovalAuto,
            CommandApprovalMode::Bypass => MessageId::StatusApprovalFullAccess,
            CommandApprovalMode::Never => MessageId::StatusApprovalNever,
        },
    )
}

/// Session identity and the size of the conversation it names.
fn session_summary(view: &ConfigStatusView, locale: &Messages) -> String {
    let session = view
        .session_id
        .clone()
        .unwrap_or_else(|| tr(locale, MessageId::StatusSessionNotSaved).into_owned());
    localized(
        locale,
        MessageId::StatusSessionSummary,
        &[
            ("{session}", &session),
            ("{cells}", &view.history_count.to_string()),
            ("{messages}", &view.message_count.to_string()),
        ],
    )
}

/// Cumulative token ledger on one row.
///
/// The session input/output split and the cumulative cache totals live only
/// here; the per-turn figures they used to sit beside are `/tokens`.
fn session_tokens(view: &ConfigStatusView, locale: &Messages) -> String {
    let cache = if view.cache_hit_tokens == 0 && view.cache_miss_tokens == 0 {
        tr(locale, MessageId::StatusCacheNotReported).into_owned()
    } else {
        localized(
            locale,
            MessageId::StatusCacheSummary,
            &[
                ("{hit}", &view.cache_hit_tokens.to_string()),
                ("{miss}", &view.cache_miss_tokens.to_string()),
            ],
        )
    };
    localized(
        locale,
        MessageId::StatusSessionTokensSummary,
        &[
            ("{input}", &view.input_tokens.to_string()),
            ("{output}", &view.output_tokens.to_string()),
            ("{total}", &view.total_tokens.to_string()),
            ("{cache}", &cache),
        ],
    )
}

fn push_row(out: &mut String, locale: &Messages, label: MessageId, value: &str) {
    let label = format!("{}:", tr(locale, label));
    let _ = writeln!(out, "  {label:<LABEL_WIDTH$} {value}");
}

fn safety_summary(view: &ConfigStatusView, locale: &Messages) -> Cow<'static, str> {
    let id = match view.safety {
        StatusSafety::ReadOnly { enforced: false } => MessageId::StatusSafetyReadOnlyUnenforced,
        StatusSafety::ReadOnly { enforced: true } => MessageId::StatusSafetyReadOnly,
        StatusSafety::WorkspaceWrite {
            enforced: false,
            network_access: true,
        } => MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOn,
        StatusSafety::WorkspaceWrite {
            enforced: false,
            network_access: false,
        } => MessageId::StatusSafetyWorkspaceWriteUnenforcedNetworkOff,
        StatusSafety::WorkspaceWrite {
            enforced: true,
            network_access: true,
        } => MessageId::StatusSafetyWorkspaceWriteNetworkOn,
        StatusSafety::WorkspaceWrite {
            enforced: true,
            network_access: false,
        } => MessageId::StatusSafetyWorkspaceWriteNetworkOff,
        StatusSafety::FullAccess { no_new_privs } => safety_disabled_message(no_new_privs),
        StatusSafety::External => MessageId::StatusSafetyExternal,
    };
    tr(locale, id)
}

/// The full-access safety row must disclose the residual setuid block
/// truthfully (#5723): the no-new-privileges kernel flag is set at startup in
/// every narrower posture and is irreversible, so "sandbox disabled" alone
/// would promise `sudo`/setuid workflows the process tree cannot perform.
/// `None` is a platform without the flag, where the plain label is accurate.
pub fn safety_disabled_message(no_new_privs_active: Option<bool>) -> MessageId {
    match no_new_privs_active {
        Some(true) => MessageId::StatusSafetyDisabledSetuidBlocked,
        Some(false) => MessageId::StatusSafetyDisabledSetuidAllowed,
        None => MessageId::StatusSafetyDisabled,
    }
}

fn project_docs(docs: &[String], locale: &Messages) -> String {
    if docs.is_empty() {
        tr(locale, MessageId::StatusProjectDocsNone).into_owned()
    } else {
        docs.join(", ")
    }
}

fn context_usage(view: &ConfigStatusView) -> (usize, u32, f64) {
    let used = view.context_used;
    let max = view.context_window;
    (
        used,
        max,
        ((used as f64 / f64::from(max)) * 100.0).clamp(0.0, 100.0),
    )
}

/// Where the effective context window came from.
///
/// #5134: `/status` printed the window as a bare number, so a user watching
/// auto-compaction fire at 128K on a 1M-capable model had no way to learn that
/// `context_window` exists, let alone which table it belongs on. The
/// provenance label alone is not enough — the actionable half is the key path,
/// which now gets its own aligned row rather than a parenthesis that wrapped
/// the provenance off the end of the line.
fn context_window_source(view: &ConfigStatusView) -> StatusContextSource {
    view.context_source
}

fn context_window_source_label(
    source: StatusContextSource,
    locale: &Messages,
) -> Cow<'static, str> {
    tr(
        locale,
        match source {
            StatusContextSource::Configured | StatusContextSource::UserDeclared => {
                MessageId::StatusContextSourceConfigured
            }
            StatusContextSource::ConfiguredModel => MessageId::StatusContextSourceConfiguredModel,
            StatusContextSource::ProviderReported => MessageId::StatusContextSourceProviderReported,
            StatusContextSource::StaticKimiCodeSafeFloor => {
                MessageId::StatusContextSourceKimiSafeFloor
            }
            StatusContextSource::Catalog => MessageId::StatusContextSourceCatalog,
            StatusContextSource::NameSuffixHint => MessageId::StatusContextSourceModelHint,
            StatusContextSource::Fallback => MessageId::StatusContextSourceFallback,
        },
    )
}

/// The exact key that changes the window, or `None` when the user already set
/// it and the row would be naming a key they have already used.
fn context_window_override_key(view: &ConfigStatusView, locale: &Messages) -> Option<String> {
    view.window_override.as_ref().map(|key| match key {
        StatusWindowOverride::Provider(table) => localized(
            locale,
            MessageId::StatusWindowOverrideProvider,
            &[("{table}", table)],
        ),
        StatusWindowOverride::ActiveProvider => {
            tr(locale, MessageId::StatusWindowOverrideActiveProvider).into_owned()
        }
    })
}

fn localized(locale: &Messages, id: MessageId, replacements: &[(&str, &str)]) -> String {
    super::interpolate(&tr(locale, id), replacements)
}

fn snapshot_notice(notice: &StatusSnapshotNotice, locale: &Messages) -> String {
    let id = match notice.scope {
        StatusSnapshotScope::WorkspaceTooLarge => MessageId::SnapshotsDisabledTooLarge,
        StatusSnapshotScope::TooManyFiles => MessageId::SnapshotsDisabledTooManyFiles,
        StatusSnapshotScope::UnsafeLocation => MessageId::SnapshotsDisabledUnsafeLocation,
        StatusSnapshotScope::HistoryRepaired => MessageId::SnapshotsHistoryRepaired,
        StatusSnapshotScope::Failing => MessageId::SnapshotsFailing,
    };
    notice.render(&tr(locale, id))
}

fn metric_labels(locale: &Messages) -> codewhale_command_contract::metrics::MetricLabels {
    codewhale_command_contract::metrics::MetricLabels {
        turn: tr(locale, MessageId::SessionMetricsTurn).into_owned(),
        turns: tr(locale, MessageId::SessionMetricsTurns).into_owned(),
        step: tr(locale, MessageId::SessionMetricsStep).into_owned(),
        steps: tr(locale, MessageId::SessionMetricsSteps).into_owned(),
        llm: tr(locale, MessageId::SessionMetricsLlm).into_owned(),
        tools: tr(locale, MessageId::SessionMetricsTools).into_owned(),
        ttft: tr(locale, MessageId::SessionMetricsTtft).into_owned(),
        tokens_per_second: tr(locale, MessageId::SessionMetricsTokensPerSecond).into_owned(),
        cache: tr(locale, MessageId::SessionMetricsCache).into_owned(),
        input: tr(locale, MessageId::SessionMetricsInput).into_owned(),
    }
}
fn tool_output_labels(
    locale: &Messages,
) -> codewhale_command_contract::tool_outputs::ToolOutputLabels {
    codewhale_command_contract::tool_outputs::ToolOutputLabels {
        raw_pressure: tr(locale, MessageId::StatusToolRawPressure).into_owned(),
        compact_receipts: tr(locale, MessageId::StatusToolCompactReceipts).into_owned(),
        artifacts: tr(locale, MessageId::StatusToolArtifacts).into_owned(),
        none: tr(locale, MessageId::StatusToolNone).into_owned(),
    }
}
