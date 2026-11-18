use std::sync::Arc;

use codewhale_account_cli::{AccountClient, ClaudeOrgError, ClaudeOrgErrorKind, ClaudeOrgToken};
use codewhale_config::credentials::CLAUDE_ORG_LINK_COMMAND;
use reqwest::header::{HeaderName, HeaderValue};
use tokio::sync::Mutex;

use crate::llm_client::LlmError;

pub(super) type ClaudeOrgTokenSource =
    Arc<dyn Fn() -> std::result::Result<ClaudeOrgToken, ClaudeOrgError> + Send + Sync>;

pub(super) struct ClaudeOrgAuth {
    source: ClaudeOrgTokenSource,
    route_base_url: String,
    cached: Mutex<Option<ClaudeOrgToken>>,
}

impl ClaudeOrgAuth {
    pub(super) fn for_profile(profile: Option<String>, route_base_url: String) -> Self {
        Self::with_source(
            Arc::new(move || {
                AccountClient::new(profile.as_deref(), None)
                    .map_err(|error| {
                        ClaudeOrgError::with_message(
                            ClaudeOrgErrorKind::NotSignedIn,
                            error.to_string(),
                        )
                    })?
                    .claude_org_token()
            }),
            route_base_url,
        )
    }

    pub(super) fn with_source(source: ClaudeOrgTokenSource, route_base_url: String) -> Self {
        Self {
            source,
            route_base_url,
            cached: Mutex::new(None),
        }
    }

    #[allow(clippy::result_large_err)]
    async fn token(&self, rejected: Option<&str>) -> Result<ClaudeOrgToken, LlmError> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref().filter(|token| {
            token.is_fresh_at(chrono::Utc::now())
                && rejected.is_none_or(|rejected| rejected != token.access_token)
        }) {
            return Ok(token.clone());
        }
        *cached = None;
        let source = Arc::clone(&self.source);
        let token = tokio::task::spawn_blocking(move || source())
            .await
            .map_err(|error| {
                LlmError::Other(format!("Claude organization token task failed: {error}"))
            })?
            .map_err(token_error)?;
        let same_origin = reqwest::Url::parse(&token.base_url)
            .ok()
            .zip(reqwest::Url::parse(&self.route_base_url).ok())
            .is_some_and(|(token_base, route)| token_base.origin() == route.origin());
        if !same_origin {
            return Err(LlmError::Other(
                "Refusing to send the Claude organization token: the Codewhale service bound it to a different Anthropic endpoint than this route".to_string(),
            ));
        }
        *cached = Some(token.clone());
        Ok(token)
    }

    #[allow(clippy::result_large_err)]
    pub(super) async fn authorize(
        &self,
        request: reqwest::RequestBuilder,
        rejected: Option<&str>,
    ) -> Result<(reqwest::RequestBuilder, String), LlmError> {
        let token = self.token(rejected).await?;
        let version = HeaderValue::from_str(&token.anthropic_version).map_err(|_| {
            LlmError::Other("The Claude organization token carried an invalid API version".into())
        })?;
        let request = request
            .bearer_auth(&token.access_token)
            .header(HeaderName::from_static("anthropic-version"), version);
        Ok((request, token.access_token))
    }
}

fn token_error(error: ClaudeOrgError) -> LlmError {
    match error.kind {
        ClaudeOrgErrorKind::NotSignedIn
        | ClaudeOrgErrorKind::NotLinked
        | ClaudeOrgErrorKind::RelinkRequired
        | ClaudeOrgErrorKind::Disabled => LlmError::authentication_error(error.to_string()),
        ClaudeOrgErrorKind::Unavailable => LlmError::ServerError {
            status: 503,
            message: error.to_string(),
        },
        ClaudeOrgErrorKind::Invalid => LlmError::Other(error.to_string()),
    }
}

pub(super) fn relink_hint() -> String {
    format!("run `{CLAUDE_ORG_LINK_COMMAND}` to link your Claude organization again")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use codewhale_models::{ContentBlock, Message, MessageRequest, Role};
    use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::client::CodewhaleClient;
    use crate::llm_client::LlmClient;

    fn claude_org_client(server: &MockServer) -> (CodewhaleClient, Arc<AtomicUsize>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = Config::default();
        config.provider = Some("anthropic".to_string());
        config.providers = Some(ProvidersConfig {
            anthropic: ProviderConfig {
                auth_mode: Some("claude-org".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut client = CodewhaleClient::new(&config).expect("claude-org client");
        assert!(client.claude_org.is_some());
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fetches);
        client.claude_org = Some(Arc::new(ClaudeOrgAuth::with_source(
            Arc::new(move || {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(ClaudeOrgToken {
                    access_token: format!("org-token-{n}"),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    base_url: "https://api.anthropic.com".to_string(),
                    anthropic_version: "2026-10-21".to_string(),
                })
            }),
            client.base_url.clone(),
        )));
        client.test_messages_transport_base_url = Some(server.uri());
        (client, fetches)
    }

    fn request(client: &CodewhaleClient) -> MessageRequest {
        MessageRequest {
            model: client.default_model.clone(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 32,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(false),
            temperature: None,
            top_p: None,
            safety_check: None,
            title: None,
        }
    }

    fn ok_message() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 1}
        }))
    }

    #[tokio::test]
    async fn claude_org_sends_connection_bearer_and_version_and_refetches_once_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", "Bearer org-token-1"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "type": "error",
                "error": {"type": "authentication_error", "message": "expired"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", "Bearer org-token-2"))
            .respond_with(ok_message())
            .expect(1)
            .mount(&server)
            .await;
        let (client, fetches) = claude_org_client(&server);

        client
            .create_message(request(&client))
            .await
            .expect("retried message succeeds");

        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert!(request.headers.get("x-api-key").is_none());
            let versions: Vec<_> = request
                .headers
                .get_all("anthropic-version")
                .iter()
                .collect();
            assert_eq!(versions.len(), 1);
            assert_eq!(versions[0], "2026-10-21");
        }
    }

    #[tokio::test]
    async fn claude_org_spend_cap_is_reported_and_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(402).set_body_json(json!({
                "type": "error",
                "error": {
                    "type": "linked_organization_spend_limit_reached",
                    "message": "spend limit reached"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (client, fetches) = claude_org_client(&server);

        let error = client
            .create_message(request(&client))
            .await
            .expect_err("spend cap stops the request");

        let llm = error.downcast_ref::<LlmError>().expect("typed error");
        assert!(matches!(llm, LlmError::QuotaExhausted(_)), "{llm:?}");
        assert!(llm.to_string().contains("Claude Console"), "{llm}");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }
}
