//! Finance quote tool backed by Yahoo Finance-style public endpoints.
//!
//! The tool prefers Yahoo's quote endpoint and falls back to the chart endpoint
//! when quote access is unavailable or returns no data.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, optional_u64,
};
use codewhale_net::network_policy::Decision;

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const QUOTE_SOURCE: &str = "yahoo_quote";
const CHART_SOURCE: &str = "yahoo_chart";

#[derive(Debug, Clone)]
struct FinanceEndpoints {
    quote_base: String,
    chart_base: String,
}

impl Default for FinanceEndpoints {
    fn default() -> Self {
        Self {
            quote_base: std::env::var("CODEWHALE_FINANCE_QUOTE_BASE_URL")
                .or_else(|_| std::env::var("DEEPSEEK_FINANCE_QUOTE_BASE_URL"))
                .unwrap_or_else(|_| "https://query1.finance.yahoo.com/v7/finance/quote".into()),
            chart_base: std::env::var("CODEWHALE_FINANCE_CHART_BASE_URL")
                .or_else(|_| std::env::var("DEEPSEEK_FINANCE_CHART_BASE_URL"))
                .unwrap_or_else(|_| "https://query1.finance.yahoo.com/v8/finance/chart".into()),
        }
    }
}

impl FinanceEndpoints {
    fn quote_url(&self, symbol: &str) -> String {
        format!(
            "{}?symbols={}",
            self.quote_base.trim_end_matches('/'),
            codewhale_util::utils::url_encode(symbol)
        )
    }

    fn chart_url(&self, symbol: &str) -> String {
        format!(
            "{}/{}?interval=1d&range=5d",
            self.chart_base.trim_end_matches('/'),
            codewhale_util::utils::url_encode(symbol)
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct FinanceRequest {
    requested_ticker: String,
    resolved_symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FinanceQuoteResponse {
    requested_ticker: String,
    ticker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(deserialize_with = "deserialize_host_float")]
    price: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_host_optional")]
    change: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_host_optional")]
    change_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_host_optional")]
    previous_close: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    market_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quote_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exchange: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_host_optional")]
    market_time: Option<i64>,
    source: String,
    fallback_used: bool,
}

fn deserialize_host_float<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<f64, D::Error> {
    let text = String::deserialize(decoder)?;
    if text.len() > 32 {
        return Err(serde::de::Error::custom(
            "host numeric field exceeds its bound",
        ));
    }
    text.parse().map_err(serde::de::Error::custom)
}
fn deserialize_host_optional<'de, D, T>(decoder: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    Option::<String>::deserialize(decoder)?
        .map(|text| {
            if text.len() > 32 {
                return Err(serde::de::Error::custom(
                    "host numeric field exceeds its bound",
                ));
            }
            text.parse().map_err(serde::de::Error::custom)
        })
        .transpose()
}
fn serialize_timestamp<S: serde::Serializer>(
    value: &Option<i64>,
    encoder: S,
) -> Result<S::Ok, S::Error> {
    value.map(|time| time.to_string()).serialize(encoder)
}

#[derive(Debug, Clone)]
enum AttemptFailureKind {
    Timeout,
    NotFound,
    Upstream,
}

#[derive(Debug, Clone)]
struct AttemptFailure {
    endpoint: &'static str,
    kind: AttemptFailureKind,
    detail: String,
}

impl AttemptFailure {
    fn timeout(endpoint: &'static str) -> Self {
        Self {
            endpoint,
            kind: AttemptFailureKind::Timeout,
            detail: "request timed out".to_string(),
        }
    }

    fn not_found(endpoint: &'static str, detail: impl Into<String>) -> Self {
        Self {
            endpoint,
            kind: AttemptFailureKind::NotFound,
            detail: detail.into(),
        }
    }

    fn upstream(endpoint: &'static str, detail: impl Into<String>) -> Self {
        Self {
            endpoint,
            kind: AttemptFailureKind::Upstream,
            detail: detail.into(),
        }
    }

    fn is_timeout(&self) -> bool {
        matches!(self.kind, AttemptFailureKind::Timeout)
    }

    fn is_not_found(&self) -> bool {
        matches!(self.kind, AttemptFailureKind::NotFound)
    }

    fn summary(&self) -> String {
        format!("{}: {}", self.endpoint, self.detail)
    }
}

pub struct FinanceTool {
    endpoints: FinanceEndpoints,
    client: Client,
}

impl FinanceTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            endpoints: FinanceEndpoints::default(),
            client: codewhale_release::tls::reqwest_client_builder()
                .user_agent(USER_AGENT)
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    #[cfg(test)]
    fn with_endpoints(quote_base: impl Into<String>, chart_base: impl Into<String>) -> Self {
        Self {
            endpoints: FinanceEndpoints {
                quote_base: quote_base.into(),
                chart_base: chart_base.into(),
            },
            client: codewhale_release::tls::reqwest_client_builder()
                .user_agent(USER_AGENT)
                .build()
                .expect("failed to build HTTP client"),
        }
    }
}

impl Default for FinanceTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolSpec for FinanceTool {
    fn name(&self) -> &'static str {
        "finance"
    }

