//! Bounded provider-native/configured web search with explicit fallback receipts.
//! Adapters include Firecrawl, Tavily, Bocha, Metaso, SearXNG, Baidu,
//! Volcengine, Sofya, Serply, and DeepSeek; browsing remains a separate `web.run` workflow.
//! `[search]` example:
//!   provider = "firecrawl"  # keyless on Firecrawl Cloud; optional api_key
//!   base_url = `"https://search.example/"`  # DDG-compatible URL or SearXNG instance

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_u64,
};
use async_trait::async_trait;
use codewhale_net::network_policy::{Decision, NetworkPolicyDecider};
use codewhale_runtime::config::{SearchProvider, tavily_env_key, tavily_key_from};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::web::adapter::{self, AdapterFailure, AdapterResult};
use super::web::backend::SearchBackendChain;
use super::web::cache;
use super::web::contract::{
    BackendId, BackendSearch, DEFAULT_SEARCH_RESULTS, DEFAULT_SEARCH_TIMEOUT_MS, DegradedReason,
    HonoredQueryCapabilities, MAX_SEARCH_RESULTS, MAX_SEARCH_TIMEOUT_MS, QueryKnob, Recency,
    SearchQuery, SearchReceipt, SearchResponse, SearchResult,
};
use super::web::scrape::{
    BROWSER_USER_AGENT as USER_AGENT, ScrapedSearchResult, is_duckduckgo_challenge,
    parse_bing_results as scrape_bing_results,
    parse_duckduckgo_results as scrape_duckduckgo_results,
};

const DUCKDUCKGO_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const BING_HOST: &str = "www.bing.com";
const BING_ENDPOINT: &str = "https://www.bing.com/search";
const FIRECRAWL_ENDPOINT: &str = "https://api.firecrawl.dev/v2/search";
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
const BOCHA_ENDPOINT: &str = "https://api.bochaai.com/v1/web-search";
const METASO_ENDPOINT: &str = "https://metaso.cn/api/v1";
const BAIDU_ENDPOINT: &str = "https://qianfan.baidubce.com/v2/ai_search/web_search";
const VOLCENGINE_RESPONSES_ENDPOINT: &str = "https://ark.cn-beijing.volces.com/api/v3/responses";
const SOFYA_ENDPOINT: &str = "https://sofya.co/v1/search";
const SERPLY_ENDPOINT: &str = "https://api.serply.io/v1/search";
const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/v1/tools/search";
const ERROR_BODY_PREVIEW_BYTES: usize = 512;
const PROVIDER_NATIVE_MIN_TIMEOUT_MS: u64 = 45_000;
const KIMI_K3_FORMULA_MIN_TIMEOUT_MS: u64 = 180_000;
/// Time a native-search attempt allows for its search round-trips before the
/// answer is generated (#6508).
const NATIVE_SEARCH_ROUND_TRIP_ALLOWANCE_MS: u64 = 30_000;
/// Conservative generation rate for a native-search answer. The request is
/// non-streaming, so the attempt must outlast the whole generation or the
/// answer is lost to the timeout rather than returned whole.
const NATIVE_SEARCH_ASSUMED_TOKENS_PER_SEC: u64 = 40;
const VOLCENGINE_MIN_TIMEOUT_MS: u64 = 90_000;

/// The recency and locale knobs an adapter forwards to its backend.
///
/// Recency is rounded *up* to the backend's nearest window (day, week, month,
/// year), so `recency = 10` days asks for the last month: results are never
/// cut tighter than requested, but may be older than asked. Locale is a BCP 47
/// style tag (`en`, `en-US`, `de_DE`); each backend takes the part it accepts.
#[derive(Debug, Clone, Copy, Default)]
struct QueryFilters<'a> {
    recency: Option<Recency>,
    locale: Option<&'a str>,
    prepared: Option<&'a PreparedFilters>,
}

impl<'a> QueryFilters<'a> {
    fn of(query: &'a SearchQuery) -> Self {
        Self {
            prepared: None,
            recency: query.recency,
            locale: query
                .locale
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        }
    }

    /// `day` / `week` / `month` / `year`, rounded up from the request.
    fn window(self) -> Option<&'a str> {
        if let Some(prepared) = self.prepared {
            return prepared.window.as_deref();
        }
        self.recency.map(|recency| match recency.days() {
            0..=1 => "day",
            2..=7 => "week",
            8..=31 => "month",
            _ => "year",
        })
    }

    /// Lowercase language subtag (`en` from `en-US`).
    fn language(self) -> Option<String> {
        if let Some(prepared) = self.prepared {
            return prepared.language.clone();
        }
        let language = self.locale?.split(['-', '_']).next()?;
        (matches!(language.len(), 2 | 3) && language.chars().all(|ch| ch.is_ascii_alphabetic()))
            .then(|| language.to_ascii_lowercase())
    }

    /// Two-letter region subtag, uppercase (`US` from `en-US`).
    fn region(self) -> Option<String> {
        if let Some(prepared) = self.prepared {
            return prepared.region.clone();
        }
        let region = self.locale?.split(['-', '_']).nth(1)?;
        (region.len() == 2 && region.chars().all(|ch| ch.is_ascii_alphabetic()))
            .then(|| region.to_ascii_uppercase())
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedFilters {
    kind: String,
    window: Option<String>,
    language: Option<String>,
    region: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestProposal {
    kind: String,
    payload: Value,
    pairs: Vec<(String, String)>,
}

async fn host_request(
    backend: &str,
    query: &str,
    filters: QueryFilters<'_>,
    max_results: usize,
    context: &ToolContext,
    timeout_ms: u64,
) -> AdapterResult<Option<RequestProposal>> {
    if !adapter::search_selected(context) {
        return Ok(None);
    }
    let proposal: RequestProposal = adapter::transform(
        crate::extension_host::StockOperation::WebRequest,
        json!({"backend":backend,"query":query,"max_results":max_results,
            "locale":filters.locale,"filters":{"window":filters.window(),
            "language":filters.language(),"region":filters.region()}}),
        context,
        Duration::from_millis(timeout_ms),
    )
    .await?;
    let valid = proposal.kind == "web_request"
        && match backend {
            "serply" | "searxng" | "bing" | "duckduckgo" => {
                proposal.payload.is_null()
                    && proposal.pairs.first() == Some(&("q".into(), query.into()))
                    && proposal
                        .pairs
                        .iter()
                        .enumerate()
                        .all(|(index, (key, value))| {
                            !proposal.pairs[..index]
                                .iter()
                                .any(|(prior, _)| prior == key)
                                && (key != "q" || value == query)
                                && (key != "num" || value == &max_results.to_string())
                        })
                    && proposal.pairs.iter().all(|(key, _)| {
                        matches!(
                            key.as_str(),
                            "q" | "num" | "hl" | "gl" | "format" | "time_range" | "language"
                        )
                    })
            }
            "volcengine" => {
                proposal.pairs.is_empty()
                    && proposal
                        .payload
                        .as_object()
                        .is_some_and(|value| value.len() == 1 && value.contains_key("input"))
            }
            name => {
                let (query_path, count_path) = match name {
                    "firecrawl" => ("/query", "/limit"),
                    "tavily" | "sofya" => ("/query", "/max_results"),
                    "bocha" => ("/query", "/count"),
                    "metaso" => ("/q", "/size"),
                    "baidu" => ("/messages/0/content", "/resource_type_filter/0/top_k"),
                    _ => ("", ""),
                };
                proposal.pairs.is_empty()
                    && proposal.payload.pointer(query_path).and_then(Value::as_str) == Some(query)
                    && proposal.payload.pointer(count_path).and_then(Value::as_u64)
                        == Some(max_results as u64)
                    && proposal.payload.as_object().is_some_and(|value| {
                        value.keys().all(|key| {
                            matches!(
                                key.as_str(),
                                "query"
                                    | "q"
                                    | "limit"
                                    | "max_results"
                                    | "count"
                                    | "size"
                                    | "sources"
                                    | "tbs"
                                    | "country"
                                    | "search_depth"
                                    | "time_range"
                                    | "freshness"
                                    | "scope"
                                    | "messages"
                                    | "search_source"
                                    | "resource_type_filter"
                            )
                        })
                    })
            }
        };
    if !valid {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned a request outside the captured query contract",
        )));
    }
    Ok(Some(proposal))
}

#[derive(Default)]
pub(crate) struct OpaqueUrls(std::collections::HashMap<String, String>);
impl OpaqueUrls {
    pub(crate) fn capture(&mut self, value: &str) -> String {
        if value.trim().is_empty() {
            return value.into();
        }
        let token = format!("web-url-{}", self.0.len());
        let leading = value.len() - value.trim_start().len();
        let trailing = value.trim_end().len();
        let wire = format!("{}{}{}", &value[..leading], token, &value[trailing..]);
        self.0.insert(token, value.trim().into());
        self.0.insert(wire.clone(), value.into());
        wire
    }
    pub(crate) fn restore(&self, value: &str) -> AdapterResult<String> {
        if value.trim().is_empty() {
            return Ok(value.into());
        }
        self.0.get(value).cloned().ok_or_else(|| {
            AdapterFailure::host(ToolError::execution_failed(
                "Web Host returned an unknown source handle",
            ))
        })
    }
}

fn provider_projection(value: &Value, urls: &mut OpaqueUrls, secret: Option<&str>) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, value)| {
                    // Only fields consumed by a registered adapter cross the process.
                    if !matches!(
                        key.as_str(),
                        "results"
                            | "title"
                            | "name"
                            | "url"
                            | "link"
                            | "content"
                            | "snippet"
                            | "summary"
                            | "description"
                            | "markdown"
                            | "data"
                            | "web"
                            | "webpages"
                            | "webPages"
                            | "value"
                            | "pages"
                            | "references"
                            | "success"
                            | "error"
                            | "message"
                            | "msg"
                            | "code"
                            | "error_code"
                            | "error_msg"
                            | "output"
                            | "type"
                            | "text"
                            | "score"
                    ) {
                        return None;
                    }
                    let projected = if matches!(key.as_str(), "url" | "link") && value.is_string() {
                        Value::String(urls.capture(value.as_str().expect("string")))
                    } else {
                        provider_projection(value, urls, secret)
                    };
                    Some((key.clone(), projected))
                })
                .collect(),
        ),
        Value::Array(array) => Value::Array(
            array
                .iter()
                .map(|value| provider_projection(value, urls, secret))
                .collect(),
        ),
        Value::String(text) => Value::String(match secret.filter(|key| !key.is_empty()) {
            Some(key) => text.replace(key, "[redacted]"),
            None => text.clone(),
        }),
        _ => value.clone(),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderProposal {
    kind: String,
    entries: Vec<WebSearchEntry>,
    error: Option<String>,
}

async fn host_provider(
    backend: &str,
    parsed: &Value,
    max_results: usize,
    secret: Option<&str>,
    context: &ToolContext,
    timeout_ms: u64,
) -> AdapterResult<Option<Vec<WebSearchEntry>>> {
    if !adapter::search_selected(context) {
        return Ok(None);
    }
    let mut urls = OpaqueUrls::default();
    let captured;
    let source = if backend == "volcengine" {
        // Rust's existing JSON/scalar guard is also the privacy boundary: never
        // forward the raw model body or an embedded credential-bearing URL.
        captured = if let Some(error) = parsed.get("error") {
            json!({"error":provider_projection(error,&mut urls,secret)})
        } else if let Some(text) = volcengine_extract_text(parsed) {
            let inner = serde_json::from_str::<Value>(extract_json_block(&text).unwrap_or(&text))
                .ok()
                .map(|value| provider_projection(&value, &mut urls, secret));
            json!({"output":[{"type":"message","content":[{"text":inner.map(|value| value.to_string()).unwrap_or_default()}]}]})
        } else {
            json!({})
        };
        &captured
    } else {
        parsed
    };
    let projection = if backend == "volcengine" {
        source.clone()
    } else {
        provider_projection(source, &mut urls, secret)
    };
    let mut numbers = serde_json::Map::new();
    for key in ["code", "error_code"] {
        if let Some(value) = parsed.get(key) {
            numbers.insert(
                format!("/{key}"),
                json!({"i64":value.as_i64().map(|value| value.to_string())}),
            );
        }
    }
    if backend == "searxng"
        && let Some(values) = parsed.get("results").and_then(Value::as_array)
    {
        for (index, value) in values.iter().enumerate() {
            numbers.insert(
                format!("/results/{index}/score"),
                json!({"score":searxng_score(value).to_string()}),
            );
        }
    }
    let mut proposal: ProviderProposal = adapter::transform(crate::extension_host::StockOperation::WebProvider,
        json!({"backend":backend,"max_results":max_results,"parsed":projection.as_object().map(|_| &projection).unwrap_or(&json!({})),"number_facts":numbers}),
        context,Duration::from_millis(timeout_ms)).await?;
    if proposal.kind != "web_provider" || proposal.entries.len() > max_results {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned an invalid provider proposal",
        )));
    }
    if let Some(error) = proposal.error {
        return Err(ToolError::execution_failed(error).into());
    }
    for entry in &mut proposal.entries {
        entry.url = urls.restore(&entry.url)?;
    }
    Ok(Some(proposal.entries))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntriesProposal {
    kind: String,
    entries: Vec<super::web::contract::CapturedSearchEntry>,
}

pub(crate) async fn normalize_captured_entries(
    entries: Vec<super::web::contract::CapturedSearchEntry>,
    context: &ToolContext,
    budget: Duration,
) -> AdapterResult<Vec<super::web::contract::CapturedSearchEntry>> {
    if !adapter::search_selected(context) {
        return Ok(entries);
    }
    let mut urls = OpaqueUrls::default();
    let mut captured = entries.clone();
    for entry in &mut captured {
        entry.url = urls.capture(&entry.url);
    }
    let proposal: EntriesProposal = adapter::transform(
        crate::extension_host::StockOperation::WebEntries,
        json!({"entries":captured}),
        context,
        budget,
    )
    .await?;
    if proposal.kind != "web_entries" || proposal.entries.len() != entries.len() {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host changed the captured source count",
        )));
    }
    proposal
        .entries
        .into_iter()
        .map(|mut entry| {
            entry.url = urls.restore(&entry.url)?;
            Ok(entry)
        })
        .collect()
}

/// Credential-free endpoint selected for an explicit doctor reachability
/// probe. The ordinary search request builders remain the source of truth for
/// provider endpoints; doctor borrows those endpoints without constructing a
/// query or reading an API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchProbeTarget {
    pub(crate) url: reqwest::Url,
    pub(crate) host: String,
}

/// Safe configuration failures for a search reachability probe.
///
/// These variants deliberately carry no configured URL: userinfo, paths, and
/// query strings may contain credentials and must never be echoed by doctor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchProbeTargetError {
    Missing,
    Unsupported,
    Invalid,
}

/// Resolve the configured provider's transport endpoint for doctor.
///
/// Built-in endpoints keep their known request path. User-configured
/// DuckDuckGo-compatible and SearXNG URLs are reduced to their HTTP(S)
/// authority so the probe cannot transmit userinfo, path/query credentials,
/// or a real search query.
pub(crate) fn search_probe_target(
    provider: SearchProvider,
    base_url: Option<&str>,
) -> Result<SearchProbeTarget, SearchProbeTargetError> {
    let configured_base_url = configured_search_base_url(base_url);
    if configured_base_url.is_some()
        && !matches!(
            provider,
            SearchProvider::DuckDuckGo | SearchProvider::Searxng
        )
    {
        return Err(SearchProbeTargetError::Unsupported);
    }

    let (raw, configured) = match provider {
        SearchProvider::Bing => (BING_ENDPOINT, false),
        SearchProvider::DuckDuckGo => (
            configured_base_url.unwrap_or(DUCKDUCKGO_ENDPOINT),
            configured_base_url.is_some(),
        ),
        SearchProvider::Firecrawl => (FIRECRAWL_ENDPOINT, false),
        SearchProvider::Tavily => (TAVILY_ENDPOINT, false),
        SearchProvider::Bocha => (BOCHA_ENDPOINT, false),
        SearchProvider::Metaso => (METASO_ENDPOINT, false),
        SearchProvider::Searxng => (
            configured_base_url.ok_or(SearchProbeTargetError::Missing)?,
            true,
        ),
        SearchProvider::Baidu => (BAIDU_ENDPOINT, false),
        SearchProvider::Volcengine => (VOLCENGINE_RESPONSES_ENDPOINT, false),
        SearchProvider::Sofya => (SOFYA_ENDPOINT, false),
        SearchProvider::Serply => (SERPLY_ENDPOINT, false),
        SearchProvider::DeepSeek => (DEEPSEEK_ENDPOINT, false),
    };

    let mut url = reqwest::Url::parse(raw).map_err(|_| SearchProbeTargetError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(SearchProbeTargetError::Invalid);
    }

    url.set_fragment(None);
    url.set_query(None);
    if configured {
        url.set_username("")
            .map_err(|_| SearchProbeTargetError::Invalid)?;
        url.set_password(None)
            .map_err(|_| SearchProbeTargetError::Invalid)?;
        url.set_path("/");
    }
    let host = url
        .host_str()
        .ok_or(SearchProbeTargetError::Invalid)?
        .to_string();

    Ok(SearchProbeTarget { url, host })
}

/// Returns `Ok(())` if the policy allows the call, or a `ToolError` otherwise.
/// Falls through silently when no policy is attached (back-compat).
pub(crate) fn check_policy(
    decider: Option<&NetworkPolicyDecider>,
    host: &str,
) -> Result<(), ToolError> {
    let Some(decider) = decider else {
        return Ok(());
    };
    match decider.evaluate(host, "web_search") {
        Decision::Allow => Ok(()),
        Decision::Deny => Err(ToolError::permission_denied(format!(
            "web search to '{host}' blocked by network policy"
        ))),
        Decision::Prompt => Err(ToolError::permission_denied(format!(
            "web search to '{host}' requires approval; \
             re-run after `/network allow {host}` or set network.default = \"allow\" in config"
        ))),
    }
}

// Cached regex for secret redaction in error bodies
static BEARER_TOKEN_RE: OnceLock<Regex> = OnceLock::new();

fn get_bearer_token_re() -> &'static Regex {
    BEARER_TOKEN_RE.get_or_init(|| {
        Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+")
            .expect("bearer token regex pattern is valid")
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSearchEntry {
    title: String,
    url: String,
    snippet: Option<String>,
}

pub struct WebSearchTool;

#[async_trait]
impl ToolSpec for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Search the web and return ranked results with URLs, snippets, session-scoped ref_ids, and an execution receipt. `web.run` opens a result ref_id when the short summary is not enough. When the exact active route reports a documented first-party server-side search tool, it is tried first; otherwise keyless Firecrawl is the default. Configured API backends visibly degrade through DuckDuckGo then Bing when unavailable, and every hop is recorded. Configuration and network-policy errors fail closed. Explicit Bing and private DuckDuckGo-compatible routes do not cross providers. Set `[search] provider = \"firecrawl\" | \"bing\" | \"tavily\" | \"bocha\" | \"metaso\" | \"searxng\" | \"baidu\" | \"volcengine\" | \"sofya\" | \"serply\" | \"deepseek\"` in config.toml. Firecrawl Cloud works keyless with a bounded quota. `fetch_url` retrieves a known canonical URL directly."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query. Compatibility aliases: q, or search_query[0].q."
                },
                "q": {
                    "type": "string",
                    "description": "Search query."
                },
                "search_query": {
                    "type": "array",
                    "description": "Array form for advanced queries: [{\"q\":\"...\", \"max_results\": 5}]",
                    "items": {
                        "type": "object",
                        "properties": {
                            "q": { "type": "string" },
                            "query": { "type": "string" },
                            "max_results": { "type": "integer" },
                            "recency": {
                                "oneOf": [
                                    { "type": "string", "enum": ["day", "week", "month", "year"] },
                                    { "type": "integer", "minimum": 1, "maximum": 3650 }
                                ]
                            },
                            "domains": { "type": "array", "items": { "type": "string" } },
                            "locale": { "type": "string" }
                        }
                    }
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 10)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Configured/local search timeout in milliseconds (default: 15000, max: 60000). Model-backed provider-native search has a separate bounded minimum before fallback."
                },
                "recency": {
                    "oneOf": [
                        { "type": "string", "enum": ["day", "week", "month", "year"] },
                        { "type": "integer", "minimum": 1, "maximum": 3650 }
                    ],
                    "description": "Requested freshness window. Unsupported backends report it as degraded instead of silently ignoring it."
                },
                "domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict returned results to these domains. Backends without native support report post-filtering."
                },
                "locale": {
                    "type": "string",
                    "description": "Requested result locale as a BCP 47-style tag such as zh-CN or ja-JP; malformed values are ignored. The keyless Bing scrape honors it via its mkt/setlang market parameters; the keyless DuckDuckGo scrape maps a fixed region list and reports malformed or unmapped regions as ignored; unsupported backends report it as degraded."
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Read-only HTTP can still disclose local data through a URL or query.
        // Host allowlisting controls reachability, not approval of this payload.
        ApprovalRequirement::Required
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = search_query_from_input(&input)?;
        let timeout_ms = optional_u64(&input, "timeout_ms", DEFAULT_SEARCH_TIMEOUT_MS)?
            .min(MAX_SEARCH_TIMEOUT_MS);
        let response = execute_search(query, timeout_ms, context).await?;
        ToolResult::json(&response).map_err(|error| ToolError::execution_failed(error.to_string()))
    }
}

