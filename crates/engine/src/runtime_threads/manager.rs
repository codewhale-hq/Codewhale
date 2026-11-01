use super::*;

pub mod history;

pub(super) const EVENT_CHANNEL_CAPACITY: usize = 1024;
pub(super) const MAX_ACTIVE_THREADS_DEFAULT: usize = 8;
pub(super) const MAX_PENDING_DYNAMIC_TOOL_CALLS: usize = 128;
pub(super) const SUMMARY_LIMIT: usize = 280;
pub(super) const STREAM_DELTA_BATCH_MAX_LATENCY: Duration = Duration::from_millis(32);
pub(super) const STREAM_DELTA_BATCH_MAX_BYTES: usize = 16 * 1024;
/// How long `steer_turn` observes the engine's verdict before answering with
/// the honest `Queued` receipt instead. A steer sent into a streaming turn
/// settles in milliseconds; one sent behind a long tool call cannot, and an
/// API request must not hang for the length of a tool call (#6276).
pub(super) const STEER_SETTLE_WAIT: Duration = Duration::from_secs(2);
/// Why a steer never reached the model, in the words a client can show.
pub(super) const STEER_DROPPED_REASON: &str =
    "the turn moved on before the engine committed it, so the model never saw it — resend it";
pub(super) const REQUEST_USER_INPUT_TOOL_NAME: &str = "request_user_input";
pub(super) const REDACTED_USER_INPUT_RECEIPT: &str = "User input submitted";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamDeltaKind {
    Message,
    Reasoning,
}

pub(super) struct StreamDeltaBatch {
    pub(super) content: String,
    pub(super) pending_event: Option<EngineEvent>,
    pub(super) channel_closed: bool,
}

pub(super) async fn coalesce_stream_delta(
    engine: &EngineHandle,
    kind: StreamDeltaKind,
    mut content: String,
) -> StreamDeltaBatch {
    let deadline = tokio::time::Instant::now() + STREAM_DELTA_BATCH_MAX_LATENCY;
    let mut pending_event = None;
    let mut channel_closed = false;
    let mut rx = engine.rx_event.write().await;

    while content.len() < STREAM_DELTA_BATCH_MAX_BYTES {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let next = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                channel_closed = true;
                break;
            }
            Err(_) => break,
        };
        match next {
            EngineEvent::MessageDelta { content: next, .. } if kind == StreamDeltaKind::Message => {
                content.push_str(&next);
            }
            EngineEvent::ThinkingDelta { content: next, .. }
                if kind == StreamDeltaKind::Reasoning =>
            {
                content.push_str(&next);
            }
            event => {
                pending_event = Some(event);
                break;
            }
        }
    }

    StreamDeltaBatch {
        content,
        pending_event,
        channel_closed,
    }
}

/// The system prompt a sync installs. A history that already carries its
/// compaction checkpoint outranks a legacy summary section persisted in the
/// thread record's prompt, which would otherwise replace that checkpoint.
fn synced_system_prompt(prompt: Option<&str>, messages: &[Message]) -> Option<SystemPrompt> {
    let prompt = prompt.map(|text| SystemPrompt::Text(text.to_string()));
    if crate::compaction::history_is_post_provenance(messages) {
        crate::compaction::strip_compaction_summaries(prompt.as_ref())
    } else {
        prompt
    }
}

/// A fresh durable record id: `<prefix>_<32 hex>`. Records live in flat
/// per-kind directories and saves replace by id, so ids carry a full UUID; an
/// 8-hex suffix (32 bits) collides often enough to overwrite another record.
pub(super) fn runtime_record_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

/// Longest tool call id this store will look up, in bytes.
pub(super) const MAX_TOOL_CALL_ID_BYTES: usize = 256;

/// Whether a **provider's** tool call id can be looked up in the store.
///
/// This is deliberately not [`validated_record_id`]. A record id is ours and
/// minted in a shape we chose; a tool call id is the model endpoint's, echoed
/// back to us as an opaque string, and it is only ever *compared* — against a
/// receipt's `tool_call_id` and an item's `tool_use_id` — never used as a
/// path, a command, or a file name. Real endpoints hand out shapes the record
/// charset refuses (`call_01_f3d82r…|f8912d4c-…` from one gateway, `toolu_…`
/// from another), and rejecting those made every command's workspace span
/// unreadable while looking exactly like "no such call". Only emptiness and
/// control characters are refused, plus a length bound so a nonsense URL
/// cannot make the store scan work hard.
pub(super) fn usable_provider_call_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_TOOL_CALL_ID_BYTES && !id.chars().any(char::is_control)
}

pub(super) fn agent_mail_workspace_id(workspace: &Path) -> Result<String> {
    let canonical = workspace
        .canonicalize()
        .with_context(|| format!("resolve Agent Mail workspace {}", workspace.display()))?;
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("ws_{digest}"))
}

pub(super) fn agent_mail_sender_identity(thread: &ThreadRecord) -> Result<String> {
    thread
        .task_id
        .as_ref()
        .or(thread.session_id.as_ref())
        .cloned()
        .with_context(|| {
            format!(
                "Thread '{}' is not addressable by Agent Mail: task_id or session_id is required",
                thread.id
            )
        })
}

fn agent_mail_address(owner_id: &str, thread: &ThreadRecord) -> Result<AgentMailAddress> {
    let address = AgentMailAddress {
        owner_id: owner_id.to_string(),
        workspace_id: agent_mail_workspace_id(&thread.workspace)?,
        thread_id: thread.id.clone(),
        task_id: thread.task_id.clone(),
        session_id: thread.session_id.clone(),
    };
    address.validate().map_err(|error| anyhow!(error))?;
    Ok(address)
}

pub(super) fn agent_mail_token_is_credential(token: &str) -> bool {
    let trimmed = token
        .trim_matches(|ch: char| ch.is_ascii_punctuation() && !matches!(ch, '_' | '-' | '=' | ':'));
    let lower = trimmed.to_ascii_lowercase();
    if [
        "sk-",
        "sk_",
        "rk-",
        "pk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "akia",
        "aiza",
        "eyj",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    let Some((name, _)) = lower.split_once(['=', ':']) else {
        return false;
    };
    let normalized = name.replace('-', "_");
    normalized.ends_with("api_key")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
        || normalized.ends_with("passwd")
}

pub(super) fn sanitize_agent_mail_text(raw: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    let mut redact_next_credential = false;
    for token in raw.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let replacement = if redact_next_credential || agent_mail_token_is_credential(token) {
            redact_next_credential = false;
            "[redacted-credential]"
        } else if matches!(lower.as_str(), "bearer" | "basic" | "digest" | "apikey")
            || lower.contains("authorization:")
            || lower.contains("proxy-authorization:")
        {
            redact_next_credential = true;
            "[redacted-credential]"
        } else if token.contains("://") {
            "[redacted-url]"
        } else if token.starts_with('/')
            || token.starts_with("~/")
            || token.contains('\\')
            || token.contains('/')
            || (token.as_bytes().get(1) == Some(&b':')
                && token
                    .as_bytes()
                    .get(2)
                    .is_some_and(|separator| matches!(separator, b'/' | b'\\')))
        {
            "[redacted-path]"
        } else {
            token
        };
        if !out.is_empty() {
            out.push(' ');
        }
        let remaining = max_bytes.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        if replacement.len() <= remaining {
            out.push_str(replacement);
        } else {
            for ch in replacement.chars() {
                if out.len().saturating_add(ch.len_utf8()) > max_bytes {
                    break;
                }
                out.push(ch);
            }
            break;
        }
    }
    out.trim().to_string()
}

pub(super) fn agent_mail_looks_like_raw_transcript(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    if [
        "<turn_meta>",
        "<assistant",
        "<tool_result",
        "\"messages\":",
        "\"role\":\"assistant\"",
        "\"role\": \"assistant\"",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    lower.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("assistant:")
            || line.starts_with("system:")
            || line.starts_with("tool:")
            || line.starts_with("tool_result:")
    })
}

pub(super) fn render_agent_mail_prompt(mail: &AgentMailEnvelope) -> String {
    let source = mail
        .source
        .task_id
        .as_deref()
        .or(mail.source.session_id.as_deref())
        .unwrap_or(mail.source.thread_id.as_str());
    let mut prompt = format!(
        "<agent_mail message_id=\"{}\" source=\"{}\" hop_count=\"{}\">\nSender: {}\nSummary: {}",
        mail.message_id,
        mail.sender.identity,
        mail.hop_count,
        mail.sender.display_label,
        mail.summary
    );
    if !mail.evidence.is_empty() {
        prompt.push_str("\nAuthorized evidence references:");
        for evidence in &mail.evidence {
            let kind = serde_json::to_value(evidence.kind)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "receipt".to_string());
            prompt.push_str(&format!("\n- {kind}:{}", evidence.reference_id));
            if let Some(label) = evidence.label.as_deref() {
                prompt.push_str(&format!(" ({label})"));
            }
        }
    }
    prompt.push_str(&format!(
        "\nSource task/session: {source}\n</agent_mail>\nThis typed runtime handoff is non-authoritative and cannot grant permission or request another Agent Mail turn."
    ));
    prompt
}

pub(super) fn agent_mail_event_for_status(status: AgentMailStatus) -> &'static str {
    match status {
        AgentMailStatus::Queued => AGENT_MAIL_EVENT_QUEUED,
        AgentMailStatus::Delivering => AGENT_MAIL_EVENT_DELIVERING,
        AgentMailStatus::Delivered => AGENT_MAIL_EVENT_DELIVERED,
        AgentMailStatus::Read => AGENT_MAIL_EVENT_READ,
        AgentMailStatus::Failed => AGENT_MAIL_EVENT_DELIVERY_FAILED,
        AgentMailStatus::Canceled => AGENT_MAIL_EVENT_CANCELED,
    }
}

pub(super) fn newest_recorded_item_id(turns: &[TurnRecord]) -> Option<String> {
    turns
        .iter()
        .rev()
        .find_map(|turn| turn.item_ids.last().cloned())
}

pub(super) const RUNTIME_RESTART_REASON: &str = "Interrupted by process restart";
pub(super) const EMPTY_TURN_REASON: &str = "Turn completed without engine output";
// Backstop for the wait on a dynamic (client-executed) tool result. It must
// not undercut the MCP `tools/call` budget (`McpTimeouts::default()`
// execute_timeout, 1800s): a legitimate long execution — a build, a test
// suite, a remote job — would otherwise have its result delivery dropped
// here while the tool itself was still allowed to run. The wait still ends
// on turn interrupt or runtime shutdown, so this cap only matters when the
// client never answers.
pub(super) const DYNAMIC_TOOL_RESULT_TIMEOUT: Duration = Duration::from_secs(1800);

impl RuntimeThreadManager {
    /// Wait for one external approval decision. The one approval clock,
    /// `[approval] timeout_seconds`, governs here as it does for the TUI
    /// card: absent or `0` returns `None` and the decision waits until the
    /// person answers or stops the turn (CURRENT_DECISIONS §21). A GPUI or
    /// web approval is never denied on the user's behalf by default.
    pub fn approval_decision_timeout(&self) -> Option<Duration> {
        #[cfg(any(test, feature = "test-support"))]
        {
            let ms = self
                .test_approval_decision_timeout_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            if ms > 0 {
                return Some(Duration::from_millis(ms));
            }
        }
        self.read_config().approval_timeout()
    }

    pub(super) fn dynamic_tool_result_timeout(&self) -> Duration {
        #[cfg(any(test, feature = "test-support"))]
        {
            let ms = self
                .test_dynamic_tool_result_timeout_ms
                .load(std::sync::atomic::Ordering::SeqCst);
            if ms > 0 {
                return Duration::from_millis(ms);
            }
        }
        DYNAMIC_TOOL_RESULT_TIMEOUT
    }

    /// Test seam: shorten this manager's external approval wait. Scoped to
    /// one manager so parallel libtest cases never see another's value (#6698).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_approval_decision_timeout_ms(&self, ms: u64) {
        self.test_approval_decision_timeout_ms
            .store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test seam: shorten this manager's dynamic tool result wait.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_dynamic_tool_result_timeout_ms(&self, ms: u64) {
        self.test_dynamic_tool_result_timeout_ms
            .store(ms, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `tool_call_after` (and `on_error` for a failed call) on a Runtime API
/// thread, as the TUI fires them (B4). Observer events: their output never
/// changes the result the model sees, and a full observer queue is logged,
/// not fatal to the turn.
pub(super) fn fire_runtime_tool_completion_hooks(
    hooks: &crate::hooks::HookExecutor,
    thread_id: &str,
    turn_id: &str,
    id: &str,
    name: &str,
    input: Option<&serde_json::Value>,
    result: &std::result::Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
) {
    use crate::hooks::HookContext;
    use codewhale_config::hooks::HookEvent;
    let wants_after = hooks.has_hooks_for_event(HookEvent::ToolCallAfter);
    let wants_error = hooks.has_hooks_for_event(HookEvent::OnError);
    if !wants_after && !wants_error {
        return;
    }
    let context = HookContext::new()
        .with_caller(crate::hooks::HookCaller {
            workspace: hooks.default_working_dir().to_path_buf(),
            plugins: crate::extension_host::caller_view(
                hooks.default_working_dir(),
                Some(thread_id),
                None,
            ),
            session_id: Some(thread_id.into()),
            agent_id: None,
            origin_turn_id: Some(turn_id.into()),
            origin_call_id: Some(id.into()),
        })
        .with_workspace(hooks.default_working_dir().to_path_buf())
        .with_session_id(thread_id)
        .with_tool_name(name)
        .with_tool_call_id(id)
        .with_tool_outcome(result);
    let context = input.map_or_else(
        || context.clone(),
        |input| context.clone().with_tool_args(input),
    );
    let failed = context.tool_success == Some(false);
    let error_context = (wants_error && failed).then(|| {
        let text = context.tool_result.as_deref().unwrap_or_default();
        let message = format!("tool `{name}` failed: {text}");
        context.clone().with_error(&message)
    });
    if wants_after && let Err(error) = hooks.submit_observer(HookEvent::ToolCallAfter, context) {
        tracing::warn!(target: "hooks", %error, thread_id, "tool_call_after hook was not submitted");
    }
    if let Some(error_context) = error_context
        && let Err(error) = hooks.submit_observer(HookEvent::OnError, error_context)
    {
        tracing::warn!(target: "hooks", %error, thread_id, "on_error hook was not submitted");
    }
}

/// The prompt text a user-role message contributes to a history comparison,
/// or `None` when it carries none.
///
/// Tool results are user-role messages with no text. The per-turn
/// `<turn_meta>` preamble is rebuilt from runtime facts when a turn is
/// installed and never recorded on the turn's items, so it is not part of the
/// conversation a reconstruction can identify — both sides of every comparison
/// drop it, and `extract_user_prompt` is the repository's one rule for what a
/// user's prompt is once that envelope is removed (it trims the block's edges
/// too, and applies the same way on both sides).
///
/// What is left is the prompt a person typed, which is exactly what a turn
/// record keeps. Everything the engine wraps around it — the metadata block,
/// an attachment's `<image path=…>` tags — and everything the engine injects
/// in its place — compaction checkpoints, runtime-owned control messages —
/// contributes nothing, because no reconstruction can reproduce it.
pub(super) fn projected_user_text(message: &Message) -> Option<String> {
    if message.role.as_str() != "user" {
        return None;
    }
    // An engine-owned compaction checkpoint is not a turn's prompt. No turn
    // record can reproduce one — the compaction ran in the engine, between
    // records — so a document that carries it would read as drifted from the
    // records it was rebuilt from. A fork of a compacted source starts from
    // the settled history that carries the checkpoint, so leaving it in took
    // the cut away from the fork itself: `/undo`, retry and fork-again each
    // refused with "cannot identify an exact saved-history boundary".
    //
    // Runtime-owned control messages are the same case for the same reason:
    // the MCP-server notice, a shell-completion handoff and the deferred-
    // toolbox snapshot are injected by the engine into the model-visible
    // history and are never recorded on a turn, so reading one as a kept
    // prompt made every conversation that had received one uncuttable. They
    // carry the runtime provenance line in their `<turn_meta>` envelope;
    // a person's own prompt does not, which is what keeps this recognition
    // from swallowing a real turn.
    if codewhale_models::turn_classify::is_wire_compaction_checkpoint_message(message)
        || codewhale_models::turn_classify::is_runtime_owned_user_message(message)
    {
        return None;
    }
    let text = message
        .content
        .iter()
        // An attachment's tags are the engine's envelope, not what the user
        // wrote: the record keeps the text block naming `[Attached image: …]`,
        // so counting the tags as prompt text made every prompt carrying a
        // screenshot unmatchable.
        .filter(|block| !crate::image_attach::is_attachment_tag_block(block))
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .map(crate::session_manager::extract_user_prompt)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let text = strip_turn_metadata_envelope(&text);
    (!text.is_empty()).then_some(text.to_string())
}

/// Remove one complete `<turn_meta>…</turn_meta>` envelope from either end.
///
/// The saved document and the record reconstruction reach the same prompt in
/// two shapes — `[<turn_meta>, prompt]` and `[prompt, <turn_meta>]` — and only
/// the leading shape is stripped per block above. Leaving the trailing envelope
/// in made every record-side prompt longer than its transcript twin, so the
/// boundary search that compares the two never matched and refused a restore it
/// could have placed exactly. The envelope is engine metadata, never prompt
/// text, so neither side may carry it into the comparison.
fn strip_turn_metadata_envelope(text: &str) -> &str {
    let text = crate::session_manager::extract_user_prompt(text);
    let trimmed = text.trim_end();
    if !trimmed.ends_with("</turn_meta>") {
        return text;
    }
    match trimmed.rfind("<turn_meta>") {
        Some(open) => text[..open].trim_end(),
        None => text,
    }
}

pub(super) fn projected_user_texts(messages: &[Message]) -> Vec<String> {
    messages.iter().filter_map(projected_user_text).collect()
}

/// The message index in a saved transcript where the undone turn begins.
///
/// A backtrack may only keep the messages that belong to the turns before the
/// undone one, and the transcript alone cannot say where that is: it is the
/// model-visible history, carrying the per-turn `<turn_meta>` preamble and tool
/// results as the route's compaction left them, while the turn records keep the
/// prompt and the raw output. The prompts *are* recorded verbatim, and a turn
/// begins with its user message, so walking the transcript's user text
/// messages — the kept turns' prompts in order, then the undone turn's —
/// locates the boundary exactly.
///
/// `None` refuses: the caller must not cut a history it cannot account for, and
/// a transcript that drifted from the records (edited, purged, or belonging to
/// another conversation) fails the prompt sequence rather than matching by
/// coincidence.
pub(super) fn saved_history_boundary(
    messages: &[Message],
    kept_prompts: &[String],
    target_prompt: &str,
) -> Option<usize> {
    let mut kept = kept_prompts.iter();
    let mut expected = kept.next();
    for (index, message) in messages.iter().enumerate() {
        let Some(text) = projected_user_text(message) else {
            continue;
        };
        match expected {
            Some(next) if next == &text => expected = kept.next(),
            // A kept prompt is still unaccounted for, so this transcript has
            // drifted from the records; even the undone turn's prompt here
            // would cut away a turn the backtrack must keep.
            Some(_) => return None,
            // The first user text after the whole kept prefix is where the
            // undone turn begins — and it has to be that turn's own prompt.
            None => return (text == target_prompt).then_some(index),
        }
    }
    None
}

/// The number of leading messages whose recovery projection is exactly
/// `target`, or `None` when no prefix matches.
///
/// `session_recovery_projection` is a per-message concatenation, so the first
/// exact prefix match is found in one walk: append each message's entries in
/// order, and the moment one is longer than `target` or differs from `target`'s
/// entry at that position, no longer prefix can match either.
///
/// Rebuilding the projection of every prefix instead — what the alignment used
/// to do — is quadratic in the transcript: measured at ~9 s on a 5 MB session,
/// before a fork could do anything else.
pub(super) fn exact_prefix_boundary(messages: &[Message], target: &[Value]) -> Option<usize> {
    if target.is_empty() {
        return Some(0);
    }
    let mut produced = 0usize;
    for (index, message) in messages.iter().enumerate() {
        for entry in session_recovery_projection(std::slice::from_ref(message)) {
            if produced >= target.len() || target[produced] != entry {
                return None;
            }
            produced += 1;
        }
        if produced == target.len() {
            return Some(index + 1);
        }
    }
    None
}

/// The conversation identity two histories are compared by: user prompts (their
/// `<turn_meta>` envelope removed), assistant text, thinking and tool calls,
/// and tool results.
///
/// Everything a turn record cannot reproduce stays out — that preamble, image
/// blocks, and the bytes the route's compaction left in a tool result — so a
/// match identifies a prefix boundary rather than a byte-for-byte replay. The
/// saved messages themselves retain every raw block.
pub(super) fn session_recovery_projection(messages: &[Message]) -> Vec<Value> {
    let mut projection = Vec::new();
    for message in messages {
        let role = message.role.as_str();
        if let Some(text) = projected_user_text(message) {
            projection.push(json!(["user", text]));
        }
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. }
                    if role == "assistant" && !text.trim().is_empty() =>
                {
                    projection.push(json!(["assistant", text]))
                }
                ContentBlock::Thinking { thinking, .. }
                    if role == "assistant" && !thinking.trim().is_empty() =>
                {
                    projection.push(json!(["thinking", thinking]))
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    execution_id,
                    ..
                } if role == "assistant" => {
                    let mut row = json!(["tool_use", id, name, input]);
                    if let Some(id) = execution_id {
                        row.as_array_mut().expect("projection row").push(json!(id));
                    }
                    projection.push(row);
                }
                ContentBlock::ServerToolUse {
                    id, name, input, ..
                } if role == "assistant" => projection.push(json!(["tool_use", id, name, input])),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    content_blocks,
                    execution_id,
                } if role == "user" => {
                    let mut row = json!([
                        "tool_result",
                        tool_use_id,
                        content,
                        is_error.unwrap_or(false),
                        content_blocks
                    ]);
                    if let Some(id) = execution_id {
                        row.as_array_mut().expect("projection row").push(json!(id));
                    }
                    projection.push(row);
                }
                _ => {}
            }
        }
    }
    projection
}

/// Whether a source's saved transcript has stopped being a transcript of its
/// turns.
///
/// A context compaction rewrites the model-visible history in place: what
/// stays verbatim is the user prompts inside a token budget, and everything a
/// prompt stood for — the answers, the tool calls, their results — is
/// summarized away. The saved messages are therefore an *abbreviation* of the
/// turns, and every boundary in them still names a turn while carrying only
/// its prompt.
///
/// That matters to a fork, which is a slice of that history. Copying the
/// abbreviation into a document that claims, through its checkpoint, to cover
/// the kept turns hands every reader of that document — the session view,
/// `restore_thread_messages` — a run of prompts with the agent's
/// output missing, because nothing is left for it to rebuild the turns from.
/// The records can: a compacting turn stores the history its engine settled
/// on, and the fork rebuilds the kept exchanges from the newest one it keeps
/// plus the raw turns after it (`RuntimeThreadStore::history_from_records`).
///
/// Compaction and active-prefix checkpoints name their owning turn. Either
/// requires the canonical record history instead of a saved abbreviation.
fn saved_transcript_requires_record_history(
    turns: &[TurnRecord],
    items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
) -> bool {
    turns.iter().any(|turn| {
        turn.prefix_checkpoint.is_some()
            || items_by_turn.get(&turn.id).is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.kind == TurnItemKind::ContextCompaction)
            })
    })
}

/// Only the admitted start supplies correlation, even when an optional value
/// was absent. Tool output cannot manufacture those missing fields.
pub(super) fn retain_started_tool_metadata(
    output: &mut serde_json::Map<String, Value>,
    started: Option<&Value>,
) {
    for key in [
        "tool_use_id",
        "tool_name",
        "tool_input",
        "visibility",
        "execution_id",
        "acp_result_content",
        "provider_tool_use_id",
        "tool_caller",
        "tool_thought_signature",
    ] {
        output.remove(key);
        if let Some(value) = started.and_then(|meta| meta.get(key)) {
            output.insert(key.to_string(), value.clone());
        }
    }
}

/// The status a turn's terminal outcome gives an item that never reached one
/// of its own.
///
/// The single mapping every repair path uses: the monitor that closes what it
/// holds at the end of a turn, and the projection that closes what an earlier
/// build left behind. A completed turn's leftovers were part of a delivered
/// answer, so they close as `Completed`; an interrupt gives `Interrupted`; a
/// failure gives `Failed`.
fn terminal_item_status(turn_status: RuntimeTurnStatus) -> TurnItemLifecycleStatus {
    match turn_status {
        RuntimeTurnStatus::Completed => TurnItemLifecycleStatus::Completed,
        RuntimeTurnStatus::Interrupted | RuntimeTurnStatus::Canceled => {
            TurnItemLifecycleStatus::Interrupted
        }
        RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress | RuntimeTurnStatus::Failed => {
            TurnItemLifecycleStatus::Failed
        }
    }
}

/// The event that publishes `status` as an item's terminal state.
fn item_terminal_event(status: TurnItemLifecycleStatus) -> &'static str {
    match status {
        TurnItemLifecycleStatus::Interrupted => "item.interrupted",
        TurnItemLifecycleStatus::Canceled => "item.canceled",
        TurnItemLifecycleStatus::Failed => "item.failed",
        TurnItemLifecycleStatus::Queued
        | TurnItemLifecycleStatus::InProgress
        | TurnItemLifecycleStatus::Completed => "item.completed",
    }
}

pub(super) fn thread_execution_state_matches(left: &ThreadRecord, right: &ThreadRecord) -> bool {
    left.schema_version == right.schema_version
        && left.id == right.id
        && left.model == right.model
        && left.model_provider == right.model_provider
        && left.model_provider_id == right.model_provider_id
        && left.reasoning_effort == right.reasoning_effort
        && left.allowed_tools == right.allowed_tools
        && left.workspace == right.workspace
        && left.mode == right.mode
        && left.permission_posture == right.permission_posture
        && left.allow_shell == right.allow_shell
        && left.trust_mode == right.trust_mode
        && left.auto_approve == right.auto_approve
        && left.latest_turn_id == right.latest_turn_id
        && left.latest_response_bookmark == right.latest_response_bookmark
        && left.archived == right.archived
        && left.system_prompt == right.system_prompt
        && left.task_id == right.task_id
        && left.session_id == right.session_id
        && left.saved_session_checkpoint == right.saved_session_checkpoint
}

pub(super) fn checkpoint_tool_declarations(
    messages: &[Message],
) -> Result<HashMap<String, String>> {
    let mut declarations = HashMap::new();
    for block in messages.iter().flat_map(|message| &message.content) {
        if let ContentBlock::ToolUse {
            execution_id, id, ..
        } = block
        {
            declarations.insert(
                serde_json::to_string(&(execution_id, id))?,
                hex_digest(Sha256::digest(serde_json::to_vec(block)?)),
            );
        }
    }
    Ok(declarations)
}

/// The only mutation path for routed provider usage. Every source is recorded
/// once, route labels are sanitized at the boundary, and retained records are
/// bounded regardless of whether they arrived synchronously, by mailbox, or
/// from the fallback journal.
pub(super) fn append_routed_usage_record(
    turn: &mut TurnRecord,
    source_id: &str,
    usage: EffectiveRouteUsage,
) -> bool {
    if usage.usage == Usage::default() {
        return append_routed_usage_drop_record(
            turn,
            RuntimeUsageDropRecord {
                reason: crate::cost_status::RuntimeUsageMissingReason::default(),
                source_id: source_id.to_string(),
                route: usage.route,
            },
        );
    }
    let source_fingerprint = routed_usage_source_fingerprint(source_id);
    if let Some(index) = turn
        .routed_usage_drop_records
        .iter()
        .position(|old| routed_usage_source_fingerprint(&old.source_id) == source_fingerprint)
    {
        let old = &turn.routed_usage_drop_records[index];
        if old.route != usage.route.sanitized_for_persistence() {
            return false;
        }
        turn.routed_usage_drop_records.remove(index);
        turn.routed_usage.push(EffectiveRouteUsage {
            route: usage.route.sanitized_for_persistence(),
            usage: usage.usage,
        });
        return true;
    }
    if turn
        .routed_usage_source_ids
        .iter()
        .any(|persisted| persisted == &source_fingerprint)
    {
        return false;
    }
    if turn.routed_usage_source_ids.len() >= MAX_ROUTED_USAGE_RECORDS_PER_TURN {
        // Stop admitting new records once both the record and exact-dedupe
        // ledgers reach their shared bound. Retaining the first accepted set
        // makes replay idempotent; one explicit incompleteness marker is safer
        // than evicting fingerprints and later counting a replay twice.
        if turn.routed_usage_dropped_records == 0 {
            turn.routed_usage_dropped_records = 1;
            return true;
        }
        return false;
    }
    turn.routed_usage_source_ids.push(source_fingerprint);
    turn.routed_usage.push(EffectiveRouteUsage {
        route: usage.route.sanitized_for_persistence(),
        usage: usage.usage,
    });
    true
}

pub(super) fn append_routed_usage_drop_record(
    turn: &mut TurnRecord,
    record: RuntimeUsageDropRecord,
) -> bool {
    let source_fingerprint = routed_usage_source_fingerprint(&record.source_id);
    if turn
        .routed_usage_source_ids
        .iter()
        .any(|persisted| persisted == &source_fingerprint)
    {
        return false;
    }
    if turn.routed_usage_source_ids.len() >= MAX_ROUTED_USAGE_RECORDS_PER_TURN {
        if turn.routed_usage_dropped_records == 0 {
            turn.routed_usage_dropped_records = 1;
            return true;
        }
        return false;
    }
    turn.routed_usage_source_ids
        .push(source_fingerprint.clone());
    turn.routed_usage_drop_records.push(RuntimeUsageDropRecord {
        reason: record.reason,
        source_id: source_fingerprint,
        route: record.route.sanitized_for_persistence(),
    });
    true
}

pub(super) fn append_decision_receipt(
    turn: &mut TurnRecord,
    receipt: &crate::cost_status::RuntimeDecisionReceipt,
) -> bool {
    if !receipt.is_bounded() {
        return false;
    }
    let receipt = receipt.sanitized();
    if turn
        .decision_receipts
        .iter()
        .any(|old| old.source_id == receipt.source_id)
    {
        return false;
    }
    if turn.decision_receipts.len() >= MAX_ROUTED_USAGE_RECORDS_PER_TURN {
        if turn.routed_usage_dropped_records == 0 {
            turn.routed_usage_dropped_records = 1;
            return true;
        }
        return false;
    }
    turn.decision_receipts.push(receipt);
    true
}

/// Bind pre-parent auxiliary calls to a reserved Runtime turn before the
/// engine accepts the parent operation. Exact records are persisted now so a
/// crash cannot erase them; the engine's terminal event remains the sole
/// owner of `dropped_records`, preventing the same incompleteness count from
/// being added both before and after the turn.
pub(super) fn append_initial_routed_usage_to_turn(
    turn: &mut TurnRecord,
    batch: &crate::cost_status::RuntimeUsageBatch,
) {
    for receipt in &batch.decisions {
        append_decision_receipt(turn, receipt);
    }
    for record in &batch.records {
        append_routed_usage_record(turn, &record.source_id, record.usage.clone());
    }
    for record in &batch.drop_records {
        append_routed_usage_drop_record(turn, record.clone());
    }
}

/// Displayed title of a settlement record. Deliberately fixed text: no turn
/// was accepted, so no prompt content belongs in a record that exists only to
/// keep an already-incurred auxiliary provider call.
pub(super) const UNACCEPTED_TURN_SUMMARY: &str = "Routing failed before the turn started";
pub(super) const UNACCEPTED_TURN_REASON: &str =
    "Turn was not accepted; this record keeps the completed pre-turn provider call";

pub(super) fn routed_usage_batch_is_empty(batch: &crate::cost_status::RuntimeUsageBatch) -> bool {
    batch.decisions.is_empty()
        && batch.records.is_empty()
        && batch.drop_records.is_empty()
        && batch.dropped_records == 0
}

/// Durable identity of the settlement record for one completed pre-turn
/// provider call.
///
/// Derived from the batch's own exact response fingerprints, so settling the
/// same completed call again — a raced operation replay, a retried start after
/// a restart, a reload of the same store — lands on the one record instead of
/// minting a second charge. A batch with no identifiable receipt has no
/// identity to be idempotent on, so it takes a fresh id rather than silently
/// collapsing two distinct coverage gaps into one.
pub(super) fn unaccepted_routed_usage_turn_id(
    thread_id: &str,
    batch: &crate::cost_status::RuntimeUsageBatch,
) -> String {
    let mut identities = batch
        .records
        .iter()
        .map(|record| routed_usage_source_fingerprint(&record.source_id))
        .chain(
            batch
                .drop_records
                .iter()
                .map(|record| routed_usage_source_fingerprint(&record.source_id)),
        )
        .collect::<Vec<_>>();
    identities.extend(
        batch
            .decisions
            .iter()
            .map(|record| routed_usage_source_fingerprint(&record.source_id)),
    );
    if identities.is_empty() {
        return runtime_record_id("turn_unaccepted");
    }
    identities.sort_unstable();
    identities.dedup();
    let digest = routed_usage_source_fingerprint(&format!("{thread_id}:{}", identities.join(":")));
    format!("turn_unaccepted_{}", &digest[..32])
}

/// Settle a completed pre-turn provider call through the same durable receipt
/// authority an accepted turn uses: a Runtime turn record whose bounded
/// `routed_usage` / `routed_usage_drop_records` / `routed_usage_source_ids`
/// ledger is exact-once by source fingerprint.
///
/// The record is terminal on creation and deliberately carries no parent
/// `usage`, no parent route, and no thread-pointer update: the parent turn was
/// never accepted, so only the auxiliary call's own frozen route may be
/// charged. Because no engine `TurnComplete` will ever arrive for it, this
/// record — unlike an accepted turn — also owns the coverage gap its bounded
/// ledger could not represent. Every persisted field is re-derived from the
/// batch, so replaying the same settlement rewrites the same values.
pub(super) fn settle_unaccepted_routed_usage(
    store: &RuntimeThreadStore,
    thread_id: &str,
    batch: &crate::cost_status::RuntimeUsageBatch,
) -> Result<String> {
    let turn_id = unaccepted_routed_usage_turn_id(thread_id, batch);
    let _turn_mutation = store.turn_mutation.lock();
    let mut turn = if store.turn_path(&turn_id)?.exists() {
        let existing = store.load_turn(&turn_id)?;
        if existing.thread_id != thread_id {
            bail!("settlement turn {turn_id} already belongs to another thread");
        }
        existing
    } else {
        let now = Utc::now();
        TurnRecord {
            prefix_checkpoint: None,
            decision_receipts: Vec::new(),
            max_output_tokens: None,
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: turn_id.clone(),
            thread_id: thread_id.to_string(),
            status: RuntimeTurnStatus::Failed,
            input_summary: UNACCEPTED_TURN_SUMMARY.to_string(),
            created_at: now,
            started_at: Some(now),
            ended_at: Some(now),
            duration_ms: Some(0),
            usage: None,
            routing_settlement: true,
            effective_route_usage: None,
            permission_posture: None,
            // No turn ran: this settles a turn that was never accepted.
            mode: None,
            effective_provider: None,
            effective_provider_id: None,
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: None,
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            model_request_diagnostics: None,
            operation_activity: None,
            error: Some(UNACCEPTED_TURN_REASON.to_string()),
            item_ids: Vec::new(),
            steer_count: 0,
            agent_mail_message_id: None,
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        }
    };
    append_initial_routed_usage_to_turn(&mut turn, batch);
    let reported = batch.records.len().saturating_add(batch.drop_records.len());
    let retained = turn
        .routed_usage
        .len()
        .saturating_add(turn.routed_usage_drop_records.len());
    let unretained = u64::try_from(reported.saturating_sub(retained)).unwrap_or(u64::MAX);
    let residual = batch
        .dropped_records
        .saturating_sub(u64::try_from(batch.drop_records.len()).unwrap_or(u64::MAX));
    turn.routed_usage_dropped_records = residual.saturating_add(unretained);
    store.save_turn(&turn)?;
    Ok(turn_id)
}

pub(super) struct InitialRoutedUsageSettlementGuard {
    /// The Runtime store that owns the thread whose `start_turn` incurred the
    /// call. It is the durability authority for runtime accounting, exactly as
    /// it is for an accepted turn's routed usage. Ownerless in-process
    /// accounting is only the last resort below: a headless Runtime has no
    /// foreground session to drain it, and a closed cost scope (`/new`,
    /// session load) rejects a stale report outright.
    pub(super) store: RuntimeThreadStore,
    pub(super) thread_id: String,
    pub(super) cost_scope: crate::cost_status::CostScopeToken,
    pub(super) batch: crate::cost_status::RuntimeUsageBatch,
    pub(super) armed: bool,
}

impl InitialRoutedUsageSettlementGuard {
    pub(super) fn new(
        store: RuntimeThreadStore,
        thread_id: &str,
        cost_scope: crate::cost_status::CostScopeToken,
        batch: &crate::cost_status::RuntimeUsageBatch,
    ) -> Self {
        Self {
            store,
            thread_id: thread_id.to_string(),
            cost_scope,
            batch: batch.clone(),
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InitialRoutedUsageSettlementGuard {
    fn drop(&mut self) {
        if !self.armed || routed_usage_batch_is_empty(&self.batch) {
            return;
        }
        match settle_unaccepted_routed_usage(&self.store, &self.thread_id, &self.batch) {
            Ok(turn_id) => tracing::debug!(
                thread_id = %self.thread_id,
                turn_id = %turn_id,
                "Settled a completed pre-turn provider call into the Runtime store"
            ),
            Err(error) => {
                // The durable authority is unreachable. In-process accounting
                // keeps the receipt for whatever is still draining this scope
                // rather than discarding it, but it does not survive the
                // process, so say so once at warn level.
                tracing::warn!(
                    thread_id = %self.thread_id,
                    error = %error,
                    "Failed to persist a completed pre-turn provider call; \
                     falling back to in-process accounting"
                );
                crate::cost_status::report_runtime_usage_batch(self.cost_scope, None, &self.batch);
            }
        }
    }
}

pub(super) fn event_append_is_indeterminate(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<RuntimeEventAppendError>()
            .is_some_and(|append| !append.retry_safe())
    })
}

#[derive(Debug, Clone)]
pub struct RuntimeThreadManagerConfig {
    pub data_dir: PathBuf,
    pub task_data_dir: PathBuf,
    /// Selected document store, captured once before owner recovery or hydration.
    pub sessions_dir: Option<PathBuf>,
    pub max_active_threads: usize,
}

impl RuntimeThreadManagerConfig {
    #[must_use]
    pub fn from_task_data_dir(task_data_dir: PathBuf) -> Self {
        Self::resolved(task_data_dir, None)
    }

    /// Scope the Runtime thread store to one interactive session.
    ///
    /// The process-owner lock stays exclusive; isolation comes from the path,
    /// not from weakening the lock (#5630).
    #[must_use]
    pub fn for_session(task_data_dir: PathBuf, session_id: &str) -> Self {
        Self::resolved(task_data_dir, Some(session_id))
    }

    pub(super) fn resolved(task_data_dir: PathBuf, session_id: Option<&str>) -> Self {
        let data_dir = runtime_dir_override()
            .unwrap_or_else(|| default_runtime_store_root(&task_data_dir, session_id));
        Self {
            data_dir,
            task_data_dir,
            sessions_dir: None,
            max_active_threads: MAX_ACTIVE_THREADS_DEFAULT,
        }
    }
}

fn default_runtime_store_root(task_data_dir: &Path, session_id: Option<&str>) -> PathBuf {
    match session_id
        .map(str::trim)
        .filter(|id| is_runtime_session_scope(id))
    {
        Some(id) => match crate::session_manager::default_sessions_dir() {
            Ok(sessions_dir) => sessions_dir.join(id).join("runtime"),
            Err(_) => task_data_dir.join("runtime").join(id),
        },
        None => task_data_dir.join("runtime"),
    }
}

pub(super) fn is_runtime_session_scope(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && id != "checkpoints"
        && id != "session_boot_owners"
}

/// Visibility filter for `list_threads`. Default is `ActiveOnly`. The runtime
/// API exposes this as the combination of `include_archived` and
/// `archived_only` query params (see `runtime_api.rs`); whalescale#260 / #563.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadListFilter {
    /// Only `archived = false` threads. The original default.
    #[default]
    ActiveOnly,
    /// Active and archived threads, sorted as the store returns them.
    IncludeArchived,
    /// Only `archived = true` threads.
    ArchivedOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateThreadRequest {
    #[serde(default, alias = "operationKey")]
    pub operation_key: Option<String>,
    pub model: Option<String>,
    /// Generic provider kind or, for legacy clients, an exact provider id.
    #[serde(default)]
    pub model_provider: Option<String>,
    /// Exact configured provider key. Takes precedence over `model_provider`.
    #[serde(default)]
    pub model_provider_id: Option<String>,
    /// Default reasoning preference for turns in this thread.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Default model-visible tool allowlist for turns in this thread.
    /// An empty array intentionally disables every model-visible tool.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    pub workspace: Option<PathBuf>,
    pub mode: Option<String>,
    #[serde(default)]
    pub permission_posture: Option<String>,
    pub allow_shell: Option<bool>,
    pub trust_mode: Option<bool>,
    pub auto_approve: Option<bool>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub dynamic_tools: Vec<DynamicToolSpec>,
    #[serde(default)]
    pub environments: Vec<TurnEnvironmentParams>,
    #[serde(default, alias = "parentThreadId")]
    pub parent_thread_id: Option<String>,
}

/// Mutable fields accepted by `PATCH /v1/threads/{id}`.
///
/// Each field is optional — missing means "no change". Extended in v0.8.10
/// (#562, whalescale#256) so the UI can flip persistent thread state without
/// having to recreate a thread or pass per-turn overrides on every send.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateThreadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_idle: Option<bool>,
    pub archived: Option<bool>,
    pub allow_shell: Option<bool>,
    pub trust_mode: Option<bool>,
    pub auto_approve: Option<bool>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub permission_posture: Option<String>,
    pub title: Option<String>,
    pub system_prompt: Option<String>,
    pub workspace: Option<PathBuf>,
    /// Switch the provider this thread's future turns route through: a
    /// built-in kind (`deepseek`, `xai`, ...) or a configured route name, as
    /// `/provider` accepts. Validated against the live config (the route must
    /// resolve and its credentials must be usable) before it is saved. Without
    /// `model`, the thread takes that provider's default model; `auto` stays
    /// `auto`. Conversation history is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Exact configured provider id (`[providers.<id>]`), as `POST /v1/threads`
    /// and `POST /v1/providers/{id}/switch` accept it. Takes precedence over a
    /// route name in `model_provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCommandRequest {
    pub name: String,
    pub args: String,
    pub registration_id: String,
    pub operation_key: String,
    pub expected_workspace: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCommandResult {
    #[serde(default)]
    pub operation_key: String,
    pub name: String,
    pub origin: String,
    pub status: String,
    pub text: String,
    pub extension: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadRecord>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCommandReceipt {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub name: String,
    pub origin: String,
    pub status: String,
    pub text: String,
    pub extension: Value,
}

pub(super) struct RuntimeCommandGuard {
    pub(super) pending: Arc<parking_lot::Mutex<HashMap<(String, String), CancellationToken>>>,
    pub(super) key: (String, String),
}

impl Drop for RuntimeCommandGuard {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.key);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartReviewRequest {
    pub review: codewhale_protocol::op::ReviewOperation,
    pub operation_key: String,
    pub account_model_owner: Option<String>,
    pub model: String,
    pub model_provider: Option<String>,
    pub model_provider_id: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(rename = "maxOutputTokens", alias = "max_output_tokens")]
    pub max_output_tokens: std::num::NonZeroU32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StartTurnRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_model_owner: Option<String>,
    /// Account-authorized data, rendered only by the Engine for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_constitution:
        Option<codewhale_config::user_constitution::ProfileConstitutionSnapshot>,
    /// Narrowing assertion captured by an acknowledged selected frontend.
    /// A mismatch refuses; this field never changes a thread's workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_workspace: Option<PathBuf>,
    /// Per-primary-request allowance, including reasoning where the provider counts it.
    #[serde(
        default,
        rename = "maxOutputTokens",
        alias = "max_output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
    /// Optional caller-supplied idempotency key, scoped to this Runtime store
    /// and thread. The raw key is validated but never persisted.
    #[serde(default, alias = "operationKey")]
    pub operation_key: Option<String>,
    #[serde(default)]
    pub input_summary: Option<String>,
    pub model: Option<String>,
    /// Per-turn reasoning override. Missing inherits the thread, then config.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Per-turn model-visible tool override. Missing inherits the thread;
    /// an empty array intentionally disables every model-visible tool.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    pub mode: Option<String>,
    #[serde(default)]
    pub permission_posture: Option<String>,
    pub allow_shell: Option<bool>,
    pub trust_mode: Option<bool>,
    pub auto_approve: Option<bool>,
    #[serde(default)]
    pub dynamic_tools: Vec<DynamicToolSpec>,
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Route this turn only through another provider (same grammar as
    /// `UpdateThreadRequest::model_provider`). The thread's saved provider is
    /// unchanged. Without `model`, the turn uses that provider's default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Exact configured provider id for this turn only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
}

/// Resolve a caller-selected provider the way `/provider` and
/// `POST /v1/providers/{id}/switch` do. An exact configured id names one
/// `[providers.<id>]` route; otherwise `model_provider` is a provider pin: a
/// built-in kind or a configured route name. `Ok(None)` when neither is set.
pub(super) fn requested_provider_identity(
    config: &Config,
    model_provider: Option<&str>,
    model_provider_id: Option<&str>,
) -> Result<Option<ProviderIdentity>> {
    if model_provider.is_some_and(|value| value.trim().is_empty()) {
        bail!("model_provider must not be empty");
    }
    if model_provider_id.is_some_and(|value| value.trim().is_empty()) {
        bail!("model_provider_id must not be empty");
    }
    let kind = model_provider.map(str::trim);
    let identity = match (kind, model_provider_id.map(str::trim)) {
        (None, None) => return Ok(None),
        (kind, Some(exact_id)) => config.resolve_persisted_provider_identity(kind, Some(exact_id)),
        (Some(kind), None) => config.resolve_provider_pin_identity(kind),
    };
    identity.map(Some).map_err(|reason| anyhow!(reason))
}

/// Resolve and client-preflight `identity` for `model` (`None` or `auto`
/// selects the provider's default route). A provider whose route does not
/// resolve, or whose credentials cannot build a client, is not ready.
pub(super) fn ready_provider_route(
    config: &Config,
    identity: &ProviderIdentity,
    model: Option<&str>,
) -> Result<crate::route_runtime::ResolvedRuntimeRoute> {
    let model = model.filter(|model| !model.trim().eq_ignore_ascii_case("auto"));
    resolve_runtime_thread_route_for_identity(config, identity, model)?
        .preflight()
        .map_err(|reason| anyhow!("provider '{}' is not ready: {reason}", identity.key))
}

pub(super) fn parse_runtime_reasoning_effort(
    value: &str,
) -> Result<crate::reasoning_preference::ReasoningEffort> {
    crate::reasoning_preference::ReasoningEffort::parse_strict(value).map_err(anyhow::Error::msg)
}

pub(super) fn canonical_runtime_reasoning_effort(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(parse_runtime_reasoning_effort)
        .transpose()
        .map(|effort| effort.map(|effort| effort.as_setting().to_string()))
}

#[derive(Debug, thiserror::Error)]
#[error("operation_key already binds a different thread creation request")]
pub struct RuntimeThreadCreateConflict;

#[derive(Debug, Clone)]
pub(super) struct PreparedRuntimeTurnOperation {
    pub(super) binding: RuntimeTurnOperationBinding,
    pub(super) requested_turn_id: Option<String>,
}

/// Lookup errors deliberately omit operation keys and persisted file paths.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeTurnOperationLookupError {
    #[error("Invalid thread id or operation key")]
    InvalidRequest,
    #[error("Turn operation acceptance is incomplete; retry lookup")]
    Incomplete,
    #[error("Turn operation lookup unavailable")]
    Unavailable,
}

pub(super) fn runtime_turn_operation_key_fingerprint(
    owner_id: &str,
    thread_id: &str,
    operation_key: &str,
) -> Result<String> {
    validate_runtime_turn_operation_key(operation_key)?;
    Ok(codewhale_runtime::hashing::sha256_hex(format!(
        "runtime-turn-operation\u{1f}{owner_id}\u{1f}{thread_id}\u{1f}{operation_key}"
    )))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn runtime_turn_request_fingerprint(
    thread: &ThreadRecord,
    prompt: &str,
    input_summary: Option<&str>,
    requested_model: &str,
    reasoning_effort: Option<crate::reasoning_preference::ReasoningEffort>,
    allowed_tools: Option<&[String]>,
    policy: RuntimePolicyProjection,
    allow_shell: bool,
    trust_mode: bool,
    dynamic_tools: &[DynamicToolSpec],
    environment_id: Option<&str>,
    images: &[codewhale_protocol::runtime::RuntimeImageInput],
    max_output_tokens: Option<std::num::NonZeroU32>,
) -> Result<String> {
    let mut payload = json!({
        "version": 1,
        "thread_id": thread.id,
        "provider": thread.model_provider,
        "provider_id": thread.model_provider_id,
        "model": requested_model,
        "prompt": prompt,
        "input_summary": input_summary,
        "reasoning_effort": reasoning_effort.map(|effort| effort.as_setting()),
        "allowed_tools": allowed_tools,
        "mode": policy.mode_setting(),
        "permission_posture": policy.permission_wire(),
        "allow_shell": allow_shell,
        "trust_mode": trust_mode,
        "auto_approve": policy.auto_approve(),
        "dynamic_tools": dynamic_tools,
        "environment_id": environment_id,
        "workspace": thread.workspace,
        "system_prompt": thread.system_prompt,
    });
    // Preserve the exact historical text-only fingerprint payload.
    if !images.is_empty() {
        payload["images"] = json!(images);
    }
    if let Some(max_output_tokens) = max_output_tokens {
        payload["maxOutputTokens"] = json!(max_output_tokens);
    }
    Ok(codewhale_runtime::hashing::sha256_hex(
        codewhale_runtime::hashing::canonical_json(&payload),
    ))
}

#[derive(Debug, Clone)]
pub(super) enum RuntimeTurnInputSource {
    ExternalUser,
    Review(codewhale_protocol::op::ReviewOperation),
    ExtensionCommand {
        reference: crate::extension_host::command::ExtensionCommandRef,
        operation_fingerprint: String,
        cancel: CancellationToken,
    },
    AgentMail {
        message_id: String,
        persisted_summary: String,
    },
    /// A host-driven goal pass (kickoff or continuation). The runtime host,
    /// not the engine, owns the durable goal loop: host-managed engines
    /// never self-continue, so each pass is claimed here with the same
    /// durable-turn machinery as any other input.
    GoalContinuation {
        continuation_index: u32,
        expected_goal: Box<codewhale_protocol::ThreadGoal>,
    },
    Scheduled {
        task_id: String,
    },
}

impl RuntimeTurnInputSource {
    pub(super) fn provenance(&self) -> crate::core::ops::UserInputProvenance {
        match self {
            Self::ExternalUser | Self::Review(_) => {
                crate::core::ops::UserInputProvenance::ExternalUser
            }
            Self::ExtensionCommand { .. } => crate::core::ops::UserInputProvenance::Runtime,
            Self::AgentMail { .. } => crate::core::ops::UserInputProvenance::AgentMail,
            Self::GoalContinuation {
                continuation_index, ..
            } if *continuation_index > 0 => crate::core::ops::UserInputProvenance::GoalContinuation,
            Self::GoalContinuation { .. } | Self::Scheduled { .. } => {
                crate::core::ops::UserInputProvenance::Runtime
            }
        }
    }

    pub(super) fn mail_message_id(&self) -> Option<&str> {
        match self {
            Self::ExternalUser | Self::Review(_) | Self::ExtensionCommand { .. } => None,
            Self::AgentMail { message_id, .. } => Some(message_id),
            Self::GoalContinuation { .. } | Self::Scheduled { .. } => None,
        }
    }

    pub(super) fn item_detail(&self, prompt: &str) -> Option<String> {
        match self {
            Self::ExternalUser
            | Self::Review(_)
            | Self::ExtensionCommand { .. }
            | Self::Scheduled { .. } => Some(prompt.to_string()),
            // The provider projection contains runtime-only framing. Persist
            // the bounded canonical mail summary instead, so history and app
            // clients never mistake that projection for typed user input.
            Self::AgentMail {
                persisted_summary, ..
            } => Some(persisted_summary.clone()),
            // Continuation prompts embed goal JSON and engine framing, so the
            // same rule applies: persist a bounded marker, not the projection.
            // Index 0 is the kickoff pass, not a continuation; the summary
            // must not call it one.
            Self::GoalContinuation {
                continuation_index: 0,
                ..
            } => Some("Goal kickoff (host-driven)".to_string()),
            Self::GoalContinuation {
                continuation_index, ..
            } => Some(format!(
                "Goal continuation pass #{continuation_index} (host-driven)"
            )),
        }
    }

    pub(super) fn item_metadata(&self) -> Option<Value> {
        match self {
            Self::ExternalUser => None,
            Self::ExtensionCommand {
                reference,
                operation_fingerprint,
                ..
            } => Some(json!({
                "input_provenance": "extension_command",
                "extension": reference.receipt(),
                "command_operation": operation_fingerprint,
            })),
            Self::Review(request) => Some(json!({
                "input_provenance": "structured_review",
                "review_request": request,
                "review_system_prompt": crate::tools::review_core::review_system_prompt(),
            })),
            Self::AgentMail { message_id, .. } => Some(json!({
                "input_provenance": "agent_mail",
                "agent_mail_message_id": message_id,
            })),
            Self::GoalContinuation {
                continuation_index, ..
            } => Some(json!({
                "input_provenance": "goal_continuation",
                "goal_continuation_index": continuation_index,
            })),
            Self::Scheduled { task_id } => Some(json!({
                "input_provenance": "scheduled",
                "task_id": task_id,
            })),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerTurnRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompactThreadRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub thread: ThreadRecord,
    pub turns: Vec<TurnRecord>,
    pub items: Vec<TurnItemRecord>,
    #[serde(default)]
    pub command_receipts: Vec<RuntimeCommandReceipt>,
    pub latest_seq: u64,
    /// Approval prompts that are still waiting for a decision. These are part
    /// of the canonical snapshot so clients can recover attention UI after a
    /// tab reload without replaying events older than `latest_seq`.
    #[serde(default)]
    pub pending_approvals: Vec<PendingApprovalRequest>,
    /// User-input prompts that are still waiting for answers. As with
    /// approvals, the snapshot is authoritative across client reconnects.
    #[serde(default)]
    pub pending_user_inputs: Vec<PendingUserInputRequest>,
    /// Client-executed dynamic tool calls that are still waiting for a result.
    /// Keeping the typed request in the canonical snapshot lets an external
    /// Runtime client reload from `latest_seq` without stranding a call whose
    /// `tool_call.requested` event is already behind that cursor.
    #[serde(default)]
    pub pending_dynamic_tool_calls: Vec<DynamicToolCallParams>,
    /// Live session approval grants on this thread (see
    /// [`RuntimeApprovalGrant`]); each can be revoked by `grant_id`.
    #[serde(default)]
    pub approval_grants: Vec<RuntimeApprovalGrant>,
}

pub(super) fn deserialize_present_approval_input<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApprovalRequest {
    /// Runtime-owned, single-use approval ID. Clients echo this value; it is
    /// independent of the provider's tool-call ID.
    pub id: String,
    pub turn_id: String,
    pub tool_name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_approval_input"
    )]
    pub input: Option<Value>,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_summary: Option<String>,
    /// The Engine tool-execution ID this approval gates, mirroring the
    /// `tool_call_id` on `approval.required`. A client resuming from a
    /// snapshot needs it to attach the prompt to the execution it belongs to.
    /// It is a correlator, never a capability: `deliver_external_approval`
    /// matches `id` only, so this value settles nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Model-independent one-line summary of the gated call ("Search the web
    /// for '…'"), with workspace-relative paths. Clients show it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// A session approval grant: "allow for this conversation" on one call.
///
/// The grant covers later calls of the same tool and argument class (the
/// approval grouping key) on this thread, for the life of this Runtime
/// process or until the thread is archived or deleted. It never changes the
/// thread's permission posture, and it can be revoked. Grants persist in the
/// Engine grant store; after a Runtime restart they stay suspended until the
/// thread is resumed by an authenticated client, and the next matching call
/// prompts again until then (fail closed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeApprovalGrant {
    /// Runtime-minted `grt_<32 hex>`; the revoke endpoint accepts only this.
    pub grant_id: String,
    pub tool_name: String,
    /// The approval grouping key the grant matches (tool + argument class).
    pub scope: String,
    /// The summary of the call the person approved.
    pub summary: String,
    pub granted_at: DateTime<Utc>,
}

impl RuntimeApprovalGrant {
    pub(super) fn from_grant(grant: &crate::core::authority::grants::Grant) -> Self {
        Self {
            grant_id: grant.grant_id.clone(),
            tool_name: grant.display.tool_name.clone(),
            scope: grant.matcher.grouping_key.clone().unwrap_or_default(),
            summary: grant.display.redacted_summary.clone(),
            granted_at: grant.created.at,
        }
    }
}

pub(super) fn grant_tool_class(tool_name: &str) -> String {
    format!("{:?}", crate::core::authority::get_tool_category(tool_name)).to_lowercase()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUserInputRequest {
    pub id: String,
    pub turn_id: String,
    pub request: crate::tools::user_input::UserInputRequest,
}

/// Aggregation key for `aggregate_usage`. Whalescale#261 / #564.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGroupBy {
    Day,
    Model,
    Provider,
    Thread,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub reasoning_replay_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    /// Provider-published CNY subtotal, accrued only from turns whose route
    /// published an authoritative CNY row (e.g. DeepSeek Platform). Never an
    /// FX projection of `cost_usd`: USD-only routes contribute 0 here rather
    /// than a fabricated amount, mirroring `SessionCostSnapshot`.
    pub cost_cny: f64,
    /// Authoritative USD coverage for this aggregate. `cost_usd` is a priced
    /// subtotal whenever `unpriced_turns > 0`.
    pub priced_turns: u64,
    pub unpriced_turns: u64,
    /// CNY-specific coverage over the same money-metered turns: a USD-only
    /// route is CNY-unpriced rather than a fabricated complete zero, same
    /// rule as `SessionCostSnapshot::cny_priced_turns`.
    pub cny_priced_turns: u64,
    pub cny_unpriced_turns: u64,
    /// Why CNY is missing on money-metered turns. USD-only routes record
    /// `currency_not_published` rather than a fabricated complete zero.
    pub cny_unpriced_reasons: std::collections::BTreeSet<String>,
    pub nonmetered_turns: u64,
    pub cost_complete: bool,
    pub unpriced_reasons: std::collections::BTreeSet<String>,
    pub unpriced_classes: std::collections::BTreeSet<String>,
    pub pricing_provenances: std::collections::BTreeSet<String>,
    pub live_pricing_defects: std::collections::BTreeSet<String>,
    pub live_pricing_unusable_defects: std::collections::BTreeSet<String>,
    pub route_receipts: std::collections::BTreeSet<String>,
    /// Provider-call receipts lost from a bounded fallback journal. A non-zero
    /// value always makes `cost_complete` false.
    pub dropped_usage_records: u64,
    /// Number of provider-call usage records (parent turns plus child and
    /// compaction calls), including zero-token audited calls.
    pub turns: u64,
}

/// One in-flight or queued turn, for running-work accounting. Served by
/// `GET /v1/threads/running` so background-capable clients (quit,
/// backgrounding) can enumerate owned work without inferring it from
/// per-thread latest-turn status (#6180, codewhale-apps#573).
#[derive(Debug, Clone, Serialize)]
pub struct ActiveTurn {
    pub turn_id: String,
    pub status: RuntimeTurnStatus,
}

/// One thread with at least one [`ActiveTurn`]. Archive state is ignored:
/// archiving is a plain flag with no quiescence gate, so an archived thread
/// can still carry live work and quit-accounting must count it.
#[derive(Debug, Clone, Serialize)]
pub struct RunningThread {
    pub thread_id: String,
    pub model: String,
    pub title: Option<String>,
    pub active_turns: Vec<ActiveTurn>,
}

/// The per-row facts `GET /v1/threads/summary` cannot read off a
/// [`ThreadRecord`], harvested for a whole page in one pass over the store.
///
/// Everything else a row needs (`title`, `model`, `mode`, `workspace`,
/// `archived`, `updated_at`, `latest_turn_id`) already comes from the thread
/// record the caller holds.
#[derive(Debug, Clone)]
pub struct ThreadListFacts {
    /// Status of the thread's newest turn, `Debug`-lowercased for the wire.
    pub latest_turn_status: Option<String>,
    /// Newest turn's `input_summary`, verbatim. The row uses it only as the
    /// title fallback for a thread that has no title of its own.
    pub latest_turn_input_summary: Option<String>,
    /// Text of the newest agent/user message in the newest turn that has one,
    /// untruncated. `None` when the thread has no such message.
    pub preview: Option<String>,
    /// Pending approvals plus pending user-input requests, from live request
    /// state rather than the store.
    pub pending_attention_count: usize,
    pub newest_item_id: Option<String>,
}

/// One watchable notice on a thread, projected from engine events so
/// watch-only clients see what the TUI shows (#6180, codewhale-apps#573).
/// `kind` is a [`codewhale_config::notifications::NotificationEvent`] name
/// (`subagent-terminal`, `elevation-needed`, `model-notify`); `subject` is
/// the agent, tool-call, or tool id the notice is about, for targeted
/// clearing. Notices are in-memory session state, bounded per thread, and
/// never persisted: terminal/notify kinds clear on client ack, elevation
/// clears when its tool call completes.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveNotice {
    pub id: String,
    pub kind: String,
    pub turn_id: String,
    pub subject: String,
    pub detail: String,
    pub raised_at: DateTime<Utc>,
}

/// Per-thread notice bound. Oldest-first eviction keeps a chatty child from
/// growing a watch-only client's banner list without bound.
pub const MAX_NOTICES_PER_THREAD: usize = 32;

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageBucket {
    pub key: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub reasoning_replay_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    /// Provider-published CNY subtotal; same coverage rule as the totals
    /// field of the same name.
    pub cost_cny: f64,
    pub priced_turns: u64,
    pub unpriced_turns: u64,
    /// CNY-specific coverage; same rule as the totals field of the same name.
    pub cny_priced_turns: u64,
    pub cny_unpriced_turns: u64,
    /// Why CNY is missing; same rule as the totals field of the same name.
    pub cny_unpriced_reasons: std::collections::BTreeSet<String>,
    pub nonmetered_turns: u64,
    pub cost_complete: bool,
    pub unpriced_reasons: std::collections::BTreeSet<String>,
    pub unpriced_classes: std::collections::BTreeSet<String>,
    pub pricing_provenances: std::collections::BTreeSet<String>,
    pub live_pricing_defects: std::collections::BTreeSet<String>,
    pub live_pricing_unusable_defects: std::collections::BTreeSet<String>,
    pub route_receipts: std::collections::BTreeSet<String>,
    pub dropped_usage_records: u64,
    /// Provider-call usage records contributing to this bucket.
    pub turns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageAggregation {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub group_by: String,
    pub totals: UsageTotals,
    pub buckets: Vec<UsageBucket>,
}

/// Thread-scoped usage split by spend owner, for session persistence.
///
/// The split mirrors `SessionCostSnapshot`'s field semantics
/// (`session_cost_*` carries parent-turn spend, `subagent_cost_*` routed
/// child spend) so a writer can persist each side into the field readers
/// already project into one display total.
#[derive(Debug, Clone, Default)]
pub struct ThreadUsageSplit {
    /// Parent-turn usage: the session's own provider calls.
    pub parent: UsageTotals,
    /// Routed child (sub-agent/background) usage recorded on those turns,
    /// including dropped-record incompleteness markers.
    pub routed_children: UsageTotals,
}

impl ThreadUsageSplit {
    /// Whole-history combined totals — the figure the global `/v1/usage`
    /// thread bucket reports and the per-thread endpoint returns.
    #[must_use]
    pub fn combined(&self) -> UsageTotals {
        let mut combined = self.parent.clone();
        merge_usage_totals(&mut combined, &self.routed_children);
        finalize_usage_totals(&mut combined);
        combined
    }
}

/// Recompute the derived completeness flag after accumulation.
pub(super) fn finalize_usage_totals(totals: &mut UsageTotals) {
    // Dropped fallback receipts also bump `unpriced_turns`, so one check
    // covers both incompleteness markers.
    totals.cost_complete = totals.unpriced_turns == 0;
}

/// Component-wise saturating merge of usage totals; used to rebuild the
/// combined thread figure from the parent/child split.
pub(super) fn merge_usage_totals(into: &mut UsageTotals, from: &UsageTotals) {
    into.input_tokens = into.input_tokens.saturating_add(from.input_tokens);
    into.output_tokens = into.output_tokens.saturating_add(from.output_tokens);
    into.cached_tokens = into.cached_tokens.saturating_add(from.cached_tokens);
    into.reasoning_tokens = into.reasoning_tokens.saturating_add(from.reasoning_tokens);
    into.reasoning_replay_tokens = into
        .reasoning_replay_tokens
        .saturating_add(from.reasoning_replay_tokens);
    into.cache_write_tokens = into
        .cache_write_tokens
        .saturating_add(from.cache_write_tokens);
    saturating_add_cost_amount(&mut into.cost_usd, from.cost_usd);
    saturating_add_cost_amount(&mut into.cost_cny, from.cost_cny);
    into.priced_turns = into.priced_turns.saturating_add(from.priced_turns);
    into.unpriced_turns = into.unpriced_turns.saturating_add(from.unpriced_turns);
    into.cny_priced_turns = into.cny_priced_turns.saturating_add(from.cny_priced_turns);
    into.cny_unpriced_turns = into
        .cny_unpriced_turns
        .saturating_add(from.cny_unpriced_turns);
    into.cny_unpriced_reasons
        .extend(from.cny_unpriced_reasons.clone());
    into.nonmetered_turns = into.nonmetered_turns.saturating_add(from.nonmetered_turns);
    into.unpriced_reasons.extend(from.unpriced_reasons.clone());
    into.unpriced_classes.extend(from.unpriced_classes.clone());
    into.pricing_provenances
        .extend(from.pricing_provenances.clone());
    into.live_pricing_defects
        .extend(from.live_pricing_defects.clone());
    into.live_pricing_unusable_defects
        .extend(from.live_pricing_unusable_defects.clone());
    into.route_receipts.extend(from.route_receipts.clone());
    into.dropped_usage_records = into
        .dropped_usage_records
        .saturating_add(from.dropped_usage_records);
    into.turns = into.turns.saturating_add(from.turns);
}

#[allow(clippy::too_many_arguments)] // pre-existing baseline signature; FEAT-022 gate repair
pub(super) fn accumulate_runtime_cost_coverage(
    audit: Option<&crate::pricing::TurnCostAudit>,
    priced_turns: &mut u64,
    unpriced_turns: &mut u64,
    cny_priced_turns: &mut u64,
    cny_unpriced_turns: &mut u64,
    nonmetered_turns: &mut u64,
    reasons: &mut std::collections::BTreeSet<String>,
    cny_reasons: &mut std::collections::BTreeSet<String>,
    provenances: &mut std::collections::BTreeSet<String>,
) {
    let Some(audit) = audit else {
        *unpriced_turns = (*unpriced_turns).saturating_add(1);
        *cny_unpriced_turns = (*cny_unpriced_turns).saturating_add(1);
        reasons.insert("unknown_provider_route".to_string());
        cny_reasons.insert("unknown_provider_route".to_string());
        return;
    };
    if let Some(provenance) = audit.provenance.as_ref() {
        provenances.insert(provenance.label().to_string());
    }
    if !audit.counts_toward_money_coverage() {
        *nonmetered_turns = (*nonmetered_turns).saturating_add(1);
        return;
    }
    if audit.usd_priced {
        *priced_turns = (*priced_turns).saturating_add(1);
    } else {
        *unpriced_turns = (*unpriced_turns).saturating_add(1);
        if let Some(reason) = audit.unpriced_reason {
            reasons.insert(reason.label().to_string());
        }
    }
    // CNY coverage counts the same money-metered turns under the provider's
    // own CNY row: a USD-only route stays CNY-unpriced instead of reading as
    // a complete zero (mirrors `cost_status`'s session-side accounting).
    if audit.cny_priced {
        *cny_priced_turns = (*cny_priced_turns).saturating_add(1);
    } else {
        *cny_unpriced_turns = (*cny_unpriced_turns).saturating_add(1);
        cny_reasons.insert(
            audit
                .unpriced_reason
                .map_or("currency_not_published", |reason| reason.label())
                .to_string(),
        );
    }
}

pub(super) fn accumulate_runtime_cost_details(
    audit: Option<&crate::pricing::TurnCostAudit>,
    unpriced_classes: &mut std::collections::BTreeSet<String>,
    live_pricing_defects: &mut std::collections::BTreeSet<String>,
    live_pricing_unusable_defects: &mut std::collections::BTreeSet<String>,
) {
    let Some(audit) = audit else {
        return;
    };
    unpriced_classes.extend(
        audit
            .unpriced_classes
            .iter()
            .map(|class| class.label().to_string()),
    );
    if let Some(defect) = audit.live_pricing_defect.as_ref() {
        if audit.estimate.is_some() {
            live_pricing_defects.insert(defect.label().to_string());
        } else {
            live_pricing_unusable_defects.insert(defect.label().to_string());
        }
    }
}

/// Add one priced amount to a running total without ever producing NaN,
/// infinity, or a negative sum. Mirrors the component rule of
/// `CostEstimate::saturating_add` so USD and CNY subtotals saturate
/// identically.
pub(super) fn saturating_add_cost_amount(total: &mut f64, delta: f64) {
    fn component(left: f64, right: f64) -> f64 {
        let left = if left.is_finite() && left >= 0.0 {
            left
        } else {
            0.0
        };
        let right = if right.is_finite() && right >= 0.0 {
            right
        } else {
            0.0
        };
        let sum = left + right;
        if sum.is_finite() { sum } else { f64::MAX }
    }
    *total = component(*total, delta);
}

pub(super) fn runtime_usage_bucket_key(
    group_by: UsageGroupBy,
    route: Option<&EffectiveRouteEnvelope>,
    turn: &TurnRecord,
    thread: &ThreadRecord,
) -> String {
    match group_by {
        UsageGroupBy::Day => route
            .map_or(turn.created_at, |route| route.dispatched_at)
            .format("%Y-%m-%d")
            .to_string(),
        UsageGroupBy::Model => codewhale_runtime::safe_label::sanitize_persisted_route_label(
            route
                .map(|route| route.model.as_str())
                .or_else(|| {
                    turn.effective_model
                        .as_deref()
                        .filter(|model| !model.trim().is_empty())
                })
                .unwrap_or(&thread.model),
        ),
        UsageGroupBy::Provider => codewhale_runtime::safe_label::sanitize_persisted_route_label(
            route
                .map(|route| {
                    if route.provider_identity.trim().is_empty() {
                        route.provider.as_str()
                    } else {
                        route.provider_identity.as_str()
                    }
                })
                .or_else(|| turn.effective_provider_label())
                .unwrap_or("unknown"),
        ),
        UsageGroupBy::Thread => thread.id.clone(),
    }
}

fn priced_step_usage(route: Option<&EffectiveRouteEnvelope>, usage: &Usage) -> Option<Value> {
    let audit = route?.audit(usage);
    let estimate = audit.estimate?.sanitized();
    Some(json!({
        "usd": if audit.usd_priced { estimate.usd } else { 0.0 },
        "cny": if audit.cny_priced { estimate.cny } else { 0.0 },
        "usd_priced": audit.usd_priced,
        "cny_priced": audit.cny_priced,
    }))
}

pub(super) fn billed_prompt_tokens(usage: &Usage) -> u32 {
    usage
        .request_usages
        .last()
        .map_or(usage.input_tokens, |last| last.input_tokens)
}

pub(super) fn accumulate_runtime_usage_record(
    totals: &mut UsageTotals,
    buckets: &mut std::collections::BTreeMap<String, UsageBucket>,
    group_by: UsageGroupBy,
    route: Option<&EffectiveRouteEnvelope>,
    usage: &Usage,
    turn: &TurnRecord,
    thread: &ThreadRecord,
) {
    let classes = crate::pricing::token_usage_for_pricing(usage);
    let reasoning = u64::from(usage.reasoning_tokens.unwrap_or(0));
    let reasoning_replay = u64::from(usage.reasoning_replay_tokens.unwrap_or(0));
    let audit = route.map(|route| route.audit(usage));
    let cost = audit
        .as_ref()
        .filter(|audit| audit.usd_priced)
        .and_then(|audit| audit.estimate)
        .map_or(0.0, |estimate| estimate.usd);
    // CNY accrues only from provider-published CNY rows, never projected
    // from the USD column, so routes without an authoritative CNY price
    // contribute 0 rather than a fabricated amount (mirrors the session
    // cost model in `tui/app.rs`).
    let cost_cny = audit
        .as_ref()
        .filter(|audit| audit.cny_priced)
        .and_then(|audit| audit.estimate)
        .map_or(0.0, |estimate| estimate.cny);
    let receipt = route.zip(audit.as_ref()).map(|(route, audit)| {
        crate::cost_status::effective_route_usage_receipt(route, audit, usage)
    });

    totals.input_tokens = totals.input_tokens.saturating_add(classes.input);
    totals.output_tokens = totals.output_tokens.saturating_add(classes.output);
    totals.cached_tokens = totals.cached_tokens.saturating_add(classes.cache_read);
    totals.reasoning_tokens = totals.reasoning_tokens.saturating_add(reasoning);
    totals.reasoning_replay_tokens = totals
        .reasoning_replay_tokens
        .saturating_add(reasoning_replay);
    totals.cache_write_tokens = totals
        .cache_write_tokens
        .saturating_add(classes.cache_write);
    saturating_add_cost_amount(&mut totals.cost_usd, cost);
    saturating_add_cost_amount(&mut totals.cost_cny, cost_cny);
    accumulate_runtime_cost_coverage(
        audit.as_ref(),
        &mut totals.priced_turns,
        &mut totals.unpriced_turns,
        &mut totals.cny_priced_turns,
        &mut totals.cny_unpriced_turns,
        &mut totals.nonmetered_turns,
        &mut totals.unpriced_reasons,
        &mut totals.cny_unpriced_reasons,
        &mut totals.pricing_provenances,
    );
    accumulate_runtime_cost_details(
        audit.as_ref(),
        &mut totals.unpriced_classes,
        &mut totals.live_pricing_defects,
        &mut totals.live_pricing_unusable_defects,
    );
    if let Some(receipt) = receipt.as_ref() {
        totals.route_receipts.insert(receipt.clone());
    }
    totals.turns = totals.turns.saturating_add(1);

    let key = runtime_usage_bucket_key(group_by, route, turn, thread);
    let bucket = buckets.entry(key.clone()).or_insert_with(|| UsageBucket {
        key,
        ..UsageBucket::default()
    });
    bucket.input_tokens = bucket.input_tokens.saturating_add(classes.input);
    bucket.output_tokens = bucket.output_tokens.saturating_add(classes.output);
    bucket.cached_tokens = bucket.cached_tokens.saturating_add(classes.cache_read);
    bucket.reasoning_tokens = bucket.reasoning_tokens.saturating_add(reasoning);
    bucket.reasoning_replay_tokens = bucket
        .reasoning_replay_tokens
        .saturating_add(reasoning_replay);
    bucket.cache_write_tokens = bucket
        .cache_write_tokens
        .saturating_add(classes.cache_write);
    saturating_add_cost_amount(&mut bucket.cost_usd, cost);
    saturating_add_cost_amount(&mut bucket.cost_cny, cost_cny);
    accumulate_runtime_cost_coverage(
        audit.as_ref(),
        &mut bucket.priced_turns,
        &mut bucket.unpriced_turns,
        &mut bucket.cny_priced_turns,
        &mut bucket.cny_unpriced_turns,
        &mut bucket.nonmetered_turns,
        &mut bucket.unpriced_reasons,
        &mut bucket.cny_unpriced_reasons,
        &mut bucket.pricing_provenances,
    );
    accumulate_runtime_cost_details(
        audit.as_ref(),
        &mut bucket.unpriced_classes,
        &mut bucket.live_pricing_defects,
        &mut bucket.live_pricing_unusable_defects,
    );
    if let Some(receipt) = receipt {
        bucket.route_receipts.insert(receipt);
    }
    bucket.turns = bucket.turns.saturating_add(1);
}

pub(super) fn usage_timestamp_in_range(
    timestamp: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
    since.is_none_or(|lower| timestamp >= lower) && until.is_none_or(|upper| timestamp <= upper)
}

pub(super) fn accumulate_truncated_runtime_usage(
    totals: &mut UsageTotals,
    buckets: &mut std::collections::BTreeMap<String, UsageBucket>,
    group_by: UsageGroupBy,
    dropped: u64,
    turn: &TurnRecord,
    thread: &ThreadRecord,
) {
    if dropped == 0 {
        return;
    }
    totals.dropped_usage_records = totals.dropped_usage_records.saturating_add(dropped);
    totals.unpriced_turns = totals.unpriced_turns.saturating_add(dropped);
    totals.cny_unpriced_turns = totals.cny_unpriced_turns.saturating_add(dropped);
    totals.turns = totals.turns.saturating_add(dropped);
    totals
        .unpriced_reasons
        .insert("runtime_usage_journal_truncated".to_string());
    totals
        .cny_unpriced_reasons
        .insert("runtime_usage_journal_truncated".to_string());

    let key = match group_by {
        UsageGroupBy::Day => turn.created_at.format("%Y-%m-%d").to_string(),
        UsageGroupBy::Model | UsageGroupBy::Provider => "unknown-truncated".to_string(),
        UsageGroupBy::Thread => thread.id.clone(),
    };
    let bucket = buckets.entry(key.clone()).or_insert_with(|| UsageBucket {
        key,
        ..UsageBucket::default()
    });
    bucket.dropped_usage_records = bucket.dropped_usage_records.saturating_add(dropped);
    bucket.unpriced_turns = bucket.unpriced_turns.saturating_add(dropped);
    bucket.cny_unpriced_turns = bucket.cny_unpriced_turns.saturating_add(dropped);
    bucket.turns = bucket.turns.saturating_add(dropped);
    bucket
        .unpriced_reasons
        .insert("runtime_usage_journal_truncated".to_string());
    bucket
        .cny_unpriced_reasons
        .insert("runtime_usage_journal_truncated".to_string());
}

pub(super) fn accumulate_runtime_child_usage_record(
    totals: &mut UsageTotals,
    buckets: &mut std::collections::BTreeMap<String, UsageBucket>,
    group_by: UsageGroupBy,
    child: &EffectiveRouteUsage,
    turn: &TurnRecord,
    thread: &ThreadRecord,
) {
    // Legacy persisted routed records may predate admission normalization.
    // Interpret their absent usage without rewriting or duplicating receipts.
    if child.usage == Usage::default() {
        accumulate_exact_runtime_usage_drop(
            totals,
            buckets,
            group_by,
            &child.route,
            crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage,
            turn,
            thread,
        );
    } else {
        accumulate_runtime_usage_record(
            totals,
            buckets,
            group_by,
            Some(&child.route),
            &child.usage,
            turn,
            thread,
        );
    }
}

pub(super) fn accumulate_exact_runtime_usage_drop(
    totals: &mut UsageTotals,
    buckets: &mut std::collections::BTreeMap<String, UsageBucket>,
    group_by: UsageGroupBy,
    route: &EffectiveRouteEnvelope,
    reason: crate::cost_status::RuntimeUsageMissingReason,
    turn: &TurnRecord,
    thread: &ThreadRecord,
) {
    let nonmetered = matches!(
        route.billing_mode,
        RouteBillingMode::Subscription | RouteBillingMode::Local
    );
    totals.dropped_usage_records = totals.dropped_usage_records.saturating_add(1);
    totals.turns = totals.turns.saturating_add(1);
    if nonmetered {
        totals.nonmetered_turns = totals.nonmetered_turns.saturating_add(1);
    } else {
        totals.unpriced_turns = totals.unpriced_turns.saturating_add(1);
        totals.cny_unpriced_turns = totals.cny_unpriced_turns.saturating_add(1);
        totals.unpriced_reasons.insert(reason.label().to_string());
        totals
            .cny_unpriced_reasons
            .insert(reason.label().to_string());
    }
    let audit = route.audit(&Usage::default());
    totals.route_receipts.insert(format!(
        "{} usage=missing reason={}",
        route.receipt(&audit),
        reason.label()
    ));

    let key = runtime_usage_bucket_key(group_by, Some(route), turn, thread);
    let bucket = buckets.entry(key.clone()).or_insert_with(|| UsageBucket {
        key,
        ..UsageBucket::default()
    });
    bucket.dropped_usage_records = bucket.dropped_usage_records.saturating_add(1);
    bucket.turns = bucket.turns.saturating_add(1);
    if nonmetered {
        bucket.nonmetered_turns = bucket.nonmetered_turns.saturating_add(1);
    } else {
        bucket.unpriced_turns = bucket.unpriced_turns.saturating_add(1);
        bucket.cny_unpriced_turns = bucket.cny_unpriced_turns.saturating_add(1);
        bucket.unpriced_reasons.insert(reason.label().to_string());
        bucket
            .cny_unpriced_reasons
            .insert(reason.label().to_string());
    }
    bucket.route_receipts.insert(format!(
        "{} usage=missing reason={}",
        route.receipt(&audit),
        reason.label()
    ));
}

pub(super) fn resolve_runtime_thread_route(
    config: &Config,
    identity: &ProviderIdentity,
    model_selector: Option<&str>,
) -> Result<ResolvedRuntimeRoute> {
    resolve_runtime_route_for_identity(config, identity, model_selector)
        .map_err(|reason| anyhow!("Failed to resolve runtime thread route: {reason}"))
}

pub(super) fn resolve_runtime_thread_route_for_identity(
    config: &Config,
    identity: &ProviderIdentity,
    model_selector: Option<&str>,
) -> Result<ResolvedRuntimeRoute> {
    resolve_runtime_route_for_identity(config, identity, model_selector)
        .map_err(|reason| anyhow!("Failed to resolve runtime thread route: {reason}"))
}

pub(super) fn runtime_compaction_config(
    config: &Config,
    provider: ProviderKind,
    model: &str,
    route_budget: (Option<codewhale_config::route::RouteLimits>, Option<u32>),
    auto_compact: bool,
    auto_compact_explicit: bool,
    threshold_percent: f64,
) -> CompactionConfig {
    let (route_limits, effective_window) = route_budget;
    CompactionConfig {
        enabled: if auto_compact_explicit {
            auto_compact
        } else {
            auto_compact_default_for_route(provider, model, route_limits)
        },
        model: model.to_string(),
        token_threshold: crate::route_budget::compaction_threshold_for_route_at_percent_with_window(
            provider,
            model,
            route_limits,
            effective_window,
            threshold_percent,
        ),
        effective_context_window: Some(
            effective_window
                .unwrap_or_else(|| route_context_window_tokens(provider, model, route_limits)),
        ),
        summary_instructions: config.compaction_summary_instructions(),
        retained_user_message_tokens: config.compaction_retained_user_message_tokens(),
        ..Default::default()
    }
}

#[derive(Debug, Clone)]
pub(super) struct ActiveTurnState {
    pub(super) narrowing: crate::core::engine::TurnNarrowing,
    pub(super) turn_id: String,
    pub(super) goal_id: Option<String>,
    pub(super) goal_progress: Option<crate::tools::goal::GoalSnapshot>,
    pub(super) interrupt_requested: bool,
    pub(super) compaction_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ClaimedTurnKind {
    Message {
        structured_review: bool,
        narrowing: crate::core::engine::TurnNarrowing,
    },
    Compaction,
}

impl ClaimedTurnKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Message { .. } => "turn",
            Self::Compaction => "compaction turn",
        }
    }
}

/// Shared streamed/terminal projection. Usage is accrued once at terminal
/// settlement; compact reviewer history is checkpointed at every GoalUpdated.
pub(super) fn merge_engine_goal_progress(
    goal: &mut codewhale_protocol::ThreadGoal,
    snapshot: &crate::tools::goal::GoalSnapshot,
) {
    use codewhale_protocol::ThreadGoalStatus as Status;
    if goal.status != Status::Active
        || snapshot.objective.as_deref() != Some(goal.objective.as_str())
        || snapshot
            .goal_id
            .as_deref()
            .is_some_and(|id| id != goal.goal_id)
        || i64::from(snapshot.continuation_count) < goal.continuation_count
    {
        return;
    }
    goal.continuation_count = i64::from(snapshot.continuation_count);
    goal.last_gap_fingerprint
        .clone_from(&snapshot.last_gap_fingerprint);
    goal.repeated_gap_count = snapshot.repeated_gap_count;
    goal.last_gap_pass = snapshot.last_gap_pass;
    goal.pause_reason = snapshot.pause_reason;
    goal.status = match snapshot.status.as_str() {
        "complete" => Status::Complete,
        "blocked" => Status::Blocked,
        "paused" => match snapshot.pause_reason {
            Some(codewhale_protocol::GoalPauseReason::UsageLimit) => Status::UsageLimited,
            Some(codewhale_protocol::GoalPauseReason::BudgetLimit) => Status::BudgetLimited,
            _ => Status::Paused,
        },
        _ => Status::Active,
    };
    goal.updated_at = chrono::Utc::now().timestamp();
}

#[derive(Clone)]
pub(super) struct ActiveThreadState {
    pub(super) engine: EngineHandle,
    pub(super) active_turn: Option<ActiveTurnState>,
    pub(super) route_identity: ProviderIdentity,
    pub(super) route_model: String,
    /// The thread's hook executor (B4): global, reviewed plugin and trusted
    /// project hooks for the thread's workspace. `None` only for injected
    /// test engines.
    pub(super) hook_executor: Option<Arc<crate::hooks::HookExecutor>>,
    /// Real engines client-preflight before an in-progress record is written.
    /// Explicitly injected test engines own their client seam.
    pub(super) client_preflight_required: bool,
}

#[derive(Default)]
pub(super) struct ActiveThreads {
    pub(super) engines: HashMap<String, ActiveThreadState>,
    pub(super) lru: VecDeque<String>,
    /// Per-thread shell/job authority shared with the thread's loaded engine.
    /// Entries outlive engine LRU eviction so background jobs keep running and
    /// stay reachable through the jobs API; an entry is removed only when the
    /// thread itself is removed, which drops the manager and kills its jobs.
    pub(super) shell_managers: HashMap<String, SharedShellManager>,
}

pub(super) struct NativeForkSource {
    pub(super) sessions_dir: PathBuf,
    pub(super) session: Option<crate::session_manager::SavedSession>,
    pub(super) goal: Option<crate::session_manager::SessionGoalState>,
    pub(super) lease: Option<crate::session_manager::SessionLease>,
}

pub struct PreparedThreadFork {
    pub(super) source_id: String,
    /// The first turn the fork *drops*, when it drops any: the turn whose
    /// prompt the receipt hands back. `None` for a fork that keeps the whole
    /// conversation (a branch point at the last turn).
    pub(super) dropped_turn_id: Option<String>,
    pub(super) depth_from_tail: usize,
    pub(super) thread: ThreadRecord,
    pub(super) records: Vec<(TurnRecord, Vec<TurnItemRecord>)>,
    pub(super) original_user_text: Option<String>,
    pub(super) original_images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
    pub(super) max_output_tokens: Option<std::num::NonZeroU32>,
    /// The checked full source/goal/lease, retained prefix and covered cloned
    /// turn travel together. The private target document is written only at
    /// publish time, so abandoning preparation never publishes a fork.
    pub(super) own_session: (NativeForkSource, Vec<Message>, Option<String>),
    /// The turns the fork drops, oldest first, with the workspace restore
    /// points each recorded: what a turn-scoped file undo rolls back.
    pub(super) dropped_turns: Vec<DroppedTurnSnapshots>,
}

impl PreparedThreadFork {
    /// The turns this fork drops, oldest first.
    pub fn dropped_turns(&self) -> &[DroppedTurnSnapshots] {
        &self.dropped_turns
    }

    pub fn original_user_text(&self) -> Option<&str> {
        self.original_user_text.as_deref()
    }
}

/// One turn a fork drops, as a turn-scoped file undo sees it.
#[derive(Debug, Clone)]
pub struct DroppedTurnSnapshots {
    pub turn_id: String,
    /// Whether the turn may have changed workspace files, and so needs a
    /// restore point to be undone. Every change a turn makes goes through a
    /// tool call (file tools, shell, sub-agents), so a turn this Runtime ran
    /// (its record carries the policy receipt every accepted turn gets) with
    /// no tool items provably changed nothing, as does an accounting-only
    /// routing settlement or a record with no items at all. A turn imported
    /// from a saved session proves nothing either way: saved documents need
    /// not keep tool calls.
    pub may_change_files: bool,
    /// The restore points the engine reported for the turn, in order.
    pub snapshots: Vec<codewhale_snapshot::WorkspaceSnapshotRef>,
    /// Paths the turn's file-tool calls (`write_file`, `edit_file`,
    /// `apply_patch`) declared they write, as the calls named them, for every
    /// call that ran (completed, or stopped mid-run). A turn-scoped undo can
    /// only restore what the snapshots hold, so each is checked against the
    /// snapshot exclusions.
    pub declared_writes: Vec<String>,
    /// Tool calls recorded as failed, canceled or never started: whatever
    /// paths their snapshot receipts declare, they wrote nothing.
    pub unrun_tool_calls: std::collections::BTreeSet<String>,
}

/// Shared ownership of an existing task's join. A canceled drain drops only
/// its await/lock guard; the unfinished join stays available to the next drain.
pub(super) type RuntimeCompletion = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

pub(super) fn retained_completion(worker: tokio::task::JoinHandle<()>) -> RuntimeCompletion {
    Arc::new(Mutex::new(Some(worker)))
}

pub(super) fn completion_finished(completion: &RuntimeCompletion) -> bool {
    completion
        .try_lock()
        .is_ok_and(|worker| worker.as_ref().is_none_or(|worker| worker.is_finished()))
}

pub(super) async fn await_retained_completion(completion: &RuntimeCompletion) -> Result<()> {
    let mut worker = completion.lock().await;
    let Some(join) = worker.as_mut() else {
        return Ok(());
    };
    let result = join.await;
    *worker = None;
    result.context("Runtime worker shutdown failed")
}

pub type SharedRuntimeThreadManager = Arc<RuntimeThreadManager>;

pub(super) fn push_runtime_notice(
    notices: &parking_lot::Mutex<HashMap<String, Vec<ActiveNotice>>>,
    thread_id: &str,
    kind: &str,
    turn_id: &str,
    subject: &str,
    detail: String,
) -> String {
    debug_assert!(
        codewhale_config::notifications::NotificationEvent::parse(kind).is_some(),
        "notice kind must be a NotificationEvent name: {kind}"
    );
    let id = runtime_record_id("notice");
    let mut notices = notices.lock();
    let list = notices.entry(thread_id.to_string()).or_default();
    list.push(ActiveNotice {
        id: id.clone(),
        kind: kind.to_string(),
        turn_id: turn_id.to_string(),
        subject: subject.to_string(),
        detail,
        raised_at: Utc::now(),
    });
    if list.len() > MAX_NOTICES_PER_THREAD {
        list.drain(..list.len() - MAX_NOTICES_PER_THREAD);
    }
    id
}

pub(super) struct RuntimeWorkerEventSink {
    pub(super) thread_id: String,
    pub(super) workspace: PathBuf,
    pub(super) manager: SharedSubAgentManager,
    pub(super) store: RuntimeThreadStore,
    pub(super) projection: Arc<Mutex<()>>,
    pub(super) event_emit: Arc<Mutex<()>>,
    pub(super) event_tx: broadcast::Sender<RuntimeEventRecord>,
    pub(super) notices: Arc<parking_lot::Mutex<HashMap<String, Vec<ActiveNotice>>>>,
    pub(super) owner: std::sync::Weak<RuntimeProcessOwnerLock>,
}

impl RuntimeWorkerEventSink {
    pub(super) async fn publish(&self, event: EngineEvent) -> Result<Option<String>> {
        let (owner, agent_id, event_name) = match &event {
            EngineEvent::AgentSpawned {
                owner_session_id,
                id,
                ..
            } => (owner_session_id, id.clone(), "agent.spawned"),
            EngineEvent::AgentProgress {
                owner_session_id,
                id,
                ..
            } => (owner_session_id, id.clone(), "agent.progress"),
            EngineEvent::AgentComplete {
                owner_session_id,
                id,
                ..
            } => (owner_session_id, id.clone(), "agent.completed"),
            _ => return Ok(None),
        };
        if owner != &self.thread_id {
            return Ok(None);
        }
        let _owner = self
            .owner
            .upgrade()
            .context("Runtime event owner has closed")?;
        let record = self
            .manager
            .read()
            .await
            .worker_records_for_workspace(&self.workspace)
            .map_err(anyhow::Error::msg)?
            .into_iter()
            .map(|(record, _)| record)
            .find(|record| {
                record.owner_session_id == self.thread_id && record.spec.worker_id == agent_id
            });
        let Some(record) = record else {
            return Ok(None);
        };
        let Some(turn_id) = record.runtime_origin_turn_id else {
            return Ok(None);
        };
        let thread_id = self.thread_id.clone();
        let _projection = self.projection.lock().await;
        let store = self.store.clone();
        let origin = turn_id.clone();
        let selected_thread = thread_id.clone();
        let selected_workspace = self.workspace.clone();
        let selected_agent = agent_id.clone();
        let duplicate = tokio::task::spawn_blocking(move || -> Result<bool> {
            let thread = store.load_thread(&selected_thread)?;
            let turn = store.load_turn(&origin)?;
            anyhow::ensure!(
                turn.thread_id == selected_thread && thread.workspace == selected_workspace,
                "child Runtime turn owner has changed"
            );
            if event_name == "agent.progress" {
                return Ok(false);
            }
            store.contains_event(
                &selected_thread,
                &RuntimeEventMatch::WorkerLifecycle {
                    turn_id: origin,
                    agent_id: selected_agent,
                    event_name: event_name.to_string(),
                },
            )
        })
        .await
        .context("child receipt origin check failed")??;
        if duplicate {
            return Ok(Some(turn_id));
        }
        let item_id = if event_name == "agent.progress" {
            runtime_record_id("item")
        } else {
            format!(
                "item_{}",
                codewhale_runtime::hashing::sha256_hex(format!(
                    "{turn_id}\0{agent_id}\0{event_name}"
                ))
            )
        };
        match event {
            EngineEvent::AgentSpawned {
                owner_session_id,
                id,
                prompt,
                worker_status,
                parent_run_id,
                spawn_depth,
                display_name,
                ..
            } if owner_session_id == thread_id => {
                // Hosts name the agent the way the TUI does (#6565).
                let name = display_name.as_deref().unwrap_or(&id);
                let message = format!(
                    "Sub-agent {name} spawned: {}",
                    summarize_text(&prompt, SUMMARY_LIMIT)
                );
                let item = TurnItemRecord {
                    schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                    id: item_id,
                    turn_id: turn_id.clone(),
                    kind: TurnItemKind::Status,
                    status: TurnItemLifecycleStatus::Completed,
                    summary: summarize_text(&message, SUMMARY_LIMIT),
                    detail: Some(message),
                    metadata: None,
                    artifact_refs: Vec::new(),
                    artifacts: Vec::new(),
                    started_at: Some(Utc::now()),
                    ended_at: Some(Utc::now()),
                };
                self.save_item(&item).await?;
                self.emit_event(
                    &thread_id,
                    Some(&turn_id),
                    Some(&item.id),
                    "agent.spawned",
                    json!({ "item": item, "agent_id": id, "agent_name": display_name,
                            "worker_status": worker_status, "parent_run_id": parent_run_id,
                            "spawn_depth": spawn_depth }),
                )
                .await?;
            }
            EngineEvent::AgentProgress {
                owner_session_id,
                id,
                status,
                activity,
                parent_run_id,
                spawn_depth,
            } if owner_session_id == thread_id => {
                let message = format!("Sub-agent {id}: {status}");
                let item = TurnItemRecord {
                    schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                    id: item_id,
                    turn_id: turn_id.clone(),
                    kind: TurnItemKind::Status,
                    status: TurnItemLifecycleStatus::Completed,
                    summary: summarize_text(&message, SUMMARY_LIMIT),
                    detail: Some(message),
                    metadata: None,
                    artifact_refs: Vec::new(),
                    artifacts: Vec::new(),
                    started_at: Some(Utc::now()),
                    ended_at: Some(Utc::now()),
                };
                self.save_item(&item).await?;
                self.emit_event(
                    &thread_id,
                    Some(&turn_id),
                    Some(&item.id),
                    "agent.progress",
                    json!({ "item": item, "agent_id": id,
                            "worker_status": activity.worker_status, "step": activity.step,
                            "parent_run_id": parent_run_id, "spawn_depth": spawn_depth }),
                )
                .await?;
            }
            EngineEvent::AgentComplete {
                owner_session_id,
                id,
                result,
                outcome,
                parent_run_id,
                spawn_depth,
                continuable,
                usage,
                display_name,
            } if owner_session_id == thread_id => {
                let worker_status = outcome
                    .as_ref()
                    .map(crate::tools::subagent::subagent_status_name);
                let name = display_name.as_deref().unwrap_or(&id);
                let message = format!(
                    "Sub-agent {name} {}: {}",
                    worker_status.unwrap_or("settled (outcome unconfirmed)"),
                    summarize_text(&result, SUMMARY_LIMIT)
                );
                let item = TurnItemRecord {
                    schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                    id: item_id,
                    turn_id: turn_id.clone(),
                    kind: TurnItemKind::Status,
                    status: TurnItemLifecycleStatus::Completed,
                    summary: summarize_text(&message, SUMMARY_LIMIT),
                    detail: Some(message),
                    metadata: None,
                    artifact_refs: Vec::new(),
                    artifacts: Vec::new(),
                    started_at: Some(Utc::now()),
                    ended_at: Some(Utc::now()),
                };
                self.save_item(&item).await?;
                self.emit_event(
                    &thread_id,
                    Some(&turn_id),
                    Some(&item.id),
                    "agent.completed",
                    json!({ "item": item, "agent_id": id, "agent_name": display_name,
                            "worker_status": worker_status, "parent_run_id": parent_run_id,
                            "spawn_depth": spawn_depth, "continuable": continuable,
                            "usage": usage }),
                )
                .await?;
                self.raise_notice(
                    &thread_id,
                    codewhale_config::notifications::NotificationEvent::SubagentTerminal.as_str(),
                    &turn_id,
                    &id,
                    format!(
                        "{name} {}",
                        match outcome.as_ref() {
                            Some(SubAgentStatus::Completed) => "finished",
                            Some(SubAgentStatus::Failed(_) | SubAgentStatus::BudgetExhausted) =>
                                "failed",
                            Some(SubAgentStatus::Cancelled | SubAgentStatus::Interrupted(_)) =>
                                "stopped",
                            Some(SubAgentStatus::Running) | None => "settled (outcome unconfirmed)",
                        }
                    ),
                );
            }
            _ => {}
        }
        Ok(Some(turn_id))
    }

    pub(super) async fn save_item(&self, item: &TurnItemRecord) -> Result<()> {
        let store = self.store.clone();
        let item = item.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            store.save_item(&item)?;
            let _turn_mutation = store.turn_mutation.lock();
            let mut turn = store.load_turn(&item.turn_id)?;
            if !turn.item_ids.contains(&item.id) {
                turn.item_ids.push(item.id);
                store.save_turn(&turn)?;
            }
            Ok(())
        })
        .await
        .context("child receipt persistence failed")?
    }

    pub(super) async fn emit_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        let _emit = self.event_emit.lock().await;
        let record = self
            .store
            .append_event(thread_id, turn_id, item_id, event, payload)
            .await?;
        let _ = self.event_tx.send(record.clone());
        Ok(record)
    }

    pub(super) fn raise_notice(
        &self,
        thread_id: &str,
        kind: &str,
        turn_id: &str,
        subject: &str,
        detail: String,
    ) -> String {
        push_runtime_notice(&self.notices, thread_id, kind, turn_id, subject, detail)
    }
}

#[derive(Clone)]
pub(super) struct RecoveredTurnReceipt {
    pub(super) turn: TurnRecord,
    pub(super) unresolved_dynamic_tools: Vec<DynamicToolCallParams>,
}

/// Manages active engine threads, lifecycle, and event persistence.
///
/// # Lock ordering invariant
///
/// Runtime state uses nine lock classes:
/// - `RuntimeThreadManager::engine_load` — serializes cache-miss engine builds.
///   It may cross awaits and is always acquired before `active`.
/// - `RuntimeThreadManager::event_emit` — preserves append-to-broadcast event
///   order and is only acquired after all record/engine guards are released.
/// - `RuntimeThreadManager::projection_locks` — one async lock per thread,
///   held while a streamed item checkpoint and its event are published or
///   while a terminal turn projection, receipt, and active-claim cleanup are
///   published, or while a snapshot captures its cursor and reads projections.
/// - `RuntimeThreadManager::recovery_flush` — serializes deferred receipt
///   reconciliation before it acquires a projection lock and `event_emit`.
/// - the Runtime event-file transaction lock — serializes writes across processes.
/// - `RuntimeThreadStore::thread_mutation` — synchronizes short, synchronous
///   thread-record load-modify-save transactions and never crosses `.await`.
/// - `RuntimeThreadStore::turn_mutation` — reentrant guard for turn records; `save_turn` always acquires it.
/// - `RuntimeThreadStore::goal_mutation` — serializes goal controls and progress;
///   acquired after `active` and before `thread_mutation` at turn admission.
/// - `RuntimeThreadManager::active` — protects the set of loaded engine handles.
///
/// `state` is never held with `active`, either record-mutation guard, or
/// `engine_load`. Streaming projection publication acquires its per-thread
/// projection lock before `event_emit`, which acquires `state`; snapshots
/// acquire only the projection lock and then `state`. All guards are released
/// before returning. All
/// `emit_event` calls happen after `active`, `thread_mutation`, and
/// `turn_mutation` have been released. When record and engine state must change
/// atomically, acquire `active` before the applicable record-mutation guard and
/// release both before awaiting.
#[derive(Clone)]
pub struct RuntimeThreadManager {
    pub(super) host_profile: crate::core::engine::EngineHostProfile,
    pub(super) config: Arc<parking_lot::RwLock<Config>>,
    pub(super) workspace: PathBuf,
    pub(super) plugin_registry: Option<Arc<codewhale_plugins::PluginRegistry>>,
    pub(super) store: RuntimeThreadStore,
    pub(super) _process_owner_lock: Arc<RuntimeProcessOwnerLock>,
    /// Concurrent turn admissions share a read lease; config reload owns the
    /// write lease from validation through publication. Saved-session binding
    /// also owns the write lease across snapshot and checkpoint persistence.
    /// This orders both route changes and saved-history boundaries with dispatch.
    pub(super) config_admission: Arc<AsyncRwLock<()>>,
    pub(super) engine_load: Arc<Mutex<()>>,
    pub(super) shutdown_drain: Arc<Mutex<()>>,
    pub(super) engine_workers:
        Arc<parking_lot::Mutex<Vec<(String, EngineHandle, RuntimeCompletion)>>>,
    pub(super) subagent_manager: Arc<Mutex<Option<SharedSubAgentManager>>>,
    pub(super) turn_monitors: Arc<parking_lot::Mutex<Vec<RuntimeCompletion>>>,
    pub(super) event_monitors: Arc<parking_lot::Mutex<Vec<RuntimeCompletion>>>,
    pub(super) event_reader_wake: Arc<Notify>,
    pub(super) event_pump_stop: CancellationToken,
    pub(super) _event_pump_owner: Arc<tokio_util::sync::DropGuard>,
    pub(super) active: Arc<Mutex<ActiveThreads>>,
    pub(super) event_emit: Arc<Mutex<()>>,
    pub(super) projection_locks: Arc<parking_lot::Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    pub(super) event_tx: broadcast::Sender<RuntimeEventRecord>,
    pub(super) manager_cfg: RuntimeThreadManagerConfig,
    pub(super) cancel_token: CancellationToken,
    task_manager: Arc<parking_lot::Mutex<std::sync::Weak<crate::task_manager::TaskManager>>>,
    task_execution_lease:
        Arc<parking_lot::Mutex<Option<Arc<crate::task_manager::TaskExecutionLease>>>>,
    automations:
        Arc<parking_lot::Mutex<Option<crate::automation_manager::SharedAutomationManager>>>,
    pub(super) pending_approvals: Arc<parking_lot::Mutex<HashMap<String, PendingApprovalEntry>>>,
    /// Session approval grants per thread id.
    pub(super) grants: crate::core::authority::grants::GrantStore,
    pub(super) pending_user_inputs:
        Arc<parking_lot::Mutex<HashMap<(String, String), PendingUserInputEntry>>>,
    pub(super) pending_dynamic_tools:
        Arc<parking_lot::Mutex<HashMap<String, PendingDynamicToolEntry>>>,
    pending_commands: Arc<parking_lot::Mutex<HashMap<(String, String), CancellationToken>>>,
    pub(super) recovery_receipts:
        Arc<parking_lot::Mutex<HashMap<String, Vec<RecoveredTurnReceipt>>>>,
    pub(super) notices: Arc<parking_lot::Mutex<HashMap<String, Vec<ActiveNotice>>>>,
    pub(super) recovery_flush: Arc<Mutex<()>>,
    /// One hook observer pool for the process. Each thread's executor is a
    /// `rebind` of it, so a thread gets its own hook set and workspace without
    /// spawning another dispatcher.
    pub(super) hook_base: Arc<std::sync::OnceLock<crate::hooks::HookExecutor>>,
    #[cfg(any(test, feature = "test-support"))]
    snapshot_test_hook: Arc<parking_lot::Mutex<Option<mpsc::UnboundedSender<SnapshotTestPoint>>>>,
    #[cfg(any(test, feature = "test-support"))]
    replay_test_hook: Arc<parking_lot::Mutex<Option<mpsc::UnboundedSender<ReplayTestPoint>>>>,
    /// Test seam: the model client every engine this manager builds uses in
    /// place of the route's provider client. Everything else about the build
    /// — `EngineConfig`, `SyncSession`, snapshots — is the production path.
    #[cfg(any(test, feature = "test-support"))]
    test_model_client:
        Arc<parking_lot::Mutex<Option<crate::core::model_client::SharedModelClient>>>,
    /// Test seams for the external approval and dynamic tool waits; `0`
    /// keeps the production behavior.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) test_approval_decision_timeout_ms: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(any(test, feature = "test-support"))]
    pub(super) test_dynamic_tool_result_timeout_ms: Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(any(test, feature = "test-support"))]
impl RuntimeThreadManager {
    pub fn test_store(&self) -> &RuntimeThreadStore {
        &self.store
    }

    pub fn reset_whole_store_scan_file_reads(&self) {
        self.store
            .turn_dir_files_read
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.store
            .item_dir_files_read
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn whole_store_scan_file_reads(&self) -> (u64, u64) {
        (
            self.store
                .turn_dir_files_read
                .load(std::sync::atomic::Ordering::SeqCst),
            self.store
                .item_dir_files_read
                .load(std::sync::atomic::Ordering::SeqCst),
        )
    }
}

/// Helper types for `seed_thread_from_messages` — intermediate representation
/// of a turn being built from session messages before persisting as items.
///
/// A single content block extracted from an assistant message.
pub(super) enum SeedItem {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        execution_id: Option<String>,
        name: String,
        input: serde_json::Value,
        caller: Option<ToolCaller>,
        thought_signature: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        execution_id: Option<String>,
        content: String,
        is_error: bool,
        content_blocks: Option<Vec<serde_json::Value>>,
    },
}

/// A turn being assembled from session messages.
pub(super) struct TurnSeed {
    pub(super) user_text: String,
    pub(super) image_content: Vec<ContentBlock>,
    pub(super) items: Vec<SeedItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeApprovalDecision {
    ApproveTool,
    DenyTool,
    RetryWithFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalApprovalDecision {
    Allow { remember: bool },
    Deny { remember: bool },
}

pub(super) struct PendingApprovalEntry {
    pub(super) thread_id: String,
    pub(super) request: PendingApprovalRequest,
    pub(super) sender: oneshot::Sender<ExternalApprovalDecision>,
}

pub(super) struct PendingUserInputEntry {
    pub(super) request: PendingUserInputRequest,
    /// A request remains snapshot-visible while its winner appends the
    /// secret-free terminal receipt. This prevents a snapshot cursor from
    /// observing neither the pending prompt nor its settlement event.
    pub(super) settling: bool,
    pub(super) settlement_tx: watch::Sender<u64>,
    /// An append whose rollback failed may or may not be durable. Never send
    /// the answer or allow a retry in that state: either could disclose or
    /// duplicate a response whose receipt cannot be established safely.
    pub(super) indeterminate: bool,
}

pub(super) enum PendingUserInputClaim {
    Claimed(PendingUserInputRequest),
    Settling,
    Indeterminate,
    Missing,
}

pub(super) enum UserInputTerminalOutcome {
    Answered(crate::tools::user_input::UserInputResponse),
    Canceled { terminal: bool },
    Closed(crate::core::events::UserInputClosedOutcome),
}

#[derive(Clone)]
pub(super) struct RuntimeUserInputSettlement {
    thread_id: String,
    pending_user_inputs: Arc<parking_lot::Mutex<HashMap<(String, String), PendingUserInputEntry>>>,
    store: RuntimeThreadStore,
    projection: Arc<Mutex<()>>,
    event_emit: Arc<Mutex<()>>,
    event_tx: broadcast::Sender<RuntimeEventRecord>,
    owner: std::sync::Weak<RuntimeProcessOwnerLock>,
}

impl RuntimeUserInputSettlement {
    pub(super) fn claim_pending_user_input(
        &self,
        thread_id: &str,
        input_id: &str,
        turn_id: Option<&str>,
        response: Option<&crate::tools::user_input::UserInputResponse>,
    ) -> Result<PendingUserInputClaim> {
        let mut pending = self.pending_user_inputs.lock();
        let Some(entry) = pending.get_mut(&(thread_id.to_string(), input_id.to_string())) else {
            return Ok(PendingUserInputClaim::Missing);
        };
        if turn_id.is_some_and(|turn_id| entry.request.turn_id != turn_id) {
            return Ok(PendingUserInputClaim::Missing);
        }
        if entry.indeterminate {
            return Ok(PendingUserInputClaim::Indeterminate);
        }
        if entry.settling {
            return Ok(PendingUserInputClaim::Settling);
        }
        if let Some(response) = response {
            entry.request.request.validate_response(response)?;
        }
        entry.settling = true;
        Ok(PendingUserInputClaim::Claimed(entry.request.clone()))
    }

    pub(super) fn restore_pending_user_input_claim(
        &self,
        thread_id: &str,
        request: &PendingUserInputRequest,
    ) {
        let settlement_tx = if let Some(entry) = self
            .pending_user_inputs
            .lock()
            .get_mut(&(thread_id.to_string(), request.id.clone()))
            && entry.request.turn_id == request.turn_id
        {
            entry.settling = false;
            entry.indeterminate = false;
            Some(entry.settlement_tx.clone())
        } else {
            None
        };
        if let Some(settlement_tx) = settlement_tx {
            settlement_tx.send_modify(|epoch| *epoch = epoch.saturating_add(1));
        }
    }

    fn mark_pending_user_input_indeterminate(
        &self,
        thread_id: &str,
        request: &PendingUserInputRequest,
    ) {
        let settlement_tx = if let Some(entry) = self
            .pending_user_inputs
            .lock()
            .get_mut(&(thread_id.to_string(), request.id.clone()))
            && entry.request.turn_id == request.turn_id
        {
            entry.settling = true;
            entry.indeterminate = true;
            Some(entry.settlement_tx.clone())
        } else {
            None
        };
        if let Some(settlement_tx) = settlement_tx {
            settlement_tx.send_modify(|epoch| *epoch = epoch.saturating_add(1));
        }
    }

    fn finish_pending_user_input_settlement(
        &self,
        thread_id: &str,
        request: &PendingUserInputRequest,
    ) -> Option<watch::Sender<u64>> {
        let mut pending = self.pending_user_inputs.lock();
        let key = (thread_id.to_string(), request.id.clone());
        let settlement_tx = if pending.get(&key).is_some_and(|entry| {
            entry.request.turn_id == request.turn_id && entry.settling && !entry.indeterminate
        }) {
            pending.remove(&key).map(|entry| entry.settlement_tx)
        } else {
            None
        };
        drop(pending);
        settlement_tx
    }

    pub(super) async fn settle_claimed_user_input(
        &self,
        thread_id: &str,
        engine: Option<EngineHandle>,
        request: PendingUserInputRequest,
        outcome: UserInputTerminalOutcome,
    ) -> Result<bool> {
        let _owner = self
            .owner
            .upgrade()
            .context("Runtime user-input owner was released")?;
        anyhow::ensure!(
            thread_id == self.thread_id,
            "Runtime user-input thread changed"
        );
        let projection_lock = self.projection.clone();
        let _projection = projection_lock.lock().await;
        let (event, payload) = match &outcome {
            UserInputTerminalOutcome::Answered(_) => (
                "user_input.answered",
                json!({ "id": &request.id, "input_id": &request.id }),
            ),
            UserInputTerminalOutcome::Closed(outcome) => (
                if *outcome == crate::core::events::UserInputClosedOutcome::Answered {
                    "user_input.answered"
                } else {
                    "user_input.canceled"
                },
                json!({"id": &request.id, "input_id": &request.id, "outcome": outcome}),
            ),
            UserInputTerminalOutcome::Canceled { terminal } => (
                "user_input.canceled",
                json!({
                    "id": &request.id,
                    "input_id": &request.id,
                    "terminal": terminal,
                }),
            ),
        };
        if let Err(error) = self
            .emit_event(thread_id, Some(&request.turn_id), None, event, payload)
            .await
        {
            if event_append_is_indeterminate(&error) {
                self.mark_pending_user_input_indeterminate(thread_id, &request);
            } else {
                self.restore_pending_user_input_claim(thread_id, &request);
            }
            return Err(error);
        }
        drop(_projection);

        let terminal = matches!(
            &outcome,
            UserInputTerminalOutcome::Canceled { terminal: true }
                | UserInputTerminalOutcome::Closed(_)
        );
        let delivery_result = match (engine, outcome) {
            (Some(engine), UserInputTerminalOutcome::Answered(response)) => {
                engine.submit_user_input(&request.id, response).await
            }
            (Some(engine), UserInputTerminalOutcome::Canceled { .. }) => {
                engine.cancel_user_input(&request.id).await
            }
            (_, UserInputTerminalOutcome::Closed(_)) | (None, _) => Ok(()),
        };
        if let Err(error) = delivery_result {
            if !terminal {
                // Keep the same claim until Engine's verdict. A simultaneous
                // ToolCallComplete waits for this owner, then closes it; a
                // failed delivery alone must not erase an indefinite waiter.
                let _projection = projection_lock.lock().await;
                if let Err(receipt_error) = self.emit_event(
                    thread_id,
                    Some(&request.turn_id),
                    None,
                    "user_input.required",
                    json!({
                        "id": &request.id,
                        "request": &request.request,
                        "delivery_error": "Engine did not accept the decision. Retry if the question is still pending.",
                    }),
                ).await {
                    if event_append_is_indeterminate(&receipt_error) {
                        self.mark_pending_user_input_indeterminate(thread_id, &request);
                    } else {
                        self.restore_pending_user_input_claim(thread_id, &request);
                    }
                    return Err(receipt_error);
                }
                self.restore_pending_user_input_claim(thread_id, &request);
                return Err(error);
            }
            // A terminal turn already ended the waiter. Its durable cleanup
            // remains authoritative even when Engine rejects a late cancel.
            tracing::debug!(thread_id, input_id = %request.id,
                "Terminal user-input cancellation was durable after waiter ended: {error}");
        }
        let settlement_tx = self.finish_pending_user_input_settlement(thread_id, &request);
        if let Some(settlement_tx) = settlement_tx {
            settlement_tx.send_modify(|epoch| *epoch = epoch.saturating_add(1));
        }
        Ok(true)
    }

    pub(super) async fn close(
        &self,
        input_id: &str,
        turn_id: Option<&str>,
        outcome: crate::core::events::UserInputClosedOutcome,
    ) -> Result<()> {
        let request = loop {
            match self.claim_pending_user_input(&self.thread_id, input_id, turn_id, None)? {
                PendingUserInputClaim::Claimed(request) => break request,
                PendingUserInputClaim::Missing => return Ok(()),
                PendingUserInputClaim::Settling => {
                    let progress = self
                        .pending_user_inputs
                        .lock()
                        .get(&(self.thread_id.clone(), input_id.to_string()))
                        .filter(|entry| {
                            entry.settling
                                && !entry.indeterminate
                                && turn_id.is_none_or(|turn_id| entry.request.turn_id == turn_id)
                        })
                        .map(|entry| entry.settlement_tx.subscribe());
                    if let Some(mut progress) = progress {
                        let _ = progress.changed().await;
                    }
                }
                PendingUserInputClaim::Indeterminate => bail!(
                    "User-input request '{input_id}' has an indeterminate terminal receipt; inspect Runtime storage before closing"
                ),
            }
        };
        self.settle_claimed_user_input(
            &self.thread_id,
            None,
            request,
            UserInputTerminalOutcome::Closed(outcome),
        )
        .await?;
        Ok(())
    }

    async fn emit_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        let _emit = self.event_emit.lock().await;
        let record = self
            .store
            .append_event(thread_id, turn_id, item_id, event, payload)
            .await?;
        let _ = self.event_tx.send(record.clone());
        Ok(record)
    }
}

pub(super) struct PendingDynamicToolEntry {
    pub(super) params: DynamicToolCallParams,
    /// Present while the call can still be claimed by result delivery,
    /// timeout, or turn termination. The entry remains in the registry after
    /// the winner takes this sender so snapshots continue to advertise the
    /// request until its terminal receipt is durably appended.
    pub(super) sender: Option<oneshot::Sender<DynamicToolCallResult>>,
    pub(super) settlement_tx: watch::Sender<u64>,
    pub(super) indeterminate: bool,
}

pub(super) struct ClaimedDynamicToolSettlement {
    pub(super) params: DynamicToolCallParams,
    pub(super) sender: oneshot::Sender<DynamicToolCallResult>,
    pub(super) settlement_tx: watch::Sender<u64>,
}

pub(super) enum PendingDynamicToolClaim {
    Claimed(ClaimedDynamicToolSettlement),
    Settling(watch::Receiver<u64>),
    Indeterminate,
    Missing,
}

pub(super) enum DynamicToolTerminalOutcome {
    Resolved(DynamicToolCallResult),
    Canceled {
        reason: &'static str,
        terminal: bool,
    },
    Timeout {
        timeout: Duration,
    },
}

pub(super) struct DynamicToolSettlementAck {
    pub(super) result_accepted: bool,
}

impl RuntimeThreadManager {
    /// Helper to read the current config under RwLock.
    /// The hook executor for one workspace: global hooks from `config`,
    /// reviewed plugin hooks, then trusted and approved project hooks — the
    /// set the TUI and `exec --hooks` build. It shares this process's observer
    /// pool instead of spawning its own.
    pub fn hook_executor_for_workspace(
        &self,
        config: &Config,
        workspace: &Path,
        plugins: Option<&codewhale_plugins::PluginRegistry>,
    ) -> crate::hooks::HookExecutor {
        let hooks = crate::hooks::config::load_with_project_and_plugins(
            config.hooks_config(),
            workspace,
            plugins,
        );
        self.hook_base
            .get_or_init(|| {
                crate::hooks::HookExecutor::new(
                    codewhale_config::hooks::HooksConfig::default(),
                    workspace.to_path_buf(),
                )
            })
            .rebind(hooks, workspace.to_path_buf())
    }

    pub fn read_config(&self) -> parking_lot::RwLockReadGuard<'_, Config> {
        self.config.read()
    }

    pub(super) fn resolved_route_for_thread(
        &self,
        config: &Config,
        thread: &ThreadRecord,
    ) -> Result<ResolvedRuntimeRoute> {
        let provider_identity = self.provider_identity_for_thread(config, thread)?;
        if !thread.model.trim().eq_ignore_ascii_case("auto") {
            return resolve_runtime_thread_route_for_identity(
                config,
                &provider_identity,
                Some(&thread.model),
            );
        }

        let mut thread_config = config.clone();
        thread_config
            .scope_to_provider_identity(&provider_identity)
            .map_err(anyhow::Error::msg)?;

        let restored = self
            .store
            .list_turns_for_thread(&thread.id)?
            .into_iter()
            .rev()
            .find_map(|turn| {
                let model = turn.effective_model?.trim().to_string();
                let provider_kind = turn
                    .effective_provider
                    .filter(|provider| !provider.trim().is_empty());
                // Preserve an explicitly empty additive id so malformed
                // imported receipts fail closed instead of becoming an
                // id-less legacy custom route.
                let provider_id = turn.effective_provider_id;
                ((provider_kind.is_some() || provider_id.is_some()) && !model.is_empty())
                    .then_some((provider_kind, provider_id, model))
            });
        match restored {
            Some((restored_kind, restored_id, model)) => {
                let identity = thread_config.resolve_persisted_provider_identity(
                    restored_kind.as_deref(),
                    restored_id.as_deref(),
                );
                // The saved thread provider is the authority. An earlier Auto
                // pick is restored only when it was made on that same
                // provider; a pick from before a provider switch, from a
                // one-turn override, or from a provider no longer configured
                // is ignored and the saved provider's default route applies.
                match identity {
                    Ok(identity)
                        if identity.provider == provider_identity.provider
                            && identity.key == provider_identity.key
                            && identity.persisted_id() == provider_identity.persisted_id() =>
                    {
                        resolve_runtime_thread_route_for_identity(config, &identity, Some(&model))
                    }
                    _ => {
                        resolve_runtime_thread_route_for_identity(config, &provider_identity, None)
                    }
                }
            }
            None => resolve_runtime_thread_route_for_identity(config, &provider_identity, None),
        }
    }

    pub(super) fn provider_identity_for_thread(
        &self,
        config: &Config,
        thread: &ThreadRecord,
    ) -> Result<ProviderIdentity> {
        let has_persisted_route = thread
            .model_provider
            .as_deref()
            .is_some_and(|provider| !provider.trim().is_empty())
            || thread.model_provider_id.is_some();
        let identity = if has_persisted_route {
            config.resolve_persisted_provider_identity(
                thread.model_provider.as_deref(),
                thread.model_provider_id.as_deref(),
            )
        } else {
            config.active_provider_identity()
        };
        identity.map_err(|reason| anyhow!(reason))
    }

    /// Atomically replace the authoritative runtime config after preflighting
    /// every loaded thread's exact route. Active turns retain their immutable
    /// descriptor; the next `start_turn` resolves and installs the new route.
    pub async fn reload_config(
        &self,
        mut new_config: Config,
    ) -> Result<codewhale_config::WorkshopConfig> {
        new_config.runtime_thread_inference_unrelated = !new_config.runtime_chat_isolated;
        let _config_admission = self.config_admission.write().await;
        new_config.preserve_runtime_credentials(&self.read_config());
        let _engine_load = self.engine_load.lock().await;
        let entries: Vec<(
            String,
            EngineHandle,
            ProviderIdentity,
            String,
            Option<String>,
        )> = {
            let active = self.active.lock().await;
            active
                .engines
                .iter()
                .map(|(id, state)| {
                    (
                        id.clone(),
                        state.engine.clone(),
                        state.route_identity.clone(),
                        state.route_model.clone(),
                        state
                            .active_turn
                            .as_ref()
                            .map(|active| active.turn_id.clone()),
                    )
                })
                .collect()
        };

        let mut validated = Vec::with_capacity(entries.len());
        let mut failures = Vec::new();
        for (thread_id, engine, provider_identity, engine_model, active_turn_id) in entries {
            match resolve_runtime_thread_route_for_identity(
                &new_config,
                &provider_identity,
                Some(&engine_model),
            ) {
                Ok(route) => validated.push((thread_id, engine, route, active_turn_id)),
                // An idle engine still carries the route of its last turn,
                // which may predate a provider switch or have been a one-turn
                // override. Its next turn resolves the saved thread route, so
                // that is the route the new config has to serve.
                Err(err) if active_turn_id.is_none() => match self
                    .store
                    .load_thread(&thread_id)
                    .and_then(|thread| self.resolved_route_for_thread(&new_config, &thread))
                {
                    Ok(route) => validated.push((thread_id, engine, route, active_turn_id)),
                    Err(_) => failures.push(format!("{thread_id}: {err}")),
                },
                Err(err) => failures.push(format!("{thread_id}: {err}")),
            }
        }
        if !failures.is_empty() {
            bail!(
                "Config reload rejected because active thread routes are invalid: {}",
                failures.join("; ")
            );
        }

        // `engine_load` is still held here, so a thread cannot construct an
        // engine from the accepted config before its process-wide read/tool
        // byte limits are active. Rejected reloads leave the prior limits in
        // place.
        new_config.retain_compatible_session_keys();
        let workshop_activation = codewhale_runtime::active_workshop::install_active_workshop(
            new_config.workshop.as_ref(),
        );
        codewhale_runtime::config::initialize_cloud_facts(&new_config);
        crate::provider_catalog_live::maybe_load_persisted_cache_for_config(&new_config);
        let workflow_table = new_config.workflow_config();
        {
            let mut guard = self.config.write();
            *guard = new_config;
        }
        crate::tools::workflow::set_session_workflow_config(&self.workspace, workflow_table);

        let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
        let stream_chunk_timeout_secs = self.read_config().stream_chunk_timeout_secs();
        for (thread_id, engine, route, active_turn_id) in validated {
            let provider = route.identity.provider;
            let route_limits = known_route_limits(route.candidate.limits());
            let mut engine_compaction = runtime_compaction_config(
                &route.config,
                provider,
                &route.model,
                (route_limits, Some(route.context_window.tokens)),
                settings.auto_compact,
                codewhale_runtime::settings::Settings::auto_compact_explicitly_configured(),
                settings.auto_compact_threshold_percent,
            );
            engine_compaction.runtime_cost_owner = active_turn_id;
            let route_config = route.config;
            let _ = engine
                .send(Op::SetCompaction {
                    config: engine_compaction,
                })
                .await;
            let _ = engine
                .send(Op::SetStreamChunkTimeout {
                    timeout_secs: stream_chunk_timeout_secs,
                })
                .await;
            let _ = engine
                .send(Op::SetSubagentRuntimeConfig {
                    enabled: route_config.subagents_enabled_for_provider(&route.identity),
                    max_subagents: route_config
                        .max_subagents_for_provider(&route.identity)
                        .clamp(1, codewhale_runtime::config::MAX_SUBAGENTS),
                    launch_concurrency: route_config
                        .launch_concurrency_for_provider(&route.identity),
                    max_spawn_depth: route_config
                        .subagent_max_spawn_depth_for_provider(&route.identity),
                    api_timeout_secs: route_config
                        .subagent_api_timeout_secs_for_provider(&route.identity),
                    heartbeat_timeout_secs: route_config
                        .subagent_heartbeat_timeout_secs_for_provider(&route.identity),
                })
                .await;
            tracing::info!(
                thread_id = %thread_id,
                "Reloaded runtime controls; provider route will apply on the next turn"
            );
        }
        Ok(workshop_activation)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open(
        config: Config,
        workspace: PathBuf,
        manager_cfg: RuntimeThreadManagerConfig,
    ) -> Result<Self> {
        Self::open_inner(
            config,
            workspace,
            manager_cfg,
            None,
            None,
            crate::core::engine::EngineHostProfile::Normal,
            None,
        )
    }

    pub fn open_with_plugin_registry(
        config: Config,
        workspace: PathBuf,
        manager_cfg: RuntimeThreadManagerConfig,
        plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
    ) -> Result<Self> {
        Self::open_inner(
            config,
            workspace,
            manager_cfg,
            Some(plugin_registry),
            None,
            crate::core::engine::EngineHostProfile::Normal,
            None,
        )
    }

    pub fn open_for_session(
        config: Config,
        workspace: PathBuf,
        mut manager_cfg: RuntimeThreadManagerConfig,
        plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
        binding: Option<&RuntimeStoreBinding>,
    ) -> Result<Self> {
        if let Some(binding) = binding {
            if let Some(override_dir) = runtime_dir_override() {
                anyhow::ensure!(
                    checked_runtime_store_root(override_dir)? == binding.data_dir,
                    "Runtime directory override conflicts with the saved session's Runtime store"
                );
            }
            if binding.is_missing_session_store()? {
                // Never claim the missing owner's scope. A fresh store cannot
                // execute its queued tasks, approvals, mail or automations.
                // All recovery attempts for this conversation contend on the
                // same host lock. Random paths would let two processes mint
                // competing owners before either saves the repaired binding.
                manager_cfg.data_dir = manager_cfg
                    .data_dir
                    .with_file_name("runtime-recovered-session");
                return Self::open_inner(
                    config,
                    workspace,
                    manager_cfg,
                    Some(plugin_registry),
                    None,
                    crate::core::engine::EngineHostProfile::Normal,
                    None,
                );
            }
            binding.validate_existing_store()?;
            manager_cfg.data_dir.clone_from(&binding.data_dir);
        }
        Self::open_inner(
            config,
            workspace,
            manager_cfg,
            Some(plugin_registry),
            binding,
            crate::core::engine::EngineHostProfile::Normal,
            None,
        )
    }

    /// Mounted inactive-owner handoff must never take missing-store recovery.
    /// The captured binding is checked again under the same exclusive owner lock.
    pub fn open_existing_session(
        config: Config,
        workspace: PathBuf,
        mut manager_cfg: RuntimeThreadManagerConfig,
        plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
        binding: &RuntimeStoreBinding,
    ) -> Result<Self> {
        if let Some(override_dir) = runtime_dir_override() {
            anyhow::ensure!(
                checked_runtime_store_root(override_dir)? == binding.data_dir,
                "Runtime directory override conflicts with the saved session's Runtime store"
            );
        }
        binding.validate_existing_store()?;
        manager_cfg.data_dir.clone_from(&binding.data_dir);
        Self::open_inner(
            config,
            workspace,
            manager_cfg,
            Some(plugin_registry),
            Some(binding),
            crate::core::engine::EngineHostProfile::Normal,
            None,
        )
    }

    /// Recovery of an unbound legacy source may attach only an already
    /// existing owner. It must never create or recover an empty store.
    pub fn open_existing_session_unbound(
        config: Config,
        workspace: PathBuf,
        manager_cfg: RuntimeThreadManagerConfig,
        plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
    ) -> Result<Self> {
        let root = checked_existing_runtime_store_dir(&manager_cfg.data_dir)?;
        let owner: RuntimeStoreOwner =
            serde_json::from_str(&read_store_file(&root.join(AGENT_MAIL_OWNER_FILE))?)?;
        validated_record_id(&owner.owner_id, "Runtime owner id")?;
        let binding = RuntimeStoreBinding {
            execution_scope: runtime_execution_scope(
                &owner.owner_id,
                &root.join(EVENT_TRANSACTION_LOCK_FILE),
            ),
            data_dir: root,
        };
        Self::open_existing_session(config, workspace, manager_cfg, plugin_registry, &binding)
    }

    pub fn open_host(
        config: Config,
        workspace: PathBuf,
        manager_cfg: RuntimeThreadManagerConfig,
        plugins: Arc<codewhale_plugins::PluginRegistry>,
        host_profile: crate::core::engine::EngineHostProfile,
        preheld_lock: Option<RuntimeProcessOwnerLock>,
        binding: Option<&RuntimeStoreBinding>,
    ) -> Result<Self> {
        Self::open_inner(
            config,
            workspace,
            manager_cfg,
            Some(plugins),
            binding,
            host_profile,
            preheld_lock,
        )
    }

    pub(super) fn open_inner(
        mut config: Config,
        workspace: PathBuf,
        mut manager_cfg: RuntimeThreadManagerConfig,
        plugin_registry: Option<Arc<codewhale_plugins::PluginRegistry>>,
        binding: Option<&RuntimeStoreBinding>,
        host_profile: crate::core::engine::EngineHostProfile,
        preheld_lock: Option<RuntimeProcessOwnerLock>,
    ) -> Result<Self> {
        // A public RuntimeThreadManager owns independent native threads. They
        // may run concurrently with the interactive TUI because their events
        // cannot be projected into that TUI's attached CWC run. The private
        // Runtime Chat manager keeps its isolated marker instead and executes
        // only while its host holds the exclusive run lease.
        config.runtime_thread_inference_unrelated = !config.runtime_chat_isolated;
        let sessions_dir = match manager_cfg.sessions_dir.take() {
            Some(sessions_dir) => sessions_dir,
            None => crate::session_manager::default_sessions_dir()?,
        };
        manager_cfg.sessions_dir = Some(
            crate::session_manager::SessionManager::new(sessions_dir)?
                .sessions_dir()
                .to_path_buf(),
        );
        let process_owner_lock = match preheld_lock {
            Some(lock) => lock,
            None => RuntimeProcessOwnerLock::acquire(&manager_cfg.data_dir)?,
        };
        process_owner_lock.validate_at_root(&manager_cfg.data_dir)?;
        let process_owner_lock = Arc::new(process_owner_lock);
        // Recheck under the exclusive host lock, before store recovery can write.
        if let Some(binding) = binding {
            anyhow::ensure!(
                checked_runtime_store_root(manager_cfg.data_dir.clone())? == binding.data_dir,
                "captured Runtime store differs from the held host lease"
            );
            binding.validate_existing_store()?;
        }
        let store = RuntimeThreadStore::open(manager_cfg.data_dir.clone())?;
        codewhale_runtime::config::initialize_cloud_facts(&config);
        let (event_tx, _event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let grants = crate::core::authority::grants::GrantStore::open(&manager_cfg.data_dir);
        if let Some(reason) = grants.degraded_reason() {
            tracing::warn!(%reason, "approval grants are unavailable; remembered approvals will not apply");
        }
        let event_pump_stop = CancellationToken::new();
        let manager = Self {
            host_profile,
            config: Arc::new(parking_lot::RwLock::new(config)),
            workspace,
            plugin_registry,
            store,
            _process_owner_lock: process_owner_lock,
            config_admission: Arc::new(AsyncRwLock::new(())),
            engine_load: Arc::new(Mutex::new(())),
            shutdown_drain: Arc::new(Mutex::new(())),
            engine_workers: Arc::new(parking_lot::Mutex::new(Vec::new())),
            subagent_manager: Arc::new(Mutex::new(None)),
            turn_monitors: Arc::new(parking_lot::Mutex::new(Vec::new())),
            event_monitors: Arc::new(parking_lot::Mutex::new(Vec::new())),
            event_reader_wake: Arc::new(Notify::new()),
            event_pump_stop: event_pump_stop.clone(),
            _event_pump_owner: Arc::new(event_pump_stop.drop_guard()),
            active: Arc::new(Mutex::new(ActiveThreads::default())),
            event_emit: Arc::new(Mutex::new(())),
            projection_locks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            event_tx,
            manager_cfg,
            cancel_token: CancellationToken::new(),
            task_manager: Arc::new(parking_lot::Mutex::new(std::sync::Weak::new())),
            task_execution_lease: Arc::new(parking_lot::Mutex::new(None)),
            automations: Arc::new(parking_lot::Mutex::new(None)),
            pending_approvals: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            grants,
            pending_user_inputs: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            pending_dynamic_tools: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            pending_commands: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            recovery_receipts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            notices: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            recovery_flush: Arc::new(Mutex::new(())),
            hook_base: Arc::new(std::sync::OnceLock::new()),
            #[cfg(any(test, feature = "test-support"))]
            snapshot_test_hook: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(test, feature = "test-support"))]
            replay_test_hook: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(test, feature = "test-support"))]
            test_model_client: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(any(test, feature = "test-support"))]
            test_approval_decision_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(any(test, feature = "test-support"))]
            test_dynamic_tool_result_timeout_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        manager.recover_interrupted_state()?;
        Ok(manager)
    }

    pub fn sessions_dir(&self) -> &Path {
        self.manager_cfg
            .sessions_dir
            .as_deref()
            .expect("session directory captured before Runtime owner recovery")
    }

    /// Attach the durable task manager so model-visible task tools work inside
    /// runtime thread turns as well as interactive TUI turns.
    pub fn attach_task_manager(&self, task_manager: crate::task_manager::SharedTaskManager) {
        *self.task_manager.lock() = Arc::downgrade(&task_manager);
    }

    /// Identity of the actual Runtime store, not a model-supplied session label.
    pub fn task_execution_identity(&self) -> (String, Arc<RuntimeProcessOwnerLock>) {
        (
            runtime_execution_scope(&self.store.owner_id, &self.store.event_lock_path),
            self._process_owner_lock.clone(),
        )
    }

    #[cfg(any(unix, windows))]
    pub fn capture_control_owner(&self) -> Result<(RuntimeStoreBinding, String)> {
        let binding = self.session_store_binding();
        let path = binding.data_dir.join(RUNTIME_PROCESS_OWNER_LOCK_FILE);
        anyhow::ensure!(
            owner_lock_is_at_path(&self._process_owner_lock._file, &path)?,
            "held Runtime owner lease changed before control publication"
        );
        Ok((binding, self._process_owner_lock.generation.clone()))
    }

    pub fn is_acp_host(&self) -> bool {
        self.host_profile.is_acp()
    }

    pub fn session_store_binding(&self) -> RuntimeStoreBinding {
        RuntimeStoreBinding {
            data_dir: self
                .store
                .event_lock_path
                .parent()
                .expect("Runtime store root")
                .to_path_buf(),
            execution_scope: self.task_execution_identity().0,
        }
    }

    pub async fn close_execution_admission(&self) {
        let _admission = self.config_admission.write().await;
        self.cancel_token.cancel();
        let _loading = self.engine_load.lock().await;
    }

    /// Close admission, then drain the existing engines and terminal receipt
    /// monitors. Ownership stays attached to this Runtime until its last user
    /// drops it, including any actual execution still awaiting shutdown.
    pub async fn shutdown_and_wait(&self) -> Result<()> {
        let _drain = self.shutdown_drain.lock().await;
        self.close_execution_admission().await;
        let (engines, active_turns) = {
            let active = self.active.lock().await;
            (
                active
                    .engines
                    .values()
                    .map(|state| state.engine.clone())
                    .collect::<Vec<_>>(),
                active
                    .engines
                    .iter()
                    .filter_map(|(id, state)| {
                        state
                            .active_turn
                            .as_ref()
                            .map(|turn| (id.clone(), turn.turn_id.clone()))
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let mut failure = None;
        for (thread_id, turn_id) in active_turns {
            if let Err(error) = self.interrupt_turn(&thread_id, &turn_id).await
                && !self
                    .store
                    .load_turn(&turn_id)
                    .is_ok_and(|turn| turn.status != RuntimeTurnStatus::InProgress)
            {
                failure = Some(anyhow!("Runtime turn interruption failed: {error}"));
            }
        }
        let workers = self.engine_workers.lock().clone();
        for engine in engines
            .iter()
            .chain(workers.iter().map(|(_, engine, _)| engine))
        {
            engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
        }
        for engine in engines
            .iter()
            .chain(workers.iter().map(|(_, engine, _)| engine))
        {
            let _ = engine.send(Op::Shutdown).await;
        }
        for (_, _, worker) in &workers {
            if let Err(error) = await_retained_completion(worker).await {
                failure = Some(anyhow!("Runtime engine shutdown failed: {error}"));
            }
        }
        // Recovery reads can publish receipts too. Flushes that already passed
        // their admission check retain this mutex through all receipt writes;
        // later readers see the closed latch under the same mutex.
        {
            let _recovery = self.recovery_flush.lock().await;
        }
        loop {
            let monitors = self.turn_monitors.lock().clone();
            for monitor in monitors {
                if let Err(error) = await_retained_completion(&monitor).await {
                    failure = Some(anyhow!("Runtime terminal receipt monitor failed: {error}"));
                }
            }
            let mut monitors = self.turn_monitors.lock();
            monitors.retain(|monitor| !completion_finished(monitor));
            if monitors.is_empty() {
                break;
            }
        }
        for (owner, engine, _) in &workers {
            let authority = engine.subagent_manager();
            let live = authority.read().await.live_count_for_session(owner);
            if live > 0 {
                failure = Some(anyhow!(
                    "Runtime shutdown left {live} unsettled workers for {owner}"
                ));
            }
        }
        for engine in engines
            .iter()
            .chain(workers.iter().map(|(_, engine, _)| engine))
        {
            if let Err(error) = engine.wait_terminal_worker_events().await {
                failure = Some(anyhow!("Runtime terminal worker delivery failed: {error}"));
            }
        }
        self.event_pump_stop.cancel();
        self.event_reader_wake.notify_waiters();
        let pumps = self.event_monitors.lock().clone();
        for pump in pumps {
            if let Err(error) = await_retained_completion(&pump).await {
                failure = Some(anyhow!("Runtime Engine event receiver failed: {error}"));
            }
        }
        self.event_monitors.lock().clear();
        self.engine_workers
            .lock()
            .retain(|(_, _, worker)| !completion_finished(worker));
        let mut active = self.active.lock().await;
        active.engines.clear();
        active.lru.clear();
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn track_receipt_worker(&self, worker: tokio::task::JoinHandle<()>) {
        let mut monitors = self.turn_monitors.lock();
        monitors.retain(|monitor| !completion_finished(monitor));
        monitors.push(retained_completion(worker));
    }

    pub(super) fn ensure_accepting_execution(&self) -> Result<()> {
        if self.cancel_token.is_cancelled() {
            bail!("Runtime is shutting down; execution admission is closed");
        }
        Ok(())
    }

    /// Runtime clones retained by live Engine/tool work retain this lease too.
    /// A TaskManager cancellation timeout cannot release execution ownership.
    pub fn retain_task_execution_lease(
        &self,
        lease: Arc<crate::task_manager::TaskExecutionLease>,
    ) -> Result<()> {
        let mut current = self.task_execution_lease.lock();
        if current.is_some() {
            bail!("This Runtime already owns a task execution manager");
        }
        *current = Some(lease);
        Ok(())
    }

    /// Attach the automation manager for model-visible scheduling tools.
    pub fn attach_automation_manager(
        &self,
        automations: crate::automation_manager::SharedAutomationManager,
    ) {
        *self.automations.lock() = Some(automations);
    }

    pub fn thread_event_sink(&self) -> crate::automation_manager::ThreadEventSink {
        let manager = self.clone();
        crate::automation_manager::ThreadEventSink(Arc::new(move |thread_id, event, payload| {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                tracing::warn!(thread_id, event, "no runtime to publish thread event");
                return;
            };
            let manager = manager.clone();
            handle.spawn(async move {
                if let Err(error) = manager
                    .append_and_broadcast_event(&thread_id, None, None, event, payload)
                    .await
                {
                    tracing::warn!(thread_id, event, %error, "thread event was not recorded");
                }
            });
        }))
    }

    pub async fn start_scheduled_turn(
        &self,
        thread_id: &str,
        prompt: &str,
        task_id: &str,
    ) -> Result<Option<TurnRecord>> {
        let thread = self.get_thread(thread_id).await?;
        if thread.archived {
            bail!("thread {thread_id} is archived");
        }
        if self
            .active
            .lock()
            .await
            .engines
            .get(thread_id)
            .and_then(|state| state.active_turn.as_ref())
            .is_some()
        {
            return Ok(None);
        }
        let request = StartTurnRequest {
            prompt: prompt.to_string(),
            ..Default::default()
        };
        match self
            .start_turn_with_source(
                thread_id,
                request,
                RuntimeTurnInputSource::Scheduled {
                    task_id: task_id.to_string(),
                },
                None,
                false,
            )
            .await
        {
            Ok((turn, _replayed)) => Ok(Some(turn)),
            Err(error) if error.to_string().contains("already has an active") => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Mints an approval identifier that no provider can predict or collide
    /// with. Every `approval_id` an external client ever sees comes from here,
    /// so `approval_id` never carries a provider tool-call ID and a client can
    /// echo it without inspecting where it came from.
    pub(super) fn mint_approval_id() -> String {
        format!("approval_{}", Uuid::new_v4().simple())
    }

    pub(super) fn register_pending_approval(
        &self,
        thread_id: &str,
        mut request: PendingApprovalRequest,
    ) -> (String, oneshot::Receiver<ExternalApprovalDecision>) {
        // Provider call IDs can repeat across sessions and responses. Mint the
        // external capability here so delivery and cleanup can only settle this
        // registration, including after the provider reuses a previous ID.
        let mut pending = self.pending_approvals.lock();
        loop {
            let id = Self::mint_approval_id();
            if let std::collections::hash_map::Entry::Vacant(entry) = pending.entry(id.clone()) {
                request.id = id.clone();
                let (tx, rx) = oneshot::channel();
                entry.insert(PendingApprovalEntry {
                    thread_id: thread_id.to_string(),
                    request,
                    sender: tx,
                });
                return (id, rx);
            }
        }
    }

    pub(super) fn cancel_pending_approval(&self, approval_id: &str) {
        self.pending_approvals.lock().remove(approval_id);
    }

    pub(super) fn user_input_settlement(&self, thread_id: &str) -> RuntimeUserInputSettlement {
        RuntimeUserInputSettlement {
            thread_id: thread_id.to_string(),
            pending_user_inputs: self.pending_user_inputs.clone(),
            store: self.store.clone(),
            projection: self.projection_lock(thread_id),
            event_emit: self.event_emit.clone(),
            event_tx: self.event_tx.clone(),
            owner: Arc::downgrade(&self._process_owner_lock),
        }
    }

    pub(super) fn register_pending_user_input(
        &self,
        thread_id: &str,
        request: PendingUserInputRequest,
    ) {
        let (settlement_tx, _settlement_rx) = watch::channel(0);
        self.pending_user_inputs.lock().insert(
            (thread_id.to_string(), request.id.clone()),
            PendingUserInputEntry {
                request,
                settling: false,
                settlement_tx,
                indeterminate: false,
            },
        );
    }

    pub(super) fn discard_pending_user_input_registration(&self, thread_id: &str, input_id: &str) {
        let key = (thread_id.to_string(), input_id.to_string());
        let mut pending = self.pending_user_inputs.lock();
        if pending.get(&key).is_some_and(|entry| !entry.settling) {
            pending.remove(&key);
        }
    }

    pub(super) fn claim_pending_user_inputs_for_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(Vec<PendingUserInputRequest>, Vec<watch::Receiver<u64>>)> {
        let mut pending = self.pending_user_inputs.lock();
        if let Some((_, entry)) = pending.iter().find(|((pending_thread_id, _), entry)| {
            pending_thread_id == thread_id
                && entry.request.turn_id == turn_id
                && entry.indeterminate
        }) {
            bail!(
                "User-input request '{}' has an indeterminate terminal receipt; inspect Runtime storage before completing turn '{turn_id}'",
                entry.request.id
            );
        }
        let mut claims = Vec::new();
        let mut settling = Vec::new();
        for ((pending_thread_id, _), entry) in pending.iter_mut() {
            if pending_thread_id != thread_id || entry.request.turn_id != turn_id {
                continue;
            }
            if entry.settling {
                settling.push(entry.settlement_tx.subscribe());
                continue;
            }
            entry.settling = true;
            claims.push(entry.request.clone());
        }
        Ok((claims, settling))
    }

    pub(super) fn pending_requests_for_thread(
        &self,
        thread_id: &str,
    ) -> (Vec<PendingApprovalRequest>, Vec<PendingUserInputRequest>) {
        let mut approvals = self
            .pending_approvals
            .lock()
            .values()
            .filter(|entry| entry.thread_id == thread_id)
            .map(|entry| entry.request.clone())
            .collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.turn_id
                .cmp(&right.turn_id)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut user_inputs = self
            .pending_user_inputs
            .lock()
            .iter()
            .filter(|((pending_thread_id, _), _)| pending_thread_id == thread_id)
            .map(|(_, entry)| entry.request.clone())
            .collect::<Vec<_>>();
        user_inputs.sort_by(|left, right| {
            left.turn_id
                .cmp(&right.turn_id)
                .then_with(|| left.id.cmp(&right.id))
        });
        (approvals, user_inputs)
    }

    pub(super) fn register_pending_dynamic_tool(
        &self,
        params: DynamicToolCallParams,
    ) -> Result<oneshot::Receiver<DynamicToolCallResult>> {
        let (tx, rx) = oneshot::channel();
        let (settlement_tx, _settlement_rx) = watch::channel(0);
        let mut pending = self.pending_dynamic_tools.lock();
        if pending.len() >= MAX_PENDING_DYNAMIC_TOOL_CALLS {
            bail!(
                "Runtime has reached the pending dynamic tool call limit ({MAX_PENDING_DYNAMIC_TOOL_CALLS})"
            );
        }
        if pending.contains_key(&params.call_id) {
            bail!("Dynamic tool call '{}' is already pending", params.call_id);
        }
        pending.insert(
            params.call_id.clone(),
            PendingDynamicToolEntry {
                params,
                sender: Some(tx),
                settlement_tx,
                indeterminate: false,
            },
        );
        Ok(rx)
    }

    /// Atomically select the single terminal owner for a dynamic tool call.
    ///
    /// The registry entry intentionally remains present with an empty sender
    /// while the winner commits its receipt. `get_thread_detail` therefore
    /// cannot publish a cursor that has neither the pending request nor the
    /// terminal event, and competing result/timeout/cancel paths cannot claim
    /// the same call twice.
    pub(super) fn claim_pending_dynamic_tool(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
    ) -> PendingDynamicToolClaim {
        let mut pending = self.pending_dynamic_tools.lock();
        let Some(entry) = pending.get_mut(call_id) else {
            return PendingDynamicToolClaim::Missing;
        };
        let matches_route = entry.params.thread_id == thread_id && entry.params.turn_id == turn_id;
        if !matches_route {
            return PendingDynamicToolClaim::Missing;
        }
        if entry.indeterminate {
            return PendingDynamicToolClaim::Indeterminate;
        }
        match entry.sender.take() {
            Some(sender) => PendingDynamicToolClaim::Claimed(ClaimedDynamicToolSettlement {
                params: entry.params.clone(),
                sender,
                settlement_tx: entry.settlement_tx.clone(),
            }),
            None => PendingDynamicToolClaim::Settling(entry.settlement_tx.subscribe()),
        }
    }

    pub(super) fn remove_pending_dynamic_tool(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
    ) -> Option<PendingDynamicToolEntry> {
        let mut pending = self.pending_dynamic_tools.lock();
        let matches_route = pending.get(call_id).is_some_and(|entry| {
            entry.params.thread_id == thread_id && entry.params.turn_id == turn_id
        });
        matches_route.then(|| pending.remove(call_id)).flatten()
    }

    fn pending_dynamic_tool_calls_for_thread(&self, thread_id: &str) -> Vec<DynamicToolCallParams> {
        let mut calls = self
            .pending_dynamic_tools
            .lock()
            .values()
            .filter(|entry| entry.params.thread_id == thread_id)
            .map(|entry| entry.params.clone())
            .collect::<Vec<_>>();
        calls.sort_by(|left, right| {
            left.turn_id
                .cmp(&right.turn_id)
                .then_with(|| left.call_id.cmp(&right.call_id))
        });
        calls
    }

    pub(super) fn claim_or_watch_pending_dynamic_tools_for_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> (
        Vec<ClaimedDynamicToolSettlement>,
        Vec<watch::Receiver<u64>>,
        bool,
    ) {
        let mut pending = self.pending_dynamic_tools.lock();
        let mut claims = Vec::new();
        let mut settling = Vec::new();
        let mut indeterminate = false;
        for entry in pending
            .values_mut()
            .filter(|entry| entry.params.thread_id == thread_id && entry.params.turn_id == turn_id)
        {
            if entry.indeterminate {
                indeterminate = true;
                continue;
            }
            match entry.sender.take() {
                Some(sender) => claims.push(ClaimedDynamicToolSettlement {
                    params: entry.params.clone(),
                    sender,
                    settlement_tx: entry.settlement_tx.clone(),
                }),
                None => settling.push(entry.settlement_tx.subscribe()),
            }
        }
        (claims, settling, indeterminate)
    }

    pub(super) fn finish_dynamic_tool_settlement(&self, params: &DynamicToolCallParams) {
        let mut pending = self.pending_dynamic_tools.lock();
        let can_remove = pending.get(&params.call_id).is_some_and(|entry| {
            entry.params.thread_id == params.thread_id
                && entry.params.turn_id == params.turn_id
                && entry.sender.is_none()
        });
        if can_remove {
            pending.remove(&params.call_id);
        }
    }

    pub(super) fn restore_dynamic_tool_claim(&self, claim: ClaimedDynamicToolSettlement) {
        let settlement_tx = claim.settlement_tx.clone();
        let mut pending = self.pending_dynamic_tools.lock();
        if let Some(entry) = pending.get_mut(&claim.params.call_id)
            && entry.params.thread_id == claim.params.thread_id
            && entry.params.turn_id == claim.params.turn_id
            && entry.sender.is_none()
        {
            entry.sender = Some(claim.sender);
            entry.indeterminate = false;
        }
        settlement_tx.send_modify(|epoch| *epoch = epoch.saturating_add(1));
    }

    fn mark_dynamic_tool_claim_indeterminate(&self, claim: &ClaimedDynamicToolSettlement) {
        let mut pending = self.pending_dynamic_tools.lock();
        if let Some(entry) = pending.get_mut(&claim.params.call_id)
            && entry.params.thread_id == claim.params.thread_id
            && entry.params.turn_id == claim.params.turn_id
            && entry.sender.is_none()
        {
            entry.indeterminate = true;
        }
        claim
            .settlement_tx
            .send_modify(|epoch| *epoch = epoch.saturating_add(1));
    }

    pub fn deliver_external_approval(
        &self,
        approval_id: &str,
        decision: ExternalApprovalDecision,
    ) -> bool {
        let entry = self.pending_approvals.lock().remove(approval_id);
        match entry {
            Some(entry) => entry.sender.send(decision).is_ok(),
            None => false,
        }
    }

    pub async fn deliver_dynamic_tool_result(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
        result: DynamicToolCallResult,
    ) -> Result<bool> {
        let admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let claim = match self.claim_pending_dynamic_tool(thread_id, turn_id, call_id) {
            PendingDynamicToolClaim::Claimed(claim) => claim,
            PendingDynamicToolClaim::Settling(_) | PendingDynamicToolClaim::Missing => {
                return Ok(false);
            }
            PendingDynamicToolClaim::Indeterminate => {
                bail!(
                    "Dynamic tool call '{call_id}' has an indeterminate terminal receipt; inspect Runtime storage before retrying"
                );
            }
        };
        let ack =
            self.spawn_dynamic_tool_settlement(claim, DynamicToolTerminalOutcome::Resolved(result));
        drop(admission);
        Ok(Self::await_dynamic_tool_settlement(ack)
            .await?
            .result_accepted)
    }

    pub async fn submit_user_input(
        &self,
        thread_id: &str,
        input_id: &str,
        response: crate::tools::user_input::UserInputResponse,
    ) -> Result<bool> {
        let admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let engine = {
            let active = self.active.lock().await;
            let Some(state) = active.engines.get(thread_id) else {
                bail!("thread '{thread_id}' not found");
            };
            state.engine.clone()
        };
        let request = match self
            .user_input_settlement(thread_id)
            .claim_pending_user_input(thread_id, input_id, None, Some(&response))?
        {
            PendingUserInputClaim::Claimed(request) => request,
            PendingUserInputClaim::Missing | PendingUserInputClaim::Settling => {
                return Ok(false);
            }
            PendingUserInputClaim::Indeterminate => {
                bail!(
                    "User-input request '{input_id}' has an indeterminate terminal receipt; inspect Runtime storage before retrying"
                );
            }
        };

        // This child task deliberately outlives the HTTP future. Once a
        // request is claimed, client disconnect/cancellation cannot strand it
        // between durable acceptance and engine delivery.
        let manager = self.clone();
        let thread_id = thread_id.to_string();
        let (ack_tx, ack_rx) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let result = manager
                .user_input_settlement(&thread_id)
                .settle_claimed_user_input(
                    &thread_id,
                    Some(engine),
                    request,
                    UserInputTerminalOutcome::Answered(response),
                )
                .await;
            let _ = ack_tx.send(result);
        });
        self.track_receipt_worker(worker);
        drop(admission);
        ack_rx.await.context("User-input settlement task failed")?
    }

    pub async fn cancel_user_input(&self, thread_id: &str, input_id: &str) -> Result<bool> {
        let admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let engine = {
            let active = self.active.lock().await;
            let Some(state) = active.engines.get(thread_id) else {
                bail!("thread '{thread_id}' not found");
            };
            state.engine.clone()
        };
        let request = match self
            .user_input_settlement(thread_id)
            .claim_pending_user_input(thread_id, input_id, None, None)?
        {
            PendingUserInputClaim::Claimed(request) => request,
            PendingUserInputClaim::Missing | PendingUserInputClaim::Settling => {
                return Ok(false);
            }
            PendingUserInputClaim::Indeterminate => {
                bail!(
                    "User-input request '{input_id}' has an indeterminate terminal receipt; inspect Runtime storage before retrying"
                );
            }
        };
        let manager = self.clone();
        let thread_id = thread_id.to_string();
        let (ack_tx, ack_rx) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let result = manager
                .user_input_settlement(&thread_id)
                .settle_claimed_user_input(
                    &thread_id,
                    Some(engine),
                    request,
                    UserInputTerminalOutcome::Canceled { terminal: false },
                )
                .await;
            let _ = ack_tx.send(result);
        });
        self.track_receipt_worker(worker);
        drop(admission);
        ack_rx
            .await
            .context("User-input cancellation task failed")?
    }

    async fn publish_user_input_request(
        &self,
        thread_id: &str,
        turn_id: &str,
        engine: &EngineHandle,
        id: String,
        request: crate::tools::user_input::UserInputRequest,
    ) -> Result<()> {
        let projection_lock = self.projection_lock(thread_id);
        let projection = projection_lock.lock().await;
        self.register_pending_user_input(
            thread_id,
            PendingUserInputRequest {
                id: id.clone(),
                turn_id: turn_id.to_string(),
                request: request.clone(),
            },
        );
        if let Err(err) = self
            .emit_event(
                thread_id,
                Some(turn_id),
                None,
                "user_input.required",
                json!({
                    "id": id,
                    "request": request,
                }),
            )
            .await
        {
            self.discard_pending_user_input_registration(thread_id, &id);
            drop(projection);
            let _ = engine.cancel_user_input(&id).await;
            return Err(err);
        }
        drop(projection);
        Ok(())
    }

    pub(super) async fn settle_user_input_for_completed_tool(
        &self,
        thread_id: &str,
        turn_id: &str,
        input_id: &str,
    ) -> Result<()> {
        let request = loop {
            match self
                .user_input_settlement(thread_id)
                .claim_pending_user_input(thread_id, input_id, Some(turn_id), None)?
            {
                PendingUserInputClaim::Claimed(request) => break request,
                PendingUserInputClaim::Missing => return Ok(()),
                PendingUserInputClaim::Settling => {
                    // An API answer/cancel may still fail its durable append
                    // and restore the request. Wait for that owner, then
                    // retry rather than leaving the completed waiter pending.
                    let progress = self
                        .pending_user_inputs
                        .lock()
                        .get(&(thread_id.to_string(), input_id.to_string()))
                        .filter(|entry| {
                            entry.request.turn_id == turn_id
                                && entry.settling
                                && !entry.indeterminate
                        })
                        .map(|entry| entry.settlement_tx.subscribe());
                    if let Some(mut progress) = progress {
                        let _ = progress.changed().await;
                    }
                }
                PendingUserInputClaim::Indeterminate => {
                    bail!(
                        "User-input request '{input_id}' has an indeterminate terminal receipt; inspect Runtime storage before completing its tool"
                    );
                }
            }
        };
        // Engine already ended this waiter (timeout, cancellation or failure).
        // Reuse durable request settlement without sending a stale decision
        // back to it. The tool result carries the actual reason it ended.
        self.user_input_settlement(thread_id)
            .settle_claimed_user_input(
                thread_id,
                None,
                request,
                UserInputTerminalOutcome::Canceled { terminal: false },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn settle_user_inputs_for_terminal_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        engine: Option<EngineHandle>,
    ) -> Result<()> {
        loop {
            let (requests, settling) =
                self.claim_pending_user_inputs_for_turn(thread_id, turn_id)?;
            for request in requests {
                self.user_input_settlement(thread_id)
                    .settle_claimed_user_input(
                        thread_id,
                        engine.clone(),
                        request,
                        UserInputTerminalOutcome::Canceled { terminal: true },
                    )
                    .await?;
            }
            if settling.is_empty() {
                return Ok(());
            }
            for mut progress in settling {
                let _ = progress.changed().await;
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_approvals_count(&self) -> usize {
        self.pending_approvals.lock().len()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_dynamic_tools_count(&self) -> usize {
        self.pending_dynamic_tools.lock().len()
    }

    /// Registers a pending approval and returns `(minted approval id, waiter)`.
    /// `label` is only a description and a stand-in provider call ID: the
    /// caller cannot choose the approval ID, exactly as a provider cannot.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_pending_approval_for_test(
        &self,
        label: &str,
    ) -> (String, oneshot::Receiver<ExternalApprovalDecision>) {
        self.register_pending_approval_for_thread_for_test("test-thread", label)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn register_pending_approval_for_thread_for_test(
        &self,
        thread_id: &str,
        label: &str,
    ) -> (String, oneshot::Receiver<ExternalApprovalDecision>) {
        self.register_pending_approval(
            thread_id,
            PendingApprovalRequest {
                // Overwritten by the mint; a caller-supplied ID is never honored.
                id: String::new(),
                turn_id: "test-turn".to_string(),
                tool_name: "test-tool".to_string(),
                input: None,
                description: format!("test approval {label}"),
                intent_summary: None,
                // Stands in for the provider's raw call ID so tests can prove
                // the correlator is visible and still not deliverable.
                tool_call_id: Some(label.to_string()),
                summary: None,
            },
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn register_pending_user_input_for_thread_for_test(&self, thread_id: &str, input_id: &str) {
        self.register_pending_user_input(
            thread_id,
            PendingUserInputRequest {
                id: input_id.to_string(),
                turn_id: "test-turn".to_string(),
                request: crate::tools::user_input::UserInputRequest {
                    require_typed_answers: false,
                    questions: Vec::new(),
                },
            },
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn register_pending_dynamic_tool_for_test(
        &self,
        thread_id: &str,
        turn_id: &str,
        call_id: &str,
    ) -> Result<oneshot::Receiver<DynamicToolCallResult>> {
        self.register_pending_dynamic_tool(DynamicToolCallParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            call_id: call_id.to_string(),
            namespace: Some("test".to_string()),
            tool: "test_tool".to_string(),
            arguments: json!({ "input": "test" }),
        })
    }

    /// Live session approval grants on `thread_id`, oldest first.
    #[must_use]
    pub fn approval_grants_for_thread(&self, thread_id: &str) -> Vec<RuntimeApprovalGrant> {
        self.grants
            .thread_grants(thread_id)
            .iter()
            .map(RuntimeApprovalGrant::from_grant)
            .collect()
    }

    /// Record "allow for this conversation" as a grant scoped to the tool and
    /// its argument class (E1). The thread's permission posture is untouched:
    /// promoting a one-call approval to Full Access is what this replaced.
    ///
    /// Returns `None` (nothing recorded) when the thread is archived or gone.
    /// Archiving has no quiescence gate, so a prompt raised before archive can
    /// be answered after it; recording that grant would outlive the archive
    /// that was meant to end it. The approved call itself still runs.
    pub(super) async fn add_session_grant(
        &self,
        thread_id: &str,
        turn_id: &str,
        tool_name: &str,
        scope: &str,
        summary: &str,
    ) -> Option<RuntimeApprovalGrant> {
        use crate::core::authority::grants::{
            GrantAppliesTo, GrantDecider, GrantEffect, GrantLimits, GrantMatcher, GrantScope,
            GrantSurface, NewGrant,
        };
        let grant = {
            // Same order as update_thread's archive path (thread_mutation,
            // then the grant store), so archive and record cannot interleave.
            let _thread_mutation = self.store.thread_mutation.lock();
            let live = self
                .store
                .load_thread(thread_id)
                .is_ok_and(|thread| !thread.archived);
            if !live {
                return None;
            }
            if let Some(existing) = self
                .grants
                .thread_grants(thread_id)
                .iter()
                .find(|grant| grant.matcher.grouping_key.as_deref() == Some(scope))
            {
                return Some(RuntimeApprovalGrant::from_grant(existing));
            }
            let created = self.grants.create(NewGrant {
                effect: GrantEffect::Allow,
                scope: GrantScope::Thread {
                    thread_id: thread_id.to_string(),
                },
                matcher: GrantMatcher {
                    tool_class: grant_tool_class(tool_name),
                    grouping_key: Some(scope.to_string()),
                    ..GrantMatcher::default()
                },
                tool_name: tool_name.to_string(),
                redacted_summary: summary.to_string(),
                limits: GrantLimits::default(),
                applies_to: GrantAppliesTo::default(),
                principal: "authenticated_client".to_string(),
                surface: GrantSurface::Runtime,
                decider: GrantDecider::HumanCard,
                source_approval_id: None,
            });
            match created {
                Ok(grant) => RuntimeApprovalGrant::from_grant(&grant),
                Err(error) => {
                    tracing::warn!(target: "approval", %error, "session grant was not recorded");
                    return None;
                }
            }
        };
        self.emit_event(
            thread_id,
            Some(turn_id),
            None,
            "approval.grant_added",
            json!({ "grant": grant.clone() }),
        )
        .await
        .ok();
        Some(grant)
    }

    /// End every session grant on `thread_id` and return them. Archiving or
    /// deleting a thread calls this, so a grant never outlives the
    /// conversation it was given in.
    pub(super) fn take_approval_grants(&self, thread_id: &str) -> Vec<RuntimeApprovalGrant> {
        self.grants
            .revoke_thread(thread_id, "runtime", "thread archived or deleted")
            .iter()
            .map(RuntimeApprovalGrant::from_grant)
            .collect()
    }

    /// Revoke one session grant. Returns `false` when the thread holds no
    /// grant with that id. The next matching call prompts again.
    pub async fn revoke_approval_grant(&self, thread_id: &str, grant_id: &str) -> Result<bool> {
        let scope = crate::core::authority::grants::GrantScope::Thread {
            thread_id: thread_id.to_string(),
        };
        let Some(revoked) = self.grants.revoke(
            grant_id,
            Some(&scope),
            "authenticated_client",
            "revoked by an authenticated client",
        ) else {
            return Ok(false);
        };
        let revoked = RuntimeApprovalGrant::from_grant(&revoked);
        self.emit_event(
            thread_id,
            None,
            None,
            "approval.grant_revoked",
            json!({ "grant": revoked }),
        )
        .await?;
        Ok(true)
    }

    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<RuntimeEventRecord> {
        self.event_tx.subscribe()
    }

    /// Emit a durable `thread_goal_updated` event while its revision is current.
    pub async fn emit_goal_updated_event(
        &self,
        thread_id: &str,
        goal: codewhale_protocol::ThreadGoal,
    ) -> Result<Option<RuntimeEventRecord>> {
        let thread_id = thread_id.to_string();
        self.with_goal_publication(move |manager| {
            let Some(mut current) = manager.store.load_goal(&thread_id)? else {
                return Ok(None);
            };
            current.tokens_used = current.tokens_used.max(goal.tokens_used);
            current.time_used_seconds = current.time_used_seconds.max(goal.time_used_seconds);
            current.continuation_count = current.continuation_count.max(goal.continuation_count);
            if current != goal {
                return Ok(None);
            }
            manager
                .emit_goal_event_under_lock(&thread_id, Some(&goal))
                .map(Some)
        })
        .await
    }

    pub(super) async fn with_goal_publication<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let emit_order = self.event_emit.clone().lock_owned().await;
        let manager = self.clone();
        tokio::task::spawn_blocking(move || {
            let _emit_order = emit_order;
            let _goal_mutation = manager.store.goal_mutation.lock();
            operation(&manager)
        })
        .await
        .context("goal publication task panicked")?
    }

    pub(super) fn emit_goal_event_under_lock(
        &self,
        thread_id: &str,
        goal: Option<&codewhale_protocol::ThreadGoal>,
    ) -> Result<RuntimeEventRecord> {
        let (event, payload) = match goal {
            Some(goal) => (
                "thread_goal_updated",
                json!({ "kind": "thread_goal_updated", "goal": goal }),
            ),
            None => (
                "thread_goal_cleared",
                json!({ "kind": "thread_goal_cleared", "thread_id": thread_id }),
            ),
        };
        let record = self.store.append_event_transaction(
            thread_id.to_string(),
            None,
            None,
            event.to_string(),
            payload,
            EVENT_TRANSACTION_LOCK_TIMEOUT,
        )?;
        let _ = self.event_tx.send(record.clone());
        Ok(record)
    }

    /// Return the persistent goal for a thread, or `Ok(None)` if none exists.
    pub async fn get_goal(
        &self,
        thread_id: &str,
    ) -> Result<Option<codewhale_protocol::ThreadGoal>> {
        let requested_id = thread_id.to_string();
        let store = self.store.clone();
        let mut goal = tokio::task::spawn_blocking(move || store.load_goal(&requested_id))
            .await
            .context("goal load task panicked")??;
        // Live usage is a projection only. Terminal settlement remains the
        // single durable accrual, so polling cannot double-count spend.
        let active = self.active.lock().await;
        if let Some(goal) = goal.as_mut()
            && let Some(turn) = active
                .engines
                .get(thread_id)
                .and_then(|state| state.active_turn.as_ref())
            && turn.goal_id.as_deref() == Some(goal.goal_id.as_str())
            && let Some(progress) = turn.goal_progress.as_ref()
            && progress.objective.as_deref() == Some(goal.objective.as_str())
            && progress
                .goal_id
                .as_deref()
                .is_none_or(|id| id == goal.goal_id)
        {
            goal.tokens_used = goal
                .tokens_used
                .max(i64::try_from(progress.tokens_used).unwrap_or(i64::MAX));
            goal.time_used_seconds = goal
                .time_used_seconds
                .max(i64::try_from(progress.time_used_seconds).unwrap_or(i64::MAX));
            goal.continuation_count = goal
                .continuation_count
                .max(i64::from(progress.continuation_count));
        }
        Ok(goal)
    }

    /// Every readable goal record across threads, with live usage projected.
    /// Unreadable records are skipped so one corrupt file cannot hide the rest.
    pub async fn list_goals(&self) -> Result<Vec<codewhale_protocol::ThreadGoal>> {
        let store = self.store.clone();
        let ids = tokio::task::spawn_blocking(move || store.list_goal_thread_ids())
            .await
            .context("goal list task panicked")??;
        let mut goals = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(Some(goal)) = self.get_goal(&id).await {
                goals.push(goal);
            }
        }
        Ok(goals)
    }

    pub async fn thread_exists(&self, thread_id: &str) -> bool {
        let thread_id = thread_id.to_string();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.thread_exists(&thread_id))
            .await
            .unwrap_or(false)
    }

    /// Persist (create or replace) and publish the goal for a thread.
    pub async fn save_goal(&self, goal: codewhale_protocol::ThreadGoal) -> Result<()> {
        self.with_goal_publication(move |manager| {
            manager.store.save_goal_under_lock(&goal)?;
            manager.emit_goal_event_under_lock(&goal.thread_id, Some(&goal))?;
            Ok(())
        })
        .await
    }

    /// Remove and publish the goal for a thread. Returns `true` if a goal existed.
    pub async fn remove_goal(&self, thread_id: &str) -> Result<bool> {
        let thread_id = thread_id.to_string();
        self.with_goal_publication(move |manager| {
            let deleted = manager.store.delete_goal_under_lock(&thread_id)?;
            if deleted {
                manager.emit_goal_event_under_lock(&thread_id, None)?;
            }
            Ok(deleted)
        })
        .await
    }

    /// Transition the goal status through a revision-checked mutation. A
    /// stale load-then-save could otherwise overwrite a concurrent PUT
    /// replacement or DELETE, or a concurrent terminal transition; here the
    /// write commits only when the record still carries the goal id *and* the
    /// status the caller read. Returns the updated goal, or `Ok(None)` when
    /// the goal changed or vanished since the read.
    pub async fn transition_goal_status(
        &self,
        thread_id: &str,
        expected_goal_id: &str,
        expected_status: codewhale_protocol::ThreadGoalStatus,
        status: codewhale_protocol::ThreadGoalStatus,
    ) -> Result<Option<codewhale_protocol::ThreadGoal>> {
        let thread_id = thread_id.to_string();
        let expected_goal_id = expected_goal_id.to_string();
        self.with_goal_publication(move |manager| {
            let updated = manager.store.update_goal_if_revision_under_lock(
                &thread_id,
                &expected_goal_id,
                Some(expected_status),
                |goal| {
                    goal.status = status;
                    goal.updated_at = chrono::Utc::now().timestamp();
                },
            )?;
            if let Some(goal) = updated.as_ref() {
                manager.emit_goal_event_under_lock(&thread_id, Some(goal))?;
            }
            Ok(updated)
        })
        .await
    }

    /// Activate a persisted `Active` goal: make sure the engine carries the
    /// goal state, then dispatch the kickoff turn while the thread is idle.
    /// A busy thread is left alone — the running turn already carries the
    /// persisted goal, and its terminal settlement arms the next pass.
    pub async fn activate_thread_goal(&self, thread_id: &str) -> Result<()> {
        let Some(goal) = self.store.load_goal(thread_id)? else {
            return Ok(());
        };
        if !matches!(goal.status, codewhale_protocol::ThreadGoalStatus::Active) {
            return Ok(());
        }
        {
            let active = self.active.lock().await;
            if let Some(state) = active.engines.get(thread_id)
                && state.active_turn.is_some()
            {
                // A PUT during a running goal replaces its revision. Park the
                // obsolete within-turn loop; settlement admits the new goal.
                let current = self.store.load_goal(thread_id)?;
                state.engine.sync_runtime_goal_control(current.as_ref())?;
                return Ok(());
            }
        }
        let objective = goal.objective.trim().to_string();
        if objective.is_empty() {
            return Ok(());
        }
        self.start_goal_turn(thread_id, objective, 0, goal).await?;
        Ok(())
    }

    /// Push a durable goal lifecycle transition into a cached engine so its
    /// prompt surface and tool gates follow the store. Engines are not
    /// loaded for this; a later load re-derives the state from the record.
    pub async fn sync_engine_goal_status(&self, thread_id: &str) -> Result<()> {
        let active = self.active.lock().await;
        if let Some(state) = active.engines.get(thread_id) {
            // Read the latest store revision while admission is excluded. A
            // delayed DELETE/complete response must never stop a newer goal.
            let goal = self.store.load_goal(thread_id)?;
            state.engine.sync_runtime_goal_control(goal.as_ref())?;
        }
        Ok(())
    }

    /// Claim one host-driven goal turn with the standard durable machinery.
    /// The caller renders the prompt (kickoff objective or continuation
    /// frame); `continuation_index` is 0 for the kickoff pass.
    pub(super) async fn start_goal_turn(
        &self,
        thread_id: &str,
        prompt: String,
        continuation_index: u32,
        expected_goal: codewhale_protocol::ThreadGoal,
    ) -> Result<TurnRecord> {
        let req = StartTurnRequest {
            account_model_owner: None,
            profile_constitution: None,
            expected_workspace: None,
            max_output_tokens: None,
            prompt,
            images: Vec::new(),
            operation_key: None,
            input_summary: Some(if continuation_index == 0 {
                "goal kickoff".to_string()
            } else {
                format!("goal continuation pass #{continuation_index}")
            }),
            model: None,
            reasoning_effort: None,
            allowed_tools: None,
            mode: None,
            permission_posture: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            dynamic_tools: Vec::new(),
            environment_id: None,
            model_provider: None,
            model_provider_id: None,
        };
        self.start_turn_with_source(
            thread_id,
            req,
            RuntimeTurnInputSource::GoalContinuation {
                continuation_index,
                expected_goal: Box::new(expected_goal),
            },
            None,
            false,
        )
        .await
        .map(|(turn, _replayed)| turn)
    }

    /// Terminal goal settlement for one finished turn.
    ///
    /// Host-managed runtime engines never self-continue (the interactive
    /// sibling schedules `Op::ContinueGoal` in-process), so the runtime host
    /// owns the durable loop here: turn usage is written back to the goal
    /// record, the model's terminal decision (`update_goal` complete/blocked)
    /// is mirrored from the engine snapshot, and the next pass is armed after
    /// the configured quiet period while the goal is still Active.
    pub(super) async fn settle_thread_goal_after_turn(
        &self,
        thread_id: &str,
        turn: &TurnRecord,
        engine_goal: Option<crate::tools::goal::GoalSnapshot>,
        admitted_goal_id: Option<&str>,
        turn_tool_catalog: Option<&[codewhale_core::request::Tool]>,
    ) {
        let Some(admitted_goal_id) = admitted_goal_id else {
            return;
        };
        let mut continue_after: Option<u64> = None;
        let updated = self.store.update_goal_if_revision(thread_id, admitted_goal_id, None, |goal| {
        // Accrue this turn's provider spend onto the durable counters. The
        // engine tracks the same totals in memory; the record is the
        // cross-restart authority.
        let token_delta = turn
            .usage
            .as_ref()
            .map(|usage| i64::from(usage.input_tokens) + i64::from(usage.output_tokens))
            .unwrap_or(0);
        let time_delta_seconds = turn.duration_ms.map(|ms| (ms / 1000) as i64).unwrap_or(0);
        if token_delta > 0 || time_delta_seconds > 0 {
            goal.tokens_used = goal.tokens_used.saturating_add(token_delta);
            goal.time_used_seconds = goal.time_used_seconds.saturating_add(time_delta_seconds);
            goal.updated_at = chrono::Utc::now().timestamp();
        }

        if let Some(snapshot) = engine_goal.as_ref() {
            merge_engine_goal_progress(goal, snapshot);
        }

        // Only a cleanly completed pass continues the loop. Failed or
        // interrupted passes leave the goal Active for an explicit resume
        // (PUT, or the next user turn).
        if turn.status == RuntimeTurnStatus::Completed
            && matches!(goal.status, codewhale_protocol::ThreadGoalStatus::Active)
        {
            let max_continuations = i64::from(self.read_config().goal_max_continuations());
            // The engine stops its own intra-turn loop at this cap with only
            // a status line (`goal_continuation_allowed` → Stop), so a
            // snapshot at or beyond the cap means the engine already refused
            // the next pass. Mirror that stop as a host-side pause instead of
            // arming passes that can only trip the same gate; an explicit
            // PUT resumes the goal.
            let engine_hit_cap = engine_goal.as_ref().is_some_and(|snapshot| {
                max_continuations != 0
                    && i64::from(snapshot.continuation_count) >= max_continuations
            });
            // A turn whose catalog lacked `update_goal` (an `allowed_tools`
            // restriction, or an `isolated_chat` engine) has no tool with
            // which the model could ever report complete/blocked, so the
            // engine's own continuation hook skips such turns
            // (`goal_continuation_message_if_needed`). The host mirrors that
            // precondition: re-arming would spend one provider call per pass
            // with no terminal path. A missing catalog means the turn never
            // reached the request seam, so the same conservative gate holds.
            let update_goal_available = turn_tool_catalog
                .is_some_and(|catalog| catalog.iter().any(|tool| tool.name == "update_goal"));
            if max_continuations != 0 && goal.continuation_count >= max_continuations {
                tracing::info!(
                    "goal for {thread_id} reached the continuation cap ({}); stopping",
                    goal.continuation_count
                );
                goal.status = codewhale_protocol::ThreadGoalStatus::Paused;
                goal.updated_at = chrono::Utc::now().timestamp();
            } else if engine_hit_cap {
                tracing::info!(
                    "goal for {thread_id} hit the engine continuation cap ({}); pausing",
                    goal.continuation_count
                );
                goal.status = codewhale_protocol::ThreadGoalStatus::Paused;
                goal.updated_at = chrono::Utc::now().timestamp();
            } else if self.read_config().goal_enforce_token_budget()
                && goal
                    .token_budget
                    .is_some_and(|budget| goal.tokens_used >= budget)
            {
                // The engine stops its own continuation at an enforced token
                // budget (#6013); the host mirrors that as a durable pause so
                // the cross-turn re-arm does not resurrect the spend.
                tracing::info!(
                    "goal for {thread_id} reached its enforced token budget ({}); pausing",
                    goal.tokens_used
                );
                goal.status = codewhale_protocol::ThreadGoalStatus::Paused;
                goal.pause_reason = Some(codewhale_protocol::GoalPauseReason::BudgetLimit);
                goal.updated_at = chrono::Utc::now().timestamp();
            } else if !update_goal_available {
                tracing::info!(
                    "goal for {thread_id} stays parked: the finished turn's catalog lacked update_goal"
                );
            } else {
                continue_after = Some(self.read_config().goal_continuation_delay_seconds());
            }
        }

        });
        let goal = match updated {
            Ok(Some(goal)) => goal,
            Ok(None) => {
                // A new explicit goal was accepted while the old turn ran.
                // Let its own durable gates claim the next idle pass.
                self.spawn_goal_continuation(thread_id.to_string(), 0);
                return;
            }
            Err(err) => {
                tracing::warn!("failed to record goal progress for {thread_id}: {err}");
                return;
            }
        };
        if let Err(err) = self.emit_goal_updated_event(thread_id, goal.clone()).await {
            tracing::warn!("failed to emit goal update for {thread_id}: {err}");
        }
        if let Some(delay_seconds) = continue_after {
            self.spawn_goal_continuation(thread_id.to_string(), delay_seconds);
        }
    }

    /// Arm one goal continuation pass to run after the quiet period. The
    /// sleep is interrupted by Runtime shutdown. A pause, clear, completion, or
    /// cap that lands while the timer runs is honored by the re-read inside
    /// `run_goal_continuation`, which is the cancellation path — `DELETE
    /// /goal` and status syncs therefore do not need to interrupt this task.
    pub(super) fn spawn_goal_continuation(&self, thread_id: String, delay_seconds: u64) {
        if self.cancel_token.is_cancelled() {
            return;
        }
        let manager = self.clone();
        let worker = tokio::spawn(async move {
            // Quiet period between passes, mirroring the interactive
            // engine's `goal_continuation_delay_seconds` behavior (#5508).
            if delay_seconds > 0 {
                tokio::select! {
                    _ = manager.cancel_token.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(delay_seconds)) => {}
                }
            }
            if manager.cancel_token.is_cancelled() {
                return;
            }
            if let Err(err) = manager.run_goal_continuation(&thread_id).await {
                tracing::warn!("goal continuation for {thread_id} failed: {err}");
            }
        });
        self.track_receipt_worker(worker);
    }

    /// Dispatch one goal continuation pass after the quiet period. Every
    /// guard re-reads durable state: the goal may have been paused, cleared,
    /// completed, or capped while the timer ran.
    pub(super) async fn run_goal_continuation(&self, thread_id: &str) -> Result<()> {
        self.ensure_accepting_execution()?;
        let Some(goal) = self.store.load_goal(thread_id)? else {
            return Ok(());
        };
        if !matches!(goal.status, codewhale_protocol::ThreadGoalStatus::Active) {
            return Ok(());
        }
        {
            let active = self.active.lock().await;
            if let Some(state) = active.engines.get(thread_id)
                && state.active_turn.is_some()
            {
                return Ok(());
            }
        }
        let max_continuations = i64::from(self.read_config().goal_max_continuations());
        if max_continuations != 0 && goal.continuation_count >= max_continuations {
            return Ok(());
        }
        let continuation_index = u32::try_from(goal.continuation_count.max(1)).unwrap_or(u32::MAX);
        let snapshot = crate::tools::goal::GoalSnapshot::from_thread_goal(&goal);
        let prompt = crate::tools::goal::render_continuation_prompt(&snapshot, continuation_index);
        self.start_goal_turn(thread_id, prompt, continuation_index, goal)
            .await?;
        Ok(())
    }

    /// Persist one canonical Agent Mail envelope in the runtime store. The
    /// caller-supplied id is an idempotency key: an exact replay returns the
    /// existing lifecycle record, while conflicting intent fails closed.
    pub async fn queue_agent_mail(
        &self,
        mut request: AgentMailSendRequest,
    ) -> Result<AgentMailSendResponse> {
        if agent_mail_looks_like_raw_transcript(&request.summary) {
            bail!("Agent Mail accepts a bounded handoff summary, not a raw transcript");
        }
        request.summary = sanitize_agent_mail_text(&request.summary, MAX_AGENT_MAIL_SUMMARY_BYTES);
        request.sender.display_label = sanitize_agent_mail_text(
            &request.sender.display_label,
            codewhale_protocol::agent_mail::MAX_AGENT_MAIL_DISPLAY_LABEL_BYTES,
        );
        for evidence in &mut request.evidence {
            if let Some(label) = evidence.label.as_mut() {
                *label = sanitize_agent_mail_text(
                    label,
                    codewhale_protocol::agent_mail::MAX_AGENT_MAIL_EVIDENCE_LABEL_BYTES,
                );
            }
        }
        request.validate().map_err(|error| anyhow!(error))?;
        if request.source_thread_id == request.destination_thread_id {
            bail!("Agent Mail source and destination threads must differ");
        }

        let source_thread = self.get_thread(&request.source_thread_id).await?;
        let destination_thread = self.get_thread(&request.destination_thread_id).await?;
        let source = agent_mail_address(&self.store.owner_id, &source_thread)?;
        let destination = agent_mail_address(&self.store.owner_id, &destination_thread)?;
        if source.owner_id != destination.owner_id
            || source.workspace_id != destination.workspace_id
        {
            bail!(
                "Agent Mail ownership denied: source and destination must belong to the same runtime owner and workspace"
            );
        }
        let expected_sender = agent_mail_sender_identity(&source_thread)?;
        if request.sender.identity != expected_sender {
            bail!(
                "Agent Mail ownership denied: sender identity does not own the source task/session"
            );
        }

        let (envelope, idempotent_replay) = {
            let _mail_mutation = self.store.mail_mutation.lock();
            let path = self.store.mail_path(&request.message_id)?;
            if path.exists() {
                let persisted = self.store.load_agent_mail(&request.message_id)?;
                if !persisted.matches_send_request(&request) {
                    bail!(
                        "Agent Mail message id '{}' already exists with different delivery intent",
                        request.message_id
                    );
                }
                (persisted, true)
            } else {
                let envelope = AgentMailEnvelope {
                    schema_version: AGENT_MAIL_SCHEMA_VERSION,
                    message_id: request.message_id,
                    source,
                    destination,
                    sender: request.sender,
                    summary: request.summary,
                    evidence: request.evidence,
                    delivery_mode: request.delivery_mode,
                    trigger_turn: request.trigger_turn,
                    hop_count: request.hop_count,
                    status: AgentMailStatus::Queued,
                    created_at: Utc::now(),
                    delivered_at: None,
                    read_at: None,
                    attempt_count: 0,
                    failure: None,
                    delivery_turn_id: None,
                };
                self.store.save_agent_mail(&envelope)?;
                (envelope, false)
            }
        };

        self.emit_agent_mail_event(agent_mail_event_for_status(envelope.status), &envelope)
            .await?;
        Ok(AgentMailSendResponse {
            envelope,
            idempotent_replay,
        })
    }

    pub async fn list_agent_mail_for_thread(
        &self,
        thread_id: &str,
    ) -> Result<Vec<AgentMailEnvelope>> {
        let thread = self.get_thread(thread_id).await?;
        let address = agent_mail_address(&self.store.owner_id, &thread)?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let inbox = store
                .list_agent_mail()?
                .into_iter()
                .filter(|mail| mail.destination == address)
                .collect::<Vec<_>>();
            Ok(inbox)
        })
        .await
        .context("Agent Mail inbox task panicked")?
    }

    pub async fn mark_agent_mail_read(
        &self,
        thread_id: &str,
        message_id: &AgentMailMessageId,
    ) -> Result<AgentMailEnvelope> {
        let thread = self.get_thread(thread_id).await?;
        let address = agent_mail_address(&self.store.owner_id, &thread)?;
        let envelope = {
            let _mail_mutation = self.store.mail_mutation.lock();
            let mut envelope = self.store.load_agent_mail(message_id)?;
            if envelope.destination != address {
                bail!("Agent Mail ownership denied: message does not belong to this destination");
            }
            match envelope.status {
                AgentMailStatus::Read => envelope,
                AgentMailStatus::Delivered => {
                    envelope.status = AgentMailStatus::Read;
                    envelope.read_at = Some(Utc::now());
                    self.store.save_agent_mail(&envelope)?;
                    envelope
                }
                _ => bail!("Agent Mail can be marked read only after delivery"),
            }
        };
        self.emit_agent_mail_event(AGENT_MAIL_EVENT_READ, &envelope)
            .await?;
        Ok(envelope)
    }

    pub async fn mark_thread_read(&self, thread_id: &str, item_id: &str) -> Result<ThreadRecord> {
        let store = self.store.clone();
        let thread_id = thread_id.to_string();
        let item_id = validated_record_id(item_id, "item id")?.to_string();
        tokio::task::spawn_blocking(move || -> Result<ThreadRecord> {
            store
                .load_thread(&thread_id)
                .with_context(|| format!("Thread not found: {thread_id}"))?;
            let Some((item, turn)) = store.thread_item(&thread_id, &item_id)? else {
                bail!("item {item_id} is not part of thread {thread_id}");
            };
            let turn_items = store
                .list_items_for_turns_map(std::slice::from_ref(&turn.id))?
                .remove(&turn.id)
                .unwrap_or_default();
            let marker = ThreadReadMarker {
                last_in_turn: turn_items.last().is_some_and(|last| last.id == item_id),
                watermark_item_id: newest_recorded_item_id(std::slice::from_ref(&turn)),
                item_id,
            };
            let read_order = |item: &TurnItemRecord, turn: &TurnRecord| {
                (turn.created_at, item.started_at.is_none(), item.started_at)
            };

            let _thread_mutation = store.thread_mutation.lock();
            let mut thread = store
                .load_thread(&thread_id)
                .with_context(|| format!("Thread not found: {thread_id}"))?;
            if let Some(current) = thread.last_read.as_ref()
                && let Some((current_item, current_turn)) =
                    store.thread_item(&thread_id, &current.item_id)?
                && read_order(&item, &turn) < read_order(&current_item, &current_turn)
            {
                return Ok(thread);
            }
            if thread.last_read.as_ref() != Some(&marker) {
                thread.last_read = Some(marker);
                store.save_thread(&thread)?;
            }
            Ok(thread)
        })
        .await
        .context("thread read marker task panicked")?
    }

    /// Withdraw a queued envelope before it starts delivery (#6176). Only
    /// `Queued` mail can be canceled; anything that reached delivery keeps
    /// its receipt. Re-canceling an already-canceled envelope is an
    /// idempotent no-op returning the stored envelope.
    pub async fn cancel_agent_mail(
        &self,
        thread_id: &str,
        message_id: &AgentMailMessageId,
    ) -> Result<AgentMailEnvelope> {
        let thread = self.get_thread(thread_id).await?;
        let address = agent_mail_address(&self.store.owner_id, &thread)?;
        let envelope = {
            let _mail_mutation = self.store.mail_mutation.lock();
            let mut envelope = self.store.load_agent_mail(message_id)?;
            if envelope.destination != address {
                bail!("Agent Mail ownership denied: message does not belong to this destination");
            }
            match envelope.status {
                AgentMailStatus::Canceled => envelope,
                AgentMailStatus::Queued => {
                    envelope.status = AgentMailStatus::Canceled;
                    self.store.save_agent_mail(&envelope)?;
                    envelope
                }
                _ => bail!(
                    "Agent Mail can be canceled only while queued (status: {:?})",
                    envelope.status
                ),
            }
        };
        self.emit_agent_mail_event(AGENT_MAIL_EVENT_CANCELED, &envelope)
            .await?;
        Ok(envelope)
    }

    /// Claim and project one envelope into the existing destination turn
    /// queue. A busy thread keeps queued mail untouched; retryable failures are
    /// claimed again only below the bounded attempt ceiling.
    pub async fn deliver_agent_mail(
        &self,
        thread_id: &str,
        message_id: &AgentMailMessageId,
    ) -> Result<(AgentMailEnvelope, Option<TurnRecord>)> {
        self.ensure_accepting_execution()?;
        let thread = self.get_thread(thread_id).await?;
        let address = agent_mail_address(&self.store.owner_id, &thread)?;
        {
            let active = self.active.lock().await;
            if active
                .engines
                .get(thread_id)
                .and_then(|state| state.active_turn.as_ref())
                .is_some()
            {
                let envelope = self.store.load_agent_mail(message_id)?;
                if envelope.destination != address {
                    bail!(
                        "Agent Mail ownership denied: message does not belong to this destination"
                    );
                }
                return Ok((envelope, None));
            }
        }

        let (claimed, terminal) = {
            let _mail_mutation = self.store.mail_mutation.lock();
            self.ensure_accepting_execution()?;
            let mut envelope = self.store.load_agent_mail(message_id)?;
            if envelope.destination != address {
                bail!("Agent Mail ownership denied: message does not belong to this destination");
            }
            let terminal_event = match envelope.status {
                AgentMailStatus::Delivered => Some(AGENT_MAIL_EVENT_DELIVERED),
                AgentMailStatus::Read => Some(AGENT_MAIL_EVENT_READ),
                AgentMailStatus::Delivering => Some(AGENT_MAIL_EVENT_DELIVERING),
                // Canceled mail is terminal: a later deliver returns the
                // envelope with no turn rather than claiming it (#6176).
                AgentMailStatus::Canceled => Some(AGENT_MAIL_EVENT_CANCELED),
                AgentMailStatus::Failed
                    if envelope
                        .failure
                        .as_ref()
                        .is_none_or(|failure| !failure.retryable) =>
                {
                    Some(AGENT_MAIL_EVENT_DELIVERY_FAILED)
                }
                _ => None,
            };
            if let Some(event) = terminal_event {
                (None, Some((envelope, None, event)))
            } else if envelope.attempt_count >= MAX_AGENT_MAIL_DELIVERY_ATTEMPTS {
                envelope.status = AgentMailStatus::Failed;
                envelope.failure = Some(AgentMailFailureReceipt {
                    code: AgentMailFailureCode::AttemptLimit,
                    message: "Agent Mail delivery attempt limit reached".to_string(),
                    retryable: false,
                    failed_at: Utc::now(),
                });
                self.store.save_agent_mail(&envelope)?;
                (
                    None,
                    Some((envelope, None, AGENT_MAIL_EVENT_DELIVERY_FAILED)),
                )
            } else if let Some(turn) = self
                .store
                .list_turns_for_thread(thread_id)?
                .into_iter()
                .find(|turn| turn.agent_mail_message_id.as_deref() == Some(message_id.as_str()))
            {
                envelope.status = AgentMailStatus::Delivered;
                envelope.attempt_count = envelope.attempt_count.max(1);
                envelope.delivered_at = Some(turn.created_at);
                envelope.read_at = None;
                envelope.failure = None;
                envelope.delivery_turn_id = Some(turn.id.clone());
                self.store.save_agent_mail(&envelope)?;
                (
                    None,
                    Some((envelope, Some(turn), AGENT_MAIL_EVENT_DELIVERED)),
                )
            } else {
                envelope.status = AgentMailStatus::Delivering;
                envelope.attempt_count = envelope.attempt_count.saturating_add(1);
                envelope.failure = None;
                self.store.save_agent_mail(&envelope)?;
                (Some(envelope), None)
            }
        };
        if let Some((envelope, turn, event)) = terminal {
            self.emit_agent_mail_event(event, &envelope).await?;
            return Ok((envelope, turn));
        }
        let claimed = claimed.context("Agent Mail delivery claim was not produced")?;
        if let Err(error) = self
            .emit_agent_mail_event(AGENT_MAIL_EVENT_DELIVERING, &claimed)
            .await
        {
            let failed = {
                let _mail_mutation = self.store.mail_mutation.lock();
                let mut envelope = self.store.load_agent_mail(message_id)?;
                if envelope.status == AgentMailStatus::Delivering
                    && envelope.attempt_count == claimed.attempt_count
                {
                    envelope.status = AgentMailStatus::Failed;
                    envelope.failure = Some(AgentMailFailureReceipt {
                        code: AgentMailFailureCode::DeliveryRejected,
                        message: "Delivery event persistence failed before turn start".to_string(),
                        retryable: true,
                        failed_at: Utc::now(),
                    });
                    self.store.save_agent_mail(&envelope)?;
                }
                envelope
            };
            let _ = self
                .emit_agent_mail_event(AGENT_MAIL_EVENT_DELIVERY_FAILED, &failed)
                .await;
            return Err(error).context("Failed to persist Agent Mail delivery claim");
        }

        let prompt = render_agent_mail_prompt(&claimed);
        let input_summary = format!(
            "Agent Mail from {} ({})",
            claimed.sender.display_label, claimed.source.thread_id
        );
        let turn_result = self
            .start_turn_with_source(
                thread_id,
                StartTurnRequest {
                    account_model_owner: None,
                    profile_constitution: None,
                    expected_workspace: None,
                    max_output_tokens: None,
                    prompt,
                    images: Vec::new(),
                    operation_key: None,
                    input_summary: Some(input_summary),
                    model: None,
                    reasoning_effort: None,
                    allowed_tools: None,
                    mode: None,
                    permission_posture: None,
                    allow_shell: None,
                    trust_mode: None,
                    auto_approve: None,
                    dynamic_tools: Vec::new(),
                    environment_id: None,
                    model_provider: None,
                    model_provider_id: None,
                },
                RuntimeTurnInputSource::AgentMail {
                    message_id: message_id.to_string(),
                    persisted_summary: claimed.summary.clone(),
                },
                None,
                false,
            )
            .await;

        match turn_result {
            Ok((turn, _replayed)) => {
                let delivered = {
                    let _mail_mutation = self.store.mail_mutation.lock();
                    let mut envelope = self.store.load_agent_mail(message_id)?;
                    envelope.status = AgentMailStatus::Delivered;
                    envelope.delivered_at = Some(Utc::now());
                    envelope.failure = None;
                    envelope.delivery_turn_id = Some(turn.id.clone());
                    self.store.save_agent_mail(&envelope)?;
                    envelope
                };
                self.emit_agent_mail_event(AGENT_MAIL_EVENT_DELIVERED, &delivered)
                    .await?;
                Ok((delivered, Some(turn)))
            }
            Err(error) => {
                let failed = {
                    let _mail_mutation = self.store.mail_mutation.lock();
                    let mut envelope = self.store.load_agent_mail(message_id)?;
                    envelope.status = AgentMailStatus::Failed;
                    envelope.failure = Some(AgentMailFailureReceipt {
                        code: AgentMailFailureCode::DestinationUnavailable,
                        message: "Destination rejected Agent Mail at this boundary".to_string(),
                        retryable: true,
                        failed_at: Utc::now(),
                    });
                    self.store.save_agent_mail(&envelope)?;
                    envelope
                };
                tracing::warn!(
                    message_id = %message_id,
                    thread_id,
                    %error,
                    "Agent Mail delivery failed"
                );
                self.emit_agent_mail_event(AGENT_MAIL_EVENT_DELIVERY_FAILED, &failed)
                    .await?;
                Ok((failed, None))
            }
        }
    }

    pub(super) async fn emit_agent_mail_event(
        &self,
        event: &'static str,
        envelope: &AgentMailEnvelope,
    ) -> Result<bool> {
        let _emit_order = self.event_emit.lock().await;
        let store = self.store.clone();
        let thread_id = envelope.destination.thread_id.clone();
        let expected = RuntimeEventMatch::AgentMail {
            event_name: event.to_string(),
            message_id: envelope.message_id.to_string(),
            attempt_count: envelope.attempt_count,
        };
        let already_emitted =
            tokio::task::spawn_blocking(move || store.contains_event(&thread_id, &expected))
                .await
                .context("Agent Mail event dedupe scan failed")??;
        if already_emitted {
            return Ok(false);
        }
        let payload = serde_json::to_value(AgentMailEventPayload {
            mail: envelope.clone(),
        })?;
        self.append_and_broadcast_event(
            &envelope.destination.thread_id,
            envelope.delivery_turn_id.as_deref(),
            None,
            event,
            payload,
        )
        .await?;
        Ok(true)
    }

    pub(super) async fn deliver_next_wake_agent_mail(&self, thread_id: &str) -> Result<()> {
        self.ensure_accepting_execution()?;
        let next = self
            .list_agent_mail_for_thread(thread_id)
            .await?
            .into_iter()
            .find(|mail| {
                mail.delivery_mode == AgentMailDeliveryMode::WakeAtSafeBoundary
                    && mail.trigger_turn
                    && (mail.status == AgentMailStatus::Queued
                        || (mail.status == AgentMailStatus::Failed
                            && mail
                                .failure
                                .as_ref()
                                .is_some_and(|failure| failure.retryable)))
            });
        if let Some(mail) = next {
            let _ = Box::pin(self.deliver_agent_mail(thread_id, &mail.message_id)).await?;
        }
        Ok(())
    }

    pub(super) fn spawn_agent_mail_safe_boundary_delivery(&self, thread_id: String) {
        if self.cancel_token.is_cancelled() {
            return;
        }
        let manager = self.clone();
        let worker = tokio::spawn(async move {
            if let Err(error) = Box::pin(manager.deliver_next_wake_agent_mail(&thread_id)).await {
                tracing::warn!(thread_id, %error, "Failed to deliver queued Agent Mail");
            }
        });
        self.track_receipt_worker(worker);
    }

    pub(super) fn projection_lock(&self, thread_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.projection_locks.lock();
        Arc::clone(
            locks
                .entry(thread_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    pub(super) async fn emit_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        let _emit_order = self.event_emit.lock().await;
        self.append_and_broadcast_event(thread_id, turn_id, item_id, event, payload)
            .await
    }

    /// Append and broadcast an event while the caller owns `event_emit`.
    /// Keeping this primitive separate lets dynamic-tool settlement hold its
    /// projection boundary through durable append, registry removal, and the
    /// non-awaiting result send.
    pub(super) async fn append_and_broadcast_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        let record = self
            .store
            .append_event(thread_id, turn_id, item_id, event, payload)
            .await?;
        if let Err(e) = self.event_tx.send(record.clone()) {
            tracing::debug!(
                "Runtime event broadcast failed (no receivers or channel full): {}",
                e
            );
        }
        Ok(record)
    }

    pub(super) async fn emit_turn_completed_if_missing(
        &self,
        turn: &TurnRecord,
        recovered: bool,
    ) -> Result<bool> {
        let _emit_order = self.event_emit.lock().await;
        let store = self.store.clone();
        let thread_id = turn.thread_id.clone();
        let expected = RuntimeEventMatch::TurnCompleted {
            turn_id: turn.id.clone(),
        };
        let already_emitted =
            tokio::task::spawn_blocking(move || store.contains_event(&thread_id, &expected))
                .await
                .context("Runtime turn-completion dedupe scan failed")??;
        if already_emitted {
            return Ok(false);
        }
        let mut payload = json!({ "turn": turn });
        if recovered && let Some(object) = payload.as_object_mut() {
            object.insert("recovered".to_string(), json!(true));
        }
        self.append_and_broadcast_event(
            &turn.thread_id,
            Some(&turn.id),
            None,
            "turn.completed",
            payload,
        )
        .await?;
        Ok(true)
    }

    pub(super) async fn emit_recovered_dynamic_cancellation_if_missing(
        &self,
        params: &DynamicToolCallParams,
    ) -> Result<bool> {
        let _emit_order = self.event_emit.lock().await;
        let store = self.store.clone();
        let thread_id = params.thread_id.clone();
        let expected = RuntimeEventMatch::DynamicTerminal {
            turn_id: params.turn_id.clone(),
            call_id: params.call_id.clone(),
        };
        let already_emitted =
            tokio::task::spawn_blocking(move || store.contains_event(&thread_id, &expected))
                .await
                .context("Runtime dynamic-tool terminal dedupe scan failed")??;
        if already_emitted {
            return Ok(false);
        }
        let mut payload =
            dynamic_tool_terminal_payload(params, "canceled", None, Some("process_restart"));
        if let Some(object) = payload.as_object_mut() {
            object.insert("terminal".to_string(), json!(true));
            object.insert("recovered".to_string(), json!(true));
        }
        self.append_and_broadcast_event(
            &params.thread_id,
            Some(&params.turn_id),
            None,
            "tool_call.canceled",
            payload,
        )
        .await?;
        Ok(true)
    }

    pub(super) async fn flush_recovery_receipts_for_thread(&self, thread_id: &str) -> Result<()> {
        if !self.recovery_receipts.lock().contains_key(thread_id) {
            return Ok(());
        }
        let _recovery_flush = self.recovery_flush.lock().await;
        if self.cancel_token.is_cancelled() {
            bail!("Runtime is shutting down; recovery receipts remain pending");
        }
        loop {
            let next = self
                .recovery_receipts
                .lock()
                .get(thread_id)
                .and_then(|receipts| receipts.first())
                .cloned();
            let Some(receipt) = next else {
                return Ok(());
            };

            // An in-process monitor failure may leave retry-safe calls in the
            // live registry. Retry their supervised cancellation before the
            // static restart-recovery receipts below. Startup recovery has no
            // live registry entries, so this is a no-op in that case.
            self.settle_dynamic_tools_for_terminal_turn(thread_id, &receipt.turn.id)
                .await?;
            let engine = {
                let active = self.active.lock().await;
                active
                    .engines
                    .get(thread_id)
                    .map(|state| state.engine.clone())
            };
            self.settle_user_inputs_for_terminal_turn(thread_id, &receipt.turn.id, engine)
                .await?;

            let projection_lock = self.projection_lock(thread_id);
            let _projection = projection_lock.lock().await;
            for params in &receipt.unresolved_dynamic_tools {
                self.emit_recovered_dynamic_cancellation_if_missing(params)
                    .await?;
            }
            self.emit_turn_completed_if_missing(&receipt.turn, true)
                .await?;
            drop(_projection);

            let mut queued = self.recovery_receipts.lock();
            let remove_thread = if let Some(receipts) = queued.get_mut(thread_id) {
                receipts.retain(|candidate| candidate.turn.id != receipt.turn.id);
                receipts.is_empty()
            } else {
                false
            };
            if remove_thread {
                queued.remove(thread_id);
            }
        }
    }

    /// Log a store fault and, when the error carries a typed
    /// [`RuntimeStoreRecordFailure`], publish it as a `runtime.store_failure`
    /// event so whoever can act on the file sees it: the task timeline, SSE
    /// clients, and the TUI (#5931). The event log lives in the same store,
    /// so a fault that also blocks publication stays a log line.
    pub(super) async fn report_store_failure(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        message: &str,
        error: &anyhow::Error,
        terminal: bool,
    ) -> Option<RuntimeEventRecord> {
        tracing::error!(thread_id = %thread_id, turn_id = ?turn_id, "{message}");
        let failure = RuntimeStoreRecordFailure::from_error(error)?;
        let notice = failure.notice(error, terminal);
        let item_id = (failure.record_kind == RuntimeStoreRecordKind::Item)
            .then_some(failure.record_id.as_str())
            .filter(|id| validated_record_id(id, "item id").is_ok());
        match self
            .emit_event(
                thread_id,
                turn_id,
                item_id,
                RUNTIME_STORE_FAILURE_EVENT,
                json!(notice),
            )
            .await
        {
            Ok(record) => Some(record),
            Err(emit_error) => {
                tracing::error!(
                    thread_id = %thread_id,
                    path = %failure.path.display(),
                    "runtime store failure notice could not be published: {emit_error:#}"
                );
                None
            }
        }
    }

    pub(super) fn queue_recovery_receipt(&self, receipt: RecoveredTurnReceipt) {
        let thread_id = receipt.turn.thread_id.clone();
        let turn_id = receipt.turn.id.clone();
        let mut queued = self.recovery_receipts.lock();
        let receipts = queued.entry(thread_id).or_default();
        if let Some(existing) = receipts
            .iter_mut()
            .find(|candidate| candidate.turn.id == turn_id)
        {
            let mut known_calls = existing
                .unresolved_dynamic_tools
                .iter()
                .map(|params| params.call_id.clone())
                .collect::<HashSet<_>>();
            existing.unresolved_dynamic_tools.extend(
                receipt
                    .unresolved_dynamic_tools
                    .into_iter()
                    .filter(|params| known_calls.insert(params.call_id.clone())),
            );
            return;
        }
        receipts.push(receipt);
        receipts.sort_by_key(|candidate| candidate.turn.created_at);
    }

    pub(super) fn spawn_dynamic_tool_settlement(
        &self,
        claim: ClaimedDynamicToolSettlement,
        outcome: DynamicToolTerminalOutcome,
    ) -> oneshot::Receiver<std::result::Result<DynamicToolSettlementAck, String>> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let manager = self.clone();
        let worker = tokio::spawn(async move {
            use futures_util::FutureExt;

            let mut claim = Some(claim);
            let mut outcome = Some(outcome);
            let settlement = std::panic::AssertUnwindSafe(async {
                let claim_ref = claim
                    .as_ref()
                    .ok_or_else(|| "Dynamic tool settlement lost its claim".to_string())?;
                let outcome_ref = outcome
                    .as_ref()
                    .ok_or_else(|| "Dynamic tool settlement lost its outcome".to_string())?;
                let projection_lock = manager.projection_lock(&claim_ref.params.thread_id);
                let _projection = projection_lock.lock().await;
                let emit_order = manager.event_emit.lock().await;

                // `resolved` linearizes durable acceptance by the Runtime. It
                // deliberately does not claim that the model consumed the
                // result: the receiver may close at any point before the
                // post-receipt, non-awaiting send.
                let (event, payload) = match outcome_ref {
                    DynamicToolTerminalOutcome::Resolved(result) => {
                        let mut payload = dynamic_tool_terminal_payload(
                            &claim_ref.params,
                            "resolved",
                            Some(result.success),
                            None,
                        );
                        if let Some(object) = payload.as_object_mut() {
                            object.insert("result_accepted".to_string(), json!(true));
                        }
                        ("tool_call.resolved", payload)
                    }
                    DynamicToolTerminalOutcome::Canceled { reason, terminal } => {
                        let mut payload = dynamic_tool_terminal_payload(
                            &claim_ref.params,
                            "canceled",
                            None,
                            Some(reason),
                        );
                        if *terminal && let Some(object) = payload.as_object_mut() {
                            object.insert("terminal".to_string(), json!(true));
                        }
                        ("tool_call.canceled", payload)
                    }
                    DynamicToolTerminalOutcome::Timeout { timeout } => {
                        let mut payload =
                            dynamic_tool_terminal_payload(&claim_ref.params, "timeout", None, None);
                        if let Some(object) = payload.as_object_mut() {
                            object.insert("timeout_secs".to_string(), json!(timeout.as_secs()));
                        }
                        ("tool_call.timeout", payload)
                    }
                };

                if let Err(error) = manager
                    .append_and_broadcast_event(
                        &claim_ref.params.thread_id,
                        Some(&claim_ref.params.turn_id),
                        None,
                        event,
                        payload,
                    )
                    .await
                {
                    drop(emit_order);
                    if let Some(claim) = claim.take() {
                        let retry_safe = error
                            .downcast_ref::<RuntimeEventAppendError>()
                            .is_none_or(RuntimeEventAppendError::retry_safe);
                        if retry_safe {
                            // Definite pre-write failures and transactionally
                            // rolled-back appends return the call to Awaiting.
                            manager.restore_dynamic_tool_claim(claim);
                        } else {
                            // A failed rollback means the JSONL tail may already
                            // contain the terminal line. Keep the request
                            // explicitly indeterminate so neither an API retry
                            // nor turn timeout can append a duplicate.
                            manager.mark_dynamic_tool_claim_indeterminate(&claim);
                            drop(claim);
                        }
                    }
                    return Err(error.to_string());
                }

                let claim = claim
                    .take()
                    .ok_or_else(|| "Dynamic tool settlement lost its claim".to_string())?;
                let outcome = outcome
                    .take()
                    .ok_or_else(|| "Dynamic tool settlement lost its outcome".to_string())?;

                // The snapshot boundary stays held until the request
                // disappears. The model-facing channel is only woken after the
                // terminal event is on disk, and send itself cannot suspend or
                // be caller-canceled.
                manager.finish_dynamic_tool_settlement(&claim.params);
                claim
                    .settlement_tx
                    .send_modify(|epoch| *epoch = epoch.saturating_add(1));
                let result_accepted = matches!(&outcome, DynamicToolTerminalOutcome::Resolved(_));
                match outcome {
                    DynamicToolTerminalOutcome::Resolved(result) => {
                        if claim.sender.send(result).is_err() {
                            tracing::debug!(
                                call_id = %claim.params.call_id,
                                "Durably accepted dynamic tool result had no remaining model receiver"
                            );
                        }
                    }
                    DynamicToolTerminalOutcome::Canceled { .. }
                    | DynamicToolTerminalOutcome::Timeout { .. } => drop(claim.sender),
                }
                Ok(DynamicToolSettlementAck { result_accepted })
            })
            .catch_unwind()
            .await;

            let result = match settlement {
                Ok(result) => result,
                Err(payload) => {
                    // A panic before durable completion must not leave a
                    // Settling tombstone. Reacquire the same projection
                    // boundary before returning the sender to Awaiting.
                    if let Some(claim) = claim.take() {
                        let projection_lock = manager.projection_lock(&claim.params.thread_id);
                        let _projection = projection_lock.lock().await;
                        manager.restore_dynamic_tool_claim(claim);
                    }
                    Err(format!(
                        "Dynamic tool settlement task panicked: {}",
                        panic_payload_message(&*payload)
                    ))
                }
            };
            let _ = ack_tx.send(result);
        });
        self.track_receipt_worker(worker);
        ack_rx
    }

    pub(super) async fn await_dynamic_tool_settlement(
        ack: oneshot::Receiver<std::result::Result<DynamicToolSettlementAck, String>>,
    ) -> Result<DynamicToolSettlementAck> {
        match ack.await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(error)) => bail!("{error}"),
            Err(_) => bail!("Dynamic tool settlement task ended before acknowledgement"),
        }
    }

    pub(super) async fn settle_dynamic_tool_timeout(
        &self,
        claim: ClaimedDynamicToolSettlement,
        timeout: Duration,
    ) -> Result<()> {
        let ack = self
            .spawn_dynamic_tool_settlement(claim, DynamicToolTerminalOutcome::Timeout { timeout });
        Self::await_dynamic_tool_settlement(ack).await?;
        Ok(())
    }

    pub(super) async fn settle_dynamic_tools_for_terminal_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        loop {
            let (claims, mut settling, indeterminate) =
                self.claim_or_watch_pending_dynamic_tools_for_turn(thread_id, turn_id);
            if indeterminate {
                bail!(
                    "Turn {turn_id} has an indeterminate dynamic-tool receipt; refusing to publish turn completion"
                );
            }
            if claims.is_empty() && settling.is_empty() {
                return Ok(());
            }

            let mut first_error = None;
            for claim in claims {
                let ack = self.spawn_dynamic_tool_settlement(
                    claim,
                    DynamicToolTerminalOutcome::Canceled {
                        reason: "turn_terminal",
                        terminal: true,
                    },
                );
                if let Err(error) = Self::await_dynamic_tool_settlement(ack).await
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }

            // If result delivery or timeout already owned a call, wait for its
            // supervised completion/rollback before publishing turn.completed.
            // On rollback the next iteration claims terminal cancellation; on
            // success the completed entry is gone.
            for progress in &mut settling {
                let _ = progress.changed().await;
            }

            // Every claim selected above has now either committed, restored
            // itself to Awaiting, or entered the explicit indeterminate state.
            // Returning only after supervising the whole batch prevents an
            // early failure from dropping unstarted senders into permanent
            // Settling tombstones.
            if let Some(error) = first_error {
                return Err(error);
            }
        }
    }

    /// Persist a streaming item without blocking the Tokio worker that drives
    /// engine events. Each delta must reach the item projection before its
    /// durable event is sequenced, otherwise a snapshot at that cursor can
    /// expose stale text. Keeping the full record in memory avoids rereading
    /// and reparsing the same item for every provider chunk.
    pub(super) async fn save_streaming_item(&self, item: &TurnItemRecord) -> Result<()> {
        let store = self.store.clone();
        let item = item.clone();
        tokio::task::spawn_blocking(move || store.save_item(&item))
            .await
            .context("Streaming item persistence task failed")??;
        Ok(())
    }

    /// Close a streaming item that something other than its own completion
    /// frame has ended, and publish it as terminal.
    ///
    /// Two boundaries mean "the segment above is over": a new thinking block
    /// starting, and a tool call starting. The engine already reads the first
    /// that way — `ContentBlockStart::Thinking` parks a block that never saw
    /// its stop frame (`flush_thinking_block`), and a stream retry after
    /// system sleep or a mid-stream network drop discards the fragment without
    /// ever emitting `ThinkingComplete` — but the projection used to drop the
    /// pointer without closing the item. That left an `in_progress` row
    /// inside a turn that went on to complete, and
    /// `thread_detail_has_live_work` then refused every later session save for
    /// the thread.
    ///
    /// The slot is taken first: an event that arrives without a fresh start
    /// cannot append to a closed segment.
    async fn close_streaming_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        slot: &mut Option<TurnItemRecord>,
        status: TurnItemLifecycleStatus,
    ) -> Result<()> {
        let Some(mut item) = slot.take() else {
            return Ok(());
        };
        item.status = status;
        item.summary = summarize_text(item.detail.as_deref().unwrap_or_default(), SUMMARY_LIMIT);
        item.ended_at = Some(Utc::now());
        self.save_streaming_item(&item).await?;
        self.emit_event(
            thread_id,
            Some(turn_id),
            Some(&item.id),
            item_terminal_event(status),
            json!({ "item": item }),
        )
        .await?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn emit_event_for_test(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        event: &str,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        self.emit_event(thread_id, turn_id, None, event, payload)
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_snapshot_test_hook(&self, hook: mpsc::UnboundedSender<SnapshotTestPoint>) {
        *self.snapshot_test_hook.lock() = Some(hook);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn events_path_for_test(&self, thread_id: &str) -> Result<PathBuf> {
        self.store.events_path(thread_id)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_replay_test_hook(&self, hook: mpsc::UnboundedSender<ReplayTestPoint>) {
        *self.replay_test_hook.lock() = Some(hook);
    }

    pub async fn create_thread(&self, req: CreateThreadRequest) -> Result<ThreadRecord> {
        self.create_thread_with_shell_policy(req, None, None).await
    }

    /// Create a thread, resolving an unset `allow_shell` against the config
    /// source the host actually loaded (`config_path`/`config_profile`).
    ///
    /// An unset `allow_shell` takes the interactive default
    /// (`Config::interactive_allow_shell`, on unless configured off): a
    /// conversation opened from an app is attended, and every shell command
    /// still passes the thread's approval posture. The default is then checked
    /// with the same `validate_shell_access_policy` a PATCH opt-in runs, so a
    /// shell restriction from a project, profile, environment or managed
    /// source still wins at creation: an unset value falls back to no shell,
    /// and an explicit `allow_shell: true` is refused, as PATCH refuses it.
    /// An explicit `false` is never checked.
    pub async fn create_thread_with_shell_policy(
        &self,
        req: CreateThreadRequest,
        config_path: Option<&Path>,
        config_profile: Option<&str>,
    ) -> Result<ThreadRecord> {
        self.create_thread_with_reserved_id(req, config_path, config_profile, None)
            .await
    }

    pub async fn create_thread_with_reserved_id(
        &self,
        req: CreateThreadRequest,
        config_path: Option<&Path>,
        config_profile: Option<&str>,
        reserved_id: Option<String>,
    ) -> Result<ThreadRecord> {
        let now = Utc::now();
        let reasoning_effort = canonical_runtime_reasoning_effort(req.reasoning_effort.as_deref())?;
        let (model_provider, model_provider_id, default_model) = {
            let config = self.read_config().clone();
            let requested_kind = req
                .model_provider
                .as_deref()
                .filter(|provider| !provider.trim().is_empty());
            // `Some("")` is malformed provenance, not absence. Pass it to
            // the resolver so an imported/API-created record cannot silently
            // acquire the root custom route.
            let requested_id = req.model_provider_id.as_deref().map(str::trim);
            let identity = if requested_kind.is_some() || requested_id.is_some() {
                config.resolve_persisted_provider_identity(requested_kind, requested_id)
            } else {
                config.active_provider_identity()
            }
            .map_err(|reason| anyhow!(reason))?;
            // A caller-selected model must not depend on an unrelated default
            // being available (notably an unset local Ollama catalog).
            let default_model = resolve_runtime_route_for_identity(
                &config,
                &identity,
                req.model
                    .as_deref()
                    .filter(|model| !model.trim().is_empty()),
            )
            .map_err(|reason| anyhow!(reason))?
            .model;
            (
                identity.persisted_kind().to_string(),
                identity.persisted_id().map(str::to_string),
                default_model,
            )
        };
        let model = req
            .model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(default_model);
        let workspace = req.workspace.unwrap_or_else(|| self.workspace.clone());
        let requested_mode = req
            .mode
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "agent".to_string());
        let policy = RuntimePolicyProjection::from_request(
            &requested_mode,
            req.permission_posture.as_deref(),
            req.auto_approve,
        )?;
        let mode = policy.mode_setting().to_string();
        let permission_posture = Some(policy.permission_wire().to_string());
        let allow_shell = match req.allow_shell {
            // An explicit opt-in passes the same policy check a PATCH opt-in
            // runs, and is refused (not silently downgraded) on denial.
            Some(true) => {
                self.validate_shell_access_policy(&workspace, config_path, config_profile)
                    .await?;
                true
            }
            Some(false) => false,
            None => {
                let interactive_default = self.read_config().interactive_allow_shell();
                interactive_default
                    && self
                        .validate_shell_access_policy(&workspace, config_path, config_profile)
                        .await
                        .is_ok()
            }
        };
        let trust_mode = req.trust_mode.unwrap_or(false);
        let auto_approve = policy.auto_approve();

        let parent_thread_id = match req.parent_thread_id.as_deref().map(str::trim) {
            Some(parent) => {
                let parent = validated_record_id(parent, "parent thread id")?.to_string();
                if self.store.load_thread(&parent).is_err() {
                    bail!("parent_thread_id {parent} does not name a thread");
                }
                Some(parent)
            }
            None => None,
        };
        let thread = ThreadRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: match reserved_id {
                Some(id) => validated_record_id(&id, "reserved history thread id")?.to_string(),
                None => runtime_record_id("thr"),
            },
            created_at: now,
            updated_at: now,
            model,
            model_provider: Some(model_provider),
            model_provider_id,
            reasoning_effort,
            allowed_tools: req.allowed_tools,
            workspace,
            mode,
            permission_posture,
            allow_shell,
            trust_mode,
            auto_approve,
            latest_turn_id: None,
            latest_response_bookmark: None,
            archived: req.archived,
            system_prompt: req.system_prompt,
            task_id: req.task_id,
            parent_thread_id,
            title: None,
            session_id: None,
            saved_session_checkpoint: None,
            last_read: None,
        };
        if let Some(operation_key) = req.operation_key {
            let key_fingerprint = runtime_turn_operation_key_fingerprint(
                &self.store.owner_id,
                ":thread-create:",
                &operation_key,
            )?;
            let request_fingerprint =
                codewhale_runtime::hashing::sha256_hex(serde_json::to_vec(&json!({
                    "version": 1,
                    "model": thread.model,
                    "model_provider": thread.model_provider,
                    "model_provider_id": thread.model_provider_id,
                    "reasoning_effort": thread.reasoning_effort,
                    "allowed_tools": thread.allowed_tools,
                    "workspace": thread.workspace,
                    "mode": thread.mode,
                    "permission_posture": thread.permission_posture,
                    "allow_shell": thread.allow_shell,
                    "trust_mode": thread.trust_mode,
                    "auto_approve": thread.auto_approve,
                    "archived": thread.archived,
                    "system_prompt": thread.system_prompt,
                    "task_id": thread.task_id,
                    "dynamic_tools": req.dynamic_tools,
                    "environments": req.environments,
                }))?);
            let _emit_order = self.event_emit.lock().await;
            let store = self.store.clone();
            let (thread, event) = tokio::task::spawn_blocking(move || {
                store.with_turn_operation_claim(Some(&key_fingerprint), || {
                    let path = store.thread_create_operation_path(&key_fingerprint)?;
                    let mut binding = match read_store_file(&path) {
                        Ok(raw) => {
                            let binding: RuntimeThreadCreateBinding = serde_json::from_str(&raw)?;
                            binding.validate()?;
                            anyhow::ensure!(
                                binding.operation_key_fingerprint == key_fingerprint,
                                "thread creation operation does not match its binding"
                            );
                            if binding.request_fingerprint != request_fingerprint {
                                return Err(RuntimeThreadCreateConflict.into());
                            }
                            binding
                        }
                        Err(error)
                            if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                                error.kind() == std::io::ErrorKind::NotFound
                            }) =>
                        {
                            let binding = RuntimeThreadCreateBinding {
                                schema_version: 1,
                                operation_key_fingerprint: key_fingerprint.clone(),
                                request_fingerprint,
                                thread_id: thread.id.clone(),
                                created_at: thread.created_at,
                                committed: false,
                            };
                            binding.validate()?;
                            write_json_atomic(&path, &binding)?;
                            binding
                        }
                        Err(error) => return Err(error),
                    };
                    if binding.committed {
                        return Ok((store.load_thread(&binding.thread_id)?, None));
                    }
                    let started = store
                        .contains_event(&binding.thread_id, &RuntimeEventMatch::ThreadStarted)?;
                    let thread_path = store.thread_path(&binding.thread_id)?;
                    let thread = if thread_path.try_exists()? {
                        store.load_thread(&binding.thread_id)?
                    } else {
                        anyhow::ensure!(
                            !started && store.list_turns_for_thread(&binding.thread_id)?.is_empty(),
                            "reserved thread data is missing; recovery required"
                        );
                        let mut thread = thread;
                        thread.id.clone_from(&binding.thread_id);
                        thread.created_at = binding.created_at;
                        thread.updated_at = binding.created_at;
                        store.save_thread(&thread)?;
                        thread
                    };
                    let event = if started {
                        None
                    } else {
                        Some(store.append_event_transaction(
                            thread.id.clone(),
                            None,
                            None,
                            "thread.started".into(),
                            json!({ "thread": thread }),
                            EVENT_TRANSACTION_LOCK_TIMEOUT,
                        )?)
                    };
                    binding.committed = true;
                    write_json_atomic(&path, &binding)?;
                    Ok((thread, event))
                })
            })
            .await
            .context("Runtime thread creation worker failed")??;
            if let Some(event) = event {
                let _ = self.event_tx.send(event);
            }
            return Ok(thread);
        }
        self.store.save_thread(&thread)?;
        if let Err(error) = self
            .emit_event(
                &thread.id,
                None,
                None,
                "thread.started",
                json!({ "thread": thread.clone() }),
            )
            .await
        {
            self.active.lock().await.shell_managers.remove(&thread.id);
            let _ = self.store.remove_thread(&thread.id);
            return Err(error);
        }
        Ok(thread)
    }

    pub async fn discard_empty_thread(&self, thread_id: &str) -> Result<()> {
        let mut active = self.active.lock().await;
        if active.engines.contains_key(thread_id) {
            bail!("cannot discard a loaded Runtime thread");
        }
        let _thread_mutation = self.store.thread_mutation.lock();
        let thread = self.store.load_thread(thread_id)?;
        if thread.latest_turn_id.is_some() {
            bail!("cannot discard a Runtime thread that owns turns");
        }
        // Drop the thread's shell authority with it: the manager owns any
        // API-created jobs, and dropping the last handle kills them.
        active.shell_managers.remove(thread_id);
        drop(active);
        self.store.remove_thread(thread_id)?;
        // A deleted conversation keeps no session grants behind it.
        self.take_approval_grants(thread_id);
        Ok(())
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<ThreadRecord> {
        let mut active = self.active.lock().await;
        if active
            .engines
            .get(thread_id)
            .is_some_and(|state| state.active_turn.is_some())
        {
            bail!("thread {thread_id} already has an active turn; stop it before deleting");
        }
        let _thread_mutation = self.store.thread_mutation.lock();
        let thread = self.store.load_thread(thread_id)?;
        active.lru.retain(|id| id != thread_id);
        active.shell_managers.remove(thread_id);
        let evicted = active.engines.remove(thread_id).map(|state| state.engine);
        for turn in self.store.list_turns_for_thread(thread_id)? {
            for item_id in &turn.item_ids {
                self.store.remove_item(item_id)?;
            }
            self.store.remove_turn(&turn.id)?;
        }
        remove_file_if_exists(&self.store.events_path(thread_id)?)?;
        remove_file_if_exists(&self.store.goal_path(thread_id)?)?;
        self.store.remove_seed_journal(thread_id)?;
        self.store.remove_thread(thread_id)?;
        drop(active);
        if let Some(engine) = evicted {
            engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
            let _ = engine.try_send(Op::Shutdown);
        }
        self.take_approval_grants(thread_id);
        Ok(thread)
    }

    pub async fn list_threads(
        &self,
        filter: ThreadListFilter,
        limit: Option<usize>,
    ) -> Result<Vec<ThreadRecord>> {
        let mut threads = self.store.list_threads()?;
        match filter {
            ThreadListFilter::ActiveOnly => threads.retain(|t| !t.archived),
            ThreadListFilter::ArchivedOnly => threads.retain(|t| t.archived),
            ThreadListFilter::IncludeArchived => {}
        }
        if let Some(limit) = limit {
            threads.truncate(limit);
        }
        Ok(threads)
    }

    /// Threads with at least one queued or in-progress turn, for
    /// running-work accounting (#6180). One turns scan grouped by thread
    /// (never a scan per thread, #3757), joined against the thread rows in
    /// store order. Archive state is ignored: see [`RunningThread`].
    pub async fn running_threads(&self) -> Result<Vec<RunningThread>> {
        let mut active_by_thread: std::collections::BTreeMap<String, Vec<ActiveTurn>> =
            std::collections::BTreeMap::new();
        for turn in self.store.list_all_turns()? {
            if turn.status.is_active_work() {
                active_by_thread
                    .entry(turn.thread_id.clone())
                    .or_default()
                    .push(ActiveTurn {
                        turn_id: turn.id.clone(),
                        status: turn.status,
                    });
            }
        }
        if active_by_thread.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(active_by_thread.len());
        for thread in self.store.list_threads()? {
            if let Some(active_turns) = active_by_thread.remove(&thread.id) {
                out.push(RunningThread {
                    thread_id: thread.id.clone(),
                    model: thread.model.clone(),
                    title: thread.title.clone(),
                    active_turns,
                });
            }
        }
        Ok(out)
    }

    /// Flush queued recovery receipts for every thread on a page.
    ///
    /// [`Self::get_thread_detail`] flushes per thread, and the summary route
    /// reached it row by row, so a listing used to settle every recovered turn
    /// it displayed: the flush cancels that turn's pending user inputs and its
    /// unresolvable dynamic tools, and publishes the `turn.completed` a crashed
    /// turn is missing. `pending_attention_count` reads exactly that pending
    /// state, so dropping the flush would leave a page reporting attention for
    /// turns that are already over until somebody opened the thread. Now that
    /// rows no longer go through detail, doing it here keeps what the route
    /// publishes. A thread with nothing queued costs one lock and a map lookup
    /// — only startup recovery queues receipts — and the flush has to precede
    /// the scan, because it changes what the scan then reports.
    pub async fn flush_recovery_receipts(&self, thread_ids: &[String]) -> Result<()> {
        for thread_id in thread_ids {
            self.flush_recovery_receipts_for_thread(thread_id).await?;
        }
        Ok(())
    }

    /// Read the store's item index ahead of the read that needs it.
    ///
    /// Opening a thread is the first thing every client asks for and the last
    /// thing the store can answer cheaply: a reader has to learn which items
    /// each turn owns, and nothing but a pass over the whole items directory can
    /// tell it, because an item's filename carries the item id and not its turn.
    /// That pass is the same one every open used to pay. Paid here, while the
    /// Runtime API is starting and no client is waiting, it is paid once
    /// instead. See [`RuntimeThreadStore::ensure_item_index`].
    pub fn warm_item_index(&self) -> Result<()> {
        self.store.ensure_item_index()
    }

    /// The [`ThreadListFacts`] for every id in `thread_ids`, read in one pass.
    ///
    /// `GET /v1/threads/summary` used to call [`Self::get_thread_detail`] once
    /// per row. Detail is a whole-store walk — `list_turns_for_thread` scans
    /// every turn record and `list_items_for_turns_map` scans every item record,
    /// because item filenames carry only the item id —
    /// so a listing of `T` threads cost `T x (N + M)` JSON reads and parses:
    /// ~25s for 72 threads against a 189MB store, measured 2026-09-17, which is
    /// the cheap end of the store growing. This harvests the same facts with one
    /// turns scan and one items scan, so the cost is `T + N + M`.
    ///
    /// This is a best-effort snapshot, and deliberately cheaper than detail in
    /// three ways: it takes no per-thread projection lock, and it does not flush
    /// recovery receipts or read the event cursor. A listing may therefore lag a
    /// turn that is mid-flight. That is the right trade for a list — the
    /// alternative is what made the call seconds-per-thread — and
    /// `pending_attention_count` still comes from live in-memory state, so
    /// attention grouping stays immediate. Callers wanting a consistent
    /// projection of one thread still want [`Self::get_thread_detail`].
    pub async fn thread_list_facts(
        &self,
        thread_ids: &[String],
    ) -> Result<HashMap<String, ThreadListFacts>> {
        if thread_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let wanted: HashSet<String> = thread_ids.iter().cloned().collect();

        let store = self.store.clone();
        let scanned = wanted.clone();
        let (turns_by_thread, preview_by_thread) = tokio::task::spawn_blocking(move || {
            // One turns scan, grouped by thread. `list_all_turns` sorts by
            // `created_at`, so each group keeps ascending turn order and a
            // group's last element is the newest turn — the same turn a
            // per-thread detail read would have called `turns.last()`.
            let mut turns_by_thread: HashMap<String, Vec<TurnRecord>> = HashMap::new();
            for turn in store.list_all_turns()? {
                if scanned.contains(&turn.thread_id) {
                    turns_by_thread
                        .entry(turn.thread_id.clone())
                        .or_default()
                        .push(turn);
                }
            }
            // The preview is read from each row's own newest turn, not from a
            // page-wide scan of every item record: see
            // `newest_message_text_by_thread` for what that cost and why.
            let preview_by_thread = store.newest_message_text_by_thread(&turns_by_thread)?;
            Ok::<_, anyhow::Error>((turns_by_thread, preview_by_thread))
        })
        .await
        .context("Runtime thread list scan task failed")??;

        let mut facts = HashMap::with_capacity(wanted.len());
        for thread_id in &wanted {
            let turns = turns_by_thread.get(thread_id);
            let latest_turn = turns.and_then(|turns| turns.last());
            // Newest turn first, resolved by the walk inside
            // `newest_message_text_by_thread`: the first turn holding a message
            // is the message a per-thread detail read would have found.
            let preview = preview_by_thread.get(thread_id).cloned();
            let (pending_approvals, pending_user_inputs) =
                self.pending_requests_for_thread(thread_id);
            facts.insert(
                thread_id.clone(),
                ThreadListFacts {
                    latest_turn_status: latest_turn
                        .map(|turn| format!("{:?}", turn.status).to_ascii_lowercase()),
                    latest_turn_input_summary: latest_turn.map(|turn| turn.input_summary.clone()),
                    preview,
                    pending_attention_count: pending_approvals
                        .len()
                        .saturating_add(pending_user_inputs.len()),
                    newest_item_id: turns.map(Vec::as_slice).and_then(newest_recorded_item_id),
                },
            );
        }
        Ok(facts)
    }

    /// Raise a watchable notice on a thread (#6180). Kind names must stay in
    /// the [`codewhale_config::notifications::NotificationEvent`] vocabulary
    /// so clients and `[notifications.events]` gates agree. Oldest-first
    /// eviction past [`MAX_NOTICES_PER_THREAD`] keeps the list bounded.
    pub fn raise_notice(
        &self,
        thread_id: &str,
        kind: &str,
        turn_id: &str,
        subject: &str,
        detail: String,
    ) -> String {
        push_runtime_notice(&self.notices, thread_id, kind, turn_id, subject, detail)
    }

    /// Notices currently raised on a thread, oldest first. Unknown threads
    /// simply have none; the API layer maps thread existence to 404.
    pub fn list_notices(&self, thread_id: &str) -> Vec<ActiveNotice> {
        self.notices
            .lock()
            .get(thread_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Acknowledge one notice. Returns false when the notice (or thread) is
    /// unknown; acking is idempotent from the client's view via GET.
    pub fn ack_notice(&self, thread_id: &str, notice_id: &str) -> bool {
        let mut notices = self.notices.lock();
        let Some(list) = notices.get_mut(thread_id) else {
            return false;
        };
        let before = list.len();
        list.retain(|notice| notice.id != notice_id);
        list.len() != before
    }

    /// Drop every notice about one subject on a thread. Elevation notices
    /// clear this way when their tool call completes, however it completed.
    pub fn clear_notices_for_subject(&self, thread_id: &str, subject: &str) {
        let mut notices = self.notices.lock();
        if let Some(list) = notices.get_mut(thread_id) {
            list.retain(|notice| notice.subject != subject);
        }
    }

    /// Whether `/v1/threads/summary?search=` should keep this thread.
    ///
    /// Matches fields already on the thread record (`id`, explicit `title`,
    /// `model`). When the title is unset, peeks the latest turn file for the
    /// displayed title (`input_summary`) — one JSON read, not a whole-store
    /// scan. Preview text lives on items and is not a search key: using it as
    /// one forced `get_thread_detail` (itself a full turns+items directory
    /// walk) on every thread, including non-matches.
    pub fn thread_matches_summary_search(&self, thread: &ThreadRecord, search: &str) -> bool {
        if thread.id.to_ascii_lowercase().contains(search)
            || thread.model.to_ascii_lowercase().contains(search)
        {
            return true;
        }
        if let Some(title) = thread
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_ascii_lowercase().contains(search);
        }
        thread
            .latest_turn_id
            .as_deref()
            .and_then(|turn_id| self.store.load_turn(turn_id).ok())
            .is_some_and(|turn| turn.input_summary.to_ascii_lowercase().contains(search))
    }

    /// Aggregate token + cost usage across all threads/turns inside the time
    /// range `[since, until]`. Each parent, child, and compaction call is
    /// computed via provider-aware pricing using its persisted concrete route.
    /// Legacy turns without provider provenance and providers without an
    /// authoritative runtime price (including ChatGPT/Codex OAuth) accrue
    /// tokens but no fabricated dollar cost. Whalescale#261 / #564.
    ///
    /// Buckets are sorted by ascending key for deterministic output. Empty
    /// ranges produce empty `buckets` (never an error).
    pub async fn aggregate_usage(
        &self,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        group_by: UsageGroupBy,
    ) -> Result<UsageAggregation> {
        let store = self.store.clone();
        let (threads, turns_by_thread) = tokio::task::spawn_blocking(move || {
            let threads = store.list_threads()?;
            let mut turns_by_thread: HashMap<String, Vec<TurnRecord>> = HashMap::new();
            if let Some(first) = threads.first() {
                validated_record_id(&first.id, "thread id")?;
                for turn in store.list_all_turns()? {
                    turns_by_thread
                        .entry(turn.thread_id.clone())
                        .or_default()
                        .push(turn);
                }
            }
            Ok::<_, anyhow::Error>((threads, turns_by_thread))
        })
        .await
        .context("Runtime usage aggregation task failed")??;

        let mut buckets: std::collections::BTreeMap<String, UsageBucket> =
            std::collections::BTreeMap::new();
        let mut totals = UsageTotals::default();
        for thread in &threads {
            validated_record_id(&thread.id, "thread id")?;
            for turn in turns_by_thread.get(&thread.id).into_iter().flatten() {
                let parent_route = turn.effective_route_envelope();
                let parent_dispatched_at = parent_route
                    .as_ref()
                    .map_or(turn.created_at, |route| route.dispatched_at);
                if let Some(usage) = turn.effective_route_usage.as_ref().or(turn.usage.as_ref())
                    && usage_timestamp_in_range(parent_dispatched_at, since, until)
                {
                    accumulate_runtime_usage_record(
                        &mut totals,
                        &mut buckets,
                        group_by,
                        parent_route.as_ref(),
                        usage,
                        turn,
                        thread,
                    );
                }
                for receipt in &turn.decision_receipts {
                    if usage_timestamp_in_range(receipt.route.dispatched_at, since, until) {
                        if totals
                            .route_receipts
                            .iter()
                            .filter(|r| r.starts_with("decision:"))
                            .count()
                            < 64
                        {
                            totals.route_receipts.insert(receipt.diagnostic_receipt());
                        } else {
                            totals
                                .route_receipts
                                .insert("decision:diagnostic_receipt_bound_reached".to_string());
                        }
                    }
                }
                for child in &turn.routed_usage {
                    if usage_timestamp_in_range(child.route.dispatched_at, since, until) {
                        accumulate_runtime_child_usage_record(
                            &mut totals,
                            &mut buckets,
                            group_by,
                            child,
                            turn,
                            thread,
                        );
                    }
                }
                for drop_record in &turn.routed_usage_drop_records {
                    if usage_timestamp_in_range(drop_record.route.dispatched_at, since, until) {
                        accumulate_exact_runtime_usage_drop(
                            &mut totals,
                            &mut buckets,
                            group_by,
                            &drop_record.route,
                            drop_record.reason,
                            turn,
                            thread,
                        );
                    }
                }
                // Dropped fallback receipts no longer carry a trustworthy
                // dispatch timestamp. Use the owning turn timestamp only to
                // decide whether the explicit incompleteness marker belongs
                // in this query window; never fabricate a model/provider.
                if usage_timestamp_in_range(turn.created_at, since, until) {
                    accumulate_truncated_runtime_usage(
                        &mut totals,
                        &mut buckets,
                        group_by,
                        turn.routed_usage_dropped_records,
                        turn,
                        thread,
                    );
                }
            }
        }

        let group_by_str = match group_by {
            UsageGroupBy::Day => "day",
            UsageGroupBy::Model => "model",
            UsageGroupBy::Provider => "provider",
            UsageGroupBy::Thread => "thread",
        }
        .to_string();

        totals.cost_complete = totals.unpriced_turns == 0;
        for bucket in buckets.values_mut() {
            bucket.cost_complete = bucket.unpriced_turns == 0;
        }

        Ok(UsageAggregation {
            since,
            until,
            group_by: group_by_str,
            totals,
            buckets: buckets.into_values().collect(),
        })
    }

    /// Thread-scoped token + cost totals for one thread's whole history,
    /// split by spend owner.
    ///
    /// Reuses the exact per-record accumulation of [`Self::aggregate_usage`]
    /// (parent usage, routed child usage, dropped-record markers) so the
    /// per-thread figure and the global `/v1/usage` figure can never disagree
    /// about how a turn is priced — including the CNY coverage rule. The
    /// parent/child split mirrors the session-persistence field semantics
    /// (`session_cost_*` vs `subagent_cost_*`), so a session resumed across
    /// the TUI and runtime writers never double-counts child spend. A
    /// missing thread is an error, matching [`Self::get_thread`].
    pub async fn aggregate_usage_for_thread(&self, id: &str) -> Result<ThreadUsageSplit> {
        let store = self.store.clone();
        let thread_id = id.to_string();
        let (thread, turns) = tokio::task::spawn_blocking(move || {
            let thread = store
                .load_thread(&thread_id)
                .with_context(|| format!("Thread not found: {thread_id}"))?;
            let turns = store.list_turns_for_thread(&thread_id)?;
            Ok::<_, anyhow::Error>((thread, turns))
        })
        .await
        .context("Runtime thread usage aggregation task failed")??;

        let mut split = ThreadUsageSplit::default();
        // Buckets are a discarded byproduct: the shared accumulator writes
        // both totals and buckets, and this surface reports totals only.
        let mut buckets: std::collections::BTreeMap<String, UsageBucket> =
            std::collections::BTreeMap::new();
        for turn in &turns {
            let parent_route = turn.effective_route_envelope();
            if let Some(usage) = turn.effective_route_usage.as_ref().or(turn.usage.as_ref()) {
                accumulate_runtime_usage_record(
                    &mut split.parent,
                    &mut buckets,
                    UsageGroupBy::Thread,
                    parent_route.as_ref(),
                    usage,
                    turn,
                    &thread,
                );
            }
            for child in &turn.routed_usage {
                accumulate_runtime_child_usage_record(
                    &mut split.routed_children,
                    &mut buckets,
                    UsageGroupBy::Thread,
                    child,
                    turn,
                    &thread,
                );
            }
            for drop_record in &turn.routed_usage_drop_records {
                accumulate_exact_runtime_usage_drop(
                    &mut split.routed_children,
                    &mut buckets,
                    UsageGroupBy::Thread,
                    &drop_record.route,
                    drop_record.reason,
                    turn,
                    &thread,
                );
            }
            // Dropped fallback receipts are routed-child records, so the
            // incompleteness marker lands on the child side.
            accumulate_truncated_runtime_usage(
                &mut split.routed_children,
                &mut buckets,
                UsageGroupBy::Thread,
                turn.routed_usage_dropped_records,
                turn,
                &thread,
            );
        }
        finalize_usage_totals(&mut split.parent);
        finalize_usage_totals(&mut split.routed_children);
        Ok(split)
    }

    /// One turn's artifact references, read from the store: the turn's own
    /// aggregate once it has ended, or its items' refs merged on the fly
    /// while it runs. `Ok(None)` when the turn does not exist or belongs to
    /// another thread.
    pub async fn turn_artifacts(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<TurnArtifactsView>> {
        let thread = self.get_thread(thread_id).await?;
        if validated_record_id(turn_id, "turn id").is_err() {
            return Ok(None);
        }
        let manager = self.clone();
        let thread_id = thread_id.to_string();
        let turn_id = turn_id.to_string();
        tokio::task::spawn_blocking(move || {
            if !manager.store.turn_path(&turn_id)?.exists() {
                return Ok(None);
            }
            let turn = manager.store.load_turn(&turn_id)?;
            if turn.thread_id != thread_id {
                return Ok(None);
            }
            let item_artifacts = manager.item_artifact_refs(&turn);
            let artifacts = if turn.workspace.is_some() {
                turn.artifacts
            } else {
                turn_artifacts::merge_turn_artifacts(&item_artifacts, None).artifacts
            };
            Ok(Some(TurnArtifactsView {
                thread_id,
                turn_id,
                workspace: turn.workspace,
                artifacts,
                item_artifacts,
                thread_workspace: thread.workspace,
            }))
        })
        .await
        .context("turn artifact read task failed")?
    }

    /// The workspace span one tool call of one turn ran in, read from the
    /// store: the `tool:<call_id>` restore point the call started from and the
    /// `post-tool:<call_id>` receipt that closed it.
    ///
    /// A span exists only for a call that may write. With
    /// `EngineConfig::record_restore_points` the engine brackets every
    /// non-read-only call — a file tool, a shell command, a program, a
    /// write-capable MCP tool — so a shell command's own writes are
    /// attributable to it. A call the engine judged read-only takes neither
    /// receipt, and a turn recorded before receipts existed carries none;
    /// `Ok(None)` says so, and a caller must report that as "not bounded"
    /// rather than as an empty change list.
    pub async fn turn_call_span(
        &self,
        thread_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<CallWorkspaceSpan>> {
        let thread = self.get_thread(thread_id).await?;
        if validated_record_id(turn_id, "turn id").is_err()
            || !usable_provider_call_id(tool_call_id)
        {
            return Ok(None);
        }
        let manager = self.clone();
        let thread_id = thread_id.to_string();
        let turn_id = turn_id.to_string();
        let tool_call_id = tool_call_id.to_string();
        tokio::task::spawn_blocking(move || {
            if !manager.store.turn_path(&turn_id)?.exists() {
                return Ok(None);
            }
            let turn = manager.store.load_turn(&turn_id)?;
            if turn.thread_id != thread_id {
                return Ok(None);
            }
            let recorded = |kind: codewhale_snapshot::WorkspaceSnapshotKind| {
                turn.workspace_snapshots.iter().rfind(|receipt| {
                    receipt.kind == kind
                        && receipt.tool_call_id.as_deref() == Some(tool_call_id.as_str())
                })
            };
            let pre = recorded(codewhale_snapshot::WorkspaceSnapshotKind::Tool);
            let post = recorded(codewhale_snapshot::WorkspaceSnapshotKind::PostTool);
            let item = manager.item_for_call(&turn, &tool_call_id)?;
            // A call the turn recorded no item for is a call this turn never
            // ran: the caller asked about the wrong turn. A call with an item
            // but no receipt is the read-only case — known, and bounded by
            // nothing, which the route reports rather than 404s.
            if pre.is_none() && post.is_none() && item.is_none() {
                return Ok(None);
            }
            let tool_name = item
                .as_ref()
                .and_then(|item| item.metadata.as_ref())
                .and_then(|metadata| metadata.get("tool_name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(Some(CallWorkspaceSpan {
                thread_id,
                turn_id,
                tool_call_id,
                tool_name,
                item_status: item.as_ref().map(|item| item.status),
                pre_tool_snapshot_id: pre.map(|receipt| receipt.tree_id.clone()),
                post_tool_snapshot_id: post.map(|receipt| receipt.tree_id.clone()),
                thread_workspace: thread.workspace,
            }))
        })
        .await
        .context("call workspace span read task failed")?
    }

    /// The turn's item record for one call id, when any item carries it.
    ///
    /// `tool_use_id` is the identity the engine also labels the call's
    /// `tool:<call_id>` restore point with, so this is how a receipt is tied
    /// back to the tool that ran.
    pub(super) fn item_for_call(
        &self,
        turn: &TurnRecord,
        tool_call_id: &str,
    ) -> Result<Option<TurnItemRecord>> {
        for item_id in &turn.item_ids {
            let item = self.store.load_item(item_id)?;
            if item
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("tool_use_id"))
                .and_then(Value::as_str)
                == Some(tool_call_id)
            {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    pub async fn get_thread(&self, id: &str) -> Result<ThreadRecord> {
        self.flush_recovery_receipts_for_thread(id).await?;
        self.store
            .load_thread(id)
            .with_context(|| format!("Thread not found: {id}"))
    }

    // Unit fixtures without a runtime source use the ordinary default source.
    // Production callers must pass the source actually loaded by their host.
    #[cfg(test)]
    pub(super) async fn update_thread(
        &self,
        id: &str,
        req: UpdateThreadRequest,
    ) -> Result<ThreadRecord> {
        self.update_thread_with_shell_policy(id, req, None, None)
            .await
    }

    pub async fn update_thread_with_shell_policy(
        &self,
        id: &str,
        req: UpdateThreadRequest,
        config_path: Option<&Path>,
        config_profile: Option<&str>,
    ) -> Result<ThreadRecord> {
        // Keep policy publication ordered with this authorization decision.
        let _config_admission = self.config_admission.read().await;
        self.update_thread_under_history_guard(id, req, config_path, config_profile)
            .await
    }

    pub async fn update_thread_under_history_guard(
        &self,
        id: &str,
        req: UpdateThreadRequest,
        config_path: Option<&Path>,
        config_profile: Option<&str>,
    ) -> Result<ThreadRecord> {
        if req.archived.is_none()
            && req.allow_shell.is_none()
            && req.trust_mode.is_none()
            && req.auto_approve.is_none()
            && req.model.is_none()
            && req.mode.is_none()
            && req.permission_posture.is_none()
            && req.title.is_none()
            && req.system_prompt.is_none()
            && req.workspace.is_none()
            && req.model_provider.is_none()
            && req.model_provider_id.is_none()
        {
            bail!("At least one thread field is required");
        }

        if let Some(model) = req.model.as_ref()
            && model.trim().is_empty()
        {
            bail!("model must not be empty");
        }
        if let Some(mode) = req.mode.as_ref()
            && mode.trim().is_empty()
        {
            bail!("mode must not be empty");
        }
        if let Some(permission_posture) = req.permission_posture.as_ref()
            && permission_posture.trim().is_empty()
        {
            bail!("permission_posture must not be empty");
        }
        if let Some(workspace) = req.workspace.as_ref()
            && workspace.as_os_str().is_empty()
        {
            bail!("workspace must not be empty");
        }

        // A provider switch resolves and preflights the target route before
        // anything is saved, exactly like the TUI's `/provider`: a provider
        // that cannot serve the next turn is refused, not recorded. History
        // stays with the thread; the next turn installs the new route.
        let provider_switch = {
            let config = self.read_config().clone();
            match requested_provider_identity(
                &config,
                req.model_provider.as_deref(),
                req.model_provider_id.as_deref(),
            )? {
                Some(identity) => {
                    let current_model = self.get_thread(id).await?.model;
                    let requested_model = req.model.clone().or_else(|| {
                        current_model
                            .trim()
                            .eq_ignore_ascii_case("auto")
                            .then_some(current_model)
                    });
                    let route =
                        ready_provider_route(&config, &identity, requested_model.as_deref())?;
                    let model = match requested_model {
                        Some(model) if model.trim().eq_ignore_ascii_case("auto") => model,
                        _ => route.model.clone(),
                    };
                    Some((
                        identity.persisted_kind().to_string(),
                        identity.persisted_id().map(str::to_string),
                        model,
                    ))
                }
                None => None,
            }
        };

        // Source resolution reads config files. Do it off the Tokio worker,
        // then recheck the conversation identity before committing the grant.
        let shell_policy_workspace = if req.allow_shell == Some(true) || req.workspace.is_some() {
            let current = self.get_thread(id).await?;
            if req.allow_shell.unwrap_or(current.allow_shell) {
                let workspace = req.workspace.clone().unwrap_or(current.workspace);
                self.validate_shell_access_policy(&workspace, config_path, config_profile)
                    .await?;
                Some(workspace)
            } else {
                None
            }
        } else {
            None
        };
        let configured_sandbox_mode = self.read_config().sandbox_mode.clone();
        let (thread, changes, evicted_engine, posture_engine, ended_grants, permission_receipt) = {
            // Take the active guard first so a workspace mutation can check
            // and evict the cached engine atomically with the durable update.
            // Using the same order as start/compact avoids lock inversion.
            let mut active = self.active.lock().await;
            let _thread_mutation = self.store.thread_mutation.lock();
            let mut thread = self
                .store
                .load_thread(id)
                .with_context(|| format!("Thread not found: {id}"))?;
            if req.expected_idle == Some(true)
                && active
                    .engines
                    .get(id)
                    .and_then(|state| state.active_turn.as_ref())
                    .is_some()
            {
                bail!("thread '{id}' already has an active turn; finish it before this update");
            }
            // Shell opt-in broadens only an idle conversation. Check while
            // holding the same active + record locks used by turn admission.
            if req.allow_shell == Some(true)
                && !thread.allow_shell
                && active
                    .engines
                    .get(id)
                    .and_then(|state| state.active_turn.as_ref())
                    .is_some()
            {
                bail!(
                    "thread '{id}' already has an active turn; finish it before enabling shell commands"
                );
            }
            if req.allow_shell.unwrap_or(thread.allow_shell)
                && (req.allow_shell.is_some() || req.workspace.is_some())
                && shell_policy_workspace.as_deref()
                    != Some(req.workspace.as_deref().unwrap_or(&thread.workspace))
            {
                bail!(
                    "thread permissions changed during update; refresh the conversation before trying again"
                );
            }
            let mut changes = serde_json::Map::new();
            let policy_patch = if req.mode.is_some()
                || req.permission_posture.is_some()
                || req.auto_approve.is_some()
            {
                Some(runtime_policy_with_overrides(
                    &thread,
                    req.mode.as_deref(),
                    req.permission_posture.as_deref(),
                    req.auto_approve,
                )?)
            } else {
                None
            };
            let previous_policy = RuntimePolicyProjection::from_persisted(
                &thread.mode,
                thread.permission_posture.as_deref(),
                thread.auto_approve,
            );

            if let Some(archived) = req.archived
                && thread.archived != archived
            {
                thread.archived = archived;
                changes.insert("archived".to_string(), json!(archived));
            }
            if let Some(allow_shell) = req.allow_shell
                && thread.allow_shell != allow_shell
            {
                thread.allow_shell = allow_shell;
                changes.insert("allow_shell".to_string(), json!(allow_shell));
            }
            if let Some(trust_mode) = req.trust_mode
                && thread.trust_mode != trust_mode
            {
                thread.trust_mode = trust_mode;
                changes.insert("trust_mode".to_string(), json!(trust_mode));
            }
            let requested_model = match provider_switch {
                Some((model_provider, model_provider_id, model)) => {
                    if thread.model_provider.as_deref() != Some(model_provider.as_str())
                        || thread.model_provider_id != model_provider_id
                    {
                        changes.insert("model_provider".to_string(), json!(model_provider));
                        changes.insert("model_provider_id".to_string(), json!(model_provider_id));
                        thread.model_provider = Some(model_provider);
                        thread.model_provider_id = model_provider_id;
                    }
                    Some(model)
                }
                None => req.model,
            };
            if let Some(model) = requested_model
                && thread.model != model
            {
                thread.model = model.clone();
                changes.insert("model".to_string(), json!(model));
            }
            if let Some(policy) = policy_patch {
                let mode = policy.mode_setting().to_string();
                let permission_posture = Some(policy.permission_wire().to_string());
                let auto_approve = policy.auto_approve();
                if thread.mode != mode {
                    thread.mode = mode.clone();
                    changes.insert("mode".to_string(), json!(mode));
                }
                if thread.permission_posture != permission_posture {
                    thread.permission_posture = permission_posture.clone();
                    changes.insert("permission_posture".to_string(), json!(permission_posture));
                }
                if thread.auto_approve != auto_approve {
                    thread.auto_approve = auto_approve;
                    changes.insert("auto_approve".to_string(), json!(auto_approve));
                }
            }
            if let Some(title) = req.title {
                // Empty string clears a previously-set title and reverts to derived.
                let new_title = if title.trim().is_empty() {
                    None
                } else {
                    Some(title)
                };
                if thread.title != new_title {
                    thread.title = new_title.clone();
                    changes.insert("title".to_string(), json!(new_title));
                }
            }
            if let Some(system_prompt) = req.system_prompt {
                let new_sys = if system_prompt.trim().is_empty() {
                    None
                } else {
                    Some(system_prompt)
                };
                if thread.system_prompt != new_sys {
                    thread.system_prompt = new_sys.clone();
                    changes.insert("system_prompt".to_string(), json!(new_sys));
                }
            }
            if let Some(workspace) = req.workspace
                && thread.workspace != workspace
            {
                changes.insert("workspace".to_string(), json!(workspace));
                thread.workspace = workspace;
            }

            let workspace_changed = changes.contains_key("workspace");
            let rebuild_engine = workspace_changed || changes.contains_key("system_prompt");
            if rebuild_engine
                && active
                    .engines
                    .get(id)
                    .and_then(|state| state.active_turn.as_ref())
                    .is_some()
            {
                bail!(
                    "{} cannot be changed while the thread has an active turn",
                    if workspace_changed {
                        "workspace"
                    } else {
                        "system_prompt"
                    }
                );
            }

            // A posture/mode edit must reach the live engine even while a
            // turn is running. EngineHandle publishes the authority snapshot
            // before queueing ChangeMode; the turn loop applies that pending
            // update before the next tool batch.
            let posture_changed = changes.contains_key("auto_approve")
                || changes.contains_key("permission_posture")
                || changes.contains_key("trust_mode")
                || changes.contains_key("allow_shell")
                || changes.contains_key("mode");

            let evicted_engine = if changes.is_empty() {
                None
            } else {
                thread.updated_at = Utc::now();
                self.store.save_thread(&thread)?;
                if rebuild_engine {
                    active.lru.retain(|thread_id| thread_id != id);
                    active.engines.remove(id).map(|state| state.engine)
                } else {
                    None
                }
            };
            let posture_engine = if posture_changed && !rebuild_engine {
                active.engines.get(id).map(|state| state.engine.clone())
            } else {
                None
            };
            let permission_receipt = policy_patch.map(|policy| {
                let label = |policy: RuntimePolicyProjection| {
                    crate::core::authority::permission_label_for(
                        codewhale_localization::Locale::En,
                        policy.mode,
                        policy.permission,
                    )
                    .into_owned()
                };
                (label(previous_policy), label(policy))
            });
            // Archiving ends the conversation's session grants: unarchiving
            // later starts from a clean slate and the next call prompts.
            let ended_grants = if changes.get("archived") == Some(&json!(true)) {
                self.take_approval_grants(id)
            } else {
                Vec::new()
            };
            (
                thread,
                changes,
                evicted_engine,
                posture_engine,
                ended_grants,
                permission_receipt,
            )
        };

        for grant in ended_grants {
            self.emit_event(
                &thread.id,
                None,
                None,
                "approval.grant_revoked",
                json!({ "grant": grant }),
            )
            .await?;
        }

        if let Some(engine) = evicted_engine {
            let _ = engine.send(Op::Shutdown).await;
        }

        // Keep the live engine session converged with the thread record.
        // Idle engines apply it immediately; a running turn applies it at
        // the next mid-turn drain (before the next tool batch).
        if let Some(engine) = posture_engine {
            let policy = RuntimePolicyProjection::from_persisted(
                &thread.mode,
                thread.permission_posture.as_deref(),
                thread.auto_approve,
            );
            let _ = engine.try_send(Op::ChangeMode {
                mode: policy.mode,
                allow_shell: thread.allow_shell,
                trust_mode: thread.trust_mode,
                auto_approve: policy.auto_approve(),
                approval_mode: policy.permission,
                configured_sandbox_mode: configured_sandbox_mode.clone(),
            });
        }

        if !changes.is_empty() {
            self.emit_event(
                &thread.id,
                None,
                None,
                "thread.updated",
                json!({
                    "thread": thread.clone(),
                    "changes": Value::Object(changes),
                }),
            )
            .await?;
        }

        if let Some((from, to)) = permission_receipt
            && from != to
        {
            self.publish_permissions_changed(&thread, &from, &to)
                .await?;
        }

        Ok(thread)
    }

    pub(super) async fn publish_permissions_changed(
        &self,
        thread: &ThreadRecord,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let message = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::PermissionsChangedReceipt,
        )
        .replace("{label}", to);
        if let Some(turn_id) = thread.latest_turn_id.as_deref() {
            let item = TurnItemRecord {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                id: runtime_record_id("item"),
                turn_id: turn_id.to_string(),
                kind: TurnItemKind::Status,
                status: TurnItemLifecycleStatus::Completed,
                summary: summarize_text(&message, SUMMARY_LIMIT),
                detail: Some(message),
                metadata: Some(json!({ "permissions_changed": { "from": from, "to": to } })),
                artifact_refs: Vec::new(),
                artifacts: Vec::new(),
                started_at: Some(Utc::now()),
                ended_at: Some(Utc::now()),
            };
            self.store.save_item(&item)?;
            self.attach_item_to_turn(turn_id, &item.id)?;
            self.emit_event(
                &thread.id,
                Some(turn_id),
                Some(&item.id),
                "item.completed",
                json!({ "item": item }),
            )
            .await?;
        }
        self.emit_event(
            &thread.id,
            None,
            None,
            "thread.permissions_changed",
            json!({ "from": from, "to": to, "source": "runtime_api" }),
        )
        .await?;
        Ok(())
    }

    /// Save/resume holds this existing admission lease from snapshot through
    /// binding, so a newer turn cannot be mistaken for part of the snapshot.
    pub async fn session_checkpoint_guard(&self) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        Arc::clone(&self.config_admission).write_owned().await
    }

    pub fn try_history_import_guard(&self) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>> {
        Arc::clone(&self.config_admission)
            .try_write_owned()
            .map_err(|_| anyhow!("canonical history admission is busy; retry the same operation"))
    }

    pub fn reserve_history_operation(
        &self,
        key: &str,
        request_digest: &str,
        history_digest: &str,
    ) -> Result<RuntimeHistoryOperation> {
        self.reserve_history_operation_for_target(key, request_digest, history_digest, None)
    }

    pub fn reserve_history_operation_for_target(
        &self,
        key: &str,
        request_digest: &str,
        history_digest: &str,
        target: Option<(&str, &str)>,
    ) -> Result<RuntimeHistoryOperation> {
        validate_runtime_turn_operation_key(key)?;
        validate_sha256_fingerprint(request_digest, "history request digest")?;
        validate_sha256_fingerprint(history_digest, "history digest")?;
        let fingerprint = codewhale_runtime::hashing::sha256_hex(
            format!("codewhale:history-operation:v1\0{key}").as_bytes(),
        );
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                if let Some(operation) = self.store.load_history_operation(&fingerprint)? {
                    anyhow::ensure!(
                        operation.receipt.operation_key == key
                            && operation.receipt.request_digest == request_digest
                            && operation.receipt.history_digest == history_digest,
                        "history operation key already binds different data"
                    );
                    let binding = self.session_store_binding();
                    anyhow::ensure!(
                        operation.receipt.data_dir == binding.data_dir
                            && operation.receipt.execution_scope == binding.execution_scope,
                        "history operation belongs to a different Runtime store"
                    );
                    return Ok(operation);
                }
                let binding = self.session_store_binding();
                if let Some((thread, session)) = target {
                    anyhow::ensure!(
                        is_runtime_session_scope(thread) && is_runtime_session_scope(session),
                        "invalid existing history target identity"
                    );
                }
                let operation = RuntimeHistoryOperation {
                    version: 1,
                    key_fingerprint: fingerprint.clone(),
                    committed: false,
                    created_at: Utc::now(),
                    journal_witness: None,
                    association: None,
                    target_document_digest: None,
                    goal_source_digest: None,
                    goal_target_digest: None,
                    session_goal_source_digest: None,
                    session_goal_target_digest: None,
                    receipt: codewhale_protocol::CanonicalThreadReceipt {
                        version: 1,
                        data_dir: binding.data_dir,
                        execution_scope: binding.execution_scope,
                        operation_key: key.into(),
                        request_digest: request_digest.into(),
                        history_digest: history_digest.into(),
                        runtime_thread_id: target
                            .map(|(thread, _)| thread.to_string())
                            .unwrap_or_else(|| runtime_record_id("thr")),
                        session_id: target
                            .map(|(_, session)| session.to_string())
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    },
                };
                self.store.save_history_operation(&operation)?;
                Ok(operation)
            })
    }

    pub fn lookup_history_operation(
        &self,
        key: &str,
        request_digest: &str,
    ) -> Result<Option<RuntimeHistoryOperation>> {
        validate_sha256_fingerprint(request_digest, "history request digest")?;
        let operation = self.lookup_history_operation_by_key(key)?;
        if let Some(operation) = operation.as_ref() {
            anyhow::ensure!(
                operation.receipt.request_digest == request_digest,
                "operation key belongs to a different request body"
            );
        }
        Ok(operation)
    }

    pub fn lookup_history_operation_by_key(
        &self,
        key: &str,
    ) -> Result<Option<RuntimeHistoryOperation>> {
        validate_runtime_turn_operation_key(key)?;
        let fingerprint = codewhale_runtime::hashing::sha256_hex(
            format!("codewhale:history-operation:v1\0{key}").as_bytes(),
        );
        let operation = self.store.load_history_operation(&fingerprint)?;
        if let Some(operation) = operation.as_ref() {
            let binding = self.session_store_binding();
            anyhow::ensure!(
                operation.receipt.operation_key == key
                    && operation.receipt.data_dir == binding.data_dir
                    && operation.receipt.execution_scope == binding.execution_scope,
                "operation key belongs to a different owner store"
            );
        }
        Ok(operation)
    }

    pub fn bind_history_operation_witness(
        &self,
        mut operation: RuntimeHistoryOperation,
        witness: RuntimeHistoryWitness,
        association: codewhale_protocol::CanonicalThreadOperationAssociation,
    ) -> Result<RuntimeHistoryOperation> {
        let fingerprint = operation.key_fingerprint.clone();
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                let current = self
                    .store
                    .load_history_operation(&fingerprint)?
                    .ok_or_else(|| anyhow!("history reservation is absent"))?;
                anyhow::ensure!(
                    current.receipt == operation.receipt,
                    "history operation changed before witness binding"
                );
                if let Some(previous) = current.journal_witness.as_ref() {
                    anyhow::ensure!(
                        previous == &witness && current.association.as_ref() == Some(&association),
                        "history operation journal witness or association changed"
                    );
                    return Ok(current);
                }
                anyhow::ensure!(
                    !current.committed,
                    "committed operation has no journal witness"
                );
                operation.journal_witness = Some(witness);
                operation.association = Some(association);
                operation.validate()?;
                self.store.save_history_operation(&operation)?;
                Ok(operation)
            })
    }

    /// Bind the prepared full target document before its protected publication.
    /// An old Resume document/checkpoint alone cannot prove a new intent settled.
    pub fn bind_history_operation_document(
        &self,
        operation: RuntimeHistoryOperation,
        digest: &str,
    ) -> Result<RuntimeHistoryOperation> {
        validate_sha256_fingerprint(digest, "history target document digest")?;
        let fingerprint = operation.key_fingerprint.clone();
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                let mut current = self
                    .store
                    .load_history_operation(&fingerprint)?
                    .ok_or_else(|| anyhow!("history reservation is absent"))?;
                anyhow::ensure!(
                    current.receipt == operation.receipt && !current.committed,
                    "history operation changed before target publication"
                );
                if let Some(previous) = current.target_document_digest.as_ref() {
                    anyhow::ensure!(
                        previous == digest,
                        "prepared history target document changed"
                    );
                } else {
                    current.target_document_digest = Some(digest.to_owned());
                    current.validate()?;
                    self.store.save_history_operation(&current)?;
                }
                Ok(current)
            })
    }

    /// Same Engine mailbox and immutable thread policy as ordinary hydration.
    /// Local goal sidecar witnesses share this intent's existing commit record.
    pub fn bind_history_operation_session_goal(
        &self,
        operation: RuntimeHistoryOperation,
        source: &str,
        target: &str,
    ) -> Result<RuntimeHistoryOperation> {
        validate_sha256_fingerprint(source, "history source session goal")?;
        validate_sha256_fingerprint(target, "history target session goal")?;
        let fingerprint = operation.key_fingerprint.clone();
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                let mut current = self
                    .store
                    .load_history_operation(&fingerprint)?
                    .ok_or_else(|| anyhow!("history goal reservation is absent"))?;
                anyhow::ensure!(
                    current.receipt == operation.receipt && !current.committed,
                    "history operation changed before local goal preparation"
                );
                if let Some(previous) = current.session_goal_source_digest.as_ref() {
                    anyhow::ensure!(
                        previous == source
                            && current.session_goal_target_digest.as_deref() == Some(target),
                        "history local goal source/outcome changed"
                    );
                } else {
                    current.session_goal_source_digest = Some(source.to_owned());
                    current.session_goal_target_digest = Some(target.to_owned());
                    current.validate()?;
                    self.store.save_history_operation(&current)?;
                }
                Ok(current)
            })
    }

    /// A queued snapshot is the acknowledgement barrier; this sends no turn.
    pub async fn synchronize_history_operation(
        &self,
        thread_id: &str,
        session: &crate::session_manager::SavedSession,
    ) -> Result<()> {
        let thread = self.get_thread(thread_id).await?;
        let engine = self.ensure_engine_loaded(&thread).await?;
        let system_prompt =
            synced_system_prompt(thread.system_prompt.as_deref(), &session.messages);
        let expected = {
            let checkpoint = crate::core::engine::compaction_checkpoint_for_restore(
                &session.messages,
                system_prompt.clone(),
            );
            crate::core::engine::project_history_for_restore(
                session.messages.clone(),
                checkpoint.as_ref(),
            )
        };
        engine
            .send(Op::SyncSession {
                session_id: Some(thread.id.clone()),
                messages: session.messages.clone(),
                system_prompt_override: system_prompt.is_some(),
                system_prompt,
                model: thread.model.clone(),
                workspace: thread.workspace.clone(),
                mode: RuntimePolicyProjection::from_persisted(
                    &thread.mode,
                    thread.permission_posture.as_deref(),
                    thread.auto_approve,
                )
                .mode,
            })
            .await?;
        let observed = engine.get_session_snapshot().await?;
        anyhow::ensure!(
            observed.session_id == thread.id
                && observed.messages == expected
                && observed.workspace == thread.workspace,
            "canonical history mailbox acknowledgement changed"
        );
        Ok(())
    }

    pub fn history_operation_thread(&self, id: &str) -> Result<Option<ThreadRecord>> {
        match self.store.load_thread(id) {
            Ok(thread) => Ok(Some(thread)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Metadata adoption on the existing goal/intent stores. Captured source
    /// progress is historical; it grants neither tool authority nor execution.
    pub fn adopt_history_goal(
        &self,
        operation: &RuntimeHistoryOperation,
        source_thread_id: &str,
        source: Option<codewhale_protocol::ThreadGoal>,
    ) -> Result<()> {
        let Some(source) = source else {
            return Ok(());
        };
        anyhow::ensure!(
            source.thread_id == source_thread_id
                && !source.goal_id.is_empty()
                && source.goal_id.len() <= 128
                && !source.goal_id.chars().any(char::is_control)
                && !source.objective.trim().is_empty()
                && source.objective.len() <= 64 * 1024
                && source.tokens_used >= 0
                && source.time_used_seconds >= 0
                && source.continuation_count >= 0
                && source.token_budget.is_none_or(|budget| budget > 0),
            "invalid imported goal source"
        );
        source.validate_stall_state().map_err(anyhow::Error::msg)?;
        let source_digest = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&source)?).as_bytes(),
        );
        let mut target = source;
        target
            .thread_id
            .clone_from(&operation.receipt.runtime_thread_id);
        if matches!(target.status, codewhale_protocol::ThreadGoalStatus::Active) {
            target.status = codewhale_protocol::ThreadGoalStatus::Paused;
            target.pause_reason = None;
        }
        target.normalize_restored_stall_state();
        let target_digest = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&serde_json::to_value(&target)?).as_bytes(),
        );
        let fingerprint = operation.key_fingerprint.clone();
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                let mut current = self
                    .store
                    .load_history_operation(&fingerprint)?
                    .ok_or_else(|| anyhow!("history goal reservation is absent"))?;
                anyhow::ensure!(
                    current.receipt == operation.receipt,
                    "history goal intent changed"
                );
                if let Some(previous) = current.goal_source_digest.as_ref() {
                    anyhow::ensure!(
                        previous == &source_digest
                            && current.goal_target_digest.as_ref() == Some(&target_digest),
                        "history goal source/outcome changed"
                    );
                    // A committed intent proves the original settled adoption. Later
                    // legitimate goal updates/clear must never be overwritten on retry.
                    if current.committed {
                        return Ok(());
                    }
                } else {
                    anyhow::ensure!(
                        !current.committed,
                        "committed import lost its goal witness; recovery required"
                    );
                    current.goal_source_digest = Some(source_digest);
                    current.goal_target_digest = Some(target_digest);
                    self.store.save_history_operation(&current)?;
                }
                let _goal = self.store.goal_mutation.lock();
                match self.store.load_goal(&target.thread_id)? {
                    Some(previous) => anyhow::ensure!(
                        previous == target,
                        "canonical target already has a conflicting goal; source retained"
                    ),
                    None => self.store.save_goal_under_lock(&target)?,
                }
                Ok(())
            })
    }

    pub fn commit_history_operation(
        &self,
        operation: RuntimeHistoryOperation,
    ) -> Result<codewhale_protocol::CanonicalThreadReceipt> {
        let fingerprint = operation.key_fingerprint.clone();
        self.store
            .with_turn_operation_claim(Some(&fingerprint), || {
                let mut current = self
                    .store
                    .load_history_operation(&operation.key_fingerprint)?
                    .ok_or_else(|| anyhow!("history reservation is absent"))?;
                anyhow::ensure!(
                    current.receipt == operation.receipt,
                    "history reservation changed before commit"
                );
                let thread = self
                    .store
                    .load_thread(&operation.receipt.runtime_thread_id)?;
                anyhow::ensure!(
                    thread.session_id.as_deref() == Some(operation.receipt.session_id.as_str())
                        && thread.saved_session_checkpoint.is_some(),
                    "history operation has no durable canonical session checkpoint"
                );
                current.committed = true;
                self.store.save_history_operation(&current)?;
                Ok(current.receipt.clone())
            })
    }

    /// Exclude new turns and saved-history/config changes while a restore
    /// mutates files. Active turns in any overlapping workspace (the same
    /// tree, a parent or a nested checkout) are rejected instead of raced.
    /// Callers move the owned guard into the worker that performs the Git
    /// mutation so client cancellation cannot release it early.
    pub async fn workspace_restore_guard(
        &self,
        workspace: &Path,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>> {
        let admission = self.session_checkpoint_guard().await;
        self.reject_active_turns_in_workspace(workspace).await?;
        Ok(admission)
    }

    /// Same reservation as [`Self::workspace_restore_guard`], but the thread
    /// record is read *under* admission so trust, session binding and
    /// workspace are the values that hold while the restore runs.
    pub async fn thread_restore_guard(
        &self,
        id: &str,
    ) -> Result<(tokio::sync::OwnedRwLockWriteGuard<()>, ThreadRecord)> {
        let admission = self.session_checkpoint_guard().await;
        let thread = self.get_thread(id).await?;
        self.reject_active_turns_in_workspace(&thread.workspace)
            .await?;
        Ok((admission, thread))
    }

    /// Lock order: `config_admission` (held by the caller) before `active`,
    /// matching turn admission and config reload.
    pub async fn reject_active_turns_in_workspace(&self, workspace: &Path) -> Result<()> {
        // A workspace directory that no longer exists cannot host a running
        // tool; compare the recorded path as-is in that case.
        let workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        // Collect ids under the async lock, then do filesystem and store
        // I/O without holding it.
        let busy: Vec<String> = {
            let active = self.active.lock().await;
            active
                .engines
                .iter()
                .filter(|(_, state)| state.active_turn.is_some())
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in busy {
            let other = self.store.load_thread(&id)?.workspace;
            let other = other.canonicalize().unwrap_or(other);
            if workspace.starts_with(&other) || other.starts_with(&workspace) {
                bail!(
                    "Workspace already has an active turn (thread {id}); wait for it to finish before restoring files"
                );
            }
        }
        Ok(())
    }

    /// Every workspace restore point recorded on this thread's turns — the
    /// snapshots the thread owns, including those its cloned (forked) turns
    /// carry. Callers hold `thread_restore_guard`, so no turn is recording.
    pub fn thread_workspace_snapshots(
        &self,
        thread_id: &str,
    ) -> Result<Vec<codewhale_snapshot::WorkspaceSnapshotRef>> {
        Ok(self
            .store
            .list_turns_for_thread(thread_id)?
            .into_iter()
            .flat_map(|turn| turn.workspace_snapshots)
            .collect())
    }

    /// Test seam: build every later engine with `client` as its model client.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_model_client(&self, client: crate::core::model_client::SharedModelClient) {
        *self.test_model_client.lock() = Some(client);
    }

    /// Test seam: mark or clear an active turn on an installed test engine so
    /// route-level tests can exercise restore admission.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn set_active_turn_for_test(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
    ) -> Result<()> {
        let mut active = self.active.lock().await;
        let state = active
            .engines
            .get_mut(thread_id)
            .ok_or_else(|| anyhow!("no engine installed for {thread_id}"))?;
        state.active_turn = turn_id.map(|turn_id| ActiveTurnState {
            narrowing: crate::core::engine::TurnNarrowing::Inherit,
            turn_id: turn_id.to_string(),
            goal_id: None,
            goal_progress: None,
            interrupt_requested: false,
            compaction_id: None,
        });
        Ok(())
    }

    /// Bind full-fidelity saved history to the exact prefix it covers. Callers
    /// hold session_checkpoint_guard across the snapshot, save and this write.
    pub async fn set_thread_session_checkpoint(
        &self,
        thread_id: &str,
        session: &crate::session_manager::SavedSession,
    ) -> Result<()> {
        let session_id = &session.metadata.id;
        let messages_sha256 = session_messages_sha256(&session.messages)?;
        let thread = {
            let _thread_mutation = self.store.thread_mutation.lock();
            let mut thread = self.store.load_thread(thread_id)?;
            thread.session_id = Some(session_id.clone());
            thread.saved_session_checkpoint = Some(SavedSessionCheckpoint {
                covered_turn_id: thread.latest_turn_id.clone(),
                messages_sha256,
                messages_len: Some(session.messages.len()),
                retained_messages: None,
            });
            thread.updated_at = Utc::now();
            self.store.save_thread(&thread)?;
            thread
        };
        self.emit_event(
            thread_id,
            None,
            None,
            "thread.updated",
            json!({ "thread": thread, "changes": { "session_id": session_id } }),
        )
        .await?;
        Ok(())
    }

    /// Close the items a terminal turn still lists as live, in the snapshot the
    /// caller already holds.
    ///
    /// A stream retry — system sleep, a mid-stream network drop — discards a
    /// reasoning or message fragment without emitting its completion frame,
    /// and builds before the boundary closes dropped the superseded pointer
    /// without closing the record. The turn itself went on to complete, so no
    /// restart recovery touches it, and nothing else in the transcript path
    /// can: the row reads `in_progress` inside a finished turn forever.
    ///
    /// Called from [`Self::get_thread_detail`], where the items are already in
    /// memory — the repair therefore costs no extra I/O and happens once per
    /// affected thread, the first time somebody opens it. A live turn is left
    /// alone: its monitor owns those records and is still writing them.
    ///
    /// No event is emitted. Appending one here would land after the snapshot's
    /// `latest_seq` and so be invisible to a client resuming from it; a client
    /// that reloads reads the repaired record, which is what this changes.
    async fn repair_terminal_turn_items(
        &self,
        thread_id: &str,
        turns: &[TurnRecord],
        items: Vec<TurnItemRecord>,
    ) -> Result<Vec<TurnItemRecord>> {
        let mut terminal: HashMap<&str, (TurnItemLifecycleStatus, DateTime<Utc>)> = HashMap::new();
        for turn in turns {
            if matches!(
                turn.status,
                RuntimeTurnStatus::Completed
                    | RuntimeTurnStatus::Failed
                    | RuntimeTurnStatus::Interrupted
                    | RuntimeTurnStatus::Canceled
            ) {
                terminal.insert(
                    turn.id.as_str(),
                    (
                        terminal_item_status(turn.status),
                        turn.ended_at.unwrap_or_else(Utc::now),
                    ),
                );
            }
        }
        if terminal.is_empty() {
            return Ok(items);
        }
        let mut items = items;
        let mut repaired: Vec<TurnItemRecord> = Vec::new();
        for item in &mut items {
            if !matches!(
                item.status,
                TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
            ) {
                continue;
            }
            let Some((status, ended_at)) = terminal.get(item.turn_id.as_str()).copied() else {
                continue;
            };
            item.status = status;
            item.summary =
                summarize_text(item.detail.as_deref().unwrap_or_default(), SUMMARY_LIMIT);
            item.ended_at = Some(ended_at);
            repaired.push(item.clone());
        }
        if repaired.is_empty() {
            return Ok(items);
        }
        tracing::info!(
            thread_id,
            items = repaired.len(),
            "closed items a terminal turn was still listing as live"
        );
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let records: Vec<&TurnItemRecord> = repaired.iter().collect();
            store.save_items_batch(&records)
        })
        .await
        .context("Terminal item repair persistence task failed")??;
        Ok(items)
    }

    pub async fn get_thread_detail(&self, id: &str) -> Result<ThreadDetail> {
        self.flush_recovery_receipts_for_thread(id).await?;
        // Hold the per-thread projection boundary from cursor capture through
        // item reads. A streamed delta is therefore either entirely before
        // this snapshot (materialized item + included cursor) or entirely
        // after it (old item + replayable delta), never both.
        let projection_lock = self.projection_lock(id);
        let _projection = projection_lock.lock().await;
        let latest_seq = self.store.current_seq().await?;

        #[cfg(any(test, feature = "test-support"))]
        let snapshot_test_hook = { self.snapshot_test_hook.lock().take() };
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = snapshot_test_hook {
            let (resume, wait_for_resume) = oneshot::channel();
            hook.send(SnapshotTestPoint {
                thread_id: id.to_string(),
                latest_seq,
                resume,
            })
            .map_err(|_| anyhow!("snapshot test hook closed"))?;
            wait_for_resume
                .await
                .map_err(|_| anyhow!("snapshot test hook dropped resume"))?;
        }

        // Recovery was flushed before taking the non-reentrant projection
        // lock. Do not call `get_thread` here: a receipt queued between that
        // flush and this read would re-enter recovery and wait forever on the
        // projection lock held by this snapshot.
        let store = self.store.clone();
        let snapshot_thread_id = id.to_string();
        let (thread, turns, items, command_receipts) = tokio::task::spawn_blocking(move || {
            let thread = store
                .load_thread(&snapshot_thread_id)
                .with_context(|| format!("Thread not found: {snapshot_thread_id}"))?;
            let turns = store.list_turns_for_thread(&snapshot_thread_id)?;
            let turn_ids: Vec<String> = turns.iter().map(|turn| turn.id.clone()).collect();
            let mut items_by_turn = store.list_items_for_turns_map(&turn_ids)?;
            let mut items = Vec::new();
            for turn in &turns {
                if let Some(mut turn_items) = items_by_turn.remove(&turn.id) {
                    items.append(&mut turn_items);
                }
            }
            let mut command_receipts = VecDeque::new();
            store.for_each_event(&snapshot_thread_id, |event| {
                if event.seq <= latest_seq && event.event == "extension_command_completed" {
                    let result: RuntimeCommandResult =
                        serde_json::from_value(event.payload["result"].clone())
                            .context("command receipt is invalid")?;
                    if result.turn.is_none() {
                        let fingerprint = event.payload["operation_fingerprint"]
                            .as_str()
                            .context("command receipt identity is absent")?;
                        validate_sha256_fingerprint(fingerprint, "command receipt identity")?;
                        if command_receipts.len() == 100 {
                            command_receipts.pop_front();
                        }
                        command_receipts.push_back(RuntimeCommandReceipt {
                            id: format!("command_{fingerprint}"),
                            created_at: event.timestamp,
                            name: result.name,
                            origin: result.origin,
                            status: result.status,
                            text: result.text,
                            extension: result.extension,
                        });
                    }
                }
                Ok(())
            })?;
            Ok::<_, anyhow::Error>((thread, turns, items, command_receipts.into_iter().collect()))
        })
        .await
        .context("Runtime thread projection task failed")??;
        // The projection also repairs what an earlier process left behind — a
        // terminal turn still listing a live item. Doing it here costs nothing
        // extra (the items are already in memory) and touches only the threads
        // somebody opens, instead of making every launch read every item in
        // every store to find the few that need it.
        let items = self.repair_terminal_turn_items(id, &turns, items).await?;
        let (pending_approvals, pending_user_inputs) = self.pending_requests_for_thread(id);
        let pending_dynamic_tool_calls = self.pending_dynamic_tool_calls_for_thread(id);
        Ok(ThreadDetail {
            thread,
            turns,
            items,
            command_receipts,
            latest_seq,
            pending_approvals,
            pending_user_inputs,
            pending_dynamic_tool_calls,
            approval_grants: self.approval_grants_for_thread(id),
        })
    }

    pub async fn resume_thread(&self, id: &str) -> Result<ThreadRecord> {
        let thread = self.get_thread(id).await?;
        self.ensure_engine_loaded(&thread).await?;
        self.grants.resume_thread(id);
        Ok(thread)
    }

    /// The active thread that already holds this saved session, when hydrating
    /// it would work.
    ///
    /// `POST /v1/sessions/{id}/resume-thread` mints a thread, so resuming a
    /// conversation that is *already open* added a second rail row for it, once
    /// per visit. The binding that answers this is already durable on the
    /// thread record (`session_id`), so the runtime can say "this conversation
    /// is already open" itself instead of leaving every client to remember it.
    ///
    /// Archived threads do not count: the rail hides them, so handing one back
    /// would move the conversation where the user cannot see it. A thread whose
    /// checkpoint no longer describes the bytes in the session file is skipped
    /// too — [`Self::saved_session_prefix`] refuses to hydrate those, so
    /// offering it back would trade a duplicate row for a load failure.
    ///
    /// A thread with a turn in flight *does* count. It is the conversation the
    /// user asked to continue, and the alternative — minting a second thread
    /// exactly when the first one is busy — is where the duplicates came from.
    /// A client that lands on one has to treat its composer as steering rather
    /// than as a new turn (`start_turn` refuses a busy thread by design).
    /// Another unarchived thread in this store bound to `session_id`.
    pub fn thread_bound_to_session(
        &self,
        session_id: &str,
        except_thread_id: &str,
    ) -> Option<String> {
        self.store
            .list_threads()
            .ok()?
            .into_iter()
            .find(|thread| {
                !thread.archived
                    && thread.id != except_thread_id
                    && thread.session_id.as_deref() == Some(session_id)
            })
            .map(|thread| thread.id)
    }

    pub fn thread_holding_session(
        &self,
        session_id: &str,
        session: &crate::session_manager::SavedSession,
    ) -> Option<ThreadRecord> {
        // Store order is newest-first, so the first match is the newest binding
        // and the one a client most likely means.
        for thread in self.store.list_threads().ok()? {
            if thread.archived || thread.session_id.as_deref() != Some(session_id) {
                continue;
            }
            // The checkpoint must cover the *whole* document. Hydration
            // accepts a prefix (the document may have grown since the bind),
            // but that thread shows the prefix plus its own turns, so anything
            // another writer appended after the bind would be missing from it.
            // A grown document gets a fresh thread holding all of it instead.
            if let Some(checkpoint) = thread.saved_session_checkpoint.as_ref()
                && !matches!(
                    checkpoint_prefix_len(checkpoint, &session.messages),
                    Ok(Some(len)) if len == session.messages.len()
                )
            {
                continue;
            }
            // An unreadable turn list proves nothing about whether this thread
            // can be hydrated, so it skips the candidate — it must not end the
            // search for every remaining one.
            let Ok(turns) = self.store.list_turns_for_thread(&thread.id) else {
                continue;
            };
            if matches!(self.saved_session_prefix(&thread, &turns), Ok(Some(_))) {
                return Some(thread);
            }
        }
        None
    }

    /// Capture the same bounded protected source for prefix proof and full
    /// graph copying. The source lease survives preparation and file rollback.
    pub(super) async fn capture_native_fork_source(
        &self,
        source: &ThreadRecord,
        turns: &[TurnRecord],
        sessions_dir: &Path,
    ) -> Result<(NativeForkSource, Option<(Vec<Message>, usize)>)> {
        let runtime = self.clone();
        let source = source.clone();
        let turns = turns.to_vec();
        let sessions_dir = sessions_dir.to_path_buf();
        history::history_owner_work(move || {
            anyhow::ensure!(
                sessions_dir == runtime.sessions_dir(),
                "native fork sessions directory differs from the captured Runtime owner"
            );
            let manager = crate::session_manager::SessionManager::new(sessions_dir.clone())?;
            let (session, goal, lease, prefix) = if let Some(id) = source.session_id.as_deref() {
                let lease = manager.reserve_session_for_external_write(id)?;
                let session = manager.load_session_snapshot_bounded(
                    id,
                    codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
                )?;
                anyhow::ensure!(
                    session.metadata.workspace == source.workspace
                        && session
                            .metadata
                            .runtime_store
                            .as_ref()
                            .is_none_or(|binding| binding == &runtime.session_store_binding()),
                    "native fork source belongs to another workspace or Runtime owner"
                );
                let goal = manager.load_session_goal(id)?;
                let prefix =
                    runtime.saved_session_prefix_from_session(&source, &turns, session.clone())?;
                (Some(session), goal, Some(lease), Some(prefix))
            } else {
                (None, None, None, None)
            };
            Ok((
                NativeForkSource {
                    sessions_dir,
                    session,
                    goal,
                    lease,
                },
                prefix,
            ))
        })
        .await
    }

    /// Every fork owns a full graph document. A failed copy refuses publication;
    /// it never leaves two Runtime threads writing the same saved session.
    pub(super) async fn bind_fork_to_own_session(
        &self,
        mut forked: ThreadRecord,
        captured: NativeForkSource,
        prefix: Vec<Message>,
        covered_turn_id: Option<String>,
    ) -> Result<(
        ThreadRecord,
        Option<crate::session_manager::SessionLease>,
        crate::session_manager::SessionLease,
    )> {
        let binding = self.session_store_binding();
        history::history_owner_work(move || {
            let manager = crate::session_manager::SessionManager::new(captured.sessions_dir)?;
            if let Some(source) = captured.session.as_ref() {
                let current = manager.load_session_snapshot_bounded(
                    &source.metadata.id,
                    codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
                )?;
                anyhow::ensure!(
                    history::saved_document_digest(&current)?
                        == history::saved_document_digest(source)?
                        && manager.load_session_goal(&source.metadata.id)? == captured.goal,
                    "native fork source document or local goal changed; source retained"
                );
            }
            let mut session = match captured.session {
                Some(source) => {
                    let parent = source.metadata.clone();
                    let mut session = source;
                    let journal = session
                        .journal
                        .as_ref()
                        .ok_or_else(|| anyhow!("native fork source full journal missing"))?
                        .fork_from(None)
                        .map_err(anyhow::Error::msg)?;
                    session.journal = Some(journal);
                    session.metadata.mark_forked_from(&parent);
                    session
                }
                None => crate::session_manager::create_saved_session_with_id_and_mode(
                    Uuid::new_v4().to_string(),
                    &[],
                    &forked.model,
                    &forked.workspace,
                    0,
                    forked
                        .system_prompt
                        .as_ref()
                        .map(|text| SystemPrompt::Text(text.clone()))
                        .as_ref(),
                    Some(&forked.mode),
                ),
            };
            let journal = session
                .journal
                .as_mut()
                .ok_or_else(|| anyhow!("native fork graph missing"))?;
            journal.rebranch_active_messages(&prefix);
            session.leaf_id = journal.leaf_id.clone();
            session.metadata.spawn_depth = journal.spawn_depth;
            session.messages = journal.to_messages();
            session.metadata.id = Uuid::new_v4().to_string();
            session.metadata.created_at = forked.created_at;
            session.metadata.updated_at = forked.updated_at;
            session.metadata.message_count = session.messages.len();
            session.metadata.workspace = forked.workspace.clone();
            session.metadata.runtime_store = Some(binding);
            session.metadata.set_model_provider_route(
                forked
                    .model_provider
                    .as_deref()
                    .ok_or_else(|| anyhow!("native fork lacks admitted provider identity"))?,
                forked.model_provider_id.as_deref(),
            );
            // Normalize only the private target using the existing recovery
            // codec. The raw source graph remains untouched as branch evidence.
            crate::session_manager::repair_recovered_session(&mut session);
            let journal = session
                .journal
                .as_ref()
                .ok_or_else(|| anyhow!("native repaired graph missing"))?;
            anyhow::ensure!(
                journal.entries.len() <= codewhale_protocol::MAX_CANONICAL_HISTORY_ENTRIES,
                "native fork full graph exceeds entry bound; source retained"
            );
            crate::session_manager::SavedSession::import_foreign(
                codewhale_runtime::session_tree::SessionImportContainer::new(
                    "native-fork-validation".into(),
                    journal,
                    None,
                ),
                forked.workspace.clone(),
                forked.model.clone(),
            )
            .map_err(anyhow::Error::msg)?;
            // Judge the fork's history the way its reader will: a document
            // repeats its own messages while it carries a journal, and
            // `load_session_snapshot_bounded` — which reads this fork back
            // on its next turn — accepts an oversized file whose carried form
            // fits. Measuring the stored shape here refused forks of
            // conversations the runtime was still opening, saving and running
            // turns on.
            history::ensure_history_transport_fits(&session)?;
            let session_id = session.metadata.id.clone();
            let target_lease = manager.reserve_session_for_external_write(&session_id)?;
            anyhow::ensure!(
                !manager.session_document_exists(&session_id),
                "native fork target already exists"
            );
            manager.save_session(&session)?;
            let mut goal = captured.goal;
            if let Some(goal) = goal.as_mut()
                && goal.status == crate::session_manager::SessionGoalStatus::Active
            {
                goal.status = crate::session_manager::SessionGoalStatus::Paused;
                goal.pause_reason = None;
            }
            manager.save_session_goal(&session_id, goal.as_ref())?;
            forked.session_id = Some(session_id);
            forked.saved_session_checkpoint = Some(SavedSessionCheckpoint {
                covered_turn_id,
                messages_sha256: session_messages_sha256(&session.messages)?,
                messages_len: Some(session.messages.len()),
                retained_messages: None,
            });
            Ok((forked, captured.lease, target_lease))
        })
        .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn fork_thread(&self, id: &str) -> Result<ThreadRecord> {
        self.fork_thread_in_sessions_dir(id, self.sessions_dir())
            .await
    }

    pub async fn fork_thread_in_sessions_dir(
        &self,
        id: &str,
        sessions_dir: &Path,
    ) -> Result<ThreadRecord> {
        let (admission, _) = self.thread_restore_guard(id).await?;
        let runtime = self.clone();
        let id = id.to_string();
        let dir = sessions_dir.to_path_buf();
        tokio::spawn(async move {
            let _admission = admission;
            runtime.fork_thread_under_guard(&id, &dir).await
        })
        .await
        .context("native fork outcome uncertain; inspect retained source and fork records")?
    }

    async fn fork_thread_under_guard(&self, id: &str, sessions_dir: &Path) -> Result<ThreadRecord> {
        let source = self.get_thread(id).await?;
        let mut forked = source.clone();
        let now = Utc::now();
        forked.id = runtime_record_id("thr");
        forked.created_at = now;
        forked.updated_at = now;
        forked.latest_turn_id = None;
        forked.archived = false;
        forked.last_read = None;
        forked.parent_thread_id = Some(source.id.clone());

        let source_turns = self.store.list_turns_for_thread(&source.id)?;
        // One read for every turn's items — see `prepared_items_for_turns`: a
        // whole-thread fork visits every turn, and the per-turn scan made that
        // quadratic over a store that only grows.
        let mut items_by_turn = self.prepared_items_for_turns(&source_turns)?;
        let (captured_source, saved_prefix) = self
            .capture_native_fork_source(&source, &source_turns, sessions_dir)
            .await?;
        let fork_prefix = if saved_transcript_requires_record_history(&source_turns, &items_by_turn)
        {
            (
                self.store
                    .history_from_records(&source_turns, &items_by_turn)?,
                source_turns.len(),
            )
        } else {
            saved_prefix.unwrap_or((
                reconstruct_messages_from_turns_with(&source_turns, &items_by_turn)?,
                source_turns.len(),
            ))
        };
        let mut cloned_records = Vec::with_capacity(source_turns.len());
        for source_turn in source_turns {
            let mut cloned_turn = source_turn.clone();
            cloned_turn.id = runtime_record_id("turn");
            cloned_turn.thread_id = forked.id.clone();
            cloned_turn.prefix_checkpoint = None;
            if let Some(checkpoint) = forked.saved_session_checkpoint.as_mut()
                && checkpoint.covered_turn_id.as_deref() == Some(source_turn.id.as_str())
            {
                checkpoint.covered_turn_id = Some(cloned_turn.id.clone());
            }
            cloned_turn.item_ids.clear();

            let items = items_by_turn.remove(&source_turn.id).unwrap_or_default();
            let mut cloned_items = Vec::with_capacity(items.len());
            for item in items {
                let mut cloned_item = item.clone();
                cloned_item.id = runtime_record_id("item");
                cloned_item.turn_id = cloned_turn.id.clone();
                cloned_turn.item_ids.push(cloned_item.id.clone());
                cloned_items.push(cloned_item);
            }
            if !cloned_turn.routing_settlement {
                forked.latest_turn_id = Some(cloned_turn.id.clone());
            }
            forked.updated_at = now;
            cloned_records.push((cloned_turn, cloned_items));
        }
        let (prefix, covered) = fork_prefix;
        let covered_turn_id = covered
            .checked_sub(1)
            .map(|index| cloned_records[index].0.id.clone());
        let (forked, _source_lease, _target_lease) = self
            .bind_fork_to_own_session(forked, captured_source, prefix, covered_turn_id)
            .await?;
        self.publish_fork(&forked, &cloned_records)?;

        self.emit_event(
            &forked.id,
            None,
            None,
            "thread.forked",
            json!({
                "thread": forked,
                "source_thread_id": source.id,
            }),
        )
        .await?;
        Ok(forked)
    }

    /// Fork a thread, dropping every turn from the Nth-from-tail user
    /// message onward (issue #133 — Esc-Esc backtrack).
    ///
    /// `depth_from_tail` selects which user turn to roll back *to*:
    ///
    /// - `0` — drop the most recent turn (the freshest user message and
    ///   everything after it)
    /// - `1` — drop the two most recent turns (rewind one further)
    /// - …and so on
    ///
    /// Returns a tuple of `(forked_thread, original_user_text)` where the
    /// second element is the `detail` of the first `UserMessage` item in
    /// the *first dropped* turn — i.e. the input the user typed to start
    /// that turn — so the caller can pre-populate the composer with it.
    /// `None` when no detail was recorded (defensive — every persisted
    /// `UserMessage` since v0.6 carries a detail string).
    ///
    /// Counts user turns over `list_turns_for_thread` (sorted
    /// oldest → newest). A turn is counted as a "user turn"
    /// when at least one of its items has `kind ==
    /// TurnItemKind::UserMessage`. Steered turns (which append additional
    /// `UserMessage` items) still count as one turn — backtrack rewinds
    /// at the turn boundary, not at the steer boundary. That predicate lives
    /// in `user_turn_indices`, which the named anchor is resolved through
    /// (`fork_cut_for_user_turn`) as well.
    ///
    /// Errors:
    /// - `depth_from_tail` exceeds the number of user turns
    /// - source thread not found
    #[cfg(any(test, feature = "test-support"))]
    pub async fn fork_at_user_message(
        &self,
        id: &str,
        depth: usize,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        self.fork_at_user_message_in_sessions_dir(id, depth, self.sessions_dir())
            .await
    }

    pub async fn fork_at_user_message_in_sessions_dir(
        &self,
        id: &str,
        depth: usize,
        sessions_dir: &Path,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        let (admission, _) = self.thread_restore_guard(id).await?;
        let runtime = self.clone();
        let id = id.to_string();
        let dir = sessions_dir.to_path_buf();
        tokio::spawn(async move {
            let _admission = admission;
            let prepared = runtime
                .prepare_fork_at_user_message_in_sessions_dir(&id, depth, &dir)
                .await?;
            runtime.publish_prepared_fork(prepared).await
        })
        .await
        .context("native backtrack outcome uncertain; inspect retained source and fork records")?
    }

    /// Fork a thread at a named user turn — the branch point a transcript's
    /// "continue from here" row names.
    ///
    /// The fork *keeps* that turn and every turn before it, and drops the
    /// turns after it; the first dropped turn's prompt comes back so a client
    /// can put it in the composer as the thing that was asked next, to edit or
    /// replace. Naming the last turn keeps every turn — a fork of the whole
    /// conversation — which is the same rule read at the end of the thread.
    ///
    /// `turn_id` is a turn id as `GET /v1/threads/{id}` reports it and must
    /// name a user turn (the same predicate [`Self::fork_at_user_message`]
    /// counts). This exists because a client cannot count those turns for
    /// itself: the transcript it renders and the turn store this cuts are not
    /// the same list — steers, image-only prompts and injected handoffs each
    /// sit on one side only — so a depth the client computed is an off-by-one
    /// waiting to fork the wrong prefix and report success. Naming the turn
    /// moves that decision to the side that owns the list.
    ///
    /// Like every other fork, this touches neither the source thread nor the
    /// workspace: it is a sibling conversation, never an undo. Rollback of
    /// the dropped turns' file changes is `/patch-undo` territory and is
    /// deliberately absent here — a fork that rewound the workspace would
    /// rewind it for the branch that was left behind too.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn fork_at_user_turn(
        &self,
        id: &str,
        turn_id: &str,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        self.fork_at_user_turn_in_sessions_dir(id, turn_id, self.sessions_dir())
            .await
    }

    pub async fn fork_at_user_turn_in_sessions_dir(
        &self,
        id: &str,
        turn_id: &str,
        sessions_dir: &Path,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        let (admission, _) = self.thread_restore_guard(id).await?;
        let runtime = self.clone();
        let id = id.to_string();
        let turn_id = turn_id.to_string();
        let dir = sessions_dir.to_path_buf();
        tokio::spawn(async move {
            let _admission = admission;
            let prepared = runtime
                .prepare_fork_at_user_turn_in_sessions_dir(&id, &turn_id, &dir)
                .await?;
            runtime.publish_prepared_fork(prepared).await
        })
        .await
        .context(
            "native anchored fork outcome uncertain; inspect retained source and fork records",
        )?
    }

    pub async fn fork_dropping_user_turn_in_sessions_dir(
        &self,
        id: &str,
        turn_id: &str,
        sessions_dir: &Path,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        let (admission, _) = self.thread_restore_guard(id).await?;
        let runtime = self.clone();
        let id = id.to_string();
        let turn_id = turn_id.to_string();
        let dir = sessions_dir.to_path_buf();
        tokio::spawn(async move {
            let _admission = admission;
            let turns = runtime.store.list_turns_for_thread(&id)?;
            let items_by_turn = runtime.prepared_items_for_turns(&turns)?;
            let (_, _, depth_from_tail) =
                runtime.fork_cut_for_user_turn(&turns, &items_by_turn, &turn_id)?;
            let prepared = runtime
                .prepare_fork_at_user_message_in_sessions_dir(&id, depth_from_tail, &dir)
                .await?;
            runtime.publish_prepared_fork(prepared).await
        })
        .await
        .context("native backtrack outcome uncertain; inspect retained source and fork records")?
    }

    /// How many turns a named anchor keeps, the first user turn it drops, and
    /// its distance from the tail.
    ///
    /// The fork keeps the anchor turn, so the cut is one past it: the branch
    /// point is the answer a person is looking at, not the question above it.
    /// The turn right after the anchor need not be a user turn — a manual
    /// `/compact` is a turn of its own with no prompt — so the receipt names
    /// the next *user* turn, which is what was asked next, while the cut still
    /// drops everything after the anchor.
    pub(super) fn fork_cut_for_user_turn(
        &self,
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
        turn_id: &str,
    ) -> Result<(usize, Option<usize>, usize)> {
        // Oldest → newest, so the named turn's position is a direct index.
        let user_turn_indices = Self::user_turn_indices(turns, items_by_turn);
        let position = user_turn_indices
            .iter()
            .position(|index| turns[*index].id == turn_id)
            .with_context(|| {
                format!("fork_at_user_turn: turn {turn_id} is not a user turn of this thread")
            })?;
        Ok((
            user_turn_indices[position] + 1,
            user_turn_indices.get(position + 1).copied(),
            user_turn_indices.len() - 1 - position,
        ))
    }

    /// The indices into `turns` that count as user turns, oldest → newest.
    ///
    /// A turn is a user turn when at least one of its items has `kind ==
    /// TurnItemKind::UserMessage`; a steered turn counts once, at its turn
    /// boundary, not once per steer. One home for that rule, so a
    /// tail-relative depth and a named-turn anchor can never disagree about
    /// which turn they mean. Free-standing because it reads nothing but the
    /// turns and the items already loaded for them.
    pub(super) fn user_turn_indices(
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
    ) -> Vec<usize> {
        let mut indices = Vec::new();
        for (idx, turn) in turns.iter().enumerate() {
            let is_user_turn = items_by_turn.get(&turn.id).is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.kind == TurnItemKind::UserMessage)
            });
            if is_user_turn {
                indices.push(idx);
            }
        }
        indices
    }

    /// Every item of every turn in `turns`, keyed by turn id.
    ///
    /// `list_items_for_turn` scans the store's whole items directory to find
    /// one turn's items, and a fork visits every turn: on a store holding tens
    /// of thousands of items that is seconds per turn, which is how a branch
    /// came to take minutes. One scan answers all of them — the same call the
    /// thread projection already reads a transcript with.
    pub(super) fn prepared_items_for_turns(
        &self,
        turns: &[TurnRecord],
    ) -> Result<HashMap<String, Vec<TurnItemRecord>>> {
        let turn_ids: Vec<String> = turns.iter().map(|turn| turn.id.clone()).collect();
        self.store.list_items_for_turns_map(&turn_ids)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn prepare_fork_at_user_message(
        &self,
        id: &str,
        depth_from_tail: usize,
    ) -> Result<PreparedThreadFork> {
        self.prepare_fork_at_user_message_in_sessions_dir(id, depth_from_tail, self.sessions_dir())
            .await
    }

    pub async fn prepare_fork_at_user_message_in_sessions_dir(
        &self,
        id: &str,
        depth_from_tail: usize,
        sessions_dir: &Path,
    ) -> Result<PreparedThreadFork> {
        let source = self.get_thread(id).await?;
        let source_turns = self.store.list_turns_for_thread(&source.id)?;
        // Every item of every turn in one read: see `prepared_items_for_turns`.
        let items_by_turn = self.prepared_items_for_turns(&source_turns)?;

        // Which turns count as user turns, oldest first; the depth names one
        // of them counting back from the end.
        let user_turn_indices = Self::user_turn_indices(&source_turns, &items_by_turn);
        if depth_from_tail >= user_turn_indices.len() {
            bail!(
                "fork_at_user_message: depth {} exceeds {} user turn(s)",
                depth_from_tail,
                user_turn_indices.len()
            );
        }
        let target_turn_idx = user_turn_indices[user_turn_indices.len() - 1 - depth_from_tail];
        // A depth-relative cut *drops* the turn it names — `/undo` removes the
        // exchange a person pointed at — where a named-turn cut keeps it. That
        // one turn is the whole difference between "undo this" and "branch
        // after this", and it lives here rather than in either caller.
        let (captured_source, saved_prefix) = self
            .capture_native_fork_source(&source, &source_turns, sessions_dir)
            .await?;
        self.prepare_fork_from_cutoff(
            source,
            captured_source,
            saved_prefix,
            source_turns,
            items_by_turn,
            target_turn_idx,
            Some(target_turn_idx),
            depth_from_tail,
        )
    }

    pub async fn prepare_fork_at_user_turn_in_sessions_dir(
        &self,
        id: &str,
        turn_id: &str,
        sessions_dir: &Path,
    ) -> Result<PreparedThreadFork> {
        let source = self.get_thread(id).await?;
        let source_turns = self.store.list_turns_for_thread(&source.id)?;
        let items_by_turn = self.prepared_items_for_turns(&source_turns)?;
        let (cutoff_turn_idx, receipt_turn_idx, depth_from_tail) =
            self.fork_cut_for_user_turn(&source_turns, &items_by_turn, turn_id)?;
        let (captured_source, saved_prefix) = self
            .capture_native_fork_source(&source, &source_turns, sessions_dir)
            .await?;
        self.prepare_fork_from_cutoff(
            source,
            captured_source,
            saved_prefix,
            source_turns,
            items_by_turn,
            cutoff_turn_idx,
            receipt_turn_idx,
            depth_from_tail,
        )
    }

    /// Clone the turns before `cutoff_turn_idx` into a sibling thread, and name
    /// the first *user* turn left behind (`receipt_turn_idx`, at or after the
    /// cutoff) in the receipt.
    ///
    /// One body for every fork that cuts a suffix: the depth-relative path
    /// (`/undo`, retry, backtrack) passes the anchor turn's own index and drops
    /// it, the named-anchor path passes one past its anchor and keeps it, so
    /// the cloning, the saved-session prefix and the receipt cannot drift apart
    /// between them.
    pub(super) fn prepare_fork_from_cutoff(
        &self,
        source: ThreadRecord,
        captured_source: NativeForkSource,
        saved_prefix: Option<(Vec<Message>, usize)>,
        source_turns: Vec<TurnRecord>,
        mut items_by_turn: HashMap<String, Vec<TurnItemRecord>>,
        cutoff_turn_idx: usize,
        receipt_turn_idx: Option<usize>,
        depth_from_tail: usize,
    ) -> Result<PreparedThreadFork> {
        // The first user turn the fork drops, when it drops any. Its prompt is
        // what the caller puts back in the composer: for a depth-relative cut
        // that is the turn being undone, and for an anchored cut it is the
        // question that followed the branch point — not a prompt-less turn
        // (a manual compaction, a routing settlement) that happens to sit
        // between them.
        debug_assert!(receipt_turn_idx.is_none_or(|idx| idx >= cutoff_turn_idx));
        let dropped_turns = source_turns
            .iter()
            .skip(cutoff_turn_idx)
            .map(|turn| {
                let items = items_by_turn.get(&turn.id).map_or(&[][..], Vec::as_slice);
                let ran_tools = items.iter().any(|item| {
                    matches!(
                        item.kind,
                        TurnItemKind::ToolCall
                            | TurnItemKind::FileChange
                            | TurnItemKind::CommandExecution
                    )
                });
                let ran_here = turn.permission_posture.is_some();
                let mut declared_writes = Vec::new();
                let mut unrun_tool_calls = std::collections::BTreeSet::new();
                for item in items {
                    let Some(meta) = item.metadata.as_ref() else {
                        continue;
                    };
                    let call_id = meta.get("tool_use_id").and_then(Value::as_str);
                    if matches!(
                        item.status,
                        TurnItemLifecycleStatus::Failed
                            | TurnItemLifecycleStatus::Canceled
                            | TurnItemLifecycleStatus::Queued
                    ) {
                        if let Some(call_id) = call_id {
                            unrun_tool_calls.insert(call_id.to_string());
                        }
                        continue;
                    }
                    let (Some(name), Some(input)) = (
                        meta.get("tool_name").and_then(Value::as_str),
                        meta.get("tool_input").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let Ok(input) = serde_json::from_str::<Value>(input) else {
                        continue;
                    };
                    if let Some(paths) =
                        crate::core::engine::file_write_tool_target_paths(name, &input)
                    {
                        declared_writes.extend(paths);
                    }
                }
                DroppedTurnSnapshots {
                    turn_id: turn.id.clone(),
                    may_change_files: !turn.routing_settlement
                        && !items.is_empty()
                        && (ran_tools || !ran_here),
                    snapshots: turn.workspace_snapshots.clone(),
                    declared_writes,
                    unrun_tool_calls,
                }
            })
            .collect();
        let dropped_turn = receipt_turn_idx.and_then(|idx| source_turns.get(idx));
        let dropped_turn_id = dropped_turn.map(|turn| turn.id.clone());
        let dropped_user_item = dropped_turn
            .and_then(|turn| items_by_turn.get(&turn.id))
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.kind == TurnItemKind::UserMessage)
            });
        let original_user_text = dropped_user_item
            .as_ref()
            .and_then(|item| item.detail.clone());
        let original_images = dropped_user_item
            .as_ref()
            .map(|item| {
                item.user_content()
                    .and_then(|content| crate::image_attach::runtime_images_from_blocks(&content))
            })
            .transpose()?
            .unwrap_or_default();
        // The next turn's allowance travels with the prompt it belongs to;
        // with nothing dropped there is no next turn to inherit from.
        let max_output_tokens = dropped_turn.and_then(|turn| turn.max_output_tokens);

        // Copy the turns before the cutoff into a new thread. Mirrors
        // `fork_thread` but stops at the cutoff instead of copying every turn.
        // Kept structurally close so future parity reviews can spot drift
        // between the two paths.
        let mut forked = source.clone();
        let now = Utc::now();
        forked.id = runtime_record_id("thr");
        forked.created_at = now;
        forked.updated_at = now;
        forked.latest_turn_id = None;
        forked.archived = false;
        forked.last_read = None;
        forked.parent_thread_id = Some(source.id.clone());

        // The fork keeps only the prefix it was cut at, in a document of its
        // own — see `bind_fork_to_own_session`.
        let mut fork_prefix: Option<(Vec<Message>, usize)> = None;
        if let Some((messages, covered)) = saved_prefix {
            let kept_turns = covered.min(cutoff_turn_idx);
            // A compacted source has no transcript to cut — see
            // `saved_transcript_requires_record_history`. Every boundary in its saved
            // messages names a turn while carrying only that turn's prompt, so
            // a slice of them would claim coverage of exchanges the fork's own
            // document does not hold. There is nothing for the boundary search
            // to find either, so it is skipped rather than risk a refusal for a
            // trim the fork is not going to take.
            let compacted = saved_transcript_requires_record_history(&source_turns, &items_by_turn);
            let retained_messages = if compacted {
                None
            } else if covered <= cutoff_turn_idx {
                Some(messages.len())
            } else {
                let kept_messages = reconstruct_messages_from_turns_with(
                    &source_turns[..cutoff_turn_idx],
                    &items_by_turn,
                )?;
                let kept_projection = session_recovery_projection(&kept_messages);
                // An exact projection match is the strongest proof: message for
                // message, this prefix *is* the kept history. It holds for a
                // transcript the records reproduce, and is tried first so those
                // shapes keep their exact boundary.
                // One walk, not one rebuild per prefix — see
                // `exact_prefix_boundary`.
                Some(
                    if let Some(count) = exact_prefix_boundary(&messages, &kept_projection) {
                        count
                    } else {
                        // It cannot hold for a real conversation: the model-visible
                        // transcript carries the per-turn `<turn_meta>` preamble and
                        // tool results as the route's compaction left them, neither
                        // of which the records keep. The prompt is recorded
                        // verbatim, so it still names the message the first dropped
                        // turn begins at — see `saved_history_boundary`.
                        //
                        // That turn is the one the kept history stops *before*: the
                        // anchor itself for a depth-relative cut, and the next user
                        // turn after the anchor for a fork at a named turn, which
                        // keeps it. When no user turn is dropped at all, every
                        // saved prompt belongs to a kept turn, and so does the
                        // whole saved transcript.
                        match dropped_turn {
                            None => messages.len(),
                            Some(dropped_turn) => {
                                let dropped_prompt = projected_user_texts(
                        &reconstruct_messages_from_turns_with(
                            std::slice::from_ref(dropped_turn),
                            &items_by_turn,
                        )?,
                    )
                    .into_iter()
                    .next()
                    .with_context(|| {
                        format!(
                            "Turn {} records no user prompt to align the saved history with; the source thread was preserved",
                            dropped_turn.id
                        )
                    })?;
                                saved_history_boundary(
                        &messages,
                        &projected_user_texts(&kept_messages),
                        &dropped_prompt,
                    )
                    .context("Cannot identify an exact saved-history boundary for this backtrack; the source thread was preserved")?
                            }
                        }
                    },
                )
            };
            let (prefix, covered_turns) = match retained_messages {
                Some(retained) => (messages[..retained].to_vec(), kept_turns),
                None => (
                    self.store
                        .history_from_records(&source_turns[..cutoff_turn_idx], &items_by_turn)?,
                    cutoff_turn_idx,
                ),
            };
            forked.saved_session_checkpoint = Some(SavedSessionCheckpoint {
                covered_turn_id: covered_turns
                    .checked_sub(1)
                    .map(|index| source_turns[index].id.clone()),
                // Preparation proves the source slice's original extent.
                // Publication replaces this with the private fork document's
                // complete checkpoint; a failed copy refuses the fork.
                messages_sha256: if compacted {
                    session_messages_sha256(&prefix)?
                } else {
                    match &source.saved_session_checkpoint {
                        Some(checkpoint) => checkpoint.messages_sha256.clone(),
                        None => session_messages_sha256(&messages)?,
                    }
                },
                // The fingerprint's own extent: the rebuilt prefix, the
                // source checkpoint's (a legacy `None` migrates on read), or
                // the whole verified legacy document.
                messages_len: if compacted {
                    Some(prefix.len())
                } else {
                    match &source.saved_session_checkpoint {
                        Some(checkpoint) => checkpoint.messages_len,
                        None => Some(messages.len()),
                    }
                },
                retained_messages: Some(prefix.len()),
            });
            fork_prefix = Some((prefix, covered_turns));
        }

        let mut cloned_records = Vec::with_capacity(cutoff_turn_idx);
        for source_turn in source_turns.iter().take(cutoff_turn_idx) {
            let mut cloned_turn = source_turn.clone();
            cloned_turn.id = runtime_record_id("turn");
            cloned_turn.thread_id = forked.id.clone();
            cloned_turn.prefix_checkpoint = None;
            if let Some(checkpoint) = forked.saved_session_checkpoint.as_mut()
                && checkpoint.covered_turn_id.as_deref() == Some(source_turn.id.as_str())
            {
                checkpoint.covered_turn_id = Some(cloned_turn.id.clone());
            }
            cloned_turn.item_ids.clear();

            let items = items_by_turn.remove(&source_turn.id).unwrap_or_default();
            let mut cloned_items = Vec::with_capacity(items.len());
            for item in items {
                let mut cloned_item = item.clone();
                cloned_item.id = runtime_record_id("item");
                cloned_item.turn_id = cloned_turn.id.clone();
                cloned_turn.item_ids.push(cloned_item.id.clone());
                cloned_items.push(cloned_item);
            }
            if !cloned_turn.routing_settlement {
                forked.latest_turn_id = Some(cloned_turn.id.clone());
            }
            forked.updated_at = now;
            cloned_records.push((cloned_turn, cloned_items));
        }
        if fork_prefix.is_none() {
            fork_prefix = Some((
                self.store.history_from_records(
                    &source_turns[..cutoff_turn_idx],
                    &self.prepared_items_for_turns(&source_turns[..cutoff_turn_idx])?,
                )?,
                cutoff_turn_idx,
            ));
        }
        // Bound at publish time — see `PreparedThreadFork::own_session`.
        let (prefix, kept_turns) = fork_prefix.ok_or_else(|| anyhow!("fork prefix missing"))?;
        let covered_turn_id = kept_turns
            .checked_sub(1)
            .map(|index| cloned_records[index].0.id.clone());
        let own_session = (captured_source, prefix, covered_turn_id);
        Ok(PreparedThreadFork {
            source_id: source.id,
            dropped_turn_id,
            depth_from_tail,
            thread: forked,
            records: cloned_records,
            original_user_text,
            original_images,
            max_output_tokens,
            own_session,
            dropped_turns,
        })
    }

    pub async fn publish_prepared_fork(
        &self,
        mut prepared: PreparedThreadFork,
    ) -> Result<(
        ThreadRecord,
        Option<String>,
        Vec<codewhale_protocol::runtime::RuntimeImageInput>,
        Option<std::num::NonZeroU32>,
    )> {
        let (captured_source, prefix, covered_turn_id) = prepared.own_session;
        let (thread, _source_lease, _target_lease) = self
            .bind_fork_to_own_session(prepared.thread, captured_source, prefix, covered_turn_id)
            .await?;
        prepared.thread = thread;
        self.publish_fork(&prepared.thread, &prepared.records)?;
        // The fork is durable once publish_fork returns. A failed event
        // append must not report the fork as unsaved: the caller already
        // holds the forked record and clients can reload it.
        if let Err(error) = self
            .emit_event(
                &prepared.thread.id,
                None,
                None,
                "thread.forked",
                json!({
                    "thread": prepared.thread,
                    "source_thread_id": prepared.source_id,
                    "backtrack_depth_from_tail": prepared.depth_from_tail,
                    "dropped_turn_id": prepared.dropped_turn_id,
                }),
            )
            .await
        {
            tracing::warn!(
                thread_id = %prepared.thread.id,
                "fork {} was saved but its thread.forked event failed: {error:#}",
                prepared.thread.id
            );
        }
        Ok((
            prepared.thread,
            prepared.original_user_text,
            prepared.original_images,
            prepared.max_output_tokens,
        ))
    }

    /// Persist cloned records before publishing their thread. Until the final
    /// atomic thread write succeeds, list/get/start callers cannot observe a
    /// partial fork. Any failed write removes all unpublished clone artifacts.
    pub(super) fn publish_fork(
        &self,
        thread: &ThreadRecord,
        records: &[(TurnRecord, Vec<TurnItemRecord>)],
    ) -> Result<()> {
        let mut saved_turn_ids = Vec::new();
        let mut saved_item_ids = Vec::new();
        let persistence = (|| -> Result<()> {
            // Every cloned item in one batch. Each `save_item` fsyncs the items
            // directory, which is right for a single record and ruinous for
            // the hundreds a fork clones. Items first, turns next, the
            // thread record that makes them reachable last: the commit point is
            // unchanged, and the ids are recorded before the batch so a partial
            // write is still cleaned up.
            let batch: Vec<&TurnItemRecord> =
                records.iter().flat_map(|(_, items)| items.iter()).collect();
            saved_item_ids.extend(batch.iter().map(|item| item.id.clone()));
            self.store.save_items_batch(&batch)?;
            for (turn, _) in records {
                self.store.save_turn(turn)?;
                saved_turn_ids.push(turn.id.clone());
            }
            self.store.save_thread(thread)
        })();

        if let Err(persistence_error) = persistence {
            let mut cleanup_errors = Vec::new();
            if let Err(error) = self.store.remove_thread(&thread.id) {
                cleanup_errors.push(format!("remove thread: {error}"));
            }
            for turn_id in saved_turn_ids.iter().rev() {
                if let Err(error) = self.store.remove_turn(turn_id) {
                    cleanup_errors.push(format!("remove turn {turn_id}: {error}"));
                }
            }
            for item_id in saved_item_ids.iter().rev() {
                if let Err(error) = self.store.remove_item(item_id) {
                    cleanup_errors.push(format!("remove item {item_id}: {error}"));
                }
            }
            if cleanup_errors.is_empty() {
                return Err(persistence_error);
            }
            bail!(
                "Failed to persist fork: {persistence_error}; cleanup also failed: {}",
                cleanup_errors.join("; ")
            );
        }
        Ok(())
    }

    /// Seed a thread with messages from a saved session so subsequent turns
    /// continue with the prior conversation context.
    ///
    /// Unlike the old text-only implementation, this preserves all content
    /// block types (thinking, tool_use, tool_result, etc.) as separate turn
    /// items so that `loadHistory` in the GUI can reconstruct the full
    /// conversation including process information.
    pub async fn seed_thread_from_messages(
        &self,
        thread_id: &str,
        messages: &[Message],
    ) -> Result<()> {
        self.seed_thread_from_messages_with_history_operation(thread_id, messages, None, messages)
            .await
    }

    pub async fn seed_thread_from_messages_with_history_operation(
        &self,
        thread_id: &str,
        messages: &[Message],
        history_operation: Option<&RuntimeHistoryOperation>,
        checkpoint_messages: &[Message],
    ) -> Result<()> {
        let config = self.read_config().clone();
        let secrets = receipt_secret_values(config).await?;
        let mut tool_names = HashMap::new();
        for block in checkpoint_messages[..checkpoint_messages.len().saturating_sub(messages.len())]
            .iter()
            .flat_map(|message| &message.content)
        {
            if let ContentBlock::ToolUse { name, .. } = block {
                tool_names.insert(block.tool_call_key(), name.as_str());
            }
        }
        // Session seeding writes turns/items and then advances the existing
        // thread pointer as one synchronous record transaction.
        let thread_mutation = self.store.thread_mutation.lock();
        let mut thread = self
            .store
            .load_thread(thread_id)
            .with_context(|| format!("Thread not found: {thread_id}"))?;
        // Seeded records are historical: their real wall-clock times are gone
        // with the provider transcript. The store's only ordering keys are
        // `TurnRecord::created_at` and `TurnItemRecord::started_at`, so
        // stamping every seeded record with one `Utc::now()` made both sorts a
        // single tie and left turn/item order to `read_dir`. That order is
        // what `get_thread_detail` hands the dashboard transcript and what the
        // fork paths freeze into the cloned `item_ids`. Hand out strictly
        // increasing synthetic stamps instead so the recorded order survives
        // every scan.
        let seed_epoch = Utc::now();
        let mut seed_step: i64 = 0;
        let mut next_seed_stamp = move || {
            let stamp = seed_epoch + chrono::Duration::microseconds(seed_step);
            seed_step += 1;
            stamp
        };

        // Group messages into turns. A turn starts with a user message and
        // includes all subsequent assistant messages (which may contain
        // thinking, tool_use, tool_result blocks) until the next user message.
        let mut turns: Vec<TurnSeed> = Vec::new();
        let mut current_turn: Option<TurnSeed> = None;

        for msg in messages {
            match msg.role.as_str() {
                "user" => {
                    let mut user_text = String::new();
                    let mut tool_results = Vec::new();
                    let image_content: Vec<_> = if msg
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ImageUrl { .. }))
                    {
                        msg.content
                            .iter()
                            .filter(|block| {
                                matches!(
                                    block,
                                    ContentBlock::Text { .. } | ContentBlock::ImageUrl { .. }
                                )
                            })
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    };

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                                if !user_text.is_empty() {
                                    user_text.push('\n');
                                }
                                user_text.push_str(text);
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                execution_id,
                                content,
                                is_error,
                                content_blocks,
                            } => {
                                let name = tool_names
                                    .get(&block.tool_call_key())
                                    .copied()
                                    .unwrap_or_default();
                                tool_results.push(SeedItem::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    execution_id: execution_id.clone(),
                                    content: tool_receipt_detail(name, content, &secrets),
                                    is_error: is_error.unwrap_or(false),
                                    content_blocks: content_blocks
                                        .as_ref()
                                        .filter(|_| name != REQUEST_USER_INPUT_TOOL_NAME)
                                        .map(|blocks| {
                                            blocks
                                                .iter()
                                                .map(|value| {
                                                    crate::client::redact_json_model_bound_text(
                                                        value, &secrets,
                                                    )
                                                })
                                                .collect()
                                        }),
                                });
                            }
                            // Other block types in user messages are rare;
                            // skip them gracefully.
                            _ => {}
                        }
                    }

                    if !user_text.is_empty() || !image_content.is_empty() {
                        // A real user prompt begins a new turn. Tool results
                        // without text belong to the preceding assistant turn.
                        if let Some(t) = current_turn.take() {
                            turns.push(t);
                        }
                        current_turn = Some(TurnSeed {
                            user_text,
                            image_content,
                            items: tool_results,
                        });
                    } else if !tool_results.is_empty() {
                        let turn = current_turn.get_or_insert_with(|| TurnSeed {
                            user_text: String::new(),
                            image_content: Vec::new(),
                            items: Vec::new(),
                        });
                        turn.items.extend(tool_results);
                    } else {
                        if let Some(t) = current_turn.take() {
                            turns.push(t);
                        }
                        current_turn = Some(TurnSeed {
                            user_text: String::new(),
                            image_content: Vec::new(),
                            items: Vec::new(),
                        });
                    }
                }
                "assistant" => {
                    // If no current turn exists (e.g. session starts with
                    // an assistant message), create a placeholder turn.
                    let turn = current_turn.get_or_insert_with(|| TurnSeed {
                        user_text: String::new(),
                        image_content: Vec::new(),
                        items: Vec::new(),
                    });
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                                turn.items.push(SeedItem::Text(text.clone()));
                            }
                            ContentBlock::Thinking { thinking, .. }
                                if !thinking.trim().is_empty() =>
                            {
                                turn.items.push(SeedItem::Thinking(thinking.clone()));
                            }
                            ContentBlock::ToolUse {
                                id,
                                execution_id,
                                name,
                                input,
                                caller,
                                thought_signature,
                            } => {
                                tool_names.insert(block.tool_call_key(), name.as_str());
                                turn.items.push(SeedItem::ToolUse {
                                    id: id.clone(),
                                    execution_id: execution_id.clone(),
                                    name: name.clone(),
                                    input: input.clone(),
                                    caller: caller.clone(),
                                    thought_signature: thought_signature.clone(),
                                });
                            }
                            ContentBlock::ServerToolUse {
                                id, name, input, ..
                            } => {
                                turn.items.push(SeedItem::ToolUse {
                                    id: id.clone(),
                                    execution_id: None,
                                    name: name.clone(),
                                    input: input.clone(),
                                    caller: None,
                                    thought_signature: None,
                                });
                            }
                            // Skip other block types (image_url, etc.)
                            _ => {}
                        }
                    }
                }
                // System messages and other roles are ignored for turn seeding.
                _ => {}
            }
        }
        // Flush the last turn.
        if let Some(t) = current_turn.take() {
            turns.push(t);
        }

        // Validate the entire import before the first durable write. Saved local
        // images keep their 5 MiB ceiling; no path or remote URL is dereferenced.
        let mut execution_providers = HashMap::new();
        for turn_seed in &turns {
            if !turn_seed.image_content.is_empty() {
                crate::image_attach::validate_stored_image_content(&turn_seed.image_content)?;
            }
            for item in &turn_seed.items {
                if let SeedItem::ToolUse {
                    execution_id,
                    id: provider,
                    ..
                }
                | SeedItem::ToolResult {
                    execution_id,
                    tool_use_id: provider,
                    ..
                } = item
                {
                    check_execution_provider(
                        &mut execution_providers,
                        execution_id.as_deref(),
                        provider,
                    )?;
                }
            }
        }

        let previous_latest_turn_id = thread.latest_turn_id.clone();
        let mut seed_items: Vec<TurnItemRecord> = Vec::new();
        let mut seed_turns: Vec<TurnRecord> = Vec::new();
        for turn_seed in turns {
            let turn_at = next_seed_stamp();
            let turn_id = runtime_record_id("turn");
            let summary = codewhale_util::utils::truncate_with_ellipsis(
                &turn_seed.user_text,
                SUMMARY_LIMIT,
                "...",
            );
            let mut item_ids = Vec::new();

            // Save user message item.
            if !turn_seed.user_text.is_empty() || !turn_seed.image_content.is_empty() {
                let item_id = runtime_record_id("item");
                let item_at = next_seed_stamp();
                let mut item = TurnItemRecord {
                    schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                    id: item_id.clone(),
                    turn_id: turn_id.clone(),
                    kind: TurnItemKind::UserMessage,
                    status: TurnItemLifecycleStatus::Completed,
                    summary: summary.clone(),
                    detail: Some(turn_seed.user_text.clone()),
                    metadata: None,
                    artifact_refs: Vec::new(),
                    artifacts: Vec::new(),
                    started_at: Some(item_at),
                    ended_at: Some(item_at),
                };
                if !turn_seed.image_content.is_empty() {
                    item.set_image_content(turn_seed.image_content.clone());
                    thread.schema_version = IMAGE_RUNTIME_SCHEMA_VERSION;
                }
                seed_items.push(item);
                item_ids.push(item_id);
            }

            // Save assistant content items in order.
            for seed_item in &turn_seed.items {
                let item_id = runtime_record_id("item");
                let item_at = next_seed_stamp();
                match seed_item {
                    SeedItem::Text(text) => {
                        let asst_summary = if text.len() > SUMMARY_LIMIT {
                            codewhale_util::utils::truncate_with_ellipsis(
                                text,
                                SUMMARY_LIMIT,
                                "...",
                            )
                        } else {
                            text.clone()
                        };
                        seed_items.push(TurnItemRecord {
                            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                            id: item_id.clone(),
                            turn_id: turn_id.clone(),
                            kind: TurnItemKind::AgentMessage,
                            status: TurnItemLifecycleStatus::Completed,
                            summary: asst_summary,
                            detail: Some(text.clone()),
                            metadata: None,
                            artifact_refs: Vec::new(),
                            artifacts: Vec::new(),
                            started_at: Some(item_at),
                            ended_at: Some(item_at),
                        });
                    }
                    SeedItem::Thinking(thinking) => {
                        let thinking_summary = if thinking.len() > SUMMARY_LIMIT {
                            codewhale_util::utils::truncate_with_ellipsis(
                                thinking,
                                SUMMARY_LIMIT,
                                "...",
                            )
                        } else {
                            thinking.clone()
                        };
                        seed_items.push(TurnItemRecord {
                            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                            id: item_id.clone(),
                            turn_id: turn_id.clone(),
                            kind: TurnItemKind::AgentReasoning,
                            status: TurnItemLifecycleStatus::Completed,
                            summary: thinking_summary,
                            detail: Some(thinking.clone()),
                            metadata: None,
                            artifact_refs: Vec::new(),
                            artifacts: Vec::new(),
                            started_at: Some(item_at),
                            ended_at: Some(item_at),
                        });
                    }
                    SeedItem::ToolUse {
                        id: tool_id,
                        execution_id,
                        name,
                        input,
                        caller,
                        thought_signature,
                    } => {
                        let input_str =
                            serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
                        let tool_summary = format!("{name}({})", {
                            let s = &input_str;
                            if s.len() > 80 {
                                codewhale_util::utils::truncate_with_ellipsis(s, 80, "...")
                            } else {
                                s.clone()
                            }
                        });
                        seed_items.push(TurnItemRecord {
                            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                            id: item_id.clone(),
                            turn_id: turn_id.clone(),
                            kind: TurnItemKind::ToolCall,
                            status: TurnItemLifecycleStatus::Completed,
                            summary: tool_summary,
                            detail: Some(input_str),
                            metadata: Some({
                                let mut meta = json!({
                                    "tool_use_id": execution_id.as_ref().unwrap_or(tool_id),
                                    "tool_name": name,
                                });
                                if let Some(id) = execution_id {
                                    meta["execution_id"] = json!(id);
                                    meta["provider_tool_use_id"] = json!(tool_id);
                                }
                                if let Some(caller) = caller {
                                    meta["tool_caller"] = json!(caller);
                                }
                                if let Some(signature) = thought_signature {
                                    meta["tool_thought_signature"] = json!(signature);
                                }
                                meta
                            }),
                            artifact_refs: Vec::new(),
                            artifacts: Vec::new(),
                            started_at: Some(item_at),
                            ended_at: Some(item_at),
                        });
                    }
                    SeedItem::ToolResult {
                        tool_use_id,
                        execution_id,
                        content,
                        is_error,
                        content_blocks,
                    } => {
                        let result_summary = if content.len() > SUMMARY_LIMIT {
                            codewhale_util::utils::truncate_with_ellipsis(
                                content,
                                SUMMARY_LIMIT,
                                "...",
                            )
                        } else {
                            content.clone()
                        };
                        let mut metadata = serde_json::Map::new();
                        metadata.insert(
                            "tool_result_for".to_string(),
                            json!(execution_id.as_ref().unwrap_or(tool_use_id)),
                        );
                        if let Some(id) = execution_id {
                            metadata.insert("execution_id".to_string(), json!(id));
                            metadata.insert("provider_tool_use_id".to_string(), json!(tool_use_id));
                        }
                        metadata.insert("is_error".to_string(), json!(is_error));
                        if let Some(blocks) = content_blocks {
                            metadata
                                .insert("content_blocks".to_string(), Value::Array(blocks.clone()));
                        }
                        seed_items.push(TurnItemRecord {
                            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                            id: item_id.clone(),
                            turn_id: turn_id.clone(),
                            kind: TurnItemKind::ToolCall,
                            status: if *is_error {
                                TurnItemLifecycleStatus::Failed
                            } else {
                                TurnItemLifecycleStatus::Completed
                            },
                            summary: result_summary,
                            detail: Some(content.clone()),
                            metadata: Some(Value::Object(metadata)),
                            artifact_refs: Vec::new(),
                            artifacts: Vec::new(),
                            started_at: Some(item_at),
                            ended_at: Some(item_at),
                        });
                    }
                }
                item_ids.push(item_id);
            }

            // Only create a turn if there's content.
            if !item_ids.is_empty() {
                seed_turns.push(TurnRecord {
                    prefix_checkpoint: None,
                    decision_receipts: Vec::new(),
                    max_output_tokens: None,
                    schema_version: if turn_seed.image_content.is_empty() {
                        CURRENT_RUNTIME_SCHEMA_VERSION
                    } else {
                        IMAGE_RUNTIME_SCHEMA_VERSION
                    },
                    id: turn_id.clone(),
                    thread_id: thread_id.to_string(),
                    status: RuntimeTurnStatus::Completed,
                    input_summary: summary,
                    created_at: turn_at,
                    started_at: Some(turn_at),
                    ended_at: Some(turn_at),
                    duration_ms: Some(0),
                    usage: None,
                    routing_settlement: false,
                    effective_route_usage: None,
                    permission_posture: None,
                    // An imported conversation: this runtime never selected a
                    // mode for it, and inventing one would be a guess.
                    mode: None,
                    effective_provider: None,
                    effective_provider_id: None,
                    effective_openrouter_vendor: None,
                    effective_billing_surface: None,
                    effective_endpoint_fingerprint: None,
                    effective_provider_live_pricing: None,
                    effective_billing_mode: None,
                    effective_dispatched_at: None,
                    effective_model: None,
                    routed_usage: Vec::new(),
                    routed_usage_drop_records: Vec::new(),
                    routed_usage_source_ids: Vec::new(),
                    routed_usage_dropped_records: 0,
                    model_request_diagnostics: None,
                    operation_activity: None,
                    error: None,
                    item_ids,
                    steer_count: 0,
                    agent_mail_message_id: None,
                    artifacts: Vec::new(),
                    workspace: None,
                    workspace_snapshots: Vec::new(),
                });

                thread.latest_turn_id = Some(turn_id);
                thread.updated_at = turn_at;
            }
        }

        if let Some(operation) = history_operation {
            let reserved = self
                .store
                .load_history_operation(&operation.key_fingerprint)?
                .ok_or_else(|| anyhow!("history seed reservation is absent"))?;
            anyhow::ensure!(
                reserved.receipt == operation.receipt
                    && operation.receipt.runtime_thread_id == thread_id,
                "history seed does not match its captured reservation"
            );
            thread.session_id = Some(operation.receipt.session_id.clone());
            thread.saved_session_checkpoint = Some(SavedSessionCheckpoint {
                covered_turn_id: thread.latest_turn_id.clone(),
                messages_sha256: session_messages_sha256(checkpoint_messages)?,
                messages_len: Some(checkpoint_messages.len()),
                retained_messages: None,
            });
        }

        // One journaled transaction (#6555): the journal names every record
        // first, then items, their turns, and last the thread record — the
        // commit record that advances `latest_turn_id`. A crash in between
        // leaves a journal whose pointer never moved, and startup recovery
        // discards those records instead of restoring a partial history.
        let journal = SeedJournal {
            thread_id: thread_id.to_string(),
            previous_latest_turn_id,
            turn_ids: seed_turns.iter().map(|turn| turn.id.clone()).collect(),
            item_ids: seed_items.iter().map(|item| item.id.clone()).collect(),
        };
        self.store.save_seed_journal(&journal)?;
        let published = (|| -> Result<()> {
            self.store
                .save_items_batch(&seed_items.iter().collect::<Vec<_>>())?;
            for turn in &seed_turns {
                self.store.save_turn(turn)?;
            }
            self.store.save_thread(&thread)
        })();
        if let Err(error) = published {
            // Roll back now; a rollback that fails leaves the journal, and
            // startup finishes it.
            if let Err(cleanup) = self.store.discard_seed(&journal) {
                tracing::warn!(
                    target: "runtime",
                    thread_id,
                    "partial seed left for startup recovery: {cleanup:#}"
                );
            }
            return Err(error);
        }
        // Committed. A journal that outlives this line is settled as committed.
        if let Err(error) = self.store.remove_seed_journal(thread_id) {
            tracing::warn!(target: "runtime", thread_id, "seed journal not removed: {error:#}");
        }
        drop(thread_mutation);
        self.emit_event(
            thread_id,
            None,
            None,
            "thread.updated",
            json!({ "thread": thread, "reason": "session_resume" }),
        )
        .await?;
        Ok(())
    }

    pub(super) fn prepare_runtime_turn_operation(
        &self,
        thread_id: &str,
        operation_key: Option<&str>,
        request_fingerprint: String,
        requested_turn_id: Option<&str>,
    ) -> Result<Option<PreparedRuntimeTurnOperation>> {
        let Some(operation_key) = operation_key else {
            return Ok(None);
        };
        let operation_key_fingerprint =
            runtime_turn_operation_key_fingerprint(&self.store.owner_id, thread_id, operation_key)?;
        let requested_turn_id = requested_turn_id
            .map(|turn_id| validated_record_id(turn_id, "requested turn id").map(str::to_string))
            .transpose()?;
        let turn_id = match requested_turn_id.as_deref() {
            Some(turn_id) => turn_id.to_string(),
            None => runtime_record_id("turn"),
        };
        Ok(Some(PreparedRuntimeTurnOperation {
            binding: RuntimeTurnOperationBinding {
                schema_version: TURN_OPERATION_BINDING_SCHEMA_VERSION,
                thread_id: thread_id.to_string(),
                turn_id,
                operation_key_fingerprint,
                request_fingerprint,
                created_at: Utc::now(),
            },
            requested_turn_id,
        }))
    }

    pub(super) fn replay_turn_for_operation(
        &self,
        prepared: &PreparedRuntimeTurnOperation,
    ) -> Result<Option<TurnRecord>> {
        let requested = &prepared.binding;
        let Some(persisted) = self
            .store
            .load_turn_operation_binding(&requested.operation_key_fingerprint)?
        else {
            return Ok(None);
        };
        if persisted.thread_id != requested.thread_id
            || persisted.operation_key_fingerprint != requested.operation_key_fingerprint
            || persisted.request_fingerprint != requested.request_fingerprint
            || prepared
                .requested_turn_id
                .as_deref()
                .is_some_and(|turn_id| persisted.turn_id != turn_id)
        {
            bail!("operation_key is already bound to a different turn request");
        }
        let turn_path = self.store.turn_path(&persisted.turn_id)?;
        if !turn_path.exists() {
            bail!("operation_key binding is incomplete; retry after Runtime recovery");
        }
        let turn = self.store.load_turn(&persisted.turn_id)?;
        if turn.id != persisted.turn_id || turn.thread_id != persisted.thread_id {
            bail!("operation_key binding does not match its persisted Runtime turn");
        }
        Ok(Some(turn))
    }

    /// Read the exact durable binding without replaying, recovering, loading an
    /// engine, or creating even a claim-lock file. The shared claim prevents a
    /// reader from observing admission records that can still be rolled back.
    pub fn lookup_turn_operation(
        &self,
        thread_id: &str,
        operation_key: &str,
    ) -> Result<Option<TurnRecord>, RuntimeTurnOperationLookupError> {
        use RuntimeTurnOperationLookupError::{Incomplete, InvalidRequest, Unavailable};
        validated_record_id(thread_id, "thread id").map_err(|_| InvalidRequest)?;
        let fingerprint =
            runtime_turn_operation_key_fingerprint(&self.store.owner_id, thread_id, operation_key)
                .map_err(|_| InvalidRequest)?;
        checked_existing_runtime_store_dir(&self.store.turn_operations_dir)
            .map_err(|_| Unavailable)?;
        let lock_path = self
            .store
            .turn_operation_lock_path(&fingerprint)
            .map_err(|_| Unavailable)?;
        let lock_file = match open_runtime_store_file(
            &lock_path,
            "Runtime turn operation lookup lock",
            |options| {
                options.read(true);
            },
        ) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return match self
                    .store
                    .load_turn_operation_binding(&fingerprint)
                    .map_err(|_| Unavailable)?
                {
                    None => Ok(None),
                    Some(binding)
                        if binding.thread_id != thread_id
                            || binding.operation_key_fingerprint != fingerprint =>
                    {
                        Ok(None)
                    }
                    Some(_) => Err(Incomplete),
                };
            }
            Err(_) => return Err(Unavailable),
        };
        let claim = fd_lock::RwLock::new(lock_file);
        let _guard = claim.try_read().map_err(|error| match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Incomplete,
            _ => Unavailable,
        })?;
        let Some(binding) = self
            .store
            .load_turn_operation_binding(&fingerprint)
            .map_err(|_| Unavailable)?
        else {
            return Ok(None);
        };
        if binding.thread_id != thread_id || binding.operation_key_fingerprint != fingerprint {
            return Ok(None);
        }
        let thread = match self.store.load_thread(thread_id) {
            Ok(thread) => thread,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(_) => return Err(Unavailable),
        };
        if thread.id != thread_id {
            return Ok(None);
        }
        let turn = self.store.load_turn(&binding.turn_id).map_err(|error| {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                Incomplete
            } else {
                Unavailable
            }
        })?;
        if turn.id != binding.turn_id || turn.thread_id != thread_id {
            return Ok(None);
        }
        Ok(Some(turn))
    }

    pub(super) fn lookup_extension_command_turn(
        &self,
        thread_id: &str,
        fingerprint: &str,
        extension: &Value,
    ) -> Result<Option<TurnRecord>> {
        anyhow::ensure!(extension.is_object(), "command receipt identity is absent");
        let Some(turn) =
            self.lookup_turn_operation(thread_id, &format!("extension-command-{fingerprint}"))?
        else {
            return Ok(None);
        };
        let item_id = turn
            .item_ids
            .first()
            .context("Command turn receipt has no input item")?;
        let item = self.store.load_item(item_id)?;
        anyhow::ensure!(
            item.turn_id == turn.id,
            "Command turn input belongs to another turn"
        );
        let owned = item.kind == TurnItemKind::UserMessage
            && item.metadata.as_ref().is_some_and(|metadata| {
                metadata["input_provenance"] == "extension_command"
                    && metadata["command_operation"].as_str() == Some(fingerprint)
                    && &metadata["extension"] == extension
            });
        Ok(owned.then_some(turn))
    }

    pub(super) fn cleanup_unaccepted_turn_records(
        &self,
        turn_id: &str,
        item_id: Option<&str>,
        operation_key_fingerprint: Option<&str>,
    ) -> Result<()> {
        let mut errors = Vec::new();
        if let Some(item_id) = item_id
            && let Err(err) = self.store.remove_item(item_id)
        {
            errors.push(format!("remove item: {err}"));
        }
        if let Err(err) = self.store.remove_turn(turn_id) {
            errors.push(format!("remove turn: {err}"));
        }
        if let Some(operation_key_fingerprint) = operation_key_fingerprint
            && let Err(err) = self
                .store
                .remove_turn_operation_binding(operation_key_fingerprint)
        {
            errors.push(format!("remove turn operation binding: {err}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("; "))
        }
    }

    pub(super) async fn emit_claimed_turn_started(
        &self,
        turn: &TurnRecord,
        user_item: Option<&TurnItemRecord>,
        kind: ClaimedTurnKind,
    ) {
        let start_payload = match kind {
            ClaimedTurnKind::Message { .. } => json!({ "turn": turn.clone() }),
            ClaimedTurnKind::Compaction => {
                json!({ "turn": turn.clone(), "manual_compaction": true })
            }
        };
        if let Err(err) = self
            .emit_event(
                &turn.thread_id,
                Some(&turn.id),
                None,
                "turn.started",
                start_payload,
            )
            .await
        {
            tracing::warn!(
                "Failed to persist {}.started after engine acceptance: {err}",
                kind.label()
            );
        }

        if let Some(user_item) = user_item {
            if let Err(err) = self
                .emit_event(
                    &turn.thread_id,
                    Some(&turn.id),
                    Some(&user_item.id),
                    "item.started",
                    json!({ "item": user_item.clone() }),
                )
                .await
            {
                tracing::warn!("Failed to persist item.started after engine acceptance: {err}");
            }
            if let Err(err) = self
                .emit_event(
                    &turn.thread_id,
                    Some(&turn.id),
                    Some(&user_item.id),
                    "item.completed",
                    json!({ "item": user_item.clone() }),
                )
                .await
            {
                tracing::warn!("Failed to persist item.completed after engine acceptance: {err}");
            }
        }
    }

    pub(super) async fn settle_claimed_turn_failure(
        &self,
        thread_id: &str,
        turn_id: &str,
        reason: &str,
    ) {
        // Block steer attempts while terminal receipts are being settled; the
        // active claim remains present so a replacement turn cannot start.
        {
            let mut active = self.active.lock().await;
            if let Some(turn) = active
                .engines
                .get_mut(thread_id)
                .and_then(|state| state.active_turn.as_mut())
                && turn.turn_id == turn_id
            {
                turn.interrupt_requested = true;
            }
        }
        let now = Utc::now();
        crate::cost_status::finish_runtime_usage_owner(turn_id);
        let background_usage = crate::cost_status::take_runtime_usage(turn_id);
        let mut terminal_items = Vec::new();
        match self.store.list_items_for_turn(turn_id) {
            Ok(items) => {
                for mut item in items {
                    if matches!(
                        item.status,
                        TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
                    ) {
                        item.status = TurnItemLifecycleStatus::Failed;
                        item.ended_at = Some(now);
                        match self.store.save_item(&item) {
                            Ok(()) => terminal_items.push(item),
                            Err(err) => {
                                self.report_store_failure(
                                    thread_id,
                                    Some(turn_id),
                                    &format!(
                                        "Failed to terminalize item {} after monitor failure: {err}",
                                        item.id
                                    ),
                                    &err,
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            Err(err) => {
                self.report_store_failure(
                    thread_id,
                    Some(turn_id),
                    &format!(
                        "Failed to list turn items after monitor failure for {turn_id}: {err}"
                    ),
                    &err,
                    false,
                )
                .await;
            }
        }
        let (terminal_turn, load_failure) = {
            let _turn_mutation = self.store.turn_mutation.lock();
            match self.store.load_turn(turn_id) {
                Ok(mut turn) => {
                    for receipt in &background_usage.decisions {
                        append_decision_receipt(&mut turn, receipt);
                    }
                    for record in background_usage.records.iter().cloned() {
                        append_routed_usage_record(&mut turn, &record.source_id, record.usage);
                    }
                    for record in background_usage.drop_records.iter().cloned() {
                        append_routed_usage_drop_record(&mut turn, record);
                    }
                    let background_residual = background_usage.dropped_records.saturating_sub(
                        u64::try_from(background_usage.drop_records.len()).unwrap_or(u64::MAX),
                    );
                    turn.routed_usage_dropped_records = turn
                        .routed_usage_dropped_records
                        .saturating_add(background_residual);
                    if turn.status == RuntimeTurnStatus::InProgress {
                        turn.status = RuntimeTurnStatus::Failed;
                        if let Some(activity) = &mut turn.operation_activity {
                            activity.active.clear();
                        }
                        turn.ended_at = Some(now);
                        turn.duration_ms = turn.started_at.map(|start| duration_ms(start, now));
                        turn.error = Some(reason.to_string());
                    }
                    if turn.workspace.is_none() {
                        // The monitor died before the engine reported a
                        // snapshot pair; keep what the tool receipts say.
                        self.set_turn_artifacts(
                            &mut turn,
                            None,
                            TurnWorkspaceArtifacts::unavailable(TurnWorkspaceReason::NotCaptured),
                        );
                    }
                    (
                        matches!(
                            turn.status,
                            RuntimeTurnStatus::Completed
                                | RuntimeTurnStatus::Failed
                                | RuntimeTurnStatus::Interrupted
                                | RuntimeTurnStatus::Canceled
                        )
                        .then_some(turn),
                        None,
                    )
                }
                Err(err) => (None, Some(err)),
            }
        };
        if let Some(err) = load_failure {
            // Without its record the turn can never be terminalized: name
            // the file now and tell anyone waiting on `turn.completed` to
            // stop waiting (#5931).
            self.report_store_failure(
                thread_id,
                Some(turn_id),
                &format!("Failed to load turn after monitor failure: {err}"),
                &err,
                true,
            )
            .await;
        }

        for item in terminal_items {
            if let Err(err) = self
                .emit_event(
                    thread_id,
                    Some(turn_id),
                    Some(&item.id),
                    "item.failed",
                    json!({ "item": item, "error": reason }),
                )
                .await
            {
                tracing::error!("Failed to emit terminal item failure: {err}");
            }
        }

        // A failed turn can no longer answer an outstanding prompt. Mirror the
        // happy terminal path's receipt-before-removal ordering.
        let engine_for_cancel = {
            let active = self.active.lock().await;
            active
                .engines
                .get(thread_id)
                .map(|state| state.engine.clone())
        };
        let user_inputs_settled = if let Err(err) = self
            .settle_user_inputs_for_terminal_turn(thread_id, turn_id, engine_for_cancel)
            .await
        {
            tracing::error!("Failed to emit user-input cancellation after monitor failure: {err}");
            false
        } else {
            true
        };

        let dynamic_tools_settled = if let Err(err) = self
            .settle_dynamic_tools_for_terminal_turn(thread_id, turn_id)
            .await
        {
            tracing::error!(
                "Failed to emit dynamic-tool cancellation after monitor failure: {err}"
            );
            false
        } else {
            true
        };

        // A terminal record is the externally visible lifecycle boundary.
        // Keep snapshots outside that boundary until its terminal receipt and
        // active-claim cleanup are also ordered. The dedupe scan may yield to
        // a blocking worker while this projection guard remains held.
        let projection_lock = self.projection_lock(thread_id);
        let _projection = projection_lock.lock().await;
        let mut persist_failure = None;
        let terminal_turn = terminal_turn.and_then(|turn| {
            let _turn_mutation = self.store.turn_mutation.lock();
            match self.store.save_turn(&turn) {
                Ok(()) => Some(turn),
                Err(err) => {
                    persist_failure = Some(err);
                    None
                }
            }
        });
        if let Some(err) = persist_failure {
            // The projection guard is held and the record guard is not; the
            // publish takes `event_emit` after the projection lock, which is
            // the documented order. No terminal receipt follows (#5931).
            self.report_store_failure(
                thread_id,
                Some(turn_id),
                &format!("Failed to persist terminal monitor failure: {err}"),
                &err,
                true,
            )
            .await;
        }
        if let Some(turn) = terminal_turn.as_ref() {
            if user_inputs_settled && dynamic_tools_settled {
                if let Err(err) = self.emit_turn_completed_if_missing(turn, false).await {
                    tracing::error!("Failed to emit terminal monitor failure: {err}");
                    self.queue_recovery_receipt(RecoveredTurnReceipt {
                        turn: turn.clone(),
                        unresolved_dynamic_tools: Vec::new(),
                    });
                }
            } else {
                self.queue_recovery_receipt(RecoveredTurnReceipt {
                    turn: turn.clone(),
                    unresolved_dynamic_tools: Vec::new(),
                });
            }
        }

        // Keep the failed claim in place until its terminal receipts are
        // ordered. Then poison and evict this engine so the next turn gets a
        // distinct event receiver and cannot consume stale terminal events.
        let evicted_engine = {
            let mut active = self.active.lock().await;
            let owns_failed_turn = active
                .engines
                .get(thread_id)
                .and_then(|state| state.active_turn.as_ref())
                .is_some_and(|turn| turn.turn_id == turn_id);
            if owns_failed_turn {
                active.lru.retain(|id| id != thread_id);
                active.engines.remove(thread_id).map(|state| state.engine)
            } else {
                None
            }
        };
        if let Some(engine) = evicted_engine {
            drop(_projection);
            engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
            let _ = engine.try_send(Op::Shutdown);
        }
    }

    pub(super) async fn monitor_claimed_turn(
        &self,
        thread_id: String,
        turn_id: String,
        engine: EngineHandle,
        kind: ClaimedTurnKind,
    ) {
        if self.cancel_token.is_cancelled() {
            engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
            self.settle_claimed_turn_failure(
                &thread_id,
                &turn_id,
                "Runtime shutdown requested before turn monitoring started",
            )
            .await;
            self.event_reader_wake.notify_waiters();
            return;
        }

        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(self.monitor_turn(
            thread_id.clone(),
            turn_id.clone(),
            engine.clone(),
            match kind {
                ClaimedTurnKind::Message { narrowing, .. } => narrowing,
                ClaimedTurnKind::Compaction => crate::core::engine::TurnNarrowing::Inherit,
            },
            matches!(
                kind,
                ClaimedTurnKind::Message {
                    structured_review: true,
                    ..
                }
            ),
        ))
        .catch_unwind()
        .await;
        let failure = match result {
            Ok(Ok(())) => {
                self.event_reader_wake.notify_waiters();
                return;
            }
            Ok(Err(error)) => {
                let failure = format!("Failed to monitor {}: {error}", kind.label());
                // An unreadable item or turn under the monitor is the
                // operator's own state: name the file before settling (#5931).
                self.report_store_failure(&thread_id, Some(&turn_id), &failure, &error, false)
                    .await;
                failure
            }
            Err(payload) => {
                let failure = format!(
                    "{} monitor panicked: {}",
                    kind.label(),
                    panic_payload_message(&*payload)
                );
                tracing::error!("{failure}");
                failure
            }
        };
        engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
        self.settle_claimed_turn_failure(&thread_id, &turn_id, &failure)
            .await;
        self.event_reader_wake.notify_waiters();
    }

    pub(super) fn spawn_claimed_turn_monitor(
        &self,
        turn: TurnRecord,
        user_item: Option<TurnItemRecord>,
        engine: EngineHandle,
        kind: ClaimedTurnKind,
    ) -> oneshot::Receiver<std::result::Result<TurnRecord, String>> {
        let (acceptance_tx, acceptance_rx) = oneshot::channel();
        let manager = Arc::new(self.clone());
        let monitor = tokio::spawn(async move {
            use futures_util::FutureExt;
            let start_events = std::panic::AssertUnwindSafe(manager.emit_claimed_turn_started(
                &turn,
                user_item.as_ref(),
                kind,
            ))
            .catch_unwind()
            .await;
            if let Err(payload) = start_events {
                let failure = format!(
                    "{} start-event recording panicked after engine acceptance: {}",
                    kind.label(),
                    panic_payload_message(&*payload)
                );
                tracing::error!("{failure}");
                let _ = acceptance_tx.send(Ok(turn.clone()));
                engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
                manager
                    .settle_claimed_turn_failure(&turn.thread_id, &turn.id, &failure)
                    .await;
                manager.event_reader_wake.notify_waiters();
                return;
            }

            let _ = acceptance_tx.send(Ok(turn.clone()));
            manager
                .monitor_claimed_turn(turn.thread_id.clone(), turn.id.clone(), engine, kind)
                .await;
        });
        self.track_receipt_worker(monitor);
        acceptance_rx
    }

    /// Settle one steer against the engine's real verdict.
    ///
    /// The detached worker — not the API caller — owns `outcome_rx`, so a
    /// cancelled request or a timed-out caller can never orphan the verdict
    /// and strand the item in `Queued` (#6276). Exactly one of two
    /// settlements is written, record first and events after:
    ///
    /// - `Accepted`: the item flips to `Completed`, `steer_count` rises, and
    ///   `turn.steered` + `item.completed` are emitted — the happy path
    ///   clients already know.
    /// - `Dropped` (including a closed channel, which means the engine is
    ///   gone): the item flips to `Canceled` and `turn.steer_dropped` carries
    ///   the settled item, so a client can requeue the text it was told had
    ///   been delivered.
    pub(super) fn spawn_steer_settlement(
        &self,
        turn: TurnRecord,
        item: TurnItemRecord,
        prompt: String,
        outcome_rx: oneshot::Receiver<SteerOutcome>,
    ) -> oneshot::Receiver<(SteerOutcome, TurnRecord)> {
        let (settle_tx, settle_rx) = oneshot::channel();
        let manager = Arc::new(self.clone());
        let worker = tokio::spawn(async move {
            use futures_util::FutureExt;
            // The engine sends exactly one verdict on every exit path. A
            // closed channel means the engine itself went away without one,
            // which is a drop by any honest reading.
            let outcome = outcome_rx.await.unwrap_or(SteerOutcome::Dropped);
            let mut item = item;
            item.status = match outcome {
                SteerOutcome::Accepted => TurnItemLifecycleStatus::Completed,
                SteerOutcome::Dropped => TurnItemLifecycleStatus::Canceled,
            };
            item.ended_at = Some(Utc::now());
            let settled_turn = std::panic::AssertUnwindSafe(async {
                let persisted = {
                    let _turn_mutation = manager.store.turn_mutation.lock();
                    (|| -> Result<TurnRecord> {
                        manager.store.save_item(&item)?;
                        let mut turn = manager.store.load_turn(&item.turn_id)?;
                        if outcome == SteerOutcome::Accepted {
                            turn.steer_count = turn.steer_count.saturating_add(1);
                        }
                        manager.store.save_turn(&turn)?;
                        Ok(turn)
                    })()
                };
                let settled_turn = match persisted {
                    Ok(turn) => turn,
                    Err(err) => {
                        tracing::error!("Failed to settle steer item {}: {err}", item.id);
                        turn.clone()
                    }
                };
                match outcome {
                    SteerOutcome::Accepted => {
                        if let Err(err) = manager
                            .emit_event(
                                &turn.thread_id,
                                Some(&turn.id),
                                Some(&item.id),
                                "turn.steered",
                                json!({
                                    "thread_id": turn.thread_id.clone(),
                                    "turn_id": turn.id.clone(),
                                    "input": prompt,
                                }),
                            )
                            .await
                        {
                            tracing::warn!(
                                "Failed to persist turn.steered after engine acceptance: {err}"
                            );
                        }
                        if let Err(err) = manager
                            .emit_event(
                                &turn.thread_id,
                                Some(&turn.id),
                                Some(&item.id),
                                "item.completed",
                                json!({ "item": item }),
                            )
                            .await
                        {
                            tracing::warn!("Failed to persist steer item.completed: {err}");
                        }
                    }
                    SteerOutcome::Dropped => {
                        if let Err(err) = manager
                            .emit_event(
                                &turn.thread_id,
                                Some(&turn.id),
                                Some(&item.id),
                                "turn.steer_dropped",
                                json!({
                                    "thread_id": turn.thread_id.clone(),
                                    "turn_id": turn.id.clone(),
                                    "input": prompt,
                                    "reason": STEER_DROPPED_REASON,
                                    "item": item,
                                }),
                            )
                            .await
                        {
                            tracing::warn!("Failed to persist turn.steer_dropped: {err}");
                        }
                    }
                }
                settled_turn
            })
            .catch_unwind()
            .await;
            let settled_turn = match settled_turn {
                Ok(turn) => turn,
                Err(payload) => {
                    tracing::error!(
                        "Steer settlement task panicked: {}",
                        panic_payload_message(&*payload)
                    );
                    turn
                }
            };
            let _ = settle_tx.send((outcome, settled_turn));
        });
        self.track_receipt_worker(worker);
        settle_rx
    }

    pub async fn start_review_turn(
        &self,
        thread_id: &str,
        req: StartReviewRequest,
    ) -> Result<(TurnRecord, bool)> {
        self.start_review_turn_inner(thread_id, req, None).await
    }

    pub async fn start_review_turn_with_reserved_id(
        &self,
        thread_id: &str,
        req: StartReviewRequest,
        reserved_turn_id: &str,
    ) -> Result<TurnRecord> {
        validated_record_id(reserved_turn_id, "reserved turn id")?;
        let (turn, _) = self
            .start_review_turn_inner(thread_id, req, Some(reserved_turn_id))
            .await?;
        anyhow::ensure!(
            turn.id == reserved_turn_id,
            "reserved Runtime review id does not match the operation binding"
        );
        Ok(turn)
    }

    pub(super) async fn start_review_turn_inner(
        &self,
        thread_id: &str,
        req: StartReviewRequest,
        reserved_turn_id: Option<&str>,
    ) -> Result<(TurnRecord, bool)> {
        crate::core::engine::review::validate(&req.review)?;
        anyhow::ensure!(
            (256..=8192).contains(&req.max_output_tokens.get()),
            "review maxOutputTokens must be 256..8192"
        );
        anyhow::ensure!(
            !req.model.is_empty()
                && req.model.trim() == req.model
                && !req.model.eq_ignore_ascii_case("auto"),
            "review requires an exact model"
        );
        let prompt = crate::core::engine::review::user_message(&req.review);
        let summary = format!(
            "Review {}#{} at {}",
            req.review.repository, req.review.pr_number, req.review.head_sha
        );
        let request = StartTurnRequest {
            account_model_owner: req.account_model_owner,
            profile_constitution: None,
            expected_workspace: None,
            max_output_tokens: Some(req.max_output_tokens),
            prompt,
            images: Vec::new(),
            operation_key: Some(req.operation_key),
            input_summary: Some(summary),
            model: Some(req.model),
            reasoning_effort: req.reasoning_effort,
            allowed_tools: Some(Vec::new()),
            mode: Some("act".to_string()),
            permission_posture: Some("ask".to_string()),
            allow_shell: Some(false),
            trust_mode: Some(false),
            auto_approve: Some(false),
            dynamic_tools: Vec::new(),
            environment_id: None,
            model_provider: req.model_provider,
            model_provider_id: req.model_provider_id,
        };
        self.start_turn_with_source(
            thread_id,
            request,
            RuntimeTurnInputSource::Review(req.review),
            reserved_turn_id,
            false,
        )
        .await
    }

    pub async fn extension_commands(
        &self,
        thread_id: &str,
    ) -> Result<Vec<crate::extension_host::command::ExtensionCommandEntry>> {
        let _admission = self.config_admission.read().await;
        let thread = self.get_thread(thread_id).await?;
        if !codewhale_plugins::activation::extension_host_policy_enabled() {
            return Ok(Vec::new());
        }
        self.ensure_engine_loaded(&thread).await?;
        crate::extension_host::manager()
            .reconcile()
            .await
            .map_err(anyhow::Error::msg)?;
        let Some(plugins) =
            crate::extension_host::caller_view(&thread.workspace, Some(thread_id), None)
        else {
            return Ok(Vec::new());
        };
        Ok(crate::extension_host::live_commands_for_plugins(&plugins))
    }

    pub async fn run_extension_command(
        &self,
        thread_id: &str,
        request: RuntimeCommandRequest,
    ) -> Result<RuntimeCommandResult> {
        let manager = self.clone();
        let thread_id = thread_id.to_string();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            use futures_util::FutureExt;
            let result = std::panic::AssertUnwindSafe(
                manager.run_extension_command_inner(&thread_id, request),
            )
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(anyhow!(
                    "Command lifecycle stopped; use the receipt lookup before retrying"
                ))
            });
            let _ = sender.send(result);
        });
        self.track_receipt_worker(worker);
        receiver.await.context("command lifecycle worker stopped")?
    }

    pub async fn extension_command_status(
        &self,
        thread_id: &str,
        operation_key: &str,
    ) -> Result<Option<Value>> {
        self.get_thread(thread_id).await?;
        let fingerprint =
            runtime_turn_operation_key_fingerprint(&self.store.owner_id, thread_id, operation_key)?;
        let emit_order = self.event_emit.lock().await;
        let manager = self.clone();
        let lookup_thread = thread_id.to_string();
        let (started, finished, cancelled, recovered) = tokio::task::spawn_blocking(move || {
            manager.store.with_turn_operation_claim(Some(&fingerprint), || {
                let (started, mut finished, cancelled) = manager.store.extension_command_events(&lookup_thread, &fingerprint)?;
                let live = manager.pending_commands.lock().contains_key(&(lookup_thread.clone(), fingerprint.clone()));
                let recovered = if finished.is_none() && !live && let Some(started) = &started {
                    let extension = started.get("extension").cloned().context("command receipt identity is absent")?;
                    let turn = manager.lookup_extension_command_turn(&lookup_thread, &fingerprint, &extension)?;
                    let result = RuntimeCommandResult {
                        operation_key: String::new(),
                        name: started.get("name").and_then(Value::as_str).context("command receipt name is absent")?.to_string(),
                        origin: extension.get("origin").and_then(Value::as_str).context("command receipt origin is absent")?.to_string(),
                        status: "failed".to_string(),
                        text: if turn.is_some() {
                            "Command execution ended without a recorded result. Its effects are uncertain; its admitted turn was recovered. The command callback was not rerun."
                        } else {
                            "Command execution ended without a recorded result. Its effects are uncertain. The command callback was not rerun."
                        }.to_string(),
                        extension,
                        thread: turn.as_ref().map(|_| manager.store.load_thread(&lookup_thread)).transpose()?,
                        turn,
                        idempotent_replay: false,
                    };
                    let record = manager.store.append_event_transaction(
                        lookup_thread.clone(), result.turn.as_ref().map(|turn| turn.id.clone()), None,
                        "extension_command_completed".to_string(), json!({"operation_fingerprint": fingerprint, "result": result}),
                        EVENT_TRANSACTION_LOCK_TIMEOUT,
                    )?;
                    finished = Some(record.payload.clone());
                    Some(record)
                } else { None };
                Ok::<_, anyhow::Error>((started, finished, cancelled, recovered))
            })
        })
        .await
        .context("command receipt lookup failed")??;
        if let Some(event) = recovered {
            let _ = self.event_tx.send(event);
        }
        drop(emit_order);
        if let Some(finished) = finished {
            let mut result: RuntimeCommandResult =
                serde_json::from_value(finished["result"].clone())
                    .context("command receipt is invalid")?;
            result.operation_key = operation_key.to_string();
            result.idempotent_replay = true;
            return Ok(Some(serde_json::to_value(result)?));
        }
        if started.is_some() {
            return Ok(Some(
                json!({ "status": "pending", "operation_key": operation_key }),
            ));
        }
        Ok(cancelled.then(|| json!({ "status": "cancelled", "operation_key": operation_key,
            "name": "", "origin": "", "text": "Command cancelled before admission", "extension": null,
            "idempotent_replay": true })))
    }

    pub async fn cancel_extension_command(
        &self,
        thread_id: &str,
        operation_key: &str,
    ) -> Result<bool> {
        self.get_thread(thread_id).await?;
        let fingerprint =
            runtime_turn_operation_key_fingerprint(&self.store.owner_id, thread_id, operation_key)?;
        let _emit_order = self.event_emit.lock().await;
        if let Some(cancel) = self
            .pending_commands
            .lock()
            .get(&(thread_id.to_string(), fingerprint.clone()))
        {
            cancel.cancel();
        }
        let store = self.store.clone();
        let cancel_thread = thread_id.to_string();
        let cancel_fingerprint = fingerprint.clone();
        let (event, extension) = tokio::task::spawn_blocking(move || {
            store.with_turn_operation_claim(Some(&cancel_fingerprint), || {
                let (started, finished, cancelled) =
                    store.extension_command_events(&cancel_thread, &cancel_fingerprint)?;
                let extension = started.and_then(|started| started.get("extension").cloned());
                if finished.is_some() || cancelled {
                    return Ok((None, extension));
                }
                store
                    .append_event_transaction(
                        cancel_thread.clone(),
                        None,
                        None,
                        "extension_command_cancelled".to_string(),
                        json!({ "operation_fingerprint": cancel_fingerprint }),
                        EVENT_TRANSACTION_LOCK_TIMEOUT,
                    )
                    .map(|event| (Some(event), extension))
            })
        })
        .await
        .context("command cancellation worker failed")??;
        if let Some(event) = event {
            let _ = self.event_tx.send(event);
        }
        drop(_emit_order);
        let _ = self
            .extension_command_status(thread_id, operation_key)
            .await?;
        let turn_id = if let Some(extension) = extension {
            let manager = self.clone();
            let lookup_thread = thread_id.to_string();
            tokio::task::spawn_blocking(move || {
                manager.lookup_extension_command_turn(&lookup_thread, &fingerprint, &extension)
            })
            .await
            .context("Command cancellation turn lookup worker failed")??
            .map(|turn| turn.id)
        } else {
            None
        };
        if let Some(turn_id) = turn_id {
            let active = self
                .active
                .lock()
                .await
                .engines
                .get(thread_id)
                .and_then(|state| state.active_turn.as_ref())
                .is_some_and(|turn| turn.turn_id == turn_id);
            if active {
                match self.interrupt_turn(thread_id, &turn_id).await {
                    Ok(_) => {}
                    Err(error)
                        if error.to_string().contains("No active turn")
                            || error.to_string().contains("is not active") => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(true)
    }

    pub(super) async fn run_extension_command_inner(
        &self,
        thread_id: &str,
        request: RuntimeCommandRequest,
    ) -> Result<RuntimeCommandResult> {
        validate_runtime_turn_operation_key(&request.operation_key)?;
        anyhow::ensure!(
            !request.name.is_empty()
                && request.name.len() <= 64
                && request.name.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || b"_-".contains(&byte)),
            "invalid command name"
        );
        anyhow::ensure!(
            request.args.len() <= crate::extension_host::command::MAX_PROMPT_BYTES,
            "command arguments exceed the byte limit"
        );
        validate_sha256_fingerprint(&request.registration_id, "command registration_id")?;
        let fingerprint = runtime_turn_operation_key_fingerprint(
            &self.store.owner_id,
            thread_id,
            &request.operation_key,
        )?;
        let request_fingerprint = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&json!({
                "name": request.name, "args": request.args, "registration_id": request.registration_id,
                "expected_workspace": request.expected_workspace,
            })),
        );
        let admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let thread = self.get_thread(thread_id).await?;
        let store = self.store.clone();
        let lookup_thread = thread_id.to_string();
        let lookup_fingerprint = fingerprint.clone();
        let (started, finished, cancelled) = tokio::task::spawn_blocking(move || {
            store.extension_command_events(&lookup_thread, &lookup_fingerprint)
        })
        .await
        .context("command receipt lookup failed")??;
        if let Some(started) = started {
            anyhow::ensure!(
                started.get("request_fingerprint").and_then(Value::as_str)
                    == Some(request_fingerprint.as_str()),
                "Command operation key already belongs to a different request"
            );
            if let Some(finished) = finished {
                let mut result: RuntimeCommandResult =
                    serde_json::from_value(finished["result"].clone())
                        .context("command receipt is invalid")?;
                result.operation_key = request.operation_key;
                result.idempotent_replay = true;
                return Ok(result);
            }
            bail!(
                "Command operation is pending or its outcome is uncertain; it will not be executed again"
            );
        }
        anyhow::ensure!(
            !cancelled,
            "Command operation was cancelled before admission"
        );
        anyhow::ensure!(
            thread.workspace == request.expected_workspace,
            "Command workspace changed; select the command again"
        );
        anyhow::ensure!(
            !thread.archived,
            "Cannot run a command in an archived thread"
        );
        let catalog = crate::command_catalog::installed()
            .ok_or_else(|| anyhow!("Command registration is stale; command catalog unavailable"))?;
        let command_name = request.name.clone();
        let command_workspace = thread.workspace.clone();
        let shadowed = tokio::task::spawn_blocking(move || {
            catalog.answers_to(&command_name)
                || catalog
                    .user_commands(&command_workspace)
                    .iter()
                    .filter(|command| !command.extension)
                    .any(|command| {
                        command.name == command_name || command.aliases.contains(&command_name)
                    })
        })
        .await
        .context("command catalog lookup failed")?;
        anyhow::ensure!(
            !shadowed,
            "Command registration is stale; another command owns this spelling"
        );
        self.ensure_engine_loaded(&thread).await?;
        crate::extension_host::manager()
            .reconcile()
            .await
            .map_err(anyhow::Error::msg)?;
        let plugins = crate::extension_host::caller_view(&thread.workspace, Some(thread_id), None)
            .ok_or_else(|| anyhow!("Command registration is stale; select the command again"))?;
        let entries = crate::extension_host::live_commands_for_plugins(&plugins);
        let entry = entries
            .into_iter()
            .find(|entry| {
                entry.registration.name == request.name
                    && entry.reference().registration_id(thread_id) == request.registration_id
            })
            .ok_or_else(|| anyhow!("Command registration is stale; select the command again"))?;
        let reference = entry.reference();
        crate::extension_host::command::validate_reference(&reference, thread_id)
            .await
            .map_err(anyhow::Error::msg)?;
        let cancel = self.cancel_token.child_token();
        let key = (thread_id.to_string(), fingerprint.clone());
        {
            let active = self.active.lock().await;
            anyhow::ensure!(
                active
                    .engines
                    .get(thread_id)
                    .is_some_and(|state| state.active_turn.is_none()),
                "Thread already has an active turn"
            );
            let mut pending = self.pending_commands.lock();
            anyhow::ensure!(
                !pending.keys().any(|(id, _)| id == thread_id),
                "Thread already has an active command"
            );
            pending.insert(key.clone(), cancel.clone());
        }
        let _pending = RuntimeCommandGuard {
            pending: Arc::clone(&self.pending_commands),
            key,
        };
        let emit_order = self.event_emit.lock().await;
        let store = self.store.clone();
        let start_thread = thread_id.to_string();
        let start_fingerprint = fingerprint.clone();
        let start_workspace = request.expected_workspace.clone();
        let started_payload = json!({ "operation_fingerprint": fingerprint, "request_fingerprint": request_fingerprint,
            "name": request.name, "args": request.args, "extension": reference.receipt() });
        let started = tokio::task::spawn_blocking(move || {
            store.with_turn_operation_claim(Some(&start_fingerprint), || {
                let (started, _, cancelled) = store.extension_command_events(&start_thread, &start_fingerprint)?;
                anyhow::ensure!(!cancelled, "Command operation was cancelled before admission");
                anyhow::ensure!(started.is_none(), "Command operation is pending or its outcome is uncertain; it will not be executed again");
                let _thread_mutation = store.thread_mutation.lock();
                let current_thread = store.load_thread(&start_thread)?;
                anyhow::ensure!(current_thread.workspace == start_workspace, "Command workspace changed; select the command again");
                anyhow::ensure!(!current_thread.archived, "Cannot run a command in an archived thread");
                store.append_event_transaction(start_thread.clone(), None, None, "extension_command_started".to_string(),
                    started_payload, EVENT_TRANSACTION_LOCK_TIMEOUT)
            })
        }).await.context("command admission worker failed").and_then(|result| result);
        match started {
            Ok(event) => {
                let _ = self.event_tx.send(event);
            }
            Err(error) => return Err(error),
        }
        drop(emit_order);
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("Command cancelled".to_string()),
            outcome = crate::extension_host::run_command_for_plugins(&reference, &request.args, Some(thread_id), &plugins) => outcome,
        };
        drop(admission);
        let mut result = RuntimeCommandResult {
            operation_key: String::new(),
            name: request.name,
            origin: reference.origin.clone(),
            status: "completed".to_string(),
            text: String::new(),
            extension: reference.receipt(),
            turn: None,
            thread: None,
            idempotent_replay: false,
        };
        match outcome {
            Ok(crate::extension_host::command::CommandOutcome::Show { text }) => result.text = text,
            Ok(crate::extension_host::command::CommandOutcome::Submit { prompt, note }) => {
                result.text = note.unwrap_or_default();
                if cancel.is_cancelled() {
                    result.status = "cancelled".to_string();
                    result.text = "Command cancelled before submitting a turn".to_string();
                } else {
                    let submitted = self
                        .start_turn_with_source(
                            thread_id,
                            StartTurnRequest {
                                prompt,
                                expected_workspace: Some(request.expected_workspace),
                                operation_key: Some(format!("extension-command-{fingerprint}")),
                                input_summary: Some(format!(
                                    "/{} ({})",
                                    result.name, reference.origin
                                )),
                                ..StartTurnRequest::default()
                            },
                            RuntimeTurnInputSource::ExtensionCommand {
                                reference,
                                operation_fingerprint: fingerprint.clone(),
                                cancel: cancel.clone(),
                            },
                            None,
                            false,
                        )
                        .await;
                    match submitted {
                        Ok((turn, _)) => {
                            if cancel.is_cancelled() {
                                self.cancel_extension_command(thread_id, &request.operation_key)
                                    .await?;
                            }
                            result.turn = Some(turn);
                            result.thread = self.get_thread(thread_id).await.ok();
                        }
                        Err(error) => {
                            let manager = self.clone();
                            let lookup_thread = thread_id.to_string();
                            let lookup_fingerprint = fingerprint.clone();
                            let lookup_extension = result.extension.clone();
                            let turn = tokio::task::spawn_blocking(move || {
                                manager.lookup_extension_command_turn(&lookup_thread, &lookup_fingerprint, &lookup_extension)
                            })
                            .await
                            .context("Command turn acknowledgement is uncertain; receipt lookup worker failed")?
                            .context("Command turn acknowledgement is uncertain; use the receipt lookup before retrying")?;
                            result.status = if cancel.is_cancelled() {
                                "cancelled"
                            } else {
                                "failed"
                            }
                            .to_string();
                            result.text = if let Some(turn) = turn {
                                if cancel.is_cancelled() {
                                    self.cancel_extension_command(
                                        thread_id,
                                        &request.operation_key,
                                    )
                                    .await?;
                                }
                                result.thread = Some(self.get_thread(thread_id).await?);
                                result.turn = Some(turn);
                                format!(
                                    "Command completed, but turn acknowledgement is uncertain. Its admitted turn was recovered; the command callback was not rerun: {error}"
                                )
                            } else {
                                format!(
                                    "Command completed, but no admitted turn was recovered: {error}"
                                )
                            };
                        }
                    }
                }
            }
            Err(error) => {
                result.status = if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                }
                .to_string();
                result.text = error;
            }
        }
        let settled = self
            .emit_event(
                thread_id,
                result.turn.as_ref().map(|turn| turn.id.as_str()),
                None,
                "extension_command_completed",
                json!({ "operation_fingerprint": fingerprint, "result": result }),
            )
            .await;
        settled?;
        result.operation_key = request.operation_key;
        Ok(result)
    }

    pub async fn start_turn(&self, thread_id: &str, req: StartTurnRequest) -> Result<TurnRecord> {
        self.start_turn_inner(thread_id, req, None).await
    }

    /// Start a turn and report whether the durable `operation_key` made it a
    /// replay of an already-accepted submission.
    ///
    /// The distinction belongs to admission, not to the route: a client that
    /// retried an ambiguous submit needs to be told "this is the turn you
    /// already started" so it does not render a duplicate, and only the
    /// admission path knows that for certain.
    pub async fn start_turn_reporting_replay(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
    ) -> Result<(TurnRecord, bool)> {
        self.start_turn_inner_reporting_replay(thread_id, req, None)
            .await
    }

    pub async fn start_turn_with_reserved_id(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
        reserved_turn_id: &str,
    ) -> Result<TurnRecord> {
        validated_record_id(reserved_turn_id, "reserved turn id")?;
        let turn = self
            .start_turn_inner(thread_id, req, Some(reserved_turn_id))
            .await?;
        if turn.id != reserved_turn_id {
            bail!("reserved Runtime turn id does not match the durable operation binding");
        }
        Ok(turn)
    }

    pub(super) async fn start_turn_inner(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
        reserved_turn_id: Option<&str>,
    ) -> Result<TurnRecord> {
        self.start_turn_inner_reporting_replay(thread_id, req, reserved_turn_id)
            .await
            .map(|(turn, _replayed)| turn)
    }

    pub(super) async fn start_turn_inner_reporting_replay(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
        reserved_turn_id: Option<&str>,
    ) -> Result<(TurnRecord, bool)> {
        if reserved_turn_id.is_some() && req.operation_key.is_none() {
            bail!("a reserved turn id requires an operation key");
        }
        self.start_turn_with_source(
            thread_id,
            req,
            RuntimeTurnInputSource::ExternalUser,
            reserved_turn_id,
            false,
        )
        .await
    }

    /// Retry only bytes recovered from validated durable history. This internal
    /// entry point changes the historical byte ceiling, never model or policy
    /// admission, and cannot be selected by a network request field.
    pub async fn start_turn_from_stored_images(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
    ) -> Result<TurnRecord> {
        self.start_turn_with_source(
            thread_id,
            req,
            RuntimeTurnInputSource::ExternalUser,
            None,
            true,
        )
        .await
        .map(|(turn, _replayed)| turn)
    }

    /// Returns the turn and whether the durable operation key made this a
    /// replay of an already-accepted submission rather than a new admission.
    pub(super) async fn start_turn_with_source(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
        input_source: RuntimeTurnInputSource,
        reserved_turn_id: Option<&str>,
        stored_image_bytes: bool,
    ) -> Result<(TurnRecord, bool)> {
        self.start_turn_with_narrowing(
            thread_id,
            req,
            input_source,
            reserved_turn_id,
            stored_image_bytes,
            crate::core::engine::TurnNarrowing::Inherit,
        )
        .await
    }

    /// Trusted captured ACP frontend only; no request field can select it.
    pub async fn start_acp_turn(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
    ) -> Result<TurnRecord> {
        self.start_turn_with_narrowing(
            thread_id,
            req,
            RuntimeTurnInputSource::ExternalUser,
            None,
            false,
            crate::core::engine::TurnNarrowing::Acp,
        )
        .await
        .map(|(turn, _)| turn)
    }

    pub(super) async fn start_turn_with_narrowing(
        &self,
        thread_id: &str,
        req: StartTurnRequest,
        input_source: RuntimeTurnInputSource,
        reserved_turn_id: Option<&str>,
        stored_image_bytes: bool,
        narrowing: crate::core::engine::TurnNarrowing,
    ) -> Result<(TurnRecord, bool)> {
        // Heap-allocate the turn-start state machine. Its future holds two full
        // Config clones plus ThreadRecord/EngineHandle/TurnRecord/TurnItemRecord
        // and the Op::SendMessage, and inlines the large ensure_engine_loaded
        // sub-future (which builds a full EngineConfig), all across ~8 sequential
        // .awaits. On Windows the runtime thread stack is ~1 MiB and this
        // monolithic frame overflowed it (test
        // start_turn_accepts_dynamic_tools_and_environment_id on windows-latest,
        // STATUS_STACK_OVERFLOW). Box::pin moves the whole frame to the heap so
        // no caller's stack carries it; behavior is unchanged.
        Box::pin(async move {
        // Keep config publication and turn admission in one ordering domain.
        // This read lease spans route/classifier resolution and the durable
        // engine handoff, so a completed reload is a hard boundary: no later
        // dispatch can carry its predecessor's URL, key, model, or policy.
        let _config_admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        {
            let pending = self.pending_commands.lock();
            let permitted = match &input_source {
                RuntimeTurnInputSource::ExtensionCommand { operation_fingerprint, .. } => Some(operation_fingerprint.as_str()),
                _ => None,
            };
            anyhow::ensure!(!pending.keys().any(|(id, fingerprint)| id == thread_id && Some(fingerprint.as_str()) != permitted), "Thread already has an active command");
        }
        if let RuntimeTurnInputSource::ExtensionCommand { reference, cancel, .. } = &input_source {
            anyhow::ensure!(!cancel.is_cancelled(), "Command cancelled before submitting a turn");
            crate::extension_host::command::validate_reference(reference, thread_id).await.map_err(anyhow::Error::msg)?;
        }
        if let RuntimeTurnInputSource::GoalContinuation { expected_goal, .. } = &input_source {
            anyhow::ensure!(
                expected_goal.status == codewhale_protocol::ThreadGoalStatus::Active
                    && self.store.load_goal(thread_id)?.as_ref() == Some(expected_goal.as_ref()),
                "Goal changed before its automatic turn could start"
            );
        }
        if matches!(&input_source, RuntimeTurnInputSource::Review(_)) {
            anyhow::ensure!(!narrowing.is_acp(), "structured review is unavailable in an ACP turn");
            anyhow::ensure!(self.store.load_goal(thread_id)?.is_none(), "structured review requires a thread without a goal");
        }
        let image_blocks = if stored_image_bytes {
            crate::image_attach::prepare_stored_images(&req.images)?
        } else {
            crate::image_attach::prepare_runtime_images(&req.images)?
        };
        let prompt = req.prompt.trim().to_string();
        if prompt.is_empty() {
            bail!("prompt is required");
        }

        let thread = self.get_thread(thread_id).await?;
        if req.expected_workspace.as_ref().is_some_and(|workspace| workspace != &thread.workspace) {
            bail!("Thread workspace changed from the selected frontend admission");
        }
        let expected_workspace = req.expected_workspace.clone();
        let turn_reasoning_preference = req
            .reasoning_effort
            .as_deref()
            .map(parse_runtime_reasoning_effort)
            .transpose()?;
        let thread_reasoning_preference = thread
            .reasoning_effort
            .as_deref()
            .map(parse_runtime_reasoning_effort)
            .transpose()
            .with_context(|| format!("Thread {thread_id} has invalid reasoning_effort"))?;
        let policy =
            if req.mode.is_some() || req.permission_posture.is_some() || req.auto_approve.is_some()
            {
                runtime_policy_with_overrides(
                    &thread,
                    req.mode.as_deref(),
                    req.permission_posture.as_deref(),
                    req.auto_approve,
                )?
            } else {
                RuntimePolicyProjection::from_persisted(
                    &thread.mode,
                    thread.permission_posture.as_deref(),
                    thread.auto_approve,
                )
            };
        let mode = policy.mode;
        let mut cfg_snapshot = self.config.read().clone();
        // Optional per-turn provider override: routes this turn only. The
        // saved thread keeps its provider; `route_thread` is the view the
        // route, fingerprint and turn receipt are resolved from.
        let turn_provider = requested_provider_identity(
            &cfg_snapshot,
            req.model_provider.as_deref(),
            req.model_provider_id.as_deref(),
        )?;
        let mut route_thread = thread.clone();
        let requested_model = match turn_provider.as_ref() {
            Some(identity) => {
                route_thread.model_provider = Some(identity.persisted_kind().to_string());
                route_thread.model_provider_id = identity.persisted_id().map(str::to_string);
                match req.model.as_deref() {
                    Some(model) => model.to_string(),
                    None if thread.model.trim().eq_ignore_ascii_case("auto") => {
                        thread.model.clone()
                    }
                    None => ready_provider_route(&cfg_snapshot, identity, None)?.model.clone(),
                }
            }
            None => req.model.as_deref().unwrap_or(&thread.model).to_string(),
        };
        if req.account_model_owner.is_some() {
            let identity = self.provider_identity_for_thread(&cfg_snapshot, &route_thread)?;
            anyhow::ensure!(identity.provider == ProviderKind::Codewhale && identity.key.as_str() == "codewhale"
                && requested_model.split_once('/').is_some_and(|(provider, model)| !provider.is_empty() && !provider.eq_ignore_ascii_case("codewhale") && !model.is_empty())
                && !requested_model.chars().any(|c| c.is_whitespace() || c.is_control()),
                "The agent model requires an exact account provider and model");
        }
        let auto_model = requested_model.trim().eq_ignore_ascii_case("auto");
        if !auto_model
            && let Ok(identity) = self.provider_identity_for_thread(&cfg_snapshot, &route_thread)
        {
            crate::provider_lake::refresh_unverified_local_context(
                &cfg_snapshot,
                &identity,
                &requested_model,
            )
            .await;
        }
        if !image_blocks.is_empty() && (requested_model.is_empty() || requested_model.trim() != requested_model) {
            bail!("image inputs require an exact nonempty named model");
        }
        if !image_blocks.is_empty() && auto_model {
            bail!("image inputs require an exact named model with supported image input; Auto is unavailable for images");
        }
        let configured_reasoning_preference = cfg_snapshot
            .reasoning_effort()
            .map(crate::reasoning_preference::ReasoningEffort::from_setting);
        // Runtime API precedence is explicit and stable: a turn override wins
        // over its persisted thread default, which wins over normal config.
        let reasoning_preference = turn_reasoning_preference
            .or(thread_reasoning_preference)
            .or(configured_reasoning_preference);
        let allow_shell = req.allow_shell.unwrap_or(thread.allow_shell);
        let trust_mode = req.trust_mode.unwrap_or(thread.trust_mode);
        let auto_approve = policy.auto_approve();
        let allowed_tools = req
            .allowed_tools
            .clone()
            .or_else(|| thread.allowed_tools.clone());
        if req.max_output_tokens.is_some() && auto_model {
            bail!("maxOutputTokens requires an exact model; Auto routing is unsupported");
        }
        if let Some(snapshot) = &req.profile_constitution {
            snapshot.validate()?;
            anyhow::ensure!(req.account_model_owner.as_ref().is_none_or(|owner| &snapshot.account_id == owner),
                "The profile constitution and agent model must belong to the same account");
        }
        let operation = if let Some(operation_key) = req.operation_key.as_deref() {
            validate_runtime_turn_operation_key(operation_key)?;
            let request_fingerprint = runtime_turn_request_fingerprint(
                &route_thread,
                &prompt,
                req.input_summary.as_deref(),
                &requested_model,
                reasoning_preference,
                allowed_tools.as_deref(),
                policy,
                allow_shell,
                trust_mode,
                &req.dynamic_tools,
                req.environment_id.as_deref(),
                &req.images,
                req.max_output_tokens,
            )?;
            let request_fingerprint = if let Some(workspace) = expected_workspace.as_ref() {
                codewhale_runtime::hashing::sha256_hex(codewhale_runtime::hashing::canonical_json(&json!({
                    "domain": "codewhale:selected-workspace-turn:v1",
                    "historical_fingerprint": request_fingerprint,
                    "expected_workspace": workspace,
                })).as_bytes())
            } else { request_fingerprint };
            let request_fingerprint = if let Some(snapshot) = &req.profile_constitution {
                codewhale_runtime::hashing::sha256_hex(codewhale_runtime::hashing::canonical_json(&json!({
                    "domain": "codewhale:profile-constitution-turn:v1",
                    "historical_fingerprint": request_fingerprint,
                    "profile_constitution": snapshot,
                })).as_bytes())
            } else { request_fingerprint };
            let request_fingerprint = if let Some(owner) = &req.account_model_owner {
                codewhale_runtime::hashing::sha256_hex(codewhale_runtime::hashing::canonical_json(&json!({
                    "domain": "codewhale:account-model-owner-turn:v1",
                    "historical_fingerprint": request_fingerprint,
                    "account_model_owner": owner,
                })).as_bytes())
            } else { request_fingerprint };
            let request_fingerprint = if let RuntimeTurnInputSource::Review(review) = &input_source {
                codewhale_runtime::hashing::sha256_hex(serde_json::to_vec(&(
                    "codewhale.runtime.review-turn.v1",
                    request_fingerprint,
                    crate::core::engine::review::fingerprint(review)?,
                ))?)
            } else { request_fingerprint };
            let request_fingerprint=narrowing.request_fingerprint(request_fingerprint);
            let request_fingerprint = if matches!(&input_source, RuntimeTurnInputSource::ExtensionCommand { .. }) {
                codewhale_runtime::hashing::sha256_hex(codewhale_runtime::hashing::canonical_json(&json!({
                    "domain": "codewhale:extension-command-turn:v1",
                    "historical_fingerprint": request_fingerprint,
                    "input_metadata": input_source.item_metadata(),
                })).as_bytes())
            } else { request_fingerprint };
            self.prepare_runtime_turn_operation(
                thread_id,
                Some(operation_key),
                request_fingerprint,
                reserved_turn_id,
            )?
        } else {
            None
        };
        if let Some(operation) = operation.as_ref()
            && let Some(original_turn) = self.replay_turn_for_operation(operation)?
        {
            return Ok((original_turn, true));
        }
        if let Some(owner) = &req.account_model_owner {
            cfg_snapshot.bind_account_model_owner(owner)?;
        }
        if !image_blocks.is_empty() || req.max_output_tokens.is_some() {
            let identity = self.provider_identity_for_thread(&cfg_snapshot, &route_thread)?;
            let route = resolve_runtime_thread_route_for_identity(&cfg_snapshot, &identity, Some(&requested_model))?;
            if !image_blocks.is_empty() && route.candidate.capabilities().image_input != codewhale_config::route::CapabilityState::Supported {
                bail!("image inputs require a model with explicitly supported image input");
            }
            if req.max_output_tokens.is_some() && !crate::route_budget::route_supports_output_token_limit(route.identity.provider, route.candidate.protocol()) {
                bail!("maxOutputTokens is unsupported by the selected provider transport");
            }
        }
        let engine = self.ensure_engine_loaded(&thread).await?;

        let client_preflight_required = {
            let active = self.active.lock().await;
            if let Some(active_thread) = active.engines.get(thread_id)
                && active_thread.active_turn.is_some()
            {
                bail!("Thread already has an active turn");
            }
            active
                .engines
                .get(thread_id)
                .is_none_or(|state| state.client_preflight_required)
        };

        // Resolve the concrete provider/model before persisting a turn. Auto
        // routing can fail, and such a failure must not leave a zombie
        // in-progress record behind.
        let identity = self.provider_identity_for_thread(&cfg_snapshot, &route_thread)?;
        let mut thread_config = cfg_snapshot.clone();
        thread_config.scope_to_provider_identity(&identity).map_err(anyhow::Error::msg)?;
        let verbosity = thread_config.verbosity.clone();
        let (
            route,
            reasoning_effort,
            auto_controls_reasoning,
            initial_routed_usage,
            mut initial_routed_usage_settlement,
        ) = if auto_model {
            let classifier_cost_scope = crate::cost_status::scope_token();
            let mut selection = crate::model_routing::resolve_auto_route_with_inventory(
                &thread_config,
                &prompt,
                "",
                "auto",
                "auto",
            )
            .await?;
            // The classifier call has already completed. Move its immutable
            // receipts out before resolving the selected parent route so a
            // malformed/stale parent catalog entry cannot erase real
            // auxiliary spend on the error path.
            let initial_routed_usage = crate::cost_status::RuntimeUsageBatch {
                decisions: Vec::new(),
                records: std::mem::take(&mut selection.routed_usage),
                drop_records: std::mem::take(&mut selection.routed_usage_drop_records),
                dropped_records: std::mem::take(
                    &mut selection.routed_usage_dropped_records,
                ),
            };
            // Until the new turn is durably persisted and handed to the
            // engine, every early return must settle this completed
            // classifier call against its captured origin. The bounded clone
            // is intentionally kept out of the original/replayed turn.
            let settlement = InitialRoutedUsageSettlementGuard::new(
                self.store.clone(),
                thread_id,
                classifier_cost_scope,
                &initial_routed_usage,
            );
            let route = resolve_runtime_thread_route(
                &thread_config,
                &selection.provider,
                Some(&selection.model),
            )?;
            let (selected_reasoning, auto_controls_reasoning) =
                crate::model_routing::resolve_auto_model_reasoning(
                    reasoning_preference,
                    selection.reasoning_effort,
                );
            let reasoning_effort = selected_reasoning.map(|effort| {
                effort
                    .normalize_for_route_identity(
                        route.identity.provider,
                        route.identity.key.as_str(),
                        &route.candidate.endpoint().base_url,
                        &route.model,
                    )
                    .as_setting()
                    .to_string()
            });
            (
                route,
                reasoning_effort,
                auto_controls_reasoning,
                initial_routed_usage,
                Some(settlement),
            )
        } else {
            let route = resolve_runtime_thread_route_for_identity(
                &cfg_snapshot,
                &identity,
                Some(&requested_model),
            )?;
            let auto_controls_reasoning = matches!(
                reasoning_preference,
                Some(crate::reasoning_preference::ReasoningEffort::Auto)
            );
            let selected_reasoning = reasoning_preference.map(|effort| {
                if effort == crate::reasoning_preference::ReasoningEffort::Auto {
                    crate::auto_reasoning::select()
                } else {
                    effort
                }
            });
            let reasoning_effort = selected_reasoning.map(|effort| {
                effort
                    .normalize_for_route_identity(
                        route.identity.provider,
                        route.identity.key.as_str(),
                        &route.candidate.endpoint().base_url,
                        &route.model,
                    )
                    .as_setting()
                    .to_string()
            });
            (
                route,
                reasoning_effort,
                auto_controls_reasoning,
                crate::cost_status::RuntimeUsageBatch::default(),
                None,
            )
        };
        let route = if client_preflight_required || turn_provider.is_some() || req.account_model_owner.is_some() {
            route
                .preflight()
                .map_err(|reason| anyhow!("Failed to validate runtime thread route: {reason}"))?
        } else {
            route
        };
        let configured_sandbox_mode = route.config.sandbox_mode.clone();
        let provider = route.identity.provider;
        let provider_identity = route.identity.clone();
        let model = route.model.clone();
        let route_limits = known_route_limits(route.candidate.limits());
        let max_output_tokens = req.max_output_tokens.and_then(|requested| {
            std::num::NonZeroU32::new(crate::route_budget::effective_max_output_tokens_for_turn_with_window(
                provider,
                &model,
                route_limits,
                Some(route.context_window.tokens),
                Some(requested),
                reasoning_effort.as_deref(),
            ))
        });
        let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
        let mut compaction = runtime_compaction_config(
            &route.config,
            provider,
            &model,
            (route_limits, Some(route.context_window.tokens)),
            settings.auto_compact,
            codewhale_runtime::settings::Settings::auto_compact_explicitly_configured(),
            settings.auto_compact_threshold_percent,
        );
        let now = Utc::now();
        let turn_id = operation
            .as_ref()
            .map(|operation| operation.binding.turn_id.clone())
            .unwrap_or_else(|| runtime_record_id("turn"));
        compaction.runtime_cost_owner = Some(turn_id.clone());
        let input_summary = req
            .input_summary
            .clone()
            .unwrap_or_else(|| summarize_text(&prompt, SUMMARY_LIMIT));
        let mut turn = TurnRecord {
            prefix_checkpoint: None,
            decision_receipts: Vec::new(),
            max_output_tokens,
            schema_version: if max_output_tokens.is_some() { OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION } else { CURRENT_RUNTIME_SCHEMA_VERSION },
            id: turn_id.clone(),
            thread_id: thread_id.to_string(),
            status: RuntimeTurnStatus::InProgress,
            input_summary: input_summary.clone(),
            created_at: now,
            started_at: Some(now),
            ended_at: None,
            duration_ms: None,
            usage: None,
            routing_settlement: false,
            effective_route_usage: None,
            permission_posture: Some(policy.permission_wire().to_string()),
            mode: Some(mode.as_setting().to_string()),
            effective_provider: Some(provider_identity.persisted_kind().to_string()),
            effective_provider_id: provider_identity
                .persisted_id()
                .map(codewhale_runtime::safe_label::sanitize_persisted_route_label),
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: Some(codewhale_runtime::safe_label::sanitize_persisted_route_label(&model)),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            model_request_diagnostics: None,
            operation_activity: None,
            error: None,
            item_ids: Vec::new(),
            steer_count: 0,
            agent_mail_message_id: input_source.mail_message_id().map(str::to_string),
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        };
        append_initial_routed_usage_to_turn(&mut turn, &initial_routed_usage);
        // The engine's TurnComplete owns synchronous dropped coverage,
        // including this classifier batch. Pre-persisting the count here and
        // adding TurnComplete at settlement would count the same gap twice.

        let user_item_id = runtime_record_id("item");
        let mut user_item = TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: user_item_id.clone(),
            turn_id: turn_id.clone(),
            kind: TurnItemKind::UserMessage,
            status: TurnItemLifecycleStatus::Completed,
            summary: input_summary,
            detail: input_source.item_detail(&prompt),
            metadata: input_source.item_metadata(),
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(now),
            ended_at: Some(now),
        };
        if !image_blocks.is_empty() {
            let mut content = vec![ContentBlock::Text { text: if cfg_snapshot.runtime_chat_isolated { crate::core::engine::sanitize_isolated_chat_attachments(prompt.clone()) } else { prompt.clone() }, cache_control: None }];
            content.extend(image_blocks.iter().cloned());
            user_item.set_image_content(content);
            if turn.max_output_tokens.is_none() {
                turn.schema_version = IMAGE_RUNTIME_SCHEMA_VERSION;
            }
        }
        turn.item_ids.push(user_item_id.clone());

        // Every turn carries the persisted goal alongside its message. The
        // engine's `handle_send_message` installs these fields into its
        // host-surface projection unconditionally, so passing `None` here
        // would clear an injected goal on any ordinary message. Passing the
        // durable record keeps the engine aligned with the store; a replaced
        // objective (PUT) still resets counters through the same sync path.
        let turn_goal = self.store.load_goal(thread_id)?;
        if let RuntimeTurnInputSource::GoalContinuation { expected_goal, .. } = &input_source {
            anyhow::ensure!(
                turn_goal.as_ref() == Some(expected_goal.as_ref()),
                "Goal changed before its automatic turn could start"
            );
        }
        let turn_goal_objective = turn_goal
            .as_ref()
            .map(|goal| goal.objective.trim().to_string())
            .filter(|objective| !objective.is_empty());
        let turn_goal_token_budget = turn_goal
            .as_ref()
            .and_then(|goal| goal.token_budget)
            .and_then(|value| u32::try_from(value.max(0)).ok());
        let turn_goal_status = turn_goal
            .as_ref()
            .map(|goal| {
                crate::tools::goal::thread_goal_status_projection(
                    goal.status.clone(),
                    goal.pause_reason,
                )
                .0
            })
            .unwrap_or(crate::tools::goal::GoalStatus::Active);

        let turn_hook_executor = self
            .active
            .lock()
            .await
            .engines
            .get(thread_id)
            .and_then(|state| state.hook_executor.clone());
        let spec = TurnSpec {
            profile_constitution: req.profile_constitution,
            max_output_tokens,
            content: prompt,
            images: req.images,
            mode,
            route: Box::new(route),
            compaction: Box::new(compaction),
            initial_routed_usage: Box::new(initial_routed_usage),
            goal_objective: turn_goal_objective,
            goal_token_budget: turn_goal_token_budget,
            goal_status: turn_goal_status,
            reasoning_effort,
            reasoning_effort_auto: auto_controls_reasoning,
            auto_model,
            allow_shell,
            trust_mode,
            auto_approve,
            translation_enabled: false,
            allowed_tools,
            dynamic_tools: req.dynamic_tools,
            // The turn op re-installs the executor into the engine, so it
            // must carry the thread's own, never `None`.
            hook_executor: if narrowing.is_acp() || matches!(&input_source, RuntimeTurnInputSource::Review(_)) { None } else { turn_hook_executor },
            approval_mode: policy.permission,
            verbosity,
            provenance: input_source.provenance(),
            // Durable-runtime submissions carry no host correlation token.
            submission_id: None,
        };
        let op = match &input_source {
            RuntimeTurnInputSource::Review(request) => Op::Review {
                turn: Box::new(spec),
                request: request.clone(),
            },
            _ => Op::SendMessage(spec),
        };

        // Reserve mailbox capacity before claiming or persisting anything.
        // If the caller is cancelled while capacity is unavailable, no
        // durable or in-memory turn state has changed.
        let permit = engine
            .tx_op
            .clone()
            .reserve_owned()
            .await
            .map_err(|_| anyhow!("Failed to start turn: engine operation channel closed"))?;

        if let RuntimeTurnInputSource::ExtensionCommand { reference, cancel, .. } = &input_source {
            anyhow::ensure!(!cancel.is_cancelled(), "Command cancelled before submitting a turn");
            crate::extension_host::command::validate_reference(reference, thread_id).await.map_err(anyhow::Error::msg)?;
        }
        let acceptance_rx = {
            // Lock order is active -> thread_mutation. Neither guard crosses
            // an await, and spawning the owned lifecycle task is synchronous.
            // The operation claim is an OS-backed file lock, so another
            // Runtime process sharing this store cannot pass the replay check
            // or persist a competing turn for the same operation key.
            let mut active = self.active.lock().await;
            if let RuntimeTurnInputSource::ExtensionCommand { reference, cancel, .. } = &input_source {
                anyhow::ensure!(!cancel.is_cancelled(), "Command cancelled before submitting a turn");
                crate::extension_host::command::validate_selected_registration(reference, thread_id).map_err(anyhow::Error::msg)?;
            }
            {
                let pending = self.pending_commands.lock();
                let permitted = match &input_source {
                    RuntimeTurnInputSource::ExtensionCommand { operation_fingerprint, .. } => Some(operation_fingerprint.as_str()),
                    _ => None,
                };
                anyhow::ensure!(!pending.keys().any(|(id, fingerprint)| id == thread_id && Some(fingerprint.as_str()) != permitted), "Thread already has an active command");
            }
            let mut operation_claim = operation
                .as_ref()
                .map(|operation| {
                    self.store.open_turn_operation_claim_lock(
                        &operation.binding.operation_key_fingerprint,
                    )
                })
                .transpose()?
                .map(fd_lock::RwLock::new);
            let operation_claim_guard = operation_claim
                .as_mut()
                .map(|claim| self.store.acquire_turn_operation_claim(claim))
                .transpose()?;
            // A concurrent exact retry may have crossed the first lookup
            // before the original request committed its binding. Recheck
            // under the same claim lock before inspecting active-turn state or
            // persisting/sending anything.
            if let Some(operation) = operation.as_ref()
                && let Some(original_turn) = self.replay_turn_for_operation(operation)?
            {
                return Ok((original_turn, true));
            }
            let Some(state) = active.engines.get_mut(thread_id) else {
                bail!("Thread engine not loaded");
            };
            if state.active_turn.is_some() {
                bail!("Thread already has an active turn");
            }
            let _goal_mutation = self.store.goal_mutation.lock();
            if self.store.load_goal(thread_id)? != turn_goal {
                bail!("Goal changed while preparing the turn; retry");
            }
            engine.restore_runtime_goal(turn_goal.as_ref())?;
            let _thread_mutation = self.store.thread_mutation.lock();
            let mut current_thread = self.store.load_thread(thread_id)?;
            if expected_workspace.as_ref().is_some_and(|workspace| workspace != &current_thread.workspace) {
                bail!("Thread workspace changed before selected frontend admission");
            }
            if !thread_execution_state_matches(&thread, &current_thread) {
                bail!("Thread execution settings changed while preparing the turn; retry");
            }
            let previous_active_route = (state.route_identity.clone(), state.route_model.clone());
            state.active_turn = Some(ActiveTurnState {
                narrowing,
                goal_id: (!narrowing.is_acp()).then(||turn_goal.as_ref().map(|goal| goal.goal_id.clone())).flatten(),
                turn_id: turn_id.clone(),
                goal_progress: None,
                interrupt_requested: false,
                compaction_id: None,
            });
            state.route_identity = provider_identity;
            state.route_model.clone_from(&model);

            let persistence_result = (|| -> Result<()> {
                if let Some(operation) = operation.as_ref() {
                    self.store
                        .save_turn_operation_binding(&operation.binding)?;
                }
                self.store.save_item(&user_item)?;
                self.store.save_turn(&turn)?;
                current_thread.schema_version = current_thread.schema_version.max(turn.schema_version);
                current_thread.latest_turn_id = Some(turn_id.clone());
                current_thread.updated_at = now;
                self.store.save_thread(&current_thread)
            })();
            if let Err(persistence_error) = persistence_result {
                let cleanup_error = self
                    .cleanup_unaccepted_turn_records(
                        &turn_id,
                        Some(&user_item_id),
                        operation
                            .as_ref()
                            .map(|operation| operation.binding.operation_key_fingerprint.as_str()),
                    )
                    .err();
                state.active_turn = None;
                state.route_identity = previous_active_route.0;
                state.route_model = previous_active_route.1;
                return match cleanup_error {
                    None => Err(anyhow!("Failed to persist turn: {persistence_error}")),
                    Some(cleanup_error) => Err(anyhow!(
                        "Failed to persist turn: {persistence_error}; cleanup also failed: {cleanup_error}"
                    )),
                };
            }

            // The binding, item, turn, and thread pointer are now durable.
            // Release the cross-process claim before handing the provider work
            // to the engine; exact retries will observe and replay this turn.
            drop(operation_claim_guard);
            drop(operation_claim);

            self.register_runtime_usage_sink(&turn_id);
            // Sending through an owned permit cannot await or fail. From this
            // point the engine owns the operation and the spawned task owns
            // lifecycle events, monitoring, and terminal cleanup even if the
            // HTTP/client future is dropped.
            engine.publish_turn_authority(
                mode,
                allow_shell,
                trust_mode,
                auto_approve,
                policy.permission,
                configured_sandbox_mode,
            );
            if narrowing.is_acp() { engine.send_reserved_acp_op(permit, op); } else { engine.send_reserved_op(permit, op); }
            if let Some(settlement) = initial_routed_usage_settlement.as_mut() {
                settlement.disarm();
            }
            touch_lru(&mut active.lru, thread_id);
            self.spawn_claimed_turn_monitor(
                turn.clone(),
                Some(user_item),
                engine.clone(),
                ClaimedTurnKind::Message {
                    narrowing,
                    structured_review: matches!(&input_source, RuntimeTurnInputSource::Review(_)),
                },
            )
        };

        let turn = acceptance_rx
            .await
            .map_err(|_| anyhow!("Turn lifecycle task ended before acknowledgement"))?
            .map_err(anyhow::Error::msg)?;
        Ok((turn, false))
        })
        .await
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<TurnRecord> {
        {
            let mut active = self.active.lock().await;
            let Some(active_thread) = active.engines.get_mut(thread_id) else {
                bail!("Thread is not loaded");
            };
            let Some(active_turn) = active_thread.active_turn.as_mut() else {
                bail!("No active turn on thread {thread_id}");
            };
            if active_turn.turn_id != turn_id {
                bail!("Turn {turn_id} is not active on thread {thread_id}");
            }
            active_turn.interrupt_requested = true;
            // Wake the monitor's approval wait so it can consume the Engine's
            // terminal receipt. Revoke only this turn's minted capabilities.
            // Registration takes the same active lock, so a late event cannot
            // install a new waiter after this sweep.
            self.pending_approvals.lock().retain(|_, entry| {
                entry.thread_id != thread_id || entry.request.turn_id != turn_id
            });
            if let Some(compaction_id) = active_turn.compaction_id.as_deref() {
                active_thread.engine.cancel_compaction(compaction_id)?;
            } else {
                active_thread.engine.cancel();
            }
            touch_lru(&mut active.lru, thread_id);
        }

        self.emit_event(
            thread_id,
            Some(turn_id),
            None,
            "turn.interrupt_requested",
            json!({ "thread_id": thread_id, "turn_id": turn_id }),
        )
        .await?;

        self.store.load_turn(turn_id)
    }

    pub async fn steer_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        req: SteerTurnRequest,
    ) -> Result<TurnRecord> {
        let _admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let prompt = req.prompt.trim().to_string();
        if prompt.is_empty() {
            bail!("prompt is required");
        }

        let engine = {
            let mut active = self.active.lock().await;
            let engine = {
                let Some(active_thread) = active.engines.get_mut(thread_id) else {
                    bail!("Thread is not loaded");
                };
                let Some(active_turn) = active_thread.active_turn.as_mut() else {
                    bail!("No active turn on thread {thread_id}");
                };
                if active_turn.turn_id != turn_id {
                    bail!("Turn {turn_id} is not active on thread {thread_id}");
                }
                if active_turn.interrupt_requested {
                    bail!("Turn {turn_id} is stopping and cannot be steered");
                }
                active_thread.engine.clone()
            };
            touch_lru(&mut active.lru, thread_id);
            engine
        };

        let permit = engine
            .reserve_steer()
            .await
            .map_err(|error| anyhow!("Failed to steer turn: {error}"))?;

        let now = Utc::now();
        let queued_turn;
        let item = TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: runtime_record_id("item"),
            turn_id: turn_id.to_string(),
            kind: TurnItemKind::UserMessage,
            // Queued, not Completed: the text is in the engine's mailbox, not
            // yet in the turn's record. `spawn_steer_settlement` flips it once
            // the engine reports what actually happened (#6276).
            status: TurnItemLifecycleStatus::Queued,
            summary: summarize_text(&prompt, SUMMARY_LIMIT),
            detail: Some(prompt.clone()),
            metadata: None,
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(now),
            ended_at: None,
        };
        let settle_rx = {
            let mut active = self.active.lock().await;
            let Some(active_thread) = active.engines.get(thread_id) else {
                bail!("Thread is not loaded");
            };
            let Some(active_turn) = active_thread.active_turn.as_ref() else {
                bail!("No active turn on thread {thread_id}");
            };
            if active_turn.turn_id != turn_id {
                bail!("Turn {turn_id} is not active on thread {thread_id}");
            }
            if active_turn.interrupt_requested {
                bail!("Turn {turn_id} is stopping and cannot be steered");
            }
            if !active_thread.engine.tx_op.same_channel(&engine.tx_op) {
                bail!("Thread engine changed while preparing steer; retry");
            }
            let _turn_mutation = self.store.turn_mutation.lock();
            let persistence = (|| -> Result<TurnRecord> {
                let mut turn = self.store.load_turn(turn_id)?;
                if turn.status != RuntimeTurnStatus::InProgress {
                    bail!("Turn {turn_id} is no longer in progress and cannot be steered");
                }
                self.store.save_item(&item)?;
                // `steer_count` counts steers the model actually received, so
                // it rises in the settlement path, not here.
                if !turn.item_ids.iter().any(|id| id == &item.id) {
                    turn.item_ids.push(item.id.clone());
                }
                self.store.save_turn(&turn)?;
                Ok(turn)
            })();
            let turn = match persistence {
                Ok(turn) => turn,
                Err(error) => {
                    let cleanup = self.store.remove_item(&item.id);
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(anyhow!(
                            "Failed to persist steer: {error}; cleanup also failed: {cleanup_error}"
                        )),
                    };
                }
            };
            // The reserved send has no await/failure point. From here the
            // engine and durable record agree even if the API caller drops.
            let outcome_rx = permit.send_with_outcome(prompt.clone());
            touch_lru(&mut active.lru, thread_id);
            queued_turn = turn.clone();
            self.spawn_steer_settlement(turn, item, prompt, outcome_rx)
        };

        // The settler owns the verdict; this is only an observation of it.
        // Steering a streaming turn settles in milliseconds, so the caller
        // almost always gets the truth in-band. Behind a long tool call the
        // engine will not look at its mailbox for minutes, and an API request
        // must not hang that long — so the wait is bounded and the caller
        // falls back to the honest `Queued` receipt it already has.
        match tokio::time::timeout(STEER_SETTLE_WAIT, settle_rx).await {
            Ok(Ok((SteerOutcome::Accepted, turn))) => Ok(turn),
            Ok(Ok((SteerOutcome::Dropped, _))) => bail!(
                "Turn {turn_id} moved on before the steer reached the model; {STEER_DROPPED_REASON}"
            ),
            Ok(Err(_)) => bail!("Steer settlement task ended before acknowledgement"),
            Err(_) => Ok(queued_turn),
        }
    }

    pub async fn compact_thread(
        &self,
        thread_id: &str,
        req: CompactThreadRequest,
    ) -> Result<TurnRecord> {
        // Compaction carries a concrete provider route just like a normal
        // turn. Keep the same reload/admission boundary through durable engine
        // handoff so it cannot dispatch an old credential or endpoint after a
        // successful config reload.
        let _config_admission = self.config_admission.read().await;
        self.ensure_accepting_execution()?;
        let thread = self.get_thread(thread_id).await?;
        let engine = self.ensure_engine_loaded(&thread).await?;

        let client_preflight_required = {
            let active = self.active.lock().await;
            let Some(active_thread) = active.engines.get(thread_id) else {
                bail!("Thread engine not loaded");
            };
            if active_thread.active_turn.is_some() {
                bail!("Thread already has an active turn");
            }
            active_thread.client_preflight_required
        };
        let route = self.resolved_route_for_thread(&self.read_config(), &thread)?;
        let route = if client_preflight_required {
            route
                .preflight()
                .map_err(|reason| anyhow!("Failed to validate runtime thread route: {reason}"))?
        } else {
            route
        };
        let configured_sandbox_mode = route.config.sandbox_mode.clone();
        let route_provider = route.identity.provider;
        let route_identity = route.identity.clone();
        let route_model = route.model.clone();
        let route_limits = known_route_limits(route.candidate.limits());
        let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
        let mut compaction = runtime_compaction_config(
            &route.config,
            route_provider,
            &route_model,
            (route_limits, Some(route.context_window.tokens)),
            settings.auto_compact,
            codewhale_runtime::settings::Settings::auto_compact_explicitly_configured(),
            settings.auto_compact_threshold_percent,
        );

        let now = Utc::now();
        let turn_id = runtime_record_id("turn");
        let compaction_id = runtime_record_id("compact");
        compaction.runtime_cost_owner = Some(turn_id.clone());
        // The same projection the turn record receipts, computed once: the
        // compaction runs under the thread's persisted policy.
        let projection = RuntimePolicyProjection::from_persisted(
            &thread.mode,
            thread.permission_posture.as_deref(),
            thread.auto_approve,
        );
        let turn = TurnRecord {
            prefix_checkpoint: None,
            decision_receipts: Vec::new(),
            max_output_tokens: None,
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: turn_id.clone(),
            thread_id: thread_id.to_string(),
            status: RuntimeTurnStatus::InProgress,
            input_summary: req
                .reason
                .as_deref()
                .map(|s| summarize_text(s, SUMMARY_LIMIT))
                .unwrap_or_else(|| "Manual context compaction".to_string()),
            created_at: now,
            started_at: Some(now),
            ended_at: None,
            duration_ms: None,
            usage: None,
            routing_settlement: false,
            effective_route_usage: None,
            permission_posture: Some(projection.permission_wire().to_string()),
            mode: Some(projection.mode.as_setting().to_string()),
            effective_provider: Some(route_identity.persisted_kind().to_string()),
            effective_provider_id: route_identity
                .persisted_id()
                .map(codewhale_runtime::safe_label::sanitize_persisted_route_label),
            effective_openrouter_vendor: None,
            effective_billing_surface: None,
            effective_endpoint_fingerprint: None,
            effective_provider_live_pricing: None,
            effective_billing_mode: None,
            effective_dispatched_at: None,
            effective_model: Some(
                codewhale_runtime::safe_label::sanitize_persisted_route_label(&route_model),
            ),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_source_ids: Vec::new(),
            routed_usage_dropped_records: 0,
            model_request_diagnostics: None,
            operation_activity: None,
            error: None,
            item_ids: Vec::new(),
            steer_count: 0,
            agent_mail_message_id: None,
            artifacts: Vec::new(),
            workspace: None,
            workspace_snapshots: Vec::new(),
        };
        let op = Op::CompactContext {
            id: compaction_id.clone(),
            route: Box::new(route),
            compaction: Box::new(compaction),
        };
        let permit = engine.tx_op.clone().reserve_owned().await.map_err(|_| {
            anyhow!("Failed to trigger compaction: engine operation channel closed")
        })?;

        let acceptance_rx = {
            let mut active = self.active.lock().await;
            let Some(state) = active.engines.get_mut(thread_id) else {
                bail!("Thread engine not loaded");
            };
            if state.active_turn.is_some() {
                bail!("Thread already has an active turn");
            }
            let _thread_mutation = self.store.thread_mutation.lock();
            let mut current_thread = self.store.load_thread(thread_id)?;
            if !thread_execution_state_matches(&thread, &current_thread) {
                bail!("Thread execution settings changed while preparing compaction; retry");
            }
            let previous_active_route = (state.route_identity.clone(), state.route_model.clone());
            state.active_turn = Some(ActiveTurnState {
                narrowing: crate::core::engine::TurnNarrowing::Inherit,
                goal_id: None,
                turn_id: turn_id.clone(),
                goal_progress: None,
                interrupt_requested: false,
                compaction_id: Some(compaction_id),
            });
            state.route_identity = route_identity;
            state.route_model = route_model;

            let persistence_result = (|| -> Result<()> {
                self.store.save_turn(&turn)?;
                current_thread.latest_turn_id = Some(turn_id.clone());
                current_thread.updated_at = now;
                self.store.save_thread(&current_thread)
            })();
            if let Err(persistence_error) = persistence_result {
                let cleanup_error = self
                    .cleanup_unaccepted_turn_records(&turn_id, None, None)
                    .err();
                state.active_turn = None;
                state.route_identity = previous_active_route.0;
                state.route_model = previous_active_route.1;
                return match cleanup_error {
                    None => Err(anyhow!("Failed to persist compaction: {persistence_error}")),
                    Some(cleanup_error) => Err(anyhow!(
                        "Failed to persist compaction: {persistence_error}; cleanup also failed: {cleanup_error}"
                    )),
                };
            }

            self.register_runtime_usage_sink(&turn_id);
            let policy = RuntimePolicyProjection::from_persisted(
                &current_thread.mode,
                current_thread.permission_posture.as_deref(),
                current_thread.auto_approve,
            );
            engine.publish_turn_authority(
                policy.mode,
                current_thread.allow_shell,
                current_thread.trust_mode,
                policy.auto_approve(),
                policy.permission,
                configured_sandbox_mode,
            );
            engine.send_reserved_op(permit, op);
            touch_lru(&mut active.lru, thread_id);
            self.spawn_claimed_turn_monitor(
                turn.clone(),
                None,
                engine.clone(),
                ClaimedTurnKind::Compaction,
            )
        };

        acceptance_rx
            .await
            .map_err(|_| anyhow!("Compaction lifecycle task ended before acknowledgement"))?
            .map_err(anyhow::Error::msg)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn events_since(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<RuntimeEventRecord>> {
        self.store.events_since(thread_id, since_seq)
    }

    pub async fn events_since_async(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<RuntimeEventRecord>> {
        // Startup recovery deliberately queues terminal receipts until an
        // async consumer can append them without blocking manager open. The
        // Runtime Chat relay reads this API directly (rather than first
        // loading a thread detail), so make the event boundary itself flush
        // those receipts. Otherwise a crash-recovered accepted turn could
        // remain terminal on disk without ever producing turn.completed.
        self.flush_recovery_receipts_for_thread(thread_id).await?;
        let store = self.store.clone();
        let thread_id = thread_id.to_string();
        tokio::task::spawn_blocking(move || store.events_since(&thread_id, since_seq))
            .await
            .context("Runtime event history task failed")?
    }

    pub async fn events_from_offset_async(
        &self,
        thread_id: &str,
        offset: u64,
        limit: Option<usize>,
    ) -> Result<(Vec<RuntimeEventRecord>, u64)> {
        let store = self.store.clone();
        let thread_id = thread_id.to_string();
        tokio::task::spawn_blocking(move || store.events_from_offset(&thread_id, offset, limit))
            .await
            .context("Runtime event cursor task failed")?
    }

    pub async fn replay_events(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
        tail_limit: Option<usize>,
    ) -> Result<RuntimeEventReplay> {
        if tail_limit.is_some_and(|limit| limit > MAX_RUNTIME_EVENT_REPLAY_TAIL) {
            bail!("Runtime event replay_limit cannot exceed {MAX_RUNTIME_EVENT_REPLAY_TAIL}");
        }
        #[cfg(any(test, feature = "test-support"))]
        let replay_test_hook = { self.replay_test_hook.lock().take() };
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = replay_test_hook {
            let (resume, wait_for_resume) = oneshot::channel();
            hook.send(ReplayTestPoint {
                thread_id: thread_id.to_string(),
                resume,
            })
            .map_err(|_| anyhow!("replay test hook closed"))?;
            wait_for_resume
                .await
                .map_err(|_| anyhow!("replay test hook dropped resume"))?;
        }
        let (base_tx, base_rx) = oneshot::channel();
        let (batch_tx, batches) = mpsc::channel(2);
        let store = self.store.clone();
        let thread_id = thread_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.publish_event_replay(&thread_id, since_seq, tail_limit, base_tx, batch_tx);
        });
        let base_seq = base_rx
            .await
            .context("Runtime event replay worker ended before initialization")?
            .map_err(anyhow::Error::msg)?;
        Ok(RuntimeEventReplay { base_seq, batches })
    }

    async fn ensure_engine_loaded(&self, thread_hint: &ThreadRecord) -> Result<EngineHandle> {
        self.ensure_accepting_execution()?;
        {
            let mut active = self.active.lock().await;
            self.ensure_accepting_execution()?;
            if let Some(engine) = active
                .engines
                .get(thread_hint.id.as_str())
                .map(|state| state.engine.clone())
            {
                touch_lru(&mut active.lru, &thread_hint.id);
                return Ok(engine);
            }
        }

        // Only one cache-miss build may run at a time. Recheck after taking
        // the build lock because another caller may already have won.
        let _engine_load = self.engine_load.lock().await;
        self.ensure_accepting_execution()?;
        loop {
            {
                let mut active = self.active.lock().await;
                self.ensure_accepting_execution()?;
                if let Some(engine) = active
                    .engines
                    .get(thread_hint.id.as_str())
                    .map(|state| state.engine.clone())
                {
                    touch_lru(&mut active.lru, &thread_hint.id);
                    return Ok(engine);
                }
            }
            let thread = {
                let _thread_mutation = self.store.thread_mutation.lock();
                self.store
                    .load_thread(&thread_hint.id)
                    .with_context(|| format!("Thread not found: {}", thread_hint.id))?
            };

            // Snapshot and prepare the concrete provider route once so the engine,
            // route limits, compaction budget, and restored session all agree.
            let base_config = self.read_config().clone();
            let route = self.resolved_route_for_thread(&base_config, &thread)?;
            let context_window = route.context_window;
            let provider = route.identity.provider;
            let route_identity = route.identity;
            let route_model = route.model;
            let route_limits = known_route_limits(route.candidate.limits());
            let cfg = route.config;
            let isolated_chat = cfg.runtime_chat_isolated;

            // Resolve the provider-route-aware auto-compaction default unless the
            // user persisted an explicit preference.
            let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
            let compaction = runtime_compaction_config(
                &cfg,
                provider,
                &route_model,
                (route_limits, Some(context_window.tokens)),
                settings.auto_compact,
                codewhale_runtime::settings::Settings::auto_compact_explicitly_configured(),
                settings.auto_compact_threshold_percent,
            );
            let network_policy =
                (!isolated_chat)
                    .then(|| cfg.network.clone())
                    .flatten()
                    .map(|toml_cfg| {
                        codewhale_net::network_policy::NetworkPolicyDecider::with_default_audit(
                            codewhale_net::network_policy::NetworkPolicy::from(toml_cfg),
                        )
                    });
            let lsp_config = (!isolated_chat)
                .then(|| cfg.lsp.clone())
                .flatten()
                .map(codewhale_lsp::LspConfig::from);
            let max_subagents = cfg
                .max_subagents_for_provider(&route_identity)
                .clamp(1, MAX_SUBAGENTS);
            let thread_plugin_registry = (!isolated_chat)
                .then(|| {
                    self.plugin_registry
                        .as_ref()
                        .map(|registry| registry.rediscover_for_workspace(&thread.workspace))
                })
                .flatten();
            // Hooks fire on Runtime API threads as they do in the TUI and
            // `exec --hooks` (B4): the same global, reviewed-plugin and
            // trusted-project set, for this thread's workspace.
            let thread_hooks = Arc::new(self.hook_executor_for_workspace(
                &cfg,
                &thread.workspace,
                thread_plugin_registry.as_deref(),
            ));
            // Rehydrate the persisted thread goal into the engine so the
            // goal loop, prompt surface, and `update_goal` tool operate on
            // the durable record from the first turn. Usage and continuation
            // counters are preserved; `sync_from_host_status` would reset
            // them because the fresh state's objective "changed".
            let persisted_goal = self.store.load_goal(&thread.id)?;
            let (goal_objective, goal_token_budget, goal_status, goal_state) = match &persisted_goal
            {
                Some(goal) => {
                    let objective = goal.objective.trim();
                    if objective.is_empty() {
                        (
                            None,
                            None,
                            crate::tools::goal::GoalStatus::Active,
                            crate::tools::goal::new_shared_goal_state(),
                        )
                    } else {
                        let snapshot = crate::tools::goal::GoalSnapshot::from_thread_goal(goal);
                        let status = crate::tools::goal::thread_goal_status_projection(
                            goal.status.clone(),
                            goal.pause_reason,
                        )
                        .0;
                        (
                            Some(objective.to_string()),
                            snapshot.token_budget,
                            status,
                            crate::tools::goal::new_shared_goal_state_from_snapshot(&snapshot),
                        )
                    }
                }
                None => (
                    None,
                    None,
                    crate::tools::goal::GoalStatus::Active,
                    crate::tools::goal::new_shared_goal_state(),
                ),
            };
            // One shell/job authority per thread, shared with the engine so
            // `GET /v1/jobs` sees the same jobs the model sees and background
            // work survives engine LRU eviction. The engine applies its
            // per-thread sandbox settings to the manager on construction.
            let shell_manager = {
                let mut active = self.active.lock().await;
                let manager = active
                    .shell_managers
                    .entry(thread.id.clone())
                    .or_insert_with(|| new_shared_shell_manager(thread.workspace.clone()))
                    .clone();
                drop(active);
                if let Ok(mut guard) = manager.lock() {
                    guard.set_default_workspace(thread.workspace.clone());
                }
                manager
            };
            let mut features = cfg.features();
            if self.host_profile.is_acp() {
                features.disable(codewhale_runtime::features::Feature::Mcp);
            }
            let engine_cfg = EngineConfig {
                user_input_responder: true,
                session_checkpoint_backend: Some(
                    crate::core::events::SessionCheckpointProjection::RuntimeReceiptRedacted,
                ),
                model: route_model.clone(),
                active_route_limits: route_limits,
                workspace: thread.workspace.clone(),
                // The engine runs every Runtime thread under the thread's own
                // id, from its first turn and across every restart and LRU
                // eviction. That id tags every workspace snapshot the engine
                // takes; a generated id (what `None` meant here) changed on
                // each rebuild and was recorded nowhere, so no later request
                // could prove the thread owned its own snapshots (#6621). It
                // is deliberately not `thread.session_id`: that names the
                // saved-session document the thread is bound to, which save,
                // resume and fork rebind, and the conversation's identity
                // must not change when its document does. Ownership itself is
                // the receipts recorded on the thread's turns
                // (`TurnRecord::workspace_snapshots`).
                session_id: Some(thread.id.clone()),
                subagent_state_root: None,
                plugin_registry: thread_plugin_registry.clone(),
                allow_shell: thread.allow_shell,
                trust_mode: thread.trust_mode,
                notes_path: cfg.notes_path(),
                mcp_config_path: cfg.mcp_config_path(),
                mcp_oauth_callback_port: cfg.mcp_oauth_callback_port,
                mcp_oauth_callback_url: cfg.mcp_oauth_callback_url.clone(),
                skills_dir: cfg.skills_dir(),
                skills_config: std::sync::Arc::new(cfg.skills_config()),
                instructions: if isolated_chat {
                    Vec::new()
                } else {
                    cfg.instructions_paths()
                        .into_iter()
                        .map(Into::into)
                        .collect()
                },
                project_context_pack_enabled: !isolated_chat && cfg.project_context_pack_enabled(),
                translation_enabled: false,
                // R1: runtime/API turns follow the same finite-budget
                // contract as the ordinary interactive engine.
                max_steps: cfg.max_model_steps(),
                max_subagents,
                max_admitted_subagents: cfg
                    .max_admitted_subagents_for_provider(&route_identity)
                    .max(max_subagents),
                launch_concurrency: cfg.launch_concurrency_for_provider(&route_identity),
                subagents_enabled: !isolated_chat
                    && cfg.subagents_enabled_for_provider(&route_identity),
                features,
                auto_review_policy:
                    crate::core::authority::auto_review::AutoReviewPolicy::from_config(&cfg),
                compaction,
                todos: new_shared_todo_list(),
                plan_state: new_shared_plan_state(),
                goal_state,
                max_spawn_depth: cfg.subagent_max_spawn_depth_for_provider(&route_identity),
                network_policy,
                snapshots_enabled: !isolated_chat && cfg.snapshots_config().enabled,
                snapshots_max_workspace_bytes: cfg
                    .snapshots_config()
                    .max_workspace_gb
                    .saturating_mul(1024 * 1024 * 1024),
                // Every snapshot receipt, post-turn included, reaches
                // `monitor_turn` before the turn settles, and every tool
                // call that may write is bounded by its own snapshots, so
                // turn-scoped undo can tell the turn's changes from anyone
                // else's.
                record_restore_points: true,
                lsp_config,
                runtime_services: crate::tools::spec::RuntimeToolServices {
                    task_manager: self.task_manager.lock().upgrade(),
                    automations: self.automations.lock().clone(),
                    task_data_dir: Some(self.manager_cfg.task_data_dir.clone()),
                    active_task_id: thread.task_id.clone(),
                    active_thread_id: Some(thread.id.clone()),
                    dynamic_tool_executor: if isolated_chat {
                        None
                    } else {
                        Some(Arc::new(self.clone()))
                    },
                    work: None,
                    shell_manager: Some(shell_manager),
                    persist_services_enabled: false,
                    hook_executor: Some(Arc::clone(&thread_hooks)),
                    handle_store: crate::tools::handle::new_shared_handle_store(),
                    rlm_sessions: crate::rlm::session::new_shared_rlm_session_store(),
                    media_originals_dir: codewhale_runtime::media_originals::default_store_dir(),
                },
                subagent_model_overrides: if isolated_chat {
                    HashMap::new()
                } else {
                    cfg.subagent_model_overrides()
                },
                fleet_roster: if isolated_chat {
                    Arc::new(crate::fleet::roster::FleetRoster::built_ins_only())
                } else {
                    Arc::new(crate::fleet::identity::load_effective_roster(
                        &cfg.fleet_config(),
                        &thread.workspace,
                        thread_plugin_registry.as_deref(),
                    ))
                },
                subagent_api_timeout: std::time::Duration::from_secs(
                    cfg.subagent_api_timeout_secs_for_provider(&route_identity),
                ),
                stream_chunk_timeout: std::time::Duration::from_secs(
                    cfg.stream_chunk_timeout_secs(),
                ),
                turn_wall_clock: cfg.turn_wall_clock(),
                stream_max_content_bytes: cfg.stream_max_content_bytes(),
                stream_max_duration: cfg.stream_max_duration(),
                stream_retry_limits: cfg.stream_retry_limits(),
                stream_open_timeout: cfg.stream_open_timeout(),
                subagent_heartbeat_timeout: std::time::Duration::from_secs(
                    cfg.subagent_heartbeat_timeout_secs_for_provider(&route_identity),
                ),
                prefer_bwrap: cfg.prefers_bwrap(),
                bwrap_extensions: codewhale_sandbox::BwrapMountExtensions {
                    read_only_roots: cfg.bwrap_ro_roots.clone(),
                    device_roots: cfg.bwrap_dev_roots.clone(),
                },
                read_denylist: codewhale_sandbox::read_guard::ReadDenylist::from_config(&cfg),
                memory_enabled: !isolated_chat && cfg.memory_enabled(),
                memory_path: cfg.memory_path(),
                speech_output_dir: cfg.speech_output_dir(),
                vision_config: (!isolated_chat)
                    .then(|| cfg.vision_model_config())
                    .flatten(),
                strict_tool_mode: cfg.strict_tool_mode.unwrap_or(false),
                goal_objective,
                goal_token_budget,
                goal_status,
                goal_max_continuations: cfg.goal_max_continuations(),
                goal_continuation_delay_seconds: cfg.goal_continuation_delay_seconds(),
                goal_enforce_token_budget: cfg.goal_enforce_token_budget(),
                reasoning_only_max_reprompts: cfg.reasoning_only_max_reprompts(),
                reasoning_only_on_output_limit: cfg.reasoning_only_on_output_limit(),
                reasoning_only_reprompt_message: Some(
                    cfg.reasoning_only_reprompt_message().to_string(),
                ),
                allowed_tools: isolated_chat.then(Vec::new),
                disallowed_tools: None,
                max_tool_calls: None,
                hook_executor: Some(Arc::clone(&thread_hooks)),
                locale_tag: codewhale_localization::resolve_locale(&settings.locale)
                    .tag()
                    .to_string(),
                workshop: cfg.workshop.clone(),
                search_provider: cfg.search_provider(),
                search_api_key: cfg.search.as_ref().and_then(|s| s.api_key.clone()),
                search_base_url: cfg.search.as_ref().and_then(|s| s.base_url.clone()),
                search_native: cfg.search_native(),
                tools_always_load: if isolated_chat {
                    HashSet::new()
                } else {
                    cfg.tools_always_load()
                },
                user_input_limits: crate::tools::user_input::UserInputLimits::from_config(&cfg),
                user_input_timeout: cfg.user_input_timeout(),
                goal_max_steps: Some(cfg.goal_max_steps()),
                tools: (!isolated_chat).then(|| cfg.tools.clone()).flatten(),
                verbosity: cfg.verbosity.clone(),
                workspace_follow_symlinks: settings.workspace_follow_symlinks,
                exec_policy_engine: cfg.exec_policy_engine.clone(),
                terminal_chrome_enabled: false,
                advisor_config: cfg
                    .advisor
                    .as_ref()
                    .map(crate::tools::subagent::AdvisorConfig::from_toml)
                    .unwrap_or_else(crate::tools::subagent::AdvisorConfig::disabled),
            };

            // Verify the persisted history before spawning an Engine task.
            let session_messages = self.restore_thread_messages(&thread)?;
            #[cfg(any(test, feature = "test-support"))]
            let model_client = self.test_model_client.lock().clone();
            #[cfg(not(any(test, feature = "test-support")))]
            let model_client = None;
            let subagent_manager = self.subagent_manager().await?;
            let previous = self.engine_workers.lock().clone();
            for (owner, old_engine, completion) in previous {
                if owner == thread.id {
                    old_engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
                    let _ = old_engine.send(Op::Shutdown).await;
                    await_retained_completion(&completion).await?;
                }
            }
            let (engine, worker) = spawn_engine_with_authoritative_route_config(
                engine_cfg,
                &cfg,
                self.host_profile,
                Arc::clone(&self.config),
                model_client,
                Some(subagent_manager),
                self.cancel_token.clone(),
            );
            let engine = self.route_engine_events(&thread, engine).await;
            {
                let mut workers = self.engine_workers.lock();
                workers.retain(|(_, _, worker)| !completion_finished(worker));
                workers.push((
                    thread.id.clone(),
                    engine.clone(),
                    retained_completion(worker),
                ));
            }

            let sys_prompt =
                synced_system_prompt(thread.system_prompt.as_deref(), &session_messages);
            if !session_messages.is_empty() || sys_prompt.is_some() {
                engine
                    .send(Op::SyncSession {
                        // Same identity the engine was built with: a
                        // re-sync of the same conversation, never a
                        // conversation boundary.
                        session_id: Some(thread.id.clone()),
                        system_prompt_override: sys_prompt.is_some(),
                        messages: session_messages,
                        system_prompt: sys_prompt,
                        model: route_model.clone(),
                        workspace: thread.workspace.clone(),
                        mode: RuntimePolicyProjection::from_persisted(
                            &thread.mode,
                            thread.permission_posture.as_deref(),
                            thread.auto_approve,
                        )
                        .mode,
                    })
                    .await
                    .map_err(|e| anyhow!("Failed to sync thread session: {e}"))?;
            }

            let mut active = self.active.lock().await;
            if let Some(winner) = active
                .engines
                .get(&thread.id)
                .map(|state| state.engine.clone())
            {
                touch_lru(&mut active.lru, &thread.id);
                drop(active);
                engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
                let _ = engine.try_send(Op::Shutdown);
                return Ok(winner);
            }

            // Atomically compare the record used for construction with the latest
            // durable record while holding the same active -> thread lock order as
            // updates. A concurrent workspace/model/session/policy change makes
            // this engine stale; discard it and rebuild from the new snapshot.
            let thread_mutation = self.store.thread_mutation.lock();
            let record_is_current =
                thread_execution_state_matches(&self.store.load_thread(&thread.id)?, &thread);
            if !record_is_current {
                drop(thread_mutation);
                drop(active);
                engine.cancel_with_reason(crate::core::engine::CancelReason::Internal);
                let _ = engine.try_send(Op::Shutdown);
                continue;
            }

            let pending_commands = self
                .pending_commands
                .lock()
                .keys()
                .map(|(thread_id, _)| thread_id.clone())
                .collect();
            let evicted = enforce_lru_capacity(
                &mut active,
                self.manager_cfg.max_active_threads,
                &pending_commands,
            );
            active.engines.insert(
                thread.id.clone(),
                ActiveThreadState {
                    engine: engine.clone(),
                    active_turn: None,
                    route_identity,
                    route_model,
                    hook_executor: Some(Arc::clone(&thread_hooks)),
                    client_preflight_required: true,
                },
            );
            touch_lru(&mut active.lru, &thread.id);
            drop(thread_mutation);
            drop(active);
            for handle in evicted {
                let _ = handle.send(Op::Shutdown).await;
            }
            return Ok(engine);
        }
    }

    pub(super) async fn route_engine_events(
        &self,
        thread: &ThreadRecord,
        mut engine: EngineHandle,
    ) -> EngineHandle {
        let original = engine.rx_event.clone();
        let capacity = original.read().await.max_capacity();
        let (forward, receiver) = mpsc::channel(capacity);
        engine.rx_event = Arc::new(AsyncRwLock::new(receiver));
        let identity = Arc::downgrade(&engine.rx_event);
        let active = Arc::downgrade(&self.active);
        let wake = self.event_reader_wake.clone();
        let stop = self.event_pump_stop.clone();
        let sink = RuntimeWorkerEventSink {
            thread_id: thread.id.clone(),
            workspace: thread.workspace.clone(),
            manager: engine.subagent_manager(),
            store: self.store.clone(),
            projection: self.projection_lock(&thread.id),
            event_emit: self.event_emit.clone(),
            event_tx: self.event_tx.clone(),
            notices: self.notices.clone(),
            owner: Arc::downgrade(&self._process_owner_lock),
        };
        let input_settlement = self.user_input_settlement(&thread.id);
        let handle = engine.clone();
        let thread_id = thread.id.clone();
        let pump = tokio::spawn(async move {
            'pump: loop {
                let event = tokio::select! {
                    biased;
                    event = async { original.write().await.recv().await } => match event { Some(event) => event, None => break },
                    _ = stop.cancelled() => break,
                };
                if matches!(
                    &event,
                    EngineEvent::AgentSpawned { .. }
                        | EngineEvent::AgentProgress { .. }
                        | EngineEvent::AgentComplete { .. }
                ) {
                    let origin = match sink.publish(event.clone()).await {
                        Ok(origin) => origin,
                        Err(error) => {
                            handle.mark_worker_event_delivery_failed();
                            tracing::error!(thread_id = %thread_id, "Runtime child event publication failed: {error:#}");
                            if let Some(_owner) = sink.owner.upgrade() {
                                if let Some(failure) = RuntimeStoreRecordFailure::from_error(&error)
                                {
                                    let _ = sink
                                        .emit_event(
                                            &thread_id,
                                            None,
                                            None,
                                            RUNTIME_STORE_FAILURE_EVENT,
                                            json!(failure.notice(&error, false)),
                                        )
                                        .await;
                                } else {
                                    let _ = sink.emit_event(&thread_id, None, None, "error", json!({"message": "A background agent receipt could not be recorded."})).await;
                                }
                            }
                            break;
                        }
                    };
                    let owns_origin =
                        if let (Some(origin), Some(active)) = (origin, active.upgrade()) {
                            active
                                .lock()
                                .await
                                .engines
                                .get(&thread_id)
                                .and_then(|state| state.active_turn.as_ref())
                                .is_some_and(|turn| turn.turn_id == origin)
                        } else {
                            false
                        };
                    if !owns_origin {
                        continue;
                    }
                }
                loop {
                    let notified = wake.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    let Some(active) = active.upgrade() else {
                        if let EngineEvent::SessionUpdated {
                            checkpoint: Some(checkpoint),
                            ..
                        } = &event
                        {
                            checkpoint.fail("Runtime checkpoint active owner was released");
                        }
                        break 'pump;
                    };
                    let (owns_receiver, owns_turn) = {
                        let active = active.lock().await;
                        active
                            .engines
                            .get(&thread_id)
                            .map(|state| {
                                let owns_receiver = identity.upgrade().is_some_and(|receiver| {
                                    Arc::ptr_eq(&receiver, &state.engine.rx_event)
                                });
                                (owns_receiver, owns_receiver && state.active_turn.is_some())
                            })
                            .unwrap_or((false, false))
                    };
                    if !owns_turn {
                        match &event {
                            EngineEvent::SessionUpdated {
                                checkpoint: Some(checkpoint),
                                ..
                            } => {
                                checkpoint.fail("Runtime checkpoint has no active event owner");
                            }
                            EngineEvent::ApprovalRequired { id, .. }
                            | EngineEvent::ElevationRequired { tool_id: id, .. } => {
                                let _ = handle
                                    .deny_tool_call_by(
                                        id,
                                        crate::approval_log::ApprovalDecider::Host,
                                    )
                                    .await;
                            }
                            EngineEvent::UserInputRequired { id, .. } => {
                                let _ = handle.cancel_user_input(id).await;
                            }
                            EngineEvent::UserInputClosed { id, outcome } if owns_receiver => {
                                if let Err(error) = input_settlement.close(id, None, *outcome).await
                                {
                                    tracing::error!(thread_id = %thread_id, input_id = %id, "Runtime user-input closure failed: {error:#}");
                                    break 'pump;
                                }
                            }
                            _ => {}
                        }
                        break;
                    }
                    tokio::select! {
                        permit = forward.reserve() => match permit {
                            Ok(permit) => { permit.send(event); break; },
                            Err(_) => {
                                if let EngineEvent::SessionUpdated { checkpoint: Some(checkpoint), .. } = &event {
                                    checkpoint.fail("Runtime checkpoint event receiver closed");
                                }
                                break 'pump;
                            },
                        },
                        _ = &mut notified => {},
                        _ = stop.cancelled() => {
                            if let EngineEvent::SessionUpdated { checkpoint: Some(checkpoint), .. } = &event {
                                checkpoint.fail("Runtime checkpoint event owner stopped");
                            }
                            break 'pump;
                        },
                    }
                }
            }
            handle.cancel_with_reason(crate::core::engine::CancelReason::Internal);
            let _ = handle.try_send(Op::Shutdown);
        });
        let mut monitors = self.event_monitors.lock();
        monitors.retain(|monitor| !completion_finished(monitor));
        monitors.push(retained_completion(pump));
        engine
    }

    pub async fn attach_subagent_manager(&self, manager: SharedSubAgentManager) -> Result<()> {
        let _loading = self.engine_load.lock().await;
        let mut selected = self.subagent_manager.lock().await;
        if let Some(current) = selected.as_ref() {
            anyhow::ensure!(
                Arc::ptr_eq(current, &manager),
                "Runtime already has another worker authority"
            );
        } else {
            anyhow::ensure!(
                self.active.lock().await.engines.is_empty(),
                "Runtime engines started before worker authority admission"
            );
            *selected = Some(manager);
        }
        Ok(())
    }

    pub(super) async fn subagent_manager(&self) -> Result<SharedSubAgentManager> {
        let mut selected = self.subagent_manager.lock().await;
        if let Some(manager) = selected.as_ref() {
            return Ok(manager.clone());
        }
        let workspace = self.workspace.clone();
        let manager = tokio::task::spawn_blocking(move || {
            new_shared_runtime_subagent_manager(workspace, MAX_SUBAGENTS)
        })
        .await
        .context("Runtime worker authority admission failed")??;
        *selected = Some(manager.clone());
        Ok(manager)
    }

    pub async fn admit_history_execution(&self) -> Result<()> {
        self.ensure_accepting_execution()?;
        self.subagent_manager().await.map(drop)
    }

    /// The thread's engine only when it is already live in this process.
    /// Control routes that act on in-flight work (stopping an agent run) use
    /// this: loading a cold engine cannot reach work that is not running here.
    pub async fn loaded_engine(&self, thread_id: &str) -> Option<EngineHandle> {
        self.active
            .lock()
            .await
            .engines
            .get(thread_id)
            .map(|state| state.engine.clone())
    }

    /// Get the engine handle for a thread, loading it if necessary.
    /// Public wrapper around the private `ensure_engine_loaded`.
    pub async fn get_engine(&self, thread_id: &str) -> Result<EngineHandle> {
        let thread = self.get_thread(thread_id).await?;
        self.ensure_engine_loaded(&thread).await
    }

    /// The thread's shared shell/job authority — the same manager its engine
    /// uses — so `/v1/jobs` and the model see one job set. With `create`, an
    /// API-created job works before the thread's first engine load; without
    /// it, `None` means the thread has never run shell work.
    pub async fn thread_shell_manager(
        &self,
        thread_id: &str,
        create: bool,
    ) -> Result<Option<SharedShellManager>> {
        let thread = self.get_thread(thread_id).await?;
        let (prefer_bwrap, bwrap_extensions, denied_read_paths) = {
            let cfg = self.read_config();
            (
                cfg.prefers_bwrap(),
                codewhale_sandbox::BwrapMountExtensions {
                    read_only_roots: cfg.bwrap_ro_roots.clone(),
                    device_roots: cfg.bwrap_dev_roots.clone(),
                },
                codewhale_sandbox::read_guard::ReadDenylist::from_config(&cfg).subtree_paths(),
            )
        };
        let mut active = self.active.lock().await;
        let manager = match active.shell_managers.entry(thread_id.to_string()) {
            std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().clone()),
            std::collections::hash_map::Entry::Vacant(entry) => create.then(|| {
                let manager = new_shared_shell_manager(thread.workspace.clone());
                if let Ok(mut guard) = manager.lock() {
                    guard.set_prefer_bwrap(prefer_bwrap);
                    guard.set_bwrap_extensions(bwrap_extensions);
                    guard.set_denied_read_subpaths(denied_read_paths);
                }
                entry.insert(manager).clone()
            }),
        };
        drop(active);
        if let Some(manager) = &manager
            && let Ok(mut guard) = manager.lock()
        {
            guard.set_default_workspace(thread.workspace.clone());
        }
        Ok(manager)
    }

    /// Every live (thread_id, manager) pair, for the flat `GET /v1/jobs`.
    pub async fn shell_managers_snapshot(&self) -> Vec<(String, SharedShellManager)> {
        self.active
            .lock()
            .await
            .shell_managers
            .iter()
            .map(|(thread_id, manager)| (thread_id.clone(), manager.clone()))
            .collect()
    }

    /// A thread opt-in cannot override an externally controlled denial.
    /// Root config is the user's default, so an explicit conversation opt-in
    /// may override it. ProjectConfig is returned only for an explicit local
    /// false, even when the host's merged Config was loaded in another folder.
    /// Other external values come from loaded Config: this does not reload
    /// changed files or revoke already-running shell jobs.
    pub async fn validate_shell_access_policy(
        &self,
        workspace: &Path,
        config_path: Option<&Path>,
        config_profile: Option<&str>,
    ) -> Result<()> {
        use codewhale_runtime::config::ShellAccessControl;
        let config = self.read_config().clone();
        let workspace = workspace.to_path_buf();
        let config_path = config_path.map(Path::to_path_buf);
        let config_profile = config_profile.map(str::to_owned);
        #[cfg(any(test, feature = "test-support"))]
        let env_ticket = crate::test_support::env_scope_ticket();
        tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            let control = config.allow_shell_control(
                config_path.as_deref(),
                config_profile.as_deref(),
                &workspace,
            );
            let denied = match control {
                ShellAccessControl::Unset | ShellAccessControl::RootConfig => false,
                ShellAccessControl::ProjectConfig | ShellAccessControl::Ambiguous => true,
                ShellAccessControl::Profile
                | ShellAccessControl::Environment
                | ShellAccessControl::ManagedConfig => !config.allow_shell(),
            };
            if denied {
                bail!("shell commands are restricted by {}", control.label());
            }
            Ok(())
        })
        .await
        .context("shell policy inspection worker failed")?
    }

    /// The sandbox policy an API-created job inherits — the same posture
    /// projection a turn applies to its shell calls, so a client terminal
    /// cannot run looser than the thread's own tools would.
    pub async fn thread_job_sandbox_policy(
        &self,
        thread: &ThreadRecord,
    ) -> codewhale_sandbox::SandboxPolicy {
        let policy = RuntimePolicyProjection::from_persisted(
            &thread.mode,
            thread.permission_posture.as_deref(),
            thread.auto_approve,
        );
        let authority = crate::core::authority::TurnAuthority::from_effective_fields(
            policy.mode,
            thread.allow_shell,
            thread.trust_mode,
            policy.auto_approve(),
            policy.permission,
        );
        let config = self.read_config();
        authority.sandbox_policy(
            &thread.workspace,
            config.sandbox_mode.as_deref(),
            crate::core::authority::SandboxNetworkAccess::from_config(
                config.sandbox_network_access,
            ),
        )
    }

    pub(super) fn restore_thread_messages(&self, thread: &ThreadRecord) -> Result<Vec<Message>> {
        let turns = self.store.list_turns_for_thread(&thread.id)?;
        let (messages, covered) = match self.saved_session_prefix(thread, &turns) {
            Ok(prefix) => prefix.unwrap_or_default(),
            Err(error) => {
                let Some(stale) = error.downcast_ref::<StaleSessionBinding>() else {
                    return Err(error);
                };
                // The binding describes no readable document. The thread's
                // own turns are its history; drop the dead link (keeping it
                // in a receipt) instead of stranding the thread (#6144).
                self.unbind_stale_session(thread, &stale.reason, false)?;
                (Vec::new(), 0)
            }
        };
        let items_by_turn = self.prepared_items_for_turns(&turns[covered..])?;
        self.store
            .history_with_prefix(&turns, &items_by_turn, messages, covered)
    }

    /// Drop `thread`'s saved-session binding when the stored record still
    /// carries exactly the binding that was found stale (and, with
    /// `pin_latest_turn`, the same latest turn), and record the old binding in
    /// the session reconcile receipts. Returns whether the binding was dropped.
    pub(super) fn unbind_stale_session(
        &self,
        thread: &ThreadRecord,
        reason: &str,
        pin_latest_turn: bool,
    ) -> Result<bool> {
        let _thread_mutation = self.store.thread_mutation.lock();
        let mut stored = self.store.load_thread(&thread.id)?;
        if stored.session_id != thread.session_id
            || stored.saved_session_checkpoint != thread.saved_session_checkpoint
            || (pin_latest_turn && stored.latest_turn_id != thread.latest_turn_id)
        {
            return Ok(false);
        }
        crate::session_reconcile::record_thread_unbound(
            &self.session_store_binding().data_dir,
            &stored,
            reason,
        );
        stored.session_id = None;
        stored.saved_session_checkpoint = None;
        stored.updated_at = Utc::now();
        self.store.save_thread(&stored)?;
        Ok(true)
    }

    /// Release a canonical holder that holds nothing its saved document lacks,
    /// using the same compare-and-swap unbind and reconcile receipt as a stale
    /// binding found while restoring messages. Fails when the stored record
    /// changed since `thread` was read.
    pub fn release_stale_session_holder(&self, thread: &ThreadRecord, reason: &str) -> Result<()> {
        anyhow::ensure!(
            self.unbind_stale_session(thread, reason, true)?,
            "saved holder changed during admission; retry"
        );
        Ok(())
    }

    /// Unbind every thread in this store that names `session_id` (#6144).
    /// Called when that document is deleted: the threads keep their turns and
    /// hydrate from them instead of failing with "Cannot read saved session".
    pub fn unbind_session_threads(&self, session_id: &str) -> Result<usize> {
        unbind_session_threads_in_store(
            &self.store,
            &self.session_store_binding().data_dir,
            session_id,
            "the session document was deleted",
        )
    }

    /// Give a legacy `session_id`-only link the checkpoint its projection
    /// match just established. Best effort, like [`Self::record_checkpoint_len`].
    pub(super) fn record_legacy_checkpoint(
        &self,
        thread: &ThreadRecord,
        messages: &[Message],
        turns: &[TurnRecord],
        covered: usize,
    ) {
        let Ok(messages_sha256) = session_messages_sha256(messages) else {
            return;
        };
        let _thread_mutation = self.store.thread_mutation.lock();
        let Ok(mut stored) = self.store.load_thread(&thread.id) else {
            return;
        };
        if stored.session_id != thread.session_id || stored.saved_session_checkpoint.is_some() {
            return;
        }
        stored.saved_session_checkpoint = Some(SavedSessionCheckpoint {
            covered_turn_id: covered
                .checked_sub(1)
                .and_then(|index| turns.get(index))
                .map(|turn| turn.id.clone()),
            messages_sha256,
            messages_len: Some(messages.len()),
            retained_messages: None,
        });
        if let Err(error) = self.store.save_thread(&stored) {
            tracing::debug!(thread_id = %thread.id, %error, "migrated legacy checkpoint was not saved");
        }
    }

    /// Record a migrated legacy checkpoint's prefix length, so the prefix
    /// search runs once. Best effort: a failed write only repeats the search.
    pub(super) fn record_checkpoint_len(&self, thread: &ThreadRecord, len: usize) {
        let _thread_mutation = self.store.thread_mutation.lock();
        let Ok(mut stored) = self.store.load_thread(&thread.id) else {
            return;
        };
        if stored.session_id != thread.session_id
            || stored.saved_session_checkpoint != thread.saved_session_checkpoint
        {
            return;
        }
        if let Some(checkpoint) = stored.saved_session_checkpoint.as_mut() {
            checkpoint.messages_len = Some(len);
        }
        if let Err(error) = self.store.save_thread(&stored) {
            tracing::debug!(thread_id = %thread.id, %error, "migrated checkpoint length was not saved");
        }
    }

    pub(super) fn saved_session_prefix(
        &self,
        thread: &ThreadRecord,
        turns: &[TurnRecord],
    ) -> Result<Option<(Vec<Message>, usize)>> {
        let Some(session_id) = thread.session_id.as_deref() else {
            return Ok(None);
        };
        let loaded = crate::session_manager::SessionManager::new(self.sessions_dir().to_path_buf())
            .and_then(|manager| {
                manager
                    .resume_session(session_id)
                    .map(|recovery| recovery.session)
            });
        let session = match loaded {
            Ok(session) => session,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StaleSessionBinding {
                    reason: format!("saved session {session_id} no longer exists"),
                }
                .into());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot read saved session {session_id}; restore that session file before resuming thread {}", thread.id));
            }
        };
        self.saved_session_prefix_from_session(thread, turns, session)
            .map(Some)
    }

    pub(super) fn saved_session_prefix_from_session(
        &self,
        thread: &ThreadRecord,
        turns: &[TurnRecord],
        mut session: crate::session_manager::SavedSession,
    ) -> Result<(Vec<Message>, usize)> {
        let session_id = session.metadata.id.clone();
        let covered = if let Some(checkpoint) = &thread.saved_session_checkpoint {
            let Some(len) = checkpoint_prefix_len(checkpoint, &session.messages)? else {
                return Err(StaleSessionBinding {
                    reason: format!(
                        "Saved session {session_id} changed after this thread's checkpoint: its first messages are no longer the ones this thread was bound to"
                    ),
                }
                .into());
            };
            if checkpoint.messages_len.is_none() {
                self.record_checkpoint_len(thread, len);
            }
            // Messages past the prefix were appended to the document by its
            // own conversation after this thread's checkpoint; the thread's
            // history is the prefix plus its own later turns.
            session.messages.truncate(len);
            match checkpoint.covered_turn_id.as_deref() {
                Some(id) => turns.iter().position(|turn| turn.id == id)
                    .map(|index| index + 1)
                    .with_context(|| format!("Saved session checkpoint turn {id} is missing; restore the Runtime turn records before resuming"))?,
                None => 0,
            }
        } else {
            // Legacy links have no cursor. Establish it only from an exact
            // match to the existing seeder's projection; never use mtime or
            // silently let a saved file replace a newer Runtime transcript.
            let expected = session_recovery_projection(&session.messages);
            // One read for every turn, not one per turn: the walk below
            // rebuilds each turn's messages as it compares prefixes, and
            // `list_items_for_turn` scans the store's whole items directory
            // per call.
            let items_by_turn = self.prepared_items_for_turns(turns)?;
            let mut prefix = Vec::new();
            let mut covered = (expected.is_empty()).then_some(0);
            for (index, turn) in turns.iter().enumerate() {
                if covered.is_some() {
                    break;
                }
                prefix.extend(session_recovery_projection(
                    &reconstruct_messages_from_turns_with(
                        std::slice::from_ref(turn),
                        &items_by_turn,
                    )?,
                ));
                if prefix == expected {
                    covered = Some(index + 1);
                } else if !expected.starts_with(&prefix) {
                    break;
                }
            }
            match covered {
                Some(covered) => {
                    // Migrate the legacy link to a prefix checkpoint so this
                    // projection walk runs once, not on every load.
                    self.record_legacy_checkpoint(thread, &session.messages, turns, covered);
                    covered
                }
                None => {
                    return Err(StaleSessionBinding {
                        reason: format!("Saved session {session_id} has no verifiable Runtime checkpoint: no prefix of this thread's turns matches it"),
                    }
                    .into());
                }
            }
        };
        let mut messages = session.messages;
        if let Some(retained) = thread
            .saved_session_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.retained_messages)
        {
            if retained > messages.len() {
                bail!(
                    "Saved session checkpoint exceeds its transcript; restore the saved session before resuming"
                );
            }
            messages.truncate(retained);
        }
        Ok((messages, covered))
    }

    #[cfg(test)]
    pub(super) fn reconstruct_messages_from_turns(
        &self,
        turns: &[TurnRecord],
    ) -> Result<Vec<Message>> {
        // One batch read for the whole set. `list_items_for_turn` scans the
        // store's entire items directory to answer for a single turn, so a
        // caller with several turns — the fork alignment below, a legacy
        // saved-session link — paid one scan per turn.
        let items_by_turn = self.prepared_items_for_turns(turns)?;
        reconstruct_messages_from_turns_with(turns, &items_by_turn)
    }

    pub fn history_from_records(
        &self,
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
    ) -> Result<Vec<Message>> {
        self.store.history_from_records(turns, items_by_turn)
    }

    pub(super) fn append_routed_usage_to_turn(
        &self,
        turn_id: &str,
        source_id: &str,
        usage: EffectiveRouteUsage,
    ) -> Result<()> {
        let _turn_mutation = self.store.turn_mutation.lock();
        let mut turn = self.store.load_turn(turn_id)?;
        if append_routed_usage_record(&mut turn, source_id, usage) {
            self.store.save_turn(&turn)?;
        }
        Ok(())
    }

    pub(super) fn register_runtime_usage_sink(&self, turn_id: &str) {
        let store = self.store.clone();
        let sink_turn_id = turn_id.to_string();
        let drop_store = self.store.clone();
        let drop_turn_id = turn_id.to_string();
        crate::cost_status::register_runtime_usage_sink_with_drop(
            turn_id,
            Arc::new(move |record: RuntimeUsageRecord| {
                let _turn_mutation = store.turn_mutation.lock();
                let Ok(mut turn) = store.load_turn(&sink_turn_id) else {
                    return false;
                };
                if !append_routed_usage_record(&mut turn, &record.source_id, record.usage) {
                    return true;
                }
                store.save_turn(&turn).is_ok()
            }),
            Some(Arc::new(move |record: RuntimeUsageDropRecord| {
                let _turn_mutation = drop_store.turn_mutation.lock();
                let Ok(mut turn) = drop_store.load_turn(&drop_turn_id) else {
                    return false;
                };
                if !append_routed_usage_drop_record(&mut turn, record) {
                    return true;
                }
                drop_store.save_turn(&turn).is_ok()
            })),
        );
        let decision_store = self.store.clone();
        let decision_turn_id = turn_id.to_string();
        crate::cost_status::register_runtime_decision_sink(
            turn_id,
            Arc::new(move |receipt| {
                let _turn_mutation = decision_store.turn_mutation.lock();
                let Ok(mut turn) = decision_store.load_turn(&decision_turn_id) else {
                    return false;
                };
                if !append_decision_receipt(&mut turn, &receipt) {
                    return true;
                }
                decision_store.save_turn(&turn).is_ok()
            }),
        );
    }

    /// Persist an engine status line as a completed `status` item.
    ///
    /// Model-facing hints (deferred-tool retry) already reach the model in the
    /// tool result; they are not user items. Scheduler, continuation and
    /// approval-wait rows keep a receipt tagged so clients collapse them.
    /// An approval-wait heartbeat naming a call in `settled_approval_calls`
    /// is stale (its approval was already answered) and is dropped, whichever
    /// path dequeued it.
    pub(super) async fn publish_status_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        message: String,
        settled_approval_calls: &HashSet<String>,
    ) -> Result<()> {
        if crate::core::events::approval_wait_tool_call(&message)
            .is_some_and(|call| settled_approval_calls.contains(call))
        {
            return Ok(());
        }
        let visibility = crate::core::events::status_visibility(&message);
        if visibility == crate::core::events::StatusVisibility::ModelOnly {
            return Ok(());
        }
        let item = TurnItemRecord {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            id: runtime_record_id("item"),
            turn_id: turn_id.to_string(),
            kind: TurnItemKind::Status,
            status: TurnItemLifecycleStatus::Completed,
            summary: summarize_text(&message, SUMMARY_LIMIT),
            detail: Some(message),
            metadata: (visibility == crate::core::events::StatusVisibility::Internal)
                .then(|| json!({ "visibility": visibility.as_str() })),
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(Utc::now()),
            ended_at: Some(Utc::now()),
        };
        self.store.save_item(&item)?;
        self.attach_item_to_turn(turn_id, &item.id)?;
        self.emit_event(
            thread_id,
            Some(turn_id),
            Some(&item.id),
            "item.completed",
            json!({ "item": item }),
        )
        .await?;
        Ok(())
    }

    async fn persist_runtime_checkpoint(
        &self,
        thread_id: &str,
        turn_id: &str,
        engine: &EngineHandle,
        engine_turn_id: Option<&str>,
        request: Arc<crate::core::events::SessionCheckpointRequest>,
        receipt_secrets: &[String],
    ) -> Result<crate::core::events::SessionCheckpointReceipt> {
        let thread = self.store.load_thread(thread_id)?;
        request
            .validate_prefix(thread_id, &thread.workspace, &request.snapshot.messages)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            request.caller.agent_id.is_none()
                && engine_turn_id == Some(request.caller.turn_id.as_str())
                && !request.is_stale(),
            "Runtime checkpoint caller is not the active root turn"
        );
        {
            let active = self.active.lock().await;
            anyhow::ensure!(
                active.engines.get(thread_id).is_some_and(|state| {
                    Arc::ptr_eq(&state.engine.rx_event, &engine.rx_event)
                        && state.active_turn.as_ref().is_some_and(|turn| {
                            turn.turn_id == turn_id && !turn.interrupt_requested
                        })
                }),
                "Runtime checkpoint active owner changed"
            );
        }
        let tool_declarations = checkpoint_tool_declarations(&request.snapshot.messages)?;
        let private_results: HashSet<_> = request
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    execution_id,
                    id,
                    name,
                    ..
                } if name == REQUEST_USER_INPUT_TOOL_NAME => {
                    Some((execution_id.clone(), id.clone()))
                }
                _ => None,
            })
            .collect();
        let settled_calls: HashSet<_> = request
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    execution_id,
                    tool_use_id,
                    ..
                } => Some((execution_id.clone(), tool_use_id.clone())),
                _ => None,
            })
            .collect();
        let mut messages = request.snapshot.messages.clone();
        for message in &mut messages {
            crate::core::engine::redact_tool_blocks(message, |text| {
                crate::client::redact_model_bound_text(text, receipt_secrets)
            });
            for block in &mut message.content {
                if let ContentBlock::ToolResult {
                    execution_id,
                    tool_use_id,
                    content,
                    content_blocks,
                    ..
                } = block
                    && private_results.contains(&(execution_id.clone(), tool_use_id.clone()))
                {
                    *content = REDACTED_USER_INPUT_RECEIPT.to_string();
                    *content_blocks = None;
                }
            }
        }
        let store = self.store.clone();
        let owner = self._process_owner_lock.clone();
        let owner_path = self
            .session_store_binding()
            .data_dir
            .join(RUNTIME_PROCESS_OWNER_LOCK_FILE);
        let turn_id_owned = turn_id.to_owned();
        let pending = request.clone();
        let receipt = tokio::task::spawn_blocking(
            move || -> Result<crate::core::events::SessionCheckpointReceipt> {
                anyhow::ensure!(
                    !pending.is_stale() && owner_lock_is_at_path(&owner._file, &owner_path)?,
                    "Runtime checkpoint owner lease expired"
                );
                let _mutation = store.turn_mutation.lock();
                let mut turn = store.load_turn(&turn_id_owned)?;
                anyhow::ensure!(
                    turn.thread_id == pending.caller.session_id && turn.status.is_active_work(),
                    "Runtime checkpoint turn is no longer active"
                );
                let items_by_turn =
                    store.list_items_for_turns_map(std::slice::from_ref(&turn.id))?;
                let items = ordered_turn_items(&turn, &items_by_turn)?;
                anyhow::ensure!(
                    items.iter().map(|item| &item.id).eq(turn.item_ids.iter()),
                    "Runtime checkpoint item cursor is incomplete"
                );
                let mut covered_items = HashMap::new();
                let mut pending_items = Vec::new();
                for item in &items {
                    if projects_tool_history(item)
                        && let Some(identity) =
                            RuntimeToolIdentity::read(item.metadata.as_ref(), "tool_result_for")?
                        && settled_calls.contains(&(
                            identity.execution_id.map(str::to_owned),
                            identity.provider_id.to_owned(),
                        ))
                    {
                        covered_items.insert(item.id.clone(), checkpoint_item_digest(item)?);
                    }
                    if matches!(item.kind, TurnItemKind::UserMessage)
                        && matches!(
                            item.status,
                            TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::Canceled
                        )
                    {
                        pending_items.push(item.id.clone());
                    }
                    if !projects_tool_history(item)
                        && matches!(
                            item.kind,
                            TurnItemKind::UserMessage
                                | TurnItemKind::AgentMessage
                                | TurnItemKind::AgentReasoning
                        )
                        && !matches!(
                            item.status,
                            TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::Canceled
                        )
                    {
                        covered_items.insert(item.id.clone(), checkpoint_item_digest(item)?);
                    }
                }
                let digest = session_messages_sha256(&messages)?;
                turn.schema_version = turn
                    .schema_version
                    .max(PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION);
                turn.prefix_checkpoint = Some(RuntimePrefixCheckpoint {
                    checkpoint_id: pending.checkpoint_id.clone(),
                    session_id: pending.caller.session_id.clone(),
                    agent_id: pending.caller.agent_id.clone(),
                    runtime_turn_id: turn.id.clone(),
                    engine_turn_id: pending.caller.turn_id.clone(),
                    call_id: pending.caller.call_id.clone(),
                    owner_id: store.owner_id.clone(),
                    source_sha256: pending.prefix_sha256.clone(),
                    persisted_sha256: digest.clone(),
                    messages_len: messages.len(),
                    projection: "runtime_receipt_redacted".to_string(),
                    item_ids: turn.item_ids.clone(),
                    covered_items,
                    pending_items,
                    tool_declarations,
                });
                store.checkpoint_tail(&turn, &items_by_turn, &messages)?;
                let reference = store.save_settled_history(&messages)?;
                anyhow::ensure!(
                    reference["sha256"].as_str() == Some(digest.as_str()),
                    "Runtime checkpoint stored projection changed"
                );
                anyhow::ensure!(
                    !pending.is_stale() && owner_lock_is_at_path(&owner._file, &owner_path)?,
                    "Runtime checkpoint was withdrawn before publication"
                );
                store.save_turn(&turn)?;
                let published = store.load_turn(&turn.id)?;
                let checkpoint = published
                    .prefix_checkpoint
                    .as_ref()
                    .context("Runtime checkpoint publication was lost")?;
                anyhow::ensure!(
                    checkpoint.checkpoint_id == pending.checkpoint_id
                        && store.load_prefix_history(checkpoint)? == messages,
                    "Runtime checkpoint publication could not be verified"
                );
                Ok(crate::core::events::SessionCheckpointReceipt {
                    persisted_sha256: digest,
                    messages_len: messages.len(),
                    projection:
                        crate::core::events::SessionCheckpointProjection::RuntimeReceiptRedacted,
                })
            },
        )
        .await
        .context("Runtime checkpoint writer stopped; persistence is uncertain")??;
        let active = self.active.lock().await;
        anyhow::ensure!(
            !request.is_stale()
                && active.engines.get(thread_id).is_some_and(|state| {
                    Arc::ptr_eq(&state.engine.rx_event, &engine.rx_event)
                        && state.active_turn.as_ref().is_some_and(|turn| {
                            turn.turn_id == turn_id && !turn.interrupt_requested
                        })
                }),
            "Runtime checkpoint owner changed after writing; persistence is uncertain"
        );
        Ok(receipt)
    }

    pub(super) async fn monitor_turn(
        &self,
        thread_id: String,
        turn_id: String,
        engine: EngineHandle,
        narrowing: crate::core::engine::TurnNarrowing,
        structured_review: bool,
    ) -> Result<()> {
        let mut current_message_item: Option<TurnItemRecord> = None;
        let mut current_reasoning_item: Option<TurnItemRecord> = None;
        let mut tool_items: HashMap<String, String> = HashMap::new();
        let mut compaction_items: HashMap<String, String> = HashMap::new();
        let mut completed_compaction_item: Option<String> = None;
        let mut terminal_received = false;
        let mut turn_usage: Option<Usage> = None;
        let mut turn_effective_route_usage: Option<Usage> = None;
        let mut turn_routed_usage_dropped_records = 0_u64;
        // The engine emits an initial request snapshot before connection setup
        // and one terminal snapshot after it knows the outcome. Keep only the
        // latter; a prepared request is not evidence of delivery or a model
        // call, and the terminal counters intentionally exclude HTTP retries.
        let mut turn_model_request_diagnostics: Option<RuntimeTurnRequestDiagnostics> = None;
        let mut turn_status: Option<RuntimeTurnStatus> = None;
        let mut turn_error: Option<String> = None;
        let mut saw_engine_activity = false;
        let mut saw_turn_started = false;
        let mut engine_turn_id: Option<String> = None;
        let mut dispatched_route: Option<EffectiveRouteEnvelope> = None;
        let mut pending_events: VecDeque<EngineEvent> = VecDeque::new();
        let mut event_channel_closed = false;
        // Raw tool call IDs whose external approval this turn already settled.
        // An approval-wait heartbeat naming one of them is stale by the time
        // it is dequeued and must not be published as a live claim.
        let mut settled_approval_calls: HashSet<String> = HashSet::new();
        // Latest engine-side goal snapshot observed during this turn. The
        // model's `update_goal` decision (complete/blocked/paused) lands here
        // before TurnComplete, so terminal settlement can mirror it into the
        // durable goal record instead of continuing to spend.
        let (mut admitted_goal_id, thread_hooks, admitted_acp) = {
            let active = self.active.lock().await;
            let state = active.engines.get(&thread_id);
            let admitted = state
                .and_then(|state| state.active_turn.as_ref())
                .filter(|turn| turn.turn_id == turn_id);
            anyhow::ensure!(
                admitted.is_none_or(|turn| turn.narrowing == narrowing),
                "claimed turn narrowing changed before monitoring"
            );
            (
                admitted.and_then(|turn| turn.goal_id.clone()),
                state.and_then(|state| state.hook_executor.clone()),
                // Captured by the no-await admission and retained by the
                // lifecycle task. Later active-state changes cannot reinterpret
                // an ACP result as ordinary work or restart its goal loop.
                self.host_profile.is_acp() || narrowing.is_acp(),
            )
        };
        // Lifecycle scripts belong to the original ordinary admission; a
        // narrowed ACP request cannot run an existing Native hook graph.
        let thread_hooks = if admitted_acp || structured_review {
            None
        } else {
            thread_hooks
        };
        // Runtime receipts always mask credentials, including when verbatim
        // model-bound tool output was explicitly enabled. Resolve the active
        // thread's key off the runtime and reuse the client's exact-value list.
        let mut receipt_config = self.read_config().clone();
        if let Some(state) = self.active.lock().await.engines.get(&thread_id) {
            receipt_config
                .scope_to_provider_identity(&state.route_identity)
                .map_err(anyhow::Error::msg)?;
        }
        let receipt_secrets = receipt_secret_values(receipt_config).await?;
        let mut latest_goal_snapshot: Option<crate::tools::goal::GoalSnapshot> = None;
        // Tool definitions of the finished turn's request surface, from the
        // final TurnComplete receipt. Goal settlement uses it to mirror the
        // engine's own `update_goal` precondition for continuation.
        let mut turn_tool_catalog: Option<Vec<codewhale_core::request::Tool>> = None;
        // Every file path in a tool receipt is confined to the thread
        // workspace.
        let artifact_workspace = self
            .store
            .load_thread(&thread_id)
            .map(|thread| thread.workspace)
            .unwrap_or_else(|_| self.workspace.clone());
        let artifact_workspace_roots = {
            let mut roots = vec![artifact_workspace.clone()];
            if let Ok(canonical) = tokio::fs::canonicalize(&artifact_workspace).await
                && canonical != artifact_workspace
            {
                roots.push(canonical);
            }
            roots
        };
        // The `tool` restore point recorded on this turn for each call, by
        // call id: a tool artifact names one only when this thread owns it
        // (#6621), so file-revert accepts exactly what the ref advertises.
        let mut tool_restore_points: HashMap<String, String> = HashMap::new();

        loop {
            let event = if let Some(event) = pending_events.pop_front() {
                Some(event)
            } else if event_channel_closed {
                None
            } else {
                let mut rx = engine.rx_event.write().await;
                rx.recv().await
            };
            let Some(event) = event else {
                if self
                    .is_interrupt_requested(&thread_id, &turn_id)
                    .await
                    .unwrap_or(false)
                {
                    turn_status = Some(RuntimeTurnStatus::Interrupted);
                    break;
                }
                bail!("engine event channel closed before turn {turn_id} completed");
            };

            if let EngineEvent::UserInputClosed { id, outcome } = &event {
                self.user_input_settlement(&thread_id)
                    .close(id, Some(&turn_id), *outcome)
                    .await?;
                continue;
            }

            if let EngineEvent::SessionUpdated {
                checkpoint: Some(checkpoint),
                ..
            } = &event
            {
                match self
                    .persist_runtime_checkpoint(
                        &thread_id,
                        &turn_id,
                        &engine,
                        engine_turn_id.as_deref(),
                        checkpoint.clone(),
                        &receipt_secrets,
                    )
                    .await
                {
                    Ok(receipt) => checkpoint.succeed(receipt),
                    Err(error) => checkpoint.fail(format!("Runtime checkpoint failed: {error:#}")),
                }
                continue;
            }

            // SyncSession and configuration operations emit control status
            // receipts on the same channel before SendMessage is processed.
            // They belong to engine setup, not to the next claimed turn.
            if !saw_turn_started
                && matches!(
                    &event,
                    EngineEvent::Status { .. }
                        | EngineEvent::McpSessionBoot { .. }
                        | EngineEvent::SessionUpdated { .. }
                        | EngineEvent::AgentList { .. }
                        | EngineEvent::AgentSpawned { .. }
                        | EngineEvent::AgentProgress { .. }
                        | EngineEvent::AgentComplete { .. }
                        | EngineEvent::SubAgentMailbox { .. }
                )
            {
                continue;
            }

            // Engine configuration and session synchronization can emit
            // Status/SessionUpdated events before a turn is claimed. Those
            // control-plane receipts share the engine channel, but they are
            // not model output and must not make an otherwise empty turn look
            // successful. Count only events that carry turn-scoped work or
            // user-visible output.
            if matches!(
                &event,
                EngineEvent::MessageStarted { .. }
                    | EngineEvent::MessageDelta { .. }
                    | EngineEvent::MessageComplete { .. }
                    | EngineEvent::ThinkingStarted { .. }
                    | EngineEvent::ThinkingDelta { .. }
                    | EngineEvent::ThinkingComplete { .. }
                    | EngineEvent::ToolCallStarted { .. }
                    | EngineEvent::ToolCallComplete { .. }
                    | EngineEvent::CompactionStarted { .. }
                    | EngineEvent::CompactionCompleted { .. }
                    | EngineEvent::CompactionCancelled { .. }
                    | EngineEvent::CompactionFailed { .. }
                    | EngineEvent::AgentSpawned { .. }
                    | EngineEvent::AgentProgress { .. }
                    | EngineEvent::AgentComplete { .. }
                    | EngineEvent::SubAgentMailbox { .. }
                    | EngineEvent::ApprovalRequired { .. }
                    | EngineEvent::ElevationRequired { .. }
                    | EngineEvent::UserInputRequired { .. }
                    | EngineEvent::Error { .. }
            ) {
                saw_engine_activity = true;
            }

            match event {
                EngineEvent::TurnStarted {
                    turn_id: started_turn_id,
                    created_at,
                    route,
                    submission_id: _,
                } => {
                    saw_turn_started = true;
                    engine_turn_id = Some(started_turn_id);
                    let started_route = route
                        .as_ref()
                        .and_then(crate::core::events::TurnRoute::cost_envelope);
                    {
                        let _turn_mutation = self.store.turn_mutation.lock();
                        let mut turn = self.store.load_turn(&turn_id)?;
                        // Load-under-lock sees any concurrent terminal settle;
                        // field updates below preserve that status.
                        turn.started_at = Some(created_at);
                        // A lifecycle start carries no billing envelope, so
                        // there is nothing to persist yet. The dispatch event
                        // below is the only writer of effective-route columns.
                        if let Some(route) = started_route.as_ref() {
                            turn.persist_effective_route(route);
                        }
                        self.store.save_turn(&turn)?;
                    }
                    if started_route.is_some() {
                        dispatched_route = started_route;
                    }
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        None,
                        "turn.lifecycle",
                        json!({ "status": "in_progress" }),
                    )
                    .await?;
                }
                EngineEvent::RouteDispatched {
                    turn_id: dispatched_turn_id,
                    route,
                } => {
                    if engine_turn_id
                        .as_deref()
                        .is_some_and(|started| started == dispatched_turn_id)
                    {
                        let envelope = route.cost_envelope();
                        {
                            let _turn_mutation = self.store.turn_mutation.lock();
                            let mut turn = self.store.load_turn(&turn_id)?;
                            // Preserve whatever status the store already has (including
                            // a concurrent terminal settle); only refresh route fields.
                            if let Some(envelope) = envelope.as_ref() {
                                turn.persist_effective_route(envelope);
                            }
                            self.store.save_turn(&turn)?;
                        }
                        if envelope.is_some() {
                            dispatched_route = envelope;
                        }
                    }
                }
                EngineEvent::MessageStarted { .. } => {
                    // A new assistant text block starts a new item below, and
                    // nothing can append to the previous one any more (its
                    // `ContentBlockStop` was what let this block start). Older
                    // builds overwrote the pointer, so a message whose stop
                    // frame was lost to a stream retry stayed `in_progress`
                    // forever in a turn that completed.
                    self.close_streaming_item(
                        &thread_id,
                        &turn_id,
                        &mut current_message_item,
                        TurnItemLifecycleStatus::Completed,
                    )
                    .await?;
                    let item_id = runtime_record_id("item");
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: item_id.clone(),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::AgentMessage,
                        status: TurnItemLifecycleStatus::InProgress,
                        summary: String::new(),
                        detail: Some(String::new()),
                        metadata: None,
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: None,
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item_id),
                        "item.started",
                        json!({ "item": item.clone() }),
                    )
                    .await?;
                    current_message_item = Some(item);
                }
                EngineEvent::MessageDelta { content, .. } => {
                    let content = if pending_events.is_empty() {
                        let batch =
                            coalesce_stream_delta(&engine, StreamDeltaKind::Message, content).await;
                        pending_events.extend(batch.pending_event);
                        event_channel_closed |= batch.channel_closed;
                        batch.content
                    } else {
                        content
                    };
                    if let Some(item) = current_message_item.as_mut() {
                        let text = item.detail.get_or_insert_default();
                        text.push_str(&content);
                        // Materialize the prefix before sequencing its delta.
                        // A snapshot whose cursor includes this event must not
                        // still observe the empty item saved at MessageStarted,
                        // and restart recovery must retain the partial output.
                        item.summary = summarize_text(text, SUMMARY_LIMIT);
                        let projection_lock = self.projection_lock(&thread_id);
                        let _projection = projection_lock.lock().await;
                        self.save_streaming_item(item).await?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item.id),
                            "item.delta",
                            json!({ "delta": content, "kind": "agent_message" }),
                        )
                        .await?;
                    }
                }
                EngineEvent::MessageComplete { .. } => {
                    if let Some(mut item) = current_message_item.take() {
                        item.status = TurnItemLifecycleStatus::Completed;
                        item.summary = summarize_text(
                            item.detail.as_deref().unwrap_or_default(),
                            SUMMARY_LIMIT,
                        );
                        item.ended_at = Some(Utc::now());
                        self.save_streaming_item(&item).await?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item.id),
                            "item.completed",
                            json!({ "item": item }),
                        )
                        .await?;
                    }
                }
                EngineEvent::ThinkingStarted { .. } => {
                    // The engine parks a thinking block that never saw its stop
                    // frame the moment the next one begins
                    // (`ContentBlockStart::Thinking` → `flush_thinking_block`).
                    // A stream retry — system sleep, a mid-stream network drop
                    // — is exactly that case: it discards the fragment and
                    // re-issues the request without a `ThinkingComplete`. Close
                    // the superseded item here too, or it stays `in_progress`
                    // inside a turn that goes on to complete.
                    self.close_streaming_item(
                        &thread_id,
                        &turn_id,
                        &mut current_reasoning_item,
                        TurnItemLifecycleStatus::Completed,
                    )
                    .await?;
                    let item_id = runtime_record_id("item");
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: item_id.clone(),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::AgentReasoning,
                        status: TurnItemLifecycleStatus::InProgress,
                        summary: String::new(),
                        detail: Some(String::new()),
                        metadata: None,
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: None,
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item_id),
                        "item.started",
                        json!({ "item": item.clone() }),
                    )
                    .await?;
                    current_reasoning_item = Some(item);
                }
                EngineEvent::ThinkingDelta { content, .. } => {
                    let content = if pending_events.is_empty() {
                        let batch =
                            coalesce_stream_delta(&engine, StreamDeltaKind::Reasoning, content)
                                .await;
                        pending_events.extend(batch.pending_event);
                        event_channel_closed |= batch.channel_closed;
                        batch.content
                    } else {
                        content
                    };
                    if let Some(item) = current_reasoning_item.as_mut() {
                        let text = item.detail.get_or_insert_default();
                        text.push_str(&content);
                        item.summary = summarize_text(text, SUMMARY_LIMIT);
                        let projection_lock = self.projection_lock(&thread_id);
                        let _projection = projection_lock.lock().await;
                        self.save_streaming_item(item).await?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item.id),
                            "item.delta",
                            json!({ "delta": content, "kind": "agent_reasoning" }),
                        )
                        .await?;
                    }
                }
                EngineEvent::ThinkingComplete { .. } => {
                    if let Some(mut item) = current_reasoning_item.take() {
                        item.status = TurnItemLifecycleStatus::Completed;
                        item.summary = summarize_text(
                            item.detail.as_deref().unwrap_or_default(),
                            SUMMARY_LIMIT,
                        );
                        item.ended_at = Some(Utc::now());
                        self.save_streaming_item(&item).await?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item.id),
                            "item.completed",
                            json!({ "item": item }),
                        )
                        .await?;
                    }
                }
                EngineEvent::ToolCallStarted {
                    id,
                    name,
                    input,
                    model_call,
                } => {
                    // Reasoning ends where the tool call it produced begins:
                    // a thinking block cannot run across a tool call, so any
                    // segment still open here lost its stop frame to a retried
                    // stream and would otherwise never close.
                    self.close_streaming_item(
                        &thread_id,
                        &turn_id,
                        &mut current_reasoning_item,
                        TurnItemLifecycleStatus::Completed,
                    )
                    .await?;
                    let item_id = runtime_record_id("item");
                    tool_items.insert(id.clone(), item_id.clone());
                    let kind = tool_kind_for_name(&name);
                    let summary = summarize_text(&format!("{name} started"), SUMMARY_LIMIT);
                    let input_str = serde_json::to_string(&input).unwrap_or_default();
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: item_id.clone(),
                        turn_id: turn_id.clone(),
                        kind,
                        status: TurnItemLifecycleStatus::InProgress,
                        summary,
                        detail: Some(input_str.clone()),
                        // The tool identity must live in the durable item
                        // snapshot: restart history rebuild reads it back to
                        // re-emit provider tool_calls. Without it a restart
                        // replays empty id/name/arguments shells that strict
                        // OpenAI-compatible endpoints reject (#5823).
                        metadata: Some({
                            let mut meta = json!({
                                "tool_use_id": id.clone(),
                                "tool_name": name.clone(),
                                "tool_input": input_str,
                                "execution_id": id.clone(),
                            });
                            if let Some(model_call) = model_call {
                                meta["provider_tool_use_id"] = json!(model_call.provider_id);
                                if let Some(caller) = model_call.caller {
                                    meta["tool_caller"] = json!(caller);
                                }
                                if let Some(signature) = model_call.thought_signature {
                                    meta["tool_thought_signature"] = json!(signature);
                                }
                            }
                            // Tool discovery is engine plumbing, not work the
                            // user asked for: clients collapse it by default.
                            if codewhale_tools::names::is_tool_search_tool(&name) {
                                meta["visibility"] = json!(INTERNAL_ITEM_VISIBILITY);
                            }
                            meta
                        }),
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: None,
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item_id),
                        "item.started",
                        json!({ "item": item, "tool": { "id": id, "name": name, "input": input } }),
                    )
                    .await?;
                }
                operation @ (EngineEvent::OperationActivityStarted { .. }
                | EngineEvent::OperationActivityCompleted { .. }
                | EngineEvent::ToolCallHeartbeat) => {
                    if !saw_turn_started {
                        continue;
                    }
                    if !matches!(operation, EngineEvent::ToolCallHeartbeat) {
                        saw_engine_activity = true;
                    }
                    let projection_lock = self.projection_lock(&thread_id);
                    let _projection = projection_lock.lock().await;
                    let store = self.store.clone();
                    let operation_turn_id = turn_id.clone();
                    let projected = tokio::task::spawn_blocking(move || {
                        let _turn_mutation = store.turn_mutation.lock();
                        let mut turn = store.load_turn(&operation_turn_id)?;
                        if turn.status != RuntimeTurnStatus::InProgress {
                            return Ok::<_, anyhow::Error>(None);
                        }
                        if matches!(operation, EngineEvent::ToolCallHeartbeat)
                            && turn.operation_activity.as_ref().is_none_or(|a| a.active.is_empty())
                        {
                            return Ok(None);
                        }
                        let now = Utc::now();
                        let activity = turn.operation_activity.get_or_insert_with(|| RuntimeOperationActivity {
                            observed_at: now,
                            active: Vec::new(),
                            last_completed: None,
                            overflowed: false,
                        });
                        let now = now.max(activity.observed_at);
                        activity.observed_at = now;
                        let (name, mut payload) = match operation {
                            EngineEvent::OperationActivityStarted { span_id, activity_kind, action_id } => {
                                let span_id = format!("operation:{}", codewhale_runtime::hashing::sha256_hex(span_id));
                                let action_id = action_id.filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control));
                                if !activity.active.iter().any(|span| span.span_id == span_id) {
                                    if activity.active.len() < 256 {
                                        activity.active.push(RuntimeOperationSpan {
                                            span_id: span_id.clone(), activity_kind, action_id: action_id.clone(), started_at: now,
                                        });
                                    } else {
                                        activity.overflowed = true;
                                    }
                                }
                                ("operation.activity_started", json!({"span_id":span_id,"activity_kind":activity_kind,"action_id":action_id,"observed_at":now}))
                            }
                            EngineEvent::OperationActivityCompleted { span_id, activity_kind, action_id, outcome } => {
                                let span_id = format!("operation:{}", codewhale_runtime::hashing::sha256_hex(span_id));
                                let action_id = action_id.filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control));
                                activity.active.retain(|span| span.span_id != span_id);
                                activity.last_completed = Some(RuntimeOperationCompletion {
                                    span_id: span_id.clone(), activity_kind, action_id: action_id.clone(), completed_at: now, outcome,
                                });
                                ("operation.activity_completed", json!({"span_id":span_id,"activity_kind":activity_kind,"action_id":action_id,"outcome":outcome,"observed_at":now}))
                            }
                            EngineEvent::ToolCallHeartbeat => ("operation.heartbeat", json!({"observed_at":now})),
                            _ => unreachable!(),
                        };
                        if payload.get("action_id").is_some_and(Value::is_null) {
                            payload.as_object_mut().expect("operation payload").remove("action_id");
                        }
                        store.save_turn(&turn)?;
                        Ok(Some((name, payload)))
                    }).await.context("Runtime operation projection task failed")??;
                    if let Some((name, payload)) = projected {
                        self.emit_event(&thread_id, Some(&turn_id), None, name, payload)
                            .await?;
                    }
                }
                EngineEvent::ToolExecutionStarted { id } => {
                    if let Some(item_id) = tool_items.get(&id) {
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(item_id),
                            "tool.execution_started",
                            json!({"execution_id": id}),
                        )
                        .await?;
                    }
                }
                EngineEvent::ToolResultContent { id, blocks } => {
                    if let Some(item_id) = tool_items.get(&id) {
                        let mut item = self.store.load_item(item_id)?;
                        let value = serde_json::to_value(blocks)?;
                        let value =
                            crate::client::redact_json_model_bound_text(&value, &receipt_secrets);
                        item.metadata.get_or_insert_with(|| json!({}))["acp_result_content"] =
                            value;
                        self.store.save_item(&item)?;
                    }
                }
                EngineEvent::ToolCallComplete {
                    id, name, result, ..
                } => {
                    self.settle_user_input_for_completed_tool(&thread_id, &turn_id, &id)
                        .await?;
                    if let Some(hooks) = thread_hooks.as_deref() {
                        let input = tool_items
                            .get(&id)
                            .map(|item_id| self.store.load_item(item_id))
                            .transpose()?
                            .and_then(|item| item.metadata)
                            .and_then(|meta| {
                                meta.get("tool_input")
                                    .and_then(serde_json::Value::as_str)
                                    .and_then(|text| {
                                        serde_json::from_str::<serde_json::Value>(text).ok()
                                    })
                            });
                        fire_runtime_tool_completion_hooks(
                            hooks,
                            &thread_id,
                            &turn_id,
                            &id,
                            &name,
                            input.as_ref(),
                            &result,
                        );
                    }
                    // An elevation question is over once its tool call
                    // completes, however it completed. Clear before the
                    // notify raise below: a notify call of its own settles
                    // its elevation and still raises its notice.
                    self.clear_notices_for_subject(&thread_id, &id);
                    // Model-notify projection (#6180): the notify tool has no
                    // thread context of its own, so its completion is the
                    // watchable "come back" signal.
                    if name == "notify" {
                        self.raise_notice(
                            &thread_id,
                            codewhale_config::notifications::NotificationEvent::ModelNotify
                                .as_str(),
                            &turn_id,
                            &id,
                            "model asked the user to come back".to_string(),
                        );
                    }
                    if let Ok(output) = &result
                        && let Some(metadata) = output.metadata.as_ref()
                    {
                        if let Some(batch) =
                            crate::cost_status::child_usage_records_from_metadata(metadata)
                        {
                            if !batch.decisions.is_empty() {
                                let _turn_mutation = self.store.turn_mutation.lock();
                                let mut turn = self.store.load_turn(&turn_id)?;
                                for receipt in &batch.decisions {
                                    append_decision_receipt(&mut turn, receipt);
                                }
                                self.store.save_turn(&turn)?;
                            }
                            for record in batch.records {
                                self.append_routed_usage_to_turn(
                                    &turn_id,
                                    &record.source_id,
                                    record.usage,
                                )?;
                            }
                            if !batch.drop_records.is_empty() {
                                let _turn_mutation = self.store.turn_mutation.lock();
                                let mut turn = self.store.load_turn(&turn_id)?;
                                for record in batch.drop_records {
                                    append_routed_usage_drop_record(&mut turn, record);
                                }
                                self.store.save_turn(&turn)?;
                            }
                            // Exact records share the direct sink's source
                            // ledger. Only residual/unidentified coverage is
                            // owned by TurnComplete, so it is not added here.
                        } else if let Some(route) =
                            crate::cost_status::child_route_envelope_from_metadata(metadata)
                            && let Some(usage) =
                                crate::cost_status::child_usage_from_metadata(metadata)
                        {
                            let source = format!("tool:{id}");
                            self.append_routed_usage_to_turn(
                                &turn_id,
                                &source,
                                EffectiveRouteUsage { route, usage },
                            )?;
                        }
                    }
                    if let Some(item_id) = tool_items.remove(&id) {
                        let mut item = self.store.load_item(&item_id)?;
                        let now = Utc::now();
                        item.ended_at = Some(now);
                        match result {
                            Ok(output) => {
                                item.status = if output.success {
                                    TurnItemLifecycleStatus::Completed
                                } else {
                                    TurnItemLifecycleStatus::Failed
                                };
                                let detail =
                                    tool_receipt_detail(&name, &output.content, &receipt_secrets);
                                if name == REQUEST_USER_INPUT_TOOL_NAME {
                                    // The engine must return the structured
                                    // answers to the model, but Runtime
                                    // receipts are durable and fan out to UI
                                    // clients. Persist only a machine-readable
                                    // redaction marker, never answer labels or
                                    // free-text values.
                                    item.summary = detail.clone();
                                    item.detail = Some(detail);
                                    item.metadata = Some(json!({
                                        "tool_call_id": id,
                                        "tool_name": REQUEST_USER_INPUT_TOOL_NAME,
                                        "response_redacted": true,
                                    }));
                                } else {
                                    // Durable receipt: credentials a tool
                                    // printed are masked before they reach
                                    // the item store or the event log (B1).
                                    item.summary =
                                        summarize_text(&format!("{name}: {detail}"), SUMMARY_LIMIT);
                                    item.detail = Some(detail);
                                    // `detail` is now the tool output, so the
                                    // call identity persisted at start must be
                                    // carried through metadata. Mark the
                                    // terminal result too so restart history
                                    // rebuild can re-emit the paired
                                    // tool_call/tool_result (#5823).
                                    // Tool metadata carries output too
                                    // (`exec_shell` keeps stdout/stderr
                                    // summaries), so it is masked the same way.
                                    let mut meta = match output.metadata.as_ref().map(|value| {
                                        crate::client::redact_json_model_bound_text(
                                            value,
                                            &receipt_secrets,
                                        )
                                    }) {
                                        Some(Value::Object(map)) => Value::Object(map),
                                        _ => json!({}),
                                    };
                                    if let Some(obj) = meta.as_object_mut() {
                                        retain_started_tool_metadata(obj, item.metadata.as_ref());
                                        // A first call to a deferred tool only
                                        // loads its schema; the model retries.
                                        // That hand-off is not a user-facing step.
                                        if obj.get("deferred_tool_loaded").and_then(Value::as_bool)
                                            == Some(true)
                                        {
                                            obj.insert(
                                                "visibility".to_string(),
                                                json!(INTERNAL_ITEM_VISIBILITY),
                                            );
                                        }
                                        obj.insert("tool_result_for".to_string(), json!(id));
                                        obj.insert("is_error".to_string(), json!(!output.success));
                                        // The shell execution receipt (#6689) is
                                        // for completion hooks, which already
                                        // read it from the live result; it
                                        // repeats output previews `detail`
                                        // holds, so it is never persisted.
                                        obj.remove("execution_receipt");
                                    }
                                    // Failed calls count too: a large error
                                    // output spills like any other.
                                    let refs = turn_artifacts::artifact_refs_from_tool_metadata(
                                        &meta,
                                        &turn_artifacts::ToolArtifactContext {
                                            item_id: &item_id,
                                            tool_call_id: &id,
                                            tool_name: &name,
                                            workspace_roots: &artifact_workspace_roots,
                                            restore_snapshot_id: tool_restore_points
                                                .get(&id)
                                                .map(String::as_str),
                                            recorded_at: now,
                                        },
                                    );
                                    item.artifact_refs =
                                        turn_artifacts::legacy_artifact_refs(&refs);
                                    item.artifacts = refs;
                                    item.metadata = Some(meta);
                                }
                            }
                            Err(err) => {
                                item.status = TurnItemLifecycleStatus::Failed;
                                let err = crate::client::redact_model_bound_text(
                                    &err.to_string(),
                                    &receipt_secrets,
                                );
                                item.summary =
                                    summarize_text(&format!("{name} failed: {err}"), SUMMARY_LIMIT);
                                item.detail = Some(err);
                                // The engine answered this call with an error
                                // result the model received, so the record
                                // holds that answer the same way a completed
                                // or `success: false` call holds its own: the
                                // started identity stays, `detail` is the
                                // result, and the item is marked as the call's
                                // result. Without the marker a restart rebuilt
                                // a call nothing answered (#6803).
                                if let Some(Value::Object(meta)) = item.metadata.as_mut() {
                                    meta.insert("tool_result_for".to_string(), json!(id));
                                    meta.insert("is_error".to_string(), json!(true));
                                }
                            }
                        }
                        self.store.save_item(&item)?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item_id),
                            if item.status == TurnItemLifecycleStatus::Completed {
                                "item.completed"
                            } else {
                                "item.failed"
                            },
                            json!({ "item": item }),
                        )
                        .await?;
                    }
                }
                EngineEvent::SubAgentMailbox {
                    owner_session_id,
                    turn_id: mailbox_turn_id,
                    message:
                        crate::tools::subagent::MailboxMessage::TokenUsage {
                            source_id,
                            route,
                            usage,
                            ..
                        },
                    ..
                } if owner_session_id == thread_id => {
                    let belongs_to_turn = engine_turn_id
                        .as_deref()
                        .is_some_and(|started| started == mailbox_turn_id);
                    if belongs_to_turn {
                        self.append_routed_usage_to_turn(
                            &turn_id,
                            &source_id,
                            EffectiveRouteUsage {
                                route: *route,
                                usage,
                            },
                        )?;
                    }
                }
                EngineEvent::CompactionStarted { id, auto, message } => {
                    let item_id = runtime_record_id("item");
                    compaction_items.insert(id.clone(), item_id.clone());
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: item_id.clone(),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::ContextCompaction,
                        status: TurnItemLifecycleStatus::InProgress,
                        summary: summarize_text(&message, SUMMARY_LIMIT),
                        detail: Some(message.clone()),
                        metadata: Some(json!({ "compaction_id": id })),
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: None,
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item_id),
                        "item.started",
                        json!({ "item": item, "auto": auto }),
                    )
                    .await?;
                }
                EngineEvent::CompactionCompleted {
                    id,
                    auto,
                    message,
                    messages_before,
                    messages_after,
                    summary_prompt: _,
                    post_input_tokens: _,
                } => {
                    if let Some(item_id) = compaction_items.remove(&id) {
                        completed_compaction_item = Some(item_id.clone());
                        let mut item = self.store.load_item(&item_id)?;
                        item.status = TurnItemLifecycleStatus::Completed;
                        item.summary = summarize_text(&message, SUMMARY_LIMIT);
                        item.detail = Some(message);
                        item.ended_at = Some(Utc::now());
                        self.store.save_item(&item)?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item_id),
                            "item.completed",
                            json!({
                                "item": item,
                                "auto": auto,
                                "messages_before": messages_before,
                                "messages_after": messages_after,
                            }),
                        )
                        .await?;
                    }
                }
                EngineEvent::CompactionCancelled { id, auto, message } => {
                    if let Some(item_id) = compaction_items.remove(&id) {
                        let mut item = self.store.load_item(&item_id)?;
                        item.status = TurnItemLifecycleStatus::Canceled;
                        item.summary = summarize_text(&message, SUMMARY_LIMIT);
                        item.detail = Some(message);
                        item.ended_at = Some(Utc::now());
                        self.store.save_item(&item)?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item_id),
                            "item.canceled",
                            json!({ "item": item, "auto": auto }),
                        )
                        .await?;
                    }
                }
                EngineEvent::CompactionFailed { id, auto, message } => {
                    if let Some(item_id) = compaction_items.remove(&id) {
                        let mut item = self.store.load_item(&item_id)?;
                        item.status = TurnItemLifecycleStatus::Failed;
                        item.summary = summarize_text(&message, SUMMARY_LIMIT);
                        item.detail = Some(message);
                        item.ended_at = Some(Utc::now());
                        self.store.save_item(&item)?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            Some(&item_id),
                            "item.failed",
                            json!({ "item": item, "auto": auto }),
                        )
                        .await?;
                    }
                }
                EngineEvent::AgentList {
                    owner_session_id,
                    agents,
                    ..
                } if owner_session_id == thread_id => {
                    let running = agents
                        .iter()
                        .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
                        .count();
                    let interrupted = agents
                        .iter()
                        .filter(|agent| matches!(agent.status, SubAgentStatus::Interrupted(_)))
                        .count();
                    let completed = agents
                        .iter()
                        .filter(|agent| matches!(agent.status, SubAgentStatus::Completed))
                        .count();
                    let message = format!(
                        "Sub-agent list refreshed: {} total ({running} running, {interrupted} interrupted, {completed} completed)",
                        agents.len()
                    );
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: runtime_record_id("item"),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::Status,
                        status: TurnItemLifecycleStatus::Completed,
                        summary: summarize_text(&message, SUMMARY_LIMIT),
                        detail: Some(message),
                        metadata: None,
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: Some(Utc::now()),
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item.id),
                        "agent.list",
                        json!({ "item": item, "agents": agents }),
                    )
                    .await?;
                }
                EngineEvent::ApprovalRequired {
                    id,
                    tool_name,
                    description,
                    input,
                    approval_key,
                    approval_grouping_key,
                    intent_summary,
                    approval_force_prompt,
                    ..
                } => {
                    let Some(authority) = self
                        .active_turn_authority(&thread_id, &turn_id, &engine)
                        .await
                    else {
                        let _ = engine
                            .deny_tool_call_by(&id, crate::approval_log::ApprovalDecider::Host)
                            .await;
                        continue;
                    };
                    let auto_approve = authority.auto_approve;
                    let trust_mode = authority.trust_mode;
                    let approval_mode = authority.approval_mode;
                    let summary_workspace = self
                        .store
                        .load_thread(&thread_id)
                        .ok()
                        .map(|thread| thread.workspace);
                    let summary = crate::tools::approval_summary::approval_summary(
                        &tool_name,
                        &input,
                        summary_workspace.as_deref(),
                    );

                    let pending_request = PendingApprovalRequest {
                        id: String::new(),
                        turn_id: turn_id.clone(),
                        tool_name: tool_name.clone(),
                        input: Some(input.clone()),
                        description: description.clone(),
                        intent_summary: intent_summary.clone(),
                        tool_call_id: Some(id.clone()),
                        summary: Some(summary.clone()),
                    };

                    use crate::core::authority::{
                        ApprovalRequestDisposition, TurnAuthority,
                        resolve_approval_request_disposition,
                    };
                    let grant_workspace_id = match summary_workspace.clone() {
                        Some(workspace) => tokio::task::spawn_blocking(move || {
                            crate::core::authority::grants::workspace_id(&workspace)
                        })
                        .await
                        .unwrap_or_default(),
                        None => String::new(),
                    };
                    let grant_posture = if auto_approve {
                        crate::core::authority::grants::GrantPosture::FullAccess
                    } else {
                        match approval_mode {
                            codewhale_execpolicy::ApprovalMode::Auto => {
                                crate::core::authority::grants::GrantPosture::AutoReview
                            }
                            codewhale_execpolicy::ApprovalMode::Never => {
                                crate::core::authority::grants::GrantPosture::Never
                            }
                            _ => crate::core::authority::grants::GrantPosture::Ask,
                        }
                    };
                    let grant_class = grant_tool_class(&tool_name);
                    let grant_verdict =
                        self.grants
                            .evaluate(&crate::core::authority::grants::GrantQuery {
                                thread_id: &thread_id,
                                workspace_id: &grant_workspace_id,
                                origin: crate::core::authority::grants::GrantOrigin::Interactive,
                                posture: grant_posture,
                                tool_class: &grant_class,
                                exact_digest: &approval_key,
                                grouping_key: &approval_grouping_key,
                                argv: &[],
                                net_host: None,
                            });
                    let (grant, deny_grant) = match grant_verdict {
                        crate::core::authority::grants::GrantVerdict::Allow(grant) => {
                            (Some(grant), None)
                        }
                        crate::core::authority::grants::GrantVerdict::Deny(grant) => {
                            (None, Some(grant))
                        }
                        crate::core::authority::grants::GrantVerdict::NoMatch => (None, None),
                    };
                    // This resolver reads the exact live approval posture only;
                    // Core already owns mode and shell admission for this call.
                    let approval_authority = TurnAuthority::from_effective_fields(
                        codewhale_config::AppMode::Agent,
                        true,
                        trust_mode,
                        auto_approve,
                        approval_mode,
                    );
                    let disposition = resolve_approval_request_disposition(
                        &approval_authority,
                        grant.is_some(),
                        deny_grant.is_some(),
                        approval_force_prompt,
                        // Rust mints this namespace; tool text cannot claim origin.
                        approval_key.starts_with("extcall:ext:"),
                    );

                    if disposition == ApprovalRequestDisposition::AutoApprove && auto_approve {
                        // No waiter is registered on this path, but the emitted
                        // identity still has to obey the one contract clients
                        // read: `approval_id` is ours, `tool_call_id` is the
                        // provider's. Two turns holding the same raw call ID
                        // therefore stay distinguishable in the event stream.
                        let approval_id = Self::mint_approval_id();
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.required",
                            json!({
                                "id": approval_id,
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "tool_name": tool_name,
                                "input": input,
                                "summary": summary,
                                "description": description,
                                "intent_summary": intent_summary,
                            }),
                        )
                        .await?;
                        // Emit approval.decided so external clients (GUI)
                        // know the approval was resolved automatically and
                        // can clear any pending approval UI.  Without this
                        // event the GUI would show a frozen approval dialog
                        // that never receives approval.decided.
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.decided",
                            json!({
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "decision": "allow",
                                "remember": false,
                                "auto": true,
                            }),
                        )
                        .await
                        .ok();
                        let _ = engine
                            .approve_tool_call_by(id, crate::approval_log::ApprovalDecider::Posture)
                            .await;
                        continue;
                    }

                    // The same typed dispositions used by TUI fail closed:
                    // a Full Access policy hold or Never posture must not be
                    // answered by posture or a remembered conversation grant.
                    let denied_posture = match disposition {
                        ApprovalRequestDisposition::AutoDenyFullAccessPolicyHold => {
                            Some("full_access_policy_hold")
                        }
                        ApprovalRequestDisposition::AutoDenyAutoReview => Some("auto_review"),
                        ApprovalRequestDisposition::AutoDenyNeverPosture => Some("never"),
                        ApprovalRequestDisposition::AutoDenySessionDenied => Some("session_denied"),
                        ApprovalRequestDisposition::AutoApprove
                        | ApprovalRequestDisposition::Prompt => None,
                    };
                    if let Some(posture) = denied_posture {
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.decided",
                            json!({
                                "approval_id": Self::mint_approval_id(),
                                "tool_call_id": id,
                                "decision": "deny",
                                "remember": false,
                                "auto": true,
                                "posture": posture,
                                "grant_id": deny_grant.as_ref().map(|grant| grant.grant_id.clone()),
                            }),
                        )
                        .await
                        .ok();
                        let _ = match deny_grant {
                            Some(grant) => {
                                engine
                                    .deny_tool_call_by_grant(
                                        id,
                                        crate::approval_log::ApprovalDecider::SessionRule,
                                        grant,
                                    )
                                    .await
                            }
                            None => {
                                engine
                                    .deny_tool_call_by(
                                        id,
                                        crate::approval_log::ApprovalDecider::Posture,
                                    )
                                    .await
                            }
                        };
                        continue;
                    }

                    // A session grant for this tool and argument class
                    // answers the prompt without a modal and without touching
                    // posture (E1). A forced prompt is never pre-answered.
                    if disposition == ApprovalRequestDisposition::AutoApprove
                        && let Some(grant) = grant
                        && self.grants.record_use(&grant.grant_id).is_ok()
                    {
                        let approval_id = Self::mint_approval_id();
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.required",
                            json!({
                                "id": approval_id,
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "tool_name": tool_name,
                                "input": input,
                                "summary": summary,
                                "description": description,
                                "intent_summary": intent_summary,
                            }),
                        )
                        .await?;
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.decided",
                            json!({
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "decision": "allow",
                                "remember": false,
                                "auto": true,
                                "grant_id": grant.grant_id,
                            }),
                        )
                        .await
                        .ok();
                        let _ = engine
                            .approve_tool_call_by_grant(
                                id,
                                crate::approval_log::ApprovalDecider::SessionRule,
                                grant,
                            )
                            .await;
                        continue;
                    }

                    // Register before sequencing the event. A snapshot racing
                    // this branch therefore either contains the request or
                    // subscribes from an older cursor that will replay it.
                    let projection_lock = self.projection_lock(&thread_id);
                    let projection = projection_lock.lock().await;
                    let registration = {
                        let active = self.active.lock().await;
                        let accepting = active
                            .engines
                            .get(&thread_id)
                            .and_then(|state| state.active_turn.as_ref())
                            .is_some_and(|turn| {
                                turn.turn_id == turn_id && !turn.interrupt_requested
                            });
                        accepting
                            .then(|| self.register_pending_approval(&thread_id, pending_request))
                    };
                    let Some((approval_id, rx)) = registration else {
                        drop(projection);
                        let _ = engine
                            .deny_tool_call_by(&id, crate::approval_log::ApprovalDecider::Host)
                            .await;
                        continue;
                    };
                    if let Err(err) = self
                        .emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.required",
                            json!({
                                "id": approval_id,
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "tool_name": tool_name,
                                "input": input,
                                "summary": summary,
                                "description": description,
                                "intent_summary": intent_summary,
                            }),
                        )
                        .await
                    {
                        self.cancel_pending_approval(&approval_id);
                        drop(projection);
                        let _ = engine
                            .deny_tool_call_by(&id, crate::approval_log::ApprovalDecider::Host)
                            .await;
                        return Err(err);
                    }
                    drop(projection);
                    let approval_timeout = self.approval_decision_timeout();
                    let wait_for_decision = async {
                        match approval_timeout {
                            Some(wait) => tokio::time::timeout(wait, rx).await,
                            None => Ok(rx.await),
                        }
                    };
                    let mut wait_for_decision = std::pin::pin!(wait_for_decision);
                    // Keep draining engine status while the card is open. The
                    // engine's approval-wait heartbeat fires during this wait;
                    // parking the pump here used to sequence it after
                    // `approval.decided`, where it read as a live claim that
                    // the answered call was still waiting (DESKTOP-QA-20260923).
                    // Other events stay ordered for the main loop, but must
                    // not hide an approval withdrawal queued behind them.
                    let decision = loop {
                        tokio::select! {
                            biased;
                            event = async { engine.rx_event.write().await.recv().await },
                                if !event_channel_closed =>
                            {
                                match event {
                                    Some(EngineEvent::UserInputRequired { id, request }) => {
                                        self.publish_user_input_request(&thread_id, &turn_id, &engine, id, request).await?;
                                    }
                                    Some(EngineEvent::UserInputClosed { id, outcome }) => {
                                        self.user_input_settlement(&thread_id).close(&id, Some(&turn_id), outcome).await?;
                                    }
                                    Some(EngineEvent::SessionUpdated { checkpoint: Some(checkpoint), .. }) => {
                                        checkpoint.fail("Runtime checkpoint cannot wait behind an approval");
                                    }
                                    Some(EngineEvent::ApprovalWithdrawn { id: withdrawn }) if withdrawn == id => {
                                        self.cancel_pending_approval(&approval_id);
                                        break None;
                                    }
                                    Some(terminal @ EngineEvent::TurnComplete { .. }) => {
                                        pending_events.push_back(terminal);
                                        self.cancel_pending_approval(&approval_id);
                                        break None;
                                    }
                                    Some(EngineEvent::Status { message }) => {
                                        if let Err(err) = self
                                            .publish_status_item(
                                                &thread_id,
                                                &turn_id,
                                                message,
                                                &settled_approval_calls,
                                            )
                                            .await
                                        {
                                            tracing::warn!(
                                                thread_id = %thread_id,
                                                turn_id = %turn_id,
                                                "failed to persist status during approval wait: {err:#}"
                                            );
                                        }
                                    }
                                    Some(other) => pending_events.push_back(other),
                                    None => {
                                        event_channel_closed = true;
                                        self.cancel_pending_approval(&approval_id);
                                        break None;
                                    }
                                }
                            }
                            decision = &mut wait_for_decision => break Some(decision),
                        }
                    };
                    settled_approval_calls.insert(id.clone());
                    let Some(decision) = decision else {
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.decided",
                            json!({
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "decision": "deny",
                                "remember": false,
                                "cancelled": true,
                            }),
                        )
                        .await?;
                        // The engine has already recorded Cancelled. A stale
                        // card cannot grant work or overwrite that receipt.
                        continue;
                    };
                    // A decision may already have consumed the sender when
                    // Stop wins. Never remember or dispatch that late allow.
                    let cancelled = {
                        let active = self.active.lock().await;
                        let accepting = active
                            .engines
                            .get(&thread_id)
                            .and_then(|state| state.active_turn.as_ref())
                            .is_some_and(|turn| {
                                turn.turn_id == turn_id && !turn.interrupt_requested
                            });
                        !accepting
                    };
                    if cancelled {
                        self.cancel_pending_approval(&approval_id);
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "approval.decided",
                            json!({
                                "approval_id": approval_id,
                                "tool_call_id": id,
                                "decision": "deny",
                                "remember": false,
                                "cancelled": true,
                            }),
                        )
                        .await
                        .ok();
                        let _ = engine
                            .deny_tool_call_by(id, crate::approval_log::ApprovalDecider::Host)
                            .await;
                        continue;
                    }
                    match decision {
                        Ok(Ok(ExternalApprovalDecision::Allow { remember })) => {
                            // "Allow for this conversation" records a grant
                            // for this tool and argument class. It must not
                            // change posture: a posture change mid-turn used
                            // to fail the very call it approved (E1/E2).
                            let grant = if remember {
                                self.add_session_grant(
                                    &thread_id,
                                    &turn_id,
                                    &tool_name,
                                    &approval_grouping_key,
                                    &summary,
                                )
                                .await
                            } else {
                                None
                            };
                            self.emit_event(
                                &thread_id,
                                Some(&turn_id),
                                None,
                                "approval.decided",
                                json!({
                                    "approval_id": approval_id,
                                    "tool_call_id": id,
                                    "decision": "allow",
                                    "remember": remember,
                                    "grant_id": grant.map(|grant| grant.grant_id),
                                }),
                            )
                            .await
                            .ok();
                            let _ = engine.approve_tool_call(id).await;
                        }
                        Ok(Ok(ExternalApprovalDecision::Deny { remember })) => {
                            self.emit_event(
                                &thread_id,
                                Some(&turn_id),
                                None,
                                "approval.decided",
                                json!({
                                    "approval_id": approval_id,
                                    "tool_call_id": id,
                                    "decision": "deny",
                                    "remember": remember,
                                }),
                            )
                            .await
                            .ok();
                            let _ = engine.deny_tool_call(id).await;
                        }
                        Ok(Err(_recv_err)) => {
                            // The decision channel closed with no answer:
                            // nobody refused the call, it was unavailable.
                            self.cancel_pending_approval(&approval_id);
                            let _ = engine.deny_tool_call_unavailable(id).await;
                        }
                        Err(_timeout) => {
                            self.cancel_pending_approval(&approval_id);
                            self.emit_event(
                                &thread_id,
                                Some(&turn_id),
                                None,
                                "approval.timeout",
                                json!({
                                    "approval_id": approval_id,
                                    "tool_call_id": id,
                                    "timeout_secs": approval_timeout.map(|wait| wait.as_secs()),
                                }),
                            )
                            .await
                            .ok();
                            self.emit_event(
                                &thread_id,
                                Some(&turn_id),
                                None,
                                "approval.decided",
                                json!({
                                    "approval_id": approval_id,
                                    "tool_call_id": id,
                                    "decision": "deny",
                                    "remember": false,
                                    "timeout": true,
                                }),
                            )
                            .await
                            .ok();
                            // Recorded and reported as a timeout, not the
                            // operator's denial; the engine refunds the call's
                            // tool-call budget slot.
                            let _ = engine.deny_tool_call_timed_out(id).await;
                        }
                    }
                }
                EngineEvent::ApprovalWithdrawn { .. } => {}
                EngineEvent::ElevationRequired {
                    tool_id,
                    tool_name,
                    denial_reason,
                    ..
                } => {
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        None,
                        "sandbox.denied",
                        json!({
                            "tool_id": tool_id,
                            "tool_name": tool_name,
                            "reason": denial_reason,
                        }),
                    )
                    .await?;
                    self.raise_notice(
                        &thread_id,
                        codewhale_config::notifications::NotificationEvent::ElevationNeeded
                            .as_str(),
                        &turn_id,
                        &tool_id,
                        format!("{tool_name} needs elevation: {denial_reason}"),
                    );
                    let authority = self
                        .active_turn_authority(&thread_id, &turn_id, &engine)
                        .await
                        .unwrap_or(crate::core::engine::RuntimePermissionAuthority {
                            auto_approve: false,
                            trust_mode: false,
                            approval_mode: ApprovalMode::Suggest,
                        });
                    let auto_approve = authority.auto_approve;
                    let trust_mode = authority.trust_mode;
                    match Self::approval_decision(auto_approve, trust_mode, true) {
                        RuntimeApprovalDecision::RetryWithFullAccess => {
                            let _ = engine
                                .retry_tool_with_policy_by(
                                    tool_id,
                                    codewhale_sandbox::SandboxPolicy::DangerFullAccess,
                                    crate::approval_log::ApprovalDecider::Posture,
                                )
                                .await;
                        }
                        RuntimeApprovalDecision::ApproveTool
                        | RuntimeApprovalDecision::DenyTool => {
                            let _ = engine
                                .deny_tool_call_by(
                                    tool_id,
                                    crate::approval_log::ApprovalDecider::Posture,
                                )
                                .await;
                        }
                    }
                }
                EngineEvent::UserInputRequired { id, request } => {
                    self.publish_user_input_request(&thread_id, &turn_id, &engine, id, request)
                        .await?;
                }
                EngineEvent::Status { message } => {
                    // A heartbeat queued behind another event while its
                    // approval was answered is dropped inside the helper.
                    self.publish_status_item(
                        &thread_id,
                        &turn_id,
                        message,
                        &settled_approval_calls,
                    )
                    .await?;
                }
                EngineEvent::ToolProjectionWarning {
                    provider,
                    omitted_tool_names,
                    omitted_tool_count,
                } => {
                    let message = crate::core::events::tool_projection_warning_message(
                        &provider,
                        &omitted_tool_names,
                        omitted_tool_count,
                    );
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: runtime_record_id("item"),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::Status,
                        status: TurnItemLifecycleStatus::Completed,
                        summary: summarize_text(&message, SUMMARY_LIMIT),
                        detail: Some(message),
                        metadata: Some(json!({
                            "code": "provider_tool_projection_warning",
                            "provider": provider,
                            "omitted_tool_names": omitted_tool_names,
                            "omitted_tool_count": omitted_tool_count,
                        })),
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: Some(Utc::now()),
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item.id),
                        "item.completed",
                        json!({ "item": item }),
                    )
                    .await?;
                }
                EngineEvent::RequestImageWindow {
                    provider,
                    model,
                    total,
                    elided,
                    cap,
                    cap_source,
                    message,
                } => {
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: runtime_record_id("item"),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::Status,
                        status: TurnItemLifecycleStatus::Completed,
                        summary: summarize_text(&message, SUMMARY_LIMIT),
                        detail: Some(message),
                        metadata: Some(json!({
                            "code": "request_image_window",
                            "provider": provider,
                            "model": model,
                            "total": total,
                            "elided": elided,
                            "cap": cap,
                            "cap_source": cap_source,
                        })),
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: Some(Utc::now()),
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item.id),
                        "item.completed",
                        json!({ "item": item }),
                    )
                    .await?;
                }
                EngineEvent::Error { envelope, .. } => {
                    turn_status = Some(RuntimeTurnStatus::Failed);
                    turn_error = Some(envelope.message.clone());
                    let message = envelope.message.clone();
                    let item = TurnItemRecord {
                        schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                        id: runtime_record_id("item"),
                        turn_id: turn_id.clone(),
                        kind: TurnItemKind::Error,
                        status: TurnItemLifecycleStatus::Failed,
                        summary: summarize_text(&message, SUMMARY_LIMIT),
                        detail: Some(message),
                        metadata: None,
                        artifact_refs: Vec::new(),
                        artifacts: Vec::new(),
                        started_at: Some(Utc::now()),
                        ended_at: Some(Utc::now()),
                    };
                    self.store.save_item(&item)?;
                    self.attach_item_to_turn(&turn_id, &item.id)?;
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        Some(&item.id),
                        "item.failed",
                        json!({ "item": item }),
                    )
                    .await?;
                }
                EngineEvent::ProviderRequestReceipt { receipt } => {
                    if receipt["outcome"] == "steered"
                        || receipt["outcome"] == "steered_before_stream"
                    {
                        for mut item in [current_message_item.take(), current_reasoning_item.take()]
                            .into_iter()
                            .flatten()
                        {
                            item.status = TurnItemLifecycleStatus::Interrupted;
                            item.summary = summarize_text(
                                item.detail.as_deref().unwrap_or_default(),
                                SUMMARY_LIMIT,
                            );
                            item.ended_at = Some(Utc::now());
                            self.save_streaming_item(&item).await?;
                            self.emit_event(
                                &thread_id,
                                Some(&turn_id),
                                Some(&item.id),
                                "item.interrupted",
                                json!({ "item": item }),
                            )
                            .await?;
                        }
                    }
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        None,
                        "turn.provider_request",
                        receipt,
                    )
                    .await?;
                }
                EngineEvent::TurnUsage {
                    max_output_tokens,
                    usage,
                    duration_ms,
                    first_token_ms,
                    request_ms,
                    finish_reason: _,
                } => {
                    let cost = priced_step_usage(dispatched_route.as_ref(), &usage);
                    let billed_prompt = billed_prompt_tokens(&usage);
                    self.emit_event(
                        &thread_id,
                        Some(&turn_id),
                        None,
                        "turn.usage",
                        json!({
                            "usage": usage, "duration_ms": duration_ms,
                            "first_token_ms": first_token_ms, "request_ms": request_ms,
                            "maxOutputTokens": max_output_tokens,
                            "billed_prompt_tokens": billed_prompt,
                            "cost": cost,
                        }),
                    )
                    .await?;
                }
                EngineEvent::ToolRequestSnapshot { snapshot } => {
                    if !snapshot.turn_id.truncated
                        && engine_turn_id.as_deref() == Some(snapshot.turn_id.value.as_str())
                    {
                        if let Some(terminal) = snapshot.terminal.as_ref() {
                            let mut facts: RuntimeTurnRequestDiagnostics = terminal.into();
                            if !admitted_acp {
                                facts.stop_reason = None;
                            }
                            turn_model_request_diagnostics = Some(facts);
                        }
                        // Keep the existing bounded request projection in the
                        // originating task's durable event stream. It describes
                        // prepared tools, never proves provider delivery, and
                        // does not add conversation input or transcript noise.
                        let snapshot = codewhale_config::persistence::redact_json_secrets(
                            &serde_json::to_value(snapshot)?,
                        );
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "model.tools.snapshot",
                            json!({ "snapshot": snapshot, "projection_redacted": true }),
                        )
                        .await?;
                    }
                }
                EngineEvent::WorkspaceSnapshotTaken { snapshot } => {
                    // The restore point belongs to the turn this monitor
                    // owns: snapshot receipts share the engine's FIFO
                    // channel and, with `record_restore_points`, all
                    // arrive before this turn's TurnComplete. A receipt that
                    // cannot be recorded leaves the turn without that
                    // restore point, and patch-undo then refuses the turn
                    // instead of guessing — so say it loudly, but do not fail
                    // a turn whose work already happened.
                    let recorded = {
                        let _turn_mutation = self.store.turn_mutation.lock();
                        self.store.load_turn(&turn_id).and_then(|mut turn| {
                            turn.workspace_snapshots.push(snapshot.clone());
                            self.store.save_turn(&turn)
                        })
                    };
                    if let Err(err) = recorded {
                        tracing::warn!(
                            target: "snapshot",
                            thread_id = %thread_id,
                            turn_id = %turn_id,
                            kind = ?snapshot.kind,
                            "workspace snapshot receipt was not recorded; this turn has no such restore point: {err:#}"
                        );
                    } else {
                        if snapshot.kind == codewhale_snapshot::WorkspaceSnapshotKind::Tool
                            && let Some(call_id) = snapshot.tool_call_id.as_ref()
                        {
                            tool_restore_points.insert(call_id.clone(), snapshot.tree_id.clone());
                        }
                        self.emit_event(
                            &thread_id,
                            Some(&turn_id),
                            None,
                            "turn.workspace_snapshot",
                            serde_json::to_value(&snapshot)?,
                        )
                        .await?;
                    }
                }
                EngineEvent::TurnComplete {
                    usage,
                    parent_route_usage,
                    routed_usage_dropped_records,
                    status,
                    error,
                    tool_catalog,
                    ..
                } => {
                    turn_usage = Some(usage);
                    turn_effective_route_usage = Some(parent_route_usage);
                    turn_routed_usage_dropped_records = routed_usage_dropped_records;
                    if tool_catalog.is_some() {
                        turn_tool_catalog = tool_catalog;
                    }
                    let reported_status = match status {
                        TurnOutcomeStatus::Completed => RuntimeTurnStatus::Completed,
                        TurnOutcomeStatus::Interrupted => RuntimeTurnStatus::Interrupted,
                        TurnOutcomeStatus::Failed => RuntimeTurnStatus::Failed,
                    };
                    // Some engines emit a categorized Error followed by their
                    // generic TurnComplete(Completed) cleanup receipt. Keep
                    // the error authoritative instead of silently converting
                    // a failed turn back to success.
                    turn_status = Some(
                        if turn_status == Some(RuntimeTurnStatus::Failed)
                            && reported_status == RuntimeTurnStatus::Completed
                        {
                            RuntimeTurnStatus::Failed
                        } else {
                            reported_status
                        },
                    );
                    if let Some(err) = error {
                        turn_error = Some(err);
                    }
                    terminal_received = true;
                    break;
                }
                EngineEvent::GoalUpdated { snapshot } => {
                    snapshot
                        .validate_stall_state()
                        .map_err(anyhow::Error::msg)?;
                    if let Some(goal_id) = admitted_goal_id.as_deref() {
                        // Persist an acknowledged review before awaiting another
                        // event, so restart midway through a turn retains it.
                        self.store
                            .update_goal_if_revision(&thread_id, goal_id, None, |goal| {
                                merge_engine_goal_progress(goal, &snapshot);
                            })?;
                    } else if snapshot.is_active() {
                        // The turn was admitted with no host goal, but the
                        // model created one through create_goal. Persist it
                        // only when the store still has none — a concurrent
                        // PUT/DELETE is the newer revision and wins — and
                        // adopt the revision only when we actually created it,
                        // so settlement and later checkpoints stay fenced.
                        if let Some(goal_id) = self
                            .store
                            .create_goal_from_snapshot_if_absent(&thread_id, &snapshot)?
                        {
                            admitted_goal_id = Some(goal_id);
                        }
                    }
                    {
                        let mut active = self.active.lock().await;
                        if let Some(turn) = active
                            .engines
                            .get_mut(&thread_id)
                            .and_then(|state| state.active_turn.as_mut())
                            && turn.turn_id == turn_id
                        {
                            turn.goal_id.clone_from(&admitted_goal_id);
                            turn.goal_progress = Some(snapshot.clone());
                        }
                    }
                    if let Some(goal) = self.get_goal(&thread_id).await? {
                        self.emit_goal_updated_event(&thread_id, goal).await?;
                    }
                    latest_goal_snapshot = Some(snapshot);
                }
                _ => {}
            }
        }

        let mut turn_status = turn_status
            .expect("turn monitor exits normally only after assigning a terminal status");

        if self
            .is_interrupt_requested(&thread_id, &turn_id)
            .await
            .unwrap_or(false)
        {
            turn_status = RuntimeTurnStatus::Interrupted;
        }

        let item_status = terminal_item_status(turn_status);
        self.close_streaming_item(&thread_id, &turn_id, &mut current_message_item, item_status)
            .await?;
        self.close_streaming_item(
            &thread_id,
            &turn_id,
            &mut current_reasoning_item,
            item_status,
        )
        .await?;

        if turn_status == RuntimeTurnStatus::Completed && !saw_engine_activity {
            turn_status = RuntimeTurnStatus::Failed;
            turn_error = Some(EMPTY_TURN_REASON.to_string());
            let item = TurnItemRecord {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                id: runtime_record_id("item"),
                turn_id: turn_id.clone(),
                kind: TurnItemKind::Error,
                status: TurnItemLifecycleStatus::Failed,
                summary: EMPTY_TURN_REASON.to_string(),
                detail: Some(EMPTY_TURN_REASON.to_string()),
                metadata: None,
                artifact_refs: Vec::new(),
                artifacts: Vec::new(),
                started_at: Some(Utc::now()),
                ended_at: Some(Utc::now()),
            };
            self.store.save_item(&item)?;
            self.attach_item_to_turn(&turn_id, &item.id)?;
            self.emit_event(
                &thread_id,
                Some(&turn_id),
                Some(&item.id),
                "item.failed",
                json!({ "item": item }),
            )
            .await?;
        }

        let ended_at = Utc::now();
        crate::cost_status::finish_runtime_usage_owner(&turn_id);
        let background_usage = crate::cost_status::take_runtime_usage(&turn_id);

        // A terminal turn can no longer answer an outstanding prompt. Commit
        // each cancellation while the request remains snapshot-authoritative,
        // then remove and notify the engine before publishing completion.
        self.settle_user_inputs_for_terminal_turn(&thread_id, &turn_id, Some(engine.clone()))
            .await?;

        self.settle_dynamic_tools_for_terminal_turn(&thread_id, &turn_id)
            .await?;

        if terminal_received
            && !self.cancel_token.is_cancelled()
            && let Some(item_id) = completed_compaction_item.as_deref()
        {
            let mut messages = engine.get_session_snapshot().await?.messages;
            for message in &mut messages {
                crate::core::engine::redact_tool_blocks(message, |text| {
                    crate::client::redact_model_bound_text(text, &receipt_secrets)
                });
            }
            let store = self.store.clone();
            let reference =
                tokio::task::spawn_blocking(move || store.save_settled_history(&messages))
                    .await
                    .context("settled history write task failed")??;
            let mut item = self.store.load_item(item_id)?;
            let mut metadata = item.metadata.take().unwrap_or_else(|| json!({}));
            let Some(fields) = metadata.as_object_mut() else {
                bail!("Compaction item {item_id} has non-object metadata");
            };
            fields.insert("settled_history".to_string(), reference);
            item.metadata = Some(metadata);
            self.store.save_item(&item)?;
        }

        // Publish the terminal projection as one snapshot boundary. The
        // duplicate scan is offloaded while this guard is held, so public
        // readers cannot observe a terminal record before its receipt and
        // active-claim cleanup are ordered.
        let projection_lock = self.projection_lock(&thread_id);
        let _projection = projection_lock.lock().await;
        // A leased child may persist a late receipt while pending requests
        // settle above. Read the latest turn only now and commit its terminal
        // fields under the same mutation lock, so no stale snapshot can
        // overwrite that successfully persisted receipt.
        let (turn, swept_items) = {
            let _turn_mutation = self.store.turn_mutation.lock();
            let mut turn = self.store.load_turn(&turn_id)?;
            turn.status = turn_status;
            if let Some(activity) = &mut turn.operation_activity {
                activity.active.clear();
            }
            turn.ended_at = Some(ended_at);
            turn.duration_ms = turn.started_at.map(|start| duration_ms(start, ended_at));
            turn.usage = turn_usage;
            turn.effective_route_usage = turn_effective_route_usage;
            for receipt in &background_usage.decisions {
                append_decision_receipt(&mut turn, receipt);
            }
            for record in background_usage.records {
                append_routed_usage_record(&mut turn, &record.source_id, record.usage);
            }
            let background_exact_drop_count = background_usage.drop_records.len();
            for record in background_usage.drop_records {
                append_routed_usage_drop_record(&mut turn, record);
            }
            let background_residual = background_usage
                .dropped_records
                .saturating_sub(u64::try_from(background_exact_drop_count).unwrap_or(u64::MAX));
            turn.routed_usage_dropped_records = turn
                .routed_usage_dropped_records
                .saturating_add(background_residual)
                .saturating_add(turn_routed_usage_dropped_records);
            turn.model_request_diagnostics = turn_model_request_diagnostics;
            turn.error = turn_error;
            // Item refs are final now. The workspace delta derives from the
            // pre/post-turn restore points recorded on this turn (every
            // receipt arrives before TurnComplete) and settles off the
            // monitor, since diffing them is git work.
            let initial = self.initial_turn_workspace(&thread_id, &turn.workspace_snapshots);
            self.set_turn_artifacts(&mut turn, None, initial);
            // Every item reaches its terminal state before the parent turn is
            // written. The segment closes above only cover what this monitor
            // still held in memory; a tool call, a compaction, or a reasoning
            // segment whose completion frame the engine never sent (a stream
            // retry discards the fragment without one) is reachable only
            // through the turn's item list. Normalizing them here is the
            // mechanism that keeps a terminal turn free of `in_progress` rows,
            // whichever event branch forgets to close one: otherwise
            // `thread_detail_has_live_work` reads the thread as permanently
            // busy and refuses every later session save.
            let swept_items = self.settle_turn_items(&turn, turn_status, ended_at)?;
            self.store.save_turn(&turn)?;
            (turn, swept_items)
        };
        // Published after the record guard is released: `emit_event` awaits,
        // and the turn mutation lock is a std mutex.
        for item in swept_items {
            self.emit_event(
                &thread_id,
                Some(&turn_id),
                Some(&item.id),
                item_terminal_event(item.status),
                json!({ "item": item }),
            )
            .await?;
        }
        {
            let _thread_mutation = self.store.thread_mutation.lock();
            let mut thread = self.store.load_thread(&thread_id)?;
            thread.latest_turn_id = Some(turn_id.clone());
            thread.updated_at = Utc::now();
            self.store.save_thread(&thread)?;
        }
        self.emit_turn_completed_if_missing(&turn, false).await?;
        if let Some(pair) = turn_snapshot_pair(&turn.workspace_snapshots)
            && turn
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.state == TurnWorkspaceState::Pending)
        {
            self.spawn_turn_workspace_settlement(thread_id.clone(), turn_id.clone(), pair);
        }

        {
            let mut active = self.active.lock().await;
            if let Some(state) = active.engines.get_mut(&thread_id)
                && state
                    .active_turn
                    .as_ref()
                    .is_some_and(|t| t.turn_id == turn_id)
            {
                state.active_turn = None;
            }
            touch_lru(&mut active.lru, &thread_id);
        }

        // The same terminal boundary settles the durable goal loop: usage is
        // written back, the model's terminal decision is mirrored, and the
        // next pass is armed while the goal is still Active. Runs after the
        // active-turn cleanup above so an armed pass sees an idle thread. A
        // goal pass and the mail wake below can race for the same durable
        // claim; a goal pass that loses the race simply re-arms after the
        // mail turn's own settlement, so no arbitration is needed here.
        if !admitted_acp {
            self.settle_thread_goal_after_turn(
                &thread_id,
                &turn,
                latest_goal_snapshot,
                admitted_goal_id.as_deref(),
                turn_tool_catalog.as_deref(),
            )
            .await;
        }

        // A terminal turn is the declared safe boundary. Wake at most the
        // oldest eligible envelope; its own terminal boundary may advance the
        // next one, keeping every wake explicit and bounded to one turn.
        if !admitted_acp {
            self.spawn_agent_mail_safe_boundary_delivery(thread_id.clone());
        }

        Ok(())
    }

    /// Every artifact ref this turn's items recorded, in item order.
    pub(super) fn item_artifact_refs(&self, turn: &TurnRecord) -> Vec<TurnArtifactRef> {
        turn.item_ids
            .iter()
            .filter_map(|item_id| self.store.load_item(item_id).ok())
            .flat_map(|item| item.artifacts)
            .collect()
    }

    /// Bring every item this turn still lists as live to the turn's own
    /// terminal status, and return the ones that were closed.
    ///
    /// The turn's item list is the only complete view of what it produced: a
    /// tool call whose result never arrived, a compaction the stream died
    /// under, and a reasoning or message segment superseded by a retried
    /// stream all outlive the monitor's `current_*` pointers. A turn that ends
    /// while any of them still reads `queued`/`in_progress` is a turn every
    /// reader treats as unfinished — `thread_detail_has_live_work` refuses to
    /// save a session for it, `receipts` reports a step still running, and
    /// `call-changes` answers `pending` forever.
    ///
    /// An item record that cannot be read is left alone and logged: the sweep
    /// repairs what it can see and must never fail the turn's own completion.
    fn settle_turn_items(
        &self,
        turn: &TurnRecord,
        turn_status: RuntimeTurnStatus,
        ended_at: DateTime<Utc>,
    ) -> Result<Vec<TurnItemRecord>> {
        let status = terminal_item_status(turn_status);
        let mut settled = Vec::new();
        for item_id in &turn.item_ids {
            let mut item = match self.store.load_item(item_id) {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(
                        turn_id = %turn.id,
                        item_id = %item_id,
                        %error,
                        "turn settlement could not read an item; leaving it as recorded"
                    );
                    continue;
                }
            };
            if !matches!(
                item.status,
                TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
            ) {
                continue;
            }
            item.status = status;
            item.summary =
                summarize_text(item.detail.as_deref().unwrap_or_default(), SUMMARY_LIMIT);
            item.ended_at = Some(ended_at);
            self.store.save_item(&item)?;
            settled.push(item);
        }
        Ok(settled)
    }

    /// Recompute a turn's aggregate through the one merge function.
    pub(super) fn set_turn_artifacts(
        &self,
        turn: &mut TurnRecord,
        delta: Option<&turn_artifacts::WorkspaceDelta>,
        mut workspace: TurnWorkspaceArtifacts,
    ) {
        let items = self.item_artifact_refs(turn);
        let merged = turn_artifacts::merge_turn_artifacts(&items, delta);
        workspace.truncated = merged.truncated;
        workspace.omitted = merged.omitted;
        turn.artifacts = merged.artifacts;
        turn.workspace = Some(workspace);
    }

    pub(super) fn spawn_turn_workspace_settlement(
        &self,
        thread_id: String,
        turn_id: String,
        pair: TurnSnapshotPair,
    ) {
        let manager = self.clone();
        let worker = tokio::spawn(async move {
            if let Err(error) = manager
                .settle_turn_workspace(&thread_id, &turn_id, pair)
                .await
            {
                tracing::warn!(thread_id, turn_id, %error, "Failed to settle turn artifacts");
            }
        });
        self.track_receipt_worker(worker);
    }

    /// A turn's workspace state at terminal settlement, before any delta,
    /// from the restore points recorded on it. `pending` when it holds both a
    /// pre-turn and a post-turn receipt; otherwise `unavailable`, with the
    /// snapshot gate that refused the thread's engine when there is one.
    pub(super) fn initial_turn_workspace(
        &self,
        thread_id: &str,
        snapshots: &[codewhale_snapshot::WorkspaceSnapshotRef],
    ) -> TurnWorkspaceArtifacts {
        if let Some(pair) = turn_snapshot_pair(snapshots) {
            return TurnWorkspaceArtifacts::pending(pair.pre.tree_id);
        }
        let pre = snapshots
            .iter()
            .find(|snapshot| snapshot.kind == codewhale_snapshot::WorkspaceSnapshotKind::PreTurn);
        let gated = || -> Option<TurnWorkspaceReason> {
            if !self.config.read().snapshots_config().enabled {
                return Some(TurnWorkspaceReason::SnapshotsDisabled);
            }
            let workspace = self.store.load_thread(thread_id).ok()?.workspace;
            // The engine runs under the thread's own id (#6621), so its
            // gate notice is keyed by it.
            let status = crate::core::turn::snapshots_disabled_status(&workspace, Some(thread_id))?;
            use crate::core::turn::SnapshotsDisabledScope;
            match status.scope {
                SnapshotsDisabledScope::WorkspaceTooLarge => {
                    Some(TurnWorkspaceReason::WorkspaceTooLarge)
                }
                SnapshotsDisabledScope::TooManyFiles => Some(TurnWorkspaceReason::TooManyFiles),
                SnapshotsDisabledScope::UnsafeLocation => Some(TurnWorkspaceReason::UnsafeLocation),
                _ => Some(TurnWorkspaceReason::SnapshotFailed),
            }
        };
        match pre {
            // A pre-turn restore point without its post-turn pair: the
            // closing snapshot failed or was gated.
            Some(pre) => TurnWorkspaceArtifacts {
                pre_turn_snapshot_id: Some(pre.tree_id.clone()),
                ..TurnWorkspaceArtifacts::unavailable(
                    gated().unwrap_or(TurnWorkspaceReason::SnapshotFailed),
                )
            },
            // No pre-turn receipt: snapshots were off or gated, or the turn
            // (a compaction, a purge, one that ended early) took none. A
            // failed snapshot reports no receipt either.
            None => TurnWorkspaceArtifacts::unavailable(
                gated().unwrap_or(TurnWorkspaceReason::NotCaptured),
            ),
        }
    }

    /// Diff the turn's recorded pre-turn and post-turn restore points, merge
    /// the delta into the turn's aggregate, and publish `turn.artifacts`.
    /// Every outcome publishes, so a client waiting on a `pending` turn
    /// always hears back.
    pub(super) async fn settle_turn_workspace(
        &self,
        thread_id: &str,
        turn_id: &str,
        pair: TurnSnapshotPair,
    ) -> Result<()> {
        let thread = self.store.load_thread(thread_id)?;
        let turn = self.store.load_turn(turn_id)?;
        let item_paths: Vec<String> = self
            .item_artifact_refs(&turn)
            .into_iter()
            .filter(|reference| reference.kind == TurnArtifactKind::File)
            .flat_map(|reference| std::iter::once(reference.path).chain(reference.previous_path))
            .collect();
        // The pre-turn restore point is recorded on this turn, so this
        // thread owns it and file-revert accepts it for every path the delta
        // names (#6621). Trees, not commit ids: a prune rewrites commits.
        let restore_snapshot_id = Some(pair.pre.tree_id.clone());
        let TurnSnapshotPair { pre, post } = pair;
        let workspace = thread.workspace.clone();
        let (pre_tree, post_tree) = (pre.tree_id, post.tree_id);
        #[cfg(any(test, feature = "test-support"))]
        let env_ticket = crate::test_support::env_scope_ticket();
        let delta = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            workspace_delta(&workspace, &pre_tree, &post_tree, &item_paths)
                .map(|(delta, tracked)| (post_tree, delta, tracked))
        })
        .await
        .map_err(|error| anyhow!("turn delta task failed: {error}"))
        .and_then(|result| result)
        .map_err(|error| {
            tracing::warn!(turn_id, %error, "Failed to diff turn snapshots");
            TurnWorkspaceReason::DeltaFailed
        });

        let turn = {
            let _turn_mutation = self.store.turn_mutation.lock();
            let mut turn = self.store.load_turn(turn_id)?;
            let Some(workspace) = turn
                .workspace
                .clone()
                .filter(|workspace| workspace.state == TurnWorkspaceState::Pending)
            else {
                return Ok(());
            };
            match delta {
                Ok((post, delta, tracked)) => {
                    let refs = turn_artifacts::delta_refs(
                        &delta,
                        restore_snapshot_id.as_deref(),
                        Utc::now(),
                    );
                    let delta = turn_artifacts::WorkspaceDelta {
                        refs,
                        tracked_item_paths: tracked,
                        truncated: delta.truncated,
                        omitted: delta.omitted,
                    };
                    let settled = TurnWorkspaceArtifacts {
                        state: TurnWorkspaceState::Settled,
                        post_turn_snapshot_id: Some(post),
                        ..workspace
                    };
                    self.set_turn_artifacts(&mut turn, Some(&delta), settled);
                }
                Err(reason) => {
                    let unavailable = TurnWorkspaceArtifacts {
                        state: TurnWorkspaceState::Unavailable,
                        reason: Some(reason),
                        ..workspace
                    };
                    self.set_turn_artifacts(&mut turn, None, unavailable);
                }
            }
            self.store.save_turn(&turn)?;
            turn
        };
        self.emit_event(
            thread_id,
            Some(turn_id),
            None,
            "turn.artifacts",
            json!({
                "turn_id": turn.id,
                "workspace": turn.workspace,
                "artifacts": turn.artifacts,
            }),
        )
        .await?;
        Ok(())
    }

    pub(super) fn attach_item_to_turn(&self, turn_id: &str, item_id: &str) -> Result<()> {
        let _turn_mutation = self.store.turn_mutation.lock();
        let mut turn = self.store.load_turn(turn_id)?;
        // A terminal settle must not be rewritten by a late stream item attach.
        if turn.status.is_active_work() && !turn.item_ids.iter().any(|id| id == item_id) {
            turn.item_ids.push(item_id.to_string());
            self.store.save_turn(&turn)?;
        }
        Ok(())
    }

    async fn is_interrupt_requested(&self, thread_id: &str, turn_id: &str) -> Result<bool> {
        let active = self.active.lock().await;
        let Some(state) = active.engines.get(thread_id) else {
            return Ok(false);
        };
        let Some(turn) = state.active_turn.as_ref() else {
            return Ok(false);
        };
        Ok(turn.turn_id == turn_id && turn.interrupt_requested)
    }

    pub(super) async fn active_turn_authority(
        &self,
        thread_id: &str,
        turn_id: &str,
        engine: &EngineHandle,
    ) -> Option<crate::core::engine::RuntimePermissionAuthority> {
        let active = self.active.lock().await;
        let state = active.engines.get(thread_id)?;
        let turn = state.active_turn.as_ref()?;
        if turn.turn_id != turn_id || turn.interrupt_requested {
            return None;
        }
        Some(engine.runtime_permission_authority())
    }

    #[cfg(test)]
    pub(super) async fn active_turn_flags(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Option<(bool, bool)> {
        let active = self.active.lock().await;
        let state = active.engines.get(thread_id)?;
        let turn = state.active_turn.as_ref()?;
        if turn.turn_id != turn_id {
            return None;
        }
        let authority = state.engine.runtime_permission_authority();
        Some((authority.auto_approve, authority.trust_mode))
    }

    pub(super) async fn active_turn_id(&self, thread_id: &str) -> Option<String> {
        let active = self.active.lock().await;
        active
            .engines
            .get(thread_id)?
            .active_turn
            .as_ref()
            .map(|turn| turn.turn_id.clone())
    }

    pub(super) fn approval_decision(
        auto_approve: bool,
        trust_mode: bool,
        requires_full_access: bool,
    ) -> RuntimeApprovalDecision {
        if !auto_approve {
            return RuntimeApprovalDecision::DenyTool;
        }
        if requires_full_access {
            if trust_mode {
                RuntimeApprovalDecision::RetryWithFullAccess
            } else {
                RuntimeApprovalDecision::DenyTool
            }
        } else {
            RuntimeApprovalDecision::ApproveTool
        }
    }

    pub(super) fn recover_interrupted_state(&self) -> Result<()> {
        // Unpublished history seeds go first: the pointer recomputation below
        // would otherwise publish their turns as recovered history (#6555).
        // A thread whose seed cannot be settled is held out of this whole
        // pass, records untouched, instead of failing every other thread.
        let quarantined = self.store.settle_seed_journals()?;
        let now = Utc::now();
        let mut threads = self
            .store
            .list_threads()?
            .into_iter()
            .filter(|thread| !quarantined.contains(&thread.id))
            .map(|thread| (thread.id.clone(), thread))
            .collect::<HashMap<_, _>>();
        let mut turns_by_thread: HashMap<String, Vec<TurnRecord>> = HashMap::new();
        let mut latest_turn_by_thread: HashMap<String, (DateTime<Utc>, String)> = HashMap::new();
        let mut changed_threads = HashSet::new();

        // First terminalize interrupted candidates. Keep every terminal turn
        // in the same one-pass grouping so already-terminal records whose
        // completion append failed are reconciled too.
        for mut turn in self.store.list_all_turns()? {
            if quarantined.contains(&turn.thread_id) {
                continue;
            }
            if !turn.routing_settlement {
                latest_turn_by_thread
                    .entry(turn.thread_id.clone())
                    .and_modify(|latest| {
                        if (turn.created_at, turn.id.as_str()) > (latest.0, latest.1.as_str()) {
                            *latest = (turn.created_at, turn.id.clone());
                        }
                    })
                    .or_insert_with(|| (turn.created_at, turn.id.clone()));
            }
            let mut thread_changed = false;
            let interrupted_candidate = matches!(
                turn.status,
                RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
            );
            let resume_interrupted_normalization = turn.status == RuntimeTurnStatus::Interrupted
                && turn.error.as_deref() == Some(RUNTIME_RESTART_REASON);
            if interrupted_candidate || resume_interrupted_normalization {
                // Items must reach their terminal state before the parent
                // turn. If a process stops during this loop, the still-live
                // parent makes the next recovery pass resume normalization.
                // Also repair stores written by older builds that committed
                // the interrupted parent first and crashed before its items.
                for item_id in &turn.item_ids {
                    let mut item = self.store.load_item(item_id)?;
                    if matches!(
                        item.status,
                        TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::InProgress
                    ) {
                        item.status = TurnItemLifecycleStatus::Interrupted;
                        item.ended_at = Some(now);
                        self.store.save_item(&item)?;
                        thread_changed = true;
                    }
                }
            }
            if interrupted_candidate {
                turn.status = RuntimeTurnStatus::Interrupted;
                if let Some(activity) = &mut turn.operation_activity {
                    activity.active.clear();
                }
                turn.error = Some(RUNTIME_RESTART_REASON.to_string());
                turn.ended_at = Some(now);
                if let Some(started_at) = turn.started_at {
                    let elapsed = now.signed_duration_since(started_at);
                    turn.duration_ms = Some(elapsed.num_milliseconds().max(0) as u64);
                }
                self.set_turn_artifacts(
                    &mut turn,
                    None,
                    TurnWorkspaceArtifacts::unavailable(TurnWorkspaceReason::RuntimeRestarted),
                );
                self.store.save_turn(&turn)?;
                thread_changed = true;
            } else if let Some(workspace) = turn
                .workspace
                .as_mut()
                .filter(|workspace| workspace.state == TurnWorkspaceState::Pending)
            {
                // The post-turn snapshot id died with the process. Keep the
                // item-derived aggregate rather than guess at a delta.
                workspace.state = TurnWorkspaceState::Unavailable;
                workspace.reason = Some(TurnWorkspaceReason::RuntimeRestarted);
                self.store.save_turn(&turn)?;
            }
            if thread_changed && let Some(thread) = threads.get_mut(&turn.thread_id) {
                thread.updated_at = now;
                changed_threads.insert(thread.id.clone());
            }
            if matches!(
                turn.status,
                RuntimeTurnStatus::Completed
                    | RuntimeTurnStatus::Failed
                    | RuntimeTurnStatus::Interrupted
                    | RuntimeTurnStatus::Canceled
            ) {
                turns_by_thread
                    .entry(turn.thread_id.clone())
                    .or_default()
                    .push(turn);
            }
        }

        // A crash can land after the turn record but before the thread's
        // latest-turn pointer for both ordinary and compaction admissions.
        // Recompute it from durable records on every recovery pass so detail
        // and subsequent mutation never hide an accepted/recovered turn.
        for thread in threads.values_mut() {
            let latest = latest_turn_by_thread
                .get(&thread.id)
                .map(|(_, turn_id)| turn_id.clone());
            if thread.latest_turn_id != latest {
                thread.latest_turn_id = latest;
                changed_threads.insert(thread.id.clone());
            }
        }

        for thread_id in changed_threads {
            if let Some(thread) = threads.get(&thread_id) {
                self.store.save_thread(thread)?;
            }
        }

        let mut recovery_receipts: HashMap<String, Vec<RecoveredTurnReceipt>> = HashMap::new();
        for (thread_id, mut turns) in turns_by_thread {
            // One streaming pass keeps only what recovery needs (completed
            // turns, terminal calls, tool requests), not the whole log: a
            // long thread's history is never held in memory at startup.
            let mut completed_turns = HashSet::new();
            let mut terminal_calls = HashSet::new();
            let mut requested = Vec::new();
            self.store.for_each_event(&thread_id, |event| {
                match event.event.as_str() {
                    "turn.completed" => {
                        if let Some(turn_id) = event.turn_id {
                            completed_turns.insert(turn_id);
                        }
                    }
                    "tool_call.resolved" | "tool_call.canceled" | "tool_call.timeout" => {
                        if let (Some(turn_id), Some(call_id)) = (
                            event.turn_id,
                            event.payload.get("call_id").and_then(|id| id.as_str()),
                        ) {
                            terminal_calls.insert((turn_id, call_id.to_string()));
                        }
                    }
                    "tool_call.requested" => requested.push(event),
                    _ => {}
                }
                Ok(())
            })?;
            let mut requests_by_turn: HashMap<String, Vec<DynamicToolCallParams>> = HashMap::new();
            for event in &requested {
                let Ok(params) =
                    serde_json::from_value::<DynamicToolCallParams>(event.payload.clone())
                else {
                    tracing::warn!(
                        thread_id,
                        seq = event.seq,
                        "Ignoring malformed dynamic-tool request during Runtime recovery"
                    );
                    continue;
                };
                if params.thread_id == thread_id
                    && !terminal_calls.contains(&(params.turn_id.clone(), params.call_id.clone()))
                {
                    requests_by_turn
                        .entry(params.turn_id.clone())
                        .or_default()
                        .push(params);
                }
            }

            turns.sort_by_key(|turn| turn.created_at);
            for turn in turns {
                let unresolved_dynamic_tools =
                    requests_by_turn.remove(&turn.id).unwrap_or_default();
                if completed_turns.contains(&turn.id) && unresolved_dynamic_tools.is_empty() {
                    continue;
                }
                recovery_receipts
                    .entry(thread_id.clone())
                    .or_default()
                    .push(RecoveredTurnReceipt {
                        unresolved_dynamic_tools,
                        turn,
                    });
            }
        }

        *self.recovery_receipts.lock() = recovery_receipts;

        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn install_test_engine(&self, thread_id: &str, engine: EngineHandle) -> Result<()> {
        let thread = self.get_thread(thread_id).await?;
        let config = self.read_config().clone();
        let route = self.resolved_route_for_thread(&config, &thread)?;
        {
            let mut selected = self.subagent_manager.lock().await;
            if let Some(manager) = selected.as_ref() {
                anyhow::ensure!(
                    Arc::ptr_eq(manager, &engine.subagent_manager()),
                    "fixture worker authority differs from Runtime owner"
                );
            } else {
                *selected = Some(engine.subagent_manager());
            }
        }
        let engine = self.route_engine_events(&thread, engine).await;
        let mut active = self.active.lock().await;
        active.engines.insert(
            thread_id.to_string(),
            ActiveThreadState {
                engine,
                active_turn: None,
                route_identity: route.identity,
                route_model: route.model,
                hook_executor: None,
                client_preflight_required: false,
            },
        );
        touch_lru(&mut active.lru, thread_id);
        Ok(())
    }
}

pub(super) fn dynamic_tool_result_text(content: &[DynamicToolCallContent]) -> String {
    content
        .iter()
        .map(|item| match item {
            DynamicToolCallContent::InputText { text } => text.clone(),
            DynamicToolCallContent::InputImage { image_url } => format!("[image] {image_url}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn dynamic_tool_result_to_tool_result(
    result: DynamicToolCallResult,
) -> crate::tools::spec::ToolResult {
    let text = dynamic_tool_result_text(&result.content);
    if result.success {
        crate::tools::spec::ToolResult::success(text)
    } else {
        crate::tools::spec::ToolResult::error(if text.is_empty() {
            "dynamic tool failed".to_string()
        } else {
            text
        })
    }
}

pub(super) fn dynamic_tool_terminal_payload(
    params: &DynamicToolCallParams,
    status: &str,
    success: Option<bool>,
    reason: Option<&str>,
) -> Value {
    let mut payload = json!({
        "thread_id": params.thread_id,
        "turn_id": params.turn_id,
        "call_id": params.call_id,
        "status": status,
    });
    if let Some(object) = payload.as_object_mut() {
        if let Some(success) = success {
            object.insert("success".to_string(), json!(success));
        }
        if let Some(reason) = reason {
            object.insert("reason".to_string(), json!(reason));
        }
    }
    payload
}

#[async_trait::async_trait]
impl crate::tools::spec::DynamicToolExecutor for RuntimeThreadManager {
    async fn execute_dynamic_tool(
        &self,
        thread_id: Option<String>,
        namespace: Option<String>,
        name: String,
        input: Value,
    ) -> std::result::Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError> {
        let thread_id = thread_id.ok_or_else(|| {
            crate::tools::spec::ToolError::not_available(format!(
                "runtime dynamic tool '{name}' has no active thread"
            ))
        })?;
        let turn_id = self.active_turn_id(&thread_id).await.ok_or_else(|| {
            crate::tools::spec::ToolError::not_available(format!(
                "runtime dynamic tool '{name}' has no active turn"
            ))
        })?;
        let call_id = runtime_record_id("call");
        let params = DynamicToolCallParams {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            call_id: call_id.clone(),
            namespace,
            tool: name.clone(),
            arguments: input,
        };
        let projection_lock = self.projection_lock(&thread_id);
        let projection = projection_lock.lock().await;
        let mut rx = self
            .register_pending_dynamic_tool(params.clone())
            .map_err(|err| crate::tools::spec::ToolError::execution_failed(err.to_string()))?;
        if let Err(err) = self
            .emit_event(
                &thread_id,
                Some(&turn_id),
                None,
                "tool_call.requested",
                json!(&params),
            )
            .await
        {
            self.remove_pending_dynamic_tool(&thread_id, &turn_id, &call_id);
            drop(projection);
            return Err(crate::tools::spec::ToolError::execution_failed(format!(
                "failed to emit runtime dynamic tool request for '{name}': {err}"
            )));
        }
        drop(projection);

        let result_timeout = self.dynamic_tool_result_timeout();
        match tokio::time::timeout(result_timeout, &mut rx).await {
            Ok(Ok(result)) => Ok(dynamic_tool_result_to_tool_result(result)),
            Ok(Err(_recv_err)) => Err(crate::tools::spec::ToolError::execution_failed(format!(
                "runtime dynamic tool '{name}' result channel closed"
            ))),
            Err(_timeout) => {
                let mut settlement_progress = match self
                    .claim_pending_dynamic_tool(&thread_id, &turn_id, &call_id)
                {
                    PendingDynamicToolClaim::Claimed(claim) => {
                        self.settle_dynamic_tool_timeout(claim, result_timeout)
                            .await
                            .map_err(|err| {
                                crate::tools::spec::ToolError::execution_failed(err.to_string())
                            })?;
                        return Err(crate::tools::spec::ToolError::Timeout {
                            seconds: result_timeout.as_secs(),
                        });
                    }
                    PendingDynamicToolClaim::Settling(progress) => progress,
                    PendingDynamicToolClaim::Indeterminate => {
                        return Err(crate::tools::spec::ToolError::execution_failed(format!(
                            "runtime dynamic tool '{name}' has an indeterminate terminal receipt"
                        )));
                    }
                    PendingDynamicToolClaim::Missing => {
                        return match rx.await {
                            Ok(result) => Ok(dynamic_tool_result_to_tool_result(result)),
                            Err(_recv_err) => Err(crate::tools::spec::ToolError::execution_failed(
                                format!("runtime dynamic tool '{name}' result channel closed"),
                            )),
                        };
                    }
                };

                // A result or turn cancellation claimed the call just before
                // the timer fired. Preserve that winner. Its supervised task
                // notifies this watcher on either durable completion or
                // rollback, so a panic/persistence error cannot strand this
                // executor in an unbounded `rx.await`.
                loop {
                    tokio::select! {
                        received = &mut rx => {
                            return match received {
                                Ok(result) => Ok(dynamic_tool_result_to_tool_result(result)),
                                Err(_recv_err) => Err(
                                    crate::tools::spec::ToolError::execution_failed(format!(
                                        "runtime dynamic tool '{name}' result channel closed"
                                    )),
                                ),
                            };
                        }
                        _ = settlement_progress.changed() => {
                            match self.claim_pending_dynamic_tool(
                                &thread_id,
                                &turn_id,
                                &call_id,
                            ) {
                                PendingDynamicToolClaim::Claimed(claim) => {
                                    self.settle_dynamic_tool_timeout(claim, result_timeout)
                                        .await
                                        .map_err(|err| {
                                            crate::tools::spec::ToolError::execution_failed(
                                                err.to_string(),
                                            )
                                        })?;
                                    return Err(crate::tools::spec::ToolError::Timeout {
                                        seconds: result_timeout.as_secs(),
                                    });
                                }
                                PendingDynamicToolClaim::Settling(progress) => {
                                    settlement_progress = progress;
                                }
                                PendingDynamicToolClaim::Indeterminate => {
                                    return Err(
                                        crate::tools::spec::ToolError::execution_failed(format!(
                                            "runtime dynamic tool '{name}' has an indeterminate terminal receipt"
                                        )),
                                    );
                                }
                                PendingDynamicToolClaim::Missing => {
                                    return match rx.await {
                                        Ok(result) => {
                                            Ok(dynamic_tool_result_to_tool_result(result))
                                        }
                                        Err(_recv_err) => Err(
                                            crate::tools::spec::ToolError::execution_failed(
                                                format!(
                                                    "runtime dynamic tool '{name}' result channel closed"
                                                ),
                                            ),
                                        ),
                                    };
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn touch_lru(lru: &mut VecDeque<String>, thread_id: &str) {
    if let Some(idx) = lru.iter().position(|id| id == thread_id) {
        lru.remove(idx);
    }
    lru.push_back(thread_id.to_string());
}

pub(super) fn enforce_lru_capacity(
    active: &mut ActiveThreads,
    max_active_threads: usize,
    pending_commands: &HashSet<String>,
) -> Vec<EngineHandle> {
    let mut evicted = Vec::new();
    if max_active_threads == 0 || active.engines.len() < max_active_threads {
        return evicted;
    }
    let protected = active
        .engines
        .iter()
        .filter_map(|(thread_id, state)| {
            let manager = state.engine.subagent_manager();
            let live_children = manager.try_read().map_or(true, |manager| {
                manager.live_count_for_session(thread_id) > 0
            });
            if state.active_turn.is_some() || live_children || pending_commands.contains(thread_id)
            {
                Some(thread_id.clone())
            } else {
                None
            }
        })
        .collect::<HashSet<_>>();

    let scan_limit = active.lru.len();
    for _ in 0..scan_limit {
        let Some(candidate) = active.lru.pop_front() else {
            break;
        };
        if protected.contains(&candidate) {
            active.lru.push_back(candidate);
            continue;
        }
        if let Some(state) = active.engines.remove(&candidate) {
            evicted.push(state.engine);
        }
        break;
    }
    evicted
}

/// Merge per-request compatibility inputs with a thread's canonical policy.
/// A mode-only edit must preserve the effective posture of a legacy record
/// even when that record predates `permission_posture`.
pub(super) fn runtime_policy_with_overrides(
    thread: &ThreadRecord,
    mode: Option<&str>,
    permission_posture: Option<&str>,
    auto_approve: Option<bool>,
) -> Result<RuntimePolicyProjection> {
    let requested_mode = mode.unwrap_or(&thread.mode);
    let legacy_bypass_mode = mode.is_some_and(codewhale_config::AppMode::is_legacy_bypass_alias);
    let inherited = RuntimePolicyProjection::from_persisted(
        &thread.mode,
        thread.permission_posture.as_deref(),
        thread.auto_approve,
    );
    let requested_permission = match permission_posture {
        Some(explicit) => Some(explicit),
        None if auto_approve.is_some() || legacy_bypass_mode => None,
        None => Some(inherited.permission_wire()),
    };
    RuntimePolicyProjection::from_request(requested_mode, requested_permission, auto_approve)
}

/// Compatibility parser retained for focused Runtime tests.
#[cfg(test)]
pub(super) fn parse_mode_opt(mode: &str) -> Option<codewhale_config::AppMode> {
    codewhale_runtime::runtime_policy::parse_runtime_mode(mode)
}

#[cfg(test)]
pub(super) fn parse_mode(mode: &str) -> codewhale_config::AppMode {
    parse_mode_opt(mode).unwrap_or(codewhale_config::AppMode::Agent)
}

/// `metadata.visibility` value for runtime items that are engine plumbing
/// (scheduler rows, tool discovery, deferred-schema hand-offs). Clients
/// collapse these by default; the durable receipt is kept.
pub const INTERNAL_ITEM_VISIBILITY: &str = "internal";

pub(super) fn tool_kind_for_name(name: &str) -> TurnItemKind {
    let lower = name.to_ascii_lowercase();
    if lower == "exec_shell" || lower == "exec_shell_wait" || lower == "exec_shell_interact" {
        return TurnItemKind::CommandExecution;
    }
    if lower.contains("patch") || lower.contains("write") || lower.contains("edit") {
        return TurnItemKind::FileChange;
    }
    TurnItemKind::ToolCall
}

pub fn tool_receipt_detail(name: &str, content: &str, secrets: &[String]) -> String {
    if name == REQUEST_USER_INPUT_TOOL_NAME {
        REDACTED_USER_INPUT_RECEIPT.to_string()
    } else {
        crate::client::redact_model_bound_text(content, secrets)
    }
}

pub async fn receipt_secret_values(config: Config) -> Result<Vec<String>> {
    #[cfg(any(test, feature = "test-support"))]
    let ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = crate::test_support::join_env_scope(ticket);
        crate::client::active_model_bound_secret_values(&config)
    })
    .await
    .context("Runtime receipt redaction setup failed")
}

pub fn summarize_text(text: &str, limit: usize) -> String {
    let take = limit.saturating_sub(3);
    let mut count = 0;
    let mut out = String::new();
    for ch in text.chars() {
        if count >= take {
            out.push_str("...");
            return out;
        }
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        out.push(ch);
        count += 1;
    }
    out
}

pub(super) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// One tool call's workspace span, as the call-change route serves it.
///
/// Both tree ids name restore points recorded on the calling turn, so the
/// thread owns them: the `pre_tool` one is what `file-revert` accepts for
/// every path the span changed.
#[derive(Debug, Clone)]
pub struct CallWorkspaceSpan {
    pub thread_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: Option<String>,
    /// The persisted item owns whether this call is still awaiting settlement.
    pub item_status: Option<TurnItemLifecycleStatus>,
    pub pre_tool_snapshot_id: Option<String>,
    /// `None` while the call is active, or when the closing snapshot failed
    /// or was gated: the span's changes are then unknown, not empty.
    pub post_tool_snapshot_id: Option<String>,
    pub thread_workspace: PathBuf,
}

/// A turn's artifact references as the Runtime API serves them.
#[derive(Debug, Clone, Serialize)]
pub struct TurnArtifactsView {
    pub thread_id: String,
    pub turn_id: String,
    /// `null` while the turn is still running.
    pub workspace: Option<TurnWorkspaceArtifacts>,
    pub artifacts: Vec<TurnArtifactRef>,
    /// Every item-level ref, including intermediate revisions of a file the
    /// turn wrote more than once. The read route resolves `?revision=`
    /// against these.
    #[serde(skip)]
    pub item_artifacts: Vec<TurnArtifactRef>,
    /// The thread workspace every file ref is relative to.
    #[serde(skip)]
    pub thread_workspace: PathBuf,
}

/// The pre-turn and post-turn restore points recorded on one turn: the
/// pair its workspace delta is diffed from.
pub(super) struct TurnSnapshotPair {
    pre: codewhale_snapshot::WorkspaceSnapshotRef,
    post: codewhale_snapshot::WorkspaceSnapshotRef,
}

/// The turn's first `pre_turn` and last `post_turn` receipts, when both were
/// recorded.
pub(super) fn turn_snapshot_pair(
    snapshots: &[codewhale_snapshot::WorkspaceSnapshotRef],
) -> Option<TurnSnapshotPair> {
    use codewhale_snapshot::WorkspaceSnapshotKind;
    let pre = snapshots
        .iter()
        .find(|snapshot| snapshot.kind == WorkspaceSnapshotKind::PreTurn)?;
    let post = snapshots
        .iter()
        .rev()
        .find(|snapshot| snapshot.kind == WorkspaceSnapshotKind::PostTurn)?;
    Some(TurnSnapshotPair {
        pre: pre.clone(),
        post: post.clone(),
    })
}

/// Diff the turn's snapshot pair in the existing side repo, and report
/// which item-recorded paths either snapshot can see.
pub(super) fn workspace_delta(
    workspace: &Path,
    pre: &str,
    post: &str,
    item_paths: &[String],
) -> Result<(codewhale_snapshot::SnapshotDelta, HashSet<String>)> {
    let repo = codewhale_snapshot::SnapshotRepo::open_existing(workspace)?
        .context("workspace snapshot repo is missing")?;
    let pre = codewhale_snapshot::SnapshotId::parse(pre)?;
    let post = codewhale_snapshot::SnapshotId::parse(post)?;
    let delta = repo.diff_snapshots(&pre, &post, turn_artifacts::MAX_TURN_ARTIFACTS)?;
    let mut tracked = repo.tracked_paths(&pre, item_paths)?;
    tracked.extend(repo.tracked_paths(&post, item_paths)?);
    Ok((delta, tracked))
}

#[cfg(any(test, feature = "test-support"))]
pub async fn install_mock_engine(
    manager: &RuntimeThreadManager,
    thread_id: &str,
) -> crate::core::engine::MockEngineHandle {
    let mut harness = crate::core::engine::mock_engine_handle();
    if let Some(authority) = manager.subagent_manager.lock().await.as_ref() {
        harness.handle.set_subagent_manager(authority.clone());
    }
    manager
        .install_test_engine(thread_id, harness.handle.clone())
        .await
        .expect("install mock engine");
    harness
}
