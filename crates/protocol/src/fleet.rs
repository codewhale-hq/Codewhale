//! Agent Fleet control-plane protocol types.
//!
//! These types define the durable, serializable contract between the fleet
//! manager, workers, CLI/TUI surfaces, and the Runtime API. They are
//! intentionally additive: existing runtime-event consumers ignore unknown
//! fields and are unaffected by fleet extensions.
//!
//! See:
//! - <https://github.com/codewhale-hq/CodeWhale/issues/3154> (Agent Fleet control plane)
//! - <https://github.com/codewhale-hq/CodeWhale/issues/3096> (Runtime API sub-agent direction)

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

use super::Status;

/// Explicit boundary migration for pre-Fleet serialized role tokens.
///
/// Call this only at load / parse edges. Runtime code must use Fleet role
/// names via `FleetRole::as_str`. Returns `None` for tokens that are
/// already canonical or unknown — callers should prefer `FleetRole::parse`
/// for full acceptance (canonical + legacy).
#[must_use]
pub fn migrate_legacy_role_token(token: &str) -> Option<&'static str> {
    match token.trim().to_ascii_lowercase().as_str() {
        "worker" | "general-purpose" | "general_purpose" | "default" => Some("general"),
        "scout" | "exploration" | "explorer" => Some("explore"),
        "plan" | "planning" | "awaiter" => Some("planner"),
        "review" | "code-review" | "code_review" => Some("reviewer"),
        "builder" | "implementer" | "implementation" => Some("implement"),
        "verifier" | "verify" | "verification" | "validator" | "tester" => Some("test"),
        "consultant" | "oracle" => Some("advisor"),
        _ => None,
    }
}

pub const FLEET_PROTOCOL_VERSION: &str = "0.1.0";

/// Globally unique identifier for a fleet run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FleetRunId(pub String);

impl From<String> for FleetRunId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for FleetRunId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Top-level fleet run handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRun {
    pub id: FleetRunId,
    pub name: String,
    pub status: FleetRunStatus,
    /// Explicit execution target selected by the managed client.
    ///
    /// Older CLI-created runs predate target selection and therefore omit
    /// this field. Runtime API creation always persists it and currently
    /// accepts only [`FleetRuntimeTarget::ThisComputer`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<FleetRuntimeTarget>,
    /// Named Workflow descriptor that owns this Fleet run.
    ///
    /// The durable task specs below remain the executable source of truth;
    /// this descriptor keeps the product identity and scheduling policy
    /// inspectable without smuggling them through labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<FleetWorkflowDescriptor>,
    /// Canonical named roles declared for the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Maximum number of workers the manager may drive concurrently.
    ///
    /// Older ledgers omit this field; callers fall back to the persisted
    /// worker roster when resuming those runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<usize>,
    /// Optional run-wide usage ceiling (R6, #5567). When the accumulated
    /// worker usage crosses it, the ledger refuses new task admissions,
    /// pauses the run, and records exactly one budget alert. Absent on older
    /// ledgers and by default: unbounded, today's behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_ceiling: Option<FleetUsageCeiling>,
    #[serde(default)]
    pub task_specs: Vec<FleetTaskSpec>,
    #[serde(default)]
    pub worker_specs: Vec<FleetWorkerSpec>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Legacy replay-only execution policy from pre-0.9.11 ledgers.
    ///
    /// New Fleet runs reject this field: Fleet selects identity, while the
    /// Runtime owns trust, secrets, approvals, sandboxing, and tool authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_policy: Option<FleetSecurityPolicy>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// Product-level Runtime target for a managed Fleet run.
///
/// The enum intentionally names unsupported targets as contract values so a
/// client receives a precise capability refusal instead of silently falling
/// back to local execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetRuntimeTarget {
    ThisComputer,
    AnotherComputer,
    Cloud,
}

/// Scheduling shape currently executable by the durable Fleet manager.
///
/// Fleet tasks are independent queue entries today, so only parallel
/// workflows are advertised. Sequence/pipeline support must not be accepted
/// until dependencies are durable in the Fleet ledger.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetWorkflowKind {
    Parallel,
}

/// Durable identity for the Workflow that coordinates a Fleet run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetWorkflowDescriptor {
    pub id: String,
    pub kind: FleetWorkflowKind,
}

/// One privacy-bounded durable event exposed to managed Fleet clients.
///
/// `cursor` is an opaque stable digest of the underlying ledger transition.
/// Clients persist it and send it back on reconnect; they must not parse it.
/// Worker-local sequence numbers remain available separately because they are
/// monotonic only within one `(worker, task)` lifecycle, not across a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetRuntimeEvent {
    pub cursor: String,
    pub event: String,
    pub run_id: FleetRunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_seq: Option<u64>,
    #[serde(default)]
    pub payload: Value,
}

/// Bounded durable replay page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetEventReplay {
    pub run_id: FleetRunId,
    pub events: Vec<FleetRuntimeEvent>,
    #[serde(default)]
    pub has_more: bool,
    /// True when a no-cursor request returned only the newest bounded tail.
    #[serde(default)]
    pub history_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Lifecycle status for an entire fleet run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetRunStatus {
    Pending,
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl Status for FleetRunStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Queued | Self::Running)
    }
    fn is_paused(&self) -> bool {
        matches!(self, Self::Paused)
    }
}

/// Specification of a single unit of work within a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetTaskSpec {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    pub instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<FleetTaskWorkerProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<FleetWorkspaceRequirements>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input_files: Vec<PathBuf>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<FleetTaskBudget>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub expected_artifacts: Vec<FleetArtifactKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scorer: Option<FleetScorerSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<FleetRetryPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alert_policy: Option<FleetAlertPolicy>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

/// Worker role and tool expectations for a task.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetTaskWorkerProfile {
    /// Bounded human selector for one Fleet member.
    ///
    /// Accepts member id/name, semantic role, model id/display name, or an
    /// explicit `route:<provider>/<model>`. `profile` is accepted as a shorter
    /// authoring alias. Resolution and permission narrowing happen in the Fleet
    /// runtime layer.
    #[serde(default, alias = "profile", skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Fleet loadout intent such as `auto`, `fast`, or `review`.
    ///
    /// This is not a concrete provider/model selection; route resolution owns
    /// the executable provider/model/wire-model decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loadout: Option<String>,
    /// Fleet model class hint such as `strong`, `balanced`, or `fast`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_class: Option<String>,
    /// Optional explicit model id for this worker.
    ///
    /// Task-level model overrides are visible authoring data. They apply only
    /// when the selected member does not pin an exact provider/model route;
    /// conflicting overrides of an exact member route are rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Workspace and environment constraints needed before a task starts.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetWorkspaceRequirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<crate::placement::PlacementRequirements>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_files: Vec<PathBuf>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub writable_paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<FleetEnvironmentRequirements>,
}

/// Environment variables a task requires or may pass through to workers.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetEnvironmentRequirements {
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<String>,
}

/// Budget limits for a task.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetTaskBudget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Maximum model turns. `None` and `Some(0)` both mean unbounded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<u64>,
}

/// Reference to an artifact produced or consumed by a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetArtifactRef {
    pub kind: FleetArtifactKind,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

/// Kind of artifact a task may produce or consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetArtifactKind {
    Log,
    Patch,
    TestResult,
    Report,
    Checkpoint,
    Receipt,
    Other(String),
}

