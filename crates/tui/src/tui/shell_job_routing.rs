//! Background shell job-center helpers for slash commands and pagers.
//!
//! Known limitations: the Bash Job pager (opened from `/jobs`, Alt+V or a
//! work-dock shell row) follows a running job by re-reading it on the event
//! loop's 2.5 s refresh (a bounded read that never consumes the model's
//! output cursor), so new output lands up to 2.5 s late. Shells run by
//! delegated agents show in `/jobs` and the dock with their Owner, not in the
//! agent's detail. Named PTY sessions (`tools/terminal_session.rs`) are not
//! listed here or in `/jobs`; they live in the terminal dock (#6912).

use crate::tools::shell::{ShellJobDetail, ShellJobSnapshot, ShellResult, ShellStatus};
use crate::tui::app::App;
use crate::tui::history::{HistoryCell, ToolCell};
use crate::tui::pager::PagerView;
use crate::tui::views::{CommandPaletteAction, ViewEvent};
use codewhale_localization::MessageId;

fn status_label(status: &ShellStatus, stale: bool) -> &'static str {
    if stale {
        return "stale";
    }
    match status {
        ShellStatus::Running => "running",
        ShellStatus::Completed => "complete",
        ShellStatus::Failed => "failed",
        ShellStatus::Killed => "killed",
        ShellStatus::TimedOut => "timeout",
    }
}

pub(super) fn format_shell_job_list(jobs: &[ShellJobSnapshot]) -> String {
    if jobs.is_empty() {
        return "No live Bash jobs. Bash jobs are process-local; after a restart, inspect durable task artifacts for prior command output.".to_string();
    }

    let mut lines = vec![
        format!("Bash jobs ({})", jobs.len()),
        "----------------------------------------".to_string(),
    ];
    for job in jobs {
        let task = job
            .linked_task_id
            .as_ref()
            .map(|id| format!(" task={id}"))
            .unwrap_or_default();
        lines.push(format!(
            "{}  {:8}  {}  {}{}",
            job.id,
            status_label(&job.status, job.stale),
            codewhale_command_contract::elapsed::format_elapsed_ms(job.elapsed_ms),
            exit_label(job.exit_code),
            task
        ));
        lines.push(format!("  cwd: {}", crate::utils::display_path(&job.cwd)));
        lines.push(format!("  cmd: {}", job.command));
        let tail = if !job.stderr_tail.trim().is_empty() {
            job.stderr_tail.trim()
        } else {
            job.stdout_tail.trim()
        };
        let tail_lines: Vec<&str> = tail.lines().collect();
        let shown = tail_lines.len().saturating_sub(3);
        for line in &tail_lines[shown..] {
            lines.push(format!("  | {line}"));
        }
    }
    lines.push(
        "Controls: /jobs show <id>, /jobs poll <id>, /jobs wait <id>, /jobs stdin <id> <input>, /jobs cancel <id|all>."
            .to_string(),
    );
    lines.join("\n")
}

fn exit_label(code: Option<i64>) -> String {
    match code {
        Some(0) => "exit 0".to_string(),
        Some(code) => format!("exit {code}"),
        None => "no exit yet".to_string(),
    }
}

pub(super) fn format_shell_poll(result: &ShellResult) -> String {
    let mut lines = vec![
        format!(
            "Bash job {}: {} {} elapsed={}",
            result.task_id.as_deref().unwrap_or("(unknown)"),
            status_label(&result.status, false),
            exit_label(result.exit_code),
            codewhale_command_contract::elapsed::format_elapsed_ms(result.duration_ms)
        ),
        String::new(),
    ];
    if result.status == ShellStatus::Killed {
        lines.push(
            crate::tools::shell_output::kill_outcome_note(&format!(
                "{}{}",
                result.stdout, result.stderr
            ))
            .to_string(),
        );
        lines.push(String::new());
    }
    if result.stdout.is_empty() && result.stderr.is_empty() {
        lines.push("(no new output)".to_string());
    } else {
        if !result.stdout.is_empty() {
            lines.push("STDOUT:".to_string());
            lines.push(result.stdout.clone());
        }
        if !result.stderr.is_empty() {
            lines.push("STDERR:".to_string());
            lines.push(result.stderr.clone());
        }
    }
    lines.join("\n")
}

