//! Bounded, redacted, read-only transcript peek for the dashboard (#4397).
//!
//! The dashboard needs to show *what a saved session was about* without
//! becoming a second transcript viewer. Three constraints shape this module,
//! and all three are enforced here rather than in the client:
//!
//! * **Bounded on the wire.** The peek carries at most
//!   [`MAX_PEEK_ENTRIES`] entries of at most [`MAX_ENTRY_CHARS`] characters.
//!   Doing this client-side would mean shipping a multi-megabyte transcript to
//!   a browser in order to throw most of it away.
//! * **Redacted.** A saved transcript can contain an API key a user pasted, a
//!   token echoed by a tool, an `Authorization` header in a curl command. The
//!   dashboard is reachable over a LAN; a peek pane is not the place to
//!   re-emit those.
//! * **Read-only and non-live.** A peek is a recording. It carries no turn
//!   status, no "running" flag, nothing that could be mistaken for live state.
//!   Live state comes from a resumed thread and its SSE stream, never from
//!   here — see `runtime_web/app.mjs`'s reply-target rules.
//!
//! Tool payloads are summarised to a kind and a size, never inlined: a tool
//! result is the most likely place for both bulk and secrets.

use serde::Serialize;

use crate::session_manager::SavedSession;
use codewhale_models::ContentBlock;

/// Most entries a peek carries. The dashboard shows a tail, so this is "the
/// last N exchanges", which is what a peek is for.
pub const MAX_PEEK_ENTRIES: usize = 12;

/// Longest text any single entry carries.
pub const MAX_ENTRY_CHARS: usize = 400;

/// What produced an entry. Deliberately coarse — the peek is not a
/// reconstruction of the turn structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeekEntryKind {
    User,
    Assistant,
    Reasoning,
    /// A tool call or result, summarised. Never the payload itself.
    Tool,
}

/// One bounded line of recorded conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeekEntry {
    pub kind: PeekEntryKind,
    /// Already bounded and redacted. Safe to render as text — and only as
    /// text; the client inserts it with `textContent`, never `innerHTML`.
    pub text: String,
    /// True when [`Self::text`] was shortened.
    pub truncated: bool,
    /// True when at least one redaction was applied.
    pub redacted: bool,
}

/// A read-only view of a saved session.
///
/// Note what is absent: no turn status, no `active`, no `running`. A saved
/// session has none of those, and inventing them is the fabricated-live-state
/// failure this whole slice exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionPeek {
    pub session_id: String,
    pub title: String,
    pub workspace: std::path::PathBuf,
    pub model: String,
    pub mode: String,
    pub archived: bool,
    /// Messages of conversation, runtime control traffic excluded. The peek
    /// never shows that traffic, so counting it here would print a total the
    /// pane cannot account for.
    pub message_count: usize,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Entries actually carried, oldest-first within the tail.
    pub entries: Vec<PeekEntry>,
    /// How many messages were dropped from the front to fit the bound. The
    /// client shows this rather than implying it has the whole conversation.
    pub omitted_before: usize,
    /// Always true: a peek is a recording of a saved session, never a live
    /// thread. Serialised so a client cannot mistake one payload for the
    /// other even by accident.
    pub live: bool,
}

/// Build a bounded, redacted peek from a loaded session.
#[must_use]
pub fn build_peek(session: &SavedSession, max_entries: usize) -> SessionPeek {
    let max_entries = max_entries.clamp(1, MAX_PEEK_ENTRIES);
    // Runtime control traffic is persisted with `role = "user"` because strict
    // chat templates reject anything else mid-conversation — see
    // `runtime_handoff`, which owns both the envelope and its recognition.
    // Rendering that transport role would attribute the runtime's own
    // bookkeeping to the person, and in a session with busy sub-agents it is
    // most of what the pane would show. Drop it before the tail is taken:
    // filtering afterwards spends the entry budget on rows nobody sees.
    let conversation: Vec<_> = session
        .messages
        .iter()
        .filter(|message| !crate::runtime_handoff::is_internal_runtime_handoff(message))
        .collect();
    let total = conversation.len();
    let start = total.saturating_sub(max_entries);

    let entries: Vec<PeekEntry> = conversation[start..]
        .iter()
        .map(|message| {
            let kind = match message.role.as_str() {
                "user" => PeekEntryKind::User,
                _ => PeekEntryKind::Assistant,
            };
            entry_for_blocks(kind, &message.content)
        })
        .collect();

    SessionPeek {
        session_id: session.metadata.id.clone(),
        title: session.metadata.title.clone(),
        workspace: session.metadata.workspace.clone(),
        model: session.metadata.model.clone(),
        mode: session
            .metadata
            .mode
            .clone()
            .unwrap_or_else(|| "agent".to_string()),
        archived: session.metadata.archived,
        message_count: total,
        updated_at: session.metadata.updated_at,
        entries,
        omitted_before: start,
        live: false,
    }
}

