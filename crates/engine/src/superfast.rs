//! Superfast Decision Gate — a small, off-by-default "System One" front-door
//! classifier that rides the decision router's request (#6603).
//!
//! Every user message normally wakes a large, slow, expensive model just to
//! decide intent and whether a tool is needed. The Decision Gate asks a small,
//! fast decision model (a Jev-compatible System One endpoint) those routine
//! questions in one non-generating pass, and derives a conservative routing
//! recommendation. A later, validated step could send each turn to the cheapest
//! correct path; this increment only measures and logs.
//!
//! Design contract:
//!   - Off by default. Nothing is asked unless `[auto.router]` has
//!     `kind = "decision"` and `shadow_gate = true`.
//!   - One call. The gate's three questions are appended to the decision
//!     router's own System One request (`model_routing`): same route, key,
//!     model, `timeout_secs`, redacted state and usage record. There is no
//!     second request, client, deadline or configuration surface.
//!   - Shadow mode. The gate logs a typed [`ShadowOutcome`] through `tracing`
//!     (target `superfast`) and publishes one shadow decision receipt, but
//!     never changes routing and never alters any user-visible behavior.
//!   - Fail open. A router failure of any class is the gate's failure class
//!     and the turn continues exactly as the router's fallback decides.
//!
//! Known limits (written down so nobody assumes them):
//!   - Shadow only: the recommendation is logged, never acted on.
//!   - The gate runs only when the decision router runs: Auto model mode with
//!     a runnable strong/fast pair. It no longer fires on every turn.
//!   - The transport validates every offered question, so a decision server
//!     that cannot answer the gate's questions fails the router's answer too
//!     and the turn takes the router's local fallback.
//!   - The router's state (bounded recent context plus the latest request,
//!     truncated to 4,000 characters and redacted of configured secrets) is
//!     what the gate's questions are asked over. No prompt text is logged;
//!     the log carries the route, failure class and latency.
//!   - The `SUPERFAST_*` environment variables are no longer read. Setting
//!     any of them logs one warning per process that names the config key.
//!
//! The Decision Gate concept and the reference implementation are by Andrea
//! Bruno, released under Creative Commons Attribution 4.0 (CC BY 4.0); see
//! <https://github.com/Andrea-Bruno/harness-superfast>. The decision models
//! (Von, OpenJev, Laya) are third-party open models; only the integration
//! architecture and the routing method here are covered by that attribution.

use serde_json::{Map, Value, json};

use crate::client::system_one::SystemOneResponse;
use crate::cost_status::{
    EffectiveRouteEnvelope, NativeUsageSource, RuntimeDecisionReceipt, RuntimeUsageBatch,
};
use crate::model_routing::AutoRouterFailure;

const REMOVED_ENV_VARS: [&str; 5] = [
    "SUPERFAST_ENABLED",
    "SUPERFAST_PROVIDER",
    "SUPERFAST_BASE_URL",
    "SUPERFAST_MODEL",
    "SUPERFAST_TIMEOUT_MS",
];

const INTENT_OPTIONS: [&str; 5] = ["code_change", "code_question", "command", "chat", "other"];
const GATE_QUESTIONS: [&str; 3] = ["intent", "needs_tool", "answerable_from_context"];

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
    /// Fail open: the router's call gave the gate nothing to read.
    /// `NotRunnable` means the route could not be built (no key, bad URL)
    /// and nothing was sent.
    Failed {
        failure: AutoRouterFailure,
        latency_ms: u64,
    },
}

/// One line naming the replacement key when a removed `SUPERFAST_*` variable
/// is still set, or `None` when none is.
pub fn removed_env_notice(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let set: Vec<&str> = REMOVED_ENV_VARS
        .into_iter()
        .filter(|name| lookup(*name).is_some_and(|value| !value.trim().is_empty()))
        .collect();
    (!set.is_empty()).then(|| {
        format!(
            "ignoring {}: the shadow Decision Gate is now `shadow_gate = true` under `[auto.router]` with `kind = \"decision\"`",
            set.join(", ")
        )
    })
}

/// Log [`removed_env_notice`] once per process.
pub fn warn_on_removed_env() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        if let Some(notice) = removed_env_notice(|name| std::env::var(name).ok()) {
            tracing::warn!(target: "superfast", "{notice}");
        }
    });
}

