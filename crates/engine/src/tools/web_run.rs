//! Web browsing tool with multi-command support (search/open/click/find/screenshot).
//!
//! This mirrors the Codex harness `web.run` interface so models can use a single
//! tool call to perform multiple web actions and cite sources with ref_ids.

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_u64, required_str,
};
use super::web::extract::{DocumentKind, ExtractedDocument, extract_document};
#[cfg(test)]
use super::web::fetch::fetch_readable_with_initial_pin;
use super::web::fetch::{FetchOptions, HARD_MAX_BYTES, Readable, fetch_readable};
use super::web::overflow::bound_text as bound_web_text;
#[cfg(test)]
use super::web::overflow::inline_char_budget;
use async_trait::async_trait;
#[cfg(test)]
use codewhale_net::guard::DnsPin;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

use parking_lot::{RwLock, RwLockWriteGuard};

use super::web::contract::{
    DEFAULT_SEARCH_RESULTS, DEFAULT_SEARCH_TIMEOUT_MS, MAX_SEARCH_RESULTS, MAX_SEARCH_TIMEOUT_MS,
    Recency, SearchQuery, SearchReceipt, SearchResult as NormalizedSearchResult,
};
use super::web::scrape::BROWSER_USER_AGENT as USER_AGENT;
use super::web_search::{domain_matches, execute_search};

// Search and open share the retrieval-path defaults from `web::contract` and
// `web::fetch` so `web.run` cannot drift from `web_search` / `fetch_url`.
const DEFAULT_OPEN_TIMEOUT_MS: u64 = super::web::fetch::DEFAULT_TIMEOUT.as_millis() as u64;
const MAX_WEB_RUN_SESSIONS: usize = 64;
const MAX_PAGES_PER_SESSION: usize = 256;
const WEB_RUN_SESSION_TTL: Duration = Duration::from_secs(30 * 60);

static WEB_RUN_STATE: OnceLock<WebRunCache> = OnceLock::new();

#[derive(Default)]
struct WebRunCache {
    sessions: RwLock<HashMap<String, WebRunSessionState>>,
    pages: RwLock<HashMap<String, StoredWebPage>>,
}

#[derive(Default)]
struct WebRunState {
    sessions: HashMap<String, WebRunSessionState>,
    pages: HashMap<String, StoredWebPage>,
}

struct WebRunSessionState {
    next_turn: u64,
    refs: VecDeque<String>,
    last_access: Instant,
    /// Hosts that refused to serve a page this session (HTTP 401/403 even
    /// after the browser-agent fallback). Later search results from them are
    /// ranked after sources that are likely to load.
    refusing_hosts: HashSet<String>,
}

