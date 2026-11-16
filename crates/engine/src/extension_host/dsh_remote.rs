use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use codewhale_config::OwnerRef;
use codewhale_core::secret_eq::constant_time_eq;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::ExtensionHostManager;
use super::dsh_broker::{
    self, BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, decode,
};
use super::protocol::DshService;
use super::tier::HostTier;

const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);
const CLIENT_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_PENDING_PER_SESSION: usize = 4;
const MAX_PENDING_PER_PLUGIN: usize = 8;
const MAX_PENDING_TOTAL: usize = 16;
const MAX_GRANTS: usize = 64;
const MAX_LABEL_CHARS: usize = 32;
const LABEL_MARKS: &str = "-_.'()#+&";
const HOST_NAME: &str = "codewhale";
const GRANT_PREFIX: &str = "cwrg_";
const PAIRING_PREFIX: &str = "cwrp_";
const CLIENT_PREFIX: &str = "cwrc_";

#[derive(Deserialize)]
#[serde(
    tag = "method",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum RemoteMethod {
    Status {},
    Pair {
        #[serde(default)]
        label: Option<String>,
    },
    Clients {},
    Revoke {
        grant: String,
    },
}

impl BrokerMethods for RemoteMethod {
    const NAMES: &'static [&'static str] = &["status", "pair", "clients", "revoke"];
}

#[derive(Clone)]
pub struct RemoteSecret(String);

impl RemoteSecret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RemoteSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteSecret(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteState {
    AwaitingHost,
    Paired,
}

impl RemoteState {
    fn name(self) -> &'static str {
        match self {
            Self::AwaitingHost => "awaiting_host",
            Self::Paired => "paired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePairing {
    pub grant: String,
    pub plugin_id: String,
    pub plugin_name: String,
    pub session_id: String,
    pub state: RemoteState,
    pub plugin_label: String,
    pub client_label: Option<String>,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteClient {
    pub grant: String,
    pub session_id: String,
    pub token: RemoteSecret,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteDenial {
    Unknown,
    NonLoopback,
    ForeignSession,
}

#[cfg(test)]
thread_local! {
    static WALL_SKEW: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

#[derive(Clone, Copy)]
struct Now {
    mono: Instant,
    wall: SystemTime,
}

impl Now {
    fn read() -> Self {
        let wall = SystemTime::now();
        #[cfg(test)]
        let wall = wall + WALL_SKEW.with(std::cell::Cell::get);
        Self {
            mono: Instant::now(),
            wall,
        }
    }

    fn after(self, ttl: Duration) -> Deadline {
        Deadline {
            mono: self.mono + ttl,
            wall: self.wall + ttl,
        }
    }
}

#[derive(Clone, Copy)]
struct Deadline {
    mono: Instant,
    wall: SystemTime,
}

impl Deadline {
    fn passed(self, now: Now) -> bool {
        now.mono >= self.mono || now.wall >= self.wall
    }

    fn seconds_left(self, now: Now) -> u64 {
        self.mono
            .saturating_duration_since(now.mono)
            .min(self.wall.duration_since(now.wall).unwrap_or_default())
            .as_secs()
    }
}

enum Stage {
    AwaitingHost {
        presented: Option<[u8; 32]>,
    },
    Paired {
        token: [u8; 32],
        client_label: String,
    },
}

struct Grant {
    owner: OwnerRef,
    session_id: String,
    label: String,
    expires: Deadline,
    stage: Stage,
}

impl Grant {
    fn state(&self) -> RemoteState {
        match self.stage {
            Stage::AwaitingHost { .. } => RemoteState::AwaitingHost,
            Stage::Paired { .. } => RemoteState::Paired,
        }
    }

    fn held_by(&self, owner: &OwnerRef, session: &str) -> bool {
        self.owner == *owner && self.session_id == session
    }
}

type Table = HashMap<String, Grant>;

struct RemoteGrants {
    pairing_ttl: Duration,
    client_ttl: Duration,
    grants: Mutex<Table>,
}

fn remote_grants() -> &'static RemoteGrants {
    static GRANTS: OnceLock<RemoteGrants> = OnceLock::new();
    GRANTS.get_or_init(|| RemoteGrants::new(PAIRING_TTL, CLIENT_TTL))
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move { remote_grants().serve(&call).await })
}

pub(super) fn withdraw_owner(owner: &OwnerRef) -> usize {
    remote_grants().withdraw_owner(owner)
}

pub(crate) fn end_remote_session(session_id: &str) -> usize {
    remote_grants().revoke_session(session_id)
}

fn refused(message: &str) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Remote,
        message: message.to_string(),
    }
}

fn invalid(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Remote,
        message: message.into(),
    }
}

