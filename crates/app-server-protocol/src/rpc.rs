use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub use crate::v2::{ThreadExecutor, ThreadPlacement as ThreadEnvironment};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct W3cTraceContext {
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: RequestId,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorResponse {
    pub id: RequestId,
    pub error: RpcErrorDetails,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcErrorDetails {
    pub code: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    pub method: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub params: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcMessage {
    Request(RpcRequest),
    Notification(RpcNotification),
    Response(RpcResponse),
    Error(RpcErrorResponse),
}

impl RpcMessage {
    pub fn into_call(self) -> Option<RpcCall> {
        match self {
            Self::Request(request) => Some(RpcCall {
                id: Some(request.id),
                method: request.method,
                params: request.params.unwrap_or(Value::Null),
                trace: request.trace,
            }),
            Self::Notification(notification) => Some(RpcCall {
                id: None,
                method: notification.method,
                params: notification.params.unwrap_or(Value::Null),
                trace: None,
            }),
            Self::Response(_) | Self::Error(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for RpcCall {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RpcMessage::deserialize(deserializer)?
            .into_call()
            .ok_or_else(|| serde::de::Error::custom("RPC response is not a call"))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadLoadedListParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadLoadedListResponse {
    pub data: Vec<String>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub data: Vec<ThreadListItem>,
    pub next_cursor: Option<String>,
    pub backwards_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadListItem {
    pub id: String,
    pub session_id: String,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: ThreadListStatus,
    pub cwd: String,
    pub cli_version: String,
    pub source: ThreadListSource,
    pub project_id: Option<String>,
    pub turns: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ThreadExecutor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ThreadEnvironment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadListStatus {
    NotLoaded,
    Idle,
    SystemError,
    Active {
        #[serde(rename = "activeFlags")]
        active_flags: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ThreadListSource {
    Cli,
    AppServer,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadStartParams {
    pub operation_key: Option<String>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub cwd: Option<String>,
    pub ephemeral: Option<bool>,
    pub approval_policy: Option<Value>,
    pub approvals_reviewer: Option<Value>,
    pub sandbox: Option<Value>,
    pub config: Option<Value>,
    pub base_instructions: Option<String>,
    pub developer_instructions: Option<String>,
    pub service_name: Option<String>,
    pub service_tier: Option<String>,
    pub session_start_source: Option<Value>,
    pub personality: Option<Value>,
    pub thread_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: ThreadListItem,
    pub model: String,
    pub model_provider: String,
    pub cwd: String,
    pub approval_policy: String,
    pub approvals_reviewer: String,
    pub sandbox: ThreadSandboxPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadSandboxPolicy {
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadReadParams {
    pub thread_id: String,
    #[serde(default)]
    pub include_turns: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResponse {
    pub thread: ThreadListItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurn {
    pub id: String,
    pub items: Vec<ThreadTurnItem>,
    pub items_view: ThreadTurnItemsView,
    pub status: ThreadTurnStatus,
    pub error: Option<ThreadTurnError>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ThreadTurnItemsView {
    Summary,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ThreadTurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadTurnItem {
    UserMessage {
        id: String,
        content: Vec<ThreadUserInput>,
    },
    AgentMessage {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        content: Vec<String>,
    },
    ContextCompaction {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadUserInput {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ThreadSetNameParams {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ThreadSetNameResponse {}
