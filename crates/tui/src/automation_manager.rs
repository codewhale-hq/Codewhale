//! Durable automation records and scheduler support.
//!
//! Automations are local-first recurring jobs that enqueue standard background
//! tasks. This module stores automation definitions and run history under
//! `~/.codewhale/automations` (or `DEEPSEEK_AUTOMATIONS_DIR` override).
//!
//! Deleting an automation keeps its terminal run history: terminal runs
//! (completed, failed, canceled) are moved under `<root>/archive/<id>/` before
//! the live paths are removed. The archive is bounded — it retains the most
//! recent [`ARCHIVE_RETAINED_TERMINAL_RUNS`] terminal runs per automation and
//! deletes older ones — and lives outside the `automations/`, `runs/`, and
//! `triggers/` trees, so live listings and scheduler scans never see it.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDateTime, TimeZone, Timelike, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::task_manager::{NewTaskRequest, SharedTaskManager, TaskStatus};
use codewhale_util::utils::spawn_supervised;

#[derive(Clone)]
pub struct ThreadEventSink(pub Arc<dyn Fn(String, &'static str, serde_json::Value) + Send + Sync>);

impl std::fmt::Debug for ThreadEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ThreadEventSink")
    }
}

/// Current automation record schema. `pub(crate)` so the Operate keepalive
/// can build a fixed-id record directly (no create/delete id swap).
// v2 pins provider identity. Older runtimes must reject a pinned definition
// instead of silently sending its model through their current provider.
pub(crate) const CURRENT_AUTOMATION_SCHEMA_VERSION: u32 = 3;
const CURRENT_RUN_SCHEMA_VERSION: u32 = 3;
const CURRENT_TRIGGER_SCHEMA_VERSION: u32 = 3;
const DEFAULT_AUTOMATION_MODE: &str = "agent";
const DEFAULT_AUTOMATION_ALLOW_SHELL: bool = false;
const DEFAULT_AUTOMATION_TRUST_MODE: bool = false;
const DEFAULT_AUTOMATION_AUTO_APPROVE: bool = false;
const DEFAULT_AUTOMATION_DELIVERY_MODE: AutomationDeliveryMode = AutomationDeliveryMode::Task;
/// Terminal runs retained in `<root>/archive/<automation_id>/` when an
/// automation is deleted; older archived runs are removed (newest-first by
/// `ended_at`, falling back to `created_at`).
const ARCHIVE_RETAINED_TERMINAL_RUNS: usize = 50;
pub const AUTOMATION_WATCHER_NO_REPORT_SENTINEL: &str = "NOTHING_TO_REPORT";
const MAX_HOURLY_SEARCH_STEPS: usize = 24 * 21;
const MAX_CRON_SEARCH_MINUTES: usize = 60 * 24 * 366 * 5;
const fn default_automation_schema_version() -> u32 {
    CURRENT_AUTOMATION_SCHEMA_VERSION
}

const fn default_run_schema_version() -> u32 {
    CURRENT_RUN_SCHEMA_VERSION
}

const fn default_trigger_schema_version() -> u32 {
    CURRENT_TRIGGER_SCHEMA_VERSION
}

// ── Delayed-trigger types ──────────────────────────────────────────────────

/// Status of a one-shot delayed trigger.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DelayedTriggerStatus {
    /// Waiting to fire.
    Pending,
    /// Durable admission owns this trigger; task acceptance is being recovered.
    Dispatching,
    /// The trigger was fired and a task was enqueued.
    Fired,
    /// The trigger was explicitly canceled before it fired.
    Canceled,
    /// The trigger fired but failed to enqueue a task.
    Failed,
}

/// A durable one-shot delayed continuation record.
///
/// Stored under `~/.codewhale/automations/triggers/{trigger_id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelayedTriggerRecord {
    #[serde(default = "default_trigger_schema_version")]
    pub schema_version: u32,
    pub trigger_id: String,
    /// Absolute UTC time at which the trigger should fire.
    pub fire_at: DateTime<Utc>,
    /// The message that will be submitted as a new task when the trigger fires.
    pub message: String,
    /// Working directory for the task that fires when the trigger trips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Session that scheduled this trigger. Missing legacy ownership fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
    pub status: DelayedTriggerStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional lineage: the trigger id that scheduled this one (for re-arm chains).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_trigger_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<AutomationDispatch>,
    /// Bound by the trusted service, independently of visibility ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_scope: Option<String>,
}

/// Input for creating a new delayed trigger.
#[derive(Debug, Clone)]
pub struct CreateDelayedTriggerRequest {
    /// Absolute fire time.  Callers must resolve `delay_minutes` → `fire_at`
    /// before calling this function.
    pub fire_at: DateTime<Utc>,
    /// Message to submit as a new task when the trigger fires.
    pub message: String,
    /// Optional workspace directory for the fired task.
    pub workspace: Option<PathBuf>,
    /// Session that owns controls and the task created when this trigger fires.
    pub owner_session_id: Option<String>,
    /// Optional parent trigger id for re-arm lineage tracking.
    pub parent_trigger_id: Option<String>,
    pub destination_thread_id: Option<String>,
}

// ── End delayed-trigger types ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationStatus {
    Active,
    Paused,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationDeliveryMode {
    #[default]
    Task,
    Watcher,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRecord {
    #[serde(default = "default_automation_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub rrule: String,
    #[serde(default)]
    pub cwds: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Exact provider provenance for a pinned model; absent on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_shell: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<AutomationDeliveryMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
    pub status: AutomationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    /// Bound by the trusted service, independently of visibility ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_scope: Option<String>,
    /// Increments on every definition mutation; absent on legacy records (0).
    #[serde(default)]
    pub revision: u64,
}

impl AutomationRecord {
    fn task_mode(&self) -> String {
        self.mode
            .as_deref()
            .map(str::trim)
            .filter(|mode| !mode.is_empty())
            .unwrap_or(DEFAULT_AUTOMATION_MODE)
            .to_string()
    }

    fn task_allow_shell(&self) -> bool {
        self.allow_shell.unwrap_or(DEFAULT_AUTOMATION_ALLOW_SHELL)
    }

    fn task_trust_mode(&self) -> bool {
        self.trust_mode.unwrap_or(DEFAULT_AUTOMATION_TRUST_MODE)
    }

    fn task_auto_approve(&self) -> bool {
        self.auto_approve.unwrap_or(DEFAULT_AUTOMATION_AUTO_APPROVE)
    }

    fn delivery_mode(&self) -> AutomationDeliveryMode {
        self.delivery_mode
            .unwrap_or(DEFAULT_AUTOMATION_DELIVERY_MODE)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationRunRecord {
    #[serde(default = "default_run_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub automation_id: String,
    pub scheduled_for: DateTime<Utc>,
    pub status: AutomationRunStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<AutomationDispatch>,
}

/// Immutable request and store bound to a durable occurrence before enqueue.
/// `accepted` records task promotion, not provider execution or completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationDispatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_scope: Option<String>,
    request: NewTaskRequest,
    task_data_dir: PathBuf,
    #[serde(default)]
    accepted: bool,
    #[serde(default)]
    delivery_mode: AutomationDeliveryMode,
    #[serde(default)]
    suppress_report: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<AdmittedSchedule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdmittedSchedule {
    updated_at: DateTime<Utc>,
    rrule: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAutomationRequest {
    pub name: String,
    pub prompt: String,
    pub rrule: String,
    #[serde(default)]
    pub cwds: Vec<PathBuf>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub model_provider_id: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub allow_shell: Option<bool>,
    #[serde(default)]
    pub trust_mode: Option<bool>,
    #[serde(default)]
    pub auto_approve: Option<bool>,
    #[serde(default)]
    pub delivery_mode: Option<AutomationDeliveryMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
    #[serde(default)]
    pub status: Option<AutomationStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateAutomationRequest {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub rrule: Option<String>,
    pub cwds: Option<Vec<PathBuf>>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub model_provider_id: Option<String>,
    pub mode: Option<String>,
    pub allow_shell: Option<bool>,
    pub trust_mode: Option<bool>,
    pub auto_approve: Option<bool>,
    pub delivery_mode: Option<AutomationDeliveryMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
    pub status: Option<AutomationStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Debug)]
pub struct RevisionConflict {
    pub current: u64,
}

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Automation revision conflict: current revision is {}",
            self.current
        )
    }
}

impl std::error::Error for RevisionConflict {}

fn check_expected_revision(record: &AutomationRecord, expected: Option<u64>) -> Result<()> {
    match expected {
        Some(expected) if expected != record.revision => Err(RevisionConflict {
            current: record.revision,
        }
        .into()),
        _ => Ok(()),
    }
}

pub const IDEMPOTENCY_TTL_HOURS: i64 = 24;
pub const IDEMPOTENCY_MAX_ENTRIES: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdempotencyEntry {
    key: String,
    fingerprint: String,
    status: u16,
    body: serde_json::Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum IdempotencyLookup {
    Miss,
    Replay {
        status: u16,
        body: serde_json::Value,
    },
    KeyReused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomationFrequency {
    Hourly,
    Weekly,
}

#[derive(Debug, Clone)]
pub enum AutomationSchedule {
    Once {
        at: DateTime<Utc>,
    },
    Hourly {
        interval_hours: u32,
        byday: Option<Vec<Weekday>>,
        anchor_hour: Option<u32>,
        anchor_minute: Option<u32>,
    },
    Weekly {
        byday: Vec<Weekday>,
        byhour: u32,
        byminute: u32,
    },
    Cron {
        expr: String,
    },
}

impl AutomationSchedule {
    pub fn parse_rrule(rrule: &str) -> Result<Self> {
        let mut parts: BTreeMap<String, String> = BTreeMap::new();
        for raw in rrule.split(';') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            let Some((k, v)) = item.split_once('=') else {
                bail!("Invalid RRULE segment '{item}'");
            };
            parts.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
        }

        let freq = match parts
            .get("FREQ")
            .map(|value| value.trim().to_ascii_uppercase())
            .as_deref()
        {
            Some("ONCE") => return parse_once_schedule(&parts),
            Some("HOURLY") => AutomationFrequency::Hourly,
            Some("WEEKLY") => AutomationFrequency::Weekly,
            Some("CRON") => return parse_cron_schedule(&parts),
            Some(other) => {
                bail!("Unsupported RRULE FREQ '{other}'. Supported: ONCE, HOURLY, WEEKLY, and CRON")
            }
            None => bail!("RRULE must include FREQ"),
        };

        match freq {
            AutomationFrequency::Hourly => {
                for key in parts.keys() {
                    if key != "FREQ"
                        && key != "INTERVAL"
                        && key != "BYDAY"
                        && key != "BYHOUR"
                        && key != "BYMINUTE"
                    {
                        bail!(
                            "Unsupported RRULE field '{key}' for HOURLY. Allowed: FREQ,INTERVAL,BYDAY,BYHOUR,BYMINUTE"
                        );
                    }
                }
                let interval_hours = parts
                    .get("INTERVAL")
                    .map(|v| v.parse::<u32>())
                    .transpose()
                    .context("Failed to parse INTERVAL")?
                    .unwrap_or(1);
                if interval_hours == 0 {
                    bail!("INTERVAL must be >= 1 for HOURLY schedules");
                }
                let byday = parts
                    .get("BYDAY")
                    .map(|value| parse_byday(&value.to_ascii_uppercase()))
                    .transpose()?;
                let anchor_hour = parts
                    .get("BYHOUR")
                    .map(|value| value.parse::<u32>())
                    .transpose()
                    .context("Failed to parse BYHOUR")?;
                let anchor_minute = parts
                    .get("BYMINUTE")
                    .map(|value| value.parse::<u32>())
                    .transpose()
                    .context("Failed to parse BYMINUTE")?;
                if anchor_hour.is_some_and(|hour| hour > 23) {
                    bail!("BYHOUR must be between 0 and 23");
                }
                if anchor_minute.is_some_and(|minute| minute > 59) {
                    bail!("BYMINUTE must be between 0 and 59");
                }
                Ok(Self::Hourly {
                    interval_hours,
                    byday,
                    anchor_hour,
                    anchor_minute,
                })
            }
            AutomationFrequency::Weekly => {
                for key in parts.keys() {
                    if key != "FREQ" && key != "BYDAY" && key != "BYHOUR" && key != "BYMINUTE" {
                        bail!(
                            "Unsupported RRULE field '{key}' for WEEKLY. Allowed: FREQ,BYDAY,BYHOUR,BYMINUTE"
                        );
                    }
                }
                let byday_raw = parts
                    .get("BYDAY")
                    .ok_or_else(|| anyhow::anyhow!("WEEKLY schedules require BYDAY"))?;
                let byday = parse_byday(&byday_raw.to_ascii_uppercase())?;
                if byday.is_empty() {
                    bail!("BYDAY cannot be empty for WEEKLY schedules");
                }
                let byhour = parts
                    .get("BYHOUR")
                    .ok_or_else(|| anyhow::anyhow!("WEEKLY schedules require BYHOUR"))?
                    .parse::<u32>()
                    .context("Failed to parse BYHOUR")?;
                let byminute = parts
                    .get("BYMINUTE")
                    .ok_or_else(|| anyhow::anyhow!("WEEKLY schedules require BYMINUTE"))?
                    .parse::<u32>()
                    .context("Failed to parse BYMINUTE")?;

                if byhour > 23 {
                    bail!("BYHOUR must be between 0 and 23");
                }
                if byminute > 59 {
                    bail!("BYMINUTE must be between 0 and 59");
                }

                Ok(Self::Weekly {
                    byday,
                    byhour,
                    byminute,
                })
            }
        }
    }

    pub(crate) fn next_after_with_anchor(
        &self,
        after: DateTime<Utc>,
        anchor_reference: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        self.next_after_in_timezone(after, anchor_reference, &Local)
    }

    fn next_after_in_timezone<Tz: TimeZone>(
        &self,
        after: DateTime<Utc>,
        anchor_reference: DateTime<Utc>,
        timezone: &Tz,
    ) -> Result<DateTime<Utc>> {
        let local_after = after.with_timezone(timezone);
        match self {
            Self::Once { at } => {
                if *at > after {
                    Ok(*at)
                } else {
                    bail!(
                        "Once schedule has no future run after {}",
                        after.to_rfc3339()
                    )
                }
            }
            Self::Hourly {
                interval_hours,
                byday,
                anchor_hour,
                anchor_minute,
            } => {
                if anchor_hour.is_some() || anchor_minute.is_some() {
                    let local_anchor_reference = anchor_reference.with_timezone(timezone);
                    let hour = anchor_hour.unwrap_or(local_anchor_reference.hour());
                    let minute = anchor_minute.unwrap_or(0);
                    let anchor_naive = local_anchor_reference
                        .date_naive()
                        .and_hms_opt(hour, minute, 0)
                        .ok_or_else(|| anyhow::anyhow!("Unable to construct HOURLY anchor"))?;
                    let interval_seconds = i64::from(*interval_hours) * 60 * 60;
                    let elapsed_seconds = local_after
                        .naive_local()
                        .signed_duration_since(anchor_naive)
                        .num_seconds();
                    let mut steps = if elapsed_seconds < 0 {
                        0
                    } else {
                        elapsed_seconds / interval_seconds + 1
                    };

                    for _ in 0..MAX_HOURLY_SEARCH_STEPS {
                        let hours = i64::from(*interval_hours)
                            .checked_mul(steps)
                            .ok_or_else(|| anyhow::anyhow!("HOURLY schedule exceeded its range"))?;
                        let delta = Duration::try_hours(hours)
                            .ok_or_else(|| anyhow::anyhow!("HOURLY schedule exceeded its range"))?;
                        let candidate_naive = anchor_naive
                            .checked_add_signed(delta)
                            .ok_or_else(|| anyhow::anyhow!("HOURLY schedule exceeded its range"))?;

                        if byday
                            .as_ref()
                            .is_none_or(|days| days.contains(&candidate_naive.weekday()))
                            && let Some(candidate) =
                                resolve_local_datetime(timezone, candidate_naive)
                        {
                            let candidate = candidate.with_timezone(&Utc);
                            if candidate > after {
                                return Ok(candidate);
                            }
                        }

                        steps = steps
                            .checked_add(1)
                            .ok_or_else(|| anyhow::anyhow!("HOURLY schedule exceeded its range"))?;
                    }
                    bail!("Unable to compute next anchored HOURLY run");
                }

                let after_second = local_after.second();
                let after_nanosecond = local_after.nanosecond();
                let mut candidate = local_after + Duration::hours(i64::from(*interval_hours))
                    - Duration::seconds(i64::from(after_second))
                    - Duration::nanoseconds(i64::from(after_nanosecond));

                if let Some(days) = byday {
                    for _ in 0..(24 * 21) {
                        if days.contains(&candidate.weekday()) {
                            return Ok(candidate.with_timezone(&Utc));
                        }
                        candidate += Duration::hours(i64::from(*interval_hours));
                    }
                    bail!("Unable to compute next HOURLY run for BYDAY filter");
                }

                Ok(candidate.with_timezone(&Utc))
            }
            Self::Weekly {
                byday,
                byhour,
                byminute,
            } => {
                for day_offset in 0..15 {
                    let date = local_after.date_naive() + Duration::days(i64::from(day_offset));
                    if !byday.contains(&date.weekday()) {
                        continue;
                    }
                    let Some(candidate_naive) = date.and_hms_opt(*byhour, *byminute, 0) else {
                        continue;
                    };
                    if let Some(candidate) = resolve_local_datetime(timezone, candidate_naive)
                        && candidate.with_timezone(&Utc) > after
                    {
                        return Ok(candidate.with_timezone(&Utc));
                    }
                }
                bail!("Unable to compute next WEEKLY run");
            }
            Self::Cron { expr } => {
                let cron = ParsedCronExpr::parse(expr)?;
                let mut candidate_naive = local_after
                    .naive_local()
                    .with_second(0)
                    .and_then(|dt| dt.with_nanosecond(0))
                    .ok_or_else(|| anyhow::anyhow!("Unable to round CRON search start"))?
                    .checked_add_signed(Duration::minutes(1))
                    .ok_or_else(|| anyhow::anyhow!("CRON schedule exceeded its range"))?;

                for _ in 0..MAX_CRON_SEARCH_MINUTES {
                    if cron.matches(candidate_naive)
                        && let Some(candidate) = resolve_local_datetime(timezone, candidate_naive)
                    {
                        let candidate = candidate.with_timezone(&Utc);
                        if candidate > after {
                            return Ok(candidate);
                        }
                    }
                    candidate_naive = candidate_naive
                        .checked_add_signed(Duration::minutes(1))
                        .ok_or_else(|| anyhow::anyhow!("CRON schedule exceeded its range"))?;
                }
                bail!("Unable to compute next CRON run within 5 years");
            }
        }
    }

    fn next_after_slot(
        &self,
        slot: DateTime<Utc>,
        anchor_reference: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        match self {
            Self::Once { .. } => Ok(None),
            _ => self
                .next_after_with_anchor(slot, anchor_reference)
                .map(Some),
        }
    }

    /// First slot after `slot` that is still in the future at `now`.
    ///
    /// Missed slots coalesce: downtime, a paused window, or an in-flight
    /// occurrence earn one receipt for the oldest owed slot, then the
    /// schedule resumes on its own grid instead of replaying one stale slot
    /// per tick. Calendar-anchored schedules (anchored HOURLY, WEEKLY, CRON)
    /// live on a fixed wall-clock grid, so the first slot after `now` is
    /// exactly the slot plain chaining would converge to; computing from
    /// `now` directly skips the whole backlog in one step. Unanchored HOURLY
    /// is a relative cadence with no calendar grid — hop along its
    /// established `slot + k * interval` chain so a late recovery does not
    /// re-phase the schedule to the recovery instant.
    fn next_unskipped_slot(
        &self,
        slot: DateTime<Utc>,
        now: DateTime<Utc>,
        anchor_reference: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        if let Self::Hourly {
            interval_hours,
            anchor_hour: None,
            anchor_minute: None,
            ..
        } = self
        {
            let first = self.next_after_with_anchor(slot, anchor_reference)?;
            if first > now {
                return Ok(Some(first));
            }
            // Jump whole intervals on the established UTC grid, then reuse
            // the schedule's weekday filter for the next eligible slot.
            // Minute normalization happens in the first advance above.
            let interval_seconds = i64::from(*interval_hours) * 60 * 60;
            let elapsed = (now - first).num_seconds();
            let delta = Duration::seconds(elapsed / interval_seconds * interval_seconds);
            let previous = first
                .checked_add_signed(delta)
                .context("HOURLY catch-up exceeded its range")?;
            self.next_after_slot(previous, anchor_reference)
        } else {
            self.next_after_slot(slot.max(now), anchor_reference)
        }
    }
}

/// Resolve one calendar-local schedule slot.
///
/// Nonexistent wall times in a forward clock change are skipped rather than
/// shifted to a different clock time. Ambiguous wall times in a backward clock
/// change use the first occurrence only, preventing a recurring automation from
/// running twice for one calendar slot.
fn resolve_local_datetime<Tz: TimeZone>(
    timezone: &Tz,
    naive: NaiveDateTime,
) -> Option<DateTime<Tz>> {
    timezone.from_local_datetime(&naive).earliest()
}

fn parse_byday(value: &str) -> Result<Vec<Weekday>> {
    let mut days = Vec::new();
    for token in value.split(',') {
        let day = match token.trim().to_ascii_uppercase().as_str() {
            "MO" => Weekday::Mon,
            "TU" => Weekday::Tue,
            "WE" => Weekday::Wed,
            "TH" => Weekday::Thu,
            "FR" => Weekday::Fri,
            "SA" => Weekday::Sat,
            "SU" => Weekday::Sun,
            other => bail!("Invalid BYDAY value '{other}'"),
        };
        if !days.contains(&day) {
            days.push(day);
        }
    }
    Ok(days)
}

fn parse_once_schedule(parts: &BTreeMap<String, String>) -> Result<AutomationSchedule> {
    for key in parts.keys() {
        if key != "FREQ" && key != "AT" {
            bail!("Unsupported RRULE field '{key}' for ONCE. Allowed: FREQ,AT");
        }
    }
    let raw_at = parts
        .get("AT")
        .ok_or_else(|| anyhow::anyhow!("ONCE schedules require AT"))?;
    let at = parse_once_at(raw_at)?;
    Ok(AutomationSchedule::Once { at })
}

fn parse_cron_schedule(parts: &BTreeMap<String, String>) -> Result<AutomationSchedule> {
    for key in parts.keys() {
        if key != "FREQ" && key != "EXPR" {
            bail!("Unsupported RRULE field '{key}' for CRON. Allowed: FREQ,EXPR");
        }
    }
    let expr = parts
        .get("EXPR")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("CRON schedules require EXPR"))?;
    ParsedCronExpr::parse(&expr)?;
    Ok(AutomationSchedule::Cron { expr })
}

fn parse_once_at(raw: &str) -> Result<DateTime<Utc>> {
    let trimmed = raw.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(at.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, format) {
            return resolve_local_datetime(&Local, naive)
                .map(|value| value.with_timezone(&Utc))
                .ok_or_else(|| anyhow::anyhow!("ONCE local time does not exist: {trimmed}"));
        }
    }
    bail!("Failed to parse ONCE AT '{trimmed}'. Use local YYYY-MM-DDTHH:MM[:SS] or RFC3339")
}

