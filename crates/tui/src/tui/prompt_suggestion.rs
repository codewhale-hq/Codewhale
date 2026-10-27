//! Ghost-text follow-up prompt suggestion.
//!
//! After each completed turn, a lightweight API call generates ONE short
//! follow-up question the user might want to ask next. The suggestion is
//! rendered as dimmed ghost text in the composer when the input is empty.

use std::fmt;
use std::sync::OnceLock;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tracing::debug;

use crate::core::events::TurnRoute;
use crate::route_receipt::{TurnRouteReceipt, endpoint_identity};
use codewhale_config::ProviderKind;
use codewhale_runtime::config::Config;

/// The exact route authority a turn was launched against.
///
/// This is a thin, gated wrapper around the [`TurnRouteReceipt`] the **engine**
/// minted from the installed, preflighted client. It is never derived from
/// live config: the whole point is that by the time the TUI processes
/// `TurnStarted`, config may already describe a different endpoint or
/// credential (web config events are drained ahead of engine events), and
/// authority resolved from that mutable state would authorize sending a
/// completed turn's context to a route the turn never ran on.
///
/// `TurnComplete` re-resolves the same identity and must reproduce every field
/// of this record; anything else — including a same-identity endpoint or key
/// rotation performed mid-turn — fails closed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SuggestionRouteAuthority {
    receipt: TurnRouteReceipt,
}

impl SuggestionRouteAuthority {
    #[must_use]
    pub fn provider(&self) -> ProviderKind {
        self.receipt.provider()
    }

    /// Exact configured route key (`TurnRoute::provider_identity`).
    #[must_use]
    pub fn provider_identity(&self) -> &str {
        self.receipt.provider_identity()
    }

    /// Exact wire model the turn's client was bound to.
    #[must_use]
    pub fn model(&self) -> &str {
        self.receipt.wire_model()
    }

    /// Normalized, redacted identity of the endpoint the turn's client used.
    #[must_use]
    pub fn endpoint_identity(&self) -> &str {
        self.receipt.endpoint_identity()
    }

    /// Whether a live re-resolution still lands on the same endpoint and the
    /// same credential generation.
    fn authorizes(&self, base_url: &str, api_key: &str, openrouter_vendor: Option<&str>) -> bool {
        self.receipt.matches_live_route(base_url, api_key)
            && self.receipt.openrouter_vendor() == openrouter_vendor
    }

    #[cfg(test)]
    pub(crate) fn from_receipt_for_test(receipt: TurnRouteReceipt) -> Self {
        Self { receipt }
    }
}

/// Non-secret route provenance for the turn that just completed.
///
/// This is a snapshot of `TurnRoute` plus the authority minted when that turn's
/// client was installed, not live UI selection state. Every suggestion decision
/// is anchored to it, so a route switch made after the turn completed cannot
/// redirect the background request.
#[derive(Clone, Copy)]
pub struct SuggestionRouteSnapshot<'a> {
    pub provider: ProviderKind,
    /// Exact configured route key (`TurnRoute::provider_identity`).
    pub provider_identity: &'a str,
    /// Exact wire model the completed turn actually used.
    pub model: &'a str,
    /// Authority carried on the completed turn's route receipt.
    pub authority: &'a SuggestionRouteAuthority,
    /// Actual base URL this turn's client used, from `Event::TurnComplete`.
    pub actual_base_url: Option<&'a str>,
}

/// Redacted: `actual_base_url` is a raw endpoint that may carry URL userinfo or
/// sensitive query values, so it renders as its normalized redacted identity.
impl fmt::Debug for SuggestionRouteSnapshot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SuggestionRouteSnapshot")
            .field("provider", &self.provider)
            .field("provider_identity", &self.provider_identity)
            .field("model", &self.model)
            .field("authority", &self.authority)
            .field(
                "actual_endpoint_identity",
                &self.actual_base_url.map(endpoint_identity),
            )
            .finish()
    }
}

/// Credential material resolved for exactly one route identity.
///
/// The resolver that produces this must scope itself to the snapshot identity;
/// it must never fall back to the ambient/active provider.
#[derive(Clone, PartialEq, Eq)]
pub struct SuggestionRouteCredentials {
    pub api_key: String,
    pub base_url: String,
    /// Wire model the resolver arrived at. Must equal the snapshot model.
    pub model: String,
    pub openrouter_vendor: Option<String>,
}

/// Redacted: an API key must never reach a log line, panic message, or test
/// failure output through `{:?}`, and a raw `base_url` can itself carry
/// credentials in URL userinfo or a query token.
impl fmt::Debug for SuggestionRouteCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SuggestionRouteCredentials")
            .field("api_key", &"<redacted>")
            .field("endpoint_identity", &endpoint_identity(&self.base_url))
            .field("model", &self.model)
            .finish()
    }
}

/// A fully validated background suggestion request.
#[derive(Clone, PartialEq, Eq)]
pub struct SuggestionLaunch {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub openrouter_vendor: Option<String>,
}

/// Redacted: see [`SuggestionRouteCredentials`].
impl fmt::Debug for SuggestionLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SuggestionLaunch")
            .field("api_key", &"<redacted>")
            .field("endpoint_identity", &endpoint_identity(&self.base_url))
            .field("model", &self.model)
            .finish()
    }
}

/// Whether a provider speaks the ordinary OpenAI-compatible
/// `/chat/completions` shape [`generate_suggestion`] hardcodes.
///
/// Gate on wire protocol, not a vendor enum: Anthropic Messages and the
/// OpenAI Responses API are different request shapes and stay out.
#[must_use]
pub fn route_is_supported_suggestion_provider(provider: ProviderKind) -> bool {
    crate::client::provider_speaks_chat_completions(provider)
}

/// Resolve credentials for exactly one configured route identity.
///
/// The identity is revalidated against live config and then scoped with
/// `resolve_runtime_route_for_identity`, so the key and endpoint come from that
/// route's own configuration rather than from whichever provider happens to be
/// selected now. An identity that no longer resolves, or that now resolves to a
/// different provider kind, yields `None`.
///
/// The returned `base_url` is the **resolved route candidate's** endpoint, not
/// `Config::active_route_base_url()`. Those two are not the same string: the config
/// accessor is one input to candidate resolution, and the candidate endpoint is
/// what `CodewhaleClient::from_candidate` binds the transport to and therefore
/// what `Event::TurnComplete` reports back. Comparing anything else here would
/// compare a turn's actual endpoint against a differently-canonicalized value
/// and fail closed on routes that never changed.
fn resolve_credentials_for_identity(
    config: &Config,
    identity: &codewhale_runtime::config::ProviderIdentity,
    model: &str,
) -> Option<SuggestionRouteCredentials> {
    // Belt and braces: callers already gated, but this function must never
    // read credentials for a wire this helper does not speak.
    let provider = identity.provider;
    if !route_is_supported_suggestion_provider(provider) {
        return None;
    }
    config.verify_provider_identity(identity).ok()?;
    let resolved =
        crate::route_runtime::resolve_runtime_route_for_identity(config, identity, Some(model))
            .ok()?;
    if resolved.identity.provider != provider {
        return None;
    }
    // This helper intentionally sends the ordinary Chat Completions shape.
    // A configured path override may describe a provider-specific transport
    // contract that this bounded feature does not implement, so fail closed
    // instead of silently bypassing it with the canonical path.
    if resolved
        .config
        .provider_config_for(&resolved.identity)
        .and_then(|route| route.path_suffix.as_ref())
        .is_some()
    {
        return None;
    }
    let api_key = resolved.config.active_route_api_key().ok()?;
    Some(SuggestionRouteCredentials {
        api_key,
        base_url: resolved.candidate.endpoint().base_url.clone(),
        model: resolved.model.clone(),
        openrouter_vendor: resolved.config.openrouter_vendor().ok()?,
    })
}

