//! Codewhale account and BYOK credential commands.
//!
//! This module is deliberately separate from the provider-facing `login` and
//! `auth` commands in `lib.rs`: those configure the local runtime, while this
//! surface signs a CLI profile into the managed Codewhale account and stores
//! provider keys in that account's remote vault.

use std::io::{self, IsTerminal, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use codewhale_config::device_code::DevicePollOutcome;
use codewhale_config::{ConfigStore, ProviderKind};
use codewhale_secrets::Secrets;
use codewhale_secrets::account::{
    ACCOUNT_API_BASE_ENV as CLOUD_API_BASE_ENV, AccountAuthBundle as AuthBundle,
    AccountSessionSnapshot, AccountSessionStore, AccountUser as CloudUser,
    DEFAULT_ACCOUNT_API_BASE as DEFAULT_API_BASE, StoredAccountAuth as StoredCloudAuth,
    normalize_account_profile as normalized_profile, secure_account_session_secrets,
    validate_account_auth_bundle as validate_auth_bundle,
};
use reqwest::Url;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(crate) mod machine;
mod work;

const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_FILE_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;
const MAX_COMPUTER_FILE_SAVE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_COMPUTER_FILE_RESTORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILE_VERSION: u64 = 9_007_199_254_740_991;
const MIN_API_KEY_BYTES: usize = 8;
const MAX_API_KEY_BYTES: u64 = 4096;
const MAX_API_KEY_STDIN_BYTES: u64 = MAX_API_KEY_BYTES + 1024;
const MAX_KEY_LABEL_CHARS: usize = 80;
pub(crate) const DEFAULT_LOGIN_TIMEOUT_SECONDS: u64 = 600;
pub(crate) const MAX_LOGIN_TIMEOUT_SECONDS: u64 = 3600;

#[derive(Debug, Args)]
pub(crate) struct CloudArgs {
    /// Codewhale account API origin. HTTPS is required except for loopback HTTP.
    #[arg(long, global = true, value_name = "URL")]
    api_base: Option<String>,
    #[command(subcommand)]
    command: CloudCommand,
}

#[derive(Debug, Subcommand)]
enum CloudCommand {
    /// Sign this CLI profile in through the browser device flow.
    Login(CloudLoginArgs),
    /// Show the signed-in account for this CLI profile.
    Status,
    /// Remove this profile's local account session and revoke it when reachable.
    Logout,
    /// Manage provider API keys stored in the signed-in Codewhale account.
    ///
    /// These are credentials Codewhale presents *to* a model provider. For the
    /// machine tokens a customer presents *to* Codewhale, see `api-keys`.
    Keys(CloudKeysArgs),
    /// Manage Computers in this signed-in Codewhale account.
    Computers(CloudComputersArgs),
    /// List Projects available to this signed-in Codewhale account.
    Projects(CloudProjectsArgs),
    /// Inspect GitHub repositories authorized for this account.
    Github(CloudGithubArgs),
    /// Manage named Agents and their account conversations.
    Agents(CloudAgentsArgs),
    /// Manage Codewhale account API keys: machine tokens for CI.
    #[command(name = "api-keys")]
    ApiKeys(machine::ApiKeysArgs),
    /// Show the account this CLI authenticates as, preferring a machine key.
    Whoami,
    /// Check the account's agent-model precondition for machine work.
    Agent,
    /// Inspect the account document; local settings import is not available yet.
    Pull(CloudPullArgs),
    /// Push local settings to the account document (never automatic, --dry-run required).
    Push(CloudPushArgs),
}

#[derive(Debug, Args)]
struct CloudLoginArgs {
    /// Print the verification URL without trying to open a browser.
    #[arg(long, default_value_t = false)]
    no_open: bool,
    /// Maximum time to wait for browser authorization.
    #[arg(
        long = "timeout-seconds",
        default_value_t = DEFAULT_LOGIN_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=MAX_LOGIN_TIMEOUT_SECONDS)
    )]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct CloudPullArgs {
    /// Inspect the account document without writing local files.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct CloudPushArgs {
    /// Show what would be pushed without writing the remote document.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct CloudKeysArgs {
    #[command(subcommand)]
    command: CloudKeysCommand,
}

#[derive(Debug, Args)]
struct CloudComputersArgs {
    #[command(subcommand)]
    command: CloudComputersCommand,
}

#[derive(Debug, Args)]
struct CloudAgentsArgs {
    #[command(subcommand)]
    command: CloudAgentsCommand,
}

#[derive(Debug, Args)]
struct CloudProjectsArgs {
    #[command(subcommand)]
    command: CloudProjectsCommand,
}

#[derive(Debug, Args)]
struct CloudGithubArgs {
    #[command(subcommand)]
    command: CloudGithubCommand,
}

#[derive(Debug, Subcommand)]
enum CloudGithubCommand {
    /// List repository bindings saved by GitHub App installation.
    Bindings {
        #[arg(long)]
        json: bool,
    },
    /// Connect one repository from an installed GitHub App to this account.
    ///
    /// The account API checks that you own the installation and can read the
    /// repository; nothing is written to GitHub. Omit --installation-id when
    /// every repository already connected shares one installation.
    Bind {
        /// Repository as OWNER/REPO.
        repo: String,
        /// GitHub App installation ID (see `account github bindings --json`).
        #[arg(long)]
        installation_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum CloudProjectsCommand {
    /// List account Projects; use an ID when binding an Agent.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a Project from a GitHub repository already connected to this account.
    Create {
        name: String,
        #[arg(long)]
        repo_binding_id: String,
        #[arg(long)]
        operation_key: String,
    },
}

#[derive(Debug, Subcommand)]
enum CloudAgentsCommand {
    /// List active Agents in this account.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a named Agent. Reuse the operation key if a response is lost.
    Create {
        name: String,
        /// Assign an existing account Project to this Agent.
        #[arg(long)]
        project_id: Option<String>,
        #[arg(long)]
        operation_key: String,
    },
    /// Assign an existing Project to an Agent, checking its saved revision.
    BindProject { agent: String, project_id: String },
    /// List recent, active conversations for a named Agent or Agent ID.
    Threads {
        agent: String,
        #[arg(long)]
        json: bool,
    },
    /// Create an Agent conversation with an explicit saved model route.
    NewThread {
        agent: String,
        #[arg(long, default_value = "Main")]
        title: String,
        #[arg(long, default_value = "deepseek")]
        provider: String,
        #[arg(long, default_value = "deepseek-flash")]
        model: String,
        #[arg(long)]
        operation_key: String,
    },
    /// Send one message to a selected conversation, using its saved model.
    Send {
        agent: String,
        prompt: String,
        /// Select a specific conversation; otherwise use its only active one, or a unique Main.
        #[arg(long)]
        thread: Option<String>,
        /// Explicit payer. Only byok_external is available under the current launch policy.
        #[arg(long)]
        billing_mode: String,
        /// Stable message ID for safe retry after an uncertain response.
        #[arg(long)]
        operation_key: String,
    },
    /// Read one turn's answer and status from this account's conversation events.
    Result {
        agent: String,
        thread: String,
        turn: String,
        /// Resume from a previously printed event sequence for long conversations.
        #[arg(long, default_value_t = 0)]
        since_seq: u64,
    },
    /// Give a named Agent durable Work. This records a request; it does not start compute.
    ///
    /// The Agent reads the message first: a plain instruction becomes Work, a
    /// command such as "stop" is applied to its active Work, a correction
    /// edits that Work's objective, and a question changes nothing. The
    /// output states which happened. If the reply is lost, re-run the same
    /// command with the same --message-id; that never creates a second Work for
    /// the same instruction. If the message was a stop or a correction, check
    /// `work-status` first.
    Work {
        agent: String,
        objective: String,
        /// Stable message ID for safe retry after an uncertain response.
        #[arg(long)]
        message_id: String,
        #[arg(long, help = "UTF-8 source context file, at most 64000 UTF-16 units")]
        context_file: Option<PathBuf>,
        #[arg(
            long = "file",
            help = "Saved account file reference FILE_ID@VERSION; repeat for up to ten files"
        )]
        file_refs: Vec<String>,
    },
    /// Record Work, review its quote, and start it on a new cloud computer.
    ///
    /// Without --yes and --confirm-eu-compute, a terminal asks before launching
    /// and a script refuses; nothing starts without that consent. Reuse
    /// --operation-key and --message-id to retry after an uncertain response.
    Dispatch {
        objective: String,
        /// Agent name or ID; omitted, the account's repository-free default Agent.
        #[arg(long)]
        agent: Option<String>,
        /// Task time in seconds; defaults to what the account's offer allows.
        #[arg(long)]
        seconds: Option<u64>,
        #[arg(long, help = "UTF-8 source context file, at most 64000 UTF-16 units")]
        context_file: Option<PathBuf>,
        #[arg(
            long = "file",
            help = "Saved account file reference FILE_ID@VERSION; repeat for up to ten files"
        )]
        file_refs: Vec<String>,
        #[arg(long)]
        operation_key: Option<String>,
        #[arg(long)]
        message_id: Option<String>,
        /// Do not ask before launching (also needs --confirm-eu-compute).
        #[arg(long)]
        yes: bool,
        /// Agree that repository code and Work files run on EU compute.
        #[arg(long)]
        confirm_eu_compute: bool,
    },
    /// Read the account-owned status of a Work request.
    WorkStatus { id: String },
    /// Cancel Work and stop its computer; safe to repeat.
    ///
    /// If the Work still has queued prompts, choose --queue discard or --queue
    /// park. An already-finished Work is reported and exits successfully. When
    /// the reply is lost the outcome is unknown: run `work-status` before
    /// retrying.
    WorkCancel {
        id: String,
        /// What happens to queued prompts: discard them or park them on the Work.
        #[arg(long, value_enum)]
        queue: Option<WorkQueueChoice>,
        /// Short reason recorded with the cancellation.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Read a Work's outcome: state, attempts, evidence, artifacts, draft PR, model route and usage.
    ///
    /// --json prints the account API's raw result and attempt records.
    WorkResult {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Quote offered computer time for Work. Nothing starts.
    ///
    /// Prints the funding, EU placement and time disclosure plus a short-lived
    /// confirmation for `work-launch`. Reuse one --operation-key for the quote
    /// and the launch.
    WorkQuote {
        id: String,
        /// Stable launch ID, reused by `work-launch` so a retry cannot start two computers.
        #[arg(long)]
        operation_key: String,
        #[arg(
            long,
            help = "Whole seconds within the served offer; defaults to its maximum"
        )]
        seconds: Option<u64>,
        #[arg(
            long,
            help = "Failed workspace ID to recover on a fresh computer with the same Work ID"
        )]
        recovery_from: Option<String>,
    },
    /// Start quoted Work on EU compute using the reviewed computer balance or trial.
    ///
    /// Requires the confirmation printed by `work-quote` and --confirm-eu-compute,
    /// which agrees that repository code and Work files run on a cloud
    /// computer in the EU. Re-running with the same --operation-key and --confirmation is
    /// safe: it replays the launch instead of starting another computer.
    WorkLaunch {
        id: String,
        #[arg(long)]
        operation_key: String,
        /// Confirmation printed by `work-quote`; use - to read bounded piped stdin.
        #[arg(long)]
        confirmation: String,
        /// Agree that repository code and Work files run on EU compute.
        #[arg(long)]
        confirm_eu_compute: bool,
        #[arg(
            long,
            help = "Exact whole seconds printed by work-quote; default is what your current offer allows"
        )]
        seconds: Option<u64>,
        #[arg(long, help = "Exact failed workspace ID used for the recovery quote")]
        recovery_from: Option<String>,
    },
}

/// What a cancellation does with prompts still waiting behind the Work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum WorkQueueChoice {
    Discard,
    Park,
}

#[derive(Debug, Subcommand)]
enum CloudComputersCommand {
    /// List this account's Computers; --json includes allowance and entitlement data.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Save a Computer identity; compute is allocated only when it starts.
    Create {
        name: String,
        /// Try cloud compute in the EU for this Computer.
        #[arg(long, alias = "boat-trial", requires = "eu_compute_opt_in")]
        computer_trial: bool,
        /// Confirm that trial code and files run on EU compute.
        #[arg(long, requires = "computer_trial")]
        eu_compute_opt_in: bool,
    },
    /// Show one Computer; --json includes allowance and meter data.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Read trial usage receipts for one Computer.
    Usage {
        id: String,
        /// Print the full metering response.
        #[arg(long, required = true)]
        json: bool,
    },
    /// Start a Computer, subject to the account's plan and capacity.
    Start { id: String },
    /// Pause a Computer.
    Pause { id: String },
    /// Permanently delete a Computer and its provider allocation.
    Delete {
        id: String,
        #[arg(
            long,
            required = true,
            help = "Confirm discarding files still on this computer"
        )]
        discard_files: bool,
    },
    #[command(
        about = "Save and restore computer files, or inspect and download account file versions"
    )]
    Files {
        #[command(subcommand)]
        command: CloudComputerFilesCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CloudComputerFilesCommand {
    #[command(about = "Save a nonempty workspace file up to 16 MiB to account storage")]
    Save {
        computer_id: String,
        path: String,
        #[arg(
            long,
            help = "Stable save request ID; reuse it when retrying the same save"
        )]
        request_id: String,
        #[arg(long, requires = "expected_version")]
        file_id: Option<String>,
        #[arg(long, requires = "file_id", value_parser = clap::value_parser!(u64).range(1..=MAX_FILE_VERSION))]
        expected_version: Option<u64>,
        #[arg(long)]
        expected_revision: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    #[command(
        about = "Restore an account file version up to 4 MiB; existing destinations need --expected-revision"
    )]
    Restore {
        computer_id: String,
        file_id: String,
        path: String,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=MAX_FILE_VERSION))]
        version: u64,
        #[arg(
            long,
            help = "Current destination SHA-256, required to replace an existing file"
        )]
        expected_revision: Option<String>,
    },
    #[command(about = "Read a computer workspace file's size and current SHA-256 revision")]
    Revision { computer_id: String, path: String },
    #[command(about = "List an account file's saved versions, sizes and SHA-256 hashes")]
    Versions { file_id: String },
    #[command(
        about = "Download and verify one account file version without overwriting local files"
    )]
    Download {
        file_id: String,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=MAX_FILE_VERSION))]
        version: u64,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum CloudKeysCommand {
    /// List configured providers without revealing key values.
    List,
    /// Save a provider key to the signed-in Codewhale account.
    Set(CloudKeySetArgs),
    /// Remove a provider key from the signed-in Codewhale account.
    Remove {
        /// Provider id from the account's catalog (`account keys list`).
        provider: String,
    },
}

