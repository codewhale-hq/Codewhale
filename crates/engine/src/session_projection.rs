//! One projection of durable saved sessions, shared by every surface that
//! browses them (#2934 / #4397).
//!
//! Before this module the TUI session picker and the Runtime API answered
//! "what sessions are there?" with two different shapes: the picker filtered,
//! sorted, and fuzzy-matched [`SessionMetadata`] in place, while
//! `GET /v1/sessions` returned an unfiltered, unsorted metadata dump with an
//! ad-hoc substring search. That divergence is exactly what the web dashboard
//! could not consume, and it is what made "the rail says one thing, the
//! dashboard says another" possible.
//!
//! Everything here is pure and offline:
//!
//! * no provider or network call — browsing and resuming must never talk to a
//!   model, so a projection is computed from already-read metadata only;
//! * no session-file reads — [`SessionSummary`] is built from
//!   [`SessionMetadata`], which the manager already extracts from a bounded
//!   64 KB prefix. A rail that re-read every transcript on every render would
//!   be a per-keystroke I/O storm on a 50-session store.
//!
//! The consequence is deliberate and worth stating plainly: a summary's
//! `preview` is the session's own title, not its last message. Session
//! metadata does not record a last message, and inventing one by reading N
//! transcripts per frame would trade a truthful cheap row for an expensive
//! one. Full transcript preview stays where it already works — the session
//! picker, which reads one selected session and caches it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::session_manager::{SessionListFilter, SessionMetadata, workspace_scope_matches};

/// Maximum rows any single projection returns. Bounds the sidebar rail, the
/// `/v1/sessions/summary` response, and search results with one number so a
/// user with hundreds of sessions cannot make a surface unbounded.
pub const MAX_PROJECTED_SESSIONS: usize = 500;

/// Ordering for a session listing.
///
/// The same three modes the picker has always cycled with `s`; naming them
/// here lets the Runtime API offer the identical ordering instead of relying
/// on whatever order `read_dir` happened to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSortMode {
    /// Most recently updated first.
    #[default]
    Recent,
    /// Title, ascending.
    Name,
    /// Message count, descending.
    Size,
}

impl SessionSortMode {
    /// Parse a wire/config value. Unknown values fall back to `Recent` rather
    /// than erroring, so a stale client cannot break a listing.
    #[must_use]
    pub fn from_str_or_recent(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "name" | "title" | "alpha" => Self::Name,
            "size" | "messages" | "length" => Self::Size,
            _ => Self::Recent,
        }
    }

    /// Advance to the next mode in the picker's cycle order.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Recent => Self::Name,
            Self::Name => Self::Size,
            Self::Size => Self::Recent,
        }
    }
}

/// What a browse surface is asking for.
#[derive(Debug, Clone)]
pub struct SessionQuery {
    /// Archive state to include. Defaults to active-only.
    pub filter: SessionListFilter,
    pub sort: SessionSortMode,
    /// Fuzzy query over title, id, and workspace. Empty means "no filter".
    pub search: String,
    /// When `Some`, only sessions recorded against an equivalent workspace
    /// are returned. `None` means the caller deliberately opted out of
    /// scoping (the picker's `a` toggle, or an API caller that asked for
    /// every workspace).
    pub workspace_scope: Option<PathBuf>,
    /// Exclude empty auto-created sessions (zero messages under the default
    /// title) — the same definition the launch list and `--continue` apply
    /// (#6014). Off by default so API listings stay complete; browse
    /// surfaces that exist to pick *resumable* work set it.
    pub hide_empty_auto_created: bool,
    /// Hard row cap, clamped to [`MAX_PROJECTED_SESSIONS`].
    pub limit: usize,
}

impl Default for SessionQuery {
    fn default() -> Self {
        Self {
            filter: SessionListFilter::ActiveOnly,
            sort: SessionSortMode::Recent,
            search: String::new(),
            workspace_scope: None,
            hide_empty_auto_created: false,
            limit: MAX_PROJECTED_SESSIONS,
        }
    }
}

impl SessionQuery {
    /// Scope the query to one workspace.
    #[must_use]
    pub fn scoped_to(mut self, workspace: &Path) -> Self {
        self.workspace_scope = Some(workspace.to_path_buf());
        self
    }

    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    #[must_use]
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = search.into();
        self
    }

    #[must_use]
    pub fn with_filter(mut self, filter: SessionListFilter) -> Self {
        self.filter = filter;
        self
    }

    #[must_use]
    pub fn with_sort(mut self, sort: SessionSortMode) -> Self {
        self.sort = sort;
        self
    }

    /// Exclude empty auto-created sessions from the listing (#6014).
    #[must_use]
    pub fn without_empty_auto_created(mut self) -> Self {
        self.hide_empty_auto_created = true;
        self
    }
}