impl FleetArtifactKind {
    fn as_wire_str(&self) -> &str {
        match self {
            Self::Log => "log",
            Self::Patch => "patch",
            Self::TestResult => "test_result",
            Self::Report => "report",
            Self::Checkpoint => "checkpoint",
            Self::Receipt => "receipt",
            Self::Other(kind) => kind.as_str(),
        }
    }

    fn from_wire_str(value: &str) -> Self {
        match value {
            "log" => Self::Log,
            "patch" => Self::Patch,
            "test_result" => Self::TestResult,
            "report" => Self::Report,
            "checkpoint" => Self::Checkpoint,
            "receipt" => Self::Receipt,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for FleetArtifactKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_wire_str())
    }
}

impl<'de> Deserialize<'de> for FleetArtifactKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_wire_str(&value))
    }
}

/// Scoring rule used to verify a task result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FleetScorerSpec {
    ExitCode,
    FileExists {
        path: PathBuf,
    },
    RegexMatch {
        path: PathBuf,
        pattern: String,
    },
    JsonPath {
        path: PathBuf,
        expression: String,
    },
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    CodeWhaleVerifierPrompt {
        prompt: String,
    },
    Manual,
}

/// Worker specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetWorkerSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<crate::placement::ExecutionEnvironment>,
    pub id: String,
    pub name: String,
    pub host: FleetHostSpec,
    /// Legacy replay-only host trust label. New runs reject author-supplied
    /// values and derive execution authority from Runtime policy instead.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<FleetTrustLevel>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<usize>,
}

/// Host on which a worker runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FleetHostSpec {
    Local,
    Tailscale {
        node_id: String,
        user: String,
        working_directory: PathBuf,
        codewhale_binary: String,
    },
    Ssh {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        identity: Option<PathBuf>,
        /// Known hosts file for host-key verification.
        #[serde(skip_serializing_if = "Option::is_none")]
        known_hosts: Option<PathBuf>,
        /// Legacy field retained for decoding; the SSH host adapter rejects it
        /// when set. Configure `known_hosts` for host-key verification instead.
        #[serde(skip_serializing_if = "Option::is_none")]
        host_key_fingerprint: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_directory: Option<PathBuf>,
        #[serde(default)]
        #[serde(skip_serializing_if = "Vec::is_empty")]
        env_allowlist: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        codewhale_binary: Option<String>,
    },
    #[serde(alias = "container")]
    #[serde(alias = "Container")]
    Docker {
        image: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

// ── Legacy Runtime-policy wire compatibility ───────────────────────────────

/// Legacy trust classification retained only to deserialize old Fleet ledgers.
///
/// It is not Fleet identity and new run creation rejects it. Current authority
/// comes from live Runtime policy; these helper predicates describe the old
/// wire vocabulary only.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
pub enum FleetTrustLevel {
    /// Fully isolated: no network, no secrets, no writes outside `.codewhale/fleet/`.
    /// Suitable for untrusted code review, community PR checks, or third-party tool runs.
    #[default]
    Sandbox = 0,
    /// Local-only worker with access to the workspace and configured secrets.
    /// Default for local workers. May read repo files but writes are gated.
    Local = 1,
    /// Worker on a known remote host with verified identity and a bounded
    /// set of explicitly granted capabilities. Requires SSH host-key
    /// verification or equivalent attestation.
    #[serde(alias = "remote-verified", alias = "remoteVerified")]
    RemoteVerified = 2,
    /// Fully trusted worker (e.g. operator's own machine, CI runner).
    /// Has access to all configured secrets and may perform any action the
    /// operator can. Reserved for dogfood smoke and operator-owned machines.
    Operator = 3,
}

impl FleetTrustLevel {
    /// Whether this trust level is allowed to access provider secrets.
    #[must_use]
    pub fn may_access_secrets(&self) -> bool {
        matches!(self, Self::Operator | Self::RemoteVerified | Self::Local)
    }

    /// Whether this trust level is allowed to write outside `.codewhale/fleet/`.
    #[must_use]
    pub fn may_write_workspace(&self) -> bool {
        matches!(self, Self::Operator | Self::Local)
    }

    /// Whether this trust level is allowed network access.
    #[must_use]
    pub fn may_access_network(&self) -> bool {
        matches!(self, Self::Operator | Self::RemoteVerified | Self::Local)
    }
}

/// Legacy Runtime execution-policy envelope retained for ledger replay.
///
/// This type is accepted while reading older protocol data but is rejected for
/// new Fleet runs. Fleet membership/selection never grants trust, secrets, or
/// capabilities; the Runtime derives those from its live policy boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetSecurityPolicy {
    /// Default trust level for workers that don't declare one explicitly.
    #[serde(default)]
    pub default_trust_level: FleetTrustLevel,
    /// Secret refs that workers may resolve. An empty list means no secrets
    /// are available. Each entry is a key name, not a value.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_secrets: Vec<FleetSecretRef>,
    /// Capability grants for workers in this run.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capability_grants: Vec<FleetCapabilityGrant>,
    /// Maximum trust level any worker in this run may have, even if the
    /// worker spec requests higher. Defaults to Operator (no ceiling).
    #[serde(default = "default_max_trust_level")]
    pub max_trust_level: FleetTrustLevel,
    /// Require identity verification for remote workers. When true, SSH
    /// workers must pass host-key verification before being trusted at
    /// RemoteVerified level; unverified remotes stay at Sandbox.
    #[serde(default)]
    pub require_identity_verification: bool,
    /// Allow conservative parallel execution of read-only tools (#2983).
    /// When true, workers may batch independent read-only tool calls
    /// (reads, searches, greps) into concurrent turns. Disabled by default
    /// to avoid overwhelming providers or hitting rate limits.
    #[serde(default)]
    pub allow_parallel_reads: bool,
}

fn default_max_trust_level() -> FleetTrustLevel {
    FleetTrustLevel::Operator
}

impl Default for FleetSecurityPolicy {
    fn default() -> Self {
        Self {
            default_trust_level: FleetTrustLevel::Sandbox,
            allowed_secrets: Vec::new(),
            capability_grants: Vec::new(),
            max_trust_level: FleetTrustLevel::Operator,
            require_identity_verification: false,
            allow_parallel_reads: false,
        }
    }
}

/// A reference to a secret that should be resolved at runtime, never
/// serialized as a plaintext value.
///
/// Secret refs appear in task specs, alert configs, and worker definitions.
/// The actual secret value is resolved by the fleet manager from the
/// secrets backend (OS keyring, environment, or file store) just before
/// the worker starts.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FleetSecretRef {
    /// The secret key name (e.g. `"CODEWHALE_API_KEY"`, `"GH_TOKEN"`).
    pub key: String,
    /// Optional source hint for resolution order.
    /// - `"env"` — resolve from environment variable
    /// - `"keyring"` — resolve from OS keyring
    /// - `"file"` — resolve from `~/.codewhale/secrets/`
    /// - absent / null — try all sources in default order
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl FleetSecretRef {
    /// Create a secret ref from a key name with default resolution.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            source: None,
        }
    }

    /// Create a secret ref with an explicit source.
    #[must_use]
    pub fn with_source(key: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            source: Some(source.into()),
        }
    }

    /// Redacted display form for logging. Shows the key name and source
    /// but never the resolved value.
    #[must_use]
    pub fn redacted(&self) -> String {
        match &self.source {
            Some(src) => format!("<secret:{}.{}>", src, self.key),
            None => format!("<secret:{}>", self.key),
        }
    }
}