/// Adopt the engine's route receipt as this turn's suggestion authority.
///
/// Takes **no `Config`**, by design. This runs while the TUI handles
/// `TurnStarted`, which is strictly after the engine resolved, preflighted, and
/// installed the turn's client — and strictly after any web config event queued
/// in the meantime has been drained. Reading credentials here would capture
/// whatever route config describes *now*, not the route the turn is running on.
/// The receipt was minted from the installed client itself, so it cannot drift.
///
/// Unsupported providers — Anthropic Messages, Responses, and any other
/// non-Chat-Completions wire — return `None`, and no credential material
/// of any provider is inspected on this path at all.
#[must_use]
pub fn capture_route_authority(route: &TurnRoute) -> Option<SuggestionRouteAuthority> {
    if !route_is_supported_suggestion_provider(route.provider) {
        return None;
    }
    let provider_identity = route.provider_identity.trim();
    let model = route.model.trim();
    if provider_identity.is_empty() || model.is_empty() {
        return None;
    }

    // A receipt that describes a different route than the event's own
    // `TurnRoute` is broken provenance, not a usable authority.
    let receipt = route.receipt.as_ref()?;
    if receipt.provider() != route.provider
        || receipt.provider_identity() != provider_identity
        || receipt.wire_model() != model
        || receipt.endpoint_identity().is_empty()
        || receipt.credential_generation().is_empty()
    {
        return None;
    }

    Some(SuggestionRouteAuthority {
        receipt: receipt.clone(),
    })
}

/// Decide whether a completed turn may launch a background prompt suggestion,
/// and with exactly what credentials, endpoint, and model.
///
/// Fail-closed by construction:
/// - `resolve_route_credentials` is only invoked once every non-credential gate
///   has passed for a Chat Completions route, so a Messages/Responses
///   completion never reaches another provider's credentials at all.
/// - The decision reads only `completed_route`, never live selection state, so
///   a later route switch cannot redirect it.
/// - The route must still resolve to the *same* provider, identity, wire model,
///   endpoint identity, and credential generation the engine recorded on this
///   turn's route receipt, and to the endpoint the turn's client actually used.
///   A same-identity endpoint or key rotation is therefore a mismatch, not an
///   accepted match, and no conversation context is sent anywhere.
pub fn plan_suggestion_launch<F>(
    turn_completed: bool,
    suggestion_enabled: bool,
    api_message_count: usize,
    completed_route: Option<SuggestionRouteSnapshot<'_>>,
    resolve_route_credentials: F,
) -> Option<SuggestionLaunch>
where
    F: FnOnce(&SuggestionRouteSnapshot<'_>) -> Option<SuggestionRouteCredentials>,
{
    if !turn_completed || !suggestion_enabled || api_message_count < 2 {
        return None;
    }
    // No route snapshot means no provenance. Non-model turns (composer `!`
    // shell commands) land here too.
    let route = completed_route?;
    if !route_is_supported_suggestion_provider(route.provider) {
        return None;
    }
    let identity = route.provider_identity.trim();
    if identity.is_empty() {
        return None;
    }
    let model = route.model.trim();
    if model.is_empty() {
        return None;
    }

    // The authority came off this turn's own route receipt. If it describes
    // anything else, the provenance chain is broken.
    let authority = route.authority;
    if authority.provider() != route.provider
        || authority.provider_identity() != identity
        || authority.model() != model
        || authority.endpoint_identity().is_empty()
    {
        return None;
    }

    // The engine reports the endpoint this turn's client actually used. It is
    // required, and it must be the endpoint the receipt was minted from.
    let actual_endpoint = endpoint_identity(route.actual_base_url?);
    if actual_endpoint.is_empty() || actual_endpoint != authority.endpoint_identity() {
        return None;
    }

    let credentials = resolve_route_credentials(&route)?;
    if credentials.api_key.trim().is_empty() {
        return None;
    }
    // Never silently swap in a cheaper/different model than the one the
    // completed turn was actually routed to.
    if credentials.model.trim() != model {
        return None;
    }
    // A same-identity endpoint mutation *or* credential rotation lands here.
    // Both are checked against the receipt in one step, over the raw endpoint
    // and raw credential, so a mutation hidden behind identical redaction (URL
    // userinfo, a query token) is still a mismatch.
    if !authority.authorizes(
        &credentials.base_url,
        &credentials.api_key,
        credentials.openrouter_vendor.as_deref(),
    ) {
        return None;
    }

    // Dispatch from the exact base endpoint and credential the receipt
    // authorized — the raw pair the digest was taken over, not a
    // re-canonicalized variant. `generate_suggestion` applies the same
    // canonical ordinary Chat Completions path mapping as the installed
    // client; custom path overrides failed closed above.
    Some(SuggestionLaunch {
        api_key: credentials.api_key,
        base_url: credentials.base_url,
        model: model.to_string(),
        openrouter_vendor: credentials.openrouter_vendor,
    })
}

/// [`plan_suggestion_launch`] wired to the real, identity-scoped config
/// resolver. This is the only production entry point.
#[must_use]
pub fn plan_suggestion_launch_with_config(
    config: &Config,
    turn_completed: bool,
    suggestion_enabled: bool,
    api_message_count: usize,
    completed_route: Option<SuggestionRouteSnapshot<'_>>,
) -> Option<SuggestionLaunch> {
    plan_suggestion_launch(
        turn_completed,
        suggestion_enabled,
        api_message_count,
        completed_route,
        |route| {
            resolve_credentials_for_identity(
                config,
                route.authority.receipt.admitted_identity(),
                route.model.trim(),
            )
        },
    )
}

/// Reusable static client — avoids creating a new connection pool per request.
fn suggestion_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(codewhale_release::tls::reqwest_client)
}