/// One durable session, projected for display.
///
/// Field names deliberately mirror `ThreadSummary` in
/// [`crate::runtime_api`] (`id`, `title`, `preview`, `model`, `mode`,
/// `workspace`, `archived`, `updated_at`) so the embedded dashboard can render
/// a saved session and a live thread with the same row code, and so a reader
/// comparing the two payloads sees one vocabulary rather than two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// Bounded preview text. See the module docs: this is the session title,
    /// never a fabricated "last message" the metadata does not record.
    pub preview: String,
    pub model: String,
    pub mode: String,
    pub workspace: PathBuf,
    pub archived: bool,
    pub message_count: usize,
    pub total_tokens: u64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Set when this session was created by `/fork`, so lineage is visible
    /// without opening the session.
    pub parent_session_id: Option<String>,
    /// True when this row is the session the calling surface currently has
    /// loaded. Computed by the caller passing its active session id; never
    /// inferred from disk state.
    pub is_current: bool,
}

/// Longest preview/title a projected row carries.
const MAX_SUMMARY_TEXT: usize = 140;

impl SessionSummary {
    fn from_metadata(metadata: &SessionMetadata, current_session_id: Option<&str>) -> Self {
        let title = bounded(&metadata.title, MAX_SUMMARY_TEXT);
        Self {
            preview: title.clone(),
            id: metadata.id.clone(),
            title,
            model: metadata.model.clone(),
            mode: metadata.mode.clone().unwrap_or_else(|| "agent".to_string()),
            workspace: metadata.workspace.clone(),
            archived: metadata.archived,
            message_count: metadata.message_count,
            total_tokens: metadata.total_tokens,
            updated_at: metadata.updated_at,
            created_at: metadata.created_at,
            parent_session_id: metadata.parent_session_id.clone(),
            is_current: current_session_id.is_some_and(|id| id == metadata.id),
        }
    }
}

fn bounded(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "Untitled session".to_string();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Filter, sort, and bound a metadata list — the single selection seam.
///
/// Every browse surface funnels through this: the TUI session picker's
/// filtered list, the sidebar rail, and both `/v1/sessions` routes. Returning
/// borrowed metadata (rather than only [`SessionSummary`]) is what lets the
/// picker use it — the picker renders from `SessionMetadata`, and giving it a
/// summary-only API is exactly what pushed it into keeping a private
/// filter/sort in the first place.
///
/// `sessions` is whatever [`crate::session_manager::SessionManager::list_sessions`]
/// returned; this function never touches the filesystem.
#[must_use]
pub fn select_sessions<'a>(
    sessions: &'a [SessionMetadata],
    query: &SessionQuery,
) -> Vec<&'a SessionMetadata> {
    let mut matched: Vec<&SessionMetadata> = sessions
        .iter()
        .filter(|session| query.filter.admits(session.archived))
        .filter(|session| {
            !query.hide_empty_auto_created
                || !crate::session_manager::is_empty_auto_created_session(session)
        })
        .filter(|session| matches_workspace_scope(session, query.workspace_scope.as_deref()))
        .filter(|session| session_matches_query(&query.search, session))
        .collect();

    match query.sort {
        // Ties break on id in every mode so a listing is stable across
        // processes rather than inheriting directory-read order. Two surfaces
        // that sort "the same way" but tie-break differently are two backends.
        SessionSortMode::Recent => matched.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        }),
        SessionSortMode::Name => {
            matched.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)))
        }
        SessionSortMode::Size => matched.sort_by(|a, b| {
            b.message_count
                .cmp(&a.message_count)
                .then_with(|| b.updated_at.cmp(&a.updated_at))
                .then_with(|| a.id.cmp(&b.id))
        }),
    }

    matched.truncate(query.limit.min(MAX_PROJECTED_SESSIONS));
    matched
}

/// [`select_sessions`], projected into display rows.
#[must_use]
pub fn project_sessions(
    sessions: &[SessionMetadata],
    query: &SessionQuery,
    current_session_id: Option<&str>,
) -> Vec<SessionSummary> {
    select_sessions(sessions, query)
        .into_iter()
        .map(|metadata| SessionSummary::from_metadata(metadata, current_session_id))
        .collect()
}

