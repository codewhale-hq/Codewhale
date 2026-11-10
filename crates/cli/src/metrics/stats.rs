use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use codewhale_engine::cost_status::{UsageSourceContribution, usage_source_fingerprint};
use codewhale_engine::llm_client::ProviderUsageCoverage;
use codewhale_engine::runtime_threads::{
    RuntimeDiagnosticRecord, RuntimeEventRecord, RuntimeThreadStore, ThreadRecord, TurnItemKind,
    TurnItemLifecycleStatus, TurnItemRecord, TurnRecord,
};
use codewhale_engine::session_manager::{
    SESSION_METADATA_READ_LIMIT, SessionManager, SessionMetadata,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::MetricsArgs;

#[derive(Debug, Default, Serialize)]
pub(super) struct InspectionCoverage {
    pub gaps: BTreeMap<String, u64>,
    pub unknown_fields: BTreeSet<String>,
    pub inherited_runtime_turns_excluded: u64,
    pub source_copies_deduplicated: u64,
    pub source_conflicts: u64,
    pub project_ownership_ambiguous_sources: u64,
    pub legacy_session_projections: u64,
    pub unlinked_runtime_threads: u64,
}

impl InspectionCoverage {
    pub(super) fn gap(&mut self, kind: &str) {
        let count = self.gaps.entry(kind.to_string()).or_default();
        *count = count.saturating_add(1);
    }
}

#[derive(Debug, Default, Serialize)]
pub(super) struct MoneyUsage {
    pub identified_sources: u64,
    pub reported_usage_sources: u64,
    pub incomplete_usage_sources: u64,
    pub missing_usage_sources: u64,
    pub not_dispatched_sources: u64,
    pub rejected_sources: u64,
    pub zero_usage_presence_ambiguous_sources: u64,
    pub observed_physical_attempts: u64,
    pub omitted_physical_attempt_rows: u64,
    pub input_tokens_known: u64,
    pub output_tokens_known: u64,
    pub cache_read_tokens_known: Option<u64>,
    pub cache_miss_tokens_known: Option<u64>,
    pub cache_write_tokens_known: Option<u64>,
    pub cache_read_coverage_sources: u64,
    pub inconsistent_cache_partition_sources: u64,
    pub cache_input_partition_known: u64,
    pub reasoning_tokens_known: Option<u64>,
    pub reasoning_coverage_sources: u64,
    pub reasoning_replay_tokens_known: Option<u64>,
    pub reasoning_replay_coverage_sources: u64,
    pub known_subtotal_usd: f64,
    pub known_subtotal_cny: f64,
    pub usd_price_unknown_sources: u64,
    pub cny_price_unknown_sources: u64,
    pub quote_unavailable_sources: u64,
    pub cache_hit_percent_of_reported_subset: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct SessionOverview {
    pub saved_documents: u64,
    pub messages_in_saved_documents: u64,
    pub runtime_threads: u64,
    pub runtime_turns: u64,
    pub runtime_message_items: u64,
    pub average_saved_session_tokens: Option<f64>,
    pub median_saved_session_tokens: Option<f64>,
    pub unique_conversations: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct LegacyDisplay {
    pub document: String,
    pub workspace: String,
    pub known_session_and_agent_usd: f64,
    pub known_session_and_agent_cny: f64,
    pub global_charge_identity_unproved: bool,
    pub recorded_session_coverage: bool,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct ToolReport {
    pub identified_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub unknown_outcomes: u64,
    pub retained_text_token_estimate_known: Option<u64>,
    pub retained_text_samples: u64,
    pub original_output_bytes_known: Option<u64>,
    pub truncated_samples: u64,
    pub spooled_samples: u64,
    pub output_shape_unknown_executions: u64,
    pub failure_categories: BTreeMap<String, u64>,
    pub exact_error_signature_recovery_rate: Option<f64>,
    pub failure_rate_percent: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct CompactionReport {
    pub completed_items: u64,
    pub before_messages_known: Option<u64>,
    pub after_messages_known: Option<u64>,
    pub summarizer_input_tokens_known: Option<u64>,
    pub summarizer_output_tokens_known: Option<u64>,
    pub attributable_cost_usd: Option<f64>,
    pub attributable_cost_cny: Option<f64>,
    pub history_tokens_dropped: Option<u64>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct RoleReport {
    pub recorded_tasks: u64,
    pub usage_report_samples: Option<u64>,
    pub input_tokens_known: Option<u64>,
    pub output_tokens_known: Option<u64>,
    pub task_states: BTreeMap<String, u64>,
    pub failure_sources: BTreeMap<String, u64>,
    pub known_cost_usd: Option<f64>,
    pub known_cost_cny: Option<f64>,
    pub recovery_rate: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct StatsReport {
    pub since: Option<DateTime<Utc>>,
    pub project: Option<String>,
    pub workspace_ids: BTreeSet<String>,
    pub overview: SessionOverview,
    pub accounting: Option<MoneyUsage>,
    pub models: BTreeMap<String, MoneyUsage>,
    pub legacy_display_subtotals_not_added_to_accounting: Vec<LegacyDisplay>,
    pub latest_prepared_request_components: Option<Value>,
    pub tools: BTreeMap<String, ToolReport>,
    pub unattributed_tool_receipts: u64,
    pub compaction: CompactionReport,
    pub roles: BTreeMap<String, RoleReport>,
    pub conflicting_source_views_not_added_to_accounting: Vec<Value>,
    pub coverage: InspectionCoverage,
    pub historical_coverage_complete: bool,
    pub accounting_basis: &'static str,
    pub project_filter_basis: &'static str,
    pub legacy_receipt_projections_included: bool,
}

#[derive(Serialize)]
struct SourceEntry {
    contribution: UsageSourceContribution,
    conflict: bool,
}

#[derive(Clone, Serialize)]
struct ThreadScope {
    workspace: PathBuf,
}

#[derive(Clone, Serialize)]
struct TurnScope {
    workspace: Option<PathBuf>,
    inherited: bool,
}

#[derive(Default, Serialize)]
struct ToolObservation {
    name: String,
    outcome: Option<bool>,
    history_estimate: Option<u64>,
    original_bytes: Option<u64>,
    truncated: Option<bool>,
    spooled: Option<bool>,
    category: Option<String>,
    identity_conflicted: bool,
    outcome_conflicted: bool,
    history_conflicted: bool,
}

struct Collector<'a> {
    args: &'a MetricsArgs,
    report: StatsReport,
    sources: BTreeMap<String, BTreeMap<u64, SourceEntry>>,
    tools: BTreeMap<String, ToolObservation>,
    threads: BTreeMap<String, ThreadScope>,
    turns: BTreeMap<String, TurnScope>,
    workspaces: BTreeSet<PathBuf>,
    session_tokens: Vec<u64>,
    retained_bytes: usize,
    compaction_items: BTreeSet<String>,
    compaction_details: BTreeSet<String>,
}

fn path_id(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn scoped(root: &Path, id: &str) -> String {
    format!("{}:{id}", usage_source_fingerprint(&path_id(root)))
}

fn checked_sum(total: &mut u64, value: u64) -> Result<()> {
    *total = total
        .checked_add(value)
        .context("Stats token/count overflow; report refused")?;
    Ok(())
}

fn optional_sum(total: &mut Option<u64>, value: u64) -> Result<()> {
    let next = total
        .unwrap_or(0)
        .checked_add(value)
        .context("Stats optional count overflow; report refused")?;
    *total = Some(next);
    Ok(())
}

fn money_sum(total: &mut f64, value: f64) -> Result<()> {
    let next = *total + value;
    if !value.is_finite() || value < 0.0 || !next.is_finite() {
        bail!("Stats invalid/overflowed monetary subtotal; report refused");
    }
    *total = next;
    Ok(())
}

impl MoneyUsage {
    fn add(&mut self, contribution: &UsageSourceContribution) -> Result<()> {
        contribution.validate()?;
        let record = &contribution.record;
        let revision = record
            .settlement
            .as_ref()
            .context("Stats source has no native revision")?;
        checked_sum(&mut self.identified_sources, 1)?;
        checked_sum(
            &mut self.observed_physical_attempts,
            revision.physical_requests.len() as u64,
        )?;
        checked_sum(
            &mut self.omitted_physical_attempt_rows,
            revision.physical_requests_dropped,
        )?;
        let mut usage_known = match revision.coverage {
            ProviderUsageCoverage::Reported => {
                checked_sum(&mut self.reported_usage_sources, 1)?;
                true
            }
            ProviderUsageCoverage::Incomplete => {
                checked_sum(&mut self.incomplete_usage_sources, 1)?;
                true
            }
            ProviderUsageCoverage::Missing => {
                checked_sum(&mut self.missing_usage_sources, 1)?;
                false
            }
            ProviderUsageCoverage::NotDispatched => {
                checked_sum(&mut self.not_dispatched_sources, 1)?;
                false
            }
            ProviderUsageCoverage::Rejected => {
                checked_sum(&mut self.rejected_sources, 1)?;
                false
            }
        };
        let usage = &record.usage.usage;
        let zero_presence_ambiguous = usage_known
            && usage.input_tokens == 0
            && usage.output_tokens == 0
            && [
                usage.prompt_cache_hit_tokens,
                usage.prompt_cache_miss_tokens,
                usage.prompt_cache_write_tokens,
                usage.reasoning_tokens,
                usage.input_audio_tokens,
                usage.output_audio_tokens,
                usage.reasoning_replay_tokens,
            ]
            .into_iter()
            .flatten()
            .all(|tokens| tokens == 0)
            && usage
                .request_usages
                .iter()
                .all(|usage| usage.input_tokens == 0 && usage.output_tokens == 0);
        if zero_presence_ambiguous {
            checked_sum(&mut self.zero_usage_presence_ambiguous_sources, 1)?;
            usage_known = false;
        }
        let zero_money_unknown = zero_presence_ambiguous
            && record
                .usage
                .route
                .audit_frozen(&record.usage.usage)
                .unpriced_reason
                .is_none_or(|reason| reason.counts_toward_money_coverage());
        if usage_known {
            let usage = &record.usage.usage;
            checked_sum(&mut self.input_tokens_known, u64::from(usage.input_tokens))?;
            checked_sum(
                &mut self.output_tokens_known,
                u64::from(usage.output_tokens),
            )?;
            let classes = codewhale_engine::pricing::token_usage_for_pricing(usage);
            let cache_consistent =
                codewhale_engine::pricing::usage_cache_partition_is_consistent(usage);
            if !cache_consistent {
                checked_sum(&mut self.inconsistent_cache_partition_sources, 1)?;
            }
            if cache_consistent && usage.prompt_cache_hit_tokens.is_some() {
                optional_sum(&mut self.cache_read_tokens_known, classes.cache_read)?;
                checked_sum(&mut self.cache_read_coverage_sources, 1)?;
                checked_sum(
                    &mut self.cache_input_partition_known,
                    classes
                        .cache_read
                        .saturating_add(classes.input)
                        .saturating_add(classes.cache_write),
                )?;
            }
            if cache_consistent && let Some(miss) = usage.prompt_cache_miss_tokens {
                optional_sum(&mut self.cache_miss_tokens_known, u64::from(miss))?;
            }
            if cache_consistent && usage.prompt_cache_write_tokens.is_some() {
                optional_sum(&mut self.cache_write_tokens_known, classes.cache_write)?;
            }
            if let Some(reasoning) = usage.reasoning_tokens {
                optional_sum(&mut self.reasoning_tokens_known, u64::from(reasoning))?;
                checked_sum(&mut self.reasoning_coverage_sources, 1)?;
            }
            if let Some(replay) = usage.reasoning_replay_tokens {
                optional_sum(&mut self.reasoning_replay_tokens_known, u64::from(replay))?;
                checked_sum(&mut self.reasoning_replay_coverage_sources, 1)?;
            }
        }
        money_sum(&mut self.known_subtotal_usd, contribution.usd)?;
        money_sum(&mut self.known_subtotal_cny, contribution.cny)?;
        checked_sum(
            &mut self.usd_price_unknown_sources,
            u64::from(contribution.unpriced > 0 || zero_money_unknown),
        )?;
        checked_sum(
            &mut self.cny_price_unknown_sources,
            u64::from(contribution.cny_unpriced > 0 || zero_money_unknown),
        )?;
        if record.usage.route.provider_live_pricing.is_none() {
            checked_sum(&mut self.quote_unavailable_sources, 1)?;
        }
        self.cache_hit_percent_of_reported_subset =
            (self.cache_input_partition_known > 0).then(|| {
                100.0 * self.cache_read_tokens_known.unwrap_or(0) as f64
                    / self.cache_input_partition_known as f64
            });
        Ok(())
    }
}

impl<'a> Collector<'a> {
    fn new(args: &'a MetricsArgs) -> Self {
        let mut report = StatsReport {
            since: args.since,
            project: args.project.as_deref().map(path_id),
            historical_coverage_complete: false,
            accounting: args.project.is_none().then(MoneyUsage::default),
            accounting_basis: "identified persisted native source contributions; known estimates are subtotals, not invoice charges; receipt, transcript, Fleet and legacy display projections are not added again",
            project_filter_basis: "current saved/thread/workspace-ledger views; native money and model subtotals unavailable without captured original source workspace",
            legacy_receipt_projections_included: args.project.is_none(),
            ..StatsReport::default()
        };
        for field in [
            "complete historical coverage/retirement",
            "legacy source contributions and historical quotes",
            "explicit all-zero transport usage presence",
            "cache warm/cold classification and churn causes",
            "latest prepared-request component capture",
            "tool bytes actually replayed on each provider request",
            "exact repeated-error signature/recovery linkage",
            "compaction source-to-money linkage",
            "Fleet role-to-native-money linkage",
            "Fleet worker/run attempt lifecycle and recovery linkage",
        ] {
            report.coverage.unknown_fields.insert(field.to_string());
        }
        Self {
            args,
            report,
            sources: BTreeMap::new(),
            tools: BTreeMap::new(),
            threads: BTreeMap::new(),
            turns: BTreeMap::new(),
            workspaces: BTreeSet::new(),
            session_tokens: Vec::new(),
            retained_bytes: 0,
            compaction_items: BTreeSet::new(),
            compaction_details: BTreeSet::new(),
        }
    }

    fn retain<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let size = serde_json::to_vec(value)?
            .len()
            .checked_add(128)
            .context("Stats retained size overflow")?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(size)
            .context("Stats retained size overflow")?;
        if self.retained_bytes > SESSION_METADATA_READ_LIMIT {
            bail!(
                "Stats retained inspection index exceeds the 16 MiB session inspection limit; no receipt was evicted; narrow --days"
            );
        }
        Ok(())
    }

    fn in_window(&self, timestamp: DateTime<Utc>) -> bool {
        self.args.since.is_none_or(|cutoff| timestamp >= cutoff)
    }

    fn in_project(&self, workspace: Option<&Path>) -> bool {
        self.args.project.as_deref().is_none_or(|expected| {
            workspace.is_some_and(|workspace| path_id(workspace) == path_id(expected))
        })
    }

    fn remember_workspace(&mut self, workspace: &Path) -> Result<()> {
        if !self.workspaces.contains(workspace) {
            self.retain(&(workspace, path_id(workspace)))?;
            self.workspaces.insert(workspace.to_path_buf());
            self.report.workspace_ids.insert(path_id(workspace));
        }
        Ok(())
    }

    fn source(&mut self, contribution: UsageSourceContribution) -> Result<()> {
        if contribution.validate().is_err() {
            self.report.coverage.gap("invalid_source_contribution");
            return Ok(());
        }
        let record = &contribution.record;
        let Some(revision) = record.settlement.as_ref() else {
            self.report.coverage.gap("legacy_unidentified_source");
            return Ok(());
        };
        if !self.in_window(record.usage.route.dispatched_at) {
            return Ok(());
        }
        let key = format!(
            "{}:{}",
            usage_source_fingerprint(&record.source_id),
            revision.lineage()
        );
        let revision = revision.revision();
        if let Some(old) = self
            .sources
            .get_mut(&key)
            .and_then(|versions| versions.get_mut(&revision))
        {
            checked_sum(&mut self.report.coverage.source_copies_deduplicated, 1)?;
            if old.contribution.record != contribution.record {
                old.conflict = true;
            }
        } else {
            let entry = SourceEntry {
                contribution,
                conflict: false,
            };
            self.retain(&entry)?;
            self.sources.entry(key).or_default().insert(revision, entry);
        }
        Ok(())
    }

    fn session(&mut self, root: &Path, metadata: SessionMetadata) -> Result<()> {
        self.remember_workspace(&metadata.workspace)?;
        if self.in_project(Some(&metadata.workspace)) && self.in_window(metadata.updated_at) {
            checked_sum(&mut self.report.overview.saved_documents, 1)?;
            checked_sum(
                &mut self.report.overview.messages_in_saved_documents,
                metadata.message_count as u64,
            )?;
            self.retain(&metadata.total_tokens)?;
            self.session_tokens.push(metadata.total_tokens);
            let display = LegacyDisplay {
                document: usage_source_fingerprint(&scoped(root, &metadata.id)),
                workspace: path_id(&metadata.workspace),
                known_session_and_agent_usd: metadata.cost.session_cost_usd
                    + metadata.cost.subagent_cost_usd,
                known_session_and_agent_cny: metadata.cost.session_cost_cny
                    + metadata.cost.subagent_cost_cny,
                global_charge_identity_unproved: true,
                recorded_session_coverage: metadata.cost.coverage_recorded,
            };
            if !display.known_session_and_agent_usd.is_finite()
                || !display.known_session_and_agent_cny.is_finite()
                || display.known_session_and_agent_usd < 0.0
                || display.known_session_and_agent_cny < 0.0
            {
                self.report.coverage.gap("invalid_legacy_display_subtotal");
            } else {
                self.retain(&display)?;
                self.report
                    .legacy_display_subtotals_not_added_to_accounting
                    .push(display);
            }
            if !metadata.cost.coverage_recorded {
                checked_sum(&mut self.report.coverage.legacy_session_projections, 1)?;
            }
            if metadata.cost.missing_usage_overflowed || !metadata.cost.unpriced_reasons.is_empty()
            {
                self.report.coverage.gap("session_usage_coverage_gap");
            }
        }
        for contribution in metadata.cost.accounted_usage_sources.into_values() {
            self.source(contribution)?;
        }
        Ok(())
    }

    fn runtime(&mut self, root: &Path, record: RuntimeDiagnosticRecord) -> Result<()> {
        match record {
            RuntimeDiagnosticRecord::Thread(thread) => self.thread(root, *thread),
            RuntimeDiagnosticRecord::Turn(turn) => self.turn(root, *turn),
            RuntimeDiagnosticRecord::Item(item) => self.item(root, *item),
            RuntimeDiagnosticRecord::Event(event) => self.event(root, *event),
        }
    }

    fn thread(&mut self, root: &Path, thread: ThreadRecord) -> Result<()> {
        self.remember_workspace(&thread.workspace)?;
        if self.in_project(Some(&thread.workspace)) && self.in_window(thread.updated_at) {
            checked_sum(&mut self.report.overview.runtime_threads, 1)?;
        }
        if thread.session_id.is_none() {
            checked_sum(&mut self.report.coverage.unlinked_runtime_threads, 1)?;
        }
        let row = ThreadScope {
            workspace: thread.workspace,
        };
        self.retain(&row)?;
        self.threads.insert(scoped(root, &thread.id), row);
        Ok(())
    }

    fn tool(&mut self, execution: Option<&str>, row: ToolObservation) -> Result<()> {
        let Some(execution) = execution.filter(|execution| !execution.is_empty()) else {
            checked_sum(&mut self.report.unattributed_tool_receipts, 1)?;
            return Ok(());
        };
        let key = usage_source_fingerprint(execution);
        if let Some(old) = self.tools.get_mut(&key) {
            if old.name == "unknown" && row.name != "unknown" {
                old.name = row.name.clone();
            } else if row.name != "unknown" && old.name != row.name {
                old.identity_conflicted = true;
                self.report.coverage.gap("tool_execution_identity_conflict");
            }
            if old.outcome.is_some() && row.outcome.is_some() && old.outcome != row.outcome {
                old.outcome_conflicted = true;
                self.report.coverage.gap("tool_execution_outcome_conflict");
            } else if row.outcome.is_some() {
                old.outcome = row.outcome;
            }
            if row.history_estimate.is_some() {
                if old.history_estimate.is_some() && old.history_estimate != row.history_estimate {
                    old.history_conflicted = true;
                    self.report.coverage.gap("tool_retained_projection_differs");
                } else {
                    old.history_estimate = row.history_estimate;
                }
            }
            old.original_bytes = old.original_bytes.or(row.original_bytes);
            old.truncated = old.truncated.or(row.truncated);
            old.spooled = old.spooled.or(row.spooled);
            old.category = old.category.take().or(row.category);
        } else {
            self.retain(&row)?;
            self.tools.insert(key, row);
        }
        Ok(())
    }

    fn item(&mut self, root: &Path, item: TurnItemRecord) -> Result<()> {
        let Some(scope) = self.turns.get(&scoped(root, &item.turn_id)) else {
            self.report.coverage.gap("runtime_item_turn_unavailable");
            return Ok(());
        };
        if scope.inherited || !self.in_project(scope.workspace.as_deref()) {
            return Ok(());
        }
        let timestamp = item.started_at.or(item.ended_at);
        if self.args.since.is_some() && timestamp.is_none() {
            self.report
                .coverage
                .gap("runtime_item_timestamp_unavailable");
            return Ok(());
        }
        if timestamp.is_some_and(|time| !self.in_window(time)) {
            return Ok(());
        }
        if matches!(
            item.kind,
            TurnItemKind::UserMessage | TurnItemKind::AgentMessage
        ) {
            checked_sum(&mut self.report.overview.runtime_message_items, 1)?;
            return Ok(());
        }
        if item.kind == TurnItemKind::ContextCompaction {
            if item.status == TurnItemLifecycleStatus::Completed {
                let key = scoped(root, &item.id);
                if !self.compaction_items.contains(&key) {
                    self.retain(&key)?;
                    self.compaction_items.insert(key);
                    checked_sum(&mut self.report.compaction.completed_items, 1)?;
                }
            }
            return Ok(());
        }
        if !matches!(
            item.kind,
            TurnItemKind::ToolCall | TurnItemKind::FileChange | TurnItemKind::CommandExecution
        ) {
            return Ok(());
        }
        let metadata = item.metadata.as_ref();
        let field = |name| metadata.and_then(|metadata| metadata.get(name));
        let result = field("tool_result_for").is_some();
        let outcome = field("is_error")
            .and_then(Value::as_bool)
            .map(|error| !error)
            .or_else(|| (item.status == TurnItemLifecycleStatus::Failed).then_some(false));
        let detail = item.detail.as_deref();
        let row = ToolObservation {
            name: label(
                field("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            ),
            outcome,
            history_estimate: result.then(|| detail.map(history_tokens)).flatten(),
            original_bytes: field("original_byte_count").and_then(Value::as_u64),
            truncated: field("truncated").and_then(Value::as_bool),
            spooled: field("spooled").and_then(Value::as_bool).or_else(|| {
                field("spillover_path")
                    .and_then(Value::as_str)
                    .map(|_| true)
            }),
            category: (outcome == Some(false))
                .then(|| detail.and_then(failure_category))
                .flatten(),
            ..ToolObservation::default()
        };
        self.tool(field("execution_id").and_then(Value::as_str), row)
    }

    fn event(&mut self, root: &Path, event: RuntimeEventRecord) -> Result<()> {
        if event.event != "item.completed" {
            return Ok(());
        }
        let Some(turn) = event
            .turn_id
            .as_ref()
            .and_then(|id| self.turns.get(&scoped(root, id)))
        else {
            return Ok(());
        };
        if turn.inherited
            || !self.in_project(turn.workspace.as_deref())
            || !self.in_window(event.timestamp)
        {
            return Ok(());
        }
        if event.payload.pointer("/item/kind").and_then(Value::as_str) != Some("context_compaction")
        {
            return Ok(());
        }
        let Some(id) = event.item_id.as_deref() else {
            self.report.coverage.gap("compaction_identity_unavailable");
            return Ok(());
        };
        let key = scoped(root, id);
        if self.compaction_details.contains(&key) {
            return Ok(());
        }
        self.retain(&key)?;
        self.compaction_details.insert(key.clone());
        if self.compaction_items.insert(key) {
            checked_sum(&mut self.report.compaction.completed_items, 1)?;
        }
        if let Some(before) = event.payload.get("messages_before").and_then(Value::as_u64) {
            optional_sum(&mut self.report.compaction.before_messages_known, before)?;
        }
        if let Some(after) = event.payload.get("messages_after").and_then(Value::as_u64) {
            optional_sum(&mut self.report.compaction.after_messages_known, after)?;
        }
        Ok(())
    }

    fn session_tools(&mut self, value: &Value, metadata: &SessionMetadata) -> Result<()> {
        use codewhale_runtime::session_tree::{SessionEntryKind, SessionJournal};
        if !self.in_project(Some(&metadata.workspace)) {
            return Ok(());
        }
        let mut names = BTreeMap::new();
        if let Some(journal) = value.get("journal").filter(|journal| !journal.is_null()) {
            let journal: SessionJournal = match serde_json::from_value(journal.clone()) {
                Ok(journal) => journal,
                Err(_) => {
                    self.report.coverage.gap("session_tool_journal_unreadable");
                    return Ok(());
                }
            };
            if journal.validate().is_err() {
                self.report.coverage.gap("session_tool_journal_invalid");
                return Ok(());
            }
            for entry in journal.root_to_leaf() {
                if let SessionEntryKind::Message { message } = entry.kind.as_ref() {
                    self.session_message_tools(
                        &serde_json::to_value(message)?,
                        Some(entry.created_at),
                        &mut names,
                    )?;
                }
            }
            return Ok(());
        }
        if let Some(messages) = value.get("messages").and_then(Value::as_array) {
            for message in messages {
                self.session_message_tools(message, None, &mut names)?;
            }
        }
        Ok(())
    }

    fn session_message_tools(
        &mut self,
        message: &Value,
        timestamp: Option<DateTime<Utc>>,
        names: &mut BTreeMap<String, String>,
    ) -> Result<()> {
        let include = match timestamp {
            Some(timestamp) => self.in_window(timestamp),
            None if self.args.since.is_some() => {
                self.report
                    .coverage
                    .gap("legacy_tool_timestamp_unavailable");
                false
            }
            None => true,
        };
        for block in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if let Some(execution) = block.get("execution_id").and_then(Value::as_str) {
                        let name = label(
                            block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown"),
                        );
                        names.insert(execution.to_string(), name.clone());
                        if include {
                            self.tool(
                                Some(execution),
                                ToolObservation {
                                    name,
                                    ..Default::default()
                                },
                            )?;
                        }
                    } else if include {
                        checked_sum(&mut self.report.unattributed_tool_receipts, 1)?;
                    }
                }
                Some("tool_result") if include => {
                    let execution = block.get("execution_id").and_then(Value::as_str);
                    let name = execution
                        .and_then(|id| names.get(id))
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    let content = block.get("content").and_then(Value::as_str);
                    let outcome = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .map(|error| !error);
                    self.tool(
                        execution,
                        ToolObservation {
                            name,
                            outcome,
                            history_estimate: content.map(history_tokens),
                            category: (outcome == Some(false))
                                .then(|| content.and_then(failure_category))
                                .flatten(),
                            ..Default::default()
                        },
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn fleet(&mut self, workspace: &Path) -> Result<()> {
        use codewhale_engine::fleet::ledger::{FleetLedger, FleetLedgerState, apply_record};
        if !self.in_project(Some(workspace)) {
            return Ok(());
        }
        let mut state = FleetLedgerState::default();
        let mut replay_bytes = 0usize;
        let result = FleetLedger::visit_diagnostic_records(workspace, |record| {
            let record = record?;
            replay_bytes = replay_bytes
                .checked_add(serde_json::to_vec(&record)?.len())
                .context("Stats Fleet inspection size overflow")?;
            if replay_bytes > SESSION_METADATA_READ_LIMIT {
                bail!("Stats Fleet replay exceeds the bounded inspection limit");
            }
            apply_record(&mut state, record);
            Ok(())
        });
        if result.is_err() {
            self.report
                .coverage
                .gap("fleet_replay_unavailable_or_inspection_limit");
            return Ok(());
        }
        for task in state.tasks.values() {
            let timestamp = chrono::DateTime::parse_from_rfc3339(&task.entry.enqueued_at)
                .ok()
                .map(|time| time.with_timezone(&Utc));
            if self.args.since.is_some() && timestamp.is_none() {
                self.report.coverage.gap("fleet_task_timestamp_unavailable");
                continue;
            }
            if timestamp.is_some_and(|time| !self.in_window(time)) {
                continue;
            }
            let run = state.runs.get(&task.entry.run_id.0);
            let role = run
                .and_then(|run| {
                    run.task_specs
                        .iter()
                        .find(|spec| spec.id == task.entry.task_id)
                })
                .and_then(|spec| spec.worker.as_ref())
                .and_then(|worker| worker.role.as_deref());
            let name = label(role.unwrap_or("unknown-role"));
            if role.is_none() {
                self.report.coverage.gap("fleet_task_role_unavailable");
            }
            let status = serde_json::to_value(task.status)?
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let receipt = state.receipts.values().find(|receipt| {
                receipt.run_id == task.entry.run_id && receipt.task_id == task.entry.task_id
            });
            let failure = receipt
                .and_then(|receipt| receipt.failure_kind.as_ref())
                .map(serde_json::to_value)
                .transpose()?;
            let failure = failure.as_ref().and_then(Value::as_str).map(label);
            if !self.report.roles.contains_key(&name) {
                self.retain(&name)?;
            }
            let report = self.report.roles.entry(name).or_default();
            checked_sum(&mut report.recorded_tasks, 1)?;
            checked_sum(report.task_states.entry(status).or_default(), 1)?;
            if let Some(failure) = failure {
                checked_sum(report.failure_sources.entry(failure).or_default(), 1)?;
            }
        }
        Ok(())
    }

    fn turn(&mut self, root: &Path, turn: TurnRecord) -> Result<()> {
        let workspace = self
            .threads
            .get(&scoped(root, &turn.thread_id))
            .map(|scope| scope.workspace.clone());
        let inherited = turn
            .source_usage_origin_turn_id
            .as_deref()
            .is_some_and(|original| original != turn.id);
        let row = TurnScope {
            workspace: workspace.clone(),
            inherited,
        };
        self.retain(&row)?;
        self.turns.insert(scoped(root, &turn.id), row);
        if codewhale_engine::runtime_threads::runtime_usage_partition_parent(&turn).is_none()
            && self.in_project(workspace.as_deref())
            && self.in_window(turn.created_at)
        {
            checked_sum(&mut self.report.overview.runtime_turns, 1)?;
        }
        if inherited {
            if codewhale_engine::runtime_threads::runtime_usage_partition_parent(&turn).is_none() {
                checked_sum(
                    &mut self.report.coverage.inherited_runtime_turns_excluded,
                    1,
                )?;
            }
            return Ok(());
        }
        if !turn.source_accounting || turn.routed_usage_dropped_records > 0 {
            self.report.coverage.gap("runtime_usage_coverage_gap");
        }
        for (key, record) in turn.source_usage_records {
            if key != usage_source_fingerprint(&record.source_id) {
                self.report.coverage.gap("runtime_source_key_mismatch");
                continue;
            }
            self.source(UsageSourceContribution::for_record(&record))?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<StatsReport> {
        for (source, versions) in &self.sources {
            let mut selected: Option<&SourceEntry> = None;
            let mut conflict = false;
            for entry in versions.values() {
                conflict |= entry.conflict;
                if let Some(previous) = selected {
                    let prior_revision = previous
                        .contribution
                        .record
                        .settlement
                        .as_ref()
                        .map(|revision| revision.revision());
                    let current_revision = entry
                        .contribution
                        .record
                        .settlement
                        .as_ref()
                        .map(|revision| revision.revision());
                    conflict |= prior_revision.and_then(|revision| revision.checked_add(1))
                        != current_revision;
                    conflict |= !entry
                        .contribution
                        .record
                        .follows_protected(&previous.contribution.record)
                        .unwrap_or(false);
                }
                selected = Some(entry);
            }
            if conflict {
                checked_sum(&mut self.report.coverage.source_conflicts, 1)?;
                for (revision, entry) in versions {
                    self.report.conflicting_source_views_not_added_to_accounting.push(json!({"source": usage_source_fingerprint(source), "revision": revision, "known_usd": entry.contribution.usd, "known_cny": entry.contribution.cny, "usd_unknown": entry.contribution.unpriced > 0, "cny_unknown": entry.contribution.cny_unpriced > 0}));
                }
                continue;
            }
            let Some(entry) = selected else {
                continue;
            };
            checked_sum(
                &mut self.report.coverage.project_ownership_ambiguous_sources,
                1,
            )?;
            if self.args.project.is_some() {
                continue;
            }
            let route = &entry.contribution.record.usage.route;
            let key = format!(
                "{}/{} @ {}",
                route.provider_identity,
                route.model,
                route
                    .endpoint_fingerprint
                    .as_deref()
                    .unwrap_or("unknown-endpoint")
            );
            self.report
                .accounting
                .as_mut()
                .context("global Stats accounting projection unavailable")?
                .add(&entry.contribution)?;
            self.report
                .models
                .entry(key)
                .or_default()
                .add(&entry.contribution)?;
        }
        for row in self.tools.values() {
            let report = self.report.tools.entry(row.name.clone()).or_default();
            checked_sum(&mut report.identified_executions, 1)?;
            match row
                .outcome
                .filter(|_| !row.identity_conflicted && !row.outcome_conflicted)
            {
                Some(true) => checked_sum(&mut report.successful_executions, 1)?,
                Some(false) => checked_sum(&mut report.failed_executions, 1)?,
                None => checked_sum(&mut report.unknown_outcomes, 1)?,
            }
            if let Some(tokens) = row
                .history_estimate
                .filter(|_| !row.identity_conflicted && !row.history_conflicted)
            {
                optional_sum(&mut report.retained_text_token_estimate_known, tokens)?;
                checked_sum(&mut report.retained_text_samples, 1)?;
            }
            if let Some(bytes) = row.original_bytes {
                optional_sum(&mut report.original_output_bytes_known, bytes)?;
            }
            if row.truncated == Some(true) {
                checked_sum(&mut report.truncated_samples, 1)?;
            }
            if row.spooled == Some(true) {
                checked_sum(&mut report.spooled_samples, 1)?;
            }
            if row.truncated.is_none() || row.spooled.is_none() {
                checked_sum(&mut report.output_shape_unknown_executions, 1)?;
            }
            if let Some(category) = &row.category {
                checked_sum(
                    report
                        .failure_categories
                        .entry(category.clone())
                        .or_default(),
                    1,
                )?;
            }
            let judged = report.successful_executions + report.failed_executions;
            report.failure_rate_percent =
                (judged > 0).then(|| 100.0 * report.failed_executions as f64 / judged as f64);
        }
        self.session_tokens.sort_unstable();
        let mut total = 0u64;
        for tokens in &self.session_tokens {
            checked_sum(&mut total, *tokens)?;
        }
        if !self.session_tokens.is_empty() {
            self.report.overview.average_saved_session_tokens =
                Some(total as f64 / self.session_tokens.len() as f64);
            let mid = self.session_tokens.len() / 2;
            self.report.overview.median_saved_session_tokens =
                Some(if self.session_tokens.len().is_multiple_of(2) {
                    self.session_tokens[mid - 1] as f64 / 2.0
                        + self.session_tokens[mid] as f64 / 2.0
                } else {
                    self.session_tokens[mid] as f64
                });
        }
        limit_models(&mut self.report.models, self.args.models);
        limit_tools(&mut self.report.tools, self.args.tools);
        limit_roles(&mut self.report.roles, self.args.roles);
        Ok(self.report)
    }
}

fn label(value: &str) -> String {
    codewhale_runtime::safe_label::sanitize_persisted_route_label(value)
}

fn history_tokens(value: &str) -> u64 {
    codewhale_engine::compaction::estimate_text_tokens_conservative(value) as u64
}

fn failure_category(value: &str) -> Option<String> {
    let signal = json!({"event": "tool_error", "message": value}).to_string();
    codewhale_runtime::session_diagnostics::analyze_session_failure_jsonl(&signal)
        .counts
        .keys()
        .next()
        .map(ToString::to_string)
}

fn keep_top<T>(values: &mut BTreeMap<String, T>, count: Option<usize>, score: impl Fn(&T) -> u64) {
    let Some(count) = count else {
        return;
    };
    let mut selected = values
        .iter()
        .map(|(name, value)| (name.clone(), score(value)))
        .collect::<Vec<_>>();
    selected.sort_by(|(a_name, a_score), (b_name, b_score)| {
        b_score.cmp(a_score).then_with(|| a_name.cmp(b_name))
    });
    let names = selected
        .into_iter()
        .take(count)
        .map(|(name, _)| name)
        .collect::<BTreeSet<_>>();
    values.retain(|name, _| names.contains(name));
}

fn limit_models(values: &mut BTreeMap<String, MoneyUsage>, count: Option<usize>) {
    keep_top(values, count, |value| {
        value
            .input_tokens_known
            .saturating_add(value.output_tokens_known)
    });
}

fn limit_tools(values: &mut BTreeMap<String, ToolReport>, count: Option<usize>) {
    keep_top(values, count, |value| value.identified_executions);
}

fn limit_roles(values: &mut BTreeMap<String, RoleReport>, count: Option<usize>) {
    keep_top(values, count, |value| value.recorded_tasks);
}

pub(super) fn collect(
    args: &MetricsArgs,
    sessions: &Path,
    events: &[PathBuf],
) -> Result<StatsReport> {
    let mut collector = Collector::new(args);
    if let Some(workspace) = &args.project {
        collector.remember_workspace(workspace)?;
    }
    match SessionManager::open_read_only(sessions.to_path_buf()) {
        Ok(Some(manager)) => {
            for entry in std::fs::read_dir(manager.sessions_dir())? {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        collector
                            .report
                            .coverage
                            .gap("session_directory_entry_unreadable");
                        continue;
                    }
                };
                if entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "json")
                {
                    continue;
                }
                if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    collector.report.coverage.gap("session_record_not_regular");
                    continue;
                }
                let filename = entry.file_name();
                let Some(id) = Path::new(&filename).file_stem().and_then(|id| id.to_str()) else {
                    collector
                        .report
                        .coverage
                        .gap("session_filename_unavailable");
                    continue;
                };
                if manager.validated_session_id(id).is_err() {
                    continue;
                }
                let metadata = match manager.load_usage_metadata(id) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        collector
                            .report
                            .coverage
                            .gap("session_metadata_or_accounting_unreadable");
                        continue;
                    }
                };
                match super::read_diagnostic_json(&entry.path()) {
                    Ok(value) => {
                        if value.get("schema_version").and_then(Value::as_u64).is_some_and(|version| version > u64::from(codewhale_engine::session_manager::CURRENT_SESSION_SCHEMA_VERSION)) {
                            collector.report.coverage.gap("session_schema_unsupported");
                            continue;
                        }
                        collector.session_tools(&value, &metadata)?;
                    }
                    Err(_) => collector
                        .report
                        .coverage
                        .gap("session_tool_projection_unreadable"),
                }
                collector.session(manager.sessions_dir(), metadata)?;
            }
        }
        Ok(None) => {}
        Err(_) => collector.report.coverage.gap("session_store_unreadable"),
    }
    let mut roots = BTreeSet::new();
    for directory in events {
        let Some(root) = directory.parent() else {
            collector.report.coverage.gap("runtime_root_unavailable");
            continue;
        };
        if !roots.insert(path_id(root)) {
            continue;
        }
        match RuntimeThreadStore::open_read_only(root.to_path_buf()) {
            Ok(Some(store)) => {
                store.visit_diagnostic_records(|record| match record {
                    Ok(record) => collector.runtime(root, record),
                    Err(_) => {
                        collector.report.coverage.gap("runtime_record_unreadable");
                        Ok(())
                    }
                })?;
            }
            Ok(None) => match std::fs::symlink_metadata(root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => collector
                    .report
                    .coverage
                    .gap("runtime_thread_index_unavailable"),
            },
            Err(_) => collector.report.coverage.gap("runtime_store_unreadable"),
        }
    }
    let workspaces = collector.workspaces.iter().cloned().collect::<Vec<_>>();
    for workspace in workspaces {
        collector.fleet(&workspace)?;
    }
    collector.finish()
}

pub(super) fn print_human(report: &StatsReport) {
    if let Some(accounting) = &report.accounting {
        println!(
            "Known native-source subtotals: USD {:.6} ({} identified sources with unknown USD coverage), CNY {:.6} ({} identified sources with unknown CNY coverage)",
            accounting.known_subtotal_usd,
            accounting.usd_price_unknown_sources,
            accounting.known_subtotal_cny,
            accounting.cny_price_unknown_sources
        );
        println!(
            "Known source tokens: {} input, {} output; cache read {}, cache write {}, reasoning {}, reasoning replay {}.",
            accounting.input_tokens_known,
            accounting.output_tokens_known,
            optional_count(accounting.cache_read_tokens_known),
            optional_count(accounting.cache_write_tokens_known),
            optional_count(accounting.reasoning_tokens_known),
            optional_count(accounting.reasoning_replay_tokens_known)
        );
    } else {
        println!(
            "Native source money and model subtotals: unavailable for a workspace filter; original source workspace is not captured."
        );
    }
    println!(
        "Historical coverage: incomplete; estimates from identified persisted sources, not provider invoice totals."
    );
    println!(
        "Observed views: {} saved documents, {} saved messages, {} Runtime threads, {} Runtime turns; unique conversations unknown.",
        report.overview.saved_documents,
        report.overview.messages_in_saved_documents,
        report.overview.runtime_threads,
        report.overview.runtime_turns
    );
    for (name, usage) in &report.models {
        println!(
            "Model {}: USD {:.6}, CNY {:.6}; {} input, {} output; unknown USD/CNY sources {}/{}.",
            serde_json::to_string(name).unwrap_or_default(),
            usage.known_subtotal_usd,
            usage.known_subtotal_cny,
            usage.input_tokens_known,
            usage.output_tokens_known,
            usage.usd_price_unknown_sources,
            usage.cny_price_unknown_sources
        );
    }
    println!(
        "Latest prepared-request components: unavailable; no layer estimate is inferred from prefix hashes."
    );
    for (name, tool) in &report.tools {
        println!(
            "Tool {}: {} identified executions, {} success, {} failure, {} unknown outcomes; retained-text token estimate {} ({} samples).",
            serde_json::to_string(name).unwrap_or_default(),
            tool.identified_executions,
            tool.successful_executions,
            tool.failed_executions,
            tool.unknown_outcomes,
            optional_count(tool.retained_text_token_estimate_known),
            tool.retained_text_samples
        );
    }
    println!(
        "Compaction: {} completed Runtime item observations; summarizer usage, attributable cost and dropped-history tokens unavailable.",
        report.compaction.completed_items
    );
    for (name, role) in &report.roles {
        println!(
            "Role {}: {} observed tasks; states {}; source-linked cost/usage and recovery rate unavailable.",
            serde_json::to_string(name).unwrap_or_default(),
            role.recorded_tasks,
            serde_json::to_string(&role.task_states).unwrap_or_default()
        );
    }
    if !report.coverage.gaps.is_empty() {
        println!(
            "Inspection gaps: {}",
            serde_json::to_string(&report.coverage.gaps).unwrap_or_default()
        );
    }
    println!(
        "Unavailable or unproven fields: {}",
        report
            .coverage
            .unknown_fields
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );
}

fn optional_count(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_string(), |count| count.to_string())
}
