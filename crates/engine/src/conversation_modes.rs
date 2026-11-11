use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::extension_host::ExtensionHostOptions;

pub const STANDARD: &str = "standard";
pub const CREATOR: &str = "creator";
pub const SOURCE_BUILTIN: &str = "builtin";

pub const CREATOR_LOADS_ITS_OWN_TOOLS: bool = false;
pub const CREATOR_NOT_READY_REASON: &str = "Creator mode is not ready in this build.";

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

type Cached = Option<(Instant, Option<String>)>;

static CREATOR_PROBE: Mutex<Cached> = Mutex::new(None);

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

async fn creator_unavailable_reason(options: ExtensionHostOptions) -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(forced) = CREATOR_PROBE_OVERRIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return forced;
    }
    if !CREATOR_LOADS_ITS_OWN_TOOLS {
        return Some(CREATOR_NOT_READY_REASON.to_string());
    }
    if let Some((at, reason)) = CREATOR_PROBE.lock().expect("probe cache").clone()
        && at.elapsed() < AVAILABILITY_TTL
    {
        return reason;
    }
    let reason = tokio::task::spawn_blocking(move || probe_host_runtime(&options))
        .await
        .unwrap_or_else(|_| Some("Creator mode could not check for a plugin runtime.".to_string()));
    *CREATOR_PROBE.lock().expect("probe cache") = Some((Instant::now(), reason.clone()));
    reason
}

pub async fn current_table(options: ExtensionHostOptions) -> ConversationModeTable {
    table(creator_unavailable_reason(options).await)
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

pub async fn select(id: &str, options: ExtensionHostOptions) -> Result<ModeSelection, ModeRefusal> {
    let table = current_table(options).await;
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
