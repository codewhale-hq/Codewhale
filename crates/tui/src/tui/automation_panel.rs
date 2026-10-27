//! Live scheduled-work projection for the activity band
//! (AUTOMATION-VISIBILITY-SPEC §2.1).
//!
//! One owner per fact: this projection is the single reader of "how much
//! scheduled work is live" — Active automations (`N`), runs currently
//! Queued|Running (`M`), and failed runs not yet acknowledged. The top
//! strip reads it; `background_indicator.rs` keeps owning
//! shells/tasks/agents and never learns about automations.
//!
//! The disk scan lives in `tui/ui/task_projection.rs`
//! (`refresh_automation_panel`): taken on a blocking thread, folded on the
//! same ~2.5 s cadence as the task panel. Everything here is a pure,
//! testable fold over that scan.
//!
//! The fold also detects the one transition Slice 1 can observe without an
//! engine change: a run this session watched go live and then settle.
//! Each such run is reported once (`ScanDelta::settled`) so the projection's
//! owner can post the typed `completed in background` / `failed` receipt
//! (spec §2.2) — the transcript learns about background work from the same
//! scan that lights the band, never from a second reader.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use crate::automation_manager::{
    AutomationRecord, AutomationRunRecord, AutomationRunStatus, AutomationStatus,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette::ChromeInk;

/// Band glyph for the automation slot. Composed in code (locales/AGENTS.md);
/// the ASCII-safe projection comes from `glyphs::ascii_fallback`.
const SLOT_GLYPH: &str = "⏱";

/// One scan of the durable automation store — every definition plus the
/// newest runs of each — taken off the runtime thread and handed to
/// [`AutomationPanelState::fold_scan`] on the task-panel tick.
#[derive(Debug, Default)]
pub struct AutomationScan {
    pub records: Vec<AutomationRecord>,
    pub runs: Vec<AutomationRunRecord>,
}

/// Snapshot of live scheduled work, refreshed on the task-panel cadence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutomationPanelState {
    /// Automations whose status is `Active`.
    pub active_automations: usize,
    /// Runs currently `Queued` or `Running` within the scan window.
    pub live_runs: usize,
    /// Ids of the runs behind `live_runs`, so the next fold can tell which
    /// of them settled (a settle is reported once, then forgotten).
    live_run_ids: BTreeSet<String>,
    /// Which automation owns each live run (`run_id -> automation_id`), so
    /// the next scan can re-fetch runs this session watched go live even
    /// after newer runs push them past the newest-run window.
    live_run_owners: BTreeMap<String, String>,
    /// Failed runs (`{automation_id}/{run_id}`) observed this session and not
    /// yet acknowledged by an automation-surface interaction. A failure, once
    /// seen, stays unacknowledged even after it ages out of the scan window —
    /// that is the point of the acknowledgement contract.
    unacknowledged_failures: BTreeSet<String>,
    /// Failures the operator already acknowledged. The ~2.5s fold re-sees
    /// every persisted Failed run, so without this watermark it would
    /// re-light an acknowledged failure forever. An acknowledgement lasts
    /// until the run leaves the records (automation deleted).
    acknowledged_failures: BTreeSet<String>,
}

/// How a run the session watched go live ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledOutcome {
    Completed,
    Failed,
    /// The run was canceled — by the operator, a cancel timeout or shutdown
    /// (#6162). It gets a receipt so a stopped run is never silent, but it
    /// never lights the failure demand: nothing crashed.
    Canceled,
}

/// A run this session saw live that has now settled — surfaced exactly once
/// so the owner can post its receipt (spec §2.2). Carries the raw definition
/// name and error; the receipt producer display-sanitizes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledRun {
    pub automation_id: String,
    pub automation_name: String,
    pub run_id: String,
    pub outcome: SettledOutcome,
    /// Wall-clock `started_at → ended_at` when both are recorded.
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// What one fold changed: whether the band needs a repaint, and which runs
/// settled since the previous fold.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanDelta {
    /// Anything the band paints changed (#3757: an unchanged scan must not
    /// force a redraw).
    pub changed: bool,
    pub settled: Vec<SettledRun>,
}

