//! Session receipts: what a session or turn actually did, read back from the
//! records Codewhale already persists.
//!
//! There is one builder. It reads two persisted shapes and nothing else:
//!
//! - a terminal session: the saved transcript (`sessions/<id>.json`, whose
//!   `tool_use`/`tool_result` blocks are the calls) plus the session's
//!   approval log (`sessions/<id>/approval_receipts.jsonl`) and saved turn outcomes;
//! - a Runtime thread (the app, `codewhale serve`): the thread's turn and item
//!   records plus the `approval.*` events in its append-only event log.
//!
//! A terminal session also reads the workspace snapshots the engine already
//! takes before and after each turn (`codewhale_snapshot`, when snapshots are
//! on): their difference is every file the turn changed, including files a
//! shell command changed, which no tool record names.
//!
//! Both are normalized into [`ToolStep`]s and [`ApprovalStep`]s and then
//! classified by the same code for `/receipts`, `codewhale receipts`, and
//! `GET /v1/threads/{id}/receipt`. Live and saved terminal sessions supply
//! their recorded turn outcomes so both surfaces report the same failures.
//!
//! The builder only reads. It never calls a provider, runs a tool, or writes
//! a file (reading the snapshots runs `git diff` inside the side repo, which
//! touches neither the work tree nor the user's repository). It exports no reasoning text and no raw tool output: commands,
//! queries, and error lines are bounded and passed through the shared secret
//! redactor. A fact the record does not hold is reported as not recorded,
//! never inferred from display text (see `docs/RECEIPTS.md`).
//!
//! Known limits: a Runtime thread's engine does not tag its snapshots with
//! the thread, so a thread receipt cannot read them and says that shell file
//! changes are not itemized. A snapshot difference covers everything that
//! wrote to the workspace during the turn, not only this agent. Snapshots are
//! pruned to the newest [`codewhale_snapshot::DEFAULT_MAX_SNAPSHOTS`], so older
//! turns have none.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::{ContentBlock, Message};
use serde_json::Value;

use crate::approval_log::{ApprovalOutcome, ApprovalReceipt, ApprovalReplay};
use crate::runtime_threads::{
    RuntimeEventRecord, RuntimeTurnStatus, ThreadRecord, TurnItemKind, TurnItemLifecycleStatus,
    TurnItemRecord, TurnRecord,
};

const MAX_COMMAND_CHARS: usize = 200;
const MAX_ERROR_CHARS: usize = 160;
const MAX_QUERY_CHARS: usize = 120;
const MAX_FILES_PER_ACTION: usize = 50;
const MAX_NESTED_CALLS: usize = 20;

/// Terminal sessions have kept an approval log since 0.9.10 (commit
/// 11717b48ff, released 2026-08-20). A session that started earlier has no
/// record of which calls asked first, so its receipt does not count calls that
/// "ran without asking".
const APPROVAL_LOG_SINCE: &str = "2026-08-20T00:00:00Z";

const CLAIM_CEILING: [&str; 3] = [
    "local_record_only",
    "not_safety_certification",
    "not_provider_compatibility_certification",
];

// ---------------------------------------------------------------------------
// Output shape
// ---------------------------------------------------------------------------

pub use codewhale_command_contract::facets::debug_receipts::*;

// ---------------------------------------------------------------------------
// Normalized input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    Ok,
    Failed,
    Interrupted,
    Running,
    Unknown,
}

/// One tool call as a persisted record holds it.
#[derive(Debug, Clone)]
struct ToolStep {
    turn: Option<String>,
    call_id: Option<String>,
    name: String,
    input: Value,
    outcome: StepOutcome,
    output: Option<String>,
    metadata: Option<Value>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
}

/// A turn and the permission posture its own record names, if any.
type TurnPosture = (String, Option<&'static str>);

/// A turn and the user's prompt text, if it had any.
type TurnPrompt = (String, Option<String>);

type TurnFailure = (Option<String>, Option<DateTime<Utc>>, Option<String>);

/// Each matched turn's workspace change, and how many turns had no pair.
type TurnChanges = (Vec<(String, TurnWorkspaceChange)>, usize);

/// Files a turn changed, from its before/after workspace snapshots.
#[derive(Debug, Clone, Default)]
struct TurnWorkspaceChange {
    files: Vec<FileTouch>,
    /// More paths changed than [`MAX_FILES_PER_ACTION`].
    truncated: bool,
}

#[derive(Debug, Clone)]
struct ApprovalStep {
    turn: Option<String>,
    call_id: Option<String>,
    tool: String,
    fact: ApprovalFact,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Receipt for a terminal session: its transcript plus its approval log.
/// `turn` is a 1-based turn number; `None` covers the whole session.
pub fn session_receipt(
    source: ReceiptSource,
    messages: &[Message],
    approval_receipts: &[ApprovalReceipt],
    turn_outcomes: &[crate::session_manager::SavedTurnOutcome],
    turn: Option<&str>,
) -> anyhow::Result<Receipt> {
    let mut notes = BTreeSet::new();
    let (steps, turn_postures, turn_prompts) = steps_from_messages(messages);
    let failures = turn_outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == crate::core::events::TurnOutcomeStatus::Failed
                && outcome.after_message_count <= messages.len()
        })
        .map(|outcome| {
            let turn = messages
                .iter()
                .take(outcome.after_message_count)
                .filter(|message| {
                    codewhale_models::turn_classify::classify_user_turn_prompt(message)
                        != codewhale_models::turn_classify::UserTurnPromptKind::NotPrompt
                })
                .count();
            (
                (turn > 0).then(|| turn.to_string()),
                Some(outcome.ended_at),
                Some(outcome.error.clone()),
            )
        })
        .collect();
    let approvals = match ApprovalReplay::from_receipts(approval_receipts) {
        Ok(replay) => approvals_from_replay(&replay),
        Err(error) => {
            notes.insert(format!(
                "Approvals: the session's approval log did not replay ({error}), so approvals are left out."
            ));
            Vec::new()
        }
    };
    if let Some(turn) = turn {
        let count = turn_postures.len();
        if !turn
            .parse::<usize>()
            .is_ok_and(|number| (1..=count).contains(&number))
        {
            anyhow::bail!(
                "turn '{turn}' is not a turn in this session; it has {}",
                plural(count, "turn", "turns")
            );
        }
    }
    notes.insert(
        "Timestamps: a terminal session saves calls in order, not when each one ran.".to_string(),
    );
    let log_since = DateTime::parse_from_rfc3339(APPROVAL_LOG_SINCE)
        .map(|at| at.with_timezone(&Utc))
        .ok();
    let approvals_recorded = match (source.started_at, log_since) {
        (Some(started), Some(since)) => started >= since,
        _ => true,
    };
    if !approvals_recorded {
        notes.insert(
            "Approvals: this session started before Codewhale kept an approval log (0.9.10, 2026-08-20), so it cannot show which calls asked first.".to_string(),
        );
    }
    let snapshot_changes = match source.workspace.as_deref() {
        Some(workspace) => snapshot_turn_changes(
            std::path::Path::new(workspace),
            &source.id,
            &turn_prompts,
            turn,
        ),
        None => Ok(None),
    };
    let workspace_changes = match snapshot_changes {
        Ok(Some((changes, unmatched))) => {
            notes.insert(
                "Files changed outside file tools come from the workspace snapshots taken before and after each turn, so they include anything that wrote to the workspace during the turn, not only Codewhale. They leave out what snapshots do not track: ignored and skipped paths (.gitignore entries, .env, node_modules, target, and the like) and anything outside the workspace.".to_string(),
            );
            if unmatched > 0 {
                notes.insert(format!(
                    "Shell file changes: {} without a before/after snapshot (snapshots off, or pruned: the newest {} are kept; or a repeated prompt whose snapshots could not be told apart), so files a command changed there are not itemized.",
                    plural(unmatched, "turn", "turns"),
                    codewhale_snapshot::DEFAULT_MAX_SNAPSHOTS
                ));
            }
            changes
        }
        Ok(None) => {
            notes.insert(
                "Shell file changes: this workspace has no snapshots (snapshots are off, or the workspace is too large for them), so files a command changed are not itemized; only file tools are.".to_string(),
            );
            Vec::new()
        }
        Err(error) => {
            notes.insert(format!(
                "Shell file changes: the workspace snapshots could not be read ({}), so files a command changed are not itemized; only file tools are.",
                bounded(&error, MAX_ERROR_CHARS)
            ));
            Vec::new()
        }
    };
    Ok(assemble(
        source,
        steps,
        approvals,
        failures,
        workspace_changes,
        Assembly {
            turn,
            kind: SourceKind::Session,
            turn_postures,
            approvals_recorded,
        },
        notes,
    ))
}

