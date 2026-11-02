//! Provider-neutral web-search request, result, and receipt types.
//!
//! Search backends vary widely in what they accept and return. These types are
//! the model-visible honesty boundary shared by `web_search` and `web.run`:
//! callers can distinguish requested knobs, actually honored behavior, and
//! degraded/post-filtered execution without depending on provider payloads.

use serde::{Deserialize, Serialize};

pub const MAX_SEARCH_RESULTS: u8 = 10;

/// Shared retrieval defaults. `web_search` and `web.run` both derive their
/// search knobs from these so the surfaces cannot drift apart.
pub const DEFAULT_SEARCH_RESULTS: usize = 5;
pub const DEFAULT_SEARCH_TIMEOUT_MS: u64 = 15_000;
pub const MAX_SEARCH_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    ProviderNative,
    Bing,
    #[serde(rename = "duckduckgo")]
    DuckDuckGo,
    Firecrawl,
    Tavily,
    Bocha,
    Metaso,
    Searxng,
    Baidu,
    Volcengine,
    Sofya,
    Serply,
    #[serde(rename = "deepseek")]
    DeepSeek,
}

impl BackendId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderNative => "provider_native",
            Self::Bing => "bing",
            Self::DuckDuckGo => "duckduckgo",
            Self::Firecrawl => "firecrawl",
            Self::Tavily => "tavily",
            Self::Bocha => "bocha",
            Self::Metaso => "metaso",
            Self::Searxng => "searxng",
            Self::Baidu => "baidu",
            Self::Volcengine => "volcengine",
            Self::Sofya => "sofya",
            Self::Serply => "serply",
            Self::DeepSeek => "deepseek",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recency {
    Day,
    Week,
    Month,
    Year,
    Days(u16),
}

