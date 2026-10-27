//! Narrow provider-native web-search client.
//!
//! This adapter reuses the active route's authenticated HTTP client without
//! exposing credentials to tool code. Route capability facts decide whether
//! the adapter is attached; this module only speaks the three documented
//! first-party wire contracts.

use anyhow::{Context, Result, bail};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Value, json};

use codewhale_config::ProviderKind;

use super::{CodewhaleClient, api_url, responses_api_url};

mod zai;

mod kimi;

#[derive(Clone)]
pub(crate) struct ProviderNativeSearchClient {
    pub(super) inner: CodewhaleClient,
}

#[derive(Clone)]
pub(crate) struct ProviderNativeSearchRequest {
    pub(crate) query: String,
    pub(crate) max_results: u8,
    pub(crate) domains: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderNativeCitation {
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) snippet: Option<String>,
    pub(crate) published: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderNativeSearchResponse {
    pub(crate) answer: Option<String>,
    pub(crate) citations: Vec<ProviderNativeCitation>,
    /// The provider stopped the answer at its output limit (#6508).
    pub(crate) truncated: bool,
}

/// What the Anthropic and MiMo adapters requested before #6508, kept for a
/// model whose output ceiling the catalogue does not document.
const PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS: u32 = 2_048;

/// Ceiling on the output a native-search request asks for (#6508). These are
/// single-shot, non-streaming requests: nothing arrives until the whole answer
/// is generated, so web_search gives the attempt enough time to generate what
/// was asked for (`tools::web_search::native_answer_time_budget`). The clamp
/// bounds that wait: at the budget's assumed generation rate, 8,192 tokens is
/// about four minutes before a stalled provider falls back to another backend.
/// It is not a context or API limit; the model's own output ceiling still
/// applies beneath it.
const NATIVE_SEARCH_MAX_OUTPUT_TOKENS: u32 = 8_192;

impl ProviderNativeSearchClient {
    #[must_use]
    pub(crate) fn new(inner: CodewhaleClient) -> Option<Self> {
        matches!(
            inner.api_provider,
            ProviderKind::Openai
                | ProviderKind::Anthropic
                | ProviderKind::Xai
                | ProviderKind::XiaomiMimo
                | ProviderKind::Zai
                | ProviderKind::ModelstudioTokenPlan
                | ProviderKind::Deepseek
                | ProviderKind::Moonshot
        )
        .then_some(Self { inner })
    }

    #[must_use]
    pub(crate) fn provider(&self) -> ProviderKind {
        self.inner.api_provider
    }

    #[must_use]
    pub(crate) fn model(&self) -> &str {
        &self.inner.default_model
    }

    /// Output tokens to request for a native search answer: the search
    /// model's route ceiling up to [`NATIVE_SEARCH_MAX_OUTPUT_TOKENS`]. When
    /// the catalogue does not document that ceiling, keep `fallback` (the
    /// value this adapter always sent) rather than risk a provider rejecting
    /// a larger request.
    #[must_use]
    pub(super) fn answer_output_tokens(&self, fallback: u32) -> u32 {
        let model = &self.inner.default_model;
        let route_cap = self.inner.effective_max_output_tokens(model);
        let documented = matches!(
            crate::route_budget::output_ceiling_source(self.inner.api_provider, model),
            crate::route_budget::OutputCeilingSource::Documented(_)
        );
        let wanted = if documented {
            NATIVE_SEARCH_MAX_OUTPUT_TOKENS
        } else {
            fallback
        };
        wanted.min(route_cap)
    }

