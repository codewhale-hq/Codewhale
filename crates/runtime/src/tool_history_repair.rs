//! Deterministic integrity repair for persisted model-visible tool history.
//!
//! Session JSON predates a durable per-call journal, so process exit can leave
//! a `tool_use` without its terminal `tool_result`. Provider APIs reject that
//! shape. This module repairs the existing message format without changing its
//! schema and returns a bounded diagnostic receipt for every mutation.

use std::collections::{HashMap, HashSet};

use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message};

/// The repository's one spelling for a tool call the process lost while it was
/// running. The session-facing repair writes it here, and a history rebuilt
/// from turn records makes the same call say the same thing.
pub const CRASH_REPAIR_CONTENT: &str =
    "Tool call interrupted by process exit; terminal status: crashed_and_repaired.";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRepairReceipt {
    pub repaired_call_ids: Vec<String>,
    pub duplicate_result_ids: Vec<String>,
    pub orphan_result_ids: Vec<String>,
    /// Exact synthetic result positions in the repaired messages; callers must
    /// not rediscover them by potentially reused provider IDs.
    pub repaired_result_positions: Vec<(usize, usize)>,
}

impl ToolRepairReceipt {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repaired_call_ids.is_empty()
            && self.duplicate_result_ids.is_empty()
            && self.orphan_result_ids.is_empty()
    }

    fn visible_message(&self) -> String {
        format!(
            "[tool_history_repair] Repaired {} crashed tool call(s); quarantined {} duplicate and {} orphan terminal result(s).",
            self.repaired_call_ids.len(),
            self.duplicate_result_ids.len(),
            self.orphan_result_ids.len(),
        )
    }
}

/// Repair tool-use/result integrity in place.
///
/// The first terminal result after a known call and before the next assistant
/// turn is retained. Results that precede their call, arrive after a later
/// assistant turn, reference no call, or repeat a retained result are
/// quarantined by removing them from model-visible history. Every dangling
/// call receives a synthetic error result directly after its assistant call
/// message. A visible system receipt makes the repair apparent after resume.
pub fn repair_tool_call_pairs(messages: &mut Vec<Message>) -> ToolRepairReceipt {
    repair_tool_call_pairs_inner(messages, true)
}

/// Repair an ephemeral provider request without appending a trailing receipt.
///
/// Anthropic-style APIs interpret a final assistant message as a completion
/// prefill. Pair repair must therefore leave the synthetic user tool result as
/// the request tail; the durable session-facing path owns the visible receipt.
pub fn repair_tool_call_pairs_for_provider(messages: &mut Vec<Message>) -> ToolRepairReceipt {
    repair_tool_call_pairs_inner(messages, false)
}

