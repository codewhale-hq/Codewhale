pub const CONTRACT_NAME: &str = "codewhale-codex-app-server-v2";
pub const CONTRACT_VERSION: u32 = 1;
pub const CODEX_PROTOCOL_TAG: &str = "rust-v0.166.0";
pub const CODEX_PROTOCOL_COMMIT: &str = "6ff1e1ca3b30fc8b914720681fcf5fe62e95f856";

pub mod error_code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const OVERLOADED: i64 = -32001;
    pub const THREAD_NOT_FOUND: i64 = -32004;
    pub const RUNTIME_UNAVAILABLE: i64 = -32005;
    pub const CAPABILITY_UNSUPPORTED: i64 = -32020;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    Selected(Tier),
    C,
    Undecided,
}

impl Disposition {
    pub const fn selected(self) -> bool {
        matches!(self, Self::Selected(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClientRequestMethod {
    Initialize,
    ThreadStart,
    ThreadResume,
    ThreadFork,
    ThreadArchive,
    ThreadDelete,
    ThreadUnsubscribe,
    ThreadSetName,
    ThreadGoalSet,
    ThreadGoalGet,
    ThreadGoalClear,
    ThreadUnarchive,
    ThreadCompactStart,
    ThreadShellCommand,
    ThreadRevert,
    ThreadList,
    ThreadLoadedList,
    ThreadRead,
    ThreadTurnsList,
    ThreadItemsList,
    SkillsList,
    HooksList,
    FsReadFile,
    FsWriteFile,
    FsCreateDirectory,
    FsGetMetadata,
    FsReadDirectory,
    FsRemove,
    FsCopy,
    FsWatch,
    FsUnwatch,
    TurnStart,
    TurnSteer,
    TurnInterrupt,
    ReviewStart,
    ModelList,
    McpServerStatusList,
    OneOffCommandExec,
    CommandExecWrite,
    CommandExecTerminate,
    CommandExecResize,
    ConfigRead,
    ConfigValueWrite,
    ConfigBatchWrite,
}

impl ClientRequestMethod {
    pub const ALL: &'static [Self] = &[
        Self::Initialize,
        Self::ThreadStart,
        Self::ThreadResume,
        Self::ThreadFork,
        Self::ThreadArchive,
        Self::ThreadDelete,
        Self::ThreadUnsubscribe,
        Self::ThreadSetName,
        Self::ThreadGoalSet,
        Self::ThreadGoalGet,
        Self::ThreadGoalClear,
        Self::ThreadUnarchive,
        Self::ThreadCompactStart,
        Self::ThreadShellCommand,
        Self::ThreadRevert,
        Self::ThreadList,
        Self::ThreadLoadedList,
        Self::ThreadRead,
        Self::ThreadTurnsList,
        Self::ThreadItemsList,
        Self::SkillsList,
        Self::HooksList,
        Self::FsReadFile,
        Self::FsWriteFile,
        Self::FsCreateDirectory,
        Self::FsGetMetadata,
        Self::FsReadDirectory,
        Self::FsRemove,
        Self::FsCopy,
        Self::FsWatch,
        Self::FsUnwatch,
        Self::TurnStart,
        Self::TurnSteer,
        Self::TurnInterrupt,
        Self::ReviewStart,
        Self::ModelList,
        Self::McpServerStatusList,
        Self::OneOffCommandExec,
        Self::CommandExecWrite,
        Self::CommandExecTerminate,
        Self::CommandExecResize,
        Self::ConfigRead,
        Self::ConfigValueWrite,
        Self::ConfigBatchWrite,
    ];

    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::ThreadFork => "thread/fork",
            Self::ThreadArchive => "thread/archive",
            Self::ThreadDelete => "thread/delete",
            Self::ThreadUnsubscribe => "thread/unsubscribe",
            Self::ThreadSetName => "thread/name/set",
            Self::ThreadGoalSet => "thread/goal/set",
            Self::ThreadGoalGet => "thread/goal/get",
            Self::ThreadGoalClear => "thread/goal/clear",
            Self::ThreadUnarchive => "thread/unarchive",
            Self::ThreadCompactStart => "thread/compact/start",
            Self::ThreadShellCommand => "thread/shellCommand",
            Self::ThreadRevert => "thread/revert",
            Self::ThreadList => "thread/list",
            Self::ThreadLoadedList => "thread/loaded/list",
            Self::ThreadRead => "thread/read",
            Self::ThreadTurnsList => "thread/turns/list",
            Self::ThreadItemsList => "thread/items/list",
            Self::SkillsList => "skills/list",
            Self::HooksList => "hooks/list",
            Self::FsReadFile => "fs/readFile",
            Self::FsWriteFile => "fs/writeFile",
            Self::FsCreateDirectory => "fs/createDirectory",
            Self::FsGetMetadata => "fs/getMetadata",
            Self::FsReadDirectory => "fs/readDirectory",
            Self::FsRemove => "fs/remove",
            Self::FsCopy => "fs/copy",
            Self::FsWatch => "fs/watch",
            Self::FsUnwatch => "fs/unwatch",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
            Self::TurnInterrupt => "turn/interrupt",
            Self::ReviewStart => "review/start",
            Self::ModelList => "model/list",
            Self::McpServerStatusList => "mcpServerStatus/list",
            Self::OneOffCommandExec => "command/exec",
            Self::CommandExecWrite => "command/exec/write",
            Self::CommandExecTerminate => "command/exec/terminate",
            Self::CommandExecResize => "command/exec/resize",
            Self::ConfigRead => "config/read",
            Self::ConfigValueWrite => "config/value/write",
            Self::ConfigBatchWrite => "config/batchWrite",
        }
    }

    pub const fn tier(self) -> Tier {
        match self {
            Self::Initialize => Tier::A,
            Self::ThreadStart => Tier::A,
            Self::ThreadResume => Tier::A,
            Self::ThreadFork => Tier::A,
            Self::ThreadArchive => Tier::A,
            Self::ThreadDelete => Tier::B,
            Self::ThreadUnsubscribe => Tier::A,
            Self::ThreadSetName => Tier::A,
            Self::ThreadGoalSet => Tier::A,
            Self::ThreadGoalGet => Tier::A,
            Self::ThreadGoalClear => Tier::A,
            Self::ThreadUnarchive => Tier::A,
            Self::ThreadCompactStart => Tier::B,
            Self::ThreadShellCommand => Tier::B,
            Self::ThreadRevert => Tier::B,
            Self::ThreadList => Tier::A,
            Self::ThreadLoadedList => Tier::A,
            Self::ThreadRead => Tier::A,
            Self::ThreadTurnsList => Tier::A,
            Self::ThreadItemsList => Tier::A,
            Self::SkillsList => Tier::B,
            Self::HooksList => Tier::B,
            Self::FsReadFile => Tier::B,
            Self::FsWriteFile => Tier::B,
            Self::FsCreateDirectory => Tier::B,
            Self::FsGetMetadata => Tier::B,
            Self::FsReadDirectory => Tier::B,
            Self::FsRemove => Tier::B,
            Self::FsCopy => Tier::B,
            Self::FsWatch => Tier::B,
            Self::FsUnwatch => Tier::B,
            Self::TurnStart => Tier::A,
            Self::TurnSteer => Tier::A,
            Self::TurnInterrupt => Tier::A,
            Self::ReviewStart => Tier::B,
            Self::ModelList => Tier::B,
            Self::McpServerStatusList => Tier::B,
            Self::OneOffCommandExec => Tier::B,
            Self::CommandExecWrite => Tier::B,
            Self::CommandExecTerminate => Tier::B,
            Self::CommandExecResize => Tier::B,
            Self::ConfigRead => Tier::B,
            Self::ConfigValueWrite => Tier::B,
            Self::ConfigBatchWrite => Tier::B,
        }
    }

    pub const fn params_type(self) -> Option<&'static str> {
        match self {
            Self::Initialize => Some("InitializeParams"),
            Self::ThreadStart => Some("ThreadStartParams"),
            Self::ThreadResume => Some("ThreadResumeParams"),
            Self::ThreadFork => Some("ThreadForkParams"),
            Self::ThreadArchive => Some("ThreadArchiveParams"),
            Self::ThreadDelete => Some("ThreadDeleteParams"),
            Self::ThreadUnsubscribe => Some("ThreadUnsubscribeParams"),
            Self::ThreadSetName => Some("ThreadSetNameParams"),
            Self::ThreadGoalSet => Some("ThreadGoalSetParams"),
            Self::ThreadGoalGet => Some("ThreadGoalGetParams"),
            Self::ThreadGoalClear => Some("ThreadGoalClearParams"),
            Self::ThreadUnarchive => Some("ThreadUnarchiveParams"),
            Self::ThreadCompactStart => Some("ThreadCompactStartParams"),
            Self::ThreadShellCommand => Some("ThreadShellCommandParams"),
            Self::ThreadRevert => Some("ThreadRevertParams"),
            Self::ThreadList => Some("ThreadListParams"),
            Self::ThreadLoadedList => Some("ThreadLoadedListParams"),
            Self::ThreadRead => Some("ThreadReadParams"),
            Self::ThreadTurnsList => Some("ThreadTurnsListParams"),
            Self::ThreadItemsList => Some("ThreadItemsListParams"),
            Self::SkillsList => Some("SkillsListParams"),
            Self::HooksList => Some("HooksListParams"),
            Self::FsReadFile => Some("FsReadFileParams"),
            Self::FsWriteFile => Some("FsWriteFileParams"),
            Self::FsCreateDirectory => Some("FsCreateDirectoryParams"),
            Self::FsGetMetadata => Some("FsGetMetadataParams"),
            Self::FsReadDirectory => Some("FsReadDirectoryParams"),
            Self::FsRemove => Some("FsRemoveParams"),
            Self::FsCopy => Some("FsCopyParams"),
            Self::FsWatch => Some("FsWatchParams"),
            Self::FsUnwatch => Some("FsUnwatchParams"),
            Self::TurnStart => Some("TurnStartParams"),
            Self::TurnSteer => Some("TurnSteerParams"),
            Self::TurnInterrupt => Some("TurnInterruptParams"),
            Self::ReviewStart => Some("ReviewStartParams"),
            Self::ModelList => Some("ModelListParams"),
            Self::McpServerStatusList => Some("ListMcpServerStatusParams"),
            Self::OneOffCommandExec => Some("CommandExecParams"),
            Self::CommandExecWrite => Some("CommandExecWriteParams"),
            Self::CommandExecTerminate => Some("CommandExecTerminateParams"),
            Self::CommandExecResize => Some("CommandExecResizeParams"),
            Self::ConfigRead => Some("ConfigReadParams"),
            Self::ConfigValueWrite => Some("ConfigValueWriteParams"),
            Self::ConfigBatchWrite => Some("ConfigBatchWriteParams"),
        }
    }

