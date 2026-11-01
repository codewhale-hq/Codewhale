use crate::{ContentBlock, Message, Role};

/// Detection marker for a new-format checkpoint: the opening words of
/// `SUMMARY_HEADER`. A new checkpoint is recognised only when its first
/// block starts with this marker and its second block is the provenance
/// block, so a user message that merely quotes the phrase stays a user
/// message.
pub const COMPACTION_SUMMARY_MARKER: &str = "Codewhale handoff note";

/// Legacy detection marker only. Checkpoints written before the handoff note
/// opened with this sentence. Saved sessions that still carry it must be
/// recognised so the next pass replaces that checkpoint instead of stacking a
/// second one.
pub const LEGACY_V2_COMPACTION_SUMMARY_MARKER: &str =
    "Another language model started to solve this problem";

pub const COMPACTION_CHECKPOINT_PROVENANCE: &str = "<!-- codewhale.compaction-checkpoint.v1 -->";

/// Request-time recognition: exactly the header text block plus the
/// provenance block. User text merely quoting a marker keeps its original
/// wire position. Checkpoints saved before the handoff note still pass, so
/// their position survives restore and the next pass replaces them.
pub fn is_wire_compaction_checkpoint_message(message: &Message) -> bool {
    let [
        ContentBlock::Text {
            text,
            cache_control: None,
        },
        ContentBlock::Text {
            text: provenance,
            cache_control: None,
        },
    ] = message.content.as_slice()
    else {
        return false;
    };
    message.role == Role::User
        && (text.starts_with(COMPACTION_SUMMARY_MARKER)
            || text.starts_with(LEGACY_V2_COMPACTION_SUMMARY_MARKER))
        && provenance == COMPACTION_CHECKPOINT_PROVENANCE
}

pub const RESTORED_CHECKPOINT_TURN_META: &str = concat!(
    "<turn_meta>\n",
    "Input provenance: subagent_handoff (non-authoritative)\n",
    "Restore projection: subagent_checkpoint_v1\n",
    "</turn_meta>",
);

pub const RESTORED_COMPLETION_HEADER: &str = "[Codewhale restored sub-agent checkpoint]";

pub const RESTORED_COMPLETIONS_HEADER: &str = "[Codewhale restored sub-agent checkpoints]";

pub const RESTORED_RUNNING_HEADER: &str = "[Codewhale restored sub-agent runtime checkpoint]";

pub const RESTORED_TOPOLOGY_HEADER: &str = "[Codewhale restored Agent topology checkpoint]";

/// Return the user-safe display body for an already projected checkpoint.
/// The exact metadata marker keeps arbitrary user-authored text on the normal
/// conversation path.
pub fn restored_subagent_checkpoint_display(message: &Message) -> Option<&str> {
    if message.role != "user" {
        return None;
    }
    let [
        ContentBlock::Text {
            text,
            cache_control: first_cache,
        },
        ContentBlock::Text {
            text: turn_meta,
            cache_control: meta_cache,
        },
    ] = message.content.as_slice()
    else {
        return None;
    };
    if first_cache.is_some()
        || meta_cache.is_some()
        || turn_meta != RESTORED_CHECKPOINT_TURN_META
        || ![
            RESTORED_COMPLETION_HEADER,
            RESTORED_COMPLETIONS_HEADER,
            RESTORED_RUNNING_HEADER,
            RESTORED_TOPOLOGY_HEADER,
        ]
        .iter()
        .any(|header| text.starts_with(header))
    {
        return None;
    }
    Some(text)
}

/// Classification used when locating a user-authored turn in the session log.
///
/// Runtime and tool messages are skipped because their provider-compatible
/// `role = "user"` is not user authority. Unsupported user content is a real
/// turn boundary, however, so callers must not skip it and edit an older turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTurnPromptKind {
    /// Assistant messages, tool results, and runtime-owned control messages.
    NotPrompt,
    /// A genuine user turn containing editable text.
    Editable,
    /// A genuine user turn without editable text, such as an image-only turn.
    Unsupported,
}

