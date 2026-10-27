//! System One decision API on the existing client (#6525).
//!
//! `[auto.router] kind = "decision"` asks a non-generative decision model
//! (TypeSafe's Jev) one typed Choice per turn. The wire is a plain JSON
//! `POST to the route’s decision endpoint` — not chat completions — served on two routes:
//!
//! * **OpenRouter** — `https://openrouter.ai/api/alpha/decisions` with the user's OpenRouter
//!   key, built exactly like every other OpenRouter client.
//! * **TypeSafe direct** — `https://api.typesafe.ai/v1/systemone` with a
//!   TypeSafe key.
//!
//! This is a child of `client` so it reuses the client's auth headers, TLS,
//! redaction and bounded retry plumbing; there is no second HTTP client.
//!
//! Known limits (written down so nobody assumes them):
//! * TypeSafe is **not** an [`ProviderKind`]: it serves no chat route, so it is
//!   the decision router's own endpoint + key (`TYPESAFE_API_KEY`, the
//!   `typesafe` secret-store slot, or `[providers.typesafe] api_key` /
//!   `api_key_env`). Its tokens enter the shared usage ledger under a frozen
//!   `custom` / `typesafe` route with unknown billing; reported cost is retained
//!   on decision receipts. Unknown pricing is never interpreted as free.
//! * The routing call makes one attempt (no retry): it is bounded by the
//!   router timeout, and a retried decision would arrive after the turn has
//!   already fallen back.
//! * The router parses the `choice` answer shape; the shadow Decision Gate
//!   (`crate::superfast`) also reads `noul`. The transport strictly validates
//!   Choice, Noul and fractional Score responses before either policy uses them.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::value::RawValue;

use super::*;
use crate::model_routing::AutoRouterFailure;

/// TypeSafe's direct API base (the `/systemone` path is appended).
pub(crate) const TYPESAFE_DEFAULT_BASE_URL: &str = "https://api.typesafe.ai/v1";
/// Environment variable holding a TypeSafe API key.
pub(crate) const TYPESAFE_API_KEY_ENV: &str = "TYPESAFE_API_KEY";
/// Secret-store slot and `[providers.<name>]` table name for the TypeSafe key.
pub(crate) const TYPESAFE_KEY_NAME: &str = "typesafe";
/// Bound a non-generative decision response before allocating/decoding it.
const DECISION_RESPONSE_MAX_BYTES: usize = 256 * 1024;
const DECISION_REQUEST_MAX_BYTES: usize = 1024 * 1024;

/// Which endpoint serves a decision router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecisionRouterRoute {
    Openrouter,
    Typesafe,
}

impl DecisionRouterRoute {
    /// Parse an `[auto.router] provider` value for a decision router.
    #[must_use]
    pub(crate) fn parse(provider: &str) -> Option<Self> {
        let provider = provider.trim();
        if ProviderKind::parse(provider) == Some(ProviderKind::Openrouter) {
            Some(Self::Openrouter)
        } else if provider.eq_ignore_ascii_case(TYPESAFE_KEY_NAME)
            || provider.eq_ignore_ascii_case("typesafe-ai")
        {
            Some(Self::Typesafe)
        } else {
            None
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Openrouter => "openrouter",
            Self::Typesafe => TYPESAFE_KEY_NAME,
        }
    }

    #[must_use]
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Openrouter => "OpenRouter",
            Self::Typesafe => "TypeSafe",
        }
    }

    /// Whether a credential for this route is present (never returns it).
    #[must_use]
    pub(crate) fn has_key(self, config: &Config) -> bool {
        match self {
            Self::Openrouter => config
                .builtin_provider_identity(ProviderKind::Openrouter)
                .is_ok_and(|identity| {
                    crate::provider_readiness::has_api_key_for(config, &identity)
                }),
            Self::Typesafe => typesafe_api_key(config).is_some(),
        }
    }
}