/// Generate a follow-up prompt suggestion based on recent messages.
///
/// Sends the conversation summary to the API with a system prompt that
/// asks for a single short follow-up question. Returns `None` on failure
/// or empty result — callers treat this as best-effort.
pub async fn generate_suggestion(
    api_key: &str,
    base_url: &str,
    model: &str,
    recent_messages: &str,
    openrouter_vendor: Option<&str>,
) -> Option<String> {
    // Suggestions are model output derived from the just-completed
    // interactive transcript. They therefore participate in the same
    // attached CWC run even though this narrow adapter owns a raw reqwest
    // client instead of a `CodewhaleClient`. Retain the permit through decode
    // so Runtime Chat cannot overlap or project a second inference lifecycle.
    let _inference = crate::client::acquire_remote_control_inference_participant().await;
    let client = suggestion_client();
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "\
    You are a helpful assistant. Based on the recent conversation context, generate \
    ONE short follow-up question (under 60 characters) the user might want to ask \
    next. Reply with ONLY the question text, nothing else — no quotes, no explanations, \
    no prefixes."
            },
            {
                "role": "user",
                "content": format!(
                    "Recent conversation:\n{recent_messages}\n\n\
                     Generate ONE short follow-up question the user might ask next:"
                )
            }
        ],
        "max_tokens": 64,
        "temperature": 0.3,
        "stream": false
    });
    crate::client::apply_openrouter_vendor(&mut body, openrouter_vendor);

    let url = crate::client::api_url(base_url, "chat/completions");
    // Never log the raw request URL: a base URL can carry credentials in its
    // userinfo or in a query token. The redacted endpoint identity keeps the
    // line diagnosable without carrying either.
    debug!(
        endpoint = %endpoint_identity(&url),
        %model,
        "generating prompt suggestion"
    );
    let mut request = client
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header(CONTENT_TYPE, "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .json(&body);
    // OpenRouter app attribution: same headers the Chat Completions client
    // already sends. Infer from the turn's actual endpoint so this helper
    // does not grow a provider enum of its own.
    if endpoint_identity(&url).contains("openrouter.ai") {
        request = request
            .header("HTTP-Referer", "https://codewhale.net")
            .header("X-Title", "Codewhale");
    }
    let response = match request.send().await {
        Ok(r) => r,
        Err(_) => return None,
    };

    let value: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return None,
    };

    let suggestion = value["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty() && s.len() <= 200)?;

    // The suggestion is model output derived from conversation context, so its
    // text stays out of logs; only its shape is recorded.
    debug!(
        chars = suggestion.chars().count(),
        "prompt suggestion generated"
    );
    Some(suggestion)
}

