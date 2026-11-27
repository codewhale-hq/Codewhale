//! Account preferences enter the existing Engine history once per turn, and
//! the account a person's turn was admitted under fences that turn's calls to
//! the desktop side browser.
//!
//! Known limitations of the fence: it compares the local account store only,
//! so account states the desktop keeps to itself are not seen; it sends the
//! browser server no caller identity; it cannot order browser registrations;
//! and a turn awaits its capture, an off-runtime read of the local account
//! store, with no deadline and no Stop, before the bounded account profile
//! wait.
use anyhow::{Context, Result, ensure};
use codewhale_config::user_constitution::ProfileConstitutionSnapshot;
use codewhale_secrets::account::{
    AccountSessionSnapshot, AccountSessionState, AccountSessionStore,
    secure_account_session_secrets,
};

const DESKTOP_BROWSER_SERVER: &str = "codewhale-browser";
const ACCOUNT_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const TURN_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountEpoch {
    api_base: String,
    state: AccountSessionState,
    account_id: Option<String>,
    session_id: Option<String>,
    signed_out_revision: Option<String>,
}

pub fn account_epoch(profile: Option<&str>) -> Result<AccountEpoch> {
    let api_base = runtime_account_api_base();
    #[cfg(any(test, feature = "test-support"))]
    if !account_api_base_is_loopback() {
        return Ok(AccountEpoch {
            api_base,
            state: AccountSessionState::SignedOut,
            account_id: None,
            session_id: None,
            signed_out_revision: None,
        });
    }
    epoch_of(
        &AccountSessionStore::new(secure_account_session_secrets()?, profile, &api_base)
            .snapshot()?,
    )
}

