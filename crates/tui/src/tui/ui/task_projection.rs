//! Task-panel and shell projection: task-panel refresh, shell live-output
//! reconciliation, detached-job projection, and RLM task entries
//! (TUI_MODULARIZATION.md slice 5). Pure projection — no dispatch here.

use super::*;
use crate::tui::automation_panel::AutomationScan;
use crate::tui::background_finished::{FinishedOutcome, FinishedWork, MAX_FINISHED_SHELLS};
use codewhale_localization::{Locale, MessageId, tr};

pub(super) async fn refresh_active_task_panel(
    app: &mut App,
    task_manager: &SharedTaskManager,
) -> bool {
    let namespace_changed = app.task_panel_session_id != app.current_session_id;
    if namespace_changed {
        app.task_panel.clear();
        app.task_panel_session_id = app.current_session_id.clone();
        app.task_panel_unavailable = false;
    }
    let tasks = match app.current_session_id.as_deref() {
        Some(session_id) => match task_manager
            .list_tasks_for_owner(None, None, session_id)
            .await
        {
            Ok(tasks) => tasks,
            Err(error) => {
                let changed = namespace_changed || !app.task_panel_unavailable;
                if !app.task_panel_unavailable {
                    app.push_status_toast(
                        codewhale_localization::tr(
                            app.ui_locale,
                            codewhale_localization::MessageId::TaskInventoryUnavailable,
                        )
                        .to_string(),
                        crate::tui::app::StatusToastLevel::Warning,
                        Some(8_000),
                    );
                    tracing::warn!(%error, "Task inventory unavailable; preserving scoped snapshot");
                }
                app.task_panel_unavailable = true;
                return changed;
            }
        },
        None => Vec::new(),
    };
    let was_unavailable = std::mem::replace(&mut app.task_panel_unavailable, false);
    // #6565: a durable task that failed or was cancelled is as much news as
    // one that completed; each lands in the batched notice by its summary.
    let newly_finished_tasks = tasks
        .iter()
        .filter_map(|task| {
            let (outcome, word) = match task.status {
                TaskStatus::Completed => (FinishedOutcome::Done, MessageId::BackgroundOutcomeDone),
                TaskStatus::Failed => (FinishedOutcome::Failed, MessageId::BackgroundOutcomeFailed),
                TaskStatus::Canceled => (
                    FinishedOutcome::Stopped,
                    MessageId::BackgroundOutcomeCancelled,
                ),
                TaskStatus::Queued | TaskStatus::Running => return None,
            };
            // Restored history is not a new completion. A task that finishes
            // between polls still counts, without ever needing a live row.
            if !task.ended_at.is_some_and(|at| at >= app.session_started_at)
                || !app.notified_task_ids.insert(task.id.clone())
            {
                return None;
            }
            let word = tr(app.ui_locale, word);
            let summary = match task.duration_ms {
                Some(ms) => format!("{word} · {}", crate::agent_roster::format_duration(ms)),
                None => word.to_string(),
            };
            let summary = match task.error.as_deref().map(str::trim) {
                Some(error) if !error.is_empty() && outcome != FinishedOutcome::Done => {
                    format!("{summary} · {}", bound_agent_activity_text(error))
                }
                _ => summary,
            };
            Some(
                FinishedWork::task(
                    &task.prompt_summary,
                    outcome,
                    summary,
                    std::time::Duration::from_millis(task.duration_ms.unwrap_or_default()),
                )
                .in_turn(app.is_loading),
            )
        })
        .collect::<Vec<_>>();
    let durable_background_completed = newly_finished_tasks
        .iter()
        .any(|task| task.outcome == FinishedOutcome::Done);
    app.background_finished.extend(newly_finished_tasks);
    let mut lifecycle_changed = false;
    if let (Some(work), Some(session_id)) = (
        app.runtime_services.work.as_ref(),
        app.current_session_id.as_deref(),
    ) {
        for task in &tasks {
            if !task.execution_binding_known {
                continue;
            }
            let external = format!("task:{}", task.id);
            if !work.has_operation_binding(Some(session_id), &external) {
                continue;
            }
            match work.reconcile_operation(
                session_id,
                task_owner_snapshot(
                    &task.id,
                    task.status,
                    task.lifecycle_seq,
                    task.created_at,
                    task.started_at,
                    task.ended_at,
                ),
            ) {
                Ok(changed) => lifecycle_changed |= changed,
                Err(err) => {
                    tracing::warn!(task_id = %task.id, error = %err, "failed to reconcile durable task lifecycle");
                }
            }
        }
    }
    if lifecycle_changed && let Err(err) = persist_pending_work_checkpoint(app).await {
        tracing::warn!(error = %err, "durable task lifecycle checkpoint remains pending");
    }
    let session_started_at = app.session_started_at;
    let mut entries: Vec<TaskPanelEntry> =
        select_work_sidebar_tasks(tasks, session_started_at, app.current_session_id.as_deref())
            .into_iter()
            .map(|summary| {
                let unverified = !summary.execution_binding_known
                    && matches!(summary.status, TaskStatus::Queued | TaskStatus::Running);
                let mut entry = task_summary_to_panel_entry(summary);
                if unverified {
                    entry.stale = true;
                    entry.role = Some(
                        codewhale_localization::tr(
                            app.ui_locale,
                            codewhale_localization::MessageId::TaskOwnershipUnverified,
                        )
                        .to_string(),
                    );
                }
                entry
            })
            .collect();

    entries.extend(active_rlm_task_entries(app));

    // #3804: this is a render-only read of shell jobs and must not block the
    // async UI loop on the shell manager's std::sync Mutex. Use try_lock; on
    // contention, fail closed (no shell rows this frame) rather than show a
    // snapshot that may belong to a replaced session. Shell ownership,
    // cancellation, approval state, and output capture never depend on this
    // refresh succeeding.
    let jobs = match app.runtime_services.shell_manager.as_ref() {
        Some(shell_mgr) => match shell_mgr.try_lock() {
            Ok(mut mgr) => Some(
                mgr.list_jobs_for_session(app.current_session_id.as_deref().unwrap_or_default()),
            ),
            Err(_) => None,
        },
        None => None,
    };
    let jobs = jobs.unwrap_or_default();
    let shell_background_completed = project_shell_jobs(app, &mut entries, &jobs);

    // Report whether anything visible changed so the idle tick can skip the
    // redraw: an unconditional 2.5 s repaint kept the app from ever going
    // quiescent (#3757).
    let changed = namespace_changed
        || was_unavailable
        || lifecycle_changed
        || shell_background_completed
        || app.task_panel != entries;
    app.task_panel = entries;
    let tip_shown = (durable_background_completed || shell_background_completed)
        && app.maybe_show_behavioral_tip(
            crate::tui::behavioral_tips::BehavioralTip::BackgroundJobReceipt,
        );
    changed || tip_shown
}