    /// Output tokens this client asks for per native-search answer, or `None`
    /// for adapters that leave the answer length to the provider's default
    /// (the Responses and Z.ai adapters). web_search sizes the attempt's time
    /// budget from this, so the request and the wait cannot disagree.
    #[must_use]
    pub(crate) fn requested_answer_output_tokens(&self) -> Option<u32> {
        match self.inner.api_provider {
            ProviderKind::Anthropic | ProviderKind::XiaomiMimo => {
                Some(self.answer_output_tokens(PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS))
            }
            ProviderKind::Moonshot => {
                Some(self.answer_output_tokens(kimi::PRIOR_NATIVE_SEARCH_MAX_COMPLETION_TOKENS))
            }
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    #[must_use]
    pub(crate) fn host(&self) -> Option<String> {
        reqwest::Url::parse(&self.inner.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    }

    #[must_use]
    pub(crate) fn cache_identity(&self) -> String {
        format!(
            "provider-native://{}/{}/{}",
            self.inner.api_provider.as_str(),
            self.host().as_deref().unwrap_or("unknown-host"),
            self.inner.default_model
        )
    }

    #[must_use]
    pub(crate) const fn maximum_domain_count(&self) -> Option<usize> {
        match self.inner.api_provider {
            ProviderKind::Xai => Some(5),
            ProviderKind::Openai => Some(100),
            ProviderKind::Anthropic => None,
            _ => Some(0),
        }
    }

    pub(crate) async fn search(
        &self,
        request: &ProviderNativeSearchRequest,
    ) -> Result<ProviderNativeSearchResponse> {
        // This adapter performs model-backed inference directly instead of
        // calling `CodewhaleClient::create_message*`. It must therefore join
        // the same attached-run ownership boundary explicitly. The guard is
        // retained through response decode so a relay writer cannot start
        // while this result is still able to feed the interactive turn.
        let _inference = self.inner.acquire_remote_control_inference_permit().await;
        if self.inner.api_provider == ProviderKind::Moonshot {
            // Kimi/Moonshot runs a bounded multi-round agentic search with its
            // own request/reply loop, so it cannot share the single-shot body
            // dispatch below. It still runs under the inference permit above.
            let mut parsed = kimi::search(self, request).await?;
            parsed.citations.truncate(usize::from(request.max_results));
            return Ok(parsed);
        }
        let body = match self.inner.api_provider {
            ProviderKind::Openai => build_responses_search_body(
                &self.inner.default_model,
                request,
                ResponsesSearchDialect::Openai,
            ),
            ProviderKind::Xai => build_responses_search_body(
                &self.inner.default_model,
                request,
                ResponsesSearchDialect::Xai,
            ),
            ProviderKind::ModelstudioTokenPlan => build_responses_search_body(
                &self.inner.default_model,
                request,
                ResponsesSearchDialect::ModelStudio,
            ),
            ProviderKind::Deepseek => build_responses_search_body(
                &self.inner.default_model,
                request,
                ResponsesSearchDialect::Deepseek,
            ),
            ProviderKind::Anthropic => build_anthropic_search_body(
                &self.inner.default_model,
                request,
                self.requested_answer_output_tokens()
                    .unwrap_or(PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS),
            ),
            ProviderKind::XiaomiMimo => build_mimo_search_body(
                &self.inner.default_model,
                request,
                self.requested_answer_output_tokens()
                    .unwrap_or(PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS),
            ),
            ProviderKind::Zai => zai::build_body(request, &self.inner.base_url)?,
            _ => bail!("active provider has no native web-search adapter"),
        };
        let url = match self.inner.api_provider {
            ProviderKind::Openai | ProviderKind::Xai | ProviderKind::ModelstudioTokenPlan => {
                api_url(&self.inner.base_url, "responses")
            }
            ProviderKind::Deepseek => {
                responses_api_url(&self.inner.base_url, self.inner.api_provider)
            }
            ProviderKind::Anthropic => anthropic_messages_url(&self.inner.base_url),
            ProviderKind::XiaomiMimo => api_url(&self.inner.base_url, "chat/completions"),
            ProviderKind::Zai => api_url(&self.inner.base_url, "web_search"),
            _ => unreachable!("provider checked above"),
        };
        let body_bytes = serde_json::to_vec(&body)
            .context("failed to serialize provider-native web-search request")?;
        let response = self
            .inner
            .send_with_retry(|| {
                self.inner
                    .http_client
                    .post(&url)
                    .header("Accept", "application/json")
                    .body(body_bytes.clone())
            })
            .await
            .context("provider-native web search request failed")?;
        let payload = response
            .json::<Value>()
            .await
            .context("provider-native web search returned invalid JSON")?;
        let mut parsed = match self.inner.api_provider {
            ProviderKind::Openai
            | ProviderKind::Xai
            | ProviderKind::ModelstudioTokenPlan
            | ProviderKind::Deepseek => parse_responses_search(&payload),
            ProviderKind::Anthropic => parse_anthropic_search(&payload),
            ProviderKind::XiaomiMimo => parse_mimo_search(&payload),
            ProviderKind::Zai => zai::parse(&payload),
            _ => unreachable!("provider checked above"),
        };
        parsed.citations.truncate(usize::from(request.max_results));
        Ok(parsed)
    }

    pub(super) async fn post_json(
        &self,
        url: &str,
        body: &Value,
        headers: &[(HeaderName, HeaderValue)],
    ) -> Result<Value> {
        let body_bytes = serde_json::to_vec(&body)
            .context("failed to serialize provider-native web-search request")?;
        let headers = headers.to_vec();
        let response = self
            .inner
            .send_with_retry(|| {
                let mut request = self
                    .inner
                    .http_client
                    .post(url)
                    .header("Accept", "application/json")
                    .body(body_bytes.clone());
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                request
            })
            .await
            .context("provider-native web search request failed")?;
        response
            .json::<Value>()
            .await
            .context("provider-native web search returned invalid JSON")
    }

    pub(super) async fn get_json(&self, url: &str) -> Result<Value> {
        let response = self
            .inner
            .send_with_retry(|| {
                self.inner
                    .http_client
                    .get(url)
                    .header("Accept", "application/json")
            })
            .await
            .context("provider-native web search request failed")?;
        response
            .json::<Value>()
            .await
            .context("provider-native web search returned invalid JSON")
    }
}

#[derive(Clone, Copy)]
enum ResponsesSearchDialect {
    Openai,
    Xai,
    ModelStudio,
    Deepseek,
}

fn search_prompt(request: &ProviderNativeSearchRequest) -> String {
    format!(
        "Search the web for the following query and answer only from web sources. \
         Use concise prose. Cite as many distinct sources as support the answer, up to {}, \
         each by the exact URL of a page you retrieved; never guess or construct a URL.\n\n{}",
        request.max_results, request.query
    )
}

fn build_responses_search_body(
    model: &str,
    request: &ProviderNativeSearchRequest,
    dialect: ResponsesSearchDialect,
) -> Value {
    let mut tool = json!({ "type": "web_search" });
    if !request.domains.is_empty()
        && matches!(
            dialect,
            ResponsesSearchDialect::Openai | ResponsesSearchDialect::Xai
        )
    {
        tool["filters"] = json!({ "allowed_domains": request.domains });
    }
    let mut body = json!({
        "model": model,
        "input": search_prompt(request),
        "tools": [tool],
    });
    match dialect {
        ResponsesSearchDialect::Openai => {
            body["tool_choice"] = json!("required");
            body["store"] = json!(false);
            body["include"] = json!(["web_search_call.action.sources"]);
        }
        ResponsesSearchDialect::Xai => {
            body["tool_choice"] = json!("required");
        }
        ResponsesSearchDialect::ModelStudio => {
            body["tool_choice"] = json!("required");
        }
        ResponsesSearchDialect::Deepseek => {
            body["tool_choice"] = json!({ "type": "web_search" });
        }
    }
    body
}

fn build_anthropic_search_body(
    model: &str,
    request: &ProviderNativeSearchRequest,
    max_tokens: u32,
) -> Value {
    let mut tool = json!({
        "type": "web_search_20250305",
        "name": "web_search",
        "max_uses": 1,
    });
    if !request.domains.is_empty() {
        tool["allowed_domains"] = json!(request.domains);
    }
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": search_prompt(request) }],
        "tools": [tool],
    })
}

fn build_mimo_search_body(
    model: &str,
    request: &ProviderNativeSearchRequest,
    max_completion_tokens: u32,
) -> Value {
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": search_prompt(request) }],
        "tools": [{
            "type": "web_search",
            "max_keyword": 1,
            "force_search": true,
            "limit": request.max_results,
        }],
        "tool_choice": "auto",
        "max_completion_tokens": max_completion_tokens,
        "stream": false,
        "thinking": { "type": "disabled" },
    })
}