impl Default for WebRunSessionState {
    fn default() -> Self {
        Self {
            next_turn: 0,
            refs: VecDeque::new(),
            last_access: Instant::now(),
            refusing_hosts: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct StoredWebPage {
    namespace: String,
    page: Arc<WebPage>,
}

impl WebRunState {
    fn cleanup(&mut self) {
        let now = Instant::now();
        let expired = self
            .sessions
            .iter()
            .filter_map(|(namespace, session)| {
                if now.duration_since(session.last_access) > WEB_RUN_SESSION_TTL {
                    Some(namespace.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for namespace in expired {
            self.remove_session(&namespace);
        }

        while self.sessions.len() > MAX_WEB_RUN_SESSIONS {
            let Some(oldest_namespace) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.last_access)
                .map(|(namespace, _)| namespace.clone())
            else {
                break;
            };
            self.remove_session(&oldest_namespace);
        }
    }

    fn remove_session(&mut self, namespace: &str) {
        if let Some(session) = self.sessions.remove(namespace) {
            for ref_id in session.refs {
                self.pages.remove(&ref_id);
            }
        }
    }

    fn touch_session(&mut self, namespace: &str) {
        self.cleanup();
        if !self.sessions.contains_key(namespace)
            && self.sessions.len() >= MAX_WEB_RUN_SESSIONS
            && let Some(oldest_namespace) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.last_access)
                .map(|(existing_namespace, _)| existing_namespace.clone())
        {
            self.remove_session(&oldest_namespace);
        }

        let session = self.sessions.entry(namespace.to_string()).or_default();
        session.last_access = Instant::now();
    }

    fn next_turn(&mut self, namespace: &str) -> u64 {
        self.touch_session(namespace);
        let session = self
            .sessions
            .get_mut(namespace)
            .expect("session should exist after touch");
        let current = session.next_turn;
        session.next_turn = session.next_turn.saturating_add(1);
        current
    }

    fn note_refusing_host(&mut self, namespace: &str, host: &str) {
        self.touch_session(namespace);
        if let Some(session) = self.sessions.get_mut(namespace) {
            session.refusing_hosts.insert(host.to_string());
        }
    }

    fn refusing_hosts(&self, namespace: &str) -> HashSet<String> {
        self.sessions
            .get(namespace)
            .map(|session| session.refusing_hosts.clone())
            .unwrap_or_default()
    }

    fn store_page(&mut self, namespace: &str, ref_id: &str, page: WebPage) {
        self.touch_session(namespace);
        let mut evicted_refs = Vec::new();
        {
            let session = self
                .sessions
                .get_mut(namespace)
                .expect("session should exist after touch");
            if let Some(existing_idx) = session.refs.iter().position(|existing| existing == ref_id)
            {
                session.refs.remove(existing_idx);
            }
            session.refs.push_back(ref_id.to_string());

            while session.refs.len() > MAX_PAGES_PER_SESSION {
                if let Some(evicted_ref) = session.refs.pop_front() {
                    evicted_refs.push(evicted_ref);
                }
            }
        }

        self.pages.insert(
            ref_id.to_string(),
            StoredWebPage {
                namespace: namespace.to_string(),
                page: Arc::new(page),
            },
        );
        for evicted_ref in evicted_refs {
            self.pages.remove(&evicted_ref);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct WebLink {
    id: usize,
    url: String,
    text: String,
}

#[derive(Debug, Clone)]
struct WebPage {
    url: String,
    title: Option<String>,
    content_type: Option<String>,
    lines: Vec<String>,
    links: Vec<WebLink>,
    pdf_pages: Option<Vec<Vec<String>>>,
    truncated: bool,
    partial: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ResponseLength {
    Short,
    Medium,
    Long,
}

impl ResponseLength {
    fn from_input(input: Option<&Value>) -> Self {
        let raw = input.and_then(|v| v.as_str()).unwrap_or("medium");
        match raw.to_lowercase().as_str() {
            "short" => Self::Short,
            "long" => Self::Long,
            _ => Self::Medium,
        }
    }

    fn view_lines(self) -> usize {
        match self {
            Self::Short => 40,
            Self::Medium => 80,
            Self::Long => 160,
        }
    }

    fn wrap_width(self) -> usize {
        match self {
            Self::Short => 88,
            Self::Medium => 110,
            Self::Long => 140,
        }
    }

    fn max_results(self) -> usize {
        match self {
            Self::Short => DEFAULT_SEARCH_RESULTS,
            Self::Medium => 8,
            Self::Long => usize::from(MAX_SEARCH_RESULTS),
        }
    }

    fn max_find_matches(self) -> usize {
        match self {
            Self::Short => 8,
            Self::Medium => 15,
            Self::Long => 30,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct WebRunSearchResult {
    ref_id: String,
    query: String,
    source: String,
    count: usize,
    results: Vec<NormalizedSearchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    receipt: SearchReceipt,
}

#[derive(Debug, Clone, Serialize)]
struct PageViewResult {
    ref_id: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    content: String,
    links: Vec<WebLink>,
}

#[derive(Debug, Clone, Serialize)]
struct FindMatch {
    line: usize,
    text: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
struct FindResult {
    ref_id: String,
    pattern: String,
    count: usize,
    matches: Vec<FindMatch>,
}

#[derive(Debug, Clone, Serialize)]
struct ScreenshotResult {
    ref_id: String,
    pageno: usize,
    total_pages: usize,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct ImageResultEntry {
    ref_id: String,
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct ImageQueryResult {
    query: String,
    source: String,
    count: usize,
    results: Vec<ImageResultEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct WebRunOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    search_query: Option<Vec<WebRunSearchResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_query: Option<Vec<ImageQueryResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<Vec<PageViewResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    click: Option<Vec<PageViewResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    find: Option<Vec<FindResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<Vec<ScreenshotResult>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    warnings: Vec<String>,
    /// Every page this call tried to open or click, loaded ones first, so the
    /// model cites what actually loaded and moves past what did not.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    sources: Vec<SourceEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceStatus {
    Loaded,
    /// The site answered but would not serve a readable page (403, 404,
    /// script-only body). Not retryable as-is; use another source.
    Unavailable,
    /// Network or server trouble (timeout, 5xx, 429). May load on a later try.
    Transient,
}

#[derive(Debug, Clone, Serialize)]
struct SourceEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_id: Option<String>,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    status: SourceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl SourceEntry {
    fn loaded(ref_id: &str, page: &WebPage) -> Self {
        Self {
            ref_id: Some(ref_id.to_string()),
            url: page.url.clone(),
            title: page.title.clone(),
            status: SourceStatus::Loaded,
            reason: None,
        }
    }
}

pub struct WebRunTool;

#[async_trait]
impl ToolSpec for WebRunTool {
    fn name(&self) -> &'static str {
        "web.run"
    }

    fn description(&self) -> &'static str {
        "Browse the web (search/open/click/find/screenshot/image_query) and return structured results with ref_ids for citations."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "search_query": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "q": { "type": "string" },
                            "recency": { "type": "integer", "minimum": 1, "maximum": 3650 },
                            "max_results": { "type": "integer" },
                            "timeout_ms": { "type": "integer" },
                            "domains": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["q"]
                    }
                },
                "image_query": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "q": { "type": "string" },
                            "recency": { "type": "integer" },
                            "max_results": { "type": "integer" },
                            "timeout_ms": { "type": "integer" },
                            "domains": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["q"]
                    }
                },
                "open": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "ref_id": { "type": "string" },
                            "lineno": { "type": "integer" }
                        },
                        "required": ["ref_id"]
                    }
                },
                "click": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "ref_id": { "type": "string" },
                            "id": { "type": "integer" }
                        },
                        "required": ["ref_id", "id"]
                    }
                },
                "find": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "ref_id": { "type": "string" },
                            "pattern": { "type": "string" }
                        },
                        "required": ["ref_id", "pattern"]
                    }
                },
                "screenshot": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "ref_id": { "type": "string" },
                            "pageno": { "type": "integer" }
                        },
                        "required": ["ref_id", "pageno"]
                    }
                },
                "response_length": {
                    "type": "string",
                    "enum": ["short", "medium", "long"],
                    "description": "Controls result verbosity"
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

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let response_length = ResponseLength::from_input(input.get("response_length"));
        let mut output = WebRunOutput::default();
        let scope = scoped_ref_prefix(&context.state_namespace);
        let turn = with_state(|state| state.next_turn(&context.state_namespace));

        let mut search_counter = 0usize;
        let mut view_counter = 0usize;
        let mut click_counter = 0usize;

        if let Some(searches) = input.get("search_query").and_then(|v| v.as_array()) {
            let mut results = Vec::new();
            for search in searches {
                let query = required_str(search, "q")?.trim().to_string();
                if query.is_empty() {
                    continue;
                }
                let recency = optional_u64(search, "recency", 0)?;
                let max_results = usize::try_from(optional_u64(
                    search,
                    "max_results",
                    response_length.max_results() as u64,
                )?)
                .unwrap_or(response_length.max_results())
                .clamp(1, usize::from(MAX_SEARCH_RESULTS));
                let timeout_ms = optional_u64(search, "timeout_ms", DEFAULT_SEARCH_TIMEOUT_MS)?
                    .min(MAX_SEARCH_TIMEOUT_MS);

                let domains = search
                    .get("domains")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let requested_recency = if recency == 0 {
                    None
                } else {
                    let days = u16::try_from(recency)
                        .ok()
                        .filter(|days| *days <= 3650)
                        .ok_or_else(|| {
                            ToolError::invalid_input(
                                "Field 'search_query[].recency' must be between 1 and 3650 days",
                            )
                        })?;
                    Some(Recency::Days(days))
                };
                let mut response = execute_search(
                    SearchQuery::new(query, max_results, requested_recency, domains, None),
                    timeout_ms,
                    context,
                )
                .await?;
                let refusing_hosts =
                    with_state(|state| state.refusing_hosts(&context.state_namespace));
                prefer_loading_sources(&mut response.results, &refusing_hosts);
                let warning = response.receipt.warning();
                search_counter += 1;
                let ref_id = format!("{scope}turn{turn}search{search_counter}");

                let page = page_from_search(&response.query, &response.results);
                store_page(&context.state_namespace, &ref_id, page);

                results.push(WebRunSearchResult {
                    ref_id,
                    query: response.query,
                    source: response.source,
                    count: response.count,
                    results: response.results,
                    warning,
                    receipt: response.receipt,
                });
            }
            if !results.is_empty() {
                output.search_query = Some(results);
            }
        }

        if let Some(images) = input.get("image_query").and_then(|v| v.as_array()) {
            let mut results = Vec::new();
            for image in images {
                let query = required_str(image, "q")?.trim().to_string();
                if query.is_empty() {
                    continue;
                }
                let recency = optional_u64(image, "recency", 0)?;
                let max_results = usize::try_from(optional_u64(
                    image,
                    "max_results",
                    response_length.max_results() as u64,
                )?)
                .unwrap_or(response_length.max_results())
                .clamp(1, usize::from(MAX_SEARCH_RESULTS));
                let timeout_ms = optional_u64(image, "timeout_ms", DEFAULT_SEARCH_TIMEOUT_MS)?
                    .min(MAX_SEARCH_TIMEOUT_MS);

                let domains = image
                    .get("domains")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let (mut entries, warning) =
                    run_image_search(&query, max_results, timeout_ms, &domains, context).await?;
                entries.retain_mut(|entry| {
                    let canonical_url = entry.url.as_deref().unwrap_or(&entry.image);
                    let Some(citation) = super::web::citations::register(
                        &context.state_namespace,
                        canonical_url,
                        entry.title.as_deref(),
                    ) else {
                        return false;
                    };
                    entry.ref_id = citation.ref_id;
                    true
                });

                let mut warnings = Vec::new();
                if recency > 0 {
                    warnings.push(format!(
                        "Recency filter not enforced (requested last {recency} days)"
                    ));
                }
                if let Some(w) = warning {
                    warnings.push(w);
                }

                results.push(ImageQueryResult {
                    query,
                    source: "duckduckgo_images".to_string(),
                    count: entries.len(),
                    results: entries,
                    warning: if warnings.is_empty() {
                        None
                    } else {
                        Some(warnings.join("; "))
                    },
                });
            }
            if !results.is_empty() {
                output.image_query = Some(results);
            }
        }

        if let Some(opens) = input.get("open").and_then(|v| v.as_array()) {
            let mut views = Vec::new();
            for open in opens {
                let ref_id = required_str(open, "ref_id")?.to_string();
                let lineno = optional_u64(open, "lineno", 1)?.max(1) as usize;

                let page =
                    match resolve_or_fetch_page(&ref_id, DEFAULT_OPEN_TIMEOUT_MS, context).await {
                        Ok(page) => page,
                        Err(error) => {
                            let target = open_target_url(&context.state_namespace, &ref_id);
                            output.sources.push(source_failure(
                                &context.state_namespace,
                                target.as_deref().unwrap_or(&ref_id),
                                error,
                            )?);
                            continue;
                        }
                    };
                view_counter += 1;
                let view_ref = format!("{scope}turn{turn}view{view_counter}");
                store_page(&context.state_namespace, &view_ref, (*page).clone());
                output.sources.push(SourceEntry::loaded(&view_ref, &page));

                let view = render_view(&view_ref, &page, lineno, response_length);
                views.push(view);
            }
            if !views.is_empty() {
                output.open = Some(views);
            }
        }

        if let Some(clicks) = input.get("click").and_then(|v| v.as_array()) {
            let mut views = Vec::new();
            for click in clicks {
                let ref_id = required_str(click, "ref_id")?.to_string();
                let link_id = optional_u64(click, "id", 0)? as usize;
                if link_id == 0 {
                    return Err(ToolError::invalid_input("click.id must be >= 1"));
                }
                let page = get_page(&context.state_namespace, &ref_id).ok_or_else(|| {
                    ToolError::invalid_input(format!("Unknown ref_id '{ref_id}'"))
                })?;
                let link = page.links.iter().find(|l| l.id == link_id).ok_or_else(|| {
                    ToolError::invalid_input(format!(
                        "Link id {link_id} not found for ref_id '{ref_id}'"
                    ))
                })?;
                let target = link.url.clone();
                let fetched =
                    match resolve_or_fetch_page(&target, DEFAULT_OPEN_TIMEOUT_MS, context).await {
                        Ok(page) => page,
                        Err(error) => {
                            output.sources.push(source_failure(
                                &context.state_namespace,
                                &target,
                                error,
                            )?);
                            continue;
                        }
                    };
                click_counter += 1;
                let click_ref = format!("{scope}turn{turn}click{click_counter}");
                store_page(&context.state_namespace, &click_ref, (*fetched).clone());
                output
                    .sources
                    .push(SourceEntry::loaded(&click_ref, &fetched));
                let view = render_view(&click_ref, &fetched, 1, response_length);
                views.push(view);
            }
            if !views.is_empty() {
                output.click = Some(views);
            }
        }

        if let Some(find_requests) = input.get("find").and_then(|v| v.as_array()) {
            let mut finds = Vec::new();
            for find_req in find_requests {
                let ref_id = required_str(find_req, "ref_id")?.to_string();
                let pattern = required_str(find_req, "pattern")?.to_string();
                let page = get_page(&context.state_namespace, &ref_id).ok_or_else(|| {
                    ToolError::invalid_input(format!("Unknown ref_id '{ref_id}'"))
                })?;
                let find_result = find_in_page(&ref_id, &pattern, &page, response_length);
                finds.push(find_result);
            }
            if !finds.is_empty() {
                output.find = Some(finds);
            }
        }

        if let Some(shots) = input.get("screenshot").and_then(|v| v.as_array()) {
            let mut screenshots = Vec::new();
            for shot in shots {
                let ref_id = required_str(shot, "ref_id")?.to_string();
                let pageno = optional_u64(shot, "pageno", 0)? as usize;
                let page = get_page(&context.state_namespace, &ref_id).ok_or_else(|| {
                    ToolError::invalid_input(format!("Unknown ref_id '{ref_id}'"))
                })?;
                let screenshot = screenshot_page(&ref_id, pageno, &page)?;
                screenshots.push(screenshot);
            }
            if !screenshots.is_empty() {
                output.screenshot = Some(screenshots);
            }
        }

        let failed = output
            .sources
            .iter()
            .filter(|source| source.status != SourceStatus::Loaded)
            .count();
        if failed > 0 {
            output
                .sources
                .sort_by_key(|source| source.status != SourceStatus::Loaded);
            output.warnings.push(format!(
                "{failed} source(s) did not load. Cite only sources marked loaded; open another \
                 search result instead of retrying an unavailable URL."
            ));
        }

        if output.performed_no_op() && output.sources.is_empty() {
            // #5123-class: an empty success here reads as "nothing found"
            // rather than "you called the tool wrong" (e.g. the natural
            // {"query": …} shape, which matches no op key).
            let received: Vec<&str> = input
                .as_object()
                .map(|object| object.keys().map(String::as_str).collect())
                .unwrap_or_default();
            return Err(ToolError::invalid_input(format!(
                "web.run performed no operation. Pass at least one non-empty op array \
                 ({}). Received keys: [{}].",
                WEB_RUN_OP_KEYS.join(", "),
                received.join(", ")
            )));
        }

        let mut result = bounded_web_run_result(&output, context)?;
        if failed > 0 {
            // A site refusing a page is an outcome of the browse, not a tool
            // failure: the call still succeeds, and clients render these
            // neutrally instead of as errors.
            let metadata = result.metadata.get_or_insert_with(|| json!({}));
            metadata["source_failures"] = json!(failed);
            metadata["source_failure_severity"] = json!("neutral");
        }
        Ok(result)
    }
}