    fn description(&self) -> &'static str {
        "Fetch live stock, ETF or crypto quotes via Yahoo-style endpoints under the session network policy."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ticker": {
                    "type": "string",
                    "description": "Ticker symbol to look up (for example: AAPL, SPY, BTC)."
                },
                "symbol": {
                    "type": "string",
                    "description": "Alias for ticker."
                },
                "type": {
                    "type": "string",
                    "description": "Optional asset type hint such as equity, fund, crypto, or index."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Total lookup timeout in milliseconds, shared by the quote request and its chart fallback (default: 10000, max: 60000)."
                }
            },
            "anyOf": [
                { "required": ["ticker"] },
                { "required": ["symbol"] }
            ],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ReadOnly,
            ToolCapability::Network,
            ToolCapability::Sandboxable,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let raw_ticker = match optional_str(&input, "ticker")? {
            Some(ticker) => Some(ticker),
            None => optional_str(&input, "symbol")?,
        }
        .ok_or_else(|| ToolError::missing_field("ticker"))?
        .trim();
        if raw_ticker.is_empty() {
            return Err(ToolError::invalid_input("ticker cannot be empty"));
        }

        let type_hint = optional_str(&input, "type")?.map(str::trim);
        let timeout_ms =
            optional_u64(&input, "timeout_ms", DEFAULT_TIMEOUT_MS)?.clamp(100, MAX_TIMEOUT_MS);

        let request = normalize_request(raw_ticker, type_hint);
        let timeout = Duration::from_millis(timeout_ms);

        // #135: quote and chart hosts are both vetted before any transport
        // fires, so a tightened session (e.g. network.default = "deny")
        // cannot leak a request through the chart fallback.
        check_network_policy(context, &self.endpoints)?;

        // One budget covers the quote request and its chart fallback
        // (#6557 D03-m3): the fallback gets only what the first attempt left,
        // so `timeout_ms` — and the Timeout error that reports it — is the
        // real wall-clock bound rather than half of it.
        let deadline = std::time::Instant::now() + timeout;
        let host = context
            .features
            .enabled(codewhale_runtime::features::Feature::FinanceHost);
        let quote_result = if host {
            fetch_host_endpoint(
                &self.client,
                timeout,
                &self.endpoints,
                &request,
                context,
                deadline,
                false,
            )
            .await
            .map_err(|error| normalize_host_error(error, timeout_ms))?
        } else {
            fetch_quote_endpoint(&self.client, timeout, &self.endpoints, &request).await
        };
        match quote_result {
            Ok(result) => {
                ToolResult::json(&result).map_err(|e| ToolError::execution_failed(e.to_string()))
            }
            Err(first_failure) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let chart_result = if remaining.is_zero() {
                    Err(AttemptFailure::timeout(CHART_SOURCE))
                } else {
                    if host {
                        fetch_host_endpoint(
                            &self.client,
                            remaining,
                            &self.endpoints,
                            &request,
                            context,
                            deadline,
                            true,
                        )
                        .await
                        .map_err(|error| normalize_host_error(error, timeout_ms))?
                    } else {
                        fetch_chart_endpoint(&self.client, remaining, &self.endpoints, &request)
                            .await
                    }
                };
                match chart_result {
                    Ok(result) => ToolResult::json(&result)
                        .map_err(|e| ToolError::execution_failed(e.to_string())),
                    Err(second_failure) => Err(finalize_failure(
                        &request,
                        timeout_ms,
                        &[first_failure, second_failure],
                    )),
                }
            }
        }
    }
}

