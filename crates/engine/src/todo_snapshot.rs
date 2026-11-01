//! Bounded renderings of a To-do snapshot.
//!
//! Codewhale has exactly one To-do list. The model learns what is on it the
//! same way it learns anything else: from its own `todo_write` /
//! `work_update` call, whose input is the whole list, confirmed by a short
//! receipt (counts and the item in progress). Both are ordinary persisted
//! transcript state. **Nothing in this module is appended to a provider
//! request**, and no step of a tool loop re-states the list. If the model
//! wants the current list, it reads its own last call or calls the tool again.
//!
//! What this module owns is the small set of places that render a snapshot
//! *once*, at an explicit seam a person asked for:
//!
//! 1. the `<codewhale:fork_state>` block a newly forked sub-agent is handed,
//! 2. `/relay` handoff instructions,
//! 3. the in-transcript agent card (display only).
//!
//! All three share [`todo_snapshot_body`] byte-for-byte, so no two surfaces
//! can disagree about what the list says.
//!
//! Rules the renderer must keep, because this text reaches a model:
//!
//! - An empty To-do renders nothing at all. Silence beats an empty list that
//!   reads as "there is no work".
//! - `update_plan` strategy state is conversational reasoning, not a second
//!   list, and never appears here.
//! - Items and characters are both hard-bounded, so a large list cannot eat the
//!   context window. The in-progress item is preserved preferentially — losing
//!   the active item is the one omission that would actively mislead.
//! - Truncation happens on `char` boundaries and marks the omission, so a
//!   multi-byte item can neither panic nor silently shrink the list.
//! - Item text can never close its wrapper: a closing tag in the `codewhale:`
//!   namespace is escaped before it reaches the model, and control characters
//!   are flattened so content cannot forge a new line.
//!
//! **What this module does not do:** it does not sanitize To-do content against
//! prompt injection, and no caller should claim that it does. The guarantees
//! above are exactly three — wrapper framing cannot be closed early, control
//! characters cannot forge the line format, and the item/character bounds hold.
//! The *meaning* of arbitrary item text is not inspected, filtered, or
//! neutralized; a To-do item containing instructions still reaches the model as
//! item text. Treating that text as untrusted data is the model contract's job
//! (the constitution), not the renderer's.

use crate::tools::todo::{SharedTodoList, TodoItem, TodoListSnapshot, TodoStatus};
use crate::work_graph::SharedWorkRuntime;

/// Maximum number of item lines rendered in the body.
pub const MAX_ITEM_LINES: usize = 24;
/// Hard character ceiling for the body (counted in `char`s).
pub const MAX_BODY_CHARS: usize = 2_000;
/// Per-item content ceiling before the omission marker is appended.
pub const MAX_ITEM_CONTENT_CHARS: usize = 160;

/// Marks any text elided by a bound.
const OMISSION_MARKER: char = '…';

/// Escaped form of a closing wrapper tag found inside item content.
const ESCAPED_CLOSE_PREFIX: &str = "<\\/codewhale:";
const CLOSE_PREFIX: &str = "</codewhale:";

/// Render the To-do snapshot body, or `None` when there is nothing on the list.
///
/// The returned string carries no framing; each seam supplies its own, so the
/// body itself stays comparable across surfaces.
#[must_use]
pub fn todo_snapshot_body(snapshot: &TodoListSnapshot) -> Option<String> {
    if snapshot.items.is_empty() {
        return None;
    }

    let header = format!("To-do ({}% settled)", snapshot.completion_pct);
    let lines: Vec<String> = snapshot.items.iter().map(item_line).collect();
    let priority = priority_order(snapshot);

    let mut selected: Vec<usize> = Vec::new();
    let mut used = header.chars().count();
    for idx in priority {
        if selected.len() >= MAX_ITEM_LINES {
            break;
        }
        let cost = 1 + lines[idx].chars().count();
        if used + cost > MAX_BODY_CHARS {
            break;
        }
        used += cost;
        selected.push(idx);
    }

    // The omission line itself costs characters, so it has to fit inside the
    // same ceiling. Drop lowest-priority selections until it does; the active
    // item sits at index 0 and is never the one dropped.
    let mut omitted = lines.len() - selected.len();
    if omitted > 0 {
        loop {
            let cost = 1 + omission_line(omitted).chars().count();
            if used + cost <= MAX_BODY_CHARS || selected.len() <= 1 {
                break;
            }
            if let Some(dropped) = selected.pop() {
                used -= 1 + lines[dropped].chars().count();
                omitted += 1;
            }
        }
    }

    selected.sort_unstable();
    let mut body = header;
    for idx in selected {
        body.push('\n');
        body.push_str(&lines[idx]);
    }
    if omitted > 0 {
        body.push('\n');
        body.push_str(&omission_line(omitted));
    }

    debug_assert!(body.chars().count() <= MAX_BODY_CHARS);
    Some(body)
}