/// Fold one session-scoped snapshot. An empty snapshot may be a lock miss;
/// neither completion deduplication nor retained IDs depend on visible rows.
pub(super) fn project_shell_jobs(
    app: &mut App,
    entries: &mut Vec<TaskPanelEntry>,
    jobs: &[crate::tools::shell::ShellJobSnapshot],
) -> bool {
    // #6565: every terminal status is a completion a person should hear
    // about (a failed, killed or timed-out shell used to vanish silently),
    // and a finished shell stays listed, muted, with how it ended.
    let finished_now = newly_terminal(&mut app.notified_shell_ids, jobs, app.session_started_at);
    for job in &finished_now {
        let retained = app
            .finished_shell_ids
            .entry(app.current_session_id.clone().unwrap_or_default())
            .or_default();
        retained.push_back(job.id.clone());
        while retained.len() > MAX_FINISHED_SHELLS {
            retained.pop_front();
        }
        let (outcome, summary) = shell_outcome(app.ui_locale, job);
        let toast = format!("shell · {} · {summary}", job.command.trim());
        let level = if outcome == FinishedOutcome::Done {
            crate::tui::app::StatusToastLevel::Info
        } else {
            crate::tui::app::StatusToastLevel::Warning
        };
        app.push_status_toast(bound_agent_activity_text(&toast), level, Some(6_000));
        let parent_busy = app.is_loading;
        app.background_finished.push(
            FinishedWork::shell(
                &job.command,
                outcome,
                summary,
                std::time::Duration::from_millis(job.elapsed_ms),
            )
            .in_turn(parent_busy),
        );
    }
    let finished_order = app
        .finished_shell_ids
        .get(app.current_session_id.as_deref().unwrap_or_default())
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let shell_entry = |job: &crate::tools::shell::ShellJobSnapshot| TaskPanelEntry {
        id: job.id.clone(),
        status: shell_status_token(&job.status).to_string(),
        prompt_summary: format!("shell: {}", job.command),
        duration_ms: Some(job.elapsed_ms),
        kind: TaskPanelEntryKind::Shell,
        stale: job.stale,
        elapsed_since_output_ms: job.elapsed_since_output_ms,
        owner_agent_id: job.owner_agent_id.clone(),
        owner_agent_name: job.owner_agent_name.clone(),
        current_tool: None,
        role: None,
        files_touched: 0,
        exit_code: job.exit_code,
    };
    // Only shells that really run in the background are jobs. A foreground
    // Bash wait is already the live tool card in the transcript; listing it
    // here too counted one command as a `1 shell` footer chip, a `Shells 1`
    // row and the tool card at once. A wait that outlives its foreground
    // budget (or Ctrl+B) flips `background`, and the shell then appears here.
    entries.extend(
        jobs.iter()
            .filter(|job| {
                job.background && matches!(job.status, crate::tools::shell::ShellStatus::Running)
            })
            .map(shell_entry),
    );
    // Finished shells in the order they finished, so the list is stable and
    // an idle tick with nothing new changes nothing (#3757).
    entries.extend(
        finished_order
            .iter()
            .filter_map(|id| jobs.iter().find(|job| &job.id == id))
            .filter(|job| {
                job.background && !matches!(job.status, crate::tools::shell::ShellStatus::Running)
            })
            .map(shell_entry),
    );
    !finished_now.is_empty()
}