/// Fail closed when the session network policy denies (or has not approved)
/// either endpoint host. Mirrors the Web/web_search/speech family: `Deny`
/// and an undecided `Prompt` both stop before any request is made; no
/// attached policy falls through permissively for back-compat.
fn check_network_policy(
    context: &ToolContext,
    endpoints: &FinanceEndpoints,
) -> Result<(), ToolError> {
    let Some(decider) = context.network_policy.as_ref() else {
        return Ok(());
    };
    for base in [&endpoints.quote_base, &endpoints.chart_base] {
        let Some(host) = codewhale_net::network_policy::host_from_url(base) else {
            continue;
        };
        match decider.evaluate(&host, "finance") {
            Decision::Allow => {}
            Decision::Deny => {
                return Err(ToolError::permission_denied(format!(
                    "finance lookup to '{host}' blocked by network policy"
                )));
            }
            Decision::Prompt => {
                return Err(ToolError::permission_denied(format!(
                    "finance lookup to '{host}' requires approval; \
                     re-run after `/network allow {host}` or set network.default = \"allow\" in config"
                )));
            }
        }
    }
    Ok(())
}

fn normalize_request(raw_ticker: &str, type_hint: Option<&str>) -> FinanceRequest {
    let requested_ticker = raw_ticker.trim().to_ascii_uppercase();
    let resolved_symbol = if requested_ticker == "BTC" {
        "BTC-USD".to_string()
    } else if type_hint.is_some_and(|hint| hint.eq_ignore_ascii_case("crypto"))
        && !requested_ticker.contains('-')
    {
        format!("{requested_ticker}-USD")
    } else {
        requested_ticker.clone()
    };

    FinanceRequest {
        requested_ticker,
        resolved_symbol,
    }
}

fn normalize_host_error(error: ToolError, timeout_ms: u64) -> ToolError {
    match error {
        ToolError::Timeout { .. } => ToolError::Timeout {
            seconds: millis_to_timeout_seconds(timeout_ms),
        },
        error => error,
    }
}

/// The inner error is an upstream/business failure and may take the existing
/// chart fallback. Host admission/runtime/result errors are outer errors and
/// leave immediately, so an enabled Host failure never runs Rust normalization.
async fn fetch_host_endpoint(
    client: &Client,
    timeout: Duration,
    endpoints: &FinanceEndpoints,
    request: &FinanceRequest,
    context: &ToolContext,
    deadline: std::time::Instant,
    chart: bool,
) -> Result<Result<FinanceQuoteResponse, AttemptFailure>, ToolError> {
    let endpoint = if chart { CHART_SOURCE } else { QUOTE_SOURCE };
    let url = if chart {
        endpoints.chart_url(&request.resolved_symbol)
    } else {
        endpoints.quote_url(&request.resolved_symbol)
    };
    let body = tokio::select! {
        biased;
        _ = async { if let Some(cancel) = context.cancel_token.as_ref() { cancel.cancelled().await } else { std::future::pending::<()>().await } } => return Err(ToolError::not_available("finance cancelled")),
        body = fetch_response_body_bounded(client, timeout, &url, endpoint) => match body { Ok(body) => body, Err(error) => return Ok(Err(error)) },
    };
    let parsed = if chart {
        serde_json::from_str::<ChartEndpointResponse>(&body).and_then(serde_json::to_value)
    } else {
        serde_json::from_str::<QuoteEndpointResponse>(&body).and_then(serde_json::to_value)
    };
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            return Ok(Err(AttemptFailure::upstream(
                endpoint,
                format!("invalid JSON response: {error}"),
            )));
        }
    };
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Ok(Err(AttemptFailure::timeout(endpoint)));
    }
    let result = crate::extension_host::manager()
        .execute_stock(
            if chart {
                crate::extension_host::StockOperation::FinanceChart
            } else {
                crate::extension_host::StockOperation::FinanceQuote
            },
            json!({"request":request,"parsed":parsed}),
            context,
            remaining,
        )
        .await?;
    let metadata = result
        .metadata
        .ok_or_else(|| ToolError::execution_failed("Host finance result omitted metadata"))?;
    if result.success {
        let quote: FinanceQuoteResponse = serde_json::from_value(metadata)
            .map_err(|_| ToolError::execution_failed("Host finance result malformed"))?;
        if quote.requested_ticker != request.requested_ticker
            || quote.source != endpoint
            || quote.fallback_used != chart
            || !quote.price.is_finite()
        {
            return Err(ToolError::execution_failed(
                "Host finance result changed its captured request",
            ));
        }
        Ok(Ok(quote))
    } else {
        if metadata.get("endpoint").and_then(Value::as_str) != Some(endpoint) {
            return Err(ToolError::execution_failed(
                "Host finance failure changed its endpoint",
            ));
        }
        let detail = metadata
            .get("detail")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution_failed("Host finance failure omitted detail"))?;
        let failure = match metadata.get("kind").and_then(Value::as_str) {
            Some("not_found") => AttemptFailure::not_found(endpoint, detail),
            Some("upstream") => AttemptFailure::upstream(endpoint, detail),
            _ => {
                return Err(ToolError::execution_failed(
                    "Host finance failure malformed",
                ));
            }
        };
        Ok(Err(failure))
    }
}

