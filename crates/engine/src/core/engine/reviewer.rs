//! Model-backed Auto-Review guardian tier (v0.9.8).
//!
//! The deterministic policy engine (see [`crate::core::authority::auto_review`]) decides
//! first: configured block rules and the built-in safety floor are hard
//! blocks that never reach a model. Only deterministic *fallback holds* — the
//! `AskUser` outcomes Auto posture would otherwise turn into bare permission
//! denials — escalate to a one-shot reviewer request. A denial returns the
//! rule and rationale while inviting another route or person approval,
//! and any reviewer failure (timeout, transport error, unparseable answer)
//! is a denial — fail closed. The reviewer is deliberately stateless: each
//! proposed call stands on its own deterministic context.

use std::time::Duration;

use crate::core::authority::auto_review::{
    AutoReviewAction, DEFAULT_GUARDIAN_POLICY, MAX_REVIEW_CONTEXT_BYTES, ReviewerRiskLevel,
    ReviewerVerdict, SUBAGENT_RETURN_GUARDIAN_POLICY, parse_reviewer_verdict,
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

/// The reviewer's answer, with failure modes separated from explicit denials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerOutcome {
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
    pub fn audit_decision(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::Deny { .. } => "deny",
            Self::Unavailable { .. } => "unavailable",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn audit_risk(&self) -> Option<&'static str> {
        match self {
            Self::Allow { risk, .. } | Self::Deny { risk, .. } => Some(risk.as_str()),
            Self::Unavailable { .. } | Self::Cancelled => None,
        }
    }

    pub fn into_tool_result(self, tool_name: &str) -> Result<String, ToolError> {
        self.into_tool_result_with_rule(tool_name, None)
    }

    pub fn into_tool_result_with_rule(
        self,
        _tool_name: &str,
        rule: Option<&str>,
    ) -> Result<String, ToolError> {
        match self {
            Self::Allow { reason, .. } => Ok(reason),
            Self::Deny { reason, .. } => Err(ToolError::permission_denied(
                crate::core::authority::auto_review::format_auto_review_block_message(
                    &reason,
                    rule.or(Some("guardian")),
                ),
            )),
            Self::Unavailable { reason } => Err(ToolError::permission_denied(format!(
                "Auto-Review guardian unavailable ({reason}); the call was denied (fail closed). Try again once the reviewer is reachable, or the person can switch Permissions to Ask where a prompt can be answered."
            ))),
            Self::Cancelled => Err(ToolError::cancelled(
                "Auto-Review guardian request cancelled",
            )),
        }
    }
}

/// One guardian review and its provider usage, when a request was dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerResult {
    pub outcome: ReviewerOutcome,
    pub usage: Option<Usage>,
    pub reviewer_model: String,
}

impl ReviewerResult {
    fn finish(outcome: ReviewerOutcome, usage: Option<Usage>, reviewer_model: String) -> Self {
        Self {
            outcome,
            usage,
            reviewer_model,
        }
    }

    fn unavailable(
        reason: impl Into<String>,
        usage: Option<Usage>,
        reviewer_model: String,
    ) -> Self {
        Self::finish(
            ReviewerOutcome::Unavailable {
                reason: reason.into(),
            },
            usage,
            reviewer_model,
        )
    }
}

pub fn resolve_reviewer_model(configured: Option<&str>, client: &dyn ModelClient) -> String {
    if let Some(explicit) = configured.map(str::trim).filter(|s| !s.is_empty()) {
        return explicit.to_string();
    }
    let provider = client
        .provider_name()
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    let candidates: &[&str] = match provider.as_str() {
        "anthropic" => &["claude-sonnet-5-5", "claude-sonnet-4-6"],
        "openai" => &["gpt-5.6", "gpt-5.5"],
        "openai-codex" => &["gpt-6.1-sol", "gpt-5.3-codex", "gpt-5.6"],
        "deepseek" | "deepseek-anthropic" => &["deepseek-v4-pro"],
        "google" => &["gemini-3.1-pro-preview", "gemini-4-argon"],
        "openrouter" => &["deepseek/deepseek-v4-pro"],
        "xai" => &["grok-4.7"],
        "zai" => &["GLM-5.3"],
        "moonshot" => &["kimi-k3.1-pro"],
        "minimax" | "minimax-anthropic" => &["MiniMax-M3"],
        _ => &[],
    };
    for &candidate in candidates {
        if client.supports_catalog_reviewer_model(candidate) {
            return candidate.to_string();
        }
    }
    client.model().to_string()
}

pub async fn consult_reviewer(
    client: &dyn ModelClient,
    context_text: &str,
    cancel_token: &CancellationToken,
) -> ReviewerResult {
    consult_reviewer_with_model_and_retry(client, None, context_text, cancel_token, false).await
}