/// The wire token a shell status shows as in the task panel.
pub(crate) fn shell_status_token(status: &crate::tools::shell::ShellStatus) -> &'static str {
    use crate::tools::shell::ShellStatus;
    match status {
        ShellStatus::Running => "running",
        ShellStatus::Completed => "completed",
        ShellStatus::Failed => "failed",
        ShellStatus::Killed => "killed",
        ShellStatus::TimedOut => "timed_out",
    }
}

/// How a finished shell ended, in the words its row and notice use:
/// `exit 0 · 12s`, `failed · exit 2`, `killed`, `timed out`.
pub(crate) fn shell_outcome(
    locale: Locale,
    job: &crate::tools::shell::ShellJobSnapshot,
) -> (FinishedOutcome, String) {
    use crate::tools::shell::ShellStatus;
    let exit = job
        .exit_code
        .map(|code| tr(locale, MessageId::BackgroundExitCode).replace("{code}", &code.to_string()));
    let took = crate::agent_roster::format_duration(job.elapsed_ms);
    match job.status {
        ShellStatus::Completed | ShellStatus::Running => (
            FinishedOutcome::Done,
            format!(
                "{} · {took}",
                exit.unwrap_or_else(|| tr(locale, MessageId::BackgroundOutcomeDone).into_owned())
            ),
        ),
        ShellStatus::Failed => (
            FinishedOutcome::Failed,
            match exit {
                Some(exit) => format!(
                    "{} · {exit}",
                    tr(locale, MessageId::BackgroundOutcomeFailed)
                ),
                None => tr(locale, MessageId::BackgroundOutcomeFailed).into_owned(),
            },
        ),
        ShellStatus::Killed => (
            FinishedOutcome::Stopped,
            tr(locale, MessageId::BackgroundOutcomeKilled).into_owned(),
        ),
        ShellStatus::TimedOut => (
            FinishedOutcome::Stopped,
            tr(locale, MessageId::BackgroundOutcomeTimedOut).into_owned(),
        ),
    }
}

/// Every fresh background completion once, even when never observed running.
/// A missed lock, session switch or evicted row never resets this ledger.
/// The session-start cutoff suppresses restored history after a TUI restart.
/// Finish time is the manager's first terminal observation, not OS exit time;
/// legacy snapshots without it are history, never new completion receipts.
pub(super) fn newly_terminal<'a>(
    notified_ids: &mut HashSet<String>,
    jobs: &'a [crate::tools::shell::ShellJobSnapshot],
    session_started_at: chrono::DateTime<chrono::Utc>,
) -> Vec<&'a crate::tools::shell::ShellJobSnapshot> {
    jobs.iter()
        .filter(|job| job.background && job.finished_at.is_some_and(|at| at >= session_started_at))
        .filter(|job| !matches!(job.status, crate::tools::shell::ShellStatus::Running))
        .filter(|job| notified_ids.insert(job.id.clone()))
        .collect()
}

