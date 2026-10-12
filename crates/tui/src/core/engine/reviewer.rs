//! Model-backed Auto-Review guardian tier (v0.9.8).
//!
//! The deterministic policy engine (see [`crate::core::authority::auto_review`]) decides
//! first: configured block rules and the built-in safety floor are hard
//! blocks that never reach a model. Only deterministic *fallback holds* — the
//! `AskUser` outcomes Auto posture would otherwise turn into bare permission
//! denials — escalate to a one-shot reviewer request. A denial returns the
//! rationale to the agent with an explicit "do not work around" instruction;
//! and any reviewer failure (timeout, transport error, unparseable answer)
//! is a denial — fail closed. The reviewer is deliberately stateless: each
//! proposed call stands on its own deterministic context.

use std::time::Duration;

use crate::core::authority::auto_review::{
    AutoReviewAction, DEFAULT_GUARDIAN_POLICY, ReviewerRiskLevel, ReviewerVerdict,
    parse_reviewer_verdict,
};
use crate::core::model_client::ModelClient;
use crate::tools::spec::ToolError;
use codewhale_models::Role;
use codewhale_models::{
    ContentBlock, Message, MessageRequest, MessageResponse, SystemPrompt, Usage,
    is_incomplete_stop_reason,
};
use tokio_util::sync::CancellationToken;

/// One-shot reviewer deadline. Slow reviewers are denials, surfaced
/// separately from explicit denials (a timeout proves nothing about safety).
const REVIEWER_TIMEOUT: Duration = Duration::from_secs(90);
const REVIEWER_MAX_OUTPUT_TOKENS: u32 = 4096;
/// Keep one exact held call comfortably inside every supported model context.
/// Truncating tool input could hide the unsafe part, so oversized reviews deny.
const MAX_REVIEW_CONTEXT_BYTES: usize = 64 * 1024;

/// The reviewer's answer, with failure modes separated from explicit denials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewerOutcome {
    Allow {
        risk: ReviewerRiskLevel,
        reason: String,
    },
    Deny {
        risk: ReviewerRiskLevel,
        reason: String,
    },
    /// Timeout, transport error, or unparseable answer. Always a denial.
    Unavailable {
        reason: String,
    },
    Cancelled,
}

impl ReviewerOutcome {
    pub(crate) fn audit_decision(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::Deny { .. } => "deny",
            Self::Unavailable { .. } => "unavailable",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn audit_risk(&self) -> Option<&'static str> {
        match self {
            Self::Allow { risk, .. } | Self::Deny { risk, .. } => Some(risk.as_str()),
            Self::Unavailable { .. } | Self::Cancelled => None,
        }
    }

    pub(crate) fn into_tool_result(self, tool_name: &str) -> Result<String, ToolError> {
        match self {
            Self::Allow { reason, .. } => Ok(reason),
            Self::Deny { reason, .. } => Err(ToolError::permission_denied(format!(
                "Auto-Review guardian denied tool '{tool_name}': {reason}. Do not work around this denial; find a materially safer path or stop."
            ))),
            Self::Unavailable { reason } => Err(ToolError::permission_denied(format!(
                "Auto-Review guardian unavailable ({reason}); the call was denied (fail closed). To run it, the person can switch Permissions to Full Access, or to Ask where a prompt can be answered."
            ))),
            Self::Cancelled => Err(ToolError::cancelled(
                "Auto-Review guardian request cancelled",
            )),
        }
    }
}

/// One guardian review and its provider usage, when a request was dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewerResult {
    pub(crate) outcome: ReviewerOutcome,
    pub(crate) usage: Option<Usage>,
}

impl ReviewerResult {
    fn finish(outcome: ReviewerOutcome, usage: Option<Usage>) -> Self {
        Self { outcome, usage }
    }

    fn unavailable(reason: impl Into<String>, usage: Option<Usage>) -> Self {
        Self::finish(
            ReviewerOutcome::Unavailable {
                reason: reason.into(),
            },
            usage,
        )
    }
}