#[derive(Debug, Args)]
struct CloudKeySetArgs {
    /// Provider id from the account's catalog (`account keys list`).
    provider: String,
    /// Read the key from stdin. Useful for pipes and secret-manager commands.
    #[arg(long = "api-key-stdin", conflicts_with = "from_local")]
    api_key_stdin: bool,
    /// Upload the locally resolved key (config, secret store, then environment).
    #[arg(long, conflicts_with = "api_key_stdin")]
    from_local: bool,
    /// Non-secret label shown beside the stored credential.
    #[arg(long, default_value = "Codewhale CLI")]
    label: String,
}

/// One row of the account control plane's public provider catalog.
///
/// This is untrusted remote data, not a Codewhale-owned enum: the account
/// service adds providers without a CLI release, so the catalog is read as
/// data and every id is re-validated locally before it reaches a URL path.
/// Only the fields this surface actually uses are modeled; unknown fields are
/// ignored rather than being turned into behavior.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogProvider {
    id: String,
    #[serde(default)]
    label: String,
    /// The runtime provider id this catalog row maps onto, when one exists.
    /// `--from-local` uses it to find the local credential; without it the
    /// row's own id is tried.
    #[serde(default)]
    runtime_provider: Option<String>,
    /// Model ids the account API lists for this provider. `new-thread` checks
    /// its explicit route against this served list, not a compiled table.
    #[serde(default)]
    models: Vec<String>,
    /// False for runtime-only rows a hosted conversation cannot use.
    #[serde(default)]
    connection_available: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ProviderCatalogResponse {
    #[serde(default)]
    providers: Vec<CatalogProvider>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountComputer {
    id: String,
    owner_id: String,
    name: String,
    region: String,
    status: String,
    #[serde(default)]
    start_queue_reason: String,
}

#[derive(Deserialize)]
struct ComputerListResponse {
    computers: Vec<AccountComputer>,
}

#[derive(Deserialize)]
struct ComputerResponse {
    computer: AccountComputer,
    #[serde(default)]
    queued: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComputerDeleteResponse {
    deleted: bool,
    computer_id: String,
}

#[derive(Deserialize)]
struct AccountFileVersion {
    version: u64,
    size: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct AccountProject {
    id: String,
    name: String,
    #[serde(rename = "defaultRepoProvider", default)]
    default_repo_provider: String,
    #[serde(rename = "defaultRepo", default)]
    default_repo: String,
}

#[derive(Deserialize)]
struct ProjectListResponse {
    projects: Vec<AccountProject>,
}

#[derive(Deserialize)]
struct ProjectResponse {
    project: AccountProject,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountGitHubBinding {
    id: String,
    provider: String,
    repo: String,
    status: String,
    #[serde(default)]
    installation_id: String,
}

#[derive(Deserialize)]
struct GitHubBindingListResponse {
    bindings: Vec<AccountGitHubBinding>,
}

#[derive(Serialize)]
struct ComputerCreateRequest<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'static str>,
    #[serde(rename = "boatEuComputeOptIn", skip_serializing_if = "Option::is_none")]
    boat_eu_compute_opt_in: Option<bool>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountAgent {
    id: String,
    name: String,
    status: String,
    #[serde(default)]
    project_id: String,
    revision: u64,
}

#[derive(Deserialize)]
struct AgentListResponse {
    agents: Vec<AccountAgent>,
}

#[derive(Deserialize)]
struct AgentResponse {
    agent: AccountAgent,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentThread {
    id: String,
    agent_id: String,
    #[serde(default)]
    project_id: String,
    title: String,
    model: String,
    model_provider: String,
    #[serde(default)]
    model_provider_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    archived_at: String,
}

#[derive(Deserialize)]
struct ThreadResponse {
    thread: AgentThread,
}

#[derive(Deserialize)]
struct TurnResponse {
    turn: TurnReceipt,
}

#[derive(Deserialize)]
struct TurnReceipt {
    id: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountAgentWork {
    id: String,
    agent_id: String,
    status: String,
    #[serde(default)]
    objective: String,
    #[serde(default)]
    task_context_digest: String,
}

#[derive(Serialize)]
struct WorkTaskContext {
    text: String,
    #[serde(rename = "fileRefs")]
    file_refs: Vec<WorkFileRef>,
}

#[derive(Serialize)]
struct WorkFileRef {
    #[serde(rename = "fileId")]
    file_id: String,
    version: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentWorkMessageResponse {
    intent: String,
    /// Why the Agent read the message that way (a server-owned code).
    #[serde(default)]
    reason: String,
    /// The action applied to active Work when `intent` is `control`.
    #[serde(default)]
    control_action: Option<String>,
    #[serde(default)]
    confident: Option<bool>,
    /// What the Agent offers to do when it declined to act on an unclear message.
    #[serde(default)]
    suggestion: Option<String>,
    work: Option<AccountAgentWork>,
    #[serde(default)]
    queued_work: Vec<AccountAgentWork>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentTurnEvent {
    seq: u64,
    #[serde(rename = "type")]
    kind: String,
    turn_id: String,
    payload: serde_json::Value,
}

struct AgentTurnResult {
    last_seq: u64,
    status: String,
    answer: String,
    seen_turn: bool,
}

impl CatalogProvider {
    /// Non-empty display label, falling back to the id.
    fn display_label(&self) -> String {
        let label = printable(&self.label);
        if label.is_empty() {
            printable(&self.id)
        } else {
            label
        }
    }

    /// The local [`ProviderKind`] this catalog row maps onto, if any.
    ///
    /// The catalog states its own runtime mapping (`xiaomi` →
    /// `xiaomi-mimo`); the row id is only a fallback for a provider whose
    /// catalog id already equals the runtime id.
    fn local_kind(&self) -> Option<ProviderKind> {
        self.runtime_provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(ProviderKind::parse_config_identity)
            .or_else(|| ProviderKind::parse_config_identity(&self.id))
    }
}

/// Accept a provider id conservatively before it is ever put in a URL path.
///
/// `^[a-z0-9][a-z0-9-]{0,63}$`. The catalog is remote data, so this guards
/// both directions: a hostile catalog cannot smuggle a path segment, and a
/// mistyped argument fails locally instead of as a confusing 404.
fn validate_provider_id(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    let well_formed = (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
    if !well_formed {
        bail!(
            "`{}` is not a valid provider id. Ids are 1-64 characters of lowercase letters, digits, and `-`. Run `codewhale account keys list` to see the account's providers",
            printable(trimmed)
        );
    }
    Ok(trimmed.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

pub(crate) struct CloudRequest {
    method: HttpMethod,
    path: String,
    bearer: Option<String>,
    body: Option<Vec<u8>>,
}

pub(crate) struct CloudResponse {
    status: u16,
    body: Vec<u8>,
    /// `Retry-After` in whole seconds, when the service supplied one. Kept on
    /// the response rather than re-parsed by callers so the retry policy has a
    /// single source for how long the server asked us to wait.
    retry_after: Option<u64>,
}

pub(crate) trait CloudTransport {
    fn execute(&self, request: CloudRequest) -> Result<CloudResponse>;
}

struct ReqwestTransport {
    base: Url,
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    fn new(base: Url) -> Result<Self> {
        let client = codewhale_release::platform_blocking_http_client_builder()
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(30))
            // Never replay bearer tokens or provider-key request bodies to a
            // redirect target. The control-plane origin is an explicit trust
            // boundary, so redirects are treated as ordinary non-2xx replies.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("codewhale/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to initialize the Codewhale account HTTP client")?;
        Ok(Self { base, client })
    }
}

impl CloudTransport for ReqwestTransport {
    fn execute(&self, request: CloudRequest) -> Result<CloudResponse> {
        let url = self
            .base
            .join(request.path.trim_start_matches('/'))
            .context("failed to construct the Codewhale account request URL")?;
        let file_download = request.method == HttpMethod::Get
            && url
                .path()
                .strip_prefix("/v1/files/")
                .and_then(|path| path.strip_suffix("/content"))
                .is_some_and(|id| validate_account_file_id(id).is_ok());
        let max_response_bytes = if file_download {
            MAX_FILE_DOWNLOAD_BYTES
        } else {
            MAX_RESPONSE_BYTES
        };
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, url).header(
            reqwest::header::ACCEPT,
            if file_download {
                "application/octet-stream"
            } else {
                "application/json"
            },
        );
        // Hosted launch waits for provider creation and guest bootstrap. Its
        // HTTP deadline is separate from the guest's bounded trial runtime.
        if request.method == HttpMethod::Post && request.path == "/api/cloud-sessions" {
            builder = builder.timeout(Duration::from_secs(600));
        }
        if let Some(token) = request.bearer {
            builder = builder.bearer_auth(token);
        }
        if let Some(body) = request.body {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = builder.send().map_err(|source| {
            CloudTransportError::new("could not reach the Codewhale service", source)
        })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let mut body = Vec::new();
        response
            .take(max_response_bytes + 1)
            .read_to_end(&mut body)
            .map_err(|source| {
                CloudTransportError::new("failed to read the Codewhale service response", source)
            })?;
        if body.len() as u64 > max_response_bytes {
            return Err(CloudTransportError::new(
                "The Codewhale service returned an unexpectedly large response",
                std::io::Error::other("response exceeded the account API size limit"),
            )
            .into());
        }
        Ok(CloudResponse {
            status,
            body,
            retry_after,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct MeResponse {
    user: CloudUser,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceTokenRequest<'a> {
    device_code: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshRequest<'a> {
    refresh_token: &'a str,
}

#[derive(Serialize)]
struct ModelKeyRequest<'a> {
    key: &'a str,
    label: &'a str,
}

pub(crate) struct CloudClient<'a, T: CloudTransport> {
    transport: &'a T,
    account_store: AccountSessionStore,
}

impl<'a, T: CloudTransport> CloudClient<'a, T> {
    fn new(transport: &'a T, secrets: &'a Secrets, profile: &str, api_base: &'a str) -> Self {
        Self {
            transport,
            account_store: AccountSessionStore::new(secrets.clone(), Some(profile), api_base),
        }
    }

    fn start_device(&self) -> Result<DeviceStart> {
        let response = self.transport.execute(CloudRequest {
            method: HttpMethod::Post,
            path: "/api/cli/device/start".to_string(),
            bearer: None,
            body: Some(b"{}".to_vec()),
        })?;
        expect_json(response, &[200])
    }

    fn poll_device(
        &self,
        device: &DeviceStart,
        timeout: Duration,
        sleep: &mut dyn FnMut(Duration),
    ) -> Result<AuthBundle> {
        validate_device_code(&device.device_code)?;
        let server_lifetime =
            Duration::from_secs(device.expires_in.clamp(1, MAX_LOGIN_TIMEOUT_SECONDS));
        // The Codewhale account service answers HTTP 202 while the code is
        // still pending, so the first response is already meaningful: poll
        // immediately and sleep afterwards. It has no slow_down.
        let bundle = codewhale_config::device_code::DeviceCodePoll::new(
            timeout.min(server_lifetime),
            "Codewhale account login timed out; run `codewhale login` to try again",
        )
        .interval_seconds(Some(device.interval))
        .max_interval_seconds(10)
        .run(sleep, || {
            let response = self.transport.execute(CloudRequest {
                method: HttpMethod::Post,
                path: "/api/cli/device/token".to_string(),
                bearer: None,
                body: Some(json_body(&DeviceTokenRequest {
                    device_code: &device.device_code,
                })?),
            })?;
            match response.status {
                200 => {
                    let bundle: AuthBundle = parse_json_body(&response.body)?;
                    validate_auth_bundle(&bundle)?;
                    Ok(DevicePollOutcome::Complete(bundle))
                }
                202 => Ok(DevicePollOutcome::Pending),
                _ => Err(response_error(&response)),
            }
        })?;
        self.save_auth(bundle.clone())?;
        Ok(bundle)
    }

    fn load_auth(&self) -> Result<Option<StoredCloudAuth>> {
        self.account_store.load().context(
            "the local Codewhale account session is unreadable; run `codewhale account logout` and sign in again",
        )
    }

    fn save_auth(&self, bundle: AuthBundle) -> Result<()> {
        self.account_store
            .save(bundle)
            .context("failed to save the Codewhale account session in the local secret store")
    }

    fn me(&self) -> Result<CloudUser> {
        let (response, snapshot) =
            self.execute_authenticated_snapshot(HttpMethod::Get, "/api/me", None)?;
        let me: MeResponse = expect_json(response, &[200])?;
        if me.user.id.trim().is_empty() {
            bail!("The Codewhale service returned an account without an ID");
        }
        if let Some(mut stored) = snapshot.load()? {
            if stored
                .bundle
                .user
                .as_ref()
                .is_some_and(|user| !user.id.is_empty() && user.id != me.user.id)
            {
                bail!("The signed-in account changed. Refresh and try again.");
            }
            stored.bundle.user = Some(me.user.clone());
            if self
                .account_store
                .save_if_unchanged(&snapshot, stored.bundle)?
                .is_none()
            {
                bail!("The signed-in account changed. Refresh and try again.");
            }
        }
        Ok(me.user)
    }

    fn set_key(&self, provider: &str, key: &str, label: &str) -> Result<()> {
        let path = format!("/api/model-keys/{provider}");
        let response = self.execute_authenticated(
            HttpMethod::Put,
            &path,
            Some(json_body(&ModelKeyRequest { key, label })?),
        )?;
        expect_empty(response, &[200, 201])
    }

    fn remove_key(&self, provider: &str) -> Result<()> {
        let path = format!("/api/model-keys/{provider}");
        let response = self.execute_authenticated(HttpMethod::Delete, &path, None)?;
        expect_empty(response, &[200, 204])
    }

    /// The account control plane's public provider catalog.
    ///
    /// This replaced a hardcoded eight-provider enum: the set of providers a
    /// customer can connect is owned by the control plane, not the CLI, so a
    /// newly supported provider must not need a CLI release. The route is
    /// public, so no session is required to *list* what could be connected —
    /// only to read or write this account's keys.
    ///
    /// Rows with an id this CLI would refuse to put in a URL path are dropped
    /// rather than trusted; duplicates collapse onto the first row.
    fn provider_catalog(&self) -> Result<Vec<CatalogProvider>> {
        let response = self.transport.execute(CloudRequest {
            method: HttpMethod::Get,
            path: "/api/model-providers".to_string(),
            bearer: None,
            body: None,
        })?;
        let listing: ProviderCatalogResponse = expect_json(response, &[200])?;
        let mut seen = std::collections::BTreeSet::new();
        let providers: Vec<CatalogProvider> = listing
            .providers
            .into_iter()
            .filter(|row| validate_provider_id(&row.id).is_ok())
            .filter(|row| seen.insert(row.id.trim().to_string()))
            .map(|mut row| {
                row.id = row.id.trim().to_string();
                row
            })
            .collect();
        if providers.is_empty() {
            bail!(
                "The Codewhale service returned no connectable providers. Check the account API origin, or try again"
            );
        }
        Ok(providers)
    }

    fn computers(&self) -> Result<serde_json::Value> {
        let response = self.execute_authenticated(HttpMethod::Get, "/api/computers", None)?;
        expect_json(response, &[200])
    }

    fn agents(&self) -> Result<serde_json::Value> {
        let response = self.execute_authenticated(HttpMethod::Get, "/api/agents", None)?;
        expect_json(response, &[200])
    }

    fn projects(&self) -> Result<serde_json::Value> {
        let response = self.execute_authenticated(HttpMethod::Get, "/api/projects", None)?;
        expect_json(response, &[200])
    }

    fn github_bindings(&self) -> Result<serde_json::Value> {
        let response =
            self.execute_authenticated(HttpMethod::Get, "/api/integrations/github/bindings", None)?;
        if matches!(response.status, 404 | 503) {
            return Err(response_error(&response)).context(
                "GitHub repository bindings are unavailable on this Codewhale API; the account GitHub route must be attached before CLI setup",
            );
        }
        expect_json(response, &[200])
    }

    fn create_github_project(
        &self,
        name: &str,
        binding: &AccountGitHubBinding,
        operation_key: &str,
    ) -> Result<AccountProject> {
        let name = validate_named_text(name, "Project name", 80)?;
        let operation_key = validate_operation_key(operation_key)?;
        let response = self.execute_authenticated(
            HttpMethod::Post,
            "/api/projects",
            Some(json_body(&serde_json::json!({
                "name": name,
                "defaultMode": "chat",
                "chatFilesystem": "optional_scratch",
                "repoBindingId": binding.id,
                "operationKey": operation_key,
            }))?),
        )?;
        let result: ProjectResponse = expect_json(response, &[200, 201])?;
        validate_resource_id(&result.project.id, "Project")?;
        if result.project.default_repo_provider != "github"
            || !result
                .project
                .default_repo
                .eq_ignore_ascii_case(&binding.repo)
        {
            bail!("The Codewhale service returned a Project for a different GitHub repository");
        }
        Ok(result.project)
    }

    fn create_agent(
        &self,
        name: &str,
        project_id: Option<&str>,
        operation_key: &str,
    ) -> Result<AccountAgent> {
        let name = validate_named_text(name, "Agent name", 80)?;
        let operation_key = validate_operation_key(operation_key)?;
        let mut body = serde_json::json!({
            "name": name,
            "operationKey": operation_key,
        });
        if let Some(project_id) = project_id {
            body["projectId"] = serde_json::json!(validate_resource_id(project_id, "Project")?);
        }
        let response =
            self.execute_authenticated(HttpMethod::Post, "/api/agents", Some(json_body(&body)?))?;
        let result: AgentResponse = expect_json(response, &[200, 201])?;
        Ok(result.agent)
    }

    fn bind_agent_project(
        &self,
        agent_id: &str,
        project_id: &str,
        revision: u64,
    ) -> Result<AccountAgent> {
        let agent_id = validate_resource_id(agent_id, "Agent")?;
        let project_id = validate_resource_id(project_id, "Project")?;
        if revision == 0 {
            bail!("The Codewhale service returned an Agent without a valid revision");
        }
        let response = self.execute_authenticated(
            HttpMethod::Patch,
            &format!("/api/agents/{agent_id}"),
            Some(json_body(&serde_json::json!({
                "projectId": project_id,
                "revision": revision,
            }))?),
        )?;
        let result: AgentResponse = expect_json(response, &[200])?;
        if result.agent.id != agent_id || result.agent.project_id != project_id {
            bail!("The Codewhale service returned an unexpected Agent Project binding");
        }
        Ok(result.agent)
    }

    fn threads(&self, agent_id: &str) -> Result<Vec<AgentThread>> {
        let agent_id = validate_resource_id(agent_id, "Agent")?;
        let response = self.execute_authenticated(
            HttpMethod::Get,
            &format!("/v1/threads/summary?agentId={agent_id}&limit=100"),
            None,
        )?;
        expect_json(response, &[200])
    }

    fn thread(&self, id: &str) -> Result<AgentThread> {
        let id = validate_resource_id(id, "Conversation")?;
        let response =
            self.execute_authenticated(HttpMethod::Get, &format!("/v1/threads/{id}"), None)?;
        let result: ThreadResponse = expect_json(response, &[200])?;
        Ok(result.thread)
    }

    fn create_agent_thread(
        &self,
        agent_id: &str,
        project_id: Option<&str>,
        title: &str,
        provider: &str,
        model: &str,
        operation_key: &str,
    ) -> Result<AgentThread> {
        let agent_id = validate_resource_id(agent_id, "Agent")?;
        let title = validate_named_text(title, "Conversation title", 120)?;
        let (provider, model) = validate_model_route(provider, model)?;
        let operation_key = validate_operation_key(operation_key)?;
        let mut body = serde_json::json!({
            "title": title,
            "productMode": "chat",
            "mode": "chat",
            "agentId": agent_id,
            "modelProvider": provider,
            "model": model,
            "operationKey": operation_key,
        });
        if let Some(project_id) = project_id {
            body["projectId"] = serde_json::json!(validate_resource_id(project_id, "Project")?);
        }
        let response =
            self.execute_authenticated(HttpMethod::Post, "/v1/threads", Some(json_body(&body)?))?;
        let result: ThreadResponse = expect_json(response, &[200, 201])?;
        Ok(result.thread)
    }

    fn send_agent_turn(
        &self,
        thread: &AgentThread,
        prompt: &str,
        billing_mode: &str,
        operation_key: &str,
    ) -> Result<TurnReceipt> {
        let thread_id = validate_resource_id(&thread.id, "Conversation")?;
        let prompt = prompt.trim();
        if prompt.is_empty() || prompt.chars().count() > 32_000 {
            bail!("Message must contain 1-32000 characters");
        }
        let billing_mode = validate_billing_mode(billing_mode)?;
        let (provider, model) = validate_model_route(&thread.model_provider, &thread.model)?;
        let operation_key = validate_operation_key(operation_key)?;
        let response = self.execute_authenticated(
            HttpMethod::Post,
            &format!("/v1/threads/{thread_id}/turns"),
            Some(json_body(&serde_json::json!({
                "prompt": prompt,
                "billingMode": billing_mode,
                "modelProvider": provider,
                "modelProviderId": thread.model_provider_id.as_str(),
                "model": model,
                "requiresByok": billing_mode == "byok_external",
                "mode": "chat",
                "productMode": "chat",
                "operationKey": operation_key,
                "sourceMessageId": operation_key,
            }))?),
        )?;
        let result: TurnResponse = expect_json(response, &[200, 201, 202])?;
        Ok(result.turn)
    }

    fn agent_turn_result(
        &self,
        thread_id: &str,
        turn_id: &str,
        since_seq: u64,
    ) -> Result<AgentTurnResult> {
        let thread_id = validate_resource_id(thread_id, "Conversation")?;
        let turn_id = validate_turn_id(turn_id)?;
        if since_seq > i64::MAX as u64 {
            bail!("Event sequence is too large");
        }
        let response = self.execute_authenticated(
            HttpMethod::Get,
            &format!("/v1/threads/{thread_id}/events?since_seq={since_seq}"),
            None,
        )?;
        if response.status != 200 {
            return Err(response_error(&response));
        }
        parse_agent_turn_events(&response.body, turn_id, since_seq)
    }

    fn assign_agent_work(
        &self,
        agent_id: &str,
        objective: &str,
        message_id: &str,
        task_context: Option<&WorkTaskContext>,
    ) -> Result<AgentWorkMessageResponse> {
        let agent_id = validate_resource_id(agent_id, "Agent")?;
        let objective = objective.trim();
        if objective.is_empty() || objective.encode_utf16().count() > 32_000 {
            bail!("Work objective must contain 1-32000 characters");
        }
        let message_id = validate_operation_key(message_id)?;
        let mut body = serde_json::json!({ "messageId": message_id, "text": objective });
        if let Some(context) = task_context {
            body["taskContext"] = serde_json::to_value(context)?;
        }
        let response = self.execute_authenticated(
            HttpMethod::Post,
            &format!("/api/agents/{agent_id}/messages"),
            Some(json_body(&body)?),
        )?;
        if ![200, 201, 202].contains(&response.status) {
            return Err(response_error(&response));
        }
        // A 2xx the client cannot read means the service acted: an unknown
        // outcome (replay with the same message id), not a plain failure.
        serde_json::from_slice(&response.body).map_err(|source| {
            CloudTransportError::new(
                "The Codewhale service returned an unreadable reply to this message",
                source,
            )
            .into()
        })
    }

    fn agent_work_status(&self, id: &str) -> Result<serde_json::Value> {
        let id = validate_resource_id(id, "Work")?;
        let response =
            self.execute_authenticated(HttpMethod::Get, &format!("/api/runs/{id}"), None)?;
        expect_json(response, &[200])
    }

    fn computer(&self, id: &str) -> Result<serde_json::Value> {
        let id = validate_computer_id(id)?;
        let response =
            self.execute_authenticated(HttpMethod::Get, &format!("/api/computers/{id}"), None)?;
        expect_json(response, &[200])
    }

    fn computer_usage(&self, id: &str) -> Result<serde_json::Value> {
        let id = validate_computer_id(id)?;
        let response = self.execute_authenticated(
            HttpMethod::Get,
            &format!("/api/computers/{id}/usage"),
            None,
        )?;
        expect_json(response, &[200])
    }

    fn create_computer(
        &self,
        name: &str,
        computer_trial: bool,
        eu_compute_opt_in: bool,
    ) -> Result<AccountComputer> {
        if computer_trial != eu_compute_opt_in {
            bail!("A computer trial requires both --computer-trial and --eu-compute-opt-in");
        }
        let name = validate_computer_name(name)?;
        let response = self.execute_authenticated(
            HttpMethod::Post,
            "/api/computers",
            Some(json_body(&ComputerCreateRequest {
                name: &name,
                provider: computer_trial.then_some("boat"),
                boat_eu_compute_opt_in: computer_trial.then_some(true),
            })?),
        )?;
        let result: ComputerResponse = expect_json(response, &[200, 201])?;
        Ok(result.computer)
    }

    fn computer_action(&self, id: &str, action: &str) -> Result<ComputerResponse> {
        let id = validate_computer_id(id)?;
        let response = self.execute_authenticated(
            HttpMethod::Post,
            &format!("/api/computers/{id}/{action}"),
            None,
        )?;
        let status = response.status;
        let result: ComputerResponse = expect_json(
            response,
            if action == "start" {
                &[200, 202]
            } else {
                &[200]
            },
        )?;
        if status == 202 && !result.queued {
            bail!("The Codewhale service returned a queued start without queue details");
        }
        Ok(result)
    }

    fn delete_computer(&self, id: &str) -> Result<()> {
        let id = validate_computer_id(id)?;
        let response = self.execute_authenticated(
            HttpMethod::Delete,
            &format!("/api/computers/{id}"),
            Some(json_body(&serde_json::json!({ "discardFiles": true }))?),
        )?;
        let result: ComputerDeleteResponse = expect_json(response, &[200])?;
        if !result.deleted || result.computer_id != id {
            bail!("The Codewhale service did not confirm deletion of Computer {id}");
        }
        Ok(())
    }

    fn computer_file_revision(&self, computer_id: &str, path: &str) -> Result<serde_json::Value> {
        let id = validate_computer_id(computer_id)?;
        validate_computer_file_path(path)?;
        let mut url = Url::parse("https://codewhale.invalid/")?;
        url.set_path(&format!("/api/computers/{id}/files/revision"));
        url.query_pairs_mut().append_pair("path", path);
        let response = self.execute_authenticated(
            HttpMethod::Get,
            &format!("{}?{}", url.path(), url.query().unwrap_or_default()),
            None,
        )?;
        let value: serde_json::Value = expect_json(response, &[200])?;
        if value["computerId"].as_str() != Some(id)
            || value["path"].as_str() != Some(path)
            || value["size"].as_u64().is_none()
        {
            bail!("The Codewhale service returned an invalid file revision");
        }
        validate_file_revision(value["revision"].as_str().unwrap_or_default())?;
        Ok(value)
    }

    fn computer_file_action(
        &self,
        computer_id: &str,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = validate_computer_id(computer_id)?;
        let response = self.execute_authenticated(
            HttpMethod::Post,
            &format!("/api/computers/{id}/files/{action}"),
            Some(json_body(&payload)?),
        )?;
        expect_json(response, &[200, 201])
    }

    fn account_file(&self, file_id: &str) -> Result<serde_json::Value> {
        let id = validate_account_file_id(file_id)?;
        let response =
            self.execute_authenticated(HttpMethod::Get, &format!("/v1/files/{id}"), None)?;
        let value: serde_json::Value = expect_json(response, &[200])?;
        if value["file"]["id"].as_str() != Some(id) {
            bail!("The Codewhale service returned a different account file");
        }
        Ok(value)
    }

    fn download_account_file(&self, file_id: &str, version: u64, output: &Path) -> Result<()> {
        let id = validate_account_file_id(file_id)?;
        match std::fs::symlink_metadata(output) {
            Ok(_) => bail!("The output path already exists; choose another file name"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("could not inspect the output path"),
        }
        let metadata = account_file_version(&self.account_file(id)?, version)?;
        let response = self.execute_authenticated(
            HttpMethod::Get,
            &format!("/v1/files/{id}/content?version={version}"),
            None,
        )?;
        if response.status != 200 {
            return Err(response_error(&response));
        }
        if response.body.len() as u64 != metadata.size
            || crate::update::sha256_hex(&response.body) != metadata.sha256
        {
            bail!(
                "The downloaded file did not match its saved size and SHA-256; nothing was written"
            );
        }
        let parent = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut pending = tempfile::NamedTempFile::new_in(parent)
            .context("could not prepare the download beside its destination")?;
        pending
            .write_all(&response.body)
            .context("could not write the verified download")?;
        pending
            .as_file()
            .sync_all()
            .context("could not sync the verified download")?;
        pending
            .persist_noclobber(output)
            .context("could not save the download without replacing an existing file")?;
        Ok(())
    }

    fn logout(&self) -> Result<bool> {
        let snapshot = self.account_store.snapshot()?;
        self.account_store
            .with_transaction(|transaction| -> Result<bool> {
                if !transaction.matches(&snapshot) {
                    bail!("The signed-in account changed. Refresh and try again.");
                }
                let stored = match transaction.load() {
                    Ok(Some(stored)) => stored,
                    Ok(None) | Err(_) => {
                        transaction.clear();
                        return Ok(false);
                    }
                };
                let body = json_body(&RefreshRequest {
                    refresh_token: &stored.bundle.refresh_token,
                })?;
                let response = self.transport.execute(CloudRequest {
                    method: HttpMethod::Post,
                    path: "/api/auth/logout".into(),
                    bearer: None,
                    body: Some(body),
                })?;
                if (200..300).contains(&response.status) || matches!(response.status, 401 | 403) {
                    transaction.clear();
                    return Ok((200..300).contains(&response.status));
                }
                // Keep custody on transient failure so revocation can be retried.
                Err(response_error(&response))
            })
    }

    fn execute_authenticated(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<CloudResponse> {
        self.execute_authenticated_snapshot(method, path, body)
            .map(|(response, _)| response)
    }

    fn execute_authenticated_snapshot(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(CloudResponse, AccountSessionSnapshot)> {
        let snapshot = self.account_store.snapshot()?;
        let Some(mut stored) = snapshot.load()? else {
            bail!("Not signed in. Run `codewhale login` first");
        };
        let first = self.transport.execute(CloudRequest {
            method,
            path: path.into(),
            bearer: Some(stored.bundle.access_token.clone()),
            body: body.clone(),
        })?;
        if first.status != 401 {
            return Ok((first, snapshot));
        }
        // Serialize the refresh HTTP request itself with native/CLI writers:
        // two processes must not spend the same rotating refresh token.
        let renewed = self
            .account_store
            .with_transaction(|transaction| -> Result<_> {
                if !transaction.matches(&snapshot) {
                    bail!("The signed-in account changed. Refresh and try again.");
                }
                let refresh = self.transport.execute(CloudRequest {
                    method: HttpMethod::Post,
                    path: "/api/auth/refresh".into(),
                    bearer: None,
                    body: Some(json_body(&RefreshRequest {
                        refresh_token: &stored.bundle.refresh_token,
                    })?),
                })?;
                match refresh.status {
                    200 => {}
                    401 => {
                        transaction.clear();
                        return Ok(None);
                    }
                    _ => return Err(response_error(&refresh)),
                }
                let mut next: AuthBundle = parse_json_body(&refresh.body)?;
                validate_auth_bundle(&next)?;
                if next.user.is_none() {
                    next.user = stored.bundle.user.take();
                }
                if next.session.is_none() {
                    next.session = stored.bundle.session.take();
                }
                transaction.replace(next.clone())?;
                Ok(Some((next, transaction.snapshot())))
            })?;
        let Some((next, next_snapshot)) = renewed else {
            bail!("The Codewhale account session expired. Run `codewhale login` again");
        };
        // The rotated token is durable before a potentially failing retry.
        let retried = self.transport.execute(CloudRequest {
            method,
            path: path.into(),
            bearer: Some(next.access_token),
            body,
        })?;
        if retried.status == 401 {
            self.account_store.clear_if_unchanged(&next_snapshot)?;
            bail!("The Codewhale account session expired. Run `codewhale login` again");
        }
        Ok((retried, next_snapshot))
    }

    /// Whether an interactive session exists for this profile and origin.
    ///
    /// A management command asks this before it asks anything of the network,
    /// so "you have a machine key but no login" is answered locally instead of
    /// as a 403 from a route the key was never allowed to touch.
    fn has_session(&self) -> Result<bool> {
        Ok(self.load_auth()?.is_some())
    }

    /// `execute_authenticated`, retrying only what the caller marks replayable.
    ///
    /// `machine::Retry::Never` is not a default worth having: the one POST in
    /// this surface mints a secret shown exactly once, so a retry that quietly
    /// succeeded server-side would leave an unrevocable key behind.
    fn execute_authenticated_with_retry(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Vec<u8>>,
        retry: machine::Retry,
        sleeper: &mut dyn FnMut(Duration),
    ) -> Result<CloudResponse> {
        let max_attempts = if retry == machine::Retry::Idempotent {
            3
        } else {
            1
        };
        let mut attempt = 1;
        loop {
            let response = self.execute_authenticated(method, path, body.clone())?;
            if (200..300).contains(&response.status) || attempt >= max_attempts {
                return Ok(response);
            }
            let retry_after = response.retry_after;
            if !machine::classify(&response).retryable {
                return Ok(response);
            }
            sleeper(machine::backoff_delay(attempt, retry_after));
            attempt += 1;
        }
    }
}

fn validate_computer_id(value: &str) -> Result<&str> {
    validate_resource_id(value, "Computer")
}

fn validate_account_file_id(value: &str) -> Result<&str> {
    let suffix = value.strip_prefix("file_").unwrap_or_default();
    if !(16..=64).contains(&suffix.len())
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("Account file ID must be file_ followed by 16-64 letters, digits, `-`, or `_`");
    }
    Ok(value)
}

fn validate_computer_file_path(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > 4096
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        || value.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        bail!(
            "Choose a relative file path inside the computer workspace, without `.` or `..` segments"
        );
    }
    Ok(())
}

fn validate_file_revision(value: &str) -> Result<&str> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        bail!("File revision must be a lowercase SHA-256 hash of 64 characters");
    }
    Ok(value)
}

fn account_file_version(value: &serde_json::Value, version: u64) -> Result<AccountFileVersion> {
    if !(1..=MAX_FILE_VERSION).contains(&version) {
        bail!("Choose an explicit positive saved file version");
    }
    let versions: Vec<AccountFileVersion> = serde_json::from_value(value["versions"].clone())
        .context("The Codewhale service returned invalid file versions")?;
    let mut matches = versions
        .into_iter()
        .filter(|entry| entry.version == version);
    let selected = matches
        .next()
        .ok_or_else(|| anyhow!("That saved file version is unavailable"))?;
    if matches.next().is_some() || selected.size == 0 || selected.size > MAX_FILE_DOWNLOAD_BYTES {
        bail!("The Codewhale service returned invalid saved file metadata");
    }
    validate_file_revision(&selected.sha256)?;
    Ok(selected)
}

fn validate_resource_id<'a>(value: &'a str, kind: &str) -> Result<&'a str> {
    let id = value.trim();
    let bytes = id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 160
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{kind} ID must be a bounded identifier of letters, digits, `-`, or `_`");
    }
    Ok(id)
}

fn validate_turn_id(value: &str) -> Result<&str> {
    let id = value.trim();
    let bytes = id.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 160
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes.iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@')
        })
        || id.contains("..")
    {
        bail!(
            "Turn ID must be a bounded identifier of letters, digits, `-`, `_`, `.`, `:`, or `@`"
        );
    }
    Ok(id)
}

fn validate_computer_name(value: &str) -> Result<String> {
    validate_named_text(value, "Computer name", 80)
}

fn validate_named_text(value: &str, label: &str, maximum: usize) -> Result<String> {
    let text = value.trim();
    if text.is_empty() || text.chars().count() > maximum || text.chars().any(char::is_control) {
        bail!("{label} must contain 1-{maximum} characters without control characters");
    }
    Ok(text.to_string())
}

fn validate_operation_key(value: &str) -> Result<&str> {
    let key = value.trim();
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        bail!("Operation key must be 1-128 URL-safe characters");
    }
    Ok(key)
}

fn validate_billing_mode(value: &str) -> Result<&str> {
    match value.trim() {
        "byok_external" => Ok("byok_external"),
        "membership_included" | "managed_wallet" => {
            bail!(
                "Codewhale-managed model billing is unavailable under the current launch policy. Use --billing-mode byok_external with your own provider key"
            )
        }
        _ => bail!("Billing mode must be byok_external under the current launch policy"),
    }
}

fn validate_model_route<'a>(provider: &'a str, model: &'a str) -> Result<(&'a str, &'a str)> {
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty()
        || provider.len() > 128
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || model.is_empty()
        || model.len() > 256
        || !model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'+' | b'-')
        })
        || provider.contains("..")
        || model.contains("..")
    {
        bail!("Model route must contain a bounded provider and model identifier");
    }
    Ok((provider, model))
}

/// Refuse a (provider, model) the live account catalog does not list.
///
/// The catalog is served data (`GET /api/model-providers`), so a new model
/// needs no CLI release and a retired one stops being accepted here. The
/// message names the catalog and what it does list for the provider.
///
/// Returns the row's canonical id: the control plane stores that id even when
/// the caller named the runtime alias (`xiaomi-mimo` for `xiaomi`), so it is
/// the value to send and to compare the created conversation against.
fn assert_catalog_route<'a>(
    catalog: &'a [CatalogProvider],
    provider: &str,
    model: &str,
) -> Result<&'a str> {
    let row = catalog.iter().find(|row| {
        row.connection_available != Some(false)
            && (row.id == provider || row.runtime_provider.as_deref() == Some(provider))
    });
    if let Some(row) = row
        && row.models.iter().any(|listed| listed == model)
    {
        return Ok(row.id.as_str());
    }
    let listed = match row {
        Some(row) if !row.models.is_empty() => format!(
            "The catalog lists these models for `{}`: {}",
            printable(provider),
            row.models
                .iter()
                .take(12)
                .map(|model| printable(model))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(_) => format!("The catalog lists no models for `{}`", printable(provider)),
        None => format!(
            "`{}` is not a hosted provider in the catalog. Known providers: {}",
            printable(provider),
            catalog
                .iter()
                .filter(|row| row.connection_available != Some(false))
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    bail!(
        "`{}/{}` is not listed in the live Codewhale model catalog (GET /api/model-providers), so no conversation was created. {listed}",
        printable(provider),
        printable(model)
    )
}

fn work_objective_and_context(
    objective: &str,
    context_file: Option<&Path>,
    file_refs: &[String],
) -> Result<(String, Option<WorkTaskContext>)> {
    let objective = objective.trim();
    if objective.is_empty() || objective.encode_utf16().count() > 32_000 {
        bail!("Work objective must contain 1-32000 UTF-16 units");
    }
    let mut context = work::task_context(context_file, file_refs)?;
    if objective.encode_utf16().count() > 12_000 {
        let context = context.get_or_insert_with(|| WorkTaskContext {
            text: String::new(),
            file_refs: Vec::new(),
        });
        context.text = if context.text.is_empty() {
            objective.to_string()
        } else {
            format!("{objective}\n\n{}", context.text)
        };
        if context.text.encode_utf16().count() > 64_000 {
            bail!("Task source context exceeds 64000 UTF-16 units");
        }
    }
    Ok((objective.to_string(), context))
}

fn prepare_work_agent<T: CloudTransport>(
    client: &CloudClient<'_, T>,
    selector: Option<&str>,
) -> Result<AccountAgent> {
    let selected = match selector {
        Some(selector) => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let agent = resolve_account_agent(&listing.agents, selector)?;
            if !agent.project_id.is_empty() {
                return Ok(agent);
            }
            Some(agent)
        }
        None => None,
    };
    let response: AgentResponse = expect_json(
        client.execute_authenticated(
            HttpMethod::Post,
            "/api/work-box",
            Some(json_body(&serde_json::json!({}))?),
        )?,
        &[200],
    )?;
    match selected {
        Some(selected)
            if response.agent.id != selected.id || response.agent.project_id.is_empty() =>
        {
            bail!(
                "This Agent cannot use the account's repository-free task context. Use Agent {}",
                printable(&response.agent.name)
            )
        }
        None if response.agent.project_id.is_empty() => {
            bail!("The account has no default Agent for repository-free tasks")
        }
        _ => Ok(response.agent),
    }
}

fn dispatch_stamp(objective: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let seed = format!("{nanos}:{}:{objective}", std::process::id());
    crate::update::sha256_hex(seed.as_bytes())[..24].to_string()
}

pub(crate) enum AccountDispatch {
    Run {
        objective: String,
        agent: Option<String>,
        seconds: Option<u64>,
        context_file: Option<PathBuf>,
        file_refs: Vec<String>,
        operation_key: Option<String>,
        message_id: Option<String>,
        yes: bool,
        confirm_eu_compute: bool,
    },
    Status(String),
    Cancel(String),
}

pub(crate) fn run_dispatch(
    call: AccountDispatch,
    profile: Option<&str>,
    config: &mut ConfigStore,
) -> Result<()> {
    let command = match call {
        AccountDispatch::Run {
            objective,
            agent,
            seconds,
            context_file,
            file_refs,
            operation_key,
            message_id,
            yes,
            confirm_eu_compute,
        } => CloudAgentsCommand::Dispatch {
            objective,
            agent,
            seconds,
            context_file,
            file_refs,
            operation_key,
            message_id,
            yes,
            confirm_eu_compute,
        },
        AccountDispatch::Status(id) => CloudAgentsCommand::WorkStatus { id },
        AccountDispatch::Cancel(id) => CloudAgentsCommand::WorkCancel {
            id,
            queue: None,
            reason: None,
        },
    };
    run(
        CloudArgs {
            api_base: None,
            command: CloudCommand::Agents(CloudAgentsArgs { command }),
        },
        profile,
        config,
    )
}

fn resolve_account_agent(agents: &[AccountAgent], selector: &str) -> Result<AccountAgent> {
    let selector = selector.trim();
    if selector.is_empty() {
        bail!("Choose an Agent name or ID");
    }
    let by_id = agents.iter().find(|agent| agent.id == selector);
    let matches = if let Some(agent) = by_id {
        vec![agent]
    } else {
        agents
            .iter()
            .filter(|agent| agent.name == selector)
            .collect::<Vec<_>>()
    };
    if matches.len() != 1 {
        bail!(
            "Expected one Agent named `{}`; found {}. Run `codewhale account agents list` and use its ID",
            printable(selector),
            matches.len()
        );
    }
    let agent = matches[0];
    validate_resource_id(&agent.id, "Agent")?;
    if agent.status != "active" {
        bail!("Agent {} is not active", printable(&agent.name));
    }
    Ok(agent.clone())
}

fn active_agent_thread(thread: &AgentThread, agent_id: &str) -> bool {
    thread.agent_id == agent_id && thread.archived_at.is_empty() && thread.kind != "smoke"
}

fn parse_agent_turn_events(body: &[u8], turn_id: &str, since_seq: u64) -> Result<AgentTurnResult> {
    let text = std::str::from_utf8(body)
        .context("The Codewhale service returned invalid conversation events")?;
    let normalized = text.replace("\r\n", "\n");
    let mut result = AgentTurnResult {
        last_seq: since_seq,
        status: "not_seen".to_string(),
        answer: String::new(),
        seen_turn: false,
    };
    for frame in normalized.split("\n\n") {
        let mut data = None;
        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("data:")
                && data.replace(value.trim_start_matches(' ')).is_some()
            {
                bail!("The Codewhale service returned a malformed conversation event");
            }
        }
        let Some(data) = data else { continue };
        let event: AgentTurnEvent = serde_json::from_str(data)
            .context("The Codewhale service returned an invalid conversation event")?;
        if event.seq <= result.last_seq {
            bail!("The Codewhale service returned an out-of-order conversation event");
        }
        result.last_seq = event.seq;
        if event.turn_id != turn_id {
            continue;
        }
        result.seen_turn = true;
        if result.status == "not_seen" {
            result.status = "pending".to_string();
        }
        match event.kind.as_str() {
            "assistant.delta" => {
                let delta = event
                    .payload
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("The Codewhale service returned invalid assistant text")
                    })?;
                result.answer.push_str(delta);
                if result.answer.len() > 128 * 1024 {
                    bail!("The Codewhale service returned an unexpectedly long answer");
                }
            }
            "turn.completed" => {
                let status = event
                    .payload
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("completed");
                if !matches!(status, "completed" | "failed") {
                    bail!("The Codewhale service returned an invalid turn status");
                }
                result.status = status.to_string();
            }
            "turn.canceled" => result.status = "canceled".to_string(),
            _ => {}
        }
    }
    Ok(result)
}