const MAX_INSPECTOR_STREAM_BYTES: usize = 256 * 1024;

fn bounded_tail(text: &str) -> String {
    if text.len() <= MAX_INSPECTOR_STREAM_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_INSPECTOR_STREAM_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("... ({start} earlier bytes omitted)\n{}", &text[start..])
}

pub(crate) fn shell_job_for_cell(app: &App, cell_index: usize) -> Option<ShellJobDetail> {
    let HistoryCell::Tool(ToolCell::Exec(exec)) = app.cell_at_virtual_index(cell_index)? else {
        return None;
    };
    let session = app.current_session_id.as_deref()?;
    let mut manager = app
        .runtime_services
        .shell_manager
        .as_ref()?
        .try_lock()
        .ok()?;
    let job_id = match exec.shell_task_id.clone() {
        Some(id) => id,
        None => {
            let tool_id = app
                .tool_cells
                .iter()
                .find(|(_, index)| **index == cell_index)
                .map(|(tool_id, _)| tool_id.clone())?;
            manager
                .list_jobs_for_session(session)
                .into_iter()
                .find(|job| job.origin_tool_call_id.as_deref() == Some(tool_id.as_str()))?
                .id
        }
    };
    manager.inspect_job_for_session(session, &job_id).ok()
}

pub(crate) fn open_shell_job_pager(app: &mut App, detail: &ShellJobDetail) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(100)
        .saturating_sub(4);
    let live = detail.snapshot.status == ShellStatus::Running && !detail.snapshot.stale;
    let body = format_shell_job_detail(detail);
    let mut pager = PagerView::from_text(
        format!("Bash Job {}", detail.snapshot.id),
        &body,
        width.max(60),
    )
    .with_copy_text(body);
    if live {
        pager = pager.with_live_shell_job(detail.snapshot.id.clone());
        pager = pager.with_destructive_action(
            's',
            app.tr(MessageId::SidebarStopControl),
            app.tr(MessageId::WorkSurfaceStopConfirmHint),
            ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand {
                    command: format!("/jobs cancel {}", detail.snapshot.id),
                },
            },
        );
    }
    app.view_stack.push(pager);
}

pub(crate) fn refresh_live_shell_job_pager(app: &mut App) -> bool {
    let Some(job_id) = app
        .view_stack
        .top_pager_mut()
        .and_then(|pager| pager.live_shell_job().map(str::to_owned))
    else {
        return false;
    };
    let Some(shell_manager) = app.runtime_services.shell_manager.clone() else {
        return false;
    };
    let session = app.current_session_id.clone().unwrap_or_default();
    let detail = {
        let Ok(mut manager) = shell_manager.try_lock() else {
            return false;
        };
        manager.inspect_job_for_session(&session, &job_id).ok()
    };
    let Some(pager) = app.view_stack.top_pager_mut() else {
        return false;
    };
    let Some(detail) = detail else {
        pager.end_live_shell_job();
        return true;
    };
    let changed = pager.replace_live_text(&format_shell_job_detail(&detail));
    if detail.snapshot.status != ShellStatus::Running {
        pager.end_live_shell_job();
        return true;
    }
    changed
}

fn detail_status_label(job: &ShellJobSnapshot) -> String {
    let quiet =
        u64::try_from(crate::tools::shell::QUIET_NO_OUTPUT_AFTER.as_millis()).unwrap_or(u64::MAX);
    match job.elapsed_since_output_ms {
        Some(ms) if job.status == ShellStatus::Running && !job.stale && ms >= quiet => {
            crate::tui::history::stale_shell_status_label(ms)
        }
        _ => status_label(&job.status, job.stale).to_string(),
    }
}

