//! Which (route, model) pairs this person actually uses (#6533).
//!
//! Replaces the additive `settings.toml [enabled_models]` store as the
//! `/model` picker's notion of "models I use". That store was appended to on
//! every switch by older builds and never pruned, so the default picker view
//! reflected history nobody curated. This index is derived from data that
//! already exists — saved session metadata (`model_provider[_id]`, `model`,
//! `updated_at`) and the redacted `cost.route_receipts` each session keeps —
//! and adds no new store.
//!
//! Known limitations:
//! - Resolution is per session, not per turn: a session's `updated_at` is the
//!   last-used time for every route it touched, and a route counts once per
//!   session however many turns it ran.
//! - In-session use is recorded on route switches only (see
//!   [`RouteUsageIndex::record`]); turns on the startup route reach the index
//!   through the saved session on the next start.
//! - Receipts are redacted strings; a model id containing characters the
//!   receipt formatter replaces is keyed by its redacted spelling.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};

use crate::session_manager::SessionMetadata;

/// Sessions older than this contribute nothing.
pub const WINDOW_DAYS: i64 = 30;
/// Score halves every week.
const HALF_LIFE_DAYS: f64 = 7.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RouteKey {
    /// Exact persistence identity (`deepseek`, or a named custom route's
    /// `[providers.<name>]` key). Case is preserved: case-distinct custom
    /// tables are distinct routes.
    identity: String,
    /// Lower-cased model id.
    model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteUsage {
    pub identity: String,
    /// Model id as first seen (display spelling).
    pub model: String,
    pub last_used: DateTime<Utc>,
    pub sessions: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RouteUsageIndex {
    entries: HashMap<RouteKey, RouteUsage>,
    /// Routes already counted by [`Self::record`] in this live session, so
    /// switching A → B → A counts A once.
    live: HashSet<RouteKey>,
}

/// Shared between the startup builder thread and the UI.
pub type SharedRouteUsage = Arc<RwLock<RouteUsageIndex>>;

impl RouteUsageIndex {
    /// Build from saved session metadata. Archived sessions and sessions not
    /// updated within [`WINDOW_DAYS`] of `now` are skipped.
    pub fn from_sessions(sessions: &[SessionMetadata], now: DateTime<Utc>) -> Self {
        let cutoff = now - Duration::days(WINDOW_DAYS);
        let mut index = Self::default();
        for session in sessions {
            if session.archived || session.updated_at < cutoff {
                continue;
            }
            let mut routes: BTreeSet<(String, String)> = BTreeSet::new();
            let identity = session
                .model_provider_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .unwrap_or(session.model_provider.trim());
            routes.insert((identity.to_string(), session.model.trim().to_string()));
            for receipt in &session.cost.route_receipts {
                if let Some(route) = parse_route_receipt(receipt) {
                    routes.insert(route);
                }
            }
            // One session counts once per route, however many receipts or
            // spellings name it.
            let mut seen = BTreeSet::new();
            for (identity, model) in routes {
                if seen.insert(key(&identity, &model)) {
                    index.add(&identity, &model, session.updated_at, 1);
                }
            }
        }
        index
    }

    fn add(&mut self, identity: &str, model: &str, at: DateTime<Utc>, sessions: u32) {
        let identity = identity.trim();
        let model = model.trim();
        if identity.is_empty() || model.is_empty() || model.eq_ignore_ascii_case("auto") {
            return;
        }
        let entry = self
            .entries
            .entry(key(identity, model))
            .or_insert_with(|| RouteUsage {
                identity: identity.to_string(),
                model: model.to_string(),
                last_used: at,
                sessions: 0,
            });
        entry.sessions = entry.sessions.saturating_add(sessions);
        if at > entry.last_used {
            entry.last_used = at;
        }
    }

    /// Record in-session use of a route (a committed switch). The first record
    /// of a route counts as one more session so a route picked today outranks
    /// one last used a month ago; later records of the same route in this
    /// session only refresh its last-used time.
    pub fn record(&mut self, identity: &str, model: &str, at: DateTime<Utc>) {
        let first_this_session = self.live.insert(key(identity, model));
        self.add(identity, model, at, u32::from(first_this_session));
    }

    /// Fold a freshly built index into this one, keeping in-session records
    /// made while the builder was still reading.
    pub fn merge(&mut self, other: Self) {
        for usage in other.entries.into_values() {
            self.add(
                &usage.identity,
                &usage.model,
                usage.last_used,
                usage.sessions,
            );
        }
    }

    /// `sessions × 0.5^(age_days / 7)`; zero outside the window.
    #[cfg(any(test, feature = "test-support"))]
    pub fn score(&self, identity: &str, model: &str, now: DateTime<Utc>) -> f64 {
        self.entries
            .get(&key(identity, model))
            .map_or(0.0, |usage| decayed_score(usage, now))
    }

    /// Routes used within the window, highest score first. Ties break on the
    /// more recent use, then identity/model for a stable order.
    pub fn ranked(&self, now: DateTime<Utc>) -> Vec<&RouteUsage> {
        let cutoff = now - Duration::days(WINDOW_DAYS);
        let mut ranked: Vec<_> = self
            .entries
            .values()
            .filter(|usage| usage.last_used >= cutoff)
            .collect();
        ranked.sort_by(|a, b| {
            decayed_score(b, now)
                .total_cmp(&decayed_score(a, now))
                .then_with(|| b.last_used.cmp(&a.last_used))
                .then_with(|| a.identity.cmp(&b.identity))
                .then_with(|| a.model.cmp(&b.model))
        });
        ranked
    }

    /// Model ids used on exactly this route within the window.
    pub fn models_for_identity(&self, identity: &str, now: DateTime<Utc>) -> Vec<String> {
        self.ranked(now)
            .into_iter()
            .filter(|usage| usage.identity == identity)
            .map(|usage| usage.model.clone())
            .collect()
    }

    pub fn identity_used(&self, identity: &str, now: DateTime<Utc>) -> bool {
        let cutoff = now - Duration::days(WINDOW_DAYS);
        self.entries
            .values()
            .any(|usage| usage.identity == identity && usage.last_used >= cutoff)
    }
}

fn key(identity: &str, model: &str) -> RouteKey {
    RouteKey {
        identity: identity.trim().to_string(),
        model: model.trim().to_ascii_lowercase(),
    }
}

fn decayed_score(usage: &RouteUsage, now: DateTime<Utc>) -> f64 {
    let age = now.signed_duration_since(usage.last_used);
    if age > Duration::days(WINDOW_DAYS) {
        return 0.0;
    }
    let age_days = (age.num_seconds().max(0) as f64) / 86_400.0;
    f64::from(usage.sessions) * 0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

/// `(identity, model)` from one `cost_status::route_receipt` string. The
/// identity falls back to the provider kind when the receipt carries `-`.
fn parse_route_receipt(receipt: &str) -> Option<(String, String)> {
    let mut provider = None;
    let mut identity = None;
    let mut model = None;
    for field in receipt.split_whitespace() {
        if let Some(value) = field.strip_prefix("provider=") {
            provider = Some(value);
        } else if let Some(value) = field.strip_prefix("identity=") {
            identity = Some(value);
        } else if let Some(value) = field.strip_prefix("model=") {
            model = Some(value);
        }
    }
    let identity = identity
        .filter(|id| *id != "-" && !id.is_empty())
        .or(provider)?;
    let model = model.filter(|model| *model != "-" && !model.is_empty())?;
    Some((identity.to_string(), model.to_string()))
}

/// Build the index from the saved sessions on a dedicated thread and merge it
/// into `shared` when done. Listing reads only each file's metadata prefix,
/// but hundreds of files are still too slow for the UI thread.
pub fn spawn_build(shared: SharedRouteUsage) {
    // Unit tests build `App`s by the hundred; they seed usage explicitly
    // rather than racing a reader of the (hermetic) sessions directory.
    if cfg!(any(test, feature = "test-support")) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("model-relevance".to_string())
        .spawn(move || {
            let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
                return;
            };
            let Ok(sessions) = manager.list_sessions() else {
                return;
            };
            let built = RouteUsageIndex::from_sessions(&sessions, Utc::now());
            if let Ok(mut index) = shared.write() {
                index.merge(built);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("model relevance index not built: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::SessionMetadata;

    fn session(
        provider: &str,
        provider_id: Option<&str>,
        model: &str,
        age_days: i64,
        now: DateTime<Utc>,
    ) -> SessionMetadata {
        let mut metadata: SessionMetadata = serde_json::from_value(serde_json::json!({
            "id": "s",
            "title": "t",
            "created_at": now,
            "updated_at": now,
            "message_count": 1,
            "total_tokens": 0,
            "model": model,
            "model_provider": provider,
            "workspace": "/tmp",
        }))
        .expect("session metadata");
        metadata.model_provider_id = provider_id.map(str::to_string);
        metadata.updated_at = now - Duration::days(age_days);
        metadata
    }

    #[test]
    fn recency_and_session_count_rank_with_a_one_week_half_life() {
        let now = Utc::now();
        let sessions = vec![
            // Used a lot, but three weeks ago: 4 × 0.125 = 0.5.
            session("zai", None, "GLM-5.3", 21, now),
            session("zai", None, "GLM-5.3", 21, now),
            session("zai", None, "GLM-5.3", 21, now),
            session("zai", None, "glm-5.3", 21, now),
            // Once, today: 1.0.
            session("xai", None, "grok-4.7", 0, now),
            // Outside the window: nothing.
            session("zai", None, "GLM-5.2", 31, now),
        ];
        let index = RouteUsageIndex::from_sessions(&sessions, now);
        let ranked: Vec<_> = index
            .ranked(now)
            .iter()
            .map(|usage| {
                (
                    usage.identity.as_str(),
                    usage.model.as_str(),
                    usage.sessions,
                )
            })
            .collect();
        assert_eq!(
            ranked,
            vec![("xai", "grok-4.7", 1), ("zai", "GLM-5.3", 4)],
            "case-insensitive model ids merge; stale sessions drop out"
        );
        assert!((index.score("zai", "glm-5.3", now) - 0.5).abs() < 1e-9);
        assert_eq!(index.score("zai", "GLM-5.2", now), 0.0);
        assert!(!index.identity_used("openrouter", now));
    }

    #[test]
    fn archived_sessions_are_excluded() {
        let now = Utc::now();
        let mut archived = session("openrouter", None, "stealth/ox-alpha", 0, now);
        archived.archived = true;
        let index = RouteUsageIndex::from_sessions(&[archived], now);
        assert!(index.ranked(now).is_empty());
    }

    #[test]
    fn named_custom_routes_with_the_same_model_stay_distinct() {
        let now = Utc::now();
        let sessions = vec![
            session("custom", Some("command_code"), "deepseek/v4", 0, now),
            session("custom", Some("other_code"), "deepseek/v4", 2, now),
            session("custom", Some("Other_Code"), "deepseek/v4", 3, now),
        ];
        let index = RouteUsageIndex::from_sessions(&sessions, now);
        assert_eq!(index.ranked(now).len(), 3);
        assert_eq!(
            index.models_for_identity("command_code", now),
            ["deepseek/v4"]
        );
        assert!(index.identity_used("Other_Code", now));
        assert!(!index.identity_used("custom", now));
    }

    #[test]
    fn route_receipts_add_background_routes_once_per_session() {
        let now = Utc::now();
        let mut metadata = session("deepseek", None, "deepseek-flash", 1, now);
        for receipt in [
            "provider=xai identity=- model=grok-4.6 surface=api endpoint_fp=a billing_mode=metered currency=USD",
            "provider=xai identity=xai model=grok-4.6 surface=api endpoint_fp=b billing_mode=metered currency=USD",
            "provider=custom identity=command_code model=deepseek/v4 surface=api endpoint_fp=c billing_mode=metered currency=USD",
            "provider=deepseek identity=deepseek model=deepseek-flash surface=api endpoint_fp=d billing_mode=metered currency=USD",
        ] {
            metadata.cost.route_receipts.insert(receipt.to_string());
        }
        let index = RouteUsageIndex::from_sessions(&[metadata], now);
        let sessions = |identity: &str, model: &str| {
            index
                .ranked(now)
                .into_iter()
                .find(|usage| usage.identity == identity && usage.model.eq_ignore_ascii_case(model))
                .map(|usage| usage.sessions)
        };
        assert_eq!(sessions("xai", "grok-4.6"), Some(1));
        assert_eq!(sessions("command_code", "deepseek/v4"), Some(1));
        assert_eq!(sessions("deepseek", "deepseek-flash"), Some(1));
    }

    #[test]
    fn in_session_record_outranks_old_use_and_survives_a_late_build() {
        let now = Utc::now();
        let mut live = RouteUsageIndex::default();
        live.record("stepfun", "step-5-preview", now);
        let built = RouteUsageIndex::from_sessions(
            &[session("deepseek", None, "deepseek-v4-pro", 14, now)],
            now,
        );
        live.merge(built);
        let order: Vec<_> = live
            .ranked(now)
            .iter()
            .map(|usage| usage.model.clone())
            .collect();
        assert_eq!(order, ["step-5-preview", "deepseek-v4-pro"]);
    }

    #[test]
    fn switching_back_and_forth_counts_each_route_once_per_session() {
        let now = Utc::now();
        let mut live = RouteUsageIndex::default();
        live.record("deepseek", "deepseek-v4-pro", now - Duration::minutes(5));
        live.record("xai", "grok-4.7", now - Duration::minutes(4));
        live.record("deepseek", "DeepSeek-V4-Pro", now - Duration::minutes(3));
        live.record("deepseek", "deepseek-v4-pro", now);
        let pro = live
            .ranked(now)
            .into_iter()
            .find(|usage| usage.identity == "deepseek")
            .expect("deepseek route recorded");
        assert_eq!(pro.sessions, 1, "re-selecting a route is not a new session");
        assert_eq!(pro.last_used, now, "re-selecting refreshes last use");
    }
}
