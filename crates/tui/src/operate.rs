//! Operate: always-on fleet operation matching landed CWC `OperateRecord`
//! (`Hmbown/cwc` `20de981`, PR #284).
//!
//! One schema for `cw · operate` and CWC `/operate`. Burn rate is optional
//! (`null` = unbounded). The lead plans before workers. Pace throttles or
//! widens; it never stops the operation. Supervisor over nested instances —
//! no second `Engine::run_turn`.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::automation_manager::{AutomationManager, AutomationRecord, AutomationStatus};

pub const CWC_OPERATE_SCHEMA_VERSION: u32 = 1;
pub const OPERATE_MAX_WRITERS: usize = 3;
/// `hold` admits no new writers past this budget (the 8% band is met; hold
/// the current width instead of widening).
pub const OPERATE_HOLD_WRITERS: usize = 2;
/// `throttle` (observed more than 8% over target) cuts worker concurrency to
/// one writer. Pace throttles; it never stops the operation.
pub const OPERATE_THROTTLE_WRITERS: usize = 1;
pub const OPERATE_KEEPALIVE_ID: &str = "cw-operate";
/// Follow-up lead runs recur hourly; the first lead-plan step is kicked to
/// the next scheduler tick instead of waiting for the first recurrence.
pub const OPERATE_KEEPALIVE_RRULE: &str = "FREQ=HOURLY;INTERVAL=1";
pub const AUTO_MERGE_CHECKER_ENV: &str = "CODEWHALE_AUTO_MERGE_CHECKER";
pub const DIRECTION_PATH_ENV: &str = "CODEWHALE_DIRECTION_PATH";
pub const CHECK_AUTO_MERGE_SCRIPT: &str = "scripts/check-auto-merge.py";
pub const AUTO_MERGE_SCRIPT: &str = "scripts/auto_merge.py";
pub const AUTO_MERGE_PR_SCRIPT: &str = "scripts/auto-merge-pr.py";

