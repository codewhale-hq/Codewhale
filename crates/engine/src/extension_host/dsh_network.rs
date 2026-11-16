use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codewhale_agent_contract::role::NETWORK_DENIAL_SENTINEL;
use codewhale_execpolicy::ApprovalMode;
use codewhale_execpolicy::tool_rules::tool_denied;
use codewhale_net::guard::{
    PrivateTargets, guarded_reqwest_client_builder, validate_fetch_destination,
};
use codewhale_secrets::{FileKeyringStore, Secrets};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::dsh_broker::{
    BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, decode,
};
use super::protocol::DshService;
use crate::tools::spec::{
    ApprovalRequirement, PreparedToolCall, ResourceClaim, ToolCapability, ToolContext, ToolError,
    ToolResult, ToolSpec,
};
use crate::tools::web::fetch::network_authorized;
use crate::tools::web::guard::validate_network_policy;

pub const CREDENTIAL_HANDLE_PREFIX: &str = "codewhale-credential:";
pub const SEND_TOOL: &str = "plugin_network_send";
const MAX_REF_LEN: usize = 128;
const MIN_SECRET_BYTES: usize = 8;
const MAX_SECRET_BYTES: usize = 4096;
const MAX_REFS_PER_PLUGIN: usize = 16;
const MAX_REFS_TOTAL: usize = 256;
const MAX_HANDLES_PER_REQUEST: usize = 8;
const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FETCHES_AT_ONCE: usize = 4;
const MAX_REMEMBERED_ANSWERS: usize = 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
const AUDIT_LABEL: &str = "dsh_network";
const AUDIT_EVENT: &str = "plugin_network_send";
const VAULT_ENTRY: &str = "plugin-credentials";
const VAULT_FILE: &str = "plugin-credentials.json";
const LOG: &str = "codewhale::dsh_network";
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
];
const FORBIDDEN_WITH_A_CREDENTIAL: &[&str] = &["accept-encoding", "range", "if-range"];
const DROPPED_ON_ANOTHER_SITE: &[&str] = &["authorization", "cookie", "cookie2"];
const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

const NOT_ALLOWED_HERE: &str = "network access to this site is not allowed here";
const NOT_REVIEWED: &str = "this site is not one of the plugin's reviewed network sites";
const NOT_FOR_THIS_SITE: &str = "this credential is not stored for this site";
const DECLINED: &str = "the person declined to let this plugin send to this site in this session";
const VAULT_UNREADABLE: &str = "the plugin credential store could not be read";
const VAULT_FULL: &str = "the plugin credential store is full";

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum NetworkMethod {
    #[serde(rename = "fetch")]
    Fetch(FetchInput),
    #[serde(rename = "credentials.describe")]
    Describe(CredentialRef),
    #[serde(rename = "credentials.resolve")]
    Resolve(CredentialRef),
    #[serde(rename = "credentials.set")]
    Set(CredentialWrite),
}