fn epoch_of(snapshot: &AccountSessionSnapshot) -> Result<AccountEpoch> {
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
    #[cfg(any(test, feature = "test-support"))]
    if let Some((store, ..)) = TEST_ACCOUNT_SOURCE.with(|cell| cell.borrow().clone()) {
        return store
            .snapshot()
            .ok()
            .and_then(|snapshot| epoch_of(&snapshot).ok());
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
    load_for(profile, None).await
}

async fn load_for(
    profile: Option<&str>,
    expected: Option<&AccountEpoch>,
) -> Result<Option<ProfileConstitutionSnapshot>> {
    let api_base = runtime_account_api_base();
    let profile = profile.map(str::to_owned);
    let base = api_base.clone();
    let store = tokio::task::spawn_blocking(move || {
        anyhow::Ok(AccountSessionStore::new(
            secure_account_session_secrets()?,
            profile.as_deref(),
            &base,
        ))
    })
    .await??;
    load_from(&store, &api_base, expected).await
}

async fn off_runtime<T: Send + 'static>(
    store: &AccountSessionStore,
    operation: impl FnOnce(&AccountSessionStore) -> Result<T> + Send + 'static,
) -> Result<T> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || operation(&store)).await?
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
    expected: Option<&AccountEpoch>,
) -> Result<Option<ProfileConstitutionSnapshot>> {
    let captured = off_runtime(store, |store| Ok(store.snapshot()?)).await?;
    if let Some(expected) = expected {
        ensure!(
            epoch_of(&captured)? == *expected,
            "The signed-in account changed while loading its constitution; retry"
        );
    }
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
    let client = tokio::task::spawn_blocking(|| {
        crate::platform_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(ACCOUNT_LOAD_TIMEOUT)
            .build()
    })
    .await??;
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
    let unchanged = off_runtime(store, move |store| {
        store.with_transaction(|current| Ok::<_, anyhow::Error>(current.matches(&captured)))
    })
    .await?;
    ensure!(
        unchanged,
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

type Loaded = Result<Option<ProfileConstitutionSnapshot>, String>;

pub enum AccountRead {
    Ready(Result<Option<String>, String>),
    Pending {
        last: Option<String>,
        signed_in: bool,
    },
}

#[derive(Default)]
pub struct AccountConstitution {
    last: Option<(AccountEpoch, ProfileConstitutionSnapshot)>,
    failure: Option<(AccountEpoch, String)>,
    pending: Option<(AccountEpoch, tokio::task::JoinHandle<Loaded>)>,
}

impl AccountConstitution {
    pub async fn capture(
        &mut self,
        profile: Option<&str>,
        fence: Option<&AccountFence>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> AccountRead {
        let key = fence.and_then(|fence| fence.admitted.clone());
        let expected = key.clone();
        #[cfg(any(test, feature = "test-support"))]
        {
            if let Some(message) = TEST_ACCOUNT_LOAD_FAILURE.get() {
                return AccountRead::Ready(Err(message.to_owned()));
            }
            if let Some((store, api_base, wait)) =
                TEST_ACCOUNT_SOURCE.with(|cell| cell.borrow().clone())
            {
                return self
                    .capture_with(key, cancel, wait, async move {
                        load_from(&store, &api_base, expected.as_ref())
                            .await
                            .map_err(|error| error.to_string())
                    })
                    .await;
            }
            if !account_api_base_is_loopback() {
                return AccountRead::Ready(Ok(crate::prompts::load_user_constitution_block()));
            }
        }
        let profile = profile.map(str::to_owned);
        self.capture_with(key, cancel, TURN_WAIT, async move {
            load_for(profile.as_deref(), expected.as_ref())
                .await
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn capture_with<F>(
        &mut self,
        key: Option<AccountEpoch>,
        cancel: &tokio_util::sync::CancellationToken,
        wait: std::time::Duration,
        read: F,
    ) -> AccountRead
    where
        F: std::future::Future<Output = Loaded> + Send + 'static,
    {
        let current = |epoch: &AccountEpoch| Some(epoch) == key.as_ref();
        if !self.last.as_ref().is_some_and(|(epoch, _)| current(epoch)) {
            self.last = None;
        }
        if !self
            .failure
            .as_ref()
            .is_some_and(|(epoch, _)| current(epoch))
        {
            self.failure = None;
        }
        let in_flight = match self.pending.take() {
            Some((epoch, handle)) if current(&epoch) && !handle.is_finished() => Some(handle),
            Some((epoch, handle)) if current(&epoch) => {
                self.adopt(
                    epoch,
                    handle.await.unwrap_or_else(|error| Err(error.to_string())),
                );
                None
            }
            Some((_, handle)) => {
                handle.abort();
                None
            }
            None => None,
        };
        let mut handle = in_flight.unwrap_or_else(|| tokio::spawn(read));
        let wait = if self.failure.is_some() {
            std::time::Duration::ZERO
        } else {
            wait
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            joined = &mut handle => {
                let loaded = joined.unwrap_or_else(|error| Err(error.to_string()));
                if let Some(epoch) = key {
                    self.adopt(epoch, loaded.clone());
                }
                return AccountRead::Ready(rendered(loaded));
            }
            () = tokio::time::sleep(wait) => {}
        }
        let signed_in = key
            .as_ref()
            .is_some_and(|epoch| epoch.state != AccountSessionState::SignedOut);
        match key {
            Some(epoch) => self.pending = Some((epoch, handle)),
            None => handle.abort(),
        }
        if let Some((_, error)) = &self.failure {
            return AccountRead::Ready(Err(error.clone()));
        }
        AccountRead::Pending {
            last: self
                .last
                .as_ref()
                .and_then(|(_, snapshot)| snapshot.render().ok()),
            signed_in,
        }
    }

    #[cfg(test)]
    pub(crate) fn read_in_flight(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(_, handle)| !handle.is_finished())
    }

    fn adopt(&mut self, epoch: AccountEpoch, loaded: Loaded) {
        match loaded {
            Ok(snapshot) => {
                self.failure = None;
                self.last = snapshot.map(|snapshot| (epoch, snapshot));
            }
            Err(error) => self.failure = Some((epoch, error)),
        }
    }
}

impl Drop for AccountConstitution {
    fn drop(&mut self) {
        if let Some((_, handle)) = self.pending.take() {
            handle.abort();
        }
    }
}

fn rendered(loaded: Loaded) -> Result<Option<String>, String> {
    match loaded? {
        Some(snapshot) => snapshot
            .render()
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(crate::prompts::load_user_constitution_block()),
    }
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

/// Make the account profile that `AccountConstitution::capture` would load itself fail on this
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
thread_local! {
    static TEST_ACCOUNT_SOURCE: std::cell::RefCell<
        Option<(AccountSessionStore, String, std::time::Duration)>,
    > = const { std::cell::RefCell::new(None) };
}

/// Serve this thread's turns the signed-in account in `store`, read from
/// `api_base` through the real profile load, waiting at most `wait` per turn.
#[cfg(any(test, feature = "test-support"))]
pub fn serve_account_for_test(
    store: AccountSessionStore,
    api_base: String,
    wait: std::time::Duration,
) -> AccountSourceGuard {
    TEST_ACCOUNT_SOURCE.with(|cell| *cell.borrow_mut() = Some((store, api_base, wait)));
    AccountSourceGuard
}

#[cfg(any(test, feature = "test-support"))]
pub struct AccountSourceGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for AccountSourceGuard {
    fn drop(&mut self) {
        TEST_ACCOUNT_SOURCE.with(|cell| *cell.borrow_mut() = None);
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
pub(crate) fn fixture_account_store(base: &str) -> AccountSessionStore {
    use codewhale_secrets::account::{AccountAuthBundle, AccountSession, AccountUser};
    use codewhale_secrets::{InMemoryKeyringStore, Secrets};
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

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::user_constitution::ProfileConstitution;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    fn me() -> serde_json::Value {
        serde_json::json!({"user":{"id":"acct_fixture","settingsRevision":9,
            "preferences":{"constitution":ProfileConstitution { notes:"🐋".repeat(4000), ..Default::default() }}}})
    }

    #[tokio::test]
    async fn profile_constitution_account_transport_is_bounded_and_identity_checked() {
        let server = MockServer::start().await;
        let store = fixture_account_store(&server.uri());
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
        let snapshot = load_from(&store, &server.uri(), None)
            .await
            .unwrap()
            .unwrap();
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
        let store = fixture_account_store(&server.uri());
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(me())
                    .set_delay(std::time::Duration::from_millis(100)),
            )
            .mount(&server)
            .await;
        let base = server.uri();
        let (result, ()) = tokio::join!(load_from(&store, &base, None), async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            store.clear().unwrap();
        });
        assert!(result.unwrap_err().to_string().contains("account changed"));
        assert!(load_from(&store, &base, None).await.unwrap().is_none());
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

    #[tokio::test]
    async fn account_reads_are_never_cached_and_never_cross_accounts() {
        let _home = crate::test_support::SealedHome::new();
        let server = MockServer::start().await;
        let store = fixture_account_store(&server.uri());
        let epoch = epoch_of(&store.snapshot().unwrap()).unwrap();
        let mut other = epoch.clone();
        other.account_id = Some("acct_other".into());
        assert!(
            load_from(&store, &server.uri(), Some(&other))
                .await
                .unwrap_err()
                .to_string()
                .contains("account changed")
        );
        let snapshot = snapshot_from_me(me(), "acct_fixture").unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let wait = std::time::Duration::from_secs(60);
        let mut reads = AccountConstitution::default();
        let ready = |read: AccountRead| match read {
            AccountRead::Ready(result) => result.unwrap(),
            AccountRead::Pending { .. } => panic!("a finished read is used by its own turn"),
        };
        let first = ready(
            reads
                .capture_with(Some(epoch.clone()), &cancel, wait, {
                    let snapshot = snapshot.clone();
                    async move { Ok(snapshot) }
                })
                .await,
        );
        assert!(first.unwrap().contains("revision 9"));
        codewhale_config::UserConstitution {
            about: Some("Signed out local edit.".to_string()),
            ..Default::default()
        }
        .save()
        .unwrap();
        let signed_out = ready(
            reads
                .capture_with(Some(epoch.clone()), &cancel, wait, async { Ok(None) })
                .await,
        );
        assert!(
            signed_out.unwrap().contains("Signed out local edit."),
            "no account is rendered from the local file at use time"
        );

        let slow = || std::future::pending::<Loaded>();
        reads.last = Some((
            epoch.clone(),
            snapshot_from_me(me(), "acct_fixture").unwrap().unwrap(),
        ));
        let AccountRead::Pending { last, signed_in } = reads
            .capture_with(
                Some(other.clone()),
                &cancel,
                std::time::Duration::ZERO,
                slow(),
            )
            .await
        else {
            panic!("an unfinished read is pending");
        };
        assert!(last.is_none(), "another account never reuses a read");
        assert!(signed_in);
        assert!(reads.pending.is_some());
        let AccountRead::Pending { last, .. } = reads
            .capture_with(None, &cancel, std::time::Duration::ZERO, slow())
            .await
        else {
            panic!("an unfinished read is pending");
        };
        assert!(last.is_none());
        assert!(
            reads.pending.is_none() && reads.last.is_none(),
            "an unknown account keeps nothing"
        );
    }
}