impl WebSearchTool {
    async fn run_firecrawl_search(
        &self,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<(Vec<WebSearchEntry>, String)> {
        let env_key = std::env::var("FIRECRAWL_API_KEY").ok();
        self.run_firecrawl_search_at_for_context(
            FIRECRAWL_ENDPOINT,
            query,
            filters,
            max_results,
            timeout_ms,
            context.search_api_key.as_deref().or(env_key.as_deref()),
            context,
        )
        .await
    }

    #[cfg(test)]
    async fn run_firecrawl_search_at(
        &self,
        endpoint: &str,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        api_key: Option<&str>,
    ) -> Result<(Vec<WebSearchEntry>, String), ToolError> {
        let context = ToolContext::new(
            std::env::current_dir()
                .map_err(|error| ToolError::execution_failed(error.to_string()))?,
        );
        self.run_firecrawl_search_at_for_context(
            endpoint,
            query,
            filters,
            max_results,
            timeout_ms,
            api_key,
            &context,
        )
        .await
        .map_err(Into::into)
    }

    async fn run_firecrawl_search_at_for_context(
        &self,
        endpoint: &str,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        api_key: Option<&str>,
        context: &ToolContext,
    ) -> AdapterResult<(Vec<WebSearchEntry>, String)> {
        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;
        let api_key = api_key.map(str::trim).filter(|key| !key.is_empty());
        let payload = if let Some(plan) = host_request(
            "firecrawl",
            query,
            filters,
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            {
                let mut payload =
                    json!({"query":query,"limit":max_results,"sources":[{"type":"web"}]});
                if let Some(window) = filters.window() {
                    payload["tbs"] = json!(format!("qdr:{}", &window[..1]));
                }
                if let Some(region) = filters.region() {
                    payload["country"] = json!(region);
                }
                payload
            }
        };
        let mut request = client.post(endpoint).json(&payload);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| {
            ToolError::execution_failed(format!("Firecrawl search request failed: {e}"))
        })?;
        let status = response.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(response, context).await?
        } else {
            response.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Firecrawl response: {e}"))
            })?
        };
        if !status.is_success() {
            let message = match status.as_u16() {
                401 | 403 if api_key.is_none() => "Firecrawl rejected keyless search; set `[search] api_key` or FIRECRAWL_API_KEY".to_string(),
                401 | 403 => "Firecrawl authentication was rejected; check `[search] api_key` or FIRECRAWL_API_KEY".to_string(),
                429 if api_key.is_none() => "Firecrawl keyless quota is exhausted; retry later or set `[search] api_key` / FIRECRAWL_API_KEY".to_string(),
                429 => "Firecrawl quota is exhausted; retry later or check the configured account limits".to_string(),
                code => format!("Firecrawl search failed: HTTP {code} — {}", truncate_error_body(&body)),
            };
            return Err((ToolError::execution_failed(message)).into());
        }
        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Firecrawl response: {e}"))
        })?;
        if let Some(entries) = host_provider(
            "firecrawl",
            &parsed,
            max_results,
            api_key,
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok((
                entries,
                format!(
                    "Firecrawl {}",
                    if api_key.is_some() {
                        "authenticated"
                    } else {
                        "keyless"
                    }
                ),
            ));
        }

        if parsed.get("success").and_then(Value::as_bool) == Some(false) {
            let detail = first_non_empty_string(&parsed, &["error", "message"])
                .unwrap_or_else(|| "unknown API error".to_string());
            return Err((ToolError::execution_failed(format!(
                "Firecrawl search failed: {detail}"
            )))
            .into());
        }
        let mode = if api_key.is_some() {
            "authenticated"
        } else {
            "keyless"
        };
        Ok((
            parse_firecrawl_results(&parsed, max_results),
            format!("Firecrawl {mode}"),
        ))
    }

    /// Search a configured SearXNG JSON API; no public instance is assumed.
    async fn run_searxng_search(
        &self,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<(Vec<WebSearchEntry>, String)> {
        let (url, host) = if let Some(plan) =
            host_request("searxng", query, filters, max_results, context, timeout_ms).await?
        {
            // The configured origin/path/query are private Core authority.
            let (base, host) = searxng_search_base(context.search_base_url.as_deref())?;
            let mut url = base;
            for (key, value) in plan.pairs {
                url.query_pairs_mut().append_pair(&key, &value);
            }
            (url.to_string(), host)
        } else {
            searxng_search_url(context.search_base_url.as_deref(), query, filters)?
        };
        check_policy(context.network_policy.as_ref(), &host)?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let resp = client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("SearXNG search request to {host} failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!(
                    "Failed to read SearXNG response from {host}: {e}"
                ))
            })?
        };

        if !status.is_success() {
            let truncated = truncate_error_body(&body);
            let msg = match status.as_u16() {
                403 => format!(
                    "SearXNG search failed: HTTP 403 from {host}. Check that JSON output is enabled and this instance permits API access. {truncated}"
                ),
                429 => format!(
                    "SearXNG search failed: HTTP 429 from {host}. The configured instance is rate-limiting requests; use a trusted/self-hosted instance or retry later. {truncated}"
                ),
                code => format!("SearXNG search failed: HTTP {code} from {host}. {truncated}"),
            };
            return Err((ToolError::execution_failed(msg)).into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!(
                "Failed to parse SearXNG JSON response from {host}: {e}. Ensure the instance supports format=json and JSON output is enabled."
            ))
        })?;

        if let Some(entries) =
            host_provider("searxng", &parsed, max_results, None, context, timeout_ms).await?
        {
            return Ok((entries, host));
        }

        Ok((parse_searxng_results(&parsed, max_results), host))
    }

    /// Search via Tavily AI Search API (<https://tavily.com>).
    async fn run_tavily_search(
        &self,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let api_key = tavily_key_from(context.search_api_key.as_deref())
            .or_else(|| {
                // An explicit `provider = "tavily"` still accepts any
                // non-empty generic key, so a non-`tvly-` pin keeps working.
                // Reaching this hop at all means Tavily was the resolved
                // provider (pinned, or selected by a `tvly-` signal), so the
                // generic fallback is never a Firecrawl/sentinel key.
                context
                    .search_api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Tavily search requires an API key. Set `[search] api_key = \"tvly-...\"` in config.toml or the `TAVILY_API_KEY` env var.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let mut payload = if let Some(plan) =
            host_request("tavily", query, filters, max_results, context, timeout_ms).await?
        {
            plan.payload
        } else {
            tavily_search_payload(&api_key, query, filters, max_results)
        };
        // The credential never enters a transform snapshot or its grant.
        payload["api_key"] = json!(api_key);

        let resp = client
            .post(TAVILY_ENDPOINT)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Tavily search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Tavily response: {e}"))
            })?
        };

        if !status.is_success() {
            let truncated = truncate_error_body(&body);
            return Err((ToolError::execution_failed(format!(
                "Tavily search failed: HTTP {} — {truncated}",
                status.as_u16()
            )))
            .into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Tavily response: {e}"))
        })?;

        if let Some(entries) = host_provider(
            "tavily",
            &parsed,
            max_results,
            Some(&api_key),
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok(entries);
        }

        Ok(parse_tavily_results(&parsed, max_results))
    }

    /// Search Sofya; it returns extracted content and accepts `SOFYA_API_KEY`.
    async fn run_sofya_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let env_key = std::env::var("SOFYA_API_KEY").ok();
        let api_key = context
            .search_api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Sofya search requires an API key. Set `[search] api_key = \"ay_live_...\"` in config.toml or the SOFYA_API_KEY env var.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let payload = if let Some(plan) = host_request(
            "sofya",
            query,
            QueryFilters::default(),
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            json!({
                "query": query,
                "max_results": max_results,
            })
        };

        let resp = client
            .post(SOFYA_ENDPOINT)
            .header("Content-Type", "application/json")
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Sofya search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Sofya response: {e}"))
            })?
        };

        if !status.is_success() {
            let truncated = truncate_error_body(&body);
            return Err((ToolError::execution_failed(format!(
                "Sofya search failed: HTTP {} — {truncated}",
                status.as_u16()
            )))
            .into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Sofya response: {e}"))
        })?;

        if let Some(entries) = host_provider(
            "sofya",
            &parsed,
            max_results,
            Some(api_key),
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok(entries);
        }

        Ok(parse_sofya_results(&parsed, max_results))
    }

    /// Search Serply (<https://serply.io>); it returns Google organic results and
    /// accepts `SERPLY_API_KEY`.
    async fn run_serply_search(
        &self,
        query: &str,
        filters: QueryFilters<'_>,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let env_key = std::env::var("SERPLY_API_KEY").ok();
        let api_key = context
            .search_api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                ToolError::invalid_input(
                    "Serply search requires an API key. Set `[search] api_key` in config.toml or the SERPLY_API_KEY env var.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let url = if let Some(plan) =
            host_request("serply", query, filters, max_results, context, timeout_ms).await?
        {
            let mut url = reqwest::Url::parse(SERPLY_ENDPOINT)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            for (key, value) in plan.pairs {
                url.query_pairs_mut().append_pair(&key, &value);
            }
            url
        } else {
            serply_search_url(query, filters, max_results)?
        };
        let resp = client
            .get(url)
            .header("X-Api-Key", api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Serply search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Serply response: {e}"))
            })?
        };

        if !status.is_success() {
            let truncated = truncate_error_body(&body);
            return Err((ToolError::execution_failed(format!(
                "Serply search failed: HTTP {}: {truncated}",
                status.as_u16()
            )))
            .into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Serply response: {e}"))
        })?;

        if let Some(entries) = host_provider(
            "serply",
            &parsed,
            max_results,
            Some(api_key),
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok(entries);
        }

        Ok(parse_serply_results(&parsed, max_results))
    }

    async fn run_deepseek_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<SearchResult>> {
        let api_key = deepseek_search_key(context).ok_or_else(|| {
            ToolError::invalid_input(
                "DeepSeek search requires an API key. Set `[search] api_key` in config.toml or the DEEPSEEK_API_KEY env var.",
            )
        })?;
        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;
        let count = max_results.clamp(1, 50);
        let payload = json!({
            "query": query.chars().take(128).collect::<String>(),
            "count": count,
        });
        let resp = client
            .post(DEEPSEEK_ENDPOINT)
            .header("Content-Type", "application/json")
            .bearer_auth(&api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("DeepSeek search request failed: {e}"))
            })?;
        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read DeepSeek response: {e}"))
            })?
        };
        if !status.is_success() {
            let message = if status.as_u16() == 402 {
                "DeepSeek search failed: HTTP 402 — top up your DeepSeek account to continue."
                    .to_string()
            } else {
                let truncated = truncate_error_body(&body.replace(&api_key, "[REDACTED]"));
                format!(
                    "DeepSeek search failed: HTTP {}: {truncated}",
                    status.as_u16()
                )
            };
            return Err(ToolError::execution_failed(message).into());
        }
        let parsed: Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse DeepSeek response: {e}"))
        })?;
        Ok(parse_deepseek_results(&parsed, count))
    }

    /// Search via Bocha AI Search API (<https://bochaai.com>).
    async fn run_bocha_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let api_key = context
            .search_api_key
            .as_deref()
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Bocha search requires an API key. Set `[search] api_key = \"sk-...\"` in config.toml.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let payload = if let Some(plan) = host_request(
            "bocha",
            query,
            QueryFilters::default(),
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            json!({
                "query": query,
                "freshness": "noLimit",
                "count": max_results,
            })
        };

        let resp = client
            .post(BOCHA_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Bocha search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Bocha response: {e}"))
            })?
        };

        if !status.is_success() {
            let truncated = truncate_error_body(&body);
            return Err((ToolError::execution_failed(format!(
                "Bocha search failed: HTTP {} — {truncated}",
                status.as_u16()
            )))
            .into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Bocha response: {e}"))
        })?;

        if let Some(entries) = host_provider(
            "bocha",
            &parsed,
            max_results,
            Some(api_key),
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok(entries);
        }

        if let Some(error) = bocha_error_message(&parsed) {
            return Err((ToolError::execution_failed(error)).into());
        }

        Ok(parse_bocha_results(&parsed, max_results))
    }

    /// Search via Metaso AI Search API (<https://metaso.cn>). Falls back to
    /// `METASO_API_KEY` when no config key is set.
    async fn run_metaso_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let env_key = std::env::var("METASO_API_KEY").ok();
        let api_key = context
            .search_api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Metaso search requires an API key. Set `METASO_API_KEY` or `[search] api_key` in config.toml.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let size = max_results.clamp(1, 100);
        let payload = if let Some(plan) = host_request(
            "metaso",
            query,
            QueryFilters::default(),
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            json!({
                "q": query,
                "scope": "webpage",
                "size": size,
            })
        };

        let resp = client
            .post(format!("{METASO_ENDPOINT}/search"))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Metaso search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Metaso response: {e}"))
            })?
        };

        if !status.is_success() {
            let msg = match status.as_u16() {
                401 | 403 => "Metaso API key rejected — check METASO_API_KEY or set `[search] api_key` in config.toml, or get one at https://metaso.cn/search-api/playground".to_string(),
                429 => "Metaso rate-limited — wait and retry, or get your own API key at https://metaso.cn/search-api/playground".to_string(),
                _ => {
                    let truncated = truncate_error_body(&body);
                    format!("Metaso server error (HTTP {status}) — {truncated}")
                }
            };
            return Err((ToolError::execution_failed(msg)).into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Metaso response: {e}"))
        })?;

        if let Some(entries) =
            host_provider("metaso", &parsed, size, Some(api_key), context, timeout_ms).await?
        {
            return Ok(entries);
        }

        // Check business-logic error codes in the response body.
        if let Some(code) = parsed.get("code").and_then(|v| v.as_i64())
            && code != 0
        {
            let msg = parsed
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err((ToolError::execution_failed(match code {
                3003 => "Metaso: daily search limit reached — set METASO_API_KEY or get one at https://metaso.cn/search-api/playground".to_string(),
                2005 => "Metaso API key rejected — check METASO_API_KEY or set `[search] api_key` in config.toml".to_string(),
                _ => format!("Metaso API error (code {code}: {msg})"),
            })).into());
        }

        Ok(parse_metaso_results(&parsed, size))
    }

    /// Search via Baidu AI Search API (<https://qianfan.baidubce.com>).
    async fn run_baidu_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let env_key = std::env::var("BAIDU_SEARCH_API_KEY").ok();
        let api_key = context
            .search_api_key
            .as_deref()
            .or(env_key.as_deref())
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Baidu search requires an API key. Set `BAIDU_SEARCH_API_KEY` or `[search] api_key` in config.toml.",
                )
            })?;

        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let payload = if let Some(plan) = host_request(
            "baidu",
            query,
            QueryFilters::default(),
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            baidu_search_payload(query, max_results)
        };

        // Baidu's AI Search endpoint accepts conversational messages rather
        // than an index-only query. Treat the entire request/response decode
        // as model-backed for attached-run ownership; a false negative here
        // could overlap Runtime Chat, while the conservative read lease only
        // serializes work that already belongs to the same interactive run.
        let _inference = acquire_model_backed_search_inference_participant().await;

        let resp = client
            .post(BAIDU_ENDPOINT)
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Baidu search request failed: {e}"))
            })?;

        let status = resp.status();
        let body = if adapter::search_selected(context) {
            adapter::read_response(resp, context).await?
        } else {
            resp.text().await.map_err(|e| {
                ToolError::execution_failed(format!("Failed to read Baidu response: {e}"))
            })?
        };

        if !status.is_success() {
            let msg = match status.as_u16() {
                401 | 403 => "Baidu search API key rejected — check BAIDU_SEARCH_API_KEY or `[search] api_key` in config.toml".to_string(),
                429 => "Baidu search rate-limited — wait and retry, or check your Baidu AI Search quota".to_string(),
                _ => {
                    let truncated = truncate_error_body(&body);
                    format!("Baidu search failed: HTTP {} — {truncated}", status.as_u16())
                }
            };
            return Err((ToolError::execution_failed(msg)).into());
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ToolError::execution_failed(format!("Failed to parse Baidu response: {e}"))
        })?;

        if let Some(entries) = host_provider(
            "baidu",
            &parsed,
            max_results,
            Some(api_key),
            context,
            timeout_ms,
        )
        .await?
        {
            return Ok(entries);
        }

        if let Some(error) = baidu_error_message(&parsed) {
            return Err((ToolError::execution_failed(error)).into());
        }

        Ok(parse_baidu_results(&parsed, max_results))
    }

    /// Search via Volcengine Ark; it needs a 90s floor and retries transport failures.
    async fn run_volcengine_search(
        &self,
        query: &str,
        max_results: usize,
        timeout_ms: u64,
        context: &ToolContext,
    ) -> AdapterResult<Vec<WebSearchEntry>> {
        let volc_key = std::env::var("VOLCENGINE_API_KEY").ok();
        let volc_ark_key = std::env::var("VOLCENGINE_ARK_API_KEY").ok();
        let ark_key = std::env::var("ARK_API_KEY").ok();
        let api_key = context
            .search_api_key
            .as_deref()
            .or(volc_key.as_deref())
            .or(volc_ark_key.as_deref())
            .or(ark_key.as_deref())
            .ok_or_else(|| {
                ToolError::execution_failed(
                    "Volcengine search requires an API key. Set `[search] api_key`, \
                     or VOLCENGINE_API_KEY / VOLCENGINE_ARK_API_KEY / ARK_API_KEY env var.",
                )
            })?;

        let effective_timeout = timeout_ms.max(90_000);

        let client = codewhale_release::tls::reqwest_client_builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_millis(effective_timeout))
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Some(Duration::from_secs(15)))
            .http2_keep_alive_timeout(Duration::from_secs(20))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to build HTTP client: {e}"))
            })?;

        let mut payload = if let Some(plan) = host_request(
            "volcengine",
            query,
            QueryFilters::default(),
            max_results,
            context,
            timeout_ms,
        )
        .await?
        {
            plan.payload
        } else {
            volcengine_search_payload(query, max_results)
        };
        // The fixed Core route/model/tool selection is never Host authority.
        payload["model"] = json!("doubao-seed-2-0-lite-260428");
        payload["stream"] = json!(false);
        payload["tools"] = json!([{"type":"web_search"}]);

        // Unlike the ordinary index-search backends, Volcengine's Responses
        // endpoint runs a named model and returns model-generated text. Keep
        // that provider lifecycle inside the attached CWC run's shared read
        // ownership through retries and response decoding, so an isolated
        // Runtime Chat turn cannot be projected alongside it.
        let _inference = acquire_model_backed_search_inference_participant().await;

        let mut last_err: Option<ToolError> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(1000 * (1 << (attempt - 1)))).await;
            }

            match client
                .post(VOLCENGINE_RESPONSES_ENDPOINT)
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let body = if adapter::search_selected(context) {
                        adapter::read_response(resp, context).await?
                    } else {
                        resp.text().await.map_err(|e| {
                            ToolError::execution_failed(format!(
                                "Failed to read Volcengine response: {e}"
                            ))
                        })?
                    };

                    if !status.is_success() {
                        let msg = match status.as_u16() {
                            401 | 403 => "Volcengine API key rejected — check `[search] api_key` in config.toml or VOLCENGINE_API_KEY / VOLCENGINE_ARK_API_KEY / ARK_API_KEY".to_string(),
                            429 => "Volcengine API rate-limited — wait and retry, or check your quota".to_string(),
                            _ => {
                                let truncated = truncate_error_body(&body);
                                format!("Volcengine search failed: HTTP {} — {truncated}", status.as_u16())
                            }
                        };
                        return Err((ToolError::execution_failed(msg)).into());
                    }

                    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
                        ToolError::execution_failed(format!(
                            "Failed to parse Volcengine response: {e}"
                        ))
                    })?;

                    if let Some(entries) = host_provider(
                        "volcengine",
                        &parsed,
                        max_results,
                        Some(api_key),
                        context,
                        timeout_ms,
                    )
                    .await?
                    {
                        return Ok(entries);
                    }

                    if let Some(error) = volcengine_error_message(&parsed) {
                        return Err((ToolError::execution_failed(error)).into());
                    }

                    let response_text = volcengine_extract_text(&parsed).ok_or_else(|| {
                        ToolError::execution_failed("Volcengine response contains no output text")
                    })?;

                    return Ok(parse_volcengine_results(&response_text, max_results));
                }
                Err(e) => {
                    let is_transient = e.is_timeout() || e.is_connect();
                    if !is_transient || attempt == 2 {
                        return Err((ToolError::execution_failed(format!(
                            "Volcengine search request failed: {e}"
                        )))
                        .into());
                    }
                    last_err = Some(ToolError::execution_failed(format!(
                        "Volcengine search request failed (attempt {}/3): {e}",
                        attempt + 1
                    )));
                }
            }
        }

        // Unreachable — the final iteration always returns above.
        Err(last_err
            .unwrap_or_else(|| {
                ToolError::execution_failed("Volcengine search: unexpected retry exit")
            })
            .into())
    }
}

pub(crate) async fn execute_search(
    query: SearchQuery,
    timeout_ms: u64,
    context: &ToolContext,
) -> Result<SearchResponse, ToolError> {
    if configured_search_base_url(context.search_base_url.as_deref()).is_some()
        && !matches!(
            context.search_provider,
            SearchProvider::DuckDuckGo | SearchProvider::Searxng
        )
    {
        return Err(ToolError::invalid_input(format!(
            "[search].base_url is only supported with provider = \"duckduckgo\" or \"searxng\"; current provider is \"{}\"",
            context.search_provider.as_str()
        )));
    }

    let chain = SearchBackendChain::from_context(context);
    let initial_backend = chain.initial_backend();
    if initial_backend != BackendId::ProviderNative {
        debug_assert_eq!(initial_backend.as_str(), context.search_provider.as_str());
        preflight_search_provider(context)?;
    }
    let cache_scope = if initial_backend == BackendId::ProviderNative {
        context
            .provider_native_search
            .as_ref()
            .map(crate::client::ProviderNativeSearchClient::cache_identity)
    } else {
        normalized_search_base_url(context.search_base_url.as_deref())
    };

    if let Some(mut cached) = cache::get_search(
        &context.state_namespace,
        initial_backend,
        cache_scope.as_deref(),
        &query,
    ) {
        validate_cached_search_policy(&cached, context)?;
        register_search_citations(&mut cached, context);
        cached.receipt.cache_hit = true;
        cached.receipt.latency_ms = 0;
        return Ok(cached);
    }

    let started = Instant::now();
    let requested_timeout = Duration::from_millis(timeout_ms.max(1));
    let provider_native_timeout_floor = context
        .provider_native_search
        .as_ref()
        .and_then(provider_native_timeout_floor);
    let (total_timeout, first_attempt_budget, fallback_budget_after_first) = search_timeout_budgets(
        initial_backend,
        requested_timeout,
        provider_native_timeout_floor,
    );
    let deadline = started + total_timeout;
    let chained = chain
        .search(
            &query,
            deadline,
            first_attempt_budget,
            fallback_budget_after_first,
        )
        .await?;
    let mut response = finalize_search_response_for_context(
        query.clone(),
        chained.capabilities,
        chained.raw,
        started,
        context,
        deadline.saturating_duration_since(Instant::now()),
    )
    .await?;
    register_search_citations(&mut response, context);
    cache::insert_search(
        &context.state_namespace,
        initial_backend,
        cache_scope.as_deref(),
        &query,
        response.clone(),
    );
    Ok(response)
}

fn search_timeout_budgets(
    initial_backend: BackendId,
    requested_timeout: Duration,
    provider_native_timeout_floor: Option<Duration>,
) -> (Duration, Option<Duration>, Option<Duration>) {
    match initial_backend {
        BackendId::Volcengine => {
            let provider_budget = Duration::from_millis(VOLCENGINE_MIN_TIMEOUT_MS);
            (
                provider_budget + requested_timeout,
                Some(provider_budget),
                None,
            )
        }
        BackendId::ProviderNative => {
            // Provider-native search performs a model-backed request. Give it
            // a dedicated minimum without donating unused time to the
            // configured/local fallback selected by the caller.
            let provider_budget = requested_timeout
                .max(Duration::from_millis(PROVIDER_NATIVE_MIN_TIMEOUT_MS))
                .max(provider_native_timeout_floor.unwrap_or_default());
            (
                provider_budget.saturating_add(requested_timeout),
                Some(provider_budget),
                Some(requested_timeout),
            )
        }
        _ => (requested_timeout, None, None),
    }
}

fn provider_native_timeout_floor(
    client: &crate::client::ProviderNativeSearchClient,
) -> Option<Duration> {
    let k3_formula = codewhale_runtime::config::is_exact_direct_moonshot_k3_route(
        client.provider(),
        client.base_url(),
        client.model(),
    )
    .then_some(Duration::from_millis(KIMI_K3_FORMULA_MIN_TIMEOUT_MS));
    let answer = client
        .requested_answer_output_tokens()
        .map(native_answer_time_budget);
    k3_formula.max(answer)
}

/// Time a native-search attempt needs to return an answer of `output_tokens`
/// whole: the search round-trips plus generation at a conservative rate
/// (#6508). Without it, raising the requested answer length only moved the
/// cut from the provider's token limit to this tool's timeout.
fn native_answer_time_budget(output_tokens: u32) -> Duration {
    let generation_ms =
        u64::from(output_tokens).saturating_mul(1_000) / NATIVE_SEARCH_ASSUMED_TOKENS_PER_SEC;
    Duration::from_millis(NATIVE_SEARCH_ROUND_TRIP_ALLOWANCE_MS.saturating_add(generation_ms))
}

fn register_search_citations(response: &mut SearchResponse, context: &ToolContext) {
    let mut seen = std::collections::HashSet::new();
    response.results.retain_mut(|result| {
        let Some(citation) = super::web::citations::register(
            &context.state_namespace,
            &result.url,
            Some(&result.title),
        ) else {
            return false;
        };
        result.ref_id = citation.ref_id;
        result.url = citation.url;
        seen.insert(result.ref_id.clone())
    });
    if response.count != response.results.len() {
        rerank(&mut response.results);
        response.count = response.results.len();
        response.message = if response.count == 0 {
            "No usable web citations found".to_string()
        } else {
            format!("Found {} result(s)", response.count)
        };
        note_provider_native(
            &mut response.message,
            response.receipt.backend,
            response.count,
            unchecked_citations(&response.receipt.degraded),
            response.receipt.requested.max_results,
            response.unverified_synthesis.is_some(),
        );
    }
}

fn deepseek_search_key(context: &ToolContext) -> Option<String> {
    context
        .search_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("DEEPSEEK_API_KEY")
                .ok()
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
        })
}

/// Reject misconfiguration before cache lookup or network access.
fn preflight_search_provider(context: &ToolContext) -> Result<(), ToolError> {
    let configured_key = context
        .search_api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty());
    let env_key = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let not_configured = |message: &str| Err(ToolError::invalid_input(message));

    match context.search_provider {
        SearchProvider::Tavily if !configured_key && tavily_env_key().is_none() => not_configured(
            "Tavily search is not configured: it requires an API key. Set `[search] api_key = \"tvly-...\"` in config.toml or the `TAVILY_API_KEY` env var.",
        ),
        SearchProvider::Bocha if !configured_key => not_configured(
            "Bocha search is not configured: it requires an API key. Set `[search] api_key = \"sk-...\"` in config.toml.",
        ),
        SearchProvider::Metaso if !configured_key && !env_key("METASO_API_KEY") => not_configured(
            "Metaso search is not configured: it requires an API key. Set `METASO_API_KEY` or `[search] api_key` in config.toml.",
        ),
        SearchProvider::Baidu if !configured_key && !env_key("BAIDU_SEARCH_API_KEY") => {
            not_configured(
                "Baidu search is not configured: it requires an API key. Set `BAIDU_SEARCH_API_KEY` or `[search] api_key` in config.toml.",
            )
        }
        SearchProvider::Volcengine
            if !configured_key
                && !env_key("VOLCENGINE_API_KEY")
                && !env_key("VOLCENGINE_ARK_API_KEY")
                && !env_key("ARK_API_KEY") =>
        {
            not_configured(
                "Volcengine search is not configured: it requires an API key. Set `[search] api_key`, or VOLCENGINE_API_KEY / VOLCENGINE_ARK_API_KEY / ARK_API_KEY env var.",
            )
        }
        SearchProvider::Sofya if !configured_key && !env_key("SOFYA_API_KEY") => not_configured(
            "Sofya search is not configured: it requires an API key. Set `[search] api_key = \"ay_live_...\"` in config.toml or the SOFYA_API_KEY env var.",
        ),
        SearchProvider::Serply if !configured_key && !env_key("SERPLY_API_KEY") => not_configured(
            "Serply search is not configured: it requires an API key. Set `[search] api_key` in config.toml or the SERPLY_API_KEY env var.",
        ),
        SearchProvider::DeepSeek if deepseek_search_key(context).is_none() => not_configured(
            "DeepSeek search is not configured: it requires an API key. Set `[search] api_key` in config.toml or the DEEPSEEK_API_KEY env var.",
        ),
        SearchProvider::Searxng
            if configured_search_base_url(context.search_base_url.as_deref()).is_none() =>
        {
            not_configured(
                "SearXNG search requires [search] base_url = \"https://your-searxng.example\"; no public instance is used by default.",
            )
        }
        _ => Ok(()),
    }
}