impl BrokerMethods for NetworkMethod {
    const NAMES: &'static [&'static str] = &[
        "fetch",
        "credentials.describe",
        "credentials.resolve",
        "credentials.set",
    ];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchInput {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body_base64: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRef {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialWrite {
    #[serde(rename = "ref")]
    name: String,
    value: String,
    site: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Origin {
    https: bool,
    host: String,
    port: u16,
}

impl Origin {
    fn of(url: &reqwest::Url) -> Option<Self> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        if !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        if host.is_empty() || host.ends_with('.') {
            return None;
        }
        Some(Self {
            https,
            host,
            port: url.port_or_known_default()?,
        })
    }

    pub(super) fn parse(site: &str) -> Option<Self> {
        let url = reqwest::Url::parse(site).ok()?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return None;
        }
        Self::of(&url)
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (scheme, default_port) = if self.https {
            ("https", 443)
        } else {
            ("http", 80)
        };
        write!(f, "{scheme}://{}", self.host)?;
        if self.port != default_port {
            write!(f, ":{}", self.port)?;
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    site: String,
    value: String,
}

#[derive(Default, Serialize, Deserialize)]
struct PluginSlots {
    build: String,
    credentials: BTreeMap<String, Stored>,
}

#[derive(Default, Serialize, Deserialize)]
struct VaultFile {
    plugins: BTreeMap<String, PluginSlots>,
}

enum VaultError {
    Unreadable,
    Full,
}

impl From<VaultError> for BrokerRefusal {
    fn from(error: VaultError) -> Self {
        BrokerRefusal::Refused {
            service: DshService::Network,
            message: match error {
                VaultError::Unreadable => VAULT_UNREADABLE,
                VaultError::Full => VAULT_FULL,
            }
            .to_string(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Vault {
    store: Secrets,
    #[cfg(test)]
    plaintext_for_test: bool,
}

pub(super) fn vault_path(codewhale_home: &Path) -> PathBuf {
    codewhale_home.join("secrets").join(VAULT_FILE)
}

impl Vault {
    pub(super) fn over(store: Secrets) -> Self {
        Self {
            store,
            #[cfg(test)]
            plaintext_for_test: false,
        }
    }

    pub(super) fn at(path: PathBuf) -> Self {
        Self::over(Secrets::new(Arc::new(FileKeyringStore::new(path))))
    }

    fn open() -> Option<Self> {
        let home = codewhale_config::codewhale_home().ok()?;
        Some(Self::at(vault_path(&home)))
    }

    fn carries(&self, site: &Origin) -> bool {
        #[cfg(test)]
        if self.plaintext_for_test {
            return true;
        }
        site.https
    }

    fn load(&self) -> Result<VaultFile, VaultError> {
        match self.store.get(VAULT_ENTRY) {
            Ok(None) => Ok(VaultFile::default()),
            Ok(Some(raw)) => serde_json::from_str(&raw).map_err(|_| VaultError::Unreadable),
            Err(_) => Err(VaultError::Unreadable),
        }
    }

    fn read_blocking(
        &self,
        plugin: &str,
        build: &str,
    ) -> Result<BTreeMap<String, Stored>, VaultError> {
        Ok(self
            .load()?
            .plugins
            .remove(plugin)
            .filter(|slots| slots.build == build)
            .map(|slots| slots.credentials)
            .unwrap_or_default())
    }

    fn write_blocking(
        &self,
        plugin: &str,
        build: &str,
        name: &str,
        stored: Stored,
    ) -> Result<(), VaultError> {
        self.store
            .with_entry_transaction(VAULT_ENTRY, |entry| {
                let mut file: VaultFile = match entry.as_deref() {
                    None => VaultFile::default(),
                    Some(raw) => match serde_json::from_str(raw) {
                        Ok(file) => file,
                        Err(_) => return Ok(Err(VaultError::Unreadable)),
                    },
                };
                let slots = file.plugins.entry(plugin.to_string()).or_default();
                if slots.build != build {
                    *slots = PluginSlots {
                        build: build.to_string(),
                        credentials: BTreeMap::new(),
                    };
                }
                let new = !slots.credentials.contains_key(name);
                if new && slots.credentials.len() >= MAX_REFS_PER_PLUGIN {
                    return Ok(Err(VaultError::Full));
                }
                let total: usize = file
                    .plugins
                    .values()
                    .map(|slots| slots.credentials.len())
                    .sum();
                if new && total >= MAX_REFS_TOTAL {
                    return Ok(Err(VaultError::Full));
                }
                file.plugins
                    .entry(plugin.to_string())
                    .or_default()
                    .credentials
                    .insert(name.to_string(), stored);
                *entry = Some(serde_json::to_string(&file)?);
                Ok(Ok(()))
            })
            .unwrap_or(Err(VaultError::Unreadable))
    }

    fn forget_blocking(&self, plugin: &str) -> Result<(), VaultError> {
        self.store
            .with_entry_transaction(VAULT_ENTRY, |entry| {
                let Some(raw) = entry.as_deref() else {
                    return Ok(Ok(()));
                };
                let mut file: VaultFile = match serde_json::from_str(raw) {
                    Ok(file) => file,
                    Err(_) => return Ok(Err(VaultError::Unreadable)),
                };
                if file.plugins.remove(plugin).is_some() {
                    *entry = Some(serde_json::to_string(&file)?);
                }
                Ok(Ok(()))
            })
            .unwrap_or(Err(VaultError::Unreadable))
    }

    async fn read(
        &self,
        plugin: &str,
        build: &str,
    ) -> Result<BTreeMap<String, Stored>, BrokerRefusal> {
        let (vault, plugin, build) = (self.clone(), plugin.to_string(), build.to_string());
        tokio::task::spawn_blocking(move || vault.read_blocking(&plugin, &build))
            .await
            .unwrap_or(Err(VaultError::Unreadable))
            .map_err(BrokerRefusal::from)
    }

    async fn write(
        &self,
        plugin: &str,
        build: &str,
        name: &str,
        stored: Stored,
    ) -> Result<(), BrokerRefusal> {
        let (vault, plugin, build, name) = (
            self.clone(),
            plugin.to_string(),
            build.to_string(),
            name.to_string(),
        );
        tokio::task::spawn_blocking(move || vault.write_blocking(&plugin, &build, &name, stored))
            .await
            .unwrap_or(Err(VaultError::Unreadable))
            .map_err(BrokerRefusal::from)
    }
}

pub fn forget_plugin_credentials(plugin_id: &str) -> bool {
    Vault::open().is_some_and(|vault| vault.forget_blocking(plugin_id).is_ok())
}

#[derive(Clone)]
pub(super) struct NetworkAuthority {
    pub build: String,
    pub sites: BTreeSet<Origin>,
    pub vault: Vault,
    #[cfg(test)]
    pub test_dns_pins: BTreeMap<String, std::net::IpAddr>,
}

fn reviewed_sites(
    _plugin_id: &str,
    _plugins: &codewhale_plugins::PluginRegistry,
) -> BTreeSet<Origin> {
    BTreeSet::new()
}

fn authority_of(call: &BrokerCall) -> Option<NetworkAuthority> {
    let plugins = call.context.tool().plugin_registry.as_ref()?;
    let plugin = plugins.get(&call.owner.plugin_id)?;
    if !plugin.trusted() || !plugin.enabled {
        return None;
    }
    Some(NetworkAuthority {
        build: plugin.content_hash.clone(),
        sites: reviewed_sites(&call.owner.plugin_id, plugins),
        vault: Vault::open()?,
        #[cfg(test)]
        test_dns_pins: BTreeMap::new(),
    })
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    match authority_of(&call) {
        Some(authority) => serve_with(call, authority),
        None => Box::pin(async move {
            decode::<NetworkMethod>(&call)?;
            Err(BrokerRefusal::NotAvailable(DshService::Network))
        }),
    }
}

pub(super) fn serve_with(call: BrokerCall, authority: NetworkAuthority) -> BrokerFuture {
    Box::pin(async move {
        let method = decode::<NetworkMethod>(&call)?;
        let plugin = call.owner.plugin_id.clone();
        if call.context.cancel().is_cancelled() {
            return Err(refused(&plugin, "cancelled", NOT_ALLOWED_HERE));
        }
        match method {
            NetworkMethod::Fetch(input) => fetch(&call, &authority, input).await,
            NetworkMethod::Describe(CredentialRef { name }) => {
                let name = checked_ref(&name)?;
                call.context
                    .act("credentials.describe", BrokerEffect::Read, Some(name))
                    .await?;
                let stored = authority
                    .vault
                    .read(&plugin, &authority.build)
                    .await?
                    .remove(name);
                tracing::info!(target: LOG, plugin = %plugin, credential = %name, set = stored.is_some(), "credential described");
                Ok(json!({
                    "ref": name,
                    "set": stored.is_some(),
                    "site": stored.map(|stored| stored.site),
                    "handle": handle(name),
                }))
            }
            NetworkMethod::Resolve(CredentialRef { name }) => {
                let name = checked_ref(&name)?;
                call.context
                    .act("credentials.resolve", BrokerEffect::Read, Some(name))
                    .await?;
                let set = authority
                    .vault
                    .read(&plugin, &authority.build)
                    .await?
                    .contains_key(name);
                tracing::info!(target: LOG, plugin = %plugin, credential = %name, set, "credential resolved to its handle");
                Ok(json!({ "ref": name, "value": set.then(|| handle(name)) }))
            }
            NetworkMethod::Set(CredentialWrite { name, value, site }) => {
                let name = checked_ref(&name)?;
                let site = Origin::parse(&site)
                    .filter(|site| authority.vault.carries(site))
                    .ok_or_else(|| {
                        invalid(
                            "a credential site is an https origin such as https://api.example.com",
                        )
                    })?;
                if !authority.sites.contains(&site) {
                    return Err(refused(&plugin, "unreviewed_site", NOT_REVIEWED));
                }
                if !(MIN_SECRET_BYTES..=MAX_SECRET_BYTES).contains(&value.len()) {
                    return Err(invalid(format!(
                        "a credential value is {MIN_SECRET_BYTES} to {MAX_SECRET_BYTES} bytes"
                    )));
                }
                if value.contains(CREDENTIAL_HANDLE_PREFIX)
                    || HeaderValue::from_str(&value).is_err()
                {
                    return Err(invalid(
                        "a credential value is text a request header can carry, not a handle",
                    ));
                }
                call.context
                    .act("credentials.set", BrokerEffect::Write, Some(name))
                    .await?;
                if call.context.cancel().is_cancelled() {
                    return Err(refused(&plugin, "cancelled", NOT_ALLOWED_HERE));
                }
                authority
                    .vault
                    .write(
                        &plugin,
                        &authority.build,
                        name,
                        Stored {
                            site: site.to_string(),
                            value,
                        },
                    )
                    .await?;
                tracing::info!(target: LOG, plugin = %plugin, credential = %name, site = %site, "credential stored");
                Ok(
                    json!({ "ref": name, "set": true, "site": site.to_string(), "handle": handle(name) }),
                )
            }
        }
    })
}

fn invalid(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Network,
        message: message.into(),
    }
}

fn refused(plugin: &str, reason: &'static str, message: &'static str) -> BrokerRefusal {
    tracing::info!(target: LOG, plugin = %plugin, reason, "network broker refused");
    BrokerRefusal::Refused {
        service: DshService::Network,
        message: message.to_string(),
    }
}

fn checked_ref(name: &str) -> Result<&str, BrokerRefusal> {
    let ok = !name.is_empty()
        && name.len() <= MAX_REF_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(name)
    } else {
        Err(invalid(format!(
            "a credential ref is 1 to {MAX_REF_LEN} letters, digits, `_`, `-` or `.`"
        )))
    }
}

fn handle(name: &str) -> String {
    format!("{CREDENTIAL_HANDLE_PREFIX}{name}")
}

fn caller_allows_network(tool: &ToolContext) -> bool {
    let mut live = tool.clone();
    live.refresh_live_posture();
    let denied = |name: &str| tool_denied(Some(live.disallowed_tools.as_slice()), name);
    network_authorized(&live)
        && !matches!(live.approval_mode, ApprovalMode::Never | ApprovalMode::Auto)
        && live.shell_network_denied_hint.is_none()
        && live.acp_host != Some(codewhale_config::AppMode::Plan)
        && !denied(NETWORK_DENIAL_SENTINEL)
        && !denied(SEND_TOOL)
}

type AnswerKey = (String, String, String);

#[derive(Default)]
struct Answers {
    known: HashMap<AnswerKey, bool>,
    order: VecDeque<AnswerKey>,
    asking: HashMap<AnswerKey, Arc<tokio::sync::Mutex<()>>>,
}

static ANSWERS: LazyLock<Mutex<Answers>> = LazyLock::new(Mutex::default);
static PENDING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);
static SLOTS: LazyLock<Mutex<HashMap<String, Arc<Semaphore>>>> = LazyLock::new(Mutex::default);

struct PendingRequest(String);

impl PendingRequest {
    fn open() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        PENDING.lock().expect("pending lock").insert(id.clone());
        Self(id)
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        PENDING.lock().expect("pending lock").remove(&self.0);
    }
}

fn answered(key: &AnswerKey) -> Option<bool> {
    ANSWERS
        .lock()
        .expect("answers lock")
        .known
        .get(key)
        .copied()
}

fn declined() -> BrokerRefusal {
    BrokerRefusal::Denied {
        service: DshService::Network,
        message: DECLINED.to_string(),
    }
}

async fn consent(call: &BrokerCall, site: &Origin) -> Result<(), BrokerRefusal> {
    let plugin = &call.owner.plugin_id;
    let key = (call.session_id.clone(), plugin.clone(), site.to_string());
    let turn = {
        let mut answers = ANSWERS.lock().expect("answers lock");
        match answers.known.get(&key).copied() {
            Some(true) => return Ok(()),
            Some(false) => return Err(declined()),
            None => {
                if answers.asking.len() >= MAX_REMEMBERED_ANSWERS {
                    answers.asking.retain(|_, turn| Arc::strong_count(turn) > 1);
                }
                Arc::clone(answers.asking.entry(key.clone()).or_default())
            }
        }
    };
    let waiting = turn.lock().await;
    let mut answer = answered(&key);
    if answer.is_none() {
        let request = PendingRequest::open();
        let outcome = call
            .context
            .core_call(
                SEND_TOOL,
                json!({ "request": request.0, "plugin": plugin, "url": format!("{site}/") }),
            )
            .await;
        answer = match outcome {
            Ok(response) if response.ok => Some(true),
            Err(BrokerRefusal::Denied { .. }) => Some(false),
            _ => None,
        };
        tracing::info!(target: LOG, plugin = %plugin, site = %site, answer = ?answer, "first send to a site asked");
    }
    {
        let mut answers = ANSWERS.lock().expect("answers lock");
        if let Some(answer) = answer
            && answers.known.insert(key.clone(), answer).is_none()
        {
            answers.order.push_back(key.clone());
            while answers.order.len() > MAX_REMEMBERED_ANSWERS {
                if let Some(oldest) = answers.order.pop_front() {
                    answers.known.remove(&oldest);
                }
            }
        }
        drop(waiting);
        if Arc::strong_count(&turn) <= 2 {
            answers.asking.remove(&key);
        }
    }
    match answer {
        Some(true) => Ok(()),
        Some(false) => Err(declined()),
        None => Err(refused(plugin, "no_answer", NOT_ALLOWED_HERE)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentInput {
    request: String,
    plugin: String,
    url: String,
}

fn pending(input: &Value) -> Result<ConsentInput, ToolError> {
    let asked: ConsentInput = serde_json::from_value(input.clone())
        .map_err(|_| ToolError::invalid_input("not a plugin network request"))?;
    if PENDING
        .lock()
        .expect("pending lock")
        .contains(&asked.request)
    {
        Ok(asked)
    } else {
        Err(ToolError::permission_denied(
            "only the plugin network broker raises this request",
        ))
    }
}

pub(super) struct SendConsent;

#[async_trait]
impl ToolSpec for SendConsent {
    fn name(&self) -> &str {
        SEND_TOOL
    }

    fn description(&self) -> &str {
        "Asks the person before a plugin sends data to a site for the first time in a session. Only the plugin network broker raises it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "request": { "type": "string" },
                "plugin": { "type": "string" },
                "url": { "type": "string" },
            },
            "required": ["request", "plugin", "url"],
            "additionalProperties": false,
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::RequiresApproval]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn prepare(&self, input: Value, _context: &ToolContext) -> Result<PreparedToolCall, ToolError> {
        let asked = pending(&input)?;
        Ok(PreparedToolCall {
            name: SEND_TOOL.to_string(),
            description: format!(
                "Plugin `{}` wants to send data to {}. Approving lets it send to this site for the rest of this session.",
                asked.plugin,
                asked.url.trim_end_matches('/')
            ),
            read_only: false,
            supports_parallel: true,
            starts_detached: false,
            approval: ApprovalRequirement::Required,
            resources: vec![ResourceClaim::GlobalExclusive],
            input,
        })
    }

    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        pending(&input)?;
        Ok(ToolResult::success("approved"))
    }
}

fn slots(call: &BrokerCall) -> Arc<Semaphore> {
    let key = format!(
        "{}\n{}",
        call.session_id,
        call.context
            .tool()
            .origin_tool_call_id
            .as_deref()
            .unwrap_or_default()
    );
    let mut slots = SLOTS.lock().expect("slots lock");
    slots.retain(|_, slot| Arc::strong_count(slot) > 1);
    Arc::clone(
        slots
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_FETCHES_AT_ONCE))),
    )
}

enum Part {
    Text(String),
    Handle(String),
}

struct HeaderPlan {
    name: HeaderName,
    parts: Vec<Part>,
}

impl HeaderPlan {
    fn refs(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Handle(name) => Some(name.as_str()),
            Part::Text(_) => None,
        })
    }
}