fn repair_tool_call_pairs_inner(
    messages: &mut Vec<Message>,
    append_visible_receipt: bool,
) -> ToolRepairReceipt {
    let mut pending_call_message = None;
    let mut pending_call_ids = Vec::new();
    let mut retained_for_pending = HashSet::new();
    let mut missing_by_message: HashMap<usize, Vec<(String, Option<String>)>> = HashMap::new();
    let mut repaired_call_ids = Vec::new();
    let mut duplicate_result_ids = Vec::new();
    let mut orphan_result_ids = Vec::new();
    let mut keep_results = HashSet::new();
    let mut result_ordinal = 0usize;

    for (message_index, message) in messages.iter().enumerate() {
        if message.role == "assistant"
            || message.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE
        {
            record_missing_results(
                pending_call_message,
                &pending_call_ids,
                &retained_for_pending,
                &mut missing_by_message,
                &mut repaired_call_ids,
            );
            pending_call_ids = message
                .content
                .iter()
                .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
                .collect();
            pending_call_message = (!pending_call_ids.is_empty()).then_some(message_index);
            retained_for_pending.clear();
        }
        for block in &message.content {
            let ContentBlock::ToolResult { tool_use_id, .. } = block else {
                continue;
            };
            let ordinal = result_ordinal;
            result_ordinal = result_ordinal.saturating_add(1);

            let follows_known_call = pending_call_message
                .is_some_and(|call_index| call_index < message_index)
                && block
                    .tool_call_key()
                    .is_some_and(|key| !key.as_str().trim().is_empty())
                && pending_call_ids
                    .iter()
                    .filter(|call| call.tool_call_key() == block.tool_call_key())
                    .count()
                    == 1
                && pending_call_ids.iter().any(|call| {
                    call.tool_call_key() == block.tool_call_key()
                        && matches!(call, ContentBlock::ToolUse { id, .. } if id == tool_use_id)
                });
            if !follows_known_call {
                orphan_result_ids.push(tool_use_id.clone());
            } else if !retained_for_pending.insert(block.tool_call_key().expect("tool result key"))
            {
                duplicate_result_ids.push(tool_use_id.clone());
            } else {
                keep_results.insert(ordinal);
            }
        }
    }
    record_missing_results(
        pending_call_message,
        &pending_call_ids,
        &retained_for_pending,
        &mut missing_by_message,
        &mut repaired_call_ids,
    );

    let mut receipt = ToolRepairReceipt {
        repaired_call_ids,
        duplicate_result_ids,
        orphan_result_ids,
        repaired_result_positions: Vec::new(),
    };
    if receipt.is_empty() {
        return receipt;
    }

    let original = std::mem::take(messages);
    let mut rebuilt: Vec<Message> = Vec::with_capacity(
        original
            .len()
            .saturating_add(receipt.repaired_call_ids.len()),
    );
    let mut seen_result_ordinal = 0usize;
    let mut dropped_since_push = false;

    for (message_index, message) in original.into_iter().enumerate() {
        let missing_after_message = missing_by_message
            .remove(&message_index)
            .unwrap_or_default();
        let mut filtered = message;
        filtered.content.retain(|block| {
            if matches!(block, ContentBlock::ToolResult { .. }) {
                let keep = keep_results.contains(&seen_result_ordinal);
                seen_result_ordinal = seen_result_ordinal.saturating_add(1);
                keep
            } else {
                true
            }
        });
        if filtered.content.is_empty() {
            dropped_since_push = true;
        } else if dropped_since_push
            && filtered.role == "assistant"
            && let Some(previous) = rebuilt.last_mut()
            && previous.role == "assistant"
        {
            previous.content.extend(filtered.content);
            dropped_since_push = false;
        } else {
            rebuilt.push(filtered);
            dropped_since_push = false;
        }

        if !missing_after_message.is_empty() {
            receipt.repaired_result_positions.extend(
                (0..missing_after_message.len()).map(|block_index| (rebuilt.len(), block_index)),
            );
            rebuilt.push(Message {
                role: Role::User,
                content: missing_after_message
                    .into_iter()
                    .map(|(tool_use_id, execution_id)| ContentBlock::ToolResult {
                        execution_id,
                        tool_use_id,
                        content: CRASH_REPAIR_CONTENT.to_string(),
                        is_error: Some(true),
                        content_blocks: None,
                    })
                    .collect(),
            });
        }
    }

    if append_visible_receipt {
        let receipt_block = ContentBlock::Text {
            text: receipt.visible_message(),
            cache_control: None,
        };
        match rebuilt.last_mut() {
            Some(last)
                if last.role == "assistant"
                    || last.role == codewhale_models::INTERRUPTED_ASSISTANT_ROLE =>
            {
                last.content.push(receipt_block)
            }
            _ => rebuilt.push(Message {
                role: Role::Assistant,
                content: vec![receipt_block],
            }),
        }
    }
    *messages = rebuilt;
    receipt
}

