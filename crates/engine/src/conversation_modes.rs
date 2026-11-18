use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::extension_host::ExtensionHostOptions;

pub const STANDARD: &str = "standard";
pub const CREATOR: &str = "creator";
pub const SOURCE_BUILTIN: &str = "builtin";

pub const CREATOR_KEY_OFF_REASON: &str =
    "Plugin creator is off in [features] (plugin_creator = false).";

pub const CREATOR_TOOL_NAMES: [&str; 3] = [
    "plugin_manager",
    "cordis_inspect_list",
    "cordis_inspect_query",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loadout {
    pub always_load: &'static [&'static str],
    pub creator_skills: bool,
    pub first_turn_event: bool,
}

impl Loadout {
    pub const STANDARD: Self = Self {
        always_load: &[],
        creator_skills: false,
        first_turn_event: false,
    };
    pub const CREATOR: Self = Self {
        always_load: &CREATOR_TOOL_NAMES,
        creator_skills: true,
        first_turn_event: true,
    };
}

#[must_use]
pub fn loadout(stored: Option<&str>) -> Loadout {
    if effective_id(stored) == CREATOR {
        Loadout::CREATOR
    } else {
        Loadout::STANDARD
    }
}

#[must_use]
pub fn is_creator(stored: Option<&str>) -> bool {
    effective_id(stored) == CREATOR
}

const AVAILABILITY_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ModeRefusal {
    pub code: &'static str,
    pub message: String,
}

impl ModeRefusal {
    pub const UNKNOWN_MODE: &'static str = "unknown_mode";
    pub const MODE_UNAVAILABLE: &'static str = "mode_unavailable";
    pub const MODE_LOCKED: &'static str = "mode_locked";
    pub const MODE_NOT_SUPPORTED_WITH_EXECUTOR: &'static str = "mode_not_supported_with_executor";

    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn locked() -> Self {
        Self::new(
            Self::MODE_LOCKED,
            "The conversation mode is fixed once the conversation has its first turn; start a new conversation to use another mode.",
        )
    }