    pub const fn response_type(self) -> &'static str {
        match self {
            Self::Initialize => "InitializeResponse",
            Self::ThreadStart => "ThreadStartResponse",
            Self::ThreadResume => "ThreadResumeResponse",
            Self::ThreadFork => "ThreadForkResponse",
            Self::ThreadArchive => "ThreadArchiveResponse",
            Self::ThreadDelete => "ThreadDeleteResponse",
            Self::ThreadUnsubscribe => "ThreadUnsubscribeResponse",
            Self::ThreadSetName => "ThreadSetNameResponse",
            Self::ThreadGoalSet => "ThreadGoalSetResponse",
            Self::ThreadGoalGet => "ThreadGoalGetResponse",
            Self::ThreadGoalClear => "ThreadGoalClearResponse",
            Self::ThreadUnarchive => "ThreadUnarchiveResponse",
            Self::ThreadCompactStart => "ThreadCompactStartResponse",
            Self::ThreadShellCommand => "ThreadShellCommandResponse",
            Self::ThreadRevert => "ThreadRevertResponse",
            Self::ThreadList => "ThreadListResponse",
            Self::ThreadLoadedList => "ThreadLoadedListResponse",
            Self::ThreadRead => "ThreadReadResponse",
            Self::ThreadTurnsList => "ThreadTurnsListResponse",
            Self::ThreadItemsList => "ThreadItemsListResponse",
            Self::SkillsList => "SkillsListResponse",
            Self::HooksList => "HooksListResponse",
            Self::FsReadFile => "FsReadFileResponse",
            Self::FsWriteFile => "FsWriteFileResponse",
            Self::FsCreateDirectory => "FsCreateDirectoryResponse",
            Self::FsGetMetadata => "FsGetMetadataResponse",
            Self::FsReadDirectory => "FsReadDirectoryResponse",
            Self::FsRemove => "FsRemoveResponse",
            Self::FsCopy => "FsCopyResponse",
            Self::FsWatch => "FsWatchResponse",
            Self::FsUnwatch => "FsUnwatchResponse",
            Self::TurnStart => "TurnStartResponse",
            Self::TurnSteer => "TurnSteerResponse",
            Self::TurnInterrupt => "TurnInterruptResponse",
            Self::ReviewStart => "ReviewStartResponse",
            Self::ModelList => "ModelListResponse",
            Self::McpServerStatusList => "ListMcpServerStatusResponse",
            Self::OneOffCommandExec => "CommandExecResponse",
            Self::CommandExecWrite => "CommandExecWriteResponse",
            Self::CommandExecTerminate => "CommandExecTerminateResponse",
            Self::CommandExecResize => "CommandExecResizeResponse",
            Self::ConfigRead => "ConfigReadResponse",
            Self::ConfigValueWrite => "ConfigWriteResponse",
            Self::ConfigBatchWrite => "ConfigWriteResponse",
        }
    }

    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "initialize" => Some(Self::Initialize),
            "thread/start" => Some(Self::ThreadStart),
            "thread/resume" => Some(Self::ThreadResume),
            "thread/fork" => Some(Self::ThreadFork),
            "thread/archive" => Some(Self::ThreadArchive),
            "thread/delete" => Some(Self::ThreadDelete),
            "thread/unsubscribe" => Some(Self::ThreadUnsubscribe),
            "thread/name/set" => Some(Self::ThreadSetName),
            "thread/goal/set" => Some(Self::ThreadGoalSet),
            "thread/goal/get" => Some(Self::ThreadGoalGet),
            "thread/goal/clear" => Some(Self::ThreadGoalClear),
            "thread/unarchive" => Some(Self::ThreadUnarchive),
            "thread/compact/start" => Some(Self::ThreadCompactStart),
            "thread/shellCommand" => Some(Self::ThreadShellCommand),
            "thread/revert" => Some(Self::ThreadRevert),
            "thread/list" => Some(Self::ThreadList),
            "thread/loaded/list" => Some(Self::ThreadLoadedList),
            "thread/read" => Some(Self::ThreadRead),
            "thread/turns/list" => Some(Self::ThreadTurnsList),
            "thread/items/list" => Some(Self::ThreadItemsList),
            "skills/list" => Some(Self::SkillsList),
            "hooks/list" => Some(Self::HooksList),
            "fs/readFile" => Some(Self::FsReadFile),
            "fs/writeFile" => Some(Self::FsWriteFile),
            "fs/createDirectory" => Some(Self::FsCreateDirectory),
            "fs/getMetadata" => Some(Self::FsGetMetadata),
            "fs/readDirectory" => Some(Self::FsReadDirectory),
            "fs/remove" => Some(Self::FsRemove),
            "fs/copy" => Some(Self::FsCopy),
            "fs/watch" => Some(Self::FsWatch),
            "fs/unwatch" => Some(Self::FsUnwatch),
            "turn/start" => Some(Self::TurnStart),
            "turn/steer" => Some(Self::TurnSteer),
            "turn/interrupt" => Some(Self::TurnInterrupt),
            "review/start" => Some(Self::ReviewStart),
            "model/list" => Some(Self::ModelList),
            "mcpServerStatus/list" => Some(Self::McpServerStatusList),
            "command/exec" => Some(Self::OneOffCommandExec),
            "command/exec/write" => Some(Self::CommandExecWrite),
            "command/exec/terminate" => Some(Self::CommandExecTerminate),
            "command/exec/resize" => Some(Self::CommandExecResize),
            "config/read" => Some(Self::ConfigRead),
            "config/value/write" => Some(Self::ConfigValueWrite),
            "config/batchWrite" => Some(Self::ConfigBatchWrite),
            _ => None,
        }
    }
}