/// Classify `message` for edit-last-turn and admitted-display handling.
#[must_use]
pub fn classify_user_turn_prompt(message: &Message) -> UserTurnPromptKind {
    if message.role != Role::User {
        return UserTurnPromptKind::NotPrompt;
    }
    if message.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolResult { .. }
                | ContentBlock::ToolSearchToolResult { .. }
                | ContentBlock::CodeExecutionToolResult { .. }
        )
    }) {
        return UserTurnPromptKind::NotPrompt;
    }
    if is_runtime_owned_user_message(message) || is_wire_compaction_checkpoint_message(message) {
        return UserTurnPromptKind::NotPrompt;
    }

    let turn_metadata_index = turn_metadata_text(message).map(|(index, _)| index);
    if message.content.iter().enumerate().any(|(index, block)| {
        Some(index) != turn_metadata_index && matches!(block, ContentBlock::Text { .. })
    }) {
        UserTurnPromptKind::Editable
    } else {
        UserTurnPromptKind::Unsupported
    }
}

/// True when a `role = "user"` message is runtime-owned rather than
/// user-authored. Runtime authority is accepted only from the engine-owned
/// structural `<turn_meta>` block, never from arbitrary user text that happens
/// to resemble a runtime envelope or metadata marker.
pub fn is_runtime_owned_user_message(message: &Message) -> bool {
    restored_subagent_checkpoint_display(message).is_some()
        || has_non_authoritative_turn_provenance(message)
}

/// Return engine-owned metadata in either the current trailing shape or the
/// historical leading shape. Requiring a separate prompt block prevents a
/// user who submits `<turn_meta>…</turn_meta>` as ordinary text from minting
/// authority.
pub fn turn_metadata_text(message: &Message) -> Option<(usize, &str)> {
    if message.content.len() < 2 {
        return None;
    }
    if let Some(ContentBlock::Text {
        text,
        cache_control: None,
    }) = message.content.last()
    {
        let trimmed = text.trim();
        if is_complete_turn_metadata(trimmed) {
            return Some((message.content.len() - 1, trimmed));
        }
    }
    // Sessions written before the metadata-tail migration used
    // `[turn_meta, prompt, ...]`. Match the same conservative legacy shape as
    // the transcript renderer: a metadata envelope first and ordinary text
    // last. A single user-authored metadata example is never hidden.
    let ContentBlock::Text {
        text,
        cache_control: None,
    } = message.content.first()?
    else {
        return None;
    };
    let trimmed = text.trim();
    let trailing_text_is_ordinary = matches!(
        message.content.last(),
        Some(ContentBlock::Text { text, .. }) if !is_complete_turn_metadata(text.trim())
    );
    (is_complete_turn_metadata(trimmed) && trailing_text_is_ordinary).then_some((0, trimmed))
}

fn is_complete_turn_metadata(text: &str) -> bool {
    text.starts_with("<turn_meta>") && text.ends_with("</turn_meta>")
}

/// Recognize both the current condensed provenance line and the legacy
/// provenance/authority pair. Any explicitly non-authoritative provenance is
/// runtime-owned; this stays correct as new provenance variants are added.
fn has_non_authoritative_turn_provenance(message: &Message) -> bool {
    let Some((_, metadata)) = turn_metadata_text(message) else {
        return false;
    };
    let mut has_provenance = false;
    let mut condensed_non_authoritative = false;
    let mut legacy_non_authoritative = false;
    for line in metadata.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Input provenance: ") {
            has_provenance = true;
            condensed_non_authoritative |= value.ends_with(" (non-authoritative)");
        }
        legacy_non_authoritative |= line == "Input authority: non_authoritative";
    }
    condensed_non_authoritative || (has_provenance && legacy_non_authoritative)
}
