use chrono::{DateTime, Utc};
use codewhale_models::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
pub const CURRENT_JOURNAL_SCHEMA_VERSION: u32 = 1;
pub type EntryId = String;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SpawnDepth(pub u32);
impl SpawnDepth {
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEntryKind {
    Message {
        message: Message,
    },
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Compaction {
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_after: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    BranchSummary {
        branch_id: String,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_branch_id: Option<String>,
    },
    System {
        content: String,
    },
}
impl SessionEntryKind {
    pub fn is_contextual(&self) -> bool {
        matches!(
            self,
            Self::Message { .. } | Self::User { .. } | Self::Assistant { .. }
        )
    }
    /// `self.as_message() == Some(message)` without materializing the
    /// projection. Every autosave compares the whole active branch with the
    /// live transcript, so the common `Message` entry must not be deep-cloned
    /// just to be compared.
    pub fn projects_to(&self, message: &Message) -> bool {
        match self {
            Self::Message { message: own } => own == message,
            other => other.as_message().as_ref() == Some(message),
        }
    }
    pub fn as_message(&self) -> Option<Message> {
        match self {
            Self::Message { message } => Some(message.clone()),
            Self::User { text } => Some(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                }],
            }),
            Self::Assistant { text } => Some(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                }],
            }),
            Self::Compaction { summary, .. } => Some(Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: format!("[compaction summary] {summary}"),
                    cache_control: None,
                }],
            }),
            Self::BranchSummary { summary, .. } => Some(Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: format!("[branch summary] {summary}"),
                    cache_control: None,
                }],
            }),
            Self::System { content } => Some(Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: content.clone(),
                    cache_control: None,
                }],
            }),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEntry {
    pub id: EntryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<EntryId>,
    #[serde(flatten)]
    pub kind: Arc<SessionEntryKind>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub spawn_depth: u32,
}
impl SessionEntry {
    pub fn new(kind: SessionEntryKind, parent_id: Option<EntryId>, spawn_depth: u32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id,
            kind: Arc::new(kind),
            created_at: Utc::now(),
            spawn_depth,
        }
    }
    pub fn short_id(&self) -> &str {
        char_prefix(&self.id, 8)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rebranch {
    pub previous_active: usize,
    pub shared_prefix: usize,
}

impl Rebranch {
    #[must_use]
    pub fn replaced_from_root(&self) -> bool {
        self.previous_active > 0 && self.shared_prefix == 0
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionJournal {
    #[serde(default)]
    pub entries: Vec<SessionEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<EntryId>,
    #[serde(default = "default_journal_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub spawn_depth: u32,
}
fn default_journal_schema_version() -> u32 {
    CURRENT_JOURNAL_SCHEMA_VERSION
}
impl SessionJournal {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            leaf_id: None,
            schema_version: CURRENT_JOURNAL_SCHEMA_VERSION,
            spawn_depth: 0,
        }
    }
    pub fn with_spawn_depth(depth: u32) -> Self {
        Self {
            spawn_depth: depth,
            ..Self::new()
        }
    }
    pub fn append(&mut self, kind: SessionEntryKind) -> EntryId {
        self.append_stamped(kind, Utc::now())
    }
    /// Append an entry carrying the time the underlying event happened, not
    /// the time the save ran. `created_at` is the journal's timeline; stamping
    /// it at append is what keeps a rebuilt journal honest — a save must never
    /// rewrite an entry's time to the moment it was written.
    pub fn append_stamped(&mut self, kind: SessionEntryKind, created_at: DateTime<Utc>) -> EntryId {
        let mut entry = SessionEntry::new(kind, self.leaf_id.clone(), self.spawn_depth);
        entry.created_at = created_at;
        let id = entry.id.clone();
        self.entries.push(entry);
        self.leaf_id = Some(id.clone());
        id
    }
    pub fn append_message(&mut self, message: Message) -> EntryId {
        self.append(SessionEntryKind::Message { message })
    }
    pub fn append_compaction(
        &mut self,
        summary: String,
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        model: Option<String>,
    ) -> EntryId {
        self.append(SessionEntryKind::Compaction {
            summary,
            tokens_before,
            tokens_after,
            model,
        })
    }
    pub fn append_branch_summary(
        &mut self,
        branch_id: String,
        summary: String,
        parent_branch_id: Option<String>,
    ) -> EntryId {
        self.append(SessionEntryKind::BranchSummary {
            branch_id,
            summary,
            parent_branch_id,
        })
    }
    pub fn branch_to(&mut self, entry_id: &str) -> Result<(), String> {
        if self.entries.iter().any(|e| e.id == entry_id) {
            self.leaf_id = Some(entry_id.to_string());
            Ok(())
        } else {
            Err(format!("entry {entry_id} not found"))
        }
    }
    pub fn fork_from(&self, from_entry_id: Option<&str>) -> Result<Self, String> {
        let leaf = if let Some(id) = from_entry_id {
            if !self.entries.iter().any(|e| e.id == id) {
                return Err(format!("fork source {id} not found"));
            }
            Some(id.to_string())
        } else {
            self.leaf_id.clone()
        };
        Ok(Self {
            entries: self.entries.clone(),
            leaf_id: leaf,
            schema_version: self.schema_version,
            spawn_depth: self.spawn_depth.saturating_add(1),
        })
    }
    pub fn index(&self) -> HashMap<&str, &SessionEntry> {
        self.entries.iter().map(|e| (e.id.as_str(), e)).collect()
    }
    pub fn children_of(&self, parent_id: Option<&str>) -> Vec<&SessionEntry> {
        self.entries
            .iter()
            .filter(|e| e.parent_id.as_deref() == parent_id)
            .collect()
    }
    pub fn contains(&self, entry_id: &str) -> bool {
        self.entries.iter().any(|e| e.id == entry_id)
    }
    pub fn leaf(&self) -> Option<&SessionEntry> {
        self.leaf_id
            .as_deref()
            .and_then(|id| self.entries.iter().find(|e| e.id == id))
    }
    pub fn root_to_leaf(&self) -> Vec<&SessionEntry> {
        let index: HashMap<&str, &SessionEntry> =
            self.entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let mut path = Vec::new();
        let mut cur = self.leaf_id.as_deref();
        let mut seen = HashSet::new();
        while let Some(id) = cur {
            if !seen.insert(id) {
                break;
            }
            if let Some(entry) = index.get(id) {
                path.push(*entry);
                cur = entry.parent_id.as_deref();
            } else {
                break;
            }
        }
        path.reverse();
        path
    }
    pub fn active_messages(&self, include_system: bool) -> Vec<Message> {
        self.root_to_leaf()
            .into_iter()
            .filter_map(|e| {
                if !include_system && !e.kind.is_contextual() {
                    return None;
                }
                e.kind.as_message()
            })
            .collect()
    }
    pub fn leaves(&self) -> Vec<&SessionEntry> {
        let parents: HashSet<&str> = self
            .entries
            .iter()
            .filter_map(|e| e.parent_id.as_deref())
            .collect();
        self.entries
            .iter()
            .filter(|e| !parents.contains(e.id.as_str()))
            .collect()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn validate(&self) -> Result<(), String> {
        let ids: HashSet<&str> = self.entries.iter().map(|e| e.id.as_str()).collect();
        for entry in &self.entries {
            if let Some(parent) = entry.parent_id.as_deref()
                && !ids.contains(parent)
            {
                return Err(format!("entry {} missing parent {}", entry.id, parent));
            }
        }
        if let Some(leaf) = self.leaf_id.as_deref()
            && !ids.contains(leaf)
        {
            return Err(format!("leaf {leaf} not found"));
        }
        Ok(())
    }
    pub fn from_messages(messages: Vec<Message>, spawn_depth: u32) -> Self {
        let mut j = Self::with_spawn_depth(spawn_depth);
        for msg in messages {
            j.append(SessionEntryKind::Message { message: msg });
        }
        j
    }
    /// Build the journal honoring a per-message append stamp. `stamps[i]` is
    /// the time `messages[i]` entered the conversation; a missing stamp falls
    /// back to now, so a drifted caller degrades to save-time ordering rather
    /// than dropping the message.
    pub fn from_messages_stamped(
        messages: Vec<Message>,
        stamps: &[DateTime<Utc>],
        spawn_depth: u32,
    ) -> Self {
        let mut j = Self::with_spawn_depth(spawn_depth);
        for (index, msg) in messages.into_iter().enumerate() {
            let created_at = stamps.get(index).copied().unwrap_or_else(Utc::now);
            j.append_stamped(SessionEntryKind::Message { message: msg }, created_at);
        }
        j
    }
    pub fn to_messages(&self) -> Vec<Message> {
        self.active_messages(true)
    }

    /// Make `messages` the active projection without rewriting the journal.
    ///
    /// The existing active branch remains as evidence. We reuse its longest
    /// unchanged prefix, then append the repaired suffix as a sibling branch.
    pub fn rebranch_active_messages(&mut self, messages: &[Message]) -> Rebranch {
        self.rebranch_active_messages_stamped(messages, &[])
    }

    /// Preserve existing entry identity and timestamps; only append the changed
    /// suffix, keeping the previous branch reachable.
    pub fn rebranch_active_messages_stamped(
        &mut self,
        messages: &[Message],
        stamps: &[DateTime<Utc>],
    ) -> Rebranch {
        let active_path = self.root_to_leaf();
        let previous_active = active_path.len();
        let shared_prefix = active_path
            .iter()
            .zip(messages)
            .take_while(|(entry, message)| entry.kind.projects_to(message))
            .count();
        self.leaf_id = shared_prefix
            .checked_sub(1)
            .map(|index| active_path[index].id.clone());
        for (index, message) in messages.iter().enumerate().skip(shared_prefix) {
            self.append_stamped(
                SessionEntryKind::Message {
                    message: message.clone(),
                },
                stamps.get(index).copied().unwrap_or_else(Utc::now),
            );
        }
        Rebranch {
            previous_active,
            shared_prefix,
        }
    }

    /// Ids a bounded save may move out of this journal, in journal order.
    ///
    /// Empty until more than `dead_threshold` entries sit off the active
    /// branch — superseded versions left behind by compaction, undo and
    /// edits (#6842). Past it, the plan keeps the root→leaf path, then whole
    /// off-branch chains (an entry with every ancestor) newest first, with
    /// `Compaction`/`BranchSummary` chains ahead of plain leaves, while the
    /// kept off-branch total stays within `dead_budget`. A kept entry never
    /// loses its parent, so the remainder always passes [`Self::validate`].
    ///
    /// Pure: callers archive the planned entries durably before removing
    /// them with [`Self::remove_entries`]. Known limit: the active branch
    /// itself is never pruned, so a session that never compacts stays as
    /// long as its conversation.
    pub fn prune_plan(&self, dead_threshold: usize, dead_budget: usize) -> Vec<EntryId> {
        let active = self.root_to_leaf();
        if self.entries.len().saturating_sub(active.len()) <= dead_threshold {
            return Vec::new();
        }
        let index: HashMap<&str, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.as_str(), i))
            .collect();
        let mut keep = vec![false; self.entries.len()];
        for entry in &active {
            if let Some(&i) = index.get(entry.id.as_str()) {
                keep[i] = true;
            }
        }
        let parents: HashSet<&str> = self
            .entries
            .iter()
            .filter_map(|e| e.parent_id.as_deref())
            .collect();
        let summary = |e: &SessionEntry| {
            matches!(
                *e.kind,
                SessionEntryKind::Compaction { .. } | SessionEntryKind::BranchSummary { .. }
            )
        };
        // Newest first: journal order is append order.
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, e)| summary(e))
            .chain(
                self.entries
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, e)| !parents.contains(e.id.as_str())),
            );
        let mut used = 0usize;
        for (start, _) in candidates {
            if keep[start] {
                continue;
            }
            let mut chain = Vec::new();
            let mut seen = HashSet::new();
            let mut cursor = Some(start);
            let mut closed = true;
            while let Some(i) = cursor {
                if keep[i] {
                    break;
                }
                if !seen.insert(i) || used + chain.len() >= dead_budget {
                    closed = false;
                    break;
                }
                chain.push(i);
                cursor = match self.entries[i].parent_id.as_deref() {
                    None => None,
                    Some(parent) => match index.get(parent) {
                        Some(&p) => Some(p),
                        None => {
                            closed = false;
                            None
                        }
                    },
                };
            }
            if closed {
                used += chain.len();
                for i in chain {
                    keep[i] = true;
                }
            }
        }
        self.entries
            .iter()
            .zip(keep)
            .filter(|(_, kept)| !kept)
            .map(|(e, _)| e.id.clone())
            .collect()
    }

    /// Remove `ids` atomically: either every listed entry leaves and the
    /// remainder still validates, or the journal is left exactly as it was
    /// and the validation error is returned. Returns how many were removed.
    pub fn remove_entries(&mut self, ids: &HashSet<EntryId>) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        let original = std::mem::take(&mut self.entries);
        let mut removed = Vec::new();
        for (position, entry) in original.into_iter().enumerate() {
            if ids.contains(&entry.id) {
                removed.push((position, entry));
            } else {
                self.entries.push(entry);
            }
        }
        if let Err(error) = self.validate() {
            // Ascending positions put every entry back where it was.
            for (position, entry) in removed {
                self.entries.insert(position, entry);
            }
            return Err(error);
        }
        Ok(removed.len())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionImportContainer {
    pub format_version: u32,
    pub source: String,
    pub metadata: Option<serde_json::Value>,
    pub entries: Vec<SessionEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<EntryId>,
    pub exported_at: DateTime<Utc>,
    #[serde(default)]
    pub spawn_depth: u32,
}
impl SessionImportContainer {
    pub fn new(
        source: String,
        journal: &SessionJournal,
        metadata: Option<serde_json::Value>,
    ) -> Self {
        Self {
            format_version: CURRENT_JOURNAL_SCHEMA_VERSION,
            source,
            metadata,
            entries: journal.entries.clone(),
            leaf_id: journal.leaf_id.clone(),
            exported_at: Utc::now(),
            spawn_depth: journal.spawn_depth,
        }
    }
    pub fn into_journal(self) -> Result<SessionJournal, String> {
        let j = SessionJournal {
            entries: self.entries,
            leaf_id: self.leaf_id,
            schema_version: self.format_version,
            spawn_depth: self.spawn_depth,
        };
        j.validate()?;
        Ok(j)
    }
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}
/// The first `chars` characters of `text`. Ids come from imported journals as
/// well as from this process, so a byte slice could split a character.
fn char_prefix(text: &str, chars: usize) -> &str {
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

/// Render the journal as a tree, one line per reachable entry.
///
/// Indentation grows only where the history forks: an entry with a single
/// child is followed by that child at the same depth, so a long linear
/// session stays flat instead of drawing entry N at depth N. Each fork on the
/// way down still adds one guide column. Several roots (a rewind to before
/// the first message starts a new one) are drawn as siblings. The walk uses
/// an explicit stack, so no session length can exhaust the thread's stack.
pub fn render_tree(journal: &SessionJournal) -> String {
    if journal.entries.is_empty() {
        return "(empty session — no entries yet)".to_string();
    }
    let mut out = String::new();
    let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        children
            .entry(entry.parent_id.as_deref())
            .or_default()
            .push(index);
    }
    let active_ids: HashSet<&str> = journal
        .root_to_leaf()
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    let leaf = journal.leaf_id.as_deref();
    // (entry index, guide columns inherited from forks above, connector)
    let mut stack: Vec<(usize, String, &'static str)> = Vec::new();
    push_tree_children(&mut stack, children.get(&None), "");
    // Duplicate ids in an imported journal would otherwise revisit a subtree
    // forever; each entry is drawn at most once.
    let mut drawn = vec![false; journal.entries.len()];
    while let Some((index, guide, connector)) = stack.pop() {
        if std::mem::replace(&mut drawn[index], true) {
            continue;
        }
        let entry = &journal.entries[index];
        let marker = if Some(entry.id.as_str()) == leaf {
            "*"
        } else if active_ids.contains(entry.id.as_str()) {
            "●"
        } else {
            "○"
        };
        out.push_str(&format!(
            "{guide}{connector}{marker} {} [{}] {}\n",
            entry.short_id(),
            entry.id,
            tree_label(&entry.kind)
        ));
        let child_guide = match connector {
            "├─ " => format!("{guide}│  "),
            "└─ " => format!("{guide}   "),
            _ => guide,
        };
        push_tree_children(
            &mut stack,
            children.get(&Some(entry.id.as_str())),
            &child_guide,
        );
    }
    if let Some(leaf_id) = leaf {
        out.push_str(&format!(
            "\nleaf: {leaf_id} (active, {} entries)\n",
            journal.entries.len()
        ));
    }
    // Entries whose parents form a loop hang off no root and cannot be drawn;
    // say so instead of letting the total above silently disagree.
    let unreachable = drawn.iter().filter(|drawn| !**drawn).count();
    if unreachable > 0 {
        out.push_str(&format!(
            "{unreachable} entries not reachable from a root\n"
        ));
    }
    out
}

/// Queue `nodes` so they pop in journal order. A lone child (or a lone root)
/// continues its line of history without a connector; siblings get one each.
fn push_tree_children(
    stack: &mut Vec<(usize, String, &'static str)>,
    nodes: Option<&Vec<usize>>,
    guide: &str,
) {
    let Some(nodes) = nodes else {
        return;
    };
    let forked = nodes.len() > 1;
    for (position, &index) in nodes.iter().enumerate().rev() {
        let connector = if !forked {
            ""
        } else if position + 1 == nodes.len() {
            "└─ "
        } else {
            "├─ "
        };
        stack.push((index, guide.to_string(), connector));
    }
}

fn tree_label(kind: &SessionEntryKind) -> String {
    match kind {
        SessionEntryKind::Message { message } => {
            let role = &message.role;
            let meta_index = codewhale_models::turn_classify::turn_metadata_text(message)
                .filter(|_| message.role == Role::User)
                .map(|(index, _)| index);
            let snippet: String = message
                .content
                .iter()
                .enumerate()
                .filter_map(|(index, b)| match b {
                    ContentBlock::Text { text, .. } if Some(index) != meta_index => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{role}: {}", label_snippet(&snippet))
        }
        SessionEntryKind::User { text } => format!("user: {}", label_snippet(text)),
        SessionEntryKind::Assistant { text } => format!("assistant: {}", label_snippet(text)),
        SessionEntryKind::Compaction { summary, .. } => {
            format!("compaction: {}", label_snippet(summary))
        }
        SessionEntryKind::BranchSummary {
            branch_id, summary, ..
        } => format!(
            "branch:{} {}",
            char_prefix(branch_id, 8),
            label_snippet(summary)
        ),
        SessionEntryKind::System { content } => format!("system: {}", label_snippet(content)),
    }
}

/// The first 60 characters of an entry, kept on its one tree line: a line
/// break, tab or other control character reads as a space, and bidirectional
/// format characters, which would reorder the rest of the row, are dropped.
fn label_snippet(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                ch,
                '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            )
        })
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                ch
            }
        })
        .take(60)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::{ContentBlock, Message, Role};
    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }
    #[test]
    fn append_creates_child_of_leaf() {
        let mut j = SessionJournal::new();
        let a = j.append(SessionEntryKind::User {
            text: "hello".into(),
        });
        let b = j.append(SessionEntryKind::Assistant {
            text: "world".into(),
        });
        assert_eq!(j.leaf_id.as_deref(), Some(b.as_str()));
        let be = j.entries.iter().find(|e| e.id == b).unwrap();
        assert_eq!(be.parent_id.as_deref(), Some(a.as_str()));
    }
    #[test]
    fn branch_moves_leaf_only() {
        let mut j = SessionJournal::new();
        let a = j.append(SessionEntryKind::User { text: "a".into() });
        let b = j.append(SessionEntryKind::User { text: "b".into() });
        let _c = j.append(SessionEntryKind::User { text: "c".into() });
        j.branch_to(&a).unwrap();
        let d = j.append(SessionEntryKind::User { text: "d".into() });
        assert_eq!(j.entries.len(), 4);
        assert_eq!(j.leaf_id.as_deref(), Some(d.as_str()));
        let path: Vec<String> = j.root_to_leaf().iter().map(|e| e.id.clone()).collect();
        assert_eq!(path, vec![a.clone(), d.clone()]);
        assert!(j.entries.iter().any(|e| e.id == b));
    }
    #[test]
    fn from_messages_migrates() {
        let msgs = vec![msg("user", "hi"), msg("assistant", "hello")];
        let j = SessionJournal::from_messages(msgs, 0);
        assert_eq!(j.entries.len(), 2);
        assert!(j.validate().is_ok());
        assert_eq!(j.root_to_leaf().len(), 2);
    }
    #[test]
    fn repaired_messages_form_an_append_only_sibling_branch() {
        let original = vec![
            msg("user", "shared"),
            msg("assistant", "broken"),
            msg("user", "old tail"),
        ];
        let mut journal = SessionJournal::from_messages(original, 0);
        let old_leaf = journal.leaf_id.clone().expect("old leaf");
        let repaired = vec![
            msg("user", "shared"),
            msg("assistant", "repaired"),
            msg("user", "new tail"),
        ];

        journal.rebranch_active_messages(&repaired);

        assert_eq!(journal.to_messages(), repaired);
        assert!(journal.contains(&old_leaf), "old evidence must remain");
        assert_eq!(
            journal.entries.len(),
            5,
            "one shared entry plus two branches"
        );
    }
    #[test]
    fn stamped_rebranch_keeps_prefix_identity_and_suffix_stamps() {
        let stamp = |secs: i64| DateTime::from_timestamp(secs, 0).expect("stamp");
        let mut j = SessionJournal::new();
        j.append_stamped(
            SessionEntryKind::Message {
                message: msg("user", "a"),
            },
            stamp(100),
        );
        j.append_stamped(
            SessionEntryKind::Message {
                message: msg("assistant", "b"),
            },
            stamp(200),
        );
        let a_id = j.entries[0].id.clone();
        j.rebranch_active_messages_stamped(
            &[msg("user", "a"), msg("assistant", "b2")],
            &[stamp(100), stamp(300)],
        );
        assert_eq!(j.entries.len(), 3);
        assert_eq!(j.entries[0].id, a_id, "shared prefix keeps its id");
        assert_eq!(j.entries[0].created_at, stamp(100));
        let path = j.root_to_leaf();
        assert_eq!(path.len(), 2);
        assert_eq!(
            path[1].created_at,
            stamp(300),
            "suffix keeps the live stamp"
        );
        assert!(
            j.entries
                .iter()
                .any(|e| e.kind.as_message().as_ref() == Some(&msg("assistant", "b"))),
            "replaced suffix survives as a sibling"
        );
    }
    #[test]
    fn compaction_fits() {
        let mut j = SessionJournal::new();
        let id = j.append_compaction("summary".into(), Some(1000), Some(100), None);
        assert!(j.contains(&id));
        assert!(matches!(
            *j.leaf().unwrap().kind,
            SessionEntryKind::Compaction { .. }
        ));
    }
    #[test]
    fn branch_summary_fits() {
        let mut j = SessionJournal::new();
        let a = j.append(SessionEntryKind::User {
            text: "root".into(),
        });
        let b = j.append_branch_summary(a.clone(), "branch summary".into(), None);
        assert!(j.contains(&b));
    }
    #[test]
    fn spawn_depth_fork() {
        let mut j = SessionJournal::with_spawn_depth(1);
        let forked = j.fork_from(None).unwrap();
        assert_eq!(forked.spawn_depth, 2);
        let a = j.append(SessionEntryKind::User {
            text: "root".into(),
        });
        let fork2 = j.fork_from(Some(&a)).unwrap();
        assert_eq!(fork2.spawn_depth, 2);
    }
    #[test]
    fn foreign_roundtrip() {
        let mut j = SessionJournal::new();
        j.append(SessionEntryKind::User {
            text: "hello".into(),
        });
        let c = SessionImportContainer::new("codewhale".into(), &j, None);
        let json = c.to_json().unwrap();
        let back = SessionImportContainer::from_json(&json).unwrap();
        let j2 = back.into_journal().unwrap();
        assert_eq!(j.entries.len(), j2.entries.len());
    }
    #[test]
    fn render_marks_active() {
        let mut j = SessionJournal::new();
        j.append(SessionEntryKind::User {
            text: "root".into(),
        });
        let tree = render_tree(&j);
        assert!(tree.contains('*'));
    }
    #[test]
    fn render_keeps_a_linear_session_flat() {
        let mut j = SessionJournal::new();
        for i in 0..200 {
            j.append(SessionEntryKind::User {
                text: format!("turn {i}"),
            });
        }
        let tree = render_tree(&j);
        let entry_lines: Vec<&str> = tree.lines().take(200).collect();
        for (i, line) in entry_lines.iter().enumerate() {
            assert!(
                line.ends_with(&format!("user: turn {i}")),
                "entry {i} out of order: {line:?}"
            );
            assert!(
                line.starts_with(['●', '*']),
                "a linear history must not indent: {line:?}"
            );
        }
    }

    #[test]
    fn render_indents_only_at_forks() {
        let mut j = SessionJournal::new();
        let a = j.append(SessionEntryKind::User { text: "a".into() });
        j.append(SessionEntryKind::User { text: "b".into() });
        j.append(SessionEntryKind::User { text: "c".into() });
        j.branch_to(&a).unwrap();
        j.append(SessionEntryKind::User { text: "d".into() });
        j.append(SessionEntryKind::User { text: "e".into() });
        let tree = render_tree(&j);
        let shape: Vec<String> = tree
            .lines()
            .take(5)
            .map(|line| {
                let (head, label) = line.split_once(" [").unwrap();
                let prefix: String = head.chars().take_while(|c| !c.is_alphanumeric()).collect();
                format!("{prefix}{}", label.rsplit(": ").next().unwrap())
            })
            .collect();
        assert_eq!(
            shape,
            ["● a", "├─ ○ b", "│  ○ c", "└─ ● d", "   * e"],
            "{tree}"
        );
    }

    #[test]
    fn render_draws_several_roots_as_siblings() {
        let mut j = SessionJournal::new();
        j.append(SessionEntryKind::User { text: "r1".into() });
        j.append(SessionEntryKind::User { text: "c1".into() });
        // A rewind to before the first message appends a second root.
        j.leaf_id = None;
        j.append(SessionEntryKind::User { text: "r2".into() });
        j.append(SessionEntryKind::User { text: "d1".into() });
        let tree = render_tree(&j);
        let shape: Vec<String> = tree
            .lines()
            .take(4)
            .map(|line| {
                let (head, label) = line.split_once(" [").unwrap();
                let prefix: String = head.chars().take_while(|c| !c.is_alphanumeric()).collect();
                format!("{prefix}{}", label.rsplit(": ").next().unwrap())
            })
            .collect();
        assert_eq!(
            shape,
            ["├─ ○ r1", "│  ○ c1", "└─ ● r2", "   * d1"],
            "{tree}"
        );
    }

    #[test]
    fn render_keeps_each_entry_on_one_line_whatever_its_text_or_id() {
        let mut j = SessionJournal::new();
        j.append(SessionEntryKind::User {
            text: "first line\nsecond line\r\n\tthird\u{202E}".into(),
        });
        // Imported ids need not be ASCII; a byte slice would split a char.
        j.entries[0].id = "会話会話会話会話".into();
        j.leaf_id = Some(j.entries[0].id.clone());
        j.append(SessionEntryKind::BranchSummary {
            branch_id: "枝枝枝枝枝枝枝枝枝".into(),
            summary: "multi\nline".into(),
            parent_branch_id: None,
        });
        let tree = render_tree(&j);
        let lines: Vec<&str> = tree.lines().collect();
        assert_eq!(
            lines[0], "● 会話会話会話会話 [会話会話会話会話] user: first line second line   third",
            "{tree}"
        );
        assert!(
            lines[1].ends_with("branch:枝枝枝枝枝枝枝枝 multi line"),
            "{tree}"
        );
        assert_eq!(lines.len(), 2 + 2, "two entries plus the footer: {tree}");
    }

    #[test]
    fn render_draws_a_repeated_id_once_and_counts_unreachable_entries() {
        let mut j = SessionJournal::new();
        let a = j.append(SessionEntryKind::User { text: "a".into() });
        j.append(SessionEntryKind::User { text: "b".into() });
        // An imported journal repeating `a`'s id under `a` itself: following
        // children by id would revisit `a`'s subtree forever.
        let mut repeat = j.entries[0].clone();
        repeat.parent_id = Some(a.clone());
        j.entries.push(repeat);
        // Two entries that are each other's parent hang off no root.
        let mut x = SessionEntry::new(SessionEntryKind::User { text: "x".into() }, None, 0);
        let mut y = SessionEntry::new(SessionEntryKind::User { text: "y".into() }, None, 0);
        x.parent_id = Some(y.id.clone());
        y.parent_id = Some(x.id.clone());
        j.entries.extend([x, y]);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(render_tree(&j)).ok());
        let tree = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("a repeated id must not make the walk revisit a subtree forever");
        let entry_lines = tree.lines().take_while(|line| !line.is_empty()).count();
        assert_eq!(
            entry_lines, 3,
            "each journal entry is drawn at most once: {tree}"
        );
        assert!(
            tree.ends_with("2 entries not reachable from a root\n"),
            "{tree}"
        );
    }

    #[test]
    fn render_handles_a_long_session_on_a_small_stack() {
        let mut j = SessionJournal::new();
        for i in 0..20_000 {
            j.append(SessionEntryKind::User {
                text: format!("turn {i}"),
            });
        }
        // One recursion frame per entry overflows this long before the end.
        let lines = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || render_tree(&j).lines().count())
            .unwrap()
            .join()
            .expect("rendering must not exhaust the stack");
        assert_eq!(lines, 20_000 + 2);
    }

    #[test]
    fn active_messages_root_to_leaf() {
        let mut j = SessionJournal::new();
        j.append(SessionEntryKind::User { text: "a".into() });
        j.append(SessionEntryKind::User { text: "b".into() });
        let msgs = j.active_messages(false);
        assert_eq!(msgs.len(), 2);
        j.branch_to(&j.entries[0].id.clone()).unwrap();
        j.append(SessionEntryKind::User { text: "c".into() });
        let msgs2 = j.active_messages(false);
        assert_eq!(msgs2.len(), 2);
    }

    #[test]
    fn projects_to_matches_as_message_equality_for_every_kind() {
        let kinds = [
            SessionEntryKind::Message {
                message: msg("assistant", "hi"),
            },
            SessionEntryKind::User {
                text: "hi".to_string(),
            },
            SessionEntryKind::Assistant {
                text: "hi".to_string(),
            },
            SessionEntryKind::System {
                content: "hi".to_string(),
            },
        ];
        let probes = [
            msg("assistant", "hi"),
            msg("user", "hi"),
            msg("system", "hi"),
            msg("assistant", "other"),
        ];
        for kind in &kinds {
            for probe in &probes {
                assert_eq!(
                    kind.projects_to(probe),
                    kind.as_message().as_ref() == Some(probe),
                    "{kind:?} vs {probe:?}"
                );
            }
        }
    }

    /// A compaction-heavy session: each round rebranches the whole tail, so
    /// superseded copies pile up off the active branch (#6842).
    fn compacted_journal(rounds: usize, tail: usize) -> SessionJournal {
        let mut j = SessionJournal::new();
        for round in 0..rounds {
            let messages: Vec<Message> = (0..tail)
                .map(|i| msg("user", &format!("round {round} message {i}")))
                .collect();
            j.rebranch_active_messages(&messages);
        }
        j
    }

    #[test]
    fn prune_plan_is_empty_under_the_threshold() {
        let j = compacted_journal(4, 10);
        assert_eq!(j.len(), 40);
        assert!(j.prune_plan(30, 5).is_empty());
    }

    #[test]
    fn prune_plan_keeps_active_branch_and_whole_recent_chains() {
        let mut j = compacted_journal(50, 10);
        let active_before = j.active_messages(true);
        let leaf = j.leaf_id.clone();
        let plan = j.prune_plan(100, 25);
        // 490 dead entries; 2 whole 10-entry chains fit a 25 budget.
        assert_eq!(plan.len(), 490 - 20);
        let ids: HashSet<EntryId> = plan.into_iter().collect();
        assert_eq!(j.remove_entries(&ids).unwrap(), 470);
        assert_eq!(j.len(), 30);
        j.validate().unwrap();
        assert_eq!(j.leaf_id, leaf);
        assert_eq!(j.active_messages(true), active_before);
        // The newest superseded round survives, not an arbitrary one.
        let kept: Vec<String> = j
            .entries
            .iter()
            .filter_map(|e| e.kind.as_message())
            .map(|m| match &m.content[0] {
                ContentBlock::Text { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(kept.iter().any(|t| t.starts_with("round 48 ")), "{kept:?}");
        assert!(!kept.iter().any(|t| t.starts_with("round 0 ")), "{kept:?}");
    }

    #[test]
    fn prune_plan_prefers_summary_chains() {
        let mut j = compacted_journal(30, 4);
        let leaf = j.leaf_id.clone().unwrap();
        // An old compaction hanging off the very first entry.
        let first = j.entries[0].id.clone();
        j.branch_to(&first).unwrap();
        let summary = j.append_compaction("old summary".into(), None, None, None);
        j.branch_to(&leaf).unwrap();
        let plan = j.prune_plan(10, 2);
        assert!(!plan.contains(&summary));
        assert!(!plan.contains(&first));
    }

    #[test]
    fn remove_entries_refuses_to_orphan_and_leaves_journal_untouched() {
        let mut j = compacted_journal(3, 3);
        let before = j.clone();
        let active: HashSet<EntryId> = j
            .root_to_leaf()
            .iter()
            .map(|e| e.id.clone())
            .take(1)
            .collect();
        assert!(j.remove_entries(&active).is_err());
        assert_eq!(j, before);
    }
}