impl std::fmt::Display for ClientRequestMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.method())
    }
}

impl serde::Serialize for ClientRequestMethod {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.method())
    }
}

impl<'de> serde::Deserialize<'de> for ClientRequestMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_method(&method)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown method: {method}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClientNotificationMethod {
    Initialized,
}

impl ClientNotificationMethod {
    pub const ALL: &'static [Self] = &[
        Self::Initialized,
    ];

    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialized => "initialized",
        }
    }

    pub const fn tier(self) -> Tier {
        match self {
            Self::Initialized => Tier::A,
        }
    }

    pub const fn params_type(self) -> Option<&'static str> {
        match self {
            Self::Initialized => None,
        }
    }

    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "initialized" => Some(Self::Initialized),
            _ => None,
        }
    }
}

impl std::fmt::Display for ClientNotificationMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.method())
    }
}

impl serde::Serialize for ClientNotificationMethod {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.method())
    }
}

impl<'de> serde::Deserialize<'de> for ClientNotificationMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_method(&method)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown method: {method}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServerRequestMethod {
    CommandExecutionRequestApproval,
    FileChangeRequestApproval,
    ToolRequestUserInput,
    PermissionsRequestApproval,
    DynamicToolCall,
}

impl ServerRequestMethod {
    pub const ALL: &'static [Self] = &[
        Self::CommandExecutionRequestApproval,
        Self::FileChangeRequestApproval,
        Self::ToolRequestUserInput,
        Self::PermissionsRequestApproval,
        Self::DynamicToolCall,
    ];

    pub const fn method(self) -> &'static str {
        match self {
            Self::CommandExecutionRequestApproval => "item/commandExecution/requestApproval",
            Self::FileChangeRequestApproval => "item/fileChange/requestApproval",
            Self::ToolRequestUserInput => "item/tool/requestUserInput",
            Self::PermissionsRequestApproval => "item/permissions/requestApproval",
            Self::DynamicToolCall => "item/tool/call",
        }
    }

    pub const fn tier(self) -> Tier {
        match self {
            Self::CommandExecutionRequestApproval => Tier::A,
            Self::FileChangeRequestApproval => Tier::A,
            Self::ToolRequestUserInput => Tier::A,
            Self::PermissionsRequestApproval => Tier::A,
            Self::DynamicToolCall => Tier::A,
        }
    }

    pub const fn params_type(self) -> Option<&'static str> {
        match self {
            Self::CommandExecutionRequestApproval => Some("CommandExecutionRequestApprovalParams"),
            Self::FileChangeRequestApproval => Some("FileChangeRequestApprovalParams"),
            Self::ToolRequestUserInput => Some("ToolRequestUserInputParams"),
            Self::PermissionsRequestApproval => Some("PermissionsRequestApprovalParams"),
            Self::DynamicToolCall => Some("DynamicToolCallParams"),
        }
    }

    pub const fn response_type(self) -> &'static str {
        match self {
            Self::CommandExecutionRequestApproval => "CommandExecutionRequestApprovalResponse",
            Self::FileChangeRequestApproval => "FileChangeRequestApprovalResponse",
            Self::ToolRequestUserInput => "ToolRequestUserInputResponse",
            Self::PermissionsRequestApproval => "PermissionsRequestApprovalResponse",
            Self::DynamicToolCall => "DynamicToolCallResponse",
        }
    }

    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "item/commandExecution/requestApproval" => Some(Self::CommandExecutionRequestApproval),
            "item/fileChange/requestApproval" => Some(Self::FileChangeRequestApproval),
            "item/tool/requestUserInput" => Some(Self::ToolRequestUserInput),
            "item/permissions/requestApproval" => Some(Self::PermissionsRequestApproval),
            "item/tool/call" => Some(Self::DynamicToolCall),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServerRequestMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.method())
    }
}