/// Receipt for a Runtime thread from its snapshot and event log. `turn` is a
/// turn id of this thread; `None` covers every turn.
pub fn thread_receipt(
    thread: &ThreadRecord,
    turns: &[TurnRecord],
    items: &[TurnItemRecord],
    events: &[RuntimeEventRecord],
    turn: Option<&str>,
) -> anyhow::Result<Receipt> {
    if let Some(turn) = turn
        && !turns.iter().any(|record| record.id == turn)
    {
        anyhow::bail!("turn '{turn}' does not belong to thread '{}'", thread.id);
    }
    let mut ordered: Vec<&TurnRecord> = turns.iter().collect();
    ordered.sort_by_key(|record| record.created_at);
    let items_by_id: HashMap<&str, &TurnItemRecord> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut steps = Vec::new();
    let mut failures = Vec::new();
    let mut turn_postures: Vec<TurnPosture> = Vec::new();
    for record in &ordered {
        turn_postures.push((
            record.id.clone(),
            record.permission_posture.as_deref().and_then(posture_label),
        ));
        let mut turn_items: Vec<&TurnItemRecord> = record
            .item_ids
            .iter()
            .filter_map(|id| items_by_id.get(id.as_str()).copied())
            .collect();
        // Items a turn record does not list yet (a live turn) still belong
        // to it by their own turn id.
        for item in items {
            if item.turn_id == record.id && !record.item_ids.contains(&item.id) {
                turn_items.push(item);
            }
        }
        for item in turn_items {
            if let Some(step) = step_from_item(item) {
                steps.push(step);
            }
        }
        if record.status == RuntimeTurnStatus::Failed {
            failures.push((
                Some(record.id.clone()),
                record.ended_at,
                record.error.clone(),
            ));
        }
    }
    let approvals = approvals_from_events(events);
    let source = ReceiptSource {
        kind: SourceKind::Thread,
        id: thread.id.clone(),
        title: thread.title.clone().or_else(|| {
            ordered
                .first()
                .map(|record| record.input_summary.clone())
                .filter(|text| !text.trim().is_empty())
        }),
        workspace: Some(thread.workspace.display().to_string()),
        model: Some(thread.model.clone()),
        started_at: Some(thread.created_at),
        updated_at: Some(thread.updated_at),
    };
    let notes = BTreeSet::from([
        "Shell file changes: a Runtime thread's workspace snapshots are not tagged with the thread, so files a command changed are not itemized; only file tools are.".to_string(),
    ]);
    Ok(assemble(
        source,
        steps,
        approvals,
        failures,
        Vec::new(),
        Assembly {
            turn,
            kind: SourceKind::Thread,
            turn_postures,
            approvals_recorded: true,
        },
        notes,
    ))
}

// ---------------------------------------------------------------------------
// Loading from disk (`codewhale receipts`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReceiptFormat {
    Md,
    Json,
}

/// Receipt for a Runtime thread read straight from its store.
pub fn thread_receipt_from_store(
    store: &crate::runtime_threads::RuntimeThreadStore,
    thread_id: &str,
    turn: Option<&str>,
) -> anyhow::Result<Receipt> {
    let thread = store.load_thread(thread_id)?;
    let turns = store.list_turns_for_thread(thread_id)?;
    let turn_ids: Vec<String> = turns.iter().map(|record| record.id.clone()).collect();
    let items: Vec<TurnItemRecord> = store
        .list_items_for_turns_map(&turn_ids)?
        .into_values()
        .flatten()
        .collect();
    let events: Vec<RuntimeEventRecord> = store
        .events_since(thread_id, None)?
        .into_iter()
        .filter(|event| event.event.starts_with("approval."))
        .collect();
    thread_receipt(&thread, &turns, &items, &events, turn)
}

pub fn session_source(metadata: &crate::session_manager::SessionMetadata) -> ReceiptSource {
    ReceiptSource {
        kind: SourceKind::Session,
        id: metadata.id.clone(),
        title: Some(metadata.title.clone()).filter(|title| !title.trim().is_empty()),
        workspace: Some(metadata.workspace.display().to_string()),
        model: Some(metadata.model.clone()).filter(|model| !model.is_empty()),
        started_at: Some(metadata.created_at),
        updated_at: Some(metadata.updated_at),
    }
}

/// `codewhale receipts [ID|--last] [--turn T] [--format md|json]`.
///
/// `ID` is a saved session id (or unique prefix) or a Runtime thread id
/// (`thr_…`). With neither an id nor `--last`, the most recently updated
/// session or thread is used. Reads only.
pub fn run_receipts_command(
    id: Option<&str>,
    turn: Option<&str>,
    format: ReceiptFormat,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let sessions = crate::session_manager::SessionManager::default_location()
        .context("could not open the saved sessions directory")?;
    let runtime_root =
        crate::runtime_threads::manager::RuntimeThreadManagerConfig::from_task_data_dir(
            crate::task_manager::default_tasks_dir(),
        )
        .data_dir;
    let store = crate::runtime_threads::RuntimeThreadStore::open_read_only(runtime_root)?;

    let receipt = match id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) if id.starts_with("thr_") => {
            let store = store.context("no Runtime thread store exists on this machine")?;
            thread_receipt_from_store(&store, id, turn)?
        }
        Some(prefix) => {
            let id = sessions.resolve_session_id_prefix(prefix)?;
            let session = sessions.load_session_snapshot(&id)?;
            session_receipt(
                session_source(&session.metadata),
                &session.messages,
                &session.approval_receipts,
                &session.turn_outcomes,
                turn,
            )?
        }
        None => {
            let latest_session = sessions.list_sessions()?.into_iter().next();
            let latest_thread = store
                .as_ref()
                .map(|store| store.list_threads())
                .transpose()?
                .and_then(|threads| threads.into_iter().find(|thread| !thread.archived));
            let thread_is_newer = match (&latest_session, &latest_thread) {
                (Some(session), Some(thread)) => thread.updated_at > session.updated_at,
                (None, Some(_)) => true,
                _ => false,
            };
            match (latest_session, latest_thread, store.as_ref()) {
                (_, Some(thread), Some(store)) if thread_is_newer => {
                    thread_receipt_from_store(store, &thread.id, turn)?
                }
                (Some(metadata), _, _) => {
                    let session = sessions.load_session_snapshot(&metadata.id)?;
                    session_receipt(
                        session_source(&session.metadata),
                        &session.messages,
                        &session.approval_receipts,
                        &session.turn_outcomes,
                        turn,
                    )?
                }
                _ => anyhow::bail!("no saved session or Runtime thread to read"),
            }
        }
    };
    let text = match format {
        ReceiptFormat::Md => render_markdown(&receipt),
        ReceiptFormat::Json => render_json(&receipt),
    };
    println!("{}", text.trim_end());
    Ok(())
}

// ---------------------------------------------------------------------------
// Readers: persisted shape -> normalized steps
// ---------------------------------------------------------------------------

/// Tool calls in transcript order, plus each turn's posture. A turn starts at
/// a real user prompt, by the same rule edit-last-turn and titles use
/// ([`codewhale_models::turn_classify::classify_user_turn_prompt`]); runtime-injected
/// messages and tool results do not start one.
/// Each turn's prompt text (the user's words, without the `<turn_meta>`
/// block) comes back too: it labels the turn's workspace snapshots.
fn steps_from_messages(messages: &[Message]) -> (Vec<ToolStep>, Vec<TurnPosture>, Vec<TurnPrompt>) {
    let mut steps: Vec<ToolStep> = Vec::new();
    let mut by_id = HashMap::new();
    let mut call_counts = HashMap::<codewhale_models::ToolCallKey<'_>, usize>::new();
    let mut raw_counts = HashMap::<&str, usize>::new();
    let mut results = HashMap::new();
    for block in messages.iter().flat_map(|message| &message.content) {
        let Some(key) = block.tool_call_key() else {
            continue;
        };
        match block {
            ContentBlock::ToolUse { .. } | ContentBlock::ServerToolUse { .. } => {
                *call_counts.entry(key).or_default() += 1;
                *raw_counts.entry(key.as_str()).or_default() += 1;
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                results
                    .entry(key)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(tool_use_id.as_str()));
            }
            _ => {}
        }
    }
    let mut turn_postures: Vec<TurnPosture> = Vec::new();
    let mut turn_prompts: Vec<TurnPrompt> = Vec::new();
    let mut turn = 0usize;
    for message in messages {
        if codewhale_models::turn_classify::classify_user_turn_prompt(message)
            != codewhale_models::turn_classify::UserTurnPromptKind::NotPrompt
        {
            turn += 1;
            turn_postures.push((turn.to_string(), turn_meta_posture(message)));
            turn_prompts.push((turn.to_string(), prompt_text(message)));
        }
        for block in &message.content {
            match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                }
                | ContentBlock::ServerToolUse { id, name, input } => {
                    let key = block.tool_call_key().expect("tool use key");
                    let unambiguous = !key.as_str().trim().is_empty()
                        && call_counts.get(&key) == Some(&1)
                        && raw_counts.get(key.as_str()) == Some(&1)
                        && results
                            .get(&key)
                            .is_none_or(|result| result.as_deref() == Some(id.as_str()));
                    if unambiguous {
                        by_id.insert(key, (steps.len(), id.as_str()));
                    }
                    steps.push(ToolStep {
                        turn: (turn > 0).then(|| turn.to_string()),
                        call_id: unambiguous.then(|| key.as_str().to_string()),
                        name: name.clone(),
                        input: input.clone(),
                        outcome: StepOutcome::Unknown,
                        output: None,
                        metadata: None,
                        started_at: None,
                        ended_at: None,
                    });
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } => {
                    if let Some(&(index, provider_id)) =
                        block.tool_call_key().and_then(|key| by_id.get(&key))
                        && provider_id == tool_use_id
                    {
                        let step = &mut steps[index];
                        step.outcome = if is_error.unwrap_or(false) {
                            StepOutcome::Failed
                        } else {
                            StepOutcome::Ok
                        };
                        step.output = Some(content.clone());
                    }
                }
                _ => {}
            }
        }
    }
    (steps, turn_postures, turn_prompts)
}