/// Host captures are bounded before JSON parsing and before broker admission.
/// The request's existing timeout includes every streamed body chunk.
async fn fetch_response_body_bounded(
    client: &Client,
    timeout: Duration,
    url: &str,
    endpoint: &'static str,
) -> Result<String, AttemptFailure> {
    const MAX_BODY: usize = 1024 * 1024;
    let mut response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                AttemptFailure::timeout(endpoint)
            } else {
                AttemptFailure::upstream(endpoint, format!("request failed: {error}"))
            }
        })?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            AttemptFailure::timeout(endpoint)
        } else {
            AttemptFailure::upstream(endpoint, format!("failed to read response body: {error}"))
        }
    })? {
        if chunk.len() > MAX_BODY.saturating_sub(bytes.len()) {
            return Err(AttemptFailure::upstream(
                endpoint,
                "Host finance response exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    if !status.is_success() {
        return Err(status_failure(endpoint, status, &body));
    }
    Ok(body)
}

async fn fetch_quote_endpoint(
    client: &Client,
    timeout: Duration,
    endpoints: &FinanceEndpoints,
    request: &FinanceRequest,
) -> Result<FinanceQuoteResponse, AttemptFailure> {
    let url = endpoints.quote_url(&request.resolved_symbol);
    let body = fetch_response_body(client, timeout, &url, QUOTE_SOURCE).await?;
    let parsed: QuoteEndpointResponse = serde_json::from_str(&body).map_err(|e| {
        AttemptFailure::upstream(QUOTE_SOURCE, format!("invalid JSON response: {e}"))
    })?;

    let quote = parsed
        .quote_response
        .result
        .into_iter()
        .find(|item| item.symbol.eq_ignore_ascii_case(&request.resolved_symbol))
        .ok_or_else(|| {
            AttemptFailure::not_found(
                QUOTE_SOURCE,
                format!("no result for symbol '{}'", request.resolved_symbol),
            )
        })?;

    let price = quote.regular_market_price.ok_or_else(|| {
        AttemptFailure::upstream(QUOTE_SOURCE, "response missing regularMarketPrice")
    })?;
    let previous_close = quote.regular_market_previous_close;
    let change = quote
        .regular_market_change
        .or_else(|| compute_change(price, previous_close));
    let change_percent = quote
        .regular_market_change_percent
        .or_else(|| compute_change_percent(price, previous_close));

    Ok(FinanceQuoteResponse {
        requested_ticker: request.requested_ticker.clone(),
        ticker: quote.symbol,
        name: quote.long_name.or(quote.short_name),
        price,
        currency: quote.currency,
        change,
        change_percent,
        previous_close,
        market_state: quote.market_state,
        quote_type: quote.quote_type,
        exchange: quote.full_exchange_name.or(quote.exchange),
        market_time: quote.regular_market_time,
        source: QUOTE_SOURCE.to_string(),
        fallback_used: false,
    })
}

async fn fetch_chart_endpoint(
    client: &Client,
    timeout: Duration,
    endpoints: &FinanceEndpoints,
    request: &FinanceRequest,
) -> Result<FinanceQuoteResponse, AttemptFailure> {
    let url = endpoints.chart_url(&request.resolved_symbol);
    let body = fetch_response_body(client, timeout, &url, CHART_SOURCE).await?;
    let parsed: ChartEndpointResponse = serde_json::from_str(&body).map_err(|e| {
        AttemptFailure::upstream(CHART_SOURCE, format!("invalid JSON response: {e}"))
    })?;

    if let Some(error) = parsed.chart.error {
        let description = error
            .description
            .unwrap_or_else(|| "chart endpoint returned an error".to_string());
        if error
            .code
            .as_deref()
            .is_some_and(|code| code.eq_ignore_ascii_case("Not Found"))
            || description.to_ascii_lowercase().contains("not found")
            || description
                .to_ascii_lowercase()
                .contains("symbol may be delisted")
        {
            return Err(AttemptFailure::not_found(CHART_SOURCE, description));
        }
        return Err(AttemptFailure::upstream(CHART_SOURCE, description));
    }

    let result = parsed
        .chart
        .result
        .and_then(|mut entries| entries.drain(..).next())
        .ok_or_else(|| {
            AttemptFailure::not_found(
                CHART_SOURCE,
                format!("no chart data for symbol '{}'", request.resolved_symbol),
            )
        })?;

    let meta = result.meta;
    let price = meta.regular_market_price.ok_or_else(|| {
        AttemptFailure::upstream(CHART_SOURCE, "response missing regularMarketPrice")
    })?;
    let previous_close = meta.chart_previous_close.or(meta.previous_close);
    let change = compute_change(price, previous_close);
    let change_percent = compute_change_percent(price, previous_close);

    Ok(FinanceQuoteResponse {
        requested_ticker: request.requested_ticker.clone(),
        ticker: meta.symbol,
        name: meta.long_name.or(meta.short_name),
        price,
        currency: meta.currency,
        change,
        change_percent,
        previous_close,
        market_state: None,
        quote_type: meta.instrument_type,
        exchange: meta.full_exchange_name.or(meta.exchange_name),
        market_time: meta.regular_market_time,
        source: CHART_SOURCE.to_string(),
        fallback_used: true,
    })
}

async fn fetch_response_body(
    client: &Client,
    timeout: Duration,
    url: &str,
    endpoint: &'static str,
) -> Result<String, AttemptFailure> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                AttemptFailure::timeout(endpoint)
            } else {
                AttemptFailure::upstream(endpoint, format!("request failed: {err}"))
            }
        })?;

    let status = response.status();
    let body = response.text().await.map_err(|err| {
        if err.is_timeout() {
            AttemptFailure::timeout(endpoint)
        } else {
            AttemptFailure::upstream(endpoint, format!("failed to read response body: {err}"))
        }
    })?;

    if !status.is_success() {
        return Err(status_failure(endpoint, status, &body));
    }

    Ok(body)
}