fn prune_stale_generation(grants: &mut Table, owner: &OwnerRef) -> Result<(), BrokerRefusal> {
    if grants.values().any(|grant| {
        grant.owner.plugin_id == owner.plugin_id && grant.owner.generation > owner.generation
    }) {
        return Err(refused(
            "the remote broker serves the plugin's current activation only",
        ));
    }
    grants.retain(|_, grant| grant.owner.plugin_id != owner.plugin_id || grant.owner == *owner);
    Ok(())
}

fn held_grant(grants: &Table, owner: &OwnerRef, session: &str, grant: &str) -> bool {
    well_formed(grant, GRANT_PREFIX)
        && grants
            .get(grant)
            .is_some_and(|row| row.held_by(owner, session))
}

impl RemoteGrants {
    fn new(pairing_ttl: Duration, client_ttl: Duration) -> Self {
        Self {
            pairing_ttl,
            client_ttl,
            grants: Mutex::new(Table::new()),
        }
    }

    fn lock(&self) -> (std::sync::MutexGuard<'_, Table>, Now) {
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Now::read();
        grants.retain(|_, grant| !grant.expires.passed(now));
        (grants, now)
    }

    fn has_pairing_room(&self, grants: &Table, plugin: &str, session: &str) -> bool {
        let pending = |counted: &dyn Fn(&Grant) -> bool| {
            grants
                .values()
                .filter(|grant| grant.state() == RemoteState::AwaitingHost && counted(grant))
                .count()
        };
        grants.len() < MAX_GRANTS
            && pending(&|_| true) < MAX_PENDING_TOTAL
            && pending(&|grant| grant.owner.plugin_id == plugin) < MAX_PENDING_PER_PLUGIN
            && pending(&|grant| grant.owner.plugin_id == plugin && grant.session_id == session)
                < MAX_PENDING_PER_SESSION
    }

    fn validate_pair(
        &self,
        grants: &Table,
        call: &BrokerCall,
        session: &str,
        label: Option<&str>,
    ) -> Result<String, BrokerRefusal> {
        let execution = &call.context.tool().execution;
        if execution.owner_agent_id.is_some() || execution.child_host.is_some() {
            return Err(refused(
                "remote pairing is offered to the main session only, not to a child agent",
            ));
        }
        let label = plugin_label(label).map_err(invalid)?;
        if !self.has_pairing_room(grants, &call.owner.plugin_id, session) {
            return Err(refused(
                "too many remote pairings are waiting; revoke one or try again later",
            ));
        }
        Ok(label)
    }

    fn precheck(
        &self,
        call: &BrokerCall,
        method: &RemoteMethod,
    ) -> Result<(&'static str, BrokerEffect), BrokerRefusal> {
        let owner = &call.owner;
        if HostTier::Plugin.check_owner_id(&owner.plugin_id).is_err() {
            return Err(refused("the remote broker serves reviewed plugins only"));
        }
        let session = call.session_id.as_str();
        let (mut grants, _) = self.lock();
        prune_stale_generation(&mut grants, owner)?;
        match method {
            RemoteMethod::Status {} => Ok(("status", BrokerEffect::Read)),
            RemoteMethod::Clients {} => Ok(("clients", BrokerEffect::Read)),
            RemoteMethod::Pair { label } => {
                self.validate_pair(&grants, call, session, label.as_deref())?;
                Ok(("pair", BrokerEffect::Reach))
            }
            RemoteMethod::Revoke { grant } => {
                if !held_grant(&grants, owner, session, grant) {
                    return Err(invalid("no such remote grant for this plugin and session"));
                }
                Ok(("revoke", BrokerEffect::Write))
            }
        }
    }

