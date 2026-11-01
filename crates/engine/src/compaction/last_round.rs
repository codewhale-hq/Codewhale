//! Recent-history coverage floor for compaction replacement history.
//!
//! Compaction summarizes older turns while retaining a token-budgeted tail
//! and the current user instructions. Tool exchanges stay paired and retained
//! output is bounded. See [`SURVIVAL_CONTRACT.md`].

use anyhow::Result;
use std::collections::HashSet;

use codewhale_models::{ContentBlock, Message, SystemPrompt};

use super::{
    compaction_checkpoint_message, history_is_post_provenance, is_compaction_checkpoint_message,
    is_compaction_checkpoint_message_in_era, retained_user_messages_in_era,
    truncate_retained_block, user_text_of,
};

const LAST_ROUND_TOOL_RESULT_MAX_CHARS: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactionPath {
    #[default]
    Summary,
    PruneOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionCoverage {
    pub path: CompactionPath,
    pub last_round_messages: usize,
    pub last_round_tool_results: usize,
    pub last_round_assistant: bool,
    pub dropped_messages: usize,
    pub anchors_chars: usize,
    /// Effective `[compaction] retained_user_message_tokens` budget this pass
    /// spent on verbatim user messages (#5956). `0` on the prune-only path,
    /// which never builds a replacement history.
    pub retained_user_message_tokens: usize,
    /// Whether `[compaction] summary_instructions` was appended to the
    /// summarizer prompt on this pass (#5956).
    pub operator_instructions_applied: bool,
}

impl CompactionCoverage {
    #[must_use]
    pub fn receipt_clause(&self) -> String {
        let path = match self.path {
            CompactionPath::Summary => "summary",
            CompactionPath::PruneOnly => "prune-only",
        };
        let assistant = if self.last_round_assistant {
            ", assistant"
        } else {
            ""
        };
        let mut clause = format!(
            "{path}; last round kept: {} messages ({} tool results{assistant})",
            self.last_round_messages, self.last_round_tool_results
        );
        if self.anchors_chars > 0 {
            clause.push_str(&format!("; anchors {} chars", self.anchors_chars));
        }
        // Name the tuning knobs so an operator who set them can tell they took
        // effect without reading the log (#5956). The prune-only path builds no
        // replacement history, so it reports no budget.
        if self.retained_user_message_tokens > 0 {
            clause.push_str(&format!(
                "; verbatim user budget {} tokens",
                self.retained_user_message_tokens
            ));
        }
        if self.operator_instructions_applied {
            clause.push_str("; operator instructions applied");
        }
        clause
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCompactionSnapshot {
    pub auto: bool,
    pub coverage: CompactionCoverage,
    pub messages_before: usize,
    pub messages_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionKeep {
    pub has_checkpoint: bool,
    pub last_round_messages: usize,
    pub last_round_tool_results: usize,
    pub last_round_assistant: bool,
}

#[must_use]
pub fn inspect_compaction_keep(messages: &[Message]) -> CompactionKeep {
    let last_round = last_round_slice(messages);
    CompactionKeep {
        // Existence check: a stamped carrier also matches the loose form, so
        // "any checkpoint" reads the same in both eras and needs no era here.
        has_checkpoint: messages.iter().any(is_compaction_checkpoint_message),
        last_round_messages: last_round.len(),
        last_round_tool_results: last_round.iter().flat_map(tool_result_ids).count(),
        last_round_assistant: last_round
            .iter()
            .any(|message| message.role.is_assistant_like()),
    }
}

/// Workspace anchors are passive model input; only user-configured trust admits
/// them. Missing, linked, unreadable, or empty anchors contribute no text.
#[must_use]
pub fn pinned_anchors_text(workspace: Option<&std::path::Path>) -> Option<String> {
    let workspace = workspace?;
    if !codewhale_runtime::config::is_workspace_trusted(workspace) {
        return None;
    }
    let primary = workspace.join(".codewhale").join("anchors.md");
    let path = if primary.symlink_metadata().is_ok() || workspace.join(".codewhale").is_symlink() {
        primary
    } else {
        workspace.join(".deepseek").join("anchors.md")
    };
    codewhale_util::fs_confined::read_to_string(workspace, &path)
        .ok()
        .map(|contents| contents.trim().to_string())
        .filter(|contents| !contents.is_empty())
}

fn is_plain_user_text(post_provenance: bool, message: &Message) -> bool {
    !is_compaction_checkpoint_message_in_era(message, post_provenance)
        && !codewhale_models::turn_classify::is_runtime_owned_user_message(message)
        && user_text_of(message).is_some()
}

fn user_prompt_text_of(post_provenance: bool, message: &Message) -> Option<String> {
    is_plain_user_text(post_provenance, message)
        .then(|| user_text_of(message))
        .flatten()
}

fn last_plain_user_index(messages: &[Message], end: usize, post_provenance: bool) -> Option<usize> {
    messages[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, message)| is_plain_user_text(post_provenance, message).then_some(idx))
}

fn slice_has_tool_result(messages: &[Message], start: usize) -> bool {
    messages[start..].iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    })
}

#[must_use]
pub fn last_round_start(messages: &[Message]) -> usize {
    last_round_start_in_era(messages, history_is_post_provenance(messages))
}

/// Era-aware core of [`last_round_start`]. The era belongs to the history the
/// slice is cut from: `last_round_kept_count` reads a prefix of a longer
/// history whose carrier may sit after the cut, so it passes the full
/// history's era down instead of letting this re-derive one from the prefix.
fn last_round_start_in_era(messages: &[Message], post_provenance: bool) -> usize {
    let Some(last_user) = last_plain_user_index(messages, messages.len(), post_provenance) else {
        return 0;
    };
    if slice_has_tool_result(messages, last_user) {
        return last_user;
    }
    // Trailing toolless user/assistant turns still need the previous
    // tool-bearing round; otherwise those results vanish behind the summary.
    // If no tool round exists, keep only the latest user turn so chat-only
    // sessions can still summarize older text.
    let mut candidate = last_user;
    loop {
        let Some(prev) = last_plain_user_index(messages, candidate, post_provenance) else {
            return last_user;
        };
        if slice_has_tool_result(messages, prev) {
            return prev;
        }
        candidate = prev;
    }
}

#[must_use]
pub fn last_round_range(messages: &[Message]) -> (usize, usize) {
    let start = last_round_start(messages).min(messages.len());
    // A previous checkpoint can sit in the middle of an uninterrupted task.
    // Stopping at that marker hid every tool step after the first compact
    // from the next pass's survival checks.
    let post_provenance = history_is_post_provenance(messages);
    let end =
        messages
            .len()
            .saturating_sub(usize::from(messages.last().is_some_and(|message| {
                is_compaction_checkpoint_message_in_era(message, post_provenance)
            })));
    (start, end)
}

/// How many messages of the open round sit in `messages` before a checkpoint.
#[must_use]
pub fn last_round_kept_count(messages: &[Message]) -> Option<usize> {
    let post_provenance = history_is_post_provenance(messages);
    let checkpoint = messages
        .iter()
        .rposition(|message| is_compaction_checkpoint_message_in_era(message, post_provenance))?;
    if checkpoint == 0 {
        return None;
    }
    // The prefix belongs to the full history: recognize its turns with the
    // full history's era, not one re-derived from the prefix (whose carrier
    // may sit after the cut).
    let start = last_round_start_in_era(&messages[..checkpoint], post_provenance);
    Some(checkpoint.saturating_sub(start))
}

fn last_round_slice(messages: &[Message]) -> &[Message] {
    let (start, end) = last_round_range(messages);
    &messages[start..end]
}

pub(super) fn recent_tail_start(messages: &[Message], recent_tokens: usize) -> usize {
    let post_provenance = history_is_post_provenance(messages);
    let mut tokens = 0usize;
    let mut start = messages.len();
    for (idx, message) in messages.iter().enumerate().rev() {
        if is_compaction_checkpoint_message_in_era(message, post_provenance) {
            continue;
        }
        tokens = tokens.saturating_add(
            super::estimate_tokens(std::slice::from_ref(message)).saturating_add(12),
        );
        start = idx;
        if tokens >= recent_tokens {
            break;
        }
    }
    let mut pending = HashSet::new();
    let mut boundaries = vec![0];
    for (idx, message) in messages.iter().enumerate() {
        if pending.is_empty() {
            boundaries.push(idx);
        }
        pending.extend(tool_use_ids(message));
        for id in tool_result_ids(message) {
            pending.remove(&id);
        }
    }
    boundaries
        .into_iter()
        .rfind(|&idx| idx <= start)
        .unwrap_or(0)
}

fn protected_messages(messages: &[Message], recent_tokens: usize) -> Vec<&Message> {
    let post_provenance = history_is_post_provenance(messages);
    let start = recent_tail_start(messages, recent_tokens);
    let round_start = last_round_start_in_era(messages, post_provenance);
    messages
        .iter()
        .enumerate()
        .filter_map(|(idx, message)| {
            (!is_compaction_checkpoint_message_in_era(message, post_provenance)
                && (idx >= start
                    || (idx >= round_start && is_plain_user_text(post_provenance, message))))
            .then_some(message)
        })
        .collect()
}

pub(super) fn replacement_messages(
    messages: &[Message],
    retained_user_message_tokens: usize,
    recent_tokens: usize,
) -> Vec<Message> {
    let post_provenance = history_is_post_provenance(messages);
    let (start, _) = last_round_range(messages);
    let start = start.min(recent_tail_start(messages, recent_tokens));
    let mut retained = retained_user_messages_in_era(
        post_provenance,
        &messages[..start],
        retained_user_message_tokens,
    );
    let round = protected_messages(messages, recent_tokens)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    retained.extend(bound_last_round(&round));
    // The Operate contract applies to the current tool loop as well as later
    // turns. It must survive compaction even when old-user retention is full.
    let current_contract = messages
        .iter()
        .rev()
        .find(|message| crate::runtime_handoff::is_current_operate_contract_message(message));
    let contract = current_contract.or_else(|| {
        messages
            .iter()
            .rev()
            .find(|message| crate::runtime_handoff::is_operate_contract_message(message))
    });
    if let Some(contract) = contract {
        retained.retain(|message| !crate::runtime_handoff::is_operate_contract_message(message));
        retained.insert(0, contract.clone());
    }
    // Contributions are captured once per turn. Preserve the complete latest
    // snapshot, including a withdrawal, rather than summarizing or truncating
    // instructions that still apply to this tool loop.
    if let Some(snapshot) = messages.iter().rev().find(|message| {
        crate::runtime_handoff::extension_prompt_contributions_display(message).is_some()
    }) {
        retained.retain(|message| {
            crate::runtime_handoff::extension_prompt_contributions_display(message).is_none()
        });
        retained.insert(0, snapshot.clone());
    }
    if let Some(snapshot) = messages
        .iter()
        .rev()
        .find(|message| crate::runtime_handoff::constitution_display(message).is_some())
    {
        retained.retain(|message| crate::runtime_handoff::constitution_display(message).is_none());
        retained.insert(0, snapshot.clone());
    }
    retained
}

pub(super) fn bound_last_round(messages: &[Message]) -> Vec<Message> {
    let mut round = messages.to_vec();
    for message in &mut round {
        for block in &mut message.content {
            if let ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } = block
                && truncate_retained_block("tool result", content, LAST_ROUND_TOOL_RESULT_MAX_CHARS)
            {
                *content_blocks = None;
            }
        }
    }
    round
}