fn entry_for_blocks(default_kind: PeekEntryKind, blocks: &[ContentBlock]) -> PeekEntry {
    let mut kind = default_kind;
    let mut parts: Vec<String> = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => parts.push(text.trim().to_string()),
            ContentBlock::Thinking { thinking, .. } => {
                kind = PeekEntryKind::Reasoning;
                parts.push(thinking.trim().to_string());
            }
            // Tool traffic is summarised, never inlined: it is the most
            // likely carrier of both bulk output and credentials.
            ContentBlock::ToolUse { name, .. } | ContentBlock::ServerToolUse { name, .. } => {
                kind = PeekEntryKind::Tool;
                parts.push(format!("[tool call: {name}]"));
            }
            ContentBlock::ToolResult { content, .. } => {
                kind = PeekEntryKind::Tool;
                parts.push(format!("[tool result: {} chars]", content.chars().count()));
            }
            // Structured tool results are JSON. Report their serialized size
            // rather than their shape: the size is the honest number, and any
            // field of the payload could be a credential.
            ContentBlock::ToolSearchToolResult { content, .. }
            | ContentBlock::CodeExecutionToolResult { content, .. } => {
                kind = PeekEntryKind::Tool;
                parts.push(format!(
                    "[tool result: {} chars]",
                    content.to_string().len()
                ));
            }
            ContentBlock::ImageUrl { .. } => {
                kind = PeekEntryKind::Tool;
                parts.push("[image]".to_string());
            }
        }
    }

    let joined = parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (text, redacted) = redact(&joined);
    let (text, truncated) = bound(&text, MAX_ENTRY_CHARS);

    PeekEntry {
        kind,
        text,
        truncated,
        redacted,
    }
}

fn bound(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    (format!("{kept}…"), true)
}

/// Placeholder substituted for anything that looks like a credential.
pub const REDACTED_PLACEHOLDER: &str = "[redacted]";

/// Mask credential-shaped substrings.
///
/// The shared export sanitizer ([`codewhale_secrets::sanitize::sanitize_text`],
/// also used by `/export`) runs first: it covers `Bearer <token>`, JWTs,
/// URL credentials, PEM blocks, and quoted JSON/TOML keyed secrets. A
/// token-level pass then masks the extra bare prefixes (AWS, Google, Hugging
/// Face) and long opaque runs a transcript tends to carry. Over-redacting a
/// peek line is cheap; leaking a key over a LAN is not.
#[must_use]
pub fn redact(text: &str) -> (String, bool) {
    let sanitized = codewhale_secrets::sanitize::sanitize_text(text);
    // The sanitizer also strips control bytes and normalises URLs, so compare
    // placeholder counts rather than bytes to decide whether it redacted.
    let mut redacted = redaction_marks(&sanitized) > redaction_marks(text);
    let mut out = String::with_capacity(sanitized.len());

    for token in sanitized.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        let trailing = &token[trimmed.len()..];
        // Keep closing quotes and punctuation outside the mask so a masked
        // token inside `"…"` or `(…)` does not swallow its delimiter.
        let core = trimmed.trim_end_matches(['"', '\'', '`', ',', ';', ')', ']', '}']);
        let closing = &trimmed[core.len()..];
        if looks_like_secret(core) {
            out.push_str(REDACTED_PLACEHOLDER);
            out.push_str(closing);
            out.push_str(trailing);
            redacted = true;
        } else if let Some(masked) = mask_assignment(core) {
            out.push_str(&masked);
            out.push_str(closing);
            out.push_str(trailing);
            redacted = true;
        } else {
            out.push_str(token);
        }
    }

    (out, redacted)
}

fn redaction_marks(text: &str) -> usize {
    text.matches("[redacted").count() + text.matches("***").count()
}