/// Extract the first text line from a single message.
fn message_summary(m: &codewhale_models::Message) -> Option<String> {
    let role = match m.role.as_str() {
        "user" => "User",
        "assistant" => "Assistant",
        _ => return None,
    };
    let text = m
        .content
        .iter()
        .filter_map(|block| match block {
            codewhale_models::ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return None;
    }
    let truncated: String = first_line
        .chars()
        .take(120)
        .chain(if first_line.chars().count() > 120 {
            Some('…')
        } else {
            None
        })
        .collect();
    Some(format!("{role}: {truncated}"))
}

/// Build a one-line-per-message summary of recent conversation context.
/// Takes the last N messages, skipping tool-only messages.
pub fn summarize_recent_messages(messages: &[codewhale_models::Message], limit: usize) -> String {
    let start = messages.len().saturating_sub(limit);
    messages[start..]
        .iter()
        .filter_map(message_summary)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        Config, ProviderKind, SuggestionRouteAuthority, SuggestionRouteCredentials,
        SuggestionRouteSnapshot, TurnRoute, TurnRouteReceipt, capture_route_authority,
        endpoint_identity, generate_suggestion, plan_suggestion_launch,
        plan_suggestion_launch_with_config, resolve_credentials_for_identity,
    };
    use crate::test_support::{EnvVarGuard, TestEnvLock, lock_test_env};
    use codewhale_runtime::config::ProvidersConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1";
    const DEEPSEEK_KEY: &str = "sk-deepseek-secret";

    #[tokio::test]
    async fn suggestion_request_preserves_openrouter_vendor_pin() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "What should we do next?" } }]
            })))
            .expect(2)
            .mount(&server)
            .await;

        for vendor in [Some("chutes/region-fixture"), None] {
            assert!(
                generate_suggestion(
                    "fixture-key",
                    &format!("{}/v1", server.uri()),
                    "fixture/model",
                    "User: hello",
                    vendor,
                )
                .await
                .is_some()
            );
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let pinned: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            pinned["provider"],
            serde_json::json!({"order": ["chutes/region-fixture"], "allow_fallbacks": false})
        );
        let unpinned: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(unpinned.get("provider").is_none());
    }

    #[tokio::test]
    async fn suggestion_inference_waits_for_runtime_chat_ownership() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "What should we do next?" } }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let ownership = crate::client::acquire_runtime_chat_inference_ownership().await;
        let base_url = format!("{}/v1", server.uri());
        let mut suggestion = tokio::spawn(async move {
            generate_suggestion(
                "fixture-key",
                &base_url,
                "deepseek-v4-flash",
                "User: hello\nAssistant: hi",
                None,
            )
            .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut suggestion)
                .await
                .is_err(),
            "background suggestion provider output must wait behind Runtime Chat"
        );
        drop(ownership);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), suggestion)
                .await
                .expect("suggestion resumes after relay settlement")
                .expect("suggestion task")
                .as_deref(),
            Some("What should we do next?")
        );
    }

    /// Stand-in for the real credential resolver. Records every identity it was
    /// asked about so a test can prove it was never consulted at all.
    struct RecordingResolver {
        /// Credentials keyed by exact `(provider, provider_identity)`.
        available: Vec<(ProviderKind, &'static str, SuggestionRouteCredentials)>,
        asked: RefCell<Vec<(ProviderKind, String)>>,
    }

    impl RecordingResolver {
        fn new(available: Vec<(ProviderKind, &'static str, SuggestionRouteCredentials)>) -> Self {
            Self {
                available,
                asked: RefCell::new(Vec::new()),
            }
        }

        fn resolve(
            &self,
            route: &SuggestionRouteSnapshot<'_>,
        ) -> Option<SuggestionRouteCredentials> {
            self.asked
                .borrow_mut()
                .push((route.provider, route.provider_identity.to_string()));
            self.available
                .iter()
                .find(|(provider, identity, _)| {
                    *provider == route.provider && *identity == route.provider_identity
                })
                .map(|(_, _, credentials)| credentials.clone())
        }

        fn asked(&self) -> Vec<(ProviderKind, String)> {
            self.asked.borrow().clone()
        }
    }

    fn credentials(api_key: &str, base_url: &str, model: &str) -> SuggestionRouteCredentials {
        SuggestionRouteCredentials {
            api_key: api_key.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            openrouter_vendor: None,
        }
    }

    fn deepseek_credentials(model: &str) -> SuggestionRouteCredentials {
        credentials(DEEPSEEK_KEY, DEEPSEEK_BASE, model)
    }

    /// A receipt as the engine would have minted it from the installed client.
    fn receipt(
        provider: ProviderKind,
        identity: &str,
        model: &str,
        base_url: &str,
        api_key: &str,
    ) -> TurnRouteReceipt {
        TurnRouteReceipt::new(provider, identity, model, base_url, api_key)
    }

    /// Authority as the TUI would have adopted it at `TurnStarted`.
    ///
    /// Bypasses the provider gate so the unsupported-provider tests below can
    /// prove the *later* gates also hold, not just the first one.
    fn route_authority(
        provider: ProviderKind,
        identity: &str,
        model: &str,
        base_url: &str,
        api_key: &str,
    ) -> SuggestionRouteAuthority {
        SuggestionRouteAuthority::from_receipt_for_test(receipt(
            provider, identity, model, base_url, api_key,
        ))
    }

    fn deepseek_authority(model: &str) -> SuggestionRouteAuthority {
        route_authority(
            ProviderKind::Deepseek,
            "deepseek",
            model,
            DEEPSEEK_BASE,
            DEEPSEEK_KEY,
        )
    }

    fn snapshot<'a>(
        provider: ProviderKind,
        identity: &'a str,
        model: &'a str,
        authority: &'a SuggestionRouteAuthority,
    ) -> SuggestionRouteSnapshot<'a> {
        SuggestionRouteSnapshot {
            provider,
            provider_identity: identity,
            model,
            authority,
            actual_base_url: Some(DEEPSEEK_BASE),
        }
    }

    #[test]
    fn deepseek_route_uses_its_exact_wire_model_and_base_url() {
        let resolver = RecordingResolver::new(vec![(
            ProviderKind::Deepseek,
            "deepseek",
            deepseek_credentials("deepseek-reasoner"),
        )]);
        let authority = deepseek_authority("deepseek-reasoner");
        let launch = plan_suggestion_launch(
            true,
            true,
            2,
            Some(snapshot(
                ProviderKind::Deepseek,
                "deepseek",
                "deepseek-reasoner",
                &authority,
            )),
            |route| resolver.resolve(route),
        )
        .expect("supported deepseek route with unchanged credentials must launch");

        assert_eq!(launch.model, "deepseek-reasoner");
        assert_eq!(launch.base_url, DEEPSEEK_BASE);
        assert_eq!(launch.api_key, DEEPSEEK_KEY);
    }

    #[test]
    fn non_chat_completions_completion_never_touches_foreign_credentials() {
        // A Chat Completions key exists and would resolve fine — the gate must
        // run before the resolver is ever consulted.
        let resolver = RecordingResolver::new(vec![(
            ProviderKind::Deepseek,
            "deepseek",
            deepseek_credentials("deepseek-chat"),
        )]);
        for (provider, identity, model) in [
            (ProviderKind::Anthropic, "anthropic", "claude-sonnet-4"),
            (ProviderKind::OpenaiCodex, "openai-codex", "gpt-5.4"),
            (
                ProviderKind::DeepseekAnthropic,
                "deepseek-anthropic",
                "deepseek-chat",
            ),
        ] {
            let authority = route_authority(provider, identity, model, DEEPSEEK_BASE, DEEPSEEK_KEY);
            let launch = plan_suggestion_launch(
                true,
                true,
                8,
                Some(snapshot(provider, identity, model, &authority)),
                |route| resolver.resolve(route),
            );
            assert!(
                launch.is_none(),
                "{provider:?} completion must not launch a prompt suggestion"
            );
        }
        assert!(
            resolver.asked().is_empty(),
            "credential resolution must never be attempted for unsupported routes, got {:?}",
            resolver.asked()
        );
    }

    #[test]
    fn chat_completions_routes_launch_with_their_own_credentials() {
        for (provider, identity, model, base, key) in [
            (
                ProviderKind::Deepseek,
                "deepseek",
                "deepseek-chat",
                DEEPSEEK_BASE,
                DEEPSEEK_KEY,
            ),
            (
                ProviderKind::Openai,
                "openai",
                "gpt-5.6",
                "https://api.openai.com/v1",
                "sk-openai",
            ),
            (
                ProviderKind::Openrouter,
                "openrouter",
                "some/model",
                "https://openrouter.ai/api/v1",
                "sk-or",
            ),
            (
                ProviderKind::Custom,
                "lm-studio",
                "local-model",
                "http://127.0.0.1:1234/v1",
                "lm-key",
            ),
            (
                ProviderKind::Zai,
                "zai",
                "GLM-5.3",
                "https://api.z.ai/api/paas/v4",
                "zai-key",
            ),
        ] {
            let resolver =
                RecordingResolver::new(vec![(provider, identity, credentials(key, base, model))]);
            let authority = route_authority(provider, identity, model, base, key);
            let route = SuggestionRouteSnapshot {
                provider,
                provider_identity: identity,
                model,
                authority: &authority,
                actual_base_url: Some(base),
            };
            let launch =
                plan_suggestion_launch(true, true, 2, Some(route), |route| resolver.resolve(route))
                    .unwrap_or_else(|| panic!("{provider:?} Chat Completions route must launch"));
            assert_eq!(launch.api_key, key, "{provider:?}");
            assert_eq!(launch.base_url, base, "{provider:?}");
            assert_eq!(launch.model, model, "{provider:?}");
            assert_eq!(resolver.asked(), vec![(provider, identity.to_string())]);
        }
    }

    #[test]
    fn missing_credentials_fail_closed() {
        let authority = deepseek_authority("deepseek-chat");
        // No entry for the deepseek identity: resolver returns None.
        let empty = RecordingResolver::new(Vec::new());
        assert!(
            plan_suggestion_launch(
                true,
                true,
                2,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "deepseek",
                    "deepseek-chat",
                    &authority
                )),
                |route| empty.resolve(route),
            )
            .is_none(),
            "unresolvable route credentials must fail closed"
        );

        for incomplete in [
            credentials("   ", DEEPSEEK_BASE, "deepseek-chat"),
            credentials(DEEPSEEK_KEY, "", "deepseek-chat"),
        ] {
            assert!(
                plan_suggestion_launch(
                    true,
                    true,
                    2,
                    Some(snapshot(
                        ProviderKind::Deepseek,
                        "deepseek",
                        "deepseek-chat",
                        &authority
                    )),
                    |_| Some(incomplete.clone()),
                )
                .is_none(),
                "incomplete credentials must fail closed: {incomplete:?}"
            );
        }
    }

    #[test]
    fn resolver_is_asked_only_about_the_completed_route_identity() {
        // Live selection has moved on to another provider; the plan is built
        // from the completed-turn snapshot, so the resolver only ever sees the
        // completed identity.
        const CN_BASE: &str = "https://api.deepseek.cn/v1";
        let resolver = RecordingResolver::new(vec![
            (
                ProviderKind::Deepseek,
                "deepseek",
                deepseek_credentials("deepseek-chat"),
            ),
            (
                ProviderKind::Deepseek,
                "deepseek-cn",
                credentials("sk-cn", CN_BASE, "deepseek-chat"),
            ),
        ]);
        let authority = route_authority(
            ProviderKind::Deepseek,
            "deepseek-cn",
            "deepseek-chat",
            CN_BASE,
            "sk-cn",
        );
        let launch = plan_suggestion_launch(
            true,
            true,
            4,
            Some(SuggestionRouteSnapshot {
                provider: ProviderKind::Deepseek,
                provider_identity: "deepseek-cn",
                model: "deepseek-chat",
                authority: &authority,
                actual_base_url: Some(CN_BASE),
            }),
            |route| resolver.resolve(route),
        )
        .expect("completed deepseek-cn route must launch on its own endpoint");

        assert_eq!(launch.base_url, CN_BASE);
        assert_eq!(launch.api_key, "sk-cn");
        assert_eq!(
            resolver.asked(),
            vec![(ProviderKind::Deepseek, "deepseek-cn".to_string())],
            "only the completed route identity may be inspected"
        );
    }

    #[test]
    fn model_substitution_by_the_resolver_fails_closed() {
        let authority = deepseek_authority("deepseek-reasoner");
        assert!(
            plan_suggestion_launch(
                true,
                true,
                2,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "deepseek",
                    "deepseek-reasoner",
                    &authority
                )),
                // Silent downgrade to a cheaper model.
                |_| Some(deepseek_credentials("deepseek-chat")),
            )
            .is_none(),
            "a resolver-substituted model must not be dispatched"
        );
    }

    #[test]
    fn same_identity_endpoint_or_key_rotation_fails_closed() {
        let authority = deepseek_authority("deepseek-chat");
        // Same provider, same identity, same model — but the endpoint moved
        // while the turn was in flight.
        assert!(
            plan_suggestion_launch(
                true,
                true,
                2,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "deepseek",
                    "deepseek-chat",
                    &authority
                )),
                |_| Some(credentials(
                    DEEPSEEK_KEY,
                    "https://exfil.example.com/v1",
                    "deepseek-chat"
                )),
            )
            .is_none(),
            "a same-identity endpoint mutation must fail closed"
        );
        // …and the same for a credential rotation onto the same endpoint.
        assert!(
            plan_suggestion_launch(
                true,
                true,
                2,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "deepseek",
                    "deepseek-chat",
                    &authority
                )),
                |_| Some(credentials(
                    "sk-rotated-elsewhere",
                    DEEPSEEK_BASE,
                    "deepseek-chat"
                )),
            )
            .is_none(),
            "a same-identity credential mutation must fail closed"
        );
    }

    #[test]
    fn userinfo_rotation_behind_identical_redaction_fails_closed() {
        // Both endpoints redact to the same identity string. Only the
        // credential-generation digest, which covers the raw endpoint, can
        // tell them apart — so redaction must not be the whole comparison.
        const ORIGINAL: &str = "https://svc:original@api.deepseek.com/v1";
        const ROTATED: &str = "https://svc:rotated@api.deepseek.com/v1";
        assert_eq!(endpoint_identity(ORIGINAL), endpoint_identity(ROTATED));

        let authority = route_authority(
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-chat",
            ORIGINAL,
            DEEPSEEK_KEY,
        );
        let route = SuggestionRouteSnapshot {
            provider: ProviderKind::Deepseek,
            provider_identity: "deepseek",
            model: "deepseek-chat",
            authority: &authority,
            actual_base_url: Some(ORIGINAL),
        };
        assert!(
            plan_suggestion_launch(true, true, 2, Some(route), |_| Some(credentials(
                DEEPSEEK_KEY,
                ROTATED,
                "deepseek-chat"
            )))
            .is_none(),
            "a URL-userinfo rotation hidden by redaction must fail closed"
        );
        assert!(
            plan_suggestion_launch(true, true, 2, Some(route), |_| Some(credentials(
                DEEPSEEK_KEY,
                ORIGINAL,
                "deepseek-chat"
            )))
            .is_some(),
            "control: the unrotated endpoint still launches"
        );
    }

    #[test]
    fn actual_turn_endpoint_must_be_present_and_match_the_authority() {
        let authority = deepseek_authority("deepseek-chat");
        for actual_base_url in [None, Some("https://exfil.example.com/v1"), Some("  ")] {
            let route = SuggestionRouteSnapshot {
                provider: ProviderKind::Deepseek,
                provider_identity: "deepseek",
                model: "deepseek-chat",
                authority: &authority,
                actual_base_url,
            };
            assert!(
                plan_suggestion_launch(true, true, 2, Some(route), |_| Some(deepseek_credentials(
                    "deepseek-chat"
                )))
                .is_none(),
                "actual turn endpoint {actual_base_url:?} must fail closed"
            );
        }

        // A trailing-slash-only difference is the same endpoint.
        let route = SuggestionRouteSnapshot {
            provider: ProviderKind::Deepseek,
            provider_identity: "deepseek",
            model: "deepseek-chat",
            authority: &authority,
            actual_base_url: Some("https://api.deepseek.com/v1/"),
        };
        assert!(
            plan_suggestion_launch(true, true, 2, Some(route), |_| Some(deepseek_credentials(
                "deepseek-chat"
            )))
            .is_some(),
            "trailing-slash normalization must not break the exact-route match"
        );
    }

    #[test]
    fn authority_from_a_different_route_fails_closed() {
        // Authority belongs to deepseek-cn; the completed snapshot claims
        // deepseek. Broken provenance must never dispatch.
        let cn = route_authority(
            ProviderKind::Deepseek,
            "deepseek-cn",
            "deepseek-chat",
            "https://api.deepseek.cn/v1",
            "sk-cn",
        );
        let route = SuggestionRouteSnapshot {
            provider: ProviderKind::Deepseek,
            provider_identity: "deepseek",
            model: "deepseek-chat",
            authority: &cn,
            actual_base_url: Some(DEEPSEEK_BASE),
        };
        assert!(
            plan_suggestion_launch(true, true, 2, Some(route), |_| Some(deepseek_credentials(
                "deepseek-chat"
            )))
            .is_none(),
            "an authority captured for another route must fail closed"
        );
    }

    #[test]
    fn missing_route_snapshot_or_disabled_gates_produce_no_request() {
        let resolver = RecordingResolver::new(vec![(
            ProviderKind::Deepseek,
            "deepseek",
            deepseek_credentials("deepseek-chat"),
        )]);
        let authority = deepseek_authority("deepseek-chat");
        let route = snapshot(
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-chat",
            &authority,
        );

        // No route provenance (non-model turn).
        assert!(plan_suggestion_launch(true, true, 4, None, |r| resolver.resolve(r)).is_none());
        // Turn did not complete.
        assert!(
            plan_suggestion_launch(false, true, 4, Some(route), |r| resolver.resolve(r)).is_none()
        );
        // Feature disabled.
        assert!(
            plan_suggestion_launch(true, false, 4, Some(route), |r| resolver.resolve(r)).is_none()
        );
        // Not enough conversation context.
        assert!(
            plan_suggestion_launch(true, true, 1, Some(route), |r| resolver.resolve(r)).is_none()
        );
        // Empty identity is malformed provenance, not a legacy root route.
        let empty_identity = route_authority(
            ProviderKind::Deepseek,
            "  ",
            "deepseek-chat",
            DEEPSEEK_BASE,
            DEEPSEEK_KEY,
        );
        assert!(
            plan_suggestion_launch(
                true,
                true,
                4,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "  ",
                    "deepseek-chat",
                    &empty_identity
                )),
                |r| resolver.resolve(r),
            )
            .is_none()
        );
        // Empty model.
        let empty_model = route_authority(
            ProviderKind::Deepseek,
            "deepseek",
            "",
            DEEPSEEK_BASE,
            DEEPSEEK_KEY,
        );
        assert!(
            plan_suggestion_launch(
                true,
                true,
                4,
                Some(snapshot(
                    ProviderKind::Deepseek,
                    "deepseek",
                    "",
                    &empty_model
                )),
                |r| resolver.resolve(r),
            )
            .is_none()
        );

        assert!(
            resolver.asked().is_empty(),
            "gates must reject before credential resolution, got {:?}",
            resolver.asked()
        );
    }

    /// Every URL-bearing rendered surface in this feature, exercised against a
    /// base URL that carries credentials in both userinfo and query values.
    #[test]
    fn debug_never_renders_credential_material_or_raw_urls() {
        let secret_base = format!(
            "https://{}:{}@api.deepseek.com/v1?api_key={}{}&token={}{}&region=us-east",
            "svc-user", "hunter2", "sk", "-live-abc123", "tok", "-secret-xyz"
        );
        let secrets = [
            DEEPSEEK_KEY.to_string(),
            "svc-user".to_string(),
            "hunter2".to_string(),
            ["sk", "-live-abc123"].concat(),
            ["tok", "-secret-xyz"].concat(),
        ];

        let credentials = credentials(DEEPSEEK_KEY, &secret_base, "deepseek-chat");
        let authority = route_authority(
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-chat",
            &secret_base,
            DEEPSEEK_KEY,
        );
        let route = SuggestionRouteSnapshot {
            provider: ProviderKind::Deepseek,
            provider_identity: "deepseek",
            model: "deepseek-chat",
            authority: &authority,
            actual_base_url: Some(&secret_base),
        };
        let launch =
            plan_suggestion_launch(true, true, 2, Some(route), |_| Some(credentials.clone()))
                .expect("unchanged route must launch");

        for rendered in [
            format!("{credentials:?}"),
            format!("{credentials:#?}"),
            format!("{launch:?}"),
            format!("{launch:#?}"),
            format!("{authority:?}"),
            format!("{authority:#?}"),
            format!("{route:?}"),
            format!("{route:#?}"),
        ] {
            for secret in &secrets {
                assert!(
                    !rendered.contains(secret),
                    "a rendered surface leaked {secret}: {rendered}"
                );
            }
            // The endpoint identity is still useful for diagnostics.
            assert!(
                rendered.contains("api.deepseek.com"),
                "endpoint identity must survive redaction: {rendered}"
            );
            assert!(
                rendered.contains("region=us-east"),
                "non-sensitive query values must survive redaction: {rendered}"
            );
        }
        for rendered in [format!("{credentials:?}"), format!("{launch:?}")] {
            assert!(
                rendered.contains("<redacted>"),
                "Debug must mark the redacted field: {rendered}"
            );
        }
        // …while the launch still dispatches to the real, unredacted endpoint.
        assert_eq!(launch.base_url, secret_base);
        assert_eq!(launch.api_key, DEEPSEEK_KEY);
    }

    // === Config-backed tests ===
    //
    // These drive the real, identity-scoped config resolver and the real
    // client-minted route receipt rather than recording stand-ins, so they
    // cover the actual production path.

    /// Hold the env lock and remove every ambient variable that could displace
    /// the fixture's configured DeepSeek route.
    ///
    /// Without this, a developer shell that exports `DEEPSEEK_API_KEY` (or a
    /// dispatcher-marked `--api-key` forward) can make these tests pass or fail
    /// for reasons that have nothing to do with the privacy contract.
    ///
    /// Field order is load-bearing: the guards must restore the environment
    /// before the lock is released.
    struct SealedDeepseekEnv {
        _guards: Vec<EnvVarGuard>,
        _lock: TestEnvLock,
    }

    fn seal_deepseek_env() -> SealedDeepseekEnv {
        let lock = lock_test_env();
        let guards = [
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_API_KEY_SOURCE",
            "CODEWHALE_CLI_API_KEY",
            "DEEPSEEK_BASE_URL",
            "CODEWHALE_BASE_URL",
        ]
        .into_iter()
        .map(EnvVarGuard::remove)
        .collect();
        SealedDeepseekEnv {
            _guards: guards,
            _lock: lock,
        }
    }

    fn deepseek_config(api_key: &str, base_url: &str) -> Config {
        let mut config = {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        let providers = config
            .providers
            .get_or_insert_with(ProvidersConfig::default);
        providers.deepseek.api_key = Some(api_key.to_string());
        providers.deepseek.base_url = Some(base_url.to_string());
        config
    }

    #[test]
    fn suggestion_vendor_pin_rotation_or_invalid_config_fails_closed() {
        let _env = seal_deepseek_env();
        let mut config = {
            let __cw_config_init_1_0 = Some("openrouter".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                openrouter: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("fixture-openrouter-key".to_string()),
                    base_url: Some("http://127.0.0.1:18080/v1".to_string()),
                    model: Some("fixture/model".to_string()),
                    vendor: Some("chutes/region-fixture".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_1_value: Config = Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let client = crate::client::CodewhaleClient::new(&config).unwrap();
        let authority =
            SuggestionRouteAuthority::from_receipt_for_test(client.turn_route_receipt());
        let route = SuggestionRouteSnapshot {
            provider: ProviderKind::Openrouter,
            provider_identity: "openrouter",
            model: authority.model(),
            authority: &authority,
            actual_base_url: Some(client.base_url()),
        };
        let launch = plan_suggestion_launch_with_config(&config, true, true, 2, Some(route))
            .expect("unchanged OpenRouter pin remains authorized");
        assert_eq!(
            launch.openrouter_vendor.as_deref(),
            Some("chutes/region-fixture")
        );

        for changed_vendor in [Some("another-vendor"), None, Some("bad vendor")] {
            config.providers.as_mut().unwrap().openrouter.vendor =
                changed_vendor.map(str::to_string);
            assert!(
                plan_suggestion_launch_with_config(&config, true, true, 2, Some(route)).is_none(),
                "changed, removed, or invalid vendor must not broaden the completed turn's route"
            );
        }
    }

    /// Build the completed-turn route the engine would have reported, using the
    /// same resolution the engine performs. This keeps the tests correct even
    /// if a model selector normalizes to a different wire id.
    ///
    /// The receipt is minted from the **preflighted client**, exactly as
    /// `Engine::send_message` does — not from config — so these tests exercise
    /// the real provenance chain rather than a re-derivation of it.
    fn deepseek_turn_route(config: &Config) -> TurnRoute {
        let identity = config
            .resolve_provider_identity("deepseek")
            .expect("test config must expose the deepseek identity");
        let resolved = crate::route_runtime::resolve_runtime_route_for_identity(
            config,
            &identity,
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
        )
        .expect("test config must resolve the deepseek route");
        let model = resolved.model.clone();
        let validated = resolved
            .validate()
            .expect("test config must preflight a deepseek client");
        TurnRoute {
            provider: ProviderKind::Deepseek,
            provider_identity: "deepseek".to_string(),
            model,
            auto_model: false,
            receipt: Some(validated.client.turn_route_receipt()),
            billing: Some(crate::core::events::RouteBillingEnvelope {
                billing_surface: None,
                endpoint_fingerprint: None,
                openrouter_vendor: None,
                provider_live_pricing: None,
                billing_mode: crate::cost_status::RouteBillingMode::Unknown,
                dispatched_at: chrono::Utc::now(),
            }),
            base_url: codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL
                .to_string(),
            billing_product: crate::route_billing::RouteProduct::Unproven,
        }
    }

    /// The endpoint `Event::TurnComplete` would report for this config.
    ///
    /// Deliberately derived from the production resolver rather than written
    /// as a literal: `Config::active_route_base_url()` canonicalizes DeepSeek hosts
    /// (it strips a trailing `/v1`), and the transport is bound to the resolved
    /// candidate's endpoint, so a hand-written literal is a different string
    /// than the one the client actually uses.
    fn deepseek_actual_base_url(config: &Config, route: &TurnRoute) -> String {
        resolve_credentials_for_identity(
            config,
            route
                .receipt
                .as_ref()
                .expect("test receipt")
                .admitted_identity(),
            &route.model,
        )
        .expect("test config must resolve the deepseek route")
        .base_url
    }

    /// Redacted mismatch report for a route that unexpectedly failed closed.
    ///
    /// Names the non-secret field that diverged so a future regression is
    /// diagnosable without ever printing a key, a raw URL, or a credential
    /// generation digest.
    fn route_mismatch_report(config: &Config, snapshot: &SuggestionRouteSnapshot<'_>) -> String {
        let resolved = resolve_credentials_for_identity(
            config,
            snapshot.authority.receipt.admitted_identity(),
            snapshot.model.trim(),
        );
        let credential_matches = resolved.as_ref().map(|credentials| {
            snapshot
                .authority
                .authorizes(&credentials.base_url, &credentials.api_key, None)
        });
        format!(
            "snapshot={snapshot:?}, resolved={resolved:?}, \
             resolved_authorized_by_receipt={credential_matches:?}"
        )
    }

    fn config_snapshot<'a>(
        route: &'a TurnRoute,
        authority: &'a SuggestionRouteAuthority,
        actual_base_url: &'a str,
    ) -> SuggestionRouteSnapshot<'a> {
        SuggestionRouteSnapshot {
            provider: route.provider,
            provider_identity: route.provider_identity.as_str(),
            model: route.model.as_str(),
            authority,
            actual_base_url: Some(actual_base_url),
        }
    }

    #[test]
    fn config_exact_unchanged_completed_route_launches() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let route = deepseek_turn_route(&config);
        let actual_base_url = deepseek_actual_base_url(&config, &route);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");

        let snapshot = config_snapshot(&route, &authority, &actual_base_url);
        let launch = plan_suggestion_launch_with_config(&config, true, true, 4, Some(snapshot))
            .unwrap_or_else(|| {
                panic!(
                    "an unchanged deepseek route must launch: {}",
                    route_mismatch_report(&config, &snapshot)
                )
            });

        assert_eq!(launch.base_url, actual_base_url);
        assert_eq!(launch.api_key, DEEPSEEK_KEY);
        assert_eq!(launch.model, route.model);
        // The turn's endpoint is the configured DeepSeek host, canonicalized
        // by the route resolver — not some other provider's endpoint.
        assert!(
            launch.base_url.contains("api.deepseek.com"),
            "unexpected endpoint host: {}",
            endpoint_identity(&launch.base_url)
        );
    }

    #[test]
    fn configured_chat_path_override_fails_closed() {
        let _env = seal_deepseek_env();
        let mut config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        config
            .providers
            .as_mut()
            .expect("providers")
            .deepseek
            .path_suffix = Some("/private/chat".to_string());
        let route = deepseek_turn_route(&config);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");
        let identity = config
            .resolve_provider_identity("deepseek")
            .expect("deepseek identity");
        let actual_base_url = crate::route_runtime::resolve_runtime_route_for_identity(
            &config,
            &identity,
            Some(&route.model),
        )
        .expect("resolved route")
        .candidate
        .endpoint()
        .base_url
        .clone();
        let snapshot = config_snapshot(&route, &authority, &actual_base_url);

        assert!(
            plan_suggestion_launch_with_config(&config, true, true, 4, Some(snapshot)).is_none(),
            "the suggestion helper must not bypass a provider-specific path contract"
        );

        config
            .providers
            .as_mut()
            .expect("providers")
            .deepseek
            .path_suffix = Some("   ".to_string());
        assert!(
            resolve_credentials_for_identity(
                &config,
                route
                    .receipt
                    .as_ref()
                    .expect("test receipt")
                    .admitted_identity(),
                &route.model,
            )
            .is_none(),
            "even a blank configured suffix is an installed transport override"
        );
    }

    /// The #4404/#4411 race, end to end.
    ///
    /// Route A is resolved, preflighted, and installed; the engine mints its
    /// receipt from that client. Config is then mutated to route B *before* the
    /// TUI ever handles `TurnStarted` — which is reachable because web config
    /// events are drained ahead of engine events. Authority must still be A,
    /// and the completed turn's context must not be dispatchable with B.
    #[test]
    fn config_mutated_before_turn_started_cannot_move_authority_off_route_a() {
        let _env = seal_deepseek_env();

        // --- Route A: resolved, preflighted, installed, receipt minted. ---
        const KEY_A: &str = "sk-route-a-secret";
        const BASE_A: &str = "https://api.deepseek.com/v1";
        let config_a = deepseek_config(KEY_A, BASE_A);
        let route = deepseek_turn_route(&config_a);
        let actual_base_url = deepseek_actual_base_url(&config_a, &route);

        // --- Config mutates to route B, still before TurnStarted handling. ---
        const KEY_B: &str = "sk-route-b-attacker";
        const BASE_B: &str = "https://exfil.example.com/v1";
        let config_b = deepseek_config(KEY_B, BASE_B);

        // --- TurnStarted handling. It takes no config, by construction. ---
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");
        assert_eq!(
            authority.endpoint_identity(),
            endpoint_identity(&actual_base_url),
            "authority must describe route A, not whatever config says now"
        );
        assert!(
            !authority.endpoint_identity().contains("exfil.example.com"),
            "authority leaked onto route B: {}",
            authority.endpoint_identity()
        );
        assert!(
            !authority.authorizes(BASE_B, KEY_B, None),
            "route B must not be authorized by route A's receipt"
        );
        assert!(
            authority.authorizes(&actual_base_url, KEY_A, None),
            "route A must still authorize itself"
        );

        // --- TurnComplete: the later suggestion launch is refused. ---
        let snapshot = config_snapshot(&route, &authority, &actual_base_url);
        assert!(
            plan_suggestion_launch_with_config(&config_b, true, true, 4, Some(snapshot)).is_none(),
            "completed-turn context must not be dispatchable under the mutated config"
        );
        // A mutation of only the key, with route A's endpoint intact, is the
        // narrower form of the same race and must also fail closed.
        let key_only_mutation = deepseek_config(KEY_B, BASE_A);
        assert!(
            plan_suggestion_launch_with_config(&key_only_mutation, true, true, 4, Some(snapshot))
                .is_none(),
            "a credential-only mutation must fail closed too"
        );

        // --- Control: under the unmutated config A, the launch happens on A. ---
        let launch = plan_suggestion_launch_with_config(&config_a, true, true, 4, Some(snapshot))
            .unwrap_or_else(|| {
                panic!(
                    "route A must still launch on itself: {}",
                    route_mismatch_report(&config_a, &snapshot)
                )
            });
        assert_eq!(launch.api_key, KEY_A);
        assert_eq!(launch.base_url, actual_base_url);
        assert_ne!(launch.api_key, KEY_B);
        assert!(!launch.base_url.contains("exfil.example.com"));
    }

    #[test]
    fn config_same_identity_base_url_mutation_fails_closed() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let route = deepseek_turn_route(&config);
        // The endpoint the completed turn really used, so this test fails
        // closed on the mutation itself rather than on a stale literal.
        let actual_base_url = deepseek_actual_base_url(&config, &route);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");

        // The web config surface repoints the SAME provider identity at a
        // different endpoint while the turn is still running.
        let mutated = deepseek_config(DEEPSEEK_KEY, "https://exfil.example.com/v1");

        assert!(
            plan_suggestion_launch_with_config(
                &mutated,
                true,
                true,
                4,
                Some(config_snapshot(&route, &authority, &actual_base_url)),
            )
            .is_none(),
            "a same-identity endpoint mutation must send no context anywhere"
        );
    }

    #[test]
    fn config_same_identity_api_key_mutation_fails_closed() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let route = deepseek_turn_route(&config);
        let actual_base_url = deepseek_actual_base_url(&config, &route);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");

        // Same identity, same endpoint, different credential. Everything except
        // the key matches, so only the credential-generation gate can reject.
        let mutated = deepseek_config("sk-attacker-rotated", DEEPSEEK_BASE);
        assert_eq!(
            deepseek_actual_base_url(&mutated, &route),
            actual_base_url,
            "this test must isolate the credential rotation, not an endpoint change"
        );

        assert!(
            plan_suggestion_launch_with_config(
                &mutated,
                true,
                true,
                4,
                Some(config_snapshot(&route, &authority, &actual_base_url)),
            )
            .is_none(),
            "a same-identity credential mutation must send no context anywhere"
        );
    }

    #[test]
    fn config_selection_switch_cannot_redirect_the_completed_route() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let route = deepseek_turn_route(&config);
        let actual_base_url = deepseek_actual_base_url(&config, &route);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");

        // Ordinary UI selection switch: the live provider is now OpenAI, with
        // its own key and endpoint. The completed DeepSeek turn must still
        // resolve DeepSeek — and must never reach the OpenAI route.
        let mut switched = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        switched.provider = Some("openai".to_string());
        {
            let providers = switched
                .providers
                .get_or_insert_with(ProvidersConfig::default);
            providers.openai.api_key = Some("sk-openai-secret".to_string());
            providers.openai.base_url = Some("https://api.openai.com/v1".to_string());
        }

        let snapshot = config_snapshot(&route, &authority, &actual_base_url);
        let launch = plan_suggestion_launch_with_config(&switched, true, true, 4, Some(snapshot))
            .unwrap_or_else(|| {
                panic!(
                    "the completed deepseek route stays valid across a selection switch: {}",
                    route_mismatch_report(&switched, &snapshot)
                )
            });

        assert_eq!(launch.base_url, actual_base_url);
        assert_eq!(launch.api_key, DEEPSEEK_KEY);
        assert_ne!(launch.api_key, "sk-openai-secret");
        assert!(!launch.base_url.contains("openai"));
    }

    #[test]
    fn config_unsupported_providers_capture_no_authority() {
        let _env = seal_deepseek_env();
        // A usable Chat Completions credential exists in this config, and
        // each route below is even handed a receipt. A Messages/Responses
        // completed route must still capture nothing.
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);

        for (provider, identity, model) in [
            (ProviderKind::Anthropic, "anthropic", "claude-sonnet-4"),
            (ProviderKind::OpenaiCodex, "openai-codex", "gpt-5.4"),
            (
                ProviderKind::DeepseekAnthropic,
                "deepseek-anthropic",
                "deepseek-chat",
            ),
        ] {
            let route = TurnRoute {
                provider,
                provider_identity: identity.to_string(),
                model: model.to_string(),
                auto_model: false,
                receipt: Some(receipt(
                    provider,
                    identity,
                    model,
                    DEEPSEEK_BASE,
                    DEEPSEEK_KEY,
                )),
                billing: Some(crate::core::events::RouteBillingEnvelope {
                    billing_surface: None,
                    endpoint_fingerprint: None,
                    openrouter_vendor: None,
                    provider_live_pricing: None,
                    billing_mode: crate::cost_status::RouteBillingMode::Unknown,
                    dispatched_at: chrono::Utc::now(),
                }),
                base_url: DEEPSEEK_BASE.to_string(),
                billing_product: crate::route_billing::RouteProduct::Unproven,
            };
            assert!(
                capture_route_authority(&route).is_none(),
                "{provider:?} must not capture a suggestion authority"
            );
        }

        // The direct credential resolver refuses unsupported providers too, so
        // no later caller can reach a key through it.
        assert!(
            resolve_credentials_for_identity(
                &config,
                &(config).test_identity_for_kind(ProviderKind::DeepseekAnthropic),
                "deepseek-chat",
            )
            .is_none(),
            "DeepseekAnthropic must never reach a credential lookup"
        );
    }

    #[test]
    fn route_without_a_receipt_captures_no_authority() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let mut route = deepseek_turn_route(&config);
        route.receipt = None;
        assert!(
            capture_route_authority(&route).is_none(),
            "a turn with no installed-client receipt has no provenance to trust"
        );
    }

    #[test]
    fn receipt_describing_another_route_captures_no_authority() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let mut route = deepseek_turn_route(&config);
        // Same provider and identity, different wire model than the event's
        // own route: the chain is broken, not merely stale.
        route.receipt = Some(receipt(
            ProviderKind::Deepseek,
            "deepseek",
            "some-other-model",
            DEEPSEEK_BASE,
            DEEPSEEK_KEY,
        ));
        assert!(capture_route_authority(&route).is_none());

        route.receipt = Some(receipt(
            ProviderKind::Deepseek,
            "deepseek-cn",
            &route.model,
            DEEPSEEK_BASE,
            DEEPSEEK_KEY,
        ));
        assert!(capture_route_authority(&route).is_none());
    }

    #[test]
    fn config_requires_the_endpoint_the_turn_actually_used() {
        let _env = seal_deepseek_env();
        let config = deepseek_config(DEEPSEEK_KEY, DEEPSEEK_BASE);
        let route = deepseek_turn_route(&config);
        let actual_base_url = deepseek_actual_base_url(&config, &route);
        let authority =
            capture_route_authority(&route).expect("deepseek turn must capture authority");

        // Control: with the endpoint the turn really used, this route launches.
        // Without it, the two negatives below would prove nothing.
        let baseline = config_snapshot(&route, &authority, &actual_base_url);
        assert!(
            plan_suggestion_launch_with_config(&config, true, true, 4, Some(baseline)).is_some(),
            "baseline route must launch: {}",
            route_mismatch_report(&config, &baseline)
        );

        // `Event::TurnComplete` reported a different endpoint than the one the
        // receipt was minted from: the turn was not on this route.
        let mut snapshot = config_snapshot(&route, &authority, &actual_base_url);
        snapshot.actual_base_url = Some("https://exfil.example.com/v1");
        assert!(
            plan_suggestion_launch_with_config(&config, true, true, 4, Some(snapshot)).is_none(),
            "a completed turn on a different endpoint must fail closed"
        );

        // A missing endpoint is missing provenance, not an implicit match.
        let mut snapshot = config_snapshot(&route, &authority, &actual_base_url);
        snapshot.actual_base_url = None;
        assert!(
            plan_suggestion_launch_with_config(&config, true, true, 4, Some(snapshot)).is_none(),
            "an absent turn endpoint must fail closed"
        );
    }
}
