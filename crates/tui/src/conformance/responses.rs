//! Responses family: profile selection, request body, and stream termination.
//!
//! Route cases call `RouteResolver`. Request cases call
//! `prepare_outbound_request`. Stream cases replay `.sse` bytes through the
//! shared loopback into `create_message_stream`. The runner does not build a
//! second Responses parser.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};

use codewhale_config::ProviderKind;
use codewhale_config::provider::WireFormat;
use codewhale_config::route::{LogicalModelRef, RouteRequest, RouteResolver};
use codewhale_models::{ContentBlock, Message, MessageRequest, Role, SystemPrompt, Tool};
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};

use super::golden::{self, Failures};
use super::sse::{Framing, LoopbackRecording};
use super::stream_json;
use crate::test_support::{EnvVarGuard, lock_test_env};
use codewhale_engine::client::CodewhaleClient;
use codewhale_engine::llm_client::LlmClient;

const FAMILY: &str = "responses";
const STREAM_DEADLINE: Duration = Duration::from_secs(20);

#[test]
fn responses_routes_match_goldens() {
    let dir = golden::family_dir(FAMILY);
    let names = golden::case_names(FAMILY)
        .into_iter()
        .filter(|name| golden::read_case(FAMILY, name)["kind"] == "route")
        .collect::<Vec<_>>();
    assert!(!names.is_empty(), "responses family has no route cases");
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        let candidate = resolve_route(&case);
        let profile_id = candidate
            .responses_profile_id()
            .map(|id| id.as_str().to_string());
        if matches!(profile_id.as_deref(), Some("codex") | Some("concentrate")) {
            assert_ne!(
                candidate.endpoint().endpoint_key,
                "responses",
                "{name} must not depend on endpoint_key == responses"
            );
        }
        let actual = json!({
            "protocol": protocol_name(candidate.protocol()),
            "profile_id": profile_id,
            "endpoint_path_policy": profile_id.as_deref().map(path_policy_name),
        });
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.golden.json")),
                &golden::pretty(&actual),
            ),
        );
    }
    failures.finish(FAMILY, names.len());
}

#[test]
fn responses_requests_match_goldens() {
    let _lock = lock_test_env();
    let _token = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "conformance-token");
    let _legacy = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let dir = golden::family_dir(FAMILY);
    let names = golden::case_names(FAMILY)
        .into_iter()
        .filter(|name| golden::read_case(FAMILY, name)["kind"] == "request")
        .collect::<Vec<_>>();
    assert!(!names.is_empty(), "responses family has no request cases");
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        let actual = match prepare_case(&case, None) {
            Ok(actual) => actual,
            Err(error) => {
                failures.push(name, error);
                continue;
            }
        };
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.golden.json")),
                &golden::pretty(&actual),
            ),
        );
    }
    failures.finish(FAMILY, names.len());
}

#[tokio::test]
async fn responses_streams_match_goldens() {
    let dir = golden::family_dir(FAMILY);
    let names = golden::case_names(FAMILY)
        .into_iter()
        .filter(|name| golden::read_case(FAMILY, name)["kind"] == "stream")
        .collect::<Vec<_>>();
    assert!(!names.is_empty(), "responses family has no stream cases");
    let mut failures = Failures::default();
    for name in &names {
        let case = golden::read_case(FAMILY, name);
        let recording = std::fs::read(dir.join(format!("{name}.sse")))
            .unwrap_or_else(|error| panic!("read {name}.sse: {error}"));
        let mut lines = match replay_stream(&case, recording).await {
            Ok(lines) => lines,
            Err(error) => {
                failures.push(name, error);
                continue;
            }
        };
        let mut masker = golden::Masker::new(&[]);
        for line in &mut lines {
            masker.value(line);
        }
        for line in lines.iter().skip(1) {
            let kind = line["type"].as_str().unwrap_or_default();
            if matches!(kind, "stream_failure" | "open_failure") {
                continue;
            }
            if let Err(message) = stream_json::round_trips(line) {
                failures.push(
                    name,
                    format!("normalized event is not StreamEvent: {message}"),
                );
            }
        }
        failures.record(
            name,
            golden::check_golden(
                &dir.join(format!("{name}.golden.jsonl")),
                &golden::jsonl(&lines),
            ),
        );
    }
    failures.finish(FAMILY, names.len());
}