impl std::fmt::Display for FleetSecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.redacted())
    }
}

impl From<&str> for FleetSecretRef {
    fn from(key: &str) -> Self {
        Self::new(key)
    }
}

impl From<String> for FleetSecretRef {
    fn from(key: String) -> Self {
        Self::new(key)
    }
}

impl<'de> Deserialize<'de> for FleetSecretRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum SecretRefWire {
            Key(String),
            Structured {
                key: String,
                #[serde(default)]
                source: Option<String>,
            },
        }

        match SecretRefWire::deserialize(deserializer)? {
            SecretRefWire::Key(key) if !key.trim().is_empty() => Ok(FleetSecretRef::new(key)),
            SecretRefWire::Key(_) => Err(de::Error::custom("secret ref key cannot be empty")),
            SecretRefWire::Structured { key, source } if !key.trim().is_empty() => {
                Ok(FleetSecretRef { key, source })
            }
            SecretRefWire::Structured { .. } => {
                Err(de::Error::custom("secret ref key cannot be empty"))
            }
        }
    }
}

/// How a worker authenticates to the fleet manager.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum FleetWorkerAuth {
    /// No authentication (local workers share the same uid).
    None,
    /// SSH key-based authentication with host-key verification.
    SshKey {
        /// Path to the SSH identity file (may be a FleetSecretRef in JSON
        /// as `{"key": "...", "source": "file"}`).
        identity: PathBuf,
        /// Known hosts file for host-key verification.
        #[serde(skip_serializing_if = "Option::is_none")]
        known_hosts: Option<PathBuf>,
        /// Legacy metadata; does not enforce key pinning. The SSH host adapter
        /// rejects `FleetHostSpec::Ssh::host_key_fingerprint` when set; configure
        /// `known_hosts` on the host spec for host-key verification instead.
        #[serde(skip_serializing_if = "Option::is_none")]
        host_key_fingerprint: Option<String>,
        /// SSH user for the connection.
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
    },
    /// Token-based authentication for remote workers behind a fleet proxy.
    Token {
        /// Reference to the token secret.
        token_ref: FleetSecretRef,
    },
    /// mTLS certificate-based authentication.
    Mtls {
        /// Path to the client certificate.
        cert_path: PathBuf,
        /// Reference to the private key secret.
        key_ref: FleetSecretRef,
    },
}

/// A capability grant that explicitly authorizes a worker to perform
/// a specific class of action.
///
/// By default, new workers get no grants (least privilege). Grants are
/// additive: a worker's effective capabilities are the union of its
/// trust-level defaults plus any explicit grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetCapabilityGrant {
    /// The capability being granted (e.g. `"network"`, `"git-push"`,
    /// `"provider-secrets"`, `"release"`).
    pub capability: String,
    /// Optional scope limiting the grant (e.g. `"github.com"` for network,
    /// `"crates/tui/**"` for file writes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Optional justification for the grant (audit trail).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Runtime status of a worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetWorkerStatus {
    Unknown,
    Online,
    Busy,
    Offline,
    Unhealthy,
    Draining,
    Retired,
}

impl Status for FleetWorkerStatus {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Retired)
    }
    fn is_active(&self) -> bool {
        matches!(self, Self::Online | Self::Busy)
    }
    fn is_paused(&self) -> bool {
        false
    }
}

/// Durable inbox entry: a task waiting to be leased to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetInboxEntry {
    pub run_id: FleetRunId,
    pub task_id: String,
    pub priority: i32,
    pub enqueued_at: String,
    #[serde(default)]
    pub lease_deadline: Option<String>,
    #[serde(default)]
    pub attempts: u32,
}

/// Worker event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetWorkerEvent {
    pub seq: u64,
    pub run_id: FleetRunId,
    pub worker_id: String,
    pub task_id: String,
    pub timestamp: String,
    #[serde(flatten)]
    pub payload: FleetWorkerEventPayload,
    #[serde(default)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// Union of all worker event payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FleetWorkerEventPayload {
    Queued,
    Leased {
        #[serde(skip_serializing_if = "Option::is_none")]
        lease_expires_at: Option<String>,
    },
    Starting,
    Running,
    ModelWait {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    RunningTool {
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// Typed receipt emitted by a Workflow running inside this worker.
    WorkflowEvent {
        /// Inner Workflow run id. Named distinctly from the outer Fleet run id
        /// because payloads are flattened into `FleetWorkerEvent`.
        workflow_run_id: String,
        event: Value,
    },
    Heartbeat {
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        cpu_percent: Option<f32>,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        memory_mb: Option<u64>,
    },
    /// Provider-reported usage receipt for one model call inside this
    /// worker (R6, #5567). Feeds the run-level accumulator that enforces
    /// [`FleetUsageCeiling`].
    UsageReport {
        input_tokens: u64,
        output_tokens: u64,
    },
    Artifact(FleetArtifactRef),
    Completed {
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Failed {
        reason: String,
        #[serde(default)]
        recoverable: bool,
    },
    Cancelled {
        #[serde(skip_serializing_if = "Option::is_none")]
        cancelled_by: Option<String>,
    },
    Interrupted {
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    Stale {
        #[serde(skip_serializing_if = "Option::is_none")]
        last_heartbeat_at: Option<String>,
    },
    Restarted {
        #[serde(default)]
        restart_count: u32,
    },
    Escalated {
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        alert_id: Option<String>,
    },
}

/// Run-wide usage ceiling (R6, #5567). Token-denominated: workers report
/// provider token counts; a cost-denominated ceiling needs priced receipts
/// in the worker stream and is deliberately not declared until it can be
/// enforced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetUsageCeiling {
    /// Maximum input+output tokens accumulated across every worker model
    /// call in the run.
    pub max_total_tokens: u64,
}

/// Retry policy for a task or worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetRetryPolicy {
    #[serde(default = "default_retry_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_retry_initial_backoff_seconds")]
    pub initial_backoff_seconds: u64,
    #[serde(default = "default_retry_max_backoff_seconds")]
    pub max_backoff_seconds: u64,
    #[serde(default = "default_retry_backoff_multiplier")]
    pub backoff_multiplier: u32,
}

impl Default for FleetRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_seconds: 5,
            max_backoff_seconds: 300,
            backoff_multiplier: 2,
        }
    }
}

fn default_retry_max_attempts() -> u32 {
    FleetRetryPolicy::default().max_attempts
}

fn default_retry_initial_backoff_seconds() -> u64 {
    FleetRetryPolicy::default().initial_backoff_seconds
}

fn default_retry_max_backoff_seconds() -> u64 {
    FleetRetryPolicy::default().max_backoff_seconds
}

fn default_retry_backoff_multiplier() -> u32 {
    FleetRetryPolicy::default().backoff_multiplier
}

