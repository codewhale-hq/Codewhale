//! Superfast Decision Gate — a small, off-by-default "System One" front-door
//! classifier for the agent turn loop (#6603).
//!
//! Every user message normally wakes a large, slow, expensive model just to
//! decide intent and whether a tool is needed. The Decision Gate asks a small,
//! fast decision model (a Jev-compatible System One endpoint) those routine
//! questions in one non-generating pass, and derives a conservative routing
//! recommendation. A later, validated step could send each turn to the cheapest
//! correct path; this first increment only measures and logs.
//!
//! Design contract:
//!   - Off by default. Nothing runs unless `SUPERFAST_ENABLED` is truthy, and
//!     an enabled gate also needs `SUPERFAST_PROVIDER` naming the route it may
//!     call, so turning the gate on never picks a paid endpoint by itself.
//!   - Shadow mode. The gate classifies the turn and logs a typed
//!     [`ShadowOutcome`] through `tracing` (target `superfast`), but never
//!     changes routing, never skips or delays the model call, and never alters
//!     any user-visible behavior.
//!   - Fail open. Any error, timeout, non-2xx response, unreachable backend,
//!     or malformed body is a typed failure class and the turn continues
//!     exactly as if the gate were off. The call runs on a detached task.
//!   - One transport. The call goes through the existing System One client
//!     (`client::system_one`: `CodewhaleClient::for_decision_route` and
//!     `system_one_decide`) that serves the `[auto.router] kind = "decision"`
//!     router — same auth, TLS, secret redaction and one-attempt policy. There
//!     is no second HTTP client.
//!
//! Configuration (environment, read when a turn starts):
//!   - `SUPERFAST_ENABLED` — `1` / `true` / `yes` / `on` turns the gate on.
//!   - `SUPERFAST_PROVIDER` — `typesafe` or `openrouter` (required when on).
//!     The key comes from the same place the decision router reads it.
//!   - `SUPERFAST_BASE_URL` — optional TypeSafe-route base override, e.g. a
//!     self-hosted Jev server at `http://localhost:8000/v1`; `/systemone` is
//!     appended.
//!   - `SUPERFAST_MODEL` — decision model id (default `jev-latest` for
//!     TypeSafe, `~typesafe/jev-latest` for OpenRouter).
//!   - `SUPERFAST_TIMEOUT_MS` — per-call deadline, 1..=10000 (default 150).
//!
//! Known limits (written down so nobody assumes them):
//!   - Shadow only: the recommendation is logged, never acted on.
//!   - Only the latest user message's text is sent, truncated to 4,000
//!     characters and redacted of configured secrets. No prompt text is
//!     logged; the log carries the route, failure class and latency.
//!   - The TypeSafe route needs a TypeSafe key even for a self-hosted server
//!     (the transport always authenticates); a server that ignores auth can
//!     be given any placeholder key.
//!   - Usage settles through the originating turn's shared ledger. Missing
//!     usage or cancellation after dispatch records a coverage gap. TypeSafe
//!     is an unpriced Custom route until a billing basis is reviewed.
//!   - Misconfiguration while enabled (missing or unknown provider, bad
//!     timeout, missing key) is logged at `warn` for each turn and nothing is
//!     sent.
//!
//! The Decision Gate concept and the reference implementation are by Andrea
//! Bruno, released under Creative Commons Attribution 4.0 (CC BY 4.0); see
//! <https://github.com/Andrea-Bruno/harness-superfast>. The decision models
//! (Von, OpenJev, Laya) are third-party open models; only the integration
//! architecture and the routing method here are covered by that attribution.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use codewhale_core::request::{ContentBlock, Message};
use codewhale_core::role::Role;

use crate::client::CodewhaleClient;
use crate::client::system_one::{DecisionRouterRoute, SystemOneResponse};
use crate::model_routing::{
    AutoRouterFailure, auto_route_usage_source_id, decision_usage_batch, truncate_for_auto_router,
};
use codewhale_runtime::config::Config;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct ShadowUsageContext {
    scope: crate::cost_status::CostScopeToken,
    runtime_owner: Option<String>,
    // Retains the origin's existing durable ledger until settlement finishes.
    _lease: Option<crate::cost_status::RuntimeUsageLease>,
    #[cfg(any(test, feature = "test-support"))]
    test_origin: std::thread::ThreadId,
}