fn anthropic_messages_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

fn parse_responses_search(payload: &Value) -> ProviderNativeSearchResponse {
    let mut answer_parts = Vec::new();
    let mut citations = Vec::new();
    if let Some(output) = payload.get("output").and_then(Value::as_array) {
        for item in output {
            let item_type = item.get("type").and_then(Value::as_str);
            if item_type == Some("web_search_call")
                && let Some(action) = item.get("action")
            {
                if let Some(sources) = action.get("sources").and_then(Value::as_array) {
                    for source in sources {
                        push_citation(&mut citations, citation_from_value(source, None, None));
                    }
                }
                push_citation(&mut citations, citation_from_value(action, None, None));
            }

            if item_type == Some("message")
                && let Some(content) = item.get("content").and_then(Value::as_array)
            {
                for block in content {
                    if matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("output_text" | "text")
                    ) && let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        answer_parts.push(text.trim().to_string());
                    }
                    if let Some(annotations) = block.get("annotations").and_then(Value::as_array) {
                        for annotation in annotations {
                            push_citation(
                                &mut citations,
                                citation_from_value(annotation, None, None),
                            );
                        }
                    }
                }
            }
        }
    }
    if answer_parts.is_empty()
        && let Some(output_text) = payload.get("output_text").and_then(Value::as_str)
        && !output_text.trim().is_empty()
    {
        answer_parts.push(output_text.trim().to_string());
    }
    for answer in &answer_parts {
        for citation in citations_from_text(answer) {
            push_citation(&mut citations, Some(citation));
        }
    }
    if let Some(top_level) = payload.get("citations").and_then(Value::as_array) {
        for citation in top_level {
            let parsed = citation
                .as_str()
                .and_then(|url| citation_from_url(url, None, None, None))
                .or_else(|| citation_from_value(citation, None, None));
            push_citation(&mut citations, parsed);
        }
    }
    // A Responses reply cut at its output limit is `incomplete` with reason
    // `max_output_tokens`; the text it did return is a partial answer.
    let truncated = payload.get("status").and_then(Value::as_str) == Some("incomplete")
        && payload
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            == Some("max_output_tokens");
    ProviderNativeSearchResponse {
        answer: joined_answer(answer_parts),
        citations,
        truncated,
    }
}

