use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt;

use codewhale_models::Message;
use codewhale_models::Role;
use codewhale_models::SystemPrompt;
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig, RetryConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone)]
struct RetryThenSuccess {
    attempts: Arc<AtomicUsize>,
    retry_status: u16,
    retry_body: &'static str,
}

impl Respond for RetryThenSuccess {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut response =
                ResponseTemplate::new(self.retry_status).set_body_string(self.retry_body);
            if self.retry_status == 429 {
                response = response.insert_header("Retry-After", "0");
            }
            return response;
        }

        ResponseTemplate::new(200)
            .insert_header("Content-Type", "text/event-stream")
            .set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")
    }
}

#[derive(Clone)]
struct AlwaysError {
    attempts: Arc<AtomicUsize>,
    status: u16,
    body: &'static str,
}

impl Respond for AlwaysError {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(self.status).set_body_string(self.body)
    }
}

fn minimal_responses_request() -> MessageRequest {
    MessageRequest {
        model: "gpt-5.5".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    }
}

fn test_codex_config(server: &MockServer) -> Config {
    {
        let __cw_config_init_0_0 = Some("openai-codex".to_string());
        let __cw_config_init_0_1 = Some(RetryConfig {
            enabled: Some(true),
            max_retries: Some(1),
            initial_delay: Some(0.0),
            max_delay: Some(0.0),
            exponential_base: Some(1.0),
            jitter: None,
            jitter_factor: None,
            respect_retry_after: None,
        });
        let __cw_config_init_0_2 = Some(ProvidersConfig {
            openai_codex: ProviderConfig {
                base_url: Some(format!("{}/v1", server.uri())),
                api_key: Some("test-token".to_string()),
                ..ProviderConfig::default()
            },
            ..ProvidersConfig::default()
        });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.retry = __cw_config_init_0_1;
        __cw_config_init_0_value.providers = __cw_config_init_0_2;
        __cw_config_init_0_value
    }
}

#[tokio::test]
async fn responses_stream_retries_rate_limited_request() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(RetryThenSuccess {
            attempts: Arc::clone(&attempts),
            retry_status: 429,
            retry_body: "rate limited",
        })
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut request = minimal_responses_request();
    request.max_tokens = 384_000;
    let prepared = client
        .prepare_outbound_request(request, true)
        .expect("responses request prepares");
    assert_eq!(
        prepared.endpoint.url,
        format!("{}/v1/responses", server.uri())
    );
    // The official ChatGPT plan preview does not support output-cap fields;
    // omit them while retaining the allowance in the resolved envelope.
    assert!(prepared.body.get("max_output_tokens").is_none());
    let mut stream = client.handle_responses_stream(&prepared).await.unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("Responses retry stream should finish after response.completed");

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let requests = server
        .received_requests()
        .await
        .expect("recorded retry requests");
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body: Value = serde_json::from_slice(&request.body).expect("Responses JSON");
        assert!(
            body.get("max_output_tokens").is_none(),
            "ChatGPT plan body must not name the unsupported output cap: {body}"
        );
    }
}

#[tokio::test]
async fn responses_stream_retries_transient_server_error() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(RetryThenSuccess {
            attempts: Arc::clone(&attempts),
            retry_status: 503,
            retry_body: "temporarily unavailable",
        })
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("Responses retry stream should finish after response.completed");

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn responses_stream_retries_upstream_499_before_streaming() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(RetryThenSuccess {
            attempts: Arc::clone(&attempts),
            retry_status: 499,
            retry_body: "upstream request cancelled",
        })
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("Responses retry stream should finish after response.completed");

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

async fn collect_responses_stream(sse_body: &str) -> Vec<Result<StreamEvent>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .expect("Responses stream opens");
    tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect())
        .await
        .expect("stream ends")
}

#[tokio::test]
async fn responses_stream_eof_without_a_terminal_event_is_an_error() {
    let events = collect_responses_stream(concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
    ))
    .await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::MessageStop))),
        "a truncated stream must not report MessageStop: {events:?}"
    );
    assert!(
        events
            .last()
            .is_some_and(|event| event.as_ref().is_err_and(|error| error
                .to_string()
                .contains("closed before a successful completion event"))),
        "{events:?}"
    );
}

#[tokio::test]
async fn chatgpt_stream_requires_valid_successful_completion() {
    for body in [
        "data: [DONE]\n\n",
        "data: {\"type\":\"response.completed\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"failed\"}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
        concat!(
            "data: {invalid-json}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        ),
    ] {
        let events = collect_responses_stream(body).await;
        assert!(
            events.last().is_some_and(Result::is_err),
            "{body}: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::MessageStop))),
            "an invalid or incomplete response must not settle the turn: {body}: {events:?}"
        );
    }
}

#[tokio::test]
async fn chatgpt_stream_validates_returned_function_namespace() {
    let events = collect_responses_stream(concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"namespace\":\"codewhale\",\"call_id\":\"call_1\",\"id\":\"fc_1\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{}\"}\n\n",
        "data: {\"type\":\"response.output_item.done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    )).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { id, name, .. }, ..
            }) if id == "call_1|fc_1" && name == "read"
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event, Ok(StreamEvent::MessageDelta { delta, .. })
                if delta.stop_reason.as_deref() == Some("tool_use")
        )),
        "{events:?}"
    );
    assert!(matches!(events.last(), Some(Ok(StreamEvent::MessageStop))));

    for body in [
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"namespace\":\"untrusted\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\"}}\n\n",
    ] {
        let events = collect_responses_stream(body).await;
        assert!(events.last().is_some_and(Result::is_err), "{events:?}");
        assert!(
            !events.iter().any(|event| matches!(
                event,
                Ok(StreamEvent::ContentBlockStart {
                    content_block: ContentBlockStart::ToolUse { .. },
                    ..
                })
            )),
            "unrecognized namespaces must not invoke local tools: {events:?}"
        );
    }
}

