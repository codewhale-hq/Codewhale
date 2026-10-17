//! Shared command result data. Hosts consume actions after command dispatch;
//! constructing an outcome never executes an action.

use crate::facets::SessionSyncPayload;

#[derive(Debug, Clone, PartialEq)]
pub struct CommandResult<A> {
    pub message: Option<String>,
    pub action: Option<A>,
    pub is_error: bool,
}

impl<A> CommandResult<A> {
    pub fn ok() -> Self {
        Self {
            message: None,
            action: None,
            is_error: false,
        }
    }
    pub fn message(msg: impl Into<String>) -> Self {
        Self {
            message: Some(msg.into()),
            action: None,
            is_error: false,
        }
    }
    pub fn action(action: A) -> Self {
        Self {
            message: None,
            action: Some(action),
            is_error: false,
        }
    }
    pub fn with_message_and_action(msg: impl Into<String>, action: A) -> Self {
        Self {
            message: Some(msg.into()),
            action: Some(action),
            is_error: false,
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            message: Some(format!("Error: {}", msg.into())),
            action: None,
            is_error: true,
        }
    }
}

/// Complete debug-group action vocabulary. This replaces its TUI AppAction
/// references; other command groups' action ownership remains staged.
#[derive(Debug, Clone, PartialEq)]
pub enum DebugAction {
    FetchBalance,
    CacheWarmup,
    PreviewOutboundRequest {
        json: bool,
        base_prompt_only: bool,
        hypothetical_prompt: Option<String>,
    },
    OpenTextPager {
        title: String,
        content: String,
    },
    /// Show a unified diff in a pager, rendered as the transcript renders an
    /// edit, without copying it into transcript history.
    OpenDiffPager {
        title: String,
        diff: String,
    },
    OpenContextInspector,
    SendMessage(String),
    SyncSession(SessionSyncPayload),
    /// Install and persist a conversation rollback; `/retry` resends its
    /// input only after both succeed, replacing the removed exchange (#6788).
    ConversationUndo {
        sync: SessionSyncPayload,
        retry_input: Option<String>,
    },
}

pub type DebugCommandResult = CommandResult<DebugAction>;

/// Structural copy cannot request a host action.
pub type StructcopyCommandResult = CommandResult<std::convert::Infallible>;

/// Complete session-group vocabulary. Hosts execute these requests after dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    CompactContext { focus: Option<String> },
    PurgeContext,
    LoadSession(std::path::PathBuf),
    SyncSession(SessionSyncPayload),
    SendMessage(String),
    RemoteControl(SessionRemoteControlAction),
    OpenExternalUrl { url: String, label: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRemoteControlAction {
    Start,
    Stop,
}

pub type SessionCommandResult = CommandResult<SessionAction>;

/// Complete config-group action vocabulary (FEAT-028 D5). Unifies the FEAT-027
/// policy action with the auth, picker, view, screen-mode, trust and
/// host-effect actions of the main config slice. Constructing one never
/// executes it; the TUI host adapter converts it to a concrete `AppAction`
/// outside the movable closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    /// A permission rule was safely removed; the host reloads active rules.
    PermissionRulesChanged,
    // Auth flows (no host observation needed).
    StartXaiDeviceLogin,
    StartClaudeLogin,
    StartClaudeRevoke,
    StartChatgptPkceLogin,
    StartChatgptRevoke,
    StartOrcarouterPkceLogin,
    StartOrcarouterRevoke,
    // Views and pickers.
    OpenConfigView,
    OpenModePicker,
    OpenThemePicker,
    OpenStatusPicker,
    OpenProviderPicker,
    StartPluginLogin {
        provider: String,
    },
    StartPluginLogout {
        provider: String,
    },
    SetScreenMode(crate::types::CommandScreenMode),
    SetWorkspaceTrust {
        trusted: bool,
        save: bool,
    },
    // Engine/route effects carrying the exact host payload.
    ModeChanged(crate::types::CommandMode),
    ApprovalPolicyChanged {
        policy: Option<String>,
    },
    SwitchProvider {
        provider: crate::types::CommandProviderId,
        model: Option<String>,
    },
    CompactionChanged(crate::config::CommandCompactionConfig),
    StreamChunkTimeoutChanged(u64),
    SubagentRuntimeChanged(crate::config::CommandSubagentRuntimeConfig),
    SearchProviderChanged(crate::config::CommandSearchProvider),
    PromptSuggestionChanged {
        enabled: bool,
    },
    NotificationChanged(crate::config::CommandNotificationUpdate),
}
pub type ConfigCommandResult = CommandResult<ConfigAction>;
/// FEAT-027 compatibility alias; the whole group now standardizes on `ConfigAction`.
pub type ConfigPolicyAction = ConfigAction;
pub type ConfigPolicyCommandResult = ConfigCommandResult;
pub type ConfigStatusCommandResult = CommandResult<std::convert::Infallible>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreNavigationAction {
    OpenHotbarSetup,
    DisableHotbar,
    RestoreHotbarDefaults,
    OpenLiveTranscript,
    OpenTurnInspector,
    OpenWorktreeManager,
    SwitchWorkspace { workspace: std::path::PathBuf },
    VoiceCapture,
}

pub type CoreNavigationCommandResult = CommandResult<CoreNavigationAction>;