/// Newest runs scanned per automation when refreshing the automation
/// projection. Live runs sit at the head of the newest-first listing, and a
/// failure once seen is held unacknowledged by the projection until the
/// operator engages the automation surface, so the band never needs a full
/// run-history scan on the render cadence.
const AUTOMATION_PANEL_RUN_SCAN: usize = 25;

/// Refresh the activity band's scheduled-work projection
/// (AUTOMATION-VISIBILITY-SPEC §2.1) from the durable automation store.
///
/// The store is files on disk — every definition plus up to
/// `AUTOMATION_PANEL_RUN_SCAN` run files per definition — so the scan never
/// runs on the async UI loop: it is taken on a blocking thread, and this
/// tick folds whatever scan has finished, then starts the next one. At most
/// one scan is in flight; a slow disk costs the band latency, never the
/// frame. Returns whether the visible state changed, so the idle tick can
/// skip the redraw (#3757).
///
/// `start_next` says whether the next scan may begin once a finished one is
/// folded. The caller passes [`automation_scan_is_due`], so a quiet session
/// with nothing scheduled does not re-read the store every 2.5 s (#6728); a
/// scan already in flight is still folded on every tick.
pub(super) async fn refresh_automation_panel(app: &mut App, start_next: bool) -> bool {
    let mut changed = false;
    if let Some(scan) = app.automation_scan.take() {
        if scan.is_finished() {
            match scan.await {
                Ok(scan) => changed = fold_automation_scan(app, &scan),
                Err(err) => {
                    tracing::warn!(error = %err, "automation panel scan task failed");
                }
            }
        } else {
            app.automation_scan = Some(scan);
            return false;
        }
    }
    if start_next {
        app.automation_scan = start_automation_scan(app, false);
    }
    changed
}

/// The 2.5 s cadence the activity band is scanned at whenever it has
/// something to show or the user is around.
pub(super) const AUTOMATION_SCAN_BUSY_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(2_500);

/// The cadence for a quiet session whose band shows nothing: a new
/// automation created from outside this process lights the band within this
/// long. Anything created from inside it (a command, a tool call, a click) is
/// input or an engine event, which restores the busy cadence at once.
pub(super) const AUTOMATION_SCAN_QUIET_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(15);

/// How long to wait between automation scans. Pure: busy unless the UI is
/// quiet *and* the band has nothing to track *and* no automation surface is
/// open. A scan already in flight does not matter here: it is folded on every
/// 2.5 s tick whatever this says, and only the *start* of the next one waits.
pub(super) fn automation_scan_interval(app: &App, ui_quiet: bool) -> std::time::Duration {
    let panel = &app.automation_panel;
    let band_tracks_something = panel.active_automations > 0
        || panel.live_runs > 0
        || !panel.live_run_owners().is_empty()
        || panel.has_unacknowledged_failure();
    let surface_open = app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::Automations);
    if ui_quiet && !band_tracks_something && !surface_open {
        AUTOMATION_SCAN_QUIET_INTERVAL
    } else {
        AUTOMATION_SCAN_BUSY_INTERVAL
    }
}

/// Whether the next automation scan may start, `since_last_scan` after the
/// previous one.
pub(super) fn automation_scan_is_due(
    app: &App,
    ui_quiet: bool,
    since_last_scan: std::time::Duration,
) -> bool {
    since_last_scan >= automation_scan_interval(app, ui_quiet)
}

/// Startup variant: take one scan and wait for it, so the first frame
/// already carries the band count (the task panel gets the same courtesy).
/// The startup pass is FULL — every run file — so a long-running task that
/// already sits behind more than a window of newer runs is visible from the
/// first frame. The manager lock is retried briefly: the scheduler tick
/// holds it only while persisting, and giving up here would blank the band
/// on a contended startup.
pub(super) async fn refresh_automation_panel_blocking(app: &mut App) -> bool {
    let scan = 'retry: {
        for _ in 0..STARTUP_SCAN_LOCK_RETRIES {
            if let Some(scan) = start_automation_scan(app, true) {
                break 'retry Some(scan);
            }
            tokio::time::sleep(STARTUP_SCAN_LOCK_RETRY_DELAY).await;
        }
        None
    };
    let Some(scan) = scan else {
        return false;
    };
    match scan.await {
        Ok(scan) => fold_automation_scan(app, &scan),
        Err(err) => {
            tracing::warn!(error = %err, "automation panel scan task failed");
            false
        }
    }
}