impl ShadowUsageContext {
    fn capture(owner: Option<&str>) -> Self {
        let owner = owner.map(str::trim).filter(|owner| !owner.is_empty());
        Self {
            scope: crate::cost_status::scope_token(),
            runtime_owner: owner.map(str::to_owned),
            _lease: owner.and_then(crate::cost_status::acquire_runtime_usage_lease),
            #[cfg(any(test, feature = "test-support"))]
            test_origin: crate::cost_status::test_cost_scope_id(),
        }
    }

    async fn report(&self, batch: crate::cost_status::RuntimeUsageBatch) {
        let context = self.clone();
        // Existing sinks may write the origin-session ledger. Keep their
        // filesystem/SQLite work off Tokio workers as well.
        let settled = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _origin = crate::cost_status::bind_test_cost_scope(context.test_origin);
            crate::cost_status::report_runtime_usage_batch(
                context.scope,
                context.runtime_owner.as_deref(),
                &batch,
            );
            // Release the lease while its captured test binding is still live.
            drop(context);
        })
        .await;
        if settled.is_err() {
            tracing::warn!(target: "superfast", "decision usage settlement worker failed");
        }
    }
}

/// Master switch. The gate never runs unless this env var is truthy.
const ENABLED_VAR: &str = "SUPERFAST_ENABLED";
/// Which System One route the gate may call: `typesafe` or `openrouter`.
const PROVIDER_VAR: &str = "SUPERFAST_PROVIDER";
/// Optional TypeSafe-route base URL (`/systemone` is appended).
const BASE_URL_VAR: &str = "SUPERFAST_BASE_URL";
/// Decision model id sent in the request body.
const MODEL_VAR: &str = "SUPERFAST_MODEL";
/// Per-call deadline in milliseconds.
const TIMEOUT_VAR: &str = "SUPERFAST_TIMEOUT_MS";

const DEFAULT_TIMEOUT_MS: u64 = 150;
const MAX_TIMEOUT_MS: u64 = 10_000;
/// Characters of the latest user message sent as decision state.
const MAX_STATE_CHARS: usize = 4_000;

/// Conservative routing recommendation derived from a turn's answers. Only a
/// decisive set of numbers produces a fast route; anything else is `Unknown`,
/// which means "fall back to the full model exactly as today".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    NeedsTool,
    AnswerFromContext,
    PlainChat,
    Unknown,
}

impl Route {
    fn as_str(self) -> &'static str {
        match self {
            Route::NeedsTool => "needs_tool",
            Route::AnswerFromContext => "answer_from_context",
            Route::PlainChat => "plain_chat",
            Route::Unknown => "unknown",
        }
    }
}

/// Everything one shadow evaluation can end in. Bounded by construction: a
/// route or a non-secret failure class, plus the measured latency. Provider
/// bodies and prompt text never enter this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowOutcome {
    /// The decision model answered; `route` is the derived recommendation.
    Recommendation { route: Route, latency_ms: u64 },
    /// Fail open: nothing reached the model request. `NotRunnable` means the
    /// route could not be built (no key, bad URL) and nothing was sent.
    Failed {
        failure: AutoRouterFailure,
        latency_ms: u64,
    },
}

/// The route an enabled gate calls, resolved from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowSettings {
    route: DecisionRouterRoute,
    base_url: Option<String>,
    model: String,
    timeout: Duration,
}