fn write_agent_thread<W: Write>(out: &mut W, thread: &AgentThread) -> Result<()> {
    validate_resource_id(&thread.id, "Conversation")?;
    let (provider, model) = validate_model_route(&thread.model_provider, &thread.model)?;
    writeln!(out, "Conversation: {}", printable(&thread.title))?;
    writeln!(out, "ID: {}", thread.id)?;
    writeln!(out, "Model: {provider}/{model}")?;
    Ok(())
}

fn run_projects<T: CloudTransport, W: Write>(
    command: CloudProjectsCommand,
    client: &CloudClient<'_, T>,
    machine: &machine::MachineKeyEnv,
    out: &mut W,
) -> Result<()> {
    if machine.is_present() {
        bail!(
            "Projects require an interactive Codewhale account login; unset CODEWHALE_API_KEY and run `codewhale login`"
        );
    }
    match command {
        CloudProjectsCommand::List { json } => {
            let response = client.projects()?;
            if json {
                return write_computer_json(out, &response);
            }
            let listing: ProjectListResponse = serde_json::from_value(response)
                .context("The Codewhale service returned an invalid Project list")?;
            writeln!(out, "Codewhale Projects ({})", listing.projects.len())?;
            for project in listing.projects {
                validate_resource_id(&project.id, "Project")?;
                writeln!(out, "{} — {}", printable(&project.name), project.id)?;
            }
            Ok(())
        }
        CloudProjectsCommand::Create {
            name,
            repo_binding_id,
            operation_key,
        } => {
            let listing: GitHubBindingListResponse =
                serde_json::from_value(client.github_bindings()?)
                    .context("The Codewhale service returned an invalid GitHub repository list")?;
            let binding = listing
                .bindings
                .iter()
                .find(|binding| binding.id == repo_binding_id)
                .ok_or_else(|| anyhow!(
                    "GitHub binding {} is not connected to this account. Run `codewhale account github bindings`",
                    printable(&repo_binding_id)
                ))?;
            if binding.provider != "github"
                || binding.installation_id.is_empty()
                || ["error", "revoked", "suspended", "disabled"]
                    .contains(&binding.status.to_ascii_lowercase().as_str())
            {
                bail!(
                    "That GitHub binding is unavailable for a Project. Reconnect the repository, then run `codewhale account github bindings`"
                );
            }
            let project = client.create_github_project(&name, binding, &operation_key)?;
            writeln!(out, "Project: {}", printable(&project.name))?;
            writeln!(out, "ID: {}", project.id)?;
            writeln!(
                out,
                "GitHub repository: {}",
                printable(&project.default_repo)
            )?;
            writeln!(
                out,
                "Assign a Whale: codewhale account agents bind-project AGENT {}",
                project.id
            )?;
            Ok(())
        }
    }
}

