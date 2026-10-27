//! Web-search provider configuration types.
//!
//! Self-contained `[search]` table types extracted verbatim from `config.rs`.
//! Re-exported from `crate::config` via `pub use search::*;` so existing
//! `crate::config::SearchProvider` (and sibling) paths resolve unchanged
//! (#3311).

use serde::{Deserialize, Serialize};

/// Search provider enumeration — selects the first backend `web_search` uses.
/// API-backed providers may visibly degrade through DuckDuckGo → Bing after
/// runtime failure or an empty result. Configuration and
/// network-policy errors fail closed without crossing providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    /// Bing HTML scraping. No API key needed.
    Bing,
    /// DuckDuckGo HTML scraping with Bing fallback. No API key needed.
    #[serde(alias = "duckduckgo")]
    DuckDuckGo,
    /// Firecrawl Search API. Works keyless on Firecrawl Cloud with a bounded
    /// per-IP quota; `[search] api_key` or `FIRECRAWL_API_KEY` raises limits.
    #[default]
    Firecrawl,
    /// Tavily AI Search API (<https://tavily.com>). Requires api_key.
    Tavily,
    /// Bocha AI Search API (<https://bochaai.com>). Requires api_key.
    Bocha,
    /// Metaso AI Search API (<https://metaso.cn>). Requires `[search] api_key`
    /// or the `METASO_API_KEY` env var.
    #[serde(alias = "metaso")]
    Metaso,
    /// SearXNG JSON search API. Requires a trusted/self-hosted `base_url`.
    #[serde(alias = "searx", alias = "searx-ng", alias = "searx_ng")]
    Searxng,
    /// Baidu AI Search API (<https://qianfan.baidubce.com>). Requires api_key.
    #[serde(
        alias = "baidu-search",
        alias = "baidu_ai_search",
        alias = "baidu_search",
        alias = "baidu-ai-search"
    )]
    Baidu,
    /// Volcengine Ark web_search via Responses API. Requires api_key.
    /// Free tier: 20K queries/month per API key. Falls back to
    /// `VOLCENGINE_API_KEY` / `VOLCENGINE_ARK_API_KEY` / `ARK_API_KEY`
    /// env vars when `[search] api_key` is not set.
    #[serde(
        alias = "volcengine",
        alias = "ark",
        alias = "volc",
        alias = "volcengine-ark",
        alias = "volcengine_ark",
        alias = "volc-ark"
    )]
    Volcengine,
    /// Sofya web search API (<https://sofya.co>). Requires api_key
    /// (`ay_live_...`). Returns full extracted page content rather than
    /// snippets; falls back to the `SOFYA_API_KEY` env var when
    /// `[search] api_key` is not set.
    Sofya,
    /// Serply Google search API (<https://serply.io>). Requires api_key;
    /// returns Google organic results with snippets. Falls back to the
    /// `SERPLY_API_KEY` env var when `[search] api_key` is not set.
    Serply,
    /// DeepSeek Search API (<https://api-docs.deepseek.com/guides/search>). Requires api_key.
    #[serde(rename = "deepseek", alias = "deep-seek", alias = "deepseek-search")]
    DeepSeek,
}

impl SearchProvider {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bing" => Some(Self::Bing),
            "duckduckgo" | "duck-duck-go" | "duck_duck_go" | "ddg" => Some(Self::DuckDuckGo),
            "firecrawl" | "fire-crawl" | "fire_crawl" => Some(Self::Firecrawl),
            "tavily" => Some(Self::Tavily),
            "bocha" => Some(Self::Bocha),
            "metaso" => Some(Self::Metaso),
            "searxng" | "searx" | "searx-ng" | "searx_ng" => Some(Self::Searxng),
            "baidu" | "baidu-search" | "baidu_search" | "baidu-ai-search" | "baidu_ai_search" => {
                Some(Self::Baidu)
            }
            "volcengine" | "ark" | "volc" | "volcengine-ark" => Some(Self::Volcengine),
            "sofya" => Some(Self::Sofya),
            "serply" => Some(Self::Serply),
            "deepseek" | "deep-seek" | "deepseek-search" => Some(Self::DeepSeek),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
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

