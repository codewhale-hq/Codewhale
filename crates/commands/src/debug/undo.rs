//! Portable edit/diff/undo/retry policy. Hosts own I/O and mutation; typed
//! outcomes preserve refusal versus conversation-fallback without parsing text.

use super::{CommandResult, DebugAction};
use codewhale_command_contract::facets::*;
use codewhale_command_contract::handler::{
    CommandCapabilities as Caps, CommandContexts, CommandHandler,
};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

macro_rules! registration {
    ($ty:ident, $name:literal, $usage:literal, $aliases:expr, $key:literal, $caps:expr, $handler:ident) => {
        pub struct $ty;
        impl RegisterCommand<CommandResult> for $ty {
            fn info() -> &'static CommandInfo {
                &CommandInfo {
                    name: $name,
                    aliases: $aliases,
                    usage: $usage,
                    description_key: $key,
                }
            }
            fn handler() -> CommandHandler<CommandResult> {
                CommandHandler::Contextual {
                    capabilities: $caps,
                    handler: $handler,
                }
            }
        }
    };
}
registration!(
    EditCmd,
    "edit",
    "/edit",
    &[],
    "cmd_edit_description",
    Caps::DEBUG_HISTORY,
    edit
);
registration!(
    DiffCmd,
    "diff",
    "/diff",
    &[],
    "cmd_diff_description",
    Caps::DEBUG_DIFF.union(Caps::PRESENTATION),
    diff
);
registration!(
    UndoCmd,
    "undo",
    "/undo [force]",
    &[],
    "cmd_undo_description",
    Caps::DEBUG_UNDO
        .union(Caps::DEBUG_HISTORY)
        .union(Caps::PRESENTATION),
    undo
);
registration!(
    RetryCmd,
    "retry",
    "/retry",
    &["chongshi"],
    "cmd_retry_description",
    Caps::DEBUG_HISTORY,
    retry
);

pub(super) fn edit(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(history) = parts.debug_history.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_history");
    };
    match history.last_user_input() {
        Some(input) => {
            history.load_composer(input);
            CommandResult::message(
                "Last message loaded into composer — edit and press Enter to resubmit",
            )
        }
        None => CommandResult::message("No previous message to edit"),
    }
}

pub(super) fn retry(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(history) = parts.debug_history.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_history");
    };
    match history.last_user_input() {
        Some(input) => {
            let undone = history.undo_conversation();
            let display_input = if input.len() > 50 {
                let truncate_at = input
                    .char_indices()
                    .take_while(|(i, _)| *i <= 50)
                    .last()
                    .map_or(0, |(i, _)| i);
                format!("{}...", &input[..truncate_at])
            } else {
                input.clone()
            };
            CommandResult::with_message_and_action(
                format!("Retrying: {display_input}"),
                DebugAction::ConversationUndo {
                    sync: undone.sync,
                    retry_input: Some(input),
                },
            )
        }
        None => CommandResult::error("No previous request to retry"),
    }
}

pub(super) fn diff(contexts: CommandContexts<'_>, _: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diff) = parts.debug_diff.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_diff");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    match diff_report(diff.diff(), presentation) {
        Ok(result) => result,
        Err(error) => CommandResult::error(error),
    }
}