fn run_github<T: CloudTransport, W: Write>(
    command: CloudGithubCommand,
    client: &CloudClient<'_, T>,
    machine: &machine::MachineKeyEnv,
    out: &mut W,
) -> Result<()> {
    if machine.is_present() {
        bail!(
            "GitHub bindings require an interactive Codewhale account login; unset CODEWHALE_API_KEY and run `codewhale login`"
        );
    }
    match command {
        CloudGithubCommand::Bindings { json } => {
            let response = client.github_bindings()?;
            if json {
                return write_computer_json(out, &response);
            }
            let listing: GitHubBindingListResponse = serde_json::from_value(response)
                .context("The Codewhale service returned an invalid GitHub repository list")?;
            writeln!(out, "GitHub repositories ({})", listing.bindings.len())?;
            if listing.bindings.is_empty() {
                writeln!(
                    out,
                    "No GitHub repositories are connected to this account. Authorize one in Codewhale's GitHub setup, then retry."
                )?;
            }
            for binding in listing.bindings {
                if binding.provider != "github" || binding.id.is_empty() || binding.repo.is_empty()
                {
                    bail!("The Codewhale service returned an invalid GitHub repository binding");
                }
                writeln!(
                    out,
                    "{} — {} ({})",
                    printable(&binding.repo),
                    printable(&binding.id),
                    printable(&binding.status)
                )?;
            }
            Ok(())
        }
        CloudGithubCommand::Bind {
            repo,
            installation_id,
        } => work::bind_github_repo(client, out, &repo, installation_id.as_deref()),
    }
}

