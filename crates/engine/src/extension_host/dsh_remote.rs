use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::dsh_broker::{BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::protocol::DshService;

pub const PAIRING_TTL: Duration = Duration::from_secs(10 * 60);
pub const CLIENT_TTL: Duration = Duration::from_secs(30 * 60);
pub const MAX_PENDING_PAIRINGS: usize = 4;
const MAX_LABEL_CHARS: usize = 64;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePairing {
    pub grant: String,
    pub plugin_id: String,
    pub session_id: String,
    pub label: String,
    pub secret: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteClient {
    pub grant: String,
    pub session_id: String,
    pub token: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteDenial {
    Unknown,
    Expired,
    Replayed,
    ForeignSession,
}

struct Client {
    token_digest: [u8; 32],
    label: String,
    expires_at: Instant,
}

struct Grant {
    plugin_id: String,
    session_id: String,
    label: String,
    pairing_digest: [u8; 32],
    pairing: Option<String>,
    pairing_expires_at: Instant,
    client: Option<Client>,
}

impl Grant {
    fn state(&self, now: Instant) -> &'static str {
        match &self.client {
            Some(client) if now < client.expires_at => "paired",
            Some(_) => "expired",
            None if now < self.pairing_expires_at => "awaiting_host",
            None => "expired",
        }
    }

    fn retire_at(&self, pairing_ttl: Duration) -> Instant {
        self.client
            .as_ref()
            .map_or(self.pairing_expires_at, |client| client.expires_at)
            + pairing_ttl
    }
}

#[derive(Default)]
struct Grants {
    by_id: HashMap<String, Grant>,
    by_pairing: HashMap<[u8; 32], String>,
    by_client: HashMap<[u8; 32], String>,
}

impl Grants {
    fn remove(&mut self, id: &str) -> Option<Grant> {
        let grant = self.by_id.remove(id)?;
        self.by_pairing.remove(&grant.pairing_digest);
        if let Some(client) = &grant.client {
            self.by_client.remove(&client.token_digest);
        }
        Some(grant)
    }

    fn prune(&mut self, now: Instant, pairing_ttl: Duration) {
        let retired: Vec<String> = self
            .by_id
            .iter()
            .filter(|(_, grant)| now >= grant.retire_at(pairing_ttl))
            .map(|(id, _)| id.clone())
            .collect();
        for id in retired {
            self.remove(&id);
        }
    }
}

pub struct RemoteGrants {
    pairing_ttl: Duration,
    client_ttl: Duration,
    grants: Mutex<Grants>,
}

pub fn remote_grants() -> &'static RemoteGrants {
    static GRANTS: OnceLock<RemoteGrants> = OnceLock::new();
    GRANTS.get_or_init(|| RemoteGrants::new(PAIRING_TTL, CLIENT_TTL))
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(std::future::ready(remote_grants().serve(&call)))
}

impl RemoteGrants {
    #[must_use]
    pub fn new(pairing_ttl: Duration, client_ttl: Duration) -> Self {
        Self {
            pairing_ttl,
            client_ttl,
            grants: Mutex::new(Grants::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Grants> {
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        grants.prune(Instant::now(), self.pairing_ttl);
        grants
    }

    fn serve(&self, call: &BrokerCall) -> Result<Value, BrokerRefusal> {
        let plugin = call.owner.plugin_id.as_str();
        let session = call.session_id.as_str();
        match decode::<RemoteMethod>(call)? {
            RemoteMethod::Status {} => Ok(self.status(plugin, session)),
            RemoteMethod::Pair { label } => self.pair(plugin, session, label.as_deref()),
            RemoteMethod::Clients {} => Ok(self.clients(plugin, session)),
            RemoteMethod::Revoke { grant } => self.revoke(plugin, session, &grant),
        }
    }

    fn status(&self, plugin: &str, session: &str) -> Value {
        let grants = self.lock();
        let now = Instant::now();
        let mut awaiting = 0;
        let mut paired = 0;
        for grant in grants.by_id.values() {
            if grant.plugin_id != plugin || grant.session_id != session {
                continue;
            }
            match grant.state(now) {
                "awaiting_host" => awaiting += 1,
                "paired" => paired += 1,
                _ => {}
            }
        }
        json!({
            "transport": "runtime_http_sse",
            "pairing": "host_presented",
            "public_listener": false,
            "tunnel": false,
            "awaiting_host": awaiting,
            "paired": paired,
            "max_pending": MAX_PENDING_PAIRINGS,
        })
    }

    fn pair(
        &self,
        plugin: &str,
        session: &str,
        label: Option<&str>,
    ) -> Result<Value, BrokerRefusal> {
        let label = clean_label(label).map_err(|message| BrokerRefusal::InvalidInput {
            service: DshService::Remote,
            message,
        })?;
        let mut grants = self.lock();
        let now = Instant::now();
        let pending = grants
            .by_id
            .values()
            .filter(|grant| {
                grant.plugin_id == plugin
                    && grant.session_id == session
                    && grant.state(now) == "awaiting_host"
            })
            .count();
        if pending >= MAX_PENDING_PAIRINGS {
            return Err(BrokerRefusal::InvalidInput {
                service: DshService::Remote,
                message: format!(
                    "this plugin already has {MAX_PENDING_PAIRINGS} pairings awaiting the host in this session; revoke one first"
                ),
            });
        }
        let id = random_capability(GRANT_PREFIX);
        let secret = random_capability(PAIRING_PREFIX);
        let pairing_digest = digest(&secret);
        grants.by_pairing.insert(pairing_digest, id.clone());
        grants.by_id.insert(
            id.clone(),
            Grant {
                plugin_id: plugin.to_string(),
                session_id: session.to_string(),
                label: label.clone(),
                pairing_digest,
                pairing: Some(secret),
                pairing_expires_at: now + self.pairing_ttl,
                client: None,
            },
        );
        Ok(json!({
            "grant": id,
            "label": label,
            "state": "awaiting_host",
            "expires_in_seconds": self.pairing_ttl.as_secs(),
        }))
    }

    fn clients(&self, plugin: &str, session: &str) -> Value {
        let grants = self.lock();
        let now = Instant::now();
        let mut rows: Vec<Value> = grants
            .by_id
            .iter()
            .filter(|(_, grant)| grant.plugin_id == plugin && grant.session_id == session)
            .map(|(id, grant)| {
                let expires_at = grant
                    .client
                    .as_ref()
                    .map_or(grant.pairing_expires_at, |client| client.expires_at);
                json!({
                    "grant": id,
                    "label": grant.label,
                    "client": grant.client.as_ref().map(|client| client.label.clone()),
                    "state": grant.state(now),
                    "expires_in_seconds": expires_at.saturating_duration_since(now).as_secs(),
                })
            })
            .collect();
        rows.sort_by(|a, b| a["grant"].as_str().cmp(&b["grant"].as_str()));
        json!({ "clients": rows })
    }

    fn revoke(&self, plugin: &str, session: &str, grant: &str) -> Result<Value, BrokerRefusal> {
        let mut grants = self.lock();
        let owned = grants
            .by_id
            .get(grant)
            .is_some_and(|row| row.plugin_id == plugin && row.session_id == session);
        if !owned {
            return Err(BrokerRefusal::InvalidInput {
                service: DshService::Remote,
                message: "no such remote grant for this plugin and session".into(),
            });
        }
        grants.remove(grant);
        Ok(json!({ "grant": grant, "revoked": true }))
    }

    #[must_use]
    pub fn awaiting_host(&self) -> Vec<RemotePairing> {
        let grants = self.lock();
        let now = Instant::now();
        let mut rows: Vec<RemotePairing> = grants
            .by_id
            .iter()
            .filter(|(_, grant)| grant.state(now) == "awaiting_host")
            .filter_map(|(id, grant)| {
                Some(RemotePairing {
                    grant: id.clone(),
                    plugin_id: grant.plugin_id.clone(),
                    session_id: grant.session_id.clone(),
                    label: grant.label.clone(),
                    secret: grant.pairing.clone()?,
                    expires_in_seconds: grant
                        .pairing_expires_at
                        .saturating_duration_since(now)
                        .as_secs(),
                })
            })
            .collect();
        rows.sort_by(|a, b| a.grant.cmp(&b.grant));
        rows
    }

    pub fn redeem(&self, secret: &str, client_label: &str) -> Result<RemoteClient, RemoteDenial> {
        let mut grants = self.lock();
        let now = Instant::now();
        let id = grants
            .by_pairing
            .get(&digest(secret))
            .cloned()
            .ok_or(RemoteDenial::Unknown)?;
        let client_ttl = self.client_ttl;
        let grant = grants.by_id.get_mut(&id).ok_or(RemoteDenial::Unknown)?;
        if grant.client.is_some() {
            return Err(RemoteDenial::Replayed);
        }
        if now >= grant.pairing_expires_at {
            return Err(RemoteDenial::Expired);
        }
        let token = random_capability(CLIENT_PREFIX);
        let token_digest = digest(&token);
        grant.pairing = None;
        grant.client = Some(Client {
            token_digest,
            label: clean_label(Some(client_label)).unwrap_or_default(),
            expires_at: now + client_ttl,
        });
        let session_id = grant.session_id.clone();
        grants.by_client.insert(token_digest, id.clone());
        Ok(RemoteClient {
            grant: id,
            session_id,
            token,
            expires_in_seconds: client_ttl.as_secs(),
        })
    }

    pub fn authorize(&self, token: &str, session_id: &str) -> Result<String, RemoteDenial> {
        let grants = self.lock();
        let id = grants
            .by_client
            .get(&digest(token))
            .ok_or(RemoteDenial::Unknown)?;
        let grant = grants.by_id.get(id).ok_or(RemoteDenial::Unknown)?;
        let client = grant.client.as_ref().ok_or(RemoteDenial::Unknown)?;
        if Instant::now() >= client.expires_at {
            return Err(RemoteDenial::Expired);
        }
        if grant.session_id != session_id {
            return Err(RemoteDenial::ForeignSession);
        }
        Ok(id.clone())
    }

    pub fn revoke_session(&self, session_id: &str) -> usize {
        let mut grants = self.lock();
        let ids: Vec<String> = grants
            .by_id
            .iter()
            .filter(|(_, grant)| grant.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            grants.remove(id);
        }
        ids.len()
    }
}

fn clean_label(label: Option<&str>) -> Result<String, String> {
    let label = label.unwrap_or("").trim();
    if label.chars().any(char::is_control) {
        return Err("a remote label may not contain control characters".into());
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "a remote label is at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label.to_string())
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
mod tests {
    use super::*;
    use codewhale_config::OwnerRef;

    fn call(plugin: &str, session: &str, method: &str, input: Value) -> BrokerCall {
        let owner = OwnerRef {
            plugin_id: plugin.into(),
            generation: 1,
            owner_token: "owner".into(),
        };
        BrokerCall {
            context: super::super::dsh_broker::BrokerContext::detached_for_test(
                &owner,
                DshService::Remote,
            ),
            owner,
            session_id: session.into(),
            service: DshService::Remote,
            method: method.into(),
            input,
        }
    }

    #[tokio::test]
    async fn remote_grants_pair_present_redeem_once_and_revoke_bound_to_plugin_and_session() {
        let grants = RemoteGrants::new(PAIRING_TTL, CLIENT_TTL);
        let paired = grants
            .serve(&call("a", "s1", "pair", json!({"label": "phone"})))
            .unwrap();
        let id = paired["grant"].as_str().unwrap().to_string();
        assert_eq!(paired["state"], "awaiting_host");
        assert!(!paired.to_string().contains(PAIRING_PREFIX));

        let shown = grants.awaiting_host();
        assert_eq!(shown.len(), 1);
        assert_eq!(
            (shown[0].plugin_id.as_str(), shown[0].session_id.as_str()),
            ("a", "s1")
        );
        let client = grants.redeem(&shown[0].secret, "pixel").unwrap();
        assert_eq!(client.session_id, "s1");
        assert_eq!(
            grants.redeem(&shown[0].secret, "pixel"),
            Err(RemoteDenial::Replayed)
        );
        assert!(grants.awaiting_host().is_empty());
        assert_eq!(grants.authorize(&client.token, "s1"), Ok(id.clone()));
        assert_eq!(
            grants.authorize(&client.token, "s2"),
            Err(RemoteDenial::ForeignSession)
        );

        let listed = grants
            .serve(&call("a", "s1", "clients", json!({})))
            .unwrap();
        assert_eq!(listed["clients"][0]["state"], "paired");
        assert_eq!(listed["clients"][0]["client"], "pixel");
        assert!(!listed.to_string().contains(CLIENT_PREFIX));
        for (plugin, session) in [("b", "s1"), ("a", "s2")] {
            assert_eq!(
                grants
                    .serve(&call(plugin, session, "clients", json!({})))
                    .unwrap(),
                json!({"clients": []})
            );
            assert!(matches!(
                grants.serve(&call(plugin, session, "revoke", json!({"grant": id}))),
                Err(BrokerRefusal::InvalidInput { .. })
            ));
        }
        assert_eq!(grants.authorize(&client.token, "s1"), Ok(id.clone()));
        assert_eq!(
            grants
                .serve(&call("a", "s1", "revoke", json!({"grant": id})))
                .unwrap(),
            json!({"grant": id, "revoked": true})
        );
        assert_eq!(
            grants.authorize(&client.token, "s1"),
            Err(RemoteDenial::Unknown)
        );
    }

    #[tokio::test]
    async fn remote_grants_refuse_stale_pairings_and_clients_and_cap_pending_pairings() {
        let stale = RemoteGrants::new(Duration::ZERO, Duration::from_secs(60));
        stale.serve(&call("a", "s", "pair", json!({}))).unwrap();
        let secret = {
            let grants = stale.grants.lock().unwrap();
            grants.by_id.values().next().unwrap().pairing.clone()
        };
        assert!(stale.awaiting_host().is_empty());
        assert_eq!(
            stale.redeem(&secret.unwrap_or_default(), "x"),
            Err(RemoteDenial::Unknown)
        );

        let expiring = RemoteGrants::new(Duration::from_secs(60), Duration::ZERO);
        expiring.serve(&call("a", "s", "pair", json!({}))).unwrap();
        let secret = expiring.awaiting_host()[0].secret.clone();
        let client = expiring.redeem(&secret, "x").unwrap();
        assert_eq!(
            expiring.authorize(&client.token, "s"),
            Err(RemoteDenial::Expired)
        );

        let capped = RemoteGrants::new(PAIRING_TTL, CLIENT_TTL);
        for _ in 0..MAX_PENDING_PAIRINGS {
            capped.serve(&call("a", "s", "pair", json!({}))).unwrap();
        }
        assert!(matches!(
            capped.serve(&call("a", "s", "pair", json!({}))),
            Err(BrokerRefusal::InvalidInput { .. })
        ));
        assert!(capped.serve(&call("b", "s", "pair", json!({}))).is_ok());
        assert!(matches!(
            capped.serve(&call("a", "s", "pair", json!({"label": "a\u{7}"}))),
            Err(BrokerRefusal::InvalidInput { .. })
        ));
        assert_eq!(capped.revoke_session("s"), MAX_PENDING_PAIRINGS + 1);
        assert!(capped.awaiting_host().is_empty());
    }

    #[tokio::test]
    async fn remote_methods_decode_strictly_and_refuse_unknown_methods() {
        let grants = RemoteGrants::new(PAIRING_TTL, CLIENT_TTL);
        for method in ["listen", "tunnel", "expose", "anything"] {
            assert!(matches!(
                grants.serve(&call("a", "s", method, json!({}))),
                Err(BrokerRefusal::UnknownMethod {
                    service: DshService::Remote,
                    ..
                })
            ));
        }
        for (method, input) in [
            ("pair", json!({"label": 3})),
            ("pair", json!({"label": "x", "public": true})),
            ("revoke", json!({})),
            ("status", Value::Null),
        ] {
            assert!(
                matches!(
                    grants.serve(&call("a", "s", method, input.clone())),
                    Err(BrokerRefusal::InvalidInput { .. })
                ),
                "{method} {input}"
            );
        }
        let status = grants.serve(&call("a", "s", "status", json!({}))).unwrap();
        assert_eq!(status["public_listener"], false);
        assert_eq!(status["transport"], "runtime_http_sse");
    }
}
