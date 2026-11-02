//! Host operations moved out of `groups/debug/undo.rs` and `receipts.rs`.
//! Snapshot safety and authoritative state mutation remain here. The portable
//! commands receive typed observations/outcomes, never completed command text.
//! File undo is admitted by the asynchronous TUI owner before mutation.

use super::SharedCommandHost;
use crate::tui::app::App;
use crate::tui::history::HistoryCell;
use codewhale_command_contract::facets::*;
use codewhale_dependencies::{ExternalTool, Git};
use std::path::PathBuf;

pub(super) struct DebugOperationsAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl CommandDebugReceiptsContext for DebugOperationsAdapter<'_> {
    fn receipt(&self, turn: Option<&str>) -> Result<Receipt, DebugReceiptError> {
        let app = self.host.app.borrow();
        let approvals = match app.current_session_id.as_deref() {
            Some(id) => codewhale_engine::approval_log::ApprovalReceiptStore::default_location()
                .and_then(|store| store.load(id))
                .map_err(|error| DebugReceiptError::ApprovalLog(error.to_string()))?,
            None => Vec::new(),
        };
        let source = ReceiptSource {
            kind: SourceKind::Session,
            id: app
                .current_session_id
                .clone()
                .unwrap_or_else(|| "unsaved".to_string()),
            title: app.session_title.clone(),
            workspace: Some(app.workspace.display().to_string()),
            model: Some(app.model.clone()),
            started_at: Some(app.session_started_at),
            updated_at: None,
        };
        codewhale_engine::receipts::session_receipt(
            source,
            &app.api_messages,
            &approvals,
            &app.session_turn_outcomes,
            turn,
        )
        .map_err(|error| DebugReceiptError::Build(error.to_string()))
    }
}

impl CommandDebugChangeContext for DebugOperationsAdapter<'_> {
    fn change_projection(&self) -> DebugChangeProjection {
        let app = self.host.app.borrow();
        DebugChangeProjection {
            changelog: crate::whats_new::BUNDLED_CHANGELOG,
            is_english: app.ui_locale == codewhale_localization::Locale::En,
            translation_available: !app.offline_mode && !app.onboarding_needs_api_key,
            translation_target: app.ui_locale.translation_target_name(),
        }
    }
}

impl CommandDebugHistoryContext for DebugOperationsAdapter<'_> {
    fn last_user_input(&self) -> Option<String> {
        self.host
            .app
            .borrow()
            .history
            .iter()
            .rev()
            .find_map(|cell| match cell {
                HistoryCell::User { content } => Some(content.clone()),
                _ => None,
            })
    }
    fn load_composer(&mut self, input: String) {
        let mut app = self.host.app.borrow_mut();
        // A queued follow-up still open for editing would otherwise stay bound
        // to the composer and be overwritten, or sent in place of this edit.
        // Return it to the queue first — the same hand-back as Esc.
        if app.cancel_queued_draft_edit() {
            app.status_message = Some("Queued edit canceled; follow-up restored".to_string());
        }
        app.input = input;
        app.cursor_position = app.input.chars().count();
        app.edit_in_progress = true;
    }
    fn undo_conversation(&mut self) -> DebugConversationUndo {
        undo_conversation_for_engine(&mut self.host.app.borrow_mut())
    }
}

impl CommandDebugUndoContext for DebugOperationsAdapter<'_> {
    fn undo_files(&mut self, _force: bool) -> DebugUndoOutcome {
        DebugUndoOutcome::RestoreBlocked(
            "File undo requires the interactive Engine admission boundary; use /undo or /restore."
                .to_string(),
        )
    }
}

impl CommandDebugDiffContext for DebugOperationsAdapter<'_> {
    fn diff(&self) -> DebugDiffObservation {
        let app = self.host.app.borrow();
        if !Git::available() {
            return DebugDiffObservation::GitUnavailable;
        }
        match session_diff(&app) {
            Ok(Some(observation)) => observation,
            Ok(None) => workspace_git_diff(&app.workspace),
            Err(error) => DebugDiffObservation::Failed(error.to_string()),
        }
    }
}

/// Most patch text `/diff` hands to the pager. The file list and the stat
/// are never cut.
const MAX_DIFF_PATCH_BYTES: usize = 256 * 1024;

