//! Host operations moved out of `groups/debug/undo.rs` and `receipts.rs`.
//! Snapshot safety and authoritative state mutation remain here. The portable
//! commands receive typed observations/outcomes, never completed command text.
//! Existing synchronous dispatch timing is preserved; this does not introduce
//! an asynchronous I/O execution model.

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
    fn undo_files(&mut self, force: bool) -> DebugUndoOutcome {
        undo_files(&mut self.host.app.borrow_mut(), force)
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
    if let Some(index) = sync.messages.iter().rposition(|message| {
        !matches!(
            codewhale_models::turn_classify::classify_user_turn_prompt(message),
            codewhale_models::turn_classify::UserTurnPromptKind::NotPrompt
        )
    }) {
        sync.messages.truncate(index);
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

/// Full text of this conversation's requests, oldest first. Missing text is
/// retained: an image-only request is a boundary, not a request to skip.
fn request_prompts(app: &App) -> Vec<Option<String>> {
    if app.api_messages.is_empty() {
        return app
            .history
            .iter()
            .filter_map(|cell| match cell {
                HistoryCell::User { content } => Some(Some(content.clone())),
                _ => None,
            })
            .collect();
    }
    app.api_messages
        .iter()
        .filter(|message| {
            !matches!(
                codewhale_models::turn_classify::classify_user_turn_prompt(message),
                codewhale_models::turn_classify::UserTurnPromptKind::NotPrompt
            )
        })
        .map(|message| {
            let meta_index = codewhale_models::turn_classify::turn_metadata_text(message)
                .map(|(index, _)| index);
            let mut content = message
                .content
                .iter()
                .enumerate()
                .filter(|(index, _)| Some(*index) != meta_index);
            // A text label cannot identify the rest of a multipart request.
            match (content.next(), content.next()) {
                (Some((_, codewhale_models::ContentBlock::Text { text, .. })), None) => {
                    Some(text.clone())
                }
                _ => None,
            }
        })
        .collect()
}

type RequestPositionResult = Result<Option<bool>, Box<DebugUndoOutcome>>;

/// Match only unique prompt snippets. Display snippets are not durable
/// request identities: repeated snippets or multipart prompts cannot
/// authorize a combined file/conversation rollback. `/restore` remains explicit.
fn request_is_last(
    requests: &[Option<String>],
    owned_request_snippets: &[Option<String>],
    label: &str,
) -> RequestPositionResult {
    use codewhale_engine::core::turn::{parse_snapshot_label, snapshot_label_prompt_snippet};
    if requests.is_empty() {
        return Ok(None);
    }
    let refusal = || {
        Box::new(DebugUndoOutcome::RestoreBlocked(
            concat!(
                "Nothing was undone. This restore point cannot be matched safely to a request ",
                "in the conversation. Use /restore to choose a restore point explicitly.",
            )
            .to_string(),
        ))
    };
    if requests.iter().any(Option::is_none) {
        return Err(refusal());
    }
    let requests: Vec<_> = requests
        .iter()
        .map(|prompt| {
            prompt
                .as_deref()
                .and_then(snapshot_label_prompt_snippet)
                .map(|snippet| snippet.trim_end().to_string())
                .filter(|snippet| !snippet.is_empty())
        })
        .collect();
    let request = parse_snapshot_label(label).prompt_snippet;
    let Some(prompt) = request.as_deref().map(str::trim_end).filter(|prompt| {
        !prompt.is_empty() && snapshot_label_prompt_snippet(prompt).as_deref() == Some(*prompt)
    }) else {
        return Err(refusal());
    };
    let matches = requests
        .iter()
        .filter(|candidate| candidate.as_deref() == Some(prompt))
        .count();
    if matches != 1
        || owned_request_snippets
            .iter()
            .filter(|candidate| candidate.as_deref().map(str::trim_end) == Some(prompt))
            .count()
            > 1
    {
        // Snapshots outlive conversation rewinds, so checking only the held
        // prompts would still confuse an older occurrence with the latest.
        return Err(refusal());
    }
    Ok(Some(
        requests.last().and_then(Option::as_deref) == Some(prompt),
    ))
}

/// Deepest fork chain [`snapshot_owners`] follows. A chain this long is
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
pub(in crate::commands) fn snapshot_owners(app: &App) -> Vec<SnapshotOwner> {
    let Some(current) = app.current_session_id.clone() else {
        return Vec::new();
    };
    let manager = codewhale_engine::session_manager::SessionManager::default_location().ok();
    let load = |id: &str| {
        manager
            .as_ref()
            .and_then(|manager| manager.load_session_metadata_by_id(id).ok())
    };
    let mut metadata = app
        .current_session_metadata
        .clone()
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

/// One `/undo` step, planned but not applied.
pub(in crate::commands) struct UndoStep {
    /// Restore point the step started at.
    pub(in crate::commands) target: codewhale_snapshot::Snapshot,
    /// Tree the step ended at: the next restore point this conversation
    /// owns, or, for the newest step, a snapshot of the workspace as it is
    /// now. Trees, not commit ids, because a prune rewrites commit ids.
    pub(in crate::commands) end: codewhale_snapshot::SnapshotId,
    /// The paths the step changed that are still as it left them.
    pub(in crate::commands) restore: Vec<PathBuf>,
    /// Changed paths `/undo` leaves in place because they are not regular
    /// files (a symlink, a directory, a submodule), in this step or in a
    /// newer one it walked past.
    pub(in crate::commands) skipped: Vec<PathBuf>,
    /// The `pre-restore:` snapshot planning took of the workspace, when the
    /// step ends now; the restore reuses it as its safety backup.
    pub(in crate::commands) backup: Option<codewhale_snapshot::SnapshotId>,
    /// The step has no recorded end and runs only because the user typed
    /// `force`: it also puts back edits made since the request started.
    pub(in crate::commands) forced: bool,
    /// The validated request boundary, captured before any backup is taken.
    request_is_last: Option<bool>,
}

/// Find the newest step of `snapshots` (newest first) that `owners` own and
/// that is not undone yet, and the paths undoing it restores.
///
/// A step is scoped to the paths that changed between its restore point and
/// the next one: edits to any other file (the user's, another session's)
/// are never touched. A path the step changed that changed again since is
/// refused rather than overwritten. A step whose paths are all back at its
/// restore point is already undone, so `/undo` walks back one request at a
/// time. A changed path that is not a regular file is
/// left in place and reported (file-scoped restore never writes symlinks or
/// directories); it does not block the step's other paths or older steps.
///
/// Trust is never asked. A step that runs from a request's `pre-turn:` point
/// to that same request's `post-turn:` point puts back, inside the
/// workspace, only what changed while that request ran, and only where the
/// file is still as the request left it: the reverse of writes the session
/// already made. Any other step has no recorded end (the request never
/// finished, or its post-turn snapshot is missing), so it also takes in
/// whatever was edited since; that one is refused unless the user typed
/// `force`.
///
/// `request_position` validates each non-no-op candidate before any backup or
/// restore write and says whether it names the last held request. An earlier held request means
/// the conversation still holds a request newer than this restore point. Such a
/// request left no file change to undo and comes off first (the caller's
/// conversation-only undo), so the conversation never keeps a request whose files went back,
/// and a refusal about an older request never blocks taking a newer one off.
///
/// Planning writes nothing, except when the newest step ends now: the
/// workspace is then snapshotted, and only with `force`.
///
/// Known limits: the TUI records no per-tool receipts (the Runtime's
/// `post-tool:` spans and declared write paths), so a step owns everything
/// that changed between its restore point and the next one this
/// conversation owns, including a write another session made in that window.
/// The newest step, when no later restore point exists yet (the turn is still
/// running, or its post-turn snapshot failed), ends at the workspace as it
/// is now, so an edit made since the step's restore point counts as the
/// step's. [`undo_files`] first waits for a post-turn snapshot this process
/// is still taking, so this is not the case right after a turn.
// The planner is host-internal. Box only its uncommon early outcome to keep
// Result small; the public facet still returns an owned data-only value.
fn plan_undo_step(
    repo: &codewhale_snapshot::SnapshotRepo,
    snapshots: Vec<codewhale_snapshot::Snapshot>,
    owners: &[SnapshotOwner],
    force: bool,
    request_position: &dyn Fn(&str) -> RequestPositionResult,
) -> Result<UndoStep, Box<DebugUndoOutcome>> {
    let owned: Vec<codewhale_snapshot::Snapshot> = snapshots
        .into_iter()
        .filter(|snapshot| is_restore_point_label(&snapshot.label))
        .filter(|snapshot| owners.iter().any(|owner| owner.owns(snapshot)))
        .collect();
    if !owned
        .iter()
        .any(|snapshot| is_undo_step_label(&snapshot.label))
    {
        return Err(Box::new(DebugUndoOutcome::NoOwnedSteps));
    }

    let compare_failed = |error: std::io::Error| DebugUndoOutcome::CompareFailed(error.to_string());
    // `InvalidInput` from a path comparison: the path is not a regular file
    // (or not a safe workspace path) on one side, so it is left alone.
    let unrestorable = |error: &std::io::Error| error.kind() == std::io::ErrorKind::InvalidInput;

    let mut skipped: Vec<PathBuf> = Vec::new();
    for (index, target) in owned.iter().enumerate() {
        if !is_undo_step_label(&target.label) {
            continue;
        }
        let mut backup = None;
        let bounded = index.checked_sub(1).is_some_and(|newer| {
            let start = codewhale_engine::core::turn::parse_snapshot_label(&target.label);
            let end = codewhale_engine::core::turn::parse_snapshot_label(&owned[newer].label);
            end.kind == "post-turn" && end.seq.is_some() && end.seq == start.seq
        });
        let end = match index.checked_sub(1) {
            Some(newer) => owned[newer].tree.clone(),
            // The newest step has no later restore point (the turn is still
            // running, stopped early, or its post-turn snapshot has not
            // landed): the workspace now is the only record of its end.
            None => {
                if repo
                    .work_tree_matches_snapshot(&target.tree)
                    .map_err(compare_failed)?
                {
                    continue;
                }
                // An open-ended step can write its backup while planning:
                // establish identity before force can take that snapshot.
                let request_is_last = request_position(&target.label)?;
                if request_is_last == Some(false) {
                    return Err(Box::new(DebugUndoOutcome::NoDifference));
                }
                if !force {
                    return Err(Box::new(DebugUndoOutcome::OpenEnded));
                }
                let short = &target.id.as_str()[..target.id.as_str().len().min(12)];
                let taken = repo
                    .take_snapshot(&format!("pre-restore:{short}"), None)
                    .map_err(|error| DebugUndoOutcome::SnapshotFailed(error.to_string()))?;
                backup = Some(taken.id);
                taken.tree
            }
        };
        let changed = repo
            .changed_paths_between(&target.tree, &end)
            .map_err(compare_failed)?;
        let mut restore = Vec::new();
        let mut changed_since = Vec::new();
        'paths: for path in changed {
            match repo.path_matches_snapshot(&end, &path) {
                // Still as the step left it. Comparing the step's start too
                // proves it holds a regular file (or nothing) to restore.
                Ok(true) => match repo.path_same_in_snapshots(&target.tree, &end, &path) {
                    Ok(_) => {
                        restore.push(path);
                        continue;
                    }
                    Err(error) if unrestorable(&error) => {
                        skipped.push(path);
                        continue;
                    }
                    Err(error) => return Err(Box::new(compare_failed(error))),
                },
                Ok(false) => {}
                Err(error) if unrestorable(&error) => {
                    skipped.push(path);
                    continue;
                }
                Err(error) => return Err(Box::new(compare_failed(error))),
            }
            // Back at the step's start, or at an older restore point that an
            // earlier `/undo` walked it back to: already undone.
            for older in &owned[index..] {
                match repo.path_matches_snapshot(&older.tree, &path) {
                    Ok(true) => continue 'paths,
                    Ok(false) => {}
                    Err(error) if unrestorable(&error) => {
                        skipped.push(path);
                        continue 'paths;
                    }
                    Err(error) => return Err(Box::new(compare_failed(error))),
                }
            }
            changed_since.push(path.display().to_string());
        }
        if restore.is_empty() && changed_since.is_empty() {
            // Already undone, changed nothing, or changed only paths `/undo`
            // cannot restore: keep walking back.
            continue;
        }
        // No-op and already-undone snapshots outlive conversation rewinds.
        // Only a candidate with remaining changes needs request identity.
        let request_is_last = request_position(&target.label)?;
        if request_is_last == Some(false) {
            return Err(Box::new(DebugUndoOutcome::NoDifference));
        }
        if !changed_since.is_empty() {
            return Err(Box::new(DebugUndoOutcome::ChangedSince {
                paths: changed_since,
            }));
        }
        if !bounded && !force {
            return Err(Box::new(DebugUndoOutcome::OpenEnded));
        }
        skipped.sort();
        skipped.dedup();
        return Ok(UndoStep {
            target: target.clone(),
            end,
            restore,
            skipped,
            backup,
            forced: !bounded,
            request_is_last,
        });
    }
    Err(Box::new(DebugUndoOutcome::NoDifference))
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

/// Undo the most recent request: its file changes and its messages.
///
/// Opens the side-git snapshot repo and finds the newest `pre-turn:*` restore
/// point this conversation owns (see [`snapshot_owners`]) whose step is not
/// undone yet, then restores only the files that request changed (see
/// [`plan_undo_step`]), whichever tool or shell command changed them.
///
/// The conversation moves with the files. The step's request is matched to
/// the conversation by the prompt its label carries: when it is the last
/// request, that request and its reply are removed; when newer requests are
/// still in the conversation, nothing is restored and the caller's
/// conversation-only undo takes the newest one off first; when it is no
/// longer in the conversation (Esc Esc rewound past it), only its files go
/// back. The outcome says which happened.
///
/// Trust mode and Full Access are not consulted: the restore is confined to
/// regular files inside the workspace that the request itself changed.
/// `force` (the user typed `/undo force`) accepts a request with no recorded
/// end, never an ambiguous request identity.
pub(in crate::commands) fn undo_files(app: &mut App, force: bool) -> DebugUndoOutcome {
    if let Some(refusal) = active_turn_restore_refusal(app) {
        return DebugUndoOutcome::RestoreBlocked(refusal);
    }
    let workspace = app.workspace.clone();

    let repo = match codewhale_snapshot::SnapshotRepo::open_or_init(&workspace) {
        Ok(r) => r,
        Err(e) => {
            return DebugUndoOutcome::RepoUnavailable {
                workspace,
                error: e.to_string(),
            };
        }
    };

    // A post-turn snapshot this process is still taking is the newest step's
    // end: without it, every edit since the step's restore point would count
    // as the step's.
    if !codewhale_snapshot::wait_for_pending_post_turn_snapshots(POST_TURN_SNAPSHOT_WAIT) {
        return DebugUndoOutcome::SnapshotPending;
    }

    // The whole store: an older restore point that is still stored must not
    // be mistaken for a pruned one.
    let snapshots = match repo.list(usize::MAX) {
        Ok(s) => s,
        Err(e) => {
            return DebugUndoOutcome::ListFailed(e.to_string());
        }
    };

    if snapshots.is_empty() {
        return DebugUndoOutcome::NoSnapshots;
    }

    // Automatic file rollback is allowed only when ownership is provable.
    // Untagged legacy snapshots and snapshots from another conversation may
    // describe unrelated user work in this same workspace, so fail closed
    // and let the command dispatcher fall back to conversation-only undo.
    let owners = snapshot_owners(app);
    if owners.is_empty() {
        return DebugUndoOutcome::NoSession;
    }

    let requests = request_prompts(app);
    let owned_request_snippets: Vec<_> = snapshots
        .iter()
        .filter(|snapshot| is_undo_step_label(&snapshot.label))
        .filter(|snapshot| owners.iter().any(|owner| owner.owns(snapshot)))
        .map(|snapshot| {
            codewhale_engine::core::turn::parse_snapshot_label(&snapshot.label).prompt_snippet
        })
        .collect();
    let request_position = |label: &str| request_is_last(&requests, &owned_request_snippets, label);
    let step = match plan_undo_step(&repo, snapshots, &owners, force, &request_position) {
        Ok(step) => step,
        Err(outcome) => return *outcome,
    };
    let target = &step.target;
    let is_last_request = step.request_is_last;

    let plan: Vec<(PathBuf, codewhale_snapshot::SnapshotId)> = step
        .restore
        .iter()
        .map(|path| (path.clone(), target.tree.clone()))
        .collect();
    // Re-verify after the safety snapshot, immediately before the first
    // write: a change that landed meanwhile is refused.
    let preflight = || {
        for path in &step.restore {
            if !repo.path_matches_snapshot(&step.end, path)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "'{}' changed while the undo was being prepared; nothing was changed.",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    };
    let restored = match &step.backup {
        // Planning already snapshotted the workspace (and `preflight` proves
        // every planned path is still as that snapshot holds it).
        Some(backup) => repo.restore_path_plan_with_backup(&plan, backup, true, preflight),
        None => {
            let backup_short = &target.id.as_str()[..target.id.as_str().len().min(12)];
            repo.restore_path_plan(
                &plan,
                &format!("pre-restore:{backup_short}"),
                true,
                preflight,
            )
        }
    };
    let outcomes = match restored {
        Ok(outcomes) => outcomes,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return DebugUndoOutcome::RestoreBlocked(e.to_string());
        }
        Err(e) => return DebugUndoOutcome::RestoreFailed(e.to_string()),
    };

    // The files are back. Take the request off the conversation at the same
    // boundary a conversation-only undo uses (the user's prompt, never a
    // tool result), or leave a conversation that no longer holds it alone.
    let (conversation_removed, sync) = if is_last_request == Some(true) {
        let before = app.api_messages.len();
        let undone = undo_conversation_for_engine(app);
        app.truncate_history_to(app.history.len() - undone.removed);
        app.truncate_api_messages(undone.sync.messages.len());
        let removed = undone.removed > 0 || undone.sync.messages.len() < before;
        (removed, undone.sync)
    } else {
        (false, session_sync_payload(app))
    };

    DebugUndoOutcome::Restored(DebugUndoRestored {
        request: codewhale_engine::core::turn::parse_snapshot_label(&target.label).prompt_snippet,
        conversation_removed,
        forced: step.forced,
        files: outcomes
            .into_iter()
            .map(|outcome| DebugRestoredFile {
                path: outcome.path,
                action: match outcome.action {
                    codewhale_snapshot::PathRestoreAction::Modified => DebugRestoreAction::Modified,
                    codewhale_snapshot::PathRestoreAction::Recreated => {
                        DebugRestoreAction::Recreated
                    }
                    codewhale_snapshot::PathRestoreAction::Removed => DebugRestoreAction::Removed,
                },
            })
            .collect(),
        skipped: step.skipped,
        sync,
    })
}