fn run_agents<T: CloudTransport, W: Write>(
    command: CloudAgentsCommand,
    client: &CloudClient<'_, T>,
    machine: &machine::MachineKeyEnv,
    out: &mut W,
) -> Result<()> {
    if machine.is_present() {
        bail!(
            "Agents require an interactive Codewhale account login; unset CODEWHALE_API_KEY and run `codewhale login`"
        );
    }
    match command {
        CloudAgentsCommand::List { json } => {
            let response = client.agents()?;
            if json {
                return write_computer_json(out, &response);
            }
            let listing: AgentListResponse = serde_json::from_value(response)
                .context("The Codewhale service returned an invalid Agent list")?;
            writeln!(out, "Codewhale Agents ({})", listing.agents.len())?;
            for agent in listing.agents {
                validate_resource_id(&agent.id, "Agent")?;
                writeln!(out, "{} — {}", printable(&agent.name), agent.id)?;
            }
            Ok(())
        }
        CloudAgentsCommand::Create {
            name,
            project_id,
            operation_key,
        } => {
            let agent = client.create_agent(&name, project_id.as_deref(), &operation_key)?;
            validate_resource_id(&agent.id, "Agent")?;
            if let Some(project_id) = project_id.as_deref()
                && agent.project_id != validate_resource_id(project_id, "Project")?
            {
                bail!("The Codewhale service returned an unexpected Agent Project binding");
            }
            writeln!(out, "Agent: {}", printable(&agent.name))?;
            writeln!(out, "ID: {}", agent.id)?;
            if !agent.project_id.is_empty() {
                writeln!(
                    out,
                    "Project ID: {}",
                    validate_resource_id(&agent.project_id, "Project")?
                )?;
            }
            writeln!(
                out,
                "Create request ID: {}",
                validate_operation_key(&operation_key)?
            )?;
            if agent.project_id.is_empty() {
                writeln!(
                    out,
                    "This Agent can chat now. Bind a Project for repository Work; run `codewhale account projects list` to find one."
                )?;
            }
            Ok(())
        }
        CloudAgentsCommand::BindProject { agent, project_id } => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let selected = resolve_account_agent(&listing.agents, &agent)?;
            let project_id = validate_resource_id(&project_id, "Project")?;
            let projects: ProjectListResponse = serde_json::from_value(client.projects()?)
                .context("The Codewhale service returned an invalid Project list")?;
            if !projects
                .projects
                .iter()
                .any(|project| project.id == project_id)
            {
                bail!(
                    "Project {project_id} is not available on this account. Run `codewhale account projects list`"
                );
            }
            if selected.project_id == project_id {
                writeln!(
                    out,
                    "Agent {} is already bound to Project {project_id}.",
                    printable(&selected.name)
                )?;
                return Ok(());
            }
            let bound = client.bind_agent_project(&selected.id, project_id, selected.revision)?;
            writeln!(
                out,
                "Agent {} is bound to Project {project_id}.",
                printable(&bound.name)
            )?;
            Ok(())
        }
        CloudAgentsCommand::Threads { agent, json } => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let selected = resolve_account_agent(&listing.agents, &agent)?;
            let threads = client
                .threads(&selected.id)?
                .into_iter()
                .filter(|thread| active_agent_thread(thread, &selected.id))
                .collect::<Vec<_>>();
            if json {
                return write_computer_json(out, &serde_json::to_value(&threads)?);
            }
            writeln!(
                out,
                "Recent conversations for {} ({})",
                printable(&selected.name),
                threads.len()
            )?;
            for thread in &threads {
                write_agent_thread(out, thread)?;
            }
            Ok(())
        }
        CloudAgentsCommand::NewThread {
            agent,
            title,
            provider,
            model,
            operation_key,
        } => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let selected = resolve_account_agent(&listing.agents, &agent)?;
            let project_id = selected.project_id.trim();
            // The saved route is a promise about which model answers, so a
            // route the live catalog does not list is refused before anything
            // is created rather than accepted and discovered at first send.
            let (route_provider, route_model) = validate_model_route(&provider, &model)?;
            let catalog = client.provider_catalog()?;
            let route_provider = assert_catalog_route(&catalog, route_provider, route_model)?;
            let thread = client.create_agent_thread(
                &selected.id,
                if project_id.is_empty() {
                    None
                } else {
                    Some(project_id)
                },
                &title,
                route_provider,
                route_model,
                &operation_key,
            )?;
            if !active_agent_thread(&thread, &selected.id) {
                bail!("The Codewhale service did not return an active conversation for this Agent");
            }
            if thread.model_provider != route_provider || thread.model != route_model {
                bail!(
                    "The Codewhale service created conversation {} on {}/{} instead of the requested {route_provider}/{route_model}. Do not send to it; create a new conversation or report this route mismatch",
                    printable(&thread.id),
                    printable(&thread.model_provider),
                    printable(&thread.model)
                );
            }
            if !project_id.is_empty() && thread.project_id != project_id {
                bail!(
                    "The Codewhale service created this conversation in a different Project; verify the Agent's Project before sending"
                );
            }
            write_agent_thread(out, &thread)?;
            writeln!(
                out,
                "Create request ID: {}",
                validate_operation_key(&operation_key)?
            )?;
            writeln!(
                out,
                "Sending requires an explicit --billing-mode and a new --operation-key for each message."
            )?;
            Ok(())
        }
        CloudAgentsCommand::Send {
            agent,
            prompt,
            thread,
            billing_mode,
            operation_key,
        } => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let selected = resolve_account_agent(&listing.agents, &agent)?;
            let selected_thread = if let Some(id) = thread {
                client.thread(&id)?
            } else {
                let threads = client.threads(&selected.id)?;
                if threads.len() >= 100 {
                    bail!(
                        "Agent {} has at least 100 recent conversations; pass --thread with an ID so an older Main conversation is not missed",
                        printable(&selected.name)
                    );
                }
                let mut active = threads
                    .into_iter()
                    .filter(|thread| active_agent_thread(thread, &selected.id))
                    .collect::<Vec<_>>();
                if active.len() == 1 {
                    active.remove(0)
                } else {
                    let mut main = active.into_iter().filter(|thread| thread.title == "Main");
                    let first = main.next().ok_or_else(|| anyhow!(
                        "Agent {} has no unique active conversation. Run `codewhale account agents new-thread` first, or pass --thread",
                        printable(&selected.name)
                    ))?;
                    if main.next().is_some() {
                        bail!(
                            "Agent {} has several Main conversations; pass --thread with an ID",
                            printable(&selected.name)
                        );
                    }
                    first
                }
            };
            if !active_agent_thread(&selected_thread, &selected.id) {
                bail!(
                    "That conversation is not active or does not belong to Agent {}",
                    printable(&selected.name)
                );
            }
            if !selected.project_id.is_empty() && selected_thread.project_id != selected.project_id
            {
                bail!(
                    "That conversation belongs to a different Project than Agent {} currently owns. Bind the Agent and create a new conversation in its Project before sending",
                    printable(&selected.name)
                );
            }
            let receipt =
                client.send_agent_turn(&selected_thread, &prompt, &billing_mode, &operation_key)?;
            validate_turn_id(&receipt.id)?;
            writeln!(out, "Conversation ID: {}", selected_thread.id)?;
            writeln!(out, "Turn ID: {}", receipt.id)?;
            writeln!(out, "Status: {}", printable(&receipt.status))?;
            writeln!(
                out,
                "Message request ID: {}",
                validate_operation_key(&operation_key)?
            )?;
            writeln!(
                out,
                "Read the answer: codewhale account agents result {} {} {}",
                selected.id, selected_thread.id, receipt.id
            )?;
            Ok(())
        }
        CloudAgentsCommand::Result {
            agent,
            thread,
            turn,
            since_seq,
        } => {
            let listing: AgentListResponse = serde_json::from_value(client.agents()?)
                .context("The Codewhale service returned an invalid Agent list")?;
            let selected = resolve_account_agent(&listing.agents, &agent)?;
            let selected_thread = client.thread(&thread)?;
            if !active_agent_thread(&selected_thread, &selected.id) {
                bail!(
                    "That conversation is not active or does not belong to Agent {}",
                    printable(&selected.name)
                );
            }
            let turn_id = validate_turn_id(&turn)?;
            let result = client.agent_turn_result(&selected_thread.id, turn_id, since_seq)?;
            writeln!(out, "Turn ID: {turn_id}")?;
            writeln!(out, "Status: {}", result.status)?;
            writeln!(out, "Last event sequence: {}", result.last_seq)?;
            if !result.seen_turn {
                writeln!(out, "No events for this turn after the selected sequence.")?;
            } else if !result.answer.is_empty() {
                let label = if result.status == "pending" {
                    "Answer so far"
                } else {
                    "Answer"
                };
                writeln!(out, "{label}:")?;
                writeln!(
                    out,
                    "{}",
                    result
                        .answer
                        .chars()
                        .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
                        .collect::<String>()
                )?;
            }
            if result.status == "pending" || result.status == "not_seen" {
                writeln!(out, "Run this result command again to read later events.")?;
            }
            Ok(())
        }
        CloudAgentsCommand::Work {
            agent,
            objective,
            message_id,
            context_file,
            file_refs,
        } => {
            let (objective, context) =
                work_objective_and_context(&objective, context_file.as_deref(), &file_refs)?;
            validate_operation_key(&message_id)?;
            let selected = prepare_work_agent(client, Some(&agent))?;
            work::assign(
                client,
                out,
                &selected,
                &objective,
                &message_id,
                context.as_ref(),
            )
            .map(|_| ())
        }
        CloudAgentsCommand::Dispatch {
            objective,
            agent,
            seconds,
            context_file,
            file_refs,
            operation_key,
            message_id,
            yes,
            confirm_eu_compute,
        } => {
            let (objective, context) =
                work_objective_and_context(&objective, context_file.as_deref(), &file_refs)?;
            let stamp = dispatch_stamp(&objective);
            let message_id = message_id.unwrap_or_else(|| format!("dispatch-m-{stamp}"));
            let operation_key = operation_key.unwrap_or_else(|| format!("dispatch-o-{stamp}"));
            validate_operation_key(&message_id)?;
            validate_operation_key(&operation_key)?;
            let selected = prepare_work_agent(client, agent.as_deref())?;
            let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
            work::dispatch(
                client,
                out,
                &selected,
                &objective,
                context.as_ref(),
                work::DispatchOptions {
                    seconds,
                    operation_key: &operation_key,
                    message_id: &message_id,
                    yes,
                    confirm_eu_compute,
                    interactive,
                },
                &mut || {
                    let mut line = String::new();
                    io::stdin()
                        .read_line(&mut line)
                        .context("failed to read the answer from stdin")?;
                    Ok(line)
                },
            )
        }
        CloudAgentsCommand::WorkStatus { id } => {
            let result = client.agent_work_status(&id)?;
            let run = result
                .get("run")
                .filter(|run| run.is_object())
                .ok_or_else(|| anyhow!("The Codewhale service returned no Work record"))?;
            if run.get("id").and_then(|value| value.as_str())
                != Some(validate_resource_id(&id, "Work")?)
            {
                bail!("The Codewhale service returned a different Work record");
            }
            writeln!(out, "Work ID: {}", validate_resource_id(&id, "Work")?)?;
            writeln!(
                out,
                "Status: {}",
                printable(
                    run.get("state")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                )
            )?;
            if let Some(title) = run.get("title").and_then(|value| value.as_str()) {
                writeln!(out, "Objective: {}", printable(title))?;
            }
            if let Some(workspace) = run
                .get("workspaceId")
                .and_then(|value| value.as_str())
                .filter(|id| !id.is_empty())
            {
                writeln!(out, "Workspace ID: {}", printable(workspace))?;
            }
            Ok(())
        }
        CloudAgentsCommand::WorkCancel { id, queue, reason } => {
            work::cancel(client, out, &id, queue, reason.as_deref())
        }
        CloudAgentsCommand::WorkResult { id, json } => work::result(client, out, &id, json),
        CloudAgentsCommand::WorkQuote {
            id,
            operation_key,
            seconds,
            recovery_from,
        } => work::quote(
            client,
            out,
            &id,
            &operation_key,
            seconds,
            recovery_from.as_deref(),
            true,
        )
        .map(|_| ()),
        CloudAgentsCommand::WorkLaunch {
            id,
            operation_key,
            confirmation,
            confirm_eu_compute,
            seconds,
            recovery_from,
        } => {
            let confirmation = if confirmation == "-" && confirm_eu_compute {
                if io::stdin().is_terminal() {
                    bail!(
                        "Pipe the launch confirmation to stdin; it must not be typed into the terminal"
                    );
                }
                work::read_confirmation(io::stdin().lock())?
            } else {
                confirmation
            };
            work::launch(
                client,
                out,
                &id,
                &operation_key,
                &confirmation,
                confirm_eu_compute,
                seconds,
                recovery_from.as_deref(),
            )
        }
    }
}

