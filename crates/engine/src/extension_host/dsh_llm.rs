use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use codewhale_config::OwnerRef;
use codewhale_execpolicy::tool_rules::tool_denied;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::dsh_broker::{
    BrokerCall, BrokerContext, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, decode,
};
use super::protocol::DshService;
use crate::core::engine::rlm_host::{CapturedRlmCaller, RlmInvocation, RlmMode, RlmPromptPolicy};
use crate::llm_client::ProviderUsageCoverage;
use crate::rlm::bridge::{RlmUsageAccumulator, RlmUsageSnapshot};
use crate::tools::rlm::RLM_TOOL_NAME;
use crate::tools::spec::ToolContext;

pub const SESSION_PROVIDER: &str = "session";
const MAX_PROMPT_BYTES: usize = 256 * 1024;
const MAX_SYSTEM_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 256;
const DEFAULT_OUTPUT_TOKENS: u32 = 4096;
const MAX_OUTPUT_TOKENS: u32 = 16_384;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_IN_FLIGHT: u32 = 2;
const MAX_IN_FLIGHT_ROWS: usize = 256;
const LOG: &str = "codewhale::dsh_llm";

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum LlmMethod {
    #[serde(rename = "catalog")]
    Catalog(Empty),
    #[serde(rename = "complete")]
    Complete(CompleteInput),
}

impl BrokerMethods for LlmMethod {
    const NAMES: &'static [&'static str] = &["catalog", "complete"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteInput {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn invalid(message: &str) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Llm,
        message: message.to_string(),
    }
}

fn refused(message: &str) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Llm,
        message: message.to_string(),
    }
}

fn denied(message: &str) -> BrokerRefusal {
    BrokerRefusal::Denied {
        service: DshService::Llm,
        message: message.to_string(),
    }
}

#[derive(Clone)]
enum Gate {
    Live(Arc<BrokerContext>),
    #[cfg(test)]
    Stub,
}

impl Gate {
    async fn act(
        &self,
        method: &str,
        effect: BrokerEffect,
        target: Option<&str>,
    ) -> Result<(), BrokerRefusal> {
        match self {
            Self::Live(context) => context.act(method, effect, target).await,
            #[cfg(test)]
            Self::Stub => Ok(()),
        }
    }

    fn record(&self, entry: Value) -> bool {
        match self {
            Self::Live(context) => context.record(entry),
            #[cfg(test)]
            Self::Stub => true,
        }
    }
}

type FlightKey = (String, String);

fn flights() -> &'static Mutex<HashMap<FlightKey, u32>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<FlightKey, u32>>> = OnceLock::new();
    FLIGHTS.get_or_init(Mutex::default)
}

fn flight_key(owner: &OwnerRef, session_id: &str) -> FlightKey {
    (owner.plugin_id.clone(), session_id.to_string())
}

