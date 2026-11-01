//! Small session-scoped TTL caches for web searches and fetched bodies.

use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;

use super::contract::{BackendId, SearchQuery, SearchResponse};

const FETCH_CACHE_ENTRIES: usize = 256;
const SEARCH_CACHE_ENTRIES: usize = 128;
const FETCH_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

static FETCH_CACHE: OnceLock<Mutex<LruCache<FetchCacheKey, FetchCacheEntry>>> = OnceLock::new();
static SEARCH_CACHE: OnceLock<Mutex<LruCache<SearchCacheKey, SearchCacheEntry>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FetchCacheKey {
    namespace: String,
    url: String,
    accept: String,
}

#[derive(Debug, Clone)]
pub struct CachedFetch {
    pub url: String,
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub content_type: String,
    pub bytes: Arc<Vec<u8>>,
    pub truncated: bool,
    pub redirects: usize,
}

#[derive(Debug, Clone)]
struct FetchCacheEntry {
    fetched_at: Instant,
    payload: CachedFetch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchCacheKey {
    namespace: String,
    initial_backend: BackendId,
    base_url: Option<String>,
    query: SearchQuery,
}

#[derive(Debug, Clone)]
struct SearchCacheEntry {
    searched_at: Instant,
    response: SearchResponse,
}

fn cache() -> &'static Mutex<LruCache<FetchCacheKey, FetchCacheEntry>> {
    FETCH_CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(FETCH_CACHE_ENTRIES).expect("non-zero cache capacity"),
        ))
    })
}

fn search_cache() -> &'static Mutex<LruCache<SearchCacheKey, SearchCacheEntry>> {
    SEARCH_CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(SEARCH_CACHE_ENTRIES).expect("non-zero search cache capacity"),
        ))
    })
}

fn search_key(
    namespace: &str,
    initial_backend: BackendId,
    base_url: Option<&str>,
    query: &SearchQuery,
) -> SearchCacheKey {
    SearchCacheKey {
        namespace: namespace.to_string(),
        initial_backend,
        base_url: base_url.map(str::to_string),
        query: query.clone(),
    }
}

fn key(namespace: &str, url: &reqwest::Url, accept: &str) -> FetchCacheKey {
    let mut canonical = url.clone();
    canonical.set_fragment(None);
    FetchCacheKey {
        namespace: namespace.to_string(),
        url: canonical.to_string(),
        accept: accept.to_string(),
    }
}

pub fn get(
    namespace: &str,
    url: &reqwest::Url,
    accept: &str,
    max_bytes: usize,
) -> Option<CachedFetch> {
    let key = key(namespace, url, accept);
    let mut cache = cache().lock();
    let entry = cache.get(&key)?.clone();
    if entry.fetched_at.elapsed() > FETCH_CACHE_TTL {
        cache.pop(&key);
        return None;
    }

    // A truncated entry can answer an equal or smaller request. Asking for
    // more is an explicit refetch so the cached cap never becomes permanent.
    if entry.payload.truncated && max_bytes > entry.payload.bytes.len() {
        cache.pop(&key);
        return None;
    }

    let mut payload = entry.payload;
    if payload.bytes.len() > max_bytes {
        payload.bytes = Arc::new(payload.bytes[..max_bytes].to_vec());
        payload.truncated = true;
    }
    Some(payload)
}

pub fn insert(namespace: &str, url: &reqwest::Url, accept: &str, payload: CachedFetch) {
    cache().lock().put(
        key(namespace, url, accept),
        FetchCacheEntry {
            fetched_at: Instant::now(),
            payload,
        },
    );
}

pub fn get_search(
    namespace: &str,
    initial_backend: BackendId,
    base_url: Option<&str>,
    query: &SearchQuery,
) -> Option<SearchResponse> {
    let key = search_key(namespace, initial_backend, base_url, query);
    let mut cache = search_cache().lock();
    let entry = cache.get(&key)?.clone();
    if entry.searched_at.elapsed() > SEARCH_CACHE_TTL {
        cache.pop(&key);
        return None;
    }

    Some(entry.response)
}

