//! App-owned credential storage: `modify` is the only write path.
//!
//! Ported from pi-mono `packages/ai/src/auth/types.ts` (`CredentialStore`) and
//! `packages/ai/src/auth/credential-store.ts` (`InMemoryCredentialStore`),
//! MIT, Copyright (c) 2025 Mario Zechner — full notice in the parent module.
//! Several doc comments below are adapted closely from pi's.
//!
//! pi's rule, kept verbatim in spirit: every mutation is a serialized
//! read-modify-write whose closure sees the current credential, so a refresh
//! and a concurrent login cannot clobber each other.
//!
//! CodeWhale already serializes the xAI OAuth refresh that way, but not
//! through this store: `xai_oauth` holds `with_xai_oauth_lifecycle_lock`
//! across the token-file read, the refresh request, and the write-back, so
//! two concurrent near-expiry observers share one rotated epoch rather than
//! overwriting each other's refresh token. That lock is process- and
//! file-level, not this registry's per-provider mutex, and the xAI flow is
//! **not** rewritten onto [`CredentialStore::modify`] here. This trait is
//! the shape that would put API-key slots and that OAuth flow on one write
//! path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;

use super::{Credential, CredentialKind};

/// Non-secret credential metadata for account/status enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialInfo {
    pub provider_id: String,
    pub(crate) kind: CredentialKind,
}

/// App-owned credential storage, keyed by provider id, one credential per
/// provider.
///
/// `modify` is the only write path, so every mutation is a serialized
/// read-modify-write. Callers that need to refresh a rotated token run the
/// refresh *inside* `modify` so concurrent requests cannot double-refresh.
///
/// Error semantics: `read` yields `Ok(None)` for a missing entry and `Err`
/// on storage failure. `list` is best-effort per slot: a single unreadable
/// entry is omitted so one corrupt slot cannot hide every other stored
/// credential from enumeration (status, `/provider`, logout). `list` fails
/// only when enumeration itself cannot run. `list` must not execute
/// configured API-key commands or open network flows.
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired. Display/status use.
    fn read(&self, provider_id: &str) -> Result<Option<Credential>>;

    /// List stored credential metadata without exposing secrets.
    ///
    /// Per-slot read failures are omitted rather than propagated.
    fn list(&self) -> Result<Vec<CredentialInfo>>;

    /// Serialized write — the only write path. `f` sees the current
    /// credential because correct writes (refresh, login-during-refresh)
    /// depend on it; return the new credential, or `None` to leave the entry
    /// unchanged. Mutual exclusion is per provider id. Yields the post-write
    /// credential.
    ///
    /// Not yet on any production write path: CodeWhale's two credential writes
    /// (`save_api_key_for_identity`, logout) already interleave a config-file
    /// mutation and a compensating rollback between their snapshot and their
    /// store write, so they take [`with_provider_write_lock`] around the whole
    /// sequence instead. Collapsing them onto `modify` would change their
    /// error messages and rollback shape, which is deliberately out of this
    /// change's scope.
    fn modify(
        &self,
        provider_id: &str,
        f: &mut dyn FnMut(Option<Credential>) -> Result<Option<Credential>>,
    ) -> Result<Option<Credential>>;

    /// Remove a credential (logout). Serialized against `modify`.
    ///
    /// Exercised by the store tests but not yet on the production logout path:
    /// the full-wipe logout in `config.rs` still deletes slots directly rather
    /// than through this trait. Routing it here is the remaining half of the
    /// port and is deliberately not folded into this change — logout is
    /// security-sensitive and deserves its own commit and its own tests.
    fn delete(&self, provider_id: &str) -> Result<()>;
}

