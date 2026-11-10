//! Structured JSON rendering of a conversation export.
//!
//! The document is built from the same sanitized projection as the Markdown
//! and HTML renderings and is lossless about message roles and block kinds:
//! a tool result keeps the `user` role it travels under, and omitted content
//! is flagged instead of dropped. Payload values pass through the shared JSON
//! redactor; the envelope is written by this module and is not redacted by
//! key. Workspace restore points are not part of the document.

use std::collections::HashMap;

use codewhale_command_contract::facets::{
    ConversationExportProjection, ExportBlock, ExportMessage, HistoryEntry, TranscriptProjection,
};
use codewhale_sanitize::sanitize::{
    is_internal_role, redact_json, redact_url_for_display, sanitize_text,
};
use serde_json::{Value, json};

use super::export::{OMISSION_NOTE, format_export_time, tool_names};

const FORMAT_NAME: &str = "codewhale-conversation-export";
const FORMAT_VERSION: u32 = 1;

pub(super) fn render(projection: ConversationExportProjection) -> String {
    let ConversationExportProjection {
        metadata,
        transcript,
        restore_points: _,
    } = projection;
    let names = tool_names(&transcript);
    let (transcript_kind, messages) = match transcript {
        TranscriptProjection::Authoritative(messages) => (
            "authoritative",
            messages
                .into_iter()
                .enumerate()
                .map(|(index, message)| message_json(index + 1, message, &names))
                .collect::<Vec<_>>(),
        ),
        TranscriptProjection::HistoryFallback(entries) => (
            "history_fallback",
            entries
                .into_iter()
                .enumerate()
                .map(|(index, entry)| entry_json(index + 1, entry))
                .collect::<Vec<_>>(),
        ),
    };
    let document = json!({
        "format": FORMAT_NAME,
        "format_version": FORMAT_VERSION,
        "generator": {
            "name": "codewhale",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "exported_at": format_export_time(metadata.exported_at_unix),
        "session": {
            "label": sanitize_text(&metadata.session_label),
            "title": metadata.title.as_deref().map(sanitize_text),
            "provider": sanitize_text(&metadata.provider),
            "model": sanitize_text(&metadata.model),
            "mode": sanitize_text(&metadata.mode),
            "workspace": sanitize_text(&metadata.workspace_name),
            "message_count": metadata.message_count,
        },
        "transcript": transcript_kind,
        "notice": OMISSION_NOTE,
        "messages": messages,
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_string())
}

fn message_json(index: usize, message: ExportMessage, names: &HashMap<String, String>) -> Value {
    let role = sanitize_text(&message.role);
    if is_internal_role(&message.role) {
        return json!({ "index": index, "role": role, "omitted": true, "blocks": [] });
    }
    let blocks: Vec<Value> = message
        .blocks
        .into_iter()
        .map(|block| block_json(block, names))
        .collect();
    json!({ "index": index, "role": role, "blocks": blocks })
}

fn entry_json(index: usize, entry: HistoryEntry) -> Value {
    let (role, text) = match entry {
        HistoryEntry::Sanitized { role, body } => (role, sanitize_text(&body)),
        HistoryEntry::Literal { role, body } => (role, body),
    };
    json!({
        "index": index,
        "role": sanitize_text(&role),
        "blocks": [{ "type": "text", "text": text }],
    })
}

fn block_json(block: ExportBlock, names: &HashMap<String, String>) -> Value {
    match block {
        ExportBlock::Text { text } => json!({ "type": "text", "text": sanitize_text(&text) }),
        ExportBlock::ImageReference { url } => json!({
            "type": "image",
            "url": sanitize_text(&redact_url_for_display(&url)),
        }),
        ExportBlock::ImageOmitted => json!({ "type": "image", "omitted": true }),
        ExportBlock::InternalReasoning => json!({ "type": "reasoning", "omitted": true }),
        ExportBlock::ToolCall {
            id,
            name,
            caller,
            input,
        } => {
            let mut value = json!({
                "type": "tool_use",
                "id": sanitize_text(&id),
                "name": sanitize_text(&name),
                "input": redacted(input),
            });
            if let Some(caller) = caller {
                value["caller"] = json!({
                    "type": sanitize_text(&caller.caller_type),
                    "tool_id": caller.tool_id.as_deref().map(sanitize_text),
                });
            }
            value
        }
        ExportBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            structured,
        } => {
            let mut value = json!({
                "type": "tool_result",
                "tool_use_id": sanitize_text(&tool_use_id),
                "is_error": is_error,
                "content": sanitize_text(&content),
            });
            if let Some(name) = names.get(&tool_use_id) {
                value["name"] = Value::String(sanitize_text(name));
            }
            if let Some(structured) = structured {
                value["structured"] = redacted(structured);
            }
            value
        }
        ExportBlock::ServerToolCall { id, name, input } => json!({
            "type": "server_tool_use",
            "id": sanitize_text(&id),
            "name": sanitize_text(&name),
            "input": redacted(input),
        }),
        ExportBlock::ToolSearchResult {
            tool_use_id,
            content,
        } => json!({
            "type": "tool_search_result",
            "tool_use_id": sanitize_text(&tool_use_id),
            "content": redacted(content),
        }),
        ExportBlock::CodeExecutionResult {
            tool_use_id,
            content,
        } => json!({
            "type": "code_execution_result",
            "tool_use_id": sanitize_text(&tool_use_id),
            "content": redacted(content),
        }),
    }
}

fn redacted(mut value: Value) -> Value {
    redact_json(&mut value, None);
    value
}
