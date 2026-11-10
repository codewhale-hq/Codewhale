pub const CODEX_PROTOCOL_TAG: &str = "rust-v0.166.0";
pub const CODEX_PROTOCOL_COMMIT: &str = "6ff1e1ca3b30fc8b914720681fcf5fe62e95f856";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    A,
    B,
}

macro_rules! codex_methods {
    ($name:ident { $($variant:ident => $wire:literal: $tier:ident,)* }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)*
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn method(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)*
                }
            }

            pub const fn tier(self) -> Tier {
                match self {
                    $(Self::$variant => Tier::$tier,)*
                }
            }

            pub fn from_method(method: &str) -> Option<Self> {
                match method {
                    $($wire => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.method())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.method())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let method = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::from_method(&method).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown method: {method}"))
                })
            }
        }
    };
}

codex_methods! {
    ClientRequestMethod {
        Initialize => "initialize": A,
        ThreadStart => "thread/start": A,
        ThreadResume => "thread/resume": A,
        ThreadFork => "thread/fork": A,
        ThreadArchive => "thread/archive": A,
        ThreadDelete => "thread/delete": B,
        ThreadUnsubscribe => "thread/unsubscribe": A,
        ThreadSetName => "thread/name/set": A,
        ThreadGoalSet => "thread/goal/set": A,
        ThreadGoalGet => "thread/goal/get": A,
        ThreadGoalClear => "thread/goal/clear": A,
        ThreadUnarchive => "thread/unarchive": A,
        ThreadCompactStart => "thread/compact/start": B,
        ThreadShellCommand => "thread/shellCommand": B,
        ThreadRevert => "thread/revert": B,
        ThreadList => "thread/list": A,
        ThreadLoadedList => "thread/loaded/list": A,
        ThreadRead => "thread/read": A,
        ThreadTurnsList => "thread/turns/list": A,
        ThreadItemsList => "thread/items/list": A,
        SkillsList => "skills/list": B,
        HooksList => "hooks/list": B,
        FsReadFile => "fs/readFile": B,
        FsWriteFile => "fs/writeFile": B,
        FsCreateDirectory => "fs/createDirectory": B,
        FsGetMetadata => "fs/getMetadata": B,
        FsReadDirectory => "fs/readDirectory": B,
        FsRemove => "fs/remove": B,
        FsCopy => "fs/copy": B,
        FsWatch => "fs/watch": B,
        FsUnwatch => "fs/unwatch": B,
        TurnStart => "turn/start": A,
        TurnSteer => "turn/steer": A,
        TurnInterrupt => "turn/interrupt": A,
        ReviewStart => "review/start": B,
        ModelList => "model/list": B,
        McpServerStatusList => "mcpServerStatus/list": B,
        OneOffCommandExec => "command/exec": B,
        CommandExecWrite => "command/exec/write": B,
        CommandExecTerminate => "command/exec/terminate": B,
        CommandExecResize => "command/exec/resize": B,
        ConfigRead => "config/read": B,
        ConfigValueWrite => "config/value/write": B,
        ConfigBatchWrite => "config/batchWrite": B,
    }
}

codex_methods! {
    ClientNotificationMethod {
        Initialized => "initialized": A,
    }
}

codex_methods! {
    ServerRequestMethod {
        CommandExecutionRequestApproval => "item/commandExecution/requestApproval": A,
        FileChangeRequestApproval => "item/fileChange/requestApproval": A,
        ToolRequestUserInput => "item/tool/requestUserInput": A,
        PermissionsRequestApproval => "item/permissions/requestApproval": A,
        DynamicToolCall => "item/tool/call": A,
    }
}

codex_methods! {
    ServerNotificationMethod {
        Error => "error": A,
        ThreadStarted => "thread/started": A,
        ThreadStatusChanged => "thread/status/changed": A,
        ThreadArchived => "thread/archived": A,
        ThreadDeleted => "thread/deleted": B,
        ThreadUnarchived => "thread/unarchived": A,
        ThreadClosed => "thread/closed": A,
        ThreadReverted => "thread/reverted": B,
        SkillsChanged => "skills/changed": B,
        ThreadNameUpdated => "thread/name/updated": A,
        ThreadGoalUpdated => "thread/goal/updated": A,
        ThreadGoalCleared => "thread/goal/cleared": A,
        ThreadTokenUsageUpdated => "thread/tokenUsage/updated": A,
        TurnStarted => "turn/started": A,
        TurnCompleted => "turn/completed": A,
        TurnDiffUpdated => "turn/diff/updated": A,
        TurnPlanUpdated => "turn/plan/updated": A,
        ItemStarted => "item/started": A,
        ItemCompleted => "item/completed": A,
        AgentMessageDelta => "item/agentMessage/delta": A,
        PlanDelta => "item/plan/delta": A,
        CommandExecOutputDelta => "command/exec/outputDelta": B,
        CommandExecutionOutputDelta => "item/commandExecution/outputDelta": A,
        TerminalInteraction => "item/commandExecution/terminalInteraction": A,
        FileChangeOutputDelta => "item/fileChange/outputDelta": A,
        FileChangePatchUpdated => "item/fileChange/patchUpdated": A,
        ServerRequestResolved => "serverRequest/resolved": A,
        McpToolCallProgress => "item/mcpToolCall/progress": A,
        FsChanged => "fs/changed": B,
        ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta": A,
        ReasoningSummaryPartAdded => "item/reasoning/summaryPartAdded": A,
        ReasoningTextDelta => "item/reasoning/textDelta": A,
        ContextCompacted => "thread/compacted": B,
    }
}