impl ShadowSettings {
    /// `None` when the gate is off; `Some(Err)` when it is on but
    /// misconfigured (the message names the variable to fix).
    fn from_env() -> Option<Result<Self, String>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Result<Self, String>> {
        let value = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let enabled = value(ENABLED_VAR).is_some_and(|flag| {
            matches!(
                flag.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        });
        enabled.then(|| Self::parse(&value))
    }

    fn parse(value: &impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let provider = value(PROVIDER_VAR).ok_or_else(|| {
            format!(
                "{ENABLED_VAR} is set but {PROVIDER_VAR} is not; set it to typesafe or openrouter"
            )
        })?;
        let route = DecisionRouterRoute::parse(&provider).ok_or_else(|| {
            format!(
                "{PROVIDER_VAR}={provider:?} is not a decision route; use typesafe or openrouter"
            )
        })?;
        let timeout_ms = match value(TIMEOUT_VAR) {
            None => DEFAULT_TIMEOUT_MS,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|ms| (1..=MAX_TIMEOUT_MS).contains(ms))
                .ok_or_else(|| {
                    format!("{TIMEOUT_VAR}={raw:?} must be 1..={MAX_TIMEOUT_MS} milliseconds")
                })?,
        };
        let model = value(MODEL_VAR).unwrap_or_else(|| {
            match route {
                DecisionRouterRoute::Typesafe => "jev-latest",
                DecisionRouterRoute::Openrouter => "~typesafe/jev-latest",
            }
            .to_string()
        });
        Ok(Self {
            route,
            base_url: value(BASE_URL_VAR),
            model,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

/// Text of the last user message, or `None` when there is none or it is blank.
fn last_user_text(messages: &[Message]) -> Option<String> {
    let message = messages.iter().rev().find(|m| m.role == Role::User)?;
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Fire the shadow gate for a turn's first model request. Returns at once:
/// the detached evaluation uses the turn's cancellation and accounting owner. No task is started
/// (and `None` is returned) when the gate is off or misconfigured, when there
/// is no user text, or when no Tokio runtime is present. The handle exists
/// for tests; the turn loop drops it.
pub fn spawn_shadow_gate(
    config: &Config,
    messages: &[Message],
    runtime_owner: Option<&str>,
    cancel_token: &CancellationToken,
) -> Option<tokio::task::JoinHandle<ShadowOutcome>> {
    let settings = match ShadowSettings::from_env()? {
        Ok(settings) => settings,
        Err(message) => {
            tracing::warn!(target: "superfast", "decision gate (shadow) not run: {message}");
            return None;
        }
    };
    let latest_request = last_user_text(messages)?;
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let config = config.clone();
    // Capture before detaching; the next session/turn must never acquire it.
    let usage_context = ShadowUsageContext::capture(runtime_owner);
    let cancel_token = cancel_token.clone();
    Some(runtime.spawn(async move {
        let outcome = evaluate(
            &config,
            &settings,
            &latest_request,
            &usage_context,
            &cancel_token,
        )
        .await;
        log_outcome(outcome);
        outcome
    }))
}

fn log_outcome(outcome: ShadowOutcome) {
    match outcome {
        ShadowOutcome::Recommendation { route, latency_ms } => tracing::info!(
            target: "superfast",
            route = route.as_str(),
            latency_ms,
            "decision gate (shadow) recommendation"
        ),
        ShadowOutcome::Failed {
            failure: AutoRouterFailure::NotRunnable,
            ..
        } => tracing::warn!(
            target: "superfast",
            "decision gate (shadow) not run: route not runnable (check the {PROVIDER_VAR} key and {BASE_URL_VAR})"
        ),
        ShadowOutcome::Failed {
            failure,
            latency_ms,
        } => tracing::debug!(
            target: "superfast",
            failure = %failure.label(),
            latency_ms,
            "decision gate (shadow) no opinion (fail-open)"
        ),
    }
}

/// One shadow evaluation over the existing System One transport.
async fn evaluate(
    config: &Config,
    settings: &ShadowSettings,
    latest_request: &str,
    usage_context: &ShadowUsageContext,
    cancel_token: &CancellationToken,
) -> ShadowOutcome {
    if cancel_token.is_cancelled() {
        return ShadowOutcome::Failed {
            failure: AutoRouterFailure::Cancelled,
            latency_ms: 0,
        };
    }
    // Client construction resolves keys (environment, secret store), so it
    // runs off the async worker (#6149).
    let built = {
        let config = config.clone();
        let route = settings.route;
        let base_url = settings.base_url.clone();
        #[cfg(any(test, feature = "test-support"))]
        let ticket = crate::test_support::env_scope_ticket();
        tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _membership = crate::test_support::join_env_scope(ticket);
            CodewhaleClient::for_decision_route(&config, route, base_url.as_deref())
        })
        .await
    };
    let Ok(Ok(client)) = built else {
        return ShadowOutcome::Failed {
            failure: AutoRouterFailure::NotRunnable,
            latency_ms: 0,
        };
    };
    let body = decision_body(&client, &settings.model, latest_request);
    let request_route = client.effective_route_envelope(&settings.model, chrono::Utc::now());
    let dispatched = AtomicBool::new(false);
    let started = Instant::now();
    let answer = tokio::select! {
        biased;
        () = cancel_token.cancelled() => Err(AutoRouterFailure::Cancelled),
        result = tokio::time::timeout(settings.timeout, client.system_one_decide(&body, &dispatched)) => {
            result.unwrap_or(Err(AutoRouterFailure::Timeout))
        }
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match answer {
        Err(failure) => {
            if dispatched.load(Ordering::Acquire) {
                usage_context
                    .report(crate::cost_status::RuntimeUsageBatch {
                        decisions: Vec::new(),
                        drop_records: vec![crate::cost_status::RuntimeUsageDropRecord {
                            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
                            source_id: auto_route_usage_source_id(
                                &request_route,
                                "shadow:dispatched-unreceipted",
                            ),
                            route: request_route.sanitized_for_persistence(),
                        }],
                        dropped_records: 1,
                        ..Default::default()
                    })
                    .await;
            }
            ShadowOutcome::Failed {
                failure,
                latency_ms,
            }
        }
        Ok(response) => {
            // Billing evidence settles even when strict answer validation
            // prevents the policy from producing a recommendation.
            let mut batch = decision_usage_batch(&request_route, &response);
            let intent = crate::model_routing::validated_choice(
                response.answers.get("intent"),
                &["code_change", "code_question", "command", "chat", "other"],
            );
            batch
                .decisions
                .push(crate::cost_status::RuntimeDecisionReceipt {
                    source_id: auto_route_usage_source_id(
                        &request_route,
                        response.id.as_deref().unwrap_or("systemone"),
                    ),
                    route: request_route.sanitized_for_persistence(),
                    usage: response.usage.as_ref().map(|u| codewhale_models::Usage {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        ..Default::default()
                    }),
                    usage_complete: response
                        .usage
                        .as_ref()
                        .is_some_and(|u| u.complete && (u.input_tokens > 0 || u.output_tokens > 0)),
                    shadow: true,
                    valid_answers: response.answers_validated != Some(false),
                    evidence: codewhale_models::auto_route_evidence::AutoRouteDecisionEvidence {
                        choice: intent
                            .as_ref()
                            .map_or_else(|| "invalid".to_string(), |v| v.choice.clone()),
                        probabilities_bp: intent
                            .as_ref()
                            .map_or_else(Default::default, |v| v.probabilities_bp.clone()),
                        confidence_bp: intent.as_ref().map_or(0, |v| v.confidence_bp),
                        min_confidence_bp: 5_000,
                        cost_saving_kept_fast: false,
                        thinking: None,
                        provider_reported_cost_usd: response
                            .usage
                            .as_ref()
                            .and_then(|u| u.reported_cost()),
                        latency_ms,
                        response_model: response.model.clone(),
                    },
                });
            usage_context.report(batch).await;
            if response.answers_validated == Some(false) {
                ShadowOutcome::Failed {
                    failure: AutoRouterFailure::InvalidAnswer,
                    latency_ms,
                }
            } else {
                ShadowOutcome::Recommendation {
                    route: derive_route(&response),
                    latency_ms,
                }
            }
        }
    }
}

/// The System One request: two `noul` questions and one `choice` over the
/// redacted, bounded latest request.
fn decision_body(client: &CodewhaleClient, model: &str, latest_request: &str) -> Value {
    json!({
        "model": model,
        "state": {
            "latest_request": client
                .redact_model_bound_text(&truncate_for_auto_router(latest_request, MAX_STATE_CHARS)),
        },
        "questions": {
            "needs_tool": {
                "type": "noul",
                "instructions": "Does answering this request require taking an action with a tool (reading, writing, running, searching), rather than replying from what is already known?"
            },
            "answerable_from_context": {
                "type": "noul",
                "instructions": "Can this request be answered from information already present in the conversation, without any new investigation?"
            },
            "intent": {
                "type": "choice",
                "instructions": "Classify the primary intent of the user request.",
                "criteria": {
                    "code_change": "Create, edit, or delete code or files.",
                    "code_question": "Explain or reason about code without changing it.",
                    "command": "Run a command or operation.",
                    "chat": "Casual conversation or a question needing no tools.",
                    "other": "None of the above."
                }
            }
        }
    })
}

/// A finite probability in [0, 1], or `None` ("no evidence").
fn unit_interval(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

/// Read a `noul` answer only when it is typed as one and its value is a real
/// probability. Anything else (absent, wrong type, out of range) is "no
/// evidence", so a mis-scaled or missing answer can never produce a decisive
/// fast route.
fn read_noul(response: &SystemOneResponse, key: &str) -> Option<f64> {
    let answer = response.answers.get(key)?;
    (answer.kind == "noul")
        .then_some(answer.noul)
        .and_then(unit_interval)
}

/// Derive a conservative route. The gate only recommends a fast route when the
/// relevant numbers are decisive; otherwise it says `Unknown` so the caller
/// falls back to the normal path.
fn derive_route(response: &SystemOneResponse) -> Route {
    let needs_tool = read_noul(response, "needs_tool");
    let from_context = read_noul(response, "answerable_from_context");

    // Decisive "needs a tool" wins first — the harness must not skip work.
    if needs_tool.is_some_and(|nt| nt >= 0.85) {
        return Route::NeedsTool;
    }

    // Strongly answerable from context, with a present and low tool-need signal.
    if from_context.is_some_and(|fc| fc >= 0.85) && needs_tool.is_some_and(|nt| nt <= 0.3) {
        return Route::AnswerFromContext;
    }

    // Clearly chat, with a calibrated intent and a present, low tool-need signal.
    let intent_is_calibrated_chat = crate::model_routing::validated_choice(
        response.answers.get("intent"),
        &["code_change", "code_question", "command", "chat", "other"],
    )
    .is_some_and(|intent| intent.choice == "chat" && intent.confidence_bp >= 5_000);
    if intent_is_calibrated_chat && needs_tool.is_some_and(|nt| nt <= 0.2) {
        return Route::PlainChat;
    }

    Route::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const TYPESAFE_TEST_KEY: &str = "sf-typesafe-test-key-0123456789";

    /// Fields drop in declaration order: restore the environment before the
    /// lock is released, or another test observes our overrides.
    struct Env {
        _guards: Vec<crate::test_support::EnvVarGuard>,
        _home: tempfile::TempDir,
        _lock: crate::test_support::TestEnvLock,
    }

    /// A hermetic home with a TypeSafe key. `gate` switches the gate on
    /// against the TypeSafe route at that server with that deadline (ms).
    fn hermetic_env(gate: Option<(&MockServer, u64)>) -> Env {
        use crate::test_support::EnvVarGuard;
        let lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("test home");
        let mut guards = vec![
            EnvVarGuard::set("CODEWHALE_HOME", home.path()),
            EnvVarGuard::remove("OPENROUTER_API_KEY"),
            EnvVarGuard::set("TYPESAFE_API_KEY", TYPESAFE_TEST_KEY),
            EnvVarGuard::remove(MODEL_VAR),
        ];
        match gate {
            Some((server, timeout_ms)) => guards.extend([
                EnvVarGuard::set(ENABLED_VAR, "1"),
                EnvVarGuard::set(PROVIDER_VAR, "typesafe"),
                EnvVarGuard::set(BASE_URL_VAR, format!("{}/v1", server.uri())),
                EnvVarGuard::set(TIMEOUT_VAR, timeout_ms.to_string()),
            ]),
            None => guards.extend([
                EnvVarGuard::remove(ENABLED_VAR),
                EnvVarGuard::remove(PROVIDER_VAR),
                EnvVarGuard::remove(BASE_URL_VAR),
                EnvVarGuard::remove(TIMEOUT_VAR),
            ]),
        }
        Env {
            _guards: guards,
            _home: home,
            _lock: lock,
        }
    }

    /// A chat provider the client can be built from; the gate never calls it.
    fn config() -> Config {
        {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = Some("deepseek-v4-pro".to_string());
            let __cw_config_init_0_2 = Some(codewhale_runtime::config::ProvidersConfig {
                deepseek: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("ds-test-key".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
            __cw_config_init_0_value.providers = __cw_config_init_0_2;
            __cw_config_init_0_value
        }
    }

    fn text(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    fn turn(latest: &str) -> Vec<Message> {
        vec![
            text(Role::User, "earlier question"),
            text(Role::Assistant, "earlier answer"),
            text(Role::User, latest),
        ]
    }

    fn noul_body(needs_tool: f64, from_context: f64) -> Value {
        json!({
            "id": "sf-1",
            "model": "jev-latest",
            "usage": { "input_tokens": 121, "output_tokens": 8 },
            "answers": {
                "needs_tool": { "type": "noul", "noul": needs_tool },
                "answerable_from_context": { "type": "noul", "noul": from_context },
                "intent": {
                    "type": "choice",
                    "choice": "code_change",
                    "probabilities": {
                        "code_change": 0.9, "code_question": 0.04, "command": 0.03,
                        "chat": 0.02, "other": 0.01
                    },
                    "confidence": 0.8
                }
            }
        })
    }

    async fn requests(server: &MockServer) -> Vec<Request> {
        server.received_requests().await.expect("recorded")
    }

    async fn run(messages: &[Message]) -> Option<ShadowOutcome> {
        let handle = spawn_shadow_gate(&config(), messages, None, &CancellationToken::new())?;
        Some(handle.await.expect("shadow task"))
    }

    #[tokio::test]
    async fn disabled_gate_sends_nothing_and_starts_no_task() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(noul_body(0.9, 0.1)))
            .mount(&server)
            .await;
        let _env = hermetic_env(None);

        assert_eq!(run(&turn("Refactor the parser")).await, None);
        assert!(
            requests(&server).await.is_empty(),
            "a disabled gate must not call"
        );
    }

    #[tokio::test]
    async fn enabled_gate_posts_one_redacted_bounded_decision_and_recommends() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .and(header(
                "authorization",
                format!("Bearer {TYPESAFE_TEST_KEY}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(noul_body(0.93, 0.1)))
            .expect(1)
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 2_000)));

        // The latest message echoes the key and is longer than the state cap.
        let latest = format!(
            "Fix the build. token={TYPESAFE_TEST_KEY} {}",
            "x".repeat(6_000)
        );
        let outcome = run(&turn(&latest)).await.expect("enabled gate runs");
        assert!(
            matches!(
                outcome,
                ShadowOutcome::Recommendation {
                    route: Route::NeedsTool,
                    ..
                }
            ),
            "{outcome:?}"
        );

        let requests = requests(&server).await;
        assert_eq!(requests.len(), 1);
        let raw = String::from_utf8(requests[0].body.clone()).expect("utf8 body");
        assert!(
            !raw.contains(TYPESAFE_TEST_KEY),
            "key leaked into decision state"
        );
        assert!(
            !raw.contains("earlier question"),
            "only the latest message is sent"
        );
        let body: Value = serde_json::from_str(&raw).expect("json body");
        assert_eq!(body["model"], "jev-latest");
        let state = body["state"]["latest_request"]
            .as_str()
            .expect("state text");
        assert!(state.starts_with("Fix the build."));
        assert!(
            state.chars().count() <= MAX_STATE_CHARS + 3,
            "state is bounded"
        );
        let questions: Vec<&str> = body["questions"]
            .as_object()
            .expect("questions")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            questions,
            ["needs_tool", "answerable_from_context", "intent"]
        );
    }

    #[tokio::test]
    async fn http_error_and_malformed_answers_fail_open_as_typed_classes() {
        for (response, expected) in [
            (
                ResponseTemplate::new(500).set_body_string("upstream exploded: secret body"),
                AutoRouterFailure::Http { status: 500 },
            ),
            (
                ResponseTemplate::new(200).set_body_string("not json"),
                AutoRouterFailure::InvalidAnswer,
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/systemone"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let _env = hermetic_env(Some((&server, 2_000)));

            let outcome = run(&turn("Explain the diff"))
                .await
                .expect("enabled gate runs");
            assert!(
                matches!(outcome, ShadowOutcome::Failed { failure, .. } if failure == expected),
                "{outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn slow_endpoint_times_out_without_holding_up_the_caller() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(noul_body(0.93, 0.1))
                    .set_delay(Duration::from_secs(3)),
            )
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 100)));

        let started = Instant::now();
        let handle = spawn_shadow_gate(
            &config(),
            &turn("Run the tests"),
            None,
            &CancellationToken::new(),
        )
        .expect("task");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "spawning must not wait on the decision call"
        );
        let outcome = handle.await.expect("shadow task");
        assert!(
            matches!(
                outcome,
                ShadowOutcome::Failed {
                    failure: AutoRouterFailure::Timeout,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the deadline bounds the call"
        );
    }

    #[tokio::test]
    async fn no_op_and_unrunnable_turns_send_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(noul_body(0.9, 0.1)))
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 2_000)));

        // No user text: no task.
        assert_eq!(run(&[text(Role::Assistant, "only assistant")]).await, None);
        assert_eq!(run(&turn("   ")).await, None);

        // No TypeSafe key: the route is not runnable and nothing is sent.
        let _no_key = crate::test_support::EnvVarGuard::remove("TYPESAFE_API_KEY");
        let outcome = run(&turn("Refactor the parser"))
            .await
            .expect("enabled gate runs");
        assert_eq!(
            outcome,
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::NotRunnable,
                latency_ms: 0,
            }
        );
        assert!(requests(&server).await.is_empty());
    }

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
        }
    }

    #[test]
    fn settings_are_off_by_default_and_misconfiguration_is_named() {
        assert_eq!(ShadowSettings::from_lookup(lookup(&[])), None);
        assert_eq!(
            ShadowSettings::from_lookup(lookup(&[(ENABLED_VAR, "0"), (PROVIDER_VAR, "typesafe")])),
            None
        );
        let missing = ShadowSettings::from_lookup(lookup(&[(ENABLED_VAR, "on")]))
            .expect("enabled")
            .expect_err("no provider");
        assert!(missing.contains(PROVIDER_VAR), "{missing}");
        let unknown =
            ShadowSettings::from_lookup(lookup(&[(ENABLED_VAR, "1"), (PROVIDER_VAR, "deepseek")]))
                .expect("enabled")
                .expect_err("not a decision route");
        assert!(unknown.contains("deepseek"), "{unknown}");
        let timeout = ShadowSettings::from_lookup(lookup(&[
            (ENABLED_VAR, "1"),
            (PROVIDER_VAR, "openrouter"),
            (TIMEOUT_VAR, "0"),
        ]))
        .expect("enabled")
        .expect_err("zero timeout");
        assert!(timeout.contains(TIMEOUT_VAR), "{timeout}");
        let settings = ShadowSettings::from_lookup(lookup(&[
            (ENABLED_VAR, "yes"),
            (PROVIDER_VAR, "openrouter"),
        ]))
        .expect("enabled")
        .expect("valid");
        assert_eq!(settings.route, DecisionRouterRoute::Openrouter);
        assert_eq!(settings.model, "~typesafe/jev-latest");
        assert_eq!(settings.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(settings.base_url, None);
    }

    fn response(answers: Value) -> SystemOneResponse {
        serde_json::from_value(json!({ "answers": answers })).expect("response")
    }

    #[test]
    fn only_decisive_typed_answers_route_fast() {
        let cases = [
            (
                json!({ "needs_tool": { "type": "noul", "noul": 0.9 } }),
                Route::NeedsTool,
            ),
            (
                json!({
                    "needs_tool": { "type": "noul", "noul": 0.1 },
                    "answerable_from_context": { "type": "noul", "noul": 0.9 }
                }),
                Route::AnswerFromContext,
            ),
            (
                json!({
                    "needs_tool": { "type": "noul", "noul": 0.05 },
                    "intent": { "type": "choice", "choice": "chat", "confidence": 0.8,
                        "probabilities": { "chat": 0.9, "code_change": 0.04, "code_question": 0.03, "command": 0.02, "other": 0.01 } }
                }),
                Route::PlainChat,
            ),
            // Out of range, wrong type, or untyped answers are no evidence.
            (
                json!({
                    "needs_tool": { "type": "noul", "noul": 5.0 },
                    "answerable_from_context": { "type": "choice", "noul": 0.95 }
                }),
                Route::Unknown,
            ),
            (json!({ "needs_tool": { "noul": 0.95 } }), Route::Unknown),
            // Chat without a present, low tool-need signal is not decisive.
            (
                json!({ "intent": { "type": "choice", "choice": "chat", "confidence": 0.9 } }),
                Route::Unknown,
            ),
            (json!({}), Route::Unknown),
        ];
        for (answers, expected) in cases {
            assert_eq!(
                derive_route(&response(answers.clone())),
                expected,
                "{answers}"
            );
        }
    }

    #[test]
    fn last_user_text_picks_the_latest_user_text() {
        assert_eq!(
            last_user_text(&turn("  latest question  ")).as_deref(),
            Some("latest question")
        );
        assert_eq!(
            last_user_text(&[text(Role::Assistant, "only assistant")]),
            None
        );
    }
    #[tokio::test]
    async fn cancelled_before_dispatch_sends_nothing() {
        let server = MockServer::start().await;
        let _env = hermetic_env(Some((&server, 2_000)));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = spawn_shadow_gate(&config(), &turn("hi"), None, &cancel)
            .expect("task")
            .await
            .expect("joined");
        assert!(matches!(
            outcome,
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::Cancelled,
                ..
            }
        ));
        assert!(requests(&server).await.is_empty());
    }

    #[tokio::test]
    async fn cancelled_dispatched_call_records_gap_for_its_owner() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(2))
                    .set_body_json(noul_body(0.9, 0.1)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 3_000)));
        let owner = "superfast-cancel-owner";
        let dropped = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = dropped.clone();
        crate::cost_status::register_runtime_usage_sink_with_drop(
            owner,
            std::sync::Arc::new(|_| true),
            Some(std::sync::Arc::new(move |record| {
                sink.lock().expect("drop sink").push(record);
                true
            })),
        );

        let cancel = CancellationToken::new();
        let handle = spawn_shadow_gate(&config(), &turn("hi"), Some(owner), &cancel).expect("task");
        tokio::time::timeout(Duration::from_secs(1), async {
            while requests(&server).await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request admitted");
        let start = Instant::now();
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_millis(300), handle)
            .await
            .expect("cancellation stops the pending request")
            .expect("joined");
        assert!(matches!(
            outcome,
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::Cancelled,
                ..
            }
        ));
        assert!(start.elapsed() < Duration::from_millis(300));
        let records = dropped.lock().expect("drop records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].route.provider_identity, "typesafe");
        let batch = crate::cost_status::take_runtime_usage(owner);
        assert!(batch.records.is_empty());
        assert!(batch.drop_records.is_empty());
        crate::cost_status::finish_runtime_usage_owner(owner);
    }

    #[tokio::test]
    async fn owner_lease_retains_late_usage_and_invalid_answer_cost() {
        let server = MockServer::start().await;
        let mut body = noul_body(0.9, 0.1);
        body["answers"]["needs_tool"]["noul"] = 2.0.into();
        body["usage"]["cost"] = 0.000012054.into();
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_json(body),
            )
            .expect(1)
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 2_000)));
        let owner = "superfast-late-owner";
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = observed.clone();
        crate::cost_status::register_runtime_usage_sink(
            owner,
            std::sync::Arc::new(move |record| {
                sink.lock().expect("sink").push(record);
                true
            }),
        );
        let decisions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let decision_sink = decisions.clone();
        crate::cost_status::register_runtime_decision_sink(
            owner,
            std::sync::Arc::new(move |receipt| {
                decision_sink.lock().expect("decision sink").push(receipt);
                true
            }),
        );
        let handle = spawn_shadow_gate(
            &config(),
            &turn("hi"),
            Some(owner),
            &CancellationToken::new(),
        )
        .expect("task");
        crate::cost_status::finish_runtime_usage_owner(owner);
        let outcome = handle.await.expect("joined");
        assert!(matches!(
            outcome,
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::InvalidAnswer,
                ..
            }
        ));
        let records = observed.lock().expect("records");
        assert_eq!(
            records.len(),
            1,
            "retired origin still owns its late response"
        );
        assert_eq!(records[0].usage.usage.input_tokens, 121);
        assert_eq!(records[0].usage.usage.output_tokens, 8);
        assert_eq!(records[0].usage.route.provider_identity, "typesafe");
        assert_eq!(
            records[0].usage.route.billing_mode,
            crate::cost_status::RouteBillingMode::Unknown
        );
        let receipts = decisions.lock().expect("decision receipts");
        assert_eq!(receipts.len(), 1);
        assert!(!receipts[0].valid_answers);
        assert!(receipts[0].shadow);
        assert_eq!(
            receipts[0].evidence.provider_reported_cost_usd.as_deref(),
            Some("0.000012054")
        );
        assert!(
            crate::cost_status::take_runtime_usage(owner)
                .records
                .is_empty(),
            "settled response must not enter fallback journal"
        );
    }

    #[tokio::test]
    async fn oversized_decision_body_fails_closed_with_dispatched_coverage_gap() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(256 * 1024 + 1)))
            .expect(1)
            .mount(&server)
            .await;
        let _env = hermetic_env(Some((&server, 2_000)));
        let owner = "superfast-oversized-owner";
        let drops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = drops.clone();
        crate::cost_status::register_runtime_usage_sink_with_drop(
            owner,
            std::sync::Arc::new(|_| true),
            Some(std::sync::Arc::new(move |record| {
                sink.lock().expect("drops").push(record);
                true
            })),
        );
        let handle = spawn_shadow_gate(
            &config(),
            &turn("hi"),
            Some(owner),
            &CancellationToken::new(),
        )
        .expect("task");
        assert!(matches!(
            handle.await.expect("joined"),
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::InvalidAnswer,
                ..
            }
        ));
        assert_eq!(drops.lock().expect("drop receipts").len(), 1);
        crate::cost_status::finish_runtime_usage_owner(owner);
    }
    #[tokio::test]
    async fn empty_origin_and_receipt_only_batch_use_the_existing_interactive_pool() {
        let _scope = crate::cost_status::test_scope();
        let context = ShadowUsageContext::capture(Some("  "));
        assert!(context.runtime_owner.is_none());
        context
            .report(crate::cost_status::RuntimeUsageBatch {
                decisions: vec![crate::cost_status::decision_receipt_fixture(
                    "ownerless-fixture",
                )],
                ..Default::default()
            })
            .await;
        let projected = crate::cost_status::drain();
        assert!(
            projected
                .route_receipts
                .iter()
                .any(|r| r.contains("0.000012054"))
        );
        assert_eq!(
            projected.unpriced_turns, 0,
            "diagnostic receipts do not mint a new charge"
        );
        assert!(
            crate::cost_status::take_runtime_usage("  ")
                .decisions
                .is_empty()
        );
    }
}