fn plan_headers(headers: &[(String, String)]) -> Result<Vec<HeaderPlan>, BrokerRefusal> {
    if headers.len() > MAX_HEADERS
        || headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > MAX_HEADER_BYTES
    {
        return Err(invalid("too many or too large request headers"));
    }
    let mut plans = Vec::with_capacity(headers.len());
    let mut handles = 0usize;
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid("a request header name is not valid"))?;
        if FORBIDDEN_HEADERS.contains(&name.as_str()) {
            return Err(invalid(format!(
                "request header `{name}` is set by the broker, not the plugin"
            )));
        }
        let mut parts = Vec::new();
        let mut rest = value.as_str();
        while let Some(at) = rest.find(CREDENTIAL_HANDLE_PREFIX) {
            if at > 0 {
                parts.push(Part::Text(rest[..at].to_string()));
            }
            let tail = &rest[at + CREDENTIAL_HANDLE_PREFIX.len()..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
                .unwrap_or(tail.len());
            parts.push(Part::Handle(checked_ref(&tail[..end])?.to_string()));
            handles += 1;
            if handles > MAX_HANDLES_PER_REQUEST {
                return Err(invalid(format!(
                    "a request carries at most {MAX_HANDLES_PER_REQUEST} credential handles"
                )));
            }
            rest = &tail[end..];
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.to_string()));
        }
        plans.push(HeaderPlan { name, parts });
    }
    if handles > 0
        && let Some(plan) = plans
            .iter()
            .find(|plan| FORBIDDEN_WITH_A_CREDENTIAL.contains(&plan.name.as_str()))
    {
        return Err(invalid(format!(
            "request header `{}` is not sent together with a credential",
            plan.name
        )));
    }
    Ok(plans)
}