#[tokio::test]
async fn chatgpt_plan_usage_errors_explain_where_to_check_allowance() {
    for body in [
        "data: {\"type\":\"error\",\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"opaque upstream message\"}\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_unavailable\",\"message\":\"opaque upstream message\"}}}\n\n",
    ] {
        let events = collect_responses_stream(body).await;
        let error = events
            .last()
            .unwrap()
            .as_ref()
            .expect_err("plan usage error");
        let typed = error
            .downcast_ref::<crate::llm_client::LlmError>()
            .expect("typed allowance error");
        assert!(matches!(
            typed,
            crate::llm_client::LlmError::QuotaExhausted(_)
        ));
        assert!(!typed.is_retryable());
        assert!(error.to_string().contains("ChatGPT plan usage"), "{error}");
        assert!(
            error.to_string().contains("ChatGPT Settings > Usage"),
            "{error}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::MessageStop))),
            "{events:?}"
        );
    }
}

#[tokio::test]
async fn chatgpt_terminal_failures_preserve_usage_without_settling() {
    for response in [
        json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}),
        json!({"status":"failed","error":{"code":"subscription_sharing_usage_unavailable"}}),
    ] {
        let event_type = if response["status"] == "incomplete" {
            "response.incomplete"
        } else {
            "response.failed"
        };
        let mut response = response;
        response["usage"] = json!({"input_tokens":13,"output_tokens":5});
        let events = collect_responses_stream(&format!(
            "data: {}\n\n",
            json!({"type":event_type,"response":response})
        ))
        .await;
        assert!(events.iter().any(|event| matches!(event, Ok(StreamEvent::MessageDelta { usage: Some(usage), .. }) if usage.input_tokens == 13 && usage.output_tokens == 5)));
        let error = events.last().unwrap().as_ref().unwrap_err();
        let typed = error.downcast_ref::<crate::llm_client::LlmError>().unwrap();
        assert!(!typed.is_retryable());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::MessageStop)))
        );
    }
}

#[tokio::test]
async fn chatgpt_http_usage_limit_is_not_retried() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(AlwaysError {
            attempts: Arc::clone(&attempts),
            status: 429,
            body: r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"opaque"}}"#,
        }).mount(&server).await;
    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let request = client
        .prepare_outbound_request(minimal_responses_request(), true)
        .unwrap();
    let error = match client.handle_responses_stream(&request).await {
        Ok(_) => panic!("allowance error must fail"),
        Err(error) => error,
    };
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    let typed = error.downcast_ref::<crate::llm_client::LlmError>().unwrap();
    assert!(matches!(
        typed,
        crate::llm_client::LlmError::QuotaExhausted(_)
    ));
    assert!(!typed.is_retryable());
}

#[tokio::test]
async fn responses_stream_joins_multiline_data_fields_into_one_event() {
    let events = collect_responses_stream(concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\n",
        "data: \"delta\":\"joined\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    ))
    .await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Ok(StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text },
                ..
            }) if text == "joined"
        )),
        "the split event was lost: {events:?}"
    );
    assert!(matches!(events.last(), Some(Ok(StreamEvent::MessageStop))));
}

#[tokio::test]
async fn responses_stream_finishes_on_semantic_terminal_event_without_done_marker() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .expect("semantic Responses stream opens");

    let mut saw_stop = false;
    let mut usage = None;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::MessageStop => saw_stop = true,
                StreamEvent::MessageDelta { usage: value, .. } => usage = value,
                _ => {}
            }
        }
    })
    .await
    .expect("terminal event ends the stream without [DONE]");
    assert!(saw_stop);
    let usage = usage.expect("completed response carries usage");
    assert_eq!(usage.input_tokens, 3);
    assert_eq!(usage.output_tokens, 2);
}

#[tokio::test]
async fn responses_stream_surfaces_notice_for_web_search_call_items() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"call_id\":\"call_1\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .expect("semantic Responses stream opens");

    let mut saw_notice = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            if let Ok(StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::Text { text },
                ..
            }) = event
                && text.contains("not replayed")
            {
                saw_notice = true;
            }
        }
    })
    .await
    .expect("stream terminates");
    assert!(saw_notice, "web_search_call must surface a visible notice");
}

#[tokio::test]
async fn responses_stream_fails_fast_on_non_retryable_provider_error() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(AlwaysError {
            attempts: Arc::clone(&attempts),
            status: 403,
            body: "<html><title>Access Denied</title><body>Security alert. Contact support. Ray ID 1234abcd.</body></html>",
        })
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();

    let err = match client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
    {
        Ok(_) => panic!("non-retryable Responses errors should fail fast"),
        Err(err) => err,
    };

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    let message = format!("{err:#}");
    assert!(
        message.contains("Responses API request failed"),
        "{message}"
    );
    assert!(message.contains("OpenAI Codex"), "{message}");
    assert!(message.contains("Access Denied"), "{message}");
    assert!(
        message.contains("blocked before it reached the model"),
        "{message}"
    );
    // #3884: the structured LlmError must stay downcastable through the
    // context layers so sub-agent failure records can classify it.
    assert!(
        err.downcast_ref::<crate::llm_client::LlmError>().is_some(),
        "LlmError should survive the anyhow chain"
    );
}