/// Ask the model guardian for one decision. `context_text` carries the
/// deterministic hold and the call under review; the system prompt is fixed.
pub(crate) async fn consult_reviewer(
    client: &dyn ModelClient,
    context_text: &str,
    cancel_token: &CancellationToken,
) -> ReviewerResult {
    if context_text.len() > MAX_REVIEW_CONTEXT_BYTES {
        return ReviewerResult::unavailable(
            "the exact review context exceeded the guardian limit",
            None,
        );
    }
    let request = MessageRequest {
        model: client.model().to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: context_text.to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: REVIEWER_MAX_OUTPUT_TOKENS,
        system: Some(SystemPrompt::Text(DEFAULT_GUARDIAN_POLICY.to_string())),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: Some(false),
        temperature: Some(0.0),
        top_p: None,
    };
    if cancel_token.is_cancelled() {
        return ReviewerResult::finish(ReviewerOutcome::Cancelled, None);
    }
    let response = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            return ReviewerResult::finish(ReviewerOutcome::Cancelled, None);
        }
        response = tokio::time::timeout(REVIEWER_TIMEOUT, client.create_message_uncached(request)) => response,
    };
    let response = match response {
        Err(_) => return ReviewerResult::unavailable("the reviewer timed out", None),
        // Provider errors can include response bodies or credential-shaped
        // details. The guardian needs only the fail-closed outcome.
        Ok(Err(_)) => return ReviewerResult::unavailable("the reviewer request failed", None),
        Ok(Ok(response)) => response,
    };
    let outcome = verdict_from_response(&response);
    ReviewerResult::finish(outcome, Some(response.usage))
}