impl serde::Serialize for ServerRequestMethod {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.method())
    }
}

impl<'de> serde::Deserialize<'de> for ServerRequestMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_method(&method)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown method: {method}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ServerNotificationMethod {
    Error,
    ThreadStarted,
    ThreadStatusChanged,
    ThreadArchived,
    ThreadDeleted,
    ThreadUnarchived,
    ThreadClosed,
    ThreadReverted,
    SkillsChanged,
    ThreadNameUpdated,
    ThreadGoalUpdated,
    ThreadGoalCleared,
    ThreadTokenUsageUpdated,
    TurnStarted,
    TurnCompleted,
    TurnDiffUpdated,
    TurnPlanUpdated,
    ItemStarted,
    ItemCompleted,
    AgentMessageDelta,
    PlanDelta,
    CommandExecOutputDelta,
    CommandExecutionOutputDelta,
    TerminalInteraction,
    FileChangeOutputDelta,
    FileChangePatchUpdated,
    ServerRequestResolved,
    McpToolCallProgress,
    FsChanged,
    ReasoningSummaryTextDelta,
    ReasoningSummaryPartAdded,
    ReasoningTextDelta,
    ContextCompacted,
}

impl ServerNotificationMethod {
    pub const ALL: &'static [Self] = &[
        Self::Error,
        Self::ThreadStarted,
        Self::ThreadStatusChanged,
        Self::ThreadArchived,
        Self::ThreadDeleted,
        Self::ThreadUnarchived,
        Self::ThreadClosed,
        Self::ThreadReverted,
        Self::SkillsChanged,
        Self::ThreadNameUpdated,
        Self::ThreadGoalUpdated,
        Self::ThreadGoalCleared,
        Self::ThreadTokenUsageUpdated,
        Self::TurnStarted,
        Self::TurnCompleted,
        Self::TurnDiffUpdated,
        Self::TurnPlanUpdated,
        Self::ItemStarted,
        Self::ItemCompleted,
        Self::AgentMessageDelta,
        Self::PlanDelta,
        Self::CommandExecOutputDelta,
        Self::CommandExecutionOutputDelta,
        Self::TerminalInteraction,
        Self::FileChangeOutputDelta,
        Self::FileChangePatchUpdated,
        Self::ServerRequestResolved,
        Self::McpToolCallProgress,
        Self::FsChanged,
        Self::ReasoningSummaryTextDelta,
        Self::ReasoningSummaryPartAdded,
        Self::ReasoningTextDelta,
        Self::ContextCompacted,
    ];

    pub const fn method(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::ThreadStarted => "thread/started",
            Self::ThreadStatusChanged => "thread/status/changed",
            Self::ThreadArchived => "thread/archived",
            Self::ThreadDeleted => "thread/deleted",
            Self::ThreadUnarchived => "thread/unarchived",
            Self::ThreadClosed => "thread/closed",
            Self::ThreadReverted => "thread/reverted",
            Self::SkillsChanged => "skills/changed",
            Self::ThreadNameUpdated => "thread/name/updated",
            Self::ThreadGoalUpdated => "thread/goal/updated",
            Self::ThreadGoalCleared => "thread/goal/cleared",
            Self::ThreadTokenUsageUpdated => "thread/tokenUsage/updated",
            Self::TurnStarted => "turn/started",
            Self::TurnCompleted => "turn/completed",
            Self::TurnDiffUpdated => "turn/diff/updated",
            Self::TurnPlanUpdated => "turn/plan/updated",
            Self::ItemStarted => "item/started",
            Self::ItemCompleted => "item/completed",
            Self::AgentMessageDelta => "item/agentMessage/delta",
            Self::PlanDelta => "item/plan/delta",
            Self::CommandExecOutputDelta => "command/exec/outputDelta",
            Self::CommandExecutionOutputDelta => "item/commandExecution/outputDelta",
            Self::TerminalInteraction => "item/commandExecution/terminalInteraction",
            Self::FileChangeOutputDelta => "item/fileChange/outputDelta",
            Self::FileChangePatchUpdated => "item/fileChange/patchUpdated",
            Self::ServerRequestResolved => "serverRequest/resolved",
            Self::McpToolCallProgress => "item/mcpToolCall/progress",
            Self::FsChanged => "fs/changed",
            Self::ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta",
            Self::ReasoningSummaryPartAdded => "item/reasoning/summaryPartAdded",
            Self::ReasoningTextDelta => "item/reasoning/textDelta",
            Self::ContextCompacted => "thread/compacted",
        }
    }

    pub const fn tier(self) -> Tier {
        match self {
            Self::Error => Tier::A,
            Self::ThreadStarted => Tier::A,
            Self::ThreadStatusChanged => Tier::A,
            Self::ThreadArchived => Tier::A,
            Self::ThreadDeleted => Tier::B,
            Self::ThreadUnarchived => Tier::A,
            Self::ThreadClosed => Tier::A,
            Self::ThreadReverted => Tier::B,
            Self::SkillsChanged => Tier::B,
            Self::ThreadNameUpdated => Tier::A,
            Self::ThreadGoalUpdated => Tier::A,
            Self::ThreadGoalCleared => Tier::A,
            Self::ThreadTokenUsageUpdated => Tier::A,
            Self::TurnStarted => Tier::A,
            Self::TurnCompleted => Tier::A,
            Self::TurnDiffUpdated => Tier::A,
            Self::TurnPlanUpdated => Tier::A,
            Self::ItemStarted => Tier::A,
            Self::ItemCompleted => Tier::A,
            Self::AgentMessageDelta => Tier::A,
            Self::PlanDelta => Tier::A,
            Self::CommandExecOutputDelta => Tier::B,
            Self::CommandExecutionOutputDelta => Tier::A,
            Self::TerminalInteraction => Tier::A,
            Self::FileChangeOutputDelta => Tier::A,
            Self::FileChangePatchUpdated => Tier::A,
            Self::ServerRequestResolved => Tier::A,
            Self::McpToolCallProgress => Tier::A,
            Self::FsChanged => Tier::B,
            Self::ReasoningSummaryTextDelta => Tier::A,
            Self::ReasoningSummaryPartAdded => Tier::A,
            Self::ReasoningTextDelta => Tier::A,
            Self::ContextCompacted => Tier::B,
        }
    }

    pub const fn params_type(self) -> Option<&'static str> {
        match self {
            Self::Error => Some("ErrorNotification"),
            Self::ThreadStarted => Some("ThreadStartedNotification"),
            Self::ThreadStatusChanged => Some("ThreadStatusChangedNotification"),
            Self::ThreadArchived => Some("ThreadArchivedNotification"),
            Self::ThreadDeleted => Some("ThreadDeletedNotification"),
            Self::ThreadUnarchived => Some("ThreadUnarchivedNotification"),
            Self::ThreadClosed => Some("ThreadClosedNotification"),
            Self::ThreadReverted => Some("ThreadRevertedNotification"),
            Self::SkillsChanged => Some("SkillsChangedNotification"),
            Self::ThreadNameUpdated => Some("ThreadNameUpdatedNotification"),
            Self::ThreadGoalUpdated => Some("ThreadGoalUpdatedNotification"),
            Self::ThreadGoalCleared => Some("ThreadGoalClearedNotification"),
            Self::ThreadTokenUsageUpdated => Some("ThreadTokenUsageUpdatedNotification"),
            Self::TurnStarted => Some("TurnStartedNotification"),
            Self::TurnCompleted => Some("TurnCompletedNotification"),
            Self::TurnDiffUpdated => Some("TurnDiffUpdatedNotification"),
            Self::TurnPlanUpdated => Some("TurnPlanUpdatedNotification"),
            Self::ItemStarted => Some("ItemStartedNotification"),
            Self::ItemCompleted => Some("ItemCompletedNotification"),
            Self::AgentMessageDelta => Some("AgentMessageDeltaNotification"),
            Self::PlanDelta => Some("PlanDeltaNotification"),
            Self::CommandExecOutputDelta => Some("CommandExecOutputDeltaNotification"),
            Self::CommandExecutionOutputDelta => Some("CommandExecutionOutputDeltaNotification"),
            Self::TerminalInteraction => Some("TerminalInteractionNotification"),
            Self::FileChangeOutputDelta => Some("FileChangeOutputDeltaNotification"),
            Self::FileChangePatchUpdated => Some("FileChangePatchUpdatedNotification"),
            Self::ServerRequestResolved => Some("ServerRequestResolvedNotification"),
            Self::McpToolCallProgress => Some("McpToolCallProgressNotification"),
            Self::FsChanged => Some("FsChangedNotification"),
            Self::ReasoningSummaryTextDelta => Some("ReasoningSummaryTextDeltaNotification"),
            Self::ReasoningSummaryPartAdded => Some("ReasoningSummaryPartAddedNotification"),
            Self::ReasoningTextDelta => Some("ReasoningTextDeltaNotification"),
            Self::ContextCompacted => Some("ContextCompactedNotification"),
        }
    }

    pub fn from_method(method: &str) -> Option<Self> {
        match method {
            "error" => Some(Self::Error),
            "thread/started" => Some(Self::ThreadStarted),
            "thread/status/changed" => Some(Self::ThreadStatusChanged),
            "thread/archived" => Some(Self::ThreadArchived),
            "thread/deleted" => Some(Self::ThreadDeleted),
            "thread/unarchived" => Some(Self::ThreadUnarchived),
            "thread/closed" => Some(Self::ThreadClosed),
            "thread/reverted" => Some(Self::ThreadReverted),
            "skills/changed" => Some(Self::SkillsChanged),
            "thread/name/updated" => Some(Self::ThreadNameUpdated),
            "thread/goal/updated" => Some(Self::ThreadGoalUpdated),
            "thread/goal/cleared" => Some(Self::ThreadGoalCleared),
            "thread/tokenUsage/updated" => Some(Self::ThreadTokenUsageUpdated),
            "turn/started" => Some(Self::TurnStarted),
            "turn/completed" => Some(Self::TurnCompleted),
            "turn/diff/updated" => Some(Self::TurnDiffUpdated),
            "turn/plan/updated" => Some(Self::TurnPlanUpdated),
            "item/started" => Some(Self::ItemStarted),
            "item/completed" => Some(Self::ItemCompleted),
            "item/agentMessage/delta" => Some(Self::AgentMessageDelta),
            "item/plan/delta" => Some(Self::PlanDelta),
            "command/exec/outputDelta" => Some(Self::CommandExecOutputDelta),
            "item/commandExecution/outputDelta" => Some(Self::CommandExecutionOutputDelta),
            "item/commandExecution/terminalInteraction" => Some(Self::TerminalInteraction),
            "item/fileChange/outputDelta" => Some(Self::FileChangeOutputDelta),
            "item/fileChange/patchUpdated" => Some(Self::FileChangePatchUpdated),
            "serverRequest/resolved" => Some(Self::ServerRequestResolved),
            "item/mcpToolCall/progress" => Some(Self::McpToolCallProgress),
            "fs/changed" => Some(Self::FsChanged),
            "item/reasoning/summaryTextDelta" => Some(Self::ReasoningSummaryTextDelta),
            "item/reasoning/summaryPartAdded" => Some(Self::ReasoningSummaryPartAdded),
            "item/reasoning/textDelta" => Some(Self::ReasoningTextDelta),
            "thread/compacted" => Some(Self::ContextCompacted),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServerNotificationMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.method())
    }
}

