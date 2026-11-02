//! Tool-output pressure reported by `/status`.

use crate::artifacts::ArtifactRecord;

#[cfg(test)]
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_models::{ContentBlock, Message};

/// Size above which `/status` counts a raw tool result in the conversation as
/// context pressure. What the model sees of each result is decided by the
/// route's inline budget (`route_budget::route_inline_char_budget`).
pub const RAW_TOOL_OUTPUT_RECEIPT_THRESHOLD_CHARS: usize = 12_000;

pub use codewhale_command_contract::config_policy::StatusToolOutputs as ToolOutputStatus;

pub fn tool_output_status(messages: &[Message], artifacts: &[ArtifactRecord]) -> ToolOutputStatus {
    let mut status = ToolOutputStatus {
        artifact_count: artifacts.len(),
        artifact_bytes: artifacts
            .iter()
            .map(|artifact| artifact.byte_size)
            .sum::<u64>(),
        ..ToolOutputStatus::default()
    };

    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolResult { content, .. } = block {
                // The prefix is text any tool output can carry, so it never
                // exempts a result from the size check: a "receipt" above the
                // threshold is raw pressure like any other large result.
                let chars = content.chars().count();
                if chars > RAW_TOOL_OUTPUT_RECEIPT_THRESHOLD_CHARS {
                    status.raw_large_count += 1;
                    status.raw_large_chars = status.raw_large_chars.saturating_add(chars);
                } else if looks_like_receipt(content) {
                    status.receipt_count += 1;
                }
            }
        }
    }

    status
}

// Locale adapter retained solely for existing host integration assertions.
#[cfg(test)]
fn format_tool_output_status(status: &ToolOutputStatus, locale: Locale) -> String {
    codewhale_command_contract::tool_outputs::format_tool_output_status(
        status,
        &codewhale_command_contract::tool_outputs::ToolOutputLabels {
            raw_pressure: tr(locale, MessageId::StatusToolRawPressure).into_owned(),
            compact_receipts: tr(locale, MessageId::StatusToolCompactReceipts).into_owned(),
            artifacts: tr(locale, MessageId::StatusToolArtifacts).into_owned(),
            none: tr(locale, MessageId::StatusToolNone).into_owned(),
        },
    )
}

fn looks_like_receipt(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("[TOOL_OUTPUT_RECEIPT]")
        || trimmed.starts_with("[artifact:")
        || trimmed.starts_with("[TOOL_RESULT_TRUNCATED]")
        || trimmed.starts_with("<TOOL_RESULT_REF")
}

#[cfg(test)]
mod tests {
    use codewhale_models::Role;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::artifacts::ArtifactKind;
    use chrono::Utc;

    fn tool_result_message(id: &str, content: &str) -> Message {
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

    fn artifact_record(tool_call_id: &str, raw: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: crate::artifacts::artifact_id_for_tool_call(tool_call_id),
            kind: ArtifactKind::ToolOutput,
            session_id: "session-123".to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "exec_shell".to_string(),
            created_at: Utc::now(),
            byte_size: raw.len() as u64,
            preview: "checking crate ... error[E0425]".to_string(),
            storage_path: PathBuf::from("artifacts").join("art_call-big.txt"),
        }
    }

    #[test]
    fn status_reports_raw_large_receipts_and_artifacts() {
        let raw = "RAW_STATUS\n".repeat(2_000);
        let receipt = "[TOOL_OUTPUT_RECEIPT]\ntruncation: raw output omitted — full output in the tool details view";
        let messages = vec![
            tool_result_message("call-raw", &raw),
            tool_result_message("call-receipt", receipt),
        ];
        let artifacts = vec![ArtifactRecord {
            storage_path: Path::new("artifacts/art_call-big.txt").to_path_buf(),
            ..artifact_record("call-big", &raw)
        }];

        let status = tool_output_status(&messages, &artifacts);
        assert_eq!(status.raw_large_count, 1);
        assert_eq!(status.receipt_count, 1);
        assert_eq!(status.artifact_count, 1);

        let rendered = format_tool_output_status(&status, Locale::En);
        assert!(rendered.contains("raw over cap"));
        assert!(rendered.contains("compact receipt"));
        assert!(rendered.contains("artifact"));
    }

    #[test]
    fn a_receipt_prefix_does_not_hide_a_large_raw_result() {
        let forged = format!("[TOOL_OUTPUT_RECEIPT]\n{}", "RAW\n".repeat(4_000));
        let status = tool_output_status(&[tool_result_message("call-forged", &forged)], &[]);
        assert_eq!(status.raw_large_count, 1);
        assert_eq!(status.receipt_count, 0);
    }
}