/// Resolve the TypeSafe key: environment, then the durable secret store, then
/// `[providers.typesafe] api_key` / `api_key_env`.
pub(crate) fn typesafe_api_key(config: &Config) -> Option<String> {
    let non_empty = |value: String| (!value.trim().is_empty()).then(|| value.trim().to_string());
    if let Some(key) = std::env::var(TYPESAFE_API_KEY_ENV).ok().and_then(non_empty) {
        return Some(key);
    }
    if let Some(key) = codewhale_runtime::config::credential_secret_store()
        .and_then(|store| store.get(TYPESAFE_KEY_NAME).ok().flatten())
        .and_then(non_empty)
    {
        return Some(key);
    }
    let entry = config
        .providers
        .as_ref()
        .and_then(|providers| providers.custom_provider_config(TYPESAFE_KEY_NAME))?;
    entry.api_key.clone().and_then(non_empty).or_else(|| {
        entry
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .and_then(|name| std::env::var(name).ok())
            .and_then(non_empty)
    })
}

/// A decoded System One response. Unknown fields are ignored.
#[derive(Debug, Clone)]
pub(crate) struct SystemOneResponse {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) answers: BTreeMap<String, SystemOneAnswer>,
    pub(crate) usage: Option<SystemOneUsage>,
    /// Transport validation is separate from decoding so a rejected policy
    /// answer still preserves the provider's usage/cost evidence.
    pub(crate) answers_validated: Option<bool>,
}

impl<'de> Deserialize<'de> for SystemOneResponse {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            id: Option<Box<RawValue>>,
            #[serde(default)]
            model: Option<Box<RawValue>>,
            #[serde(default)]
            answers: Option<Box<RawValue>>,
            #[serde(default)]
            usage: Option<Box<RawValue>>,
        }
        let wire = Envelope::deserialize(deserializer)?;
        let text = |raw: Option<&RawValue>| {
            raw.and_then(|raw| serde_json::from_str::<String>(raw.get()).ok())
        };
        let id = text(wire.id.as_deref());
        let model = text(wire.model.as_deref());
        let malformed_identity =
            (wire.id.is_some() && id.is_none()) || (wire.model.is_some() && model.is_none());
        // Invalid policy fields must fail validation without discarding the
        // separately reported billing evidence. Empty/default answers never
        // satisfy the required question types or probabilities.
        let answers: BTreeMap<String, Box<RawValue>> = wire
            .answers
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or_default();
        Ok(Self {
            id,
            model,
            answers: answers
                .into_iter()
                .map(|(key, raw)| (key, serde_json::from_str(raw.get()).unwrap_or_default()))
                .collect(),
            usage: wire
                .usage
                .and_then(|raw| serde_json::from_str(raw.get()).ok()),
            answers_validated: malformed_identity.then_some(false),
        })
    }
}

/// One answer. The `choice` and `noul` subsets are interpreted.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SystemOneAnswer {
    #[serde(rename = "type", default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) choice: Option<String>,
    /// A `noul` answer's probability; validated by its reader.
    #[serde(default)]
    pub(crate) noul: Option<f64>,
    #[serde(default)]
    pub(crate) score: Option<f64>,
    #[serde(default)]
    pub(crate) legend: BTreeMap<String, Value>,
    #[serde(default)]
    pub(crate) probabilities: BTreeMap<String, Option<f64>>,
    #[serde(default)]
    pub(crate) confidence: Option<f64>,
}

/// Provider usage. OpenRouter adds `cost`; the token-count casing differs
/// between surfaces, so both spellings are accepted.
#[derive(Debug, Clone)]
pub(crate) struct SystemOneUsage {
    /// Kept verbatim so the receipt never re-renders a float.
    pub(crate) cost: Option<Box<RawValue>>,
    pub(crate) input_tokens: u32,
    pub(crate) output_tokens: u32,
    pub(crate) complete: bool,
}

impl<'de> Deserialize<'de> for SystemOneUsage {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut wire = BTreeMap::<String, Box<RawValue>>::deserialize(deserializer)?;
        let count = |snake: &str, camel: &str| match (wire.get(snake), wire.get(camel)) {
            (Some(raw), None) | (None, Some(raw)) => serde_json::from_str::<u32>(raw.get()).ok(),
            // Missing, malformed or ambiguous counts cannot authorize policy
            // or a priced subtotal; keep an independently valid raw cost.
            _ => None,
        };
        let input_tokens = count("input_tokens", "inputTokens");
        let output_tokens = count("output_tokens", "outputTokens");
        Ok(Self {
            complete: input_tokens.is_some() && output_tokens.is_some(),
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            cost: wire.remove("cost"),
        })
    }
}