struct Rendered {
    headers: Vec<(HeaderName, HeaderValue)>,
    sent: Vec<String>,
}

fn render_headers(
    plugin: &str,
    plans: &[HeaderPlan],
    credentials: &BTreeMap<String, Stored>,
    vault: &Vault,
    site: &Origin,
    first: &Origin,
) -> Result<Rendered, BrokerRefusal> {
    let here = site.to_string();
    let mut rendered = Rendered {
        headers: Vec::with_capacity(plans.len()),
        sent: Vec::new(),
    };
    let mut bytes = 0usize;
    for plan in plans {
        let bound_here = plan.refs().next().is_none()
            || (vault.carries(site)
                && plan.refs().all(|name| {
                    credentials
                        .get(name)
                        .is_some_and(|stored| stored.site == here)
                }));
        if !bound_here {
            if site == first {
                return Err(refused(plugin, "credential_binding", NOT_FOR_THIS_SITE));
            }
            continue;
        }
        if site != first && DROPPED_ON_ANOTHER_SITE.contains(&plan.name.as_str()) {
            continue;
        }
        let mut value = String::new();
        for part in &plan.parts {
            match part {
                Part::Text(text) => value.push_str(text),
                Part::Handle(name) => {
                    value.push_str(&credentials[name].value);
                    if !rendered.sent.contains(name) {
                        rendered.sent.push(name.clone());
                    }
                }
            }
        }
        bytes += plan.name.as_str().len() + value.len();
        if bytes > MAX_HEADER_BYTES {
            return Err(invalid(
                "request headers are too large once credentials are filled in",
            ));
        }
        let value = HeaderValue::from_str(&value).map_err(|_| {
            invalid(format!(
                "request header `{}` has an invalid value",
                plan.name
            ))
        })?;
        rendered.headers.push((plan.name.clone(), value));
    }
    Ok(rendered)
}