impl AutomationPanelState {
    /// Fold one scan into the projection.
    ///
    /// A run failure counts only when it finished at or after session start:
    /// the scheduler is host-durable, and without that gate every TUI launch
    /// would re-light the band for history the operator already saw. Settled
    /// runs are likewise gated on having been seen live by a previous fold,
    /// so the first scan of a session never replays old receipts.
    pub(crate) fn fold_scan(
        &mut self,
        records: &[AutomationRecord],
        runs: &[AutomationRunRecord],
        session_started_at: DateTime<Utc>,
    ) -> ScanDelta {
        let previous = std::mem::take(self);
        self.active_automations = records
            .iter()
            .filter(|record| matches!(record.status, AutomationStatus::Active))
            .count();
        self.live_run_ids = runs
            .iter()
            .filter(|run| {
                matches!(
                    run.status,
                    AutomationRunStatus::Queued | AutomationRunStatus::Running
                )
            })
            .map(|run| run.id.clone())
            .collect();
        self.live_run_owners = runs
            .iter()
            .filter(|run| self.live_run_ids.contains(&run.id))
            .map(|run| (run.id.clone(), run.automation_id.clone()))
            .collect();
        self.live_runs = self.live_run_ids.len();
        let names: BTreeMap<&str, &str> = records
            .iter()
            .map(|record| (record.id.as_str(), record.name.as_str()))
            .collect();
        let settled =
            runs.iter()
                .filter(|run| previous.live_run_ids.contains(&run.id))
                .filter_map(|run| {
                    let outcome = match run.status {
                        AutomationRunStatus::Completed => SettledOutcome::Completed,
                        AutomationRunStatus::Failed => SettledOutcome::Failed,
                        AutomationRunStatus::Canceled => SettledOutcome::Canceled,
                        AutomationRunStatus::Queued | AutomationRunStatus::Running => return None,
                    };
                    Some(SettledRun {
                        automation_id: run.automation_id.clone(),
                        automation_name: names
                            .get(run.automation_id.as_str())
                            .map_or_else(|| run.automation_id.clone(), |name| (*name).to_string()),
                        run_id: run.id.clone(),
                        outcome,
                        duration_ms: run.started_at.zip(run.ended_at).map(|(started, ended)| {
                            (ended - started).num_milliseconds().max(0) as u64
                        }),
                        error: run.error.clone(),
                    })
                })
                .collect();
        // Runs of deleted automations drop out; the surviving ids keep their
        // acknowledgement demand. The acknowledged watermark sheds deleted
        // automations the same way so it cannot grow unbounded across a
        // long session.
        let automation_ids: BTreeSet<&str> =
            records.iter().map(|record| record.id.as_str()).collect();
        let mut unacknowledged: BTreeSet<String> = previous
            .unacknowledged_failures
            .iter()
            .filter(|key| {
                key.split_once('/')
                    .is_some_and(|(automation_id, _)| automation_ids.contains(automation_id))
            })
            .cloned()
            .collect();
        let acknowledged: BTreeSet<String> = previous
            .acknowledged_failures
            .iter()
            .filter(|key| {
                key.split_once('/')
                    .is_some_and(|(automation_id, _)| automation_ids.contains(automation_id))
            })
            .cloned()
            .collect();
        self.acknowledged_failures = acknowledged;
        for run in runs {
            let finished_at = run.ended_at.unwrap_or(run.created_at);
            if matches!(run.status, AutomationRunStatus::Failed)
                && finished_at >= session_started_at
            {
                let key = format!("{}/{}", run.automation_id, run.id);
                if !self.acknowledged_failures.contains(&key) {
                    unacknowledged.insert(key);
                }
            }
        }
        self.unacknowledged_failures = unacknowledged;
        let changed = self.active_automations != previous.active_automations
            || self.live_runs != previous.live_runs
            || self.unacknowledged_failures != previous.unacknowledged_failures;
        ScanDelta { changed, settled }
    }

