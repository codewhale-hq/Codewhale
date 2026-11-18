use std::sync::{Arc, Mutex as StdMutex};

use codewhale_account_cli::{AccountClient, ClaudeOrgError, ClaudeOrgErrorKind, ClaudeOrgToken};
use codewhale_config::credentials::CLAUDE_ORG_LINK_COMMAND;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::llm_client::LlmError;

pub(super) type ClaudeOrgTokenSource =
    Arc<dyn Fn() -> std::result::Result<ClaudeOrgToken, ClaudeOrgError> + Send + Sync>;

const USER_PROFILES_BETA: &str = "user-profiles-billing-2026-10-01";
const ISSUED_TOKENS_KEPT: usize = 4;

pub(super) struct ClaudeOrgAuth {
    source: ClaudeOrgTokenSource,
    default_beta: Option<String>,
    cached: Mutex<Option<ClaudeOrgToken>>,
    issued: StdMutex<Vec<String>>,
}

impl ClaudeOrgAuth {
    pub(super) fn for_profile(profile: Option<String>, default_beta: Option<String>) -> Self {
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
            default_beta,
        )
    }

    pub(super) fn with_source(source: ClaudeOrgTokenSource, default_beta: Option<String>) -> Self {
        Self {
            source,
            default_beta,
            cached: Mutex::new(None),
            issued: StdMutex::new(Vec::new()),
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
        if let Ok(mut issued) = self.issued.lock() {
            if !issued.contains(&token.access_token) {
                issued.push(token.access_token.clone());
            }
            let excess = issued.len().saturating_sub(ISSUED_TOKENS_KEPT);
            issued.drain(..excess);
        }
        *cached = Some(token.clone());
        Ok(token)
    }

    #[allow(clippy::result_large_err)]
    pub(super) async fn refetch_after_rejection(&self, rejected: &str) -> Result<bool, LlmError> {
        Ok(self.token(Some(rejected)).await?.access_token != rejected)
    }

    #[allow(clippy::result_large_err)]
    pub(super) async fn authorize(
        &self,
        request: reqwest::RequestBuilder,
        test_transport_base: Option<&str>,
    ) -> Result<(reqwest::RequestBuilder, String), LlmError> {
        let token = self.token(None).await?;
        let inspected = request
            .try_clone()
            .ok_or_else(|| {
                LlmError::Other("Claude organization request cannot be inspected".into())
            })?
            .build()
            .map_err(|error| LlmError::Other(error.without_url().to_string()))?;
        let destination = inspected.url().origin();
        let allowed = [Some(token.base_url.as_str()), test_transport_base]
            .into_iter()
            .flatten()
            .filter_map(|base| reqwest::Url::parse(base).ok())
            .any(|base| base.origin() == destination);
        if !allowed {
            return Err(LlmError::Other(
                "Refusing to send the Claude organization token: the request left the Anthropic endpoint the Codewhale service bound it to".into(),
            ));
        }
        let invalid = |what: &str| {
            LlmError::Other(format!(
                "The Claude organization token carried an invalid {what}"
            ))
        };
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", token.access_token))
            .map_err(|_| invalid("access token"))?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_str(&token.anthropic_version).map_err(|_| invalid("API version"))?,
        );
        let mut request = request;
        if let Some(profile) = token.user_profile_id.as_deref() {
            headers.insert(
                HeaderName::from_static("anthropic-user-profile-id"),
                HeaderValue::from_str(profile).map_err(|_| invalid("user profile id"))?,
            );
            let requested: Vec<&str> = inspected
                .headers()
                .get_all("anthropic-beta")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect();
            let sources = if requested.is_empty() {
                self.default_beta.as_deref().into_iter().collect()
            } else {
                requested
            };
            let mut betas: Vec<&str> = Vec::new();
            for beta in sources
                .iter()
                .flat_map(|value| value.split(','))
                .map(str::trim)
                .filter(|beta| !beta.is_empty())
                .chain([USER_PROFILES_BETA])
            {
                if !betas.contains(&beta) {
                    betas.push(beta);
                }
            }
            headers.insert(
                HeaderName::from_static("anthropic-beta"),
                HeaderValue::from_str(&betas.join(",")).map_err(|_| invalid("beta header"))?,
            );
            if inspected.method() == reqwest::Method::POST
                && inspected.url().path().ends_with("/v1/messages")
            {
                let mut body: Value = inspected
                    .body()
                    .and_then(reqwest::Body::as_bytes)
                    .and_then(|bytes| serde_json::from_slice(bytes).ok())
                    .ok_or_else(|| {
                        LlmError::Other("Claude organization Messages body is not JSON".into())
                    })?;
                body.as_object_mut()
                    .ok_or_else(|| {
                        LlmError::Other("Claude organization Messages body is not an object".into())
                    })?
                    .insert(
                        "billing_policy".into(),
                        json!({"type": "billing_policy", "bill_to": "user_profile"}),
                    );
                let body = serde_json::to_vec(&body)
                    .map_err(|error| LlmError::Other(error.to_string()))?;
                request = request.body(body);
            }
        }
        Ok((request.headers(headers), token.access_token))
    }

    pub(super) fn redact(&self, text: &str) -> String {
        let Ok(issued) = self.issued.lock() else {
            return text.to_string();
        };
        issued.iter().fold(text.to_string(), |text, token| {
            text.replace(token.as_str(), codewhale_config::persistence::REDACTED)
        })
    }
}