fn with_flights<T>(act: impl FnOnce(&mut HashMap<FlightKey, u32>) -> T) -> T {
    act(&mut flights()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

#[cfg(test)]
fn in_flight_count(key: &FlightKey) -> u32 {
    with_flights(|table| table.get(key).copied().unwrap_or(0))
}

fn reserve(table: &mut HashMap<FlightKey, u32>, key: &FlightKey) -> Result<(), BrokerRefusal> {
    if !table.contains_key(key) && table.len() >= MAX_IN_FLIGHT_ROWS {
        return Err(refused("the llm broker is busy"));
    }
    let count = table.entry(key.clone()).or_insert(0);
    if *count >= MAX_IN_FLIGHT {
        return Err(refused(
            "this plugin already has model calls running in this session",
        ));
    }
    *count += 1;
    Ok(())
}

fn release(table: &mut HashMap<FlightKey, u32>, key: &FlightKey) {
    if let Some(count) = table.get_mut(key) {
        if *count <= 1 {
            table.remove(key);
        } else {
            *count -= 1;
        }
    }
}

struct RecordMeta {
    plugin_id: String,
    session_id: String,
    provider: String,
    model: String,
    prompt_bytes: usize,
    prompt_sha256: String,
    system_bytes: usize,
    system_sha256: Option<String>,
    started: Instant,
}

fn record_completion(
    meta: &RecordMeta,
    gate: &Gate,
    outcome: &str,
    observed: Option<&Observed>,
) -> u64 {
    let duration_ms = u64::try_from(meta.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (provider, model) = observed
        .and_then(|obs| obs.route.clone())
        .unwrap_or_else(|| (meta.provider.clone(), meta.model.clone()));
    let tokens = observed.and_then(|obs| obs.tokens.as_ref());
    let requests = observed.map_or(0, |obs| obs.requests);
    let coverage = observed.map_or("unknown", |obs| obs.coverage);
    tracing::info!(
        target: LOG,
        plugin = %meta.plugin_id,
        session = %meta.session_id,
        outcome,
        provider = %provider,
        model = %model,
        coverage,
        input_tokens = tokens.map(|tokens| tokens.input),
        output_tokens = tokens.map(|tokens| tokens.output),
        requests,
        duration_ms,
        prompt_bytes = meta.prompt_bytes,
        prompt_sha256 = %meta.prompt_sha256,
        system_bytes = meta.system_bytes,
        "auxiliary model call for a plugin"
    );
    let mut entry = json!({
        "method": "complete",
        "outcome": outcome,
        "provider": provider,
        "model": model,
        "requests": requests,
        "duration_ms": duration_ms,
        "prompt_bytes": meta.prompt_bytes,
        "prompt_sha256": meta.prompt_sha256,
        "system_bytes": meta.system_bytes,
    });
    if let Some(system_sha256) = &meta.system_sha256 {
        entry["system_sha256"] = json!(system_sha256);
    }
    if let Some(tokens) = tokens {
        entry["input_tokens"] = json!(tokens.input);
        entry["output_tokens"] = json!(tokens.output);
    }
    gate.record(entry);
    duration_ms
}

struct Flight {
    key: FlightKey,
    gate: Gate,
    meta: RecordMeta,
    settled: bool,
}

impl Flight {
    fn admit(key: FlightKey, gate: Gate, meta: RecordMeta) -> Result<Self, BrokerRefusal> {
        with_flights(|table| reserve(table, &key))?;
        Ok(Self {
            key,
            gate,
            meta,
            settled: false,
        })
    }

    fn settle(mut self, outcome: &str, observed: &Observed) -> u64 {
        self.settled = true;
        with_flights(|table| release(table, &self.key));
        record_completion(&self.meta, &self.gate, outcome, Some(observed))
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        with_flights(|table| release(table, &self.key));
        record_completion(&self.meta, &self.gate, "unknown", None);
    }
}

struct Tokens {
    input: u64,
    output: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
}

struct Observed {
    route: Option<(String, String)>,
    coverage: &'static str,
    tokens: Option<Tokens>,
    requests: usize,
}

impl Observed {
    fn of(snapshot: &RlmUsageSnapshot) -> Self {
        let route = snapshot
            .records
            .iter()
            .map(|record| &record.usage.route)
            .chain(snapshot.drop_records.iter().map(|record| &record.route))
            .next()
            .map(|route| (route.provider_identity.clone(), route.model.clone()));
        let requests = snapshot.records.len() + snapshot.drop_records.len();
        let clean = snapshot.drop_records.is_empty() && snapshot.dropped_records == 0;
        let coverages: Vec<Option<ProviderUsageCoverage>> = snapshot
            .records
            .iter()
            .map(|record| record.settlement.as_ref().map(|revision| revision.coverage))
            .collect();
        let all = |wanted: ProviderUsageCoverage| {
            !coverages.is_empty() && coverages.iter().all(|coverage| *coverage == Some(wanted))
        };
        let coverage = if !clean {
            "unknown"
        } else if coverages.is_empty() {
            "not_dispatched"
        } else if all(ProviderUsageCoverage::Reported) {
            "reported"
        } else if all(ProviderUsageCoverage::Missing) {
            "missing"
        } else if all(ProviderUsageCoverage::NotDispatched) {
            "not_dispatched"
        } else if all(ProviderUsageCoverage::Rejected) {
            "rejected"
        } else if coverages.iter().any(Option::is_none) {
            "unknown"
        } else {
            "incomplete"
        };
        let tokens = (coverage == "reported").then(|| {
            let sum = |field: fn(&codewhale_models::Usage) -> Option<u32>| {
                let values: Vec<u64> = snapshot
                    .records
                    .iter()
                    .filter_map(|record| field(&record.usage.usage).map(u64::from))
                    .collect();
                (!values.is_empty()).then(|| values.iter().sum::<u64>())
            };
            Tokens {
                input: sum(|usage| Some(usage.input_tokens)).unwrap_or(0),
                output: sum(|usage| Some(usage.output_tokens)).unwrap_or(0),
                cache_read: sum(|usage| usage.prompt_cache_hit_tokens),
                cache_write: sum(|usage| usage.prompt_cache_write_tokens),
                reasoning: sum(|usage| usage.reasoning_tokens),
            }
        });
        Self {
            route,
            coverage,
            tokens,
            requests,
        }
    }

    fn usage_json(&self) -> Value {
        let tokens = self.tokens.as_ref();
        json!({
            "coverage": self.coverage,
            "input_tokens": tokens.map(|tokens| tokens.input),
            "output_tokens": tokens.map(|tokens| tokens.output),
            "cache_read_tokens": tokens.and_then(|tokens| tokens.cache_read),
            "cache_write_tokens": tokens.and_then(|tokens| tokens.cache_write),
            "reasoning_tokens": tokens.and_then(|tokens| tokens.reasoning),
        })
    }

    fn route_json(&self) -> Value {
        self.route.as_ref().map_or(
            Value::Null,
            |(provider, model)| json!({ "provider": provider, "model": model }),
        )
    }
}

fn authority<'a>(
    owner: &OwnerRef,
    tool: &'a ToolContext,
    cancel: &CancellationToken,
) -> Result<&'a CapturedRlmCaller, BrokerRefusal> {
    if cancel.is_cancelled() {
        return Err(refused("the llm broker call was withdrawn"));
    }
    let Some(caller) = tool.rlm_caller.as_deref() else {
        return Err(BrokerRefusal::NotAvailable(DshService::Llm));
    };
    if tool_denied(Some(&tool.disallowed_tools), RLM_TOOL_NAME)
        || tool_denied(Some(&tool.disallowed_tools), "rlm_eval")
        || tool
            .child_host
            .as_ref()
            .is_some_and(|child| !child.is_action_allowed(RLM_TOOL_NAME, "eval"))
    {
        tracing::info!(target: LOG, plugin = %owner.plugin_id, "model call denied by the caller's own tool limits");
        return Err(denied("model calls are not allowed for this caller"));
    }
    caller.validate_context(tool).map_err(|error| {
        tracing::warn!(target: LOG, plugin = %owner.plugin_id, %error, "the session route refused an llm broker call");
        refused("the session's model route is not available to this call")
    })?;
    Ok(caller)
}

fn route_model(caller: &CapturedRlmCaller) -> Result<&str, BrokerRefusal> {
    Some(caller.route_model())
        .filter(|model| !model.is_empty())
        .ok_or(BrokerRefusal::NotAvailable(DshService::Llm))
}

async fn catalog(
    caller: &CapturedRlmCaller,
    tool: &ToolContext,
    gate: &Gate,
) -> Result<Value, BrokerRefusal> {
    let model = route_model(caller)?.to_string();
    let context_window = caller.route_context_window().or(tool.route_context_window);
    gate.act("catalog", BrokerEffect::Read, None).await?;
    Ok(json!({
        "providers": [{ "id": SESSION_PROVIDER, "name": "Codewhale session route" }],
        "models": [{
            "provider": SESSION_PROVIDER,
            "id": model,
            "name": model,
            "context_window": context_window,
            "default_max_output_tokens": DEFAULT_OUTPUT_TOKENS,
            "max_output_tokens": MAX_OUTPUT_TOKENS,
        }],
        "limits": {
            "prompt_bytes": MAX_PROMPT_BYTES,
            "system_bytes": MAX_SYSTEM_BYTES,
            "default_timeout_ms": u64::try_from(DEFAULT_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            "max_timeout_ms": u64::try_from(MAX_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            "concurrent_calls": MAX_IN_FLIGHT,
        },
    }))
}

struct Checked {
    prompt: String,
    system: Option<String>,
    max_tokens: u32,
    timeout: Duration,
}

fn checked(input: CompleteInput, caller: &CapturedRlmCaller) -> Result<Checked, BrokerRefusal> {
    if input.prompt.trim().is_empty() {
        return Err(invalid("prompt is empty"));
    }
    if input.prompt.len() > MAX_PROMPT_BYTES {
        return Err(invalid("prompt is larger than the catalog's prompt_bytes"));
    }
    let system = input.system.filter(|system| !system.trim().is_empty());
    if system
        .as_ref()
        .is_some_and(|system| system.len() > MAX_SYSTEM_BYTES)
    {
        return Err(invalid("system is larger than the catalog's system_bytes"));
    }
    if input.max_output_tokens == Some(0) {
        return Err(invalid("max_output_tokens must be greater than zero"));
    }
    if input.timeout_ms == Some(0) {
        return Err(invalid("timeout_ms must be greater than zero"));
    }
    for name in [input.provider.as_ref(), input.model.as_ref()]
        .into_iter()
        .flatten()
    {
        if name.len() > MAX_NAME_BYTES {
            return Err(invalid("provider and model names are at most 256 bytes"));
        }
    }
    let model = route_model(caller)?;
    if input
        .provider
        .as_deref()
        .is_some_and(|provider| provider != SESSION_PROVIDER)
        || input.model.as_deref().is_some_and(|asked| asked != model)
    {
        return Err(refused(
            "the llm broker serves only the session's own provider and model; see catalog",
        ));
    }
    let max_timeout_ms = u64::try_from(MAX_TIMEOUT.as_millis()).unwrap_or(u64::MAX);
    Ok(Checked {
        prompt: input.prompt,
        system,
        max_tokens: input
            .max_output_tokens
            .unwrap_or(DEFAULT_OUTPUT_TOKENS)
            .min(MAX_OUTPUT_TOKENS),
        timeout: input
            .timeout_ms
            .map_or(DEFAULT_TIMEOUT, |ms| {
                Duration::from_millis(ms.min(max_timeout_ms))
            })
            .min(MAX_TIMEOUT),
    })
}

async fn complete(
    owner: &OwnerRef,
    session_id: &str,
    cancel: &CancellationToken,
    caller: &CapturedRlmCaller,
    gate: Gate,
    input: CompleteInput,
) -> Result<Value, BrokerRefusal> {
    let checked = checked(input, caller)?;
    let model = route_model(caller)?.to_string();
    let provider = caller.route_provider_id();
    let target = format!("model {model}, {} bytes", checked.prompt.len());
    gate.act("complete", BrokerEffect::Reach, Some(&target))
        .await?;

    let prompt_bytes = checked.prompt.len();
    let prompt_sha256 = codewhale_runtime::hashing::sha256_hex(checked.prompt.as_bytes());
    let system_bytes = checked.system.as_deref().map_or(0, str::len);
    let system_sha256 = checked
        .system
        .as_deref()
        .map(|system| codewhale_runtime::hashing::sha256_hex(system.as_bytes()));
    let started = Instant::now();
    let flight = Flight::admit(
        flight_key(owner, session_id),
        gate,
        RecordMeta {
            plugin_id: owner.plugin_id.clone(),
            session_id: session_id.to_string(),
            provider,
            model,
            prompt_bytes,
            prompt_sha256,
            system_bytes,
            system_sha256,
            started,
        },
    )?;

    let call_deadline = tokio::time::Instant::now() + checked.timeout;
    let deadline = caller
        .deadline()
        .map_or(call_deadline, |turn| turn.min(call_deadline));
    let accounting = RlmUsageAccumulator::new();
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        result = caller.dispatch(RlmInvocation {
            prompt: checked.prompt,
            mode: RlmMode::Completion,
            prompt_policy: RlmPromptPolicy::IsolatedPlugin,
            max_tokens: Some(checked.max_tokens),
            task_instructions: checked.system,
            deadline: Some(deadline),
            gate: None,
            events: None,
            usage: accounting.clone(),
        }) => Some(result),
    };
    let observed = Observed::of(&accounting.snapshot().await);
    let outcome = if result.is_none() || cancel.is_cancelled() {
        "cancelled"
    } else if result.as_ref().is_some_and(|result| result.error.is_some()) {
        if tokio::time::Instant::now() >= deadline {
            "timed_out"
        } else {
            "failed"
        }
    } else {
        "completed"
    };
    if let Some(error) = result.as_ref().and_then(|result| result.error.as_ref()) {
        tracing::warn!(
            target: LOG,
            plugin = %owner.plugin_id,
            session = %session_id,
            %error,
            "auxiliary model call failed"
        );
    }
    if observed.coverage != "reported" && observed.requests > 0 {
        tracing::warn!(
            target: LOG,
            plugin = %owner.plugin_id,
            session = %session_id,
            coverage = observed.coverage,
            "auxiliary model call has no complete usage report; the work is unaffected"
        );
    }
    let duration_ms = flight.settle(outcome, &observed);
    match outcome {
        "cancelled" => Err(refused("the model call was cancelled")),
        "timed_out" => Err(refused("the model call ran out of time")),
        "failed" => Err(refused("the model call did not complete")),
        _ => {
            let answer = result.expect("completed outcome has result").answer;
            Ok(json!({
                "text": answer,
                "finish": "stop",
                "route": observed.route_json(),
                "usage": observed.usage_json(),
                "receipt": {
                    "requests": observed.requests,
                    "duration_ms": duration_ms,
                },
            }))
        }
    }
}

async fn run(
    owner: &OwnerRef,
    session_id: &str,
    tool: &ToolContext,
    cancel: &CancellationToken,
    gate: Gate,
    method: LlmMethod,
) -> Result<Value, BrokerRefusal> {
    let caller = authority(owner, tool, cancel)?;
    match method {
        LlmMethod::Catalog(Empty {}) => catalog(caller, tool, &gate).await,
        LlmMethod::Complete(input) => {
            complete(owner, session_id, cancel, caller, gate, input).await
        }
    }
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<LlmMethod>(&call)?;
        let context = Arc::new(call.context);
        run(
            &call.owner,
            &call.session_id,
            context.tool(),
            context.cancel(),
            Gate::Live(Arc::clone(&context)),
            method,
        )
        .await
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use codewhale_agent_contract::role::FleetRole;
    use codewhale_agent_contract::worker_profile::WorkerRuntimeProfile;
    use codewhale_models::{ContentBlock, MessageRequest, MessageResponse, Usage};
    use codewhale_runtime::config::Config;

    use super::*;
    use crate::client::CodewhaleClient;
    use crate::core::engine::tests::rlm_host::{Replies, context_for_replies, fixture_config};
    use crate::llm_client::mock::MockLlmClient;
    use crate::tools::subagent::engine::ChildAuthority;
    use crate::tools::subagent::{SubAgentRuntime, new_shared_subagent_manager};

    const MODEL: &str = "captured-model";

    fn owner(plugin: &str) -> OwnerRef {
        OwnerRef {
            plugin_id: plugin.to_string(),
            generation: 1,
            owner_token: "token".to_string(),
        }
    }

    fn reply(text: &str, usage: Usage) -> MessageResponse {
        MessageResponse {
            id: "mock_msg".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: MODEL.to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            title: None,
            stop_details: None,
            refusal: None,
            usage,
        }
    }

    fn admitted(replies: Arc<dyn Replies>, session: &str) -> (tempfile::TempDir, ToolContext) {
        let workspace = tempfile::tempdir().unwrap();
        let context = context_for_replies(
            &fixture_config(MODEL),
            MODEL,
            &ToolContext::new(workspace.path()),
            replies,
        )
        .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
            session.into(),
            MODEL.into(),
            workspace.path().to_path_buf(),
            None,
            Arc::new(Vec::new()),
        ));
        (workspace, context)
    }

    async fn ask(
        owner: &OwnerRef,
        session: &str,
        tool: &ToolContext,
        cancel: &CancellationToken,
        method: &str,
        input: Value,
    ) -> Result<Value, BrokerRefusal> {
        let method =
            serde_json::from_value::<LlmMethod>(json!({ "method": method, "input": input }))
                .map_err(|error| invalid(&error.to_string()))?;
        run(owner, session, tool, cancel, Gate::Stub, method).await
    }

    fn code(result: &Result<Value, BrokerRefusal>) -> &'static str {
        result.as_ref().expect_err("a refusal").code()
    }

    #[tokio::test]
    async fn a_completion_runs_on_the_session_route_and_reports_observed_usage() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(reply(
            "brokered answer",
            Usage {
                input_tokens: 7,
                output_tokens: 11,
                ..Usage::default()
            },
        ));
        mock.push_message_response(reply("answer without usage", Usage::default()));
        let (_workspace, tool) = admitted(mock.clone(), "llm-usage-session");
        let owner = owner("llm-usage-plugin");
        let cancel = CancellationToken::new();

        let catalog = ask(
            &owner,
            "llm-usage-session",
            &tool,
            &cancel,
            "catalog",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(catalog["providers"][0]["id"], SESSION_PROVIDER);
        assert_eq!(catalog["models"][0]["id"], MODEL);
        assert_eq!(catalog["limits"].get("calls_per_session"), None);
        assert!(!catalog.to_string().contains("owned-fixture-key"));
        assert!(!catalog.to_string().contains("127.0.0.1"));

        let first = ask(
            &owner,
            "llm-usage-session",
            &tool,
            &cancel,
            "complete",
            json!({
                "prompt": "summarize this",
                "system": "be brief",
                "provider": SESSION_PROVIDER,
                "model": MODEL,
                "max_output_tokens": 1_000_000,
            }),
        )
        .await
        .unwrap();
        assert_eq!(first["text"], "brokered answer");
        assert_eq!(first["finish"], "stop");
        assert_eq!(first["route"]["model"], MODEL);
        assert!(
            first["route"]["provider"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(first["usage"]["coverage"], "reported");
        assert_eq!(first["usage"]["input_tokens"], 7);
        assert_eq!(first["usage"]["output_tokens"], 11);
        assert_eq!(first["receipt"].get("call"), None);
        assert_eq!(first["receipt"]["requests"], 1);
        assert!(!first.to_string().contains("owned-fixture-key"));
        assert!(!first.to_string().contains("127.0.0.1"));

        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 1, "one real Engine dispatch");
        assert_eq!(requests[0].model, MODEL);
        assert!((1..=MAX_OUTPUT_TOKENS).contains(&requests[0].max_tokens));
        assert!(requests[0].tools.as_ref().is_none_or(Vec::is_empty));
        let system = serde_json::to_string(&requests[0].system).unwrap();
        assert!(system.contains("be brief"));
        assert!(!system.contains("subject to the captured Core policy"));

        let second = ask(
            &owner,
            "llm-usage-session",
            &tool,
            &cancel,
            "complete",
            json!({ "prompt": "again" }),
        )
        .await
        .unwrap();
        assert_eq!(second["text"], "answer without usage");
        assert_eq!(second["usage"]["coverage"], "missing");
        assert_eq!(second["usage"]["input_tokens"], Value::Null);
        assert_eq!(second["usage"]["output_tokens"], Value::Null);
        assert!((1..=DEFAULT_OUTPUT_TOKENS).contains(&mock.captured_requests()[1].max_tokens));
        assert_eq!(in_flight_count(&flight_key(&owner, "llm-usage-session")), 0);
    }

    #[tokio::test]
    async fn a_missing_route_a_denied_caller_and_bad_input_are_refused_before_any_dispatch() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let (workspace, tool) = admitted(mock.clone(), "llm-refusal-session");
        let owner = owner("llm-refusal-plugin");
        let cancel = CancellationToken::new();
        let complete = |tool: ToolContext, cancel: CancellationToken, input: Value| {
            let owner = owner.clone();
            async move {
                ask(
                    &owner,
                    "llm-refusal-session",
                    &tool,
                    &cancel,
                    "complete",
                    input,
                )
                .await
            }
        };

        let bare = ToolContext::new(workspace.path());
        assert_eq!(
            code(
                &ask(
                    &owner,
                    "llm-refusal-session",
                    &bare,
                    &cancel,
                    "catalog",
                    json!({})
                )
                .await
            ),
            "not_available"
        );
        assert_eq!(
            code(&complete(bare, cancel.clone(), json!({ "prompt": "x" })).await),
            "not_available"
        );
        assert_eq!(
            code(
                &ask(
                    &owner,
                    "llm-refusal-session",
                    &tool,
                    &cancel,
                    "usage",
                    json!({})
                )
                .await
            ),
            "invalid_input"
        );

        let mut denied = tool.clone();
        denied.disallowed_tools = vec!["rlm".to_string()];
        let result = complete(denied, cancel.clone(), json!({ "prompt": "x" })).await;
        assert_eq!(code(&result), "denied");

        let mut moved = tool.clone();
        moved.state_namespace.push_str("-other");
        let result = complete(moved, cancel.clone(), json!({ "prompt": "x" })).await;
        assert_eq!(code(&result), "refused");
        assert!(!result.unwrap_err().wire().message.contains("RLM"));

        for input in [
            json!({ "prompt": "  " }),
            json!({ "prompt": "x".repeat(MAX_PROMPT_BYTES + 1) }),
            json!({ "prompt": "x", "system": "s".repeat(MAX_SYSTEM_BYTES + 1) }),
            json!({ "prompt": "x", "max_output_tokens": 0 }),
            json!({ "prompt": "x", "timeout_ms": 0 }),
            json!({ "prompt": "x", "tools": [] }),
            json!({ "prompt": "x", "api_key": "sk-own" }),
            json!({ "messages": [] }),
        ] {
            assert_eq!(
                code(&complete(tool.clone(), cancel.clone(), input).await),
                "invalid_input"
            );
        }
        for input in [
            json!({ "prompt": "x", "model": "another-model" }),
            json!({ "prompt": "x", "provider": "openai" }),
        ] {
            assert_eq!(
                code(&complete(tool.clone(), cancel.clone(), input).await),
                "refused"
            );
        }

        let withdrawn = CancellationToken::new();
        withdrawn.cancel();
        assert_eq!(
            code(&complete(tool.clone(), withdrawn, json!({ "prompt": "x" })).await),
            "refused"
        );

        tool.cancel_token.as_ref().unwrap().cancel();
        assert_eq!(
            code(&complete(tool.clone(), cancel.clone(), json!({ "prompt": "x" })).await),
            "refused"
        );

        assert_eq!(mock.call_count(), 0, "nothing reached the provider");
        assert_eq!(
            in_flight_count(&flight_key(&owner, "llm-refusal-session")),
            0
        );
    }

    #[tokio::test]
    async fn a_sub_agent_saved_without_the_model_action_gets_no_completion() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(reply("allowed child reply", Usage::default()));
        let (workspace, tool) = admitted(mock.clone(), "llm-child-session");
        let owner = owner("llm-child-plugin");
        let cancel = CancellationToken::new();
        let client = CodewhaleClient::new(
            &Config::default().with_legacy_root(Some("test-key".to_string()), None),
        )
        .expect("stub client");
        let manager = new_shared_subagent_manager(workspace.path().to_path_buf(), 4);

        let mut blocked_runtime = SubAgentRuntime::new(
            client.clone(),
            MODEL.to_string(),
            tool.clone(),
            false,
            None,
            manager.clone(),
        );
        blocked_runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        blocked_runtime.worker_profile.denied_tools =
            vec!["rlm_open".to_string(), "rlm_eval".to_string()];
        let blocked_child = ChildAuthority::capture(
            blocked_runtime,
            FleetRole::Builder,
            "agent_child".into(),
            "child".into(),
            None,
        );
        let mut blocked_tool = tool.clone();
        blocked_tool.child_host = Some(blocked_child);
        let denied = ask(
            &owner,
            "llm-child-session",
            &blocked_tool,
            &cancel,
            "complete",
            json!({ "prompt": "blocked child" }),
        )
        .await;
        assert_eq!(code(&denied), "denied");
        assert_eq!(mock.call_count(), 0);

        let mut allowed_runtime = SubAgentRuntime::new(
            client,
            MODEL.to_string(),
            tool.clone(),
            false,
            None,
            manager,
        );
        allowed_runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        let allowed_child = ChildAuthority::capture(
            allowed_runtime,
            FleetRole::Builder,
            "agent_child_ok".into(),
            "child".into(),
            None,
        );
        let mut allowed_tool = tool;
        allowed_tool.child_host = Some(allowed_child);
        let served = ask(
            &owner,
            "llm-child-session",
            &allowed_tool,
            &cancel,
            "complete",
            json!({ "prompt": "allowed child" }),
        )
        .await
        .unwrap();
        assert_eq!(served["text"], "allowed child reply");
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn a_session_deny_list_naming_the_model_action_refuses_a_completion() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(reply("unrelated deny passes", Usage::default()));
        let (_workspace, tool) = admitted(mock.clone(), "llm-deny-action-session");
        let owner = owner("llm-deny-action-plugin");
        let cancel = CancellationToken::new();

        for rule in ["rlm_eval", "rlm", "rlm*", "Bash"] {
            let mut denied = tool.clone();
            denied.disallowed_tools = vec![rule.to_string()];
            let result = ask(
                &owner,
                "llm-deny-action-session",
                &denied,
                &cancel,
                "complete",
                json!({ "prompt": "must refuse" }),
            )
            .await;
            assert_eq!(code(&result), "denied", "deny rule {rule}");
        }
        assert_eq!(mock.call_count(), 0);

        let mut unrelated = tool;
        unrelated.disallowed_tools = vec!["read_file".to_string()];
        let served = ask(
            &owner,
            "llm-deny-action-session",
            &unrelated,
            &cancel,
            "complete",
            json!({ "prompt": "allowed" }),
        )
        .await
        .unwrap();
        assert_eq!(served["text"], "unrelated deny passes");
        assert_eq!(mock.call_count(), 1);
    }

    struct Pending(tokio::sync::Notify);

    impl Replies for Pending {
        fn effective_route_envelope(
            &self,
            model: &str,
            at: chrono::DateTime<chrono::Utc>,
        ) -> crate::cost_status::EffectiveRouteEnvelope {
            crate::cost_status::EffectiveRouteEnvelope::capture_observed(
                codewhale_config::ProviderKind::Custom,
                "rlm-fixture",
                model,
                Some("http://127.0.0.1:9/v1"),
                at,
            )
        }

        fn effective_max_output_tokens(&self, _: &str) -> u32 {
            8192
        }

        fn create_message_boxed(
            &self,
            _: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
            Box::pin(async move {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }

    #[tokio::test]
    async fn cancellation_a_deadline_and_the_concurrency_cap_end_calls_with_a_terminal_receipt() {
        let pending = Arc::new(Pending(tokio::sync::Notify::new()));
        let (_workspace, tool) = admitted(pending.clone(), "llm-cancel-session");
        let owner = owner("llm-cancel-plugin");
        let key = flight_key(&owner, "llm-cancel-session");

        let never = CancellationToken::new();
        let timed_out = ask(
            &owner,
            "llm-cancel-session",
            &tool,
            &never,
            "complete",
            json!({ "prompt": "slow", "timeout_ms": 150 }),
        )
        .await;
        assert_eq!(code(&timed_out), "refused");
        assert_eq!(
            timed_out.unwrap_err().wire().message,
            "the model call ran out of time"
        );
        assert_eq!(in_flight_count(&key), 0);

        let cancel = CancellationToken::new();
        let (cancelled, ()) = tokio::join!(
            ask(
                &owner,
                "llm-cancel-session",
                &tool,
                &cancel,
                "complete",
                json!({ "prompt": "slow" }),
            ),
            async {
                pending.0.notified().await;
                cancel.cancel();
            }
        );
        assert_eq!(code(&cancelled), "refused");
        assert_eq!(
            cancelled.unwrap_err().wire().message,
            "the model call was cancelled"
        );
        assert_eq!(in_flight_count(&key), 0);

        let hold = CancellationToken::new();
        let running: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|_| {
                let (owner, tool, hold) = (owner.clone(), tool.clone(), hold.clone());
                tokio::spawn(async move {
                    ask(
                        &owner,
                        "llm-cancel-session",
                        &tool,
                        &hold,
                        "complete",
                        json!({ "prompt": "slow" }),
                    )
                    .await
                })
            })
            .collect();
        for _ in 0..200 {
            if in_flight_count(&key) == MAX_IN_FLIGHT {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(in_flight_count(&key), MAX_IN_FLIGHT);
        let over = ask(
            &owner,
            "llm-cancel-session",
            &tool,
            &never,
            "complete",
            json!({ "prompt": "one too many" }),
        )
        .await;
        assert_eq!(code(&over), "refused");
        running[0].abort();
        hold.cancel();
        for task in running {
            let _ = task.await;
        }
        assert_eq!(
            in_flight_count(&key),
            0,
            "dropped and cancelled calls leave no in-flight row"
        );
    }

    #[test]
    fn in_flight_rows_exist_only_while_a_call_runs_and_a_full_table_refuses() {
        let mut table = HashMap::new();
        let owner_gen1 = owner("llm-bound-plugin");
        let mut owner_gen2 = owner("llm-bound-plugin");
        owner_gen2.generation = 99;
        let k1 = flight_key(&owner_gen1, "s0");
        let k2 = flight_key(&owner_gen2, "s0");
        assert_eq!(k1, k2, "generation changes share one plugin/session key");

        reserve(&mut table, &k1).unwrap();
        reserve(&mut table, &k2).unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table[&k1], MAX_IN_FLIGHT);
        assert_eq!(reserve(&mut table, &k2).unwrap_err().code(), "refused");

        release(&mut table, &k1);
        assert_eq!(table[&k1], 1);
        release(&mut table, &k2);
        assert!(table.is_empty(), "finished calls leave no row behind");

        for index in 0..MAX_IN_FLIGHT_ROWS {
            let key = (format!("plugin-{index}"), "s".to_string());
            reserve(&mut table, &key).unwrap();
        }
        assert_eq!(table.len(), MAX_IN_FLIGHT_ROWS);
        let extra = ("plugin-overflow".to_string(), "s".to_string());
        assert_eq!(reserve(&mut table, &extra).unwrap_err().code(), "refused");
        assert_eq!(table.len(), MAX_IN_FLIGHT_ROWS);
        assert!(!table.contains_key(&extra));
        let existing = ("plugin-0".to_string(), "s".to_string());
        assert!(table.contains_key(&existing), "live row was not evicted");
    }
}