/// What changed in the workspace since this session's first restore point,
/// or `None` when the session has none here (no turn has run yet, or
/// snapshots are off for this workspace).
///
/// The restore points are the session's own record of where it started, so
/// this works the same in a folder that is not a git repository, and it
/// includes files created since. Reads only: no snapshot is taken and the
/// side repo is not created.
fn session_diff(app: &App) -> std::io::Result<Option<DebugDiffObservation>> {
    let Some(session_id) = app.current_session_id.as_deref() else {
        return Ok(None);
    };
    let Some(repo) = codewhale_snapshot::SnapshotRepo::open_existing(&app.workspace)? else {
        return Ok(None);
    };
    // Newest first, so the session's first restore point is the last match.
    let Some(start) = repo
        .list(usize::MAX)?
        .into_iter()
        .rev()
        .find(|snapshot| snapshot.session_id.as_deref() == Some(session_id))
    else {
        return Ok(None);
    };
    let changes = repo.work_tree_changes_since(&start.tree, MAX_DIFF_PATCH_BYTES)?;
    Ok(Some(DebugDiffObservation::Output {
        names: changes.names,
        stat: changes.stat,
        patch: changes.patch,
        patch_truncated: changes.patch_truncated,
    }))
}

/// The workspace repository's own uncommitted changes: what `/diff` shows
/// before the session has a restore point to compare against.
fn workspace_git_diff(workspace: &std::path::Path) -> DebugDiffObservation {
    let git = |args: &[&str]| Git::output(args, workspace);
    // Outside a repository `git diff` prints nothing on stdout, which would
    // read as "no changes".
    match git(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(probe)
            if probe.status.success()
                && String::from_utf8_lossy(&probe.stdout).trim() == "true" => {}
        Ok(_) => return DebugDiffObservation::NoBaseline,
        Err(error) => return DebugDiffObservation::Failed(error.to_string()),
    }
    let names = git(&["diff", "--name-only"]);
    let stat = git(&["diff", "--stat"]);
    let patch = git(&["diff", "--no-color", "--no-ext-diff", "--no-textconv"]);
    match (names, stat, patch) {
        (Ok(names), Ok(stat), Ok(patch)) => {
            let (patch, patch_truncated) = codewhale_snapshot::repo::truncate_at_char_boundary(
                &String::from_utf8_lossy(&patch.stdout),
                MAX_DIFF_PATCH_BYTES,
            );
            DebugDiffObservation::Output {
                names: String::from_utf8_lossy(&names.stdout).into_owned(),
                stat: String::from_utf8_lossy(&stat.stdout).into_owned(),
                patch,
                patch_truncated,
            }
        }
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
            DebugDiffObservation::Failed(error.to_string())
        }
    }
}

/// Prepare the rollback; the UI action owns Engine acknowledgement and save.
/// The last real user boundary includes following tool results/runtime notes.
pub(in crate::commands) fn undo_conversation_for_engine(app: &mut App) -> DebugConversationUndo {
    let removed = app
        .history
        .iter()
        .rposition(|cell| matches!(cell, HistoryCell::User { .. }))
        .map_or(0, |index| app.history.len() - index);
    let mut sync = session_sync_payload(app);
    if let Some(index) = crate::tui::ui::backtrack_api_cut_index(app, 0) {
        sync.messages.truncate(index);
    } else if !sync.messages.is_empty() {
        return DebugConversationUndo { removed: 0, sync };
    }
    DebugConversationUndo { removed, sync }
}

fn session_sync_payload(app: &App) -> SessionSyncPayload {
    SessionSyncPayload {
        session_id: app.current_session_id.clone(),
        messages: app.api_messages.as_ref().clone(),
        system_prompt: app.system_prompt.clone(),
        model: app.model.clone(),
        workspace: app.workspace.clone(),
        mode: super::to_command_mode(app.mode),
    }
}

/// Deepest fork chain [`snapshot_owners_from`] follows. A chain this long is
/// already unusual; the bound only stops a corrupt lineage from looping.
const MAX_FORK_ANCESTORS: usize = 32;

/// A session whose restore points this conversation owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::commands) struct SnapshotOwner {
    /// Session tag the snapshots carry.
    pub(in crate::commands) session_id: String,
    /// Newest snapshot time (Unix seconds) owned from this session: `None`
    /// for the current session, the fork time for a session it was forked
    /// from. The source keeps working after the fork, and its later
    /// snapshots are not the fork's.
    pub(in crate::commands) until: Option<i64>,
}