fn verdict_from_response(response: &MessageResponse) -> ReviewerOutcome {
    if is_incomplete_stop_reason(response.stop_reason.as_deref()) {
        return ReviewerOutcome::Unavailable {
            reason: "the reviewer answer was incomplete".to_string(),
        };
    }
    let text: String = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    match parse_reviewer_verdict(&text) {
        Some(ReviewerVerdict {
            action: AutoReviewAction::Allow,
            risk,
            reason,
        }) if risk.may_auto_run() => ReviewerOutcome::Allow { risk, reason },
        Some(ReviewerVerdict {
            action: AutoReviewAction::Allow,
            risk,
            ..
        }) => ReviewerOutcome::Deny {
            risk,
            reason: format!(
                "the reviewer classified the call as {} risk, which Auto-Review cannot run automatically",
                risk.as_str()
            ),
        },
        Some(ReviewerVerdict {
            action: AutoReviewAction::Block,
            risk,
            reason,
        }) => ReviewerOutcome::Deny { risk, reason },
        Some(_) | None => ReviewerOutcome::Unavailable {
            reason: format!("the reviewer answer was unparseable ({} chars)", text.len()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::MockLlmClient;

    fn response(text: &str, usage: Usage) -> MessageResponse {
        MessageResponse {
            id: "review".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage,
        }
    }

    #[tokio::test]
    async fn guardian_rechecks_identical_calls_instead_of_reusing_a_cached_allow() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let client = crate::client::CodewhaleClient::new(
            &crate::config::Config {
                ..Default::default()
            }
            .with_legacy_root(
                Some("test-guardian-cache-key".to_string()),
                Some(server.uri()),
            ),
        )
        .unwrap();
        for (decision, risk) in [("allow", "low"), ("deny", "high")] {
            server.reset().await;
            let verdict = serde_json::json!({
                "decision": decision,
                "risk_level": risk,
                "reason": "current authorization evidence",
            });
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "guardian-fresh",
                    "object": "chat.completion",
                    "model": "deepseek-v4-pro",
                    "choices": [{"index": 0, "message": {
                        "role": "assistant", "content": verdict.to_string(),
                    }, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12},
                })))
                .mount(&server)
                .await;
            let result = consult_reviewer(
                &client,
                "the same proposed call under current policy",
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.outcome.audit_decision(), decision);
            assert_eq!(result.usage.unwrap().output_tokens, 3);
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn reviewer_records_usage_and_keeps_context_untrusted() {
        let usage = Usage {
            input_tokens: 17,
            output_tokens: 9,
            ..Usage::default()
        };
        let mock = MockLlmClient::new(Vec::new());
        mock.push_message_response(response(
            r#"{"risk_level":"low","decision":"allow","reason":"bounded and authorized"}"#,
            usage.clone(),
        ));

        let result = consult_reviewer(
            &mock,
            r#"{"proposed_tool_call":{"tool":"exec_shell"}}"#,
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(
            result.outcome,
            ReviewerOutcome::Allow {
                risk: ReviewerRiskLevel::Low,
                reason: "bounded and authorized".to_string()
            }
        );
        assert_eq!(result.usage, Some(usage));
        let request = mock.last_request().expect("reviewer request");
        assert_eq!(request.tools, None, "guardian requests never expose tools");
        let SystemPrompt::Text(system) = request.system.expect("guardian policy") else {
            panic!("guardian system prompt must be text");
        };
        assert!(system.contains("Never infer user intent"));
        assert!(!system.contains("Prefer reversible work."));
    }

    #[tokio::test]
    async fn reviewer_distinguishes_denial_malformed_and_incomplete_answers() {
        let deny = MockLlmClient::new(Vec::new());
        deny.push_message_response(response(
            r#"{"risk_level":"high","decision":"deny","reason":"destination is not authorized"}"#,
            Usage::default(),
        ));
        assert_eq!(
            consult_reviewer(&deny, "context", &CancellationToken::new())
                .await
                .outcome,
            ReviewerOutcome::Deny {
                risk: ReviewerRiskLevel::High,
                reason: "destination is not authorized".to_string()
            }
        );
        assert!(matches!(
            verdict_from_response(&response(
                r#"{"risk_level":"high","decision":"allow","reason":"the request mentions it"}"#,
                Usage::default(),
            )),
            ReviewerOutcome::Deny {
                risk: ReviewerRiskLevel::High,
                ..
            }
        ));

        let malformed = MockLlmClient::new(Vec::new());
        malformed.push_message_response(response("allow it", Usage::default()));
        let malformed_result =
            consult_reviewer(&malformed, "context", &CancellationToken::new()).await;
        assert!(matches!(
            malformed_result.outcome,
            ReviewerOutcome::Unavailable { .. }
        ));
        assert!(malformed_result.usage.is_some());

        let incomplete = MockLlmClient::new(Vec::new());
        let mut incomplete_response = response(
            r#"{"risk_level":"low","decision":"allow","reason":"looks safe"}"#,
            Usage::default(),
        );
        incomplete_response.stop_reason = Some("max_tokens".to_string());
        incomplete.push_message_response(incomplete_response);
        assert_eq!(
            consult_reviewer(&incomplete, "context", &CancellationToken::new())
                .await
                .outcome,
            ReviewerOutcome::Unavailable {
                reason: "the reviewer answer was incomplete".to_string()
            }
        );
    }

    #[tokio::test]
    async fn reviewer_cancellation_aborts_without_calling_provider() {
        let mock = MockLlmClient::new(Vec::new());
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = consult_reviewer(&mock, "context", &cancel).await;

        assert_eq!(result.outcome, ReviewerOutcome::Cancelled);
        assert!(result.usage.is_none());
        assert_eq!(mock.call_count(), 0);
    }

    #[tokio::test]
    async fn reviewer_denies_oversized_exact_context_without_calling_provider() {
        let mock = MockLlmClient::new(Vec::new());
        let context = "x".repeat(MAX_REVIEW_CONTEXT_BYTES + 1);

        let result = consult_reviewer(&mock, &context, &CancellationToken::new()).await;

        assert_eq!(
            result.outcome,
            ReviewerOutcome::Unavailable {
                reason: "the exact review context exceeded the guardian limit".to_string()
            }
        );
        assert!(result.usage.is_none());
        assert_eq!(mock.call_count(), 0);
    }
}