/// Alert/escalation policy attached to a task or run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetAlertPolicy {
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<FleetAlertEventClass>,
    #[serde(default)]
    pub channels: Vec<FleetAlertChannel>,
    #[serde(default)]
    pub after_attempts: Option<u32>,
    #[serde(default)]
    pub after_minutes_stale: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FleetAlertEventClass {
    Stale,
    RestartExhausted,
    NeedsHuman,
    BudgetExceeded,
    VerifierFailed,
    RunCompleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FleetAlertChannel {
    Slack {
        /// Webhook URL, resolved from a secret ref or inline.
        #[serde(flatten)]
        webhook: FleetAlertEndpoint,
    },
    Webhook {
        #[serde(flatten)]
        endpoint: FleetAlertEndpoint,
    },
    #[serde(alias = "pager_duty")]
    #[serde(alias = "pagerduty")]
    PagerDuty {
        routing_key: String,
        severity: String,
    },
}

/// An alert channel endpoint, supporting both inline URLs and secret refs.
///
/// For Slack and generic webhook channels, the URL may be provided directly
/// or as a secret reference resolved at send time. When both `url` and
/// `url_ref` are present, `url_ref` takes precedence after resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetAlertEndpoint {
    /// Inline URL (plaintext; only for non-sensitive endpoints).
    #[serde(
        alias = "webhook_url",
        alias = "endpoint_url",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Option<String>,
    /// Reference to a secret containing the webhook URL.
    #[serde(
        alias = "webhook_url_ref",
        alias = "webhook_ref",
        alias = "url_secret_ref",
        skip_serializing_if = "Option::is_none"
    )]
    pub url_ref: Option<FleetSecretRef>,
    /// Optional HMAC secret for webhook payload signing, as a secret ref.
    #[serde(
        alias = "secret",
        alias = "webhook_secret",
        alias = "signing_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub secret_ref: Option<FleetSecretRef>,
}

impl FleetAlertEndpoint {
    /// Create an inline URL endpoint (for non-sensitive use).
    #[must_use]
    pub fn inline(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            url_ref: None,
            secret_ref: None,
        }
    }

    /// Create a secret-backed URL endpoint.
    #[must_use]
    pub fn from_secret(url_ref: FleetSecretRef) -> Self {
        Self {
            url: None,
            url_ref: Some(url_ref),
            secret_ref: None,
        }
    }

    /// Redacted display form for logging.
    #[must_use]
    pub fn redacted(&self) -> String {
        self.url_ref
            .as_ref()
            .map_or_else(|| "<inline-url>".to_string(), |r| r.redacted())
    }
}

/// Resolved-route detail persisted on a [`FleetReceipt`] (#3154).
///
/// This is an additive, *plain-strings* snapshot of the route a fleet worker
/// resolved to. It deliberately does NOT depend on any `codewhale-config` route
/// type so the protocol crate stays free of the route model.
///
/// CRITICAL no-secrets invariant: this struct carries ONLY non-sensitive route
/// shape — provider id/kind, model ids, wire protocol, role/loadout/model-class
/// intent, reasoning tier when known, and deterministic intent sources. It
/// must NEVER hold a credential, API key, bearer token, or a base URL that
/// embeds credentials. There is intentionally no field that could carry a
/// secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetResolvedRoute {
    /// Resolved provider canonical id (e.g. `"deepseek"`).
    pub provider_id: String,
    /// Exact configured provider-table id when the worker used one.
    ///
    /// This is intentionally additive to `provider_id`: literal
    /// `[providers.custom]` resolves to `Some("custom")`, while the legacy
    /// idless root custom route resolves to `None`. Keeping the distinction
    /// prevents a receipt from silently collapsing two different credential
    /// and endpoint authorities into the same generic `custom` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_exact_id: Option<String>,
    /// Resolved provider kind (e.g. `"deepseek"`).
    pub provider_kind: String,
    /// Canonical, provider-agnostic model identity, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    /// Provider-owned wire model id placed on the request.
    pub wire_model_id: String,
    /// Selected wire protocol (e.g. `"chat_completions"`).
    pub protocol: String,
    /// Effective Fleet role intent, when one applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Effective Fleet loadout intent, when one applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loadout: Option<String>,
    /// Original task-level model-class intent, when authored separately from
    /// `loadout`. Profile `model_class_hint` is normalized into `loadout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_class: Option<String>,
    /// Runtime model-route seam used by sub-agent routing (`inherit`, `faster`,
    /// `auto`, or `fixed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route: Option<String>,
    /// Concrete reasoning tier, when it is known by the route resolver path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Deterministic source for the effective role intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_source: Option<String>,
    /// Deterministic source for the effective loadout intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loadout_source: Option<String>,
    /// Deterministic source for the model-class hint, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_class_source: Option<String>,
    /// Deterministic source for the model selector used by the resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source: Option<String>,
    /// How the route was produced (e.g. `"resolver"`).
    pub source: String,
}

/// Effective worker authority persisted on a [`FleetReceipt`] (#3211).
///
/// This is a non-secret snapshot of the already-computed runtime profile. It
/// records what the worker was allowed to do; it does not grant permissions and
/// does not carry credentials, sandbox paths, or provider endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetEffectivePermissions {
    /// Whether the worker profile may modify workspace files.
    pub write: bool,
    /// Whether the worker profile may use network-capable tools.
    pub network: bool,
    /// Shell posture (`none`, `read_only`, or `full`).
    pub shell: String,
    /// Tool-surface posture (`inherit` or `explicit`).
    pub tool_scope: String,
    /// Explicit tool names when `tool_scope` is `explicit`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Whether the worker is intended to run detached/background.
    pub background: bool,
    /// Remaining nested-delegation budget after parent intersection/hardening.
    pub max_spawn_depth: u32,
    /// Roster profile id that contributed to this worker, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    /// Roster layer for `profile_id` (`built_in`, `config`, or `workspace`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_origin: Option<String>,
    /// How this snapshot was produced (e.g. `"worker_runtime_profile"`).
    pub source: String,
}

/// Receipt produced when a task completes verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetReceipt {
    pub run_id: FleetRunId,
    pub task_id: String,
    pub worker_id: String,
    /// Durable lease generation that produced this receipt.
    ///
    /// Optional for backward compatibility with receipts written before Fleet
    /// attempts were fenced explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Sequence of the terminal worker event finalized with this receipt.
    ///
    /// Optional so older ledger records remain replayable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_seq: Option<u64>,
    pub completed_at: String,
    pub result: FleetTaskResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<FleetTaskFailureKind>,
    #[serde(default)]
    pub artifacts: Vec<FleetArtifactRef>,
    #[serde(default)]
    pub score: Option<FleetScore>,
    /// Resolved-route snapshot for this task (#3154).
    ///
    /// `#[serde(default)]` keeps older ledgers (written before this field
    /// existed) deserializable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_route: Option<FleetResolvedRoute>,
    /// Saved exec session id holding the worker's full transcript, when the
    /// local worker persisted its parent-assigned capture in the Runtime's
    /// session store (the exec stream's `session_capture.saved_session_id`).
    /// Remote-only or unavailable transcripts omit this field. Callers resolve the final
    /// assistant reply via `GET /v1/sessions/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_session_id: Option<String>,
    /// Effective worker authority for this task (#3211).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_permissions: Option<FleetEffectivePermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetTaskResult {
    Pass,
    Partial,
    Fail,
    Skip,
    Timeout,
}