#[test]
fn responses_body_serializes_the_child_catalog_without_duplication() {
    // Mirror of the Anthropic contract: the real child catalog fixture
    // maps 1:1 into Responses function tools with one canonical `read` entry.
    // `load_skill` is eager in DEFAULT_ACTIVE_NATIVE_TOOLS and children resolve
    // the same catalog authority, so it maps through exactly once too.
    let tools = crate::tools::subagent::kimi_general_child_request_tools_fixture();
    let mut request = minimal_responses_request();
    request.tools = Some(tools);
    let body = build_responses_body(&request);
    assert_eq!(body["parallel_tool_calls"], false);
    let generic = build_responses_body_for_provider(&request, ProviderKind::Openai, None);
    assert_eq!(generic["parallel_tool_calls"], true);
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["type"], "namespace");
    assert_eq!(body["tools"][0]["name"], "codewhale");
    let serialized = body["tools"][0]["tools"]
        .as_array()
        .expect("functions serialize inside the Codewhale namespace");
    let reads: Vec<_> = serialized
        .iter()
        .filter(|tool| tool["name"] == "read")
        .collect();
    assert_eq!(
        reads.len(),
        1,
        "exactly one canonical read definition reaches the Responses wire"
    );
    assert!(
        reads[0]["parameters"]["properties"].is_object(),
        "read keeps a valid parameters schema: {}",
        reads[0]
    );
    assert_eq!(
        serialized
            .iter()
            .filter(|tool| tool["name"] == "load_skill")
            .count(),
        1,
        "exactly one canonical load_skill definition reaches the Responses wire"
    );
}

#[tokio::test]
async fn responses_stream_open_preserves_wire_headers_through_shared_seam() {
    use wiremock::matchers::header;

    let server = MockServer::start().await;
    // Public API requests retain bearer/SSE headers and Codewhale identity
    // through the shared stream-entry transport; no backend headers survive.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("Accept", "text/event-stream"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .expect("stream opens with preserved headers");

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("stream should finish after response.completed");

    let requests = server
        .received_requests()
        .await
        .expect("recorded public API request");
    assert_eq!(requests.len(), 1);
    let user_agent = requests[0]
        .headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(user_agent.contains("codewhale/"), "{user_agent}");
    assert!(!user_agent.contains("codex_cli_rs"), "{user_agent}");
    for header in ["openai-beta", "originator", "chatgpt-account-id"] {
        assert!(
            requests[0].headers.get(header).is_none(),
            "{header} must not reach the public API"
        );
    }
}

#[tokio::test]
async fn responses_stream_inserts_boundary_between_reasoning_summary_parts() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"item_id\":\"rs_1\",\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"partA\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"item_id\":\"rs_1\",\"summary_index\":1,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"partB\"}\n\n",
        "data: {\"type\":\"response.output_item.done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let mut stream = client
        .handle_responses_stream(
            &client
                .prepare_outbound_request(minimal_responses_request(), true)
                .expect("responses request prepares"),
        )
        .await
        .unwrap();

    let mut thinking = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            if let StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking: chunk },
                ..
            } = event.unwrap()
            {
                thinking.push_str(&chunk);
            }
        }
    })
    .await
    .expect("Responses reasoning stream should finish after response.completed");

    // The second summary part must be separated from the first by a
    // paragraph break, and no separator may precede the first part.
    assert_eq!(thinking, "partA\n\npartB");
}

#[test]
fn codex_reasoning_effort_uses_responses_labels() {
    assert_eq!(codex_responses_reasoning_effort("max"), Some("max"));
    assert_eq!(codex_responses_reasoning_effort("maximum"), Some("max"));
    assert_eq!(codex_responses_reasoning_effort("xhigh"), Some("xhigh"));
    assert_eq!(codex_responses_reasoning_effort("ultra"), Some("ultra"));
    assert_eq!(codex_responses_reasoning_effort("ultracode"), Some("ultra"));
    assert_eq!(codex_responses_reasoning_effort("high"), Some("high"));
    assert_eq!(codex_responses_reasoning_effort("medium"), Some("medium"));
    assert_eq!(codex_responses_reasoning_effort("minimal"), Some("low"));
    assert_eq!(codex_responses_reasoning_effort("auto"), Some("medium"));
    assert_eq!(codex_responses_reasoning_effort("off"), Some("low"));
}

#[tokio::test]
async fn codex_selected_effort_reaches_preview_wire_and_restored_receipt_unchanged() {
    use crate::reasoning_preference::{EffectiveReasoningEffort, ReasoningEffort};
    use crate::work_graph::WorkActivityEvent;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"),
        )
        .expect(6)
        .mount(&server)
        .await;
    let client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
    let receipts = tempfile::tempdir().unwrap();
    for effort in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        let selected = ReasoningEffort::parse_strict(effort).unwrap();
        let activity = WorkActivityEvent::ReasoningEffortChanged {
            requested: selected.into(),
            effective: selected.into(),
            provider_kind: Some(ProviderKind::OpenaiCodex),
            provider: "openai-codex".to_string(),
            endpoint_identity: Some(
                codewhale_config::descriptors::defaults::DEFAULT_OPENAI_CODEX_BASE_URL.to_string(),
            ),
            model: Some("gpt-6-astra".to_string()),
            ts: 1,
            operation: None,
        };
        let persisted = serde_json::to_value(activity).unwrap();
        assert_eq!(persisted["requested"], effort);
        assert_eq!(persisted["effective"], effort);
        let receipt_path = receipts.path().join(format!("{effort}.json"));
        std::fs::write(&receipt_path, serde_json::to_vec(&persisted).unwrap()).unwrap();
        let WorkActivityEvent::ReasoningEffortChanged { effective, .. } =
            serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
        let restored = EffectiveReasoningEffort::from(effective)
            .request_tier_for_replay()
            .unwrap();
        assert_eq!(restored, selected);
        let mut request = minimal_responses_request();
        request.model = "gpt-6-astra".to_string();
        request.reasoning_effort = restored
            .api_value_for_provider(ProviderKind::OpenaiCodex)
            .map(str::to_string);
        let prepared = client.prepare_outbound_request(request, true).unwrap();
        assert_eq!(
            prepared.reasoning.wire_effort(),
            Some(("reasoning.effort", effort))
        );
        assert_eq!(prepared.body["reasoning"]["effort"], effort);
        let mut stream = client.handle_responses_stream(&prepared).await.unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 6);
    for (request, effort) in requests
        .iter()
        .zip(["low", "medium", "high", "xhigh", "max", "ultra"])
    {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "gpt-6-astra");
        assert_eq!(body["reasoning"]["effort"], effort);
    }
}