    /// The operator engaged with the automation surface (`/automation …`;
    /// the Slice-2 panel hooks the same call): every failure observed so far
    /// is acknowledged and the band ink settles. The acknowledgement rides
    /// the watermark — the next fold re-sees the same persisted Failed runs
    /// and must not re-light them.
    pub fn acknowledge_failures(&mut self) {
        self.acknowledged_failures
            .extend(self.unacknowledged_failures.iter().cloned());
        self.unacknowledged_failures.clear();
    }

    #[must_use]
    pub fn has_unacknowledged_failure(&self) -> bool {
        !self.unacknowledged_failures.is_empty()
    }

    /// Live runs this session is watching (`run_id -> automation_id`), so
    /// the next scan can re-fetch them even after newer runs push them
    /// past the newest-run window.
    pub(crate) fn live_run_owners(&self) -> BTreeMap<String, String> {
        self.live_run_owners.clone()
    }

    /// Activity-band slot text: `⏱ N scheduled` while any automation is
    /// Active, plus `· M running` while runs are Queued|Running. Pausing an
    /// automation does not cancel its already-enqueued runs, so the slot
    /// stays up while either count is nonzero and vanishes only at zero —
    /// it never becomes permanent furniture (spec §7.2).
    #[must_use]
    pub fn activity_slot(&self, locale: Locale) -> Option<String> {
        if self.active_automations == 0 && self.live_runs == 0 {
            return None;
        }
        if self.active_automations == 0 {
            // Runs outlive their paused automation: show the live work,
            // not the (zero) scheduled count.
            return Some(format!(
                "{SLOT_GLYPH} {} {}",
                self.live_runs,
                tr(locale, MessageId::AutomationRunStatusRunning)
            ));
        }
        let mut slot = format!(
            "{SLOT_GLYPH} {} {}",
            self.active_automations,
            tr(locale, MessageId::AutomationBandScheduled)
        );
        if self.live_runs > 0 {
            slot.push_str(&format!(
                " · {} {}",
                self.live_runs,
                tr(locale, MessageId::AutomationRunStatusRunning)
            ));
        }
        Some(slot)
    }

    /// Compact-tier form of the same fact: `⏱ 2·1` (scheduled·running).
    /// Chrome sheds before content on narrow terminals, so the live-work
    /// count abbreviates instead of vanishing; still zero-suppressed on
    /// both counts.
    #[must_use]
    pub fn activity_slot_compact(&self) -> Option<String> {
        if self.active_automations == 0 && self.live_runs == 0 {
            return None;
        }
        Some(if self.live_runs > 0 {
            if self.active_automations == 0 {
                format!("{SLOT_GLYPH} ·{}", self.live_runs)
            } else {
                format!(
                    "{SLOT_GLYPH} {}·{}",
                    self.active_automations, self.live_runs
                )
            }
        } else {
            format!("{SLOT_GLYPH} {}", self.active_automations)
        })
    }