impl Recency {
    #[must_use]
    pub const fn days(self) -> u16 {
        match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
            Self::Days(days) => days,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub max_results: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recency: Option<Recency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

impl SearchQuery {
    #[must_use]
    pub fn new(
        query: String,
        max_results: usize,
        recency: Option<Recency>,
        domains: Vec<String>,
        locale: Option<String>,
    ) -> Self {
        let mut domains = domains
            .into_iter()
            .map(|domain| {
                let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
                domain.trim_start_matches("www.").to_string()
            })
            .filter(|domain| !domain.is_empty())
            .collect::<Vec<_>>();
        domains.sort_unstable();
        domains.dedup();
        Self {
            query,
            max_results: u8::try_from(max_results.clamp(1, usize::from(MAX_SEARCH_RESULTS)))
                .unwrap_or(MAX_SEARCH_RESULTS),
            recency,
            domains,
            locale: locale
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryCapabilities {
    pub max_results: CapabilityState,
    pub recency: CapabilityState,
    pub domains: CapabilityState,
    pub locale: CapabilityState,
    pub published_date: CapabilityState,
}

impl QueryCapabilities {
    #[must_use]
    pub const fn count_only() -> Self {
        Self {
            max_results: CapabilityState::Supported,
            recency: CapabilityState::Unsupported,
            domains: CapabilityState::Unsupported,
            locale: CapabilityState::Unsupported,
            published_date: CapabilityState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HonoredQueryCapabilities {
    pub max_results: bool,
    pub recency: bool,
    pub domains: bool,
    pub locale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKnob {
    Recency,
    Domains,
    Locale,
}

impl QueryKnob {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Recency => "recency",
            Self::Domains => "domains",
            Self::Locale => "locale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DegradedReason {
    BackendUnavailable {
        backend: BackendId,
    },
    NoUsableResults {
        backend: BackendId,
    },
    BackendFallback {
        from: BackendId,
        to: BackendId,
    },
    ChallengeDetected {
        backend: BackendId,
    },
    ScrapeFallback {
        from: BackendId,
        to: BackendId,
    },
    KnobIgnored {
        knob: QueryKnob,
    },
    PostFiltered {
        knob: QueryKnob,
    },
    SynthesizedResults,
    /// The provider stopped a native search answer at its output limit, so
    /// the answer is incomplete (#6508).
    AnswerCutByProvider,
    CitationUnverified {
        url: String,
        reason: String,
    },
    CitationUnchecked {
        url: String,
        reason: String,
    },
}

impl DegradedReason {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::BackendUnavailable { backend } => {
                format!("{} was unavailable", backend.as_str())
            }
            Self::NoUsableResults { backend } => {
                format!("{} returned no usable results", backend.as_str())
            }
            Self::BackendFallback { from, to } => format!(
                "{} did not answer; tried {} next",
                from.as_str(),
                to.as_str()
            ),
            Self::ChallengeDetected { backend } => {
                format!("{} returned a bot challenge", backend.as_str())
            }
            Self::ScrapeFallback { from, to } => format!(
                "{} returned no usable results; used {} fallback",
                from.as_str(),
                to.as_str()
            ),
            Self::KnobIgnored { knob } => {
                format!("{} filter was not enforced by this backend", knob.as_str())
            }
            Self::PostFiltered { knob } => {
                format!("results were post-filtered by {}", knob.as_str())
            }
            Self::SynthesizedResults => {
                "results were synthesized by a model-backed search response".to_string()
            }
            Self::AnswerCutByProvider => {
                "the provider stopped the search answer at its output limit; the answer is incomplete"
                    .to_string()
            }
            Self::CitationUnverified { url, reason } => {
                format!("provider citation {url} failed link verification ({reason}) and was removed")
            }
            Self::CitationUnchecked { url, reason } => {
                format!("provider citation {url} was not checked ({reason}) and stays in results unverified")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedSearchEntry {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub published: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub rank: u8,
    /// Session-scoped citation handle. Backends leave this empty; the shared
    /// execution surface mints it before any result crosses the tool boundary.
    pub ref_id: String,
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    pub domain: String,
}

impl SearchResult {
    #[must_use]
    pub fn new(
        rank: usize,
        title: String,
        url: String,
        snippet: Option<String>,
        published: Option<String>,
    ) -> Self {
        let domain = reqwest::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        Self {
            rank: u8::try_from(rank.clamp(1, 255)).unwrap_or(u8::MAX),
            ref_id: String::new(),
            title,
            url,
            snippet,
            published,
            domain,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackendSearch {
    pub backend: BackendId,
    pub source: String,
    pub backend_detail: Option<String>,
    pub results: Vec<SearchResult>,
    pub degraded: Vec<DegradedReason>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReceipt {
    pub backend: BackendId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_detail: Option<String>,
    pub requested: SearchQuery,
    pub capabilities: QueryCapabilities,
    pub honored: HonoredQueryCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<DegradedReason>,
    pub latency_ms: u32,
    pub cache_hit: bool,
}

impl SearchReceipt {
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        if self.degraded.is_empty() {
            None
        } else {
            Some(
                self.degraded
                    .iter()
                    .map(DegradedReason::message)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub source: String,
    pub count: usize,
    pub message: String,
    pub results: Vec<SearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified_synthesis: Option<String>,
    pub receipt: SearchReceipt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_defaults_are_coherent_across_search_and_fetch() {
        use super::super::fetch::{DEFAULT_TIMEOUT, HARD_MAX_TIMEOUT};

        // Search and fetch share one default and one hard-cap timeout so the
        // two halves of the retrieval path behave identically by default.
        assert_eq!(
            u128::from(DEFAULT_SEARCH_TIMEOUT_MS),
            DEFAULT_TIMEOUT.as_millis()
        );
        assert_eq!(
            u128::from(MAX_SEARCH_TIMEOUT_MS),
            HARD_MAX_TIMEOUT.as_millis()
        );
        assert!(DEFAULT_SEARCH_RESULTS <= usize::from(MAX_SEARCH_RESULTS));
    }

    #[test]
    fn search_query_normalizes_domains_and_bounds_count() {
        let query = SearchQuery::new(
            "rust async".to_string(),
            99,
            Some(Recency::Week),
            vec![" WWW.Example.COM. ".to_string(), "example.com".to_string()],
            Some(" en-US ".to_string()),
        );

        assert_eq!(query.max_results, 10);
        assert_eq!(query.domains, vec!["example.com"]);
        assert_eq!(query.locale.as_deref(), Some("en-US"));
        assert_eq!(query.recency.map(Recency::days), Some(7));
    }

    #[test]
    fn normalized_result_derives_domain_and_rank() {
        let result = SearchResult::new(
            2,
            "Example".to_string(),
            "https://Docs.Example.com/path".to_string(),
            Some("summary".to_string()),
            None,
        );

        assert_eq!(result.rank, 2);
        assert_eq!(result.domain, "docs.example.com");
    }

    #[test]
    fn degraded_receipt_is_machine_readable_and_human_visible() {
        let receipt = SearchReceipt {
            backend: BackendId::DuckDuckGo,
            backend_detail: None,
            requested: SearchQuery::new(
                "fresh result".to_string(),
                5,
                Some(Recency::Day),
                Vec::new(),
                None,
            ),
            capabilities: QueryCapabilities::count_only(),
            honored: HonoredQueryCapabilities {
                max_results: true,
                ..HonoredQueryCapabilities::default()
            },
            degraded: vec![DegradedReason::KnobIgnored {
                knob: QueryKnob::Recency,
            }],
            latency_ms: 4,
            cache_hit: false,
        };

        let value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(value["backend"], "duckduckgo");
        assert_eq!(value["degraded"][0]["kind"], "knob_ignored");
        assert!(receipt.warning().expect("warning").contains("recency"));
    }

    #[test]
    fn scrape_fallback_receipt_preserves_backend_transitions() {
        let receipt = SearchReceipt {
            backend: BackendId::Bing,
            backend_detail: None,
            requested: SearchQuery::new("fallback query".to_string(), 5, None, Vec::new(), None),
            capabilities: QueryCapabilities::count_only(),
            honored: HonoredQueryCapabilities {
                max_results: true,
                ..HonoredQueryCapabilities::default()
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
            latency_ms: 12,
            cache_hit: false,
        };

        let value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(value["backend"], "bing");
        assert_eq!(value["degraded"][0]["kind"], "challenge_detected");
        assert_eq!(value["degraded"][0]["backend"], "duckduckgo");
        assert_eq!(value["degraded"][1]["kind"], "scrape_fallback");
        assert_eq!(value["degraded"][1]["from"], "duckduckgo");
        assert_eq!(value["degraded"][1]["to"], "bing");
        let warning = receipt.warning().expect("warning");
        assert!(warning.contains("bot challenge"));
        assert!(warning.contains("used bing fallback"));
    }
}