#[test]
fn codex_tiers_do_not_change_other_responses_provider_dialects() {
    let mut request = minimal_responses_request();
    for effort in ["max", "ultra"] {
        request.reasoning_effort = Some(effort.to_string());
        assert_eq!(
            build_responses_body_for_provider(&request, ProviderKind::Concentrate, None)["reasoning"]
                ["effort"],
            "xhigh"
        );
        assert_eq!(
            build_responses_body_for_provider(&request, ProviderKind::Deepseek, None)["reasoning"]
                ["effort"],
            "max"
        );
    }
}

/// Concentrate's parameter reference documents `model`, `input`, `stream`,
/// `max_output_tokens`, `tools`/`tool_choice`/`parallel_tool_calls`, and
/// `reasoning.effort`; `store`, `include`, `instructions`, and
/// `reasoning.summary` are absent. The body sends only documented fields and
/// carries the system prompt as a leading `system` input item.
/// https://concentrate.ai/docs/api-reference/endpoint/request-parameters
#[test]
fn concentrate_responses_body_sends_only_documented_fields() {
    let mut request = minimal_responses_request();
    request.model = "openai/gpt-5.6-sol".to_string();
    request.system = Some(SystemPrompt::Text(
        "You are the Codewhale test system prompt.".to_string(),
    ));
    request.reasoning_effort = Some("high".to_string());
    request.tools = Some(vec![Tool {
        tool_type: None,
        name: "read".to_string(),
        description: "Read a file".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    }]);

    let body = build_responses_body_for_provider(&request, ProviderKind::Concentrate, None);
    let documented = [
        "model",
        "input",
        "max_output_tokens",
        "temperature",
        "top_p",
        "stream",
        "text",
        "reasoning",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "routing",
        "cache_control",
        "prompt_cache_options",
    ];
    for key in body.as_object().expect("object body").keys() {
        assert!(
            documented.contains(&key.as_str()),
            "undocumented top-level field `{key}` reached the Concentrate wire: {body}"
        );
    }
    assert_eq!(
        body["model"], "openai/gpt-5.6-sol",
        "provider/model ids pass through verbatim"
    );
    assert_eq!(body["stream"], true);
    assert!(body.get("store").is_none(), "{body}");
    assert!(body.get("include").is_none(), "{body}");
    assert!(body.get("instructions").is_none(), "{body}");
    let input = body["input"].as_array().expect("input array");
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[0]["content"][0]["type"], "input_text");
    assert_eq!(
        input[0]["content"][0]["text"],
        "You are the Codewhale test system prompt."
    );
    assert_eq!(input[1]["role"], "user");
    assert_eq!(body["reasoning"], serde_json::json!({ "effort": "high" }));
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["strict"], false);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);

    // The same request on the generic Responses path still carries the
    // OpenAI-only fields, so the Concentrate branch is a deliberate subset.
    let generic = build_responses_body_for_provider(&request, ProviderKind::Openai, None);
    assert!(
        generic.get("store").is_some()
            && generic.get("include").is_some()
            && generic.get("instructions").is_some()
    );
}

#[test]
fn deepseek_flash_responses_body_uses_stateless_0731_contract() {
    let mut request = minimal_responses_request();
    request.model = "deepseek-v4-flash".to_string();
    request.reasoning_effort = Some("xhigh".to_string());
    request.temperature = Some(1.0);
    request.top_p = Some(0.95);
    request.messages.insert(
        0,
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: "preserve this tool-loop reasoning".to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            }],
        },
    );

    let body = build_responses_body_for_provider(&request, ProviderKind::Deepseek, None);

    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(body["max_output_tokens"], 128);
    assert_eq!(body["temperature"], 1.0);
    assert!(
        (body["top_p"].as_f64().expect("top_p number") - 0.95).abs() < 1e-6,
        "{}",
        body["top_p"]
    );
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert!(body.pointer("/reasoning/summary").is_none());
    assert!(body.get("include").is_none());
    assert!(body.get("store").is_none());
    assert_eq!(
        body.pointer("/input/0/content/0/type"),
        Some(&json!("reasoning_text"))
    );
    assert_eq!(
        body.pointer("/input/0/content/0/text"),
        Some(&json!("preserve this tool-loop reasoning"))
    );
    request.reasoning_effort = Some("auto".to_string());
    let automatic = build_responses_body_for_provider(&request, ProviderKind::Deepseek, None);
    assert!(automatic.get("reasoning").is_none());
}

#[test]
fn chatgpt_plan_body_omits_unsupported_output_caps() {
    // The official ChatGPT plan preview does not support output-cap fields.
    // Other Responses providers keep the central cap on the wire.
    let mut request = minimal_responses_request();
    request.max_tokens = 4_096;

    let codex = build_responses_body_for_provider(&request, ProviderKind::OpenaiCodex, None);
    assert!(
        codex.get("max_output_tokens").is_none(),
        "ChatGPT plan body names an unsupported output cap: {codex}"
    );
    assert!(
        codex.get("max_tokens").is_none() && codex.get("max_completion_tokens").is_none(),
        "no alternate output-cap spelling may sneak onto the Codex wire: {codex}"
    );

    let deepseek = build_responses_body_for_provider(&request, ProviderKind::Deepseek, None);
    assert_eq!(deepseek["max_output_tokens"], json!(4_096));
}