fn normalized_search_base_url(base_url: Option<&str>) -> Option<String> {
    let raw = configured_search_base_url(base_url)?;
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return Some(raw.to_string());
    };
    url.set_fragment(None);
    Some(url.to_string())
}

fn validate_cached_search_policy(
    response: &SearchResponse,
    context: &ToolContext,
) -> Result<(), ToolError> {
    let host = response
        .receipt
        .backend_detail
        .as_deref()
        .or_else(|| default_backend_host(response.receipt.backend))
        .ok_or_else(|| {
            ToolError::execution_failed("cached search receipt did not identify its backend host")
        })?;
    check_policy(context.network_policy.as_ref(), host)
}

const fn default_backend_host(backend: BackendId) -> Option<&'static str> {
    match backend {
        BackendId::ProviderNative => None,
        BackendId::Bing => Some(BING_HOST),
        BackendId::DuckDuckGo => Some("html.duckduckgo.com"),
        BackendId::Firecrawl => Some("api.firecrawl.dev"),
        BackendId::Tavily => Some("api.tavily.com"),
        BackendId::Bocha => Some("api.bochaai.com"),
        BackendId::Metaso => Some("metaso.cn"),
        BackendId::Searxng => None,
        BackendId::Baidu => Some("qianfan.baidubce.com"),
        BackendId::Volcengine => Some("ark.cn-beijing.volces.com"),
        BackendId::Sofya => Some("sofya.co"),
        BackendId::Serply => Some("api.serply.io"),
        BackendId::DeepSeek => Some("api.deepseek.com"),
    }
}

async fn finalize_search_response_for_context(
    query: SearchQuery,
    capabilities: super::web::contract::QueryCapabilities,
    mut raw: BackendSearch,
    started: Instant,
    context: &ToolContext,
    budget: Duration,
) -> AdapterResult<SearchResponse> {
    if !adapter::search_selected(context) {
        return Ok(finalize_search_response(query, capabilities, raw, started));
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Proposal {
        kind: String,
        honored: HonoredQueryCapabilities,
        degraded: Vec<DegradedReason>,
        prefix: String,
        suffix: String,
    }
    let initial = raw.degraded.clone();
    apply_domain_constraints(&query, capabilities, &mut raw);
    let domain_extra = raw
        .degraded
        .iter()
        .filter(|reason| !initial.contains(reason))
        .cloned()
        .collect::<Vec<_>>();
    let requested_max = usize::from(query.max_results);
    raw.results.truncate(requested_max);
    rerank(&mut raw.results);
    let synthesis = take_provider_native_synthesis(&mut raw);
    let (unverified, initial): (Vec<_>, Vec<_>) = initial.into_iter().partition(|reason| {
        matches!(
            reason,
            DegradedReason::CitationUnverified { .. } | DegradedReason::CitationUnchecked { .. }
        )
    });
    let unchecked = unverified
        .iter()
        .filter(|reason| matches!(reason, DegradedReason::CitationUnchecked { .. }))
        .count();
    let proposal:Proposal=adapter::transform(crate::extension_host::StockOperation::WebFinalize,
        json!({"requested":{"recency":query.recency.is_some(),"domains":!query.domains.is_empty(),"locale":query.locale.is_some()},
            "capabilities":capabilities,"count":raw.results.len(),"degraded":initial,"domain_extra":domain_extra,"has_note":raw.note.is_some()}),context,budget).await?;
    let known = |reason: &DegradedReason| {
        initial.contains(reason)
            || domain_extra.contains(reason)
            || matches!(
                reason,
                DegradedReason::KnobIgnored {
                    knob: QueryKnob::Recency | QueryKnob::Locale
                }
            )
    };
    if proposal.kind != "web_finalize"
        || proposal.prefix.len() > 128
        || proposal.suffix.len() > 128
        || !proposal.degraded.iter().all(known)
        || !initial
            .iter()
            .chain(&domain_extra)
            .all(|reason| proposal.degraded.contains(reason))
        || proposal.honored.domains != !query.domains.is_empty()
        || (proposal.honored.recency
            && (!query.recency.is_some()
                || !matches!(
                    capabilities.recency,
                    super::web::contract::CapabilityState::Supported
                )))
        || (proposal.honored.locale
            && (!query.locale.is_some()
                || !matches!(
                    capabilities.locale,
                    super::web::contract::CapabilityState::Supported
                )))
    {
        return Err(AdapterFailure::host(ToolError::execution_failed(
            "Web Host returned an inconsistent final receipt",
        )));
    }
    let count = raw.results.len();
    // The host is not told the requested cap, so it can only speak to
    // capability. Core owns the fulfillment half: a backend that returned
    // fewer results than asked did not honor the knob, whatever it claimed.
    let honored = HonoredQueryCapabilities {
        max_results: proposal.honored.max_results && count >= requested_max,
        ..proposal.honored
    };
    let mut message = format!(
        "{}{}{}",
        proposal.prefix,
        raw.note.as_deref().unwrap_or_default(),
        proposal.suffix
    );
    note_provider_native(
        &mut message,
        raw.backend,
        count,
        unchecked,
        query.max_results,
        synthesis.is_some(),
    );
    Ok(SearchResponse {
        query: query.query.clone(),
        source: raw.source,
        count,
        message,
        results: raw.results,
        unverified_synthesis: synthesis,
        receipt: SearchReceipt {
            backend: raw.backend,
            backend_detail: raw.backend_detail,
            requested: query,
            capabilities,
            honored,
            degraded: proposal.degraded.into_iter().chain(unverified).collect(),
            latency_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            cache_hit: false,
        },
    })
}

fn finalize_search_response(
    query: SearchQuery,
    capabilities: super::web::contract::QueryCapabilities,
    mut raw: BackendSearch,
    started: Instant,
) -> SearchResponse {
    let mut honored = HonoredQueryCapabilities {
        max_results: matches!(
            capabilities.max_results,
            super::web::contract::CapabilityState::Supported
        ),
        ..HonoredQueryCapabilities::default()
    };

    let adapter_ignored = |raw: &BackendSearch, knob: QueryKnob| {
        raw.degraded.contains(&DegradedReason::KnobIgnored { knob })
    };
    if query.recency.is_some() && !adapter_ignored(&raw, QueryKnob::Recency) {
        if matches!(
            capabilities.recency,
            super::web::contract::CapabilityState::Supported
        ) {
            honored.recency = true;
        } else {
            raw.degraded.push(DegradedReason::KnobIgnored {
                knob: QueryKnob::Recency,
            });
        }
    }
    if !query.domains.is_empty() {
        // The backend chain applies this before deciding whether an attempt
        // produced usable results. Keep finalization defensive for cached or
        // directly constructed responses; the helper is idempotent.
        apply_domain_constraints(&query, capabilities, &mut raw);
        honored.domains = true;
    }
    if query.locale.is_some() && !adapter_ignored(&raw, QueryKnob::Locale) {
        if matches!(
            capabilities.locale,
            super::web::contract::CapabilityState::Supported
        ) {
            honored.locale = true;
        } else {
            raw.degraded.push(DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            });
        }
    }

    raw.results.truncate(usize::from(query.max_results));
    rerank(&mut raw.results);
    honored.max_results =
        honored.max_results && raw.results.len() >= usize::from(query.max_results);
    let synthesis = take_provider_native_synthesis(&mut raw);
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let receipt = SearchReceipt {
        backend: raw.backend,
        backend_detail: raw.backend_detail,
        requested: query.clone(),
        capabilities,
        honored,
        degraded: raw.degraded,
        latency_ms,
        cache_hit: false,
    };
    let count = raw.results.len();
    let mut message = match (count, raw.note.as_deref()) {
        (0, Some(note)) => format!("No results found. {note}"),
        (0, None) => "No results found".to_string(),
        (_, Some(note)) => format!("Found {count} result(s). {note}"),
        (_, None) => format!("Found {count} result(s)"),
    };
    // The answer sits in `unverified_synthesis`; the message still says when
    // the provider cut it short (#6508).
    if let Some(cut) = receipt
        .degraded
        .iter()
        .find(|reason| matches!(reason, DegradedReason::AnswerCutByProvider))
    {
        message.push_str(&format!("\n[{}]", cut.message()));
    }
    note_provider_native(
        &mut message,
        receipt.backend,
        count,
        unchecked_citations(&receipt.degraded),
        receipt.requested.max_results,
        synthesis.is_some(),
    );

    SearchResponse {
        query: query.query,
        source: raw.source,
        count,
        message,
        results: raw.results,
        unverified_synthesis: synthesis,
        receipt,
    }
}

fn take_provider_native_synthesis(raw: &mut BackendSearch) -> Option<String> {
    if raw.backend == BackendId::ProviderNative {
        raw.note.take()
    } else {
        None
    }
}

fn unchecked_citations(degraded: &[DegradedReason]) -> usize {
    degraded
        .iter()
        .filter(|reason| matches!(reason, DegradedReason::CitationUnchecked { .. }))
        .count()
}

fn note_provider_native(
    message: &mut String,
    backend: BackendId,
    count: usize,
    unchecked: usize,
    requested: u8,
    synthesis: bool,
) {
    if backend != BackendId::ProviderNative {
        return;
    }
    if unchecked == 0 {
        message.push_str(&format!(
            "\n[provider-native search: {count} of {requested} requested source(s) passed link verification and appear in `results`; cite only those, by ref_id"
        ));
    } else {
        let verified = count.saturating_sub(unchecked);
        message.push_str(&format!(
            "\n[provider-native search: {count} of {requested} requested source(s) appear in `results`; {verified} passed link verification and {unchecked} were not checked because the [network] policy needs approval for their hosts; cite only those, by ref_id"
        ));
    }
    if synthesis {
        message.push_str(
            ". `unverified_synthesis` is the provider model's own summary: its claims and links were not checked and may name pages missing from `results`",
        );
    }
    message.push(']');
}

pub(crate) fn apply_domain_constraints(
    query: &SearchQuery,
    capabilities: super::web::contract::QueryCapabilities,
    raw: &mut BackendSearch,
) {
    if query.domains.is_empty() {
        return;
    }

    let before = raw.results.len();
    raw.results
        .retain(|result| domain_matches(&result.url, &query.domains));
    rerank(&mut raw.results);
    let provider_honored = matches!(
        capabilities.domains,
        super::web::contract::CapabilityState::Supported
    );
    let filtered_any = raw.results.len() != before;
    if raw.backend == BackendId::ProviderNative && (!provider_honored || filtered_any) {
        // Post-filtering constrains returned citations but cannot prove that a
        // provider-generated answer did not rely on a removed source.
        raw.note = None;
    }
    let already_recorded = raw.degraded.iter().any(|reason| {
        matches!(
            reason,
            DegradedReason::PostFiltered {
                knob: QueryKnob::Domains
            }
        )
    });
    if (!provider_honored || filtered_any) && !already_recorded {
        raw.degraded.push(DegradedReason::PostFiltered {
            knob: QueryKnob::Domains,
        });
    }
}

pub(crate) async fn run_backend_search(
    provider: SearchProvider,
    query: &SearchQuery,
    deadline: Instant,
    context: &ToolContext,
) -> AdapterResult<BackendSearch> {
    let timeout_ms = u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1),
    )
    .unwrap_or(u64::MAX);
    let max_results = usize::from(query.max_results);
    let prepared = if adapter::search_selected(context) {
        let value: PreparedFilters = adapter::transform(
            crate::extension_host::StockOperation::WebFilters,
            json!({"recency_days":query.recency.map(Recency::days),"locale":query.locale}),
            context,
            Duration::from_millis(timeout_ms),
        )
        .await?;
        if value.kind != "web_filters"
            || value
                .window
                .as_deref()
                .is_some_and(|value| !matches!(value, "day" | "week" | "month" | "year"))
            || value.language.as_deref().is_some_and(|value| {
                !matches!(value.len(), 2 | 3) || !value.chars().all(|c| c.is_ascii_lowercase())
            })
            || value.region.as_deref().is_some_and(|value| {
                value.len() != 2 || !value.chars().all(|c| c.is_ascii_uppercase())
            })
        {
            return Err(AdapterFailure::host(ToolError::execution_failed(
                "Web Host returned malformed filters",
            )));
        }
        Some(value)
    } else {
        None
    };
    let mut filters = QueryFilters::of(query);
    filters.prepared = prepared.as_ref();
    let tool = WebSearchTool;
    let simple = |backend, entries: Vec<WebSearchEntry>| BackendSearch {
        backend,
        source: backend.as_str().to_string(),
        backend_detail: None,
        results: normalize_entries(entries),
        degraded: Vec::new(),
        note: None,
    };

    match provider {
        SearchProvider::Firecrawl => {
            check_policy(context.network_policy.as_ref(), "api.firecrawl.dev")?;
            let (results, note) = tool
                .run_firecrawl_search(&query.query, filters, max_results, timeout_ms, context)
                .await?;
            // Firecrawl targets a country, not a language: a bare `en` has
            // nothing to send and is reported as ignored.
            let degraded = (filters.locale.is_some() && filters.region().is_none())
                .then_some(DegradedReason::KnobIgnored {
                    knob: QueryKnob::Locale,
                })
                .into_iter()
                .collect();
            Ok(BackendSearch {
                backend: BackendId::Firecrawl,
                source: "firecrawl".to_string(),
                backend_detail: Some("api.firecrawl.dev".to_string()),
                results: normalize_entries(results),
                degraded,
                note: Some(note),
            })
        }
        SearchProvider::Tavily => {
            check_policy(context.network_policy.as_ref(), "api.tavily.com")?;
            Ok(simple(
                BackendId::Tavily,
                tool.run_tavily_search(&query.query, filters, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::Bocha => {
            check_policy(context.network_policy.as_ref(), "api.bochaai.com")?;
            Ok(simple(
                BackendId::Bocha,
                tool.run_bocha_search(&query.query, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::Metaso => {
            check_policy(context.network_policy.as_ref(), "metaso.cn")?;
            Ok(simple(
                BackendId::Metaso,
                tool.run_metaso_search(&query.query, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::Searxng => {
            let (entries, host) = tool
                .run_searxng_search(&query.query, filters, max_results, timeout_ms, context)
                .await?;
            let note = format!("Backend: searxng at {host}");
            Ok(BackendSearch {
                backend: BackendId::Searxng,
                source: "searxng".to_string(),
                backend_detail: Some(host),
                results: normalize_entries(entries),
                degraded: Vec::new(),
                note: Some(note),
            })
        }
        SearchProvider::Baidu => {
            check_policy(context.network_policy.as_ref(), "qianfan.baidubce.com")?;
            Ok(simple(
                BackendId::Baidu,
                tool.run_baidu_search(&query.query, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::Volcengine => {
            check_policy(context.network_policy.as_ref(), "ark.cn-beijing.volces.com")?;
            let mut response = simple(
                BackendId::Volcengine,
                tool.run_volcengine_search(&query.query, max_results, timeout_ms, context)
                    .await?,
            );
            response.degraded.push(DegradedReason::SynthesizedResults);
            Ok(response)
        }
        SearchProvider::Sofya => {
            check_policy(context.network_policy.as_ref(), "sofya.co")?;
            Ok(simple(
                BackendId::Sofya,
                tool.run_sofya_search(&query.query, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::Serply => {
            check_policy(context.network_policy.as_ref(), "api.serply.io")?;
            Ok(simple(
                BackendId::Serply,
                tool.run_serply_search(&query.query, filters, max_results, timeout_ms, context)
                    .await?,
            ))
        }
        SearchProvider::DeepSeek => {
            check_policy(context.network_policy.as_ref(), "api.deepseek.com")?;
            Ok(BackendSearch {
                backend: BackendId::DeepSeek,
                source: "deepseek".to_string(),
                backend_detail: None,
                results: tool
                    .run_deepseek_search(&query.query, max_results, timeout_ms, context)
                    .await?,
                degraded: Vec::new(),
                note: None,
            })
        }
        SearchProvider::Bing | SearchProvider::DuckDuckGo => {
            run_scrape_search(provider, query, timeout_ms, context).await
        }
    }
}

/// Share of the search budget a DuckDuckGo request may use when Bing is allowed
/// to answer after it (#6746).
const DUCKDUCKGO_BUDGET_SHARE: f64 = 0.6;

#[derive(Clone, Copy)]
struct ScrapeEndpoints<'a> {
    bing: &'a str,
    allow_bing_fallback: Option<bool>,
}

impl Default for ScrapeEndpoints<'static> {
    fn default() -> Self {
        Self {
            bing: BING_ENDPOINT,
            allow_bing_fallback: None,
        }
    }
}

async fn run_scrape_search(
    provider: SearchProvider,
    query: &SearchQuery,
    timeout_ms: u64,
    context: &ToolContext,
) -> AdapterResult<BackendSearch> {
    let fallback_context = (provider == SearchProvider::DuckDuckGo
        && context.search_provider != SearchProvider::DuckDuckGo)
        .then(|| {
            let mut cloned = context.clone();
            cloned.search_base_url = None;
            cloned
        });
    let context = fallback_context.as_ref().unwrap_or(context);
    run_scrape_search_with_endpoints(
        provider,
        query,
        timeout_ms,
        context,
        ScrapeEndpoints::default(),
    )
    .await
}

async fn run_scrape_search_with_endpoints(
    provider: SearchProvider,
    query: &SearchQuery,
    timeout_ms: u64,
    context: &ToolContext,
    endpoints: ScrapeEndpoints<'_>,
) -> AdapterResult<BackendSearch> {
    let started = Instant::now();
    let budget = Duration::from_millis(timeout_ms);
    let decider = context.network_policy.as_ref();
    let client = codewhale_release::tls::reqwest_client_builder()
        .timeout(budget)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| {
            ToolError::execution_failed(format!("Failed to build HTTP client: {error}"))
        })?;
    let max_results = usize::from(query.max_results);
    let mut degraded = Vec::new();

    if provider == SearchProvider::Bing {
        check_policy(decider, BING_HOST)?;
        if bing_locale_was_ignored(query.locale.as_deref()) {
            degraded.push(DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            });
        }
        let results = run_bing_search(
            &client,
            &query.query,
            query.locale.as_deref(),
            max_results,
            endpoints.bing,
            budget,
            context,
        )
        .await?;
        return Ok(BackendSearch {
            backend: BackendId::Bing,
            source: "bing".to_string(),
            backend_detail: None,
            results: normalize_entries(results),
            degraded,
            note: None,
        });
    }

    let market = scrape_market(query.locale.as_deref(), &query.query);
    if ddg_locale_was_ignored(query.locale.as_deref(), market.as_deref()) {
        degraded.push(DegradedReason::KnobIgnored {
            knob: QueryKnob::Locale,
        });
    }
    let accept_language = scrape_accept_language(market.as_deref());
    let (url, duckduckgo_host) = if let Some(plan) = host_request(
        "duckduckgo",
        &query.query,
        QueryFilters::default(),
        max_results,
        context,
        timeout_ms,
    )
    .await?
    {
        let (mut url, host) = duckduckgo_search_base(context.search_base_url.as_deref())?;
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in plan.pairs {
                pairs.append_pair(&key, &value);
            }
            // The scrape chain hands the host adapter default filters, so the
            // plan never carries the locale; the verified `kl` region pair
            // (see [`ddg_region_param`]) rides along in either request shape.
            if let Some(region) = market.as_deref().and_then(ddg_region_param) {
                pairs.append_pair("kl", &region);
            }
        }
        (url.to_string(), host)
    } else {
        duckduckgo_search_url(
            context.search_base_url.as_deref(),
            &query.query,
            market.as_deref(),
        )?
    };
    let allow_bing_fallback = endpoints
        .allow_bing_fallback
        .unwrap_or_else(|| duckduckgo_allows_bing_fallback(context.search_base_url.as_deref()));
    check_policy(decider, &duckduckgo_host)?;
    // #6746: a hanging DuckDuckGo must leave the Bing fallback time to answer.
    // When Bing may follow, DuckDuckGo gets a share of the budget and Bing
    // the remainder; without the fallback DuckDuckGo keeps the whole budget.
    let duckduckgo_budget = if allow_bing_fallback {
        budget.mul_f64(DUCKDUCKGO_BUDGET_SHARE)
    } else {
        budget
    };
    let fetched =
        fetch_duckduckgo_html(&client, &url, &accept_language, duckduckgo_budget, context).await;
    let bing_budget = || {
        budget
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(1))
    };
    let body = match fetched {
        Ok(body) => body,
        // #6746: an unreachable DuckDuckGo (connection error, timeout, or a
        // non-2xx status) must still reach the Bing fallback, not end the
        // chain. Only a Bing answer with results replaces the DuckDuckGo
        // error; otherwise the original failure is reported.
        Err(error)
            if !error.content()
                || matches!(
                    error.error,
                    ToolError::Cancelled { .. } | ToolError::Timeout { seconds: 0 }
                ) =>
        {
            return Err(error);
        }
        Err(error) if allow_bing_fallback => {
            let error = match error.error {
                ToolError::ExecutionFailed { message, .. } => message,
                other => other.to_string(),
            };
            check_policy(decider, BING_HOST)?;
            return match run_bing_search(
                &client,
                &query.query,
                query.locale.as_deref(),
                max_results,
                endpoints.bing,
                bing_budget(),
                context,
            )
            .await
            {
                Ok(results) if !results.is_empty() => {
                    degraded.push(DegradedReason::BackendUnavailable {
                        backend: BackendId::DuckDuckGo,
                    });
                    degraded.push(DegradedReason::BackendFallback {
                        from: BackendId::DuckDuckGo,
                        to: BackendId::Bing,
                    });
                    prune_fallback_locale_ignored(&mut degraded, query.locale.as_deref());
                    Ok(BackendSearch {
                        backend: BackendId::Bing,
                        source: "bing".to_string(),
                        backend_detail: None,
                        results: normalize_entries(results),
                        degraded,
                        note: Some(format!("{error}; used Bing fallback")),
                    })
                }
                Ok(_) => Err((ToolError::execution_failed(format!(
                    "{error}; Bing fallback returned no results"
                )))
                .into()),
                Err(bing_error) if !bing_error.content() => Err(bing_error),
                Err(bing_error) => Err((ToolError::execution_failed(format!(
                    "{error}; Bing fallback failed: {}",
                    match &bing_error.error {
                        ToolError::ExecutionFailed { message, .. } => message.clone(),
                        other => other.to_string(),
                    }
                )))
                .into()),
            };
        }
        Err(error) => return Err(error),
    };

    let results = normalize_scraped_entries(
        parse_duckduckgo_results(&body, max_results),
        context,
        bing_budget(),
    )
    .await?;
    let blocked = is_duckduckgo_challenge(&body);
    if !results.is_empty() {
        return Ok(BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: if allow_bing_fallback {
                "duckduckgo".to_string()
            } else {
                duckduckgo_host.clone()
            },
            backend_detail: (!allow_bing_fallback).then_some(duckduckgo_host),
            results: normalize_entries(results),
            degraded,
            note: None,
        });
    }
    if blocked {
        degraded.push(DegradedReason::ChallengeDetected {
            backend: BackendId::DuckDuckGo,
        });
    }
    if !allow_bing_fallback {
        if blocked {
            return Err((ToolError::execution_failed(format!(
                "DuckDuckGo-compatible search endpoint at {duckduckgo_host} returned a bot challenge; check the private search service, credentials, or network policy"
            ))).into());
        }
        return Ok(BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: duckduckgo_host.clone(),
            backend_detail: Some(duckduckgo_host),
            results: Vec::new(),
            degraded,
            note: None,
        });
    }

    check_policy(decider, BING_HOST)?;
    match run_bing_search(
        &client,
        &query.query,
        query.locale.as_deref(),
        max_results,
        endpoints.bing,
        bing_budget(),
        context,
    )
    .await
    {
        Ok(results) if !results.is_empty() => {
            degraded.push(DegradedReason::ScrapeFallback {
                from: BackendId::DuckDuckGo,
                to: BackendId::Bing,
            });
            prune_fallback_locale_ignored(&mut degraded, query.locale.as_deref());
            Ok(BackendSearch {
                backend: BackendId::Bing,
                source: "bing".to_string(),
                backend_detail: None,
                results: normalize_entries(results),
                degraded,
                note: Some(if blocked {
                    "DuckDuckGo returned a bot challenge; used Bing fallback".to_string()
                } else {
                    "DuckDuckGo returned no parseable results; used Bing fallback".to_string()
                }),
            })
        }
        Err(error) if !error.content() => Err(error),
        Ok(_) if blocked => Err((ToolError::execution_failed(
            "DuckDuckGo returned a bot challenge and Bing fallback returned no results",
        ))
        .into()),
        Err(error) if blocked => Err((ToolError::execution_failed(format!(
            "DuckDuckGo returned a bot challenge and Bing fallback failed: {error}"
        )))
        .into()),
        Ok(_) | Err(_) => Ok(BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: Vec::new(),
            degraded,
            note: None,
        }),
    }
}

/// Fetch the DuckDuckGo HTML results page. A transport failure or a non-2xx
/// status is an error so the caller can decide whether Bing may answer.
async fn fetch_duckduckgo_html(
    client: &reqwest::Client,
    url: &str,
    accept_language: &str,
    timeout: Duration,
    context: &ToolContext,
) -> AdapterResult<String> {
    let resp = client
        .get(url)
        .timeout(timeout)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", accept_language)
        .send()
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("Web search request failed: {error}"))
        })?;
    let status = resp.status();
    let body = if adapter::search_selected(context) {
        adapter::read_response_with_limit(resp, context, super::web::fetch::HARD_MAX_BYTES).await?
    } else {
        resp.text().await.map_err(|error| {
            ToolError::execution_failed(format!("Failed to read response: {error}"))
        })?
    };
    if !status.is_success() {
        return Err(ToolError::execution_failed(format!(
            "Web search failed: HTTP {}",
            status.as_u16()
        ))
        .into());
    }
    Ok(body)
}

async fn normalize_scraped_entries(
    entries: Vec<WebSearchEntry>,
    context: &ToolContext,
    budget: Duration,
) -> AdapterResult<Vec<WebSearchEntry>> {
    normalize_captured_entries(
        entries
            .into_iter()
            .map(|entry| super::web::contract::CapturedSearchEntry {
                title: entry.title,
                url: entry.url,
                snippet: entry.snippet,
                published: None,
            })
            .collect(),
        context,
        budget,
    )
    .await
    .map(|entries| {
        entries
            .into_iter()
            .map(|entry| WebSearchEntry {
                title: entry.title,
                url: entry.url,
                snippet: entry.snippet,
            })
            .collect()
    })
}

fn normalize_entries(entries: Vec<WebSearchEntry>) -> Vec<SearchResult> {
    entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            SearchResult::new(index + 1, entry.title, entry.url, entry.snippet, None)
        })
        .collect()
}

fn rerank(results: &mut [SearchResult]) {
    for (index, result) in results.iter_mut().enumerate() {
        result.rank = u8::try_from(index + 1).unwrap_or(u8::MAX);
    }
}

pub(crate) fn domain_matches(url: &str, domains: &[String]) -> bool {
    if domains.is_empty() {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    domains.iter().any(|domain| {
        let domain = domain.trim_start_matches("www.").to_ascii_lowercase();
        host == domain || host.ends_with(&format!(".{domain}"))
    })
}

fn truncate_error_body(body: &str) -> String {
    let stripped = sanitize_error_body(body);
    if stripped.len() <= ERROR_BODY_PREVIEW_BYTES {
        stripped
    } else {
        let mut end = ERROR_BODY_PREVIEW_BYTES;
        while !stripped.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &stripped[..end])
    }
}

static TAG_RE: OnceLock<Regex> = OnceLock::new();

fn get_tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag regex pattern is valid"))
}

fn strip_html_tags(text: &str) -> String {
    get_tag_re().replace_all(text, "").to_string()
}

fn sanitize_error_body(body: &str) -> String {
    let stripped = strip_html_tags(body);
    let visible: String = stripped
        .chars()
        .filter(|c| !c.is_control() || c.is_ascii_whitespace())
        .collect();
    get_bearer_token_re()
        .replace_all(&visible, "Bearer [REDACTED]")
        .to_string()
}

fn parse_tavily_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("url")?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet: first_non_empty_string(item, &["content", "snippet"]),
            })
        })
        .take(max_results)
        .collect()
}