fn diff_report(
    observation: DebugDiffObservation,
    presentation: &dyn CommandPresentationContext,
) -> Result<CommandResult, String> {
    Ok(match observation {
        DebugDiffObservation::GitUnavailable => {
            CommandResult::error(presentation.translate("cmd_diff_git_unavailable", &[])?)
        }
        DebugDiffObservation::NoBaseline => {
            CommandResult::message(presentation.translate("cmd_diff_no_baseline", &[])?)
        }
        DebugDiffObservation::Failed(error) => {
            CommandResult::message(presentation.translate("cmd_diff_failed", &[("error", &error)])?)
        }
        DebugDiffObservation::Output {
            names,
            stat,
            patch,
            patch_truncated,
        } => {
            if names.trim().is_empty() {
                return Ok(CommandResult::message(
                    presentation.translate("cmd_diff_no_changes", &[])?,
                ));
            }

            let files: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
            let file_count = files.len();
            let file_list = files.join("\n");

            // Detect rename entries (e.g. "foo -> bar") and exclude them
            // from the file-count header so the user sees only actual
            // modifications.
            let renamed_count = files.iter().filter(|f| f.contains(" -> ")).count();
            let summary = if renamed_count > 0 {
                presentation.translate(
                    "cmd_diff_changed_files_renamed",
                    &[
                        ("count", &file_count.to_string()),
                        ("renamed", &renamed_count.to_string()),
                        ("files", &file_list),
                    ],
                )?
            } else {
                presentation.translate(
                    "cmd_diff_changed_files",
                    &[("count", &file_count.to_string()), ("files", &file_list)],
                )?
            };

            let stat_str = stat.trim();
            let mut message = summary;
            if !stat_str.is_empty() {
                let stat_header = presentation.translate("cmd_diff_stat_header", &[])?;
                message.push_str(&format!("\n\n── {stat_header} ──\n"));
                message.push_str(stat_str);
            }
            if patch_truncated {
                message.push_str("\n\n");
                message.push_str(&presentation.translate("cmd_diff_truncated", &[])?);
            }
            if patch.trim().is_empty() {
                return Ok(CommandResult::message(message));
            }
            CommandResult::with_message_and_action(
                message,
                DebugAction::OpenDiffPager {
                    title: presentation.translate("cmd_diff_title", &[])?,
                    diff: patch,
                },
            )
        }
    })
}

/// A conversation undo that removed anything hands the truncated conversation
/// to the engine, which owns the model context (#6788).
pub fn conversation_result(
    undone: DebugConversationUndo,
    presentation: &dyn CommandPresentationContext,
) -> CommandResult {
    match conversation_report(undone, presentation) {
        Ok(result) => result,
        Err(error) => CommandResult::error(error),
    }
}

fn conversation_report(
    undone: DebugConversationUndo,
    presentation: &dyn CommandPresentationContext,
) -> Result<CommandResult, String> {
    if undone.removed > 0 {
        let message = presentation.translate(
            "cmd_undo_removed_messages",
            &[("count", &undone.removed.to_string())],
        )?;
        Ok(CommandResult::with_message_and_action(
            message,
            DebugAction::ConversationUndo {
                sync: undone.sync,
                retry_input: None,
            },
        ))
    } else {
        Ok(CommandResult::message(
            presentation.translate("cmd_undo_nothing_to_undo", &[])?,
        ))
    }
}

pub(super) fn undo(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(undo) = parts.debug_undo.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_undo");
    };
    let Some(history) = parts.debug_history.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_history");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    match undo_report(undo, history, presentation, arg) {
        Ok(result) => result,
        Err(error) => CommandResult::error(error),
    }
}

fn undo_report(
    undo: &mut dyn CommandDebugUndoContext,
    history: &mut dyn CommandDebugHistoryContext,
    presentation: &dyn CommandPresentationContext,
    arg: Option<&str>,
) -> Result<CommandResult, String> {
    // `force` accepts a request whose end was never recorded. Anything else
    // is a mistake to report, not a request to undo.
    let force = match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
        None => false,
        Some(arg) if arg.eq_ignore_ascii_case("force") => true,
        Some(other) => {
            return Ok(CommandResult::error(
                presentation.translate("cmd_undo_unknown_option", &[("option", other)])?,
            ));
        }
    };
    match undo.undo_files(force) {
        // No restore point of this conversation covers the request: say that
        // its files stayed as they are.
        DebugUndoOutcome::NoSnapshots
        | DebugUndoOutcome::NoSession
        | DebugUndoOutcome::NoOwnedSteps => conversation_only(
            history.undo_conversation(),
            presentation,
            "cmd_undo_files_not_reverted_note",
        ),
        DebugUndoOutcome::NoDifference => conversation_only(
            history.undo_conversation(),
            presentation,
            "cmd_undo_no_file_changes_note",
        ),
        DebugUndoOutcome::RepoUnavailable { workspace, error } => {
            let mut result = conversation_report(history.undo_conversation(), presentation)?;
            let note = format!(
                "{}\n{}",
                presentation.translate("cmd_undo_files_not_reverted_note", &[])?,
                presentation.translate(
                    "cmd_undo_snapshot_repo_unavailable",
                    &[
                        ("workspace", &workspace.display().to_string()),
                        ("error", &error),
                    ],
                )?
            );
            result.message = Some(match result.message.take() {
                Some(message) => format!("{message}\n{note}"),
                None => note,
            });
            Ok(result)
        }
        outcome => patch_report(outcome, presentation),
    }
}