/// Startup lock-retry budget: the scheduler's persist phase is short; ten
/// 50 ms attempts covers it without parking startup on a stuck lock.
const STARTUP_SCAN_LOCK_RETRIES: usize = 10;
const STARTUP_SCAN_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Start one store scan on a blocking thread. The manager is cloned out
/// from under its tokio Mutex (`try_lock`, same rule as the shell snapshot:
/// the scheduler tick holds that lock while persisting, and the UI loop
/// must not park behind it; on contention the next tick retries) so the
/// scan holds no lock while it reads — the store's writes are atomic
/// renames, so a concurrent read sees a whole file either way.
///
/// `full` reads every run file (startup only). The cadence scan reads the
/// newest `AUTOMATION_PANEL_RUN_SCAN` runs per automation PLUS the runs
/// this session already watched go live, wherever they sit in history — a
/// frequent automation can stack newer runs behind a long-running task,
/// and neither the live count nor the settle receipt may depend on the
/// task staying inside the newest window.
fn start_automation_scan(app: &App, full: bool) -> Option<tokio::task::JoinHandle<AutomationScan>> {
    let automations = app.runtime_services.automations.as_ref()?;
    let manager = automations.try_lock().ok()?.clone();
    let live_owners = app.automation_panel.live_run_owners();
    Some(tokio::task::spawn_blocking(move || {
        let records = match manager.list_automations() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "automation panel refresh could not list automations");
                return AutomationScan::default();
            }
        };
        let mut runs = Vec::new();
        for record in &records {
            let limit = if full {
                None
            } else {
                Some(AUTOMATION_PANEL_RUN_SCAN)
            };
            match manager.list_runs(&record.id, limit) {
                Ok(recent) => runs.extend(recent),
                Err(err) => {
                    tracing::warn!(automation_id = %record.id, error = %err, "automation panel refresh could not list runs");
                }
            }
            if !full {
                let wanted: std::collections::BTreeSet<String> = live_owners
                    .iter()
                    .filter(|(_, owner)| owner.as_str() == record.id.as_str())
                    .map(|(run_id, _)| run_id.clone())
                    .collect();
                if !wanted.is_empty() {
                    match manager.get_runs_by_ids(&record.id, &wanted) {
                        Ok(found) => runs.extend(found),
                        Err(err) => {
                            tracing::warn!(automation_id = %record.id, error = %err, "automation panel refresh could not re-read live runs");
                        }
                    }
                }
            }
        }
        // A re-read live run may also sit inside the newest window; the
        // fold counts by id, so dedupe before handing the scan over.
        let mut seen = std::collections::BTreeSet::new();
        runs.retain(|run| seen.insert(run.id.clone()));
        AutomationScan { records, runs }
    }))
}

/// Fold a finished scan into the projection and post the typed receipt
/// (spec §2.2) for every run this session watched go live and settle — the
/// transcript learns about background work from the same scan that
/// repaints the band.
fn fold_automation_scan(app: &mut App, scan: &AutomationScan) -> bool {
    let session_started_at = app.session_started_at;
    let delta = app
        .automation_panel
        .fold_scan(&scan.records, &scan.runs, session_started_at);
    let locale = app.ui_locale;
    for run in &delta.settled {
        app.add_message(crate::tui::automation_routing::settled_run_receipt(
            locale, run,
        ));
    }
    delta.changed || !delta.settled.is_empty()
}