fn resolve_route(case: &Value) -> codewhale_config::route::ReadyRouteCandidate {
    let provider = provider_kind(case);
    RouteResolver::new()
        .resolve(&RouteRequest {
            explicit_provider: Some(provider),
            model_selector: Some(LogicalModelRef::from(case["model"].as_str().unwrap())),
            base_url_override: case["base_url"].as_str().map(str::to_string),
            wire_override: (case["wire"].as_str() == Some("responses"))
                .then_some(WireFormat::Responses),
            ..RouteRequest::default()
        })
        .unwrap_or_else(|error| panic!("resolve {}: {error}", case["model"]))
}

fn prepare_case(case: &Value, transport_base: Option<&str>) -> Result<Value, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = config_for(case, transport_base);
    let _codex_grant =
        if case["provider"].as_str() == Some("openai-codex") && transport_base.is_none() {
            let home = tempfile::tempdir().map_err(|error| format!("codex home: {error}"))?;
            let root = home
                .path()
                .canonicalize()
                .map_err(|error| format!("codex home: {error}"))?;
            let guard = EnvVarGuard::set("CODEWHALE_HOME", &root);
            codewhale_runtime::oauth::install_test_chatgpt_registration(&mut config)
                .map_err(|error| format!("codex grant: {error:#}"))?;
            Some((home, guard))
        } else {
            None
        };
    let provider = provider_kind(case);
    let resolved = codewhale_engine::route_runtime::resolve_runtime_route(
        &config,
        provider,
        Some(case["model"].as_str().unwrap()),
    )
    .map_err(|error| format!("resolve: {error}"))?;
    let profile_id = resolved
        .candidate
        .responses_profile_id()
        .map(|id| id.as_str().to_string())
        .ok_or_else(|| "request case resolved without a responses profile".to_string())?;
    let client = CodewhaleClient::from_candidate(&resolved.config, &resolved.candidate)
        .map_err(|error| format!("client: {error:#}"))?;
    let prepared = client
        .prepare_outbound_request(message_for_case(case), true)
        .map_err(|error| format!("prepare: {error:#}"))?;
    if prepared.responses_profile_id.map(|id| id.as_str()) != Some(profile_id.as_str()) {
        return Err(format!(
            "prepared profile {:?} does not match route profile {profile_id}",
            prepared.responses_profile_id.map(|id| id.as_str())
        ));
    }
    Ok(json!({
        "profile_id": profile_id,
        "method": "POST",
        "path": url_path(&prepared.endpoint.url),
        "wire_model": prepared.wire_model,
        "body": prepared.body,
    }))
}

async fn replay_stream(case: &Value, recording: Vec<u8>) -> Result<Vec<Value>, String> {
    let server = LoopbackRecording::start(recording, Framing::closed()).await;
    let _lock = lock_test_env();
    let _token = EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "conformance-token");
    let _legacy = EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
    let _ = rustls::crypto::ring::default_provider().install_default();
    let base = format!("{}/v1", server.base);
    let config = config_for(case, Some(&base));
    let resolved = codewhale_engine::route_runtime::resolve_runtime_route(
        &config,
        provider_kind(case),
        Some(case["model"].as_str().unwrap()),
    )
    .map_err(|error| format!("resolve: {error}"))?;
    let profile_id = resolved
        .candidate
        .responses_profile_id()
        .map(|id| id.as_str().to_string())
        .ok_or_else(|| "stream case resolved without a responses profile".to_string())?;
    let client = CodewhaleClient::from_candidate(&resolved.config, &resolved.candidate)
        .map_err(|error| format!("client: {error:#}"))?;
    let events = golden::complete_within("Responses SSE replay", STREAM_DEADLINE, async {
        let mut events = Vec::new();
        match client.create_message_stream(message_for_case(case)).await {
            Err(error) => events.push(json!({
                "type": "open_failure",
                "detail": format!("{error:#}"),
            })),
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(event) => events.push(stream_json::to_json(&event)),
                        Err(error) => events.push(json!({
                            "type": "stream_failure",
                            "detail": format!("{error:#}"),
                        })),
                    }
                }
            }
        }
        events
    })
    .await?;
    if events.is_empty() {
        return Err("Responses replay produced no event or provider error".to_string());
    }
    let requests = server.requests.lock().expect("request log").clone();
    if requests.is_empty() {
        return Err("Responses recording was not exercised: no loopback request".to_string());
    }
    let mut lines = vec![json!({
        "profile_id": profile_id,
        "request": requests
            .iter()
            .map(|request| json!({ "method": request.method, "path": request.path }))
            .collect::<Vec<_>>(),
    })];
    lines.extend(events);
    Ok(lines)
}