impl SnapshotOwner {
    fn owns(&self, snapshot: &codewhale_snapshot::Snapshot) -> bool {
        snapshot.session_id.as_deref() == Some(self.session_id.as_str())
            && self.until.is_none_or(|until| snapshot.timestamp <= until)
    }
}

/// The sessions whose restore points `/undo` may use: the current session,
/// and for a fork each session it was forked from, up to the fork. A fork
/// copies its source's turns, so the snapshots those turns took (tagged
/// with the source's id) are the fork's too, as the Runtime's thread-owned
/// restore points are (#6621).
///
/// Lineage the saved sessions cannot prove ends the chain: fewer owners
/// means fewer restorable steps, never someone else's.
fn snapshot_owners_from(
    session_id: Option<String>,
    current_metadata: Option<codewhale_engine::session_manager::SessionMetadata>,
) -> Vec<SnapshotOwner> {
    let Some(current) = session_id else {
        return Vec::new();
    };
    let manager = codewhale_engine::session_manager::SessionManager::default_location().ok();
    let load = |id: &str| {
        manager
            .as_ref()
            .and_then(|manager| manager.load_session_metadata_by_id(id).ok())
    };
    let mut metadata = current_metadata
        .filter(|metadata| metadata.id == current)
        .or_else(|| load(&current));
    let mut owners = vec![SnapshotOwner {
        session_id: current,
        until: None,
    }];
    while let Some(child) = metadata.take() {
        let Some(parent) = child.parent_session_id.clone() else {
            break;
        };
        if owners.len() > MAX_FORK_ANCESTORS
            || owners.iter().any(|owner| owner.session_id == parent)
        {
            break;
        }
        let forked_at = child.created_at.timestamp();
        let until = owners
            .last()
            .and_then(|owner| owner.until)
            .map_or(forked_at, |child_until| child_until.min(forked_at));
        metadata = load(&parent);
        owners.push(SnapshotOwner {
            session_id: parent,
            until: Some(until),
        });
    }
    owners
}

pub(crate) struct UndoRangeInput {
    workspace: PathBuf,
    session_id: Option<String>,
    metadata: Option<codewhale_engine::session_manager::SessionMetadata>,
    requests: Vec<Option<String>>,
    messages: std::sync::Arc<Vec<codewhale_models::Message>>,
    boundaries: Vec<usize>,
    first_request: usize,
}

pub(crate) struct UndoRangePlan {
    workspace: PathBuf,
    turns: Vec<codewhale_engine::runtime_threads::manager::DroppedTurnSnapshots>,
}

pub(crate) fn undo_range_input(app: &App, cut: usize) -> anyhow::Result<UndoRangeInput> {
    let total = app
        .history
        .iter()
        .filter(|cell| matches!(cell, HistoryCell::User { .. }))
        .count();
    let boundaries = (0..total).rev().map(|depth| crate::tui::ui::backtrack_api_cut_index(app, depth))
        .collect::<Option<Vec<_>>>().ok_or_else(|| anyhow::anyhow!("The displayed requests no longer match the Engine conversation; nothing was undone."))?;
    anyhow::ensure!(
        boundaries.windows(2).all(|pair| pair[0] < pair[1]),
        "The displayed request order changed; nothing was undone."
    );
    let first_request = boundaries
        .iter()
        .position(|index| *index == cut)
        .ok_or_else(|| {
            anyhow::anyhow!("The selected request is no longer present; nothing was undone.")
        })?;
    let requests = boundaries
        .iter()
        .map(|index| {
            let message = &app.api_messages[*index];
            let metadata = codewhale_models::turn_classify::turn_metadata_text(message)
                .map(|(index, _)| index);
            let mut content = message
                .content
                .iter()
                .enumerate()
                .filter(|(index, _)| Some(*index) != metadata);
            match (content.next(), content.next()) {
                (Some((_, codewhale_models::ContentBlock::Text { text, .. })), None) => {
                    Some(text.clone())
                }
                _ => None,
            }
        })
        .collect();
    Ok(UndoRangeInput {
        workspace: app.workspace.clone(),
        session_id: app.current_session_id.clone(),
        metadata: app.current_session_metadata.clone(),
        requests,
        messages: app.api_messages.clone(),
        boundaries,
        first_request,
    })
}