    pub fn with_executor() -> Self {
        Self::new(
            Self::MODE_NOT_SUPPORTED_WITH_EXECUTOR,
            "A session with another agent uses that agent's own tools and instructions; conversation modes are unavailable.",
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConversationMode {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: &'static str,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConversationModeTable {
    pub default: &'static str,
    pub modes: Vec<ConversationMode>,
}

#[must_use]
pub fn effective_id(stored: Option<&str>) -> &str {
    stored.unwrap_or(STANDARD)
}

#[must_use]
pub fn stored_form(id: &str) -> Option<String> {
    (id != STANDARD).then(|| id.to_string())
}

#[must_use]
pub fn table(creator_unavailable: Option<String>) -> ConversationModeTable {
    ConversationModeTable {
        default: STANDARD,
        modes: vec![
            ConversationMode {
                id: STANDARD.to_string(),
                name: "Standard".to_string(),
                description: "The everyday Codewhale agent with its normal tools and skills."
                    .to_string(),
                source: SOURCE_BUILTIN,
                available: true,
                reason: None,
            },
            ConversationMode {
                id: CREATOR.to_string(),
                name: "Creator".to_string(),
                description:
                    "Build, inspect and test Codewhale plugins in this conversation, with the plugin authoring tools and guides loaded."
                        .to_string(),
                source: SOURCE_BUILTIN,
                available: creator_unavailable.is_none(),
                reason: creator_unavailable,
            },
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CreatorProbeKey {
    runtime: &'static str,
    node_override: Option<PathBuf>,
    bun_override: Option<PathBuf>,
}

impl CreatorProbeKey {
    fn from_options(options: &ExtensionHostOptions) -> Self {
        Self {
            runtime: options.runtime.as_str(),
            node_override: options.node_override.clone(),
            bun_override: options.bun_override.clone(),
        }
    }
}

type ProbeCache = HashMap<CreatorProbeKey, (Instant, Option<String>)>;

static CREATOR_PROBE: Mutex<Option<ProbeCache>> = Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
static CREATOR_PROBE_OVERRIDE: Mutex<Option<Option<String>>> = Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
static CREATOR_PROBE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(any(test, feature = "test-support"))]
pub struct CreatorAvailabilityGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(any(test, feature = "test-support"))]
impl CreatorAvailabilityGuard {
    pub fn set(unavailable_reason: Option<&str>) -> Self {
        Self::hold(Some(unavailable_reason.map(str::to_string)))
    }

    pub fn unforced() -> Self {
        Self::hold(None)
    }

    fn hold(forced: Option<Option<String>>) -> Self {
        let serial = CREATOR_PROBE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *CREATOR_PROBE_OVERRIDE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = forced;
        Self { _serial: serial }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn clear_probe_cache_for_tests() {
    *CREATOR_PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for CreatorAvailabilityGuard {
    fn drop(&mut self) {
        *CREATOR_PROBE_OVERRIDE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

fn probe_host_runtime(options: &ExtensionHostOptions) -> Option<String> {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        options.runtime,
        options.node_override.as_deref(),
        options.bun_override.as_deref(),
        crate::extension_host::bundle_sha256(),
    );
    if resolution.selected.is_some() {
        None
    } else {
        Some(format!(
            "Creator mode needs a plugin runtime (Node.js or Bun), and none was found. {}",
            resolution.failure()
        ))
    }
}

pub async fn creator_unavailable_reason(
    options: ExtensionHostOptions,
    plugin_creator_enabled: bool,
) -> Option<String> {
    if !plugin_creator_enabled {
        return Some(CREATOR_KEY_OFF_REASON.to_string());
    }
    #[cfg(any(test, feature = "test-support"))]
    if let Some(forced) = CREATOR_PROBE_OVERRIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return forced;
    }
    let key = CreatorProbeKey::from_options(&options);
    if let Some((at, reason)) = CREATOR_PROBE
        .lock()
        .expect("probe cache")
        .as_ref()
        .and_then(|cache| cache.get(&key).cloned())
        && at.elapsed() < AVAILABILITY_TTL
    {
        return reason;
    }
    let reason = tokio::task::spawn_blocking(move || probe_host_runtime(&options))
        .await
        .unwrap_or_else(|_| Some("Creator mode could not check for a plugin runtime.".to_string()));
    CREATOR_PROBE
        .lock()
        .expect("probe cache")
        .get_or_insert_with(HashMap::new)
        .insert(key, (Instant::now(), reason.clone()));
    reason
}

pub async fn current_table(
    options: ExtensionHostOptions,
    plugin_creator_enabled: bool,
) -> ConversationModeTable {
    table(creator_unavailable_reason(options, plugin_creator_enabled).await)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeSelection {
    pub id: String,
    pub unavailable: Option<String>,
}

impl ModeSelection {
    pub fn require_available(&self) -> Result<(), ModeRefusal> {
        match &self.unavailable {
            Some(reason) => Err(ModeRefusal::new(
                ModeRefusal::MODE_UNAVAILABLE,
                reason.clone(),
            )),
            None => Ok(()),
        }
    }
}

pub async fn select(
    id: &str,
    options: ExtensionHostOptions,
    plugin_creator_enabled: bool,
) -> Result<ModeSelection, ModeRefusal> {
    let table = current_table(options, plugin_creator_enabled).await;
    match table.modes.into_iter().find(|mode| mode.id == id) {
        Some(mode) => Ok(ModeSelection {
            id: mode.id,
            unavailable: if mode.available {
                None
            } else {
                Some(
                    mode.reason
                        .unwrap_or_else(|| "This mode is unavailable.".to_string()),
                )
            },
        }),
        None => Err(ModeRefusal::new(
            ModeRefusal::UNKNOWN_MODE,
            format!("`{id}` is not a conversation mode; GET /v1/modes lists them."),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creator_unavailable_reasons_and_key_off_beats_probe_cache() {
        let _unforced = CreatorAvailabilityGuard::unforced();
        clear_probe_cache_for_tests();
        let options = ExtensionHostOptions::default();
        let key = CreatorProbeKey::from_options(&options);
        assert_eq!(
            creator_unavailable_reason(options.clone(), false).await,
            Some(CREATOR_KEY_OFF_REASON.to_string())
        );

        let missing_runtime = ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(PathBuf::from("/nonexistent/node-for-creator-test")),
            ..Default::default()
        };
        let reason = probe_host_runtime(&missing_runtime).expect("missing runtime reason");
        assert!(reason.contains("Node.js or Bun"), "{reason}");
        let uncached_missing = creator_unavailable_reason(missing_runtime.clone(), true)
            .await
            .expect("missing runtime reason");
        assert!(
            uncached_missing.contains("Node.js or Bun"),
            "{uncached_missing}"
        );

        {
            let mut cache = CREATOR_PROBE.lock().expect("probe cache");
            cache
                .get_or_insert_with(HashMap::new)
                .insert(key.clone(), (Instant::now(), None));
        }
        assert_eq!(
            creator_unavailable_reason(options.clone(), true).await,
            None
        );
        assert_eq!(
            creator_unavailable_reason(missing_runtime, true).await,
            Some(uncached_missing)
        );
        assert_eq!(
            creator_unavailable_reason(options.clone(), false).await,
            Some(CREATOR_KEY_OFF_REASON.to_string())
        );

        {
            let mut cache = CREATOR_PROBE.lock().expect("probe cache");
            cache
                .get_or_insert_with(HashMap::new)
                .insert(key, (Instant::now(), Some("stale probe".to_string())));
        }
        assert_eq!(
            creator_unavailable_reason(options.clone(), true).await,
            Some("stale probe".to_string())
        );
        assert_eq!(
            creator_unavailable_reason(options, false).await,
            Some(CREATOR_KEY_OFF_REASON.to_string())
        );
        clear_probe_cache_for_tests();
    }
}