fn status_failure(endpoint: &'static str, status: StatusCode, body: &str) -> AttemptFailure {
    if endpoint == CHART_SOURCE && status == StatusCode::NOT_FOUND {
        return AttemptFailure::not_found(endpoint, format!("HTTP {}", status.as_u16()));
    }

    let snippet = body.trim();
    let detail = if snippet.is_empty() {
        format!("HTTP {}", status.as_u16())
    } else {
        format!("HTTP {} ({})", status.as_u16(), truncate_for_error(snippet))
    };

    AttemptFailure::upstream(endpoint, detail)
}

fn finalize_failure(
    request: &FinanceRequest,
    timeout_ms: u64,
    failures: &[AttemptFailure],
) -> ToolError {
    if failures.iter().all(AttemptFailure::is_not_found) {
        return ToolError::invalid_input(format!(
            "Unknown finance ticker '{}'",
            request.requested_ticker
        ));
    }

    if failures.iter().any(AttemptFailure::is_timeout) {
        return ToolError::Timeout {
            seconds: millis_to_timeout_seconds(timeout_ms),
        };
    }

    let detail = failures
        .iter()
        .map(AttemptFailure::summary)
        .collect::<Vec<_>>()
        .join("; ");
    ToolError::execution_failed(format!(
        "Finance lookup failed for '{}': {}",
        request.requested_ticker, detail
    ))
}