impl SystemOneUsage {
    /// The provider-reported cost as its verbatim JSON decimal, when it is a
    /// finite, non-negative number.
    #[must_use]
    pub(crate) fn reported_cost(&self) -> Option<String> {
        let raw = self.cost.as_ref()?.get().trim();
        let value: f64 = raw.parse().ok()?;
        (raw.len() <= 128 && value.is_finite() && value >= 0.0).then(|| raw.to_string())
    }
}

impl CodewhaleClient {
    /// Build the client that serves `route`.
    ///
    /// OpenRouter is the ordinary OpenRouter client (its key, base URL,
    /// attribution headers). TypeSafe re-points a clone of the active route's
    /// client — keeping its retry, TLS and redaction policy — at the TypeSafe
    /// endpoint with the TypeSafe key only. Its captured request budget and
    /// remote-control ownership continue to apply to the one decision attempt.
    pub(crate) fn for_decision_route(
        config: &Config,
        route: DecisionRouterRoute,
        base_url_override: Option<&str>,
    ) -> Result<Self> {
        match route {
            DecisionRouterRoute::Openrouter => {
                let mut scoped = config.clone();
                scoped.provider = Some(ProviderKind::Openrouter.as_str().to_string());
                // The decision model id is not a chat route; it travels in the
                // JSON body only.
                scoped.default_text_model = None;
                Self::new(&scoped)
            }
            DecisionRouterRoute::Typesafe => {
                let key = typesafe_api_key(config).with_context(|| {
                    format!("TypeSafe API key not configured ({TYPESAFE_API_KEY_ENV})")
                })?;
                let base_url = base_url_override
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .unwrap_or(TYPESAFE_DEFAULT_BASE_URL)
                    .trim_end_matches('/')
                    .to_string();
                validate_base_url_security(&base_url, false)?;
                let mut client = Self::new(config)?;
                client.http_client = Self::http_client_builder_with_auth_mode(
                    &key,
                    &HashMap::new(),
                    ProviderKind::Custom,
                    &base_url,
                    WireFormat::ChatCompletions,
                    false,
                    client.force_http1,
                    config,
                    AuthHeaderSurface::Inference,
                )?
                .build()?;
                client.base_url = base_url;
                client.http1_client = client.http_client.clone();
                // Repointing auth/URL must also replace the inherited chat
                // route identity. No TypeSafe price/product is guessed.
                client.api_provider = ProviderKind::Custom;
                // The fixed decision authority owns this validated key/URL;
                // ordinary route admission supplies only its exact identity.
                let mut route_config = config.clone();
                route_config.provider = Some(TYPESAFE_KEY_NAME.into());
                route_config
                    .providers
                    .get_or_insert_with(codewhale_runtime::config::ProvidersConfig::default)
                    .custom
                    .insert(
                        TYPESAFE_KEY_NAME.into(),
                        codewhale_runtime::config::ProviderConfig {
                            kind: Some("openai-compatible".into()),
                            base_url: Some(client.base_url.clone()),
                            api_key: Some(key.clone()),
                            ..codewhale_runtime::config::ProviderConfig::default()
                        },
                    );
                client.admitted_identity = route_config
                    .active_provider_identity()
                    .map_err(anyhow::Error::msg)?;
                client.openrouter_vendor = None;
                client.billing_surface = None;
                client.billing_mode = crate::cost_status::RouteBillingMode::Unknown;
                client.route_limits = None;
                // `Self::new` froze the redaction set from the chat provider's
                // secrets; the TypeSafe key is none of them, so add it before
                // any decision body is built from untrusted context.
                let mut secrets = client.model_bound_secret_values.as_ref().clone();
                push_model_bound_secret(&mut secrets, Some(&key));
                client.model_bound_secret_values = Arc::new(secrets);
                client.api_key = key;
                Ok(client)
            }
        }
    }