const PACE_BAND: f64 = 0.08;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperateBurnRate {
    pub kind: String,
    pub amount_usd_per_hour: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperateStatus {
    Planning,
    Running,
    #[serde(rename = "idle_blocked")]
    IdleBlocked,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperateIdleReason {
    MissingCredentials,
    AwaitingLeadPlan,
    DirectionEmpty,
    HumanGated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatePace {
    Unbounded,
    Hold,
    Throttle,
    Widen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperateRosterMember {
    pub id: String,
    pub display_name: String,
    pub role: String,
    /// Saved selection for the lead; empty when no model has been assigned.
    pub model: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatePlanSlice {
    pub id: String,
    pub title: String,
    pub owner_id: String,
    pub depends_on: Vec<String>,
    pub est_cost_usd: f64,
    pub start_offset_sec: u32,
    pub duration_sec: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperateLeadPlan {
    pub slices: Vec<OperatePlanSlice>,
}

/// Landed CWC `OperateRecord` (packages/contracts/src/operate.js @ 20de981).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub schema_version: u32,
    pub direction: String,
    pub burn_rate: Option<OperateBurnRate>,
    pub lead_operator: OperateRosterMember,
    pub roster: Vec<OperateRosterMember>,
    pub lead_plan: Option<OperateLeadPlan>,
    pub status: OperateStatus,
    pub idle_blocked_reason: Option<OperateIdleReason>,
    pub pace: OperatePace,
    pub writers_in_flight: usize,
    pub workers_admitted: bool,
    pub spent_usd: f64,
    pub observed_burn_usd_per_hour: Option<f64>,
    pub credentials_present: bool,
    pub human_gated: bool,
    pub human_gate: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_keep_alive_at: String,
    #[serde(default)]
    pub cancelled_at: String,
}

impl Operation {
    /// Update display metadata from the saved selection without assigning a
    /// route to planned workers or claiming that an executor is running.
    pub(crate) fn set_lead_model(&mut self, model: &str) {
        self.lead_operator.model = model.to_string();
        for member in &mut self.roster {
            if member.id == self.lead_operator.id {
                member.model = model.to_string();
            }
        }
    }

    #[must_use]
    pub fn new(direction: impl Into<String>, burn_usd_per_hour: Option<f64>) -> Self {
        let now = Utc::now().to_rfc3339();
        let lead = OperateRosterMember {
            id: "lead".to_string(),
            display_name: "Lead operator".to_string(),
            role: "lead".to_string(),
            model: String::new(),
            state: "planning".to_string(),
        };
        let mut op = Self {
            id: format!("op_{}", Uuid::new_v4()),
            schema_version: CWC_OPERATE_SCHEMA_VERSION,
            direction: normalize_direction(direction.into()),
            burn_rate: normalize_burn_rate(burn_usd_per_hour),
            lead_operator: lead.clone(),
            roster: vec![lead],
            lead_plan: None,
            status: OperateStatus::Planning,
            idle_blocked_reason: None,
            pace: OperatePace::Unbounded,
            writers_in_flight: 0,
            workers_admitted: false,
            spent_usd: 0.0,
            observed_burn_usd_per_hour: None,
            credentials_present: false,
            human_gated: false,
            human_gate: String::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_keep_alive_at: now,
            cancelled_at: String::new(),
        };
        op.project();
        op
    }

    pub fn plan_from_direction(&mut self) {
        self.lead_plan = slices_from_direction(&self.direction);
        sync_plan_owners(self);
        self.project();
    }

    pub fn project(&mut self) {
        if self.status == OperateStatus::Cancelled {
            self.idle_blocked_reason = None;
            self.workers_admitted = false;
            self.writers_in_flight = 0;
            for member in &mut self.roster {
                member.state = "idle".to_string();
            }
            return;
        }
        let (status, reason) = derive_status(self);
        self.status = status;
        self.idle_blocked_reason = reason;
        self.workers_admitted = workers_admitted(self);
        self.pace = derive_pace(self);
        // Pace is a dispatch budget, not a label: the roster and the
        // `writersInFlight` count the keepalive lead actually dispatches at
        // come from `worker_dispatch_budget`, so over-target burn reduces
        // real concurrency instead of only renaming it.
        let budget = worker_dispatch_budget(self);
        live_roster(self, budget);
        self.writers_in_flight = if self.workers_admitted {
            self.roster
                .iter()
                .filter(|member| member.role == "worker" && member.state == "in_flight")
                .count()
        } else {
            0
        };
        if let Some(lead) = self.roster.iter().find(|member| member.role == "lead") {
            self.lead_operator = lead.clone();
        }
    }
}

fn normalize_direction(value: String) -> String {
    value.trim().chars().take(4000).collect()
}

fn normalize_burn_rate(amount: Option<f64>) -> Option<OperateBurnRate> {
    parse_burn_amount(amount).ok().flatten()
}

/// CWC `normalizeOperateBurnRate`: number, `$/hr` object, or null.
pub fn parse_burn_rate(value: Option<&serde_json::Value>) -> Result<Option<OperateBurnRate>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null()
        || value == &serde_json::Value::Bool(false)
        || value.as_str().is_some_and(str::is_empty)
    {
        return Ok(None);
    }
    if let Some("unbounded") = value.get("kind").and_then(|kind| kind.as_str()) {
        return Ok(None);
    }
    let amount = if value.is_number() || value.is_string() {
        json_number(value)
    } else {
        json_number(
            value
                .get("amountUsdPerHour")
                .or_else(|| value.get("usdPerHour"))
                .or_else(|| value.get("amount"))
                .unwrap_or(&serde_json::Value::Null),
        )
    };
    if amount.is_none() {
        anyhow::bail!("Burn rate is optional. When set, it must be a positive $/hr.");
    }
    parse_burn_amount(amount)
}

fn json_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn parse_burn_amount(amount: Option<f64>) -> Result<Option<OperateBurnRate>> {
    let Some(amount) = amount else {
        return Ok(None);
    };
    if !amount.is_finite() || amount <= 0.0 {
        anyhow::bail!("Burn rate is optional. When set, it must be a positive $/hr.");
    }
    if amount > 10_000.0 {
        anyhow::bail!("Burn rate must be 10000 $/hr or less.");
    }
    let rounded = (amount * 100.0).round() / 100.0;
    if rounded <= 0.0 {
        // A sub-cent rate rounds to a $0/hr target, which the pace governor
        // would treat as unbounded — reject it instead of silently dropping
        // the requested cap.
        anyhow::bail!("Burn rate must be at least $0.01/hr.");
    }
    Ok(Some(OperateBurnRate {
        kind: "usd_per_hour".to_string(),
        amount_usd_per_hour: rounded,
    }))
}

fn derive_status(op: &Operation) -> (OperateStatus, Option<OperateIdleReason>) {
    if op.status == OperateStatus::Cancelled {
        return (OperateStatus::Cancelled, None);
    }
    if !op.credentials_present {
        return (
            OperateStatus::IdleBlocked,
            Some(OperateIdleReason::MissingCredentials),
        );
    }
    if op.direction.is_empty() {
        return (
            OperateStatus::IdleBlocked,
            Some(OperateIdleReason::DirectionEmpty),
        );
    }
    if op.human_gated {
        return (
            OperateStatus::IdleBlocked,
            Some(OperateIdleReason::HumanGated),
        );
    }
    if op
        .lead_plan
        .as_ref()
        .is_none_or(|plan| plan.slices.is_empty())
    {
        return (
            OperateStatus::IdleBlocked,
            Some(OperateIdleReason::AwaitingLeadPlan),
        );
    }
    (OperateStatus::Running, None)
}

fn workers_admitted(op: &Operation) -> bool {
    op.status != OperateStatus::Cancelled
        && op.credentials_present
        && !op.direction.is_empty()
        && op
            .lead_plan
            .as_ref()
            .is_some_and(|plan| !plan.slices.is_empty())
        && !op.human_gated
}

fn derive_pace(op: &Operation) -> OperatePace {
    let Some(rate) = &op.burn_rate else {
        return OperatePace::Unbounded;
    };
    let Some(observed) = op.observed_burn_usd_per_hour.filter(|value| *value > 0.0) else {
        return OperatePace::Widen;
    };
    let target = rate.amount_usd_per_hour;
    if target <= 0.0 {
        return OperatePace::Unbounded;
    }
    let delta = (observed - target) / target;
    if delta > PACE_BAND {
        OperatePace::Throttle
    } else if delta < -PACE_BAND {
        OperatePace::Widen
    } else {
        OperatePace::Hold
    }
}

/// Pace-driven worker-dispatch budget within the 8% band semantics.
///
/// `widen`/`unbounded` opens the full writer width, `hold` admits no new
/// writers past the hold width, and `throttle` cuts concurrency to one
/// writer. While workers are admitted the budget is never zero — pace
/// throttles spend, it never stops the operation.
#[must_use]
pub fn worker_dispatch_budget(op: &Operation) -> usize {
    if !op.workers_admitted {
        return 0;
    }
    match op.pace {
        OperatePace::Unbounded | OperatePace::Widen => OPERATE_MAX_WRITERS,
        OperatePace::Hold => OPERATE_HOLD_WRITERS,
        OperatePace::Throttle => OPERATE_THROTTLE_WRITERS,
    }
}

fn live_roster(op: &mut Operation, budget: usize) {
    let admitted = op.workers_admitted;
    let cancelled = op.status == OperateStatus::Cancelled;
    let has_plan = op
        .lead_plan
        .as_ref()
        .is_some_and(|plan| !plan.slices.is_empty());
    // Only the first `budget` workers in roster (plan) order dispatch; the
    // rest stay idle until a slot frees or pace widens.
    let mut worker_slot = 0usize;
    for member in &mut op.roster {
        if cancelled {
            member.state = "idle".to_string();
        } else if !op.credentials_present {
            member.state = "blocked".to_string();
        } else if member.role == "lead" && !has_plan {
            member.state = "planning".to_string();
        } else if !admitted {
            member.state = if member.role == "lead" {
                "planning".to_string()
            } else {
                "idle".to_string()
            };
        } else if member.role == "worker" {
            worker_slot += 1;
            member.state = if worker_slot <= budget {
                "in_flight".to_string()
            } else {
                "idle".to_string()
            };
        } else {
            member.state = "planning".to_string();
        }
    }
}

fn slices_from_direction(direction: &str) -> Option<OperateLeadPlan> {
    let items = direction_items(direction);
    if items.is_empty() {
        return None;
    }
    let mut cursor = 0u32;
    let slices = items
        .into_iter()
        .enumerate()
        .map(|(index, title)| {
            let duration_sec = 1800;
            let slice = OperatePlanSlice {
                id: format!("slice-{}", index + 1),
                title,
                owner_id: if index == 0 {
                    "lead".to_string()
                } else {
                    format!("worker-{index}")
                },
                depends_on: if index == 0 {
                    Vec::new()
                } else {
                    vec![format!("slice-{index}")]
                },
                est_cost_usd: 0.25,
                start_offset_sec: cursor,
                duration_sec,
            };
            cursor = cursor.saturating_add(duration_sec);
            slice
        })
        .collect();
    Some(OperateLeadPlan { slices })
}

fn direction_items(direction: &str) -> Vec<String> {
    let mut items = Vec::new();
    for raw in direction.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let item = line
            .trim_start_matches(|c: char| {
                c.is_ascii_digit() || c == '.' || c == ')' || c == '-' || c == '*' || c == '#'
            })
            .trim();
        if !item.is_empty() {
            items.push(item.to_string());
        }
    }
    if items.is_empty() && !direction.trim().is_empty() {
        items.push(direction.trim().to_string());
    }
    items
}

#[must_use]
pub fn render_plan_board(op: &Operation) -> String {
    render_plan_board_locale(op, codewhale_localization::Locale::En)
}

/// Plan board with locale-aware chrome. Contract tokens (status / pace enum
/// values, slice ids, owner ids) stay verbatim; the surrounding prose comes
/// from the TUI locale packs.
#[must_use]
pub fn render_plan_board_locale(op: &Operation, locale: codewhale_localization::Locale) -> String {
    use codewhale_localization::{MessageId, tr};
    let tr_line = |id: MessageId| tr(locale, id).into_owned();

    let mut out = String::new();
    out.push_str(
        &tr_line(MessageId::OperateBoardHeader)
            .replace("{id}", &op.id)
            .replace("{status}", &status_label(op))
            .replace("{pace}", pace_label(op.pace))
            .replace("{writers}", &op.writers_in_flight.to_string()),
    );
    out.push('\n');
    match &op.burn_rate {
        Some(rate) => out.push_str(
            &tr_line(MessageId::OperateBoardBurnObserved)
                .replace(
                    "{actual}",
                    &format!("{}", op.observed_burn_usd_per_hour.unwrap_or(0.0)),
                )
                .replace("{target}", &format!("{}", rate.amount_usd_per_hour)),
        ),
        None => out.push_str(&tr_line(MessageId::OperateBoardBurnNoCap)),
    }
    out.push('\n');
    if op.direction.is_empty() {
        out.push_str(&tr_line(MessageId::OperateBoardDirectionEmpty));
    } else {
        out.push_str(
            &tr_line(MessageId::OperateBoardDirectionLine)
                .replace("{line}", op.direction.lines().next().unwrap_or("")),
        );
    }
    out.push('\n');
    let Some(plan) = &op.lead_plan else {
        out.push_str(&tr_line(MessageId::OperateBoardPlanMissing));
        out.push('\n');
        return out;
    };
    out.push_str(&tr_line(MessageId::OperateBoardPlanHeader));
    out.push('\n');
    for slice in &plan.slices {
        out.push_str(&format!(
            "          {:<9} {:<8} {:>5} {:>5} {:>6.2}  {:<7} {}\n",
            slice.id,
            slice.owner_id,
            slice.start_offset_sec,
            slice.duration_sec,
            slice.est_cost_usd,
            if slice.depends_on.is_empty() {
                "-".to_string()
            } else {
                slice.depends_on.join(",")
            },
            slice.title
        ));
    }
    out.push_str(&render_timeline(&plan.slices, locale));
    out
}

fn render_timeline(slices: &[OperatePlanSlice], locale: codewhale_localization::Locale) -> String {
    use codewhale_localization::{MessageId, tr};
    let max_end = slices
        .iter()
        .map(|slice| slice.start_offset_sec.saturating_add(slice.duration_sec))
        .max()
        .unwrap_or(0)
        .max(1);
    let width = 24u32;
    let mut out = format!("{}\n", tr(locale, MessageId::OperateBoardGantt));
    for slice in slices {
        let start = (slice.start_offset_sec.saturating_mul(width)) / max_end;
        let end = (slice
            .start_offset_sec
            .saturating_add(slice.duration_sec)
            .saturating_mul(width))
            / max_end;
        let end = end.max(start.saturating_add(1)).min(width);
        let mut bar = vec!['.'; width as usize];
        for idx in start..end {
            if let Some(cell) = bar.get_mut(idx as usize) {
                *cell = '#';
            }
        }
        out.push_str(&format!(
            "      {:<9} {}\n",
            slice.id,
            bar.into_iter().collect::<String>()
        ));
    }
    out
}

fn status_label(op: &Operation) -> String {
    match (op.status, op.idle_blocked_reason) {
        (OperateStatus::Cancelled, _) => "cancelled".to_string(),
        (OperateStatus::Running, _) => "running".to_string(),
        (OperateStatus::Planning, _) => "planning".to_string(),
        (_, Some(OperateIdleReason::DirectionEmpty)) => "idle_blocked: direction_empty".to_string(),
        (_, Some(OperateIdleReason::AwaitingLeadPlan)) => {
            "idle_blocked: awaiting_lead_plan".to_string()
        }
        (_, Some(OperateIdleReason::MissingCredentials)) => {
            "idle_blocked: missing_credentials".to_string()
        }
        (_, Some(OperateIdleReason::HumanGated)) => {
            format!("idle_blocked: human_gated {}", op.human_gate)
        }
        (OperateStatus::IdleBlocked, None) => "idle_blocked".to_string(),
    }
}

fn pace_label(pace: OperatePace) -> &'static str {
    match pace {
        OperatePace::Unbounded => "unbounded",
        OperatePace::Hold => "hold",
        OperatePace::Throttle => "throttle",
        OperatePace::Widen => "widen",
    }
}

/// Honor an explicit operator-provided path (env override) only when it is a
/// non-empty value without NUL bytes or `..` traversal segments that actually
/// names one regular file. Keeps env-provided values out of raw path
/// expressions (CodeQL "uncontrolled data in path" class).
fn explicit_file_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    path.is_file().then_some(path)
}

/// Same hardening for an explicit directory (env override): no NUL bytes, no
/// `..` traversal segments. Existence is probed by the caller.
fn explicit_dir_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(path)
}