    /// Slot ink, the grammar table's existing Goal-chip rule: `Info` idle,
    /// `Active` while a run is live, `Attention` when a run failed since last
    /// acknowledgement. Never `Failure` — a failed report job is not a
    /// product failure.
    #[must_use]
    pub fn activity_ink(&self) -> ChromeInk {
        if self.has_unacknowledged_failure() {
            ChromeInk::Attention
        } else if self.live_runs > 0 {
            ChromeInk::Active
        } else {
            ChromeInk::Info
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, status: AutomationStatus) -> AutomationRecord {
        let now = Utc::now();
        AutomationRecord {
            destination_thread_id: None,
            schema_version: 1,
            execution_scope: Some(crate::task_manager::test_execution_scope("test")),
            id: id.to_string(),
            name: id.to_string(),
            prompt: "prompt".to_string(),
            rrule: "FREQ=DAILY".to_string(),
            cwds: Vec::new(),
            model: None,
            model_provider: None,
            model_provider_id: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            delivery_mode: None,
            status,
            created_at: now,
            updated_at: now,
            next_run_at: None,
            last_run_at: None,
            revision: 0,
        }
    }

    fn run(
        automation_id: &str,
        id: &str,
        status: AutomationRunStatus,
        ended_at: Option<DateTime<Utc>>,
    ) -> AutomationRunRecord {
        let now = Utc::now();
        AutomationRunRecord {
            schema_version: 1,
            id: id.to_string(),
            automation_id: automation_id.to_string(),
            scheduled_for: now,
            status,
            created_at: now,
            started_at: Some(now),
            ended_at,
            task_id: None,
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        }
    }

    #[test]
    fn activity_slot_counts_and_zero_suppression() {
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        assert_eq!(panel.activity_slot(Locale::En), None, "zero-suppressed");

        panel.fold_scan(
            &[
                record("a1", AutomationStatus::Active),
                record("a2", AutomationStatus::Active),
                record("a3", AutomationStatus::Paused),
            ],
            &[
                run("a1", "r1", AutomationRunStatus::Running, None),
                run("a2", "r2", AutomationRunStatus::Completed, Some(Utc::now())),
            ],
            session_started_at,
        );
        assert_eq!(panel.active_automations, 2, "paused automation excluded");
        assert_eq!(panel.live_runs, 1, "completed run excluded");
        assert_eq!(
            panel.activity_slot(Locale::En).as_deref(),
            Some("⏱ 2 scheduled · 1 running")
        );
        assert_eq!(panel.activity_ink(), ChromeInk::Active);

        panel.fold_scan(
            &[
                record("a1", AutomationStatus::Active),
                record("a2", AutomationStatus::Active),
            ],
            &[],
            session_started_at,
        );
        assert_eq!(
            panel.activity_slot(Locale::En).as_deref(),
            Some("⏱ 2 scheduled"),
            "no live runs: the running clause drops"
        );
        assert_eq!(panel.activity_ink(), ChromeInk::Info);
    }

    #[test]
    fn failure_ink_holds_until_acknowledged_and_survives_the_scan_window() {
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[run(
                "a1",
                "r1",
                AutomationRunStatus::Failed,
                Some(Utc::now()),
            )],
            session_started_at,
        );
        assert!(panel.has_unacknowledged_failure());
        assert_eq!(panel.activity_ink(), ChromeInk::Attention);

        // The failed run ages out of the scan window: the acknowledgement
        // demand survives — a capped scan must not quietly settle the band.
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[],
            session_started_at,
        );
        assert_eq!(panel.activity_ink(), ChromeInk::Attention);

        panel.acknowledge_failures();
        assert_eq!(panel.activity_ink(), ChromeInk::Info);

