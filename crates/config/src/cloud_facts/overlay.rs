//! One process-wide publication gate for verified facts, status and reader generation.
//! Fetchers capture a settings ticket before work; stale settings cannot publish or
//! write their cache after a newer disable or source change has been admitted.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

use super::provenance::{CloudFactsState, CloudFactsStatus};
use super::registry::{AdmittedRegistry, REGISTRY_SCHEMA};
use super::scope::ScopedFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayTicket {
    epoch: u64,
    source_identity: String,
}

#[derive(Debug, Clone, Default)]
pub struct OverlaySnapshot {
    pub generation: u64,
    pub fetched_at: Option<u64>,
    pub facts: Option<Arc<ScopedFacts>>,
    pub status: CloudFactsStatus,
}

#[derive(Default)]
struct State {
    epoch: u64,
    enabled: bool,
    source_identity: String,
    highest_seen: BTreeMap<String, u64>,
    snapshot: OverlaySnapshot,
    registry: Option<Arc<AdmittedRegistry>>,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(State::default()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSource {
    Compiled,
    CloudFacts { facts_version: u64 },
}

#[must_use]
pub fn hard_disabled() -> bool {
    std::env::var("CODEWHALE_DISABLE_CLOUD_FACTS").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn disable(state: &mut State) {
    if state.enabled
        || state.snapshot.facts.is_some()
        || state.registry.is_some()
        || state.snapshot.status.state != CloudFactsState::Off
    {
        state.epoch = state.epoch.saturating_add(1);
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
    state.enabled = false;
    state.snapshot.facts = None;
    state.snapshot.status = CloudFactsStatus::default();
    state.registry = None;
}

fn admits(state: &mut State, ticket: &OverlayTicket) -> bool {
    if hard_disabled() {
        disable(state);
    }
    state.enabled && ticket.epoch == state.epoch && ticket.source_identity == state.source_identity
}

/// Admit effective settings synchronously. Refreshes must use `current_ticket`
/// instead: an old settings object must never implicitly re-enable this layer.
pub fn configure(enabled: bool, source_identity: &str) -> Option<OverlayTicket> {
    let mut state = STATE.write().ok()?;
    if !enabled || hard_disabled() {
        disable(&mut state);
        return None;
    }
    if !state.enabled || state.source_identity != source_identity {
        state.epoch = state.epoch.saturating_add(1);
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
        state.snapshot.facts = None;
        state.registry = None;
        state.snapshot.status = CloudFactsStatus {
            state: CloudFactsState::BundledOnly,
            ..Default::default()
        };
        state.source_identity = source_identity.to_string();
        state.enabled = true;
    }
    Some(OverlayTicket {
        epoch: state.epoch,
        source_identity: state.source_identity.clone(),
    })
}

/// Accepted in-process rollback floors survive settings and trust changes.
#[must_use]
pub fn highest_seen(channel: &str) -> Option<u64> {
    STATE.read().ok()?.highest_seen.get(channel).copied()
}

#[must_use]
pub fn current_ticket(source_identity: &str) -> Option<OverlayTicket> {
    let mut state = STATE.write().ok()?;
    if hard_disabled() {
        disable(&mut state);
    }
    (state.enabled && state.source_identity == source_identity).then(|| OverlayTicket {
        epoch: state.epoch,
        source_identity: state.source_identity.clone(),
    })
}

#[must_use]
pub fn is_current(ticket: &OverlayTicket) -> bool {
    STATE
        .write()
        .is_ok_and(|mut state| admits(&mut state, ticket))
}

pub fn publish(
    ticket: &OverlayTicket,
    facts: Option<ScopedFacts>,
    status: CloudFactsStatus,
) -> bool {
    publish_with(ticket, facts, status, || {})
}

/// The callback may save/delete the bounded cache but must not reenter this
/// module. Its write and the state publication share the disable/source gate.
pub fn publish_with(
    ticket: &OverlayTicket,
    facts: Option<ScopedFacts>,
    status: CloudFactsStatus,
    before_publish: impl FnOnce(),
) -> bool {
    let Ok(mut state) = STATE.write() else {
        return false;
    };
    if !admits(&mut state, ticket) {
        return false;
    }
    if let Some(facts) = &facts {
        if state
            .highest_seen
            .get(&facts.channel)
            .is_some_and(|version| facts.facts_version < *version)
        {
            return false;
        }
        state
            .highest_seen
            .insert(facts.channel.clone(), facts.facts_version);
    }
    before_publish();
    state.snapshot.fetched_at = if facts.is_none() {
        None
    } else {
        match &status.state {
            CloudFactsState::Verified { fetched_at, .. } => Some(*fetched_at),
            _ => state.snapshot.fetched_at,
        }
    };
    state.snapshot.facts = facts.map(Arc::new);
    state.snapshot.status = status;
    state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    true
}

pub fn publish_registry(
    ticket: &OverlayTicket,
    registry: Option<Arc<AdmittedRegistry>>,
    before_publish: impl FnOnce(Option<u64>),
) -> bool {
    let Ok(mut state) = STATE.write() else {
        return false;
    };
    if !admits(&mut state, ticket) {
        return false;
    }
    if let Some(registry) = &registry {
        if state
            .highest_seen
            .get(REGISTRY_SCHEMA)
            .is_some_and(|revision| registry.revision < *revision)
        {
            return false;
        }
        state
            .highest_seen
            .insert(REGISTRY_SCHEMA.to_string(), registry.revision);
    }
    before_publish(state.highest_seen.get(REGISTRY_SCHEMA).copied());
    if state.registry.is_some() || registry.is_some() {
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
    state.registry = registry;
    true
}

#[must_use]
pub fn pulled_registry() -> Option<Arc<AdmittedRegistry>> {
    pulled_registry_at(crate::catalog::now_unix())
}

#[must_use]
pub fn pulled_registry_at(now: u64) -> Option<Arc<AdmittedRegistry>> {
    if let Ok(state) = STATE.read() {
        let Some(registry) = &state.registry else {
            return None;
        };
        if !hard_disabled() && !registry.is_expired_at(now) {
            return Some(Arc::clone(registry));
        }
    }
    let mut state = STATE.write().ok()?;
    if hard_disabled() {
        disable(&mut state);
        return None;
    }
    if state
        .registry
        .as_ref()
        .is_some_and(|registry| registry.is_expired_at(now))
    {
        state.registry = None;
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
    state.registry.clone()
}

fn snapshot_at(now: u64) -> OverlaySnapshot {
    let Ok(mut state) = STATE.write() else {
        return OverlaySnapshot::default();
    };
    if hard_disabled() {
        disable(&mut state);
    }
    if state
        .registry
        .as_ref()
        .is_some_and(|registry| registry.is_expired_at(now))
    {
        state.registry = None;
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
    }
    if state
        .snapshot
        .facts
        .as_ref()
        .is_some_and(|facts| !facts.is_current_at(now))
    {
        state.snapshot.facts = None;
        state.snapshot.generation = state.snapshot.generation.saturating_add(1);
        if let CloudFactsState::Verified { stale, .. } = &mut state.snapshot.status.state {
            *stale = true;
        }
    }
    state.snapshot.clone()
}

#[must_use]
pub fn snapshot() -> OverlaySnapshot {
    snapshot_at(crate::catalog::now_unix())
}

#[must_use]
pub fn overlay() -> Option<Arc<ScopedFacts>> {
    snapshot().facts
}

#[must_use]
pub fn status() -> CloudFactsStatus {
    snapshot().status
}

#[must_use]
pub fn is_verified() -> bool {
    overlay().is_some()
}

#[must_use]
pub fn cloud_default_model(provider: &str) -> Option<(String, DefaultSource)> {
    if let Some(overlay) = overlay()
        && let Some(model) = overlay
            .provider_defaults
            .get(provider)
            .and_then(|d| d.default_model.clone())
    {
        return Some((
            model,
            DefaultSource::CloudFacts {
                facts_version: overlay.facts_version,
            },
        ));
    }
    let reg = pulled_registry()?;
    let model = reg.compiled.provider_defaults.get(provider)?.clone();
    Some((
        model,
        DefaultSource::CloudFacts {
            facts_version: reg.revision,
        },
    ))
}

/// Cloud defaults are a floor for official hosted endpoints, never an override
/// for named compatible routes, local machines, or a Codex account roster.
#[must_use]
pub fn cloud_default_model_for_route(
    provider: crate::ProviderKind,
    base_url: &str,
) -> Option<(String, DefaultSource)> {
    if matches!(
        provider,
        crate::ProviderKind::Custom
            | crate::ProviderKind::Ollama
            | crate::ProviderKind::Sglang
            | crate::ProviderKind::Vllm
            | crate::ProviderKind::OpenaiCodex
    ) || !super::scope::base_url_allowed(provider.as_str(), base_url)
    {
        return None;
    }
    cloud_default_model(provider.as_str())
}

#[must_use]
pub fn cloud_default_base_url(provider: &str) -> Option<(String, DefaultSource)> {
    let overlay = overlay()?;
    let url = overlay.provider_defaults.get(provider)?.base_url.clone()?;
    Some((
        url,
        DefaultSource::CloudFacts {
            facts_version: overlay.facts_version,
        },
    ))
}

pub fn clear() {
    if let Ok(mut state) = STATE.write() {
        disable(&mut state);
    }
}