#[must_use]
pub fn discover_direction_path(workspace: &Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(DIRECTION_PATH_ENV)
        && let Some(path) = explicit_file_path(&explicit)
    {
        return Some(path);
    }
    let local = workspace.join("DIRECTION.md");
    if local.is_file() {
        return Some(local);
    }
    materialize_ops_origin_main()
        .ok()
        .map(|root| root.join("DIRECTION.md"))
        .filter(|path| path.is_file())
}

pub fn read_direction(workspace: &Path) -> Result<String> {
    match discover_direction_path(workspace) {
        Some(path) => fs::read_to_string(&path)
            .with_context(|| format!("Failed to read direction {}", path.display())),
        None => Ok(String::new()),
    }
}

/// Resolve the saved keepalive pin, or the caller's effective session route
/// for a fresh/unpinned record. Auto stays a policy until Runtime admits a turn;
/// resolving its inventory here could run a paid classifier.
fn keepalive_route(
    config: &codewhale_runtime::config::Config,
    current: Option<&AutomationRecord>,
    selection: Option<(&codewhale_runtime::config::ProviderIdentity, &str)>,
) -> Result<(codewhale_runtime::config::ProviderIdentity, String, bool)> {
    let (identity, model) = if let Some(record) = current
        .filter(|record| record.model_provider.is_some() || record.model_provider_id.is_some())
    {
        let identity = config
            .resolve_persisted_provider_identity(
                record.model_provider.as_deref(),
                record.model_provider_id.as_deref(),
            )
            .map_err(anyhow::Error::msg)?;
        let model = record
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .context("Pinned Operate keepalive has no model; repair its saved route")?;
        (identity, model.to_string())
    } else if let Some((identity, model)) = selection {
        let identity = config
            .resolve_persisted_provider_identity(
                Some(identity.provider.as_str()),
                identity.persisted_id(),
            )
            .map_err(anyhow::Error::msg)?;
        (identity, model.to_string())
    } else {
        (
            config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?,
            config.default_model(),
        )
    };
    if model.trim().eq_ignore_ascii_case("auto") {
        let mut scoped = config.clone();
        scoped
            .scope_to_provider_identity(&identity)
            .map_err(anyhow::Error::msg)?;
        let credentials = crate::provider_readiness::has_api_key_for(&scoped, &identity);
        return Ok((identity, "auto".to_string(), credentials));
    }
    let route =
        crate::route_runtime::resolve_runtime_route_for_identity(config, &identity, Some(&model))
            .map_err(anyhow::Error::msg)?;
    let credentials = crate::provider_readiness::has_api_key_for(&route.config, &route.identity);
    Ok((route.identity, route.model, credentials))
}

/// Inspect the same saved route used by the scheduler. Credential presence is
/// a local readiness observation, not proof of provider execution.
pub(crate) fn keepalive_readiness(
    manager: &AutomationManager,
    config: &codewhale_runtime::config::Config,
    selection: Option<(&codewhale_runtime::config::ProviderIdentity, &str)>,
) -> Result<(String, bool)> {
    let mut readiness = (String::new(), false);
    manager.edit_automation(OPERATE_KEEPALIVE_ID, |current| {
        if current
            .as_ref()
            .and_then(|record| record.execution_scope.as_deref())
            .is_some_and(|scope| Some(scope) != manager.execution_scope())
        {
            bail!("Operate keepalive belongs to another Runtime execution scope");
        }
        let (_, model, credentials) = keepalive_route(config, current.as_ref(), selection)?;
        readiness = (model, credentials);
        Ok(None)
    })?;
    Ok(readiness)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMergeRequest<'a> {
    pub pr: &'a str,
    pub role: &'a str,
    pub repo: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMergeDecision {
    Allow,
    Deny { reason: String },
}

#[must_use]
pub fn check_auto_merge_args(repo: &str, pr: &str, agent: &str) -> Vec<String> {
    vec![
        CHECK_AUTO_MERGE_SCRIPT.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--pr".to_string(),
        pr.to_string(),
        "--agent".to_string(),
        agent.to_string(),
    ]
}

#[must_use]
pub fn auto_merge_pr_args(repo: &str, pr: &str, agent: &str) -> Vec<String> {
    vec![
        AUTO_MERGE_PR_SCRIPT.to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--pr".to_string(),
        pr.to_string(),
        "--agent".to_string(),
        agent.to_string(),
    ]
}

/// Strictly validate an auto-merge request before any of it reaches argv.
///
/// The checker is spawned without a shell, but these values still become
/// arguments to `python3` and then to `gh`, so they are held to the shapes
/// GitHub itself allows: `repo` is `owner/name`, `pr` is a positive decimal
/// number, and `agent` is a short role token. No value may start with `-`.
pub fn validate_auto_merge_request(request: &AutoMergeRequest<'_>) -> Result<(), String> {
    fn is_owner(owner: &str) -> bool {
        (1..=39).contains(&owner.len())
            && !owner.starts_with('-')
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
    fn is_repo_name(name: &str) -> bool {
        (1..=100).contains(&name.len())
            && name != "."
            && name != ".."
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    }
    let repo_ok = request
        .repo
        .split_once('/')
        .is_some_and(|(owner, name)| is_owner(owner) && is_repo_name(name));
    if !repo_ok {
        return Err("repo must be `owner/name` using GitHub name characters".to_string());
    }
    let pr_ok = (1..=10).contains(&request.pr.len())
        && request.pr.bytes().all(|b| b.is_ascii_digit())
        && request.pr.parse::<u64>().is_ok_and(|n| n > 0);
    if !pr_ok {
        return Err("pr must be a positive pull request number".to_string());
    }
    let agent_ok = (1..=64).contains(&request.role.len())
        && !request.role.starts_with('-')
        && request
            .role
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if !agent_ok {
        return Err("agent must be 1-64 letters, digits, `-` or `_`".to_string());
    }
    Ok(())
}

pub fn evaluate_auto_merge(
    request: AutoMergeRequest<'_>,
    checker: Option<&Path>,
) -> AutoMergeDecision {
    if let Err(reason) = validate_auto_merge_request(&request) {
        return AutoMergeDecision::Deny { reason };
    }
    let Some(checker) = checker else {
        return AutoMergeDecision::Deny {
            reason: "auto-merge checker missing; fail-closed".to_string(),
        };
    };
    if !checker.exists() {
        return AutoMergeDecision::Deny {
            reason: "auto-merge checker missing; fail-closed".to_string(),
        };
    }
    match Command::new("python3")
        .arg(checker)
        .arg("--repo")
        .arg(request.repo)
        .arg("--pr")
        .arg(request.pr)
        .arg("--agent")
        .arg(request.role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => AutoMergeDecision::Allow,
        Ok(_) => AutoMergeDecision::Deny {
            reason: "auto-merge checker refused".to_string(),
        },
        Err(error) => AutoMergeDecision::Deny {
            reason: format!("auto-merge checker failed to start: {error}"),
        },
    }
}

#[must_use]
pub fn discover_auto_merge_checker(_workspace: &Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(AUTO_MERGE_CHECKER_ENV)
        && let Some(path) = explicit_file_path(&explicit)
    {
        return Some(path);
    }
    materialize_ops_origin_main()
        .ok()
        .map(|root| root.join(CHECK_AUTO_MERGE_SCRIPT))
        .filter(|path| path.is_file())
}

fn ops_git_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["CODEWHALE_OPS_GIT", "CODEWHALE_OPS_ROOT"] {
        if let Ok(path) = std::env::var(key)
            && let Some(path) = explicit_dir_path(&path)
        {
            out.push(path);
        }
    }
    out
}

fn git_origin_main_sha(repo: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "origin/main"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // The sha becomes a path segment below; only plain hex of a plausible
    // length may flow into it.
    if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(sha)
}

pub fn materialize_ops_origin_main() -> Result<PathBuf> {
    let repo = ops_git_candidates()
        .into_iter()
        .find(|path| git_origin_main_sha(path).is_some())
        .context("no codewhale-ops git checkout with origin/main")?;
    let sha = git_origin_main_sha(&repo).context("origin/main sha")?;
    let dest = default_operate_dir()
        .parent()
        .unwrap_or(Path::new("."))
        .join("ops-main")
        .join(&sha[..12.min(sha.len())]);
    let marker = dest.join(CHECK_AUTO_MERGE_SCRIPT);
    if marker.is_file()
        && dest.join("DIRECTION.md").is_file()
        && dest.join(AUTO_MERGE_SCRIPT).is_file()
    {
        return Ok(dest);
    }
    fs::create_dir_all(&dest).with_context(|| format!("Failed to create {}", dest.display()))?;
    let archive = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
            "archive",
            "origin/main",
            "--",
            "DIRECTION.md",
            CHECK_AUTO_MERGE_SCRIPT,
            AUTO_MERGE_SCRIPT,
            AUTO_MERGE_PR_SCRIPT,
            "agent-workstreams/AUTO_MERGE.toml",
        ])
        .stdin(Stdio::null())
        .output()
        .context("git archive origin/main")?;
    if !archive.status.success() {
        anyhow::bail!(
            "git archive origin/main failed: {}",
            String::from_utf8_lossy(&archive.stderr).trim()
        );
    }
    let mut child = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&dest)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("tar extract ops origin/main")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(&archive.stdout)?;
    }
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("failed to extract ops origin/main archive");
    }
    if !marker.is_file() {
        anyhow::bail!("ops origin/main archive missing {CHECK_AUTO_MERGE_SCRIPT}");
    }
    Ok(dest)
}