/// The prompt a user message carries: its text blocks, less the
/// `<turn_meta>` block the engine appends.
pub fn prompt_text(message: &Message) -> Option<String> {
    let meta_index =
        codewhale_models::turn_classify::turn_metadata_text(message).map(|(index, _)| index);
    message
        .content
        .iter()
        .enumerate()
        .find_map(|(index, block)| match block {
            ContentBlock::Text { text, .. } if Some(index) != meta_index => Some(text.clone()),
            _ => None,
        })
}

/// Files each turn changed, from the `pre-turn:N` / `post-turn:N` snapshots
/// the engine takes around a turn in this session (`core::turn`). A turn is
/// matched to its pair by the prompt snippet the labels carry, in order, the
/// same way `/restore` listings are read ([`crate::core::turn::
/// snapshot_label_prompt_snippet`]). When another turn has the same snippet
/// ("continue", "yes", or none), the snippet cannot say whose pair it is, so
/// the pair's turn number `N` must agree too ([`seq_fits`]); otherwise the
/// turn counts as unmatched rather than taking a later turn's files. The
/// count of unmatched turns is returned beside the changes. `None` when this
/// workspace has no snapshot repo.
fn snapshot_turn_changes(
    workspace: &std::path::Path,
    session_id: &str,
    turn_prompts: &[TurnPrompt],
    wanted: Option<&str>,
) -> Result<Option<TurnChanges>, String> {
    use crate::core::turn::{parse_snapshot_label, snapshot_label_prompt_snippet};
    let Some(repo) = codewhale_snapshot::SnapshotRepo::open_existing(workspace)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut snapshots = repo.list(usize::MAX).map_err(|error| error.to_string())?;
    snapshots.reverse();
    // Pair each post-turn:N with the open pre-turn:N of this session, oldest
    // first. The sequence restarts when the session is resumed, so a pair
    // closes on the first matching post-turn.
    let mut open: HashMap<u64, (codewhale_snapshot::SnapshotId, Option<String>)> = HashMap::new();
    let mut pairs = Vec::new();
    for snapshot in snapshots
        .into_iter()
        .filter(|snapshot| snapshot.session_id.as_deref() == Some(session_id))
    {
        let label = parse_snapshot_label(&snapshot.label);
        let Some(seq) = label.seq else { continue };
        match label.kind.as_str() {
            "pre-turn" => {
                open.insert(seq, (snapshot.id, label.prompt_snippet));
            }
            "post-turn" => {
                if let Some((pre, snippet)) = open.remove(&seq) {
                    pairs.push((pre, snapshot.id, snippet, seq));
                }
            }
            _ => {}
        }
    }
    let snippets: Vec<Option<String>> = turn_prompts
        .iter()
        .map(|(_, prompt)| prompt.as_deref().and_then(snapshot_label_prompt_snippet))
        .collect();
    let mut changes = Vec::new();
    let mut unmatched = 0usize;
    let mut next_pair = 0usize;
    // The last turn given a pair: its place in the transcript (1-based) and
    // the pair's turn number.
    let mut last: Option<(u64, u64)> = None;
    let mut last_position = 0u64;
    for (position, ((turn, _), snippet)) in turn_prompts.iter().zip(&snippets).enumerate() {
        let position = position as u64 + 1;
        let repeated = snippets
            .iter()
            .enumerate()
            .any(|(other, label)| other as u64 + 1 != position && label == snippet);
        let found = pairs[next_pair..]
            .iter()
            .position(|(_, _, label, _)| label == snippet)
            .map(|offset| next_pair + offset)
            .filter(|&index| {
                // Engine turns with no transcript prompt (a `!` shell
                // command) number a pair too; count the ones still listed.
                let extra = pairs[next_pair..index]
                    .iter()
                    .filter(|(_, _, label, _)| !snippets.contains(label))
                    .count() as u64;
                let since = position - last_position + extra;
                !repeated || seq_fits(pairs[index].3, since, last.map(|(_, seq)| seq))
            });
        let Some(index) = found else {
            if wanted.is_none_or(|wanted| wanted == turn) {
                unmatched += 1;
            }
            continue;
        };
        next_pair = index + 1;
        last = Some((position, pairs[index].3));
        last_position = position;
        if wanted.is_some_and(|wanted| wanted != turn) {
            continue;
        }
        let (pre, post, _, _) = &pairs[index];
        let (paths, truncated) = repo
            .path_changes_between(pre, post, MAX_FILES_PER_ACTION)
            .map_err(|error| error.to_string())?;
        let files = paths
            .into_iter()
            .map(|change| FileTouch {
                path: change.path,
                change: match change.status {
                    'A' => FileChangeKind::Created,
                    'D' => FileChangeKind::Deleted,
                    _ => FileChangeKind::Edited,
                },
                lines_added: change.added,
                lines_removed: change.removed,
            })
            .collect();
        changes.push((turn.clone(), TurnWorkspaceChange { files, truncated }));
    }
    Ok(Some((changes, unmatched)))
}

/// Whether a snapshot pair numbered `seq` can belong to a turn that comes
/// `since` engine turns after the last matched pair (numbered `last_seq`),
/// or `since` turns after the session started when none matched yet. The
/// engine numbers turns from 1 each time it starts, so within one run the
/// number moves in step with the turns; after a resume it restarts, and can
/// be at most `since`.
fn seq_fits(seq: u64, since: u64, last_seq: Option<u64>) -> bool {
    let restarted = (1..=since).contains(&seq);
    match last_seq {
        Some(last_seq) => seq == last_seq + since || restarted,
        None => restarted,
    }
}

/// `path` relative to `workspace` when it is inside it, without a leading
/// `./`, for comparing a file tool's path with a snapshot's.
fn workspace_relative(path: &str, workspace: Option<&str>) -> String {
    let relative = workspace
        .and_then(|workspace| {
            path.strip_prefix(workspace.trim_end_matches('/'))
                .and_then(|rest| rest.strip_prefix('/'))
        })
        .unwrap_or(path);
    relative.trim_start_matches("./").to_string()
}

/// The posture line the engine writes into a prompt's `<turn_meta>` block
/// ([`crate::core::engine::PERMISSION_POSTURE_LINE`]).
fn turn_meta_posture(message: &Message) -> Option<&'static str> {
    let (_, meta) = codewhale_models::turn_classify::turn_metadata_text(message)?;
    meta.lines().find_map(|line| {
        line.trim()
            .strip_prefix(crate::core::engine::PERMISSION_POSTURE_LINE)
            .and_then(posture_label)
    })
}

/// The chip label for a posture the host wrote: a `<turn_meta>` label
/// (`Full Access`) or a Runtime turn's wire value (`full_access`). Anything
/// else is not a posture and yields `None`.
fn posture_label(value: &str) -> Option<&'static str> {
    let value = value.trim();
    [
        ApprovalMode::Suggest,
        ApprovalMode::Auto,
        ApprovalMode::Bypass,
        ApprovalMode::Never,
    ]
    .into_iter()
    .map(ApprovalMode::permission_chip_label)
    .find(|label| label.eq_ignore_ascii_case(value))
    .or_else(|| ApprovalMode::from_config_value(value).map(ApprovalMode::permission_chip_label))
}