/// Known credential prefixes plus long opaque runs.
fn looks_like_secret(token: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_",
        "pk_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "ASIA",
        "AIza",
        "hf_",
        "Bearer",
    ];
    if PREFIXES
        .iter()
        .any(|prefix| token.len() > prefix.len() && token.starts_with(prefix))
    {
        return true;
    }
    // A long unbroken run of base64/hex-ish characters is almost never prose.
    token.len() >= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '_')
        && token.chars().any(|c| c.is_ascii_digit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}

/// `key=value`, `token: value`, `--password value` style assignments.
fn mask_assignment(token: &str) -> Option<String> {
    const KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "api-key",
        "token",
        "secret",
        "password",
        "passwd",
        "authorization",
        "auth",
        "credential",
    ];
    let (name, sep_index) = token
        .find('=')
        .map(|i| (&token[..i], i))
        .or_else(|| token.find(':').map(|i| (&token[..i], i)))?;
    let normalized = name.trim_start_matches('-').to_ascii_lowercase();
    if !KEYS.contains(&normalized.as_str()) {
        return None;
    }
    if token[sep_index + 1..].trim().is_empty() {
        return None;
    }
    Some(format!(
        "{}{}{REDACTED_PLACEHOLDER}",
        name,
        &token[sep_index..=sep_index]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::create_saved_session_with_id_and_mode;
    use codewhale_models::Message;
    use codewhale_models::Role;

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }
    }

    fn session_with(messages: Vec<Message>) -> SavedSession {
        create_saved_session_with_id_and_mode(
            "peek-session".to_string(),
            &messages,
            "deepseek-chat",
            std::path::Path::new("/repo"),
            10,
            None,
            Some("agent"),
        )
    }

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![text_block(text)],
        }
    }

    #[test]
    fn peek_is_bounded_in_entries_and_reports_what_it_dropped() {
        let messages: Vec<Message> = (0..40).map(|i| user(&format!("message {i}"))).collect();
        let peek = build_peek(&session_with(messages), MAX_PEEK_ENTRIES);

        assert_eq!(peek.entries.len(), MAX_PEEK_ENTRIES);
        assert_eq!(peek.message_count, 40);
        assert_eq!(peek.omitted_before, 40 - MAX_PEEK_ENTRIES);
        assert!(
            peek.entries.last().expect("tail").text.contains("39"),
            "the peek must be the tail, not the head"
        );
    }

    #[test]
    fn a_request_for_more_than_the_cap_still_gets_the_cap() {
        let messages: Vec<Message> = (0..100).map(|i| user(&format!("m{i}"))).collect();
        let peek = build_peek(&session_with(messages), usize::MAX);
        assert_eq!(peek.entries.len(), MAX_PEEK_ENTRIES);
    }

    #[test]
    fn long_entries_are_truncated_and_flagged() {
        let peek = build_peek(&session_with(vec![user(&"x".repeat(5_000))]), 4);
        let entry = &peek.entries[0];
        assert!(entry.truncated);
        assert!(entry.text.chars().count() <= MAX_ENTRY_CHARS);
    }

    #[test]
    fn credentials_are_redacted_out_of_peek_text() {
        for secret in [
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "ghp_abcdefghijklmnopqrstuvwxyz1234",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            let peek = build_peek(&session_with(vec![user(&format!("here: {secret}"))]), 4);
            let entry = &peek.entries[0];
            assert!(entry.redacted, "{secret} should have been redacted");
            assert!(
                !entry.text.contains(secret),
                "peek leaked {secret}: {}",
                entry.text
            );
            assert!(entry.text.contains(REDACTED_PLACEHOLDER));
        }
    }

    #[test]
    fn assignment_style_secrets_are_masked_but_keep_their_key() {
        let (masked, redacted) = redact("api_key=hunter2 and password:swordfish");
        assert!(redacted);
        assert!(masked.contains("api_key="));
        assert!(!masked.contains("hunter2"));
        assert!(!masked.contains("swordfish"));
    }

    #[test]
    fn bearer_jwt_and_quoted_json_secrets_are_redacted() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXZhbHVl";
        let bearer = format!("Authorization: Bearer {jwt}");
        let json_key = r#"{"api_key": "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"}"#;
        for (input, secret) in [
            (bearer.as_str(), jwt),
            (
                json_key,
                "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
            ),
            (
                "run: curl https://user:hunter2pass@api.example/v1 now",
                "hunter2pass",
            ),
        ] {
            let (out, redacted) = redact(input);
            assert!(redacted, "{input} should have been redacted: {out}");
            assert!(!out.contains(secret), "peek leaked {secret}: {out}");
        }
    }

    #[test]
    fn a_masked_token_keeps_its_closing_quote() {
        let (out, redacted) =
            redact(r#"curl -H "X-Key: ghp_abcdefghijklmnopqrstuvwxyz1234" https://api.example"#);
        assert!(redacted);
        assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz1234"), "{out}");
        assert!(out.contains(r#"[redacted]" https://api.example"#), "{out}");
    }

    #[test]
    fn ordinary_prose_is_not_redacted() {
        let (out, redacted) = redact("Please refactor the lane registry and update the docs.");
        assert!(!redacted);
        assert_eq!(
            out,
            "Please refactor the lane registry and update the docs."
        );
    }

    #[test]
    fn tool_payloads_are_summarised_never_inlined() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call-1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({ "path": "/etc/shadow" }),
                    caller: None,
                    thought_signature: None,
                },
                ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call-1".to_string(),
                    content: "root:$6$verysecrethash".to_string(),
                    is_error: None,
                    content_blocks: None,
                },
            ],
        };
        let peek = build_peek(&session_with(vec![message]), 4);
        let entry = &peek.entries[0];

        assert_eq!(entry.kind, PeekEntryKind::Tool);
        assert!(entry.text.contains("[tool call: read_file]"));
        assert!(entry.text.contains("[tool result:"));
        assert!(
            !entry.text.contains("verysecrethash"),
            "tool output must never be inlined into a peek: {}",
            entry.text
        );
        assert!(
            !entry.text.contains("/etc/shadow"),
            "tool input must not be inlined either: {}",
            entry.text
        );
    }

    #[test]
    fn a_peek_never_claims_to_be_live() {
        let peek = build_peek(&session_with(vec![user("hello")]), 4);
        assert!(!peek.live, "a saved session is a recording, never live");
        let json = serde_json::to_value(&peek).expect("serialize");
        for forbidden in ["status", "running", "active", "turn"] {
            assert!(
                json.get(forbidden).is_none(),
                "peek payload must not carry a `{forbidden}` field a client could read as live state"
            );
        }
    }

    #[test]
    fn archive_state_rides_along_so_the_dashboard_need_not_guess() {
        let mut session = session_with(vec![user("hello")]);
        session.metadata.archived = true;
        assert!(build_peek(&session, 4).archived);
    }

    /// Every runtime handoff shape that can reach a saved session, including
    /// the restore checkpoints a post-resume save persists.
    fn runtime_handoffs() -> Vec<(&'static str, Message)> {
        let waiting = crate::runtime_handoff::waiting_for_subagents_runtime_message(2);
        let restored =
            crate::runtime_handoff::project_owned_messages_for_restore(vec![waiting.clone()]);
        vec![
            ("waiting_for_subagents", waiting),
            (
                "background_shell_completion",
                crate::runtime_handoff::shell_completion_runtime_message(&[]),
            ),
            (
                "restored_checkpoint",
                restored.into_iter().next().expect("projected"),
            ),
        ]
    }

    #[test]
    fn internal_runtime_events_are_absent_from_a_peek() {
        for (kind, handoff) in runtime_handoffs() {
            let peek = build_peek(
                &session_with(vec![user("ship the release"), handoff]),
                MAX_PEEK_ENTRIES,
            );

            assert_eq!(
                peek.entries.len(),
                1,
                "{kind} was rendered as conversation: {:?}",
                peek.entries
            );
            assert_eq!(peek.entries[0].text, "ship the release");
            for entry in &peek.entries {
                assert!(
                    !entry.text.contains("<codewhale:runtime_event"),
                    "{kind} leaked its envelope into a peek entry: {}",
                    entry.text
                );
                assert!(
                    !entry.text.contains("[Codewhale restored"),
                    "{kind} leaked a restore checkpoint into a peek entry: {}",
                    entry.text
                );
            }
        }
    }

    #[test]
    fn real_user_messages_survive_the_runtime_filter() {
        let mut messages = vec![user("first")];
        for (_, handoff) in runtime_handoffs() {
            messages.push(handoff);
        }
        messages.push(user("second"));

        let peek = build_peek(&session_with(messages), MAX_PEEK_ENTRIES);

        let texts: Vec<&str> = peek.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second"]);
        assert!(peek.entries.iter().all(|e| e.kind == PeekEntryKind::User));
    }

    #[test]
    fn a_person_who_pastes_a_runtime_envelope_is_still_the_person_talking() {
        // The filter keys on runtime provenance, never on the envelope text.
        // A composer turn is `ExternalUser`, whose authority is implicit, so
        // its metadata carries no provenance line — which is what keeps the
        // second shape here visible even though it is block-for-block what a
        // handoff looks like.
        let question = "why did I get <codewhale:runtime_event \
                        kind=\"waiting_for_subagents\" visibility=\"internal\"> \
                        in my transcript?";
        let pastes = [
            user(question),
            Message {
                role: Role::User,
                content: vec![
                    text_block(question),
                    text_block(concat!(
                        "<turn_meta>\n",
                        "Current approval mode: on-request\n",
                        "</turn_meta>",
                    )),
                ],
            },
        ];

        for paste in pastes {
            let peek = build_peek(&session_with(vec![paste]), MAX_PEEK_ENTRIES);

            assert_eq!(peek.entries.len(), 1);
            assert_eq!(peek.entries[0].kind, PeekEntryKind::User);
            assert!(peek.entries[0].text.contains("why did I get"));
        }
    }

    /// Rebuild a handoff the way the engine's ordinary send path does, with
    /// the blocks `user_content_blocks` inserts for an `[Attached image: …]`
    /// line in the payload. Idle completions take that path rather than
    /// `runtime_handoff_message_with_meta`, so this shape reaches saved
    /// sessions too.
    fn with_attachment_blocks(handoff: &Message) -> Message {
        let (
            ContentBlock::Text { text: envelope, .. },
            Some(ContentBlock::Text { text: meta, .. }),
        ) = (&handoff.content[0], handoff.content.last())
        else {
            panic!("handoff should be text-anchored");
        };
        Message {
            role: handoff.role.clone(),
            content: vec![
                text_block(envelope),
                text_block("<image path=\"/tmp/shot.png\">"),
                ContentBlock::ImageUrl {
                    image_url: codewhale_models::ImageUrlContent {
                        url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    },
                },
                text_block("</image>"),
                text_block(meta),
            ],
        }
    }

    #[test]
    fn a_handoff_that_carried_an_attachment_is_still_recognized() {
        for (kind, handoff) in runtime_handoffs() {
            let expanded = with_attachment_blocks(&handoff);
            let peek = build_peek(
                &session_with(vec![user("look at this"), expanded]),
                MAX_PEEK_ENTRIES,
            );

            assert_eq!(
                peek.entries.len(),
                1,
                "{kind} leaked once its payload mentioned an attachment: {:?}",
                peek.entries
            );
            assert_eq!(peek.entries[0].text, "look at this");
        }
    }

    #[test]
    fn runtime_traffic_does_not_spend_the_entry_budget() {
        // Filtering after the tail was taken would leave a session with chatty
        // sub-agents showing two or three lines of conversation out of twelve.
        let mut messages = Vec::new();
        for i in 0..MAX_PEEK_ENTRIES {
            messages.push(user(&format!("message {i}")));
            messages.push(crate::runtime_handoff::shell_completion_runtime_message(&[]));
        }

        let peek = build_peek(&session_with(messages), MAX_PEEK_ENTRIES);

        assert_eq!(peek.entries.len(), MAX_PEEK_ENTRIES);
        assert!(peek.entries[0].text.contains("message 0"));
        assert!(
            peek.entries
                .last()
                .expect("tail")
                .text
                .contains(&format!("message {}", MAX_PEEK_ENTRIES - 1))
        );
    }

    #[test]
    fn the_counters_describe_what_the_pane_can_show() {
        // The dashboard prints both numbers next to the rows it rendered, so a
        // count that included hidden runtime traffic would not add up.
        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(user(&format!("m{i}")));
            messages.push(crate::runtime_handoff::waiting_for_subagents_runtime_message(1));
        }

        let peek = build_peek(&session_with(messages), MAX_PEEK_ENTRIES);

        assert_eq!(peek.message_count, 20);
        assert_eq!(
            peek.omitted_before + peek.entries.len(),
            peek.message_count,
            "omitted + shown must account for every message the peek claims"
        );
    }
}
