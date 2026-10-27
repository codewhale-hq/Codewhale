//! Durable automation formatting and operator actions.
//!
//! Receipts for run/definition events are typed `HistoryCell::Automation`
//! cards (AUTOMATION-VISIBILITY-SPEC §2.2). Deletion uses the shared pager
//! review control; query responses (list/show) retain their text receipts.

use crate::automation_manager::{
    AutomationRecord, AutomationRunRecord, AutomationRunStatus, AutomationStatus,
    SharedAutomationManager, run_now_shared,
};
use crate::task_manager::SharedTaskManager;
use crate::tui::app::{App, AutomationAction};
use crate::tui::automation_panel::{SettledOutcome, SettledRun};
use crate::tui::history::{AutomationCell, AutomationCellKind, HistoryCell};
use codewhale_localization::{Locale, MessageId, tr};

pub(super) async fn handle_action(
    app: &mut App,
    config: &codewhale_runtime::config::Config,
    action: AutomationAction,
    task_manager: &SharedTaskManager,
) {
    let locale = app.ui_locale;
    // Engaging the automation surface acknowledges the failures the activity
    // band is demanding attention for (spec §2.1).
    app.automation_panel.acknowledge_failures();
    let Some(automations) = app.runtime_services.automations.clone() else {
        add_message(
            app,
            tr(locale, MessageId::AutomationManagerUnavailable).into_owned(),
        );
        return;
    };

    let cell = match action {
        AutomationAction::Open { focus } => {
            // The room, not a receipt: the one place to see, pause, run,
            // cancel, and delete automations (0.9.12 defect #14).
            if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::Automations) {
                app.view_stack.pop();
            }
            app.view_stack
                .push(crate::tui::views::automations::AutomationsView::new(
                    app,
                    config,
                    focus.as_deref(),
                ));
            app.needs_redraw = true;
            return;
        }
        AutomationAction::List => HistoryCell::System {
            content: list(locale, &automations).await,
        },
        AutomationAction::Show(id) => HistoryCell::System {
            content: show(locale, &automations, &id).await,
        },
        AutomationAction::Pause(id) => mutate(locale, &automations, &id, Mutation::Pause).await,
        AutomationAction::Resume(id) => mutate(locale, &automations, &id, Mutation::Resume).await,
        AutomationAction::Delete { id, confirmation } => {
            let (cell, command) = delete(locale, &automations, &id, confirmation.as_deref()).await;
            if let Some(command) = command {
                if let HistoryCell::System { content } = cell {
                    let width = app
                        .viewport
                        .last_transcript_area
                        .map_or(80, |area| area.width);
                    app.view_stack
                        .push(crate::tui::pager::PagerView::command_review(
                            tr(locale, MessageId::AutomationActionDelete),
                            &content,
                            width.saturating_sub(2),
                            command,
                            locale,
                        ));
                    app.needs_redraw = true;
                }
                return;
            }
            cell
        }
        AutomationAction::Run(id) => run_now(locale, &automations, &id, task_manager).await,
    };
    present_receipt(app, cell);
}

fn present_receipt(app: &mut App, cell: HistoryCell) {
    // The automation room covers the transcript. A refused action still needs
    // an immediate visible receipt there, even when no run record was created.
    if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::Automations)
        && let Some(mut view) = app.view_stack.pop()
    {
        if let Some(automations_view) =
            view.as_any_mut()
                .downcast_mut::<crate::tui::views::automations::AutomationsView>()
        {
            let receipt = match &cell {
                HistoryCell::Automation(receipt) => receipt.plain_summary(),
                HistoryCell::System { content } => content.clone(),
                _ => String::new(),
            };
            automations_view.show_action_receipt(receipt);
        }
        app.view_stack.push_boxed(view);
        app.needs_redraw = true;
    }
    app.add_message(cell);
}

async fn list(locale: Locale, automations: &SharedAutomationManager) -> String {
    match automations.lock().await.list_automations() {
        Ok(records) => format_list(locale, &records),
        Err(error) => {
            tr(locale, MessageId::AutomationListFailed).replace("{error}", &error.to_string())
        }
    }
}