#[test]
fn chatgpt_replays_only_exact_grant_and_model_opaque_reasoning_state() {
    const SENTINEL: &str = "readable private reasoning must not be replayed";
    const SCOPE: &str = "openai-responses-siwc-v1:test-grant";
    let state = OpaqueReasoningState {
        provider: ProviderKind::OpenaiCodex.as_str().to_string(),
        api: SCOPE.to_string(),
        model: "gpt-5.5".to_string(),
        id: Some("rs_opaque".to_string()),
        encrypted_content: "enc_opaque_payload".to_string(),
    };
    let mut request = minimal_responses_request();
    request.messages.insert(
        0,
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: SENTINEL.to_string(),
                signature: None,
                state: Some(state),
                redacted_data: None,
            }],
        },
    );

    let exact = build_responses_body_for_provider(&request, ProviderKind::OpenaiCodex, Some(SCOPE));
    let exact_wire = exact.to_string();
    assert!(!exact_wire.contains(SENTINEL), "{exact}");
    assert_eq!(exact.pointer("/input/0/type"), Some(&json!("reasoning")));
    assert_eq!(exact.pointer("/input/0/id"), Some(&json!("rs_opaque")));
    assert_eq!(exact.pointer("/input/0/summary"), Some(&json!([])));
    assert_eq!(
        exact.pointer("/input/0/encrypted_content"),
        Some(&json!("enc_opaque_payload"))
    );

    for other_scope in [None, Some("openai-responses-siwc-v1:another-grant")] {
        let body =
            build_responses_body_for_provider(&request, ProviderKind::OpenaiCodex, other_scope);
        assert!(!body.to_string().contains("enc_opaque_payload"), "{body}");
        assert!(!body.to_string().contains(SENTINEL), "{body}");
    }
    let mut legacy = request.clone();
    if let ContentBlock::Thinking {
        state: Some(state), ..
    } = &mut legacy.messages[0].content[0]
    {
        state.api = "openai-responses".to_string();
    }
    let legacy_body =
        build_responses_body_for_provider(&legacy, ProviderKind::OpenaiCodex, Some(SCOPE));
    assert!(!legacy_body.to_string().contains("enc_opaque_payload"));

    request.model = "gpt-5.6".to_string();
    let switched_model =
        build_responses_body_for_provider(&request, ProviderKind::OpenaiCodex, Some(SCOPE));
    assert!(!switched_model.to_string().contains(SENTINEL));
    assert!(
        switched_model
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().all(|item| item["type"] != "reasoning")),
        "{switched_model}"
    );

    let switched_provider =
        build_responses_body_for_provider(&request, ProviderKind::Deepseek, None);
    let switched_wire = switched_provider.to_string();
    assert!(!switched_wire.contains(SENTINEL), "{switched_provider}");
    assert!(
        !switched_wire.contains("enc_opaque_payload"),
        "{switched_provider}"
    );
}

#[tokio::test]
async fn chatgpt_stream_captures_only_scoped_encrypted_reasoning() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"visible summary\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[],\"encrypted_content\":\"enc_state\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    for scope in [Some("openai-responses-siwc-v1:verified-test-grant"), None] {
        let mut client = CodewhaleClient::new(&test_codex_config(&server)).unwrap();
        // The local HTTP fixture stands in for the official transport; production
        // obtains this frozen marker only from its selected verified grant.
        client.chatgpt_reasoning_api = scope.map(str::to_string);
        let mut stream = client
            .handle_responses_stream(
                &client
                    .prepare_outbound_request(minimal_responses_request(), true)
                    .expect("responses request prepares"),
            )
            .await
            .unwrap();
        let mut captured = None;
        while let Some(event) = stream.next().await {
            if let StreamEvent::ContentBlockDelta {
                delta: Delta::ReasoningStateDelta { state },
                ..
            } = event.unwrap()
            {
                captured = Some(state);
            }
        }

        let Some(scope) = scope else {
            assert!(
                captured.is_none(),
                "a custom API-key route cannot mint grant state"
            );
            continue;
        };
        let state = captured.expect("encrypted reasoning state delta");
        assert_eq!(state.provider, ProviderKind::OpenaiCodex.as_str());
        assert_eq!(state.api, scope);
        assert_eq!(state.model, "gpt-5.5");
        assert_eq!(state.id.as_deref(), Some("rs_1"));
        assert_eq!(state.encrypted_content, "enc_state");
    }
}

#[test]
fn deepseek_responses_reasoning_effort_uses_documented_labels() {
    assert_eq!(responses_reasoning_effort("low", true), Some("low"));
    assert_eq!(responses_reasoning_effort("medium", true), Some("high"));
    assert_eq!(responses_reasoning_effort("high", true), Some("high"));
    assert_eq!(responses_reasoning_effort("xhigh", true), Some("high"));
    assert_eq!(responses_reasoning_effort("max", true), Some("max"));
    // The off tier must disable thinking on the wire, not collapse into
    // low: DeepSeek documents `reasoning.effort: "none"` as the off value.
    assert_eq!(responses_reasoning_effort("off", true), Some("none"));
    assert_eq!(responses_reasoning_effort("disabled", true), Some("none"));
    assert_eq!(responses_reasoning_effort("none", true), Some("none"));
    assert_eq!(responses_reasoning_effort("false", true), Some("none"));
    // minimal stays a low tier for DeepSeek (undocumented label preserved
    // for Codex compatibility).
    assert_eq!(responses_reasoning_effort("minimal", true), Some("low"));
}