/// A conversation-only undo, with what happened to the files stated under it.
fn conversation_only(
    undone: DebugConversationUndo,
    presentation: &dyn CommandPresentationContext,
    note_key: &str,
) -> Result<CommandResult, String> {
    let removed = undone.removed;
    let mut result = conversation_report(undone, presentation)?;
    if removed > 0 {
        let note = presentation.translate(note_key, &[])?;
        result.message = result
            .message
            .take()
            .map(|message| format!("{message}\n{note}"));
    }
    Ok(result)
}

/// Format snapshot outcomes without invoking host operations. Host tests also
/// use this for the file-only operation before the command's chat fallback.
pub fn patch_result(
    outcome: DebugUndoOutcome,
    presentation: &dyn CommandPresentationContext,
) -> CommandResult {
    match patch_report(outcome, presentation) {
        Ok(result) => result,
        Err(error) => CommandResult::error(error),
    }
}

fn patch_report(
    outcome: DebugUndoOutcome,
    presentation: &dyn CommandPresentationContext,
) -> Result<CommandResult, String> {
    match outcome {
        DebugUndoOutcome::RepoUnavailable { workspace, error } => {
            Ok(CommandResult::error(presentation.translate(
                "cmd_undo_snapshot_repo_unavailable",
                &[
                    ("workspace", &workspace.display().to_string()),
                    ("error", &error),
                ],
            )?))
        }
        DebugUndoOutcome::SnapshotPending => Ok(CommandResult::message(
            presentation.translate("cmd_undo_snapshot_pending", &[])?,
        )),
        DebugUndoOutcome::NoSnapshots => Ok(CommandResult::message(
            presentation.translate("cmd_undo_no_snapshots", &[])?,
        )),
        DebugUndoOutcome::NoSession => Ok(CommandResult::message(
            presentation.translate("cmd_undo_no_session", &[])?,
        )),
        DebugUndoOutcome::NoOwnedSteps => Ok(CommandResult::message(
            presentation.translate("cmd_undo_no_owned_steps", &[])?,
        )),
        DebugUndoOutcome::NoDifference => Ok(CommandResult::message(
            presentation.translate("cmd_undo_no_difference", &[])?,
        )),
        DebugUndoOutcome::OpenEnded => Ok(CommandResult::message(
            presentation.translate("cmd_undo_open_ended", &[])?,
        )),
        DebugUndoOutcome::CompareFailed(error) => Ok(CommandResult::error(
            presentation.translate("cmd_undo_compare_failed", &[("error", &error)])?,
        )),
        DebugUndoOutcome::SnapshotFailed(error) => Ok(CommandResult::error(
            presentation.translate("cmd_undo_snapshot_failed", &[("error", &error)])?,
        )),
        DebugUndoOutcome::ListFailed(error) => Ok(CommandResult::error(
            presentation.translate("cmd_undo_list_failed", &[("error", &error)])?,
        )),
        DebugUndoOutcome::RestoreBlocked(error) => Ok(CommandResult::message(error)),
        DebugUndoOutcome::RestoreFailed(error) => Ok(CommandResult::error(
            presentation.translate("cmd_undo_restore_failed", &[("error", &error)])?,
        )),
        DebugUndoOutcome::ChangedSince { paths } => Ok(CommandResult::message(
            presentation.translate("cmd_undo_changed_since", &[("paths", &paths.join(", "))])?,
        )),
        DebugUndoOutcome::Restored(restored) => {
            let summary = restored_summary(&restored, presentation)
                .unwrap_or_else(|error| restored_fallback(&restored, &error));
            Ok(CommandResult::with_message_and_action(
                summary,
                DebugAction::SyncSession(restored.sync),
            ))
        }
    }
}