pub async fn consult_reviewer_with_model(
    client: &dyn ModelClient,
    configured_model: Option<&str>,
    context_text: &str,
    cancel_token: &CancellationToken,
) -> ReviewerResult {
    consult_reviewer_with_model_and_retry(
        client,
        configured_model,
        context_text,
        cancel_token,
        false,
    )
    .await
}

pub async fn consult_reviewer_with_retry_approval(
    client: &dyn ModelClient,
    context_text: &str,
    cancel_token: &CancellationToken,
    person_approved_retry: bool,
) -> ReviewerResult {
    consult_reviewer_with_model_and_retry(
        client,
        None,
        context_text,
        cancel_token,
        person_approved_retry,
    )
    .await
}

pub async fn consult_subagent_return_reviewer(
    client: &dyn ModelClient,
    configured_model: Option<&str>,
    context_text: &str,
    cancel_token: &CancellationToken,
) -> ReviewerResult {
    consult_reviewer_inner(
        client,
        configured_model,
        SUBAGENT_RETURN_GUARDIAN_POLICY,
        context_text,
        cancel_token,
        false,
    )
    .await
}

pub async fn consult_reviewer_with_model_and_retry(
    client: &dyn ModelClient,
    configured_model: Option<&str>,
    context_text: &str,
    cancel_token: &CancellationToken,
    person_approved_retry: bool,
) -> ReviewerResult {
    consult_reviewer_inner(
        client,
        configured_model,
        DEFAULT_GUARDIAN_POLICY,
        context_text,
        cancel_token,
        person_approved_retry,
    )
    .await
}

async fn consult_reviewer_inner(
    client: &dyn ModelClient,
    configured_model: Option<&str>,
    system_policy: &str,
    context_text: &str,
    cancel_token: &CancellationToken,
    person_approved_retry: bool,
) -> ReviewerResult {
    let model = resolve_reviewer_model(configured_model, client);
    if context_text.len() > MAX_REVIEW_CONTEXT_BYTES {
        return ReviewerResult::unavailable(
            "the exact review context exceeded the guardian limit",
            None,
            model,
        );
    }
    let deepseek = client.provider_name() == codewhale_config::ProviderKind::Deepseek.as_str();
    let reasoning_effort = deepseek.then(|| "low".to_string());
    let temperature = reasoning_effort.is_none().then_some(0.0);
    let request = MessageRequest {
        model: model.clone(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: context_text.to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: REVIEWER_MAX_OUTPUT_TOKENS,
        system: Some(SystemPrompt::Text(system_policy.to_string())),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort,
        stream: Some(false),
        temperature,
        top_p: None,
        safety_check: None,
        title: None,
    };
    if cancel_token.is_cancelled() {
        return ReviewerResult::finish(ReviewerOutcome::Cancelled, None, model);
    }
    let response = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            return ReviewerResult::finish(ReviewerOutcome::Cancelled, None, model);
        }
        response = tokio::time::timeout(REVIEWER_TIMEOUT, client.create_message_uncached(request)) => response,
    };
    let response = match response {
        Err(_) => return ReviewerResult::unavailable("the reviewer timed out", None, model),
        Ok(Err(_)) => {
            return ReviewerResult::unavailable("the reviewer request failed", None, model);
        }
        Ok(Ok(response)) => response,
    };
    let outcome = verdict_from_response(&response, person_approved_retry);
    ReviewerResult::finish(outcome, Some(response.usage), model)
}

