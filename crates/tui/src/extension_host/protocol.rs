//! Extension-host protocol v1 — the phase-1 subset, and its source of truth.
//!
//! Frame: 4-byte magic `CWX1`, u32 little-endian payload length, UTF-8 JSON.
//! Envelope: JSON-RPC 2.0. A length prefix (not NDJSON) makes a plugin that
//! writes raw bytes to the channel detectable: bad magic or length ends the
//! host instead of desynchronising it.
//!
//! Host→core types use `deny_unknown_fields` — host output is untrusted
//! input. Core→host types are what this side writes.
//!
//! **This file is the protocol's single source.** [`METHODS`] is every method
//! either side may send; both parsers admit nothing else. The TypeScript
//! side's constants, method table, params shapes and wire types are generated
//! from the types here into `crates/tui/extension-host/src/protocol.generated.ts`
//! by `protocol/tests.rs`, which fails when the committed file drifts. What
//! stays hand-written in `protocol.ts`: the frame codec, the JSON-RPC envelope
//! checks, and the one rule [`parse_host_message`] applies beyond the types
//! (`host/hello`'s runtime name). Both sides also parse the shared corpus in
//! `tests/fixtures/extension_host/protocol`. Known limit: result shapes are
//! generated as TypeScript types only; the host does not validate the
//! results the core sends it, and the core validates what it reads.
//!
//! **Tiers.** Each [`MethodSpec`] carries the trust tiers (`HostTier`) that may
//! send or be sent it, and both parsers and the sender enforce that: a
//! plugin-tier host is never sent, and a plugin-tier host's frame is never
//! admitted for, a method reserved for the built-in tier. Every method allows
//! both tiers today ([`ALL_TIERS`]); a reserved method is a row written as
//! `MethodSpec { tiers: &[HostTier::Builtin], ..row(direction, name, request) }`.
//! `host/hello` carries the tier the host serves and the digests of the built-in
//! modules its bundle embeds, which the core checks against what it launched
//! (`supervisor::check_hello_identity`).
//!
//! There is deliberately no method that expresses approval. Registrations
//! are admitted or refused and tool calls flow core→host after the gate. The
//! builtin-only broker methods redeem single-use exact Rust-issued operation
//! tickets; they expose no launch command, credential or decision key. `command/run`
//! flows core→host only when the user invokes the command themselves; the
//! host answers with text or a prompt, and the core decides whether and how
//! to show or submit it. The authority lint in `protocol/tests.rs` keeps
//! [`METHODS`] that way.

use codewhale_config::{EntryRef, OwnerRef};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::tier::HostTier;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAGIC: [u8; 4] = *b"CWX1";
pub const HEADER_LEN: usize = 8;
/// Enough for base64 screenshots later; anything larger is refused, never truncated.
pub const MAX_FRAME: usize = 32 * 1024 * 1024;
/// Requests in flight per direction.
pub const MAX_INFLIGHT: usize = 256;

/// JSON-RPC error codes used on this channel.
pub mod error_code {
    /// A method the receiver has no handler for (the core never sends one the
    /// host's tier may not receive; see `MethodSpec::tiers`).
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// Unknown, revoked, or not-yet-active handle.
    pub const NOT_AVAILABLE: i64 = -32001;
    /// Cancelled by `$/cancel`.
    pub const CANCELLED: i64 = -32800;
    /// `core/call`: the core's policy refuses the call (a tool the extension
    /// may not reach, a limit, an invalid ticket), or the host's request is
    /// refused for a reason that is not a person's answer.
    pub const REFUSED: i64 = -32002;
    /// `core/call`: the user declined the approval card for it.
    pub const DENIED: i64 = -32003;

    /// Every code on the channel, by the name the TypeScript side uses. The
    /// core interprets only the three above; the host also answers with the
    /// standard JSON-RPC codes and `ExecutionFailed` (a tool body threw),
    /// which reach the caller as `HostCallError::Rpc`.
    #[cfg(test)]
    pub const ALL: &[(&str, i64)] = &[
        ("ParseError", -32700),
        ("InvalidRequest", -32600),
        ("MethodNotFound", METHOD_NOT_FOUND),
        ("InvalidParams", INVALID_PARAMS),
        ("Internal", -32603),
        ("ExecutionFailed", -32000),
        ("NotAvailable", NOT_AVAILABLE),
        ("Refused", REFUSED),
        ("Denied", DENIED),
        ("Cancelled", CANCELLED),
    ];
}