fn restored_summary(
    restored: &DebugUndoRestored,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let lines: Vec<String> = restored
        .files
        .iter()
        .map(|file| {
            let verb = match file.action {
                DebugRestoreAction::Modified => {
                    presentation.translate("cmd_undo_action_restored", &[])?
                }
                // The request deleted it.
                DebugRestoreAction::Recreated => {
                    presentation.translate("cmd_undo_action_recreated", &[])?
                }
                // The request created it.
                DebugRestoreAction::Removed => {
                    presentation.translate("cmd_undo_action_removed", &[])?
                }
            };
            Ok(format!("  {verb} {}", file.path.display()))
        })
        .collect::<Result<_, String>>()?;
    let request = match &restored.request {
        Some(prompt) => presentation.translate("cmd_undo_request_named", &[("prompt", prompt)])?,
        None => presentation.translate("cmd_undo_request_last", &[])?,
    };
    let count = match restored.files.len() {
        1 => presentation.translate("cmd_undo_one_file_back", &[])?,
        files => presentation.translate("cmd_undo_files_back", &[("count", &files.to_string())])?,
    };
    let mut summary = presentation.translate(
        "cmd_undo_undid_summary",
        &[
            ("request", &request),
            ("count", &count),
            ("files", &lines.join("\n")),
        ],
    )?;
    if restored.forced {
        summary.push('\n');
        summary.push_str(&presentation.translate("cmd_undo_forced_note", &[])?);
    }
    summary.push('\n');
    summary.push_str(&presentation.translate(
        if restored.conversation_removed {
            "cmd_undo_conversation_removed"
        } else {
            "cmd_undo_conversation_kept"
        },
        &[],
    )?);
    if !restored.skipped.is_empty() {
        let skipped: Vec<String> = restored
            .skipped
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        summary.push('\n');
        summary.push_str(
            &presentation.translate("cmd_undo_left_in_place", &[("paths", &skipped.join(", "))])?,
        );
    }
    Ok(summary)
}

fn restored_fallback(restored: &DebugUndoRestored, error: &str) -> String {
    let mut report = format!("Undo was applied, but its report could not be formatted: {error}");
    if let Some(prompt) = &restored.request {
        report.push_str(&format!("\nRequest: {prompt}"));
    }
    if restored.files.is_empty() {
        report.push_str("\nNo files were changed.");
    } else {
        report.push_str("\nFiles it changed:");
    }
    for file in &restored.files {
        let verb = match file.action {
            DebugRestoreAction::Modified => "restored",
            DebugRestoreAction::Recreated => "brought back",
            DebugRestoreAction::Removed => "removed",
        };
        report.push_str(&format!("\n  {verb} {}", file.path.display()));
    }
    if restored.forced {
        report.push_str(
            "\nThe end of this request was not recorded, so edits made to these files since it started were also put back.",
        );
    }
    report.push_str(if restored.conversation_removed {
        "\nThe request and its reply were removed from the conversation."
    } else {
        "\nThe conversation was not changed."
    });
    if !restored.skipped.is_empty() {
        let skipped: Vec<String> = restored
            .skipped
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        report.push_str(&format!(
            "\nLeft in place, not reverted: {}",
            skipped.join(", ")
        ));
    }
    report
}