#[tokio::test]
async fn generic_responses_captures_and_replays_opaque_reasoning() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("Content-Type", "text/event-stream")
            .set_body_string(concat!(
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_generic\"}}\n\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_generic\",\"encrypted_content\":\"enc_generic\"}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            )))
        .mount(&server).await;
    let config = {
        let __cw_config_init_1_0 = Some("openai".into());
        let __cw_config_init_1_1 = Some(ProvidersConfig {
            openai: ProviderConfig {
                api_key: Some("test-token".into()),
                base_url: Some(format!("{}/v1", server.uri())),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut __cw_config_init_1_value: Config = Default::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.providers = __cw_config_init_1_1;
        __cw_config_init_1_value
    };
    let client = CodewhaleClient::from_parts(
        format!("{}/v1", server.uri()),
        "gpt-5.5".into(),
        codewhale_config::provider::WireFormat::Responses,
        None,
        &config,
    )
    .unwrap();
    assert!(client.chatgpt_reasoning_api.is_none());
    let prepared = client
        .prepare_outbound_request(minimal_responses_request(), true)
        .unwrap();
    assert_eq!(
        prepared.endpoint.url,
        format!("{}/v1/responses", server.uri())
    );
    let mut stream = client.handle_responses_stream(&prepared).await.unwrap();
    let mut captured = None;
    while let Some(event) = stream.next().await {
        if let StreamEvent::ContentBlockDelta {
            delta: Delta::ReasoningStateDelta { state },
            ..
        } = event.unwrap()
        {
            captured = Some(state);
        }
    }
    let state = captured.expect("generic Responses must retain encrypted state");
    assert_eq!(state.provider, "openai");
    assert_eq!(state.api, "openai-responses");
    assert_eq!(state.model, "gpt-5.5");
    let mut continuation = minimal_responses_request();
    continuation.messages.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Thinking {
            thinking: "readable-private-summary".into(),
            signature: None,
            state: Some(state),
            redacted_data: None,
        }],
    });
    let replay = client
        .prepare_outbound_request(continuation.clone(), true)
        .unwrap();
    assert!(
        replay.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "reasoning" && item["encrypted_content"] == "enc_generic")
    );
    assert!(!replay.body.to_string().contains("readable-private-summary"));
    continuation.model = "another-model".into();
    let wrong_model = build_responses_body_for_provider(&continuation, ProviderKind::Openai, None);
    assert!(!wrong_model.to_string().contains("enc_generic"));
    let wrong_provider =
        build_responses_body_for_provider(&continuation, ProviderKind::Deepseek, None);
    assert!(!wrong_provider.to_string().contains("enc_generic"));
}

#[test]
fn codex_responses_body_uses_responses_reasoning_not_deepseek_thinking() {
    let request = MessageRequest {
        model: "gpt-6-astra".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some("max".to_string()),
        stream: None,
        temperature: None,
        top_p: None,
    };

    let body = build_responses_body(&request);

    assert_eq!(
        body.pointer("/reasoning/effort").and_then(Value::as_str),
        Some("max")
    );
    assert_eq!(
        body.pointer("/reasoning/summary").and_then(Value::as_str),
        Some("auto")
    );
    assert!(body.get("thinking").is_none());
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn responses_failed_event_reports_nested_error() {
    let event = json!({
        "type": "response.failed",
        "response": {
            "id": "resp_123",
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Please retry later"
            }
        }
    });

    let (code, message) = responses_event_error_details(&event);

    assert_eq!(code, "rate_limit_exceeded");
    assert_eq!(message, "Please retry later");
}

#[test]
fn responses_incomplete_event_reports_reason() {
    let event = json!({
        "type": "response.incomplete",
        "response": {
            "id": "resp_123",
            "status": "incomplete",
            "error": null,
            "incomplete_details": {
                "reason": "content_filter"
            }
        }
    });

    let (code, message) = responses_event_error_details(&event);

    assert_eq!(code, "content_filter");
    assert_eq!(message, "response incomplete: content_filter");
}

#[test]
fn responses_incomplete_stop_reason_preserves_provider_reason() {
    assert_eq!(
        responses_stop_reason(
            &json!({
                "status": "incomplete",
                "incomplete_details": { "reason": "max_output_tokens" }
            }),
            false,
        ),
        "incomplete:max_output_tokens"
    );
    assert_eq!(
        responses_stop_reason(&json!({"status": "incomplete"}), false),
        "incomplete:max_tokens"
    );
}

#[test]
fn parse_responses_usage_derives_cache_miss_and_reasoning() {
    let usage = json!({
        "input_tokens": 1000,
        "output_tokens": 200,
        "input_tokens_details": { "cached_tokens": 600 },
        "output_tokens_details": { "reasoning_tokens": 120 }
    });

    let parsed = parse_responses_usage(&usage);

    assert_eq!(parsed.input_tokens, 1000);
    assert_eq!(parsed.output_tokens, 200);
    assert_eq!(parsed.prompt_cache_hit_tokens, Some(600));
    // Cache-miss is derived as input minus the cached hit when cached > 0.
    assert_eq!(parsed.prompt_cache_miss_tokens, Some(400));
    // Reasoning surfaces from output_tokens_details (Responses dialect).
    assert_eq!(parsed.reasoning_tokens, Some(120));

    // Without cached/reasoning details, the derived fields stay None.
    let bare = json!({ "input_tokens": 1000, "output_tokens": 200 });
    let parsed_bare = parse_responses_usage(&bare);
    assert_eq!(parsed_bare.prompt_cache_hit_tokens, None);
    assert_eq!(parsed_bare.prompt_cache_miss_tokens, None);
    assert_eq!(parsed_bare.reasoning_tokens, None);
}

#[test]
fn parse_responses_usage_saturates_u64_fields() {
    let parsed = parse_responses_usage(&json!({
        "input_tokens": u64::MAX,
        "output_tokens": u64::MAX,
        "input_tokens_details": { "cached_tokens": u64::MAX },
        "output_tokens_details": { "reasoning_tokens": u64::MAX }
    }));
    assert_eq!(parsed.input_tokens, u32::MAX);
    assert_eq!(parsed.output_tokens, u32::MAX);
    assert_eq!(parsed.prompt_cache_hit_tokens, Some(u32::MAX));
    assert_eq!(parsed.prompt_cache_miss_tokens, Some(0));
    assert_eq!(parsed.reasoning_tokens, Some(u32::MAX));
}

