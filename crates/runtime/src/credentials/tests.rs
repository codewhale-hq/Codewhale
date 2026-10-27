//! Coverage for the ported credential store contract.

use super::context::MapAuthContext;
use super::store::{CredentialStore, InMemoryCredentialStore, SecretStoreCredentials};
use super::{AuthContext, Credential, CredentialKind};
use codewhale_secrets::{InMemoryKeyringStore, KeyringStore, Secrets, SecretsError};
use std::sync::Arc;

#[test]
fn credential_debug_never_prints_secret_material() {
    let api_key = Credential::ApiKey {
        key: "sk-super-secret-value".to_string(),
    };
    let rendered = format!("{api_key:?}");
    assert!(!rendered.contains("sk-super-secret-value"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");

    let oauth = Credential::OAuth {
        access: "oauth-secret-token".to_string(),
        expires_at_unix_secs: Some(42),
    };
    let rendered = format!("{oauth:?}");
    assert!(!rendered.contains("oauth-secret-token"), "{rendered}");
    assert!(rendered.contains("42"), "{rendered}");
}

#[test]
fn modify_sees_the_current_credential_and_is_the_write_path() {
    let store = InMemoryCredentialStore::new();
    assert_eq!(store.read("deepseek").unwrap(), None);

    let written = store
        .modify("deepseek", &mut |current| {
            assert_eq!(current, None, "first write must observe an empty slot");
            Ok(Some(Credential::ApiKey {
                key: "first".to_string(),
            }))
        })
        .unwrap();
    assert_eq!(
        written.as_ref().map(Credential::kind),
        Some(CredentialKind::ApiKey)
    );

    let mut observed = None;
    store
        .modify("deepseek", &mut |current| {
            observed = current.clone();
            Ok(Some(Credential::ApiKey {
                key: "second".to_string(),
            }))
        })
        .unwrap();
    assert_eq!(
        observed.as_ref().map(Credential::expose_secret),
        Some("first"),
        "the closure must see the credential it is replacing"
    );
    assert_eq!(
        store
            .read("deepseek")
            .unwrap()
            .as_ref()
            .map(Credential::expose_secret),
        Some("second")
    );
}

#[test]
fn modify_returning_none_leaves_the_entry_unchanged() {
    let store = InMemoryCredentialStore::new();
    store
        .modify("xai", &mut |_| {
            Ok(Some(Credential::OAuth {
                access: "token".to_string(),
                expires_at_unix_secs: Some(100),
            }))
        })
        .unwrap();
    let kept = store.modify("xai", &mut |_| Ok(None)).unwrap();
    assert_eq!(
        kept.as_ref().map(Credential::expose_secret),
        Some("token"),
        "a no-op modify must not clear the slot"
    );
}

/// pi's whole reason for making `modify` the only write path: a refresh that
/// runs inside it cannot be interleaved with a concurrent one. Two threads
/// both observing the same near-expiry token must produce exactly one refresh.
#[test]
fn concurrent_modify_on_one_provider_refreshes_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify("concurrent-provider", &mut |_| {
            Ok(Some(Credential::OAuth {
                access: "stale".to_string(),
                expires_at_unix_secs: Some(0),
            }))
        })
        .unwrap();

    let refreshes = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            let refreshes = Arc::clone(&refreshes);
            std::thread::spawn(move || {
                store
                    .modify("concurrent-provider", &mut |current| {
                        // Double-checked under the lock, exactly as pi does.
                        let needs_refresh = matches!(
                            current,
                            Some(Credential::OAuth {
                                expires_at_unix_secs: Some(0),
                                ..
                            })
                        );
                        if !needs_refresh {
                            return Ok(None);
                        }
                        refreshes.fetch_add(1, Ordering::SeqCst);
                        Ok(Some(Credential::OAuth {
                            access: "rotated".to_string(),
                            expires_at_unix_secs: Some(9_999),
                        }))
                    })
                    .unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "modify must serialize per provider so only one thread refreshes"
    );
    assert_eq!(
        store
            .read("concurrent-provider")
            .unwrap()
            .as_ref()
            .map(Credential::expose_secret),
        Some("rotated")
    );
}

/// The store module used to claim `xai_oauth` refresh was unlocked. That is
/// false: refresh holds `with_xai_oauth_lifecycle_lock` (see
/// `oauth::concurrent_refreshes_share_one_rotated_epoch`). A doc comment
/// that misdescribes neighbouring code is how the next person gets misled.
#[test]
fn store_docs_name_the_xai_lifecycle_lock_instead_of_an_unlocked_refresh() {
    let source = include_str!("store.rs");
    assert!(
        source.contains("with_xai_oauth_lifecycle_lock"),
        "store.rs must name the lock xAI OAuth refresh actually holds"
    );
    assert!(
        !source.contains("with no lock held"),
        "store.rs still claims xAI OAuth refresh writes back unlocked"
    );
}

#[test]
fn list_reports_metadata_without_secrets() {
    let store = InMemoryCredentialStore::new();
    store
        .modify("alpha", &mut |_| {
            Ok(Some(Credential::ApiKey {
                key: "alpha-secret".to_string(),
            }))
        })
        .unwrap();
    store
        .modify("beta", &mut |_| {
            Ok(Some(Credential::OAuth {
                access: "beta-secret".to_string(),
                expires_at_unix_secs: None,
            }))
        })
        .unwrap();

    let listed = store.list().unwrap();
    let rendered = format!("{listed:?}");
    assert!(!rendered.contains("alpha-secret"), "{rendered}");
    assert!(!rendered.contains("beta-secret"), "{rendered}");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].provider_id, "alpha");
    assert_eq!(listed[0].kind, CredentialKind::ApiKey);
    assert_eq!(listed[1].provider_id, "beta");
    assert_eq!(listed[1].kind, CredentialKind::OAuth);
}