    async fn serve(&self, call: &BrokerCall) -> Result<Value, BrokerRefusal> {
        if call.context.cancel().is_cancelled() {
            return Err(refused("the remote broker call was withdrawn"));
        }
        let method = decode::<RemoteMethod>(call)?;
        let (method_name, effect) = self.precheck(call, &method)?;
        call.context.act(method_name, effect, None).await?;
        let owner = &call.owner;
        let session = call.session_id.as_str();
        let (mut grants, now) = self.lock();
        prune_stale_generation(&mut grants, owner)?;
        match method {
            RemoteMethod::Status {} => Ok(status(&grants, owner, session)),
            RemoteMethod::Pair { label } => {
                self.pair(&mut grants, now, call, session, label.as_deref())
            }
            RemoteMethod::Clients {} => Ok(clients(&grants, now, owner, session)),
            RemoteMethod::Revoke { grant } => {
                if !held_grant(&grants, owner, session, &grant) {
                    return Err(invalid("no such remote grant for this plugin and session"));
                }
                grants.remove(&grant);
                Ok(json!({ "grant": grant, "revoked": true }))
            }
        }
    }

    fn pair(
        &self,
        grants: &mut Table,
        now: Now,
        call: &BrokerCall,
        session: &str,
        label: Option<&str>,
    ) -> Result<Value, BrokerRefusal> {
        let label = self.validate_pair(grants, call, session, label)?;
        let id = random_capability(GRANT_PREFIX);
        grants.insert(
            id.clone(),
            Grant {
                owner: call.owner.clone(),
                session_id: session.to_string(),
                label: label.clone(),
                expires: now.after(self.pairing_ttl),
                stage: Stage::AwaitingHost { presented: None },
            },
        );
        Ok(json!({
            "grant": id,
            "label": label,
            "state": RemoteState::AwaitingHost.name(),
            "expires_in_seconds": self.pairing_ttl.as_secs(),
        }))
    }

    fn rows(&self) -> Vec<(OwnerRef, RemotePairing)> {
        let (grants, now) = self.lock();
        let mut rows: Vec<(OwnerRef, RemotePairing)> = grants
            .iter()
            .map(|(id, grant)| {
                (
                    grant.owner.clone(),
                    RemotePairing {
                        grant: id.clone(),
                        plugin_id: grant.owner.plugin_id.clone(),
                        plugin_name: String::new(),
                        session_id: grant.session_id.clone(),
                        state: grant.state(),
                        plugin_label: grant.label.clone(),
                        client_label: match &grant.stage {
                            Stage::AwaitingHost { .. } => None,
                            Stage::Paired { client_label, .. } => Some(client_label.clone()),
                        },
                        expires_in_seconds: grant.expires.seconds_left(now),
                    },
                )
            })
            .collect();
        rows.sort_by(|a, b| a.1.grant.cmp(&b.1.grant));
        rows
    }

    fn admit(
        &self,
        find: impl Fn(&Table) -> Option<String>,
        live: &dyn Fn(&OwnerRef) -> bool,
    ) -> Result<String, RemoteDenial> {
        let (id, owner) = {
            let (grants, _) = self.lock();
            let id = find(&grants).ok_or(RemoteDenial::Unknown)?;
            let owner = grants[&id].owner.clone();
            (id, owner)
        };
        if live(&owner) {
            return Ok(id);
        }
        self.lock().0.remove(&id);
        Err(RemoteDenial::Unknown)
    }