fn token_error(error: ClaudeOrgError) -> LlmError {
    match error.kind {
        ClaudeOrgErrorKind::NotSignedIn
        | ClaudeOrgErrorKind::NotLinked
        | ClaudeOrgErrorKind::RelinkRequired
        | ClaudeOrgErrorKind::Disabled => LlmError::authentication_error(error.to_string()),
        ClaudeOrgErrorKind::Unavailable => LlmError::ConnectError {
            message: error.to_string(),
            transient: true,
            timed_out: false,
        },
        ClaudeOrgErrorKind::Invalid => LlmError::Other(error.to_string()),
    }
}

pub(super) fn http_error(status: u16, raw: &str) -> Option<LlmError> {
    if status == 403 && raw.contains("user_profile_not_billable") {
        return Some(LlmError::authentication_error(format!(
            "Anthropic could not bill your linked Claude organization: the link ended or the organization cannot be billed. Run `{CLAUDE_ORG_LINK_COMMAND}` and check Linked platforms in the Claude Console."
        )));
    }
    LlmError::from_linked_organization_billing_stop(status, raw)
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

    fn claude_org_config() -> Config {
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
        config
    }

    fn claude_org_client(server: &MockServer) -> (CodewhaleClient, Arc<AtomicUsize>) {
        claude_org_client_with(server, None, None)
    }

    fn claude_org_client_with(
        server: &MockServer,
        user_profile_id: Option<&str>,
        default_beta: Option<&str>,
    ) -> (CodewhaleClient, Arc<AtomicUsize>) {
        let mut client = CodewhaleClient::new(&claude_org_config()).expect("claude-org client");
        assert!(client.claude_org.is_some());
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fetches);
        let user_profile_id = user_profile_id.map(str::to_string);
        client.claude_org = Some(Arc::new(ClaudeOrgAuth::with_source(
            Arc::new(move || {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(ClaudeOrgToken {
                    access_token: format!("org-token-{n}"),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    base_url: "https://api.anthropic.com".to_string(),
                    anthropic_version: "2026-10-21".to_string(),
                    user_profile_id: user_profile_id.clone(),
                })
            }),
            default_beta.map(str::to_string),
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

    #[tokio::test]
    async fn claude_org_user_profile_sends_profile_beta_and_billing_policy() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ok_message())
            .expect(1)
            .mount(&server)
            .await;
        let (client, _) = claude_org_client_with(
            &server,
            Some("uprof_test-1"),
            Some("prompt-caching-2024-07-31"),
        );

        client
            .create_message(request(&client))
            .await
            .expect("message succeeds");

        let requests = server.received_requests().await.expect("requests");
        let request = &requests[0];
        assert_eq!(
            request.headers.get("anthropic-user-profile-id").unwrap(),
            "uprof_test-1"
        );
        let betas: Vec<_> = request.headers.get_all("anthropic-beta").iter().collect();
        assert_eq!(betas.len(), 1);
        assert_eq!(
            betas[0],
            "prompt-caching-2024-07-31,user-profiles-billing-2026-10-01"
        );
        let body: Value = serde_json::from_slice(&request.body).expect("json body");
        assert_eq!(
            body["billing_policy"],
            json!({"type": "billing_policy", "bill_to": "user_profile"})
        );
        assert!(request.headers.get("x-api-key").is_none());
    }

    #[test]
    fn claude_org_session_key_wins_over_the_linked_organization() {
        let config = claude_org_config();
        let identity = config.active_provider_identity().expect("identity");
        let endpoint = config.base_url_for_route(&identity);
        config.session_provider_keys.write().insert(
            identity,
            (
                endpoint,
                codewhale_runtime::credentials::Credential::ApiKey {
                    key: "sk-ant-session".to_string(),
                },
            ),
        );
        let client = CodewhaleClient::new(&config).expect("session-key client");
        assert!(client.claude_org.is_none());
        assert_eq!(client.api_key, "sk-ant-session");
    }
}