#[test]
fn parse_responses_usage_reads_deepseek_top_level_cache_fields() {
    // DeepSeek's Responses dialect reports cache telemetry as top-level
    // `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` with
    // `cache_write_tokens` nested under `input_tokens_details` -- none of
    // which the old parser read (it only looked at
    // `input_tokens_details.cached_tokens`, which DeepSeek leaves unset,
    // so every V4 Flash turn recorded cache_hit = None).
    let usage = json!({
        "input_tokens": 1_000,
        "output_tokens": 200,
        "prompt_cache_hit_tokens": 600,
        "prompt_cache_miss_tokens": 200,
        "input_tokens_details": { "cached_tokens": 999, "cache_write_tokens": 100 },
        "output_tokens_details": { "reasoning_tokens": 120 }
    });

    let parsed = parse_responses_usage(&usage);

    // Top-level DeepSeek fields win over the nested OpenAI-style shape,
    // and the explicit miss is trusted over the derived fallback.
    assert_eq!(parsed.prompt_cache_hit_tokens, Some(600));
    assert_eq!(parsed.prompt_cache_miss_tokens, Some(200));
    assert_eq!(parsed.prompt_cache_write_tokens, Some(100));
    // `input_tokens` remains the provider-reported total; the pricing
    // layer partitions it into hit / miss / write classes.
    assert_eq!(parsed.input_tokens, 1_000);
    assert_eq!(parsed.output_tokens, 200);
    assert_eq!(parsed.reasoning_tokens, Some(120));

    // The parsed fields must reach the pricing classes unchanged: 600 hit
    // at the cache-read rate, 100 write at the creation rate, and the
    // remaining 300 (200 reported miss + 100 uncategorized) at the miss
    // rate -- instead of the pre-fix all-raw-input miss billing.
    let classes = crate::pricing::token_usage_for_pricing(&parsed);
    assert_eq!(classes.input, 300);
    assert_eq!(classes.cache_read, 600);
    assert_eq!(classes.cache_write, 100);
}

#[test]
fn parse_responses_usage_keeps_old_shape_with_cache_write_fallback() {
    // OpenAI-style payloads still parse from `input_tokens_details` alone:
    // hit from `cached_tokens` (fallback), miss derived as input minus
    // hit, and the write class from `cache_write_tokens` when present.
    let usage = json!({
        "input_tokens": 1_000,
        "output_tokens": 200,
        "input_tokens_details": { "cached_tokens": 600, "cache_write_tokens": 100 }
    });

    let parsed = parse_responses_usage(&usage);

    assert_eq!(parsed.input_tokens, 1_000);
    assert_eq!(parsed.prompt_cache_hit_tokens, Some(600));
    assert_eq!(parsed.prompt_cache_miss_tokens, Some(400));
    assert_eq!(parsed.prompt_cache_write_tokens, Some(100));
    assert_eq!(parsed.reasoning_tokens, None);
}

/// Regression fixture for the reasoning double-billing bug: a real
/// Responses usage payload has to survive the whole way into the pricing
/// conversion without reasoning tokens being charged twice. OpenAI's
/// `output_tokens` is already the *total* billable completion count, with
/// `output_tokens_details.reasoning_tokens` a subset of it.
#[test]
fn responses_usage_reaches_pricing_conversion_without_double_billing_reasoning() {
    use crate::pricing::{calculate_turn_cost_estimate_for_provider, token_usage_for_pricing};
    use codewhale_config::ProviderKind;

    let usage = parse_responses_usage(&json!({
        "input_tokens": 10_000,
        "output_tokens": 4_000,
        "total_tokens": 14_000,
        "input_tokens_details": { "cached_tokens": 6_000 },
        "output_tokens_details": { "reasoning_tokens": 3_500 }
    }));

    let classes = token_usage_for_pricing(&usage);
    assert_eq!(classes.output, 4_000, "reasoning must not inflate output");
    assert_eq!(classes.input, 4_000);
    assert_eq!(classes.cache_read, 6_000);
    assert_eq!(classes.cache_write, 0);

    // gpt-5.5: 0.50 cache-read / 5.00 input / 30.00 output per million.
    let cost = calculate_turn_cost_estimate_for_provider(ProviderKind::Openai, "gpt-5.5", &usage)
        .expect("direct OpenAI route is priced");
    let expected = 0.006 * 0.50 + 0.004 * 5.00 + 0.004 * 30.00;
    assert!(
        (cost.usd - expected).abs() < 1e-12,
        "expected {expected}, got {}",
        cost.usd
    );

    // The bug charged the 3_500 reasoning tokens a second time at the
    // output rate; assert the difference explicitly so a reintroduction is
    // unambiguous rather than a silent number change.
    let double_billed = expected + 0.0035 * 30.00;
    assert!((cost.usd - double_billed).abs() > 1e-6);
}

#[test]
fn responses_input_includes_user_role_tool_results() {
    let request = MessageRequest {
        model: "gpt-5.5".to_string(),
        messages: vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call_abc|fc_123".to_string(),
                    name: "checklist_write".to_string(),
                    input: json!({"items": []}),
                    caller: None,
                    thought_signature: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_abc|fc_123".to_string(),
                    content: "<6 items>".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    };

    let input = convert_messages_to_responses_input(&request, ProviderKind::OpenaiCodex, None);

    assert_eq!(input[0]["type"], "function_call");
    assert_eq!(input[0]["call_id"], "call_abc");
    assert_eq!(input[0]["name"], "checklist_write");
    assert_eq!(input[0]["namespace"], "codewhale");
    assert_eq!(input[1]["type"], "function_call_output");
    assert_eq!(input[1]["call_id"], "call_abc");
    assert_eq!(input[1]["output"], "<6 items>");
}

#[test]
fn responses_input_encodes_tool_call_names() {
    let request = MessageRequest {
        model: "gpt-5.5".to_string(),
        messages: vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call_abc|fc_123".to_string(),
                name: "web.run".to_string(),
                input: json!({}),
                caller: None,
                thought_signature: None,
            }],
        }],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    };

    let input = convert_messages_to_responses_input(&request, ProviderKind::OpenaiCodex, None);

    assert_eq!(input[0]["type"], "function_call");
    assert_eq!(input[0]["name"], to_api_tool_name("web.run"));
    assert_eq!(input[0]["namespace"], "codewhale");
    let generic = convert_messages_to_responses_input(&request, ProviderKind::Openai, None);
    assert!(generic[0].get("namespace").is_none());
}