    /// `POST to the route’s decision endpoint` once, isolated like the Auto chat classifier:
    /// no global retry banners or response cache; shared admission still applies.
    ///
    /// Only a failure class leaves this function — provider error bodies can
    /// echo the prompt and must never reach receipts.
    ///
    /// `dispatched` is set once both permits are held and the request is
    /// handed to the transport, so a caller whose deadline cancels this
    /// future can tell a possibly-billed request from one never sent.
    pub(crate) async fn system_one_decide(
        &self,
        body: &Value,
        dispatched: &std::sync::atomic::AtomicBool,
    ) -> std::result::Result<SystemOneResponse, AutoRouterFailure> {
        if serde_json::to_vec(body).map_or(true, |bytes| bytes.len() > DECISION_REQUEST_MAX_BYTES)
            || !valid_decision_request(body)
        {
            return Err(AutoRouterFailure::NotRunnable);
        }
        let mut isolated = self.clone();
        isolated.isolated_request_state = true;
        isolated.retry.max_retries = 0;
        let _inference = isolated.acquire_remote_control_inference_permit().await;
        let _permit = isolated.acquire_provider_request_permit().await;
        let url = if isolated.api_provider == ProviderKind::Openrouter {
            // The OpenRouter Decisions API is a sibling of /api/v1, so keep
            // the configured origin/proxy prefix and replace only /v1.
            let mut url = reqwest::Url::parse(&isolated.base_url)
                .map_err(|_| AutoRouterFailure::NotRunnable)?;
            let prefix = url.path().trim_end_matches('/').trim_end_matches("/v1");
            url.set_path(&format!("{prefix}/alpha/decisions"));
            url.to_string()
        } else {
            api_url(&isolated.base_url, "systemone")
        };
        dispatched.store(true, std::sync::atomic::Ordering::Release);
        let response = isolated
            .send_json_with_retry(&url, body)
            .await
            .map_err(|error| router_failure_from_error(&error))?;
        let text = bounded_provider_catalog_text(response, DECISION_RESPONSE_MAX_BYTES)
            .await
            .map_err(|error| match error {
                CatalogRefreshError::Network => AutoRouterFailure::Transport,
                _ => AutoRouterFailure::InvalidAnswer,
            })?;
        let mut response: SystemOneResponse =
            serde_json::from_str(&text).map_err(|_| AutoRouterFailure::InvalidAnswer)?;
        response.answers_validated = Some(valid_decision_response(body, &response));
        response.model = response.model.map(|model| {
            self.redact_model_bound_text(&model)
                .chars()
                .take(128)
                .collect()
        });
        Ok(response)
    }
}

fn valid_decision_request(body: &Value) -> bool {
    let Some(questions) = body.get("questions").and_then(Value::as_object) else {
        return false;
    };
    let supported_value =
        |value: &Value| matches!(value, Value::String(_) | Value::Object(_) | Value::Array(_));
    let model = body.get("model").and_then(Value::as_str);
    model.is_some_and(|m| !m.trim().is_empty() && m.len() <= 256)
        && body.get("state").is_some_and(|state| {
            matches!(state, Value::String(_) | Value::Object(_) | Value::Array(_))
        })
        && !questions.is_empty()
        && questions.len() <= 64
        && questions
            .values()
            .all(|q| match q.get("type").and_then(Value::as_str) {
                Some("noul") => true,
                Some("choice") => q
                    .get("criteria")
                    .and_then(Value::as_object)
                    .is_some_and(|c| {
                        !c.is_empty()
                            && c.len() <= 64
                            && c.iter().all(|(name, v)| {
                                !name.trim().is_empty()
                                    && name.len() <= 128
                                    && (v.is_null() || supported_value(v))
                            })
                    }),
                Some("score") => q
                    .get("criteria")
                    .and_then(Value::as_array)
                    .is_some_and(|c| {
                        !c.is_empty() && c.len() <= 10 && c.iter().all(supported_value)
                    }),
                _ => false,
            })
}