        // The next fold re-sees the same persisted Failed run and must not
        // re-light the acknowledged failure.
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[run(
                "a1",
                "r1",
                AutomationRunStatus::Failed,
                Some(Utc::now()),
            )],
            session_started_at,
        );
        assert!(
            !panel.has_unacknowledged_failure(),
            "an acknowledged failure must not re-light on the next scan"
        );
        assert_eq!(panel.activity_ink(), ChromeInk::Info);

        // A NEW failed run is still a fresh acknowledgement demand.
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[run(
                "a1",
                "r2",
                AutomationRunStatus::Failed,
                Some(Utc::now()),
            )],
            session_started_at,
        );
        assert!(panel.has_unacknowledged_failure());
    }

    #[test]
    fn live_runs_stay_visible_when_their_automation_is_paused() {
        // Pausing prevents future scheduling; it does not cancel the
        // already-enqueued run. The projection must stay up while either
        // count is nonzero.
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        panel.fold_scan(
            &[record("a1", AutomationStatus::Paused)],
            &[run("a1", "r1", AutomationRunStatus::Running, None)],
            session_started_at,
        );
        assert_eq!(panel.active_automations, 0);
        assert_eq!(panel.live_runs, 1);
        assert_eq!(
            panel.activity_slot(Locale::En).as_deref(),
            Some("⏱ 1 running"),
            "a paused automation's live run stays visible"
        );
        assert_eq!(panel.activity_slot_compact().as_deref(), Some("⏱ ·1"));
        assert_eq!(panel.activity_ink(), ChromeInk::Active);

        // Both counts at zero: the slot vanishes.
        panel.fold_scan(
            &[record("a1", AutomationStatus::Paused)],
            &[],
            session_started_at,
        );
        assert_eq!(panel.activity_slot(Locale::En), None);
        assert_eq!(panel.activity_slot_compact(), None);
    }

    #[test]
    fn pre_session_failures_do_not_light_the_band() {
        let session_started_at = Utc::now();
        let old = session_started_at - chrono::Duration::hours(2);
        let mut stale_run = run("a1", "r0", AutomationRunStatus::Failed, Some(old));
        stale_run.created_at = old;
        let mut panel = AutomationPanelState::default();
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[stale_run],
            session_started_at,
        );
        assert!(!panel.has_unacknowledged_failure());
        assert_eq!(panel.activity_ink(), ChromeInk::Info);
    }

    #[test]
    fn failures_of_deleted_automations_drop_out() {
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        panel.fold_scan(
            &[record("a1", AutomationStatus::Active)],
            &[run(
                "a1",
                "r1",
                AutomationRunStatus::Failed,
                Some(Utc::now()),
            )],
            session_started_at,
        );
        assert!(panel.has_unacknowledged_failure());
        panel.fold_scan(&[], &[], session_started_at);
        assert!(
            !panel.has_unacknowledged_failure(),
            "deleting the automation clears its failure demand"
        );
    }

    #[test]
    fn fold_scan_reports_only_visible_change() {
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        assert!(
            panel
                .fold_scan(
                    &[record("a1", AutomationStatus::Active)],
                    &[],
                    session_started_at
                )
                .changed
        );
        assert!(
            !panel
                .fold_scan(
                    &[record("a1", AutomationStatus::Active)],
                    &[],
                    session_started_at
                )
                .changed,
            "an unchanged scan must not force a redraw"
        );
    }

    /// A run the session watched go live settles exactly once, carrying the
    /// definition name and its wall-clock duration; runs that were already
    /// finished before the session saw them never replay as receipts.
    #[test]
    fn runs_watched_live_settle_once_with_name_and_duration() {
        let session_started_at = Utc::now();
        let started = Utc::now();
        let mut panel = AutomationPanelState::default();
        let mut docs = record("a1", AutomationStatus::Active);
        docs.name = "Documentation".to_string();
        let mut finished_earlier = run("a1", "r0", AutomationRunStatus::Completed, Some(started));
        finished_earlier.started_at = Some(started);

        // First scan: one live run, one already-finished run. Nothing settles
        // — the finished run was never seen live.
        let mut live = run("a1", "r1", AutomationRunStatus::Running, None);
        live.started_at = Some(started);
        let delta = panel.fold_scan(
            std::slice::from_ref(&docs),
            &[live, finished_earlier.clone()],
            session_started_at,
        );
        assert!(delta.changed);
        assert!(delta.settled.is_empty(), "{:?}", delta.settled);
        assert_eq!(panel.live_runs, 1);

        // Second scan: the live run completed 42 s later.
        let mut done = run(
            "a1",
            "r1",
            AutomationRunStatus::Completed,
            Some(started + chrono::Duration::seconds(42)),
        );
        done.started_at = Some(started);
        let delta = panel.fold_scan(
            std::slice::from_ref(&docs),
            &[done.clone(), finished_earlier.clone()],
            session_started_at,
        );
        assert_eq!(
            delta.settled,
            vec![SettledRun {
                automation_id: "a1".to_string(),
                automation_name: "Documentation".to_string(),
                run_id: "r1".to_string(),
                outcome: SettledOutcome::Completed,
                duration_ms: Some(42_000),
                error: None,
            }]
        );
        assert_eq!(panel.live_runs, 0);
        assert_eq!(panel.activity_ink(), ChromeInk::Info);

        // Third scan, same picture: the settle is not reported twice.
        let delta = panel.fold_scan(
            std::slice::from_ref(&docs),
            &[done, finished_earlier],
            session_started_at,
        );
        assert!(!delta.changed);
        assert!(delta.settled.is_empty(), "{:?}", delta.settled);
    }

    /// A watched run that fails settles as `Failed` with its error, and the
    /// same fold lights the band's acknowledgement demand.
    #[test]
    fn a_watched_run_that_fails_settles_with_its_error_and_lights_the_band() {
        let session_started_at = Utc::now();
        let mut panel = AutomationPanelState::default();
        let records = [record("a1", AutomationStatus::Active)];
        panel.fold_scan(
            &records,
            &[run("a1", "r1", AutomationRunStatus::Running, None)],
            session_started_at,
        );
        let mut failed = run("a1", "r1", AutomationRunStatus::Failed, Some(Utc::now()));
        failed.error = Some("provider timeout".to_string());
        let delta = panel.fold_scan(&records, &[failed], session_started_at);
        assert_eq!(delta.settled.len(), 1);
        assert_eq!(delta.settled[0].outcome, SettledOutcome::Failed);
        assert_eq!(delta.settled[0].error.as_deref(), Some("provider timeout"));
        assert_eq!(panel.activity_ink(), ChromeInk::Attention);

        // A canceled run leaves the live set with a receipt that names the
        // cancellation (#6162), without lighting the failure demand.
        panel.acknowledge_failures();
        assert!(!panel.has_unacknowledged_failure());
        panel.fold_scan(
            &records,
            &[run("a1", "r2", AutomationRunStatus::Queued, None)],
            session_started_at,
        );
        let mut canceled = run("a1", "r2", AutomationRunStatus::Canceled, Some(Utc::now()));
        canceled.error = Some("canceled by request".to_string());
        let delta = panel.fold_scan(&records, &[canceled.clone()], session_started_at);
        assert_eq!(delta.settled.len(), 1, "{:?}", delta.settled);
        assert_eq!(delta.settled[0].outcome, SettledOutcome::Canceled);
        assert_eq!(delta.settled[0].run_id, "r2");
        assert_eq!(
            delta.settled[0].error.as_deref(),
            Some("canceled by request")
        );
        assert!(
            !panel.has_unacknowledged_failure(),
            "a cancellation is not a failure"
        );

        // The receipt is posted once: the same picture settles nothing more.
        let delta = panel.fold_scan(&records, &[canceled], session_started_at);
        assert!(delta.settled.is_empty(), "{:?}", delta.settled);
    }

    /// The spec's reservation check (§6 Slice 1 accept): no automation band
    /// ink may resolve to the theme's failure color in any selectable preset.
    #[test]
    fn automation_band_ink_never_resolves_to_failure_red() {
        for theme_id in codewhale_palette::SELECTABLE_THEMES {
            let theme = theme_id.ui_theme();
            for ink in [ChromeInk::Info, ChromeInk::Active, ChromeInk::Attention] {
                assert_ne!(
                    ink.color(&theme),
                    theme.error_fg,
                    "theme '{}' spends Failure red on the automation band ({ink:?})",
                    theme_id.name()
                );
            }
        }
    }
}