/// Where an `open` ref would be fetched from, for the failure receipt.
fn open_target_url(namespace: &str, ref_id: &str) -> Option<String> {
    if let Some(citation) = super::web::citations::resolve(namespace, ref_id) {
        return Some(citation.url);
    }
    looks_like_url(ref_id).then(|| ref_id.to_string())
}

/// Turn a failed page fetch into a source receipt, or pass the error through
/// when it is the caller's mistake or a gate (bad ref, denied host, cancel).
fn source_failure(namespace: &str, url: &str, error: ToolError) -> Result<SourceEntry, ToolError> {
    let (status, reason) = match &error {
        ToolError::Timeout { .. } => (SourceStatus::Transient, error.to_string()),
        ToolError::ExecutionFailed { message, .. } => {
            let status = match http_status_of(message) {
                Some(code) if code == 429 || (500..600).contains(&code) => SourceStatus::Transient,
                Some(_) => SourceStatus::Unavailable,
                None if message.contains("timed out") || message.contains("after one retry") => {
                    SourceStatus::Transient
                }
                None => SourceStatus::Unavailable,
            };
            (status, message.clone())
        }
        _ => return Err(error),
    };
    if let Some(code) = http_status_of(&reason)
        && matches!(code, 401 | 403)
        && let Some(host) = reqwest::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
    {
        with_state(|state| state.note_refusing_host(namespace, &host));
    }
    Ok(SourceEntry {
        ref_id: None,
        url: url.to_string(),
        title: None,
        status,
        reason: Some(reason),
    })
}

/// The HTTP status carried by a `document_from_fetched` rejection.
fn http_status_of(message: &str) -> Option<u16> {
    let (_, tail) = message.rsplit_once(" failed: HTTP ")?;
    tail.get(..3)?.parse().ok()
}

/// Rank results from hosts that already refused this session after the ones
/// likely to load, keeping each group's order and renumbering `rank`.
fn prefer_loading_sources(
    results: &mut [NormalizedSearchResult],
    refusing_hosts: &HashSet<String>,
) {
    if refusing_hosts.is_empty() {
        return;
    }
    results.sort_by_key(|result| refusing_hosts.contains(&result.domain));
    for (index, result) in results.iter_mut().enumerate() {
        result.rank = u8::try_from(index + 1).unwrap_or(u8::MAX);
    }
}

const WEB_RUN_OP_KEYS: [&str; 6] = [
    "search_query",
    "image_query",
    "open",
    "click",
    "find",
    "screenshot",
];

impl WebRunOutput {
    fn performed_no_op(&self) -> bool {
        self.search_query.is_none()
            && self.image_query.is_none()
            && self.open.is_none()
            && self.click.is_none()
            && self.find.is_none()
            && self.screenshot.is_none()
    }
}

fn with_state<T>(f: impl FnOnce(&mut WebRunState) -> T) -> T {
    let cache = WEB_RUN_STATE.get_or_init(WebRunCache::default);
    let sessions = cache.sessions.write();
    let pages = cache.pages.write();
    let mut guard = WebRunStateWriteBack::new(sessions, pages);
    guard.state_mut().cleanup();
    let result = f(guard.state_mut());
    guard.write_back();
    result
}

struct WebRunStateWriteBack<'a> {
    sessions: RwLockWriteGuard<'a, HashMap<String, WebRunSessionState>>,
    pages: RwLockWriteGuard<'a, HashMap<String, StoredWebPage>>,
    state: Option<WebRunState>,
}

impl<'a> WebRunStateWriteBack<'a> {
    fn new(
        mut sessions: RwLockWriteGuard<'a, HashMap<String, WebRunSessionState>>,
        mut pages: RwLockWriteGuard<'a, HashMap<String, StoredWebPage>>,
    ) -> Self {
        let state = WebRunState {
            sessions: std::mem::take(&mut *sessions),
            pages: std::mem::take(&mut *pages),
        };
        Self {
            sessions,
            pages,
            state: Some(state),
        }
    }

    fn state_mut(&mut self) -> &mut WebRunState {
        self.state
            .as_mut()
            .expect("web run state should be present until write-back")
    }

    fn write_back(mut self) {
        self.restore();
    }

    fn restore(&mut self) {
        if let Some(state) = self.state.take() {
            *self.sessions = state.sessions;
            *self.pages = state.pages;
        }
    }
}

impl Drop for WebRunStateWriteBack<'_> {
    fn drop(&mut self) {
        self.restore();
    }
}

fn scoped_ref_prefix(namespace: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    format!("s{:016x}_", hasher.finish())
}

fn store_page(namespace: &str, ref_id: &str, page: WebPage) {
    let _ = super::web::citations::register_with_ref(
        namespace,
        ref_id,
        &page.url,
        page.title.as_deref(),
    );
    with_state(|state| {
        state.store_page(namespace, ref_id, page);
    });
}

fn get_page(namespace: &str, ref_id: &str) -> Option<Arc<WebPage>> {
    let cache = WEB_RUN_STATE.get_or_init(WebRunCache::default);
    let stored = {
        let pages = cache.pages.read();
        pages.get(ref_id).cloned()
    }?;
    if stored.namespace != namespace {
        return None;
    }
    {
        let mut sessions = cache.sessions.write();
        if let Some(session) = sessions.get_mut(namespace) {
            session.last_access = Instant::now();
        }
    }
    Some(stored.page)
}

#[cfg(test)]
fn reset_web_run_state() {
    with_state(|state| {
        *state = WebRunState::default();
    });
}

#[cfg(test)]
fn next_turn_for_namespace(namespace: &str) -> u64 {
    with_state(|state| state.next_turn(namespace))
}