fn valid_decision_response(body: &Value, response: &SystemOneResponse) -> bool {
    let Some(questions) = body.get("questions").and_then(Value::as_object) else {
        return false;
    };
    response.answers_validated != Some(false)
        && response
            .model
            .as_deref()
            .is_some_and(|m| !m.trim().is_empty() && m.len() <= 256)
        && response.usage.as_ref().is_some_and(|usage| usage.complete)
        && response.answers.len() == questions.len()
        && questions.iter().all(|(name, question)| {
            let Some(answer) = response.answers.get(name) else {
                return false;
            };
            if Some(answer.kind.as_str()) != question.get("type").and_then(Value::as_str) {
                return false;
            }
            match answer.kind.as_str() {
                "noul" => answer
                    .noul
                    .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
                "choice" => {
                    let Some(criteria) = question.get("criteria").and_then(Value::as_object) else {
                        return false;
                    };
                    let options = criteria.keys().map(String::as_str).collect::<Vec<_>>();
                    crate::model_routing::validated_choice(Some(answer), &options).is_some()
                }
                "score" => {
                    let Some(criteria) = question.get("criteria").and_then(Value::as_array) else {
                        return false;
                    };
                    if criteria.is_empty() || criteria.len() > 10 {
                        return false;
                    }
                    let Some(score) = answer.score else {
                        return false;
                    };
                    if !score.is_finite()
                        || !(0.0..=(criteria.len() - 1) as f64).contains(&score)
                        || !answer
                            .confidence
                            .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                        || answer.legend.len() != criteria.len()
                        || answer.probabilities.len() != criteria.len()
                    {
                        return false;
                    }
                    let mut sum = 0.0;
                    let mut expected_score = 0.0;
                    for (level, criterion) in criteria.iter().enumerate() {
                        let key = level.to_string();
                        let Some(Some(probability)) = answer.probabilities.get(&key) else {
                            return false;
                        };
                        if answer.legend.get(&key) != Some(criterion)
                            || !probability.is_finite()
                            || !(0.0..=1.0).contains(probability)
                        {
                            return false;
                        }
                        sum += probability;
                        expected_score += level as f64 * probability;
                    }
                    (sum - 1.0).abs() <= 0.02 && (score - expected_score).abs() <= 0.02
                }
                _ => false,
            }
        })
}

/// Collapse a client error into a non-secret failure class. The HTTP status
/// is kept where the error carries it; the body never is.
pub(crate) fn router_failure_from_error(error: &anyhow::Error) -> AutoRouterFailure {
    let Some(error) = error.downcast_ref::<LlmError>() else {
        return AutoRouterFailure::Transport;
    };
    match error {
        LlmError::RateLimited { .. } => AutoRouterFailure::Http { status: 429 },
        LlmError::ServerError { status, .. } | LlmError::InvalidRequest { status, .. } => {
            AutoRouterFailure::Http { status: *status }
        }
        LlmError::AuthenticationError(_) => AutoRouterFailure::Http { status: 401 },
        LlmError::AuthorizationError(_) => AutoRouterFailure::Http { status: 403 },
        LlmError::QuotaExhausted(_) => AutoRouterFailure::QuotaExhausted,
        LlmError::ModelError(_)
        | LlmError::ContextLengthError(_)
        | LlmError::ContentPolicyError(_) => AutoRouterFailure::Rejected,
        LlmError::Other(message) => message
            .strip_prefix("HTTP ")
            .and_then(|rest| rest.split(':').next())
            .and_then(|status| status.trim().parse::<u16>().ok())
            .map_or(AutoRouterFailure::Transport, |status| {
                AutoRouterFailure::Http { status }
            }),
        LlmError::NetworkError(_) | LlmError::Timeout(_) | LlmError::ParseError(_) => {
            AutoRouterFailure::Transport
        }
    }
}

#[cfg(test)]
mod decisions_compatibility_tests {
    use super::*;

    fn request() -> Value {
        json!({"model":"typesafe/jev-1.13", "state":{"ticket":"charged twice"}, "questions": {
            "intent": {"type":"choice", "criteria":{"billing":"money", "other":"anything else"}},
            "refund": {"type":"noul", "instructions":"Does it ask for a refund?"},
            "urgency": {"type":"score", "criteria":["Can wait", "Needs attention this week", "Needs attention today"]}
        }})
    }