/// Does this session belong to `scope`?
///
/// `None` means the caller opted out of scoping. Matching reuses
/// [`workspace_scope_matches`], so a worktree and its repository root are
/// treated the same way the resume path already treats them — the rail cannot
/// disagree with what `--continue` would pick.
#[must_use]
pub fn matches_workspace_scope(session: &SessionMetadata, scope: Option<&Path>) -> bool {
    match scope {
        None => true,
        Some(scope) => workspace_scope_matches(&session.workspace, scope),
    }
}

/// Fuzzy match over title, id, and workspace.
///
/// Case-insensitive substring first, then subsequence. This is the session
/// picker's historical `fuzzy_match` behaviour, lifted here verbatim so the
/// picker, the rail, and `GET /v1/sessions?search=` cannot disagree about
/// what "matches". An empty or whitespace-only query matches everything.
#[must_use]
pub fn session_matches_query(query: &str, session: &SessionMetadata) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {}",
        session.title,
        session.id,
        session.workspace.display()
    )
    .to_ascii_lowercase();
    if haystack.contains(&query) {
        return true;
    }
    is_subsequence(&query, &haystack)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars();
    let mut current = match chars.next() {
        Some(c) => c,
        None => return true,
    };
    for ch in haystack.chars() {
        if ch == current {
            match chars.next() {
                Some(next) => current = next,
                None => return true,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn metadata(id: &str, title: &str, workspace: &str, minutes_ago: i64) -> SessionMetadata {
        let ts = Utc::now() - Duration::minutes(minutes_ago);
        SessionMetadata {
            id: id.to_string(),
            title: title.to_string(),
            created_at: ts,
            updated_at: ts,
            message_count: title.len(),
            total_tokens: 0,
            model: "deepseek-chat".to_string(),
            model_provider: "deepseek".to_string(),
            model_provider_id: None,
            workspace: PathBuf::from(workspace),
            mode: Some("agent".to_string()),
            cost: Default::default(),
            parent_session_id: None,
            forked_from_message_count: None,
            runtime_store: None,
            cumulative_turn_secs: 0,
            archived: false,
            spawn_depth: 0,
        }
    }

    #[test]
    fn recent_sort_puts_newest_first_and_marks_the_current_row() {
        let sessions = vec![
            metadata("old", "Older work", "/repo", 120),
            metadata("new", "Newer work", "/repo", 5),
        ];
        let rows = project_sessions(&sessions, &SessionQuery::default(), Some("old"));

        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert!(!rows[0].is_current);
        assert!(rows[1].is_current);
    }

    #[test]
    fn archived_sessions_are_hidden_until_explicitly_requested() {
        let mut archived = metadata("gone", "Archived work", "/repo", 1);
        archived.archived = true;
        let sessions = vec![archived, metadata("live", "Live work", "/repo", 2)];

        let active = project_sessions(&sessions, &SessionQuery::default(), None);
        assert_eq!(
            active.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["live"]
        );

        let all = project_sessions(
            &sessions,
            &SessionQuery::default().with_filter(SessionListFilter::IncludeArchived),
            None,
        );
        assert_eq!(all.len(), 2);

        let only = project_sessions(
            &sessions,
            &SessionQuery::default().with_filter(SessionListFilter::ArchivedOnly),
            None,
        );
        assert_eq!(
            only.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["gone"]
        );
    }

    #[test]
    fn workspace_scope_excludes_other_projects_and_none_opts_out() {
        let sessions = vec![
            metadata("here", "Here", "/repo-a", 1),
            metadata("there", "There", "/repo-b", 2),
        ];
        let scoped = project_sessions(
            &sessions,
            &SessionQuery::default().scoped_to(Path::new("/repo-a")),
            None,
        );
        assert_eq!(
            scoped.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["here"]
        );

        let unscoped = project_sessions(&sessions, &SessionQuery::default(), None);
        assert_eq!(unscoped.len(), 2);
    }

    #[test]
    fn search_matches_substring_and_subsequence_over_title_id_and_workspace() {
        let sessions = vec![metadata("abc123", "Whale migration notes", "/repo-a", 1)];

        for query in ["whale", "WHALE", "abc1", "repo-a", "wmn"] {
            let rows =
                project_sessions(&sessions, &SessionQuery::default().with_search(query), None);
            assert_eq!(rows.len(), 1, "query {query} should match");
        }

        let rows = project_sessions(
            &sessions,
            &SessionQuery::default().with_search("zzzz"),
            None,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn sort_modes_are_deterministic_across_equal_keys() {
        let mut a = metadata("aaa", "Same title", "/repo", 1);
        let mut b = metadata("bbb", "Same title", "/repo", 1);
        a.message_count = 4;
        b.message_count = 4;
        let sessions = vec![b, a];

        let by_name = project_sessions(
            &sessions,
            &SessionQuery::default().with_sort(SessionSortMode::Name),
            None,
        );
        assert_eq!(
            by_name.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );
    }

    #[test]
    fn size_sort_orders_by_message_count_descending() {
        let mut small = metadata("small", "Small", "/repo", 1);
        let mut large = metadata("large", "Large", "/repo", 2);
        small.message_count = 2;
        large.message_count = 40;

        let rows = project_sessions(
            &[small, large],
            &SessionQuery::default().with_sort(SessionSortMode::Size),
            None,
        );
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["large", "small"]
        );
    }

    #[test]
    fn limit_is_bounded_by_the_projection_cap() {
        let sessions: Vec<SessionMetadata> = (0..20)
            .map(|i| metadata(&format!("s{i}"), &format!("Session {i}"), "/repo", i))
            .collect();

        let rows = project_sessions(&sessions, &SessionQuery::default().with_limit(5), None);
        assert_eq!(rows.len(), 5);

        let capped = project_sessions(
            &sessions,
            &SessionQuery::default().with_limit(usize::MAX),
            None,
        );
        assert_eq!(capped.len(), 20);
    }

    #[test]
    fn preview_is_the_title_and_never_a_fabricated_last_message() {
        let sessions = vec![metadata("s", "Refactor the lane registry", "/repo", 1)];
        let rows = project_sessions(&sessions, &SessionQuery::default(), None);
        assert_eq!(rows[0].preview, "Refactor the lane registry");
        assert_eq!(rows[0].preview, rows[0].title);
    }

    #[test]
    fn long_titles_are_bounded_and_blank_titles_get_a_stable_label() {
        let long = "x".repeat(400);
        let sessions = vec![
            metadata("s", &long, "/repo", 1),
            metadata("b", "   ", "/repo", 2),
        ];
        let rows = project_sessions(&sessions, &SessionQuery::default(), None);

        let long_row = rows.iter().find(|r| r.id == "s").expect("long row");
        assert_eq!(long_row.title.chars().count(), MAX_SUMMARY_TEXT);
        assert!(long_row.title.ends_with('…'));

        let blank_row = rows.iter().find(|r| r.id == "b").expect("blank row");
        assert_eq!(blank_row.title, "Untitled session");
    }

    #[test]
    fn every_wire_alias_parses_and_unknown_values_fall_back_to_recent() {
        for value in ["recent", "", "  ", "nonsense"] {
            assert_eq!(
                SessionSortMode::from_str_or_recent(value),
                SessionSortMode::Recent
            );
        }
        for value in ["name", "Title", " ALPHA "] {
            assert_eq!(
                SessionSortMode::from_str_or_recent(value),
                SessionSortMode::Name
            );
        }
        for value in ["size", "messages", "length"] {
            assert_eq!(
                SessionSortMode::from_str_or_recent(value),
                SessionSortMode::Size
            );
        }
    }

    #[test]
    fn the_sort_cycle_visits_every_mode_and_returns_to_the_start() {
        let mut mode = SessionSortMode::Recent;
        let mut seen = vec![mode];
        for _ in 0..3 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            vec![
                SessionSortMode::Recent,
                SessionSortMode::Name,
                SessionSortMode::Size,
                SessionSortMode::Recent
            ]
        );
    }

    #[test]
    fn list_filter_resolves_the_same_query_pair_as_threads() {
        assert_eq!(
            SessionListFilter::from_query(None, None),
            SessionListFilter::ActiveOnly
        );
        assert_eq!(
            SessionListFilter::from_query(Some(true), None),
            SessionListFilter::IncludeArchived
        );
        assert_eq!(
            SessionListFilter::from_query(Some(true), Some(true)),
            SessionListFilter::ArchivedOnly
        );
        assert_eq!(
            SessionListFilter::from_query(None, Some(true)),
            SessionListFilter::ArchivedOnly
        );
    }
}