pub(crate) fn format_shell_job_detail(detail: &ShellJobDetail) -> String {
    let job = &detail.snapshot;
    let mut lines = vec![
        format!("Job: {}", job.id),
        format!("Status: {}", detail_status_label(job)),
        format!("Command: {}", job.command),
        format!("Cwd: {}", crate::utils::display_path(&job.cwd)),
    ];
    if let Some(owner) = job
        .owner_agent_name
        .as_ref()
        .or(job.owner_agent_id.as_ref())
    {
        lines.push(format!("Owner: {owner}"));
    }
    lines.push(format!(
        "Elapsed: {}",
        codewhale_command_contract::elapsed::format_elapsed_ms(job.elapsed_ms)
    ));
    lines.push(format!("Exit Code: {}", exit_label(job.exit_code)));
    lines.push(format!("Stdin Available: {}", job.stdin_available));
    if let Some(task_id) = job.linked_task_id.as_ref() {
        lines.push(format!("Linked Task: {task_id}"));
    }
    if job.stale {
        lines.push(
            "Completion state: stale after restart; the process is no longer running.".to_string(),
        );
    } else if job.status == ShellStatus::Running {
        lines.push("Completion State: still running in this TUI process.".to_string());
    } else {
        lines.push("Completion State: finished.".to_string());
    }
    lines.push(String::new());
    if detail.stdout.is_empty()
        && detail.stderr.is_empty()
        && job.status == ShellStatus::Running
        && !job.stale
    {
        lines.push("(no output yet)".to_string());
        return lines.join("\n");
    }
    lines.push(format!("STDOUT ({} bytes):", job.stdout_len));
    lines.push(if detail.stdout.is_empty() {
        "(empty)".to_string()
    } else {
        bounded_tail(&detail.stdout)
    });
    lines.push(String::new());
    lines.push(format!("STDERR ({} bytes):", job.stderr_len));
    lines.push(if detail.stderr.is_empty() {
        "(empty)".to_string()
    } else {
        bounded_tail(&detail.stderr)
    });
    lines.join("\n")
}

pub(super) fn add_shell_job_message(app: &mut App, content: String) {
    app.add_message(HistoryCell::System { content });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn snapshot(status: ShellStatus, stale: bool) -> ShellJobSnapshot {
        ShellJobSnapshot {
            id: "shell_dead".to_string(),
            job_id: "shell_dead".to_string(),
            command: "cargo test".to_string(),
            cwd: PathBuf::from("/tmp/repo"),
            status,
            exit_code: None,
            elapsed_ms: 0,
            stdout_tail: String::new(),
            stderr_tail: "detached".to_string(),
            stdout_len: 0,
            stderr_len: 8,
            stdin_available: false,
            stale,
            elapsed_since_output_ms: None,
            linked_task_id: Some("task_1".to_string()),
            owner_agent_id: None,
            owner_agent_name: None,
            origin_tool_call_id: None,
            origin_turn_id: None,
            owner_session_id: "session-test".to_string(),
            background: true,
            finished_at: Some(chrono::Utc::now()),
        }
    }

    #[test]
    fn silent_running_job_detail_reads_quiet_not_stale() {
        let mut job = snapshot(ShellStatus::Running, false);
        job.elapsed_since_output_ms = Some(125_000);
        let body = format_shell_job_detail(&ShellJobDetail {
            snapshot: job,
            stdout: String::new(),
            stderr: String::new(),
        });
        assert!(body.contains("Status: running · no output for"), "{body}");
        assert!(body.contains("(no output yet)"), "{body}");
        assert!(!body.contains("stale"), "{body}");
    }

    #[test]
    fn list_shows_controls_and_stale_state() {
        let jobs = vec![snapshot(ShellStatus::Killed, true)];
        let formatted = format_shell_job_list(&jobs);
        assert!(formatted.contains("Bash jobs (1)"));
        assert!(!formatted.contains("Background commands"));
        assert!(formatted.contains("shell_dead"));
        assert!(formatted.contains("stale"));
        assert!(formatted.contains("/jobs poll <id>"));
        assert!(formatted.contains("/jobs cancel <id|all>"));
        assert!(!formatted.contains("cancel-all"));
        assert!(formatted.contains("task=task_1"));
    }
}
