//! The one table that answers "where does a message with this role go on this
//! wire, and when is the pair not representable at all?"
//!
//! Before this module existed the question was answered once per adapter, and
//! the answers disagreed — not by design, by drift:
//!
//! * Chat Completions matched `user`/`assistant`/`system` and let anything
//!   else fall off the end of an `if`/`else if` chain, silently.
//! * OpenAI Responses matched `user`/`assistant`/`tool` and swallowed
//!   `system` in a catch-all `_ => {}`.
//! * Anthropic Messages forwarded `message.role` **verbatim**, so a `system`
//!   message earned an opaque provider-side 400 that named neither the role
//!   nor the message.
//!
//! Now each adapter asks [`role_placement`] which channel to render into, and
//! [`reject_unsupported_roles`] runs at the outbound seam
//! (`CodewhaleClient::prepare_outbound_request`) so an unrepresentable pair is
//! refused locally, before any transport serialization, instead of being
//! discovered by the provider.
//!
//! Two rules govern edits here:
//!
//! * A role is **omitted** only where dropping it was already the behaviour
//!   and the content is not load-bearing. Turning a hard error into a silent
//!   drop is a fail-open regression, not a cleanup.
//! * Placement grants no authority. It says which wire channel carries the
//!   bytes, never how much the model should trust them.

use super::prepared::WireDialect;
use codewhale_models::{Message, Role};

/// Which channel of a wire body a message renders into.
///
/// Adapters own the structural rendering for their own dialect — Chat's
/// `tool_calls` array, Responses' `function_call_output` items, and Anthropic's
/// content blocks. This enum only names the channel, so
/// that the *choice* of channel is made in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolePlacement {
    /// The wire's user/input channel.
    User,
    /// The wire's assistant/model output channel.
    Assistant,
    /// The assistant channel, with the interrupted-output prefix prepended to
    /// the replayed text so the model can see the turn was cut short.
    InterruptedAssistant,
    /// A system-role entry inside the transcript body (not the top-level
    /// system prompt, which every dialect carries separately).
    System,
    /// A developer-role entry inside the transcript body.
    Developer,
    /// Not representable on this wire, and dropped rather than sent. Every
    /// `Omitted` cell below is behaviour that already shipped.
    Omitted,
    /// Not representable and not safe to drop. The outbound seam refuses the
    /// request; adapters treat it as unreachable and fail closed if reached.
    Rejected,
}

impl RolePlacement {
    /// True when the message renders into the assistant channel, interrupted
    /// replay included.
    pub fn is_assistant_channel(self) -> bool {
        matches!(self, Self::Assistant | Self::InterruptedAssistant)
    }
}

/// The placement table.
///
/// Read the cells as the current, audited behaviour of each adapter. The two
/// cells that deliberately changed are marked; see the commit that introduced
/// this module.
pub fn role_placement(role: &Role, dialect: WireDialect) -> RolePlacement {
    match (role, dialect) {
        // Every dialect carries user input.
        (Role::User, _) => RolePlacement::User,

        // Every dialect carries assistant output.
        (Role::Assistant, _) => RolePlacement::Assistant,

        // Interrupted assistant text replays as assistant output with a marker.
        (Role::InterruptedAssistant, _) => RolePlacement::InterruptedAssistant,

        // Chat Completions and Responses both accept load-bearing system and
        // developer entries inside transcript history. Anthropic accepts only
        // user/assistant message roles, so it preserves the positioned content
        // by projecting those entries onto the user channel. Hoisting them to
        // the top-level system field or dropping them would reorder or delete
        // compaction and branch summaries.
        (Role::System, WireDialect::ChatCompletions | WireDialect::OpenAiResponses) => {
            RolePlacement::System
        }
        (Role::System, WireDialect::AnthropicMessages) => RolePlacement::User,

        (Role::Developer, WireDialect::ChatCompletions | WireDialect::OpenAiResponses) => {
            RolePlacement::Developer
        }
        (Role::Developer, WireDialect::AnthropicMessages) => RolePlacement::User,

        // A role this build does not know, e.g. from a transcript written by
        // a newer build. The OpenAI-shaped dialects already dropped these;
        // Anthropic sent them verbatim for the provider to reject.
        (Role::Unrecognized(_), WireDialect::ChatCompletions | WireDialect::OpenAiResponses) => {
            RolePlacement::Omitted
        }
        // CHANGED: was a verbatim pass-through ending in a provider 400.
        (Role::Unrecognized(_), WireDialect::AnthropicMessages) => RolePlacement::Rejected,
    }
}

/// A role/dialect pair this build refuses to put on the wire.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "message {index} has role {role:?}, which the {dialect} wire cannot represent; \
     the request was refused before it was sent"
)]
pub struct UnsupportedRoleForDialect {
    /// Index of the offending message in the outbound transcript.
    pub index: usize,
    /// The role as it appears in the transcript.
    pub role: String,
    /// Stable machine label for the wire dialect.
    pub dialect: &'static str,
}