fn record_missing_results(
    call_message: Option<usize>,
    calls: &[&ContentBlock],
    retained_results: &HashSet<codewhale_models::ToolCallKey<'_>>,
    missing_by_message: &mut HashMap<usize, Vec<(String, Option<String>)>>,
    repaired_call_ids: &mut Vec<String>,
) {
    let Some(message_index) = call_message else {
        return;
    };
    let missing = calls
        .iter()
        .filter(|call| {
            !call
                .tool_call_key()
                .is_some_and(|key| retained_results.contains(&key))
        })
        .filter_map(|call| match call {
            ContentBlock::ToolUse {
                id, execution_id, ..
            } => Some((id.clone(), execution_id.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return;
    }
    repaired_call_ids.extend(missing.iter().map(|(id, _)| id.clone()));
    missing_by_message.insert(message_index, missing);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool_call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: "read_file".to_string(),
                input: json!({"path": "README.md"}),
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

    fn text(role: &str, content: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: content.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn repair_preserves_execution_identity_and_refuses_mismatched_results() {
        let mut messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":"first","name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire","execution_id":"first","content":"kept"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":"second","name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire","execution_id":"first","content":"stale"}]}
        ])).unwrap();
        let first_pair = messages[..2].to_vec();
        let receipt = repair_tool_call_pairs_for_provider(&mut messages);
        assert_eq!(&messages[..2], first_pair.as_slice());
        assert_eq!(receipt.orphan_result_ids, ["wire"]);
        assert_eq!(receipt.repaired_result_positions, [(3, 0)]);
        assert!(
            matches!(&messages[3].content[0], ContentBlock::ToolResult { tool_use_id, execution_id: Some(id), content, is_error: Some(true), .. }
            if tool_use_id == "wire" && id == "second" && content == CRASH_REPAIR_CONTENT)
        );
        assert!(repair_tool_call_pairs_for_provider(&mut messages).is_empty());

        for (call_id, result_id, result_wire) in [
            (Some("local"), None, "wire"),
            (None, Some("wire"), "wire"),
            (Some("local"), Some("local"), "wrong-wire"),
            (Some(""), Some(""), "wire"),
        ] {
            let mut malformed: Vec<Message> = serde_json::from_value(json!([
                {"role":"assistant","content":[{"type":"tool_use","id":"wire","execution_id":call_id,"name":"read","input":{}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":result_wire,"execution_id":result_id,"content":"not a match"}]}
            ])).unwrap();
            let receipt = repair_tool_call_pairs_for_provider(&mut malformed);
            assert_eq!(receipt.orphan_result_ids.len(), 1);
            assert_eq!(receipt.repaired_result_positions, [(1, 0)]);
            assert!(
                matches!(&malformed[1].content[0], ContentBlock::ToolResult { execution_id, .. } if execution_id.as_deref() == call_id)
            );
        }
    }

    #[test]
    fn duplicate_execution_ids_cannot_lend_one_result_to_two_calls() {
        let mut messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"assistant","content":[
                {"type":"tool_use","id":"wire-a","execution_id":"duplicate","name":"read","input":{}},
                {"type":"tool_use","id":"wire-b","execution_id":"duplicate","name":"read","input":{}}
            ]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"wire-a","execution_id":"duplicate","content":"not attributable"}]}
        ])).unwrap();
        let receipt = repair_tool_call_pairs_for_provider(&mut messages);
        assert_eq!(receipt.orphan_result_ids, ["wire-a"]);
        assert_eq!(receipt.repaired_result_positions, [(1, 0), (1, 1)]);
        assert!(
            messages[1]
                .content
                .iter()
                .all(|block| matches!(block, ContentBlock::ToolResult {
            execution_id: Some(id), content, is_error: Some(true), ..
        } if id == "duplicate" && content == CRASH_REPAIR_CONTENT))
        );
    }

    #[test]
    fn well_formed_history_is_unchanged() {
        let mut messages = vec![tool_call("call-1"), tool_result("call-1", "ok")];
        let before = messages.clone();

        let receipt = repair_tool_call_pairs(&mut messages);

        assert!(receipt.is_empty());
        assert_eq!(messages, before);
    }

    #[test]
    fn repeated_provider_call_id_is_scoped_to_each_assistant_turn() {
        let mut messages = vec![
            tool_call("call-reused"),
            tool_result("call-reused", "hydrated"),
            tool_call("call-reused"),
            tool_result("call-reused", "executed"),
        ];
        let before = messages.clone();

        let receipt = repair_tool_call_pairs_for_provider(&mut messages);

        assert!(receipt.is_empty());
        assert_eq!(messages, before);
    }

    #[test]
    fn repairs_dangling_calls_beside_their_assistant_message() {
        let mut messages = vec![
            tool_call("call-1"),
            text("assistant", "later assistant text"),
        ];

        let receipt = repair_tool_call_pairs(&mut messages);

        assert_eq!(receipt.repaired_call_ids, vec!["call-1"]);
        assert_eq!(messages[1].role, "user");
        assert!(matches!(
            &messages[1].content[0],
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: Some(true),
                ..
            } if tool_use_id == "call-1" && content.contains("crashed_and_repaired")
        ));
        assert_eq!(messages.last().expect("receipt").role, "assistant");
        assert_eq!(messages.len(), 3);

        let mut dropped_between_assistants = vec![
            text("user", "question"),
            text("assistant", "first answer"),
            tool_result("orphan", "bad"),
            text("assistant", "second answer"),
        ];
        let receipt = repair_tool_call_pairs(&mut dropped_between_assistants);
        assert_eq!(receipt.orphan_result_ids, vec!["orphan"]);
        let roles: Vec<_> = dropped_between_assistants
            .iter()
            .map(|message| message.role.to_string())
            .collect();
        assert_eq!(roles, ["user", "assistant"]);
        assert_eq!(dropped_between_assistants[1].content.len(), 3);
    }

    #[test]
    fn provider_repair_never_appends_an_assistant_prefill_receipt() {
        let mut messages = vec![tool_call("call-1")];

        let receipt = repair_tool_call_pairs_for_provider(&mut messages);

        assert_eq!(receipt.repaired_call_ids, vec!["call-1"]);
        assert_eq!(messages.last().expect("synthetic result").role, "user");
        assert!(!messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { text, .. }
                        if text.contains("[tool_history_repair]")
                )
            })
        }));
    }

    #[test]
    fn quarantines_orphan_and_duplicate_results_without_losing_other_blocks() {
        let mut mixed_result = tool_result("call-1", "duplicate");
        mixed_result.content.push(ContentBlock::Text {
            text: "keep me".to_string(),
            cache_control: None,
        });
        let mut messages = vec![
            tool_result("orphan", "bad"),
            tool_call("call-1"),
            tool_result("call-1", "first"),
            mixed_result,
        ];

        let receipt = repair_tool_call_pairs(&mut messages);

        assert_eq!(receipt.orphan_result_ids, vec!["orphan"]);
        assert_eq!(receipt.duplicate_result_ids, vec!["call-1"]);
        let result_contents: Vec<_> = messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(result_contents, vec!["first"]);
        assert!(messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text, .. } if text == "keep me"))
        }));
    }

    #[test]
    fn repair_is_idempotent() {
        let mut messages = vec![tool_call("call-1"), tool_result("orphan", "bad")];

        let first = repair_tool_call_pairs(&mut messages);
        let after_first = messages.clone();
        let second = repair_tool_call_pairs(&mut messages);

        assert!(!first.is_empty());
        assert!(second.is_empty());
        assert_eq!(messages, after_first);
    }

    #[test]
    fn result_preceding_its_call_is_orphaned_and_call_is_repaired() {
        let mut messages = vec![tool_result("call-1", "too early"), tool_call("call-1")];

        let receipt = repair_tool_call_pairs(&mut messages);

        assert_eq!(receipt.orphan_result_ids, vec!["call-1"]);
        assert_eq!(receipt.repaired_call_ids, vec!["call-1"]);
        assert!(messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { content, .. }
                        if content.contains("crashed_and_repaired")
                )
            })
        }));
    }

    #[test]
    fn result_after_a_later_assistant_turn_is_quarantined_as_too_late() {
        let mut messages = vec![
            tool_call("call-1"),
            text("assistant", "a later model turn"),
            tool_result("call-1", "too late"),
        ];

        let receipt = repair_tool_call_pairs(&mut messages);

        assert_eq!(receipt.orphan_result_ids, vec!["call-1"]);
        assert_eq!(receipt.repaired_call_ids, vec!["call-1"]);
        assert!(!messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { content, .. } if content == "too late")
            })
        }));
    }
}