pub(super) fn refresh_shell_exec_live_output(app: &mut App) -> bool {
    let Some(shell_mgr) = app.runtime_services.shell_manager.as_ref().cloned() else {
        return false;
    };
    // #3804: render-only read — try_lock so a contended shell Mutex can never
    // block the async UI loop; skip this frame's live-output update on
    // contention (the next refresh picks it up).
    let jobs = {
        let Ok(mut mgr) = shell_mgr.try_lock() else {
            return false;
        };
        mgr.list_jobs_for_session(app.current_session_id.as_deref().unwrap_or_default())
            .into_iter()
            .map(|job| (job.id.clone(), job))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let mut changed = false;
    for index in 0..app.virtual_cell_count() {
        let Some(ShellExecLiveUpdate {
            task_id,
            status: next_status,
            output: next_live,
            duration_ms: next_duration,
            finalized,
            stale_elapsed_since_output_ms,
        }) = shell_exec_live_update(app, index, &jobs)
        else {
            continue;
        };
        let Some(HistoryCell::Tool(ToolCell::Exec(exec))) = app.cell_at_virtual_index_mut(index)
        else {
            continue;
        };
        if exec.output.is_some() || exec.shell_task_id.as_deref() != Some(task_id.as_str()) {
            continue;
        }
        exec.status = next_status;
        exec.duration_ms = Some(next_duration);
        exec.stale_elapsed_since_output_ms = stale_elapsed_since_output_ms;
        if finalized {
            exec.output = next_live;
            exec.output_summary = exec
                .output
                .as_deref()
                .map(crate::tui::history::summarize_tool_output);
            exec.live_output = None;
            exec.stale_elapsed_since_output_ms = None;
        } else {
            exec.live_output = next_live;
        }
        changed = true;
    }
    changed
}

pub(super) struct ShellExecLiveUpdate {
    pub(super) task_id: String,
    pub(super) status: ToolStatus,
    pub(super) output: Option<String>,
    pub(super) duration_ms: u64,
    pub(super) finalized: bool,
    pub(super) stale_elapsed_since_output_ms: Option<u64>,
}

pub(super) fn shell_exec_live_update(
    app: &App,
    index: usize,
    jobs: &std::collections::HashMap<String, ShellJobSnapshot>,
) -> Option<ShellExecLiveUpdate> {
    let HistoryCell::Tool(ToolCell::Exec(exec)) = app.cell_at_virtual_index(index)? else {
        return None;
    };
    if exec.output.is_some() {
        return None;
    }
    let task_id = exec.shell_task_id.as_deref()?;
    let Some(job) = jobs.get(task_id) else {
        return Some(ShellExecLiveUpdate {
            task_id: task_id.to_string(),
            status: ToolStatus::Failed,
            output: detached_shell_job_output(task_id, exec),
            duration_ms: exec.duration_ms.unwrap_or_default(),
            finalized: true,
            stale_elapsed_since_output_ms: None,
        });
    };
    let next_status = shell_job_tool_status(&job.status);
    let next_live = shell_job_live_output(job).or_else(|| exec.live_output.clone());
    let finalized = !matches!(job.status, ShellStatus::Running);
    let quiet_after_ms =
        u64::try_from(crate::tools::shell::QUIET_NO_OUTPUT_AFTER.as_millis()).unwrap_or(u64::MAX);
    let stale_elapsed_since_output_ms = if matches!(job.status, ShellStatus::Running) {
        job.elapsed_since_output_ms
            .filter(|elapsed| *elapsed >= quiet_after_ms)
    } else {
        None
    };
    if exec.status == next_status
        && exec.live_output == next_live
        && exec.duration_ms == Some(job.elapsed_ms)
        && exec.stale_elapsed_since_output_ms == stale_elapsed_since_output_ms
    {
        return None;
    }
    Some(ShellExecLiveUpdate {
        task_id: task_id.to_string(),
        status: next_status,
        output: next_live,
        duration_ms: job.elapsed_ms,
        finalized,
        stale_elapsed_since_output_ms,
    })
}

pub(super) fn detached_shell_job_output(task_id: &str, exec: &ExecCell) -> Option<String> {
    let mut output = exec.live_output.clone().unwrap_or_default();
    if !output.trim().is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(&format!(
        "Shell job `{task_id}` is no longer attached to this TUI session."
    ));
    Some(output)
}

pub(super) fn shell_job_tool_status(status: &ShellStatus) -> ToolStatus {
    match status {
        ShellStatus::Running => ToolStatus::Running,
        ShellStatus::Completed => ToolStatus::Success,
        ShellStatus::Failed | ShellStatus::Killed | ShellStatus::TimedOut => ToolStatus::Failed,
    }
}

pub(super) fn shell_job_live_output(job: &ShellJobSnapshot) -> Option<String> {
    match (job.stdout_tail.is_empty(), job.stderr_tail.is_empty()) {
        (true, true) => None,
        (false, true) => Some(job.stdout_tail.clone()),
        (true, false) => Some(format!("STDERR:\n{}", job.stderr_tail)),
        (false, false) => Some(format!(
            "{}\n\nSTDERR:\n{}",
            job.stdout_tail, job.stderr_tail
        )),
    }
}

pub(super) fn active_rlm_task_entries(app: &App) -> Vec<TaskPanelEntry> {
    let Some(active) = app.active_cell.as_ref() else {
        return Vec::new();
    };
    let duration_ms = app
        .turn_started_at
        .map(|started| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    active
        .entries()
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let HistoryCell::Tool(ToolCell::Generic(generic)) = entry else {
                return None;
            };
            if !matches!(
                generic.name.as_str(),
                "rlm_open" | "rlm_eval" | "rlm_configure" | "rlm_close" | "rlm"
            ) || generic.status != ToolStatus::Running
            {
                return None;
            }
            let summary = generic
                .input_summary
                .as_deref()
                .filter(|summary| !summary.trim().is_empty())
                .unwrap_or("running chunked analysis");
            Some(TaskPanelEntry {
                exit_code: None,
                id: format!("rlm-{}", idx + 1),
                status: "running".to_string(),
                prompt_summary: format!("RLM: {summary}"),
                duration_ms,
                kind: TaskPanelEntryKind::Background,
                stale: false,
                elapsed_since_output_ms: None,
                owner_agent_id: None,
                owner_agent_name: None,
                current_tool: None,
                role: None,
                files_touched: 0,
            })
        })
        .collect()
}