    fn present(
        &self,
        grant: &str,
        live: &dyn Fn(&OwnerRef) -> bool,
    ) -> Result<RemoteSecret, RemoteDenial> {
        if !well_formed(grant, GRANT_PREFIX) {
            return Err(RemoteDenial::Unknown);
        }
        let id = self.admit(
            |grants| grants.contains_key(grant).then(|| grant.to_string()),
            live,
        )?;
        let (mut grants, _) = self.lock();
        let Some(Stage::AwaitingHost { presented }) = grants.get_mut(&id).map(|row| &mut row.stage)
        else {
            return Err(RemoteDenial::Unknown);
        };
        let secret = random_capability(PAIRING_PREFIX);
        *presented = Some(digest(&secret));
        Ok(RemoteSecret(secret))
    }

    fn redeem(
        &self,
        secret: &str,
        client_label: &str,
        peer: IpAddr,
        live: &dyn Fn(&OwnerRef) -> bool,
    ) -> Result<RemoteClient, RemoteDenial> {
        if !peer.is_loopback() {
            return Err(RemoteDenial::NonLoopback);
        }
        if !well_formed(secret, PAIRING_PREFIX) {
            return Err(RemoteDenial::Unknown);
        }
        let wanted = digest(secret);
        let presented = |grant: &Grant| match &grant.stage {
            Stage::AwaitingHost {
                presented: Some(presented),
            } => constant_time_eq(presented, &wanted),
            _ => false,
        };
        let id = self.admit(|grants| find(grants, &presented), live)?;
        let (mut grants, now) = self.lock();
        let Some(grant) = grants.get_mut(&id) else {
            return Err(RemoteDenial::Unknown);
        };
        if !presented(grant) {
            return Err(RemoteDenial::Unknown);
        }
        let token = random_capability(CLIENT_PREFIX);
        grant.stage = Stage::Paired {
            token: digest(&token),
            client_label: device_label(client_label),
        };
        grant.expires = now.after(self.client_ttl);
        Ok(RemoteClient {
            grant: id,
            session_id: grant.session_id.clone(),
            token: RemoteSecret(token),
            expires_in_seconds: self.client_ttl.as_secs(),
        })
    }

    fn authorize(
        &self,
        token: &str,
        session_id: &str,
        live: &dyn Fn(&OwnerRef) -> bool,
    ) -> Result<String, RemoteDenial> {
        if !well_formed(token, CLIENT_PREFIX) {
            return Err(RemoteDenial::Unknown);
        }
        let wanted = digest(token);
        let paired = |grant: &Grant| match &grant.stage {
            Stage::Paired { token, .. } => constant_time_eq(token, &wanted),
            Stage::AwaitingHost { .. } => false,
        };
        let id = self.admit(|grants| find(grants, &paired), live)?;
        let (grants, _) = self.lock();
        let Some(grant) = grants.get(&id) else {
            return Err(RemoteDenial::Unknown);
        };
        if !paired(grant) {
            return Err(RemoteDenial::Unknown);
        }
        if grant.session_id != session_id {
            return Err(RemoteDenial::ForeignSession);
        }
        Ok(id)
    }

    fn withdraw(&self, grant: &str) -> bool {
        well_formed(grant, GRANT_PREFIX) && self.lock().0.remove(grant).is_some()
    }

    fn withdraw_owner(&self, owner: &OwnerRef) -> usize {
        self.drop_where(|grant| grant.owner == *owner)
    }

    fn revoke_session(&self, session_id: &str) -> usize {
        self.drop_where(|grant| grant.session_id == session_id)
    }

    fn drop_where(&self, gone: impl Fn(&Grant) -> bool) -> usize {
        let (mut grants, _) = self.lock();
        let before = grants.len();
        grants.retain(|_, grant| !gone(grant));
        before - grants.len()
    }
}

fn status(grants: &Table, owner: &OwnerRef, session: &str) -> Value {
    let count = |state: RemoteState| {
        grants
            .values()
            .filter(|grant| grant.held_by(owner, session) && grant.state() == state)
            .count()
    };
    json!({
        "transport": "runtime_http_sse",
        "pairing": "host_presented",
        "public_listener": false,
        "tunnel": false,
        "awaiting_host": count(RemoteState::AwaitingHost),
        "paired": count(RemoteState::Paired),
        "max_pending": MAX_PENDING_PER_SESSION,
    })
}

