//! Moonshot/Kimi native search adapters.

use anyhow::{Context, Result, bail};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    ProviderNativeSearchClient, ProviderNativeSearchRequest, ProviderNativeSearchResponse,
    citation_from_url, citations_from_text, finish_reason_is_length, joined_answer, push_citation,
};
use crate::client::api_url;
use codewhale_config::catalog::reviewed::constants::MOONSHOT_KIMI_K3_MODEL;
use codewhale_runtime::config::moonshot_base_url_is_exact_kimi_code;

const MAX_NATIVE_SEARCH_ROUNDS: usize = 4;
const MAX_NATIVE_SEARCH_TOOL_CALLS: usize = 8;
/// What the Kimi adapters requested before #6508, kept for a model whose
/// output ceiling the catalogue does not document.
pub(super) const PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS: u32 = 4_096;
const WEB_SEARCH_FORMULA_URI: &str = "moonshot/web-search:latest";
const WEB_SEARCH_FORMULA_FUNCTION: &str = "web_search";

pub(super) async fn search(
    client: &ProviderNativeSearchClient,
    request: &ProviderNativeSearchRequest,
) -> Result<ProviderNativeSearchResponse> {
    if moonshot_base_url_is_exact_kimi_code(&client.inner.base_url) {
        // Exact Kimi Code membership endpoint only: the structured `/search`
        // service is a first-party contract, so differently-cased or adjacent
        // Kimi-hosted paths must not inherit it.
        search_kimi_code(client, request).await
    } else if client
        .inner
        .default_model
        .trim()
        .eq_ignore_ascii_case(MOONSHOT_KIMI_K3_MODEL)
    {
        search_formula(client, request).await
    } else {
        search_builtin(client, request).await
    }
}

async fn search_kimi_code(
    client: &ProviderNativeSearchClient,
    request: &ProviderNativeSearchRequest,
) -> Result<ProviderNativeSearchResponse> {
    let call_id = HeaderValue::from_str(&Uuid::new_v4().to_string())
        .context("failed to build Kimi search call id")?;
    let url = format!("{}/search", client.inner.base_url.trim_end_matches('/'));
    let payload = client
        .post_json(
            &url,
            &json!({ "text_query": request.query }),
            &[(HeaderName::from_static("x-msh-tool-call-id"), call_id)],
        )
        .await?;
    Ok(parse_kimi_code(&payload))
}

async fn search_builtin(
    client: &ProviderNativeSearchClient,
    request: &ProviderNativeSearchRequest,
) -> Result<ProviderNativeSearchResponse> {
    let tools = builtin_search_tools();
    let mut messages = vec![json!({
        "role": "user",
        "content": super::search_prompt(request),
    })];
    let mut tool_calls_executed = 0;
    let url = api_url(&client.inner.base_url, "chat/completions");
    let max_completion_tokens = client
        .requested_answer_output_tokens()
        .unwrap_or(PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS);

    for _ in 0..MAX_NATIVE_SEARCH_ROUNDS {
        let body = json!({
            "model": client.inner.default_model,
            "messages": &messages,
            "tools": &tools,
            "max_completion_tokens": max_completion_tokens,
            "stream": false,
            "thinking": { "type": "disabled" },
        });
        let payload = client.post_json(&url, &body, &[]).await?;
        let choice = payload
            .pointer("/choices/0")
            .context("Kimi web search response omitted choices[0]")?;
        let message = choice
            .get("message")
            .and_then(Value::as_object)
            .context("Kimi web search response omitted assistant message")?;
        if choice.get("finish_reason").and_then(Value::as_str) != Some("tool_calls") {
            return Ok(parse_final_message(message, choice));
        }

        messages.push(Value::Object(message.clone()));
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .context("Kimi returned tool_calls finish reason without tool calls")?;
        if tool_calls.is_empty() {
            bail!("Kimi returned an empty native web-search tool call list");
        }
        reserve_native_search_tool_calls(&mut tool_calls_executed, tool_calls.len())?;
        for tool_call in tool_calls {
            if tool_call.pointer("/function/name").and_then(Value::as_str) != Some("$web_search") {
                bail!("Kimi native search requested an unexpected tool");
            }
            let id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .context("Kimi native web-search call omitted id")?;
            let arguments = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .context("Kimi native web-search call omitted arguments")?;
            let _: Value = serde_json::from_str(arguments)
                .context("Kimi native web-search arguments were not valid JSON")?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "name": "$web_search",
                "content": arguments,
            }));
        }
    }

    bail!("Kimi native web search exceeded the bounded tool-call loop")
}