#[cfg(test)]
mod automation_scan_gate_tests {
    use super::*;
    use std::time::Duration;

    fn app() -> App {
        crate::test_support::test_app_with_options(crate::tui::app::TuiOptions {
            skip_onboarding: true,
            ..crate::test_support::test_tui_options(std::path::PathBuf::from("."))
        })
    }

    #[test]
    fn a_quiet_session_with_nothing_scheduled_rescans_on_the_long_interval() {
        let app = app();
        assert_eq!(
            automation_scan_interval(&app, true),
            AUTOMATION_SCAN_QUIET_INTERVAL
        );
        assert!(!automation_scan_is_due(&app, true, Duration::from_secs(14)));
        assert!(automation_scan_is_due(&app, true, Duration::from_secs(15)));
    }

    #[test]
    fn a_session_in_use_keeps_the_busy_cadence() {
        let app = app();
        assert_eq!(
            automation_scan_interval(&app, false),
            AUTOMATION_SCAN_BUSY_INTERVAL
        );
        assert!(!automation_scan_is_due(
            &app,
            false,
            Duration::from_millis(2_499)
        ));
        assert!(automation_scan_is_due(
            &app,
            false,
            Duration::from_millis(2_500)
        ));
    }

    #[test]
    fn scheduled_work_in_the_band_keeps_the_busy_cadence_even_when_quiet() {
        let mut app = app();
        app.automation_panel.active_automations = 1;
        assert_eq!(
            automation_scan_interval(&app, true),
            AUTOMATION_SCAN_BUSY_INTERVAL,
            "an active automation can start a run at any moment"
        );

        let mut app = self::app();
        app.automation_panel.live_runs = 1;
        assert_eq!(
            automation_scan_interval(&app, true),
            AUTOMATION_SCAN_BUSY_INTERVAL,
            "a live run settles into a receipt the scan produces"
        );
    }

    #[test]
    fn a_finished_scan_is_folded_without_starting_the_next_one() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut app = app();
        rt.block_on(async {
            let scan =
                tokio::spawn(async { crate::tui::automation_panel::AutomationScan::default() });
            while !scan.is_finished() {
                tokio::task::yield_now().await;
            }
            app.automation_scan = Some(scan);
            // Not due: the scan is folded and nothing replaces it, so a
            // quiet session does not chain scan after scan.
            let changed = refresh_automation_panel(&mut app, false).await;
            assert!(!changed, "an empty store changes nothing visible");
        });
        assert!(app.automation_scan.is_none());
    }
}