    #[must_use]
    pub fn names_hint() -> &'static str {
        "bing, duckduckgo, firecrawl, tavily, bocha, metaso, searxng, baidu, volcengine, sofya, serply, deepseek"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProviderSource {
    Default,
    Config,
    EnvOverride,
    /// Autodetected from a Tavily key signal: `TAVILY_API_KEY`, or a generic
    /// `[search] api_key` / `CODEWHALE_SEARCH_API_KEY` value in the `tvly-`
    /// family. Runtime-only — resolution never writes `[search] provider`.
    TavilyKey,
}

impl SearchProviderSource {
    /// One honest source token for doctor, `/config`, and the runtime GET
    /// route. `tavily key` names where the signal came from without claiming a
    /// disk pin or naming `TAVILY_API_KEY` when the winner was a `tvly-`
    /// generic key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Config => "config",
            Self::EnvOverride => "env override",
            Self::TavilyKey => "tavily key",
        }
    }
}

/// Tavily issues keys in the `tvly-` family; the Tavily error copy already
/// says so. Applied to **generic** keys only — a dedicated `TAVILY_API_KEY`
/// is honored as-is, however it is shaped.
#[must_use]
pub fn looks_like_tavily_key(value: &str) -> bool {
    value.trim().starts_with("tvly-")
}

/// `TAVILY_API_KEY`, read at request time. Deliberately never merged into
/// [`SearchConfig::api_key`]: that generic slot is shared by every provider,
/// so merging would hand a Tavily key to Firecrawl (or the reverse) the moment
/// the operator pins a different provider.
#[must_use]
pub fn tavily_env_key() -> Option<String> {
    std::env::var("TAVILY_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The key a Tavily request should send.
///
/// Dedicated env wins over the generic `[search] api_key` so resolution and
/// the request agree on one key: `[search] api_key` is shared, and a
/// Firecrawl `fc-` / sentinel generic value must never be POSTed to
/// `api.tavily.com`. The generic fallback is prefix-gated, which is what
/// stops `CODEWHALE_SEARCH_API_KEY=doctor-offline-search-sentinel` from
/// autodetecting Tavily.
#[must_use]
pub fn tavily_key_from(search_api_key: Option<&str>) -> Option<String> {
    if let Some(env_key) = tavily_env_key() {
        return Some(env_key);
    }
    search_api_key
        .map(str::trim)
        .filter(|value| !value.is_empty() && looks_like_tavily_key(value))
        .map(str::to_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchProviderResolution {
    pub provider: SearchProvider,
    pub source: SearchProviderSource,
}

/// Web search provider configuration (`[search]` table in config.toml).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SearchConfig {
    /// Search provider. Default: keyless `firecrawl`.
    #[serde(default)]
    pub provider: Option<SearchProvider>,
    /// Optional search endpoint. With `duckduckgo`, this is a
    /// DuckDuckGo-compatible HTML endpoint. With `searxng`, this is the trusted
    /// SearXNG instance root or `/search` endpoint.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Optional for Firecrawl; required for Tavily, Bocha, Metaso, Baidu, Volcengine, Sofya, Serply, or DeepSeek.
    /// Metaso also falls back to the `METASO_API_KEY` env var.
    /// Baidu also falls back to `BAIDU_SEARCH_API_KEY` env var.
    /// Serply also falls back to the `SERPLY_API_KEY` env var.
    /// Volcengine also falls back to `VOLCENGINE_API_KEY` / `VOLCENGINE_ARK_API_KEY` / `ARK_API_KEY` env vars.
    ///
    /// This slot is shared across providers. `TAVILY_API_KEY` is **not**
    /// merged into it — Tavily reads its dedicated env at request time
    /// ([`tavily_key_from`]) so pinning another provider never forwards a
    /// Tavily key, and a `tvly-` value here can autodetect Tavily without a
    /// disk write.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whether the active route's provider-native search runs ahead of the
    /// search provider. Unset: native search leads only when no provider is
    /// configured, so a pinned provider always wins; see
    /// [`crate::config::Config::search_native`].
    #[serde(default)]
    pub native: Option<bool>,
}