fn write_computer<W: Write>(out: &mut W, computer: &AccountComputer) -> Result<()> {
    validate_computer_id(&computer.id)?;
    if computer.owner_id.trim().is_empty() {
        bail!("The Codewhale service returned a Computer without an owner");
    }
    writeln!(out, "Computer: {}", printable(&computer.name))?;
    writeln!(out, "ID: {}", computer.id)?;
    writeln!(out, "Account ID: {}", printable(&computer.owner_id))?;
    writeln!(out, "Status: {}", printable(&computer.status))?;
    writeln!(out, "Region: {}", printable(&computer.region))?;
    Ok(())
}

fn write_computer_json<W: Write>(out: &mut W, value: &serde_json::Value) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)
        .context("failed to write Codewhale Computer JSON")?;
    writeln!(out)?;
    Ok(())
}

fn run_computers<T: CloudTransport, W: Write>(
    command: CloudComputersCommand,
    client: &CloudClient<'_, T>,
    machine: &machine::MachineKeyEnv,
    out: &mut W,
) -> Result<()> {
    // Machine keys are intentionally narrower than an interactive account
    // session. A present key must never silently fall back to a human login.
    if machine.is_present() {
        bail!(
            "Computers require an interactive Codewhale account login; unset CODEWHALE_API_KEY and run `codewhale login`"
        );
    }
    match command {
        CloudComputersCommand::List { json } => {
            let response = client.computers()?;
            if json {
                return write_computer_json(out, &response);
            }
            let computers: ComputerListResponse = serde_json::from_value(response)
                .context("The Codewhale service returned an invalid Computer list")?;
            let computers = computers.computers;
            writeln!(out, "Codewhale Computers ({})", computers.len())?;
            for computer in &computers {
                write_computer(out, computer)?;
            }
            Ok(())
        }
        CloudComputersCommand::Create {
            name,
            computer_trial,
            eu_compute_opt_in,
        } => {
            let computer = client.create_computer(&name, computer_trial, eu_compute_opt_in)?;
            write_computer(out, &computer)?;
            writeln!(
                out,
                "Saved Computer identity; compute is allocated when you start it."
            )?;
            Ok(())
        }
        CloudComputersCommand::Show { id, json } => {
            let response = client.computer(&id)?;
            if json {
                write_computer_json(out, &response)
            } else {
                let result: ComputerResponse = serde_json::from_value(response)
                    .context("The Codewhale service returned an invalid Computer")?;
                write_computer(out, &result.computer)
            }
        }
        CloudComputersCommand::Usage { id, json } => {
            if !json {
                bail!("Use --json to inspect Computer usage receipts");
            }
            write_computer_json(out, &client.computer_usage(&id)?)
        }
        CloudComputersCommand::Start { id } => {
            let result = client.computer_action(&id, "start")?;
            if result.queued {
                writeln!(out, "Computer start queued.")?;
                if !result.computer.start_queue_reason.is_empty() {
                    writeln!(
                        out,
                        "Reason: {}",
                        printable(&result.computer.start_queue_reason)
                    )?;
                }
            }
            write_computer(out, &result.computer)
        }
        CloudComputersCommand::Pause { id } => {
            let result = client.computer_action(&id, "pause")?;
            write_computer(out, &result.computer)
        }
        CloudComputersCommand::Delete { id, discard_files } => {
            if !discard_files {
                bail!(
                    "Deleting a computer discards its remaining files; save them first, then pass --discard-files"
                );
            }
            client.delete_computer(&id)?;
            writeln!(out, "Deleted Computer {}.", validate_computer_id(&id)?)?;
            Ok(())
        }
        CloudComputersCommand::Files { command } => run_computer_files(command, client, out),
    }
}

fn run_computer_files<T: CloudTransport, W: Write>(
    command: CloudComputerFilesCommand,
    client: &CloudClient<'_, T>,
    out: &mut W,
) -> Result<()> {
    match command {
        CloudComputerFilesCommand::Save {
            computer_id,
            path,
            request_id,
            file_id,
            expected_version,
            expected_revision,
            name,
        } => {
            let id = validate_computer_id(&computer_id)?;
            validate_computer_file_path(&path)?;
            let request_id = validate_turn_id(&request_id)?;
            if file_id.is_some() != expected_version.is_some()
                || expected_version.is_some_and(|version| !(1..MAX_FILE_VERSION).contains(&version))
            {
                bail!("Saving another version needs both --file-id and a valid --expected-version");
            }
            if let Some(file_id) = &file_id {
                validate_account_file_id(file_id)?;
            }
            if let Some(revision) = &expected_revision {
                validate_file_revision(revision)?;
            }
            let result = client.computer_file_action(id, "save", serde_json::json!({
                "path": path, "requestId": request_id, "fileId": file_id,
                "expectedVersion": expected_version.unwrap_or(0), "expectedRevision": expected_revision, "name": name,
            }))?;
            let saved_id = result["file"]["id"].as_str().unwrap_or_default();
            validate_account_file_id(saved_id)?;
            let sha256 = result["file"]["sha256"].as_str().unwrap_or_default();
            validate_file_revision(sha256)?;
            if result["saved"]["computerId"].as_str() != Some(id)
                || result["saved"]["path"].as_str() != Some(path.as_str())
                || result["saved"]["revision"].as_str() != Some(sha256)
                || result["file"]["version"].as_u64() != Some(expected_version.unwrap_or(0) + 1)
                || !result["file"]["size"]
                    .as_u64()
                    .is_some_and(|size| (1..=MAX_COMPUTER_FILE_SAVE_BYTES).contains(&size))
                || file_id
                    .as_deref()
                    .is_some_and(|expected| expected != saved_id)
                || expected_revision
                    .as_deref()
                    .is_some_and(|expected| expected != sha256)
            {
                bail!("The save receipt could not be verified; retry using the same --request-id");
            }
            write_computer_json(out, &result)
        }
        CloudComputerFilesCommand::Restore {
            computer_id,
            file_id,
            path,
            version,
            expected_revision,
        } => {
            let id = validate_computer_id(&computer_id)?;
            validate_computer_file_path(&path)?;
            if let Some(revision) = &expected_revision {
                validate_file_revision(revision)?;
            }
            let metadata = account_file_version(&client.account_file(&file_id)?, version)?;
            if metadata.size > MAX_COMPUTER_FILE_RESTORE_BYTES {
                bail!("Restore supports files up to 4 MiB; download this version instead");
            }
            match client.computer_file_revision(id, &path) {
                Ok(current) => match expected_revision.as_deref() {
                    Some(expected) if current["revision"].as_str() == Some(expected) => {}
                    Some(_) => {
                        bail!("The destination revision changed; inspect it before restoring")
                    }
                    None => bail!(
                        "The destination exists; inspect its revision and pass --expected-revision to replace it"
                    ),
                },
                Err(error)
                    if expected_revision.is_none()
                        && error.downcast_ref::<CloudHttpError>().is_some_and(|error| {
                            error.status == 404
                                && error.code.as_deref() == Some("computer_file_not_found")
                        }) => {}
                Err(error) => return Err(error),
            }
            let result = client.computer_file_action(id, "restore", serde_json::json!({
                "path": path, "fileId": file_id, "version": version, "expectedRevision": expected_revision,
            }))?;
            let restored = &result["restored"];
            if restored["computerId"].as_str() != Some(id)
                || restored["path"].as_str() != Some(path.as_str())
                || restored["fileId"].as_str() != Some(file_id.as_str())
                || restored["version"].as_u64() != Some(version)
                || restored["size"].as_u64() != Some(metadata.size)
                || restored["revision"].as_str() != Some(metadata.sha256.as_str())
                || restored["created"].as_bool().is_none()
            {
                bail!(
                    "The restore receipt could not be verified; inspect the destination revision before retrying"
                );
            }
            write_computer_json(out, &result)
        }
        CloudComputerFilesCommand::Revision { computer_id, path } => {
            write_computer_json(out, &client.computer_file_revision(&computer_id, &path)?)
        }
        CloudComputerFilesCommand::Versions { file_id } => {
            write_computer_json(out, &client.account_file(&file_id)?)
        }
        CloudComputerFilesCommand::Download {
            file_id,
            version,
            output,
        } => {
            client.download_account_file(&file_id, version, &output)?;
            writeln!(
                out,
                "Downloaded {} version {} to {} (size and SHA-256 verified).",
                file_id,
                version,
                printable(&output.display().to_string())
            )?;
            Ok(())
        }
    }
}