fn clients(grants: &Table, now: Now, owner: &OwnerRef, session: &str) -> Value {
    let mut rows: Vec<Value> = grants
        .iter()
        .filter(|(_, grant)| grant.held_by(owner, session))
        .map(|(id, grant)| {
            json!({
                "grant": id,
                "label": grant.label,
                "state": grant.state().name(),
                "expires_in_seconds": grant.expires.seconds_left(now),
            })
        })
        .collect();
    rows.sort_by(|a, b| a["grant"].as_str().cmp(&b["grant"].as_str()));
    json!({ "clients": rows })
}

fn find(grants: &Table, matches: &dyn Fn(&Grant) -> bool) -> Option<String> {
    let mut found = None;
    for (id, grant) in grants {
        if matches(grant) {
            found = Some(id.clone());
        }
    }
    found
}

impl ExtensionHostManager {
    fn remote_owner_is_live(&self, owner: &OwnerRef) -> bool {
        let registry = self.shared.registry.lock().expect("registry lock");
        registry.tier_of(owner) == Some(HostTier::Plugin)
            && dsh_broker::granted(&registry, owner, DshService::Remote)
    }

    #[must_use]
    pub fn remote_pairings(&self) -> Vec<RemotePairing> {
        let rows = remote_grants().rows();
        let registry = self.shared.registry.lock().expect("registry lock");
        rows.into_iter()
            .filter_map(|(owner, mut row)| {
                let entry = registry.owner(&owner.plugin_id).filter(|entry| {
                    entry.owner == owner
                        && registry.tier_of(&owner) == Some(HostTier::Plugin)
                        && dsh_broker::granted(&registry, &owner, DshService::Remote)
                })?;
                row.plugin_name.clone_from(&entry.plugin_name);
                Some(row)
            })
            .collect()
    }

    pub fn present_remote_pairing(&self, grant: &str) -> Result<RemoteSecret, RemoteDenial> {
        remote_grants().present(grant, &|owner| self.remote_owner_is_live(owner))
    }

    pub fn redeem_remote_pairing(
        &self,
        secret: &str,
        client_label: &str,
        peer: IpAddr,
    ) -> Result<RemoteClient, RemoteDenial> {
        remote_grants().redeem(secret, client_label, peer, &|owner| {
            self.remote_owner_is_live(owner)
        })
    }

    pub fn authorize_remote_client(
        &self,
        token: &str,
        session_id: &str,
    ) -> Result<String, RemoteDenial> {
        remote_grants().authorize(token, session_id, &|owner| self.remote_owner_is_live(owner))
    }

    pub fn withdraw_remote_grant(&self, grant: &str) -> bool {
        remote_grants().withdraw(grant)
    }
}

fn label_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == ' ' || LABEL_MARKS.contains(ch)
}

fn names_the_host(label: &str) -> bool {
    label
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .contains(HOST_NAME)
}

fn plugin_label(label: Option<&str>) -> Result<String, String> {
    let label: String = label.unwrap_or("").trim().nfkc().collect();
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "a remote label is at most {MAX_LABEL_CHARS} characters"
        ));
    }
    if !label.chars().all(label_char) || label.contains("  ") {
        return Err(format!(
            "a remote label holds letters, digits, single spaces and {LABEL_MARKS} only"
        ));
    }
    if names_the_host(&label) {
        return Err("a remote label may not name the host".into());
    }
    Ok(label)
}

fn device_label(label: &str) -> String {
    let mut cleaned = String::new();
    for ch in label.nfkc().filter(|ch| label_char(*ch)) {
        if ch == ' ' && (cleaned.is_empty() || cleaned.ends_with(' ')) {
            continue;
        }
        if cleaned.chars().count() == MAX_LABEL_CHARS {
            break;
        }
        cleaned.push(ch);
    }
    let cleaned = cleaned.trim_end();
    if names_the_host(cleaned) {
        String::new()
    } else {
        cleaned.to_string()
    }
}

fn well_formed(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|random| {
        random.len() == 64 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn random_capability(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests;