/// One slot whose backend `get` fails must not hide the others. The adapter
/// that replaced the old `.ok().flatten()` probe loop used `read(slot)?`,
/// which turned a single corrupt entry into an empty `/provider` list.
struct UnreadableSlotStore {
    inner: InMemoryKeyringStore,
    unreadable: &'static str,
}

impl KeyringStore for UnreadableSlotStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretsError> {
        if key == self.unreadable {
            return Err(SecretsError::Keyring(format!("slot {key} is unreadable")));
        }
        self.inner.get(key)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretsError> {
        self.inner.set(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), SecretsError> {
        self.inner.delete(key)
    }

    fn backend_name(&self) -> &'static str {
        "unreadable-slot (test)"
    }
}

#[test]
fn list_skips_an_unreadable_slot_instead_of_failing_the_enumeration() {
    let backend = UnreadableSlotStore {
        inner: InMemoryKeyringStore::new(),
        unreadable: "deepseek",
    };
    backend
        .set("deepseek", "deepseek-secret")
        .expect("seed unreadable slot");
    backend
        .set("openrouter", "openrouter-secret")
        .expect("seed readable slot");
    let store = SecretStoreCredentials::new(
        Secrets::new(Arc::new(backend)),
        vec![
            "deepseek".to_string(),
            "openrouter".to_string(),
            "xai".to_string(),
        ],
    );

    assert!(
        store.read("deepseek").is_err(),
        "the bad slot must still fail when asked for by name"
    );
    let listed = store
        .list()
        .expect("one unreadable slot must not fail the whole list");
    assert_eq!(
        listed
            .iter()
            .map(|info| info.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["openrouter"],
        "the readable slot must still appear; the empty slot and the bad slot must not"
    );
}

#[test]
fn delete_removes_the_entry() {
    let store = InMemoryCredentialStore::new();
    store
        .modify("gamma", &mut |_| {
            Ok(Some(Credential::ApiKey {
                key: "value".to_string(),
            }))
        })
        .unwrap();
    store.delete("gamma").unwrap();
    assert_eq!(store.read("gamma").unwrap(), None);
}

#[test]
fn map_auth_context_answers_without_touching_the_process() {
    let ctx = MapAuthContext::new()
        .with_env("DEEPSEEK_API_KEY", "value")
        .with_env("BLANK_KEY", "   ");
    assert_eq!(ctx.env("DEEPSEEK_API_KEY").as_deref(), Some("value"));
    assert_eq!(
        ctx.env("BLANK_KEY"),
        None,
        "blank exports are not credentials"
    );
    assert_eq!(ctx.env("UNSET_KEY"), None);
}