/// The gate's two `noul` questions and one `choice`, appended to the decision
/// router's request when `[auto.router] shadow_gate = true`.
pub(crate) fn gate_questions() -> Map<String, Value> {
    match json!({
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
    }) {
        Value::Object(questions) => questions,
        _ => Map::new(),
    }
}

/// Record that the router's call produced no answer for the gate to read.
pub(crate) fn record_shadow_failure(failure: AutoRouterFailure, latency_ms: u64) -> ShadowOutcome {
    let outcome = ShadowOutcome::Failed {
        failure,
        latency_ms,
    };
    log_outcome(outcome);
    outcome
}

/// Derive, log and receipt the shadow recommendation from the router's own
/// response. Usage is settled by the router's record; this adds evidence only.
pub(crate) async fn record_shadow_response(
    response: &SystemOneResponse,
    latency_ms: u64,
    request_route: &EffectiveRouteEnvelope,
    source: &NativeUsageSource,
) -> ShadowOutcome {
    let intent =
        crate::model_routing::validated_choice(response.answers.get("intent"), &INTENT_OPTIONS);
    let receipt = RuntimeDecisionReceipt {
        source_id: source.source_id(),
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
            abstained_questions: response.abstained_questions(&GATE_QUESTIONS),
            cost_saving_kept_fast: false,
            thinking: None,
            provider_reported_cost_usd: response.usage.as_ref().and_then(|u| u.reported_cost()),
            latency_ms,
            response_model: response
                .model
                .as_deref()
                .map(|model| model.chars().take(128).collect()),
        },
    };
    report_receipt(source, receipt).await;
    let outcome = if response.answers_validated == Some(false) {
        ShadowOutcome::Failed {
            failure: AutoRouterFailure::InvalidAnswer,
            latency_ms,
        }
    } else {
        ShadowOutcome::Recommendation {
            route: derive_route(response),
            latency_ms,
        }
    };
    log_outcome(outcome);
    outcome
}