fn parse_firecrawl_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .pointer("/data/web")
        .or_else(|| parsed.get("data"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("url")?.as_str()?.trim();
            (!title.is_empty() && !url.is_empty()).then(|| WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet: first_non_empty_string(item, &["description", "markdown", "content"])
                    .map(|value| value.chars().take(1_000).collect()),
            })
        })
        .take(max_results)
        .collect()
}

fn parse_metaso_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("webpages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.trim();
            let url = item.get("link")?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet: first_non_empty_string(item, &["snippet", "summary"]),
            })
        })
        .take(max_results)
        .collect()
}

fn parse_bocha_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("data")
        .and_then(|d| {
            d.get("webPages")
                .and_then(|w| w.get("value"))
                .or_else(|| d.get("pages"))
        })
        .or_else(|| parsed.get("pages"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item
                .get("name")
                .or_else(|| item.get("title"))
                .and_then(|s| s.as_str())?
                .trim();
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(|s| s.as_str())?
                .trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = item
                .get("summary")
                .or_else(|| item.get("snippet"))
                .or_else(|| item.get("description"))
                .and_then(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            Some(WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

fn bocha_error_message(parsed: &Value) -> Option<String> {
    let code = parsed.get("code").and_then(|v| v.as_i64())?;
    if code == 0 || code == 200 {
        return None;
    }
    let message = parsed
        .get("msg")
        .or_else(|| parsed.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    Some(format!("Bocha search API error (code {code}: {message})"))
}

fn parse_baidu_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("references")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item
                .get("title")
                .or_else(|| item.get("name"))
                .and_then(|s| s.as_str())?
                .trim();
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .and_then(|s| s.as_str())?
                .trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = item
                .get("content")
                .or_else(|| item.get("snippet"))
                .or_else(|| item.get("summary"))
                .and_then(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            Some(WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

/// Read a SearXNG result `score`.
///
/// SearXNG emits a float, but instances and versions vary: a JSON integer, a
/// numeric string, or no `score` at all are all tolerated. Unusable or
/// non-finite values (`"not-a-number"`, `"NaN"`, `"inf"`, missing) read as
/// `0.0`, so such rows keep their input order behind scored rows instead of
/// being dropped or sorted by NaN.
fn searxng_score(item: &Value) -> f64 {
    let raw = item.get("score");
    let n = raw
        .and_then(Value::as_f64)
        .or_else(|| raw.and_then(Value::as_i64).map(|i| i as f64))
        .or_else(|| {
            raw.and_then(Value::as_str)
                .and_then(|s| s.trim().parse().ok())
        })
        .unwrap_or(0.0);
    if n.is_finite() { n } else { 0.0 }
}

/// Normalize a SearXNG JSON response into the engine-agnostic result shape.
///
/// Rows without a non-empty `title` or `url` are skipped. Everything else is
/// ordered by descending `score` with a stable sort (equal scores keep the
/// instance's order) and only then capped, so a strong late row is not lost to
/// an earlier `take` over the raw instance order.
fn parse_searxng_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    let mut scored: Vec<(f64, WebSearchEntry)> = parsed
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item.get("title").and_then(Value::as_str)?.trim();
            let url = item.get("url").and_then(Value::as_str)?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = first_non_empty_string(item, &["content", "snippet"]);
            Some((
                searxng_score(item),
                WebSearchEntry {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet,
                },
            ))
        })
        .collect();

    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(max_results);

    scored.into_iter().map(|(_, entry)| entry).collect()
}

fn baidu_error_message(parsed: &Value) -> Option<String> {
    let code = parsed
        .get("error_code")
        .or_else(|| parsed.get("code"))
        .and_then(|v| v.as_i64())?;
    if code == 0 {
        return None;
    }
    let message = parsed
        .get("error_msg")
        .or_else(|| parsed.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    Some(format!("Baidu search API error (code {code}: {message})"))
}

async fn acquire_model_backed_search_inference_participant()
-> crate::client::RemoteControlInferencePermit {
    crate::client::acquire_remote_control_inference_participant().await
}

fn parse_sofya_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let url = item.get("url")?.as_str()?.to_string();
            let snippet = first_non_empty_string(item, &["content", "description"]);
            Some(WebSearchEntry {
                title,
                url,
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

/// Build the Serply `/v1/search` URL; `num` is the number of organic results.
fn tavily_search_payload(
    api_key: &str,
    query: &str,
    filters: QueryFilters<'_>,
    max_results: usize,
) -> Value {
    let mut payload = json!({
        "api_key": api_key, // noqa: api-key-in-body
        "query": query,
        "search_depth": "basic",
        "max_results": max_results,
    });
    if let Some(window) = filters.window() {
        payload["time_range"] = json!(window);
    }
    payload
}

/// Serply documents `gl` (country); `hl` is the Google interface-language
/// parameter it forwards. Recency is not documented, so it is not sent.
fn serply_search_url(
    query: &str,
    filters: QueryFilters<'_>,
    max_results: usize,
) -> Result<reqwest::Url, ToolError> {
    let mut url = reqwest::Url::parse(SERPLY_ENDPOINT)
        .map_err(|error| ToolError::invalid_input(format!("Invalid Serply endpoint: {error}")))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs
            .append_pair("q", query)
            .append_pair("num", &max_results.to_string());
        if let Some(language) = filters.language() {
            pairs.append_pair("hl", &language);
        }
        if let Some(region) = filters.region() {
            pairs.append_pair("gl", &region.to_ascii_lowercase());
        }
    }
    Ok(url)
}

/// Parse Serply `/v1/search` output: `results[]` rows carry `title`, `link`, and
/// a `description` snippet; ads, knowledge graph, and related questions are
/// top-level siblings and are ignored.
fn parse_serply_results(parsed: &Value, max_results: usize) -> Vec<WebSearchEntry> {
    parsed
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let url = item.get("link")?.as_str()?.to_string();
            let snippet = first_non_empty_string(item, &["description", "snippet"]);
            Some(WebSearchEntry {
                title,
                url,
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

fn parse_deepseek_results(parsed: &Value, max_results: usize) -> Vec<SearchResult> {
    parsed
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let title = first_non_empty_string(item, &["title"])?;
            let url = first_non_empty_string(item, &["url"])?;
            Some((
                title,
                url,
                first_non_empty_string(item, &["snippet", "summary"]),
                first_non_empty_string(item, &["date"]),
            ))
        })
        .take(max_results)
        .enumerate()
        .map(|(index, (title, url, snippet, published))| {
            SearchResult::new(index + 1, title, url, snippet, published)
        })
        .collect()
}

fn first_non_empty_string(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        item.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn baidu_search_payload(query: &str, max_results: usize) -> Value {
    json!({
        "messages": [
            {
                "role": "user",
                "content": query,
            }
        ],
        "search_source": "baidu_search_v2",
        "resource_type_filter": [
            {
                "type": "web",
                "top_k": max_results,
            }
        ],
    })
}

fn volcengine_search_payload(query: &str, max_results: usize) -> Value {
    json!({
        "model": "doubao-seed-2-0-lite-260428",
        "stream": false,
        "tools": [{"type": "web_search"}],
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!(
                    "Search the web for: {query}\n\n\
                     CRITICAL: Respond ONLY with a valid JSON object. No markdown, no explanation.\n\
                     Schema: {{\"results\":[{{\"title\":\"...\",\"url\":\"https://...\",\"snippet\":\"...\"}}]}}\n\
                     - results: 1-{max_results} most relevant pages\n\
                     - title: page title (required)\n\
                     - url: full URL starting with https:// (required)\n\
                     - snippet: 1-2 sentence factual summary (required)\n\
                     - If zero results: {{\"results\":[]}}\n\
                     - Your entire response must be valid, parseable JSON."
                )
            }]
        }]
    })
}

/// Extracts the model's text response from a Volcengine Responses API output.
fn volcengine_extract_text(parsed: &Value) -> Option<String> {
    parsed
        .get("output")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter().rev())
        .find(|item| item.get("type").and_then(|t| t.as_str()) == Some("message"))
        .and_then(|msg| msg.get("content").and_then(|c| c.as_array()))
        .and_then(|content| {
            content
                .iter()
                .find(|c| c.get("text").and_then(|t| t.as_str()).is_some())
        })
        .and_then(|c| c.get("text").and_then(|t| t.as_str()))
        .map(|s| s.to_string())
}

/// Checks for business-logic errors in a Volcengine Responses API response.
fn volcengine_error_message(parsed: &Value) -> Option<String> {
    let error = parsed.get("error")?;
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("no details");
    Some(format!("Volcengine API error (code {code}: {message})"))
}

/// Parses Volcengine model-generated JSON results into `WebSearchEntry` items.
fn parse_volcengine_results(response_text: &str, max_results: usize) -> Vec<WebSearchEntry> {
    let json_text = extract_json_block(response_text).unwrap_or(response_text);

    let parsed: Value = match serde_json::from_str(json_text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    parsed
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|arr| arr.iter())
        .filter_map(|item| {
            let title = item.get("title").and_then(|s| s.as_str())?.trim();
            let url = item.get("url").and_then(|s| s.as_str())?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = item
                .get("snippet")
                .and_then(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            Some(WebSearchEntry {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .take(max_results)
        .collect()
}

/// Attempts to extract a JSON block from text that may be wrapped in
/// markdown fences (```json ... ```) or contain surrounding commentary.
fn extract_json_block(text: &str) -> Option<&str> {
    if let Some(start) = text.find("```json") {
        let inner = &text[start + 7..];
        if let Some(end) = inner.find("```") {
            return Some(inner[..end].trim());
        }
    }
    if let Some(start) = text.find('{')
        && let Some(end) = text.rfind('}')
    {
        return Some(&text[start..=end]);
    }
    None
}

fn extract_search_query(input: &Value) -> Result<String, ToolError> {
    for key in ["query", "q"] {
        if let Some(value) = input.get(key) {
            let Some(query) = value.as_str() else {
                return Err(ToolError::invalid_input(format!(
                    "Field '{key}' must be a string"
                )));
            };
            let query = query.trim();
            if !query.is_empty() {
                return Ok(query.to_string());
            }
        }
    }

    for item in search_query_items(input) {
        for key in ["q", "query"] {
            if let Some(value) = item.get(key) {
                let Some(query) = value.as_str() else {
                    return Err(ToolError::invalid_input(format!(
                        "Field 'search_query[].{key}' must be a string"
                    )));
                };
                let query = query.trim();
                if !query.is_empty() {
                    return Ok(query.to_string());
                }
            }
        }
    }

    Err(ToolError::missing_field("query"))
}

fn optional_search_max_results(input: &Value) -> u64 {
    if let Some(value) = input.get("max_results").and_then(Value::as_u64) {
        return value;
    }
    search_query_items(input)
        .filter_map(|item| item.get("max_results").and_then(Value::as_u64))
        .next()
        .unwrap_or(DEFAULT_SEARCH_RESULTS as u64)
}

fn search_query_from_input(input: &Value) -> Result<SearchQuery, ToolError> {
    let query = extract_search_query(input)?;
    if query.is_empty() {
        return Err(ToolError::invalid_input("Query cannot be empty"));
    }
    let max_results = usize::try_from(optional_search_max_results(input))
        .unwrap_or(DEFAULT_SEARCH_RESULTS)
        .clamp(1, usize::from(MAX_SEARCH_RESULTS));
    let recency = search_option(input, "recency")
        .map(parse_recency)
        .transpose()?;
    let domains = match search_option(input, "domains") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| ToolError::invalid_input("Field 'domains' must be an array"))?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    ToolError::invalid_input("Every 'domains' entry must be a string")
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    let locale = search_option(input, "locale")
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| ToolError::invalid_input("Field 'locale' must be a string"))
        })
        .transpose()?;

    Ok(SearchQuery::new(
        query,
        max_results,
        recency,
        domains,
        locale,
    ))
}

fn search_option<'a>(input: &'a Value, key: &str) -> Option<&'a Value> {
    input
        .get(key)
        .or_else(|| search_query_items(input).find_map(|item| item.get(key)))
}

fn parse_recency(value: &Value) -> Result<Recency, ToolError> {
    if let Some(days) = value.as_u64() {
        let days = u16::try_from(days)
            .ok()
            .filter(|days| (1..=3650).contains(days))
            .ok_or_else(|| {
                ToolError::invalid_input("Field 'recency' must be between 1 and 3650 days")
            })?;
        return Ok(Recency::Days(days));
    }
    match value.as_str() {
        Some("day") => Ok(Recency::Day),
        Some("week") => Ok(Recency::Week),
        Some("month") => Ok(Recency::Month),
        Some("year") => Ok(Recency::Year),
        _ => Err(ToolError::invalid_input(
            "Field 'recency' must be day, week, month, year, or an integer day count",
        )),
    }
}

fn search_query_items(input: &Value) -> impl Iterator<Item = &Value> {
    input
        .get("search_query")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
}

/// Whether `query` contains Han ideographs (unified ideographs plus
/// Extension A). Kana and Hangul are deliberately excluded: this heuristic
/// exists only to pick a Chinese market for the keyless Bing/DuckDuckGo
/// scrapes when the model omits `locale`, and forcing Japanese or Korean
/// queries into the zh-CN market would be worse than sending no market
/// signal at all.
fn query_contains_han(query: &str) -> bool {
    query
        .chars()
        .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'))
}

/// Whether `query` carries kana or hangul — the scripts that make a
/// Han-bearing query Japanese or Korean rather than Chinese. Real Japanese
/// queries almost always contain kana alongside kanji, so a Han-bearing
/// query with any kana or hangul must keep the no-market request instead of
/// being pushed onto the zh-CN market (the inverse of the drift the
/// fallback exists to fix). Only a purely Han-script query — rare for
/// search and indistinguishable from Chinese — still takes the fallback,
/// alongside unmarked Traditional Chinese (the accepted cost).
fn query_contains_kana_or_hangul(query: &str) -> bool {
    query.chars().any(|c| {
        matches!(
            c,
            '\u{3040}'..='\u{30FF}' // hiragana + katakana
                | '\u{31F0}'..='\u{31FF}' // katakana phonetic extensions
                | '\u{AC00}'..='\u{D7AF}' // hangul syllables
                | '\u{1100}'..='\u{11FF}' // hangul jamo
                | '\u{3130}'..='\u{318F}' // hangul compatibility jamo
        )
    })
}

/// Market tag the scrape backends should request, or `None` to keep the
/// historical no-market-signal request. An explicit locale wins; otherwise a
/// Han-script query without kana or hangul falls back to zh-CN because
/// without any market hint (and with an English `Accept-Language`) Bing
/// serves unrelated Japanese results for Chinese queries. The model-supplied
/// locale is shape-checked first so a malformed value cannot become a broken
/// market tag or header.
fn scrape_market(locale: Option<&str>, query: &str) -> Option<String> {
    locale
        .map(str::trim)
        .filter(|tag| is_plausible_locale_tag(tag))
        .map(|tag| tag.replace('_', "-"))
        .or_else(|| {
            (query_contains_han(query) && !query_contains_kana_or_hangul(query))
                .then(|| "zh-CN".to_string())
        })
}

/// Light shape check for a model-supplied `locale`: an alphabetic primary
/// language subtag followed by alphanumeric subtags separated by `-`/`_`,
/// each at most 8 characters, with no empty or separator-only edges. The
/// alphabetic primary matters for receipt honesty: `12345` is not a locale,
/// and transmitting it (with `honored.locale=true`) would contradict the
/// schema text promising that malformed values are ignored. Anything else
/// (e.g. `-CN`, `zh CN`, an injection attempt) is treated as no locale and
/// keeps the historical request shape.
fn is_plausible_locale_tag(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > 35 {
        return false;
    }
    let mut subtags = tag.split(['-', '_']);
    let primary = subtags.next().unwrap_or_default();
    if primary.is_empty() || primary.len() > 8 || !primary.chars().all(|c| c.is_ascii_alphabetic())
    {
        return false;
    }
    subtags.all(|subtag| {
        !subtag.is_empty() && subtag.len() <= 8 && subtag.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// `Accept-Language` matching [`scrape_market`]. With no market resolved the
/// English default is kept — the value the Bing path has always sent
/// (`en-US,en;q=0.9`); the DuckDuckGo path previously sent `q=0.5` and now
/// shares this one rule.
fn scrape_accept_language(market: Option<&str>) -> String {
    match market {
        None => "en-US,en;q=0.9".to_string(),
        Some(market) => {
            let primary = market
                .split(['-', '_'])
                .next()
                .filter(|tag| !tag.is_empty())
                .unwrap_or("en");
            let mut value = if market.eq_ignore_ascii_case(primary) {
                format!("{market};q=0.9")
            } else {
                format!("{market},{primary};q=0.9")
            };
            // The generic `en` fallback duplicates the primary tag for
            // English markets (`en-US,en;q=0.9,en;q=0.8`), where two q-values
            // for one range have no defined precedence — only add it when it
            // contributes a distinct range.
            if !primary.eq_ignore_ascii_case("en") {
                value.push_str(",en;q=0.8");
            }
            value
        }
    }
}

/// Bing request adjustments for the resolved market: the `mkt`/`setlang`
/// query parameters and the `Accept-Language` header value.
fn scrape_locale_params(locale: Option<&str>, query: &str) -> (Vec<(String, String)>, String) {
    let market = scrape_market(locale, query);
    let accept_language = scrape_accept_language(market.as_deref());
    let Some(market) = market else {
        return (Vec::new(), accept_language);
    };
    let primary = market
        .split(['-', '_'])
        .next()
        .filter(|tag| !tag.is_empty())
        .unwrap_or("en");
    // Bing expects `setlang` to carry a script tag for Chinese (a bare `zh`
    // is invalid and silently defaults to `en`). An explicit script subtag
    // wins (`zh-Hant-TW` must not degrade to Simplified); otherwise the
    // script is picked from the market's region subtag, defaulting to
    // Simplified because the heuristic that reaches this branch without an
    // explicit region is Han-driven.
    let setlang = if primary.eq_ignore_ascii_case("zh") {
        let segments: Vec<&str> = market.split(['-', '_']).collect();
        if segments
            .iter()
            .any(|segment| segment.eq_ignore_ascii_case("hant"))
        {
            "zh-Hant"
        } else if segments
            .iter()
            .any(|segment| segment.eq_ignore_ascii_case("hans"))
        {
            "zh-Hans"
        } else {
            match segments.get(1) {
                Some(region)
                    if matches!(region.to_ascii_lowercase().as_str(), "tw" | "hk" | "mo") =>
                {
                    "zh-Hant"
                }
                _ => "zh-Hans",
            }
        }
    } else {
        primary
    };
    (
        vec![
            ("mkt".to_string(), market.clone()),
            ("setlang".to_string(), setlang.to_string()),
        ],
        accept_language,
    )
}

/// True when an explicit `locale` was supplied but cannot shape a market
/// signal for the Bing scrape (`mkt`/`setlang`): the value is malformed, so
/// the request proceeds with no locale signal at all and the receipt must
/// say so instead of claiming the knob was honored.
fn bing_locale_was_ignored(locale: Option<&str>) -> bool {
    locale.is_some_and(|tag| !is_plausible_locale_tag(tag.trim()))
}

/// The Bing fallback resolves the locale through Bing's own market mapping,
/// so a `KnobIgnored` flag pushed for the DuckDuckGo leg (whose verified
/// `kl` list is narrow) no longer describes the backend that produced the
/// results: drop it and re-derive from Bing's rule, where anything
/// shape-valid was sent as `mkt`/`setlang` and only an implausible tag
/// stays ignored.
fn prune_fallback_locale_ignored(degraded: &mut Vec<DegradedReason>, locale: Option<&str>) {
    degraded.retain(|reason| {
        !matches!(
            reason,
            DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            }
        )
    });
    if bing_locale_was_ignored(locale) {
        degraded.push(DegradedReason::KnobIgnored {
            knob: QueryKnob::Locale,
        });
    }
}

/// True when an explicit `locale` was supplied but the DuckDuckGo scrape
/// honors no part of it as a region signal: the value is malformed (the
/// shape check fails, independently of how [`scrape_market`] resolved the
/// market — a pure-Han query still falls back to the `zh-CN` market for a
/// malformed locale) or the resolved market is outside the verified `kl`
/// region list. In the unmapped case no `kl` parameter is sent, but the
/// `Accept-Language` header still carries the locale (see
/// [`scrape_accept_language`]); only the missing region signal is
/// receipted, so the schema's "malformed values are ignored" promise — and
/// parity with the Bing leg — holds either way.
fn ddg_locale_was_ignored(locale: Option<&str>, market: Option<&str>) -> bool {
    locale.is_some_and(|tag| {
        !is_plausible_locale_tag(tag.trim()) || market.and_then(ddg_region_param).is_none()
    })
}

/// Translate a BCP 47-style market tag into DuckDuckGo's `kl` region value.
/// DuckDuckGo's HTML endpoints take `kl` from a fixed, non-systematic list
/// (`cn-zh`, `us-en`, `jp-jp`, `kr-kr`, `tw-tzh`, `hk-tzh`, ...), so a
/// mechanical reversal of a BCP 47 tag produces off-list junk for most
/// non-English locales. Only verified pairs are translated; anything else
/// sends no `kl` at all — exactly the no-region request the scrape made
/// before this knob existed. Custom DuckDuckGo-compatible services typically
/// ignore `kl` entirely.
fn ddg_region_param(market: &str) -> Option<String> {
    let normalized = market.to_ascii_lowercase().replace('_', "-");
    // Script+region Chinese tags reduce to their region pair: the schema
    // teaches BCP 47, where the script subtag is best practice for Chinese,
    // and the verified list maps the region form.
    let region = match normalized.as_str() {
        "zh-cn" | "zh-hans-cn" => "cn-zh",
        "en-us" => "us-en",
        "ja-jp" => "jp-jp",
        "ko-kr" => "kr-kr",
        "zh-tw" | "zh-hant-tw" => "tw-tzh",
        "zh-hk" | "zh-hant-hk" => "hk-tzh",
        _ => return None,
    };
    Some(region.to_string())
}

async fn run_bing_search(
    client: &reqwest::Client,
    query: &str,
    locale: Option<&str>,
    max_results: usize,
    endpoint: &str,
    timeout: Duration,
    context: &ToolContext,
) -> AdapterResult<Vec<WebSearchEntry>> {
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|error| ToolError::invalid_input(format!("Invalid Bing endpoint: {error}")))?;
    let (extra_params, accept_language) = scrape_locale_params(locale, query);
    if let Some(plan) = host_request(
        "bing",
        query,
        QueryFilters::default(),
        max_results,
        context,
        u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
    )
    .await?
    {
        for (key, value) in plan.pairs {
            url.query_pairs_mut().append_pair(&key, &value);
        }
    } else {
        url.query_pairs_mut().append_pair("q", query);
    }
    // The scrape chain hands the host adapter default filters, so the plan
    // never carries the locale; the mkt/setlang parameters ride along in
    // either request shape so the receipt cannot overclaim locale support.
    for (key, value) in &extra_params {
        url.query_pairs_mut().append_pair(key, value);
    }
    let resp = client
        .get(url)
        .timeout(timeout)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", accept_language)
        .send()
        .await
        .map_err(|e| ToolError::execution_failed(format!("Bing search request failed: {e}")))?;

    let status = resp.status();
    let body = if adapter::search_selected(context) {
        adapter::read_response_with_limit(resp, context, super::web::fetch::HARD_MAX_BYTES).await?
    } else {
        resp.text().await.map_err(|e| {
            ToolError::execution_failed(format!("Failed to read Bing search response: {e}"))
        })?
    };

    if !status.is_success() {
        return Err((ToolError::execution_failed(format!(
            "Bing search failed: HTTP {}",
            status.as_u16()
        )))
        .into());
    }

    normalize_scraped_entries(parse_bing_results(&body, max_results), context, timeout).await
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<WebSearchEntry> {
    scrape_duckduckgo_results(html, max_results)
        .into_iter()
        .map(web_search_entry_from_scraped)
        .collect()
}

fn parse_bing_results(html: &str, max_results: usize) -> Vec<WebSearchEntry> {
    scrape_bing_results(html, max_results)
        .into_iter()
        .map(web_search_entry_from_scraped)
        .collect()
}

fn web_search_entry_from_scraped(entry: ScrapedSearchResult) -> WebSearchEntry {
    WebSearchEntry {
        title: entry.title,
        url: entry.url,
        snippet: entry.snippet,
    }
}

fn duckduckgo_search_base(base_url: Option<&str>) -> Result<(reqwest::Url, String), ToolError> {
    let raw = configured_search_base_url(base_url).unwrap_or(DUCKDUCKGO_ENDPOINT);
    let url = reqwest::Url::parse(raw).map_err(|err| {
        ToolError::invalid_input(format!(
            "Invalid DuckDuckGo-compatible search base_url: {err}"
        ))
    })?;
    let host = url.host_str().ok_or_else(|| {
        ToolError::invalid_input("DuckDuckGo-compatible search base_url must include a host")
    })?;
    let host = host.to_owned();
    Ok((url, host))
}

fn duckduckgo_search_url(
    base_url: Option<&str>,
    query: &str,
    market: Option<&str>,
) -> Result<(String, String), ToolError> {
    let (mut url, host) = duckduckgo_search_base(base_url)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        // DuckDuckGo HTML endpoints take the market hint as `kl`; see
        // [`ddg_region_param`] for the verified region-language mapping.
        // Custom DDG-compatible services simply ignore the extra parameter.
        if let Some(region) = market.and_then(ddg_region_param) {
            pairs.append_pair("kl", &region);
        }
    }
    Ok((url.to_string(), host))
}

fn searxng_search_base(base_url: Option<&str>) -> Result<(reqwest::Url, String), ToolError> {
    let raw = configured_search_base_url(base_url).ok_or_else(|| {
        ToolError::invalid_input(
            "SearXNG search requires [search] base_url = \"https://your-searxng.example\"; no public instance is used by default.",
        )
    })?;
    let mut url = reqwest::Url::parse(raw).map_err(|err| {
        ToolError::invalid_input(format!("Invalid SearXNG search base_url: {err}"))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| ToolError::invalid_input("SearXNG search base_url must include a host"))?
        .to_string();

    let path = url.path().trim_end_matches('/');
    if path.is_empty() {
        url.set_path("search");
    } else if path != "/search" && !path.ends_with("/search") {
        url.set_path(&format!("{path}/search"));
    }
    Ok((url, host))
}

fn searxng_search_url(
    base_url: Option<&str>,
    query: &str,
    filters: QueryFilters<'_>,
) -> Result<(String, String), ToolError> {
    let (mut url, host) = searxng_search_base(base_url)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query).append_pair("format", "json");
        if let Some(window) = filters.window() {
            pairs.append_pair("time_range", window);
        }
        if let Some(locale) = filters.locale {
            pairs.append_pair("language", locale);
        }
    }

    Ok((url.to_string(), host))
}

fn configured_search_base_url(base_url: Option<&str>) -> Option<&str> {
    base_url.map(str::trim).filter(|value| !value.is_empty())
}

fn duckduckgo_allows_bing_fallback(base_url: Option<&str>) -> bool {
    configured_search_base_url(base_url).is_none()
}

#[cfg(test)]
mod tests {
    use super::{
        ERROR_BODY_PREVIEW_BYTES, KIMI_K3_FORMULA_MIN_TIMEOUT_MS, QueryFilters, ScrapeEndpoints,
        SearchProbeTargetError, WebSearchTool, acquire_model_backed_search_inference_participant,
        baidu_search_payload, bing_locale_was_ignored, bocha_error_message, ddg_locale_was_ignored,
        ddg_region_param, domain_matches, duckduckgo_search_url, extract_search_query,
        finalize_search_response, native_answer_time_budget, optional_search_max_results,
        parse_baidu_results, parse_bocha_results, parse_deepseek_results, parse_metaso_results,
        parse_searxng_results, parse_serply_results, parse_sofya_results, parse_tavily_results,
        parse_volcengine_results, prune_fallback_locale_ignored, register_search_citations, rerank,
        run_scrape_search_with_endpoints, sanitize_error_body, scrape_accept_language,
        scrape_locale_params, scrape_market, search_probe_target, search_timeout_budgets,
        searxng_score, searxng_search_url, serply_search_url, truncate_error_body,
        volcengine_extract_text,
    };
    use crate::tools::web::contract::{
        BackendId, BackendSearch, CapabilityState, DegradedReason, QueryCapabilities, QueryKnob,
        Recency, SearchQuery, SearchResult,
    };
    use crate::tools::web::scrape::{decode_html_entities, normalize_bing_url};
    use codewhale_runtime::config::SearchProvider;
    use serde_json::json;
    use std::time::{Duration, Instant};

    #[test]
    fn provider_native_receives_dedicated_budget_without_extending_fallback() {
        let requested = Duration::from_millis(15_000);
        let (total, first, fallback) =
            search_timeout_budgets(BackendId::ProviderNative, requested, None);

        assert_eq!(total, Duration::from_millis(60_000));
        assert_eq!(first, Some(Duration::from_millis(45_000)));
        assert_eq!(fallback, Some(requested));

        let (total, first, fallback) = search_timeout_budgets(
            BackendId::ProviderNative,
            requested,
            Some(Duration::from_millis(KIMI_K3_FORMULA_MIN_TIMEOUT_MS)),
        );
        assert_eq!(total, Duration::from_millis(195_000));
        assert_eq!(first, Some(Duration::from_millis(180_000)));
        assert_eq!(fallback, Some(requested));
    }

    #[test]
    fn provider_native_budget_covers_the_requested_answer_length() {
        // #6508: native search asks for up to 8,192 output tokens in one
        // non-streaming request. The attempt must outlast generating them at
        // the assumed rate, or a long answer times out instead of arriving.
        let requested = Duration::from_millis(15_000);
        for tokens in [128_u32, 2_048, 4_096, 8_192] {
            let floor = native_answer_time_budget(tokens);
            let generation = Duration::from_millis(
                u64::from(tokens) * 1_000 / super::NATIVE_SEARCH_ASSUMED_TOKENS_PER_SEC,
            );
            assert!(floor >= generation, "{tokens} tokens: {floor:?}");
            let (total, first, fallback) =
                search_timeout_budgets(BackendId::ProviderNative, requested, Some(floor));
            let first = first.expect("provider-native gets a dedicated attempt");
            assert!(first >= floor, "{tokens} tokens: attempt {first:?}");
            // The minimum never drops below the pre-#6508 floor.
            assert!(first >= Duration::from_millis(45_000));
            assert_eq!(total, first + requested);
            assert_eq!(fallback, Some(requested));
        }
        // 8,192 tokens at 40/s plus the round-trip allowance: ~4 minutes.
        assert_eq!(
            native_answer_time_budget(8_192),
            Duration::from_millis(234_800)
        );
    }

    #[test]
    fn provider_native_floor_follows_what_the_client_requests() {
        use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
        let config = {
            let __cw_config_init_0_0 = Some("anthropic".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                anthropic: ProviderConfig {
                    api_key: Some("anthropic-test-key".to_string()),
                    base_url: Some("https://api.anthropic.com".to_string()),
                    model: Some("claude-opus-4-8".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let client = crate::client::ProviderNativeSearchClient::new(
            crate::client::CodewhaleClient::new(&config).expect("Anthropic client"),
        )
        .expect("Anthropic native adapter");
        let tokens = client
            .requested_answer_output_tokens()
            .expect("Anthropic requests an explicit answer length");
        assert!(tokens > 2_048, "catalogued model asks for more: {tokens}");
        assert_eq!(
            super::provider_native_timeout_floor(&client),
            Some(native_answer_time_budget(tokens))
        );
    }

    #[test]
    fn doctor_search_probe_targets_cover_every_builtin_provider() {
        let cases = [
            (SearchProvider::Bing, "https://www.bing.com/search"),
            (
                SearchProvider::DuckDuckGo,
                "https://html.duckduckgo.com/html/",
            ),
            (
                SearchProvider::Firecrawl,
                "https://api.firecrawl.dev/v2/search",
            ),
            (SearchProvider::Tavily, "https://api.tavily.com/search"),
            (
                SearchProvider::Bocha,
                "https://api.bochaai.com/v1/web-search",
            ),
            (SearchProvider::Metaso, "https://metaso.cn/api/v1"),
            (
                SearchProvider::Baidu,
                "https://qianfan.baidubce.com/v2/ai_search/web_search",
            ),
            (
                SearchProvider::Volcengine,
                "https://ark.cn-beijing.volces.com/api/v3/responses",
            ),
            (SearchProvider::Sofya, "https://sofya.co/v1/search"),
            (SearchProvider::Serply, "https://api.serply.io/v1/search"),
            (
                SearchProvider::DeepSeek,
                "https://api.deepseek.com/v1/tools/search",
            ),
        ];

        for (provider, expected) in cases {
            let target = search_probe_target(provider, None).expect("built-in target");
            assert_eq!(target.url.as_str(), expected, "{provider:?}");
            assert_eq!(target.host, target.url.host_str().unwrap(), "{provider:?}");
        }
    }

    #[test]
    fn doctor_search_probe_strips_every_secret_capable_custom_url_component() {
        let target = search_probe_target(
            SearchProvider::Searxng,
            Some(
                "https://URL-USER:URL-PASSWORD@search.example:8443/private/URL-PATH?URL-QUERY=secret#URL-FRAGMENT",
            ),
        )
        .expect("credential-free target");

        assert_eq!(target.url.as_str(), "https://search.example:8443/");
        assert_eq!(target.host, "search.example");
    }

    #[test]
    fn doctor_search_probe_rejects_configuration_that_runtime_cannot_use() {
        assert_eq!(
            search_probe_target(SearchProvider::Searxng, None),
            Err(SearchProbeTargetError::Missing)
        );
        assert_eq!(
            search_probe_target(SearchProvider::Tavily, Some("https://ignored.example")),
            Err(SearchProbeTargetError::Unsupported)
        );
        assert_eq!(
            search_probe_target(SearchProvider::DuckDuckGo, Some("file:///tmp/search")),
            Err(SearchProbeTargetError::Invalid)
        );
    }

    #[test]
    fn bing_ckurl_with_html_entities_decodes_real_url() {
        let href = "https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9ydXN0LWxhbmcub3JnLw&amp;ntb=1";
        assert_eq!(normalize_bing_url(href), "https://rust-lang.org/");
    }

    #[test]
    fn decode_html_entities_handles_named_entities() {
        assert_eq!(decode_html_entities("&amp;"), "&");
        assert_eq!(decode_html_entities("&lt;"), "<");
        assert_eq!(decode_html_entities("&gt;"), ">");
        assert_eq!(decode_html_entities("&quot;"), "\"");
        assert_eq!(decode_html_entities("&apos;"), "'");
        assert_eq!(decode_html_entities("&nbsp;"), " ");
        assert_eq!(decode_html_entities("&copy;"), "\u{00A9}");
        assert_eq!(decode_html_entities("&mdash;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_handles_decimal_numeric_references() {
        assert_eq!(decode_html_entities("&#65;"), "A");
        assert_eq!(decode_html_entities("&#60;"), "<");
        assert_eq!(decode_html_entities("&#8211;"), "\u{2013}");
    }

    #[test]
    fn decode_html_entities_handles_hex_numeric_references() {
        assert_eq!(decode_html_entities("&#x41;"), "A");
        assert_eq!(decode_html_entities("&#x3C;"), "<");
        assert_eq!(decode_html_entities("&#x2014;"), "\u{2014}");
    }

    #[test]
    fn decode_html_entities_passthrough_unknown() {
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn decode_html_entities_mixed_content() {
        let input = "Hello &amp; welcome to &quot;Rust&apos;s world&quot; &mdash; enjoy!";
        let expected = "Hello & welcome to \"Rust's world\" \u{2014} enjoy!";
        assert_eq!(decode_html_entities(input), expected);
    }

    #[test]
    fn extract_search_query_accepts_legacy_query() {
        let query =
            extract_search_query(&json!({"query": " deepseek v4 "})).expect("query should parse");
        assert_eq!(query, "deepseek v4");
    }

    #[test]
    fn extract_search_query_accepts_q_alias() {
        let query =
            extract_search_query(&json!({"q": "deepseek v4 pro"})).expect("q alias should parse");
        assert_eq!(query, "deepseek v4 pro");
    }

    #[test]
    fn extract_search_query_accepts_array_form() {
        let input = json!({"search_query": [{"q": "deepseek api", "max_results": 3}]});
        let query = extract_search_query(&input).expect("array form should parse");
        assert_eq!(query, "deepseek api");
        assert_eq!(optional_search_max_results(&input), 3);
    }

    #[test]
    fn extract_search_query_rejects_missing_query() {
        let err = extract_search_query(&json!({"max_results": 2}))
            .expect_err("missing query should fail");
        assert!(format!("{err}").contains("missing required field 'query'"));
    }

    #[test]
    fn optional_max_results_prefers_top_level_value() {
        assert_eq!(
            optional_search_max_results(
                &json!({"query": "x", "max_results": 8, "search_query": [{"q": "y", "max_results": 2}]})
            ),
            8,
        );
    }

    #[test]
    fn optional_max_results_falls_back_to_array_form() {
        assert_eq!(
            optional_search_max_results(&json!({"search_query": [{"q": "y", "max_results": 3}]})),
            3,
        );
    }

    #[test]
    fn optional_max_results_uses_default_when_neither_set() {
        assert_eq!(optional_search_max_results(&json!({"query": "x"})), 5);
        assert_eq!(
            optional_search_max_results(&json!({"search_query": [{"q": "y"}]})),
            5,
        );
    }

    #[test]
    fn optional_max_results_only_reads_first_array_entry() {
        assert_eq!(
            optional_search_max_results(
                &json!({"search_query": [{"q": "first", "max_results": 1}, {"q": "second", "max_results": 9}]})
            ),
            1,
        );
    }

    #[test]
    fn extract_search_query_trims_whitespace_from_array_form_q_alias() {
        let q = extract_search_query(&json!({"search_query": [{"q": "  deepseek tui  "}]}))
            .expect("array form should parse with trim");
        assert_eq!(q, "deepseek tui");
    }

    #[test]
    fn extract_search_query_rejects_empty_query() {
        for body in [json!({"query": ""}), json!({"q": "   "}), json!({})] {
            let err = extract_search_query(&body).expect_err("empty query must reject");
            let msg = format!("{err}");
            assert!(
                msg.contains("missing required field 'query'") || msg.contains("Query"),
                "expected query-missing error, got `{msg}`"
            );
        }
    }

    #[test]
    fn truncate_error_body_truncates_long_body() {
        let body = "a".repeat(ERROR_BODY_PREVIEW_BYTES + 100);
        let truncated = truncate_error_body(&body);
        assert!(truncated.len() <= ERROR_BODY_PREVIEW_BYTES + 3);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_error_body_keeps_short_body_intact() {
        let body = "short error";
        assert_eq!(truncate_error_body(body), body);
    }

    #[test]
    fn sanitize_error_body_strips_html_and_control_chars() {
        let body = "<p>error</p>\x00\x01\x02";
        let sanitized = sanitize_error_body(body);
        assert_eq!(sanitized, "error");
    }

    #[test]
    fn sanitize_error_body_redacts_bearer_tokens() {
        let body = r#"{"error":"bad token","authorization":"Bearer test-token/with+chars="}"#;

        let sanitized = sanitize_error_body(body);

        assert!(!sanitized.contains("test-token/with+chars="));
        assert!(sanitized.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn parse_bocha_web_pages_value_extracts_ranked_results() {
        let body = json!({
            "code": 200,
            "msg": null,
            "data": {
                "webPages": {
                    "value": [
                        {
                            "name": "广州天气",
                            "url": "https://bocha.cn/share/weather",
                            "snippet": "广州今日雷阵雨转晴。"
                        },
                        {
                            "name": "中央气象台",
                            "url": "https://www.weather.com.cn/",
                            "summary": "天气实况。"
                        }
                    ]
                }
            }
        });

        let results = parse_bocha_results(&body, 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "广州天气");
        assert_eq!(results[0].url, "https://bocha.cn/share/weather");
        assert_eq!(results[0].snippet.as_deref(), Some("广州今日雷阵雨转晴。"));
        assert_eq!(results[1].title, "中央气象台");
    }

    #[test]
    fn parse_bocha_keeps_legacy_pages_shape() {
        let body = json!({
            "code": 200,
            "data": {
                "pages": [
                    {
                        "title": "Legacy title",
                        "link": "https://example.com/legacy",
                        "description": "Legacy description"
                    }
                ]
            }
        });

        let results = parse_bocha_results(&body, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Legacy title");
        assert_eq!(results[0].url, "https://example.com/legacy");
        assert_eq!(results[0].snippet.as_deref(), Some("Legacy description"));
    }

    #[test]
    fn bocha_error_message_flags_non_success_business_code() {
        let body = json!({"code": 401, "msg": "invalid api key"});

        let error = bocha_error_message(&body).expect("non-success code should error");

        assert!(error.contains("Bocha"));
        assert!(error.contains("401"));
        assert!(error.contains("invalid api key"));
    }

    #[test]
    fn parse_baidu_references_extracts_ranked_results() {
        let body = json!({
            "references": [
                {
                    "title": "Rust 官方文档",
                    "url": "https://www.rust-lang.org/",
                    "content": "Rust 是一门注重性能和可靠性的语言。"
                },
                {
                    "title": "Cargo Book",
                    "url": "https://doc.rust-lang.org/cargo/",
                    "snippet": "Cargo is Rust's package manager."
                }
            ]
        });

        let results = parse_baidu_results(&body, 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust 官方文档");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Rust 是一门注重性能和可靠性的语言。")
        );
        assert_eq!(results[1].title, "Cargo Book");
        assert_eq!(results[1].url, "https://doc.rust-lang.org/cargo/");
        assert_eq!(
            results[1].snippet.as_deref(),
            Some("Cargo is Rust's package manager.")
        );
    }

    #[test]
    fn parse_baidu_references_skips_incomplete_entries() {
        let body = json!({
            "references": [
                {"title": "No URL", "content": "missing url"},
                {"url": "https://example.com/no-title", "content": "missing title"},
                {"title": "Valid", "url": "https://example.com/valid"}
            ]
        });

        let results = parse_baidu_results(&body, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Valid");
        assert_eq!(results[0].url, "https://example.com/valid");
        assert_eq!(results[0].snippet, None);
    }

    #[test]
    fn baidu_search_payload_uses_official_search_source() {
        let payload = baidu_search_payload("Rust cargo workspace", 3);

        assert_eq!(
            payload.get("search_source").and_then(|v| v.as_str()),
            Some("baidu_search_v2")
        );
        assert_eq!(
            payload
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|messages| messages.first())
                .and_then(|message| message.get("content"))
                .and_then(|v| v.as_str()),
            Some("Rust cargo workspace")
        );
        assert_eq!(
            payload
                .get("resource_type_filter")
                .and_then(|v| v.as_array())
                .and_then(|filters| filters.first())
                .and_then(|filter| filter.get("top_k"))
                .and_then(|v| v.as_u64()),
            Some(3)
        );
    }

    #[test]
    fn serply_search_url_encodes_query_and_result_count() {
        let url = serply_search_url("rust tui & ratatui", QueryFilters::default(), 7)
            .expect("serply url");

        assert_eq!(url.host_str(), Some("api.serply.io"));
        assert_eq!(url.path(), "/v1/search");
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("q".to_string(), "rust tui & ratatui".to_string()),
                ("num".to_string(), "7".to_string()),
            ]
        );
    }

    #[test]
    fn parse_deepseek_results_reads_title_url_snippet_and_summary_fallback() {
        let body = json!({
            "results": [
                {
                    "title": "Ratatui",
                    "url": "https://ratatui.rs/",
                    "snippet": "Cook up delicious terminal user interfaces in Rust.",
                    "summary": "ignored when a snippet is present",
                    "date": "2026-10-21"
                },
                {
                    "title": "Summary only",
                    "url": "https://example.com/summary",
                    "snippet": "",
                    "summary": "Falls back to the summary field."
                },
                {
                    "title": "Missing url",
                    "snippet": "dropped because there is no url"
                },
                {
                    "url": "https://example.com/no-title",
                    "snippet": "dropped because there is no title"
                },
                "not an object",
                {
                    "title": "Beyond the cap",
                    "url": "https://example.com/beyond",
                    "snippet": "beyond max_results"
                }
            ]
        });

        let results = parse_deepseek_results(&body, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[0].title, "Ratatui");
        assert_eq!(results[0].url, "https://ratatui.rs/");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Cook up delicious terminal user interfaces in Rust.")
        );
        assert_eq!(results[0].published.as_deref(), Some("2026-10-21"));
        assert_eq!(results[1].rank, 2);
        assert_eq!(results[1].url, "https://example.com/summary");
        assert_eq!(
            results[1].snippet.as_deref(),
            Some("Falls back to the summary field.")
        );
        assert_eq!(results[1].published, None);

        assert!(parse_deepseek_results(&json!({}), 5).is_empty());
        assert!(parse_deepseek_results(&json!({"results": []}), 5).is_empty());
    }

    #[test]
    fn parse_serply_results_reads_link_and_description_and_skips_malformed_rows() {
        let body = json!({
            "results": [
                {
                    "title": "Ratatui",
                    "link": "https://ratatui.rs/",
                    "description": "Cook up delicious terminal user interfaces in Rust.",
                    "position": 1,
                    "realPosition": 1
                },
                {
                    "title": "No description",
                    "link": "https://example.com/plain",
                    "description": ""
                },
                {
                    "title": "Missing link",
                    "description": "dropped because there is no link"
                },
                "not an object",
                {
                    "title": "Fourth",
                    "link": "https://example.com/fourth",
                    "description": "beyond max_results"
                }
            ],
            "knowledge_graph": {"title": "ignored sidebar"},
            "related_questions": [{"question": "ignored"}],
            "ads": [{"title": "ignored ad", "link": "https://ads.example.com"}]
        });

        let results = parse_serply_results(&body, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Ratatui");
        assert_eq!(results[0].url, "https://ratatui.rs/");
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("Cook up delicious terminal user interfaces in Rust.")
        );
        assert_eq!(results[1].url, "https://example.com/plain");
        assert_eq!(results[1].snippet, None);

        assert!(parse_serply_results(&json!({"total": 0}), 5).is_empty());
    }

    #[test]
    fn parse_sofya_results_falls_back_to_description_for_empty_content() {
        let body = json!({
            "results": [
                {
                    "title": "Full content",
                    "url": "https://example.com/full",
                    "content": "full extracted page content",
                    "description": "unused description"
                },
                {
                    "title": "Null content",
                    "url": "https://example.com/null",
                    "content": null,
                    "description": "description for null content"
                },
                {
                    "title": "Empty content",
                    "url": "https://example.com/empty",
                    "content": "",
                    "description": "description for empty content"
                },
                {
                    "title": "Whitespace content",
                    "url": "https://example.com/blank",
                    "content": "   ",
                    "description": "description for blank content"
                },
                {
                    "title": "No snippet",
                    "url": "https://example.com/no-snippet"
                }
            ]
        });

        let results = parse_sofya_results(&body, 10);

        assert_eq!(results.len(), 5);
        assert_eq!(
            results[0].snippet.as_deref(),
            Some("full extracted page content")
        );
        assert_eq!(
            results[1].snippet.as_deref(),
            Some("description for null content")
        );
        assert_eq!(
            results[2].snippet.as_deref(),
            Some("description for empty content")
        );
        assert_eq!(
            results[3].snippet.as_deref(),
            Some("description for blank content")
        );
        assert_eq!(results[4].snippet, None);
    }

    #[test]
    fn tavily_metaso_and_volcengine_payloads_use_normalized_entry_shape() {
        let tavily = parse_tavily_results(
            &json!({"results": [{
                "title": " Tavily result ",
                "url": "https://tavily.example/result",
                "content": " content "
            }]}),
            5,
        );
        let metaso = parse_metaso_results(
            &json!({"webpages": [{
                "title": " Metaso result ",
                "link": "https://metaso.example/result",
                "summary": " summary "
            }]}),
            5,
        );
        let volcengine = parse_volcengine_results(
            r#"{"results":[{"title":"Volcengine result","url":"https://volc.example/result","snippet":"summary"}]}"#,
            5,
        );

        for (entries, title, snippet) in [
            (tavily, "Tavily result", "content"),
            (metaso, "Metaso result", "summary"),
            (volcengine, "Volcengine result", "summary"),
        ] {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].title, title);
            assert_eq!(entries[0].snippet.as_deref(), Some(snippet));
        }
    }

    #[tokio::test]
    async fn firecrawl_keyless_request_is_headerless_and_keyed_request_is_explicit() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"web": [{
                    "title": "Firecrawl result",
                    "url": "https://example.com/firecrawl",
                    "description": "x".repeat(1_200)
                }]}
            })))
            .mount(&server)
            .await;
        let endpoint = format!("{}/v2/search", server.uri());
        let (entries, mode) = WebSearchTool
            .run_firecrawl_search_at(
                &endpoint,
                "codewhale",
                QueryFilters::default(),
                5,
                5_000,
                None,
            )
            .await
            .expect("keyless Firecrawl search");
        WebSearchTool
            .run_firecrawl_search_at(
                &endpoint,
                "codewhale",
                QueryFilters::default(),
                5,
                5_000,
                Some("fc-secret"),
            )
            .await
            .expect("authenticated Firecrawl search");
        let requests = server.received_requests().await.expect("recorded requests");
        let payload: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("request JSON");

        assert_eq!(entries[0].title, "Firecrawl result");
        assert_eq!(entries[0].url, "https://example.com/firecrawl");
        assert_eq!(
            entries[0].snippet.as_deref().unwrap().chars().count(),
            1_000
        );
        assert_eq!(mode, "Firecrawl keyless");
        assert!(requests[0].headers.get("authorization").is_none());
        assert_eq!(requests[1].headers["authorization"], "Bearer fc-secret");
        assert!(payload.get("integration").is_none());
        assert_eq!(payload["sources"][0]["type"], "web");
    }

    #[tokio::test]
    async fn firecrawl_keyless_rate_limit_is_actionable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/search"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let error = WebSearchTool
            .run_firecrawl_search_at(
                &format!("{}/v2/search", server.uri()),
                "quota",
                QueryFilters::default(),
                5,
                5_000,
                None,
            )
            .await
            .expect_err("429 must be actionable");
        assert!(error.to_string().contains("keyless quota is exhausted"));
        assert!(error.to_string().contains("FIRECRAWL_API_KEY"));
    }

    #[test]
    fn volcengine_extract_text_skips_non_text_content_blocks() {
        let body = json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "reasoning", "summary": "thinking first"},
                        {"type": "output_text", "text": "{\"results\":[]}"}
                    ]
                }
            ]
        });

        assert_eq!(
            volcengine_extract_text(&body).as_deref(),
            Some("{\"results\":[]}")
        );
    }

    #[tokio::test]
    async fn volcengine_model_search_waits_for_runtime_chat_ownership() {
        let ownership = crate::client::acquire_runtime_chat_inference_ownership().await;
        let mut participant =
            tokio::spawn(async { acquire_model_backed_search_inference_participant().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut participant)
                .await
                .is_err(),
            "model-backed web search must wait behind Runtime Chat ownership"
        );
        drop(ownership);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(1), participant)
            .await
            .expect("model-backed search resumes after relay settlement")
            .expect("model-backed search participant task");
        drop(permit);
    }

    #[tokio::test]
    async fn baidu_provider_without_api_key_surfaces_clear_error_not_silent_fallback() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _env = crate::test_support::lock_test_env();
        let _baidu_key = crate::test_support::EnvVarGuard::remove("BAIDU_SEARCH_API_KEY");

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Baidu;
        ctx.search_api_key = None;
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx)
            .await
            .expect_err("missing api_key must surface as ToolError");

        let msg = err.to_string();
        assert!(
            msg.contains("Baidu") && msg.contains("API key"),
            "error must name the provider and missing key; got `{msg}`"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn serply_missing_key_is_fail_closed_inside_the_backend_chain() {
        use crate::tools::spec::ToolContext;

        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("SERPLY_API_KEY");
        unsafe { std::env::remove_var("SERPLY_API_KEY") };

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_api_key = None;
        let err = WebSearchTool
            .run_serply_search("anything", QueryFilters::default(), 5, 1_000, &ctx)
            .await
            .expect_err("missing api_key must be an error");

        match prev {
            Some(value) => unsafe { std::env::set_var("SERPLY_API_KEY", value) },
            None => unsafe { std::env::remove_var("SERPLY_API_KEY") },
        }

        // A configured Serply route that reaches the adapter after a failed
        // provider-native attempt must stop the chain, not degrade to DuckDuckGo.
        assert!(
            matches!(
                err.error,
                crate::tools::spec::ToolError::InvalidInput { .. }
            ),
            "missing key must be classified fail-closed; got `{err:?}`"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn serply_provider_without_api_key_surfaces_clear_error_not_silent_fallback() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("SERPLY_API_KEY");
        unsafe { std::env::remove_var("SERPLY_API_KEY") };

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Serply;
        ctx.search_api_key = None;
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx)
            .await
            .expect_err("missing api_key must surface as ToolError");

        match prev {
            Some(value) => unsafe { std::env::set_var("SERPLY_API_KEY", value) },
            None => unsafe { std::env::remove_var("SERPLY_API_KEY") },
        }

        let msg = err.to_string();
        assert!(
            msg.contains("Serply") && msg.contains("API key"),
            "error must name the provider and missing key; got `{msg}`"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn sofya_provider_without_api_key_surfaces_clear_error_not_silent_fallback() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("SOFYA_API_KEY");
        unsafe { std::env::remove_var("SOFYA_API_KEY") };

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Sofya;
        ctx.search_api_key = None;
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx)
            .await
            .expect_err("missing api_key must surface as ToolError");

        match prev {
            Some(value) => unsafe { std::env::set_var("SOFYA_API_KEY", value) },
            None => unsafe { std::env::remove_var("SOFYA_API_KEY") },
        }

        let msg = err.to_string();
        assert!(
            msg.contains("Sofya") && msg.contains("API key"),
            "error must name the provider and missing key; got `{msg}`"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn volcengine_provider_without_api_key_lists_supported_env_fallbacks() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _guard = crate::test_support::lock_test_env();
        let prev_volc = std::env::var_os("VOLCENGINE_API_KEY");
        let prev_volc_ark = std::env::var_os("VOLCENGINE_ARK_API_KEY");
        let prev_ark = std::env::var_os("ARK_API_KEY");
        unsafe {
            std::env::remove_var("VOLCENGINE_API_KEY");
            std::env::remove_var("VOLCENGINE_ARK_API_KEY");
            std::env::remove_var("ARK_API_KEY");
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Volcengine;
        ctx.search_api_key = None;
        let err = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx)
            .await
            .expect_err("missing api_key must surface as ToolError");

        match prev_volc {
            Some(value) => unsafe { std::env::set_var("VOLCENGINE_API_KEY", value) },
            None => unsafe { std::env::remove_var("VOLCENGINE_API_KEY") },
        }
        match prev_volc_ark {
            Some(value) => unsafe { std::env::set_var("VOLCENGINE_ARK_API_KEY", value) },
            None => unsafe { std::env::remove_var("VOLCENGINE_ARK_API_KEY") },
        }
        match prev_ark {
            Some(value) => unsafe { std::env::set_var("ARK_API_KEY", value) },
            None => unsafe { std::env::remove_var("ARK_API_KEY") },
        }

        let msg = err.to_string();
        assert!(msg.contains("Volcengine") && msg.contains("API key"));
        assert!(msg.contains("VOLCENGINE_API_KEY"));
        assert!(msg.contains("VOLCENGINE_ARK_API_KEY"));
        assert!(msg.contains("ARK_API_KEY"));
        assert!(!msg.contains("DEEPSEEK_SEARCH_API_KEY"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn metaso_provider_without_api_key_fails_closed_before_fallback() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _guard = crate::test_support::lock_test_env();
        let previous = std::env::var_os("METASO_API_KEY");
        unsafe { std::env::remove_var("METASO_API_KEY") };

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Metaso;
        ctx.search_api_key = None;
        let error = WebSearchTool
            .execute(json!({"query": "anything"}), &ctx)
            .await
            .expect_err("missing Metaso key must fail before the fallback chain");

        match previous {
            Some(value) => unsafe { std::env::set_var("METASO_API_KEY", value) },
            None => unsafe { std::env::remove_var("METASO_API_KEY") },
        }

        let message = error.to_string();
        assert!(
            message.contains("Metaso")
                && message.contains("API key")
                && message.contains("METASO_API_KEY"),
            "got `{message}`"
        );
        assert!(
            !message.contains("duckduckgo"),
            "missing configuration must not cross providers: `{message}`"
        );
    }

    #[test]
    fn duckduckgo_compatible_url_uses_custom_base_url_and_preserves_query() {
        let (url, host) = duckduckgo_search_url(
            Some("https://search.internal.example/html/?region=us"),
            "rust async",
            None,
        )
        .expect("custom duckduckgo-compatible url");

        assert_eq!(host, "search.internal.example");
        assert_eq!(
            url,
            "https://search.internal.example/html/?region=us&q=rust+async"
        );
    }

    #[test]
    fn custom_duckduckgo_endpoint_disables_public_bing_fallback() {
        assert!(super::duckduckgo_allows_bing_fallback(None));
        assert!(super::duckduckgo_allows_bing_fallback(Some("   ")));
        assert!(!super::duckduckgo_allows_bing_fallback(Some(
            "https://search.internal.example/html/"
        )));
    }

    #[test]
    fn bing_locale_params_follow_explicit_locale() {
        let (params, accept_language) = scrape_locale_params(Some("zh-CN"), "rust async");
        assert_eq!(
            params,
            vec![
                ("mkt".to_string(), "zh-CN".to_string()),
                ("setlang".to_string(), "zh-Hans".to_string()),
            ]
        );
        assert_eq!(accept_language, "zh-CN,zh;q=0.9,en;q=0.8");

        let (params, accept_language) = scrape_locale_params(Some("en-US"), "rust async");
        assert_eq!(
            params,
            vec![
                ("mkt".to_string(), "en-US".to_string()),
                ("setlang".to_string(), "en".to_string()),
            ]
        );
        assert_eq!(accept_language, "en-US,en;q=0.9");

        // Bing's setlang table keys the Chinese script off the region:
        // Taiwan/Hong Kong/Macao are Traditional, everything else (including
        // the Han-heuristic default) is Simplified.
        let (params, _) = scrape_locale_params(Some("zh-TW"), "rust async");
        assert_eq!(params[1].1, "zh-Hant");
        let (params, _) = scrape_locale_params(Some("zh-HK"), "rust async");
        assert_eq!(params[1].1, "zh-Hant");
    }

    #[test]
    fn bing_locale_params_fall_back_to_china_market_for_han_queries() {
        // Without this fallback a locale-less Chinese query used to get no
        // market hint plus an English Accept-Language, and Bing served
        // unrelated Japanese results.
        let (params, accept_language) = scrape_locale_params(None, "凹语言 编程");
        assert_eq!(
            params,
            vec![
                ("mkt".to_string(), "zh-CN".to_string()),
                ("setlang".to_string(), "zh-Hans".to_string()),
            ]
        );
        assert_eq!(accept_language, "zh-CN,zh;q=0.9,en;q=0.8");
    }

    #[test]
    fn bing_locale_params_keep_english_default_without_locale_signal() {
        let (params, accept_language) = scrape_locale_params(None, "rust async");
        assert!(params.is_empty());
        assert_eq!(accept_language, "en-US,en;q=0.9");
    }

    #[test]
    fn bing_locale_params_normalize_underscore_separators() {
        // The model often writes `zh_CN`; the underscore must not leak into
        // the `mkt` parameter or the `Accept-Language` ranges.
        let (params, accept_language) = scrape_locale_params(Some("zh_CN"), "rust async");
        assert_eq!(
            params,
            vec![
                ("mkt".to_string(), "zh-CN".to_string()),
                ("setlang".to_string(), "zh-Hans".to_string()),
            ]
        );
        assert_eq!(accept_language, "zh-CN,zh;q=0.9,en;q=0.8");
    }

    #[test]
    fn bing_setlang_keeps_an_explicit_script_subtag() {
        // `zh-Hant-TW` must not degrade to Simplified: the explicit script
        // subtag wins over the region-derived default.
        let (params, _) = scrape_locale_params(Some("zh-Hant-TW"), "rust async");
        assert_eq!(params[1].1, "zh-Hant");
        let (params, _) = scrape_locale_params(Some("zh-Hans-CN"), "rust async");
        assert_eq!(params[1].1, "zh-Hans");
        let (params, _) = scrape_locale_params(Some("zh-Hant"), "rust async");
        assert_eq!(params[1].1, "zh-Hant");
    }

    #[test]
    fn ddg_locale_ignored_only_when_an_explicit_locale_yields_no_region() {
        // Mapped regions are honored.
        assert!(!ddg_locale_was_ignored(Some("zh-CN"), Some("zh-CN")));
        // Unmapped but well-formed regions send no `kl`: the receipt must
        // not claim the knob was honored.
        assert!(ddg_locale_was_ignored(Some("fr-FR"), Some("fr-FR")));
        // Malformed values resolve no market at all.
        assert!(ddg_locale_was_ignored(Some("zh CN"), None));
        // Malformed values stay ignored even when a pure-Han query makes
        // scrape_market fall back to the zh-CN market — the shape check is
        // independent of the market resolution, matching the Bing leg.
        assert!(ddg_locale_was_ignored(Some("zh CN"), Some("zh-CN")));
        assert!(ddg_locale_was_ignored(Some("中文"), Some("zh-CN")));
        // No explicit locale: the Han fallback is the tool's own choice,
        // never a dropped knob.
        assert!(!ddg_locale_was_ignored(None, Some("zh-CN")));
        assert!(!ddg_locale_was_ignored(None, None));
    }

    #[test]
    fn ddg_malformed_locale_with_han_query_receipt_stays_ignored() {
        // End-to-end across the DDG leg: a malformed locale plus a pure-Han
        // query resolves the zh-CN market via the Han heuristic, but the
        // receipt must keep KnobIgnored (honored.locale=false), exactly what
        // the Bing leg reports for the same input.
        let locale = Some("中文");
        let han_query = "凹语言 编程";
        let market = scrape_market(locale, han_query);
        assert_eq!(market.as_deref(), Some("zh-CN"));
        let degraded: Vec<DegradedReason> = if ddg_locale_was_ignored(locale, market.as_deref()) {
            vec![DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            }]
        } else {
            Vec::new()
        };
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: Vec::new(),
            degraded,
            note: None,
        };
        let query = SearchQuery::new(
            han_query.to_string(),
            5,
            None,
            Vec::new(),
            locale.map(str::to_string),
        );
        let capabilities = QueryCapabilities {
            max_results: CapabilityState::Supported,
            recency: CapabilityState::Unsupported,
            domains: CapabilityState::Unsupported,
            locale: CapabilityState::Supported,
            published_date: CapabilityState::Unknown,
        };
        let response = finalize_search_response(query, capabilities, raw, Instant::now());
        assert!(
            !response.receipt.honored.locale,
            "a malformed locale must be receipted as ignored even when the \
             Han heuristic supplies a market"
        );
        assert!(response.receipt.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale
            }
        )));

        // fr-FR (well-formed, unmapped on the verified kl list) stays
        // ignored on the DDG leg, as before the fix.
        let market = scrape_market(Some("fr-FR"), "rust async");
        assert_eq!(market.as_deref(), Some("fr-FR"));
        assert!(ddg_locale_was_ignored(Some("fr-FR"), market.as_deref()));

        // A mapped, well-formed value (ja-JP) is honored.
        let market = scrape_market(Some("ja-JP"), "rust async");
        assert_eq!(market.as_deref(), Some("ja-JP"));
        assert!(!ddg_locale_was_ignored(Some("ja-JP"), market.as_deref()));
    }

    #[test]
    fn bing_locale_ignored_only_for_malformed_explicit_values() {
        assert!(!bing_locale_was_ignored(Some("zh-CN")));
        assert!(!bing_locale_was_ignored(None));
        assert!(bing_locale_was_ignored(Some("zh CN")));
        assert!(bing_locale_was_ignored(Some("-CN")));
    }

    #[test]
    fn han_detection_covers_han_only_and_ignores_other_cjk_scripts() {
        assert!(super::query_contains_han("学 rust"));
        assert!(super::query_contains_han("\u{3400}")); // Extension A
        assert!(!super::query_contains_han("rust async 123"));
        assert!(!super::query_contains_han("ルスト programming")); // Katakana
        assert!(!super::query_contains_han("러스트 programming")); // Hangul
    }

    #[test]
    fn duckduckgo_url_adds_kl_for_resolved_market() {
        let (url, _) = duckduckgo_search_url(None, "凹语言 编程", Some("zh-CN")).expect("ddg url");
        let parsed = reqwest::Url::parse(&url).expect("valid url");
        assert_eq!(
            parsed.query_pairs().find(|(key, _)| key == "kl").unwrap().1,
            "cn-zh"
        );

        // A locale with no verified kl pair sends no kl at all rather than
        // an off-list guess.
        let (url, _) = duckduckgo_search_url(None, "rust async", Some("fr-FR")).expect("ddg url");
        let parsed = reqwest::Url::parse(&url).expect("valid url");
        assert!(parsed.query_pairs().all(|(key, _)| key != "kl"));

        let (url, _) = duckduckgo_search_url(None, "rust async", None).expect("ddg url");
        let parsed = reqwest::Url::parse(&url).expect("valid url");
        assert!(parsed.query_pairs().all(|(key, _)| key != "kl"));
    }

    #[test]
    fn ddg_region_param_translates_only_verified_pairs() {
        // DuckDuckGo's kl list is fixed and non-systematic; only pairs
        // verified against that list are translated (China, US, Japan,
        // Korea, Taiwan, Hong Kong). A mechanical reversal of a BCP 47 tag
        // produced off-list junk such as `jp-ja` (the real value is
        // `jp-jp`), so anything unverified sends no kl at all.
        assert_eq!(ddg_region_param("zh-CN").as_deref(), Some("cn-zh"));
        assert_eq!(ddg_region_param("en_US").as_deref(), Some("us-en"));
        assert_eq!(ddg_region_param("ja-JP").as_deref(), Some("jp-jp"));
        assert_eq!(ddg_region_param("ko-KR").as_deref(), Some("kr-kr"));
        assert_eq!(ddg_region_param("zh-TW").as_deref(), Some("tw-tzh"));
        assert_eq!(ddg_region_param("zh-HK").as_deref(), Some("hk-tzh"));
        // Unverified locales and script/bare tags send no region hint —
        // the no-kl request is the historical shape and strictly safer
        // than guessing an off-list value.
        assert_eq!(ddg_region_param("fr-FR"), None);
        assert_eq!(ddg_region_param("zh-Hans"), None);
        assert_eq!(ddg_region_param("zh"), None);
        // Script+region Chinese tags reduce to their verified region pair:
        // the schema teaches BCP 47 where the script subtag is best
        // practice for Chinese.
        assert_eq!(ddg_region_param("zh-Hans-CN").as_deref(), Some("cn-zh"));
        assert_eq!(ddg_region_param("zh-Hant-TW").as_deref(), Some("tw-tzh"));
        assert_eq!(ddg_region_param("zh-Hant-HK").as_deref(), Some("hk-tzh"));
    }

    #[test]
    fn bing_fallback_rederives_the_locale_flag_from_bings_rule() {
        // A shape-valid locale the DuckDuckGo leg could not map (no kl
        // pair) is carried by the Bing fallback through mkt/setlang, so the
        // stale DDG-leg flag must not keep the receipt claiming the knob
        // was ignored.
        let mut degraded = vec![DegradedReason::KnobIgnored {
            knob: QueryKnob::Locale,
        }];
        prune_fallback_locale_ignored(&mut degraded, Some("fr-FR"));
        assert!(
            !degraded.iter().any(|reason| matches!(
                reason,
                DegradedReason::KnobIgnored {
                    knob: QueryKnob::Locale,
                }
            )),
            "the DDG-leg ignored flag must not survive the Bing fallback: \
             {degraded:?}"
        );
        // An implausible tag stays ignored on the fallback too.
        let mut degraded = vec![DegradedReason::KnobIgnored {
            knob: QueryKnob::Locale,
        }];
        prune_fallback_locale_ignored(&mut degraded, Some("zh CN"));
        assert!(degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            }
        )));
    }

    #[test]
    fn ddg_accept_language_unifies_with_bing_rule() {
        assert_eq!(scrape_accept_language(None), "en-US,en;q=0.9");
        assert_eq!(
            scrape_accept_language(Some("zh-CN")),
            "zh-CN,zh;q=0.9,en;q=0.8"
        );
        // A bare-language market (reachable through an explicit `locale: en`)
        // emits a single range and, like every English market, never appends
        // a duplicate `en` fallback.
        assert_eq!(scrape_accept_language(Some("en")), "en;q=0.9");
    }

    #[test]
    fn implausible_locale_values_are_treated_as_no_locale() {
        // The locale reaches URLs and the Accept-Language header, so a
        // malformed model-supplied value keeps the historical no-market
        // request instead of becoming a broken tag or header value.
        assert_eq!(scrape_market(Some("-CN"), "rust async"), None);
        assert_eq!(scrape_market(Some("zh CN"), "rust async"), None);
        assert_eq!(scrape_market(Some(""), "rust async"), None);
        assert_eq!(scrape_market(Some("zh=CN"), "rust async"), None);
        assert_eq!(scrape_market(Some("  "), "rust async"), None);
        // Digits are not a language: the primary subtag must be alphabetic,
        // or the value would be transmitted (and receipted as honored)
        // against schema text promising malformed values are ignored.
        assert_eq!(scrape_market(Some("12345"), "rust async"), None);
        assert_eq!(scrape_market(Some("en-123456789"), "rust async"), None);
        // Well-formed tags survive the shape check verbatim.
        assert_eq!(
            scrape_market(Some("zh-TW"), "rust async").as_deref(),
            Some("zh-TW")
        );
        // The Han fallback is untouched by the shape check.
        assert_eq!(scrape_market(None, "凹语言 编程").as_deref(), Some("zh-CN"));
    }

    #[test]
    fn oversized_primary_locale_is_ignored_even_with_han_fallback() {
        let locale = Some("abcdefghij-US");
        assert_eq!(scrape_market(locale, "rust async"), None);
        assert!(bing_locale_was_ignored(locale));
        assert!(ddg_locale_was_ignored(locale, None));

        // A query-derived market must not hide the invalid explicit knob.
        let market = scrape_market(locale, "中国 的 首都");
        assert_eq!(market.as_deref(), Some("zh-CN"));
        assert!(ddg_locale_was_ignored(locale, market.as_deref()));
    }

    #[test]
    fn han_fallback_vetoes_kana_and_hangul_queries() {
        // A Han-bearing query with kana is Japanese: pushing it onto the
        // zh-CN market would be the inverse of the drift the fallback
        // exists to fix, so it keeps the no-market request.
        assert_eq!(scrape_market(None, "東京の天気"), None);
        assert_eq!(scrape_market(None, "日本の地図"), None);
        // Hangul-bearing queries likewise never take the fallback.
        assert_eq!(scrape_market(None, "러스트 프로그래밍"), None);
        // Purely Han queries (indistinguishable from Chinese) and an
        // explicit locale keep the existing behavior.
        assert_eq!(
            scrape_market(None, "中国 的 首都").as_deref(),
            Some("zh-CN")
        );
        assert_eq!(
            scrape_market(Some("ja-JP"), "東京の天気").as_deref(),
            Some("ja-JP")
        );
    }

    #[test]
    fn searxng_url_uses_search_path_and_json_format() {
        let (url, host) = searxng_search_url(
            Some("https://search.example/"),
            "rust async",
            QueryFilters::default(),
        )
        .expect("searxng url");
        let parsed = reqwest::Url::parse(&url).expect("valid url");
        assert_eq!(host, "search.example");
        assert_eq!(parsed.path(), "/search");
        assert_eq!(
            parsed.query_pairs().find(|(key, _)| key == "q").unwrap().1,
            "rust async"
        );
        assert_eq!(
            parsed
                .query_pairs()
                .find(|(key, _)| key == "format")
                .unwrap()
                .1,
            "json"
        );

        let (subpath_url, _) = searxng_search_url(
            Some("https://search.example/searxng?language=en"),
            "codewhale",
            QueryFilters::default(),
        )
        .expect("searxng subpath url");
        let parsed = reqwest::Url::parse(&subpath_url).expect("valid subpath url");
        assert_eq!(parsed.path(), "/searxng/search");
        assert_eq!(
            parsed
                .query_pairs()
                .find(|(key, _)| key == "language")
                .unwrap()
                .1,
            "en"
        );

        let (search_url, _) = searxng_search_url(
            Some("https://search.example/searxng/search"),
            "codewhale",
            QueryFilters::default(),
        )
        .expect("searxng search endpoint");
        assert_eq!(
            reqwest::Url::parse(&search_url)
                .expect("valid search url")
                .path(),
            "/searxng/search"
        );
    }

    #[test]
    fn searxng_parser_normalizes_results() {
        let parsed = json!({
            "results": [
                {
                    "title": " Rust async ",
                    "url": " https://example.com/rust ",
                    "content": " Result content "
                },
                {
                    "title": "Empty snippet",
                    "url": "https://example.com/empty",
                    "content": "   ",
                    "snippet": " Fallback snippet "
                },
                {
                    "title": "",
                    "url": "https://example.com/missing-title",
                    "content": "ignored"
                },
                {
                    "title": "Missing URL",
                    "content": "ignored"
                }
            ]
        });

        let results = parse_searxng_results(&parsed, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust async");
        assert_eq!(results[0].url, "https://example.com/rust");
        assert_eq!(results[0].snippet.as_deref(), Some("Result content"));
        assert_eq!(results[1].snippet.as_deref(), Some("Fallback snippet"));
    }

    #[test]
    fn searxng_score_reads_floats_integers_strings_and_clamps_junk() {
        assert_eq!(searxng_score(&json!({"score": 0.75})), 0.75);
        assert_eq!(searxng_score(&json!({"score": 1})), 1.0);
        assert_eq!(searxng_score(&json!({"score": " 2.5 "})), 2.5);
        assert_eq!(searxng_score(&json!({"score": "-1.5"})), -1.5);
        assert_eq!(searxng_score(&json!({})), 0.0);
        assert_eq!(searxng_score(&json!({"score": null})), 0.0);
        assert_eq!(searxng_score(&json!({"score": true})), 0.0);
        assert_eq!(searxng_score(&json!({"score": ""})), 0.0);
        assert_eq!(searxng_score(&json!({"score": "not-a-number"})), 0.0);
        assert_eq!(searxng_score(&json!({"score": {"nested": 1.0}})), 0.0);
        assert_eq!(
            searxng_score(&json!({"score": "NaN"})),
            0.0,
            "a non-finite score must not reach the sort"
        );
        assert_eq!(
            searxng_score(&json!({"score": "inf"})),
            0.0,
            "an infinite score must not outrank every finite row"
        );
    }

    #[test]
    fn searxng_parser_sorts_by_descending_score() {
        // The strongest row is last in the instance's own order; only the
        // score sort can promote it.
        let parsed = json!({
            "results": [
                {"title": "Low", "url": "https://example.com/low", "score": 0.25},
                {"title": "Middle", "url": "https://example.com/mid", "score": 1},
                {"title": "High", "url": "https://example.com/high", "score": "4.5"},
                {"title": "Zero", "url": "https://example.com/zero", "score": 0.0}
            ]
        });

        let titles: Vec<String> = parse_searxng_results(&parsed, 10)
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert_eq!(titles, ["High", "Middle", "Low", "Zero"]);
    }

    #[test]
    fn searxng_parser_keeps_input_order_for_equal_scores() {
        let parsed = json!({
            "results": [
                {"title": "First", "url": "https://example.com/1", "score": 1.5},
                {"title": "Second", "url": "https://example.com/2", "score": 1.5},
                {"title": "Third", "url": "https://example.com/3", "score": 1.5},
                {"title": "Lower", "url": "https://example.com/4", "score": 1.4}
            ]
        });

        let titles: Vec<String> = parse_searxng_results(&parsed, 10)
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert_eq!(titles, ["First", "Second", "Third", "Lower"]);
    }

    #[test]
    fn searxng_parser_sorts_missing_or_invalid_scores_last() {
        let parsed = json!({
            "results": [
                {"title": "No score", "url": "https://example.com/none"},
                {
                    "title": "Garbage",
                    "url": "https://example.com/garbage",
                    "score": "not-a-number"
                },
                {"title": "NaN string", "url": "https://example.com/nan", "score": "NaN"},
                {"title": "Infinite string", "url": "https://example.com/inf", "score": "inf"},
                {"title": "Boolean", "url": "https://example.com/bool", "score": true},
                {"title": "Scored", "url": "https://example.com/scored", "score": 0.5}
            ]
        });

        let results = parse_searxng_results(&parsed, 10);
        let titles: Vec<&str> = results.iter().map(|entry| entry.title.as_str()).collect();
        // Every row with a title and a URL survives. Unusable scores read as
        // 0.0 and keep their input order behind the one scored row.
        assert_eq!(
            titles,
            [
                "Scored",
                "No score",
                "Garbage",
                "NaN string",
                "Infinite string",
                "Boolean"
            ]
        );
    }

    #[test]
    fn searxng_parser_caps_after_score_sort() {
        // A `take` before the sort would drop "Strong"; the cap must apply to
        // the ranked list instead.
        let parsed = json!({
            "results": [
                {"title": "Weak one", "url": "https://example.com/1", "score": 0.1},
                {"title": "Weak two", "url": "https://example.com/2", "score": 0.2},
                {"title": "Strong", "url": "https://example.com/3", "score": 9.0}
            ]
        });

        let results = parse_searxng_results(&parsed, 2);
        assert_eq!(results.len(), 2, "max_results caps the ranked list");
        assert_eq!(results[0].title, "Strong");
        assert_eq!(results[1].title, "Weak two");
    }

    #[tokio::test]
    async fn searxng_provider_requires_base_url() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = None;

        let err = WebSearchTool
            .execute(json!({"query": "rust async"}), &ctx)
            .await
            .expect_err("searxng requires explicit base_url");
        let msg = err.to_string();
        assert!(
            matches!(err, crate::tools::spec::ToolError::InvalidInput { .. }),
            "missing base_url is a configuration gap, not a transport failure: {err:?}"
        );
        assert!(
            msg.contains("SearXNG")
                && msg.contains("base_url")
                && msg.contains("no public instance"),
            "got `{msg}`"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn missing_provider_key_fails_closed_as_not_configured() {
        use crate::tools::spec::{ToolContext, ToolError, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let _guard = crate::test_support::lock_test_env();
        let prev_tavily = std::env::var_os("TAVILY_API_KEY");
        // "both keys empty" must mean *both*: an ambient key from the
        // operator's shell would otherwise satisfy the Tavily arm.
        unsafe { std::env::remove_var("TAVILY_API_KEY") };

        for provider in [SearchProvider::Tavily, SearchProvider::Bocha] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let mut ctx = ToolContext::new(tmp.path().to_path_buf());
            ctx.search_provider = provider;
            ctx.search_api_key = None;

            let error = WebSearchTool
                .execute(json!({"query": "needs configuration"}), &ctx)
                .await
                .expect_err("a keyed provider without an API key must fail closed");
            assert!(
                matches!(error, ToolError::InvalidInput { .. }),
                "config gaps must stay distinguishable from transport failures: {error:?}"
            );
            let message = error.to_string();
            assert!(message.contains("is not configured"), "got `{message}`");
            assert!(message.contains("api_key"), "got `{message}`");
        }

        // Sibling case: only `TAVILY_API_KEY` is set. Explicit Tavily is
        // configured, and the copy that names both sources is the one the
        // operator never sees here.
        unsafe { std::env::set_var("TAVILY_API_KEY", "tvly-test-env-only") };
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Tavily;
        ctx.search_api_key = None;
        let preflight = super::preflight_search_provider(&ctx);

        match prev_tavily {
            Some(value) => unsafe { std::env::set_var("TAVILY_API_KEY", value) },
            None => unsafe { std::env::remove_var("TAVILY_API_KEY") },
        }

        assert!(
            preflight.is_ok(),
            "TAVILY_API_KEY alone must configure explicit Tavily: {preflight:?}"
        );
    }

    #[test]
    fn tavily_key_from_prefers_dedicated_env_and_prefix_gates_only_the_generic_key() {
        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("TAVILY_API_KEY");

        unsafe { std::env::set_var("TAVILY_API_KEY", "tvly-a") };
        assert_eq!(
            codewhale_runtime::config::tavily_key_from(Some("tvly-b")).as_deref(),
            Some("tvly-a"),
            "the dedicated env wins over the shared generic slot"
        );
        assert_eq!(
            codewhale_runtime::config::tavily_env_key().as_deref(),
            Some("tvly-a")
        );

        // A dedicated env key is never prefix-checked.
        unsafe { std::env::set_var("TAVILY_API_KEY", "not-a-tvly-prefix") };
        assert_eq!(
            codewhale_runtime::config::tavily_key_from(None).as_deref(),
            Some("not-a-tvly-prefix")
        );

        unsafe { std::env::set_var("TAVILY_API_KEY", "   ") };
        assert_eq!(codewhale_runtime::config::tavily_env_key(), None);

        unsafe { std::env::remove_var("TAVILY_API_KEY") };
        assert_eq!(
            codewhale_runtime::config::tavily_key_from(Some("tvly-b")).as_deref(),
            Some("tvly-b")
        );
        assert_eq!(
            codewhale_runtime::config::tavily_key_from(Some("doctor-offline-search-sentinel")),
            None,
            "a non-`tvly-` generic key must never autodetect Tavily"
        );
        assert_eq!(
            codewhale_runtime::config::tavily_key_from(Some("   ")),
            None
        );
        assert!(codewhale_runtime::config::looks_like_tavily_key(" tvly-x "));
        assert!(!codewhale_runtime::config::looks_like_tavily_key(
            "fc-live-test"
        ));

        match prev {
            Some(value) => unsafe { std::env::set_var("TAVILY_API_KEY", value) },
            None => unsafe { std::env::remove_var("TAVILY_API_KEY") },
        }
    }

    #[tokio::test]
    async fn searxng_search_returns_json_results() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "rust async"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "title": "Rust async",
                        "url": "https://example.com/rust",
                        "content": "Async Rust result"
                    }
                ]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let result = WebSearchTool
            .execute(json!({"query": "rust async"}), &ctx)
            .await
            .expect("searxng endpoint should return results");
        let value: serde_json::Value =
            serde_json::from_str(&result.content).expect("web search json response");

        assert_eq!(value["source"].as_str(), Some("searxng"));
        assert_eq!(value["count"].as_u64(), Some(1));
        assert_eq!(value["results"][0]["rank"].as_u64(), Some(1));
        assert_eq!(value["results"][0]["domain"], "example.com");
        assert_eq!(value["receipt"]["backend"], "searxng");
        assert_eq!(
            value["receipt"]["backend_detail"].as_str(),
            Some("127.0.0.1")
        );
        assert!(
            value["message"]
                .as_str()
                .expect("message")
                .contains("Backend: searxng at")
        );
    }

    #[tokio::test]
    async fn searxng_honors_recency_and_locale_and_post_filters_domains() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "fresh rust"))
            .and(query_param("format", "json"))
            .and(query_param("time_range", "week"))
            .and(query_param("language", "en-US"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"title": "Keep", "url": "https://docs.example.com/rust", "content": "kept"},
                    {"title": "Drop", "url": "https://other.test/rust", "content": "dropped"}
                ]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let result = WebSearchTool
            .execute(
                json!({
                    "query": "fresh rust",
                    "recency": "week",
                    "domains": ["example.com"],
                    "locale": "en-US"
                }),
                &ctx,
            )
            .await
            .expect("structured query should execute");
        let value: serde_json::Value =
            serde_json::from_str(&result.content).expect("web search json response");

        assert_eq!(value["count"], 1);
        assert_eq!(value["results"][0]["domain"], "docs.example.com");
        assert_eq!(value["receipt"]["honored"]["domains"], true);
        assert_eq!(value["receipt"]["honored"]["recency"], true);
        assert_eq!(value["receipt"]["honored"]["locale"], true);
        let degraded = value["receipt"]["degraded"]
            .as_array()
            .expect("degraded receipt array");
        assert!(
            degraded
                .iter()
                .any(|item| { item["kind"] == "post_filtered" && item["knob"] == "domains" })
        );
        assert!(
            !degraded.iter().any(|item| item["kind"] == "knob_ignored"),
            "SearXNG forwards both knobs: {degraded:?}"
        );
    }

    #[test]
    fn finalize_reports_a_result_shortfall_instead_of_a_honored_capability() {
        let query = SearchQuery::new("current release".to_string(), 8, None, Vec::new(), None);
        let raw = BackendSearch {
            backend: BackendId::ProviderNative,
            source: "provider-native/deepseek/deepseek-flash".to_string(),
            backend_detail: Some("api.deepseek.com".to_string()),
            results: vec![SearchResult::new(
                1,
                "Only source".to_string(),
                "https://example.com/only".to_string(),
                None,
                None,
            )],
            degraded: Vec::new(),
            note: None,
        };
        let response = finalize_search_response(
            query,
            QueryCapabilities {
                max_results: CapabilityState::Supported,
                recency: CapabilityState::Unsupported,
                domains: CapabilityState::Unsupported,
                locale: CapabilityState::Unsupported,
                published_date: CapabilityState::Unknown,
            },
            raw,
            Instant::now(),
        );

        assert_eq!(response.results.len(), 1);
        assert!(
            !response.receipt.honored.max_results,
            "one result for eight requested is a shortfall, not an honored cap"
        );
    }

    #[test]
    fn provider_native_domain_filter_is_reported_as_provider_honored() {
        let query = SearchQuery::new(
            "current release".to_string(),
            3,
            Some(Recency::Week),
            vec!["example.com".to_string()],
            None,
        );
        let raw = BackendSearch {
            backend: BackendId::ProviderNative,
            source: "provider-native/xai/grok-4.5".to_string(),
            backend_detail: Some("api.x.ai".to_string()),
            results: vec![
                SearchResult::new(
                    1,
                    "Exact source".to_string(),
                    "https://docs.example.com/release".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    2,
                    "Second source".to_string(),
                    "https://docs.example.com/release-notes".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    3,
                    "Third source".to_string(),
                    "https://docs.example.com/changelog".to_string(),
                    None,
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: Some("Grounded answer.".to_string()),
        };
        let response = finalize_search_response(
            query,
            QueryCapabilities {
                max_results: CapabilityState::Supported,
                recency: CapabilityState::Unsupported,
                domains: CapabilityState::Supported,
                locale: CapabilityState::Unsupported,
                published_date: CapabilityState::Unknown,
            },
            raw,
            Instant::now(),
        );

        assert!(response.receipt.honored.max_results);
        assert!(response.receipt.honored.domains);
        assert!(!response.receipt.honored.recency);
        assert!(response.receipt.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::KnobIgnored {
                knob: QueryKnob::Recency
            }
        )));
        assert!(!response.receipt.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::PostFiltered {
                knob: QueryKnob::Domains
            }
        )));
        assert_eq!(
            response.unverified_synthesis.as_deref(),
            Some("Grounded answer.")
        );
        assert!(!response.message.contains("Grounded answer."));
    }

    fn native_backend_search(note: String, degraded: Vec<DegradedReason>) -> BackendSearch {
        BackendSearch {
            backend: BackendId::ProviderNative,
            source: "provider-native/anthropic/claude-opus-4-8".to_string(),
            backend_detail: Some("api.anthropic.com".to_string()),
            results: (1..=5)
                .map(|rank| {
                    SearchResult::new(
                        rank,
                        format!("Source {rank}"),
                        format!("https://example.com/{rank}"),
                        None,
                        None,
                    )
                })
                .collect(),
            degraded,
            note: Some(note),
        }
    }

    fn native_query() -> SearchQuery {
        SearchQuery::new("current release".to_string(), 5, None, Vec::new(), None)
    }

    fn native_capabilities() -> QueryCapabilities {
        QueryCapabilities {
            max_results: CapabilityState::Supported,
            recency: CapabilityState::Unsupported,
            domains: CapabilityState::Supported,
            locale: CapabilityState::Unsupported,
            published_date: CapabilityState::Unknown,
        }
    }

    #[test]
    fn native_answer_reaches_the_model_whole_under_the_route_budget() {
        // #6508: a 6,000-character native answer with five citations used to
        // reach a 128K route as a ~900-character snippet (and earlier was cut
        // at 4,000 characters). Now the search result is whole within the
        // route's one inline budget.
        let answer = format!("{}END OF ANSWER", "Grounded answer sentence. ".repeat(240));
        assert!(answer.chars().count() > 6_000);
        let response = finalize_search_response(
            native_query(),
            native_capabilities(),
            native_backend_search(answer.clone(), Vec::new()),
            Instant::now(),
        );
        assert!(
            response
                .unverified_synthesis
                .as_deref()
                .is_some_and(|synthesis| synthesis.ends_with("END OF ANSWER"))
        );
        assert!(!response.message.contains("output limit"));

        let output = crate::tools::spec::ToolResult::json(&response).expect("json");
        let context = crate::core::engine::compact_tool_result_for_route(
            codewhale_config::ProviderKind::Deepseek,
            "deepseek-v3.2-128k",
            None,
            "web_search",
            &output,
        );
        assert_eq!(context, output.content.trim());
        assert!(context.contains("END OF ANSWER"));
    }

    #[test]
    fn native_answer_cut_by_the_provider_says_so() {
        let response = finalize_search_response(
            native_query(),
            native_capabilities(),
            native_backend_search(
                "Partial answer".to_string(),
                vec![DegradedReason::AnswerCutByProvider],
            ),
            Instant::now(),
        );
        assert!(
            response
                .receipt
                .degraded
                .contains(&DegradedReason::AnswerCutByProvider)
        );
        assert!(
            response
                .message
                .contains("the provider stopped the search answer at its output limit"),
            "{}",
            response.message
        );
    }

    #[test]
    fn provider_native_discards_answer_when_domain_filter_removes_a_source() {
        let query = SearchQuery::new(
            "current release".to_string(),
            3,
            None,
            vec!["example.com".to_string()],
            None,
        );
        let raw = BackendSearch {
            backend: BackendId::ProviderNative,
            source: "provider-native/xai/grok-4.5".to_string(),
            backend_detail: Some("api.x.ai".to_string()),
            results: vec![
                SearchResult::new(
                    1,
                    "Allowed source".to_string(),
                    "https://docs.example.com/release".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    2,
                    "Leaked source".to_string(),
                    "https://outside.test/release".to_string(),
                    None,
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: Some("Answer synthesized from both sources.".to_string()),
        };
        let response = finalize_search_response(
            query,
            QueryCapabilities {
                max_results: CapabilityState::Supported,
                recency: CapabilityState::Unsupported,
                domains: CapabilityState::Supported,
                locale: CapabilityState::Unsupported,
                published_date: CapabilityState::Unknown,
            },
            raw,
            Instant::now(),
        );

        assert_eq!(response.count, 1);
        assert_eq!(response.results[0].domain, "docs.example.com");
        assert!(response.message.starts_with("Found 1 result(s)\n"));
        assert!(response.unverified_synthesis.is_none());
        assert!(response.receipt.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::PostFiltered {
                knob: QueryKnob::Domains
            }
        )));
    }

    #[test]
    fn search_results_receive_session_scoped_refs_and_sanitize_credential_urls() {
        let query = SearchQuery::new("sources".to_string(), 5, None, Vec::new(), None);
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: vec![
                SearchResult::new(
                    1,
                    "Valid".to_string(),
                    "https://example.com/source#section".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    2,
                    "Protected".to_string(),
                    "https://example.com/protected?access_token=sensitive&view=full".to_string(),
                    None,
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: None,
        };
        let mut response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, Instant::now());
        let context = crate::tools::spec::ToolContext::new(std::path::PathBuf::from("."))
            .with_state_namespace("search-citation-session");

        register_search_citations(&mut response, &context);

        assert_eq!(response.count, 2);
        assert_eq!(response.results[0].url, "https://example.com/source");
        assert_eq!(
            response.results[1].url,
            "https://example.com/protected?view=full"
        );
        assert!(!response.results[1].url.contains("sensitive"));
        assert!(response.results[0].ref_id.starts_with("web_"));
        assert!(
            crate::tools::web::citations::resolve(
                "search-citation-session",
                &response.results[0].ref_id
            )
            .is_some()
        );
        assert!(
            crate::tools::web::citations::resolve(
                "foreign-search-citation-session",
                &response.results[0].ref_id
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn searxng_empty_results_report_backend() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "empty"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let (results, host) = WebSearchTool
            .run_searxng_search("empty", QueryFilters::default(), 5, 5_000, &ctx)
            .await
            .expect("empty SearXNG adapter response should be successful");
        let expected_host = reqwest::Url::parse(&server.uri())
            .expect("mock URL")
            .host_str()
            .expect("mock host")
            .to_string();

        assert!(results.is_empty());
        assert_eq!(host, expected_host);
    }

    #[tokio::test]
    async fn searxng_http_errors_are_actionable() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "blocked"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(403).set_body_string("json disabled"))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let err = WebSearchTool
            .run_searxng_search("blocked", QueryFilters::default(), 5, 5_000, &ctx)
            .await
            .expect_err("403 should be actionable");
        let msg = err.to_string();
        assert!(
            msg.contains("HTTP 403")
                && msg.contains("JSON output")
                && msg.contains("permits API access"),
            "got `{msg}`"
        );
    }

    #[tokio::test]
    async fn searxng_rate_limit_error_mentions_configured_instance() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "later"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(429).set_body_string("too many requests"))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let err = WebSearchTool
            .run_searxng_search("later", QueryFilters::default(), 5, 5_000, &ctx)
            .await
            .expect_err("429 should be actionable");
        let msg = err.to_string();
        assert!(
            msg.contains("HTTP 429")
                && msg.contains("rate-limiting")
                && msg.contains("trusted/self-hosted instance"),
            "got `{msg}`"
        );
    }

    #[tokio::test]
    async fn searxng_invalid_json_is_actionable() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "html"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>not json</html>"))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());

        let err = WebSearchTool
            .run_searxng_search("html", QueryFilters::default(), 5, 5_000, &ctx)
            .await
            .expect_err("invalid JSON should be actionable");
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to parse SearXNG JSON response")
                && msg.contains("format=json")
                && msg.contains("JSON output"),
            "got `{msg}`"
        );
    }

    #[tokio::test]
    async fn custom_duckduckgo_results_report_custom_host_source() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "rust async"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"
                <html><body>
                  <a class="result__a" href="https://example.com/rust">Rust async</a>
                  <div class="result__snippet">Async Rust result</div>
                </body></html>
                "#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::DuckDuckGo;
        let base_url = format!("{}/html/", server.uri());
        let expected_host = reqwest::Url::parse(&base_url)
            .expect("mock server url")
            .host_str()
            .expect("mock server host")
            .to_string();
        ctx.search_base_url = Some(base_url);

        let result = WebSearchTool
            .execute(json!({"query": "rust async"}), &ctx)
            .await
            .expect("custom endpoint should return results");
        let value: serde_json::Value =
            serde_json::from_str(&result.content).expect("web search json response");

        assert_eq!(value["source"].as_str(), Some(expected_host.as_str()));
        assert_eq!(value["count"].as_u64(), Some(1));
    }

    #[tokio::test]
    async fn repeated_search_uses_session_cache_and_marks_receipt() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use crate::tools::web::cache;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        cache::reset_search();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "session cache receipt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"
                <html><body>
                  <a class="result__a" href="https://example.com/cached">Cached result</a>
                  <div class="result__snippet">Fetched once.</div>
                </body></html>
                "#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf())
            .with_state_namespace("web-search-query-cache");
        context.search_provider = SearchProvider::DuckDuckGo;
        context.search_base_url = Some(format!("{}/html/", server.uri()));

        let first = WebSearchTool
            .execute(json!({"query": "session cache receipt"}), &context)
            .await
            .expect("first search should succeed");
        let second = WebSearchTool
            .execute(json!({"query": "session cache receipt"}), &context)
            .await
            .expect("second search should hit cache");
        let first: serde_json::Value =
            serde_json::from_str(&first.content).expect("first response json");
        let second: serde_json::Value =
            serde_json::from_str(&second.content).expect("second response json");
        let requests = server.received_requests().await.expect("recorded requests");

        assert_eq!(requests.len(), 1);
        assert_eq!(first["receipt"]["cache_hit"], false);
        assert_eq!(second["receipt"]["cache_hit"], true);
        assert_eq!(second["receipt"]["latency_ms"], 0);
        assert_eq!(second["results"], first["results"]);

        use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
        let denied_host = reqwest::Url::parse(&server.uri())
            .expect("mock server URL")
            .host_str()
            .expect("mock server host")
            .to_string();
        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: vec![denied_host],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let blocked = context
            .clone()
            .with_network_policy(NetworkPolicyDecider::new(policy, None));
        let error = WebSearchTool
            .execute(json!({"query": "session cache receipt"}), &blocked)
            .await
            .expect_err("tightened policy must win over the query cache");
        assert!(error.to_string().contains("blocked by network policy"));
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("recorded requests")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn explicit_bing_does_not_fall_back_to_duckduckgo() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bing"))
            .and(query_param("q", "one way fallback"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::Bing;
        context.search_base_url = Some(format!("{}/must-not-be-used", server.uri()));
        let query = SearchQuery::new("one way fallback".to_string(), 5, None, Vec::new(), None);
        let raw = run_scrape_search_with_endpoints(
            SearchProvider::Bing,
            &query,
            5_000,
            &context,
            ScrapeEndpoints {
                bing: &format!("{}/bing", server.uri()),
                allow_bing_fallback: Some(true),
            },
        )
        .await
        .expect("empty Bing response is a successful empty search");
        let requests = server.received_requests().await.expect("recorded requests");

        assert_eq!(raw.backend, BackendId::Bing);
        assert!(raw.results.is_empty());
        assert!(raw.degraded.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/bing");
    }

    #[tokio::test]
    async fn custom_duckduckgo_challenge_returns_actionable_error() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "rust async"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<html><body><div class="anomaly-modal">Unfortunately, bots use DuckDuckGo too</div></body></html>"#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::DuckDuckGo;
        ctx.search_base_url = Some(format!("{}/html/", server.uri()));

        let err = WebSearchTool
            .execute(json!({"query": "rust async"}), &ctx)
            .await
            .expect_err("custom endpoint challenge should error");
        let msg = err.to_string();
        assert!(
            msg.contains("DuckDuckGo-compatible search endpoint")
                && msg.contains("bot challenge")
                && msg.contains("private search service"),
            "got `{msg}`"
        );
    }

    #[tokio::test]
    async fn duckduckgo_challenge_to_bing_success_populates_fallback_receipt() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use std::time::Instant;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .and(query_param("q", "fallback receipt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<html><body><div class="anomaly-modal">Unfortunately, bots use DuckDuckGo too</div></body></html>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bing"))
            .and(query_param("q", "fallback receipt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"
                <ol><li class="b_algo">
                  <h2><a href="https://example.com/fallback">Fallback result</a></h2>
                  <div class="b_caption"><p>Bing result after challenge.</p></div>
                </li></ol>
                "#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::DuckDuckGo;
        context.search_base_url = Some(format!("{}/html/", server.uri()));
        let query = SearchQuery::new("fallback receipt".to_string(), 5, None, Vec::new(), None);
        let started = Instant::now();
        let raw = run_scrape_search_with_endpoints(
            SearchProvider::DuckDuckGo,
            &query,
            5_000,
            &context,
            ScrapeEndpoints {
                bing: &format!("{}/bing", server.uri()),
                allow_bing_fallback: Some(true),
            },
        )
        .await
        .expect("Bing fallback should succeed");
        let response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, started);
        let value = serde_json::to_value(&response).expect("response serializes");

        assert_eq!(value["source"], "bing");
        assert_eq!(value["count"], 1);
        assert_eq!(value["receipt"]["backend"], "bing");
        assert_eq!(
            value["receipt"]["degraded"][0],
            json!({"kind": "challenge_detected", "backend": "duckduckgo"})
        );
        assert_eq!(
            value["receipt"]["degraded"][1],
            json!({"kind": "scrape_fallback", "from": "duckduckgo", "to": "bing"})
        );
        assert!(
            response
                .receipt
                .warning()
                .expect("warning")
                .contains("used bing fallback")
        );
    }

    /// #6746: DuckDuckGo being unreachable (connection refused) or answering
    /// non-2xx must still reach the Bing fallback instead of ending the chain.
    #[tokio::test]
    async fn duckduckgo_unreachable_or_non_2xx_falls_back_to_bing() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bing"))
            .and(query_param("q", "ddg down"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"
                <ol><li class="b_algo">
                  <h2><a href="https://example.com/reachable">Reachable result</a></h2>
                  <div class="b_caption"><p>Bing answered while DuckDuckGo was down.</p></div>
                </li></ol>
                "#,
            ))
            .mount(&server)
            .await;
        // Bind then drop a listener so the port refuses connections.
        let refused = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let bing = format!("{}/bing", server.uri());
        let query = SearchQuery::new("ddg down".to_string(), 5, None, Vec::new(), None);
        for ddg in [
            format!("http://127.0.0.1:{refused}/html/"),
            format!("{}/html/", server.uri()),
        ] {
            let mut context = ToolContext::new(tmp.path().to_path_buf());
            context.search_provider = SearchProvider::DuckDuckGo;
            context.search_base_url = Some(ddg.clone());
            let raw = run_scrape_search_with_endpoints(
                SearchProvider::DuckDuckGo,
                &query,
                5_000,
                &context,
                ScrapeEndpoints {
                    bing: &bing,
                    allow_bing_fallback: Some(true),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{ddg}: Bing fallback should answer: {error}"));

            assert_eq!(raw.backend, BackendId::Bing, "{ddg}");
            assert_eq!(raw.results.len(), 1, "{ddg}");
            assert_eq!(raw.results[0].url, "https://example.com/reachable");
            assert_eq!(
                raw.degraded,
                vec![
                    DegradedReason::BackendUnavailable {
                        backend: BackendId::DuckDuckGo
                    },
                    DegradedReason::BackendFallback {
                        from: BackendId::DuckDuckGo,
                        to: BackendId::Bing
                    },
                ],
                "{ddg}"
            );
            assert!(
                raw.note
                    .as_deref()
                    .is_some_and(|note| note.contains("used Bing fallback")),
                "{ddg}: {:?}",
                raw.note
            );
        }

        // Without the Bing fallback the DuckDuckGo failure is still reported.
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::DuckDuckGo;
        context.search_base_url = Some(format!("{}/html/", server.uri()));
        let Err(error) = run_scrape_search_with_endpoints(
            SearchProvider::DuckDuckGo,
            &query,
            5_000,
            &context,
            ScrapeEndpoints {
                bing: &bing,
                allow_bing_fallback: Some(false),
            },
        )
        .await
        else {
            panic!("no fallback means the HTTP failure surfaces");
        };
        assert!(error.to_string().contains("HTTP 503"), "{error}");
    }

    /// #6746: a DuckDuckGo that accepts the connection and then hangs must not
    /// eat the whole budget; Bing answers inside the same total.
    #[tokio::test]
    async fn hanging_duckduckgo_leaves_bing_time_inside_the_total_budget() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html/"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bing"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"
                <ol><li class="b_algo">
                  <h2><a href="https://example.com/after-hang">After the hang</a></h2>
                  <div class="b_caption"><p>Bing answered after DuckDuckGo hung.</p></div>
                </li></ol>
                "#,
            ))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::DuckDuckGo;
        context.search_base_url = Some(format!("{}/html/", server.uri()));
        let query = SearchQuery::new("hang".to_string(), 5, None, Vec::new(), None);
        let budget = Duration::from_millis(3_000);
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            budget,
            run_scrape_search_with_endpoints(
                SearchProvider::DuckDuckGo,
                &query,
                3_000,
                &context,
                ScrapeEndpoints {
                    bing: &format!("{}/bing", server.uri()),
                    allow_bing_fallback: Some(true),
                },
            ),
        )
        .await
        .expect("the whole search must finish inside its budget");
        let raw = outcome.expect("Bing fallback should answer after DuckDuckGo hangs");

        assert_eq!(raw.backend, BackendId::Bing);
        assert_eq!(raw.results.len(), 1);
        assert!(started.elapsed() < budget, "{:?}", started.elapsed());
        assert!(
            raw.degraded.contains(&DegradedReason::BackendUnavailable {
                backend: BackendId::DuckDuckGo
            }),
            "{:?}",
            raw.degraded
        );
    }

    /// #6746: a custom `search_base_url` keeps no public fallback and keeps the
    /// whole budget: a slow private endpoint is waited for, and Bing is never
    /// contacted, whether the endpoint answers late or hangs.
    #[tokio::test]
    async fn custom_base_url_keeps_full_budget_and_never_reaches_bing() {
        use crate::tools::spec::ToolContext;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::tempdir().expect("tempdir");
        let query = SearchQuery::new("private".to_string(), 5, None, Vec::new(), None);
        for (delay, expect_results) in [
            (Duration::from_millis(1_500), true),
            (Duration::from_secs(30), false),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/html/"))
                .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_string(
                    r#"
                            <html><body>
                              <a class="result__a" href="https://example.com/private">Private</a>
                              <div class="result__snippet">Private result</div>
                            </body></html>
                            "#,
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/bing"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;

            let mut context = ToolContext::new(tmp.path().to_path_buf());
            context.search_provider = SearchProvider::DuckDuckGo;
            context.search_base_url = Some(format!("{}/html/", server.uri()));
            // 60% of 2s would be 1.2s: a 1.5s answer only arrives if the
            // private endpoint kept the whole budget.
            let outcome = run_scrape_search_with_endpoints(
                SearchProvider::DuckDuckGo,
                &query,
                2_000,
                &context,
                ScrapeEndpoints {
                    bing: &format!("{}/bing", server.uri()),
                    allow_bing_fallback: None,
                },
            )
            .await;
            if expect_results {
                let raw = outcome.expect("a slow private endpoint keeps its full budget");
                assert_eq!(raw.backend, BackendId::DuckDuckGo);
                assert_eq!(raw.results.len(), 1);
            } else {
                assert!(outcome.is_err(), "a hanging private endpoint is an error");
            }
            let bing_requests = server
                .received_requests()
                .await
                .expect("recorded requests")
                .iter()
                .filter(|request| request.url.path() == "/bing")
                .count();
            assert_eq!(bing_requests, 0, "custom base URL must not reach Bing");
        }
    }

    #[tokio::test]
    async fn search_base_url_with_non_duckduckgo_provider_is_explicit_error() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        use codewhale_runtime::config::SearchProvider;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Tavily;
        ctx.search_base_url = Some("https://search.internal.example/html/".to_string());

        let err = WebSearchTool
            .execute(json!({"query": "rust async"}), &ctx)
            .await
            .expect_err("non-duckduckgo provider with base_url should error");
        let msg = err.to_string();
        assert!(
            msg.contains("[search].base_url")
                && msg.contains("provider = \"duckduckgo\" or \"searxng\"")
                && msg.contains("tavily"),
            "got `{msg}`"
        );
    }

    #[test]
    fn rerank_assigns_sequential_ranks_starting_at_one() {
        // Simulates the post-dedup path: ranks may be non-contiguous after a
        // result is dropped; rerank must restore a clean 1..N sequence.
        let mut results = vec![
            SearchResult::new(
                5,
                "C".to_string(),
                "https://c.example.com/".to_string(),
                None,
                None,
            ),
            SearchResult::new(
                3,
                "A".to_string(),
                "https://a.example.com/".to_string(),
                None,
                None,
            ),
            SearchResult::new(
                1,
                "B".to_string(),
                "https://b.example.com/".to_string(),
                None,
                None,
            ),
        ];
        rerank(&mut results);
        assert_eq!(results[0].rank, 1);
        assert_eq!(results[1].rank, 2);
        assert_eq!(results[2].rank, 3);
    }

    #[test]
    fn rerank_on_empty_slice_is_a_no_op() {
        let mut results: Vec<SearchResult> = Vec::new();
        rerank(&mut results); // must not panic
    }

    #[test]
    fn register_search_citations_deduplicates_results_with_same_canonical_url() {
        let namespace = "dedup-test-session-fragments";
        let query = SearchQuery::new("deduplicate".to_string(), 5, None, Vec::new(), None);
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: vec![
                SearchResult::new(
                    1,
                    "First occurrence".to_string(),
                    "https://dedup.example.com/page#section-a".to_string(),
                    Some("first snippet".to_string()),
                    None,
                ),
                SearchResult::new(
                    2,
                    "Unique result".to_string(),
                    "https://other.dedup.example.com/different".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    3,
                    "Duplicate of first".to_string(),
                    "https://dedup.example.com/page#section-b".to_string(),
                    Some("duplicate snippet".to_string()),
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: None,
        };
        let mut response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, Instant::now());
        let context = crate::tools::spec::ToolContext::new(std::path::PathBuf::from("."))
            .with_state_namespace(namespace);

        register_search_citations(&mut response, &context);

        assert_eq!(
            response.count, 2,
            "duplicate canonical URL must reduce the result count"
        );
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].rank, 1);
        assert_eq!(response.results[1].rank, 2);
        assert_eq!(response.results[0].url, "https://dedup.example.com/page");
        assert_eq!(
            response.results[1].url,
            "https://other.dedup.example.com/different"
        );
        assert_ne!(
            response.results[0].ref_id, response.results[1].ref_id,
            "surviving results must have distinct ref_ids"
        );
        assert!(response.message.contains('2'), "{}", response.message);
    }

    #[test]
    fn register_search_citations_preserves_title_url_and_ref_id_metadata() {
        let namespace = "citation-metadata-test-session";
        let query = SearchQuery::new("docs".to_string(), 5, None, Vec::new(), None);
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: vec![SearchResult::new(
                1,
                "Official Docs".to_string(),
                "https://docs.citation-meta.example.com/reference".to_string(),
                Some("Comprehensive reference documentation.".to_string()),
                None,
            )],
            degraded: Vec::new(),
            note: None,
        };
        let mut response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, Instant::now());
        let context = crate::tools::spec::ToolContext::new(std::path::PathBuf::from("."))
            .with_state_namespace(namespace);

        register_search_citations(&mut response, &context);

        assert_eq!(response.count, 1);
        let result = &response.results[0];
        assert!(
            result.ref_id.starts_with("web_"),
            "ref_id must use web_ prefix; got `{}`",
            result.ref_id
        );
        assert_eq!(result.title, "Official Docs");
        assert_eq!(
            result.url,
            "https://docs.citation-meta.example.com/reference"
        );
        assert_eq!(result.rank, 1);
        let citation = crate::tools::web::citations::resolve(namespace, &result.ref_id)
            .expect("citation must be registered and resolvable in its session");
        assert_eq!(citation.ref_id, result.ref_id);
        assert_eq!(citation.url, result.url);
        assert_eq!(citation.title.as_deref(), Some("Official Docs"));
        assert!(
            !citation.retrieved_at.is_empty(),
            "retrieved_at must be set to the retrieval timestamp"
        );
        assert!(
            crate::tools::web::citations::resolve("other-session", &result.ref_id).is_none(),
            "citation must not leak to foreign sessions"
        );
    }

    #[test]
    fn finalize_search_response_truncates_to_max_results_and_reranks() {
        let query = SearchQuery::new("truncate me".to_string(), 2, None, Vec::new(), None);
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: vec![
                SearchResult::new(
                    1,
                    "A".to_string(),
                    "https://a.trunc.example.com/".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    2,
                    "B".to_string(),
                    "https://b.trunc.example.com/".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    3,
                    "C".to_string(),
                    "https://c.trunc.example.com/".to_string(),
                    None,
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: None,
        };

        let response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, Instant::now());

        assert_eq!(response.count, 2, "must be truncated to max_results");
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[0].rank, 1);
        assert_eq!(response.results[1].rank, 2);
        assert_eq!(response.results[0].title, "A");
        assert_eq!(response.results[1].title, "B");
        assert!(response.message.contains('2'), "{}", response.message);
    }

    #[test]
    fn finalize_search_response_does_not_claim_an_ignored_locale_was_honored() {
        // A scrape flagged the explicit locale as ignored (here: an unmapped
        // DuckDuckGo region): the receipt must keep `honored.locale` false
        // instead of claiming the knob was honored.
        let query = SearchQuery::new(
            "query".to_string(),
            5,
            None,
            Vec::new(),
            Some("fr-FR".to_string()),
        );
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: Vec::new(),
            degraded: vec![DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            }],
            note: None,
        };
        let scrape_capabilities = QueryCapabilities {
            max_results: CapabilityState::Supported,
            recency: CapabilityState::Unsupported,
            domains: CapabilityState::Unsupported,
            locale: CapabilityState::Supported,
            published_date: CapabilityState::Unknown,
        };
        let response = finalize_search_response(query, scrape_capabilities, raw, Instant::now());
        assert!(
            !response.receipt.honored.locale,
            "an ignored locale must not be reported as honored"
        );
        assert!(
            response.receipt.degraded.iter().any(|reason| matches!(
                reason,
                DegradedReason::KnobIgnored {
                    knob: QueryKnob::Locale
                }
            )),
            "the degraded receipt must carry the locale flag exactly once"
        );
        assert_eq!(response.receipt.degraded.len(), 1);

        // A mapped locale with no scrape flag stays honored.
        let query = SearchQuery::new(
            "query".to_string(),
            5,
            None,
            Vec::new(),
            Some("zh-CN".to_string()),
        );
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: Vec::new(),
            degraded: Vec::new(),
            note: None,
        };
        let response = finalize_search_response(query, scrape_capabilities, raw, Instant::now());
        assert!(response.receipt.honored.locale);
    }

    #[test]
    fn domain_matches_handles_subdomains_www_prefix_and_empty_list() {
        assert!(
            domain_matches("https://any.example.com/page", &[]),
            "empty domain list must accept all URLs"
        );
        assert!(domain_matches(
            "https://example.com/page",
            &["example.com".to_string()]
        ));
        assert!(domain_matches(
            "https://docs.example.com/page",
            &["example.com".to_string()]
        ));
        assert!(domain_matches(
            "https://www.example.com/page",
            &["example.com".to_string()]
        ));
        assert!(domain_matches(
            "https://example.com/page",
            &["www.example.com".to_string()]
        ));
        assert!(!domain_matches(
            "https://other.com/page",
            &["example.com".to_string()]
        ));
        assert!(!domain_matches(
            "https://notexample.com/page",
            &["example.com".to_string()]
        ));
    }

    #[test]
    fn finalize_search_response_domain_post_filter_reranks_survivors() {
        let query = SearchQuery::new(
            "domain filter".to_string(),
            5,
            None,
            vec!["keep.example.com".to_string()],
            None,
        );
        let raw = BackendSearch {
            backend: BackendId::DuckDuckGo,
            source: "duckduckgo".to_string(),
            backend_detail: None,
            results: vec![
                SearchResult::new(
                    1,
                    "Drop this".to_string(),
                    "https://other.example.com/page".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    2,
                    "Keep this".to_string(),
                    "https://keep.example.com/page".to_string(),
                    None,
                    None,
                ),
                SearchResult::new(
                    3,
                    "Also drop".to_string(),
                    "https://unrelated.example.com/page".to_string(),
                    None,
                    None,
                ),
            ],
            degraded: Vec::new(),
            note: None,
        };

        let response =
            finalize_search_response(query, QueryCapabilities::count_only(), raw, Instant::now());

        assert_eq!(response.count, 1, "only the matching domain must survive");
        assert_eq!(
            response.results[0].rank, 1,
            "survivor must be re-ranked to 1"
        );
        assert_eq!(response.results[0].title, "Keep this");
        assert!(
            response.receipt.degraded.iter().any(|reason| matches!(
                reason,
                DegradedReason::PostFiltered {
                    knob: QueryKnob::Domains
                }
            )),
            "post-filtered degraded reason must be present"
        );
    }

    #[test]
    fn fallback_receipt_carries_full_backend_chain_history() {
        // Verifies that the machine-readable degraded vec records every hop in
        // the fallback chain so callers can audit exactly what happened.
        let receipt = crate::tools::web::contract::SearchReceipt {
            backend: BackendId::Bing,
            backend_detail: None,
            requested: SearchQuery::new("fallback chain".to_string(), 5, None, Vec::new(), None),
            capabilities: QueryCapabilities::count_only(),
            honored: crate::tools::web::contract::HonoredQueryCapabilities {
                max_results: true,
                ..Default::default()
            },
            degraded: vec![
                DegradedReason::ChallengeDetected {
                    backend: BackendId::DuckDuckGo,
                },
                DegradedReason::ScrapeFallback {
                    from: BackendId::DuckDuckGo,
                    to: BackendId::Bing,
                },
            ],
            latency_ms: 42,
            cache_hit: false,
        };

        let value = serde_json::to_value(&receipt).expect("receipt must serialize");
        assert_eq!(value["backend"], "bing");
        assert_eq!(value["degraded"].as_array().unwrap().len(), 2);
        assert_eq!(value["degraded"][0]["kind"], "challenge_detected");
        assert_eq!(value["degraded"][0]["backend"], "duckduckgo");
        assert_eq!(value["degraded"][1]["kind"], "scrape_fallback");
        assert_eq!(value["degraded"][1]["from"], "duckduckgo");
        assert_eq!(value["degraded"][1]["to"], "bing");

        let warning = receipt
            .warning()
            .expect("degraded receipt must produce a warning");
        assert!(warning.contains("bot challenge"), "{warning}");
        assert!(warning.contains("used bing fallback"), "{warning}");
    }

    fn filtered_query(recency: Option<Recency>, locale: Option<&str>) -> SearchQuery {
        SearchQuery::new(
            "whale song".to_string(),
            5,
            recency,
            Vec::new(),
            locale.map(str::to_string),
        )
    }

    #[test]
    fn query_filters_round_recency_up_and_split_locale() {
        let query = filtered_query(Some(Recency::Days(10)), Some("pt_BR"));
        let filters = QueryFilters::of(&query);
        assert_eq!(filters.window(), Some("month"));
        assert_eq!(filters.language().as_deref(), Some("pt"));
        assert_eq!(filters.region().as_deref(), Some("BR"));
        for (recency, window) in [
            (Recency::Day, "day"),
            (Recency::Week, "week"),
            (Recency::Month, "month"),
            (Recency::Year, "year"),
            (Recency::Days(400), "year"),
        ] {
            let query = filtered_query(Some(recency), None);
            assert_eq!(QueryFilters::of(&query).window(), Some(window));
        }
        let bare = filtered_query(None, Some("en"));
        assert_eq!(QueryFilters::of(&bare).region(), None);
        assert_eq!(QueryFilters::of(&bare).window(), None);
    }

    #[tokio::test]
    async fn firecrawl_sends_recency_and_country() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/search"))
            .and(body_partial_json(json!({"tbs": "qdr:w", "country": "DE"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"web": [{"title": "Fresh", "url": "https://example.de/a"}]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let query = filtered_query(Some(Recency::Week), Some("de-DE"));
        let (entries, _) = WebSearchTool
            .run_firecrawl_search_at(
                &format!("{}/v2/search", server.uri()),
                &query.query,
                QueryFilters::of(&query),
                5,
                5_000,
                None,
            )
            .await
            .expect("filtered Firecrawl search");
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn searxng_sends_time_range_and_language() {
        use crate::tools::spec::ToolContext;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("time_range", "day"))
            .and(query_param("language", "fr-FR"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"title": "Aujourd'hui", "url": "https://example.fr/a", "content": "x"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.search_provider = SearchProvider::Searxng;
        ctx.search_base_url = Some(server.uri());
        let query = filtered_query(Some(Recency::Day), Some("fr-FR"));

        let (results, _) = WebSearchTool
            .run_searxng_search(&query.query, QueryFilters::of(&query), 5, 5_000, &ctx)
            .await
            .expect("filtered SearXNG search");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn tavily_payload_carries_time_range_only_when_requested() {
        let query = filtered_query(Some(Recency::Year), Some("en-US"));
        let payload =
            super::tavily_search_payload("tvly-x", &query.query, QueryFilters::of(&query), 5);
        assert_eq!(payload["time_range"], "year");
        assert!(
            payload.get("country").is_none(),
            "Tavily takes country names, not tags"
        );
        let plain = super::tavily_search_payload("tvly-x", "q", QueryFilters::default(), 5);
        assert!(plain.get("time_range").is_none());
    }

    #[test]
    fn serply_url_carries_language_and_country() {
        let query = filtered_query(Some(Recency::Day), Some("ja-JP"));
        let url = serply_search_url(&query.query, QueryFilters::of(&query), 3).expect("serply url");
        let pairs: std::collections::BTreeMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(pairs.get("hl").map(String::as_str), Some("ja"));
        assert_eq!(pairs.get("gl").map(String::as_str), Some("jp"));
        assert!(
            !pairs.contains_key("tbs"),
            "Serply documents no recency parameter"
        );
    }

    #[test]
    fn adapter_reported_ignored_locale_is_not_counted_as_honored() {
        let query = filtered_query(Some(Recency::Week), Some("en"));
        let raw = BackendSearch {
            backend: BackendId::Firecrawl,
            source: "firecrawl".to_string(),
            backend_detail: None,
            results: Vec::new(),
            degraded: vec![DegradedReason::KnobIgnored {
                knob: QueryKnob::Locale,
            }],
            note: None,
        };
        let capabilities = QueryCapabilities {
            recency: CapabilityState::Supported,
            locale: CapabilityState::Supported,
            ..QueryCapabilities::count_only()
        };
        let response = finalize_search_response(query, capabilities, raw, Instant::now());
        assert!(response.receipt.honored.recency);
        assert!(!response.receipt.honored.locale);
        assert_eq!(
            response
                .receipt
                .degraded
                .iter()
                .filter(|reason| matches!(
                    reason,
                    DegradedReason::KnobIgnored {
                        knob: QueryKnob::Locale
                    }
                ))
                .count(),
            1
        );
    }
}

#[cfg(test)]
#[path = "web/search_host_tests.rs"]
mod host_tests;