/// Source category for a failed task receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FleetTaskFailureKind {
    Transport,
    Task,
    Verifier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetScore {
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_run_round_trip() {
        let run = FleetRun {
            id: FleetRunId::from("run-001"),
            name: "dogfood smoke".to_string(),
            status: FleetRunStatus::Running,
            target: Some(FleetRuntimeTarget::ThisComputer),
            workflow: Some(FleetWorkflowDescriptor {
                id: "release-checks".to_string(),
                kind: FleetWorkflowKind::Parallel,
            }),
            roles: vec!["release-checker".to_string()],
            max_workers: Some(1),
            task_specs: vec![FleetTaskSpec {
                id: "task-1".to_string(),
                name: "lint".to_string(),
                description: None,
                objective: Some("Keep the workspace lint-clean".to_string()),
                instructions: "run cargo clippy".to_string(),
                worker: Some(FleetTaskWorkerProfile {
                    agent_profile: None,
                    role: Some("release-checker".to_string()),
                    loadout: None,
                    model_class: None,
                    model: None,
                    tool_profile: Some("read-only".to_string()),
                    tools: vec!["cargo".to_string()],
                    capabilities: vec!["rust".to_string()],
                }),
                workspace: Some(FleetWorkspaceRequirements {
                    placement: None,
                    root: Some(PathBuf::from(".")),
                    required_files: vec![PathBuf::from("Cargo.toml")],
                    writable_paths: vec![],
                    environment: Some(FleetEnvironmentRequirements {
                        required: vec!["PATH".to_string()],
                        allowlist: vec!["RUST_LOG".to_string()],
                    }),
                }),
                input_files: vec![PathBuf::from("crates/tui/src/main.rs")],
                context: vec!["release gate".to_string()],
                budget: Some(FleetTaskBudget {
                    max_tokens: Some(8000),
                    max_steps: Some(0),
                    max_tool_calls: Some(20),
                    max_seconds: Some(300),
                }),
                tags: vec!["release".to_string()],
                expected_artifacts: vec![FleetArtifactKind::Log],
                scorer: Some(FleetScorerSpec::ExitCode),
                retry_policy: Some(FleetRetryPolicy::default()),
                alert_policy: None,
                timeout_seconds: Some(300),
                metadata: BTreeMap::new(),
            }],
            worker_specs: vec![],
            labels: BTreeMap::new(),
            security_policy: None,
            created_at: "2026-06-12T17:00:00Z".to_string(),
            updated_at: None,
            completed_at: None,
            usage_ceiling: None,
        };
        let json = serde_json::to_string(&run).unwrap();
        let back: FleetRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, run.id);
        assert_eq!(back.status, FleetRunStatus::Running);
        assert_eq!(back.target, Some(FleetRuntimeTarget::ThisComputer));
        assert_eq!(back.roles, vec!["release-checker"]);
        assert_eq!(
            back.workflow.as_ref().map(|workflow| workflow.id.as_str()),
            Some("release-checks")
        );
        assert_eq!(back.task_specs.len(), 1);
        assert_eq!(
            back.task_specs[0].budget.as_ref().unwrap().max_steps,
            Some(0)
        );
        assert_eq!(
            back.task_specs[0].worker.as_ref().unwrap().role.as_deref(),
            Some("release-checker")
        );
        assert_eq!(
            back.task_specs[0]
                .workspace
                .as_ref()
                .unwrap()
                .required_files,
            vec![PathBuf::from("Cargo.toml")]
        );
    }

    #[test]
    fn worker_profile_carries_agent_profile_and_loadout_intent() {
        let json = r#"{
            "profile": "adversarial_reviewer",
            "role": "reviewer",
            "loadout": "auto",
            "model_class": "balanced",
            "model": "deepseek-v4-pro",
            "tool_profile": "read-only",
            "tools": ["read_file"],
            "capabilities": ["rust"]
        }"#;

        let profile: FleetTaskWorkerProfile = serde_json::from_str(json).unwrap();

        assert_eq!(
            profile.agent_profile.as_deref(),
            Some("adversarial_reviewer")
        );
        assert_eq!(profile.role.as_deref(), Some("reviewer"));
        assert_eq!(profile.loadout.as_deref(), Some("auto"));
        assert_eq!(profile.model_class.as_deref(), Some("balanced"));
        assert_eq!(profile.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(profile.tool_profile.as_deref(), Some("read-only"));

        let serialized = serde_json::to_value(&profile).unwrap();
        assert_eq!(serialized["agent_profile"], "adversarial_reviewer");
        assert_eq!(serialized["model"], "deepseek-v4-pro");
        assert!(serialized.get("profile").is_none());
    }

    #[test]
    fn worker_event_lifecycle_round_trip() {
        let events = vec![
            FleetWorkerEvent {
                seq: 1,
                run_id: FleetRunId::from("run-002"),
                worker_id: "worker-a".to_string(),
                task_id: "task-1".to_string(),
                timestamp: "2026-06-12T17:01:00Z".to_string(),
                payload: FleetWorkerEventPayload::Queued,
                extra: BTreeMap::new(),
            },
            FleetWorkerEvent {
                seq: 2,
                run_id: FleetRunId::from("run-002"),
                worker_id: "worker-a".to_string(),
                task_id: "task-1".to_string(),
                timestamp: "2026-06-12T17:01:05Z".to_string(),
                payload: FleetWorkerEventPayload::RunningTool {
                    tool: "bash".to_string(),
                    call_id: Some("call-1".to_string()),
                },
                extra: BTreeMap::new(),
            },
            FleetWorkerEvent {
                seq: 3,
                run_id: FleetRunId::from("run-002"),
                worker_id: "worker-a".to_string(),
                task_id: "task-1".to_string(),
                timestamp: "2026-06-12T17:02:00Z".to_string(),
                payload: FleetWorkerEventPayload::Completed {
                    exit_code: Some(0),
                    summary: Some("ok".to_string()),
                },
                extra: BTreeMap::new(),
            },
        ];
        let json = serde_json::to_string(&events).unwrap();
        let back: Vec<FleetWorkerEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 3);
        assert!(matches!(back[0].payload, FleetWorkerEventPayload::Queued));
        assert!(matches!(
            back[2].payload,
            FleetWorkerEventPayload::Completed { .. }
        ));
    }

    #[test]
    fn workflow_receipt_round_trip_keeps_outer_and_inner_run_ids_distinct() {
        let event = FleetWorkerEvent {
            seq: 3,
            run_id: FleetRunId::from("fleet-run-1"),
            worker_id: "worker-a".to_string(),
            task_id: "task-1".to_string(),
            timestamp: "2026-07-10T00:00:00Z".to_string(),
            payload: FleetWorkerEventPayload::WorkflowEvent {
                workflow_run_id: "workflow_1".to_string(),
                event: serde_json::json!({"type": "task_completed"}),
            },
            extra: BTreeMap::new(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["run_id"], "fleet-run-1");
        assert_eq!(value["workflow_run_id"], "workflow_1");
        let back: FleetWorkerEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            back.payload,
            FleetWorkerEventPayload::WorkflowEvent {
                workflow_run_id,
                ref event,
            } if workflow_run_id == "workflow_1" && event["type"] == "task_completed"
        ));
    }

    #[test]
    fn alert_policy_round_trip() {
        let policy = FleetAlertPolicy {
            events: vec![FleetAlertEventClass::Stale],
            channels: vec![FleetAlertChannel::Slack {
                webhook: FleetAlertEndpoint::inline("https://hooks.slack.com/test"),
            }],
            after_attempts: Some(2),
            after_minutes_stale: Some(10),
        };
        let json = serde_json::to_string(&policy).unwrap();
        assert!(json.contains("\"events\":[\"stale\"]"));
        assert!(json.contains("\"kind\":\"slack\""));
        let back: FleetAlertPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.events, vec![FleetAlertEventClass::Stale]);
        assert_eq!(back.after_attempts, Some(2));
    }

    #[test]
    fn artifact_other_kind_round_trip() {
        let artifact = FleetArtifactRef {
            kind: FleetArtifactKind::Other("coverage.xml".to_string()),
            path: PathBuf::from("/tmp/coverage.xml"),
            checksum: Some("sha256:abc".to_string()),
            mime_type: Some("application/xml".to_string()),
            size_bytes: Some(1024),
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let back: FleetArtifactRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, artifact.kind);
        assert_eq!(back.size_bytes, Some(1024));
    }

    #[test]
    fn ssh_host_spec_accepts_minimal_legacy_json() {
        let json = r#"{"kind":"ssh","host":"builder.example.test"}"#;
        let host: FleetHostSpec = serde_json::from_str(json).unwrap();

        match host {
            FleetHostSpec::Ssh {
                host,
                port,
                user,
                identity,
                known_hosts,
                host_key_fingerprint,
                working_directory,
                env_allowlist,
                codewhale_binary,
            } => {
                assert_eq!(host, "builder.example.test");
                assert_eq!(port, None);
                assert_eq!(user, None);
                assert_eq!(identity, None);
                assert_eq!(known_hosts, None);
                assert_eq!(host_key_fingerprint, None);
                assert_eq!(working_directory, None);
                assert!(env_allowlist.is_empty());
                assert_eq!(codewhale_binary, None);
            }
            other => panic!("expected ssh host spec, got {other:?}"),
        }
    }

    #[test]
    fn artifact_kind_uses_flat_string_json() {
        let known = serde_json::to_string(&FleetArtifactKind::TestResult).unwrap();
        assert_eq!(known, "\"test_result\"");

        let custom =
            serde_json::to_string(&FleetArtifactKind::Other("coverage.xml".to_string())).unwrap();
        assert_eq!(custom, "\"coverage.xml\"");

        let parsed: FleetArtifactKind = serde_json::from_str("\"coverage.xml\"").unwrap();
        assert_eq!(parsed, FleetArtifactKind::Other("coverage.xml".to_string()));
    }

    #[test]
    fn retry_policy_missing_fields_use_nonzero_defaults() {
        let policy: FleetRetryPolicy = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(policy, FleetRetryPolicy::default());

        let policy: FleetRetryPolicy =
            serde_json::from_value(serde_json::json!({"max_attempts": 5})).unwrap();
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(
            policy.initial_backoff_seconds,
            FleetRetryPolicy::default().initial_backoff_seconds
        );
        assert_eq!(
            policy.max_backoff_seconds,
            FleetRetryPolicy::default().max_backoff_seconds
        );
        assert_eq!(
            policy.backoff_multiplier,
            FleetRetryPolicy::default().backoff_multiplier
        );
    }

    #[test]
    fn sparse_worker_events_omit_absent_optional_fields() {
        let heartbeat = FleetWorkerEventPayload::Heartbeat {
            cpu_percent: None,
            memory_mb: None,
        };
        let heartbeat_json = serde_json::to_value(&heartbeat).unwrap();
        assert_eq!(heartbeat_json, serde_json::json!({"state": "heartbeat"}));

        let completed = FleetWorkerEventPayload::Completed {
            exit_code: None,
            summary: None,
        };
        let completed_json = serde_json::to_value(&completed).unwrap();
        assert_eq!(completed_json, serde_json::json!({"state": "completed"}));
    }

    #[test]
    fn receipt_round_trip() {
        let receipt = FleetReceipt {
            run_id: FleetRunId::from("run-003"),
            task_id: "task-1".to_string(),
            worker_id: "worker-b".to_string(),
            attempt: Some(2),
            terminal_seq: Some(7),
            completed_at: "2026-06-12T17:03:00Z".to_string(),
            result: FleetTaskResult::Pass,
            failure_kind: None,
            artifacts: vec![],
            score: Some(FleetScore {
                value: 0.95,
                max: Some(1.0),
                notes: None,
            }),
            resolved_route: None,
            saved_session_id: None,
            effective_permissions: None,
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let back: FleetReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, FleetTaskResult::Pass);
        assert_eq!(back.score.as_ref().unwrap().value, 0.95);
        assert_eq!(back.attempt, Some(2));
        assert_eq!(back.terminal_seq, Some(7));
    }

    #[test]
    fn partial_receipt_records_failure_source_when_needed() {
        let receipt = FleetReceipt {
            run_id: FleetRunId::from("run-004"),
            task_id: "task-2".to_string(),
            worker_id: "worker-c".to_string(),
            attempt: None,
            terminal_seq: None,
            completed_at: "2026-06-12T17:04:00Z".to_string(),
            result: FleetTaskResult::Partial,
            failure_kind: Some(FleetTaskFailureKind::Verifier),
            artifacts: vec![],
            score: Some(FleetScore {
                value: 0.5,
                max: Some(1.0),
                notes: Some("manual verification required".to_string()),
            }),
            resolved_route: None,
            saved_session_id: None,
            effective_permissions: None,
        };

        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains("\"result\":\"partial\""));
        assert!(json.contains("\"failure_kind\":\"verifier\""));
        let back: FleetReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, FleetTaskResult::Partial);
        assert_eq!(back.failure_kind, Some(FleetTaskFailureKind::Verifier));
    }

    #[test]
    fn ssh_host_spec_with_key_pinning_round_trip() {
        let spec = FleetHostSpec::Ssh {
            host: "builder.trusted.example.com".to_string(),
            port: Some(22),
            user: Some("codewhale".to_string()),
            identity: Some(PathBuf::from("~/.ssh/codewhale_fleet")),
            known_hosts: Some(PathBuf::from("~/.ssh/known_hosts")),
            host_key_fingerprint: Some("SHA256:aLGqZo1M6c...".to_string()),
            working_directory: Some(PathBuf::from("/srv/codewhale/work")),
            env_allowlist: vec!["CODEWHALE_PROFILE".to_string()],
            codewhale_binary: Some("/usr/local/bin/codewhale".to_string()),
        };
        let json = serde_json::to_string_pretty(&spec).unwrap();
        assert!(json.contains("\"known_hosts\""));
        assert!(json.contains("\"host_key_fingerprint\""));
        assert!(json.contains("SHA256:aLGqZo1M6c..."));

        let back: FleetHostSpec = serde_json::from_str(&json).unwrap();
        match back {
            FleetHostSpec::Ssh {
                host,
                known_hosts,
                host_key_fingerprint,
                ..
            } => {
                assert_eq!(host, "builder.trusted.example.com");
                assert_eq!(known_hosts, Some(PathBuf::from("~/.ssh/known_hosts")));
                assert_eq!(
                    host_key_fingerprint,
                    Some("SHA256:aLGqZo1M6c...".to_string())
                );
            }
            other => panic!("expected ssh host spec, got {other:?}"),
        }
    }

    #[test]
    fn secret_ref_redacted_never_exposes_value() {
        let ref_ = FleetSecretRef::new("DEEPSEEK_API_KEY");
        let redacted = ref_.redacted();
        assert!(redacted.contains("DEEPSEEK_API_KEY"));
        assert!(!redacted.contains("sk-"));
        assert!(redacted.contains("<secret:"));

        let ref_ = FleetSecretRef::with_source("GH_TOKEN", "env");
        let redacted = ref_.redacted();
        assert!(redacted.contains("env.GH_TOKEN"));
        assert!(!redacted.contains("ghp_"));
    }

    #[test]
    fn alert_endpoint_from_secret_round_trip() {
        let endpoint = FleetAlertEndpoint::from_secret(FleetSecretRef::new("SLACK_WEBHOOK"));
        let json = serde_json::to_string(&endpoint).unwrap();
        assert!(json.contains("SLACK_WEBHOOK"));
        assert!(!json.contains("hooks.slack.com"));

        let back: FleetAlertEndpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url_ref.as_ref().unwrap().key, "SLACK_WEBHOOK");
        assert_eq!(back.url, None);
    }

    #[test]
    fn secret_ref_accepts_legacy_string_wire_shape() {
        let ref_: FleetSecretRef = serde_json::from_str(r#""CODEWHALE_FLEET_TOKEN""#).unwrap();
        assert_eq!(ref_, FleetSecretRef::new("CODEWHALE_FLEET_TOKEN"));

        let ref_: FleetSecretRef =
            serde_json::from_str(r#"{"key":"GH_TOKEN","source":"env"}"#).unwrap();
        assert_eq!(ref_, FleetSecretRef::with_source("GH_TOKEN", "env"));
    }

    #[test]
    fn trust_level_accepts_hyphenated_remote_verified() {
        let trust: FleetTrustLevel = serde_json::from_str(r#""remote-verified""#).unwrap();
        assert_eq!(trust, FleetTrustLevel::RemoteVerified);

        let canonical = serde_json::to_string(&trust).unwrap();
        assert_eq!(canonical, r#""remote_verified""#);
    }

    #[test]
    fn alert_channel_accepts_legacy_webhook_fields() {
        let channel: FleetAlertChannel = serde_json::from_str(
            r#"{
                "kind": "slack",
                "webhook_url": "https://hooks.slack.com/test",
                "secret": "SLACK_SIGNING_SECRET"
            }"#,
        )
        .unwrap();

        match channel {
            FleetAlertChannel::Slack { webhook } => {
                assert_eq!(webhook.url.as_deref(), Some("https://hooks.slack.com/test"));
                assert_eq!(
                    webhook.secret_ref,
                    Some(FleetSecretRef::new("SLACK_SIGNING_SECRET"))
                );
            }
            other => panic!("expected slack channel, got {other:?}"),
        }
    }

    #[test]
    fn security_policy_defaults_are_conservative() {
        let policy = FleetSecurityPolicy::default();
        assert_eq!(policy.default_trust_level, FleetTrustLevel::Sandbox);
        assert!(policy.allowed_secrets.is_empty());
        assert!(policy.capability_grants.is_empty());
        assert_eq!(policy.max_trust_level, FleetTrustLevel::Operator);
        assert!(!policy.require_identity_verification);
    }

    #[test]
    fn trust_level_ordinal_reflects_privilege() {
        assert!(FleetTrustLevel::Operator > FleetTrustLevel::RemoteVerified);
        assert!(FleetTrustLevel::RemoteVerified > FleetTrustLevel::Local);
        assert!(FleetTrustLevel::Local > FleetTrustLevel::Sandbox);

        assert!(FleetTrustLevel::Operator.may_access_secrets());
        assert!(!FleetTrustLevel::Sandbox.may_access_secrets());
        assert!(!FleetTrustLevel::Sandbox.may_write_workspace());
        assert!(FleetTrustLevel::Operator.may_write_workspace());
    }

    fn sample_receipt_with_route() -> FleetReceipt {
        FleetReceipt {
            run_id: FleetRunId::from("run-route"),
            task_id: "task-route".to_string(),
            worker_id: "worker-route".to_string(),
            attempt: Some(1),
            terminal_seq: Some(4),
            completed_at: "2026-06-23T00:00:00Z".to_string(),
            result: FleetTaskResult::Pass,
            failure_kind: None,
            artifacts: vec![],
            score: None,
            resolved_route: Some(FleetResolvedRoute {
                provider_id: "deepseek".to_string(),
                provider_exact_id: None,
                provider_kind: "deepseek".to_string(),
                canonical_model: Some("deepseek-v4-pro".to_string()),
                wire_model_id: "deepseek-v4-pro".to_string(),
                protocol: "chat_completions".to_string(),
                role: Some("builder".to_string()),
                loadout: Some("auto".to_string()),
                model_class: Some("balanced".to_string()),
                model_route: Some("auto".to_string()),
                reasoning_effort: Some("high".to_string()),
                role_source: Some("task.role".to_string()),
                loadout_source: Some("task.loadout".to_string()),
                model_class_source: Some("task.model_class".to_string()),
                model_source: Some("task.model".to_string()),
                source: "resolver".to_string(),
            }),
            saved_session_id: None,
            effective_permissions: Some(FleetEffectivePermissions {
                write: true,
                network: true,
                shell: "full".to_string(),
                tool_scope: "explicit".to_string(),
                tools: vec!["read_file".to_string(), "apply_patch".to_string()],
                background: true,
                max_spawn_depth: 2,
                profile_id: Some("builder".to_string()),
                profile_origin: Some("built_in".to_string()),
                source: "worker_runtime_profile".to_string(),
            }),
        }
    }

    #[test]
    fn fleet_resolved_route_round_trips() {
        let receipt = sample_receipt_with_route();
        let json = serde_json::to_string(&receipt).unwrap();
        let back: FleetReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resolved_route, receipt.resolved_route);
        assert_eq!(back.effective_permissions, receipt.effective_permissions);
        let route = back.resolved_route.unwrap();
        assert_eq!(route.provider_id, "deepseek");
        assert_eq!(route.wire_model_id, "deepseek-v4-pro");
        assert_eq!(route.protocol, "chat_completions");
        assert_eq!(route.role.as_deref(), Some("builder"));
        assert_eq!(route.loadout.as_deref(), Some("auto"));
        assert_eq!(route.model_class.as_deref(), Some("balanced"));
        assert_eq!(route.model_route.as_deref(), Some("auto"));
        assert_eq!(route.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(route.role_source.as_deref(), Some("task.role"));
        assert_eq!(route.loadout_source.as_deref(), Some("task.loadout"));
        assert_eq!(
            route.model_class_source.as_deref(),
            Some("task.model_class")
        );
        assert_eq!(route.model_source.as_deref(), Some("task.model"));
        assert_eq!(route.source, "resolver");

        let permissions = back
            .effective_permissions
            .expect("effective permissions should round-trip");
        assert!(permissions.write);
        assert!(permissions.network);
        assert_eq!(permissions.shell, "full");
        assert_eq!(permissions.tool_scope, "explicit");
        assert_eq!(
            permissions.tools,
            vec!["read_file".to_string(), "apply_patch".to_string()]
        );
        assert!(permissions.background);
        assert_eq!(permissions.max_spawn_depth, 2);
        assert_eq!(permissions.profile_id.as_deref(), Some("builder"));
        assert_eq!(permissions.profile_origin.as_deref(), Some("built_in"));
        assert_eq!(permissions.source, "worker_runtime_profile");
    }

    #[test]
    fn fleet_receipt_without_resolved_route_still_deserializes() {
        // An old ledger receipt JSON written before #3154 has no
        // `resolved_route` key; `#[serde(default)]` must keep it readable.
        let legacy = r#"{
            "run_id": "run-legacy",
            "task_id": "task-legacy",
            "worker_id": "worker-legacy",
            "completed_at": "2026-06-01T00:00:00Z",
            "result": "pass",
            "artifacts": [],
            "score": null
        }"#;
        let receipt: FleetReceipt = serde_json::from_str(legacy).unwrap();
        assert_eq!(receipt.task_id, "task-legacy");
        assert!(receipt.resolved_route.is_none());
        assert!(receipt.attempt.is_none());
        assert!(receipt.terminal_seq.is_none());
    }

    #[test]
    fn fleet_resolved_route_legacy_shape_still_deserializes() {
        let legacy = r#"{
            "run_id": "run-route",
            "task_id": "task-route",
            "worker_id": "worker-route",
            "completed_at": "2026-06-23T00:00:00Z",
            "result": "pass",
            "artifacts": [],
            "score": null,
            "resolved_route": {
                "provider_id": "deepseek",
                "provider_kind": "deepseek",
                "canonical_model": "deepseek-v4-pro",
                "wire_model_id": "deepseek-v4-pro",
                "protocol": "chat_completions",
                "role": "builder",
                "loadout": "fast",
                "source": "resolver"
            }
        }"#;

        let receipt: FleetReceipt = serde_json::from_str(legacy).unwrap();
        let route = receipt.resolved_route.expect("legacy route should parse");
        assert_eq!(route.source, "resolver");
        assert_eq!(route.role.as_deref(), Some("builder"));
        assert_eq!(route.loadout.as_deref(), Some("fast"));
        assert_eq!(route.model_class, None);
        assert_eq!(route.model_route, None);
        assert_eq!(route.reasoning_effort, None);
        assert_eq!(route.role_source, None);
        assert_eq!(route.loadout_source, None);
        assert_eq!(route.model_class_source, None);
        assert_eq!(route.model_source, None);
    }

    #[test]
    fn fleet_resolved_route_serialization_carries_no_secrets() {
        let receipt = sample_receipt_with_route();
        // Scan the serialized resolved-route object: this is the field whose
        // no-secrets invariant we are asserting. Scoping to the route value
        // avoids false positives from unrelated envelope ids (e.g. a task id
        // such as "task-foo" innocently contains the substring "sk-").
        let route_json = serde_json::to_string(receipt.resolved_route.as_ref().unwrap()).unwrap();
        assert_no_secret_markers(&route_json);
        // The envelope as a whole must also stay credential-free.
        let receipt_json = serde_json::to_string(&receipt).unwrap();
        for needle in SECRET_KEY_MARKERS {
            assert!(
                !receipt_json.to_ascii_lowercase().contains(needle),
                "receipt JSON must not contain secret-key marker {needle:?}: {receipt_json}"
            );
        }
    }

    /// Substrings that indicate a leaked credential field/value. These are
    /// deliberately specific so legitimate ids/model names do not trip them.
    const SECRET_KEY_MARKERS: &[&str] = &[
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "bearer ",
        "auth_token",
        "auth-token",
        "password",
        "credential",
        "sk-ant-",
        "sk-proj-",
        "sk-or-",
        "secret",
    ];

    fn assert_no_secret_markers(json: &str) {
        let haystack = json.to_ascii_lowercase();
        for needle in SECRET_KEY_MARKERS {
            assert!(
                !haystack.contains(needle),
                "resolved-route JSON must not contain secret marker {needle:?}: {json}"
            );
        }
    }
}

