//! Search backend selection and the shared async adapter contract.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;

use super::adapter::{self, AdapterFailure, AdapterResult};
use super::contract::{BackendId, BackendSearch, DegradedReason, QueryCapabilities, SearchQuery};
use super::contract::{CapabilityState as QueryCapabilityState, SearchResult};
use crate::client::ProviderNativeSearchRequest;
use crate::tools::spec::{ToolContext, ToolError};
use codewhale_net::network_policy::Decision;
use codewhale_runtime::config::SearchProvider;

const SEARCH_BACKEND_CONFIGURATION_HINT: &str = concat!(
    "Check network access, or configure `[search] provider` and `[search] api_key` in ",
    "config.toml. Keyed providers include tavily, bocha, metaso, baidu, volcengine, serply, deepseek, ",
    "and sofya; metaso also accepts METASO_API_KEY, tavily accepts TAVILY_API_KEY, baidu ",
    "accepts BAIDU_SEARCH_API_KEY, volcengine accepts VOLCENGINE_API_KEY / ",
    "VOLCENGINE_ARK_API_KEY / ARK_API_KEY, serply accepts SERPLY_API_KEY, deepseek accepts ",
    "DEEPSEEK_API_KEY, and sofya ",
    "accepts SOFYA_API_KEY. SearXNG needs a trusted self-hosted `[search] base_url`. For a ",
    "keyless route, use the default `[search] provider = \"firecrawl\"` or ",
    "`[search] provider = \"bing\"`."
);

const LINK_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const LINK_CHECK_CONCURRENCY: usize = 4;
const LINK_CHECK_RESERVE: Duration = Duration::from_millis(250);

#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn capabilities(&self) -> QueryCapabilities;
    async fn search(&self, query: &SearchQuery, deadline: Instant) -> AdapterResult<BackendSearch>;
}

#[derive(Clone, Copy)]
pub struct BackendContext<'a> {
    tool_context: &'a ToolContext,
}