async fn search_formula(
    client: &ProviderNativeSearchClient,
    request: &ProviderNativeSearchRequest,
) -> Result<ProviderNativeSearchResponse> {
    let formula_path = format!("formulas/{WEB_SEARCH_FORMULA_URI}");
    let tools_payload = client
        .get_json(&api_url(
            &client.inner.base_url,
            &format!("{formula_path}/tools"),
        ))
        .await?;
    let tools = formula_web_search_tools(&tools_payload)?;
    let mut messages = vec![json!({
        "role": "user",
        "content": super::search_prompt(request),
    })];
    let mut tool_calls_executed = 0;
    let chat_url = api_url(&client.inner.base_url, "chat/completions");
    let max_completion_tokens = client
        .requested_answer_output_tokens()
        .unwrap_or(PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS);
    let fiber_url = api_url(&client.inner.base_url, &format!("{formula_path}/fibers"));

    for _ in 0..MAX_NATIVE_SEARCH_ROUNDS {
        let body = json!({
            "model": client.inner.default_model,
            "messages": &messages,
            "tools": &tools,
            "max_completion_tokens": max_completion_tokens,
            "stream": false,
        });
        let payload = client.post_json(&chat_url, &body, &[]).await?;
        let choice = payload
            .pointer("/choices/0")
            .context("Kimi Formula web search response omitted choices[0]")?;
        let message = choice
            .get("message")
            .and_then(Value::as_object)
            .context("Kimi Formula web search response omitted assistant message")?;
        let Some(tool_calls) = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .filter(|calls| !calls.is_empty())
        else {
            return Ok(parse_final_message(message, choice));
        };

        reserve_native_search_tool_calls(&mut tool_calls_executed, tool_calls.len())?;
        messages.push(Value::Object(message.clone()));
        for tool_call in tool_calls {
            let id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .context("Kimi Formula web-search call omitted id")?;
            let function = tool_call
                .get("function")
                .and_then(Value::as_object)
                .context("Kimi Formula web-search call omitted function")?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .context("Kimi Formula web-search call omitted function name")?;
            if name != WEB_SEARCH_FORMULA_FUNCTION {
                bail!("Kimi Formula web search requested an unexpected tool");
            }
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .context("Kimi Formula web-search call omitted arguments")?;
            let _: Value = serde_json::from_str(arguments)
                .context("Kimi Formula web-search arguments were not valid JSON")?;
            let fiber = client
                .post_json(
                    &fiber_url,
                    &json!({ "name": name, "arguments": arguments }),
                    &[],
                )
                .await?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": formula_fiber_result(&fiber)?,
            }));
        }
    }

    bail!("Kimi Formula web search exceeded the bounded tool-call loop")
}

fn reserve_native_search_tool_calls(executed: &mut usize, additional: usize) -> Result<()> {
    let total = executed
        .checked_add(additional)
        .context("Kimi native web search tool-call count overflowed")?;
    if total > MAX_NATIVE_SEARCH_TOOL_CALLS {
        bail!(
            "Kimi native web search exceeded the {MAX_NATIVE_SEARCH_TOOL_CALLS}-call safety limit"
        );
    }
    *executed = total;
    Ok(())
}

fn formula_web_search_tools(payload: &Value) -> Result<Value> {
    let tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .context("Kimi web-search Formula omitted tools")?;
    if tools.len() != 1
        || tools[0].get("type").and_then(Value::as_str) != Some("function")
        || tools[0].pointer("/function/name").and_then(Value::as_str)
            != Some(WEB_SEARCH_FORMULA_FUNCTION)
    {
        bail!("Kimi web-search Formula returned an unexpected tool declaration");
    }
    Ok(Value::Array(tools.clone()))
}