async fn show(locale: Locale, automations: &SharedAutomationManager, id: &str) -> String {
    let manager = automations.lock().await;
    match manager.get_automation(id) {
        Ok(record) => {
            let runs = manager.list_runs(id, Some(5)).ok();
            format_detail(locale, &record, runs.as_deref())
        }
        Err(error) => action_failed(locale, MessageId::AutomationActionInspect, id, &error),
    }
}

#[derive(Clone, Copy)]
enum Mutation {
    Pause,
    Resume,
}

impl Mutation {
    const fn action_id(self) -> MessageId {
        match self {
            Self::Pause => MessageId::AutomationActionPause,
            Self::Resume => MessageId::AutomationActionResume,
        }
    }

    const fn receipt_id(self) -> MessageId {
        match self {
            Self::Pause => MessageId::AutomationActionPaused,
            Self::Resume => MessageId::AutomationActionResumed,
        }
    }
}

async fn mutate(
    locale: Locale,
    automations: &SharedAutomationManager,
    id: &str,
    mutation: Mutation,
) -> HistoryCell {
    let manager = automations.lock().await;
    let result = match mutation {
        Mutation::Pause => manager.pause_automation(id),
        Mutation::Resume => manager.resume_automation(id),
    };

    match result {
        Ok(record) => HistoryCell::Automation(AutomationCell::mutated(
            display_text(&record.name),
            tr(locale, mutation.receipt_id()).into_owned(),
        )),
        Err(error) => HistoryCell::System {
            content: action_failed(locale, mutation.action_id(), id, &error),
        },
    }
}

async fn delete(
    locale: Locale,
    automations: &SharedAutomationManager,
    id: &str,
    confirmation: Option<&str>,
) -> (HistoryCell, Option<String>) {
    let manager = automations.lock().await;
    let record = match manager.get_automation(id) {
        Ok(record) => record,
        Err(error) => {
            return (
                system(action_failed(
                    locale,
                    MessageId::AutomationActionDelete,
                    id,
                    &error,
                )),
                None,
            );
        }
    };
    let runs = match manager.list_runs(id, None) {
        Ok(runs) => runs,
        Err(error) => {
            return (
                system(action_failed(
                    locale,
                    MessageId::AutomationActionDelete,
                    id,
                    &error,
                )),
                None,
            );
        }
    };
    let token = match deletion_token(&record, &runs) {
        Ok(token) => token,
        Err(error) => {
            return (
                system(action_failed(
                    locale,
                    MessageId::AutomationActionDelete,
                    id,
                    &error,
                )),
                None,
            );
        }
    };

    let Some(confirmation) = confirmation else {
        let command = format!("/automation delete {id} --confirm {token}");
        let recent_len = runs.len().min(5);
        let detail = format_detail(locale, &record, Some(&runs[..recent_len]));
        let preview = tr(locale, MessageId::AutomationDeletePreview)
            .replace("{id}", id)
            .replace("{name}", &display_text(&record.name))
            .replace("{run_count}", &runs.len().to_string())
            .replace("{command}", "y → Enter");
        return (system(format!("{detail}\n\n{preview}")), Some(command));
    };

    if confirmation != token {
        let command = format!("/automation delete {id}");
        return (
            system(
                tr(locale, MessageId::AutomationDeleteConfirmationStale)
                    .replace("{id}", id)
                    .replace("{command}", &command),
            ),
            None,
        );
    }

    let receipt = match manager.delete_automation(id) {
        Ok(record) => HistoryCell::Automation(
            AutomationCell::mutated(
                display_text(&record.name),
                tr(locale, MessageId::AutomationReceiptDeleted).into_owned(),
            )
            .with_detail(Some(
                tr(locale, MessageId::AutomationDeletedRunsDetail)
                    .replace("{run_count}", &runs.len().to_string()),
            )),
        ),
        Err(error) => system(action_failed(
            locale,
            MessageId::AutomationActionDelete,
            id,
            &error,
        )),
    };
    (receipt, None)
}

fn system(content: String) -> HistoryCell {
    HistoryCell::System { content }
}

fn deletion_token(
    record: &AutomationRecord,
    runs: &[AutomationRunRecord],
) -> Result<String, serde_json::Error> {
    let mut canonical_runs = runs.iter().collect::<Vec<_>>();
    canonical_runs.sort_by(|left, right| left.id.cmp(&right.id));
    serde_json::to_vec(&(record, canonical_runs)).map(codewhale_runtime::hashing::sha256_hex)
}