pub enum ConfiguredSearchBackend<'a> {
    Bing(BackendContext<'a>),
    DuckDuckGo(BackendContext<'a>),
    Firecrawl(BackendContext<'a>),
    Tavily(BackendContext<'a>),
    Bocha(BackendContext<'a>),
    Metaso(BackendContext<'a>),
    Searxng(BackendContext<'a>),
    Baidu(BackendContext<'a>),
    Volcengine(BackendContext<'a>),
    Sofya(BackendContext<'a>),
    Serply(BackendContext<'a>),
    DeepSeek(BackendContext<'a>),
}

#[derive(Clone, Copy)]
struct ProviderNativeSearchBackend<'a> {
    context: &'a ToolContext,
}

impl<'a> ConfiguredSearchBackend<'a> {
    #[must_use]
    pub fn from_provider(context: &'a ToolContext, provider: SearchProvider) -> Self {
        let backend = BackendContext {
            tool_context: context,
        };
        match provider {
            SearchProvider::Bing => Self::Bing(backend),
            SearchProvider::DuckDuckGo => Self::DuckDuckGo(backend),
            SearchProvider::Firecrawl => Self::Firecrawl(backend),
            SearchProvider::Tavily => Self::Tavily(backend),
            SearchProvider::Bocha => Self::Bocha(backend),
            SearchProvider::Metaso => Self::Metaso(backend),
            SearchProvider::Searxng => Self::Searxng(backend),
            SearchProvider::Baidu => Self::Baidu(backend),
            SearchProvider::Volcengine => Self::Volcengine(backend),
            SearchProvider::Sofya => Self::Sofya(backend),
            SearchProvider::Serply => Self::Serply(backend),
            SearchProvider::DeepSeek => Self::DeepSeek(backend),
        }
    }

    const fn provider(&self) -> SearchProvider {
        match self {
            Self::Bing(_) => SearchProvider::Bing,
            Self::DuckDuckGo(_) => SearchProvider::DuckDuckGo,
            Self::Firecrawl(_) => SearchProvider::Firecrawl,
            Self::Tavily(_) => SearchProvider::Tavily,
            Self::Bocha(_) => SearchProvider::Bocha,
            Self::Metaso(_) => SearchProvider::Metaso,
            Self::Searxng(_) => SearchProvider::Searxng,
            Self::Baidu(_) => SearchProvider::Baidu,
            Self::Volcengine(_) => SearchProvider::Volcengine,
            Self::Sofya(_) => SearchProvider::Sofya,
            Self::Serply(_) => SearchProvider::Serply,
            Self::DeepSeek(_) => SearchProvider::DeepSeek,
        }
    }

    const fn context(&self) -> &BackendContext<'a> {
        match self {
            Self::Bing(context)
            | Self::DuckDuckGo(context)
            | Self::Firecrawl(context)
            | Self::Tavily(context)
            | Self::Bocha(context)
            | Self::Metaso(context)
            | Self::Searxng(context)
            | Self::Baidu(context)
            | Self::Volcengine(context)
            | Self::Sofya(context)
            | Self::Serply(context)
            | Self::DeepSeek(context) => context,
        }
    }
}

pub struct SearchBackendChain<'a> {
    backends: Vec<Box<dyn SearchBackend + 'a>>,
}

#[derive(Debug)]
pub struct ChainedSearch {
    pub raw: BackendSearch,
    pub capabilities: QueryCapabilities,
}

impl<'a> SearchBackendChain<'a> {
    #[must_use]
    pub fn from_context(context: &'a ToolContext) -> Self {
        let selected = context.search_provider;
        let mut backends: Vec<Box<dyn SearchBackend + 'a>> = Vec::new();
        if should_prepend_provider_native(context) {
            backends.push(Box::new(ProviderNativeSearchBackend { context }));
        }
        backends.push(Box::new(ConfiguredSearchBackend::from_provider(
            context, selected,
        )));
        if !matches!(selected, SearchProvider::Bing | SearchProvider::DuckDuckGo) {
            backends.push(Box::new(ConfiguredSearchBackend::from_provider(
                context,
                SearchProvider::DuckDuckGo,
            )));
        }
        Self { backends }
    }

    #[must_use]
    pub fn initial_backend(&self) -> BackendId {
        self.backends
            .first()
            .expect("a search chain always has a configured backend")
            .id()
    }

    pub async fn search(
        &self,
        query: &SearchQuery,
        deadline: Instant,
        first_attempt_budget: Option<Duration>,
        fallback_budget_after_first: Option<Duration>,
    ) -> AdapterResult<ChainedSearch> {
        let backends = self
            .backends
            .iter()
            .map(|backend| backend.as_ref())
            .collect::<Vec<_>>();
        run_backend_chain(
            &backends,
            query,
            deadline,
            first_attempt_budget,
            fallback_budget_after_first,
        )
        .await
    }
}

fn should_prepend_provider_native(context: &ToolContext) -> bool {
    provider_native_is_available(
        context
            .route_capabilities
            .server_side_web_search
            .is_supported(),
        context.provider_native_search.is_some(),
    )
}

const fn provider_native_is_available(capability_supported: bool, client_present: bool) -> bool {
    capability_supported && client_present
}

async fn run_backend_chain(
    backends: &[&dyn SearchBackend],
    query: &SearchQuery,
    mut deadline: Instant,
    first_attempt_budget: Option<Duration>,
    fallback_budget_after_first: Option<Duration>,
) -> AdapterResult<ChainedSearch> {
    let mut degraded = Vec::new();
    let mut last_empty = None;
    let mut attempted = Vec::new();

    for (index, backend) in backends.iter().enumerate() {
        if index == 1
            && let Some(fallback_budget) = fallback_budget_after_first
        {
            deadline = Instant::now() + fallback_budget.max(Duration::from_millis(1));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let backend_id = backend.id();
        if let Some(previous) = attempted.last() {
            degraded.push(DegradedReason::BackendFallback {
                from: *previous,
                to: backend_id,
            });
        }
        attempted.push(backend_id);

        let attempts_left = u32::try_from(backends.len() - index).unwrap_or(u32::MAX);
        let fair_share = remaining / attempts_left;
        let attempt_budget = if index == 0 {
            first_attempt_budget
                .map(|budget| budget.min(remaining))
                .unwrap_or(fair_share)
        } else {
            fair_share
        }
        .max(Duration::from_millis(1));
        let attempt_deadline = Instant::now() + attempt_budget;

        let pending_host = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let result = tokio::time::timeout(
            attempt_budget,
            adapter::HOST_PENDING.scope(
                std::sync::Arc::clone(&pending_host),
                backend.search(query, attempt_deadline),
            ),
        )
        .await
        .map_err(|_| {
            let error = ToolError::Timeout {
                seconds: u64::try_from(attempt_budget.as_millis())
                    .unwrap_or(u64::MAX)
                    .div_ceil(1_000),
            };
            if pending_host.load(std::sync::atomic::Ordering::SeqCst) {
                AdapterFailure::host(error)
            } else {
                error.into()
            }
        })
        .and_then(std::convert::identity);

        match result {
            Ok(mut raw) => {
                let capabilities = backend.capabilities();
                crate::tools::web_search::apply_domain_constraints(query, capabilities, &mut raw);
                if !raw.results.is_empty() {
                    degraded.append(&mut raw.degraded);
                    raw.degraded = degraded;
                    return Ok(ChainedSearch { raw, capabilities });
                }
                degraded.push(DegradedReason::NoUsableResults {
                    backend: backend_id,
                });
                degraded.append(&mut raw.degraded);
                last_empty = Some((raw, capabilities));
            }
            Err(error) if !error.content() || is_fail_closed(&error.error) => return Err(error),
            Err(error) if backends.len() == 1 => return Err(error),
            Err(_) => degraded.push(DegradedReason::BackendUnavailable {
                backend: backend_id,
            }),
        }
    }

    if let Some((mut raw, capabilities)) = last_empty {
        raw.degraded = degraded;
        return Ok(ChainedSearch { raw, capabilities });
    }

    if attempted.is_empty() {
        return Err((ToolError::Timeout { seconds: 1 }).into());
    }

    let backend_ids = attempted
        .into_iter()
        .map(BackendId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Err((ToolError::not_available(format!(
        "web search backends unavailable: {backend_ids}. {SEARCH_BACKEND_CONFIGURATION_HINT}"
    )))
    .into())
}

const fn is_fail_closed(error: &ToolError) -> bool {
    matches!(
        error,
        ToolError::InvalidInput { .. }
            | ToolError::MissingField { .. }
            | ToolError::PathEscape { .. }
            | ToolError::Cancelled { .. }
            | ToolError::Timeout { seconds: 0 }
            | ToolError::PermissionDenied { .. }
    )
}

#[async_trait]
impl SearchBackend for ConfiguredSearchBackend<'_> {
    fn id(&self) -> BackendId {
        match self.provider() {
            SearchProvider::Bing => BackendId::Bing,
            SearchProvider::DuckDuckGo => BackendId::DuckDuckGo,
            SearchProvider::Firecrawl => BackendId::Firecrawl,
            SearchProvider::Tavily => BackendId::Tavily,
            SearchProvider::Bocha => BackendId::Bocha,
            SearchProvider::Metaso => BackendId::Metaso,
            SearchProvider::Searxng => BackendId::Searxng,
            SearchProvider::Baidu => BackendId::Baidu,
            SearchProvider::Volcengine => BackendId::Volcengine,
            SearchProvider::Sofya => BackendId::Sofya,
            SearchProvider::Serply => BackendId::Serply,
            SearchProvider::DeepSeek => BackendId::DeepSeek,
        }
    }

    fn capabilities(&self) -> QueryCapabilities {
        // All current adapters enforce result count. Recency and locale are
        // forwarded where the backend's API takes them (see `QueryFilters` in
        // `web_search.rs`) — and the keyless Bing and DuckDuckGo scrapes
        // honor `locale` directly in the request (Bing mkt/setlang,
        // DuckDuckGo kl, plus a matching Accept-Language); every other knob
        // is post-filtered by the shared harness or reported as not honored.
        let (recency, locale) = match self.provider() {
            SearchProvider::Firecrawl | SearchProvider::Searxng => (true, true),
            SearchProvider::Tavily => (true, false),
            SearchProvider::Serply => (false, true),
            SearchProvider::Bing | SearchProvider::DuckDuckGo => (false, true),
            _ => (false, false),
        };
        let state = |supported: bool| {
            if supported {
                QueryCapabilityState::Supported
            } else {
                QueryCapabilityState::Unsupported
            }
        };
        QueryCapabilities {
            recency: state(recency),
            locale: state(locale),
            ..QueryCapabilities::count_only()
        }
    }

    async fn search(&self, query: &SearchQuery, deadline: Instant) -> AdapterResult<BackendSearch> {
        crate::tools::web_search::run_backend_search(
            self.provider(),
            query,
            deadline,
            self.context().tool_context,
        )
        .await
    }
}

#[async_trait]
impl SearchBackend for ProviderNativeSearchBackend<'_> {
    fn id(&self) -> BackendId {
        BackendId::ProviderNative
    }

    fn capabilities(&self) -> QueryCapabilities {
        QueryCapabilities {
            max_results: QueryCapabilityState::Supported,
            recency: QueryCapabilityState::Unsupported,
            domains: QueryCapabilityState::Supported,
            locale: QueryCapabilityState::Unsupported,
            published_date: QueryCapabilityState::Unknown,
        }
    }

    async fn search(&self, query: &SearchQuery, deadline: Instant) -> AdapterResult<BackendSearch> {
        if !self
            .context
            .route_capabilities
            .server_side_web_search
            .is_supported()
        {
            return Err((ToolError::not_available(
                "active route does not report provider-native web search",
            ))
            .into());
        }
        let client = self
            .context
            .provider_native_search
            .as_ref()
            .ok_or_else(|| ToolError::not_available("provider-native search client unavailable"))?;
        // Moonshot/Kimi, Z.AI, MiMo, and the Responses-dialect routes cannot
        // express domain filters in their native wire contracts. Declining
        // here must not fail the whole search: report this backend unavailable
        // so the chain falls back to the configured provider or DuckDuckGo,
        // which honor domains natively or through post-filtering.
        let domain_limit = client.maximum_domain_count();
        if !query.domains.is_empty() && domain_limit == Some(0) {
            return Err((ToolError::not_available(format!(
                "{} native web search cannot honor domain filters",
                client.provider().as_str()
            )))
            .into());
        }
        if let Some(maximum) = domain_limit
            && query.domains.len() > maximum
        {
            return Err((ToolError::invalid_input(format!(
                "{} native web search accepts at most {maximum} domains",
                client.provider().as_str()
            )))
            .into());
        }
        if query.recency.is_some() {
            return Err((ToolError::not_available(format!(
                "{} native web search cannot enforce a recency window",
                client.provider().as_str()
            )))
            .into());
        }
        let host = client.host().ok_or_else(|| {
            ToolError::execution_failed("provider-native search endpoint has no valid host")
        })?;
        crate::tools::web_search::check_policy(
            self.context.network_policy.as_ref(),
            host.as_str(),
        )?;
        let response = client
            .search(&ProviderNativeSearchRequest {
                query: query.query.clone(),
                max_results: query.max_results,
                domains: query.domains.clone(),
            })
            .await
            .map_err(|error| {
                ToolError::execution_failed(format!(
                    "{} provider-native web search failed: {error}",
                    client.provider().as_str()
                ))
            })?;
        let entries = response
            .citations
            .into_iter()
            .map(|citation| super::contract::CapturedSearchEntry {
                title: citation.title,
                url: citation.url,
                snippet: citation.snippet,
                published: citation.published,
            })
            .collect();
        let results = crate::tools::web_search::normalize_captured_entries(
            entries,
            self.context,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await?
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            SearchResult::new(
                index + 1,
                entry.title,
                entry.url,
                entry.snippet,
                entry.published,
            )
        })
        .collect();
        let mut raw = BackendSearch {
            backend: BackendId::ProviderNative,
            source: format!(
                "provider-native/{}/{}",
                client.provider().as_str(),
                client.model()
            ),
            backend_detail: Some(host),
            results,
            degraded: if response.truncated {
                vec![DegradedReason::AnswerCutByProvider]
            } else {
                Vec::new()
            },
            note: response.answer,
        };
        crate::tools::web_search::apply_domain_constraints(query, self.capabilities(), &mut raw);
        let (results, unverified) = verify_citations(
            std::mem::take(&mut raw.results),
            &query.query,
            self.context,
            deadline,
        )
        .await;
        raw.results = results;
        raw.degraded.extend(unverified);
        Ok(raw)
    }
}

async fn verify_citations(
    results: Vec<SearchResult>,
    query: &str,
    context: &ToolContext,
    deadline: Instant,
) -> (Vec<SearchResult>, Vec<DegradedReason>) {
    let needle = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let needle = needle.as_str();
    let checks = futures_util::stream::iter(results)
        .map(|result| async move {
            let budget = deadline
                .saturating_duration_since(Instant::now())
                .saturating_sub(LINK_CHECK_RESERVE)
                .min(LINK_CHECK_TIMEOUT);
            let outcome = match super::citations::normalize_http_url(&result.url) {
                Some(url) if repeats_query(&url, needle) => LinkCheck::Failed(
                    "the address repeats the search query, so it was not requested".to_string(),
                ),
                Some(_) if budget.is_zero() => LinkCheck::Deadline,
                Some(url) => match host_needing_approval(&url, context) {
                    Some(host) => LinkCheck::NeedsApproval(host),
                    None => {
                        match super::fetch::reachable_status(&url, budget, context, "web_search")
                            .await
                        {
                            Ok(status) => LinkCheck::Status(status),
                            Err(
                                ToolError::PermissionDenied { message }
                                | ToolError::InvalidInput { message },
                            ) => LinkCheck::Failed(message),
                            Err(_) => LinkCheck::Failed("no usable response".to_string()),
                        }
                    }
                },
                None => LinkCheck::Failed("not an http(s) address with a host".to_string()),
            };
            (result, outcome)
        })
        .buffered(LINK_CHECK_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    let mut kept = Vec::new();
    let mut unverified = Vec::new();
    for (result, outcome) in checks {
        let reason = match outcome {
            LinkCheck::Status(status) if (200..300).contains(&status) => {
                kept.push(result);
                continue;
            }
            LinkCheck::Status(status) => format!("HTTP {status}"),
            LinkCheck::Failed(message) => message,
            LinkCheck::Deadline => "not checked before the search deadline".to_string(),
            LinkCheck::NeedsApproval(host) => {
                unverified.push(DegradedReason::CitationUnchecked {
                    url: citation_label(&result.url),
                    reason: format!(
                        "the [network] policy needs approval for {host}; run `/network allow {host}` to have it checked"
                    ),
                });
                kept.push(result);
                continue;
            }
        };
        unverified.push(DegradedReason::CitationUnverified {
            url: citation_label(&result.url),
            reason,
        });
    }
    (kept, unverified)
}

enum LinkCheck {
    Status(u16),
    Failed(String),
    Deadline,
    NeedsApproval(String),
}

fn host_needing_approval(url: &str, context: &ToolContext) -> Option<String> {
    let decider = context.network_policy.as_ref()?;
    let host = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
    matches!(decider.evaluate(&host, "web_search"), Decision::Prompt).then_some(host)
}

fn repeats_query(url: &str, needle: &str) -> bool {
    !needle.is_empty()
        && reqwest::Url::parse(url).is_ok_and(|url| {
            url.query_pairs().any(|(_, value)| {
                value
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(needle)
            })
        })
}

fn citation_label(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct FakeBackend {
        id: BackendId,
        result: Result<Vec<super::super::contract::SearchResult>, ToolError>,
    }

    struct DeadlineBackend {
        id: BackendId,
        observed_budget: Arc<Mutex<Option<Duration>>>,
        delay: Duration,
    }

    #[async_trait]
    impl SearchBackend for FakeBackend {
        fn id(&self) -> BackendId {
            self.id
        }

        fn capabilities(&self) -> QueryCapabilities {
            QueryCapabilities::count_only()
        }

        async fn search(
            &self,
            _query: &SearchQuery,
            _deadline: Instant,
        ) -> AdapterResult<BackendSearch> {
            Ok(BackendSearch {
                backend: self.id,
                source: self.id.as_str().to_string(),
                backend_detail: None,
                results: self.result.clone()?,
                degraded: Vec::new(),
                note: None,
            })
        }
    }

    #[async_trait]
    impl SearchBackend for DeadlineBackend {
        fn id(&self) -> BackendId {
            self.id
        }

        fn capabilities(&self) -> QueryCapabilities {
            QueryCapabilities::count_only()
        }

        async fn search(
            &self,
            _query: &SearchQuery,
            deadline: Instant,
        ) -> AdapterResult<BackendSearch> {
            *self.observed_budget.lock().expect("budget lock") =
                Some(deadline.saturating_duration_since(Instant::now()));
            tokio::time::sleep(self.delay).await;
            Ok(BackendSearch {
                backend: self.id,
                source: self.id.as_str().to_string(),
                backend_detail: None,
                results: vec![result()],
                degraded: Vec::new(),
                note: None,
            })
        }
    }

    fn query() -> SearchQuery {
        SearchQuery::new("bounded chain".to_string(), 5, None, Vec::new(), None)
    }

    fn result() -> super::super::contract::SearchResult {
        super::super::contract::SearchResult::new(
            1,
            "Fallback result".to_string(),
            "https://example.com/result".to_string(),
            None,
            None,
        )
    }

    #[test]
    fn every_configured_provider_maps_to_one_explicit_backend_adapter() {
        let cases = [
            (SearchProvider::Bing, BackendId::Bing),
            (SearchProvider::DuckDuckGo, BackendId::DuckDuckGo),
            (SearchProvider::Firecrawl, BackendId::Firecrawl),
            (SearchProvider::Tavily, BackendId::Tavily),
            (SearchProvider::Bocha, BackendId::Bocha),
            (SearchProvider::Metaso, BackendId::Metaso),
            (SearchProvider::Searxng, BackendId::Searxng),
            (SearchProvider::Baidu, BackendId::Baidu),
            (SearchProvider::Volcengine, BackendId::Volcengine),
            (SearchProvider::Sofya, BackendId::Sofya),
            (SearchProvider::Serply, BackendId::Serply),
            (SearchProvider::DeepSeek, BackendId::DeepSeek),
        ];

        for (provider, expected) in cases {
            let mut context = ToolContext::new(std::path::PathBuf::from("."));
            context.search_provider = provider;
            let backend = ConfiguredSearchBackend::from_provider(&context, provider);
            assert_eq!(backend.id(), expected);
            assert_eq!(
                backend.capabilities().max_results,
                super::super::contract::CapabilityState::Supported
            );
        }
    }

    #[test]
    fn keyless_scrape_backends_declare_locale_support() {
        // The keyless Bing and DuckDuckGo scrapes honor the locale knob in
        // their scrape requests; every other configured adapter must keep
        // reporting locale per its own API so the receipt does not overclaim.
        let cases = [
            (SearchProvider::Bing, QueryCapabilityState::Supported),
            (SearchProvider::DuckDuckGo, QueryCapabilityState::Supported),
            (SearchProvider::Firecrawl, QueryCapabilityState::Supported),
            (SearchProvider::Searxng, QueryCapabilityState::Supported),
            (SearchProvider::Serply, QueryCapabilityState::Supported),
            (SearchProvider::Tavily, QueryCapabilityState::Unsupported),
            (SearchProvider::Bocha, QueryCapabilityState::Unsupported),
            (SearchProvider::Metaso, QueryCapabilityState::Unsupported),
            (SearchProvider::Baidu, QueryCapabilityState::Unsupported),
            (
                SearchProvider::Volcengine,
                QueryCapabilityState::Unsupported,
            ),
            (SearchProvider::Sofya, QueryCapabilityState::Unsupported),
            (SearchProvider::DeepSeek, QueryCapabilityState::Unsupported),
        ];
        for (provider, expected_locale) in cases {
            let mut context = ToolContext::new(std::path::PathBuf::from("."));
            context.search_provider = provider;
            let backend = ConfiguredSearchBackend::from_provider(&context, provider);
            assert_eq!(
                backend.capabilities().locale,
                expected_locale,
                "{provider:?}"
            );
        }
    }

    #[test]
    fn provider_native_is_fail_closed_without_both_fact_and_client() {
        assert!(!provider_native_is_available(false, false));
        assert!(!provider_native_is_available(true, false));
        assert!(!provider_native_is_available(false, true));
        assert!(provider_native_is_available(true, true));
    }

    #[tokio::test]
    async fn unavailable_api_falls_back_with_explicit_receipts() {
        let api = FakeBackend {
            id: BackendId::Tavily,
            result: Err(ToolError::execution_failed(
                "provider detail must stay private",
            )),
        };
        let scrape = FakeBackend {
            id: BackendId::DuckDuckGo,
            result: Ok(vec![result()]),
        };
        let response = run_backend_chain(
            &[&api, &scrape],
            &query(),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect("fallback should succeed");

        assert_eq!(response.raw.backend, BackendId::DuckDuckGo);
        assert_eq!(
            response.raw.degraded,
            vec![
                DegradedReason::BackendUnavailable {
                    backend: BackendId::Tavily,
                },
                DegradedReason::BackendFallback {
                    from: BackendId::Tavily,
                    to: BackendId::DuckDuckGo,
                },
            ]
        );
    }

    #[tokio::test]
    async fn provider_native_to_api_to_scrape_records_every_transition() {
        let native = FakeBackend {
            id: BackendId::ProviderNative,
            result: Err(ToolError::execution_failed("native unavailable")),
        };
        let api = FakeBackend {
            id: BackendId::Tavily,
            result: Err(ToolError::execution_failed("API unavailable")),
        };
        let scrape = FakeBackend {
            id: BackendId::DuckDuckGo,
            result: Ok(vec![result()]),
        };

        let response = run_backend_chain(
            &[&native, &api, &scrape],
            &query(),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect("final scrape fallback should succeed");

        assert_eq!(response.raw.backend, BackendId::DuckDuckGo);
        assert_eq!(
            response.raw.degraded,
            vec![
                DegradedReason::BackendUnavailable {
                    backend: BackendId::ProviderNative,
                },
                DegradedReason::BackendFallback {
                    from: BackendId::ProviderNative,
                    to: BackendId::Tavily,
                },
                DegradedReason::BackendUnavailable {
                    backend: BackendId::Tavily,
                },
                DegradedReason::BackendFallback {
                    from: BackendId::Tavily,
                    to: BackendId::DuckDuckGo,
                },
            ]
        );
    }

    #[tokio::test]
    async fn zero_domain_native_providers_decline_without_failing_the_chain() {
        use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};

        let moonshot_config = {
            let __cw_config_init_0_0 = Some("moonshot".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                moonshot: ProviderConfig {
                    api_key: Some("moonshot-test-key".to_string()),
                    base_url: Some("https://api.moonshot.ai/v1".to_string()),
                    model: Some("kimi-k3".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.route_capabilities.server_side_web_search =
            codewhale_config::route::CapabilityState::Supported;
        context.provider_native_search = Some(
            crate::client::ProviderNativeSearchClient::new(
                crate::client::CodewhaleClient::new(&moonshot_config)
                    .expect("test Moonshot client"),
            )
            .expect("Moonshot native adapter"),
        );
        let backend = ProviderNativeSearchBackend { context: &context };

        let domain_query = SearchQuery::new(
            "bounded chain".to_string(),
            5,
            None,
            vec!["example.com".to_string()],
            None,
        );
        let error = backend
            .search(&domain_query, Instant::now() + Duration::from_secs(1))
            .await
            .expect_err("Moonshot native search must decline domain-filtered queries");
        assert!(
            matches!(error.error, ToolError::NotAvailable { .. }),
            "declining must stay fallback-shaped, not fail-closed: {error:?}"
        );

        let xai_config = {
            let __cw_config_init_1_0 = Some("xai".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                xai: ProviderConfig {
                    api_key: Some("xai-test-key".to_string()),
                    base_url: Some("https://api.x.ai/v1".to_string()),
                    model: Some("grok-4.5".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let mut xai_context = ToolContext::new(tmp.path().to_path_buf());
        xai_context.route_capabilities.server_side_web_search =
            codewhale_config::route::CapabilityState::Supported;
        xai_context.provider_native_search = Some(
            crate::client::ProviderNativeSearchClient::new(
                crate::client::CodewhaleClient::new(&xai_config).expect("test xAI client"),
            )
            .expect("xAI native adapter"),
        );
        let oversized_domain_query = SearchQuery::new(
            "bounded chain".to_string(),
            5,
            None,
            [
                "a.example",
                "b.example",
                "c.example",
                "d.example",
                "e.example",
                "f.example",
            ]
            .iter()
            .map(|domain| domain.to_string())
            .collect(),
            None,
        );
        let error = ProviderNativeSearchBackend {
            context: &xai_context,
        }
        .search(
            &oversized_domain_query,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect_err("too many domains stays a typed user error");
        assert!(
            matches!(error.error, ToolError::InvalidInput { .. }),
            "over the provider limit must stay fail-closed: {error:?}"
        );
    }

    #[tokio::test]
    async fn first_attempt_budget_overrides_the_default_fair_share() {
        let observed_budget = Arc::new(Mutex::new(None));
        let volcengine = DeadlineBackend {
            id: BackendId::Volcengine,
            observed_budget: Arc::clone(&observed_budget),
            delay: Duration::ZERO,
        };
        let fallback = FakeBackend {
            id: BackendId::DuckDuckGo,
            result: Ok(vec![result()]),
        };
        let first_attempt_budget = Duration::from_millis(1_500);
        let response = run_backend_chain(
            &[&volcengine, &fallback],
            &query(),
            Instant::now() + Duration::from_secs(2),
            Some(first_attempt_budget),
            None,
        )
        .await
        .expect("the first backend should complete inside its dedicated budget");

        assert_eq!(response.raw.backend, BackendId::Volcengine);
        let observed = observed_budget
            .lock()
            .expect("budget lock")
            .expect("first backend must observe a deadline");
        assert!(
            observed > Duration::from_millis(1_250),
            "dedicated first-attempt budget should exceed the default one-second fair share: {observed:?}"
        );
        assert!(observed <= first_attempt_budget);
    }

    #[tokio::test]
    async fn provider_native_unused_budget_does_not_extend_fallback_deadline() {
        let native = FakeBackend {
            id: BackendId::ProviderNative,
            result: Err(ToolError::execution_failed("native unavailable")),
        };
        let observed_budget = Arc::new(Mutex::new(None));
        let fallback = DeadlineBackend {
            id: BackendId::DuckDuckGo,
            observed_budget: Arc::clone(&observed_budget),
            delay: Duration::from_millis(200),
        };
        let fallback_budget = Duration::from_millis(30);
        let error = run_backend_chain(
            &[&native, &fallback],
            &query(),
            Instant::now() + Duration::from_millis(500),
            Some(Duration::from_millis(500)),
            Some(fallback_budget),
        )
        .await
        .expect_err("blocking fallback must stop at its own budget");

        assert!(matches!(error.error, ToolError::NotAvailable { .. }));
        let observed = observed_budget
            .lock()
            .expect("budget lock")
            .expect("fallback must observe a deadline");
        assert!(observed <= fallback_budget);
    }

    #[tokio::test]
    async fn all_unavailable_returns_actionable_error_without_private_details() {
        let private_error = "secret provider response";
        let api = FakeBackend {
            id: BackendId::Bocha,
            result: Err(ToolError::execution_failed(private_error)),
        };
        let scrape = FakeBackend {
            id: BackendId::DuckDuckGo,
            result: Err(ToolError::execution_failed("different private response")),
        };
        let error = run_backend_chain(
            &[&api, &scrape],
            &query(),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect_err("all-down chain must fail");
        let message = error.to_string();

        assert!(matches!(error.error, ToolError::NotAvailable { .. }));
        assert!(message.contains("bocha, duckduckgo"));
        for provider in [
            "tavily",
            "bocha",
            "metaso",
            "baidu",
            "volcengine",
            "serply",
            "deepseek",
            "sofya",
        ] {
            assert!(
                message.contains(provider),
                "configuration hint must name {provider}: `{message}`"
            );
        }
        assert!(message.contains("[search] provider"));
        assert!(message.contains("[search] api_key"));
        assert!(message.contains("config.toml"));
        assert!(message.contains("METASO_API_KEY"));
        assert!(message.contains("TAVILY_API_KEY"));
        assert!(message.contains("BAIDU_SEARCH_API_KEY"));
        assert!(message.contains("VOLCENGINE_API_KEY"));
        assert!(message.contains("VOLCENGINE_ARK_API_KEY"));
        assert!(message.contains("ARK_API_KEY"));
        assert!(message.contains("SERPLY_API_KEY"));
        assert!(message.contains("DEEPSEEK_API_KEY"));
        assert!(message.contains("SOFYA_API_KEY"));
        assert!(message.contains("[search] base_url"));
        assert!(message.contains("provider = \"firecrawl\""));
        assert!(message.contains("provider = \"bing\""));
        assert!(!message.contains(private_error));
        assert!(!message.contains("different private response"));
    }

    #[tokio::test]
    async fn policy_failure_does_not_leak_query_to_fallback() {
        struct CountingBackend {
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait]
        impl SearchBackend for CountingBackend {
            fn id(&self) -> BackendId {
                BackendId::DuckDuckGo
            }

            fn capabilities(&self) -> QueryCapabilities {
                QueryCapabilities::count_only()
            }

            async fn search(
                &self,
                _query: &SearchQuery,
                _deadline: Instant,
            ) -> AdapterResult<BackendSearch> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(ToolError::execution_failed("unexpected fallback").into())
            }
        }

        let api = FakeBackend {
            id: BackendId::Searxng,
            result: Err(ToolError::permission_denied("policy blocked")),
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let scrape = CountingBackend {
            calls: Arc::clone(&calls),
        };
        let error = run_backend_chain(
            &[&api, &scrape],
            &query(),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect_err("policy error must fail closed");

        assert!(matches!(error.error, ToolError::PermissionDenied { .. }));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn empty_api_falls_back_and_records_no_usable_results() {
        let api = FakeBackend {
            id: BackendId::Metaso,
            result: Ok(Vec::new()),
        };
        let scrape = FakeBackend {
            id: BackendId::DuckDuckGo,
            result: Ok(vec![result()]),
        };
        let response = run_backend_chain(
            &[&api, &scrape],
            &query(),
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect("empty API response should fall back");

        assert_eq!(
            response.raw.degraded,
            vec![
                DegradedReason::NoUsableResults {
                    backend: BackendId::Metaso,
                },
                DegradedReason::BackendFallback {
                    from: BackendId::Metaso,
                    to: BackendId::DuckDuckGo,
                },
            ]
        );
    }

    #[tokio::test]
    async fn domain_filtered_results_fall_back_before_chain_success() {
        let native = FakeBackend {
            id: BackendId::ProviderNative,
            result: Ok(vec![SearchResult::new(
                1,
                "Outside source".to_string(),
                "https://outside.test/result".to_string(),
                None,
                None,
            )]),
        };
        let configured = FakeBackend {
            id: BackendId::Searxng,
            result: Ok(vec![SearchResult::new(
                1,
                "Matching source".to_string(),
                "https://docs.rs/example/latest/example/".to_string(),
                None,
                None,
            )]),
        };
        let constrained = SearchQuery::new(
            "example docs".to_string(),
            5,
            None,
            vec!["docs.rs".to_string()],
            None,
        );

        let response = run_backend_chain(
            &[&native, &configured],
            &constrained,
            Instant::now() + Duration::from_secs(1),
            None,
            None,
        )
        .await
        .expect("configured backend should satisfy the domain constraint");

        assert_eq!(response.raw.backend, BackendId::Searxng);
        assert_eq!(response.raw.results.len(), 1);
        assert!(response.raw.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::NoUsableResults {
                backend: BackendId::ProviderNative
            }
        )));
        assert!(response.raw.degraded.iter().any(|reason| matches!(
            reason,
            DegradedReason::BackendFallback {
                from: BackendId::ProviderNative,
                to: BackendId::Searxng
            }
        )));
    }
    #[tokio::test]
    async fn adapter_origins_and_selected_host_timeout_stop_backend_fallback() {
        use super::super::adapter::FailureOrigin;
        struct Refusal {
            origin: FailureOrigin,
            delay: bool,
        }
        #[async_trait]
        impl SearchBackend for Refusal {
            fn id(&self) -> BackendId {
                BackendId::Tavily
            }
            fn capabilities(&self) -> QueryCapabilities {
                QueryCapabilities::count_only()
            }
            async fn search(
                &self,
                _query: &SearchQuery,
                _deadline: Instant,
            ) -> AdapterResult<BackendSearch> {
                if self.delay {
                    if self.origin == FailureOrigin::Host {
                        let _ = adapter::HOST_PENDING.try_with(|pending| {
                            pending.store(true, std::sync::atomic::Ordering::SeqCst)
                        });
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                Err(AdapterFailure {
                    origin: self.origin,
                    error: ToolError::execution_failed(
                        "No readable page content was found at fixture",
                    ),
                })
            }
        }
        struct Count(std::sync::atomic::AtomicUsize);
        #[async_trait]
        impl SearchBackend for Count {
            fn id(&self) -> BackendId {
                BackendId::Bing
            }
            fn capabilities(&self) -> QueryCapabilities {
                QueryCapabilities::count_only()
            }
            async fn search(
                &self,
                _query: &SearchQuery,
                _deadline: Instant,
            ) -> AdapterResult<BackendSearch> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(BackendSearch {
                    backend: BackendId::Bing,
                    source: "bing".into(),
                    backend_detail: None,
                    results: vec![result()],
                    degraded: vec![],
                    note: None,
                })
            }
        }
        let query = SearchQuery::new("authorized".into(), 5, None, vec![], None);
        for (origin, delay) in [
            (FailureOrigin::Host, false),
            (FailureOrigin::CaptureGuard, false),
            (FailureOrigin::Host, true),
            (FailureOrigin::ContentOrProvider, false),
            (FailureOrigin::ContentOrProvider, true),
        ] {
            let first = Refusal { origin, delay };
            let second = Count(std::sync::atomic::AtomicUsize::new(0));
            let response = run_backend_chain(
                &[&first, &second],
                &query,
                Instant::now() + Duration::from_millis(100),
                None,
                None,
            )
            .await;
            assert_eq!(response.is_ok(), origin == FailureOrigin::ContentOrProvider);
            assert_eq!(
                second.0.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(origin == FailureOrigin::ContentOrProvider)
            );
        }
    }
}