#[derive(Debug, Clone)]
struct ParsedCronExpr {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

impl ParsedCronExpr {
    fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            bail!(
                "CRON EXPR must have exactly 5 fields: minute hour day-of-month month day-of-week"
            );
        }
        let parsed = Self {
            minute: CronField::parse(fields[0], 0, 59, CronNameMap::none(), "minute")?,
            hour: CronField::parse(fields[1], 0, 23, CronNameMap::none(), "hour")?,
            day_of_month: CronField::parse(fields[2], 1, 31, CronNameMap::none(), "day-of-month")?,
            month: CronField::parse(fields[3], 1, 12, CronNameMap::month(), "month")?,
            day_of_week: CronField::parse(fields[4], 0, 7, CronNameMap::weekday(), "day-of-week")?
                .normalized_day_of_week(),
        };
        parsed.validate_date_space()?;
        Ok(parsed)
    }

    fn matches(&self, candidate: NaiveDateTime) -> bool {
        if !self.minute.contains(candidate.minute())
            || !self.hour.contains(candidate.hour())
            || !self.month.contains(candidate.month())
        {
            return false;
        }

        let day_of_month = self.day_of_month.contains(candidate.day());
        let weekday = self
            .day_of_week
            .contains(weekday_to_cron(candidate.weekday()));
        if self.day_of_month.is_wildcard && self.day_of_week.is_wildcard {
            true
        } else if self.day_of_month.is_wildcard {
            weekday
        } else if self.day_of_week.is_wildcard {
            day_of_month
        } else {
            day_of_month || weekday
        }
    }

    fn validate_date_space(&self) -> Result<()> {
        if self.day_of_month.is_wildcard {
            return Ok(());
        }
        let months = self.month.values();
        let days = self.day_of_month.values();
        let valid = months.iter().copied().any(|month| {
            let common = days_in_month(2025, month);
            let leap = days_in_month(2024, month);
            days.iter().copied().any(|day| day <= common || day <= leap)
        });
        if valid {
            Ok(())
        } else {
            bail!("CRON EXPR day-of-month/month combination can never occur")
        }
    }
}

#[derive(Debug, Clone)]
struct CronField {
    values: Vec<u32>,
    is_wildcard: bool,
}

impl CronField {
    fn parse(raw: &str, min: u32, max: u32, names: CronNameMap, field_name: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("CRON {field_name} field must not be empty");
        }
        let mut values = Vec::new();
        let is_wildcard = trimmed == "*";
        for part in trimmed.split(',') {
            let part = part.trim();
            if part.is_empty() {
                bail!("CRON {field_name} field contains an empty list item");
            }
            let (base, step) = if let Some((base, step)) = part.split_once('/') {
                let step = step
                    .trim()
                    .parse::<u32>()
                    .with_context(|| format!("Failed to parse CRON {field_name} step"))?;
                if step == 0 {
                    bail!("CRON {field_name} step must be >= 1");
                }
                (base.trim(), step)
            } else {
                (part, 1)
            };

            let range = if base == "*" {
                (min, max)
            } else if let Some((start, end)) = base.split_once('-') {
                let start = parse_cron_atom(start.trim(), min, max, names, field_name)?;
                let end = parse_cron_atom(end.trim(), min, max, names, field_name)?;
                if start > end {
                    bail!("CRON {field_name} range start must be <= end");
                }
                (start, end)
            } else {
                let start = parse_cron_atom(base, min, max, names, field_name)?;
                if part.contains('/') {
                    (start, max)
                } else {
                    (start, start)
                }
            };

            let mut current = range.0;
            while current <= range.1 {
                if !values.contains(&current) {
                    values.push(current);
                }
                let Some(next) = current.checked_add(step) else {
                    break;
                };
                if next <= current {
                    break;
                }
                current = next;
            }
        }
        values.sort_unstable();
        Ok(Self {
            values,
            is_wildcard,
        })
    }

    fn normalized_day_of_week(mut self) -> Self {
        for value in &mut self.values {
            if *value == 7 {
                *value = 0;
            }
        }
        self.values.sort_unstable();
        self.values.dedup();
        self
    }

    fn contains(&self, value: u32) -> bool {
        self.values.binary_search(&value).is_ok()
    }

    fn values(&self) -> &[u32] {
        &self.values
    }
}

#[derive(Debug, Clone, Copy)]
struct CronNameMap(&'static [(&'static str, u32)]);

impl CronNameMap {
    const fn none() -> Self {
        Self(&[])
    }

    const fn month() -> Self {
        Self(&[
            ("JAN", 1),
            ("FEB", 2),
            ("MAR", 3),
            ("APR", 4),
            ("MAY", 5),
            ("JUN", 6),
            ("JUL", 7),
            ("AUG", 8),
            ("SEP", 9),
            ("OCT", 10),
            ("NOV", 11),
            ("DEC", 12),
        ])
    }

    const fn weekday() -> Self {
        Self(&[
            ("SUN", 0),
            ("MON", 1),
            ("TUE", 2),
            ("WED", 3),
            ("THU", 4),
            ("FRI", 5),
            ("SAT", 6),
        ])
    }

    fn lookup(self, token: &str) -> Option<u32> {
        let needle = token.trim().to_ascii_uppercase();
        self.0
            .iter()
            .find_map(|(name, value)| (*name == needle).then_some(*value))
    }
}

fn parse_cron_atom(
    raw: &str,
    min: u32,
    max: u32,
    names: CronNameMap,
    field_name: &str,
) -> Result<u32> {
    let value = names
        .lookup(raw)
        .or_else(|| raw.parse::<u32>().ok())
        .ok_or_else(|| anyhow::anyhow!("Invalid CRON {field_name} value '{raw}'"))?;
    if !(min..=max).contains(&value) {
        bail!("CRON {field_name} value {value} is out of range {min}-{max}");
    }
    Ok(value)
}

