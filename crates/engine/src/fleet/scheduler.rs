//! Fleet scheduler policy: leases, heartbeats, backpressure, and recovery.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use codewhale_protocol::fleet::*;
use serde_json::Value;

use super::ledger::{FleetLedger, FleetLedgerState, FleetTaskLedgerStatus, FleetTaskState};

#[derive(Debug, Clone)]
pub struct FleetSchedulerPolicy {
    pub max_workers_per_run: usize,
    pub max_workers_per_host: usize,
    pub max_workers_per_task_class: usize,
    pub lease_seconds: u64,
    pub heartbeat_timeout: Duration,
}

impl Default for FleetSchedulerPolicy {
    fn default() -> Self {
        Self {
            max_workers_per_run: 4,
            max_workers_per_host: 4,
            max_workers_per_task_class: 4,
            lease_seconds: 300,
            heartbeat_timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetSchedulerReport {
    pub launched: usize,
    pub heartbeats: usize,
    pub marked_stale: usize,
    pub restarted: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub alerts: usize,
}

#[derive(Debug)]
pub struct FleetScheduler {
    ledger: FleetLedger,
    policy: FleetSchedulerPolicy,
    now: DateTime<Utc>,
}

impl FleetScheduler {
    pub fn open(workspace: impl AsRef<Path>, policy: FleetSchedulerPolicy) -> Result<Self> {
        Ok(Self {
            ledger: FleetLedger::open(workspace.as_ref())?,
            policy,
            now: Utc::now(),
        })
    }

    pub fn set_now(&mut self, now: DateTime<Utc>) {
        self.now = now;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn tick_run(&self, run_id: &FleetRunId) -> Result<FleetSchedulerReport> {
        let mut report = FleetSchedulerReport::default();
        self.recover_unhealthy_work(run_id, &mut report, &mut |_, _, _, _| Ok(()))?;
        self.launch_queued_work(run_id, &mut report)?;
        self.refresh_run_status(run_id)?;
        Ok(report)
    }

    /// Resume reconciliation after a manager restart: detect orphaned/stale
    /// in-flight leases left by a prior process and apply retry/escalation
    /// policy, then recompute run status.
    ///
    /// Unlike `tick_run`, this launches no new queued work and does not
    /// re-process tasks that already reached a terminal state, so it is safe
    /// and idempotent to call on a fresh process: a task re-leased by an
    /// earlier resume is no longer stale at the same instant, and a terminally
    /// failed task is never failed or escalated twice.
    #[cfg(any(test, feature = "test-support"))]
    pub fn resume_run(&self, run_id: &FleetRunId) -> Result<FleetSchedulerReport> {
        self.resume_run_with_restart_callback(run_id, |_, _, _, _| Ok(()))
    }

    pub fn resume_run_with_restart_callback(
        &self,
        run_id: &FleetRunId,
        mut on_restarting: impl FnMut(
            &FleetLedgerState,
            &FleetTaskState,
            &FleetTaskSpec,
            &str,
        ) -> Result<()>,
    ) -> Result<FleetSchedulerReport> {
        let mut report = FleetSchedulerReport::default();
        self.reconcile_stale_leases(run_id, &mut report, &mut on_restarting)?;
        self.refresh_run_status(run_id)?;
        Ok(report)
    }

    pub fn cancel_run(&self, run_id: &FleetRunId, reason: &str) -> Result<FleetSchedulerReport> {
        let state = self.ledger.rebuild_state()?;
        let mut report = FleetSchedulerReport::default();
        for task in state
            .tasks
            .values()
            .filter(|task| task.entry.run_id == *run_id)
        {
            if !matches!(
                task.status,
                FleetTaskLedgerStatus::Enqueued | FleetTaskLedgerStatus::Leased
            ) {
                continue;
            }
            if self.ledger.cancel_task_if_active(
                &task.entry.run_id,
                &task.entry.task_id,
                None,
                &self.timestamp(),
                Some(reason),
                Some("scheduler"),
            )? {
                report.cancelled += 1;
            }
        }
        self.ledger
            .update_run_status(run_id, FleetRunStatus::Cancelled, &self.timestamp())?;
        Ok(report)
    }

    fn recover_unhealthy_work(
        &self,
        run_id: &FleetRunId,
        report: &mut FleetSchedulerReport,
        on_restarting: &mut dyn FnMut(
            &FleetLedgerState,
            &FleetTaskState,
            &FleetTaskSpec,
            &str,
        ) -> Result<()>,
    ) -> Result<()> {
        let state = self.ledger.rebuild_state()?;
        for task in state
            .tasks
            .values()
            .filter(|task| task.entry.run_id == *run_id)
        {
            let Some(task_spec) = task_spec_for(&state, task) else {
                continue;
            };
            match task.status {
                FleetTaskLedgerStatus::Leased if self.task_is_stale(task, &state) => {
                    let worker_id = task
                        .leased_to
                        .clone()
                        .unwrap_or_else(|| "fleet-scheduler".to_string());
                    let heartbeat_at = state
                        .heartbeats
                        .get(&worker_id)
                        .map(|heartbeat| heartbeat.timestamp.as_str());
                    let latest_seq = state
                        .latest_seq
                        .get(&format!(
                            "{}:{}:{}",
                            worker_id, task.entry.run_id.0, task.entry.task_id
                        ))
                        .copied()
                        .unwrap_or(0);
                    let Some(stale_event) = self.ledger.append_event_if_lease_unchanged(
                        &task.entry.run_id,
                        &worker_id,
                        &task.entry.task_id,
                        task.entry.attempts,
                        latest_seq,
                        heartbeat_at,
                        &self.timestamp(),
                        FleetWorkerEventPayload::Stale {
                            last_heartbeat_at: heartbeat_at.map(str::to_string),
                        },
                    )?
                    else {
                        continue;
                    };
                    report.marked_stale += 1;
                    self.retry_or_fail(
                        &state,
                        task,
                        &task_spec,
                        &worker_id,
                        stale_event.seq,
                        heartbeat_at,
                        report,
                        on_restarting,
                    )
                    .with_context(|| format!("recovering stale task {}", task.entry.task_id))?;
                }
                FleetTaskLedgerStatus::Failed => {
                    let worker_id = task
                        .leased_to
                        .clone()
                        .unwrap_or_else(|| "fleet-scheduler".to_string());
                    let latest_seq = state
                        .latest_seq
                        .get(&format!(
                            "{}:{}:{}",
                            worker_id, task.entry.run_id.0, task.entry.task_id
                        ))
                        .copied()
                        .unwrap_or(0);
                    let heartbeat_at = state
                        .heartbeats
                        .get(&worker_id)
                        .map(|heartbeat| heartbeat.timestamp.as_str());
                    self.retry_or_fail(
                        &state,
                        task,
                        &task_spec,
                        &worker_id,
                        latest_seq,
                        heartbeat_at,
                        report,
                        on_restarting,
                    )
                    .with_context(|| format!("recovering failed task {}", task.entry.task_id))?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Reconcile only orphaned/stale in-flight leases (the restart-recovery
    /// subset of `recover_unhealthy_work`): a `Leased` task whose worker has
    /// stopped heartbeating is marked stale and routed through the shared
    /// retry/escalation budget. Terminal and healthy tasks are left untouched,
    /// which keeps `resume_run` idempotent.
    fn reconcile_stale_leases(
        &self,
        run_id: &FleetRunId,
        report: &mut FleetSchedulerReport,
        on_restarting: &mut dyn FnMut(
            &FleetLedgerState,
            &FleetTaskState,
            &FleetTaskSpec,
            &str,
        ) -> Result<()>,
    ) -> Result<()> {
        let state = self.ledger.rebuild_state()?;
        for task in state
            .tasks
            .values()
            .filter(|task| task.entry.run_id == *run_id)
        {
            if !matches!(task.status, FleetTaskLedgerStatus::Leased)
                || !self.task_is_stale(task, &state)
            {
                continue;
            }
            let Some(task_spec) = task_spec_for(&state, task) else {
                continue;
            };
            let worker_id = task
                .leased_to
                .clone()
                .unwrap_or_else(|| "fleet-scheduler".to_string());
            let heartbeat_at = state
                .heartbeats
                .get(&worker_id)
                .map(|heartbeat| heartbeat.timestamp.as_str());
            let latest_seq = state
                .latest_seq
                .get(&format!(
                    "{}:{}:{}",
                    worker_id, task.entry.run_id.0, task.entry.task_id
                ))
                .copied()
                .unwrap_or(0);
            let Some(stale_event) = self.ledger.append_event_if_lease_unchanged(
                &task.entry.run_id,
                &worker_id,
                &task.entry.task_id,
                task.entry.attempts,
                latest_seq,
                heartbeat_at,
                &self.timestamp(),
                FleetWorkerEventPayload::Stale {
                    last_heartbeat_at: heartbeat_at.map(str::to_string),
                },
            )?
            else {
                continue;
            };
            report.marked_stale += 1;
            self.retry_or_fail(
                &state,
                task,
                &task_spec,
                &worker_id,
                stale_event.seq,
                heartbeat_at,
                report,
                on_restarting,
            )
            .with_context(|| format!("resuming stale task {}", task.entry.task_id))?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn retry_or_fail(
        &self,
        state: &FleetLedgerState,
        task: &FleetTaskState,
        task_spec: &FleetTaskSpec,
        worker_id: &str,
        expected_latest_seq: u64,
        expected_heartbeat_at: Option<&str>,
        report: &mut FleetSchedulerReport,
        on_restarting: &mut dyn FnMut(
            &FleetLedgerState,
            &FleetTaskState,
            &FleetTaskSpec,
            &str,
        ) -> Result<()>,
    ) -> Result<()> {
        let retry_policy = task_spec.retry_policy.clone().unwrap_or_default();
        if task.entry.attempts < retry_policy.max_attempts {
            let lease_expires_at = self.lease_expires_at();
            if !self.ledger.restart_task_if_unchanged_with_callback(
                &task.entry.run_id,
                &task.entry.task_id,
                worker_id,
                task.status,
                task.entry.attempts,
                expected_latest_seq,
                expected_heartbeat_at,
                &self.timestamp(),
                Some(&lease_expires_at),
                task.entry.attempts,
                || on_restarting(state, task, task_spec, worker_id),
            )? {
                return Ok(());
            }
            report.restarted += 1;
            return Ok(());
        }

        if task.status == FleetTaskLedgerStatus::Failed {
            report.alerts += self.record_alerts(
                &task.entry.run_id,
                &task.entry.task_id,
                worker_id,
                task.entry.attempts,
                task_spec,
                FleetAlertEventClass::RestartExhausted,
            )?;
            return Ok(());
        }

        let terminal = self.ledger.append_terminal_event_if_lease_unchanged(
            &task.entry.run_id,
            worker_id,
            &task.entry.task_id,
            task.entry.attempts,
            expected_latest_seq,
            expected_heartbeat_at,
            &self.timestamp(),
            FleetWorkerEventPayload::Failed {
                reason: format!(
                    "retry attempts exhausted after {} attempt(s)",
                    task.entry.attempts
                ),
                recoverable: false,
            },
        )?;
        if terminal.is_none() {
            return Ok(());
        }
        report.failed += 1;
        report.alerts += self.record_alerts(
            &task.entry.run_id,
            &task.entry.task_id,
            worker_id,
            task.entry.attempts,
            task_spec,
            FleetAlertEventClass::RestartExhausted,
        )?;
        Ok(())
    }

    fn launch_queued_work(
        &self,
        run_id: &FleetRunId,
        report: &mut FleetSchedulerReport,
    ) -> Result<()> {
        loop {
            let state = self.ledger.rebuild_state()?;
            let run = state
                .runs
                .get(&run_id.0)
                .ok_or_else(|| anyhow!("Fleet run {} does not exist", run_id.0))?;
            let active = active_tasks_for_run(&state, run_id);
            if active.len() >= self.policy.max_workers_per_run {
                return Ok(());
            }
            let counts = active_counts(&state, run);
            let Some((worker_id, task, decision)) = self.next_launch(run, &state, &counts) else {
                return Ok(());
            };
            let workspace = self
                .ledger
                .path()
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| anyhow!("Fleet ledger has no workspace"))?;
            let placement = super::placement::write_receipt(
                workspace,
                run_id,
                task.entry.attempts.saturating_add(1),
                &decision,
            )?;
            let lease_expires_at = self.lease_expires_at();
            if !self.ledger.start_task_if_enqueued(
                &task.entry.run_id,
                &task.entry.task_id,
                &worker_id,
                &self.timestamp(),
                Some(&lease_expires_at),
                Some(self.policy.max_workers_per_run),
                vec![
                    FleetWorkerEventPayload::Leased {
                        lease_expires_at: Some(lease_expires_at.clone()),
                    },
                    FleetWorkerEventPayload::Starting,
                    FleetWorkerEventPayload::Artifact(placement),
                    FleetWorkerEventPayload::Running,
                ],
                || Ok(()),
            )? {
                continue;
            }
            report.launched += 1;
            report.heartbeats += 1;
        }
    }

    fn next_launch(
        &self,
        run: &FleetRun,
        state: &FleetLedgerState,
        counts: &ActiveCounts,
    ) -> Option<(
        String,
        FleetTaskState,
        codewhale_protocol::placement::PlacementDecision,
    )> {
        let active_workers: BTreeSet<_> = active_tasks_for_run(state, &run.id)
            .into_iter()
            .filter_map(|task| task.leased_to)
            .collect();
        let mut queued: Vec<_> = state
            .tasks
            .values()
            .filter(|task| {
                task.entry.run_id == run.id
                    && matches!(task.status, FleetTaskLedgerStatus::Enqueued)
            })
            .cloned()
            .collect();
        queued.sort_by_key(|task| {
            (
                task.entry.priority,
                task.entry.enqueued_at.clone(),
                task.entry.task_id.clone(),
            )
        });
        for task in queued {
            let task_spec = run
                .task_specs
                .iter()
                .find(|spec| spec.id == task.entry.task_id)?;
            let task_class = task_class(task_spec);
            if counts.by_task_class.get(&task_class).copied().unwrap_or(0)
                >= self.policy.max_workers_per_task_class
            {
                continue;
            }
            let unavailable = run
                .worker_specs
                .iter()
                .filter(|worker| {
                    active_workers.contains(&worker.id)
                        || counts.by_host.get(&host_key(worker)).copied().unwrap_or(0)
                            >= self.policy.max_workers_per_host
                })
                .map(|worker| worker.id.clone())
                .collect();
            let decision = super::placement::decide(task_spec, &run.worker_specs, &unavailable);
            if let Some(worker_id) = decision.worker_id.clone() {
                return Some((worker_id, task, decision));
            }
        }
        None
    }

    fn task_is_stale(&self, task: &FleetTaskState, state: &FleetLedgerState) -> bool {
        if let Some(worker_id) = task.leased_to.as_deref()
            && let Some(heartbeat) = state.heartbeats.get(worker_id)
            && let Ok(last) = DateTime::parse_from_rfc3339(&heartbeat.timestamp)
        {
            let age = self.now.signed_duration_since(last.with_timezone(&Utc));
            return age
                .to_std()
                .map_or(true, |age| age > self.policy.heartbeat_timeout);
        }
        if let Some(deadline) = task.entry.lease_deadline.as_deref()
            && let Ok(deadline) = DateTime::parse_from_rfc3339(deadline)
        {
            return self.now > deadline.with_timezone(&Utc);
        }
        true
    }

    fn record_alerts(
        &self,
        run_id: &FleetRunId,
        task_id: &str,
        worker_id: &str,
        expected_attempts: u32,
        task_spec: &FleetTaskSpec,
        event_class: FleetAlertEventClass,
    ) -> Result<usize> {
        let Some(policy) = &task_spec.alert_policy else {
            return Ok(0);
        };
        if !alert_policy_matches(policy, event_class) {
            return Ok(0);
        }
        let mut count = 0;
        for (channel_index, channel) in policy.channels.iter().enumerate() {
            let label = alert_channel_label(channel);
            // A policy may contain multiple endpoints of the same kind. The
            // run snapshot preserves channel order while redacting secrets, so
            // a kind + ordinal key is stable, non-secret, and instance-unique.
            let channel_key = format!("{label}#{channel_index}");
            if self.ledger.record_failed_attempt_alert_once(
                run_id,
                task_id,
                worker_id,
                expected_attempts,
                label,
                &channel_key,
                &self.timestamp(),
            )? {
                count += 1;
            }
        }
        Ok(count)
    }

    fn refresh_run_status(&self, run_id: &FleetRunId) -> Result<()> {
        let state = self.ledger.rebuild_state()?;
        let mut has_open = false;
        let mut has_failed = false;
        let mut has_cancelled = false;
        for task in state
            .tasks
            .values()
            .filter(|task| task.entry.run_id == *run_id)
        {
            match task.status {
                FleetTaskLedgerStatus::Enqueued | FleetTaskLedgerStatus::Leased => has_open = true,
                FleetTaskLedgerStatus::Failed => has_failed = true,
                FleetTaskLedgerStatus::Cancelled => has_cancelled = true,
                FleetTaskLedgerStatus::Completed => {}
            }
        }
        let status = if has_open {
            FleetRunStatus::Running
        } else if has_failed {
            FleetRunStatus::Failed
        } else if has_cancelled {
            FleetRunStatus::Cancelled
        } else {
            FleetRunStatus::Completed
        };
        self.ledger
            .update_run_status(run_id, status, &self.timestamp())
    }

    fn append_worker_event(
        &self,
        run_id: &FleetRunId,
        worker_id: &str,
        task_id: &str,
        payload: FleetWorkerEventPayload,
    ) -> Result<FleetWorkerEvent> {
        self.ledger
            .append_event_next_seq(run_id, worker_id, task_id, &self.timestamp(), payload)
    }

    fn timestamp(&self) -> String {
        self.now.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn lease_expires_at(&self) -> String {
        (self.now + chrono::Duration::seconds(self.policy.lease_seconds as i64))
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

#[derive(Debug, Default)]
struct ActiveCounts {
    by_host: BTreeMap<String, usize>,
    by_task_class: BTreeMap<String, usize>,
}

fn active_counts(state: &FleetLedgerState, run: &FleetRun) -> ActiveCounts {
    let mut counts = ActiveCounts::default();
    for task in active_tasks_for_run(state, &run.id) {
        if let Some(worker_id) = task.leased_to.as_deref()
            && let Some(worker) = run
                .worker_specs
                .iter()
                .find(|worker| worker.id == worker_id)
        {
            *counts.by_host.entry(host_key(worker)).or_default() += 1;
        }
        if let Some(task_spec) = run
            .task_specs
            .iter()
            .find(|spec| spec.id == task.entry.task_id)
        {
            *counts
                .by_task_class
                .entry(task_class(task_spec))
                .or_default() += 1;
        }
    }
    counts
}

fn active_tasks_for_run(state: &FleetLedgerState, run_id: &FleetRunId) -> Vec<FleetTaskState> {
    state
        .tasks
        .values()
        .filter(|task| {
            task.entry.run_id == *run_id && matches!(task.status, FleetTaskLedgerStatus::Leased)
        })
        .cloned()
        .collect()
}

fn task_spec_for(state: &FleetLedgerState, task: &FleetTaskState) -> Option<FleetTaskSpec> {
    state
        .runs
        .get(&task.entry.run_id.0)?
        .task_specs
        .iter()
        .find(|spec| spec.id == task.entry.task_id)
        .cloned()
}

fn host_key(worker: &FleetWorkerSpec) -> String {
    match &worker.host {
        FleetHostSpec::Local => "local".to_string(),
        FleetHostSpec::Ssh { host, .. } => format!("ssh:{host}"),
        FleetHostSpec::Tailscale { node_id, .. } => format!("tailscale:{node_id}"),
        FleetHostSpec::Docker { image, .. } => format!("docker:{image}"),
    }
}

fn task_class(task: &FleetTaskSpec) -> String {
    task.metadata
        .get("class")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("default")
        .to_string()
}

fn alert_channel_label(channel: &FleetAlertChannel) -> &'static str {
    match channel {
        FleetAlertChannel::Slack { .. } => "slack",
        FleetAlertChannel::Webhook { .. } => "webhook",
        FleetAlertChannel::PagerDuty { .. } => "pagerduty",
    }
}

fn alert_policy_matches(policy: &FleetAlertPolicy, class: FleetAlertEventClass) -> bool {
    policy.events.is_empty() || policy.events.contains(&class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    fn base_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-13T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn scheduler(tmp: &TempDir, max_workers: usize) -> FleetScheduler {
        let mut scheduler = FleetScheduler::open(
            tmp.path(),
            FleetSchedulerPolicy {
                max_workers_per_run: max_workers,
                max_workers_per_host: max_workers,
                max_workers_per_task_class: max_workers,
                lease_seconds: 30,
                heartbeat_timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        scheduler.set_now(base_now());
        scheduler
    }

    fn worker(id: &str) -> FleetWorkerSpec {
        FleetWorkerSpec {
            environment: None,
            id: id.to_string(),
            name: id.to_string(),
            host: FleetHostSpec::Local,
            trust_level: Some(FleetTrustLevel::Local),
            labels: BTreeMap::new(),
            capabilities: vec!["local".to_string()],
            max_concurrent_tasks: Some(1),
        }
    }

    fn task(id: &str, max_attempts: u32) -> FleetTaskSpec {
        FleetTaskSpec {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            objective: Some(format!("Schedule {id}")),
            instructions: format!("do {id}"),
            worker: None,
            workspace: None,
            input_files: Vec::new(),
            context: Vec::new(),
            budget: None,
            tags: Vec::new(),
            expected_artifacts: vec![FleetArtifactKind::Log],
            scorer: None,
            retry_policy: Some(FleetRetryPolicy {
                max_attempts,
                ..FleetRetryPolicy::default()
            }),
            alert_policy: None,
            timeout_seconds: None,
            metadata: BTreeMap::new(),
        }
    }

    fn create_run(
        scheduler: &FleetScheduler,
        run_id: &str,
        tasks: Vec<FleetTaskSpec>,
        workers: usize,
    ) {
        let run_id = FleetRunId::from(run_id);
        scheduler
            .ledger
            .create_run(&FleetRun {
                id: run_id.clone(),
                name: "scheduler smoke".to_string(),
                status: FleetRunStatus::Queued,
                target: None,
                workflow: None,
                roles: Vec::new(),
                max_workers: Some(workers),
                usage_ceiling: None,
                task_specs: tasks.clone(),
                worker_specs: (1..=workers)
                    .map(|idx| worker(&format!("worker-{idx}")))
                    .collect(),
                labels: BTreeMap::new(),
                security_policy: None,
                created_at: scheduler.timestamp(),
                updated_at: None,
                completed_at: None,
            })
            .unwrap();
        for task in tasks {
            scheduler
                .ledger
                .enqueue(FleetInboxEntry {
                    run_id: run_id.clone(),
                    task_id: task.id,
                    priority: 0,
                    enqueued_at: scheduler.timestamp(),
                    lease_deadline: None,
                    attempts: 0,
                })
                .unwrap();
        }
    }

    fn ledger_text(scheduler: &FleetScheduler) -> String {
        std::fs::read_to_string(scheduler.ledger.path()).unwrap()
    }

    #[test]
    fn fleet_scheduler_backpressure_prevents_over_launch() {
        let tmp = TempDir::new().unwrap();
        let scheduler = scheduler(&tmp, 2);
        create_run(
            &scheduler,
            "run-1",
            vec![task("task-a", 3), task("task-b", 3), task("task-c", 3)],
            3,
        );

        let report = scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();

        assert_eq!(report.launched, 2);
        let state = scheduler.ledger.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert_eq!(
            state.tasks["run-1:task-b"].status,
            FleetTaskLedgerStatus::Leased
        );
        assert_eq!(
            state.tasks["run-1:task-c"].status,
            FleetTaskLedgerStatus::Enqueued
        );
    }

    #[test]
    fn fleet_scheduler_lost_heartbeat_restarts_within_retry_limit() {
        let tmp = TempDir::new().unwrap();
        let mut scheduler = scheduler(&tmp, 1);
        create_run(&scheduler, "run-1", vec![task("task-a", 2)], 1);
        scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();
        scheduler.set_now(base_now() + chrono::Duration::seconds(10));

        let report = scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();

        assert_eq!(report.marked_stale, 1);
        assert_eq!(report.restarted, 1);
        let state = scheduler.ledger.rebuild_state().unwrap();
        let task = &state.tasks["run-1:task-a"];
        assert_eq!(task.status, FleetTaskLedgerStatus::Leased);
        assert_eq!(task.entry.attempts, 2);
        let ledger = ledger_text(&scheduler);
        assert!(ledger.contains("\"state\":\"stale\""));
        assert!(ledger.contains("\"state\":\"restarted\""));
    }

    #[test]
    fn fleet_scheduler_restart_exhaustion_records_terminal_failure_and_alert() {
        let tmp = TempDir::new().unwrap();
        let mut scheduler = scheduler(&tmp, 1);
        let mut failing = task("task-a", 1);
        failing.alert_policy = Some(FleetAlertPolicy {
            events: vec![FleetAlertEventClass::RestartExhausted],
            channels: vec![
                FleetAlertChannel::Slack {
                    webhook: FleetAlertEndpoint::inline("https://hooks.slack.invalid/secret-a"),
                },
                FleetAlertChannel::Slack {
                    webhook: FleetAlertEndpoint::inline("https://hooks.slack.invalid/secret-b"),
                },
            ],
            after_attempts: Some(1),
            after_minutes_stale: Some(1),
        });
        create_run(&scheduler, "run-1", vec![failing], 1);
        scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();
        scheduler.set_now(base_now() + chrono::Duration::seconds(10));

        let report = scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();

        assert_eq!(report.marked_stale, 1);
        assert_eq!(report.restarted, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.alerts, 2);
        let state = scheduler.ledger.rebuild_state().unwrap();
        assert_eq!(
            state.tasks["run-1:task-a"].status,
            FleetTaskLedgerStatus::Failed
        );
        assert_eq!(state.alerts.len(), 2);
        assert_eq!(state.escalated_events.len(), 1);
        let ledger = ledger_text(&scheduler);
        assert!(ledger.contains("\"state\":\"failed\""));
        assert!(ledger.contains("\"record\":\"alert_sent\""));
        assert_eq!(
            ledger
                .lines()
                .filter(|line| line.contains("\"record\":\"alert_sent\""))
                .count(),
            2
        );
        assert!(!ledger.contains("hooks.slack.invalid/secret"));
    }

    #[test]
    fn exhausted_failed_attempt_alerts_exactly_once_across_competing_schedulers() {
        let tmp = TempDir::new().unwrap();
        let owner = scheduler(&tmp, 1);
        let mut failing = task("task-a", 1);
        failing.alert_policy = Some(FleetAlertPolicy {
            events: vec![FleetAlertEventClass::RestartExhausted],
            channels: vec![FleetAlertChannel::Slack {
                webhook: FleetAlertEndpoint::inline("https://hooks.slack.invalid/secret"),
            }],
            after_attempts: Some(1),
            after_minutes_stale: Some(1),
        });
        create_run(&owner, "run-1", vec![failing], 1);
        owner.tick_run(&FleetRunId::from("run-1")).unwrap();
        owner
            .ledger
            .append_terminal_event_if_leased(
                &FleetRunId::from("run-1"),
                "worker-1",
                "task-a",
                1,
                &owner.timestamp(),
                FleetWorkerEventPayload::Failed {
                    reason: "worker failed before scheduler recovery".to_string(),
                    recoverable: true,
                },
            )
            .unwrap()
            .unwrap();

        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut contender = FleetScheduler::open(
                        root,
                        FleetSchedulerPolicy {
                            max_workers_per_run: 1,
                            max_workers_per_host: 1,
                            max_workers_per_task_class: 1,
                            lease_seconds: 30,
                            heartbeat_timeout: Duration::from_secs(5),
                        },
                    )
                    .unwrap();
                    contender.set_now(base_now() + chrono::Duration::seconds(10));
                    barrier.wait();
                    contender.tick_run(&FleetRunId::from("run-1")).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let reports = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(reports.iter().map(|report| report.alerts).sum::<usize>(), 1);
        assert_eq!(reports.iter().map(|report| report.failed).sum::<usize>(), 0);
        let state = owner.ledger.rebuild_state().unwrap();
        assert_eq!(state.alerts.len(), 1);
        assert_eq!(state.escalated_events.len(), 1);
        assert_eq!(
            ledger_text(&owner)
                .lines()
                .filter(|line| line.contains("\"record\":\"alert_sent\""))
                .count(),
            1
        );

        owner.ledger.compact().unwrap();
        let mut after_compaction = scheduler(&tmp, 1);
        after_compaction.set_now(base_now() + chrono::Duration::seconds(20));
        let report = after_compaction
            .tick_run(&FleetRunId::from("run-1"))
            .unwrap();
        assert_eq!(report.alerts, 0);
        assert_eq!(
            after_compaction
                .ledger
                .rebuild_state()
                .unwrap()
                .alerts
                .len(),
            1
        );
    }

    #[test]
    fn fleet_scheduler_slow_provider_response_with_fresh_heartbeat_is_not_stale() {
        let tmp = TempDir::new().unwrap();
        let mut scheduler = scheduler(&tmp, 1);
        create_run(&scheduler, "run-1", vec![task("task-a", 2)], 1);
        scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();
        scheduler.set_now(base_now() + chrono::Duration::seconds(4));
        scheduler
            .append_worker_event(
                &FleetRunId::from("run-1"),
                "worker-1",
                "task-a",
                FleetWorkerEventPayload::ModelWait {
                    model: Some("deepseek-v4-pro".to_string()),
                },
            )
            .unwrap();
        scheduler
            .ledger
            .heartbeat("worker-1", &scheduler.timestamp(), None, None)
            .unwrap();

        let report = scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();

        assert_eq!(report.marked_stale, 0);
        assert_eq!(report.restarted, 0);
        let state = scheduler.ledger.rebuild_state().unwrap();
        assert_eq!(state.tasks["run-1:task-a"].entry.attempts, 1);
        assert_eq!(state.workers["worker-1"], FleetWorkerStatus::Busy);
    }

    #[test]
    fn fleet_scheduler_cancel_run_interrupts_active_and_cancels_queued() {
        let tmp = TempDir::new().unwrap();
        let scheduler = scheduler(&tmp, 1);
        create_run(
            &scheduler,
            "run-1",
            vec![task("task-a", 3), task("task-b", 3), task("task-c", 3)],
            2,
        );
        scheduler.tick_run(&FleetRunId::from("run-1")).unwrap();

        let report = scheduler
            .cancel_run(&FleetRunId::from("run-1"), "operator")
            .unwrap();

        assert_eq!(report.cancelled, 3);
        let state = scheduler.ledger.rebuild_state().unwrap();
        for task in state.tasks.values() {
            assert_eq!(task.status, FleetTaskLedgerStatus::Cancelled);
        }
        let ledger = ledger_text(&scheduler);
        assert!(ledger.contains("\"state\":\"interrupted\""));
        assert!(ledger.contains("\"state\":\"cancelled\""));
    }
}