async fn resolve_or_fetch_page(
    ref_id: &str,
    timeout_ms: u64,
    context: &ToolContext,
) -> Result<Arc<WebPage>, ToolError> {
    if let Some(page) = get_page(&context.state_namespace, ref_id) {
        return Ok(page);
    }
    if let Some(citation) = super::web::citations::resolve(&context.state_namespace, ref_id) {
        return fetch_page(&citation.url, timeout_ms, context)
            .await
            .map(Arc::new);
    }
    if looks_like_url(ref_id) {
        return fetch_page(ref_id, timeout_ms, context).await.map(Arc::new);
    }
    Err(ToolError::invalid_input(format!(
        "Unknown ref_id '{ref_id}'"
    )))
}

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DuckDuckGoImageResponse {
    #[serde(default)]
    results: Vec<DuckDuckGoImageResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DuckDuckGoImageResult {
    image: String,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

fn extract_duckduckgo_vqd(html: &str) -> Option<String> {
    let html = html.trim();
    if html.is_empty() {
        return None;
    }

    for (prefix, suffix) in [("vqd='", "'"), ("vqd=\"", "\"")] {
        if let Some(start) = html.find(prefix) {
            let rest = &html[start + prefix.len()..];
            if let Some(end) = rest.find(suffix) {
                let token = rest[..end].trim();
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }

    // Fallback: look for `vqd=` and accept a conservative token charset.
    if let Some(start) = html.find("vqd=") {
        let rest = &html[start + 4..];
        let mut token = String::new();
        for ch in rest.chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                token.push(ch);
            } else {
                break;
            }
        }
        if !token.is_empty() {
            return Some(token);
        }
    }

    None
}

async fn run_image_search(
    query: &str,
    max_results: usize,
    timeout_ms: u64,
    domains: &[String],
    context: &ToolContext,
) -> Result<(Vec<ImageResultEntry>, Option<String>), ToolError> {
    super::web_search::check_policy(context.network_policy.as_ref(), "duckduckgo.com")?;
    let client = codewhale_release::tls::reqwest_client_builder()
        .timeout(Duration::from_millis(timeout_ms))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| ToolError::execution_failed(format!("Failed to build HTTP client: {e}")))?;

    // Step 1: fetch the HTML page to obtain the `vqd` token used by the images API.
    let encoded = url_encode(query);
    let seed_url = format!("https://duckduckgo.com/?q={encoded}&iax=images&ia=images");
    let seed_resp = client
        .get(&seed_url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.5")
        .send()
        .await
        .map_err(|e| {
            ToolError::execution_failed(format!("Image search seed request failed: {e}"))
        })?;

    let seed_status = seed_resp.status();
    let seed_body = if super::web::adapter::search_selected(context) {
        super::web::adapter::read_response_with_limit(seed_resp, context, HARD_MAX_BYTES)
            .await
            .map_err(ToolError::from)?
    } else {
        let bytes = codewhale_util::utils::read_response_body_capped(seed_resp, HARD_MAX_BYTES)
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to read image seed response: {e}"))
            })?;
        String::from_utf8_lossy(&bytes).into_owned()
    };

    if !seed_status.is_success() {
        return Err(ToolError::execution_failed(format!(
            "Image search seed request failed: HTTP {}",
            seed_status.as_u16()
        )));
    }

    let vqd = extract_duckduckgo_vqd(&seed_body).ok_or_else(|| {
        ToolError::execution_failed("Failed to extract DuckDuckGo image token (vqd)")
    })?;

    // Step 2: query the DuckDuckGo images JSON endpoint.
    let api_url = format!("https://duckduckgo.com/i.js?l=us-en&o=json&q={encoded}&vqd={vqd}&p=1");
    super::web_search::check_policy(context.network_policy.as_ref(), "duckduckgo.com")?;
    let api_resp = client
        .get(&api_url)
        .header("Accept", "application/json")
        .header("Referer", "https://duckduckgo.com/")
        .send()
        .await
        .map_err(|e| ToolError::execution_failed(format!("Image search request failed: {e}")))?;

    let api_status = api_resp.status();
    let api_body = if super::web::adapter::search_selected(context) {
        super::web::adapter::read_response(api_resp, context)
            .await
            .map_err(ToolError::from)?
    } else {
        let bytes = codewhale_util::utils::read_response_body_capped(api_resp, HARD_MAX_BYTES)
            .await
            .map_err(|e| {
                ToolError::execution_failed(format!("Failed to read image response: {e}"))
            })?;
        String::from_utf8_lossy(&bytes).into_owned()
    };

    if !api_status.is_success() {
        return Err(ToolError::execution_failed(format!(
            "Image search failed: HTTP {}",
            api_status.as_u16()
        )));
    }

    let mut parsed: DuckDuckGoImageResponse = serde_json::from_str(&api_body).map_err(|e| {
        ToolError::execution_failed(format!("Failed to parse image search JSON: {e}"))
    })?;

    if super::web::adapter::search_selected(context) {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Proposal {
            kind: String,
            entries: Vec<DuckDuckGoImageResult>,
            max_results: usize,
        }
        let mut urls = super::web_search::OpaqueUrls::default();
        let mut captured = parsed.clone();
        for entry in &mut captured.results {
            entry.image = urls.capture(&entry.image);
            if let Some(value) = entry.thumbnail.as_mut() {
                *value = urls.capture(value);
            }
            if let Some(value) = entry.url.as_mut() {
                *value = urls.capture(value);
            }
        }
        let proposal: Proposal = super::web::adapter::transform(
            crate::extension_host::StockOperation::WebImages,
            json!({"max_results":max_results,"parsed":captured}),
            context,
            Duration::from_millis(timeout_ms),
        )
        .await
        .map_err(ToolError::from)?;
        if proposal.kind != "web_images"
            || proposal.max_results != max_results
            || proposal.entries.len() > parsed.results.len()
        {
            return Err(ToolError::execution_failed(
                "Web Host returned inconsistent image candidates; no fallback was attempted",
            ));
        }
        parsed.results = proposal.entries;
        for entry in &mut parsed.results {
            entry.image = urls.restore(&entry.image).map_err(ToolError::from)?;
            if let Some(value) = entry.thumbnail.as_mut() {
                *value = urls.restore(value).map_err(ToolError::from)?;
            }
            if let Some(value) = entry.url.as_mut() {
                *value = urls.restore(value).map_err(ToolError::from)?;
            }
        }
    }

    let mut results = parsed
        .results
        .into_iter()
        .filter(|item| !item.image.trim().is_empty())
        .map(|item| ImageResultEntry {
            ref_id: String::new(),
            image: item.image,
            thumbnail: item.thumbnail,
            title: item.title,
            url: item.url,
            source: item.source,
            width: item.width,
            height: item.height,
        })
        .collect::<Vec<_>>();

    // Domain filter is applied to the source page URL when available.
    let warning = if !domains.is_empty() {
        let before = results.len();
        results.retain(|entry| match entry.url.as_deref() {
            Some(url) => domain_matches(url, domains),
            None => true,
        });
        if before != results.len() {
            Some("Filtered image results by domain list".to_string())
        } else {
            None
        }
    } else {
        None
    };

    results.truncate(max_results);
    Ok((results, warning))
}

fn page_from_search(query: &str, results: &[NormalizedSearchResult]) -> WebPage {
    let mut lines = Vec::new();
    let mut links = Vec::new();

    lines.push(format!("Search results for: {query}"));
    for (idx, entry) in results.iter().enumerate() {
        let id = idx + 1;
        links.push(WebLink {
            id,
            url: entry.url.clone(),
            text: entry.title.clone(),
        });
        lines.push(format!("{}. [{}] {}", id, id, entry.title));
        if let Some(snippet) = entry.snippet.as_ref()
            && !snippet.trim().is_empty()
        {
            lines.push(format!("    {snippet}"));
        }
        lines.push(format!("    {url}", url = entry.url));
    }

    WebPage {
        url: "https://html.duckduckgo.com/html/".to_string(),
        title: Some("Search Results".to_string()),
        content_type: Some("text/html".to_string()),
        lines,
        links,
        pdf_pages: None,
        truncated: false,
        partial: None,
    }
}

fn open_fetch_options(timeout_ms: u64) -> FetchOptions {
    FetchOptions::new(
        Duration::from_millis(timeout_ms),
        HARD_MAX_BYTES,
        "text/html,text/markdown,text/plain,application/xhtml+xml,application/pdf,image/*,audio/*,video/*,*/*;q=0.5",
    )
}

/// A 401/403 earns one browser-agent retry inside [`fetch_readable`].
async fn fetch_page(
    url: &str,
    timeout_ms: u64,
    context: &ToolContext,
) -> Result<WebPage, ToolError> {
    fetch_page_with(url, &open_fetch_options(timeout_ms), context).await
}

async fn fetch_page_with(
    url: &str,
    options: &FetchOptions,
    context: &ToolContext,
) -> Result<WebPage, ToolError> {
    let readable = fetch_readable(
        url,
        options,
        context,
        "web_run",
        |payload: super::web::fetch::FetchedPayload| {
            Box::pin(async move { document_from_fetched(&payload, context).await })
        },
    )
    .await?;
    page_from_readable(readable.payload, readable.document, context)
}

#[cfg(test)]
async fn fetch_page_with_initial_pin(
    url: &str,
    timeout_ms: u64,
    context: &ToolContext,
    initial_pin: Option<DnsPin>,
) -> Result<WebPage, ToolError> {
    let readable = fetch_readable_with_initial_pin(
        url,
        &open_fetch_options(timeout_ms),
        context,
        "web_run",
        initial_pin.flatten(),
        |payload: super::web::fetch::FetchedPayload| {
            Box::pin(async move { document_from_fetched(&payload, context).await })
        },
    )
    .await?;
    page_from_readable(readable.payload, readable.document, context)
}

/// Reject non-2xx responses, then extract one readable document.
///
/// Kept separate from rendering so `fetch_readable` can re-run exactly this
/// step against a cache-busted second response (#5904).
async fn document_from_fetched(
    payload: &super::web::fetch::FetchedPayload,
    context: &ToolContext,
) -> super::web::adapter::AdapterResult<ExtractedDocument> {
    if !(200..300).contains(&payload.status) {
        return Err((ToolError::execution_failed(format!(
            "Web request to {} failed: HTTP {}",
            payload.url, payload.status
        )))
        .into());
    }
    extract_document(
        &payload.url,
        Some(&payload.content_type),
        &payload.bytes,
        payload.truncated,
        Some(context),
    )
    .await
}

fn page_from_readable(
    payload: super::web::fetch::FetchedPayload,
    document: Readable<ExtractedDocument>,
    context: &ToolContext,
) -> Result<WebPage, ToolError> {
    match document {
        Readable::Document(document) => page_from_document(payload, document, context),
        Readable::Partial(page) => {
            page_from_document(payload, page.document, context).map(|web_page| WebPage {
                partial: Some(page.note),
                ..web_page
            })
        }
    }
}