fn formula_fiber_result(payload: &Value) -> Result<&str> {
    if payload.get("status").and_then(Value::as_str) != Some("succeeded") {
        bail!("Kimi web-search Formula fiber did not succeed");
    }
    payload
        .pointer("/context/output")
        .or_else(|| payload.pointer("/context/encrypted_output"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|result| !result.is_empty())
        .context("Kimi web-search Formula fiber omitted its result")
}

fn builtin_search_tools() -> Value {
    json!([{
        "type": "builtin_function",
        "function": { "name": "$web_search" }
    }])
}

fn parse_kimi_code(payload: &Value) -> ProviderNativeSearchResponse {
    let mut citations = Vec::new();
    if let Some(results) = payload.get("search_results").and_then(Value::as_array) {
        for result in results {
            let Some(url) = result.get("url").and_then(Value::as_str) else {
                continue;
            };
            let title = result
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let snippet = result
                .get("snippet")
                .and_then(Value::as_str)
                .map(str::to_string);
            let published = result
                .get("date")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            push_citation(
                &mut citations,
                citation_from_url(url, title, snippet, published),
            );
        }
    }
    ProviderNativeSearchResponse {
        answer: None,
        citations,
        truncated: false,
    }
}

/// The final answer of a Kimi search loop. `finish_reason: "length"` means
/// Kimi stopped it at the output limit, so it is marked as cut (#6508).
fn parse_final_message(
    message: &Map<String, Value>,
    choice: &Value,
) -> ProviderNativeSearchResponse {
    let answer = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let citations = answer
        .as_deref()
        .map(citations_from_text)
        .unwrap_or_default();
    ProviderNativeSearchResponse {
        answer: joined_answer(answer.into_iter().collect()),
        citations,
        truncated: finish_reason_is_length(Some(choice)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ProviderNativeSearchRequest {
        ProviderNativeSearchRequest {
            query: "current release".to_string(),
            max_results: 3,
            domains: Vec::new(),
        }
    }

    #[test]
    fn kimi_code_request_and_structured_response_contract() {
        let body = json!({ "text_query": request().query });
        assert_eq!(body["text_query"], "current release");
        assert_eq!(body.as_object().map(serde_json::Map::len), Some(1));

        let parsed = parse_kimi_code(&json!({
            "search_results": [{
                "title": "Kimi",
                "url": "https://example.com/kimi",
                "snippet": "Summary",
                "date": "2026-08-28"
            }]
        }));
        assert_eq!(parsed.citations.len(), 1);
        assert_eq!(parsed.citations[0].snippet.as_deref(), Some("Summary"));
        assert_eq!(parsed.citations[0].published.as_deref(), Some("2026-08-28"));
    }

    #[test]
    fn direct_search_contracts_are_bounded() {
        let tools = builtin_search_tools();
        assert_eq!(tools[0]["function"]["name"], "$web_search");
        assert_eq!(PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS, 4_096);

        let formula_tools = formula_web_search_tools(&json!({
            "tools": [{
                "type": "function",
                "function": { "name": "web_search" }
            }]
        }))
        .expect("formula tools");
        assert_eq!(formula_tools[0]["function"]["name"], "web_search");
        assert_eq!(
            formula_fiber_result(&json!({
                "status": "succeeded",
                "context": { "encrypted_output": "encrypted result" }
            }))
            .expect("formula result"),
            "encrypted result"
        );
    }

    #[test]
    fn native_search_tool_call_limit_is_total_not_per_round() {
        let mut executed = 0;
        reserve_native_search_tool_calls(&mut executed, 4).expect("first rounds");
        reserve_native_search_tool_calls(&mut executed, 4).expect("final allowed round");
        assert_eq!(executed, MAX_NATIVE_SEARCH_TOOL_CALLS);
        assert!(reserve_native_search_tool_calls(&mut executed, 1).is_err());
    }

    #[test]
    fn kimi_code_dispatch_reuses_the_exact_route_matcher() {
        for route in [
            "https://api.kimi.com/coding/v1",
            "https://api.kimi.com/coding/v1/",
            "HTTPS://API.KIMI.COM/coding/v1",
        ] {
            assert!(
                moonshot_base_url_is_exact_kimi_code(route),
                "{route} is the membership endpoint"
            );
        }
        for neighboring_route in [
            // A case-variant path is a different route, not the official one.
            "https://API.KIMI.COM/CODING/V1",
            "https://api.kimi.com/coding/v2",
            "https://api.kimi.com/coding",
            "http://api.kimi.com/coding/v1",
            "https://api.moonshot.ai/v1",
        ] {
            assert!(
                !moonshot_base_url_is_exact_kimi_code(neighboring_route),
                "{neighboring_route} must not reach the Kimi Code /search service"
            );
        }
    }

    #[test]
    fn finish_reason_length_marks_the_final_answer_as_cut() {
        // Both Kimi loops end in parse_final_message. A reply stopped at the
        // output limit used to be returned as a finished answer (#6508).
        let message = json!({ "role": "assistant", "content": "Partial answer" });
        let message = message.as_object().expect("object");
        let cut = parse_final_message(message, &json!({ "finish_reason": "length" }));
        assert!(cut.truncated);
        assert_eq!(cut.answer.as_deref(), Some("Partial answer"));
        let whole = parse_final_message(message, &json!({ "finish_reason": "stop" }));
        assert!(!whole.truncated);
    }

    #[test]
    fn model_without_a_documented_ceiling_keeps_4096() {
        let config = {
            let __cw_config_init_0_0 = Some("moonshot".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                moonshot: ProviderConfig {
                    api_key: Some("moonshot-test-key".to_string()),
                    base_url: Some("https://api.moonshot.ai/v1".to_string()),
                    model: Some("kimi-unlisted-test-model".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let client = ProviderNativeSearchClient::new(
            crate::client::CodewhaleClient::new(&config).expect("test Moonshot client"),
        )
        .expect("Moonshot native adapter");
        let route_cap = client
            .inner
            .effective_max_output_tokens("kimi-unlisted-test-model");
        assert_eq!(
            client.answer_output_tokens(PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS),
            PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS.min(route_cap)
        );
    }

    #[tokio::test]
    async fn k3_formula_executes_tool_fiber_and_returns_citations() {
        let server = MockServer::start().await;
        let config = {
            let __cw_config_init_1_0 = Some("moonshot".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                moonshot: ProviderConfig {
                    api_key: Some("moonshot-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some("kimi-k3".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let client = ProviderNativeSearchClient::new(
            crate::client::CodewhaleClient::new(&config).expect("test Moonshot client"),
        )
        .expect("Moonshot native adapter");
        let expected_completion_tokens = client
            .requested_answer_output_tokens()
            .expect("Kimi requests an explicit answer length");
        Mock::given(method("GET"))
            .and(path("/v1/formulas/moonshot/web-search:latest/tools"))
            .and(header("authorization", "Bearer moonshot-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "description": "Search the web",
                        "parameters": {
                            "type": "object",
                            "properties": { "query": { "type": "string" } },
                            "required": ["query"]
                        }
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer moonshot-test-key"))
            .and(body_partial_json(json!({
                "model": "kimi-k3",
                "max_completion_tokens": expected_completion_tokens,
                "tools": [{
                    "type": "function",
                    "function": { "name": "web_search" }
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "web_search:0",
                            "type": "function",
                            "function": {
                                "name": "web_search",
                                "arguments": "{\"query\":\"current release\"}"
                            }
                        }]
                    }
                }]
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/formulas/moonshot/web-search:latest/fibers"))
            .and(header("authorization", "Bearer moonshot-test-key"))
            .and(body_partial_json(json!({
                "name": "web_search",
                "arguments": "{\"query\":\"current release\"}"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "succeeded",
                "context": { "encrypted_output": "encrypted-search-result" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer moonshot-test-key"))
            .and(body_string_contains("encrypted-search-result"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "role": "assistant",
                        "content": "See https://example.com/kimi for the current result."
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = search_formula(&client, &request())
            .await
            .expect("K3 Formula search");

        assert_eq!(response.citations.len(), 1);
        assert_eq!(response.citations[0].url, "https://example.com/kimi");
    }
}
