use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const GRANT_SCHEMA_VERSION: u32 = 1;
const GRANTS_FILE: &str = "approval_grants.json";
const GRANT_LOG_FILE: &str = "approval_grants.jsonl";
pub const DEFAULT_WORKSPACE_IDLE_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const CLOCK_SKEW_TOLERANCE_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantScope {
    Thread { thread_id: String },
    Workspace { workspace_id: String },
}

impl GrantScope {
    fn narrowness(&self) -> u8 {
        match self {
            Self::Thread { .. } => 2,
            Self::Workspace { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantOrigin {
    Interactive,
    ChildAgent,
    Automation,
    Acp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostureCeiling {
    Ask,
    AutoReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantPosture {
    Ask,
    AutoReview,
    FullAccess,
    Never,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Active,
    Revoked,
    Expired,
    Exhausted,
    Suspended,
}

/// Who answered. There is deliberately no variant for model output, tool
/// text, a bot, or a hosted gate: a grant can only be minted from an
/// authenticated human channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantDecider {
    HumanCard,
    HumanSettings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantSurface {
    Tui,
    App,
    Web,
    Runtime,
    Cli,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantMatcher {
    pub tool_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv_prefix: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_host: Option<String>,
}

impl GrantMatcher {
    fn narrows_beyond_class(&self) -> bool {
        self.exact_digest.is_some()
            || self.grouping_key.is_some()
            || !self.argv_prefix.is_empty()
            || self.net_host.is_some()
    }

    fn matches(&self, query: &GrantQuery<'_>) -> bool {
        self.tool_class == query.tool_class
            && self
                .exact_digest
                .as_deref()
                .is_none_or(|digest| digest == query.exact_digest)
            && self
                .grouping_key
                .as_deref()
                .is_none_or(|key| key == query.grouping_key)
            && (self.argv_prefix.is_empty() || query.argv.starts_with(&self.argv_prefix))
            && self.net_host.as_deref().is_none_or(|host| {
                query
                    .net_host
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(host))
            })
    }

    fn specificity(&self) -> (u8, usize) {
        if self.exact_digest.is_some() {
            (3, 0)
        } else if !self.argv_prefix.is_empty() {
            (2, self.argv_prefix.len())
        } else if self.grouping_key.is_some() || self.net_host.is_some() {
            (1, 0)
        } else {
            (0, 0)
        }
    }

    fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        hex_digest(&canonical)
    }

    fn label(&self) -> String {
        if let Some(key) = &self.grouping_key {
            return format!("{}: {key}", self.tool_class);
        }
        if !self.argv_prefix.is_empty() {
            return format!("{}: {} ...", self.tool_class, self.argv_prefix.join(" "));
        }
        if let Some(host) = &self.net_host {
            return format!("{}: {host}", self.tool_class);
        }
        if self.exact_digest.is_some() {
            return format!("{}: this exact call", self.tool_class);
        }
        format!("any {} call", self.tool_class)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantDisplay {
    pub label: String,
    #[serde(default)]
    pub tool_name: String,
    pub redacted_summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(default)]
    pub uses: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantAppliesTo {
    pub origins: Vec<GrantOrigin>,
    pub posture_ceiling: PostureCeiling,
}

impl Default for GrantAppliesTo {
    fn default() -> Self {
        Self {
            origins: vec![GrantOrigin::Interactive],
            posture_ceiling: PostureCeiling::Ask,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantCreated {
    pub at: DateTime<Utc>,
    pub principal: String,
    pub surface: GrantSurface,
    pub decider: GrantDecider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_approval_id: Option<String>,
    pub engine_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantRevoked {
    pub at: DateTime<Utc>,
    pub by: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub grant_id: String,
    pub schema_v: u32,
    pub effect: GrantEffect,
    pub scope: GrantScope,
    pub matcher: GrantMatcher,
    pub matcher_digest: String,
    pub display: GrantDisplay,
    pub limits: GrantLimits,
    pub applies_to: GrantAppliesTo,
    pub created: GrantCreated,
    pub state: GrantState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<GrantRevoked>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_reason: Option<String>,
}

impl Grant {
    fn lapse_reason(&self, now: DateTime<Utc>) -> Option<GrantState> {
        if self.limits.expires_at.is_some_and(|at| at <= now) {
            return Some(GrantState::Expired);
        }
        if self.effect == GrantEffect::Deny {
            return None;
        }
        if let Some(ttl) = self.limits.idle_ttl_secs {
            let base = self.limits.last_used_at.unwrap_or(self.created.at);
            let ttl = Duration::seconds(i64::try_from(ttl).unwrap_or(i64::MAX / 2));
            let skewed = base - now > Duration::seconds(CLOCK_SKEW_TOLERANCE_SECS);
            if skewed || base.checked_add_signed(ttl).is_none_or(|end| end <= now) {
                return Some(GrantState::Expired);
            }
        }
        if self
            .limits
            .max_uses
            .is_some_and(|max| self.limits.uses >= max)
        {
            return Some(GrantState::Exhausted);
        }
        None
    }

    fn reference(&self) -> GrantRef {
        GrantRef {
            grant_id: self.grant_id.clone(),
            label: self.display.label.clone(),
        }
    }
}

/// What a decision receipt names: the grant that answered and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRef {
    pub grant_id: String,
    pub label: String,
}

pub struct NewGrant {
    pub effect: GrantEffect,
    pub scope: GrantScope,
    pub matcher: GrantMatcher,
    pub tool_name: String,
    pub redacted_summary: String,
    pub limits: GrantLimits,
    pub applies_to: GrantAppliesTo,
    pub principal: String,
    pub surface: GrantSurface,
    pub decider: GrantDecider,
    pub source_approval_id: Option<String>,
}

pub struct GrantQuery<'a> {
    pub thread_id: &'a str,
    pub workspace_id: &'a str,
    pub origin: GrantOrigin,
    pub posture: GrantPosture,
    pub tool_class: &'a str,
    pub exact_digest: &'a str,
    pub grouping_key: &'a str,
    pub argv: &'a [String],
    pub net_host: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantVerdict {
    Deny(GrantRef),
    Allow(GrantRef),
    NoMatch,
}

#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("an allow grant must narrow beyond the tool class")]
    TooBroad,
    #[error("grant store is degraded: {0}")]
    Degraded(String),
    #[error("a grant needs a non-empty thread or workspace id")]
    InvalidScope,
    #[error("grant is exhausted, expired or no longer active")]
    NotUsable,
    #[error("grant evidence could not be written: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Serialize)]
struct GrantEvent {
    at: DateTime<Utc>,
    event: &'static str,
    grant_id: String,
    effect: GrantEffect,
    scope: GrantScope,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uses: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grant: Option<Grant>,
}

impl GrantEvent {
    fn new(event: &'static str, grant: &Grant) -> Self {
        Self {
            at: Utc::now(),
            event,
            grant_id: grant.grant_id.clone(),
            effect: grant.effect,
            scope: grant.scope.clone(),
            label: grant.display.label.clone(),
            by: None,
            reason: None,
            uses: None,
            grant: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct GrantFile {
    schema_v: u32,
    grants: Vec<Grant>,
}

struct Inner {
    dir: Option<PathBuf>,
    grants: Vec<Grant>,
    degraded: Option<String>,
    stamp: Option<(std::time::SystemTime, u64)>,
}

impl Inner {
    fn file_stamp(&self) -> Option<(std::time::SystemTime, u64)> {
        let meta = fs::metadata(self.dir.as_ref()?.join(GRANTS_FILE)).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    fn refresh(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let stamp = self.file_stamp();
        if stamp.is_none() || stamp == self.stamp {
            return;
        }
        let Ok(bytes) = fs::read(dir.join(GRANTS_FILE)) else {
            return;
        };
        if let Ok(file) = serde_json::from_slice::<GrantFile>(&bytes)
            && file.schema_v <= GRANT_SCHEMA_VERSION
        {
            self.grants = file.grants;
            self.stamp = stamp;
        }
    }

    fn persist(&self, grants: &[Grant], events: &[GrantEvent]) -> std::io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        if !events.is_empty() {
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(GRANT_LOG_FILE))?;
            let mut buf = Vec::new();
            for event in events {
                serde_json::to_writer(&mut buf, event).map_err(std::io::Error::other)?;
                buf.push(b'\n');
            }
            log.write_all(&buf)?;
            log.sync_data()?;
        }
        let file = GrantFile {
            schema_v: GRANT_SCHEMA_VERSION,
            grants: grants.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        crate::utils::write_atomic(&dir.join(GRANTS_FILE), &bytes)
    }

    fn commit(&mut self, grants: Vec<Grant>, events: Vec<GrantEvent>) -> std::io::Result<()> {
        self.persist(&grants, &events)?;
        self.grants = grants;
        self.stamp = self.file_stamp();
        Ok(())
    }

    fn commit_tightening(&mut self, grants: Vec<Grant>, events: Vec<GrantEvent>) {
        if let Err(error) = self.persist(&grants, &events) {
            tracing::warn!(target: "approval", %error, "grant revocation evidence could not be written");
        }
        self.grants = grants;
        self.stamp = self.file_stamp();
    }

    fn sweep(&mut self, now: DateTime<Utc>) {
        self.refresh();
        let mut grants = self.grants.clone();
        let mut events = Vec::new();
        for grant in &mut grants {
            if grant.state != GrantState::Active {
                continue;
            }
            if let Some(state) = grant.lapse_reason(now) {
                grant.state = state;
                let name = if state == GrantState::Expired {
                    "expired"
                } else {
                    "exhausted"
                };
                events.push(GrantEvent::new(name, grant));
            }
        }
        if !events.is_empty() {
            self.commit_tightening(grants, events);
        }
    }

    fn transition(
        &mut self,
        select: impl Fn(&Grant) -> bool,
        from: GrantState,
        to: GrantState,
        event: &'static str,
        by: &str,
        reason: &str,
        tighten: bool,
    ) -> Vec<Grant> {
        self.refresh();
        let mut grants = self.grants.clone();
        let mut events = Vec::new();
        let mut changed = Vec::new();
        for grant in &mut grants {
            if grant.state != from || !select(grant) {
                continue;
            }
            grant.state = to;
            match to {
                GrantState::Revoked => {
                    grant.revoked = Some(GrantRevoked {
                        at: Utc::now(),
                        by: by.to_string(),
                        reason: reason.to_string(),
                    });
                }
                GrantState::Suspended => grant.suspended_reason = Some(reason.to_string()),
                _ => grant.suspended_reason = None,
            }
            let mut record = GrantEvent::new(event, grant);
            record.by = Some(by.to_string());
            record.reason = Some(reason.to_string());
            events.push(record);
            changed.push(grant.clone());
        }
        if changed.is_empty() {
            return changed;
        }
        if tighten {
            self.commit_tightening(grants, events);
            changed
        } else if self.commit(grants, events).is_ok() {
            changed
        } else {
            Vec::new()
        }
    }
}

/// The single grant authority: records, matching, precedence, expiry,
/// revocation and receipts. Cloning shares the same store.
#[derive(Clone)]
pub struct GrantStore {
    inner: Arc<Mutex<Inner>>,
}

impl GrantStore {
    /// Open the file-backed store under `dir`. An unreadable or corrupt store
    /// is left untouched on disk and the store runs degraded (no grants, no
    /// writes, see [`Self::degraded_reason`]): it never allows from a file it
    /// cannot read, and it never overwrites standing denies it could not parse.
    #[must_use]
    pub fn open(dir: &Path) -> Self {
        let mut inner = Inner {
            dir: Some(dir.to_path_buf()),
            grants: Vec::new(),
            degraded: None,
            stamp: None,
        };
        let path = dir.join(GRANTS_FILE);
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<GrantFile>(&bytes) {
                Ok(file) if file.schema_v <= GRANT_SCHEMA_VERSION => {
                    inner.grants = file.grants;
                    inner.stamp = inner.file_stamp();
                }
                Ok(file) => {
                    inner.degraded = Some(format!("grant store schema {} is newer", file.schema_v));
                    inner.dir = None;
                }
                Err(error) => {
                    inner.degraded = Some(format!("grant store was unreadable: {error}"));
                    inner.dir = None;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                inner.degraded = Some(format!("grant store could not be read: {error}"));
                inner.dir = None;
            }
        }
        if let Some(reason) = &inner.degraded {
            tracing::warn!(target: "approval", %reason, "standing grants are unavailable; every call asks");
        }
        inner.transition(
            |grant| {
                grant.effect == GrantEffect::Allow
                    && matches!(grant.scope, GrantScope::Thread { .. })
            },
            GrantState::Active,
            GrantState::Suspended,
            "suspended",
            "engine",
            "restart: awaiting authenticated resume",
            true,
        );
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    #[must_use]
    pub fn degraded_reason(&self) -> Option<String> {
        self.inner.lock().degraded.clone()
    }

    /// Create a grant. Allow grants cannot cover a whole tool class, and an
    /// identical live grant is returned instead of duplicated.
    pub fn create(&self, new: NewGrant) -> Result<Grant, GrantError> {
        if new.effect == GrantEffect::Allow && !new.matcher.narrows_beyond_class() {
            return Err(GrantError::TooBroad);
        }
        let empty_id = match &new.scope {
            GrantScope::Thread { thread_id } => thread_id.is_empty(),
            GrantScope::Workspace { workspace_id } => workspace_id.is_empty(),
        };
        if empty_id {
            return Err(GrantError::InvalidScope);
        }
        let mut inner = self.inner.lock();
        if let Some(reason) = &inner.degraded
            && inner.dir.is_none()
        {
            return Err(GrantError::Degraded(reason.clone()));
        }
        let now = Utc::now();
        inner.sweep(now);
        let matcher_digest = new.matcher.digest();
        if let Some(existing) = inner.grants.iter().find(|grant| {
            matches!(grant.state, GrantState::Active | GrantState::Suspended)
                && grant.effect == new.effect
                && grant.scope == new.scope
                && grant.matcher_digest == matcher_digest
        }) {
            return Ok(existing.clone());
        }
        let mut limits = new.limits;
        if limits.idle_ttl_secs.is_none()
            && new.effect == GrantEffect::Allow
            && matches!(new.scope, GrantScope::Workspace { .. })
        {
            limits.idle_ttl_secs = Some(DEFAULT_WORKSPACE_IDLE_TTL_SECS);
        }
        let grant = Grant {
            grant_id: format!("grt_{}", uuid::Uuid::new_v4().simple()),
            schema_v: GRANT_SCHEMA_VERSION,
            effect: new.effect,
            scope: new.scope,
            display: GrantDisplay {
                label: new.matcher.label(),
                tool_name: new.tool_name,
                redacted_summary: new.redacted_summary,
            },
            matcher: new.matcher,
            matcher_digest,
            limits,
            applies_to: new.applies_to,
            created: GrantCreated {
                at: now,
                principal: new.principal,
                surface: new.surface,
                decider: new.decider,
                source_approval_id: new.source_approval_id,
                engine_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            state: GrantState::Active,
            revoked: None,
            suspended_reason: None,
        };
        let mut event = GrantEvent::new("created", &grant);
        event.by = Some(grant.created.principal.clone());
        event.grant = Some(grant.clone());
        let mut grants = inner.grants.clone();
        grants.push(grant.clone());
        inner.commit(grants, vec![event])?;
        Ok(grant)
    }

    /// Decide a call against the live grants. Deny beats allow in every
    /// posture and for every origin; an allow needs a live posture at or
    /// under its ceiling and an origin it lists. Among allows the most
    /// specific matcher wins, then the narrower scope, then the newest.
    /// This only reads; call [`Self::record_use`] when the allow is applied.
    #[must_use]
    pub fn evaluate(&self, query: &GrantQuery<'_>) -> GrantVerdict {
        let mut inner = self.inner.lock();
        inner.sweep(Utc::now());
        let in_scope = |grant: &&Grant| {
            grant.state == GrantState::Active
                && match &grant.scope {
                    GrantScope::Thread { thread_id } => thread_id == query.thread_id,
                    GrantScope::Workspace { workspace_id } => {
                        !workspace_id.is_empty() && workspace_id == query.workspace_id
                    }
                }
                && grant.matcher.matches(query)
        };
        let best = |effect: GrantEffect, usable: &dyn Fn(&Grant) -> bool| {
            inner
                .grants
                .iter()
                .filter(in_scope)
                .filter(|grant| grant.effect == effect && usable(grant))
                .max_by_key(|grant| {
                    (
                        grant.matcher.specificity(),
                        grant.scope.narrowness(),
                        grant.created.at,
                    )
                })
                .map(Grant::reference)
        };
        if let Some(deny) = best(GrantEffect::Deny, &|_| true) {
            return GrantVerdict::Deny(deny);
        }
        let allow = best(GrantEffect::Allow, &|grant| {
            grant.applies_to.origins.contains(&query.origin)
                && match query.posture {
                    GrantPosture::Ask => true,
                    GrantPosture::AutoReview => {
                        grant.applies_to.posture_ceiling == PostureCeiling::AutoReview
                    }
                    GrantPosture::FullAccess | GrantPosture::Never => false,
                }
        });
        allow.map_or(GrantVerdict::NoMatch, GrantVerdict::Allow)
    }

    /// Count one use of an allow grant under the store lock. Fails closed:
    /// an exhausted, lapsed or unwritable grant is not usable and the caller
    /// must ask instead.
    pub fn record_use(&self, grant_id: &str) -> Result<(), GrantError> {
        let mut inner = self.inner.lock();
        let now = Utc::now();
        inner.sweep(now);
        let mut grants = inner.grants.clone();
        let Some(grant) = grants
            .iter_mut()
            .find(|grant| grant.grant_id == grant_id && grant.state == GrantState::Active)
        else {
            return Err(GrantError::NotUsable);
        };
        grant.limits.uses = grant.limits.uses.saturating_add(1);
        grant.limits.last_used_at = Some(now);
        let mut events = Vec::new();
        if grant.limits.uses == 1 {
            let mut used = GrantEvent::new("used", grant);
            used.uses = Some(1);
            events.push(used);
        }
        if grant
            .limits
            .max_uses
            .is_some_and(|max| grant.limits.uses >= max)
        {
            grant.state = GrantState::Exhausted;
            let mut done = GrantEvent::new("exhausted", grant);
            done.uses = Some(grant.limits.uses);
            events.push(done);
        }
        inner.commit(grants, events)?;
        Ok(())
    }

    /// Revoke one active or suspended grant, optionally only inside `scope`.
    pub fn revoke(
        &self,
        grant_id: &str,
        scope: Option<&GrantScope>,
        by: &str,
        reason: &str,
    ) -> Option<Grant> {
        let mut inner = self.inner.lock();
        let select = |grant: &Grant| {
            grant.grant_id == grant_id && scope.is_none_or(|scope| &grant.scope == scope)
        };
        let mut done = inner.transition(
            select,
            GrantState::Active,
            GrantState::Revoked,
            "revoked",
            by,
            reason,
            true,
        );
        if done.is_empty() {
            done = inner.transition(
                select,
                GrantState::Suspended,
                GrantState::Revoked,
                "revoked",
                by,
                reason,
                true,
            );
        }
        done.pop()
    }

    /// End every grant of a thread (archive or delete).
    pub fn revoke_thread(&self, thread_id: &str, by: &str, reason: &str) -> Vec<Grant> {
        let mut inner = self.inner.lock();
        let select = |grant: &Grant| matches!(&grant.scope, GrantScope::Thread { thread_id: id } if id == thread_id);
        let mut done = inner.transition(
            select,
            GrantState::Active,
            GrantState::Revoked,
            "revoked",
            by,
            reason,
            true,
        );
        done.extend(inner.transition(
            select,
            GrantState::Suspended,
            GrantState::Revoked,
            "revoked",
            by,
            reason,
            true,
        ));
        done
    }

    /// Reactivate a thread's grants suspended by a restart. Call only from an
    /// authenticated resume; rehydrating a wait never restores authority.
    pub fn resume_thread(&self, thread_id: &str) -> Vec<Grant> {
        self.inner.lock().transition(
            |grant| matches!(&grant.scope, GrantScope::Thread { thread_id: id } if id == thread_id),
            GrantState::Suspended,
            GrantState::Active,
            "resumed",
            "authenticated_resume",
            "thread resumed by an authenticated client",
            false,
        )
    }

    /// Live (active) grants of one thread, oldest first.
    #[must_use]
    pub fn thread_grants(&self, thread_id: &str) -> Vec<Grant> {
        let mut inner = self.inner.lock();
        inner.sweep(Utc::now());
        inner
            .grants
            .iter()
            .filter(|grant| {
                grant.state == GrantState::Active
                    && matches!(&grant.scope, GrantScope::Thread { thread_id: id } if id == thread_id)
            })
            .cloned()
            .collect()
    }
}

/// Stable id for a workspace: the git root (or the directory itself) as a
/// canonical real path, never the path string a client supplied.
#[must_use]
pub fn workspace_id(workspace: &Path) -> String {
    let real = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let root = real
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(&real);
    format!(
        "ws_{}",
        &hex_digest(root.to_string_lossy().as_bytes())[..24]
    )
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