/// Which side sends a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    CoreToHost,
    HostToCore,
}

impl Direction {
    /// The corpus and TypeScript spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreToHost => "core_to_host",
            Self::HostToCore => "host_to_core",
        }
    }
}

/// One method either side may send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub direction: Direction,
    /// A request carries an id and gets a response; a notification does not.
    pub request: bool,
    /// The trust tiers whose host may send (host to core) or be sent (core to
    /// host) this method. Enforced in both directions: [`admit_in`] on what is
    /// received, [`allowed_on`] before the core sends.
    pub tiers: &'static [HostTier],
}

/// Both trust tiers: what a method allows unless it is reserved.
pub const ALL_TIERS: &[HostTier] = &HostTier::ALL;

/// A method open to both tiers. Reserve one for a tier with struct update:
/// `MethodSpec { tiers: &[HostTier::Builtin], ..row(..) }`.
const fn row(direction: Direction, name: &'static str, request: bool) -> MethodSpec {
    MethodSpec {
        name,
        direction,
        request,
        tiers: ALL_TIERS,
    }
}

/// The whole protocol surface. A method missing here is refused by both
/// parsers; a method added here must pass the authority lint.
pub const METHODS: &[MethodSpec] = &[
    row(Direction::CoreToHost, "host/initialize", true),
    row(Direction::CoreToHost, "host/ping", true),
    row(Direction::CoreToHost, "host/shutdown", true),
    row(Direction::CoreToHost, "ext/activate", true),
    row(Direction::CoreToHost, "ext/deactivate", true),
    row(Direction::CoreToHost, "tool/call", true),
    row(Direction::CoreToHost, "command/run", true),
    row(Direction::CoreToHost, "hook/evaluate", true),
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::CoreToHost, "mcp/open", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::CoreToHost, "mcp/request", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::CoreToHost, "mcp/close", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::CoreToHost, "harness/run", true)
    },
    row(Direction::CoreToHost, "$/cancel", false),
    row(Direction::HostToCore, "host/hello", false),
    row(Direction::HostToCore, "host/ready", false),
    row(Direction::HostToCore, "registry/register", true),
    row(Direction::HostToCore, "registry/unregister", true),
    row(Direction::HostToCore, "core/call", true),
    row(Direction::HostToCore, "session/checkpoint", true),
    row(Direction::HostToCore, "user-questions/ask", true),
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "proc/launch", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "proc/read", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "proc/write", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "proc/close", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "net/start", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "net/fetch", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "net/read", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "net/release", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "net/close", true)
    },
    MethodSpec {
        tiers: &[HostTier::Builtin],
        ..row(Direction::HostToCore, "exec/redeem", true)
    },
    row(Direction::HostToCore, "ext/faulted", false),
    row(Direction::HostToCore, "log", false),
    row(Direction::HostToCore, "$/cancel", false),
];

/// Admit `method` travelling in `direction` from [`METHODS`]: anything not
/// in the table is refused, a request must carry an id and a notification
/// must not. Returns the id.
fn admit(
    direction: Direction,
    method: &str,
    id: Option<u64>,
    tier: HostTier,
) -> Result<Option<u64>, ProtocolError> {
    admit_in(METHODS, direction, method, id, tier)
}

/// [`admit`] over `table`, so the tier rule is testable with a reserved row
/// that production does not have.
fn admit_in(
    table: &[MethodSpec],
    direction: Direction,
    method: &str,
    id: Option<u64>,
    tier: HostTier,
) -> Result<Option<u64>, ProtocolError> {
    let spec = table
        .iter()
        .find(|spec| spec.direction == direction && spec.name == method)
        .ok_or_else(|| perr(format!("unknown {} method `{method}`", direction.as_str())))?;
    if !spec.tiers.contains(&tier) {
        return Err(perr(format!(
            "`{method}` is not allowed on the {} tier",
            tier.name()
        )));
    }
    match (spec.request, id) {
        (true, Some(_)) | (false, None) => Ok(id),
        (true, None) => Err(perr(format!("`{method}` must be a request (with id)"))),
        (false, Some(_)) => Err(perr(format!("`{method}` must be a notification (no id)"))),
    }
}

/// Whether a host of `tier` may be sent (core to host) or send (host to core)
/// `method`. A method not in [`METHODS`] is not allowed.
#[must_use]
pub fn allowed_on(direction: Direction, method: &str, tier: HostTier) -> bool {
    allowed_in(METHODS, direction, method, tier)
}