fn page_from_document(
    payload: super::web::fetch::FetchedPayload,
    document: ExtractedDocument,
    context: &ToolContext,
) -> Result<WebPage, ToolError> {
    let content_type = Some(payload.content_type);
    match document.kind {
        DocumentKind::Html => {
            let html = document.cleaned_html.ok_or_else(|| {
                ToolError::execution_failed("Readable HTML extraction returned no document")
            })?;
            let (lines, links, parsed_title) = parse_html(&html, &payload.url);
            Ok(WebPage {
                url: payload.url,
                title: document.title.or(parsed_title),
                content_type,
                lines,
                links,
                pdf_pages: None,
                truncated: payload.truncated,
                partial: None,
            })
        }
        DocumentKind::Markdown => {
            let (lines, links) = parse_markdown(&document.markdown, &payload.url);
            Ok(WebPage {
                url: payload.url,
                title: document.title,
                content_type,
                lines,
                links,
                pdf_pages: None,
                truncated: payload.truncated,
                partial: None,
            })
        }
        DocumentKind::Text => Ok(WebPage {
            url: payload.url,
            title: document.title,
            content_type,
            lines: readable_lines(&document.text),
            links: Vec::new(),
            pdf_pages: None,
            truncated: payload.truncated,
            partial: None,
        }),
        DocumentKind::Pdf => {
            let pages = document.pdf_pages.unwrap_or_default();
            Ok(WebPage {
                url: payload.url,
                title: document.title,
                content_type,
                lines: pages.first().cloned().unwrap_or_default(),
                links: Vec::new(),
                pdf_pages: Some(pages),
                truncated: payload.truncated,
                partial: None,
            })
        }
        DocumentKind::Media => {
            let extension = document.media_extension.unwrap_or("bin");
            let digest = codewhale_runtime::hashing::sha256_hex(&*payload.bytes);
            let artifact_id = format!("web_media_{}", &digest[..16]);
            let (_absolute, relative) = codewhale_runtime::artifacts::write_session_artifact_bytes(
                &context.state_namespace,
                &artifact_id,
                extension,
                &payload.bytes,
            )
            .map_err(|error| {
                ToolError::execution_failed(format!(
                    "failed to preserve fetched media artifact: {error}"
                ))
            })?;
            let path = codewhale_runtime::artifacts::format_artifact_relative_path(&relative);
            Ok(WebPage {
                url: payload.url,
                title: Some("Media artifact".to_string()),
                content_type,
                lines: vec![format!("Fetched media saved to {path}")],
                links: Vec::new(),
                pdf_pages: None,
                truncated: payload.truncated,
                partial: None,
            })
        }
    }
}

fn bounded_web_run_result(
    output: &WebRunOutput,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let full = serde_json::to_string_pretty(output)
        .map_err(|error| ToolError::execution_failed(error.to_string()))?;
    let bounded = bound_web_text(
        full,
        context,
        |body| {
            let digest = codewhale_runtime::hashing::sha256_hex(body.as_bytes());
            format!("web_run_{}", &digest[..16])
        },
        "web.run result",
    )?;
    let metadata = bounded.artifact.map(|artifact| {
        json!({
            "spillover_path": artifact.absolute_path.display().to_string(),
            "artifact_session_id": artifact.session_id,
            "artifact_relative_path": codewhale_runtime::artifacts::format_artifact_relative_path(&artifact.relative_path),
            "artifact_byte_size": artifact.byte_size,
            "artifact_preview": artifact.preview,
            // The overflow footer points at `retrieve_tool_result`; flag the
            // evidence so the engine auto-activates that tool next turn (same
            // contract as the shell-truncation spillover).
            "evidence_available": true,
        })
    });

    Ok(ToolResult {
        content: bounded.content,
        success: true,
        metadata,
    })
}

fn render_view(
    ref_id: &str,
    page: &WebPage,
    lineno: usize,
    response: ResponseLength,
) -> PageViewResult {
    let total = page.lines.len();
    let view_lines = response.view_lines();
    let start = if total == 0 {
        1
    } else if lineno > total {
        total.saturating_sub(view_lines.saturating_sub(1)).max(1)
    } else {
        lineno
    };
    let end = if total == 0 {
        0
    } else {
        (start + view_lines - 1).min(total)
    };

    let content = if total == 0 {
        "(no content)".to_string()
    } else {
        render_lines(&page.lines, start, end)
    };

    PageViewResult {
        ref_id: ref_id.to_string(),
        url: page.url.clone(),
        title: page.title.clone(),
        content_type: page.content_type.clone(),
        line_start: start,
        line_end: end,
        total_lines: total,
        truncated: page.truncated,
        partial: page.partial.is_some(),
        note: page.partial.clone(),
        content,
        links: page.links.clone(),
    }
}

fn render_lines(lines: &[String], start: usize, end: usize) -> String {
    lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_no = idx + 1;
            if line_no < start || line_no > end {
                return None;
            }
            Some(format!("{line_no:>4} {line}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_in_page(
    ref_id: &str,
    pattern: &str,
    page: &WebPage,
    response: ResponseLength,
) -> FindResult {
    let needle = pattern.to_lowercase();
    let mut matches = Vec::new();
    for (idx, line) in page.lines.iter().enumerate() {
        if line.to_lowercase().contains(&needle) {
            matches.push(FindMatch {
                line: idx + 1,
                text: line.clone(),
            });
        }
        if matches.len() >= response.max_find_matches() {
            break;
        }
    }

    FindResult {
        ref_id: ref_id.to_string(),
        pattern: pattern.to_string(),
        count: matches.len(),
        matches,
    }
}

fn screenshot_page(
    ref_id: &str,
    pageno: usize,
    page: &WebPage,
) -> Result<ScreenshotResult, ToolError> {
    let pages = page
        .pdf_pages
        .as_ref()
        .ok_or_else(|| ToolError::invalid_input("screenshot is only supported for PDF pages"))?;
    if pages.is_empty() {
        return Err(ToolError::execution_failed("PDF has no pages"));
    }
    if pageno >= pages.len() {
        return Err(ToolError::invalid_input(format!(
            "pageno {pageno} out of range (0..{max})",
            max = pages.len().saturating_sub(1)
        )));
    }
    let content = pages[pageno].join("\n");
    Ok(ScreenshotResult {
        ref_id: ref_id.to_string(),
        pageno,
        total_pages: pages.len(),
        content,
    })
}

// === HTML Parsing ===

static ANCHOR_RE: OnceLock<Regex> = OnceLock::new();
static TAG_RE: OnceLock<Regex> = OnceLock::new();
static BLOCK_RE: OnceLock<Regex> = OnceLock::new();
static SCRIPT_RE: OnceLock<Regex> = OnceLock::new();
static STYLE_RE: OnceLock<Regex> = OnceLock::new();
static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static MARKDOWN_LINK_RE: OnceLock<Regex> = OnceLock::new();

fn get_anchor_re() -> &'static Regex {
    ANCHOR_RE.get_or_init(|| {
        Regex::new(r#"(?is)<a\s+[^>]*href\s*=\s*['\"]([^'\"]+)['\"][^>]*>(.*?)</a>"#)
            .expect("anchor regex")
    })
}

fn get_tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag regex"))
}

fn get_block_re() -> &'static Regex {
    BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?is)</?(p|div|li|ul|ol|br|h[1-6]|tr|td|th|table|section|article)[^>]*>")
            .expect("block regex")
    })
}

fn get_script_re() -> &'static Regex {
    SCRIPT_RE.get_or_init(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap())
}

fn get_style_re() -> &'static Regex {
    STYLE_RE.get_or_init(|| Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap())
}

fn get_title_re() -> &'static Regex {
    TITLE_RE.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap())
}

fn parse_html(html: &str, base_url: &str) -> (Vec<String>, Vec<WebLink>, Option<String>) {
    let title = extract_title(html);
    let without_scripts = get_script_re().replace_all(html, "").to_string();
    let without_styles = get_style_re().replace_all(&without_scripts, "").to_string();

    let (with_links, links) = replace_links(&without_styles, base_url);
    let with_breaks = get_block_re().replace_all(&with_links, "\n").to_string();
    let without_tags = get_tag_re().replace_all(&with_breaks, "").to_string();
    let decoded = decode_html_entities(&without_tags);

    let mut lines = Vec::new();
    for line in decoded.lines() {
        let trimmed = normalize_whitespace(line);
        if trimmed.is_empty() {
            continue;
        }
        for wrapped in wrap_line(&trimmed, ResponseLength::Medium.wrap_width()) {
            lines.push(wrapped);
        }
    }

    (lines, links, title)
}