impl serde::Serialize for ServerNotificationMethod {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.method())
    }
}

impl<'de> serde::Deserialize<'de> for ServerNotificationMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_method(&method)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown method: {method}")))
    }
}

pub const CLIENT_REQUESTS: &[(&str, Disposition)] = &[
    ("initialize", Disposition::Selected(Tier::A)),
    ("thread/start", Disposition::Selected(Tier::A)),
    ("thread/resume", Disposition::Selected(Tier::A)),
    ("thread/fork", Disposition::Selected(Tier::A)),
    ("thread/archive", Disposition::Selected(Tier::A)),
    ("thread/delete", Disposition::Selected(Tier::B)),
    ("thread/unsubscribe", Disposition::Selected(Tier::A)),
    ("thread/name/set", Disposition::Selected(Tier::A)),
    ("thread/goal/set", Disposition::Selected(Tier::A)),
    ("thread/goal/get", Disposition::Selected(Tier::A)),
    ("thread/goal/clear", Disposition::Selected(Tier::A)),
    ("thread/metadata/update", Disposition::Undecided),
    ("thread/attachment/add", Disposition::Undecided),
    ("thread/attachment/list", Disposition::Undecided),
    ("thread/attachmentOwner/list", Disposition::Undecided),
    ("thread/attachment/remove", Disposition::Undecided),
    ("thread/section/move", Disposition::Undecided),
    ("thread/unarchive", Disposition::Selected(Tier::A)),
    ("thread/compact/start", Disposition::Selected(Tier::B)),
    ("thread/shellCommand", Disposition::Selected(Tier::B)),
    ("thread/approveGuardianDeniedAction", Disposition::Undecided),
    ("thread/revert", Disposition::Selected(Tier::B)),
    ("thread/list", Disposition::Selected(Tier::A)),
    ("threadSection/list", Disposition::Undecided),
    ("threadSection/create", Disposition::Undecided),
    ("threadSection/update", Disposition::Undecided),
    ("threadSection/delete", Disposition::Undecided),
    ("thread/loaded/list", Disposition::Selected(Tier::A)),
    ("thread/read", Disposition::Selected(Tier::A)),
    ("thread/turns/list", Disposition::Selected(Tier::A)),
    ("thread/items/list", Disposition::Selected(Tier::A)),
    ("thread/inject_items", Disposition::Undecided),
    ("skills/list", Disposition::Selected(Tier::B)),
    ("skills/extraRoots/set", Disposition::Undecided),
    ("hooks/list", Disposition::Selected(Tier::B)),
    ("marketplace/add", Disposition::Undecided),
    ("marketplace/remove", Disposition::Undecided),
    ("marketplace/upgrade", Disposition::Undecided),
    ("plugin/list", Disposition::Undecided),
    ("plugin/installed", Disposition::Undecided),
    ("plugin/reconcile", Disposition::Undecided),
    ("plugin/read", Disposition::Undecided),
    ("plugin/skill/read", Disposition::Undecided),
    ("plugin/share/save", Disposition::C),
    ("plugin/share/updateTargets", Disposition::C),
    ("plugin/share/list", Disposition::C),
    ("plugin/share/checkout", Disposition::C),
    ("plugin/share/delete", Disposition::C),
    ("app/read", Disposition::C),
    ("app/list", Disposition::C),
    ("app/installed", Disposition::C),
    ("fs/readFile", Disposition::Selected(Tier::B)),
    ("fs/writeFile", Disposition::Selected(Tier::B)),
    ("fs/createDirectory", Disposition::Selected(Tier::B)),
    ("fs/getMetadata", Disposition::Selected(Tier::B)),
    ("fs/readDirectory", Disposition::Selected(Tier::B)),
    ("fs/remove", Disposition::Selected(Tier::B)),
    ("fs/copy", Disposition::Selected(Tier::B)),
    ("fs/watch", Disposition::Selected(Tier::B)),
    ("fs/unwatch", Disposition::Selected(Tier::B)),
    ("skills/config/write", Disposition::Undecided),
    ("plugin/install", Disposition::Undecided),
    ("plugin/uninstall", Disposition::Undecided),
    ("turn/start", Disposition::Selected(Tier::A)),
    ("turn/steer", Disposition::Selected(Tier::A)),
    ("turn/interrupt", Disposition::Selected(Tier::A)),
    ("review/start", Disposition::Selected(Tier::B)),
    ("model/list", Disposition::Selected(Tier::B)),
    ("account/gatewayOAuth/read", Disposition::C),
    ("account/gatewayOAuth/login", Disposition::C),
    ("account/gatewayOAuth/cancel", Disposition::C),
    ("modelProvider/capabilities/read", Disposition::Undecided),
    ("experimentalFeature/list", Disposition::Undecided),
    ("permissionProfile/list", Disposition::Undecided),
    ("experimentalFeature/enablement/set", Disposition::Undecided),
    ("mcpServer/oauth/login", Disposition::Undecided),
    ("config/mcpServer/reload", Disposition::Undecided),
    ("mcpServerStatus/list", Disposition::Selected(Tier::B)),
    ("mcpServer/resource/read", Disposition::Undecided),
    ("mcpServer/tool/call", Disposition::Undecided),
    ("windowsSandbox/setupStart", Disposition::Undecided),
    ("windowsSandbox/readiness", Disposition::Undecided),
    ("account/login/start", Disposition::Undecided),
    ("account/login/cancel", Disposition::Undecided),
    ("account/logout", Disposition::Undecided),
    ("account/rateLimits/read", Disposition::Undecided),
    ("account/rateLimitResetCredit/consume", Disposition::C),
    ("account/usage/read", Disposition::Undecided),
    ("account/workspaceMessages/read", Disposition::C),
    ("account/sendAddCreditsNudgeEmail", Disposition::C),
    ("feedback/upload", Disposition::C),
    ("feedback/save", Disposition::C),
    ("command/exec", Disposition::Selected(Tier::B)),
    ("command/exec/write", Disposition::Selected(Tier::B)),
    ("command/exec/terminate", Disposition::Selected(Tier::B)),
    ("command/exec/resize", Disposition::Selected(Tier::B)),
    ("config/read", Disposition::Selected(Tier::B)),
    ("externalAgentConfig/detect", Disposition::Undecided),
    ("externalAgentConfig/import", Disposition::Undecided),
    ("externalAgentConfig/import/recordHistory", Disposition::Undecided),
    ("externalAgentConfig/import/readHistories", Disposition::Undecided),
    ("config/value/write", Disposition::Selected(Tier::B)),
    ("config/batchWrite", Disposition::Selected(Tier::B)),
    ("configRequirements/read", Disposition::Undecided),
    ("account/read", Disposition::Undecided),
    ("getConversationSummary", Disposition::C),
    ("gitDiffToRemote", Disposition::C),
    ("getAuthStatus", Disposition::C),
    ("fuzzyFileSearch", Disposition::Undecided),
];

