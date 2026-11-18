use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use codewhale_localization::{Locale, MessageId, tr};

use super::{HistoryCell, SubAgentCell, ToolCell, ToolStatus};
use crate::tui::widgets::agent_card::AgentLifecycle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFold {
    pub key: usize,
    pub members: Vec<usize>,
    pub failed: usize,
    pub files: usize,
    pub added: usize,
    pub deleted: usize,
    pub duration: Option<Duration>,
}

impl TurnFold {
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.members.binary_search(&index).is_ok()
    }

    #[must_use]
    pub fn host(&self) -> Option<usize> {
        self.members.first().copied()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TurnFoldProjection {
    pub turns: Vec<TurnFold>,
    pub agents: Vec<usize>,
}

impl TurnFoldProjection {
    #[must_use]
    pub fn turn_containing(&self, index: usize) -> Option<&TurnFold> {
        self.turns.iter().find(|turn| turn.contains(index))
    }
}

#[derive(Default)]
struct OpenTurn {
    key: usize,
    members: Vec<usize>,
    agents: Vec<usize>,
    failed: usize,
    paths: BTreeSet<String>,
    added: usize,
    deleted: usize,
    live: bool,
}

pub(crate) fn superseded_todo_write_indices(
    history: &[HistoryCell],
    active_entries: &[HistoryCell],
) -> HashSet<usize> {
    let mut hidden = HashSet::new();
    let mut latest = None;

    for (index, cell) in history.iter().chain(active_entries).enumerate() {
        let HistoryCell::Tool(ToolCell::Generic(tool)) = cell else {
            continue;
        };
        if tool.name != "todo_write" || tool.status != ToolStatus::Success {
            continue;
        }

        if let Some(previous) = latest.replace(index) {
            hidden.insert(previous);
        }
    }

    if let Some(index) = latest {
        let cell = history
            .get(index)
            .or_else(|| active_entries.get(index.saturating_sub(history.len())));
        if cell.is_some_and(todo_write_snapshot_is_empty) {
            hidden.insert(index);
        }
    }

    hidden
}

fn todo_write_snapshot_is_empty(cell: &HistoryCell) -> bool {
    let HistoryCell::Tool(ToolCell::Generic(tool)) = cell else {
        return false;
    };
    let Some(output) = tool.output.as_deref() else {
        return false;
    };
    let Some(json_start) = output.find('{') else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&output[json_start..]) else {
        return false;
    };
    value
        .get("items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn infer_open_turn_user_index(
    history: &[HistoryCell],
    active_entries: &[HistoryCell],
    echoed_queued_users: usize,
) -> Option<usize> {
    let total = history.len().saturating_add(active_entries.len());
    let users: Vec<usize> = (0..total)
        .filter(|&idx| {
            history
                .get(idx)
                .or_else(|| active_entries.get(idx - history.len()))
                .is_some_and(|cell| matches!(cell, HistoryCell::User { .. }))
        })
        .collect();
    if echoed_queued_users > 0 && users.len() > echoed_queued_users {
        return Some(users[users.len() - 1 - echoed_queued_users]);
    }
    let &last_user = users.last()?;
    if last_user + 1 == total && users.len() >= 2 {
        let prev_user = users[users.len() - 2];
        let slice = (prev_user + 1)..last_user;
        let has_work = slice.clone().any(|idx| {
            history
                .get(idx)
                .is_some_and(|cell| !matches!(cell, HistoryCell::User { .. }))
        });
        let has_assistant = slice.into_iter().any(|idx| {
            history
                .get(idx)
                .is_some_and(|cell| matches!(cell, HistoryCell::Assistant { .. }))
        });
        if has_work && !has_assistant {
            return Some(prev_user);
        }
    }
    Some(last_user)
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn detect_turn_folds(
    history: &[HistoryCell],
    active_entries: &[HistoryCell],
    open_tail: bool,
    open_turn_user_index: Option<usize>,
    echoed_queued_users: usize,
    superseded_todos: &HashSet<usize>,
    durations: &HashMap<usize, Duration>,
) -> TurnFoldProjection {
    let mut projection = TurnFoldProjection::default();
    let open_from = if !open_tail {
        None
    } else if open_turn_user_index.is_some() {
        open_turn_user_index
    } else {
        infer_open_turn_user_index(history, active_entries, echoed_queued_users)
    };
    let mut current: Option<OpenTurn> = None;
    let total = history.len().saturating_add(active_entries.len());
    for index in 0..total {
        let cell = history
            .get(index)
            .or_else(|| active_entries.get(index - history.len()));
        let Some(cell) = cell else { continue };
        if matches!(cell, HistoryCell::User { .. }) {
            if let Some(turn) = current.take() {
                close_turn(turn, open_from, durations, &mut projection);
            }
            current = Some(OpenTurn {
                key: index,
                ..OpenTurn::default()
            });
            continue;
        }
        let Some(turn) = current.as_mut() else {
            continue;
        };
        if superseded_todos.contains(&index) {
            continue;
        }
        match cell {
            HistoryCell::Tool(tool) if tool.is_running() => turn.live = true,
            HistoryCell::Tool(tool) if tool.is_failed() => turn.failed += 1,
            HistoryCell::Tool(tool)
                if matches!(
                    tool.status(),
                    Some(ToolStatus::Warning | ToolStatus::Stopped)
                ) => {}
            HistoryCell::Tool(ToolCell::PatchSummary(patch)) => {
                if let Some(receipt) = patch.receipt.as_ref() {
                    turn.paths
                        .extend(receipt.files.iter().map(|file| file.path.clone()));
                    turn.added += receipt.added;
                    turn.deleted += receipt.deleted;
                }
                turn.members.push(index);
            }
            HistoryCell::Tool(tool) if tool.is_collapsible_guard() => {}
            HistoryCell::Tool(_) => {
                turn.members.push(index);
            }
            HistoryCell::SubAgent(agent) => {
                if !sub_agent_settled(agent) {
                    turn.live = true;
                } else if !sub_agent_attention(agent) {
                    turn.agents.push(index);
                }
            }
            _ => {}
        }
    }
    if let Some(turn) = current.take() {
        close_turn(turn, open_from, durations, &mut projection);
    }
    projection
}

fn close_turn(
    turn: OpenTurn,
    open_from: Option<usize>,
    durations: &HashMap<usize, Duration>,
    projection: &mut TurnFoldProjection,
) {
    if turn.live || open_from.is_some_and(|open| turn.key >= open) {
        return;
    }
    projection.agents.extend(turn.agents);
    if turn.members.is_empty() {
        return;
    }
    projection.turns.push(TurnFold {
        key: turn.key,
        members: turn.members,
        failed: turn.failed,
        files: turn.paths.len(),
        added: turn.added,
        deleted: turn.deleted,
        duration: durations.get(&turn.key).copied(),
    });
}

fn sub_agent_settled(agent: &SubAgentCell) -> bool {
    match agent {
        SubAgentCell::Delegate(card) => card.status.is_terminal(),
        SubAgentCell::Fanout(card) => card.aggregate_status_public().is_terminal(),
    }
}

fn sub_agent_attention(agent: &SubAgentCell) -> bool {
    let status = match agent {
        SubAgentCell::Delegate(card) => card.status,
        SubAgentCell::Fanout(card) => card.aggregate_status_public(),
    };
    matches!(status, AgentLifecycle::Failed | AgentLifecycle::Interrupted)
}

#[must_use]
pub fn turn_fold_headline(fold: &TurnFold, locale: Locale) -> String {
    let steps = super::tool_run::counted(
        fold.members.len(),
        locale,
        MessageId::TurnStepOne,
        MessageId::TurnStepMany,
    );
    let mut parts = Vec::new();
    if let Some(duration) = fold.duration.filter(|duration| duration.as_secs() > 0) {
        parts.push(tr(locale, MessageId::TurnWorkedFor).replace(
            "{duration}",
            &codewhale_command_contract::elapsed::format_elapsed_secs(duration.as_secs()),
        ));
    }
    parts.push(steps);
    if fold.failed > 0 {
        parts.push(
            tr(locale, MessageId::WorkflowCountFailed).replace("{count}", &fold.failed.to_string()),
        );
    }
    super::tool_run::sentence_case_activity(parts.join(" \u{00B7} "))
}

#[must_use]
pub fn turn_fold_changes(fold: &TurnFold, locale: Locale) -> Option<String> {
    (fold.files > 0).then(|| {
        let files = super::tool_run::counted(
            fold.files,
            locale,
            MessageId::TurnFileChangedOne,
            MessageId::BackgroundFilesChanged,
        );
        format!("{files} +{} -{}", fold.added, fold.deleted)
    })
}

#[must_use]
pub fn turn_fold_diff(fold: &TurnFold, cells: impl Fn(usize) -> Option<HistoryCell>) -> String {
    let mut diff = String::new();
    for &index in &fold.members {
        if let Some(HistoryCell::Tool(ToolCell::PatchSummary(patch))) = cells(index)
            && let Some(receipt) = patch.receipt.as_ref()
        {
            diff.push_str(&receipt.display_diff);
            if !diff.ends_with('\n') {
                diff.push('\n');
            }
        }
    }
    diff
}