fn verdict_from_response(
    response: &MessageResponse,
    person_approved_retry: bool,
) -> ReviewerOutcome {
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
        }) if risk.may_auto_run() || person_approved_retry => {
            ReviewerOutcome::Allow { risk, reason }
        }
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
            title: None,
            stop_details: None,
            refusal: None,
        }
    }

    #[tokio::test]
    async fn guardian_rechecks_identical_calls_instead_of_reusing_a_cached_allow() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let client = crate::client::CodewhaleClient::new(
            &<codewhale_runtime::config::Config as Default>::default().with_legacy_root(
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
        assert!(system.contains("Judge the proposed call against"));
        assert!(system.contains("Never invent facts"));
        assert!(!system.contains("Never infer user intent"));
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
            verdict_from_response(
                &response(
                    r#"{"risk_level":"high","decision":"allow","reason":"the request mentions it"}"#,
                    Usage::default(),
                ),
                false,
            ),
            ReviewerOutcome::Deny {
                risk: ReviewerRiskLevel::High,
                ..
            }
        ));
        assert_eq!(
            verdict_from_response(
                &response(
                    r#"{"risk_level":"high","decision":"allow","reason":"person approved retry"}"#,
                    Usage::default(),
                ),
                true,
            ),
            ReviewerOutcome::Allow {
                risk: ReviewerRiskLevel::High,
                reason: "person approved retry".to_string(),
            }
        );
        assert!(matches!(
            verdict_from_response(
                &response(
                    r#"{"risk_level":"high","decision":"deny","reason":"still dangerous"}"#,
                    Usage::default(),
                ),
                true,
            ),
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

    #[test]
    fn block_texts_name_a_way_forward_without_full_access() {
        let deny = ReviewerOutcome::Deny {
            risk: ReviewerRiskLevel::High,
            reason: "writes outside the workspace".to_string(),
        }
        .into_tool_result("bash")
        .unwrap_err()
        .to_string();
        assert!(deny.contains("Auto-Review blocked this:"), "{deny}");
        assert!(deny.contains("writes outside the workspace"), "{deny}");
        assert!(deny.contains("[guardian]"), "{deny}");
        assert!(
            deny.contains("The person can approve it from the receipt"),
            "{deny}"
        );
        assert!(!deny.contains("do not work around"), "{deny}");
        assert!(!deny.contains("Full Access"), "{deny}");

        let unavailable = ReviewerOutcome::Unavailable {
            reason: "timeout".to_string(),
        }
        .into_tool_result("bash")
        .unwrap_err()
        .to_string();
        assert!(unavailable.contains("Permissions to Ask"), "{unavailable}");
        assert!(!unavailable.contains("Full Access"), "{unavailable}");
        assert!(!unavailable.contains("do not work around"), "{unavailable}");
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

    struct RouteStubClient {
        provider: &'static str,
        session_model: &'static str,
        supports_catalog: bool,
        seen_models: std::sync::Mutex<Vec<String>>,
    }

    impl RouteStubClient {
        fn new(
            provider: &'static str,
            session_model: &'static str,
            supports_catalog: bool,
        ) -> Self {
            Self {
                provider,
                session_model,
                supports_catalog,
                seen_models: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl ModelClient for RouteStubClient {
        fn provider_name(&self) -> &str {
            self.provider
        }

        fn model(&self) -> &str {
            self.session_model
        }

        fn supports_catalog_reviewer_model(&self, _candidate: &str) -> bool {
            self.supports_catalog
        }

        async fn create_message(&self, request: MessageRequest) -> anyhow::Result<MessageResponse> {
            self.seen_models
                .lock()
                .expect("seen_models lock")
                .push(request.model.clone());
            Ok(response(
                r#"{"risk_level":"low","decision":"allow","reason":"ok"}"#,
                Usage::default(),
            ))
        }

        async fn create_message_stream(
            &self,
            _request: MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            unreachable!()
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn resolve_reviewer_model_honors_override_matches_catalog_and_falls_back_to_session() {
        let custom = RouteStubClient::new("anthropic", "custom-claude", true);
        assert_eq!(
            resolve_reviewer_model(Some("  explicit-model "), &custom),
            "explicit-model"
        );
        assert_eq!(resolve_reviewer_model(None, &custom), "claude-sonnet-5-5");

        let expected_catalog = [
            ("anthropic", "claude-sonnet-5-5"),
            ("openai", "gpt-5.6"),
            ("openai-codex", "gpt-6.1-sol"),
            ("deepseek", "deepseek-v4-pro"),
            ("deepseek-anthropic", "deepseek-v4-pro"),
            ("google", "gemini-3.1-pro-preview"),
            ("openrouter", "deepseek/deepseek-v4-pro"),
            ("xai", "grok-4.7"),
            ("zai", "GLM-5.3"),
            ("moonshot", "kimi-k3.1-pro"),
            ("minimax", "MiniMax-M3"),
            ("minimax-anthropic", "MiniMax-M3"),
        ];
        for (provider, expected) in expected_catalog {
            let client = RouteStubClient::new(provider, "session-fallback", true);
            assert_eq!(
                resolve_reviewer_model(None, &client),
                expected,
                "provider={provider}"
            );
            assert_eq!(
                resolve_reviewer_model(Some("   "), &client),
                expected,
                "provider={provider}"
            );

            let unofficial = RouteStubClient::new(provider, "custom-route-model", false);
            assert_eq!(
                resolve_reviewer_model(None, &unofficial),
                "custom-route-model",
                "unofficial route must fall back to session model for {provider}"
            );
        }

        let ollama = RouteStubClient::new("ollama", "qwen3-coder", true);
        assert_eq!(resolve_reviewer_model(None, &ollama), "qwen3-coder");

        let consulted = consult_reviewer_with_model(
            &custom,
            Some("chosen-reviewer"),
            "{\"proposed_tool_call\":{}}",
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(consulted.reviewer_model, "chosen-reviewer");
        assert!(matches!(consulted.outcome, ReviewerOutcome::Allow { .. }));
        assert_eq!(
            custom.seen_models.lock().expect("seen_models").as_slice(),
            &["chosen-reviewer"]
        );
    }
}