fn tool_result_ids(message: &Message) -> Vec<(codewhale_models::ToolCallKey<'_>, &str)> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => {
                block.tool_call_key().map(|key| (key, tool_use_id.as_str()))
            }
            _ => None,
        })
        .collect()
}

fn has_tool_result_id(message: &Message, id: &(codewhale_models::ToolCallKey<'_>, &str)) -> bool {
    if id.0.as_str().trim().is_empty() {
        return false;
    }
    message.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolResult { tool_use_id, .. } if block.tool_call_key() == Some(id.0) && tool_use_id == id.1
        )
    })
}

fn tool_use_ids(message: &Message) -> Vec<(codewhale_models::ToolCallKey<'_>, &str)> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => block.tool_call_key().map(|key| (key, id.as_str())),
            _ => None,
        })
        .collect()
}

fn has_tool_use_id(message: &Message, id: &(codewhale_models::ToolCallKey<'_>, &str)) -> bool {
    if id.0.as_str().trim().is_empty() {
        return false;
    }
    message.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolUse { id: provider, .. } if block.tool_call_key() == Some(id.0) && provider == id.1
        )
    })
}

fn tool_call_identity_label(id: &(codewhale_models::ToolCallKey<'_>, &str)) -> String {
    match id.0 {
        codewhale_models::ToolCallKey::Execution(execution_id) => {
            format!("execution {execution_id} (provider call {})", id.1)
        }
        codewhale_models::ToolCallKey::LegacyProvider(provider_id) => provider_id.to_string(),
    }
}

fn assistant_text_of(message: &Message) -> Option<String> {
    if !message.role.is_assistant_like() {
        return None;
    }
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A retained copy may be truncated (`bound_last_round` caps oversized blocks),
/// so a prefix either way counts as survival -- but nothing weaker does.
fn survives(text: &str, replacement: &[Message], of: impl Fn(&Message) -> Option<String>) -> bool {
    replacement.iter().any(|message| {
        of(message)
            .is_some_and(|kept| kept == text || text.starts_with(&kept) || kept.starts_with(text))
    })
}

pub fn validate_retained_coverage(
    original: &[Message],
    replacement: &[Message],
    recent_tokens: usize,
) -> Result<()> {
    let post_provenance = history_is_post_provenance(original);
    let last_round = protected_messages(original, recent_tokens);
    if last_round.is_empty() {
        return Ok(());
    }
    // Every user turn in the round, not the first one `find_map` happens to
    // reach. `last_round_start` walks back past a toolless tail to the previous
    // tool-bearing turn, so the round routinely spans two user messages -- and
    // checking only the earliest let a rewrite drop the *latest* one, which is
    // the turn this whole contract exists to keep.
    for text in last_round
        .iter()
        .copied()
        .filter_map(|message| user_prompt_text_of(post_provenance, message))
    {
        if !survives(&text, replacement, |message| {
            user_prompt_text_of(post_provenance, message)
        }) {
            anyhow::bail!(
                "Making room stopped: a last-round user message was dropped; history was not replaced."
            );
        }
    }
    for id in last_round.iter().copied().flat_map(tool_result_ids) {
        if !replacement
            .iter()
            .any(|message| has_tool_result_id(message, &id))
        {
            let label = tool_call_identity_label(&id);
            anyhow::bail!(
                "Making room stopped: last-round tool result {label} was dropped; history was not replaced."
            );
        }
    }
    // The call, not just its result. Keeping a tool_result whose tool_use was
    // summarized away leaves an orphaned result that providers reject outright.
    for id in last_round.iter().copied().flat_map(tool_use_ids) {
        if !replacement
            .iter()
            .any(|message| has_tool_use_id(message, &id))
        {
            let label = tool_call_identity_label(&id);
            anyhow::bail!(
                "Making room stopped: last-round tool call {label} was dropped; history was not replaced."
            );
        }
    }
    // Match the assistant's actual output. An existential "some assistant
    // message survived" check passed on a replacement whose only assistant
    // message was the summary the rewrite had just written.
    for text in last_round.iter().copied().filter_map(assistant_text_of) {
        if !survives(&text, replacement, assistant_text_of) {
            anyhow::bail!(
                "Making room stopped: last-round assistant output was dropped; history was not replaced."
            );
        }
    }
    if last_round
        .iter()
        .any(|message| message.role.is_assistant_like())
        && !replacement
            .iter()
            .any(|message| message.role.is_assistant_like())
    {
        anyhow::bail!(
            "Making room stopped: last-round assistant output was dropped; history was not replaced."
        );
    }
    Ok(())
}

pub fn require_text_survives(replacement: &[Message], needle: &str, label: &str) -> Result<()> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(());
    }
    let kept = replacement.iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::Text { text, .. } => text.contains(needle),
            ContentBlock::ToolResult { content, .. } => content.contains(needle),
            _ => false,
        })
    });
    if !kept {
        anyhow::bail!("Making room stopped: {label} was dropped; history was not replaced.");
    }
    Ok(())
}