pub const CLIENT_NOTIFICATIONS: &[(&str, Disposition)] = &[
    ("initialized", Disposition::Selected(Tier::A)),
];

pub const SERVER_REQUESTS: &[(&str, Disposition)] = &[
    ("item/commandExecution/requestApproval", Disposition::Selected(Tier::A)),
    ("item/fileChange/requestApproval", Disposition::Selected(Tier::A)),
    ("item/tool/requestUserInput", Disposition::Selected(Tier::A)),
    ("mcpServer/elicitation/request", Disposition::Undecided),
    ("item/permissions/requestApproval", Disposition::Selected(Tier::A)),
    ("item/tool/call", Disposition::Selected(Tier::A)),
    ("account/chatgptAuthTokens/refresh", Disposition::C),
    ("attestation/generate", Disposition::C),
    ("applyPatchApproval", Disposition::C),
    ("execCommandApproval", Disposition::C),
];

pub const SERVER_NOTIFICATIONS: &[(&str, Disposition)] = &[
    ("error", Disposition::Selected(Tier::A)),
    ("thread/started", Disposition::Selected(Tier::A)),
    ("thread/status/changed", Disposition::Selected(Tier::A)),
    ("thread/readState/changed", Disposition::Undecided),
    ("thread/archived", Disposition::Selected(Tier::A)),
    ("thread/deleted", Disposition::Selected(Tier::B)),
    ("thread/unarchived", Disposition::Selected(Tier::A)),
    ("thread/closed", Disposition::Selected(Tier::A)),
    ("thread/reverted", Disposition::Selected(Tier::B)),
    ("skills/changed", Disposition::Selected(Tier::B)),
    ("thread/name/updated", Disposition::Selected(Tier::A)),
    ("thread/attachment/updated", Disposition::Undecided),
    ("thread/goal/updated", Disposition::Selected(Tier::A)),
    ("thread/prediction/updated", Disposition::Undecided),
    ("thread/goal/cleared", Disposition::Selected(Tier::A)),
    ("thread/queue/changed", Disposition::Undecided),
    ("project/changed", Disposition::Undecided),
    ("thread/project/updated", Disposition::Undecided),
    ("thread/environment/connected", Disposition::Undecided),
    ("thread/environment/disconnected", Disposition::Undecided),
    ("thread/settings/updated", Disposition::Undecided),
    ("thread/tokenUsage/updated", Disposition::Selected(Tier::A)),
    ("turn/started", Disposition::Selected(Tier::A)),
    ("hook/started", Disposition::Undecided),
    ("turn/completed", Disposition::Selected(Tier::A)),
    ("hook/completed", Disposition::Undecided),
    ("turn/diff/updated", Disposition::Selected(Tier::A)),
    ("turn/plan/updated", Disposition::Selected(Tier::A)),
    ("item/started", Disposition::Selected(Tier::A)),
    ("item/autoApprovalReview/started", Disposition::Undecided),
    ("item/autoApprovalReview/completed", Disposition::Undecided),
    ("autoApprovalReview/strictReviewRequired", Disposition::Undecided),
    ("item/completed", Disposition::Selected(Tier::A)),
    ("rawResponseItem/completed", Disposition::C),
    ("rawResponse/completed", Disposition::C),
    ("item/agentMessage/delta", Disposition::Selected(Tier::A)),
    ("item/plan/delta", Disposition::Selected(Tier::A)),
    ("command/exec/outputDelta", Disposition::Selected(Tier::B)),
    ("process/outputDelta", Disposition::Undecided),
    ("process/exited", Disposition::Undecided),
    ("item/commandExecution/outputDelta", Disposition::Selected(Tier::A)),
    ("item/commandExecution/terminalInteraction", Disposition::Selected(Tier::A)),
    ("item/fileChange/outputDelta", Disposition::Selected(Tier::A)),
    ("item/fileChange/patchUpdated", Disposition::Selected(Tier::A)),
    ("serverRequest/resolved", Disposition::Selected(Tier::A)),
    ("item/mcpToolCall/progress", Disposition::Selected(Tier::A)),
    ("mcpServer/oauthLogin/completed", Disposition::Undecided),
    ("mcpServer/startupStatus/updated", Disposition::Undecided),
    ("mcpServer/event/stream/notification", Disposition::Undecided),
    ("account/updated", Disposition::Undecided),
    ("account/gatewayOAuth/changed", Disposition::C),
    ("account/rateLimits/updated", Disposition::Undecided),
    ("app/list/updated", Disposition::C),
    ("remoteControl/status/changed", Disposition::Undecided),
    ("externalAgentConfig/import/progress", Disposition::Undecided),
    ("externalAgentConfig/import/completed", Disposition::Undecided),
    ("fs/changed", Disposition::Selected(Tier::B)),
    ("item/reasoning/summaryTextDelta", Disposition::Selected(Tier::A)),
    ("item/reasoning/summaryPartAdded", Disposition::Selected(Tier::A)),
    ("item/reasoning/textDelta", Disposition::Selected(Tier::A)),
    ("thread/compacted", Disposition::Selected(Tier::B)),
    ("model/rerouted", Disposition::Undecided),
    ("model/verification", Disposition::Undecided),
    ("modelProvider/authRecoveryStarted", Disposition::Undecided),
    ("modelProvider/authRecoveryCompleted", Disposition::Undecided),
    ("turn/moderationMetadata", Disposition::Undecided),
    ("model/safetyBuffering/updated", Disposition::Undecided),
    ("warning", Disposition::Undecided),
    ("guardianWarning", Disposition::Undecided),
    ("deprecationNotice", Disposition::Undecided),
    ("configWarning", Disposition::Undecided),
    ("fuzzyFileSearch/sessionUpdated", Disposition::Undecided),
    ("fuzzyFileSearch/sessionCompleted", Disposition::Undecided),
    ("thread/realtime/started", Disposition::Undecided),
    ("thread/realtime/itemAdded", Disposition::Undecided),
    ("thread/realtime/item/started", Disposition::Undecided),
    ("thread/realtime/item/transcript/delta", Disposition::Undecided),
    ("thread/realtime/item/completed", Disposition::Undecided),
    ("thread/realtime/transcript/delta", Disposition::Undecided),
    ("thread/realtime/transcript/done", Disposition::Undecided),
    ("thread/realtime/outputAudio/delta", Disposition::Undecided),
    ("thread/realtime/sdp", Disposition::Undecided),
    ("thread/realtime/error", Disposition::Undecided),
    ("thread/realtime/closed", Disposition::Undecided),
    ("windows/worldWritableWarning", Disposition::Undecided),
    ("windowsSandbox/setupCompleted", Disposition::Undecided),
    ("account/login/completed", Disposition::Undecided),
];

fn lookup(table: &[(&str, Disposition)], method: &str) -> Option<Disposition> {
    table
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, disposition)| *disposition)
}

pub fn client_request_disposition(method: &str) -> Option<Disposition> {
    lookup(CLIENT_REQUESTS, method)
}

pub fn client_notification_disposition(method: &str) -> Option<Disposition> {
    lookup(CLIENT_NOTIFICATIONS, method)
}

pub fn server_request_disposition(method: &str) -> Option<Disposition> {
    lookup(SERVER_REQUESTS, method)
}

pub fn server_notification_disposition(method: &str) -> Option<Disposition> {
    lookup(SERVER_NOTIFICATIONS, method)
}

pub fn unselected_request_error(method: &str) -> Option<i64> {
    match client_request_disposition(method) {
        Some(Disposition::Selected(_)) => None,
        _ => Some(error_code::METHOD_NOT_FOUND),
    }
}