fn allowed_in(table: &[MethodSpec], direction: Direction, method: &str, tier: HostTier) -> bool {
    table.iter().any(|spec| {
        spec.direction == direction && spec.name == method && spec.tiers.contains(&tier)
    })
}

fn undecoded(method: &str) -> ProtocolError {
    perr(format!(
        "`{method}` is in the method table but has no decoder"
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("bad frame magic")]
    BadMagic,
    #[error("frame of {0} bytes exceeds MAX_FRAME")]
    TooLarge(usize),
    #[error("frame payload is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("channel read failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolError(pub String);

fn perr(message: impl Into<String>) -> ProtocolError {
    ProtocolError(message.into())
}

/// Encode one message into a `CWX1` frame.
pub fn encode_frame(message: &Value) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(message)?;
    if payload.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Read one frame. `Ok(None)` is a clean EOF at a frame boundary. The payload
/// buffer is allocated only after the length is checked against `MAX_FRAME`.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Value>, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        let read = reader.read(&mut header[filled..]).await?;
        if read == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(FrameError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        filled += read;
    }
    if header[..4] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if length > MAX_FRAME {
        return Err(FrameError::TooLarge(length));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

/// `Some(value)` even for an explicit JSON `null`, so `"result": null` is a
/// present result and round-trips.
fn present_entry<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<EntryRef>, D::Error> {
    EntryRef::deserialize(deserializer).map(Some)
}
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RpcErrorWire {
    pub code: i64,
    pub message: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// Host → core (strict)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HelloParams {
    pub protocol: ProtocolRange,
    pub host_version: String,
    pub bundle_sha256: String,
    /// The runtime actually running the host. Under Bun this comes from
    /// `process.versions.bun`, not the Node version Bun emulates.
    pub runtime: HelloRuntime,
    /// The trust tier this host process serves (`--tier=`). The core refuses a
    /// host that reports any tier but the one it launched.
    pub tier: HostTier,
    /// The SHA-256 of every built-in module source this host build embeds
    /// (`dist/builtin-modules.json`), one row per module, in id order. The
    /// core refuses a host whose list is not the one its own table pins.
    pub builtin_modules: Vec<ModuleDigestWire>,
    /// A kernel memory limit the host applied to itself before loading any
    /// plugin, in MiB: macOS + Bun, when the core asked for one
    /// (`supervisor::MemoryEnforcement::Jetsam`). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_mib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HelloRuntime {
    /// `bun` or `node`.
    pub name: String,
    pub version: String,
}

/// One built-in module's pinned source digest, as the host build records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ModuleDigestWire {
    pub id: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EmptyParams {}

/// What a registration is: a `tool` the model calls (`tool/call`), or a slash
/// `command` the user runs (`/name args`, `command/run`). No variant docs:
/// schemars would then render the enum as a mix the generator does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RegisterKind {
    Tool,
    Command,
    Hook,
    PromptSection,
    PromptTemplate,
    SkillRoot,
    AvatarPack,
    ShellHook,
    McpServer,
}

/// What a registration proposes. The fields a kind uses are fixed by
/// [`RegisterParams::check_spec`] (the generated shapes cannot express a
/// per-kind union): a tool has an `input_schema` and no `argument_hint`; a
/// command has no `input_schema` and may have an `argument_hint`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterSpecWire {
    pub name: String,
    pub description: String,
    /// Tools only: the JSON object schema of the call's input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Map<String, Value>>,
    /// Commands only: the argument placeholder shown after the name (for
    /// example `<topic>`). A command with a hint waits in the composer for
    /// arguments; one without runs directly from the palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterParams {
    pub owner: OwnerRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_entry"
    )]
    pub scope: Option<EntryRef>,
    pub kind: RegisterKind,
    pub spec: RegisterSpecWire,
}