enum KeyReadMode {
    Stdin,
    HiddenPrompt(String),
}

pub(crate) fn run(args: CloudArgs, profile: Option<&str>, config: &mut ConfigStore) -> Result<()> {
    let machine = machine::MachineKeyEnv::from_process_env();
    let requested_base = machine::resolve_api_base(
        args.api_base.as_deref(),
        std::env::var(machine::MACHINE_API_BASE_ENV).ok().as_deref(),
        std::env::var(CLOUD_API_BASE_ENV).ok().as_deref(),
        DEFAULT_API_BASE,
    );
    if machine.is_present() {
        // A machine token is a bearer credential with no replay protection.
        // Refuse cleartext to a remote host before a transport exists, so
        // there is no code path on which the key could be written to a socket.
        machine::require_secure_base(&requested_base)?;
    }
    let api_base = validate_api_base(&requested_base)?;
    let transport = ReqwestTransport::new(api_base.url.clone())?;
    // Account refresh tokens require an OS credential manager. The ordinary
    // provider backend remains independently configurable for `--from-local`.
    let cloud_secrets = cloud_session_secrets()?;
    let provider_secrets = Secrets::auto_detect();
    let profile = normalized_profile(profile);
    let mut stdout = io::stdout().lock();
    let mut key_reader = |mode: KeyReadMode| match mode {
        KeyReadMode::Stdin => read_key_from_stdin(),
        KeyReadMode::HiddenPrompt(provider) => read_key_hidden(&provider),
    };
    let mut opener = |url: String| webbrowser::open(&url).is_ok();
    let mut sleeper = |duration| thread::sleep(duration);
    run_with(
        args.command,
        &profile,
        &api_base.display,
        config,
        &cloud_secrets,
        &provider_secrets,
        &machine,
        &transport,
        &mut stdout,
        &mut key_reader,
        &mut opener,
        &mut sleeper,
    )
}

fn cloud_session_secrets() -> Result<Secrets> {
    // Codex-style storage contract: the OS credential manager is preferred
    // but never required; without one, sessions live in the private 0600
    // Codewhale secrets file. Only an unresolvable store path fails here.
    secure_account_session_secrets().map_err(|err| anyhow!(err.to_string()))
}

/// `codewhale login` is a convenience entry to the account device flow — the
/// same path as `codewhale account login`, without re-spelling the subcommand.
pub(crate) fn run_account_login(
    no_open: bool,
    timeout_seconds: u64,
    profile: Option<&str>,
    config: &mut ConfigStore,
) -> Result<()> {
    run(
        CloudArgs {
            api_base: None,
            command: CloudCommand::Login(CloudLoginArgs {
                no_open,
                timeout_seconds,
            }),
        },
        profile,
        config,
    )
}

pub(crate) fn reject_inline_api_key(api_key: Option<&str>) -> Result<()> {
    if api_key.is_some() {
        bail!(
            "`codewhale account` does not accept the global `--api-key` flag because command-line values can leak through shell history. Use `account keys set <provider>` for a hidden prompt, `--api-key-stdin`, or `--from-local`"
        );
    }
    Ok(())
}

/// On account sign-in, point a never-configured local route at the managed
/// Codewhale provider so chat works immediately. A provider the user chose
/// explicitly, or one that already has a local key (config, secret store or
/// environment), is left alone: the default provider is DeepSeek, so equality
/// with the default cannot tell "never configured" from "chose DeepSeek".
fn select_managed_route_on_login<W: Write>(
    config: &mut ConfigStore,
    provider_secrets: &Secrets,
    out: &mut W,
) -> Result<()> {
    let explicit_provider = config
        .original_body()
        .and_then(|body| body.parse::<toml::Table>().ok())
        .is_some_and(|table| table.contains_key("provider"));
    let has_local_key =
        resolve_local_key(config, provider_secrets, config.config.provider)?.is_some();
    if config.config.provider == ProviderKind::default() && !explicit_provider && !has_local_key {
        config.config.provider = ProviderKind::Codewhale;
        config.config.model = Some("auto".to_string());
        config.save()?;
        writeln!(
            out,
            "Using your Codewhale account route (provider codewhale, model auto)."
        )?;
    } else {
        writeln!(
            out,
            "Keeping your configured {} route.",
            config.config.provider.as_str()
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_with<T: CloudTransport, W: Write>(
    command: CloudCommand,
    profile: &str,
    api_base: &str,
    config: &mut ConfigStore,
    cloud_secrets: &Secrets,
    provider_secrets: &Secrets,
    machine: &machine::MachineKeyEnv,
    transport: &T,
    out: &mut W,
    key_reader: &mut dyn FnMut(KeyReadMode) -> Result<String>,
    opener: &mut dyn FnMut(String) -> bool,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<()> {
    let client = CloudClient::new(transport, cloud_secrets, profile, api_base);
    match command {
        CloudCommand::Login(login) => {
            let device = client.start_device()?;
            validate_user_code(&device.user_code)?;
            validate_verification_url(
                &device.verification_uri,
                api_base,
                &device.user_code,
                false,
            )?;
            let verification_uri_complete = validate_verification_url(
                &device.verification_uri_complete,
                api_base,
                &device.user_code,
                true,
            )?;
            writeln!(out, "Codewhale account sign-in")?;
            writeln!(out, "Code: {}", device.user_code)?;
            writeln!(out, "Open: {verification_uri_complete}")?;
            writeln!(out, "Profile: {}", printable(profile))?;
            if !login.no_open && !opener(verification_uri_complete) {
                writeln!(
                    out,
                    "Browser could not be opened; use the URL and code above."
                )?;
            }
            let _ =
                client.poll_device(&device, Duration::from_secs(login.timeout_seconds), sleeper)?;
            let user = client.me()?;
            write_account(out, "Signed in to Codewhale.", profile, api_base, &user)?;
            select_managed_route_on_login(config, provider_secrets, out)?;
            Ok(())
        }
        CloudCommand::Status => match client.load_auth()? {
            Some(_) => {
                let user = client.me()?;
                write_account(out, "Signed in to Codewhale.", profile, api_base, &user)
            }
            None => {
                writeln!(out, "Not signed in to Codewhale.")?;
                writeln!(out, "Profile: {}", printable(profile))?;
                writeln!(out, "API: {api_base}")?;
                writeln!(out, "Run `codewhale login` to sign in.")?;
                Ok(())
            }
        },
        CloudCommand::Logout => {
            let remote_revoked = client.logout()?;
            writeln!(out, "Removed the local Codewhale account session.")?;
            writeln!(out, "Profile: {}", printable(profile))?;
            if !remote_revoked {
                writeln!(
                    out,
                    "Remote revocation was not confirmed; the local tokens are gone."
                )?;
            }
            Ok(())
        }
        CloudCommand::Keys(keys) => match keys.command {
            CloudKeysCommand::List => {
                let user = client.me()?;
                let catalog = client.provider_catalog()?;
                write_account(out, "Codewhale account keys.", profile, api_base, &user)?;
                for row in &catalog {
                    let stored = user.model_keys.get(&row.id);
                    let status = match stored {
                        Some(state) if state.configured => match state
                            .state
                            .as_deref()
                            .map(printable)
                            .filter(|value| !value.is_empty())
                        {
                            Some(reported) => format!("set ({reported})"),
                            None => "set".to_string(),
                        },
                        _ => "not set".to_string(),
                    };
                    writeln!(out, "{}: {status} — {}", row.id, row.display_label())?;
                }
                Ok(())
            }
            CloudKeysCommand::Set(set) => {
                let provider = validate_provider_id(&set.provider)?;
                let user = client.me()?;
                let catalog = client.provider_catalog()?;
                let row = catalog_row(&catalog, &provider)?;
                let key = if set.from_local {
                    let kind = row.local_kind().ok_or_else(|| {
                        anyhow!(
                            "`{provider}` has no local runtime provider, so there is no local key to copy. Use `--api-key-stdin` or the hidden prompt"
                        )
                    })?;
                    resolve_local_key(config, provider_secrets, kind)?.ok_or_else(|| {
                        anyhow!(
                            "No local {} API key was found in config, the secret store, or the environment",
                            kind.as_str()
                        )
                    })?
                } else if set.api_key_stdin {
                    key_reader(KeyReadMode::Stdin)?
                } else {
                    key_reader(KeyReadMode::HiddenPrompt(provider.clone()))?
                };
                let key = key.trim().to_string();
                validate_api_key(&key)?;
                let label = validate_label(&set.label)?;
                client.set_key(&provider, &key, &label)?;
                writeln!(
                    out,
                    "Saved {provider} for Codewhale account {} (profile {}).",
                    printable(&user.id),
                    printable(profile)
                )?;
                Ok(())
            }
            CloudKeysCommand::Remove { provider } => {
                let provider = validate_provider_id(&provider)?;
                let user = client.me()?;
                let catalog = client.provider_catalog()?;
                let _ = catalog_row(&catalog, &provider)?;
                client.remove_key(&provider)?;
                writeln!(
                    out,
                    "Removed {provider} from Codewhale account {} (profile {}).",
                    printable(&user.id),
                    printable(profile)
                )?;
                Ok(())
            }
        },
        CloudCommand::Computers(computers) => {
            run_computers(computers.command, &client, machine, out)
        }
        CloudCommand::Projects(projects) => run_projects(projects.command, &client, machine, out),
        CloudCommand::Github(github) => run_github(github.command, &client, machine, out),
        CloudCommand::Agents(agents) => run_agents(agents.command, &client, machine, out),
        CloudCommand::ApiKeys(api_keys) => {
            machine::run_api_keys(api_keys, &client, machine, provider_secrets, out, sleeper)
        }
        CloudCommand::Whoami => match machine.resolve()? {
            // A present machine key wins and never falls back: silently
            // downgrading a machine credential to a human one is how CI ends
            // up running as the wrong identity.
            Some(key) => {
                let machine_client = machine::MachineClient::new(transport, key);
                let who = machine_client.whoami(sleeper)?;
                machine::write_whoami(out, &who, api_base, machine_client.key_head())
            }
            None => {
                let user = client.me()?;
                write_account(out, "Signed in to Codewhale.", profile, api_base, &user)
            }
        },
        CloudCommand::Agent => {
            // The agent route is machine-key-only by design, so CI and humans
            // never blur in an audit trail. There is no session fallback.
            let key = machine.require()?;
            let machine_client = machine::MachineClient::new(transport, key);
            let agent = machine_client.agent(sleeper)?.agent;
            machine::write_agent(out, &agent)
        }
        CloudCommand::Pull(args) => {
            if !args.dry_run {
                bail!(
                    "Account settings import is not available yet; local config was not changed. Run `codewhale account pull --dry-run` to inspect the signed-in account."
                );
            }
            let user = client.me()?;
            // `/api/me` currently exposes account identity and key metadata,
            // not a versioned settings document that can be applied locally.
            // Stay read-only and explicit until that import contract exists.
            writeln!(out, "Account settings (pull --dry-run):")?;
            writeln!(out, "Account ID: {}", printable(&user.id))?;
            writeln!(out, "Profile: {}", printable(profile))?;
            writeln!(out, "API: {api_base}")?;
            writeln!(
                out,
                "dry-run: remote settings import is not available; local config unchanged"
            )?;
            // Show the invariant: account session tokens stay in the private
            // Codewhale secrets file, never in config.toml.
            writeln!(
                out,
                "Secure custody: account session tokens stay in the private Codewhale secrets file, never in config.toml"
            )?;
            Ok(())
        }
        CloudCommand::Push(args) => {
            let user = client.me()?;
            if !args.dry_run {
                bail!(
                    "Push is never automatic; re-run with --dry-run to preview, then confirm explicitly"
                );
            }
            writeln!(out, "Account settings (push --dry-run):")?;
            writeln!(out, "Account ID: {}", printable(&user.id))?;
            writeln!(out, "Profile: {}", printable(profile))?;
            writeln!(out, "API: {api_base}")?;
            writeln!(
                out,
                "dry-run: would PATCH /api/me/preferences with If-Match revision check (412 on conflict)"
            )?;
            writeln!(
                out,
                "No credentials, paths, or env are copied; only explicit fields (field-level last-writer-wins)"
            )?;
            Ok(())
        }
    }
}

/// Resolve the account's configured agent route for a machine-key run.
///
/// `codewhale review` hard-errors when a model resolves to several configured
/// routes. When CI authenticates with a machine key, the account has already
/// answered that question, so its configured provider is the disambiguator —
/// no new flag, and no guess. Returns `None` when no machine key is set, which
/// leaves the ordinary local resolution untouched.
pub(crate) fn machine_review_provider() -> Result<Option<ProviderKind>> {
    let machine = machine::MachineKeyEnv::from_process_env();
    let Some(key) = machine.resolve()? else {
        return Ok(None);
    };
    let requested_base = machine::resolve_api_base(
        None,
        std::env::var(machine::MACHINE_API_BASE_ENV).ok().as_deref(),
        std::env::var(CLOUD_API_BASE_ENV).ok().as_deref(),
        DEFAULT_API_BASE,
    );
    machine::require_secure_base(&requested_base)?;
    let api_base = validate_api_base(&requested_base)?;
    let transport = ReqwestTransport::new(api_base.url)?;
    let client = machine::MachineClient::new(&transport, key);
    // The call that actually needs a model is the call that refuses without
    // one, so this precondition runs before any review work starts.
    let agent = client.agent(&mut |duration| thread::sleep(duration))?.agent;
    machine::review_provider_from_agent(&agent).map(Some)
}

fn write_account<W: Write>(
    out: &mut W,
    heading: &str,
    profile: &str,
    api_base: &str,
    user: &CloudUser,
) -> Result<()> {
    writeln!(out, "{heading}")?;
    writeln!(out, "Account ID: {}", printable(&user.id))?;
    if !user.display_name.trim().is_empty() {
        writeln!(out, "Name: {}", printable(&user.display_name))?;
    }
    if !user.email.trim().is_empty() {
        writeln!(out, "Email: {}", printable(&user.email))?;
    }
    if !user.plan.trim().is_empty() {
        writeln!(out, "Plan: {}", printable(&user.plan))?;
    }
    writeln!(out, "Profile: {}", printable(profile))?;
    writeln!(out, "API: {api_base}")?;
    Ok(())
}

struct ValidatedApiBase {
    url: Url,
    display: String,
}

fn validate_api_base(value: &str) -> Result<ValidatedApiBase> {
    let mut url = Url::parse(value.trim()).context("invalid Codewhale account API base URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("Codewhale account API base URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("Codewhale account API base URL must not contain a query or fragment");
    }
    if !matches!(url.path(), "" | "/") {
        bail!("Codewhale account API base URL must be an origin without a path");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Codewhale account API base URL must include a host"))?;
    let allowed = url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(host));
    if !allowed {
        bail!(
            "Codewhale account API base URL must use HTTPS (loopback HTTP is allowed for testing)"
        );
    }
    url.set_path("/");
    let display = url.as_str().trim_end_matches('/').to_string();
    Ok(ValidatedApiBase { url, display })
}

fn validate_verification_url(
    value: &str,
    api_base: &str,
    user_code: &str,
    complete: bool,
) -> Result<String> {
    let url =
        Url::parse(value).context("The Codewhale service returned an invalid verification URL")?;
    if value != url.as_str() {
        bail!("The Codewhale service returned an unsafe verification URL");
    }
    let host = url.host_str().ok_or_else(|| {
        anyhow!("The Codewhale service returned a verification URL without a host")
    })?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("The Codewhale service returned an unsafe verification URL");
    }
    if url.path() != "/cli/authorize" {
        bail!("The Codewhale service returned an unsafe verification URL");
    }

    let api = Url::parse(api_base).context("invalid Codewhale account API base URL")?;
    let canonical_api = api.scheme() == "https"
        && api.host_str() == Some("api.codewhale.net")
        && api.port_or_known_default() == Some(443);
    let loopback_api = api.host_str().is_some_and(is_loopback_host);
    if canonical_api {
        if url.scheme() != "https"
            || !host.eq_ignore_ascii_case("app.codewhale.net")
            || url.port_or_known_default() != Some(443)
        {
            bail!("The Codewhale service returned an untrusted verification origin");
        }
    } else if loopback_api {
        if !matches!(url.scheme(), "http" | "https") || !is_loopback_host(host) {
            bail!("The Codewhale service returned an untrusted verification origin");
        }
    } else {
        bail!(
            "Browser login is only enabled for the canonical Codewhale account API or a loopback test API"
        );
    }

    let query = url.query_pairs().collect::<Vec<_>>();
    if complete {
        if query.len() != 1 || query[0].0 != "user_code" || query[0].1 != user_code {
            bail!("The Codewhale service returned an unsafe verification URL");
        }
    } else if !query.is_empty() {
        bail!("The Codewhale service returned an unsafe verification URL");
    }
    Ok(url.to_string())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn validate_user_code(code: &str) -> Result<()> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = code.as_bytes();
    if bytes.len() != 14
        || bytes[4] != b'-'
        || bytes[9] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 9) && !ALPHABET.contains(byte))
    {
        bail!("The Codewhale service returned an invalid user code");
    }
    Ok(())
}

fn validate_device_code(code: &str) -> Result<()> {
    if code.len() != 43
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("The Codewhale service returned an invalid device authorization response");
    }
    Ok(())
}