fn parse_anthropic_search(payload: &Value) -> ProviderNativeSearchResponse {
    let mut answer_parts = Vec::new();
    let mut citations = Vec::new();
    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("web_search_tool_result") => {
                    if let Some(results) = block.get("content").and_then(Value::as_array) {
                        for result in results {
                            let published = result
                                .get("page_age")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            push_citation(
                                &mut citations,
                                citation_from_value(result, None, published),
                            );
                        }
                    }
                }
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        answer_parts.push(text.trim().to_string());
                    }
                    if let Some(block_citations) = block.get("citations").and_then(Value::as_array)
                    {
                        for citation in block_citations {
                            let snippet = citation
                                .get("cited_text")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            push_citation(
                                &mut citations,
                                citation_from_value(citation, snippet, None),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
    ProviderNativeSearchResponse {
        answer: joined_answer(answer_parts),
        citations,
        truncated: payload.get("stop_reason").and_then(Value::as_str) == Some("max_tokens"),
    }
}

fn parse_mimo_search(payload: &Value) -> ProviderNativeSearchResponse {
    let message = payload.pointer("/choices/0/message");
    let answer = message
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let mut citations = Vec::new();
    if let Some(annotations) = message
        .and_then(|value| value.get("annotations"))
        .and_then(Value::as_array)
        .or_else(|| payload.get("annotations").and_then(Value::as_array))
    {
        for annotation in annotations {
            let Some(url) = annotation.get("url").and_then(Value::as_str) else {
                continue;
            };
            let title = annotation
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string);
            let snippet = annotation
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string);
            let published = annotation
                .get("publish_time")
                .and_then(Value::as_str)
                .map(str::to_string);
            push_citation(
                &mut citations,
                citation_from_url(url, title, snippet, published),
            );
        }
    }
    ProviderNativeSearchResponse {
        answer: joined_answer(answer.into_iter().collect()),
        citations,
        truncated: finish_reason_is_length(payload.pointer("/choices/0")),
    }
}

/// A Chat Completions choice that stopped at its output limit.
fn finish_reason_is_length(choice: Option<&Value>) -> bool {
    choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        == Some("length")
}

fn citation_from_value(
    value: &Value,
    snippet: Option<String>,
    published: Option<String>,
) -> Option<ProviderNativeCitation> {
    let url = value.get("url").and_then(Value::as_str)?.trim();
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    citation_from_url(url, title, snippet, published)
}

fn citation_from_url(
    url: &str,
    title: Option<String>,
    snippet: Option<String>,
    published: Option<String>,
) -> Option<ProviderNativeCitation> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    Some(ProviderNativeCitation {
        url: url.to_string(),
        title: title.unwrap_or_else(|| fallback_title(url)),
        snippet,
        published,
    })
}

fn push_citation(
    citations: &mut Vec<ProviderNativeCitation>,
    candidate: Option<ProviderNativeCitation>,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if let Some(existing) = citations
        .iter_mut()
        .find(|existing| existing.url == candidate.url)
    {
        if existing.title == fallback_title(&existing.url)
            && candidate.title != fallback_title(&candidate.url)
        {
            existing.title = candidate.title;
        }
        if existing.snippet.is_none() {
            existing.snippet = candidate.snippet;
        }
        if existing.published.is_none() {
            existing.published = candidate.published;
        }
        return;
    }
    citations.push(candidate);
}

fn citations_from_text(text: &str) -> Vec<ProviderNativeCitation> {
    let mut citations = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let remaining = &text[offset..];
        let relative_start = match (remaining.find("https://"), remaining.find("http://")) {
            (Some(https), Some(http)) => Some(https.min(http)),
            (Some(https), None) => Some(https),
            (None, Some(http)) => Some(http),
            (None, None) => None,
        };
        let Some(relative_start) = relative_start else {
            break;
        };
        let start = offset + relative_start;
        let tail = &text[start..];
        // Balanced parentheses belong to the URL (Wikipedia titles such as
        // `Foo_(bar)` keep their closing paren); an unmatched closer ends it.
        let mut open_parens = 0_usize;
        let end = tail
            .char_indices()
            .find_map(|(index, ch)| match ch {
                '(' => {
                    open_parens += 1;
                    None
                }
                ')' if open_parens > 0 => {
                    open_parens -= 1;
                    None
                }
                _ => (index > 0
                    && (ch.is_whitespace()
                        || matches!(ch, ')' | ']' | '}' | '>' | '"' | '\'' | '`')))
                .then_some(index),
            })
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        push_citation(&mut citations, citation_from_url(url, None, None, None));
        offset = start + end.max(1);
    }
    citations
}