fn weekday_to_cron(day: Weekday) -> u32 {
    match day {
        Weekday::Sun => 0,
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

#[derive(Debug, Clone)]
pub struct AutomationManager {
    execution_scope: Option<String>,
    thread_events: Option<ThreadEventSink>,
    automations_dir: PathBuf,
    runs_dir: PathBuf,
    triggers_dir: PathBuf,
    archive_dir: PathBuf,
}

impl AutomationManager {
    fn open_lock(&self, name: &str) -> Result<fd_lock::RwLock<fs::File>> {
        let path = self
            .automations_dir
            .parent()
            .context("automation root")?
            .join(name);
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("Automation lock must be a regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.nlink() != 1 {
                bail!("Automation lock must not have hard links");
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & 0x400 != 0 {
                bail!("Automation lock must not be a reparse point");
            }
        }
        Ok(fd_lock::RwLock::new(file))
    }

    fn with_transaction<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut lock = self.open_lock("state.lock")?;
        let _guard = lock.write().context("lock automation state")?;
        operation()
    }

    /// Short read/modify/write transaction shared with scheduler admission.
    /// Returning None leaves an absent record absent; it does not delete one.
    pub(crate) fn edit_automation(
        &self,
        id: &str,
        edit: impl FnOnce(Option<AutomationRecord>) -> Result<Option<AutomationRecord>>,
    ) -> Result<Option<AutomationRecord>> {
        self.with_transaction(|| {
            let current = if self.automation_path(id)?.try_exists()? {
                Some(self.get_automation(id)?)
            } else {
                None
            };
            let edited = edit(current)?;
            if let Some(record) = &edited {
                if record.id != id {
                    bail!("Automation transaction cannot replace its identity");
                }
                self.save_automation_unlocked(record)?;
            }
            Ok(edited)
        })
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        let automations_dir = root.join("automations");
        let runs_dir = root.join("runs");
        let triggers_dir = root.join("triggers");
        let archive_dir = root.join("archive");
        fs::create_dir_all(&automations_dir)
            .with_context(|| format!("Failed to create {}", automations_dir.display()))?;
        fs::create_dir_all(&runs_dir)
            .with_context(|| format!("Failed to create {}", runs_dir.display()))?;
        fs::create_dir_all(&triggers_dir)
            .with_context(|| format!("Failed to create {}", triggers_dir.display()))?;
        fs::create_dir_all(&archive_dir)
            .with_context(|| format!("Failed to create {}", archive_dir.display()))?;
        Ok(Self {
            execution_scope: None,
            thread_events: None,
            automations_dir,
            runs_dir,
            triggers_dir,
            archive_dir,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(root: PathBuf) -> Result<Self> {
        let mut manager = Self::open(root)?;
        manager.execution_scope = Some(crate::task_manager::test_execution_scope("test"));
        Ok(manager)
    }

    pub(crate) fn bind_task_manager(
        &mut self,
        tasks: &crate::task_manager::TaskManager,
    ) -> Result<()> {
        if self
            .execution_scope
            .as_deref()
            .is_some_and(|scope| scope != tasks.execution_scope())
        {
            bail!("Automation service belongs to another Runtime scope");
        }
        self.execution_scope = Some(tasks.execution_scope().to_string());
        Ok(())
    }

    pub(crate) fn set_thread_event_sink(&mut self, sink: ThreadEventSink) {
        self.thread_events = Some(sink);
    }

    fn emit_thread_event(
        &self,
        thread_id: Option<&str>,
        event: &'static str,
        payload: serde_json::Value,
    ) {
        if let (Some(sink), Some(thread_id)) = (self.thread_events.as_ref(), thread_id) {
            (sink.0)(thread_id.to_string(), event, payload);
        }
    }

    fn emit_automation_event(&self, event: &'static str, record: &AutomationRecord) {
        self.emit_thread_event(
            record.destination_thread_id.as_deref(),
            event,
            serde_json::json!({
                "automation_id": record.id,
                "name": record.name,
                "rrule": record.rrule,
                "status": record.status,
                "next_run_at": record.next_run_at,
            }),
        );
    }

    pub(crate) fn emit_trigger_event(&self, event: &'static str, record: &DelayedTriggerRecord) {
        self.emit_thread_event(
            record.destination_thread_id.as_deref(),
            event,
            serde_json::json!({
                "trigger_id": record.trigger_id,
                "message": record.message,
                "fire_at": record.fire_at,
                "status": record.status,
                "task_id": record.task_id,
            }),
        );
    }

    pub(crate) fn execution_scope(&self) -> Option<&str> {
        self.execution_scope.as_deref()
    }

    fn eligible_scope(&self, scope: Option<&str>) -> bool {
        scope.is_some() && scope == self.execution_scope()
    }

    /// Explicit control may bind an unbound definition; saved admissions never
    /// read this field back from the definition during recovery.
    fn adopt_for_run(&self, automation: &mut AutomationRecord) -> Result<()> {
        let scope = self
            .execution_scope()
            .context("Automation execution ownership is unverified")?;
        if let Some(bound) = &automation.execution_scope {
            if bound != scope {
                bail!("Automation belongs to another Runtime execution scope");
            }
        } else {
            automation.execution_scope = Some(scope.to_string());
            automation.schema_version = CURRENT_AUTOMATION_SCHEMA_VERSION;
            automation.updated_at = Utc::now();
            if automation.status == AutomationStatus::Active {
                let schedule = AutomationSchedule::parse_rrule(&automation.rrule)?;
                automation.next_run_at =
                    match schedule.next_after_with_anchor(Utc::now(), automation.created_at) {
                        Ok(next) => Some(next),
                        Err(_) if matches!(schedule, AutomationSchedule::Once { .. }) => {
                            automation.status = AutomationStatus::Paused;
                            None
                        }
                        Err(error) => return Err(error),
                    };
            }
            self.save_automation_unlocked(automation)?;
        }
        Ok(())
    }

    pub fn default_location() -> Result<Self> {
        Self::open(default_automations_dir())
    }

    fn automation_path(&self, id: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("automation id", id)?;
        Ok(self.automations_dir.join(format!("{id}.json")))
    }

    fn runs_dir_for(&self, automation_id: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("automation id", automation_id)?;
        Ok(self.runs_dir.join(automation_id))
    }

    fn trigger_path(&self, trigger_id: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("trigger id", trigger_id)?;
        Ok(self.triggers_dir.join(format!("{trigger_id}.json")))
    }

    /// Current run file name: `{sortable-created-at}-{run_id}.json`. The
    /// fixed-width timestamp prefix makes directory listings sort
    /// chronologically without reading file contents (see [`Self::list_runs`]).
    fn run_path(&self, run: &AutomationRunRecord) -> Result<PathBuf> {
        ensure_safe_storage_id("run id", &run.id)?;
        Ok(self.runs_dir_for(&run.automation_id)?.join(format!(
            "{}-{}.json",
            run_file_stamp(run.created_at),
            run.id
        )))
    }

    /// Pre-sortable-name run file: `{run_id}.json` (run ids are UUIDs, so
    /// these carry no ordering hint and must be read to learn `created_at`).
    fn legacy_run_path(&self, automation_id: &str, run_id: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("run id", run_id)?;
        Ok(self
            .runs_dir_for(automation_id)?
            .join(format!("{run_id}.json")))
    }

    /// Terminal-run archive for one automation. Lives beside — never inside —
    /// the live `runs/` tree so `list_runs` and scheduler scans cannot see it.
    fn archive_dir_for(&self, automation_id: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("automation id", automation_id)?;
        Ok(self.archive_dir.join(automation_id))
    }

    /// Archived run file name: the same sortable `{stamp}-{run_id}.json` shape
    /// as live runs, so legacy-named runs are normalized when archived.
    fn archive_run_path(&self, run: &AutomationRunRecord) -> Result<PathBuf> {
        ensure_safe_storage_id("run id", &run.id)?;
        Ok(self.archive_dir_for(&run.automation_id)?.join(format!(
            "{}-{}.json",
            run_file_stamp(run.created_at),
            run.id
        )))
    }

    pub fn create_automation(&self, req: CreateAutomationRequest) -> Result<AutomationRecord> {
        validate_name_and_prompt(&req.name, &req.prompt)?;
        let schedule = AutomationSchedule::parse_rrule(&req.rrule)?;
        let now = Utc::now();
        let status = req.status.unwrap_or(AutomationStatus::Active);
        let next_run_at = if matches!(status, AutomationStatus::Active) {
            Some(schedule.next_after_with_anchor(now, now)?)
        } else {
            None
        };

        let record = AutomationRecord {
            schema_version: CURRENT_AUTOMATION_SCHEMA_VERSION,
            execution_scope: self.execution_scope.clone(),
            id: Uuid::new_v4().to_string(),
            name: req.name.trim().to_string(),
            prompt: req.prompt.trim().to_string(),
            rrule: req.rrule.trim().to_ascii_uppercase(),
            cwds: req.cwds,
            model: normalize_optional_string(req.model),
            model_provider: normalize_optional_string(req.model_provider),
            model_provider_id: normalize_optional_string(req.model_provider_id),
            mode: normalize_optional_string(req.mode),
            allow_shell: req.allow_shell,
            trust_mode: req.trust_mode,
            auto_approve: req.auto_approve,
            delivery_mode: req.delivery_mode,
            destination_thread_id: normalize_optional_string(req.destination_thread_id),
            status,
            created_at: now,
            updated_at: now,
            next_run_at,
            last_run_at: None,
            revision: 1,
        };

        self.save_automation(&record)?;
        self.emit_automation_event("automation.created", &record);
        Ok(record)
    }

    pub fn get_automation(&self, id: &str) -> Result<AutomationRecord> {
        let path = self.automation_path(id)?;
        read_automation_file(&path)
    }

    pub fn save_automation(&self, record: &AutomationRecord) -> Result<()> {
        self.with_transaction(|| self.save_automation_unlocked(record))
    }

    fn save_automation_unlocked(&self, record: &AutomationRecord) -> Result<()> {
        if record.model_provider.is_some() || record.model_provider_id.is_some() {
            if record
                .model
                .as_deref()
                .is_none_or(|model| model.trim().is_empty())
            {
                bail!("A pinned automation provider requires an explicit model");
            }
            let mut record = record.clone();
            record.schema_version = record.schema_version.max(CURRENT_AUTOMATION_SCHEMA_VERSION);
            return write_json_atomic(&self.automation_path(&record.id)?, &record);
        }
        write_json_atomic(&self.automation_path(&record.id)?, record)
    }

    pub fn list_automations(&self) -> Result<Vec<AutomationRecord>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.automations_dir)
            .with_context(|| format!("Failed to read {}", self.automations_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let record = read_automation_file(&path)?;
            out.push(record);
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
        Ok(out)
    }

    pub fn update_automation(
        &self,
        id: &str,
        req: UpdateAutomationRequest,
    ) -> Result<AutomationRecord> {
        self.with_transaction(|| self.update_automation_unlocked(id, req))
    }

    fn update_automation_unlocked(
        &self,
        id: &str,
        req: UpdateAutomationRequest,
    ) -> Result<AutomationRecord> {
        let mut existing = self.get_automation(id)?;
        check_expected_revision(&existing, req.expected_revision)?;
        let adopting = existing.execution_scope.is_none()
            && self.execution_scope.is_some()
            && req.status != Some(AutomationStatus::Paused);
        if adopting {
            existing.execution_scope = self.execution_scope.clone();
        }
        let schedule_changed = adopting || req.rrule.is_some() || req.status.is_some();

        if let Some(name) = req.name {
            if name.trim().is_empty() {
                bail!("Automation name cannot be empty");
            }
            existing.name = name.trim().to_string();
        }
        if let Some(prompt) = req.prompt {
            if prompt.trim().is_empty() {
                bail!("Automation prompt cannot be empty");
            }
            existing.prompt = prompt.trim().to_string();
        }
        if let Some(rrule) = req.rrule {
            let normalized = rrule.trim().to_ascii_uppercase();
            AutomationSchedule::parse_rrule(&normalized)?;
            existing.rrule = normalized;
        }
        if let Some(cwds) = req.cwds {
            existing.cwds = cwds;
        }
        if let Some(model) = req.model {
            existing.model = normalize_optional_string(Some(model));
        }
        if let Some(provider) = req.model_provider {
            existing.model_provider = normalize_optional_string(Some(provider));
        }
        if let Some(provider_id) = req.model_provider_id {
            existing.model_provider_id = normalize_optional_string(Some(provider_id));
        }
        if let Some(mode) = req.mode {
            existing.mode = normalize_optional_string(Some(mode));
        }
        if let Some(allow_shell) = req.allow_shell {
            existing.allow_shell = Some(allow_shell);
        }
        if let Some(trust_mode) = req.trust_mode {
            existing.trust_mode = Some(trust_mode);
        }
        if let Some(auto_approve) = req.auto_approve {
            existing.auto_approve = Some(auto_approve);
        }
        if let Some(delivery_mode) = req.delivery_mode {
            existing.delivery_mode = Some(delivery_mode);
        }
        if let Some(destination) = req.destination_thread_id {
            existing.destination_thread_id = normalize_optional_string(Some(destination));
        }
        if let Some(status) = req.status {
            existing.status = status;
        }
        // Evaluate the final status once: editing a schedule and pausing it is
        // one atomic update, and must not first schedule an active run.
        if schedule_changed {
            if matches!(existing.status, AutomationStatus::Paused) {
                existing.next_run_at = None;
            } else {
                let schedule = AutomationSchedule::parse_rrule(&existing.rrule)?;
                existing.next_run_at =
                    Some(schedule.next_after_with_anchor(Utc::now(), existing.created_at)?);
            }
        }

        if existing.execution_scope.is_some()
            || existing.model_provider.is_some()
            || existing.model_provider_id.is_some()
        {
            existing.schema_version = CURRENT_AUTOMATION_SCHEMA_VERSION;
        }

        existing.updated_at = Utc::now();
        existing.revision = existing.revision.saturating_add(1);
        self.save_automation_unlocked(&existing)?;
        self.emit_automation_event("automation.updated", &existing);
        Ok(existing)
    }

    fn idempotency_path(&self) -> Result<PathBuf> {
        Ok(self
            .automations_dir
            .parent()
            .context("automation root")?
            .join("idempotency.json"))
    }

    fn read_idempotency_unlocked(&self, now: DateTime<Utc>) -> Result<Vec<IdempotencyEntry>> {
        let path = self.idempotency_path()?;
        let mut entries: Vec<IdempotencyEntry> = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(entries) => entries,
                Err(parse_error) => {
                    let quarantine = path.with_file_name(format!(
                        "idempotency.corrupt-{}.json",
                        now.format("%Y%m%dT%H%M%S%.3f")
                    ));
                    fs::rename(&path, &quarantine)
                        .with_context(|| format!("quarantine corrupt {}", path.display()))?;
                    anyhow::bail!(
                        "idempotency store {} was unparseable ({parse_error}); moved to {}",
                        path.display(),
                        quarantine.display()
                    );
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let cutoff = now - Duration::hours(IDEMPOTENCY_TTL_HOURS);
        entries.retain(|entry| entry.created_at > cutoff);
        Ok(entries)
    }

    pub fn idempotency_lookup(&self, key: &str, fingerprint: &str) -> Result<IdempotencyLookup> {
        self.with_transaction(|| {
            let entries = self.read_idempotency_unlocked(Utc::now())?;
            Ok(match entries.into_iter().find(|entry| entry.key == key) {
                None => IdempotencyLookup::Miss,
                Some(entry) if entry.fingerprint == fingerprint => IdempotencyLookup::Replay {
                    status: entry.status,
                    body: entry.body,
                },
                Some(_) => IdempotencyLookup::KeyReused,
            })
        })
    }

    pub fn idempotency_store(
        &self,
        key: &str,
        fingerprint: &str,
        status: u16,
        body: serde_json::Value,
    ) -> Result<()> {
        self.with_transaction(|| {
            let now = Utc::now();
            let mut entries = self.read_idempotency_unlocked(now)?;
            entries.retain(|entry| entry.key != key);
            entries.push(IdempotencyEntry {
                key: key.to_string(),
                fingerprint: fingerprint.to_string(),
                status,
                body,
                created_at: now,
            });
            if entries.len() > IDEMPOTENCY_MAX_ENTRIES {
                let excess = entries.len() - IDEMPOTENCY_MAX_ENTRIES;
                entries.drain(..excess);
            }
            write_json_atomic(&self.idempotency_path()?, &entries)
        })
    }

    pub fn pause_automation(&self, id: &str) -> Result<AutomationRecord> {
        self.update_automation(
            id,
            UpdateAutomationRequest {
                status: Some(AutomationStatus::Paused),
                ..UpdateAutomationRequest::default()
            },
        )
    }

    pub fn resume_automation(&self, id: &str) -> Result<AutomationRecord> {
        self.update_automation(
            id,
            UpdateAutomationRequest {
                status: Some(AutomationStatus::Active),
                ..UpdateAutomationRequest::default()
            },
        )
    }

    /// Delete an automation, keeping its terminal run history. Terminal runs
    /// (completed, failed, canceled) are archived under `<root>/archive/<id>/`
    /// — bounded to the most recent [`ARCHIVE_RETAINED_TERMINAL_RUNS`] per
    /// automation — before the live paths are removed. Deleting is refused
    /// while any run is still queued or running: an in-flight occurrence has
    /// already crossed the admission boundary, so its definition and run
    /// binding must stay intact for recovery to settle it.
    pub fn delete_automation(&self, id: &str) -> Result<AutomationRecord> {
        self.with_transaction(|| {
            let existing = self.get_automation(id)?;
            let runs = self.list_runs_with_visibility(id, None, true)?;
            if let Some(active) = runs.iter().find(|run| {
                matches!(
                    run.status,
                    AutomationRunStatus::Queued | AutomationRunStatus::Running
                )
            }) {
                bail!(
                    "Refusing to delete automation {id}: run {} is still active",
                    active.id
                );
            }

            // Archive before removing live receipts, and remove the
            // definition last. A crash or cleanup failure leaves the
            // definition available to retry; retained archive copies survive
            // even when some live receipts have already been removed.
            let terminal: Vec<AutomationRunRecord> = runs
                .into_iter()
                .filter(|run| {
                    matches!(
                        run.status,
                        AutomationRunStatus::Completed
                            | AutomationRunStatus::Failed
                            | AutomationRunStatus::Canceled
                    )
                })
                .collect();
            for run in &terminal {
                write_json_atomic(&self.archive_run_path(run)?, run)?;
            }
            self.enforce_archive_retention(id)?;

            for run in &terminal {
                self.delete_run(run)?;
            }
            let runs_dir = self.runs_dir_for(id)?;
            if runs_dir.try_exists()? && fs::read_dir(&runs_dir)?.next().is_none() {
                fs::remove_dir(&runs_dir).with_context(|| {
                    format!(
                        "Failed to remove empty run directory {}",
                        runs_dir.display()
                    )
                })?;
            }
            let path = self.automation_path(id)?;
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete automation {}", path.display()))?;
            Ok(existing)
        })
    }

    /// Terminal runs archived for a deleted automation (see
    /// [`Self::delete_automation`]), newest-first by `ended_at` (falling back
    /// to `created_at`).
    pub fn list_archived_runs(&self, automation_id: &str) -> Result<Vec<AutomationRunRecord>> {
        Ok(self
            .read_archive_entries(automation_id)?
            .into_iter()
            .map(|(_, run)| run)
            .collect())
    }

    /// Archived runs paired with their file paths, newest-first by `ended_at`
    /// (falling back to `created_at`).
    fn read_archive_entries(
        &self,
        automation_id: &str,
    ) -> Result<Vec<(PathBuf, AutomationRunRecord)>> {
        let dir = self.archive_dir_for(automation_id)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in
            fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let run = read_run_file(&path)?;
            entries.push((path, run));
        }
        entries.sort_by_key(|(_, run)| std::cmp::Reverse(run.ended_at.unwrap_or(run.created_at)));
        Ok(entries)
    }

    /// Drop archived terminal runs beyond the per-automation retention budget,
    /// keeping the most recent [`ARCHIVE_RETAINED_TERMINAL_RUNS`].
    fn enforce_archive_retention(&self, automation_id: &str) -> Result<()> {
        let entries = self.read_archive_entries(automation_id)?;
        for (path, _) in entries.into_iter().skip(ARCHIVE_RETAINED_TERMINAL_RUNS) {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete archived run {}", path.display()))?;
        }
        Ok(())
    }

    pub fn list_runs(
        &self,
        automation_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<AutomationRunRecord>> {
        self.list_runs_with_visibility(automation_id, limit, false)
    }

    fn list_runs_with_visibility(
        &self,
        automation_id: &str,
        limit: Option<usize>,
        include_suppressed: bool,
    ) -> Result<Vec<AutomationRunRecord>> {
        let dir = self.runs_dir_for(automation_id)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }

        // Split the listing into sortable-name files (newest-first by file
        // name alone, so reads stop after the newest `limit`) and legacy
        // `{uuid}.json` files, which must all be read to learn `created_at`.
        let mut sortable = Vec::new();
        let mut legacy = Vec::new();
        for entry in
            fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            if path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(has_sortable_run_stem)
            {
                sortable.push(path);
            } else {
                legacy.push(path);
            }
        }

        // A sortable receipt supersedes its legacy copy even when hidden or
        // older than the requested window. Fence identity before visibility.
        let sortable_ids: std::collections::BTreeSet<_> = sortable
            .iter()
            .filter_map(|path| path.file_stem()?.to_str()?.get(RUN_STAMP_LEN + 1..))
            .collect();
        legacy.retain(|path| {
            !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|id| sortable_ids.contains(id))
        });
        sortable.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        let visible = |run: &AutomationRunRecord| {
            include_suppressed
                || !run
                    .dispatch
                    .as_ref()
                    .is_some_and(|dispatch| dispatch.suppress_report)
        };
        let mut out = Vec::new();
        for path in sortable {
            if limit.is_some_and(|limit| out.len() >= limit) {
                break;
            }
            let run = read_run_file(&path)?;
            if visible(&run) {
                out.push(run);
            }
        }
        for path in legacy {
            let run = read_run_file(&path)?;
            if visible(&run) {
                out.push(run);
            }
        }

        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        // A crash between the sortable-name write and the legacy-file removal
        // in `save_run` can leave one run under both names; keep the sortable
        // copy (chained first above, so it survives the stable sort).
        out.dedup_by(|a, b| a.id == b.id);
        if let Some(limit) = limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    /// Re-read specific runs by id without a full history pass. Run file
    /// names end in `-{run_id}.json` (or are legacy `{run_id}.json`), so a
    /// directory listing locates them and only those files are read. Used
    /// by the activity-band scan to keep watching runs this session saw go
    /// live even after newer runs push them past the newest-run window.
    pub fn get_runs_by_ids(
        &self,
        automation_id: &str,
        run_ids: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<AutomationRunRecord>> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let dir = self.runs_dir_for(automation_id)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = BTreeMap::<String, (bool, PathBuf)>::new();
        for entry in
            fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let sortable = has_sortable_run_stem(stem);
            let id = if sortable {
                &stem[RUN_STAMP_LEN + 1..]
            } else {
                stem
            };
            if run_ids.contains(id)
                && paths
                    .get(id)
                    .is_none_or(|(current, _)| !current && sortable)
            {
                paths.insert(id.to_string(), (sortable, path));
            }
        }
        let mut out = Vec::new();
        for (_, path) in paths.into_values() {
            let run = read_run_file(&path)?;
            if !run
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| dispatch.suppress_report)
            {
                out.push(run);
            }
        }
        Ok(out)
    }

    fn save_run(&self, run: &AutomationRunRecord) -> Result<()> {
        let dir = self.runs_dir_for(&run.automation_id)?;
        fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
        let path = self.run_path(run)?;
        write_json_atomic(&path, run)?;
        // Rewrites of a legacy-named run migrate it to the sortable name; drop
        // the old file so the run never exists twice.
        let legacy = self.legacy_run_path(&run.automation_id, &run.id)?;
        if legacy != path && legacy.exists() {
            fs::remove_file(&legacy)
                .with_context(|| format!("Failed to remove legacy run {}", legacy.display()))?;
        }
        Ok(())
    }

    fn delete_run(&self, run: &AutomationRunRecord) -> Result<()> {
        let sortable = self.run_path(run)?;
        if sortable.exists() {
            fs::remove_file(&sortable)
                .with_context(|| format!("Failed to delete run {}", sortable.display()))?;
        }
        let legacy = self.legacy_run_path(&run.automation_id, &run.id)?;
        if legacy.exists() {
            fs::remove_file(&legacy)
                .with_context(|| format!("Failed to delete run {}", legacy.display()))?;
        }
        Ok(())
    }

    /// Definitions this build can read, in `list_automations` order.
    ///
    /// One corrupt, unreadable, or newer-schema file is quarantined in place:
    /// its bytes stay on disk and every pass logs the path, but it cannot
    /// starve collection of the healthy definitions behind it, and it is never
    /// rewritten or adopted by a runtime that does not understand it.
    fn readable_automations(&self) -> Result<Vec<AutomationRecord>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.automations_dir)
            .with_context(|| format!("Failed to read {}", self.automations_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match read_automation_file(&path) {
                Ok(record) => out.push(record),
                Err(error) => {
                    tracing::warn!("Skipping damaged automation file: {error:#}");
                }
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
        Ok(out)
    }

    /// List proposals only. Every proposal is revalidated and durably claimed
    /// immediately before dispatch, not when an earlier batch item is awaited.
    fn collect_due_runs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(AutomationRecord, AutomationRunRecord)>> {
        self.with_transaction(|| {
            let mut due = Vec::new();
            for mut automation in self.readable_automations()? {
                if automation.status != AutomationStatus::Active
                    || !self.eligible_scope(automation.execution_scope.as_deref())
                {
                    continue;
                }
                // An owned definition whose schedule cannot be evaluated is
                // quarantined like a damaged file: left untouched (a newer
                // build may understand it), diagnosed every pass, and never
                // allowed to take down the rest of the collection.
                let schedule = match AutomationSchedule::parse_rrule(&automation.rrule) {
                    Ok(schedule) => schedule,
                    Err(error) => {
                        tracing::warn!(
                            "Skipping automation {} with unevaluable schedule {:?}: {error:#}",
                            automation.id,
                            automation.rrule
                        );
                        continue;
                    }
                };
                let Some(due_at) = automation.next_run_at else {
                    automation.next_run_at =
                        match schedule.next_after_with_anchor(now, automation.created_at) {
                            Ok(next) => Some(next),
                            Err(error)
                                if matches!(schedule, AutomationSchedule::Once { .. })
                                    && error
                                        .to_string()
                                        .contains("Once schedule has no future run") =>
                            {
                                automation.status = AutomationStatus::Paused;
                                None
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "Skipping automation {} whose schedule cannot produce a slot: {error:#}",
                                    automation.id
                                );
                                continue;
                            }
                        };
                    automation.updated_at = now;
                    self.save_automation_unlocked(&automation)?;
                    continue;
                };
                if due_at <= now {
                    due.push((
                        automation.clone(),
                        new_run_record(&automation.id, due_at, now),
                    ));
                }
            }
            Ok(due)
        })
    }

    fn claim_scheduled_run(
        &self,
        observed: &AutomationRecord,
        mut run: AutomationRunRecord,
        task_data_dir: &Path,
    ) -> Result<Option<AutomationRunRecord>> {
        self.with_transaction(|| {
            if !self.automation_path(&observed.id)?.try_exists()? {
                return Ok(None);
            }
            let mut current = self.get_automation(&observed.id)?;
            if !self.eligible_scope(current.execution_scope.as_deref())
                || current != *observed
                || current.status != AutomationStatus::Active
                || current.next_run_at != Some(run.scheduled_for)
            {
                return Ok(None);
            }
            let schedule = AutomationSchedule::parse_rrule(&current.rrule)?;
            // Include the complete history, including legacy occurrence ids.
            let history = self.list_runs_with_visibility(&current.id, None, true)?;
            if history
                .iter()
                .any(|existing| existing.scheduled_for == run.scheduled_for)
            {
                self.advance_automation_after_slot(
                    &mut current,
                    &schedule,
                    run.scheduled_for,
                    Utc::now(),
                )?;
                return Ok(None);
            }
            // Keep the owed slot until the earlier run settles. Its eventual
            // catch-up coalesces the backlog without overlapping executions
            // or inventing cancellation receipts. Explicit run-now requests
            // remain operator intent and stay ungated.
            if history.iter().any(|existing| {
                matches!(
                    existing.status,
                    AutomationRunStatus::Queued | AutomationRunStatus::Running
                )
            }) {
                return Ok(None);
            }
            bind_run_dispatch(&mut run, &current, task_data_dir, true)?;
            self.save_run(&run)?;
            // The durable claim is the point of no return. Pausing after
            // this point affects future occurrences, not this admitted work
            // (deleting an automation with an admitted run is refused).
            // No task can start before the binding above is durable.
            self.advance_automation_after_slot(
                &mut current,
                &schedule,
                run.scheduled_for,
                Utc::now(),
            )?;
            Ok(Some(run))
        })
    }

    /// Repair only a torn claim/advance transaction. A replacement definition,
    /// pause, or Operate kick has a different generation or slot and wins.
    fn recover_schedule_advance(&self, run: &AutomationRunRecord) -> Result<()> {
        let Some(admitted) = run
            .dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.schedule.as_ref())
        else {
            return Ok(());
        };
        self.with_transaction(|| {
            if !self.automation_path(&run.automation_id)?.try_exists()? {
                return Ok(());
            }
            let mut current = self.get_automation(&run.automation_id)?;
            if current.status == AutomationStatus::Active
                && current.updated_at == admitted.updated_at
                && current.rrule == admitted.rrule
                && current.next_run_at == Some(run.scheduled_for)
            {
                let schedule = AutomationSchedule::parse_rrule(&current.rrule)?;
                self.advance_automation_after_slot(
                    &mut current,
                    &schedule,
                    run.scheduled_for,
                    Utc::now(),
                )?;
            }
            Ok(())
        })
    }

    /// Completion publishes only this occurrence's receipt. It never advances
    /// a definition that may have been edited during the enqueue await.
    fn finish_scheduled_run(&self, run: &AutomationRunRecord, now: DateTime<Utc>) -> Result<()> {
        self.with_transaction(|| {
            self.save_run(run)?;
            if matches!(
                run.status,
                AutomationRunStatus::Completed
                    | AutomationRunStatus::Failed
                    | AutomationRunStatus::Canceled
            ) && !run
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| dispatch.suppress_report)
                && self.automation_path(&run.automation_id)?.try_exists()?
            {
                let mut current = self.get_automation(&run.automation_id)?;
                let ended = run.ended_at.unwrap_or(now);
                current.last_run_at = Some(
                    current
                        .last_run_at
                        .map_or(ended, |previous| previous.max(ended)),
                );
                // Receipt metadata is not a new schedule generation. Never
                // recompute the current definition from this run's old slot.
                self.save_automation_unlocked(&current)?;
            }
            Ok(())
        })
    }

    fn advance_automation_after_slot(
        &self,
        automation: &mut AutomationRecord,
        schedule: &AutomationSchedule,
        slot: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        automation.updated_at = now;
        automation.next_run_at = schedule.next_unskipped_slot(slot, now, automation.created_at)?;
        if automation.next_run_at.is_none() {
            automation.status = AutomationStatus::Paused;
        }
        self.save_automation_unlocked(automation)
    }

    /// Active receipts are independent of the definition's lifetime and of
    /// presentation limits on recent history.
    fn collect_pending_runs(&self) -> Result<Vec<AutomationRunRecord>> {
        let mut pending = Vec::new();
        for entry in fs::read_dir(&self.runs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // A damaged receipt quarantines only its own automation: the bytes
            // stay on disk and the diagnostic is logged every pass, but one
            // corrupt file must not block recovery of every other pending run.
            let runs = match self.list_runs_with_visibility(&id, None, true) {
                Ok(runs) => runs,
                Err(error) => {
                    tracing::warn!("Skipping damaged run history for automation {id}: {error:#}");
                    continue;
                }
            };
            for run in runs {
                if matches!(
                    run.status,
                    AutomationRunStatus::Queued | AutomationRunStatus::Running
                ) && run.task_id.is_some()
                {
                    pending.push(run);
                }
            }
        }
        Ok(pending)
    }

    // ── Delayed-trigger storage methods ──────────────────────────────────

    /// Persist a new delayed trigger and return the record.
    pub fn create_trigger(&self, req: CreateDelayedTriggerRequest) -> Result<DelayedTriggerRecord> {
        let now = Utc::now();
        if req.fire_at <= now {
            bail!(
                "fire_at must be in the future (got {}, now is {})",
                req.fire_at.to_rfc3339(),
                now.to_rfc3339()
            );
        }
        if req.message.trim().is_empty() {
            bail!("Trigger message must not be empty");
        }
        let record = DelayedTriggerRecord {
            schema_version: CURRENT_TRIGGER_SCHEMA_VERSION,
            execution_scope: self.execution_scope.clone(),
            trigger_id: format!("trig_{}", Uuid::new_v4().simple()),
            fire_at: req.fire_at,
            message: req.message.trim().to_string(),
            workspace: req.workspace,
            owner_session_id: req.owner_session_id,
            destination_thread_id: normalize_optional_string(req.destination_thread_id),
            status: DelayedTriggerStatus::Pending,
            created_at: now,
            fired_at: None,
            task_id: None,
            thread_id: None,
            error: None,
            parent_trigger_id: req.parent_trigger_id,
            dispatch: None,
        };
        self.save_trigger(&record)?;
        self.emit_trigger_event("trigger.scheduled", &record);
        Ok(record)
    }

    /// Load a trigger by id.
    pub fn get_trigger(&self, trigger_id: &str) -> Result<DelayedTriggerRecord> {
        let path = self.trigger_path(trigger_id)?;
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Trigger '{trigger_id}' not found"))?;
        let record: DelayedTriggerRecord = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse trigger '{trigger_id}'"))?;
        if record.schema_version > CURRENT_TRIGGER_SCHEMA_VERSION {
            bail!(
                "Trigger schema v{} is newer than supported v{}",
                record.schema_version,
                CURRENT_TRIGGER_SCHEMA_VERSION
            );
        }
        Ok(record)
    }

    /// Load a trigger only when it belongs to the given session.
    ///
    /// Foreign, ownerless legacy, unreadable, and absent records share the same
    /// result so trigger existence cannot be disclosed across sessions.
    pub fn get_trigger_for_owner(
        &self,
        trigger_id: &str,
        owner_session_id: &str,
    ) -> Result<DelayedTriggerRecord> {
        self.get_trigger(trigger_id)
            .ok()
            .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
            .ok_or_else(|| anyhow::anyhow!("Trigger '{trigger_id}' not found"))
    }

    /// Atomically persist a trigger record.
    pub fn save_trigger(&self, record: &DelayedTriggerRecord) -> Result<()> {
        self.with_transaction(|| self.save_trigger_unlocked(record))
    }

    fn save_trigger_unlocked(&self, record: &DelayedTriggerRecord) -> Result<()> {
        let path = self.trigger_path(&record.trigger_id)?;
        write_json_atomic(&path, record)
    }

    /// List triggers, newest first.  Pass `status_filter` to restrict results.
    pub fn list_triggers(
        &self,
        status_filter: Option<DelayedTriggerStatus>,
        limit: Option<usize>,
    ) -> Result<Vec<DelayedTriggerRecord>> {
        let mut out = Vec::new();
        if !self.triggers_dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.triggers_dir)
            .with_context(|| format!("Failed to read {}", self.triggers_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            match fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<DelayedTriggerRecord>(&raw).ok())
            {
                Some(record) => {
                    if let Some(filter) = status_filter
                        && record.status != filter
                    {
                        continue;
                    }
                    out.push(record);
                }
                None => {
                    tracing::warn!("Skipping unreadable trigger file {}", path.display());
                }
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        if let Some(limit) = limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    /// List session-owned triggers, applying ownership before sorting and limit.
    pub fn list_triggers_for_owner(
        &self,
        status_filter: Option<DelayedTriggerStatus>,
        limit: Option<usize>,
        owner_session_id: &str,
    ) -> Result<Vec<DelayedTriggerRecord>> {
        let mut records = self.list_triggers(status_filter, None)?;
        records.retain(|record| record.owner_session_id.as_deref() == Some(owner_session_id));
        if let Some(limit) = limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    /// Cancel a pending trigger owned by the given session.
    pub fn cancel_trigger_for_owner(
        &self,
        trigger_id: &str,
        owner_session_id: &str,
    ) -> Result<DelayedTriggerRecord> {
        self.cancel_trigger_visible_to(trigger_id, Some(owner_session_id))
    }

    pub fn cancel_trigger(&self, trigger_id: &str) -> Result<DelayedTriggerRecord> {
        self.cancel_trigger_visible_to(trigger_id, None)
    }

    fn cancel_trigger_visible_to(
        &self,
        trigger_id: &str,
        owner_session_id: Option<&str>,
    ) -> Result<DelayedTriggerRecord> {
        let record = self.with_transaction(|| {
            let mut record = match owner_session_id {
                Some(owner) => self.get_trigger_for_owner(trigger_id, owner)?,
                None => self.get_trigger(trigger_id)?,
            };
            if record.status != DelayedTriggerStatus::Pending || record.dispatch.is_some() {
                bail!(
                    "Trigger '{trigger_id}' cannot be canceled after admission (status: {:?})",
                    record.status
                );
            }
            record.status = DelayedTriggerStatus::Canceled;
            self.save_trigger_unlocked(&record)?;
            Ok(record)
        })?;
        self.emit_trigger_event("trigger.canceled", &record);
        Ok(record)
    }

    /// Return due proposals and unfinished durable trigger admissions.
    pub fn collect_due_triggers(&self, now: DateTime<Utc>) -> Result<Vec<DelayedTriggerRecord>> {
        Ok(self
            .list_triggers(None, None)?
            .into_iter()
            .filter(|trigger| {
                trigger.status == DelayedTriggerStatus::Dispatching
                    || (trigger.status == DelayedTriggerStatus::Pending
                        && trigger.owner_session_id.is_some()
                        && trigger.fire_at <= now)
            })
            .collect())
    }
}

fn new_run_record(
    automation_id: &str,
    scheduled_for: DateTime<Utc>,
    created_at: DateTime<Utc>,
) -> AutomationRunRecord {
    AutomationRunRecord {
        schema_version: CURRENT_RUN_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        automation_id: automation_id.to_string(),
        scheduled_for,
        status: AutomationRunStatus::Queued,
        created_at,
        started_at: None,
        ended_at: None,
        task_id: None,
        thread_id: None,
        turn_id: None,
        error: None,
        dispatch: None,
    }
}

/// The posture an automation's `auto_approve` bit stands for, in the wire
/// spelling a task request carries.
///
/// A scheduled run fires with no session to inherit from, so the only authority
/// it has is its own record — and stating it explicitly keeps the scheduled
/// task from re-deriving a posture out of a legacy bit on every later change to
/// what that bit means.
fn automation_posture(auto_approve: bool) -> String {
    codewhale_runtime::runtime_policy::approval_wire(
        codewhale_execpolicy::ApprovalMode::from_auto_approve(auto_approve),
    )
    .to_string()
}

fn automation_task_request(automation: &AutomationRecord) -> NewTaskRequest {
    NewTaskRequest {
        prompt: automation.prompt.clone(),
        name: Some(automation.name.clone()),
        model: automation.model.clone(),
        model_provider: automation.model_provider.clone(),
        model_provider_id: automation.model_provider_id.clone(),
        workspace: automation.cwds.first().cloned(),
        mode: Some(automation.task_mode()),
        allow_shell: Some(automation.task_allow_shell()),
        trust_mode: Some(automation.task_trust_mode()),
        auto_approve: Some(automation.task_auto_approve()),
        permission_posture: Some(automation_posture(automation.task_auto_approve())),
        owner_session_id: None,
        destination_thread_id: automation.destination_thread_id.clone(),
    }
}

fn bind_run_dispatch(
    run: &mut AutomationRunRecord,
    automation: &AutomationRecord,
    task_data_dir: &Path,
    scheduled: bool,
) -> Result<()> {
    if automation.execution_scope.is_none() {
        bail!("Automation execution ownership is unverified");
    }
    run.schema_version = CURRENT_RUN_SCHEMA_VERSION;
    run.task_id = Some(crate::task_manager::TaskManager::new_task_id());
    run.dispatch = Some(AutomationDispatch {
        execution_scope: automation.execution_scope.clone(),
        request: automation_task_request(automation),
        task_data_dir: task_data_dir
            .canonicalize()
            .context("resolve task store before automation admission")?,
        accepted: false,
        delivery_mode: automation.delivery_mode(),
        suppress_report: false,
        schedule: scheduled.then(|| AdmittedSchedule {
            updated_at: automation.updated_at,
            rrule: automation.rrule.clone(),
        }),
    });
    Ok(())
}

fn check_dispatch_store(dispatch: &AutomationDispatch, tasks: &SharedTaskManager) -> Result<()> {
    if tasks.data_dir().canonicalize()? != dispatch.task_data_dir.canonicalize()? {
        bail!("Automation admission belongs to a different task store; it cannot be replayed here");
    }
    Ok(())
}

async fn dispatch_bound_task(
    dispatch: &mut AutomationDispatch,
    task_id: &str,
    tasks: &SharedTaskManager,
) -> Result<crate::task_manager::TaskRecord> {
    check_dispatch_store(dispatch, tasks)?;
    if dispatch.execution_scope.as_deref() != Some(tasks.execution_scope()) {
        bail!(
            "Automation admission execution ownership is unverified or belongs to another Runtime"
        );
    }
    let task = if dispatch.accepted {
        tasks
            .read_bound_task(task_id)?
            .context("Accepted automation task is missing; refusing to replay it")?
    } else {
        tasks
            .recover_task_admission(dispatch.request.clone(), task_id.to_owned())
            .await?
    };
    crate::task_manager::validate_bound_task_request(&task, &dispatch.request)?;
    dispatch.accepted = true;
    dispatch.suppress_report = dispatch.delivery_mode == AutomationDeliveryMode::Watcher
        && task.status == TaskStatus::Completed
        && task
            .result_summary
            .as_deref()
            .is_some_and(|summary| summary.trim() == AUTOMATION_WATCHER_NO_REPORT_SENTINEL);
    Ok(task)
}

/// Caller owns dispatch.lock and has already persisted this exact binding.
async fn enqueue_run_task(run: &mut AutomationRunRecord, tasks: &SharedTaskManager) {
    let result = match (&mut run.dispatch, &run.task_id) {
        (Some(dispatch), Some(task_id)) => dispatch_bound_task(dispatch, task_id, tasks).await,
        _ => Err(anyhow::anyhow!(
            "Automation run has no durable task binding"
        )),
    };
    match result {
        Ok(task) => {
            run.error = None;
            apply_task_status(run, &task);
        }
        Err(error) => {
            // Keep the same pending identity after uncertain admission. A later
            // tick first looks for its canonical task; no new id is allocated.
            run.error = Some(format!(
                "Automation task admission needs recovery: {error:#}"
            ));
        }
    }
}

fn dispatch_lock_busy(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

pub async fn run_now_shared(
    automations: &SharedAutomationManager,
    automation_id: &str,
    task_manager: &SharedTaskManager,
) -> Result<AutomationRunRecord> {
    run_now_shared_if(automations, automation_id, None, task_manager).await
}

pub async fn run_now_shared_if(
    automations: &SharedAutomationManager,
    automation_id: &str,
    expected_revision: Option<u64>,
    task_manager: &SharedTaskManager,
) -> Result<AutomationRunRecord> {
    automations.lock().await.bind_task_manager(task_manager)?;
    let task_manager = Arc::clone(task_manager);
    let task_data_dir = task_manager.data_dir();
    run_now_inner(
        automations,
        automation_id,
        expected_revision,
        &task_data_dir,
        move |_, mut run| async move {
            enqueue_run_task(&mut run, &task_manager).await;
            run
        },
    )
    .await
}

#[cfg(test)]
async fn run_now_with<F, Fut>(
    automations: &SharedAutomationManager,
    automation_id: &str,
    task_data_dir: &Path,
    enqueue: F,
) -> Result<AutomationRunRecord>
where
    F: FnOnce(AutomationRecord, AutomationRunRecord) -> Fut,
    Fut: Future<Output = AutomationRunRecord>,
{
    run_now_inner(automations, automation_id, None, task_data_dir, enqueue).await
}

/// Keep the process-wide dispatch claim over the await, never the manager mutex.
async fn run_now_inner<F, Fut>(
    automations: &SharedAutomationManager,
    automation_id: &str,
    expected_revision: Option<u64>,
    task_data_dir: &Path,
    enqueue: F,
) -> Result<AutomationRunRecord>
where
    F: FnOnce(AutomationRecord, AutomationRunRecord) -> Fut,
    Fut: Future<Output = AutomationRunRecord>,
{
    let mut lock = automations.lock().await.open_lock("dispatch.lock")?;
    let _dispatch = lock
        .try_write()
        .context("Automation dispatcher is busy; retry the run")?;
    let (automation, run) = {
        let manager = automations.lock().await;
        manager.with_transaction(|| {
            let mut automation = manager.get_automation(automation_id)?;
            check_expected_revision(&automation, expected_revision)?;
            manager.adopt_for_run(&mut automation)?;
            let now = Utc::now();
            let mut run = new_run_record(&automation.id, now, now);
            bind_run_dispatch(&mut run, &automation, task_data_dir, false)?;
            manager.save_run(&run)?;
            Ok((automation, run))
        })?
    };
    let run = enqueue(automation, run).await;
    automations
        .lock()
        .await
        .finish_scheduled_run(&run, Utc::now())?;
    Ok(run)
}

/// Run a scheduler store operation on the blocking pool (#6149), holding the
/// manager lock exactly as the inline call did: the whole-store scans (every
/// automation or run record read and parsed) and the per-record claim,
/// recovery, and receipt transactions, which wait on the cross-process
/// `state.lock` and read/write JSON. Known limit: opening `dispatch.lock`
/// (one small file open whose non-blocking guard must outlive the awaits)
/// and the in-memory scope checks stay inline.
async fn with_manager_blocking<T, F>(automations: &SharedAutomationManager, scan: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&AutomationManager) -> Result<T> + Send + 'static,
{
    let manager = Arc::clone(automations).lock_owned().await;
    tokio::task::spawn_blocking(move || scan(&manager))
        .await
        .context("automation store scan task failed")?
}

async fn scheduler_tick_shared(
    automations: &SharedAutomationManager,
    task_manager: &SharedTaskManager,
) -> Result<()> {
    automations.lock().await.bind_task_manager(task_manager)?;
    let tasks = Arc::clone(task_manager);
    scheduler_tick_with(automations, &tasks.data_dir(), move |mut run| {
        let tasks = Arc::clone(&tasks);
        async move {
            enqueue_run_task(&mut run, &tasks).await;
            run
        }
    })
    .await
}

async fn scheduler_tick_with<F, Fut>(
    automations: &SharedAutomationManager,
    task_data_dir: &Path,
    mut enqueue: F,
) -> Result<()>
where
    F: FnMut(AutomationRunRecord) -> Fut,
    Fut: Future<Output = AutomationRunRecord>,
{
    let mut lock = automations.lock().await.open_lock("dispatch.lock")?;
    let _dispatch = match lock.try_write() {
        Ok(guard) => guard,
        Err(error) if dispatch_lock_busy(&error) => return Ok(()),
        Err(error) => return Err(error).context("claim automation dispatch"),
    };
    // Repair admitted work before collecting a new occurrence, including claims
    // whose definitions were edited/deleted or whose final enqueue save tore.
    let pending =
        with_manager_blocking(automations, AutomationManager::collect_pending_runs).await?;
    for run in pending.into_iter().filter(|run| {
        run.dispatch
            .as_ref()
            .is_some_and(|dispatch| !dispatch.accepted)
    }) {
        if !automations.lock().await.eligible_scope(
            run.dispatch
                .as_ref()
                .and_then(|dispatch| dispatch.execution_scope.as_deref()),
        ) {
            continue;
        }
        // A single damaged admission is quarantined to its diagnostic; it must
        // not take down recovery of every pending run behind it.
        let recovered = with_manager_blocking(automations, {
            let run = run.clone();
            move |manager| manager.recover_schedule_advance(&run)
        })
        .await;
        if let Err(error) = recovered {
            tracing::warn!(
                "automation schedule recovery failed for run {}: {error:#}",
                run.id
            );
            continue;
        }
        let run = enqueue(run).await;
        if let Err(error) = with_manager_blocking(automations, {
            let run = run.clone();
            move |manager| manager.finish_scheduled_run(&run, Utc::now())
        })
        .await
        {
            tracing::warn!(
                "automation run {} receipt could not be persisted: {error:#}",
                run.id
            );
        }
    }
    let now = Utc::now();
    let due =
        with_manager_blocking(automations, move |manager| manager.collect_due_runs(now)).await?;
    for (observed, proposed) in due {
        if !automations
            .lock()
            .await
            .eligible_scope(observed.execution_scope.as_deref())
        {
            continue;
        }
        let claimed = with_manager_blocking(automations, {
            let observed = observed.clone();
            let task_data_dir = task_data_dir.to_path_buf();
            move |manager| manager.claim_scheduled_run(&observed, proposed, &task_data_dir)
        })
        .await;
        let run = match claimed {
            Ok(run) => run,
            Err(error) => {
                // One automation's claim failure (for example a corrupt
                // receipt in its own dedup history) quarantines that
                // automation, not the tick: later due work still dispatches.
                tracing::warn!(
                    "automation {} occurrence claim failed: {error:#}",
                    observed.id
                );
                continue;
            }
        };
        let Some(run) = run else {
            continue;
        };
        let run = enqueue(run).await;
        if let Err(error) = with_manager_blocking(automations, {
            let run = run.clone();
            move |manager| manager.finish_scheduled_run(&run, now)
        })
        .await
        {
            tracing::warn!(
                "automation run {} receipt could not be persisted: {error:#}",
                run.id
            );
        }
    }
    Ok(())
}

async fn fire_due_triggers_shared(
    automations: &SharedAutomationManager,
    task_manager: &SharedTaskManager,
) -> Result<()> {
    automations.lock().await.bind_task_manager(task_manager)?;
    let tasks = Arc::clone(task_manager);
    fire_due_triggers_with(automations, &tasks.data_dir(), move |trigger| {
        let tasks = Arc::clone(&tasks);
        async move { enqueue_trigger_task(trigger, &tasks).await }
    })
    .await
}

async fn enqueue_trigger_task(
    mut trigger: DelayedTriggerRecord,
    tasks: &SharedTaskManager,
) -> Result<DelayedTriggerRecord> {
    let result = dispatch_bound_task(
        trigger.dispatch.as_mut().context("trigger dispatch")?,
        trigger.task_id.as_deref().context("trigger task binding")?,
        tasks,
    )
    .await;
    match result {
        Ok(task) => {
            trigger.status = DelayedTriggerStatus::Fired;
            trigger.fired_at = Some(Utc::now());
            trigger.thread_id = task
                .thread_id
                .clone()
                .or_else(|| trigger.destination_thread_id.clone());
            trigger.error = None;
        }
        Err(error) => {
            trigger.error = Some(format!(
                "Delayed trigger admission needs recovery: {error:#}"
            ))
        }
    }
    Ok(trigger)
}

async fn fire_due_triggers_with<F, Fut>(
    automations: &SharedAutomationManager,
    task_data_dir: &Path,
    mut enqueue: F,
) -> Result<()>
where
    F: FnMut(DelayedTriggerRecord) -> Fut,
    Fut: Future<Output = Result<DelayedTriggerRecord>>,
{
    let mut lock = automations.lock().await.open_lock("dispatch.lock")?;
    let _dispatch = match lock.try_write() {
        Ok(guard) => guard,
        Err(error) if dispatch_lock_busy(&error) => return Ok(()),
        Err(error) => return Err(error).context("claim delayed-trigger dispatch"),
    };
    let now = Utc::now();
    let candidates = with_manager_blocking(automations, move |manager| {
        manager.collect_due_triggers(now)
    })
    .await?;
    for candidate in candidates {
        let scope = if candidate.status == DelayedTriggerStatus::Dispatching {
            candidate
                .dispatch
                .as_ref()
                .and_then(|d| d.execution_scope.as_deref())
        } else {
            candidate.execution_scope.as_deref()
        };
        if !automations.lock().await.eligible_scope(scope) {
            continue;
        }
        if !(candidate.status == DelayedTriggerStatus::Dispatching
            || (candidate.status == DelayedTriggerStatus::Pending && candidate.fire_at <= now))
        {
            continue;
        }
        let claim = with_manager_blocking(automations, {
            let trigger_id = candidate.trigger_id.clone();
            let task_data_dir = task_data_dir.to_path_buf();
            move |manager| {
                manager.with_transaction(|| {
                    let mut current = manager.get_trigger(&trigger_id)?;
                    if current.status == DelayedTriggerStatus::Dispatching {
                        if !manager.eligible_scope(
                            current
                                .dispatch
                                .as_ref()
                                .and_then(|d| d.execution_scope.as_deref()),
                        ) {
                            return Ok(None);
                        }
                        if current.dispatch.is_none() || current.task_id.is_none() {
                            bail!("Claimed delayed trigger has no durable task binding");
                        }
                        return Ok(Some(current));
                    }
                    if !manager.eligible_scope(current.execution_scope.as_deref())
                        || current.status != DelayedTriggerStatus::Pending
                        || current.fire_at > now
                        || current.owner_session_id.is_none()
                    {
                        return Ok(None);
                    }
                    current.schema_version = CURRENT_TRIGGER_SCHEMA_VERSION;
                    current.status = DelayedTriggerStatus::Dispatching;
                    current.task_id = Some(crate::task_manager::TaskManager::new_task_id());
                    current.dispatch = Some(AutomationDispatch {
                        execution_scope: current.execution_scope.clone(),
                        request: NewTaskRequest {
                            prompt: current.message.clone(),
                            name: None,
                            model: None,
                            model_provider: None,
                            model_provider_id: None,
                            workspace: current.workspace.clone(),
                            mode: Some("agent".into()),
                            allow_shell: Some(false),
                            trust_mode: Some(false),
                            auto_approve: Some(false),
                            permission_posture: Some(automation_posture(false)),
                            owner_session_id: current.owner_session_id.clone(),
                            destination_thread_id: current.destination_thread_id.clone(),
                        },
                        task_data_dir: task_data_dir.canonicalize()?,
                        accepted: false,
                        delivery_mode: AutomationDeliveryMode::Task,
                        suppress_report: false,
                        schedule: None,
                    });
                    manager.save_trigger_unlocked(&current)?;
                    Ok(Some(current))
                })
            }
        })
        .await;
        let claimed = match claim {
            Ok(claimed) => claimed,
            Err(error) => {
                // One damaged trigger record quarantines to a diagnostic;
                // the remaining due triggers still fire this pass.
                tracing::warn!(
                    "delayed trigger {} claim failed: {error:#}",
                    candidate.trigger_id
                );
                continue;
            }
        };
        let Some(trigger) = claimed else {
            continue;
        };
        let trigger = match enqueue(trigger).await {
            Ok(trigger) => trigger,
            Err(error) => {
                tracing::warn!(
                    "delayed trigger {} enqueue failed: {error:#}",
                    candidate.trigger_id
                );
                continue;
            }
        };
        if let Err(error) = with_manager_blocking(automations, {
            let trigger = trigger.clone();
            move |manager| {
                manager.save_trigger(&trigger)?;
                if trigger.status == DelayedTriggerStatus::Fired {
                    manager.emit_trigger_event("trigger.fired", &trigger);
                }
                Ok(())
            }
        })
        .await
        {
            tracing::warn!(
                "delayed trigger {} receipt could not be persisted: {error:#}",
                trigger.trigger_id
            );
        }
    }
    Ok(())
}

/// Fold a durable task's state back into its automation run. Returns whether
/// the run changed and needs persisting.
fn apply_task_status(
    run: &mut AutomationRunRecord,
    task: &crate::task_manager::TaskRecord,
) -> bool {
    let mut changed = run.thread_id != task.thread_id || run.turn_id != task.turn_id;
    run.thread_id = task.thread_id.clone();
    run.turn_id = task.turn_id.clone();
    match task.status {
        TaskStatus::Queued => {
            if !matches!(run.status, AutomationRunStatus::Queued) {
                run.status = AutomationRunStatus::Queued;
                changed = true;
            }
        }
        TaskStatus::Running => {
            if !matches!(run.status, AutomationRunStatus::Running) {
                run.status = AutomationRunStatus::Running;
                changed = true;
            }
            if run.started_at.is_none() {
                run.started_at = Some(task.started_at.unwrap_or_else(Utc::now));
                changed = true;
            }
        }
        TaskStatus::Completed => {
            run.status = AutomationRunStatus::Completed;
            run.started_at = run.started_at.or(task.started_at);
            run.ended_at = task.ended_at.or(Some(Utc::now()));
            run.error = None;
            changed = true;
        }
        TaskStatus::Failed => {
            run.status = AutomationRunStatus::Failed;
            run.started_at = run.started_at.or(task.started_at);
            run.ended_at = task.ended_at.or(Some(Utc::now()));
            run.error = task.error.clone();
            changed = true;
        }
        TaskStatus::Canceled => {
            run.status = AutomationRunStatus::Canceled;
            run.started_at = run.started_at.or(task.started_at);
            run.ended_at = task.ended_at.or(Some(Utc::now()));
            // #6162: a cancellation is not silent. Keep the task's own error
            // when it recorded one, otherwise name the terminal reason so the
            // settled receipt can say who or what canceled the run.
            run.error = task.error.clone().or_else(|| {
                task.terminal_reason
                    .as_deref()
                    .map(cancellation_reason_text)
            });
            changed = true;
        }
    }
    changed
}

/// Human-readable cancellation detail for a run whose task ended without an
/// error of its own. The task manager's terminal reasons are stable strings
/// (`TaskTerminalReason::as_str`); anything unknown is passed through.
fn cancellation_reason_text(terminal_reason: &str) -> String {
    // A cooperative cancel is the one path that arrives without an error of
    // its own; cancel-timeout and shutdown already carry the task manager's
    // receipt message, so those arms are a fallback for records that lost it.
    match terminal_reason {
        "canceled" => "canceled by request".to_string(),
        "cancel_timeout" => "canceled; the task did not stop within the cancel timeout".to_string(),
        "shutdown" => "canceled by shutdown".to_string(),
        other => format!("canceled ({other})"),
    }
}

async fn reconcile_run_statuses_shared(
    automations: &SharedAutomationManager,
    task_manager: &SharedTaskManager,
) -> Result<()> {
    automations.lock().await.bind_task_manager(task_manager)?;
    let mut lock = automations.lock().await.open_lock("dispatch.lock")?;
    let _dispatch = match lock.try_write() {
        Ok(guard) => guard,
        Err(error) if dispatch_lock_busy(&error) => return Ok(()),
        Err(error) => return Err(error).context("claim automation reconciliation"),
    };
    let pending =
        with_manager_blocking(automations, AutomationManager::collect_pending_runs).await?;
    for mut run in pending {
        // Shared storage is not shared ownership. A receipt admitted under
        // another execution scope is reconciled by that scope's owner; this
        // process must not stamp errors onto it or rewrite it from a bound
        // task record it cannot see.
        if run
            .dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.execution_scope.as_deref())
            != Some(task_manager.execution_scope())
        {
            continue;
        }
        let Some(task_id) = run.task_id.clone() else {
            continue;
        };
        // The bound task record is a JSON read under the task store: keep it
        // off the Tokio worker like the scans above (#6149).
        let lookup = tokio::task::spawn_blocking({
            let dispatch = run.dispatch.clone();
            let task_id = task_id.clone();
            let task_manager = Arc::clone(task_manager);
            move || {
                if let Some(dispatch) = &dispatch {
                    check_dispatch_store(dispatch, &task_manager)?;
                }
                let task = task_manager.read_bound_task(&task_id)?;
                if let Some(task) = &task
                    && let Some(dispatch) = &dispatch
                {
                    crate::task_manager::validate_bound_task_request(task, &dispatch.request)?;
                }
                Ok::<_, anyhow::Error>(task)
            }
        })
        .await
        .context("automation task lookup failed")
        .and_then(|lookup| lookup);
        let task = match lookup {
            Ok(Some(task)) => task,
            Ok(None) => {
                if run
                    .dispatch
                    .as_ref()
                    .is_some_and(|dispatch| dispatch.accepted)
                {
                    // The admission was durably accepted but the bound task
                    // record is gone: this occurrence can never be replayed
                    // or reconciled. Settle it terminally instead of retrying
                    // the same lookup every pass and starving the schedule
                    // behind it.
                    run.status = AutomationRunStatus::Failed;
                    run.ended_at = Some(run.ended_at.unwrap_or_else(Utc::now));
                    run.error = Some(format!(
                        "Bound automation task {task_id} is missing after durable acceptance"
                    ));
                } else {
                    // Unaccepted admissions are the scheduler's recovery path:
                    // the next tick reuses the durable binding or recreates
                    // the task; reconcile only records the uncertainty.
                    run.error = Some(format!(
                        "Automation reconciliation unavailable: bound task {task_id} is missing"
                    ));
                }
                if let Err(error) = with_manager_blocking(automations, {
                    let run = run.clone();
                    move |manager| manager.finish_scheduled_run(&run, Utc::now())
                })
                .await
                {
                    tracing::warn!(
                        "automation run {} receipt could not be persisted: {error:#}",
                        run.id
                    );
                }
                continue;
            }
            Err(error) => {
                run.error = Some(format!("Automation reconciliation unavailable: {error:#}"));
                if let Err(error) = with_manager_blocking(automations, {
                    let run = run.clone();
                    move |manager| manager.finish_scheduled_run(&run, Utc::now())
                })
                .await
                {
                    tracing::warn!(
                        "automation run {} receipt could not be persisted: {error:#}",
                        run.id
                    );
                }
                continue;
            }
        };
        // The bound task itself belongs to another Runtime — hands off.
        if task.execution_scope.as_deref() != Some(task_manager.execution_scope()) {
            continue;
        }
        let watcher = run
            .dispatch
            .as_ref()
            .map(|dispatch| dispatch.delivery_mode)
            .or_else(|| {
                automations.try_lock().ok().and_then(|manager| {
                    manager
                        .get_automation(&run.automation_id)
                        .ok()
                        .map(|automation| automation.delivery_mode())
                })
            })
            == Some(AutomationDeliveryMode::Watcher);
        let watcher_noop = watcher
            && task.status == TaskStatus::Completed
            && task
                .result_summary
                .as_deref()
                .is_some_and(|summary| summary.trim() == AUTOMATION_WATCHER_NO_REPORT_SENTINEL);
        if !apply_task_status(&mut run, &task) {
            continue;
        }
        let dispatch = run.dispatch.get_or_insert_with(|| AutomationDispatch {
            execution_scope: task.execution_scope.clone(),
            request: NewTaskRequest::from_task(&task),
            task_data_dir: task_manager.data_dir(),
            accepted: true,
            delivery_mode: if watcher {
                AutomationDeliveryMode::Watcher
            } else {
                AutomationDeliveryMode::Task
            },
            suppress_report: false,
            schedule: None,
        });
        run.schema_version = CURRENT_RUN_SCHEMA_VERSION;
        dispatch.accepted = true;
        dispatch.suppress_report = watcher_noop;
        if let Err(error) = with_manager_blocking(automations, {
            let run = run.clone();
            move |manager| manager.finish_scheduled_run(&run, Utc::now())
        })
        .await
        {
            tracing::warn!(
                "automation run {} receipt could not be persisted: {error:#}",
                run.id
            );
        }
    }
    Ok(())
}

/// Fixed-width, lexically-sortable UTC stamp for run file names, e.g.
/// `20260705T142530123Z` (millisecond precision; the run id suffix breaks
/// same-millisecond ties deterministically).
const RUN_STAMP_FORMAT: &str = "%Y%m%dT%H%M%S%3fZ";
const RUN_STAMP_LEN: usize = "20260705T142530123Z".len();

fn run_file_stamp(created_at: DateTime<Utc>) -> String {
    created_at.format(RUN_STAMP_FORMAT).to_string()
}

/// Shape check for `{stamp}-{run_id}` file stems. Ordering trusts the file
/// name only for pruning; the parsed record's `created_at` stays
/// authoritative for the final sort.
fn has_sortable_run_stem(stem: &str) -> bool {
    let Some((stamp, rest)) = stem.split_at_checked(RUN_STAMP_LEN) else {
        return false;
    };
    if !rest.starts_with('-') || rest.len() < 2 {
        return false;
    }
    stamp.char_indices().all(|(idx, ch)| match idx {
        8 => ch == 'T',
        18 => ch == 'Z',
        _ => ch.is_ascii_digit(),
    })
}

fn read_automation_file(path: &Path) -> Result<AutomationRecord> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read automation {}", path.display()))?;
    let record: AutomationRecord = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse automation {}", path.display()))?;
    if record.schema_version > CURRENT_AUTOMATION_SCHEMA_VERSION {
        bail!(
            "Automation schema v{} is newer than supported v{}",
            record.schema_version,
            CURRENT_AUTOMATION_SCHEMA_VERSION
        );
    }
    Ok(record)
}

fn read_run_file(path: &Path) -> Result<AutomationRunRecord> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let run: AutomationRunRecord = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    if run.schema_version > CURRENT_RUN_SCHEMA_VERSION {
        bail!(
            "Automation run schema v{} is newer than supported v{}",
            run.schema_version,
            CURRENT_RUN_SCHEMA_VERSION
        );
    }
    Ok(run)
}