fn compute_change(price: f64, previous_close: Option<f64>) -> Option<f64> {
    previous_close.map(|prev| price - prev)
}

fn compute_change_percent(price: f64, previous_close: Option<f64>) -> Option<f64> {
    previous_close.and_then(|prev| {
        if prev.abs() < f64::EPSILON {
            None
        } else {
            Some(((price - prev) / prev) * 100.0)
        }
    })
}

fn millis_to_timeout_seconds(timeout_ms: u64) -> u64 {
    timeout_ms.saturating_add(999) / 1000
}

fn truncate_for_error(text: &str) -> String {
    const MAX_ERROR_CHARS: usize = 120;
    let mut out = String::new();
    for ch in text.chars().take(MAX_ERROR_CHARS) {
        out.push(ch);
    }
    if text.chars().count() > MAX_ERROR_CHARS {
        out.push_str("...");
    }
    out
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteEndpointResponse {
    quote_response: QuoteResponseBody,
}

#[derive(Debug, Deserialize, Serialize)]
struct QuoteResponseBody {
    result: Vec<QuoteItem>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteItem {
    symbol: String,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    long_name: Option<String>,
    #[serde(default)]
    regular_market_price: Option<f64>,
    #[serde(default)]
    regular_market_change: Option<f64>,
    #[serde(default)]
    regular_market_change_percent: Option<f64>,
    #[serde(default)]
    regular_market_previous_close: Option<f64>,
    #[serde(default)]
    #[serde(serialize_with = "serialize_timestamp")]
    regular_market_time: Option<i64>,
    #[serde(default)]
    market_state: Option<String>,
    #[serde(default)]
    quote_type: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    exchange: Option<String>,
    #[serde(default)]
    full_exchange_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChartEndpointResponse {
    chart: ChartBody,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChartBody {
    #[serde(default)]
    result: Option<Vec<ChartResult>>,
    #[serde(default)]
    error: Option<ChartErrorBody>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChartResult {
    meta: ChartMeta,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChartMeta {
    symbol: String,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    long_name: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    regular_market_price: Option<f64>,
    #[serde(default)]
    #[serde(serialize_with = "serialize_timestamp")]
    regular_market_time: Option<i64>,
    #[serde(default)]
    chart_previous_close: Option<f64>,
    #[serde(default)]
    previous_close: Option<f64>,
    #[serde(default)]
    instrument_type: Option<String>,
    #[serde(default)]
    exchange_name: Option<String>,
    #[serde(default)]
    full_exchange_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChartErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn tool_with_server(server: &MockServer) -> FinanceTool {
        FinanceTool::with_endpoints(
            server.uri().to_string() + "/quote",
            server.uri().to_string() + "/chart",
        )
    }

    fn context() -> (ToolContext, tempfile::TempDir) {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().to_path_buf();
        let ctx = ToolContext::new(path);
        (ctx, tmp)
    }

    #[tokio::test]
    async fn finance_uses_quote_endpoint_when_available() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quoteResponse": {
                    "result": [{
                        "symbol": "AAPL",
                        "shortName": "Apple Inc.",
                        "regularMarketPrice": 189.23,
                        "regularMarketChange": 1.12,
                        "regularMarketChangePercent": 0.595,
                        "regularMarketPreviousClose": 188.11,
                        "regularMarketTime": 1_710_000_000,
                        "marketState": "REGULAR",
                        "quoteType": "EQUITY",
                        "currency": "USD",
                        "fullExchangeName": "NasdaqGS"
                    }]
                }
            })))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let result = tool
            .execute(json!({"ticker": "aapl"}), &context().0)
            .await
            .expect("finance quote should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&result.content).expect("tool output should be json");
        assert_eq!(parsed["requested_ticker"], "AAPL");
        assert_eq!(parsed["ticker"], "AAPL");
        assert_eq!(parsed["source"], QUOTE_SOURCE);
        assert_eq!(parsed["fallback_used"], false);
        assert_eq!(parsed["price"], 189.23);
    }

    #[tokio::test]
    async fn finance_falls_back_to_chart_for_btc() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "BTC-USD"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/BTC-USD"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "chart": {
                    "result": [{
                        "meta": {
                            "symbol": "BTC-USD",
                            "longName": "Bitcoin USD",
                            "currency": "USD",
                            "regularMarketPrice": 73474.88,
                            "regularMarketTime": 1_710_000_001,
                            "chartPreviousClose": 72974.19,
                            "instrumentType": "CRYPTOCURRENCY",
                            "fullExchangeName": "CCC"
                        }
                    }],
                    "error": null
                }
            })))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let result = tool
            .execute(json!({"ticker": "BTC", "type": "crypto"}), &context().0)
            .await
            .expect("finance chart fallback should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&result.content).expect("tool output should be json");
        assert_eq!(parsed["requested_ticker"], "BTC");
        assert_eq!(parsed["ticker"], "BTC-USD");
        assert_eq!(parsed["source"], CHART_SOURCE);
        assert_eq!(parsed["fallback_used"], true);
        assert_eq!(parsed["quote_type"], "CRYPTOCURRENCY");
    }

    #[tokio::test]
    async fn finance_reports_invalid_symbol() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "NOTREAL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quoteResponse": {
                    "result": []
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/NOTREAL"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "NOTREAL"}), &context().0)
            .await
            .expect_err("invalid symbol should error");

        assert!(matches!(err, ToolError::InvalidInput { .. }));
        assert!(err.to_string().contains("NOTREAL"));
    }

    #[tokio::test]
    async fn finance_reports_upstream_failure_after_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "SPY"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/SPY"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "SPY"}), &context().0)
            .await
            .expect_err("double upstream failure should error");

        match err {
            ToolError::ExecutionFailed { message, .. } => {
                assert!(message.contains(QUOTE_SOURCE));
                assert!(message.contains("HTTP 401"));
                assert!(message.contains(CHART_SOURCE));
                assert!(message.contains("HTTP 503"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn finance_does_not_mask_upstream_failure_with_chart_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "SPY"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/SPY"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "SPY"}), &context().0)
            .await
            .expect_err("mixed upstream/not-found failures should not look like an invalid symbol");

        match err {
            ToolError::ExecutionFailed { message, .. } => {
                assert!(message.contains(QUOTE_SOURCE));
                assert!(message.contains("HTTP 503"));
                assert!(message.contains(CHART_SOURCE));
                assert!(message.contains("HTTP 404"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn finance_does_not_mask_quote_auth_failure_with_unknown_symbol() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "SPY"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/SPY"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "SPY"}), &context().0)
            .await
            .expect_err("quote auth failures should not collapse into invalid input");

        match err {
            ToolError::ExecutionFailed { message, .. } => {
                assert!(message.contains(QUOTE_SOURCE));
                assert!(message.contains("HTTP 401"));
                assert!(message.contains(CHART_SOURCE));
                assert!(message.contains("HTTP 404"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn finance_reports_timeout_when_fallback_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/AAPL"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(250))
                    .set_body_json(json!({
                        "chart": {
                            "result": [{
                                "meta": {
                                    "symbol": "AAPL",
                                    "regularMarketPrice": 260.48,
                                    "chartPreviousClose": 255.92
                                }
                            }],
                            "error": null
                        }
                    })),
            )
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "AAPL", "timeout_ms": 1}), &context().0)
            .await
            .expect_err("timeout should surface cleanly");

        assert!(matches!(err, ToolError::Timeout { .. }));
    }

    /// #6557 D03-m3: the chart fallback used to get a fresh full timeout, so
    /// a lookup could run for twice `timeout_ms`. Here the quote attempt
    /// spends the whole budget; the fallback, which would answer in 150ms,
    /// must not be given a second budget.
    #[tokio::test]
    async fn finance_fallback_shares_one_timeout_budget() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(3_000))
                    .set_body_json(json!({"quoteResponse": {"result": []}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/AAPL"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(150))
                    .set_body_json(json!({
                        "chart": {
                            "result": [{
                                "meta": {
                                    "symbol": "AAPL",
                                    "regularMarketPrice": 260.48,
                                    "chartPreviousClose": 255.92
                                }
                            }],
                            "error": null
                        }
                    })),
            )
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let started = std::time::Instant::now();
        let err = tool
            .execute(json!({"ticker": "AAPL", "timeout_ms": 500}), &context().0)
            .await
            .expect_err("the shared budget is spent before the fallback can answer");
        assert!(matches!(err, ToolError::Timeout { seconds: 1 }), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_millis(2_500),
            "lookup overran its budget: {:?}",
            started.elapsed()
        );
    }

    /// JSON cannot carry NaN or infinity, and an overflowing literal is a
    /// parse error rather than an infinite price.
    #[tokio::test]
    async fn finance_rejects_an_overflowing_price_literal() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"quoteResponse":{"result":[{"symbol":"AAPL","regularMarketPrice":1e400}]}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/AAPL"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = tool_with_server(&server)
            .execute(json!({"ticker": "AAPL"}), &context().0)
            .await
            .expect_err("an overflowing price is not a quote");
        assert!(err.to_string().contains("invalid JSON response"), "{err}");
    }

    #[tokio::test]
    async fn finance_prefers_timeout_over_unknown_symbol_when_any_attempt_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/quote"))
            .and(query_param("symbols", "AAPL"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(250))
                    .set_body_json(json!({
                        "quoteResponse": {
                            "result": [{
                                "symbol": "AAPL",
                                "regularMarketPrice": 189.23
                            }]
                        }
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/chart/AAPL"))
            .and(query_param("interval", "1d"))
            .and(query_param("range", "5d"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let tool = tool_with_server(&server);
        let err = tool
            .execute(json!({"ticker": "AAPL", "timeout_ms": 1}), &context().0)
            .await
            .expect_err("timeout should win over a later chart not-found");

        assert!(matches!(err, ToolError::Timeout { .. }));
    }

    #[test]
    fn finance_schema_allows_ticker_or_symbol() {
        let schema = FinanceTool::new().input_schema();
        let any_of = schema["anyOf"]
            .as_array()
            .expect("finance schema should advertise alternate required fields");

        assert_eq!(any_of.len(), 2);
        assert_eq!(any_of[0]["required"], json!(["ticker"]));
        assert_eq!(any_of[1]["required"], json!(["symbol"]));
    }

    fn denied_context_for(host: &str) -> (ToolContext, tempfile::TempDir) {
        use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};
        let (ctx, tmp) = context();
        let policy = NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: vec![host.to_string()],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        (
            ctx.with_network_policy(NetworkPolicyDecider::new(policy, None)),
            tmp,
        )
    }

    #[tokio::test]
    async fn finance_fails_closed_when_network_policy_denies_endpoint_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quoteResponse": {"result": []}
            })))
            .mount(&server)
            .await;

        let host = reqwest::Url::parse(&server.uri())
            .expect("mock server URL")
            .host_str()
            .expect("mock server host")
            .to_string();
        let (blocked, _tmp) = denied_context_for(&host);

        let tool = tool_with_server(&server);
        let error = tool
            .execute(json!({"ticker": "AAPL"}), &blocked)
            .await
            .expect_err("denied host must fail closed");
        assert!(
            error.to_string().contains("blocked by network policy"),
            "{error}"
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("recorded requests")
                .len(),
            0,
            "no request may leave before the policy check"
        );
    }

    #[tokio::test]
    async fn finance_fails_closed_on_prompt_when_default_is_prompt() {
        let server = MockServer::start().await;

        // default = prompt with no allow list: the undecided host must fail
        // closed with the approval hint, never with a silent request.
        let (ctx, tmp) = context();
        use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};
        let policy = NetworkPolicy {
            default: Decision::Prompt.into(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let blocked = ctx.with_network_policy(NetworkPolicyDecider::new(policy, None));
        drop(tmp);

        let tool = tool_with_server(&server);
        let error = tool
            .execute(json!({"ticker": "AAPL"}), &blocked)
            .await
            .expect_err("undecided host must not reach the endpoint");
        assert!(
            error.to_string().contains("requires approval"),
            "unexpected error: {error}"
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("recorded requests")
                .len(),
            0
        );
    }
}

#[cfg(test)]
#[path = "stock_host_tests.rs"]
mod stock_host_tests;
