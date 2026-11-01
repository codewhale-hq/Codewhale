use super::*;

pub(super) fn wall_deadline(runtime: &SubAgentRuntime) -> Option<Instant> {
    runtime
        .worker_profile
        .wall_deadline_ms
        .and_then(|deadline| {
            Instant::now().checked_add(Duration::from_millis(
                deadline.saturating_sub(epoch_millis_now()),
            ))
        })
}

#[cfg(test)]
pub(super) fn repair_stopped_tool_calls(messages: &mut Vec<Message>, cause: &str) {
    // Keep the final call blocks, so keys remain borrowed from their original
    // identities while repair changes the message vector.
    let final_calls = messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.content.clone())
        .unwrap_or_default();
    let repair =
        codewhale_runtime::tool_history_repair::repair_tool_call_pairs_for_provider(messages);
    let final_message = messages
        .iter()
        .rposition(|message| message.role == Role::Assistant);
    for (message_index, block_index) in repair.repaired_result_positions {
        if !final_message.is_some_and(|index| message_index > index) {
            continue;
        }
        let block = &mut messages[message_index].content[block_index];
        let key = block.tool_call_key();
        let is_final_call = key.is_some_and(|key| !key.as_str().trim().is_empty())
            && final_calls.iter().any(|call| {
                matches!(call, ContentBlock::ToolUse { .. }) && call.tool_call_key() == key
            });
        if is_final_call && let ContentBlock::ToolResult { content, .. } = block {
            *content = format!(
                "Tool call not executed: task execution stopped at its budget boundary. Terminal status: budget_exhausted. {cause}"
            );
        }
    }
}

const HANDBACK_DIGEST_DIR: &str = "subagent-results";

/// Private state-root path of a child's budget-death result artifact
/// (#6536). Derived from `agent_id`, never accepted from input.
fn digest_artifact_relative_path(agent_id: &str) -> PathBuf {
    let digest = codewhale_runtime::hashing::sha256_hex(agent_id.as_bytes());
    Path::new(".codewhale")
        .join("state")
        .join(HANDBACK_DIGEST_DIR)
        .join(format!("{digest}.md"))
}

/// Record the child's budget-death deliverable as a private file under the
/// manager state root (#6536). The host stores its retained result
/// here before terminal delivery. Blocking IO runs under `spawn_blocking`.
///
/// Known limitation: the file outlives the agent record; removing an agent
/// does not delete it.
pub(super) async fn write_digest_artifact(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    body: String,
) -> Option<PathBuf> {
    let state_root = runtime.manager.read().await.state_root.clone();
    let agent = agent_id.to_string();
    let written = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let relative = digest_artifact_relative_path(&agent);
        let path = checked_subagent_state_path(&state_root, &relative)?;
        create_private_subagent_transcript(&state_root, &path, body.as_bytes())?;
        Ok(path)
    })
    .await;
    match written {
        Ok(Ok(path)) => Some(path),
        Ok(Err(error)) => {
            tracing::warn!(target: "subagent", agent_id, %error, "budget digest artifact not written");
            None
        }
        Err(error) => {
            tracing::warn!(target: "subagent", agent_id, %error, "budget digest artifact task failed");
            None
        }
    }
}

/// Everything is bounded; the parent gets evidence, never a report.
pub fn fallback_partial_text(messages: &[Message]) -> String {
    const MAX_TEXT_CHARS: usize = 4_000;
    const MAX_TOOL_ENTRIES: usize = 12;
    const MAX_THINKING_BYTES: usize = 1_500;

    if let Some(text) = messages
        .iter()
        .rev()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.content.iter().rev())
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.trim().is_empty() => Some(text),
            _ => None,
        })
    {
        return text.chars().take(MAX_TEXT_CHARS).collect();
    }
    let mut tools = Vec::new();
    let mut extra_tools = 0usize;
    let mut thinking = None;
    for message in messages.iter().rev() {
        if message.role != Role::Assistant {
            continue;
        }
        for block in message.content.iter().rev() {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    if tools.len() < MAX_TOOL_ENTRIES {
                        tools.push(format!("{name} {}", tool_target_preview(input)));
                    } else {
                        extra_tools += 1;
                    }
                }
                ContentBlock::Thinking { thinking: text, .. }
                    if thinking.is_none() && !text.trim().is_empty() =>
                {
                    thinking = Some(text);
                }
                _ => {}
            }
        }
    }
    if tools.is_empty() && thinking.is_none() {
        return "No assistant text was recorded; inspect the checkpoint for completed tool work."
            .to_string();
    }
    let mut digest =
        String::from("No assistant text was recorded. Work recorded before the budget death:");
    if !tools.is_empty() {
        digest.push_str("\nTool calls (newest first):");
        for entry in &tools {
            digest.push_str(&format!("\n- {entry}"));
        }
        if extra_tools > 0 {
            digest.push_str(&format!("\n- ...and {extra_tools} more"));
        }
    }
    if let Some(text) = thinking {
        digest.push_str("\nLatest reasoning (unverified, may be incomplete):\n");
        digest.push_str(&lifecycle::text_preview(text, MAX_THINKING_BYTES));
    }
    digest
}

/// One-line target for a recorded tool call: the well-known path/commandish
/// key when present, else a truncated rendering of the whole input.
fn tool_target_preview(input: &serde_json::Value) -> String {
    const KEYS: [&str; 7] = [
        "path",
        "file",
        "file_path",
        "command",
        "pattern",
        "query",
        "url",
    ];
    for key in KEYS {
        if let Some(hit) = input.get(key).and_then(serde_json::Value::as_str)
            && !hit.trim().is_empty()
        {
            return lifecycle::text_preview(hit, 120);
        }
    }
    if let Some(hit) = input.as_str() {
        return lifecycle::text_preview(hit, 120);
    }
    lifecycle::text_preview(&input.to_string(), 120)
}