fn failure_table(needle: &[u8]) -> Vec<usize> {
    let mut table = vec![0usize; needle.len()];
    let mut matched = 0usize;
    for at in 1..needle.len() {
        while matched > 0 && needle[matched] != needle[at] {
            matched = table[matched - 1];
        }
        if needle[matched] == needle[at] {
            matched += 1;
        }
        table[at] = matched;
    }
    table
}

fn scrub(bytes: &[u8], secrets: &[(String, String)], cap: usize) -> Option<Vec<u8>> {
    const INSIDE: u8 = u8::MAX;
    let mut cover = vec![0u8; bytes.len()];
    for (index, (_, secret)) in secrets.iter().enumerate().take(usize::from(INSIDE) - 1) {
        let needle = secret.as_bytes();
        if needle.is_empty() || needle.len() > bytes.len() {
            continue;
        }
        let table = failure_table(needle);
        let tag = index as u8 + 1;
        let (mut matched, mut marked) = (0usize, 0usize);
        for (at, byte) in bytes.iter().enumerate() {
            while matched > 0 && needle[matched] != *byte {
                matched = table[matched - 1];
            }
            if needle[matched] == *byte {
                matched += 1;
            }
            if matched == needle.len() {
                let start = at + 1 - needle.len();
                for slot in &mut cover[start.max(marked)..=at] {
                    if *slot == 0 {
                        *slot = INSIDE;
                    }
                }
                if cover[start] == INSIDE {
                    cover[start] = tag;
                }
                marked = at + 1;
                matched = table[matched - 1];
            }
        }
    }
    let mut out = Vec::with_capacity(bytes.len().min(cap));
    let mut at = 0usize;
    while at < bytes.len() {
        if cover[at] == 0 {
            out.push(bytes[at]);
            at += 1;
        } else {
            let handle = secrets
                .get(usize::from(cover[at]) - 1)
                .map_or(CREDENTIAL_HANDLE_PREFIX, |(handle, _)| handle.as_str());
            out.extend_from_slice(handle.as_bytes());
            while at < bytes.len() && cover[at] != 0 {
                at += 1;
            }
        }
        if out.len() > cap {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
pub(super) static SCRUBBED_ON: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

struct Scrubbed {
    url: String,
    headers: Vec<(String, String)>,
    body_base64: String,
}

fn scrub_answer(
    url: String,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    secrets: Vec<(String, String)>,
) -> Option<Scrubbed> {
    #[cfg(test)]
    {
        *SCRUBBED_ON.lock().expect("scrub thread lock") = Some(std::thread::current().id());
    }
    let text = |bytes: &[u8], cap: usize| {
        scrub(bytes, &secrets, cap).and_then(|clean| String::from_utf8(clean).ok())
    };
    Some(Scrubbed {
        url: text(url.as_bytes(), MAX_RESPONSE_BYTES)?,
        headers: headers
            .into_iter()
            .filter_map(|(name, value)| Some((name, text(&value, MAX_HEADER_BYTES)?)))
            .collect(),
        body_base64: STANDARD.encode(scrub(&body, &secrets, MAX_RESPONSE_BYTES)?),
    })
}

struct Receipt(Option<Value>);

impl Receipt {
    fn open(call: &BrokerCall, site: &Origin, method: &str, bytes_out: usize) -> Self {
        Self(Some(json!({
            "plugin": call.owner.plugin_id,
            "session": call.session_id,
            "site": site.to_string(),
            "method": method,
            "bytes_out": bytes_out,
            "bytes_in": 0,
            "status": Value::Null,
            "outcome": "cancelled",
        })))
    }

    async fn close(mut self, status: Option<u16>, bytes_in: usize, outcome: &'static str) {
        if let Some(mut event) = self.0.take() {
            event["status"] = json!(status);
            event["bytes_in"] = json!(bytes_in);
            event["outcome"] = json!(outcome);
            let _ = tokio::task::spawn_blocking(move || {
                codewhale_runtime::audit::log_sensitive_event(AUDIT_EVENT, event);
            })
            .await;
        }
    }
}

impl Drop for Receipt {
    fn drop(&mut self) {
        if let Some(event) = self.0.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn_blocking(move || {
                codewhale_runtime::audit::log_sensitive_event(AUDIT_EVENT, event);
            });
        }
    }
}

fn pinned_client(
    pin: Option<(String, std::net::IpAddr)>,
    timeout: Duration,
) -> Result<reqwest::Client, BrokerRefusal> {
    let mut builder = guarded_reqwest_client_builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    if let Some((name, ip)) = pin {
        builder = builder.resolve(&name, std::net::SocketAddr::new(ip, 0));
    }
    builder
        .build()
        .map_err(|_| invalid("the network broker could not build its client"))
}

fn failed(plugin: &str, error: &reqwest::Error) -> BrokerRefusal {
    let (reason, message) = if error.is_timeout() {
        ("timeout", "the request failed: timed out")
    } else if error.is_connect() {
        ("connect", "the request failed: could not connect")
    } else {
        ("transport", "the request failed: transport error")
    };
    refused(plugin, reason, message)
}

async fn fetch(
    call: &BrokerCall,
    authority: &NetworkAuthority,
    input: FetchInput,
) -> Result<Value, BrokerRefusal> {
    let plugin = call.owner.plugin_id.as_str();
    let tool = call.context.tool();
    let mut url = reqwest::Url::parse(&input.url).map_err(|_| invalid("the URL is not valid"))?;
    if input.url.contains(CREDENTIAL_HANDLE_PREFIX) {
        return Err(invalid(
            "credential handles are substituted only in request header values",
        ));
    }
    let mut method = input
        .method
        .as_deref()
        .unwrap_or("GET")
        .to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(invalid(
            "the request method is not one the network broker sends",
        ));
    }
    let plans = plan_headers(&input.headers)?;
    let mut body = match input.body_base64.as_deref() {
        Some(encoded) => {
            if encoded.len() > MAX_REQUEST_BYTES.div_ceil(3) * 4 {
                return Err(invalid("the request body is too large"));
            }
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| invalid("body_base64 is not valid base64"))?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(invalid("the request body is too large"));
            }
            Some(bytes)
        }
        None => None,
    };
    let first = Origin::of(&url).ok_or_else(|| {
        invalid("only http and https URLs with a plain host and no userinfo are brokered")
    })?;
    let credentials = if plans.iter().any(|plan| plan.refs().next().is_some()) {
        let stored = authority.vault.read(plugin, &authority.build).await?;
        if let Some(missing) = plans
            .iter()
            .flat_map(|plan| plan.refs())
            .find(|name| !stored.contains_key(*name))
        {
            return Err(invalid(format!(
                "credential `{missing}` is not set for this plugin"
            )));
        }
        stored
    } else {
        BTreeMap::new()
    };
    let slot = slots(call);
    let _slot = slot
        .acquire()
        .await
        .map_err(|_| refused(plugin, "slots_closed", NOT_ALLOWED_HERE))?;
    let timeout = input
        .timeout_ms
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(MAX_TIMEOUT);
    let mut deadline = Instant::now() + timeout;
    let mut redirects = 0usize;
    let mut sent: Vec<String> = Vec::new();
    let (response, receipt) = loop {
        if call.context.cancel().is_cancelled() || !caller_allows_network(tool) {
            return Err(refused(plugin, "caller_limit", NOT_ALLOWED_HERE));
        }
        let site = Origin::of(&url)
            .filter(|site| authority.sites.contains(site))
            .ok_or_else(|| refused(plugin, "unreviewed_site", NOT_REVIEWED))?;
        let decider = tool
            .network_policy
            .as_ref()
            .ok_or_else(|| refused(plugin, "no_policy", NOT_ALLOWED_HERE))?;
        validate_network_policy(&site.host, tool, AUDIT_LABEL)
            .map_err(|_| refused(plugin, "policy", NOT_ALLOWED_HERE))?;
        let pin = {
            #[cfg(test)]
            if let Some(ip) = authority.test_dns_pins.get(&site.host) {
                Some((site.host.clone(), *ip))
            } else {
                validate_fetch_destination(
                    site.host.clone(),
                    Some(decider),
                    AUDIT_LABEL,
                    true,
                    PrivateTargets::Refuse,
                )
                .await
                .map_err(|_| refused(plugin, "address", NOT_ALLOWED_HERE))?
            }
            #[cfg(not(test))]
            validate_fetch_destination(
                site.host.clone(),
                Some(decider),
                AUDIT_LABEL,
                true,
                PrivateTargets::Refuse,
            )
            .await
            .map_err(|_| refused(plugin, "address", NOT_ALLOWED_HERE))?
        };
        let rendered = render_headers(
            plugin,
            &plans,
            &credentials,
            &authority.vault,
            &site,
            &first,
        )?;
        if answered(&(
            call.session_id.clone(),
            plugin.to_string(),
            site.to_string(),
        )) == Some(false)
        {
            return Err(declined());
        }
        let asked = Instant::now();
        call.context
            .act("fetch", BrokerEffect::Reach, Some(&site.host))
            .await?;
        consent(call, &site).await?;
        deadline += asked.elapsed();
        if call.context.cancel().is_cancelled() || !caller_allows_network(tool) {
            return Err(refused(plugin, "caller_limit", NOT_ALLOWED_HERE));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(refused(plugin, "timeout", "the request failed: timed out"));
        }
        let client = pinned_client(pin, remaining)?;
        let verb = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| invalid("the request method is not valid"))?;
        let mut request = client.request(verb, url.clone());
        for (name, value) in &rendered.headers {
            request = request.header(name, value);
        }
        if let Some(bytes) = body.clone() {
            request = request.body(bytes);
        }
        for name in rendered.sent {
            if !sent.contains(&name) {
                sent.push(name);
            }
        }
        let receipt = Receipt::open(call, &site, &method, body.as_ref().map_or(0, Vec::len));
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                receipt.close(None, 0, "failed").await;
                return Err(failed(plugin, &error));
            }
        };
        let status = response.status();
        let location = status
            .is_redirection()
            .then(|| response.headers().get(reqwest::header::LOCATION))
            .flatten()
            .and_then(|value| value.to_str().ok());
        let Some(location) = location else {
            break (response, receipt);
        };
        let next = url.join(location);
        drop(response);
        receipt.close(Some(status.as_u16()), 0, "redirected").await;
        if redirects >= MAX_REDIRECTS {
            return Err(refused(
                plugin,
                "redirects",
                "the request failed: too many redirects",
            ));
        }
        if status == reqwest::StatusCode::SEE_OTHER
            || (matches!(status.as_u16(), 301 | 302) && method == "POST")
        {
            method = "GET".into();
            body = None;
        }
        url = next.map_err(|_| refused(plugin, "redirect_location", NOT_ALLOWED_HERE))?;
        redirects += 1;
    };
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let headers: Vec<(String, Vec<u8>)> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
        .collect();
    let mut response = response;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = match tokio::time::timeout(remaining, response.chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(_)) | Err(_) => {
                receipt.close(Some(status), bytes.len(), "failed").await;
                return Err(refused(
                    plugin,
                    "body",
                    "the request failed: the response could not be read",
                ));
            }
        };
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            receipt
                .close(Some(status), bytes.len(), "response_too_large")
                .await;
            return Err(refused(
                plugin,
                "response_too_large",
                "the request failed: the response is larger than the broker returns",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let bytes_in = bytes.len();
    let secrets: Vec<(String, String)> = sent
        .iter()
        .map(|name| (handle(name), credentials[name].value.clone()))
        .collect();
    let scrubbed =
        tokio::task::spawn_blocking(move || scrub_answer(final_url, headers, bytes, secrets))
            .await
            .ok()
            .flatten();
    let Some(scrubbed) = scrubbed else {
        receipt
            .close(Some(status), bytes_in, "response_too_large")
            .await;
        return Err(refused(
            plugin,
            "scrub_cap",
            "the request failed: the response is larger than the broker returns",
        ));
    };
    receipt.close(Some(status), bytes_in, "ok").await;
    tracing::info!(target: LOG, plugin = %plugin, site = %first, status, redirects, "brokered fetch");
    Ok(json!({
        "status": status,
        "url": scrubbed.url,
        "redirected": redirects > 0,
        "headers": scrubbed.headers,
        "body_base64": scrubbed.body_base64,
    }))
}

#[cfg(test)]
#[path = "dsh_network_tests.rs"]
mod tests;