async fn report_receipt(source: &NativeUsageSource, receipt: RuntimeDecisionReceipt) {
    let origin = source.origin().cloned();
    let scope = origin.as_ref().map_or_else(
        crate::cost_status::scope_token,
        crate::cost_status::NativeUsageOrigin::scope,
    );
    #[cfg(any(test, feature = "test-support"))]
    let test_origin = crate::cost_status::test_cost_scope_id();
    let settled = codewhale_runtime::runtime_control::owner_work(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _origin = crate::cost_status::bind_test_cost_scope(test_origin);
        crate::cost_status::record_runtime_usage_batch(
            scope,
            origin.as_ref(),
            &RuntimeUsageBatch {
                decisions: vec![receipt],
                ..Default::default()
            },
        );
        Ok(())
    })
    .await;
    if settled.is_err() {
        tracing::warn!(target: "superfast", "decision receipt settlement worker failed");
    }
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
            "decision gate (shadow) not run: route not runnable (check the router key and [auto.router])"
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
    (answer.kind == "noul" && !answer.abstained)
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
    let intent_is_calibrated_chat =
        crate::model_routing::validated_choice(response.answers.get("intent"), &INTENT_OPTIONS)
            .is_some_and(|intent| {
                !intent.abstained && intent.choice == "chat" && intent.confidence_bp >= 5_000
            });
    if intent_is_calibrated_chat && needs_tool.is_some_and(|nt| nt <= 0.2) {
        return Route::PlainChat;
    }

    Route::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_abstention_never_authorizes_a_shadow_route() {
        let mut value = json!({"answers":{
            "intent":{"type":"choice","choice":"chat","confidence":0.9,
                "probabilities":{"chat":0.9,"code_change":0.1,"code_question":0.0,"command":0.0,"other":0.0}},
            "needs_tool":{"type":"noul","noul":0.1},
            "answerable_from_context":{"type":"noul","noul":0.1}}});
        let decoded =
            |value: Value| serde_json::from_value::<SystemOneResponse>(value).expect("response");
        assert_eq!(derive_route(&decoded(value.clone())), Route::PlainChat);
        value["answers"]["intent"]["abstained"] = true.into();
        assert_eq!(derive_route(&decoded(value.clone())), Route::Unknown);
        value["answers"]["needs_tool"]["noul"] = 0.95.into();
        value["answers"]["needs_tool"]["abstained"] = true.into();
        assert_eq!(derive_route(&decoded(value)), Route::Unknown);
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
    fn removed_environment_variables_name_the_router_key_in_one_line() {
        assert_eq!(removed_env_notice(|_| None), None);
        assert_eq!(
            removed_env_notice(|name| (name == "SUPERFAST_MODEL").then(|| "  ".to_string())),
            None,
            "a blank value is not a setting"
        );
        let notice = removed_env_notice(|name| {
            matches!(name, "SUPERFAST_ENABLED" | "SUPERFAST_TIMEOUT_MS").then(|| "1".to_string())
        })
        .expect("notice");
        assert!(
            notice.contains("SUPERFAST_ENABLED, SUPERFAST_TIMEOUT_MS"),
            "{notice}"
        );
        assert!(notice.contains("shadow_gate = true"), "{notice}");
        assert!(notice.contains("[auto.router]"), "{notice}");
        assert!(!notice.contains('\n'), "{notice}");
    }

    #[test]
    fn gate_offers_its_three_typed_questions() {
        let questions = gate_questions();
        let names: Vec<&str> = questions.keys().map(String::as_str).collect();
        assert_eq!(names, ["needs_tool", "answerable_from_context", "intent"]);
        let intent: Vec<&str> = questions["intent"]["criteria"]
            .as_object()
            .expect("criteria")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(intent, INTENT_OPTIONS);
    }

    #[tokio::test]
    async fn router_response_yields_one_shadow_receipt_and_keeps_rejected_cost() {
        let decisions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let decision_sink = decisions.clone();
        let origin = crate::cost_status::NativeUsageOrigin::captured_runtime_with_sinks(
            crate::cost_status::scope_token(),
            "superfast-owner".to_string(),
            std::sync::Arc::new(|_record: crate::cost_status::RuntimeUsageRecord| true),
            None,
            Some(std::sync::Arc::new(
                move |receipt: crate::cost_status::RuntimeDecisionReceipt| {
                    decision_sink.lock().expect("decision sink").push(receipt);
                    true
                },
            )),
        );
        let source = NativeUsageSource::capture_with_origin(
            "auto-router:shadow-test",
            crate::cost_status::UsageSpendOwner::Auxiliary,
            Some(origin),
        );
        let route = crate::cost_status::decision_receipt_fixture("route").route;
        let mut body = noul_body(0.93, 0.1);
        body["usage"]["cost"] = 0.000012054.into();
        let mut response: SystemOneResponse =
            serde_json::from_str(&body.to_string()).expect("response");

        assert_eq!(
            record_shadow_response(&response, 42, &route, &source).await,
            ShadowOutcome::Recommendation {
                route: Route::NeedsTool,
                latency_ms: 42
            }
        );
        response.answers_validated = Some(false);
        assert_eq!(
            record_shadow_response(&response, 43, &route, &source).await,
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::InvalidAnswer,
                latency_ms: 43
            }
        );

        let receipts = decisions.lock().expect("decision receipts");
        assert_eq!(receipts.len(), 2);
        assert!(receipts.iter().all(|receipt| receipt.shadow));
        assert!(
            receipts
                .iter()
                .all(|receipt| receipt.source_id == "auto-router:shadow-test")
        );
        assert!(receipts[0].valid_answers);
        assert_eq!(receipts[0].evidence.choice, "code_change");
        assert!(!receipts[1].valid_answers);
        assert_eq!(
            receipts[1].evidence.provider_reported_cost_usd.as_deref(),
            Some("0.000012054")
        );
    }

    #[test]
    fn router_failure_is_the_gate_failure_class() {
        assert_eq!(
            record_shadow_failure(AutoRouterFailure::Timeout, 2_000),
            ShadowOutcome::Failed {
                failure: AutoRouterFailure::Timeout,
                latency_ms: 2_000
            }
        );
    }

    #[tokio::test]
    async fn ownerless_receipt_uses_the_existing_interactive_pool() {
        let _scope = crate::cost_status::test_scope();
        let source = NativeUsageSource::capture(
            "auto-router:ownerless",
            crate::cost_status::UsageSpendOwner::Auxiliary,
        );
        report_receipt(
            &source,
            crate::cost_status::decision_receipt_fixture("ownerless-fixture"),
        )
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
    }
}
