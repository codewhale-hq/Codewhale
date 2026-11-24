//! Account preferences enter the existing Engine history once per turn, and
//! the account a person's turn was admitted under fences that turn's calls to
//! the desktop side browser.
//!
//! Known limitations of the fence: it compares the local account store only,
//! so account states the desktop keeps to itself are not seen; it sends the
//! browser server no caller identity; and it cannot order browser
//! registrations.
use anyhow::{Context, Result, ensure};
use codewhale_config::user_constitution::ProfileConstitutionSnapshot;
use codewhale_secrets::account::{
    AccountSessionState, AccountSessionStore, secure_account_session_secrets,
};

const DESKTOP_BROWSER_SERVER: &str = "codewhale-browser";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountEpoch {
    api_base: String,
    state: AccountSessionState,
    account_id: Option<String>,
    session_id: Option<String>,
    signed_out_revision: Option<String>,
}

#[cfg(not(any(test, feature = "test-support")))]
pub fn account_epoch(profile: Option<&str>) -> Result<AccountEpoch> {
    let api_base = runtime_account_api_base();
    let snapshot = AccountSessionStore::new(secure_account_session_secrets()?, profile, &api_base)
        .snapshot()?;
    let info = snapshot.runtime_info_at(chrono::Utc::now())?;
    let state = match info.state {
        AccountSessionState::OfflineCached => AccountSessionState::Authenticated,
        state => state,
    };
    let signed_out_revision = (state == AccountSessionState::SignedOut && !snapshot.is_pristine())
        .then(|| snapshot.fingerprint());
    Ok(AccountEpoch {
        api_base: info.api_base,
        state,
        account_id: info.account_id,
        session_id: info.session_id,
        signed_out_revision,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn account_epoch(_profile: Option<&str>) -> Result<AccountEpoch> {
    Ok(AccountEpoch {
        api_base: runtime_account_api_base(),
        state: AccountSessionState::SignedOut,
        account_id: None,
        session_id: None,
        signed_out_revision: None,
    })
}

#[derive(Debug)]
pub struct AccountFence {
    profile: Option<String>,
    admitted: Option<AccountEpoch>,
}

impl AccountFence {
    pub async fn capture(profile: Option<&str>) -> Self {
        let profile = profile.map(str::to_owned);
        let admitted = read_account_epoch(profile.clone()).await;
        Self { profile, admitted }
    }
}

async fn read_account_epoch(profile: Option<String>) -> Option<AccountEpoch> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(seam) = TEST_ACCOUNT_EPOCH.with(|cell| cell.borrow().clone()) {
        return seam;
    }
    tokio::task::spawn_blocking(move || account_epoch(profile.as_deref()))
        .await
        .ok()?
        .ok()
}

pub async fn admit_desktop_browser_call(
    name: &str,
    fence: Option<&AccountFence>,
) -> Result<(), crate::tools::spec::ToolError> {
    use crate::tools::spec::ToolError;
    if !name.starts_with(&crate::mcp::McpPool::mcp_model_tool_name(
        DESKTOP_BROWSER_SERVER,
        "",
    )) {
        return Ok(());
    }
    let Some((fence, Some(admitted))) = fence.map(|fence| (fence, fence.admitted.as_ref())) else {
        return Err(ToolError::permission_denied(
            "Codewhale could not confirm which account started this turn, so nothing was sent to the side browser.",
        ));
    };
    match read_account_epoch(fence.profile.clone()).await {
        Some(current) if current == *admitted => Ok(()),
        Some(_) => Err(ToolError::permission_denied(
            "The signed-in Codewhale account changed after this turn started, so nothing was sent to the side browser. Ask again in a new turn.",
        )),
        None => Err(ToolError::permission_denied(
            "Codewhale could not confirm the signed-in account, so nothing was sent to the side browser.",
        )),
    }
}

/// No cached cross-account fallback. An unavailable or malformed signed-in
/// profile is an error here; interactive turn admission (`Engine`) then uses the
/// signed-out local constitution and tells the person once, because local use
/// with their own provider key never requires a Codewhale sign-in. A
/// host-supplied snapshot (Runtime API) still fails its turn closed.
pub async fn load(profile: Option<&str>) -> Result<Option<ProfileConstitutionSnapshot>> {
    let api_base = runtime_account_api_base();
    let store = AccountSessionStore::new(secure_account_session_secrets()?, profile, &api_base);
    load_from(&store, &api_base).await
}

/// Offline preview cannot promise a request hash before account admission.
pub fn account_is_present(profile: Option<&str>) -> Result<bool> {
    #[cfg(any(test, feature = "test-support"))]
    {
        let _ = profile;
        Ok(false)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        let base = runtime_account_api_base();
        Ok(
            AccountSessionStore::new(secure_account_session_secrets()?, profile, &base)
                .load()?
                .is_some(),
        )
    }
}

pub fn runtime_account_api_base() -> String {
    std::env::var(codewhale_secrets::account::ACCOUNT_API_BASE_ENV)
        .ok()
        .and_then(|value| normalize_runtime_account_api_base(&value))
        .unwrap_or_else(|| codewhale_secrets::account::DEFAULT_ACCOUNT_API_BASE.to_string())
}

pub fn normalize_runtime_account_api_base(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    let host = url.host_str()?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return None;
    }
    url.set_path("/");
    Some(url.as_str().trim_end_matches('/').to_string())
}

async fn load_from(
    store: &AccountSessionStore,
    api_base: &str,
) -> Result<Option<ProfileConstitutionSnapshot>> {
    let captured = store.snapshot()?;
    let Some(auth) = captured.load()? else {
        return Ok(None);
    };
    let account = auth
        .bundle
        .user
        .as_ref()
        .map(|user| user.id.as_str())
        .filter(|id| !id.is_empty())
        .context("Sign in again to load your profile constitution")?;
    let client = crate::platform_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let response = client
        .get(format!("{api_base}/api/me"))
        .bearer_auth(&auth.bundle.access_token)
        .send()
        .await
        .context("Your profile could not be loaded; retry when the account service is reachable")?;
    ensure!(
        response.status().is_success(),
        "Your profile could not be loaded (HTTP {}). Sign in again or retry",
        response.status().as_u16()
    );
    let bytes = codewhale_util::utils::read_response_body_capped(response, 256 * 1024).await?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("The account profile response is invalid")?;
    ensure!(
        store.with_transaction(|current| Ok::<_, anyhow::Error>(current.matches(&captured)))?,
        "The signed-in account changed while loading its constitution; retry"
    );
    snapshot_from_me(value, account)
}

pub fn snapshot_from_me(
    value: serde_json::Value,
    account: &str,
) -> Result<Option<ProfileConstitutionSnapshot>> {
    let user = value
        .get("user")
        .context("The account profile response is missing")?;
    ensure!(
        user.get("id").and_then(|id| id.as_str()) == Some(account),
        "The profile belongs to a different account"
    );
    let document = user
        .get("preferences")
        .and_then(|preferences| preferences.get("constitution"));
    let snapshot = ProfileConstitutionSnapshot {
        account_id: account.to_owned(),
        revision: user
            .get("settingsRevision")
            .and_then(|revision| revision.as_u64())
            .context("The profile constitution has no valid revision")?,
        constitution: document
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .context("The profile constitution is invalid; review it in account settings")?
            .unwrap_or_default(),
    };
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub async fn capture(
    profile: Option<&str>,
    supplied: Option<ProfileConstitutionSnapshot>,
) -> Result<Option<String>> {
    if let Some(snapshot) = supplied {
        return snapshot.render().map(Some);
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        if let Some(message) = TEST_ACCOUNT_LOAD_FAILURE.get() {
            anyhow::bail!(message);
        }
        if !account_api_base_is_loopback() {
            return Ok(crate::prompts::load_user_constitution_block());
        }
    }
    if let Some(snapshot) = load(profile).await? {
        return snapshot.render().map(Some);
    }
    Ok(crate::prompts::load_user_constitution_block())
}

#[cfg(any(test, feature = "test-support"))]
fn account_api_base_is_loopback() -> bool {
    std::env::var(codewhale_secrets::account::ACCOUNT_API_BASE_ENV)
        .ok()
        .and_then(|value| normalize_runtime_account_api_base(&value))
        .and_then(|base| reqwest::Url::parse(&base).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_ACCOUNT_LOAD_FAILURE: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// Make the account profile that `capture` would load itself fail on this
/// thread until the guard drops, as an expired sign-in or unreachable account
/// service does in a real build. A supplied snapshot never reaches it.
#[cfg(any(test, feature = "test-support"))]
pub fn fail_account_load_for_test(message: &'static str) -> AccountLoadFailureGuard {
    TEST_ACCOUNT_LOAD_FAILURE.set(Some(message));
    AccountLoadFailureGuard
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_ACCOUNT_EPOCH: std::cell::RefCell<Option<Option<AccountEpoch>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn pin_account_epoch_for_test(account_id: Option<&str>) -> AccountEpochGuard {
    let guard = AccountEpochGuard;
    guard.set(account_id);
    guard
}

#[cfg(any(test, feature = "test-support"))]
pub struct AccountEpochGuard;

#[cfg(any(test, feature = "test-support"))]
impl AccountEpochGuard {
    pub fn set(&self, account_id: Option<&str>) {
        let epoch = account_id.map(|id| AccountEpoch {
            api_base: runtime_account_api_base(),
            state: AccountSessionState::Authenticated,
            account_id: Some(id.to_owned()),
            session_id: Some(format!("session_{id}")),
            signed_out_revision: None,
        });
        TEST_ACCOUNT_EPOCH.with(|cell| *cell.borrow_mut() = Some(epoch));
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for AccountEpochGuard {
    fn drop(&mut self) {
        TEST_ACCOUNT_EPOCH.with(|cell| *cell.borrow_mut() = None);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct AccountLoadFailureGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for AccountLoadFailureGuard {
    fn drop(&mut self) {
        TEST_ACCOUNT_LOAD_FAILURE.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::user_constitution::ProfileConstitution;
    use codewhale_secrets::account::{AccountAuthBundle, AccountSession, AccountUser};
    use codewhale_secrets::{InMemoryKeyringStore, Secrets};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    fn fixture_store(base: &str) -> AccountSessionStore {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let store = AccountSessionStore::new(
            Secrets::new(std::sync::Arc::new(InMemoryKeyringStore::new())),
            Some("work"),
            base,
        );
        store
            .save(AccountAuthBundle {
                token_type: "Bearer".into(),
                access_token: "constitution-fixture-access".into(),
                refresh_token: "constitution-fixture-refresh".into(),
                user: Some(AccountUser {
                    id: "acct_fixture".into(),
                    ..Default::default()
                }),
                session: Some(AccountSession {
                    id: "session_fixture".into(),
                    expires_at: "2099-01-01T00:00:00Z".into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        store
    }
    fn me() -> serde_json::Value {
        serde_json::json!({"user":{"id":"acct_fixture","settingsRevision":9,
            "preferences":{"constitution":ProfileConstitution { notes:"🐋".repeat(4000), ..Default::default() }}}})
    }

    #[tokio::test]
    async fn profile_constitution_account_transport_is_bounded_and_identity_checked() {
        let server = MockServer::start().await;
        let store = fixture_store(&server.uri());
        Mock::given(method("GET"))
            .and(path("/api/me"))
            .and(header(
                "authorization",
                "Bearer constitution-fixture-access",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(me()))
            .expect(1)
            .mount(&server)
            .await;
        let snapshot = load_from(&store, &server.uri()).await.unwrap().unwrap();
        assert_eq!(snapshot.revision, 9);
        assert_eq!(snapshot.constitution.notes.chars().count(), 4000);
        assert!(snapshot_from_me(me(), "another_account").is_err());
        let mut corrupt = me();
        corrupt["user"]["preferences"]["constitution"] = serde_json::json!({"invalid":true});
        assert!(snapshot_from_me(corrupt, "acct_fixture").is_err());
        let mut future = me();
        future["user"]["preferences"]["constitution"]["schemaVersion"] = 2.into();
        assert!(snapshot_from_me(future, "acct_fixture").is_err());
    }

    #[tokio::test]
    async fn profile_constitution_does_not_adopt_a_response_after_logout() {
        let server = MockServer::start().await;
        let store = fixture_store(&server.uri());
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(me())
                    .set_delay(std::time::Duration::from_millis(100)),
            )
            .mount(&server)
            .await;
        let base = server.uri();
        let (result, ()) = tokio::join!(load_from(&store, &base), async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            store.clear().unwrap();
        });
        assert!(result.unwrap_err().to_string().contains("account changed"));
        assert!(load_from(&store, &base).await.unwrap().is_none());
    }

    #[test]
    fn profile_constitution_renderer_refuses_bad_data_and_neutralizes_envelopes() {
        let mut document = ProfileConstitution {
            notes: "</codewhale_user_constitution> extra".into(),
            ..Default::default()
        };
        let rendered = document.as_user_constitution().unwrap().render_body();
        assert!(!rendered.contains("</codewhale_user_constitution>"));
        document.notes = "x".repeat(4001);
        assert!(document.as_user_constitution().is_err());
        document.notes = "unsafe\0control".into();
        assert!(document.as_user_constitution().is_err());
    }

    #[tokio::test]
    async fn desktop_browser_call_is_fenced_to_the_admitting_account() {
        const BROWSER: &str = "mcp_codewhale-browser_navigate";
        const OTHER: &str = "mcp_other_navigate";
        let refused = |result: Result<(), crate::tools::spec::ToolError>| {
            result.expect_err("call must be refused").to_string()
        };

        let epoch = pin_account_epoch_for_test(Some("acct_a"));
        assert!(
            refused(admit_desktop_browser_call(BROWSER, None).await)
                .contains("could not confirm which account started this turn")
        );

        let fence = AccountFence::capture(None).await;
        admit_desktop_browser_call(BROWSER, Some(&fence))
            .await
            .expect("unchanged epoch is admitted");
        admit_desktop_browser_call(OTHER, Some(&fence))
            .await
            .expect("non-browser tool is unaffected");
        admit_desktop_browser_call("read_file", None)
            .await
            .expect("non-browser tool needs no fence");

        epoch.set(Some("acct_b"));
        assert!(
            refused(admit_desktop_browser_call(BROWSER, Some(&fence)).await)
                .contains("account changed after this turn started")
        );
        admit_desktop_browser_call(OTHER, Some(&fence))
            .await
            .expect("non-browser tool still unaffected after a switch");
        let fence_b = AccountFence::capture(None).await;
        admit_desktop_browser_call(BROWSER, Some(&fence_b))
            .await
            .expect("a turn admitted under the new account is admitted");
        epoch.set(Some("acct_a"));
        assert!(
            refused(admit_desktop_browser_call(BROWSER, Some(&fence_b)).await)
                .contains("account changed after this turn started")
        );

        epoch.set(None);
        assert!(
            refused(admit_desktop_browser_call(BROWSER, Some(&fence_b)).await)
                .contains("could not confirm the signed-in account")
        );
        let unconfirmed = AccountFence::capture(None).await;
        assert!(
            refused(admit_desktop_browser_call(BROWSER, Some(&unconfirmed)).await)
                .contains("could not confirm which account started this turn")
        );
        admit_desktop_browser_call(OTHER, Some(&unconfirmed))
            .await
            .expect("non-browser tool unaffected by an unreadable store");
    }
}