fn ensure_safe_storage_id(kind: &str, value: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    let Some(component) = components.next() else {
        bail!("{kind} must not be empty");
    };
    if components.next().is_some() || !matches!(component, std::path::Component::Normal(_)) {
        bail!("{kind} must be a single path component");
    }
    Ok(())
}

fn validate_name_and_prompt(name: &str, prompt: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("Automation name is required");
    }
    if prompt.trim().is_empty() {
        bail!("Automation prompt is required");
    }
    Ok(())
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    codewhale_util::utils::write_atomic(path, &serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("write {}", path.display()))
}

pub fn default_automations_dir() -> PathBuf {
    // Most-specific override: an explicit automations dir.
    for var in ["CODEWHALE_AUTOMATIONS_DIR", "DEEPSEEK_AUTOMATIONS_DIR"] {
        if let Ok(path) = std::env::var(var) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
    }
    // $CODEWHALE_HOME is a hard override of the base data directory
    // (docs/CONFIGURATION.md): when SET, automations live under it and we do
    // NOT fall back to the legacy ~/.deepseek path — silent fallback would
    // defeat the isolation the override promises. Check the env var directly
    // (not codewhale_home()'s Ok/Err, which succeeds for the default home too).
    if let Some(home) = codewhale_paths::codewhale_home_override().ok().flatten() {
        return home.join("automations");
    }
    codewhale_paths::user_home()
        .map(|home| {
            let primary = home.join(".codewhale").join("automations");
            let legacy = home.join(".deepseek").join("automations");
            if primary.exists() || !legacy.exists() {
                return primary;
            }
            legacy
        })
        .unwrap_or_else(|| PathBuf::from(".codewhale").join("automations"))
}