    fn response() -> Value {
        json!({"id":"fixture-decision", "model":"typesafe/jev-1.13-20260917", "usage":{"input_tokens":287,"output_tokens":20,"cost":0.000012054}, "answers": {
            "intent":{"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.9,"other":0.1}},
            "refund":{"type":"noul","noul":0.99},
            "urgency":{"type":"score","score":1.7,"confidence":0.9,"legend":{"0":"Can wait","1":"Needs attention this week","2":"Needs attention today"},"probabilities":{"0":0.1,"1":0.1,"2":0.8}}
        }})
    }

    #[test]
    fn all_documented_primitives_validate_without_rescaling_score() {
        let decoded: SystemOneResponse =
            serde_json::from_value(response()).expect("documented fixture");
        assert!(valid_decision_request(&request()));
        assert!(valid_decision_response(&request(), &decoded));
        assert_eq!(decoded.answers["urgency"].score, Some(1.7));
        assert_eq!(
            decoded.usage.expect("usage").reported_cost().as_deref(),
            Some("0.000012054")
        );
    }

    #[test]
    fn wrong_types_unoffered_choices_scores_and_partial_shapes_are_rejected() {
        let changes = [
            ("/answers/refund/noul", json!(1.01)),
            ("/answers/refund/type", json!("score")),
            ("/answers/intent/choice", json!("not-offered")),
            ("/answers/intent/choice", json!("other")),
            ("/answers/intent/confidence", json!(-0.1)),
            ("/answers/intent/probabilities", json!({"billing":0.9})),
            (
                "/answers/intent/probabilities",
                json!({"billing":0.6,"other":0.6}),
            ),
            ("/answers/urgency/score", json!(2.01)),
            ("/answers/urgency/score", json!(0.5)),
            ("/answers/urgency/legend/1", json!("different rubric")),
            ("/answers/urgency/probabilities/1", Value::Null),
            ("/answers/urgency/confidence", json!(1.1)),
            ("/model", Value::Null),
            ("/usage", Value::Null),
            ("/usage", json!({"input_tokens": 287, "cost":0.000012054})),
            ("/answers", json!({})),
        ];
        for (pointer, value) in changes {
            let mut body = response();
            *body.pointer_mut(pointer).expect("fixture field") = value;
            let decoded: SystemOneResponse =
                serde_json::from_value(body).expect("structural decode");
            assert!(
                !valid_decision_response(&request(), &decoded),
                "accepted {pointer}"
            );
        }
    }

    #[test]
    fn non_finite_in_memory_answers_and_unbounded_request_shapes_are_rejected() {
        let mut decoded: SystemOneResponse = serde_json::from_value(response()).expect("fixture");
        decoded.answers.get_mut("refund").expect("noul").noul = Some(f64::NAN);
        assert!(!valid_decision_response(&request(), &decoded));
        let mut decoded: SystemOneResponse = serde_json::from_value(response()).expect("fixture");
        decoded.answers.get_mut("urgency").expect("score").score = Some(f64::INFINITY);
        assert!(!valid_decision_response(&request(), &decoded));
        for shape in [
            json!({}),
            json!({"model":"jev", "state":"hello", "questions":{}}),
            json!({"model":"jev", "state":"hello", "questions":{"q":{"type":"score","criteria":vec!["level";11]}}}),
        ] {
            assert!(!valid_decision_request(&shape));
        }
    }
    #[tokio::test]
    async fn decision_transport_preserves_typesafe_auth_and_shared_admission_before_dispatch() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .and(header("authorization", "Bearer decision-fixture-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response()))
            .expect(1)
            .mount(&server)
            .await;
        let _lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _key =
            crate::test_support::EnvVarGuard::set(TYPESAFE_API_KEY_ENV, "decision-fixture-key");
        let config = {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                deepseek: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("chat-fixture-key".into()),
                    max_concurrency: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let base = format!("{}/v1", server.uri());
        let ticket = crate::test_support::env_scope_ticket();
        let client = tokio::task::spawn_blocking(move || {
            let _membership = crate::test_support::join_env_scope(ticket);
            CodewhaleClient::for_decision_route(&config, DecisionRouterRoute::Typesafe, Some(&base))
        })
        .await
        .expect("worker")
        .expect("client");
        assert_eq!(client.provider_request_concurrency_limit(), Some(1));
        assert!(
            client.remote_control_inference_participant,
            "the cloned budget must remain an attached inference participant"
        );
        let clone = client.clone();
        let held = client
            .acquire_provider_request_permit()
            .await
            .expect("shared permit");
        let dispatched = std::sync::atomic::AtomicBool::new(false);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                clone.system_one_decide(&request(), &dispatched)
            )
            .await
            .is_err()
        );
        assert!(
            !dispatched.load(std::sync::atomic::Ordering::Acquire),
            "a queued request has no dispatched-spend marker"
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
        drop(held);
        let answer = client
            .system_one_decide(&request(), &dispatched)
            .await
            .expect("one admitted request");
        assert!(dispatched.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(answer.answers_validated, Some(true));
        assert_eq!(
            client
                .effective_route_envelope("jev-latest", chrono::Utc::now())
                .provider_identity,
            "typesafe"
        );
    }
    #[test]
    fn decision_partial_usage_retains_raw_cost_but_cannot_authorize_policy() {
        let mut body = response();
        body["usage"] = json!({"inputTokens": 287, "cost": 0.000012054});
        let decoded: SystemOneResponse = serde_json::from_value(body).expect("partial receipt");
        assert!(!valid_decision_response(&request(), &decoded));
        let usage = decoded.usage.expect("reported usage");
        assert!(!usage.complete);
        assert_eq!(usage.input_tokens, 287);
        assert_eq!(usage.reported_cost().as_deref(), Some("0.000012054"));
        let route = crate::cost_status::decision_receipt_fixture("partial").route;
        let mut body = response();
        body["usage"] = json!({"inputTokens": 287, "cost": 0.000012054});
        let response: SystemOneResponse = serde_json::from_value(body).expect("partial response");
        let batch = crate::model_routing::decision_usage_batch(&route, &response);
        assert!(
            batch.records.is_empty(),
            "a partial token count cannot produce a complete priced subtotal"
        );
        assert_eq!(batch.dropped_records, 1);
        assert_eq!(batch.drop_records.len(), 1);
        let mut invalid = request();
        invalid["questions"]["urgency"]["criteria"] = json!([null]);
        assert!(!valid_decision_request(&invalid));
    }

    #[test]
    fn malformed_policy_fields_retain_independent_raw_cost() {
        for (pointer, value) in [
            ("/answers/intent/choice", json!(17)),
            ("/answers/intent/confidence", json!("high")),
            ("/answers/refund", Value::Null),
            ("/answers", json!([])),
            ("/model", json!({"invalid":"model"})),
            ("/id", json!([])),
        ] {
            let mut body = response();
            *body.pointer_mut(pointer).expect("fixture field") = value;
            let decoded: SystemOneResponse =
                serde_json::from_value(body).expect("billing envelope");
            assert!(
                !valid_decision_response(&request(), &decoded),
                "accepted {pointer}"
            );
            let usage = decoded
                .usage
                .expect("independent usage survives rejected policy");
            assert!(usage.complete);
            assert_eq!(usage.input_tokens, 287);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.reported_cost().as_deref(), Some("0.000012054"));
        }
    }

    #[test]
    fn malformed_or_overflowed_counters_retain_cost_without_priced_usage() {
        for counters in [
            json!({"input_tokens": u64::MAX, "output_tokens":20}),
            json!({"input_tokens": -1, "output_tokens":20}),
            json!({"input_tokens": "287", "output_tokens":20}),
            json!({"input_tokens": 287, "inputTokens":287, "output_tokens":20}),
            json!({"input_tokens": 287, "output_tokens":1.5}),
        ] {
            let mut body = response();
            body["usage"] = counters;
            body["usage"]["cost"] = json!(0.000012054);
            let decoded: SystemOneResponse =
                serde_json::from_value(body).expect("billing envelope");
            assert!(!valid_decision_response(&request(), &decoded));
            let usage = decoded.usage.as_ref().expect("independent cost");
            assert!(!usage.complete);
            assert_eq!(usage.reported_cost().as_deref(), Some("0.000012054"));
            let route = crate::cost_status::decision_receipt_fixture("malformed-count").route;
            let batch = crate::model_routing::decision_usage_batch(&route, &decoded);
            assert!(
                batch.records.is_empty(),
                "invalid counters cannot fabricate priced usage"
            );
            assert_eq!(batch.dropped_records, 1);
            assert_eq!(batch.drop_records.len(), 1);
        }
    }
}