pub fn validate_survival_contract(
    original: &[Message],
    replacement: &[Message],
    anchors: Option<&str>,
    recent_tokens: usize,
) -> Result<()> {
    validate_retained_coverage(original, replacement, recent_tokens)?;
    // The receipt count reads the replacement in its own era: a replacement
    // carrying the engine-stamped receipt is modern, so retained turns that
    // paste a bare header do not count as duplicated summaries, while a
    // legacy-shaped replacement keeps the loose count that the fixture
    // matrix pins. A post-provenance *original* raises the bar instead: the
    // engine stamps every replacement it builds, and the stamped carrier is
    // the only form request rewriting relocates, so a replacement of a
    // modern history without a stamp is a dropped receipt even where a
    // retained pasted turn would loosely count as one.
    let receipt_recognition = if history_is_post_provenance(original) {
        true
    } else {
        history_is_post_provenance(replacement)
    };
    let checkpoints = replacement
        .iter()
        .filter(|message| is_compaction_checkpoint_message_in_era(message, receipt_recognition))
        .count();
    if checkpoints == 0 {
        anyhow::bail!(
            "Making room stopped: checkpoint receipt was dropped; history was not replaced."
        );
    }
    if checkpoints > 1 {
        anyhow::bail!(
            "Making room stopped: prior summaries were duplicated; history was not replaced."
        );
    }
    if let Some(anchors) = anchors {
        require_text_survives(replacement, anchors, "pinned /anchor text")?;
    }
    Ok(())
}