pub fn default_operate_dir() -> PathBuf {
    if let Ok(path) = std::env::var("CODEWHALE_OPERATE_DIR") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::automation_manager::default_automations_dir()
        .parent()
        .map(|parent| parent.join("operate"))
        .unwrap_or_else(|| PathBuf::from("operate"))
}

pub struct OperationStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl OperationStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
        let path = dir.join("current.json");
        let lock_path = dir.join("current.json.lock");
        Ok(Self { path, lock_path })
    }

    pub fn load(&self) -> Result<Option<Operation>> {
        // Match the skill-state discipline: pure readers take the shared
        // cross-process read lock only when one exists, so a read never
        // fabricates a lock file.
        if self.lock_path.exists() {
            let file = fs::File::open(&self.lock_path)
                .with_context(|| format!("Failed to open {}", self.lock_path.display()))?;
            let lock = fd_lock::RwLock::new(file);
            let _guard = lock
                .read()
                .with_context(|| format!("read-lock {}", self.path.display()))?;
            return self.load_unlocked();
        }
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<Option<Operation>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read {}", self.path.display()))?;
        let op: Operation = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", self.path.display()))?;
        Ok(Some(op))
    }

    /// Save under the cross-process writer lock with an atomic temp+rename
    /// write, so concurrent Codewhale processes never interleave partial
    /// records.
    pub fn save(&self, op: &Operation) -> Result<()> {
        let file = self.open_lock_file()?;
        let mut lock = fd_lock::RwLock::new(file);
        let _guard = lock
            .write()
            .with_context(|| format!("write-lock {}", self.path.display()))?;
        self.save_unlocked(op)
    }

    /// Read-merge-write under the cross-process writer lock: the latest
    /// on-disk record is reloaded *inside* the lock before `edit` runs, so a
    /// concurrent PATCH/keepalive/plan save can no longer be silently lost by
    /// a stale read. Returns `None` when no operation is recorded yet.
    pub fn mutate(
        &self,
        edit: impl FnOnce(&mut Operation) -> Result<()>,
    ) -> Result<Option<Operation>> {
        let file = self.open_lock_file()?;
        let mut lock = fd_lock::RwLock::new(file);
        let _guard = lock
            .write()
            .with_context(|| format!("write-lock {}", self.path.display()))?;
        let Some(mut op) = self.load_unlocked()? else {
            return Ok(None);
        };
        edit(&mut op)?;
        self.save_unlocked(&op)?;
        Ok(Some(op))
    }

    fn open_lock_file(&self) -> Result<fs::File> {
        crate::session_manager::open_private_lock_file(&self.lock_path)
            .with_context(|| format!("Failed to open {}", self.lock_path.display()))
    }

    fn save_unlocked(&self, op: &Operation) -> Result<()> {
        codewhale_config::persistence::atomic_write_json(&self.path, op)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

pub fn start_operation(
    store: &OperationStore,
    workspace: &Path,
    direction: Option<String>,
    burn_usd_per_hour: Option<f64>,
    credentials_present: bool,
    lead_model: &str,
) -> Result<Operation> {
    let mut direction = match direction {
        Some(text) if !text.trim().is_empty() => text,
        _ => read_direction(workspace)?,
    };
    if direction.trim().is_empty()
        && let Some(existing) = store.load()?
    {
        direction = existing.direction;
    }
    let mut op = Operation::new(direction, burn_usd_per_hour);
    op.set_lead_model(lead_model);
    op.credentials_present = credentials_present;
    op.project();
    store.save(&op)?;
    Ok(op)
}

/// Re-entering Operate attaches to the recorded operation — same id, spend,
/// lead plan, and roster — instead of minting a fresh record that silently
/// resets progress. A cancelled record (or an empty store) starts a new
/// operation. An explicitly provided direction is applied through the normal
/// patch rules (which invalidate a superseded lead plan).
pub fn attach_or_start_operation(
    store: &OperationStore,
    workspace: &Path,
    direction: Option<String>,
    burn_usd_per_hour: Option<f64>,
    credentials_present: bool,
    lead_model: &str,
) -> Result<Operation> {
    if store
        .load()?
        .is_some_and(|op| op.status != OperateStatus::Cancelled)
    {
        let attached = store.mutate(|op| {
            if let Some(text) = direction.as_ref().filter(|text| !text.trim().is_empty()) {
                apply_operate_patch(op, &serde_json::json!({ "direction": text }))?;
            }
            op.set_lead_model(lead_model);
            op.credentials_present = credentials_present;
            op.project();
            Ok(())
        })?;
        if let Some(op) = attached {
            return Ok(op);
        }
    }
    start_operation(
        store,
        workspace,
        direction,
        burn_usd_per_hour,
        credentials_present,
        lead_model,
    )
}

pub fn apply_operate_patch(op: &mut Operation, patch: &serde_json::Value) -> Result<()> {
    if op.status == OperateStatus::Cancelled {
        anyhow::bail!("A cancelled Operation cannot be edited.");
    }
    if let Some(direction) = patch.get("direction") {
        let next = normalize_direction(direction.as_str().map(str::to_string).unwrap_or_default());
        if patch.get("leadPlan").is_none() && next != op.direction {
            // A changed direction supersedes the recorded lead plan: workers
            // must stop executing slices derived from the old direction. The
            // operation idles `awaiting_lead_plan` until the lead re-plans
            // (same patch may instead carry an explicit replacement plan).
            op.lead_plan = None;
        }
        op.direction = next;
    }
    if patch.get("burnRate").is_some() {
        op.burn_rate = parse_burn_rate(patch.get("burnRate"))?;
    }
    if let Some(plan) = patch.get("leadPlan") {
        op.lead_plan = if plan.is_null() {
            None
        } else {
            Some(serde_json::from_value(plan.clone()).context("leadPlan is invalid")?)
        };
        // Plan owners are the worker roster: PUT /v1/operate/plan and a
        // PATCHed plan must admit their owners or workers never dispatch.
        sync_plan_owners(op);
    }
    if let Some(flag) = patch.get("humanGated").and_then(serde_json::Value::as_bool) {
        op.human_gated = flag;
    }
    if let Some(gate) = patch.get("humanGate").and_then(serde_json::Value::as_str) {
        op.human_gate = gate.trim().chars().take(160).collect();
        if human_gate_for(&op.human_gate) {
            op.human_gated = true;
        }
    }
    if let Some(flag) = patch
        .get("credentialsPresent")
        .and_then(serde_json::Value::as_bool)
    {
        op.credentials_present = flag;
    }
    op.updated_at = Utc::now().to_rfc3339();
    op.project();
    Ok(())
}

/// Add every non-lead plan owner to the roster (idempotent) so admitted
/// slices have a worker to dispatch to.
fn sync_plan_owners(op: &mut Operation) {
    if let Some(plan) = &op.lead_plan {
        for slice in &plan.slices {
            if slice.owner_id != "lead"
                && !slice.owner_id.is_empty()
                && !op.roster.iter().any(|member| member.id == slice.owner_id)
            {
                op.roster.push(OperateRosterMember {
                    id: slice.owner_id.clone(),
                    display_name: slice.owner_id.clone(),
                    role: "worker".to_string(),
                    model: String::new(),
                    state: "idle".to_string(),
                });
            }
        }
    }
}

pub fn cancel_operation(store: &OperationStore) -> Result<Option<Operation>> {
    store.mutate(|op| {
        let now = Utc::now().to_rfc3339();
        op.status = OperateStatus::Cancelled;
        op.cancelled_at = now.clone();
        op.updated_at = now.clone();
        op.last_keep_alive_at = now;
        op.project();
        Ok(())
    })
}

pub fn keep_alive_observation(
    op: &mut Operation,
    observed_burn_usd_per_hour: Option<f64>,
    spent_usd: Option<f64>,
    credentials_present: Option<bool>,
    human_gated: Option<bool>,
) {
    let now = Utc::now().to_rfc3339();
    op.last_keep_alive_at = now.clone();
    op.updated_at = now;
    if let Some(burn) = observed_burn_usd_per_hour {
        op.observed_burn_usd_per_hour = Some(burn.max(0.0));
    }
    if let Some(spent) = spent_usd {
        op.spent_usd = spent.max(0.0);
    }
    if let Some(credentials) = credentials_present {
        op.credentials_present = credentials;
    }
    if let Some(gated) = human_gated {
        op.human_gated = gated;
    }
    op.project();
}

/// Install (or refresh) the `cw-operate` keepalive bound to `workspace`.
///
/// The record is built directly under the fixed id — no create-then-delete
/// id swap that could orphan an active UUID-named automation. Reuse
/// refreshes the prompt and workspace while preserving an explicit saved
/// model/provider pair. Fresh and legacy unpinned records capture the caller's
/// effective route together, including Auto intent and exact custom identity.
///
/// `kick_now` schedules the first lead-plan step for the next scheduler tick
/// (a fresh operation otherwise idles up to an hour awaiting its plan); the
/// hourly recurrence covers follow-ups.
pub(crate) fn upsert_keepalive(
    manager: &AutomationManager,
    workspace: &Path,
    kick_now: bool,
    config: &codewhale_runtime::config::Config,
    selection: Option<(&codewhale_runtime::config::ProviderIdentity, &str)>,
) -> Result<(String, bool)> {
    let now = Utc::now();
    let prompt = format!(
        "Keep Operate alive. Read the Operate record (current.json) and its direction, refresh the lead plan, and dispatch ready slices with at most `writersInFlight` concurrent workers — that budget already encodes pace (hold/throttle/widen), so honor it instead of widening on your own. Burn rate paces spend; it never stops the operation. Workspace: {}",
        workspace.display()
    );
    let mut readiness = (String::new(), false);
    manager.edit_automation(OPERATE_KEEPALIVE_ID, |current| {
        let scope = manager
            .execution_scope()
            .context("Operate execution ownership is unverified")?;
        if current
            .as_ref()
            .and_then(|record| record.execution_scope.as_deref())
            .is_some_and(|bound| bound != scope)
        {
            bail!("Operate keepalive belongs to another Runtime execution scope");
        }
        let (identity, model, ready) = keepalive_route(config, current.as_ref(), selection)?;
        readiness = (model.clone(), ready);
        let mut record = current.unwrap_or_else(|| AutomationRecord {
            destination_thread_id: None,
            schema_version: crate::automation_manager::CURRENT_AUTOMATION_SCHEMA_VERSION,
            execution_scope: Some(scope.to_string()),
            id: OPERATE_KEEPALIVE_ID.to_string(),
            name: "Operate keep-alive".to_string(),
            prompt: prompt.clone(),
            rrule: OPERATE_KEEPALIVE_RRULE.to_string(),
            cwds: Vec::new(),
            model: None,
            model_provider: None,
            model_provider_id: None,
            mode: Some("operate".to_string()),
            allow_shell: Some(true),
            trust_mode: Some(false),
            auto_approve: Some(false),
            delivery_mode: None,
            status: AutomationStatus::Active,
            created_at: now,
            updated_at: now,
            next_run_at: None,
            last_run_at: None,
            revision: 0,
        });
        record.schema_version = crate::automation_manager::CURRENT_AUTOMATION_SCHEMA_VERSION;
        if record.execution_scope.is_none() {
            record.execution_scope = Some(scope.to_string());
        }
        record.name = "Operate keep-alive".to_string();
        record.prompt = prompt;
        record.rrule = OPERATE_KEEPALIVE_RRULE.to_string();
        record.cwds = vec![workspace.to_path_buf()];
        record.model = Some(model);
        record.model_provider = Some(identity.provider.as_str().to_string());
        record.model_provider_id = identity.persisted_id().map(str::to_string);
        record.mode = Some("operate".to_string());
        record.allow_shell = Some(true);
        record.trust_mode = Some(false);
        record.auto_approve = Some(false);
        record.delivery_mode = None;
        record.status = AutomationStatus::Active;
        record.updated_at = now;
        if kick_now {
            record.next_run_at = Some(now);
        }
        Ok(Some(record))
    })?;
    Ok(readiness)
}

/// Cancel tears the operation down *including* its keepalive: an unattended
/// hourly lead run after cancel is pure cost. The automation is paused
/// (never deleted) so its run history survives and a later start reactivates
/// it. A missing keepalive is not an error; any other failure to establish
/// its state is. A requested spending stop that cannot confirm the record is
/// paused must never look like one that happened — an unreadable record may
/// still be Active and scheduled.
pub fn pause_keepalive(manager: &AutomationManager) -> Result<()> {
    let record = match manager.get_automation(OPERATE_KEEPALIVE_ID) {
        Ok(record) => record,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => {
            return Err(error.context(
                "Operate keepalive state could not be established; it may still be scheduled",
            ));
        }
    };
    if matches!(record.status, AutomationStatus::Active) {
        manager.pause_automation(OPERATE_KEEPALIVE_ID)?;
    }
    Ok(())
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Pull the next keepalive lead run to the next scheduler tick (for example
/// after a direction PATCH invalidated the plan). No-op when the keepalive is
/// absent or paused (a paused keepalive belongs to a cancelled operation).
pub fn kick_keepalive(manager: &AutomationManager) -> Result<bool> {
    let mut kicked = false;
    manager.edit_automation(OPERATE_KEEPALIVE_ID, |current| {
        let Some(mut record) = current else {
            return Ok(None);
        };
        if record.status == AutomationStatus::Active {
            let now = Utc::now();
            record.next_run_at = Some(now);
            record.updated_at = now;
            kicked = true;
        }
        Ok(Some(record))
    })?;
    Ok(kicked)
}

#[must_use]
pub fn human_gate_for(action: &str) -> bool {
    matches!(
        action,
        "deploy" | "billing" | "force-push" | "forbidden-pr" | "red-ci"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn route_fixture_config() -> codewhale_runtime::config::Config {
        toml::from_str(
            r#"
provider = "route-a"
[providers.route-a]
kind = "openai-compatible"
base_url = "https://route-a.example.test/v1"
model = "same-model"
auth_mode = "none"
[providers.route-b]
kind = "openai-compatible"
base_url = "https://route-b.example.test/v1"
model = "same-model"
auth_mode = "api-key"
api_key_env = "CW_OPERATE_MISSING_TEST_KEY"
"#,
        )
        .expect("route fixture")
    }

    #[test]
    fn keepalive_saved_route_controls_readiness_refresh_and_auto_intent() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let _missing = crate::test_support::EnvVarGuard::remove("CW_OPERATE_MISSING_TEST_KEY");
        let root = TempDir::new()?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let mut config = route_fixture_config();
        assert!(upsert_keepalive(&manager, root.path(), true, &config, None)?.1);
        let first = manager.get_automation(OPERATE_KEEPALIVE_ID)?;
        assert_eq!(first.model.as_deref(), Some("same-model"));
        assert_eq!(first.model_provider.as_deref(), Some("custom"));
        assert_eq!(first.model_provider_id.as_deref(), Some("route-a"));
        assert_eq!(first.auto_approve, Some(false));

        // Explicitly edit the saved route. Parent route-a is credential-ready;
        // route-b is not, even though both expose the same model spelling.
        let mut pinned = first.clone();
        pinned.model_provider_id = Some("route-b".into());
        manager.save_automation(&pinned)?;
        assert!(!keepalive_readiness(&manager, &config, None)?.1);
        assert!(!upsert_keepalive(&manager, root.path(), false, &config, None)?.1);
        assert_eq!(
            manager
                .get_automation(OPERATE_KEEPALIVE_ID)?
                .model_provider_id,
            Some("route-b".into())
        );

        pinned.model = Some("auto".into());
        manager.save_automation(&pinned)?;
        assert!(!upsert_keepalive(&manager, root.path(), false, &config, None)?.1);
        let auto = manager.get_automation(OPERATE_KEEPALIVE_ID)?;
        assert_eq!(auto.model.as_deref(), Some("auto"));
        assert_eq!(auto.model_provider_id.as_deref(), Some("route-b"));
        assert_eq!(auto.created_at, first.created_at);

        config.providers.as_mut().unwrap().custom.remove("route-b");
        let before = serde_json::to_value(&auto)?;
        assert!(keepalive_readiness(&manager, &config, None).is_err());
        assert!(upsert_keepalive(&manager, root.path(), true, &config, None).is_err());
        assert_eq!(
            serde_json::to_value(manager.get_automation(OPERATE_KEEPALIVE_ID)?)?,
            before
        );

        // Legacy unpinned GLM is a previous scheduler default, not an exact
        // provider choice. Migration replaces all route fields together.
        pinned.model = Some("GLM-5.3".into());
        pinned.model_provider = None;
        pinned.model_provider_id = None;
        manager.save_automation(&pinned)?;
        assert!(upsert_keepalive(&manager, root.path(), true, &config, None)?.1);
        let migrated = manager.get_automation(OPERATE_KEEPALIVE_ID)?;
        assert_eq!(migrated.model.as_deref(), Some("same-model"));
        assert_eq!(migrated.model_provider.as_deref(), Some("custom"));
        assert_eq!(migrated.model_provider_id.as_deref(), Some("route-a"));
        Ok(())
    }

    #[test]
    fn keepalive_legacy_custom_records_the_table_id() -> Result<()> {
        let _env = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove("CODEWHALE_CLI_API_KEY");
        let root = TempDir::new()?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let config = codewhale_runtime::config::parse_config_base(
            r#"provider = "custom"
[providers.custom]
kind = "openai-compatible"
base_url = "https://legacy.example.test/v1"
model = "legacy-model"
auth_mode = "none"
"#,
        )?;
        upsert_keepalive(&manager, root.path(), false, &config, None)?;
        let record = manager.get_automation(OPERATE_KEEPALIVE_ID)?;
        assert_eq!(record.model.as_deref(), Some("legacy-model"));
        // The literal route is the `[providers.custom]` table since #6394.
        assert_eq!(record.model_provider.as_deref(), Some("custom"));
        assert_eq!(record.model_provider_id.as_deref(), Some("custom"));
        upsert_keepalive(&manager, root.path(), false, &config, None)?;
        assert_eq!(
            manager
                .get_automation(OPERATE_KEEPALIVE_ID)?
                .model_provider_id
                .as_deref(),
            Some("custom")
        );
        Ok(())
    }

    struct RouteRecordingExecutor(
        std::sync::Arc<std::sync::Mutex<Vec<crate::runtime_threads::CreateThreadRequest>>>,
    );

    #[async_trait::async_trait]
    impl crate::task_manager::TaskExecutor for RouteRecordingExecutor {
        async fn execute(
            &self,
            task: crate::task_manager::ExecutionTask,
            _events: tokio::sync::mpsc::Sender<crate::task_manager::TaskExecutionEvent>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> crate::task_manager::TaskExecutionResult {
            self.0.lock().unwrap().push(task.thread_request());
            crate::task_manager::TaskExecutionResult {
                status: crate::task_manager::TaskStatus::Completed,
                result_text: Some("route fixture completed".into()),
                error: None,
                terminal_reason: crate::task_manager::TaskTerminalReason::Completed,
            }
        }
    }

    #[tokio::test]
    async fn keepalive_pin_reaches_task_and_survives_parent_change() -> Result<()> {
        let root = TempDir::new()?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let mut config = route_fixture_config();
        upsert_keepalive(&manager, root.path(), false, &config, None)?;
        pause_keepalive(&manager)?;
        config.provider = Some("route-b".into());
        assert!(upsert_keepalive(&manager, root.path(), true, &config, None)?.1);
        let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tasks = crate::task_manager::TaskManager::start_with_executor(
            crate::task_manager::TaskManagerConfig {
                data_dir: root.path().join("tasks"),
                worker_count: 1,
                default_workspace: root.path().to_path_buf(),
                default_model: "changed-default".into(),
                default_mode: "agent".into(),
                allow_shell: false,
                trust_mode: false,
                execution_limits: Default::default(),
            },
            std::sync::Arc::new(RouteRecordingExecutor(observations.clone())),
        )
        .await?;
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(manager));
        let run = crate::automation_manager::run_now_shared(&shared, OPERATE_KEEPALIVE_ID, &tasks)
            .await?;
        let id = run.task_id.as_deref().context("bound task")?;
        let task = crate::task_manager::wait_for_terminal_state(
            &tasks,
            id,
            std::time::Duration::from_secs(5),
        )
        .await?;
        assert_eq!(task.status, crate::task_manager::TaskStatus::Completed);
        assert_eq!(task.model, "same-model");
        assert_eq!(task.model_provider.as_deref(), Some("custom"));
        assert_eq!(task.model_provider_id.as_deref(), Some("route-a"));
        let observed = observations.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].model, Some(task.model.clone()));
        assert_eq!(observed[0].model_provider_id, task.model_provider_id);
        assert_eq!(observed[0].auto_approve, Some(false));
        drop(observed);
        let reopened = AutomationManager::open_for_test(root.path().join("automations"))?;
        assert!(keepalive_readiness(&reopened, &config, None)?.1);
        assert_eq!(
            reopened.list_runs(OPERATE_KEEPALIVE_ID, None)?[0]
                .task_id
                .as_deref(),
            Some(id)
        );
        // A subsequent explicit edit cannot rewrite an accepted task binding.
        let mut edited = reopened.get_automation(OPERATE_KEEPALIVE_ID)?;
        edited.model = Some("replacement-model".into());
        edited.model_provider_id = Some("route-b".into());
        reopened.save_automation(&edited)?;
        assert_eq!(
            tasks
                .read_bound_task(id)?
                .context("accepted task")?
                .model_provider_id,
            Some("route-a".into())
        );
        tasks.shutdown();
        Ok(())
    }

    fn with_credentials(mut op: Operation) -> Operation {
        op.credentials_present = true;
        op.project();
        op
    }

    #[test]
    fn unbounded_start_matches_cwc_contract() {
        let op = with_credentials(Operation::new("Keep shipping honest slices", None));
        assert_eq!(op.schema_version, 1);
        assert!(op.burn_rate.is_none());
        assert_eq!(op.pace, OperatePace::Unbounded);
        assert_eq!(op.status, OperateStatus::IdleBlocked);
        assert_eq!(
            op.idle_blocked_reason,
            Some(OperateIdleReason::AwaitingLeadPlan)
        );
        assert!(!op.workers_admitted);
        assert!(op.lead_operator.model.is_empty(), "no model was assigned");
        let json = serde_json::to_value(&op).expect("json");
        assert!(json.get("burnRate").unwrap().is_null());
        assert_eq!(json["leadOperator"]["model"], "");
        assert_eq!(json["schemaVersion"], 1);
        assert!(json.get("leadPlan").unwrap().is_null());
        assert_eq!(json["idleBlockedReason"], "awaiting_lead_plan");
        assert_eq!(json["workersAdmitted"], false);
        assert!(json["id"].as_str().unwrap().starts_with("op_"));
    }

    #[test]
    fn operation_lead_label_follows_selection_and_planned_workers_stay_unassigned() -> Result<()> {
        let root = TempDir::new()?;
        let store = OperationStore::open(root.path())?;
        let operation = start_operation(
            &store,
            root.path(),
            Some("First bounded task\nSecond bounded task".into()),
            None,
            true,
            "selected-model",
        )?;
        assert_eq!(operation.lead_operator.model, "selected-model");
        let mut attached =
            attach_or_start_operation(&store, root.path(), None, None, true, "auto")?;
        assert_eq!(attached.id, operation.id);
        assert_eq!(attached.lead_operator.model, "auto");
        assert!(
            attached
                .roster
                .iter()
                .filter(|member| member.id == "lead")
                .all(|member| member.model == "auto")
        );
        attached.plan_from_direction();
        assert!(attached.roster.iter().any(|member| member.id != "lead"));
        assert!(
            attached
                .roster
                .iter()
                .filter(|member| member.id != "lead")
                .all(|member| member.model.is_empty())
        );
        assert!(
            store
                .load()?
                .context("saved operation")?
                .roster
                .iter()
                .filter(|member| member.id == "lead")
                .all(|member| member.model == "auto")
        );
        Ok(())
    }

    /// The cross-process lock sidecar is owner-only like the operation file.
    #[cfg(unix)]
    #[test]
    fn operation_lock_file_is_owner_only() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TempDir::new()?;
        let store = OperationStore::open(root.path())?;
        start_operation(&store, root.path(), None, None, true, "auto")?;
        let lock = root.path().join("current.json.lock");
        assert_eq!(fs::metadata(&lock)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn create_without_credentials_fails_closed() {
        let op = Operation::new("Keep shipping honest slices", None);
        assert_eq!(op.status, OperateStatus::IdleBlocked);
        assert_eq!(
            op.idle_blocked_reason,
            Some(OperateIdleReason::MissingCredentials)
        );
        assert!(!op.workers_admitted);
    }

    #[test]
    fn burn_rate_paces_and_never_stops() {
        let mut op = with_credentials(Operation::new(
            "Hold a $12/hr burn\nSecond slice\nThird slice",
            Some(12.0),
        ));
        op.plan_from_direction();
        assert_eq!(op.status, OperateStatus::Running);
        assert!(op.workers_admitted);
        keep_alive_observation(&mut op, Some(20.0), Some(80.0), None, None);
        assert_eq!(op.status, OperateStatus::Running);
        assert_eq!(op.pace, OperatePace::Throttle);
        // Throttle is a real dispatch cut, not a label: of the two planned
        // workers only one stays in flight while the operation keeps running.
        assert_eq!(op.writers_in_flight, OPERATE_THROTTLE_WRITERS);
        assert_eq!(
            op.roster
                .iter()
                .filter(|member| member.role == "worker" && member.state == "in_flight")
                .count(),
            OPERATE_THROTTLE_WRITERS
        );
        assert_eq!(
            op.roster
                .iter()
                .filter(|member| member.role == "worker" && member.state == "idle")
                .count(),
            1,
            "the worker past the throttle budget idles"
        );
        assert!(op.idle_blocked_reason.is_none());
        assert!(op.workers_admitted);
        let board = render_plan_board(&op);
        assert!(!board.contains("exhausted"));
        assert!(!board.contains("wallet"));
        assert_eq!(op.burn_rate.as_ref().unwrap().kind, "usd_per_hour");
        assert!((op.burn_rate.as_ref().unwrap().amount_usd_per_hour - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn hold_band_freezes_writer_width() {
        let mut op = with_credentials(Operation::new("one\ntwo\nthree", Some(12.0)));
        op.plan_from_direction();
        keep_alive_observation(&mut op, Some(12.0), None, None, None);
        assert_eq!(op.pace, OperatePace::Hold);
        assert_eq!(op.status, OperateStatus::Running);
        // Hold admits no new writers past the hold width; this plan has two
        // workers, so both stay in flight but the budget stops at two.
        assert_eq!(op.writers_in_flight, 2);
        let mut four = with_credentials(Operation::new("one\ntwo\nthree\nfour\nfive", Some(12.0)));
        four.plan_from_direction();
        keep_alive_observation(&mut four, Some(12.3), None, None, None);
        assert_eq!(four.pace, OperatePace::Hold);
        assert_eq!(
            four.writers_in_flight, OPERATE_HOLD_WRITERS,
            "hold never widens to the full writer width"
        );
    }

    #[test]
    fn under_rate_widens() {
        let mut op = with_credentials(Operation::new("one\ntwo\nthree\nfour", Some(12.0)));
        op.plan_from_direction();
        keep_alive_observation(&mut op, Some(1.0), None, None, None);
        assert_eq!(op.pace, OperatePace::Widen);
        assert_eq!(op.status, OperateStatus::Running);
        assert_eq!(
            op.writers_in_flight, OPERATE_MAX_WRITERS,
            "widen opens the full writer width"
        );
    }

    #[test]
    fn cancelled_field_matches_landed_cwc_contract() {
        // CWC `packages/contracts/src/operate.js` (`20de981`, PR #284) names
        // the field `cancelledAt` — `publicOperateRecord` always emits it,
        // as `""` before cancellation. There is no bare `cancelled` field.
        let dir = TempDir::new().expect("temp");
        let store = OperationStore::open(dir.path()).expect("store");
        let op = start_operation(
            &store,
            dir.path(),
            Some("Contract shape".into()),
            None,
            true,
            "selected-model",
        )
        .expect("start");
        let json = serde_json::to_value(&op).expect("json");
        assert_eq!(json["cancelledAt"], serde_json::json!(""));
        assert!(json.get("cancelled").is_none());

        let cancelled = cancel_operation(&store).expect("cancel").expect("present");
        let json = serde_json::to_value(&cancelled).expect("json");
        assert!(!json["cancelledAt"].as_str().unwrap().is_empty());
        assert!(json.get("cancelled").is_none());
    }

    #[test]
    fn direction_change_invalidates_stale_lead_plan() {
        let mut op = with_credentials(Operation::new("Old direction", None));
        op.plan_from_direction();
        assert_eq!(op.status, OperateStatus::Running);

        apply_operate_patch(
            &mut op,
            &serde_json::json!({ "direction": "Brand new direction" }),
        )
        .expect("patch");
        assert_eq!(op.direction, "Brand new direction");
        assert!(
            op.lead_plan.is_none(),
            "a changed direction must not leave superseded slices executing"
        );
        assert_eq!(op.status, OperateStatus::IdleBlocked);
        assert_eq!(
            op.idle_blocked_reason,
            Some(OperateIdleReason::AwaitingLeadPlan)
        );
        assert!(!op.workers_admitted);
        assert_eq!(op.writers_in_flight, 0);
    }

    #[test]
    fn same_direction_patch_keeps_plan() {
        let mut op = with_credentials(Operation::new("Steady", None));
        op.plan_from_direction();
        apply_operate_patch(&mut op, &serde_json::json!({ "direction": "Steady" })).expect("patch");
        assert!(op.lead_plan.is_some());
        assert_eq!(op.status, OperateStatus::Running);
    }

    #[test]
    fn put_plan_admits_worker_owners() {
        let mut op = with_credentials(Operation::new("Slice it", None));
        let plan = serde_json::json!({
            "slices": [
                { "id": "slice-1", "title": "Scout", "ownerId": "lead",
                  "dependsOn": [], "estCostUsd": 0.1, "startOffsetSec": 0, "durationSec": 600 },
                { "id": "slice-2", "title": "Build", "ownerId": "worker-7",
                  "dependsOn": ["slice-1"], "estCostUsd": 0.2, "startOffsetSec": 600, "durationSec": 1200 }
            ]
        });
        apply_operate_patch(&mut op, &serde_json::json!({ "leadPlan": plan })).expect("patch");
        assert!(
            op.roster.iter().any(|member| member.id == "worker-7"),
            "plan owners must join the roster or workers never dispatch"
        );
        assert_eq!(op.status, OperateStatus::Running);
        assert!(op.workers_admitted);
        // Idempotent: re-applying the same plan must not duplicate the owner.
        let before = op.roster.len();
        apply_operate_patch(&mut op, &serde_json::json!({ "leadPlan": plan })).expect("patch");
        assert_eq!(op.roster.len(), before);
    }

    #[test]
    fn attach_preserves_operation_record() {
        let dir = TempDir::new().expect("temp");
        let store = OperationStore::open(dir.path()).expect("store");
        let first = start_operation(
            &store,
            dir.path(),
            Some("Keep the lineage".into()),
            None,
            true,
            "selected-model",
        )
        .expect("start");
        let mut spent = first.clone();
        keep_alive_observation(&mut spent, Some(3.0), Some(42.0), None, None);
        store.save(&spent).expect("save spend");

        let reentered =
            attach_or_start_operation(&store, dir.path(), None, None, true, "selected-model")
                .expect("attach");
        assert_eq!(reentered.id, first.id, "re-entry must attach, not reset");
        assert_eq!(reentered.spent_usd, 42.0);
        assert_eq!(reentered.observed_burn_usd_per_hour, Some(3.0));

        // A cancelled record is terminal: re-entry starts a new operation.
        cancel_operation(&store).expect("cancel");
        let fresh =
            attach_or_start_operation(&store, dir.path(), None, None, true, "selected-model")
                .expect("restart");
        assert_ne!(fresh.id, first.id);
        // The fresh operation reuses the recorded direction and awaits its
        // own lead plan (CWC projects a plan-less record to idle_blocked).
        assert_eq!(fresh.direction, "Keep the lineage");
        assert_eq!(fresh.status, OperateStatus::IdleBlocked);
        assert_eq!(
            fresh.idle_blocked_reason,
            Some(OperateIdleReason::AwaitingLeadPlan)
        );
        assert_eq!(fresh.spent_usd, 0.0);
    }

    #[test]
    fn mutate_reloads_latest_record_under_lock() {
        let dir = TempDir::new().expect("temp");
        let writer = OperationStore::open(dir.path()).expect("store a");
        let reader = OperationStore::open(dir.path()).expect("store b");
        start_operation(
            &writer,
            dir.path(),
            Some("First".into()),
            None,
            true,
            "selected-model",
        )
        .expect("start");

        writer
            .mutate(|op| {
                apply_operate_patch(op, &serde_json::json!({ "direction": "Second" }))?;
                Ok(())
            })
            .expect("mutate a")
            .expect("present");
        reader
            .mutate(|op| {
                keep_alive_observation(op, Some(9.0), Some(5.0), None, None);
                Ok(())
            })
            .expect("mutate b")
            .expect("present");

        // The second store reloaded under the lock, so the first store's
        // direction write survives alongside the keepalive observation.
        let merged = writer.load().expect("load").expect("present");
        assert_eq!(merged.direction, "Second");
        assert_eq!(merged.spent_usd, 5.0);
        assert_eq!(merged.observed_burn_usd_per_hour, Some(9.0));
    }

    #[test]
    fn burn_rate_below_a_cent_is_rejected() {
        let err = parse_burn_rate(Some(&serde_json::json!(0.001))).expect_err("rejects");
        assert!(err.to_string().contains("at least $0.01/hr"));
        let zero_target = parse_burn_rate(Some(&serde_json::json!(0.004))).expect_err("rejects");
        assert!(zero_target.to_string().contains("at least $0.01/hr"));
    }

    #[test]
    fn keepalive_reuse_refreshes_cwds_and_kicks_first_lead_run() {
        let dir = TempDir::new().expect("temp");
        let manager = AutomationManager::open_for_test(dir.path().to_path_buf()).expect("manager");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("dir a");
        fs::create_dir_all(&workspace_b).expect("dir b");

        upsert_keepalive(&manager, &workspace_a, false, &route_fixture_config(), None)
            .expect("upsert a");
        let first = manager
            .get_automation(OPERATE_KEEPALIVE_ID)
            .expect("keepalive a");
        assert_eq!(first.cwds, vec![workspace_a.clone()]);

        upsert_keepalive(&manager, &workspace_b, true, &route_fixture_config(), None)
            .expect("upsert b");
        let second = manager
            .get_automation(OPERATE_KEEPALIVE_ID)
            .expect("keepalive b");
        assert_eq!(
            second.cwds,
            vec![workspace_b],
            "reuse must retarget the workspace scheduled runs execute in"
        );
        assert_eq!(
            second.next_run_at.map(|at| at <= Utc::now()),
            Some(true),
            "kick schedules the first lead run for the next scheduler tick"
        );
        assert_eq!(second.model.as_deref(), Some("same-model"));
        assert_eq!(second.mode.as_deref(), Some("operate"));
        assert_eq!(second.rrule, OPERATE_KEEPALIVE_RRULE);

        // Only one automation exists — no orphaned UUID-named twin.
        assert_eq!(
            manager
                .list_automations()
                .expect("list")
                .iter()
                .filter(|record| record.mode.as_deref() == Some("operate"))
                .count(),
            1
        );
    }

    #[test]
    fn cancel_pauses_keepalive_so_no_cost_accrues() {
        let dir = TempDir::new().expect("temp");
        let manager = AutomationManager::open_for_test(dir.path().to_path_buf()).expect("manager");
        upsert_keepalive(&manager, dir.path(), false, &route_fixture_config(), None)
            .expect("upsert");
        pause_keepalive(&manager).expect("pause");
        let paused = manager
            .get_automation(OPERATE_KEEPALIVE_ID)
            .expect("keepalive");
        assert_eq!(paused.status, AutomationStatus::Paused);
        assert_eq!(paused.next_run_at, None, "nothing fires after cancel");

        // Pausing is idempotent and a missing keepalive is not an error.
        pause_keepalive(&manager).expect("pause again");
        let empty =
            AutomationManager::open_for_test(dir.path().join("empty")).expect("empty manager");
        pause_keepalive(&empty).expect("missing keepalive is a no-op");
        assert!(!kick_keepalive(&empty).expect("kick missing"));

        // A fresh start reactivates the keepalive.
        upsert_keepalive(&manager, dir.path(), false, &route_fixture_config(), None)
            .expect("reactivate");
        let active = manager
            .get_automation(OPERATE_KEEPALIVE_ID)
            .expect("keepalive");
        assert_eq!(active.status, AutomationStatus::Active);

        // Kicks only touch an active keepalive.
        assert!(kick_keepalive(&manager).expect("kick"));
        pause_keepalive(&manager).expect("pause");
        assert!(!kick_keepalive(&manager).expect("kick paused"));
    }

    #[test]
    fn pause_keepalive_refuses_to_report_a_stop_it_cannot_confirm() {
        let dir = TempDir::new().expect("temp");
        let manager = AutomationManager::open_for_test(dir.path().to_path_buf()).expect("manager");
        upsert_keepalive(&manager, dir.path(), false, &route_fixture_config(), None)
            .expect("upsert");
        // Fault fixture: the keepalive record exists but cannot be parsed, so
        // whether it is still Active (and spending) is unknown.
        let record = dir
            .path()
            .join("automations")
            .join(format!("{OPERATE_KEEPALIVE_ID}.json"));
        assert!(record.exists(), "fixture targets the real record path");
        fs::write(&record, b"{\"status\": \"active\", truncated").expect("corrupt record");

        let error = pause_keepalive(&manager).expect_err("an unconfirmed stop must fail");
        assert!(
            format!("{error:#}").contains("could not be established"),
            "{error:#}"
        );
    }

    #[test]
    fn explicit_env_paths_reject_traversal() {
        assert!(explicit_file_path("  /tmp/does-not-exist.md ").is_none());
        assert!(explicit_file_path("").is_none());
        assert!(explicit_file_path("/tmp/../etc/passwd").is_none());
        // Keep the temp dir alive for the whole assertion: dropping it first
        // would delete the file under the path.
        let dir = TempDir::new().expect("temp");
        let checker = dir.path().join("check.py");
        fs::write(&checker, "# marker").expect("write");
        let found =
            explicit_file_path(checker.to_str().expect("utf8")).expect("regular file accepted");
        assert_eq!(found, checker);
        assert!(explicit_dir_path("../escape").is_none());
        assert!(explicit_dir_path("ops/inside").is_some());
    }

    #[test]
    fn missing_credentials_fail_closed() {
        let dir = TempDir::new().expect("temp");
        let store = OperationStore::open(dir.path()).expect("store");
        let op = start_operation(
            &store,
            dir.path(),
            Some("Do not spend silently".into()),
            None,
            false,
            "selected-model",
        )
        .expect("start");
        assert_eq!(op.status, OperateStatus::IdleBlocked);
        assert_eq!(
            op.idle_blocked_reason,
            Some(OperateIdleReason::MissingCredentials)
        );
        assert!(!op.workers_admitted);
        assert_eq!(op.writers_in_flight, 0);
    }

    #[test]
    fn cancel_stays_cancelled_through_keep_alive() {
        let dir = TempDir::new().expect("temp");
        let store = OperationStore::open(dir.path()).expect("store");
        start_operation(
            &store,
            dir.path(),
            Some("Stop".into()),
            None,
            true,
            "selected-model",
        )
        .expect("start");
        let cancelled = cancel_operation(&store).expect("cancel").expect("present");
        let mut kept = cancelled;
        keep_alive_observation(&mut kept, Some(40.0), Some(999.0), None, None);
        assert_eq!(kept.status, OperateStatus::Cancelled);
        assert!(!kept.workers_admitted);
    }

    #[test]
    fn lead_plan_is_the_gantt_model() {
        let mut op = with_credentials(Operation::new("Scout\nWrite", None));
        op.plan_from_direction();
        let plan = op.lead_plan.as_ref().expect("plan");
        assert_eq!(plan.slices.len(), 2);
        assert_eq!(plan.slices[0].owner_id, "lead");
        assert_eq!(plan.slices[1].depends_on, vec!["slice-1".to_string()]);
        assert_eq!(plan.slices[0].start_offset_sec, 0);
        assert!(plan.slices[0].duration_sec >= 1);
        let board = render_plan_board(&op);
        assert!(board.contains("gantt  time →"), "{board}");
        assert!(board.contains("leadPlan"), "{board}");
        assert!(board.contains("No cap"), "{board}");
    }

    #[test]
    fn empty_direction_is_idle_blocked() {
        let op = with_credentials(Operation::new("", None));
        assert_eq!(op.status, OperateStatus::IdleBlocked);
        assert_eq!(
            op.idle_blocked_reason,
            Some(OperateIdleReason::DirectionEmpty)
        );
    }

    #[test]
    fn human_gates_do_not_include_merge() {
        assert!(human_gate_for("deploy"));
        assert!(human_gate_for("billing"));
        assert!(!human_gate_for("merge"));
    }

    #[test]
    fn calls_landed_checker_flags() {
        assert_eq!(
            check_auto_merge_args("codewhale-hq/CodeWhale", "1234", "keel"),
            vec![
                "scripts/check-auto-merge.py",
                "--repo",
                "codewhale-hq/CodeWhale",
                "--pr",
                "1234",
                "--agent",
                "keel"
            ]
        );
        assert_eq!(
            auto_merge_pr_args("codewhale-hq/CodeWhale", "1234", "keel")[0],
            "scripts/auto-merge-pr.py"
        );
        let deny = evaluate_auto_merge(
            AutoMergeRequest {
                pr: "12",
                role: "keel",
                repo: "codewhale-hq/CodeWhale",
            },
            None,
        );
        assert!(matches!(deny, AutoMergeDecision::Deny { .. }));
        let _ = AUTO_MERGE_CHECKER_ENV;
        let _ = discover_auto_merge_checker(Path::new("/no-ops-here"));
    }

    #[test]
    fn auto_merge_request_fields_are_validated_before_spawn() {
        let ok = |repo, pr, role| {
            validate_auto_merge_request(&AutoMergeRequest { pr, role, repo }).is_ok()
        };
        assert!(ok("codewhale-hq/CodeWhale", "1234", "keel"));
        assert!(ok("a-b/c.d_e-f", "1", "scout_2"));
        for (repo, pr, role) in [
            ("Hmbown", "1", "keel"),
            ("a/b/../../x", "1", "keel"),
            ("-x/y", "1", "keel"),
            ("x/-y", "1", "keel"),
            ("x/..", "1", "keel"),
            ("x y/z", "1", "keel"),
            ("x/y", "0", "keel"),
            ("x/y", "-1", "keel"),
            ("x/y", "1 2", "keel"),
            ("x/y", "12345678901", "keel"),
            ("x/y", "", "keel"),
            ("x/y", "1", ""),
            ("x/y", "1", "--fixture=/x"),
            ("x/y", "1", "keel ops"),
        ] {
            assert!(
                !ok(repo, pr, role),
                "{repo:?} {pr:?} {role:?} must be rejected"
            );
        }
        // A malformed request is denied even when a checker exists, so the
        // checker is never spawned with it.
        let dir = TempDir::new().expect("temp");
        let checker = dir.path().join("check-auto-merge.py");
        fs::write(&checker, "import sys\nsys.exit(0)\n").expect("write");
        assert!(matches!(
            evaluate_auto_merge(
                AutoMergeRequest {
                    pr: "1",
                    role: "--policy=x",
                    repo: "x/y",
                },
                Some(&checker),
            ),
            AutoMergeDecision::Deny { .. }
        ));
    }

    #[test]
    fn checker_exit_zero_allows() {
        let dir = TempDir::new().expect("temp");
        let checker = dir.path().join("check-auto-merge.py");
        fs::write(
            &checker,
            "#!/usr/bin/env python3\nimport argparse, sys\np=argparse.ArgumentParser()\np.add_argument('--repo')\np.add_argument('--pr')\np.add_argument('--agent', required=True)\np.parse_args()\nsys.exit(0)\n",
        )
        .expect("write");
        assert_eq!(
            evaluate_auto_merge(
                AutoMergeRequest {
                    pr: "42",
                    role: "keel",
                    repo: "codewhale-hq/CodeWhale",
                },
                Some(&checker),
            ),
            AutoMergeDecision::Allow
        );
    }

    #[test]
    fn plan_board_localizes_chrome_but_not_contract_tokens() {
        let mut op = with_credentials(Operation::new("Scout\nWrite", None));
        op.plan_from_direction();
        let english = render_plan_board_locale(&op, codewhale_localization::Locale::En);
        assert!(english.contains("gantt  time →"), "{english}");
        assert!(english.contains("burn  No cap"), "{english}");
        let japanese = render_plan_board_locale(&op, codewhale_localization::Locale::Ja);
        assert!(japanese.contains("ガント"), "{japanese}");
        // Contract tokens stay verbatim in every locale.
        assert!(japanese.contains("slice-1"), "{japanese}");
        assert!(japanese.contains(&op.id), "{japanese}");
    }

    #[test]
    fn keepalive_automation_and_defaults() {
        let dir = TempDir::new().expect("temp");
        let manager = AutomationManager::open_for_test(dir.path().to_path_buf()).expect("manager");
        upsert_keepalive(&manager, dir.path(), false, &route_fixture_config(), None)
            .expect("upsert");
        let record = manager
            .get_automation(OPERATE_KEEPALIVE_ID)
            .expect("keepalive");
        assert_eq!(record.model.as_deref(), Some("same-model"));
        assert_eq!(record.mode.as_deref(), Some("operate"));
        assert_eq!(record.cwds, vec![dir.path().to_path_buf()]);
        assert_eq!(
            record.next_run_at, None,
            "without a kick the hourly recurrence owns the next run"
        );
        assert_eq!(OPERATE_MAX_WRITERS, 3);
    }
}