/// The authoritative source of one agent's To-do state.
///
/// There are two stores in play and only one of them is current. When a
/// [`WorkRuntime`](crate::work_graph::WorkRuntime) owns this list, a
/// `work_update` *stages* the new projection in the graph and the legacy
/// `SharedTodoList` view is only refreshed later, asynchronously, by the UI's
/// publish step. Reading the legacy view alone therefore shows the model its
/// state from before its own last write. So: read the graph projection when the
/// runtime owns this exact list (`Arc::ptr_eq` via
/// [`WorkRuntime::matches_todos`](crate::work_graph::WorkRuntime::matches_todos)),
/// and read the list directly otherwise.
///
/// The ownership check is what keeps agents isolated. A child's runtime carries
/// its *parent's* `WorkRuntime` handle but its **own** list (#4810), so
/// `matches_todos` is false for every child and each child resolves against its
/// own store — a child can never read the parent's or a sibling's list here.
#[derive(Clone)]
pub struct TodoSource {
    work: Option<SharedWorkRuntime>,
    todos: SharedTodoList,
}

impl std::fmt::Debug for TodoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TodoSource")
            .field("graph_backed", &self.is_graph_backed())
            .finish()
    }
}

impl TodoSource {
    /// Bind a source to an agent's own list plus whatever work runtime its
    /// tool context carries.
    #[must_use]
    pub fn new(work: Option<SharedWorkRuntime>, todos: SharedTodoList) -> Self {
        Self { work, todos }
    }

    /// Whether the attached runtime actually owns this list.
    #[must_use]
    pub fn is_graph_backed(&self) -> bool {
        self.work
            .as_ref()
            .is_some_and(|work| work.matches_todos(&self.todos))
    }

    /// Current authoritative snapshot.
    ///
    /// Never omits and never fails: a graph read error degrades to the legacy
    /// view with a warning rather than dropping the list from a fork handoff,
    /// because a silently missing list reads to the model as "no work".
    pub async fn snapshot(&self) -> TodoListSnapshot {
        if let Some(work) = self.work.as_ref().filter(|_| self.is_graph_backed()) {
            match work.current_todos().await {
                Ok(snapshot) => return snapshot,
                Err(err) => tracing::warn!(
                    target: "todo_snapshot",
                    error = %err,
                    "work graph projection unavailable; falling back to the legacy To-do view"
                ),
            }
        }
        self.todos.lock().await.snapshot()
    }

    /// Body for the current authoritative snapshot.
    pub async fn body(&self) -> Option<String> {
        todo_snapshot_body(&self.snapshot().await)
    }
}

/// Maximum item rows an in-transcript agent card renders (#4810). Narrower
/// than the shared bound: a card is a glance, not the whole list.
pub const MAX_CARD_ITEM_LINES: usize = 3;
/// Per-item content ceiling on a card row.
pub const MAX_CARD_ITEM_CONTENT_CHARS: usize = 72;

/// Bounded, display-only projection of **one agent's own** To-do snapshot for
/// its delegate/agent card.
///
/// Same list, same priority order, same sanitizer as [`todo_snapshot_body`] —
/// only the framing and the bounds differ. Nothing here derives new work: every
/// row corresponds to an item that exists in the snapshot it was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoCardProjection {
    /// Bounded progress, e.g. `To-do 1/4 · 25% settled`.
    pub header: String,
    /// Item rows in document order, e.g. `[~] #2 Write the renderer`.
    pub items: Vec<String>,
    /// Items that exist in the snapshot but did not fit the card bound.
    pub omitted: usize,
}