/// Refuse an outbound transcript that a dialect cannot represent.
///
/// This is the validation seam: it runs inside
/// `CodewhaleClient::prepare_outbound_request`, before any dialect builds a
/// body, so no rejected pair ever reaches transport serialization.
pub fn reject_unsupported_roles(
    messages: &[Message],
    dialect: WireDialect,
) -> Result<(), UnsupportedRoleForDialect> {
    for (index, message) in messages.iter().enumerate() {
        if role_placement(&message.role, dialect) == RolePlacement::Rejected {
            return Err(UnsupportedRoleForDialect {
                index,
                role: message.role.to_string(),
                dialect: dialect.as_str(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RolePlacement, WireDialect, reject_unsupported_roles, role_placement};
    use codewhale_models::{ContentBlock, Message, Role};

    const DIALECTS: [WireDialect; 3] = [
        WireDialect::ChatCompletions,
        WireDialect::AnthropicMessages,
        WireDialect::OpenAiResponses,
    ];

    fn message(role: Role) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: "body".to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn user_and_assistant_are_carried_by_every_dialect() {
        for dialect in DIALECTS {
            assert_eq!(role_placement(&Role::User, dialect), RolePlacement::User);
            assert_eq!(
                role_placement(&Role::Assistant, dialect),
                RolePlacement::Assistant
            );
        }
    }

    #[test]
    fn interrupted_assistant_replays_on_every_supported_dialect() {
        for dialect in [
            WireDialect::ChatCompletions,
            WireDialect::AnthropicMessages,
            WireDialect::OpenAiResponses,
        ] {
            let placement = role_placement(&Role::InterruptedAssistant, dialect);
            assert_eq!(placement, RolePlacement::InterruptedAssistant);
            assert!(placement.is_assistant_channel());
        }
    }

    #[test]
    fn positioned_instruction_roles_are_preserved_where_each_dialect_can_carry_them() {
        assert_eq!(
            role_placement(&Role::System, WireDialect::ChatCompletions),
            RolePlacement::System
        );
        assert_eq!(
            role_placement(&Role::System, WireDialect::OpenAiResponses),
            RolePlacement::System
        );
        assert_eq!(
            role_placement(&Role::System, WireDialect::AnthropicMessages),
            RolePlacement::User
        );
        assert_eq!(
            role_placement(&Role::Developer, WireDialect::ChatCompletions),
            RolePlacement::Developer
        );
        assert_eq!(
            role_placement(&Role::Developer, WireDialect::OpenAiResponses),
            RolePlacement::Developer
        );
        assert_eq!(
            role_placement(&Role::Developer, WireDialect::AnthropicMessages),
            RolePlacement::User
        );
    }

    #[test]
    fn unknown_roles_never_reach_a_wire() {
        let role = Role::Unrecognized("future_role".to_string());
        for dialect in DIALECTS {
            assert_ne!(
                role_placement(&role, dialect),
                RolePlacement::User,
                "{} must not promote an unknown role",
                dialect.as_str()
            );
            assert!(matches!(
                role_placement(&role, dialect),
                RolePlacement::Omitted | RolePlacement::Rejected
            ));
        }
    }

    #[test]
    fn seam_accepts_positioned_system_history_on_anthropic() {
        let messages = vec![
            message(Role::User),
            message(Role::Assistant),
            message(Role::System),
        ];
        reject_unsupported_roles(&messages, WireDialect::AnthropicMessages)
            .expect("Anthropic projects positioned system history onto the user channel");
    }

    #[test]
    fn seam_accepts_what_each_dialect_can_carry() {
        let plain = vec![message(Role::User), message(Role::Assistant)];
        for dialect in DIALECTS {
            assert!(reject_unsupported_roles(&plain, dialect).is_ok());
        }
        // Omitted is not rejected: genuinely unknown roles keep the legacy
        // OpenAI-shaped behavior rather than failing a live session.
        assert!(
            reject_unsupported_roles(
                &[
                    message(Role::System),
                    message(Role::Developer),
                    message(Role::Unrecognized("x".into()))
                ],
                WireDialect::OpenAiResponses,
            )
            .is_ok()
        );
        assert!(
            reject_unsupported_roles(
                &[message(Role::Unrecognized("x".into()))],
                WireDialect::ChatCompletions,
            )
            .is_ok()
        );
    }
}

/// Each adapter, run over the same transcript, must land where the table says.
///
/// These tests exist because the four adapters used to disagree with each
/// other and nothing noticed. They assert the *observable wire shape*, not the
/// table — a future edit that keeps the table honest but forgets to rewire an
/// adapter fails here.
#[cfg(test)]
mod adapter_agreement_tests {
    use serde_json::{Value, json};

    use super::super::{anthropic, chat, responses};
    use codewhale_config::ProviderKind;
    use codewhale_models::{
        ContentBlock, INTERRUPTED_ASSISTANT_CONTEXT_PREFIX, Message, MessageRequest, Role,
    };

    fn message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn request(messages: Vec<Message>) -> MessageRequest {
        MessageRequest {
            model: "test-model".to_string(),
            messages,
            max_tokens: 256,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(false),
            temperature: None,
            top_p: None,
        }
    }

    fn roles(items: &[Value]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| item.get("role").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    fn transcript() -> Vec<Message> {
        vec![
            message(Role::User, "ask"),
            message(Role::Assistant, "answer"),
            message(Role::InterruptedAssistant, "half an answer"),
            message(Role::System, "compaction summary"),
            message(Role::Developer, "developer instruction"),
            message(Role::Unrecognized("future_role".to_string()), "from later"),
        ]
    }

    #[test]
    fn chat_completions_maps_every_role_the_table_says_it_can_carry() {
        let items = chat::build_chat_messages_for_request_and_provider(
            &request(transcript()),
            ProviderKind::Deepseek,
        );
        assert_eq!(
            roles(&items),
            vec!["user", "assistant", "assistant", "system", "developer"],
            "one wire entry per carried message, in transcript order: the \
             interrupted turn joins the assistant channel; positioned system \
             and developer history survive; the unknown role is dropped",
        );
        let rendered = serde_json::to_string(&items).expect("serialize");
        assert!(
            !rendered.contains("from later"),
            "an unknown role must not reach the wire: {rendered}"
        );
        assert!(rendered.contains("compaction summary"));
        assert!(rendered.contains("developer instruction"));
    }

    #[test]
    fn chat_completions_marks_interrupted_assistant_history() {
        let items = chat::build_chat_messages_for_request_and_provider(
            &request(vec![message(Role::InterruptedAssistant, "half an answer")]),
            ProviderKind::Deepseek,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["role"], json!("assistant"));
        assert_eq!(
            items[0]["content"].as_str().expect("text content"),
            format!("{INTERRUPTED_ASSISTANT_CONTEXT_PREFIX}half an answer"),
        );
    }

    #[test]
    fn responses_preserves_positioned_instruction_history_and_marks_interrupted_history() {
        let items = responses::convert_messages_to_responses_input(
            &request(transcript()),
            ProviderKind::Openai,
            None,
        );
        assert_eq!(
            roles(&items),
            vec!["user", "assistant", "assistant", "system", "developer"],
            "Responses preserves positioned system/developer history and drops unknown roles",
        );
        let rendered = serde_json::to_string(&items).expect("serialize");
        assert!(rendered.contains("compaction summary"), "{rendered}");
        assert!(rendered.contains("developer instruction"), "{rendered}");
        assert!(!rendered.contains("from later"), "{rendered}");
        assert_eq!(
            items[2]["content"][0]["text"]
                .as_str()
                .expect("output text"),
            format!("{INTERRUPTED_ASSISTANT_CONTEXT_PREFIX}half an answer"),
        );
    }

    /// Anthropic accepts only user/assistant message roles. Positioned system
    /// and developer history is projected onto user without moving or dropping
    /// its content; genuinely unknown roles are still refused at the seam.
    #[test]
    fn anthropic_never_emits_a_role_outside_user_and_assistant() {
        for role in [Role::System, Role::Developer] {
            let value = anthropic::message_to_anthropic(&message(role.clone(), "body"))
                .expect("positioned instruction history is carried");
            assert_eq!(value["role"], json!("user"), "{role}");
        }
        for role in [
            Role::Unrecognized("future_role".to_string()),
            Role::Unrecognized("tool".to_string()),
        ] {
            assert!(
                anthropic::message_to_anthropic(&message(role.clone(), "body")).is_none(),
                "anthropic must not put {role} on the wire",
            );
        }
        for (role, expected) in [(Role::User, "user"), (Role::Assistant, "assistant")] {
            let value =
                anthropic::message_to_anthropic(&message(role, "body")).expect("carried role");
            assert_eq!(value["role"], json!(expected));
        }
    }

    #[test]
    fn anthropic_marks_interrupted_assistant_history() {
        let value =
            anthropic::message_to_anthropic(&message(Role::InterruptedAssistant, "half an answer"))
                .expect("interrupted output replays as assistant");
        assert_eq!(value["role"], json!("assistant"));
        assert_eq!(
            value["content"][0]["text"].as_str().expect("text block"),
            format!("{INTERRUPTED_ASSISTANT_CONTEXT_PREFIX}half an answer"),
        );
    }
}