fn fallback_title(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return "Web source".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "Web source".to_string();
    };
    let slug = parsed
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()));
    let Some(slug) = slug else {
        return format!("{host} (home page)");
    };
    let words = slug
        .rsplit_once('.')
        .filter(|(_, extension)| matches!(*extension, "html" | "htm" | "php" | "asp" | "aspx"))
        .map_or(slug, |(stem, _)| stem)
        .split(['-', '_', '+'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    if words.is_empty() {
        return format!("{host} (home page)");
    }
    words.chars().take(120).collect()
}

/// Join the answer parts whole. No per-adapter cap: the provider already
/// bounded its output tokens, and the engine's route budget bounds what
/// reaches the model inline while keeping the rest recoverable (#6508).
fn joined_answer(parts: Vec<String>) -> Option<String> {
    let joined = parts.join("\n\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn citation_without_a_provider_title_is_never_a_bare_domain() {
        let home = citation_from_url("https://www.theverge.com/", None, None, None)
            .expect("a root URL is a valid citation");
        assert_ne!(home.title, "www.theverge.com");
        assert_eq!(home.title, "www.theverge.com (home page)");

        let article = citation_from_url(
            "https://www.theverge.com/2026/10/21/apple-says-x.html",
            None,
            None,
            None,
        )
        .expect("an article URL is a valid citation");
        assert_eq!(article.title, "Apple Says X");
    }

    fn request() -> ProviderNativeSearchRequest {
        ProviderNativeSearchRequest {
            query: "current release".to_string(),
            max_results: 3,
            domains: vec!["example.com".to_string()],
        }
    }

    fn xai_client_with_boundary(
        server: &MockServer,
        isolated: bool,
        unrelated: bool,
    ) -> ProviderNativeSearchClient {
        let config = {
            let __cw_config_init_0_0 = Some("xai".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                xai: ProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some("grok-4.5".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let __cw_config_init_0_2 = isolated;
            let __cw_config_init_0_3 = unrelated;
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value.runtime_chat_isolated = __cw_config_init_0_2;
            __cw_config_init_0_value.runtime_thread_inference_unrelated = __cw_config_init_0_3;
            __cw_config_init_0_value
        };
        ProviderNativeSearchClient::new(CodewhaleClient::new(&config).expect("test xAI client"))
            .expect("xAI native adapter")
    }

    #[test]
    fn responses_payload_requires_search_and_keeps_domains_provider_side() {
        let body =
            build_responses_search_body("gpt-5.6", &request(), ResponsesSearchDialect::Openai);
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(
            body["tools"][0]["filters"]["allowed_domains"][0],
            "example.com"
        );
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["include"][0], "web_search_call.action.sources");
    }

    #[test]
    fn modelstudio_payload_uses_required_harness_search_without_filters() {
        let body = build_responses_search_body(
            "qwen3.8-max",
            &request(),
            ResponsesSearchDialect::ModelStudio,
        );
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert!(body["tools"][0].get("filters").is_none());
        assert_eq!(body["tool_choice"], "required");
        assert!(body.get("include").is_none());
        assert!(body.get("store").is_none());
    }

    #[test]
    fn deepseek_payload_uses_its_responses_search_contract() {
        let body = build_responses_search_body(
            "deepseek-v4-flash",
            &request(),
            ResponsesSearchDialect::Deepseek,
        );
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert!(body["tools"][0].get("filters").is_none());
        assert_eq!(body["tool_choice"]["type"], "web_search");
        assert!(body.get("include").is_none());
        assert!(body.get("store").is_none());
    }

    #[test]
    fn anthropic_payload_uses_basic_direct_search_contract() {
        let body = build_anthropic_search_body("claude-opus-4-8", &request(), 2_048);
        assert_eq!(body["tools"][0]["type"], "web_search_20250305");
        assert_eq!(body["tools"][0]["max_uses"], 1);
        assert_eq!(body["tools"][0]["allowed_domains"][0], "example.com");
        assert_eq!(body["max_tokens"], 2_048);

        let tiny_route = build_anthropic_search_body("claude-opus-4-8", &request(), 128);
        assert_eq!(tiny_route["max_tokens"], 128);
    }

    #[test]
    fn mimo_payload_forces_bounded_web_search_plugin() {
        let body = build_mimo_search_body("mimo-v2.5-pro", &request(), 8_192);
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(body["tools"][0]["force_search"], true);
        assert_eq!(body["tools"][0]["limit"], 3);
        assert_eq!(body["max_completion_tokens"], 8_192);
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    fn anthropic_client(model: &str) -> ProviderNativeSearchClient {
        let config = {
            let __cw_config_init_1_0 = Some("anthropic".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                anthropic: ProviderConfig {
                    api_key: Some("anthropic-test-key".to_string()),
                    base_url: Some("https://api.anthropic.com".to_string()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        ProviderNativeSearchClient::new(CodewhaleClient::new(&config).expect("Anthropic client"))
            .expect("Anthropic native adapter")
    }

    #[test]
    fn answer_output_tokens_is_the_model_ceiling_up_to_the_clamp() {
        // #6508: native answers were requested at 2,048 tokens and silently
        // cut. A catalogued model now gets its route ceiling, up to the clamp.
        let client = anthropic_client("claude-opus-4-8");
        let route_cap = client.inner.effective_max_output_tokens("claude-opus-4-8");
        assert!(route_cap > PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS);
        assert_eq!(
            client.answer_output_tokens(PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS),
            NATIVE_SEARCH_MAX_OUTPUT_TOKENS.min(route_cap)
        );
        // web_search budgets time from the same number the body carries.
        assert_eq!(
            client.requested_answer_output_tokens(),
            Some(NATIVE_SEARCH_MAX_OUTPUT_TOKENS.min(route_cap))
        );

        // A model the catalogue does not describe keeps the old request size
        // rather than risk a rejection.
        let unknown = anthropic_client("claude-unlisted-test-model");
        assert_eq!(
            unknown.answer_output_tokens(PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS),
            PRIOR_NATIVE_SEARCH_OUTPUT_TOKENS
        );
    }

    #[test]
    fn anthropic_stop_reason_max_tokens_sets_truncated() {
        let cut = parse_anthropic_search(&json!({
            "stop_reason": "max_tokens",
            "content": [{ "type": "text", "text": "Partial answer" }]
        }));
        assert!(cut.truncated);
        assert_eq!(cut.answer.as_deref(), Some("Partial answer"));

        let whole = parse_anthropic_search(&json!({
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "Whole answer" }]
        }));
        assert!(!whole.truncated);
    }

    #[test]
    fn responses_incomplete_max_output_tokens_sets_truncated() {
        let payload = |status: &str, reason: &str| {
            json!({
                "status": status,
                "incomplete_details": { "reason": reason },
                "output": [{
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "Partial answer" }]
                }]
            })
        };
        assert!(parse_responses_search(&payload("incomplete", "max_output_tokens")).truncated);
        assert!(!parse_responses_search(&payload("incomplete", "content_filter")).truncated);
        assert!(!parse_responses_search(&payload("completed", "")).truncated);
    }

    #[test]
    fn mimo_finish_reason_length_sets_truncated() {
        let payload = |finish_reason: &str| {
            json!({
                "choices": [{
                    "finish_reason": finish_reason,
                    "message": { "content": "Partial answer" }
                }]
            })
        };
        assert!(parse_mimo_search(&payload("length")).truncated);
        assert!(!parse_mimo_search(&payload("stop")).truncated);
    }

    #[test]
    fn responses_parser_separates_answer_and_deduplicated_citations() {
        let payload = json!({
            "output": [
                {
                    "type": "web_search_call",
                    "action": { "sources": [
                        { "url": "https://example.com/a", "title": "Source A" }
                    ] }
                },
                {
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": "Grounded answer.",
                        "annotations": [
                            { "type": "url_citation", "url": "https://example.com/a", "title": "Source A" },
                            { "type": "url_citation", "url": "https://example.org/b", "title": "Source B" }
                        ]
                    }]
                }
            ]
        });
        let parsed = parse_responses_search(&payload);
        assert_eq!(parsed.answer.as_deref(), Some("Grounded answer."));
        assert_eq!(parsed.citations.len(), 2);
        assert_eq!(parsed.citations[0].title, "Source A");
        assert_eq!(parsed.citations[1].url, "https://example.org/b");
    }

    #[test]
    fn long_native_answers_are_returned_whole() {
        let long = format!("{} end-of-answer", "grounded sentence. ".repeat(600));
        assert!(long.chars().count() > 10_000);
        let payload = json!({
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": long.clone() }]
            }]
        });
        let parsed = parse_responses_search(&payload);
        assert_eq!(parsed.answer.as_deref(), Some(long.trim()));
        assert!(parsed.answer.unwrap().ends_with("end-of-answer"));
    }

    #[test]
    fn responses_parser_keeps_final_message_and_opened_pages_only() {
        let payload = json!({
            "output": [
                {
                    "type": "reasoning",
                    "content": [{
                        "type": "reasoning_text",
                        "text": "private analysis https://reasoning.example/ must stay hidden"
                    }]
                },
                {
                    "type": "web_search_call",
                    "action": {
                        "type": "open_page",
                        "url": "https://github.com/codewhale-hq/CodeWhale"
                    }
                },
                {
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": "Official repository: https://github.com/codewhale-hq/CodeWhale",
                        "annotations": []
                    }]
                }
            ]
        });

        let parsed = parse_responses_search(&payload);

        assert_eq!(
            parsed.answer.as_deref(),
            Some("Official repository: https://github.com/codewhale-hq/CodeWhale")
        );
        assert_eq!(parsed.citations.len(), 1);
        assert_eq!(
            parsed.citations[0].url,
            "https://github.com/codewhale-hq/CodeWhale"
        );
    }

    #[test]
    fn anthropic_parser_keeps_result_metadata_and_cited_text_separate() {
        let payload = json!({
            "content": [
                {
                    "type": "web_search_tool_result",
                    "content": [{
                        "type": "web_search_result",
                        "url": "https://example.com/a",
                        "title": "Source A",
                        "page_age": "July 18, 2026"
                    }]
                },
                {
                    "type": "text",
                    "text": "Grounded answer.",
                    "citations": [{
                        "type": "web_search_result_location",
                        "url": "https://example.com/a",
                        "title": "Source A",
                        "cited_text": "Supporting passage"
                    }]
                }
            ]
        });
        let parsed = parse_anthropic_search(&payload);
        assert_eq!(parsed.answer.as_deref(), Some("Grounded answer."));
        assert_eq!(parsed.citations.len(), 1);
        assert_eq!(
            parsed.citations[0].published.as_deref(),
            Some("July 18, 2026")
        );
        assert_eq!(
            parsed.citations[0].snippet.as_deref(),
            Some("Supporting passage")
        );
    }

    #[test]
    fn mimo_parser_keeps_non_streaming_annotations() {
        let parsed = parse_mimo_search(&json!({
            "choices": [{
                "message": {
                    "content": "Grounded answer.",
                    "annotations": [{
                        "type": "url_citation",
                        "url": "https://example.com/weather",
                        "title": "Weather",
                        "summary": "Forecast",
                        "publish_time": "2026-08-28"
                    }]
                }
            }]
        }));
        assert_eq!(parsed.answer.as_deref(), Some("Grounded answer."));
        assert_eq!(parsed.citations.len(), 1);
        assert_eq!(parsed.citations[0].snippet.as_deref(), Some("Forecast"));
        assert_eq!(parsed.citations[0].published.as_deref(), Some("2026-08-28"));
    }

    #[test]
    fn non_http_citations_are_rejected() {
        let payload = json!({ "citations": ["javascript:alert(1)"] });
        assert!(parse_responses_search(&payload).citations.is_empty());
    }

    #[test]
    fn answer_links_preserve_mixed_scheme_source_order() {
        let citations =
            citations_from_text("First http://legacy.example/a, then https://secure.example/b.");
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].url, "http://legacy.example/a");
        assert_eq!(citations[1].url, "https://secure.example/b");
    }

    #[test]
    fn answer_links_keep_balanced_parenthesis_segments() {
        let citations = citations_from_text(
            "See https://en.wikipedia.org/wiki/Foo_(bar) and https://en.wikipedia.org/wiki/Baz_(qux_(nested)) for details.",
        );
        assert_eq!(citations.len(), 2);
        assert_eq!(
            citations[0].url, "https://en.wikipedia.org/wiki/Foo_(bar)",
            "a balanced closing paren is part of the URL"
        );
        assert_eq!(
            citations[1].url, "https://en.wikipedia.org/wiki/Baz_(qux_(nested))",
            "nested balanced parens stay intact"
        );

        let unbalanced = citations_from_text("Broken https://en.wikipedia.org/wiki/Foo_(bar here.");
        assert_eq!(
            unbalanced[0].url, "https://en.wikipedia.org/wiki/Foo_(bar",
            "an unclosed paren cannot extend past the next whitespace"
        );
    }

    #[tokio::test]
    async fn xai_adapter_reuses_active_authenticated_transport() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer xai-test-key"))
            .and(body_partial_json(json!({
                "model": "grok-4.5",
                "tools": [{
                    "type": "web_search",
                    "filters": { "allowed_domains": ["example.com"] }
                }],
                "tool_choice": "required"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output_text": "Grounded answer.",
                "citations": ["https://example.com/source"]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let config = {
            let __cw_config_init_2_0 = Some("xai".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                xai: ProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some("grok-4.5".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        let inner = CodewhaleClient::new(&config).expect("test xAI client");
        let client = ProviderNativeSearchClient::new(inner).expect("xAI native adapter");
        let cache_identity = client.cache_identity();
        assert!(cache_identity.contains("provider-native://xai/"));
        assert!(cache_identity.ends_with("/grok-4.5"));
        assert!(!cache_identity.contains("xai-test-key"));

        let response = client.search(&request()).await.expect("native search");

        assert_eq!(response.answer.as_deref(), Some("Grounded answer."));
        assert_eq!(response.citations.len(), 1);
        assert_eq!(response.citations[0].url, "https://example.com/source");
    }

    #[tokio::test]
    async fn modelstudio_adapter_uses_token_plan_responses_contract() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer modelstudio-test-key"))
            .and(body_partial_json(json!({
                "model": "qwen3.8-max",
                "tools": [{ "type": "web_search" }],
                "tool_choice": "required"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": [{
                    "type": "web_search_call",
                    "action": {
                        "sources": [{
                            "url": "https://example.com/qwen",
                            "title": "Qwen source"
                        }]
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let config = {
            let __cw_config_init_3_0 = Some("modelstudio-token-plan".to_string());
            let __cw_config_init_3_1 = Some(ProvidersConfig {
                modelstudio_token_plan: ProviderConfig {
                    api_key: Some("modelstudio-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some("qwen3.8-max".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        let inner = CodewhaleClient::new(&config).expect("test ModelStudio client");
        let client = ProviderNativeSearchClient::new(inner).expect("Qwen native adapter");

        let response = client.search(&request()).await.expect("native search");

        assert_eq!(response.citations.len(), 1);
        assert_eq!(response.citations[0].url, "https://example.com/qwen");
    }
    #[tokio::test]
    async fn deepseek_adapter_uses_authenticated_responses_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer deepseek-test-key"))
            .and(body_partial_json(json!({
                "model": "deepseek-v4-flash",
                "tools": [{ "type": "web_search" }],
                "tool_choice": { "type": "web_search" }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output": [
                    {
                        "type": "web_search_call",
                        "action": {
                            "type": "open_page",
                            "url": "https://example.com/deepseek"
                        }
                    },
                    {
                        "type": "message",
                        "content": [{
                            "type": "output_text",
                            "text": "Grounded answer.",
                            "annotations": []
                        }]
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let config = {
            let __cw_config_init_4_0 = Some("deepseek".to_string());
            let __cw_config_init_4_1 = Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    api_key: Some("deepseek-test-key".to_string()),
                    base_url: Some(format!("{}/v1", server.uri())),
                    model: Some("deepseek-v4-flash".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_4_value: Config = Config::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.providers = __cw_config_init_4_1;
            __cw_config_init_4_value
        };
        let inner = CodewhaleClient::new(&config).expect("test DeepSeek client");
        let client = ProviderNativeSearchClient::new(inner).expect("DeepSeek native adapter");

        let response = client.search(&request()).await.expect("native search");

        assert_eq!(response.answer.as_deref(), Some("Grounded answer."));
        assert_eq!(response.citations.len(), 1);
        assert_eq!(response.citations[0].url, "https://example.com/deepseek");
    }

    #[tokio::test]
    async fn native_search_obeys_attached_run_ownership_without_blocking_unrelated_runtime() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output_text": "Grounded answer.",
                "citations": ["https://example.com/source"]
            })))
            .expect(3)
            .mount(&server)
            .await;
        let participant = xai_client_with_boundary(&server, false, false);
        let isolated = xai_client_with_boundary(&server, true, false);
        let unrelated = xai_client_with_boundary(&server, false, true);

        let ownership = crate::client::acquire_runtime_chat_inference_ownership().await;
        let participant_request = request();
        let mut waiting =
            tokio::spawn(async move { participant.search(&participant_request).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut waiting)
                .await
                .is_err(),
            "provider-native inference from the attached run must wait behind Runtime Chat"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            isolated.search(&request()),
        )
        .await
        .expect("isolated relay request must not self-deadlock")
        .expect("isolated native search fixture");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            unrelated.search(&request()),
        )
        .await
        .expect("unrelated Runtime manager stays concurrent")
        .expect("unrelated native search fixture");

        drop(ownership);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("attached participant resumes after relay settlement")
            .expect("participant task")
            .expect("participant native search fixture");
    }
}