pub fn insert_search(
    namespace: &str,
    initial_backend: BackendId,
    base_url: Option<&str>,
    query: &SearchQuery,
    response: SearchResponse,
) {
    search_cache().lock().put(
        search_key(namespace, initial_backend, base_url, query),
        SearchCacheEntry {
            searched_at: Instant::now(),
            response,
        },
    );
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset() {
    cache().lock().clear();
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_search() {
    search_cache().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FETCH_CACHE` is process-global and `reset()` empties it for every
    /// thread, so the tests that reset it cannot run concurrently: one test's
    /// `reset` landing between another's `insert` and its assertion takes the
    /// entry out from under it, and the failure reads as a cache-scoping bug
    /// rather than the test collision it is. Serializing the resetters is
    /// cheaper than teaching the cache about tests.
    fn fetch_cache_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn payload(bytes: &[u8], truncated: bool) -> CachedFetch {
        CachedFetch {
            url: "https://example.com/doc".to_string(),
            status: 200,
            headers: Default::default(),
            content_type: "text/plain".to_string(),
            bytes: Arc::new(bytes.to_vec()),
            truncated,
            redirects: 0,
        }
    }

    fn search_response(query: SearchQuery) -> SearchResponse {
        use super::super::contract::{
            HonoredQueryCapabilities, QueryCapabilities, SearchReceipt, SearchResult,
        };

        SearchResponse {
            query: query.query.clone(),
            source: "duckduckgo".to_string(),
            count: 1,
            message: "Found 1 result(s)".to_string(),
            results: vec![SearchResult::new(
                1,
                "Cached result".to_string(),
                "https://example.com/result".to_string(),
                None,
                None,
            )],
            unverified_synthesis: None,
            receipt: SearchReceipt {
                backend: BackendId::DuckDuckGo,
                backend_detail: None,
                requested: query,
                capabilities: QueryCapabilities::count_only(),
                honored: HonoredQueryCapabilities {
                    max_results: true,
                    ..HonoredQueryCapabilities::default()
                },
                degraded: Vec::new(),
                latency_ms: 4,
                cache_hit: false,
            },
        }
    }

    #[test]
    fn truncated_entry_refetches_only_when_request_asks_for_more() {
        let _guard = fetch_cache_test_guard();
        reset();
        let url = reqwest::Url::parse("https://example.com/doc#fragment").unwrap();
        insert("cache-unit", &url, "text/plain", payload(b"12345", true));

        let same = get("cache-unit", &url, "text/plain", 5).expect("same cap hit");
        assert!(same.truncated);
        let smaller = get("cache-unit", &url, "text/plain", 3).expect("smaller cap hit");
        assert_eq!(&*smaller.bytes, b"123");
        assert!(smaller.truncated);
        assert!(get("cache-unit", &url, "text/plain", 6).is_none());
    }

    #[test]
    fn cache_is_scoped_by_session_and_accept_header() {
        let _guard = fetch_cache_test_guard();
        reset();
        let url = reqwest::Url::parse("https://example.com/doc").unwrap();
        insert("session-a", &url, "text/html", payload(b"body", false));

        assert!(get("session-a", &url, "text/html", 10).is_some());
        assert!(get("session-b", &url, "text/html", 10).is_none());
        assert!(get("session-a", &url, "application/json", 10).is_none());
    }

    #[test]
    fn search_cache_is_scoped_by_session_backend_endpoint_and_query() {
        reset_search();
        let query = SearchQuery::new("cached query".to_string(), 5, None, Vec::new(), None);
        insert_search(
            "session-a",
            BackendId::Tavily,
            None,
            &query,
            search_response(query.clone()),
        );

        assert!(get_search("session-a", BackendId::Tavily, None, &query).is_some());
        assert!(get_search("session-b", BackendId::Tavily, None, &query).is_none());
        assert!(get_search("session-a", BackendId::DuckDuckGo, None, &query).is_none());
        assert!(
            get_search(
                "session-a",
                BackendId::Tavily,
                Some("https://search.example/"),
                &query,
            )
            .is_none()
        );
        let other_query =
            SearchQuery::new("different query".to_string(), 5, None, Vec::new(), None);
        assert!(get_search("session-a", BackendId::Tavily, None, &other_query).is_none());
    }
}