pub type SharedAutomationManager = Arc<Mutex<AutomationManager>>;

#[derive(Debug, Clone)]
pub struct AutomationSchedulerConfig {
    pub tick_interval_secs: u64,
}

impl Default for AutomationSchedulerConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 15,
        }
    }
}

pub fn spawn_scheduler(
    automations: SharedAutomationManager,
    task_manager: SharedTaskManager,
    cancel: CancellationToken,
    config: AutomationSchedulerConfig,
) -> tokio::task::JoinHandle<()> {
    spawn_supervised(
        "automation-scheduler",
        std::panic::Location::caller(),
        async move {
            let interval = config.tick_interval_secs.max(5);
            loop {
                if cancel.is_cancelled() {
                    break;
                }

                // Lock scope lives inside the shared helpers: the manager
                // mutex is dropped across every task-manager await so API and
                // tool callers are never queued behind enqueue/status latency.
                if let Err(err) = scheduler_tick_shared(&automations, &task_manager).await {
                    tracing::warn!("automation scheduler tick failed: {err}");
                }
                if let Err(err) = reconcile_run_statuses_shared(&automations, &task_manager).await {
                    tracing::warn!("automation reconcile failed: {err}");
                }
                if let Err(err) = fire_due_triggers_shared(&automations, &task_manager).await {
                    tracing::warn!("delayed trigger tick failed: {err}");
                }

                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(std::time::Duration::from_secs(interval)) => {}
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{FixedOffset, LocalResult, NaiveDate};
    use tokio::sync::mpsc;

    use crate::task_manager::{
        ExecutionTask, TaskExecutionEvent, TaskExecutionResult, TaskExecutor, TaskManager,
        TaskManagerConfig,
    };

    struct AutomationRecordingExecutor(PathBuf);

    #[async_trait]
    impl TaskExecutor for AutomationRecordingExecutor {
        async fn execute(
            &self,
            task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            use std::io::Write as _;
            let recorded = (|| -> Result<()> {
                let id = task
                    .thread_request()
                    .task_id
                    .context("fixture task identity")?;
                let mut file = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.0)?;
                writeln!(file, "{id}")?;
                file.sync_all()?;
                Ok(())
            })();
            match recorded {
                Ok(()) => TaskExecutionResult {
                    status: TaskStatus::Completed,
                    result_text: Some("automation fixture completed".into()),
                    error: None,
                    terminal_reason: crate::task_manager::TaskTerminalReason::Completed,
                },
                Err(error) => TaskExecutionResult {
                    status: TaskStatus::Failed,
                    result_text: None,
                    error: Some(error.to_string()),
                    terminal_reason: crate::task_manager::TaskTerminalReason::Failed,
                },
            }
        }
    }

    fn fixture_executions(path: &Path) -> Vec<String> {
        match fs::read_to_string(path) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read independent execution receipts: {error}"),
        }
    }

    async fn fixture_tasks(root: &Path, receipts: &Path) -> Result<SharedTaskManager> {
        TaskManager::start_with_executor(
            automation_task_config(root.to_path_buf()),
            Arc::new(AutomationRecordingExecutor(receipts.to_path_buf())),
        )
        .await
    }

    fn fixture_due_automation(
        manager: &AutomationManager,
        id: &str,
        order: i64,
    ) -> AutomationRecord {
        let mut automation = automation_record_with_settings(None, None, None, None);
        automation.id = id.to_owned();
        automation.prompt = format!("automation fixture {id}");
        automation.created_at = Utc::now() - Duration::hours(2);
        automation.updated_at = Utc::now() - Duration::minutes(order);
        automation.next_run_at = Some(Utc::now() - Duration::minutes(1));
        manager
            .save_automation(&automation)
            .expect("save due fixture");
        automation
    }

    #[tokio::test]
    async fn interrupted_dispatch_reuses_binding_and_preserves_replacement_schedule() -> Result<()>
    {
        for accepted_before_cut in [false, true] {
            let root = tempfile::tempdir()?;
            let receipts = root.path().join("executions");
            let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
            let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
            let automation = fixture_due_automation(&manager, "recover", 1);
            let shared = Arc::new(Mutex::new(manager));
            let (at_cut, reached_cut) = tokio::sync::oneshot::channel();
            let tick = tokio::spawn({
                let shared = shared.clone();
                let tasks = tasks.clone();
                let mut at_cut = Some(at_cut);
                async move {
                    scheduler_tick_with(&shared, &tasks.data_dir(), move |mut run| {
                        let tasks = tasks.clone();
                        let at_cut = at_cut.take();
                        async move {
                            if accepted_before_cut {
                                enqueue_run_task(&mut run, &tasks).await;
                            }
                            if let Some(at_cut) = at_cut {
                                let _ = at_cut.send(run.clone());
                            }
                            std::future::pending::<()>().await;
                            run
                        }
                    })
                    .await
                }
            });
            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(5), reached_cut).await??;
            let bound_id = observed.task_id.clone().context("claimed task id")?;
            let persisted = shared.lock().await.list_runs(&automation.id, None)?;
            assert_eq!(
                persisted.len(),
                1,
                "intent exists before either interruption boundary"
            );
            assert_eq!(persisted[0].task_id.as_deref(), Some(bound_id.as_str()));
            assert!(
                !persisted[0].dispatch.as_ref().unwrap().accepted,
                "final acceptance save has not run"
            );
            if accepted_before_cut {
                let task = crate::task_manager::wait_for_terminal_state(
                    &tasks,
                    &bound_id,
                    std::time::Duration::from_secs(5),
                )
                .await?;
                assert_eq!(task.status, TaskStatus::Completed);
                assert!(
                    task.result_summary
                        .as_deref()
                        .unwrap_or_default()
                        .contains("automation fixture completed")
                );
                assert_eq!(fixture_executions(&receipts), vec![bound_id.clone()]);
            } else {
                assert!(tasks.read_bound_task(&bound_id)?.is_none());
                assert!(fixture_executions(&receipts).is_empty());
            }
            // Simulate a dropped request/process before the final run receipt.
            tick.abort();
            assert!(tick.await.unwrap_err().is_cancelled());
            let reopened = AutomationManager::open_for_test(root.path().join("automations"))?;
            let future = Utc::now() + Duration::hours(4);
            let edited = reopened.update_automation(
                &automation.id,
                UpdateAutomationRequest {
                    rrule: Some(format!("FREQ=ONCE;AT={}", future.to_rfc3339())),
                    prompt: Some("future revised prompt".into()),
                    ..Default::default()
                },
            )?;
            let restarted = Arc::new(Mutex::new(reopened));
            scheduler_tick_shared(&restarted, &tasks).await?;
            let task = crate::task_manager::wait_for_terminal_state(
                &tasks,
                &bound_id,
                std::time::Duration::from_secs(5),
            )
            .await?;
            assert_eq!(task.status, TaskStatus::Completed);
            assert_eq!(
                task.prompt, automation.prompt,
                "claim keeps its original request"
            );
            reconcile_run_statuses_shared(&restarted, &tasks).await?;
            scheduler_tick_shared(&restarted, &tasks).await?;
            assert_eq!(
                fixture_executions(&receipts),
                vec![bound_id.clone()],
                "same occurrence executes exactly once"
            );
            let manager = restarted.lock().await;
            let runs = manager.list_runs(&automation.id, None)?;
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].task_id.as_deref(), Some(bound_id.as_str()));
            assert_eq!(runs[0].status, AutomationRunStatus::Completed);
            assert!(runs[0].dispatch.as_ref().unwrap().accepted);
            let current = manager.get_automation(&automation.id)?;
            assert_eq!(current.rrule, edited.rrule);
            assert_eq!(
                current.next_run_at, edited.next_run_at,
                "old completion must not clear future ONCE"
            );
            assert_eq!(current.status, AutomationStatus::Active);
            assert_eq!(current.updated_at, edited.updated_at);
            assert_eq!(current.prompt, edited.prompt);
            assert!(current.last_run_at.is_some());
            tasks.shutdown();
        }
        Ok(())
    }

    #[tokio::test]
    async fn pause_and_delete_before_claim_stop_collected_work_but_preserve_admitted_work()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let receipts = root.path().join("executions");
        let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let first = fixture_due_automation(&manager, "first", 1);
        let paused = fixture_due_automation(&manager, "paused", 2);
        let deleted = fixture_due_automation(&manager, "deleted", 3);
        let shared = Arc::new(Mutex::new(manager));
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let tick = tokio::spawn({
            let shared = shared.clone();
            let tasks = tasks.clone();
            let mut barrier = Some((entered, released));
            async move {
                scheduler_tick_with(&shared, &tasks.data_dir(), move |mut run| {
                    let barrier = barrier.take();
                    let tasks = tasks.clone();
                    async move {
                        if let Some((entered, released)) = barrier {
                            let _ = entered.send(run.clone());
                            let _ = released.await;
                        }
                        enqueue_run_task(&mut run, &tasks).await;
                        run
                    }
                })
                .await
            }
        });
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(5), reached).await??;
        assert_eq!(
            admitted.automation_id, first.id,
            "ordered first claim really entered"
        );
        let other = AutomationManager::open_for_test(root.path().join("automations"))?;
        other.pause_automation(&paused.id)?;
        other.delete_automation(&deleted.id)?;
        // Pause after durable admission affects future runs; this run remains owned.
        other.pause_automation(&first.id)?;
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("release fixture"))?;
        tokio::time::timeout(std::time::Duration::from_secs(5), tick).await???;
        let id = admitted.task_id.context("admitted task identity")?;
        let task = crate::task_manager::wait_for_terminal_state(
            &tasks,
            &id,
            std::time::Duration::from_secs(5),
        )
        .await?;
        assert_eq!(task.status, TaskStatus::Completed);
        reconcile_run_statuses_shared(&shared, &tasks).await?;
        assert_eq!(fixture_executions(&receipts), vec![id]);
        assert!(other.list_runs(&paused.id, None)?.is_empty());
        assert!(other.list_runs(&deleted.id, None)?.is_empty());
        assert_eq!(
            other.get_automation(&first.id)?.status,
            AutomationStatus::Paused
        );
        assert!(other.get_automation(&first.id)?.next_run_at.is_none());
        tasks.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn reconciliation_reaches_old_runs_and_delete_waits_for_settled_receipts() -> Result<()> {
        for delete_definition in [false, true] {
            let root = tempfile::tempdir()?;
            let receipts = root.path().join("executions");
            let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
            let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
            let automation = fixture_due_automation(&manager, "history", 1);
            let shared = Arc::new(Mutex::new(manager));
            let run = run_now_shared(&shared, &automation.id, &tasks).await?;
            let id = run.task_id.clone().context("real task id")?;
            let task = crate::task_manager::wait_for_terminal_state(
                &tasks,
                &id,
                std::time::Duration::from_secs(5),
            )
            .await?;
            assert_eq!(task.status, TaskStatus::Completed);
            {
                let manager = shared.lock().await;
                for offset in 1..=110 {
                    let mut newer = new_run_record(
                        &automation.id,
                        Utc::now(),
                        run.created_at + Duration::seconds(offset),
                    );
                    newer.status = AutomationRunStatus::Completed;
                    newer.ended_at = Some(newer.created_at);
                    manager.save_run(&newer)?;
                }
                assert!(
                    manager
                        .list_runs(&automation.id, Some(100))?
                        .iter()
                        .all(|candidate| candidate.id != run.id)
                );
                if delete_definition {
                    // The receipt is still queued on disk even though its task
                    // already settled, so deletion is refused: an in-flight
                    // occurrence must not be stranded by a deleted definition.
                    let refused = manager.delete_automation(&automation.id);
                    assert!(refused.is_err(), "unsettled receipts must block deletion");
                }
            }
            reconcile_run_statuses_shared(&shared, &tasks).await?;
            let manager = shared.lock().await;
            let found =
                manager.get_runs_by_ids(&automation.id, &[run.id.clone()].into_iter().collect())?;
            assert_eq!(
                found.len(),
                1,
                "run binding survives until its receipt settles"
            );
            assert_eq!(found[0].status, AutomationRunStatus::Completed);
            assert_eq!(found[0].task_id.as_deref(), Some(id.as_str()));
            assert_eq!(fixture_executions(&receipts), vec![id]);
            if delete_definition {
                // The earlier delete was refused, so the definition survived
                // and reconciliation settled its receipt in place. Deleting
                // now archives the settled history and removes the live tree.
                assert!(manager.get_automation(&automation.id).is_ok());
                manager.delete_automation(&automation.id)?;
                assert!(manager.get_automation(&automation.id).is_err());
                let ids = std::collections::BTreeSet::from([run.id.clone()]);
                assert!(manager.get_runs_by_ids(&automation.id, &ids)?.is_empty());
                let archived = manager.list_archived_runs(&automation.id)?;
                assert_eq!(archived.len(), ARCHIVE_RETAINED_TERMINAL_RUNS);
                assert!(archived.iter().all(|candidate| candidate.id != run.id));
            } else {
                assert!(
                    manager
                        .get_automation(&automation.id)?
                        .last_run_at
                        .is_some()
                );
            }
            tasks.shutdown();
        }
        Ok(())
    }

    #[tokio::test]
    async fn foreign_store_and_missing_accepted_task_preserve_binding_without_fallback()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let receipts = root.path().join("executions");
        let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
        let other_receipts = root.path().join("foreign-executions");
        let other_tasks =
            fixture_tasks(&root.path().join("foreign-tasks"), &other_receipts).await?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let automation = fixture_due_automation(&manager, "bound-store", 1);
        let proposed = manager.collect_due_runs(Utc::now())?.remove(0);
        let mut run = manager
            .claim_scheduled_run(&proposed.0, proposed.1, &tasks.data_dir())?
            .context("claim")?;
        let id = run.task_id.clone().context("bound task")?;
        let shared = Arc::new(Mutex::new(manager));
        scheduler_tick_shared(&shared, &other_tasks).await?;
        let rows = shared.lock().await.list_runs(&automation.id, None)?;
        assert_eq!(rows[0].task_id.as_deref(), Some(id.as_str()));
        assert!(
            rows[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("different task store")
        );
        assert!(fixture_executions(&other_receipts).is_empty());
        enqueue_run_task(&mut run, &tasks).await;
        let task = crate::task_manager::wait_for_terminal_state(
            &tasks,
            &id,
            std::time::Duration::from_secs(5),
        )
        .await?;
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(run.dispatch.as_ref().unwrap().accepted);
        shared.lock().await.finish_scheduled_run(&run, Utc::now())?;
        fs::remove_file(tasks.data_dir().join("tasks").join(format!("{id}.json")))?;
        scheduler_tick_shared(&shared, &tasks).await?;
        reconcile_run_statuses_shared(&shared, &tasks).await?;
        let rows = shared.lock().await.list_runs(&automation.id, None)?;
        assert_eq!(rows[0].task_id.as_deref(), Some(id.as_str()));
        assert!(rows[0].dispatch.as_ref().unwrap().accepted);
        assert!(
            rows[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("missing")
        );
        assert_eq!(fixture_executions(&receipts), vec![id]);
        assert!(fixture_executions(&other_receipts).is_empty());
        tasks.shutdown();
        other_tasks.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn canceled_collected_trigger_is_not_admitted_after_an_earlier_enqueue_wait() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let receipts = root.path().join("executions");
        let tasks = fixture_tasks(&root.path().join("tasks"), &receipts).await?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let mut triggers = Vec::new();
        for index in 0..2 {
            let mut trigger = manager.create_trigger(CreateDelayedTriggerRequest {
                destination_thread_id: None,
                fire_at: Utc::now() + Duration::hours(1),
                message: format!("trigger fixture {index}"),
                workspace: None,
                owner_session_id: Some("fixture-owner".into()),
                parent_trigger_id: None,
            })?;
            trigger.fire_at = Utc::now() - Duration::minutes(1);
            trigger.created_at = Utc::now() - Duration::minutes(index);
            manager.save_trigger(&trigger)?;
            triggers.push(trigger);
        }
        let shared = Arc::new(Mutex::new(manager));
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let tick = tokio::spawn({
            let shared = shared.clone();
            let tasks = tasks.clone();
            let mut barrier = Some((entered, released));
            async move {
                fire_due_triggers_with(&shared, &tasks.data_dir(), move |trigger| {
                    let tasks = tasks.clone();
                    let barrier = barrier.take();
                    async move {
                        if let Some((entered, released)) = barrier {
                            let _ = entered.send(trigger.clone());
                            let _ = released.await;
                        }
                        enqueue_trigger_task(trigger, &tasks).await
                    }
                })
                .await
            }
        });
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(5), reached).await??;
        assert_eq!(admitted.trigger_id, triggers[0].trigger_id);
        assert_eq!(admitted.status, DelayedTriggerStatus::Dispatching);
        let other = AutomationManager::open_for_test(root.path().join("automations"))?;
        other.cancel_trigger_for_owner(&triggers[1].trigger_id, "fixture-owner")?;
        assert!(
            other
                .cancel_trigger_for_owner(&admitted.trigger_id, "fixture-owner")
                .is_err(),
            "claimed work has crossed the admission boundary"
        );
        release
            .send(())
            .map_err(|_| anyhow::anyhow!("release trigger fixture"))?;
        tokio::time::timeout(std::time::Duration::from_secs(5), tick).await???;
        let id = admitted.task_id.context("trigger task identity")?;
        let task = crate::task_manager::wait_for_terminal_state(
            &tasks,
            &id,
            std::time::Duration::from_secs(5),
        )
        .await?;
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.owner_session_id.as_deref(), Some("fixture-owner"));
        assert!(!task.allow_shell && !task.trust_mode && !task.auto_approve);
        assert_eq!(fixture_executions(&receipts), vec![id]);
        assert_eq!(
            other.get_trigger(&triggers[0].trigger_id)?.status,
            DelayedTriggerStatus::Fired
        );
        let canceled = other.get_trigger(&triggers[1].trigger_id)?;
        assert_eq!(canceled.status, DelayedTriggerStatus::Canceled);
        assert!(canceled.task_id.is_none());
        tasks.shutdown();
        Ok(())
    }

    async fn wait_fixture_path(path: &Path) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !path.try_exists()? {
            if std::time::Instant::now() >= deadline {
                bail!("fixture barrier timed out: {}", path.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }

    struct SchedulerFixtureChild(std::process::Child);

    impl Drop for SchedulerFixtureChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_scheduler_fixture(root: &Path, role: &str) -> Result<SchedulerFixtureChild> {
        let log = fs::File::create(root.join(format!("{role}.log")))?;
        let home = root.join(format!("{role}-home"));
        fs::create_dir_all(&home)?;
        Ok(SchedulerFixtureChild(
            std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--ignored",
                    "--exact",
                    "automation_manager::tests::scheduler_process_fixture",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("CW_AUTOMATION_PROCESS_FIXTURE", root)
                .env("CW_AUTOMATION_PROCESS_ROLE", role)
                .env("CODEWHALE_HOME", &home)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(log.try_clone()?))
                .stderr(std::process::Stdio::from(log))
                .spawn()?,
        ))
    }

    async fn finish_scheduler_fixture(child: &mut SchedulerFixtureChild, log: &Path) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let Some(status) = child.0.try_wait()? {
                if !status.success() {
                    bail!("fixture process failed: {}", fs::read_to_string(log)?);
                }
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                bail!("fixture process timed out: {}", log.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Only the parent test launches this entry point with explicit temporary
    /// stores. Missing fixture parameters fail; this helper cannot pass by skip.
    #[tokio::test]
    #[ignore = "subprocess entry point for independent scheduler contention fixture"]
    async fn scheduler_process_fixture() -> Result<()> {
        let root = PathBuf::from(
            std::env::var_os("CW_AUTOMATION_PROCESS_FIXTURE").context("required fixture root")?,
        );
        let role = std::env::var("CW_AUTOMATION_PROCESS_ROLE")?;
        if !matches!(role.as_str(), "first" | "second") {
            bail!("invalid fixture role");
        }
        let scope = if role == "first" {
            "test"
        } else {
            "foreign-scheduler"
        };
        let tasks = TaskManager::start_with_executor_in_scope(
            automation_task_config(root.join("tasks")),
            Arc::new(AutomationRecordingExecutor(root.join("executions"))),
            scope,
        )
        .await?;
        let mut service = AutomationManager::open(root.join("automations"))?;
        service.bind_task_manager(&tasks)?;
        let shared = Arc::new(Mutex::new(service));
        codewhale_util::utils::write_atomic(&root.join(format!("{role}-ready")), b"ready")?;
        wait_fixture_path(&root.join(format!("{role}-go"))).await?;
        let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
        scheduler_tick_with(&shared, &tasks.data_dir(), {
            let tasks = tasks.clone();
            let root = root.clone();
            let role = role.clone();
            let observations = observations.clone();
            move |mut run| {
                let tasks = tasks.clone();
                let root = root.clone();
                let role = role.clone();
                let observations = observations.clone();
                async move {
                    let id = run.task_id.clone().expect("durably claimed fixture id");
                    observations.lock().unwrap().push(id.clone());
                    codewhale_util::utils::write_atomic(
                        &root.join(format!("{role}-entered")),
                        id.as_bytes(),
                    )
                    .expect("record dispatch entry");
                    if role == "first" {
                        codewhale_util::utils::write_atomic(
                            &root.join("first-held"),
                            id.as_bytes(),
                        )
                        .expect("record claim barrier");
                        wait_fixture_path(&root.join("release-first"))
                            .await
                            .expect("release owner");
                    }
                    enqueue_run_task(&mut run, &tasks).await;
                    run
                }
            }
        })
        .await?;
        let ids = observations.lock().unwrap().clone();
        for id in &ids {
            let task = crate::task_manager::wait_for_terminal_state(
                &tasks,
                id,
                std::time::Duration::from_secs(5),
            )
            .await?;
            assert_eq!(task.status, TaskStatus::Completed);
        }
        reconcile_run_statuses_shared(&shared, &tasks).await?;
        codewhale_util::utils::write_atomic(
            &root.join(format!("{role}-done")),
            serde_json::to_vec(&ids)?.as_slice(),
        )?;
        tasks.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn two_scheduler_processes_preserve_the_bound_scope_and_execute_one_task() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
        let automation = fixture_due_automation(&manager, "cross-process", 1);
        let mut first = spawn_scheduler_fixture(root.path(), "first")?;
        let mut second = spawn_scheduler_fixture(root.path(), "second")?;
        // Separate Runtime scopes share storage. The foreign scheduler cannot
        // adopt the first scope's immutable dispatch even before acceptance.
        wait_fixture_path(&root.path().join("first-ready")).await?;
        wait_fixture_path(&root.path().join("second-ready")).await?;
        codewhale_util::utils::write_atomic(&root.path().join("first-go"), b"go")?;
        wait_fixture_path(&root.path().join("first-held")).await?;
        let id = fs::read_to_string(root.path().join("first-held"))?;
        codewhale_util::utils::write_atomic(&root.path().join("second-go"), b"go")?;
        wait_fixture_path(&root.path().join("second-done")).await?;
        finish_scheduler_fixture(&mut second, &root.path().join("second.log")).await?;
        assert!(
            !root.path().join("second-entered").exists(),
            "a live dispatcher still owns the unaccepted intent"
        );
        assert!(
            fixture_executions(&root.path().join("executions")).is_empty(),
            "owner is blocked before actual enqueue"
        );
        codewhale_util::utils::write_atomic(&root.path().join("release-first"), b"release")?;
        finish_scheduler_fixture(&mut first, &root.path().join("first.log")).await?;
        assert_eq!(
            fixture_executions(&root.path().join("executions")),
            vec![id.clone()]
        );
        let entered: Vec<String> =
            serde_json::from_slice(&fs::read(root.path().join("first-done"))?)?;
        assert_eq!(
            entered,
            vec![id.clone()],
            "first process traversed the real enqueue path"
        );
        let runs = manager.list_runs(&automation.id, None)?;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].task_id.as_deref(), Some(id.as_str()));
        assert_eq!(runs[0].status, AutomationRunStatus::Completed);
        assert!(runs[0].dispatch.as_ref().unwrap().accepted);
        let task: crate::task_manager::TaskRecord = serde_json::from_slice(&fs::read(
            root.path().join("tasks/tasks").join(format!("{id}.json")),
        )?)?;
        assert_eq!(task.id, id);
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(
            task.result_summary
                .as_deref()
                .unwrap_or_default()
                .contains("automation fixture completed")
        );
        Ok(())
    }

    struct AutomationNoopExecutor;
    struct AutomationWatcherNoopExecutor;

    /// A deterministic America/New_York-compatible zone for the 2026 DST
    /// boundary tests. Keeping the transition table local avoids mutating the
    /// process-wide `TZ` setting while the test binary runs in parallel.
    #[derive(Debug, Clone, Copy)]
    struct Eastern2026;

    impl Eastern2026 {
        fn standard_offset() -> FixedOffset {
            FixedOffset::west_opt(5 * 60 * 60).expect("valid standard offset")
        }

        fn daylight_offset() -> FixedOffset {
            FixedOffset::west_opt(4 * 60 * 60).expect("valid daylight offset")
        }

        fn time(month: u32, day: u32, hour: u32) -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2026, month, day)
                .expect("valid transition date")
                .and_hms_opt(hour, 0, 0)
                .expect("valid transition time")
        }
    }

    impl TimeZone for Eastern2026 {
        type Offset = FixedOffset;

        fn from_offset(_offset: &Self::Offset) -> Self {
            Self
        }

        fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<Self::Offset> {
            self.offset_from_local_datetime(
                &local
                    .and_hms_opt(12, 0, 0)
                    .expect("valid local date midpoint"),
            )
        }

        fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<Self::Offset> {
            let gap_start = Self::time(3, 8, 2);
            let gap_end = Self::time(3, 8, 3);
            let fold_start = Self::time(11, 1, 1);
            let fold_end = Self::time(11, 1, 2);

            if *local >= gap_start && *local < gap_end {
                LocalResult::None
            } else if *local >= fold_start && *local < fold_end {
                LocalResult::Ambiguous(Self::daylight_offset(), Self::standard_offset())
            } else if *local >= gap_end && *local < fold_start {
                LocalResult::Single(Self::daylight_offset())
            } else {
                LocalResult::Single(Self::standard_offset())
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> Self::Offset {
            self.offset_from_utc_datetime(
                &utc.and_hms_opt(12, 0, 0).expect("valid UTC date midpoint"),
            )
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> Self::Offset {
            let daylight_start = Self::time(3, 8, 7);
            let daylight_end = Self::time(11, 1, 6);
            if *utc >= daylight_start && *utc < daylight_end {
                Self::daylight_offset()
            } else {
                Self::standard_offset()
            }
        }
    }

    #[async_trait]
    impl TaskExecutor for AutomationNoopExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("done".to_string()),
                error: None,
                terminal_reason: crate::task_manager::TaskTerminalReason::Completed,
            }
        }
    }

    #[async_trait]
    impl TaskExecutor for AutomationWatcherNoopExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some(AUTOMATION_WATCHER_NO_REPORT_SENTINEL.to_string()),
                error: None,
                terminal_reason: crate::task_manager::TaskTerminalReason::Completed,
            }
        }
    }

    fn automation_task_config(root: PathBuf) -> TaskManagerConfig {
        TaskManagerConfig {
            data_dir: root,
            worker_count: 1,
            default_workspace: PathBuf::from("."),
            default_model: "deepseek-v4-flash".to_string(),
            default_mode: "plan".to_string(),
            allow_shell: true,
            trust_mode: true,
            execution_limits: crate::task_manager::TaskExecutionLimits::default(),
        }
    }

    fn automation_record_with_settings(
        mode: Option<&str>,
        allow_shell: Option<bool>,
        trust_mode: Option<bool>,
        auto_approve: Option<bool>,
    ) -> AutomationRecord {
        let now = Utc::now();
        AutomationRecord {
            destination_thread_id: None,
            schema_version: CURRENT_AUTOMATION_SCHEMA_VERSION,
            execution_scope: Some(crate::task_manager::test_execution_scope("test")),
            id: Uuid::new_v4().to_string(),
            name: "Test automation".to_string(),
            prompt: "Run the automation".to_string(),
            rrule: "FREQ=HOURLY;INTERVAL=1".to_string(),
            cwds: Vec::new(),
            model: None,
            model_provider: None,
            model_provider_id: None,
            mode: mode.map(ToString::to_string),
            allow_shell,
            trust_mode,
            auto_approve,
            delivery_mode: None,
            status: AutomationStatus::Active,
            created_at: now,
            updated_at: now,
            next_run_at: None,
            last_run_at: None,
            revision: 0,
        }
    }

    fn queued_run_for(automation: &AutomationRecord) -> AutomationRunRecord {
        let now = Utc::now();
        AutomationRunRecord {
            schema_version: CURRENT_RUN_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            automation_id: automation.id.clone(),
            scheduled_for: now,
            status: AutomationRunStatus::Queued,
            created_at: now,
            started_at: None,
            ended_at: None,
            task_id: None,
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        }
    }

    fn eastern_datetime(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Eastern2026
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("unambiguous Eastern wall time")
            .with_timezone(&Utc)
    }

    fn anchored_automation(
        created_at: DateTime<Utc>,
        status: AutomationStatus,
    ) -> AutomationRecord {
        let mut record = automation_record_with_settings(None, None, None, None);
        record.rrule = "FREQ=HOURLY;INTERVAL=7;BYMINUTE=17".to_string();
        record.status = status;
        record.created_at = created_at;
        record.updated_at = created_at;
        record.next_run_at = None;
        record
    }

    fn local_naive_to_utc(naive: NaiveDateTime) -> DateTime<Utc> {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .expect("valid unambiguous local time")
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_hourly_rrule() {
        let parsed =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=2;BYDAY=MO,TU").expect("parse");
        match parsed {
            AutomationSchedule::Hourly {
                interval_hours,
                byday,
                ..
            } => {
                assert_eq!(interval_hours, 2);
                assert_eq!(byday.expect("byday").len(), 2);
            }
            _ => panic!("expected hourly"),
        }
    }

    #[test]
    fn parses_once_rrule() {
        let parsed =
            AutomationSchedule::parse_rrule("FREQ=ONCE;AT=2026-08-03T14:30").expect("parse");
        match parsed {
            AutomationSchedule::Once { at } => {
                assert_eq!(
                    at,
                    local_naive_to_utc(
                        NaiveDateTime::parse_from_str("2026-08-03T14:30", "%Y-%m-%dT%H:%M")
                            .expect("naive")
                    )
                );
            }
            _ => panic!("expected once"),
        }
    }

    #[test]
    fn parses_hourly_clock_anchor() {
        let parsed =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=24;BYHOUR=8;BYMINUTE=30")
                .expect("parse anchored hourly schedule");

        assert!(matches!(
            parsed,
            AutomationSchedule::Hourly {
                anchor_hour: Some(8),
                anchor_minute: Some(30),
                ..
            }
        ));

        let minute_only = AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=1;BYMINUTE=15")
            .expect("parse minute-only anchor");
        assert!(matches!(
            minute_only,
            AutomationSchedule::Hourly {
                anchor_hour: None,
                anchor_minute: Some(15),
                ..
            }
        ));
    }

    #[test]
    fn anchored_hourly_schedule_keeps_wall_time_across_spring_forward() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=24;BYHOUR=8;BYMINUTE=30")
                .expect("parse");
        let created_at = eastern_datetime(2026, 3, 6, 7, 0);
        let after = eastern_datetime(2026, 3, 7, 9, 0);

        let next = schedule
            .next_after_in_timezone(after, created_at, &Eastern2026)
            .expect("next run");

        assert_eq!(next, eastern_datetime(2026, 3, 8, 8, 30));
    }

    #[test]
    fn anchored_hourly_schedule_keeps_wall_time_across_fall_back() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=24;BYHOUR=8;BYMINUTE=30")
                .expect("parse");
        let created_at = eastern_datetime(2026, 10, 30, 7, 0);
        let after = eastern_datetime(2026, 10, 31, 9, 0);

        let next = schedule
            .next_after_in_timezone(after, created_at, &Eastern2026)
            .expect("next run");

        assert_eq!(next, eastern_datetime(2026, 11, 1, 8, 30));
    }

    #[test]
    fn anchored_hourly_schedule_skips_nonexistent_wall_time() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=24;BYHOUR=2;BYMINUTE=30")
                .expect("parse");
        let created_at = eastern_datetime(2026, 3, 7, 1, 0);
        let after = eastern_datetime(2026, 3, 7, 3, 0);

        let next = schedule
            .next_after_in_timezone(after, created_at, &Eastern2026)
            .expect("next run after spring-forward gap");

        assert_eq!(next, eastern_datetime(2026, 3, 9, 2, 30));
    }

    #[test]
    fn anchored_hourly_schedule_uses_first_ambiguous_wall_time_once() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=HOURLY;INTERVAL=24;BYHOUR=1;BYMINUTE=30")
                .expect("parse");
        let created_at = eastern_datetime(2026, 10, 31, 0, 0);
        let after = eastern_datetime(2026, 10, 31, 2, 0);
        let first_fold_occurrence = Eastern2026
            .with_ymd_and_hms(2026, 11, 1, 1, 30, 0)
            .earliest()
            .expect("first fold occurrence")
            .with_timezone(&Utc);

        let next = schedule
            .next_after_in_timezone(after, created_at, &Eastern2026)
            .expect("next run at fall-back fold");
        assert_eq!(next, first_fold_occurrence);

        let during_second_fold = Eastern2026
            .with_ymd_and_hms(2026, 11, 1, 1, 15, 0)
            .latest()
            .expect("second fold occurrence")
            .with_timezone(&Utc);
        let after_fold = schedule
            .next_after_in_timezone(during_second_fold, created_at, &Eastern2026)
            .expect("next run after fold");
        assert_eq!(after_fold, eastern_datetime(2026, 11, 2, 1, 30));
    }

    #[test]
    fn anchored_hourly_schedule_reuses_persisted_anchor_after_restart_and_resume() {
        let rrule = "FREQ=HOURLY;INTERVAL=24;BYHOUR=8;BYMINUTE=30";
        let created_at = eastern_datetime(2026, 3, 6, 7, 0);
        let schedule = AutomationSchedule::parse_rrule(rrule).expect("parse");
        let before_restart = schedule
            .next_after_in_timezone(
                eastern_datetime(2026, 3, 7, 12, 0),
                created_at,
                &Eastern2026,
            )
            .expect("next before restart");
        assert_eq!(before_restart, eastern_datetime(2026, 3, 8, 8, 30));

        // Reparsing models a process restart; the persisted creation timestamp
        // remains the recurrence anchor when the record is loaded or resumed.
        let restarted = AutomationSchedule::parse_rrule(rrule).expect("reparse after restart");
        let after_restart = restarted
            .next_after_in_timezone(
                eastern_datetime(2026, 3, 8, 10, 0),
                created_at,
                &Eastern2026,
            )
            .expect("next after restart");
        assert_eq!(after_restart, eastern_datetime(2026, 3, 9, 8, 30));

        let after_resume = restarted
            .next_after_in_timezone(
                eastern_datetime(2026, 3, 10, 12, 0),
                created_at,
                &Eastern2026,
            )
            .expect("next after resume");
        assert_eq!(after_resume, eastern_datetime(2026, 3, 11, 8, 30));
    }

    #[test]
    fn scheduler_restart_uses_persisted_creation_anchor() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let created_at = now - Duration::hours(51);
        let automation = anchored_automation(created_at, AutomationStatus::Active);
        let schedule = AutomationSchedule::parse_rrule(&automation.rrule).expect("parse");
        let expected = schedule
            .next_after_with_anchor(now, created_at)
            .expect("persisted-anchor schedule");
        let reset_anchor = schedule
            .next_after_with_anchor(now, now)
            .expect("reset-anchor schedule");
        assert_ne!(expected, reset_anchor, "fixture must detect anchor resets");

        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        manager.save_automation(&automation).expect("save");
        drop(manager);

        let restarted =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("reopen");
        assert!(
            restarted
                .collect_due_runs(now)
                .expect("restart tick")
                .is_empty(),
            "an uninitialized future slot must not enqueue immediately"
        );
        let reloaded = restarted
            .get_automation(&automation.id)
            .expect("reloaded automation");
        assert_eq!(reloaded.next_run_at, Some(expected));
    }

    #[test]
    fn resume_uses_persisted_creation_anchor() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let before = Utc::now();
        let created_at = before - Duration::hours(51);
        let automation = anchored_automation(created_at, AutomationStatus::Paused);
        let schedule = AutomationSchedule::parse_rrule(&automation.rrule).expect("parse");
        manager.save_automation(&automation).expect("save");

        let expected_before = schedule
            .next_after_with_anchor(before, created_at)
            .expect("next before resume");
        let reset_anchor = schedule
            .next_after_with_anchor(before, before)
            .expect("reset-anchor schedule");
        assert_ne!(
            expected_before, reset_anchor,
            "fixture must detect anchor resets"
        );

        let resumed = manager
            .resume_automation(&automation.id)
            .expect("resume automation");
        let after = Utc::now();
        let expected_after = schedule
            .next_after_with_anchor(after, created_at)
            .expect("next after resume");
        let actual = resumed.next_run_at.expect("resumed next run");
        assert!(
            actual == expected_before || actual == expected_after,
            "resume must keep the persisted creation anchor"
        );
    }

    #[test]
    fn anchored_hourly_schedule_applies_byday_on_calendar_slots() {
        let schedule = AutomationSchedule::parse_rrule(
            "FREQ=HOURLY;INTERVAL=24;BYDAY=MO,TU,WE,TH,FR;BYHOUR=8;BYMINUTE=30",
        )
        .expect("parse");
        let created_at = eastern_datetime(2026, 3, 6, 7, 0);

        let next = schedule
            .next_after_in_timezone(eastern_datetime(2026, 3, 6, 9, 0), created_at, &Eastern2026)
            .expect("next weekday run");

        assert_eq!(next, eastern_datetime(2026, 3, 9, 8, 30));
    }

    #[test]
    fn parses_weekly_rrule() {
        let parsed =
            AutomationSchedule::parse_rrule("FREQ=WEEKLY;BYDAY=MO,WE;BYHOUR=9;BYMINUTE=30")
                .expect("parse");
        match parsed {
            AutomationSchedule::Weekly {
                byday,
                byhour,
                byminute,
            } => {
                assert_eq!(byday.len(), 2);
                assert_eq!(byhour, 9);
                assert_eq!(byminute, 30);
            }
            _ => panic!("expected weekly"),
        }
    }

    #[test]
    fn parses_cron_rrule_and_computes_next_minute_slot() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=CRON;EXPR=*/17 * * * *").expect("parse");
        let after = Utc
            .with_ymd_and_hms(2026, 8, 3, 9, 17, 1)
            .single()
            .expect("after");
        let next = schedule
            .next_after_in_timezone(after, after, &Utc)
            .expect("next cron run");
        assert_eq!(
            next,
            Utc.with_ymd_and_hms(2026, 8, 3, 9, 34, 0)
                .single()
                .expect("next")
        );
    }

    #[test]
    fn cron_weekday_schedule_uses_standard_five_field_local_time() {
        let schedule =
            AutomationSchedule::parse_rrule("FREQ=CRON;EXPR=3 9 * * MON-FRI").expect("parse");
        let after = Utc
            .with_ymd_and_hms(2026, 8, 7, 9, 4, 0)
            .single()
            .expect("after");
        let next = schedule
            .next_after_in_timezone(after, after, &Utc)
            .expect("next weekday cron run");
        assert_eq!(
            next,
            Utc.with_ymd_and_hms(2026, 8, 10, 9, 3, 0)
                .single()
                .expect("next")
        );
    }

    #[test]
    fn cron_rejects_impossible_date() {
        let err = AutomationSchedule::parse_rrule("FREQ=CRON;EXPR=0 9 31 2 *")
            .expect_err("impossible february date must fail");
        assert!(err.to_string().contains("can never occur"));
    }

    #[test]
    fn rejects_invalid_rrule_fields() {
        let err =
            AutomationSchedule::parse_rrule("FREQ=WEEKLY;BYSECOND=5").expect_err("should fail");
        assert!(err.to_string().contains("Unsupported RRULE field"));
    }

    #[test]
    fn automation_model_round_trips_through_create_and_update() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");

        let created = manager
            .create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "Pinned model".to_string(),
                prompt: "prompt".to_string(),
                rrule: "FREQ=HOURLY;INTERVAL=1".to_string(),
                cwds: Vec::new(),
                model: Some("  scheduled-model  ".to_string()),
                model_provider: None,
                model_provider_id: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                delivery_mode: None,
                status: Some(AutomationStatus::Active),
            })
            .expect("create");
        assert_eq!(created.model.as_deref(), Some("scheduled-model"));
        assert_eq!(
            manager
                .get_automation(&created.id)
                .expect("reload")
                .model
                .as_deref(),
            Some("scheduled-model")
        );

        let updated = manager
            .update_automation(
                &created.id,
                UpdateAutomationRequest {
                    model: Some("replacement-model".to_string()),
                    ..UpdateAutomationRequest::default()
                },
            )
            .expect("update");
        assert_eq!(updated.model.as_deref(), Some("replacement-model"));
    }

    #[test]
    fn delete_refuses_active_receipts_then_archives_settled_runs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");

        let created = manager
            .create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "Delete me".to_string(),
                prompt: "prompt".to_string(),
                rrule: "FREQ=HOURLY;INTERVAL=1".to_string(),
                cwds: Vec::new(),
                model: None,
                model_provider: None,
                model_provider_id: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                delivery_mode: None,
                status: Some(AutomationStatus::Active),
            })
            .expect("create");

        let run = AutomationRunRecord {
            schema_version: CURRENT_RUN_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            automation_id: created.id.clone(),
            scheduled_for: Utc::now(),
            status: AutomationRunStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            task_id: None,
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        };
        manager.save_run(&run).expect("save run");
        let settled = AutomationRunRecord {
            id: Uuid::new_v4().to_string(),
            status: AutomationRunStatus::Completed,
            created_at: run.created_at - Duration::minutes(1),
            ended_at: Some(Utc::now() - Duration::minutes(1)),
            ..run.clone()
        };
        manager.save_run(&settled).expect("save settled run");
        assert!(
            manager
                .runs_dir_for(&created.id)
                .expect("runs dir")
                .exists()
        );

        // An unsettled receipt blocks deletion: the definition and both runs
        // survive untouched, and nothing is archived yet.
        let refused = manager
            .delete_automation(&created.id)
            .expect_err("active run must block deletion");
        assert!(refused.to_string().contains("Refusing to delete"));
        assert!(manager.get_automation(&created.id).is_ok());
        assert_eq!(
            manager
                .list_runs(&created.id, None)
                .expect("retained receipts")
                .len(),
            2
        );
        assert!(
            manager
                .list_archived_runs(&created.id)
                .expect("archive")
                .is_empty()
        );

        // Once the receipt settles, deletion archives the terminal history
        // and removes the live paths.
        let mut settled_run = run.clone();
        settled_run.status = AutomationRunStatus::Completed;
        settled_run.started_at = Some(Utc::now());
        settled_run.ended_at = Some(Utc::now());
        manager.save_run(&settled_run).expect("settle run");

        // A sortable record shadows its legacy path while listing. Make the
        // older receipt's legacy path a directory so live cleanup fails only
        // after both records are archived and the newer receipt is removed.
        let blocked_legacy = manager
            .legacy_run_path(&created.id, &settled.id)
            .expect("legacy path");
        fs::create_dir(&blocked_legacy).expect("block legacy cleanup");
        let failed = manager
            .delete_automation(&created.id)
            .expect_err("live cleanup must fail");
        assert!(failed.to_string().contains("Failed to delete run"));
        assert!(
            manager.get_automation(&created.id).is_ok(),
            "the definition must survive partial cleanup so deletion can retry"
        );
        assert!(
            !manager
                .run_path(&settled_run)
                .expect("newer run path")
                .exists(),
            "a newer receipt was already removed before the failure"
        );
        assert_eq!(
            manager
                .list_archived_runs(&created.id)
                .expect("partial archive")
                .len(),
            2,
            "both terminal records reached the archive before any live removal"
        );
        fs::remove_dir(&blocked_legacy).expect("unblock cleanup");
        manager
            .delete_automation(&created.id)
            .expect("retry deletion after partial cleanup");

        assert!(manager.get_automation(&created.id).is_err());
        assert!(
            manager
                .list_runs(&created.id, None)
                .expect("live runs")
                .is_empty()
        );
        assert!(
            !manager
                .runs_dir_for(&created.id)
                .expect("runs dir")
                .exists()
        );
        let archived = manager
            .list_archived_runs(&created.id)
            .expect("archived runs");
        assert_eq!(
            archived.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![settled_run.id.as_str(), settled.id.as_str()],
            "archive keeps both terminal runs, newest-ended first"
        );
        assert_eq!(archived[0].status, AutomationRunStatus::Completed);
        assert_eq!(archived[1].status, AutomationRunStatus::Completed);
        let archive_dir = tempdir.path().join("archive").join(&created.id);
        assert_eq!(
            fs::read_dir(&archive_dir)
                .expect("archive dir")
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count(),
            2,
            "one archived file per terminal run"
        );
    }

    #[test]
    fn delete_refuses_automation_with_active_run() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        manager.save_automation(&automation).expect("save");
        manager
            .save_run(&queued_run_for(&automation))
            .expect("save queued run");

        let err = manager
            .delete_automation(&automation.id)
            .expect_err("delete must refuse while a run is active");
        assert!(err.to_string().contains("Refusing to delete"));

        assert!(manager.get_automation(&automation.id).is_ok());
        assert!(
            manager
                .runs_dir_for(&automation.id)
                .expect("runs dir")
                .exists()
        );
        assert!(
            manager
                .list_archived_runs(&automation.id)
                .expect("archive")
                .is_empty()
        );
    }

    #[test]
    fn delete_caps_archived_terminal_runs_at_retention_limit() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        manager.save_automation(&automation).expect("save");

        let base = Utc::now();
        let total = ARCHIVE_RETAINED_TERMINAL_RUNS + 5;
        let mut saved_ids = Vec::new();
        for i in 0..total {
            let mut run = run_created_at(&automation, base - Duration::minutes(i as i64));
            run.status = AutomationRunStatus::Completed;
            run.ended_at = Some(base - Duration::minutes(i as i64));
            manager.save_run(&run).expect("save run");
            saved_ids.push(run.id);
        }

        manager.delete_automation(&automation.id).expect("delete");

        let archived = manager
            .list_archived_runs(&automation.id)
            .expect("archived runs");
        assert_eq!(
            archived.len(),
            ARCHIVE_RETAINED_TERMINAL_RUNS,
            "archive keeps only the retention budget"
        );
        let retained: std::collections::BTreeSet<&str> =
            archived.iter().map(|r| r.id.as_str()).collect();
        // `saved_ids` is newest-first (i counts minutes into the past); the
        // `total - budget` oldest, at the tail, were dropped.
        for dropped in &saved_ids[ARCHIVE_RETAINED_TERMINAL_RUNS..] {
            assert!(!retained.contains(dropped.as_str()), "oldest run survived");
        }
        for kept in &saved_ids[..ARCHIVE_RETAINED_TERMINAL_RUNS] {
            assert!(retained.contains(kept.as_str()), "newer run was dropped");
        }
    }

    #[test]
    fn scheduler_scan_ignores_archived_runs_after_delete() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = fixture_due_automation(&manager, "archived", 1);
        let mut run = run_created_at(&automation, Utc::now());
        run.status = AutomationRunStatus::Completed;
        run.ended_at = Some(Utc::now());
        manager.save_run(&run).expect("save run");

        // The due definition is collected before deletion and must not come
        // back from the archive afterwards.
        let due_before = manager
            .collect_due_runs(Utc::now())
            .expect("scheduler scan");
        assert!(
            due_before
                .iter()
                .any(|(record, _)| record.id == automation.id),
            "the fixture automation is due before deletion"
        );

        manager.delete_automation(&automation.id).expect("delete");
        assert!(tempdir.path().join("archive").join(&automation.id).exists());

        let due = manager
            .collect_due_runs(Utc::now())
            .expect("scheduler scan");
        assert!(
            due.iter().all(|(record, _)| record.id != automation.id),
            "the deleted automation must not come back due"
        );
        assert!(
            manager
                .list_runs(&automation.id, None)
                .expect("live runs")
                .is_empty(),
            "archived runs must not re-enter the live runs tree"
        );
        assert_eq!(
            manager
                .list_archived_runs(&automation.id)
                .expect("archived runs")
                .len(),
            1
        );
    }

    #[test]
    fn automation_storage_rejects_traversal_ids() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().join("root")).expect("manager");
        let escaped_file = tempdir.path().join("escape.json");
        let escaped_runs = tempdir.path().join("escape-runs");

        let err = manager
            .get_automation("../escape")
            .expect_err("traversal automation ids must be rejected");
        assert!(err.to_string().contains("single path component"));
        assert!(!escaped_file.exists());

        let err = manager
            .list_runs("../escape-runs", None)
            .expect_err("traversal run dirs must be rejected");
        assert!(err.to_string().contains("single path component"));
        assert!(!escaped_runs.exists());

        let run = AutomationRunRecord {
            schema_version: CURRENT_RUN_SCHEMA_VERSION,
            id: "../escape-run".to_string(),
            automation_id: Uuid::new_v4().to_string(),
            scheduled_for: Utc::now(),
            status: AutomationRunStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            task_id: None,
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        };
        let err = manager
            .save_run(&run)
            .expect_err("traversal run ids must be rejected");
        assert!(err.to_string().contains("single path component"));
        assert!(!tempdir.path().join("escape-run.json").exists());
    }

    #[test]
    fn automation_task_settings_default_for_legacy_records() {
        let now = Utc::now().to_rfc3339();
        let record: AutomationRecord = serde_json::from_value(serde_json::json!({
            "schema_version": CURRENT_AUTOMATION_SCHEMA_VERSION,
            "id": Uuid::new_v4().to_string(),
            "name": "Legacy automation",
            "prompt": "Run legacy automation",
            "rrule": "FREQ=HOURLY;INTERVAL=1",
            "cwds": [],
            "status": "active",
            "created_at": now,
            "updated_at": now
        }))
        .expect("legacy automation record should deserialize");

        assert_eq!(record.mode, None);
        assert_eq!(record.task_mode(), "agent");
        assert!(!record.task_allow_shell());
        assert!(!record.task_trust_mode());
        assert!(!record.task_auto_approve());
        assert_eq!(record.delivery_mode(), AutomationDeliveryMode::Task);
    }

    #[test]
    fn automation_requests_pin_the_posture_its_bit_stands_for() {
        let now = Utc::now().to_rfc3339();
        let legacy: AutomationRecord = serde_json::from_value(serde_json::json!({
            "schema_version": CURRENT_AUTOMATION_SCHEMA_VERSION,
            "id": Uuid::new_v4().to_string(),
            "name": "Nightly",
            "prompt": "Run the nightly sweep",
            "rrule": "FREQ=DAILY",
            "cwds": [],
            "status": "active",
            "created_at": now,
            "updated_at": now
        }))
        .expect("legacy automation record");

        // No session exists when a scheduler fires, so the request states the
        // posture the record's own authority means instead of leaving the
        // runtime to re-derive it from the bit.
        assert!(legacy.auto_approve.is_none());
        assert_eq!(
            automation_task_request(&legacy)
                .permission_posture
                .as_deref(),
            Some("ask")
        );

        let elevated: AutomationRecord = serde_json::from_value(serde_json::json!({
            "schema_version": CURRENT_AUTOMATION_SCHEMA_VERSION,
            "id": Uuid::new_v4().to_string(),
            "name": "Nightly, unattended",
            "prompt": "Run the nightly sweep",
            "rrule": "FREQ=DAILY",
            "cwds": [],
            "auto_approve": true,
            "status": "active",
            "created_at": now,
            "updated_at": now
        }))
        .expect("elevated automation record");
        assert_eq!(
            automation_task_request(&elevated)
                .permission_posture
                .as_deref(),
            Some("full_access")
        );
    }

    #[tokio::test]
    async fn automation_enqueue_uses_default_and_explicit_task_settings() -> Result<()> {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let task_manager = TaskManager::start_with_executor(
            automation_task_config(tempdir.path().join("tasks")),
            std::sync::Arc::new(AutomationNoopExecutor),
        )
        .await?;

        let default_automation = automation_record_with_settings(None, None, None, None);
        let mut default_run = queued_run_for(&default_automation);
        bind_run_dispatch(
            &mut default_run,
            &default_automation,
            &task_manager.data_dir(),
            false,
        )?;
        enqueue_run_task(&mut default_run, &task_manager).await;
        let default_task = task_manager
            .get_task(default_run.task_id.as_deref().expect("task id"))
            .await?;
        assert_eq!(default_task.model, "deepseek-v4-flash");
        assert_eq!(default_task.mode, "agent");
        assert!(!default_task.allow_shell);
        assert!(!default_task.trust_mode);
        assert!(!default_task.auto_approve);

        let mut explicit_automation =
            automation_record_with_settings(Some("plan"), Some(true), Some(true), Some(true));
        explicit_automation.model = Some("scheduled-model".to_string());
        let mut explicit_run = queued_run_for(&explicit_automation);
        bind_run_dispatch(
            &mut explicit_run,
            &explicit_automation,
            &task_manager.data_dir(),
            false,
        )?;
        enqueue_run_task(&mut explicit_run, &task_manager).await;
        let explicit_task = task_manager
            .get_task(explicit_run.task_id.as_deref().expect("task id"))
            .await?;
        assert_eq!(explicit_task.model, "scheduled-model");
        assert_eq!(explicit_task.mode, "plan");
        assert!(explicit_task.allow_shell);
        assert!(explicit_task.trust_mode);
        assert!(explicit_task.auto_approve);

        task_manager.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn automation_provider_pin_survives_active_route_change_and_legacy_inherits() -> Result<()>
    {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir()?;
        let mut config: codewhale_runtime::config::Config = toml::from_str(
            r#"
provider = "first"
[providers.first]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/first/v1"
api_key = "fixture-first"
model = "private-model"
[providers.second]
kind = "openai-compatible"
base_url = "http://127.0.0.1:9/second/v1"
api_key = "fixture-second"
model = "private-model"
"#,
        )?;
        let automations = AutomationManager::open_for_test(root.path().join("schedules"))?;
        let mut record = automation_record_with_settings(None, None, None, None);
        record.schema_version = 1;
        record.model = Some("private-model".to_string());
        record.model_provider = Some("custom".to_string());
        record.model_provider_id = Some("first".to_string());
        automations.save_automation(&record)?;
        let record = automations.get_automation(&record.id)?;
        assert_eq!(
            record.schema_version, CURRENT_AUTOMATION_SCHEMA_VERSION,
            "a pin cannot be ignored by an older reader"
        );

        let runtime = crate::runtime_threads::RuntimeThreadManager::open(
            config.clone(),
            root.path().to_path_buf(),
            crate::runtime_threads::RuntimeThreadManagerConfig::from_task_data_dir(
                root.path().join("runtime"),
            ),
        )?;
        config.provider = Some("second".to_string());
        runtime.reload_config(config).await?;
        let tasks = TaskManager::start_with_executor(
            automation_task_config(root.path().join("tasks")),
            std::sync::Arc::new(AutomationNoopExecutor),
        )
        .await?;
        let mut run = queued_run_for(&record);
        bind_run_dispatch(&mut run, &record, &tasks.data_dir(), false)?;
        enqueue_run_task(&mut run, &tasks).await;
        let task = tasks
            .get_task(run.task_id.as_deref().expect("task id"))
            .await?;
        let task: crate::task_manager::TaskRecord =
            serde_json::from_slice(&serde_json::to_vec(&task)?)?;
        assert_eq!(task.schema_version, 4);
        let thread = runtime
            .create_thread(ExecutionTask::from(&task).thread_request())
            .await?;
        assert_eq!(thread.model, "private-model");
        assert_eq!(thread.model_provider.as_deref(), Some("custom"));
        assert_eq!(thread.model_provider_id.as_deref(), Some("first"));

        let mut legacy = serde_json::to_value(&record)?;
        let object = legacy.as_object_mut().unwrap();
        object.remove("model_provider");
        object.remove("model_provider_id");
        object.insert("schema_version".to_string(), serde_json::json!(1));
        let legacy: AutomationRecord = serde_json::from_value(legacy)?;
        assert_eq!(legacy.model_provider, None);
        let mut run = queued_run_for(&legacy);
        bind_run_dispatch(&mut run, &legacy, &tasks.data_dir(), false)?;
        enqueue_run_task(&mut run, &tasks).await;
        let task = tasks
            .get_task(run.task_id.as_deref().expect("legacy task id"))
            .await?;
        let mut value = serde_json::to_value(&task)?;
        value.as_object_mut().unwrap().remove("model_provider");
        value.as_object_mut().unwrap().remove("model_provider_id");
        value["schema_version"] = serde_json::json!(2);
        let task: crate::task_manager::TaskRecord = serde_json::from_value(value)?;
        let thread = runtime
            .create_thread(ExecutionTask::from(&task).thread_request())
            .await?;
        assert_eq!(thread.model_provider_id.as_deref(), Some("second"));
        assert_eq!(thread.model, "private-model");
        tasks.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn delayed_trigger_fires_task_with_same_owner_and_skips_legacy_ownerless() -> Result<()> {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let task_manager = TaskManager::start_with_executor(
            automation_task_config(tempdir.path().join("tasks")),
            std::sync::Arc::new(AutomationNoopExecutor),
        )
        .await?;
        let manager = AutomationManager::open_for_test(tempdir.path().join("automations"))?;

        let mut owned = manager.create_trigger(CreateDelayedTriggerRequest {
            destination_thread_id: None,
            fire_at: Utc::now() + Duration::hours(1),
            message: "owned delayed continuation".to_string(),
            workspace: None,
            owner_session_id: Some("session-a".to_string()),
            parent_trigger_id: None,
        })?;
        owned.fire_at = Utc::now() - Duration::minutes(1);
        manager.save_trigger(&owned)?;

        let mut legacy = manager.create_trigger(CreateDelayedTriggerRequest {
            destination_thread_id: None,
            fire_at: Utc::now() + Duration::hours(1),
            message: "legacy delayed continuation".to_string(),
            workspace: None,
            owner_session_id: None,
            parent_trigger_id: None,
        })?;
        legacy.fire_at = Utc::now() - Duration::minutes(1);
        manager.save_trigger(&legacy)?;

        let shared = Arc::new(Mutex::new(manager));
        fire_due_triggers_shared(&shared, &task_manager).await?;

        let manager = shared.lock().await;
        let fired = manager.get_trigger(&owned.trigger_id)?;
        assert_eq!(fired.status, DelayedTriggerStatus::Fired);
        let task = task_manager
            .get_task(fired.task_id.as_deref().expect("fired task id"))
            .await?;
        assert_eq!(task.owner_session_id.as_deref(), Some("session-a"));

        let legacy_after = manager.get_trigger(&legacy.trigger_id)?;
        assert_eq!(legacy_after.status, DelayedTriggerStatus::Pending);
        assert!(legacy_after.task_id.is_none());
        drop(manager);
        task_manager.shutdown();
        Ok(())
    }

    #[test]
    fn legacy_delayed_trigger_deserializes_without_owner() -> Result<()> {
        let now = Utc::now();
        let record: DelayedTriggerRecord = serde_json::from_value(serde_json::json!({
            "schema_version": CURRENT_TRIGGER_SCHEMA_VERSION,
            "trigger_id": "trig_legacy",
            "fire_at": (now + Duration::hours(1)).to_rfc3339(),
            "message": "legacy trigger",
            "status": "pending",
            "created_at": now.to_rfc3339(),
            "fired_at": null,
            "task_id": null,
            "thread_id": null,
            "error": null,
            "parent_trigger_id": null
        }))?;
        assert_eq!(record.owner_session_id, None);
        Ok(())
    }

    fn write_legacy_run_file(manager: &AutomationManager, run: &AutomationRunRecord) {
        let dir = manager.runs_dir_for(&run.automation_id).expect("runs dir");
        fs::create_dir_all(&dir).expect("create runs dir");
        fs::write(
            dir.join(format!("{}.json", run.id)),
            serde_json::to_string_pretty(run).expect("serialize run"),
        )
        .expect("write legacy run");
    }

    fn run_created_at(
        automation: &AutomationRecord,
        created_at: DateTime<Utc>,
    ) -> AutomationRunRecord {
        let mut run = queued_run_for(automation);
        run.created_at = created_at;
        run.scheduled_for = created_at;
        run
    }

    #[test]
    fn interrupted_watcher_migration_deduplicates_before_visibility() -> Result<()> {
        for suppressed in [false, true] {
            let root = tempfile::tempdir()?;
            let manager = AutomationManager::open_for_test(root.path().join("automations"))?;
            let automation = automation_record_with_settings(None, None, None, None);
            let mut run = queued_run_for(&automation);
            write_legacy_run_file(&manager, &run);
            bind_run_dispatch(&mut run, &automation, root.path(), true)?;
            run.status = AutomationRunStatus::Completed;
            let dispatch = run.dispatch.as_mut().unwrap();
            dispatch.accepted = true;
            dispatch.delivery_mode = AutomationDeliveryMode::Watcher;
            dispatch.suppress_report = suppressed;
            // Persist the post-write/pre-legacy-removal crash boundary.
            write_json_atomic(&manager.run_path(&run)?, &run)?;
            let ids = std::collections::BTreeSet::from([run.id.clone()]);
            for visible in [
                manager.list_runs(&automation.id, None)?,
                manager.list_runs(&automation.id, Some(1))?,
                manager.get_runs_by_ids(&automation.id, &ids)?,
            ] {
                assert_eq!(visible.len(), usize::from(!suppressed));
                if let Some(receipt) = visible.first() {
                    assert_eq!(receipt.status, AutomationRunStatus::Completed);
                    assert_eq!(receipt.task_id, run.task_id);
                }
            }
            let durable = manager.list_runs_with_visibility(&automation.id, None, true)?;
            assert_eq!(durable.len(), 1);
            assert_eq!(durable[0].status, AutomationRunStatus::Completed);
            assert_eq!(
                durable[0].dispatch.as_ref().unwrap().suppress_report,
                suppressed
            );
            assert!(manager.collect_pending_runs()?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn save_run_uses_sortable_names_and_migrates_legacy_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        let run = queued_run_for(&automation);

        write_legacy_run_file(&manager, &run);
        manager.save_run(&run).expect("save run");

        let dir = manager.runs_dir_for(&automation.id).expect("runs dir");
        let names: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let expected = format!("{}-{}.json", run_file_stamp(run.created_at), run.id);
        assert_eq!(names, vec![expected.clone()]);
        assert!(has_sortable_run_stem(expected.trim_end_matches(".json")));
        // Legacy uuid stems are not mistaken for sortable names.
        assert!(!has_sortable_run_stem(&run.id));
    }

    #[test]
    fn finish_scheduled_run_persists_run_when_automation_deleted_mid_enqueue() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        manager.save_automation(&automation).expect("save");
        let run = queued_run_for(&automation);

        // Simulate the automation being deleted while the enqueue await ran
        // outside the lock. The task already exists in the task manager at
        // this point, so the run record must still be persisted — an early
        // return here orphans a real running task.
        manager.delete_automation(&automation.id).expect("delete");
        manager
            .finish_scheduled_run(&run, Utc::now())
            .expect("finish");

        let runs = manager.list_runs(&automation.id, None).expect("list runs");
        assert_eq!(
            runs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![run.id.as_str()],
            "run must be persisted even though its automation was deleted"
        );
        assert!(
            manager.get_automation(&automation.id).is_err(),
            "the deleted automation must not be resurrected"
        );
    }

    #[test]
    fn once_schedule_fires_once_and_auto_completes() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let due_at = Utc::now() - Duration::minutes(1);
        let automation = AutomationRecord {
            rrule: due_at
                .format("FREQ=ONCE;AT=%Y-%m-%dT%H:%M:%S+00:00")
                .to_string(),
            next_run_at: Some(due_at),
            created_at: due_at - Duration::minutes(5),
            updated_at: due_at - Duration::minutes(5),
            ..automation_record_with_settings(None, None, None, None)
        };
        manager
            .save_automation(&automation)
            .expect("save automation");

        let due = manager
            .collect_due_runs(Utc::now())
            .expect("collect due runs");
        assert_eq!(due.len(), 1);
        let (observed, proposed) = &due[0];
        let run = manager
            .claim_scheduled_run(observed, proposed.clone(), tempdir.path())
            .expect("claim one-shot occurrence")
            .expect("due occurrence admitted");
        assert_eq!(run.scheduled_for, due_at);

        manager
            .finish_scheduled_run(&run, Utc::now())
            .expect("finish one-shot run");
        let updated = manager
            .get_automation(&automation.id)
            .expect("updated automation");
        assert_eq!(updated.status, AutomationStatus::Paused);
        assert_eq!(updated.next_run_at, None);
        assert!(
            manager
                .collect_due_runs(Utc::now() + Duration::hours(1))
                .expect("later tick")
                .is_empty()
        );
    }

    #[test]
    fn get_runs_by_ids_finds_live_runs_past_the_newest_window() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        let base = Utc::now();

        // The OLDEST run is the long-running task; 25 newer runs stack on
        // top of it while it is still Running.
        let long_running = run_created_at(&automation, base - Duration::minutes(60));
        let mut long_running = long_running;
        long_running.status = AutomationRunStatus::Running;
        long_running.started_at = Some(base - Duration::minutes(60));
        manager.save_run(&long_running).expect("save live run");
        for i in 0..25 {
            let mut newer = run_created_at(&automation, base - Duration::minutes(30 - i as i64));
            newer.status = AutomationRunStatus::Completed;
            newer.ended_at = Some(base - Duration::minutes(29 - i as i64));
            manager.save_run(&newer).expect("save newer settled run");
        }

        let window = manager
            .list_runs(&automation.id, Some(25))
            .expect("windowed list");
        assert!(
            window.iter().all(|run| run.id != long_running.id),
            "the live run sits past the newest-25 window"
        );

        let wanted: std::collections::BTreeSet<String> =
            [long_running.id.clone()].into_iter().collect();
        let found = manager
            .get_runs_by_ids(&automation.id, &wanted)
            .expect("re-read live run");
        assert_eq!(found.len(), 1, "the run is found wherever it sits");
        assert_eq!(found[0].id, long_running.id);
        assert_eq!(found[0].status, AutomationRunStatus::Running);
    }

    #[test]
    fn get_runs_by_ids_ignores_non_json_noise() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        let run = queued_run_for(&automation);
        manager.save_run(&run).expect("save json run");

        let dir = manager.runs_dir_for(&automation.id).expect("runs dir");
        let json = dir
            .read_dir()
            .expect("list")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .expect("json run file");
        let stem = json
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("stem");
        fs::write(dir.join(format!("{stem}.tmp")), "not-json").expect("write tmp noise");
        fs::write(dir.join(format!("{stem}.json.bak")), "not-json").expect("write bak noise");

        let wanted: std::collections::BTreeSet<String> = [run.id.clone()].into_iter().collect();
        let found = manager
            .get_runs_by_ids(&automation.id, &wanted)
            .expect("noise must not poison the re-read");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, run.id);
    }

    #[test]
    fn list_runs_merges_legacy_and_sortable_files_newest_first() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        let base = Utc::now();

        // Legacy files sit at both ends of the timeline to prove the merge is
        // by created_at, not by file-name era.
        let legacy_oldest = run_created_at(&automation, base - Duration::minutes(30));
        let legacy_newest = run_created_at(&automation, base + Duration::minutes(30));
        write_legacy_run_file(&manager, &legacy_oldest);
        write_legacy_run_file(&manager, &legacy_newest);

        let sortable_old = run_created_at(&automation, base - Duration::minutes(20));
        let sortable_new = run_created_at(&automation, base + Duration::minutes(20));
        manager.save_run(&sortable_old).expect("save old");
        manager.save_run(&sortable_new).expect("save new");

        let all = manager.list_runs(&automation.id, None).expect("list all");
        let ids: Vec<&str> = all.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                legacy_newest.id.as_str(),
                sortable_new.id.as_str(),
                sortable_old.id.as_str(),
                legacy_oldest.id.as_str(),
            ]
        );

        let top_two = manager.list_runs(&automation.id, Some(2)).expect("list 2");
        let top_ids: Vec<&str> = top_two.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(
            top_ids,
            vec![legacy_newest.id.as_str(), sortable_new.id.as_str()]
        );
    }

    #[test]
    fn list_runs_with_limit_skips_older_sortable_files_entirely() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let automation = automation_record_with_settings(None, None, None, None);
        let base = Utc::now();

        let newest = run_created_at(&automation, base);
        manager.save_run(&newest).expect("save newest");

        // A corrupt sortable-named file older than the newest run: bounded
        // listing must never open it, while an unbounded listing fails.
        let dir = manager.runs_dir_for(&automation.id).expect("runs dir");
        let stale_stamp = run_file_stamp(base - Duration::minutes(5));
        fs::write(
            dir.join(format!("{stale_stamp}-{}.json", Uuid::new_v4())),
            "{ not json",
        )
        .expect("write corrupt run");

        let bounded = manager
            .list_runs(&automation.id, Some(1))
            .expect("bounded list must not read files beyond the limit");
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].id, newest.id);

        assert!(manager.list_runs(&automation.id, None).is_err());
    }

    #[tokio::test]
    async fn list_automations_completes_during_slow_enqueue() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let manager =
            AutomationManager::open_for_test(tempdir.path().to_path_buf()).expect("manager");
        let created = manager
            .create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "Slow enqueue".to_string(),
                prompt: "prompt".to_string(),
                rrule: "FREQ=HOURLY;INTERVAL=1".to_string(),
                cwds: Vec::new(),
                model: None,
                model_provider: None,
                model_provider_id: None,
                mode: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                delivery_mode: None,
                status: Some(AutomationStatus::Active),
            })
            .expect("create");
        let shared: SharedAutomationManager = Arc::new(Mutex::new(manager));

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        let run_task = tokio::spawn({
            let shared = Arc::clone(&shared);
            let automation_id = created.id.clone();
            let task_data_dir = tempdir.path().to_path_buf();
            async move {
                run_now_with(
                    &shared,
                    &automation_id,
                    &task_data_dir,
                    move |_, mut run| async move {
                        // Delayed task-manager stub: stall the enqueue await until
                        // the test has proven the manager mutex is free.
                        let _ = entered_tx.send(());
                        let _ = release_rx.await;
                        run.status = AutomationRunStatus::Failed;
                        run.ended_at = Some(Utc::now());
                        run.error = Some("stubbed enqueue".to_string());
                        run
                    },
                )
                .await
            }
        });

        entered_rx.await.expect("enqueue phase entered");

        let listed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            shared.lock().await.list_automations()
        })
        .await
        .expect("list_automations must not block behind a slow enqueue")
        .expect("list automations");
        assert_eq!(listed.len(), 1);

        release_tx.send(()).expect("release stub");
        let run = run_task.await.expect("join").expect("run now");
        assert!(matches!(run.status, AutomationRunStatus::Failed));

        // The final run state was persisted after the lock was reacquired.
        let manager = shared.lock().await;
        let runs = manager.list_runs(&created.id, None).expect("list runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
        assert!(matches!(runs[0].status, AutomationRunStatus::Failed));
        let automation = manager.get_automation(&created.id).expect("automation");
        assert!(automation.last_run_at.is_some());
    }

    #[tokio::test]
    async fn watcher_noop_completion_hides_but_retains_consumed_occurrence() -> Result<()> {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let task_manager = TaskManager::start_with_executor(
            automation_task_config(tempdir.path().join("tasks")),
            std::sync::Arc::new(AutomationWatcherNoopExecutor),
        )
        .await?;
        let mut automation = automation_record_with_settings(None, None, None, None);
        automation.delivery_mode = Some(AutomationDeliveryMode::Watcher);
        automation.next_run_at = Some(Utc::now() - Duration::seconds(1));
        let manager =
            AutomationManager::open_for_test(tempdir.path().join("automations")).expect("manager");
        manager
            .save_automation(&automation)
            .expect("save automation");
        let shared: SharedAutomationManager = Arc::new(Mutex::new(manager));

        scheduler_tick_shared(&shared, &task_manager).await?;
        let initial = shared
            .lock()
            .await
            .list_runs_with_visibility(&automation.id, None, true)?;
        assert_eq!(initial.len(), 1);
        let bound_id = initial[0]
            .task_id
            .as_deref()
            .context("watcher task binding")?;
        let completed = crate::task_manager::wait_for_terminal_state(
            &task_manager,
            bound_id,
            std::time::Duration::from_secs(5),
        )
        .await?;
        assert_eq!(completed.status, TaskStatus::Completed);
        reconcile_run_statuses_shared(&shared, &task_manager).await?;

        let manager = shared.lock().await;
        assert!(
            manager.list_runs(&automation.id, None)?.is_empty(),
            "watcher no-op must not leave a phantom run row"
        );
        let durable = manager.list_runs_with_visibility(&automation.id, None, true)?;
        assert_eq!(durable.len(), 1);
        assert_eq!(durable[0].status, AutomationRunStatus::Completed);
        assert!(durable[0].dispatch.as_ref().unwrap().suppress_report);
        let mut updated = manager.get_automation(&automation.id)?;
        assert!(
            updated.next_run_at.is_some(),
            "watcher should keep scheduling"
        );
        assert_eq!(
            updated.last_run_at, None,
            "no-op checks are not reportable runs"
        );
        // Revisit the consumed slot after a torn schedule write. A hidden
        // receipt still owns its occurrence and must prevent another task.
        updated.next_run_at = Some(durable[0].scheduled_for);
        manager.save_automation(&updated)?;
        drop(manager);
        scheduler_tick_shared(&shared, &task_manager).await?;
        assert_eq!(task_manager.list_tasks(None).await?.len(), 1);
        assert!(
            shared
                .lock()
                .await
                .list_runs(&automation.id, None)?
                .is_empty()
        );
        task_manager.shutdown();
        Ok(())
    }

    #[test]
    fn default_automations_dir_honors_codewhale_home_as_hard_override() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::remove_var("DEEPSEEK_AUTOMATIONS_DIR");
            std::env::set_var("CODEWHALE_HOME", tmp.path());
        }
        // $CODEWHALE_HOME IS the home dir (no ".codewhale" appended); the
        // legacy ~/.deepseek fallback is bypassed entirely.
        assert_eq!(default_automations_dir(), tmp.path().join("automations"));
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("CODEWHALE_HOME");
        }
    }

    #[test]
    fn default_automations_dir_prefers_deepseek_automations_dir_over_codewhale_home() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: serialised by lock_test_env.
        unsafe {
            std::env::set_var("DEEPSEEK_AUTOMATIONS_DIR", tmp.path());
            std::env::set_var("CODEWHALE_HOME", "/should/not/be/used");
        }
        // The most-specific override wins over the base-data-dir override.
        assert_eq!(default_automations_dir(), tmp.path());
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("DEEPSEEK_AUTOMATIONS_DIR");
            std::env::remove_var("CODEWHALE_HOME");
        }
    }
    mod ownership;
    mod recovery;

    /// #6162: the projection's Canceled branch must record why. A cooperative
    /// cancel arrives with no error of its own and takes the derived text; a
    /// task that already carries the task manager's receipt keeps it; a legacy
    /// record with neither degrades to no detail instead of inventing one.
    fn canceled_task_record(
        error: Option<&str>,
        terminal_reason: Option<&str>,
    ) -> crate::task_manager::TaskRecord {
        serde_json::from_value(serde_json::json!({
            "id": "task_canceled",
            "prompt": "nightly report",
            "model": "deepseek-v4-pro",
            "workspace": "/tmp/automation-fixture",
            "mode": "agent",
            "allow_shell": false,
            "trust_mode": false,
            "status": "canceled",
            "created_at": "2026-09-14T10:00:00Z",
            "started_at": "2026-09-14T10:00:01Z",
            "ended_at": "2026-09-14T10:00:05Z",
            "duration_ms": 4000,
            "result_summary": null,
            "result_detail_path": null,
            "error": error,
            "terminal_reason": terminal_reason,
            "tool_calls": [],
            "timeline": [],
        }))
        .expect("a canceled task record fixture")
    }

    #[test]
    fn a_cooperatively_canceled_task_names_the_cancellation_on_the_run() {
        let mut run = new_run_record("auto_1", Utc::now(), Utc::now());
        run.status = AutomationRunStatus::Running;
        let task = canceled_task_record(None, Some("canceled"));
        assert!(apply_task_status(&mut run, &task));
        assert_eq!(run.status, AutomationRunStatus::Canceled);
        assert_eq!(run.error.as_deref(), Some("canceled by request"));
        assert_eq!(run.started_at, task.started_at);
        assert_eq!(run.ended_at, task.ended_at);
    }

    #[test]
    fn a_canceled_task_with_its_own_error_keeps_that_error_on_the_run() {
        let mut run = new_run_record("auto_1", Utc::now(), Utc::now());
        run.status = AutomationRunStatus::Running;
        let task = canceled_task_record(
            Some("Task canceled because the task manager shut down"),
            Some("shutdown"),
        );
        assert!(apply_task_status(&mut run, &task));
        assert_eq!(run.status, AutomationRunStatus::Canceled);
        assert_eq!(
            run.error.as_deref(),
            Some("Task canceled because the task manager shut down")
        );
    }

    #[test]
    fn a_legacy_canceled_task_without_a_reason_records_no_detail() {
        let mut run = new_run_record("auto_1", Utc::now(), Utc::now());
        run.status = AutomationRunStatus::Running;
        let task = canceled_task_record(None, None);
        assert!(apply_task_status(&mut run, &task));
        assert_eq!(run.status, AutomationRunStatus::Canceled);
        assert_eq!(run.error, None);
    }
}