pub(crate) fn plan_undo_range(input: UndoRangeInput) -> anyhow::Result<UndoRangePlan> {
    use codewhale_engine::core::turn::{parse_snapshot_label, snapshot_label_prompt_snippet};
    use codewhale_engine::tools::spec::ToolSpec;
    use codewhale_models::{ContentBlock, ToolCallKey};
    use codewhale_snapshot::{WorkspaceSnapshotKind, WorkspaceSnapshotRef};
    let owners = snapshot_owners_from(input.session_id, input.metadata);
    let mut repo = None;
    let mut snapshots = Vec::new();
    let mut turns = Vec::new();
    let mut previous_before = None;
    let mut execution_ids = std::collections::BTreeSet::new();
    let boundaries: Vec<_> = input
        .boundaries
        .iter()
        .copied()
        .chain(std::iter::once(input.messages.len()))
        .collect();
    for turn_index in input.first_request..input.requests.len() {
        let messages = &input.messages[boundaries[turn_index]..boundaries[turn_index + 1]];
        let has_tools = messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolUse { .. }
                        | ContentBlock::ServerToolUse { .. }
                        | ContentBlock::ToolResult { .. }
                        | ContentBlock::ToolSearchToolResult { .. }
                        | ContentBlock::CodeExecutionToolResult { .. }
                )
            });
        if !has_tools {
            turns.push(
                codewhale_engine::runtime_threads::manager::DroppedTurnSnapshots {
                    turn_id: format!("conversation-{}", boundaries[turn_index]),
                    may_change_files: false,
                    snapshots: Vec::new(),
                    declared_writes: Vec::new(),
                    unrun_tool_calls: Default::default(),
                },
            );
            continue;
        }
        let mut calls = std::collections::BTreeMap::new();
        let mut declared_writes = Vec::new();
        let mut all_readonly = true;
        for message in messages {
            for block in &message.content {
                if let ContentBlock::ToolUse {
                    execution_id,
                    name,
                    input,
                    ..
                } = block
                {
                    let id = execution_id.as_deref().filter(|id| !id.trim().is_empty())
                        .ok_or_else(|| anyhow::anyhow!("A selected tool call has no recorded host execution identifier; nothing was undone. Use /restore or conversation backtracking."))?;
                    anyhow::ensure!(
                        execution_ids.insert(id),
                        "Selected tool calls have a repeated host execution identifier; nothing was undone."
                    );
                    all_readonly &= match name.as_str() {
                        "read" => {
                            codewhale_engine::tools::file_tool::ReadTool.is_read_only_for(input)
                        }
                        "read_file" => {
                            codewhale_engine::tools::file::ReadFileTool.is_read_only_for(input)
                        }
                        "list_dir" => {
                            codewhale_engine::tools::file::ListDirTool.is_read_only_for(input)
                        }
                        "File" => codewhale_engine::tools::file_tool::FileTool::with_patch("File")
                            .is_read_only_for(input),
                        _ => false,
                    };
                    let writes =
                        codewhale_engine::core::engine::file_write_tool_target_paths(name, input);
                    declared_writes.extend(writes.iter().flatten().cloned());
                    calls.insert(id, writes);
                }
            }
        }
        let mut completed = std::collections::HashSet::new();
        for block in messages.iter().flat_map(|message| &message.content) {
            match block {
                ContentBlock::ToolResult { .. } => match block.tool_call_key() {
                    Some(ToolCallKey::Execution(id)) if calls.contains_key(id) => {
                        all_readonly &= completed.insert(id);
                    }
                    _ => all_readonly = false,
                },
                ContentBlock::ServerToolUse { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. } => all_readonly = false,
                _ => {}
            }
        }
        if all_readonly && !calls.is_empty() && completed.len() == calls.len() {
            turns.push(
                codewhale_engine::runtime_threads::manager::DroppedTurnSnapshots {
                    turn_id: format!("read-only-{}", boundaries[turn_index]),
                    may_change_files: false,
                    snapshots: Vec::new(),
                    declared_writes: Vec::new(),
                    unrun_tool_calls: Default::default(),
                },
            );
            continue;
        }
        anyhow::ensure!(
            !owners.is_empty(),
            "This conversation has no owned restore points; nothing was undone. Use /restore or conversation backtracking."
        );
        if repo.is_none() {
            anyhow::ensure!(
                codewhale_snapshot::wait_for_pending_post_turn_snapshots(POST_TURN_SNAPSHOT_WAIT),
                "A restore point is still being saved; nothing was undone. Try again when it finishes."
            );
            let opened = codewhale_snapshot::SnapshotRepo::open_existing(&input.workspace)?
                .ok_or_else(|| anyhow::anyhow!("This request has no recorded file restore points; nothing was undone. Use /restore or conversation backtracking."))?;
            snapshots = opened.list(usize::MAX)?;
            repo = Some(opened);
        }
        let repo = repo.as_ref().unwrap();
        let mut window = None;
        for (id, writes) in &calls {
            let label = format!("tool:{id}");
            let matches: Vec<_> = snapshots
                .iter()
                .enumerate()
                .filter(|(_, snapshot)| {
                    snapshot.label == label && owners.iter().any(|owner| owner.owns(snapshot))
                })
                .map(|(index, _)| index)
                .collect();
            if matches.is_empty() {
                anyhow::ensure!(
                    writes.is_none(),
                    "A file-writing invocation has no owned restore point; nothing was undone. Use /restore or conversation backtracking."
                );
                continue;
            }
            anyhow::ensure!(
                matches.len() == 1,
                "A host execution has repeated restore points; nothing was undone."
            );
            let tool = matches[0];
            let before = (tool + 1..snapshots.len()).find(|index| is_restore_point_label(&snapshots[*index].label))
                .ok_or_else(|| anyhow::anyhow!("The file invocation has no complete request start; nothing was undone. Use /restore."))?;
            let after = (0..tool).rev().find(|index| is_restore_point_label(&snapshots[*index].label))
                .ok_or_else(|| anyhow::anyhow!("The file invocation has no complete request end; nothing was undone. Use /restore."))?;
            anyhow::ensure!(
                is_undo_step_label(&snapshots[before].label)
                    && snapshots[after].label.starts_with("post-turn:"),
                "The file invocation has an incomplete request boundary; nothing was undone. Use /restore."
            );
            anyhow::ensure!(
                window.is_none_or(|previous| previous == (before, after)),
                "The request's host executions do not share one completed window; nothing was undone."
            );
            window = Some((before, after));
        }
        let (oldest, newest) = window.ok_or_else(|| anyhow::anyhow!("The request has no owned file-writer receipt; nothing was undone. Use conversation backtracking, or /restore for an explicit file restore."))?;
        let before = &snapshots[oldest];
        let after = &snapshots[newest];
        let pre = parse_snapshot_label(&before.label);
        let post = parse_snapshot_label(&after.label);
        let snippet = input.requests[turn_index].as_deref().and_then(snapshot_label_prompt_snippet)
            .ok_or_else(|| anyhow::anyhow!("The selected multipart request cannot be checked against its file restore point; nothing was undone. Use /restore or conversation backtracking."))?;
        anyhow::ensure!(
            pre.prompt_snippet.as_deref().map(str::trim_end) == Some(snippet.trim_end()),
            "The file invocation's request text does not match the selected request; nothing was undone."
        );
        anyhow::ensure!(
            pre.seq.is_some() && pre.seq == post.seq && before.session_id == after.session_id,
            "The selected request has an incomplete restore point; nothing was undone. Use /restore."
        );
        anyhow::ensure!(
            snapshots[newest..=oldest]
                .iter()
                .all(|snapshot| snapshot.session_id == before.session_id),
            "The request's restore points mix different sessions; nothing was undone. Use /restore or conversation backtracking."
        );
        anyhow::ensure!(
            newest < oldest && previous_before.is_none_or(|previous| oldest < previous),
            "Selected file invocations do not follow the conversation; nothing was undone."
        );
        let range_oldest = previous_before.unwrap_or(oldest);
        anyhow::ensure!(
            snapshots[newest..=range_oldest]
                .iter()
                .all(|snapshot| owners.iter().any(|owner| owner.owns(snapshot))),
            "Another conversation recorded workspace changes inside the selected range; nothing was undone."
        );
        previous_before = Some(oldest);
        let mut receipts = Vec::new();
        let mut previous_tree = None;
        for snapshot in snapshots[newest..=oldest].iter().rev() {
            let (kind, call) = if snapshot.id == before.id {
                (WorkspaceSnapshotKind::PreTurn, None)
            } else if snapshot.id == after.id {
                (WorkspaceSnapshotKind::PostTurn, None)
            } else if let Some(call) = snapshot.label.strip_prefix("tool:") {
                (WorkspaceSnapshotKind::Tool, Some(call))
            } else if let Some(call) = snapshot.label.strip_prefix("post-tool:") {
                (WorkspaceSnapshotKind::PostTool, Some(call))
            } else {
                anyhow::bail!(
                    "An unrecognized restore point lies inside the selected request; nothing was undone."
                )
            };
            let writes = call.map(|id| calls.get(id).ok_or_else(|| anyhow::anyhow!("A restore point cannot be bound to the selected request's recorded host execution; nothing was undone. Use /restore or conversation backtracking."))).transpose()?.cloned().flatten();
            let changed_paths = previous_tree
                .as_ref()
                .map(|previous| repo.changed_paths_between(previous, &snapshot.tree))
                .transpose()?
                .map(|paths| {
                    paths
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect()
                });
            receipts.push(WorkspaceSnapshotRef {
                kind,
                snapshot_id: snapshot.id.as_str().to_string(),
                tree_id: snapshot.tree.as_str().to_string(),
                session_id: snapshot.session_id.clone().unwrap(),
                tool_call_id: call.map(str::to_owned),
                write_paths: writes,
                changed_paths,
            });
            previous_tree = Some(snapshot.tree.clone());
        }
        for (index, receipt) in receipts
            .iter()
            .enumerate()
            .filter(|(_, receipt)| receipt.kind == WorkspaceSnapshotKind::Tool)
        {
            anyhow::ensure!(
                receipts[index + 1..]
                    .iter()
                    .any(|later| later.kind == WorkspaceSnapshotKind::PostTool
                        && later.tool_call_id == receipt.tool_call_id),
                "The request has no closed per-tool restore point; nothing was undone. Use /restore or conversation backtracking."
            );
        }
        turns.push(
            codewhale_engine::runtime_threads::manager::DroppedTurnSnapshots {
                turn_id: before.id.as_str().to_string(),
                may_change_files: true,
                snapshots: receipts,
                declared_writes,
                unrun_tool_calls: Default::default(),
            },
        );
    }
    Ok(UndoRangePlan {
        workspace: input.workspace,
        turns,
    })
}