fn config_for(case: &Value, transport_base: Option<&str>) -> Config {
    let provider = case["provider"].as_str().unwrap();
    let model = case["model"].as_str().unwrap().to_string();
    let base_url = transport_base
        .map(str::to_string)
        .or_else(|| case["base_url"].as_str().map(str::to_string));
    let mut entry = ProviderConfig {
        model: Some(model),
        base_url,
        wire: case["wire"].as_str().map(str::to_string),
        ..ProviderConfig::default()
    };
    if provider != "openai-codex" {
        entry.api_key = Some("conformance-key".to_string());
    }
    if provider == "custom" {
        entry.kind = Some("openai-compatible".to_string());
    }
    let mut providers = ProvidersConfig::default();
    match provider {
        "deepseek" => providers.deepseek = entry,
        "openai-codex" => providers.openai_codex = entry,
        "concentrate" => providers.concentrate = entry,
        "custom" => {
            providers.custom.insert("gateway".to_string(), entry);
        }
        other => panic!("unknown conformance provider {other}"),
    }
    let mut config = Config::default();
    config.provider = Some(if provider == "custom" {
        "gateway".to_string()
    } else {
        provider.to_string()
    });
    config.providers = Some(providers);
    config
}

fn message_for_case(case: &Value) -> MessageRequest {
    let mut tools = None;
    if case["tool"].as_bool() == Some(true) {
        tools = Some(vec![Tool {
            tool_type: None,
            name: "read_file".to_string(),
            description: "Read a file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }]);
    }
    MessageRequest {
        model: case["model"].as_str().unwrap().to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: case["user"]
                    .as_str()
                    .unwrap_or("Inspect the project.")
                    .to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: case["max_tokens"].as_u64().unwrap_or(64) as u32,
        system: case["system"]
            .as_str()
            .map(|text| SystemPrompt::Text(text.to_string())),
        tools,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: case["reasoning_effort"].as_str().map(str::to_string),
        stream: Some(true),
        temperature: case["temperature"].as_f64().map(|value| value as f32),
        top_p: case["top_p"].as_f64().map(|value| value as f32),
    }
}

fn provider_kind(case: &Value) -> ProviderKind {
    let name = case["provider"].as_str().unwrap();
    ProviderKind::parse(name).unwrap_or_else(|| panic!("unknown provider {name}"))
}

fn protocol_name(protocol: WireFormat) -> &'static str {
    match protocol {
        WireFormat::Responses => "responses",
        WireFormat::ChatCompletions => "chat_completions",
        WireFormat::AnthropicMessages => "anthropic_messages",
    }
}

fn path_policy_name(profile_id: &str) -> &'static str {
    match profile_id {
        "deepseek_stateless_plain_reasoning" => "unversioned_responses",
        _ => "versioned_responses",
    }
}

fn url_path(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    rest.find('/')
        .map(|index| rest[index..].to_string())
        .unwrap_or_else(|| "/".to_string())
}