fn parse_markdown(markdown: &str, base_url: &str) -> (Vec<String>, Vec<WebLink>) {
    let re = MARKDOWN_LINK_RE.get_or_init(|| {
        Regex::new(r#"\[([^\]]+)\]\(([^\s)]+)(?:\s+"[^"]*")?\)"#).expect("markdown link regex")
    });
    let mut links = Vec::new();
    let mut replaced = String::with_capacity(markdown.len());
    let mut last = 0;
    for capture in re.captures_iter(markdown) {
        let Some(full) = capture.get(0) else { continue };
        let Some(text) = capture.get(1) else { continue };
        let Some(target) = capture.get(2) else {
            continue;
        };
        replaced.push_str(&markdown[last..full.start()]);
        let id = links.len() + 1;
        let text = normalize_whitespace(text.as_str());
        let url = resolve_url(base_url, target.as_str());
        links.push(WebLink {
            id,
            url,
            text: text.clone(),
        });
        replaced.push_str(&format!("[{id}] {text}"));
        last = full.end();
    }
    replaced.push_str(&markdown[last..]);
    (readable_lines(&replaced), links)
}

fn readable_lines(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            let line = normalize_whitespace(line);
            wrap_line(&line, ResponseLength::Medium.wrap_width())
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn extract_title(html: &str) -> Option<String> {
    let re = get_title_re();
    let cap = re.captures(html)?;
    let raw = cap.get(1)?.as_str();
    let cleaned = normalize_whitespace(&decode_html_entities(raw));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn replace_links(html: &str, base_url: &str) -> (String, Vec<WebLink>) {
    let re = get_anchor_re();
    let mut links = Vec::new();
    let mut output = String::with_capacity(html.len());
    let mut last = 0;

    for cap in re.captures_iter(html) {
        let Some(full) = cap.get(0) else { continue };
        let Some(href) = cap.get(1) else { continue };
        let Some(text_match) = cap.get(2) else {
            continue;
        };

        output.push_str(&html[last..full.start()]);
        let text = normalize_whitespace(&strip_tags(text_match.as_str()));
        let resolved = resolve_url(base_url, href.as_str());
        if !text.is_empty() {
            let id = links.len() + 1;
            links.push(WebLink {
                id,
                url: resolved.clone(),
                text: text.clone(),
            });
            output.push_str(&format!("[{id}] {text}"));
        } else {
            output.push_str(&resolved);
        }
        last = full.end();
    }

    output.push_str(&html[last..]);
    (output, links)
}

fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if let Ok(base_url) = reqwest::Url::parse(base)
        && let Ok(joined) = base_url.join(href)
    {
        return joined.to_string();
    }
    href.to_string()
}

fn strip_tags(text: &str) -> String {
    get_tag_re().replace_all(text, "").to_string()
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reflow one line to `width` terminal columns.
///
/// `width` is a column budget, so every measurement here is a display width.
/// Measuring `str::len()` instead made the budget script-dependent: Cyrillic
/// and Greek are two bytes per single-column character and CJK three bytes per
/// double-column character, so a Russian page wrapped at half the requested
/// width and a Japanese one at two thirds. That is not only ragged output —
/// `render_view` pages these lines by count, so the extra lines pushed real
/// content past `ResponseLength::view_lines()` and the model saw a fraction of
/// the page an English URL would have returned. Widths equal byte lengths for
/// ASCII, so Latin-script wrapping is unchanged.
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if UnicodeWidthStr::width(text) <= width {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);
        if current.is_empty() {
            current.push_str(word);
            current_width = word_width;
        } else if current_width + word_width < width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_width = word_width;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

fn url_encode(input: &str) -> String {
    codewhale_util::utils::url_encode(input)
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::web::scrape::{parse_bing_results, parse_duckduckgo_results};
    use std::path::PathBuf;
    use tokio::sync::{Mutex, MutexGuard};

    static WEB_RUN_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    struct ArtifactRootRestore(Option<PathBuf>);

    impl Drop for ArtifactRootRestore {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
        }
    }

    fn lock_web_run_test_state() -> MutexGuard<'static, ()> {
        WEB_RUN_TEST_LOCK.blocking_lock()
    }

    fn sample_page(url: &str) -> WebPage {
        WebPage {
            url: url.to_string(),
            title: Some("Example".to_string()),
            content_type: Some("text/html".to_string()),
            lines: vec!["example line".to_string()],
            links: Vec::new(),
            pdf_pages: None,
            truncated: false,
            partial: None,
        }
    }

    fn sample_page_with_link(url: &str, target: &str) -> WebPage {
        let mut page = sample_page(url);
        page.links.push(WebLink {
            id: 1,
            url: target.to_string(),
            text: "target".to_string(),
        });
        page
    }

    #[test]
    fn html_link_parsing_extracts_links() {
        let html = r#"
            <html><body>
            <p>Hello <a href="https://example.com">Example</a> world.</p>
            </body></html>
        "#;
        let (lines, links, title) = parse_html(html, "https://example.com");
        assert!(title.is_none());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com");
        assert!(lines.iter().any(|line| line.contains("Example")));
    }

    #[test]
    fn markdown_link_parsing_preserves_click_targets() {
        let (lines, links) = parse_markdown(
            "## Guide\n\nRead [the proof](/proof) before shipping.",
            "https://example.com/docs/start",
        );

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/proof");
        assert!(lines.iter().any(|line| line.contains("[1] the proof")));
    }

    #[test]
    fn oversized_web_run_output_round_trips_through_session_artifact() {
        let _lock = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prior = codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
            tmp.path().join("sessions"),
        ));
        let _restore = ArtifactRootRestore(prior);
        let context = ToolContext::new(".")
            .with_state_namespace("web-run-overflow")
            .with_route_context_window(10_000);
        let output = WebRunOutput {
            warnings: vec!["large receipt ".repeat(200)],
            ..WebRunOutput::default()
        };

        let result = bounded_web_run_result(&output, &context).unwrap();
        let metadata = result.metadata.expect("artifact metadata");
        let path = metadata["spillover_path"].as_str().unwrap();
        let full = std::fs::read_to_string(path).unwrap();

        assert!(result.content.contains("retrieve_tool_result"));
        assert!(result.content.chars().count() <= inline_char_budget(&context));
        assert_eq!(
            metadata["evidence_available"],
            json!(true),
            "overflow footer points at retrieve_tool_result; the metadata must \
             flag the evidence so the engine auto-activates that tool:\n{metadata}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&full).unwrap()["warnings"][0],
            output.warnings[0]
        );
    }

    #[test]
    fn wrap_line_measures_display_width_not_bytes() {
        // Same shape, two scripts: twelve four-character words. Cyrillic is two
        // bytes per single-column character, so measuring `len()` wrapped the
        // Russian text at half the requested column budget and handed
        // `render_view` twice as many lines to page through.
        let latin = ["abcd"; 12].join(" ");
        let cyrillic = ["абвг"; 12].join(" ");
        assert_eq!(
            wrap_line(&cyrillic, 20).len(),
            wrap_line(&latin, 20).len(),
            "cyrillic: {:?}\nlatin: {:?}",
            wrap_line(&cyrillic, 20),
            wrap_line(&latin, 20)
        );
        for line in wrap_line(&cyrillic, 20) {
            assert!(
                UnicodeWidthStr::width(line.as_str()) <= 20,
                "wrapped past the column budget: {line:?}"
            );
        }
    }

    #[test]
    fn wrap_line_splits_long_lines() {
        let line = "This is a long line that should wrap cleanly at word boundaries";
        let wrapped = wrap_line(line, 20);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|l| l.len() <= 20));
    }

    #[test]
    fn extracts_duckduckgo_vqd_token() {
        let html_single = "<script>var x = {vqd='3-1234567890'};</script>";
        assert_eq!(
            extract_duckduckgo_vqd(html_single),
            Some("3-1234567890".to_string())
        );

        let html_double = "<script>var x = {vqd=\"3-abcdef\"};</script>";
        assert_eq!(
            extract_duckduckgo_vqd(html_double),
            Some("3-abcdef".to_string())
        );

        let html_plain = "https://duckduckgo.com/?q=test&vqd=3-xyz_123&ia=images";
        assert_eq!(
            extract_duckduckgo_vqd(html_plain),
            Some("3-xyz_123".to_string())
        );
    }

    #[tokio::test]
    async fn text_search_uses_configured_shared_backend_and_exposes_receipt() {
        use crate::tools::spec::ToolSpec;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "shared seam"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "title": "Shared result",
                    "url": "https://docs.example.com/shared",
                    "content": "one adapter path"
                }]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::Searxng;
        context.search_base_url = Some(server.uri());
        context.state_namespace = "shared-backend-test".to_string();

        let result = WebRunTool
            .execute(
                json!({
                    "search_query": [{
                        "q": "shared seam",
                        "recency": 7,
                        "domains": ["example.com"]
                    }]
                }),
                &context,
            )
            .await
            .expect("web.run should use configured SearXNG backend");
        let value: Value = serde_json::from_str(&result.content).expect("web.run json");
        let search = &value["search_query"][0];

        assert_eq!(search["source"], "searxng");
        assert_eq!(search["count"], 1);
        assert_eq!(search["results"][0]["rank"], 1);
        assert_eq!(search["results"][0]["domain"], "docs.example.com");
        assert_eq!(search["receipt"]["backend"], "searxng");
        assert_eq!(search["receipt"]["honored"]["domains"], true);
        assert_eq!(
            search["receipt"]["honored"]["recency"], true,
            "SearXNG forwards recency as time_range"
        );
    }

    #[tokio::test]
    async fn search_ref_ids_resolve_to_their_source_urls_for_open() {
        use crate::tools::spec::ToolSpec;
        use codewhale_runtime::config::SearchProvider;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "citation handoff"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "title": "Handoff target",
                    "url": "https://docs.example.com/handoff",
                    "content": "the page a later open command fetches"
                }]
            })))
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut context = ToolContext::new(tmp.path().to_path_buf());
        context.search_provider = SearchProvider::Searxng;
        context.search_base_url = Some(server.uri());
        context.state_namespace = "handoff-citation-test".to_string();

        let result = WebRunTool
            .execute(
                json!({"search_query": [{"q": "citation handoff"}]}),
                &context,
            )
            .await
            .expect("web.run search should succeed");
        let value: Value = serde_json::from_str(&result.content).expect("web.run json");
        let ref_id = value["search_query"][0]["results"][0]["ref_id"]
            .as_str()
            .expect("every search result carries a minted ref_id")
            .to_string();

        // `open` resolves search-result refs through the shared citation
        // registry; the handoff must preserve the exact source URL.
        let citation = crate::tools::web::citations::resolve(&context.state_namespace, &ref_id)
            .expect("search result ref must resolve for a later open");
        assert_eq!(citation.url, "https://docs.example.com/handoff");
        assert_eq!(citation.title.as_deref(), Some("Handoff target"));
        assert!(
            crate::tools::web::citations::resolve("foreign-session", &ref_id).is_none(),
            "citation handles must stay session-scoped"
        );
    }

    #[tokio::test]
    async fn open_failure_reports_url_and_status() {
        let payload = crate::tools::web::fetch::FetchedPayload {
            url: "https://example.com/missing".to_string(),
            status: 404,
            headers: std::collections::BTreeMap::new(),
            content_type: "text/html".to_string(),
            bytes: Arc::new(Vec::new()),
            truncated: false,
            cache_hit: false,
            retries: 0,
            redirects: 0,
        };
        let context = ToolContext::new(PathBuf::from("."));

        let error = document_from_fetched(&payload, &context)
            .await
            .expect_err("non-2xx pages must not be rendered");
        let message = error.to_string();
        assert!(
            message.contains("https://example.com/missing"),
            "transport failures must name the URL: {message}"
        );
        assert!(message.contains("404"), "got `{message}`");
    }

    #[test]
    fn response_length_result_counts_are_anchored_to_the_shared_contract() {
        assert_eq!(ResponseLength::Short.max_results(), DEFAULT_SEARCH_RESULTS);
        assert_eq!(
            ResponseLength::Long.max_results(),
            usize::from(MAX_SEARCH_RESULTS)
        );
        assert!(ResponseLength::Medium.max_results() <= usize::from(MAX_SEARCH_RESULTS));
    }

    #[test]
    fn parses_bing_results_and_decodes_redirect_url() {
        let html = r#"
            <ol>
              <li class="b_algo">
                <h2><a href="https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9wYXRoP3E9MQ">Example &amp; Result</a></h2>
                <div class="b_caption"><p>A <strong>useful</strong> snippet.</p></div>
              </li>
            </ol>
        "#;

        let results = parse_bing_results(html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & Result");
        assert_eq!(results[0].url, "https://example.com/path?q=1");
        assert_eq!(results[0].snippet.as_deref(), Some("A useful snippet."));
    }

    #[test]
    fn web_run_search_path_filters_known_spam_domain() {
        // The shared scraper used by web_run filters the known #964 spam family.
        let html = r#"
            <a class="result__a" href="https://astralia.forumgratuit.org/a">A</a>
            <a class="result__snippet">spam</a>
            <a class="result__a" href="https://russia.forumgratuit.org/b">B</a>
            <a class="result__snippet">spam</a>
            <a class="result__a" href="https://other.forumgratuit.org/c">C</a>
            <a class="result__snippet">spam</a>
            <a class="result__a" href="https://hello.forumgratuit.org/d">D</a>
            <a class="result__snippet">spam</a>
            <a class="result__a" href="https://world.forumgratuit.org/e">E</a>
            <a class="result__snippet">spam</a>
        "#;
        let results = parse_duckduckgo_results(html, 10);
        assert!(
            results.is_empty(),
            "web_run path must drop the known spam family via the shared scraper"
        );
    }

    #[test]
    fn domain_scoped_fixture_preserves_legitimate_same_site_results() {
        let html = r#"
            <a class="result__a" href="https://docs.example.co.uk/a">A</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.co.uk/b">B</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://docs.example.co.uk/c">C</a>
            <a class="result__snippet">s</a>
            <a class="result__a" href="https://other.example/d">D</a>
            <a class="result__snippet">s</a>
        "#;
        let domains = vec!["docs.example.co.uk".to_string()];
        let mut results = parse_duckduckgo_results(html, 10);
        results.retain(|entry| domain_matches(&entry.url, &domains));

        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|entry| entry.url.contains("docs.example.co.uk"))
        );
    }

    #[test]
    fn scoped_ref_prefix_is_session_specific() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let alpha = scoped_ref_prefix("session-alpha");
        let beta = scoped_ref_prefix("session-beta");

        assert_ne!(alpha, beta);
        assert!(alpha.starts_with('s'));
        assert!(alpha.ends_with('_'));
        assert_eq!(alpha.len(), 18);
    }

    #[test]
    fn stored_pages_do_not_cross_scoped_sessions() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let shared_suffix = "turn1search1";
        let ref_alpha = format!("{}{}", scoped_ref_prefix("session-alpha"), shared_suffix);
        let ref_beta = format!("{}{}", scoped_ref_prefix("session-beta"), shared_suffix);

        store_page(
            "session-alpha",
            &ref_alpha,
            sample_page("https://example.com/alpha"),
        );

        assert!(get_page("session-alpha", &ref_alpha).is_some());
        assert!(get_page("session-beta", &ref_alpha).is_none());
        assert!(get_page("session-beta", &ref_beta).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_open_rejects_exact_foreign_session_ref() {
        let _lock = WEB_RUN_TEST_LOCK.lock().await;
        reset_web_run_state();
        let ref_id = format!("{}turn0search1", scoped_ref_prefix("foreign-open-owner"));
        store_page(
            "foreign-open-owner",
            &ref_id,
            sample_page("https://example.com/private-session-page"),
        );
        let context =
            ToolContext::new(PathBuf::from(".")).with_state_namespace("foreign-open-caller");

        let err = WebRunTool
            .execute(json!({"open": [{"ref_id": ref_id}]}), &context)
            .await
            .expect_err("foreign exact ref must not open");

        assert!(format!("{err}").contains("Unknown ref_id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_click_rejects_exact_foreign_session_ref() {
        let _lock = WEB_RUN_TEST_LOCK.lock().await;
        reset_web_run_state();
        let ref_id = format!("{}turn0search1", scoped_ref_prefix("foreign-click-owner"));
        store_page(
            "foreign-click-owner",
            &ref_id,
            sample_page_with_link(
                "https://example.com/private-session-page",
                "https://example.com/target",
            ),
        );
        let context =
            ToolContext::new(PathBuf::from(".")).with_state_namespace("foreign-click-caller");

        let err = WebRunTool
            .execute(json!({"click": [{"ref_id": ref_id, "id": 1}]}), &context)
            .await
            .expect_err("foreign exact ref must not be clickable");

        assert!(format!("{err}").contains("Unknown ref_id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_click_routes_target_through_shared_ssrf_guard() {
        let _lock = WEB_RUN_TEST_LOCK.lock().await;
        reset_web_run_state();
        let namespace = "guarded-click-session";
        let ref_id = format!("{}turn0search1", scoped_ref_prefix(namespace));
        store_page(
            namespace,
            &ref_id,
            sample_page_with_link("https://example.com/source", "http://127.0.0.1/admin"),
        );
        let context = ToolContext::new(PathBuf::from(".")).with_state_namespace(namespace);

        let err = WebRunTool
            .execute(json!({"click": [{"ref_id": ref_id, "id": 1}]}), &context)
            .await
            .expect_err("click target must be SSRF-guarded");

        assert!(format!("{err}").contains("restricted address"));
    }

    #[test]
    fn cached_page_reads_share_page_arc() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let namespace = "session-alpha";
        let ref_id = format!("{}turn0search1", scoped_ref_prefix(namespace));
        store_page(namespace, &ref_id, sample_page("https://example.com/alpha"));

        let first = get_page(namespace, &ref_id).expect("first page read");
        let second = get_page(namespace, &ref_id).expect("second page read");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn turn_counters_are_scoped_per_session() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();

        assert_eq!(next_turn_for_namespace("session-alpha"), 0);
        assert_eq!(next_turn_for_namespace("session-alpha"), 1);
        assert_eq!(next_turn_for_namespace("session-beta"), 0);
    }

    #[test]
    fn with_state_restores_cache_after_panic() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let namespace = "session-alpha";
        let ref_id = format!("{}turn0search1", scoped_ref_prefix(namespace));
        store_page(namespace, &ref_id, sample_page("https://example.com/alpha"));

        let panic_result = std::panic::catch_unwind(|| {
            with_state(|state| {
                let session = state
                    .sessions
                    .get_mut(namespace)
                    .expect("session should exist");
                session.next_turn = 42;
                panic!("exercise web_run write-back guard");
            });
        });

        assert!(panic_result.is_err());
        assert!(get_page(namespace, &ref_id).is_some());
        assert_eq!(next_turn_for_namespace(namespace), 42);
    }

    #[test]
    fn stale_session_pages_are_evicted() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let namespace = "session-alpha";
        let ref_id = format!("{}turn0search1", scoped_ref_prefix(namespace));
        store_page(namespace, &ref_id, sample_page("https://example.com/alpha"));

        // On Windows, Instant's epoch is system boot.  If the CI runner has
        // been up for less than WEB_RUN_SESSION_TTL the subtraction would
        // underflow, so we skip the test in that case.
        let stale = WEB_RUN_SESSION_TTL + Duration::from_secs(1);
        let can_test = with_state(|state| {
            let session = state
                .sessions
                .get_mut(namespace)
                .expect("session should exist");
            match Instant::now().checked_sub(stale) {
                Some(past) => {
                    session.last_access = past;
                    true
                }
                None => false,
            }
        });
        if !can_test {
            // System uptime shorter than session TTL; can't test eviction.
            return;
        }

        let _ = next_turn_for_namespace("session-beta");

        assert!(get_page(namespace, &ref_id).is_none());
    }

    #[test]
    fn direct_urls_remain_compatible_open_refs() {
        assert!(looks_like_url("https://example.com"));
        assert!(looks_like_url("http://example.com"));
        assert!(!looks_like_url("turn0search0"));
    }

    #[tokio::test]
    async fn network_policy_denies_direct_open_url() {
        use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

        let policy = NetworkPolicy {
            default: Decision::Deny.into(),
            allow: vec!["api.deepseek.com".to_string()],
            deny: vec![],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        };
        let decider = NetworkPolicyDecider::new(policy, None);
        let ctx = ToolContext::new(PathBuf::from(".")).with_network_policy(decider);

        let err = fetch_page("https://example.com/private", 5_000, &ctx)
            .await
            .expect_err("blocked host should fail");
        assert!(format!("{err}").contains("blocked by network policy"));
    }

    fn ssrf_ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."))
    }

    #[tokio::test]
    async fn open_refuses_loopback_ip_url() {
        let err = resolve_or_fetch_page("http://127.0.0.1/", 5_000, &ssrf_ctx())
            .await
            .expect_err("loopback open must be refused");
        assert!(
            format!("{err}").contains("restricted address"),
            "expected restricted-address error; got {err}"
        );
    }

    #[tokio::test]
    async fn open_resolves_shared_citation_refs_only_in_their_session() {
        let namespace = "shared-citation-open-session";
        let citation = crate::tools::web::citations::register(
            namespace,
            "http://127.0.0.1/private",
            Some("Private target"),
        )
        .expect("valid HTTP citation metadata");
        let owner_context = ToolContext::new(PathBuf::from(".")).with_state_namespace(namespace);
        let owner_error = resolve_or_fetch_page(&citation.ref_id, 5_000, &owner_context)
            .await
            .expect_err("resolved citation must still use the SSRF guard");
        assert!(format!("{owner_error}").contains("restricted address"));

        let foreign_context = ToolContext::new(PathBuf::from("."))
            .with_state_namespace("shared-citation-foreign-session");
        let foreign_error = resolve_or_fetch_page(&citation.ref_id, 5_000, &foreign_context)
            .await
            .expect_err("foreign session must not resolve citation ref");
        assert!(format!("{foreign_error}").contains("Unknown ref_id"));
    }

    #[tokio::test]
    async fn open_refuses_private_range_ip_url() {
        let err = resolve_or_fetch_page("http://192.168.1.50/admin", 5_000, &ssrf_ctx())
            .await
            .expect_err("private-range open must be refused");
        assert!(
            format!("{err}").contains("restricted address"),
            "expected restricted-address error; got {err}"
        );
    }

    #[tokio::test]
    async fn open_refuses_metadata_endpoint_ip_url() {
        let err = resolve_or_fetch_page(
            "http://169.254.169.254/latest/meta-data",
            5_000,
            &ssrf_ctx(),
        )
        .await
        .expect_err("cloud metadata open must be refused");
        assert!(
            format!("{err}").contains("restricted address"),
            "expected restricted-address error; got {err}"
        );
    }

    #[tokio::test]
    async fn open_refuses_redirect_from_public_host_to_private_ip() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Use a public-looking hostname pinned to the local fixture for the
        // already-validated first hop. The redirect itself still goes through
        // the real shared guard inside fetch_page's redirect loop.
        let server = MockServer::start().await;
        let private_location = "http://10.0.0.5/internal";
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", private_location))
            .mount(&server)
            .await;
        let host = "public-redirect.example.test";
        let initial_url = format!("http://{host}:{}/", server.address().port());
        let pin = Some((host.to_string(), "127.0.0.1".parse().unwrap()));

        let err = fetch_page_with_initial_pin(&initial_url, 5_000, &ssrf_ctx(), Some(pin))
            .await
            .expect_err("guarded redirect to private IP must be refused");
        assert!(
            format!("{err}").contains("restricted address"),
            "redirect loop must surface the shared guard rejection; got {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_fails_fast_when_no_op_key_matches() {
        // #5123-class: the natural {"query": …} shape matched no op key and
        // previously returned an empty SUCCESS ({"warnings":[]}).
        let _lock = WEB_RUN_TEST_LOCK.lock().await;
        reset_web_run_state();
        let tmp = tempfile::tempdir().expect("tempdir");
        let context = ToolContext::new(tmp.path());

        let err = WebRunTool
            .execute(json!({"query": "rust async runtime"}), &context)
            .await
            .expect_err("unmatched op keys must fail, not return empty success");
        let message = format!("{err}");
        assert!(message.contains("performed no operation"), "{message}");
        assert!(message.contains("search_query"), "{message}");
        assert!(message.contains("query"), "{message}");

        let err = WebRunTool
            .execute(json!({}), &context)
            .await
            .expect_err("empty input must fail fast");
        assert!(format!("{err}").contains("performed no operation"), "{err}");
    }

    #[tokio::test]
    async fn open_retries_as_browser_once_after_a_refusal() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::method};

        #[derive(Clone)]
        struct RefuseBots(Arc<AtomicUsize>);
        impl Respond for RefuseBots {
            fn respond(&self, request: &Request) -> ResponseTemplate {
                self.0.fetch_add(1, Ordering::SeqCst);
                let agent = request
                    .headers
                    .get("user-agent")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if agent.contains("codewhale") {
                    ResponseTemplate::new(403)
                } else {
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/plain")
                        .set_body_string("review body")
                }
            }
        }

        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .respond_with(RefuseBots(Arc::clone(&calls)))
            .mount(&server)
            .await;
        let host = "refuses-bots.example.test";
        let url = format!("http://{host}:{}/review", server.address().port());
        let pin = Some((host.to_string(), "127.0.0.1".parse().unwrap()));
        let context = ToolContext::new(PathBuf::from(".")).with_state_namespace("refuse-bots");

        let page = fetch_page_with_initial_pin(&url, 5_000, &context, Some(pin))
            .await
            .expect("browser-agent fallback loads the page");
        assert!(page.lines.iter().any(|line| line.contains("review body")));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "exactly one fallback request"
        );
    }

    #[tokio::test]
    async fn open_does_not_retry_a_missing_page_as_browser() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::method};

        #[derive(Clone)]
        struct Missing(Arc<AtomicUsize>);
        impl Respond for Missing {
            fn respond(&self, _request: &Request) -> ResponseTemplate {
                self.0.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(404)
            }
        }

        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .respond_with(Missing(Arc::clone(&calls)))
            .mount(&server)
            .await;
        let host = "missing.example.test";
        let url = format!("http://{host}:{}/gone", server.address().port());
        let pin = Some((host.to_string(), "127.0.0.1".parse().unwrap()));
        let context = ToolContext::new(PathBuf::from(".")).with_state_namespace("missing-page");

        let err = fetch_page_with_initial_pin(&url, 5_000, &context, Some(pin))
            .await
            .expect_err("404 stays a failure");
        assert_eq!(http_status_of(&err.to_string()), Some(404));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn source_failures_are_classified_and_gates_pass_through() {
        let _lock = lock_web_run_test_state();
        reset_web_run_state();
        let namespace = "source-failure-session";

        let refused = source_failure(
            namespace,
            "https://reviews.example.com/espresso",
            ToolError::execution_failed(
                "Web request to https://reviews.example.com/espresso failed: HTTP 403",
            ),
        )
        .expect("a refusal is a source outcome");
        assert_eq!(refused.status, SourceStatus::Unavailable);
        assert!(
            with_state(|state| state.refusing_hosts(namespace)).contains("reviews.example.com"),
            "a refusing host is remembered for this session"
        );

        let flaky = source_failure(
            namespace,
            "https://slow.example.com/",
            ToolError::execution_failed(
                "Web request to https://slow.example.com/ failed: HTTP 503",
            ),
        )
        .expect("a 5xx is a source outcome");
        assert_eq!(flaky.status, SourceStatus::Transient);
        let timeout = source_failure(
            namespace,
            "https://slow.example.com/",
            ToolError::execution_failed("request timed out before retry completed"),
        )
        .expect("a timeout is a source outcome");
        assert_eq!(timeout.status, SourceStatus::Transient);

        let gate = source_failure(
            namespace,
            "http://10.0.0.5/",
            ToolError::permission_denied("IP 10.0.0.5 is a restricted address"),
        );
        assert!(
            gate.is_err(),
            "gates must never be softened into source outcomes"
        );
        let bad_ref = source_failure(
            namespace,
            "turn9search9",
            ToolError::invalid_input("Unknown ref_id 'turn9search9'"),
        );
        assert!(bad_ref.is_err(), "caller mistakes stay errors");
    }

    #[test]
    fn search_results_prefer_hosts_that_load() {
        let mut results = vec![
            NormalizedSearchResult::new(
                1,
                "a".into(),
                "https://blocked.example/a".into(),
                None,
                None,
            ),
            NormalizedSearchResult::new(2, "b".into(), "https://open.example/b".into(), None, None),
            NormalizedSearchResult::new(3, "c".into(), "https://open.example/c".into(), None, None),
        ];
        let refusing = HashSet::from(["blocked.example".to_string()]);
        prefer_loading_sources(&mut results, &refusing);
        let order: Vec<_> = results.iter().map(|r| (r.rank, r.url.as_str())).collect();
        assert_eq!(
            order,
            vec![
                (1, "https://open.example/b"),
                (2, "https://open.example/c"),
                (3, "https://blocked.example/a"),
            ]
        );
    }
}
