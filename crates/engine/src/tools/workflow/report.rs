use super::{SCHEMA_RAW_PREVIEW_CHARS, WorkflowRunRecord, WorkflowRunStatus};
use std::path::{Path, PathBuf};

/// Persist a durable per-run report under `.codewhale/reports/<run_id>.md`
/// so a settled background run leaves one synthesized artifact even after
/// the session ends. Best-effort: report IO never affects the run outcome.
pub(super) fn write_run_report_artifact(workspace: &Path, record: &WorkflowRunRecord) {
    if !matches!(
        record.status,
        WorkflowRunStatus::Completed
            | WorkflowRunStatus::Degraded
            | WorkflowRunStatus::Failed
            | WorkflowRunStatus::Cancelled
    ) {
        return;
    }
    let Some(relative) = run_report_relative_path(&record.run_id) else {
        return;
    };
    let path = workspace.join(&relative);
    // Written through the confined helper: a link in `.codewhale` (or at the
    // report's own name) is refused instead of redirecting the write out of
    // the workspace, and missing parents are created without following links.
    if let Err(err) =
        codewhale_util::fs_confined::write(workspace, &path, render_run_report(record).as_bytes())
    {
        codewhale_runtime::logging::warn(format!(
            "workflow report {} not written: {err}",
            path.display()
        ));
    }
}

/// Workspace-relative path of a run's report, `.codewhale/reports/<id>.md`.
/// `None` when the id has no path-safe characters.
fn run_report_relative_path(run_id: &str) -> Option<PathBuf> {
    // Run ids are generated slugs, but never trust one as a path segment.
    let safe_id: String = run_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();
    (!safe_id.is_empty()).then(|| {
        Path::new(".codewhale")
            .join("reports")
            .join(format!("{safe_id}.md"))
    })
}

/// The run's report path relative to the workspace, only when the report
/// was actually written. Always `/`-separated: the path is shown to the model
/// and the user in receipts, and must read the same on every platform.
pub(super) fn written_run_report(workspace: &Path, run_id: &str) -> Option<String> {
    let relative = run_report_relative_path(run_id)?;
    workspace.join(&relative).is_file().then(|| {
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    })
}

/// Bounded preview of a raw `responseSchema` reply for run records and
/// reports (#5583): the first [`SCHEMA_RAW_PREVIEW_CHARS`] chars on a char
/// boundary, with an explicit marker when the text is longer.
pub(super) fn bounded_raw_preview(raw: &str) -> String {
    if raw.chars().count() <= SCHEMA_RAW_PREVIEW_CHARS {
        return raw.to_string();
    }
    let kept: String = raw.chars().take(SCHEMA_RAW_PREVIEW_CHARS).collect();
    format!("{kept}\n…[preview truncated; full reply in the schema artifact]")
}

/// Write the full raw reply of a failed `responseSchema` attempt as a
/// durable artifact beside the run report (#5583), returning its path.
/// `None` (with a warning) when the write fails — the bounded preview
/// remains either way.
pub(super) fn write_schema_raw_artifact(
    workspace: &Path,
    run_id: &str,
    task_id: &str,
    attempt: u32,
    raw: &str,
) -> Option<String> {
    // Run/task ids are generated slugs, but never trust one as a path segment.
    let safe = |text: &str| {
        text.chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .collect::<String>()
    };
    let (safe_run, safe_task) = (safe(run_id), safe(task_id));
    if safe_run.is_empty() || safe_task.is_empty() {
        return None;
    }
    let path = workspace.join(".codewhale").join("reports").join(format!(
        "{safe_run}.schema.{safe_task}.attempt{attempt}.txt"
    ));
    // Confined write: no link below the workspace is followed, and the missing
    // `.codewhale/reports` parents are created without following one.
    match codewhale_util::fs_confined::write(workspace, &path, raw.as_bytes()) {
        Ok(()) => Some(path.display().to_string()),
        Err(err) => {
            codewhale_runtime::logging::warn(format!(
                "workflow schema artifact {} not written: {err}",
                path.display()
            ));
            None
        }
    }
}