pub(crate) fn restore_undo_range(plan: UndoRangePlan) -> anyhow::Result<String> {
    if plan.turns.iter().all(|turn| !turn.may_change_files) {
        return Ok("Workspace files were left unchanged.".to_string());
    }
    let result = crate::runtime_api::patch_undo_workspace_files(&plan.workspace, &plan.turns, true)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    Ok(result
        .summary
        .unwrap_or_else(|| "No workspace file changes needed restoring.".to_string()))
}

/// Label a `/undo` step starts at: before one request (a turn). `/undo`,
/// `/retry`, Esc Esc and the Runtime's `patch-undo` all go back by request;
/// the per-tool `tool:` restore points stay available to `/restore`.
fn is_undo_step_label(label: &str) -> bool {
    label.starts_with("pre-turn:")
}

/// Labels of the restore points that bound a request. A step runs from its
/// `pre-turn:` point to the next one of these the conversation owns.
fn is_restore_point_label(label: &str) -> bool {
    is_undo_step_label(label) || label.starts_with("post-turn:")
}

/// How long `/undo` waits for a post-turn snapshot still being written.
const POST_TURN_SNAPSHOT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why workspace files may not be rolled back right now, if they may not.
///
/// A running turn is reading and writing this workspace: restoring files
/// under it discards the turn's in-flight work and leaves the model's view of
/// the files wrong. `/undo` and `/restore` refuse while one is active, like
/// the Runtime's restore routes.
pub(in crate::commands) fn active_turn_restore_refusal(app: &App) -> Option<String> {
    let turn_active = app.is_loading
        || app.is_compacting
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"));
    turn_active.then(|| {
        "A turn is still running in this workspace, so files were not restored and nothing was changed. Wait for it to finish, or press Esc to stop it, then run the command again."
            .to_string()
    })
}
