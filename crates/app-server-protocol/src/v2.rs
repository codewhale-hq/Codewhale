#![allow(clippy::large_enum_variant, clippy::enum_variant_names)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub use crate::methods::{
    ClientNotificationMethod, ClientRequestMethod, ServerNotificationMethod, ServerRequestMethod,
};

pub type AbsolutePathBuf = String;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdditionalFileSystemPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FileSystemSandboxEntry>>,
    #[serde(rename = "globScanMaxDepth", default, skip_serializing_if = "Option::is_none")]
    pub glob_scan_max_depth: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<LegacyAppPathString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<LegacyAppPathString>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AdditionalNetworkPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AgentMessageDelivery {
    #[serde(rename = "async")]
    Async,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AgentMessageDeltaNotification {
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

pub type AgentPath = String;

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AllowDenyRequirement {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalyticsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ApprovalsReviewer {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "auto_review")]
    AutoReview,
    #[serde(rename = "guardian_subagent")]
    GuardianSubagent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AskForApproval {
    #[serde(rename = "untrusted")]
    Untrusted,
    #[serde(rename = "on-request")]
    OnRequest,
    #[serde(rename = "never")]
    Never,
    #[serde(rename = "granular")]
    Granular {
        mcp_elicitations: bool,
        #[serde(default)]
        request_permissions: bool,
        rules: bool,
        sandbox_approval: bool,
        #[serde(default)]
        skill_approval: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AsyncUserInputQuestion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AutoCompactTokenLimitScope {
    #[serde(rename = "total")]
    Total,
    #[serde(rename = "body_after_prefix")]
    BodyAfterPrefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BrowserUseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_history_access: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_origin_policy: Option<BrowserUseOriginPolicyConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origins: Option<BTreeMap<String, BrowserUseOriginPolicyConfig>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BrowserUseOriginPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AllowDenyRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<AllowDenyRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_cdp_access: Option<AllowDenyRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploads: Option<AllowDenyRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ByteRange {
    pub end: u64,
    pub start: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapabilityRefusal {
    pub code: CapabilityRefusalCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CapabilityRefusalCode {
    #[serde(rename = "unknown_agent")]
    UnknownAgent,
    #[serde(rename = "not_installed")]
    NotInstalled,
    #[serde(rename = "adapter_missing")]
    AdapterMissing,
    #[serde(rename = "below_min_version")]
    BelowMinVersion,
    #[serde(rename = "needs_sign_in")]
    NeedsSignIn,
    #[serde(rename = "no_acp")]
    NoAcp,
    #[serde(rename = "executor_not_supported")]
    ExecutorNotSupported,
    #[serde(rename = "environment_not_supported")]
    EnvironmentNotSupported,
    #[serde(rename = "steer_not_supported")]
    SteerNotSupported,
    #[serde(rename = "reopen_not_supported")]
    ReopenNotSupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CapabilityRefusalData {
    #[serde(rename = "codewhaleRefusal")]
    pub codewhale_refusal: CapabilityRefusal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClientInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum CodexErrorInfo {
    CodexErrorInfoVariant0(CodexErrorInfoVariant0),
    HttpConnectionFailed(CodexErrorInfoVariant1),
    ResponseStreamConnectionFailed(CodexErrorInfoVariant2),
    ResponseStreamDisconnected(CodexErrorInfoVariant3),
    ResponseTooManyFailedAttempts(CodexErrorInfoVariant4),
    ActiveTurnNotSteerable(CodexErrorInfoVariant5),
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CodexErrorInfoVariant0 {
    #[serde(rename = "contextWindowExceeded")]
    ContextWindowExceeded,
    #[serde(rename = "sessionBudgetExceeded")]
    SessionBudgetExceeded,
    #[serde(rename = "usageLimitExceeded")]
    UsageLimitExceeded,
    #[serde(rename = "rateLimitExceeded")]
    RateLimitExceeded,
    #[serde(rename = "flexUnavailable")]
    FlexUnavailable,
    #[serde(rename = "serverOverloaded")]
    ServerOverloaded,
    #[serde(rename = "modelNotFound")]
    ModelNotFound,
    #[serde(rename = "policyCapacityUnavailable")]
    PolicyCapacityUnavailable,
    #[serde(rename = "cyberPolicy")]
    CyberPolicy,
    #[serde(rename = "misalignmentPolicyViolation")]
    MisalignmentPolicyViolation,
    #[serde(rename = "tooManyDenials")]
    TooManyDenials,
    #[serde(rename = "internalServerError")]
    InternalServerError,
    #[serde(rename = "unauthorized")]
    Unauthorized,
    #[serde(rename = "badRequest")]
    BadRequest,
    #[serde(rename = "threadRollbackFailed")]
    ThreadRollbackFailed,
    #[serde(rename = "sandboxError")]
    SandboxError,
    #[serde(rename = "other")]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CodexErrorInfoVariant1 {
    #[serde(rename = "httpConnectionFailed")]
    pub http_connection_failed: CodexErrorInfoVariant1HttpConnectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CodexErrorInfoVariant1HttpConnectionFailed {
    #[serde(rename = "httpStatusCode", default, skip_serializing_if = "Option::is_none")]
    pub http_status_code: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CodexErrorInfoVariant2 {
    #[serde(rename = "responseStreamConnectionFailed")]
    pub response_stream_connection_failed: CodexErrorInfoVariant2ResponseStreamConnectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CodexErrorInfoVariant2ResponseStreamConnectionFailed {
    #[serde(rename = "httpStatusCode", default, skip_serializing_if = "Option::is_none")]
    pub http_status_code: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CodexErrorInfoVariant3 {
    #[serde(rename = "responseStreamDisconnected")]
    pub response_stream_disconnected: CodexErrorInfoVariant3ResponseStreamDisconnected,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CodexErrorInfoVariant3ResponseStreamDisconnected {
    #[serde(rename = "httpStatusCode", default, skip_serializing_if = "Option::is_none")]
    pub http_status_code: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CodexErrorInfoVariant4 {
    #[serde(rename = "responseTooManyFailedAttempts")]
    pub response_too_many_failed_attempts: CodexErrorInfoVariant4ResponseTooManyFailedAttempts,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CodexErrorInfoVariant4ResponseTooManyFailedAttempts {
    #[serde(rename = "httpStatusCode", default, skip_serializing_if = "Option::is_none")]
    pub http_status_code: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CodexErrorInfoVariant5 {
    #[serde(rename = "activeTurnNotSteerable")]
    pub active_turn_not_steerable: CodexErrorInfoVariant5ActiveTurnNotSteerable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CodexErrorInfoVariant5ActiveTurnNotSteerable {
    #[serde(rename = "turnKind")]
    pub turn_kind: NonSteerableTurnKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CollabAgentState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub status: CollabAgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CollabAgentStatus {
    #[serde(rename = "pendingInit")]
    PendingInit,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "errored")]
    Errored,
    #[serde(rename = "shutdown")]
    Shutdown,
    #[serde(rename = "notFound")]
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CollabAgentTool {
    #[serde(rename = "spawnAgent")]
    SpawnAgent,
    #[serde(rename = "sendInput")]
    SendInput,
    #[serde(rename = "resumeAgent")]
    ResumeAgent,
    #[serde(rename = "wait")]
    Wait,
    #[serde(rename = "closeAgent")]
    CloseAgent,
    #[serde(rename = "sendMessage")]
    SendMessage,
    #[serde(rename = "followupTask")]
    FollowupTask,
    #[serde(rename = "interruptAgent")]
    InterruptAgent,
    #[serde(rename = "listAgents")]
    ListAgents,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CollabAgentToolCallStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "interrupted")]
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CollaborationMode {
    pub mode: ModeKind,
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum CommandAction {
    #[serde(rename = "read")]
    Read {
        command: String,
        name: String,
        path: LegacyAppPathString,
    },
    #[serde(rename = "listFiles")]
    ListFiles {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    #[serde(rename = "search")]
    Search {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    #[serde(rename = "unknown")]
    Unknown {
        command: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecOutputDeltaNotification {
    #[serde(rename = "capReached")]
    pub cap_reached: bool,
    #[serde(rename = "deltaBase64")]
    pub delta_base64: String,
    #[serde(rename = "processId")]
    pub process_id: String,
    pub stream: CommandExecOutputStream,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandExecOutputStream {
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "stderr")]
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecParams {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "disableOutputCap", default, skip_serializing_if = "Option::is_none")]
    pub disable_output_cap: Option<bool>,
    #[serde(rename = "disableTimeout", default, skip_serializing_if = "Option::is_none")]
    pub disable_timeout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, Option<String>>>,
    #[serde(rename = "outputBytesCap", default, skip_serializing_if = "Option::is_none")]
    pub output_bytes_cap: Option<u64>,
    #[serde(rename = "processId", default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(rename = "sandboxPolicy", default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<CommandExecTerminalSize>,
    #[serde(rename = "streamStdin", default, skip_serializing_if = "Option::is_none")]
    pub stream_stdin: Option<bool>,
    #[serde(rename = "streamStdoutStderr", default, skip_serializing_if = "Option::is_none")]
    pub stream_stdout_stderr: Option<bool>,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecResizeParams {
    #[serde(rename = "processId")]
    pub process_id: String,
    pub size: CommandExecTerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecResizeResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecResponse {
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
    pub stderr: String,
    pub stdout: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecTerminalSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecTerminateParams {
    #[serde(rename = "processId")]
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecTerminateResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecWriteParams {
    #[serde(rename = "closeStdin", default, skip_serializing_if = "Option::is_none")]
    pub close_stdin: Option<bool>,
    #[serde(rename = "deltaBase64", default, skip_serializing_if = "Option::is_none")]
    pub delta_base64: Option<String>,
    #[serde(rename = "processId")]
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecWriteResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandExecutionApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "acceptWithExecpolicyAmendment")]
    AcceptWithExecpolicyAmendment {
        execpolicy_amendment: Vec<String>,
    },
    #[serde(rename = "applyNetworkPolicyAmendment")]
    ApplyNetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandExecutionApprovalKind {
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "writeStdin")]
    WriteStdin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecutionOutputDeltaNotification {
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecutionRequestApprovalParams {
    #[serde(rename = "approvalId", default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(rename = "commandActions", default, skip_serializing_if = "Option::is_none")]
    pub command_actions: Option<Vec<CommandAction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<LegacyAppPathString>,
    #[serde(rename = "environmentId", default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(default = "default_command_execution_request_approval_params_kind")]
    pub kind: CommandExecutionApprovalKind,
    #[serde(rename = "networkApprovalContext", default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    #[serde(rename = "proposedExecpolicyAmendment", default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<Vec<String>>,
    #[serde(rename = "proposedNetworkPolicyAmendments", default, skip_serializing_if = "Option::is_none")]
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommandExecutionRequestApprovalResponse {
    pub decision: CommandExecutionApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandExecutionSource {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "userShell")]
    UserShell,
    #[serde(rename = "unifiedExecStartup")]
    UnifiedExecStartup,
    #[serde(rename = "unifiedExecInteraction")]
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandExecutionStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "declined")]
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComputerUseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_app_access: Option<AllowDenyRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos: Option<ComputerUseMacosConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<ComputerUseWindowsConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComputerUseMacosConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_ids: Option<BTreeMap<String, AllowDenyRequirement>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComputerUseWindowsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aumids: Option<BTreeMap<String, AllowDenyRequirement>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exes: Option<Vec<ComputerUseWindowsExeConfig>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ComputerUseWindowsExeConfig {
    pub access: AllowDenyRequirement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_name: Option<String>,
    pub product_name: String,
    pub publisher_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics: Option<AnalyticsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_use: Option<BrowserUseConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<ComputerUseConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_chatgpt_workspace_id: Option<ForcedChatgptWorkspaceIds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_login_method: Option<ForcedLoginMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_auto_compact_token_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_auto_compact_token_limit_scope: Option<AutoCompactTokenLimitScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_reasoning_summary: Option<ReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_verbosity: Option<Verbosity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<SandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_workspace_write: Option<SandboxWorkspaceWrite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchMode>,
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigBatchWriteParams {
    pub edits: Vec<ConfigEdit>,
    #[serde(rename = "expectedVersion", default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(rename = "filePath", default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(rename = "reloadUserConfig", default, skip_serializing_if = "Option::is_none")]
    pub reload_user_config: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigEdit {
    #[serde(rename = "keyPath")]
    pub key_path: String,
    #[serde(rename = "mergeStrategy")]
    pub merge_strategy: MergeStrategy,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigLayer {
    pub config: Value,
    #[serde(rename = "disabledReason", default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub name: ConfigLayerSource,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigLayerMetadata {
    pub name: ConfigLayerSource,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ConfigLayerSource {
    #[serde(rename = "packagedDefaults")]
    PackagedDefaults {
        file: AbsolutePathBuf,
    },
    #[serde(rename = "mdm")]
    Mdm {
        domain: String,
        key: String,
    },
    #[serde(rename = "system")]
    System {
        file: AbsolutePathBuf,
    },
    #[serde(rename = "enterpriseManaged")]
    EnterpriseManaged {
        id: String,
        name: String,
    },
    #[serde(rename = "user")]
    User {
        file: AbsolutePathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    #[serde(rename = "project")]
    Project {
        #[serde(rename = "dotCodexFolder")]
        dot_codex_folder: AbsolutePathBuf,
    },
    #[serde(rename = "sessionFlags")]
    SessionFlags {},
    #[serde(rename = "legacyManagedConfigTomlFromFile")]
    LegacyManagedConfigTomlFromFile {
        file: AbsolutePathBuf,
    },
    #[serde(rename = "legacyManagedConfigTomlFromMdm")]
    LegacyManagedConfigTomlFromMdm {},
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigReadParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "includeLayers", default, skip_serializing_if = "Option::is_none")]
    pub include_layers: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigReadResponse {
    pub config: Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<ConfigLayer>>,
    pub origins: BTreeMap<String, ConfigLayerMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigValueWriteParams {
    #[serde(rename = "expectedVersion", default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    #[serde(rename = "filePath", default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(rename = "keyPath")]
    pub key_path: String,
    #[serde(rename = "mergeStrategy")]
    pub merge_strategy: MergeStrategy,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigWriteResponse {
    #[serde(rename = "filePath")]
    pub file_path: AbsolutePathBuf,
    #[serde(rename = "overriddenMetadata", default, skip_serializing_if = "Option::is_none")]
    pub overridden_metadata: Option<OverriddenMetadata>,
    pub status: WriteStatus,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextCompactedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CyberAccessProgram {
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "daybreakBlue")]
    DaybreakBlue,
    #[serde(rename = "daybreakRed")]
    DaybreakRed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum DynamicToolCallOutputContentItem {
    #[serde(rename = "inputText")]
    InputText {
        text: String,
    },
    #[serde(rename = "inputImage")]
    InputImage {
        #[serde(rename = "imageUrl")]
        image_url: String,
    },
    #[serde(rename = "inputAudio")]
    InputAudio {
        #[serde(rename = "audioUrl")]
        audio_url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DynamicToolCallParams {
    pub arguments: Value,
    #[serde(rename = "callId")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub tool: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DynamicToolCallResponse {
    #[serde(rename = "contentItems")]
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DynamicToolCallStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ErrorNotification {
    pub error: TurnError,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "willRetry")]
    pub will_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FileChangeApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileChangeOutputDeltaNotification {
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileChangePatchUpdatedNotification {
    pub changes: Vec<FileUpdateChange>,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileChangeRequestApprovalParams {
    #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none")]
    pub grant_root: Option<String>,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileChangeRequestApprovalResponse {
    pub decision: FileChangeApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FileSystemAccessMode {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    #[serde(rename = "deny")]
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum FileSystemPath {
    #[serde(rename = "path")]
    Path {
        path: LegacyAppPathString,
    },
    #[serde(rename = "glob_pattern")]
    GlobPattern {
        pattern: String,
    },
    #[serde(rename = "special")]
    Special {
        value: FileSystemSpecialPath,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileSystemSandboxEntry {
    pub access: FileSystemAccessMode,
    pub path: FileSystemPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind")]
pub enum FileSystemSpecialPath {
    #[serde(rename = "root")]
    Root {},
    #[serde(rename = "minimal")]
    Minimal {},
    #[serde(rename = "project_roots")]
    ProjectRoots {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<LegacyAppPathString>,
    },
    #[serde(rename = "tmpdir")]
    Tmpdir {},
    #[serde(rename = "slash_tmp")]
    SlashTmp {},
    #[serde(rename = "unknown")]
    Unknown {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<LegacyAppPathString>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FileUpdateChange {
    pub diff: String,
    pub kind: PatchChangeKind,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ForcedChatgptWorkspaceIds {
    String(String),
    Strings(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ForcedLoginMethod {
    #[serde(rename = "chatgpt")]
    Chatgpt,
    #[serde(rename = "api")]
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsChangedNotification {
    #[serde(rename = "changedPaths")]
    pub changed_paths: Vec<AbsolutePathBuf>,
    #[serde(rename = "watchId")]
    pub watch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsCopyParams {
    #[serde(rename = "destinationPath")]
    pub destination_path: AbsolutePathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
    #[serde(rename = "sourcePath")]
    pub source_path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsCopyResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsCreateDirectoryParams {
    pub path: AbsolutePathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsCreateDirectoryResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsGetMetadataParams {
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsGetMetadataResponse {
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isFile")]
    pub is_file: bool,
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    #[serde(rename = "modifiedAtMs")]
    pub modified_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsReadDirectoryEntry {
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isFile")]
    pub is_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsReadDirectoryParams {
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsReadDirectoryResponse {
    pub entries: Vec<FsReadDirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsReadFileParams {
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsReadFileResponse {
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsRemoveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
    pub path: AbsolutePathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsRemoveResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsUnwatchParams {
    #[serde(rename = "watchId")]
    pub watch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsUnwatchResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsWatchParams {
    pub path: AbsolutePathBuf,
    #[serde(rename = "watchId")]
    pub watch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsWatchResponse {
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsWriteFileParams {
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FsWriteFileResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum FunctionCallOutputBody {
    String(String),
    FunctionCallOutputContentItemList(Vec<FunctionCallOutputContentItem>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum FunctionCallOutputContentItem {
    #[serde(rename = "input_text")]
    InputText {
        text: String,
    },
    #[serde(rename = "input_image")]
    InputImage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
        #[serde(flatten)]
        kind: FunctionCallOutputContentItemInputImageKind,
    },
    #[serde(rename = "input_audio")]
    InputAudio {
        audio_url: String,
    },
    #[serde(rename = "encrypted_content")]
    EncryptedContent {
        encrypted_content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FunctionCallOutputContentItemInputImageKind {
    #[serde(rename = "image_url")]
    ImageUrl(String),
    #[serde(rename = "file_id")]
    FileId(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GitInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(rename = "originUrl", default, skip_serializing_if = "Option::is_none")]
    pub origin_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GrantedPermissionProfile {
    #[serde(rename = "fileSystem", default, skip_serializing_if = "Option::is_none")]
    pub file_system: Option<AdditionalFileSystemPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<AdditionalNetworkPermissions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HookErrorInfo {
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HookEventName {
    #[serde(rename = "preModelRequest")]
    PreModelRequest,
    #[serde(rename = "postModelRequest")]
    PostModelRequest,
    #[serde(rename = "preToolUse")]
    PreToolUse,
    #[serde(rename = "permissionRequest")]
    PermissionRequest,
    #[serde(rename = "postToolUse")]
    PostToolUse,
    #[serde(rename = "preCompact")]
    PreCompact,
    #[serde(rename = "postCompact")]
    PostCompact,
    #[serde(rename = "sessionStart")]
    SessionStart,
    #[serde(rename = "sessionEnd")]
    SessionEnd,
    #[serde(rename = "userPromptSubmit")]
    UserPromptSubmit,
    #[serde(rename = "subagentStart")]
    SubagentStart,
    #[serde(rename = "subagentStop")]
    SubagentStop,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "interrupt")]
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HookMetadata {
    #[serde(rename = "additionalContextLimit", default, skip_serializing_if = "Option::is_none")]
    pub additional_context_limit: Option<u64>,
    #[serde(rename = "currentHash")]
    pub current_hash: String,
    #[serde(rename = "displayOrder")]
    pub display_order: i64,
    pub enabled: bool,
    #[serde(rename = "environmentId", default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(rename = "eventName")]
    pub event_name: HookEventName,
    #[serde(rename = "isManaged")]
    pub is_managed: bool,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub source: HookSource,
    #[serde(rename = "sourcePath")]
    pub source_path: AbsolutePathBuf,
    #[serde(rename = "statusMessage", default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(rename = "timeoutSec")]
    pub timeout_sec: u64,
    #[serde(rename = "trustStatus")]
    pub trust_status: HookTrustStatus,
    #[serde(flatten)]
    pub kind: HookMetadataKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "handlerType")]
pub enum HookMetadataKind {
    #[serde(rename = "command")]
    Command {
        #[serde(default)]
        r#async: bool,
        command: String,
    },
    #[serde(rename = "mcpTool")]
    McpTool {
        server: String,
        tool: String,
    },
    #[serde(rename = "prompt")]
    Prompt {},
    #[serde(rename = "agent")]
    Agent {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HookPromptFragment {
    #[serde(rename = "hookRunId")]
    pub hook_run_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HookSource {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "project")]
    Project,
    #[serde(rename = "mdm")]
    Mdm,
    #[serde(rename = "sessionFlags")]
    SessionFlags,
    #[serde(rename = "plugin")]
    Plugin,
    #[serde(rename = "cloudRequirements")]
    CloudRequirements,
    #[serde(rename = "cloudManagedConfig")]
    CloudManagedConfig,
    #[serde(rename = "legacyManagedConfigFile")]
    LegacyManagedConfigFile,
    #[serde(rename = "legacyManagedConfigMdm")]
    LegacyManagedConfigMdm,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HookTrustStatus {
    #[serde(rename = "managed")]
    Managed,
    #[serde(rename = "untrusted")]
    Untrusted,
    #[serde(rename = "trusted")]
    Trusted,
    #[serde(rename = "modified")]
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HooksListEntry {
    pub cwd: String,
    pub errors: Vec<HookErrorInfo>,
    pub hooks: Vec<HookMetadata>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HooksListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HooksListResponse {
    pub data: Vec<HooksListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ImageDetail {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "original")]
    Original,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ImageGenerationFailure {
    #[serde(rename = "usageLimitExceeded")]
    UsageLimitExceeded {
        #[serde(rename = "limitId")]
        limit_id: String,
        #[serde(rename = "resetsAt", default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ImageGenerationInputImage {
    pub path: AbsolutePathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InitializeCapabilities {
    #[serde(rename = "experimentalApi", default)]
    pub experimental_api: bool,
    #[serde(rename = "explicitGatewayOauth", default, skip_serializing_if = "Option::is_none")]
    pub explicit_gateway_oauth: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Map<String, Value>>,
    #[serde(rename = "mcpServerOpenaiFormElicitation", default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_openai_form_elicitation: Option<bool>,
    #[serde(rename = "optOutNotificationMethods", default, skip_serializing_if = "Option::is_none")]
    pub opt_out_notification_methods: Option<Vec<String>>,
    #[serde(rename = "requestAttestation", default)]
    pub request_attestation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InitializeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<InitializeCapabilities>,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InitializeResponse {
    #[serde(rename = "codexHome")]
    pub codex_home: AbsolutePathBuf,
    #[serde(rename = "platformFamily")]
    pub platform_family: String,
    #[serde(rename = "platformOs")]
    pub platform_os: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InputModality {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "image")]
    Image,
    #[serde(rename = "audio")]
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemCompletedNotification {
    #[serde(rename = "completedAtMs")]
    pub completed_at_ms: i64,
    pub item: ThreadItem,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ItemStartedNotification {
    pub item: ThreadItem,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

pub type LegacyAppPathString = String;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ListMcpServerStatusParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<McpServerStatusDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(rename = "serverName", default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ListMcpServerStatusResponse {
    pub data: Vec<McpServerStatus>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum McpAppDisplayMode {
    #[serde(rename = "inline")]
    Inline,
    #[serde(rename = "fullscreen")]
    Fullscreen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpAppUi {
    #[serde(rename = "preferredModelDisplayMode")]
    pub preferred_model_display_mode: McpAppDisplayMode,
    #[serde(rename = "resourceUri")]
    pub resource_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum McpAuthStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "unsupported")]
    Unsupported,
    #[serde(rename = "notLoggedIn")]
    NotLoggedIn,
    #[serde(rename = "bearerToken")]
    BearerToken,
    #[serde(rename = "oAuth")]
    OAuth,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum McpServerConnectionStatus {
    #[serde(rename = "notStarted")]
    NotStarted,
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "connected")]
    Connected,
    #[serde(rename = "authenticationRequired")]
    AuthenticationRequired,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "disabled")]
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpServerInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Value>>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
    #[serde(rename = "websiteUrl", default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpServerStatus {
    #[serde(rename = "authStatus")]
    pub auth_status: McpAuthStatus,
    #[serde(rename = "httpOrigin", default, skip_serializing_if = "Option::is_none")]
    pub http_origin: Option<String>,
    pub name: String,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(rename = "resourceTemplates")]
    pub resource_templates: Vec<ResourceTemplate>,
    pub resources: Vec<Resource>,
    #[serde(rename = "runtimeStatus", default, skip_serializing_if = "Option::is_none")]
    pub runtime_status: Option<McpServerConnectionStatus>,
    #[serde(rename = "serverCapabilities", default, skip_serializing_if = "Option::is_none")]
    pub server_capabilities: Option<Value>,
    #[serde(rename = "serverInfo", default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<McpServerInfo>,
    #[serde(rename = "toolListMeta", default, skip_serializing_if = "Option::is_none")]
    pub tool_list_meta: Option<Value>,
    pub tools: BTreeMap<String, Tool>,
    #[serde(rename = "toolsError", default, skip_serializing_if = "Option::is_none")]
    pub tools_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum McpServerStatusDetail {
    #[serde(rename = "full")]
    Full,
    #[serde(rename = "toolsAndAuthOnly")]
    ToolsAndAuthOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpToolCallAppContext {
    #[serde(rename = "actionName", default, skip_serializing_if = "Option::is_none")]
    pub action_name: Option<String>,
    #[serde(rename = "appName", default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(rename = "connectorId")]
    pub connector_id: String,
    #[serde(rename = "linkId", default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<String>,
    #[serde(rename = "resourceUri", default, skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpToolCallError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpToolCallProgressNotification {
    #[serde(rename = "itemId")]
    pub item_id: String,
    pub message: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct McpToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Value>,
    pub content: Vec<Value>,
    #[serde(rename = "structuredContent", default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum McpToolCallStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MemoryCitation {
    pub entries: Vec<MemoryCitationEntry>,
    #[serde(rename = "threadIds")]
    pub thread_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MemoryCitationEntry {
    #[serde(rename = "lineEnd")]
    pub line_end: u32,
    #[serde(rename = "lineStart")]
    pub line_start: u32,
    pub note: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MergeStrategy {
    #[serde(rename = "replace")]
    Replace,
    #[serde(rename = "upsert")]
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MessagePhase {
    #[serde(rename = "commentary")]
    Commentary,
    #[serde(rename = "partial_answer")]
    PartialAnswer,
    #[serde(rename = "final_answer")]
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MisalignmentErrorDetails {
    #[serde(rename = "detailedExplanation", default, skip_serializing_if = "Option::is_none")]
    pub detailed_explanation: Option<String>,
    #[serde(rename = "errorType", default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(rename = "reviewTarget", default, skip_serializing_if = "Option::is_none")]
    pub review_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<MisalignmentSteer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MisalignmentSteer {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ModeKind {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "default")]
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Model {
    #[serde(rename = "additionalSpeedTiers", default)]
    pub additional_speed_tiers: Vec<String>,
    #[serde(rename = "availabilityNux", default, skip_serializing_if = "Option::is_none")]
    pub availability_nux: Option<ModelAvailabilityNux>,
    #[serde(rename = "availableAccessPrograms", default, skip_serializing_if = "Option::is_none")]
    pub available_access_programs: Option<ModelAccessPrograms>,
    #[serde(rename = "defaultReasoningEffort")]
    pub default_reasoning_effort: ReasoningEffort,
    #[serde(rename = "defaultServiceTier", default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
    pub description: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub hidden: bool,
    pub id: String,
    #[serde(rename = "inputModalities", default = "default_model_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
    pub model: String,
    #[serde(rename = "modelSpecialty", default, skip_serializing_if = "Option::is_none")]
    pub model_specialty: Option<String>,
    #[serde(rename = "multiAgentVersion", default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    #[serde(rename = "serviceTiers", default)]
    pub service_tiers: Vec<ModelServiceTier>,
    #[serde(rename = "supportedReasoningEfforts")]
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(rename = "supportsPersonality", default)]
    pub supports_personality: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade: Option<String>,
    #[serde(rename = "upgradeInfo", default, skip_serializing_if = "Option::is_none")]
    pub upgrade_info: Option<ModelUpgradeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelAccessPrograms {
    pub cyber: Vec<CyberAccessProgram>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelAvailabilityNux {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(rename = "includeHidden", default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelServiceTier {
    pub description: String,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModelUpgradeInfo {
    #[serde(rename = "migrationMarkdown", default, skip_serializing_if = "Option::is_none")]
    pub migration_markdown: Option<String>,
    pub model: String,
    #[serde(rename = "modelLink", default, skip_serializing_if = "Option::is_none")]
    pub model_link: Option<String>,
    #[serde(rename = "retirementAt", default, skip_serializing_if = "Option::is_none")]
    pub retirement_at: Option<i64>,
    #[serde(rename = "upgradeCopy", default, skip_serializing_if = "Option::is_none")]
    pub upgrade_copy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MultiAgentVersion {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "v1")]
    V1,
    #[serde(rename = "v2")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NetworkAccess {
    #[serde(rename = "restricted")]
    Restricted,
    #[serde(rename = "enabled")]
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NetworkApprovalProtocol {
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "https")]
    Https,
    #[serde(rename = "socks5Tcp")]
    Socks5Tcp,
    #[serde(rename = "socks5Udp")]
    Socks5Udp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NetworkPolicyAmendment {
    pub action: NetworkPolicyRuleAction,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NetworkPolicyRuleAction {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NonSteerableTurnKind {
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "compact")]
    Compact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OverriddenMetadata {
    #[serde(rename = "effectiveValue")]
    pub effective_value: Value,
    pub message: String,
    #[serde(rename = "overridingLayer")]
    pub overriding_layer: ConfigLayerMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PatchApplyStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "declined")]
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum PatchChangeKind {
    #[serde(rename = "add")]
    Add {},
    #[serde(rename = "delete")]
    Delete {},
    #[serde(rename = "update")]
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        move_path: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PermissionGrantScope {
    #[serde(rename = "turn")]
    Turn,
    #[serde(rename = "session")]
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PermissionsRequestApprovalParams {
    pub cwd: LegacyAppPathString,
    #[serde(rename = "environmentId", default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(rename = "itemId")]
    pub item_id: String,
    pub permissions: RequestPermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PermissionsRequestApprovalResponse {
    pub permissions: GrantedPermissionProfile,
    #[serde(default = "default_permissions_request_approval_response_scope")]
    pub scope: PermissionGrantScope,
    #[serde(rename = "strictAutoReview", default, skip_serializing_if = "Option::is_none")]
    pub strict_auto_review: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Personality {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "friendly")]
    Friendly,
    #[serde(rename = "pragmatic")]
    Pragmatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanDeltaNotification {
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

pub type ReasoningEffort = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningEffortOption {
    pub description: String,
    #[serde(rename = "reasoningEffort")]
    pub reasoning_effort: ReasoningEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReasoningSummary {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "concise")]
    Concise,
    #[serde(rename = "detailed")]
    Detailed,
    #[serde(rename = "none")]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningSummaryPartAddedNotification {
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "summaryIndex")]
    pub summary_index: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningSummaryTextDeltaNotification {
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "summaryIndex")]
    pub summary_index: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReasoningTextDeltaNotification {
    #[serde(rename = "contentIndex")]
    pub content_index: i64,
    pub delta: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RequestPermissionProfile {
    #[serde(rename = "fileSystem", default, skip_serializing_if = "Option::is_none")]
    pub file_system: Option<AdditionalFileSystemPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<AdditionalNetworkPermissions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Resource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Value>>,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResourceTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReviewDelivery {
    #[serde(rename = "inline")]
    Inline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ReviewDelivery>,
    pub target: ReviewTarget,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewStartResponse {
    #[serde(rename = "reviewThreadId")]
    pub review_thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ReviewTarget {
    #[serde(rename = "uncommittedChanges")]
    UncommittedChanges {},
    #[serde(rename = "baseBranch")]
    BaseBranch {
        branch: String,
    },
    #[serde(rename = "commit")]
    Commit {
        sha: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "custom")]
    Custom {
        instructions: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SandboxMode {
    #[serde(rename = "read-only")]
    ReadOnly,
    #[serde(rename = "workspace-write")]
    WorkspaceWrite,
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum SandboxPolicy {
    #[serde(rename = "dangerFullAccess")]
    DangerFullAccess {},
    #[serde(rename = "readOnly")]
    ReadOnly {
        #[serde(rename = "networkAccess", default)]
        network_access: bool,
    },
    #[serde(rename = "externalSandbox")]
    ExternalSandbox {
        #[serde(rename = "networkAccess", default = "default_sandbox_policy_network_access")]
        network_access: NetworkAccess,
    },
    #[serde(rename = "workspaceWrite")]
    WorkspaceWrite {
        #[serde(rename = "excludeSlashTmp", default)]
        exclude_slash_tmp: bool,
        #[serde(rename = "excludeTmpdirEnvVar", default)]
        exclude_tmpdir_env_var: bool,
        #[serde(rename = "networkAccess", default)]
        network_access: bool,
        #[serde(rename = "writableRoots", default)]
        writable_roots: Vec<AbsolutePathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SandboxWorkspaceWrite {
    #[serde(default)]
    pub exclude_slash_tmp: bool,
    #[serde(default)]
    pub exclude_tmpdir_env_var: bool,
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub writable_roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServerRequestResolvedNotification {
    #[serde(rename = "requestId")]
    pub request_id: RequestId,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SessionSource {
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "exec")]
    Exec,
    #[serde(rename = "appServer")]
    AppServer,
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "custom")]
    Custom(String),
    #[serde(rename = "subAgent")]
    SubAgent(SubAgentSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillErrorInfo {
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillInterface {
    #[serde(rename = "brandColor", default, skip_serializing_if = "Option::is_none")]
    pub brand_color: Option<String>,
    #[serde(rename = "defaultPrompt", default, skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
    #[serde(rename = "displayName", default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(rename = "iconLarge", default, skip_serializing_if = "Option::is_none")]
    pub icon_large: Option<AbsolutePathBuf>,
    #[serde(rename = "iconLargeUrl", default, skip_serializing_if = "Option::is_none")]
    pub icon_large_url: Option<String>,
    #[serde(rename = "iconSmall", default, skip_serializing_if = "Option::is_none")]
    pub icon_small: Option<AbsolutePathBuf>,
    #[serde(rename = "iconSmallUrl", default, skip_serializing_if = "Option::is_none")]
    pub icon_small_url: Option<String>,
    #[serde(rename = "shortDescription", default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<SkillDependencies>,
    pub description: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<SkillInterface>,
    pub name: String,
    pub path: LegacyAppPathString,
    #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub scope: SkillScope,
    #[serde(rename = "shortDescription", default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SkillScope {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "repo")]
    Repo,
    #[serde(rename = "system")]
    System,
    #[serde(rename = "admin")]
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillToolDependency {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillsChangedNotification {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillsListEntry {
    pub cwd: String,
    pub errors: Vec<SkillErrorInfo>,
    pub skills: Vec<SkillMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
    #[serde(rename = "forceReload", default, skip_serializing_if = "Option::is_none")]
    pub force_reload: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SkillsListResponse {
    pub data: Vec<SkillsListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SortDirection {
    #[serde(rename = "asc")]
    Asc,
    #[serde(rename = "desc")]
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SubAgentActivityKind {
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "interacted")]
    Interacted,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "completed")]
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SubAgentSource {
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "compact")]
    Compact,
    #[serde(rename = "memory_consolidation")]
    MemoryConsolidation,
    #[serde(rename = "thread_spawn")]
    ThreadSpawn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_nickname: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_path: Option<AgentPath>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_role: Option<String>,
        depth: i32,
        parent_thread_id: ThreadId,
    },
    #[serde(rename = "other")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TerminalInteractionNotification {
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "processId")]
    pub process_id: String,
    pub stdin: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextElement {
    #[serde(rename = "byteRange")]
    pub byte_range: ByteRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Thread {
    #[serde(rename = "agentNickname", default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(rename = "agentRole", default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    #[serde(rename = "cliVersion")]
    pub cli_version: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub cwd: AbsolutePathBuf,
    pub ephemeral: bool,
    #[serde(rename = "forkedFromId", default, skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,
    #[serde(rename = "gitInfo", default, skip_serializing_if = "Option::is_none")]
    pub git_info: Option<GitInfo>,
    #[serde(rename = "historyMode", default = "default_thread_history_mode")]
    pub history_mode: ThreadHistoryMode,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(rename = "parentThreadId", default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub preview: String,
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(rename = "recencyAt", default, skip_serializing_if = "Option::is_none")]
    pub recency_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<ThreadSection>,
    #[serde(rename = "sectionEnteredAt", default, skip_serializing_if = "Option::is_none")]
    pub section_entered_at: Option<i64>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub source: SessionSource,
    pub status: ThreadStatus,
    #[serde(rename = "threadSource", default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
    pub turns: Vec<Turn>,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ThreadExecutor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ThreadPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadActiveFlag {
    #[serde(rename = "waitingOnApproval")]
    WaitingOnApproval,
    #[serde(rename = "waitingOnUserInput")]
    WaitingOnUserInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadArchiveParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadArchiveResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadArchivedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadClosedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadCompactStartParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadCompactStartResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadDeleteParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadDeleteResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadDeletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadExecutor {
    pub kind: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "agentName", default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadForkParams {
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(rename = "baseInstructions", default, skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "developerInstructions", default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    #[serde(rename = "excludeTurns", default, skip_serializing_if = "Option::is_none")]
    pub exclude_turns: Option<bool>,
    #[serde(rename = "lastTurnId", default, skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "modelProvider", default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "threadSource", default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadForkResponse {
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: AskForApproval,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: ApprovalsReviewer,
    pub cwd: AbsolutePathBuf,
    #[serde(rename = "disabledPluginIds", default)]
    pub disabled_plugin_ids: Vec<String>,
    #[serde(rename = "instructionSources", default)]
    pub instruction_sources: Vec<LegacyAppPathString>,
    pub model: String,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub sandbox: SandboxPolicy,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoal {
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "timeUsedSeconds")]
    pub time_used_seconds: i64,
    #[serde(rename = "tokenBudget", default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    #[serde(rename = "tokensUsed")]
    pub tokens_used: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalClearParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ThreadGoalMutationOrigin>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalClearResponse {
    pub cleared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalClearedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalGetParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalGetResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<ThreadGoal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadGoalMutationOrigin {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "automatic")]
    Automatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalSetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ThreadGoalMutationOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ThreadGoalStatus>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "tokenBudget", default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalSetResponse {
    pub goal: ThreadGoal,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadGoalStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "usageLimited")]
    UsageLimited,
    #[serde(rename = "budgetLimited")]
    BudgetLimited,
    #[serde(rename = "complete")]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadGoalUpdatedNotification {
    pub goal: ThreadGoal,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadHistoryMode {
    #[serde(rename = "legacy")]
    Legacy,
    #[serde(rename = "paginated")]
    Paginated,
}

pub type ThreadId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadItem {
    #[serde(rename = "completedAtMs", default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(rename = "startedAtMs", default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(flatten)]
    pub kind: ThreadItemKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadItemEntry {
    #[serde(rename = "completedAtMs", default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub item: ThreadItem,
    #[serde(rename = "startedAtMs", default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ThreadItemKind {
    #[serde(rename = "userMessage")]
    UserMessage {
        #[serde(rename = "clientId", default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        content: Vec<UserInput>,
        id: String,
    },
    #[serde(rename = "hookPrompt")]
    HookPrompt {
        fragments: Vec<HookPromptFragment>,
        id: String,
    },
    #[serde(rename = "agentMessage")]
    AgentMessage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<AgentMessageDelivery>,
        id: String,
        #[serde(rename = "memoryCitation", default, skip_serializing_if = "Option::is_none")]
        memory_citation: Option<MemoryCitation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<MessagePhase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        questions: Option<Vec<AsyncUserInputQuestion>>,
        text: String,
    },
    #[serde(rename = "functionCallOutput")]
    FunctionCallOutput {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        output: FunctionCallOutputBody,
    },
    #[serde(rename = "plan")]
    Plan {
        id: String,
        text: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        #[serde(default)]
        content: Vec<String>,
        id: String,
        #[serde(default)]
        summary: Vec<String>,
    },
    #[serde(rename = "commandExecution")]
    CommandExecution {
        #[serde(rename = "aggregatedOutput", default, skip_serializing_if = "Option::is_none")]
        aggregated_output: Option<String>,
        command: String,
        #[serde(rename = "commandActions")]
        command_actions: Vec<CommandAction>,
        cwd: LegacyAppPathString,
        #[serde(rename = "durationMs", default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i64>,
        #[serde(rename = "exitCode", default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        id: String,
        #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
        plugin_id: Option<String>,
        #[serde(rename = "processId", default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
        #[serde(rename = "scriptPath", default, skip_serializing_if = "Option::is_none")]
        script_path: Option<String>,
        #[serde(default = "default_thread_item_kind_source")]
        source: CommandExecutionSource,
        status: CommandExecutionStatus,
    },
    #[serde(rename = "fileChange")]
    FileChange {
        changes: Vec<FileUpdateChange>,
        id: String,
        status: PatchApplyStatus,
    },
    #[serde(rename = "mcpToolCall")]
    McpToolCall {
        #[serde(rename = "appContext", default, skip_serializing_if = "Option::is_none")]
        app_context: Option<McpToolCallAppContext>,
        arguments: Value,
        #[serde(rename = "durationMs", default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<McpToolCallError>,
        id: String,
        #[serde(rename = "mcpAppResourceUri", default, skip_serializing_if = "Option::is_none")]
        mcp_app_resource_uri: Option<String>,
        #[serde(rename = "mcpAppUi", default, skip_serializing_if = "Option::is_none")]
        mcp_app_ui: Option<McpAppUi>,
        #[serde(rename = "pluginId", default, skip_serializing_if = "Option::is_none")]
        plugin_id: Option<String>,
        #[serde(rename = "readOnlyHint", default, skip_serializing_if = "Option::is_none")]
        read_only_hint: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<McpToolCallResult>,
        server: String,
        status: McpToolCallStatus,
        tool: String,
    },
    #[serde(rename = "dynamicToolCall")]
    DynamicToolCall {
        arguments: Value,
        #[serde(rename = "contentItems", default, skip_serializing_if = "Option::is_none")]
        content_items: Option<Vec<DynamicToolCallOutputContentItem>>,
        #[serde(rename = "durationMs", default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i64>,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        status: DynamicToolCallStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
        tool: String,
    },
    #[serde(rename = "collabAgentToolCall")]
    CollabAgentToolCall {
        #[serde(rename = "agentsStates")]
        agents_states: BTreeMap<String, CollabAgentState>,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
        #[serde(rename = "receiverThreadIds")]
        receiver_thread_ids: Vec<String>,
        #[serde(rename = "senderThreadId")]
        sender_thread_id: String,
        status: CollabAgentToolCallStatus,
        tool: CollabAgentTool,
    },
    #[serde(rename = "subAgentActivity")]
    SubAgentActivity {
        #[serde(rename = "agentPath")]
        agent_path: String,
        #[serde(rename = "agentThreadId")]
        agent_thread_id: String,
        id: String,
        kind: SubAgentActivityKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    #[serde(rename = "webSearch")]
    WebSearch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<WebSearchAction>,
        id: String,
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        results: Option<Vec<Value>>,
    },
    #[serde(rename = "imageView")]
    ImageView {
        id: String,
        path: LegacyAppPathString,
    },
    #[serde(rename = "sleep")]
    Sleep {
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        id: String,
    },
    #[serde(rename = "imageGeneration")]
    ImageGeneration {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<ImageGenerationFailure>,
        #[serde(rename = "generationId", default, skip_serializing_if = "Option::is_none")]
        generation_id: Option<String>,
        id: String,
        #[serde(rename = "inputImages", default, skip_serializing_if = "Option::is_none")]
        input_images: Option<Vec<ImageGenerationInputImage>>,
        #[serde(rename = "revisedPrompt", default, skip_serializing_if = "Option::is_none")]
        revised_prompt: Option<String>,
        #[serde(rename = "savedPath", default, skip_serializing_if = "Option::is_none")]
        saved_path: Option<AbsolutePathBuf>,
        status: String,
        #[serde(rename = "transparentBackground", default, skip_serializing_if = "Option::is_none")]
        transparent_background: Option<bool>,
        #[serde(flatten)]
        kind: ThreadItemKindImageGenerationKind,
    },
    #[serde(rename = "enteredReviewMode")]
    EnteredReviewMode {
        id: String,
        review: String,
    },
    #[serde(rename = "exitedReviewMode")]
    ExitedReviewMode {
        id: String,
        review: String,
    },
    #[serde(rename = "contextCompaction")]
    ContextCompaction {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadItemKindImageGenerationKind {
    #[serde(rename = "fileId")]
    FileId(String),
    #[serde(rename = "result")]
    Result(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ThreadItemsListAnchor {
    #[serde(rename = "item")]
    Item {
        #[serde(rename = "itemId")]
        item_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ThreadItemsListCursor {
    String(String),
    ThreadItemsListAnchor(ThreadItemsListAnchor),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadItemsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ThreadItemsListCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(rename = "sortDirection", default, skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<SortDirection>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadItemsListResponse {
    #[serde(rename = "backwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub backwards_cursor: Option<String>,
    pub data: Vec<ThreadItemEntry>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ThreadListCwdFilter {
    String(String),
    Strings(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<ThreadListCwdFilter>,
    #[serde(rename = "excludedThreadIds", default, skip_serializing_if = "Option::is_none")]
    pub excluded_thread_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(rename = "modelProviders", default, skip_serializing_if = "Option::is_none")]
    pub model_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originators: Option<Vec<String>>,
    #[serde(rename = "searchTerm", default, skip_serializing_if = "Option::is_none")]
    pub search_term: Option<String>,
    #[serde(rename = "sectionId", default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    #[serde(rename = "sortDirection", default, skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<SortDirection>,
    #[serde(rename = "sortKey", default, skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<ThreadSortKey>,
    #[serde(rename = "sourceKinds", default, skip_serializing_if = "Option::is_none")]
    pub source_kinds: Option<Vec<ThreadSourceKind>>,
    #[serde(rename = "useStateDbOnly", default, skip_serializing_if = "Option::is_none")]
    pub use_state_db_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadListResponse {
    #[serde(rename = "backwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub backwards_cursor: Option<String>,
    pub data: Vec<Thread>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadLoadedListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadLoadedListResponse {
    pub data: Vec<String>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadNameUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "threadName", default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadPlacement {
    pub id: String,
    #[serde(rename = "providerKind")]
    pub provider_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadReadParams {
    #[serde(rename = "includeTurns", default, skip_serializing_if = "Option::is_none")]
    pub include_turns: Option<bool>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadReadResponse {
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadResumeParams {
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(rename = "baseInstructions", default, skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "developerInstructions", default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(rename = "excludeTurns", default, skip_serializing_if = "Option::is_none")]
    pub exclude_turns: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "modelProvider", default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadResumeResponse {
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: AskForApproval,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: ApprovalsReviewer,
    #[serde(rename = "collaborationMode", default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    pub cwd: AbsolutePathBuf,
    #[serde(rename = "disabledPluginIds", default)]
    pub disabled_plugin_ids: Vec<String>,
    #[serde(rename = "instructionSources", default)]
    pub instruction_sources: Vec<LegacyAppPathString>,
    #[serde(rename = "itemsBackwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub items_backwards_cursor: Option<String>,
    pub model: String,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub sandbox: SandboxPolicy,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub thread: Thread,
    #[serde(rename = "turnsBackwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub turns_backwards_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadRevertParams {
    #[serde(rename = "beforeTurnId")]
    pub before_turn_id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadRevertResponse {
    #[serde(rename = "itemsBackwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub items_backwards_cursor: Option<String>,
    pub thread: Thread,
    #[serde(rename = "turnsBackwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub turns_backwards_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadRevertedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<ThreadSectionAppearance>,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadSectionAppearance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadSetNameParams {
    pub name: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadSetNameResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadShellCommandParams {
    pub command: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadShellCommandResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadSortKey {
    #[serde(rename = "created_at")]
    CreatedAt,
    #[serde(rename = "updated_at")]
    UpdatedAt,
    #[serde(rename = "recency_at")]
    RecencyAt,
    #[serde(rename = "section_position")]
    SectionPosition,
}

pub type ThreadSource = String;

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadSourceKind {
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "exec")]
    Exec,
    #[serde(rename = "appServer")]
    AppServer,
    #[serde(rename = "subAgent")]
    SubAgent,
    #[serde(rename = "subAgentReview")]
    SubAgentReview,
    #[serde(rename = "subAgentCompact")]
    SubAgentCompact,
    #[serde(rename = "subAgentThreadSpawn")]
    SubAgentThreadSpawn,
    #[serde(rename = "subAgentOther")]
    SubAgentOther,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadStartParams {
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(rename = "baseInstructions", default, skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "developerInstructions", default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "modelProvider", default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(rename = "serviceName", default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(rename = "sessionStartSource", default, skip_serializing_if = "Option::is_none")]
    pub session_start_source: Option<ThreadStartSource>,
    #[serde(rename = "threadSource", default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadStartResponse {
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: AskForApproval,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: ApprovalsReviewer,
    pub cwd: AbsolutePathBuf,
    #[serde(rename = "disabledPluginIds", default)]
    pub disabled_plugin_ids: Vec<String>,
    #[serde(rename = "instructionSources", default)]
    pub instruction_sources: Vec<LegacyAppPathString>,
    pub model: String,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "reasoningEffort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub sandbox: SandboxPolicy,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadStartSource {
    #[serde(rename = "startup")]
    Startup,
    #[serde(rename = "clear")]
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadStartedNotification {
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum ThreadStatus {
    #[serde(rename = "notLoaded")]
    NotLoaded {},
    #[serde(rename = "idle")]
    Idle {},
    #[serde(rename = "systemError")]
    SystemError {},
    #[serde(rename = "active")]
    Active {
        #[serde(rename = "activeFlags")]
        active_flags: Vec<ThreadActiveFlag>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadStatusChangedNotification {
    pub status: ThreadStatus,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadTokenUsage {
    pub last: TokenUsageBreakdown,
    #[serde(rename = "modelContextWindow", default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<i64>,
    pub total: TokenUsageBreakdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadTokenUsageUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "tokenUsage")]
    pub token_usage: ThreadTokenUsage,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadTurnsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(rename = "itemsView", default, skip_serializing_if = "Option::is_none")]
    pub items_view: Option<TurnItemsView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(rename = "sortDirection", default, skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<SortDirection>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadTurnsListResponse {
    #[serde(rename = "backwardsCursor", default, skip_serializing_if = "Option::is_none")]
    pub backwards_cursor: Option<String>,
    pub data: Vec<Turn>,
    #[serde(rename = "nextCursor", default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadUnarchiveParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadUnarchiveResponse {
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadUnarchivedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadUnsubscribeParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ThreadUnsubscribeResponse {
    pub status: ThreadUnsubscribeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ThreadUnsubscribeStatus {
    #[serde(rename = "notLoaded")]
    NotLoaded,
    #[serde(rename = "notSubscribed")]
    NotSubscribed,
    #[serde(rename = "unsubscribed")]
    Unsubscribed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TokenUsageBreakdown {
    #[serde(rename = "cacheWriteInputTokens", default)]
    pub cache_write_input_tokens: i64,
    #[serde(rename = "cachedInputTokens")]
    pub cached_input_tokens: i64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: i64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: i64,
    #[serde(rename = "reasoningOutputTokens")]
    pub reasoning_output_tokens: i64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Tool {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Value>>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    pub name: String,
    #[serde(rename = "outputSchema", default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolRequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolRequestUserInputOption {
    pub description: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolRequestUserInputParams {
    #[serde(rename = "autoResolutionMs", default, skip_serializing_if = "Option::is_none")]
    pub auto_resolution_ms: Option<u64>,
    #[serde(rename = "isBlocking")]
    pub is_blocking: bool,
    #[serde(rename = "itemId")]
    pub item_id: String,
    pub questions: Vec<ToolRequestUserInputQuestion>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolRequestUserInputQuestion {
    pub header: String,
    pub id: String,
    #[serde(rename = "isOther", default)]
    pub is_other: bool,
    #[serde(rename = "isSecret", default)]
    pub is_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ToolRequestUserInputOption>>,
    pub question: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolRequestUserInputResponse {
    pub answers: BTreeMap<String, ToolRequestUserInputAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolsV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchToolConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Turn {
    #[serde(rename = "completedAt", default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(rename = "durationMs", default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
    pub id: String,
    pub items: Vec<ThreadItem>,
    #[serde(rename = "itemsView", default = "default_turn_items_view")]
    pub items_view: TurnItemsView,
    #[serde(rename = "rootTurnId", default, skip_serializing_if = "Option::is_none")]
    pub root_turn_id: Option<String>,
    #[serde(rename = "startedAt", default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnCompletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnDiffUpdatedNotification {
    pub diff: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnError {
    #[serde(rename = "additionalDetails", default, skip_serializing_if = "Option::is_none")]
    pub additional_details: Option<String>,
    #[serde(rename = "codexErrorInfo", default, skip_serializing_if = "Option::is_none")]
    pub codex_error_info: Option<CodexErrorInfo>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub misalignment: Option<MisalignmentErrorDetails>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnInterruptParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnInterruptResponse {
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TurnItemsView {
    #[serde(rename = "notLoaded")]
    NotLoaded,
    #[serde(rename = "summary")]
    Summary,
    #[serde(rename = "full")]
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnPlanStep {
    pub status: TurnPlanStepStatus,
    pub step: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TurnPlanStepStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnPlanUpdatedNotification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub plan: Vec<TurnPlanStep>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStartParams {
    #[serde(rename = "approvalPolicy", default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(rename = "approvalsReviewer", default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(rename = "clientUserMessageId", default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "disabledPluginIds", default, skip_serializing_if = "Option::is_none")]
    pub disabled_plugin_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    pub input: Vec<UserInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "outputSchema", default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(rename = "parentTurnId", default, skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(rename = "rootTurnId", default, skip_serializing_if = "Option::is_none")]
    pub root_turn_id: Option<String>,
    #[serde(rename = "sandboxPolicy", default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(rename = "serviceTier", default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(rename = "serviceTierForTurn", default, skip_serializing_if = "Option::is_none")]
    pub service_tier_for_turn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "toolOutput", default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<TurnToolOutput>,
    #[serde(rename = "turnTrigger", default, skip_serializing_if = "Option::is_none")]
    pub turn_trigger: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnStartedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TurnStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "inProgress")]
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnSteerParams {
    #[serde(rename = "clientUserMessageId", default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(rename = "expectedTurnId")]
    pub expected_turn_id: String,
    pub input: Vec<UserInput>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnSteerResponse {
    #[serde(rename = "turnId")]
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnToolOutput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub output: FunctionCallOutputBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum UserInput {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default)]
        text_elements: Vec<TextElement>,
    },
    #[serde(rename = "image")]
    Image {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
        #[serde(flatten)]
        kind: UserInputImageKind,
    },
    #[serde(rename = "localImage")]
    LocalImage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
        path: String,
    },
    #[serde(rename = "audio")]
    Audio {
        url: String,
    },
    #[serde(rename = "localAudio")]
    LocalAudio {
        path: String,
    },
    #[serde(rename = "skill")]
    Skill {
        name: String,
        path: String,
    },
    #[serde(rename = "mention")]
    Mention {
        name: String,
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum UserInputImageKind {
    #[serde(rename = "url")]
    Url(String),
    #[serde(rename = "fileId")]
    FileId(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Verbosity {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type")]
pub enum WebSearchAction {
    #[serde(rename = "search")]
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queries: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    #[serde(rename = "openPage")]
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    #[serde(rename = "findInPage")]
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    #[serde(rename = "other")]
    Other {},
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WebSearchContextSize {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WebSearchLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WebSearchMode {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "cached")]
    Cached,
    #[serde(rename = "indexed")]
    Indexed,
    #[serde(rename = "live")]
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WebSearchToolConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<WebSearchContextSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<WebSearchLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WriteStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "okOverridden")]
    OkOverridden,
}

fn default_command_execution_request_approval_params_kind() -> CommandExecutionApprovalKind {
    CommandExecutionApprovalKind::Command
}

fn default_model_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text, InputModality::Image]
}

fn default_permissions_request_approval_response_scope() -> PermissionGrantScope {
    PermissionGrantScope::Turn
}

fn default_sandbox_policy_network_access() -> NetworkAccess {
    NetworkAccess::Restricted
}

fn default_thread_history_mode() -> ThreadHistoryMode {
    ThreadHistoryMode::Legacy
}

fn default_thread_item_kind_source() -> CommandExecutionSource {
    CommandExecutionSource::Agent
}

fn default_turn_items_view() -> TurnItemsView {
    TurnItemsView::Full
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ClientRequestParams {
    #[serde(rename = "initialize")]
    Initialize(InitializeParams),
    #[serde(rename = "thread/start")]
    ThreadStart(ThreadStartParams),
    #[serde(rename = "thread/resume")]
    ThreadResume(ThreadResumeParams),
    #[serde(rename = "thread/fork")]
    ThreadFork(ThreadForkParams),
    #[serde(rename = "thread/archive")]
    ThreadArchive(ThreadArchiveParams),
    #[serde(rename = "thread/delete")]
    ThreadDelete(ThreadDeleteParams),
    #[serde(rename = "thread/unsubscribe")]
    ThreadUnsubscribe(ThreadUnsubscribeParams),
    #[serde(rename = "thread/name/set")]
    ThreadSetName(ThreadSetNameParams),
    #[serde(rename = "thread/goal/set")]
    ThreadGoalSet(ThreadGoalSetParams),
    #[serde(rename = "thread/goal/get")]
    ThreadGoalGet(ThreadGoalGetParams),
    #[serde(rename = "thread/goal/clear")]
    ThreadGoalClear(ThreadGoalClearParams),
    #[serde(rename = "thread/unarchive")]
    ThreadUnarchive(ThreadUnarchiveParams),
    #[serde(rename = "thread/compact/start")]
    ThreadCompactStart(ThreadCompactStartParams),
    #[serde(rename = "thread/shellCommand")]
    ThreadShellCommand(ThreadShellCommandParams),
    #[serde(rename = "thread/revert")]
    ThreadRevert(ThreadRevertParams),
    #[serde(rename = "thread/list")]
    ThreadList(ThreadListParams),
    #[serde(rename = "thread/loaded/list")]
    ThreadLoadedList(ThreadLoadedListParams),
    #[serde(rename = "thread/read")]
    ThreadRead(ThreadReadParams),
    #[serde(rename = "thread/turns/list")]
    ThreadTurnsList(ThreadTurnsListParams),
    #[serde(rename = "thread/items/list")]
    ThreadItemsList(ThreadItemsListParams),
    #[serde(rename = "skills/list")]
    SkillsList(SkillsListParams),
    #[serde(rename = "hooks/list")]
    HooksList(HooksListParams),
    #[serde(rename = "fs/readFile")]
    FsReadFile(FsReadFileParams),
    #[serde(rename = "fs/writeFile")]
    FsWriteFile(FsWriteFileParams),
    #[serde(rename = "fs/createDirectory")]
    FsCreateDirectory(FsCreateDirectoryParams),
    #[serde(rename = "fs/getMetadata")]
    FsGetMetadata(FsGetMetadataParams),
    #[serde(rename = "fs/readDirectory")]
    FsReadDirectory(FsReadDirectoryParams),
    #[serde(rename = "fs/remove")]
    FsRemove(FsRemoveParams),
    #[serde(rename = "fs/copy")]
    FsCopy(FsCopyParams),
    #[serde(rename = "fs/watch")]
    FsWatch(FsWatchParams),
    #[serde(rename = "fs/unwatch")]
    FsUnwatch(FsUnwatchParams),
    #[serde(rename = "turn/start")]
    TurnStart(TurnStartParams),
    #[serde(rename = "turn/steer")]
    TurnSteer(TurnSteerParams),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(TurnInterruptParams),
    #[serde(rename = "review/start")]
    ReviewStart(ReviewStartParams),
    #[serde(rename = "model/list")]
    ModelList(ModelListParams),
    #[serde(rename = "mcpServerStatus/list")]
    McpServerStatusList(ListMcpServerStatusParams),
    #[serde(rename = "command/exec")]
    OneOffCommandExec(CommandExecParams),
    #[serde(rename = "command/exec/write")]
    CommandExecWrite(CommandExecWriteParams),
    #[serde(rename = "command/exec/terminate")]
    CommandExecTerminate(CommandExecTerminateParams),
    #[serde(rename = "command/exec/resize")]
    CommandExecResize(CommandExecResizeParams),
    #[serde(rename = "config/read")]
    ConfigRead(ConfigReadParams),
    #[serde(rename = "config/value/write")]
    ConfigValueWrite(ConfigValueWriteParams),
    #[serde(rename = "config/batchWrite")]
    ConfigBatchWrite(ConfigBatchWriteParams),
}

impl ClientRequestParams {
    pub fn method(&self) -> ClientRequestMethod {
        match self {
            Self::Initialize(_) => ClientRequestMethod::Initialize,
            Self::ThreadStart(_) => ClientRequestMethod::ThreadStart,
            Self::ThreadResume(_) => ClientRequestMethod::ThreadResume,
            Self::ThreadFork(_) => ClientRequestMethod::ThreadFork,
            Self::ThreadArchive(_) => ClientRequestMethod::ThreadArchive,
            Self::ThreadDelete(_) => ClientRequestMethod::ThreadDelete,
            Self::ThreadUnsubscribe(_) => ClientRequestMethod::ThreadUnsubscribe,
            Self::ThreadSetName(_) => ClientRequestMethod::ThreadSetName,
            Self::ThreadGoalSet(_) => ClientRequestMethod::ThreadGoalSet,
            Self::ThreadGoalGet(_) => ClientRequestMethod::ThreadGoalGet,
            Self::ThreadGoalClear(_) => ClientRequestMethod::ThreadGoalClear,
            Self::ThreadUnarchive(_) => ClientRequestMethod::ThreadUnarchive,
            Self::ThreadCompactStart(_) => ClientRequestMethod::ThreadCompactStart,
            Self::ThreadShellCommand(_) => ClientRequestMethod::ThreadShellCommand,
            Self::ThreadRevert(_) => ClientRequestMethod::ThreadRevert,
            Self::ThreadList(_) => ClientRequestMethod::ThreadList,
            Self::ThreadLoadedList(_) => ClientRequestMethod::ThreadLoadedList,
            Self::ThreadRead(_) => ClientRequestMethod::ThreadRead,
            Self::ThreadTurnsList(_) => ClientRequestMethod::ThreadTurnsList,
            Self::ThreadItemsList(_) => ClientRequestMethod::ThreadItemsList,
            Self::SkillsList(_) => ClientRequestMethod::SkillsList,
            Self::HooksList(_) => ClientRequestMethod::HooksList,
            Self::FsReadFile(_) => ClientRequestMethod::FsReadFile,
            Self::FsWriteFile(_) => ClientRequestMethod::FsWriteFile,
            Self::FsCreateDirectory(_) => ClientRequestMethod::FsCreateDirectory,
            Self::FsGetMetadata(_) => ClientRequestMethod::FsGetMetadata,
            Self::FsReadDirectory(_) => ClientRequestMethod::FsReadDirectory,
            Self::FsRemove(_) => ClientRequestMethod::FsRemove,
            Self::FsCopy(_) => ClientRequestMethod::FsCopy,
            Self::FsWatch(_) => ClientRequestMethod::FsWatch,
            Self::FsUnwatch(_) => ClientRequestMethod::FsUnwatch,
            Self::TurnStart(_) => ClientRequestMethod::TurnStart,
            Self::TurnSteer(_) => ClientRequestMethod::TurnSteer,
            Self::TurnInterrupt(_) => ClientRequestMethod::TurnInterrupt,
            Self::ReviewStart(_) => ClientRequestMethod::ReviewStart,
            Self::ModelList(_) => ClientRequestMethod::ModelList,
            Self::McpServerStatusList(_) => ClientRequestMethod::McpServerStatusList,
            Self::OneOffCommandExec(_) => ClientRequestMethod::OneOffCommandExec,
            Self::CommandExecWrite(_) => ClientRequestMethod::CommandExecWrite,
            Self::CommandExecTerminate(_) => ClientRequestMethod::CommandExecTerminate,
            Self::CommandExecResize(_) => ClientRequestMethod::CommandExecResize,
            Self::ConfigRead(_) => ClientRequestMethod::ConfigRead,
            Self::ConfigValueWrite(_) => ClientRequestMethod::ConfigValueWrite,
            Self::ConfigBatchWrite(_) => ClientRequestMethod::ConfigBatchWrite,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ClientNotification {
    #[serde(rename = "initialized")]
    Initialized,
}

impl ClientNotification {
    pub fn method(&self) -> ClientNotificationMethod {
        match self {
            Self::Initialized => ClientNotificationMethod::Initialized,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ServerRequestParams {
    #[serde(rename = "item/commandExecution/requestApproval")]
    CommandExecutionRequestApproval(CommandExecutionRequestApprovalParams),
    #[serde(rename = "item/fileChange/requestApproval")]
    FileChangeRequestApproval(FileChangeRequestApprovalParams),
    #[serde(rename = "item/tool/requestUserInput")]
    ToolRequestUserInput(ToolRequestUserInputParams),
    #[serde(rename = "item/permissions/requestApproval")]
    PermissionsRequestApproval(PermissionsRequestApprovalParams),
    #[serde(rename = "item/tool/call")]
    DynamicToolCall(DynamicToolCallParams),
}

impl ServerRequestParams {
    pub fn method(&self) -> ServerRequestMethod {
        match self {
            Self::CommandExecutionRequestApproval(_) => ServerRequestMethod::CommandExecutionRequestApproval,
            Self::FileChangeRequestApproval(_) => ServerRequestMethod::FileChangeRequestApproval,
            Self::ToolRequestUserInput(_) => ServerRequestMethod::ToolRequestUserInput,
            Self::PermissionsRequestApproval(_) => ServerRequestMethod::PermissionsRequestApproval,
            Self::DynamicToolCall(_) => ServerRequestMethod::DynamicToolCall,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ServerNotification {
    #[serde(rename = "error")]
    Error(ErrorNotification),
    #[serde(rename = "thread/started")]
    ThreadStarted(ThreadStartedNotification),
    #[serde(rename = "thread/status/changed")]
    ThreadStatusChanged(ThreadStatusChangedNotification),
    #[serde(rename = "thread/archived")]
    ThreadArchived(ThreadArchivedNotification),
    #[serde(rename = "thread/deleted")]
    ThreadDeleted(ThreadDeletedNotification),
    #[serde(rename = "thread/unarchived")]
    ThreadUnarchived(ThreadUnarchivedNotification),
    #[serde(rename = "thread/closed")]
    ThreadClosed(ThreadClosedNotification),
    #[serde(rename = "thread/reverted")]
    ThreadReverted(ThreadRevertedNotification),
    #[serde(rename = "skills/changed")]
    SkillsChanged(SkillsChangedNotification),
    #[serde(rename = "thread/name/updated")]
    ThreadNameUpdated(ThreadNameUpdatedNotification),
    #[serde(rename = "thread/goal/updated")]
    ThreadGoalUpdated(ThreadGoalUpdatedNotification),
    #[serde(rename = "thread/goal/cleared")]
    ThreadGoalCleared(ThreadGoalClearedNotification),
    #[serde(rename = "thread/tokenUsage/updated")]
    ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification),
    #[serde(rename = "turn/started")]
    TurnStarted(TurnStartedNotification),
    #[serde(rename = "turn/completed")]
    TurnCompleted(TurnCompletedNotification),
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated(TurnDiffUpdatedNotification),
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated(TurnPlanUpdatedNotification),
    #[serde(rename = "item/started")]
    ItemStarted(ItemStartedNotification),
    #[serde(rename = "item/completed")]
    ItemCompleted(ItemCompletedNotification),
    #[serde(rename = "item/agentMessage/delta")]
    AgentMessageDelta(AgentMessageDeltaNotification),
    #[serde(rename = "item/plan/delta")]
    PlanDelta(PlanDeltaNotification),
    #[serde(rename = "command/exec/outputDelta")]
    CommandExecOutputDelta(CommandExecOutputDeltaNotification),
    #[serde(rename = "item/commandExecution/outputDelta")]
    CommandExecutionOutputDelta(CommandExecutionOutputDeltaNotification),
    #[serde(rename = "item/commandExecution/terminalInteraction")]
    TerminalInteraction(TerminalInteractionNotification),
    #[serde(rename = "item/fileChange/outputDelta")]
    FileChangeOutputDelta(FileChangeOutputDeltaNotification),
    #[serde(rename = "item/fileChange/patchUpdated")]
    FileChangePatchUpdated(FileChangePatchUpdatedNotification),
    #[serde(rename = "serverRequest/resolved")]
    ServerRequestResolved(ServerRequestResolvedNotification),
    #[serde(rename = "item/mcpToolCall/progress")]
    McpToolCallProgress(McpToolCallProgressNotification),
    #[serde(rename = "fs/changed")]
    FsChanged(FsChangedNotification),
    #[serde(rename = "item/reasoning/summaryTextDelta")]
    ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaNotification),
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ReasoningSummaryPartAdded(ReasoningSummaryPartAddedNotification),
    #[serde(rename = "item/reasoning/textDelta")]
    ReasoningTextDelta(ReasoningTextDeltaNotification),
    #[serde(rename = "thread/compacted")]
    ContextCompacted(ContextCompactedNotification),
}

impl ServerNotification {
    pub fn method(&self) -> ServerNotificationMethod {
        match self {
            Self::Error(_) => ServerNotificationMethod::Error,
            Self::ThreadStarted(_) => ServerNotificationMethod::ThreadStarted,
            Self::ThreadStatusChanged(_) => ServerNotificationMethod::ThreadStatusChanged,
            Self::ThreadArchived(_) => ServerNotificationMethod::ThreadArchived,
            Self::ThreadDeleted(_) => ServerNotificationMethod::ThreadDeleted,
            Self::ThreadUnarchived(_) => ServerNotificationMethod::ThreadUnarchived,
            Self::ThreadClosed(_) => ServerNotificationMethod::ThreadClosed,
            Self::ThreadReverted(_) => ServerNotificationMethod::ThreadReverted,
            Self::SkillsChanged(_) => ServerNotificationMethod::SkillsChanged,
            Self::ThreadNameUpdated(_) => ServerNotificationMethod::ThreadNameUpdated,
            Self::ThreadGoalUpdated(_) => ServerNotificationMethod::ThreadGoalUpdated,
            Self::ThreadGoalCleared(_) => ServerNotificationMethod::ThreadGoalCleared,
            Self::ThreadTokenUsageUpdated(_) => ServerNotificationMethod::ThreadTokenUsageUpdated,
            Self::TurnStarted(_) => ServerNotificationMethod::TurnStarted,
            Self::TurnCompleted(_) => ServerNotificationMethod::TurnCompleted,
            Self::TurnDiffUpdated(_) => ServerNotificationMethod::TurnDiffUpdated,
            Self::TurnPlanUpdated(_) => ServerNotificationMethod::TurnPlanUpdated,
            Self::ItemStarted(_) => ServerNotificationMethod::ItemStarted,
            Self::ItemCompleted(_) => ServerNotificationMethod::ItemCompleted,
            Self::AgentMessageDelta(_) => ServerNotificationMethod::AgentMessageDelta,
            Self::PlanDelta(_) => ServerNotificationMethod::PlanDelta,
            Self::CommandExecOutputDelta(_) => ServerNotificationMethod::CommandExecOutputDelta,
            Self::CommandExecutionOutputDelta(_) => ServerNotificationMethod::CommandExecutionOutputDelta,
            Self::TerminalInteraction(_) => ServerNotificationMethod::TerminalInteraction,
            Self::FileChangeOutputDelta(_) => ServerNotificationMethod::FileChangeOutputDelta,
            Self::FileChangePatchUpdated(_) => ServerNotificationMethod::FileChangePatchUpdated,
            Self::ServerRequestResolved(_) => ServerNotificationMethod::ServerRequestResolved,
            Self::McpToolCallProgress(_) => ServerNotificationMethod::McpToolCallProgress,
            Self::FsChanged(_) => ServerNotificationMethod::FsChanged,
            Self::ReasoningSummaryTextDelta(_) => ServerNotificationMethod::ReasoningSummaryTextDelta,
            Self::ReasoningSummaryPartAdded(_) => ServerNotificationMethod::ReasoningSummaryPartAdded,
            Self::ReasoningTextDelta(_) => ServerNotificationMethod::ReasoningTextDelta,
            Self::ContextCompacted(_) => ServerNotificationMethod::ContextCompacted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ClientResponse {
    Initialize(InitializeResponse),
    ThreadStart(ThreadStartResponse),
    ThreadResume(ThreadResumeResponse),
    ThreadFork(ThreadForkResponse),
    ThreadArchive(ThreadArchiveResponse),
    ThreadDelete(ThreadDeleteResponse),
    ThreadUnsubscribe(ThreadUnsubscribeResponse),
    ThreadSetName(ThreadSetNameResponse),
    ThreadGoalSet(ThreadGoalSetResponse),
    ThreadGoalGet(ThreadGoalGetResponse),
    ThreadGoalClear(ThreadGoalClearResponse),
    ThreadUnarchive(ThreadUnarchiveResponse),
    ThreadCompactStart(ThreadCompactStartResponse),
    ThreadShellCommand(ThreadShellCommandResponse),
    ThreadRevert(ThreadRevertResponse),
    ThreadList(ThreadListResponse),
    ThreadLoadedList(ThreadLoadedListResponse),
    ThreadRead(ThreadReadResponse),
    ThreadTurnsList(ThreadTurnsListResponse),
    ThreadItemsList(ThreadItemsListResponse),
    SkillsList(SkillsListResponse),
    HooksList(HooksListResponse),
    FsReadFile(FsReadFileResponse),
    FsWriteFile(FsWriteFileResponse),
    FsCreateDirectory(FsCreateDirectoryResponse),
    FsGetMetadata(FsGetMetadataResponse),
    FsReadDirectory(FsReadDirectoryResponse),
    FsRemove(FsRemoveResponse),
    FsCopy(FsCopyResponse),
    FsWatch(FsWatchResponse),
    FsUnwatch(FsUnwatchResponse),
    TurnStart(TurnStartResponse),
    TurnSteer(TurnSteerResponse),
    TurnInterrupt(TurnInterruptResponse),
    ReviewStart(ReviewStartResponse),
    ModelList(ModelListResponse),
    McpServerStatusList(ListMcpServerStatusResponse),
    OneOffCommandExec(CommandExecResponse),
    CommandExecWrite(CommandExecWriteResponse),
    CommandExecTerminate(CommandExecTerminateResponse),
    CommandExecResize(CommandExecResizeResponse),
    ConfigRead(ConfigReadResponse),
    ConfigValueWrite(ConfigWriteResponse),
    ConfigBatchWrite(ConfigWriteResponse),
}

impl ClientResponse {
    pub fn method(&self) -> ClientRequestMethod {
        match self {
            Self::Initialize(_) => ClientRequestMethod::Initialize,
            Self::ThreadStart(_) => ClientRequestMethod::ThreadStart,
            Self::ThreadResume(_) => ClientRequestMethod::ThreadResume,
            Self::ThreadFork(_) => ClientRequestMethod::ThreadFork,
            Self::ThreadArchive(_) => ClientRequestMethod::ThreadArchive,
            Self::ThreadDelete(_) => ClientRequestMethod::ThreadDelete,
            Self::ThreadUnsubscribe(_) => ClientRequestMethod::ThreadUnsubscribe,
            Self::ThreadSetName(_) => ClientRequestMethod::ThreadSetName,
            Self::ThreadGoalSet(_) => ClientRequestMethod::ThreadGoalSet,
            Self::ThreadGoalGet(_) => ClientRequestMethod::ThreadGoalGet,
            Self::ThreadGoalClear(_) => ClientRequestMethod::ThreadGoalClear,
            Self::ThreadUnarchive(_) => ClientRequestMethod::ThreadUnarchive,
            Self::ThreadCompactStart(_) => ClientRequestMethod::ThreadCompactStart,
            Self::ThreadShellCommand(_) => ClientRequestMethod::ThreadShellCommand,
            Self::ThreadRevert(_) => ClientRequestMethod::ThreadRevert,
            Self::ThreadList(_) => ClientRequestMethod::ThreadList,
            Self::ThreadLoadedList(_) => ClientRequestMethod::ThreadLoadedList,
            Self::ThreadRead(_) => ClientRequestMethod::ThreadRead,
            Self::ThreadTurnsList(_) => ClientRequestMethod::ThreadTurnsList,
            Self::ThreadItemsList(_) => ClientRequestMethod::ThreadItemsList,
            Self::SkillsList(_) => ClientRequestMethod::SkillsList,
            Self::HooksList(_) => ClientRequestMethod::HooksList,
            Self::FsReadFile(_) => ClientRequestMethod::FsReadFile,
            Self::FsWriteFile(_) => ClientRequestMethod::FsWriteFile,
            Self::FsCreateDirectory(_) => ClientRequestMethod::FsCreateDirectory,
            Self::FsGetMetadata(_) => ClientRequestMethod::FsGetMetadata,
            Self::FsReadDirectory(_) => ClientRequestMethod::FsReadDirectory,
            Self::FsRemove(_) => ClientRequestMethod::FsRemove,
            Self::FsCopy(_) => ClientRequestMethod::FsCopy,
            Self::FsWatch(_) => ClientRequestMethod::FsWatch,
            Self::FsUnwatch(_) => ClientRequestMethod::FsUnwatch,
            Self::TurnStart(_) => ClientRequestMethod::TurnStart,
            Self::TurnSteer(_) => ClientRequestMethod::TurnSteer,
            Self::TurnInterrupt(_) => ClientRequestMethod::TurnInterrupt,
            Self::ReviewStart(_) => ClientRequestMethod::ReviewStart,
            Self::ModelList(_) => ClientRequestMethod::ModelList,
            Self::McpServerStatusList(_) => ClientRequestMethod::McpServerStatusList,
            Self::OneOffCommandExec(_) => ClientRequestMethod::OneOffCommandExec,
            Self::CommandExecWrite(_) => ClientRequestMethod::CommandExecWrite,
            Self::CommandExecTerminate(_) => ClientRequestMethod::CommandExecTerminate,
            Self::CommandExecResize(_) => ClientRequestMethod::CommandExecResize,
            Self::ConfigRead(_) => ClientRequestMethod::ConfigRead,
            Self::ConfigValueWrite(_) => ClientRequestMethod::ConfigValueWrite,
            Self::ConfigBatchWrite(_) => ClientRequestMethod::ConfigBatchWrite,
        }
    }

    pub fn decode(method: ClientRequestMethod, result: Value) -> Result<Self, serde_json::Error> {
        Ok(match method {
            ClientRequestMethod::Initialize => Self::Initialize(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadStart => Self::ThreadStart(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadResume => Self::ThreadResume(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadFork => Self::ThreadFork(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadArchive => Self::ThreadArchive(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadDelete => Self::ThreadDelete(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadUnsubscribe => Self::ThreadUnsubscribe(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadSetName => Self::ThreadSetName(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadGoalSet => Self::ThreadGoalSet(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadGoalGet => Self::ThreadGoalGet(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadGoalClear => Self::ThreadGoalClear(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadUnarchive => Self::ThreadUnarchive(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadCompactStart => Self::ThreadCompactStart(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadShellCommand => Self::ThreadShellCommand(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadRevert => Self::ThreadRevert(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadList => Self::ThreadList(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadLoadedList => Self::ThreadLoadedList(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadRead => Self::ThreadRead(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadTurnsList => Self::ThreadTurnsList(serde_json::from_value(result)?),
            ClientRequestMethod::ThreadItemsList => Self::ThreadItemsList(serde_json::from_value(result)?),
            ClientRequestMethod::SkillsList => Self::SkillsList(serde_json::from_value(result)?),
            ClientRequestMethod::HooksList => Self::HooksList(serde_json::from_value(result)?),
            ClientRequestMethod::FsReadFile => Self::FsReadFile(serde_json::from_value(result)?),
            ClientRequestMethod::FsWriteFile => Self::FsWriteFile(serde_json::from_value(result)?),
            ClientRequestMethod::FsCreateDirectory => Self::FsCreateDirectory(serde_json::from_value(result)?),
            ClientRequestMethod::FsGetMetadata => Self::FsGetMetadata(serde_json::from_value(result)?),
            ClientRequestMethod::FsReadDirectory => Self::FsReadDirectory(serde_json::from_value(result)?),
            ClientRequestMethod::FsRemove => Self::FsRemove(serde_json::from_value(result)?),
            ClientRequestMethod::FsCopy => Self::FsCopy(serde_json::from_value(result)?),
            ClientRequestMethod::FsWatch => Self::FsWatch(serde_json::from_value(result)?),
            ClientRequestMethod::FsUnwatch => Self::FsUnwatch(serde_json::from_value(result)?),
            ClientRequestMethod::TurnStart => Self::TurnStart(serde_json::from_value(result)?),
            ClientRequestMethod::TurnSteer => Self::TurnSteer(serde_json::from_value(result)?),
            ClientRequestMethod::TurnInterrupt => Self::TurnInterrupt(serde_json::from_value(result)?),
            ClientRequestMethod::ReviewStart => Self::ReviewStart(serde_json::from_value(result)?),
            ClientRequestMethod::ModelList => Self::ModelList(serde_json::from_value(result)?),
            ClientRequestMethod::McpServerStatusList => Self::McpServerStatusList(serde_json::from_value(result)?),
            ClientRequestMethod::OneOffCommandExec => Self::OneOffCommandExec(serde_json::from_value(result)?),
            ClientRequestMethod::CommandExecWrite => Self::CommandExecWrite(serde_json::from_value(result)?),
            ClientRequestMethod::CommandExecTerminate => Self::CommandExecTerminate(serde_json::from_value(result)?),
            ClientRequestMethod::CommandExecResize => Self::CommandExecResize(serde_json::from_value(result)?),
            ClientRequestMethod::ConfigRead => Self::ConfigRead(serde_json::from_value(result)?),
            ClientRequestMethod::ConfigValueWrite => Self::ConfigValueWrite(serde_json::from_value(result)?),
            ClientRequestMethod::ConfigBatchWrite => Self::ConfigBatchWrite(serde_json::from_value(result)?),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ServerRequestResponse {
    CommandExecutionRequestApproval(CommandExecutionRequestApprovalResponse),
    FileChangeRequestApproval(FileChangeRequestApprovalResponse),
    ToolRequestUserInput(ToolRequestUserInputResponse),
    PermissionsRequestApproval(PermissionsRequestApprovalResponse),
    DynamicToolCall(DynamicToolCallResponse),
}

impl ServerRequestResponse {
    pub fn method(&self) -> ServerRequestMethod {
        match self {
            Self::CommandExecutionRequestApproval(_) => ServerRequestMethod::CommandExecutionRequestApproval,
            Self::FileChangeRequestApproval(_) => ServerRequestMethod::FileChangeRequestApproval,
            Self::ToolRequestUserInput(_) => ServerRequestMethod::ToolRequestUserInput,
            Self::PermissionsRequestApproval(_) => ServerRequestMethod::PermissionsRequestApproval,
            Self::DynamicToolCall(_) => ServerRequestMethod::DynamicToolCall,
        }
    }

    pub fn decode(method: ServerRequestMethod, result: Value) -> Result<Self, serde_json::Error> {
        Ok(match method {
            ServerRequestMethod::CommandExecutionRequestApproval => Self::CommandExecutionRequestApproval(serde_json::from_value(result)?),
            ServerRequestMethod::FileChangeRequestApproval => Self::FileChangeRequestApproval(serde_json::from_value(result)?),
            ServerRequestMethod::ToolRequestUserInput => Self::ToolRequestUserInput(serde_json::from_value(result)?),
            ServerRequestMethod::PermissionsRequestApproval => Self::PermissionsRequestApproval(serde_json::from_value(result)?),
            ServerRequestMethod::DynamicToolCall => Self::DynamicToolCall(serde_json::from_value(result)?),
        })
    }
}

pub const TYPE_NAMES: &[&str] = &[
    "AbsolutePathBuf",
    "AdditionalFileSystemPermissions",
    "AdditionalNetworkPermissions",
    "AgentMessageDelivery",
    "AgentMessageDeltaNotification",
    "AgentPath",
    "AllowDenyRequirement",
    "AnalyticsConfig",
    "ApprovalsReviewer",
    "AskForApproval",
    "AsyncUserInputQuestion",
    "AutoCompactTokenLimitScope",
    "BrowserUseConfig",
    "BrowserUseOriginPolicyConfig",
    "ByteRange",
    "CapabilityRefusal",
    "CapabilityRefusalCode",
    "CapabilityRefusalData",
    "ClientInfo",
    "CodexErrorInfo",
    "CodexErrorInfoVariant0",
    "CodexErrorInfoVariant1",
    "CodexErrorInfoVariant1HttpConnectionFailed",
    "CodexErrorInfoVariant2",
    "CodexErrorInfoVariant2ResponseStreamConnectionFailed",
    "CodexErrorInfoVariant3",
    "CodexErrorInfoVariant3ResponseStreamDisconnected",
    "CodexErrorInfoVariant4",
    "CodexErrorInfoVariant4ResponseTooManyFailedAttempts",
    "CodexErrorInfoVariant5",
    "CodexErrorInfoVariant5ActiveTurnNotSteerable",
    "CollabAgentState",
    "CollabAgentStatus",
    "CollabAgentTool",
    "CollabAgentToolCallStatus",
    "CollaborationMode",
    "CommandAction",
    "CommandExecOutputDeltaNotification",
    "CommandExecOutputStream",
    "CommandExecParams",
    "CommandExecResizeParams",
    "CommandExecResizeResponse",
    "CommandExecResponse",
    "CommandExecTerminalSize",
    "CommandExecTerminateParams",
    "CommandExecTerminateResponse",
    "CommandExecWriteParams",
    "CommandExecWriteResponse",
    "CommandExecutionApprovalDecision",
    "CommandExecutionApprovalKind",
    "CommandExecutionOutputDeltaNotification",
    "CommandExecutionRequestApprovalParams",
    "CommandExecutionRequestApprovalResponse",
    "CommandExecutionSource",
    "CommandExecutionStatus",
    "ComputerUseConfig",
    "ComputerUseMacosConfig",
    "ComputerUseWindowsConfig",
    "ComputerUseWindowsExeConfig",
    "Config",
    "ConfigBatchWriteParams",
    "ConfigEdit",
    "ConfigLayer",
    "ConfigLayerMetadata",
    "ConfigLayerSource",
    "ConfigReadParams",
    "ConfigReadResponse",
    "ConfigValueWriteParams",
    "ConfigWriteResponse",
    "ContextCompactedNotification",
    "CyberAccessProgram",
    "DynamicToolCallOutputContentItem",
    "DynamicToolCallParams",
    "DynamicToolCallResponse",
    "DynamicToolCallStatus",
    "ErrorNotification",
    "FileChangeApprovalDecision",
    "FileChangeOutputDeltaNotification",
    "FileChangePatchUpdatedNotification",
    "FileChangeRequestApprovalParams",
    "FileChangeRequestApprovalResponse",
    "FileSystemAccessMode",
    "FileSystemPath",
    "FileSystemSandboxEntry",
    "FileSystemSpecialPath",
    "FileUpdateChange",
    "ForcedChatgptWorkspaceIds",
    "ForcedLoginMethod",
    "FsChangedNotification",
    "FsCopyParams",
    "FsCopyResponse",
    "FsCreateDirectoryParams",
    "FsCreateDirectoryResponse",
    "FsGetMetadataParams",
    "FsGetMetadataResponse",
    "FsReadDirectoryEntry",
    "FsReadDirectoryParams",
    "FsReadDirectoryResponse",
    "FsReadFileParams",
    "FsReadFileResponse",
    "FsRemoveParams",
    "FsRemoveResponse",
    "FsUnwatchParams",
    "FsUnwatchResponse",
    "FsWatchParams",
    "FsWatchResponse",
    "FsWriteFileParams",
    "FsWriteFileResponse",
    "FunctionCallOutputBody",
    "FunctionCallOutputContentItem",
    "FunctionCallOutputContentItemInputImageKind",
    "GitInfo",
    "GrantedPermissionProfile",
    "HookErrorInfo",
    "HookEventName",
    "HookMetadata",
    "HookMetadataKind",
    "HookPromptFragment",
    "HookSource",
    "HookTrustStatus",
    "HooksListEntry",
    "HooksListParams",
    "HooksListResponse",
    "ImageDetail",
    "ImageGenerationFailure",
    "ImageGenerationInputImage",
    "InitializeCapabilities",
    "InitializeParams",
    "InitializeResponse",
    "InputModality",
    "ItemCompletedNotification",
    "ItemStartedNotification",
    "LegacyAppPathString",
    "ListMcpServerStatusParams",
    "ListMcpServerStatusResponse",
    "McpAppDisplayMode",
    "McpAppUi",
    "McpAuthStatus",
    "McpServerConnectionStatus",
    "McpServerInfo",
    "McpServerStatus",
    "McpServerStatusDetail",
    "McpToolCallAppContext",
    "McpToolCallError",
    "McpToolCallProgressNotification",
    "McpToolCallResult",
    "McpToolCallStatus",
    "MemoryCitation",
    "MemoryCitationEntry",
    "MergeStrategy",
    "MessagePhase",
    "MisalignmentErrorDetails",
    "MisalignmentSteer",
    "ModeKind",
    "Model",
    "ModelAccessPrograms",
    "ModelAvailabilityNux",
    "ModelListParams",
    "ModelListResponse",
    "ModelServiceTier",
    "ModelUpgradeInfo",
    "MultiAgentVersion",
    "NetworkAccess",
    "NetworkApprovalContext",
    "NetworkApprovalProtocol",
    "NetworkPolicyAmendment",
    "NetworkPolicyRuleAction",
    "NonSteerableTurnKind",
    "OverriddenMetadata",
    "PatchApplyStatus",
    "PatchChangeKind",
    "PermissionGrantScope",
    "PermissionsRequestApprovalParams",
    "PermissionsRequestApprovalResponse",
    "Personality",
    "PlanDeltaNotification",
    "ReasoningEffort",
    "ReasoningEffortOption",
    "ReasoningSummary",
    "ReasoningSummaryPartAddedNotification",
    "ReasoningSummaryTextDeltaNotification",
    "ReasoningTextDeltaNotification",
    "RequestId",
    "RequestPermissionProfile",
    "Resource",
    "ResourceTemplate",
    "ReviewDelivery",
    "ReviewStartParams",
    "ReviewStartResponse",
    "ReviewTarget",
    "SandboxMode",
    "SandboxPolicy",
    "SandboxWorkspaceWrite",
    "ServerRequestResolvedNotification",
    "SessionSource",
    "Settings",
    "SkillDependencies",
    "SkillErrorInfo",
    "SkillInterface",
    "SkillMetadata",
    "SkillScope",
    "SkillToolDependency",
    "SkillsChangedNotification",
    "SkillsListEntry",
    "SkillsListParams",
    "SkillsListResponse",
    "SortDirection",
    "SubAgentActivityKind",
    "SubAgentSource",
    "TerminalInteractionNotification",
    "TextElement",
    "Thread",
    "ThreadActiveFlag",
    "ThreadArchiveParams",
    "ThreadArchiveResponse",
    "ThreadArchivedNotification",
    "ThreadClosedNotification",
    "ThreadCompactStartParams",
    "ThreadCompactStartResponse",
    "ThreadDeleteParams",
    "ThreadDeleteResponse",
    "ThreadDeletedNotification",
    "ThreadExecutor",
    "ThreadForkParams",
    "ThreadForkResponse",
    "ThreadGoal",
    "ThreadGoalClearParams",
    "ThreadGoalClearResponse",
    "ThreadGoalClearedNotification",
    "ThreadGoalGetParams",
    "ThreadGoalGetResponse",
    "ThreadGoalMutationOrigin",
    "ThreadGoalSetParams",
    "ThreadGoalSetResponse",
    "ThreadGoalStatus",
    "ThreadGoalUpdatedNotification",
    "ThreadHistoryMode",
    "ThreadId",
    "ThreadItem",
    "ThreadItemEntry",
    "ThreadItemKind",
    "ThreadItemKindImageGenerationKind",
    "ThreadItemsListAnchor",
    "ThreadItemsListCursor",
    "ThreadItemsListParams",
    "ThreadItemsListResponse",
    "ThreadListCwdFilter",
    "ThreadListParams",
    "ThreadListResponse",
    "ThreadLoadedListParams",
    "ThreadLoadedListResponse",
    "ThreadNameUpdatedNotification",
    "ThreadPlacement",
    "ThreadReadParams",
    "ThreadReadResponse",
    "ThreadResumeParams",
    "ThreadResumeResponse",
    "ThreadRevertParams",
    "ThreadRevertResponse",
    "ThreadRevertedNotification",
    "ThreadSection",
    "ThreadSectionAppearance",
    "ThreadSetNameParams",
    "ThreadSetNameResponse",
    "ThreadShellCommandParams",
    "ThreadShellCommandResponse",
    "ThreadSortKey",
    "ThreadSource",
    "ThreadSourceKind",
    "ThreadStartParams",
    "ThreadStartResponse",
    "ThreadStartSource",
    "ThreadStartedNotification",
    "ThreadStatus",
    "ThreadStatusChangedNotification",
    "ThreadTokenUsage",
    "ThreadTokenUsageUpdatedNotification",
    "ThreadTurnsListParams",
    "ThreadTurnsListResponse",
    "ThreadUnarchiveParams",
    "ThreadUnarchiveResponse",
    "ThreadUnarchivedNotification",
    "ThreadUnsubscribeParams",
    "ThreadUnsubscribeResponse",
    "ThreadUnsubscribeStatus",
    "TokenUsageBreakdown",
    "Tool",
    "ToolRequestUserInputAnswer",
    "ToolRequestUserInputOption",
    "ToolRequestUserInputParams",
    "ToolRequestUserInputQuestion",
    "ToolRequestUserInputResponse",
    "ToolsV2",
    "Turn",
    "TurnCompletedNotification",
    "TurnDiffUpdatedNotification",
    "TurnError",
    "TurnInterruptParams",
    "TurnInterruptResponse",
    "TurnItemsView",
    "TurnPlanStep",
    "TurnPlanStepStatus",
    "TurnPlanUpdatedNotification",
    "TurnStartParams",
    "TurnStartResponse",
    "TurnStartedNotification",
    "TurnStatus",
    "TurnSteerParams",
    "TurnSteerResponse",
    "TurnToolOutput",
    "UserInput",
    "UserInputImageKind",
    "Verbosity",
    "WebSearchAction",
    "WebSearchContextSize",
    "WebSearchLocation",
    "WebSearchMode",
    "WebSearchToolConfig",
    "WriteStatus",
];

pub fn round_trip(type_name: &str, value: Value) -> Option<Result<Value, serde_json::Error>> {
    fn again<T: serde::de::DeserializeOwned + Serialize>(value: Value) -> Result<Value, serde_json::Error> {
        serde_json::to_value(serde_json::from_value::<T>(value)?)
    }
    Some(match type_name {
        "AbsolutePathBuf" => again::<AbsolutePathBuf>(value),
        "AdditionalFileSystemPermissions" => again::<AdditionalFileSystemPermissions>(value),
        "AdditionalNetworkPermissions" => again::<AdditionalNetworkPermissions>(value),
        "AgentMessageDelivery" => again::<AgentMessageDelivery>(value),
        "AgentMessageDeltaNotification" => again::<AgentMessageDeltaNotification>(value),
        "AgentPath" => again::<AgentPath>(value),
        "AllowDenyRequirement" => again::<AllowDenyRequirement>(value),
        "AnalyticsConfig" => again::<AnalyticsConfig>(value),
        "ApprovalsReviewer" => again::<ApprovalsReviewer>(value),
        "AskForApproval" => again::<AskForApproval>(value),
        "AsyncUserInputQuestion" => again::<AsyncUserInputQuestion>(value),
        "AutoCompactTokenLimitScope" => again::<AutoCompactTokenLimitScope>(value),
        "BrowserUseConfig" => again::<BrowserUseConfig>(value),
        "BrowserUseOriginPolicyConfig" => again::<BrowserUseOriginPolicyConfig>(value),
        "ByteRange" => again::<ByteRange>(value),
        "CapabilityRefusal" => again::<CapabilityRefusal>(value),
        "CapabilityRefusalCode" => again::<CapabilityRefusalCode>(value),
        "CapabilityRefusalData" => again::<CapabilityRefusalData>(value),
        "ClientInfo" => again::<ClientInfo>(value),
        "CodexErrorInfo" => again::<CodexErrorInfo>(value),
        "CodexErrorInfoVariant0" => again::<CodexErrorInfoVariant0>(value),
        "CodexErrorInfoVariant1" => again::<CodexErrorInfoVariant1>(value),
        "CodexErrorInfoVariant1HttpConnectionFailed" => again::<CodexErrorInfoVariant1HttpConnectionFailed>(value),
        "CodexErrorInfoVariant2" => again::<CodexErrorInfoVariant2>(value),
        "CodexErrorInfoVariant2ResponseStreamConnectionFailed" => again::<CodexErrorInfoVariant2ResponseStreamConnectionFailed>(value),
        "CodexErrorInfoVariant3" => again::<CodexErrorInfoVariant3>(value),
        "CodexErrorInfoVariant3ResponseStreamDisconnected" => again::<CodexErrorInfoVariant3ResponseStreamDisconnected>(value),
        "CodexErrorInfoVariant4" => again::<CodexErrorInfoVariant4>(value),
        "CodexErrorInfoVariant4ResponseTooManyFailedAttempts" => again::<CodexErrorInfoVariant4ResponseTooManyFailedAttempts>(value),
        "CodexErrorInfoVariant5" => again::<CodexErrorInfoVariant5>(value),
        "CodexErrorInfoVariant5ActiveTurnNotSteerable" => again::<CodexErrorInfoVariant5ActiveTurnNotSteerable>(value),
        "CollabAgentState" => again::<CollabAgentState>(value),
        "CollabAgentStatus" => again::<CollabAgentStatus>(value),
        "CollabAgentTool" => again::<CollabAgentTool>(value),
        "CollabAgentToolCallStatus" => again::<CollabAgentToolCallStatus>(value),
        "CollaborationMode" => again::<CollaborationMode>(value),
        "CommandAction" => again::<CommandAction>(value),
        "CommandExecOutputDeltaNotification" => again::<CommandExecOutputDeltaNotification>(value),
        "CommandExecOutputStream" => again::<CommandExecOutputStream>(value),
        "CommandExecParams" => again::<CommandExecParams>(value),
        "CommandExecResizeParams" => again::<CommandExecResizeParams>(value),
        "CommandExecResizeResponse" => again::<CommandExecResizeResponse>(value),
        "CommandExecResponse" => again::<CommandExecResponse>(value),
        "CommandExecTerminalSize" => again::<CommandExecTerminalSize>(value),
        "CommandExecTerminateParams" => again::<CommandExecTerminateParams>(value),
        "CommandExecTerminateResponse" => again::<CommandExecTerminateResponse>(value),
        "CommandExecWriteParams" => again::<CommandExecWriteParams>(value),
        "CommandExecWriteResponse" => again::<CommandExecWriteResponse>(value),
        "CommandExecutionApprovalDecision" => again::<CommandExecutionApprovalDecision>(value),
        "CommandExecutionApprovalKind" => again::<CommandExecutionApprovalKind>(value),
        "CommandExecutionOutputDeltaNotification" => again::<CommandExecutionOutputDeltaNotification>(value),
        "CommandExecutionRequestApprovalParams" => again::<CommandExecutionRequestApprovalParams>(value),
        "CommandExecutionRequestApprovalResponse" => again::<CommandExecutionRequestApprovalResponse>(value),
        "CommandExecutionSource" => again::<CommandExecutionSource>(value),
        "CommandExecutionStatus" => again::<CommandExecutionStatus>(value),
        "ComputerUseConfig" => again::<ComputerUseConfig>(value),
        "ComputerUseMacosConfig" => again::<ComputerUseMacosConfig>(value),
        "ComputerUseWindowsConfig" => again::<ComputerUseWindowsConfig>(value),
        "ComputerUseWindowsExeConfig" => again::<ComputerUseWindowsExeConfig>(value),
        "Config" => again::<Config>(value),
        "ConfigBatchWriteParams" => again::<ConfigBatchWriteParams>(value),
        "ConfigEdit" => again::<ConfigEdit>(value),
        "ConfigLayer" => again::<ConfigLayer>(value),
        "ConfigLayerMetadata" => again::<ConfigLayerMetadata>(value),
        "ConfigLayerSource" => again::<ConfigLayerSource>(value),
        "ConfigReadParams" => again::<ConfigReadParams>(value),
        "ConfigReadResponse" => again::<ConfigReadResponse>(value),
        "ConfigValueWriteParams" => again::<ConfigValueWriteParams>(value),
        "ConfigWriteResponse" => again::<ConfigWriteResponse>(value),
        "ContextCompactedNotification" => again::<ContextCompactedNotification>(value),
        "CyberAccessProgram" => again::<CyberAccessProgram>(value),
        "DynamicToolCallOutputContentItem" => again::<DynamicToolCallOutputContentItem>(value),
        "DynamicToolCallParams" => again::<DynamicToolCallParams>(value),
        "DynamicToolCallResponse" => again::<DynamicToolCallResponse>(value),
        "DynamicToolCallStatus" => again::<DynamicToolCallStatus>(value),
        "ErrorNotification" => again::<ErrorNotification>(value),
        "FileChangeApprovalDecision" => again::<FileChangeApprovalDecision>(value),
        "FileChangeOutputDeltaNotification" => again::<FileChangeOutputDeltaNotification>(value),
        "FileChangePatchUpdatedNotification" => again::<FileChangePatchUpdatedNotification>(value),
        "FileChangeRequestApprovalParams" => again::<FileChangeRequestApprovalParams>(value),
        "FileChangeRequestApprovalResponse" => again::<FileChangeRequestApprovalResponse>(value),
        "FileSystemAccessMode" => again::<FileSystemAccessMode>(value),
        "FileSystemPath" => again::<FileSystemPath>(value),
        "FileSystemSandboxEntry" => again::<FileSystemSandboxEntry>(value),
        "FileSystemSpecialPath" => again::<FileSystemSpecialPath>(value),
        "FileUpdateChange" => again::<FileUpdateChange>(value),
        "ForcedChatgptWorkspaceIds" => again::<ForcedChatgptWorkspaceIds>(value),
        "ForcedLoginMethod" => again::<ForcedLoginMethod>(value),
        "FsChangedNotification" => again::<FsChangedNotification>(value),
        "FsCopyParams" => again::<FsCopyParams>(value),
        "FsCopyResponse" => again::<FsCopyResponse>(value),
        "FsCreateDirectoryParams" => again::<FsCreateDirectoryParams>(value),
        "FsCreateDirectoryResponse" => again::<FsCreateDirectoryResponse>(value),
        "FsGetMetadataParams" => again::<FsGetMetadataParams>(value),
        "FsGetMetadataResponse" => again::<FsGetMetadataResponse>(value),
        "FsReadDirectoryEntry" => again::<FsReadDirectoryEntry>(value),
        "FsReadDirectoryParams" => again::<FsReadDirectoryParams>(value),
        "FsReadDirectoryResponse" => again::<FsReadDirectoryResponse>(value),
        "FsReadFileParams" => again::<FsReadFileParams>(value),
        "FsReadFileResponse" => again::<FsReadFileResponse>(value),
        "FsRemoveParams" => again::<FsRemoveParams>(value),
        "FsRemoveResponse" => again::<FsRemoveResponse>(value),
        "FsUnwatchParams" => again::<FsUnwatchParams>(value),
        "FsUnwatchResponse" => again::<FsUnwatchResponse>(value),
        "FsWatchParams" => again::<FsWatchParams>(value),
        "FsWatchResponse" => again::<FsWatchResponse>(value),
        "FsWriteFileParams" => again::<FsWriteFileParams>(value),
        "FsWriteFileResponse" => again::<FsWriteFileResponse>(value),
        "FunctionCallOutputBody" => again::<FunctionCallOutputBody>(value),
        "FunctionCallOutputContentItem" => again::<FunctionCallOutputContentItem>(value),
        "FunctionCallOutputContentItemInputImageKind" => again::<FunctionCallOutputContentItemInputImageKind>(value),
        "GitInfo" => again::<GitInfo>(value),
        "GrantedPermissionProfile" => again::<GrantedPermissionProfile>(value),
        "HookErrorInfo" => again::<HookErrorInfo>(value),
        "HookEventName" => again::<HookEventName>(value),
        "HookMetadata" => again::<HookMetadata>(value),
        "HookMetadataKind" => again::<HookMetadataKind>(value),
        "HookPromptFragment" => again::<HookPromptFragment>(value),
        "HookSource" => again::<HookSource>(value),
        "HookTrustStatus" => again::<HookTrustStatus>(value),
        "HooksListEntry" => again::<HooksListEntry>(value),
        "HooksListParams" => again::<HooksListParams>(value),
        "HooksListResponse" => again::<HooksListResponse>(value),
        "ImageDetail" => again::<ImageDetail>(value),
        "ImageGenerationFailure" => again::<ImageGenerationFailure>(value),
        "ImageGenerationInputImage" => again::<ImageGenerationInputImage>(value),
        "InitializeCapabilities" => again::<InitializeCapabilities>(value),
        "InitializeParams" => again::<InitializeParams>(value),
        "InitializeResponse" => again::<InitializeResponse>(value),
        "InputModality" => again::<InputModality>(value),
        "ItemCompletedNotification" => again::<ItemCompletedNotification>(value),
        "ItemStartedNotification" => again::<ItemStartedNotification>(value),
        "LegacyAppPathString" => again::<LegacyAppPathString>(value),
        "ListMcpServerStatusParams" => again::<ListMcpServerStatusParams>(value),
        "ListMcpServerStatusResponse" => again::<ListMcpServerStatusResponse>(value),
        "McpAppDisplayMode" => again::<McpAppDisplayMode>(value),
        "McpAppUi" => again::<McpAppUi>(value),
        "McpAuthStatus" => again::<McpAuthStatus>(value),
        "McpServerConnectionStatus" => again::<McpServerConnectionStatus>(value),
        "McpServerInfo" => again::<McpServerInfo>(value),
        "McpServerStatus" => again::<McpServerStatus>(value),
        "McpServerStatusDetail" => again::<McpServerStatusDetail>(value),
        "McpToolCallAppContext" => again::<McpToolCallAppContext>(value),
        "McpToolCallError" => again::<McpToolCallError>(value),
        "McpToolCallProgressNotification" => again::<McpToolCallProgressNotification>(value),
        "McpToolCallResult" => again::<McpToolCallResult>(value),
        "McpToolCallStatus" => again::<McpToolCallStatus>(value),
        "MemoryCitation" => again::<MemoryCitation>(value),
        "MemoryCitationEntry" => again::<MemoryCitationEntry>(value),
        "MergeStrategy" => again::<MergeStrategy>(value),
        "MessagePhase" => again::<MessagePhase>(value),
        "MisalignmentErrorDetails" => again::<MisalignmentErrorDetails>(value),
        "MisalignmentSteer" => again::<MisalignmentSteer>(value),
        "ModeKind" => again::<ModeKind>(value),
        "Model" => again::<Model>(value),
        "ModelAccessPrograms" => again::<ModelAccessPrograms>(value),
        "ModelAvailabilityNux" => again::<ModelAvailabilityNux>(value),
        "ModelListParams" => again::<ModelListParams>(value),
        "ModelListResponse" => again::<ModelListResponse>(value),
        "ModelServiceTier" => again::<ModelServiceTier>(value),
        "ModelUpgradeInfo" => again::<ModelUpgradeInfo>(value),
        "MultiAgentVersion" => again::<MultiAgentVersion>(value),
        "NetworkAccess" => again::<NetworkAccess>(value),
        "NetworkApprovalContext" => again::<NetworkApprovalContext>(value),
        "NetworkApprovalProtocol" => again::<NetworkApprovalProtocol>(value),
        "NetworkPolicyAmendment" => again::<NetworkPolicyAmendment>(value),
        "NetworkPolicyRuleAction" => again::<NetworkPolicyRuleAction>(value),
        "NonSteerableTurnKind" => again::<NonSteerableTurnKind>(value),
        "OverriddenMetadata" => again::<OverriddenMetadata>(value),
        "PatchApplyStatus" => again::<PatchApplyStatus>(value),
        "PatchChangeKind" => again::<PatchChangeKind>(value),
        "PermissionGrantScope" => again::<PermissionGrantScope>(value),
        "PermissionsRequestApprovalParams" => again::<PermissionsRequestApprovalParams>(value),
        "PermissionsRequestApprovalResponse" => again::<PermissionsRequestApprovalResponse>(value),
        "Personality" => again::<Personality>(value),
        "PlanDeltaNotification" => again::<PlanDeltaNotification>(value),
        "ReasoningEffort" => again::<ReasoningEffort>(value),
        "ReasoningEffortOption" => again::<ReasoningEffortOption>(value),
        "ReasoningSummary" => again::<ReasoningSummary>(value),
        "ReasoningSummaryPartAddedNotification" => again::<ReasoningSummaryPartAddedNotification>(value),
        "ReasoningSummaryTextDeltaNotification" => again::<ReasoningSummaryTextDeltaNotification>(value),
        "ReasoningTextDeltaNotification" => again::<ReasoningTextDeltaNotification>(value),
        "RequestId" => again::<RequestId>(value),
        "RequestPermissionProfile" => again::<RequestPermissionProfile>(value),
        "Resource" => again::<Resource>(value),
        "ResourceTemplate" => again::<ResourceTemplate>(value),
        "ReviewDelivery" => again::<ReviewDelivery>(value),
        "ReviewStartParams" => again::<ReviewStartParams>(value),
        "ReviewStartResponse" => again::<ReviewStartResponse>(value),
        "ReviewTarget" => again::<ReviewTarget>(value),
        "SandboxMode" => again::<SandboxMode>(value),
        "SandboxPolicy" => again::<SandboxPolicy>(value),
        "SandboxWorkspaceWrite" => again::<SandboxWorkspaceWrite>(value),
        "ServerRequestResolvedNotification" => again::<ServerRequestResolvedNotification>(value),
        "SessionSource" => again::<SessionSource>(value),
        "Settings" => again::<Settings>(value),
        "SkillDependencies" => again::<SkillDependencies>(value),
        "SkillErrorInfo" => again::<SkillErrorInfo>(value),
        "SkillInterface" => again::<SkillInterface>(value),
        "SkillMetadata" => again::<SkillMetadata>(value),
        "SkillScope" => again::<SkillScope>(value),
        "SkillToolDependency" => again::<SkillToolDependency>(value),
        "SkillsChangedNotification" => again::<SkillsChangedNotification>(value),
        "SkillsListEntry" => again::<SkillsListEntry>(value),
        "SkillsListParams" => again::<SkillsListParams>(value),
        "SkillsListResponse" => again::<SkillsListResponse>(value),
        "SortDirection" => again::<SortDirection>(value),
        "SubAgentActivityKind" => again::<SubAgentActivityKind>(value),
        "SubAgentSource" => again::<SubAgentSource>(value),
        "TerminalInteractionNotification" => again::<TerminalInteractionNotification>(value),
        "TextElement" => again::<TextElement>(value),
        "Thread" => again::<Thread>(value),
        "ThreadActiveFlag" => again::<ThreadActiveFlag>(value),
        "ThreadArchiveParams" => again::<ThreadArchiveParams>(value),
        "ThreadArchiveResponse" => again::<ThreadArchiveResponse>(value),
        "ThreadArchivedNotification" => again::<ThreadArchivedNotification>(value),
        "ThreadClosedNotification" => again::<ThreadClosedNotification>(value),
        "ThreadCompactStartParams" => again::<ThreadCompactStartParams>(value),
        "ThreadCompactStartResponse" => again::<ThreadCompactStartResponse>(value),
        "ThreadDeleteParams" => again::<ThreadDeleteParams>(value),
        "ThreadDeleteResponse" => again::<ThreadDeleteResponse>(value),
        "ThreadDeletedNotification" => again::<ThreadDeletedNotification>(value),
        "ThreadExecutor" => again::<ThreadExecutor>(value),
        "ThreadForkParams" => again::<ThreadForkParams>(value),
        "ThreadForkResponse" => again::<ThreadForkResponse>(value),
        "ThreadGoal" => again::<ThreadGoal>(value),
        "ThreadGoalClearParams" => again::<ThreadGoalClearParams>(value),
        "ThreadGoalClearResponse" => again::<ThreadGoalClearResponse>(value),
        "ThreadGoalClearedNotification" => again::<ThreadGoalClearedNotification>(value),
        "ThreadGoalGetParams" => again::<ThreadGoalGetParams>(value),
        "ThreadGoalGetResponse" => again::<ThreadGoalGetResponse>(value),
        "ThreadGoalMutationOrigin" => again::<ThreadGoalMutationOrigin>(value),
        "ThreadGoalSetParams" => again::<ThreadGoalSetParams>(value),
        "ThreadGoalSetResponse" => again::<ThreadGoalSetResponse>(value),
        "ThreadGoalStatus" => again::<ThreadGoalStatus>(value),
        "ThreadGoalUpdatedNotification" => again::<ThreadGoalUpdatedNotification>(value),
        "ThreadHistoryMode" => again::<ThreadHistoryMode>(value),
        "ThreadId" => again::<ThreadId>(value),
        "ThreadItem" => again::<ThreadItem>(value),
        "ThreadItemEntry" => again::<ThreadItemEntry>(value),
        "ThreadItemKind" => again::<ThreadItemKind>(value),
        "ThreadItemKindImageGenerationKind" => again::<ThreadItemKindImageGenerationKind>(value),
        "ThreadItemsListAnchor" => again::<ThreadItemsListAnchor>(value),
        "ThreadItemsListCursor" => again::<ThreadItemsListCursor>(value),
        "ThreadItemsListParams" => again::<ThreadItemsListParams>(value),
        "ThreadItemsListResponse" => again::<ThreadItemsListResponse>(value),
        "ThreadListCwdFilter" => again::<ThreadListCwdFilter>(value),
        "ThreadListParams" => again::<ThreadListParams>(value),
        "ThreadListResponse" => again::<ThreadListResponse>(value),
        "ThreadLoadedListParams" => again::<ThreadLoadedListParams>(value),
        "ThreadLoadedListResponse" => again::<ThreadLoadedListResponse>(value),
        "ThreadNameUpdatedNotification" => again::<ThreadNameUpdatedNotification>(value),
        "ThreadPlacement" => again::<ThreadPlacement>(value),
        "ThreadReadParams" => again::<ThreadReadParams>(value),
        "ThreadReadResponse" => again::<ThreadReadResponse>(value),
        "ThreadResumeParams" => again::<ThreadResumeParams>(value),
        "ThreadResumeResponse" => again::<ThreadResumeResponse>(value),
        "ThreadRevertParams" => again::<ThreadRevertParams>(value),
        "ThreadRevertResponse" => again::<ThreadRevertResponse>(value),
        "ThreadRevertedNotification" => again::<ThreadRevertedNotification>(value),
        "ThreadSection" => again::<ThreadSection>(value),
        "ThreadSectionAppearance" => again::<ThreadSectionAppearance>(value),
        "ThreadSetNameParams" => again::<ThreadSetNameParams>(value),
        "ThreadSetNameResponse" => again::<ThreadSetNameResponse>(value),
        "ThreadShellCommandParams" => again::<ThreadShellCommandParams>(value),
        "ThreadShellCommandResponse" => again::<ThreadShellCommandResponse>(value),
        "ThreadSortKey" => again::<ThreadSortKey>(value),
        "ThreadSource" => again::<ThreadSource>(value),
        "ThreadSourceKind" => again::<ThreadSourceKind>(value),
        "ThreadStartParams" => again::<ThreadStartParams>(value),
        "ThreadStartResponse" => again::<ThreadStartResponse>(value),
        "ThreadStartSource" => again::<ThreadStartSource>(value),
        "ThreadStartedNotification" => again::<ThreadStartedNotification>(value),
        "ThreadStatus" => again::<ThreadStatus>(value),
        "ThreadStatusChangedNotification" => again::<ThreadStatusChangedNotification>(value),
        "ThreadTokenUsage" => again::<ThreadTokenUsage>(value),
        "ThreadTokenUsageUpdatedNotification" => again::<ThreadTokenUsageUpdatedNotification>(value),
        "ThreadTurnsListParams" => again::<ThreadTurnsListParams>(value),
        "ThreadTurnsListResponse" => again::<ThreadTurnsListResponse>(value),
        "ThreadUnarchiveParams" => again::<ThreadUnarchiveParams>(value),
        "ThreadUnarchiveResponse" => again::<ThreadUnarchiveResponse>(value),
        "ThreadUnarchivedNotification" => again::<ThreadUnarchivedNotification>(value),
        "ThreadUnsubscribeParams" => again::<ThreadUnsubscribeParams>(value),
        "ThreadUnsubscribeResponse" => again::<ThreadUnsubscribeResponse>(value),
        "ThreadUnsubscribeStatus" => again::<ThreadUnsubscribeStatus>(value),
        "TokenUsageBreakdown" => again::<TokenUsageBreakdown>(value),
        "Tool" => again::<Tool>(value),
        "ToolRequestUserInputAnswer" => again::<ToolRequestUserInputAnswer>(value),
        "ToolRequestUserInputOption" => again::<ToolRequestUserInputOption>(value),
        "ToolRequestUserInputParams" => again::<ToolRequestUserInputParams>(value),
        "ToolRequestUserInputQuestion" => again::<ToolRequestUserInputQuestion>(value),
        "ToolRequestUserInputResponse" => again::<ToolRequestUserInputResponse>(value),
        "ToolsV2" => again::<ToolsV2>(value),
        "Turn" => again::<Turn>(value),
        "TurnCompletedNotification" => again::<TurnCompletedNotification>(value),
        "TurnDiffUpdatedNotification" => again::<TurnDiffUpdatedNotification>(value),
        "TurnError" => again::<TurnError>(value),
        "TurnInterruptParams" => again::<TurnInterruptParams>(value),
        "TurnInterruptResponse" => again::<TurnInterruptResponse>(value),
        "TurnItemsView" => again::<TurnItemsView>(value),
        "TurnPlanStep" => again::<TurnPlanStep>(value),
        "TurnPlanStepStatus" => again::<TurnPlanStepStatus>(value),
        "TurnPlanUpdatedNotification" => again::<TurnPlanUpdatedNotification>(value),
        "TurnStartParams" => again::<TurnStartParams>(value),
        "TurnStartResponse" => again::<TurnStartResponse>(value),
        "TurnStartedNotification" => again::<TurnStartedNotification>(value),
        "TurnStatus" => again::<TurnStatus>(value),
        "TurnSteerParams" => again::<TurnSteerParams>(value),
        "TurnSteerResponse" => again::<TurnSteerResponse>(value),
        "TurnToolOutput" => again::<TurnToolOutput>(value),
        "UserInput" => again::<UserInput>(value),
        "UserInputImageKind" => again::<UserInputImageKind>(value),
        "Verbosity" => again::<Verbosity>(value),
        "WebSearchAction" => again::<WebSearchAction>(value),
        "WebSearchContextSize" => again::<WebSearchContextSize>(value),
        "WebSearchLocation" => again::<WebSearchLocation>(value),
        "WebSearchMode" => again::<WebSearchMode>(value),
        "WebSearchToolConfig" => again::<WebSearchToolConfig>(value),
        "WriteStatus" => again::<WriteStatus>(value),
        _ => return None,
    })
}