pub(super) fn render_run_report(record: &WorkflowRunRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Workflow run {}\n\n", record.run_id));
    out.push_str(&format!("- status: {:?}\n", record.status));
    if let Some(goal) = record.workflow_goal.as_deref() {
        out.push_str(&format!("- goal: {goal}\n"));
    }
    if let Some(source) = record.source_path.as_deref() {
        out.push_str(&format!("- source: {}\n", source.display()));
    }
    out.push_str(&format!("- started_at_ms: {}\n", record.started_at_ms));
    if let Some(completed) = record.completed_at_ms {
        out.push_str(&format!("- completed_at_ms: {completed}\n"));
    }
    if let Some(budget) = record.token_budget {
        out.push_str(&format!("- token_budget: {budget}\n"));
    }
    out.push_str(&format!("- child_agents: {}\n", record.child_ids.len()));
    if let Some(error) = record.error.as_deref() {
        out.push_str(&format!("- error: {error}\n"));
    }
    if record.dispatch_failure_count > 0 {
        out.push_str(&format!(
            "\n## Dispatch failures ({})\n\n",
            record.dispatch_failure_count
        ));
        let omitted = record
            .dispatch_failure_count
            .saturating_sub(u64::try_from(record.dispatch_failures.len()).unwrap_or(u64::MAX));
        if omitted > 0 {
            out.push_str(&format!(
                "- {omitted} older failure receipt(s) omitted from this bounded report; see the workflow journal\n"
            ));
        }
        for failure in &record.dispatch_failures {
            let slot = failure
                .label
                .as_deref()
                .or(failure.phase.as_deref())
                .unwrap_or("task");
            out.push_str(&format!("- {slot}: {}\n", failure.message));
        }
    }
    if !record.gate_status.is_empty() {
        out.push_str("\n## Gates\n\n");
        for line in &record.gate_status {
            out.push_str(&format!("- {line:?}\n"));
        }
    }
    if !record.progress.is_empty() {
        out.push_str("\n## Progress\n\n");
        for line in &record.progress {
            out.push_str(&format!("- {line}\n"));
        }
    }
    if !record.schema_errors.is_empty() {
        out.push_str(&format!(
            "\n## Schema errors ({})\n\n",
            record.schema_errors.len()
        ));
        for error in &record.schema_errors {
            out.push_str(&format!(
                "- `{}` attempt {}: [{}] {}\n",
                error.task_id, error.attempt, error.kind, error.message
            ));
            if !error.raw_preview.is_empty() {
                out.push_str(&format!(
                    "  - raw reply ({}):\n",
                    if error.raw_truncated {
                        "bounded preview; carried text was capped"
                    } else {
                        "bounded preview"
                    }
                ));
                for line in error.raw_preview.lines() {
                    out.push_str(&format!("      {line}\n"));
                }
            }
            if let Some(artifact) = &error.artifact {
                out.push_str(&format!("  - full reply: {artifact}\n"));
            }
        }
    }
    if !record.schema_repairs.is_empty() {
        out.push_str(&format!(
            "\n## Schema repairs ({}, including succeeded ones)\n\n",
            record.schema_repair_count
        ));
        for repair in &record.schema_repairs {
            out.push_str(&format!(
                "- `{}` attempt {}: [{}] a bounded repair followed\n",
                repair.task_id, repair.attempt, repair.kind
            ));
            if let Some(artifact) = &repair.artifact {
                out.push_str(&format!("  - full reply: {artifact}\n"));
            }
        }
    }
    if let Some(result) = record.result.as_ref() {
        out.push_str("\n## Result\n\n```json\n");
        out.push_str(&serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()));
        out.push_str("\n```\n");
    }
    if let Some(verification) = record.verification.as_ref() {
        out.push_str("\n## Verification\n\n```json\n");
        out.push_str(
            &serde_json::to_string_pretty(verification)
                .unwrap_or_else(|_| verification.to_string()),
        );
        out.push_str("\n```\n");
    }
    out
}