/// Runtime delegation has no default product depth limit. Explicit owner
/// values still restrict child authority; zero disables delegation.
pub const DEFAULT_SPAWN_DEPTH: u32 = u32::MAX;

/// Shell posture, ordered most → least restrictive so `min_with` is the safe
/// side of a clamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellCeiling {
    None,
    ReadOnly,
    Full,
}

impl ShellCeiling {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "read_only",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub fn min_with(self, other: Self) -> Self {
        if self <= other { self } else { other }
    }
}

/// A Runtime child-authority envelope.
///
/// This type is intentionally not part of `ExactMember`. Exact Fleet files
/// choose identity, route, and reasoning; Runtime intersects its role posture
/// with the live parent after member selection. Keeping the envelope as a
/// separate type lets receipts describe the authority that Runtime actually
/// installed without turning a saved Fleet into a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionCeiling {
    pub write: bool,
    /// Whether the Runtime child may be handed a **model-visible network tool**
    /// (fetch, browse, HTTP).
    ///
    /// This is deliberately *not* a statement about transport. Host-owned
    /// provider inference — the ordinary API call CodeWhale makes on the
    /// member's behalf — always happens over the network and is not governed
    /// by this field. A child with `network_tool = false` still runs on a
    /// remote model; it simply has no tool with which to reach the network
    /// itself. Receipts disclose that distinction rather than implying an
    /// air-gap.
    #[serde(alias = "network")]
    pub network_tool: bool,
    pub shell: ShellCeiling,
    /// Nested-delegation budget the Runtime child may consume.
    pub delegation_depth: u32,
    /// Whether the Runtime child may be handed tools at all.
    pub tools: bool,
}