#[test]
fn responses_function_tool_sanitizes_root_composition_schema() {
    let tool = Tool {
        tool_type: None,
        name: "web.run".to_string(),
        description: "Apply patch".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string"},
                "replace": {"type": "array"},
                "changes": {"type": "array"}
            },
            "oneOf": [
                {"required": ["patch"]},
                {"required": ["replace"]},
                {"required": ["changes"]}
            ]
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    };

    let payload = tool_to_responses_function(&tool);
    let parameters = &payload["parameters"];

    assert_eq!(payload["name"], to_api_tool_name("web.run"));
    assert_eq!(parameters["type"], "object");
    assert!(parameters.get("oneOf").is_none());
    assert!(parameters.get("anyOf").is_none());
    assert!(parameters.get("allOf").is_none());
    assert!(parameters.get("enum").is_none());
    assert!(parameters.get("not").is_none());
    assert!(parameters["properties"].get("patch").is_some());
    assert!(parameters["properties"].get("replace").is_some());
    assert!(parameters["properties"].get("changes").is_some());
    assert_eq!(
        payload["description"],
        "Apply patch\n\nExactly one of these parameter groups must be provided: `changes` | `patch` | `replace`."
    );
    assert!(tool.input_schema.get("oneOf").is_some());
}

#[test]
fn responses_function_tool_trims_description_before_constraint_note() {
    let tool = Tool {
        tool_type: None,
        name: "apply_patch".to_string(),
        description: "Apply patch\n".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string"},
                "replace": {"type": "array"},
                "changes": {"type": "array"}
            },
            "oneOf": [
                {"required": ["patch"]},
                {"required": ["replace"]},
                {"required": ["changes"]}
            ]
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    };

    let payload = tool_to_responses_function(&tool);

    assert_eq!(
        payload["description"],
        "Apply patch\n\nExactly one of these parameter groups must be provided: `changes` | `patch` | `replace`."
    );
}

#[test]
fn responses_function_tool_leaves_description_unchanged_without_constraint_note() {
    let tool = Tool {
        tool_type: None,
        name: "lookup".to_string(),
        description: "Lookup".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        }),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: None,
        cache_control: None,
    };

    let payload = tool_to_responses_function(&tool);

    assert_eq!(payload["description"], "Lookup");
}

/// The Responses API projection of [`ContentBlock::ImageUrl`].
///
/// Responses is the odd one out: the image part carries `image_url` as a bare
/// string rather than the nested object Chat Completions uses. Getting that
/// wrong produces a schema error from OpenAI rather than anything that names
/// the image, so it is worth pinning explicitly.
#[test]
fn user_image_becomes_an_input_image_item() {
    const DATA_URL: &str = "data:image/png;base64,QUJD";

    let mut request = minimal_responses_request();
    request.messages[0].content.push(ContentBlock::ImageUrl {
        image_url: codewhale_models::ImageUrlContent {
            url: DATA_URL.to_string(),
        },
    });

    let items = convert_messages_to_responses_input(&request, ProviderKind::OpenaiCodex, None);

    let user = items
        .iter()
        .find(|item| item["role"] == "user")
        .expect("a user item");
    let content = user["content"].as_array().expect("content items");

    let image = content
        .iter()
        .find(|part| part["type"] == "input_image")
        .expect("an input_image part");
    assert_eq!(
        image["image_url"], DATA_URL,
        "Responses takes image_url as a bare string, not a nested object: {image}"
    );

    assert!(
        content.iter().any(|part| part["type"] == "input_text"),
        "the accompanying question must survive: {user}"
    );
}

#[test]
fn tool_result_image_becomes_native_function_output_content() {
    let mut request = minimal_responses_request();
    request.messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call_image_1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "shot.png"}),
                caller: None,
                thought_signature: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call_image_1".to_string(),
                content: "screenshot captured".to_string(),
                is_error: Some(false),
                content_blocks: Some(vec![serde_json::json!({
                    "type": "image",
                    "mime_type": "image/png",
                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
                })]),
            }],
        },
    ];

    let items = convert_messages_to_responses_input(&request, ProviderKind::OpenaiCodex, None);
    let output = items
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .expect("function output");
    let content = output["output"].as_array().expect("rich output array");

    assert_eq!(
        content[0],
        serde_json::json!({
            "type": "input_text",
            "text": "screenshot captured",
        })
    );
    assert_eq!(content[1]["type"], "input_image");
    assert_eq!(
        content[1]["image_url"],
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
    );
}

/// A `system`-role history message — the shape a compaction summary, a branch
/// summary, or an imported journal `system` entry takes once it reaches
/// `MessageRequest::messages` — must survive the Responses conversion. The
/// Chat Completions adapter already keeps it
/// (`request_builder_preserves_internal_system_messages`); dropping it here
/// silently deletes the only record of everything the compaction replaced.
#[test]
fn responses_input_preserves_system_history_with_the_provider_role() {
    let mut request = minimal_responses_request();
    request.messages.insert(
        0,
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text {
                text: "[compaction summary] the user is porting the parser".to_string(),
                cache_control: None,
            }],
        },
    );

    for (provider, role) in [
        (ProviderKind::OpenaiCodex, "developer"),
        (ProviderKind::Openai, "system"),
    ] {
        let items = convert_messages_to_responses_input(&request, provider, None);
        let system = items
            .iter()
            .find(|item| item["role"] == role)
            .expect("system history survives under the provider-supported role");
        assert_eq!(system["type"], "message");
        assert_eq!(
            system["content"][0],
            serde_json::json!({
                "type": "input_text",
                "text": "[compaction summary] the user is porting the parser",
            })
        );
    }
}