pub(super) fn measure_coverage(
    original: &[Message],
    replacement: &[Message],
    path: CompactionPath,
    anchors_chars: usize,
) -> CompactionCoverage {
    let last_round = last_round_slice(replacement);
    CompactionCoverage {
        path,
        last_round_messages: last_round.len(),
        last_round_tool_results: last_round.iter().flat_map(tool_result_ids).count(),
        last_round_assistant: last_round
            .iter()
            .any(|message| message.role.is_assistant_like()),
        dropped_messages: original.len().saturating_sub(replacement.len()),
        anchors_chars,
        // Tuning provenance is owned by the caller that holds the
        // `CompactionConfig`; measurement over two message lists cannot know it.
        retained_user_message_tokens: 0,
        operator_instructions_applied: false,
    }
}

/// Build the post-compaction history: recent plain user messages kept
/// verbatim within `retained_user_message_tokens`, the bounded recent tail, and
/// the checkpoint. The budget is `[compaction] retained_user_message_tokens`
/// (#5956); it was a hard-coded 20 000 before that key existed.
pub(super) fn build_replacement_history(
    messages: &[Message],
    checkpoint_text: &str,
    anchors: Option<&str>,
    retained_user_message_tokens: usize,
    recent_tokens: usize,
) -> Result<Vec<Message>> {
    let mut retained = replacement_messages(messages, retained_user_message_tokens, recent_tokens);
    retained.push(compaction_checkpoint_message(&SystemPrompt::Text(
        checkpoint_text.to_string(),
    )));
    validate_survival_contract(messages, &retained, anchors, recent_tokens)?;
    Ok(retained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::compaction_checkpoint_message;
    use codewhale_models::turn_classify::{
        COMPACTION_SUMMARY_MARKER, LEGACY_V2_COMPACTION_SUMMARY_MARKER,
    };
    use codewhale_models::{ContentBlock, Role};
    use serde_json::json;

    #[test]
    fn coverage_requires_the_original_execution_and_provider_pair() {
        let original: Vec<Message> = serde_json::from_value(json!([
            {"role":"user","content":[{"type":"text","text":"keep this exchange"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":"local","name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire","execution_id":"local","content":"kept"}]}
        ])).unwrap();
        assert!(validate_retained_coverage(&original, &original, 20_000).is_ok());
        for identity in [None, Some("different")] {
            let mut replacement = original.clone();
            for block in replacement
                .iter_mut()
                .flat_map(|message| &mut message.content)
            {
                match block {
                    ContentBlock::ToolUse { execution_id, .. }
                    | ContentBlock::ToolResult { execution_id, .. } => {
                        *execution_id = identity.map(str::to_string)
                    }
                    _ => {}
                }
            }
            assert!(validate_retained_coverage(&original, &replacement, 20_000).is_err());
        }
        let mut replacement = original.clone();
        if let ContentBlock::ToolResult { tool_use_id, .. } = &mut replacement[2].content[0] {
            *tool_use_id = "wrong-wire".to_string();
        }
        let error = validate_retained_coverage(&original, &replacement, 20_000).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Making room stopped: last-round tool result execution local (provider call wire) was dropped; history was not replaced."
        );
        let mut replacement = original.clone();
        if let ContentBlock::ToolUse { id, .. } = &mut replacement[1].content[0] {
            *id = "wrong-wire".to_string();
        }
        let error = validate_retained_coverage(&original, &replacement, 20_000).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Making room stopped: last-round tool call execution local (provider call wire) was dropped; history was not replaced."
        );
    }

    #[test]
    fn confined_pinned_anchors_require_workspace_trust() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _lock = lock_test_env();
        let workspace = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config.path().join("config.toml"));
        let legacy = workspace.path().join(".deepseek");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("anchors.md"), " legacy anchor \n").unwrap();
        std::fs::write(legacy.join("trusted"), "true").unwrap();
        assert_eq!(pinned_anchors_text(None), None);
        assert_eq!(pinned_anchors_text(Some(workspace.path())), None);

        codewhale_runtime::config::save_workspace_trust(workspace.path()).unwrap();
        assert!(codewhale_runtime::config::is_workspace_trusted(
            workspace.path()
        ));
        assert_eq!(
            pinned_anchors_text(Some(workspace.path())).as_deref(),
            Some("legacy anchor")
        );
        let primary = workspace.path().join(".codewhale");
        std::fs::create_dir(&primary).unwrap();
        std::fs::write(primary.join("anchors.md"), " primary anchor \n").unwrap();
        assert_eq!(
            pinned_anchors_text(Some(workspace.path())).as_deref(),
            Some("primary anchor")
        );
        std::fs::write(primary.join("anchors.md"), " \n").unwrap();
        assert_eq!(pinned_anchors_text(Some(workspace.path())), None);
    }

    #[cfg(unix)]
    #[test]
    fn confined_pinned_anchors_refuse_linked_files_and_directories() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        use std::os::unix::fs::symlink;
        let _lock = lock_test_env();
        let config = tempfile::tempdir().unwrap();
        let _config = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config.path().join("config.toml"));
        for directory in [".codewhale", ".deepseek"] {
            for linked_directory in [false, true] {
                let workspace = tempfile::tempdir().unwrap();
                let outside = tempfile::tempdir().unwrap();
                codewhale_runtime::config::save_workspace_trust(workspace.path()).unwrap();
                assert!(codewhale_runtime::config::is_workspace_trusted(
                    workspace.path()
                ));
                let target = outside.path().join("anchors.md");
                std::fs::write(&target, "separate anchor").unwrap();
                let parent = workspace.path().join(directory);
                if linked_directory {
                    symlink(outside.path(), &parent).unwrap();
                } else {
                    std::fs::create_dir(&parent).unwrap();
                    symlink(&target, parent.join("anchors.md")).unwrap();
                }
                assert_eq!(pinned_anchors_text(Some(workspace.path())), None);
                // A dangling preferred path must not select legacy content.
                if directory == ".codewhale" {
                    let legacy = workspace.path().join(".deepseek");
                    std::fs::create_dir(&legacy).unwrap();
                    std::fs::write(legacy.join("anchors.md"), "legacy anchor").unwrap();
                }
                std::fs::remove_file(&target).unwrap();
                assert_eq!(pinned_anchors_text(Some(workspace.path())), None);
            }
        }
    }

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
                thought_signature: None,
            }],
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
                content_blocks: None,
            }],
        }
    }

    fn checkpoint(summary: &str) -> Message {
        compaction_checkpoint_message(&SystemPrompt::Text(format!(
            "{COMPACTION_SUMMARY_MARKER}: {summary}"
        )))
    }

    #[test]
    fn replacement_keeps_only_latest_complete_prompt_snapshot_and_withdrawal() {
        use crate::runtime_handoff::{
            extension_prompt_contributions_display, extension_prompt_contributions_runtime_message,
        };
        let old = extension_prompt_contributions_runtime_message(Some("old instructions"));
        let current_text = "current instructions ".repeat(400);
        for current in [
            extension_prompt_contributions_runtime_message(Some(&current_text)),
            extension_prompt_contributions_runtime_message(None),
        ] {
            let quoted = msg(
                "user",
                &user_text_of(&current).expect("runtime snapshot has text"),
            );
            let original = vec![
                old.clone(),
                quoted.clone(),
                current.clone(),
                msg("user", "Continue this task."),
                tool_use("first", "Read", json!({"path": "first"})),
                tool_result("first", "first output"),
                tool_use("second", "Read", json!({"path": "second"})),
                tool_result("second", "second output"),
                tool_use("third", "Read", json!({"path": "third"})),
                tool_result("third", "third output"),
            ];
            let kept = replacement_messages(&original, 20_000, 20_000);
            let snapshots: Vec<_> = kept
                .iter()
                .filter(|message| extension_prompt_contributions_display(message).is_some())
                .collect();
            assert_eq!(snapshots, [&current]);
            assert!(
                kept.contains(&quoted),
                "a person's quote is ordinary user text"
            );
            assert_eq!(
                replacement_messages(&kept, 20_000, 20_000)
                    .iter()
                    .filter(|message| extension_prompt_contributions_display(message).is_some())
                    .count(),
                1,
                "repeated compaction must not accumulate snapshots"
            );
        }
    }

    /// The handoff header tells the next turn what survived. It must match
    /// what the replacement history keeps: only the last steps of a long
    /// round, with long tool output shortened and marked.
    #[test]
    fn profile_constitution_compaction_keeps_only_the_complete_latest_snapshot() {
        use crate::runtime_handoff::{constitution_display, constitution_runtime_message};
        let old = constitution_runtime_message(Some("old instructions"));
        let current_text = "current instructions ".repeat(400);
        for current in [
            constitution_runtime_message(Some(&current_text)),
            constitution_runtime_message(None),
        ] {
            let quoted = msg(
                "user",
                &user_text_of(&current).expect("runtime snapshot has text"),
            );
            let original = vec![
                old.clone(),
                quoted.clone(),
                current.clone(),
                msg("user", "Continue this task."),
                tool_use("first", "Read", json!({"path": "first"})),
                tool_result("first", "first output"),
                tool_use("second", "Read", json!({"path": "second"})),
                tool_result("second", "second output"),
                tool_use("third", "Read", json!({"path": "third"})),
                tool_result("third", "third output"),
            ];
            let kept = replacement_messages(&original, 20_000, 20_000);
            let snapshots: Vec<_> = kept
                .iter()
                .filter(|message| constitution_display(message).is_some())
                .collect();
            assert_eq!(snapshots, [&current]);
            assert!(
                kept.contains(&quoted),
                "a person's quote is ordinary user text"
            );
            assert_eq!(
                replacement_messages(&kept, 20_000, 20_000)
                    .iter()
                    .filter(|message| constitution_display(message).is_some())
                    .count(),
                1,
                "repeated compaction must not accumulate snapshots"
            );
        }
    }

    /// The handoff header tells the next turn what survived. It must match
    /// what the replacement history keeps: only the last steps of a long
    /// round, with long tool output shortened and marked.
    #[test]
    fn summary_header_matches_what_replacement_history_keeps() {
        let long_output = "x".repeat(LAST_ROUND_TOOL_RESULT_MAX_CHARS * 2);
        let original = vec![
            msg("user", "Fix the build."),
            tool_use("first", "Bash", json!({"command": "cargo check"})),
            tool_result("first", "first step output"),
            tool_use("second", "Bash", json!({"command": "cargo build"})),
            tool_result("second", "second step output"),
            tool_use("third", "Bash", json!({"command": "cargo test"})),
            tool_result("third", &long_output),
        ];
        let recent_tokens =
            crate::compaction::estimate_tokens(&original[3..]) + original[3..].len() * 12;
        let kept = replacement_messages(&original, 20_000, recent_tokens);
        let kept_ids: Vec<&str> = kept
            .iter()
            .flat_map(tool_result_ids)
            .map(|(_, wire)| wire)
            .collect();
        assert_eq!(kept_ids, ["second", "third"], "earlier steps are dropped");
        assert!(
            kept.iter()
                .any(|m| user_text_of(m).as_deref() == Some("Fix the build."))
        );
        let shortened = kept
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } if tool_use_id == "third" => Some(content.clone()),
                _ => None,
            })
            .expect("the last tool result is kept");
        assert!(shortened.len() < long_output.len());
        assert!(shortened.starts_with("[tool result retained-history truncated from"));

        let header = crate::compaction::SUMMARY_HEADER;
        assert!(header.contains("current user instructions"), "{header}");
        assert!(header.contains("tail selected by token budget"), "{header}");
        assert!(
            header.contains("Long tool output there is shortened"),
            "{header}"
        );
        assert!(
            header.contains("with a marker where it was cut"),
            "{header}"
        );
        assert!(!header.contains("as they were"), "{header}");
    }

    #[test]
    fn coverage_floor_rejects_a_replacement_that_drops_last_round_tools() {
        let original = vec![
            msg("user", "Run the failing test."),
            msg("assistant", "Running."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "test session_store::roundtrip ... FAILED"),
        ];
        let gutting = vec![
            msg("user", "Run the failing test."),
            checkpoint("and kept going"),
        ];
        let error = validate_retained_coverage(&original, &gutting, 20_000)
            .expect_err("dropping the last tool result must fail the coverage floor");
        assert!(error.to_string().contains("tool result live"), "{error}");
        assert!(validate_retained_coverage(&original, &original, 20_000).is_ok());
    }

    #[test]
    fn coverage_floor_rejects_a_replacement_that_drops_last_round_assistant() {
        let original = vec![
            msg("user", "What failed?"),
            msg("assistant", "session_store::roundtrip panics on reload."),
        ];
        let error = validate_retained_coverage(&original, &[msg("user", "What failed?")], 20_000)
            .expect_err("dropping last-round assistant text must fail closed");
        assert!(error.to_string().contains("assistant"), "{error}");
    }

    /// The round spans the tool-bearing turn *and* the toolless tail after it,
    /// because `last_round_start` walks back for the tools. Checking only the
    /// first user text it found meant a rewrite could keep the older question
    /// and drop the one the person actually just asked.
    #[test]
    fn coverage_floor_rejects_a_replacement_that_drops_the_latest_user_turn() {
        let original = vec![
            msg("user", "Run the suite."),
            msg("assistant", "Running."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
            msg("user", "Now ship it."),
            msg("assistant", "Shipping."),
        ];
        assert_eq!(last_round_start(&original), 0, "round must span both turns");

        let drops_latest = vec![
            msg("user", "Run the suite."),
            msg("assistant", "Running."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
            checkpoint("then shipped"),
        ];
        let error = validate_retained_coverage(&original, &drops_latest, 20_000)
            .expect_err("dropping the latest user turn must fail the coverage floor");
        assert!(error.to_string().contains("user message"), "{error}");
        assert!(validate_retained_coverage(&original, &original, 20_000).is_ok());
    }

    /// A surviving `tool_result` whose `tool_use` was summarized away is an
    /// orphan the provider rejects, so the floor must cover the call too.
    #[test]
    fn coverage_floor_rejects_a_replacement_that_drops_the_tool_call() {
        let original = vec![
            msg("user", "Run the failing test."),
            msg("assistant", "Running."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "FAILED"),
        ];
        let orphaned = vec![
            msg("user", "Run the failing test."),
            msg("assistant", "Running."),
            tool_result("live", "FAILED"),
            checkpoint("and it failed"),
        ];
        let error = validate_retained_coverage(&original, &orphaned, 20_000)
            .expect_err("dropping the tool call must fail the coverage floor");
        assert!(error.to_string().contains("tool call live"), "{error}");
    }

    /// "Some assistant message survived" was satisfied by the summary the
    /// rewrite had just written, so the round's real output could vanish.
    #[test]
    fn coverage_floor_rejects_assistant_output_replaced_by_a_summary() {
        let original = vec![
            msg("user", "What failed?"),
            msg("assistant", "session_store::roundtrip panics on reload."),
        ];
        let summarized = vec![
            msg("user", "What failed?"),
            msg("assistant", "Earlier we discussed several test failures."),
        ];
        let error = validate_retained_coverage(&original, &summarized, 20_000)
            .expect_err("substituting a summary for the round's output must fail closed");
        assert!(error.to_string().contains("assistant"), "{error}");
    }

    #[test]
    fn survival_contract_rejects_dropped_anchors_and_receipts() {
        let original = vec![msg("user", "Keep the pin."), msg("assistant", "Anchored.")];
        let without_receipt = vec![msg("user", "Keep the pin."), msg("assistant", "Anchored.")];
        let error =
            validate_survival_contract(&original, &without_receipt, Some("ship 0.9.12"), 20_000)
                .expect_err("missing checkpoint receipt must fail closed");
        assert!(error.to_string().contains("receipt"), "{error}");

        let without_anchor = vec![
            msg("user", "Keep the pin."),
            msg("assistant", "Anchored."),
            checkpoint("progress without the pin"),
        ];
        let error =
            validate_survival_contract(&original, &without_anchor, Some("ship 0.9.12"), 20_000)
                .expect_err("dropped /anchor text must fail closed");
        assert!(error.to_string().contains("anchor"), "{error}");
    }

    #[test]
    fn last_round_starts_at_the_latest_plain_user_message() {
        let messages = vec![
            msg("user", "older"),
            msg("assistant", "working"),
            tool_result("old", "stale"),
            msg("user", "Run the suite now."),
            msg("assistant", "Rerunning."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
        ];
        assert_eq!(last_round_start(&messages), 3); // last user with tools
        let (start, end) = last_round_range(&messages);
        let kept = bound_last_round(&messages[start..end]);
        assert!(kept.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "live" && content == "ok"
                )
            })
        }));
    }

    #[test]
    fn second_compaction_keeps_long_user_question_and_tool_pair_past_retention_budget() {
        const PRODUCTION_MIN_RETAINED_TOKENS: usize = 2_000;
        let long_question = format!(
            "{}?",
            "Analyze every step of this case carefully. ".repeat(400)
        );
        assert!(long_question.len() > PRODUCTION_MIN_RETAINED_TOKENS * 3);
        let original = vec![
            msg("user", &long_question),
            tool_use("call_1", "Bash", json!({"command": "echo ready"})),
            tool_result("call_1", "ready"),
        ];
        let first_summary =
            crate::compaction::build_compaction_summary_block_text("First pass complete", "");
        let mut first = build_replacement_history(
            &original,
            &first_summary,
            None,
            PRODUCTION_MIN_RETAINED_TOKENS,
            20_000,
        )
        .expect("first compaction");
        crate::runtime_handoff::replace_agent_topology_checkpoint(&mut first, &[]);
        assert_eq!(last_round_start(&first), 0);
        let topology = first
            .iter()
            .find(|message| crate::runtime_handoff::is_agent_topology_checkpoint(message))
            .expect("first compaction topology checkpoint")
            .clone();
        assert!(
            crate::compaction::retained_user_messages(
                std::slice::from_ref(&topology),
                PRODUCTION_MIN_RETAINED_TOKENS,
            )
            .is_empty(),
            "runtime topology must not consume the older-user retention budget"
        );

        let second_summary =
            crate::compaction::build_compaction_summary_block_text("Second pass complete", "");
        let second = build_replacement_history(
            &first,
            &second_summary,
            None,
            PRODUCTION_MIN_RETAINED_TOKENS,
            20_000,
        )
        .expect("second compaction");
        assert!(
            second.iter().any(|message| {
                user_text_of(message).as_deref() == Some(long_question.as_str())
            })
        );
        assert!(second.iter().any(|message| has_tool_use_id(
            message,
            &(
                codewhale_models::ToolCallKey::LegacyProvider("call_1"),
                "call_1"
            )
        )));
        assert!(second.iter().any(|message| has_tool_result_id(
            message,
            &(
                codewhale_models::ToolCallKey::LegacyProvider("call_1"),
                "call_1"
            )
        )));
        let without_question = second
            .iter()
            .filter(|message| user_text_of(message).as_deref() != Some(long_question.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        assert!(validate_retained_coverage(&first, &without_question, 20_000).is_err());
    }

    #[test]
    fn runtime_text_cannot_satisfy_real_user_coverage() {
        let runtime = crate::runtime_handoff::operate_contract_runtime_message();
        let copied_text = user_text_of(&runtime).expect("runtime text");
        let original = vec![msg("user", &copied_text), msg("assistant", "Acknowledged")];
        let replacement = vec![runtime, msg("assistant", "Acknowledged")];
        assert!(
            validate_retained_coverage(&original, &replacement, 20_000).is_err(),
            "runtime-owned text must not stand in for the user's actual prompt"
        );
    }

    #[test]
    fn operate_contract_survives_compaction_without_spending_user_budget() {
        let contract = crate::runtime_handoff::operate_contract_runtime_message();
        let original = vec![
            contract.clone(),
            msg("user", "First task"),
            msg("assistant", "Working"),
            msg("user", "Continue the same task"),
            msg("assistant", "Continuing"),
        ];
        let replaced = build_replacement_history(
            &original,
            &format!("{COMPACTION_SUMMARY_MARKER}: work continues"),
            None,
            1,
            20_000,
        )
        .expect("compaction must retain the active Operate contract");
        assert_eq!(replaced.first(), Some(&contract));
        assert_eq!(
            replaced
                .iter()
                .filter(|message| **message == contract)
                .count(),
            1
        );
    }

    #[test]
    fn compaction_prefers_current_operate_contract_over_legacy() {
        let legacy = crate::runtime_handoff::legacy_operate_contract_runtime_message();
        let current = crate::runtime_handoff::operate_contract_runtime_message();
        let original = vec![
            legacy.clone(),
            current.clone(),
            msg("user", "Continue"),
            msg("assistant", "Working"),
        ];
        let replaced = build_replacement_history(
            &original,
            &format!("{COMPACTION_SUMMARY_MARKER}: work continues"),
            None,
            1,
            20_000,
        )
        .expect("current contract must survive compaction");
        assert_eq!(replaced.first(), Some(&current));
        assert!(!replaced.contains(&legacy));
        assert_eq!(
            replaced
                .iter()
                .filter(|message| crate::runtime_handoff::is_operate_contract_message(message))
                .count(),
            1
        );
    }

    #[test]
    fn compaction_replaces_v0_10_0_operate_contract_with_current() {
        // A v0.10.0 session that entered Operate after a tool round, resumed on
        // a later build: the protected round reaches back past both contracts.
        let shipped = crate::runtime_handoff::v0_10_0_operate_contract_runtime_message();
        let current = crate::runtime_handoff::operate_contract_runtime_message();
        let original = vec![
            msg("user", "Run the failing test."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "test session_store::roundtrip ... FAILED"),
            shipped.clone(),
            msg("user", "Fix it."),
            msg("assistant", "Fixed."),
            current.clone(),
            msg("user", "Continue"),
            msg("assistant", "Working"),
        ];
        assert_eq!(last_round_start(&original), 0);
        let replaced = build_replacement_history(
            &original,
            &format!("{COMPACTION_SUMMARY_MARKER}: work continues"),
            None,
            1,
            20_000,
        )
        .expect("current contract must survive compaction");
        assert_eq!(replaced.first(), Some(&current));
        assert!(
            !replaced.contains(&shipped),
            "the v0.10.0 contract contradicts the current one about goals"
        );
        assert_eq!(
            replaced
                .iter()
                .filter(|message| crate::runtime_handoff::is_operate_contract_message(message))
                .count(),
            1
        );

        // Until an Operate turn on the new build appends the current wording,
        // the saved contract is the only one and is kept once, at the front.
        let only_shipped = vec![
            shipped.clone(),
            msg("user", "Continue"),
            msg("assistant", "Working"),
        ];
        let replaced = build_replacement_history(
            &only_shipped,
            &format!("{COMPACTION_SUMMARY_MARKER}: work continues"),
            None,
            1,
            20_000,
        )
        .expect("saved contract must survive compaction");
        assert_eq!(replaced.first(), Some(&shipped));
        assert_eq!(
            replaced
                .iter()
                .filter(|message| **message == shipped)
                .count(),
            1
        );
    }

    #[test]
    fn last_round_walks_back_through_toolless_tails_to_the_tool_round() {
        let original = vec![
            msg("user", "Run the failing test."),
            msg("assistant", "Running."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "test session_store::roundtrip ... FAILED"),
            msg("user", "ok thanks"),
            msg("assistant", "you're welcome"),
            msg("user", "one more thing"),
            msg("assistant", "sure"),
        ];
        assert_eq!(last_round_start(&original), 0);
        let next = format!("{COMPACTION_SUMMARY_MARKER}: keep the failing test result");
        let replaced = build_replacement_history(
            &original,
            &next,
            None,
            crate::compaction::COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS,
            20_000,
        )
        .expect("toolless tails must not drop the last tool result");
        assert!(replaced.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "live" && content.contains("FAILED")
                )
            })
        }));
    }

    #[test]
    fn chat_only_history_keeps_the_latest_user_round() {
        let messages = vec![
            msg("user", "hello"),
            msg("assistant", "hi"),
            msg("user", "how are you"),
            msg("assistant", "fine"),
        ];
        assert_eq!(last_round_start(&messages), 2);
    }

    /// In a history that carries the stamped carrier, a user turn pasting a
    /// bare legacy summary header is content: it anchors the round, survives
    /// the replacement, and the receipt count stays 1 instead of rejecting
    /// the rewrite as a duplicated summary.
    #[test]
    fn pasted_legacy_header_turn_is_content_in_a_post_provenance_history() {
        let pasted = format!(
            "{LEGACY_V2_COMPACTION_SUMMARY_MARKER}\n\nEarlier turns of another session, pasted."
        );
        let original = vec![
            msg("user", "Run the suite now."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
            checkpoint("suite finished"),
            msg("assistant", "done"),
            msg("user", &pasted),
        ];
        assert!(
            !history_is_post_provenance(&[msg("user", &pasted)]),
            "the pasted turn alone carries no provenance block"
        );
        assert_eq!(
            last_round_start(&original),
            0,
            "the pasted turn must anchor like any other user turn"
        );

        let retained =
            retained_user_messages_in_era(history_is_post_provenance(&original), &original, 10_000);
        assert!(
            retained
                .iter()
                .any(|message| user_text_of(message).as_deref() == Some(pasted.as_str())),
            "the pasted turn must be retained verbatim: {retained:?}"
        );

        let replacement = vec![
            checkpoint("second summary"),
            msg("user", "Run the suite now."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
            msg("assistant", "done"),
            msg("user", &pasted),
        ];
        validate_survival_contract(&original, &replacement, None, 20_000)
            .expect("a retained pasted-header turn must not break or lose the round");

        let mut dropped = replacement.clone();
        dropped.pop();
        let error = validate_survival_contract(&original, &dropped, None, 20_000)
            .expect_err("dropping the pasted turn must fail the coverage floor");
        assert!(
            error.to_string().contains("user message was dropped"),
            "{error}"
        );
    }

    /// A history without a stamped carrier predates stamping: the bare
    /// header there is indistinguishable from the saved carrier, so the
    /// loose recognition keeps applying.
    #[test]
    fn legacy_history_still_recognizes_a_bare_header_checkpoint() {
        let bare = msg(
            "user",
            &format!("{LEGACY_V2_COMPACTION_SUMMARY_MARKER}\n\nCondensed progress."),
        );
        assert!(!history_is_post_provenance(std::slice::from_ref(&bare)));
        assert!(
            retained_user_messages_in_era(
                history_is_post_provenance(std::slice::from_ref(&bare)),
                std::slice::from_ref(&bare),
                1_000,
            )
            .is_empty(),
            "a bare-header legacy checkpoint must still not be retained as user text"
        );
        let messages = vec![msg("user", "older"), bare, msg("assistant", "done")];
        assert_eq!(
            last_round_kept_count(&messages),
            Some(1),
            "the bare header still bounds the kept round in a legacy history"
        );
    }

    #[derive(serde::Deserialize)]
    struct FixtureMatrix {
        schema_version: u32,
        cases: Vec<FixtureCase>,
    }

    #[derive(serde::Deserialize)]
    struct FixtureCase {
        id: String,
        expect: String,
        #[serde(default)]
        anchors: Option<String>,
        original: Vec<Message>,
        replacement: Vec<Message>,
        #[serde(default)]
        last_round_start: Option<usize>,
        recent_tokens: usize,
    }

    #[test]
    fn fixture_matrix_enforces_survival_contract() {
        let matrix: FixtureMatrix =
            serde_json::from_str(include_str!("fixtures/matrix.json")).expect("matrix.json");
        assert_eq!(matrix.schema_version, 2);
        assert!(
            matrix.cases.len() >= 8,
            "fixture matrix must cover last-round, toolless-tail, chat-only, anchor, and receipt cases"
        );
        for case in &matrix.cases {
            if let Some(start) = case.last_round_start {
                assert_eq!(
                    last_round_start(&case.original),
                    start,
                    "{} last_round_start",
                    case.id
                );
            }
            let result = validate_survival_contract(
                &case.original,
                &case.replacement,
                case.anchors.as_deref(),
                case.recent_tokens,
            );
            match case.expect.as_str() {
                "pass" => {
                    result.unwrap_or_else(|error| panic!("{} should pass: {error}", case.id));
                }
                "fail" => {
                    result.expect_err(&format!("{} should fail closed", case.id));
                }
                other => panic!("{}: unknown expect {other}", case.id),
            }
        }
    }

    #[test]
    fn second_compact_does_not_duplicate_prior_summaries() {
        let first = vec![
            msg("user", "older"),
            msg("user", "Run the suite now."),
            msg("assistant", "Rerunning."),
            tool_use("live", "Bash", json!({"command": "cargo test"})),
            tool_result("live", "ok"),
            checkpoint("first handoff: suite still running"),
        ];
        let next = format!(
            "{COMPACTION_SUMMARY_MARKER}: second handoff with User-pinned anchors (verbatim):\nship 0.9.12"
        );
        let replaced = build_replacement_history(
            &first,
            &next,
            Some("ship 0.9.12"),
            crate::compaction::COMPACT_RETAINED_USER_MESSAGE_MAX_TOKENS,
            20_000,
        )
        .expect("second compact must keep last round and one receipt");
        let checkpoints = replaced
            .iter()
            .filter(|message| is_compaction_checkpoint_message(message))
            .count();
        assert_eq!(checkpoints, 1, "{replaced:?}");
        assert!(replaced.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "live"
                )
            })
        }));
        require_text_survives(&replaced, "ship 0.9.12", "pinned /anchor text").unwrap();
    }
}