impl PermissionCeiling {
    /// The Router's fixed posture: no tools (so no network tool), no shell, no
    /// writes, no delegation. Not configurable — see `RouterMember`.
    ///
    /// The Router itself is still *inferred* by its configured provider over
    /// the network; that is host-owned transport, disclosed on the receipt.
    pub const ROUTER: Self = Self {
        write: false,
        network_tool: false,
        shell: ShellCeiling::None,
        delegation_depth: 0,
        tools: false,
    };

    /// Legacy named presets retained for Runtime compatibility and tests.
    ///
    /// The exact Fleet parser accepts a historic `permissions` key only as
    /// ignored input; no preset selected here is projected onto a member.
    pub fn preset(name: &str) -> Option<Self> {
        let base = |write, network_tool, shell, delegation_depth| Self {
            write,
            network_tool,
            shell,
            delegation_depth,
            tools: true,
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::ROUTER),
            "analyst" => Some(base(false, false, ShellCeiling::None, 0)),
            "read_only" | "readonly" => Some(base(false, false, ShellCeiling::ReadOnly, 0)),
            "tester" | "verifier" => Some(base(false, false, ShellCeiling::Full, 0)),
            "read_write" | "readwrite" => Some(base(true, false, ShellCeiling::Full, 0)),
            "full" => Some(base(true, true, ShellCeiling::Full, 1)),
            _ => None,
        }
    }

    /// Narrow this ceiling against the active session posture. Every field
    /// takes the more restrictive side, so the result can never grant more
    /// than either input.
    #[must_use]
    pub fn clamp_to(self, session: Self) -> Self {
        Self {
            write: self.write && session.write,
            network_tool: self.network_tool && session.network_tool,
            shell: self.shell.min_with(session.shell),
            delegation_depth: self.delegation_depth.min(session.delegation_depth),
            tools: self.tools && session.tools,
        }
    }
}

impl Default for PermissionCeiling {
    fn default() -> Self {
        Self::preset("read_only").expect("read_only is a known preset")
    }
}