/// Project one agent's To-do snapshot onto its card, or `None` when that agent
/// has no work to show.
///
/// An empty list returns `None` rather than a placeholder row — the same rule
/// [`todo_snapshot_body`] follows. A card that has never received a snapshot
/// and a card whose agent reported an empty list both render nothing, because
/// neither one has a task to name.
#[must_use]
pub fn card_todo_projection(snapshot: &TodoListSnapshot) -> Option<TodoCardProjection> {
    if snapshot.items.is_empty() {
        return None;
    }

    let total = snapshot.items.len();
    let settled = snapshot
        .items
        .iter()
        .filter(|item| item.status.is_settled())
        .count();
    let header = format!(
        "To-do {settled}/{total} · {}% settled",
        snapshot.completion_pct
    );

    let mut selected: Vec<usize> = priority_order(snapshot)
        .into_iter()
        .take(MAX_CARD_ITEM_LINES)
        .collect();
    selected.sort_unstable();
    let items: Vec<String> = selected
        .iter()
        .map(|idx| card_item_line(&snapshot.items[*idx]))
        .collect();

    Some(TodoCardProjection {
        omitted: total - items.len(),
        header,
        items,
    })
}

fn card_item_line(item: &TodoItem) -> String {
    format!(
        "{} #{} {}",
        status_marker(item.status),
        item.id,
        sanitize_to(&item.content, MAX_CARD_ITEM_CONTENT_CHARS)
    )
}

/// Row appended when the card bound elided items.
#[must_use]
pub fn card_omission_line(count: usize) -> String {
    format!("{OMISSION_MARKER} +{count} more")
}

/// Heading the fork-state block uses for its To-do section.
pub const FORK_TODO_SECTION_HEADING: &str = "### To-do";

/// Render the To-do section of a `<codewhale:fork_state>` block.
///
/// This is the one place a To-do snapshot is handed to a model that did not
/// produce it, and it happens exactly once — when a sub-agent is forked, as
/// part of the context block stored in that child's own history. It is not
/// refreshed, re-sent, or appended to later requests.
#[must_use]
pub fn fork_state_todo_section(body: &str) -> String {
    format!("{FORK_TODO_SECTION_HEADING}\n\n{body}\n")
}

/// Item indexes in render priority: the active (in-progress) item first, then
/// document order. Shared by every bounded projection so no two surfaces can
/// disagree about which item matters most.
fn priority_order(snapshot: &TodoListSnapshot) -> Vec<usize> {
    let active = active_index(snapshot);
    let mut priority: Vec<usize> = Vec::with_capacity(snapshot.items.len());
    if let Some(active) = active {
        priority.push(active);
    }
    priority.extend((0..snapshot.items.len()).filter(|idx| Some(*idx) != active));
    priority
}

fn active_index(snapshot: &TodoListSnapshot) -> Option<usize> {
    snapshot
        .in_progress_id
        .and_then(|id| snapshot.items.iter().position(|item| item.id == id))
        .or_else(|| {
            snapshot
                .items
                .iter()
                .position(|item| item.status == TodoStatus::InProgress)
        })
}

fn status_marker(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "[ ]",
        TodoStatus::InProgress => "[~]",
        TodoStatus::Completed => "[x]",
        TodoStatus::Cancelled => "[-]",
    }
}

fn item_line(item: &TodoItem) -> String {
    // IDs stay visible: `work_update` addresses later transitions by stable
    // item identity, so a body without IDs is not actionable.
    format!(
        "- {} #{} {}",
        status_marker(item.status),
        item.id,
        sanitize(&item.content)
    )
}

fn omission_line(count: usize) -> String {
    format!("- {OMISSION_MARKER} +{count} more To-do items omitted")
}

fn sanitize(content: &str) -> String {
    sanitize_to(content, MAX_ITEM_CONTENT_CHARS)
}

fn sanitize_to(content: &str, max_chars: usize) -> String {
    let flattened: String = content
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let escaped = escape_wrapper(&flattened);
    truncate_chars(escaped.trim(), max_chars)
}

/// Neutralize any closing tag in the `codewhale:` namespace so item content
/// cannot terminate a wrapper early and smuggle instructions past it.
fn escape_wrapper(content: &str) -> String {
    if !content.to_ascii_lowercase().contains(CLOSE_PREFIX) {
        return content.to_string();
    }

    let lower = content.to_ascii_lowercase();
    let mut out = String::with_capacity(content.len() + 8);
    let mut cursor = 0usize;
    while let Some(found) = lower[cursor..].find(CLOSE_PREFIX) {
        let at = cursor + found;
        out.push_str(&content[cursor..at]);
        out.push_str(ESCAPED_CLOSE_PREFIX);
        cursor = at + CLOSE_PREFIX.len();
    }
    out.push_str(&content[cursor..]);
    out
}

/// Truncate on `char` boundaries, marking the omission. Never splits a
/// multi-byte scalar.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push(OMISSION_MARKER);
    out
}

#[cfg(test)]
mod tests;