/// Process-wide per-provider write locks.
///
/// pi serializes through a per-provider promise chain; the Rust equivalent is
/// a registry of per-id mutexes. This is process-local: it does not serialize
/// against another `codewhale` process writing the same slot, which is a real
/// remaining gap and is called out in the module docs.
fn provider_lock(provider_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        guard
            .entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// Run `body` holding this provider's write lock.
pub fn with_provider_write_lock<T>(provider_id: &str, body: impl FnOnce() -> T) -> T {
    let lock = provider_lock(provider_id);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    body()
}

/// Run `body` holding every listed provider's write lock.
///
/// Locks are acquired in sorted, deduplicated order so a full logout that
/// covers every slot cannot deadlock with a per-provider save (which holds
/// only one). Callers that also take the xAI OAuth lifecycle lock must take
/// that lock first, matching the documented xAI-then-config order.
pub fn with_provider_write_locks<T>(
    provider_ids: impl IntoIterator<Item = impl AsRef<str>>,
    body: impl FnOnce() -> T,
) -> T {
    let mut ids: Vec<String> = provider_ids
        .into_iter()
        .map(|id| id.as_ref().to_string())
        .collect();
    ids.sort();
    ids.dedup();
    let locks: Vec<Arc<Mutex<()>>> = ids.iter().map(|id| provider_lock(id)).collect();
    let _guards: Vec<_> = locks
        .iter()
        .map(|lock| {
            lock.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .collect();
    body()
}

/// Default in-memory store. Real stores are injected; this one backs tests and
/// keeps the trait honest about its own contract — including the serialization
/// guarantee, which is only observable through `modify`.
#[derive(Debug, Default)]
pub(crate) struct InMemoryCredentialStore {
    entries: Mutex<HashMap<String, Credential>>,
}

impl InMemoryCredentialStore {
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider_id)
            .cloned())
    }

    fn list(&self) -> Result<Vec<CredentialInfo>> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut infos: Vec<CredentialInfo> = entries
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                kind: credential.kind(),
            })
            .collect();
        infos.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        Ok(infos)
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut dyn FnMut(Option<Credential>) -> Result<Option<Credential>>,
    ) -> Result<Option<Credential>> {
        with_provider_write_lock(provider_id, || {
            let current = self.read(provider_id)?;
            let next = f(current.clone())?;
            match next {
                Some(credential) => {
                    self.entries
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(provider_id.to_string(), credential.clone());
                    Ok(Some(credential))
                }
                None => Ok(current),
            }
        })
    }

    fn delete(&self, provider_id: &str) -> Result<()> {
        with_provider_write_lock(provider_id, || {
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(provider_id);
            Ok(())
        })
    }
}

/// Adapter over CodeWhale's existing durable secret store.
///
/// This changes no on-disk format: it reads and writes exactly the slots
/// `codewhale_secrets::Secrets` already owns. Its value is that every write now
/// goes through `modify` under the provider's lock, so a save racing a rotate
/// no longer interleaves.
pub struct SecretStoreCredentials {
    secrets: codewhale_secrets::Secrets,
    /// Slots to probe for `list`. The backing keyring exposes no key
    /// enumeration, so the caller supplies the known slot names.
    known_slots: Vec<String>,
}

impl SecretStoreCredentials {
    pub fn new(secrets: codewhale_secrets::Secrets, known_slots: Vec<String>) -> Self {
        Self {
            secrets,
            known_slots,
        }
    }
}

impl std::fmt::Debug for SecretStoreCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStoreCredentials")
            .field("backend", &self.secrets.backend_name())
            .field("known_slots", &self.known_slots.len())
            .finish()
    }
}

impl CredentialStore for SecretStoreCredentials {
    fn read(&self, provider_id: &str) -> Result<Option<Credential>> {
        Ok(self
            .secrets
            .get(provider_id)?
            .filter(|value| !value.trim().is_empty())
            .map(|key| Credential::ApiKey { key }))
    }

    fn list(&self) -> Result<Vec<CredentialInfo>> {
        let mut infos = Vec::new();
        for slot in &self.known_slots {
            match self.read(slot) {
                Ok(Some(_)) => infos.push(CredentialInfo {
                    provider_id: slot.clone(),
                    kind: CredentialKind::ApiKey,
                }),
                Ok(None) => {}
                Err(error) => {
                    // Deliberate: skip the bad slot and keep going. Propagating
                    // `read`'s error used to fail the whole enumeration, so one
                    // unreadable entry hid every other credential from
                    // `/provider` and left logout unable to delete the rest.
                    tracing::warn!(
                        slot,
                        error = %error,
                        "skipping unreadable credential slot during enumeration"
                    );
                }
            }
        }
        Ok(infos)
    }

    fn modify(
        &self,
        provider_id: &str,
        f: &mut dyn FnMut(Option<Credential>) -> Result<Option<Credential>>,
    ) -> Result<Option<Credential>> {
        with_provider_write_lock(provider_id, || {
            let current = self.read(provider_id)?;
            let next = f(current.clone())?;
            match next {
                Some(credential) => {
                    self.secrets.set(provider_id, credential.expose_secret())?;
                    Ok(Some(credential))
                }
                None => Ok(current),
            }
        })
    }

    fn delete(&self, provider_id: &str) -> Result<()> {
        with_provider_write_lock(provider_id, || {
            self.secrets.delete(provider_id)?;
            Ok(())
        })
    }
}