impl RegisterParams {
    /// The rule [`RegisterSpecWire`]'s docs state: which spec fields each
    /// kind uses. Applied by [`parse_host_message`] and mirrored in the
    /// host's `validateMessage`; the corpus holds both to it.
    pub fn check_spec(&self) -> Result<(), String> {
        let spec = &self.spec;
        match self.kind {
            RegisterKind::Tool if spec.input_schema.is_none() => {
                Err("a tool registration needs `spec.input_schema`".to_string())
            }
            RegisterKind::Tool if spec.argument_hint.is_some() => {
                Err("a tool registration has no `spec.argument_hint`".to_string())
            }
            RegisterKind::Command if spec.input_schema.is_some() => {
                Err("a command registration has no `spec.input_schema`".to_string())
            }
            RegisterKind::Hook | RegisterKind::PromptSection | RegisterKind::PromptTemplate | RegisterKind::SkillRoot | RegisterKind::AvatarPack | RegisterKind::ShellHook | RegisterKind::McpServer
                if spec.input_schema.is_some() || spec.argument_hint.is_some() =>
            {
                Err(
                    "a hook, prompt or skill root registration has no input schema or argument hint"
                        .to_string(),
                )
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UnregisterParams {
    pub owner: OwnerRef,
    pub handle: u64,
}

/// A tool asking the core to run one of the core's tools for it
/// (`core/call`, answered with a [`ToolResultWire`]). The `ticket` is the
/// invocation ticket the core put in this call's `tool/call`; the core checks
/// it against its own row (owner, tier, host generation, method, limits) and
/// the host never decides anything: policy, approval and the approval card's
/// text are the core's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CoreCallParams {
    pub owner: OwnerRef,
    pub ticket: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SessionCheckpointParams {
    pub owner: OwnerRef,
    pub ticket: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UserQuestionParams {
    pub owner: OwnerRef,
    pub ticket: String,
    pub questions: Vec<crate::tools::user_input::UserInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FaultedParams {
    pub owner: OwnerRef,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LogParams {
    pub level: String,
    pub msg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CancelParams {
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HookDispatchWire {
    pub event: String,
    pub dialect: String,
    pub point: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub query: String,
}

// Opaque execution references only. Rust never sends commands, inputs, environment or credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HarnessRunParams {
    pub owner: OwnerRef,
    pub execution_id: String,
    pub ticket: String,
    pub deadline_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookDispatchWire>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExecutionRedeemParams {
    pub owner: OwnerRef,
    pub execution_id: String,
    pub ticket: String,
}

// Tier-0 MCP semantic operations and broker pipe access. Launch details and
// Computer Use keys are Rust-only; every write is an exact single-use grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpOperationGrant {
    pub ticket: String,
    pub operation_id: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_id: Option<String>,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpOpenParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub launch_ticket: String,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discover_grant: Option<McpOperationGrant>,
    pub initialize_grant: McpOperationGrant,
    pub initialized_grant: McpOperationGrant,
    pub client_version: String,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpRequestParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub grant: McpOperationGrant,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpCloseParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProcLaunchParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub ticket: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProcSessionParams {
    pub owner: OwnerRef,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProcWriteParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub frame: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// The SDK can propose framing headers only. Rust adds credentials itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct McpHttpHeaders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_protocol_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NetFetchParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub url: String,
    pub method: String,
    pub headers: McpHttpHeaders,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NetReadParams {
    pub owner: OwnerRef,
    pub session_id: String,
    pub response_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostRequest {
    Register(RegisterParams),
    Unregister(UnregisterParams),
    CoreCall(CoreCallParams),
    SessionCheckpoint(SessionCheckpointParams),
    UserQuestion(UserQuestionParams),
    ExecutionRedeem(ExecutionRedeemParams),
    ProcClose(ProcSessionParams),
    NetStart(ProcLaunchParams),
    NetFetch(NetFetchParams),
    NetRead(NetReadParams),
    NetRelease(NetReadParams),
    NetClose(ProcSessionParams),
    ProcWrite(ProcWriteParams),
    ProcRead(ProcSessionParams),
    ProcLaunch(ProcLaunchParams),
}

impl HostRequest {
    /// The plugin whose revocation cancels this request in flight.
    #[must_use]
    pub fn plugin_id(&self) -> &str {
        match self {
            Self::Register(params) => &params.owner.plugin_id,
            Self::Unregister(params) => &params.owner.plugin_id,
            Self::CoreCall(params) => &params.owner.plugin_id,
            Self::SessionCheckpoint(params) => &params.owner.plugin_id,
            Self::UserQuestion(params) => &params.owner.plugin_id,
            Self::ExecutionRedeem(params) => &params.owner.plugin_id,
            Self::ProcClose(params) => &params.owner.plugin_id,
            Self::NetClose(params) => &params.owner.plugin_id,
            Self::NetRelease(params) => &params.owner.plugin_id,
            Self::NetRead(params) => &params.owner.plugin_id,
            Self::NetFetch(params) => &params.owner.plugin_id,
            Self::NetStart(params) => &params.owner.plugin_id,
            Self::ProcWrite(params) => &params.owner.plugin_id,
            Self::ProcRead(params) => &params.owner.plugin_id,
            Self::ProcLaunch(params) => &params.owner.plugin_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostNotification {
    Hello(HelloParams),
    Ready,
    Faulted(FaultedParams),
    Log(LogParams),
    Cancel(CancelParams),
}

/// One decoded, validated host→core message.
#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    Request {
        id: u64,
        request: HostRequest,
    },
    Notification(HostNotification),
    Response {
        id: u64,
        outcome: Result<Value, RpcErrorWire>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    jsonrpc: String,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default, deserialize_with = "present")]
    params: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorWire>,
}

fn decode_envelope(value: Value) -> Result<Envelope, ProtocolError> {
    let envelope: Envelope =
        serde_json::from_value(value).map_err(|e| perr(format!("envelope: {e}")))?;
    if envelope.jsonrpc != "2.0" {
        return Err(perr("envelope: jsonrpc must be \"2.0\""));
    }
    Ok(envelope)
}

/// Params are always named: an array would otherwise decode positionally
/// into a struct (`[]` into [`EmptyParams`]), which the TypeScript side
/// rejects.
fn params<T: DeserializeOwned>(method: &str, params: Option<Value>) -> Result<T, ProtocolError> {
    let params = params.unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err(perr(format!("{method}: params must be an object")));
    }
    serde_json::from_value(params).map_err(|e| perr(format!("{method}: {e}")))
}

fn decode_response(
    envelope: Envelope,
) -> Result<(u64, Result<Value, RpcErrorWire>), ProtocolError> {
    let id = envelope.id.ok_or_else(|| perr("response: missing id"))?;
    if envelope.params.is_some() {
        return Err(perr("response: unexpected params"));
    }
    match (envelope.result, envelope.error) {
        (Some(result), None) => Ok((id, Ok(result))),
        (None, Some(error)) => Ok((id, Err(error))),
        _ => Err(perr("response: needs exactly one of result or error")),
    }
}

/// Parse and strictly validate one host→core message from a host of `tier`: a
/// method reserved for another tier is refused like an unknown one.
pub fn parse_host_message(value: Value, tier: HostTier) -> Result<HostMessage, ProtocolError> {
    let envelope = decode_envelope(value)?;
    let Some(method) = envelope.method.clone() else {
        let (id, outcome) = decode_response(envelope)?;
        return Ok(HostMessage::Response { id, outcome });
    };
    if envelope.result.is_some() || envelope.error.is_some() {
        return Err(perr(format!(
            "`{method}`: a request carries no result or error"
        )));
    }
    let id = admit(Direction::HostToCore, &method, envelope.id, tier)?;
    let p = envelope.params;
    let message = match (method.as_str(), id) {
        ("registry/register", Some(id)) => {
            let register: RegisterParams = params(&method, p)?;
            register
                .check_spec()
                .map_err(|reason| perr(format!("{method}: {reason}")))?;
            HostMessage::Request {
                id,
                request: HostRequest::Register(register),
            }
        }
        ("registry/unregister", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::Unregister(params(&method, p)?),
        },
        ("user-questions/ask", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::UserQuestion(params(&method, p)?),
        },
        ("session/checkpoint", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::SessionCheckpoint(params(&method, p)?),
        },
        ("core/call", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::CoreCall(params(&method, p)?),
        },
        ("net/start", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::NetStart(params(&method, p)?),
        },
        ("net/fetch", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::NetFetch(params(&method, p)?),
        },
        ("net/read", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::NetRead(params(&method, p)?),
        },
        ("net/release", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::NetRelease(params(&method, p)?),
        },
        ("net/close", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::NetClose(params(&method, p)?),
        },
        ("proc/close", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::ProcClose(params(&method, p)?),
        },
        ("proc/write", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::ProcWrite(params(&method, p)?),
        },
        ("proc/read", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::ProcRead(params(&method, p)?),
        },
        ("exec/redeem", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::ExecutionRedeem(params(&method, p)?),
        },
        ("proc/launch", Some(id)) => HostMessage::Request {
            id,
            request: HostRequest::ProcLaunch(params(&method, p)?),
        },
        ("host/hello", None) => {
            let hello: HelloParams = params(&method, p)?;
            if !matches!(hello.runtime.name.as_str(), "bun" | "node") {
                return Err(perr(format!(
                    "host/hello.runtime.name: unknown runtime `{}`",
                    hello.runtime.name
                )));
            }
            HostMessage::Notification(HostNotification::Hello(hello))
        }
        ("host/ready", None) => {
            let _: EmptyParams = params(&method, p)?;
            HostMessage::Notification(HostNotification::Ready)
        }
        ("ext/faulted", None) => {
            HostMessage::Notification(HostNotification::Faulted(params(&method, p)?))
        }
        ("log", None) => HostMessage::Notification(HostNotification::Log(params(&method, p)?)),
        ("$/cancel", None) => {
            HostMessage::Notification(HostNotification::Cancel(params(&method, p)?))
        }
        _ => return Err(undecoded(&method)),
    };
    Ok(message)
}

fn request_value(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn notification_value(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

pub fn response_value(id: u64, outcome: &Result<Value, RpcErrorWire>) -> Value {
    match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

#[cfg(test)]
impl HostMessage {
    /// Re-encode (used by the conformance corpus round-trip).
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Request { id, request } => match request {
                HostRequest::Register(p) => request_value(*id, "registry/register", to_value(p)),
                HostRequest::Unregister(p) => {
                    request_value(*id, "registry/unregister", to_value(p))
                }
                HostRequest::ExecutionRedeem(p) => request_value(*id, "exec/redeem", to_value(p)),
                HostRequest::CoreCall(p) => request_value(*id, "core/call", to_value(p)),
                HostRequest::UserQuestion(p) => {
                    request_value(*id, "user-questions/ask", to_value(p))
                }
                HostRequest::SessionCheckpoint(p) => {
                    request_value(*id, "session/checkpoint", to_value(p))
                }
                HostRequest::ProcClose(p) => request_value(*id, "proc/close", to_value(p)),
                HostRequest::NetStart(p) => request_value(*id, "net/start", to_value(p)),
                HostRequest::NetFetch(p) => request_value(*id, "net/fetch", to_value(p)),
                HostRequest::NetRead(p) => request_value(*id, "net/read", to_value(p)),
                HostRequest::NetRelease(p) => request_value(*id, "net/release", to_value(p)),
                HostRequest::NetClose(p) => request_value(*id, "net/close", to_value(p)),

                HostRequest::ProcWrite(p) => request_value(*id, "proc/write", to_value(p)),
                HostRequest::ProcRead(p) => request_value(*id, "proc/read", to_value(p)),
                HostRequest::ProcLaunch(p) => request_value(*id, "proc/launch", to_value(p)),
            },
            Self::Notification(notification) => match notification {
                HostNotification::Hello(p) => notification_value("host/hello", to_value(p)),
                HostNotification::Ready => notification_value("host/ready", json!({})),
                HostNotification::Faulted(p) => notification_value("ext/faulted", to_value(p)),
                HostNotification::Log(p) => notification_value("log", to_value(p)),
                HostNotification::Cancel(p) => notification_value("$/cancel", to_value(p)),
            },
            Self::Response { id, outcome } => response_value(*id, outcome),
        }
    }
}

// ---------------------------------------------------------------------------
// Core → host (written by this side)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct HostLimits {
    pub max_frame: u64,
    pub max_inflight: u64,
    pub dispose_deadline_ms: u64,
    pub activate_deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InitializeParams {
    pub protocol: u32,
    pub limits: HostLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ActivateParams {
    pub owner: OwnerRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_entry"
    )]
    pub scope: Option<EntryRef>,
    pub plugin_name: String,
    pub entry: EntryRef,
    /// The plugin's settings (`[plugins."<name>".config]`), delivered as the
    /// second argument of `apply`. Always an object; `{}` when none.
    #[serde(default = "empty_object")]
    pub config: Value,
    /// The plugin's own writable directory (read-only string to the plugin).
    /// The same for every entry of one owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeactivateParams {
    pub owner: OwnerRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_entry"
    )]
    pub entry: Option<EntryRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ToolCallParams {
    pub handle: u64,
    pub call_id: String,
    pub input: Value,
    pub deadline_ms: u64,
    /// The workspace of the session the call comes from, and no other. Absent
    /// when its path is not valid UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The invocation ticket: present only when this call runs under the turn
    /// loop's permission gate, and what `core/call` must present. Absent for a
    /// call with no gate (one nested in `execute_tools`, a test), whose tool
    /// then has no way to ask the core for anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_ticket: Option<String>,
    /// Per-invocation identity; never cached at activation or by the host root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_turn_id: Option<String>,
}

/// One user invocation of a registered command. `raw_input` is what follows
/// the command name, trimmed (the core's slash-command parser trims it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CommandRunParams {
    pub handle: u64,
    pub command_id: String,
    pub raw_input: String,
    pub deadline_ms: u64,
    /// The workspace the user ran the command in, and no other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_turn_id: Option<String>,
}

/// A Rust-composed view of a pending call. No session handle or invocation
/// ticket crosses this boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HookCallPayload {
    pub name: String,
    pub call_id: String,
    pub input: Value,
    pub mode: String,
    pub workspace: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HookEvaluateParams {
    pub handle: u64,
    pub event: String,
    pub payload: HookCallPayload,
    pub deadline_ms: u64,
}

/// Monotonic proposals. Rust folds them with native hooks and re-prepares
/// revised input through every policy and approval gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HookVerdictWire {
    Abstain,
    Deny { reason: String },
    Ask { reason: String },
    Annotate { text: String },
    Revise { input: Map<String, Value> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CoreRequest {
    Ping,
    Initialize(InitializeParams),
    /// Production relies on stdin EOF at process exit (the host is shared by
    /// every engine in the process); the bounded shutdown is test-driven.
    #[cfg(test)]
    Shutdown,
    Activate(ActivateParams),
    Deactivate(DeactivateParams),
    ToolCall(ToolCallParams),
    CommandRun(CommandRunParams),
    HookEvaluate(HookEvaluateParams),
    HarnessRun(HarnessRunParams),
    McpClose(McpCloseParams),
    McpRequest(McpRequestParams),
    McpOpen(Box<McpOpenParams>),
}

impl CoreRequest {
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Ping => "host/ping",
            Self::Initialize(_) => "host/initialize",
            #[cfg(test)]
            Self::Shutdown => "host/shutdown",
            Self::Activate(_) => "ext/activate",
            Self::Deactivate(_) => "ext/deactivate",
            Self::ToolCall(_) => "tool/call",
            Self::CommandRun(_) => "command/run",
            Self::HookEvaluate(_) => "hook/evaluate",
            Self::HarnessRun(_) => "harness/run",
            Self::McpClose(_) => "mcp/close",
            Self::McpRequest(_) => "mcp/request",
            Self::McpOpen(_) => "mcp/open",
        }
    }

    #[must_use]
    pub fn params(&self) -> Value {
        match self {
            Self::Ping => json!({}),
            Self::Initialize(p) => to_value(p),
            #[cfg(test)]
            Self::Shutdown => json!({}),
            Self::Activate(p) => to_value(p),
            Self::Deactivate(p) => to_value(p),
            Self::ToolCall(p) => to_value(p),
            Self::CommandRun(p) => to_value(p),
            Self::HookEvaluate(p) => to_value(p),
            Self::HarnessRun(p) => to_value(p),
            Self::McpClose(p) => to_value(p),
            Self::McpRequest(p) => to_value(p),
            Self::McpOpen(p) => to_value(p),
        }
    }

    /// How long the core waits for this request's answer before it sends
    /// `$/cancel`, forgets the call and fails it with
    /// `HostCallError::Timeout` (`HostProcess::call`). The match is
    /// exhaustive, so no request can be added without a deadline.
    ///
    /// | method | deadline | why |
    /// |---|---|---|
    /// | `host/initialize` | `HANDSHAKE_DEADLINE` (30 s) | the whole handshake has the same budget |
    /// | `host/ping` | `PING_DEADLINE` (10 s) | the heartbeat supervises pings with its own `ping_timeout`/`hang_timeout` and kills a silent host; this bounds any other caller |
    /// | `host/shutdown` | 2 s | tests only |
    /// | `ext/activate` | `ACTIVATE_DEADLINE` + 1 s | the host enforces activation's own deadline; 1 s for its answer to arrive |
    /// | `ext/deactivate` | `DISPOSE_DEADLINE` + 500 ms | the same, for disposal |
    /// | `tool/call` | its `deadline_ms` | the host is told the bound the core enforces (`SupervisionOptions::tool_call_deadline`, 120 s) |
    /// | `command/run` | its `deadline_ms` | the same, for a command the user is waiting on (`SupervisionOptions::command_run_deadline`, 30 s) |
    #[must_use]
    pub fn deadline(&self) -> Duration {
        use super::supervisor::{
            ACTIVATE_DEADLINE, DISPOSE_DEADLINE, HANDSHAKE_DEADLINE, PING_DEADLINE,
        };
        match self {
            Self::Ping => PING_DEADLINE,
            Self::Initialize(_) => HANDSHAKE_DEADLINE,
            #[cfg(test)]
            Self::Shutdown => Duration::from_secs(2),
            Self::Activate(_) => ACTIVATE_DEADLINE + Duration::from_secs(1),
            Self::Deactivate(_) => DISPOSE_DEADLINE + Duration::from_millis(500),
            Self::ToolCall(params) => Duration::from_millis(params.deadline_ms),
            Self::CommandRun(params) => Duration::from_millis(params.deadline_ms),
            Self::HookEvaluate(params) => Duration::from_millis(params.deadline_ms),
            Self::HarnessRun(params) => Duration::from_millis(params.deadline_ms),
            Self::McpClose(params) => Duration::from_millis(params.deadline_ms),
            Self::McpRequest(params) => Duration::from_millis(params.deadline_ms),
            Self::McpOpen(params) => Duration::from_millis(params.deadline_ms),
        }
    }

    #[must_use]
    pub fn to_value(&self, id: u64) -> Value {
        request_value(id, self.method(), self.params())
    }
}

/// `registry/register` answer: a handle, or a refusal the host reports as a
/// failed activation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum RegisterResult {
    Admitted { handle: u64 },
    Refused { refused: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActivateResult {
    Ok {
        tools: Vec<String>,
        #[serde(default)]
        commands: Vec<String>,
    },
    Failed {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DeactivateResult {
    pub disposed: bool,
    pub leaked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockWire {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ToolResultWire {
    pub content: Vec<ContentBlockWire>,
    pub is_error: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub structured: Option<Value>,
}

/// A command's answer. The core shows `text` to the user (stripped of
/// terminal escapes and bounded) and, for `submit`, sends `prompt` as the
/// user's next message through the ordinary turn: the model's work after that
/// is gated like any other. `error` is shown as a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandResultWire {
    Success {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Error {
        text: String,
    },
    Submit {
        prompt: String,
        /// An optional note shown to the user alongside the submission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// One core→host message, parsed back (tests and the corpus only).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum CoreMessage {
    Request {
        id: u64,
        request: CoreRequest,
    },
    Cancel(CancelParams),
    Response {
        id: u64,
        outcome: Result<Value, RpcErrorWire>,
    },
}

/// Parse a core→host message (the shape this side writes) bound for a host of
/// `tier`.
#[cfg(test)]
pub fn parse_core_message(value: Value, tier: HostTier) -> Result<CoreMessage, ProtocolError> {
    let envelope = decode_envelope(value)?;
    let Some(method) = envelope.method.clone() else {
        let (id, outcome) = decode_response(envelope)?;
        return Ok(CoreMessage::Response { id, outcome });
    };
    let p = envelope.params;
    let Some(id) = admit(Direction::CoreToHost, &method, envelope.id, tier)? else {
        return match method.as_str() {
            "$/cancel" => Ok(CoreMessage::Cancel(params(&method, p)?)),
            _ => Err(undecoded(&method)),
        };
    };
    let request = match method.as_str() {
        "host/initialize" => CoreRequest::Initialize(params(&method, p)?),
        "host/shutdown" => {
            let _: EmptyParams = params(&method, p)?;
            CoreRequest::Shutdown
        }
        "host/ping" => {
            let _: EmptyParams = params(&method, p)?;
            CoreRequest::Ping
        }
        "ext/activate" => CoreRequest::Activate(params(&method, p)?),
        "ext/deactivate" => CoreRequest::Deactivate(params(&method, p)?),
        "tool/call" => CoreRequest::ToolCall(params(&method, p)?),
        "command/run" => CoreRequest::CommandRun(params(&method, p)?),
        "hook/evaluate" => CoreRequest::HookEvaluate(params(&method, p)?),
        "harness/run" => CoreRequest::HarnessRun(params(&method, p)?),
        "mcp/close" => CoreRequest::McpClose(params(&method, p)?),
        "mcp/request" => CoreRequest::McpRequest(params(&method, p)?),
        "mcp/open" => CoreRequest::McpOpen(params(&method, p)?),
        _ => return Err(undecoded(&method)),
    };
    Ok(CoreMessage::Request { id, request })
}

#[cfg(test)]
impl CoreMessage {
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Request { id, request } => request.to_value(*id),
            Self::Cancel(p) => notification_value("$/cancel", to_value(p)),
            Self::Response { id, outcome } => response_value(*id, outcome),
        }
    }
}

#[must_use]
pub fn cancel_value(id: u64) -> Value {
    notification_value("$/cancel", json!({ "id": id }))
}

#[cfg(test)]
mod tests;