fn validate_api_key(key: &str) -> Result<()> {
    let bytes = key.len();
    if bytes < MIN_API_KEY_BYTES || bytes as u64 > MAX_API_KEY_BYTES {
        bail!("API key must be {MIN_API_KEY_BYTES}-{MAX_API_KEY_BYTES} UTF-8 bytes");
    }
    if key.chars().any(is_ascii_control) {
        bail!("API key contains invalid control characters");
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<String> {
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty()
        || label.chars().count() > MAX_KEY_LABEL_CHARS
        || label.chars().any(is_ascii_control)
    {
        bail!("key label must contain 1-{MAX_KEY_LABEL_CHARS} characters");
    }
    Ok(label)
}

fn is_ascii_control(character: char) -> bool {
    character <= '\u{001f}' || character == '\u{007f}'
}

/// Find one catalog row by id, or fail naming what the account does offer.
///
/// A catalog miss is the common typo, so the message lists the ids rather than
/// leaving the user to guess or read a 404.
fn catalog_row<'a>(catalog: &'a [CatalogProvider], provider: &str) -> Result<&'a CatalogProvider> {
    catalog
        .iter()
        .find(|row| row.id == provider)
        .ok_or_else(|| {
            let known = catalog
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!("`{provider}` is not a provider this Codewhale account can connect. Known providers: {known}")
        })
}

fn resolve_local_key(
    config: &ConfigStore,
    secrets: &Secrets,
    kind: ProviderKind,
) -> Result<Option<String>> {
    let provider_config = config.config.providers.for_provider(kind);
    let from_config = provider_config.api_key.clone();
    if let Some(value) = from_config
        .and_then(resolve_config_key_reference)
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Some(value));
    }
    if let Some(value) = secrets
        .get(kind.as_str())
        .context("failed to read the local provider secret store")?
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(Some(value));
    }
    Ok(kind.provider().env_vars().iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }))
}

fn resolve_config_key_reference(value: String) -> Option<String> {
    let trimmed = value.trim();
    let Some(variable) = trimmed.strip_prefix('$') else {
        return Some(value);
    };
    if variable.is_empty()
        || !variable
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn read_key_from_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_API_KEY_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read API key from stdin")?;
    parse_key_input(bytes)
}

fn parse_key_input(bytes: Vec<u8>) -> Result<String> {
    if bytes.len() as u64 > MAX_API_KEY_STDIN_BYTES {
        bail!("API key input is unexpectedly large");
    }
    let value = String::from_utf8(bytes).context("API key from stdin is not valid UTF-8")?;
    let value = value.trim().to_string();
    validate_api_key(&value)?;
    Ok(value)
}

fn read_key_hidden(provider: &str) -> Result<String> {
    if !io::stdin().is_terminal() {
        bail!("interactive key entry requires a terminal; use `--api-key-stdin` for piped input");
    }
    let term = console::Term::stderr();
    term.write_str(&format!("Enter {provider} API key: "))
        .context("failed to write API key prompt")?;
    let value = term
        .read_secure_line()
        .context("failed to read API key securely")?;
    term.write_line("").ok();
    let value = value.trim().to_string();
    validate_api_key(&value)?;
    Ok(value)
}

fn json_body(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("failed to encode Codewhale account request")
}

fn expect_json<T: DeserializeOwned>(response: CloudResponse, statuses: &[u16]) -> Result<T> {
    if !statuses.contains(&response.status) {
        return Err(response_error(&response));
    }
    parse_json_body(&response.body)
}

fn expect_empty(response: CloudResponse, statuses: &[u16]) -> Result<()> {
    if statuses.contains(&response.status) {
        Ok(())
    } else {
        Err(response_error(&response))
    }
}

fn parse_json_body<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).context("The Codewhale service returned an invalid JSON response")
}

/// A non-success reply from the account API, kept typed so a caller can tell a
/// definitive refusal (4xx) from an outcome it cannot know (5xx, timeout).
#[derive(Debug)]
pub(crate) struct CloudHttpError {
    status: u16,
    code: Option<String>,
    reconciliation_required: bool,
}

impl CloudHttpError {
    fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
}

impl std::fmt::Display for CloudHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code.as_deref().and_then(neutral_error_text) {
            Some(text) => write!(
                f,
                "Codewhale account request failed (HTTP {}): {text}",
                self.status
            ),
            None => match self.code.as_deref().filter(|code| !is_vendor_code(code)) {
                Some(code) => write!(
                    f,
                    "Codewhale account request failed (HTTP {}, code {code})",
                    self.status
                ),
                None => write!(f, "Codewhale account request failed (HTTP {})", self.status),
            },
        }
    }
}

pub(crate) fn vendor_safe_prose(text: String) -> String {
    if is_vendor_code(&text) {
        "The cloud service reported an unrecognised condition".to_string()
    } else {
        text
    }
}

fn is_vendor_code(code: &str) -> bool {
    code.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "boat" | "daytona" | "sprite" | "sprites" | "agentbay" | "alibaba"
            )
        })
}

fn neutral_error_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "boat_task_replay_expired" | "work_task_replay_expired" => {
            "The earlier request can no longer be replayed and needs operator reconciliation"
        }
        "boat_task_create_in_progress" | "work_task_create_in_progress" => {
            "The cloud computer is still being created"
        }
        "boat_task_cleanup_pending" | "work_task_cleanup_pending" => {
            "The cloud computer is still being cleaned up"
        }
        "boat_task_receipt_invalid" | "work_task_receipt_invalid" => {
            "The cloud computer's receipt could not be verified"
        }
        "boat_task_authority_changed" | "work_task_authority_changed" => {
            "Your account authority changed while the cloud computer was starting"
        }
        "boat_task_stop_unconfirmed" | "work_task_stop_unconfirmed" => {
            "The cloud computer stopped but its state is unconfirmed"
        }
        "boat_task_usage_pending" | "work_task_usage_pending" => {
            "Usage for the cloud computer is still being finalized"
        }
        "boat_task_outcome_unknown" | "work_task_outcome_unknown" => {
            "The cloud computer outcome is unknown"
        }
        _ => return None,
    })
}

pub(crate) fn display_error_code(code: &str) -> String {
    match neutral_error_text(code) {
        Some(text) => text.to_string(),
        None if is_vendor_code(code) => "unrecognised cloud error".to_string(),
        None => code.to_string(),
    }
}

impl std::error::Error for CloudHttpError {}

/// The request may or may not have been processed: it never reached the
/// service, or its reply was lost or unreadable. Mutating commands use this to
/// say "unknown" instead of guessing "failed".
#[derive(Debug)]
pub(crate) struct CloudTransportError {
    message: &'static str,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl CloudTransportError {
    fn new(message: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message,
            source: Box::new(source),
        }
    }
}

impl std::fmt::Display for CloudTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for CloudTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Whether a failed mutating request may still have taken effect.
fn outcome_unknown(err: &anyhow::Error) -> bool {
    err.downcast_ref::<CloudTransportError>().is_some()
        || err.downcast_ref::<CloudHttpError>().is_some_and(|http| {
            http.status >= 500
                || http.status == 408
                || http.reconciliation_required
                || http.code().is_some_and(|code| {
                    code.ends_with("_outcome_unknown")
                        || matches!(
                            code,
                            "boat_task_replay_expired"
                                | "boat_task_create_in_progress"
                                | "boat_task_cleanup_pending"
                                | "boat_task_receipt_invalid"
                                | "boat_task_authority_changed"
                                | "boat_task_stop_unconfirmed"
                                | "boat_task_usage_pending"
                        )
                })
        })
}

fn response_error(response: &CloudResponse) -> anyhow::Error {
    let body = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
    let code = body.as_ref().and_then(|body| {
        body.get("code")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                body.get("error")
                    .and_then(|error| error.get("code"))
                    .and_then(serde_json::Value::as_str)
            })
            .and_then(safe_error_code)
    });
    let reconciliation_required = body.as_ref().is_some_and(|body| {
        body.get("reconciliationRequired")
            .or_else(|| {
                body.get("error")
                    .and_then(|error| error.get("reconciliationRequired"))
            })
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    });
    anyhow::Error::new(CloudHttpError {
        status: response.status,
        code,
        reconciliation_required,
    })
}

fn safe_error_code(code: &str) -> Option<String> {
    if code.is_empty()
        || code.len() > 80
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(code.to_string())
}

fn printable(value: &str) -> String {
    printable_max(value, 200)
}

/// `printable` with a caller-chosen bound, for remote prose longer than a label.
fn printable_max(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests;