fn step_from_item(item: &TurnItemRecord) -> Option<ToolStep> {
    if !matches!(
        item.kind,
        TurnItemKind::ToolCall | TurnItemKind::FileChange | TurnItemKind::CommandExecution
    ) {
        return None;
    }
    let metadata = item.metadata.clone();
    let meta = metadata.as_ref();
    let name = meta
        .and_then(|meta| meta.get("tool_name"))
        .and_then(Value::as_str)?
        .to_string();
    let call_id = meta
        .and_then(|meta| meta.get("tool_use_id").or_else(|| meta.get("tool_call_id")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let input = meta
        .and_then(|meta| meta.get("tool_input"))
        .map(|raw| match raw {
            Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
            other => other.clone(),
        })
        .unwrap_or(Value::Null);
    let outcome = match item.status {
        TurnItemLifecycleStatus::Completed => StepOutcome::Ok,
        TurnItemLifecycleStatus::Failed => StepOutcome::Failed,
        TurnItemLifecycleStatus::Interrupted | TurnItemLifecycleStatus::Canceled => {
            StepOutcome::Interrupted
        }
        TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress => {
            StepOutcome::Running
        }
    };
    let finished = !matches!(outcome, StepOutcome::Running);
    Some(ToolStep {
        turn: Some(item.turn_id.clone()),
        call_id,
        name,
        input,
        outcome,
        output: finished.then(|| item.detail.clone()).flatten(),
        metadata,
        started_at: item.started_at,
        ended_at: item.ended_at,
    })
}

fn approvals_from_replay(replay: &ApprovalReplay) -> Vec<ApprovalStep> {
    let mut out = Vec::new();
    for completed in &replay.completed {
        let (decision, implied) = match &completed.outcome {
            ApprovalOutcome::ApprovedOnce => (ApprovalDecisionLabel::Approved, None),
            // The Runtime answers "deny" for a request it could not put in
            // front of anyone (no active turn, the turn stopped, the channel
            // closed). That is nobody answering, not a no.
            ApprovalOutcome::Denied if completed.decided_by == Some(ApprovalDecider::Host) => {
                (ApprovalDecisionLabel::Unavailable, None)
            }
            ApprovalOutcome::Denied => (ApprovalDecisionLabel::Denied, None),
            ApprovalOutcome::Timeout => (ApprovalDecisionLabel::TimedOut, None),
            ApprovalOutcome::Cancelled => (
                ApprovalDecisionLabel::Cancelled,
                Some(ApprovalDecider::Host),
            ),
            ApprovalOutcome::Unavailable => (
                ApprovalDecisionLabel::Unavailable,
                Some(ApprovalDecider::Host),
            ),
            ApprovalOutcome::RetryWithPolicy { .. } => {
                (ApprovalDecisionLabel::ApprovedWithPolicy, None)
            }
        };
        out.push(ApprovalStep {
            turn: None,
            call_id: Some(completed.ask.approval_id().to_string()),
            tool: completed.ask.tool_name().unwrap_or("unknown").to_string(),
            fact: ApprovalFact {
                decision,
                decided_by: completed.decided_by.or(implied),
                at: Some(completed.decided_at),
            },
        });
    }
    for ask in &replay.unmatched_asks {
        out.push(ApprovalStep {
            turn: None,
            call_id: Some(ask.approval_id().to_string()),
            tool: ask.tool_name().unwrap_or("unknown").to_string(),
            fact: ApprovalFact {
                decision: ApprovalDecisionLabel::Pending,
                decided_by: None,
                at: Some(ask.created_at()),
            },
        });
    }
    out
}

/// Pair `approval.required` with `approval.decided` by approval id. Who
/// decided is read from the flags the Runtime writes on the decision event:
/// `timeout`, `cancelled`, `posture`, `auto` (+ `grant_id` for a session
/// rule). A decision with none of them came from a client acting for a
/// person.
fn approvals_from_events(events: &[RuntimeEventRecord]) -> Vec<ApprovalStep> {
    let mut order: Vec<String> = Vec::new();
    let mut steps: HashMap<String, ApprovalStep> = HashMap::new();
    for event in events {
        let payload = &event.payload;
        let Some(approval_id) = payload
            .get("approval_id")
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        match event.event.as_str() {
            "approval.required" => {
                if !steps.contains_key(approval_id) {
                    order.push(approval_id.to_string());
                }
                steps.insert(
                    approval_id.to_string(),
                    ApprovalStep {
                        turn: event.turn_id.clone(),
                        call_id: payload
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        tool: payload
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        fact: ApprovalFact {
                            decision: ApprovalDecisionLabel::Pending,
                            decided_by: None,
                            at: Some(event.timestamp),
                        },
                    },
                );
            }
            "approval.decided" => {
                let flag = |key: &str| payload.get(key).and_then(Value::as_bool) == Some(true);
                let allowed = payload.get("decision").and_then(Value::as_str) == Some("allow");
                let decision = if allowed {
                    ApprovalDecisionLabel::Approved
                } else if flag("timeout") {
                    ApprovalDecisionLabel::TimedOut
                } else if flag("cancelled") {
                    ApprovalDecisionLabel::Cancelled
                } else {
                    ApprovalDecisionLabel::Denied
                };
                let decided_by = if flag("timeout") {
                    None
                } else if flag("cancelled") {
                    Some(ApprovalDecider::Host)
                } else if payload.get("posture").is_some() {
                    Some(ApprovalDecider::Posture)
                } else if flag("auto") {
                    if payload
                        .get("grant_id")
                        .is_some_and(|grant| !grant.is_null())
                    {
                        Some(ApprovalDecider::SessionRule)
                    } else {
                        Some(ApprovalDecider::Posture)
                    }
                } else {
                    Some(ApprovalDecider::User)
                };
                let fact = ApprovalFact {
                    decision,
                    decided_by,
                    at: Some(event.timestamp),
                };
                match steps.get_mut(approval_id) {
                    Some(step) => step.fact = fact,
                    None => {
                        order.push(approval_id.to_string());
                        steps.insert(
                            approval_id.to_string(),
                            ApprovalStep {
                                turn: event.turn_id.clone(),
                                call_id: payload
                                    .get("tool_call_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                tool: "unknown".to_string(),
                                fact,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| steps.remove(&id))
        .collect()
}

// ---------------------------------------------------------------------------
// Classification: normalized step -> action
// ---------------------------------------------------------------------------

enum Classified {
    Listed(ActionKind),
    /// Counted as an other tool call; listed only if it failed.
    Other,
    /// A later call on an agent the receipt already lists (status, wait,
    /// cancel): it updates that agent's outcome instead of adding a line.
    AgentFollowUp {
        agent_id: String,
        status: Option<String>,
    },
}

fn classify(step: &ToolStep, notes: &mut BTreeSet<String>) -> Classified {
    let structured = structured_output(step);
    let facts = step.metadata.as_ref().or(structured.as_ref());
    if let Some(files) = mutation_files(facts) {
        return Classified::Listed(ActionKind::FileChange { files });
    }
    let semantic =
        codewhale_execpolicy::canonical_action::canonical_action_alias(&step.name, &step.input);
    match semantic {
        "write_file" | "edit_file" | "apply_patch" | "fim_edit" => {
            let files = files_from_input(semantic, &step.input, step.outcome);
            if files.is_empty() {
                return Classified::Other;
            }
            if files.iter().any(|file| file.lines_added.is_none()) {
                notes.insert(
                    "Line counts: some file changes have no saved diff, so their +/- counts are not recorded.".to_string(),
                );
            }
            Classified::Listed(ActionKind::FileChange { files })
        }
        "exec_shell" | "task_shell_start" | "task_gate_run" | "run_tests" | "run_verifiers" => {
            let exit_code = number(facts, &["exit_code", "return_code"])
                .or_else(|| closing_exit_code(step.output.as_deref()));
            if exit_code.is_none() && matches!(step.outcome, StepOutcome::Ok) {
                notes.insert(
                    "Exit codes: calls that saved no structured result (terminal sessions) show pass or fail, not the exit code.".to_string(),
                );
            }
            Classified::Listed(ActionKind::Command {
                command: command_text(&step.name, semantic, &step.input),
                cwd: string_field(
                    Some(&step.input),
                    &["cwd", "working_dir", "workdir", "workspace"],
                )
                .or_else(|| string_field(facts, &["working_dir", "cwd"])),
                exit_code,
            })
        }
        "code_execution" | "js_execution" | "execute_tools" | "rlm_eval" => {
            Classified::Listed(ActionKind::Code {
                exit_code: number(facts, &["return_code", "exit_code"]),
                nested: nested_calls(structured.as_ref().or(facts)),
            })
        }
        "web_search" | "fetch_url" | "web.run" | "rlm_open" | "git_fetch" => {
            let url = string_field(Some(&step.input), &["url", "uri"]);
            let host = url
                .as_deref()
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_string))
                .or_else(|| {
                    (semantic == "git_fetch")
                        .then(|| string_field(Some(&step.input), &["remote"]))
                        .flatten()
                });
            let action = match semantic {
                "web_search" => "search",
                "git_fetch" => "git_fetch",
                _ if url.is_some() => "fetch",
                _ => "request",
            };
            let query = string_field(Some(&step.input), &["query", "q", "search_query"])
                .map(|query| bounded(&redact(&query), MAX_QUERY_CHARS));
            Classified::Listed(ActionKind::Network {
                action: action.to_string(),
                host,
                query,
            })
        }
        name if name.starts_with("github_") => Classified::Listed(ActionKind::Network {
            action: name.trim_start_matches("github_").to_string(),
            host: Some("github.com".to_string()),
            query: None,
        }),
        name if name.starts_with("mcp_") => {
            let server = crate::mcp::connected_app_server(name).map(str::to_string);
            let plugin = server
                .as_deref()
                .is_some_and(|server| server.starts_with("plugin"));
            Classified::Listed(ActionKind::Mcp { server, plugin })
        }
        "agent" => {
            let action = step
                .input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("start");
            let agent_id = string_field(facts, &["agent_id"]);
            let status = string_field(facts, &["status"]);
            if action == "start" {
                Classified::Listed(ActionKind::Subagent {
                    name: string_field(Some(&step.input), &["name"])
                        .or_else(|| string_field(facts, &["name"])),
                    agent_id,
                    outcome: status,
                })
            } else if let Some(agent_id) = agent_id {
                Classified::AgentFollowUp { agent_id, status }
            } else {
                Classified::Other
            }
        }
        _ => Classified::Other,
    }
}

/// The host-owned JSON a tool returned as its text result (agent, code,
/// execute_tools), when the persisted record has no structured metadata. A
/// leading approval note is skipped. Anything that is not a JSON object is
/// ignored rather than read as prose, and so is JSON another party wrote
/// ([`result_is_outside_text`]): an MCP server's reply cannot claim a file
/// change or an exit code.
fn structured_output(step: &ToolStep) -> Option<Value> {
    if result_is_outside_text(step) {
        return None;
    }
    let output = step.output.as_deref()?.trim_start();
    let body = if output.starts_with("[approval] ") {
        output.split_once("\n\n").map(|(_, rest)| rest)?
    } else {
        output
    };
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    value.is_object().then_some(value)
}

fn mutation_files(facts: Option<&Value>) -> Option<Vec<FileTouch>> {
    let mutation = facts?.get("mutation")?;
    let files = mutation.get("files")?.as_array()?;
    let counts = mutation
        .get("diff")
        .and_then(Value::as_str)
        .map(diff_counts_by_path)
        .unwrap_or_default();
    let mut out: Vec<FileTouch> = files
        .iter()
        .filter_map(|file| {
            let path = file.get("path")?.as_str()?.to_string();
            let change = match file.get("outcome").and_then(Value::as_str) {
                Some("created") => FileChangeKind::Created,
                Some("deleted") => FileChangeKind::Deleted,
                _ => FileChangeKind::Edited,
            };
            let (added, removed) = counts.get(&path).copied().unwrap_or((0, 0));
            Some(FileTouch {
                path,
                change,
                lines_added: Some(added),
                lines_removed: Some(removed),
            })
        })
        .collect();
    if let Some(renames) = mutation.get("renames").and_then(Value::as_array) {
        for rename in renames {
            if let Some(to) = rename.get("to").and_then(Value::as_str) {
                out.push(FileTouch {
                    path: to.to_string(),
                    change: FileChangeKind::Created,
                    lines_added: Some(0),
                    lines_removed: Some(0),
                });
            }
            if let Some(from) = rename.get("from").and_then(Value::as_str) {
                out.push(FileTouch {
                    path: from.to_string(),
                    change: FileChangeKind::Deleted,
                    lines_added: Some(0),
                    lines_removed: Some(0),
                });
            }
        }
    }
    out.truncate(MAX_FILES_PER_ACTION);
    (!out.is_empty()).then_some(out)
}

/// One line of a unified diff or patch, read in context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffLine<'a> {
    /// A `--- old` / `+++ new` file header pair, as their raw paths.
    Header {
        old: &'a str,
        new: &'a str,
    },
    /// `diff --git …`: a new file section starts.
    FileStart,
    Added,
    Removed,
    Other(&'a str),
}

/// Classify a diff's lines. `--- ` and `+++ ` are file headers only as an
/// adjacent pair outside a hunk: inside one they are a removed `-- …` or
/// added `++ …` line (a SQL or Lua comment), and a hunk's `@@ -a,b +c,d @@`
/// counts say where it ends.
fn diff_lines(text: &str) -> Vec<DiffLine<'_>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let (mut old_left, mut new_left) = (0u64, 0u64);
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        // Never hunk content, which always starts with ` `, `+`, or `-`.
        if line.starts_with("diff --git ") {
            (old_left, new_left) = (0, 0);
            out.push(DiffLine::FileStart);
            continue;
        }
        let in_hunk = old_left > 0 || new_left > 0;
        if !in_hunk {
            if let (Some(old), Some(new)) = (
                line.strip_prefix("--- "),
                lines.get(index).and_then(|next| next.strip_prefix("+++ ")),
            ) {
                out.push(DiffLine::Header { old, new });
                index += 1;
                continue;
            }
            if line.starts_with("@@") {
                if let Some((old, new)) = hunk_counts(line) {
                    (old_left, new_left) = (old, new);
                }
                out.push(DiffLine::Other(line));
                continue;
            }
        }
        out.push(match line.as_bytes().first() {
            Some(b'+') => {
                new_left = new_left.saturating_sub(1);
                DiffLine::Added
            }
            Some(b'-') => {
                old_left = old_left.saturating_sub(1);
                DiffLine::Removed
            }
            // A blank line is a context line whose leading space was trimmed.
            Some(b' ') | None => {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                DiffLine::Other(line)
            }
            _ => DiffLine::Other(line),
        });
    }
    out
}

/// `(old, new)` line counts from `@@ -a[,b] +c[,d] @@`; a missing count is 1.
fn hunk_counts(line: &str) -> Option<(u64, u64)> {
    let mut ranges = line.strip_prefix("@@ ")?.split_whitespace();
    let count = |range: &str, sign: char| -> Option<u64> {
        let range = range.strip_prefix(sign)?;
        match range.split_once(',') {
            Some((_, count)) => count.parse().ok(),
            None => range.parse::<u64>().ok().map(|_| 1),
        }
    };
    Some((count(ranges.next()?, '-')?, count(ranges.next()?, '+')?))
}

/// Per-file `(+, -)` from a unified diff, keyed by the `+++ b/<path>` (or
/// `--- a/<path>` for deletions) header.
fn diff_counts_by_path(diff: &str) -> HashMap<String, (u64, u64)> {
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut current: Option<String> = None;
    for line in diff_lines(diff) {
        match line {
            DiffLine::Header { old, new } => {
                current = header_path(new).or_else(|| header_path(old));
                if let Some(path) = &current {
                    counts.entry(path.clone()).or_default();
                }
            }
            DiffLine::FileStart => current = None,
            DiffLine::Added | DiffLine::Removed => {
                let Some(path) = &current else { continue };
                let entry = counts.entry(path.clone()).or_default();
                if line == DiffLine::Added {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
            }
            DiffLine::Other(_) => {}
        }
    }
    counts
}

fn header_path(rest: &str) -> Option<String> {
    let path = rest.split('\t').next()?.trim();
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_string(),
    )
}

/// File changes from a call's own input, for records without a saved diff.
/// A failed call changed nothing, so it lists its paths with no counts.
fn files_from_input(semantic: &str, input: &Value, outcome: StepOutcome) -> Vec<FileTouch> {
    let succeeded = outcome == StepOutcome::Ok;
    let path = string_field(Some(input), &["path", "file_path", "filePath"]);
    match semantic {
        "write_file" => path
            .map(|path| {
                vec![FileTouch {
                    path,
                    change: FileChangeKind::Written,
                    lines_added: None,
                    lines_removed: None,
                }]
            })
            .unwrap_or_default(),
        "edit_file" | "fim_edit" => {
            let Some(path) = path else {
                return Vec::new();
            };
            let (added, removed) = if succeeded {
                edit_line_counts(input)
            } else {
                (None, None)
            };
            vec![FileTouch {
                path,
                change: FileChangeKind::Edited,
                lines_added: added,
                lines_removed: removed,
            }]
        }
        "apply_patch" => {
            let Some(patch) = string_field(Some(input), &["patch", "input"]) else {
                return path
                    .map(|path| {
                        vec![FileTouch {
                            path,
                            change: FileChangeKind::Edited,
                            lines_added: None,
                            lines_removed: None,
                        }]
                    })
                    .unwrap_or_default();
            };
            let mut files = patch_files(&patch, path.as_deref());
            if !succeeded {
                for file in &mut files {
                    file.lines_added = None;
                    file.lines_removed = None;
                }
            }
            files
        }
        _ => Vec::new(),
    }
}

/// `(+, -)` from an edit call's replacement pairs: the lines of every
/// replacement text against the lines of every searched text.
fn edit_line_counts(input: &Value) -> (Option<u64>, Option<u64>) {
    const SEARCH: &[&str] = &["search", "old_string", "old_str", "oldText", "old_text"];
    const REPLACE: &[&str] = &[
        "replace",
        "new_string",
        "new_str",
        "newText",
        "new_text",
        "replacement",
    ];
    let pairs: Vec<&Value> = match input.get("edits").and_then(Value::as_array) {
        Some(edits) => edits.iter().collect(),
        None => vec![input],
    };
    let mut added = 0u64;
    let mut removed = 0u64;
    let mut any = false;
    for pair in pairs {
        let old = string_field(Some(pair), SEARCH);
        let new = string_field(Some(pair), REPLACE);
        if old.is_none() && new.is_none() {
            continue;
        }
        any = true;
        removed += line_count(old.as_deref().unwrap_or(""));
        added += line_count(new.as_deref().unwrap_or(""));
    }
    if any {
        (Some(added), Some(removed))
    } else {
        (None, None)
    }
}

fn line_count(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u64
    }
}

/// Files and counts from a patch the call supplied: `*** Add/Update/Delete
/// File:` envelopes or unified-diff headers.
fn patch_files(patch: &str, fallback_path: Option<&str>) -> Vec<FileTouch> {
    let mut files: Vec<FileTouch> = Vec::new();
    for line in diff_lines(patch) {
        let (added, removed) = match line {
            DiffLine::Header { old, new } => {
                let (path, change) = match (header_path(new), header_path(old)) {
                    (Some(path), Some(_)) => (path, FileChangeKind::Edited),
                    (Some(path), None) => (path, FileChangeKind::Created),
                    (None, Some(old)) => (old, FileChangeKind::Deleted),
                    (None, None) => continue,
                };
                files.push(FileTouch {
                    path,
                    change,
                    lines_added: Some(0),
                    lines_removed: Some(0),
                });
                continue;
            }
            DiffLine::FileStart => continue,
            DiffLine::Added => (1, 0),
            DiffLine::Removed => (0, 1),
            DiffLine::Other(line) => {
                envelope_file(line, &mut files);
                continue;
            }
        };
        if files.is_empty()
            && let Some(path) = fallback_path
        {
            files.push(FileTouch {
                path: path.to_string(),
                change: FileChangeKind::Edited,
                lines_added: Some(0),
                lines_removed: Some(0),
            });
        }
        if let Some(file) = files.last_mut() {
            *file.lines_added.get_or_insert(0) += added;
            *file.lines_removed.get_or_insert(0) += removed;
        }
    }
    files.truncate(MAX_FILES_PER_ACTION);
    files
}

/// A `*** Add/Update/Delete File: <path>` envelope line starts a file.
fn envelope_file(line: &str, files: &mut Vec<FileTouch>) {
    let envelope = [
        ("*** Add File: ", FileChangeKind::Created),
        ("*** Update File: ", FileChangeKind::Edited),
        ("*** Delete File: ", FileChangeKind::Deleted),
    ]
    .into_iter()
    .find_map(|(prefix, change)| line.strip_prefix(prefix).map(|path| (path, change)));
    if let Some((path, change)) = envelope {
        files.push(FileTouch {
            path: path.trim().to_string(),
            change,
            lines_added: Some(0),
            lines_removed: Some(0),
        });
    }
}

fn command_text(tool: &str, semantic: &str, input: &Value) -> String {
    let raw = match input.get("command").or_else(|| input.get("cmd")) {
        Some(Value::String(command)) => command.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.as_str()
                    .map_or_else(|| part.to_string(), str::to_string)
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => match semantic {
            "run_tests" | "run_verifiers" => {
                let what = if semantic == "run_tests" {
                    "tests"
                } else {
                    "verifiers"
                };
                let names = input
                    .get("commands")
                    .and_then(Value::as_array)
                    .map(|commands| {
                        commands
                            .iter()
                            .filter_map(|command| command.get("name").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|names| !names.is_empty());
                match names {
                    Some(names) => format!("{tool} {what}: {names}"),
                    None => format!("{tool} {what}"),
                }
            }
            _ => tool.to_string(),
        },
    };
    // Redact the text as written: the redactor finds a private-key block by
    // its lines, which flattening would join into one.
    let flat = redact(&raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    bounded(&flat, MAX_COMMAND_CHARS)
}

/// The engine's own closing status line for a failed shell call, e.g.
/// `Command exited with code 2`. This is a fixed format the shell tool writes,
/// not model prose; anything else yields `None`.
fn closing_exit_code(output: Option<&str>) -> Option<i64> {
    let last = output?.trim_end().lines().last()?.trim();
    last.strip_prefix("Command exited with code ")?.parse().ok()
}

/// What a failed call's own result shows about whether it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureEvidence {
    /// An exit code, or a status line the shell writes only after a process
    /// ran.
    Ran,
    /// Codewhale refused the call before it started.
    Refused,
    /// Stopped at an approval prompt. The text says `denied by user` for any
    /// decider (a host with nobody to ask writes it too), so without an
    /// approval-log record it proves the call did not run, not who stopped it.
    DeniedAtApproval,
    /// Neither.
    Unclear,
}

/// Lines the shell tools write only after a process ran
/// (`tools/shell.rs`: `contract_bash_error_status` and the `exec_shell`
/// result).
const SHELL_RAN_LINES: [&str; 5] = [
    "Command exited with code ",
    "Command failed (",
    "Command timed out",
    "Command canceled",
    "Command aborted",
];

/// The shell tools, by semantic name. Only these write `BLOCKED:` (their
/// policy and safety blocks) and [`SHELL_RAN_LINES`].
const SHELL_TOOLS: [&str; 5] = [
    "exec_shell",
    "task_shell_start",
    "task_gate_run",
    "run_tests",
    "run_verifiers",
];

/// Tools whose result text is someone else's words: an MCP server's or
/// GitHub's reply, or a fetched page. Nothing in it is read as a fact about
/// the call, not even JSON; only metadata Codewhale wrote is.
fn result_is_outside_text(step: &ToolStep) -> bool {
    let semantic =
        codewhale_execpolicy::canonical_action::canonical_action_alias(&step.name, &step.input);
    step.name.starts_with("mcp_")
        || semantic.starts_with("mcp_")
        || semantic.starts_with("github_")
        || matches!(
            semantic,
            "web_search" | "fetch_url" | "web.run" | "rlm_open" | "git_fetch"
        )
}

/// Tools whose failed result can open with text nobody at Codewhale framed:
/// [`result_is_outside_text`], plus a program's own output (code tools) and a
/// sub-agent's words. Their failure text never proves a refusal.
fn failure_text_is_outside(step: &ToolStep) -> bool {
    let semantic =
        codewhale_execpolicy::canonical_action::canonical_action_alias(&step.name, &step.input);
    result_is_outside_text(step)
        || matches!(
            semantic,
            "code_execution" | "js_execution" | "execute_tools" | "rlm_eval" | "agent"
        )
}

/// Whether a failed call's result shows it started. The record keeps no
/// "blocked" flag, so only shapes Codewhale itself writes are read, never
/// model prose or another program's text:
///
/// - `side_effect_status: not_started` in the call's metadata (the engine
///   writes it), or on the `Tool validation feedback:` line
///   `dispatch::format_tool_error_with_schema` appends as the result's last
///   line.
/// - a call refused before it runs gets its error as the result. A terminal
///   session saves `Error: ` plus `dispatch::format_tool_error_with_schema`;
///   a Runtime thread saves the `ToolError`'s own text. A shell tool's policy
///   and safety blocks start with `BLOCKED:`.
/// - a process that ran leaves an exit code or one of [`SHELL_RAN_LINES`].
///
/// A tool whose failure text can come from outside Codewhale
/// ([`failure_text_is_outside`]) is judged by metadata alone: an MCP server
/// that answers `BLOCKED:` or `{"side_effect_status":"not_started"}` must not
/// hide a call that ran. Such a call reads as failed, which over-counts what
/// ran rather than under-counting it.
fn failure_evidence(step: &ToolStep) -> FailureEvidence {
    let structured = structured_output(step);
    let facts = step.metadata.as_ref().or(structured.as_ref());
    if number(facts, &["exit_code", "return_code"]).is_some() {
        return FailureEvidence::Ran;
    }
    if string_field(step.metadata.as_ref(), &["side_effect_status"]).as_deref()
        == Some("not_started")
    {
        return FailureEvidence::Refused;
    }
    if failure_text_is_outside(step) {
        return FailureEvidence::Unclear;
    }
    let Some(output) = step.output.as_deref() else {
        return FailureEvidence::Unclear;
    };
    let lines = || output.lines().map(str::trim);
    if lines().any(|line| {
        SHELL_RAN_LINES
            .iter()
            .any(|prefix| line.starts_with(prefix))
    }) {
        return FailureEvidence::Ran;
    }
    let not_started = lines()
        .rfind(|line| !line.is_empty())
        .and_then(|last| last.strip_prefix("Tool validation feedback: "))
        .and_then(|feedback| serde_json::from_str::<Value>(feedback).ok())
        .is_some_and(|feedback| {
            feedback.get("side_effect_status").and_then(Value::as_str) == Some("not_started")
        });
    if not_started {
        return FailureEvidence::Refused;
    }
    let Some(first) = lines().find(|line| !line.is_empty() && !line.starts_with("[approval]"))
    else {
        return FailureEvidence::Unclear;
    };
    let first = first.strip_prefix("Error: ").unwrap_or(first);
    if first.starts_with("BLOCKED:") {
        let semantic =
            codewhale_execpolicy::canonical_action::canonical_action_alias(&step.name, &step.input);
        return if SHELL_TOOLS.contains(&semantic) {
            FailureEvidence::Refused
        } else {
            FailureEvidence::Unclear
        };
    }
    if first.starts_with("Tool '") && first.contains("' denied by user") {
        return FailureEvidence::DeniedAtApproval;
    }
    const REFUSED_PREFIXES: [&str; 6] = [
        "Invalid input for tool '",
        "Path escapes workspace:",
        // `ToolError` text, as a Runtime thread saves it.
        "Failed to authorize tool execution:",
        "Failed to validate input:",
        "Failed to locate tool:",
        "Failed to resolve path '",
    ];
    const REFUSED_MARKERS: [&str; 3] = [
        "' was denied: ",
        "' is not available",
        "' is missing required field ",
    ];
    let refused = REFUSED_PREFIXES
        .iter()
        .any(|prefix| first.starts_with(prefix))
        || (first.starts_with("Tool '")
            && REFUSED_MARKERS.iter().any(|marker| first.contains(marker)))
        || first.contains("is not available in Plan mode");
    if refused {
        FailureEvidence::Refused
    } else {
        FailureEvidence::Unclear
    }
}

fn nested_calls(facts: Option<&Value>) -> Vec<NestedCall> {
    let Some(calls) = facts
        .and_then(|facts| facts.get("calls"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    calls
        .iter()
        .take(MAX_NESTED_CALLS)
        .filter_map(|call| {
            Some(NestedCall {
                tool: call.get("tool")?.as_str()?.to_string(),
                ok: call.get("ok").and_then(Value::as_bool).unwrap_or(false),
                elapsed_ms: call.get("elapsed_ms").and_then(Value::as_u64),
            })
        })
        .collect()
}

fn string_field(value: Option<&Value>, keys: &[&str]) -> Option<String> {
    let value = value?;
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

fn number(value: Option<&Value>, keys: &[&str]) -> Option<i64> {
    let value = value?;
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_i64))
}

fn redact(text: &str) -> String {
    codewhale_secrets::redact::redact_secrets(text)
}

use codewhale_commands::diagnostics_reports::receipts::{bounded, plural};

fn first_error_line(output: Option<&str>) -> Option<String> {
    let line = output?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("[approval]"))?;
    let line = line.strip_prefix("Error: ").unwrap_or(line);
    Some(bounded(&redact(line), MAX_ERROR_CHARS))
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// What [`assemble`] needs to know about the record besides its steps.
struct Assembly<'a> {
    turn: Option<&'a str>,
    kind: SourceKind,
    turn_postures: Vec<TurnPosture>,
    /// False when the record predates its approval log, so "no approval on
    /// record" does not mean "did not ask".
    approvals_recorded: bool,
}

fn assemble(
    source: ReceiptSource,
    steps: Vec<ToolStep>,
    approvals: Vec<ApprovalStep>,
    turn_failures: Vec<TurnFailure>,
    workspace_changes: Vec<(String, TurnWorkspaceChange)>,
    scope: Assembly<'_>,
    mut notes: BTreeSet<String>,
) -> Receipt {
    let Assembly {
        turn,
        kind,
        turn_postures,
        approvals_recorded,
    } = scope;
    let mut approvals_by_call = HashMap::new();
    let mut calls_by_id = HashMap::<&str, usize>::new();
    for step in &steps {
        if let Some(id) = step.call_id.as_deref() {
            *calls_by_id.entry(id).or_default() += 1;
        }
    }
    for (index, approval) in approvals.iter().enumerate() {
        if let Some(call_id) = &approval.call_id {
            approvals_by_call
                .entry(call_id.as_str())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
    }
    if steps.iter().any(|step| step.call_id.is_none())
        || calls_by_id.values().any(|count| *count > 1)
        || approvals_by_call.values().any(Option::is_none)
    {
        notes.insert("Tool identity: missing, mismatched or repeated call identities cannot establish a unique approval association.".to_string());
    }
    let mut approval_used = vec![false; approvals.len()];
    let mut totals = ReceiptTotals {
        line_counts_complete: true,
        ..ReceiptTotals::default()
    };
    let mut actions: Vec<ReceiptAction> = Vec::new();
    let mut agents: HashMap<String, usize> = HashMap::new();
    let in_scope = |step_turn: Option<&str>| turn.is_none_or(|turn| step_turn == Some(turn));

    for step in &steps {
        let approval_index = step
            .call_id
            .as_deref()
            .filter(|id| !id.trim().is_empty() && calls_by_id.get(id) == Some(&1))
            .and_then(|id| approvals_by_call.get(id).copied().flatten());
        if let Some(index) = approval_index {
            approval_used[index] = true;
        }
        let approval = approval_index.map(|index| approvals[index].fact);
        if !in_scope(step.turn.as_deref()) {
            continue;
        }
        let classified = classify(step, &mut notes);
        let is_command = matches!(classified, Classified::Listed(ActionKind::Command { .. }));
        let held_at_approval = approval.is_some_and(|fact| {
            !fact.decision.ran() && fact.decision != ApprovalDecisionLabel::Pending
        });
        let status = match step.outcome {
            _ if held_at_approval => ActionStatus::NotRun,
            StepOutcome::Ok => ActionStatus::Ok,
            // A failed result is not proof the call ran: Codewhale answers a
            // call it blocks before running with an error result too.
            StepOutcome::Failed => match failure_evidence(step) {
                FailureEvidence::Ran => ActionStatus::Failed,
                FailureEvidence::Refused => ActionStatus::Blocked,
                FailureEvidence::DeniedAtApproval => ActionStatus::NotRun,
                FailureEvidence::Unclear if is_command => ActionStatus::Unknown,
                FailureEvidence::Unclear => ActionStatus::Failed,
            },
            StepOutcome::Interrupted => ActionStatus::Interrupted,
            StepOutcome::Running => ActionStatus::Running,
            StepOutcome::Unknown => ActionStatus::Unknown,
        };
        let what = match classified {
            Classified::Listed(what) => what,
            Classified::AgentFollowUp { agent_id, status } => {
                if let (Some(&index), Some(status)) = (agents.get(&agent_id), status)
                    && let ActionKind::Subagent { outcome, .. } = &mut actions[index].what
                {
                    *outcome = Some(status);
                }
                totals.other_tool_calls += 1;
                continue;
            }
            Classified::Other => {
                totals.other_tool_calls += 1;
                if !matches!(
                    status,
                    ActionStatus::Failed | ActionStatus::NotRun | ActionStatus::Blocked
                ) {
                    continue;
                }
                ActionKind::Tool
            }
        };
        if let ActionKind::Subagent {
            agent_id: Some(agent_id),
            ..
        } = &what
        {
            agents.insert(agent_id.clone(), actions.len());
        }
        let duration_ms = number(step.metadata.as_ref(), &["duration_ms"])
            .and_then(|ms| u64::try_from(ms).ok())
            .or_else(|| match (step.started_at, step.ended_at) {
                (Some(start), Some(end)) if end >= start => {
                    u64::try_from((end - start).num_milliseconds()).ok()
                }
                _ => None,
            });
        actions.push(ReceiptAction {
            seq: 0,
            turn: step.turn.clone(),
            at: step.started_at,
            call_id: step.call_id.clone(),
            tool: step.name.clone(),
            what,
            status,
            duration_ms,
            approval,
            // A block's reason is the fact worth keeping; a held call's
            // approval already says why it did not run.
            error: matches!(
                status,
                ActionStatus::Failed | ActionStatus::Unknown | ActionStatus::Blocked
            )
            .then(|| first_error_line(step.output.as_deref()))
            .flatten(),
        });
    }

    for (index, approval) in approvals.iter().enumerate() {
        if approval_used[index] {
            continue;
        }
        // Session approvals carry no turn; they are in scope only for a
        // whole-session receipt.
        if turn.is_some() && approval.turn.as_deref() != turn {
            continue;
        }
        actions.push(ReceiptAction {
            seq: 0,
            turn: approval.turn.clone(),
            at: approval.fact.at,
            call_id: approval.call_id.clone(),
            tool: approval.tool.clone(),
            what: ActionKind::Approval,
            status: if approval.fact.decision.ran() {
                ActionStatus::Ok
            } else if approval.fact.decision == ApprovalDecisionLabel::Pending {
                ActionStatus::Running
            } else {
                ActionStatus::NotRun
            },
            duration_ms: None,
            approval: Some(approval.fact),
            error: None,
        });
    }

    for (turn_id, ended_at, error) in turn_failures {
        if !in_scope(turn_id.as_deref()) {
            continue;
        }
        actions.push(ReceiptAction {
            seq: 0,
            turn: turn_id,
            at: ended_at,
            call_id: None,
            tool: "turn".to_string(),
            what: ActionKind::TurnFailed,
            status: ActionStatus::Failed,
            duration_ms: None,
            approval: None,
            error: error.map(|error| bounded(&redact(&error), MAX_ERROR_CHARS)),
        });
    }

    for (turn_id, change) in workspace_changes {
        if !in_scope(Some(&turn_id)) {
            continue;
        }
        // File tools already itemize their own paths in this turn.
        let tool_paths: BTreeSet<String> = actions
            .iter()
            .filter(|action| action.turn.as_deref() == Some(turn_id.as_str()))
            .filter(|action| action.status == ActionStatus::Ok)
            .filter_map(|action| match &action.what {
                ActionKind::FileChange { files } => Some(files),
                _ => None,
            })
            .flatten()
            .map(|file| workspace_relative(&file.path, source.workspace.as_deref()))
            .collect();
        let files: Vec<FileTouch> = change
            .files
            .into_iter()
            .filter(|file| !tool_paths.contains(&file.path))
            .collect();
        if files.is_empty() && !change.truncated {
            continue;
        }
        // Slot it after the turn's last listed action.
        let at = actions
            .iter()
            .rposition(|action| action.turn.as_deref() == Some(turn_id.as_str()))
            .map_or(actions.len(), |index| index + 1);
        actions.insert(
            at,
            ReceiptAction {
                seq: 0,
                turn: Some(turn_id),
                at: None,
                call_id: None,
                tool: "workspace".to_string(),
                what: ActionKind::WorkspaceChange {
                    files,
                    truncated: change.truncated,
                },
                status: ActionStatus::Ok,
                duration_ms: None,
                approval: None,
                error: None,
            },
        );
    }

    if kind == SourceKind::Thread {
        // Runtime actions carry timestamps; keep each turn's order and slot
        // standalone approvals and turn failures where they happened.
        actions.sort_by_key(|action| action.at);
    }
    for (index, action) in actions.iter_mut().enumerate() {
        action.seq = index + 1;
    }

    tally(&actions, &mut totals);
    if approvals_recorded {
        totals.ran_without_asking = actions
            .iter()
            .filter(|action| {
                action.ran_without_asking()
                    && action.call_id.as_deref().is_some_and(|id| {
                        !id.trim().is_empty()
                            && calls_by_id.get(id) == Some(&1)
                            && approvals_by_call.get(id).is_none_or(Option::is_some)
                    })
            })
            .count();
    }
    let mut postures: Vec<&'static str> = Vec::new();
    for (turn_id, posture) in &turn_postures {
        if let Some(posture) = posture
            && in_scope(Some(turn_id.as_str()))
            && !postures.contains(posture)
        {
            postures.push(posture);
        }
    }
    let decider_not_recorded =
        totals.approvals.approved_by.not_recorded + totals.approvals.denied_by.not_recorded;
    if decider_not_recorded > 0 {
        notes.insert(format!(
            "Who decided: {} decision(s) predate Codewhale recording the decider, or came from a sub-agent, so they show the decision without who made it.",
            decider_not_recorded
        ));
    }
    let unclear = actions
        .iter()
        .filter(|action| action.status == ActionStatus::Unknown)
        .count();
    if unclear > 0 {
        notes.insert(format!(
            "Whether it ran: {unclear} call(s) have no result, or returned an error with no exit code, so the record does not show that they started. They are listed but not counted as run."
        ));
    }

    let omitted_actions = actions.len().saturating_sub(MAX_RECEIPT_ACTIONS);
    actions.truncate(MAX_RECEIPT_ACTIONS);
    Receipt {
        schema_id: RECEIPT_SCHEMA_ID,
        source,
        turn: turn.map(str::to_string),
        postures,
        totals,
        actions,
        omitted_actions,
        not_recorded: notes.into_iter().collect(),
        claim_ceiling: CLAIM_CEILING,
    }
}

fn tally(actions: &[ReceiptAction], totals: &mut ReceiptTotals) {
    let mut changed: BTreeSet<&str> = BTreeSet::new();
    let mut created: BTreeSet<&str> = BTreeSet::new();
    let mut deleted: BTreeSet<&str> = BTreeSet::new();
    let mut outside: BTreeSet<&str> = BTreeSet::new();
    for action in actions {
        let ran = !matches!(
            action.status,
            ActionStatus::NotRun | ActionStatus::Blocked | ActionStatus::Unknown
        );
        match action.status {
            ActionStatus::Failed => totals.failures += 1,
            ActionStatus::Blocked => totals.blocked += 1,
            _ => {}
        }
        match &action.what {
            ActionKind::FileChange { files } | ActionKind::WorkspaceChange { files, .. }
                if action.status == ActionStatus::Ok =>
            {
                if matches!(action.what, ActionKind::WorkspaceChange { .. }) {
                    outside.extend(files.iter().map(|file| file.path.as_str()));
                }
                for file in files {
                    changed.insert(&file.path);
                    match file.change {
                        FileChangeKind::Created => {
                            created.insert(&file.path);
                        }
                        FileChangeKind::Deleted => {
                            deleted.insert(&file.path);
                        }
                        FileChangeKind::Edited | FileChangeKind::Written => {}
                    }
                    match (file.lines_added, file.lines_removed) {
                        (Some(added), Some(removed)) => {
                            totals.lines_added += added;
                            totals.lines_removed += removed;
                        }
                        _ => totals.line_counts_complete = false,
                    }
                }
            }
            ActionKind::Command { .. } if ran => {
                totals.commands += 1;
                if action.status == ActionStatus::Failed {
                    totals.commands_failed += 1;
                }
            }
            ActionKind::Code { .. } if ran => totals.code_runs += 1,
            ActionKind::Network { .. } if ran => totals.network += 1,
            ActionKind::Mcp { plugin, .. } if ran => {
                totals.mcp_calls += 1;
                if *plugin {
                    totals.plugin_calls += 1;
                }
            }
            ActionKind::Subagent { .. } if ran => totals.subagents += 1,
            _ => {}
        }
        if let Some(fact) = action.approval {
            let approvals = &mut totals.approvals;
            approvals.total += 1;
            match fact.decision {
                ApprovalDecisionLabel::Approved | ApprovalDecisionLabel::ApprovedWithPolicy => {
                    approvals.approved += 1
                }
                ApprovalDecisionLabel::Denied => approvals.denied += 1,
                ApprovalDecisionLabel::TimedOut => approvals.timed_out += 1,
                ApprovalDecisionLabel::Cancelled | ApprovalDecisionLabel::Unavailable => {
                    approvals.not_answered += 1
                }
                ApprovalDecisionLabel::Pending => approvals.pending += 1,
            }
            let by = match fact.decision {
                ApprovalDecisionLabel::Approved | ApprovalDecisionLabel::ApprovedWithPolicy => {
                    &mut approvals.approved_by
                }
                ApprovalDecisionLabel::Denied => &mut approvals.denied_by,
                _ => continue,
            };
            match fact.decided_by {
                Some(ApprovalDecider::User) => by.you += 1,
                Some(ApprovalDecider::SessionRule) => by.session_rule += 1,
                Some(ApprovalDecider::Posture) => by.posture += 1,
                // Codewhale never approves, and its denials are read as not
                // answered (`approvals_from_replay`), so this is only a
                // record that says neither.
                Some(ApprovalDecider::Host) | None => by.not_recorded += 1,
            }
        }
    }
    totals.files_changed = changed.len();
    totals.files_created = created.len();
    totals.files_deleted = deleted.len();
    totals.files_changed_outside_file_tools = outside.len();
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[cfg(test)]
use codewhale_commands::diagnostics_reports::receipts::code_span;
#[cfg(test)]
use codewhale_commands::diagnostics_reports::receipts::{action_line, totals_line};
pub use codewhale_commands::diagnostics_reports::receipts::{render_json, render_markdown};

#[cfg(test)]
#[path = "receipts/tests.rs"]
mod tests;