fn action_failed(
    locale: Locale,
    action_id: MessageId,
    id: &str,
    error: &impl std::fmt::Display,
) -> String {
    tr(locale, MessageId::AutomationActionFailed)
        .replace("{action}", &tr(locale, action_id))
        .replace("{id}", id)
        .replace("{error}", &error.to_string())
}

fn format_list(locale: Locale, records: &[AutomationRecord]) -> String {
    if records.is_empty() {
        return tr(locale, MessageId::AutomationEmpty).into_owned();
    }

    let lines = records
        .iter()
        .map(|record| {
            format!(
                "{}  [{}]  {}  ({}; {}: {})",
                record.id,
                status_label(locale, record.status),
                display_text(&record.name),
                delivery_mode_label(record),
                tr(locale, MessageId::AutomationNextLabel),
                timestamp(record.next_run_at)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}:\n{lines}", tr(locale, MessageId::AutomationListHeading))
}

fn format_detail(
    locale: Locale,
    record: &AutomationRecord,
    runs: Option<&[AutomationRunRecord]>,
) -> String {
    let runs = match runs {
        Some([]) => format!("  {}", tr(locale, MessageId::AutomationNoRuns)),
        Some(runs) => runs
            .iter()
            .map(|run| {
                format!(
                    "  {}  {}  ({} {})",
                    run_status_label(locale, run.status),
                    run.scheduled_for.to_rfc3339(),
                    tr(locale, MessageId::AutomationTaskLabel),
                    run.task_id.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => format!("  {}", tr(locale, MessageId::AutomationRunsUnavailable)),
    };
    let mut lines = vec![
        format!(
            "{} {} [{}]",
            tr(locale, MessageId::AutomationNoun),
            record.id,
            status_label(locale, record.status)
        ),
        field(
            locale,
            MessageId::AutomationNameLabel,
            &display_text(&record.name),
        ),
        format!("  {}:", tr(locale, MessageId::AutomationPromptLabel)),
    ];
    lines.extend(
        display_text(&record.prompt)
            .lines()
            .map(|line| format!("    {line}")),
    );
    lines.extend(record.cwds.iter().map(|cwd| {
        field(
            locale,
            MessageId::AutomationCwdLabel,
            &display_text(&codewhale_util::utils::display_path(cwd)),
        )
    }));
    if let Some(mode) = record.mode.as_deref() {
        lines.push(field(
            locale,
            MessageId::AutomationModeLabel,
            &display_text(mode),
        ));
    }
    if let Some(model) = record.model.as_deref() {
        let route = record
            .model_provider_id
            .as_ref()
            .or(record.model_provider.as_ref())
            .map_or_else(
                || model.to_string(),
                |provider| format!("{provider} / {model}"),
            );
        lines.push(field(
            locale,
            MessageId::SetupCardModelLabel,
            &display_text(&route),
        ));
    }
    if let Some(allow_shell) = record.allow_shell {
        lines.push(field(
            locale,
            MessageId::AutomationAllowShellLabel,
            &allow_shell.to_string(),
        ));
    }
    if let Some(trust_mode) = record.trust_mode {
        lines.push(field(
            locale,
            MessageId::AutomationTrustModeLabel,
            &trust_mode.to_string(),
        ));
    }
    if let Some(auto_approve) = record.auto_approve {
        lines.push(field(
            locale,
            MessageId::AutomationAutoApproveLabel,
            &auto_approve.to_string(),
        ));
    }
    lines.extend([
        field(locale, MessageId::AutomationRruleLabel, &record.rrule),
        field(
            locale,
            MessageId::AutomationDeliveryLabel,
            &delivery_mode_label(record),
        ),
        field(
            locale,
            MessageId::AutomationNextLabel,
            &timestamp(record.next_run_at),
        ),
        field(
            locale,
            MessageId::AutomationLastLabel,
            &timestamp(record.last_run_at),
        ),
        format!(
            "{}:\n{runs}",
            tr(locale, MessageId::AutomationRecentRunsLabel)
        ),
    ]);
    lines.join("\n")
}

fn field(locale: Locale, label: MessageId, value: &str) -> String {
    format!("  {}: {value}", tr(locale, label))
}

fn display_text(value: &str) -> String {
    let mut visible = String::with_capacity(value.len());
    crate::tui::osc8::strip_ansi_into(value, &mut visible);
    codewhale_config::persistence::redact_secrets(&visible)
}

/// `run <id>`: enqueue immediately and acknowledge with a typed `Started`
/// receipt — the `${subject} started in background` line, promoted from tip
/// to receipt (spec §2.2).
async fn run_now(
    locale: Locale,
    automations: &SharedAutomationManager,
    id: &str,
    task_manager: &SharedTaskManager,
) -> HistoryCell {
    let name = automations
        .lock()
        .await
        .get_automation(id)
        .ok()
        .map(|record| display_text(&record.name));
    match run_now_shared(automations, id, task_manager).await {
        Ok(run) => {
            // The run record can come back already settled: a refused
            // enqueue returns Ok with status Failed, and a "started in
            // background" receipt would then be a lie. Branch the receipt
            // on the record's own status. Running/Queued are the live
            // states; Completed/Canceled cannot occur this soon after
            // enqueue, but if one ever does, "started" would be false —
            // report the settled verb instead.
            let kind = match run.status {
                AutomationRunStatus::Failed => AutomationCellKind::Failed,
                AutomationRunStatus::Queued | AutomationRunStatus::Running => {
                    AutomationCellKind::Started
                }
                AutomationRunStatus::Completed => AutomationCellKind::Completed,
                AutomationRunStatus::Canceled => AutomationCellKind::Canceled,
            };
            // The operator asked for this run by hand: echo its full id so
            // it can be copied straight from the receipt.
            let mut detail = format!(
                "{} {} · {} {}",
                tr(locale, MessageId::AutomationRunLabel),
                run.id,
                tr(locale, MessageId::AutomationTaskLabel),
                run.task_id.as_deref().map(short_id).unwrap_or("-")
            );
            if let Some(error) = run.error.as_deref() {
                detail.push_str(" · ");
                detail.push_str(&display_text(error));
            }
            let name = name.unwrap_or_else(|| id.to_string());
            HistoryCell::Automation(
                AutomationCell::event(kind, name, locale).with_detail(Some(detail)),
            )
        }
        Err(error) => system(action_failed(
            locale,
            MessageId::AutomationActionRun,
            id,
            &error,
        )),
    }
}

/// Receipt for a run the projection watched go live and settle (spec §2.2:
/// `Documentation completed in background  42s · run r-8f19`). `Completed`
/// wears Outcome ink; a genuinely failed run is the one receipt that wears
/// Failure, and its detail leads with the (redacted) error.
/// A canceled run (#6162) wears Attention ink and its detail leads with the
/// cancellation reason, so a stopped run is never silent and never red.
pub(super) fn settled_run_receipt(locale: Locale, run: &SettledRun) -> HistoryCell {
    let kind = match run.outcome {
        SettledOutcome::Completed => AutomationCellKind::Completed,
        SettledOutcome::Failed => AutomationCellKind::Failed,
        SettledOutcome::Canceled => AutomationCellKind::Canceled,
    };
    let mut parts = Vec::new();
    if let Some(error) = run
        .error
        .as_deref()
        .map(display_text)
        .filter(|error| !error.is_empty())
    {
        parts.push(error);
    }
    if let Some(duration_ms) = run.duration_ms {
        parts.push(codewhale_command_contract::elapsed::format_elapsed_ms(
            duration_ms,
        ));
    }
    parts.push(format!(
        "{} {}",
        tr(locale, MessageId::AutomationRunLabel),
        short_id(&run.run_id)
    ));
    HistoryCell::Automation(
        AutomationCell::event(kind, display_text(&run.automation_name), locale)
            .with_detail(Some(parts.join(" · "))),
    )
}

/// Background receipts name a run/task by prefix — full UUIDs would eat the
/// card's one line. The complete ids stay on the records for
/// `/automation show`; only the operator-driven `/automation run` echo
/// carries the whole run id.
fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

fn status_label(locale: Locale, status: AutomationStatus) -> String {
    let id = match status {
        AutomationStatus::Active => MessageId::AutomationStatusActive,
        AutomationStatus::Paused => MessageId::AutomationStatusPaused,
    };
    tr(locale, id).into_owned()
}

fn run_status_label(locale: Locale, status: AutomationRunStatus) -> String {
    let id = match status {
        AutomationRunStatus::Queued => MessageId::AutomationRunStatusQueued,
        AutomationRunStatus::Running => MessageId::AutomationRunStatusRunning,
        AutomationRunStatus::Completed => MessageId::AutomationRunStatusCompleted,
        AutomationRunStatus::Failed => MessageId::AutomationRunStatusFailed,
        AutomationRunStatus::Canceled => MessageId::AutomationRunStatusCanceled,
    };
    tr(locale, id).into_owned()
}

fn timestamp(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    value
        .map(|timestamp| timestamp.to_rfc3339())
        .unwrap_or_else(|| "-".to_string())
}

/// Delivery mode is a stored enum value, not prose — render it raw like
/// `mode`/`rrule` rather than translating it. Unset means the default `task`.
fn delivery_mode_label(record: &AutomationRecord) -> String {
    format!("{:?}", record.delivery_mode.unwrap_or_default()).to_ascii_lowercase()
}

fn add_message(app: &mut App, content: String) {
    present_receipt(app, HistoryCell::System { content });
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use super::*;
    use crate::automation_manager::{
        AutomationDeliveryMode, AutomationManager, CreateAutomationRequest,
    };
    use chrono::Utc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn record(status: AutomationStatus) -> AutomationRecord {
        let now = Utc::now();
        AutomationRecord {
            destination_thread_id: None,
            schema_version: 1,
            execution_scope: Some(crate::task_manager::test_execution_scope("test")),
            id: "auto_1".to_string(),
            name: "Nightly checks".to_string(),
            prompt: "Run checks".to_string(),
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

    #[test]
    fn action_receipts_reach_the_open_room_and_remain_in_history() {
        use crate::tui::views::{ModalKind, automations::AutomationsView};
        use ratatui::{buffer::Buffer, layout::Rect};

        let root = TempDir::new().unwrap();
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(root.path()),
        );
        app.view_stack
            .push(AutomationsView::from_rows(Vec::new(), Locale::En));
        for cell in [
            system(
                "Could not run automation: Automation belongs to another Runtime execution scope"
                    .to_string(),
            ),
            HistoryCell::Automation(AutomationCell::mutated(
                "Nightly checks".into(),
                "paused".into(),
            )),
        ] {
            let expected = match &cell {
                HistoryCell::System { content } => content.clone(),
                HistoryCell::Automation(receipt) => receipt.plain_summary(),
                _ => unreachable!(),
            };
            let before = app.history.len();
            present_receipt(&mut app, cell);
            assert_eq!(app.history.len(), before + 1);
            assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Automations));
            let view = app.view_stack.pop().unwrap();
            let area = Rect::new(0, 0, 120, 20);
            let mut buffer = Buffer::empty(area);
            view.render(area, &mut buffer);
            let text = (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(&expected), "{text}");
            app.view_stack.push_boxed(view);
        }
        // The missing-manager early return uses this same path.
        add_message(&mut app, "Automation manager unavailable".into());
        let view = app.view_stack.pop().unwrap();
        let area = Rect::new(0, 0, 80, 20);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Automation manager unavailable"));
    }

    #[test]
    fn list_explains_empty_state_and_operator_controls() {
        assert!(format_list(Locale::En, &[]).contains("Choose New automation"));
        let text = format_list(Locale::En, &[record(AutomationStatus::Paused)]);
        assert!(text.contains("auto_1  [paused]  Nightly checks"));
        assert!(text.contains("next: -"));
    }

    #[test]
    fn detail_keeps_schedule_and_recent_run_shape() {
        let text = format_detail(Locale::En, &record(AutomationStatus::Active), Some(&[]));
        assert!(text.contains("Automation auto_1 [active]"));
        assert!(text.contains("rrule: FREQ=DAILY"));
        assert!(text.contains("recent runs:"));
    }

    #[test]
    fn list_and_detail_surface_delivery_mode() {
        let mut automation = record(AutomationStatus::Active);
        automation.delivery_mode = Some(AutomationDeliveryMode::Watcher);

        let list = format_list(Locale::En, std::slice::from_ref(&automation));
        assert!(list.contains("(watcher; next:"));

        let detail = format_detail(Locale::En, &automation, Some(&[]));
        assert!(detail.contains("  delivery: watcher"));

        let default_detail =
            format_detail(Locale::En, &record(AutomationStatus::Active), Some(&[]));
        assert!(default_detail.contains("  delivery: task"));
    }

    #[test]
    fn detail_exposes_configured_execution_contract_and_redacts_prompt() {
        let mut automation = record(AutomationStatus::Active);
        automation.prompt = "Run release checks\napi_key = \"sk-audit-secret-value\"".to_string();
        automation.cwds = vec!["release-workspace".into()];
        automation.mode = Some("agent".to_string());
        automation.allow_shell = Some(true);
        automation.trust_mode = Some(false);
        automation.auto_approve = Some(true);

        let text = format_detail(Locale::En, &automation, Some(&[]));

        assert!(text.contains("  prompt:\n    Run release checks"));
        assert!(text.contains("[redacted]"));
        assert!(!text.contains("sk-audit-secret-value"));
        assert!(text.contains("  cwd: release-workspace"));
        assert!(text.contains("  mode: agent"));
        assert!(text.contains("  allow_shell: true"));
        assert!(text.contains("  trust_mode: false"));
        assert!(text.contains("  auto_approve: true"));
    }

    #[test]
    fn list_stays_compact_and_detail_omits_unset_execution_overrides() {
        let automation = record(AutomationStatus::Paused);

        let list = format_list(Locale::En, std::slice::from_ref(&automation));
        assert!(!list.contains(&automation.prompt));
        assert!(!list.contains("prompt:"));
        assert!(!list.contains("cwd:"));
        assert!(!list.contains("mode:"));
        assert!(!list.contains("allow_shell:"));
        assert!(!list.contains("trust_mode:"));
        assert!(!list.contains("auto_approve:"));

        let detail = format_detail(Locale::En, &automation, Some(&[]));
        assert!(!detail.contains("cwd:"));
        assert!(!detail.contains("mode:"));
        assert!(!detail.contains("allow_shell:"));
        assert!(!detail.contains("trust_mode:"));
        assert!(!detail.contains("auto_approve:"));
    }

    #[test]
    fn automation_output_routes_through_the_selected_locale() {
        let french = format_list(Locale::Fr, &[record(AutomationStatus::Paused)]);
        assert!(french.starts_with(tr(Locale::Fr, MessageId::AutomationListHeading).as_ref()));
        assert!(french.contains(tr(Locale::Fr, MessageId::AutomationStatusPaused).as_ref()));
        assert!(!french.starts_with(tr(Locale::En, MessageId::AutomationListHeading).as_ref()));

        for locale in Locale::shipped_complete() {
            for id in [
                MessageId::AutomationManagerUnavailable,
                MessageId::AutomationDeletePreview,
                MessageId::AutomationReceiptDeleted,
                MessageId::AutomationDeletedRunsDetail,
                MessageId::AutomationReceiptStarted,
                MessageId::AutomationRunLabel,
                MessageId::AutomationBandScheduled,
            ] {
                assert_ne!(tr(*locale, id).as_ref(), format!("{id:?}"), "{locale:?}");
            }
        }
    }

    /// #6162: a canceled run gets a receipt of its own — attention ink, the
    /// `canceled` verb, and the cancellation reason leading the detail — so a
    /// stopped run is never silent and never dressed as a crash.
    #[test]
    fn a_canceled_run_settles_with_a_canceled_receipt() {
        let canceled = settled_run_receipt(
            Locale::En,
            &SettledRun {
                automation_id: "auto_1".to_string(),
                automation_name: "Documentation".to_string(),
                run_id: "r-8f21deadbeef-0000".to_string(),
                outcome: SettledOutcome::Canceled,
                duration_ms: Some(3_000),
                error: Some("canceled by request".to_string()),
            },
        );
        let HistoryCell::Automation(cell) = canceled else {
            panic!("a settled run is a typed Automation receipt");
        };
        assert_eq!(cell.kind, AutomationCellKind::Canceled);
        assert_eq!(
            cell.kind.chrome_ink(),
            codewhale_palette::ChromeInk::Attention
        );
        assert_eq!(cell.name, "Documentation");
        assert_eq!(cell.verb, "canceled");
        assert_eq!(
            cell.detail.as_deref(),
            Some("canceled by request · 3s · run r-8f21deadbe")
        );
    }

    #[test]
    fn settled_runs_become_completed_or_failed_receipts() {
        let completed = settled_run_receipt(
            Locale::En,
            &SettledRun {
                automation_id: "auto_1".to_string(),
                automation_name: "Documentation".to_string(),
                run_id: "r-8f19deadbeef-0000".to_string(),
                outcome: SettledOutcome::Completed,
                duration_ms: Some(42_000),
                error: None,
            },
        );
        let HistoryCell::Automation(cell) = completed else {
            panic!("a settled run is a typed Automation receipt");
        };
        assert_eq!(cell.kind, AutomationCellKind::Completed);
        assert_eq!(cell.name, "Documentation");
        assert_eq!(cell.verb, "completed in background");
        assert_eq!(cell.detail.as_deref(), Some("42s · run r-8f19deadbe"));

        let failed = settled_run_receipt(
            Locale::En,
            &SettledRun {
                automation_id: "auto_1".to_string(),
                automation_name: "Documentation".to_string(),
                run_id: "r-8f20".to_string(),
                outcome: SettledOutcome::Failed,
                duration_ms: None,
                error: Some(
                    "provider timeout\u{1b}[31m token=sk-abcdefghijklmnopqrstuvwxyz0123456789"
                        .to_string(),
                ),
            },
        );
        let HistoryCell::Automation(cell) = failed else {
            panic!("a failed run is a typed Automation receipt");
        };
        assert_eq!(cell.kind, AutomationCellKind::Failed);
        assert_eq!(cell.verb, "failed");
        let detail = cell.detail.expect("failure detail");
        assert!(detail.starts_with("provider timeout"), "{detail}");
        assert!(!detail.contains("\u{1b}"), "ANSI stripped: {detail}");
        assert!(
            !detail.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "secrets redacted: {detail}"
        );
        assert!(detail.ends_with("· run r-8f20"), "{detail}");
    }

    #[tokio::test]
    async fn pause_and_resume_emit_typed_mutation_receipts() {
        let temp = TempDir::new().expect("temp dir");
        let manager = AutomationManager::open_for_test(temp.path().to_path_buf()).expect("manager");
        let automation = manager
            .create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "Nightly checks".to_string(),
                prompt: "Run checks".to_string(),
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
            .expect("automation");
        let manager = Arc::new(Mutex::new(manager));

        let HistoryCell::Automation(paused) =
            mutate(Locale::En, &manager, &automation.id, Mutation::Pause).await
        else {
            panic!("pause emits a typed Automation receipt");
        };
        assert_eq!(paused.kind, AutomationCellKind::Mutated);
        assert_eq!(paused.verb, "paused");
        assert_eq!(paused.name, "Nightly checks");
        assert_eq!(paused.detail, None);

        let HistoryCell::Automation(resumed) =
            mutate(Locale::En, &manager, &automation.id, Mutation::Resume).await
        else {
            panic!("resume emits a typed Automation receipt");
        };
        assert_eq!(resumed.verb, "resumed");

        // A failed action keeps the System error path.
        let HistoryCell::System { content } =
            mutate(Locale::En, &manager, "missing", Mutation::Pause).await
        else {
            panic!("a failed mutation stays a System error");
        };
        assert!(content.contains("missing"), "{content}");
    }

    #[tokio::test]
    async fn delete_is_a_noop_until_snapshot_confirmation_then_removes_definition_and_runs() {
        let temp = TempDir::new().expect("temp dir");
        let manager = AutomationManager::open_for_test(temp.path().to_path_buf()).expect("manager");
        let automation = manager
            .create_automation(CreateAutomationRequest {
                destination_thread_id: None,
                name: "Nightly checks".to_string(),
                prompt: "Run checks".to_string(),
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
                status: Some(AutomationStatus::Paused),
            })
            .expect("automation");
        let now = Utc::now();
        let run = AutomationRunRecord {
            schema_version: 1,
            id: "run_1".to_string(),
            automation_id: automation.id.clone(),
            scheduled_for: now,
            status: AutomationRunStatus::Completed,
            created_at: now,
            started_at: Some(now),
            ended_at: Some(now),
            task_id: Some("task_1".to_string()),
            thread_id: None,
            turn_id: None,
            error: None,
            dispatch: None,
        };
        let runs_dir = temp.path().join("runs").join(&automation.id);
        fs::create_dir_all(&runs_dir).expect("runs dir");
        fs::write(
            runs_dir.join("run_1.json"),
            serde_json::to_vec_pretty(&run).expect("serialize run"),
        )
        .expect("write run");
        let manager = Arc::new(Mutex::new(manager));

        let (HistoryCell::System { content: preview }, Some(command)) =
            delete(Locale::En, &manager, &automation.id, None).await
        else {
            panic!("delete preview carries a separate confirmation command");
        };
        assert!(preview.contains("Nothing was deleted"), "{preview}");
        assert!(preview.contains("Recorded runs: 1"), "{preview}");
        assert!(preview.contains("all runs to settle"), "{preview}");
        assert!(preview.contains("Up to 50"), "{preview}");
        assert!(preview.contains("kept in the archive"), "{preview}");
        assert!(
            !preview.contains("--confirm"),
            "the token stays in the control"
        );
        assert!(
            manager.lock().await.get_automation(&automation.id).is_ok(),
            "preview must preserve the definition"
        );
        assert_eq!(
            manager
                .lock()
                .await
                .list_runs(&automation.id, None)
                .expect("runs after preview")
                .len(),
            1,
            "preview must preserve run history"
        );

        let (HistoryCell::System { content: stale }, None) =
            delete(Locale::En, &manager, &automation.id, Some("wrong-receipt")).await
        else {
            panic!("stale confirmation stays a System text report");
        };
        assert!(stale.contains("no longer matches"), "{stale}");
        assert!(
            manager.lock().await.get_automation(&automation.id).is_ok(),
            "a mismatched receipt must not delete"
        );

        let token = command.split_whitespace().last().expect("reviewed token");
        manager
            .lock()
            .await
            .resume_automation(&automation.id)
            .unwrap();
        let (HistoryCell::System { content: changed }, None) =
            delete(Locale::En, &manager, &automation.id, Some(token)).await
        else {
            panic!("changed definition refuses the old confirmation");
        };
        assert!(changed.contains("no longer matches"));
        assert!(manager.lock().await.get_automation(&automation.id).is_ok());
        let (_, Some(command)) = delete(Locale::En, &manager, &automation.id, None).await else {
            panic!("fresh review");
        };
        use crate::tui::views::{CommandPaletteAction, ModalView, ViewAction, ViewEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut pager = crate::tui::pager::PagerView::command_review(
            "Delete",
            &preview,
            78,
            command.clone(),
            Locale::En,
        );
        assert!(matches!(
            pager.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            ViewAction::None
        ));
        let ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
            action: CommandPaletteAction::ExecuteCommand { command: confirmed },
        }) = pager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("confirmed control dispatches the exact reviewed command");
        };
        assert_eq!(confirmed, command);
        let token = confirmed.split_whitespace().last().unwrap();
        let (HistoryCell::Automation(deleted), None) =
            delete(Locale::En, &manager, &automation.id, Some(token)).await
        else {
            panic!("confirmed deletion is a typed Automation receipt");
        };
        assert_eq!(deleted.kind, AutomationCellKind::Mutated);
        assert_eq!(deleted.verb, "deleted");
        assert_eq!(deleted.name, "Nightly checks");
        assert!(
            deleted
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains('1')),
            "the run count rides the receipt detail: {deleted:?}"
        );
        assert!(
            manager.lock().await.get_automation(&automation.id).is_err(),
            "confirmed deletion removes definition"
        );
        assert!(
            !runs_dir.exists(),
            "confirmed deletion removes live receipts"
        );
        let archived = manager
            .lock()
            .await
            .list_archived_runs(&automation.id)
            .expect("archived history");
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, run.id);
        assert!(
            deleted.detail.as_deref().is_some_and(|detail| {
                detail.contains("up to 50") && detail.contains("kept in the archive")
            }),
            "deletion receipt explains retention: {deleted:?}"
        );
    }
}
